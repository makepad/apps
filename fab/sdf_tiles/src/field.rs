//! A shape's distance field as native code.
//!
//! The shape's program ([`fab_sdf_engine::program`]: the node tree walked depth first, what
//! the twin interprets) is lowered straight to the kernel compiler's IR, as objects:
//! one straight element loop, Splash kernels spliced in, compiled to 4 or 8 lanes. The op codes
//! and the choices they make are code; the program's numbers stay in a buffer the code reads, so
//! a slider or a stroke is new data under the same code ([`CompiledField::structure`]).
//!
//! Three outputs per point: the value, a proven lower bound of the distance to the surface, and
//! how deep inside a solid the point is proven to be (a texture on the root path: the source
//! with the most it can raise or lower it). No masked branch skips anything far from the surface:
//! most points are near it.
//!
//! A second code of the same program gives the value and its gradient in one pass ([`crate::diff`]:
//! forward-mode derivatives through every op), the normal a sample shows: no differences of
//! nearby values, so a crease's side is exact and nothing blurs across it.
//!
//! Every shape takes this one route. What has no code here (a mesh, links, a sculpt with strokes,
//! a kernel with no Splash program or one the splice can't read) is a leaf: the code writes the
//! leaf's points in the leaf's frame, the leaf's own query answers them, and the same code runs
//! again reading the answers. No point depends on a value, so the second run sees the same points.
//! For a gradient the code writes the point's partials too, and the leaf answers its value's
//! along them: a mesh and links from their nearest point, a sculpt's strokes and a kernel with
//! no program, which have no gradient of their own, by differences of their own query.
//!
//! A mesh is answered only as far as the root hangs on it: the code writes each mesh's cap too,
//! past which the root keeps its bits whatever the mesh answers, and the mesh is searched only
//! under it. A cap waiting on another leaf's answer is proven by a run between, from what the answers
//! prove (`CompiledField::answered`), so the bits are those of every leaf answered in full.

use crate::diff::{self, Seeds, D};
use makepad_csg_math::Vec3d;
use makepad_csg_sdf::mesh_field::MeshField;
use makepad_csg_sdf::{distance_many, distance_slot, Sdf3, SdfNode};
use makepad_script_compute::boundary::{compile_air, AirBufferView, KernelRef};
use makepad_script_compute::ir::{self, Bin, Block, Cmp, Op, Program, Region, Stmt, Ty, Un, Val, Var};
use makepad_script_compute::kernel::MathMode;
use makepad_script_compute::lower::kernel::{ELEMENT_CAP, K_BASE, K_OVERFLOW, K_PARAMS, K_WORK};
use makepad_script_compute::lower::LOOP_CAP;
use makepad_script_compute::Backend;
use makepad_script_compute_sched::kernel::KernelExt;
use fab_sdf_engine::kernel::{collect_cpu, KernelSlot};
use fab_sdf_engine::program::{lower_hosted, op, prim};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const CONTROL: u8 = 0;
const POINTS: u8 = 1;
const VALUES: u8 = 2;
const DIST: u8 = 3;
const BOUND: u8 = 4;
const SOLID: u8 = 5;
/// A field with slots has this buffer, and the leaves' come after whichever is last.
const SLOT: u8 = 6;
/// The gradient's code: the value's partials, 3 per point, then the leaves' points, their
/// partials, their answers and the answers' partials.
const GRAD: u8 = 4;
const G_AT: u8 = 5;
const G_TANGENT: u8 = 6;
const G_VALUE: u8 = 7;
const G_GRAD: u8 = 8;

/// A shape's field: compiled code, and the numbers it reads.
#[derive(Clone)]
pub struct CompiledField {
    kernel: KernelRef,
    /// The value and its gradient.
    pub gradient: Gradient,
    values: Arc<[f32]>,
    /// The leaves the host answers, in site order.
    leaves: Arc<[SdfNode]>,
    /// The code's identity: equal for shapes that differ in numbers only.
    pub structure: u64,
    /// The field names material slots ([`CompiledField::eval_slots`] reads them).
    pub slots: bool,
    /// The code writes its leaves' caps ([`CompiledField::answered`]).
    caps: bool,
    /// What compiling cost here; 0: the code was there already.
    pub compile_ms: f64,
    /// An open-ended loop has stopped at its cap: the value there can be wrong.
    loop_stopped: Arc<AtomicBool>,
}

impl CompiledField {
    /// The value, the bound and the solid depth at `xyz` (3 per point).
    pub fn eval(&self, xyz: &[f32], value: &mut [f32], bound: &mut [f32], solid: &mut [f32]) {
        let mut slot = vec![0.0f32; if self.slots { value.len() } else { 0 }];
        self.answered(xyz, value, bound, solid, &mut slot, false);
    }

    /// The value and the material slot at `xyz` (3 per point), for a field with slots.
    pub fn eval_slots(&self, xyz: &[f32], value: &mut [f32], slot: &mut [f32]) {
        let mut scratch = vec![0.0f32; value.len() * 2];
        let (bound, solid) = scratch.split_at_mut(value.len());
        self.answered(xyz, value, bound, solid, slot, true);
    }

    /// The code over `xyz`, and for a shape with leaves the code again over their answers. A
    /// leaf is answered only as far as the root hangs on it. The code's first run writes each
    /// leaf's cap at each point: past it the root keeps its bits whatever the leaf answers,
    /// taking a leaf not answered yet as past every cap of the others'. A mesh is searched only
    /// under its cap ([`MeshField::signed_under`]). The second run writes the caps again from
    /// what the answers prove (a capped answer: at least its cap), and a mesh whose proven cap is
    /// over the one it was searched under is searched again under the proven one. Every cap the
    /// answers keep is then proven, so the last run gives the bits it gives every leaf answered
    /// in full. A named field's leaves are answered in full. How many mesh answers ended at their
    /// caps, and how many were searched again.
    fn answered(&self, xyz: &[f32], value: &mut [f32], bound: &mut [f32], solid: &mut [f32], slot: &mut [f32], named: bool) -> (usize, usize) {
        let n = value.len();
        let sites = self.leaves.len();
        if sites == 0 {
            self.run(xyz, value, bound, solid, slot, None);
            return (0, 0);
        }
        let mut io = LeafIo::new(n * sites, self.slots, self.caps);
        self.run(xyz, value, bound, solid, slot, Some(&mut io));
        let caps = self.caps && !named;
        let mut q = vec![Vec3d::new(0.0, 0.0, 0.0); n];
        let mut d = vec![0.0f64; n];
        let mut under = vec![false; if caps { n * sites } else { 0 }];
        for (site, leaf) in self.leaves.iter().enumerate() {
            io.points(site, sites, &mut q);
            match leaf {
                _ if named => {
                    for (e, p) in q.iter().enumerate() {
                        let (v, s) = distance_slot(leaf, *p);
                        io.answers[e * sites + site] = v as f32;
                        io.named[e * sites + site] = s as f32;
                    }
                }
                SdfNode::Mesh(m) if caps => {
                    for (e, p) in q.iter().enumerate() {
                        let w = e * sites + site;
                        under[w] = io.answer(w, m.signed_under(*p, io.cap[w] as f64));
                    }
                }
                _ => {
                    distance_many(leaf, &q, &mut d);
                    for (e, v) in d.iter().enumerate() {
                        io.answer(e * sites + site, *v);
                    }
                }
            }
        }
        let mut again = 0;
        if under.contains(&true) {
            let used = io.cap.clone();
            self.run(xyz, value, bound, solid, slot, Some(&mut io));
            for (site, leaf) in self.leaves.iter().enumerate() {
                let SdfNode::Mesh(m) = leaf else { continue };
                io.points(site, sites, &mut q);
                for (e, p) in q.iter().enumerate() {
                    let w = e * sites + site;
                    if under[w] && io.cap[w] > used[w] {
                        under[w] = io.answer(w, m.signed_under(*p, io.cap[w] as f64));
                        again += 1;
                    }
                }
            }
        }
        self.run(xyz, value, bound, solid, slot, Some(&mut io));
        (under.iter().filter(|u| **u).count(), again)
    }

    #[allow(clippy::too_many_arguments)]
    fn run(&self, xyz: &[f32], value: &mut [f32], bound: &mut [f32], solid: &mut [f32], slot: &mut [f32], leaves: Option<&mut LeafIo>) {
        let n = value.len();
        let mut call = self.kernel.call();
        call.input("sdf_points", xyz).expect("the field's points");
        call.input("sdf_values", &self.values).expect("the field's numbers");
        call.output("sdf_dist", value).expect("the field's value");
        call.output("sdf_bound", bound).expect("the field's bound");
        call.output("sdf_solid", solid).expect("the field's solid");
        if self.slots {
            call.output("sdf_slot", slot).expect("the field's slot");
        }
        if let Some(io) = leaves {
            call.output("sdf_leaf_at", &mut io.at).expect("the leaves' points");
            call.input("sdf_leaf_value", &io.answers).expect("the leaves' answers");
            if self.slots {
                call.input("sdf_leaf_slot", &io.named).expect("the leaves' slots");
            }
            if self.caps {
                call.output("sdf_leaf_cap", &mut io.cap).expect("the leaves' caps");
                call.input("sdf_leaf_lo", &io.lo).expect("the leaves' lower bounds");
                call.input("sdf_leaf_hi", &io.hi).expect("the leaves' upper bounds");
            }
        }
        let stats = call.run(n).expect("the field runs");
        if stats.overflowed() && !self.loop_stopped.swap(true, Ordering::Relaxed) {
            makepad_draw::makepad_platform::log!("sdf field: an open-ended loop in a kernel of this part stopped at its {LOOP_CAP} passes");
        }
    }

    /// An open-ended loop of this field has stopped at its cap, in the runs so far.
    pub fn loop_stopped(&self) -> bool {
        self.loop_stopped.load(Ordering::Relaxed)
    }

    /// The lanes the code runs at (4 or 8; 0: scalar).
    pub fn lanes(&self) -> u32 {
        self.kernel.vector_lanes()
    }

    /// How many leaves the host answers per point.
    pub fn sites(&self) -> usize {
        self.leaves.len()
    }
}

/// What the code and the host pass each other for the leaves: per point and leaf, the point,
/// the answer and its slot, and with caps the cap and what the answer proves of the leaf.
struct LeafIo {
    at: Vec<f32>,
    answers: Vec<f32>,
    named: Vec<f32>,
    cap: Vec<f32>,
    lo: Vec<f32>,
    hi: Vec<f32>,
    /// The gradient's: the point's partials (9 per point and leaf), the answer's (3).
    tangent: Vec<f32>,
    partials: Vec<f32>,
}

impl LeafIo {
    /// Before any answer: every leaf as far as can be, past every cap of the others', and
    /// under none of its own.
    fn new(count: usize, slots: bool, caps: bool) -> LeafIo {
        let c = if caps { count } else { 0 };
        LeafIo { at: vec![0.0; count * 3], answers: vec![f32::INFINITY; count], named: vec![0.0; if slots { count } else { 0 }], cap: vec![f32::INFINITY; c], lo: vec![f32::INFINITY; c], hi: vec![f32::INFINITY; c], tangent: Vec::new(), partials: Vec::new() }
    }

    /// Mesh answer `w` at `p` under its cap with its partials (zero for the cap): whether it is
    /// the cap.
    fn mesh_gradient(&mut self, m: &MeshField, w: usize, p: Vec3d) -> bool {
        let capped = self.answer(w, m.signed_under(p, self.cap[w] as f64));
        let g = if capped {
            [0.0; 3]
        } else {
            let n = m.normal(p);
            along(&self.tangent, w, [n.x, n.y, n.z])
        };
        for (k, g) in g.iter().enumerate() {
            self.partials[w * 3 + k] = *g as f32;
        }
        capped
    }

    /// Leaf `site`'s points, from the code's last run.
    fn points(&self, site: usize, sites: usize, q: &mut [Vec3d]) {
        for (e, p) in q.iter_mut().enumerate() {
            let w = (e * sites + site) * 3;
            *p = Vec3d::new(self.at[w] as f64, self.at[w + 1] as f64, self.at[w + 2] as f64);
        }
    }

    /// Answer `w` (`d.min(cap)` under a cap): its bounds, and whether it is the cap.
    fn answer(&mut self, w: usize, d: f64) -> bool {
        let v = d as f32;
        self.answers[w] = v;
        if self.cap.is_empty() {
            return false;
        }
        let capped = d == self.cap[w] as f64;
        (self.lo[w], self.hi[w]) = (v, if capped { f32::INFINITY } else { v });
        capped
    }
}

fn compiled() -> &'static Mutex<HashMap<u64, KernelRef>> {
    static CODE: OnceLock<Mutex<HashMap<u64, KernelRef>>> = OnceLock::new();
    CODE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// What a part shows when its field's open-ended loop stops at the language's cap.
pub const LOOP_STOP_ISSUE: &str = "A loop in this part's field stopped at its limit of 1024 passes, so its surface can be wrong. Make the loop end sooner.";
const _: () = assert!(LOOP_CAP == 1024, "the issue's text names the cap");

/// A kernel that will not splice stays a leaf, and the log says why.
fn refuse(key: u64, why: String) -> Stop {
    makepad_draw::makepad_platform::log!("sdf field: kernel {key:x} runs as a leaf: {why}");
    Stop::Refused(key)
}

/// The kernels the splice can't read: leaves from then on.
fn refused() -> &'static Mutex<HashSet<u64>> {
    static REFUSED: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    REFUSED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Why the code wasn't built.
enum Stop {
    /// The kernel with this key can't be spliced in: it becomes a leaf.
    Refused(u64),
    Broken(String),
}

impl From<String> for Stop {
    fn from(why: String) -> Stop {
        Stop::Broken(why)
    }
}

impl From<&str> for Stop {
    fn from(why: &str) -> Stop {
        Stop::Broken(why.to_string())
    }
}

/// The field of `root`, with its material slots if `slots`: its numbers now, its code compiled or,
/// for a shape of the same structure, taken as it is. Any tree lowers; an error is the backend's.
pub fn compile(root: &SdfNode, slots: bool) -> Result<CompiledField, String> {
    compile_with(root, slots, true)
}

/// [`compile`], its leaves answered under caps (`caps`) or in full.
fn compile_with(root: &SdfNode, slots: bool, caps: bool) -> Result<CompiledField, String> {
    loop {
        let (table, texels, leaves) = lowered(root)?;
        let structure = structure_key(&texels, &table, slots);
        let values: Arc<[f32]> = texels.as_flattened().into();
        let leaves: Arc<[SdfNode]> = leaves.into();
        let capped = capped(&leaves, caps);
        let (kernel, value_ms) = match code(&texels, &table, slots, &capped, false, structure) {
            Ok(code) => code,
            Err(Stop::Refused(key)) => {
                refused().lock().unwrap().insert(key);
                continue;
            }
            Err(Stop::Broken(why)) => return Err(why),
        };
        let (grad, grad_ms) = match code(&texels, &table, false, &capped, true, structure_key(&texels, &table, false)) {
            Ok(code) => code,
            Err(Stop::Refused(key)) => {
                refused().lock().unwrap().insert(key);
                continue;
            }
            Err(Stop::Broken(why)) => return Err(why),
        };
        let caps = capped.contains(&true);
        let gradient = Gradient { kernel: grad, values: values.clone(), leaves: leaves.clone(), caps };
        return Ok(CompiledField { kernel, gradient, values, leaves, structure, slots, caps, compile_ms: value_ms + grad_ms, loop_stopped: Arc::default() });
    }
}

/// The value and gradient of `root` alone (a pick's normal): the gradient's code of [`compile`],
/// compiled once for every shape of its structure.
pub fn compile_gradient(root: &SdfNode) -> Result<Gradient, String> {
    loop {
        let (table, texels, leaves) = lowered(root)?;
        let values: Arc<[f32]> = texels.as_flattened().into();
        let leaves: Arc<[SdfNode]> = leaves.into();
        let capped = capped(&leaves, true);
        match code(&texels, &table, false, &capped, true, structure_key(&texels, &table, false)) {
            Ok((kernel, _)) => {
                let caps = capped.contains(&true);
                return Ok(Gradient { kernel, values, leaves, caps });
            }
            Err(Stop::Refused(key)) => {
                refused().lock().unwrap().insert(key);
            }
            Err(Stop::Broken(why)) => return Err(why),
        }
    }
}

/// `root` as the program the code reads, with its kernels and its leaves.
fn lowered(root: &SdfNode) -> Result<(Vec<KernelSlot>, Vec<[f32; 4]>, Vec<SdfNode>), String> {
    let mut table = Vec::new();
    collect_cpu(root, &mut table);
    {
        let refused = refused().lock().unwrap();
        table.retain(|k| !refused.contains(&k.key));
    }
    let (stream, leaves) = lower_hosted(root, &table).map_err(|e| format!("{e:?}"))?;
    Ok((table, stream.texels, leaves))
}

/// Which of `leaves` the host answers under caps, if `caps`: the meshes
/// ([`MeshField::signed_under`]). The others are answered in full, so the code writes no cap of
/// theirs.
fn capped(leaves: &[SdfNode], caps: bool) -> Vec<bool> {
    leaves.iter().map(|leaf| caps && matches!(leaf, SdfNode::Mesh(_))).collect()
}

/// The code of one mode (the value's, or with `grad` the gradient's) for a program of
/// `structure`, its sites capped as `capped` says, from those compiled already or compiled now;
/// what compiling cost.
fn code(texels: &[[f32; 4]], table: &[KernelSlot], slots: bool, capped: &[bool], grad: bool, structure: u64) -> Result<(KernelRef, f64), Stop> {
    let (sites, caps) = (capped.len(), capped.contains(&true));
    let key = if grad { structure ^ 0x6772_6164_6965_6e74 } else { structure };
    // the caps' code is the capped sites'
    let key = if caps { capped.iter().fold(key ^ 0x6361_7073, |h, c| (h ^ *c as u64).wrapping_mul(0x100_0000_01b3)) } else { key };
    if let Some(kernel) = compiled().lock().unwrap().get(&key).cloned() {
        return Ok((kernel, 0.0));
    }
    let t0 = std::time::Instant::now();
    let (program, shared) = build(texels, table, slots, capped, grad)?;
    let words = ir::to_words(&program);
    let per = sites as u32;
    let mut buffers = vec![
        AirBufferView { name: "#control", write: false, stride: 1 },
        AirBufferView { name: "sdf_points", write: false, stride: 3 },
        AirBufferView { name: "sdf_values", write: false, stride: 0 },
        AirBufferView { name: "sdf_dist", write: true, stride: 1 },
    ];
    if grad {
        buffers.push(AirBufferView { name: "sdf_grad", write: true, stride: 3 });
        if per > 0 {
            buffers.push(AirBufferView { name: "sdf_leaf_at", write: true, stride: per * 3 });
            buffers.push(AirBufferView { name: "sdf_leaf_tangent", write: true, stride: per * 9 });
            buffers.push(AirBufferView { name: "sdf_leaf_value", write: false, stride: per });
            buffers.push(AirBufferView { name: "sdf_leaf_grad", write: false, stride: per * 3 });
            if caps {
                buffers.push(AirBufferView { name: "sdf_leaf_cap", write: true, stride: per });
                buffers.push(AirBufferView { name: "sdf_leaf_lo", write: false, stride: per });
                buffers.push(AirBufferView { name: "sdf_leaf_hi", write: false, stride: per });
            }
        }
    } else {
        buffers.push(AirBufferView { name: "sdf_bound", write: true, stride: 1 });
        buffers.push(AirBufferView { name: "sdf_solid", write: true, stride: 1 });
        if slots {
            buffers.push(AirBufferView { name: "sdf_slot", write: true, stride: 1 });
        }
        if per > 0 {
            buffers.push(AirBufferView { name: "sdf_leaf_at", write: true, stride: per * 3 });
            buffers.push(AirBufferView { name: "sdf_leaf_value", write: false, stride: per });
            if slots {
                buffers.push(AirBufferView { name: "sdf_leaf_slot", write: false, stride: per });
            }
            if caps {
                buffers.push(AirBufferView { name: "sdf_leaf_cap", write: true, stride: per });
                buffers.push(AirBufferView { name: "sdf_leaf_lo", write: false, stride: per });
                buffers.push(AirBufferView { name: "sdf_leaf_hi", write: false, stride: per });
            }
        }
    }
    let mut compilation = compile_air(&words, &shared, &buffers, MathMode::Fast, Backend::Native);
    let Some(kernel) = compilation.take_kernel() else { return Err(Stop::Broken(compilation.messages())) };
    compiled().lock().unwrap().insert(key, kernel.clone());
    Ok((kernel, t0.elapsed().as_secs_f64() * 1e3))
}

/// A field's value and its gradient, from one pass of its gradient's code.
#[derive(Clone)]
pub struct Gradient {
    kernel: KernelRef,
    values: Arc<[f32]>,
    leaves: Arc<[SdfNode]>,
    /// The code writes its leaves' caps (as [`CompiledField::answered`]'s).
    caps: bool,
}

impl Gradient {
    /// The value and its gradient at `xyz` (3 per point; `grad` 3 per point). A leaf with no
    /// gradient of its own differences its own query `h` along the point's partials. A mesh is
    /// answered under its caps as in [`CompiledField::answered`]; a capped answer's partials are
    /// zero, as no root's partials take a leaf's past its cap.
    pub fn eval(&self, xyz: &[f32], value: &mut [f32], grad: &mut [f32], h: f32) {
        let n = value.len();
        let sites = self.leaves.len();
        if sites == 0 {
            self.run(xyz, value, grad, None);
            return;
        }
        let mut io = LeafIo::new(n * sites, false, self.caps);
        (io.tangent, io.partials) = (vec![0.0; n * sites * 9], vec![0.0; n * sites * 3]);
        self.run(xyz, value, grad, Some(&mut io));
        let mut under = vec![false; if self.caps { n * sites } else { 0 }];
        let mut q = vec![Vec3d::new(0.0, 0.0, 0.0); n];
        for (site, leaf) in self.leaves.iter().enumerate() {
            match leaf {
                SdfNode::Mesh(m) if self.caps => {
                    io.points(site, sites, &mut q);
                    for (e, p) in q.iter().enumerate() {
                        under[e * sites + site] = io.mesh_gradient(m, e * sites + site, *p);
                    }
                }
                _ => {
                    leaf_gradient(leaf, site, sites, &io.at, &io.tangent, &mut io.answers, &mut io.partials, h as f64);
                    for e in 0..n {
                        io.answer(e * sites + site, io.answers[e * sites + site] as f64);
                    }
                }
            }
        }
        if under.contains(&true) {
            let used = io.cap.clone();
            self.run(xyz, value, grad, Some(&mut io));
            for (site, leaf) in self.leaves.iter().enumerate() {
                let SdfNode::Mesh(m) = leaf else { continue };
                io.points(site, sites, &mut q);
                for (e, p) in q.iter().enumerate() {
                    let w = e * sites + site;
                    if under[w] && io.cap[w] > used[w] {
                        io.mesh_gradient(m, w, *p);
                    }
                }
            }
        }
        self.run(xyz, value, grad, Some(&mut io));
    }

    fn run(&self, xyz: &[f32], value: &mut [f32], grad: &mut [f32], leaves: Option<&mut LeafIo>) {
        let n = value.len();
        let mut call = self.kernel.call();
        call.input("sdf_points", xyz).expect("the field's points");
        call.input("sdf_values", &self.values).expect("the field's numbers");
        call.output("sdf_dist", value).expect("the field's value");
        call.output("sdf_grad", grad).expect("the field's gradient");
        if let Some(io) = leaves {
            call.output("sdf_leaf_at", &mut io.at).expect("the leaves' points");
            call.output("sdf_leaf_tangent", &mut io.tangent).expect("the leaves' partials");
            call.input("sdf_leaf_value", &io.answers).expect("the leaves' answers");
            call.input("sdf_leaf_grad", &io.partials).expect("the leaves' answers' partials");
            if self.caps {
                call.output("sdf_leaf_cap", &mut io.cap).expect("the leaves' caps");
                call.input("sdf_leaf_lo", &io.lo).expect("the leaves' lower bounds");
                call.input("sdf_leaf_hi", &io.hi).expect("the leaves' upper bounds");
            }
        }
        call.run(n).expect("the field's gradient runs");
    }
}

/// A point's partials of a leaf answer whose gradient is `n`, along the point's own three.
fn along(tangent: &[f32], w: usize, n: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|k| {
        let t = &tangent[w * 9 + k * 3..w * 9 + k * 3 + 3];
        n[0] * t[0] as f64 + n[1] * t[1] as f64 + n[2] * t[2] as f64
    })
}

/// One leaf's answers at its points, and their partials along the points' own (`tangent`, three
/// per point): a mesh's and links' from their nearest point; what has no gradient of its own, a
/// sculpt's strokes or a kernel with no program, by its own query `h` along each.
#[allow(clippy::too_many_arguments)]
fn leaf_gradient(leaf: &SdfNode, site: usize, sites: usize, at: &[f32], tangent: &[f32], answers: &mut [f32], partials: &mut [f32], h: f64) {
    let n = answers.len() / sites;
    let pts: Vec<Vec3d> = (0..n).map(|e| {
        let w = (e * sites + site) * 3;
        Vec3d::new(at[w] as f64, at[w + 1] as f64, at[w + 2] as f64)
    }).collect();
    let along_of = |e: usize, k: usize| {
        let w = (e * sites + site) * 9 + k * 3;
        [tangent[w] as f64, tangent[w + 1] as f64, tangent[w + 2] as f64]
    };
    let mut d = vec![0.0f64; n];
    // per point, the partials along its three
    let mut g = vec![[0.0f64; 3]; n];
    let gradient = |e: usize, n: [f64; 3]| along(tangent, e * sites + site, n);
    match leaf {
        SdfNode::Mesh(m) => {
            distance_many(leaf, &pts, &mut d);
            for (e, p) in pts.iter().enumerate() {
                let n = m.normal(*p);
                g[e] = gradient(e, [n.x, n.y, n.z]);
            }
        }
        SdfNode::Links(l) => {
            for (e, p) in pts.iter().enumerate() {
                let (v, n) = l.distance_gradient(*p);
                d[e] = v;
                g[e] = gradient(e, n);
            }
        }
        SdfNode::Tubes(t) => {
            for (e, p) in pts.iter().enumerate() {
                let (v, n) = t.distance_gradient(*p);
                d[e] = v;
                g[e] = gradient(e, n);
            }
        }
        _ => {
            // each point, then a step along each of its partials
            let mut q = Vec::with_capacity(n * 4);
            for (e, p) in pts.iter().enumerate() {
                q.push(*p);
                for k in 0..3 {
                    let t = along_of(e, k);
                    q.push(Vec3d::new(p.x + h * t[0], p.y + h * t[1], p.z + h * t[2]));
                }
            }
            let mut v = vec![0.0f64; q.len()];
            distance_many(leaf, &q, &mut v);
            for e in 0..n {
                let v = &v[e * 4..e * 4 + 4];
                d[e] = v[0];
                g[e] = std::array::from_fn(|k| (v[k + 1] - v[0]) / h);
            }
        }
    }
    for e in 0..n {
        answers[e * sites + site] = d[e] as f32;
        for k in 0..3 {
            partials[(e * sites + site) * 3 + k] = g[e][k] as f32;
        }
    }
}

fn op_len(h: [f32; 4]) -> usize {
    fab_sdf_engine::program::op_len(h)
}

/// What the code depends on: the op codes, the choices they make, the kernels' programs and which
/// transform numbers are 0 or 1 (those terms are left out).
fn structure_key(t: &[[f32; 4]], kernels: &[KernelSlot], slots: bool) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |x: u64| h = (h ^ x).wrapping_mul(0x100_0000_01b3);
    mix(slots as u64);
    let end = t[0][0] as usize;
    let mut pc = 1;
    while pc < end {
        let [c, a, b, d] = t[pc];
        mix(c.to_bits() as u64);
        if c == op::PRIM {
            mix(a.to_bits() as u64);
        } else if c == op::KERNEL {
            mix(kernels[a as usize].key);
            mix(d.to_bits() as u64);
        } else if c == op::FOLD {
            mix(a.to_bits() as u64);
            mix((b > 0.0) as u64);
        } else if c == op::DEFORM_PUSH {
            mix(a.to_bits() as u64);
            mix(d.to_bits() as u64);
        } else if c == op::XFORM_PUSH {
            for row in &t[pc + 1..pc + 4] {
                for x in &row[..3] {
                    mix(if *x == 0.0 { 0 } else if *x == 1.0 { 1 } else { 2 });
                }
                mix((row[3] == 0.0) as u64);
            }
        } else if c == op::TEX {
            mix(a.to_bits() as u64);
            mix(b.to_bits() as u64);
            mix(t[pc + 1][3].to_bits() as u64);
        } else if c == op::MAT_PUSH || c == op::MAT_WITHIN {
            mix(a.to_bits() as u64);
        }
        pc += op_len(t[pc]);
    }
    h
}

/// One value of the program's stack: the value, its bound and solid depth once a root-path texture
/// gave them, its material slot.
#[derive(Clone, Copy)]
struct Entry {
    v: Val,
    b: Option<Val>,
    deep: Option<Val>,
    s: Option<Val>,
}

/// One op between a leaf and the root, as a cap passes it down: from a value past which the root
/// keeps its bits whatever the op's result is, to one for its operand.
#[derive(Clone, Copy)]
enum Step {
    /// The operand times `n` (`times`) or over it.
    Scale { n: Val, times: bool },
    /// The operand less `r` (a rounding).
    Less(Val),
    /// An op no cap passes (a shell, a texture, a material's region).
    Opaque,
    /// A union's operand, beside the other's bounds.
    Union { lo: Option<Val>, hi: Option<Val>, k: Option<Val> },
    /// A cut's cutter, beside the base's lower bound.
    Cutter { lo: Option<Val>, k: Option<Val> },
    /// A cut's base or an intersection's operand: the result is at least the operand, and at
    /// least `other` (a lower bound of the rest of it).
    Above { other: Option<Val>, k: Option<Val> },
}

/// What the caps know of a stack entry: bounds on its value that hold whatever its capped
/// leaves answer (`None`: none), and each capped leaf in it with the ops from it up to the entry.
#[derive(Clone, Default)]
struct Bounds {
    /// No capped leaf in it: once its leaves are answered (in full) its value is the same in
    /// every run, and its own bounds. Before, its leaves are as far as can be ([`LeafIo::new`]).
    fixed: bool,
    lo: Option<Val>,
    hi: Option<Val>,
    paths: Vec<(u32, Vec<Step>)>,
}

impl Bounds {
    fn known(v: Val) -> Bounds {
        Bounds { fixed: true, lo: Some(v), hi: Some(v), paths: Vec::new() }
    }
    fn through(mut self, step: Step) -> Bounds {
        self.paths.iter_mut().for_each(|p| p.1.push(step));
        self
    }
}

/// A kernel's program as the splice reads it.
struct Splice {
    prog: Program,
    points: u8,
    dist: u8,
    grid: Option<u8>,
    control: Option<u8>,
    /// Its element counter and its work counter.
    i: Var,
    work: Option<Var>,
    /// The element loop's index in its body.
    at: usize,
    shared_base: u32,
    /// The integer values it computes, for the point loads' offsets.
    defs: HashMap<u32, Op>,
}

/// Where a leaf's point goes and its answer comes from.
#[derive(Clone, Copy)]
struct Hosted {
    at: u8,
    value: u8,
    slot: u8,
    /// The leaves' caps (out) and their answers' bounds (in): see [`CompiledField::answered`].
    cap: u8,
    lo: u8,
    hi: u8,
    /// The element's first word in the points' buffer, and in the answers'.
    at_off: Val,
    value_off: Val,
}

struct Build<'a> {
    t: &'a [[f32; 4]],
    kernels: &'a [KernelSlot],
    p: Program,
    out: Block,
    splices: Vec<Splice>,
    i: Var,
    q: [Val; 3],
    frame: u32,
    slots: bool,
    hosted: Option<Hosted>,
    /// The code writes the leaves' caps: those of the sites `capped` says.
    caps: bool,
    capped: &'a [bool],
    /// Each capped leaf's ops up to the root, once the program has run through.
    paths: Vec<(u32, Vec<Step>)>,
}

impl Build<'_> {
    fn val(&mut self, ty: Ty) -> Val {
        self.p.vals.push(ty);
        Val(self.p.vals.len() as u32 - 1)
    }
    fn var(&mut self, ty: Ty) -> Var {
        self.p.vars.push(ty);
        Var(self.p.vars.len() as u32 - 1)
    }
    fn def(&mut self, ty: Ty, op: Op) -> Val {
        let v = self.val(ty);
        self.out.push(Stmt::Def(v, op));
        v
    }
    fn f(&mut self, x: f32) -> Val { self.def(Ty::F32, Op::ConstF(x)) }
    fn word(&mut self, word: usize) -> Val {
        self.def(Ty::F32, Op::Load { region: Region::Buf(VALUES), base: word as u32, extent: 1, off: None })
    }
    /// The program's number at texel `pc`, lane `lane`.
    fn num(&mut self, pc: usize, lane: usize) -> Val { self.word(pc * 4 + lane) }
    fn bin(&mut self, b: Bin, x: Val, y: Val) -> Val { self.def(Ty::F32, Op::Bin(b, x, y)) }
    fn add(&mut self, x: Val, y: Val) -> Val { self.bin(Bin::AddF, x, y) }
    fn sub(&mut self, x: Val, y: Val) -> Val { self.bin(Bin::SubF, x, y) }
    fn mul(&mut self, x: Val, y: Val) -> Val { self.bin(Bin::MulF, x, y) }
    fn div(&mut self, x: Val, y: Val) -> Val { self.bin(Bin::DivF, x, y) }
    fn min(&mut self, x: Val, y: Val) -> Val { self.bin(Bin::MinF, x, y) }
    fn max(&mut self, x: Val, y: Val) -> Val { self.bin(Bin::MaxF, x, y) }
    fn un(&mut self, u: Un, x: Val) -> Val { self.def(Ty::F32, Op::Un(u, x)) }
    fn abs(&mut self, x: Val) -> Val { self.un(Un::AbsF, x) }
    fn neg(&mut self, x: Val) -> Val { self.un(Un::NegF, x) }
    fn sqrt(&mut self, x: Val) -> Val { self.un(Un::SqrtF, x) }
    fn lt(&mut self, x: Val, y: Val) -> Val { self.def(Ty::Bool, Op::CmpF(Cmp::Lt, x, y)) }
    fn sel(&mut self, c: Val, x: Val, y: Val) -> Val { self.def(Ty::F32, Op::Sel(c, x, y)) }
    fn clamp(&mut self, x: Val, lo: Val, hi: Val) -> Val {
        let m = self.max(x, lo);
        self.min(m, hi)
    }
    fn len2(&mut self, x: Val, y: Val) -> Val {
        let (a, b) = (self.mul(x, x), self.mul(y, y));
        let s = self.add(a, b);
        self.sqrt(s)
    }
    fn len3(&mut self, x: Val, y: Val, z: Val) -> Val {
        let (a, b, c) = (self.mul(x, x), self.mul(y, y), self.mul(z, z));
        let s = self.add(a, b);
        let s = self.add(s, c);
        self.sqrt(s)
    }
    /// The twin's sin and cos: a quadrant reduction, then polynomials.
    fn sin_cos(&mut self, x: Val) -> (Val, Val) {
        let k = self.f(0.6366198);
        let half = self.f(0.5);
        let a = self.mul(x, k);
        let a = self.add(a, half);
        let q = self.un(Un::FloorF, a);
        let (p1, p2) = (self.f(1.5707964), self.f(-0.00000004371139));
        let m1 = self.mul(q, p1);
        let r = self.sub(x, m1);
        let m2 = self.mul(q, p2);
        let r = self.sub(r, m2);
        let r2 = self.mul(r, r);
        let poly = |b: &mut Self, cs: &[f32]| {
            let mut acc = b.f(*cs.last().unwrap());
            for c in cs[..cs.len() - 1].iter().rev() {
                let m = b.mul(r2, acc);
                let k = b.f(*c);
                acc = b.add(k, m);
            }
            acc
        };
        let sp = poly(self, &[-0.16666667, 0.008333331, -0.00019840874, 0.0000027525562]);
        let rr = self.mul(r, r2);
        let sm = self.mul(rr, sp);
        let s = self.add(r, sm);
        let cp = poly(self, &[-0.5, 0.041666638, -0.0013888378, 0.000024760495]);
        let cm = self.mul(r2, cp);
        let one = self.f(1.0);
        let c = self.add(one, cm);
        // the quadrant: q mod 4
        let quarter = self.f(0.25);
        let four = self.f(4.0);
        let qq = self.mul(q, quarter);
        let fl = self.un(Un::FloorF, qq);
        let fm = self.mul(fl, four);
        let n = self.sub(q, fm);
        let (ns, nc) = (self.neg(s), self.neg(c));
        let (t0, t1, t2) = (self.f(0.5), self.f(1.5), self.f(2.5));
        let (c0, c1, c2) = (self.lt(n, t0), self.lt(n, t1), self.lt(n, t2));
        let so = self.sel(c2, ns, nc);
        let so = self.sel(c1, c, so);
        let so = self.sel(c0, s, so);
        let co = self.sel(c2, nc, s);
        let co = self.sel(c1, ns, co);
        let co = self.sel(c0, c, co);
        (so, co)
    }

    fn prim(&mut self, pc: usize) -> Val {
        let kind = self.t[pc][1];
        let [px, py, pz] = self.q;
        let num = |b: &mut Self, i: usize| -> Val {
            // a, b in the header's last lanes, c.. in the next two texels
            let (texel, lane) = if i < 2 { (pc, 2 + i) } else { (pc + 1 + (i - 2) / 4, (i - 2) % 4) };
            b.num(texel, lane)
        };
        let zero = self.f(0.0);
        let one = self.f(1.0);
        if kind == prim::SPHERE {
            let a = num(self, 0);
            let l = self.len3(px, py, pz);
            self.sub(l, a)
        } else if kind == prim::BOX {
            let (a, b, c, d) = (num(self, 0), num(self, 1), num(self, 2), num(self, 3));
            let (ax, ay, az) = (self.abs(px), self.abs(py), self.abs(pz));
            let (qx, qy, qz) = (self.sub(ax, a), self.sub(ay, b), self.sub(az, c));
            let (mx, my, mz) = (self.max(qx, zero), self.max(qy, zero), self.max(qz, zero));
            let l = self.len3(mx, my, mz);
            let t = self.max(qy, qz);
            let t = self.max(qx, t);
            let t = self.min(t, zero);
            let s = self.add(l, t);
            self.sub(s, d)
        } else if kind == prim::CYLINDER {
            let (a, b) = (num(self, 0), num(self, 1));
            let qx = self.len2(px, pz);
            let dx = self.sub(qx, a);
            let ay = self.abs(py);
            let dy = self.sub(ay, b);
            let (mx, my) = (self.max(dx, zero), self.max(dy, zero));
            let l = self.len2(mx, my);
            let t = self.max(dx, dy);
            let t = self.min(t, zero);
            self.add(l, t)
        } else if kind == prim::CONE {
            let (r1, r2, hh) = (num(self, 0), num(self, 1), num(self, 2));
            let qx = self.len2(px, pz);
            let two = self.f(2.0);
            let k2x = self.sub(r2, r1);
            let k2y = self.mul(hh, two);
            let below = self.lt(py, zero);
            let r = self.sel(below, r1, r2);
            let m = self.min(qx, r);
            let cax = self.sub(qx, m);
            let ay = self.abs(py);
            let cay = self.sub(ay, hh);
            let (n0, n1) = (self.sub(r2, qx), self.sub(hh, py));
            let (t0, t1) = (self.mul(n0, k2x), self.mul(n1, k2y));
            let top = self.add(t0, t1);
            let (d0, d1) = (self.mul(k2x, k2x), self.mul(k2y, k2y));
            let dn = self.add(d0, d1);
            let t = self.div(top, dn);
            let t = self.clamp(t, zero, one);
            let (e0, e1) = (self.sub(qx, r2), self.sub(py, hh));
            let (f0, f1) = (self.mul(k2x, t), self.mul(k2y, t));
            let cbx = self.add(e0, f0);
            let cby = self.add(e1, f1);
            let (c0, c1) = (self.lt(cbx, zero), self.lt(cay, zero));
            let both = self.def(Ty::Bool, Op::Bin(Bin::AndB, c0, c1));
            let m1 = self.f(-1.0);
            let s = self.sel(both, m1, one);
            let (g0, g1) = (self.mul(cax, cax), self.mul(cay, cay));
            let (g2, g3) = (self.mul(cbx, cbx), self.mul(cby, cby));
            let (ga, gb) = (self.add(g0, g1), self.add(g2, g3));
            let g = self.min(ga, gb);
            let g = self.sqrt(g);
            self.mul(s, g)
        } else if kind == prim::TORUS {
            let (a, b) = (num(self, 0), num(self, 1));
            let l = self.len2(px, pz);
            let q = self.sub(l, a);
            let l = self.len2(q, py);
            self.sub(l, b)
        } else if kind == prim::CAPSULE {
            let (a, b, c) = (num(self, 0), num(self, 1), num(self, 2));
            let (d, e, ff, g) = (num(self, 3), num(self, 4), num(self, 5), num(self, 6));
            let (x, y, z) = (self.sub(px, a), self.sub(py, b), self.sub(pz, c));
            let (t0, t1, t2) = (self.mul(x, d), self.mul(y, e), self.mul(z, ff));
            let top = self.add(t0, t1);
            let top = self.add(top, t2);
            let (s0, s1, s2) = (self.mul(d, d), self.mul(e, e), self.mul(ff, ff));
            let bot = self.add(s0, s1);
            let bot = self.add(bot, s2);
            let h = self.div(top, bot);
            let h = self.clamp(h, zero, one);
            let (m0, m1, m2) = (self.mul(d, h), self.mul(e, h), self.mul(ff, h));
            let (w0, w1, w2) = (self.sub(x, m0), self.sub(y, m1), self.sub(z, m2));
            let l = self.len3(w0, w1, w2);
            self.sub(l, g)
        } else if kind == prim::ELLIPSOID {
            let (a, b, c) = (num(self, 0), num(self, 1), num(self, 2));
            let (x0, y0, z0) = (self.div(px, a), self.div(py, b), self.div(pz, c));
            let k0 = self.len3(x0, y0, z0);
            let (aa, bb, cc) = (self.mul(a, a), self.mul(b, b), self.mul(c, c));
            let (x1, y1, z1) = (self.div(px, aa), self.div(py, bb), self.div(pz, cc));
            let k1 = self.len3(x1, y1, z1);
            let k0m = self.sub(k0, one);
            let top = self.mul(k0, k0m);
            let tiny = self.f(0.000000000001f64 as f32);
            let k1 = self.max(k1, tiny);
            self.div(top, k1)
        } else {
            // the gyroid
            let (a, b) = (num(self, 0), num(self, 1));
            let (ax, ay, az) = (self.mul(px, a), self.mul(py, a), self.mul(pz, a));
            let (sx, cx) = self.sin_cos(ax);
            let (sy, cy) = self.sin_cos(ay);
            let (sz, cz) = self.sin_cos(az);
            let (t0, t1, t2) = (self.mul(sx, cy), self.mul(sy, cz), self.mul(sz, cx));
            let g = self.add(t0, t1);
            let g = self.add(g, t2);
            let g = self.abs(g);
            let g = self.div(g, a);
            self.sub(g, b)
        }
    }

    /// A row of a transform: its terms that are not 0, as data.
    fn row(&mut self, pc: usize, i: usize, v: [Val; 3]) -> Val {
        let r = self.t[pc + 1 + i];
        let mut acc = None;
        for k in 0..3 {
            if r[k] == 0.0 {
                continue;
            }
            let term = if r[k] == 1.0 {
                v[k]
            } else {
                let n = self.num(pc + 1 + i, k);
                self.mul(n, v[k])
            };
            acc = Some(match acc {
                Some(a) => self.add(a, term),
                None => term,
            });
        }
        let acc = match acc {
            Some(a) => a,
            None => self.f(0.0),
        };
        if r[3] == 0.0 {
            return acc;
        }
        let n = self.num(pc + 1 + i, 3);
        self.add(acc, n)
    }

    /// The kernel of the slot at `pc`'s op at the current point: its body spliced in, its numbers
    /// read from the program, its grid from the program's payload.
    fn splice(&mut self, pc: usize) -> Result<Val, String> {
        let slot = self.t[pc][1] as usize;
        let k = std::mem::replace(&mut self.splices[slot], Splice { prog: Program::default(), points: 0, dist: 0, grid: None, control: None, i: Var(0), work: None, at: 0, shared_base: 0, defs: HashMap::new() });
        let frame_base = self.frame;
        self.frame += k.prog.frame_words;
        let mut vmap: Vec<Val> = k.prog.vals.iter().map(|t| { self.p.vals.push(*t); Val(self.p.vals.len() as u32 - 1) }).collect();
        let i = self.i;
        let mut rmap: Vec<Var> = k.prog.vars.iter().map(|t| { self.p.vars.push(*t); Var(self.p.vars.len() as u32 - 1) }).collect();
        rmap[k.i.0 as usize] = i;
        let mut result = None;
        let Stmt::Loop { body, .. } = &k.prog.body[k.at] else { unreachable!("the element loop") };
        let cx = SpliceCx { k: &k, pc, frame_base };
        let prelude: Vec<Stmt> = k.prog.body[..k.at].to_vec();
        for s in prelude.iter().filter(|s| !matches!(s, Stmt::Set(v, _) if *v == k.i)) {
            let mapped = self.map_stmt(s, &cx, &mut vmap, &rmap, &mut result, 0)?;
            self.out.extend(mapped);
        }
        for s in body {
            // the element loop's own exits and step
            if let Stmt::If(_, t, e) = s {
                if matches!((ir::unmarked(t).next(), ir::unmarked(t).nth(1)), (Some(Stmt::Break(0)), None)) && ir::unmarked(e).next().is_none() {
                    continue;
                }
            }
            if matches!(s, Stmt::Set(v, _) if *v == k.i) {
                continue;
            }
            let mapped = self.map_stmt(s, &cx, &mut vmap, &rmap, &mut result, 0)?;
            self.out.extend(mapped);
        }
        let out = result.ok_or_else(|| "a kernel that writes no distance".to_string());
        self.splices[slot] = k;
        out
    }

    fn map_stmt(&mut self, s: &Stmt, cx: &SpliceCx<'_>, vmap: &mut Vec<Val>, rmap: &[Var], result: &mut Option<Val>, depth: u32) -> Result<Vec<Stmt>, String> {
        let k = cx.k;
        let v = |x: &Val, vmap: &Vec<Val>| vmap[x.0 as usize];
        Ok(match s {
            Stmt::Mark(n) => vec![Stmt::Mark(*n)],
            Stmt::Def(d, op) => {
                // what reads the host's buffers and parameters is read from here instead
                match op {
                    Op::Load { region: Region::Buf(b), base, extent, off } => {
                        if *b == k.points {
                            let lane = point_lane(k, *base, *off)?;
                            vmap[d.0 as usize] = self.q[lane];
                            return Ok(Vec::new());
                        }
                        if Some(*b) == k.control {
                            return Ok(vec![Stmt::Def(v(d, vmap), Op::ConstI(0))]);
                        }
                        if Some(*b) == k.grid {
                            let mut out = Vec::new();
                            // the payload's first word: the op's own texel plus its parameter 0
                            let at = self.val(Ty::F32);
                            out.push(Stmt::Def(at, Op::Load { region: Region::Buf(VALUES), base: (cx.pc as u32 + 1) * 4, extent: 1, off: None }));
                            let ati = self.val(Ty::I32);
                            out.push(Stmt::Def(ati, Op::Un(Un::F2I, at)));
                            let four = self.val(Ty::I32);
                            out.push(Stmt::Def(four, Op::ConstI(4)));
                            let words = self.val(Ty::I32);
                            out.push(Stmt::Def(words, Op::Bin(Bin::MulI, ati, four)));
                            let mapped = match off {
                                Some(o) => {
                                    let sum = self.val(Ty::I32);
                                    out.push(Stmt::Def(sum, Op::Bin(Bin::AddI, words, v(o, vmap))));
                                    sum
                                }
                                None => words,
                            };
                            let base = base + cx.pc as u32 * 4;
                            out.push(Stmt::Def(v(d, vmap), Op::Load { region: Region::Buf(VALUES), base, extent: *extent, off: Some(mapped) }));
                            return Ok(out);
                        }
                        return Err(format!("a kernel reads a buffer of its own ({b})"));
                    }
                    Op::Load { region: Region::Ctx, base, off: None, .. } if *base >= K_PARAMS => {
                        let word = (cx.pc as u32 + 1) * 4 + (base - K_PARAMS);
                        return Ok(vec![Stmt::Def(v(d, vmap), Op::Load { region: Region::Buf(VALUES), base: word, extent: 1, off: None })]);
                    }
                    Op::Load { region: Region::Frame, base, extent, off } => {
                        let off = off.map(|o| v(&o, vmap));
                        return Ok(vec![Stmt::Def(v(d, vmap), Op::Load { region: Region::Frame, base: base + cx.frame_base, extent: *extent, off })]);
                    }
                    Op::Load { region: Region::Shared, base, extent, off } => {
                        let off = off.map(|o| v(&o, vmap));
                        return Ok(vec![Stmt::Def(v(d, vmap), Op::Load { region: Region::Shared, base: base + k.shared_base, extent: *extent, off })]);
                    }
                    Op::BufLen(b) if Some(*b) == k.grid => {
                        // word 13 of the grid's header: its word count
                        let at = self.val(Ty::F32);
                        let hdr = (cx.pc as u32 + 1) * 4;
                        let mut out = vec![Stmt::Def(at, Op::Load { region: Region::Buf(VALUES), base: hdr, extent: 1, off: None })];
                        let ati = self.val(Ty::I32);
                        out.push(Stmt::Def(ati, Op::Un(Un::F2I, at)));
                        let four = self.val(Ty::I32);
                        out.push(Stmt::Def(four, Op::ConstI(4)));
                        let off = self.val(Ty::I32);
                        out.push(Stmt::Def(off, Op::Bin(Bin::MulI, ati, four)));
                        let n = self.val(Ty::F32);
                        out.push(Stmt::Def(n, Op::Load { region: Region::Buf(VALUES), base: cx.pc as u32 * 4 + 13, extent: 1, off: Some(off) }));
                        out.push(Stmt::Def(v(d, vmap), Op::Un(Un::F2I, n)));
                        return Ok(out);
                    }
                    Op::BufLen(b) => return Err(format!("a kernel asks for the length of buffer {b}")),
                    _ => {}
                }
                vec![Stmt::Def(v(d, vmap), ir::map_op(op, vmap, rmap))]
            }
            Stmt::Set(r, x) => {
                if Some(*r) == k.work {
                    return Ok(Vec::new());
                }
                vec![Stmt::Set(rmap[r.0 as usize], v(x, vmap))]
            }
            Stmt::Store { region: Region::Buf(b), val, .. } if *b == k.dist => {
                if depth != 0 {
                    return Err("a kernel that writes its distance under a branch".to_string());
                }
                *result = Some(v(val, vmap));
                Vec::new()
            }
            Stmt::Store { region: Region::Frame, base, extent, off, val } => {
                let off = off.map(|o| v(&o, vmap));
                vec![Stmt::Store { region: Region::Frame, base: base + cx.frame_base, extent: *extent, off, val: v(val, vmap) }]
            }
            // an open-ended loop that passed its cap: its stop is the language's, reported by the run
            Stmt::Store { region: Region::Ctx, base: K_OVERFLOW, extent, off: None, val } => {
                vec![Stmt::Store { region: Region::Ctx, base: K_OVERFLOW, extent: *extent, off: None, val: v(val, vmap) }]
            }
            Stmt::Store { region, base, .. } => return Err(format!("a kernel writes {region:?} word {base}")),
            Stmt::If(c, t, e) => {
                let c = v(c, vmap);
                let mut tt = Vec::new();
                for s in t { tt.extend(self.map_stmt(s, cx, vmap, rmap, result, depth + 1)?); }
                let mut ee = Vec::new();
                for s in e { ee.extend(self.map_stmt(s, cx, vmap, rmap, result, depth + 1)?); }
                vec![Stmt::If(c, tt, ee)]
            }
            Stmt::Loop { cap, body } => {
                let mut b = Vec::new();
                for s in body { b.extend(self.map_stmt(s, cx, vmap, rmap, result, depth + 1)?); }
                vec![Stmt::Loop { cap: *cap, body: b }]
            }
            Stmt::Break(d) => vec![Stmt::Break(*d)],
            Stmt::Continue(d) => vec![Stmt::Continue(*d)],
            Stmt::CallHost { .. } => return Err("a kernel that calls the host".to_string()),
            Stmt::Port { .. } => return Err("a kernel that calls a port".to_string()),
            Stmt::Out { .. } => return Err("a kernel that writes audio".to_string()),
            Stmt::Call { .. } => unreachable!("the kernel's calls are inlined"),
        })
    }
}

struct SpliceCx<'a> {
    k: &'a Splice,
    pc: usize,
    frame_base: u32,
}

/// Which coordinate a load of the point buffer reads: its offset is `3 e + c`, e the element.
fn point_lane(k: &Splice, base: u32, off: Option<Val>) -> Result<usize, String> {
    fn linear(k: &Splice, v: Val, depth: u32) -> Option<(i64, i64)> {
        if depth > 16 {
            return None;
        }
        match k.defs.get(&v.0)? {
            Op::ConstI(c) => Some((0, *c as i64)),
            Op::Get(g) if *g == k.i => Some((1, 0)),
            Op::Load { region: Region::Ctx, base, .. } if *base == K_BASE => Some((0, 0)),
            Op::Bin(Bin::AddI, a, b) => {
                let (x, y) = (linear(k, *a, depth + 1)?, linear(k, *b, depth + 1)?);
                Some((x.0 + y.0, x.1 + y.1))
            }
            Op::Bin(Bin::SubI, a, b) => {
                let (x, y) = (linear(k, *a, depth + 1)?, linear(k, *b, depth + 1)?);
                Some((x.0 - y.0, x.1 - y.1))
            }
            Op::Bin(Bin::MulI, a, b) => {
                let (x, y) = (linear(k, *a, depth + 1)?, linear(k, *b, depth + 1)?);
                if x.0 == 0 {
                    Some((y.0 * x.1, y.1 * x.1))
                } else if y.0 == 0 {
                    Some((x.0 * y.1, x.1 * y.1))
                } else {
                    None
                }
            }
            Op::Bin(Bin::ShlI, a, b) => {
                let (x, y) = (linear(k, *a, depth + 1)?, linear(k, *b, depth + 1)?);
                (y.0 == 0).then(|| (x.0 << y.1, x.1 << y.1))
            }
            _ => None,
        }
    }
    let (a, c) = match off {
        Some(o) => linear(k, o, 0).ok_or("a kernel's point offset is not 3 e + c")?,
        None => (0, 0),
    };
    let lane = base as i64 + c;
    if a != 3 || !(0..3).contains(&lane) {
        return Err(format!("a kernel's point read at {a} e + {lane}"));
    }
    Ok(lane as usize)
}

/// A kernel's program as words, taken apart for splicing.
fn splice_of(slot: &KernelSlot, shared: &mut Vec<u32>) -> Result<Splice, String> {
    let field = slot.splash().ok_or("a kernel slot without a program")?;
    let kernel = field.kernel();
    let mut words = vec![0u32; kernel.air_word_count()];
    kernel.air_words(&mut words);
    let prog = ir::from_words(&words).ok_or("a kernel's program did not read back")?;
    let index = |name: &str| kernel.buffer_index(name).map(|b| b as u8);
    let (Some(points), Some(dist)) = (index("sdf_points"), index("sdf_dist")) else {
        return Err("not a field kernel".to_string());
    };
    let at = prog.body.iter().rposition(|s| matches!(s, Stmt::Loop { cap, .. } if *cap == ELEMENT_CAP)).ok_or("a kernel without an element loop")?;
    let Stmt::Loop { body, .. } = &prog.body[at] else { unreachable!() };
    let Some(Stmt::Set(i, _)) = ir::unmarked(body).next_back() else { return Err("a kernel's element loop without its step".to_string()) };
    let i = *i;
    let mut defs = HashMap::new();
    for s in prog.body[..at].iter().chain(body) {
        if let Stmt::Def(v, op) = s {
            defs.insert(v.0, *op);
        }
    }
    // what runs after the loop stores the work counter
    let mut post = HashMap::new();
    let mut work = None;
    for s in &prog.body[at + 1..] {
        match s {
            Stmt::Def(v, op) => { post.insert(v.0, *op); }
            Stmt::Store { region: Region::Ctx, base, val, .. } if *base == K_WORK => {
                if let Some(Op::Get(w)) = post.get(&val.0).or_else(|| defs.get(&val.0)) {
                    work = Some(*w);
                }
            }
            _ => {}
        }
    }
    let shared_base = shared.len() as u32;
    let mut table = vec![0u32; kernel.shared_word_count()];
    kernel.shared_copy(&mut table);
    shared.extend_from_slice(&table);
    Ok(Splice { prog, points, dist, grid: index("sdf_grid"), control: index("#control"), i, work, at, shared_base, defs })
}

/// The element loop over the program at `t`, and its tables. `sites`: the leaves per point.
/// `grad`: the gradient's code (the value and its partials), else the value's (value, bound,
/// solid depth and, with `slots`, the slot).
fn build(t: &[[f32; 4]], kernels: &[KernelSlot], slots: bool, capped: &[bool], grad: bool) -> Result<(Program, Vec<u32>), Stop> {
    let (sites, caps) = (capped.len(), capped.contains(&true));
    let mut shared = Vec::new();
    let mut splices = Vec::with_capacity(kernels.len());
    for k in kernels {
        splices.push(splice_of(k, &mut shared).map_err(|why| refuse(k.key, why))?);
    }
    let slots = slots && !grad;
    let mut b = Build { t, kernels, p: Program::default(), out: Vec::new(), splices, i: Var(0), q: [Val(0); 3], frame: 0, slots, hosted: None, caps, capped, paths: Vec::new() };
    // the shape `lower_kernel` emits
    let n = b.def(Ty::I32, Op::FrameCount);
    let base = b.def(Ty::I32, Op::Load { region: Region::Ctx, base: K_BASE, extent: 1, off: None });
    let i = b.var(Ty::I32);
    b.i = i;
    let zero = b.def(Ty::I32, Op::ConstI(0));
    b.out.push(Stmt::Set(i, zero));
    let prelude = std::mem::take(&mut b.out);
    let iv = b.def(Ty::I32, Op::Get(i));
    let done = b.def(Ty::Bool, Op::CmpI(Cmp::Ge, iv, n));
    b.out.push(Stmt::If(done, vec![Stmt::Break(0)], Vec::new()));
    let cancel = b.def(Ty::I32, Op::Load { region: Region::Buf(CONTROL), base: 0, extent: 1, off: None });
    let zero2 = b.def(Ty::I32, Op::ConstI(0));
    let stop = b.def(Ty::Bool, Op::CmpI(Cmp::Ne, cancel, zero2));
    b.out.push(Stmt::If(stop, vec![Stmt::Break(0)], Vec::new()));
    let e = b.def(Ty::I32, Op::Bin(Bin::AddI, base, iv));
    let three = b.def(Ty::I32, Op::ConstI(3));
    let e3 = b.def(Ty::I32, Op::Bin(Bin::MulI, e, three));
    let mut q = [Val(0); 3];
    for (k, c) in q.iter_mut().enumerate() {
        *c = b.def(Ty::F32, Op::Load { region: Region::Buf(POINTS), base: k as u32, extent: u32::MAX, off: Some(e3) });
    }
    b.q = q;
    let mut seeds = Seeds { points: POINTS, leaves: None };
    if sites > 0 {
        let per = b.def(Ty::I32, Op::ConstI(sites as i32));
        let value_off = b.def(Ty::I32, Op::Bin(Bin::MulI, e, per));
        let at_off = b.def(Ty::I32, Op::Bin(Bin::MulI, value_off, three));
        if grad {
            let nine = b.def(Ty::I32, Op::ConstI(9));
            let tangent_off = b.def(Ty::I32, Op::Bin(Bin::MulI, value_off, nine));
            b.hosted = Some(Hosted { at: G_AT, value: G_VALUE, slot: 0, cap: G_GRAD + 1, lo: G_GRAD + 2, hi: G_GRAD + 3, at_off, value_off });
            seeds.leaves = Some(diff::LeafSeeds { at: G_AT, value: G_VALUE, tangent: G_TANGENT, grad: G_GRAD, at_off, tangent_off });
        } else {
            let at = SLOT + slots as u8;
            let cap = at + 2 + slots as u8;
            b.hosted = Some(Hosted { at, value: at + 1, slot: at + 2, cap, lo: cap + 1, hi: cap + 2, at_off, value_off });
        }
    }
    let top = b.run()?;
    if grad {
        b.p.frame_words = b.frame;
        let body = std::mem::take(&mut b.out);
        let (body, partials) = diff::forward(&mut b.p, &body, &seeds);
        b.frame = b.p.frame_words;
        b.out = body;
        // after the partials: the caps have none
        if caps {
            b.store_caps()?;
        }
        b.out.push(Stmt::Store { region: Region::Buf(DIST), base: 0, extent: u32::MAX, off: Some(e), val: top.v });
        for (k, d) in partials.of(top.v).into_iter().enumerate() {
            let val = match d {
                D::Zero => b.f(0.0),
                D::Const(c) => b.f(c as f32),
                D::V(v) => v,
            };
            b.out.push(Stmt::Store { region: Region::Buf(GRAD), base: k as u32, extent: u32::MAX, off: Some(e3), val });
        }
    } else {
        if caps {
            b.store_caps()?;
        }
        let bound = top.b.unwrap_or(top.v);
        let deep = match top.deep {
            Some(d) => d,
            None => b.neg(top.v),
        };
        b.out.push(Stmt::Store { region: Region::Buf(DIST), base: 0, extent: u32::MAX, off: Some(e), val: top.v });
        b.out.push(Stmt::Store { region: Region::Buf(BOUND), base: 0, extent: u32::MAX, off: Some(e), val: bound });
        b.out.push(Stmt::Store { region: Region::Buf(SOLID), base: 0, extent: u32::MAX, off: Some(e), val: deep });
        if slots {
            let s = match top.s {
                Some(s) => s,
                None => b.f(0.0),
            };
            b.out.push(Stmt::Store { region: Region::Buf(SLOT), base: 0, extent: u32::MAX, off: Some(e), val: s });
        }
    }
    let one = b.def(Ty::I32, Op::ConstI(1));
    let next = b.def(Ty::I32, Op::Bin(Bin::AddI, iv, one));
    b.out.push(Stmt::Set(i, next));
    let body = std::mem::take(&mut b.out);
    let mut program = b.p;
    program.body = prelude;
    program.body.push(Stmt::Loop { cap: ELEMENT_CAP, body });
    program.frame_words = b.frame;
    Ok((program, shared))
}

impl Build<'_> {
    /// The program's ops over the current point, in order; what stays on its stack.
    fn run(&mut self) -> Result<Entry, Stop> {
        let t = self.t;
        let end = t[0][0] as usize;
        let mut vs: Vec<Entry> = Vec::new();
        // the caps' bounds of each entry of `vs`
        let mut cs: Vec<Bounds> = Vec::new();
        let mut ps: Vec<[Val; 3]> = Vec::new();
        let mut ms: Vec<Option<Val>> = Vec::new();
        let mut pc = 1;
        while pc < end {
            let h = t[pc];
            let code = h[0];
            let slot_now = |ms: &Vec<Option<Val>>| ms.last().copied().flatten();
            if code == op::PRIM {
                let v = self.prim(pc);
                vs.push(Entry { v, b: None, deep: None, s: slot_now(&ms) });
                cs.push(Bounds::known(v));
            } else if code == op::KERNEL {
                let v = self.splice(pc).map_err(|why| refuse(self.kernels[h[1] as usize].key, why))?;
                vs.push(Entry { v, b: None, deep: None, s: slot_now(&ms) });
                cs.push(Bounds::known(v));
            } else if code == op::LEAF {
                let host = self.hosted.ok_or("a leaf with no sites")?;
                let site = h[1] as u32;
                for c in 0..3u32 {
                    self.out.push(Stmt::Store { region: Region::Buf(host.at), base: site * 3 + c, extent: u32::MAX, off: Some(host.at_off), val: self.q[c as usize] });
                }
                let v = self.def(Ty::F32, Op::Load { region: Region::Buf(host.value), base: site, extent: u32::MAX, off: Some(host.value_off) });
                // a leaf's slot is its own where it names one
                let s = if self.slots {
                    let own = self.def(Ty::F32, Op::Load { region: Region::Buf(host.slot), base: site, extent: u32::MAX, off: Some(host.value_off) });
                    let zero = self.f(0.0);
                    let inherit = slot_now(&ms).unwrap_or(zero);
                    let names = self.lt(zero, own);
                    Some(self.sel(names, own, inherit))
                } else {
                    None
                };
                vs.push(Entry { v, b: None, deep: None, s });
                // a leaf answered in full is its own bounds
                cs.push(if self.capped[site as usize] {
                    let lo = self.def(Ty::F32, Op::Load { region: Region::Buf(host.lo), base: site, extent: u32::MAX, off: Some(host.value_off) });
                    let hi = self.def(Ty::F32, Op::Load { region: Region::Buf(host.hi), base: site, extent: u32::MAX, off: Some(host.value_off) });
                    Bounds { fixed: false, lo: Some(lo), hi: Some(hi), paths: vec![(site, Vec::new())] }
                } else {
                    Bounds::known(v)
                });
            } else if code == op::XFORM_PUSH {
                ps.push(self.q);
                let v = self.q;
                self.q = [self.row(pc, 0, v), self.row(pc, 1, v), self.row(pc, 2, v)];
            } else if code == op::XFORM_POP {
                self.q = ps.pop().ok_or("an unbalanced transform")?;
                let top = vs.last_mut().ok_or("an empty stack")?;
                let e = *top;
                let n = self.num(pc, 1);
                let v = self.mul(e.v, n);
                let b = e.b.map(|b| self.mul(b, n));
                let deep = e.deep.map(|d| self.mul(d, n));
                *vs.last_mut().unwrap() = Entry { v, b, deep, s: e.s };
                let c = cs.pop().ok_or("an empty stack")?;
                cs.push(self.scaled(c, v, n, true));
            } else if code == op::DEFORM_PUSH {
                ps.push(self.q);
                let [x, y, z] = self.q;
                let (kind, axis) = (h[1], h[3]);
                let rate = self.num(pc, 2);
                self.q = if kind == 0.0 {
                    let a = self.mul(rate, y);
                    let (s, c) = self.sin_cos(a);
                    let (m0, m1) = (self.mul(c, x), self.mul(s, z));
                    let (m2, m3) = (self.mul(c, z), self.mul(s, x));
                    [self.add(m0, m1), y, self.sub(m2, m3)]
                } else if kind == 1.0 {
                    let a = self.mul(rate, x);
                    let (s, c) = self.sin_cos(a);
                    let (m0, m1) = (self.mul(c, x), self.mul(s, y));
                    let (m2, m3) = (self.mul(c, y), self.mul(s, x));
                    [self.add(m0, m1), self.sub(m2, m3), z]
                } else if kind == 2.0 {
                    let one = self.f(1.0);
                    let m = self.mul(rate, y);
                    let d = self.add(one, m);
                    let tiny = self.f(0.001f64 as f32);
                    let d = self.max(d, tiny);
                    let k = self.div(one, d);
                    [self.mul(x, k), y, self.mul(z, k)]
                } else if axis == 0.0 {
                    [self.abs(x), y, z]
                } else if axis == 1.0 {
                    [x, self.abs(y), z]
                } else {
                    [x, y, self.abs(z)]
                };
            } else if code == op::DEFORM_POP {
                self.q = ps.pop().ok_or("an unbalanced deform")?;
                let e = *vs.last().ok_or("an empty stack")?;
                let n = self.num(pc, 1);
                let v = self.div(e.v, n);
                let b = e.b.map(|b| self.div(b, n));
                let deep = e.deep.map(|d| self.div(d, n));
                *vs.last_mut().unwrap() = Entry { v, b, deep, s: e.s };
                let c = cs.pop().ok_or("an empty stack")?;
                cs.push(self.scaled(c, v, n, false));
            } else if code == op::FOLD {
                let e = vs.pop().ok_or("an empty stack")?;
                let d = vs.pop().ok_or("an empty stack")?;
                let smooth = h[2] > 0.0;
                let k = if smooth { Some(self.num(pc, 2)) } else { None };
                let (v, s) = if h[1] == 0.0 {
                    let v = self.smin(d.v, e.v, k);
                    (v, self.slot_pick(e, d, true))
                } else if h[1] == 1.0 {
                    let nd = self.neg(d.v);
                    let m = self.smin(nd, e.v, k);
                    (self.neg(m), d.s)
                } else {
                    let (nd, ne) = (self.neg(d.v), self.neg(e.v));
                    let m = self.smin(nd, ne, k);
                    (self.neg(m), self.slot_pick(e, d, false))
                };
                vs.push(Entry { v, b: None, deep: None, s });
                let ce = cs.pop().ok_or("an empty stack")?;
                let cd = cs.pop().ok_or("an empty stack")?;
                cs.push(self.folded(h[1], cd, ce, k, v));
            } else if code == op::SHELL {
                let e = *vs.last().ok_or("an empty stack")?;
                let a = self.abs(e.v);
                let n = self.num(pc, 1);
                let v = self.sub(a, n);
                *vs.last_mut().unwrap() = Entry { v, b: None, deep: None, s: e.s };
                let c = cs.pop().ok_or("an empty stack")?;
                cs.push(if c.fixed { Bounds::known(v) } else { Bounds { lo: None, hi: None, ..c.through(Step::Opaque) } });
            } else if code == op::ROUND {
                let e = *vs.last().ok_or("an empty stack")?;
                let n = self.num(pc, 1);
                let v = self.sub(e.v, n);
                *vs.last_mut().unwrap() = Entry { v, b: None, deep: None, s: e.s };
                let c = cs.pop().ok_or("an empty stack")?;
                cs.push(self.lessened(c, v, n));
            } else if code == op::SCALE {
                let e = *vs.last().ok_or("an empty stack")?;
                let n = self.num(pc, 1);
                let v = self.mul(e.v, n);
                let b = e.b.map(|b| self.mul(b, n));
                let deep = e.deep.map(|d| self.mul(d, n));
                *vs.last_mut().unwrap() = Entry { v, b, deep, s: e.s };
                let c = cs.pop().ok_or("an empty stack")?;
                cs.push(self.scaled(c, v, n, true));
            } else if code == op::MAT_PUSH {
                let s = if self.slots { Some(self.f(h[1])) } else { None };
                ms.push(s);
            } else if code == op::MAT_POP {
                ms.pop();
            } else if code == op::MAT_WITHIN {
                let e = vs.pop().ok_or("an empty stack")?;
                // a region gives slots alone
                let c = cs.pop().ok_or("an empty stack")?;
                self.paths.extend(c.through(Step::Opaque).paths);
                let s = if self.slots {
                    let zero = self.f(0.0);
                    let inherit = match slot_now(&ms) {
                        Some(s) => s,
                        None => zero,
                    };
                    let own = self.f(h[1]);
                    let outside = self.lt(zero, e.v);
                    Some(self.sel(outside, inherit, own))
                } else {
                    None
                };
                ms.push(s);
            } else if code == op::TEX {
                let triplanar = h[1] > 0.5;
                let grad = h[2] > 0.5;
                let np = if triplanar { 3 } else { 1 };
                let n = vs.len();
                let di = n - np - 1;
                let d = vs[di];
                let two = self.f(2.0);
                let three = self.f(3.0);
                let zero = self.f(0.0);
                let one = self.f(1.0);
                let (mut gx, mut gy, mut gz) = (zero, zero, zero);
                let mut at = di;
                if grad {
                    let (g0, g1, g2, g3) = (vs[di - 4].v, vs[di - 3].v, vs[di - 2].v, vs[di - 1].v);
                    let a1 = self.num(pc + 1, 1);
                    let (x, y, z) = {
                        let s = self.sub(g0, g1);
                        let s = self.sub(s, g2);
                        let x = self.add(s, g3);
                        let n0 = self.neg(g0);
                        let s = self.sub(n0, g1);
                        let s = self.add(s, g2);
                        let y = self.add(s, g3);
                        let s = self.add(n0, g1);
                        let s = self.sub(s, g2);
                        let z = self.add(s, g3);
                        (x, y, z)
                    };
                    gx = self.mul(x, a1);
                    gy = self.mul(y, a1);
                    gz = self.mul(z, a1);
                    at = di - 4;
                }
                let (m0, m1, m2, m3) = (self.num(pc + 2, 0), self.num(pc + 2, 1), self.num(pc + 2, 2), self.num(pc + 2, 3));
                let top_n = self.sub(m1, gy);
                let top_d = self.sub(m1, m0);
                let mt = self.div(top_n, top_d);
                let mt = self.clamp(mt, zero, one);
                let bot_n = self.add(m3, gy);
                let bot_d = self.sub(m3, m2);
                let mb = self.div(bot_n, bot_d);
                let mb = self.clamp(mb, zero, one);
                let smooth = |b: &mut Self, x: Val| {
                    let xx = b.mul(x, x);
                    let tx = b.mul(two, x);
                    let r = b.sub(three, tx);
                    b.mul(xx, r)
                };
                let (mt, mb) = (smooth(self, mt), smooth(self, mb));
                let mut pat = vs[n - 1].v;
                if triplanar {
                    let (xx, yy, zz) = (self.mul(gx, gx), self.mul(gy, gy), self.mul(gz, gz));
                    let s = self.add(xx, yy);
                    let s = self.add(s, zz);
                    let s = self.max(s, one);
                    let (wx, wy, wz) = (self.div(xx, s), self.div(yy, s), self.div(zz, s));
                    let (t0, t1, t2) = (self.mul(wx, vs[n - 3].v), self.mul(wy, vs[n - 2].v), self.mul(wz, vs[n - 1].v));
                    let s = self.add(t0, t1);
                    pat = self.add(s, t2);
                }
                let depth = self.num(pc, 3);
                let masks = self.mul(mt, mb);
                let disp = self.mul(masks, pat);
                let disp = self.mul(depth, disp);
                let raised = self.add(d.v, disp);
                let a0 = self.num(pc + 1, 0);
                let value = self.mul(raised, a0);
                // the most the pattern can lower the source bounds it from outside; the most it can
                // raise it, from inside
                let (b, deep) = if t[pc + 1][3] > 0.5 {
                    let lo = self.min(depth, zero);
                    let src = self.add(d.v, lo);
                    let hi = self.max(depth, zero);
                    let top = self.add(d.v, hi);
                    let (down, up) = (self.neg(value), self.neg(top));
                    (Some(self.max(value, src)), Some(self.max(down, up)))
                } else {
                    (None, None)
                };
                vs.truncate(at);
                vs.push(Entry { v: value, b, deep, s: d.s });
                let gone = cs.split_off(at);
                cs.push(if gone.iter().all(|c| c.fixed) {
                    Bounds::known(value)
                } else {
                    Bounds { paths: gone.into_iter().flat_map(|c| c.through(Step::Opaque).paths).collect(), ..Bounds::default() }
                });
            } else {
                return Err(format!("op {code} has no compiled form").into());
            }
            pc += op_len(h);
        }
        // the root's leaves, and any an unbalanced stack leaves under it
        for (k, c) in cs.into_iter().enumerate() {
            self.paths.extend(if k == 0 { c } else { c.through(Step::Opaque) }.paths);
        }
        vs.first().copied().ok_or_else(|| "a program with no value".into())
    }

    fn smin(&mut self, a: Val, b: Val, k: Option<Val>) -> Val {
        let Some(k) = k else { return self.min(a, b) };
        let (half, zero, one) = (self.f(0.5), self.f(0.0), self.f(1.0));
        let diff = self.sub(b, a);
        let m = self.mul(half, diff);
        let q = self.div(m, k);
        let h = self.add(half, q);
        let h = self.clamp(h, zero, one);
        let ab = self.sub(a, b);
        let t = self.mul(ab, h);
        let l = self.add(b, t);
        let kh = self.mul(k, h);
        let r = self.sub(one, h);
        let c = self.mul(kh, r);
        let blended = self.sub(l, c);
        // Outside the blend the nearer operand exactly, as csg_sdf's smooth minimum: `b + (a - b)`
        // rounds, so a far operand would move the value's last bits.
        let below = self.def(Ty::Bool, Op::CmpF(Cmp::Le, h, zero));
        let inner = self.sel(below, b, blended);
        let above = self.def(Ty::Bool, Op::CmpF(Cmp::Ge, h, one));
        self.sel(above, a, inner)
    }

    /// `x` raised past what f32 rounding of the ops a cap passes can take from it: by 2^-14 of
    /// `|x|` and of `k` (a blend's radius), and by 2^-100 (so that it rises even from zero). An
    /// infinite `x` stays as it is.
    fn up(&mut self, x: Val, k: Option<Val>) -> Val {
        let m = self.margin(x, k);
        self.add(x, m)
    }

    /// `x` lowered as [`Build::up`] raises it.
    fn down(&mut self, x: Val, k: Option<Val>) -> Val {
        let m = self.margin(x, k);
        self.sub(x, m)
    }

    fn margin(&mut self, x: Val, k: Option<Val>) -> Val {
        let a = self.abs(x);
        let most = self.f(f32::MAX);
        let a = self.min(a, most);
        let s = match k {
            Some(k) => self.add(a, k),
            None => a,
        };
        let (rel, least) = (self.f(1.0 / 16384.0), self.f(2.0f32.powi(-100)));
        let m = self.mul(s, rel);
        self.add(m, least)
    }

    fn ge(&mut self, x: Val, y: Val) -> Val {
        self.def(Ty::Bool, Op::CmpF(Cmp::Ge, x, y))
    }

    /// The bounds of an entry times `n` (`times`) or over it.
    fn scaled(&mut self, c: Bounds, v: Val, n: Val, times: bool) -> Bounds {
        if c.fixed {
            return Bounds::known(v);
        }
        if !self.caps {
            return c;
        }
        let (zero, inf, ninf) = (self.f(0.0), self.f(f32::INFINITY), self.f(f32::NEG_INFINITY));
        let pos = self.lt(zero, n);
        let by = |b: &mut Self, x: Val, raise: bool| {
            let y = if times { b.mul(x, n) } else { b.div(x, n) };
            let y = if raise { b.up(y, None) } else { b.down(y, None) };
            b.sel(pos, y, if raise { inf } else { ninf })
        };
        let lo = c.lo.map(|x| by(self, x, false));
        let hi = c.hi.map(|x| by(self, x, true));
        Bounds { lo, hi, ..c.through(Step::Scale { n, times }) }
    }

    /// The bounds of an entry less `r`.
    fn lessened(&mut self, c: Bounds, v: Val, r: Val) -> Bounds {
        if c.fixed {
            return Bounds::known(v);
        }
        if !self.caps {
            return c;
        }
        let lo = c.lo.map(|x| {
            let y = self.sub(x, r);
            self.down(y, None)
        });
        let hi = c.hi.map(|x| {
            let y = self.sub(x, r);
            self.up(y, None)
        });
        Bounds { lo, hi, ..c.through(Step::Less(r)) }
    }

    /// The bounds of a fold of `d` and `e` (`kind` 0 a union, 1 a cut, 2 an intersection), each
    /// operand's leaves passed through it beside the other's bounds. A smooth union is at most
    /// its operands and at least their least less a quarter of its radius; a cut at least its base
    /// and the cutter's opposite, an intersection at least each operand, and either at most the
    /// greater of the two plus that quarter.
    fn folded(&mut self, kind: f32, d: Bounds, e: Bounds, k: Option<Val>, v: Val) -> Bounds {
        if d.fixed && e.fixed {
            return Bounds::known(v);
        }
        if !self.caps {
            return Bounds::default();
        }
        let quarter = k.map(|k| {
            let q = self.f(0.25);
            self.mul(k, q)
        });
        let plus = |b: &mut Self, x: Val| match quarter {
            Some(q) => b.add(x, q),
            None => x,
        };
        let both = |b: &mut Self, x: Option<Val>, y: Option<Val>, f: fn(&mut Self, Val, Val) -> Val| match (x, y) {
            (Some(x), Some(y)) => Some(f(b, x, y)),
            _ => None,
        };
        let either = |b: &mut Self, x: Option<Val>, y: Option<Val>, f: fn(&mut Self, Val, Val) -> Val| match (x, y) {
            (Some(x), Some(y)) => Some(f(b, x, y)),
            (x, None) => x,
            (None, y) => y,
        };
        let (dl, dh, el, eh) = (d.lo, d.hi, e.lo, e.hi);
        let (lo, hi, sd, se) = if kind == 0.0 {
            let hi = either(self, dh, eh, Self::min).map(|x| self.up(x, k));
            let lo = both(self, dl, el, Self::min).map(|x| {
                let x = match quarter {
                    Some(q) => self.sub(x, q),
                    None => x,
                };
                self.down(x, k)
            });
            (lo, hi, Step::Union { lo: el, hi: eh, k }, Step::Union { lo: dl, hi: dh, k })
        } else if kind == 1.0 {
            let away = eh.map(|x| self.neg(x));
            let lo = either(self, dl, away, Self::max).map(|x| self.down(x, k));
            let near = el.map(|x| self.neg(x));
            let hi = both(self, dh, near, Self::max).map(|x| {
                let x = plus(self, x);
                self.up(x, k)
            });
            (lo, hi, Step::Above { other: away, k }, Step::Cutter { lo: dl, k })
        } else {
            let lo = either(self, dl, el, Self::max).map(|x| self.down(x, k));
            let hi = both(self, dh, eh, Self::max).map(|x| {
                let x = plus(self, x);
                self.up(x, k)
            });
            (lo, hi, Step::Above { other: el, k }, Step::Above { other: dl, k })
        };
        let mut paths = d.through(sd).paths;
        paths.extend(e.through(se).paths);
        Bounds { fixed: false, lo, hi, paths }
    }

    /// Each capped leaf's cap at the point, from the root down its path: `+inf` where none
    /// holds. A leaf answered in full has none written: its cap stays `+inf` ([`LeafIo::new`]).
    fn store_caps(&mut self) -> Result<(), Stop> {
        let host = self.hosted.ok_or("a leaf with no sites")?;
        let mut seen = vec![false; self.capped.len()];
        for (site, path) in std::mem::take(&mut self.paths) {
            let at = seen.get_mut(site as usize).ok_or("a leaf past the sites")?;
            if std::mem::replace(at, true) {
                return Err("a leaf on two paths".into());
            }
            let mut t = None;
            for step in path.iter().rev() {
                t = self.pass(*step, t);
            }
            let inf = self.f(f32::INFINITY);
            let cap = match t {
                // not a number: no cap
                Some(c) => {
                    let ok = self.lt(c, inf);
                    self.sel(ok, c, inf)
                }
                None => inf,
            };
            self.out.push(Stmt::Store { region: Region::Buf(host.cap), base: site, extent: u32::MAX, off: Some(host.value_off), val: cap });
        }
        if seen != self.capped {
            return Err("a capped leaf on no path".into());
        }
        Ok(())
    }

    /// The cap an operand takes from `t` (`None`: no cap) through `step`. Where a check fails
    /// or its numbers are not numbers, the cap is `+inf`: none.
    fn pass(&mut self, step: Step, t: Option<Val>) -> Option<Val> {
        match step {
            Step::Scale { n, times } => {
                let t = t?;
                let x = if times { self.div(t, n) } else { self.mul(t, n) };
                let x = self.up(x, None);
                let (zero, inf) = (self.f(0.0), self.f(f32::INFINITY));
                let pos = self.lt(zero, n);
                Some(self.sel(pos, x, inf))
            }
            Step::Less(r) => {
                let x = self.add(t?, r);
                Some(self.up(x, None))
            }
            Step::Opaque => None,
            Step::Union { lo, hi, k } => {
                let own = hi.map(|hi| self.saturating(hi, k));
                let shared = match (t, lo) {
                    (Some(t), Some(lo)) => Some(self.both_past(t, lo, k)),
                    _ => None,
                };
                self.least(own, shared)
            }
            Step::Cutter { lo, k } => {
                // the cut's smooth minimum is of the base's opposite (at most `-lo`) and the cutter
                let own = lo.map(|lo| {
                    let a = self.neg(lo);
                    self.saturating(a, k)
                });
                // a base past the threshold keeps the result past it whatever the cutter is
                let whole = match (t, lo) {
                    (Some(t), Some(lo)) => {
                        let y = self.up(t, k);
                        let past = self.ge(lo, y);
                        let (ninf, inf) = (self.f(f32::NEG_INFINITY), self.f(f32::INFINITY));
                        Some(self.sel(past, ninf, inf))
                    }
                    _ => None,
                };
                self.least(own, whole)
            }
            Step::Above { other, k } => {
                let y = self.up(t?, k);
                Some(match other {
                    Some(o) => {
                        let past = self.ge(o, y);
                        let ninf = self.f(f32::NEG_INFINITY);
                        self.sel(past, ninf, y)
                    }
                    None => y,
                })
            }
        }
    }

    /// Where an operand of a union whose other operand is at most `hi` makes no difference: past
    /// it the blend gives the other operand exactly ([`Build::smin`]), `+inf` where none holds.
    /// The operand at least `up(hi + k)` is past the other by `k` and by 2^-14 of their size,
    /// more than the blend's `h` (of a difference, a halving, a division and an addition, none
    /// fused) can lose to rounding at 2^-24 an op, so `h` is past its end and the blend picks the
    /// other operand. A radius that is not positive, and an `hi` that is not finite, have no cap.
    fn saturating(&mut self, hi: Val, k: Option<Val>) -> Val {
        let inf = self.f(f32::INFINITY);
        let x = match k {
            Some(k) => self.add(hi, k),
            None => hi,
        };
        let u = self.up(x, k);
        let past = self.lt(hi, u);
        let u = self.sel(past, u, inf);
        match k {
            Some(k) => {
                let zero = self.f(0.0);
                let blend = self.lt(zero, k);
                self.sel(blend, u, inf)
            }
            None => u,
        }
    }

    /// An operand's cap from the union's `t`, the other operand being at least `lo`: with both
    /// past `t` and a quarter of the radius (sharp: past `t`), the union is past `t`.
    fn both_past(&mut self, t: Val, lo: Val, k: Option<Val>) -> Val {
        let inf = self.f(f32::INFINITY);
        let x = match k {
            Some(k) => {
                let q = self.f(0.25);
                let quarter = self.mul(k, q);
                let x = self.add(t, quarter);
                self.up(x, Some(k))
            }
            None => t,
        };
        let past = self.ge(lo, x);
        let x = self.sel(past, x, inf);
        match k {
            Some(k) => {
                let zero = self.f(0.0);
                let blend = self.lt(zero, k);
                self.sel(blend, x, inf)
            }
            None => x,
        }
    }

    fn least(&mut self, a: Option<Val>, b: Option<Val>) -> Option<Val> {
        match (a, b) {
            (Some(a), Some(b)) => Some(self.min(a, b)),
            (a, None) => a,
            (None, b) => b,
        }
    }

    /// A fold's slot: the operand that decided it.
    fn slot_pick(&mut self, e: Entry, d: Entry, union: bool) -> Option<Val> {
        if !self.slots {
            return None;
        }
        let zero = self.f(0.0);
        let (se, sd) = (e.s.unwrap_or(zero), d.s.unwrap_or(zero));
        let c = if union { self.lt(e.v, d.v) } else { self.lt(d.v, e.v) };
        Some(self.sel(c, se, sd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_csg_sdf::links::{LinkLoop, Links};
    use makepad_csg_sdf::mesh_field::MeshData;
    use makepad_csg_sdf::strokes::{StrokeOp, Strokes};
    use makepad_csg_sdf::texture::{SdfTexture, TexProject};
    use makepad_csg_sdf::{distance_slot, SdfDeform, SdfKernel, SdfOp, SdfPlace};

    fn sphere(radius: f64) -> SdfNode {
        SdfNode::Sphere { radius }
    }
    fn cube(size: f64) -> SdfNode {
        SdfNode::Box { size: [size; 3], radius: 0.0 }
    }
    fn placed(at: [f64; 3], of: SdfNode) -> SdfNode {
        SdfNode::Transform { at, rotate: [0.0; 3], scale: 1.0, of: Box::new(of) }
    }
    fn mesh() -> SdfNode {
        let positions = (0..8).map(|i| [if i & 1 == 0 { -5.0 } else { 5.0 }, if i & 2 == 0 { -5.0 } else { 5.0 }, if i & 4 == 0 { -5.0 } else { 5.0 }]).collect();
        let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        let triangles = quads.iter().flat_map(|q: &[u32; 4]| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect();
        SdfNode::Mesh(Arc::new(MeshField::new(MeshData { positions, triangles })))
    }
    fn links() -> SdfNode {
        let ring = |at: f64, slot: u8| LinkLoop { pts: (0..12).map(|i| { let a = i as f64 * std::f64::consts::TAU / 12.0; [at + 4.0 * a.cos(), 0.0, 4.0 * a.sin()] }).collect(), slot };
        SdfNode::Links(Arc::new(Links::new(vec![ring(-3.0, 0), ring(3.0, 2)], 0.7, None)))
    }
    fn sculpted(of: SdfNode) -> SdfNode {
        let mut strokes = Strokes::new();
        strokes.push(StrokeOp::Raise, 3.0, 0.8, &[[5.0, 0.0, 0.0], [5.0, 2.0, 0.0], [5.0, 4.0, 0.0]]);
        SdfNode::Sculpt { strokes: Arc::new(strokes), of: Box::new(of) }
    }
    /// A kernel with no Splash program.
    fn opaque() -> SdfNode {
        SdfNode::Kernel(opaque_kernel())
    }
    fn opaque_kernel() -> SdfKernel {
        let ball = makepad_csg_sdf::SdfSphere::new(Vec3d::new(0.0, 0.0, 0.0), 6.0);
        SdfKernel { key: 77, field: Arc::new(ball), splash: None, bounds: Some(([-6.0; 3], [6.0; 3])), solid: None }
    }
    fn textured(of: SdfNode, top: Option<(f64, f64)>, depth: f64) -> SdfNode {
        let mut tex = SdfTexture::new(opaque_kernel(), TexProject::Axis(1), 0.2, depth, 0.5, top, None, 3.0, 0.0);
        tex.unit_pattern = true;
        SdfNode::Texture { tex: Arc::new(tex), of: Box::new(of) }
    }

    /// Points through and around a part, the same ones each run.
    fn points(reach: f64) -> Vec<f32> {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            ((seed >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) * reach
        };
        (0..3 * 1999).map(|_| rnd() as f32).collect()
    }
    fn truth(root: &SdfNode, xyz: &[f32]) -> Vec<f64> {
        let q: Vec<Vec3d> = xyz.chunks(3).map(|p| Vec3d::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect();
        let mut out = vec![0.0; q.len()];
        distance_many(root, &q, &mut out);
        out
    }
    fn eval(f: &CompiledField, xyz: &[f32]) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let n = xyz.len() / 3;
        let (mut v, mut b, mut s) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
        f.eval(xyz, &mut v, &mut b, &mut s);
        (v, b, s)
    }
    /// The code gives what the node's own query gives, to f32's rounding, and its gradient what
    /// the value's differences give.
    fn agrees(name: &str, root: &SdfNode, sites: usize) {
        let f = compile(root, false).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(f.sites(), sites, "{name}: its leaves");
        let xyz = points(14.0);
        let (v, b, s) = eval(&f, &xyz);
        let t = truth(root, &xyz);
        for (i, want) in t.iter().enumerate() {
            let tol = 2.0e-3f64.max(want.abs() * 1.0e-3);
            assert!((v[i] as f64 - want).abs() <= tol, "{name} point {i}: {} against {want}", v[i]);
            // nothing tightened it here, or a texture did, which only raises it
            assert!(b[i] >= v[i], "{name} point {i}: the bound {} under the value {}", b[i], v[i]);
            assert!(s[i] >= -v[i], "{name} point {i}: the depth {} under the value {}", s[i], v[i]);
        }
        gradient_agrees(name, &f, &xyz, &v);
    }

    fn angle(a: [f64; 3], b: [f64; 3]) -> f64 {
        let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let (la, lb) = ((a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt(), (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt());
        (dot / (la * lb)).clamp(-1.0, 1.0).acos().to_degrees()
    }

    /// The gradient's code gives the value's code's value, and a gradient within half a degree of
    /// the value's central differences wherever those agree with themselves at twice the step
    /// (no crease within their reach), at most of the points.
    pub(super) fn gradient_agrees(name: &str, f: &CompiledField, xyz: &[f32], v: &[f32]) {
        let n = v.len();
        let (mut gv, mut g) = (vec![0.0; n], vec![0.0; n * 3]);
        f.gradient.eval(xyz, &mut gv, &mut g, 1.0e-3);
        let h = 1.0e-3f32;
        let mut around = Vec::with_capacity(n * 36);
        for p in xyz.chunks(3) {
            for s in [h, 2.0 * h] {
                for k in 0..3 {
                    for sign in [1.0, -1.0] {
                        let mut q = [p[0], p[1], p[2]];
                        q[k] += sign * s;
                        around.extend(q);
                    }
                }
            }
        }
        let (d, ..) = eval(f, &around);
        let mut checked = 0;
        for i in 0..n {
            assert!((gv[i] - v[i]).abs() <= 1.0e-5 * (1.0 + v[i].abs()), "{name} point {i}: the gradient's value {} against {}", gv[i], v[i]);
            let w = &d[i * 12..i * 12 + 12];
            let diff = |o: usize, s: f32| -> [f64; 3] { std::array::from_fn(|k| ((w[o + k * 2] - w[o + k * 2 + 1]) / (2.0 * s)) as f64) };
            let (near, far) = (diff(0, h), diff(6, 2.0 * h));
            let slope = (near[0] * near[0] + near[1] * near[1] + near[2] * near[2]).sqrt();
            if slope < 0.1 || angle(near, far) > 0.1 {
                continue;
            }
            checked += 1;
            let got = [g[i * 3] as f64, g[i * 3 + 1] as f64, g[i * 3 + 2] as f64];
            assert!(angle(got, near) < 0.5, "{name} point {i} at {:?}: the gradient {got:?} against the differences {near:?}", &xyz[i * 3..i * 3 + 3]);
        }
        assert!(checked * 2 > n, "{name}: only {checked} of {n} points away from a crease");
    }

    /// At a crease the gradient is the side's whose value it is, exactly: a box's edge from
    /// inside, the seam of two balls, the rim a cut leaves. Differences there blur both sides.
    #[test]
    fn a_crease_takes_its_side_exactly() {
        let ball = |x: f64| placed([x, 0.0, 0.0], sphere(5.0));
        let unit = |v: [f64; 3]| { let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt(); v.map(|c| c / l) };
        for (name, root, p, want) in [
            ("a box's edge", cube(8.0), [3.999, 3.998, 0.5], [1.0, 0.0, 0.0]),
            ("two balls' seam", SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![ball(3.0), ball(-3.0)] }, [-1.0e-3, 4.0, 0.0], unit([3.0 - 1.0e-3, 4.0, 0.0])),
            ("a cut's rim", SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![cube(8.0), placed([4.0, 0.0, 0.0], sphere(2.0))] }, [4.0 - 1.0e-3, 0.0, -2.002], [1.0, 0.0, 0.0]),
        ] {
            let f = compile(&root, false).unwrap();
            let xyz = p.map(|c| c as f32);
            let (mut v, mut g) = ([0.0f32], [0.0f32; 3]);
            f.gradient.eval(&xyz, &mut v, &mut g, 1.0e-3);
            let got = unit(g.map(|c| c as f64));
            assert!(angle(got, want) < 1.0e-3, "{name}: {got:?} against {want:?}");
            // what the tiles took before: forward differences a twentieth of a cell long
            let h = 0.05f32 * 0.5;
            let around: Vec<f32> = (0..4).flat_map(|k| { let mut q = xyz; if k > 0 { q[k - 1] += h; } q }).collect();
            let (d, ..) = eval(&f, &around);
            let blurred = [(d[1] - d[0]) as f64, (d[2] - d[0]) as f64, (d[3] - d[0]) as f64];
            assert!(angle(blurred, want) > 5.0, "{name}: differences {blurred:?} here, no crease to take a side of");
        }
    }

    /// Outside a blend's reach the code gives its nearer operand bit for bit, as csg_sdf's smooth
    /// minimum does on the CPU, for a union, a cut and an intersection, in the value's code and the
    /// gradient's: where an operand's distance is exact in f32 the code and the CPU give the same
    /// bits, and moving the far operand moves no bit of the value.
    #[test]
    fn a_blend_outside_its_reach_is_its_nearer_operand_bit_for_bit_as_on_the_cpu() {
        let k = 2.0;
        let ball = |x: f64, r: f64| placed([x, 0.0, 0.0], sphere(r));
        let fold = |op, far: SdfNode| SdfNode::Combine { op, blend: k, of: vec![sphere(1.0), far] };
        // the far operand of each: a ball beyond the blend, a ball cut away there, a ball holding it
        let cases = |at: f64| [("union", fold(SdfOp::Union, ball(at, 1.0))), ("cut", fold(SdfOp::Difference, ball(at, 1.0))), ("intersection", fold(SdfOp::Intersection, sphere(at)))];
        let run = |root: &SdfNode, xyz: &[f32]| {
            let f = compile(root, false).unwrap();
            let n = xyz.len() / 3;
            let (v, ..) = eval(&f, xyz);
            let (mut gv, mut g) = (vec![0.0f32; n], vec![0.0f32; n * 3]);
            f.gradient.eval(xyz, &mut gv, &mut g, 1.0e-3);
            (v, gv, truth(root, xyz))
        };
        // On the x axis just outside the unit ball the ball's distance is exact in f32 (a few
        // bits past 1), and `b + (a - b)` would round it against an operand 3000 away.
        let axis: Vec<f32> = (1..64).flat_map(|j| [(1.0 + j as f64 * 2f64.powi(-14)) as f32, 0.0, 0.0]).collect();
        for (name, root) in cases(3000.0) {
            let (v, g, c) = run(&root, &axis);
            for (i, x) in axis.chunks(3).enumerate() {
                let want = x[0] as f64 - 1.0;
                assert_eq!((v[i] as f64).to_bits(), want.to_bits(), "{name} at {}: {} against {want}", x[0], v[i]);
                assert_eq!((g[i] as f64).to_bits(), want.to_bits(), "{name} at {}: the gradient's value {}", x[0], g[i]);
                assert_eq!(c[i].to_bits(), want.to_bits(), "{name} at {} on the CPU: {}", x[0], c[i]);
            }
        }
        // Anywhere outside the blend the far operand moves no bit, here or on the CPU: the same
        // code (the structure is the same) with the far operand at two places.
        let xyz = points(9.0);
        let n = xyz.len() / 3;
        let (near, ..) = run(&sphere(1.0), &xyz);
        for ((name, here), (_, there)) in cases(20.0).into_iter().zip(cases(40.0)) {
            let (v1, g1, c1) = run(&here, &xyz);
            let (v2, g2, c2) = run(&there, &xyz);
            let mut far = 0;
            for i in 0..n {
                // the near ball's distance is within 10 of 0 here, the far operand 10 or more away
                if near[i].abs() > 6.0 {
                    continue;
                }
                far += 1;
                assert_eq!((v1[i].to_bits(), g1[i].to_bits(), c1[i].to_bits()), (v2[i].to_bits(), g2[i].to_bits(), c2[i].to_bits()), "{name} point {i}: {} {} {} against {} {} {}", v1[i], g1[i], c1[i], v2[i], g2[i], c2[i]);
            }
            assert!(far > n / 4, "{name}: {far} of {n} points");
        }
    }

    #[test]
    fn nodes_agree_with_their_query() {
        let two = |op, blend| SdfNode::Combine { op, blend, of: vec![cube(9.0), placed([2.0, 1.0, 0.0], sphere(5.0))] };
        for (name, root) in [
            ("sphere", sphere(5.0)),
            ("box", SdfNode::Box { size: [4.0, 7.0, 9.0], radius: 1.0 }),
            ("cylinder", SdfNode::Cylinder { radius: 3.0, top_radius: 3.0, height: 8.0 }),
            ("cone", SdfNode::Cylinder { radius: 4.0, top_radius: 1.0, height: 8.0 }),
            ("torus", SdfNode::Torus { radius: 5.0, tube: 1.0 }),
            ("capsule", SdfNode::Capsule { a: [1.0, 2.0, 3.0], b: [-4.0, 0.0, 1.0], radius: 1.5 }),
            ("capsule with no length", SdfNode::Capsule { a: [1.0, 2.0, 3.0], b: [1.0, 2.0, 3.0], radius: 1.5 }),
            ("ellipsoid", SdfNode::Ellipsoid { radii: [3.0, 5.0, 7.0] }),
            ("gyroid", SdfNode::Gyroid { period: 8.0, wall: 1.2 }),
            ("union", two(SdfOp::Union, 0.0)),
            ("blend", two(SdfOp::Union, 1.5)),
            ("cut", two(SdfOp::Difference, 0.0)),
            ("soft cut", two(SdfOp::Difference, 0.7)),
            ("common", two(SdfOp::Intersection, 0.0)),
            ("soft common", two(SdfOp::Intersection, 0.7)),
            ("shell", SdfNode::Shell { thickness: 0.5, of: Box::new(cube(9.0)) }),
            ("round", SdfNode::Round { radius: 1.0, of: Box::new(cube(7.0)) }),
            ("transform", SdfNode::Transform { at: [1.0, -2.0, 3.0], rotate: [20.0, 40.0, -10.0], scale: 1.4, of: Box::new(cube(6.0)) }),
            ("instance", SdfNode::Instance { places: vec![SdfPlace { at: [4.0, 0.0, 0.0], ..SdfPlace::IDENTITY }, SdfPlace { at: [-4.0, 1.0, 0.0], scale: 0.7, ..SdfPlace::IDENTITY }], of: Box::new(sphere(3.0)) }),
            ("twist", SdfNode::Deform { deform: SdfDeform::Twist { rate: 0.1 }, lipschitz: 2.0, of: Box::new(cube(8.0)) }),
            ("bend", SdfNode::Deform { deform: SdfDeform::Bend { rate: 0.05 }, lipschitz: 2.0, of: Box::new(cube(8.0)) }),
            ("taper", SdfNode::Deform { deform: SdfDeform::Taper { rate: 0.02 }, lipschitz: 2.0, of: Box::new(cube(8.0)) }),
            ("mirror", SdfNode::Deform { deform: SdfDeform::Mirror { axis: 0 }, lipschitz: 1.0, of: Box::new(placed([3.0, 0.0, 0.0], sphere(2.0))) }),
            ("material", SdfNode::Material { slot: 2, within: None, of: Box::new(cube(8.0)) }),
            ("id", SdfNode::Id { id: 4, of: Box::new(cube(8.0)) }),
            ("empty sculpt", SdfNode::Sculpt { strokes: Arc::new(Strokes::new()), of: Box::new(cube(8.0)) }),
        ] {
            agrees(name, &root, 0);
        }
    }

    /// A tree no GPU program could hold still lowers.
    #[test]
    fn a_deep_tree_lowers() {
        let mut root = sphere(5.0);
        for i in 0..40 {
            root = SdfNode::Combine { op: SdfOp::Union, blend: 0.2, of: vec![placed([0.1 * i as f64, 0.0, 0.0], sphere(2.0)), root] };
        }
        agrees("deep", &root, 0);
    }

    #[test]
    fn leaves_are_answered_by_their_query() {
        for (name, root, sites) in [
            ("mesh", mesh(), 1),
            ("links", links(), 1),
            ("sculpt", sculpted(cube(8.0)), 1),
            ("kernel", opaque(), 1),
            ("placed", placed([1.0, 2.0, 3.0], links()), 1),
            ("blended", SdfNode::Combine { op: SdfOp::Union, blend: 1.0, of: vec![mesh(), placed([5.0, 0.0, 0.0], links()), sphere(4.0)] }, 2),
            ("placed twice", SdfNode::Instance { places: vec![SdfPlace { at: [4.0, 0.0, 0.0], ..SdfPlace::IDENTITY }, SdfPlace { at: [-4.0, 0.0, 0.0], scale: 0.5, ..SdfPlace::IDENTITY }], of: Box::new(mesh()) }, 2),
            ("warped", SdfNode::Deform { deform: SdfDeform::Twist { rate: 0.1 }, lipschitz: 2.0, of: Box::new(mesh()) }, 1),
            ("a texture's pattern", textured(sphere(6.0), None, 0.5), 1),
            ("a texture's source", textured(mesh(), None, 0.5), 2),
            ("under the gradient", textured(links(), Some((0.2, 0.6)), 0.5), 6),
        ] {
            agrees(name, &root, sites);
        }
    }

    /// A root-path texture's bound and depth, through a placement's scale.
    #[test]
    fn a_textured_root_bounds_from_its_source() {
        let depth = 0.5;
        let (of, scale) = (sphere(6.0), 1.5);
        let root = SdfNode::Transform { at: [0.0; 3], rotate: [0.0; 3], scale, of: Box::new(textured(of.clone(), None, depth)) };
        let f = compile(&root, false).unwrap();
        let xyz = points(14.0);
        let (v, b, s) = eval(&f, &xyz);
        let source = truth(&SdfNode::Transform { at: [0.0; 3], rotate: [0.0; 3], scale, of: Box::new(of) }, &xyz);
        for (i, g) in source.iter().enumerate() {
            let tol = 1.0e-3;
            assert!((b[i] as f64 - (v[i] as f64).max(*g)).abs() < tol, "bound at {i}: {} against value {} source {g}", b[i], v[i]);
            assert!((s[i] as f64 - (-v[i] as f64).max(-(g + depth * scale))).abs() < tol, "depth at {i}: {} against value {} source {g}", s[i], v[i]);
        }
    }

    #[test]
    fn slots_agree_with_their_query() {
        let mat = |slot, of| SdfNode::Material { slot, within: None, of: Box::new(of) };
        let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![
            mat(1, cube(8.0)),
            mat(3, placed([6.0, 0.0, 0.0], sphere(4.0))),
            placed([0.0, 5.0, 0.0], links()),
            SdfNode::Material { slot: 4, within: Some(Box::new(sphere(3.0))), of: Box::new(placed([-6.0, 0.0, 0.0], mesh())) },
        ] };
        let f = compile(&root, true).unwrap();
        let xyz = points(14.0);
        let n = xyz.len() / 3;
        let (mut v, mut slot) = (vec![0.0; n], vec![0.0; n]);
        f.eval_slots(&xyz, &mut v, &mut slot);
        for (i, p) in xyz.chunks(3).enumerate() {
            let (_, want) = distance_slot(&root, Vec3d::new(p[0] as f64, p[1] as f64, p[2] as f64));
            assert_eq!(slot[i] as u8, want as u8, "point {i}, {p:?}, value {}", v[i]);
        }
    }

    /// New numbers under the same code: no compile, and the values are the new ones.
    #[test]
    fn numbers_keep_the_code() {
        let shape = |r: f64, at: f64, side: f64, blend: f64| SdfNode::Combine { op: SdfOp::Union, blend, of: vec![
            placed([at, 1.0, 0.0], sphere(r)),
            SdfNode::Transform { at: [0.0, at, 2.0], rotate: [10.0, r * 3.0, 0.0], scale: 1.2, of: Box::new(SdfNode::Box { size: [side; 3], radius: 0.25 }) },
            placed([1.0, 1.0, at], mesh()),
        ] };
        let first = compile(&shape(5.0, 2.0, 8.0, 0.5), false).unwrap();
        for k in 1..20 {
            let k = k as f64 * 0.1;
            let root = shape(5.0 + k, 2.0 + k, 8.0 - k, 0.5 + k);
            let f = compile(&root, false).unwrap();
            assert_eq!(f.structure, first.structure, "numbers alone changed the structure");
            assert_eq!(f.compile_ms, 0.0, "numbers alone compiled");
            let xyz = points(14.0);
            let (v, ..) = eval(&f, &xyz);
            for (got, want) in v.iter().zip(truth(&root, &xyz)) {
                assert!((*got as f64 - want).abs() < 2.0e-3, "{got} against {want}");
            }
        }
    }

    /// A stroke is new data under the same code, once a part has any.
    #[test]
    fn strokes_keep_the_code() {
        let with = |n: usize| {
            let mut strokes = Strokes::new();
            for i in 0..n {
                strokes.push(StrokeOp::Raise, 3.0, 0.8, &[[5.0, i as f64, 0.0], [5.0, i as f64 + 2.0, 0.0]]);
            }
            SdfNode::Sculpt { strokes: Arc::new(strokes), of: Box::new(cube(8.0)) }
        };
        let first = compile(&with(1), false).unwrap();
        for n in 2..6 {
            let root = with(n);
            let f = compile(&root, false).unwrap();
            assert_eq!((f.structure, f.compile_ms), (first.structure, 0.0), "{n} strokes");
            agrees("strokes", &root, 1);
        }
    }

    /// A mesh ball of `rings` x `segments` and `radius`, counter-clockwise seen from outside.
    fn globe(rings: u32, segments: u32, radius: f32) -> SdfNode {
        let mut positions = vec![[0.0, radius, 0.0]];
        for i in 1..rings {
            let phi = std::f32::consts::PI * i as f32 / rings as f32;
            for j in 0..segments {
                let theta = std::f32::consts::TAU * j as f32 / segments as f32;
                positions.push([radius * phi.sin() * theta.cos(), radius * phi.cos(), radius * phi.sin() * theta.sin()]);
            }
        }
        positions.push([0.0, -radius, 0.0]);
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
    }

    fn fold(op: SdfOp, blend: f64, of: Vec<SdfNode>) -> SdfNode {
        SdfNode::Combine { op, blend, of }
    }

    /// The shares of the mesh leaves' answers over `xyz` that end at their caps, and that are
    /// searched again under proven ones.
    fn capped_share(f: &CompiledField, xyz: &[f32]) -> (f64, f64) {
        let n = xyz.len() / 3;
        let (mut v, mut b, mut s, mut slot) = (vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; if f.slots { n } else { 0 }]);
        let (capped, again) = f.answered(xyz, &mut v, &mut b, &mut s, &mut slot, false);
        let meshes = f.leaves.iter().filter(|l| matches!(l, SdfNode::Mesh(_))).count();
        let all = (n * meshes).max(1) as f64;
        (capped as f64 / all, again as f64 / all)
    }

    /// Mesh leaves answered under their caps give the field the bits of every leaf answered in
    /// full, from code with no caps at all: two meshes blended into each other and then into
    /// solids (each mesh's cap waits on the other's answer), a mesh beside a solid either side of
    /// it, sharp, cutting, cut, intersected, rounded, shelled, scaled, in a cut in a union, under a
    /// texture, as a material's region, beside another kind of leaf; values, bounds and solid
    /// depths, with and without slots, whatever batches the points come in, and the gradient's
    /// value and gradient.
    #[test]
    fn capped_leaves_give_the_bits_of_leaves_answered_in_full() {
        use SdfOp::{Difference, Intersection, Union};
        let m = || placed([1.0, -2.0, 0.5], mesh());
        let g = || placed([-4.0, 3.0, 1.0], globe(12, 24, 4.0));
        let shapes: Vec<(&str, SdfNode, f64)> = vec![
            ("pair", SdfNode::Transform { at: [0.5, 0.0, 0.0], rotate: [0.0, 20.0, 0.0], scale: 1.3, of: Box::new(fold(Union, 0.2, vec![placed([0.0, 9.0, 0.0], sphere(2.0)), fold(Union, 1.5, vec![fold(Union, 1.5, vec![fold(Union, 1.0, vec![m(), g()]), placed([7.0, 0.0, 0.0], sphere(3.0))]), cube(4.0)])])) }, 0.05),
            ("beside", fold(Union, 2.0, vec![placed([-8.0, 0.0, 0.0], sphere(3.0)), m()]), 0.05),
            ("beside, mesh first", fold(Union, 2.0, vec![m(), placed([-8.0, 0.0, 0.0], sphere(3.0))]), 0.05),
            ("sharp", fold(Union, 0.0, vec![m(), placed([0.0, 8.0, 0.0], sphere(2.0)), g()]), 0.05),
            ("cutting", fold(Difference, 1.0, vec![cube(16.0), m()]), 0.0),
            ("cutting sharp", fold(Difference, 0.0, vec![placed([9.0, 0.0, 0.0], cube(6.0)), m()]), 0.05),
            ("cut", fold(Difference, 1.0, vec![m(), placed([3.0, 3.0, 0.0], sphere(4.0))]), 0.0),
            ("cut in a union", fold(Union, 1.0, vec![placed([0.0, 9.0, 0.0], sphere(3.0)), fold(Difference, 1.0, vec![m(), placed([3.0, 3.0, 0.0], sphere(4.0))])]), 0.05),
            ("meet in a union", fold(Union, 1.0, vec![placed([0.0, 9.0, 0.0], sphere(3.0)), fold(Intersection, 1.0, vec![m(), sphere(7.0)])]), 0.05),
            ("meet, mesh second", fold(Intersection, 0.5, vec![placed([2.0, 0.0, 0.0], sphere(6.0)), g()]), 0.0),
            ("leaves alone", fold(Union, 0.5, vec![SdfNode::Round { radius: 1.0, of: Box::new(m()) }, SdfNode::Shell { thickness: 0.5, of: Box::new(g()) }]), 0.0),
            ("rounded, shelled, scaled", fold(Union, 0.5, vec![placed([0.0, 0.0, -9.0], sphere(3.0)), SdfNode::Round { radius: 1.0, of: Box::new(m()) }, SdfNode::Shell { thickness: 0.5, of: Box::new(g()) }, SdfNode::Transform { at: [8.0, 0.0, 0.0], rotate: [0.0; 3], scale: 0.5, of: Box::new(mesh()) }]), 0.05),
            ("a cut in a union", fold(Union, 1.0, vec![fold(Difference, 0.5, vec![fold(Union, 0.0, vec![m(), sphere(3.0)]), g()]), placed([0.0, -9.0, 0.0], cube(5.0))]), 0.05),
            ("textured", fold(Union, 1.0, vec![textured(m(), None, 0.4), placed([0.0, 9.0, 0.0], sphere(3.0))]), 0.0),
            ("region", fold(Union, 1.0, vec![SdfNode::Material { slot: 2, within: Some(Box::new(m())), of: Box::new(sphere(6.0)) }, g()]), 0.0),
            ("another leaf", fold(Union, 1.0, vec![links(), m(), placed([0.0, 0.0, 9.0], sphere(2.0))]), 0.05),
        ];
        let mut xyz = points(14.0);
        xyz.extend(points(40.0).iter().map(|x| x + 0.37));
        let n = xyz.len() / 3;
        let mut seen_again = false;
        for (name, root, share) in &shapes {
            for slots in [false, true] {
                let capped = compile(root, slots).unwrap_or_else(|e| panic!("{name}: {e}"));
                let full = compile_with(root, slots, false).unwrap();
                assert!(capped.caps && !full.caps, "{name}");
                let want = eval(&full, &xyz);
                let got = eval(&capped, &xyz);
                // in batches of 37 from the last
                let (mut v, mut b, mut s) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
                let mut at = n;
                while at > 0 {
                    let from = at.saturating_sub(37);
                    capped.eval(&xyz[from * 3..at * 3], &mut v[from..at], &mut b[from..at], &mut s[from..at]);
                    at = from;
                }
                for (what, got) in [("whole", &got), ("batched", &(v, b, s))] {
                    for i in 0..n {
                        let bits = |t: &(Vec<f32>, Vec<f32>, Vec<f32>)| (t.0[i].to_bits(), t.1[i].to_bits(), t.2[i].to_bits());
                        assert_eq!(bits(got), bits(&want), "{name} (slots {slots}, {what}) point {i} at {:?}: {:?} against {:?}", &xyz[i * 3..i * 3 + 3], (got.0[i], got.1[i], got.2[i]), (want.0[i], want.1[i], want.2[i]));
                    }
                }
                if !slots {
                    let gradient = |f: &CompiledField| {
                        let (mut v, mut g) = (vec![0.0f32; n], vec![0.0f32; n * 3]);
                        f.gradient.eval(&xyz, &mut v, &mut g, 1.0e-3);
                        (v, g)
                    };
                    let (want, got) = (gradient(&full), gradient(&capped));
                    for i in 0..n {
                        let bits = |t: &(Vec<f32>, Vec<f32>)| (t.0[i].to_bits(), [0, 1, 2].map(|k| t.1[i * 3 + k].to_bits()));
                        assert_eq!(bits(&got), bits(&want), "{name}'s gradient at point {i}, {:?}: {:?} against {:?}", &xyz[i * 3..i * 3 + 3], (got.0[i], &got.1[i * 3..i * 3 + 3]), (want.0[i], &want.1[i * 3..i * 3 + 3]));
                    }
                }
                let (cut, again) = capped_share(&capped, &xyz);
                println!("{name} (slots {slots}): {:.1}% of the mesh answers capped, {:.1}% searched again", cut * 100.0, again * 100.0);
                assert!(cut >= *share, "{name}: {:.0}% of the mesh answers capped", cut * 100.0);
                seen_again |= again > 0.0;
            }
        }
        assert!(seen_again, "no mesh was searched again under a proven cap");
    }

    /// Where its mesh is far, a field's sample costs a small part of what it costs answered in
    /// full, its value and its gradient: the mesh searched under its cap opens no box.
    #[test]
    fn a_far_mesh_costs_a_sample_little() {
        let root = fold(SdfOp::Union, 1.0, vec![sphere(3.0), placed([30.0, 0.0, 0.0], globe(97, 192, 5.0))]);
        let (capped, full) = (compile(&root, false).unwrap(), compile_with(&root, false, false).unwrap());
        let xyz: Vec<f32> = points(6.0);
        let n = xyz.len() / 3;
        let time = |f: &CompiledField| {
            let (mut v, mut b, mut s, mut g) = (vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n * 3]);
            let t = std::time::Instant::now();
            for _ in 0..4 {
                f.eval(&xyz, &mut v, &mut b, &mut s);
            }
            let value = t.elapsed().as_secs_f64() * 1e9 / (4 * n) as f64;
            let t = std::time::Instant::now();
            for _ in 0..4 {
                f.gradient.eval(&xyz, &mut b, &mut g, 1.0e-3);
            }
            ([value, t.elapsed().as_secs_f64() * 1e9 / (4 * n) as f64], v, g)
        };
        let mut best = [[f64::INFINITY; 2]; 2];
        for _ in 0..5 {
            let ((a, va, ga), (b, vb, gb)) = (time(&capped), time(&full));
            assert!(va.iter().zip(&vb).chain(ga.iter().zip(&gb)).all(|(x, y)| x.to_bits() == y.to_bits()));
            for k in 0..2 {
                best[0][k] = best[0][k].min(a[k]);
                best[1][k] = best[1][k].min(b[k]);
            }
        }
        for (k, what) in ["value", "value and gradient"].iter().enumerate() {
            println!("a far mesh, {what}: {:.0} ns a sample capped, {:.0} ns in full ({:.0}x)", best[0][k], best[1][k], best[1][k] / best[0][k]);
            assert!(best[0][k] * 5.0 < best[1][k], "{what}: capped {:.0} ns against {:.0} ns in full", best[0][k], best[1][k]);
        }
    }

    fn stmts(b: &Block) -> usize {
        ir::unmarked(b).map(|s| 1 + match s {
            Stmt::If(_, a, c) => stmts(a) + stmts(c),
            Stmt::Loop { body, .. } => stmts(body),
            _ => 0,
        }).sum()
    }

    /// The code writes the meshes' caps alone: a chain of other leaves blended into two meshes'
    /// union adds no code for caps of theirs, though their paths to the root are the longest. The
    /// code's statements after the optimizer with no caps, with the meshes', and with every
    /// leaf's; and what compiling costs, capped and not.
    #[test]
    fn caps_cost_the_code_the_meshes_paths_alone() {
        use SdfOp::Union;
        let chain = fold(Union, 2.8, (0..5).map(|k| placed([k as f64 * 6.0 - 12.0, -10.0, 0.0], links())).chain([placed([0.0, -16.0, 0.0], sphere(3.0))]).collect());
        let pair = fold(Union, 1.0, vec![placed([1.0, -2.0, 0.5], mesh()), placed([-4.0, 3.0, 1.0], globe(12, 24, 4.0))]);
        let meshes = fold(Union, 1.5, vec![fold(Union, 1.5, vec![pair, sphere(3.0)]), placed([0.0, 6.0, 0.0], sphere(2.0))]);
        let root = fold(Union, 0.2, vec![fold(Union, 2.0, vec![fold(Union, 1.0, vec![chain, meshes]), cube(4.0)]), placed([0.0, 9.0, 0.0], sphere(2.0))]);
        let (table, texels, leaves) = lowered(&root).unwrap();
        assert_eq!(leaves.len(), 7);
        let size = |grad: bool, capped: &[bool]| {
            let Ok((mut p, _)) = build(&texels, &table, false, capped, grad) else { panic!("the code") };
            makepad_script_compute::opt::optimize(&mut p);
            stmts(&p.body) + p.funcs.iter().map(|f| stmts(&f.body)).sum::<usize>()
        };
        for (grad, what) in [(false, "value"), (true, "gradient")] {
            let (none, meshes, all) = (size(grad, &capped(&leaves, false)), size(grad, &capped(&leaves, true)), size(grad, &[true; 7]));
            println!("caps' code, {what}: {none} statements with none, {meshes} with the meshes', {all} with every leaf's");
            assert!(none < meshes && meshes < all, "{what}: {none}, {meshes}, {all}");
        }
        let (capped, full) = (compile(&root, false).unwrap(), compile_with(&root, false, false).unwrap());
        println!("caps' code: compiled in {:.1} ms capped, {:.1} ms with no caps", capped.compile_ms, full.compile_ms);
    }
}

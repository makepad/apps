//! Kernels in documents (KERNELS.md §3.5): the per-vertex, per-instance and
//! per-element hot paths of a scene written as Splash and compiled ahead of
//! time by `makepad-script-compute`, instead of run by the VM per element.
//! The documents' `Kernel{...}` and `Sim{...}` are Splash objects the VM
//! evaluates; [`crate::objects`] compiles and loads them. This module holds
//! what a loaded document's kernels are and runs them: a kernel that reads
//! `time` or a param runs every frame (for the nodes that draw it); any
//! other runs once, at load, and its outputs are ordinary Splash arrays.

use makepad_script_compute_sched::kernel::KernelExt;
use crate::script::Loc;
use makepad_render_kernels::RecordLayout;
use makepad_script_compute::boundary::{register_layout, LayoutFieldView, LayoutView};
use makepad_script_compute::kernel::{FieldTy, Kernel, KernelRef};
use makepad_widgets::makepad_script::*;
use std::collections::HashMap;
use std::sync::Arc;

/// The record layouts kernels may write by field, by name: `Sprite`, and
/// effect physics' `Body` (a spawn), `Rigid`, `Contact` and `Event`
/// (KERNELS.md §3.6).
pub fn layouts() -> Vec<LayoutView<'static>> {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        for layout in [&SPRITE, &SEGMENT] {
            // false: another table holds the name; compiles with this one report the mismatch.
            register_layout(layout);
        }
    });
    let mut v = vec![SPRITE, SEGMENT];
    v.extend_from_slice(makepad_effect_sim::layouts());
    v
}

/// A point sprite: where, how big, its colour.
pub const SPRITE: LayoutView<'static> = LayoutView {
    name: "Sprite",
    stride: 8,
    fields: &[
        LayoutFieldView { name: "pos", ty: FieldTy::Vec3, offset: 0 },
        LayoutFieldView { name: "size", ty: FieldTy::F32, offset: 3 },
        LayoutFieldView { name: "color", ty: FieldTy::Vec4, offset: 4 },
    ],
};

/// A line batch's segment (`Line3D{source: k.out}`): ends, linear colour
/// (over 1 blooms), width (× the Line3D's), the reference film's LineBatch.
pub const SEGMENT: LayoutView<'static> = LayoutView {
    name: "Segment",
    stride: 11,
    fields: &[
        LayoutFieldView { name: "a", ty: FieldTy::Vec3, offset: 0 },
        LayoutFieldView { name: "b", ty: FieldTy::Vec3, offset: 3 },
        LayoutFieldView { name: "color", ty: FieldTy::Vec4, offset: 6 },
        LayoutFieldView { name: "width", ty: FieldTy::F32, offset: 10 },
    ],
};

/// Threads for one run: the machine's, capped (runs are short).
pub fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8)
}

// ---------------------------------------------------------------------------
// Document kernels
// ---------------------------------------------------------------------------

/// A buffer a kernel declares.
#[derive(Clone, Debug)]
pub struct KernelBuf {
    pub name: String,
    /// The element type as written (`f32`, `vec3`, `Sprite`, ...).
    pub ty: String,
    /// Words per record.
    pub width: usize,
    /// Slots per element for an emit buffer.
    pub emit: Option<usize>,
}

/// One `Kernel{...}`, compiled.
#[derive(Clone)]
pub struct DocKernel {
    pub name: String,
    pub kernel: KernelRef,
    pub outputs: Vec<KernelBuf>,
    /// Inputs bound to earlier kernels' outputs: (input, kernel id, buffer).
    pub inputs: Vec<(String, usize, String)>,
    /// Inputs the document computes (`pts: input(vec3, PTS)`): name and
    /// type width; their values, flattened, once the VM has evaluated them.
    pub data: Vec<(String, usize)>,
    pub data_values: Vec<(String, Arc<Vec<f32>>)>,
    pub params: Vec<String>,
    /// Reads `time` (a keyed param also makes it per frame, at load).
    pub per_frame: bool,
    /// Inputs physics gives each frame (`input(contacts("g"))`): the
    /// kernel runs per frame, over the first one's records unless it sets
    /// `count`.
    pub physics: Vec<(String, makepad_effect_sim::SimInput)>,
    /// Writes `Body` records: a spawn's kernel (run by physics, never at
    /// load).
    pub body_out: bool,
    /// Load-time results by output name (records, compacted for emits).
    pub results: HashMap<String, Arc<Vec<f32>>>,
    /// What it runs with: its count, params' and document arrays' values
    /// (crate::objects::load).
    pub spec: Option<ScriptObject>,
    /// The document's `Kernel{..}` object.
    pub object: Option<ScriptObject>,
    pub loc: Loc,
}

/// One `Sim{...}`, compiled (KERNELS.md §3.5.6): its state steps
/// with the document's physics.
#[derive(Clone)]
pub struct DocSim {
    pub name: String,
    pub state: RecordLayout,
    pub count: u32,
    pub every: u32,
    pub init: KernelRef,
    pub update: KernelRef,
    pub physics: Vec<(String, makepad_effect_sim::SimInput)>,
    pub params: Vec<(String, f32)>,
    /// `drops: emit_buffer(Body, n)`: bodies it spawns into group `drops`.
    pub spawn: Option<String>,
    /// A hash of the kernels' programs (the checkpoints' version stamp).
    pub program: u64,
    pub loc: Loc,
}

/// A document's kernels and sims, loaded from its `Kernel{}` and `Sim{}`
/// objects (see [`crate::objects::load`]).
#[derive(Default)]
pub struct DocKernels {
    pub list: Vec<DocKernel>,
    pub sims: Vec<DocSim>,
    /// What the nodes that draw kernels read for a document value (see
    /// [`crate::objects::resolve`]): an output's array or per-frame handle,
    /// a kernel's outputs, a sim's reference.
    pub objects: HashMap<ScriptObject, ScriptValue>,
    /// Each output declaration's kernel (index) and buffer name.
    pub outputs: HashMap<ScriptObject, (usize, String)>,
    /// Where the compiled-in fns are written (VM file, 1-based line and
    /// column of the first token, line and column after the last) and the
    /// document names compiled in as constants: an editor's edit of a
    /// literal there shows only once the document loads again.
    pub fn_sites: Vec<(String, u32, u32, u32, u32)>,
    pub constants: Vec<String>,
    /// Where the `Kernel{}` and `Sim{}` objects were made: the load copied
    /// their fields (a `count`) into the kernels, so an editor's edit of a
    /// value set there shows only once the document loads again.
    pub setup_sites: Vec<ScriptIp>,
    /// What keeps the loaded values alive.
    roots: Vec<ScriptObjectRef>,
    array_roots: Vec<ScriptArrayRef>,
}

impl DocKernels {
    /// `object` (a document value) means `value` to the nodes, which is
    /// kept alive as long as these kernels.
    pub fn keep(&mut self, vm: &mut ScriptVm, object: ScriptValue, value: ScriptValue) {
        let Some(o) = object.as_object() else { return };
        self.root(vm, value);
        self.objects.insert(o, value);
    }

    /// Whether a literal the VM reads at `file`, 0-based `line` and `col`
    /// (an opcode's place) is in a fn compiled into one of these kernels.
    pub fn compiles_at(&self, file: &str, line: u32, col: u32) -> bool {
        let (line, col) = (line + 1, col + 1);
        self.fn_sites.iter().any(|(f, l0, c0, l1, c1)| f == file && (line, col) >= (*l0, *c0) && (line, col) < (*l1, *c1))
    }

    /// Keeps `value` alive as long as these kernels.
    pub(crate) fn root(&mut self, vm: &mut ScriptVm, value: ScriptValue) {
        if let Some(v) = value.as_object() {
            self.roots.push(vm.bx.heap_mut().new_object_ref(v));
        } else if let Some(a) = value.as_array() {
            self.array_roots.push(vm.bx.heap_mut().new_array_ref(a));
        }
    }

    /// The same kernels for another document of the VM that loaded them
    /// (one sharing its prefix): the values stay alive with these.
    pub fn share(&self) -> DocKernels {
        DocKernels { list: self.list.clone(), sims: self.sims.clone(), objects: self.objects.clone(), outputs: self.outputs.clone(), fn_sites: self.fn_sites.clone(), constants: self.constants.clone(), setup_sites: self.setup_sites.clone(), roots: Vec::new(), array_roots: Vec::new() }
    }

    /// Runs kernel `id` over `count` elements with `params`; its outputs
    /// by name (emit buffers compacted in element order).
    pub fn run(&self, id: usize, count: usize, params: &[(String, f32)], time: f32) -> Result<HashMap<String, Arc<Vec<f32>>>, String> {
        let dk = &self.list[id];
        let inputs: Vec<(String, Arc<Vec<f32>>)> = dk
            .inputs
            .iter()
            .map(|(name, ki, buf)| {
                let data = self.list[*ki].results.get(buf).cloned().ok_or_else(|| format!("kernel `{}`: its input `{name}` ({}.{buf}) has not run", dk.name, self.list[*ki].name))?;
                Ok((name.clone(), data))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut inputs = inputs;
        inputs.extend(dk.data_values.iter().cloned());
        run_kernel(dk, count, params, time, &inputs)
    }

}

/// A per-frame kernel a node draws: what `Points{source: k.out}` holds.
#[derive(Clone)]
pub struct KernelRun {
    pub name: String,
    pub kernel: KernelRef,
    pub outputs: Vec<KernelBuf>,
    /// The output the node draws (index into `outputs`).
    pub buffer: usize,
    /// Elements; `None`: one per record of the first physics input.
    pub count: Option<usize>,
    pub params: Vec<(String, crate::anim::Anim<1>)>,
    pub inputs: Vec<(String, Arc<Vec<f32>>)>,
    /// Inputs physics gives at the frame's time.
    pub physics: Vec<(String, makepad_effect_sim::SimInput)>,
    /// `active_count: fn(t)`: how many of the elements run this frame (the first
    /// ones; `count` is the most); all when absent.
    pub active: Option<crate::anim::Anim<1>>,
    /// Inputs the document computes every frame (`cams: input(vec4, fn(t)
    /// {..})`): name and closure.
    pub frame_data: Vec<(String, u32)>,
}

impl std::fmt::Debug for KernelRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KernelRun({}.{} x{:?})", self.name, self.outputs[self.buffer].name, self.count)
    }
}

impl KernelRun {
    /// The drawn output's records at `time` with these param values, and
    /// the physics inputs as (name, records as words, record count).
    pub fn run(&self, time: f32, params: &[(String, f32)], physics: &[(String, Arc<Vec<f32>>, usize)], frame_data: &[(String, Arc<Vec<f32>>)], active: Option<usize>) -> Result<Arc<Vec<f32>>, String> {
        let count = match self.count {
            Some(n) => n,
            None => physics.first().map(|p| p.2).unwrap_or(0),
        };
        let count = active.map_or(count, |a| a.min(count));
        if count == 0 {
            return Ok(Arc::new(Vec::new()));
        }
        let mut inputs = self.inputs.clone();
        let mut params = params.to_vec();
        for (name, data, n) in physics {
            inputs.push((name.clone(), data.clone()));
            params.push((format!("{name}_count"), *n as f32));
        }
        inputs.extend(frame_data.iter().cloned());
        // Several nodes drawing outputs of one kernel at one time with the
        // same values share a run (the cache is emptied every evaluation).
        let key = RunKey {
            kernel: self.kernel.as_ptr() as usize,
            count,
            time: time.to_bits(),
            params: params.iter().map(|(n, v)| (n.clone(), v.to_bits())).collect(),
            inputs: inputs.iter().map(|(n, d)| (n.clone(), Arc::as_ptr(d) as usize)).filter(|(n, _)| !frame_data.iter().any(|(f, _)| f == n)).collect(),
            // Per-frame document arrays are new each call: by content.
            frame_data: frame_data.iter().map(|(n, d)| (n.clone(), d.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, x| (h ^ x.to_bits() as u64).wrapping_mul(0x100_0000_01b3)))).collect(),
        };
        let name = &self.outputs[self.buffer].name;
        if let Some(out) = RUNS.with(|r| r.borrow().iter().find(|e| e.key == key).map(|e| e.out.get(name).cloned().unwrap_or_default())) {
            return Ok(out);
        }
        let out = run_raw(&self.name, &self.kernel, &self.outputs, count, &params, time, &inputs)?;
        let drawn = out.get(name).cloned().unwrap_or_default();
        // The inputs are held with the entry, so their addresses stay theirs.
        let held = inputs.iter().map(|(_, d)| d.clone()).collect();
        RUNS.with(|r| r.borrow_mut().push(RunEntry { key, out, _held: held }));
        Ok(drawn)
    }
}

/// What a kernel run depends on: the kernel, its size, time, params and
/// inputs (by the data they point at).
#[derive(PartialEq)]
struct RunKey {
    kernel: usize,
    count: usize,
    time: u32,
    params: Vec<(String, u32)>,
    inputs: Vec<(String, usize)>,
    frame_data: Vec<(String, u64)>,
}

/// One kernel run of this evaluation: every output, and the inputs it ran
/// on (held, so no other data takes their addresses while cached).
struct RunEntry {
    key: RunKey,
    out: HashMap<String, Arc<Vec<f32>>>,
    _held: Vec<Arc<Vec<f32>>>,
}

thread_local! {
    /// This evaluation's kernel runs (see [`KernelRun::run`]); emptied by
    /// [`clear_runs`].
    static RUNS: std::cell::RefCell<Vec<RunEntry>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Forget the kernel runs of the last evaluation (their inputs' data may be
/// gone and its addresses reused).
pub(crate) fn clear_runs() {
    RUNS.with(|r| r.borrow_mut().clear());
}

/// Runs a document kernel; outputs by name.
pub fn run_kernel(dk: &DocKernel, count: usize, params: &[(String, f32)], time: f32, inputs: &[(String, Arc<Vec<f32>>)]) -> Result<HashMap<String, Arc<Vec<f32>>>, String> {
    run_raw(&dk.name, &dk.kernel, &dk.outputs, count, params, time, inputs)
}

fn run_raw(name: &str, k: &Kernel, outputs: &[KernelBuf], count: usize, params: &[(String, f32)], time: f32, inputs: &[(String, Arc<Vec<f32>>)]) -> Result<HashMap<String, Arc<Vec<f32>>>, String> {
    struct Dk<'a> {
        name: &'a str,
        outputs: &'a [KernelBuf],
    }
    let dk = Dk { name, outputs };
    let mut bufs: Vec<Vec<f32>> = dk.outputs.iter().map(|o| vec![0.0f32; (count * o.width * o.emit.unwrap_or(1)).max(1)]).collect();
    let mut counts: Vec<Vec<u32>> = dk.outputs.iter().map(|o| vec![0u32; if o.emit.is_some() { count.max(1) } else { 0 }]).collect();
    {
        let mut call = k.call();
        call.set_time(time);
        for (name, v) in params {
            call.set_param(name, *v);
        }
        for (name, data) in inputs {
            call.input(name, data).map_err(|e| format!("kernel `{}`: {e}", dk.name))?;
        }
        for ((o, buf), cnt) in dk.outputs.iter().zip(bufs.iter_mut()).zip(counts.iter_mut()) {
            call.output(&o.name, buf).map_err(|e| format!("kernel `{}`: {e}", dk.name))?;
            if o.emit.is_some() {
                call.output_u32(&format!("{}_count", o.name), cnt).map_err(|e| format!("kernel `{}`: {e}", dk.name))?;
            }
        }
        // The compute runtime splits by the kernel's measured cost (a call
        // takes one more thread per ~25 us of work, none when it is cheap).
        let stats = if k.parallel_safe() { call.run_parallel(count, threads()) } else { call.run(count) }.map_err(|e| format!("kernel `{}`: {e}", dk.name))?;
        if stats.overflowed() {
            return Err(format!("kernel `{}`: an emit buffer overflowed (raise its slots per element)", dk.name));
        }
    }
    let mut out = HashMap::new();
    for ((o, buf), cnt) in dk.outputs.iter().zip(bufs).zip(counts) {
        let data = match o.emit {
            Some(cap) => makepad_script_compute_sched::kernel::compact(&buf, &cnt, o.width, cap),
            None => buf,
        };
        out.insert(o.name.clone(), Arc::new(data));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::{DocVm, PRELUDE};

    const DOC: &str = "use mod.scene.*; use mod.kernel; let bump = fn(x, z) {\n    return sin(x) * cos(z)\n}\nlet K = 2.0\nlet walk = Kernel{\n    path: kernel.emit_buffer(vec3, 8)\n    element: fn(i) {\n        for k in 0..8 { emit(path, vec3(float(k), bump(float(k), K), 0.0)) }\n    }\n}\nlet puffs = Kernel{\n    count: 4\n    trail: walk.path\n    size: kernel.param(fn(t) { return t })\n    out: kernel.output(kernel.Sprite)\n    element: fn(i) {\n        out[i].pos = trail[i * 2] + vec3(0.0, time, 0.0)\n        out[i].size = size\n        out[i].color = vec4(1.0, 0.5, 0.2, 1.0)\n    }\n}\nScene3D{}\n";

    /// The document evaluated in a scene VM and its kernel objects loaded;
    /// errors at the document's lines.
    fn load(doc: &str) -> Result<(DocVm, DocKernels), Vec<(u32, String)>> {
        let mut vm = DocVm::new(HashMap::new());
        let first = vm.with(|vm| vm.body_count());
        let errors = vm.with(|vm| {
            vm.capture_errors();
            vm.eval(ScriptMod::new("", "", "motion3d://test/doc", 0, 0, &format!("{PRELUDE}{doc}"), &[]));
            vm.take_errors()
        });
        assert!(errors.is_empty(), "{errors:?}");
        let entries = vm.host.std.kernel_entries.clone().unwrap();
        let mut kernels = DocKernels::default();
        let errors = vm.with(|vm| crate::objects::load(vm, &mut kernels, &entries, first));
        if !errors.is_empty() {
            return Err(errors.into_iter().map(|e| (e.line - crate::script::PRELUDE_LINES, e.message)).collect());
        }
        Ok((vm, kernels))
    }

    #[test]
    fn a_document_loaded_again_compiles_no_kernel() {
        // Compiled kernels are kept for the process: loading the same
        // document again hands out the very kernels of the first.
        // (A document of its own: tests loading DOC meanwhile compile it too.)
        let doc = DOC.replace("return sin(x) * cos(z)", "return sin(x) * cos(z) + 0.125").replace("vec4(1.0, 0.5, 0.2, 1.0)", "vec4(0.5, 0.25, 0.75, 1.0)");
        let (_vm1, first) = load(&doc).unwrap();
        let (_vm2, again) = load(&doc).unwrap();
        for (a, b) in first.list.iter().zip(&again.list) {
            assert!(a.kernel.ptr_eq(&b.kernel), "kernel `{}` compiled twice", a.name);
        }
    }

    #[test]
    fn kernel_objects_load() {
        let (_vm, ks) = load(DOC).unwrap_or_else(|e| panic!("{:?}", e));
        assert_eq!(ks.list.len(), 2);
        assert_eq!((ks.list[0].name.as_str(), ks.list[1].name.as_str()), ("walk", "puffs"));
        assert!(!ks.list[0].per_frame);
        assert!(ks.list[1].per_frame, "reads time");
        assert_eq!(ks.list[1].inputs, vec![("trail".to_string(), 0, "path".to_string())]);
        assert_eq!(ks.list[1].params, vec!["size".to_string()]);
        // The load-time kernel ran: its path is the document's array.
        assert_eq!(ks.list[0].results["path"].len(), 8 * 3);
    }

    #[test]
    fn kernels_run_and_chain() {
        let (_vm, mut ks) = load(DOC).unwrap();
        let r = ks.run(0, 1, &[], 0.0).unwrap();
        let path = r["path"].clone();
        assert_eq!(path.len(), 8 * 3);
        assert!((path[3 * 3 + 1] - (3.0f32.sin() * 2.0f32.cos())).abs() < 1e-5);
        ks.list[0].results = r;
        let r = ks.run(1, 4, &[("size".into(), 0.25)], 1.5).unwrap();
        let out = &r["out"];
        assert_eq!(out.len(), 4 * 8);
        assert_eq!(out[8 + 0], 2.0);
        assert!((out[8 + 1] - (2.0f32.sin() * 2.0f32.cos() + 1.5)).abs() < 1e-5);
        assert_eq!(out[8 + 3], 0.25);
    }

    #[test]
    fn helpers_are_found_by_scope() {
        // Locals and parameters named like a document item shadow it: the
        // item (here one a kernel cannot compile: a string) is not pulled in.
        let doc = "use mod.kernel.*; let lens = fn(t) { return song(\"verse\", t) }\nlet scale = fn(lens) { return lens * 2.0 }\nlet K = 3.0\n\
                   let k = Kernel{\n    count: 4\n    out: output(f32)\n    element: fn(i) {\n        let lens = float(i)\n        out[i] = scale(lens) + K\n    }\n}\n";
        let (_vm, ks) = load(doc).unwrap_or_else(|e| panic!("{e:?}"));
        let r = ks.run(0, 4, &[], 0.0).unwrap();
        assert_eq!(&r["out"][..], &[3.0, 5.0, 7.0, 9.0]);
        // A let read before it is declared is the document's (`let x = x + 1`).
        let doc = "use mod.kernel.*; let x = 2.0\nlet k = Kernel{\n    count: 1\n    out: output(f32)\n    element: fn(i) {\n        let x = x + 1.0\n        out[i] = x\n    }\n}\n";
        let (_vm, ks) = load(doc).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(&ks.run(0, 1, &[], 0.0).unwrap()["out"][..], &[3.0]);
    }

    #[test]
    fn plain_objects_a_kernel_reads_are_constants() {
        // Plain objects, nested, and arrays of them read as constants (a
        // runtime index reads a table); a string field is only an error
        // where it is read.
        let doc = "use mod.kernel.*; let CAM = {cx: 1.0 cy: 2.0, lens: {fov: 40}}\nlet LIGHTS = [{p: vec3(1, 2, 3), w: 0.5}, {p: vec3(4, 5, 6), w: 2}]\nlet STYLE = {name: \"hero\", k: 10}\nlet k = Kernel{\n    count: 2\n    out: output(f32)\n    element: fn(i) {\n        out[i] = CAM.cx + CAM.cy + CAM.lens.fov + LIGHTS[i].p.y * LIGHTS[1].w + STYLE.k\n    }\n}\n";
        let (_vm, ks) = load(doc).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(&ks.run(0, 2, &[], 0.0).unwrap()["out"][..], &[57.0, 63.0]);
        let doc = "use mod.kernel.*; let STYLE = {name: \"hero\", k: 10}\nlet k = Kernel{\n    count: 1\n    out: output(f32)\n    element: fn(i) {\n        out[i] = STYLE.name\n    }\n}\n";
        let e = load(doc).err().expect("an error");
        assert_eq!(e[0].0, 6, "{e:?}");
        assert!(e[0].1.contains("field `name` is a string"), "{}", e[0].1);
    }

    #[test]
    fn an_object_made_from_a_prototype_is_named_where_it_is_read() {
        let doc = "use mod.kernel.*; use mod.scene; let CAM = scene.Camera3D{fov: 40}\nlet OTHER = scene.Camera3D{fov: 1}\nlet k = Kernel{\n    count: 1\n    out: output(f32)\n    element: fn(i) {\n        out[i] = CAM.fov\n    }\n}\n";
        let e = load(doc).err().expect("an error");
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(e[0].0, 7, "{e:?}");
        assert!(e[0].1.contains("an object made from a prototype"), "{}", e[0].1);
        // An object nobody reads breaks nothing.
        let doc = "use mod.kernel.*; use mod.scene; let OTHER = scene.Camera3D{fov: 1}\nlet k = Kernel{\n    count: 1\n    out: output(f32)\n    element: fn(i) {\n        out[i] = 1.0\n    }\n}\n";
        assert!(load(doc).is_ok());
    }

    #[test]
    fn ai_style_document_kernels() {
        // The documents' habits inside a kernel: `let f = fn` helpers
        // calling helpers, local functions and closures over the element's
        // values, palette constants, a param named like a document item,
        // early returns, `if` as a value, loops with a break.
        let doc = "use mod.kernel.*; use mod.shared; let ACCENT = #ff8000\nlet BG = shared.linear_rgb(#000000)\nlet sq = fn(x) { return x * x }\nlet dist = fn(a, b) { return sqrt(sq(a) + sq(b)) }\n\
                   let t = \"not a number\"\nlet clampish = fn(x) {\n    if x > 10.0 { return 10.0 }\n    x\n}\n\
                   let k = Kernel{\n    count: 4\n    gain: param(2.0)\n    out: output(vec4)\n    element: fn(i) {\n\
                   fn ease(t) { t * t }\n        let base = float(i)\n        let scale = fn(x) { x * gain + base }\n\
                   var n = 0\n        for k in 0..10 { if k * k > i { n = k\n break } }\n\
                   let side = if i % 2 == 0 { ACCENT.g } else { BG.x }\n\
                   out[i] = vec4(clampish(scale(dist(3.0, 4.0))), ease(base), float(n), side)\n    }\n}\n";
        let (_vm, ks) = load(doc).unwrap_or_else(|e| panic!("{e:?}"));
        let r = ks.run(0, 4, &[("gain".into(), 2.0)], 0.0).unwrap();
        let g = 128.0f32 / 255.0;
        assert_eq!(&r["out"][..], &[10.0, 0.0, 1.0, g, 10.0, 1.0, 2.0, 0.0, 10.0, 4.0, 2.0, g, 10.0, 9.0, 2.0, 0.0]);
    }

    #[test]
    fn errors_name_the_document_line() {
        let doc = "use mod.kernel.*; let bad = Kernel{\n    out: output(vec3)\n    element: fn(i) {\n        out[i] = nope(1.0)\n    }\n}\n";
        let e = load(doc).err().expect("an error");
        assert_eq!(e[0].0, 4, "{:?}", e);
    }

    #[test]
    fn field_functions_compile_as_vertex_kernels() {
        let doc = "let C = 0.5\nlet f = fn(x, z) {\n    return x * C + z\n}\nf\n";
        let mut vm = DocVm::new(HashMap::new());
        let f = vm.with(|vm| vm.eval(ScriptMod::new("", "", "motion3d://test/field", 0, 0, &format!("{PRELUDE}{doc}"), &[]))).as_object().unwrap();
        let entries = vm.host.std.kernel_entries.clone().unwrap();
        let k = vm.with(|vm| crate::objects::field_kernel(vm, &entries, f)).unwrap_or_else(|e| panic!("{:?}", e));
        let mut h = vec![0.0f32; 9];
        let mut c = k.call();
        for (n, v) in [("nx", 3.0), ("nz", 3.0), ("sx", 2.0), ("sz", 2.0)] {
            c.set_param(n, v);
        }
        c.output("h", &mut h).unwrap();
        c.run(9).unwrap();
        // x, z in -1..1: h = 0.5 x + z.
        assert_eq!(h[0], -1.5);
        assert_eq!(h[8], 1.5);
    }
}

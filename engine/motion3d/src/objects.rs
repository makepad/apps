//! Kernels in documents as Splash objects (KERNELS.md §3.5): the VM
//! evaluates `Kernel{...}` and `Sim{...}` like any value, and the host
//! compiles each object it finds at the document's top level, the way a
//! draw object's shader is compiled: the declarations are read from the
//! object's fields, the entry and every fn and constant it reaches are
//! taken from the VM (makepad-script-compute builds the kernel from the fn
//! objects' tokens), and errors name the document's lines. No document
//! text is cut, rewritten or generated.
//!
//! ```text
//! let descent = Kernel{
//!     path: emit_buffer(vec3, 420)          // records it emits, slots per element
//!     element: fn(i) { ... emit(path, p) }  // the entry: element | vertex | instance | primitive
//! }
//! let sparks = Kernel{
//!     count: 600
//!     trail: descent.path                   // another kernel's output, read as an input
//!     pts: input(vec3, PTS)                 // an array the document computes (or a fn(t))
//!     hits: input(contacts("g"))            // records physics gives each frame
//!     head: param(fn(t) { ... })            // keyable: a number, tween(...) or fn(t)
//!     out: output(Sprite)                   // pos, size, color: one point sprite per element
//!     element: fn(i) { ... out[i].pos = ... }
//! }
//! Points{source: sparks.out}  Line3D{source: descent.path}
//! ```
//!
//! What stays Stage's: the record layouts (Sprite, Segment, physics), how a
//! param is keyed, physics inputs, which kernels run once at load and which
//! every frame, and `Sim{}`'s state stepping.

use crate::kernels::{layouts, DocKernel, DocKernels, DocSim, KernelBuf};
use crate::script::Loc;
use makepad_script_compute::boundary::{DeclKind, DeclView, RecordFieldView, VmKernelView};
use makepad_render_kernels::{RecordField, RecordLayout};
use makepad_script_compute::kernel::{FieldTy, KernelRef, MathMode};
use makepad_script_compute::vm_kernel::Entry;
use makepad_script_compute::Backend;
use makepad_widgets::makepad_script::*;
use std::collections::HashMap;

/// The host's kernel entries, written in Splash (`kernel_entries.splash`):
/// a Sim's init (its state's first values, then the document's `init`), a
/// no-op, and FieldGeometry's vertex kernel around the document's field fn.
pub struct HostEntries {
    pub sim_init: ScriptObject,
    pub none: ScriptObject,
    pub field_vertex: ScriptObject,
    _root: ScriptObjectRef,
}

const ENTRIES_SPLASH: &str = include_str!("kernel_entries.splash");

/// The kernel vocabulary on `module` (a document VM's `mod.scene`, or Motion's
/// `mod.motion`): `Kernel` and `Sim`, `output(ty)`, `emit_buffer(ty,
/// slots)`, `param(value)`, `input(ty, data)` and the
/// record layouts as values (`Sprite`, `Segment`, physics' `Body`, ...).
/// Gives the host's kernel entries.
pub fn register(vm: &mut ScriptVm, module: ScriptObject) -> HostEntries {
    for (name, kind) in [("Kernel", KERNEL), ("Sim", SIM)] {
        let proto = vm.bx.heap_mut().new_object();
        vm.bx.heap_mut().set_value_def(proto, id!(__m3).into(), intern(kind).into());
        vm.bx.heap_mut().set_value_def(module, LiveId::from_str(name).into(), proto.into());
    }
    for layout in layouts() {
        let key = LiveId::from_str(layout.name);
        // A host name it has already (scenes' `Body{..}`) is that layout too.
        let have = vm.bx.heap_ref().value(module, key.into(), NoTrap);
        if !have.is_nil() && !have.is_err() {
            continue;
        }
        let name = vm.new_string_with(|_, s| s.push_str(layout.name));
        let marker = decl(vm, LAYOUT, &[(id!(name), name)]);
        vm.bx.heap_mut().set_value_def(module, key.into(), marker);
    }
    vm.add_method(module, id_lut!(output), script_args!(ty = NIL), |vm, args| {
        let ty = arg(vm, args, id!(ty));
        decl(vm, OUTPUT, &[(id!(ty), ty)])
    });
    vm.add_method(module, id_lut!(emit_buffer), script_args!(ty = NIL, slots = NIL), |vm, args| {
        let (ty, slots) = (arg(vm, args, id!(ty)), arg(vm, args, id!(slots)));
        decl(vm, EMIT, &[(id!(ty), ty), (id!(slots), slots)])
    });
    vm.add_method(module, id_lut!(param), script_args!(value = NIL), |vm, args| {
        let value = arg(vm, args, id!(value));
        decl(vm, PARAM, &[(id!(value), value)])
    });
    vm.add_method(module, id_lut!(input), script_args!(ty = NIL, data = NIL), |vm, args| {
        let (ty, data) = (arg(vm, args, id!(ty)), arg(vm, args, id!(data)));
        decl(vm, INPUT, &[(id!(ty), ty), (id!(data), data)])
    });
    vm.capture_errors();
    let v = vm.eval(ScriptMod::new("", "", "motion3d://kernel_entries.splash", 0, 0, ENTRIES_SPLASH, &[]));
    let errors = vm.take_errors();
    debug_assert!(errors.is_empty(), "kernel_entries.splash: {errors:?}");
    let o = v.as_object().expect("kernel_entries.splash gives an object");
    let f = |vm: &ScriptVm, k: LiveId| vm.bx.heap_ref().value(o, k.into(), NoTrap).as_object().expect("a host kernel entry");
    HostEntries { sim_init: f(vm, id!(sim_init)), none: f(vm, id!(none)), field_vertex: f(vm, id!(field_vertex)), _root: vm.bx.heap_mut().new_object_ref(o) }
}

const KERNEL: &str = "kernel";
const SIM: &str = "sim";
const LAYOUT: &str = "k_layout";
const OUTPUT: &str = "k_output";
const EMIT: &str = "k_emit";
const PARAM: &str = "k_param";
const INPUT: &str = "k_input";

fn intern(s: &str) -> LiveId {
    LiveId::from_str_with_lut(s).unwrap_or_else(|_| LiveId::from_str(s))
}

fn arg(vm: &ScriptVm, args: ScriptObject, name: LiveId) -> ScriptValue {
    let v = vm.bx.heap_ref().value(args, name.into(), NoTrap);
    if v.is_err() { NIL } else { v }
}

/// A declaration value (`{__m3: __anim op: kind ..}`, as the scene's
/// natives tag what they make), made where the document calls it.
fn decl(vm: &mut ScriptVm, kind: &str, fields: &[(LiveId, ScriptValue)]) -> ScriptValue {
    let o = vm.bx.heap_mut().new_object();
    let ip = vm.trap_ip();
    vm.bx.heap_mut().set_made_at(o, ip);
    vm.bx.heap_mut().set_value_def(o, id!(__m3).into(), intern("__anim").into());
    vm.bx.heap_mut().set_value_def(o, id!(op).into(), intern(kind).into());
    for (k, v) in fields {
        vm.bx.heap_mut().set_value_def(o, (*k).into(), *v);
    }
    o.into()
}

fn get(vm: &ScriptVm, o: ScriptObject, key: LiveId) -> ScriptValue {
    let v = vm.bx.heap_ref().value(o, key.into(), NoTrap);
    if v.is_err() { NIL } else { v }
}

/// What a value is tagged as (`op` of a tagged value, `__m3` of a type's
/// object: `kernel`, `sim`, `body`, ...).
fn tag(vm: &ScriptVm, v: ScriptValue) -> Option<String> {
    let o = v.as_object()?;
    let m3 = get(vm, o, id!(__m3)).as_id()?;
    if m3 == intern("__anim") {
        return get(vm, o, id!(op)).as_id().map(|id| id.to_string());
    }
    Some(m3.to_string())
}

fn is_fn(vm: &ScriptVm, v: ScriptValue) -> Option<ScriptObject> {
    v.as_object().filter(|o| vm.bx.heap_ref().is_fn(*o))
}

fn string(vm: &ScriptVm, v: ScriptValue) -> Option<String> {
    vm.bx.heap_ref().string_with(v, |_, s| s.to_string())
}

/// An element type value's name: a pod type (`f32`, `vec3`, ...), a record
/// layout (`Sprite`, `Segment`, a physics record).
fn type_name(vm: &ScriptVm, v: ScriptValue) -> Option<String> {
    match tag(vm, v).as_deref() {
        Some(LAYOUT) => return string(vm, get(vm, v.as_object()?, id!(name))),
        Some("body") => return Some("Body".into()),
        _ => {}
    }
    let ty = vm.bx.heap_ref().pod_type(v)?;
    use makepad_widgets::makepad_script::pod::{ScriptPodMat, ScriptPodTyTag as P, ScriptPodVec as V};
    let ty = vm.bx.heap_ref().pod_type_ref(ty).ty();
    Some(match ty.kind() {
        P::F32 => "f32",
        P::I32 => "i32",
        P::U32 => "u32",
        P::Vec => match ty.as_vec()? {
            V::Vec2f => "vec2",
            V::Vec3f => "vec3",
            V::Vec4f => "vec4",
            _ => return None,
        },
        P::Mat if ty.as_mat() == Some(ScriptPodMat::Mat4x4f) => "mat4",
        _ => return None,
    }
    .to_string())
}

/// Words per record of an element type.
fn type_width(ty: &str) -> Option<usize> {
    Some(match ty {
        "f32" | "i32" | "u32" => 1,
        "vec2" => 2,
        "vec3" => 3,
        "vec4" => 4,
        "mat4" => 16,
        other => layouts().iter().find(|l| l.name == other)?.stride as usize,
    })
}

/// `bodies("g")`, `contacts("g")`, `events("g")`: what physics gives a
/// kernel, and its record layout.
fn physics_input(vm: &ScriptVm, v: ScriptValue) -> Option<(makepad_effect_sim::SimInput, &'static str)> {
    use makepad_effect_sim::SimInput;
    let kind = tag(vm, v)?;
    let group = string(vm, get(vm, v.as_object()?, id!(group))).unwrap_or_default();
    Some(match kind.as_str() {
        "bodies" => (SimInput::Bodies(group), "Rigid"),
        "contacts" => (SimInput::Contacts(group), "Contact"),
        "events" => (SimInput::Events(group), "Event"),
        _ => return None,
    })
}

/// A kernel error at a document place: the file (a document's own body,
/// a module's), 1-based line and column, the kernel and the message.
#[derive(Clone, Debug)]
pub struct KernelError {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub kernel: String,
    pub message: String,
}

/// Where an object was made.
fn made_at(vm: &ScriptVm, o: ScriptObject) -> (String, u32, u32) {
    match vm.bx.code_ref().ip_to_loc(vm.bx.heap_ref().made_at(o)) {
        Some(l) => (l.file().to_string(), l.line() + 1, l.col() + 1),
        None => (String::new(), 1, 1),
    }
}

/// The top-level `Kernel{..}` and `Sim{..}` values of the bodies the VM
/// evaluated from `first` on (a document and its modules), by name, in the
/// order they were made.
fn top_objects(vm: &ScriptVm, first: usize) -> Vec<(String, ScriptObject, bool)> {
    let mut out: Vec<(String, ScriptObject, bool, ScriptIp)> = Vec::new();
    for body in first..vm.body_count() {
        let Some([Some(scope), _, end]) = vm.body_scopes(body as u16) else { continue };
        // From the scope the body ended in back to its own scope: every
        // `let` it made.
        let mut at = end.unwrap_or(scope);
        loop {
            for e in vm.bx.heap_ref().map_entries_ordered(at) {
                let (Some(key), Some(o)) = (e.key.as_id(), e.value.as_object()) else { continue };
                // Made from `Kernel` or `Sim` (not the prototypes themselves,
                // which a `use` brings into the scope too).
                let sim = match vm.bx.heap_ref().proto(o).as_object().and_then(|p| tag(vm, p.into())).as_deref() {
                    Some(KERNEL) => false,
                    Some(SIM) => true,
                    _ => continue,
                };
                if !out.iter().any(|(_, x, _, _)| *x == o) {
                    out.push((key.to_string(), o, sim, vm.bx.heap_ref().made_at(o)));
                }
            }
            if at == scope {
                break;
            }
            match vm.bx.heap_ref().proto(at).as_object() {
                Some(up) => at = up,
                None => break,
            }
        }
    }
    out.sort_by_key(|(_, _, _, ip)| (ip.body, ip.index));
    out.into_iter().map(|(name, o, sim, _)| (name, o, sim)).collect()
}

/// A declaration of a kernel being read, with what owns its strings.
struct Decls {
    kinds: Vec<(DeclKind, String, String, u32)>,
    params: Vec<(String, f32, Option<(f32, f32)>)>,
    record: Option<(String, Vec<(String, u32, [f64; 4], u32)>)>,
}

impl Decls {
    fn new() -> Self {
        Self { kinds: Vec::new(), params: Vec::new(), record: None }
    }

    fn buf(&mut self, kind: DeclKind, name: &str, ty: &str, slots: u32) {
        self.kinds.push((kind, name.into(), ty.into(), slots));
    }

    fn param(&mut self, name: &str, range: Option<(f32, f32)>) {
        self.params.push((name.into(), 0.0, range));
    }

    /// Compiles with the host's layouts (and `extra`) and physics' module.
    fn compile(&self, vm: &ScriptVm, entry: Entry, entry_fn: ScriptObject, bind: &[(String, ScriptObject)]) -> makepad_script_compute::boundary::KernelCompilation {
        let record_fields: Vec<RecordFieldView<'_>> = self.record.iter().flat_map(|(_, fields)| fields.iter().map(|(name, lanes, values, color)| RecordFieldView { name, lanes: *lanes, values: *values, color: *color })).collect();
        let none = DeclView { kind: DeclKind::Param, name: "", ty: "", stride: u32::MAX, offset: u32::MAX, buffer: "", words: 0, capacity: 0, default: 0.0, min: 0.0, max: 0.0, ranged: false, record: &[] };
        let mut decls = Vec::new();
        if let Some((name, _)) = &self.record {
            decls.push(DeclView { kind: DeclKind::Record, name, record: &record_fields, ..none });
        }
        for (kind, name, ty, slots) in &self.kinds {
            decls.push(DeclView { kind: *kind, name, ty, capacity: *slots, ..none });
        }
        for (name, default, range) in &self.params {
            decls.push(DeclView { name, default: *default, min: range.map_or(0.0, |r| r.0), max: range.map_or(0.0, |r| r.1), ranged: range.is_some(), ..none });
        }
        let bind_names: Vec<&str> = bind.iter().map(|(n, _)| n.as_str()).collect();
        let bind_fns: Vec<ScriptObject> = bind.iter().map(|(_, f)| *f).collect();
        let view = VmKernelView { decls: &decls, entry, entry_fn, math: MathMode::Fast, whole_ranges: false, uses: &[], bind_names: &bind_names, bind_fns: &bind_fns, whole_values: true };
        let views = layouts();
        makepad_script_compute::boundary::compile_vm_kernel(vm, &view, &views, Backend::Native, &makepad_effect_sim::modules())
    }
}

/// The kernel, or its errors at their places (`at`: the kernel's own place
/// for an error that points into no fn of the document).
fn compiled(mut c: makepad_script_compute::boundary::KernelCompilation, kernel: &str, at: &(String, u32, u32), errors: &mut Vec<KernelError>, kernels: Option<&mut DocKernels>) -> Option<KernelRef> {
    if let Some(kernels) = kernels {
        for i in 0..c.fn_place_count() {
            let p = c.fn_place(i);
            kernels.fn_sites.push((p.file.to_string(), p.line, p.col, p.end_line, p.end_col));
        }
        for i in 0..c.constant_count() {
            let name = c.constant(i);
            if !kernels.constants.iter().any(|n| n == name) {
                kernels.constants.push(name.to_string());
            }
        }
    }
    if let Some(k) = c.take_kernel() {
        return Some(k);
    }
    for i in 0..c.error_count() {
        let e = match c.error_place(i) {
            Some(p) => KernelError { file: p.file.to_string(), line: p.line, col: p.col, kernel: kernel.into(), message: p.message.to_string() },
            None => KernelError { file: at.0.clone(), line: at.1, col: at.2, kernel: kernel.into(), message: c.error(i).message.to_string() },
        };
        errors.push(e);
    }
    None
}

/// Loads the `Kernel{}` and `Sim{}` objects of the bodies the VM evaluated
/// from `first_body` on (after the ones `kernels` has): each compiles, and
/// one that reads no `time`, keyed param, per-frame input or physics runs
/// now, its outputs becoming arrays; the others' outputs are handles the
/// nodes that draw them run every frame. Consumers find them with
/// [`resolve`]. The errors, at their document places.
pub fn load(vm: &mut ScriptVm, kernels: &mut DocKernels, entries: &HostEntries, first_body: usize) -> Vec<KernelError> {
    let mut errors = Vec::new();
    for (name, o, sim) in top_objects(vm, first_body) {
        if kernels.objects.contains_key(&o) {
            continue;
        }
        let site = vm.bx.heap_ref().made_at(o);
        if !kernels.setup_sites.contains(&site) {
            kernels.setup_sites.push(site);
        }
        let at = made_at(vm, o);
        if sim {
            if let Some(s) = load_sim(vm, kernels, entries, &name, o, &at, &mut errors) {
                let id = kernels.sims.len();
                kernels.sims.push(s);
                let reference = decl(vm, "sim_ref", &[(id!(id), (id as f64).into())]);
                kernels.keep(vm, o.into(), reference);
            }
        } else {
            load_kernel(vm, kernels, &name, o, &at, &mut errors);
        }
    }
    errors
}

const ENTRY_NAMES: &[&str] = &["element", "vertex", "instance", "primitive", "reduce_sum", "reduce_min", "reduce_max"];

fn load_kernel(vm: &mut ScriptVm, kernels: &mut DocKernels, name: &str, o: ScriptObject, at: &(String, u32, u32), errors: &mut Vec<KernelError>) {
    let err = |errors: &mut Vec<KernelError>, place: (String, u32, u32), message: String| errors.push(KernelError { file: place.0, line: place.1, col: place.2, kernel: name.into(), message });
    let mut decls = Decls::new();
    let mut entry = None;
    let mut bind = Vec::new();
    let mut outputs = Vec::new();
    let mut inputs = Vec::new();
    let mut data = Vec::new();
    let mut physics = Vec::new();
    let mut params: Vec<(String, ScriptValue)> = Vec::new();
    let mut data_values: Vec<(String, ScriptValue)> = Vec::new();
    let mut own = Vec::new();
    let mut bad = false;
    for e in vm.bx.heap_ref().map_entries_ordered(o) {
        let Some(key) = e.key.as_id() else { continue };
        let (field, v) = (key.to_string(), e.value);
        if field == "count" || field == "active_count" {
            continue;
        }
        if let Some(f) = is_fn(vm, v) {
            match Entry::from_name(&field).filter(|_| ENTRY_NAMES.contains(&field.as_str())) {
                Some(kind) => entry = Some((kind, f)),
                None => bind.push((field, f)),
            }
            continue;
        }
        let place = v.as_object().map(|d| made_at(vm, d)).unwrap_or_else(|| at.clone());
        // Another kernel's output (`trail: descent.path`).
        if let Some((ki, buf)) = v.as_object().and_then(|d| kernels.outputs.get(&d).cloned()) {
            let ty = kernels.list[ki].outputs.iter().find(|b| b.name == buf).map(|b| b.ty.clone()).unwrap_or_default();
            decls.buf(DeclKind::Input, &field, &ty, 0);
            inputs.push((field, ki, buf));
            continue;
        }
        let d = v.as_object();
        let ty_v = d.map(|d| get(vm, d, id!(ty))).unwrap_or(NIL);
        match tag(vm, v).as_deref() {
            Some(OUTPUT) | Some(EMIT) => {
                let emit = tag(vm, v).as_deref() == Some(EMIT);
                let Some((ty, width)) = type_name(vm, ty_v).and_then(|t| type_width(&t).map(|w| (t, w))) else {
                    err(errors, place, format!("kernel buffer `{field}`: the element type is f32, i32, vec2, vec3, vec4, mat4, Sprite, Segment or Body"));
                    bad = true;
                    continue;
                };
                let slots = if emit { get(vm, d.unwrap(), id!(slots)).as_number().filter(|n| *n >= 1.0 && n.fract() == 0.0).unwrap_or(1.0) as u32 } else { 0 };
                decls.buf(if emit { DeclKind::EmitBuffer } else { DeclKind::Output }, &field, &ty, slots);
                outputs.push(KernelBuf { name: field.clone(), ty, width, emit: emit.then_some(slots as usize) });
                own.push((d.unwrap(), field));
            }
            Some(PARAM) => {
                decls.param(&field, None);
                params.push((field, get(vm, d.unwrap(), id!(value))));
            }
            // `input(ty, data)`.
            Some(INPUT) => {
                let d = d.unwrap();
                let given = get(vm, d, id!(data));
                if let Some((what, layout)) = physics_input(vm, ty_v) {
                    decls.buf(DeclKind::Input, &field, layout, 0);
                    decls.param(&format!("{field}_count"), Some((0.0, 100000000.0)));
                    physics.push((field, what));
                    continue;
                }
                let Some(ty) = type_name(vm, ty_v) else {
                    err(errors, place, format!("kernel input `{field}`: the element type is f32, i32, vec2, vec3, vec4 or a record"));
                    bad = true;
                    continue;
                };
                decls.buf(DeclKind::Input, &field, &ty, 0);
                if !given.is_nil() {
                    match type_width(&ty).filter(|w| *w <= 4) {
                        Some(width) => {
                            data.push((field.clone(), width));
                            data_values.push((field, given));
                        }
                        None => {
                            err(errors, place, format!("kernel input `{field}`: `{ty}` is not f32, i32, vec2, vec3 or vec4"));
                            bad = true;
                        }
                    }
                }
            }
            _ => {
                err(errors, place, format!("Kernel field `{field}`: expected output(..), input(..), emit_buffer(..), param(..), fn(i) {{..}}, another kernel's output or count"));
                bad = true;
            }
        }
    }
    let Some((kind, entry_fn)) = entry else {
        err(errors, at.clone(), "a Kernel needs an entry: element, vertex, instance or primitive: fn(i) { ... }".into());
        return;
    };
    if bad {
        return;
    }
    let compilation = decls.compile(vm, kind, entry_fn, &bind);
    let per_frame = compilation.reads("time") || !physics.is_empty();
    let Some(kernel) = compiled(compilation, name, at, errors, Some(kernels)) else { return };
    let body_out = outputs.iter().any(|b| b.ty == "Body");
    let loc = Loc { line: at.1, col: at.2 };
    let id = kernels.list.len();
    kernels.list.push(DocKernel { name: name.into(), kernel, outputs, inputs, data, data_values: Vec::new(), params: params.iter().map(|(n, _)| n.clone()).collect(), per_frame, physics, body_out, results: HashMap::new(), spec: None, object: Some(o), loc });
    for (d, buf) in &own {
        kernels.outputs.insert(*d, (id, buf.clone()));
    }
    // What `kernel_run` reads: how many elements, the params' values and
    // the document's arrays.
    let spec = vm.bx.heap_mut().new_object();
    for key in [id!(count), id!(active_count)] {
        let v = get(vm, o, key);
        if !v.is_nil() {
            vm.bx.heap_mut().set_value_def(spec, key.into(), v);
        }
    }
    for (n, v) in params.iter().chain(&data_values) {
        vm.bx.heap_mut().set_value_def(spec, LiveId::from_str(n).into(), *v);
    }
    kernels.root(vm, spec.into());
    kernels.list[id].spec = Some(spec);
    match crate::script::kernel_run(vm, kernels, id as f64, spec.into()) {
        Ok(result) => {
            kernels.keep(vm, o.into(), result);
            if let Some(r) = result.as_object() {
                for (d, buf) in &own {
                    let key = LiveId::from_str(buf);
                    let v = get(vm, r, key);
                    kernels.keep(vm, (*d).into(), v);
                    // What the document reads as `k.out` from now on (a
                    // fn(t) the frames call): the array or the handle.
                    vm.bx.heap_mut().set_value_def(o, key.into(), v);
                }
            }
        }
        Err((loc, e)) => errors.push(KernelError { file: at.0.clone(), line: loc.line, col: loc.col, kernel: name.into(), message: e }),
    }
}

/// A state field's type from its first value.
fn state_field(vm: &ScriptVm, v: ScriptValue) -> Option<(FieldTy, u32, [f64; 4], u32)> {
    if let Some(c) = v.as_color() {
        return Some((FieldTy::Vec4, 0, [0.0; 4], c));
    }
    if let Some(x) = v.as_number() {
        return Some((FieldTy::F32, 1, [x, 0.0, 0.0, 0.0], 0));
    }
    let pod = v.as_pod()?;
    let (ty, words) = vm.bx.heap_ref().pod_data(pod);
    use makepad_widgets::makepad_script::pod::ScriptPodVec as V;
    let (t, n) = match ty.ty().as_vec()? {
        V::Vec2f => (FieldTy::Vec2, 2),
        V::Vec3f => (FieldTy::Vec3, 3),
        V::Vec4f => (FieldTy::Vec4, 4),
        _ => return None,
    };
    let mut values = [0.0f64; 4];
    for (k, w) in words.iter().take(n).enumerate() {
        values[k] = f32::from_bits(*w) as f64;
    }
    Some((t, n as u32, values, 0))
}

/// One `Sim{...}` (KERNELS.md §3.5.6): its state steps with the document's
/// physics; `init` (after the state's first values) and `update` are its
/// kernels.
///
/// ```text
/// let sparks = Sim{
///     state: {pos: vec3(0, -10, 0) life: 0.0}   // fields and their first values
///     count: 64  every: 2                        // records; physics steps per sim step
///     hits: input(contacts("confetti"))          // physics inputs, params, as in a Kernel
///     drops: emit_buffer(Body, 1)                // bodies it spawns into group "drops"
///     init: fn(i) { next[i].pos = .. }           // optional: after the first values
///     update: fn(i) { next[i] = prev[i] .. }     // prev: last state (any element), next: this one
/// }
/// ```
fn load_sim(vm: &mut ScriptVm, kernels: &mut DocKernels, entries: &HostEntries, name: &str, o: ScriptObject, at: &(String, u32, u32), errors: &mut Vec<KernelError>) -> Option<DocSim> {
    let mut fail = |message: String| {
        errors.push(KernelError { file: at.0.clone(), line: at.1, col: at.2, kernel: name.into(), message });
        None
    };
    let mut state = RecordLayout { name: "State".into(), stride: 0, fields: Vec::new() };
    let mut defaults = Vec::new();
    let (mut count, mut every) = (None, 1u32);
    let (mut init, mut update) = (None, None);
    let mut decls = Decls::new();
    let mut physics = Vec::new();
    let mut params = Vec::new();
    let mut spawn = None;
    let mut has_state = false;
    for e in vm.bx.heap_ref().map_entries_ordered(o) {
        let Some(key) = e.key.as_id() else { continue };
        let (field, v) = (key.to_string(), e.value);
        match field.as_str() {
            "state" => {
                let Some(so) = v.as_object() else {
                    return fail("a Sim's `state` lists its fields and first values: `state: {pos: vec3(0, 0, 0) life: 0.0}`".into());
                };
                has_state = true;
                for se in vm.bx.heap_ref().map_entries_ordered(so) {
                    let Some(k) = se.key.as_id() else { continue };
                    let Some((ty, lanes, values, color)) = state_field(vm, se.value) else {
                        return fail(format!("a Sim's state field `{k}` starts as a number, a vec2/3/4 or a colour"));
                    };
                    state.fields.push(RecordField { name: k.to_string(), ty, offset: state.stride });
                    state.stride += ty.words();
                    defaults.push((k.to_string(), lanes, values, color));
                }
                if state.fields.is_empty() {
                    return fail("a Sim's `state` needs at least one field".into());
                }
            }
            "count" | "every" => {
                let Some(n) = v.as_number().filter(|n| *n >= 0.0 && n.fract() == 0.0) else {
                    return fail(format!("a Sim's `{field}` is a whole number"));
                };
                if field == "count" {
                    count = Some(n as u32);
                } else {
                    every = (n as u32).max(1);
                }
            }
            "init" | "update" => {
                let Some(f) = is_fn(vm, v) else { return fail(format!("`{field}` is `fn(i) {{ .. }}`")) };
                if field == "init" {
                    init = Some(f);
                } else {
                    update = Some(f);
                }
            }
            _ => match tag(vm, v).as_deref() {
                Some(INPUT) => {
                    let ty = get(vm, v.as_object().unwrap(), id!(ty));
                    let Some((what, layout)) = physics_input(vm, ty) else {
                        return fail("a Sim input is bodies(\"g\"), contacts(\"g\") or events(\"g\")".into());
                    };
                    decls.buf(DeclKind::Input, &field, layout, 0);
                    decls.param(&format!("{field}_count"), Some((0.0, 100000000.0)));
                    physics.push((field, what));
                }
                Some(EMIT) => {
                    let d = v.as_object().unwrap();
                    if type_name(vm, get(vm, d, id!(ty))).as_deref() != Some("Body") {
                        return fail("a Sim emits bodies into physics: `drops: emit_buffer(Body, 1)`".into());
                    }
                    let slots = get(vm, d, id!(slots)).as_number().unwrap_or(1.0).max(1.0) as u32;
                    decls.buf(DeclKind::EmitBuffer, &field, "Body", slots);
                    spawn = Some(field);
                }
                Some(PARAM) => {
                    let value = get(vm, v.as_object().unwrap(), id!(value)).as_number().unwrap_or(0.0) as f32;
                    decls.param(&field, Some((-1000000000.0, 1000000000.0)));
                    params.push((field, value));
                }
                _ => return fail(format!("Sim field `{field}`: expected state, count, every, init, update, input(..), param(..) or emit_buffer(Body, n)")),
            },
        }
    }
    if !has_state {
        return fail("a Sim needs `state: {field: first value ..}`".into());
    }
    let Some(count) = count else { return fail("a Sim needs `count: n` (its records)".into()) };
    let Some(update) = update else { return fail("a Sim needs `update: fn(i) { next[i] = prev[i] .. }`".into()) };
    decls.record = Some(("State".into(), defaults));
    // init: `next[i] = State{}` (the first values), then the document's.
    decls.buf(DeclKind::Output, "next", "State", 0);
    let init_k = compiled(decls.compile(vm, Entry::Element, entries.sim_init, &[("sim_init_user".into(), init.unwrap_or(entries.none))]), &format!("{name}.init"), at, errors, Some(&mut *kernels))?;
    decls.kinds.pop();
    decls.buf(DeclKind::Input, "prev", "State", 0);
    decls.buf(DeclKind::Output, "next", "State", 0);
    let update_k = compiled(decls.compile(vm, Entry::Element, update, &[]), &format!("{name}.update"), at, errors, Some(&mut *kernels))?;
    let program = {
        let mut h = makepad_effect_sim::Fnv::default();
        h.bytes(&init_k.program_key().to_le_bytes());
        h.bytes(&update_k.program_key().to_le_bytes());
        h.finish()
    };
    Some(DocSim { name: name.into(), state, count, every, init: init_k, update: update_k, physics, params, spawn, program, loc: Loc { line: at.1, col: at.2 } })
}

/// What a document value means to the nodes that draw kernels: a loaded
/// kernel's output (`k.out`: its records as an array, or the handle that
/// runs it each frame), a kernel itself (its outputs by name) or a Sim
/// (`sim_ref`); else the value.
pub fn resolve(kernels: &DocKernels, v: ScriptValue) -> ScriptValue {
    v.as_object().and_then(|o| kernels.objects.get(&o).copied()).unwrap_or(v)
}

/// FieldGeometry's `field: fn(x, z) { .. }` as a vertex kernel: one height
/// per vertex of an `nx` by `nz` grid of size `sx` by `sz` in `h`.
pub fn field_kernel(vm: &ScriptVm, entries: &HostEntries, field: ScriptObject) -> Result<KernelRef, Vec<KernelError>> {
    let mut decls = Decls::new();
    decls.buf(DeclKind::Output, "h", "f32", 0);
    for p in ["nx", "nz", "sx", "sz"] {
        decls.param(p, None);
    }
    let at = made_at(vm, field);
    let mut errors = Vec::new();
    match compiled(decls.compile(vm, Entry::Vertex, entries.field_vertex, &[("field_fn".into(), field)]), "FieldGeometry.field", &at, &mut errors, None) {
        Some(k) => Ok(k),
        None => Err(errors),
    }
}

//! The VM side of a model program: `mod.model` (the part builders `m.box(..)`,
//! `m.subtract(..)`, the value types, `param`), the host's root calls,
//! evaluation in a fresh VM, and reading the result back as plain values that
//! remember where they were made.
//!
//! A builder records its part (an object of the part's prototype holding the
//! fields it was given, made at the call) and returns an opaque handle: a part
//! is never a value with fields or operators, only something later builders
//! and root calls take.

use super::protos::{self, PROTOS};
use super::{input_key, Diag, Input, Inputs, Library, RecordValue, Refusal, RootFn};
use makepad_script::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub const DOC_FILE: &str = "model.splash";
/// Instructions one evaluation may run.
pub const EVAL_LIMIT: usize = 20_000_000;
const MAX_DEPTH: usize = 64;

/// A value read back from the VM.
#[derive(Clone, Debug)]
pub enum V {
    Nil,
    Bool(bool),
    Num(f64),
    Str(String),
    /// `@name`.
    Id(String),
    /// A colour (`#rrggbb`), 0xRRGGBBAA.
    Color(u32),
    Arr(Vec<V>),
    Obj(Arc<Obj>),
    /// A function (kept alive by the evaluation).
    Fn(ScriptValue),
    Other(String),
}

/// An object as written: its prototype, own fields, keyless entries and where
/// it was made (1-based line and column).
#[derive(Debug)]
pub struct Obj {
    /// Heap identity: the same object read twice is one `Obj`.
    pub ident: usize,
    /// The part (index into the builders' record) this is, when it is one.
    pub part: Option<usize>,
    pub proto: Option<String>,
    pub fields: Vec<(String, V)>,
    pub items: Vec<V>,
    pub loc: Option<(u32, u32)>,
}

impl Obj {
    pub fn get(&self, key: &str) -> Option<&V> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

impl V {
    pub fn num(&self) -> Option<f64> {
        match self {
            V::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn obj(&self) -> Option<&Arc<Obj>> {
        match self {
            V::Obj(o) => Some(o),
            _ => None,
        }
    }
    pub fn describe(&self) -> String {
        match self {
            V::Nil => "nothing".into(),
            V::Bool(b) => format!("{b}"),
            V::Num(n) => format!("{n}"),
            V::Str(s) => format!("\"{s}\""),
            V::Id(s) => format!("@{s}"),
            V::Color(c) => format!("#{c:08x}"),
            V::Arr(a) => format!("a list of {}", a.len()),
            V::Obj(o) => match &o.proto {
                Some(p) => format!("a {p}"),
                None => "an object".into(),
            },
            V::Fn(_) => "a fn".into(),
            V::Other(s) => s.clone(),
        }
    }
}

/// One `param(...)` call: the value it returned and its declaration.
#[derive(Clone, Debug, Default)]
pub struct ParamCall {
    pub value: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub label: Option<String>,
    pub group: Option<String>,
    pub kind: Option<String>,
    pub hint: Option<String>,
    /// Where the call is (1-based line and column of `param`).
    pub loc: Option<(u32, u32)>,
}

/// One builder call (`m.box(..)`): the part object it made, where it was called and
/// the calls of script fns it was made inside (outermost first).
pub struct PartCall {
    /// The builder (`box`).
    pub call: &'static str,
    pub object: ScriptObject,
    pub loc: Option<(u32, u32)>,
    pub stack: Vec<(u32, u32)>,
    /// For a generated part: the iteration position of each `for` loop it was made in,
    /// outermost first (empty for other parts).
    pub loops: Vec<u32>,
    /// For a piece of a call that makes several (`m.split`): its place in the list the call
    /// gives (`halves[1]`).
    pub piece: Option<u32>,
}

/// One root call (`place(...)`): its arguments, read after the evaluation.
pub struct RootCall {
    pub kind: &'static str,
    pub args: Vec<(String, ScriptValue)>,
    pub loc: Option<(u32, u32)>,
}

#[derive(Default)]
pub struct Ctx {
    pub params: Vec<ParamCall>,
    pub parts: Vec<PartCall>,
    pub roots: Vec<RootCall>,
    /// Param values to use instead of the written ones, by call location.
    pub overrides: HashMap<(u32, u32), f64>,
    /// Keeps root calls and the object holding the host inputs alive until they are read.
    pub keep: Vec<ScriptObjectRef>,
    /// The texture library (`texture.splash`): its patterns and mappings by name.
    pub texture: Option<ScriptObjectRef>,
    pub inputs: BTreeMap<(String, String), ScriptValue>,
    pub inputs_read: Vec<(String, String)>,
}

/// The patterns and mappings `m.texture` compiles its kernels from.
pub const TEXTURE_LIB: &str = include_str!("texture.splash");

pub type Shared = std::rc::Rc<RefCell<Ctx>>;

/// An id whose text reads back.
pub fn lut_id(name: &str) -> LiveId {
    LiveId::from_str_with_lut(name).unwrap_or_else(|_| LiveId::from_str(name))
}

fn id_string(id: LiveId) -> String {
    id.as_string(|s| s.map(str::to_string)).unwrap_or_else(|| format!("{id}"))
}

/// The document's location of an ip (1-based line, 1-based column).
pub fn loc_of_ip(vm: &ScriptVm, ip: ScriptIp) -> Option<(u32, u32)> {
    let loc = vm.bx.code_ref().ip_to_loc(ip)?;
    loc.file().ends_with(DOC_FILE).then_some((loc.line(), loc.col() + 1))
}

fn text_of(vm: &ScriptVm, value: ScriptValue) -> Option<String> {
    if let Some(id) = value.as_id() {
        return Some(id_string(id));
    }
    if let Some(s) = value.as_string() {
        return Some(vm.bx.heap_ref().string(s).to_string());
    }
    value.as_inline_string(|s| s.to_string())
}

/// A heat-set insert's size as `m.heat_set` gives it: its numbers as lengths and its name.
fn heat_set_value(vm: &mut ScriptVm, row: &super::heat_set::HeatSet) -> ScriptValue {
    let name = vm.bx.heap_mut().new_string_from_str(row.name);
    let heap = vm.bx.heap_mut();
    let out = heap.new_object();
    let fields = [
        (id_lut!(diameter), row.diameter),
        (id_lut!(pitch), row.pitch),
        (id_lut!(outer), row.outer),
        (id_lut!(length), row.length),
        (id_lut!(collar), row.collar),
        (id_lut!(hole), row.hole),
        (id_lut!(depth), row.depth),
        (id_lut!(wall), row.wall),
        (id_lut!(floor), row.floor),
    ];
    for (key, mm) in fields {
        heap.set_value_def(out, key.into(), (mm * 0.001).into());
    }
    heap.set_value_def(out, id_lut!(size).into(), name.into());
    out.into()
}

fn arg(vm: &ScriptVm, args: ScriptObject, key: LiveId) -> ScriptValue {
    vm.bx.heap_ref().value(args, key.into(), NoTrap)
}

/// An options object: an object that is no fn, no part and no value of a model type.
fn p_is_options(heap: &ScriptHeap, v: ScriptValue) -> bool {
    let Some(o) = v.as_object() else { return false };
    if heap.is_fn(o) {
        return false;
    }
    match heap.value(o, id_lut!(__proto).into(), NoTrap).as_id() {
        Some(id) => id != id_lut!(Part) && id != id_lut!(Face) && id != id_lut!(Edge) && !PROTOS.iter().any(|p| lut_id(p.name) == id),
        None => true,
    }
}

/// Whether `v` is an object of the model type `tag` (`Part`, `Face`, `Edge`).
fn is_a(heap: &ScriptHeap, v: ScriptValue, tag: LiveId) -> bool {
    v.as_object().is_some_and(|o| heap.value(o, id_lut!(__proto).into(), NoTrap).as_id() == Some(tag))
}

/// An exact part's faces by name: its builder's own faces as fields (`b.top`), and which of the
/// methods `side`, `face`, `corner`, `inner` name the faces it makes.
fn brep_faces(proto: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match proto {
        "BrepBox" => (&["top", "bottom", "left", "right", "front", "back"], &[]),
        "BrepCylinder" => (&["wall", "top", "bottom"], &[]),
        "BrepSphere" | "BrepTorus" => (&["wall"], &[]),
        "BrepExtrude" => (&["top", "bottom"], &["side"]),
        "BrepRevolve" | "BrepSweep" => (&["start", "end"], &["side"]),
        "BrepLoft" => (&["start", "end"], &[]),
        "BrepFillet" | "BrepChamfer" => (&[], &["face", "corner"]),
        "BrepShell" => (&[], &["inner"]),
        // Each half's face on the cut (on an exact part).
        "Split" => (&["cut"], &[]),
        _ => (&[], &[]),
    }
}

/// `v` made at the call running now: its site is where the script wrote the call (a selector
/// a step lost is named again there, [`super::Solid::selectors`]).
fn made_here(vm: &mut ScriptVm, v: ScriptValue) -> ScriptValue {
    let ip = vm.trap_ip();
    if let Some(o) = v.as_object() {
        vm.bx.heap_mut().set_made_at(o, ip);
    }
    v
}

/// A face of `part` by `role`: `{__proto: @Face, of: part, role: @role, ..fields}`.
fn new_face(heap: &mut ScriptHeap, part: ScriptValue, role: LiveId, fields: &[(LiveId, ScriptValue)]) -> ScriptValue {
    let face = heap.new_object();
    heap.set_value_def(face, id_lut!(__proto).into(), ScriptValue::from_id(id_lut!(Face)));
    heap.set_value_def(face, id_lut!(of).into(), part);
    heap.set_value_def(face, id_lut!(role).into(), ScriptValue::from_id(role));
    for (k, v) in fields {
        heap.set_value_def(face, (*k).into(), *v);
    }
    face.into()
}

/// The methods that name the faces an exact step makes, on an object of their own: `r.face(m.edge(a,
/// b))`, `r.corner(a, b, c)`, `e.side(2)`, `s.inner()` / `s.inner(b.front)`. A builder's handle
/// holds the ones of its step ([`brep_faces`]).
fn face_methods(vm: &mut ScriptVm) -> ScriptObject {
    let on = vm.bx.heap_mut().new_object();
    vm.add_method(on, id_lut!(face), script_args!(edge = NIL), |vm, args| {
        let me = arg(vm, args, id!(self));
        let edge = arg(vm, args, id_lut!(edge));
        let heap = vm.bx.heap_ref();
        if !is_a(heap, me, id_lut!(Part)) || !is_a(heap, edge, id_lut!(Edge)) {
            return script_err_invalid_args!(vm.trap(), "the face a fillet or chamfer made along an edge: r.face(m.edge(b.top, b.front))");
        }
        let o = edge.as_object().unwrap();
        let (a, b) = (heap.value(o, id_lut!(a).into(), NoTrap), heap.value(o, id_lut!(b).into(), NoTrap));
        let face = new_face(vm.bx.heap_mut(), me, id_lut!(blend), &[(id_lut!(a), a), (id_lut!(b), b)]);
        made_here(vm, face)
    });
    vm.add_method(on, id_lut!(corner), script_args!(a = NIL, b = NIL, c = NIL, d = NIL), |vm, args| {
        let me = arg(vm, args, id!(self));
        let round: Vec<ScriptValue> = [id_lut!(a), id_lut!(b), id_lut!(c), id_lut!(d)].iter().map(|k| arg(vm, args, *k)).filter(|v| !v.is_nil()).collect();
        let heap = vm.bx.heap_ref();
        if !is_a(heap, me, id_lut!(Part)) || round.len() < 2 || !round.iter().all(|v| is_a(heap, *v, id_lut!(Face))) {
            return script_err_invalid_args!(vm.trap(), "the patch where blends meet, by the faces round it: r.corner(b.top, b.front, b.right)");
        }
        let heap = vm.bx.heap_mut();
        let list = heap.new_array();
        for v in round {
            heap.array_push_unchecked(list, v);
        }
        let face = new_face(heap, me, id_lut!(corner), &[(id_lut!(round), list.into())]);
        made_here(vm, face)
    });
    vm.add_method(on, id_lut!(side), script_args!(index = NIL), |vm, args| {
        let me = arg(vm, args, id!(self));
        let index = arg(vm, args, id_lut!(index));
        let ok = index.as_number().is_some_and(|i| i >= 0.0 && i.fract() == 0.0);
        if !is_a(vm.bx.heap_ref(), me, id_lut!(Part)) || !ok {
            return script_err_invalid_args!(vm.trap(), "the side made from sketch segment i (0 the first): e.side(2)");
        }
        let face = new_face(vm.bx.heap_mut(), me, id_lut!(side), &[(id_lut!(index), index)]);
        made_here(vm, face)
    });
    vm.add_method(on, id_lut!(inner), script_args!(outer = NIL), |vm, args| {
        let me = arg(vm, args, id!(self));
        let outer = arg(vm, args, id_lut!(outer));
        let heap = vm.bx.heap_ref();
        if !is_a(heap, me, id_lut!(Part)) || !(outer.is_nil() || is_a(heap, outer, id_lut!(Face))) {
            return script_err_invalid_args!(vm.trap(), "a shell's inner faces: s.inner() every one, s.inner(b.front) the one offset from b.front");
        }
        let fields: Vec<(LiveId, ScriptValue)> = if outer.is_nil() { Vec::new() } else { vec![(id_lut!(outer), outer)] };
        let face = new_face(vm.bx.heap_mut(), me, id_lut!(inner), &fields);
        made_here(vm, face)
    });
    on
}

fn install(vm: &mut ScriptVm, module: ScriptObject, ext: ScriptObject, ctx: Shared, roots: &[RootFn]) {
    let c = ctx.clone();
    vm.add_method(
        module,
        id_lut!(param),
        script_args!(value = NIL, min = NIL, max = NIL, step = NIL, label = NIL, group = NIL, kind = NIL, hint = NIL),
        move |vm, args| {
            let num = |k: LiveId| arg(vm, args, k).as_number();
            let Some(value) = num(id!(value)) else {
                return script_err_invalid_args!(vm.trap(), "param needs a number first: param(10mm, min: 2mm, max: 40mm)");
            };
            let ip = vm.trap_ip();
            let loc = loc_of_ip(vm, ip);
            // An instance's own value (a project's override) is the value it returns.
            let value = loc.and_then(|l| c.borrow().overrides.get(&l).copied()).unwrap_or(value);
            let call = ParamCall {
                value,
                min: num(id!(min)),
                max: num(id!(max)),
                step: num(id!(step)),
                label: text_of(vm, arg(vm, args, id!(label))),
                group: text_of(vm, arg(vm, args, id!(group))),
                kind: text_of(vm, arg(vm, args, id!(kind))),
                hint: text_of(vm, arg(vm, args, id!(hint))),
                loc,
            };
            c.borrow_mut().params.push(call);
            value.into()
        },
    );
    // `m.metric("M8")` or `m.metric(d)`: a standard size's dimensions, as lengths.
    vm.add_method(module, id_lut!(metric), script_args!(size = NIL), move |vm, args| {
        let size = arg(vm, args, id!(size));
        let row = match size.as_number() {
            Some(d) => makepad_csg_cad::metric_size(d * 1000.0),
            None => text_of(vm, size).and_then(|t| t.strip_prefix('M')?.parse().ok()).and_then(makepad_csg_cad::metric_size),
        };
        let Some(row) = row else {
            let known = makepad_csg_cad::metric_names().join(", ");
            return script_err_invalid_args!(vm.trap(), "m.metric takes a standard size, \"M8\" or a diameter ({known})");
        };
        let heap = vm.bx.heap_mut();
        let out = heap.new_object();
        let fields = [
            (id_lut!(diameter), row.diameter),
            (id_lut!(pitch), row.pitch()),
            (id_lut!(across_flats), row.across_flats),
            (id_lut!(across_corners), row.across_corners()),
            (id_lut!(head_height), row.head_height),
            (id_lut!(nut_height), row.nut_height),
            (id_lut!(hole_close), row.hole[0]),
            (id_lut!(hole_normal), row.hole[1]),
            (id_lut!(hole_loose), row.hole[2]),
        ];
        for (key, mm) in fields {
            heap.set_value_def(out, key.into(), (mm * 0.001).into());
        }
        out.into()
    });
    // `m.heat_set("M3")` or `m.heat_set(d)`: a heat-set insert's size and the hole it wants, as
    // lengths (`super::heat_set`), and its name.
    vm.add_method(module, id_lut!(heat_set), script_args!(size = NIL), move |vm, args| {
        let size = arg(vm, args, id!(size));
        let row = match size.as_number() {
            Some(d) => super::heat_set::by_diameter(d * 1000.0),
            None => text_of(vm, size).and_then(|t| super::heat_set::by_name(&t)),
        };
        let Some(row) = row else {
            let known = super::heat_set::names().join(", ");
            return script_err_invalid_args!(vm.trap(), "m.heat_set takes a heat-set insert's thread, \"M3\" or a diameter ({known})");
        };
        heat_set_value(vm, row)
    });
    // `m.screw("M3")`, `m.screw(d)` or `m.screw({drill: 3.2mm})`: a machine screw's size and the
    // holes and bosses a printed case keeps for it, as lengths (`super::screw`), with its heat-set
    // insert's numbers and boss under `heat_set`. By drill, the largest screw that passes it, or
    // nil when none does.
    vm.add_method(module, id_lut!(screw), script_args!(size = NIL), move |vm, args| {
        let size = arg(vm, args, id!(size));
        let drill = size.as_object().map(|o| vm.bx.heap_ref().value(o, id_lut!(drill).into(), NoTrap));
        let row = match (drill, size.as_number()) {
            (Some(drill), _) => match drill.as_number() {
                Some(d) => match super::screw::for_drill(d * 1000.0) {
                    Some(row) => Some(row),
                    None => return NIL,
                },
                None => return script_err_invalid_args!(vm.trap(), "m.screw({{drill: ..}}) takes the drill as a length"),
            },
            (None, Some(d)) => super::screw::by_diameter(d * 1000.0),
            (None, None) => text_of(vm, size).and_then(|t| super::screw::by_name(&t)),
        };
        let Some(row) = row else {
            let known = super::screw::names().join(", ");
            return script_err_invalid_args!(vm.trap(), "m.screw takes a screw's thread, \"M3\" or a diameter ({known}), or {{drill: ..}}");
        };
        let heat = heat_set_value(vm, row.heat_set());
        if let Some(h) = heat.as_object() {
            vm.bx.heap_mut().set_value_def(h, id_lut!(boss).into(), (row.heat_boss() * 0.001).into());
        }
        let name = vm.bx.heap_mut().new_string_from_str(row.name);
        let heap = vm.bx.heap_mut();
        let out = heap.new_object();
        let fields = [
            (id_lut!(diameter), row.diameter),
            (id_lut!(clear), row.clear),
            (id_lut!(pilot), row.pilot),
            (id_lut!(boss), row.boss),
            (id_lut!(head), row.head),
        ];
        for (key, mm) in fields {
            heap.set_value_def(out, key.into(), (mm * 0.001).into());
        }
        heap.set_value_def(out, id_lut!(size).into(), name.into());
        heap.set_value_def(out, id_lut!(heat_set).into(), heat);
        out.into()
    });
    let linked = ctx.clone();
    vm.add_method(module, id_lut!(link), script_args!(part = NIL, key = NIL), move |vm, args| {
        let value = arg(vm, args, id!(part));
        let key = arg(vm, args, id!(key));
        let index = value.as_object().and_then(|o| {
            let heap = vm.bx.heap_ref();
            (heap.value(o, id_lut!(__proto).into(), NoTrap).as_id() == Some(id_lut!(Part)))
                .then(|| heap.value(o, id_lut!(part).into(), NoTrap).as_number()).flatten()
        });
        let object = index.and_then(|i| linked.borrow().parts.get(i as usize).map(|p| p.object));
        let Some(object) = object else {
            return script_err_invalid_args!(vm.trap(), "m.link needs a part first: m.link(part, \"host:key\")");
        };
        if key.as_id().is_some() || text_of(vm, key).is_none() {
            return script_err_invalid_args!(vm.trap(), "m.link's key must be text");
        }
        vm.bx.heap_mut().set_value_def(object, id_lut!(link).into(), key);
        value
    });
    let optical = ctx.clone();
    vm.add_method(module, id_lut!(optics), script_args!(part = NIL, optics = NIL), move |vm, args| {
        let value = arg(vm, args, id!(part));
        let optics = arg(vm, args, id!(optics));
        let index = value.as_object().and_then(|o| {
            let heap = vm.bx.heap_ref();
            (heap.value(o, id_lut!(__proto).into(), NoTrap).as_id() == Some(id_lut!(Part)))
                .then(|| heap.value(o, id_lut!(part).into(), NoTrap).as_number()).flatten()
        });
        let object = index.and_then(|i| optical.borrow().parts.get(i as usize).map(|p| p.object));
        let Some(object) = object else {
            return script_err_invalid_args!(vm.trap(), "m.optics needs a part first: m.optics(part, {{throughput: 0.2}})");
        };
        // Lowering validates the same record for this call and the solid's optics field.
        vm.bx.heap_mut().set_value_def(object, id_lut!(optics).into(), optics);
        value
    });
    let kept = ctx.clone();
    vm.add_method(module, id_lut!(keepouts), script_args!(part = NIL, keepouts = NIL), move |vm, args| {
        let value = arg(vm, args, id!(part));
        let keepouts = arg(vm, args, id!(keepouts));
        let index = value.as_object().and_then(|o| {
            let heap = vm.bx.heap_ref();
            (heap.value(o, id_lut!(__proto).into(), NoTrap).as_id() == Some(id_lut!(Part)))
                .then(|| heap.value(o, id_lut!(part).into(), NoTrap).as_number()).flatten()
        });
        let object = index.and_then(|i| kept.borrow().parts.get(i as usize).map(|p| p.object));
        let Some(object) = object else {
            return script_err_invalid_args!(vm.trap(), "m.keepouts needs a part first: m.keepouts(part, [..])");
        };
        // Lowering validates the same records for this call and the solid's keepouts field.
        vm.bx.heap_mut().set_value_def(object, id_lut!(keepouts).into(), keepouts);
        value
    });
    // `m.edge(b.top, b.front, {near: [..] chain: true})`: an exact edge by the two faces it joins.
    vm.add_method(module, id_lut!(edge), script_args!(a = NIL, b = NIL, opts = NIL), |vm, args| {
        let (a, b, opts) = (arg(vm, args, id_lut!(a)), arg(vm, args, id_lut!(b)), arg(vm, args, id_lut!(opts)));
        let heap = vm.bx.heap_ref();
        if !is_a(heap, a, id_lut!(Face)) || !is_a(heap, b, id_lut!(Face)) {
            return script_err_invalid_args!(vm.trap(), "m.edge takes the two faces it joins: m.edge(b.top, b.front), or m.edge(b.top, b.front, {{near: [x y z]}})");
        }
        let mut extra = Vec::new();
        if let Some(o) = opts.as_object() {
            let mut entries: Vec<(ScriptValue, ScriptValue)> = heap.map_entries(o).iter().map(|e| (e.key, e.value)).collect();
            for i in 0..heap.vec_len(o) {
                let e = heap.vec_key_value(o, i, NoTrap);
                entries.push((e.key, e.value));
            }
            for (key, value) in entries {
                match key.as_id() {
                    Some(k) if k == id_lut!(near) || k == id_lut!(chain) => extra.push((k, value)),
                    Some(k) if k == id_lut!(__proto) => {}
                    _ => return script_err_invalid_args!(vm.trap(), "m.edge's options are near: [x y z] (the piece nearest it) and chain: true (and the edges running on from it)"),
                }
            }
        }
        let heap = vm.bx.heap_mut();
        let edge = heap.new_object();
        heap.set_value_def(edge, id_lut!(__proto).into(), ScriptValue::from_id(id_lut!(Edge)));
        heap.set_value_def(edge, id_lut!(a).into(), a);
        heap.set_value_def(edge, id_lut!(b).into(), b);
        for (k, v) in extra {
            heap.set_value_def(edge, k.into(), v);
        }
        made_here(vm, edge.into())
    });
    let methods = face_methods(vm);
    let method_fns: Vec<(LiveId, ScriptValue)> = ["face", "corner", "side", "inner"].iter().map(|m| (lut_id(m), vm.bx.heap_ref().value(methods, lut_id(m).into(), NoTrap))).collect();
    ctx.borrow_mut().keep.push(vm.bx.heap_mut().new_object_ref(methods));
    // `m.brep.box(..)`: the exact engine's builders under their own name, like the others.
    let brep = vm.bx.heap_mut().new_object();
    vm.bx.heap_mut().set_value_def(module, id_lut!(brep).into(), brep.into());
    for p in PROTOS.iter().filter(|p| !p.call.is_empty()) {
        let c = ctx.clone();
        let tag = lut_id(p.name);
        let (roles, made) = brep_faces(p.name);
        let made: Vec<(LiveId, ScriptValue)> = method_fns.iter().filter(|(m, _)| made.iter().any(|n| lut_id(n) == *m)).cloned().collect();
        let module = if p.group == protos::Group::Brep { brep } else { module };
        // Arguments: the call's own in order, the options object, then every field by name.
        let mut keys: Vec<&'static str> = p.args.to_vec();
        keys.push("opts");
        keys.extend(p.fields.iter().chain(protos::PLACEMENT).map(|f| f.key).filter(|k| !p.args.contains(k)));
        let ids: Vec<LiveId> = keys.iter().map(|k| lut_id(k)).collect();
        let defaults: Vec<(LiveId, ScriptValue)> = ids.iter().map(|n| (*n, NIL)).collect();
        let call = p.call;
        vm.add_method(module, lut_id(call), &defaults, move |vm, args| {
            let ip = vm.trap_ip();
            let loc = loc_of_ip(vm, ip);
            // The calls of script fns it is made inside, outermost first (the first site is this call).
            let mut sites = Vec::new();
            vm.call_sites(&mut sites, MAX_DEPTH);
            let stack: Vec<(u32, u32)> = sites.iter().skip(1).rev().filter_map(|ip| loc_of_ip(vm, *ip)).collect();
            let mut loops = Vec::new();
            if call == "generated" {
                vm.loop_positions(&mut loops);
            }
            let mut opts = arg(vm, args, id_lut!(opts)).as_object();
            let heap = vm.bx.heap_mut();
            let part = heap.new_object();
            heap.set_value_def(part, id_lut!(__proto).into(), ScriptValue::from_id(tag));
            for (key, id) in keys.iter().zip(&ids) {
                if *key == "opts" {
                    continue;
                }
                let v = heap.value(args, (*id).into(), NoTrap);
                if v.is_nil() {
                    continue;
                }
                // An options object given where a field goes (`m.fillet(body, {radius: 1mm})`).
                if opts.is_none() && p_is_options(heap, v) {
                    opts = v.as_object();
                    continue;
                }
                heap.set_value_def(part, (*id).into(), v);
            }
            // Generator options belong to the host, including names such as `at` that
            // ordinary builders use for placement. Preserve the whole parameter object.
            if call == "generated" {
                if let Some(o) = opts {
                    heap.set_value_def(part, lut_id("params").into(), o.into());
                }
                opts = None;
            }
            if let Some(o) = opts {
                let mut entries: Vec<(ScriptValue, ScriptValue)> = heap.map_entries(o).iter().map(|e| (e.key, e.value)).collect();
                for i in 0..heap.vec_len(o) {
                    let e = heap.vec_key_value(o, i, NoTrap);
                    if !e.key.is_nil() {
                        entries.push((e.key, e.value));
                    }
                }
                for (k, v) in entries {
                    if k.as_id() != Some(id_lut!(__proto)) {
                        heap.set_value_def(part, k, v);
                    }
                }
            }
            heap.set_made_at(part, ip);
            // A split gives its pieces as a list: below and above the cut, and the pins with
            // dowels. Each is a part of its own with the call's fields ([`PartCall::piece`]).
            let pieces = if call == "split" {
                Some(2 + heap.value(part, lut_id("dowels").into(), NoTrap).as_bool().unwrap_or(false) as u32)
            } else {
                None
            };
            let parts: Vec<(ScriptObject, Option<u32>)> = match pieces {
                None => vec![(part, None)],
                Some(n) => {
                    let mut fields: Vec<(ScriptValue, ScriptValue)> = heap.map_entries(part).iter().map(|e| (e.key, e.value)).collect();
                    for i in 0..heap.vec_len(part) {
                        let e = heap.vec_key_value(part, i, NoTrap);
                        if !e.key.is_nil() {
                            fields.push((e.key, e.value));
                        }
                    }
                    (0..n)
                        .map(|i| {
                            let o = if i == 0 { part } else { heap.new_object() };
                            if i > 0 {
                                for (k, v) in &fields {
                                    heap.set_value_def(o, *k, *v);
                                }
                                heap.set_made_at(o, ip);
                            }
                            (o, Some(i))
                        })
                        .collect()
                }
            };
            let mut ctx = c.borrow_mut();
            let mut handles = Vec::with_capacity(parts.len());
            for (object, piece) in parts {
                let handle = heap.new_object();
                heap.set_value_def(handle, id_lut!(__proto).into(), ScriptValue::from_id(id_lut!(Part)));
                heap.set_value_def(handle, id_lut!(part).into(), (ctx.parts.len() as f64).into());
                // An exact part names its faces: `b.top`, and `r.face(..)` for those its step makes.
                for role in roles {
                    let face = new_face(heap, handle.into(), lut_id(role), &[]);
                    heap.set_value_def(handle, lut_id(role).into(), face);
                }
                for (m, f) in &made {
                    heap.set_value_def(handle, (*m).into(), *f);
                }
                ctx.keep.push(heap.new_object_ref(object));
                ctx.parts.push(PartCall { call, object, loc, stack: stack.clone(), loops: loops.clone(), piece });
                handles.push(handle);
            }
            if pieces.is_none() {
                return handles[0].into();
            }
            let list = heap.new_array();
            for h in handles {
                heap.array_push_unchecked(list, h.into());
            }
            list.into()
        });
    }
    for root in roots {
        let c = ctx.clone();
        let kind = root.name;
        let names: Vec<LiveId> = root.args.iter().map(|a| lut_id(a)).collect();
        let defaults: Vec<(LiveId, ScriptValue)> = names.iter().map(|n| (*n, NIL)).collect();
        let arg_names: Vec<&'static str> = root.args.to_vec();
        let on = if root.model { module } else { ext };
        vm.add_method(on, lut_id(kind), &defaults, move |vm, args| {
            let ip = vm.trap_ip();
            let loc = loc_of_ip(vm, ip);
            let mut got = Vec::new();
            let mut keep = Vec::new();
            for (name, id) in arg_names.iter().zip(&names) {
                let v = arg(vm, args, *id);
                if !v.is_nil() {
                    if let Some(o) = v.as_object() {
                        keep.push(vm.bx.heap_mut().new_object_ref(o));
                    }
                    got.push((name.to_string(), v));
                }
            }
            let mut ctx = c.borrow_mut();
            ctx.keep.extend(keep);
            ctx.roots.push(RootCall { kind, args: got, loc });
            NIL
        });
    }
}

/// A document evaluated in its VM, the VM kept (its fns are compiled from it).
pub struct Evaluated {
    pub host: Box<ScriptVmHost<(), ()>>,
    pub bx: Option<Box<ScriptVmBase>>,
    pub errors: Vec<Diag>,
    /// The parser's warnings on the document: it runs, but likely not as written to
    /// (`[0 0 -30]` is two items).
    pub warnings: Vec<Diag>,
    pub ctx: Shared,
    /// The instructions the document ran, of [`EVAL_LIMIT`].
    pub instructions: u64,
}

impl Evaluated {
    pub fn with_vm<R>(&mut self, f: impl FnOnce(&mut ScriptVm) -> R) -> R {
        let bx = self.bx.take().expect("the model's VM is in use");
        let mut vm = ScriptVm { host: self.host.host_context(), bx };
        let out = f(&mut vm);
        self.bx = Some(vm.bx);
        out
    }
}

/// `file:line:col: message` as a location in the document and the message.
/// Other files keep their full location in the message, at document position 1:1.
pub fn diag_of(error: &str) -> Diag {
    let mut parts = error.splitn(4, ':');
    let (file, line, col, rest) = (parts.next(), parts.next(), parts.next(), parts.next());
    let strip = |message: &str| match message.rfind(" (platform/script/") {
        Some(at) if message.ends_with(')') => message[..at].to_string(),
        _ => message.to_string(),
    };
    match (file, line.and_then(|l| l.trim().parse::<u32>().ok()), col.and_then(|c| c.trim().parse::<u32>().ok()), rest) {
        (Some(file), Some(line), Some(col), Some(rest)) if file.ends_with(DOC_FILE) => Diag { line: line.max(1), col: col + 1, message: strip(rest.trim()) },
        _ => Diag { line: 1, col: 1, message: strip(error) },
    }
}

fn record_value(vm: &ScriptVm, value: ScriptValue, key: &str, active: &mut Vec<ScriptValue>) -> Result<RecordValue, Refusal> {
    if let Some(n) = value.as_number() {
        return Ok(RecordValue::Num(n));
    }
    if let Some(id) = value.as_id() {
        return Ok(RecordValue::Id(id_string(id)));
    }
    if value.is_string_like() {
        if let Some(text) = text_of(vm, value) {
            return Ok(RecordValue::Text(text));
        }
    }
    if let Some(flag) = value.as_bool() {
        return Ok(RecordValue::Bool(flag));
    }
    let refused = || Refusal { key: key.to_string(), message: "is not a number, id, text, flag, list or record".into() };
    // Cycles cannot be represented as plain values. Track ancestors, allowing shared children.
    if active.contains(&value) {
        return Err(refused());
    }
    let child_key = |field: &str| if key.is_empty() { field.to_string() } else { format!("{key}.{field}") };
    let heap = vm.bx.heap_ref();
    if let Some(array) = value.as_array() {
        active.push(value);
        let values = (0..heap.array_len(array)).map(|i| {
            record_value(vm, heap.array_index(array, i, NoTrap), &child_key(&i.to_string()), active)
        }).collect::<Result<Vec<_>, _>>();
        active.pop();
        return values.map(RecordValue::List);
    }
    if let Some(object) = value.as_object().filter(|_| p_is_options(heap, value)) {
        active.push(value);
        let fields = heap.map_entries(object).iter().map(|entry| {
            let field = text_of(vm, entry.key).unwrap_or_else(|| key_string(entry.key));
            record_value(vm, entry.value, &child_key(&field), active).map(|value| (field, value))
        }).collect::<Result<BTreeMap<_, _>, _>>();
        active.pop();
        return fields.map(RecordValue::Rec);
    }
    Err(refused())
}

fn refuse_input(vm: &mut ScriptVm, kind: &str, name: &str, refusal: Refusal) -> ScriptValue {
    let Refusal { key, message } = refusal;
    if key.is_empty() {
        script_err_invalid_args!(vm.trap(), "{kind}(\"{name}\", {{..}}): {message}")
    } else {
        script_err_invalid_args!(vm.trap(), "{kind}(\"{name}\", {{..}}): '{key}' {message}")
    }
}

/// A lazy input's value, built at its first read and evaluated here, in this VM, on a thread of
/// its own (the calling thread is paused mid-instruction, as a lazy module's registration runs),
/// then back; a holder roots it like the given values' container does.
fn eval_input(vm: &mut ScriptVm, c: &Shared, key: &(String, String), input: &Input) -> ScriptValue {
    let thread = vm.current_thread();
    vm.bx.select_first_unpaused_thread();
    let read = vm.eval(ScriptMod::new("", "", &format!("{}:{}", key.0, key.1), 1, 0, &input.source, &[]));
    vm.set_current_thread(thread);
    let holder = vm.bx.heap_mut().new_object();
    vm.bx.heap_mut().set_value_def(holder, (0.0f64).into(), read);
    let holder = vm.new_object_ref(holder);
    let mut ctx = c.borrow_mut();
    ctx.keep.push(holder);
    ctx.inputs.insert(key.clone(), read);
    read
}

/// Evaluate a document in a fresh VM with `mod.model`.
pub fn evaluate(source: &str, extension: &str, roots: &[RootFn], overrides: HashMap<(u32, u32), f64>, libraries: &[Library], inputs: &Inputs) -> Evaluated {
    let mut host = Box::new(ScriptVmHost::new((), ()));
    let bx = Box::new(ScriptVmBase::new());
    let ctx: Shared = std::rc::Rc::new(RefCell::new(Ctx { overrides, ..Default::default() }));
    let mut vm = ScriptVm { host: host.host_context(), bx };
    vm.set_script_domain("model");
    let module = vm.new_module(id!(model));
    // Value types (sketch shapes, segments, handles) are prototypes; parts only come from builders.
    for p in PROTOS.iter().filter(|p| !p.is_solid()) {
        let proto = vm.bx.heap_mut().new_object();
        let tag = lut_id(p.name);
        vm.bx.heap_mut().set_value_def(proto, id_lut!(__proto).into(), ScriptValue::from_id(tag));
        vm.bx.heap_mut().set_value_def(module, tag.into(), proto.into());
    }
    // The host's root calls live in its own module (`mod.fab`), or in `mod.model`.
    let ext = if extension.is_empty() { module } else { vm.new_module(lut_id(extension)) };
    install(&mut vm, module, ext, ctx.clone(), roots);
    vm.capture_errors();
    let lib = vm.eval(ScriptMod::new("", "", "texture.splash", 1, 0, TEXTURE_LIB, &[]));
    if let Some(o) = lib.as_object() {
        ctx.borrow_mut().texture = Some(vm.new_object_ref(o));
    }
    let mut library_errors = Vec::new();
    for lib in libraries {
        let value = vm.eval(ScriptMod::new("", "", &lib.file, 1, 0, &lib.source, &[]));
        if let Some(o) = value.as_object() {
            let fields: Vec<_> = vm.bx.heap_ref().map_entries(o).iter().map(|e| (e.key, e.value)).collect();
            for (key, value) in fields {
                vm.bx.heap_mut().set_value_def(ext, key, value);
            }
        } else {
            library_errors.push(Diag { line: 1, col: 1, message: format!("library {} must end with an object of its fns", lib.file) });
        }
    }
    // Root a container so every kind of input value survives later evaluations.
    if !inputs.values.is_empty() {
        let kept = vm.bx.heap_mut().new_object();
        ctx.borrow_mut().keep.push(vm.new_object_ref(kept));
        for (index, ((kind, name), input)) in inputs.values.iter().enumerate() {
            let file = format!("{kind}:{name}");
            let value = vm.eval(ScriptMod::new("", "", &file, 1, 0, &input.source, &[]));
            vm.bx.heap_mut().set_value_def(kept, (index as f64).into(), value);
            ctx.borrow_mut().inputs.insert((kind.clone(), name.clone()), value);
        }
    }
    for kind in &inputs.kinds {
        let c = ctx.clone();
        let kind = kind.clone();
        let lazy = inputs.lazy.clone();
        vm.add_method(ext, lut_id(&kind), script_args!(name = NIL, options = NIL), move |vm, args| {
            let name = arg(vm, args, id!(name));
            let Some(name) = name.is_string_like().then(|| text_of(vm, name)).flatten() else {
                return script_err_invalid_args!(vm.trap(), "{kind}(\"name\"): the name is text");
            };
            let options = arg(vm, args, id!(options));
            let record = if options.is_nil() {
                None
            } else if options.as_object().is_some() {
                match record_value(vm, options, "", &mut Vec::new()) {
                    Ok(record) => Some(record),
                    Err(refusal) => return refuse_input(vm, &kind, &name, refusal),
                }
            } else {
                return script_err_invalid_args!(vm.trap(), "{kind}(\"{name}\", {{..}}): the options are a record");
            };
            let key = (kind.clone(), input_key(&name, record.as_ref()));
            let mut value = c.borrow().inputs.get(&key).copied();
            if value.is_none() {
                if let Some(lazy) = &lazy {
                    let input = if let Some(record) = &record {
                        match lazy.provider.provide_with(&kind, &name, record) {
                            Ok(input) => input,
                            Err(refusal) => return refuse_input(vm, &kind, &name, refusal),
                        }
                    } else {
                        lazy.provider.provide(&kind, &name)
                    };
                    if let Some(input) = input {
                        value = Some(eval_input(vm, &c, &key, &input));
                    }
                }
            }
            if let Some(value) = value {
                let mut ctx = c.borrow_mut();
                if !ctx.inputs_read.contains(&key) {
                    ctx.inputs_read.push(key);
                }
                value
            } else {
                if record.is_none() {
                    if let Some(Refusal { key, message }) = lazy.as_ref().and_then(|l| l.provider.refused(&kind, &name)) {
                        if key.is_empty() {
                            return script_err_invalid_args!(vm.trap(), "{kind}(\"{name}\"): {message}");
                        }
                        return script_err_invalid_args!(vm.trap(), "{kind}(\"{name}\"): '{key}' {message}");
                    }
                }
                let ctx = c.borrow();
                let mut names: Vec<String> = ctx.inputs.keys().filter(|(k, _)| k == &kind).map(|(_, n)| n.clone()).collect();
                names.extend(lazy.as_ref().map(|l| l.provider.names(&kind)).unwrap_or_default());
                names.sort();
                names.dedup();
                let available = if names.is_empty() { "there is none".to_string() } else { format!("there is: {}", names.join(", ")) };
                script_err_invalid_args!(vm.trap(), "{kind}: no {kind} named '{name}' ({available})")
            }
        });
    }
    let mut errors: Vec<_> = vm.take_errors().iter().map(|e| diag_of(e)).collect();
    errors.extend(library_errors);
    // A broken environment cannot run the document; keep its original diagnostics.
    let instructions = if errors.is_empty() {
        vm.capture_errors();
        // Rows count from 1 like an editor's lines; columns from 0.
        vm.with_instruction_limit(EVAL_LIMIT as u64, |vm| vm.eval(ScriptMod::new("", "", &(DOC_FILE), 1, 0, &(source), &[])));
        vm.last_limit_consumed()
    } else {
        0
    };
    errors.extend(vm.take_errors().iter().map(|e| diag_of(e)).map(|mut d| {
        // An `m.<name>` call that is no builder: what Fab calls it.
        if let Some(name) = d.message.strip_prefix("method ").and_then(|m| m.strip_suffix(" not found on object")) {
            d.message = format!("{}: {}", d.message, protos::unknown_call_hint(name));
        }
        if d.message.starts_with("script instruction limit exceeded") {
            d.message = format!("{}: the script ran all of its {EVAL_LIMIT} instructions (a model of a few hundred parts built in loops runs well under 1% of them), most likely in a fn that calls itself or a loop that never ends", d.message);
        }
        d
    }));
    // Placed as the runtime's errors are ([`loc_of_ip`]).
    let warnings = vm.take_warning_records().iter().filter_map(|w| {
        let loc = w.loc().filter(|loc| loc.file().ends_with(DOC_FILE))?;
        Some(Diag { line: loc.line().max(1), col: loc.col() + 1, message: w.message().to_string() })
    }).collect();
    let bx = vm.bx;
    Evaluated { host, bx: Some(bx), errors, warnings, ctx, instructions }
}

/// Reads VM values into [`V`]s.
pub struct Reader<'a, 'b> {
    pub vm: &'a mut ScriptVm<'b>,
    seen: HashMap<ScriptObject, Arc<Obj>>,
    next: usize,
    /// The builders' part objects, by part index.
    parts: Vec<ScriptObject>,
}

impl<'a, 'b> Reader<'a, 'b> {
    pub fn new(vm: &'a mut ScriptVm<'b>, parts: Vec<ScriptObject>) -> Self {
        Reader { vm, seen: HashMap::new(), next: 0, parts }
    }

    /// Part `index` as an object.
    pub fn part(&mut self, index: usize) -> V {
        let o = self.parts[index];
        self.read_part(o, index)
    }

    fn read_part(&mut self, o: ScriptObject, index: usize) -> V {
        match self.read_object(o, 0) {
            V::Obj(obj) if obj.part.is_none() => {
                let obj = Arc::new(Obj { ident: obj.ident, part: Some(index), proto: obj.proto.clone(), fields: obj.fields.clone(), items: obj.items.clone(), loc: obj.loc });
                self.seen.insert(o, obj.clone());
                V::Obj(obj)
            }
            v => v,
        }
    }

    fn loc_of(&self, obj: ScriptObject) -> Option<(u32, u32)> {
        loc_of_ip(self.vm, self.vm.bx.heap_ref().made_at(obj))
    }

    pub fn read(&mut self, value: ScriptValue) -> V {
        self.read_depth(value, 0)
    }

    fn read_depth(&mut self, value: ScriptValue, depth: usize) -> V {
        if value.is_nil() {
            return V::Nil;
        }
        if let Some(b) = value.as_bool() {
            return V::Bool(b);
        }
        if let Some(c) = value.as_color() {
            return V::Color(c);
        }
        if let Some(n) = value.as_number() {
            return V::Num(n);
        }
        if let Some(id) = value.as_id() {
            return V::Id(id_string(id));
        }
        // vec2 / vec3 / vec4 values read as lists of numbers.
        if value.as_pod().is_some() {
            use makepad_script::numeric::{NumericValue as N};
            let ip = self.vm.trap_ip();
            let nums = |v: &[f32]| V::Arr(v.iter().map(|x| V::Num(*x as f64)).collect());
            match N::from_script_value_heap(self.vm.heap(), value, ip) {
                N::Vec2(v) => return nums(&[v.x, v.y]),
                N::Vec3(v) => return nums(&[v.x, v.y, v.z]),
                N::Vec4(v) => return nums(&[v.x, v.y, v.z, v.w]),
                _ => {}
            }
        }
        if value.is_string_like() {
            if let Some(s) = text_of(&*self.vm, value) {
                return V::Str(s);
            }
        }
        if depth >= MAX_DEPTH {
            return V::Other("nesting too deep".into());
        }
        if let Some(array) = value.as_array() {
            let len = self.vm.bx.heap_ref().array_len(array);
            return V::Arr((0..len).map(|i| {
                let item = self.vm.bx.heap_ref().array_index(array, i, NoTrap);
                self.read_depth(item, depth + 1)
            }).collect());
        }
        if let Some(object) = value.as_object() {
            if self.vm.bx.heap_ref().is_fn(object) {
                return V::Fn(value);
            }
            // A part's handle reads as its part.
            let heap = self.vm.bx.heap_ref();
            if heap.value(object, id_lut!(__proto).into(), NoTrap).as_id() == Some(id_lut!(Part)) {
                let index = heap.value(object, id_lut!(part).into(), NoTrap).as_number().map(|n| n as usize);
                if let Some(i) = index.filter(|i| *i < self.parts.len()) {
                    return self.part(i);
                }
            }
            return self.read_object(object, depth);
        }
        V::Other(format!("{:?}", value.value_type()))
    }

    fn read_object(&mut self, object: ScriptObject, depth: usize) -> V {
        if let Some(o) = self.seen.get(&object) {
            return V::Obj(o.clone());
        }
        let mut chain = Vec::new();
        let mut at = Some(object);
        while let Some(obj) = at {
            if chain.len() > MAX_DEPTH {
                break;
            }
            chain.push(obj);
            at = self.vm.bx.heap_ref().proto(obj).as_object();
        }
        let mut proto = None;
        let mut raw: Vec<(String, ScriptValue)> = Vec::new();
        let mut items = Vec::new();
        for (level, obj) in chain.iter().enumerate() {
            let heap = self.vm.heap();
            let mut fields: Vec<(String, ScriptValue)> = Vec::new();
            heap.map_entries(*obj).iter().for_each(|e| fields.push((key_string(e.key), e.value)));
            for index in 0..heap.vec_len(*obj) {
                let entry = heap.vec_key_value(*obj, index, NoTrap);
                if entry.key.is_nil() {
                    if level == 0 {
                        items.push(entry.value);
                    }
                } else {
                    fields.push((key_string(entry.key), entry.value));
                }
            }
            for (key, value) in fields {
                if key == "__proto" {
                    if proto.is_none() {
                        proto = value.as_id().map(id_string);
                    }
                    continue;
                }
                // A prototype's own fields (the module's) are not the object's settings.
                if level > 0 && proto.is_some() {
                    continue;
                }
                if !raw.iter().any(|(k, _)| *k == key) {
                    raw.push((key, value));
                }
            }
        }
        if raw.is_empty() && proto.is_none() && !items.is_empty() {
            return V::Arr(items.into_iter().map(|v| self.read_depth(v, depth + 1)).collect());
        }
        let loc = self.loc_of(object);
        let ident = self.next;
        self.next += 1;
        let fields: Vec<(String, V)> = raw.into_iter().map(|(key, value)| (key, self.read_depth(value, depth + 1))).collect();
        let items: Vec<V> = items.into_iter().map(|v| self.read_depth(v, depth + 1)).collect();
        let obj = Arc::new(Obj { ident, part: None, proto, fields, items, loc });
        self.seen.insert(object, obj.clone());
        V::Obj(obj)
    }
}

fn key_string(key: ScriptValue) -> String {
    if let Some(id) = key.as_id() {
        return id_string(id);
    }
    if let Some(n) = key.as_number() {
        return format!("{n}");
    }
    if let Some(s) = key.as_inline_string(|s| s.to_string()) {
        return s;
    }
    format!("{key:?}")
}

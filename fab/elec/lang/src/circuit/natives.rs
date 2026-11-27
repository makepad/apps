//! `mod.elec`: the circuit builders (`e.`), part drawing (`f.`), `param` and `netclass`.
//!
//! Every builder reads its arguments into owned values, records what it makes with where it was
//! called (`Here`), and returns a handle: a small object `{__proto: @Comp ix: 3 ..}` the later
//! builders take. A mistake is a located diagnostic naming the item's path; the builder returns
//! nil and evaluation goes on, so one run reports every independent mistake.

use super::names;
use crate::map::{Input, Origin, PinSite, SourceRange, ValueSite};
use crate::resolve::{self, Leaf, LeafKind};
use crate::value_site::{tol_arg, tolerance_percent, two_pin_net_arg, ValueArg};
use super::read::{arg, field, loc_of_ip, lut_id, reported_error, Handle, Obj, Reader, V};
use super::*;
use crate::units::value_text;
use fab_elec_circuit::{
    value_unit, BlockDefId, BlockSheet, Edge, Flow, PortDef, PortKind, PortSide, QtyUnit, Quantity, ViaSize,
};
use fab_elec_core::{Angle, Contour, LayerSet, Nm, Path, PolySet, Shape, Side, Xform3, P};
use fab_elec_parts::{Body3d, BodyColour, BodyPort, BodyPortKind, BodySource, BodySurface, Emitter, EmitterFace, FpMount, GpioCard, Lifecycle, Light, Limits, PackageKind, PadKind, PadShape, PartKind, PinDef, Tol};
use fab_materials::{Colour, FinishId, MaterialId, Surface};

type Native = Box<dyn Fn(&mut ScriptVm, ScriptObject) -> ScriptValue>;

/// Install `mod.elec` and `mod.elec.f` in `vm`, recording into `ctx`.
pub(crate) fn install(vm: &mut ScriptVm, ctx: &Shared) {
    let module = vm.new_module(id_lut!(elec));
    vm.bx.heap_mut().set_value_def(module, id_lut!(__build).into(), 0.0.into());
    let f = vm.bx.heap_mut().new_object();
    vm.bx.heap_mut().set_value_def(module, id_lut!(f).into(), f.into());
    let holder = vm.bx.heap_mut().new_object();
    let r = vm.new_object_ref(holder);
    {
        let mut c = ctx.borrow_mut();
        c.holder = Some(holder);
        c.keep.push(r);
    }
    for (name, args, make) in builders() {
        let c = ctx.clone();
        add(vm, module, name, args, make(c));
    }
    for (name, args, make) in drawing() {
        let c = ctx.clone();
        add(vm, f, name, args, make(c));
    }
}

type Make = fn(Shared) -> Native;

fn add(vm: &mut ScriptVm, on: ScriptObject, name: &str, args: &[&str], f: Native) {
    let defaults: Vec<(LiveId, ScriptValue)> = args.iter().map(|a| (lut_id(a), NIL)).collect();
    let keys: Vec<_> = defaults.iter().map(|(key, _)| *key).collect();
    vm.add_method(on, lut_id(name), &defaults, move |vm, args| {
        for key in &keys {
            let value = vm.bx.heap_ref().value(args, (*key).into(), NoTrap);
            if let Some(error) = reported_error(vm, value) {
                return error;
            }
        }
        f(vm, args)
    });
}

fn builders() -> Vec<(&'static str, &'static [&'static str], Make)> {
    vec![
        ("define", &["name", "spec"], define),
        ("body", &["part", "spec"], body),
        ("lookup", &["name"], lookup),
        ("part", &["part", "opts"], part),
        ("r", &["value", "a", "b", "opts", "rest"], |c| two_pin(c, "R", QtyUnit::Ohm)),
        ("c", &["value", "a", "b", "opts", "rest"], |c| two_pin(c, "C", QtyUnit::Farad)),
        ("led", &["opts", "a", "k"], led),
        ("series", &["value", "series"], series),
        ("connect", &["a", "b"], connect),
        ("net", &["opts"], net),
        ("bus", &["name", "members"], bus),
        ("block", &["spec"], block),
        ("port", &["kind", "opts"], port),
        ("near", &["a", "b", "max"], near),
        ("side", &["t", "side"], side),
        ("fixed", &["t", "opts"], fixed),
        ("width", &["net", "w"], width),
        ("pair", &["p", "n", "opts"], pair),
        ("match_length", &["nets", "opts"], match_length),
        ("group", &["name", "members"], group),
        ("param", &["value", "min", "max", "step", "label"], param),
        ("netclass", &["name", "rules"], netclass),
        ("sheet", &["table"], sheet),
    ]
}

fn drawing() -> Vec<(&'static str, &'static [&'static str], Make)> {
    vec![
        ("smd", &["no", "at", "size", "opts"], smd),
        ("thru", &["no", "at", "d", "drill", "opts"], thru),
        ("footprint", &["spec"], footprint),
        ("stroke", &["layer", "points", "w"], stroke),
        ("dot", &["layer", "at", "r"], dot),
        ("ic_symbol", &["name", "sides"], ic_symbol),
        ("two_terminal", &["pins"], two_terminal),
        ("opamp_symbol", &["pins"], opamp_symbol),
        ("int", &["n"], int),
        ("mm", &["v"], mm_text),
        ("round", &["v", "step"], round),
        ("error", &["message"], error),
    ]
}

fn sheet(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let mut c = c.borrow_mut();
        let h = here(vm, &c);
        if c.docs.iter().any(|d| d.id == h.doc && d.lib) {
            return fail(&mut c, &h, "sheet hints belong in the circuit document".into());
        }
        if c.build.sheet_seen {
            return fail(&mut c, &h, "a circuit document may have only one sheet call".into());
        }
        c.build.sheet_seen = true;
        let table = read(vm, args, "table");
        let mut written = None;
        let mut source = "";
        if let Some(names) = c.docs.iter().find(|d| d.id == h.doc).and_then(DocInfo::names) {
            source = names.syntax.source();
            names.syntax.walk(&mut |_, node| {
                if node.span.start == h.span.start as usize && node.span.end == h.span.end as usize {
                    if let syntax::Kind::Call { args, .. } = &node.kind {
                        written = args.first();
                    }
                }
            });
        }
        match super::hints::read(&table, written, source, &h) {
            Ok((hints, keys)) => {
                c.build.hints = hints;
                c.build.hint_keys = keys;
                NIL
            }
            Err((at, message)) => fail(&mut c, &at, message),
        }
    })
}

// ---- Places, paths and errors -------------------------------------------------------------------

/// Where the running builder was called, with the item's own path from the `let`s and loops of
/// every frame of the running thread (outermost first).
fn here(vm: &ScriptVm, c: &Ctx) -> Here {
    let mut ips = Vec::new();
    vm.call_sites(&mut ips, MAX_FRAMES);
    let mut positions = Vec::new();
    vm.loop_positions(&mut positions);
    let frames: Vec<(usize, (u32, u32))> = ips.iter().rev().filter_map(|ip| {
        let (file, line, col) = loc_of_ip(vm, *ip)?;
        Some((c.doc_by_file(&file)?, (line, col)))
    }).collect();
    let mut segs = Vec::new();
    let mut used = 0usize;
    let mut out = Here { doc: DocId(0), span: 0..0, callee: None, path: None, loops: Vec::new() };
    for (k, (d, loc)) in frames.iter().enumerate() {
        let info = &c.docs[*d];
        let last = k + 1 == frames.len();
        if last {
            out.doc = info.id;
        }
        let Some(names) = info.names() else { continue };
        let Some(site) = names.site(Some(*loc)).or_else(|| names.call_site(*loc)) else { continue };
        let end = (used + site.loops as usize).min(positions.len());
        // A binding names what its own call makes, not what an argument call makes.
        if let Some(s) = names::path(&site.binds, &positions[used..end]).filter(|_| site.owns) {
            segs.push(s);
        }
        if last {
            out.span = site.span.start as u32..site.span.end as u32;
            out.callee = site.callee;
            out.loops = resolve::loop_positions(&names.syntax, &out.span, &positions);
        }
        used = end;
    }
    if !segs.is_empty() {
        out.path = Some(segs.join("."));
    }
    out
}

/// The innermost frame in a project document (where a library generator was reached from).
fn here_in_project(vm: &ScriptVm, c: &Ctx) -> Here {
    let mut ips = Vec::new();
    vm.call_sites(&mut ips, MAX_FRAMES);
    for ip in &ips {
        let Some((file, line, col)) = loc_of_ip(vm, *ip) else { continue };
        let Some(d) = c.doc_by_file(&file) else { continue };
        let info = &c.docs[d];
        if info.lib {
            continue;
        }
        let span = info.names().and_then(|n| n.node_span((line, col)).or_else(|| n.offset((line, col)).map(|o| o..o + 1))).unwrap_or(0..0);
        return Here { doc: info.id, span: span.start as u32..span.end as u32, callee: None, path: None, loops: Vec::new() };
    }
    here(vm, c)
}

/// An item's full path: its instance's path, then its own name, else its builder and a count.
fn item_path(c: &mut Ctx, h: &Here, callee: &str) -> String {
    let inst = c.build.inst();
    let own = match &h.path {
        Some(p) => p.clone(),
        None => {
            let n = c.build.unnamed.entry((inst, callee.to_string())).or_insert(0);
            *n += 1;
            format!("{callee}.{n}")
        }
    };
    let prefix = &c.build.insts[inst].path;
    if prefix.is_empty() { own } else { format!("{prefix}.{own}") }
}

fn fail(c: &mut Ctx, h: &Here, message: String) -> ScriptValue {
    c.diags.push(located(h.doc, h.span.clone(), message));
    NIL
}

fn read(vm: &mut ScriptVm, args: ScriptObject, key: &str) -> V {
    let v = arg(vm, args, lut_id(key));
    Reader::new(vm).read(v)
}

fn handle(vm: &mut ScriptVm, tag: LiveId, ix: usize) -> ScriptObject {
    let build = field(vm, vm.bx.heap_ref().module(id_lut!(elec)), id_lut!(__build));
    let heap = vm.bx.heap_mut();
    let o = heap.new_object();
    heap.set_value_def(o, id_lut!(__proto).into(), ScriptValue::from_id(tag));
    heap.set_value_def(o, id_lut!(ix).into(), (ix as f64).into());
    if [id_lut!(Comp), id_lut!(Pin), id_lut!(Net), id_lut!(Bus), id_lut!(Inst)].contains(&tag) {
        heap.set_value_def(o, id_lut!(build).into(), build);
    }
    o
}

/// A component's handle: its pins by name (`j.CC1`), and the LED's forward voltage (`d.vf`).
fn comp_handle(vm: &mut ScriptVm, c: &Ctx, comp: usize, vf: Option<f64>) -> ScriptValue {
    let o = handle(vm, id_lut!(Comp), comp);
    let pins: Vec<String> = c.parts[c.build.comps[comp].part].def.pins.iter().map(|p| p.name.clone()).collect();
    for (i, name) in pins.iter().enumerate() {
        let pin = handle(vm, id_lut!(Pin), comp);
        let heap = vm.bx.heap_mut();
        heap.set_value_def(pin, id_lut!(pin).into(), (i as f64).into());
        heap.set_value_def(o, lut_id(name).into(), pin.into());
    }
    if let Some(vf) = vf {
        vm.bx.heap_mut().set_value_def(o, id_lut!(vf).into(), vf.into());
    }
    o.into()
}

pub(crate) fn net_handle(vm: &mut ScriptVm, c: &Ctx, n: usize) -> ScriptValue {
    let o = handle(vm, id_lut!(Net), n);
    let rec = &c.build.nets[n];
    let name = vm.new_string(&rec.name);
    vm.bx.heap_mut().set_value_def(o, id_lut!(name).into(), name);
    if let Some(v) = rec.volts {
        let volts: ScriptValue = if v.min == v.max {
            v.min.into()
        } else {
            let heap = vm.bx.heap_mut();
            let a = heap.new_array();
            heap.array_push(a, v.min.into(), NoTrap);
            heap.array_push(a, v.max.into(), NoTrap);
            a.into()
        };
        vm.bx.heap_mut().set_value_def(o, id_lut!(volts).into(), volts);
    }
    o.into()
}

fn nm(metres: f64) -> i32 {
    (metres * 1e9).round() as i32
}

fn finite_nm(v: &V) -> Option<Nm> {
    let n = (v.num()? * 1e9).round();
    (n.is_finite() && n >= Nm::MIN as f64 && n <= Nm::MAX as f64).then_some(n as Nm)
}

fn point(v: &V) -> Option<P> {
    let l = v.list()?;
    if l.len() != 2 {
        return None;
    }
    Some(P::new(nm(l[0].num()?), nm(l[1].num()?)))
}

fn volts(v: &V) -> Option<VRange> {
    match v {
        V::Num(x) => Some(VRange::fixed(*x)),
        V::Arr(l) if l.len() == 2 => Some(VRange { min: l[0].num()?, max: l[1].num()? }),
        _ => None,
    }
}

/// Check an options object's field names.
fn check_opts(o: &Obj, allowed: &[&str]) -> Result<(), String> {
    if let Some(v) = o.items.first() {
        return Err(format!("{} has no name (a key is missing: `name: value`)", v.describe()));
    }
    match o.fields.iter().find(|(k, _)| !allowed.contains(&k.as_str())) {
        Some((k, _)) => Err(format!("unknown option {k} (the options are {})", allowed.join(", "))),
        None => Ok(()),
    }
}

/// Locate an argument's nested field value, falling back to the nearest written expression.
fn option_here(c: &Ctx, h: &Here, arg: usize, keys: &[&str]) -> Here {
    let mut at = h.clone();
    let Some(names) = c.docs.iter().find(|d| d.id == h.doc).and_then(DocInfo::names) else { return at };
    let Some(call) = crate::value_site::call_at(&names.syntax, &(h.span.start as usize..h.span.end as usize)) else { return at };
    let Some((callee, args)) = call.as_call() else { return at };
    let node = match callee.rsplit('.').next() {
        Some("part" | "r" | "c" | "led") => comp_options_arg(args, arg).map(|(_, node)| node),
        _ => args.get(arg),
    };
    let Some(mut node) = node else { return at };
    if let syntax::Kind::Entry { value, .. } = &node.kind {
        node = value;
    }
    for key in keys {
        let Some(field) = node.field(key) else { break };
        node = &field.value;
    }
    at.span = node.span.start as u32..node.span.end as u32;
    at
}

fn check_opts_at(o: &Obj, allowed: &[&str], c: &Ctx, h: &Here, arg: usize, keys: &[&str], at: &mut Here) -> Result<(), String> {
    if let Err(message) = check_opts(o, allowed) {
        let mut path = keys.to_vec();
        if let Some((key, _)) = o.fields.iter().find(|(k, _)| !allowed.contains(&k.as_str())) {
            path.push(key);
        }
        *at = option_here(c, h, arg, &path);
        return Err(message);
    }
    Ok(())
}

fn opts_of(v: &V) -> Result<Option<&Obj>, String> {
    match v {
        V::Nil => Ok(None),
        V::Obj(o) => Ok(Some(o)),
        other => Err(format!("expected an options object, got {}", other.describe())),
    }
}

/// A component's own text fields, `fields: {key: "text"}` in the options that are argument `arg`
/// of the call at `h` (the shape of a part definition's `fields:`), added to `fields`. A mistake is
/// located at the field it names; a key `fields` already has (a passive's `volts:`) is refused.
fn comp_fields(c: &Ctx, h: &Here, arg: usize, o: &Obj, fields: &mut Fields) -> Result<(), (Here, String)> {
    let Some(v) = o.get("fields") else { return Ok(()) };
    let whole = || option_here(c, h, arg, &["fields"]);
    let f = v.obj().ok_or_else(|| (whole(), format!("fields: expected an object, got {}", v.describe())))?;
    if !f.items.is_empty() {
        return Err((whole(), "fields: every string needs a key".into()));
    }
    for (key, value) in &f.fields {
        let at = || option_here(c, h, arg, &["fields", key]);
        let text = value.text().ok_or_else(|| {
            let got = value.num().map_or_else(|| value.describe(), |n| n.to_string());
            (at(), format!("fields: {key}: a string, got {got}"))
        })?;
        if fields.get(key).is_some() {
            return Err((at(), format!("fields: {key} is already given by the {key}: option")));
        }
        fields.0.push((key.clone(), text.to_string()));
    }
    Ok(())
}

/// Read the call's fields, stored reference and optional `added` order, locating mistakes at
/// the written value. Editor add paths write `added` so the part is numbered after the count,
/// in this order, and adding it renumbers no other part.
fn comp_options(c: &Ctx, h: &Here, arg: usize, opts: &V, fields: &mut Fields) -> Result<(Option<String>, Option<u32>, bool), (Here, String)> {
    let Some(o) = opts.obj() else { return Ok((None, None, false)) };
    comp_fields(c, h, arg, o, fields)?;
    let refdes = match o.get("ref") {
        None => None,
        Some(value) => Some(value.text().filter(|text| valid_ref(text)).ok_or_else(|| (
            option_here(c, h, arg, &["ref"]), format!("ref: expected text like \"R12\", got {}", value.describe()),
        ))?.to_string()),
    };
    let added = match o.get("added") {
        None => None,
        Some(value) => Some(value.num().filter(|n| n.fract() == 0.0 && *n > 0.0 && *n <= f64::from(u32::MAX)).ok_or_else(|| (
            option_here(c, h, arg, &["added"]), format!("added: expected a positive whole number, got {}", shown(value)),
        ))? as u32),
    };
    let dnp = match o.get("dnp") {
        None => false,
        Some(v) => v.bool().ok_or_else(|| (option_here(c, h, arg, &["dnp"]), format!("dnp: expected true or false, got {}", v.describe())))?,
    };
    Ok((refdes, added, dnp))
}

fn pin_kind(name: &str) -> Option<PinKind> {
    Some(match name {
        "in" => PinKind::In,
        "out" => PinKind::Out,
        "bidi" => PinKind::Bidi,
        "tri_state" => PinKind::TriState,
        "passive" => PinKind::Passive,
        "power_in" => PinKind::PowerIn,
        "power_out" => PinKind::PowerOut,
        "open_collector" => PinKind::OpenCollector,
        "open_emitter" => PinKind::OpenEmitter,
        "no_connect" => PinKind::NoConnect,
        "free" => PinKind::Free,
        "unspecified" => PinKind::Unspecified,
        _ => return None,
    })
}

/// A pad number written as a number (`1`) or a string (`"A12"`).
fn pad_no(v: &V) -> Option<String> {
    match v {
        V::Num(x) if x.fract() == 0.0 && *x >= 0.0 => Some(format!("{}", *x as u64)),
        V::Str(s) => Some(s.clone()),
        _ => None,
    }
}

// ---- Connections --------------------------------------------------------------------------------

/// Put pin `pin` of `comp` on what `v` names: a net, or another pin's net; `nil` leaves it
/// unconnected (a list or positional pin keeps its place: what `Disconnect` writes).
fn connect_pin(c: &mut Ctx, h: &Here, comp: usize, pin: PinIx, v: &V) -> Result<(), String> {
    if matches!(v, V::Nil) {
        return Ok(());
    }
    match v.handle() {
        Some(Handle::Net(n)) => {
            c.build.attach(comp, pin, n, h);
            Ok(())
        }
        Some(Handle::Pin(other, p)) => {
            let n = c.net_of_pin(other, PinIx(p));
            c.build.attach(comp, pin, n, h);
            Ok(())
        }
        _ => Err(format!("is given {}, not a net or a pin", v.describe())),
    }
}

/// The nets a value binds a port to: a net, a pin's net, a bus or a list of nets.
fn port_nets(c: &mut Ctx, v: &V) -> Result<Vec<usize>, String> {
    match v {
        V::Handle(Handle::Net(n)) => Ok(vec![c.build.find(*n)]),
        V::Handle(Handle::Pin(comp, p)) => Ok(vec![c.net_of_pin(*comp, PinIx(*p))]),
        V::Handle(Handle::Bus(b)) => Ok(c.build.buses[*b].members.iter().map(|(_, n)| *n).collect()),
        V::Arr(items) => items.iter().map(|i| port_nets(c, i).map(|v| v[0])).collect(),
        other => Err(format!("is given {}, not a net, a pin or a bus", other.describe())),
    }
}

/// A new component of part `part` made at `h`.
fn new_comp(c: &mut Ctx, h: &Here, callee: &str, part: usize, value: Option<Value>, fields: Fields, (refdes, added, dnp): (Option<String>, Option<u32>, bool)) -> usize {
    let path = item_path(c, h, callee);
    let inst = c.build.inst();
    c.build.comps.push(CompRec { path, part, value, refdes, added, dnp, conns: vec![], nc: vec![], inst, fields, here: h.clone(), site: None, pin_sites: vec![] });
    let comp = c.build.comps.len() - 1;
    c.build.comps[comp].site = value_site_at(c, h, comp);
    comp
}

/// A pin site at the call `h`: `site` of the call's range, or the library it is written in.
fn pin_site(c: &Ctx, h: &Here, site: impl FnOnce(SourceRange) -> PinSite) -> PinSite {
    match c.docs.iter().find(|d| d.id == h.doc) {
        Some(d) if d.lib => PinSite::Library { lib: d.file.split('/').next().unwrap_or("").to_string() },
        _ => site(SourceRange { doc: h.doc, span: h.span.clone() }),
    }
}

/// Where the value of component `comp`, made by the call at `h`, is written (architect change 3):
/// a literal, a block parameter (overridden at the instance's call), a library or an expression.
fn value_site_at(c: &Ctx, h: &Here, comp: usize) -> Option<ValueSite> {
    let info = c.docs.iter().find(|d| d.id == h.doc)?;
    let syntax = &info.names()?.syntax;
    let call = h.span.start as usize..h.span.end as usize;
    let lib_of = |d: &DocInfo| d.file.split('/').next().unwrap_or("").to_string();
    let range = |doc: DocId, r: Range<usize>| SourceRange { doc, span: r.start as u32..r.end as u32 };
    let literal = |r: Range<usize>| match info.lib {
        true => ValueSite::Library { lib: lib_of(info) },
        false => ValueSite::Literal { at: range(h.doc, r), count: 0 },
    };
    let computed = |r: Range<usize>, inputs: Vec<Input>| match info.lib {
        true => ValueSite::Library { lib: lib_of(info) },
        false => ValueSite::Derived {
            at: range(h.doc, r),
            leaf: None,
            inputs,
            siblings: 0,
        },
    };
    let param = |name: String, item: Option<usize>| -> ValueSite {
        let inst = &c.build.insts[c.build.comps[comp].inst];
        let (Some(ih), Some(rec)) = (&inst.here, inst.rec) else { return computed(call.clone(), vec![]) };
        match c.docs.iter().find(|d| d.id == ih.doc) {
            Some(d) if !d.lib => {}
            Some(d) => return ValueSite::Library { lib: lib_of(d) },
            None => return computed(call.clone(), vec![]),
        }
        let dh = &c.defs[rec].here;
        let default = c.docs.iter().find(|d| d.id == dh.doc).and_then(|d| d.names()).and_then(|n| {
            crate::value_site::param_default(&n.syntax, dh.span.start as usize..dh.span.end as usize, &name)
        });
        ValueSite::Param { name, item, call: SourceRange { doc: ih.doc, span: ih.span.clone() }, default: default.map(|r| range(dh.doc, r)), count: 0 }
    };
    let leaf_site = |leaf: &Leaf| match &leaf.kind {
        LeafKind::Number => literal(leaf.at.clone()),
        LeafKind::Param { name, item } => param(name.clone(), *item),
    };
    Some(match crate::value_site::value_arg(syntax, call.clone())? {
        ValueArg::Literal(r) => literal(r),
        ValueArg::Param { name, .. } => param(name, None),
        ValueArg::Expr(r) => {
            let key = (h.doc, h.span.clone(), h.loops.clone());
            let cached = c.resolved.borrow().get(&key).cloned();
            let resolved = cached.unwrap_or_else(|| {
                let origin = Origin { call: SourceRange { doc: h.doc, span: h.span.clone() }, loops: h.loops.clone() };
                let mut expr = None;
                syntax.walk(&mut |_, node| {
                    if node.span == r {
                        expr = Some(node);
                    }
                });
                let out = expr.map(|expr| (resolve::resolve(syntax, expr, &origin), resolve::inputs(syntax, expr, &origin)));
                c.resolved.borrow_mut().insert(key, out.clone());
                out
            });
            let Some((term, leaves)) = resolved else { return Some(computed(r, vec![])) };
            let unit = c.build.comps[comp].value.as_ref().and_then(|v| v.qty).map(|q| q.unit);
            if let Some((leaf, scale)) = term.input() {
                if scale == 1.0 && (leaf.kind != LeafKind::Number || leaf.unit.is_none() || leaf.unit == unit) {
                    return Some(leaf_site(leaf));
                }
            }
            let inputs = leaves.into_iter().map(|leaf| Input {
                at: range(h.doc, leaf.at.clone()),
                value: (leaf.kind == LeafKind::Number).then_some(leaf.value),
                site: leaf_site(&leaf),
            }).collect();
            computed(r, inputs)
        }
    })
}

/// Call a generator of `mod.std` (`chip`, `led_part`) and read the part it returns.
fn std_part(vm: &mut ScriptVm, name: &str, args: &[ScriptValue]) -> Result<usize, String> {
    let std = vm.module(id_lut!(std));
    let gen = field(vm, std, lut_id(name));
    if gen.is_nil() {
        return Err(format!("the std library has no generator {name} (is std installed?)"));
    }
    let out = vm.call(gen, args);
    match Reader::new(vm).read(out).handle() {
        Some(Handle::Part(p)) => Ok(p),
        _ => Err(format!("std {name} made no part")),
    }
}

// ---- e.: parts and components -------------------------------------------------------------------

fn define(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let name = read(vm, args, "name");
        let spec = read(vm, args, "spec");
        let mut cx = c.borrow_mut();
        let h = here(vm, &cx);
        let Some(name) = name.text().map(str::to_string) else {
            return fail(&mut cx, &h, format!("e.define: the part name must be a string, got {}", name.describe()));
        };
        let mut at = h.clone();
        match define_part(&mut cx, &name, &spec, &h, &mut at) {
            Ok(ix) => {
                drop(cx);
                handle(vm, id_lut!(Part), ix).into()
            }
            Err(m) => fail(&mut cx, &at, format!("{name}: {m}")),
        }
    })
}

/// Replace only a part's body, now or when its name is defined; return its handle if defined.
fn body(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let which = read(vm, args, "part");
        let spec = read(vm, args, "spec");
        let mut c = c.borrow_mut();
        let h = here(vm, &c);
        if c.docs.iter().any(|d| d.id == h.doc && d.lib) {
            return fail(&mut c, &h, "e.body belongs in a project document; a library gives its part a body in e.define".into());
        }
        let name = match &which {
            V::Handle(Handle::Part(ix)) => c.parts[*ix].def.name.clone(),
            V::Str(name) => name.clone(),
            other => return fail(&mut c, &h, format!("e.body: expected a part or a part name, got {}", other.describe())),
        };
        if c.bodies.iter().any(|b| b.name == name) {
            return fail(&mut c, &h, format!("{name}: e.body is given already (at the first call)"));
        }
        let mut at = h.clone();
        let spec = match body_of(&spec, &c, &h, &[], &mut at, false) {
            Ok(spec) => spec,
            Err(message) => return fail(&mut c, &at, format!("{name}: body: {message}")),
        };
        if let Some((path, named)) = &spec.missing {
            let message = format!("{name}: body: mesh {path} is not in the project: the part keeps its own body until the project holds it");
            c.warnings.push(warning(named, message));
        }
        let part = c.part_names.get(&name).copied();
        let BodySpec { body, height, ports, missing } = spec;
        c.bodies.push(BodyOverride { name, body, height, ports, missing: missing.is_some(), here: h, applied: false, own: None });
        if let Some(ix) = part {
            let old = c.parts[ix].def.clone();
            c.parts[ix].def = apply_body_override(&mut c, old);
            drop(c);
            handle(vm, id_lut!(Part), ix).into()
        } else {
            NIL
        }
    })
}

/// Apply `def`'s pending `e.body` override. What the override does not state (its height, its
/// ports) stays the part's own; an override whose mesh the project does not hold leaves the
/// part's own body and only records the mesh as missing on it.
fn apply_body_override(c: &mut Ctx, def: Arc<PartDef>) -> Arc<PartDef> {
    let Some(ix) = c.bodies.iter().position(|b| b.name == def.name && !b.applied) else { return def };
    let o = &c.bodies[ix];
    let (doc, span) = (o.here.doc, o.here.span.clone());
    let body = match def.body.as_ref() {
        // Keep the part's own body, marked with the mesh it waits for.
        Some(own) if o.missing => Body3d { missing_mesh: o.body.missing_mesh.clone(), ..own.clone() },
        Some(own) => {
            let mut body = o.body.clone();
            if !o.height {
                body.height = own.height;
            }
            if !o.ports {
                // The part's ports stay where the part put them: the override's pose (a model's
                // offset and turn onto the footprint) places its mesh, not them.
                let to = body.offset.inverse().then(&own.offset);
                body.ports = if body.offset == own.offset { own.ports.clone() } else { own.ports.iter().map(|p| p.moved(&to)).collect() };
            }
            body
        }
        None if o.height => o.body.clone(),
        None => {
            c.diags.push(located(doc, span,
                format!("{}: e.body: the part has no body of its own, so the override states its height", def.name)));
            c.bodies[ix].applied = true;
            return def;
        }
    };
    let emitter = match &def.emitter {
        None => None,
        Some(emitter) => {
            let Some(port) = body.ports.iter().find(|p| p.kind == BodyPortKind::Light) else {
                c.diags.push(located(doc, span,
                    format!("{} emits light: its body needs a @light port", def.name)));
                return def;
            };
            let (at, dir, size) = emitter_geometry(port);
            // Further faces are the part's own body's: a body that moves the light port leaves
            // them behind, and its port's face emits all the light.
            let faces = if (at, dir, size) == (emitter.at, emitter.dir, emitter.size) { emitter.faces.clone() } else { Vec::new() };
            Some(Emitter { at, dir, size, faces, ..emitter.clone() })
        }
    };
    c.bodies[ix].applied = true;
    c.bodies[ix].own = def.body.clone();
    Arc::new(PartDef { body: Some(body), emitter, ..(*def).clone() })
}

fn define_part(c: &mut Ctx, name: &str, spec: &V, h: &Here, at: &mut Here) -> Result<usize, String> {
    if c.part_names.contains_key(name) {
        return Err("a part with this name is defined already".into());
    }
    let Some(o) = spec.obj() else { return Err(format!("the definition must be an object, got {}", spec.describe())) };
    check_opts_at(o, &["kind", "symbol", "footprint", "body", "emitter", "pads", "limits", "prefix", "fields", "gpio", "lifecycle", "approved"], c, h, 1, &[], at)?;
    let Some(Handle::Sym(s)) = o.get("symbol").and_then(V::handle) else {
        return Err("needs a symbol (symbol: f.ic_symbol(..) or f.two_terminal(..))".into());
    };
    let footprint = match o.get("footprint") {
        None | Some(V::Nil) => None,
        Some(v) => match v.handle() {
            Some(Handle::Fp(f)) => Some(prims_rename(&c.fps[f], name)),
            _ => return Err(format!("footprint: expected a footprint, got {}", v.describe())),
        },
    };
    let kind = match o.get("kind") {
        None | Some(V::Nil) => return Err(format!("needs kind: what the part is ({})", KINDS)),
        Some(v) => match v.id() {
            Some(k) => PartKind::of_name(k),
            None => return Err(format!("kind: expected a part kind ({KINDS}), got {}", v.describe())),
        },
    };
    let sym = &c.syms[s];
    let mut pins: Vec<PinDef> = sym.pins.iter().map(|(n, k)| PinDef {
        name: n.clone(), pads: vec![], kind: *k, unit: 0, limits: None, alternates: vec![], delay: None,
    }).collect();
    // Pads: `{VIN: 1 GND: [2, 3]}` or `[["1", "1"], ["VBUS", ["A9", "B9"]]]`.
    let entries: Vec<(String, &V)> = match o.get("pads") {
        None | Some(V::Nil) => vec![],
        Some(V::Obj(p)) => p.fields.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Some(V::Arr(list)) => {
            let mut out = Vec::new();
            for item in list {
                match item.list() {
                    Some([pin, pads]) => match pin.text() {
                        Some(pin) => out.push((pin.to_string(), pads)),
                        None => return Err(format!("pads: a pin name must be a string, got {}", pin.describe())),
                    },
                    _ => return Err(format!("pads: each entry is [pin, pads], got {}", item.describe())),
                }
            }
            out
        }
        Some(v) => return Err(format!("pads: expected an object or a list, got {}", v.describe())),
    };
    for (pin, pads) in entries {
        let Some(p) = pins.iter_mut().find(|p| p.name == pin) else {
            return Err(format!("pads: the symbol has no pin {pin}"));
        };
        let list: Vec<&V> = match pads {
            V::Arr(l) => l.iter().collect(),
            v => vec![v],
        };
        for v in list {
            let Some(no) = pad_no(v) else { return Err(format!("pads: {pin}: a pad is a number or a string, got {}", v.describe())) };
            if let Some(fp) = &footprint {
                if !fp.pads.iter().any(|pad| pad.no == no) {
                    return Err(format!("pads: {pin}: the footprint has no pad {no}"));
                }
            }
            p.pads.push(no);
        }
    }
    if footprint.is_some() {
        if let Some(p) = pins.iter().find(|p| p.pads.is_empty()) {
            return Err(format!("pads: pin {} has no pad", p.name));
        }
    }
    // Limits per pin; `io` applies to every bidirectional pin.
    if let Some(l) = o.get("limits") {
        let Some(l) = l.obj() else { return Err(format!("limits: expected an object, got {}", l.describe())) };
        for (pin, v) in &l.fields {
            let limits = limits_of(v).map_err(|m| format!("limits: {pin}: {m}"))?;
            if pin == "io" {
                for p in pins.iter_mut().filter(|p| p.kind == PinKind::Bidi) {
                    p.limits = Some(limits.clone());
                }
            } else {
                match pins.iter_mut().find(|p| &p.name == pin) {
                    Some(p) => p.limits = Some(limits),
                    None => return Err(format!("limits: the symbol has no pin {pin}")),
                }
            }
        }
    }
    let prefix = match o.get("prefix") {
        None | Some(V::Nil) => "U".to_string(),
        Some(v) => v.text().map(str::to_string).ok_or_else(|| format!("prefix: expected a string, got {}", v.describe()))?,
    };
    let mut fields = Fields::default();
    if let Some(v) = o.get("fields") {
        *at = option_here(c, h, 1, &["fields"]);
        let f = v.obj().ok_or_else(|| format!("fields: expected an object, got {}", v.describe()))?;
        if !f.items.is_empty() {
            return Err("fields: every string needs a key".into());
        }
        for (key, value) in &f.fields {
            *at = option_here(c, h, 1, &["fields", key]);
            let text = value.text().ok_or_else(|| {
                let got = value.num().map_or_else(|| value.describe(), |n| n.to_string());
                format!("fields: {key}: a string, got {got}")
            })?;
            fields.0.push((key.clone(), text.to_string()));
        }
    }
    let card = match o.get("gpio") {
        None => None,
        Some(v) => {
            *at = option_here(c, h, 1, &["gpio"]);
            Some(gpio_of(v, c, h, at)?)
        }
    };
    let lifecycle = match o.get("lifecycle") {
        None | Some(V::Nil) => None,
        Some(v) => match v.text_or_id().and_then(Lifecycle::of_name) {
            Some(lc) => Some(lc),
            None => {
                *at = option_here(c, h, 1, &["lifecycle"]);
                return Err(format!("lifecycle: expected @active, @nrnd or @obsolete, got {}", v.describe()));
            }
        },
    };
    let approved = match o.get("approved") {
        None | Some(V::Nil) => false,
        Some(v) => match v.bool() {
            Some(b) => b,
            None => {
                *at = option_here(c, h, 1, &["approved"]);
                return Err(format!("approved: expected true or false, got {}", v.describe()));
            }
        },
    };
    *at = h.clone();
    let body = match o.get("body") {
        None | Some(V::Nil) => None,
        Some(v) => {
            let spec = body_of(v, c, h, &["body"], at, true).map_err(|m| format!("body: {m}"))?;
            if let Some((path, named)) = &spec.missing {
                let message = format!("{name}: body: mesh {path} is not in the project: the part keeps the courtyard extruded to its height until the project holds it");
                c.warnings.push(warning(named, message));
            }
            Some(spec.body)
        }
    };
    *at = h.clone();
    let emitter = match o.get("emitter") {
        None | Some(V::Nil) => None,
        Some(v) => Some(emitter_of(v, body.as_ref()).map_err(|m| format!("emitter: {m}"))?),
    };
    let def = PartDef {
        name: name.to_string(),
        kind,
        symbol: sym.symbol.clone(),
        footprint: footprint.map(Arc::new),
        body,
        pins,
        fields,
        emitter,
        gpio: card,
        lifecycle,
        approved,
    };
    let def = apply_body_override(c, Arc::new(def));
    let ix = c.parts.len();
    c.parts.push(PartRec { def, prefix, here: h.clone() });
    c.part_names.insert(name.to_string(), ix);
    Ok(ix)
}

const KINDS: &str = "@resistor, @capacitor, @inductor, @diode, @led, @regulator, @mcu, @ic, @connector, @switch, @power_flag, @crystal, @transistor, @fuse";

fn prims_rename(fp: &Footprint, name: &str) -> Footprint {
    let mut fp = fp.clone();
    fp.name = name.to_string();
    fp
}

/// `{height: 0.6mm ports: [{kind: @light profile: {circle: 0.5mm} at: [0mm, 0mm, 0.6mm] dir: [0, 0, 1]
/// reach: 10mm margin: 0.2mm}]}`: the courtyard extruded to `height`, with the ports a case must
/// open (a profile is `{circle: r}` or `{rect: [[x0, y0], [x1, y1]] r: corner}`; a press port may
/// state its plunger's actuation `travel` and its `stand` above the housing).
/// `package: @qfn` generates the family's body from the footprint at `height` instead.
/// `surface: {material: "stainless_303" finish: "spi_a1"}`
/// states the whole body's surface, including every mesh piece. Finish and colour are optional;
/// `colour: "#8899aa"` or a swatch name overrides the material's natural colour.
/// A body as written: the body, whether its height and its ports were stated, and a mesh it names
/// that the project does not hold (its project path and where it is named). Such a body is the
/// courtyard extruded to its height, with `missing_mesh` set.
struct BodySpec {
    body: Body3d,
    height: bool,
    ports: bool,
    missing: Option<(String, Here)>,
}

/// A located warning at `at`.
fn warning(at: &Here, message: String) -> Diag {
    Diag::warning(message, vec![Sel::Source { doc: at.doc, start: at.span.start, end: at.span.end }])
}

/// `need_height`: a part's own body states its height; an `e.body` override may leave it (and its
/// ports) to the part.
fn body_of(v: &V, c: &Ctx, h: &Here, keys: &[&str], at: &mut Here, need_height: bool) -> Result<BodySpec, String> {
    let field_here = |key: &str| {
        let mut path = keys.to_vec();
        path.push(key);
        option_here(c, h, 1, &path)
    };
    *at = option_here(c, h, 1, keys);
    let o = v.obj().ok_or_else(|| format!("expected {{height: .. ports: [..]}}, got {}", v.describe()))?;
    check_opts_at(o, &["height", "ports", "mesh", "package", "offset", "rotation", "surface"], c, h, 1, keys, at)?;
    let mut missing = None;
    let package = match o.get("package") {
        None => None,
        Some(v) => {
            *at = field_here("package");
            if o.get("mesh").is_some() {
                return Err("package: a body is a mesh or a package, not both".into());
            }
            let name = v.id().ok_or_else(|| format!("package: a family such as @qfn, got {}", v.describe()))?;
            Some(PackageKind::lookup(name).ok_or_else(|| format!("package: @{name} is not a package family; the families are @qfn, @sop, @chip and @display"))?)
        }
    };
    let source = match o.get("mesh") {
        None => package.map_or(BodySource::Extrude, BodySource::Package),
        Some(v) => {
            *at = field_here("mesh");
            let mesh = v.text().ok_or_else(|| format!("mesh: a string, got {}", v.describe()))?;
            let doc = c.docs.iter().find(|d| d.id == h.doc).ok_or("mesh: no calling document")?;
            if doc.lib {
                return Err("mesh: library parts may not name a mesh".into());
            }
            let folder = doc.file.rsplit_once('/').map_or("", |(folder, _)| folder);
            let relative = mesh.replace('\\', "/");
            if relative.starts_with('/') || relative.split('/').next().is_some_and(|s| s.contains(':')) {
                return Err("mesh: an absolute path is not allowed".into());
            }
            let mut path: Vec<&str> = folder.split('/').filter(|s| !s.is_empty()).collect();
            for component in relative.split('/') {
                match component {
                    "" | "." => {}
                    ".." => { if path.pop().is_none() { return Err("mesh: the path leaves the project".into()); } }
                    component => path.push(component),
                }
            }
            let path = path.join("/");
            if mesh.is_empty() {
                return Err("mesh: a file name is needed".into());
            }
            if c.assets.binary_search(&path).is_err() {
                missing = Some((path, field_here("mesh")));
                BodySource::Extrude
            } else {
                BodySource::Mesh(path)
            }
        }
    };
    let mut offset = Xform3::IDENTITY;
    if let Some(v) = o.get("offset") {
        *at = field_here("offset");
        offset.t = match v.list() {
            Some([x, y, z]) => match (finite_nm(x), finite_nm(y), finite_nm(z)) {
                (Some(x), Some(y), Some(z)) => [x, y, z],
                _ => return Err("offset: [x, y, z] lengths".into()),
            },
            _ => return Err("offset: [x, y, z] lengths".into()),
        };
    }
    if let Some(v) = o.get("rotation") {
        *at = field_here("rotation");
        let angles = match v.list() {
            Some([x, y, z]) => match (x.num(), y.num(), z.num()) {
                (Some(x), Some(y), Some(z)) if [x, y, z].iter().all(|v| v.is_finite()) => [x, y, z],
                _ => return Err("rotation: [rx, ry, rz] angles, such as 90deg".into()),
            },
            _ => return Err("rotation: [rx, ry, rz] angles, such as 90deg".into()),
        };
        // Angle literals have already been converted to radians by the unit table.
        let rx = Xform3::axis_angle([0; 3], [1.0, 0.0, 0.0], Angle::rad(angles[0]));
        let ry = Xform3::axis_angle([0; 3], [0.0, 1.0, 0.0], Angle::rad(angles[1]));
        let rz = Xform3::axis_angle([0; 3], [0.0, 0.0, 1.0], Angle::rad(angles[2]));
        offset.q = rz.then(&ry).then(&rx).q;
    }
    let surface = if let Some(v) = o.get("surface") {
        let mut path = keys.to_vec();
        path.push("surface");
        Some(body_surface_of(v, c, h, &path, at)?)
    } else {
        None
    };
    *at = field_here("height");
    let height = match o.get("height") {
        None if !need_height => 0.0,
        v => v.and_then(V::num).ok_or("height: a length is needed")?,
    };
    *at = field_here("ports");
    let mut ports = Vec::new();
    for p in o.get("ports").and_then(V::list).unwrap_or(&[]) {
        let p = p.obj().ok_or_else(|| format!("ports: expected a port object, got {}", p.describe()))?;
        check_opts(p, &["kind", "profile", "at", "dir", "reach", "margin", "travel", "stand"])?;
        let kind = match p.get("kind").and_then(V::id) {
            Some("plug") => BodyPortKind::Plug,
            Some("light") => BodyPortKind::Light,
            Some("press") => BodyPortKind::Press,
            Some("air") => BodyPortKind::Air,
            Some("cable") => BodyPortKind::Cable,
            Some("shaft") => BodyPortKind::Shaft,
            _ => return Err("port kind: @plug, @light, @press, @air, @cable or @shaft".into()),
        };
        let profile = p.get("profile").and_then(V::obj).ok_or("port profile: {circle: r} or {rect: [[x0, y0], [x1, y1]] r: corner}")?;
        let ring = if let Some(r) = profile.get("circle").and_then(V::num) {
            Contour::circle(P::ZERO, nm(r))
        } else if let Some(corners) = profile.get("rect").and_then(V::list).filter(|c| c.len() == 2) {
            let (a, b) = (point(&corners[0]).ok_or("rect: [[x0, y0], [x1, y1]]")?, point(&corners[1]).ok_or("rect: [[x0, y0], [x1, y1]]")?);
            Contour::rect(a, b, nm(profile.get("r").and_then(V::num).unwrap_or(0.0)))
        } else {
            return Err("port profile: {circle: r} or {rect: [[x0, y0], [x1, y1]] r: corner}".into());
        };
        let triple = |k: &str, scale: f64| -> Result<[f64; 3], String> {
            match p.get(k).and_then(V::list) {
                Some([x, y, z]) => match (x.num(), y.num(), z.num()) {
                    (Some(x), Some(y), Some(z)) => Ok([x * scale, y * scale, z * scale]),
                    _ => Err(format!("port {k}: three numbers")),
                },
                _ => Err(format!("port {k}: [x, y, z]")),
            }
        };
        let at = triple("at", 1e9)?;
        let len = |k: &str| p.get(k).and_then(V::num).map(nm).ok_or_else(|| format!("port {k}: a length"));
        let travel = if p.get("travel").is_some() { len("travel")? } else { 0 };
        if travel < 0 {
            return Err("port travel: a length, not negative".into());
        }
        let stand = match p.get("stand") {
            Some(v) => finite_nm(v).filter(|_| v.num().is_some_and(|v| v >= 0.0))
                .ok_or("port stand: a length, not negative")?,
            None => 0,
        };
        ports.push(BodyPort {
            kind,
            profile: PolySet::from_ring_nonzero(ring.to_ring(nm(0.01e-3))),
            at: [at[0].round() as Nm, at[1].round() as Nm, at[2].round() as Nm],
            dir: triple("dir", 1.0)?,
            reach: len("reach")?,
            margin: len("margin")?,
            travel,
            stand,
        });
    }
    // A body waiting for its mesh is the courtyard extruded: the offset placed the mesh.
    if missing.is_some() {
        offset = Xform3::IDENTITY;
    }
    let missing_mesh = missing.as_ref().map(|(path, _): &(String, Here)| path.clone());
    let stated = (o.get("height").is_some(), o.get("ports").is_some());
    Ok(BodySpec {
        body: Body3d { source, offset, height: nm(height), ports, surface, missing_mesh },
        height: stated.0,
        ports: stated.1,
        missing,
    })
}

/// A body's surface, validated against the material table before it enters a part definition.
fn body_surface_of(v: &V, c: &Ctx, h: &Here, keys: &[&str], at: &mut Here) -> Result<BodySurface, String> {
    let field_here = |key: &str| {
        let mut path = keys.to_vec();
        path.push(key);
        option_here(c, h, 1, &path)
    };
    *at = option_here(c, h, 1, keys);
    let o = v.obj().ok_or_else(|| format!("surface: expected {{material: .. finish: .. colour: ..}}, got {}", v.describe()))?;
    check_opts_at(o, &["material", "finish", "colour"], c, h, 1, keys, at).map_err(|m| format!("surface: {m}"))?;
    *at = field_here("material");
    let material = o.get("material").and_then(V::text).ok_or("surface: material: a material id string is needed")?;
    let material = MaterialId::lookup(material).ok_or_else(|| format!("surface: material: unknown material {material}"))?;
    let finish = if let Some(v) = o.get("finish") {
        *at = field_here("finish");
        let finish = v.text().ok_or_else(|| format!("surface: finish: a finish id string, got {}", v.describe()))?;
        Some(FinishId::lookup(finish).ok_or_else(|| format!("surface: finish: unknown finish {finish}"))?.name().to_owned())
    } else {
        None
    };
    let colour = if let Some(v) = o.get("colour") {
        *at = field_here("colour");
        let text = v.text().ok_or_else(|| format!("surface: colour: a #rrggbb string or swatch name, got {}", v.describe()))?;
        // Generator colour parameters use these text forms; hex keeps the material's translucency.
        let hex = text.strip_prefix('#').filter(|s| s.len() == 6 && s.bytes().all(|c| c.is_ascii_hexdigit()))
            .and_then(|s| u32::from_str_radix(s, 16).ok());
        let colour = hex.map(|v| Colour::hex(v).with_translucency(Surface::new(material).colour.translucency))
            .or_else(|| fab_materials::swatch::colour_by_name(text))
            .ok_or_else(|| format!("surface: colour: a #rrggbb string or swatch name, got {text}"))?;
        Some(BodyColour { srgb: colour.srgb, translucency: colour.translucency, ral: colour.ral })
    } else {
        None
    };
    Ok(BodySurface { material: material.name().to_owned(), finish, colour })
}

/// `{angle: 120deg light: @red flux_per_a: 15}`: the light a part gives off. It leaves by the
/// body's light port: the port's centre and direction, and its opening's extent as the face.
/// `angle` is the full viewing angle at half intensity, `flux_per_a` lumens per ampere.
fn emitter_of(v: &V, body: Option<&Body3d>) -> Result<Emitter, String> {
    let o = v.obj().ok_or_else(|| format!("expected {{angle: .. light: ..}}, got {}", v.describe()))?;
    check_opts(o, &["angle", "light", "flux_per_a", "faces"])?;
    let port = body
        .and_then(|b| b.ports.iter().find(|p| p.kind == BodyPortKind::Light))
        .ok_or("the body needs a @light port for the light to leave by")?;
    let angle = o.get("angle").and_then(V::num).ok_or("angle: the full viewing angle (120deg)")?;
    let light = light_of(o.get("light").ok_or(LIGHTS)?).map_err(|m| format!("light: {m}"))?;
    let flux_per_a = match o.get("flux_per_a") {
        None | Some(V::Nil) => None,
        Some(v) => Some(v.num().ok_or_else(|| format!("flux_per_a: lumens per ampere, got {}", v.describe()))?),
    };
    let (at, dir, size) = emitter_geometry(port);
    let faces = match o.get("faces") {
        None | Some(V::Nil) => Vec::new(),
        Some(v) => emitter_faces(v, Angle::rad(angle))?,
    };
    Ok(Emitter {
        at,
        dir,
        size,
        angle: Angle::rad(angle),
        light,
        flux_per_a,
        faces,
    })
}

const FACE: &str = "faces: [{at: [x, y, z] dir: [x, y, z] size: [w, h] share: 0.2}, ..], each a share of the part's flux; angle defaults to the emitter's";

/// The emitter's further faces: each `{at dir size angle? share}` in the part's frame, its share of
/// the flux above 0, the shares together at most 1 (the port's face keeps the rest).
fn emitter_faces(v: &V, angle: Angle) -> Result<Vec<EmitterFace>, String> {
    let list = v.list().ok_or_else(|| format!("{FACE}, got {}", v.describe()))?;
    let mut faces = Vec::with_capacity(list.len());
    for face in list {
        let o = face.obj().ok_or_else(|| format!("{FACE}, got {}", face.describe()))?;
        check_opts(o, &["at", "dir", "size", "angle", "share"])?;
        let nums = |k: &str, n: usize| -> Result<Vec<f64>, String> {
            match o.get(k).and_then(V::list) {
                Some(items) if items.len() == n => items.iter().map(|item| item.num().filter(|x| x.is_finite())).collect::<Option<Vec<f64>>>()
                    .ok_or_else(|| format!("faces: {k} takes {n} numbers")),
                _ => Err(format!("faces: {k} takes {n} numbers ({FACE})")),
            }
        };
        let at = nums("at", 3)?;
        let dir = nums("dir", 3)?;
        let size = nums("size", 2)?;
        let norm = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        if norm < 1e-9 {
            return Err("faces: dir needs a direction, not [0, 0, 0]".into());
        }
        let share = o.get("share").and_then(V::num).filter(|s| *s > 0.0 && *s <= 1.0)
            .ok_or("faces: share is the face's part of the flux, above 0 and at most 1")?;
        let angle = match o.get("angle") {
            None => angle,
            Some(a) => Angle::rad(a.num().ok_or("faces: angle is the face's full viewing angle (120deg)")?),
        };
        faces.push(EmitterFace {
            at: [nm(at[0]), nm(at[1]), nm(at[2])],
            dir: [dir[0] / norm, dir[1] / norm, dir[2] / norm],
            size: P::new(nm(size[0]), nm(size[1])),
            angle,
            share,
        });
    }
    let total: f64 = faces.iter().map(|face| face.share).sum();
    if total > 1.0 + 1e-9 {
        return Err(format!("faces: their shares add up to {total:.3}; the port's face keeps what they leave, so they take at most 1"));
    }
    Ok(faces)
}

fn emitter_geometry(port: &BodyPort) -> ([Nm; 3], [f64; 3], P) {
    let open = port.profile.bbox();
    (port.at, port.dir, P::new(open.width() as Nm, open.height() as Nm))
}

const LIGHTS: &str = "light: an LED colour (@red), {peak: 630nm}, {rgb: [r, g, b]} or {spectrum: [[450nm, 0.2], ..]}";

/// An LED colour name, a peak wavelength, linear RGB or (wavelength, relative power) samples.
fn light_of(v: &V) -> Result<Light, String> {
    // Wavelengths are lengths in metres; `Light` holds nanometres, to the picometre.
    let nanometres = |v: &V| v.num().map(|m| (m * 1e12).round() / 1e3);
    if let Some(name) = v.text_or_id() {
        return Light::of_led_color(name).ok_or_else(|| format!("no generic LED light is known for {name}"));
    }
    let o = v.obj().ok_or(LIGHTS)?;
    check_opts(o, &["peak", "rgb", "spectrum"])?;
    if let Some(p) = o.get("peak") {
        return nanometres(p).map(Light::Peak).ok_or_else(|| format!("peak: a wavelength (630nm), got {}", p.describe()));
    }
    if let Some(c) = o.get("rgb") {
        return match c.list() {
            Some([r, g, b]) => match (r.num(), g.num(), b.num()) {
                (Some(r), Some(g), Some(b)) => Ok(Light::Rgb([r as f32, g as f32, b as f32])),
                _ => Err("rgb: three numbers".into()),
            },
            _ => Err(format!("rgb: [r, g, b], got {}", c.describe())),
        };
    }
    if let Some(s) = o.get("spectrum") {
        let mut out = Vec::new();
        for sample in s.list().unwrap_or(&[]) {
            match sample.list() {
                Some([w, p]) => match (nanometres(w), p.num()) {
                    (Some(w), Some(p)) => out.push((w, p)),
                    _ => return Err("spectrum: each sample is [wavelength, power]".into()),
                },
                _ => return Err(format!("spectrum: each sample is [wavelength, power], got {}", sample.describe())),
            }
        }
        if out.is_empty() {
            return Err("spectrum: at least one [wavelength, power] sample".into());
        }
        return Ok(Light::Spectrum(out));
    }
    Err(LIGHTS.into())
}

fn gpio_of(v: &V, c: &Ctx, h: &Here, at: &mut Here) -> Result<GpioCard, String> {
    let Some(o) = v.obj() else { return Err(format!("gpio: expected an object, got {}", v.describe())) };
    check_opts_at(o, &["r_high", "r_low", "pull_up", "vih", "vil", "leak", "edge", "c_pin"], c, h, 1, &["gpio"], at)
        .map_err(|m| format!("gpio: {m}"))?;
    let positive = |key: &str, at: &mut Here| -> Result<f64, String> {
        *at = option_here(c, h, 1, &["gpio", key]);
        let value = o.get(key).ok_or_else(|| format!("gpio: {key} is required"))?;
        value.num().filter(|n| n.is_finite() && *n > 0.0).ok_or_else(|| {
            let got = value.num().map_or_else(|| value.describe(), |n| n.to_string());
            format!("gpio: {key}: expected a positive number, got {got}")
        })
    };
    let card = GpioCard {
        r_high: positive("r_high", at)?,
        r_low: positive("r_low", at)?,
        pull_up: positive("pull_up", at)?,
        vih: positive("vih", at)?,
        vil: positive("vil", at)?,
        leak: positive("leak", at)?,
        edge: match o.get("edge") {
            None | Some(V::Nil) => None,
            Some(_) => Some(positive("edge", at)?),
        },
        c_pin: match o.get("c_pin") {
            None | Some(V::Nil) => None,
            Some(value) => {
                *at = option_here(c, h, 1, &["gpio", "c_pin"]);
                Some(value.num().filter(|n| n.is_finite() && *n >= 0.0).ok_or_else(|| {
                    let got = value.num().map_or_else(|| value.describe(), |n| n.to_string());
                    format!("gpio: c_pin: expected a finite non-negative number, got {got}")
                })?)
            }
        },
    };
    if !(card.vil < card.vih && card.vih < 1.0) {
        *at = option_here(c, h, 1, &["gpio", "vih"]);
        return Err(format!("gpio: vil and vih are fractions of the supply with vil < vih < 1, got vil {} vih {}", card.vil, card.vih));
    }
    Ok(card)
}

fn limits_of(v: &V) -> Result<Limits, String> {
    let Some(o) = v.obj() else { return Err(format!("expected {{volts: .. current: ..}}, got {}", v.describe())) };
    check_opts(o, &["volts", "current", "power"])?;
    let num = |k: &str| -> Result<Option<f64>, String> {
        match o.get(k) {
            None | Some(V::Nil) => Ok(None),
            Some(v) => v.num().map(Some).ok_or_else(|| format!("{k}: expected a number, got {}", v.describe())),
        }
    };
    let volts = match o.get("volts") {
        None | Some(V::Nil) => None,
        Some(v) => Some(volts(v).ok_or_else(|| format!("volts: expected a voltage or [low, high], got {}", v.describe()))?),
    };
    Ok(Limits { volts, current: num("current")?, power: num("power")? })
}

fn lookup(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let name = read(vm, args, "name");
        let ix = name.text().and_then(|n| c.borrow().part_names.get(n).copied());
        match ix {
            Some(ix) => handle(vm, id_lut!(Part), ix).into(),
            None => NIL,
        }
    })
}

/// The part a value names: a part handle or a defined part's name.
fn part_of(c: &Ctx, v: &V) -> Result<usize, String> {
    match v {
        V::Handle(Handle::Part(p)) => Ok(*p),
        V::Str(name) => c.part_names.get(name).copied().ok_or_else(|| format!("no part named {name} is defined (e.define(\"{name}\", {{..}}))")),
        other => Err(format!("expected a part or a part name, got {}", other.describe())),
    }
}

fn part(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let which = read(vm, args, "part");
        let opts = read(vm, args, "opts");
        let mut cx = c.borrow_mut();
        let h = here(vm, &cx);
        let p = match part_of(&cx, &which) {
            Ok(p) => p,
            Err(m) => {
                let path = item_path(&mut cx, &h, "part");
                return fail(&mut cx, &h, format!("{path}: {m}"));
            }
        };
        let comp = new_comp(&mut cx, &h, "part", p, None, Fields::default(), (None, None, false));
        if let Err(m) = hook_up(&mut cx, &h, comp, &opts) {
            let path = cx.build.comps[comp].path.clone();
            return fail(&mut cx, &h, format!("{path}: {m}"));
        }
        let mut fields = Fields::default();
        let (refdes, added, dnp) = match comp_options(&cx, &h, 1, &opts, &mut fields) {
            Ok(refdes) => refdes,
            Err((at, m)) => {
                let path = cx.build.comps[comp].path.clone();
                return fail(&mut cx, &at, format!("{path}: {m}"));
            }
        };
        cx.build.comps[comp].fields = fields;
        cx.build.comps[comp].refdes = refdes;
        cx.build.comps[comp].added = added;
        cx.build.comps[comp].dnp = dnp;
        comp_handle(vm, &cx, comp, None)
    })
}

/// The pins an options object's `nc: [..]` names, for `e.part`, `e.r`, `e.c`, `e.l` and `e.led`.
fn read_nc(o: &Obj, def: &PartDef) -> Result<Vec<PinIx>, String> {
    let Some(nc) = o.get("nc") else { return Ok(vec![]) };
    let Some(list) = nc.list() else { return Err(format!("nc: expected a list of pin names, got {}", nc.describe())) };
    list.iter().map(|v| {
        let Some(name) = v.text_or_id() else { return Err(format!("nc: expected a pin name, got {}", v.describe())) };
        def.pin(name).ok_or_else(|| format!("nc: {} has no pin {name}", def.name))
    }).collect()
}

/// Read the pins, no-connect list and value of `e.part`. Its text options are read by `comp_options`.
fn hook_up(c: &mut Ctx, h: &Here, comp: usize, opts: &V) -> Result<(), String> {
    let Some(o) = opts_of(opts)? else { return Ok(()) };
    check_opts(o, &["pins", "nc", "value", "fields", "ref", "added", "dnp"])?;
    let def = c.parts[c.build.comps[comp].part].def.clone();
    match o.get("pins") {
        None | Some(V::Nil) => {}
        Some(V::Obj(pins)) => {
            for (name, v) in &pins.fields {
                let Some(pin) = def.pin(name) else { return Err(format!("pins: {} has no pin {name}", def.name)) };
                connect_pin(c, h, comp, pin, v).map_err(|m| format!("pin {name} {m}"))?;
                let site = pin_site(c, h, |call| PinSite::Field { call, pin: name.clone(), count: 0 });
                c.build.comps[comp].pin_sites.push((pin, site));
            }
        }
        Some(V::Arr(list)) => {
            if list.len() > def.pins.len() {
                return Err(format!("pins: {} has {} pins, {} are given", def.name, def.pins.len(), list.len()));
            }
            for (i, v) in list.iter().enumerate() {
                connect_pin(c, h, comp, PinIx(i as u16), v).map_err(|m| format!("pin {} {m}", def.pins[i].name))?;
                let site = pin_site(c, h, |call| PinSite::Item { call, index: i, positional_tol: false, count: 0 });
                c.build.comps[comp].pin_sites.push((PinIx(i as u16), site));
            }
        }
        Some(v) => return Err(format!("pins: expected an object or a list, got {}", v.describe())),
    }
    let nc = read_nc(o, &def)?;
    c.build.comps[comp].nc.extend(nc);
    match o.get("value") {
        None | Some(V::Nil) => {}
        // A kind with a quantity (a resistor's ohms .. a crystal's hertz) takes it, as e.r and e.c do;
        // every other kind takes a label.
        Some(v) => c.build.comps[comp].value = Some(match (v, value_unit(&def.kind)) {
            (V::Num(si), Some(unit)) if si.is_finite() && *si >= 0.0 => {
                let q = Quantity { si: *si, unit };
                Value { text: value_text(&q), qty: Some(q), tol: None }
            }
            (v, Some(unit)) => return Err(format!(
                "value: a {}'s value is a quantity in {} (not negative), got {}", def.kind.name(), unit_name(unit), shown(v),
            )),
            (V::Str(s) | V::Id(s), None) => Value { text: s.clone(), qty: None, tol: None },
            (v, None) => return Err(format!(
                "value: a {}'s value is a label, got {} (resistors, capacitors, inductors and crystals take quantities)",
                def.kind.name(), shown(v),
            )),
        }),
    }
    Ok(())
}

/// A refused value as written: a number as its number, anything else described.
fn shown(v: &V) -> String {
    match v {
        V::Num(n) => n.to_string(),
        v => v.describe(),
    }
}

/// A value quantity's unit in words, for refusals.
fn unit_name(unit: QtyUnit) -> &'static str {
    match unit {
        QtyUnit::Ohm => "ohms",
        QtyUnit::Farad => "farads",
        QtyUnit::Henry => "henries",
        QtyUnit::Hertz => "hertz",
        QtyUnit::Volt => "volts",
        QtyUnit::Ampere => "amperes",
        QtyUnit::Watt => "watts",
        QtyUnit::Metre => "metres",
        QtyUnit::Second => "seconds",
        QtyUnit::Count => "a count",
    }
}

/// Read a symmetric tolerance or a pair of signed bounds, retaining literal unit semantics.
fn tolerance(c: &Ctx, h: &Here, positional: bool, value: &V, unit: QtyUnit) -> Result<Tol, (Range<u32>, String)> {
    let arg = c.docs.iter().find(|d| d.id == h.doc).and_then(|d| d.names())
        .and_then(|n| tol_arg(&n.syntax, h.span.start as usize..h.span.end as usize, positional));
    let whole = arg.as_ref().map_or_else(|| h.span.clone(), |a| a.span.start as u32..a.span.end as u32);
    let bounds = match value {
        V::Arr(items) if items.len() == 2 => items.as_slice(),
        V::Num(_) => std::slice::from_ref(value),
        _ => return Err((whole, "tol: expected a number or two signed bounds".into())),
    };
    let pair = bounds.len() == 2;
    let mut converted = Vec::new();
    for (i, bound) in bounds.iter().enumerate() {
        let item = arg.as_ref().and_then(|a| a.items.get(i));
        let at = item.map_or_else(|| whole.clone(), |item| item.span.start as u32..item.span.end as u32);
        let fail = |message: &str| (at.clone(), message.to_string());
        let n = bound.num().ok_or_else(|| fail("a tolerance bound must be a number"))?;
        if !n.is_finite() {
            return Err(fail("a tolerance must be finite"));
        }
        if (pair && i == 0 && n > 0.0) || ((!pair || i == 1) && n < 0.0) {
            return Err(fail(if pair { "tolerance bounds must be downward (<= 0), then upward (>= 0)" } else { "a symmetric tolerance must be non-negative" }));
        }
        let pct = match item.and_then(|item| item.suffix.as_deref()) {
            None | Some("%" | "ppm") => true,
            Some(suffix) if crate::units::qty_of(suffix).is_some_and(|(u, _, _)| u == unit) => false,
            _ => return Err(fail("a tolerance is written with %, ppm or the value's unit (pF, nF, ..)")),
        };
        let magnitude = if pct { tolerance_percent(n.abs()) } else { n.abs() };
        if !magnitude.is_finite() {
            return Err(fail("a tolerance must be finite"));
        }
        if converted.first().is_some_and(|&(first_pct, _)| first_pct != pct) {
            return Err(fail("tolerance bounds must both be percentages or both use the value's unit"));
        }
        converted.push((pct, magnitude));
    }
    let (pct, minus) = converted[0];
    let plus = converted.last().expect("one or two bounds").1;
    Ok(if pct { Tol::Pct { minus, plus } } else { Tol::Abs { minus, plus } })
}

/// `e.r(value, [tolerance,] a, b, {package volts tol fields ref added})`, `e.c(..)`: a two-terminal passive.
/// Tolerances use %, ppm or the value's unit; nonliteral expressions evaluate as fractions
/// of the value (`let t = 1%` followed by `e.r(10kohm t, a, b)` means one percent).
fn two_pin(c: Shared, prefix: &'static str, unit: QtyUnit) -> Native {
    Box::new(move |vm, args| {
        let values = ["value", "a", "b", "opts", "rest"].map(|key| read(vm, args, key));
        let value = &values[0];
        let positional_tol = values[1].num().is_some();
        let a = &values[two_pin_net_arg(0, positional_tol)];
        let b = &values[two_pin_net_arg(1, positional_tol)];
        let opts = &values[two_pin_net_arg(1, positional_tol) + 1];
        let callee = prefix.to_lowercase();
        let h = here(vm, &c.borrow());
        let checked = (|| -> Result<(f64, String, Fields), String> {
            let Some(v) = value.num() else { return Err(format!("the value must be a quantity, got {}", value.describe())) };
            let o = opts_of(opts)?;
            let mut package = "0402".to_string();
            let mut fields = Fields::default();
            if let Some(o) = o {
                check_opts(o, &["package", "volts", "tol", "fields", "ref", "nc", "added", "dnp"])?;
                if positional_tol && o.get("tol").is_some() {
                    return Err("a tolerance cannot be written both positionally and in tol:".into());
                }
                if let Some(p) = o.get("package") {
                    package = p.text().map(str::to_string).ok_or_else(|| format!("package: expected a string (\"0402\"), got {}", p.describe()))?;
                }
                if let Some(v) = o.get("volts") {
                    let Some(x) = v.num() else { return Err(format!("volts: expected a voltage, got {}", v.describe())) };
                    fields.0.push(("volts".into(), value_text(&Quantity { si: x, unit: QtyUnit::Volt })));
                }
            }
            Ok((v, package, fields))
        })();
        let (v, package, mut fields) = match checked {
            Ok(x) => x,
            Err(m) => {
                let mut cx = c.borrow_mut();
                let path = item_path(&mut cx, &h, &callee);
                return fail(&mut cx, &h, format!("{path}: {m}"));
            }
        };
        let checked = comp_options(&c.borrow(), &h, two_pin_net_arg(1, positional_tol) + 1, opts, &mut fields);
        let reference = match checked {
            Ok(reference) => reference,
            Err((at, m)) => {
                let mut cx = c.borrow_mut();
                let path = item_path(&mut cx, &h, &callee);
                return fail(&mut cx, &at, format!("{path}: {m}"));
            }
        };
        let written_tol = if positional_tol { Some(&values[1]) } else { opts.obj().and_then(|o| o.get("tol")) };
        let tol = match written_tol.map(|v| tolerance(&c.borrow(), &h, positional_tol, v, unit)).transpose() {
            Ok(tol) => tol,
            Err((span, message)) => {
                let mut cx = c.borrow_mut();
                let path = item_path(&mut cx, &h, &callee);
                let mut at = h.clone();
                at.span = span;
                return fail(&mut cx, &at, format!("{path}: {message}"));
            }
        };
        let args = [vm.new_string(prefix), vm.new_string(&package)];
        let part = std_part(vm, "chip", &args);
        let mut cx = c.borrow_mut();
        let p = match part {
            Ok(p) => p,
            Err(m) => {
                let path = item_path(&mut cx, &h, &callee);
                return fail(&mut cx, &h, format!("{path}: {m}"));
            }
        };
        let q = Quantity { si: v, unit };
        let comp = new_comp(&mut cx, &h, &callee, p, Some(Value { text: value_text(&q), qty: Some(q), tol }), fields, reference);
        if let Some(o) = opts.obj() {
            match read_nc(o, &cx.parts[p].def.clone()) {
                Ok(nc) => cx.build.comps[comp].nc = nc,
                Err(m) => {
                    let path = cx.build.comps[comp].path.clone();
                    return fail(&mut cx, &h, format!("{path}: {m}"));
                }
            }
        }
        for (i, end) in [(0u16, a), (1, b)] {
            let site = pin_site(&cx, &h, |call| PinSite::Item { call, index: i as usize, positional_tol, count: 0 });
            cx.build.comps[comp].pin_sites.push((PinIx(i), site));
            if let Err(m) = connect_pin(&mut cx, &h, comp, PinIx(i), end) {
                let path = cx.build.comps[comp].path.clone();
                return fail(&mut cx, &h, format!("{path}: pin {} {m}", i + 1));
            }
        }
        comp_handle(vm, &cx, comp, None)
    })
}

/// `e.led({color package current fields ref added}, a, k)`: an LED with optional ends; `d.vf` is its forward voltage,
/// at `current` when given (the model's curve), else the typical value at the rated current.
fn led(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let opts = read(vm, args, "opts");
        let a = read(vm, args, "a");
        let k = read(vm, args, "k");
        let h = here(vm, &c.borrow());
        let checked = (|| -> Result<(String, String, f64), String> {
            let Some(o) = opts_of(&opts)? else { return Err("needs {color: @green package: \"0603\"}".into()) };
            check_opts(o, &["color", "package", "current", "fields", "ref", "nc", "added", "dnp"])?;
            let Some(color) = o.get("color").and_then(V::text_or_id).map(str::to_string) else {
                return Err("color: expected a colour name (@green)".into());
            };
            let package = match o.get("package") {
                None => "0603".to_string(),
                Some(p) => p.text().map(str::to_string).ok_or_else(|| format!("package: expected a string, got {}", p.describe()))?,
            };
            let vf = match o.get("current") {
                Some(v) => {
                    let current = num(v, "current")?;
                    if !current.is_finite() || current <= 0.0 {
                        return Err(format!("current: expected a positive current in amperes, got {}", v.describe()));
                    }
                    crate::led::forward_volts_at(&color, current)
                        .ok_or_else(|| format!("current: no forward-voltage curve is known for {color}"))?
                }
                None => crate::led::forward_volts(&color)
                    .ok_or_else(|| format!("color: no forward voltage is known for {color}"))?,
            };
            Ok((color, package, vf))
        })();
        let fail_here = |m: String| {
            let mut cx = c.borrow_mut();
            let path = item_path(&mut cx, &h, "led");
            fail(&mut cx, &h, format!("{path}: {m}"))
        };
        let (color, package, vf) = match checked {
            Ok(x) => x,
            Err(m) => return fail_here(m),
        };
        let args = [vm.new_string(&package), vm.new_string(&color)];
        let p = match std_part(vm, "led_part", &args) {
            Ok(p) => p,
            Err(m) => return fail_here(m),
        };
        let mut fields = Fields::default();
        let checked = comp_options(&c.borrow(), &h, 0, &opts, &mut fields);
        let reference = match checked {
            Ok(reference) => reference,
            Err((at, m)) => {
                let mut cx = c.borrow_mut();
                let path = item_path(&mut cx, &h, "led");
                return fail(&mut cx, &at, format!("{path}: {m}"));
            }
        };
        let mut cx = c.borrow_mut();
        let comp = new_comp(&mut cx, &h, "led", p, Some(Value { text: color, qty: None, tol: None }), fields, reference);
        if let Some(o) = opts.obj() {
            match read_nc(o, &cx.parts[p].def.clone()) {
                Ok(nc) => cx.build.comps[comp].nc = nc,
                Err(m) => {
                    let path = cx.build.comps[comp].path.clone();
                    return fail(&mut cx, &h, format!("{path}: {m}"));
                }
            }
        }
        for (i, end) in [(0u16, &a), (1, &k)] {
            if matches!(end, V::Nil) { continue; }
            let site = pin_site(&cx, &h, |call| PinSite::Item { call, index: i as usize, positional_tol: false, count: 0 });
            cx.build.comps[comp].pin_sites.push((PinIx(i), site));
            if let Err(m) = connect_pin(&mut cx, &h, comp, PinIx(i), end) {
                let path = cx.build.comps[comp].path.clone();
                return fail(&mut cx, &h, format!("{path}: pin {} {m}", i + 1));
            }
        }
        comp_handle(vm, &cx, comp, Some(vf))
    })
}

fn series(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let value = read(vm, args, "value");
        let s = read(vm, args, "series");
        let name = s.text_or_id().unwrap_or("").to_string();
        match (value.num(), crate::eseries::Series::of_name(&name)) {
            (Some(v), Some(series)) => crate::eseries::nearest(v, series).into(),
            (None, _) => {
                let mut cx = c.borrow_mut();
                let h = here(vm, &cx);
                fail(&mut cx, &h, format!("e.series: the value must be a number, got {}", value.describe()))
            }
            (_, None) => {
                let mut cx = c.borrow_mut();
                let h = here(vm, &cx);
                fail(&mut cx, &h, format!("e.series: {} is not a series (@E3 .. @E192)", s.describe()))
            }
        }
    })
}

fn connect(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let a = read(vm, args, "a");
        let b = read(vm, args, "b");
        let mut cx = c.borrow_mut();
        let h = here(vm, &cx);
        let net_of = |cx: &Ctx, v: &V| -> Result<(Option<usize>, Option<(usize, PinIx)>), String> {
            match v.handle() {
                Some(Handle::Net(n)) => Ok((Some(cx.build.find(n)), None)),
                Some(Handle::Pin(comp, p)) => Ok((cx.build.pin_net(comp, PinIx(p)), Some((comp, PinIx(p))))),
                _ => Err(format!("e.connect takes nets and pins, got {}", v.describe())),
            }
        };
        let (na, pa) = match net_of(&*cx, &a) { Ok(x) => x, Err(m) => return fail(&mut cx, &h, m) };
        let (nb, pb) = match net_of(&*cx, &b) { Ok(x) => x, Err(m) => return fail(&mut cx, &h, m) };
        for (end, pin) in [(0, pa), (1, pb)] {
            if let Some((comp, pin)) = pin {
                let site = pin_site(&cx, &h, |call| PinSite::Connect { call, end, count: 0 });
                cx.build.comps[comp].pin_sites.push((pin, site));
            }
        }
        match (na, nb) {
            (Some(x), Some(y)) => {
                cx.build.union(x, y, &h);
            }
            (Some(n), None) => {
                let (comp, pin) = pb.expect("an unconnected end is a pin");
                cx.build.attach(comp, pin, n, &h);
            }
            (None, Some(n)) => {
                let (comp, pin) = pa.expect("an unconnected end is a pin");
                cx.build.attach(comp, pin, n, &h);
            }
            (None, None) => {
                let (ca, pa) = pa.expect("a pin");
                let (cb, pb) = pb.expect("a pin");
                let n = cx.net_of_pin(ca, pa);
                cx.build.attach(cb, pb, n, &h);
            }
        }
        NIL
    })
}

// ---- e.: nets, buses, classes -------------------------------------------------------------------

/// The path prefix of names made inside a block instance (`mcu.`).
fn local_prefix(c: &Ctx) -> String {
    let p = &c.build.insts[c.build.inst()].path;
    if p.is_empty() { String::new() } else { format!("{p}.") }
}

fn net(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let opts = read(vm, args, "opts");
        let mut cx = c.borrow_mut();
        let h = here(vm, &cx);
        let made = (|| -> Result<(Option<String>, NetKind, Option<VRange>, Option<String>), String> {
            let Some(o) = opts_of(&opts)? else { return Ok((None, NetKind::Signal, None, None)) };
            check_opts(o, &["name", "kind", "volts", "class"])?;
            let name = match o.get("name") {
                None | Some(V::Nil) => None,
                Some(v) => Some(v.text().map(str::to_string).ok_or_else(|| format!("name: expected a string, got {}", v.describe()))?),
            };
            let kind = match o.get("kind").and_then(V::text_or_id) {
                None => NetKind::Signal,
                Some("signal") => NetKind::Signal,
                Some("power") => NetKind::Power,
                Some("ground") => NetKind::Ground,
                Some(k) => return Err(format!("kind: {k} is not a net kind (@signal, @power, @ground)")),
            };
            let volts = match o.get("volts") {
                None | Some(V::Nil) => None,
                Some(v) => Some(volts(v).ok_or_else(|| format!("volts: expected a voltage or [low, high], got {}", v.describe()))?),
            };
            let class = match o.get("class") {
                None | Some(V::Nil) => None,
                Some(v) => Some(v.text().map(str::to_string).ok_or_else(|| format!("class: expected a class name, got {}", v.describe()))?),
            };
            Ok((name, kind, volts, class))
        })();
        let (name, kind, volts, class) = match made {
            Ok(x) => x,
            Err(m) => {
                let path = item_path(&mut cx, &h, "net");
                return fail(&mut cx, &h, format!("{path}: {m}"));
            }
        };
        // An explicit name is the net's name; an unnamed net is named by its binding.
        let (name, explicit, scope) = match name {
            Some(n) => (n, true, None),
            None => (item_path(&mut cx, &h, "net"), false, Some(cx.build.inst())),
        };
        let n = cx.build.new_net(name, explicit, scope, h);
        cx.build.nets[n].kind = kind;
        cx.build.nets[n].volts = volts;
        cx.build.nets[n].class = class;
        net_handle(vm, &cx, n)
    })
}

/// `e.bus("LED", 8)`: nets `LED0..LED7`, a list of net handles; `e.bus("SWD", [@SWDIO ..])`: a net
/// per member, an object with the members as fields. Inside a block the nets and the bus are
/// prefixed with the instance path (`mcu.SWDIO`); members are not.
fn bus(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let name = read(vm, args, "name");
        let members = read(vm, args, "members");
        let mut cx = c.borrow_mut();
        let h = here(vm, &cx);
        let Some(name) = name.text().map(str::to_string) else {
            return fail(&mut cx, &h, format!("e.bus: the bus name must be a string, got {}", name.describe()));
        };
        let prefix = local_prefix(&cx);
        let (names, listed): (Vec<String>, bool) = match &members {
            V::Num(n) if n.fract() == 0.0 && *n >= 1.0 => ((0..*n as usize).map(|i| format!("{name}{i}")).collect(), true),
            V::Arr(list) => match list.iter().map(|v| v.text_or_id().map(str::to_string)).collect::<Option<Vec<_>>>() {
                Some(names) => (names, false),
                None => return fail(&mut cx, &h, format!("{prefix}{name}: the members are a count or a list of names")),
            },
            other => return fail(&mut cx, &h, format!("{prefix}{name}: the members are a count or a list of names, got {}", other.describe())),
        };
        let scope = Some(cx.build.inst());
        let nets: Vec<usize> = names.iter().map(|m| cx.build.new_net(format!("{prefix}{m}"), true, scope, h.clone())).collect();
        let b = cx.build.buses.len();
        cx.build.buses.push(BusRec { name: format!("{prefix}{name}"), members: names.iter().cloned().zip(nets.iter().copied()).collect() });
        cx.build.other_sites.push(h);
        let handles: Vec<ScriptValue> = nets.iter().map(|n| net_handle(vm, &cx, *n)).collect();
        if listed {
            let heap = vm.bx.heap_mut();
            let a = heap.new_array();
            for v in handles {
                heap.array_push(a, v, NoTrap);
            }
            a.into()
        } else {
            let o = handle(vm, id_lut!(Bus), b);
            let heap = vm.bx.heap_mut();
            for (m, v) in names.iter().zip(handles) {
                heap.set_value_def(o, lut_id(m).into(), v);
            }
            o.into()
        }
    })
}

fn netclass(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let name = read(vm, args, "name");
        let rules = read(vm, args, "rules");
        let raw = arg(vm, args, id_lut!(name));
        let mut cx = c.borrow_mut();
        let h = here(vm, &cx);
        let Some(name) = name.text().map(str::to_string) else {
            return fail(&mut cx, &h, format!("netclass: the name must be a string, got {}", name.describe()));
        };
        let made = (|| -> Result<ClassRules, String> {
            let mut r = ClassRules::default();
            let Some(o) = opts_of(&rules)? else { return Ok(r) };
            check_opts(o, &["track", "clearance", "via", "via_in_pad"])?;
            let len = |k: &str| -> Result<Option<i32>, String> {
                match o.get(k) {
                    None | Some(V::Nil) => Ok(None),
                    Some(v) => v.num().map(|x| Some(nm(x))).ok_or_else(|| format!("{k}: expected a length, got {}", v.describe())),
                }
            };
            r.track = len("track")?;
            r.clearance = len("clearance")?;
            r.via_in_pad = match o.get("via_in_pad") {
                None | Some(V::Nil) => None,
                Some(V::Bool(value)) => Some(*value),
                Some(v) => return Err(format!("via_in_pad: expected true or false, got {}", v.describe())),
            };
            if let Some(v) = o.get("via") {
                let Some(via) = v.obj() else { return Err(format!("via: expected {{drill: .. pad: ..}}, got {}", v.describe())) };
                check_opts(via, &["drill", "pad"])?;
                match (via.get("drill").and_then(V::num), via.get("pad").and_then(V::num)) {
                    (Some(d), Some(p)) => r.via = Some(ViaSize { drill: nm(d), pad: nm(p) }),
                    _ => return Err("via: needs drill and pad lengths".into()),
                }
            }
            Ok(r)
        })();
        match made {
            Ok(r) => {
                if cx.build.classes.iter().any(|(n, _)| *n == name) || name == "default" {
                    return fail(&mut cx, &h, format!("netclass {name}: a class with this name exists already"));
                }
                cx.build.classes.push((name, r));
                cx.build.other_sites.push(h);
                raw
            }
            Err(m) => fail(&mut cx, &h, format!("netclass {name}: {m}")),
        }
    })
}

fn param(_c: Shared) -> Native {
    Box::new(move |vm, args| arg(vm, args, id_lut!(value)))
}

// ---- e.: blocks ---------------------------------------------------------------------------------

/// The kind of value a block parameter's default literal is (`5.1kohm` a resistance).
enum ParamShape {
    Qty(QtyUnit),
    Int,
    Num,
    List(Vec<ParamShape>),
    Other,
}

fn literal_suffix(text: &str) -> &str {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.' || b[i] == b'_') {
        i += 1;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    &text[i..]
}

fn shape(syntax: &syntax::Syntax, n: &syntax::Node) -> ParamShape {
    match &n.kind {
        syntax::Kind::Number(_) => {
            let text = syntax.text(n);
            match literal_suffix(text) {
                "" if text.contains(['.', 'e', 'E']) => ParamShape::Num,
                "" => ParamShape::Int,
                s => crate::units::qty_of(s).map(|(u, _, _)| ParamShape::Qty(u)).unwrap_or(ParamShape::Num),
            }
        }
        syntax::Kind::Unary { operand, .. } => shape(syntax, operand),
        syntax::Kind::Array { items, .. } => ParamShape::List(items.iter().map(|i| shape(syntax, i)).collect()),
        _ => ParamShape::Other,
    }
}

/// The shapes of the `params: {..}` defaults of the `e.block({..})` call at `h`.
fn param_shapes(c: &Ctx, h: &Here) -> Vec<(String, ParamShape)> {
    let Some(info) = c.docs.iter().find(|d| d.id == h.doc) else { return vec![] };
    let Some(names) = info.names() else { return vec![] };
    let syntax = &names.syntax;
    let mut out = Vec::new();
    let span = h.span.start as usize..h.span.end as usize;
    syntax.walk(&mut |_, n| {
        if n.span != span || !out.is_empty() {
            return;
        }
        let syntax::Kind::Call { args, .. } = &n.kind else { return };
        let Some(spec) = args.first() else { return };
        let Some(field) = spec.field("params") else { return };
        if let syntax::Kind::Object { fields, .. } = &field.value.kind {
            for f in fields {
                if let Some(name) = &f.name {
                    out.push((name.clone(), shape(syntax, &f.value)));
                }
            }
        }
    });
    out
}

fn param_value(v: &V, s: &ParamShape) -> Option<ParamValue> {
    Some(match (v, s) {
        (V::Num(x), ParamShape::Qty(u)) => ParamValue::Qty(Quantity { si: *x, unit: *u }),
        (V::Num(x), ParamShape::Int) => ParamValue::Int(*x as i64),
        (V::Num(x), _) => ParamValue::Num(*x),
        (V::Arr(items), ParamShape::List(shapes)) => ParamValue::List(
            items.iter().enumerate().map(|(i, v)| param_value(v, shapes.get(i).unwrap_or(&ParamShape::Other))).collect::<Option<_>>()?,
        ),
        (V::Arr(items), _) => ParamValue::List(items.iter().map(|v| param_value(v, &ParamShape::Other)).collect::<Option<_>>()?),
        (V::Str(s), _) => ParamValue::Text(s.clone()),
        (V::Id(s), _) => ParamValue::Sym(s.clone()),
        (V::Bool(b), _) => ParamValue::Bool(*b),
        _ => return None,
    })
}

/// The shape an override takes: the default's (`c_in: 2.2uF` stays a capacitance).
fn shape_of(p: &ParamValue) -> ParamShape {
    match p {
        ParamValue::Qty(q) => ParamShape::Qty(q.unit),
        ParamValue::Int(_) => ParamShape::Int,
        ParamValue::List(l) => ParamShape::List(l.iter().map(shape_of).collect()),
        _ => ParamShape::Num,
    }
}

fn port(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let kind = read(vm, args, "kind");
        let opts = read(vm, args, "opts");
        let mut cx = c.borrow_mut();
        let h = here(vm, &cx);
        let made = (|| -> Result<PortDef, String> {
            let mut kind = match kind.text_or_id() {
                Some("power_in") => PortKind::PowerIn,
                Some("power_out") => PortKind::PowerOut,
                Some("ground") => PortKind::Ground,
                Some("in") => PortKind::In,
                Some("out") => PortKind::Out,
                Some("bidi") => PortKind::Bidi,
                Some("passive") => PortKind::Passive,
                Some("pair") => PortKind::Pair,
                _ => return Err(format!("e.port: {} is not a port kind (@power_in, @power_out, @ground, @in, @out, @bidi, @passive, @pair)", kind.describe())),
            };
            let mut p = PortDef { name: String::new(), kind: PortKind::Passive, volts: None, current: None, default: None };
            if let Some(o) = opts_of(&opts)? {
                check_opts(o, &["volts", "current", "width"])?;
                if let Some(v) = o.get("volts") {
                    p.volts = Some(volts(v).ok_or_else(|| format!("volts: expected a voltage or [low, high], got {}", v.describe()))?);
                }
                if let Some(v) = o.get("current") {
                    p.current = Some(v.num().ok_or_else(|| format!("current: expected a current, got {}", v.describe()))?);
                }
                if let Some(v) = o.get("width") {
                    match v.num() {
                        Some(w) if w >= 1.0 && w.fract() == 0.0 && w <= u16::MAX as f64 => kind = PortKind::Bus { width: w as u16 },
                        _ => return Err(format!("width: expected a whole number of nets, got {}", v.describe())),
                    }
                }
            }
            p.kind = kind;
            Ok(p)
        })();
        match made {
            Ok(p) => {
                let ix = cx.ports.len();
                cx.ports.push(p);
                drop(cx);
                handle(vm, id_lut!(Port), ix).into()
            }
            Err(m) => fail(&mut cx, &h, m),
        }
    })
}

/// `e.block({name ports params build sheet})`: a block definition; calling what it returns makes an
/// instance: `usb_c_power({vbus: vbus gnd: gnd})` binds the ports, overrides parameters and runs
/// `build(p, o)` with the instance's path as the prefix of everything it makes.
fn block(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let spec = read(vm, args, "spec");
        let raw = arg(vm, args, id_lut!(spec)).as_object();
        let mut cx = c.borrow_mut();
        let h = here(vm, &cx);
        let made = (|| -> Result<(BlockDef, Option<ScriptValue>, Option<ScriptObject>), String> {
            let Some(o) = spec.obj() else { return Err(format!("e.block takes {{name ports params build sheet}}, got {}", spec.describe())) };
            check_opts(o, &["name", "ports", "params", "build", "sheet", "planned", "role"])?;
            let planned = match o.get("planned") {
                None => false,
                Some(V::Bool(v)) => *v,
                Some(v) => return Err(format!("planned: expected a boolean, got {}", v.describe())),
            };
            let role = match o.get("role") {
                None => None,
                Some(V::Str(s)) if !s.trim().is_empty() => Some(s.clone()),
                Some(_) => return Err("role: expected a non-empty string".into()),
            };
            let name = o.get("name").and_then(V::text).map(str::to_string).ok_or("name: a block needs a name string")?;
            let mut ports = Vec::new();
            if let Some(p) = o.get("ports") {
                let Some(p) = p.obj() else { return Err(format!("{name}: ports: expected an object, got {}", p.describe())) };
                for (pname, v) in &p.fields {
                    let Some(Handle::Port(ix)) = v.handle() else { return Err(format!("{name}: port {pname}: expected e.port(..), got {}", v.describe())) };
                    let mut def = cx.ports[ix].clone();
                    def.name = pname.clone();
                    ports.push(def);
                }
            }
            let mut params = Vec::new();
            if let Some(p) = o.get("params") {
                let Some(p) = p.obj() else { return Err(format!("{name}: params: expected an object, got {}", p.describe())) };
                let shapes = param_shapes(&cx, &h);
                for (pname, v) in &p.fields {
                    let s = shapes.iter().find(|(n, _)| n == pname).map(|(_, s)| s).unwrap_or(&ParamShape::Other);
                    let Some(value) = param_value(v, s) else { return Err(format!("{name}: param {pname}: {} is not a parameter value", v.describe())) };
                    params.push((pname.clone(), value));
                }
            }
            let mut sheet = BlockSheet::default();
            if let Some(s) = o.get("sheet") {
                let Some(s) = s.obj() else { return Err(format!("{name}: sheet: expected an object, got {}", s.describe())) };
                check_opts(s, &["flow", "ports"]).map_err(|m| format!("{name}: sheet: {m}"))?;
                if let Some(f) = s.get("flow") {
                    sheet.flow = match f.text_or_id() {
                        Some("right") => Flow::Right,
                        Some("down") => Flow::Down,
                        Some("left") => Flow::Left,
                        Some("up") => Flow::Up,
                        _ => return Err(format!("{name}: sheet flow: {} is not @right, @down, @left or @up", f.describe())),
                    };
                }
                if let Some(p) = s.get("ports").and_then(V::obj) {
                    for (pname, v) in &p.fields {
                        let side = match v.text_or_id() {
                            Some("left") => PortSide::Left,
                            Some("right") => PortSide::Right,
                            Some("top") => PortSide::Top,
                            Some("bottom") => PortSide::Bottom,
                            _ => return Err(format!("{name}: sheet port {pname}: {} is not a side", v.describe())),
                        };
                        if !ports.iter().any(|p| &p.name == pname) {
                            return Err(format!("{name}: sheet: there is no port {pname}"));
                        }
                        sheet.sides.push((pname.clone(), side));
                    }
                }
            }
            let Some(spec_obj) = raw else { return Err("e.block: expected an object".into()) };
            let build = field(vm, spec_obj, id_lut!(build));
            let build = if planned && build.is_nil() {
                None
            } else if build.as_object().is_some_and(|b| vm.bx.heap_ref().is_fn(b)) {
                Some(build)
            } else {
                return Err(format!("{name}: build: expected fn(p, o) {{ .. }}"));
            };
            let params_obj = field(vm, spec_obj, id_lut!(params)).as_object();
            // The id is issued at the first instance (`used`), so a library block nobody makes
            // never enters the circuit.
            Ok((BlockDef { id: BlockDefId(u32::MAX), name, ports, params, sheet, template: None, planned, role }, build, params_obj))
        })();
        let (def, build, params_obj) = match made {
            Ok(x) => x,
            Err(m) => return fail(&mut cx, &h, m),
        };
        let i = cx.defs.len();
        if let Some(b) = build.and_then(|v| v.as_object()) {
            let r = vm.new_object_ref(b);
            cx.keep.push(r);
        }
        if let Some(p) = params_obj {
            let r = vm.new_object_ref(p);
            cx.keep.push(r);
        }
        cx.defs.push(DefRec { def, build, params: params_obj, here: h.clone() });
        let holder = cx.holder.expect("the holder is made at install");
        drop(cx);
        let key = lut_id(&format!("__block{i}"));
        let c2 = c.clone();
        vm.add_method(holder, key, &[(id_lut!(o), NIL)], move |vm, args| instance(&c2, i, vm, args));
        vm.bx.heap_ref().value(holder, key.into(), NoTrap)
    })
}

fn instance(c: &Shared, def: usize, vm: &mut ScriptVm, args: ScriptObject) -> ScriptValue {
    let raw = arg(vm, args, id_lut!(o));
    let given = Reader::new(vm).read(raw);
    let mut cx = c.borrow_mut();
    let h = here(vm, &cx);
    let path = item_path(&mut cx, &h, "block");
    let d = &cx.defs[def];
    let (build, params_obj, planned) = (d.build, d.params, d.def.planned);
    let port_defs: Vec<String> = d.def.ports.iter().map(|p| p.name.clone()).collect();
    let defaults = d.def.params.clone();
    let block_name = d.def.name.clone();
    let fields: Vec<(String, V)> = match &given {
        V::Nil => vec![],
        V::Obj(o) => o.fields.clone(),
        other => return fail(&mut cx, &h, format!("{path}: a {block_name} instance takes {{port: net ..}}, got {}", other.describe())),
    };
    let mut ports = Vec::new();
    for p in &port_defs {
        let value = fields.iter().find(|(k, _)| k == p).map(|(_, v)| v);
        if planned && matches!(value, None | Some(V::Nil)) {
            ports.push((p.clone(), Vec::new()));
            continue;
        }
        let Some(v) = value else {
            return fail(&mut cx, &h, format!("{path}: port {p} of {block_name} is not connected"));
        };
        match port_nets(&mut cx, v) {
            Ok(nets) => ports.push((p.clone(), nets)),
            Err(m) => return fail(&mut cx, &h, format!("{path}: port {p} {m}")),
        }
    }
    let mut params = defaults.clone();
    for (k, v) in &fields {
        if port_defs.contains(k) {
            continue;
        }
        let Some(slot) = params.iter_mut().find(|(n, _)| n == k) else {
            return fail(&mut cx, &h, format!("{path}: {block_name} has no port or parameter {k}"));
        };
        match param_value(v, &shape_of(&slot.1)) {
            Some(value) => slot.1 = value,
            None => return fail(&mut cx, &h, format!("{path}: parameter {k}: {} is not a parameter value", v.describe())),
        }
    }
    let used = match cx.build.used.iter().position(|&d| d == def) {
        Some(u) => u,
        None => {
            cx.build.used.push(def);
            cx.build.used.len() - 1
        }
    };
    let parent = cx.build.inst();
    let inst = cx.build.insts.len();
    cx.build.insts.push(InstRec { path, def: Some(used), rec: Some(def), parent: Some(parent), ports, params, here: Some(h) });
    let Some(build) = build else {
        return handle(vm, id_lut!(Inst), inst).into();
    };
    cx.build.stack.push(inst);
    let param_names: Vec<String> = defaults.iter().map(|(n, _)| n.clone()).collect();
    drop(cx);
    let raw_obj = raw.as_object();
    let get = |vm: &ScriptVm, obj: Option<ScriptObject>, key: LiveId| obj.map(|r| field(vm, r, key)).unwrap_or(NIL);
    let ports_in: Vec<(LiveId, ScriptValue)> = port_defs.iter().map(|n| (lut_id(n), get(vm, raw_obj, lut_id(n)))).collect();
    let params_in: Vec<(LiveId, ScriptValue)> = param_names.iter().map(|n| {
        let key = lut_id(n);
        let given = get(vm, raw_obj, key);
        (key, if given.is_nil() { get(vm, params_obj, key) } else { given })
    }).collect();
    let heap = vm.bx.heap_mut();
    let p = heap.new_object();
    let o = heap.new_object();
    for (key, v) in ports_in {
        heap.set_value_def(p, key.into(), v);
    }
    for (key, v) in params_in {
        heap.set_value_def(o, key.into(), v);
    }
    vm.call(build, &[p.into(), o.into()]);
    c.borrow_mut().build.stack.pop();
    handle(vm, id_lut!(Inst), inst).into()
}

// ---- e.: intents --------------------------------------------------------------------------------

fn target(v: &V) -> Result<Tgt, String> {
    match v.handle() {
        Some(Handle::Comp(c)) => Ok(Tgt::Comp(c)),
        Some(Handle::Pin(c, p)) => Ok(Tgt::Pin(c, PinIx(p))),
        Some(Handle::Inst(b)) => Ok(Tgt::Inst(b)),
        _ => Err(format!("expected a component, a pin or a block instance, got {}", v.describe())),
    }
}

fn length(v: &V, what: &str) -> Result<i32, String> {
    v.num().map(nm).ok_or_else(|| format!("{what}: expected a length, got {}", v.describe()))
}

fn positive_length(v: &V, what: &str) -> Result<Nm, String> {
    let metres = v.num().ok_or_else(|| format!("{what}: expected a length, got {}", v.describe()))?;
    let value = (metres * 1e9).round();
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("{what}: must be > 0 and finite"));
    }
    if value > Nm::MAX as f64 {
        return Err(format!("{what}: length is outside the nanometre range"));
    }
    Ok(value as Nm)
}

fn intent(c: &Shared, vm: &ScriptVm, callee: &str, make: impl FnOnce() -> Result<IntentRec, String>) -> ScriptValue {
    let mut cx = c.borrow_mut();
    match make() {
        Ok(i) => {
            cx.build.intents.push(i);
            NIL
        }
        Err(m) => {
            let h = here(vm, &cx);
            fail(&mut cx, &h, format!("e.{callee}: {m}"))
        }
    }
}

fn near(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let (a, b, max) = (read(vm, args, "a"), read(vm, args, "b"), read(vm, args, "max"));
        intent(&c, vm, "near", || Ok(IntentRec::Near { a: target(&a)?, b: target(&b)?, max: length(&max, "max")? }))
    })
}

fn side(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let (t, s) = (read(vm, args, "t"), read(vm, args, "side"));
        intent(&c, vm, "side", || {
            let side = match s.text_or_id() {
                Some("top") => Side::Top,
                Some("bottom") => Side::Bottom,
                _ => return Err(format!("{} is not @top or @bottom", s.describe())),
            };
            Ok(IntentRec::Side { t: target(&t)?, side })
        })
    })
}

fn fixed(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let (t, opts) = (read(vm, args, "t"), read(vm, args, "opts"));
        intent(&c, vm, "fixed", || {
            let t = target(&t)?;
            let (mut edge, mut at, mut rot) = (None, None, None);
            if let Some(o) = opts_of(&opts)? {
                check_opts(o, &["edge", "at", "rot"])?;
                if let Some(e) = o.get("edge") {
                    edge = Some(match e.text_or_id() {
                        Some("left") => Edge::Left,
                        Some("right") => Edge::Right,
                        Some("top") => Edge::Top,
                        Some("bottom") => Edge::Bottom,
                        _ => return Err(format!("edge: {} is not @left, @right, @top or @bottom", e.describe())),
                    });
                }
                if let Some(p) = o.get("at") {
                    at = Some(point(p).ok_or_else(|| format!("at: expected [x, y], got {}", p.describe()))?);
                    // A spot is one part's: a block's parts cannot all stand on it.
                    if matches!(t, Tgt::Inst(_)) {
                        return Err("at: a spot fixes one part: name the part, not the block".into());
                    }
                }
                if let Some(r) = o.get("rot") {
                    rot = Some(Angle::rad(r.num().ok_or_else(|| format!("rot: expected an angle, got {}", r.describe()))?));
                }
            }
            Ok(IntentRec::Fixed { t, edge, at, rot })
        })
    })
}

fn width(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let (n, w) = (read(vm, args, "net"), read(vm, args, "w"));
        intent(&c, vm, "width", || match n.handle() {
            Some(Handle::Net(net)) => Ok(IntentRec::Width { net, w: length(&w, "width")? }),
            _ => Err(format!("expected a net, got {}", n.describe())),
        })
    })
}

/// Writes a pair and, with `skew`, an adjacent length match checked once both nets are fully routed.
fn pair(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let (p, n, opts) = (read(vm, args, "p"), read(vm, args, "n"), read(vm, args, "opts"));
        let h = here(vm, &c.borrow());
        intent(&c, vm, "pair", || {
            let p = match p.handle() {
                Some(Handle::Net(p)) => p,
                _ => return Err(format!("p: expected a net, got {}", p.describe())),
            };
            let n = match n.handle() {
                Some(Handle::Net(n)) => n,
                _ => return Err(format!("n: expected a net, got {}", n.describe())),
            };
            if p == n {
                return Err("p and n are the same net".into());
            }
            let (mut gap, mut z0, mut skew) = (None, None, None);
            if let Some(o) = opts_of(&opts)? {
                check_opts(o, &["gap", "z0", "skew"])?;
                skew = o.get("skew").map(|v| positive_length(v, "skew")).transpose()?;
                if let Some(v) = o.get("gap") {
                    let g = length(v, "gap")?;
                    if g <= 0 {
                        return Err("gap: must be > 0".into());
                    }
                    gap = Some(g);
                }
                if let Some(v) = o.get("z0") {
                    let z = num(v, "z0")?;
                    if !z.is_finite() || z <= 0.0 {
                        return Err("z0: must be a finite number > 0".into());
                    }
                    z0 = Some(z);
                }
            }
            // A missing gap resolves downstream through ClassRules.pair, then the board rule.
            Ok(IntentRec::Pair { p, n, gap, z0, skew, here: h })
        })
    })
}

/// Writes a length match checked once every member net is fully routed.
fn match_length(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let (nets, opts) = (read(vm, args, "nets"), read(vm, args, "opts"));
        let h = here(vm, &c.borrow());
        intent(&c, vm, "match_length", || {
            let list = nets.list().ok_or_else(|| format!("nets: expected a list of nets, got {}", nets.describe()))?;
            let nets = list.iter().enumerate().map(|(i, v)| match v.handle() {
                Some(Handle::Net(n)) => Ok(n),
                _ => Err(format!("nets[{}]: expected a net, got {}", i, v.describe())),
            }).collect::<Result<Vec<_>, _>>()?;
            let opts = opts_of(&opts)?.ok_or("tol: missing length tolerance")?;
            check_opts(opts, &["tol"])?;
            let tol = positive_length(opts.get("tol").ok_or("tol: missing length tolerance")?, "tol")?;
            Ok(IntentRec::LengthMatch { nets, tol, here: h })
        })
    })
}

fn group(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let (name, members) = (read(vm, args, "name"), read(vm, args, "members"));
        intent(&c, vm, "group", || {
            let name = name.text().map(str::to_string).ok_or_else(|| format!("the group name must be a string, got {}", name.describe()))?;
            let list = members.list().ok_or_else(|| format!("members: expected a list, got {}", members.describe()))?;
            Ok(IntentRec::Group { name, members: list.iter().map(target).collect::<Result<_, _>>()? })
        })
    })
}

// ---- f.: part drawing ---------------------------------------------------------------------------

fn draw(c: &Shared, vm: &mut ScriptVm, callee: &str, make: impl FnOnce(&mut Ctx) -> Result<(LiveId, usize), String>) -> ScriptValue {
    let mut cx = c.borrow_mut();
    match make(&mut cx) {
        Ok((tag, ix)) => {
            drop(cx);
            handle(vm, tag, ix).into()
        }
        Err(m) => {
            let h = here_in_project(vm, &cx);
            fail(&mut cx, &h, format!("f.{callee}: {m}"))
        }
    }
}

fn draw_at(c: &Shared, vm: &mut ScriptVm, callee: &str, make: impl FnOnce(&mut Ctx, &Here, &mut Here) -> Result<(LiveId, usize), String>) -> ScriptValue {
    let mut cx = c.borrow_mut();
    let h = here(vm, &cx);
    let mut at = h.clone();
    match make(&mut cx, &h, &mut at) {
        Ok((tag, ix)) => {
            drop(cx);
            handle(vm, tag, ix).into()
        }
        Err(m) => {
            if at.doc == h.doc && at.span == h.span {
                at = here_in_project(vm, &cx);
            }
            fail(&mut cx, &at, format!("f.{callee}: {m}"))
        }
    }
}

fn num(v: &V, what: &str) -> Result<f64, String> {
    v.num().ok_or_else(|| format!("{what}: expected a number, got {}", v.describe()))
}

fn pad_rotation(o: &Obj) -> Result<Angle, String> {
    match o.get("rot") {
        None => Ok(Angle::ZERO),
        Some(v) => v.num().filter(|v| v.is_finite()).map(Angle::rad)
            .ok_or_else(|| "rot: a finite angle is needed".into()),
    }
}

fn pad_outline(v: &V) -> Result<Shape, String> {
    use fab_elec_core::{geom::orient, BBox};
    let error = "outline: at least three [x, y] points without self-intersection";
    let mut points = v.list().ok_or(error)?.iter().map(|v| match v.list() {
        Some([x, y]) => Some(P::new(finite_nm(x)?, finite_nm(y)?)),
        _ => None,
    }).collect::<Option<Vec<_>>>().ok_or(error)?;
    if points.len() > 1 && points.first() == points.last() {
        points.pop();
    }
    let n = points.len();
    if n < 3 {
        return Err(error.into());
    }
    for i in 0..n {
        let (a, b, next) = (points[i], points[(i + 1) % n], points[(i + 2) % n]);
        if a == b || (orient(a, b, next) == 0 && !BBox::new(a, next).contains(b)) {
            return Err(error.into());
        }
        for j in i + 1..n {
            if j == i + 1 || (i == 0 && j == n - 1) {
                continue;
            }
            let (c, d) = (points[j], points[(j + 1) % n]);
            if BBox::new(a, b).intersects(&BBox::new(c, d))
                && orient(a, b, c).signum() * orient(a, b, d).signum() <= 0
                && orient(c, d, a).signum() * orient(c, d, b).signum() <= 0 {
                return Err(error.into());
            }
        }
    }
    let polygon = PolySet::from_ring_nonzero(points);
    if polygon.is_empty() {
        return Err(error.into());
    }
    Ok(Shape::Poly(polygon))
}

fn smd(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let (no, at, size, opts) = (read(vm, args, "no"), read(vm, args, "at"), read(vm, args, "size"), read(vm, args, "opts"));
        draw_at(&c, vm, "smd", |cx, h, error_at| {
            let no = pad_no(&no).ok_or_else(|| format!("pad number: expected a string, got {}", no.describe()))?;
            let at = point(&at).ok_or_else(|| format!("{no}: at: expected [x, y], got {}", at.describe()))?;
            let size = point(&size).ok_or_else(|| format!("{no}: size: expected [w, h], got {}", size.describe()))?;
            let mut pad = crate::prims::smd(&no, at, size);
            // `{shape: @circle mask: 0.05mm}`: the land's shape (rounded by default) and its own
            // solder-mask margin.
            if let Some(o) = opts_of(&opts)? {
                check_opts_at(o, &["shape", "mask", "rot", "ratio", "outline"], cx, h, 3, &[], error_at)?;
                if let Some(shape) = o.get("shape") {
                    *error_at = option_here(cx, h, 3, &["shape"]);
                    pad.stack.front = match shape.id() {
                        Some("roundrect") => pad.stack.front,
                        Some("rect") => PadShape::Rect { size },
                        Some("oval") => PadShape::Oval { size },
                        Some("circle") if size.x == size.y => PadShape::Circle { d: size.x },
                        Some("circle") => return Err(format!("{no}: a circle land has equal sides, got [w, h] unequal")),
                        _ => return Err(format!("{no}: shape: {} is not @roundrect, @rect, @oval or @circle", shape.describe())),
                    };
                }
                *error_at = option_here(cx, h, 3, &["rot"]);
                pad.rot = pad_rotation(o)?;
                if let Some(ratio) = o.get("ratio") {
                    *error_at = option_here(cx, h, 3, &["ratio"]);
                    if !matches!(pad.stack.front, PadShape::RoundRect { .. }) {
                        return Err("ratio: only an @roundrect land takes a ratio".into());
                    }
                    let ratio = ratio.num().filter(|r| r.is_finite() && (0.0..=0.5).contains(r))
                        .ok_or("ratio: a fraction from 0 to 0.5")?;
                    pad.stack.front = PadShape::RoundRect { size, ratio_ppm: (ratio * 1e6).round() as u32 };
                }
                if let Some(outline) = o.get("outline") {
                    *error_at = option_here(cx, h, 3, &["outline"]);
                    if !matches!(pad.stack.front, PadShape::Rect { .. } | PadShape::Circle { .. }) {
                        return Err("outline: the anchor shape must be @rect or @circle".into());
                    }
                    pad.stack.front = PadShape::Custom { anchor: Box::new(pad.stack.front), shapes: vec![pad_outline(outline)?] };
                }
                if let Some(m) = o.get("mask") {
                    *error_at = option_here(cx, h, 3, &["mask"]);
                    pad.mask_margin = Some(nm(num(m, "mask")?));
                }
            }
            cx.pads.push(pad);
            Ok((id_lut!(Pad), cx.pads.len() - 1))
        })
    })
}

fn thru(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let (no, at, d, drill, opts) = (read(vm, args, "no"), read(vm, args, "at"), read(vm, args, "d"), read(vm, args, "drill"), read(vm, args, "opts"));
        draw_at(&c, vm, "thru", |cx, h, error_at| {
            let no = pad_no(&no).ok_or_else(|| format!("pad number: expected a string, got {}", no.describe()))?;
            let at = point(&at).ok_or_else(|| format!("{no}: at: expected [x, y], got {}", at.describe()))?;
            let (mut square, mut plated) = (false, true);
            let mut rot = Angle::ZERO;
            if let Some(o) = opts_of(&opts)? {
                check_opts_at(o, &["square", "plated", "rot"], cx, h, 4, &[], error_at)?;
                square = o.get("square").and_then(V::bool).unwrap_or(false);
                plated = o.get("plated").and_then(V::bool).unwrap_or(true);
                *error_at = option_here(cx, h, 4, &["rot"]);
                rot = pad_rotation(o)?;
            }
            let size = |v: &V, name: &str| -> Result<P, String> {
                match v {
                    V::Num(_) => finite_nm(v).map(|d| P::new(d, d)),
                    V::Arr(v) => match v.as_slice() {
                        [w, h] => finite_nm(w).zip(finite_nm(h)).map(|(w, h)| P::new(w, h)),
                        _ => None,
                    },
                    _ => None,
                }.ok_or_else(|| format!("{name}: a length or [w, h] lengths"))
            };
            *error_at = option_here(cx, h, 2, &[]);
            let land = size(&d, "d")?;
            *error_at = option_here(cx, h, 3, &[]);
            let drill = size(&drill, "drill")?;
            let mut pad = crate::prims::thru(&no, at, land.x, drill.x, square);
            pad.rot = rot;
            pad.drill = Some(fab_elec_parts::Drill { size: drill, offset: P::ZERO });
            pad.stack.front = if square {
                PadShape::Rect { size: land }
            } else if d.list().is_some() {
                PadShape::Oval { size: land }
            } else {
                PadShape::Circle { d: land.x }
            };
            // An unplated hole has no copper: its land is the hole.
            if !plated {
                pad.kind = PadKind::Npth;
                pad.stack.copper = LayerSet::EMPTY;
                pad.stack.front = if drill.x == drill.y { PadShape::Circle { d: drill.x } } else { PadShape::Oval { size: drill } };
            }
            cx.pads.push(pad);
            Ok((id_lut!(Pad), cx.pads.len() - 1))
        })
    })
}

fn footprint(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let spec = read(vm, args, "spec");
        draw(&c, vm, "footprint", |cx| {
            let o = spec.obj().ok_or_else(|| format!("expected {{pads body mount text graphics}}, got {}", spec.describe()))?;
            check_opts(o, &["pads", "body", "mount", "text", "graphics"])?;
            let pads = o.get("pads").and_then(V::list).ok_or("pads: expected a list of f.smd(..) and f.thru(..)")?;
            let pads = pads.iter().map(|p| match p.handle() {
                Some(Handle::Pad(i)) => Ok(cx.pads[i].clone()),
                _ => Err(format!("pads: expected a pad, got {}", p.describe())),
            }).collect::<Result<Vec<_>, _>>()?;
            let body = o.get("body").and_then(V::list).filter(|b| b.len() == 2).ok_or("body: expected [[x0, y0], [x1, y1]]")?;
            let (min, max) = (point(&body[0]).ok_or("body: expected [[x0, y0], [x1, y1]]")?, point(&body[1]).ok_or("body: expected [[x0, y0], [x1, y1]]")?);
            let mount = match o.get("mount").and_then(V::text_or_id) {
                None | Some("smd") => FpMount::Smd,
                Some("thru") => FpMount::Thru,
                Some(m) => return Err(format!("mount: {m} is not @smd or @thru")),
            };
            let text = nm(o.get("text").and_then(V::num).unwrap_or(0.001));
            let mut extra = Vec::new();
            for g in o.get("graphics").and_then(V::list).unwrap_or(&[]) {
                match g.handle() {
                    Some(Handle::Gfx(i)) => extra.push(cx.gfx[i].clone()),
                    _ => return Err(format!("graphics: expected f.stroke(..) or f.dot(..), got {}", g.describe())),
                }
            }
            cx.fps.push(crate::prims::finish_with("", pads, min, max, mount, text, extra));
            Ok((id_lut!(Fp), cx.fps.len() - 1))
        })
    })
}

fn layer(v: &V) -> Result<LayerId, String> {
    let name = v.text().ok_or_else(|| format!("layer: expected a layer name (\"F.SilkS\"), got {}", v.describe()))?;
    LayerId::parse(name).ok_or_else(|| format!("layer: {name} is not a layer"))
}

fn stroke(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let (l, pts, w) = (read(vm, args, "layer"), read(vm, args, "points"), read(vm, args, "w"));
        draw(&c, vm, "stroke", |cx| {
            let l = layer(&l)?;
            let pts = pts.list().ok_or("points: expected a list of [x, y]")?;
            let pts = pts.iter().map(|p| point(p).ok_or_else(|| format!("points: expected [x, y], got {}", p.describe()))).collect::<Result<Vec<_>, _>>()?;
            let Some((first, rest)) = pts.split_first() else { return Err("points: needs two points or more".into()) };
            if rest.is_empty() {
                return Err("points: needs two points or more".into());
            }
            let path = rest.iter().fold(Path::new(*first), |p, q| p.line(*q));
            cx.gfx.push((l, Gfx::Stroke { path, w: nm(num(&w, "w")?) }));
            Ok((id_lut!(Gfx), cx.gfx.len() - 1))
        })
    })
}

fn dot(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let (l, at, r) = (read(vm, args, "layer"), read(vm, args, "at"), read(vm, args, "r"));
        draw(&c, vm, "dot", |cx| {
            let l = layer(&l)?;
            let at = point(&at).ok_or_else(|| format!("at: expected [x, y], got {}", at.describe()))?;
            cx.gfx.push((l, Gfx::Fill(Shape::Circle { c: at, r: nm(num(&r, "r")?) })));
            Ok((id_lut!(Gfx), cx.gfx.len() - 1))
        })
    })
}

/// `[[pin, kind], ..]` as names and kinds.
fn pin_list(v: &V, what: &str) -> Result<Vec<(String, PinKind)>, String> {
    let list = v.list().ok_or_else(|| format!("{what}: expected a list of [pin, kind], got {}", v.describe()))?;
    list.iter().map(|item| match item.list() {
        Some([name, kind]) => {
            let name = name.text().ok_or_else(|| format!("{what}: a pin name is a string, got {}", name.describe()))?;
            let k = kind.text_or_id().and_then(pin_kind).ok_or_else(|| format!("{what}: {name}: {} is not a pin kind", kind.describe()))?;
            Ok((name.to_string(), k))
        }
        _ => Err(format!("{what}: each pin is [name, kind], got {}", item.describe())),
    }).collect()
}

fn ic_symbol(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let sides = read(vm, args, "sides");
        draw(&c, vm, "ic_symbol", |cx| {
            let o = sides.obj().ok_or_else(|| format!("sides: expected {{left: [..] right: [..] top: [..] bottom: [..]}}, got {}", sides.describe()))?;
            check_opts(o, &["left", "right", "top", "bottom"])?;
            // The part's pins are the sides' pins in the order the sides are written.
            let mut pins: Vec<(String, PinKind)> = Vec::new();
            let mut at: [Vec<PinIx>; 4] = Default::default();
            for (side, v) in &o.fields {
                let k = ["left", "right", "top", "bottom"].iter().position(|s| s == side).expect("checked above");
                for (name, kind) in pin_list(v, side)? {
                    if pins.iter().any(|(n, _)| *n == name) {
                        return Err(format!("pin {name} is on two sides"));
                    }
                    at[k].push(PinIx(pins.len() as u16));
                    pins.push((name, kind));
                }
            }
            let symbol = crate::prims::ic_box(&at[0], &at[1], &at[2], &at[3]);
            cx.syms.push(SymRec { symbol, pins });
            Ok((id_lut!(Sym), cx.syms.len() - 1))
        })
    })
}

fn two_terminal(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let pins = read(vm, args, "pins");
        draw(&c, vm, "two_terminal", |cx| {
            let pins = pin_list(&pins, "pins")?;
            if pins.len() != 2 {
                return Err(format!("pins: a two-terminal symbol has two pins, {} are given", pins.len()));
            }
            cx.syms.push(SymRec { symbol: crate::prims::two_terminal(), pins });
            Ok((id_lut!(Sym), cx.syms.len() - 1))
        })
    })
}

/// `f.opamp_symbol(pins)`: an op-amp's triangle; `pins` names its five pins in the symbol's
/// order (`prims::opamp`): non-inverting input, inverting input, output, positive and negative
/// supply, each `[name, kind]`.
fn opamp_symbol(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let pins = read(vm, args, "pins");
        draw(&c, vm, "opamp_symbol", |cx| {
            let pins = pin_list(&pins, "pins")?;
            if pins.len() != 5 {
                return Err(format!("pins: an op-amp symbol has five pins (+in, -in, out, +supply, -supply), {} are given", pins.len()));
            }
            cx.syms.push(SymRec { symbol: crate::prims::opamp(), pins });
            Ok((id_lut!(Sym), cx.syms.len() - 1))
        })
    })
}

fn int(_c: Shared) -> Native {
    Box::new(move |vm, args| {
        let n = arg(vm, args, id_lut!(n)).as_number().unwrap_or(0.0);
        vm.new_string(&format!("{}", n.round() as i64))
    })
}

/// A length as millimetre text, exact to the nanometre (`1.27mm` gives `"1.27"`).
fn mm_text(_c: Shared) -> Native {
    Box::new(move |vm, args| {
        let v = arg(vm, args, id_lut!(v)).as_number().unwrap_or(0.0);
        let n = nm(v) as i64;
        let (sign, n) = if n < 0 { ("-", -n) } else { ("", n) };
        let (whole, frac) = (n / 1_000_000, n % 1_000_000);
        let text = if frac == 0 {
            format!("{sign}{whole}")
        } else {
            format!("{sign}{whole}.{}", format!("{frac:06}").trim_end_matches('0'))
        };
        vm.new_string(&text)
    })
}

/// `v` rounded to a multiple of `step`, snapped to whole nanometres.
fn round(_c: Shared) -> Native {
    Box::new(move |vm, args| {
        let v = arg(vm, args, id_lut!(v)).as_number().unwrap_or(0.0);
        let step = arg(vm, args, id_lut!(step)).as_number().unwrap_or(0.0);
        if step <= 0.0 {
            return v.into();
        }
        // In whole nanometres first, so a length written as 0.875 mm rounds half away from zero
        // (88 steps of 0.01 mm), not by its binary fraction (87.49999..).
        let (v, step) = ((v * 1e9).round(), (step * 1e9).round().max(1.0));
        ((v / step).round() * step / 1e9).into()
    })
}

fn error(c: Shared) -> Native {
    Box::new(move |vm, args| {
        let m = read(vm, args, "message");
        let mut cx = c.borrow_mut();
        let h = here_in_project(vm, &cx);
        let text = m.text().map(str::to_string).unwrap_or_else(|| m.describe());
        fail(&mut cx, &h, text)
    })
}

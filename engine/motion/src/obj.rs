//! A document's component objects, read and made in its VM: what the
//! lowerings (procedural looks, templates, charts, exits) read from the
//! object the author wrote and the layers they make in its place.
//!
//! Values keep the shapes the loader reads: a vector is an array of
//! numbers (`v2`), a colour a packed `#rrggbbaa`, a name an `@id`; a made
//! layer is an object of its registered type ([`crate::layers::make`]).

use crate::model::{Diagnostic, Loc};
use crate::value::{fmt_num, truncate, SourceMap};
use makepad_script::pod::{ScriptPodVec};
use makepad_script::*;

/// The id of a field or `@name` (its text reads back).
pub(crate) fn key(name: &str) -> LiveId {
    LiveId::from_str_with_lut(name).unwrap_or_else(|_| LiveId::from_str(name))
}

/// `o`'s field `name` through its prototypes; `None` where no object of the
/// chain has it (an explicit `nil` is `Some(NIL)`).
pub(crate) fn opt(vm: &ScriptVm, o: ScriptObject, name: &str) -> Option<ScriptValue> {
    let k: ScriptValue = key(name).into();
    let v = vm.bx.heap_ref().value(o, k, NoTrap);
    if !v.is_err() {
        return Some(v);
    }
    // An error value the document wrote (a misspelt name) is a value; a
    // field no object of the chain has is none.
    let mut at = Some(o);
    let mut depth = 0;
    while let Some(level) = at {
        if vm.bx.heap_ref().map_entries(level).iter().any(|e| e.key == k) {
            return Some(v);
        }
        for i in 0..vm.bx.heap_ref().vec_len(level) {
            if vm.bx.heap_ref().vec_key_value(level, i, NoTrap).key == k {
                return Some(v);
            }
        }
        depth += 1;
        if depth > 64 {
            break;
        }
        at = vm.bx.heap_ref().proto(level).as_object();
    }
    None
}

/// `o`'s field `name`, nil where it has none.
pub(crate) fn get(vm: &ScriptVm, o: ScriptObject, name: &str) -> ScriptValue {
    opt(vm, o, name).unwrap_or(NIL)
}

/// The registered type a value was made from (`Rect`, `Tween`, `Beat`, …).
pub(crate) fn type_name(vm: &ScriptVm, v: ScriptValue) -> Option<&'static str> {
    crate::layers::type_of(vm, v.as_object()?)
}

pub(crate) fn is(vm: &ScriptVm, v: ScriptValue, ty: &str) -> bool {
    type_name(vm, v) == Some(ty)
}

pub(crate) fn is_fn(vm: &ScriptVm, v: ScriptValue) -> bool {
    v.as_object().is_some_and(|o| vm.bx.heap_ref().is_fn(o))
}

/// A finite number (a one-lane float vector reads as one).
pub(crate) fn num(vm: &ScriptVm, v: ScriptValue) -> Option<f64> {
    if let Some(n) = v.as_number() {
        return n.is_finite().then_some(n);
    }
    let pod = v.as_pod()?;
    let (ty, data) = vm.bx.heap_ref().pod_data(pod);
    match ty.ty().as_vec() {
        None if ty.ty().kind() == makepad_script::pod::ScriptPodTyTag::F32 => Some(f32::from_bits(*data.first()?) as f64),
        _ => None,
    }
}

/// A string's text.
pub(crate) fn string(vm: &ScriptVm, v: ScriptValue) -> Option<String> {
    if !v.is_string_like() {
        return None;
    }
    if let Some(s) = v.as_string() {
        return Some(vm.bx.heap_ref().string(s).to_string());
    }
    v.as_inline_string(|s| s.to_string())
}

/// An `@id`'s text.
pub(crate) fn ident(v: ScriptValue) -> Option<String> {
    v.as_id().map(id_text)
}

pub(crate) fn id_text(id: LiveId) -> String {
    id.as_string(|s| s.map(str::to_string)).unwrap_or_else(|| format!("{id}"))
}

/// A string's or an `@id`'s text.
pub(crate) fn name(vm: &ScriptVm, v: ScriptValue) -> Option<String> {
    string(vm, v).or_else(|| ident(v))
}

/// The lanes of a Splash float vector (`vec2(1, 2)`), as the numbers of
/// an array.
fn pod_lanes(vm: &ScriptVm, v: ScriptValue) -> Option<Vec<f64>> {
    let pod = v.as_pod()?;
    let (ty, data) = vm.bx.heap_ref().pod_data(pod);
    let lanes = match ty.ty().as_vec() {
        Some(ScriptPodVec::Vec2f) => 2,
        Some(ScriptPodVec::Vec3f) => 3,
        Some(ScriptPodVec::Vec4f) => 4,
        _ => return None,
    };
    Some(data.iter().take(lanes).map(|bits| f32::from_bits(*bits) as f64).collect())
}

/// The items of an array (a float vector's lanes as numbers, an object
/// written as a list as its items).
pub(crate) fn items(vm: &ScriptVm, v: ScriptValue) -> Option<Vec<ScriptValue>> {
    if let Some(a) = v.as_array() {
        return Some((0..vm.bx.heap_ref().array_len(a)).map(|i| vm.bx.heap_ref().array_index(a, i, NoTrap)).collect());
    }
    if let Some(lanes) = pod_lanes(vm, v) {
        return Some(lanes.into_iter().map(ScriptValue::from).collect());
    }
    let o = v.as_object()?;
    if vm.bx.heap_ref().is_fn(o) || type_name(vm, v).is_some() {
        return None;
    }
    let mut list = Vec::new();
    let mut named = false;
    for index in 0..vm.bx.heap_ref().vec_len(o) {
        let entry = vm.bx.heap_ref().vec_key_value(o, index, NoTrap);
        if entry.key.is_nil() {
            list.push(entry.value);
        } else {
            named = true;
        }
    }
    (!named && vm.bx.heap_ref().map_len(o) == 0 && vm.bx.heap_ref().proto(o).as_object().is_none() && !list.is_empty()).then_some(list)
}

/// `N` numbers: a float vector or an array of numbers.
pub(crate) fn vecn<const N: usize>(vm: &ScriptVm, v: ScriptValue) -> Option<[f64; N]> {
    let items = items(vm, v)?;
    if items.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (slot, item) in out.iter_mut().zip(items) {
        *slot = num(vm, item)?;
    }
    Some(out)
}

/// Whether `v` is a plain object (not an array, a function or a value of
/// a registered type).
pub(crate) fn record_of(vm: &ScriptVm, v: ScriptValue) -> Option<ScriptObject> {
    let o = v.as_object()?;
    (!vm.bx.heap_ref().is_fn(o) && type_name(vm, v).is_none() && items(vm, v).is_none()).then_some(o)
}

/// What a value is, for messages: "the number 3", "a Rect{…}", "@id", …
pub(crate) fn kind(vm: &ScriptVm, v: ScriptValue) -> String {
    look(vm, v).kind()
}

/// `o`'s fields, its own first, then each prototype's it does not
/// override (up to its registered type), each with the object that wrote
/// it.
pub(crate) fn fields(vm: &ScriptVm, o: ScriptObject) -> Vec<(String, ScriptValue, ScriptObject)> {
    let mut out: Vec<(String, ScriptValue, ScriptObject)> = Vec::new();
    let mut at = Some(o);
    let mut depth = 0;
    while let Some(level) = at {
        depth += 1;
        if depth > 64 {
            break;
        }
        let heap = vm.heap();
        let mut level_fields: Vec<(String, ScriptValue)> = Vec::new();
        heap.map_entries(level).iter().for_each(|e| level_fields.push((key_text(e.key), e.value)));
        for index in 0..heap.vec_len(level) {
            let entry = heap.vec_key_value(level, index, NoTrap);
            if !entry.key.is_nil() {
                level_fields.push((key_text(entry.key), entry.value));
            }
        }
        for (k, v) in level_fields {
            if !out.iter().any(|(have, _, _)| *have == k) {
                out.push((k, v, level));
            }
        }
        at = heap.proto(level).as_object();
    }
    out
}

fn key_text(k: ScriptValue) -> String {
    if let Some(id) = k.as_id() {
        return id_text(id);
    }
    if let Some(n) = k.as_number() {
        return fmt_num(n);
    }
    if let Some(s) = k.as_inline_string(|s| s.to_string()) {
        return s;
    }
    format!("{k:?}")
}

// ---- making ------------------------------------------------------------

/// An `@id`.
pub(crate) fn id(name: &str) -> ScriptValue {
    ScriptValue::from_id(key(name))
}

pub(crate) fn color(c: u32) -> ScriptValue {
    ScriptValue::from_color(c)
}

pub(crate) fn text(vm: &mut ScriptVm, s: &str) -> ScriptValue {
    vm.bx.heap_mut().new_string_from_str(s)
}

pub(crate) fn array(vm: &mut ScriptVm, values: impl IntoIterator<Item = ScriptValue>) -> ScriptValue {
    let a = vm.bx.heap_mut().new_array();
    for v in values {
        vm.bx.heap_mut().array_push(a, v, NoTrap);
    }
    a.into()
}

pub(crate) fn v2(vm: &mut ScriptVm, x: f64, y: f64) -> ScriptValue {
    array(vm, [x.into(), y.into()])
}

/// A plain object of `fields`, in order.
pub(crate) fn record(vm: &mut ScriptVm, fields: &[(&str, ScriptValue)]) -> ScriptValue {
    let o = vm.bx.heap_mut().new_object();
    for (k, v) in fields {
        set(vm, o, k, *v);
    }
    o.into()
}

/// A new object of registered type `ty` with `fields`.
pub(crate) fn make(vm: &mut ScriptVm, ty: &str, fields: &[(&str, ScriptValue)]) -> ScriptValue {
    let o = crate::layers::make(vm, ty, &[]);
    for (k, v) in fields {
        set(vm, o, k, *v);
    }
    o.into()
}

/// Set `o`'s own field `name` (a plain object keeps its fields in the
/// order they are set).
pub(crate) fn set(vm: &mut ScriptVm, o: ScriptObject, name: &str, v: ScriptValue) {
    let checked = crate::layers::type_of(vm, o).is_some_and(crate::layers::is_component);
    if checked {
        vm.bx.heap_mut().set_value_def(o, key(name).into(), v);
    } else {
        vm.bx.heap_mut().set_value_vec(o, key(name).into(), v, NoTrap);
    }
}

/// A copy of `o` as one object of its registered type (its prototypes'
/// fields read into it) without the fields `except`, made where `o` was.
pub(crate) fn copy_without(vm: &mut ScriptVm, o: ScriptObject, except: &[&str]) -> ScriptObject {
    let ty = crate::layers::type_of(vm, o);
    let fields: Vec<(String, ScriptValue, ScriptObject)> = fields(vm, o);
    let copy = match ty {
        Some(ty) => crate::layers::make(vm, ty, &[]),
        None => vm.bx.heap_mut().new_object(),
    };
    // Fields of the registered type itself (its defaults) stay inherited.
    let proto = vm.bx.heap_ref().proto(copy).as_object();
    for (k, v, level) in fields {
        if except.contains(&k.as_str()) || Some(level) == proto {
            continue;
        }
        set(vm, copy, &k, v);
    }
    let at = vm.bx.heap_ref().made_at(o);
    vm.bx.heap_mut().set_made_at(copy, at);
    copy
}

/// A copy of `o` with `name` set to `v` (the rest as [`copy_without`]).
pub(crate) fn with(vm: &mut ScriptVm, o: ScriptObject, name: &str, v: ScriptValue) -> ScriptObject {
    let copy = copy_without(vm, o, &[name]);
    set(vm, copy, name, v);
    copy
}

/// A time moved by `by` seconds: a number, or a time reference with its
/// `offset` moved.
pub(crate) fn shift(vm: &mut ScriptVm, t: ScriptValue, by: f64) -> ScriptValue {
    if let Some(n) = t.as_number() {
        return (n + by).into();
    }
    match (t.as_object(), type_name(vm, t)) {
        (Some(o), Some(ty)) if crate::value::TIMES.contains(&ty) => {
            let offset = num(vm, get(vm, o, "offset")).unwrap_or(0.0) + by;
            with(vm, o, "offset", offset.into()).into()
        }
        _ => t,
    }
}

/// `tween(from, to, start, start + dur, ease)`.
pub(crate) fn tween(vm: &mut ScriptVm, from: ScriptValue, to: ScriptValue, start: ScriptValue, dur: f64, ease: &str) -> ScriptValue {
    let end = shift(vm, start, dur);
    make(vm, "Tween", &[("from", from), ("to", to), ("start", start), ("end", end), ("ease", id(ease))])
}

/// `keys([t, v], [t, v, @ease], …)` of `points` (time, value, ease).
pub(crate) fn keys(vm: &mut ScriptVm, points: Vec<(ScriptValue, ScriptValue, Option<&str>)>) -> ScriptValue {
    let mut list = Vec::with_capacity(points.len());
    for (t, v, ease) in points {
        let mut p = vec![t, v];
        if let Some(e) = ease {
            p.push(id(e));
        }
        list.push(array(vm, p));
    }
    let points = array(vm, list);
    make(vm, "Keys", &[("points", points)])
}

// ---- where -------------------------------------------------------------

/// Where `o` was made, in the document (`None` for an object made in a
/// module or from Rust).
pub(crate) fn loc(vm: &ScriptVm, map: &SourceMap, o: ScriptObject) -> Option<Loc> {
    let at = vm.bx.code_ref().ip_to_loc(vm.bx.heap_ref().made_at(o))?;
    map.owns(at.file()).then(|| map.map(at.line(), at.col()))
}

/// Where `o`'s field `name` is written: in the object of its chain that
/// sets it, else where `o` was made.
pub(crate) fn field_loc(vm: &ScriptVm, map: &SourceMap, o: ScriptObject, name: &str) -> Option<Loc> {
    let owner = fields(vm, o).into_iter().find(|(k, _, _)| k == name).map(|(_, _, level)| level).unwrap_or(o);
    let made = vm.bx.heap_ref().made_at(owner);
    if let Some(at) = vm.bx.code_ref().field_loc(made, key(name)) {
        if map.owns(at.file()) {
            return Some(map.map(at.line(), at.col()));
        }
    }
    loc(vm, map, owner).or_else(|| loc(vm, map, o))
}

/// Diagnostics of a pass over the document in its VM, each where it is
/// written and at its property path (`layers[2].count`).
pub(crate) struct Diags<'m> {
    pub map: &'m SourceMap,
    pub out: Vec<Diagnostic>,
}

impl Diags<'_> {
    pub fn error(&mut self, vm: &ScriptVm, o: ScriptObject, path: &str, field: &str, message: String, hint: Option<&str>) {
        let loc = field_loc(vm, self.map, o, field).unwrap_or(Loc { line: 1, col: 1 });
        let path = if field.is_empty() { path.to_string() } else if path.is_empty() { field.to_string() } else { format!("{path}.{field}") };
        let diag = Diagnostic::error(loc, path, message);
        self.out.push(match hint {
            Some(h) => diag.hint(h),
            None => diag,
        });
    }

    pub fn warning(&mut self, vm: &ScriptVm, o: ScriptObject, path: &str, field: &str, message: String, hint: Option<&str>) {
        let loc = field_loc(vm, self.map, o, field).unwrap_or(Loc { line: 1, col: 1 });
        let path = if field.is_empty() { path.to_string() } else if path.is_empty() { field.to_string() } else { format!("{path}.{field}") };
        let diag = Diagnostic::warning(loc, path, message);
        self.out.push(match hint {
            Some(h) => diag.hint(h),
            None => diag,
        });
    }
}

/// A field's value where a lowering writes it: a literal, made in the VM
/// when the object is ([`make_lit`], [`record_lit`]).
pub(crate) enum Lit {
    Num(f64),
    Str(String),
    Id(&'static str),
    Color(u32),
    V2([f64; 2]),
    Bool(bool),
    /// A `Font{family weight italic axes}` value.
    Font(crate::font_spec::FontSpec),
    Value(ScriptValue),
}

/// A `Font{…}` literal of `family` at `weight`.
pub(crate) fn face(family: &'static str, weight: f64, italic: bool) -> Lit {
    Lit::Font(crate::font_spec::FontSpec { family, weight, italic, axes: Vec::new() })
}

/// A `Font{…}` value.
pub(crate) fn font(vm: &mut ScriptVm, face: &crate::font_spec::FontSpec) -> ScriptValue {
    let family = text(vm, face.family);
    let mut fields = vec![("family", family), ("weight", face.weight.into()), ("italic", face.italic.into())];
    if !face.axes.is_empty() {
        let axes: Vec<(&str, ScriptValue)> = face.axes.iter().map(|(tag, v)| (tag.as_str(), (*v).into())).collect();
        fields.push(("axes", record(vm, &axes)));
    }
    make(vm, "Font", &fields)
}

impl From<f64> for Lit {
    fn from(n: f64) -> Self {
        Lit::Num(n)
    }
}

impl From<&str> for Lit {
    fn from(s: &str) -> Self {
        Lit::Str(s.to_string())
    }
}

impl From<String> for Lit {
    fn from(s: String) -> Self {
        Lit::Str(s)
    }
}

impl From<ScriptValue> for Lit {
    fn from(v: ScriptValue) -> Self {
        Lit::Value(v)
    }
}

impl From<bool> for Lit {
    fn from(b: bool) -> Self {
        Lit::Bool(b)
    }
}

pub(crate) fn lit(vm: &mut ScriptVm, l: Lit) -> ScriptValue {
    match l {
        Lit::Num(n) => n.into(),
        Lit::Str(s) => text(vm, &s),
        Lit::Id(i) => id(i),
        Lit::Color(c) => color(c),
        Lit::V2([x, y]) => v2(vm, x, y),
        Lit::Bool(b) => b.into(),
        Lit::Font(f) => font(vm, &f),
        Lit::Value(v) => v,
    }
}

/// [`make`] of literals.
pub(crate) fn make_lit(vm: &mut ScriptVm, ty: &str, fields: Vec<(&str, Lit)>) -> ScriptValue {
    let o = crate::layers::make(vm, ty, &[]);
    for (k, l) in fields {
        let v = lit(vm, l);
        set(vm, o, k, v);
    }
    o.into()
}

/// [`record`] of literals.
pub(crate) fn record_lit(vm: &mut ScriptVm, fields: Vec<(&str, Lit)>) -> ScriptValue {
    let o = vm.bx.heap_mut().new_object();
    for (k, l) in fields {
        let v = lit(vm, l);
        set(vm, o, k, v);
    }
    o.into()
}
/// One value as the loader matches on it: shallow (a list's items and an
/// object's fields stay values to look at in turn).
#[derive(Clone, Debug)]
pub(crate) enum Look {
    Nil,
    Bool(bool),
    Num(f64),
    Str(String),
    Id(String),
    Color(u32),
    /// A list, a float vector's lanes, or an object written as a list,
    /// its items looked at.
    Arr(Vec<Look>),
    /// An object and the registered type it was made from.
    Obj(ScriptObject, Option<&'static str>),
    Fn(ScriptObject),
    /// An error value or a vector of a kind Motion does not use.
    Other(String),
}

impl Look {
    /// A finite number.
    pub fn num(&self) -> Option<f64> {
        match self {
            Look::Num(n) if n.is_finite() => Some(*n),
            _ => None,
        }
    }

    pub fn is(&self, ty: &str) -> bool {
        matches!(self, Look::Obj(_, Some(t)) if *t == ty)
    }

    pub fn obj(&self) -> Option<ScriptObject> {
        match self {
            Look::Obj(o, _) => Some(*o),
            _ => None,
        }
    }

    /// The type an object was made from.
    pub fn proto(&self) -> Option<&'static str> {
        match self {
            Look::Obj(_, ty) => *ty,
            _ => None,
        }
    }

    /// An animation's kind (`Tween`, `Keys`, `Sequence`, `Loop`).
    pub fn anim(&self) -> Option<&'static str> {
        self.proto().filter(|p| crate::value::ANIMS.contains(p))
    }

    /// A time reference's kind (`Beat`, …, `Beats`).
    pub fn time_ref(&self) -> Option<&'static str> {
        self.proto().filter(|p| crate::value::TIMES.contains(p))
    }

    /// What the value is, for messages: "the number 3", "a Rect{…}", …
    pub fn kind(&self) -> String {
        match self {
            Look::Nil => "nil".into(),
            Look::Bool(_) => "a bool".into(),
            Look::Num(n) => format!("the number {}", fmt_num(*n)),
            Look::Str(s) => format!("the string {:?}", truncate(s, 40)),
            Look::Id(id) => format!("@{id}"),
            Look::Color(_) => "a colour".into(),
            Look::Arr(items) => format!("an array of {}", items.len()),
            Look::Obj(_, Some(ty)) => format!("a {ty}{{…}}"),
            Look::Obj(_, None) => "an object".into(),
            Look::Fn(_) => "a function".into(),
            Look::Other(kind) => format!("an unusable value ({kind})"),
        }
    }
}

pub(crate) fn look(vm: &ScriptVm, v: ScriptValue) -> Look {
    look_at(vm, v, 0)
}

fn look_at(vm: &ScriptVm, v: ScriptValue, depth: usize) -> Look {
    if v.is_nil() {
        return Look::Nil;
    }
    if let Some(b) = v.as_bool() {
        return Look::Bool(b);
    }
    if let Some(n) = v.as_number() {
        return Look::Num(n);
    }
    if let Some(c) = v.as_color() {
        return Look::Color(c);
    }
    if let Some(pod) = v.as_pod() {
        let (ty, data) = vm.bx.heap_ref().pod_data(pod);
        return match ty.ty().as_vec() {
            None if ty.ty().kind() == makepad_script::pod::ScriptPodTyTag::F32 => Look::Num(data.first().map_or(0.0, |b| f32::from_bits(*b) as f64)),
            _ => match pod_lanes(vm, v) {
                Some(lanes) => Look::Arr(lanes.into_iter().map(Look::Num).collect()),
                None => Look::Other("a vector of a kind motion does not use".into()),
            },
        };
    }
    if let Some(id) = ident(v) {
        return Look::Id(id);
    }
    if let Some(s) = string(vm, v) {
        return Look::Str(s);
    }
    if let Some(items) = items(vm, v) {
        if depth >= 64 {
            return Look::Other("nesting too deep".into());
        }
        return Look::Arr(items.into_iter().map(|x| look_at(vm, x, depth + 1)).collect());
    }
    if let Some(o) = v.as_object() {
        if vm.bx.heap_ref().is_fn(o) {
            return Look::Fn(o);
        }
        return Look::Obj(o, type_name(vm, v));
    }
    Look::Other(format!("{:?}", v.value_type()))
}

/// `o`'s field `name`, looked at (`None` where it has none).
pub(crate) fn look_field(vm: &ScriptVm, o: ScriptObject, name: &str) -> Option<Look> {
    opt(vm, o, name).map(|v| look(vm, v))
}

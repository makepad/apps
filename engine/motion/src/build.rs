//! What the lowerings ([`crate::lower`]) build with: reading the author's
//! object (its fields, with the messages the loader gives) and making the
//! layers it lowers to in the document's VM.

use crate::obj::{self, Lit};
use makepad_script::*;

/// What a lowering made, or the field it failed at and why.
pub(crate) type Made = Result<ScriptValue, (String, String)>;

/// The document's frame and clock a lowering sizes and times things by,
/// and the VM it builds in.
pub(crate) struct Cx<'a, 'b> {
    pub vm: &'a mut ScriptVm<'b>,
    pub doc: [f64; 2],
    pub bpm: f64,
    pub duration: f64,
}

// ---- reading --------------------------------------------------------------

pub(crate) fn kind(cx: &Cx, v: ScriptValue) -> String {
    obj::kind(cx.vm, v)
}

fn given(cx: &Cx, o: ScriptObject, key: &str) -> Option<ScriptValue> {
    obj::opt(cx.vm, o, key).filter(|v| !v.is_nil())
}

pub(crate) fn get_num(cx: &Cx, o: ScriptObject, key: &str, default: f64) -> Result<f64, (String, String)> {
    match given(cx, o, key) {
        None => Ok(default),
        Some(v) => match obj::num(cx.vm, v) {
            Some(n) => Ok(n),
            None => Err((key.into(), format!("`{key}` is a number, not {}", kind(cx, v)))),
        },
    }
}

pub(crate) fn get_color(cx: &Cx, o: ScriptObject, key: &str, default: u32) -> Result<u32, (String, String)> {
    match given(cx, o, key) {
        None => Ok(default),
        Some(v) => match v.as_color() {
            Some(c) => Ok(c),
            None => Err((key.into(), format!("`{key}` is a colour (#rrggbb), not {}", kind(cx, v)))),
        },
    }
}

pub(crate) fn get_colors(cx: &Cx, o: ScriptObject, key: &str, default: &[u32]) -> Result<Vec<u32>, (String, String)> {
    let Some(v) = given(cx, o, key) else { return Ok(default.to_vec()) };
    if let Some(c) = v.as_color() {
        return Ok(vec![c]);
    }
    match obj::items(cx.vm, v).filter(|items| !items.is_empty()) {
        Some(items) => items
            .into_iter()
            .map(|x| x.as_color().ok_or_else(|| (key.to_string(), format!("`{key}` is a list of colours, and one is {}", kind(cx, x)))))
            .collect(),
        None => Err((key.into(), format!("`{key}` is a list of colours [#…, #…], not {}", kind(cx, v)))),
    }
}

pub(crate) fn get_vec2(cx: &Cx, o: ScriptObject, key: &str, default: [f64; 2]) -> Result<[f64; 2], (String, String)> {
    let Some(v) = given(cx, o, key) else { return Ok(default) };
    if let Some(p) = obj::vecn::<2>(cx.vm, v) {
        return Ok(p);
    }
    if let Some(n) = v.as_number() {
        return Ok([n, n]);
    }
    Err((key.into(), format!("`{key}` is `vec2(x, y)`, not {}", kind(cx, v))))
}

pub(crate) fn get_bool(cx: &Cx, o: ScriptObject, key: &str, default: bool) -> bool {
    obj::opt(cx.vm, o, key).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// The face `key` names: a `Font{…}` value (a string is the old form, an
/// error saying what to write); `default` (a family at 400) when not written.
pub(crate) fn get_face(cx: &Cx, o: ScriptObject, key: &str, default: &str) -> Result<crate::font_spec::FontSpec, (String, String)> {
    match given(cx, o, key) {
        Some(v) => crate::font_spec::font_spec(cx.vm, v).map_err(|e| (key.to_string(), e)),
        None => Ok(crate::font_spec::FontSpec { family: crate::font_spec::font_or_error(default).map_err(|e| (key.to_string(), e))?, weight: 400.0, italic: false, axes: Vec::new() }),
    }
}

/// A `Font{…}` value for a made layer's `font`.
pub(crate) fn font_value(cx: &mut Cx, face: &crate::font_spec::FontSpec) -> ScriptValue {
    obj::font(cx.vm, face)
}

pub(crate) fn get_text(cx: &Cx, o: ScriptObject, key: &str) -> Option<String> {
    obj::opt(cx.vm, o, key).and_then(|v| obj::string(cx.vm, v))
}

/// The start time: `start` as written (seconds or a time reference), 0 by default.
pub(crate) fn get_start(cx: &Cx, o: ScriptObject) -> ScriptValue {
    given(cx, o, "start").unwrap_or(0.0.into())
}

/// A modifier's settings: an object of `allowed` fields (`None` for a
/// value that is not an object).
pub(crate) fn settings(cx: &Cx, spec: ScriptValue, field: &str, allowed: &[&str]) -> Result<Option<ScriptObject>, (String, String)> {
    let Some(o) = spec.as_object().filter(|o| !cx.vm.bx.heap_ref().is_fn(*o) && obj::items(cx.vm, spec).is_none()) else { return Ok(None) };
    for (k, _, _) in obj::fields(cx.vm, o) {
        if !allowed.contains(&k.as_str()) {
            return Err((field.into(), format!("`{field}` takes {}, not `{k}`", allowed.join(" "))));
        }
    }
    Ok(Some(o))
}

// ---- building blocks ----------------------------------------------------

pub(crate) fn layer(cx: &mut Cx, proto: &str, fields: Vec<(&str, Lit)>) -> ScriptValue {
    obj::make_lit(cx.vm, proto, fields)
}

pub(crate) fn record(cx: &mut Cx, fields: Vec<(&str, Lit)>) -> ScriptValue {
    obj::record_lit(cx.vm, fields)
}

pub(crate) fn array(cx: &mut Cx, items: Vec<ScriptValue>) -> ScriptValue {
    obj::array(cx.vm, items)
}

pub(crate) fn v2(x: f64, y: f64) -> Lit {
    Lit::V2([x, y])
}

/// `tween(from, to, start, start + dur, ease)` with `start` a time (a
/// number or a time reference).
pub(crate) fn tween(cx: &mut Cx, from: Lit, to: Lit, start: ScriptValue, dur: f64, ease: &str) -> ScriptValue {
    let (from, to) = (obj::lit(cx.vm, from), obj::lit(cx.vm, to));
    obj::tween(cx.vm, from, to, start, dur, ease)
}

/// `keys(…)` with times offset from `start`, linear between.
pub(crate) fn keys(cx: &mut Cx, start: ScriptValue, points: Vec<(f64, Lit)>) -> ScriptValue {
    let mut list = Vec::with_capacity(points.len());
    for (t, v) in points {
        let t = obj::shift(cx.vm, start, t);
        let v = obj::lit(cx.vm, v);
        list.push((t, v, None));
    }
    obj::keys(cx.vm, list)
}

/// A time `by` seconds after `start`.
pub(crate) fn at_time(cx: &mut Cx, start: ScriptValue, by: f64) -> ScriptValue {
    obj::shift(cx.vm, start, by)
}

/// The common layer fields of `o` (placement, span, name, …), to carry
/// onto what a type lowers to.
pub(crate) fn common(cx: &Cx, o: ScriptObject) -> Vec<(String, ScriptValue)> {
    obj::fields(cx.vm, o).into_iter().filter(|(k, _, _)| crate::layers::is_common(k)).map(|(k, v, _)| (k, v)).collect()
}

/// `made` with `o`'s common fields (its own `extra` fields win), made
/// where `o` was.
pub(crate) fn with_common(cx: &mut Cx, o: ScriptObject, made: ScriptValue, keep: &[&str]) -> ScriptValue {
    let Some(m) = made.as_object() else { return made };
    for (k, v) in common(cx, o) {
        if !keep.contains(&k.as_str()) {
            obj::set(cx.vm, m, &k, v);
        }
    }
    let at = cx.vm.bx.heap_ref().made_at(o);
    cx.vm.bx.heap_mut().set_made_at(m, at);
    made
}

/// The Group a type lowers to: its common fields, then `extra`, then `layers`.
pub(crate) fn group(cx: &mut Cx, o: ScriptObject, layers: Vec<ScriptValue>, extra: Vec<(&str, Lit)>) -> ScriptValue {
    let g = crate::layers::make(cx.vm, "Group", &[]);
    for (k, v) in common(cx, o) {
        obj::set(cx.vm, g, &k, v);
    }
    for (k, l) in extra {
        let v = obj::lit(cx.vm, l);
        obj::set(cx.vm, g, k, v);
    }
    let layers = obj::array(cx.vm, layers);
    obj::set(cx.vm, g, "layers", layers);
    let at = cx.vm.bx.heap_ref().made_at(o);
    cx.vm.bx.heap_mut().set_made_at(g, at);
    g.into()
}

/// A Group holding `layers` with `o`'s `span` (if it has one) and `name`,
/// made where `o` was: what a modifier wraps its layer in.
pub(crate) fn wrapper(cx: &mut Cx, o: ScriptObject, name: &str, mut fields: Vec<(&str, Lit)>, layers: Vec<ScriptValue>) -> ScriptValue {
    let mut all: Vec<(&str, Lit)> = Vec::new();
    if let Some(span) = obj::opt(cx.vm, o, "span") {
        all.push(("span", span.into()));
    }
    if !name.is_empty() {
        all.push(("name", name.into()));
    }
    all.append(&mut fields);
    let layers = obj::array(cx.vm, layers);
    all.push(("layers", layers.into()));
    let g = obj::make_lit(cx.vm, "Group", all);
    let at = cx.vm.bx.heap_ref().made_at(o);
    cx.vm.bx.heap_mut().set_made_at(g.as_object().unwrap(), at);
    g
}

/// The house mono label: tiny tracked caps.
pub(crate) fn label(cx: &mut Cx, s: &str, size: f64, color: u32, at: [f64; 2], align: &'static str, font: &crate::font_spec::FontSpec) -> ScriptValue {
    let font = font_value(cx, font);
    layer(cx, "Text", vec![
        ("text", s.into()),
        ("font", font.into()),
        ("size", size.into()),
        ("color", Lit::Color(color)),
        ("tracking", (size * 0.12).into()),
        ("at", v2(at[0], at[1])),
        ("align", Lit::Id(align)),
    ])
}


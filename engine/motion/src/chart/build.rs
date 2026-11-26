//! Building the chart's layers in the document's VM, the way the author
//! would have written them: `Rect{…}`, `tween(…)`, `keys(…)`, time
//! references. Builders nest (a layer's fields are built in its call), so
//! the VM is shared through [`B`].

use crate::obj;
use makepad_script::*;
use std::cell::RefCell;

/// The document's VM the chart builds in, shared by the builders.
pub(crate) struct B<'a, 'b> {
    vm: RefCell<&'a mut ScriptVm<'b>>,
}

impl<'a, 'b> B<'a, 'b> {
    pub fn new(vm: &'a mut ScriptVm<'b>) -> Self {
        Self { vm: RefCell::new(vm) }
    }

    /// Run `f` with the VM (not from inside another builder's `with`).
    pub fn with<R>(&self, f: impl FnOnce(&mut ScriptVm) -> R) -> R {
        let mut vm = self.vm.borrow_mut();
        f(&mut vm)
    }
}

pub(crate) fn num(n: f64) -> ScriptValue {
    n.into()
}

pub(crate) fn v2(b: &B, x: f64, y: f64) -> ScriptValue {
    b.with(|vm| obj::v2(vm, x, y))
}

pub(crate) fn s(b: &B, text: &str) -> ScriptValue {
    b.with(|vm| obj::text(vm, text))
}

/// A `Font{family weight}` value.
pub(crate) fn font_face(b: &B, family: &str, weight: f64) -> ScriptValue {
    b.with(|vm| {
        let family = obj::text(vm, family);
        obj::make(vm, "Font", &[("family", family), ("weight", weight.into())])
    })
}

pub(crate) fn id(name: &str) -> ScriptValue {
    obj::id(name)
}

pub(crate) fn color(c: u32) -> ScriptValue {
    obj::color(c)
}

pub(crate) fn boolean(v: bool) -> ScriptValue {
    v.into()
}

/// A list of values.
pub(crate) fn list(b: &B, items: Vec<ScriptValue>) -> ScriptValue {
    b.with(|vm| obj::array(vm, items))
}

/// An object of registered type `proto` (or a plain one) with its fields.
pub(crate) fn obj(b: &B, proto: Option<&str>, fields: Vec<(&str, ScriptValue)>) -> ScriptValue {
    b.with(|vm| match proto {
        Some(p) => obj::make(vm, p, &fields),
        None => obj::record(vm, &fields),
    })
}

/// A layer with its fields.
pub(crate) fn layer(b: &B, kind: &str, fields: Vec<(&str, ScriptValue)>) -> ScriptValue {
    obj(b, Some(kind), fields)
}

/// Sets (or replaces) `field` of an object value.
pub(crate) fn set(b: &B, value: ScriptValue, field: &str, to: ScriptValue) {
    if let Some(o) = value.as_object() {
        b.with(|vm| obj::set(vm, o, field, to));
    }
}

pub(crate) fn tween(b: &B, from: ScriptValue, to: ScriptValue, start: ScriptValue, end: ScriptValue, ease: ScriptValue) -> ScriptValue {
    layer(b, "Tween", vec![("from", from), ("to", to), ("start", start), ("end", end), ("ease", ease)])
}

/// `keys([t, v], [t, v, ease], …)`; one point is the constant value.
pub(crate) fn keys(b: &B, points: Vec<(ScriptValue, ScriptValue)>, ease: ScriptValue) -> ScriptValue {
    if points.len() == 1 {
        return points[0].1;
    }
    let items: Vec<ScriptValue> = points.into_iter().enumerate().map(|(i, (t, v))| if i == 0 { list(b, vec![t, v]) } else { list(b, vec![t, v, ease]) }).collect();
    let points = list(b, items);
    layer(b, "Keys", vec![("points", points)])
}

/// A fade from 0 to 1 over `[t - len, t]`.
pub(crate) fn fade_in(b: &B, t: ScriptValue, len: f64) -> ScriptValue {
    let from = shift(b, t, -len);
    tween(b, num(0.0), num(1.0), from, t, id("ease_out_cubic"))
}

/// A time moved by `by` seconds (a number, or a time reference's offset).
pub(crate) fn shift(b: &B, t: ScriptValue, by: f64) -> ScriptValue {
    b.with(|vm| obj::shift(vm, t, by))
}

/// `beat(n)`.
pub(crate) fn beat(b: &B, n: f64) -> ScriptValue {
    layer(b, "Beat", vec![("n", num(n)), ("offset", num(0.0))])
}

/// `word(@voice, index_or_text)`.
pub(crate) fn word(b: &B, voice: &str, index: ScriptValue) -> ScriptValue {
    layer(b, "Word", vec![("voice", id(voice)), ("index", index), ("offset", num(0.0)), ("nth", num(0.0))])
}

// ---- reading back (a value the chart built or was given) -----------------

/// A number value.
pub(crate) fn number_of(b: &B, v: ScriptValue) -> Option<f64> {
    b.with(|vm| obj::num(vm, v))
}

/// The items of a list value.
pub(crate) fn items_of(b: &B, v: ScriptValue) -> Option<Vec<ScriptValue>> {
    b.with(|vm| obj::items(vm, v))
}

/// A field of an object value (`None` where it has none).
pub(crate) fn field_of(b: &B, v: ScriptValue, name: &str) -> Option<ScriptValue> {
    let o = v.as_object()?;
    b.with(|vm| obj::opt(vm, o, name))
}

/// A string or `@id` value's text.
pub(crate) fn text_of(b: &B, v: ScriptValue) -> Option<String> {
    b.with(|vm| obj::name(vm, v))
}

/// The registered type a value was made from.
pub(crate) fn type_of(b: &B, v: ScriptValue) -> Option<&'static str> {
    b.with(|vm| obj::type_name(vm, v))
}

/// What a value is, for messages.
pub(crate) fn kind_of(b: &B, v: ScriptValue) -> String {
    b.with(|vm| obj::kind(vm, v))
}

/// `a` with alpha `alpha` (0..255).
pub(crate) fn alpha(c: u32, alpha: u32) -> u32 {
    (c & 0xffffff00) | (alpha.min(255))
}

/// `a` towards `b` by `t`.
pub(crate) fn mix(a: u32, b: u32, t: f64) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |c: u32, shift: u32| ((c >> shift) & 0xff) as f64;
    let mut out = 0u32;
    for shift in [24u32, 16, 8, 0] {
        let v = ch(a, shift) + (ch(b, shift) - ch(a, shift)) * t;
        out |= (v.round().clamp(0.0, 255.0) as u32) << shift;
    }
    out
}

/// Relative luminance 0..1.
pub(crate) fn luminance(c: u32) -> f64 {
    let lin = |v: u32| makepad_tween::srgb_to_linear(v as f64 / 255.0);
    0.2126 * lin((c >> 24) & 0xff) + 0.7152 * lin((c >> 16) & 0xff) + 0.0722 * lin((c >> 8) & 0xff)
}

/// SVG path data for a polyline, or a smooth curve through the points
/// (Catmull-Rom as cubic Béziers).
pub(crate) fn path_through(points: &[[f64; 2]], smooth: bool) -> String {
    let mut out = String::new();
    for (i, p) in points.iter().enumerate() {
        if i == 0 {
            out.push_str(&format!("M {:.2} {:.2}", p[0], p[1]));
        } else if !smooth || points.len() < 3 {
            out.push_str(&format!(" L {:.2} {:.2}", p[0], p[1]));
        } else {
            let p0 = if i >= 2 { points[i - 2] } else { points[i - 1] };
            let p1 = points[i - 1];
            let p3 = points.get(i + 1).copied().unwrap_or(*p);
            let c1 = [p1[0] + (p[0] - p0[0]) / 6.0, p1[1] + (p[1] - p0[1]) / 6.0];
            let c2 = [p[0] - (p3[0] - p1[0]) / 6.0, p[1] - (p3[1] - p1[1]) / 6.0];
            out.push_str(&format!(" C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2}", c1[0], c1[1], c2[0], c2[1], p[0], p[1]));
        }
    }
    out
}


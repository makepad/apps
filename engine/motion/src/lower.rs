//! Lowering in the document's VM: every layer type and field that is made
//! of other layers is replaced, right after the document evaluates, by the
//! components it makes, so the loader reads only the drawn kinds.
//!
//! In the order the loader once applied them to each layer, the result
//! lowered again until nothing applies:
//! - `at: anchor(…)`: the layer is lowered without it, and keeps it;
//! - `exit: @defocus | …`: the layer inside a Group that plays the exit;
//! - `role: @label` and `morph: {…}` on a Text;
//! - the layout templates (`Comic`, `Hud`, …, [`crate::templates`]);
//! - the modifiers `onion`, `slices`, `tear`, `droste` and the procedural
//!   looks ([`crate::procedural`]);
//! - `Chart` ([`crate::chart`]).
//!
//! A lowered layer without a `name` is named after its type. `playhead()`
//! and `progress()` become tweens over the named layer's span, and a
//! `Repeat` with a `source` list gets one copy of its `layer` per item
//! (its `item()`s filled), in its `layer` as a list.

use crate::model::{Diagnostic, InputType, InputValue, Loc};
use crate::obj::{self, Diags};
use crate::signals::SignalSource;
use crate::value::SourceMap;
use makepad_script::*;
use std::sync::Arc;

/// The document's frame and clock, which lowerings size and time things by.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DocFrame {
    pub w: f64,
    pub h: f64,
    pub bpm: f64,
    pub duration: f64,
}

/// A layer a `playhead()` / `progress()` can follow: its name, span start
/// and length, `in` and `speed`.
#[derive(Clone)]
struct Clip {
    name: String,
    start: ScriptValue,
    length: ScriptValue,
    source_in: f64,
    speed: f64,
}

pub(crate) struct Pass<'m> {
    pub d: Diags<'m>,
    pub doc: DocFrame,
    signals: Arc<dyn SignalSource>,
    offset: f64,
    clips: Vec<Clip>,
    /// The document, including each replacement before its children lower.
    pub(crate) root: Option<ScriptObject>,
    /// Snapshot roots made by Zoomed. Name lookup must not mistake their
    /// duplicated names for the original document layers.
    pub(crate) zoom_copies: Vec<ScriptObject>,
}

/// The layout templates' types.
pub(crate) const TEMPLATES: &[&str] = &["Comic", "Polaroid", "Hud", "Words", "Shorts", "TokenStream", "Form", "CodeDraw", "UiWindow", "UiButton"];

/// Lower the document `root` in its VM. The diagnostics are where each
/// problem is written.
pub(crate) fn expand(vm: &mut ScriptVm, root: ScriptValue, map: &SourceMap, signals: Arc<dyn SignalSource>, offset: f64) -> Vec<Diagnostic> {
    let Some(r) = root.as_object() else { return Vec::new() };
    if crate::layers::type_of(vm, r) != Some("Motion") {
        return Vec::new();
    }
    let doc = doc_frame(vm, r);
    let mut pass = Pass { d: Diags { map, out: Vec::new() }, doc, signals, offset, clips: Vec::new(), root: Some(r), zoom_copies: Vec::new() };
    // `playhead("reel")` / `progress("reel")` everywhere in the document,
    // against the layers it names.
    collect_clips(vm, root, &mut pass.clips, &mut Seen::default());
    pass.bind_playheads(vm, root);
    let layers = obj::get(vm, r, "layers");
    pass.list(vm, layers, "layers");
    pass.d.out
}

fn doc_frame(vm: &ScriptVm, root: ScriptObject) -> DocFrame {
    let mut doc = DocFrame { w: 1920.0, h: 1080.0, bpm: 120.0, duration: 1.0 };
    if let Some(size) = obj::vecn::<2>(vm, obj::get(vm, root, "size")) {
        if size[0] >= 1.0 && size[1] >= 1.0 && size[0] <= 16384.0 && size[1] <= 16384.0 {
            doc.w = size[0];
            doc.h = size[1];
        }
    }
    if let Some(bpm) = obj::num(vm, obj::get(vm, root, "bpm")).filter(|b| *b > 0.0) {
        doc.bpm = bpm;
    }
    if let Some(duration) = obj::num(vm, obj::get(vm, root, "duration")).filter(|d| *d > 0.0) {
        doc.duration = duration;
    }
    doc
}

impl Pass<'_> {
    pub fn err(&mut self, vm: &ScriptVm, o: ScriptObject, path: &str, field: &str, message: String, hint: Option<&str>) {
        self.d.error(vm, o, path, field, message, hint)
    }

    pub fn warn(&mut self, vm: &ScriptVm, o: ScriptObject, path: &str, field: &str, message: String, hint: Option<&str>) {
        self.d.warning(vm, o, path, field, message, hint)
    }

    /// Each layer of a list, lowered in place.
    fn list(&mut self, vm: &mut ScriptVm, list: ScriptValue, path: &str) {
        let Some(a) = list.as_array() else { return };
        for i in 0..vm.bx.heap_ref().array_len(a) {
            let item = vm.bx.heap_ref().array_index(a, i, NoTrap);
            let child_path = format!("{path}[{i}]");
            let new = self.lower_value(vm, item, &child_path);
            if new != item {
                vm.bx.heap_mut().set_array_index(a, i, new, NoTrap);
            }
            // Publish the parent first: a Zoomed child searches the live root,
            // including a Plot's newly lowered items and Camera's groups.
            if let Some(o) = new.as_object() { self.children(vm, o, &child_path); }
        }
    }

    /// One layer, lowered, then its own layers.
    fn layer(&mut self, vm: &mut ScriptVm, v: ScriptValue, path: &str) -> ScriptValue {
        let new = self.lower_value(vm, v, path);
        if let Some(n) = new.as_object() { self.children(vm, n, path); }
        new
    }

    fn lower_value(&mut self, vm: &mut ScriptVm, v: ScriptValue, path: &str) -> ScriptValue {
        let Some(o) = v.as_object() else { return v };
        match obj::type_name(vm, v) {
            None | Some("Motion") => return v,
            Some(_) => {}
        }
        if vm.bx.heap_ref().is_fn(o) {
            return v;
        }
        if obj::opt(vm, o, "feather_side").is_some_and(|value| !value.is_nil())
            && obj::opt(vm, o, "feather").is_none_or(|value| value.is_nil())
        {
            self.warn(vm, o, path, "feather_side", "`feather_side` without `feather` has no effect".into(), Some("add `feather` on a Rect, Ellipse, Path, Text, Svg or Vector to soften its edges"));
        }
        let new = self.lower(vm, o, path);
        if let Some(n) = new.as_object() {
            for copy in &mut self.zoom_copies {
                if *copy == o { *copy = n; }
            }
        }
        new
    }

    /// A field holding one layer, lowered in place.
    fn one(&mut self, vm: &mut ScriptVm, o: ScriptObject, field: &str, path: &str) {
        let Some(v) = obj::opt(vm, o, field) else { return };
        let new = self.lower_value(vm, v, path);
        if new != v {
            obj::set(vm, o, field, new);
        }
        if let Some(n) = new.as_object() { self.children(vm, n, path); }
    }

    fn children(&mut self, vm: &mut ScriptVm, o: ScriptObject, path: &str) {
        match crate::layers::type_of(vm, o) {
            Some("Group" | "Bones") => {
                let layers = obj::get(vm, o, "layers");
                self.list(vm, layers, &format!("{path}.layers"));
                self.one(vm, o, "mask", &format!("{path}.mask"));
            }
            Some("FlyThrough") => {
                let layers = obj::get(vm, o, "layers");
                self.list(vm, layers, &format!("{path}.layers"));
            }
            Some("Scene3D") => {
                let layers = obj::get(vm, o, "layers");
                if let Some(map) = obj::record_of(vm, layers) {
                    for (name, value, _) in obj::fields(vm, map) {
                        let child_path = format!("{path}.layers.{name}");
                        let new = self.lower_value(vm, value, &child_path);
                        if new != value {
                            obj::set(vm, map, &name, new);
                        }
                        if let Some(n) = new.as_object() { self.children(vm, n, &child_path); }
                    }
                }
            }
            Some("Repeat") => self.repeat(vm, o, path),
            _ => {}
        }
    }

    /// A layer lowered until nothing applies; nil when it was dropped (an
    /// error the pass reported).
    fn lower(&mut self, vm: &mut ScriptVm, o: ScriptObject, path: &str) -> ScriptValue {
        let v: ScriptValue = o.into();
        let ty = crate::layers::type_of(vm, o).unwrap_or("");
        // Check before templates and procedural layers lose their type.
        // Feather belongs to drawn edges, never to a whole layer picture.
        if !matches!(ty, "Rect" | "Ellipse" | "Path" | "Text" | "Svg" | "Vector")
            && obj::opt(vm, o, "feather").is_some_and(|value| !value.is_nil())
        {
            self.err(vm, o, path, "feather", format!("feather softens shape, path, text and SVG edges; use blur for {ty}"), None);
            return NIL;
        }
        // `at: anchor(…)`: lowered without its place, which it then keeps
        // (the loader anchors the layer it lowered to).
        if let Some(at) = obj::opt(vm, o, "at").filter(|a| obj::is(vm, *a, "Anchor")) {
            let inner = obj::copy_without(vm, o, &["at"]);
            let lowered = self.lower(vm, inner, path);
            if lowered == ScriptValue::from(inner) {
                return v;
            }
            if let Some(l) = lowered.as_object() {
                obj::set(vm, l, "at", at);
            }
            return lowered;
        }
        if let Some(exit) = obj::opt(vm, o, "exit") {
            let inner = obj::copy_without(vm, o, &["exit", "exit_at", "exit_duration", "exit_to", "span", "name"]);
            return match self.exit(vm, o, path, exit, inner) {
                Some(group) => self.lower_again(vm, group, path, None),
                None => NIL,
            };
        }
        if ty == "Text" {
            if let Some(role) = obj::opt(vm, o, "role") {
                return match self.role(vm, o, path, role) {
                    Some(text) => self.lower_again(vm, text, path, None),
                    None => NIL,
                };
            }
            if let Some(morph) = obj::opt(vm, o, "morph") {
                return match self.morph(vm, o, path, morph) {
                    Some(m) => self.lower_again(vm, m, path, None),
                    None => NIL,
                };
            }
        }
        if TEMPLATES.contains(&ty) {
            return match self.template(vm, o, path, ty) {
                Some(group) => self.lower_again(vm, group, path, Some(ty)),
                None => NIL,
            };
        }
        if let Some(built) = crate::procedural::lower(self, vm, o, path, ty) {
            return match built {
                Some(made) => self.lower_again(vm, made, path, Some(ty)),
                None => NIL,
            };
        }
        if ty == "Chart" {
            return match self.chart(vm, o, path) {
                Some(group) => self.lower_again(vm, group, path, Some("Chart")),
                None => NIL,
            };
        }
        v
    }

    /// What a lowering made, lowered in turn; named after the type it was
    /// made from (`named`) when it has no name.
    fn lower_again(&mut self, vm: &mut ScriptVm, made: ScriptValue, path: &str, named: Option<&str>) -> ScriptValue {
        let Some(m) = made.as_object() else { return made };
        let lowered = self.lower(vm, m, path);
        if let (Some(kind), Some(l)) = (named, lowered.as_object()) {
            if obj::opt(vm, l, "name").is_none_or(|n| n.is_nil()) {
                let name = obj::text(vm, &kind.to_lowercase());
                obj::set(vm, l, "name", name);
            }
        }
        lowered
    }

    // ---- reads (the loader's, for the fields lowerings take) -----------

    /// The value of the input `in(@name)` placeholder `v` names.
    fn slot(&self, vm: &ScriptVm, v: ScriptValue) -> Option<String> {
        let o = v.as_object()?;
        if crate::layers::type_of(vm, o) != Some("In") {
            return None;
        }
        obj::name(vm, obj::get(vm, o, "name"))
    }

    fn input(&mut self, vm: &ScriptVm, o: ScriptObject, path: &str, field: &str, name: &str) -> Option<InputValue> {
        let host = crate::value::doc_host_ref(vm);
        match host.inputs.iter().find(|(n, _)| *n == crate::value::lut_id(name)) {
            Some((_, value)) => Some(value.clone()),
            None => {
                let names: Vec<String> = host.inputs.iter().map(|(n, _)| crate::obj::id_text(*n)).collect();
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let hint = format!("declare it: `inputs: {{{name}: …}}`");
                let message = format!("`in(@{name})` names no input (declared: {}).{}", if names.is_empty() { "none".into() } else { names.join(", ") }, crate::load::did_you_mean(name, &refs));
                self.err(vm, o, path, field, message, Some(&hint));
                None
            }
        }
    }

    pub fn num_const(&mut self, vm: &ScriptVm, o: ScriptObject, path: &str, field: &str, v: ScriptValue) -> Option<f64> {
        if let Some(name) = self.slot(vm, v) {
            return match self.input(vm, o, path, field, &name)? {
                InputValue::Number(n) => Some(n),
                other => {
                    self.err(vm, o, path, field, format!("`{field}` is a number, but input `{name}` is {}", other.describe()), None);
                    None
                }
            };
        }
        if let Some(n) = obj::num(vm, v) {
            return Some(n);
        }
        if let Some(b) = v.as_bool() {
            return Some(if b { 1.0 } else { 0.0 });
        }
        let anim = obj::type_name(vm, v).is_some_and(|t| crate::value::ANIMS.contains(&t));
        let hint = if anim { Some("this property is not animatable; give it a number") } else { None };
        self.err(vm, o, path, field, format!("`{field}` is a number, not {}", obj::kind(vm, v)), hint);
        None
    }

    pub fn vec2_const(&mut self, vm: &ScriptVm, o: ScriptObject, path: &str, field: &str, v: ScriptValue) -> Option<[f64; 2]> {
        if let Some(name) = self.slot(vm, v) {
            return match self.input(vm, o, path, field, &name)? {
                InputValue::Vec2(v) => Some(v),
                InputValue::Number(n) => Some([n, n]),
                other => {
                    self.err(vm, o, path, field, format!("`{field}` is a vec2, but input `{name}` is {}", other.describe()), None);
                    None
                }
            };
        }
        match obj::vecn::<2>(vm, v) {
            Some(v) => Some(v),
            None => {
                self.err(vm, o, path, field, format!("`{field}` is `vec2(x, y)`, not {}", obj::kind(vm, v)), None);
                None
            }
        }
    }

    pub fn color_const(&mut self, vm: &ScriptVm, o: ScriptObject, path: &str, field: &str, v: ScriptValue) -> Option<[f64; 4]> {
        if let Some(name) = self.slot(vm, v) {
            return match self.input(vm, o, path, field, &name)? {
                InputValue::Color(c) => Some(c),
                other => {
                    self.err(vm, o, path, field, format!("`{field}` is a colour, but input `{name}` is {}", other.describe()), None);
                    None
                }
            };
        }
        match color_of(vm, v) {
            Some(c) => Some(c),
            None => {
                self.err(vm, o, path, field, format!("`{field}` is a colour (`#rrggbb`, `#rrggbbaa` or `rgba(r, g, b, a)` in 0..1), not {}", obj::kind(vm, v)), None);
                None
            }
        }
    }

    // ---- exit, role, morph -------------------------------------------

    /// The Group an `exit:` lowers to: `inner` (the layer without its exit,
    /// span and name) inside a Group that takes the span and plays the exit.
    fn exit(&mut self, vm: &mut ScriptVm, o: ScriptObject, path: &str, exit: ScriptValue, inner: ScriptObject) -> Option<ScriptValue> {
        let Some(kind) = obj::ident(exit) else {
            self.err(vm, o, path, "exit", format!("`exit` is @defocus, @collapse or @fill, not {}", obj::kind(vm, exit)), None);
            return None;
        };
        let default_duration = match kind.as_str() {
            "defocus" => 0.4,
            "collapse" => 0.5,
            "fill" => 0.6,
            "iris" => 0.5,
            "curtain" => 0.6,
            "speed_lines" => 0.35,
            other => {
                self.err(vm, o, path, "exit", format!("`@{other}` is not an exit.{}", crate::load::did_you_mean(other, crate::layers::EXIT)), Some("@defocus (blur and fade), @collapse (shrink into a point), @fill (grow until it covers the frame), @iris (a closing circle), @curtain (split and slide apart), @speed_lines (a streaked wipe); a paper tear is the `tear:` modifier"));
                return None;
            }
        };
        let duration = match obj::opt(vm, o, "exit_duration").filter(|v| !v.is_nil()) {
            None => default_duration,
            Some(v) => self.num_const(vm, o, path, "exit_duration", v).unwrap_or(default_duration).max(0.01),
        };
        let doc = [self.doc.w, self.doc.h];
        // When: `exit_at`, else the end of the span, else of the document.
        let span_end = obj::opt(vm, o, "span").and_then(|s| obj::items(vm, s)).filter(|s| s.len() == 2).and_then(|s| s[1].as_number());
        let start: ScriptValue = match (obj::opt(vm, o, "exit_at").filter(|t| !t.is_nil()), span_end) {
            (Some(t), _) => t,
            (None, Some(end)) => (end - duration).into(),
            _ => (self.doc.duration - duration).into(),
        };
        let at_end = obj::shift(vm, start, duration);
        let keys = |vm: &mut ScriptVm, points: Vec<(ScriptValue, f64, Option<&str>)>| -> ScriptValue {
            obj::keys(vm, points.into_iter().map(|(t, v, e)| (t, v.into(), e)).collect())
        };
        let mut fields: Vec<(&str, ScriptValue)> = Vec::new();
        for key in ["name", "span"] {
            if let Some(v) = obj::opt(vm, o, key) {
                fields.push((key, v));
            }
        }
        let to = match obj::opt(vm, o, "exit_to").filter(|v| !v.is_nil()) {
            None => [doc[0] * 0.5, doc[1] * 0.5],
            Some(v) => self.vec2_const(vm, o, path, "exit_to", v).unwrap_or([doc[0] * 0.5, doc[1] * 0.5]),
        };
        let pivot = |vm: &mut ScriptVm, point: [f64; 2]| -> [(&'static str, ScriptValue); 2] {
            [("size", obj::v2(vm, doc[0], doc[1])), ("anchor", obj::v2(vm, point[0] / doc[0].max(1e-9), point[1] / doc[1].max(1e-9)))]
        };
        let mut inner: Vec<ScriptValue> = vec![inner.into()];
        match kind.as_str() {
            "iris" => {
                let reach = [[0.0, 0.0], [doc[0], 0.0], [0.0, doc[1]], [doc[0], doc[1]]].iter().map(|q: &[f64; 2]| ((q[0] - to[0]).powi(2) + (q[1] - to[1]).powi(2)).sqrt()).fold(0.0, f64::max) * 1.02;
                let size = obj::v2(vm, doc[0], doc[1]);
                fields.push(("size", size));
                let at = obj::v2(vm, to[0] - reach, to[1] - reach);
                let esize = obj::v2(vm, reach * 2.0, reach * 2.0);
                let scale = keys(vm, vec![(start, 1.0, None), (at_end, 0.0, Some("ease_in_cubic"))]);
                let mask = obj::make(vm, "Ellipse", &[("at", at), ("size", esize), ("scale", scale)]);
                fields.push(("mask", mask));
            }
            "curtain" => {
                let layer = inner[0];
                let half = |vm: &mut ScriptVm, x: f64, dx: f64| -> ScriptValue {
                    let size = obj::v2(vm, doc[0], doc[1]);
                    let mat = obj::v2(vm, x, 0.0);
                    let msize = obj::v2(vm, doc[0] * 0.5, doc[1]);
                    let mask = obj::make(vm, "Rect", &[("at", mat), ("size", msize)]);
                    let xk = keys(vm, vec![(start, 0.0, None), (at_end, dx, Some("ease_in_cubic"))]);
                    let layers = obj::array(vm, [layer]);
                    obj::make(vm, "Group", &[("size", size), ("mask", mask), ("x", xk), ("layers", layers)])
                };
                let left = half(vm, 0.0, -doc[0] * 0.5);
                let right = half(vm, doc[0] * 0.5, doc[0] * 0.5);
                inner = vec![left, right];
            }
            "speed_lines" => {
                // The picture swept off to the right behind a hard edge, with
                // streaks trailing the edge.
                let edge = keys(vm, vec![(start, 0.0, None), (at_end, doc[0] * 1.15, Some("ease_in_cubic"))]);
                let size = obj::v2(vm, doc[0], doc[1]);
                let msize = obj::v2(vm, doc[0], doc[1]);
                let mask = obj::make(vm, "Rect", &[("size", msize), ("x", edge)]);
                let layers = obj::array(vm, [inner[0]]);
                let wiped = obj::make(vm, "Group", &[("size", size), ("mask", mask), ("layers", layers)]);
                let n = 18.0;
                let spacing = obj::v2(vm, 0.0, doc[1] / n);
                let jitter = obj::record(vm, &[("x", (doc[0] * 0.08).into()), ("y", (doc[1] / n * 0.4).into()), ("scale", 0.5.into())]);
                let at = obj::v2(vm, -doc[0] * 0.3, doc[1] / n * 0.5);
                let fade_in = obj::shift(vm, start, duration * 0.15);
                let fade_out = obj::shift(vm, start, duration * 0.8);
                let opacity = keys(vm, vec![(start, 0.0, None), (fade_in, 0.85, None), (fade_out, 0.85, None), (at_end, 0.0, None)]);
                let rsize = obj::v2(vm, doc[0] * 0.28, (doc[1] * 0.004).max(2.0));
                let streak = obj::make(vm, "Rect", &[("size", rsize), ("fill", obj::color(0xffffffff)), ("radius", (doc[1] * 0.002).into())]);
                let streaks = obj::make(vm, "Repeat", &[
                    ("count", n.into()),
                    ("layout", obj::id("grid")),
                    ("columns", 1.0.into()),
                    ("spacing", spacing),
                    ("seed", 11.0.into()),
                    ("jitter", jitter),
                    ("at", at),
                    ("x", edge),
                    ("opacity", opacity),
                    ("layer", streak),
                ]);
                inner = vec![wiped, streaks];
            }
            "defocus" => {
                let blur = doc[0].min(doc[1]) * 0.03;
                let b = keys(vm, vec![(start, 0.0, None), (at_end, blur, Some("ease_in_cubic"))]);
                fields.push(("blur", b));
                let op = keys(vm, vec![(start, 1.0, None), (at_end, 0.0, Some("ease_in_quad"))]);
                fields.push(("opacity", op));
            }
            "collapse" => {
                fields.extend(pivot(vm, to));
                let s = keys(vm, vec![(start, 1.0, None), (at_end, 0.0, Some("ease_in_back"))]);
                fields.push(("scale", s));
                let fade = obj::shift(vm, start, duration * 0.8);
                let op = keys(vm, vec![(fade, 1.0, None), (at_end, 0.0, None)]);
                fields.push(("opacity", op));
            }
            _ => {
                // Grow about the shape's centre until it covers the frame.
                let p = obj::opt(vm, o, "at").and_then(|v| self.vec2_const(vm, o, path, "at", v));
                let s = obj::opt(vm, o, "size").and_then(|v| self.vec2_const(vm, o, path, "size", v));
                let (Some(p), Some(s)) = (p, s) else {
                    self.err(vm, o, path, "exit", "`exit: @fill` grows a shape with a constant `at` and `size` (a Rect or Ellipse) until it covers the frame".into(), None);
                    return None;
                };
                let c = [p[0] + s[0] * 0.5, p[1] + s[1] * 0.5];
                let reach = [[0.0, 0.0], [doc[0], 0.0], [0.0, doc[1]], [doc[0], doc[1]]].iter().map(|q| ((q[0] - c[0]).powi(2) + (q[1] - c[1]).powi(2)).sqrt()).fold(0.0, f64::max);
                let factor = (reach * 2.0 / s[0].min(s[1]).max(1e-6)) * 1.05;
                fields.extend(pivot(vm, c));
                let sc = keys(vm, vec![(start, 1.0, None), (at_end, factor, Some("ease_in_cubic"))]);
                fields.push(("scale", sc));
            }
        }
        let layers = obj::array(vm, inner);
        fields.push(("layers", layers));
        let group = obj::make(vm, "Group", &fields);
        let at = vm.bx.heap_ref().made_at(o);
        vm.bx.heap_mut().set_made_at(group.as_object().unwrap(), at);
        Some(group)
    }

    /// `role: @label`: the house mono label (tiny tracked capitals at 60%),
    /// where the author has not set those fields.
    fn role(&mut self, vm: &mut ScriptVm, o: ScriptObject, path: &str, role: ScriptValue) -> Option<ScriptValue> {
        if obj::ident(role).as_deref() != Some("label") {
            self.err(vm, o, path, "role", format!("`role` is @label, not {}", obj::kind(vm, role)), Some("`Text{text: \"SHOWREEL '26\" role: @label}`"));
            return None;
        }
        let t = obj::copy_without(vm, o, &["role"]);
        let size = obj::opt(vm, t, "size").and_then(|v| obj::num(vm, v)).unwrap_or((self.doc.w.min(self.doc.h) * 0.02).round());
        let font = obj::lit(vm, obj::face("JetBrains Mono", 400.0, false));
        for (k, v) in [("font", font), ("size", size.into()), ("tracking", ((size * 0.18 * 100.0).round() / 100.0).into()), ("opacity", 0.6.into())] {
            if obj::opt(vm, t, k).is_none() {
                obj::set(vm, t, k, v);
            }
        }
        if let Some(text) = obj::opt(vm, t, "text").and_then(|v| obj::string(vm, v)) {
            let upper = obj::text(vm, &text.to_uppercase());
            obj::set(vm, t, "text", upper);
        }
        Some(t.into())
    }

    /// `Text{… morph: {text: "…" time duration …}}`: a Morph of the text into
    /// the same text with the morph's fields.
    fn morph(&mut self, vm: &mut ScriptVm, o: ScriptObject, path: &str, morph: ScriptValue) -> Option<ScriptValue> {
        let Some(m) = obj::record_of(vm, morph).or_else(|| morph.as_object().filter(|_| obj::type_name(vm, morph).is_some() && !obj::is_fn(vm, morph))) else {
            self.err(vm, o, path, "morph", format!("`morph` is `{{text: \"…\" time: 1.5 duration: 0.6 size: … at: …}}`, not {}", obj::kind(vm, morph)), None);
            return None;
        };
        let from = obj::copy_without(vm, o, &["morph", "name", "span"]);
        let to = obj::copy_without(vm, from, &["animate", "start", "speed", "opacity", "x", "y", "scale", "rotate", "by", "stagger", "blur", "reveal", "caret"]);
        for (k, v, _) in obj::fields(vm, m) {
            if k == "time" || k == "duration" {
                continue;
            }
            obj::set(vm, to, &k, v);
        }
        let mut fields: Vec<(&str, ScriptValue)> = Vec::new();
        for key in ["name", "span"] {
            if let Some(v) = obj::opt(vm, o, key) {
                fields.push((key, v));
            }
        }
        fields.push(("from", from.into()));
        fields.push(("to", to.into()));
        fields.push(("time", obj::opt(vm, m, "time").unwrap_or(1.0.into())));
        fields.push(("duration", obj::opt(vm, m, "duration").unwrap_or(0.6.into())));
        let made = obj::make(vm, "Morph", &fields);
        let at = vm.bx.heap_ref().made_at(o);
        vm.bx.heap_mut().set_made_at(made.as_object().unwrap(), at);
        Some(made)
    }

    // ---- Repeat with a source -----------------------------------------

    /// A Repeat's copies: with a `source` list, its `layer` becomes one copy
    /// per item (or, for an empty list, one with blank items), each lowered.
    fn repeat(&mut self, vm: &mut ScriptVm, o: ScriptObject, path: &str) {
        let layer_path = format!("{path}.layer");
        let Some(template) = obj::opt(vm, o, "layer") else { return };
        if obj::items(vm, template).is_some() {
            self.err(vm, o, path, "layer", "a Repeat copies one `layer: Rect{…}` (any layer, a Group for several), not a list".into(), None);
            obj::set(vm, o, "layer", NIL);
            return;
        }
        let source = obj::opt(vm, o, "source").filter(|s| !s.is_nil());
        let Some(list) = source.and_then(|s| self.source_list(vm, s)) else {
            if source.is_none() && has_item(vm, template) {
                self.err(vm, o, path, "layer", "`item()` reads the item of a Repeat with a `source` list; this Repeat has none".into(), Some("`Repeat{source: in(@rows) layer: Text{text: item(@name)}}`"));
            }
            if source.is_none() {
                self.one(vm, o, "layer", &layer_path);
            }
            return;
        };
        let mut copies = Vec::with_capacity(list.len().max(1));
        if list.is_empty() {
            // Only so the template exists: its diagnostics are not the
            // document's.
            let saved = std::mem::take(&mut self.d.out);
            let blank = self.fill(vm, o, path, template, NIL, 0);
            copies.push(self.layer(vm, blank, &layer_path));
            self.d.out = saved;
        }
        for (index, item) in list.into_iter().enumerate() {
            let filled = self.fill(vm, o, path, template, item, index);
            copies.push(self.layer(vm, filled, &layer_path));
        }
        let copies = obj::array(vm, copies);
        obj::set(vm, o, "layer", copies);
    }

    /// A `source` as its list: a list written in place, or a list input
    /// (`in(@rows)`, `input(@rows)` naming a declared input).
    fn source_list(&mut self, vm: &mut ScriptVm, v: ScriptValue) -> Option<Vec<ScriptValue>> {
        if let Some(items) = v.as_array().and_then(|_| obj::items(vm, v)) {
            return Some(items);
        }
        let name = match obj::type_name(vm, v) {
            Some("In") => obj::name(vm, obj::get(vm, v.as_object()?, "name"))?,
            Some("Input") => {
                let n = obj::ident(obj::get(vm, v.as_object()?, "ty"))?;
                if InputType::from_id(&n).is_some() {
                    return None;
                }
                n
            }
            _ => return None,
        };
        let id = crate::value::lut_id(&name);
        let given = crate::value::doc_host_ref(vm).inputs.iter().find(|(n, _)| *n == id).map(|(_, v)| v.clone());
        // Read to decide structure: noted like an `in(@…)` the document asked.
        if let (Some(value), Some(asked)) = (&given, &mut crate::value::doc_host(vm).asked) {
            asked.push((id, Some(value.clone())));
        }
        match given {
            Some(InputValue::List(items)) => Some(items.iter().map(|item| crate::value::input_to_script(vm, item)).collect()),
            Some(_) => None,
            // Not given: the declared default (`rows: input(@list, [...])` or `rows: [...]`).
            None => {
                let declared = obj::opt(vm, self.root?, "inputs").and_then(|i| obj::record_of(vm, i))?;
                let mut value = obj::opt(vm, declared, &name)?;
                if obj::is(vm, value, "Input") {
                    value = obj::get(vm, value.as_object()?, "default");
                }
                value.as_array().and_then(|_| obj::items(vm, value))
            }
        }
    }

    /// `v` with `item()` / `item(@field)` replaced by its `index`th item. A
    /// nested Repeat with its own `source` keeps its template's items for
    /// itself (its `source` may read this item).
    fn fill(&mut self, vm: &mut ScriptVm, o: ScriptObject, path: &str, v: ScriptValue, item: ScriptValue, index: usize) -> ScriptValue {
        if obj::is(vm, v, "Item") {
            let Some(field) = obj::ident(obj::get(vm, v.as_object().unwrap(), "field")) else { return item };
            if item.is_nil() {
                return NIL;
            }
            return match obj::record_of(vm, item) {
                Some(record) => match obj::opt(vm, record, &field) {
                    Some(value) => value,
                    None => {
                        let fields: Vec<String> = obj::fields(vm, record).into_iter().map(|(k, _, _)| k).collect();
                        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
                        self.err(vm, o, path, "source", format!("item {index} has no field `{field}` (it has: {}).{}", fields.join(", "), crate::load::did_you_mean(&field, &refs)), None);
                        NIL
                    }
                },
                None => {
                    self.err(vm, o, path, "source", format!("`item(@{field})` reads a field, but item {index} is {}: read it whole with `item()`", obj::kind(vm, item)), None);
                    NIL
                }
            };
        }
        if v.as_array().is_some() {
            let items = obj::items(vm, v).unwrap_or_default();
            let filled: Vec<ScriptValue> = items.into_iter().map(|x| self.fill(vm, o, path, x, item, index)).collect();
            return obj::array(vm, filled);
        }
        let Some(src) = v.as_object() else { return v };
        if vm.bx.heap_ref().is_fn(src) || !fills(vm, v) {
            return v;
        }
        let own_source = obj::is(vm, v, "Repeat") && obj::opt(vm, src, "source").is_some_and(|s| !s.is_nil());
        let copy = obj::copy_without(vm, src, &[]);
        for (k, value, _) in obj::fields(vm, copy) {
            if own_source && k == "layer" {
                continue;
            }
            let filled = self.fill(vm, o, path, value, item, index);
            if filled != value {
                obj::set(vm, copy, &k, filled);
            }
        }
        copy.into()
    }

    // ---- playhead() / progress() ---------------------------------------

    /// Every `playhead(name)` / `progress(name)` under `v` as a linear
    /// tween over the named layer's span: its own seconds (from its `in`,
    /// at its `speed`) and 0..1.
    fn bind_playheads(&mut self, vm: &mut ScriptVm, v: ScriptValue) {
        let mut seen = Seen::default();
        self.bind(vm, v, None, &mut seen);
    }

    fn bind(&mut self, vm: &mut ScriptVm, v: ScriptValue, holder: Option<ScriptObject>, seen: &mut Seen) -> Option<ScriptValue> {
        if let Some(a) = v.as_array() {
            if !seen.arrays.insert(a) {
                return None;
            }
            for i in 0..vm.bx.heap_ref().array_len(a) {
                let item = vm.bx.heap_ref().array_index(a, i, NoTrap);
                if let Some(new) = self.bind(vm, item, holder, seen) {
                    vm.bx.heap_mut().set_array_index(a, i, new, NoTrap);
                }
            }
            return None;
        }
        let o = v.as_object()?;
        if vm.bx.heap_ref().is_fn(o) {
            return None;
        }
        let ty = crate::layers::type_of(vm, o);
        if matches!(ty, Some("Playhead") | Some("Progress")) {
            let name = obj::string(vm, obj::get(vm, o, "name"))?;
            let playhead = ty == Some("Playhead");
            let Some(clip) = self.clips.iter().find(|c| c.name == name).cloned() else {
                let names: Vec<&str> = self.clips.iter().map(|c| c.name.as_str()).collect();
                let message = format!("no layer named \"{name}\" with a `span` to follow.{}", crate::load::did_you_mean(&name, &names));
                let loc = obj::loc(vm, self.d.map, o).or_else(|| holder.and_then(|h| obj::loc(vm, self.d.map, h))).unwrap_or(Loc { line: 1, col: 1 });
                self.d.out.push(Diagnostic::error(loc, "", message).hint("`playhead(\"reel\")` follows a layer with `name: \"reel\" span: [start, length]`"));
                return None;
            };
            let length = obj::num(vm, clip.length).unwrap_or(0.0);
            let end = obj::shift(vm, clip.start, length);
            let (from, to) = if playhead {
                (clip.source_in.into(), (clip.source_in + length * clip.speed).into())
            } else {
                let keep = |vm: &ScriptVm, k: &str, d: f64| -> ScriptValue {
                    let v = obj::get(vm, o, k);
                    if obj::num(vm, v).is_some() || v.as_color().is_some() || obj::items(vm, v).is_some() { v } else { d.into() }
                };
                (keep(vm, "from", 0.0), keep(vm, "to", 1.0))
            };
            return Some(obj::make(vm, "Tween", &[("from", from), ("to", to), ("start", clip.start), ("end", end), ("ease", obj::id("linear"))]));
        }
        if !seen.objects.insert(o) {
            return None;
        }
        let holder = if obj::loc(vm, self.d.map, o).is_some() { Some(o) } else { holder };
        for (k, value, _) in obj::fields(vm, o) {
            if let Some(new) = self.bind(vm, value, holder, seen) {
                obj::set(vm, o, &k, new);
            }
        }
        None
    }
}

/// The layers a `playhead()` can follow: every object with a string
/// `name` and a two-part `span`.
/// Objects and arrays a walk has been through.
#[derive(Default)]
struct Seen {
    objects: std::collections::HashSet<ScriptObject>,
    arrays: std::collections::HashSet<ScriptArray>,
}

fn collect_clips(vm: &ScriptVm, v: ScriptValue, out: &mut Vec<Clip>, seen: &mut Seen) {
    if let Some(a) = v.as_array() {
        if !seen.arrays.insert(a) {
            return;
        }
        for i in 0..vm.bx.heap_ref().array_len(a) {
            collect_clips(vm, vm.bx.heap_ref().array_index(a, i, NoTrap), out, seen);
        }
        return;
    }
    let Some(o) = v.as_object() else { return };
    if vm.bx.heap_ref().is_fn(o) || !seen.objects.insert(o) {
        return;
    }
    let fields = obj::fields(vm, o);
    let get = |k: &str| fields.iter().find(|(f, _, _)| f == k).map(|(_, v, _)| *v);
    if let (Some(name), Some(span)) = (get("name").and_then(|n| obj::string(vm, n)), get("span").and_then(|s| obj::items(vm, s))) {
        if span.len() == 2 {
            let source_in = get("in").and_then(|v| obj::num(vm, v)).unwrap_or(0.0);
            let speed = get("speed").and_then(|v| obj::num(vm, v)).unwrap_or(1.0);
            out.push(Clip { name, start: span[0], length: span[1], source_in, speed });
        }
    }
    for (_, value, _) in fields {
        collect_clips(vm, value, out, seen);
    }
}

/// Whether `v` reads a Repeat item (`item()` outside a nested Repeat with
/// its own `source`).
fn has_item(vm: &ScriptVm, v: ScriptValue) -> bool {
    fn walk(vm: &ScriptVm, v: ScriptValue, depth: usize) -> bool {
        if depth > 64 {
            return false;
        }
        if let Some(items) = v.as_array().and_then(|_| obj::items(vm, v)) {
            return items.into_iter().any(|x| walk(vm, x, depth + 1));
        }
        let Some(o) = v.as_object() else { return false };
        if vm.bx.heap_ref().is_fn(o) {
            return false;
        }
        match crate::layers::type_of(vm, o) {
            Some("Item") => true,
            Some("Repeat") if obj::opt(vm, o, "source").is_some_and(|s| !s.is_nil()) => false,
            _ => obj::fields(vm, o).into_iter().any(|(_, x, _)| walk(vm, x, depth + 1)),
        }
    }
    walk(vm, v, 0)
}

/// Whether filling a Repeat's items changes `v`: it reads `item()`
/// (a nested Repeat with its own `source` keeps its `layer`'s for itself).
fn fills(vm: &ScriptVm, v: ScriptValue) -> bool {
    fn walk(vm: &ScriptVm, v: ScriptValue, depth: usize) -> bool {
        if depth > 64 {
            return false;
        }
        if let Some(items) = v.as_array().and_then(|_| obj::items(vm, v)) {
            return items.into_iter().any(|x| walk(vm, x, depth + 1));
        }
        let Some(o) = v.as_object() else { return false };
        if vm.bx.heap_ref().is_fn(o) {
            return false;
        }
        let own_source = crate::layers::type_of(vm, o) == Some("Repeat") && obj::opt(vm, o, "source").is_some_and(|s| !s.is_nil());
        match crate::layers::type_of(vm, o) {
            Some("Item") => true,
            _ => obj::fields(vm, o).into_iter().any(|(k, x, _)| !(own_source && k == "layer") && walk(vm, x, depth + 1)),
        }
    }
    walk(vm, v, 0)
}

/// A colour: `#rrggbbaa`, an HDR `rgba(…)` (`{color: [r, g, b, a]}`), or a
/// `"#hex"` string.
pub(crate) fn color_of(vm: &ScriptVm, v: ScriptValue) -> Option<[f64; 4]> {
    if let Some(c) = v.as_color() {
        return Some(crate::load::unpack(c));
    }
    if let Some(s) = obj::string(vm, v) {
        return crate::load::parse_hex(&s);
    }
    let o = obj::record_of(vm, v)?;
    let parts = obj::items(vm, obj::opt(vm, o, "color")?)?;
    if parts.len() != 4 {
        return None;
    }
    Some([obj::num(vm, parts[0])?, obj::num(vm, parts[1])?, obj::num(vm, parts[2])?, obj::num(vm, parts[3])?])
}

// ---- templates and charts ---------------------------------------------------

impl Pass<'_> {
    fn template(&mut self, vm: &mut ScriptVm, o: ScriptObject, path: &str, ty: &str) -> Option<ScriptValue> {
        // `words: voice(@vo) line: i`: a TokenStream's tokens on the sung word times.
        let voice_times = if ty == "TokenStream" {
            let voice = obj::opt(vm, o, "words").filter(|w| obj::is(vm, *w, "Voice")).and_then(|w| obj::name(vm, obj::get(vm, w.as_object().unwrap(), "id")));
            match voice {
                Some(voice) => {
                    let line = obj::opt(vm, o, "line").and_then(|l| obj::num(vm, l)).map(|l| l as u32);
                    let context = crate::MotionContext { signals: self.signals.clone(), offset: self.offset, ..Default::default() };
                    match crate::lyrics::voice_words(&context, &voice, line) {
                        Ok(words) => Some(words.iter().map(|w| w.1).collect()),
                        Err(message) => {
                            self.warn(vm, o, path, "words", format!("{message}; the tokens step by `every` instead"), None);
                            None
                        }
                    }
                }
                None => None,
            }
        } else {
            None
        };
        let doc = self.doc;
        let mut cx = crate::build::Cx { vm, doc: [doc.w, doc.h], bpm: doc.bpm, duration: doc.duration };
        let built = match ty {
            "Comic" => crate::templates::comic(&mut cx, o),
            "Polaroid" => crate::templates::polaroid(&mut cx, o),
            "Hud" => crate::templates::hud(&mut cx, o),
            "Shorts" => crate::templates::shorts(&mut cx, o),
            "TokenStream" => crate::templates::token_stream(&mut cx, o, voice_times),
            "Form" => crate::templates::form(&mut cx, o),
            "CodeDraw" => crate::templates::code_draw(&mut cx, o),
            "UiWindow" => crate::templates::ui_window(&mut cx, o),
            "UiButton" => crate::templates::ui_button(&mut cx, o),
            _ => crate::templates::words(&mut cx, o),
        };
        match built {
            Ok(made) => {
                // A template's own `playhead`/`progress` follow the document's clips.
                self.bind_playheads(vm, made);
                Some(made)
            }
            Err((field, message)) => {
                self.err(vm, o, path, &field, message, None);
                None
            }
        }
    }

    fn chart(&mut self, vm: &mut ScriptVm, o: ScriptObject, path: &str) -> Option<ScriptValue> {
        use crate::chart::build::B;
        struct Host<'p, 'm, 'c, 'a, 'b> {
            pass: &'p mut Pass<'m>,
            b: &'c B<'a, 'b>,
            o: ScriptObject,
            path: &'p str,
        }
        impl crate::chart::ChartHost for Host<'_, '_, '_, '_, '_> {
            fn doc_size(&self) -> [f64; 2] {
                [self.pass.doc.w, self.pass.doc.h]
            }
            fn error(&mut self, field: &str, message: String, hint: Option<&str>) {
                let (pass, o, path) = (&mut *self.pass, self.o, self.path);
                self.b.with(|vm| pass.err(vm, o, path, field, message, hint))
            }
            fn warning(&mut self, field: &str, message: String, hint: Option<&str>) {
                let (pass, o, path) = (&mut *self.pass, self.o, self.path);
                self.b.with(|vm| pass.warn(vm, o, path, field, message, hint))
            }
            fn number(&mut self, field: &str, value: ScriptValue) -> Option<f64> {
                let (pass, o, path) = (&mut *self.pass, self.o, self.path);
                self.b.with(|vm| pass.num_const(vm, o, path, field, value))
            }
            fn vec2(&mut self, field: &str, value: ScriptValue) -> Option<[f64; 2]> {
                let (pass, o, path) = (&mut *self.pass, self.o, self.path);
                self.b.with(|vm| pass.vec2_const(vm, o, path, field, value))
            }
            fn color(&mut self, field: &str, value: ScriptValue) -> Option<u32> {
                let (pass, o, path) = (&mut *self.pass, self.o, self.path);
                self.b.with(|vm| pass.color_const(vm, o, path, field, value)).map(crate::load::pack)
            }
            fn text(&mut self, field: &str, value: ScriptValue) -> Option<String> {
                let (pass, o, path) = (&mut *self.pass, self.o, self.path);
                self.b.with(|vm| {
                    if let Some(name) = pass.slot(vm, value) {
                        return match pass.input(vm, o, path, field, &name)? {
                            InputValue::Text(text) | InputValue::Markup(text) => Some(text),
                            other => {
                                pass.err(vm, o, path, field, format!("`{field}` is text, but input `{name}` is {}", other.describe()), None);
                                None
                            }
                        };
                    }
                    match obj::name(vm, value) {
                        Some(text) => Some(text),
                        None => {
                            pass.err(vm, o, path, field, format!("`{field}` is text, not {}", obj::kind(vm, value)), Some("CSV/JSON text inline, or `in(@data)` filled from a library file"));
                            None
                        }
                    }
                })
            }
            fn resolve(&mut self, field: &str, value: ScriptValue) -> ScriptValue {
                let (pass, o, path) = (&mut *self.pass, self.o, self.path);
                self.b.with(|vm| {
                    let Some(name) = pass.slot(vm, value) else { return value };
                    match pass.input(vm, o, path, field, &name) {
                        Some(InputValue::Number(n)) => n.into(),
                        Some(InputValue::Text(t)) => obj::text(vm, &t),
                        Some(InputValue::Color(c)) => obj::color(crate::load::pack(c)),
                        Some(InputValue::Bool(b)) => b.into(),
                        _ => NIL,
                    }
                })
            }
        }
        let b = B::new(vm);
        crate::chart::expand(&b, o, &mut Host { pass: self, b: &b, o, path })
    }
}

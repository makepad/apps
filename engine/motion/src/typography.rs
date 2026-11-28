//! Named typography animators: `Text{text: "…" animate: @fade_up}` in one
//! line, and `Text{… inside: <layer>}` (footage inside type). The animators
//! are Splash (`typography.splash`): most set the Text's own fields (units,
//! stagger, tweens, reveal, caret, plate) where the author has not; a few
//! become other layers (`@word_swap`, `@stack`/`@grid`/`@rotated`, the line
//! masks, outlines, neon, …, a Group of Texts; `@scramble`, `@split_flap`,
//! `@ticker`, `@led_ticker` a `KineticText` on that kit). They run in the
//! document's VM right after it evaluates ([`expand`]): every Text with
//! `animate:` or `inside:` is replaced in its parent by what its animator
//! makes, so the loader reads plain layers.
//!
//! Common fields: `start` (a time; default 0.2), `speed` (1 = the look's
//! own pace; 2 twice as fast).

use makepad_script::*;

/// The animators' module.
const MODULE: &str = include_str!("typography.splash");

/// `(name, what it looks like, what it suits)`: the menu, in the module's
/// order.
pub fn animators() -> Vec<(String, String, String)> {
    use crate::obj::{self, Look};
    let mut evaluated = crate::value::evaluate(&crate::model::DocSource::file(MODULE), crate::MotionContext::default().signals, 0.0, &[], &crate::modules::NoModules, None).unwrap_or_else(|diags| panic!("the type animators' module: {diags:?}"));
    let root = evaluated.root;
    evaluated.with_vm(|vm| {
        let Some(Look::Obj(all, _)) = root.as_object().and_then(|o| obj::look_field(vm, o, "type_animators")) else { return Vec::new() };
        let text = |a: Option<ScriptObject>, k: &str| match a.and_then(|a| obj::look_field(vm, a, k)) {
            Some(Look::Str(s)) => s,
            _ => String::new(),
        };
        obj::fields(vm, all).into_iter().filter(|(name, _, _)| !name.starts_with("__")).map(|(name, a, _)| {
            let a = obj::look(vm, a).obj();
            let about = text(a, "about");
            let suits = text(a, "suits");
            (name, about, suits)
        }).collect()
    })
}

/// A field's value, nil where `o` has none (a component's unset field).
fn get(vm: &ScriptVm, o: ScriptObject, key: &str) -> ScriptValue {
    let v = vm.bx.heap_ref().value(o, LiveId::from_str(key).into(), NoTrap);
    if v.is_err() { NIL } else { v }
}

/// Whether `o` is made from mod.motion's `Text`.
fn is_text(vm: &ScriptVm, o: ScriptObject, text: Option<ScriptObject>) -> bool {
    let Some(text) = text else { return false };
    let mut at = Some(o);
    while let Some(p) = at {
        if p == text {
            return true;
        }
        at = vm.bx.heap_ref().proto(p).as_object();
    }
    false
}

/// The layer arrays and single layers under `o` (a document's `layers`, a
/// Group's, a Repeat's `layer`).
fn walk(vm: &mut ScriptVm, list: ScriptValue, f: &mut dyn FnMut(&mut ScriptVm, ScriptObject) -> Option<ScriptValue>) {
    let Some(array) = list.as_array() else { return };
    for i in 0..vm.bx.heap_ref().array_len(array) {
        let item = vm.bx.heap_ref().array_index(array, i, NoTrap);
        let Some(o) = item.as_object() else { continue };
        if let Some(new) = f(vm, o) {
            vm.bx.heap_mut().set_array_index(array, i, new, NoTrap);
            continue;
        }
        let layers = get(vm, o, "layers");
        walk(vm, layers, f);
        let layer = get(vm, o, "layer");
        if let Some(single) = layer.as_object() {
            let holder = vm.bx.heap_mut().new_array();
            vm.bx.heap_mut().array_push(holder, single.into(), NoTrap);
            walk(vm, holder.into(), f);
            let new = vm.bx.heap_ref().array_index(holder, 0, NoTrap);
            if new != layer {
                vm.bx.heap_mut().set_value(o, LiveId::from_str("layer").into(), new, NoTrap);
            }
        }
    }
}

/// Run the animators of the document `root` in its VM: each Text with
/// `animate:` or `inside:` becomes what its animator makes.
pub(crate) fn expand(vm: &mut ScriptVm, root: ScriptValue, errors: &mut Vec<ScriptErrorRecord>) {
    let Some(root) = root.as_object() else { return };
    // The document's width (1920 without a size).
    let size = get(vm, root, "size");
    let doc_width = match size.as_pod() {
        Some(pod) => f32::from_bits(vm.bx.heap_ref().pod_data(pod).1[0]) as f64,
        None => 1920.0,
    };
    let motion = vm.bx.heap_ref().value(vm.modules(), id!(motion).into(), NoTrap).as_object();
    let text_proto = motion.and_then(|m| vm.bx.heap_ref().value(m, id!(Text).into(), NoTrap).as_object());
    let mut module: Option<Result<ScriptObject, Vec<ScriptErrorRecord>>> = None;
    let layers = get(vm, root, "layers");
    walk(vm, layers, &mut |vm, o| {
        if !is_text(vm, o, text_proto) {
            return None;
        }
        let animate = get(vm, o, "animate");
        let inside = get(vm, o, "inside");
        if animate.is_nil() && inside.is_nil() {
            return None;
        }
        let exports = match module.get_or_insert_with(|| crate::modules::load_internal(vm, "typography", MODULE)) {
            Ok(exports) => *exports,
            Err(e) => {
                errors.extend(e.iter().cloned());
                return None;
            }
        };
        vm.capture_errors();
        let made = if !inside.is_nil() {
            let f = get(vm, exports, "type_inside");
            vm.call(f, &[o.into()])
        } else {
            let Some(name) = animate.as_id() else {
                errors.push(crate::modules::error_at(vm, o, "`animate` names a text animator (@fade_up, @typewriter, …): `makepad-motion animators` lists them"));
                return None;
            };
            let f = get(vm, exports, "type_animate");
            vm.call(f, &[name.into(), o.into(), doc_width.into()])
        };
        errors.extend(vm.take_error_records());
        if made.is_nil() {
            let name = animate.as_id().map(|id| id.to_string()).unwrap_or_default();
            let names: Vec<String> = all_names(vm, exports);
            let names: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
            errors.push(crate::modules::error_at(vm, o, &format!("`@{name}` is not a text animator.{} Animators: @{}", crate::load::did_you_mean(&name, &names), names.join(", @"))));
            return None;
        }
        if made.is_string_like() {
            let message = vm.bx.heap_ref().string_with(made, |_, s| s.to_string()).unwrap_or_default();
            errors.push(crate::modules::error_at(vm, o, &message));
            return None;
        }
        Some(made)
    });
}

fn all_names(vm: &ScriptVm, exports: ScriptObject) -> Vec<String> {
    let Some(all) = get(vm, exports, "type_animators").as_object() else { return Vec::new() };
    let mut out = Vec::new();
    vm.bx.heap_ref().map_entries(all).iter().for_each(|e| {
        if let Some(id) = e.key.as_id() {
            out.push(id.to_string());
        }
    });
    out
}

pub use makepad_motion_plain::is_cjk;

/// Whether `text` has any [`is_cjk`] character.
pub fn has_cjk(text: &str) -> bool {
    text.chars().any(is_cjk)
}

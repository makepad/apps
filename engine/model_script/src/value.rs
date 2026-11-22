//! Script value <-> engine type conversions, shared by the model builder and
//! every `game.*` verb.
//!
//! Ported faithfully from gamemaker's game_view.rs helpers — the semantics
//! here are load-bearing (a missing option must read as NIL, not as an error
//! value that NaNs every downstream cast).

use makepad_script::makepad_math::*;
use makepad_script::numeric::{NumericValue, NumericValueTag};
use makepad_script::*;


/// Positional argument, normalized so a missing slot reads as NIL.
pub fn arg(vm: &mut ScriptVm, args: ScriptObject, index: usize) -> ScriptValue {
    let v = vm.bx.heap_ref().vec_value(args, index, NoTrap);
    if v.is_err() {
        NIL
    } else {
        v
    }
}

pub fn arg_f32(vm: &mut ScriptVm, args: ScriptObject, index: usize) -> f32 {
    let v = arg(vm, args, index);
    let ip = vm.trap_ip();
    vm.bx.heap_ref().cast_to_f64(v, ip) as f32
}

pub fn arg_f64(vm: &mut ScriptVm, args: ScriptObject, index: usize) -> f64 {
    let v = arg(vm, args, index);
    let ip = vm.trap_ip();
    vm.bx.heap_ref().cast_to_f64(v, ip)
}

/// An entity id argument. A value that is not one is said out loud rather
/// than read as entity 0 in silence: an object that names itself (a string
/// `what` key, as a placement handle has) is a script error at the calling
/// line saying what it is; any other non-number is the usual wrong-type
/// warning. Nil stays 0 (no entity), quietly.
pub fn arg_id(vm: &mut ScriptVm, args: ScriptObject, index: usize) -> u64 {
    let v = arg(vm, args, index);
    if let Some(obj) = v.as_object() {
        let what = vm.bx.heap_ref().value(obj, id!(what).into(), NoTrap);
        if what.is_string_like() {
            let what = value_string(vm, what);
            script_err_type_mismatch!(vm.trap(), "expected an entity id, got {what}");
            return 0;
        }
    }
    value_id(vm, v)
}

pub fn arg_string(vm: &mut ScriptVm, args: ScriptObject, index: usize) -> String {
    let v = arg(vm, args, index);
    value_string(vm, v)
}

pub fn value_f32(vm: &mut ScriptVm, v: ScriptValue) -> f32 {
    let ip = vm.trap_ip();
    let x = vm.bx.heap_ref().cast_to_f64(v, ip) as f32;
    if !v.is_number() && !v.is_nil() {
        let got = describe(vm, v);
        vm.push_diagnostic(&format!("expected a number, got {got} — read as {x}"));
    }
    x
}

pub fn value_f64(vm: &mut ScriptVm, v: ScriptValue) -> f64 {
    let ip = vm.trap_ip();
    let x = vm.bx.heap_ref().cast_to_f64(v, ip);
    if !v.is_number() && !v.is_nil() {
        let got = describe(vm, v);
        vm.push_diagnostic(&format!("expected a number, got {got} — read as {x}"));
    }
    x
}

pub fn value_id(vm: &mut ScriptVm, v: ScriptValue) -> u64 {
    value_f64(vm, v) as u64
}

/// A wrong-typed value as an agent reads it: `` `true` (a bool) ``, `"2" (a string)`.
fn describe(vm: &mut ScriptVm, v: ScriptValue) -> String {
    if let Some(b) = v.as_bool() {
        return format!("`{b}` (a bool)");
    }
    if v.is_string_like() {
        let t = value_string(vm, v);
        let t: String = t.chars().take(40).collect();
        return format!("\"{t}\" (a string)");
    }
    format!("a {:?}", v.value_type())
}

pub fn value_bool(vm: &mut ScriptVm, v: ScriptValue) -> bool {
    if let Some(b) = v.as_bool() {
        return b;
    }
    value_f32(vm, v) != 0.0
}

pub fn value_string(vm: &mut ScriptVm, v: ScriptValue) -> String {
    vm.bx.heap_mut().cast_to_owned_string(v, "copying a game string argument")
        .unwrap_or_default()
}

/// Report a wrong-typed option to the agent instead of coercing it silently.
///
/// A silent fallback is the most expensive failure mode we have: `size: [1,2,3]`
/// became `vec3(0,0,0)` and `color: "#ff0000"` became grey, so a generated game
/// looked correct in source and rendered an empty world with nothing to explain
/// why. The engine already hard-fails unknown verbs with a did-you-mean; typed
/// options deserve the same courtesy. NIL is exempt — a missing option is not a
/// mistake, it is a default (that distinction is load-bearing, see the module
/// docs).
pub(crate) fn wrong_type(vm: &mut ScriptVm, expected: &str, hint: &str) {
    // an ERROR diagnostic at the script line: no meaning can be read from the value, so the
    // eval is refused (the capture), and it is listed by `inspect script errors`
    vm.push_diagnostic_error(&format!("expected {expected} — {hint}"));
}

pub fn value_vec3(vm: &mut ScriptVm, v: ScriptValue) -> Vec3f {
    // An array is no vector: it would cast to NaN and surface far away as a
    // non-finite build input instead of naming the mistake here.
    if v.as_array().is_some() {
        wrong_type(vm, "a vec3", "write vec3(x, y, z); an array like [x, y, z] is not a position");
        return vec3f(0.0, 0.0, 0.0);
    }
    let ip = vm.trap_ip();
    { let numeric = NumericValue::from_script_value_heap(vm.heap(), v, ip); match numeric.kind() {
        NumericValueTag::Vec3 => { let v = numeric.as_vec3().unwrap(); v },
        NumericValueTag::F64 => { let f = numeric.as_f64().unwrap(); vec3f(f as f32, f as f32, f as f32) },
        _ => {
            if !v.is_nil() {
                wrong_type(vm, "a vec3", "write vec3(x, y, z); an array like [x, y, z] is not a position");
            }
            vec3f(0.0, 0.0, 0.0)
        }
    } }
}

pub fn value_color(vm: &mut ScriptVm, v: ScriptValue) -> Vec4f {
    let ip = vm.trap_ip();
    { let numeric = NumericValue::from_script_value_heap(vm.heap(), v, ip); match numeric.kind() {
        NumericValueTag::Color => { let c = numeric.as_color().unwrap(); c },
        NumericValueTag::Vec4 => { let c = numeric.as_vec4().unwrap(); c },
        _ => {
            if !v.is_nil() {
                wrong_type(vm, "a color literal", "write #ff8800 (or #xff88ee when a digit is followed by e); a quoted \"#ff8800\" string is not a color");
            }
            vec4(0.8, 0.8, 0.8, 1.0)
        }
    } }
}

pub fn vec3_value(vm: &mut ScriptVm, v: Vec3f) -> ScriptValue {
    vm.numeric_vec3(v)
}

/// Missing option keys come back as error values, not NIL — normalize both to
/// NIL so `is_nil()` means "not provided".
pub fn opts_value(vm: &mut ScriptVm, opts: ScriptObject, key: LiveId) -> ScriptValue {
    let v = vm.bx.heap_ref().value(opts, key.into(), NoTrap);
    if v.is_err() {
        NIL
    } else {
        v
    }
}

pub fn opt_f32(vm: &mut ScriptVm, opts: ScriptObject, key: LiveId, default: f32) -> f32 {
    let v = opts_value(vm, opts, key);
    if v.is_nil() {
        return default;
    }
    if v.is_number() {
        return value_f32(vm, v);
    }
    // a silent cast is the expensive failure: `teleport_lost: true` read as 1 m made followers
    // blink back to their target every tick, and nothing said why
    let ip = vm.trap_ip();
    let x = vm.bx.heap_ref().cast_to_f64(v, ip) as f32;
    let got = describe(vm, v);
    let hint = if v.as_bool().is_some() { "; give a number, or leave the option out for its default" } else { "" };
    vm.push_diagnostic(&format!("option `{key}`: expected a number, got {got} — read as {x}{hint}"));
    x
}

pub fn opt_bool(vm: &mut ScriptVm, opts: ScriptObject, key: LiveId, default: bool) -> bool {
    let v = opts_value(vm, opts, key);
    if v.is_nil() {
        default
    } else {
        value_bool(vm, v)
    }
}

pub fn opt_vec3(vm: &mut ScriptVm, opts: ScriptObject, key: LiveId, default: Vec3f) -> Vec3f {
    let v = opts_value(vm, opts, key);
    if v.is_nil() {
        default
    } else {
        value_vec3(vm, v)
    }
}

pub fn opt_color(vm: &mut ScriptVm, opts: ScriptObject, key: LiveId, default: Vec4f) -> Vec4f {
    let v = opts_value(vm, opts, key);
    if v.is_nil() {
        default
    } else {
        value_color(vm, v)
    }
}

/// Uniform asset recolor: `tint` is the preferred spelling, while `color`
/// remains the world-tool/primitive spelling and maps to the same lane.
pub fn opt_tint(vm: &mut ScriptVm, opts: ScriptObject, default: Vec4f) -> Vec4f {
    if !opts_value(vm, opts, id!(tint)).is_nil() {
        opt_color(vm, opts, id!(tint), default)
    } else {
        opt_color(vm, opts, id!(color), default)
    }
}

pub fn opt_string(vm: &mut ScriptVm, opts: ScriptObject, key: LiveId) -> Option<String> {
    let v = opts_value(vm, opts, key);
    if v.is_nil() {
        None
    } else {
        Some(value_string(vm, v))
    }
}

pub fn fn_ref(vm: &mut ScriptVm, v: ScriptValue) -> Option<ScriptObjectRef> {
    let obj = v.as_object()?;
    Some(vm.bx.heap_mut().new_object_ref(obj))
}

/// Script `[...]` literals are ScriptArrays; some paths hand us vec-objects.
/// Accept either.
pub fn list_len(vm: &ScriptVm, v: ScriptValue) -> usize {
    if let Some(a) = v.as_array() {
        vm.bx.heap_ref().array_len(a)
    } else if let Some(o) = v.as_object() {
        vm.bx.heap_ref().vec_len(o)
    } else {
        0
    }
}

pub fn list_value(vm: &mut ScriptVm, v: ScriptValue, index: usize) -> ScriptValue {
    if let Some(a) = v.as_array() {
        vm.bx.heap_ref().array_index(a, index, NoTrap)
    } else if let Some(o) = v.as_object() {
        vm.bx.heap_ref().vec_value(o, index, NoTrap)
    } else {
        NIL
    }
}

/// Read a `points: [vec3, ...]` option into a Vec.
pub fn opt_points(vm: &mut ScriptVm, opts: ScriptObject, key: LiveId) -> Vec<Vec3f> {
    let v = opts_value(vm, opts, key);
    let len = list_len(vm, v);
    let mut out = Vec::with_capacity(len);
    for index in 0..len {
        let item = list_value(vm, v, index);
        out.push(value_vec3(vm, item));
    }
    out
}

/// Report every option key of `opts` not in `allowed` through `log`
/// ("game.{verb}: unknown option ..."), so a typo is named, not ignored.
pub fn warn_unknown_keys(vm: &mut ScriptVm, log: &dyn Fn(String), verb: &str, opts: ScriptObject, allowed: &[LiveId]) {
    let len = vm.bx.heap_ref().iter_len(opts);
    for index in 0..len {
        let kv = vm.bx.heap_ref().iter_key_value(opts, index, NoTrap);
        let Some(key) = kv.key.as_id() else {
            continue;
        };
        if !allowed.contains(&key) {
            let names=allowed.iter().map(|id|id.to_string()).collect::<Vec<_>>().join(", ");
            log(format!("game.{verb}: unknown option `{key}` (ignored); supported options: {names}"));
        }
    }
}

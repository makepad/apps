//! `mod.forms`: the animation forms a Motion document and its 3D scenes
//! share (`tween keys sequence repeat spring bezier color asset`, `rgb`
//! `rgba` `memo`, and the pure timing and lyric helpers of `kit_pure.splash`),
//! one implementation. `mod.motion` and `mod.scene` re-export these same
//! functions ([`reexport`]), so `tween` in a document and `scene.tween` in
//! its scene are the identical value. The forms' records (`Tween{}`,
//! `Keys{}`, ...) are the prototypes in the module; makepad-motion3d reads a
//! form by its prototype ([`READS_AS`], `__forms`).
//!
//! One source file for two crates: makepad-motion compiles it (its Tween,
//! Keys, ... are typed records, see `layers.rs`) and makepad-motion3d
//! includes it by path (its scene VMs without a Motion document get plain
//! records, [`plain_types`]). It names nothing of either crate.

use makepad_script::*;

/// The prototypes of `mod.forms`.
pub const TYPES: &[&str] = &["Tween", "Keys", "Sequence", "Loop", "Blend", "Blend2", "Spring", "Bezier", "Rate", "Asset"];

/// The scene native each form's record stands for.
pub const READS_AS: &[(&str, &str)] = &[("Tween", "tween"), ("Keys", "keys"), ("Loop", "repeat"), ("Spring", "spring"), ("Bezier", "bezier"), ("Asset", "asset")];

/// The functions (`mod.forms.tween = fn ...`).
pub const FORMS_SPLASH: &str = include_str!("forms.splash");
/// The pure helpers (`mod.forms.hash01 = fn ...`).
pub const KIT_PURE: &str = include_str!("kit_pure.splash");

fn key(name: &str) -> LiveId {
    LiveId::from_str_with_lut(name).unwrap_or_else(|_| LiveId::from_str(name))
}

/// The records as plain prototypes (a scene VM without a Motion document;
/// makepad-motion's own are typed, `layers.rs`).
#[allow(dead_code)]
pub fn plain_types(vm: &mut ScriptVm, module: ScriptObject) {
    for name in TYPES {
        let proto = vm.bx.heap_mut().new_object();
        vm.bx.heap_mut().set_value_def(module, key(name).into(), proto.into());
    }
}

/// The window of child `i` of `n` in a lagged group, as (start, length) of the
/// group's 0..1 time. `lag` 0 starts every child together (each window is the
/// whole time); `lag` 1 runs them one after another (windows 1/n long, no
/// overlap); between, each starts `lag` of a window after the one before and
/// the last ends exactly at 1.
pub fn lag_window(i: usize, n: usize, lag: f64) -> (f64, f64) {
    let lag = lag.max(0.0);
    let len = 1.0 / (1.0 + n.saturating_sub(1) as f64 * lag);
    (i as f64 * lag * len, len)
}

/// The alpha (0..1, linear in the child's window) of child `i` of `n` at group
/// time `t`: a pure function of `t`. Apply an ease to it as you would to any
/// tween progress.
pub fn lagged_alpha(t: f64, i: usize, n: usize, lag: f64) -> f64 {
    // The group's ends are exact: everything has arrived at 1.
    if t >= 1.0 {
        return 1.0;
    }
    let (start, len) = lag_window(i, n, lag);
    ((t - start) / len).clamp(0.0, 1.0)
}

/// Succession: the children run one after another, child `i` taking a share of
/// the group's time proportional to `weights[i]`; the alpha of each at `t`.
pub fn succession_alphas(t: f64, weights: &[f64], out: &mut Vec<f64>) {
    out.clear();
    let total: f64 = weights.iter().map(|w| w.max(0.0)).sum();
    let mut start = 0.0;
    for w in weights {
        if t >= 1.0 {
            out.push(1.0);
            continue;
        }
        let share = if total > 0.0 { w.max(0.0) / total } else { 1.0 / weights.len() as f64 };
        out.push(if share > 0.0 { ((t - start) / share).clamp(0.0, 1.0) } else { (t >= start) as u8 as f64 });
        start += share;
    }
}

/// The module's functions and natives, once its records are in it. The
/// errors the evaluation reports.
pub fn install(vm: &mut ScriptVm, module: ScriptObject) -> Vec<String> {
    // Which prototype is which form, for the scene reader (a private key:
    // `use mod.forms.*` leaves it out).
    let reads = vm.bx.heap_mut().new_object();
    for (name, op) in READS_AS {
        if let Some(proto) = vm.bx.heap_ref().value(module, key(name).into(), NoTrap).as_object() {
            vm.bx.heap_mut().set_value_def(reads, key(op).into(), proto.into());
        }
    }
    vm.bx.heap_mut().set_value_def(module, id_lut!(__forms).into(), reads.into());
    // `rgb(r, g, b)` / `rgba(r, g, b, a)`: a colour of 0..1 sRGB components,
    // as `#rrggbb` writes it. A component over 1 is light over 1, as
    // `emit()` gives it (kept in a `color: @hdr` document, clamped in a
    // display one).
    for name in [id_lut!(rgba), id_lut!(rgb)] {
        vm.add_method(module, name, script_args!(r = NIL, g = NIL, b = NIL, a = NIL), |vm, args| {
            let c = |vm: &ScriptVm, key: LiveId, default: f64| vm.bx.heap_ref().value(args, key.into(), NoTrap).as_number().unwrap_or(default);
            let rgba = [c(vm, id!(r), 0.0), c(vm, id!(g), 0.0), c(vm, id!(b), 0.0), c(vm, id!(a), 1.0)];
            if rgba[..3].iter().any(|v| *v > 1.0) {
                let parts = vm.bx.heap_mut().new_array();
                for v in [rgba[0].max(0.0), rgba[1].max(0.0), rgba[2].max(0.0), rgba[3].clamp(0.0, 1.0)] {
                    vm.bx.heap_mut().array_push(parts, v.into(), NoTrap);
                }
                let object = vm.bx.heap_mut().new_object();
                vm.bx.heap_mut().set_value_def(object, id_lut!(color).into(), parts.into());
                return object.into();
            }
            let b = |v: f64| ((v.clamp(0.0, 1.0) * 255.0).round() as u32) & 0xff;
            ScriptValue::from_color(b(rgba[0]) << 24 | b(rgba[1]) << 16 | b(rgba[2]) << 8 | b(rgba[3]))
        });
    }
    // `lagged(t, children, lag = 0.5)`: the alphas (0..1) of `children` (a
    // count or a list: its length) at group time `t` (0..1), each child's
    // window starting `lag` of a window after the one before it.
    // `then(t, parts)`: the same for children that run one after another;
    // `parts` is a count (equal shares) or a list of durations (any numbers:
    // the weights of the shares). Pure functions of `t`: a layer with
    // `opacity: lagged(u, 5)[i]` scrubs like any other property.
    vm.add_method(module, id_lut!(lagged), script_args!(t = NIL, children = NIL, lag = NIL), |vm, args| {
        let t = vm.bx.heap_ref().value(args, id!(t).into(), NoTrap).as_number().unwrap_or(0.0);
        let n = child_count(vm, vm.bx.heap_ref().value(args, id!(children).into(), NoTrap));
        let lag = vm.bx.heap_ref().value(args, id!(lag).into(), NoTrap).as_number().unwrap_or(0.5);
        let out = vm.bx.heap_mut().new_array();
        for i in 0..n {
            vm.bx.heap_mut().array_push(out, lagged_alpha(t, i, n, lag).into(), NoTrap);
        }
        out.into()
    });
    vm.add_method(module, id_lut!(then), script_args!(t = NIL, parts = NIL), |vm, args| {
        let t = vm.bx.heap_ref().value(args, id!(t).into(), NoTrap).as_number().unwrap_or(0.0);
        let parts = vm.bx.heap_ref().value(args, id!(parts).into(), NoTrap);
        let weights: Vec<f64> = match parts.as_array() {
            Some(list) => (0..vm.bx.heap_ref().array_len(list)).map(|i| vm.bx.heap_ref().array_index(list, i, NoTrap).as_number().unwrap_or(1.0)).collect(),
            None => vec![1.0; child_count(vm, parts)],
        };
        let mut alphas = Vec::new();
        succession_alphas(t, &weights, &mut alphas);
        let out = vm.bx.heap_mut().new_array();
        for a in alphas {
            vm.bx.heap_mut().array_push(out, a.into(), NoTrap);
        }
        out.into()
    });
    // `memo(f, t)`: f(t), computed once per frame (sub-frame) and time:
    // state many layers read (an eye's projected position that 300 HUD
    // boxes follow) is worked out once, however many property functions
    // ask for it. The results are forgotten at every evaluation
    // ([`clear_memo`]), so they are invisible to the document (the same t
    // gives the same value).
    vm.add_method(module, id_lut!(memo), script_args!(f = NIL, t = NIL), |vm, args| {
        let f = vm.bx.heap_ref().value(args, id!(f).into(), NoTrap);
        let t = vm.bx.heap_ref().value(args, id!(t).into(), NoTrap);
        let Some(tn) = t.as_number() else { return vm.call(f, &[t]) };
        // (by heap too: each document and scene VM has its own values under
        // the same handles)
        let heap = vm.bx.heap_ref().heap_key();
        if let Some(v) = MEMO.with(|m| m.borrow().iter().find(|e| e.heap == heap && e.f == f && e.t == tn.to_bits()).map(|e| e.value)) {
            return v;
        }
        let v = vm.call(f, &[t]);
        if v.is_err() {
            return v;
        }
        // Objects and arrays are held (rooted) while kept; other heap
        // values are recomputed.
        let root = if let Some(o) = v.as_object() {
            Some(MemoRoot::Object(vm.bx.heap_mut().new_object_ref(o)))
        } else if let Some(a) = v.as_array() {
            Some(MemoRoot::Array(vm.bx.heap_mut().new_array_ref(a)))
        } else if v.as_number().is_some() || v.as_bool().is_some() || v.as_color().is_some() {
            Some(MemoRoot::Plain)
        } else {
            None
        };
        if let Some(root) = root {
            MEMO.with(|m| m.borrow_mut().push(MemoEntry { heap, f, t: tn.to_bits(), value: v, _root: root }));
        }
        v
    });
    let mut errors = Vec::new();
    vm.capture_errors_nested();
    for (file, text) in [("mod.forms", FORMS_SPLASH), ("mod.forms.pure", KIT_PURE)] {
        vm.eval(ScriptMod::new("", "", file, 0, 0, text, &[]));
        errors.extend(vm.take_errors());
    }
    vm.capture_errors_restore();
    errors
}

/// A count given as a number or as a list (its length).
fn child_count(vm: &ScriptVm, value: ScriptValue) -> usize {
    match (value.as_number(), value.as_array()) {
        (Some(n), _) => n.max(0.0) as usize,
        (_, Some(list)) => vm.bx.heap_ref().array_len(list),
        _ => 0,
    }
}

/// Put every public name of `from` in `into` that `into` does not have
/// (the same values, not copies): how `mod.motion` and `mod.scene` carry
/// the forms and the shared stdlib. Private keys (`__...`) stay.
pub fn reexport(vm: &mut ScriptVm, from: ScriptObject, into: ScriptObject) {
    let entries: Vec<(ScriptValue, ScriptValue)> = vm.bx.heap_ref().map_entries(from).iter().map(|e| (e.key, e.value)).collect();
    for (k, v) in entries {
        let Some(id) = k.as_id() else { continue };
        let mut head = [0u8; 2];
        if LiveId::lut_name_copy(id, &mut head) == 2 && head == *b"__" {
            continue;
        }
        let have = vm.bx.heap_ref().value(into, k, NoTrap);
        if have.is_nil() || have.is_err() {
            vm.bx.heap_mut().set_value_def(into, k, v);
        }
    }
}

/// One `memo(f, t)` result of the evaluation in progress.
struct MemoEntry {
    heap: usize,
    f: ScriptValue,
    t: u64,
    value: ScriptValue,
    _root: MemoRoot,
}

/// Holds a cached object or array alive while cached (never read).
#[allow(dead_code)]
enum MemoRoot {
    Object(ScriptObjectRef),
    Array(ScriptArrayRef),
    Plain,
}

thread_local! {
    static MEMO: std::cell::RefCell<Vec<MemoEntry>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Empty the `memo()` cache: a new evaluation (frame or sub-frame).
pub fn clear_memo() {
    MEMO.with(|m| m.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lag_windows_span_the_group_and_overlap_by_lag() {
        // lag 0: everyone moves together. lag 1: one after another.
        for i in 0..4 {
            assert_eq!(lagged_alpha(0.5, i, 4, 0.0), 0.5);
        }
        let a: Vec<f64> = (0..4).map(|i| lagged_alpha(0.5, i, 4, 1.0)).collect();
        assert_eq!(a, vec![1.0, 1.0, 0.0, 0.0]);
        // Any lag: the first starts at 0, the last ends at 1, each child is at 0 then 1.
        for lag in [0.0, 0.25, 0.5, 1.0, 3.0] {
            for n in [1usize, 2, 5, 9] {
                assert_eq!(lagged_alpha(0.0, 0, n, lag), 0.0);
                for i in 0..n {
                    assert_eq!(lagged_alpha(1.0, i, n, lag), 1.0, "lag {lag} n {n} i {i}");
                    let mut last = 0.0;
                    for k in 0..=100 {
                        let v = lagged_alpha(k as f64 / 100.0, i, n, lag);
                        assert!(v >= last && (0.0..=1.0).contains(&v));
                        last = v;
                    }
                }
                // Later children are never ahead of earlier ones.
                for i in 1..n {
                    assert!(lagged_alpha(0.37, i, n, lag) <= lagged_alpha(0.37, i - 1, n, lag));
                }
            }
        }
        let (start, len) = lag_window(3, 4, 0.5);
        assert!((start + len - 1.0).abs() < 1e-12 && (len - 0.4).abs() < 1e-12);
    }

    #[test]
    fn succession_gives_each_child_its_share_in_turn() {
        let mut out = Vec::new();
        succession_alphas(0.5, &[1.0, 1.0, 2.0], &mut out);
        assert_eq!(out, vec![1.0, 1.0, 0.0]);
        succession_alphas(0.75, &[1.0, 1.0, 2.0], &mut out);
        assert_eq!(out, vec![1.0, 1.0, 0.5]);
        succession_alphas(1.0, &[3.0, 0.0, 1.0], &mut out);
        assert_eq!(out, vec![1.0, 1.0, 1.0]);
        succession_alphas(0.2, &[0.0, 0.0], &mut out);
        assert_eq!(out, vec![0.4, 0.0]);
    }

    /// `mod.forms`'s Splash evaluates without an error: in a release build
    /// the hosts' debug_asserts on it do not run, and a doc comment that
    /// closed early once ran its English as code on every load.
    #[test]
    fn the_forms_module_evaluates_clean() {
        use makepad_script::*;
        let host = Box::leak(Box::new(ScriptVmHost::new(0i32, ())));
        let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
        let forms = vm.new_module(id!(forms));
        super::plain_types(&mut vm, forms);
        let errors = super::install(&mut vm, forms);
        assert!(errors.is_empty(), "{errors:#?}");
    }
}

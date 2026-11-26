//! Kernels in Motion documents: a document's `Kernel{...}` objects (its 2D
//! draws' and its scenes') load once, after it evaluated, into the one
//! registry its VM keeps (motion3d's [`M3Host`]); they compile and run with
//! motion3d's kernel runtime, and a
//! Draw2D draws its `Segment` output with `c.segs(k.out, {…})`, one bulk call
//! in place of a Splash loop per element. Load-time kernels give arrays; a
//! kernel reading `time`, a keyed param or a per-frame input runs every frame
//! when drawn, at the layer's time.

use crate::value::doc_host;
use makepad_script::*;
use makepad_stage_motion3d::script::{host, host_mut};

/// A document's 2D kernels, kept with its VM ([`crate::value::DocHost`]); the
/// kernels themselves are in the VM's [`makepad_stage_motion3d::script::M3Host`].
#[derive(Default)]
pub(crate) struct Kernels2d {
    /// Load-time Segment outputs (their arrays) and the kernel that made
    /// them, so a draw that gives its own data (`with:`) runs it again.
    loaded: Vec<(ScriptArray, ScriptObjectRef)>,
}

/// After the document (and its modules) evaluated: its `Kernel{..}`
/// objects compiled and loaded (makepad_stage_motion3d::objects). A
/// load-time output of `Segment` records reads as `c.segs` takes them: nine
/// numbers a segment. Errors as `file:line:col: message`.
pub(crate) fn load(vm: &mut ScriptVm, first_body: usize) -> Vec<String> {
    let Some(entries) = host(vm).and_then(|h| h.kernel_entries.clone()) else { return Vec::new() };
    let Some(mut k) = host_mut(vm).map(|h| std::mem::take(&mut h.kernels)) else { return Vec::new() };
    let errors = makepad_stage_motion3d::objects::load(vm, &mut k, &entries, first_body);
    let mut loaded = Vec::new();
    let outputs: Vec<(ScriptObject, usize, String)> = k.outputs.iter().map(|(d, (id, buf))| (*d, *id, buf.clone())).collect();
    for (decl, id, buf) in outputs {
        let dk = &k.list[id];
        if dk.outputs.iter().all(|o| o.name != buf || o.ty != "Segment") {
            continue;
        }
        let Some(arr) = k.objects.get(&decl).and_then(|v| v.as_array()) else { continue };
        let flat: Vec<f64> = (0..vm.bx.heap_ref().array_len(arr)).map(|i| vm.bx.heap_ref().array_index(arr, i, NoTrap).as_number().unwrap_or(0.0)).collect();
        let segs = vm.bx.heap_mut().new_array();
        for s in flat.chunks_exact(11) {
            for x in [s[0], s[1], s[3], s[4], s[10], s[6], s[7], s[8], s[9]] {
                vm.bx.heap_mut().array_push_unchecked(segs, x.into());
            }
        }
        // The kernel behind the array, for a draw with its own data.
        let (spec, dk_object) = (dk.spec, dk.object);
        let handle = vm.bx.heap_mut().new_object();
        let op = LiveId::from_str_with_lut("kernel_buf").unwrap_or(LiveId::from_str("kernel_buf"));
        vm.bx.heap_mut().set_value_def(handle, id!(op).into(), op.into());
        vm.bx.heap_mut().set_value_def(handle, id!(kernel).into(), (id as f64).into());
        let count = spec.map(|so| vm.bx.heap_ref().value(so, id!(count).into(), NoTrap)).filter(|v| v.as_number().is_some()).unwrap_or(NIL);
        vm.bx.heap_mut().set_value_def(handle, id!(count).into(), count);
        let name = vm.new_string_with(|_, b| b.push_str(&buf));
        vm.bx.heap_mut().set_value_def(handle, id!(buffer).into(), name);
        vm.bx.heap_mut().set_value_def(handle, id!(spec).into(), spec.map(|s| s.into()).unwrap_or(NIL));
        k.keep(vm, decl.into(), segs.into());
        // `k.out` read from the document's draw code gives the same.
        if let Some(o) = dk_object {
            vm.bx.heap_mut().set_value_def(o, LiveId::from_str(&buf).into(), segs.into());
        }
        loaded.push((segs, vm.bx.heap_mut().new_object_ref(handle)));
    }
    if let Some(h) = host_mut(vm) {
        h.kernels = k;
    }
    doc_host(vm).kernels.loaded = loaded;
    errors.into_iter().map(|e| format!("{}:{}:{}: kernel `{}`: {}", e.file, e.line, e.col, e.kernel, e.message)).collect()
}

/// Whether a literal the document's code pushes at these places (body,
/// opcode), or the `let` named `let_name`, is compiled into one of its
/// kernels: an edit there shows once the document loads again.
pub(crate) fn compiles(vm: &ScriptVm, pushed: &[(u16, u32, bool)], let_name: Option<&str>) -> bool {
    let Some(h) = host(vm) else { return false };
    let k = &h.kernels;
    if k.fn_sites.is_empty() {
        return false;
    }
    if let_name.is_some_and(|n| k.constants.iter().any(|c| c == n)) {
        return true;
    }
    pushed.iter().any(|(body, index, _)| {
        vm.bx.code_ref().ip_to_loc(ScriptIp { body: *body, index: *index }).is_some_and(|l| k.compiles_at(l.file(), l.line(), l.col()))
    })
}

/// Why a patch of a kernel's own record field shows only after a load.
pub(crate) const SETUP_READ: &str = "read into a kernel when the document loaded (its `count`)";

/// Whether the objects made at `site` include one of the document's kernels
/// or sims (`Kernel{count ..}`, `Sim{..}`): their load copied its fields into
/// the kernel runtime, which runs from that copy, so a value set in one shows
/// once the document loads again.
pub(crate) fn setup_reads(vm: &ScriptVm, site: ScriptIp) -> bool {
    host(vm).is_some_and(|h| h.kernels.setup_sites.contains(&site))
}

/// A per-frame kernel output (`k.out`) run at layer time `t` (`with`: the
/// draw's own inputs and params, see `kernel_frame`): its `Segment` records
/// as they are (ends' xyz in document units, linear colour, width: eleven
/// floats each). `None` when `value` is not a kernel's output.
pub(crate) fn frame_segments(vm: &mut ScriptVm, value: ScriptValue, t: f64, with: Option<ScriptObject>) -> Option<Result<std::sync::Arc<Vec<f32>>, String>> {
    // A load-time output drawn with the draw's own data: its kernel runs now.
    let value = makepad_stage_motion3d::objects::resolve(&host(vm)?.kernels, value);
    let loaded = with.and(value.as_array()).and_then(|a| doc_host(vm).kernels.loaded.iter().find(|(arr, _)| *arr == a).map(|(_, r)| r.as_object()));
    let obj = match loaded {
        Some(handle) => handle,
        None => value.as_object()?,
    };
    let op = vm.bx.heap_ref().value(obj, id!(op).into(), NoTrap).as_id()?;
    if op != LiveId::from_str_with_lut("kernel_buf").unwrap_or(LiveId::from_str("kernel_buf")) {
        return None;
    }
    let k = std::mem::take(&mut host_mut(vm)?.kernels);
    let run = makepad_stage_motion3d::script::kernel_frame(vm, &k, obj, t, with);
    if let Some(h) = host_mut(vm) {
        h.kernels = k;
    }
    Some(run)
}

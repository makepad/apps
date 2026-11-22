//! Fab's GPU-first slicer core. The execution model the slicer's GPU stages share:
//! - one submission per batch: every pass of a batch is recorded into one compute pass, and the
//!   batch's output is the one readback after it;
//! - layers as a grid dimension: a pass's elements span every plane of the batch;
//! - dispatch sizes the GPU writes itself (indirect), from the counts of the pass before;
//! - compaction: a pass runs over the active elements only, listed by a scan in id order, so
//!   the output never depends on how the work was split;
//! - capacities from the part's last batch, with an overflow word: a batch past them delivers
//!   nothing and runs again with them doubled.
//!
//! It runs on the plain HAL compute path ([`gpu`]): no other engine's job runtime. A part's
//! field is the SDF engine's program, evaluated by `mod.sdf.Eval` ([`field`]).
pub mod batch;
pub mod field;
pub mod gpu;
pub mod probe;
pub mod trace;

use makepad_platform::ScriptVm;

/// Registers the slicer's kernels (script module `mod.gpuslice`) and the field evaluator they
/// extend (`mod.sdf.Eval`, under the draw library's `mod.sdf`).
pub fn script_mod(vm: &mut ScriptVm) {
    fab_sdf_engine::makepad_draw::script_mod(vm);
    fab_sdf_engine::compute::script_mod(vm);
    batch::script_mod(vm);
    probe::script_mod(vm);
    field::script_mod(vm);
    trace::script_mod(vm);
}

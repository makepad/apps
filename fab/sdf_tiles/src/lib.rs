//! A distance field shown as real triangles: rays traced on the CPU from the view become tiles,
//! coarse to fine over a few frames, drawn by the GPU into a G-buffer and lit pass.

use makepad_draw::*;

pub(crate) mod diff;
pub mod draw;
pub mod field;
pub mod pick;
pub mod surface;
pub mod trace;

/// Register after `makepad_render::script_mod`: lit lanes share its `mod.draw.KeyLobe`.
pub fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
    makepad_render_material::builtin::register(vm);
    draw::register_geometry(vm);
    draw::script_mod(vm)
}

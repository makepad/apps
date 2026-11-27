//! 3D motion graphics for Motion's `Scene3D` layer (EDITS.md §10.2).
//! See `MOTION3D.md` beside this crate for the authoring guide.

pub mod anim;
pub mod crowd;
pub mod doc;
pub mod engines;
pub mod eval;
/// Other 3D tools' words for a scene's fields, in its warnings only.
mod foreign;
// `mod.forms`: one source file with makepad-motion (see its header).
#[path = "../../motion/src/forms.rs"]
pub mod forms;
pub mod frame;
pub mod frame_tool;
pub mod glb;
pub mod kits;
pub mod kernels;
pub mod objects;
pub mod layer;
pub mod layout;
pub mod models;
pub mod particles;
pub mod physics;
pub mod scene;
pub mod seq3d;
pub mod script;
pub mod shared;
pub mod text3d_node;

#[cfg(test)]
mod tests;

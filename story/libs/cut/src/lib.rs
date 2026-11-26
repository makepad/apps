//! Stage's edit model: the `mod.cut` node set,
//! loading a cut's Splash into a typed document, evaluating it as a pure
//! function of time into render, audio and caption plans, its
//! dependencies, the automatic reframing camera, and the audio curves of
//! the mix. No GPU, decoders, asset I/O, clocks or UI.

pub mod audio;
/// Where an app-run clip shows, for overlays bound to it (lane M4).
pub mod bind;
mod eval;
pub mod generate;
pub mod mix;
/// Motion graphics in a cut: `Graphic` nodes (lane M1).
pub mod graphic;
mod load;
pub mod migrate;
mod model;
mod nest;
pub mod nodes;
pub mod reframe;
mod still;
/// A lexical view of an edit's (or deck's) Splash: where the final
/// `Flow{...}` and each `let` constructor are, and the extent of every field,
/// for the source-preserving writers.
pub mod source;
pub mod subtitle;
/// Musical and spoken time in cuts (lane M1).
pub mod timing;

pub use eval::*;
pub use generate::{GenState, GenTake, GenerateNode};
pub use graphic::{GraphicNode, GraphicSrc};
pub use load::{with_cut_vm, MAX_ACTIVE_CLIPS};
pub use model::*;
pub use nest::{CutTree, MAX_NEST_DEPTH};
pub use nodes::{animatable, register, register_document, PRELUDE};
pub use subtitle::Caption;

pub use makepad_motion::tree;

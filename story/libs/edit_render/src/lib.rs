//! The cut renderer: a cut drawn frame by frame
//! on locked time. [`engine`] loads a cut (every exact reference resolved
//! on a worker, effects built on the pool) and records frame `t` into
//! offscreen passes once the exact clip frames it shows have arrived;
//! [`render`] reads each frame back and encodes it with its share of the
//! offline [`audio`] mixdown; [`preview`] is the CUT page's seam onto the
//! same engine; [`publish`] files a finished render as the cut's render.
//!
//! Everything runs on the host's UI thread only as bounded per-paint steps:
//! decoding (the reader's workers), geometry builds, conversion, hashing,
//! mixing and encoding (the task pool) never do.

pub mod audio;
pub mod compose;
pub mod engine;
pub mod frames;
/// Motion graphics and titles in the compositor (lane M1).
pub mod graphic;
pub mod media;
/// DECK's slide changes: two pictures mixed by a transition.
pub mod mixer;
pub mod mixdown;
#[cfg(feature = "export")]
pub mod mux;
pub mod preview;
#[cfg(feature = "store")]
pub mod job;
#[cfg(all(feature = "store", feature = "export"))]
pub mod publish;
pub mod render;
pub mod segments;
pub mod sound;
pub mod thumbnail;

pub use engine::{CutEngine, CutJob, EngineStatus};
/// The Scene3D coverage maps an inspect grab reads ([`CutEngine::set_scene_coverage`]).
pub use makepad_motion_draw::coverage;
pub use mixer::SlideMixer;
pub use media::{LocalMedia, MediaKind, MediaResolver, ResolvedMedia};
#[cfg(feature = "store")]
pub use media::StoreMedia;
pub use preview::{CutPreview, PreviewSource, PreviewStatus};
pub use render::{Captions, CutRenderer, RenderOptions, RenderQuality, RenderResult, RenderState};
pub use thumbnail::{CutThumbnail, Thumbnail};

use makepad_widgets::ScriptVm;

/// Register the compositor's shaders. After `makepad_widgets::script_mod`
/// and the installed packs' widgets (`makepad_content_packs::script_mod`, or
/// the host's own registration of them: a Stage build with video does it).
pub fn script_mod(vm: &mut ScriptVm) {
    makepad_motion_draw::script_mod(vm);
    compose::script_mod(vm);
}

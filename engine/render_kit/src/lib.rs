//! The rendering kit: every VFX a motion graphic or an
//! edit clip needs, as GPU passes on Makepad's pipeline, each a pure
//! function of `t` and keyable parameters.
//!
//! - [`catalog`]: every effect and its parameters (kinds, ranges, defaults,
//!   meaning); the single source for validation and the authoring guide.
//! - [`anim`], [`stack`]: keyable values and effect stacks, resolved at `t`,
//!   built from authored names with teaching diagnostics.
//! - [`kit`] (the `gpu` feature): [`RenderKit`] records a stack as offscreen passes and returns
//!   the result texture (`apply`), or renders a layer the host draws at any
//!   time for temporal effects (`apply_layer`: motion blur, echo).
//! - [`space`], [`particles`], [`path`], [`temporal`], [`color`], [`budget`]:
//!   the pure maths behind them (camera and planes, deterministic particles,
//!   path trimming and taper, shutter sampling, colour tables, the
//!   per-frame budget), each unit-tested.
//!
//! Hosts: a Motion document's layer `effects: [...]` and an edit clip's Fx
//! stack both build an [`EffectStack`] and call the kit from their draw.

pub mod anim;
pub mod budget;
pub mod catalog;
pub mod catalogue;
pub mod color;
#[cfg(feature = "gpu")]
pub mod kit;
pub mod particles;
pub mod path;
#[cfg(feature = "gpu")]
pub mod shaders;
pub mod space;
pub mod stack;
pub mod temporal;

pub use anim::{Anim, Ease, Key, Sampler, Value};
pub use budget::{Budget, Quality};
pub use catalogue::{catalogue, catalogue_json, strip_preset, EffectEntry, ParamEntry, StripPreset};
pub use catalog::{EffectInfo, EffectKind, ParamInfo, ParamKind, CAMERA, EFFECTS};
#[cfg(feature = "gpu")]
pub use kit::{Applied, FnLayer, FxContext, LayerSource, Painted, RenderKit, TextureLayer};
#[cfg(feature = "gpu")]
pub use shaders::KitBlend;
pub use space::{Camera, CameraSpec};
pub use stack::{Diagnostics, Effect, EffectStack, ResolvedEffect};
pub use temporal::Affine2;

#[cfg(feature = "gpu")]
use makepad_widgets::ScriptVm;

/// Register the kit's shaders. After `makepad_widgets::script_mod`.
#[cfg(feature = "gpu")]
pub fn script_mod(vm: &mut ScriptVm) {
    shaders::script_mod(vm);
}

/// The authoring guide (Markdown) generated from the catalog: every effect
/// with every parameter, for people and for the AI authoring prompt.
pub fn guide() -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let mut category = None;
    for effect in EFFECTS {
        if category != Some(effect.category) {
            category = Some(effect.category);
            let _ = writeln!(out, "\n## {}\n", effect.category.title());
        }
        let _ = writeln!(out, "### `{}`{}\n\n{}\n", effect.name, if effect.temporal { " (temporal)" } else { "" }, effect.doc);
        let _ = writeln!(out, "| param | takes | meaning |\n|---|---|---|");
        for p in effect.params {
            let _ = writeln!(out, "| `{}` | {} | {} |", p.name, p.kind.syntax().replace('|', "\\|"), p.doc);
        }
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "\n## Camera (`camera: Camera{{...}}`)\n\n| param | takes | meaning |\n|---|---|---|");
    for p in CAMERA {
        let _ = writeln!(out, "| `{}` | {} | {} |", p.name, p.kind.syntax(), p.doc);
    }
    out
}

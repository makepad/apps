//! Content expansion packs.
//!
//! Two kinds of provider plug into a host here:
//!
//! * [`EffectPack`] — looks, effects and transitions named by preset
//!   (`"vjfx/<name>"` in a document), library effect documents, and the
//!   procedural engines a 3D scene can grow geometry from.
//! * [`TextPack`] — kinetic text: a kit per preset name drawing words.
//!
//! An effect pack may bring a preset picker widget ([`picker`]).
//!
//! A host installs the packs it carries before it builds its first view
//! ([`install_effects`], [`install_text`]) and registers their widgets with
//! [`script_mod`]. Everything that draws (Motion's frames, the edit renderer,
//! the edit tools) asks this crate, never a pack's own crate, so it builds and
//! runs with no pack installed: a document naming a pack's preset is then
//! reported as needing it.
//!
//! The interface is object safe and names only Makepad types (textures,
//! passes, the VM, the audio picture, the engine vertex stream), so a pack
//! compiled into the host and one loaded at run time install the same way.

pub mod effects;
pub mod picker;
pub mod text;

pub use effects::{EffectPack, EffectView, PreparedEffect};
pub use text::{TextFrame, TextPack, TextView};

use std::sync::{Arc, RwLock};

static EFFECTS: RwLock<Vec<Arc<dyn EffectPack>>> = RwLock::new(Vec::new());
static TEXT: RwLock<Vec<Arc<dyn TextPack>>> = RwLock::new(Vec::new());

/// Install an effect pack (once per name; a second of the same name is ignored).
pub fn install_effects(pack: Arc<dyn EffectPack>) {
    let mut packs = EFFECTS.write().unwrap_or_else(|e| e.into_inner());
    if !packs.iter().any(|p| p.name() == pack.name()) {
        packs.push(pack);
    }
}

/// Install a kinetic text pack (once per name).
pub fn install_text(pack: Arc<dyn TextPack>) {
    let mut packs = TEXT.write().unwrap_or_else(|e| e.into_inner());
    if !packs.iter().any(|p| p.name() == pack.name()) {
        packs.push(pack);
    }
}

/// The installed effect packs, in install order.
pub fn effect_packs() -> Vec<Arc<dyn EffectPack>> {
    EFFECTS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The installed kinetic text packs, in install order.
pub fn text_packs() -> Vec<Arc<dyn TextPack>> {
    TEXT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Register every installed pack's widgets on `mod.widgets`, after
/// `makepad_widgets::script_mod`. A host whose own components register a
/// pack's widgets already (Stage's live frame owner registers the VJ's)
/// need not call it.
pub fn script_mod(vm: &mut makepad_widgets::ScriptVm) {
    for pack in effect_packs() {
        pack.script_mod(vm);
    }
    for pack in text_packs() {
        pack.script_mod(vm);
    }
}

//! Kinetic text packs: a kit per preset name, drawing a layer's words.

use makepad_audio_reactive::AudioFrame;
use makepad_widgets::{Cx, Cx2d, DrawPassId, ScriptVm, Texture};
use std::path::PathBuf;
use std::sync::Arc;

/// A provider of kinetic text kits.
pub trait TextPack: Send + Sync {
    /// The pack's name (`"kinetic"`).
    fn name(&self) -> &'static str;
    /// Register the pack's widgets and shaders on `mod.widgets` (none is fine).
    fn script_mod(&self, vm: &mut ScriptVm);
    /// Whether the pack has a kit of this preset name.
    fn has_kit(&self, name: &str) -> bool;
    /// The preset names of the pack's kits, in the pack's order: what a
    /// KineticText's `preset:` can name.
    fn kits(&self) -> Vec<&'static str>;
    /// Dials a kit takes at most (p1..pN).
    fn max_dials(&self) -> usize;
    /// A view of the kit `name`; a name with no kit gives a view that
    /// reports why ([`TextView::error`]) and draws nothing.
    fn view(&self, name: &str) -> Box<dyn TextView>;
}

/// One frame's inputs to a kit.
pub struct TextFrame {
    pub time: f32,
    pub beat: f32,
    pub bpm: f32,
    pub energy: f32,
    /// Bass, mid, high (0..1).
    pub bands: [f32; 3],
    /// p1..pN where the host sets them (`None`: the kit's own).
    pub dials: Vec<Option<f32>>,
    /// The line's sung fraction (karaoke), when the words are a voice's.
    pub karaoke: Option<f32>,
    /// The picture under the layer.
    pub content: Option<Texture>,
    pub audio: Option<AudioFrame>,
}

/// A kit's view: a layer's words drawn on locked time.
pub trait TextView {
    /// Why the kit cannot draw (missing or failed), once known.
    fn error(&self) -> Option<String>;
    /// Make the kit ready (its first call compiles it): false when it failed.
    fn ready(&mut self, cx: &mut Cx) -> bool;
    /// The weight the kit builds its letters at, if it says.
    fn shape_weight(&mut self, cx: &mut Cx) -> Option<f32>;
    fn set_overlay(&mut self, cx: &mut Cx, overlay: bool);
    /// A variable font's `wght` and other axes (by tag).
    fn set_axes(&mut self, cx: &mut Cx, weight: Option<f32>, axes: &[(u32, f32)]);
    /// A font file in place of the kit's own.
    fn set_font(&mut self, cx: &mut Cx, font: Option<PathBuf>);
    /// The words, arrived `since` seconds ago.
    fn set_text(&mut self, cx: &mut Cx, text: &str, since: f32);
    /// Draw at `px`: the picture, or `None`.
    fn render(&mut self, cx: &mut Cx2d, px: (u32, u32), frame: &TextFrame) -> Option<Texture>;
    /// The first pass the last render drew.
    fn scene_pass(&self) -> DrawPassId;
}

/// The installed pack with a kit of this name.
pub fn pack_of(name: &str) -> Option<Arc<dyn TextPack>> {
    crate::text_packs().into_iter().find(|pack| pack.has_kit(name))
}

/// The preset names of every installed pack's kits, pack by pack.
pub fn kits() -> Vec<&'static str> {
    crate::text_packs().iter().flat_map(|pack| pack.kits()).collect()
}

/// What a host says when a KineticText needs a text pack it did not install.
pub fn missing(name: &str) -> String {
    format!("the kinetic text `{name}` needs the kinetic text pack, which this host has not installed")
}

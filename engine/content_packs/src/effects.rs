//! Effect packs: looks, effects and transitions by preset, library effect
//! documents, and procedural engine geometry.

use makepad_audio_reactive::AudioFrame;
use makepad_geom3d::fx_mesh::FxMesh;
use crate::picker::PresetPicker;
use makepad_widgets::{Cx, Cx2d, DVec2, DrawPassId, ScriptVm, Texture, WidgetRef};
use std::any::Any;
use std::sync::Arc;

/// A provider of effects. Its presets are documents it carries; a library
/// effect is a document of its kind the host read from the asset store.
pub trait EffectPack: Send + Sync {
    /// The pack's name (`"vj"`): one pack of a name is installed.
    fn name(&self) -> &'static str;
    /// Register the pack's widgets and shaders on `mod.widgets`.
    fn script_mod(&self, vm: &mut ScriptVm);
    /// Every bundled preset: its name and its document's source.
    fn presets(&self) -> &'static [(&'static str, &'static str)];
    /// The preset a name means (its canonical name and source), if the pack has it.
    fn resolve(&self, name: &str) -> Option<(&'static str, &'static str)>;
    /// Whether a preset is a transition (two pictures in, one out).
    fn is_transition(&self, name: &str) -> bool;
    /// Whether a preset draws text (a KineticText may name it).
    fn is_text(&self, name: &str) -> bool;
    /// A preset's display name (its document's `name:`, else from its id).
    fn preset_title(&self, name: &str, source: &str) -> String;
    /// A preset's catalogue category, by name (`"generative"`, ...).
    fn preset_category(&self, name: &str) -> Option<&'static str>;
    /// What a document's text says: its engine and its post stages.
    fn doc_facts(&self, source: &str) -> DocFacts;
    /// The stand-in picture (BGRA, `w` x `h`) the pack's catalogue shows for
    /// a name before its real thumbnail exists: the same weave for the same name.
    fn placeholder_bgra(&self, name: &str, w: usize, h: usize) -> Vec<u32>;
    /// Why a document cannot be drawn as a function of time (feedback,
    /// holds, simulation, a per-frame script), read from its text alone:
    /// the same answer on every host. `literal_text`: the layer gives its
    /// own words in place of the document's text source.
    fn seek_refusal(&self, source: &str, literal_text: bool) -> Option<String>;
    /// Parse a document for a seekable view (`key` names it in messages):
    /// its build, which runs off the UI thread, and its install.
    fn prepare(&self, cx: &mut Cx, key: &str, source: &str) -> Result<PreparedEffect, String>;
    /// A procedural engine's geometry (`engine` is the pack's kit name)
    /// after each `(dt, t)` step of `steps`, into `out`. `Err` when the pack
    /// has no such engine.
    fn engine_geometry(&self, engine: &str, params: &[(&str, f32)], texts: &[(&str, &str)], seed: u32, steps: &[(f32, f32)], out: &mut FxMesh) -> Result<(), String>;
    /// The widget type (on `mod.widgets`) the pack's preset picker is, if it has one.
    fn picker_widget(&self) -> Option<&'static str> {
        None
    }
    /// Run `f` on the pack's picker behind `widget`; false when it is not one.
    fn with_picker(&self, _widget: &WidgetRef, _f: &mut dyn FnMut(&mut dyn PresetPicker)) -> bool {
        false
    }
}

/// An effect document read as text (no VM).
#[derive(Clone, Debug, Default)]
pub struct DocFacts {
    /// The engine it runs on (`engine:`).
    pub engine: Option<String>,
    /// Entries in its `post:` list.
    pub post_stages: usize,
}

/// What an effect's build hands its install: the pack's own value.
pub struct BuiltEffect(pub Box<dyn Any + Send>);

/// A parsed document: its build (geometry and tables, on a worker) and its
/// install into a view (on the UI thread, with the build's result).
pub struct PreparedEffect {
    pub build: Box<dyn FnOnce() -> BuiltEffect + Send>,
    pub install: Box<dyn FnOnce(&mut Cx, BuiltEffect) -> Box<dyn EffectView>>,
}

/// One effect drawn on locked time: a host sets its inputs, dials and
/// clock, then renders it into a picture. Built seekable (no compositing of
/// its own, a manual clock, a still picture until its first frame).
pub trait EffectView {
    /// Give linear light back (an HDR frame), reading the input sRGB-encoded.
    fn set_linear_output(&mut self, on: bool);
    /// Whether a transition reads its two pictures itself (both inputs
    /// bound) instead of transforming their dissolve.
    fn wants_deck_inputs(&self) -> bool;
    /// The picture bound to input `index` (0: what is under it, 1: a
    /// transition's second picture).
    fn set_input_texture(&mut self, index: usize, texture: Option<Texture>);
    /// The card's dials p1..p3 in the document's dial order, and the lever.
    fn set_card_dials(&mut self, card: &[Option<f32>], lever: Option<f32>);
    /// The music's four levels (energy, bass, mid, high).
    fn set_signals(&mut self, audio: [f32; 4]);
    /// The song's picture at this frame.
    fn set_audio(&mut self, binding: Option<AudioFrame>);
    fn set_speed_scale(&mut self, scale: f32);
    fn set_bpm(&mut self, bpm: f64);
    /// The view's clock: the layer's time, beat and step.
    fn set_time(&mut self, cx: &mut Cx, local_time: f64, beat: f64, dt: f64);
    /// Draw at `pixels`: the picture, or `None` while it is not ready.
    fn render_live(&mut self, cx: &mut Cx2d, pixels: DVec2) -> Option<Texture>;
    /// The first and last pass the last render drew.
    fn live_passes(&self) -> Option<(DrawPassId, DrawPassId)>;
    /// Whether the last render painted after `after` (a pass serial).
    fn live_output_painted(&self, cx: &Cx, after: u64) -> bool;
    /// Whether the last render is the exact frame (not a stand-in).
    fn live_output_exact(&self) -> bool;
    /// The dial values the host set over the document's, by slot (tests and inspection).
    fn user_override(&self) -> Vec<Option<f32>>;
}

/// The installed pack that has preset `name`, with its canonical name and source.
pub fn resolve(name: &str) -> Option<(Arc<dyn EffectPack>, &'static str, &'static str)> {
    crate::effect_packs().into_iter().find_map(|pack| pack.resolve(name).map(|(canonical, source)| (pack.clone(), canonical, source)))
}

/// Every installed preset (name, source), pack by pack.
pub fn presets() -> Vec<(&'static str, &'static str)> {
    crate::effect_packs().iter().flat_map(|pack| pack.presets().iter().copied()).collect()
}

/// Whether `name` is an installed transition preset.
pub fn is_transition(name: &str) -> bool {
    crate::effect_packs().iter().any(|pack| pack.resolve(name).is_some() && pack.is_transition(name))
}

/// Whether `name` is an installed text preset.
pub fn is_text(name: &str) -> bool {
    crate::effect_packs().iter().any(|pack| pack.resolve(name).is_some() && pack.is_text(name))
}

/// The pack library effect documents are read by: the first installed.
pub fn library() -> Option<Arc<dyn EffectPack>> {
    crate::effect_packs().into_iter().next()
}

/// What a host says when a document needs an effect pack it did not install.
pub fn missing(what: &str) -> String {
    format!("{what} needs the VJ effect pack, which this host has not installed")
}

/// The stand-in picture for `name` (BGRA): the installed pack's weave, or
/// with none a plain ground.
pub fn placeholder_bgra(name: &str, w: usize, h: usize) -> Vec<u32> {
    match library() {
        Some(pack) => pack.placeholder_bgra(name, w, h),
        None => vec![0xff20_2024; w * h],
    }
}

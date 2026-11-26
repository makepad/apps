//! A frame's 3D scene, loaded into motion3d: the one way the drawing side
//! and the tools make a [`Scene3dDoc`] of what a frame's scene item shows.
//!
//! A `Scene3D{..}` the document wrote is an object of the document's own
//! VM, read there (no source text is cut out of the document or evaluated
//! again). A Text3D or Seq3D lowers to a scene template, a source of its
//! own that loads in a VM of its own with the inputs the layer gives it.

use crate::frame::ItemKind;
use crate::model::{InputValue, SceneSource};
use crate::modules::Modules;
use crate::signals::SignalSource;
use std::sync::Arc;
use makepad_stage_motion3d::doc::{LoadOptions, MediaResolver, ObjectOptions, Scene3dDoc};
use makepad_stage_motion3d::script::{Diagnostic, InputValue as M3Input, Loc};
use std::collections::HashMap;

pub use makepad_stage_motion3d::doc::SceneObject;
/// What a host gives a scene's files through ([`scene_diagnostics`]).
pub use makepad_stage_motion3d::doc::MediaResolver as SceneFiles;
pub use makepad_stage_motion3d::script::MediaRef as SceneMedia;

/// A template scene's inputs with the document's tempo (as `bpm`, unless the
/// layer gives one): what its load reads and keys on.
pub fn template_inputs(values: &[(String, InputValue)], bpm: f64) -> Vec<(String, InputValue)> {
    let mut values = values.to_vec();
    if !values.iter().any(|(name, _)| name == "bpm") {
        values.push(("bpm".into(), InputValue::Number(bpm)));
    }
    values
}

/// Document inputs as a scene's (`in(@name)`).
pub fn scene_inputs(values: &[(String, InputValue)]) -> HashMap<String, M3Input> {
    values.iter().filter_map(|(name, value)| Some((name.clone(), scene_input(value, false)?))).collect()
}

/// A document input for a scene (a document's own bool input reads as
/// 0/1; one inside a list or record as a bool).
fn scene_input(value: &InputValue, nested: bool) -> Option<M3Input> {
    Some(match value {
        InputValue::Number(n) => M3Input::Number(*n),
        InputValue::Text(text) => M3Input::Text(text.clone()),
        InputValue::Color(c) => M3Input::Color([c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32]),
        InputValue::Vec2(v) => M3Input::Vec3([v[0] as f32, v[1] as f32, 0.0]),
        InputValue::Bool(b) if nested => M3Input::Bool(*b),
        InputValue::Bool(b) => M3Input::Number(if *b { 1.0 } else { 0.0 }),
        InputValue::List(items) => M3Input::List(items.iter().filter_map(|item| scene_input(item, true)).collect()),
        InputValue::Record(fields) => M3Input::Record(fields.iter().filter_map(|(name, value)| Some((name.clone(), scene_input(value, true)?))).collect()),
        _ => return None,
    })
}

/// What loads the scene a frame's item shows: the item's scene and what
/// its load reads, without the rest of the item (its layers, its pictures).
#[derive(Clone)]
pub struct SceneRequest {
    pub scene: SceneSource,
    pub first_line: u32,
    /// The scene is the editor's document's own code.
    pub written: bool,
    /// The document's tempo, for `beats(n)` times in the scene.
    pub bpm: f64,
    pub modules: Modules,
    pub signals: Arc<dyn SignalSource>,
    /// The host time of the item's 0 (a template's song timing starts there).
    pub edit_offset: f64,
}

impl SceneRequest {
    /// The request of a frame's `ItemKind::Scene3dDoc` (`None` for any other item).
    pub fn of(kind: &ItemKind) -> Option<SceneRequest> {
        let ItemKind::Scene3dDoc { scene, first_line, written, bpm, modules, signals, edit_offset, .. } = kind else { return None };
        Some(SceneRequest { scene: scene.clone(), first_line: *first_line, written: *written, bpm: *bpm, modules: modules.clone(), signals: signals.0.clone(), edit_offset: *edit_offset })
    }

    /// Load the scene. `name` is the item's path, for messages; `resolver`
    /// gives the scene's files. Errors and warnings are at the document's
    /// lines.
    pub fn load(&self, name: &str, resolver: &dyn MediaResolver) -> Result<Scene3dDoc, Vec<Diagnostic>> {
        let origin_file = self.written.then(|| crate::DOC_CODE_FILE.to_string());
        match &self.scene {
            SceneSource::Object(object) => Scene3dDoc::from_object(object, ObjectOptions { name: name.to_string(), resolver, bpm: self.bpm, origin_file, host_fields: crate::value::SCENE_KEEP }),
            SceneSource::Template { source, inputs } => {
                let song = crate::value::song_timing(&*self.signals, self.edit_offset);
                match crate::modules::resolve(source, &*self.modules.0) {
                    Ok(loaded) => {
                        let inputs = scene_inputs(&template_inputs(inputs, self.bpm));
                        Scene3dDoc::load_with_modules(source, LoadOptions { name: name.to_string(), inputs, resolver, first_line: self.first_line, origin_file, song }, &loaded)
                    }
                    Err(errors) => Err(errors.into_iter().map(|e| Diagnostic::error(Loc { line: self.first_line, col: 1 }, "", e)).collect()),
                }
            }
        }
    }
}

/// Load the scene a frame's `ItemKind::Scene3dDoc` shows (`None` for any
/// other item): [`SceneRequest::load`].
pub fn load_scene(kind: &ItemKind, name: &str, resolver: &dyn MediaResolver) -> Option<Result<Scene3dDoc, Vec<Diagnostic>>> {
    SceneRequest::of(kind).map(|request| request.load(name, resolver))
}

/// What motion3d says about each 3D scene of `doc` (every Scene3D, Text3D
/// and Seq3D layer), loaded as the renderer loads it but without drawing:
/// its errors and warnings (a field a scene type does not read, a wrong
/// value, a file that does not load) at the document's lines, each path
/// under its layer's. A layer is loaded once, at the first of `times` it
/// shows at; `resolver` gives the scenes' files.
pub fn scene_diagnostics(doc: &crate::MotionDoc, times: &[f64], resolver: &dyn MediaResolver) -> Vec<crate::Diagnostic> {
    let mut loaded = std::collections::HashSet::new();
    let mut out = Vec::new();
    for &t in times {
        let frame = doc.eval(t);
        for (_, item) in frame.walk() {
            if !matches!(item.kind, ItemKind::Scene3dDoc { .. }) || !loaded.insert(item.path.clone()) {
                continue;
            }
            let Some(result) = load_scene(&item.kind, &item.path, resolver) else { continue };
            let diagnostics = match result {
                Ok(mut scene) => std::mem::take(&mut scene.warnings),
                Err(diagnostics) => diagnostics,
            };
            out.extend(diagnostics.into_iter().map(|d| crate::Diagnostic {
                severity: if d.is_error() { crate::model::Severity::Error } else { crate::model::Severity::Warning },
                file: None,
                loc: crate::model::Loc { line: d.loc.line, col: d.loc.col },
                path: if d.path.is_empty() { item.path.clone() } else { format!("{}.{}", item.path, d.path) },
                message: d.message,
                hint: d.hint,
            }));
        }
    }
    out
}

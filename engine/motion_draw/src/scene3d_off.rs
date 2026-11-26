//! Full Scene3D layers without the `scene3d` feature: nothing is drawn and
//! the layer says why, so a host that did not link motion3d shows the rest
//! of the document and one clear problem instead of failing to build.

use super::MotionDraw;
use makepad_motion::frame::{Affine, Item};
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct Scenes3d {
    /// Library files by `id@rev` (and `id`): kept as the full side keeps
    /// them, and never read.
    pub files: HashMap<String, Arc<Vec<u8>>>,
    errors: Vec<String>,
}

impl Scenes3d {
    pub fn begin_frame(&mut self, _cx: &mut Cx) {}
    pub(crate) fn release_all(&mut self, _cx: &mut Cx) {}

    pub fn errors(&self) -> Vec<String> {
        self.errors.clone()
    }

    pub fn debug_state(&self, _cx: &Cx) -> String {
        String::new()
    }

    pub fn painted(&self, _cx: &Cx) -> bool {
        true
    }

    pub fn waiting_for_models(&self) -> bool {
        false
    }

    pub fn models_changed(&self) -> bool {
        false
    }

    pub fn environment_pending(&self) -> bool {
        false
    }

    pub fn handle_event(&mut self, _cx: &mut Cx, _event: &Event) {}

    pub fn reattach(&self, _cx: &mut Cx2d) {}

    pub fn set_coverage(&mut self, _on: bool) {}

    /// No scenes, so no coverage maps.
    pub fn coverage_maps(&self) -> Vec<crate::coverage::SceneCoverage> {
        Vec::new()
    }

    pub(super) fn anchor(&self, _path: &str, _node: &str) -> Option<([f64; 2], bool)> {
        None
    }

    pub(super) fn place(&mut self, _path: &str, _m: Affine, _size: [f64; 2]) {}
}

/// No kept scenes without motion3d.
pub(crate) fn keep_geometry(_keep: &dyn Fn(f64) -> bool) {}

/// No kept scenes without motion3d.
pub(crate) fn keep_scenes(_keep: &dyn Fn(f64) -> bool) -> Vec<SceneDoc> {
    Vec::new()
}

/// A scene's load (none without motion3d).
pub enum SceneLoad {}

/// A loaded scene (none without motion3d).
pub enum SceneLoaded {}

/// A scene document that left (none without motion3d).
pub enum SceneDoc {}

impl SceneLoad {
    pub fn run(self) -> SceneLoaded {
        match self {}
    }
}

/// Nothing to load ahead without motion3d.
pub(crate) fn scene_loads(_md: &MotionDraw, _frame: &makepad_motion::MotionFrame) -> Vec<SceneLoad> {
    Vec::new()
}

pub(crate) fn adopt_loaded(loaded: SceneLoaded) -> Option<Box<dyn FnOnce() + Send>> {
    match loaded {}
}

pub(super) fn render(md: &mut MotionDraw, _cx: &mut Cx2d, item: &Item, _scale: f64) -> Option<Texture> {
    let error = format!("{}: a Scene3D layer needs makepad-motion-draw's `scene3d` feature", item.path);
    if !md.scenes3d.errors.contains(&error) {
        md.scenes3d.errors.push(error);
    }
    None
}

/// Where the host puts a Scene3D video's frame (`MotionDraw::set_video_frame`),
/// as `Need::VideoFrame` names it: the layer's path and the texture's name.
pub(crate) fn video_key(item_path: &str, name: &str) -> String {
    format!("{item_path}/videos/{name}")
}

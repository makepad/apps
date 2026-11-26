//! A slide change (DECK): two finished pictures mixed by a transition
//! preset of the installed effect pack. The compositor does the mixing
//! ([`Compositor::transition`], the same mix as an edit's `Transition`
//! node); this keeps the preset's effect unit, built on the pool the first
//! time a change names it. Until it is built, or if the preset is refused,
//! the change is a dissolve.

use crate::compose::{Compositor, EffectDrive, Painted};
use crate::engine::{effect_view, Install};
use makepad_content_packs::effects::{self as packs, BuiltEffect, EffectView};
use makepad_widgets::*;
use std::collections::HashMap;

/// The tempo effect units run at (a transition preset reads the beat only
/// through the clock the change passes).
const BPM: f64 = 120.0;

/// What a transition name resolves to right now.
#[derive(Clone, Debug, PartialEq)]
pub enum PresetState {
    /// No preset (an empty name): the dissolve.
    Dissolve,
    /// Its geometry is building; the change dissolves meanwhile.
    Building,
    Ready,
    /// Not a bundled transition preset, or it cannot be drawn.
    Refused(String),
}

struct Building {
    name: String,
    install: Option<Install>,
    task: FrameJob<BuiltEffect>,
}

pub struct SlideMixer {
    compositor: Compositor,
    views: HashMap<String, Box<dyn EffectView>>,
    building: Vec<Building>,
    refused: HashMap<String, String>,
}

impl SlideMixer {
    pub fn new(cx: &mut Cx) -> Self {
        Self { compositor: Compositor::new(cx), views: HashMap::new(), building: Vec::new(), refused: HashMap::new() }
    }

    /// Start building transition preset `name` (`vjfx/` optional), once.
    pub fn prepare(&mut self, cx: &mut Cx, name: &str) {
        if name.is_empty() || self.views.contains_key(name) || self.refused.contains_key(name) || self.building.iter().any(|unit| unit.name == name) {
            return;
        }
        let bare = name.strip_prefix("vjfx/").unwrap_or(name);
        if makepad_content_packs::effect_packs().is_empty() {
            self.refused.insert(name.to_string(), packs::missing(&format!("`{name}`")));
            return;
        }
        if !packs::is_transition(bare) {
            self.refused.insert(name.to_string(), format!("`{name}` is not a bundled transition preset"));
            return;
        }
        let Some((pack, _, source)) = packs::resolve(bare) else {
            self.refused.insert(name.to_string(), format!("`{name}` is not a bundled effect preset"));
            return;
        };
        let prepared = match pack.prepare(cx, &format!("present/{name}"), source) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.refused.insert(name.to_string(), format!("`{name}`: {error}"));
                return;
            }
        };
        match cx.task_pool().submit_frame_job(Class::Background, prepared.build) {
            Ok(task) => self.building.push(Building { name: name.to_string(), install: Some(prepared.install), task }),
            Err(error) => { self.refused.insert(name.to_string(), format!("`{name}`: {error}")); }
        }
    }

    /// Finish the presets whose geometry has been built. Call on every event.
    pub fn handle_event(&mut self, cx: &mut Cx, _event: &Event) {
        let (views, refused) = (&mut self.views, &mut self.refused);
        self.building.retain_mut(|unit| match unit.task.try_take() {
            None => true,
            Some(Err(error)) => {
                refused.insert(unit.name.clone(), format!("`{}`: geometry build: {error:?}", unit.name));
                false
            }
            Some(Ok(built)) => {
                if let Some(install) = unit.install.take() {
                    views.insert(unit.name.clone(), effect_view(cx, install, built, BPM));
                }
                false
            }
        });
    }

    pub fn state(&self, name: &str) -> PresetState {
        if name.is_empty() {
            PresetState::Dissolve
        } else if self.views.contains_key(name) {
            PresetState::Ready
        } else if let Some(reason) = self.refused.get(name) {
            PresetState::Refused(reason.clone())
        } else {
            PresetState::Building
        }
    }

    /// `a` mixed into `b` by preset `name` at `mix` (0..1), at `size`.
    /// `time` is seconds since the change began (the preset's clock).
    /// `dials` are the preset's p1..p3 (below 0: its own value). Records
    /// under the pass being drawn; returns the mixed picture.
    #[allow(clippy::too_many_arguments)]
    pub fn mix(&mut self, cx: &mut Cx2d, a: &Texture, b: &Texture, name: &str, mix: f64, dials: [f64; 3], size: (u32, u32), time: f64, dt: f64) -> Texture {
        let drive = EffectDrive { local_time: time, beat: time * BPM / 60.0, dt };
        let params = [dials[0], dials[1], dials[2], -1.0];
        self.compositor.transition(cx, a, b, self.views.get_mut(name).map(|view| view.as_mut()), name, mix, &params, &drive, size)
    }

    /// The target the last mix lands in.
    pub fn output(&self) -> Option<&Texture> {
        self.compositor.output()
    }

    /// The picture the host shows: no mix is recorded into it.
    pub fn hold_output(&mut self, texture: Option<Texture>) {
        self.compositor.hold = texture;
    }

    pub fn painted(&self, cx: &Cx) -> Painted {
        self.compositor.painted(cx, &self.views)
    }

    pub fn stale(&self, cx: &Cx) -> bool {
        self.compositor.stale(cx, &self.views)
    }

    /// Keep the recorded mix attached while it paints.
    pub fn reattach(&self, cx: &mut Cx2d) {
        self.compositor.reattach(cx);
    }
}

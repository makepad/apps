//! Layers and rendering context of a fully loaded Motion document.

use crate::model::*;
use crate::{LiveId, MotionFrame};
use std::sync::Arc;

/// Layers and timing lowered by the full Motion loader, including scripted layer kinds.
#[derive(Clone, Debug)]
pub struct LoadedLayers {
    /// Width and height in document units; a renderer draws it at any
    /// scale (the output's size over this).
    pub size: [f64; 2],
    pub duration: f64,
    /// The frame rate tools sample it at when nothing else says (60 by
    /// default; within an edit it follows the edit's rate).
    pub fps: f64,
    /// Tempo of musical references when the context has no beat grid.
    pub bpm: f64,
    pub background: Option<Color>,
    /// `stops: [1.2 3.0]` gives the seconds where a presentation holds,
    /// in strictly increasing order within the duration.
    pub stops: Vec<f64>,
    pub inputs: Vec<InputDecl>,
    /// The inputs' current values, in `inputs` order.
    pub input_values: Vec<InputValue>,
    pub layers: Vec<Layer>,
    pub context: Arc<MotionContext>,
}

/// A frame produced by [`LoadedLayers::eval`] without calling a VM.
pub struct LoadedPlainFrame(MotionFrame);

impl LoadedPlainFrame {
    pub fn frame(&self) -> &MotionFrame {
        &self.0
    }

    pub fn into_frame(self) -> MotionFrame {
        self.0
    }
}

impl LoadedLayers {
    /// Sample the document at `t` seconds on its own clock.
    pub fn eval(&self, t: f64) -> LoadedPlainFrame {
        LoadedPlainFrame(crate::eval::eval_plain(self, t))
    }

    /// The index of the declared input `id`: resolved once by a host, which
    /// then writes by index.
    pub fn input_index(&self, id: LiveId) -> Option<usize> {
        self.inputs.iter().position(|input| input.id == id)
    }

    /// Whether the document declares `pointer_x` and/or `pointer_y`: a host
    /// with a presenter's pointer ([`MotionDoc::eval_pointer`]) feeds only
    /// these documents.
    pub fn declares_pointer(&self) -> bool {
        self.input_index(input_id("pointer_x")).is_some() || self.input_index(input_id("pointer_y")).is_some()
    }

    /// The inputs' current values as the document's VM reads them (declaration order).
    pub(crate) fn slots(&self) -> Vec<(LiveId, InputValue)> {
        self.inputs.iter().map(|decl| decl.id).zip(self.input_values.iter().cloned()).collect()
    }

    pub fn input(&self, id: LiveId) -> Option<&InputValue> {
        self.input_values.get(self.input_index(id)?)
    }

    /// Every layer, depth first (groups before their children, masks after
    /// their group's children).
    pub fn all_layers(&self) -> Vec<&Layer> {
        fn walk<'a>(layers: &'a [Layer], out: &mut Vec<&'a Layer>) {
            for layer in layers {
                out.push(layer);
                if let LayerKind::Bones(rig) = &layer.kind {
                    walk(&rig.layers, out);
                }
                if let LayerKind::Group { layers, mask, .. } = &layer.kind {
                    walk(layers, out);
                    if let Some(mask) = mask {
                        walk(std::slice::from_ref(mask), out);
                    }
                }
                if let LayerKind::Repeat { layer, .. } = &layer.kind {
                    walk(std::slice::from_ref(layer), out);
                }
                if let LayerKind::FlyThrough { layers, .. } = &layer.kind {
                    walk(layers, out);
                }
                if let LayerKind::Anchored { layer, .. } = &layer.kind {
                    walk(std::slice::from_ref(layer.as_ref()), out);
                }
                if let LayerKind::Scene3dDoc { layers, .. } = &layer.kind {
                    for (_, layer) in layers {
                        walk(std::slice::from_ref(layer), out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.layers, &mut out);
        out
    }

    /// Every library revision the document uses (its manifest's
    /// dependencies), each once, in source order.
    pub fn dependencies(&self) -> Vec<AssetRef> {
        let mut out: Vec<AssetRef> = Vec::new();
        let mut push = |media: &Media| {
            if let Media::Asset(asset) = media {
                if !out.contains(asset) {
                    out.push(asset.clone());
                }
            }
        };
        for layer in self.all_layers() {
            match &layer.kind {
                LayerKind::Svg { src, .. } | LayerKind::Image { src, .. } | LayerKind::Video { src, .. } | LayerKind::Scene3D { model: src, .. } | LayerKind::Vector { src, .. } => push(src),
                LayerKind::Shader { program: ShaderProgram::Effect(asset), .. } => push(&Media::Asset(asset.clone())),
                // A character drawn from a library SVG (lane CH1).
                LayerKind::Character(ch) => {
                    if let crate::character::Model::Asset(asset) = &ch.model {
                        push(&Media::Asset(asset.clone()));
                    }
                }
                LayerKind::Scene3dDoc { assets, textures, videos, .. } => {
                    for asset in assets {
                        push(&Media::Asset(asset.clone()));
                    }
                    for (_, media) in textures.iter().chain(videos) {
                        push(media);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Frame times of the document at its own frame rate: `n / fps` for
    /// every frame that starts before `duration`.
    pub fn frame_times(&self) -> Vec<f64> {
        let n = (self.duration * self.fps - 1e-9).ceil().max(1.0) as usize;
        (0..n).map(|k| k as f64 / self.fps).collect()
    }
}

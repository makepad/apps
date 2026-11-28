//! A Motion presentation read and sampled as data.
pub mod subset;
mod nodes;
pub use nodes::*;
mod picture;
pub use picture::{Frame, PlainFrame, Item, ItemKind, PrimitiveFrame};
mod eval;
mod read;
pub use read::{read, Source};

use crate::model::*;
use crate::LiveId;

#[derive(Clone, Debug)]
pub struct PlainDoc {
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
}

impl PlainDoc {
    /// The index of the declared input `id`: resolved once by a host, which
    /// then writes by index.
    pub fn input_index(&self, id: LiveId) -> Option<usize> {
        self.inputs.iter().position(|input| input.id == id)
    }

    /// Whether the document declares `pointer_x` and/or `pointer_y`: a host
    /// with a presenter's pointer feeds only
    /// these documents.
    pub fn declares_pointer(&self) -> bool {
        self.input_index(input_id("pointer_x")).is_some() || self.input_index(input_id("pointer_y")).is_some()
    }

    pub fn input(&self, id: LiveId) -> Option<&InputValue> {
        self.input_values.get(self.input_index(id)?)
    }

    /// Every layer, depth first, including each repeat's template and each group's mask.
    pub fn all_layers(&self) -> Vec<&Layer> {
        fn visit<'a>(layer: &'a Layer, out: &mut Vec<&'a Layer>) {
            out.push(layer);
            match &layer.kind {
                LayerKind::Group { layers, mask, .. } => {
                    for child in layers { visit(child, out); }
                    if let Some(mask) = mask { visit(mask, out); }
                }
                LayerKind::Repeat { layer, .. } => visit(layer, out),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for layer in &self.layers { visit(layer, &mut out); }
        out
    }

    /// Layers admitted by the data reader carry no library media revisions.
    pub fn dependencies(&self) -> Vec<AssetRef> {
        Vec::new()
    }

    /// Frame times of the document at its own frame rate: `n / fps` for
    /// every frame that starts before `duration`.
    pub fn frame_times(&self) -> Vec<f64> {
        let n = (self.duration * self.fps - 1e-9).ceil().max(1.0) as usize;
        (0..n).map(|k| k as f64 / self.fps).collect()
    }
}

#[cfg(test)]
mod tests;

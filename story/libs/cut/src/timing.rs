//! Musical and spoken time in cuts: a node's `span` may
//! start at `beat(n)`, `bar(n, beat)`, `downbeat(n)`, `onset(n)` /
//! `onset(@name)`, `section(@name)` or `word(@voice, i)` (each with an
//! offset in seconds) and last `beats(x)`. The same references and
//! resolver as Motion documents (`makepad_motion::timing`).
//! (`line(@voice, i)` is a Motion helper only: in a cut `line(…)` is a
//! voiceover line; use `word(@voice, i)` of the line's first word.)
//!
//! Loading places them on a steady grid at the cut's `bpm`;
//! [`crate::CutDoc::with_timing`] re-places every one against the host's
//! timing (the music's baked grid, onsets and sections, the voiceover's
//! word timings), so the cut re-syncs when the music or a take changes.

use crate::model::{CutDoc, Span};
pub use makepad_motion::timing::{BeatGrid, Length, TimeExpr, Timing};

/// Where a musical time was written.
#[derive(Clone, Debug, PartialEq)]
pub enum TimeSlot {
    Span { start: TimeExpr, length: Length },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimeBinding {
    /// The node (or Graphic / Generate) id.
    pub node: String,
    pub slot: TimeSlot,
}

impl CutDoc {
    /// The cut with every musical and spoken span placed against
    /// `timing`, and the ones `timing` cannot place (they keep their
    /// place), as messages.
    pub fn with_timing(&self, timing: &Timing) -> (CutDoc, Vec<String>) {
        let mut doc = self.clone();
        let mut problems = Vec::new();
        for binding in &self.time_bindings {
            let TimeSlot::Span { start, length } = &binding.slot;
            let start_s = match timing.resolve(start, self.bpm) {
                Ok(s) => s,
                Err(message) => {
                    problems.push(format!("`{}.span`: {message}", binding.node));
                    continue;
                }
            };
            let span = Span { start: start_s.max(0.0), length: timing.length(*length, start_s, self.bpm) };
            if let Some(node) = doc.nodes.iter_mut().find(|n| n.id == binding.node) {
                node.span = Some(span);
            } else if let Some(graphic) = doc.graphics.iter_mut().find(|g| g.id == binding.node) {
                graphic.span = Some(span);
            } else if let Some(generate) = doc.generates.iter_mut().find(|g| g.id == binding.node) {
                generate.span = span;
            }
        }
        (doc, problems)
    }
}

//! Whether a cut's picture can change with time at all.
//!
//! A deck plays a slide's cut on a loop; a slide whose picture is the same
//! at every time of the cut (a title and a live app over a six second cut)
//! must cost nothing while nothing else changes. The answer comes from the
//! cut, not from whether its timeline holds: it is true only when no node
//! can change the picture between 0 and the cut's end.

use crate::model::*;

impl CutDoc {
    /// Whether every frame of the cut from 0 to its end is the same picture,
    /// leaving out the Motion documents of its [`Self::motion_nodes`] (the
    /// renderer asks each loaded document, `MotionDoc::is_time_independent`).
    /// Conservative: video, effects, transitions, captions, a camera, an
    /// animated or timed node, generated clips and musical or spoken spans
    /// all count as moving. A live app or a code layer is not time: their
    /// frames arrive by event.
    pub fn picture_is_still(&self) -> bool {
        if !self.generates.is_empty() || !self.time_bindings.is_empty() {
            return false;
        }
        let whole = |span: &Option<Span>| span.is_none_or(|span| span.start <= 0.0 && span.end() >= self.duration);
        let nodes = self.nodes.iter().all(|node| whole(&node.span) && match &node.kind {
            CutNodeKind::Still { opacity, .. } => !opacity.is_keyed(),
            CutNodeKind::Title { font_size, .. } => !font_size.is_keyed(),
            CutNodeKind::Reframe { crop, focus, .. } => focus.is_none() && crop.as_ref().is_none_or(|crop| !crop.is_keyed()),
            CutNodeKind::Stack { .. } | CutNodeKind::AppView { .. } | CutNodeKind::Code(_) | CutNodeKind::Output { .. }
            | CutNodeKind::AudioClip { .. } | CutNodeKind::VoiceOver { .. } => true,
            CutNodeKind::Clip { .. } | CutNodeKind::Fx { .. } | CutNodeKind::Transition { .. } | CutNodeKind::Subtitles { .. } | CutNodeKind::Camera(_) => false,
        });
        nodes && self.graphics.iter().all(|graphic| whole(&graphic.span) && !graphic.opacity.is_keyed() && graphic.bind.is_none())
    }

    /// The nodes the renderer draws as Motion documents: the `Graphic`
    /// nodes and the titles (the built-in "title" document).
    pub fn motion_nodes(&self) -> impl Iterator<Item = &str> {
        let titles = self.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Title { .. })).map(|node| node.id.as_str());
        titles.chain(self.graphics.iter().map(|graphic| graphic.id.as_str()))
    }
}

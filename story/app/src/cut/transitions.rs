//! Transitions the Final Cut way: a transition dropped (or
//! armed and clicked, or added with cmd-T) on the cut point between two
//! adjacent clips on one track is centred on the cut. Each clip is extended
//! into its media handles (the source beyond its out point, before its in
//! point) to make the overlap the transition plays over. When a side has
//! fewer handles the transition is shortened to what there is; when there
//! are none the later clips are rippled left to overlap by trimming, and the
//! status says so. Two clips on different tracks that already overlap keep
//! the older behaviour: the transition covers their overlap.
//!
//! Dragging a transition's edges (or its Duration in the inspector) makes it
//! longer or shorter about its centre, within the same handles.

use super::ops::{self, overlap, CutOp, Lane, Respan};
use super::timeline::DropHint;
use super::{split_selection, VideoCut};
use makepad_story_cut::{CutDoc, CutNode, CutNodeKind, MediaRef, SourceTime, Span};

/// A new transition's length, seconds.
pub const DEFAULT_TRANSITION: f64 = 1.0;
/// cmd-T's transition: the dissolve (a transition without a preset).
pub const DEFAULT_PRESET: &str = "";

/// A transition planned on a cut: the cut, where it lands, and what the
/// status line says about it.
pub struct CutPlan {
    pub op: CutOp,
    pub start: f64,
    pub length: f64,
    pub track: u32,
    pub targets: Vec<String>,
    pub note: Option<String>,
}

/// How far the clips can be extended at a cut, edit seconds: `a` past its
/// out point, `b` before its in point.
fn plan_overlap(want: f64, handle_a: f64, handle_b: f64) -> (f64, f64) {
    let half = want * 0.5;
    let mut b = half.min(handle_b.max(0.0));
    let a = (want - b).min(handle_a.max(0.0));
    b = (want - a).min(handle_b.max(0.0));
    (a, b)
}

impl VideoCut {
    /// A source's length in seconds, when the page knows it.
    fn source_seconds(&self, src: &MediaRef) -> Option<f64> {
        let id = match src {
            MediaRef::Asset(asset) => asset.id.clone(),
            MediaRef::Run(run) => run.render_id.clone(),
            MediaRef::MissingOptional(_) => return None,
        };
        self.film_info.get(&id).map(|info| info.seconds).filter(|seconds| *seconds > 0.0)
            .or_else(|| self.lib.rows.iter().find(|row| row.reference.asset_id.to_string() == id).and_then(|row| row.seconds))
            .or_else(|| self.details.iter().find(|(reference, _)| reference.asset_id.to_string() == id).and_then(|(_, details)| details.edit.as_ref().map(|edit| edit.1)))
    }

    /// A block's handles in cut seconds: before its in point, past its out
    /// point. A still has as much as it needs.
    fn handles(&self, node: &CutNode) -> (f64, f64) {
        let Some(span) = node.span else { return (0.0, 0.0) };
        match &node.kind {
            CutNodeKind::Still { .. } | CutNodeKind::Title { .. } => (f64::INFINITY, f64::INFINITY),
            CutNodeKind::Clip { src, source_in, speed, .. } => {
                let speed = speed.abs().max(1e-6);
                let Some(total) = self.source_seconds(src) else { return (0.0, 0.0) };
                let source_in = match source_in {
                    SourceTime::Seconds(seconds) => *seconds,
                    SourceTime::Marker { .. } => return (0.0, 0.0),
                };
                (source_in / speed, ((total - source_in) / speed - span.length).max(0.0))
            }
            _ => (0.0, 0.0),
        }
    }

    fn cut_clip(doc: &CutDoc, id: &str) -> Option<CutNode> {
        ops::timed_block(doc, id).filter(|node| ops::lane_of(node) == Some(Lane::Video))
    }

    /// The cut point on `track` within `tolerance` of `t`: the clip that
    /// ends there, the one that starts there, and the cut's time.
    pub(super) fn find_cut(&self, track: u32, t: f64, tolerance: f64) -> Option<(String, String, f64)> {
        find_cut_in(&self.session.as_ref()?.doc, track, t, tolerance)
    }

    /// A transition on the cut between `a` (out) and `b` (in).
    pub(super) fn plan_cut(&self, preset: &str, a: &str, b: &str, want: f64) -> Result<CutPlan, String> {
        let session = self.session.as_ref().ok_or("Start or open a cut first")?;
        cut_plan(&session.doc, preset, a, b, want, &|node| self.handles(node), &|id| self.projection.block(id).map_or(id.to_string(), |block| block.label.clone()))
    }
}

/// The cut point on `track` within `tolerance` of `t`: the clip that ends
/// there, the one that starts there, and the cut's time.
pub fn find_cut_in(doc: &CutDoc, track: u32, t: f64, tolerance: f64) -> Option<(String, String, f64)> {
    {
        let frame = 1.0 / doc.output.fps.max(1.0);
        let clips: Vec<CutNode> = ops::video_blocks(doc).into_iter().filter(|node| node.track == track).collect();
        let mut best: Option<(String, String, f64, f64)> = None;
        for a in &clips {
            let end = a.span?.end();
            if (end - t).abs() > tolerance {
                continue;
            }
            for b in &clips {
                let start = b.span?.start;
                if a.id != b.id && (start - end).abs() <= frame && best.as_ref().is_none_or(|best| (end - t).abs() < best.3) {
                    best = Some((a.id.clone(), b.id.clone(), end, (end - t).abs()));
                }
            }
        }
        best.map(|(a, b, cut, _)| (a, b, cut))
    }
}

/// A transition on the cut between `a` (out) and `b` (in): each clip
/// extended into its `handles` (before its in, past its out, edit seconds);
/// `label` names a clip in the status.
pub fn cut_plan(doc: &CutDoc, preset: &str, a: &str, b: &str, want: f64, handles: &dyn Fn(&CutNode) -> (f64, f64), label: &dyn Fn(&str) -> String) -> Result<CutPlan, String> {
    {
        let (node_a, node_b) = (VideoCut::cut_clip(doc, a).ok_or("not a video clip")?, VideoCut::cut_clip(doc, b).ok_or("not a video clip")?);
        if !ops::joined(doc, a).is_empty() && ops::joined(doc, a).iter().any(|(_, other)| other == b) {
            return Err("These clips already have a transition; drag its edges to change it".into());
        }
        let (span_a, span_b) = (node_a.span.unwrap(), node_b.span.unwrap());
        let frame = 1.0 / doc.output.fps.max(1.0);
        let want = want.min(span_a.length).min(span_b.length).max(frame);
        let ((_, out_a), (in_b, _)) = (handles(&node_a), handles(&node_b));
        let (ext_a, ext_b) = plan_overlap(want, out_a, in_b);
        let overlap_len = ext_a + ext_b;
        let mut spans = vec![Respan { node: a.into(), start: span_a.start, length: span_a.length + ext_a, source_in: None }];
        let speed_b = match &node_b.kind { CutNodeKind::Clip { speed, .. } => speed.abs(), _ => 1.0 };
        let in_b_now = match &node_b.kind { CutNodeKind::Clip { source_in: SourceTime::Seconds(seconds), .. } => Some(*seconds), _ => None };
        let mut note = None;
        let (start, length) = if overlap_len + 1e-6 >= frame {
            if overlap_len + 1e-6 < want {
                note = Some(format!("Transition shortened to {:.2} s: the clips have no more media beyond the cut", overlap_len));
            }
            spans.push(Respan { node: b.into(), start: span_b.start - ext_b, length: span_b.length + ext_b, source_in: in_b_now.map(|source_in| source_in - ext_b * speed_b) });
            (span_a.end() - ext_b, overlap_len)
        } else {
            // No handles at all: overlap by trimming. The later clip and
            // everything after it on the track move left by the length.
            let shift = want;
            for node in ops::video_blocks(doc).iter().filter(|node| node.track == node_b.track && node.span.is_some_and(|span| span.start >= span_b.start - 1e-6)) {
                let span = node.span.unwrap();
                spans.push(Respan { node: node.id.clone(), start: (span.start - shift).max(0.0), length: span.length, source_in: None });
            }
            note = Some(format!("Overlap by trimming: no media beyond the cut, so “{}” and the clips after it move {:.2} s earlier", label(b), shift));
            (span_b.start - shift, shift)
        };
        Ok(CutPlan {
            op: CutOp::CutTransition { a: a.into(), b: b.into(), effect: preset.into(), spans },
            start,
            length,
            track: node_a.track,
            targets: vec![a.to_string(), b.to_string()],
            note,
        })
    }
}

impl VideoCut {

    /// A transition at `t` on `track`: on the cut point there, else over
    /// the overlap of the clip under the pointer with another (two tracks).
    pub(super) fn plan_transition_at(&self, preset: &str, t: f64, track: u32, block: Option<&str>, tolerance: f64) -> Result<CutPlan, String> {
        let track = block.and_then(|block| ops::timed_block(&self.session.as_ref()?.doc, split_selection(block).0).map(|node| node.track)).unwrap_or(track);
        if let Some((a, b, _)) = self.find_cut(track, t, tolerance) {
            return self.plan_cut(preset, &a, &b, DEFAULT_TRANSITION);
        }
        let op = self.plan_preset(preset, true, t, block.map(|block| split_selection(block).0)).map_err(|error| {
            if error.contains("already in a transition") { error } else { "Transitions go on a cut between two clips, or where two clips overlap".to_string() }
        })?;
        let CutOp::Transition { a, b, .. } = &op else { unreachable!() };
        let doc = &self.session.as_ref().unwrap().doc;
        let (block_a, block_b) = (ops::timed_block(doc, a), ops::timed_block(doc, b));
        let over = block_a.as_ref().and_then(|a| a.span).zip(block_b.as_ref().and_then(|b| b.span)).and_then(|(sa, sb)| overlap(sa, sb)).unwrap_or(Span { start: t, length: 1.0 });
        let track = block_a.map_or(0, |a| a.track);
        Ok(CutPlan { targets: vec![a.clone(), b.clone()], op, start: over.start, length: over.length, track, note: None })
    }

    /// A transition or effect dragged over the timeline: where it would
    /// land, or why not.
    pub(super) fn preset_hint(&self, preset: &str, transition: bool, title: &str, hit: &super::timeline::TimelineHit, tolerance: f64) -> DropHint {
        if transition {
            return match self.plan_transition_at(preset, hit.t, hit.track, hit.block.as_deref(), tolerance) {
                Ok(plan) => DropHint { lane: Lane::Video, track: plan.track, start: plan.start, length: plan.length, targets: plan.targets, label: title.into(), invalid: None, tab: true },
                Err(reason) => DropHint { lane: hit.lane, track: hit.track, start: hit.t, length: 0.5, targets: Vec::new(), label: String::new(), invalid: Some(reason), tab: true },
            };
        }
        match self.plan_preset(preset, false, hit.t, hit.block.as_deref().map(|block| split_selection(block).0)) {
            Ok(CutOp::Effect { node, .. }) => {
                let span = self.session.as_ref().and_then(|session| session.doc.node(&node)).and_then(|node| node.span).unwrap_or(Span { start: hit.t, length: 1.0 });
                DropHint { lane: Lane::Video, track: hit.track, start: span.start, length: span.length, targets: vec![node], label: title.into(), invalid: None, tab: false }
            }
            _ => DropHint { lane: hit.lane, track: hit.track, start: hit.t, length: 1.0, targets: Vec::new(), label: String::new(), invalid: Some("Effects go on a video clip".into()), tab: false },
        }
    }

    /// Put a planned transition in, saying what it did.
    pub(super) fn apply_cut_plan(&mut self, plan: CutPlan, preset: &str) {
        self.status = plan.note.unwrap_or_else(|| format!("{} · {:.2} s", super::project::preset_label(preset), plan.length));
        self.status_sticky = true;
        self.op(plan.op);
        self.repaint = true;
    }

    /// cmd-T: the default transition on the cut points of the selected
    /// clips (both ends), else on the cut nearest the playhead.
    pub(super) fn add_default_transitions(&mut self) {
        let Some(session) = &self.session else { return };
        let doc = session.doc.clone();
        let frame = 1.0 / doc.output.fps.max(1.0);
        let selected: Vec<String> = self.selected.iter().chain(self.also.iter()).map(|id| split_selection(id).0.to_string()).collect();
        let mut cuts: Vec<(String, String)> = Vec::new();
        for id in &selected {
            let Some(node) = Self::cut_clip(&doc, id) else { continue };
            let span = node.span.unwrap();
            for (t, _) in [(span.start, ()), (span.end(), ())] {
                if let Some((a, b, _)) = self.find_cut(node.track, t, frame) {
                    if !cuts.contains(&(a.clone(), b.clone())) {
                        cuts.push((a, b));
                    }
                }
            }
        }
        if cuts.is_empty() && selected.is_empty() {
            let playhead = self.playhead;
            if let Some(cut) = (0..4).find_map(|track| self.find_cut(track, playhead, 0.5)) {
                cuts.push((cut.0, cut.1));
            }
        }
        if cuts.is_empty() {
            self.status = "No cut point here: select a clip that meets another on its track".into();
            self.repaint = true;
            return;
        }
        // One cut per edit (each changes the spans the next one reads).
        let (a, b) = cuts.remove(0);
        match self.plan_cut(DEFAULT_PRESET, &a, &b, DEFAULT_TRANSITION) {
            Ok(plan) => {
                self.apply_cut_plan(plan, DEFAULT_PRESET);
                self.pending_cuts = cuts;
            }
            Err(error) => {
                self.status = error;
                self.repaint = true;
            }
        }
    }

    /// A transition chosen with a clip selected: on its out point's cut,
    /// else its in point's. False when neither end meets a clip (the
    /// transition is then armed for a click).
    pub(super) fn transition_on_selected(&mut self, preset: &str) -> bool {
        let Some(doc) = self.session.as_ref().map(|session| session.doc.clone()) else { return false };
        let Some(node) = self.selected.as_deref().and_then(|id| Self::cut_clip(&doc, split_selection(id).0)) else { return false };
        let frame = 1.0 / doc.output.fps.max(1.0);
        let span = node.span.unwrap();
        let Some((a, b, _)) = [span.end(), span.start].into_iter().find_map(|t| self.find_cut(node.track, t, frame)) else { return false };
        match self.plan_cut(preset, &a, &b, DEFAULT_TRANSITION) {
            Ok(plan) => self.apply_cut_plan(plan, preset),
            Err(error) => self.status = error,
        }
        self.armed = None;
        self.repaint = true;
        true
    }

    /// The rest of a cmd-T with several cuts, one per finished edit.
    pub(super) fn next_pending_cut(&mut self) {
        if self.in_flight.is_some() || !self.queue.is_empty() {
            return;
        }
        let Some((a, b)) = (!self.pending_cuts.is_empty()).then(|| self.pending_cuts.remove(0)) else { return };
        if let Ok(plan) = self.plan_cut(DEFAULT_PRESET, &a, &b, DEFAULT_TRANSITION) {
            self.apply_cut_plan(plan, DEFAULT_PRESET);
        }
    }

    /// A transition made longer or shorter (its edges dragged, or its
    /// Duration typed): about its centre, the clips round it following
    /// within their handles. A cross-track transition keeps to the overlap.
    pub(super) fn retime_transition(&mut self, node: &str, start: f64, length: f64) {
        let Some(session) = &self.session else { return };
        let doc = session.doc.clone();
        let Some(tr) = doc.node(node).cloned() else { return };
        let (CutNodeKind::Transition { a: Some(a), b: Some(b), .. }, Some(old)) = (&tr.kind, tr.span) else { return };
        let (a, b) = (ops::side_base(&doc, &a.node, true), ops::side_base(&doc, &b.node, false));
        let (Some(node_a), Some(node_b)) = (ops::timed_block(&doc, &a), ops::timed_block(&doc, &b)) else { return };
        let (Some(span_a), Some(span_b)) = (node_a.span, node_b.span) else { return };
        let frame = 1.0 / doc.output.fps.max(1.0);
        if node_a.track != node_b.track {
            let over = overlap(span_a, span_b).unwrap_or(old);
            let s = start.clamp(over.start, over.end() - frame);
            let l = length.min(over.end() - s).max(frame);
            self.op(CutOp::Respan { spans: vec![Respan { node: node.into(), start: s, length: l, source_in: None }] });
            return;
        }
        // Symmetric about the centre (the edges move together).
        let centre = if (start - old.start).abs() < 1e-9 && (start + length - old.end()).abs() > 1e-9 || (start - old.start).abs() > 1e-9 && (start + length - old.end()).abs() < 1e-9 {
            old.start + old.length * 0.5
        } else {
            start + length * 0.5
        };
        let requested = if (start - old.start).abs() > 1e-9 && (start + length - old.end()).abs() < 1e-9 {
            old.length + 2.0 * (old.start - start)
        } else if (start - old.start).abs() < 1e-9 {
            old.length + 2.0 * (start + length - old.end())
        } else {
            length
        };
        let ((_, out_a), (in_b, _)) = (self.handles(&node_a), self.handles(&node_b));
        // How far each side can reach from the centre.
        let reach_a = (span_a.end() - centre) + out_a;
        let reach_b = (centre - span_b.start) + in_b;
        let half = (requested * 0.5).min(reach_a).min(reach_b).min(span_a.length.min(span_b.length) * 0.5).max(frame * 0.5);
        let (new_end_a, new_start_b) = (centre + half, centre - half);
        let speed_b = match &node_b.kind { CutNodeKind::Clip { speed, .. } => speed.abs(), _ => 1.0 };
        let in_b_now = match &node_b.kind { CutNodeKind::Clip { source_in: SourceTime::Seconds(seconds), .. } => Some(*seconds), _ => None };
        let moved_b = span_b.start - new_start_b;
        let spans = vec![
            Respan { node: a.clone(), start: span_a.start, length: new_end_a - span_a.start, source_in: None },
            Respan { node: b.clone(), start: new_start_b, length: span_b.length + moved_b, source_in: in_b_now.map(|source_in| source_in - moved_b * speed_b) },
            Respan { node: node.into(), start: new_start_b, length: 2.0 * half, source_in: None },
        ];
        if (2.0 * half - requested).abs() > frame {
            self.status = format!("Transition {:.2} s: the clips have no more media for a longer one", 2.0 * half);
            self.status_sticky = true;
        } else {
            self.status = format!("Transition {:.2} s", 2.0 * half);
            self.status_sticky = true;
        }
        self.op(CutOp::Respan { spans });
    }
}

#[cfg(test)]
mod tests {
    use super::plan_overlap;

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn overlaps_come_from_the_handles_there_are() {
        assert!(near(plan_overlap(1.0, 5.0, 5.0), (0.5, 0.5)));
        // One side short: the other side gives more.
        assert!(near(plan_overlap(1.0, 5.0, 0.2), (0.8, 0.2)));
        assert!(near(plan_overlap(1.0, 0.1, 5.0), (0.1, 0.9)));
        // Neither has enough: shortened to what there is.
        assert!(near(plan_overlap(1.0, 0.1, 0.2), (0.1, 0.2)));
        assert!(near(plan_overlap(1.0, 0.0, 0.0), (0.0, 0.0)));
    }
}

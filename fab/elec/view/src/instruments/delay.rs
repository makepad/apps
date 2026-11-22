//! Cross-lane delay geometry and labels. Times are relative to the shared trigger.

use super::{axis::write_si_label, reticle::snap, simfeed::{Feature, FeatureKind}, span::End};
use makepad_widgets_core::{DVec2, Rect};

#[derive(Clone, Debug, PartialEq)]
pub struct DelayBracket {
    pub from: (String, FeatureKind, f64),
    pub to: (String, FeatureKind, f64),
}

/// A captured lane emits one step; only the bench chooses the lane under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DelayGesture {
    Start { abs: DVec2, edge: Feature, edit: Option<(usize, End)> },
    Move(DVec2),
    End(DVec2),
    /// None cancels the active gesture; an index removes one retained bracket.
    Remove(Option<usize>),
}

/// The first containing lane in display order. Rectangles exclude inter-lane gaps.
/// Accept an iterator so the bench can borrow current card rectangles without a temporary Vec.
pub fn lane_at(lanes: impl IntoIterator<Item = (usize, Rect)>, pointer: DVec2) -> Option<usize> {
    lanes.into_iter().find_map(|(lane, rect)| {
        (rect.size.x > 0.0 && rect.size.y > 0.0 && rect.contains(pointer)).then_some(lane)
    })
}

/// Nearest fifty-percent edge of the kept kind, at most one period from the kept time.
/// Ordered capture features need one binary search and only the nearby candidates; no samples.
pub fn follow_edge(features: &[Feature], kind: FeatureKind, t: f64, period: f64) -> Option<f64> {
    edge_near(features, Some(kind), t, period).map(|feature| feature.t)
}

pub(super) fn edge_near(features: &[Feature], kind: Option<FeatureKind>, t: f64, radius: f64) -> Option<&Feature> {
    if !t.is_finite() || !radius.is_finite() || radius < 0.0
        || kind.is_some_and(|kind| !matches!(kind, FeatureKind::Rise | FeatureKind::Fall)) { return None; }
    let at = features.partition_point(|feature| feature.t < t);
    let accepts = |feature: &&Feature| matches!(feature.kind, FeatureKind::Rise | FeatureKind::Fall)
        && kind.is_none_or(|kind| kind == feature.kind);
    let before = features[..at].iter().rev().take_while(|feature| t - feature.t <= radius).find(accepts);
    let after = features[at..].iter().take_while(|feature| feature.t - t <= radius).find(accepts);
    // Reuse the Reticle's radius and tie rules, excluding levels even when one is closer.
    match (before, after) {
        (Some(a), Some(b)) => {
            let candidates = [*a, *b];
            snap(&candidates, t, radius).map(|feature| if feature.t == a.t { a } else { b })
        }
        (Some(edge), None) | (None, Some(edge)) => Some(edge),
        _ => None,
    }
}

/// The answer's SI precision, prefixes and signed delay (to minus from).
pub fn delay_label(from_net: &str, from_kind: FeatureKind, to_net: &str, to_kind: FeatureKind, dt: f64) -> String {
    let mut text = String::with_capacity(from_net.len() + to_net.len() + 64);
    write_label(&mut text, from_net, Some(from_kind), to_net, Some(to_kind), Some(dt));
    text
}

pub(super) fn write_label(text: &mut String, from_net: &str, from_kind: Option<FeatureKind>, to_net: &str, to_kind: Option<FeatureKind>, dt: Option<f64>) {
    let kind = |kind| match kind { Some(FeatureKind::Rise) => "rise", Some(FeatureKind::Fall) => "fall", _ => "no edge" };
    text.clear();
    if !from_net.is_empty() { text.push_str(from_net); text.push(' '); }
    text.push_str(kind(from_kind));
    text.push_str(" -> ");
    if !to_net.is_empty() { text.push_str(to_net); text.push(' '); }
    text.push_str(kind(to_kind));
    if let Some(dt) = dt { text.push(' '); write_si_label(text, dt, "s"); }
}

/// Retained slots are prepared on press, so visiting a new target never allocates a label.
#[derive(Default)]
pub(super) struct DelayMark {
    pub end: Option<End>,
    pub feature: Option<Feature>,
    pub label: String,
}

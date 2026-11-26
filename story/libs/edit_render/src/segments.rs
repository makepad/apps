//! How a segmented export cuts a cut (`makepad-edit-render export`): at every
//! root node's span start and end on the output's frame grid, and into pieces
//! of at most [`MAX_SEGMENT_SECONDS`] (shorter, down to
//! [`MIN_SEGMENT_SECONDS`], so that every part rendering at once has one),
//! each rendered on its own and the parts joined without re-encoding
//! ([`crate::mux`]).

use crate::audio::{self, fps_ratio};
use makepad_story_cut::CutDoc;

/// The longest part: a long plain stretch renders in pieces this long.
pub const MAX_SEGMENT_SECONDS: f64 = 10.0;
/// The shortest piece a stretch is split into for more parts at once (a
/// part pays its process's start: the cut's load, its shaders).
pub const MIN_SEGMENT_SECONDS: f64 = 2.0;

/// Segment boundaries of `doc` on its frame grid: every root node's span
/// start and end, split so no segment is longer than
/// [`MAX_SEGMENT_SECONDS`], nor than the cut's `parts`-th share (`parts`
/// render at once), but no shorter than [`MIN_SEGMENT_SECONDS`].
pub fn segment_frames(doc: &CutDoc, parts: usize) -> Vec<(u64, u64)> {
    let fps = fps_ratio(doc.output.fps);
    let total = audio::frame_count(doc.duration, fps);
    let frame = |t: f64| ((t.max(0.0) * fps.0 as f64 / fps.1 as f64).round() as u64).min(total);
    let mut cuts = vec![0, total];
    for node in &doc.nodes {
        if let Some(span) = node.span {
            cuts.push(frame(span.start));
            cuts.push(frame(span.end()));
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    let share = (doc.duration / parts.max(1) as f64).clamp(MIN_SEGMENT_SECONDS, MAX_SEGMENT_SECONDS);
    let longest = ((share * fps.0 as f64 / fps.1 as f64).round() as u64).max(1);
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let (mut first, end) = (pair[0], pair[1]);
        while end - first > longest {
            out.push((first, first + longest));
            first += longest;
        }
        if end > first {
            out.push((first, end));
        }
    }
    out
}

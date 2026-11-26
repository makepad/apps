//! Teaching diagnostics on resolved frames. These run only when an author
//! requests inspection; evaluation and the drawing loop do no extra work.
//! Geometry is measured in pixels at the document's own resolution. Font
//! ink boxes, shape fills and trimmed strokes are checked before effects;
//! renderer-only geometry is reported as unchecked, never as a clean pass.

mod geometry;
mod pops;
mod sampling;
pub use pops::{pop_checks, POP_MIN_AREA, POP_SETTLE, POP_SHARE};
pub use sampling::sample_times;

use crate::{MotionDoc, MotionFrame, Severity};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Minimum readable em height at the document's output resolution.
pub const DEFAULT_MIN_TEXT_PX: f64 = 16.0;
const VISIBLE_ALPHA: f64 = 0.01;
/// The least gap between two texts' letters, as a share of the frame height.
pub const CROWDED_GAP: f64 = 0.01;
/// Letters closer than this share of the frame height to an edge are reported.
pub const EDGE_MARGIN: f64 = 0.02;

/// Same stable code, layer pair, teaching message and sampled time range
/// as the chart checks. `from` and `to` are inclusive sample times.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutIssue {
    pub code: &'static str,
    pub severity: Severity,
    pub path: String,
    pub other: Option<String>,
    pub message: String,
    pub hint: String,
    pub from: f64,
    pub to: f64,
}

fn issue(t: f64, code: &'static str, path: &str, other: Option<&str>, message: String, hint: &str) -> LayoutIssue {
    LayoutIssue { code, severity: Severity::Warning, path: path.into(), other: other.map(str::to_string), message, hint: hint.into(), from: t, to: t }
}

/// Inspect one resolved frame. Opacity, reveal, per-unit motion and paint
/// order all come from this frame, with no previous-frame state. A size of
/// 12 points is a 16-pixel em before fit and world/unit scaling.
pub fn inspect_frame(doc: &MotionDoc, frame: &MotionFrame, min_text_px: f64) -> Vec<LayoutIssue> {
    inspect_measured(doc, frame, min_text_px).0
}

/// What shows of each layer in one frame: [`geometry::Ink::presence`] by
/// item path (groups included), and the paths whose geometry needs the
/// renderer (masked content), which the continuity check leaves alone.
#[derive(Clone, Debug, Default)]
pub(crate) struct Presence {
    pub weight: HashMap<String, f64>,
    pub unchecked: HashSet<String>,
}

fn measure(doc: &MotionDoc, frame: &MotionFrame) -> Presence {
    let glyphs = glyph_groups(doc);
    let (mut ink, mut unchecked) = (Vec::new(), Vec::new());
    let canvas = [0.0, 0.0, frame.size[0], frame.size[1]];
    geometry::collect(&frame.items, 1.0, true, canvas, &glyphs, &mut ink, &mut unchecked);
    presence_of(&ink, unchecked)
}

fn presence_of(ink: &[geometry::Ink], unchecked: Vec<(String, String)>) -> Presence {
    Presence {
        weight: ink.iter().filter(|i| i.presence > 0.0).map(|i| (i.path.clone(), i.presence)).collect(),
        unchecked: unchecked.into_iter().map(|(path, _)| path).collect(),
    }
}

fn inspect_measured(doc: &MotionDoc, frame: &MotionFrame, min_text_px: f64) -> (Vec<LayoutIssue>, Presence) {
    let glyphs = glyph_groups(doc);
    let min_text_px = if min_text_px.is_finite() && min_text_px >= 0.0 { min_text_px } else { DEFAULT_MIN_TEXT_PX };
    let (mut ink, mut unchecked) = (Vec::new(), Vec::new());
    let canvas = [0.0, 0.0, frame.size[0], frame.size[1]];
    geometry::collect(&frame.items, 1.0, true, canvas, &glyphs, &mut ink, &mut unchecked);
    let presence = presence_of(&ink, unchecked.clone());
    let visible: Vec<_> = ink.iter().map(|i| i.visible(canvas)).collect();
    for item in &mut ink {
        for polygons in [&mut item.glyphs, &mut item.fill, &mut item.stroke] {
            for p in polygons.iter_mut() { *p = geometry::on_canvas(p, canvas); }
            polygons.retain(|p| p.len() >= 3);
        }
    }
    let mut out = Vec::new();
    split_sentences(frame, &ink, &visible, &mut out);
    for (path, why) in unchecked {
        out.push(issue(frame.t, "layout.unchecked", &path, None, why, "inspect a rendered frame for this layer; these checks measure 2D geometry before effects"));
    }
    for (i, a) in ink.iter().enumerate() {
        if !visible[i] { continue }
        if let Some(b) = a.bounds {
            if b[0] < -1e-6 || b[1] < -1e-6 || b[2] > frame.size[0] + 1e-6 || b[3] > frame.size[1] + 1e-6 {
                out.push(issue(frame.t, "layer.clipped", &a.path, None,
                    format!("the layer's bounds ({:.1}, {:.1})–({:.1}, {:.1}) extend outside the {:.0}×{:.0} frame", b[0], b[1], b[2], b[3], frame.size[0], frame.size[1]),
                    "move `at` inwards or reduce `size`/`scale`; for text, use `fit: @safe` or `wrap: @safe` and allow room for animated letters"));
            } else if !a.glyphs.is_empty() {
                // Letters inside the frame but crowding its edge read as cut off.
                let g = a.glyphs.iter().filter_map(|p| geometry::bounds(p)).reduce(geometry::union).unwrap_or(b);
                let m = frame.size[1] * EDGE_MARGIN;
                if g[0] < m || g[1] < m || g[2] > frame.size[0] - m || g[3] > frame.size[1] - m {
                    out.push(issue(frame.t, "text.near_edge", &a.path, None,
                        format!("the text's letters come within {m:.0} px of the frame's edge ({:.0}, {:.0})–({:.0}, {:.0})", g[0], g[1], g[2], g[3]),
                        "move the text inwards (keep about 5% of the frame clear at every edge) or make it smaller"));
                }
            }
        }
        if let Some(px) = a.min_px.filter(|px| *px < min_text_px - 1e-6) {
            out.push(issue(frame.t, "text.too_small", &a.path, None,
                format!("the text's smallest visible em is {px:.1} output pixels, below the {min_text_px:.1} px minimum"),
                "increase `size` or `scale`, or give `fit` more width; text size is in points (4/3 pixels per point), including parent and per-letter scale"));
        }
        if a.glyphs.is_empty() { continue }
        for (j, b) in ink.iter().enumerate() {
            if i == j || !visible[j] || !a.bounds.zip(b.bounds).is_some_and(|(a, b)| geometry::intersects(a, b)) { continue }
            let crosses = |polygons: &[geometry::Polygon]| a.glyphs.iter().any(|a| polygons.iter().any(|b| geometry::overlap(a, b)));
            if j > i && crosses(&b.glyphs) {
                out.push(issue(frame.t, "text.overlap", &a.path, Some(&b.path),
                    format!("the text's glyph boxes intersect text in `{}`", b.path),
                    "separate the text layers with `at`, increase line spacing, or shorten/wrap the text; inspect their animated positions at this time"));
            }
            // Two texts that do not touch but nearly do read as one block:
            // keep a gap of 1% of the frame height between their letters.
            if j > i && !crosses(&b.glyphs) {
                let gap = frame.size[1] * CROWDED_GAP;
                let grown: Vec<geometry::Polygon> = a.glyphs.iter().filter_map(|g| geometry::bounds(g)).map(|r| geometry::rect([r[0] - gap, r[1] - gap, r[2] + gap, r[3] + gap])).collect();
                if grown.iter().any(|g| b.glyphs.iter().any(|q| geometry::overlap(g, q))) {
                    out.push(issue(frame.t, "text.crowded", &a.path, Some(&b.path),
                        format!("the text's letters come within {gap:.0} px of the text in `{}`", b.path),
                        "give the two texts more room (move one with `at`, or make one smaller); a formula's fraction or a label above a plot needs its own space"));
                }
            }
            if crosses(&b.stroke) {
                out.push(issue(frame.t, "text.line_through", &a.path, Some(&b.path),
                    format!("the stroke in `{}` crosses the text's glyph boxes", b.path),
                    "route the path around the label, move the text, or trim the stroke so it stops before the letters"));
            }
            if j > i && b.opaque && crosses(&b.fill) {
                out.push(issue(frame.t, "text.covered", &a.path, Some(&b.path),
                    format!("the opaque fill in `{}` is painted above the text and covers its glyph boxes", b.path),
                    "put the filled shape earlier in `layers` so the text draws above it, or move/shrink the shape away from the letters"));
            }
        }
    }
    (out, presence)
}

/// A layer's ink box in one frame, in document pixels: what a tool draws a
/// layer's name over (`frame --names`).
#[derive(Clone, Debug, PartialEq)]
pub struct LayerBox {
    pub path: String,
    pub name: Option<String>,
    pub kind: &'static str,
    /// `[x0, y0, x1, y1]`, clipped to the frame.
    pub bounds: [f64; 4],
    pub group: bool,
}

/// The visible layers of `frame` with their ink boxes, in paint order
/// (groups after their children). A Repeat's copies keep their `#n` path
/// and their template's name.
pub fn layer_boxes(doc: &MotionDoc, frame: &MotionFrame) -> Vec<LayerBox> {
    let glyphs = glyph_groups(doc);
    let (mut ink, mut unchecked) = (Vec::new(), Vec::new());
    let canvas = [0.0, 0.0, frame.size[0], frame.size[1]];
    geometry::collect(&frame.items, 1.0, true, canvas, &glyphs, &mut ink, &mut unchecked);
    let layers: HashMap<&str, &crate::Layer> = doc.all_layers().into_iter().map(|l| (l.path.as_str(), l)).collect();
    ink.iter().filter(|i| i.visible(canvas)).filter_map(|i| {
        let b = i.bounds?;
        let bounds = [b[0].max(0.0), b[1].max(0.0), b[2].min(canvas[2]), b[3].min(canvas[3])];
        let layer = layers.get(i.path.split('#').next().unwrap_or(&i.path));
        (bounds[2] > bounds[0] && bounds[3] > bounds[1]).then(|| LayerBox {
            path: i.path.clone(),
            name: layer.and_then(|l| l.name.clone()),
            kind: layer.map_or("", |l| l.kind.type_name()),
            bounds,
            group: i.is_group(),
        })
    }).collect()
}

/// Texts that read as one sentence but are drawn as separate texts side by
/// side on one line ("One" + " word at a time"): a translation cannot keep
/// their order, and each piece is translated alone. A sentence is one text;
/// the part that animates on its own is a range of it.
fn split_sentences(frame: &MotionFrame, ink: &[geometry::Ink], visible: &[bool], out: &mut Vec<LayoutIssue>) {
    let runs: Vec<(&str, &str, geometry::Box2, f64)> = ink.iter().zip(visible).filter(|(_, v)| **v)
        .flat_map(|(i, _)| i.runs.iter().map(move |(text, b, em)| (i.path.as_str(), text.as_str(), *b, *em)))
        .filter(|(_, text, _, em)| text.trim().chars().count() >= 2 && *em > 0.0 && text.chars().any(char::is_alphabetic))
        .collect();
    let ends_sentence = |t: &str| t.trim_end().ends_with(['.', '!', '?', ':', ';', '。', '！', '？', '：', '；']);
    for (i, a) in runs.iter().enumerate() {
        for b in runs.iter().skip(i + 1).chain(runs.iter().take(i)) {
            let (ha, hb) = (a.2[3] - a.2[1], b.2[3] - b.2[1]);
            let overlap = a.2[3].min(b.2[3]) - a.2[1].max(b.2[1]);
            let same_line = overlap > 0.6 * ha.min(hb) && (a.3 - b.3).abs() < 0.2 * a.3.max(b.3);
            let gap = b.2[0] - a.2[2];
            if same_line && gap > -0.05 * a.3 && gap < 0.6 * a.3 && !ends_sentence(a.1) {
                out.push(issue(frame.t, "text.split_sentence", a.0, Some(b.0),
                    format!("`{}` and `{}` read as one sentence drawn as separate texts", a.1.trim(), b.1.trim()),
                    "write the sentence as one text and animate the part that moves with a range of it (`ranges: [Range{by: @word from: … to: …}]`, or one c.text with a highlight): separate pieces are translated alone and cannot change their order"));
            }
        }
    }
}

/// The groups whose paths are letters (`Group{glyphs: true}`).
fn glyph_groups(doc: &MotionDoc) -> Vec<String> {
    doc.all_layers().into_iter().filter(|l| matches!(l.kind, crate::LayerKind::Group { glyphs: true, .. })).map(|l| l.path.clone()).collect()
}

fn ordered_times(doc: &MotionDoc, times: &[f64]) -> Vec<f64> {
    let mut times: Vec<_> = times.iter().copied().filter(|t| t.is_finite() && *t >= 0.0 && *t < doc.duration).collect();
    times.sort_by(f64::total_cmp);
    times.dedup();
    times
}

/// A finding shorter than this (a moving line sweeping past a label) is
/// motion, not a layout problem; one that holds is reported.
pub const MIN_RUN_SECONDS: f64 = 0.25;

/// Check the supplied times (or [`sample_times`]). Each consecutive run
/// is a separate finding: a clear sample between two collisions is never
/// reported as though the collision lasted through that gap. A run lasts
/// until the first clear sample (or the document's end); runs shorter than
/// [`MIN_RUN_SECONDS`] are dropped, unless only one time was checked.
/// The continuity check ([`pop_checks`]) runs on the same samples: a
/// layer that appears or vanishes in one frame without an entrance or exit.
pub fn layout_checks(doc: &MotionDoc, times: &[f64], min_text_px: f64) -> Vec<LayoutIssue> {
    let times = ordered_times(doc, times);
    let mut out: Vec<(LayoutIssue, f64)> = Vec::new();
    let mut active: BTreeMap<(&'static str, String, Option<String>), usize> = BTreeMap::new();
    let mut samples: Vec<(f64, Presence)> = Vec::with_capacity(times.len());
    for &t in &times {
        let mut next = BTreeMap::new();
        let (findings, presence) = inspect_measured(doc, &doc.eval(t), min_text_px);
        samples.push((t, presence));
        for finding in findings {
            let key = (finding.code, finding.path.clone(), finding.other.clone());
            let index = match active.get(&key).copied() {
                Some(i) => { out[i].0.to = t; i }
                None => { out.push((finding, doc.duration)); out.len() - 1 }
            };
            next.insert(key, index);
        }
        for (key, i) in &active {
            if !next.contains_key(key) { out[*i].1 = t; }
        }
        active = next;
    }
    let single = times.len() <= 1;
    let mut out: Vec<LayoutIssue> = out.into_iter().filter(|(issue, until)| single || until - issue.from >= MIN_RUN_SECONDS - 1e-9).map(|(issue, _)| issue).collect();
    if !single {
        out.extend(pops::pops(doc, &samples));
    }
    out
}

/// Sampled visibility. Runs are half-open, ending at the first invisible
/// sample (or document end); first/last name actual visible sample times.
/// This is geometric visibility, before effects and occlusion by layers.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerVisibility {
    pub path: String,
    pub first: Option<f64>,
    pub last: Option<f64>,
    pub spans: Vec<(f64, f64)>,
    pub gaps: Vec<(f64, f64)>,
    /// Some samples require renderer-only geometry (for example a mask).
    pub unchecked: bool,
}

impl LayerVisibility {
    pub fn to_text(&self) -> String {
        let text = match (self.first, self.last) {
            (Some(first), Some(last)) => {
                let gaps = if self.gaps.is_empty() { "none".into() } else {
                    self.gaps.iter().map(|(a, b)| format!("{a:.3}–{b:.3} s")).collect::<Vec<_>>().join(", ")
                };
                format!("sampled visibility: first {first:.3} s, last {last:.3} s; gaps: {gaps}")
            }
            _ if self.unchecked => "no measured visibility".into(),
            _ => "never visible in sampled frames".into(),
        };
        if self.unchecked { format!("{text}; visibility unchecked where geometry requires rendering") } else { text }
    }
}

/// Every declared and generated layer's visibility, including wholly
/// hidden layers. Transparent paint, unrevealed letters and off-canvas
/// geometry do not count. Groups are visible when a child is visible.
pub fn visibility(doc: &MotionDoc, times: &[f64]) -> Vec<LayerVisibility> {
    let mut layers: BTreeMap<String, LayerVisibility> = doc.all_layers().into_iter().map(|l| {
        (l.path.clone(), LayerVisibility { path: l.path.clone(), first: None, last: None, spans: Vec::new(), gaps: Vec::new(), unchecked: false })
    }).collect();
    let mut active: BTreeMap<String, f64> = BTreeMap::new();
    let glyphs = glyph_groups(doc);
    for t in ordered_times(doc, times) {
        let frame = doc.eval(t);
        let (mut ink, mut unchecked) = (Vec::new(), Vec::new());
        let canvas = [0.0, 0.0, frame.size[0], frame.size[1]];
        geometry::collect(&frame.items, 1.0, true, canvas, &glyphs, &mut ink, &mut unchecked);
        for (path, _) in unchecked {
            let child_prefix = format!("{path}.");
            for layer in layers.values_mut().filter(|l| l.path == path || l.path.starts_with(&child_prefix)) {
                layer.unchecked = true;
            }
        }
        let mut visible: BTreeSet<_> = ink.iter().filter(|i| i.visible(canvas)).map(|i| i.path.clone()).collect();
        // A Repeat template is visible whenever any of its copies is.
        let sources: Vec<_> = visible.iter().filter_map(|p| p.split_once('#').map(|(base, _)| base.to_string())).collect();
        visible.extend(sources);
        let ended: Vec<_> = active.keys().filter(|p| !visible.contains(*p)).cloned().collect();
        for path in ended {
            let start = active.remove(&path).unwrap();
            layers.get_mut(&path).unwrap().spans.push((start, t));
        }
        for path in visible {
            let layer = layers.entry(path.clone()).or_insert_with(|| LayerVisibility { path: path.clone(), first: None, last: None, spans: Vec::new(), gaps: Vec::new(), unchecked: false });
            layer.first.get_or_insert(t);
            layer.last = Some(t);
            active.entry(path).or_insert(t);
        }
    }
    for (path, start) in active {
        layers.get_mut(&path).unwrap().spans.push((start, doc.duration));
    }
    for layer in layers.values_mut() {
        layer.gaps = layer.spans.windows(2).map(|p| (p[0].1, p[1].0)).collect();
    }
    layers.into_values().collect()
}

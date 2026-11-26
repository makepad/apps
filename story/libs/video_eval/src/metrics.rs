//! Measured metrics: facts of the generated edit (from `CutDoc`), brief
//! conformance checks, the inspect report's counts, and motion density
//! from the motion documents' sources. All pure; the tool runs live in
//! `tools.rs`.

use crate::bench::Case;
use makepad_micro_serde::*;
use makepad_story_cut::{CutDoc, CutNodeKind, MediaRef};

/// What the cut is, as loaded.
#[derive(Clone, Debug, Default, SerJson, DeJson)]
pub struct CutFacts {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64,
    pub nodes: u32,
    /// Motion graphics placed (`Graphic` nodes).
    pub graphics: u32,
    pub titles: u32,
    pub clips: u32,
    /// Clips that play an app run's render.
    pub app_runs: u32,
    pub voiceovers: u32,
    pub voice_lines: u32,
    pub subtitles: u32,
    pub audio_clips: u32,
    pub transitions: u32,
    pub effects: u32,
}

impl CutFacts {
    pub fn of(doc: &CutDoc) -> CutFacts {
        let mut f = CutFacts {
            width: doc.output.width,
            height: doc.output.height,
            fps: doc.output.fps,
            duration: doc.duration,
            nodes: doc.nodes.len() as u32,
            ..Default::default()
        };
        for node in &doc.nodes {
            match &node.kind {
                CutNodeKind::Clip { src, .. } => {
                    f.clips += 1;
                    if matches!(src, MediaRef::Run(_)) {
                        f.app_runs += 1;
                    }
                }
                CutNodeKind::Title { .. } => f.titles += 1,
                CutNodeKind::VoiceOver { lines, .. } => {
                    f.voiceovers += 1;
                    f.voice_lines += lines.len() as u32;
                }
                CutNodeKind::Subtitles { .. } => f.subtitles += 1,
                CutNodeKind::AudioClip { .. } => f.audio_clips += 1,
                CutNodeKind::Transition { .. } => f.transitions += 1,
                CutNodeKind::Fx { .. } => f.effects += 1,
                _ => {}
            }
        }
        // Graphic nodes are kept apart from `nodes`.
        f.graphics = doc.graphics.len() as u32;
        f.nodes += f.graphics;
        f
    }
}

#[derive(Clone, Debug, SerJson, DeJson)]
pub struct Check {
    pub name: String,
    pub pass: bool,
    pub detail: String,
}

fn check(name: &str, pass: bool, detail: String) -> Check {
    Check { name: name.to_string(), pass, detail }
}

/// Does the cut deliver the brief's measurable parts?
pub fn conformance(case: &Case, facts: &CutFacts, motion: &MotionStats) -> Vec<Check> {
    let mut out = vec![
        check(
            "size",
            facts.width == case.width && facts.height == case.height,
            format!("{}x{} (brief {}x{})", facts.width, facts.height, case.width, case.height),
        ),
        // The brief's rate is a floor: 60 fps or more.
        check("fps", facts.fps >= case.fps - 0.01, format!("{} (brief ≥ {})", facts.fps, case.fps)),
        check(
            "duration",
            (facts.duration - case.length_s).abs() <= case.length_s * 0.1,
            format!("{:.1} s (brief {:.1} s ±10 %)", facts.duration, case.length_s),
        ),
        // One long Motion document can hold every scene, so the brief's
        // "≥ n graphics" also counts animated elements (top-level layers
        // that move, a group counting once) across the placed documents.
        check(
            "graphics",
            facts.graphics >= case.min_graphics || motion.elements.unwrap_or(0) >= case.min_graphics,
            format!("{} placed, {} animated elements (brief ≥ {})", facts.graphics, motion.elements.unwrap_or(0), case.min_graphics),
        ),
    ];
    if case.voice {
        out.push(check("voiceover", facts.voice_lines > 0, format!("{} lines", facts.voice_lines)));
    }
    if case.subtitles {
        out.push(check("subtitles", facts.subtitles > 0, format!("{} subtitle nodes", facts.subtitles)));
    }
    if let Some(app) = &case.app {
        out.push(check("app run", facts.app_runs > 0, format!("{} app-run clips (brief: {app})", facts.app_runs)));
    }
    if case.chart {
        out.push(check("chart", motion.chart_like, "a motion document draws a chart (heuristic)".to_string()));
    }
    out
}

/// Counts from `makepad-edit-tools inspect --json`. The report is read
/// loosely: every object with a `severity` (or `level`) is an issue; its
/// `code`/`kind`/`rule` names the class.
#[derive(Clone, Debug, Default, SerJson, DeJson)]
pub struct InspectSummary {
    pub errors: u32,
    pub warnings: u32,
    pub safe_zone: u32,
    pub overflow: u32,
    /// The first issues as `severity code: message`, for the table's notes.
    pub first: Vec<String>,
}

/// The contact sheet's picture numbers (`strip --json`'s `exposure`, from
/// the frames as read back): the worst frame for clipping and for black,
/// with their times, and the mean luma. Reported, never scored: a dark
/// look can be what the brief asked for.
#[derive(Clone, Debug, Default, PartialEq, SerJson, DeJson)]
pub struct Exposure {
    /// The largest share of clipped pixels (a channel at 99 %+) in a frame.
    pub clipped: f64,
    pub clipped_at: f64,
    /// The largest share of near-black pixels (luma under 2.5 %) in a frame.
    pub near_black: f64,
    pub near_black_at: f64,
    pub mean_luma: f64,
}

pub fn exposure_of(report: &JsonValue) -> Option<Exposure> {
    let exposure = report.get("exposure")?;
    let worst = |key: &str| {
        let worst = exposure.get(key)?;
        Some((worst.get("share")?.as_f64()?, worst.get("t")?.as_f64()?))
    };
    let (clipped, clipped_at) = worst("most_clipped")?;
    let (near_black, near_black_at) = worst("most_near_black")?;
    Some(Exposure { clipped, clipped_at, near_black, near_black_at, mean_luma: exposure.get("mean_luma")?.as_f64()? })
}

pub fn summarize_inspect(report: &JsonValue) -> InspectSummary {
    let mut s = InspectSummary::default();
    visit_issues(report, &mut s);
    s
}

fn field<'a>(v: &'a JsonValue, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(k).and_then(JsonValue::as_str))
}

fn visit_issues(v: &JsonValue, s: &mut InspectSummary) {
    match v {
        JsonValue::Array(items) => items.iter().for_each(|i| visit_issues(i, s)),
        JsonValue::Object(map) => {
            if let Some(sev) = field(v, &["severity", "level"]) {
                let sev = sev.to_ascii_lowercase();
                let code = field(v, &["code", "kind", "rule", "check"]).unwrap_or("").to_ascii_lowercase();
                let msg = field(v, &["message", "msg", "text"]).unwrap_or("");
                let is_error = sev.starts_with("err");
                let is_warning = sev.starts_with("warn");
                if is_error {
                    s.errors += 1;
                } else if is_warning {
                    s.warnings += 1;
                }
                if is_error || is_warning {
                    let class = format!("{code} {}", msg.to_ascii_lowercase());
                    if code.contains("safe") || class.contains("safe zone") || class.contains("safe-zone") {
                        s.safe_zone += 1;
                    } else if ["overflow", "outside", "out_of_frame", "out-of-frame", "offscreen", "off_frame", "clipped"]
                        .iter()
                        .any(|k| code.contains(k))
                    {
                        s.overflow += 1;
                    }
                    if s.first.len() < 8 {
                        s.first.push(format!("{sev} {code}: {msg}"));
                    }
                }
                return;
            }
            // Deterministic order for the notes.
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            for k in keys {
                visit_issues(&map[k], s);
            }
        }
        _ => {}
    }
}

/// Layers of one motion document, from its source.
#[derive(Clone, Debug, Default, PartialEq, SerJson, DeJson)]
pub struct MotionDocStats {
    pub path: String,
    pub layers: u32,
    pub animated: u32,
    /// Top-level layers that move (themselves or any child). Optional
    /// only so results written before it existed still load.
    pub elements: Option<u32>,
}

#[derive(Clone, Debug, Default, SerJson, DeJson)]
pub struct MotionStats {
    pub docs: Vec<MotionDocStats>,
    pub animated_layers: u32,
    /// Animated top-level elements over all documents (optional as above).
    pub elements: Option<u32>,
    /// Animated layers per second of cut.
    pub density: f64,
    pub chart_like: bool,
}

/// Motion density of the cut: the animated layers of its motion documents
/// per second of cut duration.
pub fn motion_stats(sources: &[(String, String)], duration: f64) -> MotionStats {
    let mut out = MotionStats::default();
    for (path, source) in sources {
        let layers = scan_motion_layers(source);
        let mut stats = MotionDocStats { path: path.clone(), ..Default::default() };
        count_layers(&layers, &mut stats);
        let elements = layers.iter().filter(|l| moves(l)).count() as u32;
        stats.elements = Some(elements);
        out.animated_layers += stats.animated;
        out.elements = Some(out.elements.unwrap_or(0) + elements);
        out.docs.push(stats);
        let lower = source.to_ascii_lowercase();
        if ["chart", "donut", "bar_", "bars", "axis", "pie", "slice", "legend", "histogram"].iter().any(|k| lower.contains(k)) {
            out.chart_like = true;
        }
    }
    out.density = if duration > 0.0 { out.animated_layers as f64 / duration } else { 0.0 };
    out
}

fn moves(layer: &Layer) -> bool {
    layer.animated || layer.children.iter().any(moves)
}

fn count_layers(layers: &[Layer], stats: &mut MotionDocStats) {
    for layer in layers {
        stats.layers += 1;
        if layer.animated {
            stats.animated += 1;
        }
        count_layers(&layer.children, stats);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub kind: String,
    /// One of its own properties is keyed (`tween`, `keys`, `sequence`,
    /// `loop`, `stagger`), or it plays time itself (Video, Shader).
    pub animated: bool,
    pub children: Vec<Layer>,
    /// Its own `name: "…"`, when it has one.
    pub name: Option<String>,
    /// Its own `span: [start, length]` (a child's is relative to its group).
    pub span: Option<(f64, f64)>,
    /// `Text` layers anywhere inside it, itself included.
    pub texts: u32,
    /// A word sequencer (`Words{every: … words: [...]}`) anywhere inside
    /// it: its word count and its `every:` value as written.
    pub words: Option<(u32, String)>,
}

const ANIMATORS: &[&str] = &["tween", "keys", "sequence", "loop", "stagger"];

/// The layer tree of a `Motion{ layers: [...] }` source. A light scanner,
/// not the Splash parser: strings and comments are blanked, then the first
/// `layers: [` array is split into `Kind{...}` entries, recursively for
/// groups. Unparsable input yields what was found so far.
pub fn scan_motion_layers(source: &str) -> Vec<Layer> {
    let clean = blank_strings_and_comments(source);
    match find_layers_array(&clean, 0, clean.len()) {
        Some((open, close)) => parse_layer_array(&clean, source.as_bytes(), open + 1, close),
        None => Vec::new(),
    }
}

/// The position just after `key:` among the depth-0 properties of `b`
/// (nested objects and arrays are skipped).
fn field_at_depth0(b: &[u8], key: &str) -> Option<usize> {
    let k = key.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => depth -= 1,
            _ if depth == 0 && b[i..].starts_with(k) && (i == 0 || !is_ident(b[i - 1])) => {
                let mut j = i + k.len();
                while j < b.len() && b[j] == b' ' {
                    j += 1;
                }
                if j < b.len() && b[j] == b':' {
                    return Some(j + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Up to `n` numbers starting at `i` (inside `[…]` or `vec2(…)`).
fn numbers_at(b: &[u8], i: usize, n: usize) -> Vec<f64> {
    let mut out = Vec::new();
    let mut j = i;
    while j < b.len() && out.len() < n {
        let c = b[j];
        if c.is_ascii_alphabetic() || c == b'_' {
            // An identifier such as `vec2`: its digits are not a value.
            while j < b.len() && is_ident(b[j]) {
                j += 1;
            }
        } else if c.is_ascii_digit() || c == b'-' || c == b'.' {
            let start = j;
            j += 1;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.' || b[j] == b'e' || b[j] == b'E') {
                j += 1;
            }
            match std::str::from_utf8(&b[start..j]).ok().and_then(|t| t.parse().ok()) {
                Some(v) => out.push(v),
                None => break,
            }
        } else if c == b']' || c == b')' || c == b'}' || c == b'\n' && !out.is_empty() {
            break;
        } else {
            j += 1;
        }
    }
    out
}

/// The string value of a depth-0 `key: "…"`, read from the original text
/// (the clean text has its string contents blanked).
fn string_field(clean: &[u8], orig: &[u8], key: &str) -> Option<String> {
    let at = skip_ws(clean, field_at_depth0(clean, key)?);
    if clean.get(at) != Some(&b'"') {
        return None;
    }
    let end = (at + 1..clean.len()).find(|&j| clean[j] == b'"')?;
    Some(String::from_utf8_lossy(&orig[at + 1..end]).into_owned())
}

fn number_field(clean: &[u8], key: &str, n: usize) -> Option<Vec<f64>> {
    let at = field_at_depth0(clean, key)?;
    let v = numbers_at(clean, at, n);
    (v.len() == n).then_some(v)
}

/// One chapter of a document: the top-level layers sharing a name prefix
/// (`c03.plate`, `c03.fly` → `c03`), or one named top-level layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub name: String,
    pub start: f64,
    pub end: f64,
    pub texts: u32,
    /// Spans of the named layers inside (relative to the chapter's layer),
    /// for a montage's word timing.
    pub inner: Vec<(String, f64, f64)>,
    /// A word sequencer in the chapter: word count and `every:` as written.
    pub words: Option<(u32, String)>,
}

/// The time structure of a Motion document: header and chapters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timing {
    pub bpm: Option<f64>,
    pub duration: Option<f64>,
    pub size: Option<(f64, f64)>,
    pub chapters: Vec<Chapter>,
}

pub fn scan_timing(source: &str) -> Timing {
    let clean = blank_strings_and_comments(source);
    let mut t = Timing::default();
    // The document body: inside the first `{…}`.
    if let Some(open) = clean.iter().position(|&c| c == b'{') {
        if let Some(close) = matching(&clean, open, clean.len()) {
            let body = &clean[open + 1..close];
            t.bpm = number_field(body, "bpm", 1).map(|v| v[0]);
            t.duration = number_field(body, "duration", 1).map(|v| v[0]);
            t.size = number_field(body, "size", 2).map(|v| (v[0], v[1]));
        }
    }
    for layer in scan_motion_layers(source) {
        let (Some(name), Some((start, len))) = (layer.name.clone(), layer.span) else { continue };
        let key = name.split('.').next().unwrap_or(&name).to_string();
        let inner: Vec<(String, f64, f64)> =
            layer.children.iter().filter_map(|c| Some((c.name.clone()?, c.span?.0, c.span?.1))).collect();
        match t.chapters.iter_mut().find(|c| c.name == key) {
            Some(c) => {
                c.start = c.start.min(start);
                c.end = c.end.max(start + len);
                c.texts += layer.texts;
                c.inner.extend(inner);
                if c.words.is_none() {
                    c.words = layer.words.clone();
                }
            }
            None => t.chapters.push(Chapter { name: key, start, end: start + len, texts: layer.texts, inner, words: layer.words.clone() }),
        }
    }
    // Layers across (nearly) the whole document are persistent furniture
    // (a HUD, a background), not chapters.
    if let Some(d) = t.duration.filter(|d| *d > 0.0) {
        if t.chapters.iter().any(|c| c.end - c.start < 0.8 * d) {
            t.chapters.retain(|c| c.end - c.start < 0.8 * d);
        }
    }
    t.chapters.sort_by(|a, b| a.start.total_cmp(&b.start));
    t
}

/// A chapter cut against the beat grid: `(chapter, time, beat index,
/// offset in seconds from the nearest beat)`.
pub fn beat_offsets(chapters: &[Chapter], bpm: f64) -> Vec<(String, f64, f64, f64)> {
    let beat = 60.0 / bpm;
    chapters
        .iter()
        .skip(1)
        .map(|c| {
            let n = (c.start / beat).round();
            (c.name.clone(), c.start, c.start / beat, c.start - n * beat)
        })
        .collect()
}

fn blank_strings_and_comments(src: &str) -> Vec<u8> {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    out[i] = b' ';
                    i += 1;
                }
                out[i] = b' ';
                i += 1;
            }
            i += 1;
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                out[i] = b' ';
                i += 1;
            }
            if i < b.len() {
                out[i] = b' ';
                if i + 1 < b.len() {
                    out[i + 1] = b' ';
                }
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The matching close of the bracket at `open`.
fn matching(b: &[u8], open: usize, end: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, &c) in b.iter().enumerate().take(end).skip(open) {
        match c {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The first `layers: [` in `b[from..end]` at the shallowest nesting,
/// as the positions of its brackets.
fn find_layers_array(b: &[u8], from: usize, end: usize) -> Option<(usize, usize)> {
    let mut i = from;
    let mut depth = 0i32;
    let mut best: Option<(i32, usize, usize)> = None;
    while i < end {
        match b[i] {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => depth -= 1,
            b'l' if b[i..end].starts_with(b"layers") && (i == 0 || !is_ident(b[i - 1])) => {
                let mut j = i + 6;
                while j < end && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < end && b[j] == b':' {
                    j += 1;
                    while j < end && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < end && b[j] == b'[' {
                        if let Some(close) = matching(b, j, end) {
                            if best.map_or(true, |(d, _, _)| depth < d) {
                                best = Some((depth, j, close));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    best.map(|(_, open, close)| (open, close))
}

fn parse_layer_array(b: &[u8], orig: &[u8], mut i: usize, end: usize) -> Vec<Layer> {
    let mut out = Vec::new();
    while i < end {
        if !b[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < end && is_ident(b[i]) {
            i += 1;
        }
        let kind = String::from_utf8_lossy(&b[start..i]).to_string();
        while i < end && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= end || b[i] != b'{' {
            continue;
        }
        let Some(close) = matching(b, i, end) else { break };
        let (children, own) = match find_layers_array(b, i + 1, close) {
            Some((open, cclose)) => {
                let mut own = b[i + 1..close].to_vec();
                own[open - i - 1..=cclose - i - 1].iter_mut().for_each(|c| *c = b' ');
                (parse_layer_array(b, orig, open + 1, cclose), own)
            }
            None => (Vec::new(), b[i + 1..close].to_vec()),
        };
        let animated = kind == "Video" || kind == "Shader" || has_animator(&own);
        // The own-props region with its original text, for string values.
        let own_orig: Vec<u8> = orig[i + 1..close].to_vec();
        let name = string_field(&own, &own_orig, "name");
        let span = number_field(&own, "span", 2).map(|v| (v[0], v[1]));
        let texts = u32::from(kind == "Text") + children.iter().map(|c| c.texts).sum::<u32>();
        let words = if kind == "Words" {
            let count = own.windows(5).filter(|w| w == b"text:").count() as u32;
            let every = field_at_depth0(&own, "every")
                .map(|at| {
                    let at = skip_ws(&own, at);
                    let mut end = at;
                    let mut depth = 0;
                    while end < own.len() && !(depth == 0 && (own[end].is_ascii_whitespace() || own[end] == b'}')) {
                        match own[end] {
                            b'(' => depth += 1,
                            b')' => depth -= 1,
                            _ => {}
                        }
                        end += 1;
                    }
                    String::from_utf8_lossy(&own_orig[at..end]).into_owned()
                })
                .unwrap_or_default();
            Some((count, every))
        } else {
            children.iter().find_map(|c| c.words.clone())
        };
        out.push(Layer { kind, animated, children, name, span, texts, words });
        i = close + 1;
    }
    out
}

fn has_animator(b: &[u8]) -> bool {
    (0..b.len()).any(|i| {
        (i == 0 || !is_ident(b[i - 1]))
            && ANIMATORS.iter().any(|a| {
                let a = a.as_bytes();
                b[i..].starts_with(a) && {
                    let mut j = i + a.len();
                    while j < b.len() && b[j] == b' ' {
                        j += 1;
                    }
                    j < b.len() && b[j] == b'('
                }
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"use mod.motion.*; Motion{
    size: vec2(1920, 1080) duration: 6.0
    inputs: {headline: "Notes, tween(instantly)" accent: #ff5a36}
    // keys( in a comment is not motion
    layers: [
        Rect{at: vec2(0,0) size: vec2(1920,1080) fill: #0b0d12}
        Text{text: in(@headline) by: @letter stagger: 0.03
             opacity: tween(0, 1, 0.0, 0.4, @ease_out)}
        Group{opacity: 1.0 layers: [
            Path{svg: "M0 0 L400 0" trim_end: tween(0, 1, 0.5, 1.1, @ease_in_out)}
            Ellipse{at: vec2(10, 10) size: vec2(4, 4)}
        ]}
        Image{src: asset("a", "b") scale: keys([0, 0.9], [1.2, 1.0, @ease_out])}
        Shader{effect: "kit/grain" opacity: 0.15}
    ]
}"#;

    #[test]
    fn scans_layers_and_animation() {
        let layers = scan_motion_layers(DOC);
        let kinds: Vec<_> = layers.iter().map(|l| (l.kind.as_str(), l.animated)).collect();
        assert_eq!(kinds, [("Rect", false), ("Text", true), ("Group", false), ("Image", true), ("Shader", true)]);
        let group = &layers[2];
        assert_eq!(group.children.iter().map(|l| (l.kind.as_str(), l.animated)).collect::<Vec<_>>(), [("Path", true), ("Ellipse", false)]);
    }

    #[test]
    fn density_is_animated_layers_per_second() {
        let stats = motion_stats(&[("a.splash".into(), DOC.into()), ("b.splash".into(), DOC.into())], 8.0);
        assert_eq!(stats.docs[0].layers, 7);
        assert_eq!(stats.docs[0].animated, 4);
        assert_eq!(stats.animated_layers, 8);
        // Text, Group (its Path moves), Image, Shader; the Rect is still.
        assert_eq!(stats.docs[0].elements, Some(4));
        assert_eq!(stats.elements, Some(8));
        assert!((stats.density - 1.0).abs() < 1e-9);
        assert!(!stats.chart_like);
        assert_eq!(motion_stats(&[], 0.0).density, 0.0);
    }

    #[test]
    fn timing_groups_chapters_and_measures_the_beat_grid() {
        let doc = r#"use mod.motion.*; Motion{
    label: "Reel" size: vec2(1920, 1080) duration: 15.0 bpm: 120
    layers: [
        // 01
        Rect{name: "c01.plate" span: [0.0, 2.0] size: vec2(1920, 1080)}
        Text{name: "c01.title" span: [0.25, 1.5] text: "EASE" morph: {span: [9, 9]}}
        Rect{name: "c02.plate" span: [2.0, 2.0]}
        Group{name: "c03.words" span: [4.03, 1.0] layers: [
            Text{name: "w1" span: [0.0, 0.0625] text: "ONE"}
            Text{name: "w2" span: [0.0625, 0.0625] text: "TWO"}
            Words{name: "w" every: beats(0.5) words: [{text: "A"} {text: "B" look: @blur} {text: "C"}]}
        ]}
        Rect{name: "unnamed" span: [6.2, 1.0]}
        Hud{name: "hud" span: [0.0, 14.1]}
    ]
}"#;
        let t = scan_timing(doc);
        assert_eq!((t.bpm, t.duration, t.size), (Some(120.0), Some(15.0), Some((1920.0, 1080.0))));
        let names: Vec<_> = t.chapters.iter().map(|c| (c.name.as_str(), c.start, c.end, c.texts)).collect();
        assert_eq!(names, [("c01", 0.0, 2.0, 1), ("c02", 2.0, 4.0, 0), ("c03", 4.03, 5.03, 2), ("unnamed", 6.2, 7.2, 0)]);
        assert_eq!(t.chapters[2].inner, [("w1".to_string(), 0.0, 0.0625), ("w2".to_string(), 0.0625, 0.0625)]);
        assert_eq!(t.chapters[2].words, Some((3, "beats(0.5)".to_string())));
        assert_eq!(t.chapters[0].words, None);
        let off = beat_offsets(&t.chapters, 120.0);
        assert_eq!(off.len(), 3);
        assert!((off[0].3).abs() < 1e-9 && (off[1].3 - 0.03).abs() < 1e-9 && (off[2].3 - 0.2).abs() < 1e-9);
    }

    #[test]
    fn inspect_counts_by_severity_and_class() {
        let report = JsonValue::deserialize_json(
            r#"{"file":"e.splash","issues":[
                {"severity":"error","code":"missing_asset","message":"asset x"},
                {"severity":"warning","code":"safe_zone","message":"title in the top UI zone"},
                {"severity":"warning","code":"text_overflow","message":"headline wider than frame"},
                {"severity":"info","code":"static_layer","message":"ok"}],
              "layers":[{"level":"error","kind":"out_of_frame","msg":"rect"}]}"#,
        )
        .unwrap();
        let s = summarize_inspect(&report);
        assert_eq!((s.errors, s.warnings, s.safe_zone, s.overflow), (2, 2, 1, 2));
        assert_eq!(s.first.len(), 4);
    }

    /// The contact sheet's exposure is read from `strip --json` as the
    /// edit tools write it (a share of 0 written as an integer too).
    #[test]
    fn exposure_comes_from_the_strip_report() {
        let report = JsonValue::deserialize_json(
            r#"{"ok":true,"command":"strip","frames":[],"exposure":{"most_clipped":{"share":0.125,"frame":48,"t":1.6},"most_near_black":{"share":0,"frame":0,"t":0},"mean_luma":0.41,"mean_luma_span":[0.2,0.6]}}"#,
        )
        .unwrap();
        assert_eq!(exposure_of(&report), Some(Exposure { clipped: 0.125, clipped_at: 1.6, near_black: 0.0, near_black_at: 0.0, mean_luma: 0.41 }));
        assert_eq!(exposure_of(&JsonValue::deserialize_json(r#"{"ok":true}"#).unwrap()), None);
    }

    #[test]
    fn conformance_checks_the_brief() {
        let case = crate::bench::select("launch-notes-vertical").unwrap().remove(0);
        let facts = CutFacts { width: 1080, height: 1920, fps: 60.0, duration: 28.0, graphics: 3, voice_lines: 4, subtitles: 1, ..Default::default() };
        let checks = conformance(&case, &facts, &MotionStats::default());
        assert!(checks.iter().all(|c| c.pass), "{checks:?}");
        // One placed document with enough moving elements meets "≥ 3 graphics".
        let one_doc = CutFacts { graphics: 1, ..facts.clone() };
        let rich = MotionStats { elements: Some(5), ..Default::default() };
        assert!(conformance(&case, &one_doc, &rich).iter().all(|c| c.pass));
        assert!(conformance(&case, &one_doc, &MotionStats::default()).iter().any(|c| c.name == "graphics" && !c.pass));
        let short = CutFacts { duration: 20.0, ..facts };
        let failed: Vec<_> = conformance(&case, &short, &MotionStats::default()).into_iter().filter(|c| !c.pass).map(|c| c.name).collect();
        assert_eq!(failed, ["duration"]);
    }
}

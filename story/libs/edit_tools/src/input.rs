//! What the tools read: a document (a cut, or a motion document shown as
//! a one-`Graphic` edit), times written the way people and agents write
//! them, and the local media table the CLI resolves references with.

use makepad_story_edit_render::audio::{fps_ratio, frame_count, frame_time};
use makepad_story_edit_render::{LocalMedia, MediaResolver, ResolvedMedia};
use makepad_story_cut::{AssetRef, Diagnostic, DiagnosticKind, CutDoc, CutNodeKind, MediaRef, RunRef, SpokenLine, TakeInfo};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    /// A `Flow{kind: @cut …}`.
    Cut,
    /// A `Motion{…}` document.
    Motion,
}

impl DocKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Cut => "cut",
            Self::Motion => "motion",
        }
    }
}

/// A document the tools work on. Every command evaluates `cut_source`: the
/// document itself for a cut, the one-`Graphic` edit around it for a
/// motion document (at the document's own size, rate and duration).
#[derive(Clone, Debug)]
pub struct Document {
    /// The file name diagnostics are reported under.
    pub name: String,
    pub kind: DocKind,
    /// The document as written.
    pub source: String,
    /// The cut the commands evaluate (empty for a motion document that
    /// does not load).
    pub cut_source: String,
    /// A motion document's stand-in asset: the revision the wrapping edit's
    /// `Graphic` names ([`Document::resolver`] answers it with `source`).
    pub motion: Option<AssetRef>,
    /// Why a motion document could not be wrapped (its load diagnostics).
    motion_errors: Vec<Diagnostic>,
    /// The folder the document was read from (for its `media.txt`).
    pub dir: Option<PathBuf>,
    /// The immutable tree and logical path of a cut supplied by its host.
    pub tree: Option<makepad_story_cut::tree::TreeContext>,
}

/// The fixed asset id a motion document stands in under.
pub fn motion_stand_in(source: &str) -> AssetRef {
    let digest = makepad_asset_data::BlobId::hash_of(source.as_bytes());
    AssetRef {
        id: makepad_asset_data::AssetId::from_bytes([0x6d; 16]).to_string(),
        rev: format!("arev_{}", digest.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>()),
    }
}

/// The one-`Graphic` edit that shows a motion document of `size` for
/// `duration` at `fps` (sides rounded up to even, as encoders need).
pub fn motion_edit_source(asset: &AssetRef, label: &str, size: [f64; 2], duration: f64, fps: f64, bpm: f64) -> String {
    let even = |v: f64| ((v.round().max(2.0) as u32 + 1) / 2) * 2;
    format!(
        "use mod.flow.*\nuse mod.cut.*\n\nlet graphic = Graphic{{src: asset(\"{}\", \"{}\") span: [0.0, {duration}] fit: @fit}}\nlet out = CutOutput{{frame: graphic.out(@frame)}}\n\n\
         Flow{{\n    kind: @cut\n    label: \"{}\"\n    output: {{width: {} height: {} fps: {fps}}}\n    duration: {duration}\n    bpm: {bpm}\n    graphic, out\n}}\n",
        asset.id,
        asset.rev,
        label.replace('"', "'"),
        even(size[0]),
        even(size[1])
    )
}

impl Document {
    /// A document from its text; `name` is what diagnostics call it.
    pub fn from_source(source: &str, name: &str) -> Result<Document, String> {
        let kind = detect_kind(source);
        let mut doc = Document { name: name.to_string(), kind, source: source.to_string(), cut_source: String::new(), motion: None, motion_errors: Vec::new(), dir: None, tree: None };
        match kind {
            DocKind::Cut => doc.cut_source = source.to_string(),
            DocKind::Motion => match makepad_motion::MotionDoc::load(source) {
                Ok(motion) => {
                    let asset = motion_stand_in(source);
                    doc.cut_source = motion_edit_source(&asset, &motion.label, motion.size, motion.duration, motion.fps, motion.bpm);
                    doc.motion = Some(asset);
                }
                Err(diags) => {
                    doc.motion_errors = diags
                        .iter()
                        .filter(|d| d.severity == makepad_motion::Severity::Error)
                        .map(|d| Diagnostic { kind: DiagnosticKind::Eval, line: d.loc.line, col: d.loc.col, node: (!d.path.is_empty()).then(|| d.path.clone()), message: d.message.clone() })
                        .collect();
                }
            },
        }
        Ok(doc)
    }

    pub fn read(path: &Path) -> Result<Document, String> {
        let source = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut doc = Self::from_source(&source, &path.to_string_lossy())?;
        doc.dir = path.parent().map(Path::to_path_buf);
        Ok(doc)
    }

    /// The typed edit, or its load diagnostics (in the document's own lines;
    /// a motion document's own when it does not load).
    pub fn load(&self) -> Result<CutDoc, Vec<Diagnostic>> {
        if !self.motion_errors.is_empty() {
            return Err(self.motion_errors.clone());
        }
        match &self.tree {
            Some(tree) => CutDoc::load_tree(&self.cut_source, tree.clone()),
            None => CutDoc::load_named(&self.cut_source, &self.name),
        }
    }

    /// `inner` for drawing this document: a motion document's stand-in
    /// asset is answered with its own source, everything else by `inner`.
    pub fn resolver(&self, inner: Box<dyn MediaResolver>) -> Box<dyn MediaResolver> {
        match &self.motion {
            Some(asset) => Box::new(WithMotion { inner, asset: asset.clone(), source: self.source.clone() }),
            None => inner,
        }
    }

    /// `media.txt` beside the document, when there is one.
    pub fn sibling_media_file(&self) -> Option<PathBuf> {
        let path = self.dir.as_ref()?.join("media.txt");
        path.is_file().then_some(path)
    }
}

/// A cut is a flow ending in `Flow{…}`; a motion document is one
/// top-level `Motion{…}`. Comments and strings are skipped.
pub fn detect_kind(source: &str) -> DocKind {
    let mut depth = 0usize;
    let mut chars = source.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match c {
            '/' if matches!(chars.peek(), Some((_, '/'))) => {
                while chars.next_if(|(_, c)| *c != '\n').is_some() {}
            }
            '/' if matches!(chars.peek(), Some((_, '*'))) => {
                chars.next();
                let mut last = ' ';
                for (_, c) in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            '"' => {
                while let Some((_, c)) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth = depth.saturating_sub(1),
            'M' if depth == 0 && source[at..].starts_with("Motion") => {
                let prev = source[..at].chars().next_back();
                let rest = source[at + "Motion".len()..].trim_start();
                if !prev.is_some_and(|p| p.is_alphanumeric() || p == '_' || p == '.') && rest.starts_with('{') {
                    return DocKind::Motion;
                }
            }
            _ => {}
        }
    }
    DocKind::Cut
}

/// A time as written on the command line or by an agent: seconds (`2.5`,
/// `2.5s`), minutes and seconds (`1:02.5`), or a frame (`@45`, frame 45 at
/// `fps`). A frame number becomes that frame's exact time.
pub fn parse_time(text: &str, fps: f64) -> Result<f64, String> {
    let text = text.trim();
    let bad = || format!("`{text}` is not a time: write seconds (2.5), m:ss (1:02.5) or a frame (@45)");
    if let Some(frame) = text.strip_prefix('@') {
        let n: u64 = frame.trim().parse().map_err(|_| bad())?;
        return Ok(frame_time(n, fps_ratio(fps)));
    }
    let text = text.strip_suffix('s').unwrap_or(text);
    let seconds = match text.split_once(':') {
        Some((minutes, seconds)) => {
            let minutes: f64 = minutes.parse().map_err(|_| bad())?;
            let seconds: f64 = seconds.parse().map_err(|_| bad())?;
            minutes * 60.0 + seconds
        }
        None => text.parse::<f64>().map_err(|_| bad())?,
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(bad());
    }
    Ok(seconds)
}

/// The frame a time falls in (the render's grid), clamped to the cut.
pub fn frame_of(t: f64, fps: f64, duration: f64) -> u64 {
    let ratio = fps_ratio(fps);
    let frames = frame_count(duration, ratio).max(1);
    ((t.max(0.0) * ratio.0 as f64 / ratio.1 as f64 + 1e-9).floor() as u64).min(frames - 1)
}

/// `count` times spread over `[t0, t1]` (both ends included when `count`
/// is 2 or more), each snapped to the frame it falls in.
pub fn spread(t0: f64, t1: f64, count: usize, fps: f64, duration: f64) -> Vec<f64> {
    let ratio = fps_ratio(fps);
    let count = count.max(1);
    let mut out: Vec<f64> = (0..count)
        .map(|i| {
            let t = if count == 1 { t0 } else { t0 + (t1 - t0) * i as f64 / (count - 1) as f64 };
            frame_time(frame_of(t, fps, duration), ratio)
        })
        .collect();
    out.dedup_by(|a, b| a == b);
    out
}

/// The local media table: `--media id=file` pairs and the lines of a media
/// file (paths relative to it). Entries are `<asset id>=<file>` (a run's
/// render by its render asset id), `edit:<id>=<file.splash>` (a nested
/// edit), `lyrics:<id>=<file.json>` (a take's or transcript's word times)
/// and `effect:<id>=<file.splash>` (a library effect's source).
pub fn local_media(pairs: &[String], media_file: Option<&Path>) -> Result<LocalMedia, String> {
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for pair in pairs {
        let (id, file) = pair.split_once('=').ok_or_else(|| format!("--media {pair}: expected <id>=<file>"))?;
        entries.push((id.trim().to_string(), PathBuf::from(file.trim())));
    }
    if let Some(list) = media_file {
        let text = std::fs::read_to_string(list).map_err(|e| format!("{}: {e}", list.display()))?;
        let base = list.parent().unwrap_or(Path::new("."));
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let (id, file) = line.split_once('=').ok_or_else(|| format!("{}: `{line}` is not id=file", list.display()))?;
            entries.push((id.trim().to_string(), base.join(file.trim())));
        }
    }
    let mut media = LocalMedia::new();
    for (id, file) in entries {
        if let Some(id) = id.strip_prefix("lyrics:") {
            media.insert_lyrics(id, file);
        } else if let Some(id) = id.strip_prefix("edit:") {
            media.insert_edit(id, file);
        } else if let Some(id) = id.strip_prefix("effect:") {
            let source = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            media.insert_effect(id, source);
        } else {
            media.insert(id, file);
        }
    }
    Ok(media)
}

/// A resolver that also answers one motion document's source.
pub struct WithMotion {
    pub inner: Box<dyn MediaResolver>,
    pub asset: AssetRef,
    pub source: String,
}

impl MediaResolver for WithMotion {
    fn resolve(&mut self, media: &MediaRef) -> Result<ResolvedMedia, String> {
        self.inner.resolve(media)
    }
    fn effect_source(&mut self, effect: &AssetRef) -> Result<String, String> {
        self.inner.effect_source(effect)
    }
    fn take_info(&mut self, take: &AssetRef) -> Result<Option<TakeInfo>, String> {
        self.inner.take_info(take)
    }
    fn cut_source(&mut self, edit: &AssetRef) -> Result<String, String> {
        self.inner.cut_source(edit)
    }
    fn transcript(&mut self, transcript: &AssetRef) -> Result<Option<Vec<SpokenLine>>, String> {
        self.inner.transcript(transcript)
    }
    fn motion_source(&mut self, motion: &AssetRef) -> Result<String, String> {
        if *motion == self.asset {
            return Ok(self.source.clone());
        }
        self.inner.motion_source(motion)
    }
    fn asset_bytes(&mut self, asset: &AssetRef) -> Result<(Vec<u8>, &'static str), String> {
        self.inner.asset_bytes(asset)
    }    fn lyrics(&mut self, song: &AssetRef) -> Result<Option<String>, String> {
        self.inner.lyrics(song)
    }
}

/// Every app run the cut names (clips, audio clips, focus tracks,
/// voiceovers).
fn runs_of(doc: &CutDoc) -> Vec<RunRef> {
    let mut runs = Vec::new();
    for node in &doc.nodes {
        let run = match &node.kind {
            CutNodeKind::Clip { src: MediaRef::Run(run), .. } | CutNodeKind::AudioClip { src: MediaRef::Run(run), .. } | CutNodeKind::Still { src: MediaRef::Run(run), .. } => run,
            CutNodeKind::Reframe { focus: Some(MediaRef::Run(run)), .. } => run,
            CutNodeKind::VoiceOver { run: Some(run), .. } => run,
            _ => continue,
        };
        if !runs.contains(run) {
            runs.push(run.clone());
        }
    }
    runs
}

/// The marks of the cut's app runs from their `run.json` files (`marks`
/// is `name → output frame`, at `output.fps`), given as
/// `<render asset id>=<run.json>`: `name → seconds` per run.
pub fn run_marks(doc: &CutDoc, pairs: &[String]) -> Result<HashMap<RunRef, HashMap<String, f64>>, String> {
    let runs = runs_of(doc);
    let mut out = HashMap::new();
    for pair in pairs {
        let (render, file) = pair.split_once('=').ok_or_else(|| format!("--marks {pair}: expected <render asset id>=<run.json>"))?;
        let bytes = std::fs::read(file.trim()).map_err(|e| format!("{file}: {e}"))?;
        let record = makepad_strict_json::parse_depth(&bytes, 64).map_err(|e| format!("{file}: {e}"))?;
        let number = |v: Option<&makepad_strict_json::Value>| match v {
            Some(makepad_strict_json::Value::Int(i)) => Some(*i as f64),
            Some(makepad_strict_json::Value::F64(f)) => Some(*f),
            _ => None,
        };
        let fps = number(record.get("output").and_then(|o| o.get("fps")))
            .or_else(|| Some(number(record.get("frames"))? / number(record.get("duration"))?))
            .filter(|fps| *fps > 0.0)
            .ok_or_else(|| format!("{file}: no output fps"))?;
        let marks: HashMap<String, f64> = match record.get("marks") {
            Some(makepad_strict_json::Value::Obj(marks)) => marks.iter().filter_map(|(name, frame)| Some((name.clone(), number(Some(frame))? / fps))).collect(),
            _ => return Err(format!("{file}: no `marks`")),
        };
        let matching: Vec<&RunRef> = runs.iter().filter(|run| run.render_id == render.trim()).collect();
        if matching.is_empty() {
            return Err(format!("--marks: the cut plays no render `{}`", render.trim()));
        }
        for run in matching {
            out.insert(run.clone(), marks.clone());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_parse_as_seconds_minutes_or_frames() {
        assert_eq!(parse_time("2.5", 30.0).unwrap(), 2.5);
        assert_eq!(parse_time("2.5s", 30.0).unwrap(), 2.5);
        assert_eq!(parse_time("1:02.5", 30.0).unwrap(), 62.5);
        assert_eq!(parse_time("@45", 30.0).unwrap(), 1.5);
        assert!((parse_time("@1001", 29.97).unwrap() - 1001.0 * 1001.0 / 30000.0).abs() < 1e-12);
        assert!(parse_time("-1", 30.0).is_err());
        let error = parse_time("soon", 30.0).unwrap_err();
        assert!(error.contains("@45"), "{error}");
    }

    #[test]
    fn spreads_snap_to_frames_and_stay_in_the_edit() {
        assert_eq!(spread(0.0, 1.0, 3, 30.0, 4.0), vec![0.0, 0.5, 1.0]);
        // 4 s at 30 fps: the last frame is 119, at 3.9666… s.
        assert_eq!(*spread(0.0, 4.0, 2, 30.0, 4.0).last().unwrap(), 119.0 / 30.0);
        assert_eq!(spread(1.0, 1.0, 5, 30.0, 4.0), vec![1.0]);
        assert_eq!(frame_of(0.0499, 30.0, 4.0), 1);
    }

    #[test]
    fn a_motion_document_is_told_from_an_edit() {
        assert_eq!(Document::from_source("use mod.motion.*; Motion{}", "m").unwrap().kind, DocKind::Motion);
        assert_eq!(detect_kind("use mod.cut.*\nlet a = Title{text: \"Motion{\"}\nFlow{kind: @cut}"), DocKind::Cut);
        assert_eq!(detect_kind("use mod.motion.*; // Motion{ in a comment\nMotion{\n size: vec2(1920, 1080)\n}"), DocKind::Motion);
        assert_eq!(detect_kind("let m = mod.x.Motion{}\nFlow{}"), DocKind::Cut);
    }

    #[test]
    fn run_marks_come_from_run_json_in_seconds() {
        use crate::inspect::tests::{asset_id, edit};
        let run = format!("run(\"{}\", \"arev_{}\", \"{}\", \"arev_{}\")", asset_id(0x71), "71".repeat(32), asset_id(0x72), "72".repeat(32));
        let nodes = format!("let demo = Clip{{src: {run} in: marker(@saved, -0.5) span: [0.0, 2.0]}}\nlet out = CutOutput{{frame: demo.out(@frame)}}");
        let doc = edit(&nodes, "demo, out", "{width: 64 height: 64 fps: 30}", 2.0);
        let loaded = doc.load().unwrap();
        let dir = std::env::temp_dir().join(format!("edit_tools_marks_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json = dir.join("run.json");
        std::fs::write(&json, r#"{"format":"x","output":{"width":64,"height":64,"fps":60},"marks":{"saved":90,"open":6},"focus":[[[[1]]]]}"#).unwrap();
        let marks = run_marks(&loaded, &[format!("{}={}", asset_id(0x72), json.display())]).unwrap();
        let (_, named) = marks.iter().next().unwrap();
        assert_eq!(named["saved"], 1.5);
        assert_eq!(named["open"], 0.1);
        assert!(run_marks(&loaded, &[format!("{}={}", asset_id(0x73), json.display())]).is_err());
        let unresolved = crate::inspect::inspect(&doc, None, &crate::inspect::InspectOptions::default());
        assert!(unresolved.issues.iter().any(|i| i.code == "marker.unresolved"));
        let options = crate::inspect::InspectOptions { marks, ..Default::default() };
        let resolved = crate::inspect::inspect(&doc, None, &options);
        assert!(!resolved.issues.iter().any(|i| i.code == "marker.unresolved"), "{}", resolved.to_text());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

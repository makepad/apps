//! The tools on a motion document by itself, before it
//! is placed in a cut: `inspect` (M1's teaching diagnostics, evaluation
//! errors over time, missing fonts and assets, unknown or non-seekable
//! effects, layers that never show or never reach the frame, text cut by
//! the frame's edge, 9:16 safe zones) and `timeline` (inputs, layers,
//! when each is visible, scenes). Pure; with a resolver, assets are
//! resolved (on a worker).

use makepad_motion::plain::PrimitiveFrame;
use crate::input::DocKind;
use crate::inspect::{InspectOptions, InspectReport, Issue, Severity};
use crate::timeline::{Block, Scene, Timeline};
use makepad_story_edit_render::MediaResolver;
use makepad_motion::{self as motion, ItemKind, MotionDoc, MotionFrame};
use makepad_story_cut::reframe::SafeZone;
use makepad_story_cut::{AssetRef, MediaRef};
use makepad_content_packs::effects as packs;
use makepad_strict_json::Value;
use std::collections::{BTreeMap, HashMap};

/// A 3D scene's files through the inspect's resolver: library assets, and
/// files by path (a Text3D's fonts).
struct SceneFiles<'a>(std::cell::RefCell<Option<&'a mut dyn MediaResolver>>);

impl motion::scene::SceneFiles for SceneFiles<'_> {
    fn load(&self, media: &motion::scene::SceneMedia) -> Result<std::sync::Arc<Vec<u8>>, String> {
        match media {
            motion::scene::SceneMedia::Asset { id, rev } => match self.0.borrow_mut().as_mut() {
                Some(resolver) => resolver.asset_bytes(&AssetRef { id: id.clone(), rev: rev.clone() }).map(|(bytes, _)| std::sync::Arc::new(bytes)),
                None => Err(format!("asset {id} was not resolved (no media table)")),
            },
            motion::scene::SceneMedia::File(path) => std::fs::read(path).map(std::sync::Arc::new).map_err(|e| format!("{path}: {e}")),
        }
    }
}

fn from_diagnostic(diag: &motion::Diagnostic) -> Issue {
    let font = diag.message.to_ascii_lowercase().contains("font");
    let mut issue = Issue {
        severity: match diag.severity {
            motion::Severity::Error => Severity::Error,
            motion::Severity::Warning => Severity::Warning,
        },
        code: if font { "font.missing".into() } else if diag.severity == motion::Severity::Error { "motion.load".into() } else { "motion.warning".into() },
        line: diag.loc.line,
        col: diag.loc.col,
        node: (!diag.path.is_empty()).then(|| diag.path.clone()),
        at: None,
        message: diag.message.clone(),
        hint: diag.hint.clone(),
    };
    if issue.hint.is_none() && font {
        issue.hint = Some(format!("fonts are {}", makepad_doc_fonts::bundled().collect::<Vec<_>>().join(", ")));
    }
    issue
}

/// An item's box in document units, when it can be told without drawing:
/// shapes and media by their size, text by the 0.55 em estimate.
fn item_bounds(item: &motion::Item) -> Option<[f64; 4]> {
    let (size, text_origin) = match &item.kind {
        ItemKind::Primitive(PrimitiveFrame::Text { text, size, align, valign, line_height, tracking, wrap, fit, .. }) => {
            // A Text's `size` is in points: its em is 4/3 of it in document
            // units ("Beacon" at 60 measures about 290 wide).
            let em = size * 4.0 / 3.0;
            let longest = text.lines().map(|line| line.chars().count()).max().unwrap_or(0) as f64;
            let natural = longest * (em * 0.55 + tracking);
            let (width, lines) = match wrap {
                Some(wrap) if *wrap > 0.0 && natural > *wrap => (*wrap, (natural / wrap).ceil()),
                _ => (natural, text.lines().count().max(1) as f64),
            };
            // `fit` shrinks the size until the widest line fits.
            let shrink = match fit {
                Some(fit) if *fit > 0.0 && width > *fit => fit / width,
                _ => 1.0,
            };
            let width = width * shrink;
            // A line is `line_height` × the font's ascender-to-descender
            // height (about 1.21 em for the bundled faces), as the engine
            // lays it out.
            let height = lines * em * 1.21 * shrink * line_height.max(0.5);
            let x = match align {
                motion::Align::Left => 0.0,
                motion::Align::Center => -width / 2.0,
                motion::Align::Right => -width,
            };
            let y = match valign {
                motion::VAlign::Top => 0.0,
                motion::VAlign::Center => -height / 2.0,
                motion::VAlign::Bottom => -height,
            };
            ([width, height], Some([x, y]))
        }
        _ => (item.box_size()?, None),
    };
    if size[0] <= 0.0 || size[1] <= 0.0 {
        return None;
    }
    let map = item.transform(size);
    let [x0, y0] = text_origin.unwrap_or([0.0, 0.0]);
    let corners = [[x0, y0], [x0 + size[0], y0], [x0, y0 + size[1]], [x0 + size[0], y0 + size[1]]].map(|p| map.apply(p));
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in corners {
        for i in 0..2 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    Some([lo[0], lo[1], hi[0] - lo[0], hi[1] - lo[1]])
}

/// Whether an item shows something beyond the background: visible text,
/// a picture, an effect; not a fill covering most of the frame (that is a
/// background).
fn shows_content(item: &motion::Item, opacity: f64, w: f64, h: f64) -> bool {
    let opacity = opacity * item.opacity;
    if opacity <= 0.05 {
        return false;
    }
    match &item.kind {
        ItemKind::Group { children, .. } => children.iter().any(|child| shows_content(child, opacity, w, h)),
        ItemKind::Primitive(PrimitiveFrame::Text { text, reveal, units, .. }) => {
            !text.trim().is_empty() && reveal.is_none_or(|n| n > 0) && units.iter().any(|unit| unit.opacity * unit.color[3] > 0.05)
        }
        ItemKind::Primitive(PrimitiveFrame::Rect { .. }) | ItemKind::Primitive(PrimitiveFrame::Ellipse { .. }) => match item_bounds(item) {
            Some([x, y, bw, bh]) => {
                let visible_w = (x + bw).min(w) - x.max(0.0);
                let visible_h = (y + bh).min(h) - y.max(0.0);
                visible_w > 0.0 && visible_h > 0.0 && visible_w * visible_h < 0.9 * w * h
            }
            None => false,
        },
        _ => true,
    }
}

#[derive(Default)]
struct LayerSeen {
    loc: (u32, u32),
    kind: &'static str,
    /// Sampled times the layer drew at (with opacity).
    visible: Vec<f64>,
    offscreen: usize,
    cut: Vec<f64>,
    unsafe_band: Vec<f64>,
    text: bool,
    /// The last sample it was visible at: its box and whether it was cut by
    /// the frame or ran outside the side margins there.
    last: Option<(f64, bool, bool)>,
    /// Consecutive samples at full opacity with nothing changing: the
    /// longest such run, and the current one (start, previous item).
    hold_best: f64,
    hold_run: Option<(f64, motion::Item)>,
}

fn register_inspect_fonts() {
    // The CLI does not start the widget VM. The in-app host may already
    // have registered a packaged resource directory; keep that one.
    if makepad_doc_fonts::root_of("makepad_widgets").is_none() {
        makepad_doc_fonts::register_root("makepad_widgets", makepad_widgets::widgets_dir());
    }
}

/// Check a motion document.
pub fn inspect_motion(source: &str, name: &str, resolver: Option<&mut dyn MediaResolver>, options: &InspectOptions) -> InspectReport {
    register_inspect_fonts();
    let mut report = InspectReport { file: name.to_string(), kind: DocKind::Motion, summary: None, issues: Vec::new() };
    let doc = match MotionDoc::load(source) {
        Ok(doc) => doc,
        Err(diags) => {
            report.issues = diags.iter().map(from_diagnostic).collect();
            return report;
        }
    };
    report.summary = Some((doc.label.clone(), (doc.size[0].round() as u32, doc.size[1].round() as u32), doc.fps, doc.duration, doc.all_layers().len()));
    let mut issues: Vec<Issue> = doc.warnings.iter().map(from_diagnostic).collect();
    let layers: HashMap<String, (u32, u32, &'static str)> = doc.all_layers().iter().map(|l| (l.path.clone(), (l.loc.line, l.loc.col, l.kind.type_name()))).collect();
    let place = |mut issue: Issue, path: &str| {
        if let Some((line, col, _)) = layers.get(path).or_else(|| path.split_once('#').and_then(|(base, _)| layers.get(base))) {
            issue.line = *line;
            issue.col = *col;
        }
        issue.node = Some(path.to_string());
        issue
    };

    // Library media, and effects.
    let unresolved = resolver.is_none() && !doc.dependencies().is_empty();
    let files = SceneFiles(std::cell::RefCell::new(resolver));
    match files.0.borrow_mut().as_mut() {
        Some(resolver) => {
            // Library effects are read (and checked) below as sources.
            let effects: Vec<motion::AssetRef> = doc.all_layers().iter().filter_map(|layer| match &layer.kind {
                motion::LayerKind::Shader { program: motion::ShaderProgram::Effect(asset), .. } => Some(asset.clone()),
                _ => None,
            }).collect();
            for asset in doc.dependencies().into_iter().filter(|asset| !effects.contains(asset)) {
                let asset_ref = AssetRef { id: asset.id.clone(), rev: asset.rev.clone() };
                // What the renderer reads a document's files with (images,
                // SVGs, a full Scene3D's GLBs, HDRs and fonts); videos
                // resolve as media.
                if resolver.asset_bytes(&asset_ref).is_ok() {
                    continue;
                }
                if let Err(error) = resolver.resolve(&MediaRef::Asset(asset_ref)) {
                    issues.push(
                        Issue::new(Severity::Error, "media.missing", format!("asset(\"{}\", \"{}\") cannot be resolved: {error}", asset.id, asset.rev))
                            .hint(format!("add `{}=<file>` to the media table (media.txt or --media)", asset.id)),
                    );
                }
            }
            for layer in doc.all_layers() {
                if let motion::LayerKind::Shader { program: motion::ShaderProgram::Effect(asset), .. } = &layer.kind {
                    match resolver.effect_source(&AssetRef { id: asset.id.clone(), rev: asset.rev.clone() }) {
                        Ok(source) => {
                            if let Some(why) = crate::inspect::effect_refusal(&source, false) {
                                issues.push(place(Issue::new(Severity::Error, "effect.not_seekable", format!("the library effect {}: {why}", asset.id)), &layer.path));
                            }
                        }
                        Err(error) => issues.push(place(Issue::new(Severity::Error, "media.missing", format!("the library effect {} cannot be read: {error}", asset.id)), &layer.path)),
                    }
                }
            }
        }
        None if !doc.dependencies().is_empty() => issues.push(
            Issue::new(Severity::Info, "media.unchecked", "the document's assets were not resolved (no media table)")
                .hint("pass `--media <asset id>=<file>` for each asset, or a media.txt beside the document"),
        ),
        None => {}
    }
    for layer in doc.all_layers() {
        let motion::LayerKind::Shader { program: motion::ShaderProgram::Preset(name), text, .. } = &layer.kind else { continue };
        let bare = name.strip_prefix("vjfx/").unwrap_or(name);
        match packs::resolve(bare) {
            // A KineticText draws on a kinetic text pack's kit.
            None if text.is_some() => {
                let text_packs = makepad_content_packs::text_packs();
                if !text_packs.iter().any(|pack| pack.has_kit(name)) {
                    issues.push(place(
                        Issue::new(Severity::Error, "effect.unknown", format!("there is no kinetic type kit `{bare}`")).hint(if text_packs.is_empty() {
                            makepad_content_packs::text::missing(bare)
                        } else {
                            "`makepad-motion kinetic` lists the kits".to_string()
                        }),
                        &layer.path,
                    ));
                }
            }
            None => issues.push(place(
                Issue::new(Severity::Error, "effect.unknown", format!("there is no bundled effect `{name}`")).hint(if makepad_content_packs::effect_packs().is_empty() {
                    packs::missing("a vjfx/ preset")
                } else {
                    "name a bundled preset as \"vjfx/<name>\", or write the look as `pixel: fn() {…}`".to_string()
                }),
                &layer.path,
            )),
            Some((pack, _, source)) => {
                // A KineticText's own words replace the preset's source.
                if let Some(why) = pack.seek_refusal(source, text.is_some()) {
                    issues.push(place(
                        Issue::new(Severity::Error, "effect.not_seekable", format!("`{name}`: {why}; a motion document is drawn as a function of time, so it refuses this effect"))
                            .hint("pick an effect whose look is a function of time alone, or an inline `pixel: fn() {…}` shader"),
                        &layer.path,
                    ));
                }
            }
        }
    }

    // Over time: evaluation errors, visibility, the frame's edges.
    let (w, h) = (doc.size[0], doc.size[1]);
    let portrait = h > w * 1.25;
    let safe = options.safe.unwrap_or(SafeZone::Reels);
    let bands = safe.bands();
    let mut seen: BTreeMap<String, LayerSeen> = BTreeMap::new();
    for layer in doc.all_layers() {
        let entry = seen.entry(layer.path.clone()).or_default();
        entry.loc = (layer.loc.line, layer.loc.col);
        entry.kind = layer.kind.type_name();
        entry.text = matches!(layer.kind, motion::LayerKind::Text { .. });
    }
    let mut eval_errors: BTreeMap<(String, String), (f64, f64, motion::Diagnostic)> = BTreeMap::new();
    let times = motion::inspect::sample_times(&doc, options.sample_fps);
    // The 3D scenes as motion3d reads them (a field a scene type does not
    // read, a wrong value, a file that does not load), once each. Without
    // a media table a document's assets are not resolved (said above).
    if !unresolved {
        for diagnostic in motion::scene::scene_diagnostics(&doc, &times, &files) {
            let mut issue = from_diagnostic(&diagnostic);
            issue.code = if diagnostic.is_error() { "scene3d.load".into() } else { "scene3d.warning".into() };
            issues.push(issue);
        }
    }
    // Whether each sample shows anything (head, tail and gaps of nothing).
    let mut shows: Vec<(f64, bool)> = Vec::new();
    for &t in &times {
        let frame: MotionFrame = doc.eval(t);
        shows.push((t, frame.items.iter().any(|item| shows_content(item, 1.0, w, h))));
        for error in &frame.errors {
            let entry = eval_errors.entry((error.path.clone(), error.message.clone())).or_insert((t, t, error.clone()));
            entry.1 = t;
        }
        for (_, item) in frame.walk() {
            if item.opacity <= 0.01 {
                continue;
            }
            let Some(entry) = seen.get_mut(&item.path) else { continue };
            entry.visible.push(t);
            let Some([x, y, bw, bh]) = item_bounds(item) else { continue };
            if x >= w || y >= h || x + bw <= 0.0 || y + bh <= 0.0 {
                entry.offscreen += 1;
                continue;
            }
            if entry.text {
                let slack = 0.01 * w;
                let cut = x < -slack || x + bw > w + slack || y + bh > h + slack || y < -slack;
                if cut {
                    entry.cut.push(t);
                }
                let margin = (1.0 - motion::SAFE_WIDTH) / 2.0 * w;
                let outside = x < margin - slack || x + bw > w - margin + slack;
                entry.last = Some((t, cut, outside));
                // Holding: fully visible and the same as the sample before.
                if item.opacity >= 0.95 {
                    match &entry.hold_run {
                        Some((start, previous)) if previous == item => {
                            entry.hold_best = entry.hold_best.max(t - start);
                        }
                        _ => entry.hold_run = Some((t, item.clone())),
                    }
                } else {
                    entry.hold_run = None;
                }
                if portrait && safe != SafeZone::None && (y < bands.top * h || y + bh > (1.0 - bands.bottom) * h || x < bands.left * w || x + bw > (1.0 - bands.right) * w) {
                    entry.unsafe_band.push(t);
                }
            }
        }
    }
    // Resolved glyphs and paint geometry, through the same issue surface.
    for layout in motion::inspect::layout_checks(&doc, &times, options.min_text_px) {
        let severity = match layout.severity {
            motion::Severity::Error => Severity::Error,
            motion::Severity::Warning => Severity::Warning,
        };
        let issue = Issue::new(severity, layout.code, layout.message).hint(layout.hint).at(layout.from, layout.to);
        issues.push(place(issue, &layout.path));
    }
    // Charts' own label checks (DV1): overlaps, off-frame, outside the margins.
    for chart in motion::chart_checks(&doc, &times) {
        let severity = match chart.severity {
            motion::ChartSeverity::Error => Severity::Error,
            motion::ChartSeverity::Warning => Severity::Warning,
        };
        let mut issue = Issue::new(severity, chart.code, chart.message.clone()).hint(chart.hint.clone()).at(chart.from, chart.to);
        issue = place(issue, &chart.path);
        issues.push(issue);
    }
    // Nothing on screen: at the head, the tail, or for a stretch between.
    let step = shows.windows(2).map(|pair| pair[1].0 - pair[0].0).fold(0.0, f64::max).max(1.0 / doc.fps.max(1.0));
    let first = shows.iter().find(|(_, shows)| *shows).map(|(t, _)| *t);
    let last = shows.iter().rev().find(|(_, shows)| *shows).map(|(t, _)| *t);
    match (first, last) {
        (None, _) | (_, None) => issues.push(Issue::new(Severity::Warning, "frame.empty", "no frame shows anything but the background")
            .hint("check the layers' spans and opacities against the duration")),
        (Some(first), Some(last)) => {
            if first > 0.15 {
                issues.push(Issue::new(Severity::Warning, "frame.empty_head", format!("the first {first:.2} s show nothing but the background"))
                    .at(0.0, first)
                    .hint("start on the hook: have the first element on screen (or entering) at frame 0"));
            }
            let end = doc.duration - step;
            if last < end - 0.15 {
                issues.push(Issue::new(Severity::Warning, "frame.empty_tail", format!("the last {:.2} s show nothing but the background", doc.duration - last - step))
                    .at(last + step, doc.duration)
                    .hint("hold the final frame (the end card, the last line) to the end, or shorten `duration`"));
            }
            let mut gap_start: Option<f64> = None;
            for &(t, shows) in shows.iter().filter(|(t, _)| *t > first && *t < last) {
                match (shows, gap_start) {
                    (false, None) => gap_start = Some(t),
                    (true, Some(start)) => {
                        if t - start >= 0.75 {
                            issues.push(Issue::new(Severity::Warning, "frame.empty_gap", format!("{:.2} s show nothing but the background", t - start))
                                .at(start, t)
                                .hint("overlap the scenes (the next enters as the last leaves), or hold the last one until the next arrives"));
                        }
                        gap_start = None;
                    }
                    _ => {}
                }
            }
        }
    }
    for ((path, _), (t0, t1, diag)) in eval_errors {
        let mut issue = from_diagnostic(&diag);
        issue.code = "motion.eval".into();
        issue.at = Some((t0, t1));
        if issue.line == 0 {
            issue = place(issue, &path);
        }
        issues.push(issue);
    }
    for (path, layer) in &seen {
        let at = |issue: Issue| {
            let mut issue = issue;
            issue.line = layer.loc.0;
            issue.col = layer.loc.1;
            issue.node = Some(path.clone());
            issue
        };
        if path.ends_with(".mask") {
            continue;
        }
        if layer.visible.is_empty() {
            issues.push(at(Issue::new(Severity::Warning, "layer.never_visible", format!("the {} is never visible: its span, or its opacity, keeps it hidden for the whole document", layer.kind))
                .hint("check `span: [start, length]` against the duration, and the opacity's keys")));
            continue;
        }
        let (first, last) = (layer.visible[0], *layer.visible.last().unwrap());
        if layer.offscreen == layer.visible.len() {
            issues.push(at(Issue::new(Severity::Warning, "layer.outside_frame", format!("the {} never reaches the frame ({}x{})", layer.kind, w, h))
                .at(first, last)
                .hint("`at` is the top-left of the layer's box in document units, from (0, 0) to the document's size")));
        }
        if layer.cut.len() * 2 > layer.visible.len() {
            issues.push(at(Issue::new(Severity::Warning, "text.overflow", "the text runs past the frame's edge for most of its time")
                .at(layer.cut[0], *layer.cut.last().unwrap())
                .hint("lower `size`, set `wrap: <width>`, shorten the text or move `at` inwards (text is estimated at 0.55 em per character)")));
        }
        if let Some((t, cut, outside)) = layer.last {
            if cut {
                issues.push(at(Issue::new(Severity::Warning, "text.final_clipped", "the text's final state runs past the frame's edge")
                    .at(t, t)
                    .hint("set `fit: @safe` (the size shrinks until every line fits the side margins) or `wrap: @safe`")));
            } else if outside {
                issues.push(at(Issue::new(Severity::Warning, "text.final_outside_safe", format!("the text's final state runs into the side margins (keep lines inside the middle {:.0}% of the width)", motion::SAFE_WIDTH * 100.0))
                    .at(t, t)
                    .hint("set `fit: @safe` or `wrap: @safe` with the text centred (`at` x at the middle, `align: @center`)")));
            }
        }
        if layer.text && layer.visible.len() > 1 && layer.hold_best < 1.0 && doc.duration >= 2.0 {
            issues.push(at(Issue::new(Severity::Info, "text.no_hold", format!("the text never holds still and fully visible for 1 s (at most {:.2} s)", layer.hold_best))
                .at(first, last)
                .hint("let the line land and hold at least 1–1.5 s before it leaves, so it can be read")));
        }
        if layer.unsafe_band.len() * 2 > layer.visible.len() {
            issues.push(at(Issue::new(Severity::Warning, "safe_zone", format!("the text sits in a band @{} covers with its own buttons and caption", safe.id()))
                .at(layer.unsafe_band[0], *layer.unsafe_band.last().unwrap())
                .hint(format!("keep text between {:.0} and {:.0} from the top and {:.0} from each side", bands.top * h, (1.0 - bands.bottom) * h, bands.left.max(bands.right) * w))));
        }
    }
    issues.sort_by(|a, b| (a.severity, a.line, a.col).cmp(&(b.severity, b.line, b.col)));
    issues.dedup();
    report.issues = issues;
    report
}

/// A layer drawn in a frame, for `frame --names`: its box in the
/// document's pixels and the label written on it.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameLayer {
    pub path: String,
    pub name: Option<String>,
    pub kind: &'static str,
    /// Its name, else its type and path (`Rect layers[3]`).
    pub label: String,
    /// `[x0, y0, x1, y1]`.
    pub bounds: [f64; 4],
    pub group: bool,
}

/// The layers a motion document shows at `t` with their boxes: every
/// drawn layer, and the groups that have a name. `None` when `source` is
/// not a motion document that loads.
pub fn frame_layers(source: &str, t: f64) -> Option<Vec<FrameLayer>> {
    register_inspect_fonts();
    let doc = MotionDoc::load(source).ok()?;
    let frame = doc.eval(t);
    Some(motion::inspect::layer_boxes(&doc, &frame).into_iter().filter(|b| !b.group || b.name.is_some()).map(|b| FrameLayer {
        label: match &b.name {
            Some(name) => name.clone(),
            None => format!("{} {}", b.kind, b.path),
        },
        path: b.path,
        name: b.name,
        kind: b.kind,
        bounds: b.bounds,
        group: b.group,
    }).collect())
}

fn input_text(value: &motion::InputValue) -> String {
    match value {
        motion::InputValue::Text(text) => format!("\"{text}\""),
        motion::InputValue::Number(n) => format!("{n}"),
        motion::InputValue::Bool(b) => format!("{b}"),
        motion::InputValue::Vec2([x, y]) => format!("vec2({x}, {y})"),
        motion::InputValue::Asset(asset) => format!("asset({}, {})", asset.id, asset.rev),
        motion::InputValue::Markup(_) => "<svg…>".into(),
        motion::InputValue::Empty => "(empty)".into(),
        other => format!("{other:?}"),
    }
}

fn time_pairs(pairs: &[(f64, f64)]) -> Value {
    Value::Arr(pairs.iter().map(|&(a, b)| Value::Arr(vec![crate::inspect::num(a), crate::inspect::num(b)])).collect())
}

/// A motion document as a timeline: its inputs, every layer with the time
/// it is visible (sampled on its frame grid), and the scenes those times cut.
pub fn timeline_motion(source: &str) -> Result<Timeline, Vec<motion::Diagnostic>> {
    register_inspect_fonts();
    let doc = MotionDoc::load(source)?;
    let times = motion::inspect::sample_times(&doc, doc.fps);
    let visible: HashMap<_, _> = motion::inspect::visibility(&doc, &times).into_iter().map(|v| (v.path.clone(), v)).collect();
    let mut blocks = Vec::new();
    for layer in doc.all_layers() {
        let depth = layer.path.matches('.').count() as u32;
        let visibility = &visible[&layer.path];
        let start = visibility.first.unwrap_or(0.0);
        let end = visibility.spans.last().map_or(0.0, |s| s.1);
        let detail = format!(
            "{}{}, {}",
            layer.name.as_ref().map(|n| format!("\"{n}\" ")).unwrap_or_default(),
            if layer.animated { "animated" } else { "static" },
            visibility.to_text()
        );
        blocks.push(Block {
            node: layer.path.clone(),
            kind: layer.kind.type_name(),
            lane: "layers",
            track: depth,
            start,
            end: end.min(doc.duration),
            detail,
            fields: vec![
                ("animated", Value::Bool(layer.animated)), ("name", layer.name.clone().map(Value::Str).unwrap_or(Value::Null)),
                ("first_visible", visibility.first.map(crate::inspect::num).unwrap_or(Value::Null)),
                ("last_visible", visibility.last.map(crate::inspect::num).unwrap_or(Value::Null)),
                ("visible_spans", time_pairs(&visibility.spans)), ("gaps", time_pairs(&visibility.gaps)),
                ("visibility_unchecked", Value::Bool(visibility.unchecked)),
            ],
        });
    }
    let mut cuts = vec![0.0, doc.duration];
    for block in &blocks {
        for &(start, end) in &visible[&block.node].spans {
            cuts.push(start);
            cuts.push(end);
        }
    }
    cuts.sort_by(f64::total_cmp);
    cuts.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    let scenes = cuts
        .windows(2)
        .filter(|pair| pair[1] > pair[0])
        .map(|pair| {
            let mid = (pair[0] + pair[1]) / 2.0;
            Scene { start: pair[0], end: pair[1], nodes: blocks.iter().filter(|b| b.track == 0 && visible[&b.node].spans.iter().any(|&(start, end)| start <= mid && mid < end)).map(|b| b.node.clone()).collect() }
        })
        .collect();
    let inputs = doc.inputs.iter().zip(&doc.input_values).map(|(decl, value)| (decl.name.clone(), input_text(value))).collect();
    Ok(Timeline {
        label: doc.label.clone(),
        size: (doc.size[0].round() as u32, doc.size[1].round() as u32),
        fps: doc.fps,
        frames: (doc.duration * doc.fps - 1e-9).ceil().max(1.0) as u64,
        duration: doc.duration,
        bpm: doc.bpm,
        scenes,
        blocks,
        wiring: Vec::new(),
        markers: Vec::new(),
        dependencies: doc.dependencies().into_iter().map(|a| format!("asset {} {}", a.id, a.rev)).collect(),
        inputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"use mod.motion.*; Motion{
    label: "t"
    size: vec2(1080, 1920) duration: 2.0 fps: 30
    inputs: {headline: "Hello"}
    layers: [
        Rect{at: vec2(0, 0) size: vec2(1080, 1920) fill: #0b0d12}
        Text{text: in(@headline) font: Font{family: "Inter"} size: 120 at: vec2(540, 900) align: @center opacity: tween(0, 1, 0.0, 0.4, @ease_out)}
        Text{text: "This caption line is far too long for a phone screen" font: Font{family: "Inter"} size: 90 at: vec2(40, 1800)}
        Rect{at: vec2(3000, 100) size: vec2(100, 100) fill: #ffffff}
        Rect{at: vec2(0, 0) size: vec2(10, 10) fill: #ffffff span: [5.0, 1.0]}
    ]
}"#;

    #[test]
    fn a_motion_document_is_inspected_over_time() {
        let report = inspect_motion(DOC, "t.motion.splash", None, &InspectOptions::default());
        let codes: Vec<(&str, Option<&str>)> = report.issues.iter().map(|i| (i.code.as_str(), i.node.as_deref())).collect();
        assert!(report.ok(), "{}", report.to_text());
        for want in [("text.overflow", Some("layers[2]")), ("safe_zone", Some("layers[2]")), ("layer.outside_frame", Some("layers[3]"))] {
            assert!(codes.contains(&want), "{want:?} missing: {}", report.to_text());
        }
        assert!(codes.iter().any(|(code, node)| (*code == "layer.never_visible" || code.starts_with("motion.")) && *node == Some("layers[4]")), "{}", report.to_text());
        // The headline is fine.
        assert!(!codes.iter().any(|(_, node)| *node == Some("layers[1]")), "{}", report.to_text());
    }

    #[test]
    fn a_full_scenes_model_resolves_through_the_media_table() {
        let dir = std::env::temp_dir().join(format!("edit_tools_motion_glb_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let glb = dir.join("car.glb");
        std::fs::write(&glb, b"glTF").unwrap();
        let doc = "use mod.motion.*; use mod.scene; Motion{size: vec2(640, 360) duration: 2.0 layers: [Scene3D{camera: Camera{position: vec3(0, 1, 6)} children: [scene.Model{src: asset(\"lib-8962\", \"1\")}]}]}";
        let mut media = makepad_story_edit_render::LocalMedia::new();
        media.insert("lib-8962", &glb);
        let report = inspect_motion(doc, "scene.splash", Some(&mut media), &InspectOptions::default());
        assert!(!report.issues.iter().any(|i| i.code == "media.missing"), "{}", report.to_text());
        // A file the table lacks is still reported.
        let mut empty = makepad_story_edit_render::LocalMedia::new();
        let report = inspect_motion(doc, "scene.splash", Some(&mut empty), &InspectOptions::default());
        assert!(report.issues.iter().any(|i| i.code == "media.missing"), "{}", report.to_text());
        let _ = std::fs::remove_file(&glb);
    }

    /// The general check reads each 3D scene as the renderer does: a field
    /// a scene type does not read is a warning at its line, with our name
    /// for another tool's word.
    #[test]
    fn a_scenes_unread_fields_are_reported_at_their_lines() {
        let doc = "use mod.motion.*; use mod.scene; Motion{size: vec2(640, 360) duration: 2.0 layers: [\n\
                   Scene3D{camera: Camera{position: vec3(0, 0, 6)} lights: [scene.PointLight{power: 3}]\n\
                   children: [scene.Mesh{geometry: scene.BoxGeometry{} colour: #ff0000}]}]}";
        let report = inspect_motion(doc, "scene.splash", None, &InspectOptions::default());
        let warned = |text: &str| report.issues.iter().find(|i| i.code == "scene3d.warning" && i.message.contains(text)).unwrap_or_else(|| panic!("no {text:?}: {}", report.to_text())).clone();
        let power = warned("PointLight has no parameter `power` (ignored); write `intensity` here: a number");
        assert_eq!(power.line, 2, "{}", report.to_text());
        assert_eq!(warned("Mesh has no parameter `colour` (ignored)").line, 3, "{}", report.to_text());
        assert!(report.ok(), "warnings only: {}", report.to_text());
    }

    /// With no text pack installed, a KineticText's kit is reported as
    /// missing, and a VJ preset as needing the VJ effect pack.
    #[test]
    fn kinetic_text_and_a_vj_preset_without_their_packs_are_reported() {
        let kinetic = "use mod.motion.*; Motion{size: vec2(640, 360) duration: 2.0 layers: [KineticText{preset: \"340_clock_flip\" text: \"10:59\"}]}";
        let report = inspect_motion(kinetic, "clock.splash", None, &InspectOptions::default());
        let issue = report.issues.iter().find(|i| i.code == "effect.unknown").unwrap_or_else(|| panic!("{}", report.to_text()));
        assert!(issue.message.contains("340_clock_flip") && issue.hint.as_deref().is_some_and(|hint| hint.contains("needs the kinetic text pack")), "{issue:?}");
        let vj = "use mod.motion.*; Motion{size: vec2(640, 360) duration: 2.0 layers: [Shader{effect: \"vjfx/01_fireworks\"}]}";
        let report = inspect_motion(vj, "fw.splash", None, &InspectOptions::default());
        let issue = report.issues.iter().find(|i| i.code == "effect.unknown").unwrap_or_else(|| panic!("{}", report.to_text()));
        assert!(issue.hint.as_deref().is_some_and(|hint| hint.contains("needs the VJ effect pack")), "{issue:?}");
    }

    fn codes(source: &str) -> Vec<(String, Option<String>)> {
        let report = inspect_motion(source, "t.splash", None, &InspectOptions::default());
        report.issues.iter().map(|i| (i.code.clone(), i.node.clone())).collect()
    }

    #[test]
    fn empty_heads_tails_and_gaps_are_reported() {
        let doc = r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 8.0 fps: 30 background: #000000 layers: [
            Rect{at: vec2(0, 0) size: vec2(1920, 1080) fill: #101010}
            Text{text: "One" size: 120 at: vec2(960, 500) align: @center span: [1.0, 2.0]}
            Text{text: "Two" size: 120 at: vec2(960, 500) align: @center span: [4.5, 2.0]}
        ]}"#;
        let found = codes(doc);
        for want in ["frame.empty_head", "frame.empty_gap", "frame.empty_tail"] {
            assert!(found.iter().any(|(c, _)| c == want), "{want}: {found:?}");
        }
        // Back to back from frame 0 to the end: none of them.
        let full = r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 fps: 30 layers: [
            Text{text: "One" size: 120 at: vec2(960, 500) align: @center span: [0.0, 2.0]}
            Text{text: "Two" size: 120 at: vec2(960, 500) align: @center span: [2.0, 2.0]}
        ]}"#;
        assert!(!codes(full).iter().any(|(c, _)| c.starts_with("frame.")), "{:?}", codes(full));
    }

    #[test]
    fn a_final_state_clipped_or_outside_the_margins_is_reported_and_fit_fixes_it() {
        let wide = r#"use mod.motion.*; Motion{size: vec2(1080, 1920) duration: 3.0 fps: 30 layers: [
            Text{text: "This headline is far too wide to fit" size: 140 at: vec2(540, 900) align: @center}
        ]}"#;
        assert!(codes(wide).iter().any(|(c, n)| c == "text.final_clipped" && n.as_deref() == Some("layers[0]")), "{:?}", codes(wide));
        let margin = r#"use mod.motion.*; Motion{size: vec2(1080, 1920) duration: 3.0 fps: 30 layers: [
            Text{text: "Nearly at the edge" size: 70 at: vec2(20, 900)}
        ]}"#;
        assert!(codes(margin).iter().any(|(c, _)| c == "text.final_outside_safe"), "{:?}", codes(margin));
        let fitted = r#"use mod.motion.*; Motion{size: vec2(1080, 1920) duration: 3.0 fps: 30 layers: [
            Text{text: "This headline is far too wide to fit" size: 140 at: vec2(540, 900) align: @center fit: @safe}
        ]}"#;
        assert!(!codes(fitted).iter().any(|(c, _)| c.starts_with("text.final") || c == "text.overflow"), "{:?}", codes(fitted));
    }

    #[test]
    fn text_that_never_holds_is_noted() {
        let moving = r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 fps: 30 layers: [
            Text{text: "Always moving" size: 100 at: vec2(960, 500) align: @center x: tween(-200, 200, 0, 4)}
        ]}"#;
        assert!(codes(moving).iter().any(|(c, _)| c == "text.no_hold"), "{:?}", codes(moving));
        let lands = r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 fps: 30 layers: [
            Text{text: "Lands and holds" size: 100 at: vec2(960, 500) align: @center x: tween(-200, 0, 0, 1.0, @ease_out)}
        ]}"#;
        assert!(!codes(lands).iter().any(|(c, _)| c == "text.no_hold"), "{:?}", codes(lands));
    }

    #[test]
    fn motion_load_errors_keep_their_place_and_hint() {
        let report = inspect_motion("use mod.motion.*; Motion{\n  size: vec2(100, 100) duration: 1.0\n  layers: [Rect{sise: vec2(1, 1)}]\n}", "bad.splash", None, &InspectOptions::default());
        assert!(!report.ok());
        let issue = &report.issues[0];
        assert_eq!(issue.line, 3, "{}", report.to_text());
    }

    #[test]
    fn a_motion_timeline_lists_inputs_and_layers() {
        let timeline = timeline_motion(DOC).unwrap();
        assert_eq!(timeline.frames, 60);
        assert_eq!(timeline.inputs, vec![("headline".to_string(), "\"Hello\"".to_string())]);
        let text = timeline.to_text();
        assert!(text.contains("layers[1]") && text.contains("Text"), "{text}");
        assert!(text.contains("never visible"), "{text}");
    }

    #[test]
    fn layout_checks_use_the_existing_inspect_report_and_options() {
        let source = include_str!("../../../../../apps/engine/motion/fixtures/inspect/strokes.splash");
        let report = inspect_motion(source, "strokes.splash", None, &InspectOptions::default());
        assert!(report.ok(), "{}", report.to_text());
        let issue = report.issues.iter().find(|i| i.code == "text.line_through").unwrap();
        assert_eq!(issue.node.as_deref(), Some("layers[0]"));
        assert!(issue.line > 0 && issue.col > 0 && issue.at.is_some() && issue.hint.is_some());
        assert!(report.to_text().contains("warning[text.line_through]"));
        assert!(report.to_json().to_json().contains("text.line_through"));
        let source = include_str!("../../../../../apps/engine/motion/fixtures/inspect/sizes.splash");
        let report = inspect_motion(source, "sizes.splash", None, &InspectOptions { min_text_px: 0.0, ..Default::default() });
        assert!(!report.issues.iter().any(|i| i.code == "text.too_small"));
    }

    /// `inspect` reports hard pops with the time, the layer, its place in
    /// the document and the fix, as text and as JSON.
    #[test]
    fn inspect_reports_pops_with_their_time_layer_and_fix() {
        let source = include_str!("../../../../../apps/engine/motion/fixtures/inspect/pops.splash");
        let report = inspect_motion(source, "pops.splash", None, &InspectOptions::default());
        let pops: Vec<(&str, Option<&str>, Option<(f64, f64)>)> = report.issues.iter().filter(|i| i.code.starts_with("layer.pop")).map(|i| (i.code.as_str(), i.node.as_deref(), i.at)).collect();
        assert_eq!(pops.len(), 4, "{pops:?}");
        assert!(pops.contains(&("layer.pop_in", Some("layers[0]"), Some((1.0, 1.0)))), "{pops:?}");
        assert!(pops.contains(&("layer.pop_out", Some("layers[4]"), Some((3.0, 3.0)))), "{pops:?}");
        let first = report.issues.iter().find(|i| i.code == "layer.pop_in").unwrap();
        assert_eq!((first.line, first.severity), (4, Severity::Warning));
        let json = report.to_json().to_json();
        assert!(json.contains("\"layer.pop_in\"") && json.contains("hard_cut: true"), "{json}");
        assert!(report.to_text().contains("warning[layer.pop_out]"), "{}", report.to_text());
    }

    /// `frame --names`: each drawn layer's box and label at the time.
    #[test]
    fn frame_layers_are_the_drawn_layers_with_their_boxes() {
        let source = include_str!("../../../../../apps/engine/motion/fixtures/inspect/pops.splash");
        let layers = frame_layers(source, 2.5).unwrap();
        let labels: Vec<&str> = layers.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels, ["always", "card back", "card title", "card", "slide", "grow"]);
        let back = &layers[1];
        assert_eq!(back.bounds, [100.0, 500.0, 700.0, 800.0]);
        assert!(layers[3].group && !back.group);
        // Before the card and the boxes appear: fewer layers.
        assert_eq!(frame_layers(source, 0.0).unwrap().iter().map(|l| l.label.as_str()).collect::<Vec<_>>(), ["always"]);
        assert!(frame_layers("not a document", 0.0).is_none());
    }

    #[test]
    fn timeline_text_and_scenes_preserve_visibility_gaps() {
        let source = include_str!("../../../../../apps/engine/motion/fixtures/inspect/timeline.splash");
        let timeline = timeline_motion(source).unwrap();
        let text = timeline.to_text();
        assert!(text.contains("first 0.000 s, last 2.950 s; gaps: 1.000–2.000 s"), "{text}");
        for scene in &timeline.scenes {
            if scene.start >= 1.0 && scene.end <= 2.0 {
                assert!(!scene.nodes.iter().any(|n| n == "layers[0]"), "{scene:?}");
            }
        }
        let json = timeline.to_json().to_json();
        assert!(json.contains("visible_spans") && json.contains("first_visible") && json.contains("gaps"), "{json}");
    }
}

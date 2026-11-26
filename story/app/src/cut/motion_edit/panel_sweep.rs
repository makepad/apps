//! A made panel measured on the picture: each number control (and each
//! macro, all its targets at once) set to the ends of its range, the
//! graphic drawn at a moment it acts (by the edit's own renderer, read
//! back small), and the pixels compared with the picture as written. A
//! control whose whole range changes almost nothing is a defect the
//! self-check sends back to the scene's agent (widen it, bind something
//! stronger, or leave it out), so a panel ships controls that visibly
//! matter.
//!
//! The renders run one at a time in the background (`PanelSweep`, a
//! zero-size widget in the panel's column, drives them from its draw),
//! each the graphic alone (`clip_films::mini_edit`) with the desk's texts
//! (the edited value spliced in) served in place of the library's.
//!
//! Main-loop cost: nothing until a panel is made; then one small frame
//! render at a time until the sweep ends (bounded by `MAX_CONTROLS`).

use makepad_edit_values::ValueKind;
use super::super::clip_films::{self, SharedResolver};
use super::super::VideoCut;
use makepad_motion::paths;
use makepad_story_edit_render::engine::EngineOptions;
use makepad_story_edit_render::{CutJob, CutThumbnail, MediaResolver};
use makepad_story_cut::CutDoc;
use makepad_widgets::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// Controls measured at most (the rest are not).
const MAX_CONTROLS: usize = 96;
/// The renders' width (pixels).
const WIDTH: u32 = 240;
/// A pixel counts as changed when a channel moved by more than this.
const PIXEL_STEP: u8 = 12;
/// Below this share of pixels changed over its whole range, a control
/// does nothing visible.
pub const INVISIBLE: f64 = 0.004;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.PanelSweepBase = #(PanelSweep::register_widget(vm))
    mod.widgets.PanelSweep = set_type_default() do mod.widgets.PanelSweepBase{width: 0 height: 0}
}

/// The desk's texts served in place of the library's revisions: the
/// graphic's and its modules' (by asset id).
pub(super) struct Served {
    pub(super) inner: SharedResolver,
    pub(super) texts: Arc<HashMap<String, String>>,
}

impl MediaResolver for Served {
    fn resolve(&mut self, media: &makepad_story_cut::MediaRef) -> Result<makepad_story_edit_render::ResolvedMedia, String> {
        self.inner.resolve(media)
    }
    fn effect_source(&mut self, effect: &makepad_story_cut::AssetRef) -> Result<String, String> {
        self.inner.effect_source(effect)
    }
    fn cut_source(&mut self, edit: &makepad_story_cut::AssetRef) -> Result<String, String> {
        self.inner.cut_source(edit)
    }
    fn motion_source(&mut self, motion: &makepad_story_cut::AssetRef) -> Result<String, String> {
        match self.texts.get(&motion.id) {
            Some(text) => Ok(text.clone()),
            None => self.inner.motion_source(motion),
        }
    }
    fn asset_bytes(&mut self, asset: &makepad_story_cut::AssetRef) -> Result<(Vec<u8>, &'static str), String> {
        self.inner.asset_bytes(asset)
    }
    fn take_info(&mut self, take: &makepad_story_cut::AssetRef) -> Result<Option<makepad_story_cut::TakeInfo>, String> {
        self.inner.take_info(take)
    }
    fn run_data(&mut self, run: &makepad_story_cut::RunRef) -> Result<Option<Vec<u8>>, String> {
        self.inner.run_data(run)
    }
    fn transcript(&mut self, transcript: &makepad_story_cut::AssetRef) -> Result<Option<Vec<makepad_story_cut::SpokenLine>>, String> {
        self.inner.transcript(transcript)
    }
    fn lyrics(&mut self, song: &makepad_story_cut::AssetRef) -> Result<Option<String>, String> {
        self.inner.lyrics(song)
    }
}

struct Job {
    /// The control (its path, or `macro <label>`), and which end.
    control: String,
    end: &'static str,
    t: f64,
    texts: Arc<HashMap<String, String>>,
}

/// A control's measure: the share of pixels its range changes, and at
/// which moment.
#[derive(Clone, Debug)]
pub struct Measured {
    pub control: String,
    pub changed: f64,
    pub t: f64,
}

#[derive(Default)]
struct Sweep {
    /// The request it checks (its fix goes back under it).
    pub serial: u64,
    doc: Option<Arc<CutDoc>>,
    tree: Option<Arc<makepad_asset_data::tree::TreeDraft>>,
    resolver: Option<SharedResolver>,
    jobs: std::collections::VecDeque<Job>,
    running: Option<(Job, CutThumbnail)>,
    /// When the running render started (one that takes too long fails).
    /// Pixels per (control, end): `base@t` for the picture as written.
    pictures: HashMap<(String, String), Vec<u8>>,
    controls: Vec<(String, Vec<f64>)>,
    started: f64,
    done: Option<Vec<Measured>>,
    last: Vec<Measured>,
    failures: Vec<String>,
    /// Texts compared with the picture as written (the deep make's
    /// exposures), not a panel's controls.
    compare: bool,
}

thread_local! {
    static SWEEP: RefCell<Sweep> = RefCell::new(Sweep::default());
}

/// A number written as the site writes it (its unit kept).
fn written_like(site: &str, v: f64) -> String {
    let digits = site.trim_start_matches('-');
    let unit = &digits[digits.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(digits.len())..];
    let v = if site.contains('.') { v } else { v.round() };
    format!("{}{unit}", makepad_motion::patch::fmt_num(v))
}

fn number_of(text: &str) -> Option<f64> {
    let t = text.trim();
    let digits = t.trim_start_matches('-');
    let end = digits.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(digits.len());
    digits[..end].parse::<f64>().ok().map(|v| if t.starts_with('-') { -v } else { v })
}

/// A range kept within four times `value` either way (a value at 0:
/// within ±4).
fn clamp_range(value: f64, low: f64, high: f64) -> (f64, f64) {
    let span = if value == 0.0 { 4.0 } else { value.abs() * 4.0 };
    let (lo, hi) = (value - span, value + span);
    let (lo, hi) = if value > 0.0 { (value / 4.0, hi) } else { (lo, hi) };
    (low.clamp(lo.min(hi), hi.max(lo)), high.clamp(lo.min(hi), hi.max(lo)))
}

/// The panel's number controls: (control, [(path, low, high)]).
fn controls(files: &[paths::File], body: &str) -> Vec<(String, Vec<(String, f64, f64)>)> {
    let field = |line: &str, name: &str| -> Option<String> {
        let at = line.find(&format!("{name}: "))? + name.len() + 2;
        let rest = &line[at..];
        Some(match rest.strip_prefix('"') {
            Some(q) => q[..q.find('"')?].to_string(),
            None => rest.split([' ', '}']).next()?.to_string(),
        })
    };
    let mut out = Vec::new();
    for line in body.lines().map(str::trim).filter(|l| l.starts_with("Panel")) {
        if let Some(targets) = field(line, "targets") {
            let ends: Vec<(String, f64, f64)> = targets.split(',').filter_map(|t| {
                let mut w = t.split_whitespace();
                let p = w.next()?.to_string();
                let (a, b): (f64, f64) = (w.next()?.parse().ok()?, w.next()?.parse().ok()?);
                let value = paths::resolve(files, &p).ok().and_then(|site| number_of(&site.text)).unwrap_or((a + b) * 0.5);
                let (a, b) = clamp_range(value, a, b);
                Some((p, a, b))
            }).collect();
            if !ends.is_empty() {
                out.push((format!("macro {}", field(line, "label").unwrap_or_default()), ends));
            }
            continue;
        }
        let Some(path) = field(line, "path") else { continue };
        let Ok(site) = paths::resolve(files, &path) else { continue };
        if !matches!(site.kind, ValueKind::Number | ValueKind::Time | ValueKind::Angle | ValueKind::Length) {
            continue;
        }
        let Some(value) = number_of(&site.text) else { continue };
        let low = field(line, "min").and_then(|m| number_of(&m)).unwrap_or(if value == 0.0 { -1.0 } else { value * 0.25 });
        let high = field(line, "max").and_then(|m| number_of(&m)).unwrap_or(if value == 0.0 { 1.0 } else { value * 2.0 });
        // Measured within four times the value written either way: a
        // count at a wild end of its range can build more geometry than
        // memory holds (the picture tells the same at four times).
        let (low, high) = clamp_range(value, low, high);
        out.push((path.clone(), vec![(path, low, high)]));
    }
    out.truncate(MAX_CONTROLS);
    out
}

/// Start measuring the open graphic's panel (after it was written).
pub fn start(page: &VideoCut, serial: u64) -> Result<usize, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let node = session.node.clone().ok_or("the graphic is not placed in the edit")?;
    let edit = page.session.as_ref().ok_or("no edit is open")?;
    let mini = clip_films::mini_edit(&edit.doc, &edit.source, &node).ok_or("the graphic cannot be drawn alone")?;
    let doc = Arc::new(match &edit.tree {
        Some(tree) => CutDoc::load_tree(&mini, tree.context(&edit.path)),
        None => CutDoc::load(&mini),
    }.map_err(super::super::patch::diagnostics)?);
    let body = super::editor_panel::body_of(&session.source, &session.code).ok_or("the scene has no panel")?;
    let files: Vec<paths::File> = std::iter::once(paths::File { name: "", text: &session.source, code: &session.code })
        .chain(session.modules.iter().filter(|m| !m.builtin()).map(|m| paths::File { name: &m.name, text: &m.source, code: &m.code }))
        .collect();
    // Tree files use their logical paths; library files use asset ids.
    let graphic_id = session.tree_path.clone().or_else(|| session.asset.as_ref().map(|a| a.asset_id.to_string())).ok_or("the graphic has no source")?;
    let ids: Vec<String> = std::iter::once(graphic_id).chain(session.modules.iter().filter(|m| !m.builtin()).map(|m| m.id.clone())).collect();
    let base: HashMap<String, String> = ids.iter().cloned().zip(files.iter().map(|f| f.text.to_string())).collect();
    let duration = session.duration().max(1e-3);
    // The graphic's time is the edit's from where its clip starts (the
    // edit alone keeps the clip where it was).
    let start = page.projection.block(&node).map_or(0.0, |b| b.start);
    let mut jobs = std::collections::VecDeque::new();
    let mut measured = Vec::new();
    let mut base_times: Vec<f64> = Vec::new();
    for (control, ends) in controls(&files, &body) {
        // Moments it acts: a fifth, half and four fifths into its longest
        // span (a path read only at load: into the graphic); its strongest
        // change counts (a punch shows on its beat only).
        let spans = super::panel_time::spans_now(page, &ends[0].0).unwrap_or_default();
        let (a, b) = spans.iter().copied().max_by(|x, y| (x.1 - x.0).total_cmp(&(y.1 - y.0))).unwrap_or((0.0, duration));
        let times: Vec<f64> = [0.2, 0.5, 0.8].iter().map(|f| (((start + a + (b - a) * f).clamp(start, start + duration)) * 30.0).round() / 30.0).collect();
        for &t in &times {
            for (end, pick) in [("low", 0usize), ("high", 1usize)] {
                // Every end's splices, back to front per file.
                let mut texts = base.clone();
                let mut splices: Vec<(usize, std::ops::Range<usize>, String)> = ends.iter().filter_map(|(p, a, b)| {
                    let site = paths::resolve(&files, p).ok()?;
                    let v = if pick == 0 { *a } else { *b };
                    Some((site.file, site.range.clone(), written_like(&site.text, v)))
                }).collect();
                splices.sort_by(|x, y| y.1.start.cmp(&x.1.start));
                for (file, range, text) in splices {
                    if let Some(t) = texts.get_mut(&ids[file]) {
                        if t.is_char_boundary(range.start) && t.is_char_boundary(range.end) && range.end <= t.len() {
                            t.replace_range(range, &text);
                        }
                    }
                }
                jobs.push_back(Job { control: control.clone(), end, t, texts: Arc::new(texts) });
            }
            if !base_times.contains(&t) {
                base_times.push(t);
                jobs.push_front(Job { control: "base".into(), end: "base", t, texts: Arc::new(base.clone()) });
            }
        }
        measured.push((control, times));
    }
    let count = measured.len();
    SWEEP.with(|s| {
        let last = std::mem::take(&mut s.borrow_mut().last);
        *s.borrow_mut() = Sweep {
            last,
            serial,
            doc: Some(doc),
            tree: edit.tree.clone(),
            resolver: Some(SharedResolver::new(Box::new(super::super::store::LazyStoreMedia::new()))),
            jobs,
            controls: measured,
            started: Cx::time_now(),
            ..Default::default()
        };
    });
    log!("panel sweep: {count} controls, {} renders", SWEEP.with(|s| s.borrow().jobs.len()));
    Ok(count)
}

fn key(control: &str, end: &str, t: f64) -> (String, String) {
    if end == "base" { ("base".into(), format!("{t:.3}")) } else { (control.to_string(), format!("{end}@{t:.3}")) }
}

/// The finished measure, once (the self-check takes it).
pub fn take_done() -> Option<(u64, Vec<Measured>, Vec<String>)> {
    SWEEP.with(|s| {
        let mut s = s.borrow_mut();
        if s.compare {
            return None;
        }
        let done = s.done.take()?;
        Some((s.serial, done, std::mem::take(&mut s.failures)))
    })
}

/// A finished comparison, once: per candidate the largest share of pixels
/// that differ from the picture as written, over the moments drawn.
pub fn take_compared() -> Option<(u64, Vec<Measured>, Vec<String>)> {
    SWEEP.with(|s| {
        let mut s = s.borrow_mut();
        if !s.compare {
            return None;
        }
        let done = s.done.take()?;
        Some((s.serial, done, std::mem::take(&mut s.failures)))
    })
}

/// Compare candidate texts with the picture as written: each candidate (a
/// label, and the session files it changes: index, text) drawn at these
/// moments of the graphic (seconds) beside the picture as written.
pub fn start_compare(page: &VideoCut, serial: u64, candidates: Vec<(String, Vec<(usize, String)>)>, times: &[f64]) -> Result<usize, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let node = session.node.clone().ok_or("the graphic is not placed in the edit")?;
    let edit = page.session.as_ref().ok_or("no edit is open")?;
    let mini = clip_films::mini_edit(&edit.doc, &edit.source, &node).ok_or("the graphic cannot be drawn alone")?;
    let doc = Arc::new(match &edit.tree {
        Some(tree) => CutDoc::load_tree(&mini, tree.context(&edit.path)),
        None => CutDoc::load(&mini),
    }.map_err(super::super::patch::diagnostics)?);
    let graphic_id = session.tree_path.clone().or_else(|| session.asset.as_ref().map(|a| a.asset_id.to_string())).ok_or("the graphic has no source")?;
    // Session file index -> tree path or asset id (0: graphic, k + 1: module).
    let id_of = |index: usize| -> Option<String> { if index == 0 { Some(graphic_id.clone()) } else { session.modules.get(index - 1).filter(|m| !m.builtin()).map(|m| m.id.clone()) } };
    let mut base: HashMap<String, String> = HashMap::new();
    base.insert(graphic_id.clone(), session.source.clone());
    for m in session.modules.iter().filter(|m| !m.builtin()) {
        base.insert(m.id.clone(), m.source.clone());
    }
    let start = page.projection.block(&node).map_or(0.0, |b| b.start);
    let times: Vec<f64> = times.iter().map(|t| ((start + t) * 30.0).round() / 30.0).collect();
    let mut jobs = std::collections::VecDeque::new();
    for &t in &times {
        jobs.push_back(Job { control: "base".into(), end: "base", t, texts: Arc::new(base.clone()) });
    }
    let mut measured = Vec::new();
    for (label, files) in candidates {
        let mut texts = base.clone();
        for (index, text) in files {
            if let Some(id) = id_of(index) {
                texts.insert(id, text);
            }
        }
        let texts = Arc::new(texts);
        for &t in &times {
            jobs.push_back(Job { control: label.clone(), end: "high", t, texts: texts.clone() });
        }
        measured.push((label, times.clone()));
    }
    let count = measured.len();
    SWEEP.with(|s| {
        let last = std::mem::take(&mut s.borrow_mut().last);
        *s.borrow_mut() = Sweep {
            last,
            serial,
            doc: Some(doc),
            tree: edit.tree.clone(),
            resolver: Some(SharedResolver::new(Box::new(super::super::store::LazyStoreMedia::new()))),
            jobs,
            controls: measured,
            started: Cx::time_now(),
            compare: true,
            ..Default::default()
        };
    });
    log!("panel sweep: comparing {count} candidates at {} moments", times.len());
    Ok(count)
}

/// The last measure (the control API reads it).
pub fn last() -> Vec<Measured> {
    SWEEP.with(|s| s.borrow().last.clone())
}

/// Renders still to make.
pub fn busy() -> bool {
    SWEEP.with(|s| { let s = s.borrow(); s.running.is_some() || !s.jobs.is_empty() })
}

fn finish(s: &mut Sweep) {
    let mut out = Vec::new();
    for (control, times) in &s.controls {
        let mut best: Option<(f64, f64)> = None;
        for &t in times {
            let Some(base) = s.pictures.get(&key("", "base", t)) else { continue };
            let changed = |end: &str| -> Option<f64> {
                let b = s.pictures.get(&key(control, end, t))?;
                if base.len() != b.len() || base.is_empty() {
                    return None;
                }
                let n = base.len() / 4;
                let moved = base.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(p, q)| p.iter().zip(q.iter()).take(3).any(|(x, y)| x.abs_diff(*y) > PIXEL_STEP)).count();
                Some(moved as f64 / n.max(1) as f64)
            };
            // Either end against the picture as written: what the control
            // can do at this moment.
            // (A candidate the renderer could not draw changed everything.)
            let missing = |end: &str| !s.pictures.contains_key(&key(control, end, t));
            let c = if s.compare && missing("high") { 1.0 } else { changed("low").unwrap_or(0.0).max(changed("high").unwrap_or(0.0)) };
            if best.is_none_or(|(b, _)| c > b) {
                best = Some((c, t));
            }
        }
        if let Some((changed, t)) = best {
            out.push(Measured { control: control.clone(), changed, t });
        }
    }
    log!("panel sweep: done in {:.1} s: {}", Cx::time_now() - s.started, out.iter().map(|m| format!("{} {:.1}%", m.control, m.changed * 100.0)).collect::<Vec<_>>().join(", "));
    s.last = out.clone();
    s.done = Some(out);
    s.doc = None;
    s.tree = None;
    s.resolver = None;
}

/// Candidate text in a tree must replace the tree's bytes: Motion resolves
/// plates and imports through that snapshot, without consulting Served.
fn picture_doc(doc: Arc<CutDoc>, tree: Option<&Arc<makepad_asset_data::tree::TreeDraft>>, texts: &HashMap<String, String>) -> Result<Arc<CutDoc>, String> {
    let Some(tree) = tree else { return Ok(doc) };
    let mut draft = (**tree).clone();
    for (path, text) in texts {
        let kind = draft.files.get(path).ok_or_else(|| format!("no tree file `{path}`"))?.kind;
        draft.write(path, kind, text.as_bytes().into())?;
    }
    let tree = Arc::new(draft);
    let mut doc = (*doc).clone();
    for graphic in &mut doc.graphics {
        if let makepad_story_cut::GraphicSrc::Tree(context) = &mut graphic.src {
            *context = tree.context(&context.document);
        }
    }
    Ok(Arc::new(doc))
}

/// The zero-size widget that drives the renders from its draw.
#[derive(Script, ScriptHook, Widget)]
pub struct PanelSweep {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[area]
    area: Area,
}

impl Widget for PanelSweep {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let more = SWEEP.with(|s| {
            let mut s = s.borrow_mut();
            if let Some((_, thumb)) = &mut s.running {
                thumb.handle_event(cx, event);
            }
            s.running.is_some() || !s.jobs.is_empty()
        });
        if more {
            self.area.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.walk_turtle_with_area(&mut self.area, walk);
        let more = SWEEP.with(|s| {
            let mut s = s.borrow_mut();
            // The running render: drawn on, or taken (a render that cannot
            // draw its frame, e.g. a shader that does not compile, fails
            // with that error; a slow one is waited for).
            let result = s.running.as_ref().and_then(|(_, thumb)| thumb.result().cloned());
            if let Some(result) = result {
                let (job, _) = s.running.take().expect("running");
                match result {
                    Ok(thumb) => {
                        s.pictures.insert(key(&job.control, job.end, job.t), thumb.rgba);
                    }
                    Err(error) => s.failures.push(format!("{} {}: {error}", job.control, job.end)),
                }
            }
            if s.running.is_none() {
                if let Some(job) = s.jobs.pop_front() {
                    if let (Some(doc), Some(resolver)) = (s.doc.clone(), s.resolver.clone()) {
                        match picture_doc(doc, s.tree.as_ref(), &job.texts) {
                            Ok(doc) => {
                                let served = Served { inner: resolver, texts: job.texts.clone() };
                                let edit = CutJob { doc, marks: HashMap::new(), resolver: Box::new(served), audio: false };
                                let thumb = CutThumbnail::with_options(cx, edit, job.t, WIDTH, EngineOptions { max_loads: 1, budget: None });
                                s.running = Some((job, thumb));
                            }
                            Err(error) => s.failures.push(format!("{} {}: {error}", job.control, job.end)),
                        }
                    }
                } else if s.doc.is_some() {
                    finish(&mut s);
                }
            }
            if let Some((_, thumb)) = &mut s.running {
                thumb.draw(cx);
            }
            s.running.is_some() || !s.jobs.is_empty()
        });
        if more {
            self.area.redraw(cx);
        }
        DrawStep::done()
    }
}

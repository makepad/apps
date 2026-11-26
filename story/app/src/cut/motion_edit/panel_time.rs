//! When and where a panel's controls act: each path's time spans over the
//! graphic (from when the code that reads it draws), whether it is active
//! now (the playhead in its spans, or the picture's selection reading it),
//! and, for the control in focus, its spans on the scrubber and outlines
//! around what it affects in the picture.
//!
//! A path's spans: the code regions that name the `let` it is in (the
//! innermost region at each mention; a region that draws nothing stands
//! for its nearest parent that does, short of the top level), and the
//! samples at which any of them draws (the checker's activity rows). A
//! path the code reads only at load, or only in a helper that draws
//! nothing, has no spans: unknown, never dimmed.
//!
//! Main-loop cost: the panel asks per path; a path's spans are computed
//! once per sampling (a scan of the texts for its name), the active set
//! once per pump for the paths asked.

use super::super::VideoCut;
use makepad_motion::schema::CodeFile;
use makepad_widgets::*;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Default)]
struct Time {
    /// The activity the spans are of: (model, samples).
    of: Option<(u64, usize)>,
    /// Each path asked: its spans (None: unknown), and the regions that
    /// read it (file, region), for the picture's outlines.
    spans: HashMap<String, Option<Vec<(f64, f64)>>>,
    regions: HashMap<String, Vec<(CodeFile, std::ops::Range<usize>)>>,
    asked: Vec<String>,
    /// Active now, per path asked (None: unknown).
    active: HashMap<String, Option<bool>>,
    rev: u64,
    /// Rises when the picture's selection changes (the panel scrolls to
    /// what it lights).
    reveal: u64,
    selection_rev: Option<u64>,
    selected: Vec<String>,
    /// The picture selection's first label (the panel's root filter), and
    /// a filter cleared from the panel (the desk lets go of the selection).
    selected_label: Option<String>,
    clear_asked: bool,
    /// The control in focus (its paths), as set and as shown.
    focus: Option<Vec<String>>,
    focus_shown: Option<Vec<String>>,
    playhead: f64,
    /// The panel's `when:` spans per path, and its words.
    when: HashMap<String, Vec<(f64, f64)>>,
    when_said: HashMap<String, String>,
}

thread_local! {
    static TIME: RefCell<Time> = RefCell::new(Time::default());
}

/// The host's `active(path)`: as of the last pump (a path asked for the
/// first time is answered on the next).
pub fn active(path: &str) -> Option<bool> {
    TIME.with(|t| {
        let mut t = t.borrow_mut();
        match t.active.get(path) {
            Some(a) => *a,
            None => {
                if !t.asked.iter().any(|p| p == path) {
                    t.asked.push(path.to_string());
                }
                None
            }
        }
    })
}

/// Whether a path with these measured spans is in use at `t` (None: not
/// known, never dimmed).
fn active_at(spans: Option<&[(f64, f64)]>, t: f64) -> Option<bool> {
    let spans = spans.filter(|s| !s.is_empty())?;
    let first = spans.iter().map(|s| s.0).fold(f64::MAX, f64::min);
    let last = spans.iter().map(|s| s.1).fold(f64::MIN, f64::max);
    Some(t >= first && t < last)
}

/// The host's `picked(path)`: the picture's selection reads it.
pub fn picked(path: &str) -> bool {
    TIME.with(|t| t.borrow().selected.iter().any(|s| s == path || s.starts_with(&format!("{path}.")) || path.starts_with(&format!("{s}."))))
}

/// The paths the panel has asked about (what its controls bind).
pub fn asked() -> Vec<String> {
    TIME.with(|t| t.borrow().asked.clone())
}

/// The host's `lit_rev()`: rises when an answer of `active` changed.
pub fn lit_rev() -> u64 {
    TIME.with(|t| t.borrow().rev)
}

/// The host's `spans(paths)`: when any of them acts (graphic seconds).
pub fn spans_of_paths(paths: &[String]) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = paths.iter().filter_map(|p| spans(p)).flatten().collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Joined where they meet.
    let mut joined: Vec<(f64, f64)> = Vec::new();
    for (a, b) in out {
        match joined.last_mut() {
            Some(last) if a <= last.1 + 1e-6 => last.1 = last.1.max(b),
            _ => joined.push((a, b)),
        }
    }
    joined
}

/// The host's `selection_label()`.
pub fn selection_label() -> Option<String> {
    TIME.with(|t| t.borrow().selected_label.clone())
}

/// The host's `clear_selection_filter()`: the desk lets go of the
/// picture's selection on its next pump.
pub fn clear_selection_filter() {
    TIME.with(|t| t.borrow_mut().clear_asked = true);
}

/// Taken by the desk's pump.
pub fn take_clear() -> bool {
    TIME.with(|t| std::mem::take(&mut t.borrow_mut().clear_asked))
}

/// The host's `reveal()`.
pub fn reveal_rev() -> u64 {
    TIME.with(|t| t.borrow().reveal)
}

/// The host's `focus(paths)`: the control hovered or selected.
pub fn focus(paths: Option<&[String]>) {
    TIME.with(|t| t.borrow_mut().focus = paths.map(|p| p.to_vec()));
}

fn ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The `let` a path names, as code elsewhere writes it: `WORD_SPARKS` of
/// `WORD_SPARKS.count`, `POST` of `look.POST.bloom` (a module's member).
fn root_name(path: &str, modules: &[&str]) -> String {
    let mut parts = path.split(['.', '[']);
    let first = parts.next().unwrap_or("");
    if modules.contains(&first) { parts.next().unwrap_or("").to_string() } else { first.to_string() }
}

/// A path's spans (graphic seconds) and the regions that read it.
fn spans_of(page: &VideoCut, path: &str) -> (Option<Vec<(f64, f64)>>, Vec<(CodeFile, std::ops::Range<usize>)>) {
    let Some(session) = page.motion.desk.session.as_ref() else { return (None, Vec::new()) };
    spans_in(session, |file| super::desk::activity_of(page, file), path)
}

/// A path's spans in `session`, its regions' activity rows by `rows`.
fn spans_in<'a>(session: &super::session::MotionSession, rows: impl Fn(&CodeFile) -> Option<&'a Vec<Vec<f32>>>, path: &str) -> (Option<Vec<(f64, f64)>>, Vec<(CodeFile, std::ops::Range<usize>)>) {
    let modules: Vec<&str> = session.modules.iter().filter(|m| !m.builtin()).map(|m| m.name.as_str()).collect();
    let name = root_name(path, &modules);
    if name.is_empty() {
        return (None, Vec::new());
    }
    let files = std::iter::once((CodeFile::Doc, session.source.as_str(), &session.code))
        .chain(session.modules.iter().filter(|m| !m.builtin()).map(|m| (CodeFile::Module(m.key.clone()), m.source.as_str(), &m.code)));
    let duration = session.duration().max(1e-6);
    let mut drawn: Vec<bool> = Vec::new();
    let mut regions = Vec::new();
    for (file, text, code) in files {
        let Some(rows) = rows(&file) else { continue };
        let bytes = text.as_bytes();
        let mut from = 0;
        while let Some(at) = text[from..].find(name.as_str()).map(|i| from + i) {
            from = at + name.len();
            let end = at + name.len();
            if (at > 0 && ident(bytes[at - 1])) || (end < bytes.len() && ident(bytes[end])) {
                continue;
            }
            // The innermost region drawing, short of the top level (the
            // `let` itself, the whole `Motion{`).
            let mut r = code.region_at(at);
            while let Some(i) = r {
                let region = &code.regions[i];
                if region.depth == 0 {
                    r = None;
                    break;
                }
                if rows.get(i).is_some_and(|row| row.iter().any(|a| *a > 0.0)) {
                    break;
                }
                r = region.parent;
            }
            let Some(i) = r else { continue };
            let row = &rows[i];
            if drawn.is_empty() {
                drawn = vec![false; row.len()];
            }
            for (k, a) in row.iter().enumerate() {
                if *a > 0.0 && k < drawn.len() {
                    drawn[k] = true;
                }
            }
            if !regions.iter().any(|(f, rr): &(CodeFile, std::ops::Range<usize>)| *f == file && *rr == code.regions[i].range) {
                regions.push((file.clone(), code.regions[i].range.clone()));
            }
        }
    }
    if !drawn.iter().any(|d| *d) {
        return (None, regions);
    }
    // Runs of samples drawn, each sample the span around its moment.
    let n = drawn.len() as f64;
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for (k, d) in drawn.iter().enumerate() {
        if !d {
            continue;
        }
        let (a, b) = (k as f64 / n * duration, (k + 1) as f64 / n * duration);
        match spans.last_mut() {
            Some(last) if (last.1 - a).abs() < 1e-9 => last.1 = b,
            _ => spans.push((a, b)),
        }
    }
    (Some(spans), regions)
}

/// On the desk's pump: spans for paths newly asked (or all again after a
/// new sampling), the active set, the focus shown.
pub fn pump(page: &VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let of = super::desk::series_rev(page);
    let playhead = session.playhead;
    let (sel_rev, things) = super::desk::selection(page);
    let label = things.first().map(|s| s.label.clone());
    let selected: Vec<String> = things.iter().flat_map(|s| s.paths.iter().cloned()).collect();
    // What needs computing (outside the borrow: it reads the page).
    let todo: Vec<String> = TIME.with(|t| {
        let mut t = t.borrow_mut();
        // The film sampled again after a load: its coarse look first, then
        // the fine one. Spans are read from the fine one only (the coarse
        // one's moments are far apart: what acts would flip on and off
        // between the two), except for a first look.
        let coarse = of.1 == super::checker::COARSE_SAMPLES;
        if t.of != Some(of) && (!coarse || t.of.is_none()) {
            t.of = Some(of);
            t.spans.clear();
            t.regions.clear();
        }
        let mut todo: Vec<String> = t.asked.iter().filter(|p| !t.spans.contains_key(*p)).cloned().collect();
        if let Some(focus) = &t.focus {
            todo.extend(focus.iter().filter(|p| !t.spans.contains_key(*p)).cloned());
        }
        todo
    });
    let computed: Vec<(String, Option<Vec<(f64, f64)>>, Vec<(CodeFile, std::ops::Range<usize>)>)> = todo.into_iter().map(|p| {
        let (spans, regions) = spans_of(page, &p);
        (p, spans, regions)
    }).collect();
    let focus_change = TIME.with(|t| {
        let mut t = t.borrow_mut();
        let any_new = !computed.is_empty();
        for (p, spans, regions) in computed {
            t.spans.insert(p.clone(), spans);
            t.regions.insert(p, regions);
        }
        let selection_changed = t.selection_rev != Some(sel_rev);
        if selection_changed {
            t.selection_rev = Some(sel_rev);
            t.selected_label = label.filter(|_| !selected.is_empty());
            if !selected.is_empty() {
                t.reveal += 1;
            }
            t.selected = selected;
        }
        // Active now: measured at the playhead (the picture's selection
        // marks what reads it, `picked`, and dims nothing).
        if any_new || selection_changed || t.playhead != playhead {
            t.playhead = playhead;
            let mut changed = false;
            let asked = t.asked.clone();
            for p in asked {
                // Measured, never declared (a module's `when:` covers
                // paths it shares with others): inactive only before the
                // path is first read or after it is last read (a value read
                // when something is spawned or starts is in use for as
                // long as what it made lives); when in doubt, active.
                let now = active_at(t.spans.get(&p).cloned().flatten().as_deref(), playhead);
                if t.active.get(&p) != Some(&now) {
                    t.active.insert(p, now);
                    changed = true;
                }
            }
            if changed {
                t.rev += 1;
                if makepad_value_pickers::panel::host::timing() {
                    log!("panel.step: what acts changed (lit {}) at {playhead:.3}", t.rev);
                }
            }
        }
        (t.focus != t.focus_shown).then(|| {
            t.focus_shown = t.focus.clone();
            let focus = t.focus.clone();
            let spans: Vec<(f64, f64)> = focus.iter().flatten().filter_map(|p| t.spans.get(p).cloned().flatten()).flatten().collect();
            let regions: Vec<(CodeFile, std::ops::Range<usize>)> = focus.iter().flatten().filter_map(|p| t.regions.get(p).cloned()).flatten().collect();
            (focus, spans, regions)
        })
    });
    let Some((focus, spans, regions)) = focus_change else { return };
    // The scrubber: the focused control's spans, lit.
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    if let Some(mut code) = desk_ref.widget(cx, ids!(m_code)).borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
        match &focus {
            Some(paths) if !spans.is_empty() => {
                let label = paths.first().cloned().unwrap_or_default() + if paths.len() > 1 { " …" } else { "" };
                code.set_spans(cx, vec![super::code_timeline::ScrubSpans { key: "panel".into(), label, spans }]);
                code.set_span_focus(cx, Some("panel".into()));
            }
            _ => {
                code.set_spans(cx, Vec::new());
                code.set_span_focus(cx, None);
            }
        }
    }
    // The picture: what the code that reads it drew in this frame.
    // One thin outline round all it touches (never a box per stroke).
    let quads = focus.as_ref().map(|_| {
        let touched: Vec<[[f64; 2]; 4]> = super::desk::picks(page).map(|list| list.iter().filter(|p| p.content && p.code.iter().any(|(f, byte)| regions.iter().any(|(rf, r)| rf == f && r.contains(byte)))).map(|p| p.quad).collect()).unwrap_or_default();
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for q in &touched {
            for p in q {
                x0 = x0.min(p[0]);
                y0 = y0.min(p[1]);
                x1 = x1.max(p[0]);
                y1 = y1.max(p[1]);
            }
        }
        if touched.is_empty() { Vec::new() } else { vec![[[x0, y0], [x1, y0], [x1, y1], [x0, y1]]] }
    });
    super::desk::outline(page, cx, ui, quads);
}

/// A path's spans as measured, computed now if they were not.
pub fn spans_now(page: &VideoCut, path: &str) -> Option<Vec<(f64, f64)>> {
    spans_of(page, path).0
}

/// A path's spans as computed (None: not yet, or unknown).
pub fn spans(path: &str) -> Option<Vec<(f64, f64)>> {
    TIME.with(|t| {
        let mut t = t.borrow_mut();
        if !t.asked.iter().any(|p| p == path) {
            t.asked.push(path.to_string());
        }
        t.spans.get(path).cloned().flatten()
    })
}

/// Where the focused control acts, when the playhead is outside it: the
/// start of its nearest span (the desk moves the playhead there when a
/// drag starts, so the change is seen).
pub fn nearest_span_start(path: &str, playhead: f64) -> Option<f64> {
    TIME.with(|t| {
        let t = t.borrow();
        let spans = t.spans.get(path)?.as_ref()?;
        if spans.iter().any(|(a, b)| playhead >= *a && playhead < *b) {
            return None;
        }
        spans.iter().map(|(a, b)| if playhead < *a { *a } else { *b - 1e-3 }).min_by(|x, y| (x - playhead).abs().total_cmp(&(y - playhead).abs()))
    })
}

// ------------------------------------------------------------------ `when:`

/// The panel's own word on when its controls act: each `when: "…"` of a
/// module (for every path in it) or a control (its paths), read as spans:
/// `2.4s-5s` (seconds), `beats 9-16` (the song's beats, counted from 1),
/// `the lyric 'AGI'` (the sung words), several joined by commas; anything
/// else (`throughout`) says nothing. A path with such spans is lit by them;
/// the sampled ones stay the check.
pub fn set_when(body: &str) {
    let mut when: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
    let mut said: HashMap<String, String> = HashMap::new();
    for (paths, text) in when_records(body) {
        let spans = when_spans(&text);
        for p in paths {
            said.insert(p.clone(), text.clone());
            if let Some(spans) = &spans {
                when.entry(p).or_default().extend(spans.iter().copied());
            }
        }
    }
    TIME.with(|t| {
        let mut t = t.borrow_mut();
        t.when = when;
        t.when_said = said;
        t.playhead = f64::NAN;
    });
}

/// Each record of the panel with a `when:`: the paths it covers (a
/// module's: every path in it without its own) and the text.
fn when_records(body: &str) -> Vec<(Vec<String>, String)> {
    let b = body.as_bytes();
    // Each `PanelX{`: (name, its body's bytes).
    let mut records: Vec<(String, usize, usize)> = Vec::new();
    let mut i = 0;
    while let Some(at) = body[i..].find("Panel").map(|k| i + k) {
        i = at + 5;
        if at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_') {
            continue;
        }
        let name_end = body[at..].find(|c: char| !c.is_ascii_alphanumeric()).map_or(body.len(), |k| at + k);
        if body.as_bytes().get(name_end) != Some(&b'{') {
            continue;
        }
        let mut depth = 0i32;
        let mut in_str = false;
        let mut end = body.len();
        for (k, &c) in b.iter().enumerate().skip(name_end) {
            match c {
                b'"' => in_str = !in_str,
                b'{' if !in_str => depth += 1,
                b'}' if !in_str => {
                    depth -= 1;
                    if depth == 0 {
                        end = k;
                        break;
                    }
                }
                _ => {}
            }
        }
        records.push((body[at..name_end].to_string(), name_end + 1, end));
    }
    // A field of a record, outside the records nested in it.
    let field = |start: usize, end: usize, name: &str| -> Option<String> {
        let inner: Vec<(usize, usize)> = records.iter().filter(|(_, s, e)| *s > start && *e <= end).map(|(_, s, e)| (*s, *e)).collect();
        let mut from = start;
        while let Some(at) = body[from..end].find(&format!("{name}: \"")).map(|k| from + k) {
            from = at + 1;
            if inner.iter().any(|(s, e)| at >= *s && at < *e) {
                continue;
            }
            let v = at + name.len() + 3;
            let close = body[v..end].find('"')? + v;
            return Some(body[v..close].to_string());
        }
        None
    };
    let paths_in = |start: usize, end: usize| -> Vec<String> {
        let mut out = Vec::new();
        for (_, s, e) in records.iter().filter(|(_, s, e)| *s >= start && *e <= end) {
            if let Some(p) = field(*s, *e, "path") {
                out.push(p);
            }
            if let Some(targets) = field(*s, *e, "targets") {
                out.extend(targets.split(',').filter_map(|t| t.split_whitespace().next().map(str::to_string)));
            }
        }
        out
    };
    let mut out = Vec::new();
    // Modules first; a control's own `when` is pushed after and wins.
    for (name, s, e) in records.iter().filter(|(n, _, _)| n == "PanelModule") {
        let _ = name;
        if let Some(text) = field(*s, *e, "when") {
            out.push((paths_in(*s, *e), text));
        }
    }
    for (name, s, e) in records.iter().filter(|(n, _, _)| n != "PanelModule") {
        let _ = name;
        if let Some(text) = field(*s, *e, "when") {
            out.push((paths_in(*s, *e), text));
        }
    }
    out
}

/// A `when` text as spans (graphic seconds), or None (it says nothing).
fn when_spans(text: &str) -> Option<Vec<(f64, f64)>> {
    let (_, duration, _) = super::panel_feed::playhead();
    let mut out = Vec::new();
    for part in text.split(',').map(str::trim) {
        let lower = part.to_lowercase();
        let numbers: Vec<f64> = lower.split(|c: char| !(c.is_ascii_digit() || c == '.')).filter_map(|n| n.parse().ok()).collect();
        if lower.contains("beat") && numbers.len() >= 1 {
            let beats = super::panel_feed::beats(0.0, duration.max(1e-3));
            let a = numbers[0] as usize;
            let b = numbers.get(1).map_or(a, |n| *n as usize);
            if a >= 1 && a <= beats.len() {
                let end = beats.get(b).copied().unwrap_or(duration);
                out.push((beats[a - 1], end));
            }
        } else if let Some(word) = part.split(['\'', '"']).nth(1).filter(|w| !w.is_empty()) {
            let word = word.to_lowercase();
            for (a, b, w) in super::panel_feed::words(0.0, duration.max(1e-3)) {
                if w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()) == word.trim_matches(|c: char| !c.is_alphanumeric()) {
                    out.push((a, b));
                }
            }
        } else if numbers.len() >= 2 && (lower.contains('s') || lower.contains('-')) {
            out.push((numbers[0], numbers[1]));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Paths whose `when` and sampled spans do not meet (the self-check's).
pub fn when_disagreements() -> Vec<String> {
    TIME.with(|t| {
        let t = t.borrow();
        t.when.iter().filter_map(|(p, when)| {
            let sampled = t.spans.get(p)?.as_ref()?;
            let meet = when.iter().any(|(a, b)| sampled.iter().any(|(c, d)| a < d && c < b));
            (!meet).then(|| format!("`{p}`: its when \"{}\" ({}) does not meet when it acts ({})", t.when_said.get(p).cloned().unwrap_or_default(), fmt_spans(when), fmt_spans(sampled)))
        }).collect()
    })
}

fn fmt_spans(spans: &[(f64, f64)]) -> String {
    spans.iter().take(4).map(|(a, b)| format!("{a:.1}-{b:.1} s")).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use super::super::panel_effects::{bundled_font_modules, bundled_font_source};

    /// The waterline and camera both draw at one second. Selecting the
    /// gauge label's font leaves those panel rows active.
    #[test]
    fn tide_plate_waterline_and_post_are_active_at_1s() {
        makepad_editor_panel::install_doc(makepad_stage_motion_pickers::motion_doc());
        makepad_doc_fonts::register_root("makepad_widgets", makepad_widgets::widgets_dir());
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/panel_film");
        use makepad_asset_data::tree::{TreeDraft, TreeRole, TreeResolver};
        let snapshot = makepad_motion::tree::read_folder(&dir, "film.cut.splash", TreeRole::Cut).unwrap();
        let tree = std::sync::Arc::new(TreeDraft::from_memory(&snapshot).unwrap());
        let file = "plates/tide.splash";
        let text = bundled_font_source(tree.text(file).unwrap());
        let context = makepad_motion::MotionContext { modules: bundled_font_modules(tree.context(file)), ..Default::default() };
        let doc = std::sync::Arc::new(makepad_motion::MotionDoc::load_with(&text, &[], context).unwrap());
        let mut session = super::super::session::MotionSession::new(text.clone(), "Tide".into(), None, None);
        for line in text.lines() {
            let Some(rest) = line.trim_start().strip_prefix("let ") else { continue };
            let Some((name, value)) = rest.split_once('=') else { continue };
            let Some(path) = value.trim().strip_prefix("import(\"").and_then(|tail| tail.split('"').next()) else { continue };
            let module = tree.resolve(file, path).unwrap();
            let t = bundled_font_source(module.text().unwrap());
            session.modules.push(super::super::session::ModuleDoc { key: module.path.clone(), id: module.path, rev: String::new(), name: name.trim().into(), source: t.clone(), saved: t.clone(), code: makepad_motion::schema::code::outline(&t) });
        }
        session.doc = Some(doc.clone());
        let samples = super::super::checker::sample_for_test(&doc, super::super::checker::ACTIVITY_SAMPLES);
        let rows = super::super::desk::activity_rows(&session, &samples);
        // Each module's paths, from the plate's panel.
        let code = &session.code;
        let panel = &text[makepad_editor_panel::panel_region(&text, code).unwrap()];
        let module = |title: &str| -> Vec<String> {
            let body = panel.split(&format!("PanelModule{{title: \"{title}\"")).nth(1).unwrap();
            let body = body.split("PanelModule{").next().unwrap();
            body.split("path: \"").skip(1).filter_map(|p| p.split('"').next().map(str::to_string)).collect()
        };
        for title in ["Waterline", "Camera and shade"] {
            let paths = module(title);
            assert!(!paths.is_empty(), "{title} holds controls");
            let states: Vec<(String, Option<bool>)> = paths.iter().map(|p| (p.clone(), active_at(spans_in(&session, |f| rows.get(f), p).0.as_deref(), 1.0))).collect();
            // Dimmed only when every path is known not to be in use.
            let dimmed = states.iter().all(|(_, a)| *a == Some(false));
            assert!(!dimmed, "{title} is dimmed at 1 s: {states:?}");
            // A selection reading something else changes none of it.
            TIME.with(|t| t.borrow_mut().selected = vec!["look.FACE".into()]);
            assert!(!picked(&paths[0]) || paths[0].starts_with("look.FACE"));
            let again: Vec<Option<bool>> = paths.iter().map(|p| active_at(spans_in(&session, |f| rows.get(f), p).0.as_deref(), 1.0)).collect();
            assert_eq!(again, states.iter().map(|(_, a)| *a).collect::<Vec<_>>());
            TIME.with(|t| t.borrow_mut().selected.clear());
        }
    }
}

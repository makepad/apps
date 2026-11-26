//! The graphic desk's visual editors for a font or a colour (lane PE1):
//! the value a picked thing (or an inspector field) is drawn with, found in
//! the code and edited in a picker docked over the inspector.
//!
//! - A **font** is a `Font{family weight italic axes}` value: a property
//!   reading `LYRIC_LOOK.font` is followed through the `let`s it names to
//!   the `Font{…}` it comes down to (a variant such as `MONO{weight: 500}`
//!   on the way is where that variant's own fields are written). An edit
//!   writes each changed field where it is written (a field nobody writes
//!   goes into the nearest object).
//! - A **colour** is every colour literal the property's expression reads
//!   (through its `let`s and records): `color_mix8(LYRIC_LOOK.singing,
//!   LYRIC_LOOK.hit, …)` offers SIGNAL and EMBER, and the picker edits the
//!   one chosen. "Everywhere" writes every place of the scene that colour
//!   (or its `/**tag*/`) is written, each in its own form.
//!
//! Live: dragging a slider, a strip or the plane loads
//! the edited code on a worker and the viewer shows that document (nothing
//! is written); letting go writes the edit as one undo step (the loaded
//! document is shown at once when it is the one written).
//!
//! Main-loop cost: nothing per frame while closed; open, a channel is
//! polled each pump and a resolve runs per edit (the code's lets, linear).

use super::super::VideoCut;
use super::checker::DocKind;
use makepad_widgets::makepad_script::syntax::{self as tree, Kind, Node};
use makepad_motion::schema::code::CodeOutline;
use makepad_motion::MotionDoc;
use makepad_stage_motion_pickers::{FontChoice, FontPickAction, FontPicker};
use makepad_value_pickers::{format_code_color, ColorPickAction, HdrColorPicker, PaletteEntry};
use makepad_widgets::*;
use std::ops::Range;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    // Docked over the inspector while a font or a colour is edited.
    mod.widgets.MotionValuePick = View{width: Fill height: Fill flow: Down visible: false
        View{width: Fill height: 34 flow: Right spacing: 8 padding: Inset{left: 12 right: 8} align: Align{y: 0.5}
            vp_kind := Label{width: Fit text: "" draw_text +: {color: stage.secondary text_style: theme.font_bold{font_size: 8}}}
            vp_what := Label{width: Fill max_lines: 1 text: "" draw_text +: {color: stage.text_hi text_style: theme.font_regular{font_size: 9}}}
            Tip{text: "Close the picker (what was written stays; cmd-Z undoes it)" vp_done := StageButton{width: Fit height: 22 margin: 0 padding: Inset{left: 10 right: 10} text: "Done"
                draw_text +: {text_style: theme.font_regular{font_size: 9}}}}
        }
        SolidView{width: Fill height: 1 draw_bg +: {color: stage.separator}}
        ScrollYView{width: Fill height: Fill flow: Down padding: Inset{left: 12 right: 12 top: 10 bottom: 14}
            vp_where := Label{width: Fill text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
            View{width: Fill height: 8}
            vp_font := FontPicker{visible: false}
            vp_color := HdrColorPicker{visible: false}
        }
    }
}

/// A file of the open graphic: 0 the document, `k + 1` module `k`.
fn file_text(session: &super::session::MotionSession, doc: usize) -> Option<(&CodeOutline, &str)> {
    if doc == 0 {
        Some((&session.code, session.source.as_str()))
    } else {
        session.modules.get(doc - 1).filter(|m| !m.builtin()).map(|m| (&m.code, m.source.as_str()))
    }
}

// ------------------------------------------------------------------ fonts

/// The face the objects say, the nearest's fields over the font's.
fn font_choice(text: &str, objects: &[Range<usize>]) -> FontChoice {
    let mut choice = FontChoice { family: "Inter".into(), weight: 400.0, italic: false, axes: Vec::new() };
    let mut seen: Vec<String> = Vec::new();
    for r in objects {
        let Ok(node) = tree::parse_value(&text[r.clone()]) else { continue };
        let Some((_, fields)) = node.object() else { continue };
        for f in fields {
            let Some(name) = f.name.as_deref() else { continue };
            if seen.iter().any(|s| s == name) {
                continue;
            }
            seen.push(name.to_string());
            match (name, &f.value.kind) {
                ("family", Kind::Str(s)) => choice.family = s.clone(),
                ("weight", Kind::Number(w)) => choice.weight = *w,
                ("italic", Kind::Ident(b)) => choice.italic = b == "true",
                ("axes", Kind::Object { fields, .. }) => {
                    choice.axes = fields.iter().filter_map(|a| Some((a.name.clone()?, a.value.number()?))).collect();
                }
                _ => {}
            }
        }
    }
    choice
}

fn fmt_num(v: f64) -> String {
    makepad_motion::patch::fmt_num(v)
}

/// The splices that make `objects` say `to` where they said `from`: each
/// changed field where it is written, else in the nearest object.
fn font_edits(text: &str, objects: &[Range<usize>], from: &FontChoice, to: &FontChoice) -> Vec<(Range<usize>, String)> {
    let mut changed: Vec<(&str, String)> = Vec::new();
    if from.family != to.family {
        changed.push(("family", makepad_motion::patch::fmt_text(&to.family)));
    }
    if (from.weight - to.weight).abs() > 1e-9 {
        changed.push(("weight", fmt_num(to.weight)));
    }
    if from.italic != to.italic {
        changed.push(("italic", to.italic.to_string()));
    }
    let mut axes = to.axes.clone();
    axes.sort_by(|a, b| a.0.cmp(&b.0));
    let mut was = from.axes.clone();
    was.sort_by(|a, b| a.0.cmp(&b.0));
    if axes != was {
        let pairs: Vec<String> = to.axes.iter().map(|(t, v)| format!("{t}: {}", fmt_num(*v))).collect();
        changed.push(("axes", format!("{{{}}}", pairs.join(" "))));
    }
    let mut edits = Vec::new();
    for (name, value) in changed {
        let written = objects.iter().find_map(|r| {
            let node = tree::parse_value(&text[r.clone()]).ok()?;
            let f = node.field(name)?;
            Some(r.start + f.value.span.start..r.start + f.value.span.end)
        });
        match written {
            Some(range) => edits.push((range, value)),
            None => {
                // Into the nearest object, after its last field.
                let Some(r) = objects.first() else { continue };
                let Ok(node) = tree::parse_value(&text[r.clone()]) else { continue };
                let Kind::Object { fields, open, .. } = &node.kind else { continue };
                let at = fields.last().map_or(r.start + open + 1, |f| r.start + f.span.end);
                let lead = if fields.is_empty() { "" } else { " " };
                edits.push((at..at, format!("{lead}{name}: {value}")));
            }
        }
    }
    edits
}

// ------------------------------------------------------------------ colours

/// A colour literal of the scene.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorSite {
    pub doc: usize,
    pub range: Range<usize>,
    pub rgba: [f64; 4],
    /// Its `/**tag*/`.
    pub tag: Option<String>,
    /// The `let` it is the whole value of (`let SIGNAL = #ff4d12`).
    pub let_name: Option<String>,
    /// Where the thing being edited reads it by name (`singing: SIGNAL` in
    /// `LYRIC_LOOK`): "this place only" writes the colour there instead.
    pub via: Option<Via>,
}

/// A read of a colour by name: the name's bytes and what the place is.
#[derive(Clone, Debug, PartialEq)]
pub struct Via {
    pub range: Range<usize>,
    pub label: String,
    /// The record field it is read into (`singing`), when it is one.
    pub field: Option<String>,
}

impl ColorSite {
    /// What the colour is called (its tag, else its `let`).
    fn name(&self) -> Option<String> {
        self.tag.clone().or_else(|| self.let_name.clone())
    }
}

/// The tag written just before a colour literal.
fn site_tag(text: &str, site: &Range<usize>) -> Option<String> {
    let body = text[..site.start].trim_end().strip_suffix("*/")?;
    let open = body.rfind("/**")?;
    let name = body[open + 3..].trim();
    (!name.is_empty() && !name.contains('\n')).then(|| name.to_string())
}

/// The `let` a colour literal is the whole value of (its tag aside).
fn site_let(text: &str, site: &Range<usize>) -> Option<String> {
    let line = text[..site.start].rfind('\n').map_or(0, |n| n + 1);
    let mut head = text[line..site.start].trim();
    if let Some(open) = head.rfind("/**") {
        head = head[..open].trim_end();
    }
    let name = head.strip_suffix('=')?.trim().strip_prefix("let ")?.trim();
    let rest = text[site.end..].lines().next().unwrap_or("").trim();
    ((rest.is_empty() || rest.starts_with("//")) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then(|| name.to_string())
}

/// Every colour literal of a file.
fn colour_sites(doc: usize, text: &str) -> Vec<ColorSite> {
    let mut out: Vec<ColorSite> = makepad_motion::palette::of(text)
        .into_iter()
        .flat_map(|s| s.sites.into_iter().map(move |r| (r, s.rgba)))
        .map(|(range, rgba)| ColorSite { doc, tag: site_tag(text, &range), let_name: site_let(text, &range), range, rgba, via: None })
        .collect();
    out.sort_by_key(|s| s.range.start);
    out
}

/// How a colour is written where `old` was: a hex stays a hex while it
/// fits one, a `color(…)` stays a call; light over 1 is always a call.
fn write_like(old: &str, rgba: [f64; 4]) -> String {
    let hdr = rgba[..3].iter().any(|c| *c > 1.0 + 1e-9);
    if old.trim_start().starts_with('#') || hdr {
        let written = format_code_color(rgba);
        // A hex that carried alpha keeps carrying it.
        if !hdr && old.trim().len() == 9 && written.len() == 7 {
            return format!("{written}ff");
        }
        return written;
    }
    let n = |v: f64| fmt_num((v * 1000.0).round() / 1000.0);
    if old.matches(',').count() >= 3 || rgba[3] < 1.0 - 1e-9 {
        format!("color({}, {}, {}, {})", n(rgba[0]), n(rgba[1]), n(rgba[2]), n(rgba[3]))
    } else {
        format!("color({}, {}, {})", n(rgba[0]), n(rgba[1]), n(rgba[2]))
    }
}

fn same(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
}

/// The places "everywhere" covers: every literal of the scene with the
/// same colour, and every one with the same tag.
fn everywhere_sites(session: &super::session::MotionSession, site: &ColorSite) -> Vec<ColorSite> {
    let mut out = Vec::new();
    for doc in 0..=session.modules.len() {
        let Some((_, text)) = file_text(session, doc) else { continue };
        for s in colour_sites(doc, text) {
            let tagged = site.tag.is_some() && s.tag == site.tag;
            if same(&s.rgba, &site.rgba) || tagged {
                out.push(s);
            }
        }
    }
    out
}

/// How many times a `let` is read in the scene's files.
fn reads_of(session: &super::session::MotionSession, name: &str) -> usize {
    let mut n = 0;
    for doc in 0..=session.modules.len() {
        let Some((_, text)) = file_text(session, doc) else { continue };
        n += name_reads(text, name);
    }
    n
}

/// The reads of name `name` in `text` (not its `let`, not a field).
fn name_reads(text: &str, name: &str) -> usize {
    let b = text.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    text.match_indices(name).filter(|(i, _)| {
        let before = if *i == 0 { b' ' } else { b[i - 1] };
        let after = b.get(i + name.len()).copied().unwrap_or(b' ');
        !word(before) && before != b'.' && !word(after) && !text[..*i].trim_end().ends_with("let")
    }).count()
}

/// How many places a colour reaches: its literals ("everywhere") and the
/// reads of the `let`s they are.
fn reach(session: &super::session::MotionSession, site: &ColorSite) -> usize {
    let sites = everywhere_sites(session, site);
    let mut lets: Vec<String> = sites.iter().filter_map(|s| s.let_name.clone()).collect();
    lets.sort();
    lets.dedup();
    sites.len() + lets.iter().map(|n| reads_of(session, n)).sum::<usize>()
}

/// The scene's palette as the picker lists it: each colour, what it is
/// called, and how many places use it.
fn palette_entries(session: &super::session::MotionSession) -> Vec<PaletteEntry> {
    let mut out: Vec<(ColorSite, usize)> = Vec::new();
    for doc in 0..=session.modules.len() {
        let Some((_, text)) = file_text(session, doc) else { continue };
        for s in colour_sites(doc, text) {
            if !out.iter().any(|(o, _)| same(&o.rgba, &s.rgba)) {
                let n = reach(session, &s);
                out.push((s, n));
            }
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out.into_iter().map(|(s, uses)| PaletteEntry { rgba: s.rgba, name: s.name().unwrap_or_default(), uses }).collect()
}

// ------------------------------------------------------------------ state

enum Target {
    Font {
        doc: usize,
        objects: Vec<Range<usize>>,
        choice: FontChoice,
    },
    Color {
        sites: Vec<ColorSite>,
        current: usize,
        everywhere: bool,
    },
}

/// Where a picker was opened from: found again in the code after every
/// change (the picker's own writes, undo, an edit elsewhere).
#[derive(Clone, Debug)]
enum Origin {
    /// A literal (an inspector field).
    Literal { doc: usize, range: Range<usize> },
}

/// A document loading on the preview worker: its serial and the texts it
/// is made of (file, text).
struct Job {
    serial: u64,
    source: String,
    modules: Vec<(String, String, String)>,
    /// Loaded as the desk's document is ([`super::checker::Check::film`]).
    film: Option<makepad_story_edit_render::graphic::FilmLoad>,
}

#[derive(Default)]
pub struct ValuePick {
    target: Option<Target>,
    origin: Option<Origin>,
    /// The session revision and each file's text the target was found in.
    revision: u64,
    texts: Vec<String>,
    jobs: Option<Sender<Job>>,
    /// Asks up to this serial were superseded (a write, a put-away): not shown.
    dropped: u64,
    done: Option<Receiver<(u64, Option<Arc<MotionDoc>>, f64)>>,
    serial: u64,
    /// The texts of each preview asked for, by serial (the last few).
    asked: Vec<(u64, Vec<(usize, String)>)>,
    /// The document shown instead of the graphic's, and the texts it is.
    shown: Option<(u64, Arc<MotionDoc>, Vec<(usize, String)>)>,
    /// Shown in the panel's head.
    what: String,
    open: bool,
    /// The panel is over the inspector.
    panel_shown: bool,
    /// A drag set in the running document as it moves (no load), and its
    /// steps so far and their time in ms (said when it is written).
    live: Option<Live>,
    live_steps: (u32, f64),
}

/// A drag shown by setting the running document in place: each step costs
/// the patch alone, as playback costs a frame (no load, no source write
/// until the release).
enum Live {
    Colour {
        /// The colour the running document holds now.
        now: [f64; 4],
        /// Each place it is written (file, bytes in the running text).
        literals: Vec<(usize, Range<usize>)>,
        /// What copied it as data while the document loaded.
        data: makepad_motion::schema::ColourData,
    },
    /// A font's numbers (weight, axes) written in place: each literal's
    /// file and bytes in the running text.
    Numbers { literals: Vec<(usize, Range<usize>)> },
    /// This gesture cannot be set in place: its steps load the edited code
    /// (a colour computed from at load, a field not written yet, light over
    /// 1).
    Load,
}

/// The id a preview is shown under (apart from the variants' ids).
const SHOWN_ID: u64 = 1 << 62;

/// The document the viewer shows instead of the graphic's while a value is
/// previewed.
pub fn shown_doc(page: &VideoCut) -> Option<(u64, Arc<MotionDoc>)> {
    page.motion.value_pick.shown.as_ref().map(|(serial, doc, _)| (SHOWN_ID | serial, doc.clone()))
}

fn start_worker(cx: &mut Cx, pick: &mut ValuePick) {
    if pick.jobs.is_some() {
        return;
    }
    let (job_tx, job_rx) = channel::<Job>();
    let (done_tx, done_rx) = channel();
    let options = ThreadOptions { name: Some(Arc::from("stage-value-preview")), stack_size: Some(8 * 1024 * 1024), priority: CxThreadPriority::UserInitiated };
    let handle = cx.thread_spawner().spawn_worker(options, move || {
        while let Ok(mut job) = job_rx.recv() {
            // Only the newest: a drag asks faster than a document loads.
            while let Ok(newer) = job_rx.try_recv() {
                job = newer;
            }
            let started = std::time::Instant::now();
            let (_, doc, _) = super::checker::check(DocKind::Motion, &job.source, job.modules, job.film.as_ref());
            if done_tx.send((job.serial, doc, started.elapsed().as_secs_f64() * 1000.0)).is_err() {
                return;
            }
        }
    });
    match handle {
        Ok(handle) => {
            handle.detach();
            pick.jobs = Some(job_tx);
            pick.done = Some(done_rx);
        }
        Err(error) => log!("value pick: no preview worker ({error:?})"),
    }
}

/// The scene's texts with `edits` (file, range, text) made.
pub(super) fn edited_texts(session: &super::session::MotionSession, edits: &[(usize, Range<usize>, String)]) -> Result<Vec<(usize, String)>, String> {
    let mut docs: Vec<usize> = edits.iter().map(|e| e.0).collect();
    docs.sort();
    docs.dedup();
    let mut out = Vec::new();
    for doc in docs {
        let (_, text) = file_text(session, doc).ok_or("that file is read only")?;
        let mut mine: Vec<&(usize, Range<usize>, String)> = edits.iter().filter(|e| e.0 == doc).collect();
        mine.sort_by_key(|e| std::cmp::Reverse(e.1.start));
        let mut next = text.to_string();
        for (_, range, value) in mine {
            if range.end > next.len() || !next.is_char_boundary(range.start) || !next.is_char_boundary(range.end) {
                return Err("the value moved: the source changed under it".into());
            }
            next.replace_range(range.clone(), value);
        }
        out.push((doc, next));
    }
    Ok(out)
}

/// The preview of `texts`: loaded on the worker, shown when it arrives.
pub(super) fn preview(page: &mut VideoCut, cx: &mut Cx, texts: Vec<(usize, String)>) {
    let Some(session) = &page.motion.desk.session else { return };
    if page.motion.value_pick.shown.as_ref().is_some_and(|s| s.2 == texts) {
        return;
    }
    let source = texts.iter().find(|(d, _)| *d == 0).map_or(session.source.clone(), |(_, t)| t.clone());
    let mut modules = session.edited_modules();
    for (doc, text) in texts.iter().filter(|(d, _)| *d > 0) {
        let m = &session.modules[doc - 1];
        modules.retain(|(id, _, _)| *id != m.id);
        modules.push((m.id.clone(), m.rev.clone(), text.clone()));
    }
    let film = super::desk::film(page);
    let pick = &mut page.motion.value_pick;
    start_worker(cx, pick);
    pick.serial += 1;
    let serial = pick.serial;
    pick.asked.push((serial, texts));
    if pick.asked.len() > 8 {
        pick.asked.remove(0);
    }
    if let Some(jobs) = &pick.jobs {
        let _ = jobs.send(Job { serial, source, modules, film });
    }
}

/// Whether `name` is computed from while a file loads: an argument of a
/// call in a top-level statement that is not a function (`let PEN =
/// mix(SIGNAL, …)`). Copied as data (`{sung: BONE}`, `[BONE, ASH]`, a
/// record handed to a call: `note({col: BONE})`) it is not: the copies are
/// found and set in place.
fn computed_at_load(text: &str, name: &str) -> bool {
    let b = text.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    // The brackets open here: `(` a call's arguments, `f` a function's body
    // (code that runs when called: each frame for a drawing function),
    // else a record or list.
    let mut open: Vec<u8> = Vec::new();
    let mut fn_next = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'f' if text[i..].starts_with("fn") && (i == 0 || !word(b[i - 1])) && !b.get(i + 2).is_some_and(|c| word(*c)) => fn_next = true,
            b'{' => {
                open.push(if fn_next { b'f' } else { b'{' });
                fn_next = false;
            }
            b'[' | b'(' => open.push(c),
            b'}' | b']' | b')' => {
                open.pop();
            }
            _ if open.last() == Some(&b'(') && !open.contains(&b'f') && text[i..].starts_with(name) && (i == 0 || !word(b[i - 1])) && !b.get(i + name.len()).is_some_and(|c| word(*c)) => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn code_file(session: &super::session::MotionSession, doc: usize) -> Option<makepad_motion::schema::CodeFile> {
    if doc == 0 {
        Some(makepad_motion::schema::CodeFile::Doc)
    } else {
        session.modules.get(doc - 1).map(|m| makepad_motion::schema::CodeFile::Module(m.key.clone()))
    }
}

fn f32s(c: [f64; 4]) -> [f32; 4] {
    c.map(|v| v as f32)
}

/// One step of a recolour drag, in place: each literal the colour is
/// written at (file, bytes in the running text) and the shaders compiled
/// from it set to `now`, and the data it was copied into while the
/// document loaded (`data`: [`makepad_motion::MotionDoc::colour_data`] of
/// `was`, found once per gesture) moved from `was` to `now`. The next frame
/// draws it; nothing loads, nothing is written. How many places changed
/// (none: the running document cannot take it; load instead). The palette
/// strip's recolour and the colour picker both step through this.
pub(super) fn recolour_running(session: &super::session::MotionSession, running: &makepad_motion::MotionDoc, cx: &mut Cx, literals: &[(usize, Range<usize>)], data: &makepad_motion::schema::ColourData, was: [f64; 4], now: [f64; 4]) -> usize {
    let mut changed = 0;
    for (doc, range) in literals {
        let (Some(file), Some((_, text))) = (code_file(session, *doc), file_text(session, *doc)) else { continue };
        // Every place the literal landed (`Cx::patch_literal`); the copies a
        // load made of the colour are `data`, moved below.
        changed += makepad_motion::schema::site_value(&file, text, range.clone(), makepad_motion::schema::Literal::Color(f32s(now))).map_or(0, |(site, value)| cx.patch_literal(&site, value).places());
    }
    changed + running.recolour_data(data, f32s(was), f32s(now))
}

/// The colour being dragged set in the running document; false when this
/// gesture cannot be set in place (its steps load instead).
fn live_colour(page: &mut VideoCut, cx: &mut Cx, rgba: [f64; 4]) -> bool {
    if matches!(page.motion.value_pick.live, Some(Live::Load)) {
        return false;
    }
    let Some(session) = &page.motion.desk.session else { return false };
    let Some(running) = session.doc.clone().filter(|_| session.doc_of == session.revision) else {
        log!("value pick: the running document is not the source's yet; this step loads");
        return false;
    };
    if page.motion.value_pick.live.is_none() {
        let Some(Target::Color { sites, current, everywhere }) = &page.motion.value_pick.target else { return false };
        let Some(site) = sites.get(*current).cloned() else { return false };
        let everywhere = *everywhere;
        let start = || -> Result<Live, String> {
            let places = match (&site.via, everywhere) {
                // This record's field only: the colour it copied.
                (Some(via), false) => {
                    let field = via.field.as_deref().ok_or("the place reads it outside a record field")?;
                    return Ok(Live::Colour { now: site.rgba, literals: Vec::new(), data: running.colour_data(f32s(site.rgba), Some(field)) });
                }
                (_, true) => everywhere_sites(session, &site),
                _ => vec![site.clone()],
            };
            let mut lets = Vec::new();
            for p in &places {
                let (_, text) = file_text(session, p.doc).ok_or("a read-only file")?;
                // A colour call (`color(…)`) is code, not a colour value.
                if !text.get(p.range.clone()).is_some_and(|t| t.trim_start().starts_with('#')) {
                    return Err("a place writes it as color(…)".into());
                }
                lets.extend(p.let_name.clone());
            }
            for doc in 0..=session.modules.len() {
                let Some((_, text)) = file_text(session, doc) else { continue };
                if let Some(n) = lets.iter().find(|n| computed_at_load(text, n)) {
                    return Err(format!("{n} is computed from while the scene loads"));
                }
            }
            // The data it was copied into: when no other place writes the
            // same colour, every copy is this one's.
            let data = if everywhere || everywhere_sites(session, &site).len() <= 1 { running.colour_data(f32s(site.rgba), None) } else { Vec::new() };
            Ok(Live::Colour { now: site.rgba, literals: places.iter().map(|p| (p.doc, p.range.clone())).collect(), data })
        };
        let live = start().unwrap_or_else(|why| {
            log!("value pick: this colour drag loads each step: {why}");
            Live::Load
        });
        page.motion.value_pick.live = Some(live);
    }
    // Light over 1 is a colour call, not a colour value.
    if rgba[..3].iter().any(|c| *c > 1.0) {
        page.motion.value_pick.live = Some(Live::Load);
        return false;
    }
    let Some(Live::Colour { now, literals, data }) = &page.motion.value_pick.live else { return false };
    let started = std::time::Instant::now();
    let changed = recolour_running(session, &running, cx, literals, data, *now, rgba);
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    page.motion.value_pick.live_steps.0 += 1;
    page.motion.value_pick.live_steps.1 += ms;
    if changed == 0 {
        log!("value pick: the running document took none of the colour's places; this drag loads each step");
        page.motion.value_pick.live = Some(Live::Load);
        return false;
    }
    if let Some(Live::Colour { now, .. }) = &mut page.motion.value_pick.live {
        *now = rgba;
    }
    super::desk::live_shown(page);
    true
}

/// A font's numbers being dragged (weight, an axis) set in the running
/// document where they are written; false when a step writes something
/// else (a field not written yet: its steps load).
fn live_numbers(page: &mut VideoCut, cx: &mut Cx, edits: &[(usize, Range<usize>, String)]) -> bool {
    if matches!(page.motion.value_pick.live, Some(Live::Load)) {
        return false;
    }
    let Some(session) = &page.motion.desk.session else { return false };
    // (A running document as loaded from this text: the patches' sink.)
    if session.doc.is_none() || session.doc_of != session.revision {
        return false;
    }
    // Each edit a number replacing a number, or (an axes record written
    // again) the record's numbers one by one.
    let mut steps: Vec<(usize, Range<usize>, f64)> = Vec::new();
    for (doc, range, text) in edits {
        let Some((_, src)) = file_text(session, *doc) else { return false };
        let Some(old) = src.get(range.clone()) else { return false };
        if let (Ok(_), Ok(v)) = (old.trim().parse::<f64>(), text.trim().parse::<f64>()) {
            steps.push((*doc, range.clone(), v));
            continue;
        }
        // `{wdth: 100 opsz: 14}` again: the same keys, the numbers moved.
        let (Ok(a), Ok(b)) = (tree::parse_value(old), tree::parse_value(text)) else { return false };
        let (Some((_, fa)), Some((_, fb))) = (a.object(), b.object()) else { return false };
        if fa.len() != fb.len() {
            return false;
        }
        for (x, y) in fa.iter().zip(fb) {
            let (Some(vx), Some(vy)) = (x.value.number(), y.value.number()) else { return false };
            if x.name != y.name {
                return false;
            }
            if vx != vy {
                steps.push((*doc, range.start + x.value.span.start..range.start + x.value.span.end, vy));
            }
        }
    }
    if page.motion.value_pick.live.is_none() {
        page.motion.value_pick.live = Some(Live::Numbers { literals: Vec::new() });
    }
    let started = std::time::Instant::now();
    let mut changed = 0;
    for (doc, range, v) in &steps {
        let (Some(file), Some((_, text))) = (code_file(session, *doc), file_text(session, *doc)) else { continue };
        // Every place the literal landed (`Cx::patch_literal`), unless the
        // load also copied or computed with it: then it loads.
        changed += makepad_motion::schema::site_value(&file, text, range.clone(), makepad_motion::schema::Literal::Number(*v)).map_or(0, |(site, value)| {
            let reach = cx.patch_literal(&site, value);
            if reach.needs_reload() { 0 } else { reach.places() }
        });
    }
    page.motion.value_pick.live_steps.0 += 1;
    page.motion.value_pick.live_steps.1 += started.elapsed().as_secs_f64() * 1000.0;
    if changed == 0 && !steps.is_empty() {
        page.motion.value_pick.live = Some(Live::Load);
        return false;
    }
    if let Some(Live::Numbers { literals }) = &mut page.motion.value_pick.live {
        for (doc, range, _) in steps {
            if !literals.contains(&(doc, range.clone())) {
                literals.push((doc, range));
            }
        }
    }
    super::desk::live_shown(page);
    true
}

/// A live gesture put away without being written: the running document's
/// values set back from its source (a load), so the picture is the code.
fn drop_live(page: &mut VideoCut) {
    page.motion.value_pick.live_steps = (0, 0.0);
    let Some(live) = page.motion.value_pick.live.take() else { return };
    if !matches!(live, Live::Load) {
        super::desk::reload_running(page);
    }
}

/// The preview put away: the graphic as it is shows again.
pub(super) fn unpreview(page: &mut VideoCut) {
    drop_live(page);
    let pick = &mut page.motion.value_pick;
    pick.serial += 1;
    pick.dropped = pick.serial;
    if pick.shown.take().is_some() {
        page.motion.desk.repaint = true;
    }
}

/// A held gesture written as `texts`: the preview its steps loaded is
/// shown as the graphic at once when it is that text (the checker loads it
/// again behind it for the timeline); any other preview is put away.
pub(super) fn adopt(page: &mut VideoCut, texts: &[(usize, String)]) {
    let shown = page.motion.value_pick.shown.take();
    page.motion.value_pick.serial += 1;
    page.motion.value_pick.dropped = page.motion.value_pick.serial;
    if let (Some((_, doc, shown_texts)), Some(session)) = (shown, page.motion.desk.session.as_mut()) {
        if shown_texts == texts {
            session.doc = Some(doc);
        }
        page.motion.desk.repaint = true;
    }
}

/// The edits written as one undo step; true when something changed.
pub(super) fn write(page: &mut VideoCut, texts: Vec<(usize, String)>, what: &str) -> bool {
    // A live gesture ends in its write: the running document shows it
    // already, and loads the written code behind.
    if page.motion.value_pick.live.take().is_some() {
        let (n, ms) = std::mem::take(&mut page.motion.value_pick.live_steps);
        if n > 0 {
            log!("value pick: {n} live steps, {:.2} ms each (set in place, no load)", ms / n as f64);
        }
    }
    let shown = page.motion.value_pick.shown.take();
    page.motion.value_pick.serial += 1;
    page.motion.value_pick.dropped = page.motion.value_pick.serial;
    let Some(session) = &mut page.motion.desk.session else { return false };
    let burst = format!("value-pick:{}", Cx::time_now().to_bits());
    let mut changed = false;
    for (doc, text) in &texts {
        if *doc == 0 {
            changed |= session.replace_burst(text.clone(), what, &burst);
        } else {
            let key = session.modules[doc - 1].key.clone();
            changed |= session.replace_module(&key, text.clone(), what, Some(&burst));
        }
    }
    session.end_burst();
    // The document the preview loaded is the one written: shown at once
    // (the checker loads it again behind it for the timeline).
    if let Some((_, doc, shown_texts)) = shown {
        if shown_texts == texts {
            session.doc = Some(doc);
        }
    }
    page.motion.desk.author = Some("You · picker".into());
    if changed {
        super::desk::changed(page);
    }
    page.motion.desk.repaint = true;
    changed
}

/// A range moved by edits made before it in its file.
fn shifted(r: &Range<usize>, doc: usize, edits: &[(usize, Range<usize>, String)]) -> Range<usize> {
    let mut start = r.start as isize;
    let mut end = r.end as isize;
    for (d, e, text) in edits {
        if *d != doc {
            continue;
        }
        let delta = text.len() as isize - e.len() as isize;
        if e.end <= r.start {
            start += delta;
            end += delta;
        } else if e.start >= r.start && e.end <= r.end {
            end += delta;
        }
    }
    start.max(0) as usize..end.max(start) as usize
}

// ------------------------------------------------------------------ opening

/// Open the picker for the literal at `range` of file `doc` (an inspector
/// field): a colour, or a font's family (its `Font{…}`).
pub fn open_literal(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, doc: usize, range: Range<usize>, label: &str) -> bool {
    open(page, cx, desk_ref, Origin::Literal { doc, range }, label.to_string())
}

/// Open the picker for an inspector field (`code:<doc>:<literal>`, or
/// `site:<index>` of the document); false when it is no font or colour.
pub fn open_field(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, id: &str, label: &str) -> bool {
    let Some(session) = &page.motion.desk.session else { return false };
    let place = if let Some(rest) = id.strip_prefix("code:") {
        rest.split_once(':').and_then(|(d, l)| {
            let (doc, lit) = (d.parse::<usize>().ok()?, l.parse::<usize>().ok()?);
            let (code, _) = file_text(session, doc)?;
            Some((doc, code.literals.get(lit)?.range.clone()))
        })
    } else if let Some(i) = id.strip_prefix("site:") {
        i.parse::<usize>().ok().and_then(|i| Some((0, session.sites.get(i)?.range.clone())))
    } else {
        None
    };
    let Some((doc, range)) = place else { return false };
    open_literal(page, cx, desk_ref, doc, range, label)
}

/// The value written from byte `at` (as far as it parses).
fn node_at(text: &str, at: usize) -> Option<Node> {
    // The value runs to its closing bracket: find it by parsing growing
    // prefixes at its brackets' ends.
    let rest = &text[at..];
    let mut depth = 0i32;
    for (i, c) in rest.char_indices() {
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => {
                depth -= 1;
                if depth == 0 {
                    let node = tree::parse_value(&rest[..=i]).ok()?;
                    return Some(Node { span: at + node.span.start..at + node.span.end, kind: node.kind });
                }
            }
            _ => {}
        }
    }
    None
}

fn open(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, origin: Origin, what: String) -> bool {
    let Some(target) = page.motion.desk.session.as_ref().and_then(|s| resolve(s, &origin)) else { return false };
    unpreview(page);
    let session = page.motion.desk.session.as_ref().expect("resolved in it");
    let pick = &mut page.motion.value_pick;
    pick.revision = session.revision;
    pick.texts = texts_of(session);
    pick.origin = Some(origin);
    pick.target = Some(target);
    pick.what = what;
    pick.open = true;
    pick.panel_shown = true;
    show(page, cx, desk_ref, true);
    true
}

pub fn close(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef) {
    unpreview(page);
    page.motion.value_pick.target = None;
    page.motion.value_pick.origin = None;
    page.motion.value_pick.open = false;
    page.motion.value_pick.panel_shown = false;
    desk_ref.widget(cx, ids!(m_value_pick)).set_visible(cx, false);
    desk_ref.widget(cx, ids!(m_inspector_box)).set_visible(cx, true);
}

/// The panel set from the target (`all`: the picker's value too).
fn show(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, all: bool) {
    let panel = desk_ref.widget(cx, ids!(m_value_pick));
    let Some(session) = &page.motion.desk.session else { return };
    let sample = super::desk::picked_words(page);
    let pick = &page.motion.value_pick;
    let Some(target) = &pick.target else { return };
    panel.set_visible(cx, true);
    desk_ref.widget(cx, ids!(m_inspector_box)).set_visible(cx, false);
    panel.widget(cx, ids!(vp_what)).set_text(cx, &pick.what);
    match target {
        Target::Font { doc, objects, choice, .. } => {
            panel.widget(cx, ids!(vp_kind)).set_text(cx, "FONT");
            panel.widget(cx, ids!(vp_font)).set_visible(cx, true);
            panel.widget(cx, ids!(vp_color)).set_visible(cx, false);
            let text = file_text(session, *doc).map(|(_, t)| t).unwrap_or("");
            let lines: Vec<String> = objects.iter().map(|r| format!("line {}: {}", text[..r.start].matches('\n').count() + 1, text[r.clone()].chars().take(56).collect::<String>())).collect();
            panel.widget(cx, ids!(vp_where)).set_text(cx, &format!("Written at {}", lines.join(" ← ")));
            if all {
                if let Some(mut picker) = panel.widget(cx, ids!(vp_font)).borrow_mut::<FontPicker>() {
                    picker.set_value(cx, choice.clone(), &sample);
                }
            }
        }
        Target::Color { sites, current, everywhere } => {
            panel.widget(cx, ids!(vp_kind)).set_text(cx, "COLOUR");
            panel.widget(cx, ids!(vp_font)).set_visible(cx, false);
            panel.widget(cx, ids!(vp_color)).set_visible(cx, true);
            let Some(site) = sites.get(*current).cloned() else { return };
            let text = file_text(session, site.doc).map(|(_, t)| t).unwrap_or("");
            let line = text[..site.range.start.min(text.len())].matches('\n').count() + 1;
            let file = if site.doc == 0 { session.title.clone() } else { session.modules[site.doc - 1].name.clone() };
            let reach = reach(session, &site);
            let name = site.name().map(|n| format!("{n} · ")).unwrap_or_default();
            let written = format!("{name}{} · {file} line {line}", text.get(site.range.clone()).unwrap_or(""));
            let place = match (&site.via, *everywhere) {
                (Some(via), false) => format!("{written}\nThis place only: {} (it reads {}); the colour stays as it is in its {} other places.", via.label, site.let_name.clone().unwrap_or_default(), reach.saturating_sub(1)),
                _ if reach > 1 => format!("{written}\nEverywhere: {reach} places use this colour."),
                _ => written,
            };
            panel.widget(cx, ids!(vp_where)).set_text(cx, &place);
            let sources: Vec<PaletteEntry> = sites.iter().map(|s| PaletteEntry { rgba: s.rgba, name: s.via.as_ref().map(|v| format!("{} ← {}", v.label, s.name().unwrap_or_default())).or_else(|| s.name()).unwrap_or_default(), uses: 0 }).collect();
            let palette = palette_entries(session);
            // "This place only" exists when the colour is read by name here,
            // or written in more than one place.
            let places = if site.via.is_some() { reach.max(2) } else { everywhere_sites(session, &site).len() };
            let tag = site.name();
            let (current, everywhere) = (*current, *everywhere);
            if let Some(mut picker) = panel.widget(cx, ids!(vp_color)).borrow_mut::<HdrColorPicker>() {
                if all {
                    picker.set_value(cx, site.rgba);
                }
                picker.set_sources(cx, sources, current);
                picker.set_palette(cx, palette);
                picker.set_everywhere(cx, everywhere, places, tag.as_deref());
                // Light over 1 where the colour is written as one already
                // (`color(…)`): a #hex is read as a plain colour (its
                // `.r .g .b`) by the scene's colour helpers.
                let hdr = !(site.via.is_some() && !everywhere) && text.get(site.range.clone()).is_some_and(|t| t.trim_start().starts_with("color("));
                picker.set_hdr_allowed(cx, hdr);
            }
        }
    }
}

/// The colour target's edits for `rgba`: this place, or every place.
fn colour_edits(page: &VideoCut, rgba: [f64; 4]) -> Vec<(usize, Range<usize>, String)> {
    let Some(session) = &page.motion.desk.session else { return Vec::new() };
    let Some(Target::Color { sites, current, everywhere }) = &page.motion.value_pick.target else { return Vec::new() };
    let Some(site) = sites.get(*current) else { return Vec::new() };
    // This place only, where the thing reads the colour by name: the
    // colour written there instead of the name.
    if let (Some(via), false) = (&site.via, *everywhere) {
        return vec![(site.doc, via.range.clone(), format_code_color(rgba))];
    }
    let places = if *everywhere { everywhere_sites(session, site) } else { vec![site.clone()] };
    places.iter().filter_map(|s| {
        let (_, text) = file_text(session, s.doc)?;
        let old = text.get(s.range.clone())?;
        Some((s.doc, s.range.clone(), write_like(old, rgba)))
    }).collect()
}

/// The font target's edits for `to`.
fn font_target_edits(page: &VideoCut, to: &FontChoice) -> Vec<(usize, Range<usize>, String)> {
    let Some(session) = &page.motion.desk.session else { return Vec::new() };
    let Some(Target::Font { doc, objects, choice, .. }) = &page.motion.value_pick.target else { return Vec::new() };
    let Some((_, text)) = file_text(session, *doc) else { return Vec::new() };
    font_edits(text, objects, choice, to).into_iter().map(|(r, t)| (*doc, r, t)).collect()
}

/// Each file's text (document first), as the target is found in.
fn texts_of(session: &super::session::MotionSession) -> Vec<String> {
    std::iter::once(session.source.clone()).chain(session.modules.iter().map(|m| m.source.clone())).collect()
}

/// What `origin` comes down to in the code as it is now.
fn resolve(session: &super::session::MotionSession, origin: &Origin) -> Option<Target> {
    match origin {
        Origin::Literal { doc, range } => {
            let (_, text) = file_text(session, *doc)?;
            if let Some(site) = colour_sites(*doc, text).into_iter().find(|s| s.range.start <= range.start && range.start < s.range.end) {
                return Some(Target::Color { sites: vec![site], current: 0, everywhere: false });
            }
            // A family string: the `Font{…}` holding it.
            let mut start = range.start.min(text.len());
            while let Some(found) = text[..start].rfind("Font{") {
                if let Some(node) = node_at(text, found) {
                    if node.span.end >= range.end {
                        let objects = vec![found..node.span.end];
                        let choice = font_choice(text, &objects);
                        return Some(Target::Font { doc: *doc, objects, choice });
                    }
                }
                start = found;
            }
            None
        }
    }
}

/// The edits that made `now` from `then`, per file, as one splice each
/// (the common start and end kept).
fn diff_edits(then: &[String], now: &[String]) -> Vec<(usize, Range<usize>, String)> {
    let mut out = Vec::new();
    for (doc, (a, b)) in then.iter().zip(now).enumerate() {
        if a == b {
            continue;
        }
        let p = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
        let q = a.bytes().rev().zip(b.bytes().rev()).take_while(|(x, y)| x == y).count().min(a.len() - p).min(b.len() - p);
        out.push((doc, p..a.len() - q, b[p..b.len() - q].to_string()));
    }
    out
}

/// The target found again after `edits` (the picker's own write, or a
/// change elsewhere): its origin moved with them, the colour edited kept
/// as the one edited (`focus`: its file and first byte now), and whether
/// edits reach everywhere kept. False when it is gone.
fn refind(page: &mut VideoCut, edits: &[(usize, Range<usize>, String)], focus: Option<(usize, usize)>) -> bool {
    let Some(session) = &page.motion.desk.session else { return false };
    let pick = &mut page.motion.value_pick;
    let Some(origin) = pick.origin.clone() else { return false };
    let origin = match origin {
        Origin::Literal { doc, range } => Origin::Literal { doc, range: shifted(&range, doc, edits) },
    };
    let (was_current, everywhere_then) = match &pick.target {
        Some(Target::Color { sites, current, everywhere }) => (sites.get(*current).map(|s| (s.doc, shifted(&s.range, s.doc, edits).start)), *everywhere),
        _ => (None, false),
    };
    let Some(mut target) = resolve(session, &origin) else { return false };
    if let Target::Color { sites, current, everywhere } = &mut target {
        *everywhere = everywhere_then;
        let want = focus.or(was_current);
        if let Some(i) = want.and_then(|(d, at)| sites.iter().position(|s| s.doc == d && s.range.start == at)) {
            *current = i;
        }
    }
    pick.origin = Some(origin);
    pick.target = Some(target);
    pick.revision = session.revision;
    pick.texts = texts_of(session);
    true
}

// ------------------------------------------------------------------ events

pub fn handle_actions(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, actions: &Actions) {
    if !page.motion.value_pick.open {
        return;
    }
    let panel = desk_ref.widget(cx, ids!(m_value_pick));
    if panel.widget(cx, ids!(vp_done)).as_button().clicked(actions) {
        close(page, cx, desk_ref);
        return;
    }
    let font_uid = panel.widget(cx, ids!(vp_font)).widget_uid();
    for action in actions.filter_widget_actions_cast::<FontPickAction>(font_uid) {
        match action {
            FontPickAction::Preview(choice) => {
                let edits = font_target_edits(page, &choice);
                if live_numbers(page, cx, &edits) {
                    continue;
                }
                let texts = page.motion.desk.session.as_ref().map(|s| edited_texts(s, &edits));
                match texts {
                    Some(Ok(texts)) if !texts.is_empty() => preview(page, cx, texts),
                    Some(Ok(_)) => unpreview(page),
                    _ => {}
                }
            }
            FontPickAction::Commit(choice) => {
                let edits = font_target_edits(page, &choice);
                let texts = page.motion.desk.session.as_ref().map(|s| edited_texts(s, &edits));
                match texts {
                    Some(Ok(texts)) if !texts.is_empty() => {
                        let what = format!("font {} {}", choice.family, choice.weight as i64);
                        if write(page, texts, &what) && !refind(page, &edits, None) {
                            close(page, cx, desk_ref);
                            return;
                        }
                    }
                    Some(Err(error)) => super::desk::set_status(page, format!("Refused: {error}")),
                    _ => {}
                }
                show(page, cx, desk_ref, false);
            }
            FontPickAction::None => {}
        }
    }
    let color_uid = panel.widget(cx, ids!(vp_color)).widget_uid();
    for action in actions.filter_widget_actions_cast::<ColorPickAction>(color_uid) {
        match action {
            ColorPickAction::Preview(rgba) => {
                if live_colour(page, cx, rgba) {
                    continue;
                }
                let edits = colour_edits(page, rgba);
                if let Some(Ok(texts)) = page.motion.desk.session.as_ref().map(|s| edited_texts(s, &edits)) {
                    preview(page, cx, texts);
                }
            }
            ColorPickAction::Commit(rgba) => {
                let edits = colour_edits(page, rgba);
                match page.motion.desk.session.as_ref().map(|s| edited_texts(s, &edits)) {
                    Some(Ok(texts)) if !texts.is_empty() => {
                        let what = format!("colour {}", format_code_color(rgba));
                        // The colour edited is where the first edit wrote it.
                        let focus = edits.first().map(|(d, r, _)| (*d, shifted(r, *d, &edits).start));
                        if write(page, texts, &what) && !refind(page, &edits, focus) {
                            close(page, cx, desk_ref);
                            return;
                        }
                    }
                    Some(Err(error)) => super::desk::set_status(page, format!("Refused: {error}")),
                    _ => {}
                }
                show(page, cx, desk_ref, false);
            }
            ColorPickAction::Everywhere(on) => {
                if let Some(Target::Color { everywhere, .. }) = &mut page.motion.value_pick.target {
                    *everywhere = on;
                }
                show(page, cx, desk_ref, false);
            }
            ColorPickAction::Source(i) => {
                unpreview(page);
                if let Some(Target::Color { current, sites, .. }) = &mut page.motion.value_pick.target {
                    if i < sites.len() {
                        *current = i;
                    }
                }
                show(page, cx, desk_ref, true);
            }
            // A palette pick: its Commit follows.
            ColorPickAction::Palette(_) | ColorPickAction::None => {}
        }
    }
}

/// Every pump: the panel follows whether a value is being picked (a
/// graphic closed or another opened closes it), and a preview that loaded
/// is shown.
pub fn pump(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef) {
    if page.motion.desk.session.is_none() && page.motion.value_pick.open {
        page.motion.value_pick.target = None;
        page.motion.value_pick.open = false;
        page.motion.value_pick.shown = None;
    }
    if !page.motion.value_pick.open && page.motion.value_pick.panel_shown {
        close(page, cx, desk_ref);
    }
    // The code changed under the picker (undo, an edit elsewhere): the
    // value found again, or the picker closed when it is gone.
    if page.motion.value_pick.open {
        let moved = page.motion.desk.session.as_ref().filter(|s| s.revision != page.motion.value_pick.revision).map(texts_of);
        if let Some(now) = moved {
            let edits = diff_edits(&page.motion.value_pick.texts, &now);
            unpreview(page);
            if refind(page, &edits, None) {
                show(page, cx, desk_ref, true);
            } else {
                close(page, cx, desk_ref);
            }
        }
    }
    let pick = &mut page.motion.value_pick;
    let Some(done) = &pick.done else { return };
    let mut last = None;
    while let Ok(loaded) = done.try_recv() {
        last = Some(loaded);
    }
    let Some((serial, doc, ms)) = last else { return };
    // A newer ask than the one shown counts (a drag asks faster than a
    // document loads: each arrival is shown); one asked before a write or
    // a put-away does not.
    if serial <= pick.dropped || pick.shown.as_ref().is_some_and(|(s, _, _)| *s >= serial) {
        return;
    }
    let texts = pick.asked.iter().find(|(s, _)| *s == serial).map(|(_, t)| t.clone()).unwrap_or_default();
    match doc {
        Some(doc) => {
            pick.shown = Some((serial, doc, texts));
            log!("value pick: preview {serial} loaded in {ms:.0} ms");
        }
        None => super::desk::set_status(page, "That value does not load: the picture keeps the last one that did"),
    }
    page.motion.desk.repaint = true;
}

//! The scene's editor panel: a small Splash program the scene's agent (or
//! Generate, or a person) writes into the scene's own document,
//! `let editor_panel = fn() { View{ … } }`, which Motion never calls. The
//! desk runs it in a `Splash` isolate (its own VM, sandboxed: no files, no
//! processes, no network) beside the picture, so it shows at once when the
//! scene opens.
//!
//! The program reaches the scene's code only through the `mod.scene` module
//! the isolate is given:
//! - `scene.read(path)`: the value written at a path (`WORD_SPARKS.count`,
//!   `LYRIC_LOOK.font`, `camera_keys[3].zoom`, `shapes.unicorn_crude`, a
//!   tagged expression): a number, a bool, or the text as written (a
//!   colour `#rrggbb`, `@name`, a `Font{…}`, a string's words);
//! - `scene.set(path, value)`: a new value there. While a finger is down
//!   in the panel the running film takes it (where it can), and the release
//!   writes it into the code, one undo step per gesture, like typing;
//! - `scene.swap(path, name)`: a function slot names another function;
//! - `scene.replace(path, code)`: new code for the expression at a path
//!   (a generator: a motion, a pattern), refused when it does not parse.
//!
//! Paths are resolved by name each time (`makepad_motion::paths`), so a
//! panel survives edits of the code around what it binds; one that no
//! longer resolves says so, and the agent can update it.
//!
//! The runtime (the isolate's `scene`, the panel widgets' host, gestures)
//! is `makepad_editor_panel`, shared with the SCORE page; this is the
//! graphic desk's side: the scene's files in, the panel mounted, the edits
//! applied through the desk's code edits, the control API's driven gesture.
//!
//! Main-loop cost: the panel's program runs in its isolate on its own
//! events; a `set` queues an edit the desk applies on its next pump (a
//! resolve: a scan of the `let` it names). The scene's texts are handed to
//! the isolate's `read` only when they change.

use super::super::VideoCut;
use makepad_motion::paths;
use makepad_editor_panel as panel;
use makepad_strict_json::Value as Json;
use makepad_edit_values::Edit;
use makepad_widgets::*;

/// The panel's program as the scene writes it (the variants and the chat ask).
pub use makepad_editor_panel::body_of;

/// Who holds the panel widgets' host (SCORE is the other).
const OWNER: &str = "graphic";

/// The panel's state on the page.
#[derive(Default)]
pub struct Panel {
    runtime: panel::Runtime,
    /// The scene revision the isolate reads.
    scene_rev: Option<u64>,
    /// The gesture under way (its first step moves the playhead to where
    /// the control acts).
    gesture: Option<u64>,
    /// A gesture the control API plays on a path, step by step, timing
    /// each step from its value set to the picture showing it.
    pub drive: Option<Drive>,
    /// The lit state the panel was last drawn for.
    lit_drawn: u64,
    /// The model and revision the `when:`s were read for.
    when_of: Option<(u64, u64)>,
    /// The playhead the panel was last drawn at.
    playhead_drawn: f64,
    /// A control's finished write, checked once the scene loaded it:
    /// (revision, its path).
    verify: Option<(u64, String)>,
}

#[derive(Default)]
pub struct Drive {
    pub path: String,
    pub values: std::collections::VecDeque<f64>,
    pub steps: Vec<DriveStep>,
    pub done: bool,
}

pub struct DriveStep {
    pub value: f64,
    queued: f64,
    /// The scene's revision before the step and after it: the same when the
    /// running film took the value (live), a new one when it reloads.
    rev_before: u64,
    rev_after: u64,
    pub reload: bool,
    pub shown_ms: Option<f64>,
}

/// One step of a driven gesture: shown yet, and the next one set.
fn drive_step(page: &mut VideoCut, cx: &mut Cx) {
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let (revision, doc_of) = (session.revision, session.doc_of);
    let Some(drive) = page.motion.panel.drive.as_mut().filter(|d| !d.done) else { return };
    cx.new_next_frame();
    if let Some(last) = drive.steps.last_mut().filter(|s| s.shown_ms.is_none()) {
        // A live step drew in the frame after it was set; a reload, once
        // the scene was rebuilt for its revision (and drawn).
        let shown = if last.reload { doc_of == last.rev_after && revision == last.rev_after } else { true };
        if !shown {
            return;
        }
        last.shown_ms = Some((Cx::time_now() - last.queued) * 1000.0);
    }
    match drive.values.pop_front() {
        Some(value) => {
            panel::press();
            panel::queue(panel::Edit::Set { path: drive.path.clone(), value: Json::F64(value) });
            drive.steps.push(DriveStep { value, queued: Cx::time_now(), rev_before: revision, rev_after: revision, reload: false, shown_ms: None });
        }
        None => {
            panel::release();
            drive.done = true;
        }
    }
}

/// After the step's edit went through: whether the film took it live.
fn drive_applied(page: &mut VideoCut) {
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let revision = session.revision;
    if let Some(step) = page.motion.panel.drive.as_mut().and_then(|d| d.steps.last_mut()).filter(|s| s.shown_ms.is_none()) {
        step.rev_after = revision;
        step.reload = revision != step.rev_before;
    }
}

/// Every pump while a graphic is open: the scene the isolate reads, the
/// panel mounted (or taken down), the program's edits applied.
pub fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    let panel_ref = desk_ref.widget(cx, ids!(m_editor_panel));
    let Some(session) = page.motion.desk.session.as_ref() else {
        if page.motion.panel.scene_rev.take().is_some() {
            page.motion.panel.runtime.mount(cx, &panel_ref, None);
            page.motion.panel.runtime.unmount();
            panel_ref.set_visible(cx, false);
        }
        return;
    };
    // The texts `read` resolves against, when they change (or when the
    // panel comes back from SCORE); the panel widgets read again.
    let claimed = panel::claim(OWNER);
    if claimed {
        // A control's AI button and the program's `scene.ask` reach the
        // scene's agent (the variants desk runs them).
        // So do the instruments' reads of the running film (playhead, live
        // values, the song, its beats and words: `panel_feed.rs`).
        panel::install_host(super::panel_feed::hooks);
    }
    if claimed || page.motion.panel.scene_rev != Some(session.revision) {
        page.motion.panel.scene_rev = Some(session.revision);
        let files: Vec<(String, String, makepad_motion::schema::code::CodeOutline)> = std::iter::once((String::new(), session.source.clone(), session.code.clone()))
            .chain(session.modules.iter().filter(|m| m.id != "builtin").map(|m| (m.name.clone(), m.source.clone(), m.code.clone())))
            .collect();
        makepad_value_pickers::panel::host::timed("ep.set_files", || panel::set_files(files));
        panel::changed();
        // (The selected text's own controls read the new values too.)
        desk_ref.widget(cx, ids!(m_selected_text)).widget(cx, ids!(t_style)).redraw(cx);
    }
    // The program as written; mounted when it changes (shown only when
    // the scene has one).
    let body = makepad_value_pickers::panel::host::timed("ep.body_of", || panel::body_of(&session.source, &session.code));
    // A scene that does not read or load right now (a typing in progress,
    // an edit that broke it) keeps the panel it had: only a scene that
    // loads without one takes it down.
    let broken = session.outline_error.is_some() || !session.problems.is_empty() || session.doc_of != session.revision;
    let body = match body {
        None if broken => page.motion.panel.runtime.mounted_body(),
        body => body,
    };
    let shown = body.is_some();
    // The panel's `when:`s read again with the program, or with a new load
    // (the beats and words they name).
    let when_key = (session.doc_of, session.revision);
    let when_body = (page.motion.panel.when_of != Some(when_key)).then(|| body.clone().unwrap_or_default());
    page.motion.panel.when_of = Some(when_key);
    if makepad_value_pickers::panel::host::timed("ep.mount", || page.motion.panel.runtime.mount(cx, &panel_ref, body)) {
        panel_ref.set_visible(cx, shown);
    }
    makepad_value_pickers::panel::host::timed("ep.feed_refresh", || super::panel_feed::refresh(page));
    if let Some(body) = when_body {
        makepad_value_pickers::panel::host::timed("ep.set_when", || super::panel_time::set_when(&body));
    }
    makepad_value_pickers::panel::host::timed("ep.panel_time", || super::panel_time::pump(page, cx, ui));
    // The panel lights what acts now: drawn again when that changes (a
    // program draws only when asked), and scrolled to the first module
    // lit by a new selection in the picture.
    let lit = super::panel_time::lit_rev();
    if page.motion.panel.lit_drawn != lit {
        page.motion.panel.lit_drawn = lit;
        panel_ref.widget(cx, ids!(p_splash)).redraw(cx);
    }
    // The panel's own requests: let go of the picture's selection, scroll
    // to a module its index opened; and a keyed control's value at a new
    // playhead (drawn again on a seek).
    // A control's key button: its value keyed (a lane under the
    // scrubber) or made a constant again.
    for (path, on) in super::panel_feed::take_animate() {
        let done = if on { super::desk::animate(page, cx, &path) } else { super::desk::unanimate(page, cx, &path) };
        if let Err(why) = done {
            panel_ref.widget(cx, ids!(p_status)).set_text(cx, &format!("{path}: {why}"));
            panel_ref.widget(cx, ids!(p_status)).set_visible(cx, true);
        }
    }
    if super::panel_time::take_clear() {
        super::desk::clear_selection(page);
    }
    if let Some(rect) = makepad_value_pickers::panel::host::take_scroll_to() {
        let scroll = panel_ref.widget(cx, ids!(p_scroll));
        let top = scroll.area().rect(cx).pos.y;
        scroll.set_scroll_pos(cx, dvec2(0.0, (rect.pos.y - top - 8.0).max(0.0)));
    }
    let playhead = page.motion.desk.session.as_ref().map_or(0.0, |s| s.playhead);
    if page.motion.panel.playhead_drawn != playhead {
        page.motion.panel.playhead_drawn = playhead;
        panel_ref.widget(cx, ids!(p_splash)).redraw(cx);
    }
    if makepad_value_pickers::panel::host::take_reveal() {
        if let Some(rect) = makepad_value_pickers::panel::host::first_active_rect() {
            let scroll = panel_ref.widget(cx, ids!(p_scroll));
            let top = scroll.area().rect(cx).pos.y;
            // (The rect is as drawn, before the scroll.)
            scroll.set_scroll_pos(cx, dvec2(0.0, (rect.pos.y - top - 8.0).max(0.0)));
        }
    }
    drive_step(page, cx);
    verify_writes(page, &panel_ref, cx);
    makepad_value_pickers::panel::host::timed("ep.apply_edits", || apply_edits(page, cx, ui));
    if page.motion.panel.runtime.holding() {
        page.motion.panel.runtime.redraw_program(cx, &panel_ref);
        // The selected text's own controls read the held values too (its
        // swatches, its open picker's palette).
        desk_ref.widget(cx, ids!(m_selected_text)).widget(cx, ids!(t_style)).redraw(cx);
    }
    drive_applied(page);
}

/// A control's write that leaves the scene not loading is undone at once
/// (the scene keeps running as it was, and saving never meets it), and the
/// panel says which control and why.
fn verify_writes(page: &mut VideoCut, panel_ref: &WidgetRef, cx: &mut Cx) {
    let Some((revision, path)) = page.motion.panel.verify.clone() else { return };
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    if session.revision != revision {
        if session.revision > revision {
            page.motion.panel.verify = None;
        }
        return;
    }
    if session.doc_of == revision {
        page.motion.panel.verify = None;
        return;
    }
    let Some(problem) = session.problems.first().map(|p| format!("line {}: {}", p.line, p.message)) else { return };
    page.motion.panel.verify = None;
    super::desk::undo_last(page);
    let status = format!("{path}: that value breaks the scene ({problem}); put back");
    log!("editor panel: {status}");
    panel_ref.widget(cx, ids!(p_status)).set_text(cx, &status);
    panel_ref.widget(cx, ids!(p_status)).set_visible(cx, true);
}

fn apply_edits(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let files: Vec<paths::File> = std::iter::once(paths::File { name: "", text: &session.source, code: &session.code })
        .chain(session.modules.iter().filter(|m| m.id != "builtin").map(|m| paths::File { name: &m.name, text: &m.source, code: &m.code }))
        .collect();
    // Which session file each path file is (built-ins are skipped).
    let doc_of: Vec<usize> = std::iter::once(0).chain(session.modules.iter().enumerate().filter(|(_, m)| m.id != "builtin").map(|(k, _)| k + 1)).collect();
    let splices = makepad_value_pickers::panel::host::timed("ep.take", || page.motion.panel.runtime.take(&makepad_stage_motion_pickers::panel_files(&files), &|_, _| None));
    for splice in splices {
        // A drag starting where the control does nothing: the playhead to
        // the nearest moment it acts, so the change is seen.
        if !splice.done && page.motion.panel.gesture != Some(splice.gesture) {
            let playhead = page.motion.desk.session.as_ref().map_or(0.0, |s| s.playhead);
            if let Some(t) = super::panel_time::nearest_span_start(&splice.path, playhead) {
                super::desk::set_playhead(page, t);
            }
        }
        let doc = doc_of[splice.splice.file];
        // The path resolved in the session's text; within a held gesture
        // that is the gesture's start, and the literal is where the
        // gesture's steps moved it.
        let range = super::desk::now_of(page, doc, splice.splice.range.clone());
        // The code shows the line being changed: lit as the gesture
        // starts (or the one edit lands), and again at its release.
        if page.motion.panel.gesture != Some(splice.gesture) || splice.done {
            if let Some(mut code) = ui.widget(cx, ids!(cut_motion)).widget(cx, ids!(m_code)).borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
                code.show_edit(cx, doc, splice.splice.range.start);
            }
        }
        page.motion.panel.gesture = (!splice.done).then_some(splice.gesture);
        page.motion.desk.author = Some("You · editor".into());
        let path = splice.path.clone();
        super::desk::code_edits(page, cx, doc, vec![Edit::splice(range.start..range.end, splice.splice.text, splice.gesture, splice.done)]);
        if splice.done {
            // Checked when the scene has loaded it (`verify_writes`).
            let revision = page.motion.desk.session.as_ref().map_or(0, |s| s.revision);
            page.motion.panel.verify = Some((revision, path));
        }
    }
    if !page.motion.panel.runtime.holding() {
        page.motion.panel.gesture = None;
    }
}

/// What the panel binds, each path resolved in the session's text now (a
/// bind is its path: it is found again in the text as it is after every
/// edit, reload or new session), and the paths that no longer resolve,
/// each with why and the nearest that do.
pub fn bindings(page: &VideoCut) -> (Vec<super::code_timeline::Binding>, Vec<String>) {
    let Some(session) = page.motion.desk.session.as_ref() else { return (Vec::new(), Vec::new()) };
    let files: Vec<paths::File> = std::iter::once(paths::File { name: "", text: &session.source, code: &session.code })
        .chain(session.modules.iter().filter(|m| m.id != "builtin").map(|m| paths::File { name: &m.name, text: &m.source, code: &m.code }))
        .collect();
    let doc_of: Vec<usize> = std::iter::once(0).chain(session.modules.iter().enumerate().filter(|(_, m)| m.id != "builtin").map(|(k, _)| k + 1)).collect();
    let (mut bound, mut gone) = (Vec::new(), Vec::new());
    for path in super::panel_time::asked() {
        match paths::resolve(&files, &path) {
            Ok(site) => bound.push(super::code_timeline::Binding { path, doc: doc_of[site.file], range: site.range }),
            Err(why) => gone.push(format!("path gone: {why}")),
        }
    }
    (bound, gone)
}

/// The scene's paths as the agent reads them, for writing a panel: a
/// value read while the scene loads is marked RELOAD (it takes a reload to
/// show; a control must not hold one).
pub fn paths_listing(session: &super::session::MotionSession) -> String {
    let files: Vec<paths::File> = std::iter::once(paths::File { name: "", text: &session.source, code: &session.code })
        .chain(session.modules.iter().filter(|m| m.id != "builtin").map(|m| paths::File { name: &m.name, text: &m.source, code: &m.code }))
        .collect();
    // Each path's reach, traced in the running document (what a patch of it
    // sets, and whether the load computed with it), as the texts it was
    // loaded from: only while it is the session's revision, and kept per
    // loaded document (a reach is the same until the next load).
    use makepad_motion::schema::CodeFile;
    let running = session.doc.as_ref().filter(|_| session.doc_of == session.revision);
    let code_files: Vec<CodeFile> = std::iter::once(CodeFile::Doc).chain(session.modules.iter().filter(|m| m.id != "builtin").map(|m| CodeFile::Module(m.key.clone()))).collect();
    let of = running.map(|d| std::sync::Arc::as_ptr(d) as usize);
    REACH.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.0 != of {
            *cache = (of, Default::default());
        }
    });
    panel::listing(&makepad_stage_motion_pickers::panel_files(&files), &|entry| {
        let doc = running?;
        if !(entry.kind.is_numeric() || entry.kind == panel::ValueKind::Colour) {
            return None;
        }
        let site = paths::resolve_exact(&files, &entry.id).ok()?;
        let key = (site.file, site.range.start);
        let reload = match REACH.with(|c| c.borrow().1.get(&key).copied()) {
            Some(reload) => reload,
            None => {
                let file = &code_files[site.file];
                let reach = match file {
                    CodeFile::Module(k) => doc.literal_reach(file, &doc.loaded_module_text(k)?, site.range),
                    _ => doc.literal_reach(file, doc.source.text(), site.range),
                }?;
                let reload = reach.needs_reload();
                REACH.with(|c| c.borrow_mut().1.insert(key, reload));
                reload
            }
        };
        reload.then(|| "RELOAD".to_string())
    })
}

thread_local! {
    /// Reaches per loaded document: (the document, (file, literal start) -> needs a reload).
    static REACH: std::cell::RefCell<(Option<usize>, std::collections::HashMap<(usize, usize), bool>)> = std::cell::RefCell::new((None, Default::default()));
}

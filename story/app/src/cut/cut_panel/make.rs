//! The film's agent: makes the cut's panel (when a cut without one
//! opens, or Remake with a brief) and a control's AI button. Changes to the
//! film's values and to its panel on request are the chat's tools
//! (`cut_panel_gesture`, `cut_panel_write`, `cut_panel_make`).
//!
//! A panel is written in parts at once, each by its own conversation over
//! part of what the film can bind: the look (the shared modules, the
//! palette, the fonts), the pacing (the clips over the song, their cuts on
//! the beats, a jump into each scene's own panel), and what the graphics
//! share (`*:PATH`, written to every one). Each part answers
//! `PanelModule{…}` records; the parts that run are put together into
//! `let cut_panel` and written into the cut as one operation.
//!
//! Main-loop cost: a drain of each conversation's channel per pump; the
//! conversations run on the task pool.

use super::super::motion_edit::variants::{backend_named, model_spec, start_brain, Make as Brief};
use super::super::ops::CutOp;
use super::super::VideoCut;
use super::LET;
use makepad_editor_panel as panel;
use makepad_editor_panel::make;
use makepad_story_video_agent::BrainEvent;
use makepad_widgets::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// What the cut's panel is made of, and the rules of its paths.
const SYSTEM: &str = "You write part of the editor panel of a whole film made in Stage: a cut that places motion \
graphics (each its own Splash document, a `Graphic` clip) over a song, and the Splash modules they share (the look: \
colours, fonts, post; the song's timing). The panel is a small Splash program; every control binds a path. At this \
level a path names its document first:\n\
- `edit.<clip>.span` (`[start, length]` in seconds), `edit.<clip>.start`, `edit.<clip>.length`, `edit.<clip>.in`, \
`edit.<clip>.gain`; `edit.<clip>.cut` is the cut at the clip's start, rolled: the clip before it ends there too \
(`edit.<clip>.end`: the cut after the clip), and cuts land on the song's beats. Pacing is cuts: a `length` alone \
leaves a gap or an overlap;\n\
- `<module>.PATH`: a value of a module the graphics share (`look.POST.bloom`, `look.BONE`): one change reaches every \
graphic that imports it, the whole film;\n\
- `<clip>:PATH`: a value of one graphic's own document (`p_open:WORD_SPARKS.count`), or of a module it imports by \
the name it imports it as (`p_loss:look.BONE`; a shared module's value is better bound once as `look.BONE`);\n\
- sets: `*` (every graphic, or every clip), `a + b` (named ones): `*:TAGS[0].size`, `p_loss + p_room:INK`, \
`edit.p_hook1 + p_hook2 + p_hook3.cut` (the set between `edit.` and the field, never `edit.p_hook1.cut + p_hook2.cut`), `edit.*.cut`. A number control on a set moves every member by as much as \
it moved (each keeps its own value relative to the others); anything else is written to each.\n\
Answer only with `PanelModule{...}` records (Splash, as in the catalogue below): no `let`, no outer View, no prose, \
no code fences, no comments. Each module is for what a person thinks of (Mood, Palette, Type, Pacing, a section of \
the song), four to eight controls each, titled and labelled in their words, with a `when:` when it acts only in part \
of the film: a section of the song by name (`\"chorus1\"`, `\"verse1-pre1\"`), beats (`\"beats 9-16\"`), a sung \
word (`\"the lyric 'AGI'\"`) or seconds (`\"12s-30s\"`). A control shows and sets the value at its path itself \
(a range holds that value; it is not an offset). Write each widget on one line, only the fields it needs. A scene of \
the film is `PanelScene{node: \"p_shoggoth\"}` (its picture and time; a click opens its own panel). The palette \
here is `PanelPalette{}` (every document's colours; no scope). If you have nothing worth a module, answer with \
nothing.";

pub(super) enum Kind {
    /// The panel, made or remade.
    Panel,
    /// The self-check's one fix round of a panel just written (the problems
    /// it was sent).
    Fix(usize),
}

/// A make's parts as they come (the shared driver), and what it is for.
pub(super) struct Job {
    kind: Kind,
    make: make::Job,
}

fn job(cx: &Cx, kind: Kind, brief: Brief, texts: Vec<String>) -> Result<Job, String> {
    let write: make::Write = Arc::new(move |text, cancel, piece| turn(&brief, text, cancel, piece));
    Ok(Job { kind, make: make::Job::spawn(&cx.task_pool(), texts, Cx::time_now(), write)? })
}

#[derive(Default)]
pub(super) struct Make {
    job: Option<Job>,
    /// A cut already asked for its panel (once per edit, by label).
    asked: std::collections::HashSet<String>,
    progress: Option<(String, bool)>,
    /// The last make: its seconds and what it wrote (the control API).
    pub(super) last: Option<String>,
    /// A made panel whose self-check found problems, held while its fix
    /// round runs: the panel is written once, fixed (the panel is not
    /// mounted twice, a hand on it would lose what it was doing).
    held: Option<Vec<String>>,
}

thread_local! {
    /// Asks from the panel's widgets, and their answers.
    static ASKS: RefCell<(u64, Vec<(u64, String, Vec<String>, String)>, Vec<(u64, String)>)> = const { RefCell::new((0, Vec::new(), Vec::new())) };
}

/// A control's AI button: queued for the next pump.
pub(super) fn ask(prompt: &str, targets: &[String], mode: &str) -> Option<u64> {
    ASKS.with(|a| {
        let mut a = a.borrow_mut();
        a.0 += 1;
        let id = a.0;
        a.1.push((id, prompt.to_string(), targets.to_vec(), mode.to_string()));
        a.2.push((id, "working".into()));
        Some(id)
    })
}

pub(super) fn ask_status(id: u64) -> String {
    ASKS.with(|a| a.borrow().2.iter().find(|(i, _)| *i == id).map(|(_, s)| s.clone()).unwrap_or_else(|| "failed: unknown".into()))
}

fn set_ask_status(id: u64, status: String) {
    ASKS.with(|a| {
        if let Some(s) = a.borrow_mut().2.iter_mut().find(|(i, _)| *i == id) {
            s.1 = status;
        }
    });
}

/// Run one finite turn and stream its text back to the panel.
fn turn(brief: &Brief, text: String, cancel: &AtomicBool, piece: &mut dyn FnMut(&str)) -> Result<(), String> {
    let mut brain = start_brain(brief)?;
    brain.send_user(text, Vec::new());
    // End the conversation here too: closing its live CLI can wait for the child.
    makepad_widgets::makepad_platform::thread::without_permit(move || loop {
        if cancel.load(Ordering::Relaxed) {
            brain.cancel();
            return Err("stopped".into());
        }
        for event in brain.poll() {
            match event {
                BrainEvent::Delta(t) => piece(&t),
                BrainEvent::TurnDone => return Ok(()),
                BrainEvent::Failed(error) => return Err(error),
                _ => {}
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    })
}

fn brief_with(system: &str) -> Result<Brief, String> {
    let backend = backend_named(&model_spec(None));
    if let Some(reason) = backend.unavailable() {
        return Err(reason);
    }
    Ok(Brief { backend, system: Arc::from(system) })
}

/// What the film is, for every part: its clips over the song.
fn film_summary(page: &VideoCut) -> String {
    let Some(session) = page.session.as_ref() else { return String::new() };
    let doc = &session.doc;
    let mut out = format!("The film: \"{}\", {:.1} s at {:.1} bpm (a beat is {:.3} s).\nIts clips (clip: start-end s, what it shows):\n", doc.label, doc.duration, doc.bpm, 60.0 / doc.bpm.max(1.0));
    let state = &page.cut_panel;
    for (clip, _, start, length) in &state.table.clips {
        let shows = state.table.nodes.iter().find(|n| n.node == *clip).map(|n| state.docs[n.file].name.clone()).unwrap_or_else(|| "sound".into());
        out.push_str(&format!("{clip}: {start:.2}-{:.2} ({shows})\n", start + length));
    }
    out
}

/// The listing's lines in three parts: look (modules), pacing (the cut),
/// shared (`*:`).
fn parts_of(listing: &str) -> [String; 3] {
    let mut out = [String::new(), String::new(), String::new()];
    for line in listing.lines() {
        let k = if line.starts_with("edit.") { 1 } else if line.starts_with("*:") { 2 } else { 0 };
        if out[k].len() < 24_000 {
            out[k].push_str(line);
            out[k].push('\n');
        }
    }
    out
}

/// Ask for the cut's panel (again, with `brief`).
pub(super) fn make_panel(page: &mut VideoCut, cx: &mut Cx, brief: Option<&str>) -> Result<(), String> {
    if page.cut_panel.make.job.as_ref().is_some_and(|j| matches!(j.kind, Kind::Panel)) {
        return Ok(());
    }
    if page.cut_panel.docs.is_empty() {
        return Err("the film's documents are still loading".into());
    }
    let listing = {
        let state = &page.cut_panel;
        panel::listing_docs(&makepad_stage_motion_pickers::panel_files(&state.files()), &state.table)
    };
    let summary = film_summary(page);
    let steer = make::steer(brief);
    let system = format!("{SYSTEM}\n\n{}", makepad_value_pickers::panel::CATALOGUE);
    let brief_of = brief_with(&system)?;
    let [look, pacing, shared] = parts_of(&listing);
    let sections = sections_of(page);
    let section_list: String = sections.iter().map(|(name, a, b, clips)| format!("{name}: {a:.2}-{b:.2} s, scenes {}\n", clips.join(", "))).collect();
    // The look: the modules the graphics share when they share one (a
    // film whose plates import a shared look), else what the graphics have in common.
    let look_part = if look.trim().is_empty() {
        format!("Part 1 of 4: the whole film's look (film-wide modules). The graphics share no module: bind only the `*:PATH` paths below (written to every graphic at once). Start with a `PanelModule{{title: \"Mood\" open: true}}` of two or three `PanelMacro` over the colours, glows and motion they share, then a `PanelModule{{title: \"Palette\"}}` with `PanelPalette{{}}` (the colours of every document, moved together), and fonts they share if any.{steer}\n{summary}\nPaths several graphics have (`*:PATH (kind, n places)`):\n{shared}")
    } else {
        format!("Part 1 of 4: the whole film's look (film-wide modules). Start with a `PanelModule{{title: \"Mood\" open: true}}` of two or three `PanelMacro` that move the whole film's feel (energy, heat, haze) through the post and look values below. Then a `PanelModule{{title: \"Palette\"}}` with `PanelPalette{{}}` (the colours of every document, moved together) and the shared colours, and a `PanelModule{{title: \"Type\"}}` with the shared fonts.{steer}\n{summary}\nThe shared modules' paths (path (kind) = value  // what it is for):\n{look}")
    };
    let asks = [
        (look_part, "look"),
        (format!("Part 2 of 4: the whole film's pacing (one film-wide module). A `PanelModule{{title: \"Pacing\"}}` binding cuts on the song's beats: sets of them (every cut `edit.*.cut`, the hooks' cuts, a section's cuts) and single ones that matter.{steer}\n{summary}\nThe edit's paths:\n{pacing}"), "pacing"),
        (format!("Part 3 of 4: what the graphics share, written to every one at once (`*:PATH`), as one or two film-wide modules (type sizes, motion).{steer}\n{summary}\nPaths several graphics have (`*:PATH (kind, n places)`):\n{shared}"), "shared"),
        (format!("Part 4 of 4: the film as the song's sections, one `PanelModule` per section below, in order, each `title:` the section in words and `when: \"<section name>\"`; each starts with a `PanelScene{{node: \"<clip>\"}}` for each of its scenes, then two to four controls for that section only (its cuts `edit.<clip>.cut`, values its scenes share `a + b:PATH`).{steer}\n{summary}\nThe sections and the scenes playing in each:\n{section_list}\nThe paths:\n{pacing}{shared}"), "sections"),
    ];
    let texts = asks.into_iter().map(|(text, _)| text).collect();
    page.cut_panel.make.job = Some(job(cx, Kind::Panel, brief_of, texts)?);
    Ok(())
}

fn has_panel(page: &VideoCut) -> bool {
    page.cut_panel.docs.first().is_some_and(|d| panel::body_of_let(&d.text, &d.code, LET).is_some())
}

/// The cut is loaded and has no editor panel yet.
pub(crate) fn lacks_panel(page: &VideoCut) -> bool {
    !page.cut_panel.docs.is_empty() && !has_panel(page)
}

/// While the panel is made: its progress line, and whether it is busy.
pub(super) fn progress(page: &VideoCut) -> Option<(String, bool)> {
    let make = &page.cut_panel.make;
    match &make.job {
        Some(job) if matches!(job.kind, Kind::Panel) => {
            let secs = Cx::time_now() - job.make.started;
            let (done, all, _) = job.make.progress();
            Some((format!("Making the film's editor… {secs:.0} s ({done} of {all} parts)"), true))
        }
        _ => make.last.clone().map(|l| (l, false)),
    }
}

/// Write the cut's panel from `modules` (the control API's
/// `cut_panel_write`: an agent's own panel): checked first, one step.
pub(super) fn write_panel(page: &mut VideoCut, cx: &mut Cx, modules: &str) -> Result<Vec<String>, String> {
    let (m, dropped) = make::modules_of(cx, modules, "Panel");
    if m.is_empty() {
        return Err(dropped.first().cloned().unwrap_or_else(|| "no PanelModule records".into()));
    }
    let text = with_panel(&page.cut_panel.docs[0].text.clone(), &m)?;
    let body = panel::body_of_let(&text, &makepad_motion::schema::code::outline(&text), LET).unwrap_or_default();
    let issues = check(page, &body);
    super::super::motion_edit::note_author(page, "Film's agent");
    page.op(CutOp::Source { text });
    Ok(issues)
}

/// The song's sections and the clips (scenes) playing in each, in order.
pub(super) fn sections_of(page: &VideoCut) -> Vec<(String, f64, f64, Vec<String>)> {
    let state = &page.cut_panel;
    let Some(timing) = &state.timing else { return Vec::new() };
    timing.sections.iter().map(|(name, a, b)| {
        let clips = state.table.nodes.iter().filter_map(|n| {
            let (_, _, s, l) = state.table.clips.iter().find(|c| c.0 == n.node)?;
            (s.max(*a) + 0.25 < (s + l).min(*b)).then(|| n.node.clone())
        }).collect();
        (name.clone(), *a, *b, clips)
    }).collect()
}

/// A section's name in words (`chorus1` → `Chorus 1`).
fn title_of(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
        } else {
            if c.is_ascii_digit() && !out.ends_with(|p: char| p.is_ascii_digit()) {
                out.push(' ');
            }
            out.push(c);
        }
    }
    out
}

/// The cut's text with its panel `modules`.
/// The held panel written (`fixed`: the fix round's modules in its place):
/// each control whose path does not resolve left out (a macro its target),
/// one undo step. What was left out, and the problems that remain.
fn write_held(page: &mut VideoCut, fixed: Option<Vec<String>>) -> (usize, usize) {
    let Some(made) = page.cut_panel.make.held.take() else { return (0, 0) };
    let modules = fixed.unwrap_or(made);
    let source = page.cut_panel.docs[0].text.clone();
    let Ok(text) = with_panel(&source, &modules) else { return (0, 0) };
    let body = panel::body_of_let(&text, &makepad_motion::schema::code::outline(&text), LET).unwrap_or_default();
    // (Paths that do not resolve; a range or a `when:` is left as written.)
    let unresolved: Vec<String> = check(page, &body).iter().filter(|i| !i.contains("`: its range ")).filter_map(|i| i.strip_prefix('`')?.split_once("`:").map(|(p, _)| p.to_string())).collect();
    let mut dropped = 0;
    let kept: Vec<String> = modules.iter().map(|m| {
        m.lines().filter_map(|l| {
            if unresolved.iter().any(|p| l.contains(&format!("path: \"{p}\""))) {
                dropped += 1;
                return None;
            }
            // A macro: the targets that do not resolve.
            if let Some(at) = l.find("targets: \"") {
                let start = at + 10;
                let end = l[start..].find('"').map_or(l.len(), |e| start + e);
                let targets: Vec<&str> = l[start..end].split(',').filter(|t| {
                    let words: Vec<&str> = t.split_whitespace().collect();
                    let path = if words.len() > 2 { words[..words.len() - 2].join(" ") } else { words.first().copied().unwrap_or("").to_string() };
                    !unresolved.contains(&path)
                }).collect();
                if targets.is_empty() {
                    dropped += 1;
                    return None;
                }
                return Some(format!("{}{}{}", &l[..start], targets.join(","), &l[end..]));
            }
            Some(l.to_string())
        }).collect::<Vec<_>>().join("\n")
    }).collect();
    let Ok(text) = with_panel(&source, &kept) else { return (0, dropped) };
    let body = panel::body_of_let(&text, &makepad_motion::schema::code::outline(&text), LET).unwrap_or_default();
    let left = check(page, &body).len();
    super::super::motion_edit::note_author(page, "Film's agent");
    page.op(CutOp::Source { text });
    (left, dropped)
}

fn with_panel(source: &str, modules: &[String]) -> Result<String, String> {
    panel::with_let(source, &make::assemble(LET, &[], modules), "Flow{", LET)
}

/// Each path the panel binds, checked against the film as it is.
fn check(page: &VideoCut, body: &str) -> Vec<String> {
    let state = &page.cut_panel;
    let files = state.files();
    // A path that does not resolve, a range that does not hold the value
    // written there (a set's: its first member's).
    let mut issues: Vec<String> = make::range_issues(&make::bindings(body), &|path| {
        let bound = makepad_motion::paths::docs::resolve(&files, &state.table, path)?;
        Ok(bound.first().and_then(|b| make::number(&b.site.text)))
    })
    .into_iter()
    .map(|i| i.split_once(' ').map_or(i.clone(), |(_, rest)| rest.to_string()))
    .collect();
    // A `when:` that names nothing in the song.
    for line in body.lines() {
        if let Some(when) = make::field(line, "when").filter(|w| !w.trim().is_empty()) {
            if state.timing.as_ref().is_some_and(|t| t.when(&when).is_none()) {
                issues.push(format!("when: \"{when}\" names no section, beats, sung word or seconds of the song"));
            }
        }
    }
    issues
}

/// The self-check's fix round: the problems sent back to the film's agent
/// once; its whole corrected panel is what is written ([`write_held`]).
fn start_fix(page: &mut VideoCut, cx: &mut Cx, body: &str, issues: &[String]) -> Result<(), String> {
    let state = &page.cut_panel;
    let listing = panel::listing_docs(&makepad_stage_motion_pickers::panel_files(&state.files()), &state.table);
    let system = format!("{SYSTEM}\n\n{}", makepad_value_pickers::panel::CATALOGUE);
    let text = format!("{}{}\nThe panel made:\n{body}\nThe paths:\n{listing}\nIts self-check found these problems; answer the whole panel with each fixed, as PanelModule records (bind a path that exists, a range that holds the value, a `when:` the song has):\n- {}", film_summary(page), sections_summary(page), issues.join("\n- "));
    page.cut_panel.make.job = Some(job(cx, Kind::Fix(issues.len()), brief_with(&system)?, vec![text])?);
    Ok(())
}

/// The song's sections, for the agent (what `when:` can name).
fn sections_summary(page: &VideoCut) -> String {
    let Some(timing) = &page.cut_panel.timing else { return String::new() };
    let mut out = String::from("The song's sections (name: start-end s, in the cut's seconds):\n");
    for (name, a, b) in &timing.sections {
        out.push_str(&format!("{name}: {a:.2}-{b:.2}\n"));
    }
    out
}

pub(super) fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    // A cut opened without a panel gets one (once per edit).
    let state = &page.cut_panel;
    if state.make.job.is_none() && !state.docs.is_empty() && state.commit.is_none() {
        let label = page.session.as_ref().map(|s| s.doc.label.clone()).unwrap_or_default();
        let has = panel::body_of_let(&state.docs[0].text, &state.docs[0].code, LET).is_some();
        if !has && !state.make.asked.contains(&label) && std::env::var("STAGE_EDIT_PANEL_AUTO").map_or(true, |v| v != "0") {
            page.cut_panel.make.asked.insert(label);
            if let Err(error) = make_panel(page, cx, None) {
                page.cut_panel.make.last = Some(error);
            }
        }
    }
    // A control's AI button: the chat takes it as a line about its paths.
    let next = ASKS.with(|a| {
        let mut a = a.borrow_mut();
        (!a.1.is_empty()).then(|| a.1.remove(0))
    });
    if let Some((id, prompt, targets, mode)) = next {
        super::super::chat::say(page, format!("{prompt} (on {}; {mode})", targets.join(", ")));
        set_ask_status(id, "done: sent to the chat".into());
    }
    // Answers as they come.
    let mut finished = false;
    if let Some(job) = page.cut_panel.make.job.as_mut() {
        finished = job.make.pump();
        cx.new_next_frame();
    }
    if finished {
        let job = page.cut_panel.make.job.take().expect("checked");
        let secs = Cx::time_now() - job.make.started;
        match job.kind {
            Kind::Panel => panel_written(page, cx, job, secs),
            Kind::Fix(_) => fix_answered(page, cx, job, secs),
        }
    }
    // The panel slot while there is no panel.
    let has = has_panel(page);
    let making = progress(page).filter(|p| p.1);
    let shown = if page.cut_panel.docs.is_empty() { None } else if has { making } else { making.or_else(|| progress(page)).or_else(|| Some(("This film has no editor yet: ask the chat to make one.".into(), false))) };
    if shown != page.cut_panel.make.progress {
        let col = super::column(cx, ui);
        col.widget(cx, ids!(ep_making)).set_visible(cx, shown.is_some());
        if let Some((text, _)) = &shown {
            col.widget(cx, ids!(ep_progress)).set_text(cx, text);
        }
        page.cut_panel.make.progress = shown;
    }
}

fn panel_written(page: &mut VideoCut, cx: &mut Cx, job: Job, secs: f64) {
    let (modules, dropped) = job.make.modules(cx, "Panel");
    if modules.is_empty() {
        page.cut_panel.make.last = Some(format!("The film's editor was not made: {}", dropped.first().cloned().unwrap_or_else(|| "no part answered with modules".into())));
        return;
    }
    // The tree: the header, the film-wide modules, then the song's sections
    // as root nodes (a section no part wrote gets its scenes).
    let mut modules = modules;
    for (name, _, _, clips) in sections_of(page) {
        if !modules.iter().any(|m| m.contains(&format!("when: \"{name}\""))) {
            let scenes: Vec<String> = clips.iter().map(|c| format!("    PanelScene{{node: \"{c}\"}}")).collect();
            modules.push(format!("PanelModule{{title: \"{}\" when: \"{name}\"\n{}\n}}", title_of(&name), scenes.join("\n")));
        }
    }
    modules.insert(0, "PanelHeader{}".to_string());
    let source = page.cut_panel.docs[0].text.clone();
    match with_panel(&source, &modules) {
        Ok(text) => {
            let body = panel::body_of_let(&text, &makepad_motion::schema::code::outline(&text), LET).unwrap_or_default();
            let issues = check(page, &body);
            let what = format!("Made the film's editor in {secs:.1} s: {} module(s){}{}", body.matches("PanelModule{").count(),
                if dropped.is_empty() { String::new() } else { format!(", {} left out: {}", dropped.len(), dropped[0]) },
                if issues.is_empty() { String::new() } else { format!(", {} path(s) do not resolve: {}", issues.len(), issues.iter().take(3).cloned().collect::<Vec<_>>().join("; ")) });
            log!("edit panel: {what}");
            page.cut_panel.make.last = Some(what);
            // Its problems go back to the agent once, and the panel is
            // written when it answers (once); without a fix round, now,
            // what does not resolve left out.
            if issues.is_empty() {
                super::super::motion_edit::note_author(page, "Film's agent");
                page.op(CutOp::Source { text });
            } else {
                page.cut_panel.make.held = Some(modules.clone());
                if let Err(error) = start_fix(page, cx, &body, &issues) {
                    log!("edit panel: no fix round: {error}");
                    write_held(page, None);
                }
            }
        }
        Err(error) => page.cut_panel.make.last = Some(format!("The film's editor was not written: {error}")),
    }
}

/// The fix round's answer: its modules are written, once, in place of the
/// panel held back (else the made ones), what still does not resolve left
/// out.
fn fix_answered(page: &mut VideoCut, cx: &mut Cx, job: Job, secs: f64) {
    let part = &job.make.parts[0];
    let Kind::Fix(n) = job.kind else { return };
    if page.cut_panel.make.held.is_none() {
        return;
    }
    let fixed = part.failed.is_none().then(|| make::modules_of(cx, part.text.trim(), "Panel").0).filter(|m| !m.is_empty());
    let (left, dropped) = write_held(page, fixed);
    let what = format!("{} · fix round for {n} problem(s) ({secs:.1} s): {left} left, {dropped} control(s) that do not resolve left out", page.cut_panel.make.last.clone().unwrap_or_default());
    log!("edit panel: {what}");
    page.cut_panel.make.last = Some(what);
}

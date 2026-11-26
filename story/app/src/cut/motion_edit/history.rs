//! The open graphic's history as the scene's agent reads it, and merges
//! from it: "take the way the unicorn looked from here".
//!
//! The steps are the session's history (`MotionSession::history`, the one
//! the History tab shows, append-only): step k the state after entry k
//! (step 0 the graphic as opened), the last the graphic now. A step can
//! also be the row picked in the History tab ([`pick`]), step `picked`.
//!
//! - [`listing`]: the steps, labelled, with how long ago each was left;
//! - [`read`]: the code of named `let`s (or a module) at a step;
//! - [`diff`]: what changed between two steps, its passages and the
//!   top-level `let`s and modules they touch;
//! - [`merge`]: the state of some paths at a step put onto the graphic now
//!   as a variant (the variant bar's preview, Choose to apply it as one
//!   undo step): each path's top-level `let` (or a whole module,
//!   `module:<name>`) taken from the step's text, everything else current.
//!
//! Main-loop cost: none; each runs when asked (a few text scans).

use super::super::VideoCut;
use super::session::MotionSession;
use makepad_motion::schema::code;
use std::cell::RefCell;

/// A step's files: 0 the document, `k + 1` module `k` (the session's
/// order), each its text.
pub type Texts = Vec<(usize, String)>;

thread_local! {
    /// The History tab's picked row: its label and its files' texts (the
    /// document's, and modules' by key).
    static PICKED: RefCell<Option<(String, String, Vec<(String, String)>)>> = const { RefCell::new(None) };
}

/// The History tab's row picked (`None`: none): its label, the document's
/// text then, and its modules' texts by key (`id@rev`). The scene's agent
/// reads it as step `picked`, and "from here" means it.
pub fn pick(row: Option<(String, String, Vec<(String, String)>)>) {
    PICKED.with(|p| *p.borrow_mut() = row);
}

fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

fn ago(at: f64) -> String {
    let s = (now_secs() - at).max(0.0);
    if s < 90.0 { format!("{s:.0} s ago") } else if s < 5400.0 { format!("{:.0} min ago", s / 60.0) } else { format!("{:.1} h ago", s / 3600.0) }
}

/// The steps: (label, when it was made).
fn steps(session: &MotionSession) -> Vec<(String, f64)> {
    session.history.iter().map(|h| {
        let who = if h.source.is_empty() { String::new() } else { format!(" ({})", h.source) };
        (format!("{}{who}", h.label), h.time)
    }).collect()
}

/// The texts at a step (`"picked"`, `"now"`, a number, or "N min ago").
pub fn texts_at(session: &MotionSession, step: &str) -> Result<(String, Texts), String> {
    let step = step.trim().trim_matches('`');
    let files = |source: &str, modules: &[(String, String)]| -> Texts {
        std::iter::once((0, source.to_string()))
            .chain(session.modules.iter().enumerate().map(|(i, m)| (i + 1, modules.iter().find(|(k, _)| *k == m.key).map_or_else(|| m.source.clone(), |(_, t)| t.clone()))))
            .collect()
    };
    if step == "picked" {
        return PICKED.with(|p| p.borrow().as_ref().map(|(label, source, modules)| (format!("the picked row ({label})"), files(source, modules)))).ok_or_else(|| "no history row is picked".to_string());
    }
    if step == "now" {
        return Ok(("now".into(), files(&session.source, &session.modules.iter().map(|m| (m.key.clone(), m.source.clone())).collect::<Vec<_>>())));
    }
    let n = session.history.len();
    // "10 min ago": the state that held then (the last step made by then).
    let k = if let Some(minutes) = step.strip_suffix("min ago").or_else(|| step.strip_suffix("minutes ago")).and_then(|m| m.trim().parse::<f64>().ok()) {
        let then = now_secs() - minutes * 60.0;
        (0..n).rev().find(|&k| session.history[k].time <= then).unwrap_or(0)
    } else {
        step.trim_start_matches("step").trim().parse::<usize>().map_err(|_| format!("no step `{step}` (a number 0..{}, `picked`, `now` or `N min ago`)", n.saturating_sub(1)))?
    };
    let Some(h) = session.history.get(k) else { return Err(format!("no step {k}: the history has 0..{}", n.saturating_sub(1))) };
    let now = if session.current_step() == Some(k) { " (now)" } else { "" };
    Ok((format!("step {k}{now} · {}", h.label), files(&h.after.source, &h.after.modules)))
}

/// Pick a step as the History tab's row (the control API's
/// `history_pick`; `None` lets go).
pub fn pick_step(session: &MotionSession, step: Option<&str>) -> Result<String, String> {
    let Some(step) = step else {
        pick(None);
        return Ok("no row picked".into());
    };
    let (label, texts) = texts_at(session, step)?;
    let modules = session.modules.iter().enumerate().map(|(i, m)| (m.key.clone(), texts[i + 1].1.clone())).collect();
    pick(Some((label.clone(), texts[0].1.clone(), modules)));
    Ok(label)
}

/// The steps as the agent reads them (the newest `last`).
pub fn listing(session: &MotionSession, last: usize) -> String {
    let all = steps(session);
    let from = all.len().saturating_sub(last);
    let mut out = String::new();
    if from > 0 {
        out.push_str(&format!("(steps 0..{} before these)\n", from - 1));
    }
    let current = session.current_step();
    for (k, (label, at)) in all.iter().enumerate().skip(from) {
        let now = if current == Some(k) { " · now" } else { "" };
        out.push_str(&format!("step {k} · {label} · {}{now}\n", ago(*at)));
    }
    if let Some(label) = PICKED.with(|p| p.borrow().as_ref().map(|(l, _, _)| l.clone())) {
        out.push_str(&format!("step picked · {label} (the row picked in the History tab: \"from here\" means it)\n"));
    }
    out
}

/// The names of the files (0 `doc`, then the modules').
fn names(session: &MotionSession) -> Vec<String> {
    std::iter::once("doc".to_string()).chain(session.modules.iter().map(|m| m.name.clone())).collect()
}

/// A top-level `let`'s whole lines in `text`: (first byte, end byte).
fn let_lines(text: &str, name: &str) -> Option<(usize, usize)> {
    let outline = code::outline(text);
    let region = outline.regions.iter().find(|r| r.parent.is_none() && r.name == name)?;
    let start = text[..region.range.start].rfind('\n').map_or(0, |n| n + 1);
    let end = text[region.range.end.min(text.len())..].find('\n').map_or(text.len(), |n| region.range.end + n);
    Some((start, end))
}

/// A path's file and `let` (`module.NAME`, `NAME`; `module:<name>` a whole
/// module: no `let`).
fn resolve(session: &MotionSession, path: &str) -> Result<(usize, Option<String>), String> {
    let path = path.trim().trim_matches('`').trim_end_matches(',');
    let module = |name: &str| session.modules.iter().position(|m| m.name == name).map(|i| i + 1);
    if let Some(name) = path.strip_prefix("module:") {
        return module(name).map(|k| (k, None)).ok_or_else(|| format!("no module `{name}`"));
    }
    // The first part of a path names the `let` (`WORD_SPARKS.count` is in
    // `WORD_SPARKS`); a module's path starts with the module.
    let mut parts = path.split(['.', '[']);
    let first = parts.next().unwrap_or("");
    if let Some(k) = module(first) {
        let name = parts.next().ok_or_else(|| format!("`{path}`: which `let` of module `{first}`?"))?;
        return Ok((k, Some(name.to_string())));
    }
    Ok((0, Some(first.to_string())))
}

/// The code of `paths` at a step.
pub fn read(session: &MotionSession, step: &str, paths: &[String]) -> Result<String, String> {
    let (label, texts) = texts_at(session, step)?;
    let names = names(session);
    let mut out = format!("At {label}:\n");
    for path in paths {
        let (file, name) = resolve(session, path)?;
        let text = &texts[file].1;
        match name {
            None => out.push_str(&format!("@@ {} (whole)\n{text}\n", names[file])),
            Some(name) => match let_lines(text, &name) {
                Some((a, b)) => out.push_str(&format!("@@ {}\n{}\n", names[file], &text[a..b])),
                None => out.push_str(&format!("({} has no `let {name}` then)\n", names[file])),
            },
        }
    }
    Ok(out)
}

/// What changed between two steps: per file, the changed passage (new
/// lines, the old ones they replace) and the top-level `let`s it touches.
pub fn diff(session: &MotionSession, a: &str, b: &str) -> Result<String, String> {
    let (la, ta) = texts_at(session, a)?;
    let (lb, tb) = texts_at(session, b)?;
    let names = names(session);
    let mut out = format!("From {la} to {lb}:\n");
    let mut any = false;
    for ((file, old), (_, new)) in ta.iter().zip(tb.iter()) {
        if old == new {
            continue;
        }
        any = true;
        let (o, n): (Vec<&str>, Vec<&str>) = (old.split('\n').collect(), new.split('\n').collect());
        let head = o.iter().zip(n.iter()).take_while(|(x, y)| x == y).count();
        let tail = o[head..].iter().rev().zip(n[head..].iter().rev()).take_while(|(x, y)| x == y).count();
        let (old_lines, new_lines) = (&o[head..o.len() - tail], &n[head..n.len() - tail]);
        // The top-level `let`s the changed lines are in (the new text's).
        let outline = code::outline(new);
        let (from, to) = (n[..head].iter().map(|l| l.len() + 1).sum::<usize>(), n[..n.len() - tail].iter().map(|l| l.len() + 1).sum::<usize>());
        let touched: Vec<&str> = outline.regions.iter().filter(|r| r.parent.is_none() && r.range.start < to && r.range.end > from).map(|r| r.name.as_str()).collect();
        out.push_str(&format!("@@ {} lines {}-{} (touches: {})\n", names[*file], head + 1, head + new_lines.len(), if touched.is_empty() { "-".into() } else { touched.join(", ") }));
        let cap = 40;
        for l in old_lines.iter().take(cap) {
            out.push_str(&format!("-{l}\n"));
        }
        for l in new_lines.iter().take(cap) {
            out.push_str(&format!("+{l}\n"));
        }
        if old_lines.len().max(new_lines.len()) > cap {
            out.push_str("(… more)\n");
        }
    }
    if !any {
        out.push_str("(nothing changed)\n");
    }
    Ok(out)
}

/// The changes that put `paths` back as they were at `step`, on the
/// graphic now: per changed `let` (or module), its file, the current
/// lines and the step's. What could not be taken is said.
pub fn merge_hunks(session: &MotionSession, step: &str, paths: &[String]) -> Result<(String, Vec<(usize, String, String)>, Vec<String>), String> {
    let (label, then) = texts_at(session, step)?;
    let names = names(session);
    let mut out: Vec<(usize, String, String)> = Vec::new();
    let mut skipped = Vec::new();
    let mut seen: Vec<(usize, Option<String>)> = Vec::new();
    for path in paths {
        let (file, name) = resolve(session, path)?;
        if seen.contains(&(file, name.clone())) {
            continue;
        }
        seen.push((file, name.clone()));
        let now = if file == 0 { session.source.clone() } else { session.modules[file - 1].source.clone() };
        let old = &then[file].1;
        match name {
            None => {
                if *old != now {
                    out.push((file, now, old.clone()));
                }
            }
            Some(name) => match (let_lines(&now, &name), let_lines(old, &name)) {
                (Some((a, b)), Some((c, d))) if now[a..b] != old[c..d] => out.push((file, now[a..b].to_string(), old[c..d].to_string())),
                (Some(_), Some(_)) => skipped.push(format!("`{name}` is the same then and now")),
                (None, _) => skipped.push(format!("{} has no `let {name}` now", names[file])),
                (_, None) => skipped.push(format!("{} had no `let {name}` at {label}", names[file])),
            },
        }
    }
    Ok((label, out, skipped))
}

/// The scene's agent's history commands (`HISTORY: …`, `HISTORY_DIFF: …`)
/// answered; `None` when the text is not one.
pub fn answer(session: &MotionSession, text: &str) -> Option<String> {
    let text = text.trim_start();
    let words = |rest: &str| rest.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    if let Some(rest) = text.strip_prefix("HISTORY_DIFF:") {
        let w = words(rest);
        let (a, b) = (w.first().map_or("picked", |s| s.as_str()).to_string(), w.get(1).map_or("now", |s| s.as_str()).to_string());
        return Some(diff(session, &a, &b).unwrap_or_else(|e| e));
    }
    if let Some(rest) = text.strip_prefix("HISTORY:") {
        let w = words(rest);
        if w.is_empty() {
            return Some(listing(session, 30));
        }
        // A step and the names to read there ("N min ago" is one step).
        let (step, names): (String, Vec<String>) = match w.iter().position(|x| x == "ago") {
            Some(at) => (w[..=at].join(" "), w[at + 1..].to_vec()),
            None => (w[0].clone(), w[1..].to_vec()),
        };
        return Some(if names.is_empty() { diff(session, &step, "now") } else { read(session, &step, &names) }.unwrap_or_else(|e| e));
    }
    None
}

/// What the agent is told of the history (with a chat turn's context).
pub fn context(page: &VideoCut) -> String {
    let Some(session) = page.motion.desk.session.as_ref() else { return String::new() };
    if session.history.len() <= 1 && PICKED.with(|p| p.borrow().is_none()) {
        return String::new();
    }
    format!(
        "The graphic's history (the newest last):\n{}To read it answer only `HISTORY: <step> [names]` (a step's code of those `let`s, or what changed since it), `HISTORY_DIFF: <a> <b>`; to take how things were at a step (\"the way the unicorn looked from here\"), answer only `MERGE: <step> <paths>` (top-level `let`s, `module.NAME`, `module:<name>`): it is shown as a variant to choose. A step is a number, `picked`, or `N min ago`.\n",
        listing(session, 10)
    )
}

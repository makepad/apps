//! The deep make: a scene's editor panel built element by element from an
//! audit of what a designer would want to change, not only from the values
//! the code already names.
//!
//! 1. Elements: the scene's agent reads the whole scene (its document and
//!    its own modules, numbered) and names its main visual elements as a
//!    viewer sees them, each with the code lines that make it.
//! 2. Audits, one writer per element at once: what a designer changes in
//!    it, timing first (when it enters, peaks, leaves; durations, eases,
//!    anchors to beats and words), then its procedural values (shape,
//!    motion character, density, colour), then details. Each is bound to a
//!    path that exists, or to be exposed (a new name, its value now, the
//!    lines it is computed on).
//! 3. Expose, one writer per element at once: the exposures as a variant
//!    (a named value used where it is computed, the picture the same at its
//!    default).
//! 4. Check: each variant drawn at five moments beside the picture as
//!    written (`panel_sweep::start_compare`); one that changes any of it is
//!    refused, the rest applied together (one undo step).
//! 5. Panel, one writer per element at once (and one for the overview):
//!    the element's module (its key controls on its heading, then Timing,
//!    Shape and motion, Details), written in as each arrives.
//!
//! Dig deeper (`start_element`): the same for one element, its module
//! replaced. Texts are bound where they are strings; baked text is AN1's to
//! turn into strings, not exposed here.
//!
//! Main-loop cost: a state read per pump while one runs.

use super::super::VideoCut;
use makepad_motion::paths;
use makepad_widgets::*;
use std::collections::HashMap;

/// A pixel share above which an exposure changed the picture.
const IDENTICAL: f64 = 0.0;
/// The moments (shares of the graphic) the exposures are drawn at.
const MOMENTS: [f64; 5] = [0.1, 0.3, 0.5, 0.7, 0.9];

const ELEMENTS_SYSTEM: &str = "You read a motion graphic written in Splash and name its main visual elements as a \
viewer sees them (the lyrics, the creature, the axes and grid, a listing, the sparks, the camera, the post look): five \
to ten, each with the code that makes it, numbered as in the listing. One line each, nothing else:\n\
ELEMENT | <its name, in a viewer's words> | <what it is, a few words> | <file>:<from>-<to>[, <file>:<from>-<to>...]";

const AUDIT_SYSTEM: &str = "You audit one element of a motion graphic, written in Splash, for its editor panel: what \
would a designer want to change in it? List 10 to 25 things, in this order: its timing first (when it enters, peaks \
and leaves, durations, eases, its anchors to beats and words), then its procedural values (shape, motion character: \
speed, amplitude, wobble, jitter, damping; counts and densities; colours and materials; fonts), then details. One \
line each, nothing else, in one of two forms:\n\
BIND | <what changes, in a designer's words> | <a path from the list>\n\
EXPOSE | <what changes> | <NEW_NAME> = <its value now> | <file>:<line>[,<line>...]\n\
BIND when one listed path is that thing (prefer paths not marked RELOAD). EXPOSE when no one value is: it is computed, \
hard-coded in several places, or written inside an expression; NEW_NAME is a fresh capitalised name, its value the one \
that keeps the picture as it is, and the lines are where it is computed or written (the numbers in the listing). \
Texts: BIND their string literal's path when there is one; never EXPOSE a text.";

#[derive(Clone, Debug)]
pub struct Element {
    pub name: String,
    pub what: String,
    /// (session file, first line, last line), lines from 0.
    pub ranges: Vec<(usize, usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub element: usize,
    pub bind: bool,
    pub what: String,
    /// The path bound, or the new name.
    pub path: String,
    pub default: String,
    /// (session file, line) where an exposure is computed.
    pub places: Vec<(usize, usize)>,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub elements: Vec<String>,
    pub found: usize,
    pub bind: usize,
    pub expose_asked: usize,
    pub exposed: Vec<String>,
    pub refused: Vec<String>,
    pub bound: usize,
    /// Per element: (name, found, exposed, bound).
    pub per_element: Vec<(String, usize, usize, usize)>,
    pub checks: Vec<String>,
    pub stages: Vec<String>,
    pub seconds: f64,
    pub failed: Option<String>,
}

enum Stage {
    Elements(u64),
    Audits(u64),
    Expose(u64),
    Check,
    /// The exposures kept, drawn together (identical alone is not enough:
    /// two can change the same thing).
    Together(Vec<u64>),
    Panel(u64),
    Done,
}

pub struct Deep {
    scene: String,
    brief: String,
    stage: Stage,
    elements: Vec<Element>,
    /// The elements this run makes (all, or the one dug into).
    only: Option<usize>,
    items: Vec<Item>,
    /// Exposure writer index -> element.
    expose_of: Vec<usize>,
    /// Variant id -> element, for the check.
    candidates: Vec<(u64, usize)>,
    /// Panel writer index -> element (None: the overview).
    part_of: Vec<Option<usize>>,
    /// The panel writers' messages, and the parts asked again once (a
    /// module that did not run).
    part_texts: Vec<String>,
    parts_retried: bool,
    /// Exposures accepted so far, and whether the refused were asked again.
    accepted: Vec<u64>,
    retried: bool,
    /// Why each element's exposures were refused (asked again with it).
    refusals: HashMap<usize, String>,
    /// Exposures only in the scene's own document (a prefab: modules other
    /// scenes share stay as they are).
    pub doc_only: bool,
    /// A whole make's panel was started anew (its first module written).
    fresh_written: bool,
    pub report: Report,
    started: f64,
    stage_started: f64,
}

/// The scene's code, numbered per file (its own files only), or the lines
/// of these ranges.
fn numbered(page: &VideoCut, ranges: Option<&[(usize, usize, usize)]>) -> String {
    let Some(session) = page.motion.desk.session.as_ref() else { return String::new() };
    let names = super::variants::file_names(page);
    let mut out = String::new();
    let files = std::iter::once((0usize, session.source.as_str()))
        .chain(session.modules.iter().enumerate().filter(|(_, m)| !m.builtin()).map(|(k, m)| (k + 1, m.source.as_str())));
    for (index, text) in files {
        let lines: Vec<&str> = text.split('\n').collect();
        let wanted: Vec<(usize, usize)> = match ranges {
            None => vec![(0, lines.len().saturating_sub(1))],
            Some(r) => r.iter().filter(|(f, _, _)| *f == index).map(|(_, a, b)| (*a, (*b).min(lines.len().saturating_sub(1)))).collect(),
        };
        if wanted.is_empty() {
            continue;
        }
        let name = names.iter().find(|(i, _)| *i == index).map_or("doc".to_string(), |(_, n)| n.clone());
        out.push_str(&format!("@@ {name}\n"));
        for (a, b) in wanted {
            for i in a..=b {
                out.push_str(&format!("{:>4}| {}\n", i + 1, lines[i]));
            }
            out.push_str("   …\n");
        }
        out.push('\n');
    }
    out
}

fn file_index(page: &VideoCut, name: &str) -> usize {
    super::variants::file_names(page).into_iter().find(|(_, n)| n == name.trim()).map_or(0, |(i, _)| i)
}

fn parse_elements(page: &VideoCut, text: &str) -> Vec<Element> {
    let mut out = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
        if parts.first() != Some(&"ELEMENT") || parts.len() < 4 {
            continue;
        }
        let ranges = parts[3].split(',').filter_map(|r| {
            let (file, span) = r.trim().split_once(':')?;
            let (a, b) = span.split_once('-').unwrap_or((span, span));
            let (a, b) = (a.trim().parse::<usize>().ok()?, b.trim().parse::<usize>().ok()?);
            Some((file_index(page, file), a.saturating_sub(1), b.saturating_sub(1).max(a.saturating_sub(1))))
        }).collect::<Vec<_>>();
        if !ranges.is_empty() {
            // (A name is a module's title: no quotes in it.)
            let name = parts[1].replace(['"', '\\'], "'");
            out.push(Element { name, what: parts[2].replace('"', "'"), ranges });
        }
    }
    out.truncate(12);
    out
}

fn parse_items(page: &VideoCut, element: usize, text: &str) -> Vec<Item> {
    let mut out = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
        match parts.first().copied() {
            Some("BIND") if parts.len() >= 3 => out.push(Item { element, bind: true, what: parts[1].into(), path: parts[2].trim_matches('`').into(), default: String::new(), places: Vec::new() }),
            Some("EXPOSE") if parts.len() >= 4 => {
                let (name, default) = parts[2].split_once('=').map(|(a, b)| (a.trim().trim_matches('`').to_string(), b.trim().to_string())).unwrap_or((parts[2].to_string(), String::new()));
                let places = parts.get(3).and_then(|p| p.split_once(':')).map(|(file, lines)| {
                    let f = file_index(page, file);
                    lines.split([',', ' ']).filter_map(|l| l.trim().parse::<usize>().ok()).map(|l| (f, l.saturating_sub(1))).collect()
                }).unwrap_or_default();
                out.push(Item { element, bind: false, what: parts[1].into(), path: name, default, places });
            }
            _ => {}
        }
    }
    out.truncate(30);
    out
}

thread_local! {
    /// Each scene's elements, as last found (a dig deeper starts from them).
    static ELEMENTS: std::cell::RefCell<HashMap<String, Vec<Element>>> = std::cell::RefCell::new(HashMap::new());
}

fn begin(page: &mut VideoCut, brief: &str) -> Result<String, String> {
    if page.motion.deep.as_ref().is_some_and(|d| !matches!(d.stage, Stage::Done)) {
        return Err("a deep make is running".into());
    }
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let scene = session.node.clone().unwrap_or_else(|| session.title.clone());
    page.motion.deep = Some(Deep {
        scene: scene.clone(), brief: brief.trim().to_string(), stage: Stage::Done, elements: Vec::new(), only: None, items: Vec::new(),
        expose_of: Vec::new(), candidates: Vec::new(), part_of: Vec::new(), part_texts: Vec::new(), parts_retried: false, accepted: Vec::new(), retried: false, refusals: HashMap::new(), doc_only: false, fresh_written: false,
        report: Report::default(), started: Cx::time_now(), stage_started: Cx::time_now(),
    });
    Ok(scene)
}

/// Start a deep make of the open scene's panel (`brief`: what a remake
/// should follow).
pub fn start(page: &mut VideoCut, cx: &mut Cx, brief: &str) -> Result<(), String> {
    if page.motion.deep.as_ref().is_some_and(|d| !matches!(d.stage, Stage::Done)) {
        return Ok(());
    }
    let title = page.motion.desk.session.as_ref().map(|s| s.title.clone()).unwrap_or_default();
    let text = format!("The scene: {title}.{}\nThe code:\n{}", if brief.trim().is_empty() { String::new() } else { format!(" The person asks: {}.", brief.trim()) }, numbered(page, None));
    begin(page, brief)?;
    let serial = super::variants::raw_request(page, cx, "elements", ELEMENTS_SYSTEM, vec![text])?;
    if let Some(deep) = page.motion.deep.as_mut() {
        deep.stage = Stage::Elements(serial);
    }
    log!("deep make: elements asked");
    Ok(())
}

/// Dig deeper into one element (by name, or by the paths of its module):
/// audited again for more, exposed, checked, its module made anew.
pub fn start_element(page: &mut VideoCut, cx: &mut Cx, name: &str, paths: &[String], brief: &str) -> Result<(), String> {
    let scene = page.motion.desk.session.as_ref().map(|s| s.node.clone().unwrap_or_else(|| s.title.clone())).ok_or("no graphic is open")?;
    let Some(elements) = ELEMENTS.with(|e| e.borrow().get(&scene).cloned()) else {
        return start(page, cx, brief);
    };
    let lower = name.to_lowercase();
    let found = elements.iter().position(|e| !lower.is_empty() && (e.name.to_lowercase().contains(&lower) || lower.contains(&e.name.to_lowercase())));
    // (By paths: the element whose code holds most of them.)
    let found = found.or_else(|| {
        let session = page.motion.desk.session.as_ref()?;
        let files: Vec<paths::File> = std::iter::once(paths::File { name: "", text: &session.source, code: &session.code })
            .chain(session.modules.iter().filter(|m| !m.builtin()).map(|m| paths::File { name: &m.name, text: &m.source, code: &m.code }))
            .collect();
        (0..elements.len()).max_by_key(|&k| paths.iter().filter(|p| paths::resolve(&files, p).ok().is_some_and(|site| {
            let line = files[site.file].text[..site.range.start].matches('\n').count();
            elements[k].ranges.iter().any(|(f, a, b)| *f == site.file && line >= *a && line <= *b)
        })).count())
    });
    let Some(k) = found else { return Err(format!("no element `{name}`")) };
    begin(page, &format!("{} Dig deeper: more than its present controls, the finer things too.", brief.trim()))?;
    if let Some(deep) = page.motion.deep.as_mut() {
        deep.elements = elements;
        deep.only = Some(k);
        deep.report.elements = vec![deep.elements[k].name.clone()];
    }
    ask_audits(page, cx)
}

fn fail(page: &mut VideoCut, why: String) {
    if let Some(deep) = page.motion.deep.as_mut() {
        log!("deep make: failed: {why}");
        deep.report.failed = Some(why);
        deep.report.seconds = Cx::time_now() - deep.started;
        deep.stage = Stage::Done;
    }
}

fn stage_done(deep: &mut Deep, what: &str) {
    let line = format!("{what} {:.1} s", Cx::time_now() - deep.stage_started);
    log!("deep make: {line}");
    deep.report.stages.push(line);
    deep.stage_started = Cx::time_now();
}

/// The audits of the elements this run makes, one writer each.
fn ask_audits(page: &mut VideoCut, cx: &mut Cx) -> Result<(), String> {
    let deep = page.motion.deep.as_ref().ok_or("no deep make")?;
    let listing = page.motion.desk.session.as_ref().map(|s| super::editor_panel::paths_listing(s)).unwrap_or_default();
    let which: Vec<usize> = match deep.only { Some(k) => vec![k], None => (0..deep.elements.len()).collect() };
    let mut texts = Vec::new();
    for &k in &which {
        let e = &deep.elements[k];
        let code = numbered(page, Some(&e.ranges));
        // The paths its code names (a `let` it reads, a module's member).
        let words: std::collections::HashSet<String> = code.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| w.len() >= 3).map(str::to_string).collect();
        let paths: String = listing.lines().filter(|l| {
            let path = l.split(' ').next().unwrap_or("");
            path.split(['.', '[']).take(2).any(|p| words.contains(p))
        }).collect::<Vec<_>>().join("\n");
        texts.push(format!(
            "The element: {} ({}) of the scene.{}\nThe paths a control can bind that its code names (path (kind) = value  // what it is for):\n{paths}\nIts code:\n{code}",
            e.name, e.what, if deep.brief.is_empty() { String::new() } else { format!(" {}", deep.brief) }
        ));
    }
    let serial = super::variants::raw_request(page, cx, "audit", AUDIT_SYSTEM, texts)?;
    if let Some(deep) = page.motion.deep.as_mut() {
        deep.stage = Stage::Audits(serial);
        deep.expose_of = which;
    }
    Ok(())
}

/// On the desk's pump: the stage that finished moves the make on.
pub fn pump(page: &mut VideoCut, cx: &mut Cx) {
    let Some(deep) = page.motion.deep.as_ref() else { return };
    let open = page.motion.desk.session.as_ref().map(|s| s.node.clone().unwrap_or_else(|| s.title.clone()));
    if !matches!(deep.stage, Stage::Done) && open.as_deref() != Some(deep.scene.as_str()) {
        fail(page, "another graphic was opened".into());
        return;
    }
    match deep.stage {
        Stage::Elements(serial) => {
            let Some(answer) = super::variants::raw_answers(page, serial) else { return };
            let elements = match answer {
                Ok(texts) => parse_elements(page, &texts.join("\n")),
                Err(why) => return fail(page, format!("the elements: {why}")),
            };
            if elements.is_empty() {
                return fail(page, "no element was named".into());
            }
            let deep = page.motion.deep.as_mut().expect("running");
            deep.report.elements = elements.iter().map(|e| e.name.clone()).collect();
            ELEMENTS.with(|e| e.borrow_mut().insert(deep.scene.clone(), elements.clone()));
            deep.elements = elements;
            stage_done(deep, &format!("{} elements", deep.report.elements.len()));
            if let Err(why) = ask_audits(page, cx) {
                fail(page, format!("the audits: {why}"));
            }
        }
        Stage::Audits(serial) => {
            let Some(answer) = super::variants::raw_answers(page, serial) else { return };
            let texts = match answer {
                Ok(texts) => texts,
                Err(why) => return fail(page, format!("the audits: {why}")),
            };
            let which = deep.expose_of.clone();
            let mut items = Vec::new();
            for (k, text) in which.iter().zip(texts.iter()) {
                items.extend(parse_items(page, *k, text));
            }
            if items.is_empty() {
                return fail(page, "the audits listed nothing".into());
            }
            let deep = page.motion.deep.as_mut().expect("running");
            deep.report.found += items.len();
            deep.report.bind += items.iter().filter(|i| i.bind).count();
            deep.report.expose_asked += items.iter().filter(|i| !i.bind).count();
            deep.items = items;
            stage_done(deep, &format!("audits: {} things ({} to expose)", deep.report.found, deep.report.expose_asked));
            ask_exposures(page, cx, which);
        }
        Stage::Expose(serial) => {
            let Some(made) = super::variants::request_variants(page, serial) else { return };
            let deep = page.motion.deep.as_mut().expect("running");
            let mut candidates = Vec::new();
            deep.candidates.clear();
            for (id, writer, label, texts) in made {
                let Some(&k) = deep.expose_of.get(writer) else { continue };
                match texts {
                    Ok(texts) => {
                        deep.candidates.push((id, k));
                        candidates.push((id.to_string(), texts));
                    }
                    Err(why) => {
                        deep.refusals.insert(k, format!("it did not load ({why})"));
                        deep.report.refused.push(format!("{} ({label}): {why}", deep.elements[k].name));
                    }
                }
            }
            stage_done(deep, &format!("exposures written: {} of {}", candidates.len(), deep.expose_of.len()));
            if candidates.is_empty() {
                return after_check(page, cx);
            }
            let duration = page.motion.desk.session.as_ref().map_or(1.0, |s| s.duration());
            let times: Vec<f64> = MOMENTS.iter().map(|f| f * duration).collect();
            match super::panel_sweep::start_compare(page, serial, candidates, &times) {
                Ok(_) => page.motion.deep.as_mut().expect("running").stage = Stage::Check,
                Err(why) => fail(page, format!("the frame check: {why}")),
            }
        }
        Stage::Check => {
            let Some((_, measured, failures)) = super::panel_sweep::take_compared() else { return };
            let deep = page.motion.deep.as_mut().expect("running");
            let mut accept = Vec::new();
            for m in &measured {
                let Ok(id) = m.control.parse::<u64>() else { continue };
                let Some(&(_, k)) = deep.candidates.iter().find(|(i, _)| *i == id) else { continue };
                let names: Vec<String> = deep.items.iter().filter(|i| i.element == k && !i.bind).map(|i| i.path.clone()).collect();
                deep.report.checks.push(format!("{}: {:.3}% of pixels differ at worst over {} moments", deep.elements[k].name, m.changed * 100.0, MOMENTS.len()));
                if m.changed <= IDENTICAL {
                    accept.push(id);
                    deep.report.exposed.extend(names);
                } else {
                    deep.refusals.insert(k, format!("it changed {:.2}% of the picture's pixels", m.changed * 100.0));
                    deep.report.refused.push(format!("{}: its exposures changed the picture ({:.2}%)", deep.elements[k].name, m.changed * 100.0));
                }
            }
            for f in failures.iter().take(3) {
                log!("deep make: check: {f}");
            }
            stage_done(deep, &format!("checked: {} of {} identical", accept.len(), measured.len()));
            deep.accepted.extend(accept);
            after_check(page, cx);
        }
        Stage::Together(ref kept) => {
            let kept = kept.clone();
            let Some((_, measured, _)) = super::panel_sweep::take_compared() else { return };
            let changed = measured.first().map_or(1.0, |m| m.changed);
            if changed <= IDENTICAL {
                return apply_kept(page, cx, kept);
            }
            // The last one left out, and the rest drawn again.
            let mut kept = kept;
            let dropped = kept.pop();
            let deep = page.motion.deep.as_mut().expect("running");
            if let Some(&(_, k)) = dropped.and_then(|id| deep.candidates.iter().find(|(i, _)| *i == id)) {
                let names: Vec<String> = deep.items.iter().filter(|i| i.element == k && !i.bind).map(|i| i.path.clone()).collect();
                deep.report.exposed.retain(|n| !names.contains(n));
                deep.report.refused.push(format!("{}: with the others it changed the picture ({:.2}%)", deep.elements[k].name, changed * 100.0));
            }
            if kept.len() > 1 { check_together(page, cx, kept) } else { apply_kept(page, cx, kept) }
        }
        Stage::Panel(serial) => {
            // Each part written in as it arrives.
            stream_parts(page, cx, serial);
        }
        Stage::Done => {}
    }
}

/// The exposures of these elements, one writer each that has some (with
/// why a first try was refused, when it was).
fn ask_exposures(page: &mut VideoCut, cx: &mut Cx, which: Vec<usize>) {
    let deep = page.motion.deep.as_ref().expect("running");
    let only_doc = if deep.doc_only { " Only in the document `doc`: values written in a module stay as they are (leave them out)." } else { "" };
    let mut heads = Vec::new();
    let mut of = Vec::new();
    for &k in &which {
        let mine: Vec<&Item> = deep.items.iter().filter(|i| i.element == k && !i.bind && (!deep.doc_only || i.places.iter().all(|(f, _)| *f == 0))).collect();
        if mine.is_empty() {
            continue;
        }
        let places: Vec<(usize, usize)> = mine.iter().flat_map(|i| i.places.iter().copied()).collect();
        let wanted = super::variants::lines_around(page, &places, 12);
        let list: String = mine.iter().map(|i| format!("- {} = {}: {} (lines {})\n", i.path, i.default, i.what, i.places.iter().map(|(_, l)| (l + 1).to_string()).collect::<Vec<_>>().join(", "))).collect();
        let again = deep.refusals.get(&k).map(|why| format!(" Your first try was refused: {why}. This time keep every value exactly as it was computed (same operations in the same order, same constants); expose fewer values rather than change one.")).unwrap_or_default();
        heads.push((format!(
            "Expose these values of {} for the editor panel. Write variant {{number}}: one block (its label and note say what it exposes) that, for each, introduces a named top-level `let` (or a field of the record that holds its siblings) whose value is the one written now, and uses it where the value is computed or written, so the picture stays exactly the same at that value. Change nothing else; keep every other line. Leave out any you cannot expose that way. A value inside code the GPU runs (a shader's or a pass's pixel and effect functions) cannot read a `let`: pass it in the way that code already takes its values (its params or uniforms), or leave it out. Give each hunk enough unchanged lines that its old lines occur once in the file.{only_doc}{again} If you need lines you were not shown, answer only `CODE: <the names or words>`.\n{list}",
            deep.elements[k].name
        ), wanted));
        of.push(k);
    }
    if heads.is_empty() {
        return finish_exposures(page, cx);
    }
    match super::variants::variant_request(page, cx, "deep: expose", heads) {
        Ok(serial) => {
            let deep = page.motion.deep.as_mut().expect("running");
            deep.expose_of = of;
            deep.stage = Stage::Expose(serial);
        }
        Err(why) => fail(page, format!("the exposures: {why}")),
    }
}

/// The accepted exposures applied together (one undo step), then the panel.
fn finish_exposures(page: &mut VideoCut, cx: &mut Cx) {
    // Those whose patches go together (elements touching the same lines
    // keep the first's).
    let all = page.motion.deep.as_ref().map(|d| d.accepted.clone()).unwrap_or_default();
    let accept = super::variants::compatible(page, &all);
    if accept.len() < all.len() {
        let deep = page.motion.deep.as_mut().expect("running");
        for id in all.iter().filter(|id| !accept.contains(id)) {
            if let Some(&(_, k)) = deep.candidates.iter().find(|(i, _)| i == id) {
                let names: Vec<String> = deep.items.iter().filter(|i| i.element == k && !i.bind).map(|i| i.path.clone()).collect();
                deep.report.exposed.retain(|n| !names.contains(n));
                deep.report.refused.push(format!("{}: its exposures touch lines another element's already change", deep.elements[k].name));
            }
        }
    }
    if accept.len() > 1 {
        return check_together(page, cx, accept);
    }
    apply_kept(page, cx, accept);
}

/// The kept exposures drawn together beside the picture as written.
fn check_together(page: &mut VideoCut, cx: &mut Cx, accept: Vec<u64>) {
    let texts = match super::variants::combined_texts(page, &accept) {
        Ok(texts) => texts,
        Err(_) => return apply_kept(page, cx, Vec::new()),
    };
    let duration = page.motion.desk.session.as_ref().map_or(1.0, |s| s.duration());
    let times: Vec<f64> = MOMENTS.iter().map(|f| f * duration).collect();
    match super::panel_sweep::start_compare(page, 0, vec![("together".into(), texts)], &times) {
        Ok(_) => page.motion.deep.as_mut().expect("running").stage = Stage::Together(accept),
        Err(_) => apply_kept(page, cx, Vec::new()),
    }
}

/// The kept exposures applied (one undo step), then the panel.
fn apply_kept(page: &mut VideoCut, cx: &mut Cx, accept: Vec<u64>) {
    if accept.is_empty() {
        page.motion.deep.as_mut().expect("running").report.exposed.clear();
    } else {
        if let Err(why) = super::variants::apply_variants(page, &accept) {
            let deep = page.motion.deep.as_mut().expect("running");
            deep.report.refused.push(format!("applying them: {why}"));
            deep.report.exposed.clear();
        }
    }
    // (The exposed names count only where they now resolve.)
    let listing = page.motion.desk.session.as_ref().map(|s| super::editor_panel::paths_listing(s)).unwrap_or_default();
    let deep = page.motion.deep.as_mut().expect("running");
    deep.report.exposed.retain(|n| listing.lines().any(|l| l.starts_with(&format!("{n} "))));
    ask_panel(page, cx);
}

/// After a check: the refused asked again once, else on to the panel.
fn after_check(page: &mut VideoCut, cx: &mut Cx) {
    let deep = page.motion.deep.as_mut().expect("running");
    let accepted_elements: Vec<usize> = deep.candidates.iter().filter(|(id, _)| deep.accepted.contains(id)).map(|(_, k)| *k).collect();
    let again: Vec<usize> = deep.refusals.keys().copied().filter(|k| !accepted_elements.contains(k)).collect();
    if !deep.retried && !again.is_empty() {
        deep.retried = true;
        log!("deep make: asking again for {}", again.iter().map(|k| deep.elements[*k].name.clone()).collect::<Vec<_>>().join(", "));
        return ask_exposures(page, cx, again);
    }
    finish_exposures(page, cx);
}

/// The panel's writers: one per element made (its module), and, for a
/// whole make, one for the overview.
fn ask_panel(page: &mut VideoCut, cx: &mut Cx) {
    let deep = page.motion.deep.as_ref().expect("running");
    let listing = page.motion.desk.session.as_ref().map(|s| super::editor_panel::paths_listing(s)).unwrap_or_default();
    let line_of = |path: &str| listing.lines().find(|l| l.starts_with(&format!("{path} "))).map(str::to_string);
    let which: Vec<usize> = match deep.only { Some(k) => vec![k], None => (0..deep.elements.len()).collect() };
    let steer = if deep.brief.is_empty() { String::new() } else { format!(" The person asks: {}.", deep.brief) };
    let mut texts = Vec::new();
    let mut part_of = Vec::new();
    for &k in &which {
        let e = &deep.elements[k];
        let mine: Vec<String> = deep.items.iter().filter(|i| i.element == k && (i.bind || deep.report.exposed.contains(&i.path))).filter_map(|i| line_of(&i.path).map(|l| format!("{l}   [{}]", i.what))).collect();
        if mine.is_empty() {
            continue;
        }
        texts.push(format!(
            "Write the module for {} ({}): one `PanelModule{{title: \"{}\" when: \"…\" …}}` (a when only if it does not act throughout). Its two or three key controls first (what a person reaches for: its colour, size, on/off), then `PanelGroup{{title: \"Timing\" …}}` (when it enters, peaks, leaves; durations, eases, anchors), then `PanelGroup{{title: \"Shape and motion\" …}}` (its procedural values), then `PanelGroup{{title: \"Details\" …}}` (the rest). Bind every path below that fits, each once, each range kept within four times the value written either way (a count far past that can stall the picture), each control labelled with what it changes in a designer's words (`label: \"Horn length\"`), never the path.{steer}\nIts paths (path (kind) = value  // label   [what a designer calls it]):\n{}",
            e.name, e.what, e.name, mine.join("\n")
        ));
        part_of.push(Some(k));
    }
    if deep.only.is_none() {
        let all: String = deep.items.iter().filter(|i| i.bind || deep.report.exposed.contains(&i.path)).filter_map(|i| line_of(&i.path).map(|l| format!("{l}   [{}: {}]", deep.elements[i.element].name, i.what))).collect::<Vec<_>>().join("\n");
        texts.insert(0, format!("Write the panel's overview: one `PanelModule{{title: \"Overview\" open: true …}}` of four to eight `PanelMacro`s that move the whole scene's feel (energy, calm, scale, pace) through these paths (macros, not single controls: the elements' modules hold those); nothing else.{steer}\nThe paths:\n{all}"));
        part_of.insert(0, None);
    }
    if texts.is_empty() {
        return fail(page, "nothing to bind".into());
    }
    match super::variants::raw_request(page, cx, "panel parts", super::variants::panel_system_text(), texts.clone()) {
        Ok(serial) => {
            let deep = page.motion.deep.as_mut().expect("running");
            deep.part_of = part_of;
            deep.part_texts = texts;
            deep.stage = Stage::Panel(serial);
        }
        Err(why) => fail(page, format!("the panel: {why}")),
    }
}

/// The parts arrived so far, checked and written into the panel (each
/// element's module in the elements' order, the overview first; a dug-into
/// element's module replaces its old one), one undo step for the make.
fn stream_parts(page: &mut VideoCut, cx: &mut Cx, serial: u64) {
    let Some((parts, complete)) = super::variants::raw_parts(page, serial) else { return };
    let deep = page.motion.deep.as_mut().expect("running");
    let fresh: Vec<(usize, String)> = parts.into_iter().filter(|(k, _)| !deep.report.stages.iter().any(|s| s == &format!("part {k}"))).collect();
    if fresh.is_empty() && !complete {
        return;
    }
    let mut modules: Vec<(String, String)> = Vec::new();
    for (k, text) in fresh {
        deep.report.stages.push(format!("part {k}"));
        let body: String = text.lines().filter(|l| !l.trim_start().starts_with("```")).collect::<Vec<_>>().join("\n");
        let body = body.trim().to_string();
        if !body.starts_with("Panel") {
            // (Words, or a module behind words: the module itself taken.)
            let Some(at) = body.find("PanelModule{") else {
                let title = deep.part_of.get(k).copied().flatten().map_or("Overview".to_string(), |e| deep.elements[e].name.clone());
                deep.report.refused.push(format!("{title}: no module came back ({})", body.chars().take(80).collect::<String>()));
                continue;
            };
            let body = body[at..].to_string();
            let errors = makepad_widgets::splash::validate_splash_body(cx, &body, false);
            if errors.is_empty() {
                let title = match deep.part_of.get(k).copied().flatten() { Some(e) => deep.elements[e].name.clone(), None => "Overview".to_string() };
                modules.push((title, body));
            } else {
                deep.report.stages.push(format!("retry {k} {}", errors[0]));
            }
            continue;
        }
        let errors = makepad_widgets::splash::validate_splash_body(cx, &body, false);
        if let Some(error) = errors.first() {
            // Asked again once with why, when the parts are all in.
            deep.report.stages.push(format!("retry {k} {error}"));
            continue;
        }
        let title = match deep.part_of.get(k).copied().flatten() { Some(e) => deep.elements[e].name.clone(), None => "Overview".to_string() };
        modules.push((title, body));
    }
    let key = format!("deep:{serial}");
    for (title, body) in modules {
        // The first module of a whole make starts the panel anew.
        let deep = page.motion.deep.as_mut().expect("running");
        let fresh = deep.only.is_none() && !deep.fresh_written;
        deep.fresh_written = true;
        if let Err(why) = super::variants::panel_put_module(page, &title, &body, &key, fresh) {
            let deep = page.motion.deep.as_mut().expect("running");
            deep.report.refused.push(format!("{title}: {why}"));
        }
    }
    if !complete {
        return;
    }
    // Modules that did not run: written again once, told why.
    let deep = page.motion.deep.as_mut().expect("running");
    let retries: Vec<(usize, String)> = deep.report.stages.iter().filter_map(|s| s.strip_prefix("retry ")).filter_map(|r| r.split_once(' ').and_then(|(k, e)| Some((k.parse().ok()?, e.to_string())))).collect();
    deep.report.stages.retain(|s| !s.starts_with("retry "));
    if !retries.is_empty() {
        if deep.parts_retried {
            for (k, error) in &retries {
                let title = deep.part_of.get(*k).copied().flatten().map_or("Overview".to_string(), |e| deep.elements[e].name.clone());
                deep.report.refused.push(format!("{title}: its module did not run: {error}"));
            }
        } else {
            deep.parts_retried = true;
            let texts: Vec<String> = retries.iter().filter_map(|(k, error)| deep.part_texts.get(*k).map(|t| format!("{t}\nYour first module did not run: {error}. Write it again, only widgets and fields of the catalogue, every brace closed."))).collect();
            let part_of: Vec<Option<usize>> = retries.iter().map(|(k, _)| deep.part_of.get(*k).copied().flatten()).collect();
            log!("deep make: {} modules asked again", texts.len());
            deep.report.stages.retain(|s| !s.starts_with("part "));
            match super::variants::raw_request(page, cx, "panel parts", super::variants::panel_system_text(), texts.clone()) {
                Ok(serial) => {
                    let deep = page.motion.deep.as_mut().expect("running");
                    deep.part_of = part_of;
                    deep.part_texts = texts;
                    deep.stage = Stage::Panel(serial);
                    return;
                }
                Err(why) => log!("deep make: modules not asked again: {why}"),
            }
        }
    }
    if let Some(session) = page.motion.desk.session.as_mut() {
        session.end_burst();
    }
    let body = page.motion.desk.session.as_ref().and_then(|s| super::editor_panel::body_of(&s.source, &s.code)).unwrap_or_default();
    let deep = page.motion.deep.as_mut().expect("running");
    deep.report.stages.retain(|s| !s.starts_with("part "));
    let bound = |i: &Item| (i.bind || deep.report.exposed.contains(&i.path)) && body.contains(&format!("\"{}", i.path));
    deep.report.bound = deep.items.iter().filter(|i| bound(i)).count();
    deep.report.per_element = (0..deep.elements.len()).filter(|k| deep.only.is_none_or(|o| o == *k)).map(|k| {
        let mine: Vec<&Item> = deep.items.iter().filter(|i| i.element == k).collect();
        (deep.elements[k].name.clone(), mine.len(), mine.iter().filter(|i| !i.bind && deep.report.exposed.contains(&i.path)).count(), mine.iter().filter(|i| bound(i)).count())
    }).collect();
    stage_done(deep, "panel written");
    deep.report.seconds = Cx::time_now() - deep.started;
    deep.stage = Stage::Done;
    log!("deep make: done in {:.1} s: {} things found ({} bind, {} to expose), {} exposed, {} refused, {} bound in the panel; {}", deep.report.seconds, deep.report.found, deep.report.bind, deep.report.expose_asked, deep.report.exposed.len(), deep.report.refused.len(), deep.report.bound, deep.report.per_element.iter().map(|(n, f, e, b)| format!("{n} {f}/{e}/{b}")).collect::<Vec<_>>().join(", "));
    // Measured on the picture like any made panel.
    super::variants::self_check(page, cx, serial);
}

/// What the panel slot says while a deep make runs: (text, busy).
pub fn progress(page: &VideoCut) -> Option<(String, bool)> {
    let deep = page.motion.deep.as_ref()?;
    let secs = Cx::time_now() - deep.started;
    let text = match deep.stage {
        Stage::Elements(_) => "reading the whole scene: its elements".to_string(),
        Stage::Audits(_) => format!("what a designer would change in {}", deep.report.elements.join(", ")),
        Stage::Expose(_) => format!("exposing {} values no one name holds", deep.report.expose_asked),
        Stage::Check | Stage::Together(_) => "checking the picture stays the same".to_string(),
        Stage::Panel(_) => format!("writing the modules: {}", deep.report.elements.join(", ")),
        Stage::Done => return deep.report.failed.as_ref().map(|f| (format!("The deep make stopped: {f}"), false)),
    };
    if matches!(deep.stage, Stage::Panel(_)) && page.motion.desk.session.as_ref().is_some_and(|s| super::editor_panel::body_of(&s.source, &s.code).is_some()) {
        return None;
    }
    Some((format!("Making this scene's editor{} {secs:.0} s\n{text}\n{} things found", ".".repeat(1 + (secs as usize) % 3), deep.report.found), true))
}

/// Exposures only in the scene's own document (a prefab).
pub fn set_doc_only(page: &mut VideoCut, on: bool) {
    if let Some(deep) = page.motion.deep.as_mut() {
        deep.doc_only = on;
    }
}

/// The last deep make's report, for the control API.
pub fn report(page: &VideoCut) -> Option<(bool, Report)> {
    page.motion.deep.as_ref().map(|d| (matches!(d.stage, Stage::Done), d.report.clone()))
}

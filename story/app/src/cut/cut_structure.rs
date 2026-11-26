//! The cut's structure (STRUCTURE.md, phase 3: the cut level): the film
//! read from its output back, each picture through the stacks, effects,
//! transitions and reframes it passes, then its sound, as rows of the same
//! tree the graphic desk shows. A row selects its block, as a click on the
//! timeline does; nothing here is stored beside the cut's code.

use super::motion_edit::TreeRow;
use makepad_motion::structure::NodeKind;
use makepad_story_cut::{Blend, CutDoc, CutNodeKind, EffectRef, Span, Wire};
use std::collections::{HashMap, HashSet};

/// The film's own row (selecting it shows the project's settings).
pub const FILM: &str = "#film";
/// Sound and voices, and blocks no wire reaches.
const SOUND: &str = "#sound";
const LOOSE: &str = "#loose";

/// The rows of `doc`. `labels` names blocks as the timeline does (by node
/// id); `folded` lists the rows shut.
pub fn rows(doc: &CutDoc, labels: &HashMap<String, String>, folded: &HashSet<String>) -> Vec<TreeRow> {
    let mut out = vec![TreeRow { path: FILM.into(), depth: 0, label: doc.label.clone(), detail: format!("edit · {:.1} s", doc.duration), kind: NodeKind::Scene, on: true, open: None }];
    let mut seen = HashSet::new();
    let output = doc.node(&doc.output_node);
    if let Some(CutNodeKind::Output { frame: Some(frame), .. }) = output.map(|n| &n.kind) {
        walk(doc, labels, folded, &frame.node, 1, "", &mut seen, &mut out);
    }
    let audio: Vec<String> = match output.map(|n| &n.kind) {
        Some(CutNodeKind::Output { audio, .. }) => audio.iter().map(|w| w.node.clone()).collect(),
        _ => Vec::new(),
    };
    group(doc, labels, folded, SOUND, "Sound", &audio, &mut seen, &mut out);
    let all = doc.nodes.iter().map(|n| &n.id).chain(doc.graphics.iter().map(|g| &g.id)).chain(doc.generates.iter().map(|g| &g.id));
    let loose: Vec<String> = all.filter(|id| **id != doc.output_node && !seen.contains(*id)).cloned().collect();
    group(doc, labels, folded, LOOSE, "Not in the film", &loose, &mut seen, &mut out);
    out
}

/// A heading row over `ids`, when there are any.
fn group(doc: &CutDoc, labels: &HashMap<String, String>, folded: &HashSet<String>, path: &str, label: &str, ids: &[String], seen: &mut HashSet<String>, out: &mut Vec<TreeRow>) {
    let ids: Vec<&String> = ids.iter().filter(|id| !seen.contains(*id)).collect();
    if ids.is_empty() {
        return;
    }
    let shut = folded.contains(path);
    out.push(TreeRow { path: path.into(), depth: 1, label: label.into(), detail: ids.len().to_string(), kind: NodeKind::Code, on: true, open: Some(!shut) });
    for id in ids {
        if shut {
            seen.insert(id.clone());
        } else {
            walk(doc, labels, folded, id, 2, "", seen, out);
        }
    }
}

/// The row of node `id` at `depth`, then what feeds it. `role` says what
/// it is to its parent (a transition's `from`/`to`, a layer's blend).
#[allow(clippy::too_many_arguments)]
fn walk(doc: &CutDoc, labels: &HashMap<String, String>, folded: &HashSet<String>, id: &str, depth: usize, role: &str, seen: &mut HashSet<String>, out: &mut Vec<TreeRow>) {
    let Some(Seen { ty, span, feeds, fallback, kind }) = see(doc, id) else { return };
    let again = !seen.insert(id.to_string());
    let feeds = if again { Vec::new() } else { feeds };
    let label = labels.get(id).cloned().unwrap_or(fallback);
    // The type, unless the name already says it.
    let mut detail = if label.starts_with(ty) { String::new() } else { ty.to_lowercase() };
    if let Some(span) = span {
        detail = [detail, format!("{:.1}–{:.1} s", span.start, span.end())].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
    }
    if !role.is_empty() {
        detail = format!("{role} · {detail}");
    }
    if again {
        detail.push_str(" · also above");
    }
    let shut = folded.contains(id);
    out.push(TreeRow { path: id.to_string(), depth, label, detail, kind, on: true, open: (!feeds.is_empty()).then_some(!shut) });
    if shut {
        // Shut rows still count as reached.
        mark(doc, id, seen);
        return;
    }
    for (feed, role) in feeds {
        walk(doc, labels, folded, &feed, depth + 1, &role, seen, out);
    }
}

/// `id` and everything feeding it, as reached.
fn mark(doc: &CutDoc, id: &str, seen: &mut HashSet<String>) {
    for (from, _, to, _) in doc.wiring() {
        if to == id && seen.insert(from.to_string()) {
            mark(doc, from, seen);
        }
    }
}

/// What the tree shows of a node: its type, span, what feeds it (with
/// its role), a name when the timeline has none, and its row's kind.
/// `Graphic` and `Generate` nodes are kept apart from `nodes` by the
/// loader; wires name them like any node.
struct Seen {
    ty: &'static str,
    span: Option<Span>,
    feeds: Vec<(String, String)>,
    fallback: String,
    kind: NodeKind,
}

fn see(doc: &CutDoc, id: &str) -> Option<Seen> {
    let plain = |w: &Option<Wire>| w.iter().map(|w| (w.node.clone(), String::new())).collect::<Vec<_>>();
    if let Some(g) = doc.graphic(id) {
        return Some(Seen { ty: "Graphic", span: g.span, feeds: Vec::new(), fallback: id.to_string(), kind: NodeKind::Layer });
    }
    if let Some(g) = doc.generate(id) {
        return Some(Seen { ty: "Generate", span: Some(g.span), feeds: plain(&g.input), fallback: id.to_string(), kind: NodeKind::Generator });
    }
    let node = doc.node(id)?;
    let feeds = match &node.kind {
        // As written: the bottom layer first (on one track, in time).
        CutNodeKind::Stack { layers } => layers.iter().map(|(w, blend)| (w.node.clone(), if *blend == Blend::Normal { String::new() } else { format!("{blend:?}").to_lowercase() })).collect(),
        CutNodeKind::Fx { input, .. } | CutNodeKind::Reframe { input, .. } => plain(input),
        CutNodeKind::Transition { a, b, .. } => a.iter().map(|w| (w.node.clone(), "from".to_string())).chain(b.iter().map(|w| (w.node.clone(), "to".to_string()))).collect(),
        CutNodeKind::Subtitles { source, .. } => plain(source),
        _ => Vec::new(),
    };
    let fallback = match &node.kind {
        CutNodeKind::Title { text, .. } => text.clone(),
        CutNodeKind::Stack { layers } => format!("Stack of {}", layers.len()),
        CutNodeKind::Fx { effect: EffectRef::Preset(name), .. } => name.rsplit('/').next().unwrap_or(name).to_string(),
        kind => kind.type_name().to_string(),
    };
    let kind = match &node.kind {
        CutNodeKind::Fx { .. } | CutNodeKind::Transition { .. } | CutNodeKind::Reframe { .. } => NodeKind::Effect,
        CutNodeKind::Stack { .. } => NodeKind::Generator,
        _ => NodeKind::Layer,
    };
    Some(Seen { ty: node.kind.type_name(), span: node.span, feeds, fallback, kind })
}

/// The block a row selects: `None` for the film and the headings.
pub fn selects(path: &str) -> Option<Option<String>> {
    match path {
        FILM => Some(None),
        SOUND | LOOSE => None,
        id => Some(Some(id.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_film_reads_from_its_output_back() {
        let doc = CutDoc::load(include_str!("../../../libs/cut/tests/fixtures/launch.splash")).unwrap();
        let rows = rows(&doc, &HashMap::new(), &HashSet::new());
        let line = |r: &TreeRow| format!("{}{} [{}]", "  ".repeat(r.depth), r.label, r.detail);
        let text: Vec<String> = rows.iter().map(line).collect();
        let at = |id: &str| rows.iter().position(|r| r.path == id).unwrap_or_else(|| panic!("{id} missing:\n{}", text.join("\n")));
        let depth = |id: &str| rows[at(id)].depth;
        assert_eq!(rows[0].path, FILM);
        // The stack's layers as written, the picture down through its
        // effect and transition to the two clips.
        assert_eq!((depth("top"), depth("title"), depth("glow"), depth("wipe"), depth("intro"), depth("demo")), (1, 2, 2, 3, 4, 4), "{}", text.join("\n"));
        assert!(at("glow") < at("title") && at("intro") < at("demo"));
        assert!(rows[at("intro")].detail.starts_with("from"));
        // Then the sound.
        assert!(at(SOUND) > at("demo") && depth("music") == 2 && depth("voice") == 2);
        assert!(rows.iter().all(|r| r.path != LOOSE));
        // A shut row hides what feeds it, and nothing turns up loose.
        let shut = rows_with(&doc, &["top"]);
        assert!(shut.iter().all(|r| r.path != "glow" && r.path != LOOSE));
        assert_eq!(selects(FILM), Some(None));
        assert_eq!(selects("glow"), Some(Some("glow".into())));
        assert_eq!(selects(SOUND), None);
    }

    #[test]
    fn graphics_in_a_stack_are_rows() {
        let source = r#"use mod.flow.*
use mod.cut.*
let p_open = Graphic{src: asset("ast_r3gfodmn4sfstmroqto6cgrjbm", "arev_cf4fde4f351c1a7daa3040f18b11055600457f40dddd9e3f99a98fb8b5f0b0e2") span: [0.0, 4.0] fit: @fill track: 0}
let p_loss = Graphic{src: asset("ast_7cco6vrlpogci5g634tf4is3cy", "arev_d3e22b0bb49d278fdc4a92feace42b1a4efd055a4eb12e2dbbfacf9d2af20916") span: [4.0, 4.0] fit: @fill track: 0}
let top = Stack{layers: [p_open.out(@frame), p_loss.out(@frame)]}
let out = CutOutput{frame: top.out(@frame)}
Flow{kind: @cut label: "G" output: {width: 1920 height: 1080 fps: 30} duration: 8.0 p_open, p_loss, top, out}
"#;
        let doc = CutDoc::load(source).unwrap();
        let rows = rows(&doc, &HashMap::new(), &HashSet::new());
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, [FILM, "top", "p_open", "p_loss"]);
        assert_eq!(rows[3].detail, "graphic · 4.0–8.0 s");
    }

    fn rows_with(doc: &CutDoc, folded: &[&str]) -> Vec<TreeRow> {
        rows(doc, &HashMap::new(), &folded.iter().map(|s| s.to_string()).collect())
    }
}

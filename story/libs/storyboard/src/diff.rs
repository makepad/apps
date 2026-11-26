//! What changed between two versions of a board, item by item, in words a
//! model acts on ("card `hook` (scene 1) moved 0.0 → 2.0 s") and as data
//! the update plan maps onto the cut.

use crate::model::*;
use crate::splash;

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Added { id: String },
    Removed { id: String, kind: &'static str, headline: String, start: f64, length: f64 },
    /// Same length, new start.
    Moved { id: String, from: f64, to: f64 },
    /// New length (and maybe start).
    Retimed { id: String, from: (f64, f64), to: (f64, f64) },
    RowChanged { id: String, from: String, to: String },
    /// A text or choice field: `(field, before, after)`.
    Field { id: String, field: String, from: String, to: String },
    Header { field: String, from: String, to: String },
    RowAdded { id: String },
    RowRemoved { id: String },
    LinkAdded { from: String, to: String },
    LinkRemoved { from: String, to: String },
}

impl Change {
    pub fn item(&self) -> Option<&str> {
        match self {
            Change::Added { id } | Change::Removed { id, .. } | Change::Moved { id, .. } | Change::Retimed { id, .. } | Change::RowChanged { id, .. } | Change::Field { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// An item's content, without its place: what a part of the cut was
/// made from. Equal signatures mean nothing but the timing changed.
pub fn signature(item: &Item) -> String {
    let fields: Vec<String> = item_fields(item).into_iter().filter(|(key, _)| !matches!(*key, "span" | "row" | "color")).map(|(key, value)| format!("{key}={value}")).collect();
    format!("{:016x}", splash::fnv64(fields.join("\n").as_bytes()))
}

/// Every content field of an item as `(name, text)`.
pub fn content(item: &Item) -> Vec<(&'static str, String)> {
    let unquote = |value: String| if value.starts_with('"') { crate::splash::parse(&format!("let x = {value}")).stmts.first().and_then(|stmt| stmt.value.str().map(str::to_string)).unwrap_or(value) } else { value };
    item_fields(item).into_iter().filter(|(key, _)| !matches!(*key, "span" | "row")).map(|(key, value)| (key, unquote(value))).collect()
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

pub fn diff(old: &Storyboard, new: &Storyboard) -> Vec<Change> {
    let mut out = Vec::new();
    let header = |board: &Storyboard| board.header_fields().into_iter().map(|(k, v)| (k.to_string(), v)).collect::<Vec<_>>();
    let (old_header, new_header) = (header(old), header(new));
    for (key, value) in &new_header {
        let before = old_header.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default();
        if before != *value {
            out.push(Change::Header { field: key.clone(), from: before, to: value.clone() });
        }
    }
    for (key, value) in &old_header {
        if !new_header.iter().any(|(k, _)| k == key) {
            out.push(Change::Header { field: key.clone(), from: value.clone(), to: String::new() });
        }
    }
    for row in &new.rows {
        if old.row(&row.id).is_none() {
            out.push(Change::RowAdded { id: row.id.clone() });
        }
    }
    for row in &old.rows {
        if new.row(&row.id).is_none() {
            out.push(Change::RowRemoved { id: row.id.clone() });
        }
    }
    for item in &old.items {
        if new.item(&item.id).is_none() {
            out.push(Change::Removed { id: item.id.clone(), kind: item.body.kind(), headline: item.headline(), start: item.start, length: item.length });
        }
    }
    for item in &new.items {
        let Some(before) = old.item(&item.id) else {
            out.push(Change::Added { id: item.id.clone() });
            continue;
        };
        if !near(before.length, item.length) {
            out.push(Change::Retimed { id: item.id.clone(), from: (before.start, before.length), to: (item.start, item.length) });
        } else if !near(before.start, item.start) {
            out.push(Change::Moved { id: item.id.clone(), from: before.start, to: item.start });
        }
        if before.row != item.row {
            out.push(Change::RowChanged { id: item.id.clone(), from: before.row.clone(), to: item.row.clone() });
        }
        let (a, b) = (content(before), content(item));
        for (key, value) in &b {
            let was = a.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default();
            if was != *value {
                out.push(Change::Field { id: item.id.clone(), field: key.to_string(), from: was, to: value.clone() });
            }
        }
        for (key, value) in &a {
            if !b.iter().any(|(k, _)| k == key) {
                out.push(Change::Field { id: item.id.clone(), field: key.to_string(), from: value.clone(), to: String::new() });
            }
        }
    }
    let pairs = |board: &Storyboard| board.links.iter().map(|link| (link.from.clone(), link.to.clone())).collect::<Vec<_>>();
    let (old_links, new_links) = (pairs(old), pairs(new));
    for (from, to) in &new_links {
        if !old_links.contains(&(from.clone(), to.clone())) {
            out.push(Change::LinkAdded { from: from.clone(), to: to.clone() });
        }
    }
    for (from, to) in &old_links {
        if !new_links.contains(&(from.clone(), to.clone())) {
            out.push(Change::LinkRemoved { from: from.clone(), to: to.clone() });
        }
    }
    out
}

fn clip(text: &str) -> String {
    let one_line = text.replace('\n', " ⏎ ");
    if one_line.chars().count() > 120 {
        format!("\"{}…\"", one_line.chars().take(117).collect::<String>())
    } else {
        format!("\"{one_line}\"")
    }
}

/// How an item is named to a model: `card `hook` (scene 1, "Hook")`.
pub fn name_of(board: &Storyboard, id: &str) -> String {
    match board.item(id) {
        Some(item) => match &item.body {
            Body::Scene(_) => format!("card `{id}` (scene {}, {})", board.scene_number(id).unwrap_or(0), clip(&item.headline())),
            Body::Chapter(_) => format!("chapter `{id}` ({:02}/{:02}, {})", board.chapter_number(id).unwrap_or(0), board.chapters().len(), clip(&item.headline())),
            Body::Note { .. } => format!("note `{id}` on {} ({})", item.row, clip(&item.headline())),
            Body::Ref(_) => format!("reference `{id}` ({})", clip(&item.headline())),
            Body::Treatment(treatment) => format!("treatment `{id}` ({}{})", clip(&item.headline()), if treatment.chosen { ", chosen" } else { "" }),
            Body::Page(_) => format!("bible page `{id}` ({})", clip(&item.headline())),
            Body::Lyric(_) => format!("line `{id}` (line {}, {})", board.line_number(id).unwrap_or(0), clip(&item.headline())),
            Body::Device(_) => format!("device `{id}` ({})", clip(&item.headline())),
        },
        None => format!("`{id}`"),
    }
}

/// The changes as plain lines, oldest board's names for what went away.
pub fn describe(old: &Storyboard, new: &Storyboard, changes: &[Change]) -> String {
    let s = |t: f64| format!("{} s", splash::number(t));
    let mut lines = Vec::new();
    for change in changes {
        lines.push(match change {
            Change::Added { id } => {
                let item = new.item(id).unwrap();
                format!("added {} on row `{}` at {} for {}", name_of(new, id), item.row, s(item.start), s(item.length))
            }
            Change::Removed { id, kind, headline, start, length } => format!("removed {kind} `{id}` ({}) that was at {} for {}", clip(headline), s(*start), s(*length)),
            Change::Moved { id, from, to } => format!("moved {} from {} to {}", name_of(new, id), s(*from), s(*to)),
            Change::Retimed { id, from, to } => format!("retimed {} from {}–{} to {}–{}", name_of(new, id), s(from.0), s(from.0 + from.1), s(to.0), s(to.0 + to.1)),
            Change::RowChanged { id, from, to } => format!("moved {} from row `{from}` to row `{to}`", name_of(new, id)),
            Change::Field { id, field, from, to } => match (from.is_empty(), to.is_empty()) {
                (true, _) => format!("set {} `{field}` to {}", name_of(new, id), clip(to)),
                (_, true) => format!("cleared {} `{field}` (was {})", name_of(new, id), clip(from)),
                _ => format!("changed {} `{field}`: {} → {}", name_of(new, id), clip(from), clip(to)),
            },
            Change::Header { field, from, to } => format!("changed the board's `{field}`: {} → {}", if from.is_empty() { "(none)" } else { from }, if to.is_empty() { "(none)" } else { to }),
            Change::RowAdded { id } => format!("added row `{id}`"),
            Change::RowRemoved { id } => format!("removed row `{id}`"),
            Change::LinkAdded { from, to } => format!("linked {} to {}", name_of(new, from), name_of(new, to)),
            Change::LinkRemoved { from, to } => format!("unlinked {} from {}", name_of(old, from), name_of(old, to)),
        });
    }
    lines.iter().map(|line| format!("- {line}")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{apply, Op, Patch, Value};

    const BOARD: &str = "let story = Row{kind: @story}\nlet voice = Row{kind: @voice}\nlet hook = Scene{row: story span: [0.0, 4.0] title: \"Hook\" intent: \"one tap\"}\nlet demo = Scene{row: story span: [4.0, 6.0] title: \"Demo\"}\nlet vo1 = Note{row: voice span: [0.5, 3.0] text: \"Your ideas move fast.\"}\nStoryboard{title: \"T\" duration: 20.0 rows: [story, voice] items: [hook, demo, vo1]}\n";

    #[test]
    fn diffs_name_items_fields_and_times() {
        let (old, _) = Storyboard::load(BOARD).unwrap();
        let mut src = apply(BOARD, &Op::Place(vec![("hook".into(), 1.0, 4.0, None), ("demo".into(), 5.0, 8.0, None)])).unwrap().source;
        src = apply(&src, &Op::Update { id: "vo1".into(), patch: Patch { fields: vec![("text".into(), Value::Text("Ideas are fast.".into()))] } }).unwrap().source;
        src = apply(&src, &Op::Delete(vec!["hook".into()])).unwrap().source;
        let (new, _) = Storyboard::load(&src).unwrap();
        let changes = diff(&old, &new);
        assert!(changes.contains(&Change::Retimed { id: "demo".into(), from: (4.0, 6.0), to: (5.0, 8.0) }));
        assert!(changes.contains(&Change::Field { id: "vo1".into(), field: "text".into(), from: "Your ideas move fast.".into(), to: "Ideas are fast.".into() }));
        assert!(changes.iter().any(|c| matches!(c, Change::Removed { id, .. } if id == "hook")));
        let text = describe(&old, &new, &changes);
        assert!(text.contains("retimed card `demo` (scene 1, \"Demo\") from 4.0 s–10.0 s to 5.0 s–13.0 s"), "{text}");
        assert!(text.contains("changed note `vo1` on voice (\"Ideas are fast.\") `text`: \"Your ideas move fast.\" → \"Ideas are fast.\""), "{text}");
        assert!(text.contains("removed scene `hook` (\"Hook\") that was at 0.0 s for 4.0 s"), "{text}");
    }

    #[test]
    fn signatures_ignore_the_place() {
        let (board, _) = Storyboard::load(BOARD).unwrap();
        let mut moved = board.item("hook").unwrap().clone();
        let sig = signature(&moved);
        moved.start = 9.0;
        moved.row = "voice".into();
        assert_eq!(signature(&moved), sig);
        if let Body::Scene(scene) = &mut moved.body {
            scene.mood = "calm".into();
        }
        assert_ne!(signature(&moved), sig);
        assert!(diff(&board, &board).is_empty());
    }
}

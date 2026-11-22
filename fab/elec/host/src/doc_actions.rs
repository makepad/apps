//! The project's documents as the AI reads and changes them, as a person does in a code pane:
//! `elec.doc.list` (the panes' documents) and `elec.doc.read` (one document's numbered lines and
//! revision) are catalog reads; `fab.elec_text`'s replacements are found here, in the text of the
//! revision they were read from ([`find_edits`]), and the changed text goes to the engine as one
//! replacement, as the pane's typing does.

use fab_elec_core::Diag;
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};
use fab_elec_lang::DocKind;
use fab_elec_view::{DocText, DocsView};
use makepad_widgets_core::makepad_script::syntax::write::quote;
use std::fmt::Write;
use std::ops::Range;

pub const DOC_LIST: ActionDef = ActionDef {
    name: "elec.doc.list",
    label: "List documents",
    group: "Documents",
    help: "The project's electronics documents as the code panes hold them: each one's name, kind, revision and line count. elec.doc.read reads one; fab.elec_text changes its text.",
    returns: "docs: [{..}]
  name: text  the file name, relative to the project
  kind: choice circuit|board|sheets|system|cable|part|blocks|bench|profile|view|firmware
  rev: int  one more with each change of its text
  lines: int",
    fields: &[],
    inputs: &[], risk: Risk::Read, keys: "",
};

pub const DOC_READ: ActionDef = ActionDef {
    name: "elec.doc.read",
    label: "Read document",
    group: "Documents",
    help: "A document's text as its code pane shows it, each line after its number and a tab (the numbers are not part of the text), and its revision: the rev a fab.elec_text edit of it names.",
    returns: "doc: text
kind: choice circuit|board|sheets|system|cable|part|blocks|bench|profile|view|firmware
rev: int
lines: int  the document's line count
from: int  the first line shown
to: int  the last line shown
text: text  the lines shown, numbered
next: int  the from of the next page, when more remain",
    fields: &[
        FieldDef {
            name: "doc", kind: FieldKind::Text, required: true, default: "",
            help: "The document's name as elec.doc.list gives it (blink.fw.splash).",
        },
        FieldDef {
            name: "from", kind: FieldKind::Int { min: 1, max: i64::MAX }, required: false, default: "1",
            help: "The first line, counted from 1.",
        },
        FieldDef {
            name: "to", kind: FieldKind::Int { min: 1, max: i64::MAX }, required: false, default: "",
            help: "The last line; by default the document's last, or the page's when it is long.",
        },
    ],
    inputs: &[], risk: Risk::Read, keys: "",
};

pub static ACTIONS: &[ActionDef] = &[DOC_LIST, DOC_READ];

/// A read's page of lines: a long document goes in pages of about this many bytes (the reply
/// says where the next starts), so an answer fits a tool result.
const READ_PAGE: usize = 12 * 1024;

type Args = [(String, String)];

fn arg<'a>(args: &'a Args, name: &str) -> Option<&'a str> {
    args.iter().find(|(key, _)| key == name).map(|(_, text)| text.as_str())
}

fn error(field: &str, reason: impl std::fmt::Display) -> Diag {
    Diag::error(format!("{field}: {reason}"), vec![])
}

/// The kind's word, as `elec.doc.list` gives it.
pub fn kind_word(name: &str) -> &'static str {
    match DocKind::of_name(name) {
        Some(DocKind::Circuit) => "circuit",
        Some(DocKind::Board) => "board",
        Some(DocKind::Sheets) => "sheets",
        Some(DocKind::System) => "system",
        Some(DocKind::Cable) => "cable",
        Some(DocKind::Part) => "part",
        Some(DocKind::Blocks) => "blocks",
        Some(DocKind::Bench) => "bench",
        Some(DocKind::Profile) => "profile",
        Some(DocKind::View) => "view",
        Some(DocKind::Firmware) => "firmware",
        None => "other",
    }
}

/// The document `name` names: its project name, or a file name only one document has.
/// O(documents).
pub fn find_doc<'a>(docs: &'a DocsView, name: &str) -> Result<&'a DocText, String> {
    let name = name.trim();
    if let Some(text) = docs.texts.iter().find(|text| text.name == name) {
        return Ok(text);
    }
    let file = |text: &&DocText| text.name.rsplit('/').next() == Some(name);
    let mut found = docs.texts.iter().filter(file);
    match (found.next(), found.next()) {
        (Some(text), None) => Ok(text),
        (Some(_), Some(_)) => Err(format!("doc: more than one document is named {name}; give its whole name from elec.doc.list")),
        _ => {
            let names: Vec<&str> = docs.texts.iter().map(|text| text.name.as_str()).collect();
            Err(format!("doc: no document {name}; the documents are {}", names.join(", ")))
        }
    }
}

/// `elec.doc.list`: every document, in document order. O(documents + text) for the line counts.
pub fn doc_list(docs: &DocsView, args: &Args) -> Result<String, Diag> {
    DOC_LIST.check_args(args)?;
    let mut out = String::from("{docs: [");
    for text in &docs.texts {
        write!(out, "{{name: {} kind: {} rev: {} lines: {}}} ", quote(&text.name), kind_word(&text.name), text.rev, text.text.lines().count()).unwrap();
    }
    out.push_str("]}");
    Ok(out)
}

/// `elec.doc.read`: one page of a document's lines, numbered. O(text).
pub fn doc_read(docs: &DocsView, args: &Args) -> Result<String, Diag> {
    DOC_READ.check_args(args)?;
    let doc = find_doc(docs, arg(args, "doc").unwrap_or_default()).map_err(|why| Diag::error(why, vec![]))?;
    let line_arg = |name: &str| arg(args, name).map(|text| text.trim().replace('_', "").parse::<usize>()
        .map_err(|_| error(name, "expected a line number from 1"))).transpose();
    let lines: Vec<&str> = doc.text.lines().collect();
    let from = line_arg("from")?.unwrap_or(1).max(1);
    let last = line_arg("to")?.unwrap_or(lines.len()).min(lines.len());
    if from > lines.len() && !lines.is_empty() {
        return Err(error("from", format_args!("{} has {} lines", doc.name, lines.len())));
    }
    if last < from && !lines.is_empty() {
        return Err(error("to", "comes before from"));
    }
    let mut text = String::new();
    let mut to = from.saturating_sub(1);
    for (at, line) in lines.iter().enumerate().take(last).skip(from - 1) {
        if to >= from && text.len() + line.len() > READ_PAGE {
            break;
        }
        write!(text, "{}\t{line}\n", at + 1).unwrap();
        to = at + 1;
    }
    let mut out = format!("{{doc: {} kind: {} rev: {} lines: {} from: {from} to: {to} text: {}",
        quote(&doc.name), kind_word(&doc.name), doc.rev, lines.len(), quote(&text));
    if to < last {
        write!(out, " next: {}", to + 1).unwrap();
    }
    out.push('}');
    Ok(out)
}

/// What a `fab.elec_text` call changes in one document: replacements, each `(old, new)` with
/// `old` found once in the text of the revision the call names, or the whole new text.
#[derive(Clone, Debug, PartialEq)]
pub enum TextChange {
    Replace(Vec<(String, String)>),
    Whole(String),
}

/// A text change found in the text it was made for: the text it makes and each edit's lines in
/// it (0-based, end exclusive), in text order.
#[derive(Clone, Debug, PartialEq)]
pub struct FoundEdits {
    pub text: String,
    pub lines: Vec<Range<usize>>,
}

/// `old` shortened for a refusal: its first line, at most 60 characters.
fn short(old: &str) -> String {
    let line = old.lines().next().unwrap_or_default();
    let cut: String = line.chars().take(60).collect();
    if cut.len() < old.len() { format!("{}…", quote(&cut)) } else { quote(&cut) }
}

/// One replacement found in the text: its place in the call (from 1), its span and new text.
struct Found {
    n: usize,
    span: Range<usize>,
    new: String,
}

/// The text `change` makes of `text`. Each `old` must be in the text exactly once and no two may
/// overlap; a change that leaves the text as it is is refused too. Nothing is applied here. The
/// refusal names the edit by its place in the call, counted from 1. O(text x edits).
pub fn find_edits(text: &str, change: &TextChange) -> Result<FoundEdits, String> {
    let mut edits: Vec<Found> = match change {
        TextChange::Whole(new) => {
            let head = text.chars().zip(new.chars()).take_while(|(a, b)| a == b).map(|(ch, _)| ch.len_utf8()).sum::<usize>();
            let tail = text[head..].chars().rev().zip(new[head..].chars().rev()).take_while(|(a, b)| a == b).map(|(ch, _)| ch.len_utf8()).sum::<usize>();
            vec![Found { n: 1, span: head..text.len() - tail, new: new[head..new.len() - tail].to_string() }]
        }
        TextChange::Replace(pairs) if pairs.is_empty() => return Err("edits: give at least one {old new}, or the whole text".into()),
        TextChange::Replace(pairs) => {
            let mut edits = Vec::with_capacity(pairs.len());
            for (at, (old, new)) in pairs.iter().enumerate() {
                let n = at + 1;
                if old.is_empty() {
                    return Err(format!("edit {n}: old is empty; give text the document has, with enough around it to be found once"));
                }
                let Some(start) = text.find(old.as_str()) else {
                    return Err(format!("edit {n}: old is not in the text of this revision: {}", short(old)));
                };
                let step = old.chars().next().map_or(1, char::len_utf8);
                if text[start + step..].contains(old.as_str()) {
                    let count = text.match_indices(old.as_str()).count().max(2);
                    return Err(format!("edit {n}: old is in the text {count} times: {}; add the text around it so it is found once", short(old)));
                }
                edits.push(Found { n, span: start..start + old.len(), new: new.clone() });
            }
            edits
        }
    };
    edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
    for pair in edits.windows(2) {
        if pair[0].span.end > pair[1].span.start {
            let (a, b) = (pair[0].n.min(pair[1].n), pair[0].n.max(pair[1].n));
            return Err(format!("edits {a} and {b}: their old texts overlap; make them one edit"));
        }
    }
    let mut out = String::with_capacity(text.len() + edits.iter().map(|edit| edit.new.len()).sum::<usize>());
    let mut written = Vec::with_capacity(edits.len());
    let mut last = 0;
    for edit in &edits {
        out.push_str(&text[last..edit.span.start]);
        let start = out.len();
        out.push_str(&edit.new);
        written.push(start..out.len());
        last = edit.span.end;
    }
    out.push_str(&text[last..]);
    if out == text {
        return Err("the change leaves the text as it is: nothing to change".into());
    }
    // Each edit's lines: from the line of its first byte to the line of its last (a deletion,
    // its place's line).
    let mut lines = Vec::with_capacity(written.len());
    let (mut line, mut seen) = (0, 0);
    for range in written {
        line += out[seen..range.start].matches('\n').count();
        seen = range.start;
        let body = &out[range];
        let body = body.strip_suffix('\n').unwrap_or(body);
        lines.push(line..line + body.matches('\n').count() + 1);
    }
    Ok(FoundEdits { text: out, lines })
}

//! The statement writers behind `elec.req.add`, `.remove`, `.waive` and `.unwaive`: span edits of
//! a requirements document's text, each giving the document's new text. The caller commits it as
//! one text step and refuses a result that adds a mistake. Each writer reads its result back and
//! refuses one that does not change exactly what it was asked to (a measure or bound text that
//! closes the statement and opens another, say). O(text).

use crate::{read, ReqDoc};
use makepad_script::syntax::write::quote;
use std::ops::Range;

/// `text` with `require(name, measure, bound)` after the document's last requirement (at the
/// end when it has none). `measure` and `bound` are Splash text, as the reader reads them.
pub fn add(text: &str, doc: &ReqDoc, name: &str, measure: &str, bound: Option<&str>) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("name: a requirement needs a name, the person's words".into());
    }
    if doc.requirements.iter().any(|r| r.name == name) {
        return Err(format!("name: a requirement named {name:?} is there already: choose another name, or remove it first"));
    }
    let bound = bound.map(str::trim).filter(|b| !b.is_empty());
    let statement = match bound {
        Some(bound) => format!("require({}, {}, {})", quote(name), measure.trim(), bound),
        None => format!("require({}, {})", quote(name), measure.trim()),
    };
    let at = after_requirements(text, doc);
    let new = insert_line(text, at, &statement);
    let (after, mistakes) = read(doc.doc, &new);
    let old_errors = read(doc.doc, text).1;
    let added = after.requirements.iter().find(|r| r.name == name);
    if added.is_none() || !statements(text, doc, None).eq(statements(&new, &after, added.map(|r| &r.span)))
        || mistakes.len() > old_errors.len()
    {
        let mut previous: Vec<_> = old_errors.iter().map(|e| e.message.as_str()).collect();
        let new_error = mistakes.iter().find(|e| {
            if let Some(i) = previous.iter().position(|&msg| msg == e.message) {
                previous.swap_remove(i);
                false
            } else {
                true
            }
        });
        let why = new_error.map_or("check the measure and bound", |e| e.message.as_str());
        return Err(format!("measure: {statement} does not read as one requirement named {name:?}: {why}"));
    }
    Ok(new)
}

fn after_requirements(text: &str, doc: &ReqDoc) -> usize {
    doc.requirements.last().map_or(text.len(), |r| line_end(text, r.span.end as usize))
}

// Compare the other statements byte for byte, allowing their source offsets to move.
fn statements<'a>(text: &'a str, doc: &'a ReqDoc, except: Option<&'a Range<u32>>) -> impl Iterator<Item = &'a str> {
    doc.requirements.iter().map(|r| &r.span).chain(doc.waivers.iter().map(|w| &w.span))
        .chain(doc.wishes.iter().map(|w| &w.span)).chain(doc.brief.iter().map(|b| &b.span))
        .filter(move |s| Some(*s) != except).map(|s| &text[s.start as usize..s.end as usize])
}

/// Append one unmeasured wish at the same insertion point as a requirement.
pub fn wish(text: &str, doc: &ReqDoc, words: &str) -> Result<String, String> {
    let words = words.trim();
    if words.is_empty() { return Err("a wish is empty: write it in the person's words".into()); }
    let at = after_requirements(text, doc);
    let index = doc.wishes.iter().take_while(|w| (w.span.start as usize) < at).count();
    let new = insert_line(text, at, &format!("wish({})", quote(words)));
    let (after, mistakes) = read(doc.doc, &new);
    let added = after.wishes.get(index).filter(|w| w.words == words);
    if added.is_none() || !statements(text, doc, None).eq(statements(&new, &after, added.map(|w| &w.span)))
        || mistakes.len() > read(doc.doc, text).1.len() {
        return Err("the edit did not add exactly one wish".into());
    }
    Ok(new)
}

/// Replace the brief in place, insert it first, or remove it when the words are blank.
pub fn brief(text: &str, doc: &ReqDoc, words: &str) -> Result<String, String> {
    let old = doc.brief.as_ref().map(|b| &b.span);
    let words = (!words.trim().is_empty()).then_some(words);
    let new = match (old, words) {
        (None, None) => return Ok(text.into()),
        (Some(span), None) => cut(text, vec![line_of(text, span.clone())]),
        (None, Some(words)) => format!("brief({})\n{text}", quote(words)),
        (Some(span), Some(words)) => {
            let mut new = text.to_owned();
            new.replace_range(span.start as usize..span.end as usize, &format!("brief({})", quote(words)));
            new
        }
    };
    let (after, mistakes) = read(doc.doc, &new);
    if after.brief.as_ref().map(|b| b.words.as_str()) != words
        || !statements(text, doc, old).eq(statements(&new, &after, after.brief.as_ref().map(|b| &b.span)))
        || mistakes.len() > read(doc.doc, text).1.len() {
        return Err("the edit did not change just the brief".into());
    }
    Ok(new)
}

/// `text` without the requirement named `name` and its waivers (a waiver naming no requirement
/// would be a mistake).
pub fn remove(text: &str, doc: &ReqDoc, name: &str) -> Result<String, String> {
    let req = find(doc, name)?;
    let mut cuts = vec![line_of(text, req.span.clone())];
    cuts.extend(doc.waivers.iter().filter(|w| w.name == req.name).map(|w| line_of(text, w.span.clone())));
    Ok(cut(text, cuts))
}

/// `text` with the requirement named `name` waived for `reason`. The person's (`proposed` false)
/// is `waive(name, reason)`, taking the place of its waiver or proposal: accepting a proposal is
/// this. The AI's is `propose_waiver(name, reason)`, taking the place of an earlier proposal; it
/// waives nothing until the person accepts it, and a requirement the person waived is refused.
/// The new statement goes at the end.
pub fn waive(text: &str, doc: &ReqDoc, name: &str, reason: &str, proposed: bool) -> Result<String, String> {
    let req = find(doc, name)?;
    let reason = reason.trim();
    if reason.is_empty() {
        return Err("reason: a waiver needs the person's reason".into());
    }
    if proposed && doc.waivers.iter().any(|w| w.name == req.name && !w.proposed) {
        return Err(format!("name: the person waived {:?} already", req.name));
    }
    let word = if proposed { "propose_waiver" } else { "waive" };
    let statement = format!("{word}({}, {})", quote(&req.name), quote(reason));
    let cuts = doc.waivers.iter().filter(|w| w.name == req.name && (!proposed || w.proposed)).map(|w| line_of(text, w.span.clone())).collect();
    Ok(insert_line(&cut(text, cuts), usize::MAX, &statement))
}

/// `text` without the waivers and proposed waivers of the requirement named `name` (declining a
/// proposal is this).
pub fn unwaive(text: &str, doc: &ReqDoc, name: &str) -> Result<String, String> {
    let req = find(doc, name)?;
    let cuts: Vec<_> = doc.waivers.iter().filter(|w| w.name == req.name).map(|w| line_of(text, w.span.clone())).collect();
    if cuts.is_empty() {
        return Err(format!("name: {:?} is not waived, and no waiver is proposed for it", req.name));
    }
    Ok(cut(text, cuts))
}

fn find<'a>(doc: &'a ReqDoc, name: &str) -> Result<&'a crate::Requirement, String> {
    let name = name.trim();
    doc.requirements.iter().find(|r| r.name == name).ok_or_else(|| {
        let names: Vec<String> = doc.requirements.iter().map(|r| format!("{:?}", r.name)).collect();
        match names.is_empty() {
            true => format!("name: no requirement is named {name:?}: the document has none"),
            false => format!("name: no requirement is named {name:?}; there are {}", names.join(", ")),
        }
    })
}

/// The end of the line `at` is on (before its newline).
fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i)
}

/// `statement` as a line of its own after `at`, a line end (past the end: at the end).
fn insert_line(text: &str, at: usize, statement: &str) -> String {
    let at = at.min(text.len());
    let (head, tail) = text.split_at(at);
    let head = if at == text.len() { head.trim_end() } else { head };
    match (head.is_empty(), tail.is_empty()) {
        (true, true) => format!("{statement}\n"),
        (true, false) => format!("{statement}\n{tail}"),
        (false, true) => format!("{head}\n{statement}\n"),
        (false, false) => format!("{head}\n{statement}{tail}"),
    }
}

/// The bytes to cut for a statement at `span`: its whole line with the newline when the line holds
/// nothing else, else the statement alone.
fn line_of(text: &str, span: Range<u32>) -> Range<usize> {
    let (start, end) = (span.start as usize, span.end as usize);
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_stop = line_end(text, end);
    if text[line_start..start].trim().is_empty() && text[end..line_stop].trim().is_empty() {
        line_start..(line_stop + 1).min(text.len())
    } else {
        start..end
    }
}

fn cut(text: &str, mut cuts: Vec<Range<usize>>) -> String {
    cuts.sort_by_key(|c| std::cmp::Reverse(c.start));
    let mut out = text.to_owned();
    for c in cuts {
        out.replace_range(c, "");
    }
    out
}

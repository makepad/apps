//! Literal system edits for a commit: its ops are applied to a copy of the design, then each
//! link and mount the commit removed, changed or added gets its edit, against the text and map
//! before the commit. So any sequence of system ops in one commit is one edit set.
//! Cost: a design copy (units are shared `Arc`s), one parse of the system document and map lookups
//! for the commit's links and insertion anchors; no evaluation. System commits come from actions,
//! never from frames.

use crate::board_splice::{after_statement, line_start};
use crate::map::{SourceMap, SourceRange};
use crate::source::{located, DocKind, Sources};
use crate::splice::SpanEdit;
use crate::sys_doc;
use fab_elec_core::{BoardId, Diag, DocId, LinkId, Sel};
use fab_elec_design::{Design, DesignOp, Frame, Link, SysOp, TextEdit};
use makepad_script::syntax::{self, Kind, Node};
use makepad_script::tokenizer::{lex_with, ScriptTokenTag};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// One edit of the system document, in the text before the commit; a mark is an added statement:
/// its selection, offset into `text` and length.
struct Patch {
    span: Range<u32>,
    text: String,
    marks: Vec<(Sel, u32, u32)>,
}

/// The splice of a commit's system ops (`SetFactory` belongs to the board document). `before` is the
/// design the commit starts from.
pub(crate) fn splice_system(sources: &Sources, map: &SourceMap, before: &Design, ops: &[&SysOp]) -> Result<SpanEdit, Diag> {
    let source = sources.docs().iter().copied().find(|d| d.kind == DocKind::System);
    let doc = source.or_else(|| sources.docs().first().copied()).map_or(DocId(0), |d| d.id);
    let error = |message: String| located(doc, 0..source.map_or(0, |d| d.text.len() as u32), message);
    if ops.is_empty() { return Ok(SpanEdit::default()); }
    let source = source.ok_or_else(|| error("the system document is missing".into()))?;
    let text: &str = &source.text;
    let mut after = before.clone();
    for (i, op) in ops.iter().enumerate() {
        after.apply(&DesignOp::Sys((*op).clone()), false).map_err(|e| error(format!("system op {}: {e}", i + 1)))?;
    }
    let old: HashMap<LinkId, &Link> = before.links.iter().map(|l| (l.id, l)).collect();
    let new: HashMap<LinkId, &Link> = after.links.iter().map(|l| (l.id, l)).collect();
    let kept = |links: &[Link], other: &HashMap<LinkId, &Link>| -> Vec<LinkId> {
        links.iter().filter(|l| other.contains_key(&l.id)).map(|l| l.id).collect()
    };
    // Model link order is statement order, and a kept statement does not move.
    if kept(&before.links, &new) != kept(&after.links, &old) {
        return Err(error("the commit moves a link: commit its removal and insertion one at a time".into()));
    }
    let tree = syntax::parse_with(text, &sys_doc::units()).map_err(|e| {
        located(doc, e.at as u32..e.at as u32, format!("system syntax: {}", e.message))
    })?;
    let statement = |id: LinkId| {
        let range = map.locate(&Sel::Link(id)).ok_or_else(|| error(format!("link {} has no statement range", id.0)))?;
        if range.doc != doc { return Err(error(format!("link {} has a statement in another document", id.0))); }
        let whole = tree.statements().iter().find(|n| n.span.start as u32 == range.span.start && n.span.end as u32 == range.span.end)
            .ok_or_else(|| error(format!("link {} has no statement at its range", id.0)))?;
        let (call, binding) = sys_doc::declaration(whole, text);
        if !matches!(call.callee_path(), Some("mate" | "wire")) {
            return Err(located(doc, range.span, format!("link {} does not name a link statement", id.0)));
        }
        Ok((range.span, call, binding))
    };
    let old_bound: HashSet<_> = tree.statements().iter().filter_map(|n| match &n.kind {
        Kind::Let { keyword, names, .. } if keyword == "let" => Some(names.iter().cloned()),
        _ => None,
    }).flatten().collect();
    let mounts = sys_doc::mount_statements(doc, text, &before.units, tree.statements(), &old_bound);
    let mut old_names = HashMap::new();
    let mut names = HashMap::new();
    let mut bound = old_bound.clone();
    for link in &before.links {
        if let Some(name) = statement(link.id)?.2 {
            old_names.insert(link.id, name.to_string());
            let leaving = before.assembly.mounts.iter().any(|m| m.parent == Frame::Mate(link.id))
                && !after.assembly.mounts.iter().any(|m| m.parent == Frame::Mate(link.id));
            if !new.contains_key(&link.id) || leaving { bound.remove(name); }
            else { names.insert(link.id, name.to_string()); }
        }
    }
    // Retained bindings keep their names. Names released by this commit are free; other authored
    // bindings and unit names reserve theirs before newly mounted links choose in model order.
    let mut used = bound.clone();
    used.extend(after.units.iter().map(|u| u.name.clone()));
    for link in &after.links {
        if !names.contains_key(&link.id) && after.assembly.mounts.iter().any(|m| m.parent == Frame::Mate(link.id)) {
            let name = sys_doc::link_name(&after, link, &mut used);
            bound.insert(name.clone());
            names.insert(link.id, name);
        }
    }
    let link_text = |link: &Link| {
        let call = sys_doc::link_statement(&after, link, &bound);
        names.get(&link.id).map_or(call.clone(), |name| format!("let {name} = {call}"))
    };
    let mut patches = Vec::new();
    let mut removes = Vec::new();
    for link in before.links.iter().filter(|l| !new.contains_key(&l.id)) {
        let (span, _, _) = statement(link.id)?;
        patches.push(Patch { span: line_start(text, span.start)..after_statement(text, span.end), text: String::new(), marks: Vec::new() });
        removes.push(Sel::Link(link.id));
    }
    for link in after.links.iter().filter(|l| old.contains_key(&l.id)) {
        let (span, call, binding) = statement(link.id)?;
        if binding != names.get(&link.id).map(String::as_str) {
            // A whole-statement replacement preserves the mapped start, including when kind or
            // ends change too. A prefix insertion would shift the start past the new binding.
            patches.push(Patch { span, text: link_text(link), marks: Vec::new() });
        } else if old[&link.id] != link {
            replace_link(text, doc, &after, old[&link.id], link, (span, call), &bound, &mut patches)?;
        }
    }
    // Added statements go before the next kept link, else after the last link statement; several
    // at one place are one insertion, in model order.
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let end = match before.links.last() {
        Some(last) => after_statement(text, statement(last.id)?.0.end),
        None => 0,
    };
    let mut insertions: Vec<Patch> = Vec::new();
    for (i, link) in after.links.iter().enumerate().filter(|(_, l)| !old.contains_key(&l.id)) {
        let at = match after.links[i + 1..].iter().find(|n| old.contains_key(&n.id)) {
            Some(next) => line_start(text, statement(next.id)?.0.start),
            None => end,
        };
        insert_line(text, at, link_text(link), Some(Sel::Link(link.id)), newline, &mut insertions);
    }
    let old_mounts: HashMap<_, _> = before.assembly.mounts.iter().map(|m| (m.unit, m)).collect();
    let mut changed: Vec<_> = after.assembly.mounts.iter().filter(|m| old_mounts[&m.unit] != *m).collect();
    changed.sort_by_key(|m| m.unit);
    for mount in changed {
        let old = old_mounts[&mount.unit];
        // Only this unit's own statement must be editable: another unit's broken mount stays as
        // it is, with its diag.
        let mut found = mounts.iter().filter(|(unit, _, _)| *unit == mount.unit);
        let statement = match (found.next(), found.next()) {
            (_, Some((_, second, _))) => return Err(located(doc, second.span.start as u32..second.span.end as u32,
                format!("unit {} has a second mount statement: keep one", mount.unit.0))),
            (Some((_, _, Some(bad))), None) => return Err(bad.clone()),
            (Some((_, n, None)), None) => Some(*n),
            (None, None) => None,
        };
        if *old == sys_doc::default_mount(mount.unit) && statement.is_none() {
            let at = mounts.iter().filter(|(unit, _, _)| *unit > mount.unit).min_by_key(|(unit, _, _)| *unit)
                .map(|(_, n, _)| line_start(text, n.span.start as u32))
                .unwrap_or_else(|| mounts.last().map_or(text.len() as u32, |(_, n, _)| after_statement(text, n.span.end as u32)));
            insert_line(text, at, sys_doc::mount_statement(&after, mount, &names, &bound), None, newline, &mut insertions);
        } else {
            let n = statement.ok_or_else(|| error(format!("mount for unit {} has no statement", mount.unit.0)))?;
            if *mount == sys_doc::default_mount(mount.unit) {
                patches.push(Patch { span: line_start(text, n.span.start as u32)..after_statement(text, n.span.end as u32), text: String::new(), marks: Vec::new() });
            } else {
                let old = sys_doc::mount_fields(before, old, &old_names, &old_bound);
                let new = sys_doc::mount_fields(&after, mount, &names, &bound);
                replace_mount(text, n, &old, &new, &mut patches);
            }
        }
    }
    patches.extend(insertions);
    patches.sort_by_key(|p| (p.span.start, p.span.end));
    let mut out = SpanEdit { removes, ..SpanEdit::default() };
    let mut delta = 0i64;
    let mut last_end = 0;
    for patch in patches {
        if patch.span.start < last_end { return Err(error("system statement edits overlap".into())); }
        for (sel, offset, len) in patch.marks {
            let start = (i64::from(patch.span.start) + delta) as u32 + offset;
            out.adds.push((sel, SourceRange { doc, span: start..start + len }));
        }
        delta += patch.text.len() as i64 - i64::from(patch.span.end - patch.span.start);
        last_end = patch.span.end;
        out.edits.push(TextEdit { doc, span: patch.span, text: patch.text });
    }
    Ok(out)
}

fn insert_line(text: &str, at: u32, line: String, sel: Option<Sel>, newline: &str, insertions: &mut Vec<Patch>) {
    let i = insertions.iter().position(|p| p.span.start == at).unwrap_or_else(|| {
        let prefix = if at != 0 && text.as_bytes()[at as usize - 1] != b'\n' { newline } else { "" };
        insertions.push(Patch { span: at..at, text: prefix.into(), marks: Vec::new() });
        insertions.len() - 1
    });
    let patch = &mut insertions[i];
    if let Some(sel) = sel { patch.marks.push((sel, patch.text.len() as u32, line.len() as u32)); }
    patch.text.push_str(&line);
    patch.text.push_str(newline);
}

/// Kept values get token edits. Added and removed fields occupy the gaps between kept fields,
/// so simultaneous field changes never overlap a value edit or each other's separators.
fn replace_mount(text: &str, call: &Node, old: &[Option<sys_doc::MountValue>; 3], new: &[Option<sys_doc::MountValue>; 3], patches: &mut Vec<Patch>) {
    use sys_doc::{MountValue, MOUNT_FIELDS};
    let Some([_, opts]) = call.call("mount") else { unreachable!("mount statements were checked"); };
    let Kind::Object { fields, open, close, .. } = &opts.kind else { unreachable!("mount options were checked"); };
    let rank = |name: &Option<String>| MOUNT_FIELDS.iter().position(|k| Some(*k) == name.as_deref()).expect("mount fields were checked");
    let removed = |i: usize| old[i] != new[i] && new[i].is_none();
    let kept: Vec<_> = fields.iter().filter(|f| !removed(rank(&f.name))).collect();
    let mut replace = |span: Range<usize>, new: String| {
        if text[span.clone()] != new {
            patches.push(Patch { span: span.start as u32..span.end as u32, text: new, marks: Vec::new() });
        }
    };
    for field in &kept {
        let i = rank(&field.name);
        if old[i] == new[i] { continue; }
        let Some(value) = &new[i] else { continue; };
        match (&old[i], value, &field.value.kind) {
            (Some(MountValue::Three(old)), MountValue::Three(new), Kind::Array { items, .. }) if items.len() == 3 => {
                for ((old, new), n) in old.iter().zip(new).zip(items) {
                    if old != new { replace(n.span.clone(), new.clone()); }
                }
            }
            _ => replace(field.value.span.clone(), value.text()),
        }
    }
    let mut additions = vec![Vec::new(); kept.len() + 1];
    for (i, value) in new.iter().enumerate() {
        if old[i] == new[i] || fields.iter().any(|f| rank(&f.name) == i) { continue; }
        if let Some(value) = value {
            let gap = kept.iter().position(|f| rank(&f.name) > i).unwrap_or(kept.len());
            additions[gap].push(format!("{}: {}", MOUNT_FIELDS[i], value.text()));
        }
    }
    for (i, added) in additions.into_iter().enumerate() {
        let left = i.checked_sub(1).map(|j| kept[j]);
        let right = kept.get(i);
        let start = left.map_or(open + 1, |f| f.span.end);
        let end = right.map_or(*close, |f| f.span.start);
        if added.is_empty() && !fields.iter().any(|f| start <= f.span.start && f.span.end <= end && removed(rank(&f.name))) { continue; }
        let mut gap = added.join(" ");
        if left.is_some() && (!gap.is_empty() || right.is_some()) { gap.insert(0, ' '); }
        if right.is_some() && !added.is_empty() { gap.push(' '); }
        // Keep a shared separator outside the edit: removing a field consumes just its own
        // separating space, and adding one is an insertion at the proper field boundary.
        let previous = &text[start..end];
        let suffix = previous.chars().rev().zip(gap.chars().rev()).take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8()).sum::<usize>();
        let prefix = previous[..previous.len() - suffix].chars().zip(gap[..gap.len() - suffix].chars())
            .take_while(|(a, b)| a == b).map(|(c, _)| c.len_utf8()).sum::<usize>();
        replace(start + prefix..end - suffix, gap[prefix..gap.len() - suffix].to_string());
    }
}

/// A changed link's edits: each changed end, the `pins` option and the callee when the kind
/// changed. Its binding and position stay.
fn replace_link(text: &str, doc: DocId, design: &Design, old: &Link, link: &Link, (span, call): (Range<u32>, &Node), bound: &HashSet<String>, patches: &mut Vec<Patch>) -> Result<(), Diag> {
    let Kind::Call { callee, args, .. } = &call.kind else { unreachable!("a link statement is a call"); };
    if args.len() < 2 || args.len() > 3 {
        return Err(located(doc, span, "a link takes two ends and optional pins"));
    }
    let mut replace = |span: Range<usize>, new: String| {
        if text[span.clone()] != new {
            patches.push(Patch { span: span.start as u32..span.end as u32, text: new, marks: Vec::new() });
        }
    };
    if old.kind != link.kind { replace(callee.span.clone(), sys_doc::link_kind(link.kind).into()); }
    if old.a != link.a { replace(args[0].span.clone(), sys_doc::link_end(design, link.a, bound)); }
    if old.b != link.b { replace(args[1].span.clone(), sys_doc::link_end(design, link.b, bound)); }
    // Pins are named by their parts, so a moved end can change the text of an equal pin map.
    let pins = sys_doc::link_pins(design, link);
    if sys_doc::link_pins(design, old) != pins {
        match (args.get(2), pins) {
            (Some(opts), Some(pins)) => replace(opts.span.clone(), pins),
            (None, Some(pins)) => replace(args[1].span.end..args[1].span.end, format!(", {pins}")),
            (Some(opts), None) => replace(args[1].span.end..opts.span.end, String::new()),
            (None, None) => {}
        }
    }
    Ok(())
}

/// Follow circuit paths at commit time: one parse, then one identifier edit per matching end.
/// Bindings are authored names, so a renamed end can differ from the canonical writer's name.
pub(crate) fn rename_paths(sources: &Sources, design: &Design, unit: BoardId, paths: &[(&str, String)]) -> Result<Vec<TextEdit>, Diag> {
    if paths.is_empty() { return Ok(Vec::new()); }
    let Some(doc) = sources.docs().iter().copied().find(|d| d.kind == DocKind::System) else { return Ok(Vec::new()); };
    let text: &str = &doc.text;
    let units = sys_doc::units();
    let tree = syntax::parse_with(text, &units).map_err(|e| {
        located(doc.id, e.at as u32..e.at as u32, format!("system syntax: {}", e.message))
    })?;
    let names = tree.statements().iter().filter_map(|n| match &n.kind {
        Kind::Let { keyword, names, .. } if keyword == "let" => Some(names.iter().cloned().map(|name| (name, None))),
        _ => None,
    }).flatten().collect();
    let mut edits = Vec::new();
    for whole in tree.statements() {
        let (call, _) = sys_doc::declaration(whole, text);
        let Some(("mate" | "wire", args)) = call.as_call() else { continue; };
        for end in args.iter().take(2) {
            let Ok((of, path)) = sys_doc::end_path(doc.id, text, &design.units, &names, end) else { continue; };
            if of != unit { continue; }
            let renamed = paths.iter().filter(|(old, _)| {
                path.strip_prefix(*old).is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('.') || suffix.starts_with('['))
            }).max_by_key(|(old, _)| old.len());
            let Some((old, new)) = renamed else { continue; };
            // Unit names and unit(string) each contribute one identifier before the component
            // segments. Lexing skips comments, strings and numeric indices without losing gaps.
            let tokens = lex_with(&text[end.span.clone()], &units);
            let token = tokens.iter().filter(|t| t.token().kind() == ScriptTokenTag::Identifier)
                .nth(old.split('.').count())
                .ok_or_else(|| located(doc.id, end.span.start as u32..end.span.end as u32, "the renamed path has no identifier token"))?;
            let name = new.rsplit('.').next().unwrap_or(new).split('[').next().unwrap_or(new);
            edits.push(TextEdit {
                doc: doc.id,
                span: (end.span.start + token.start()) as u32..(end.span.start + token.end()) as u32,
                text: name.into(),
            });
        }
    }
    Ok(edits)
}

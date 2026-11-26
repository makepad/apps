//! Board commits as disjoint edits of their authored statements.

use crate::board_doc::{self, HeaderKind};
use crate::map::{SourceMap, SourceRange};
use crate::source::{located, DocKind, Sources, LIB_DOC};
use crate::splice::SpanEdit;
use fab_elec_board::{Board, BoardOp, ItemKind};
use fab_elec_circuit::Circuit;
use fab_elec_core::{Diag, DocId, FeatureRef, ItemId, Sel};
use fab_elec_design::TextEdit;
use makepad_script::syntax::{self, Kind, Node};
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Build a board source map from the spans returned by `board_doc::lower`.
pub fn from_spans(doc: DocId, spans: &[(Sel, Range<u32>)]) -> SourceMap {
    let mut map = SourceMap::new();
    for (sel, span) in spans {
        map.insert(sel.clone(), SourceRange { doc, span: span.clone() });
    }
    map
}

/// Item kinds in statement order (`item_order`).
const KINDS: [ItemKind; 10] = [
    ItemKind::Placed, ItemKind::Track, ItemKind::Via, ItemKind::Zone, ItemKind::Keepout,
    ItemKind::Hole, ItemKind::Cutout, ItemKind::Graphic, ItemKind::Text, ItemKind::Tuning,
];

/// The id of the item at `pos` of `kind`'s list, and whether it has a statement.
fn item_at(board: &Board, kind: ItemKind, pos: usize) -> Option<(ItemId, bool)> {
    macro_rules! get {
        ($field:ident) => { board.$field.get(pos).map(|i| (i.id, i.origin.has_statement())) };
    }
    match kind {
        ItemKind::Placed => get!(placed),
        ItemKind::Track => get!(tracks),
        ItemKind::Via => get!(vias),
        ItemKind::Zone => get!(zones),
        ItemKind::Keepout => get!(keepouts),
        ItemKind::Hole => get!(holes),
        ItemKind::Cutout => get!(cutouts),
        ItemKind::Graphic => get!(graphics),
        ItemKind::Text => get!(texts),
        ItemKind::Tuning => get!(tunings),
    }
}

/// An item's kind, its position in that kind's list, and whether it has a statement.
fn entry(board: &Board, id: ItemId) -> Option<(ItemKind, usize, bool)> {
    let (kind, index) = board.locate(id)?;
    item_at(board, kind, index).map(|(_, authored)| (kind, index, authored))
}

fn item_order(kind: ItemKind) -> usize {
    match kind {
        ItemKind::Placed => 8,
        ItemKind::Track => 9,
        ItemKind::Via => 10,
        ItemKind::Zone => 11,
        ItemKind::Keepout => 12,
        ItemKind::Hole => 13,
        ItemKind::Cutout => 14,
        ItemKind::Graphic => 15,
        ItemKind::Text => 16,
        ItemKind::Tuning => 17,
    }
}

struct Header {
    kind: HeaderKind,
    span: Range<u32>,
    key: Option<(String, String)>,
    profile: Option<(String, Range<u32>)>,
}

fn header(node: &Node) -> Option<Header> {
    let (name, args) = node.as_call()?;
    let kind = match name {
        "board" => HeaderKind::Board,
        "factory" => HeaderKind::Factory,
        "stackup" => HeaderKind::Stackup,
        "outline" => HeaderKind::Outline,
        "rules" => HeaderKind::Rules,
        "waive" => HeaderKind::Waive,
        "rule" => HeaderKind::Rule,
        "check" => HeaderKind::Check,
        _ => return None,
    };
    let key = match kind {
        HeaderKind::Waive => Some((
            format!("{:016x}", u64::from_str_radix(args.first()?.string()?, 16).ok()?),
            args.get(1)?.string()?.to_string(),
        )),
        HeaderKind::Rule | HeaderKind::Check => Some((args.first()?.string()?.to_string(), String::new())),
        _ => None,
    };
    let profile = if kind == HeaderKind::Factory {
        let value = args.first()?;
        Some((value.string()?.to_string(), value.span.start as u32..value.span.end as u32))
    } else { None };
    Some(Header { kind, span: node.span.start as u32..node.span.end as u32, key, profile })
}

struct Mark {
    sel: Sel,
    // Relative to the edit's start; a trimmed prefix lies before it.
    offset: i64,
    len: u32,
}

struct Patch {
    span: Range<u32>,
    text: String,
    marks: Vec<Mark>,
}

fn replacement(text: &str, span: Range<u32>, new: String, sel: Option<Sel>) -> Option<Patch> {
    let old = &text[span.start as usize..span.end as usize];
    if old == new { return None; }
    let prefix = old.chars().zip(new.chars()).take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8()).sum::<usize>();
    let suffix = old[prefix..].chars().rev().zip(new[prefix..].chars().rev())
        .take_while(|(a, b)| a == b).map(|(c, _)| c.len_utf8()).sum::<usize>();
    let marks = sel.into_iter().map(|sel| Mark { sel, offset: -(prefix as i64), len: new.len() as u32 }).collect();
    Some(Patch {
        span: span.start + prefix as u32..span.end - suffix as u32,
        text: new[prefix..new.len() - suffix].to_string(),
        marks,
    })
}

pub(crate) fn line_start(text: &str, at: u32) -> u32 {
    let at = at as usize;
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    if text[start..at].bytes().all(|b| matches!(b, b' ' | b'\t' | b'\r')) { start as u32 } else { at as u32 }
}

pub(crate) fn after_statement(text: &str, at: u32) -> u32 {
    let at = at as usize;
    let end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    let tail = text[at..end].trim();
    if tail.is_empty() || tail == ";" || tail.starts_with("//") || tail.starts_with("; //") {
        (end + usize::from(end < text.len())) as u32
    } else {
        at as u32
    }
}

fn deletion(text: &str, span: Range<u32>) -> Patch {
    let mut end = span.end as usize;
    if text.as_bytes().get(end) == Some(&b';') { end += 1; }
    let line_end = text[end..].find('\n').map_or(text.len(), |i| end + i);
    let start = line_start(text, span.start);
    let alone = start == 0 || text.as_bytes().get(start as usize - 1) == Some(&b'\n');
    let span = if alone && text[end..line_end].trim().is_empty() {
        start..(line_end + usize::from(line_end < text.len())) as u32
    } else {
        span.start..end as u32
    };
    Patch { span, text: String::new(), marks: Vec::new() }
}

struct Insertion {
    at: u32,
    order: (usize, usize),
    text: String,
    sel: Option<Sel>,
}

fn header_anchor(text: &str, headers: &[Header], kind: HeaderKind) -> u32 {
    headers.iter().filter(|h| h.kind <= kind).max_by_key(|h| (h.kind, h.span.end))
        .map_or(0, |h| after_statement(text, h.span.end))
}

fn op_name(op: &BoardOp) -> &'static str {
    match op {
        BoardOp::Add(_) => "Add",
        BoardOp::Insert { .. } => "Insert",
        BoardOp::Remove(_) => "Remove",
        BoardOp::Replace(_) => "Replace",
        BoardOp::Move { .. } => "Move",
        BoardOp::SetField { .. } => "SetField",
        BoardOp::Detach(_) => "Detach",
        BoardOp::SetStackup(_) => "SetStackup",
        BoardOp::SetOutline(_) => "SetOutline",
        BoardOp::SetRules(_) => "SetRules",
        BoardOp::SetNets { .. } => "SetNets",
    }
}

/// The splice of a commit's board ops on `before.unit`, against the document text and map that
/// `before` was read from. One `SpanEdit` covers the whole commit without reevaluation.
/// The ops apply under the edit rules (`Board::apply_batch`, not host), as the host's commit
/// applies them to the model, so the text writes what the model holds: a moved solved item is
/// written as authored, and an op the edit rules refuse is refused here too.
/// Changed statements are rendered canonically: a non-canonical changed statement is normalised;
/// untouched statements retain their text. Common prefixes and suffixes are left in place.
/// `factory` leaves the profile unchanged with `None`, sets it with `Some(Some(name))`, and clears
/// it with `Some(None)`. A changed profile replaces only its string token.
/// Cost: the ops applied as one batch to a clone of `before` (0.06 ms at 10k statements) and map lookups for
/// the touched items, plus one pass over a kind's items when an insertion is anchored after its
/// last statement; a header edit, a touched tuning or an insertion anchored at the headers also
/// parses the document (20 ms at 10k statements, 550 KB).
pub fn splice_board(sources: &Sources, map: &SourceMap, before: &Board, circuit: &Circuit, ops: &[BoardOp], factory: Option<Option<String>>) -> Result<SpanEdit, Diag> {
    let mut unit = map.locate(&Sel::Unit(before.unit)).ok_or_else(|| Diag::error("the board has no document", vec![Sel::Unit(before.unit)]))?;
    // board(..) is optional. Without it the unit selection points at its circuit.
    if factory.is_some() {
        if let Some(circuit) = sources.doc(unit.doc).filter(|d| d.kind == DocKind::Circuit) {
            let stem = circuit.name.strip_suffix(DocKind::Circuit.suffix()).unwrap();
            if let Some(doc) = sources.find(&format!("{stem}.pcb.splash")) {
                unit = SourceRange { doc, span: 0..0 };
            }
        }
    }
    let error = |message: String| located(unit.doc, unit.span.clone(), message);
    if unit.doc.0 & LIB_DOC != 0 { return Err(error("library board documents are read-only".into())); }
    let source = sources.doc(unit.doc).ok_or_else(|| error("the board document is missing".into()))?;
    if source.kind != DocKind::Board { return Err(Diag::error("the board has no document", vec![Sel::Unit(before.unit)])); }
    let text: &str = &source.text;
    let mut after = before.clone();
    let mut inserted = HashSet::new();
    let mut reinserted = HashSet::new();
    // One batch under the edit rules, as the model applies the change (`Board::apply_batch`);
    // before each op, the lists as the ops so far left them say where an insertion lands.
    let applied = after.apply_batch_seen(ops, false, |board, i| match &ops[i] {
        BoardOp::Insert { item, pos } => {
            reinserted.insert(item.id());
            if item_at(board, item.kind(), *pos as usize).is_some_and(|(_, authored)| authored) {
                inserted.insert(item.id());
            } else {
                inserted.remove(&item.id());
            }
        }
        BoardOp::Add(item) => {
            reinserted.insert(item.id());
            inserted.remove(&item.id());
        }
        _ => {}
    }).map_err(|(i, e)| error(format!("board op {} ({}): {e}", i + 1, op_name(&ops[i]))))?;
    let touched: HashSet<ItemId> = applied.delta.added.iter().chain(&applied.delta.removed).chain(&applied.delta.changed).copied().collect();
    let global = applied.delta.global;
    if touched.is_empty() && !global && factory.is_none() { return Ok(SpanEdit::default()); }
    // The work follows the commit, not the document: spans come from the map for the touched
    // items (and once per kind for an insertion's anchor), and the document is parsed only for a
    // header edit, a touched tuning (track bindings) or an insertion anchored at the headers.
    let parsed = OnceCell::new();
    let tree = || parsed.get_or_init(|| syntax::parse_with(text, &board_doc::units()).map_err(|e| {
        let at = e.at as u32;
        located(unit.doc, at..at, format!("board syntax: {}", e.message))
    })).as_ref().map_err(Diag::clone);
    let headers_of = || -> Result<Vec<Header>, Diag> { Ok(tree()?.statements().iter().filter_map(header).collect()) };
    let old_span = |id: ItemId| -> Result<Option<Range<u32>>, Diag> {
        if !entry(before, id).is_some_and(|(_, _, authored)| authored) { return Ok(None); }
        let range = map.locate(&Sel::Item(before.unit, id))
            .ok_or_else(|| error(format!("board item {} has no statement range", id.0)))?;
        if range.doc != unit.doc || text.get(range.span.start as usize..range.span.end as usize).is_none() {
            return Err(error(format!("board item {} has an invalid statement range", id.0)));
        }
        Ok(Some(range.span))
    };
    let authored_after = |id: ItemId| entry(&after, id).is_some_and(|(_, _, authored)| authored);
    let mut names = HashMap::new();
    let mut used = HashSet::new();
    if after.tunings.iter().any(|t| touched.contains(&t.id) && t.origin.has_statement()) {
        let mut tracks_at = HashMap::new();
        for t in &before.tracks {
            if let Some(span) = old_span(t.id)? { tracks_at.insert(span.start, t.id); }
        }
        for node in tree()?.statements() {
            if let Kind::Let { names: bound, value, .. } = &node.kind {
                used.extend(bound.iter().cloned());
                if bound.len() == 1 && value.as_ref().is_some_and(|v| v.callee_path() == Some("track")) {
                    if let Some(id) = tracks_at.get(&(node.span.start as u32)) { names.insert(*id, bound[0].clone()); }
                }
            }
        }
    } else {
        // A touched track written `let t0 = track(..)` keeps its binding: read it from its statement.
        for id in &touched {
            if !entry(before, *id).is_some_and(|(kind, _, _)| kind == ItemKind::Track) { continue; }
            let Some(span) = old_span(*id)? else { continue };
            let statement = &text[span.start as usize..span.end as usize];
            if !statement.starts_with("let") { continue; }
            if let Ok(one) = syntax::parse_with(statement, &board_doc::units()) {
                if let Some(Kind::Let { names: bound, value, .. }) = one.statements().first().map(|n| &n.kind) {
                    if bound.len() == 1 && value.as_ref().is_some_and(|v| v.callee_path() == Some("track")) {
                        names.insert(*id, bound[0].clone());
                    }
                }
            }
        }
    }
    let old_names = names.clone();
    let mut bindings = Vec::new();
    let mut next_name = 0;
    for tuning in after.tunings.iter().filter(|t| touched.contains(&t.id) && t.origin.has_statement()) {
        for id in tuning.host.iter().chain(&tuning.copper) {
            if !entry(&after, *id).is_some_and(|(kind, _, authored)| kind == ItemKind::Track && authored) {
                return Err(error(format!("tuning {} names a track without a statement", tuning.id.0)));
            }
            if names.contains_key(id) { continue; }
            let name = loop {
                let name = format!("t{next_name}");
                next_name += 1;
                if used.insert(name.clone()) { break name; }
            };
            names.insert(*id, name);
            bindings.push(*id);
        }
    }
    let mut result = SpanEdit::default();
    let mut patches = Vec::new();
    let mut insertions = Vec::new();
    // Touched items in document order of kinds, then list order (as the statements are written).
    let ordered = |board: &Board| {
        let mut ids: Vec<_> = touched.iter().filter_map(|id| entry(board, *id).map(|(kind, i, _)| (item_order(kind), i, *id))).collect();
        ids.sort_unstable();
        ids
    };
    for (_, _, id) in ordered(before) {
        if let Some(span) = old_span(id)? {
            let sel = Sel::Item(before.unit, id);
            if authored_after(id) && !reinserted.contains(&id) {
                let statement = board_doc::item_statement(&after, circuit, id, &names)
                    .ok_or_else(|| error("a changed board statement is missing".into()))?;
                // Some model changes, such as a junction's segment index, have no written field.
                // Compare both model renderings so the person's original spelling survives.
                if board_doc::item_statement(before, circuit, id, &old_names).as_ref() == Some(&statement) {
                    continue;
                }
                if let Some(patch) = replacement(text, span, statement, Some(sel)) { patches.push(patch); }
            } else {
                patches.push(deletion(text, span));
                if !authored_after(id) { result.removes.push(sel); }
            }
        }
    }
    for id in bindings {
        if touched.contains(&id) { continue; }
        if let Some(span) = old_span(id)? {
            let prefix = format!("let {} = ", names[&id]);
            patches.push(Patch {
                span: span.start..span.start,
                marks: vec![Mark { sel: Sel::Item(before.unit, id), offset: 0, len: span.end - span.start + prefix.len() as u32 }],
                text: prefix,
            });
        }
    }
    // The end of the last statement of each kind, found once per kind an insertion needs.
    let mut kind_end: [Option<Option<u32>>; KINDS.len()] = [None; KINDS.len()];
    for (order, index, id) in ordered(&after) {
        let Some((kind, _, authored)) = entry(&after, id) else { continue };
        if !authored || (entry(before, id).is_some_and(|(_, _, authored)| authored) && !reinserted.contains(&id)) { continue; }
        let mut successor = None;
        if inserted.contains(&id) {
            let mut next = index + 1;
            while let Some((next_id, _)) = item_at(&after, kind, next) {
                next += 1;
                if reinserted.contains(&next_id) { continue; }
                if let Some(span) = old_span(next_id)? { successor = Some(span); break; }
            }
        }
        let at = if let Some(span) = successor {
            line_start(text, span.start)
        } else {
            let mut end = None;
            for k in (0..KINDS.len()).rev().filter(|k| item_order(KINDS[*k]) <= order) {
                if kind_end[k].is_none() {
                    let mut last = None;
                    let mut pos = 0;
                    while let Some((item, _)) = item_at(before, KINDS[k], pos) {
                        pos += 1;
                        if let Some(span) = old_span(item)? { last = last.max(Some(span.end)); }
                    }
                    kind_end[k] = Some(last);
                }
                if let Some(Some(e)) = kind_end[k] { end = Some(e); break; }
            }
            match end {
                Some(end) => after_statement(text, end),
                None => headers_of()?.iter().map(|h| after_statement(text, h.span.end)).max().unwrap_or(0),
            }
        };
        let statement = board_doc::item_statement(&after, circuit, id, &names)
            .ok_or_else(|| error("an added board statement is missing".into()))?;
        insertions.push(Insertion { at, order: (order, index), text: statement, sel: Some(Sel::Item(before.unit, id)) });
    }
    if global || factory.is_some() {
        let headers = headers_of()?;
        let set_factory = factory.is_some();
        let profile = factory.as_ref().and_then(|p| p.as_deref());
        let set_stack = ops.iter().any(|op| matches!(op, BoardOp::SetStackup(_)));
        let set_outline = ops.iter().any(|op| matches!(op, BoardOp::SetOutline(_)));
        let set_rules = ops.iter().any(|op| matches!(op, BoardOp::SetRules(_)));
        // A base-only edit owns just rules(..). Deferred rule/check functions may have
        // source statements even when the evaluated board carries no corresponding code.
        let base_only = set_rules && before.rules.base != after.rules.base
            && before.rules.custom == after.rules.custom && before.rules.checks == after.rules.checks
            && before.rules.waivers == after.rules.waivers;
        let selected = |kind| match kind {
            HeaderKind::Board => false,
            HeaderKind::Factory => set_factory,
            HeaderKind::Stackup => set_stack,
            HeaderKind::Outline => set_outline,
            HeaderKind::Rules => set_rules,
            HeaderKind::Rule | HeaderKind::Check | HeaderKind::Waive => set_rules && !base_only,
        };
        let wanted = board_doc::header_statements(&after, circuit, profile, &|_| None);
        let mut waivers = after.rules.waivers.iter();
        let mut rules = after.rules.custom.iter();
        let mut checks = after.rules.checks.iter();
        let mut desired = Vec::new();
        for (kind, statement) in wanted {
            let key = match kind {
                HeaderKind::Waive => waivers.next().map(|w| (format!("{:016x}", w.violation), w.reason.clone())),
                HeaderKind::Rule => rules.next().map(|r| (r.name.clone(), String::new())),
                HeaderKind::Check => checks.next().map(|c| (c.name.clone(), String::new())),
                _ => None,
            };
            if selected(kind) { desired.push((kind, key, statement)); }
        }
        for kind in [HeaderKind::Factory, HeaderKind::Stackup, HeaderKind::Outline, HeaderKind::Rules, HeaderKind::Waive, HeaderKind::Rule, HeaderKind::Check] {
            if !selected(kind) { continue; }
            let existing: Vec<_> = headers.iter().filter(|h| h.kind == kind).collect();
            let wanted: Vec<_> = desired.iter().enumerate().filter(|(_, (k, _, _))| *k == kind).collect();
            if matches!(kind, HeaderKind::Factory | HeaderKind::Stackup | HeaderKind::Outline | HeaderKind::Rules) {
                let sel = (kind == HeaderKind::Outline).then_some(Sel::Feature(FeatureRef::Outline(before.unit)));
                for (i, old) in existing.iter().enumerate() {
                    if i + 1 == existing.len() && !wanted.is_empty() {
                        if let Some((old_profile, span)) = &old.profile {
                            if let Some(profile) = profile.filter(|p| *p != old_profile) {
                                patches.push(Patch { span: span.clone(), text: board_doc::string(profile), marks: Vec::new() });
                            }
                        } else {
                            let statement = wanted[0].1.2.clone();
                            if let Some(patch) = replacement(text, old.span.clone(), statement, sel.clone()) { patches.push(patch); }
                        }
                    } else {
                        patches.push(deletion(text, old.span.clone()));
                    }
                }
                if wanted.is_empty() && !existing.is_empty() {
                    if let Some(sel) = sel { result.removes.push(sel); }
                } else if existing.is_empty() {
                    if let Some((i, (_, _, statement))) = wanted.first() {
                        insertions.push(Insertion {
                            at: header_anchor(text, &headers, kind), order: (kind as usize, *i), text: statement.clone(), sel,
                        });
                    }
                }
                continue;
            }
            // Keep an ordered subsequence in place; moved code keeps its exact text.
            let mut used = HashSet::new();
            let mut retained = HashMap::new();
            let mut originals = HashMap::new();
            let mut cursor = 0;
            for &(i, (_, key, _)) in &wanted {
                if let Some((j, old)) = existing.iter().enumerate().find(|(j, old)| old.key == *key && !used.contains(j)) {
                    used.insert(j);
                    originals.insert(i, *old);
                    if j >= cursor {
                        retained.insert(i, *old);
                        cursor = j + 1;
                    }
                }
            }
            for old in &existing {
                if !retained.values().any(|h| h.span == old.span) { patches.push(deletion(text, old.span.clone())); }
            }
            for &(i, (_, _, statement)) in &wanted {
                if retained.contains_key(&i) { continue; }
                let at = wanted.iter().filter(|(next, _)| *next > i)
                    .find_map(|(next, _)| retained.get(next))
                    .map_or_else(|| header_anchor(text, &headers, kind), |h| line_start(text, h.span.start));
                let statement = originals.get(&i).map_or_else(|| statement.clone(), |h| text[h.span.start as usize..h.span.end as usize].to_string());
                insertions.push(Insertion { at, order: (kind as usize, i), text: statement, sel: None });
            }
        }
    }
    // Inserts at a deleted line's indentation belong at the deletion's start.
    for insertion in &mut insertions {
        if let Some(patch) = patches.iter().find(|p| p.text.is_empty() && p.span.start < insertion.at && insertion.at < p.span.end) {
            insertion.at = patch.span.start;
        }
    }
    insertions.sort_by_key(|i| (i.at, i.order));
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut additions: Vec<Patch> = Vec::new();
    for insertion in insertions {
        if additions.last().is_none_or(|p| p.span.start != insertion.at) {
            let prefix = if insertion.at != 0 && text.as_bytes()[insertion.at as usize - 1] != b'\n' { newline } else { "" };
            additions.push(Patch { span: insertion.at..insertion.at, text: prefix.into(), marks: Vec::new() });
        }
        let patch = additions.last_mut().expect("an insertion has a patch");
        if let Some(sel) = insertion.sel {
            patch.marks.push(Mark { sel, offset: patch.text.len() as i64, len: insertion.text.len() as u32 });
        }
        patch.text.push_str(&insertion.text);
        patch.text.push_str(newline);
    }
    additions.extend(patches);
    additions.sort_by_key(|p| (p.span.start, p.span.end));
    let mut delta = 0i64;
    let mut end = 0;
    for patch in additions {
        if patch.span.start < end { return Err(error("board statement edits overlap".into())); }
        for mark in patch.marks {
            let start = (i64::from(patch.span.start) + delta + mark.offset) as u32;
            result.adds.push((mark.sel, SourceRange { doc: unit.doc, span: start..start + mark.len }));
        }
        delta += patch.text.len() as i64 - i64::from(patch.span.end - patch.span.start);
        end = patch.span.end;
        if let Some(last) = result.edits.last_mut().filter(|e| e.span.is_empty() && e.span.start == patch.span.start) {
            last.span.end = patch.span.end;
            last.text.push_str(&patch.text);
        } else {
            result.edits.push(TextEdit { doc: unit.doc, span: patch.span, text: patch.text });
        }
    }
    Ok(result)
}

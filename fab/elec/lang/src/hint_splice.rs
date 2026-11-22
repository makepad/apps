//! Edits of the single editor-written sheet table in a circuit document.

use crate::circuit_splice::unit_doc;
use crate::map::SourceMap;
use crate::source::{Doc, Sources, located};
use crate::units::circuit_units;
use fab_elec_circuit::{Flow, NetStyle, PortSide, SheetHint};
use fab_elec_core::{BoardId, Diag, DocId};
use fab_elec_design::{Design, TextEdit};
use makepad_script::syntax::{self, Field, Kind, Node, Syntax, write};
use makepad_script::tokenizer::lex_with;
use std::collections::HashSet;
use std::ops::Range;

const LITERAL: &str = "the sheet table is written by the editor: write it as literal entries";

/// Edit one unit's part hints and net styles together. Parse once and insert new entries
/// together in each object, creating one sheet when needed.
pub(crate) fn hints(
    sources: &Sources,
    map: &SourceMap,
    design: &Design,
    unit: BoardId,
    hints: &[(&str, Option<&SheetHint>)],
    styles: &[(&str, Option<NetStyle>)],
) -> Result<Vec<TextEdit>, Diag> {
    let doc = unit_doc(sources, map, design, unit)?;
    let source = doc.text.as_ref();
    let tree = syntax::parse_with(source, &circuit_units())
        .map_err(|e| located(doc.id, e.at as u32..e.at as u32, e.message))?;
    let fail = |span: &Range<usize>, message| located(doc.id, span.start as u32..span.end as u32, message);
    let edit = |span: Range<usize>, text| TextEdit { doc: doc.id, span: span.start as u32..span.end as u32, text };
    let newline = if source.contains("\r\n") { "\r\n" } else { "\n" };
    let updates = final_updates(hints).into_iter().map(|(path, hint)| {
        hint.map(|h| entries(path, h)).transpose()
            .map(|entries| (path, entries))
            .map_err(|message| fail(&(source.len()..source.len()), message))
    }).collect::<Result<Vec<_>, _>>()?;
    let table = table(&tree, doc.id, true)?;
    let mut edits = Vec::new();
    let new_nets = net_styles(doc, table.as_ref(), styles, &mut edits);
    let Some(table) = table else {
        let mut text = String::new();
        for (_, entries) in updates {
            if let Some((part, pins)) = entries {
                text.push_str(&format!("    {part}{newline}"));
                if let Some(pins) = pins { text.push_str(&format!("    {pins}{newline}")); }
            }
        }
        if let Some(nets) = new_nets { text.push_str(&format!("    {nets}{newline}")); }
        if text.is_empty() { return Ok(Vec::new()) }
        let imported = tree.statements().iter().any(|stmt| matches!(&stmt.kind,
            Kind::Use { path } if matches!(path.ident(), Some("mod.elec.*" | "mod.elec.sheet"))));
        let callee = if imported { "sheet" } else { "mod.elec.sheet" };
        let prefix = if source.ends_with('\n') || source.is_empty() { newline.to_string() } else { format!("{newline}{newline}") };
        return Ok(vec![edit(source.len()..source.len(), format!("{prefix}{callee}({{{newline}{text}}}){newline}"))]);
    };
    let Table { stmt, fields, close } = table;
    let mut removals = Vec::new();
    let mut additions = Vec::new();
    for (path, entries) in updates {
        let pins_key = format!("{path}.pins");
        let (part, pins) = entries.map_or((None, None), |(part, pins)| (Some(part), pins));
        for (key, replacement) in [(path, part), (pins_key.as_str(), pins)] {
            let last = fields.iter().rposition(|field| field.name.as_deref() == Some(key));
            for (index, field) in fields.iter().enumerate().filter(|(_, field)| field.name.as_deref() == Some(key)) {
                if Some(index) == last {
                    if let Some(text) = &replacement {
                        edits.push(edit(field.span.clone(), text.clone()));
                        continue;
                    }
                }
                removals.push(remove_entry(doc, fields, close, index));
            }
            if last.is_none() {
                if let Some(text) = replacement { additions.push(text); }
            }
        }
    }
    additions.extend(new_nets);
    if !removals.is_empty() && removals.len() == fields.len() && additions.is_empty() {
        return Ok(vec![edit(whole_line(source, statement_span(source, &stmt.span)), String::new())]);
    }
    // Entry removals include their trailing comma and, when alone, their line.
    // Union touching spans before mixing them with replacements and the insertion.
    removals.sort_by_key(|edit| (edit.span.start, edit.span.end));
    let mut disjoint: Vec<TextEdit> = Vec::new();
    for removal in removals {
        if let Some(previous) = disjoint.last_mut() {
            if removal.span.start <= previous.span.end {
                previous.span.end = previous.span.end.max(removal.span.end);
                continue;
            }
        }
        disjoint.push(removal);
    }
    edits.extend(disjoint);
    if !additions.is_empty() {
        edits.push(append_entries(doc, stmt, close, &additions, newline));
    }
    edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
    Ok(edits)
}

/// Keep the last update for each key, in the order of those final updates.
fn final_updates<'a, T: Copy>(updates: &[(&'a str, T)]) -> Vec<(&'a str, T)> {
    let mut seen = HashSet::new();
    let mut last: Vec<_> = updates.iter().rev().copied().filter(|(key, _)| seen.insert(*key)).collect();
    last.reverse();
    last
}

/// Edit an existing nets object in place, or return the nets field to add to the sheet.
fn net_styles(doc: &Doc, table: Option<&Table<'_>>, styles: &[(&str, Option<NetStyle>)], edits: &mut Vec<TextEdit>) -> Option<String> {
    if styles.is_empty() { return None; }
    let nets = table.and_then(|table| table.fields.iter().rfind(|f| f.name.as_deref() == Some("nets"))).map(|field| {
        let Kind::Object { fields, close, .. } = &field.value.kind else { unreachable!("literal sheet entries are objects") };
        (field, fields.as_slice(), *close)
    });
    let mut additions = Vec::new();
    for (name, style) in final_updates(styles) {
        let value = style.map(|s| match s { NetStyle::Wire => "@wire", NetStyle::Label => "@label" });
        if let Some((_, fields, close)) = nets {
            let mut matches = fields.iter().enumerate().rev().filter(|(_, f)| f.name.as_deref() == Some(name));
            if let Some((index, field)) = matches.next() {
                if let Some(value) = value {
                    edits.push(TextEdit { doc: doc.id, span: field.value.span.start as u32..field.value.span.end as u32, text: value.into() });
                } else {
                    edits.push(remove_entry(doc, fields, close, index));
                }
                edits.extend(matches.map(|(index, _)| remove_entry(doc, fields, close, index)));
                continue;
            }
        }
        if let Some(value) = value { additions.push(format!("{}: {value}", write::quote(name))); }
    }
    if additions.is_empty() { return None; }
    if let Some((field, _, close)) = nets {
        let newline = if doc.text.contains("\r\n") { "\r\n" } else { "\n" };
        edits.push(append_entries(doc, &field.value, close, &additions, newline));
        None
    } else {
        Some(format!("nets: {{{}}}", additions.join(" ")))
    }
}

fn append_entries(doc: &Doc, object: &Node, close: usize, additions: &[String], newline: &str) -> TextEdit {
    let source = doc.text.as_ref();
    let indent = line_indent(source, object.span.start);
    let line = source[..close].rfind('\n').map_or(0, |i| i + 1);
    let own_line = source[line..close].chars().all(|ch| ch == ' ' || ch == '\t');
    let pos = if own_line { line } else { close };
    let mut text = if own_line { String::new() } else { newline.to_string() };
    for entry in additions { text.push_str(&format!("{indent}    {entry}{newline}")); }
    if !own_line { text.push_str(indent); }
    TextEdit { doc: doc.id, span: pos as u32..pos as u32, text }
}

struct Table<'a> {
    stmt: &'a Node,
    fields: &'a [Field],
    close: usize,
}

fn table(tree: &Syntax, doc: DocId, require_literal: bool) -> Result<Option<Table<'_>>, Diag> {
    let mut calls = tree.statements().iter().filter_map(|stmt| {
        let (callee, args) = stmt.as_call()?;
        matches!(callee, "sheet" | "e.sheet" | "mod.elec.sheet").then_some((stmt, args))
    });
    let Some((stmt, args)) = calls.next() else { return Ok(None) };
    if let Some((second, _)) = calls.next() {
        return Err(located(doc, second.span.start as u32..second.span.end as u32,
            "a circuit document may have only one sheet call"));
    }
    match literal_table(stmt, args) {
        Ok(table) => Ok(Some(table)),
        Err(span) if require_literal => Err(located(doc, span.start as u32..span.end as u32, LITERAL)),
        // Rename and Remove leave computed tables alone. Sheet edits require literal entries.
        Err(_) => Ok(None),
    }
}

fn literal_table<'a>(stmt: &'a Node, args: &'a [Node]) -> Result<Table<'a>, &'a Range<usize>> {
    let table = args.first().filter(|_| args.len() == 1).ok_or(&stmt.span)?;
    let table = match &table.kind {
        Kind::Entry { name, value, .. } if name == "table" => value.as_ref(),
        _ => table,
    };
    let Kind::Object { ty: None, fields, close, .. } = &table.kind else {
        return Err(&table.span);
    };
    for field in fields {
        if field.name.is_none() || field.op != ":" {
            return Err(&field.span);
        }
        if !matches!(field.value.kind, Kind::Object { ty: None, .. }) {
            return Err(&field.value.span);
        }
        if let Some(computed) = computed(&field.value) {
            return Err(&computed.span);
        }
    }
    Ok(Table { stmt, fields, close: *close })
}

pub(crate) fn rename_paths(doc: &Doc, tree: &Syntax, paths: &[(&str, String)]) -> Result<Vec<TextEdit>, Diag> {
    let Some(table) = table(tree, doc.id, false)? else { return Ok(Vec::new()) };
    let mut edits = Vec::new();
    for field in table.fields {
        let Some(key) = field.name.as_deref().filter(|key| *key != "nets") else { continue };
        let renamed = paths.iter().filter_map(|(old, new)| {
            let suffix = key.strip_prefix(*old)?;
            (suffix.is_empty() || suffix.starts_with('.') || suffix.starts_with('['))
                .then_some((old.len(), new, suffix))
        }).max_by_key(|(len, _, _)| *len);
        if let Some((_, new, suffix)) = renamed {
            edits.push(TextEdit {
                doc: doc.id,
                span: field.name_span.start as u32..field.name_span.end as u32,
                text: write::quote(&format!("{new}{suffix}")),
            });
        }
    }
    Ok(edits)
}

pub(crate) fn rename_net(doc: &Doc, tree: &Syntax, old: &str, to: &str) -> Result<Vec<TextEdit>, Diag> {
    let Some(table) = table(tree, doc.id, false)? else { return Ok(Vec::new()) };
    let mut edits = Vec::new();
    for field in table.fields.iter().filter(|field| field.name.as_deref() == Some("nets")) {
        let (_, nets) = field.value.object().expect("literal table entries are objects");
        for net in nets.iter().filter(|field| field.name.as_deref() == Some(old)) {
            edits.push(TextEdit {
                doc: doc.id,
                span: net.name_span.start as u32..net.name_span.end as u32,
                text: write::quote(to),
            });
        }
    }
    Ok(edits)
}

pub(crate) fn remove(doc: &Doc, tree: &Syntax, path: &str) -> Result<Vec<TextEdit>, Diag> {
    Ok(table(tree, doc.id, false)?.map_or_else(Vec::new, |table| remove_entries(doc, &table, path)))
}

fn remove_entries(doc: &Doc, table: &Table<'_>, path: &str) -> Vec<TextEdit> {
    let pins = format!("{path}.pins");
    let belongs = |field: &Field| field.name.as_deref().is_some_and(|key| key == path || key == pins);
    if !table.fields.iter().any(belongs) { return Vec::new() }
    if table.fields.iter().all(belongs) {
        let span = whole_line(&doc.text, statement_span(&doc.text, &table.stmt.span));
        return vec![TextEdit { doc: doc.id, span: span.start as u32..span.end as u32, text: String::new() }];
    }
    table.fields.iter().enumerate().filter(|(_, field)| belongs(field))
        .map(|(index, _)| remove_entry(doc, table.fields, table.close, index)).collect()
}

fn remove_entry(doc: &Doc, fields: &[Field], close: usize, index: usize) -> TextEdit {
    let source = doc.text.as_ref();
    let limit = fields.get(index + 1).map_or(close, |field| field.span.start);
    let mut span = fields[index].span.clone();
    if let Some(token) = lex_with(&source[span.end..limit], &circuit_units()).first() {
        if &source[span.end + token.span().start..span.end + token.span().end] == "," {
            span.end += token.span().end;
        }
    }
    let span = whole_line(source, span);
    TextEdit { doc: doc.id, span: span.start as u32..span.end as u32, text: String::new() }
}

fn entries(path: &str, hint: &SheetHint) -> Result<(String, Option<String>), String> {
    let mut fields = Vec::new();
    if let Some(at) = hint.at {
        // A comma separates a negative second coordinate from subtraction in Splash.
        let separator = if at.y < 0 { ", " } else { " " };
        fields.push(format!("at: [{}{separator}{}]", integer(at.x)?, integer(at.y)?));
    }
    if let Some(flow) = hint.flow {
        let name = match flow { Flow::Right => "right", Flow::Down => "down", Flow::Left => "left", Flow::Up => "up" };
        fields.push(format!("flow: @{name}"));
    }
    if hint.turn != 0 {
        if hint.turn > 3 { return Err("sheet turn must be a quarter turn from 0 to 3".into()); }
        fields.push(format!("turn: {}deg", integer(i32::from(hint.turn) * 90)?));
    }
    if hint.mirror {
        fields.push("mirror: true".into());
    }
    if hint.free {
        fields.push("free: true".into());
    }
    let part = format!("{}: {{{}}}", write::quote(path), fields.join(" "));
    let pins = if hint.pins.is_empty() {
        None
    } else {
        let mut pins = Vec::new();
        for (pin, side) in &hint.pins {
            let side = match side { PortSide::Left => "left", PortSide::Right => "right", PortSide::Top => "top", PortSide::Bottom => "bottom" };
            pins.push(format!("{}: @{side}", write::key(pin)?));
        }
        Some(format!("{}: {{{}}}", write::quote(&format!("{path}.pins")), pins.join(" ")))
    };
    Ok((part, pins))
}

fn integer(value: i32) -> Result<String, String> {
    let number = write::number(f64::from(value))?;
    Ok(number.strip_suffix(".0").unwrap_or(&number).to_string())
}

fn computed(node: &Node) -> Option<&Node> {
    match &node.kind {
        Kind::Number(_) | Kind::Name(_) | Kind::Str(_) => None,
        Kind::Ident(name) if matches!(name.as_str(), "true" | "false") => None,
        Kind::Unary { op, operand } if (op == "-" || op == "+") && matches!(operand.kind, Kind::Number(_)) => None,
        Kind::Array { items, .. } => items.iter().find_map(computed),
        Kind::Object { ty: None, fields, .. } => fields.iter().find_map(|field| {
            if field.name.is_none() || field.op != ":" { Some(&field.value) } else { computed(&field.value) }
        }),
        _ => Some(node),
    }
}

fn line_indent(source: &str, at: usize) -> &str {
    let start = source[..at].rfind('\n').map_or(0, |i| i + 1);
    let before = &source[start..at];
    &before[..before.len() - before.trim_start_matches([' ', '\t']).len()]
}

fn statement_span(source: &str, span: &Range<usize>) -> Range<usize> {
    let rest = &source[span.end..];
    let trimmed = rest.trim_start_matches([' ', '\t']);
    let end = if trimmed.starts_with(';') { span.end + rest.len() - trimmed.len() + 1 } else { span.end };
    span.start..end
}

fn whole_line(source: &str, span: Range<usize>) -> Range<usize> {
    let start = source[..span.start].rfind('\n').map_or(0, |i| i + 1);
    let end = source[span.end..].find('\n').map_or(source.len(), |i| span.end + i + 1);
    let tail = source[span.end..end].trim();
    if source[start..span.start].chars().all(|ch| ch == ' ' || ch == '\t')
        && (tail.is_empty() || tail.starts_with("//")) {
        start..end
    } else {
        span
    }
}

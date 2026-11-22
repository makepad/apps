//! Split a named net by inserting a two-terminal statement and rewriting selected pin sites.

use crate::map::{PinSite, SourceMap, SourceRange};
use crate::pin_splice::{check_sites, net_expr, rewrite_site, site_call, site_label};
use crate::source::{located, Sources, LIB_DOC};
use crate::splice::SpanEdit;
use crate::units::circuit_units;
use fab_elec_circuit::Circuit;
use fab_elec_core::{BlockId, BoardId, CompId, CompRef, Diag, DocId, NetId, NetRef, PinIx, Sel};
use fab_elec_design::{Design, TextEdit};
use makepad_script::syntax::{self, Kind, Loc, Node, Syntax};
use makepad_script::tokenizer::{lex_with, ScriptTokenTag};
use std::collections::HashSet;
use std::ops::Range;

pub(crate) fn insert_series(
    sources: &Sources,
    map: &SourceMap,
    design: &Design,
    unit: BoardId,
    net: NetId,
    stmt: &str,
    moved: &[(CompId, PinIx)],
) -> Result<SpanEdit, Diag> {
    let fallback = map.locate(&Sel::Net(NetRef { board: unit, net }))
        .or_else(|| map.locate(&Sel::Unit(unit)))
        .unwrap_or(SourceRange { doc: sources.docs().first().map_or(DocId(0), |doc| doc.id), span: 0..0 });
    let fail = |message| located(fallback.doc, fallback.span.clone(), message);
    if moved.is_empty() {
        return Err(fail("inserting a series part needs at least one pin to move".to_string()));
    }
    let circuit = &design.units.iter().find(|candidate| candidate.id == unit)
        .ok_or_else(|| fail(format!("the design has no unit {}", unit.0)))?.circuit;
    let target = circuit.nets.get(net.ix())
        .ok_or_else(|| fail(format!("the circuit has no net {}", net.0)))?;

    // Check the complete selection before considering source edits. Diagnostics identify the
    // first invalid pin in the supplied order, including a repeated or descending entry.
    let mut previous = None;
    for &(comp, pin) in moved {
        let at = map.locate(&Sel::Comp(CompRef { board: unit, comp })).unwrap_or_else(|| fallback.clone());
        let fail = |message| located(at.doc, at.span.clone(), message);
        let component = circuit.comps.get(comp.ix())
            .ok_or_else(|| fail(format!("component {} pin {} does not exist", comp.0, pin.0)))?;
        let definition = component.part.pins.get(pin.ix())
            .ok_or_else(|| fail(format!("{} pin {} does not exist", component.path, pin.0)))?;
        let label = format!("{} pin {}", component.path, definition.name);
        let key = (comp.0, pin.0);
        if previous.is_some_and(|previous| previous >= key) {
            return Err(fail(format!("{label}: the pins to move must be sorted without repeats by component and pin")));
        }
        if !component.conns.contains(&(pin, net)) {
            return Err(fail(format!("{label} is not on net {}", target.name)));
        }
        previous = Some(key);
    }

    let mut sites = Vec::with_capacity(moved.len());
    for &(comp, pin) in moved {
        let component = &circuit.comps[comp.ix()];
        let at = map.locate(&Sel::Comp(CompRef { board: unit, comp }))
            .ok_or_else(|| fail(format!("{} pin {} has no mapped call", component.path, component.part.pins[pin.ix()].name)))?;
        let found = map.pin_sites(&CompRef { board: unit, comp }, pin);
        check_sites(found, &at)?;
        let label = format!("{} pin {}", component.path, component.part.pins[pin.ix()].name);
        if found.is_empty() {
            return Err(located(at.doc, at.span.clone(), format!("{label} is not connected in the text")));
        }
        if found.len() != 1 {
            let call = site_call(&found[0]).expect("library sites were refused");
            let places: Vec<_> = found.iter().map(|site| site_label(sources, site)).collect();
            return Err(located(call.doc, call.span.clone(), format!(
                "{label} is connected in {} places: {}: edit them by hand", found.len(), places.join("; "),
            )));
        }
        let site = &found[0];
        let call = site_call(site).expect("library sites were refused");
        if matches!(site, PinSite::Field { count: 0, .. } | PinSite::Item { count: 0, .. } | PinSite::Connect { count: 0, .. }) {
            return Err(located(call.doc, call.span.clone(), format!("{label} has no single recorded use of its connection")));
        }
        if call.doc.0 & LIB_DOC != 0 {
            return Err(located(call.doc, call.span.clone(), "this call is in a library, which is not edited here"));
        }
        sites.push((component, site));
    }

    let first = site_call(sites[0].1).expect("library sites were refused");
    let different = |at: &SourceRange| located(at.doc, at.span.clone(),
        "the pins to move are written in different blocks: move them one block at a time");
    for (_, site) in &sites {
        let call = site_call(site).expect("library sites were refused");
        if call.doc != first.doc { return Err(different(call)); }
    }
    let doc = sources.doc(first.doc)
        .ok_or_else(|| located(first.doc, first.span.clone(), "the call's document is not in the project"))?;
    let tree = syntax::parse_with(&doc.text, &circuit_units())
        .map_err(|error| located(doc.id, error.at as u32..error.at as u32, error.message))?;
    let scope = scope_at(&tree, first)?;
    for (component, site) in &sites {
        let call = site_call(site).expect("library sites were refused");
        if scope_at(&tree, call)? != scope
            || (scope != Loc::default() && component.block != sites[0].0.block)
        {
            return Err(different(call));
        }
    }
    let scope_node = tree.node(&scope).expect("the connection scope exists");
    let block = scope_node.block().expect("a connection scope is a block");
    let instance = if scope == Loc::default() { BlockId(0) } else { sites[0].0.block };
    if scope != Loc::default() && instance == BlockId(0) {
        return Err(located(first.doc, first.span.clone(), "the connection's block instance cannot be resolved: edit it by hand"));
    }
    let old_expr = net_expr(&tree, map, circuit, instance, first, net)?;
    let template = template(stmt, first)?;

    let names: HashSet<_> = circuit.nets.iter().map(|net| net.name.as_str()).collect();
    let base = format!("{}_S", target.name);
    let mut new_name = base.clone();
    let mut suffix = 2;
    while names.contains(new_name.as_str()) {
        new_name = format!("{base}{suffix}");
        suffix += 1;
    }
    let base = binding_name(&new_name);
    let mut binding = base.clone();
    let mut suffix = 2;
    while binds(scope_node, &binding)
        || scope.parent().and_then(|parent| tree.node(&parent)).is_some_and(|node| match &node.kind {
            Kind::Fn { name, params, .. } => name.as_deref() == Some(binding.as_str()) || params.iter().any(|param| param.name == binding),
            _ => false,
        })
        || template.0.statements().iter().any(|node| binds(node, &binding))
    {
        binding = format!("{base}{suffix}");
        suffix += 1;
    }
    let mut part = stmt.to_string();
    let mut replacements = vec![(template.1, old_expr), (template.2, binding.clone())];
    if let Some(named) = part_binding(&tree, &scope, &template.0, circuit, instance, Some(&target.name), &[&binding]) {
        replacements.push(named);
    }
    let statement = filled_span(&template.0.statements()[0].span, &replacements);
    replacements.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
    for (span, text) in replacements { part.replace_range(span, &text); }
    let name_literal = syntax::write::write(&syntax::write::Value::Str(new_name))
        .map_err(|message| located(first.doc, first.span.clone(), message))?;
    let declaration = format!("let {binding} = e.net({{name: {name_literal}}})");
    let mut edits: Vec<_> = sites.iter().map(|(_, site)| rewrite_site(&tree, site, &binding))
        .collect::<Result<_, _>>()?;

    // The new binding must exist before every rewritten use. Component ids are evaluation
    // order, but a separate e.connect can put the earliest moved site elsewhere in the text.
    let earliest = sites.iter().filter_map(|(_, site)| site_call(site))
        .min_by_key(|call| call.span.start).expect("there is at least one pin site");
    let anchor = block.stmts.iter().find(|node| contains(&node.span, &earliest.span))
        .ok_or_else(|| located(earliest.doc, earliest.span.clone(), "the pin's connection has no enclosing statement"))?;
    let (insertion, offset) = insert_before(&tree, earliest, anchor, &declaration, &part);
    let start = insertion.span.start + (offset + statement.start - (part.len() - part.trim_start().len())) as u32;
    let mut written = SourceRange { doc: insertion.doc, span: start..start + statement.len() as u32 };
    let delta: i64 = edits.iter().filter(|edit| edit.doc == insertion.doc && edit.span.end <= insertion.span.start)
        .map(|edit| edit.text.len() as i64 - i64::from(edit.span.end - edit.span.start)).sum();
    written.span = (i64::from(written.span.start) + delta) as u32..(i64::from(written.span.end) + delta) as u32;
    edits.push(insertion);
    edits.sort_by_key(|edit| (edit.doc, edit.span.start, edit.span.end));
    for pair in edits.windows(2) {
        if pair[0].doc == pair[1].doc && (pair[0].span.end > pair[1].span.start || pair[0].span == pair[1].span) {
            return Err(located(pair[1].doc, pair[1].span.clone(), "the moved pins need overlapping text edits: edit them by hand"));
        }
    }
    Ok(SpanEdit { edits, written: vec![written], reevaluate: true, ..SpanEdit::default() })
}

fn contains(outer: &Range<usize>, inner: &Range<u32>) -> bool {
    outer.start <= inner.start as usize && inner.end as usize <= outer.end
}

/// Only the document and a block's build body are supported insertion scopes. A nested loop
/// or helper function must not acquire a binding that is invisible to another moved site.
fn scope_at(tree: &Syntax, at: &SourceRange) -> Result<Loc, Diag> {
    let mut scope: Option<(Loc, &Node)> = None;
    tree.walk(&mut |loc, node| {
        if node.block().is_some() && contains(&node.span, &at.span)
            && scope.as_ref().is_none_or(|(_, old)| node.span.len() < old.span.len())
        {
            scope = Some((loc.clone(), node));
        }
    });
    if let Some((loc, node)) = scope {
        if loc == Loc::default() { return Ok(loc); }
        let mut build = false;
        tree.walk(&mut |_, candidate| {
            let Some(field) = candidate.call("e.block").and_then(|args| args.first()).and_then(|options| options.field("build")) else { return };
            if let Kind::Fn { body, .. } = &field.value.kind {
                if body.span == node.span { build = true; }
            }
        });
        if build { return Ok(loc); }
    }
    Err(located(at.doc, at.span.clone(), "the pin connection must be written at the document top or in a block's build body"))
}

fn template(stmt: &str, at: &SourceRange) -> Result<(Syntax, Range<usize>, Range<usize>), Diag> {
    let fail = |message| located(at.doc, at.span.clone(), message);
    let tree = syntax::parse_with(stmt, &circuit_units())
        .map_err(|error| fail(format!("new statement at byte {}: {}", error.at, error.message)))?;
    if tree.statements().len() != 1 {
        return Err(fail(format!("new statement: expected exactly one statement, found {}", tree.statements().len())));
    }
    let tokens = lex_with(stmt, &circuit_units());
    let has = |names: &[&str]| tokens.iter().any(|token|
        token.token().kind() == ScriptTokenTag::Identifier && names.contains(&&stmt[token.span()]));
    let old = has(&["_a", "_b"]);
    let catalog = has(&["_n0", "_n1"]);
    if old && catalog {
        return Err(fail("use _a/_b or _n0/_n1, not both".to_string()));
    }
    let mut spans = Vec::new();
    for name in if catalog { ["_n0", "_n1"] } else { ["_a", "_b"] } {
        let mut uses = Vec::new();
        tree.walk(&mut |_, node| {
            if matches!(&node.kind, Kind::Ident(identifier) if identifier == name) { uses.push(node.span.clone()); }
        });
        // The token count also catches dotted paths, field keys and binding patterns; none
        // may impersonate a plain placeholder. Strings and comments do not count.
        let count = tokens.iter().filter(|token| token.token().kind() == ScriptTokenTag::Identifier && &stmt[token.span()] == name).count();
        if uses.len() != 1 || count != 1 {
            return Err(fail(format!("new statement must contain {name} exactly once as a plain identifier")));
        }
        spans.push(uses.remove(0));
    }
    Ok((tree, spans.remove(0), spans.remove(0)))
}

pub(crate) fn binding_name(name: &str) -> String {
    let mut binding: String = name.to_lowercase().chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '_' { ch } else { '_' }).collect();
    if binding.starts_with(|ch: char| ch.is_ascii_digit()) { binding.insert(0, '_'); }
    binding
}

pub(crate) fn binds(node: &Node, name: &str) -> bool {
    match &node.kind {
        Kind::Block(block) => block.stmts.iter().any(|stmt| binds(stmt, name)),
        Kind::Let { names, .. } => names.iter().any(|binding| binding == name),
        Kind::Fn { name: Some(binding), .. } => binding == name,
        _ => false,
    }
}

fn bound(tree: &Syntax, scope: &Loc, name: &str) -> bool {
    let mut loc = Some(scope.clone());
    while let Some(at) = loc {
        if let Some(node) = tree.node(&at) {
            if node.block().is_some() && binds(node, name) { return true; }
            if let Kind::Fn { name: own, params, .. } = &node.kind {
                if own.as_deref() == Some(name) || params.iter().any(|param| param.name == name) { return true; }
            }
        }
        loc = at.parent();
    }
    false
}

pub(crate) fn binding_taken(tree: &Syntax, scope: &Loc, circuit: &Circuit, block: BlockId, name: &str) -> bool {
    let prefix = &circuit.tree[block.ix()].path;
    let path = if prefix.is_empty() { name.to_string() } else { format!("{prefix}.{name}") };
    bound(tree, scope, name) || circuit.comps.iter().any(|comp| comp.path == path)
        || circuit.nets.iter().any(|net| net.name == path)
}

/// Pick the part's binding independently of the net handle a series insertion also creates.
pub(crate) fn part_binding(
    tree: &Syntax, scope: &Loc, template: &Syntax, circuit: &Circuit, block: BlockId,
    net: Option<&str>, reserved: &[&str],
) -> Option<(Range<usize>, String)> {
    let stmt = template.statements().first()?;
    let Kind::Let { keyword, pattern_span, .. } = &stmt.kind else { return None };
    let stem = stmt.let_name()?;
    if keyword != "let" || template.source()[pattern_span.clone()].trim() != stem
        || !crate::circuit_splice::identifier(stem) { return None; }
    let suffix = net.map(|name| binding_name(name.rsplit('.').next().unwrap_or(name))).unwrap_or_default();
    let suffix = suffix.trim_start_matches('_');
    let base = if suffix.is_empty() { stem.to_string() } else { format!("{stem}_{suffix}") };
    let taken = |name: &str| {
        let mut inside = false;
        stmt.walk(&Loc::default(), &mut |loc, node| {
            // The outer let is the stem being replaced, not a competing declaration.
            if *loc != Loc::default() && binds(node, name) { inside = true; }
            if let Kind::Fn { params, .. } = &node.kind {
                inside |= params.iter().any(|param| param.name == name);
            }
        });
        inside || reserved.contains(&name) || binding_taken(tree, scope, circuit, block, name)
    };
    let mut name = base.clone();
    let mut suffix = 2;
    while taken(&name) {
        name = format!("{base}{suffix}");
        suffix += 1;
    }
    Some((pattern_span.clone(), name))
}

/// Replacements are inside this statement; comments and separators outside it are not owned.
pub(crate) fn filled_span(span: &Range<usize>, replacements: &[(Range<usize>, String)]) -> Range<usize> {
    let delta: i64 = replacements.iter().map(|(span, text)| text.len() as i64 - span.len() as i64).sum();
    span.start..(span.end as i64 + delta) as usize
}

fn line_indent(source: &str, at: usize) -> &str {
    let start = source[..at].rfind('\n').map_or(0, |index| index + 1);
    let before = &source[start..at];
    &before[..before.len() - before.trim_start_matches([' ', '\t']).len()]
}

/// Keep the anchor's indentation, line endings, separators and comments. An inline statement
/// gets its own lines; a statement already on its own line keeps that entire line unchanged.
pub(crate) fn insert_before(tree: &Syntax, at: &SourceRange, anchor: &Node, declaration: &str, part: &str) -> (TextEdit, usize) {
    let source = tree.source();
    let newline = if source.contains("\r\n") { "\r\n" } else { "\n" };
    let indent = line_indent(source, anchor.span.start);
    let mut body = format!("{indent}{declaration}{newline}");
    if !part.trim().is_empty() { body.push_str(&format!("{indent}{}{newline}", part.trim())); }
    let start = source[..anchor.span.start].rfind('\n').map_or(0, |index| index + 1);
    let offset = indent.len() + declaration.len() + newline.len() + indent.len();
    let (pos, text, offset) = if source[start..anchor.span.start].chars().all(|ch| ch == ' ' || ch == '\t') {
        (start, body, offset)
    } else {
        (anchor.span.start, format!("{newline}{body}{indent}"), newline.len() + offset)
    };
    (TextEdit { doc: at.doc, span: pos as u32..pos as u32, text }, offset)
}

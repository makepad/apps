//! Circuit statement edits. Ranges refer to the source before the commit; only the bytes an
//! operation owns are replaced. The caller evaluates the edited documents again.

use crate::{board_doc, hint_splice, pin_splice, series_splice};
use crate::map::{PinSite, SourceMap, SourceRange};
use crate::source::{located, Doc, Sources, LIB_DOC};
use crate::units::circuit_units;
use fab_elec_circuit::{CircuitOp, End, SheetHint};
use fab_elec_core::{BlockId, BoardId, CompId, CompRef, Diag, DocId, NetId, PinIx, Sel};
use fab_elec_design::{Design, DesignOp, TextEdit, Unit};
use makepad_script::syntax::{self, Kind, Loc, Node, Syntax};
use makepad_script::tokenizer::{lex_with, ScriptTokenTag};
use std::collections::{btree_map::Entry, BTreeMap};
use std::ops::Range;

/// Parsed source and generated names for the AddParts of one commit.
#[derive(Default)]
struct AddContext {
    trees: BTreeMap<DocId, Syntax>,
    reserved: BTreeMap<DocId, Vec<String>>,
    shared: BTreeMap<u32, SharedBinding>,
    last_added: Option<u32>,
}

struct SharedBinding {
    block: (BoardId, BlockId),
    doc: DocId,
    scope: Loc,
    first: (usize, usize), // insertion byte, then part in commit order
    name: String,
}

/// One statement insertion and its range within the inserted bytes. The commit appends
/// statements at a common boundary and writes their hinted paths into one sheet table.
pub(crate) struct AddedPart {
    pub statement: TextEdit,
    pub written: Range<usize>,
    pub other: Vec<TextEdit>,
    pub path: Option<String>,
    pub anchor: SourceRange,
    prefix_len: usize,
    next_line: String,
}

/// Fill the templates in op order, parsing each owning document once. Within a scope the
/// earliest use in source gets the declaration, even if the ops name their anchors in reverse.
/// Mark component additions consecutively above the project's highest `added`, scanned once.
/// Block instances and passives with neither literal options nor a pin slot to locate their
/// options are not yet marked, so they still number by evaluation order.
pub(crate) fn add_parts(sources: &Sources, map: &SourceMap, design: &Design, ops: &[DesignOp]) -> Result<Vec<AddedPart>, Diag> {
    let mut ctx = AddContext::default();
    let mut parts = Vec::new();
    for op in ops {
        if let DesignOp::Circuit { unit, op: CircuitOp::AddPart { stmt, after, ends, hint } } = op {
            ctx.last_added.get_or_insert_with(|| map.added.values().flatten().map(|(_, n)| *n).max().unwrap_or(0));
            parts.push(add_part(sources, map, design, *unit, stmt, *after, ends, hint.as_ref(), parts.len(), &mut ctx)?);
        }
    }
    let mut declarations = BTreeMap::<usize, String>::new();
    for binding in ctx.shared.into_values() {
        let i = binding.first.1;
        declarations.entry(i).or_default().push_str(&format!("let {} = e.net(){}", binding.name, parts[i].next_line));
    }
    for (i, text) in declarations {
        let part = &mut parts[i];
        part.statement.text.insert_str(part.prefix_len, &text);
        part.written = part.written.start + text.len()..part.written.end + text.len();
    }
    Ok(parts)
}

/// One statement inserted on its own, after `after`'s statement (no ends, no sheet hint): its
/// edits, the statement last. For a pin splice's new `e.connect`.
pub(crate) fn insert_statement(
    sources: &Sources, map: &SourceMap, design: &Design, unit: BoardId, stmt: &str, after: Option<CompId>,
) -> Result<Vec<TextEdit>, Diag> {
    let part = add_part(sources, map, design, unit, stmt, after, &[], None, 0, &mut AddContext::default())?;
    let mut edits = part.other;
    edits.push(part.statement);
    Ok(edits)
}

fn add_part(
    sources: &Sources,
    map: &SourceMap,
    design: &Design,
    unit: BoardId,
    stmt: &str,
    after: Option<CompId>,
    ends: &[End],
    hint: Option<&SheetHint>,
    index: usize,
    ctx: &mut AddContext,
) -> Result<AddedPart, Diag> {
    let (doc, at) = match after {
        Some(comp) => mapped(sources, map, design, unit, &Sel::Comp(CompRef { board: unit, comp }))?,
        None => {
            let doc = unit_doc(sources, map, design, unit)?;
            (doc, SourceRange { doc: doc.id, span: 0..0 })
        }
    };
    let mut added = syntax::parse_with(stmt, &circuit_units()).map_err(|e| {
        let (line, col) = crate::source::line_col(stmt, e.at as u32).unwrap_or((1, 1));
        located(at.doc, at.span.clone(), format!("new statement at {line}:{col} (byte {}): {}", e.at, e.message))
    })?;
    if added.statements().len() != 1 {
        let offset = added.statements().get(1).map_or(0, |n| n.span.start);
        let (line, col) = added.line_col(offset);
        return Err(located(at.doc, at.span.clone(), format!("new statement at {line}:{col}: expected exactly one statement, found {}", added.statements().len())));
    }
    let fail = |message| located(at.doc, at.span.clone(), message);
    let mut slots = placeholders(&added, &at)?;
    if let (Some(last), Some(call)) = (&mut ctx.last_added, statement_call(&added.statements()[0])) {
        if let Some(index) = added_options_index(call, &slots) {
            *last = last.checked_add(1).ok_or_else(|| fail("added: no higher positive whole number fits in u32".into()))?;
            let edits = write_option(added.source(), call, index, "added", &last.to_string()).map_err(|e| fail(e.message))?;
            added = added.apply(&edits).map_err(|e| fail(e.message))?;
            slots = placeholders(&added, &at)?;
        }
    }
    for index in 0..ends.len() {
        if !slots.contains_key(&index) {
            return Err(fail(format!("the new statement has no _n{index}")));
        }
    }
    let added = drop_open_pin_fields(added, &slots, ends).map_err(|e| fail(e.message))?;
    let stmt = added.source();
    let tree: &Syntax = match ctx.trees.entry(doc.id) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => entry.insert(parse(doc)?),
    };
    let anchor = if after.is_some() {
        Some(statement(tree, &at).ok_or_else(|| fail("the component's call has no enclosing statement".to_string()))?)
    } else { None };
    let scope = anchor.as_ref().map_or_else(Loc::default, |(scope, _)| scope.clone());
    let circuit = &design.units.iter().find(|u| u.id == unit).expect("the insertion document has a unit").circuit;
    let instance = after.map_or(BlockId(0), |comp| circuit.comps[comp.ix()].block);
    let block = circuit.tree.get(instance.ix()).ok_or_else(|| fail("the component's block is missing".to_string()))?;
    let placeholders = placeholders(&added, &at)?;
    let newline = newline(&doc.text);
    let (pos, mut prefix, suffix) = if let Some((scope, index)) = &anchor {
        let block = tree.node(scope).and_then(Node::block).expect("a statement's scope is a block");
        let node = &block.stmts[*index];
        let end = block.stmts.get(index + 1).map_or(block.close.unwrap_or(doc.text.len()), |n| n.span.start);
        let indent = line_indent(&doc.text, node.span.start);
        match next_line(&doc.text[node.span.end..end]) {
            Some(delta) => (node.span.end + delta, indent.to_string(), newline.to_string()),
            None => {
                // An inline block or a last line without a newline: insert before the next
                // statement/brace, after any trailing comment, and give it its indentation back.
                let following = if end < doc.text.len() { line_indent(&doc.text, end) } else { "" };
                (end, format!("{newline}{indent}"), format!("{newline}{following}"))
            }
        }
    } else {
        let prefix = if doc.text.is_empty() || doc.text.ends_with('\n') { "" } else { newline };
        (doc.text.len(), prefix.to_string(), newline.to_string())
    };
    // Resolve all fallback pins together: a repeated slot shares its binding, and different
    // pins of one part can share a single pins-object edit without overlapping replacements.
    // A net on one pin only that no name reaches (a placed part's open end, written in place) and
    // whose pin has no field name ("1") is bound the same way: its pin entry is rewritten to the
    // new binding.
    let lone_unreachable = |net: NetId| match circuit.pins_of(net).as_slice() {
        &[(comp, pin)] if pin_splice::net_expr(tree, map, circuit, instance, &at, net).is_err()
            && pin_splice::pin_expr(tree, map, circuit, instance, &at, comp, pin).is_err() => Some((comp, pin)),
        _ => None,
    };
    let mut free = Vec::new();
    for end in ends {
        let fallback = match end {
            End::Pin(comp, pin) => match circuit.net_of(*comp, *pin) {
                None => pin_splice::pin_expr(tree, map, circuit, instance, &at, *comp, *pin).is_err().then_some((*comp, *pin)),
                Some(net) => lone_unreachable(net),
            },
            End::Net(net) => lone_unreachable(*net),
            End::Shared(_) | End::Open => None,
        };
        if let Some(pin) = fallback.filter(|pin| !free.contains(pin)) {
            free.push(pin);
        }
    }
    let insertion = SourceRange { doc: at.doc, span: pos as u32..pos as u32 };
    let (bindings, mut edits) = if free.is_empty() { (Vec::new(), Vec::new()) } else {
        let groups: Vec<_> = free.iter().map(|pin| vec![*pin]).collect();
        pin_splice::bind_free_pins(sources, map, design, unit, &groups, Some((instance, &insertion, &scope)))?
    };
    let reserved = ctx.reserved.entry(at.doc).or_default();
    let next_line = format!("{newline}{}", prefix.trim_start_matches(['\r', '\n']));
    let mut replacements = Vec::new();
    for (slot, span) in placeholders {
        let expr = match ends.get(slot) {
            None | Some(End::Open) => "e.net()".to_string(),
            Some(End::Shared(key)) => {
                if let Some(binding) = ctx.shared.get_mut(key) {
                    if binding.block != (unit, instance) || binding.doc != at.doc || binding.scope != scope {
                        return Err(fail(format!("shared net {key} is not in scope at this statement")));
                    }
                    binding.first = binding.first.min((pos, index));
                    binding.name.clone()
                } else {
                    let base = format!("wire{key}");
                    let mut name = base.clone();
                    let mut suffix = 2;
                    while series_splice::binding_taken(tree, &scope, circuit, instance, &name)
                        || reserved.contains(&name) || bindings.contains(&name) {
                        name = format!("{base}_{suffix}");
                        suffix += 1;
                    }
                    reserved.push(name.clone());
                    ctx.shared.insert(*key, SharedBinding { block: (unit, instance), doc: at.doc,
                        scope: scope.clone(), first: (pos, index), name: name.clone() });
                    name
                }
            }
            // A net no name reaches here (a placed part's open end, written in place) is reached
            // through a pin on it instead: connecting to that pin joins its net.
            Some(End::Net(net)) => match pin_splice::net_expr(tree, map, circuit, instance, &at, *net) {
                Ok(expr) => expr,
                Err(error) => {
                    let pins = circuit.pins_of(*net);
                    match pins.iter().find_map(|pin| free.iter().position(|other| other == pin)) {
                        Some(index) => bindings[index].clone(),
                        None => pins.into_iter()
                            .find_map(|(comp, pin)| pin_splice::pin_expr(tree, map, circuit, instance, &at, comp, pin).ok())
                            .ok_or(error)?,
                    }
                }
            },
            Some(End::Pin(comp, pin)) => {
                if let Some(net) = circuit.net_of(*comp, *pin) {
                    match pin_splice::net_expr(tree, map, circuit, instance, &at, net) {
                        Ok(expr) => expr,
                        Err(error) => match free.iter().position(|other| other == &(*comp, *pin)) {
                            Some(index) => bindings[index].clone(),
                            None => pin_splice::pin_expr(tree, map, circuit, instance, &at, *comp, *pin).map_err(|_| error)?,
                        },
                    }
                } else if let Some(index) = free.iter().position(|other| other == &(*comp, *pin)) {
                    bindings[index].clone()
                } else {
                    pin_splice::pin_expr(tree, map, circuit, instance, &at, *comp, *pin)?
                }
            }
        };
        replacements.push((span, expr));
    }
    let net_name = ends.iter().find_map(|end| match end {
        End::Shared(_) | End::Open => None,
        End::Net(net) => Some(match circuit.pins_of(*net).iter().find_map(|pin| free.iter().position(|other| other == pin)) {
            Some(index) => bindings[index].clone(),
            None => circuit.nets[net.ix()].name.clone(),
        }),
        End::Pin(comp, pin) => Some(if let Some(index) = free.iter().position(|other| other == &(*comp, *pin)) {
            bindings[index].clone()
        } else if let Some(net) = circuit.net_of(*comp, *pin) {
            circuit.nets[net.ix()].name.clone()
        } else if let Some(index) = free.iter().position(|other| other == &(*comp, *pin)) {
            bindings[index].clone()
        } else {
            let component = &circuit.comps[comp.ix()];
            format!("{}.{}", component.path, component.part.pins[pin.ix()].name)
        }),
    });
    reserved.extend(bindings);
    let taken: Vec<_> = reserved.iter().map(String::as_str).collect();
    let name = series_splice::part_binding(tree, &scope, &added, circuit, instance,
        net_name.as_deref(), &taken);
    if let Some((_, name)) = &name { reserved.push(name.clone()); }
    let path = name.as_ref().map(|(_, name)| if block.path.is_empty() { name.clone() } else { format!("{}.{name}", block.path) });
    if hint.is_some() && path.is_none() {
        return Err(fail("a sheet hint needs the new part to have a name (write let <stem> = ..)".to_string()));
    }
    replacements.extend(name);
    let written = series_splice::filled_span(&added.statements()[0].span, &replacements);
    replacements.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
    let mut part = stmt.to_string();
    for (span, text) in replacements { part.replace_range(span, &text); }
    let stmt = part.trim();
    // bind_free_pins already combines its insertions. A connection at this boundary goes first.
    if let Some(i) = edits.iter().position(|e| e.doc == at.doc && e.span == insertion.span) {
        prefix.insert_str(0, &edits.remove(i).text);
    }
    let start = prefix.len() + written.start - (part.len() - part.trim_start().len());
    Ok(AddedPart { statement: edit(at.doc, pos..pos, format!("{prefix}{stmt}{suffix}")),
        written: start..start + written.len(), other: edits, path, anchor: at, prefix_len: prefix.len(), next_line })
}

/// Only whole fields of a pins object disappear; positional and list slots remain expressions.
fn drop_open_pin_fields(
    mut tree: Syntax, slots: &BTreeMap<usize, Range<usize>>, ends: &[End],
) -> Result<Syntax, syntax::EditError> {
    let mut objects = Vec::new();
    tree.walk(&mut |loc, node| {
        let Some((_, fields)) = node.object() else { return };
        for (index, field) in fields.iter().enumerate().filter(|(_, field)| field.name.as_deref() == Some("pins")) {
            let Some((_, pins)) = field.value.object() else { continue };
            let removed: Vec<_> = pins.iter().enumerate().filter_map(|(i, pin)| {
                (pin.name.is_some() && slots.iter().any(|(slot, span)| {
                    *span == pin.value.span && matches!(ends.get(*slot), None | Some(End::Open))
                })).then_some(i)
            }).collect();
            if !removed.is_empty() { objects.push((loc.child(index), removed, pins.len())); }
        }
    });
    // Removing later children first keeps all earlier syntax locations valid.
    for (loc, removed, count) in objects.into_iter().rev() {
        if removed.len() == count {
            tree = tree.replace(&loc, "{}")?.syntax;
        } else {
            for index in removed.into_iter().rev() {
                tree = tree.remove_element(&loc, index)?.syntax;
            }
        }
    }
    Ok(tree)
}

/// A placeholder must be one expression, never a field key, path segment or binding pattern.
fn placeholders(tree: &Syntax, at: &SourceRange) -> Result<BTreeMap<usize, Range<usize>>, Diag> {
    let source = tree.source();
    let mut names: BTreeMap<usize, Vec<Range<usize>>> = BTreeMap::new();
    for token in lex_with(source, &circuit_units()) {
        if token.token().kind() != ScriptTokenTag::Identifier { continue; }
        let name = &source[token.span()];
        let Some(digits) = name.strip_prefix("_n") else { continue };
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) { continue; }
        let index = digits.parse::<usize>().map_err(|_| located(at.doc, at.span.clone(), format!("{name} has an invalid placeholder index")))?;
        names.entry(index).or_default().push(token.span());
    }
    let mut uses = Vec::new();
    tree.walk(&mut |_, node| {
        if matches!(&node.kind, Kind::Ident(_)) { uses.push(node.span.clone()); }
    });
    names.into_iter().map(|(index, spans)| {
        if spans.len() != 1 || !uses.contains(&spans[0]) {
            return Err(located(at.doc, at.span.clone(), format!("new statement must contain _n{index} exactly once as a plain identifier")));
        }
        Ok((index, spans[0].clone()))
    }).collect()
}

/// Find the maker's options in the unfilled template. A shared source argument can evaluate
/// as a net or a tolerance, but pin placeholders in an editor template identify the pin positions.
fn added_options_index(call: &Node, slots: &BTreeMap<usize, Range<usize>>) -> Option<usize> {
    let (maker, args) = call.as_call()?;
    match maker {
        "e.part" => Some(1),
        "e.led" => Some(0),
        "e.r" | "e.c" => {
            if let Some(index) = [4, 3].into_iter().find(|&i|
                crate::circuit::comp_options_arg(args, i).is_some_and(|(_, opts)| opts.object().is_some())
            ) { return Some(index); }
            // Named pin arguments can be written in reverse order.
            let first_pin = args.iter().enumerate().filter_map(|(i, arg)| {
                let (position, value) = match &arg.kind {
                    Kind::Entry { name, value, .. } => match name.as_str() {
                        "a" => (1, value.as_ref()), "b" => (2, value.as_ref()), _ => return None,
                    },
                    _ => (i, arg),
                };
                (matches!(position, 1 | 2) && slots.values().any(|span| *span == value.span)).then_some(position)
            }).min()?;
            Some(crate::value_site::two_pin_net_arg(1, first_pin == 2) + 1)
        }
        _ => None,
    }
}

/// Set a field in the maker's options, retaining its other fields, separators and comments.
/// Only the call's arguments and the options object's fields are examined.
fn write_option(source: &str, call: &Node, index: usize, key: &str, value: &str) -> Result<Vec<syntax::edit::Replace>, syntax::EditError> {
    use syntax::edit::Replace;
    let args = call.as_call().expect("a component maker has arguments").1;
    let object = || format!("{{{key}: {value}}}");
    let Some((_, opts)) = crate::circuit::comp_options_arg(args, index) else {
        // Naming the options keeps omitted ends open.
        let name = if index == 4 { "rest" } else { "opts" };
        let arg = if args.len() == index { object() } else { format!("{name}: {}", object()) };
        return syntax::edit::insert_element_edits(source, call, syntax::At::End, &arg);
    };
    let Kind::Object { fields, open, .. } = &opts.kind else {
        if opts.ident() == Some("nil") { return Ok(vec![Replace::new(opts.span.clone(), object())]); }
        let message = if key == "ref" { "the component's options are not a literal object: edit its reference in the source" }
            else { "the new component needs a literal options object to write added" };
        return Err(syntax::EditError { message: message.into() });
    };
    if let Some(field) = opts.field(key) { return Ok(vec![Replace::new(field.value.span.clone(), value)]); }
    if fields.is_empty() {
        return Ok(vec![Replace::new(open + 1..open + 1, format!("{key}: {value}"))]);
    }
    syntax::edit::add_field_edits(source, opts, syntax::At::End, key, value)
}

/// Splice one reference into its component call, parsing the owning document once per edit.
pub(crate) fn set_ref(
    sources: &Sources, map: &SourceMap, design: &Design, unit: BoardId, comp: CompId, to: Option<&str>,
) -> Result<Vec<TextEdit>, Diag> {
    let target = CompRef { board: unit, comp };
    let (doc, at) = mapped(sources, map, design, unit, &Sel::Comp(target))?;
    let fail = |message| located(at.doc, at.span.clone(), message);
    if components_at(map, &at) > 1 {
        return Err(fail("this call makes several components: edit their references in the source"));
    }
    if to.is_some_and(|text| !crate::circuit::valid_ref(text)) {
        return Err(fail("ref: expected nonempty text without whitespace"));
    }
    let tree = parse(doc)?;
    let call = crate::value_site::call_at(&tree, &(at.span.start as usize..at.span.end as usize))
        .ok_or_else(|| fail("the component's mapped call is missing"))?;
    let Kind::Call { args, open, close, .. } = &call.kind else { unreachable!("call_at finds calls") };
    let index = match call.callee_path().and_then(|path| path.rsplit('.').next()) {
        Some("part") => 1,
        Some("led") => 0,
        Some("r" | "c") => {
            let tol = map.pin_sites(&target, PinIx(0)).iter().any(|s| matches!(s, PinSite::Item { positional_tol: true, .. }));
            crate::value_site::two_pin_net_arg(1, tol) + 1
        }
        _ => return Err(fail("only e.part, e.r, e.c and e.led calls write a component's reference")),
    };
    let source = tree.source();
    if let Some(to) = to {
        let changes = write_option(source, call, index, "ref", &syntax::write::quote(to))
            .map_err(|e| located(at.doc, at.span.clone(), e.message))?;
        return Ok(changes.into_iter().map(|change| edit(at.doc, change.range, change.text)).collect());
    }
    let Some((i, opts)) = crate::circuit::comp_options_arg(args, index) else { return Ok(vec![]) };
    let Kind::Object { fields, open: start, close: end, ty } = &opts.kind else {
        if opts.ident() == Some("nil") { return Ok(vec![]); }
        return Err(fail("the component's options are not a literal object: edit its reference in the source"));
    };
    let Some(j) = fields.iter().position(|f| f.name.as_deref() == Some("ref")) else { return Ok(vec![]) };
    let (open, close, spans, index) = if ty.is_none() && fields.len() == 1 {
        (*open, *close, args.iter().map(|a| a.span.clone()).collect::<Vec<_>>(), i)
    } else {
        (*start, *end, fields.iter().map(|f| f.span.clone()).collect(), j)
    };
    let (span, text) = pin_splice::entry_removal(source, open, close, &spans, index, index);
    Ok(vec![edit(at.doc, span, text)])
}

fn components_at(map: &SourceMap, at: &SourceRange) -> usize {
    map.all_at(at.doc, at.span.start).iter()
        .filter(|sel| matches!(sel, Sel::Comp(_)) && map.locate(sel).as_ref() == Some(at)).count()
}

/// The models document of `unit`, where a part's new `e.body` override goes:
/// `<unit>.models.part.splash`.
pub fn models_doc_name(unit: &str) -> String {
    format!("{unit}.models{}", crate::source::DocKind::Part.suffix())
}

/// `CircuitOp::SetBody`: rewrite or remove the `e.body` override of the part named `part` where
/// it is written (a project document; never a library's), or write a new one at the end of the
/// unit's models document. A mesh path in `body` is the project's: a statement in a folder's
/// document names it from there. Removing an override that is not written changes nothing.
pub(crate) fn set_body(
    sources: &Sources, map: &SourceMap, design: &Design, unit: BoardId, part: &str, body: Option<&str>,
) -> Result<Vec<TextEdit>, Diag> {
    let written = |doc: &Doc, body: &str| {
        let depth = doc.name.matches('/').count();
        let body = if depth == 0 { body.to_string() } else { body.replace("mesh: \"", &format!("mesh: \"{}", "../".repeat(depth))) };
        format!("e.body({}, {body})", syntax::write::quote(part))
    };
    if let Some(site) = map.body_site(unit, part) {
        let at = &site.at;
        let fail = |message: &str| located(at.doc, at.span.clone(), format!("{part}: {message}"));
        if at.doc.0 & LIB_DOC != 0 {
            return Err(fail("its body is set in a library document: set it in the project instead"));
        }
        let doc = sources.doc(at.doc).ok_or_else(|| fail("the document of its e.body is missing"))?;
        let tree = parse(doc)?;
        let (scope, index) = statement(&tree, at).ok_or_else(|| fail("its e.body call has no enclosing statement: edit it by hand"))?;
        let node = &tree.node(&scope).and_then(Node::block).expect("a statement's scope is a block").stmts[index];
        if statement_call(node).is_none_or(|call| !same_span(call, at)) {
            return Err(fail("its e.body call is inside another expression: edit it by hand"));
        }
        return Ok(vec![match body {
            Some(body) => edit(at.doc, at.span.start as usize..at.span.end as usize, written(doc, body)),
            None => edit(at.doc, removal_span(&doc.text, &node.span), String::new()),
        }]);
    }
    let Some(body) = body else { return Ok(vec![]) };
    let name = design.units.iter().find(|u| u.id == unit).map(|u| models_doc_name(&u.name))
        .ok_or_else(|| Diag::error(format!("{part}: no unit {unit:?}"), vec![]))?;
    let doc = sources.find(&name).and_then(|id| sources.doc(id)).filter(|doc| doc.present)
        .ok_or_else(|| Diag::error(format!("{part}: {name} is not in the project: the commit adds it first"), vec![]))?;
    let text: &str = &doc.text;
    let nl = newline(text);
    let lead = if text.is_empty() || text.ends_with('\n') { "" } else { nl };
    Ok(vec![edit(doc.id, text.len()..text.len(), format!("{lead}{}{nl}", written(doc, body)))])
}

pub(crate) fn remove(
    sources: &Sources,
    map: &SourceMap,
    design: &Design,
    unit: BoardId,
    comp: CompId,
) -> Result<Vec<TextEdit>, Diag> {
    let (doc, at) = mapped(sources, map, design, unit, &Sel::Comp(CompRef { board: unit, comp }))?;
    let count = components_at(map, &at);
    if count > 1 {
        return Err(located(at.doc, at.span.clone(), format!("this call makes {count} components: remove them by hand")));
    }
    let tree = parse(doc)?;
    let (scope, index) = statement(&tree, &at).ok_or_else(|| located(at.doc, at.span.clone(), "the component's call has no enclosing statement: remove it by hand"))?;
    let node = &tree.node(&scope).and_then(Node::block).expect("a statement's scope is a block").stmts[index];
    if statement_call(node).is_none_or(|call| !same_span(call, &at)) {
        return Err(located(at.doc, at.span.clone(), "the component's call is inside another expression: remove it by hand"));
    }
    let circuit = &design.units.iter().find(|u| u.id == unit).expect("mapped checked the unit").circuit;
    with_sheet_edits(
        vec![edit(at.doc, removal_span(&doc.text, &node.span), String::new())],
        hint_splice::remove(doc, &tree, &circuit.comps[comp.ix()].path)?,
    )
}

pub(crate) fn rename(
    sources: &Sources,
    map: &SourceMap,
    design: &Design,
    unit: BoardId,
    what: &Sel,
    to: &str,
) -> Result<Vec<TextEdit>, Diag> {
    let (doc, at) = mapped(sources, map, design, unit, what)?;
    let tree = parse(doc)?;
    let u = design.units.iter().find(|u| u.id == unit).expect("mapped checked the unit");
    let circuit = &u.circuit;
    // A net's explicit name is content; its local handle need not have the same spelling.
    if let Sel::Net(net) = what {
        let mut name = None;
        tree.walk(&mut |_, node| {
            if same_span(node, &at) {
                if let Kind::Call { args, .. } = &node.kind {
                    name = args.first().and_then(|o| o.field("name"))
                        .filter(|f| matches!(f.value.kind, Kind::Str(_))).map(|f| &f.value);
                }
            }
        });
        if let Some(name) = name {
            // A name is content, not a binding: it collides only with another net's name.
            if to.trim().is_empty() || circuit.nets.iter().any(|n| n.name == to && n.id != net.net) {
                return Err(located(at.doc, at.span.clone(), format!("a net named {to:?} exists: pick another name")));
            }
            let old = &circuit.nets[net.net.ix()].name;
            let mut edits = with_sheet_edits(
                vec![edit(at.doc, name.span.clone(), syntax::write::quote(to))],
                hint_splice::rename_net(doc, &tree, old, to)?,
            )?;
            edits.extend(rename_board(sources, u, &[], &[(old, to.to_string())]));
            return Ok(edits);
        }
    }
    if !identifier(to) {
        return Err(located(at.doc, at.span.clone(), format!("{to:?} is not a binding identifier")));
    }
    let fail = || located(at.doc, at.span.clone(), "this item is not the value of a simple let binding: rename it by hand");
    let (scope, index) = rename_binding(&tree, &at).ok_or_else(fail)?;
    let scope_node = tree.node(&scope).expect("the enclosing scope exists");
    let block = scope_node.block().expect("a statement's scope is a block");
    let node = &block.stmts[index];
    let old = node.let_name().ok_or_else(fail)?;
    let Kind::Let { pattern_span, .. } = &node.kind else { return Err(fail()) };
    if !doc.text[pattern_span.clone()].starts_with(old) {
        return Err(fail());
    }
    if old == to {
        return Ok(Vec::new());
    }
    if bound_in_scope(&tree, &scope, to) {
        return Err(located(at.doc, at.span.clone(), format!("{to:?} is already bound in this scope")));
    }
    let mut spans = vec![pattern_span.start..pattern_span.start + old.len()];
    // Earlier closures can capture a binding declared later in the same scope. Do not cross
    // a previous declaration of the old name; those earlier uses belong to that binding.
    let start = block.stmts[..index].iter().rposition(|n| binds(n, old)).map_or(0, |i| i + 1);
    for (i, statement) in block.stmts.iter().enumerate().skip(start) {
        if i == index { continue; }
        if binds(statement, old) {
            break;
        }
        uses(statement, &doc.text, old, to, false, &mut spans)
            .map_err(|span| located(at.doc, span.start as u32..span.end as u32, format!("renaming to {to:?} would capture this use in another scope")))?;
    }
    let selections: Vec<_> = map.all_at(at.doc, at.span.start).into_iter()
        .filter(|sel| map.locate(sel).as_ref() == Some(&at))
        .collect();
    let renamed_binding = |path: &str| {
        let start = path.rfind('.').map_or(0, |dot| dot + 1);
        let suffix = path[start..].strip_prefix(old)?;
        // Loop indices stay attached to the renamed binding's final segment.
        (suffix.is_empty() || suffix.starts_with('['))
            .then(|| format!("{}{to}{suffix}", &path[..start]))
    };
    let paths: Vec<_> = selections.iter()
        .filter_map(|sel| match sel {
            Sel::Comp(c) if c.board == unit => circuit.comps.get(c.comp.ix()).map(|c| c.path.as_str()),
            Sel::Block(board, block) if *board == unit => circuit.tree.get(block.ix()).map(|b| b.path.as_str()),
            _ => None,
        })
        .filter_map(|path| renamed_binding(path).map(|new| (path, new))).collect();
    let nets: Vec<_> = selections.iter().filter_map(|sel| match sel {
        Sel::Net(n) if matches!(what, Sel::Net(_)) && n.board == unit => circuit.nets.get(n.net.ix()),
        _ => None,
    }).filter_map(|n| renamed_binding(&n.name).map(|new| (n.name.as_str(), new))).collect();
    let mut sheet = hint_splice::rename_paths(doc, &tree, &paths)?;
    for (old, new) in &nets { sheet.extend(hint_splice::rename_net(doc, &tree, old, new)?); }
    let mut edits = with_sheet_edits(
        spans.into_iter().map(|span| edit(at.doc, span, to.to_string())).collect(),
        sheet,
    )?;
    edits.extend(rename_board(sources, u, &paths, &nets));
    edits.extend(crate::sys_splice::rename_paths(sources, design, unit, &paths)?);
    Ok(edits)
}

fn renamed_path(path: &str, paths: &[(&str, String)]) -> Option<String> {
    paths.iter().filter_map(|(old, new)| {
        let suffix = path.strip_prefix(*old)?;
        (suffix.is_empty() || suffix.starts_with('.') || suffix.starts_with('['))
            .then_some((old.len(), new, suffix))
    }).max_by_key(|(len, _, _)| *len).map(|(_, new, suffix)| format!("{new}{suffix}"))
}

fn rename_board(sources: &Sources, unit: &Unit, paths: &[(&str, String)], nets: &[(&str, String)]) -> Vec<TextEdit> {
    let Some(doc) = sources.find(&format!("{}.pcb.splash", unit.name)).and_then(|id| sources.doc(id))
        .filter(|doc| doc.id.0 & LIB_DOC == 0 && doc.lib.is_none()) else { return Vec::new() };
    // The reader visits each token once; name_sites returns them in disjoint source order.
    board_doc::name_sites(doc.id, &doc.text, &unit.circuit).into_iter().filter_map(|site| {
        let new = match site.name {
            board_doc::Name::Component(path) | board_doc::Name::Block(path) => renamed_path(&path, paths),
            board_doc::Name::Pad { path, pad } => renamed_path(&path, paths).map(|path| format!("{path}:{pad}")),
            board_doc::Name::Net(name) => nets.iter().find(|(old, _)| *old == name).map(|(_, new)| new.clone())
                .or_else(|| unit.circuit.net(&name).filter(|net| net.scope.is_some())
                    .and_then(|_| renamed_path(&name, paths))),
        }?;
        Some(TextEdit { doc: doc.id, span: site.span, text: board_doc::string(&new) })
    }).collect()
}

fn with_sheet_edits(mut edits: Vec<TextEdit>, sheet: Vec<TextEdit>) -> Result<Vec<TextEdit>, Diag> {
    // The sheet is a separate statement, so its edits must not overlap the operation's edits.
    for key in &sheet {
        if let Some(other) = edits.iter().find(|other| other.doc == key.doc
            && (key.span.start < other.span.end && other.span.start < key.span.end || key.span == other.span)) {
            return Err(located(key.doc, key.span.start.min(other.span.start)..key.span.end.max(other.span.end),
                "the sheet table and circuit operation need overlapping edits: edit them by hand"));
        }
    }
    edits.extend(sheet);
    Ok(edits)
}

fn edit(doc: fab_elec_core::DocId, span: Range<usize>, text: String) -> TextEdit {
    TextEdit { doc, span: span.start as u32..span.end as u32, text }
}

fn selection_error(map: &SourceMap, what: &Sel, message: impl Into<String>) -> Diag {
    let at = map.locate(what).or_else(|| what.unit().and_then(|unit| map.locate(&Sel::Unit(unit))));
    match at {
        Some(at) => located(at.doc, at.span, message),
        None => Diag::error(message, vec![what.clone()]),
    }
}

pub(crate) fn unit_doc<'a>(sources: &'a Sources, map: &SourceMap, design: &Design, unit: BoardId) -> Result<&'a Doc, Diag> {
    let what = Sel::Unit(unit);
    let u = design.units.iter().find(|u| u.id == unit)
        .ok_or_else(|| selection_error(map, &what, format!("the design has no unit {}", unit.0)))?;
    let name = format!("{}.ckt.splash", u.name);
    sources.find(&name).and_then(|id| sources.doc(id))
        .ok_or_else(|| selection_error(map, &what, format!("the circuit document {name} is not in the project")))
}

fn mapped<'a>(sources: &'a Sources, map: &SourceMap, design: &Design, unit: BoardId, what: &Sel) -> Result<(&'a Doc, SourceRange), Diag> {
    let circuit = &design.units.iter().find(|u| u.id == unit)
        .ok_or_else(|| selection_error(map, what, format!("the design has no unit {}", unit.0)))?.circuit;
    let exists = what.unit() == Some(unit) && match what {
        Sel::Comp(c) => circuit.comps.get(c.comp.ix()).is_some(),
        Sel::Net(n) => circuit.nets.get(n.net.ix()).is_some(),
        Sel::Block(_, b) => circuit.tree.get(b.ix()).is_some(),
        _ => false,
    };
    if !exists {
        return Err(selection_error(map, what, "the selection is not a component, net or block instance in this unit"));
    }
    let at = map.locate(what).ok_or_else(|| selection_error(map, what, "the item has no mapped call"))?;
    if at.doc.0 & LIB_DOC != 0 {
        // Library text is not in `Sources`: the diag's location names the library document.
        return Err(located(at.doc, at.span, "this call is in a library, which is not edited here"));
    }
    let doc = sources.doc(at.doc).ok_or_else(|| located(at.doc, at.span.clone(), "the item's document is not in the project"))?;
    if doc.text.get(at.span.start as usize..at.span.end as usize).is_none() {
        return Err(located(at.doc, at.span, "the mapped call is outside its document"));
    }
    Ok((doc, at))
}

fn parse(doc: &Doc) -> Result<Syntax, Diag> {
    syntax::parse_with(&doc.text, &circuit_units()).map_err(|e| located(doc.id, e.at as u32..e.at as u32, e.message))
}

/// Only block children are statements: an argument is never mistaken for a statement.
pub(crate) fn statement(tree: &Syntax, at: &SourceRange) -> Option<(Loc, usize)> {
    let mut out = None;
    let mut len = usize::MAX;
    tree.walk(&mut |loc, node| {
        if let Kind::Block(block) = &node.kind {
            for (index, stmt) in block.stmts.iter().enumerate() {
                if stmt.span.start <= at.span.start as usize && at.span.end as usize <= stmt.span.end && stmt.span.len() < len {
                    len = stmt.span.len();
                    out = Some((loc.clone(), index));
                }
            }
        }
    });
    out
}

fn statement_call(node: &Node) -> Option<&Node> {
    let value = match &node.kind {
        Kind::Let { value: Some(value), .. } => &**value,
        _ => node,
    };
    matches!(value.kind, Kind::Call { .. }).then_some(value)
}

/// A collected loop's returned call belongs to the binding outside the loop.
fn rename_binding(tree: &Syntax, at: &SourceRange) -> Option<(Loc, usize)> {
    fn value(node: &Node, at: &SourceRange) -> bool {
        match &node.kind {
            Kind::Call { .. } => same_span(node, at),
            Kind::For { body, .. } => body.block().and_then(|b| b.stmts.last()).is_some_and(|n| value(n, at)),
            _ => false,
        }
    }
    let mut out = None;
    let mut len = usize::MAX;
    tree.walk(&mut |loc, node| {
        if let Some(block) = node.block() {
            for (index, stmt) in block.stmts.iter().enumerate() {
                if let Kind::Let { value: Some(v), .. } = &stmt.kind {
                    if stmt.span.len() < len && value(v, at) {
                        len = stmt.span.len();
                        out = Some((loc.clone(), index));
                    }
                }
            }
        }
    });
    out
}

fn same_span(node: &Node, at: &SourceRange) -> bool {
    node.span == (at.span.start as usize..at.span.end as usize)
}

fn newline(source: &str) -> &str {
    if source.contains("\r\n") { "\r\n" } else { "\n" }
}

fn line_indent(source: &str, at: usize) -> &str {
    let start = source[..at].rfind('\n').map_or(0, |i| i + 1);
    let before = &source[start..at];
    &before[..before.len() - before.trim_start_matches([' ', '\t']).len()]
}

/// The first newline outside a block comment in a gap between statements. A line comment
/// ends at that newline; inserting before it would silently comment out the new statement.
fn next_line(gap: &str) -> Option<usize> {
    let bytes = gap.as_bytes();
    let (mut at, mut depth, mut line_comment) = (0, 0usize, false);
    while at < bytes.len() {
        if depth == 0 && bytes[at] == b'\n' {
            return Some(at + 1);
        }
        if !line_comment && bytes[at..].starts_with(b"/*") {
            depth += 1;
            at += 2;
        } else if depth > 0 && bytes[at..].starts_with(b"*/") {
            depth -= 1;
            at += 2;
        } else if depth == 0 && bytes[at..].starts_with(b"//") {
            line_comment = true;
            at += 2;
        } else {
            at += 1;
        }
    }
    None
}

fn removal_span(source: &str, stmt: &Range<usize>) -> Range<usize> {
    let start = source[..stmt.start].rfind('\n').map_or(0, |i| i + 1);
    let end = source[stmt.end..].find('\n').map_or(source.len(), |i| stmt.end + i + 1);
    let tail = &source[stmt.end..end];
    let rest = tail.trim_start_matches([' ', '\t', ';', '\r']);
    if source[start..stmt.start].chars().all(|c| c == ' ' || c == '\t')
        && (rest.is_empty() || rest == "\n" || rest.starts_with("//"))
    {
        return start..end;
    }
    // Other statements or a brace share the line. Own the optional separator, never the
    // neighbouring statement, its indentation, or a potentially multiline comment.
    let trimmed = tail.trim_start_matches([' ', '\t']);
    let end = if trimmed.starts_with(';') { stmt.end + tail.len() - trimmed.len() + 1 } else { stmt.end };
    stmt.start..end
}

pub(crate) fn identifier(name: &str) -> bool {
    let tokens = lex_with(name, &circuit_units());
    tokens.len() == 1 && tokens[0].token().kind() == ScriptTokenTag::Identifier
        && tokens[0].span() == (0..name.len())
        && !matches!(name, "let" | "var" | "mut" | "fn" | "use" | "if" | "else" | "elif" | "for" | "while" | "loop" | "match" | "try" | "ok" | "return" | "break" | "continue" | "true" | "false" | "nil" | "me" | "scope" | "in")
}

fn binds(node: &Node, name: &str) -> bool {
    match &node.kind {
        Kind::Let { names, .. } => names.iter().any(|n| n == name),
        Kind::Fn { name: Some(n), .. } => n == name,
        _ => false,
    }
}

fn bound_in_scope(tree: &Syntax, scope: &Loc, name: &str) -> bool {
    let local = tree.node(scope).and_then(Node::block).is_some_and(|b| b.stmts.iter().any(|n| binds(n, name)));
    local || scope.parent().and_then(|loc| tree.node(&loc)).is_some_and(|owner| match &owner.kind {
        Kind::Fn { name: own, params, .. } => own.as_deref() == Some(name) || params.iter().any(|p| p.name == name),
        Kind::For { binding, .. } => pattern_has(tree.source(), binding, name),
        _ => false,
    })
}

fn pattern_has(source: &str, span: &Range<usize>, name: &str) -> bool {
    let text = &source[span.clone()];
    lex_with(text, &circuit_units()).iter().any(|t| t.token().kind() == ScriptTokenTag::Identifier && &text[t.span()] == name)
}

/// Collect identifier uses, including captures in nested scopes, until a nested binding
/// shadows the old name. A binding of the new name must not capture an edited reference.
fn uses(node: &Node, source: &str, old: &str, to: &str, captured: bool, out: &mut Vec<Range<usize>>) -> Result<(), Range<usize>> {
    match &node.kind {
        Kind::Ident(path) if path.split('.').next() == Some(old) => {
            let span = node.span.start..node.span.start + old.len();
            if captured { return Err(span) }
            out.push(span);
        }
        Kind::Block(block) => {
            if block.stmts.iter().any(|n| binds(n, old)) { return Ok(()) }
            let captured = captured || block.stmts.iter().any(|n| binds(n, to));
            for stmt in &block.stmts { uses(stmt, source, old, to, captured, out)?; }
        }
        Kind::Fn { name, params, body, .. } => {
            if name.as_deref() == Some(old) || params.iter().any(|p| p.name == old) { return Ok(()) }
            let captured = captured || name.as_deref() == Some(to) || params.iter().any(|p| p.name == to);
            uses(body, source, old, to, captured, out)?;
        }
        Kind::For { binding, iter, body } => {
            uses(iter, source, old, to, captured, out)?;
            if !pattern_has(source, binding, old) {
                uses(body, source, old, to, captured || pattern_has(source, binding, to), out)?;
            }
        }
        Kind::Unary { op, .. } if op == "." => {}
        _ => for child in node.children() { uses(child, source, old, to, captured, out)?; },
    }
    Ok(())
}

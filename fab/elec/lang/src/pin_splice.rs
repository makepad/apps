//! Pin connection edits. Resolve a net in the component's block scope and change only the
//! recorded connection site; evaluation supplies the resulting connectivity. Also the other
//! edits of a component's options object: its no-connect list and its own text fields.

use crate::{circuit_splice, series_splice};
use crate::map::{PinSite, SourceMap, SourceRange};
use crate::source::{located, Doc, Sources, LIB_DOC};
use crate::units::circuit_units;
use fab_elec_circuit::{Block, Circuit, Component, RefKind};
use fab_elec_core::{BlockId, BoardId, CompId, CompRef, Diag, DocId, NetId, NetRef, PinIx, Sel};
use fab_elec_design::{Design, TextEdit};
use makepad_script::syntax::{self, Kind, Loc, Node, Syntax};
use makepad_script::tokenizer::{lex_with, ScriptTokenTag};
use std::collections::HashSet;
use std::ops::Range;

/// The component's own Splash statement, with each written pin end replaced by a slot in pin
/// order. Strip the stored reference and addition order so the paste receives its own number.
/// Parses the mapped document once per copy. Separate connection statements stay behind.
pub fn part_template<'a>(
    map: &SourceMap, board: BoardId, part: &Component, text: impl FnOnce(DocId) -> Option<&'a str>,
) -> Result<(String, Vec<PinIx>), Diag> {
    let copy = mapped_template(map, board, part, text)?;
    Ok((copy.render(), copy.pins))
}

/// Both paste statements share the same pin slots. A scope error only prevents detaching.
pub struct PartTemplates {
    pub stmt: String,
    pub pins: Vec<PinIx>,
    pub detached: Result<String, String>,
}

/// Copy both forms from one parse. Only reads retained in the detached statement need a
/// scope lookup, and only a read of the build's second argument needs an instance value.
pub fn part_templates<'a>(
    map: &SourceMap, circuit: &Circuit, part: &Component, text: impl FnOnce(DocId) -> Option<&'a str>,
) -> Result<PartTemplates, Diag> {
    let mut copy = mapped_template(map, circuit.unit, part, text)?;
    let stmt = copy.render();
    let detached = copy.detached_edits(&circuit.tree[part.block.ix()], part).map(|literals| {
        if literals.is_empty() { return stmt.clone(); }
        copy.edits.extend(literals);
        copy.edits.sort_by_key(|(span, _)| span.start);
        copy.render()
    });
    Ok(PartTemplates { stmt, pins: copy.pins, detached })
}

struct MappedTemplate {
    tree: Syntax,
    stmt: Range<usize>,
    pins: Vec<PinIx>,
    edits: Vec<(Range<usize>, String)>,
}

fn mapped_template<'a>(
    map: &SourceMap, board: BoardId, part: &Component, text: impl FnOnce(DocId) -> Option<&'a str>,
) -> Result<MappedTemplate, Diag> {
    let comp = CompRef { board, comp: part.id };
    let at = map.locate(&Sel::Comp(comp)).ok_or_else(||
        Diag::error("the component has no mapped call", vec![Sel::Comp(comp)]))?;
    let refuse = |message| located(at.doc, at.span.clone(), format!("{message}: copy it by hand"));
    let source = text(at.doc).ok_or_else(|| refuse("the part's source is outside the project"))?;
    let tree = syntax::parse_with(source, &circuit_units())
        .map_err(|e| located(at.doc, e.at as u32..e.at as u32, e.message))?;
    let stmt = statement(&tree, &at).ok_or_else(|| refuse("the component's call has no enclosing statement"))?;
    let call = match &stmt.kind {
        Kind::Let { value: Some(call), .. } => call.as_ref(),
        _ => stmt,
    };
    if !same_span(call, &at) || !matches!(call.kind, Kind::Call { .. }) {
        return Err(refuse("the component's call is inside another expression"));
    }
    if check_shared(map, &at).is_err() {
        return Err(refuse("this call makes several components"));
    }
    let mut pins = Vec::new();
    let mut edits = Vec::new();
    let stored = part.ref_kind == RefKind::Stored;
    let added = map.added.get(&board).is_some_and(|orders| orders.binary_search_by_key(&part.id, |(id, _)| *id).is_ok());
    let opts = options_position(maker_name(call), map, comp)
        .and_then(|index| crate::circuit::comp_options_arg(call.as_call()?.1, index))
        .map(|(_, opts)| opts);
    if stored && opts.and_then(|o| o.field("ref")).is_none() {
        return Err(refuse(if opts.is_some_and(|o| o.object().is_some()) { "the stored reference is not written in this options object" }
            else { "the stored reference is outside a literal options object" }));
    }
    if added && opts.and_then(|o| o.field("added")).is_none() {
        return Err(refuse("the addition order is not written in a literal options object"));
    }
    if let Some(Kind::Object { fields, open, close, .. }) = opts.map(|o| &o.kind) {
        let spans: Vec<_> = fields.iter().map(|f| f.span.clone()).collect();
        let mut removed = fields.iter().enumerate()
            .filter_map(|(i, f)| matches!(f.name.as_deref(), Some("ref" | "added")).then_some(i)).peekable();
        while let Some(first) = removed.next() {
            let mut last = first;
            while removed.next_if_eq(&(last + 1)).is_some() { last += 1; }
            edits.push(entry_removal(source, *open, *close, &spans, first, last));
        }
    }
    for i in 0..part.part.pins.len() {
        let pin = PinIx(i as u16);
        for site in map.pin_sites(&comp, pin) {
            let Some(site_at) = site_call(site) else { continue };
            if site_at.doc != at.doc || site_at.span.start < at.span.start || site_at.span.end > at.span.end { continue; }
            let site_node = if *site_at == at { call } else {
                call_at(&tree, site_at).ok_or_else(|| refuse("the pin's mapped call is missing"))?
            };
            let Some(value) = value_in_call(site_node, site) else {
                // A passive records both positional sites even when its optional ends are absent.
                if matches!(site, PinSite::Item { .. }) && matches!(maker_name(site_node), Some("r" | "c" | "l")) { continue; }
                return Err(refuse("the pin's connection is not written in the part's call"));
            };
            // A written nil did not make this connection. The pin was tied elsewhere.
            if value.ident() == Some("nil") && part.conns.iter().any(|(p, _)| *p == pin) { continue; }
            edits.push((value.span.clone(), format!("_n{}", pins.len())));
            pins.push(pin);
        }
    }
    edits.sort_by_key(|(span, _)| span.start);
    // AddPart substitutes every _nK identifier. A name in the part's value or binding must
    // never be mistaken for an extra end. Names inside a replaced expression disappear.
    let written = &source[stmt.span.clone()];
    if pin_slot_names(written).any(|offset| !edits.iter().any(|(range, _)| range.contains(&(stmt.span.start + offset)))) {
        return Err(refuse("the statement already uses a pin slot name"));
    }
    if edits.windows(2).any(|pair| pair[0].0.end > pair[1].0.start) {
        return Err(refuse("the template edits overlap"));
    }
    Ok(MappedTemplate { stmt: stmt.span.clone(), tree, pins, edits })
}

fn pin_slot_names(text: &str) -> impl Iterator<Item = usize> + '_ {
    let tokens = if text.contains("_n") { lex_with(text, &circuit_units()) } else { Vec::new() };
    tokens.into_iter().filter_map(move |token| {
        if token.token().kind() != ScriptTokenTag::Identifier { return None; }
        let span = token.span();
        let suffix = text[span.clone()].strip_prefix("_n")?;
        (!suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())).then_some(span.start)
    })
}

impl MappedTemplate {
    fn render(&self) -> String {
        let mut template = String::new();
        let mut start = self.stmt.start;
        for (span, replacement) in &self.edits {
            template.push_str(&self.tree.source()[start..span.start]);
            template.push_str(replacement);
            start = span.end;
        }
        template.push_str(&self.tree.source()[start..self.stmt.end]);
        template
    }

    fn detached_edits(&self, block: &Block, part: &Component) -> Result<Vec<(Range<usize>, String)>, String> {
        let path = crate::resolve::ancestors(&self.tree, &self.stmt);
        if path.len() == 2 { return Ok(Vec::new()); } // A statement directly in the document.
        let stmt = path.last().expect("the mapped statement belongs to the tree");
        let build = path.iter().enumerate().rev().find_map(|(i, call)| {
            let (callee, args) = call.as_call()?;
            if callee.rsplit('.').next() != Some("block") { return None; }
            let build = &args.first()?.field("build")?.value;
            let Kind::Fn { params, body, .. } = &build.kind else { return None };
            if body.span.start > self.stmt.start || body.span.end < self.stmt.end { return None; }
            let name = i.checked_sub(1).and_then(|i| path[i].let_name()).unwrap_or(&block.path);
            Some((build, params.get(1), name))
        });
        let scope_name = build.map_or(block.path.as_str(), |(_, _, name)| name);
        let scope_name = if scope_name.is_empty() { "the source function" } else { scope_name };
        let params = build.and_then(|(build, param, _)| param.map(|param| (build, param.name.as_str())));
        let mut literals = Vec::new();
        let mut pending = vec![*stmt];
        while let Some(node) = pending.pop() {
            let index = self.edits.partition_point(|(span, _)| span.end <= node.span.start);
            if self.edits.get(index).is_some_and(|(span, _)| node.span.end <= span.end && span.start <= node.span.start) { continue; }
            if matches!(&node.kind, Kind::Unary { op, .. } if op == ".") { continue; }
            pending.extend(node.children().into_iter().rev());
            let path = match &node.kind {
                Kind::Ident(path) | Kind::Object { ty: Some(path), .. } => path,
                _ => continue,
            };
            let root = path.split_once('.').map_or(path.as_str(), |(root, _)| root);
            let owners = crate::resolve::ancestors(&self.tree, &node.span);
            let Some(binding) = template_binding(&self.tree, &owners, node, root) else { continue };
            // A declaration inside the copied statement travels with its uses.
            if self.stmt.start <= binding.span.start && binding.span.end <= self.stmt.end { continue; }
            let refuse = |used: &str| format!("{} uses {used}, which exists only inside {scope_name}: paste it there", part.refdes);
            if !params.is_some_and(|(build, param)| binding.span == build.span && param == root) {
                return Err(refuse(root));
            }
            let (field, span) = parameter_field(path, node, &owners).ok_or_else(|| refuse(root))?;
            let used = &self.tree.source()[span.clone()];
            if owners.iter().any(|n| matches!(&n.kind, Kind::Assign { target, .. } if target.span.start == span.start)) {
                return Err(refuse(used));
            }
            let mut literal = block.params.iter().find(|(name, _)| name == field)
                .and_then(|(_, value)| crate::splice::param_text(value).ok()).ok_or_else(|| refuse(used))?;
            if pin_slot_names(&literal).next().is_some() { return Err(refuse(used)); }
            // Keep a following member out of an @symbol and two adjacent minus signs apart.
            if (literal.starts_with('@') && self.tree.source()[span.end..].starts_with('.'))
                || (literal.starts_with('-') && self.tree.source()[..span.start].ends_with('-')) {
                literal = format!("({literal})");
            }
            literals.push((span, literal));
        }
        Ok(literals)
    }
}

/// The first field read from the params object. Dotted paths are one syntax node; a
/// parenthesized receiver or a literal subscript owns its field in the parent node.
fn parameter_field<'a>(path: &'a str, node: &Node, owners: &[&'a Node]) -> Option<(&'a str, Range<usize>)> {
    if let Some((root, tail)) = path.split_once('.') {
        let field = tail.split_once('.').map_or(tail, |(field, _)| field);
        return Some((field, node.span.start..node.span.start + root.len() + 1 + field.len()));
    }
    let mut receiver = &node.span;
    for parent in owners.iter().rev().skip(1) {
        match &parent.kind {
            Kind::Paren { items, .. } if items.len() == 1 => receiver = &parent.span,
            Kind::Member { base, name, .. } if base.span == *receiver => return Some((name, parent.span.clone())),
            Kind::Index { base, index, .. } if base.span == *receiver =>
                return Some((index.name().or_else(|| index.string())?, parent.span.clone())),
            _ => break,
        }
    }
    None
}

/// Resolve a read to a declaration inside an enclosing function or loop. Field keys,
/// selectors and document-level bindings do not capture a block-local name.
fn template_binding<'a>(tree: &Syntax, owners: &[&'a Node], read: &Node, name: &str) -> Option<&'a Node> {
    for &owner in owners.iter().rev() {
        match &owner.kind {
            Kind::Block(block) if block.open.is_some() => {
                let before = block.stmts.partition_point(|stmt| stmt.span.end <= read.span.start);
                if let Some(stmt) = block.stmts[..before].iter().rev().find(|s| declares_local(s, name)) {
                    return Some(stmt);
                }
            }
            Kind::Fn { name: own, params, .. } if own.as_deref() == Some(name)
                || params.iter().any(|param| param.name == name) => return Some(owner),
            Kind::For { binding, body, .. } if body.span.start <= read.span.start
                && tree.source()[binding.clone()].split(|c: char| !c.is_alphanumeric() && c != '_').any(|s| s == name)
                => return Some(owner),
            _ => {}
        }
    }
    None
}

// Conditional branches share their surrounding VM scope. A completed loop or function
// keeps its own declarations, even if its body used the same name.
fn declares_local(node: &Node, name: &str) -> bool {
    match &node.kind {
        Kind::Block(block) => block.stmts.iter().any(|stmt| declares_local(stmt, name)),
        Kind::If { .. } | Kind::Match { .. } | Kind::Try { .. } | Kind::Ok { .. } =>
            node.children().iter().any(|child| declares_local(child, name)),
        Kind::Use { path } => path.ident().and_then(|path| path.rsplit('.').next()) == Some(name),
        _ => series_splice::binds(node, name),
    }
}

pub(crate) fn connect(
    sources: &Sources,
    map: &SourceMap,
    design: &Design,
    unit: BoardId,
    comp: CompId,
    pin: PinIx,
    net: NetId,
) -> Result<Vec<TextEdit>, Diag> {
    let (at, circuit, component) = mapped(sources, map, design, unit, comp, pin)?;
    connect_sites(sources, map, unit, component, pin, &at)?;
    let tree = parse(document(sources, &at)?)?;
    let expr = match net_expr(&tree, map, circuit, component.block, &at, net) {
        Ok(expr) => expr,
        // A lone pin's net written in place (a placed part's open end, `e.net()`) has no name to
        // reach: one new net binds both pins, the lone pin's entry rewritten to it.
        Err(error) => return match circuit.pins_of(net).as_slice() {
            [lone] if *lone != (comp, pin) && net_in_place(sources, map, unit, *lone) =>
                bind_free_pins(sources, map, design, unit, &[vec![*lone, (comp, pin)]], None).map(|(_, edits)| edits),
            _ => Err(error),
        },
    };
    connect_expr(sources, map, design, unit, component, pin, &at, &tree, &expr)
}

/// Whether `pin`'s one connection is a net written in place: `e.net(..)` as its argument or entry.
fn net_in_place(sources: &Sources, map: &SourceMap, unit: BoardId, (comp, pin): (CompId, PinIx)) -> bool {
    let [site] = map.pin_sites(&CompRef { board: unit, comp }, pin) else { return false };
    let Some(call) = site_call(site) else { return false };
    let Ok(tree) = document(sources, call).and_then(parse) else { return false };
    site_value(&tree, site).is_some_and(|value| value.call("e.net").is_some())
}

/// Find a net by its exact name. A new name adds a declaration before the pin's first use.
/// Cost per edit is a net scan and the source walks for binding and rewriting the pin.
pub(crate) fn connect_named(
    sources: &Sources, map: &SourceMap, design: &Design, unit: BoardId,
    comp: CompId, pin: PinIx, name: &str,
) -> Result<Vec<TextEdit>, Diag> {
    if let Some(net) = design.unit(unit).and_then(|u| u.circuit.net(name)) {
        return connect(sources, map, design, unit, comp, pin, net.id);
    }
    let (at, circuit, component) = mapped(sources, map, design, unit, comp, pin)?;
    let sites = connect_sites(sources, map, unit, component, pin, &at)?;
    let site = sites.first();
    let use_at = site.and_then(site_call).unwrap_or(&at);
    let tree = parse(document(sources, use_at)?)?;
    let (scope, index) = circuit_splice::statement(&tree, use_at)
        .ok_or_else(|| located(use_at.doc, use_at.span.clone(), "the pin's connection has no enclosing statement"))?;
    let block = tree.node(&scope).and_then(Node::block).expect("the statement belongs to a block");
    // Start at the part as ConnectPins does. A separate connection can be in another scope:
    // keep the declaration in the block that will actually read the binding.
    let anchor = if at.doc == use_at.doc && at.span.start < use_at.span.start {
        circuit_splice::statement(&tree, &at).filter(|(part_scope, _)| part_scope == &scope)
            .map_or(index, |(_, part_index)| part_index)
    } else { index };
    // The prefix also makes labels such as "if" and "3V3" safe Splash bindings.
    let stem = series_splice::binding_name(name);
    let base = format!("net_{}", stem.trim_start_matches('_'));
    let binding = unique_binding(&base, |s| series_splice::binding_taken(&tree, &scope, circuit, component.block, s));
    let declaration = format!("let {binding} = e.net({{name: {}}})", syntax::write::quote(name));
    let mut edits = vec![series_splice::insert_before(&tree, use_at, &block.stmts[anchor], &declaration, "").0];
    if let Some(site) = site {
        edits.push(rewrite_site(&tree, site, &binding)?);
    } else {
        edits.extend(connect_expr(sources, map, design, unit, component, pin, &at, &tree, &binding)?);
    }
    Ok(edits)
}

fn unique_binding(base: &str, taken: impl Fn(&str) -> bool) -> String {
    let mut binding = base.to_string();
    let mut suffix = 2;
    // Keep a trailing digit distinct from the suffix, as in r_x_2_2.
    let separator = if base.ends_with(|ch: char| ch.is_ascii_digit()) { "_" } else { "" };
    while taken(&binding) {
        binding = format!("{base}{separator}{suffix}");
        suffix += 1;
    }
    binding
}

fn connect_sites<'a>(sources: &Sources, map: &'a SourceMap, unit: BoardId, component: &Component, pin: PinIx, at: &SourceRange) -> Result<&'a [PinSite], Diag> {
    let comp = component.id;
    let sites = map.pin_sites(&CompRef { board: unit, comp }, pin);
    check_sites(sites, at)?;
    let name = &component.part.pins[pin.ix()].name;
    if sites.len() > 1 {
        let places: Vec<_> = sites.iter().map(|site| site_label(sources, site)).collect();
        let call = site_call(&sites[0]).expect("library sites were refused");
        return Err(located(call.doc, call.span.clone(), format!(
            "{} pin {name} is connected in {} places: {}: edit them by hand",
            component.path, sites.len(), places.join("; "),
        )));
    }
    Ok(sites)
}

fn connect_expr(
    sources: &Sources, map: &SourceMap, design: &Design, unit: BoardId,
    component: &Component, pin: PinIx, at: &SourceRange, tree: &Syntax, expr: &str,
) -> Result<Vec<TextEdit>, Diag> {
    let sites = connect_sites(sources, map, unit, component, pin, at)?;
    let name = &component.part.pins[pin.ix()].name;
    if let Some(site) = sites.first() {
        let call = site_call(site).expect("library sites were refused");
        let other_tree;
        let site_tree = if call.doc == at.doc { tree } else {
            other_tree = parse(document(sources, call)?)?;
            &other_tree
        };
        let mut edits = vec![rewrite_site(site_tree, site, expr)?];
        edits.extend(unmark(tree, map, unit, at, component, pin)?);
        return Ok(edits);
    }

    // An absent site must not turn one instance's new connection into a change to every
    // component made by the same call.
    check_shared(map, at)?;
    let call = call_at(tree, at).ok_or_else(|| located(at.doc, at.span.clone(), "the component's mapped call is missing"))?;
    if let Some(opts) = call.call("e.part").and_then(|args| args.get(1)).filter(|o| o.object().is_some()) {
        return part_pin_edits(tree, at, component, opts, &[(pin, expr)]);
    }
    let binding = statement(tree, at).and_then(|stmt| {
        let Kind::Let { pattern_span, value: Some(value), .. } = &stmt.kind else { return None };
        let binding = stmt.let_name()?;
        (same_span(value, at) && tree.source()[pattern_span.clone()].trim() == binding).then_some(binding)
    }).ok_or_else(|| located(at.doc, at.span.clone(), "bind the component to a name first"))?;
    let mut edits = circuit_splice::insert_statement(sources, map, design, unit, &format!("e.connect({binding}.{name}, {expr})"), Some(component.id))?;
    edits.extend(unmark(tree, map, unit, at, component, pin)?);
    Ok(edits)
}

pub(crate) fn connect_pins(
    sources: &Sources, map: &SourceMap, design: &Design, unit: BoardId,
    a: (CompId, PinIx), b: (CompId, PinIx),
) -> Result<Vec<TextEdit>, Diag> {
    let (at, circuit, first) = mapped(sources, map, design, unit, a.0, a.1)?;
    if a == b { return Err(located(at.doc, at.span.clone(), "a pin cannot be wired to itself")); }
    // Check a before resolving b so a's existing net is always the first refusal.
    let on_net = |at: &SourceRange, component: &Component, pin: PinIx| {
        circuit.net_of(component.id, pin).map(|net| located(at.doc, at.span.clone(), format!(
            "{} pin {} is on net {}: use Connect", component.path, component.part.pins[pin.ix()].name, circuit.nets[net.ix()].name,
        )))
    };
    if let Some(error) = on_net(&at, first, a.1) { return Err(error); }
    let (bt, _, second) = mapped(sources, map, design, unit, b.0, b.1)?;
    if let Some(error) = on_net(&bt, second, b.1) { return Err(error); }
    bind_free_pins(sources, map, design, unit, &[vec![a, b]], None).map(|(_, edits)| edits)
}

/// Make one net per group of free pins. An insertion also needs each binding in its lexical
/// scope; connections to the same part are collected before editing its pins and nc objects.
pub(crate) fn bind_free_pins(
    sources: &Sources, map: &SourceMap, design: &Design, unit: BoardId,
    groups: &[Vec<(CompId, PinIx)>], insertion: Option<(BlockId, &SourceRange, &Loc)>,
) -> Result<(Vec<String>, Vec<TextEdit>), Diag> {
    let mut bindings = Vec::new();
    let mut edits = Vec::new();
    let mut connections: Vec<(SourceRange, &Component, Vec<(PinIx, String)>)> = Vec::new();
    for pins in groups {
        let a = pins[0];
        let (at, circuit, first) = mapped(sources, map, design, unit, a.0, a.1)?;
        let second = pins.get(1).map(|b| mapped(sources, map, design, unit, b.0, b.1)).transpose()?;
        let no_scope = || located(at.doc, at.span.clone(), format!(
            "no scope names both pins: {} pin {} and {} (give the block a port)",
            first.path, first.part.pins[a.1.ix()].name,
            second.as_ref().map_or_else(|| "the new part".to_string(), |(_, _, component)|
                format!("{} pin {}", component.path, component.part.pins[pins[1].1.ix()].name)),
        ));
        let (other_block, bt) = second.as_ref().map(|(bt, _, component)| (component.block, bt))
            .or_else(|| insertion.map(|(block, at, _)| (block, at))).ok_or_else(no_scope)?;
        let library = |at: &SourceRange| at.doc.0 & LIB_DOC != 0 || sources.doc(at.doc).is_some_and(|doc| doc.lib.is_some());
        if first.block != other_block || at.doc != bt.doc || library(&at) || library(bt) {
            return Err(no_scope());
        }
        let tree = parse(document(sources, &at)?)?;
        let (mut scope, _) = circuit_splice::statement(&tree, &at).ok_or_else(no_scope)?;
        let other_scope = if let Some((_, _, scope)) = insertion { scope.clone() } else {
            circuit_splice::statement(&tree, bt).ok_or_else(no_scope)?.0
        };
        // Climb syntax ancestors, skipping expressions, to the smallest block holding both.
        let block = loop {
            let node = tree.node(&scope).expect("the statement's ancestor exists");
            let mut other = Some(other_scope.clone());
            while other.as_ref().is_some_and(|other| *other != scope) {
                other = other.and_then(|other| other.parent());
            }
            if let Some(block) = node.block().filter(|_| other.is_some()) { break block; }
            scope = scope.parent().ok_or_else(no_scope)?;
        };
        let comp_name = series_splice::binding_name(first.path.rsplit('.').next().unwrap_or(&first.path));
        let pin_name = series_splice::binding_name(&first.part.pins[a.1.ix()].name);
        let base = format!("{}_{}", comp_name.trim_start_matches('_'), pin_name.trim_start_matches('_'));
        let base = base.trim_start_matches('_');
        let binding = unique_binding(base, |s| bindings.iter().any(|b| b == s)
            || series_splice::binding_taken(&tree, &scope, circuit, first.block, s));
        let earlier = if at.span.start <= bt.span.start { &at } else { bt };
        let anchor = if insertion.is_some() {
            // In a gap, anchor before the preceding statement: the following statement's
            // line may start after the insertion point, leaving the new binding too late.
            block.stmts.iter().rev().find(|stmt| stmt.span.start <= earlier.span.start as usize)
        } else {
            block.stmts.iter().find(|stmt| stmt.span.start <= earlier.span.start as usize && earlier.span.end as usize <= stmt.span.end)
        }.ok_or_else(no_scope)?;
        edits.push(series_splice::insert_before(&tree, &at, anchor, &format!("let {binding} = e.net()"), "").0);
        for &(comp, pin) in pins {
            let (at, _, component) = mapped(sources, map, design, unit, comp, pin)?;
            let sites = connect_sites(sources, map, unit, component, pin, &at)?;
            let existing = connections.iter_mut().find(|(_, other, pins)| other.id == comp && sites.is_empty()
                && pins.iter().all(|(pin, _)| map.pin_sites(&CompRef { board: unit, comp }, *pin).is_empty()));
            if let Some((_, _, pins)) = existing {
                pins.push((pin, binding.clone()));
            } else {
                connections.push((at, component, vec![(pin, binding.clone())]));
            }
        }
        bindings.push(binding);
    }
    for (at, component, pins) in &connections {
        let tree = parse(document(sources, at)?)?;
        let call = call_at(&tree, at).ok_or_else(|| located(at.doc, at.span.clone(), "the component's mapped call is missing"))?;
        if let Some(args) = call.call("e.part").filter(|_| pins.iter().all(|(pin, _)| map.pin_sites(&CompRef { board: unit, comp: component.id }, *pin).is_empty())) {
            check_shared(map, at)?;
            let entries: Vec<_> = pins.iter().map(|(pin, expr)| (*pin, expr.as_str())).collect();
            if let Some(opts) = args.get(1).filter(|opts| opts.object().is_some()) {
                edits.extend(part_pin_edits(&tree, at, component, opts, &entries)?);
                continue;
            }
            if args.len() == 1 {
                let Kind::Call { close, .. } = &call.kind else { unreachable!() };
                let entries = pin_entries(component, &entries, at)?;
                edits.push(edit(at.doc, *close..*close, format!(", {{pins: {{{}}}}}", entries.join(" "))));
                continue;
            }
        }
        for (pin, expr) in pins {
            edits.extend(connect_expr(sources, map, design, unit, component, *pin, at, &tree, expr)?);
        }
    }
    let at = &connections.first().expect("each binding connects a pin").0;
    // Two generated pins of one component can append at the same statement boundary.
    edits.sort_by_key(|edit| (edit.doc, edit.span.start, edit.span.end));
    let mut merged: Vec<TextEdit> = Vec::new();
    for edit in edits {
        if let Some(previous) = merged.last_mut().filter(|previous| previous.doc == edit.doc) {
            if previous.span == edit.span && edit.span.is_empty() {
                previous.text.push_str(&edit.text);
                continue;
            }
            if previous.span.end > edit.span.start {
                return Err(located(at.doc, at.span.clone(), "the pins need overlapping text edits: edit them by hand"));
            }
        }
        merged.push(edit);
    }
    Ok((bindings, merged))
}

/// Mark (`nc`) or clear a pin's no-connect in the options of the call that makes its component:
/// `e.part`, `e.r`, `e.c` or `e.led`. `freed`: an earlier op of this commit disconnects the pin
/// (the plan marks a loose pin, an open end on its own net, after disconnecting it).
pub(crate) fn no_connect(
    sources: &Sources, map: &SourceMap, design: &Design, unit: BoardId,
    comp: CompId, pin: PinIx, nc: bool, freed: bool,
) -> Result<Vec<TextEdit>, Diag> {
    let (at, circuit, component) = mapped(sources, map, design, unit, comp, pin)?;
    let tree = parse(document(sources, &at)?)?;
    let fail = |message| located(at.doc, at.span.clone(), message);
    let call = call_at(&tree, &at).ok_or_else(|| fail("the component's mapped call is missing".to_string()))?;
    let name = &component.part.pins[pin.ix()].name;
    let label = format!("{} pin {name}", component.path);
    let Kind::Call { args, close, .. } = &call.kind else { unreachable!("call_at finds only calls") };
    let index = options_position(maker_name(call), map, CompRef { board: unit, comp })
        .ok_or_else(|| fail(format!("{label} belongs to a generated part: only e.part, e.r, e.c and e.led pins can be no-connect")))?;
    check_sites(map.pin_sites(&CompRef { board: unit, comp }, pin), &at).map_err(|error| fail(error.message))?;
    check_shared(map, &at)?;
    if nc && !freed && circuit.net_of(comp, pin).is_some() {
        return Err(fail(format!("{label} is connected: disconnect it first")));
    }
    if component.nc.contains(&pin) == nc {
        return Err(fail(format!("{label} is {}no-connect", if nc { "already " } else { "not " })));
    }
    let literal_list = || fail("the part's nc list is not a literal list: edit it by hand".to_string());
    let opts = crate::circuit::comp_options_arg(args, index).map(|(_, opts)| opts);
    if !nc {
        let list = opts.and_then(|opts| opts.field("nc")).ok_or_else(|| fail("removing a pin from nc needs a literal list".to_string()))?;
        return remove_nc(&tree, &at, &list.value, &[name.as_str()]);
    }
    let entry = if circuit_splice::identifier(name) { format!("@{name}") } else {
        syntax::write::write(&syntax::write::Value::Str(name.clone())).map_err(fail)?
    };
    let (span, text) = if let Some(opts) = opts {
        if let Some(list) = opts.field("nc") {
            let Kind::Array { items, open, close } = &list.value.kind else { return Err(literal_list()); };
            let pos = items.last().map_or(open + 1, |item| item.span.end);
            let commas = comma(&tree.source()[pos..*close]).is_some()
                || items.windows(2).any(|pair| comma(&tree.source()[pair[0].span.end..pair[1].span.start]).is_some());
            let separator = if items.is_empty() { "" } else if commas { ", " } else { " " };
            (pos..pos, format!("{separator}{entry}"))
        } else {
            object_edit(&tree, opts, "nc", &format!("[{entry}]")).ok_or_else(literal_list)?
        }
    } else if args.len() == index {
        (*close..*close, format!(", {{nc: [{entry}]}}"))
    } else {
        // A passive may omit its ends. Naming the options keeps the missing ends open.
        let key = if index == 4 { "rest" } else { "opts" };
        (*close..*close, format!(", {key}: {{nc: [{entry}]}}"))
    };
    Ok(vec![edit(at.doc, span, text)])
}

/// A connection written to a no-connect pin drops the pin from its call's `nc: [..]`.
fn unmark(tree: &Syntax, map: &SourceMap, unit: BoardId, at: &SourceRange, component: &Component, pin: PinIx) -> Result<Vec<TextEdit>, Diag> {
    if !component.nc.contains(&pin) { return Ok(vec![]); }
    let Some(call) = call_at(tree, at) else { return Ok(vec![]) };
    let Kind::Call { args, .. } = &call.kind else { return Ok(vec![]) };
    let Some(index) = options_position(maker_name(call), map, CompRef { board: unit, comp: component.id }) else { return Ok(vec![]) };
    let Some(list) = crate::circuit::comp_options_arg(args, index).and_then(|(_, opts)| opts.field("nc")) else { return Ok(vec![]) };
    remove_nc(tree, at, &list.value, &[component.part.pins[pin.ix()].name.as_str()])
}

/// Write (`Some`) or remove (`None`) the component's own text field `key` in the options of the
/// call that makes it: the `fields: {key: "text"}` of `e.part`, `e.r`, `e.c` or `e.led`. Missing
/// options and a missing `fields` object are added. Removing the last field removes `fields`, and
/// options left empty go too when they are the call's last argument (`e.led` keeps its own).
pub(crate) fn set_field(
    sources: &Sources, map: &SourceMap, design: &Design, unit: BoardId,
    comp: CompId, key: &str, text: Option<&str>,
) -> Result<Vec<TextEdit>, Diag> {
    let (at, _, component) = mapped_comp(sources, map, design, unit, comp)?;
    let tree = parse(document(sources, &at)?)?;
    let fail = |message: String| located(at.doc, at.span.clone(), message);
    let call = call_at(&tree, &at).ok_or_else(|| fail("the component's mapped call is missing".to_string()))?;
    check_shared(map, &at)?;
    let label = format!("{} field {key}", component.path);
    let Kind::Call { args, open, close, .. } = &call.kind else { unreachable!("call_at finds only calls") };
    let maker = maker_name(call);
    let index = options_position(maker, map, CompRef { board: unit, comp })
        .ok_or_else(|| fail(format!("{label}: only e.part, e.r, e.c and e.led calls write a component's fields")))?;
    let value = text.map(|text| syntax::write::write(&syntax::write::Value::Str(text.to_string()))).transpose().map_err(fail)?;
    let entry = |value: &str| syntax::write::key(key).map(|key| format!("{key}: {value}")).map_err(fail);
    let unset = || fail(format!("{label} is not set"));
    let literal = || fail(format!("{label}: the options are not a literal object: edit them by hand"));
    let source = tree.source();
    let Some(opts) = args.get(index) else {
        let value = value.ok_or_else(unset)?;
        if args.len() != index {
            return Err(fail(format!("{label}: the call has fewer arguments than its options need")));
        }
        return Ok(vec![edit(at.doc, *close..*close, format!(", {{fields: {{{}}}}}", entry(&value)?))]);
    };
    let Kind::Object { fields: options, open: options_open, close: options_close, .. } = &opts.kind else { return Err(literal()) };
    let Some(fields) = opts.field("fields") else {
        let value = value.ok_or_else(unset)?;
        let (span, text) = object_edit(&tree, opts, "fields", &format!("{{{}}}", entry(&value)?)).ok_or_else(literal)?;
        return Ok(vec![edit(at.doc, span, text)]);
    };
    let Kind::Object { fields: entries, open: fields_open, close: fields_close, .. } = &fields.value.kind else { return Err(literal()) };
    let Some(value) = value else {
        let i = entries.iter().position(|f| f.name.as_deref() == Some(key)).ok_or_else(unset)?;
        let removal = |open: usize, close: usize, spans: Vec<Range<usize>>, i: usize| {
            let (span, text) = entry_removal(source, open, close, &spans, i, i);
            vec![edit(at.doc, span, text)]
        };
        if entries.len() > 1 {
            return Ok(removal(*fields_open, *fields_close, entries.iter().map(|f| f.span.clone()).collect(), i));
        }
        if options.len() == 1 && maker != Some("led") && index + 1 == args.len() {
            return Ok(removal(*open, *close, args.iter().map(|a| a.span.clone()).collect(), index));
        }
        let j = options.iter().position(|f| f.name.as_deref() == Some("fields")).expect("the fields entry was found above");
        return Ok(removal(*options_open, *options_close, options.iter().map(|f| f.span.clone()).collect(), j));
    };
    let (span, text) = object_edit(&tree, &fields.value, key, &value).ok_or_else(literal)?;
    Ok(vec![edit(at.doc, span, text)])
}

fn pin_entries(component: &Component, pins: &[(PinIx, &str)], at: &SourceRange) -> Result<Vec<String>, Diag> {
    pins.iter().map(|(pin, expr)| {
        let key = syntax::write::key(&component.part.pins[pin.ix()].name).map_err(|message| located(at.doc, at.span.clone(), message))?;
        Ok(format!("{key}: {expr}"))
    }).collect()
}

fn part_pin_edits(tree: &Syntax, at: &SourceRange, component: &Component, opts: &Node, pins: &[(PinIx, &str)]) -> Result<Vec<TextEdit>, Diag> {
    let fail = || located(at.doc, at.span.clone(), "adding a pin needs a pins object: pins is not an object");
    let (span, text) = match opts.field("pins") {
        Some(object) if pins.len() == 1 => object_edit(tree, &object.value, &component.part.pins[pins[0].0.ix()].name, pins[0].1).ok_or_else(fail)?,
        Some(object) => object_append(tree, &object.value, &pin_entries(component, pins, at)?).ok_or_else(fail)?,
        None => object_edit(tree, opts, "pins", &format!("{{{}}}", pin_entries(component, pins, at)?.join(" "))).ok_or_else(fail)?,
    };
    let mut edits = vec![edit(at.doc, span, text)];
    let names: Vec<_> = pins.iter().filter(|(pin, _)| component.nc.contains(pin)).map(|(pin, _)| component.part.pins[pin.ix()].name.as_str()).collect();
    if let Some(nc) = opts.field("nc").filter(|_| !names.is_empty()) {
        edits.extend(remove_nc(tree, at, &nc.value, &names)?);
    }
    Ok(edits)
}

fn remove_nc(tree: &Syntax, at: &SourceRange, list: &Node, names: &[&str]) -> Result<Vec<TextEdit>, Diag> {
    let Kind::Array { items, open, close } = &list.kind else {
        return Err(located(at.doc, at.span.clone(), "removing a pin from nc needs a literal list"));
    };
    let name = |item: &Node| item.name().or_else(|| item.string()).is_some_and(|name| names.contains(&name));
    if names.iter().any(|name| !items.iter().any(|item| item.name().or_else(|| item.string()) == Some(*name))) {
        return Err(located(at.doc, at.span.clone(), "the pin's nc entry is not a literal name: remove it by hand"));
    }
    let spans: Vec<_> = items.iter().map(|item| item.span.clone()).collect();
    let mut edits = Vec::new();
    // Remove runs together so repeated entries and adjacent selected pins cannot overlap.
    let mut i = 0;
    while i < items.len() {
        if !name(&items[i]) { i += 1; continue; }
        let first = i;
        while i + 1 < items.len() && name(&items[i + 1]) { i += 1; }
        let (span, text) = entry_removal(tree.source(), *open, *close, &spans, first, i);
        edits.push(edit(at.doc, span, text));
        i += 1;
    }
    Ok(edits)
}

pub(crate) fn disconnect(
    sources: &Sources,
    map: &SourceMap,
    design: &Design,
    unit: BoardId,
    comp: CompId,
    pin: PinIx,
) -> Result<Vec<TextEdit>, Diag> {
    let (at, _, component) = mapped(sources, map, design, unit, comp, pin)?;
    let sites = map.pin_sites(&CompRef { board: unit, comp }, pin);
    if sites.is_empty() {
        return Err(located(at.doc, at.span.clone(), format!(
            "{} pin {} is not connected in the text", component.path, component.part.pins[pin.ix()].name,
        )));
    }
    check_sites(sites, &at)?;
    let mut edits = Vec::new();
    // Several sites commonly share a document. Parse each document once for this operation.
    let mut trees = Vec::new();
    for site in sites {
        let call = site_call(site).expect("library sites were refused");
        if !trees.iter().any(|(id, _)| *id == call.doc) {
            trees.push((call.doc, parse(document(sources, call)?)?));
        }
        let tree = &trees.iter().find(|(id, _)| *id == call.doc).expect("the site document was parsed").1;
        let missing = || located(call.doc, call.span.clone(), "the pin's mapped connection expression is missing");
        let (span, text) = match site {
            PinSite::Field { pin, .. } => {
                let pins = call_at(tree, call).and_then(|n| n.call("e.part"))
                    .and_then(|args| args.get(1)).and_then(|o| o.field("pins")).ok_or_else(missing)?;
                let Kind::Object { fields, open, close, .. } = &pins.value.kind else { return Err(missing()); };
                let index = fields.iter().position(|f| f.name.as_deref() == Some(pin.as_str())).ok_or_else(missing)?;
                let spans: Vec<_> = fields.iter().map(|f| f.span.clone()).collect();
                entry_removal(tree.source(), *open, *close, &spans, index, index)
            }
            PinSite::Item { .. } => (site_value(tree, site).ok_or_else(missing)?.span.clone(), "nil".into()),
            PinSite::Connect { .. } => {
                let stmt = statement(tree, call).filter(|n| same_span(n, call) && n.call("e.connect").is_some())
                    .ok_or_else(|| located(call.doc, call.span.clone(), "the e.connect call is not a statement by itself: disconnect it by hand"))?;
                (removal_span(tree.source(), &stmt.span), String::new())
            }
            PinSite::Library { .. } => unreachable!("library sites were refused"),
        };
        let e = edit(call.doc, span, text);
        // e.connect(x.A, x.A) records both ends of the same statement.
        if !edits.contains(&e) { edits.push(e); }
    }
    Ok(edits)
}

fn mapped<'a>(
    sources: &Sources, map: &SourceMap, design: &'a Design, unit: BoardId, comp: CompId, pin: PinIx,
) -> Result<(SourceRange, &'a Circuit, &'a Component), Diag> {
    let (at, circuit, component) = mapped_comp(sources, map, design, unit, comp)?;
    if component.part.pins.get(pin.ix()).is_none() {
        return Err(located(at.doc, at.span.clone(), format!("{} has no pin {}", component.path, pin.0)));
    }
    Ok((at, circuit, component))
}

/// The call that makes component `comp` of `unit`, its circuit and the component.
fn mapped_comp<'a>(
    sources: &Sources, map: &SourceMap, design: &'a Design, unit: BoardId, comp: CompId,
) -> Result<(SourceRange, &'a Circuit, &'a Component), Diag> {
    let selection = Sel::Comp(CompRef { board: unit, comp });
    let fallback = map.locate(&selection).or_else(|| map.locate(&Sel::Unit(unit)))
        .unwrap_or(SourceRange { doc: sources.docs().first().map_or(DocId(0), |d| d.id), span: 0..0 });
    let fail = |message| located(fallback.doc, fallback.span.clone(), message);
    let circuit = &design.units.iter().find(|u| u.id == unit)
        .ok_or_else(|| fail(format!("the design has no unit {}", unit.0)))?.circuit;
    let component = circuit.comps.get(comp.ix()).ok_or_else(|| fail(format!("unit {} has no component {}", circuit.name, comp.0)))?;
    let at = map.locate(&selection).ok_or_else(|| fail("the component has no mapped call".into()))?;
    Ok((at, circuit, component))
}

pub(crate) fn site_call(site: &PinSite) -> Option<&SourceRange> {
    match site {
        PinSite::Field { call, .. } | PinSite::Item { call, .. } | PinSite::Connect { call, .. } => Some(call),
        PinSite::Library { .. } => None,
    }
}

fn shared(at: &SourceRange, count: usize) -> Diag {
    located(at.doc, at.span.clone(), format!("written in a body that {count} components share: give the block a port or a parameter"))
}

fn check_shared(map: &SourceMap, at: &SourceRange) -> Result<(), Diag> {
    let count = map.all_at(at.doc, at.span.start).iter()
        .filter(|sel| matches!(sel, Sel::Comp(_)) && map.locate(sel).as_ref() == Some(at)).count();
    if count > 1 { return Err(shared(at, count)); }
    Ok(())
}

pub(crate) fn check_sites(sites: &[PinSite], component: &SourceRange) -> Result<(), Diag> {
    for site in sites {
        match site {
            PinSite::Library { lib } => return Err(located(component.doc, component.span.clone(), format!("the pin connection is written in the {lib} library, which is not edited here"))),
            PinSite::Field { call, count, .. } | PinSite::Item { call, count, .. } | PinSite::Connect { call, count, .. } if *count > 1 => return Err(shared(call, *count)),
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn site_label(sources: &Sources, site: &PinSite) -> String {
    let Some(call) = site_call(site) else { return "a library".into() };
    let role = match site {
        PinSite::Field { pin, .. } => format!("pins.{pin}"),
        PinSite::Item { index, .. } => format!("pin item {index}"),
        PinSite::Connect { end, .. } => format!("e.connect end {end}"),
        PinSite::Library { .. } => unreachable!(),
    };
    match sources.doc(call.doc) {
        Some(doc) => {
            let (line, col) = crate::source::line_col(&doc.text, call.span.start).unwrap_or((1, 1));
            format!("{}:{line}:{col} ({role})", doc.name)
        }
        None => format!("document {} at {}..{} ({role})", call.doc.0, call.span.start, call.span.end),
    }
}

fn document<'a>(sources: &'a Sources, at: &SourceRange) -> Result<&'a Doc, Diag> {
    if at.doc.0 & LIB_DOC != 0 {
        return Err(located(at.doc, at.span.clone(), "this call is in a library, which is not edited here"));
    }
    let doc = sources.doc(at.doc).ok_or_else(|| located(at.doc, at.span.clone(), "the call's document is not in the project"))?;
    if doc.text.get(at.span.start as usize..at.span.end as usize).is_none() {
        return Err(located(at.doc, at.span.clone(), "the mapped call is outside its document"));
    }
    Ok(doc)
}

fn parse(doc: &Doc) -> Result<Syntax, Diag> {
    syntax::parse_with(&doc.text, &circuit_units()).map_err(|e| located(doc.id, e.at as u32..e.at as u32, e.message))
}

fn same_span(node: &Node, at: &SourceRange) -> bool {
    node.span == (at.span.start as usize..at.span.end as usize)
}

fn call_at<'a>(tree: &'a Syntax, at: &SourceRange) -> Option<&'a Node> {
    let mut found = None;
    tree.walk(&mut |_, node| {
        if same_span(node, at) && matches!(node.kind, Kind::Call { .. }) { found = Some(node); }
    });
    found
}

fn site_value<'a>(tree: &'a Syntax, site: &PinSite) -> Option<&'a Node> {
    value_in_call(call_at(tree, site_call(site)?)?, site)
}

fn maker_name(call: &Node) -> Option<&str> {
    call.callee_path()?.rsplit('.').next()
}

/// The maker's options parameter. A passive's mapped first pin records the tolerance shift.
fn options_position(maker: Option<&str>, map: &SourceMap, comp: CompRef) -> Option<usize> {
    match maker? {
        "part" => Some(1),
        "led" => Some(0),
        "r" | "c" => {
            let positional_tol = map.pin_sites(&comp, PinIx(0)).iter()
                .any(|site| matches!(site, PinSite::Item { positional_tol: true, .. }));
            Some(crate::value_site::two_pin_net_arg(1, positional_tol) + 1)
        }
        _ => None,
    }
}

fn value_in_call<'a>(call: &'a Node, site: &PinSite) -> Option<&'a Node> {
    let Kind::Call { args, .. } = &call.kind else { return None };
    let callee = maker_name(call)?;
    let arg = |position, name: &str| args.iter().enumerate().rev().find_map(|(i, arg)| match &arg.kind {
        Kind::Entry { name: key, value, .. } => (key == name).then_some(value.as_ref()),
        _ => (i == position).then_some(arg),
    });
    match site {
        PinSite::Field { pin, .. } if callee == "part" => Some(&arg(1, "opts")?.field("pins")?.value.field(pin)?.value),
        PinSite::Item { index, positional_tol, .. } => match callee {
            "r" | "c" | "l" => {
                let position = crate::value_site::two_pin_net_arg(*index, *positional_tol);
                arg(position, ["value", "a", "b", "opts"].get(position)?)
            }
            "led" => arg(1 + *index, ["a", "k"].get(*index)?),
            "part" => arg(1, "opts")?.field("pins")?.value.items()?.get(*index),
            _ => None,
        },
        PinSite::Connect { end, .. } if callee == "connect" => {
            let opposite = 1usize.checked_sub(*end)?;
            arg(opposite, ["a", "b"][opposite])
        }
        _ => None,
    }
}

/// Rewrite the expression that connects this pin, including the opposite end of e.connect.
pub(crate) fn rewrite_site(tree: &Syntax, site: &PinSite, expr: &str) -> Result<TextEdit, Diag> {
    let call = site_call(site).expect("library sites must be refused before rewriting");
    let value = site_value(tree, site).ok_or_else(|| located(
        call.doc, call.span.clone(), "the pin's mapped connection expression is missing",
    ))?;
    Ok(edit(call.doc, value.span.clone(), expr.to_string()))
}

/// Find the actual component binding in this lexical scope or an enclosing one. A different
/// instance of the same build body is not reachable through that body's local binding.
pub(crate) fn pin_expr(
    tree: &Syntax, map: &SourceMap, circuit: &Circuit, block: BlockId,
    at: &SourceRange, comp: CompId, pin: PinIx,
) -> Result<String, Diag> {
    let mapped = map.locate(&Sel::Comp(CompRef { board: circuit.unit, comp }));
    let call = mapped.as_ref().unwrap_or(at);
    let fail = |message| located(call.doc, call.span.clone(), message);
    let component = circuit.comps.get(comp.ix()).ok_or_else(|| fail(format!("the circuit has no component {}", comp.0)))?;
    let definition = component.part.pins.get(pin.ix()).ok_or_else(|| fail(format!("{} has no pin {}", component.path, pin.0)))?;
    let label = format!("{} pin {}", component.path, definition.name);
    if !circuit_splice::identifier(&definition.name) {
        return Err(fail(format!("{label} has no field name: connect it by hand")));
    }
    let unreachable = || fail(format!("{label} is not reachable here: bind the part to a name"));
    let mut instance = Some(block);
    while let Some(id) = instance {
        if id == component.block { break; }
        instance = circuit.tree.get(id.ix()).and_then(|block| block.parent);
    }
    if instance.is_none() || mapped.is_none() || call.doc != at.doc { return Err(unreachable()); }
    let mut scope = Loc::default();
    let mut len = usize::MAX;
    tree.walk(&mut |loc, node| {
        if node.block().is_some() && node.span.start <= at.span.start as usize
            && at.span.end as usize <= node.span.end && node.span.len() < len
        {
            scope = loc.clone();
            len = node.span.len();
        }
    });
    let mut shadowed = HashSet::new();
    let mut loc = Some(scope);
    while let Some(scope) = loc {
        if let Some(node) = tree.node(&scope) {
            if let Some(body) = node.block() {
                for stmt in &body.stmts {
                    let Kind::Let { keyword, pattern_span, value: Some(value), .. } = &stmt.kind else { continue };
                    let Some(binding) = stmt.let_name() else { continue };
                    if keyword == "let" && !shadowed.contains(binding) && same_span(value, call)
                        && tree.source()[pattern_span.clone()].trim() == binding
                    {
                        return Ok(format!("{binding}.{}", definition.name));
                    }
                }
                for stmt in &body.stmts {
                    match &stmt.kind {
                        Kind::Let { names, .. } => shadowed.extend(names.iter().cloned()),
                        Kind::Fn { name: Some(name), .. } => { shadowed.insert(name.clone()); }
                        _ => {}
                    }
                }
            }
            if let Kind::Fn { name, params, .. } = &node.kind {
                shadowed.extend(name.iter().cloned());
                shadowed.extend(params.iter().map(|param| param.name.clone()));
            }
        }
        loc = scope.parent();
    }
    Err(unreachable())
}

pub(crate) fn net_expr(tree: &Syntax, map: &SourceMap, circuit: &Circuit, block: BlockId, at: &SourceRange, net: NetId) -> Result<String, Diag> {
    let target = circuit.nets.get(net.ix()).ok_or_else(|| located(at.doc, at.span.clone(), format!("the circuit has no net {}", net.0)))?;
    let block = circuit.tree.get(block.ix()).ok_or_else(|| located(at.doc, at.span.clone(), "the component's block is missing"))?;
    let unreachable = || located(at.doc, at.span.clone(), format!("{} is not reachable inside {}: give the block a port", target.name, if block.path.is_empty() { "the document" } else { &block.path }));
    let scope = if block.id.ix() == 0 {
        tree.root()
    } else {
        let mut build: Option<(&str, &Node)> = None;
        tree.walk(&mut |_, node| {
            let Some(field) = node.call("e.block").and_then(|args| args.first()).and_then(|o| o.field("build")) else { return };
            let Kind::Fn { params, body, .. } = &field.value.kind else { return };
            if body.span.start <= at.span.start as usize && at.span.end as usize <= body.span.end
                && build.as_ref().is_none_or(|(_, old)| body.span.len() < old.span.len())
            {
                if let Some(param) = params.first() { build = Some((param.name.as_str(), body.as_ref())); }
            }
        });
        let (param, body) = build.ok_or_else(unreachable)?;
        for (port, nets) in &block.ports {
            if let Some(index) = nets.iter().position(|n| *n == net) {
                return Ok(if nets.len() == 1 { format!("{param}.{port}") } else { format!("{param}.{port}[{index}]") });
            }
        }
        body
    };
    let prefix = if block.path.is_empty() { String::new() } else { format!("{}.", block.path) };
    let net_at = map.locate(&Sel::Net(NetRef { board: circuit.unit, net }));
    for stmt in &scope.block().ok_or_else(unreachable)?.stmts {
        let Kind::Let { pattern_span, value: Some(value), .. } = &stmt.kind else { continue };
        let Some(binding) = stmt.let_name() else { continue };
        if tree.source()[pattern_span.clone()].trim() != binding { continue; }
        let Some((callee, args)) = value.as_call() else { continue };
        match callee {
            "e.net" if net_at.as_ref().is_some_and(|r| r.doc == at.doc && same_span(value, r)) => {
                // Explicit names in a body run once need not include its path. Shared calls
                // need the instance prefix to distinguish the nets made by that same text.
                let made = map.all_at(at.doc, value.span.start as u32).iter().filter(|sel| {
                    matches!(sel, Sel::Net(n) if n.board == circuit.unit)
                        && map.locate(sel).is_some_and(|r| same_span(value, &r))
                }).count();
                if target.name.starts_with(&prefix) || made == 1 { return Ok(binding.to_string()); }
            }
            "e.bus" => {
                let Some(bus) = circuit.buses.iter().find(|bus| {
                    bus.name.starts_with(&prefix) && bus.members.iter().any(|(_, n)| *n == net)
                        && match args.first().and_then(Node::string) {
                            Some(name) => bus.name == format!("{prefix}{name}"),
                            None => bus.members.iter().any(|(_, n)| map.locate(&Sel::Net(NetRef { board: circuit.unit, net: *n }))
                                .is_some_and(|r| r.doc == at.doc && same_span(value, &r))),
                        }
                }) else { continue };
                let (index, (member, _)) = bus.members.iter().enumerate().find(|(_, (_, n))| *n == net).expect("the bus contains the net");
                return match args.get(1).map(|n| &n.kind) {
                    Some(Kind::Array { .. }) => Ok(format!("{binding}.{member}")),
                    Some(Kind::Number(_)) => Ok(format!("{binding}[{index}]")),
                    _ => Err(unreachable()),
                };
            }
            _ => {}
        }
    }
    Err(unreachable())
}

/// The object's part of value_site::override_edit, for a part's second argument or its pins.
fn object_edit(tree: &Syntax, object: &Node, name: &str, value: &str) -> Option<(Range<usize>, String)> {
    object.object()?;
    if let Some(field) = object.field(name) { return Some((field.value.span.clone(), value.to_string())); }
    let field = format!("{}: {value}", syntax::write::key(name).ok()?);
    object_append(tree, object, &[field])
}

fn object_append(tree: &Syntax, object: &Node, added: &[String]) -> Option<(Range<usize>, String)> {
    let Kind::Object { fields, open, close, .. } = &object.kind else { return None };
    let source = tree.source();
    let start = fields.last().map_or(open + 1, |f| f.span.end);
    let tail = &source[start..*close];
    let trailing_comma = comma(tail).is_some();
    let commas = trailing_comma || fields.windows(2).any(|pair| comma(&source[pair[0].span.end..pair[1].span.start]).is_some());
    let field = added.join(if commas { ", " } else { " " });
    let mut text = String::new();
    if commas && !trailing_comma && !fields.is_empty() { text.push(','); }
    let trailing = if trailing_comma { "," } else { "" };
    if source[open + 1..*close].contains('\n') {
        let newline = if source[open + 1..*close].contains("\r\n") { "\r\n" } else { "\n" };
        let indent = fields.last().map_or_else(|| format!("{}    ", line_indent(source, *open)), |f| line_indent(source, f.span.start).to_string());
        let line_start = source[..*close].rfind('\n').map_or(0, |i| i + 1);
        let closing_indent = &source[line_start..*close];
        if line_start >= start && closing_indent.chars().all(|c| c == ' ' || c == '\t') {
            text.push_str(&source[start..line_start]);
            text.push_str(&format!("{indent}{field}{trailing}{newline}{closing_indent}"));
        } else {
            text.push_str(tail);
            text.push_str(&format!("{newline}{indent}{field}{trailing}"));
        }
    } else {
        text.push_str(tail);
        if !tail.ends_with([' ', '\t']) { text.push(' '); }
        text.push_str(&field);
        text.push_str(trailing);
    }
    Some((start..*close, text))
}

fn comma(text: &str) -> Option<Range<usize>> {
    lex_with(text, &circuit_units()).iter().find(|t| &text[t.span()] == ",").map(|t| t.span())
}

/// Remove entries and their preceding whitespace. Remove a comma on one side as needed,
/// preserving comments and the spacing of the surviving entries.
pub(crate) fn entry_removal(source: &str, open: usize, close: usize, spans: &[Range<usize>], first: usize, last: usize) -> (Range<usize>, String) {
    let before = if first == 0 { open + 1 } else { spans[first - 1].end };
    let gap = &source[before..spans[first].start];
    // Keep the newline terminating a preceding line comment, or the surviving entry could
    // become part of that comment. Comment-free gaps still lose all preceding whitespace.
    let start = if gap.contains("//") { spans[first].start } else { before + gap.trim_end_matches(char::is_whitespace).len() };
    let end = spans[last].end;
    if first > 0 {
        if let Some(c) = comma(gap) {
            return (before + c.start..end, source[before + c.end..start].to_string());
        }
    } else {
        let after = spans.get(last + 1).map_or(close, |s| s.start);
        if let Some(c) = comma(&source[end..after]) {
            return (start..end + c.end, source[end..end + c.start].to_string());
        }
    }
    (start..end, String::new())
}

fn statement<'a>(tree: &'a Syntax, at: &SourceRange) -> Option<&'a Node> {
    let mut found: Option<&Node> = None;
    tree.walk(&mut |_, node| {
        if let Some(block) = node.block() {
            for stmt in &block.stmts {
                if stmt.span.start <= at.span.start as usize && at.span.end as usize <= stmt.span.end
                    && found.is_none_or(|old| stmt.span.len() < old.span.len()) { found = Some(stmt); }
            }
        }
    });
    found
}

fn line_indent(source: &str, at: usize) -> &str {
    let start = source[..at].rfind('\n').map_or(0, |i| i + 1);
    let before = &source[start..at];
    &before[..before.len() - before.trim_start_matches([' ', '\t']).len()]
}

fn removal_span(source: &str, stmt: &Range<usize>) -> Range<usize> {
    let start = source[..stmt.start].rfind('\n').map_or(0, |i| i + 1);
    let end = source[stmt.end..].find('\n').map_or(source.len(), |i| stmt.end + i + 1);
    let tail = &source[stmt.end..end];
    let rest = tail.trim_start_matches([' ', '\t', ';', '\r']);
    if source[start..stmt.start].chars().all(|c| c == ' ' || c == '\t')
        && (rest.is_empty() || rest == "\n" || rest.starts_with("//")) { return start..end; }
    let trimmed = tail.trim_start_matches([' ', '\t']);
    let end = if trimmed.starts_with(';') { stmt.end + tail.len() - trimmed.len() + 1 } else { stmt.end };
    stmt.start..end
}

fn edit(doc: DocId, span: Range<usize>, text: String) -> TextEdit {
    TextEdit { doc, span: span.start as u32..span.end as u32, text }
}

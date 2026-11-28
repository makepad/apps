//! Structural edits on the Splash text of a board Design item.
use crate::board::tree::{board_kind, live, BoardTree};
use crate::model::{Action, Model, Origin};
use crate::tweak::{DesignPlace, DesignRequest, StepDir, ThemeInstall, ThemeVal};
use makepad_town_core::{
    entity::{field, kind},
    event::{Id, Value as FieldValue},
    fold::State, hex, literal::is_identifier,
};
use makepad_widgets::{
    makepad_platform::script_mod_doc::{DocKind, ScriptModDoc},
    makepad_script::syntax::{
        self, diff::{self, AstPatchOp, NodeKey}, edit::Replace,
        literal_class::parse_literal_class, widget::{self, Place, WidgetEdit},
        write::{self, ColorStyle, Value as SplashValue}, At, Kind, Loc, Node, Syntax,
    },
};
use std::collections::{HashMap, HashSet};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct D1Snapshot {
    pub item: Id,
    pub src: String,
    /// Writer of the source register.
    pub revision: Id,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct D1WidgetNode {
    pub path: String,
    pub name: String,
    pub ty: String,
    pub depth: u16,
    pub children: usize,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum D1Op {
    Insert { path: String, place: DesignPlace, ty: String, name: Option<String>, props: Vec<(String, String)> },
    Move { path: String, target: String, place: DesignPlace },
    Step { path: String, dir: StepDir },
    Delete { path: String },
    Duplicate { path: String, name: Option<String> },
    Wrap { path: String, ty: String, name: Option<String> },
    Rename { path: String, name: String },
    SetProp { path: String, prop: String, value: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct D1Edit {
    pub next_src: String,
    pub file_edits: Vec<Replace>,
    pub focus_path: Option<String>,
    pub focus_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct D1WriteOutcome {
    pub item: Id,
    pub previous_revision: Id,
    pub revision: Id,
    pub changed: bool,
    pub edit: D1Edit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum D1Error {
    InvalidItem(String),
    StaleRevision { expected: Id, current: Id },
    InvalidInput(String),
    Parse(String),
    TargetNotFound { path: String },
    AmbiguousTarget { path: String, matches: Vec<String> },
    Edit(String),
    WriteFailed(String),
}

impl fmt::Display for D1Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidItem(msg) | Self::InvalidInput(msg) | Self::Parse(msg)
                | Self::Edit(msg) | Self::WriteFailed(msg) => f.write_str(msg),
            Self::StaleRevision { expected, current } => write!(f,
                "design source changed; expected revision @{}, current revision @{}", hex(expected), hex(current)),
            Self::TargetNotFound { path } => write!(f, "design target `{path}` was not found"),
            Self::AmbiguousTarget { path, matches } => write!(f, "ambiguous design target `{path}`: {}", matches.join(", ")),
        }
    }
}

impl From<syntax::EditError> for D1Error {
    fn from(err: syntax::EditError) -> Self { Self::Edit(err.to_string()) }
}

impl D1Op {
    pub(crate) fn from_request(req: &DesignRequest) -> Result<Self, D1Error> {
        Ok(match req {
            DesignRequest::Insert { path, place, ty, name, props } => Self::Insert {
                path: path.clone(), place: *place, ty: ty.clone(), name: name.clone(), props: props.clone(),
            },
            DesignRequest::Move { path, target, place } => Self::Move { path: path.clone(), target: target.clone(), place: *place },
            DesignRequest::Step { path, dir } => Self::Step { path: path.clone(), dir: *dir },
            DesignRequest::Delete { path, .. } => Self::Delete { path: path.clone() },
            DesignRequest::Duplicate { path, name } => Self::Duplicate { path: path.clone(), name: name.clone() },
            DesignRequest::Wrap { path, ty, name } => Self::Wrap { path: path.clone(), ty: ty.clone(), name: name.clone() },
            DesignRequest::Rename { path, name, .. } => Self::Rename { path: path.clone(), name: name.clone() },
            _ => return Err(D1Error::InvalidInput("this design request requires a live app".into())),
        })
    }
}

pub(crate) fn read_design(state: &State, item: Id) -> Result<D1Snapshot, D1Error> {
    let entity = state.entities.get(&item).filter(|e| board_kind(&e.kind))
        .ok_or_else(|| D1Error::InvalidItem(format!("unknown board item {}", hex(&item))))?;
    if !live(entity) { return Err(D1Error::InvalidItem(format!("item {} is deleted or archived", hex(&item)))) }
    if BoardTree::new(state).placement(entity).is_none() {
        return Err(D1Error::InvalidItem(format!("item {} has no live board", hex(&item))));
    }
    if entity.kind != kind::DESIGN { return Err(D1Error::InvalidItem(format!("item {} is not a design", hex(&item)))) }
    let source = entity.fields.get(&field::SRC).ok_or_else(|| D1Error::InvalidItem("design has no source register".into()))?;
    let FieldValue::Text(src) = &source.value else { return Err(D1Error::InvalidItem("design source must be text".into())) };
    Ok(D1Snapshot { item, src: src.clone(), revision: source.writer })
}

fn check_tree(tree: &Syntax) -> Result<(), D1Error> {
    match tree.diagnostics().first() {
        Some(err) => Err(D1Error::Parse(err.to_string())),
        None => Ok(()),
    }
}

fn parse(src: &str) -> Result<Syntax, D1Error> {
    let tree = syntax::parse(src).map_err(|e| D1Error::Parse(e.to_string()))?;
    check_tree(&tree)?;
    Ok(tree)
}

struct Object {
    name: Option<String>,
    ty: String,
    loc: Loc,
}

impl Object {
    fn segment(&self) -> &str {
        self.name.as_deref().unwrap_or_else(|| self.ty.rsplit('.').next().unwrap_or(&self.ty))
    }
}

#[derive(Clone, Copy)]
enum Roots { Widgets, Patch }

// The widget picker sees typed declarations (including let). A diff instead
// addresses every object assignment or entry, including untyped theme blocks.
fn roots(tree: &Syntax, mode: Roots) -> Vec<Object> {
    let patch = matches!(mode, Roots::Patch);
    tree.statements().iter().enumerate().filter_map(|(index, stmt)| {
        let root = Loc(vec![index]);
        let (name, value, loc) = match &stmt.kind {
            Kind::Object { .. } => (None, stmt, root),
            Kind::Entry { name, op, value, .. } if patch || matches!(op.as_str(), ":=" | ":") =>
                (Some(name.as_str()), value.as_ref(), root.child(0)),
            Kind::Assign { target, op, value, .. } if patch || op == "=" =>
                (Some(target.ident()?), value.as_ref(), root.child(1)),
            Kind::Let { names, value: Some(value), .. } if !patch && names.len() == 1 =>
                (Some(names[0].as_str()), value.as_ref(), root.child(0)),
            _ => return None,
        };
        let (ty, _) = value.object()?;
        let ty = if patch { ty.unwrap_or("") } else { ty? };
        Some(Object { name: name.map(str::to_owned), ty: ty.into(), loc })
    }).collect()
}

fn children(tree: &Syntax, loc: &Loc) -> Vec<Object> {
    widget::widget_children(tree, loc).into_iter().map(|w| Object { name: w.name, ty: w.ty, loc: w.value_loc }).collect()
}

fn sibling_paths(parent: &str, objects: Vec<Object>) -> Vec<(String, Object)> {
    let mut counts = HashMap::new();
    for obj in &objects { *counts.entry(obj.segment()).or_insert(0usize) += 1; }
    let mut seen = HashMap::new();
    let paths: Vec<_> = objects.iter().map(|obj| {
        let seg = obj.segment();
        if counts[seg] == 1 { return format!("{parent}/{seg}") }
        let ordinal = seen.entry(seg).or_insert(0usize);
        *ordinal += 1;
        format!("{parent}/{seg}.{ordinal}")
    }).collect();
    paths.into_iter().zip(objects).collect()
}

fn widget_index(tree: &Syntax) -> Vec<D1WidgetNode> {
    fn visit(tree: &Syntax, parent: &str, depth: u16, objects: Vec<Object>, out: &mut Vec<D1WidgetNode>) {
        for (path, obj) in sibling_paths(parent, objects) {
            let kids = children(tree, &obj.loc);
            out.push(D1WidgetNode { path: path.clone(), name: obj.segment().into(), ty: obj.ty,
                depth, children: kids.len(), loc: obj.loc });
            if !kids.is_empty() { visit(tree, &path, depth + 1, kids, out); }
        }
    }
    let mut nodes = Vec::new();
    visit(tree, "", 0, roots(tree, Roots::Widgets), &mut nodes);
    nodes
}

pub(crate) fn list_widgets(src: &str) -> Result<Vec<D1WidgetNode>, D1Error> {
    Ok(widget_index(&parse(src)?))
}

fn ordinal(seg: &str) -> Option<(&str, usize)> {
    let (name, digits) = seg.rsplit_once('.')?;
    if name.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) { return None }
    Some((name, digits.parse::<usize>().ok()?.checked_sub(1)?))
}

fn pick(path: &str, mut matches: impl Iterator<Item = (String, Loc)>, index: Option<usize>) -> Result<Option<Loc>, D1Error> {
    if let Some(i) = index { return Ok(matches.nth(i).map(|(_, loc)| loc)) }
    let Some((first, loc)) = matches.next() else { return Ok(None) };
    let Some((second, _)) = matches.next() else { return Ok(Some(loc)) };
    let mut paths = vec![first, second];
    paths.extend(matches.map(|(p, _)| p));
    Err(D1Error::AmbiguousTarget { path: path.into(), matches: paths })
}

fn select(path: &str, segment: &str, siblings: &[(String, Object)], exact_root: bool) -> Result<Option<Loc>, D1Error> {
    let (base, index) = if exact_root { (segment, None) } else {
        ordinal(segment).map_or((segment, None), |(base, i)| (base, Some(i)))
    };
    let candidates = siblings.iter().filter(|(_, w)| w.segment() == base || (w.name.is_none() && w.ty == base));
    pick(path, candidates.map(|(p, w)| (p.clone(), w.loc.clone())), index)
}

fn child_path(tree: &Syntax, path: &str, mut parent: Loc, mut prefix: String, segments: &[&str]) -> Result<Option<Loc>, D1Error> {
    for &segment in segments {
        let siblings = sibling_paths(&prefix, children(tree, &parent));
        if let Some(loc) = select(path, segment, &siblings, false)? {
            prefix = siblings.into_iter().find(|(_, w)| w.loc == loc).unwrap().0;
            parent = loc;
            continue;
        }
        let Some((_, fields)) = tree.node(&parent).and_then(Node::object) else { return Ok(None) };
        if ordinal(segment).is_some() { return Ok(None) }
        let slots = fields.iter().enumerate().filter(|(_, field)| {
            field.name.as_deref() == Some(segment) && matches!(field.op.as_str(), ":" | "+:")
                && field.value.object().is_some_and(|(ty, _)| ty.is_some())
        });
        prefix = format!("{prefix}/{segment}");
        let Some(loc) = pick(path, slots.map(|(i, _)| (prefix.clone(), parent.child(i))), None)? else { return Ok(None) };
        parent = loc;
    }
    Ok(Some(parent))
}

pub(crate) fn resolve_path(tree: &Syntax, path: &str) -> Result<Loc, D1Error> {
    check_tree(tree)?;
    if path.trim().is_empty() || path.chars().any(char::is_control) {
        return Err(D1Error::InvalidInput("empty design path or control character".into()));
    }
    let path = path.trim();
    let missing = || D1Error::TargetNotFound { path: path.into() };
    let roots = sibling_paths("", roots(tree, Roots::Widgets));
    if path == "/" {
        return pick(path, roots.into_iter().map(|(p, w)| (p, w.loc)), None)?.ok_or_else(missing);
    }
    let exact = |seg: &str| roots.iter().any(|(_, w)| w.name.as_deref() == Some(seg));
    let segments: Vec<_> = if path.contains('/') {
        path.strip_prefix('/').unwrap_or(path).split('/').collect()
    } else if exact(path) || ordinal(path).is_some_and(|(seg, _)| is_identifier(seg)) {
        vec![path]
    } else {
        path.split('.').collect()
    };
    if segments.iter().any(|seg| seg.is_empty()) { return Err(D1Error::InvalidInput(format!("empty segment in `{path}`"))) }
    if let Some(loc) = select(path, segments[0], &roots, exact(segments[0]))? {
        let prefix = roots.iter().find(|(_, w)| w.loc == loc).unwrap().0.clone();
        if let Some(found) = child_path(tree, path, loc, prefix, &segments[1..])? { return Ok(found) }
    } else if let [(prefix, root)] = roots.as_slice() {
        if let Some(found) = child_path(tree, path, root.loc.clone(), prefix.clone(), &segments)? { return Ok(found) }
    }
    let suffix = format!("/{}", segments.join("/"));
    let matches = widget_index(tree).into_iter().filter(|w| w.path.ends_with(&suffix)).map(|w| (w.path, w.loc));
    pick(path, matches, None)?.ok_or_else(missing)
}

fn find_key(objects: &[Object], key: &NodeKey) -> Result<usize, D1Error> {
    match key {
        NodeKey::Named(name) => objects.iter().position(|w| w.name.as_ref() == Some(name)),
        NodeKey::Bare { ty, ordinal } => objects.iter().enumerate().filter(|(_, w)| w.name.is_none() && &w.ty == ty)
            .nth(*ordinal).map(|(i, _)| i),
    }.ok_or_else(|| D1Error::TargetNotFound { path: format!("{key:?}") })
}

pub(crate) fn resolve_node_keys(tree: &Syntax, path: &[NodeKey]) -> Result<Loc, D1Error> {
    check_tree(tree)?;
    let (root, rest) = path.split_first().ok_or_else(|| D1Error::InvalidInput("empty patch path".into()))?;
    let roots = roots(tree, Roots::Patch);
    let mut loc = roots[find_key(&roots, root)?].loc.clone();
    for key in rest {
        let kids = children(tree, &loc);
        loc = kids[find_key(&kids, key)?].loc.clone();
    }
    Ok(loc)
}

fn valid_type(ty: &str) -> Result<(), D1Error> {
    if ty.split('.').all(is_identifier) && ty.split('.').next() != Some("scope") { Ok(()) }
    else { Err(D1Error::InvalidInput(format!("invalid widget type `{ty}`"))) }
}

fn unreserved(name: &str) -> bool { write::key(name).is_ok_and(|key| key == name) }

fn valid_name(name: Option<&str>) -> Result<(), D1Error> {
    if let Some(name) = name.filter(|n| !unreserved(n)) {
        return Err(D1Error::InvalidInput(format!("invalid widget name `{name}`")));
    }
    Ok(())
}

fn valid_prop(prop: &str) -> Result<(), D1Error> {
    if prop.split('.').all(unreserved) { Ok(()) }
    else { Err(D1Error::InvalidInput(format!("invalid design property `{prop}`"))) }
}

fn literal(text: &str) -> Result<Node, D1Error> {
    if text.contains("#(") || text.contains("@(") {
        return Err(D1Error::InvalidInput("interpolation is outside the literal class".into()));
    }
    parse_literal_class(text).map_err(D1Error::InvalidInput)
}

// Verbatim writer values and insert_element preserve spelling. Only rewrite
// color tokens, leaving strings, comments and the case of the digits intact.
fn normalize_hash_colors(text: &str, node: &Node) -> String {
    let mut out = String::with_capacity(text.len());
    let mut end = 0;
    node.walk(&Loc::default(), &mut |_, node| {
        if let Kind::Color(hex) = &node.kind {
            out.push_str(&text[end..node.span.start]);
            out.push('#');
            out.push_str(hex);
            end = node.span.end;
        }
    });
    out.push_str(&text[end..]);
    out
}

fn replacement(before: &str, after: &str) -> Vec<Replace> {
    if before == after { return Vec::new() }
    let prefix = before.bytes().zip(after.bytes()).take_while(|(a, b)| a == b).count();
    let start = before.floor_char_boundary(prefix);
    let suffix = before[start..].bytes().rev().zip(after[start..].bytes().rev()).take_while(|(a, b)| a == b).count();
    let end = before.ceil_char_boundary(before.len() - suffix);
    vec![Replace::new(start..end, &after[start..after.len() - (before.len() - end)])]
}

fn document(src: &str) -> Result<ScriptModDoc, D1Error> {
    ScriptModDoc::parse("design.splash", src.into(), DocKind::Splash).map_err(D1Error::Parse)
}

// Recover the focused object's path from its post-edit address. This also
// covers edits inside a fixed slot, which is omitted from the picker.
fn canonical_path(tree: &Syntax, loc: &Loc) -> Option<String> {
    let (mut path, root) = sibling_paths("", roots(tree, Roots::Widgets)).into_iter()
        .find(|(_, root)| loc.0.starts_with(&root.loc.0))?;
    let mut at = root.loc;
    for &index in &loc.0[at.0.len()..] {
        let next = at.child(index);
        if let Some((p, _)) = sibling_paths(&path, children(tree, &at)).into_iter().find(|(_, w)| w.loc == next) {
            path = p;
        } else {
            let field = tree.node(&at)?.object()?.1.get(index)?;
            if !widget::is_fixed_slot(tree, &next) || field.value.object()?.0.is_none() { return None }
            path.push('/');
            path.push_str(field.name.as_deref()?);
        }
        at = next;
    }
    Some(path)
}

fn finish(doc: &ScriptModDoc, edit: WidgetEdit) -> Result<D1Edit, D1Error> {
    check_tree(&edit.syntax)?;
    let focus_path = edit.focus_open_brace.and_then(|open| widget::find_object_by_open_brace(&edit.syntax, open))
        .and_then(|loc| canonical_path(&edit.syntax, &loc));
    let result = doc.apply_block_edit(0, edit).map_err(D1Error::Edit)?;
    Ok(D1Edit { next_src: result.next_text, file_edits: result.file_edits, focus_path, focus_name: result.focus_name })
}

fn place(p: DesignPlace) -> Place {
    match p { DesignPlace::Before => Place::Before, DesignPlace::Inside => Place::Inside, DesignPlace::After => Place::After }
}

pub(crate) fn apply_op_to_src(src: &str, op: &D1Op) -> Result<D1Edit, D1Error> {
    let tree = parse(src)?;
    let resolve = |p: &str| resolve_path(&tree, p);
    let edit = match op {
        D1Op::Insert { path, place: p, ty, name, props } => {
            valid_type(ty)?;
            valid_name(name.as_deref())?;
            let mut seen = HashSet::new();
            let mut flat = Vec::new();
            let mut dotted = Vec::new();
            for (key, value) in props {
                valid_prop(key)?;
                if !seen.insert(key) { return Err(D1Error::InvalidInput(format!("duplicate design property `{key}`"))) }
                let node = literal(value)?;
                if key.contains('.') { dotted.push((key, value)); }
                else { flat.push((key.clone(), SplashValue::Verbatim(normalize_hash_colors(value, &node)))); }
            }
            let mut edit = widget::insert_widget(&tree, &resolve(path)?, place(*p), ty, name.as_deref(), &flat, ColorStyle::Hash)?;
            if !dotted.is_empty() {
                let loc = edit.focus_open_brace.and_then(|open| widget::find_object_by_open_brace(&edit.syntax, open))
                    .ok_or_else(|| D1Error::Edit("missing inserted widget".into()))?;
                for (key, value) in dotted {
                    // Property edits preserve the widget's own AST address.
                    let prop = widget::set_widget_prop(&edit.syntax, &loc, key, value, ColorStyle::Hash)?;
                    edit.syntax = prop.syntax;
                    edit.focus_open_brace = prop.focus_open_brace;
                }
                edit.edits = replacement(src, edit.syntax.source());
            }
            edit
        }
        D1Op::Move { path, target, place: p } => widget::move_widget(&tree, &resolve(path)?, &resolve(target)?, place(*p))?,
        D1Op::Step { path, dir } => {
            let loc = resolve(path)?;
            match dir {
                StepDir::Up => widget::step_widget_up(&tree, &loc),
                StepDir::Down => widget::step_widget_down(&tree, &loc),
                StepDir::Out => widget::hoist_widget_out(&tree, &loc),
            }?
        }
        D1Op::Delete { path } => widget::delete_widget(&tree, &resolve(path)?)?,
        D1Op::Duplicate { path, name } => {
            valid_name(name.as_deref())?;
            widget::duplicate_widget(&tree, &resolve(path)?, name.as_deref())?
        }
        D1Op::Wrap { path, ty, name } => {
            valid_name(name.as_deref())?;
            valid_type(ty)?;
            widget::wrap_widget(&tree, &resolve(path)?, ty, name.as_deref())?
        }
        D1Op::Rename { path, name } => {
            valid_name(Some(name))?;
            widget::rename_widget(&tree, &resolve(path)?, name)?
        }
        D1Op::SetProp { path, prop, value } => {
            valid_prop(prop)?;
            literal(value)?;
            widget::set_widget_prop(&tree, &resolve(path)?, prop, value, ColorStyle::Hash)?
        }
    };
    finish(&document(src)?, edit)
}

pub(crate) fn theme_val_literal(val: ThemeVal) -> Result<String, D1Error> {
    match val {
        ThemeVal::Color(c) => Ok(format!("#{c:08x}")),
        ThemeVal::Num(n) => write::number(n).map_err(D1Error::InvalidInput),
    }
}

pub(crate) fn apply_theme_tokens_to_src(src: &str, base: Option<&str>, sheet: Option<&str>, tokens: &[(String, String)]) -> Result<D1Edit, D1Error> {
    if tokens.is_empty() && base.is_none() && sheet.is_none() { return Err(D1Error::InvalidInput("empty theme edit".into())) }
    if base.is_some_and(|s| !is_identifier(s)) { return Err(D1Error::InvalidInput("invalid theme base identifier".into())) }
    if sheet.is_some_and(|s| s.chars().any(char::is_control)) { return Err(D1Error::InvalidInput("control character in theme sheet".into())) }
    for (name, value) in tokens {
        if !is_identifier(name) { return Err(D1Error::InvalidInput(format!("invalid theme token `{name}`"))) }
        literal(value)?;
    }
    parse(src)?;
    let result = document(src)?.apply_theme_tokens(0, base, sheet, tokens).map_err(D1Error::Edit)?;
    parse(&result.next_text)?;
    Ok(D1Edit { next_src: result.next_text, file_edits: result.file_edits, focus_path: None, focus_name: None })
}

pub(crate) fn diff_sources(before: &str, after: &str) -> Result<Vec<AstPatchOp>, D1Error> {
    Ok(diff::diff_syntax(&parse(before)?, &parse(after)?))
}

// As with set_widget_prop, a literal dotted field takes precedence over a
// nested object. A property edit cannot remove or descend through a widget.
fn remove_prop(tree: &Syntax, mut owner: Loc, mut prop: &str) -> Result<syntax::edit::Edited, D1Error> {
    valid_prop(prop)?;
    loop {
        let fields = tree.node(&owner).and_then(Node::object).ok_or_else(|| D1Error::Edit("expected a property object".into()))?.1;
        let entry = |name: &str| fields.iter().enumerate().find(|(_, f)| f.name.as_deref() == Some(name));
        let (index, field, rest) = if let Some((i, f)) = entry(prop) {
            (i, f, None)
        } else if let Some((i, f, rest)) = prop.split_once('.').and_then(|(head, rest)| entry(head).map(|(i, f)| (i, f, rest))) {
            (i, f, Some(rest))
        } else {
            return Err(D1Error::Edit(format!("missing property `{prop}`")));
        };
        if field.op == ":=" && field.value.object().is_some_and(|(ty, _)| ty.is_some()) {
            return Err(D1Error::Edit("property path selects an ordered child".into()));
        }
        match rest {
            None => return Ok(tree.remove_field(&owner, prop)?),
            Some(suffix) => { owner = owner.child(index); prop = suffix; }
        }
    }
}

fn composed(before: &str, tree: Syntax, focus: Option<&Loc>, name: Option<String>) -> WidgetEdit {
    let focus_open_brace = focus.and_then(|loc| match &tree.node(loc)?.kind {
        Kind::Object { open, .. } => Some(*open),
        _ => None,
    });
    WidgetEdit { edits: replacement(before, tree.source()), syntax: tree, focus_open_brace, focus_name: name }
}

fn patch_literal(allowed: bool, value: &str) -> Result<Node, D1Error> {
    if !allowed { return Err(D1Error::InvalidInput("patch payload is outside the literal class".into())) }
    literal(value)
}

fn patch_one(tree: &Syntax, op: &AstPatchOp) -> Result<WidgetEdit, D1Error> {
    Ok(match op {
        AstPatchOp::SetProp { path, prop, value_splash, literal_class } => {
            valid_prop(prop)?;
            patch_literal(*literal_class, value_splash)?;
            widget::set_widget_prop(tree, &resolve_node_keys(tree, path)?, prop, value_splash, ColorStyle::Hash)?
        }
        AstPatchOp::RemoveProp { path, prop } => {
            let loc = resolve_node_keys(tree, path)?;
            let removed = remove_prop(tree, loc.clone(), prop)?;
            composed(tree.source(), removed.syntax, Some(&loc), None)
        }
        AstPatchOp::InsertChild { parent, at_index, name, splash, literal_class } => {
            let value = patch_literal(*literal_class, splash)?;
            let Some((Some(ty), _)) = value.object() else { return Err(D1Error::InvalidInput("expected a typed widget constructor".into())) };
            valid_type(ty)?;
            valid_name(name.as_deref())?;
            let parent = resolve_node_keys(tree, parent)?;
            if name.as_ref().is_some_and(|n| widget::fresh_child_name(tree, &parent, n) != *n) {
                return Err(D1Error::Edit("duplicate patch child name".into()));
            }
            let kids = widget::widget_children(tree, &parent);
            if *at_index > kids.len() { return Err(D1Error::Edit("patch insertion index out of range".into())) }
            let at = kids.get(*at_index).map_or(At::End, |w| At::Before(w.field_index));
            let inserted = tree.insert_element(&parent, at, &normalize_hash_colors(splash, &value))?;
            let next = if let Some(name) = name {
                widget::rename_widget(&inserted.syntax, &inserted.loc, name)?.syntax
            } else { inserted.syntax };
            composed(tree.source(), next, Some(&inserted.loc), name.clone())
        }
        AstPatchOp::RemoveChild { parent, key } | AstPatchOp::ReorderChild { parent, key, .. }
            | AstPatchOp::RenameChild { parent, old_key: key, .. } => {
            let kids = children(tree, &resolve_node_keys(tree, parent)?);
            let from = find_key(&kids, key)?;
            let loc = &kids[from].loc;
            match op {
                AstPatchOp::RenameChild { new_name, .. } => {
                    valid_name(Some(new_name))?;
                    widget::rename_widget(tree, loc, new_name)?
                }
                AstPatchOp::ReorderChild { to_index, .. } => {
                    let target = kids.get(*to_index).ok_or_else(|| D1Error::Edit("patch reorder index out of range".into()))?;
                    if from == *to_index {
                        composed(tree.source(), tree.clone(), Some(loc), None)
                    } else {
                        let place = if from < *to_index { Place::After } else { Place::Before };
                        widget::move_widget(tree, loc, &target.loc, place)?
                    }
                }
                _ => widget::delete_widget(tree, loc)?,
            }
        }
    })
}

pub(crate) fn apply_patch_ops_to_src(src: &str, ops: &[AstPatchOp]) -> Result<D1Edit, D1Error> {
    let tree = parse(src)?;
    let Some((first, rest)) = ops.split_first() else {
        return Ok(D1Edit { next_src: src.into(), file_edits: vec![], focus_path: None, focus_name: None });
    };
    let mut edit = patch_one(&tree, first)?;
    for op in rest { edit = patch_one(&edit.syntax, op)?; }
    if !rest.is_empty() { edit.edits = replacement(src, edit.syntax.source()); }
    finish(&document(src)?, edit)
}

fn commit_transform(model: &mut Model, item: Id, expected_revision: Id, origin: Origin,
    transform: impl FnOnce(&str) -> Result<D1Edit, D1Error>) -> Result<D1WriteOutcome, D1Error> {
    let snap = read_design(&model.state, item)?;
    if snap.revision != expected_revision {
        return Err(D1Error::StaleRevision { expected: expected_revision, current: snap.revision });
    }
    let edit = transform(&snap.src)?;
    let changed = edit.next_src != snap.src;
    let revision = if changed {
        model.act(Action::WriteDesign { item, src: edit.next_src.clone(), revision: expected_revision }, origin)
            .map_err(|err| match read_design(&model.state, item) {
                Ok(now) if now.revision != expected_revision => D1Error::StaleRevision { expected: expected_revision, current: now.revision },
                _ => D1Error::WriteFailed(err),
            })?
    } else { expected_revision };
    Ok(D1WriteOutcome { item, previous_revision: expected_revision, revision, changed, edit })
}

pub(crate) fn apply_op(model: &mut Model, item: Id, revision: Id, op: &D1Op, origin: Origin) -> Result<D1WriteOutcome, D1Error> {
    commit_transform(model, item, revision, origin, |src| apply_op_to_src(src, op))
}

pub(crate) fn apply_theme_tokens(model: &mut Model, item: Id, revision: Id, base: Option<&str>, sheet: Option<&str>,
    tokens: &[(String, String)], origin: Origin) -> Result<D1WriteOutcome, D1Error> {
    commit_transform(model, item, revision, origin, |src| apply_theme_tokens_to_src(src, base, sheet, tokens))
}

pub(crate) fn apply_theme_install(model: &mut Model, item: Id, revision: Id, install: &ThemeInstall, origin: Origin) -> Result<D1WriteOutcome, D1Error> {
    commit_transform(model, item, revision, origin, |src| {
        let mut positions = HashMap::new();
        let mut merged: Vec<(&String, ThemeVal)> = Vec::new();
        for (name, val) in install.globals.iter().chain(&install.overrides) {
            let pos = *positions.entry(name).or_insert(merged.len());
            if pos == merged.len() { merged.push((name, *val)); }
            else { merged[pos].1 = *val; }
        }
        let tokens = merged.into_iter().map(|(name, value)| Ok((name.clone(), theme_val_literal(value)?)))
            .collect::<Result<Vec<_>, D1Error>>()?;
        apply_theme_tokens_to_src(src, Some(&install.base), install.sheet.as_deref(), &tokens)
    })
}

pub(crate) fn apply_patch_ops(model: &mut Model, item: Id, revision: Id, ops: &[AstPatchOp], origin: Origin) -> Result<D1WriteOutcome, D1Error> {
    commit_transform(model, item, revision, origin, |src| apply_patch_ops_to_src(src, ops))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::{exchange, shared_town};
    use makepad_town_core::event::Key;

    fn add_design(model: &mut Model, src: &str) -> (Id, Id) {
        let board = model.act(Action::AddBoard { project: model.project().id, title: "Study".into() }, Origin::Ui).unwrap();
        let item = model.act(Action::AddDesign { board, x: 0, y: 0, preset: "phone".into(),
            title: "Card".into(), src: src.into() }, Origin::Ui).unwrap();
        (board, item)
    }

    fn paths(src: &str) -> Vec<String> { list_widgets(src).unwrap().into_iter().map(|w| w.path).collect() }

    fn set(path: &str, prop: &str, value: &str) -> D1Op {
        D1Op::SetProp { path: path.into(), prop: prop.into(), value: value.into() }
    }

    fn replay(src: &str, edit: &D1Edit) {
        let mut text = src.to_owned();
        for splice in edit.file_edits.iter().rev() { text.replace_range(splice.range.clone(), &splice.text); }
        assert_eq!(text, edit.next_src, "splices reproduce the signed source");
    }

    #[test]
    fn d1_design_adapter_applies_structural_and_theme_ops_via_write_design() {
        let src = r#"View {
    header := View {
        title := Label { text: "Start" }
        Label { text: "First" }
        Label { text: "Second" }
    }
    footer := View {}
}"#;
        let (_homes, mut me, mut peer) = shared_town("d1-design");
        let (_, item) = add_design(&mut me, src);
        assert_eq!(read_design(&me.state, item).unwrap(), D1Snapshot { item, src: src.into(), revision: item });
        assert_eq!(paths(src), ["/View", "/View/header", "/View/header/title", "/View/header/Label.1", "/View/header/Label.2", "/View/footer"]);
        let op = D1Op::from_request(&DesignRequest::Insert { path: "/header/title".into(), place: DesignPlace::After,
            ty: "Button".into(), name: Some("save".into()), props: vec![
                ("text".into(), r#""Save""#.into()), ("draw_bg.color".into(), "#x1a2b3cff".into()),
            ] }).unwrap();
        let added = apply_op(&mut me, item, item, &op, Origin::Ui).unwrap();
        assert!(added.changed);
        assert_eq!(added.previous_revision, item);
        assert_ne!(added.revision, item);
        let snap = read_design(&me.state, item).unwrap();
        assert_eq!(snap.revision, added.revision);
        assert_eq!(added.edit.focus_path.as_deref(), Some("/View/header/save"));
        assert_eq!(added.edit.focus_name.as_deref(), Some("save"));
        assert!(snap.src.contains("#1a2b3cff") && !snap.src.contains("#x"));
        assert!(syntax::parse(&snap.src).unwrap().diagnostics().is_empty());
        replay(src, &added.edit);

        let rename = |path: &str| D1Op::Rename { path: path.into(), name: "subtitle".into() };
        let ambiguous = apply_op(&mut me, item, added.revision, &rename("/View/header/Label"), Origin::Ui);
        assert!(matches!(ambiguous, Err(D1Error::AmbiguousTarget { matches, .. })
            if matches == ["/View/header/Label.1", "/View/header/Label.2"]));
        assert_eq!(read_design(&me.state, item).unwrap(), snap);
        let moved = apply_op(&mut me, item, added.revision, &D1Op::Move { path: "/View/header/save".into(),
            target: "/View/footer".into(), place: DesignPlace::Inside }, Origin::Ui).unwrap();
        assert_eq!(moved.edit.focus_path.as_deref(), Some("/View/footer/save"));
        let nodes = paths(&moved.edit.next_src);
        assert!(nodes.iter().any(|p| p == "/View/footer/save") && !nodes.iter().any(|p| p == "/View/header/save"));
        let renamed = apply_op(&mut me, item, moved.revision, &rename("/View/header/Label.2"), Origin::Ui).unwrap();
        assert_eq!(renamed.edit.focus_path.as_deref(), Some("/View/header/subtitle"));
        assert!(renamed.edit.next_src.contains(r#"Label { text: "First" }"#));
        assert!(renamed.edit.next_src.contains(r#"subtitle := Label { text: "Second" }"#));

        let tokens = [("color_accent".into(), "#x336699ff".into()), ("space_2".into(), "12".into())];
        let theme = apply_theme_tokens(&mut me, item, renamed.revision, Some("dark"), Some("Evening"), &tokens, Origin::Ui).unwrap();
        let themed = &theme.edit.next_src;
        for expected in ["mod.theme = mod.themes.dark", "// theme sheet: Evening", "mod.theme +: {", "color_accent: #336699ff", "space_2: 12"] {
            assert!(themed.contains(expected), "missing {expected} in {themed}");
        }
        assert!(!themed.contains("#x"));
        assert_eq!(paths(themed), paths(&renamed.edit.next_src), "theme statements do not add widget roots");
        let tree = parse(themed).unwrap();
        for (short, full) in [("/", "/View"), ("/footer/save", "/View/footer/save")] {
            assert_eq!(resolve_path(&tree, short).unwrap(), resolve_path(&tree, full).unwrap());
        }
        let install = ThemeInstall { label: "Palette".into(), base: "dark".into(), sheet: Some("Evening".into()),
            globals: vec![("color_accent".into(), ThemeVal::Num(f64::NAN)), ("space_2".into(), ThemeVal::Num(16.5)),
                ("border_a".into(), ThemeVal::Num(1.0)), ("border_b".into(), ThemeVal::Num(2.0))],
            overrides: vec![("color_accent".into(), ThemeVal::Color(0x0077aaff)), ("border_a".into(), ThemeVal::Num(3.5))] };
        let baked = apply_theme_install(&mut me, item, theme.revision, &install, Origin::Ui).unwrap();
        let prior = read_design(&me.state, item).unwrap();
        assert_ne!(prior.revision, theme.revision);
        assert_eq!(prior.revision, baked.revision);
        assert_eq!(prior.src.matches("mod.theme +:").count(), 1);
        assert_eq!(prior.src.matches("color_accent:").count(), 1);
        assert!(prior.src.contains("color_accent: #0077aaff") && prior.src.contains("space_2: 16.5") && !prior.src.contains("#x"));
        assert!(prior.src.find("border_a: 3.5").unwrap() < prior.src.find("border_b:").unwrap(), "overrides keep the first token position");

        let publish = set("/footer/save", "text", r#""Publish""#);
        let events = me.open_event_count();
        assert_eq!(apply_op(&mut me, item, item, &publish, Origin::Ui),
            Err(D1Error::StaleRevision { expected: item, current: baked.revision }));
        assert_eq!(read_design(&me.state, item).unwrap(), prior);
        assert_eq!(me.open_event_count(), events);
        exchange(&mut me, &mut peer);
        let published = apply_op(&mut peer, item, baked.revision, &publish, Origin::Ui).unwrap();
        exchange(&mut me, &mut peer);
        let events = me.open_event_count();
        assert_eq!(apply_op(&mut me, item, baked.revision, &rename("missing"), Origin::Ui),
            Err(D1Error::StaleRevision { expected: baked.revision, current: published.revision }));
        let received = read_design(&me.state, item).unwrap();
        assert_eq!(received.revision, published.revision);
        assert_eq!(received.src, published.edit.next_src);
        assert!(received.src.contains(r#"text: "Publish""#));
        assert_eq!(me.open_event_count(), events);
        peer.act(Action::Undo { of: published.revision }, Origin::Ui).unwrap();
        assert_eq!(read_design(&peer.state, item).unwrap().src, prior.src, "undo restores the whole previous source");
    }

    #[test]
    fn d1_path_resolution_handles_roots_shorthand_bare_ordinals_fixed_slots_and_ambiguity() {
        let tree = parse("View{ header := View{ title := Label{} Label{} Label{} } footer := View{} }").unwrap();
        let root = resolve_path(&tree, "/").unwrap();
        let title = resolve_path(&tree, "/View/header/title").unwrap();
        assert_eq!(root, Loc(vec![0]));
        for path in ["header/title", "header.title", "/header/title", "title"] { assert_eq!(resolve_path(&tree, path).unwrap(), title); }
        for (i, path) in ["Label.1", "Label.2"].into_iter().enumerate() {
            assert_eq!(resolve_path(&tree, path).unwrap(), Loc(vec![0, 0, i + 1]));
        }
        assert!(matches!(resolve_path(&tree, "/View/header/Label.3"), Err(D1Error::TargetNotFound { .. })));
        for path in [" ", "header//title", "header/", "/header\n/title"] {
            assert!(matches!(resolve_path(&tree, path), Err(D1Error::InvalidInput(_))), "{path:?}");
        }
        let multi = parse("mod.widgets.Card = View{title := Label{}}\nlet other = View{title := Button{}}").unwrap();
        assert!(matches!(resolve_path(&multi, "/"), Err(D1Error::AmbiguousTarget { matches, .. }) if matches == ["/mod.widgets.Card", "/other"]));
        assert_eq!(resolve_path(&multi, "mod.widgets.Card").unwrap(), Loc(vec![0, 1]));
        assert!(matches!(resolve_path(&multi, "title"), Err(D1Error::AmbiguousTarget { matches, .. })
            if matches == ["/mod.widgets.Card/title", "/other/title"]));
        let well = "FieldWell { input: View {} }";
        assert_eq!(paths(well), ["/FieldWell"]);
        assert_eq!(apply_op_to_src(well, &set("input", "width", "10")).unwrap().focus_path.as_deref(), Some("/FieldWell/input"));
        let inserted = apply_op_to_src(well, &D1Op::Insert { path: "input".into(), place: DesignPlace::Inside, ty: "Label".into(),
            name: None, props: vec![("color".into(), "#xABC".into()), ("text".into(), r##""#xABC""##.into())] }).unwrap();
        assert_eq!(inserted.focus_path.as_deref(), Some("/FieldWell/input/Label"));
        assert!(inserted.next_src.contains("color: #ABC") && inserted.next_src.contains(r##"text: "#xABC""##));
        for op in [D1Op::Delete { path: "input".into() }, D1Op::Rename { path: "input".into(), name: "other".into() }] {
            assert!(matches!(apply_op_to_src(well, &op), Err(D1Error::Edit(e)) if e.contains("fixed slot `input`")));
        }
    }

    #[test]
    fn d1_literal_class_and_invalid_item_refused_before_write() {
        let (_homes, mut me, _peer) = shared_town("d1-invalid");
        let (board, item) = add_design(&mut me, "View{a := Label{}}");
        let initial = read_design(&me.state, item).unwrap();
        let insert = |ty: &str, name: &str, props: Vec<(String, String)>| D1Op::Insert {
            path: "/".into(), place: DesignPlace::Inside, ty: ty.into(), name: Some(name.into()), props,
        };
        let events = me.open_event_count();
        for value in ["fn() {}", r#"crate_resource("x")"#, "#(0)", "@(0)"] {
            for op in [set("/", "text", value), insert("Label", "b", vec![("text".into(), value.into())])] {
                assert!(matches!(apply_op(&mut me, item, item, &op, Origin::Ui), Err(D1Error::InvalidInput(_))), "{op:?}");
            }
            let tokens = [("accent".into(), value.into())];
            assert!(matches!(apply_theme_tokens(&mut me, item, item, None, None, &tokens, Origin::Ui), Err(D1Error::InvalidInput(_))));
        }
        for op in [insert("Button", "fn", vec![]), insert("scope.View", "b", vec![]), set("/", "scope.color", "1"),
            insert("Label", "b", vec![("width".into(), "1".into()), ("width".into(), "2".into())])] {
            assert!(matches!(apply_op(&mut me, item, item, &op, Origin::Ui), Err(D1Error::InvalidInput(_))), "{op:?}");
        }
        assert_eq!(read_design(&me.state, item).unwrap(), initial);
        assert_eq!(me.open_event_count(), events);
        let sticky = me.act(Action::AddItem { board, kind: Key::Number(kind::STICKY), x: 0, y: 0, w: 64, h: 64,
            style: Default::default(), parent: None }, Origin::Ui).unwrap();
        let op = set("/", "width", "16");
        let events = me.open_event_count();
        assert!(matches!(apply_op(&mut me, sticky, sticky, &op, Origin::Ui), Err(D1Error::InvalidItem(_))));
        assert!(me.state.entities[&sticky].fields.get(&field::SRC).is_none());
        assert_eq!(me.open_event_count(), events);
        me.act(Action::DeleteItems { items: vec![item], members: None }, Origin::Ui).unwrap();
        let events = me.open_event_count();
        assert!(matches!(apply_op(&mut me, item, item, &op, Origin::Ui), Err(D1Error::InvalidItem(_))));
        assert_eq!(me.state.entities[&item].fields[&field::SRC].writer, initial.revision);
        assert_eq!(me.state.entities[&item].text(field::SRC), initial.src);
        assert_eq!(me.open_event_count(), events);
    }

    #[test]
    fn d1_diff_and_apply_patch_ops_roundtrip_via_write_design() {
        let before = "mod.theme +: { accent: #abc }\nView{ width: 1 height: 4 draw_bg +: {color: #def keep: 3} a := A{} b := B{} old := Label{} gone := Box{} }";
        let after = "mod.theme +: { accent: #fff }\nView{ width: 2 draw_bg +: {keep: 3} b := B{} a := A{} new_title := Label{} icon := Icon{color: #09f} Dot{color: #abc} }";
        let (_homes, mut me, _peer) = shared_town("d1-patch");
        let (_, item) = add_design(&mut me, before);
        let mut ops = diff_sources(before, after).unwrap();
        assert!(ops.iter().any(|o| matches!(o, AstPatchOp::SetProp { .. })));
        for key in ["height", "draw_bg.color"] {
            assert!(ops.iter().any(|o| matches!(o, AstPatchOp::RemoveProp { prop, .. } if prop == key)));
        }
        assert!(ops.iter().any(|o| matches!(o, AstPatchOp::RenameChild { .. })));
        assert!(ops.iter().any(|o| matches!(o, AstPatchOp::ReorderChild { .. })));
        assert!(ops.iter().any(|o| matches!(o, AstPatchOp::RemoveChild { .. })));
        for named in [true, false] {
            let splash = ops.iter_mut().find_map(|op| match op {
                AstPatchOp::InsertChild { name, splash, .. } if name.is_some() == named => Some(splash),
                _ => None,
            }).unwrap();
            *splash = splash.replace('#', "#x");
        }
        let updated = apply_patch_ops(&mut me, item, item, &ops, Origin::Tool).unwrap();
        let snap = read_design(&me.state, item).unwrap();
        assert_eq!(snap.revision, updated.revision);
        assert_ne!(snap.revision, item);
        assert!(!snap.src.contains("#x"));
        assert!(diff_sources(&snap.src, after).unwrap().is_empty(), "all patches must replay on the source left by their predecessor");
        replay(before, &updated.edit);
        let root = vec![NodeKey::Bare { ty: "View".into(), ordinal: 0 }];
        let events = me.open_event_count();
        let noop = AstPatchOp::ReorderChild { parent: root.clone(), key: NodeKey::Named("b".into()), to_index: 0 };
        let same = apply_patch_ops(&mut me, item, snap.revision, &[noop], Origin::Tool).unwrap();
        assert!(!same.changed && same.edit.file_edits.is_empty());
        assert_eq!(same.revision, snap.revision);
        for (literal_class, payload) in [(false, "1"), (true, "fn() {}")] {
            let valid = AstPatchOp::RemoveProp { path: root.clone(), prop: "width".into() };
            let invalid = AstPatchOp::SetProp { path: root.clone(), prop: "width".into(), value_splash: payload.into(), literal_class };
            assert!(matches!(apply_patch_ops(&mut me, item, snap.revision, &[valid, invalid], Origin::Tool), Err(D1Error::InvalidInput(_))));
            let insert = AstPatchOp::InsertChild { parent: root.clone(), at_index: 0, name: None,
                splash: format!("Label{{ width: {payload} }}"), literal_class };
            assert!(matches!(apply_patch_ops(&mut me, item, snap.revision, &[insert], Origin::Tool), Err(D1Error::InvalidInput(_))));
        }
        assert_eq!(read_design(&me.state, item).unwrap(), snap);
        assert_eq!(me.open_event_count(), events, "no-op and failed patch batches append zero events");
    }
}

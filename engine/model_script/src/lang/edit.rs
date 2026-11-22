//! Edits of a model program as exact source splices: a value site's literal, one
//! field of a part's builder call, a statement added, replaced, removed or
//! renamed, a root call's argument. Every edit goes through the Splash syntax tree's structural
//! edits (only the bytes that change are replaced, the result must parse with
//! the intended shape); nothing regenerates the program's text.

use super::protos::Proto;
use super::{Arg, Program, Root, Solid};
use makepad_script::syntax::{self, At, Kind, Loc, Node, Syntax};
use std::ops::Range;

/// One edit of a program.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// A value site to Splash text (`80mm`, `true`, `@xy`): a param's name, a solid's field
    /// path (`bracket.radius`, `hole.at.1`), or an argument of a numbered statement
    /// (`fill#2.material`).
    Set { site: String, value: String },
    /// Set (or add) one field of a solid's object to Splash code.
    Field { solid: String, field: String, code: String },
    /// Remove one field of a solid's object.
    Unset { solid: String, field: String },
    /// Insert a statement (`let lid = m.box({..})`) before a named solid's statement,
    /// or before the first root call.
    Add { code: String, before: Option<String> },
    /// Replace a solid's code (its node) with other code; for a top-level `let` that names a
    /// value and no solid, its expression (`replace(width, param(30mm, ..))`).
    Replace { solid: String, code: String },
    /// Add an operand (`code`, a name or a call) to the end of a boolean's second operand list
    /// (a subtract's cutters): `m.subtract(body, [holes bores])` takes `[holes bores lightening]`;
    /// a single operand becomes a list, a name of a list takes it in that list.
    Operand { solid: String, code: String },
    /// Remove a solid and its uses: a named one's statement, an inline one's written code, and
    /// every place it is used (see [`remove`]); a numbered statement (`fill#2`).
    Remove { solid: String },
    /// Rename a named solid's `let` and every use of the name.
    Rename { solid: String, to: String },
    /// Move a solid to a sibling's slot (an operand list or top-level statements), or a numbered
    /// statement to another's (`fill#3` to `fill#1`'s).
    Move { solid: String, to: String },
    /// Set (or add) an argument of the root call that takes this solid
    /// (`place(bracket, at: ...)`); `code` none removes it.
    RootArg { kind: String, solid: String, name: String, code: Option<String> },
    /// Add a root call for a solid (`place(lid)`), or remove the one it has.
    Root { kind: String, solid: String, add: bool },
    /// Replace bytes of the source (typing in the code editor).
    Splice { range: Range<usize>, text: String },
    /// Replace the whole program.
    Program { source: String },
}

impl Edit {
    /// Needs the evaluated program (it names solids).
    pub fn names_solids(&self) -> bool {
        !matches!(self, Edit::Splice { .. } | Edit::Program { .. } | Edit::Add { .. })
    }
}

fn e(message: impl Into<String>) -> String {
    message.into()
}

/// The address of the node written at exactly `span` (the outermost one). A node's span holds
/// its children's, so only the children holding `span` are entered: the path down to it, not a
/// walk of the whole file.
pub fn loc_at(syntax: &Syntax, span: &Range<usize>) -> Option<Loc> {
    fn find(node: &Node, span: &Range<usize>, path: &mut Vec<usize>) -> bool {
        if node.span == *span && !matches!(node.kind, Kind::Block(_)) {
            return true;
        }
        let (mut i, mut found) = (0, false);
        node.for_each_child(&mut |child| {
            if !found && child.span.start <= span.start && span.end <= child.span.end {
                path.push(i);
                found = find(child, span, path);
                if !found {
                    path.pop();
                }
            }
            i += 1;
        });
        found
    }
    let mut path = Vec::new();
    find(syntax.root(), span, &mut path).then_some(Loc(path))
}

/// The top-level statement that binds `name`.
fn let_stmt(syntax: &Syntax, name: &str) -> Option<usize> {
    syntax.statements().iter().position(|s| matches!(&s.kind, Kind::Let { names, .. } if names.len() == 1 && names[0] == name))
}

/// The value a top-level `let` of `name` is written with.
fn let_value(syntax: &Syntax, name: &str) -> Option<Loc> {
    let at = Loc(vec![let_stmt(syntax, name)?, 0]);
    syntax.node(&at).map(|_| at)
}

/// The name a top-level statement defines: `let name = ..` or `fn name(..) {..}`.
fn defined(n: &Node) -> Option<&str> {
    match &n.kind {
        Kind::Let { names, .. } if names.len() == 1 => Some(&names[0]),
        Kind::Fn { name: Some(name), .. } => Some(name),
        _ => None,
    }
}

/// Whether `n` reads `name`: an identifier, or a path starting with it (`lock`, `lock.x`).
fn reads(n: &Node, name: &str) -> bool {
    if let Kind::Ident(id) = &n.kind {
        if id.strip_prefix(name).is_some_and(|rest| rest.is_empty() || rest.starts_with('.')) {
            return true;
        }
    }
    let mut found = false;
    n.for_each_child(&mut |child| found = found || reads(child, name));
    found
}

/// The top-level statement that defines `name` (`let` or `fn`).
fn def_stmt(syntax: &Syntax, name: &str) -> Option<usize> {
    syntax.statements().iter().position(|s| defined(s) == Some(name))
}

/// The top-level statements that call `kind` (`m.fill(..)`, or `fill(..)` after a `use`), in
/// script order: what `{kind}#{n}` names (`fill#2`, the second). Fills paint in this order, each
/// over the ones before it.
fn kind_statements(syn: &Syntax, kind: &str) -> Vec<usize> {
    syn.statements().iter().enumerate().filter(|(_, s)| s.callee_path().is_some_and(|c| c.rsplit('.').next() == Some(kind))).map(|(i, _)| i).collect()
}

/// The statement `{kind}#{n}` names (`fill#2`), when `id` is written so: a call written as a
/// statement of its own, which has no name to reach it by.
fn numbered(syn: &Syntax, id: &str) -> Option<Result<usize, String>> {
    let (kind, n) = id.split_once('#')?;
    let n: usize = n.parse().ok()?;
    if !valid_name(kind) {
        return None;
    }
    let all = kind_statements(syn, kind);
    Some(n.checked_sub(1).and_then(|i| all.get(i).copied()).ok_or_else(|| e(format!("no {id}: the file has {} `{kind}(..)` statement(s), {kind}#1 the first", all.len()))))
}

/// How edits name top-level statement `i` when it is a call (`fill#2`).
fn statement_id(syn: &Syntax, i: usize) -> Option<String> {
    let kind = syn.statements().get(i)?.callee_path()?.rsplit('.').next()?;
    let n = kind_statements(syn, kind).iter().position(|&j| j == i)? + 1;
    Some(format!("{kind}#{n}"))
}

/// A root call written as a statement of its own (`m.fill(figure, cream)`), as edits name it.
pub struct Numbered<'p> {
    /// `{kind}#{n}`, the n-th statement of its kind in script order: `fill#2`.
    pub id: String,
    /// What it made: its arguments, evaluated.
    pub root: &'p Root,
    /// Its arguments as written, by the root's names (`region`, `material`): each one's bytes.
    pub args: Vec<(String, Range<usize>)>,
}

/// The root call statement `i` made, and its arguments as written by the root's names (an
/// entry's value, else the positional argument in the root's order).
fn root_statement(p: &Program, i: usize) -> Option<(&Root, Vec<(String, Loc)>)> {
    let s = p.names.syntax.statements().get(i)?;
    let Kind::Call { args, open, .. } = &s.kind else { return None };
    let root = p.roots.iter().find(|r| r.loc.and_then(|l| p.names.offset(l)).is_some_and(|at| at == *open || at == s.span.start))?;
    let positional: Vec<usize> = (0..args.len()).filter(|&j| !matches!(args[j].kind, Kind::Entry { .. })).collect();
    let mut out = Vec::new();
    for (k, (name, _)) in root.args.iter().enumerate() {
        let entry = args.iter().position(|a| matches!(&a.kind, Kind::Entry { name: n, .. } if n == name));
        // Call children: the callee, then the arguments; an entry's child is its value.
        match (entry, positional.get(k)) {
            (Some(j), _) => out.push((name.clone(), Loc(vec![i, j + 1, 0]))),
            (None, Some(&j)) => out.push((name.clone(), Loc(vec![i, j + 1]))),
            (None, None) => {}
        }
    }
    Some((root, out))
}

/// The calls of root `kind` written as statements of their own, in script order (fills: the
/// order they paint in, the last over the others).
pub fn numbered_roots<'p>(p: &'p Program, kind: &str) -> Vec<Numbered<'p>> {
    let syn = &p.names.syntax;
    kind_statements(syn, kind)
        .into_iter()
        .enumerate()
        .filter_map(|(n, i)| {
            let (root, args) = root_statement(p, i)?;
            let args = args.into_iter().filter_map(|(name, at)| Some((name, syn.node(&at)?.span.clone()))).collect();
            Some(Numbered { id: format!("{kind}#{}", n + 1), root, args })
        })
        .collect()
}

/// The bytes of argument `name` of the root call statement `{kind}#{n}`, or of a field or element
/// inside it (`fill#2.material`, `stroke#1.points.2`, `stroke#1.radius`): any code goes there.
fn numbered_arg(p: &Program, id: &str, path: &str) -> Option<Result<Range<usize>, String>> {
    let i = match numbered(&p.names.syntax, id)? {
        Ok(i) => i,
        Err(x) => return Some(Err(x)),
    };
    let steps = syntax::Step::parse_path(path);
    let Some(syntax::Step::Field(name)) = steps.first() else { return Some(Err(e(format!("'{id}.{path}' names no argument")))) };
    let Some((_, args)) = root_statement(p, i) else { return Some(Err(e(format!("'{id}' made nothing: its arguments have no names")))) };
    let Some((_, at)) = args.iter().find(|(n, _)| n == name) else {
        let names: Vec<&str> = args.iter().map(|(n, _)| n.as_str()).collect();
        return Some(Err(e(format!("'{id}' has no argument '{name}' written (it has {})", names.join(", ")))));
    };
    let syn = &p.names.syntax;
    Some(syn.at_path(at, &steps[1..]).and_then(|l| syn.node(&l)).map(|n| n.span.clone()).ok_or_else(|| e(format!("'{id}.{path}' is not written in the program"))))
}

/// The first top-level statement that reads `name`, besides one defining it.
fn first_reader(syntax: &Syntax, name: &str) -> Option<usize> {
    syntax.statements().iter().position(|s| defined(s) != Some(name) && reads(s, name))
}

/// The name the statement `code` defines (`let lock = ..`, `fn lock(a) {..}`), if it is one.
pub fn defines(code: &str) -> Option<String> {
    let syn = syntax::parse(code).ok()?;
    syn.statements().first().and_then(defined).map(str::to_string)
}

/// `source` with the statement `code` added where it can work, read from the syntax alone (no
/// evaluation: what it is added for may not evaluate without it): a `use` after the file's other
/// `use`s (first when it has none); with `before`, before the statement that defines that name;
/// a `let` or `fn` before the first statement that reads its name; else before the first root
/// call (`roots`: their names), else at the end.
pub fn add(source: &str, code: &str, before: Option<&str>, roots: &[&str]) -> Result<String, String> {
    let syn = syntax::parse(source).map_err(|x| x.message)?;
    let added = syntax::parse(code).map_err(|x| format!("`{}` does not parse: {}", code.trim(), x.message))?;
    let statements = syn.statements();
    let first_root = || statements.iter().position(|s| s.as_call().is_some_and(|(callee, _)| roots.contains(&callee)));
    let index = match (before, added.statements().first().map(|s| (&s.kind, defined(s)))) {
        (_, Some((Kind::Use { .. }, _))) => statements.iter().rposition(|s| matches!(s.kind, Kind::Use { .. })).map_or(0, |i| i + 1),
        (Some(name), _) => def_stmt(&syn, name).ok_or_else(|| e(format!("no statement `let {name}` or `fn {name}`")))?,
        (None, Some((_, Some(name)))) => first_reader(&syn, name).or_else(first_root).unwrap_or(statements.len()),
        _ => first_root().unwrap_or(statements.len()),
    };
    let at = if index >= statements.len() { At::End } else { At::Before(index) };
    syn.insert_statement(&Loc::default(), at, code).map(|x| x.syntax.source().to_string()).map_err(|x| x.message)
}

/// `source` with each of the statements that define `names` moved before the first statement
/// that reads its name, when that one comes first: what a batch adds for a statement it edits
/// after adding it stays where that statement can read it.
pub fn place_defs(source: &str, names: &[String]) -> Result<String, String> {
    let mut syn = syntax::parse(source).map_err(|x| x.message)?;
    // each move takes a statement up: past every name's reader once at most
    for _ in 0..=names.len() {
        let late = names.iter().find_map(|name| {
            let at = def_stmt(&syn, name)?;
            first_reader(&syn, name).filter(|r| *r < at).map(|r| (at, r))
        });
        let Some((at, reader)) = late else { break };
        let text = syn.statements()[at].text(syn.source()).to_string();
        let removed = syn.remove_element(&Loc::default(), at).map_err(|x| x.message)?.syntax;
        syn = removed.insert_statement(&Loc::default(), At::Before(reader), &text).map_err(|x| x.message)?.syntax;
    }
    Ok(syn.source().to_string())
}

/// `names` joined, as many as a few hundred bytes hold, then how many that leaves out: a script
/// can make thousands.
fn listed(names: &[String]) -> String {
    let mut room = 240;
    let shown = names
        .iter()
        .take_while(|name| {
            let fits = name.len() + 2 <= room;
            room = room.saturating_sub(name.len() + 2);
            fits
        })
        .count();
    let left = names.len() - shown;
    match (shown, left) {
        (_, 0) => names.join(", "),
        (0, _) => format!("{left} more"),
        _ => format!("{}, {left} more", names[..shown].join(", ")),
    }
}

/// The refusal for a name no solid has: the names it could have meant, in a few words. A list's
/// parts go by the list (`beads[..]`), the parts a line makes in a loop share one id (`box@149`)
/// and come last.
fn no_solid(p: &Program, id: &str) -> String {
    let (mut solids, mut anonymous) = (Vec::new(), Vec::new());
    for s in &p.solids {
        let name = s.id.split_once('[').map_or_else(|| s.id.clone(), |(list, _)| format!("{list}[..]"));
        let names = if s.id.contains('@') { &mut anonymous } else { &mut solids };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    solids.append(&mut anonymous);
    // The `let`s that hold a value: no part or list of parts, no module.
    let is_value = |name: &str, value: &Node| !value.ident().is_some_and(|i| i.starts_with("mod.")) && !p.solids.iter().any(|s| s.id.strip_prefix(name).is_some_and(|rest| rest.is_empty() || rest.starts_with('[')));
    let values: Vec<String> = p
        .names
        .syntax
        .statements()
        .iter()
        .filter_map(|st| match &st.kind {
            Kind::Let { names, value: Some(value), .. } if names.len() == 1 && is_value(&names[0], value) => Some(names[0].clone()),
            _ => None,
        })
        .collect();
    e(format!("no solid '{id}' (solids: {}; values replace(name, code) takes: {}; fab.tree lists the solids, fab.params the params)", listed(&solids), listed(&values)))
}

fn solid<'p>(p: &'p Program, id: &str) -> Result<&'p Solid, String> {
    p.solid(id).ok_or_else(|| no_solid(p, id))
}

/// A script fn's part that its call's `let` names (`let bow = bow_loop(..)`): that `let` is the
/// part's, as a named part's own.
fn is_call_value(s: &Solid) -> bool {
    s.call.as_ref().is_some_and(|c| c.value)
}

/// The refusal for a part made on the way inside a script fn: its code is every call's.
fn fn_made(p: &Program, s: &Solid) -> String {
    let call = s.call.as_ref().map_or("", |c| &p.source()[c.span.clone()]);
    let callee = call.split('(').next().unwrap_or(call).trim();
    e(format!("'{}' is made inside {callee}(..), whose code every call of it shares: edit the part the call makes, or {callee} itself", s.id))
}

/// The fn whose code holds `loc`: the nearest one written round it.
fn enclosing_fn<'s>(syn: &'s Syntax, loc: &Loc) -> Option<&'s Node> {
    let mut at = loc.parent();
    while let Some(l) = at {
        let node = syn.node(&l)?;
        if matches!(node.kind, Kind::Fn { .. }) {
            return Some(node);
        }
        at = l.parent();
    }
    None
}

/// A loop is written round `loc` inside its fn (or the file): the code there runs once a pass.
fn in_loop(syn: &Syntax, loc: &Loc) -> bool {
    let mut at = loc.parent();
    while let Some(l) = at {
        match syn.node(&l).map(|n| &n.kind) {
            Some(Kind::For { .. } | Kind::While { .. } | Kind::Loop { .. }) => return true,
            Some(Kind::Fn { .. }) | None => return false,
            _ => at = l.parent(),
        }
    }
    false
}

/// The name a call calls (`bowLoop`).
fn callee(syn: &Syntax, call: &Loc) -> String {
    syn.node(call).and_then(Node::callee_path).unwrap_or("its fn").to_string()
}

/// The argument a call passes for its fn's parameter `index` (arguments by position).
fn passed(syn: &Syntax, call: &Loc, index: usize) -> Option<Loc> {
    let Kind::Call { args, .. } = &syn.node(call)?.kind else { return None };
    (index < args.len() && !args.iter().any(|a| matches!(a.kind, Kind::Entry { .. }))).then(|| call.child(index + 1))
}

/// The calls a part a fn makes came from, outermost first ([`FnCall`]): none for another part.
fn calls_of(syn: &Syntax, s: &Solid) -> Option<Vec<Loc>> {
    let Some(call) = &s.call else { return Some(Vec::new()) };
    std::iter::once(&call.span).chain(&call.inner).map(|r| loc_at(syn, r)).collect()
}

/// The fn whose code makes a part a fn makes: what its innermost call calls.
fn maker(p: &Program, s: &Solid) -> String {
    let syn = &p.names.syntax;
    calls_of(syn, s).and_then(|c| c.last().map(|l| callee(syn, l))).unwrap_or_else(|| "its fn".into())
}

/// The refusal for a value a script fn writes itself: every call of the fn shares it.
fn fn_value(site: &str, f: &str) -> String {
    e(format!("'{site}' is the code of {f}(..), which every call of it shares: make it a parameter of {f} and pass the value at the call"))
}

/// Where the value `steps` below the code at `at` is written for part `s` alone. A script fn's
/// code is every call's, so for a part a fn makes (`inside`: `at` is in the code of the fn that
/// makes it) a parameter of the fn is followed out to the argument its call passes, call by call,
/// to the call written at the top level; a value the fn writes itself is refused, and so is a
/// top-level call in a loop, whose arguments every pass shares.
fn follow(p: &Program, s: &Solid, mut at: Loc, mut steps: &[syntax::Step], inside: bool, site: &str) -> Result<Loc, String> {
    let syn = &p.names.syntax;
    let unwritten = || e(format!("'{site}' is not written in the program"));
    let calls = calls_of(syn, s).ok_or_else(unwritten)?;
    let mut frame = if inside { calls.len() } else { 0 };
    loop {
        while let Some((step, rest)) = steps.split_first() {
            let Some(next) = syn.at_path(&at, std::slice::from_ref(step)) else { break };
            (at, steps) = (next, rest);
        }
        if frame == 0 {
            if let Some(top) = calls.first().filter(|c| in_loop(syn, c)) {
                let f = callee(syn, top);
                return Err(e(format!("'{site}' comes from a call of {f}(..) in a loop, whose arguments every pass shares: write a call of its own for a part that differs")));
            }
            return if steps.is_empty() { Ok(at) } else { Err(unwritten()) };
        }
        let call = &calls[frame - 1];
        let f = callee(syn, call);
        let param = match (syn.node(&at).map(|n| &n.kind), enclosing_fn(syn, &at).map(|n| &n.kind)) {
            (Some(Kind::Ident(name)), Some(Kind::Fn { name: Some(fun), params, .. })) if *fun == f => params.iter().position(|q| q.name == *name),
            _ => None,
        };
        at = param.and_then(|i| passed(syn, call, i)).ok_or_else(|| fn_value(site, &f))?;
        frame -= 1;
    }
}

/// A part a fn makes: its top-level call's argument for the fn's parameter `name`.
fn call_param(p: &Program, s: &Solid, name: &str) -> Option<Loc> {
    let syn = &p.names.syntax;
    let call = loc_at(syn, &s.call.as_ref()?.span)?;
    let f = syn.node(&call)?.callee_path()?;
    let params = syn.statements().iter().find_map(|st| match &st.kind {
        Kind::Fn { name: Some(n), params, .. } if n == f => Some(params),
        _ => None,
    })?;
    passed(syn, &call, params.iter().position(|q| q.name == name)?)
}

/// Where field path `path` of part `s` is written for it alone (see [`follow`]). A part a fn
/// makes names its call's arguments by the fn's parameters too (`bow.len`), where its builder
/// writes no field of that name.
fn part_site(p: &Program, s: &Solid, path: &str, site: &str) -> Result<Loc, String> {
    let steps = syntax::Step::parse_path(path);
    let Some(syntax::Step::Field(first)) = steps.first() else { return Err(e(format!("'{site}' names no field"))) };
    match field_at(p, s, first) {
        Ok(Some(field)) => follow(p, s, field.loc().clone(), &steps[1..], true, site),
        found => match call_param(p, s, first) {
            Some(arg) => follow(p, s, arg, &steps[1..], false, site),
            None if s.call.is_some() => Err(fn_value(site, &maker(p, s))),
            None => found.and_then(|_| Err(e(format!("'{site}' is not written in the program")))),
        },
    }
}

/// The value field path `path` of part `s` is written as for it alone: in its builder call, or
/// for a part a fn makes, what its call passes ([`follow`]).
pub fn part_value<'p>(p: &'p Program, s: &Solid, path: &str) -> Result<&'p Node, String> {
    let at = part_site(p, s, path, &format!("{}.{path}", s.id))?;
    p.names.syntax.node(&at).ok_or_else(|| e(format!("'{}.{path}' has no node", s.id)))
}

/// A part a fn makes: the arguments its top-level call passes, by the fn's parameter names (none
/// when the call runs in a loop, its arguments every pass's).
pub fn call_args<'p>(p: &'p Program, s: &Solid) -> Vec<(&'p str, &'p Node)> {
    let syn = &p.names.syntax;
    let Some(call) = s.call.as_ref().and_then(|c| loc_at(syn, &c.span)).filter(|c| !in_loop(syn, c)) else { return Vec::new() };
    let Some(f) = syn.node(&call).and_then(Node::callee_path) else { return Vec::new() };
    let Some(params) = syn.statements().iter().find_map(|st| match &st.kind {
        Kind::Fn { name: Some(n), params, .. } if n == f => Some(params),
        _ => None,
    }) else {
        return Vec::new();
    };
    params.iter().enumerate().filter_map(|(i, q)| Some((q.name.as_str(), syn.node(&passed(syn, &call, i)?)?))).collect()
}

/// A part a fn makes: its top-level call, written short (`bow = bowLoop(..)`, or
/// `bowLoop(..) on line 12` when no `let` names it alone).
pub fn call_label(p: &Program, s: &Solid) -> Option<String> {
    let syn = &p.names.syntax;
    let span = &s.call.as_ref()?.span;
    let f = syn.node(&loc_at(syn, span)?)?.callee_path()?;
    let named = syn.statements().iter().find_map(|st| match &st.kind {
        Kind::Let { names, value: Some(v), .. } if names.len() == 1 && v.span == *span => Some(names[0].as_str()),
        _ => None,
    });
    Some(match named {
        Some(name) => format!("{name} = {f}(..)"),
        None => format!("{f}(..) on line {}", syn.line_col(span.start).0),
    })
}

/// A part's builder call (its loc), when it is written as one.
pub fn part_call(p: &Program, s: &Solid) -> Result<Loc, String> {
    let span = s.span.clone().ok_or_else(|| e(format!("'{}' is not written as one call", s.id)))?;
    let loc = loc_at(&p.names.syntax, &span).ok_or_else(|| e(format!("'{}' has no node", s.id)))?;
    match p.names.syntax.node(&loc).map(|n| &n.kind) {
        Some(Kind::Call { .. }) => Ok(loc),
        _ => Err(e(format!("'{}' is written as `{}`, not a builder call", s.id, &p.source()[span]))),
    }
}

/// Where a field of a builder call is written.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldAt {
    /// The call's argument (`m.fillet(body, 1mm)`, or `radius: 1mm`): its value's loc.
    Arg(Loc),
    /// A field of the options object.
    Opt(Loc),
}

impl FieldAt {
    pub fn loc(&self) -> &Loc {
        match self {
            FieldAt::Arg(l) | FieldAt::Opt(l) => l,
        }
    }
}

/// The call's options object (`{..}` among its arguments).
pub fn options_loc(syn: &Syntax, call: &Loc) -> Option<Loc> {
    let Kind::Call { args, .. } = &syn.node(call)?.kind else { return None };
    args.iter().position(|a| matches!(&a.kind, Kind::Object { ty: None, .. })).map(|i| call.child(i + 1))
}

/// Every field a builder call writes, in source order: its name, where it is.
pub fn call_fields(syn: &Syntax, call: &Loc, proto: &Proto) -> Vec<(String, FieldAt)> {
    let mut out = Vec::new();
    let Some(Kind::Call { args, .. }) = syn.node(call).map(|n| &n.kind) else { return out };
    let mut positional = proto.args.iter();
    for (i, a) in args.iter().enumerate() {
        match &a.kind {
            Kind::Entry { name, .. } => out.push((name.clone(), FieldAt::Arg(call.child(i + 1).child(0)))),
            Kind::Object { ty: None, fields, .. } => {
                for (j, f) in fields.iter().enumerate() {
                    if let Some(name) = &f.name {
                        out.push((name.clone(), FieldAt::Opt(call.child(i + 1).child(j))));
                    }
                }
            }
            _ => {
                if let Some(name) = positional.next() {
                    out.push((name.to_string(), FieldAt::Arg(call.child(i + 1))));
                }
            }
        }
    }
    out
}

fn field_at(p: &Program, s: &Solid, name: &str) -> Result<Option<FieldAt>, String> {
    let call = part_call(p, s)?;
    let proto = super::protos::proto(s.proto).ok_or_else(|| e("not a part"))?;
    Ok(call_fields(&p.names.syntax, &call, proto).into_iter().find(|(n, _)| n == name).map(|(_, at)| at))
}

/// A value written as itself: a number (`-` one too), a name (`@xy`), a colour, a string, an
/// identifier that is no param, or a list of these (a vector, a list of points).
fn is_literal(p: &Program, node: &Node) -> bool {
    match &node.kind {
        Kind::Number(_) | Kind::Name(_) | Kind::Color(_) | Kind::Str(_) => true,
        Kind::Unary { op, operand } => op == "-" && operand.number().is_some(),
        Kind::Ident(name) => !p.params.iter().any(|q| q.name.as_deref() == Some(name.as_str())),
        Kind::Array { items, .. } => items.iter().all(|i| is_literal(p, i)),
        _ => false,
    }
}

/// The outer names an expression reads (a fn's own parameters and lets are its own), in order.
fn names_read(node: &Node) -> Vec<String> {
    let (mut own, mut used) = (Vec::<String>::new(), Vec::<String>::new());
    node.walk(&Loc::default(), &mut |_, n| match &n.kind {
        Kind::Let { names, .. } => own.extend(names.iter().cloned()),
        Kind::Fn { params, .. } => own.extend(params.iter().map(|q| q.name.clone())),
        _ => {
            if let Some(path) = n.ident() {
                let first = path.split('.').next().unwrap_or(path).to_string();
                if !used.contains(&first) {
                    used.push(first);
                }
            }
        }
    });
    used.retain(|u| !own.contains(u));
    used
}

/// Why the value at `node` (site `site`, of `solid`'s `field`) is no site, and how to set it:
/// the expression that computes it, the params and names it reads, and what sets each.
fn computed(p: &Program, site: &str, node: &Node, solid: &str, field: &str) -> String {
    let src = p.source();
    let syn = &p.names.syntax;
    let input = |name: &str| -> Option<String> {
        if p.params.iter().any(|q| q.name.as_deref() == Some(name)) {
            return Some(format!("the param {name}: set({name}, ..)"));
        }
        let i = let_stmt(syn, name)?;
        let (line, _) = syn.line_col(syn.statements()[i].span.start);
        Some(format!("`let {name}` on line {line}"))
    };
    let inputs = |n: &Node| names_read(n).iter().filter_map(|u| input(u)).collect::<Vec<_>>();
    let rewrite = format!("field({solid}, {field}, ..) writes the field over");
    // A list: which of its elements are computed, which are literals set one by one.
    if let Kind::Array { items, .. } = &node.kind {
        let mut computed_items = Vec::new();
        let mut literal_at = None;
        for (k, item) in items.iter().enumerate() {
            if is_literal(p, item) {
                literal_at.get_or_insert(k);
                continue;
            }
            let from = match &item.kind {
                Kind::Ident(name) => input(name).unwrap_or_default(),
                _ => inputs(item).join(", "),
            };
            computed_items.push(format!("element {k} is `{}`{}", item.text(src), if from.is_empty() { String::new() } else { format!(" ({from})") }));
        }
        let one = literal_at.map_or(String::new(), |k| format!("; set a literal element alone: set({site}.{k}, ..)"));
        return e(format!("'{site}' (`{}`) is computed: {}{one}; or {rewrite}", node.text(src), computed_items.join(", ")));
    }
    let from = inputs(node);
    let from = if from.is_empty() { String::new() } else { format!(", reading {}", from.join(", ")) };
    e(format!("'{site}' is computed by `{}`{from}: change what it reads, or {rewrite}", node.text(src)))
}

/// Where a selector a B-rep step lost is written ([`selector_site`]) and how a pick is written
/// there.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectorSite {
    /// Its bytes: the `m.edge(..)` call or the face (`b.top`, `b.side(2)`) as written.
    pub span: Range<usize>,
    /// Written in the code of a script fn the solid is made in: that fn. Every call of it takes
    /// what is written there.
    pub shared: Option<String>,
    /// Written in a script fn's code: the name that code has for each part a pick may name there
    /// (the part's id, its name there). None: written at the top level, where a part's id is its
    /// name.
    pub names: Option<Vec<(String, String)>>,
}

/// Where selector `index` of solid `id`'s B-rep step is written (its named edges, or the faces it
/// leaves open; [`Solid::selectors`]), to name its edge or face again. A face its part made with
/// it (`b.top`) is found in the step's field as written, alone or in a list, or in the list a
/// top-level `let` binds when the field names it. In the code of a script fn the solid is made in
/// (its body, or a fn it calls on the way), a pick's parts are written with that code's names:
/// its own `let`s and its parameters, followed out to what the call passes.
pub fn selector_site(p: &Program, id: &str, index: usize) -> Result<SelectorSite, String> {
    let s = solid(p, id)?;
    let gone = || e(format!("`{id}` has no selector {index} written where it can be named again: name a new one in the code"));
    let syn = &p.names.syntax;
    let at: Loc = match s.selectors.get(index) {
        // its own call (`m.edge(..)`, `r.face(..)`): the outermost node there, a call opening or a
        // path beginning at it
        Some(Some(loc)) => {
            let at = p.names.offset(*loc).ok_or_else(gone)?;
            let mut found: Option<Loc> = None;
            syn.walk(&mut |l, node| {
                if found.is_none() && (node.span.start == at || matches!(&node.kind, Kind::Call { open, .. } if *open == at)) {
                    found = Some(l.clone());
                }
            });
            found.ok_or_else(gone)?
        }
        Some(None) => {
            let field = if s.proto == "BrepShell" { "open" } else { "edges" };
            let mut list = field_at(p, s, field)?.ok_or_else(gone)?.loc().clone();
            // a name: the list its top-level `let` binds (a fn's own name is its own)
            if let Some(Kind::Ident(name)) = syn.node(&list).map(|n| &n.kind).filter(|k| matches!(k, Kind::Ident(n) if !n.contains('.'))) {
                if enclosing_fn(syn, &list).is_some_and(|f| !names_read(f).contains(name)) {
                    return Err(gone());
                }
                let value = syn.statements().iter().find_map(|st| match &st.kind {
                    Kind::Let { names, value: Some(v), .. } if names.len() == 1 && names[0] == *name => Some(v.span.clone()),
                    _ => None,
                });
                list = value.and_then(|v| loc_at(syn, &v)).ok_or_else(gone)?;
            }
            let node = syn.node(&list).ok_or_else(gone)?;
            match &node.kind {
                Kind::Array { items, .. } if items.len() == s.selectors.len() => loc_at(syn, &items[index].span).ok_or_else(gone)?,
                Kind::Array { .. } => return Err(gone()),
                _ if index == 0 && s.selectors.len() == 1 => list,
                _ => return Err(gone()),
            }
        }
        None => return Err(gone()),
    };
    let span = syn.node(&at).ok_or_else(gone)?.span.clone();
    let Some(Kind::Fn { name: Some(fun), .. }) = enclosing_fn(syn, &at).map(|f| &f.kind) else {
        return Ok(SelectorSite { span, shared: None, names: None });
    };
    // the call of that fn the solid came from: its code names parts as that call runs it
    let calls = calls_of(syn, s).ok_or_else(gone)?;
    let Some(j) = calls.iter().rposition(|c| callee(syn, c) == *fun) else {
        return Err(e(format!("`{id}`'s selector is written in {fun}(..), which `{id}` is not made in: name a new one in the code")));
    };
    let mut names: Vec<(String, String)> = fn_scope(p, s, &calls, j).into_iter().map(|(name, part)| (part, name)).collect();
    names.sort();
    names.dedup_by(|a, b| a.0 == b.0);
    Ok(SelectorSite { span, shared: Some(fun.clone()), names: Some(names) })
}

/// The parts the code of `s`'s fn call `j` (of `calls`, outermost first) has names for, by name
/// (the name, the part's id): the parts made there that its `let`s name, and its parameters a call
/// passes a part for by name, followed out call by call to the top level's named parts.
fn fn_scope(p: &Program, s: &Solid, calls: &[Loc], j: usize) -> Vec<(String, String)> {
    let syn = &p.names.syntax;
    let frames = |t: &Solid| t.call.as_ref().map(|c| std::iter::once(&c.span).chain(&c.inner).cloned().collect::<Vec<_>>()).unwrap_or_default();
    let mine = frames(s);
    let mut out: Vec<(String, String)> = Vec::new();
    for t in p.solids.iter().filter(|t| mine.len() > j && frames(t)[..] == mine[..=j]) {
        if let Some(bind) = p.names.site(t.loc).filter(|site| site.owns).and_then(|site| site.binds.last()).filter(|b| !b.looped) {
            out.push((bind.name.clone(), t.id.clone()));
        }
    }
    let f = callee(syn, &calls[j]);
    let params = syn.statements().iter().find_map(|st| match &st.kind {
        Kind::Fn { name: Some(n), params, .. } if *n == f => Some(params),
        _ => None,
    });
    let outer = if j == 0 { p.solids.iter().filter(|t| (t.call.is_none() && t.named) || is_call_value(t)).map(|t| (t.id.clone(), t.id.clone())).collect() } else { fn_scope(p, s, calls, j - 1) };
    for (i, q) in params.into_iter().flatten().enumerate() {
        let arg = passed(syn, &calls[j], i).and_then(|l| syn.node(&l)).and_then(|n| match &n.kind {
            Kind::Ident(name) => Some(name.as_str()),
            _ => None,
        });
        if let Some((_, part)) = arg.and_then(|a| outer.iter().find(|(name, _)| name == a)) {
            out.retain(|(name, _)| *name != q.name);
            out.push((q.name.clone(), part.clone()));
        }
    }
    out
}

/// The bytes of a value site: a param's value, or a field of a solid written as a literal (a
/// number, a name, a vector or list of them, whole or one element).
pub fn site_span(p: &Program, site: &str) -> Result<Range<usize>, String> {
    if let Some(param) = p.params.iter().find(|q| q.name.as_deref() == Some(site)) {
        return param.value_span.clone().ok_or_else(|| e(format!("param '{site}' has no written value")));
    }
    // An argument of a root call written as a statement of its own: `fill#2.material`.
    if let Some(found) = site.split_once('.').and_then(|(id, path)| numbered_arg(p, id, path)) {
        return found;
    }
    // The longest solid id that prefixes the path.
    let mut best: Option<&Solid> = None;
    for s in &p.solids {
        if site.starts_with(&s.id) && site[s.id.len()..].starts_with('.') && best.is_none_or(|b| s.id.len() > b.id.len()) {
            best = Some(s);
        }
    }
    let s = best.ok_or_else(|| e(format!("no param or part field '{site}'")))?;
    let path = &site[s.id.len() + 1..];
    let at = part_site(p, s, path, site)?;
    let node = p.names.syntax.node(&at).unwrap();
    let first = path.split('.').next().unwrap_or(path);
    // A field naming a param is that param's site.
    if let Kind::Ident(name) = &node.kind {
        if p.params.iter().any(|q| q.name.as_deref() == Some(name)) {
            return site_span(p, name);
        }
    }
    if is_literal(p, node) {
        return Ok(node.span.clone());
    }
    Err(computed(p, site, node, &s.id, first))
}

/// The root call (`place(...)`) that takes solid `index` first.
fn root_call(p: &Program, kind: &str, index: usize) -> Option<Loc> {
    let root = p.roots.iter().find(|r| r.kind == kind && r.args.first().is_some_and(|(_, a)| matches!(a, Arg::Solid(i) if *i == index)))?;
    let at = p.names.offset(root.loc?)?;
    let mut found = None;
    p.names.syntax.walk(&mut |loc, node| {
        if found.is_none() {
            if let Kind::Call { open, .. } = &node.kind {
                if *open == at || node.span.start == at {
                    found = Some(loc.clone());
                }
            }
        }
    });
    found
}

/// Uses of `name`, outside the statements of the names in `gone`.
fn uses(syntax: &Syntax, name: &str, gone: &[String]) -> usize {
    let mut n = 0;
    for (i, stmt) in syntax.statements().iter().enumerate() {
        if stmt.let_name().is_some_and(|l| gone.iter().any(|g| g == l)) {
            continue;
        }
        stmt.walk(&Loc(vec![i]), &mut |_, node| {
            if node.ident() == Some(name) {
                n += 1;
            }
        });
    }
    n
}

fn valid_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
        && syntax::parse(&format!("let {s} = 0")).is_ok_and(|syn| matches!(syn.statements(), [Node { kind: Kind::Let { names, .. }, .. }] if names.len() == 1 && names[0] == s))
}

/// The fields a boolean takes its operands in, in order: what it joins, cuts or blends.
fn operand_fields(proto: &str) -> &'static [&'static str] {
    match proto {
        "Blend" => &["of"],
        "Union" | "Difference" | "Intersect" | "Carve" | "BrepUnion" | "BrepDifference" | "BrepIntersect" => &["a", "b"],
        _ => &[],
    }
}

/// Written operands of a boolean, following named lists to their declaration.
fn operand_sites(syn: &Syntax, at: &Loc, out: &mut Vec<Loc>) {
    match syn.node(at).map(|n| &n.kind) {
        Some(Kind::Array { items, .. }) => {
            for i in 0..items.len() {
                operand_sites(syn, &at.child(i), out);
            }
        }
        Some(Kind::Ident(name)) => {
            let list = syn.find_let(name).map(|l| l.child(0)).filter(|l| matches!(syn.node(l).map(|n| &n.kind), Some(Kind::Array { .. })));
            if let Some(list) = list {
                operand_sites(syn, &list, out);
            } else {
                out.push(at.clone());
            }
        }
        Some(_) => out.push(at.clone()),
        None => {}
    }
}

/// Move through syntax nodes; references and inline calls retain their written form.
fn move_solid(p: &Program, id: &str, to: &str) -> Result<String, String> {
    let syn = &p.names.syntax;
    // A call written as a statement of its own takes another's place: `move(fill#3, fill#1)`.
    match (numbered(syn, id), numbered(syn, to)) {
        (None, None) => {}
        (Some(i), Some(j)) => return syn.move_element(&Loc::default(), i?, j?).map(|x| x.syntax.source().to_string()).map_err(|x| x.message),
        (Some(_), None) | (None, Some(_)) => return Err(e(format!("move puts a numbered statement in another one's place (move(fill#3, fill#1)): '{id}' and '{to}' are not both numbered"))),
    }
    let parent_of = |id: &str| -> Result<Option<usize>, String> {
        if let Some(s) = p.solid(id) { return Ok(s.parent); }
        let member = |name: &str| name.strip_prefix(id).is_some_and(|tail| tail.starts_with(['[', '.']));
        let mut parents = p.solids.iter().filter(|s| member(&s.id))
            .filter(|s| s.parent.is_none_or(|i| !member(&p.solids[i].id))).map(|s| s.parent);
        let parent = parents.next().ok_or_else(|| e(format!("no solid or group '{id}'")))?;
        if parents.any(|p| p != parent) { return Err(e("the group spans more than one operand list")); }
        Ok(parent)
    };
    // Operands a fn writes are every call's.
    if let Some(s) = [id, to].into_iter().filter_map(|x| p.solid(x)).find(|s| s.call.is_some() && !is_call_value(s)) {
        return Err(fn_made(p, s));
    }
    let parent = parent_of(id)?;
    if parent != parent_of(to)? {
        return Err(e("move needs two siblings"));
    }
    let Some(parent) = parent else {
        let i = let_stmt(syn, id).ok_or_else(|| e(format!("'{id}' has no statement of its own")))?;
        let j = let_stmt(syn, to).ok_or_else(|| e(format!("'{to}' has no statement of its own")))?;
        return syn.move_element(&Loc::default(), i, j).map(|x| x.syntax.source().to_string()).map_err(|x| x.message);
    };
    let par = &p.solids[parent];
    let keys = operand_fields(par.proto);
    if keys.is_empty() {
        return Err(e("this part has no operand list to reorder"));
    }
    let mut sites = Vec::new();
    for key in keys {
        if let Some(at) = field_at(p, par, key)? {
            operand_sites(syn, at.loc(), &mut sites);
        }
    }
    let position = |id: &str| -> Result<usize, String> {
        let span = p.solid(id).and_then(|s| s.span.as_ref());
        let matches: Vec<_> = sites.iter().enumerate().filter(|(_, at)| syn.node(at).is_some_and(|n| n.ident() == Some(id) || span == Some(&n.span))).map(|(i, _)| i).collect();
        match matches.as_slice() {
            [i] => Ok(*i),
            _ => Err(e(format!("'{id}' is not one written operand"))),
        }
    };
    let (i, j) = (position(id)?, position(to)?);
    let mut order: Vec<_> = (0..sites.len()).collect();
    order.remove(i);
    order.insert(j, i);
    let edits: Vec<_> = order.iter().enumerate().filter(|(slot, from)| slot != *from).map(|(slot, from)| {
        let node = syn.node(&sites[slot]).unwrap();
        syntax::edit::Replace::new(node.span.clone(), syn.node(&sites[*from]).unwrap().text(p.source()).to_string())
    }).collect();
    syn.apply(&edits).map(|x| x.source().to_string()).map_err(|x| x.message)
}

/// What holds a value of the program: where a removal takes it out of.
enum Owner {
    /// A field of a builder call (`b` of `m.subtract(..)`, `of` of `m.blend(..)`, a `source`).
    Field { call: Loc, proto: &'static Proto, key: &'static str },
    /// The first argument of a call written as a statement of its own: what `object(x)`,
    /// `m.fixed(x)` or `m.fill(x, ..)` is about.
    Subject { call: Loc },
    /// The whole value of a top-level `let` of this name.
    Let(String),
    /// A statement of the file.
    Statement,
}

/// The vocabulary's builder a call is (`m.carve`, `m.brep.subtract`, `carve` after
/// `use mod.model.*`).
fn builder(syn: &Syntax, call: &Loc) -> Option<&'static Proto> {
    let path = syn.node(call)?.callee_path()?;
    let mut steps = path.rsplit('.');
    let name = steps.next()?;
    let brep = steps.next() == Some("brep");
    super::protos::PROTOS.iter().find(|p| !p.call.is_empty() && p.call == name && (p.group == super::protos::Group::Brep) == brep)
}

fn owner(syn: &Syntax, loc: &Loc) -> Option<Owner> {
    let parent = loc.parent()?;
    let node = syn.node(&parent)?;
    match &node.kind {
        Kind::Block(_) if parent.0.is_empty() => return Some(Owner::Statement),
        Kind::Let { names, .. } if parent.0.len() == 1 && names.len() == 1 => return Some(Owner::Let(names[0].clone())),
        _ => {}
    }
    // An argument, a named argument's value, or a field of the options object.
    for call in [Some(parent.clone()), parent.parent()].into_iter().flatten() {
        let Some(proto) = builder(syn, &call) else { continue };
        if let Some((key, _)) = call_fields(syn, &call, proto).into_iter().find(|(_, at)| at.loc() == loc) {
            return proto.field(&key).map(|f| Owner::Field { call, proto, key: f.key });
        }
    }
    (matches!(node.kind, Kind::Call { .. }) && parent.0.len() == 1 && *loc == parent.child(1)).then_some(Owner::Subject { call: parent })
}

/// A delete stopped at a use it cannot take out: the line, and what to do.
fn refusal(syn: &Syntax, loc: &Loc, what: &str) -> String {
    let (line, _) = syn.line_col(syn.node(loc).map_or(0, |n| n.span.start));
    let text = syn.source().lines().nth(line - 1).unwrap_or("").trim();
    // A call statement of its own has a number to change or remove it by.
    let or = match loc.0.first().and_then(|&i| statement_id(syn, i)) {
        Some(id) => format!(", or remove that statement: remove({id})"),
        None => String::new(),
    };
    format!("{what} on line {line} (`{text}`), where a delete cannot take it out: change that line first{or}")
}

fn edit_err(x: syntax::EditError) -> String {
    x.message
}

/// The first use of a name of `gone`, outside their own statements.
fn first_use(syn: &Syntax, gone: &[String]) -> Result<Option<Loc>, String> {
    for (i, stmt) in syn.statements().iter().enumerate() {
        if stmt.let_name().is_some_and(|l| gone.iter().any(|g| g == l)) {
            continue;
        }
        let mut found: Option<Result<Loc, String>> = None;
        stmt.walk(&Loc(vec![i]), &mut |loc, node| {
            let Some(path) = node.ident().filter(|_| found.is_none()) else { return };
            if gone.iter().any(|g| g == path) {
                found = Some(Ok(loc.clone()));
            } else if let Some(g) = gone.iter().find(|g| path.strip_prefix(g.as_str()).is_some_and(|t| t.starts_with('.'))) {
                found = Some(Err(refusal(syn, loc, &format!("'{g}' is used as `{path}`"))));
            }
        });
        if let Some(f) = found {
            return f.map(Some);
        }
    }
    Ok(None)
}

/// Remove solid `id` and every use of it, as one edit. A named solid's `let` goes; an inline
/// one's written code goes. Each use is taken out of what holds it:
/// - an operand list loses it (`[mouth cut1]` keeps `[mouth]`), with its separator;
/// - a boolean left with nothing to do becomes what is left: a subtract or carve without
///   cutters its base, a union or blend of one that one ([`collapse`]);
/// - a builder without its base or source, a statement about it (`object(x)`, `m.fixed(x)`),
///   or a `let` of it goes too, and that in turn is taken out of where it is used;
/// - an optional solid field of a builder (`mesh_with`) is unset.
///
/// A use none of these take out (a member `cut1.x`, an argument of a fn of the program's own)
/// refuses the delete, naming its line.
///
/// A numbered statement (`fill#2`: a call written as a statement of its own, the n-th of its
/// kind) is that statement.
pub fn remove(p: &Program, id: &str) -> Result<String, String> {
    // A call written as a statement of its own (`fill#2`): that statement.
    if let Some(i) = numbered(&p.names.syntax, id) {
        return p.names.syntax.delete_statement(&Loc(vec![i?])).map(|x| x.syntax.source().to_string()).map_err(edit_err);
    }
    let mut syn = p.names.syntax.clone();
    let mut gone: Vec<String> = Vec::new();
    match p.solid(id).filter(|s| !s.named && !is_call_value(s)) {
        Some(s) if s.call.is_some() => return Err(fn_made(p, s)),
        Some(s) => {
            let span = s.span.clone().ok_or_else(|| e(format!("'{id}' is made by code: edit that code")))?;
            let loc = loc_at(&syn, &span).ok_or_else(|| e(format!("'{id}' has no node")))?;
            syn = take_out(syn, &loc, &mut gone)?;
        }
        None => {
            let_stmt(&syn, id).ok_or_else(|| e(format!("no statement `let {id}`")))?;
            gone.push(id.to_string());
        }
    }
    while let Some(at) = first_use(&syn, &gone)? {
        let (before, names) = (syn.source().to_string(), gone.len());
        syn = take_out(syn, &at, &mut gone)?;
        if syn.source() == before && gone.len() == names {
            return Err(refusal(&syn, &at, "a use"));
        }
    }
    for name in &gone {
        if let Some(i) = let_stmt(&syn, name) {
            syn = syn.delete_statement(&Loc(vec![i])).map_err(edit_err)?.syntax;
        }
    }
    Ok(syn.source().to_string())
}

/// Take the value at `loc` out of what holds it (see [`remove`]). A `let`'s whole value adds
/// the `let`'s name to `gone`.
fn take_out(syn: Syntax, loc: &Loc, gone: &mut Vec<String>) -> Result<Syntax, String> {
    let what = || syn.node(loc).map_or(String::new(), |n| n.text(syn.source()).to_string());
    let Some(parent) = loc.parent() else { return Err(e("the file itself is no value")) };
    match syn.node(&parent).map(|n| &n.kind) {
        // `holes[1]` goes with `holes`; `(x)` with x.
        Some(Kind::Index { .. }) if *loc == parent.child(0) => return take_out(syn, &parent, gone),
        Some(Kind::Paren { items, .. }) if items.len() == 1 => return take_out(syn, &parent, gone),
        Some(Kind::Array { items, .. }) => {
            let n = items.len();
            // The outermost of nested lists, and what holds it: a list of solids.
            let mut outer = parent.clone();
            while let Some(up) = outer.parent().filter(|u| matches!(syn.node(u).map(|n| &n.kind), Some(Kind::Array { .. }))) {
                outer = up;
            }
            let holder = owner(&syn, &outer);
            let solids = match &holder {
                Some(Owner::Field { proto, key, .. }) => proto.field(key).is_some_and(|f| f.kind == super::protos::Kind::Solids),
                Some(Owner::Subject { .. } | Owner::Let(_)) => true,
                _ => false,
            };
            if !solids {
                return Err(refusal(&syn, loc, &format!("'{}' is in a list that is no list of solids", what())));
            }
            if n == 1 {
                return take_out(syn, &parent, gone);
            }
            let index = *loc.0.last().unwrap();
            let syn = syn.remove_element(&parent, index).map_err(edit_err)?.syntax;
            // A blend of one is that one.
            return match holder {
                Some(Owner::Field { call, proto, .. }) if proto.name == "Blend" && n == 2 && parent == outer => collapse(syn, &call, &parent.child(0), gone),
                _ => Ok(syn),
            };
        }
        _ => {}
    }
    match owner(&syn, loc) {
        Some(Owner::Statement) => syn.delete_statement(loc).map(|x| x.syntax).map_err(edit_err),
        Some(Owner::Let(name)) => {
            if !gone.contains(&name) {
                gone.push(name);
            }
            Ok(syn)
        }
        Some(Owner::Subject { call }) => take_out(syn, &call, gone),
        Some(Owner::Field { call, proto, key }) => emptied(syn, &call, proto, key, gone),
        None => Err(refusal(&syn, loc, &format!("'{}' is used", what()))),
    }
}

/// Builder `call` has lost what its field `key` held.
fn emptied(syn: Syntax, call: &Loc, proto: &'static Proto, key: &str, gone: &mut Vec<String>) -> Result<Syntax, String> {
    let field = |syn: &Syntax, k: &str| call_fields(syn, call, proto).into_iter().find(|(n, _)| n == k).map(|(_, at)| at);
    let missing = |k: &str| e(format!("{}(..) writes no '{k}'", proto.call));
    match (proto.name, key) {
        // No cutters left, or no second side: the base.
        ("Difference" | "BrepDifference" | "Carve" | "Union" | "BrepUnion" | "Intersect" | "BrepIntersect", "b") => {
            let a = field(&syn, "a").ok_or_else(|| missing("a"))?;
            collapse(syn, call, a.loc(), gone)
        }
        // A union's or intersection's first gone: the rest.
        ("Union" | "BrepUnion" | "Intersect" | "BrepIntersect", "a") => {
            let b = field(&syn, "b").ok_or_else(|| missing("b"))?.loc().clone();
            match syn.node(&b).map(|n| &n.kind) {
                Some(Kind::Array { items, .. }) if items.len() >= 2 => {
                    let first = items[0].text(syn.source()).to_string();
                    let syn = syn.remove_element(&b, 0).map_err(edit_err)?.syntax;
                    let a = field(&syn, "a").ok_or_else(|| missing("a"))?;
                    syn.replace(a.loc(), &first).map(|x| x.syntax).map_err(edit_err)
                }
                Some(Kind::Array { items, .. }) if items.len() == 1 => collapse(syn, call, &b.child(0), gone),
                Some(Kind::Array { .. }) => take_out(syn, call, gone),
                Some(Kind::Ident(name)) if syn.find_let(name).and_then(|l| syn.node(&l.child(0))).is_some_and(|n| matches!(n.kind, Kind::Array { .. })) => {
                    Err(refusal(&syn, &b, &format!("the rest of this {}(..) is the list '{name}'", proto.call)))
                }
                Some(_) => collapse(syn, call, &b, gone),
                None => Err(missing("b")),
            }
        }
        // Nothing left to blend, no base to cut, no source to change: the builder goes too.
        _ if proto.field(key).is_some_and(|f| f.required) => take_out(syn, call, gone),
        // An optional solid (a gear's `mesh_with`): unset.
        _ => match field(&syn, key) {
            Some(FieldAt::Opt(at)) => {
                let opts = at.parent().unwrap();
                match syn.node(&opts).and_then(|n| n.object()) {
                    Some((_, fields)) if fields.len() == 1 => syn.remove_element(call, *opts.0.last().unwrap() - 1),
                    _ => syn.remove_element(&opts, *at.0.last().unwrap()),
                }
                .map(|x| x.syntax)
                .map_err(edit_err)
            }
            Some(FieldAt::Arg(at)) => syn.remove_element(call, at.0[call.0.len()] - 1).map(|x| x.syntax).map_err(edit_err),
            None => Err(missing(key)),
        },
    }
}

/// What an op that is gone hands on to what replaces it: its placement, its bypass and its
/// meshing cell.
const HANDED_ON: [&str; 5] = ["off", "at", "rotate", "scale", "cell"];

/// Builder `call` with only `keep` left: `keep` in its place, under the builder's name (the
/// handle the rest of the program has, the name of the object it emits). A named operand used
/// nowhere else gives its code that name where it was written (its `let` and comments stay,
/// the builder's `let` goes); otherwise its code replaces the builder's. The builder's
/// placement and bypass go onto what replaces it (refused when that cannot take them), and its
/// meshing cell when that takes one.
fn collapse(syn: Syntax, call: &Loc, keep: &Loc, gone: &[String]) -> Result<Syntax, String> {
    let proto = builder(&syn, call).ok_or_else(|| e("the op is no builder"))?;
    let handed: Vec<(&str, String)> = call_fields(&syn, call, proto)
        .into_iter()
        .filter_map(|(k, at)| HANDED_ON.iter().find(|h| **h == k).map(|h| (*h, syn.node(at.loc()).unwrap().text(syn.source()).to_string())))
        .collect();
    let keep_node = syn.node(keep).ok_or_else(|| e("the operand has no node"))?;
    let keep_text = keep_node.text(syn.source()).to_string();
    // `let name = op(base, ..)` with `base` a `let` of its own used only here.
    let named = (call.0.len() == 2 && call.0[1] == 0).then(|| syn.statements()[call.0[0]].let_name()).flatten().map(str::to_string);
    let base = keep_node.ident().and_then(|b| let_stmt(&syn, b).map(|i| (b.to_string(), i)));
    let (syn, target) = match (named, base) {
        (Some(name), Some((b, i))) if b != name && i < call.0[0] && uses(&syn, &b, gone) == 1 => {
            let Kind::Let { pattern_span, value: Some(_), .. } = &syn.statements()[i].kind else { return Err(e(format!("`let {b}` has no value"))) };
            let mut edits = vec![syntax::edit::Replace::new(pattern_span.clone(), name)];
            edits.extend(syntax::edit::remove_element_edits(syn.source(), syn.root(), call.0[0]).map_err(edit_err)?);
            (syn.apply(&edits).map_err(edit_err)?, Loc(vec![i, 0]))
        }
        _ => (syn.replace(call, &keep_text).map_err(edit_err)?.syntax, call.clone()),
    };
    let mut syn = syn;
    for (k, code) in handed {
        let Some(p) = builder(&syn, &target).filter(|p| p.field(k).is_some()) else {
            if k == "cell" {
                continue;
            }
            return Err(e(format!("the `{k}: {code}` of what is collapsed onto '{keep_text}' has nothing to go onto: take it off first")));
        };
        match call_fields(&syn, &target, p).into_iter().find(|(n, _)| n == k) {
            Some((_, at)) if syn.node(at.loc()).is_some_and(|n| n.text(syn.source()) == code) || k == "cell" => continue,
            Some(_) => return Err(e(format!("the `{k}: {code}` of what is collapsed onto '{keep_text}' meets its own `{k}`: set one of them first"))),
            None => {}
        }
        syn = match options_loc(&syn, &target) {
            Some(opts) => syn.set_field(&opts, k, &code),
            None => syn.insert_element(&target, At::End, &format!("{{{k}: {code}}}")),
        }
        .map_err(edit_err)?
        .syntax;
    }
    Ok(syn)
}

/// Apply one edit to an evaluated program: the new source.
pub fn apply(p: &Program, edit: &Edit) -> Result<String, String> {
    let syn = &p.names.syntax;
    let out = |r: Result<syntax::edit::Edited, syntax::EditError>| r.map(|x| x.syntax.source().to_string()).map_err(|x| x.message);
    match edit {
        Edit::Program { source } => Ok(source.clone()),
        Edit::Move { solid, to } => move_solid(p, solid, to),
        Edit::Splice { range, text } => {
            let s = p.source();
            if range.start > range.end || range.end > s.len() || !s.is_char_boundary(range.start) || !s.is_char_boundary(range.end) {
                return Err(e("the splice lies outside the program"));
            }
            Ok(format!("{}{}{}", &s[..range.start], text, &s[range.end..]))
        }
        Edit::Set { site, value } => {
            let span = site_span(p, site)?;
            let loc = loc_at(syn, &span).ok_or_else(|| e(format!("'{site}' has no node")))?;
            out(syn.replace(&loc, value))
        }
        Edit::Field { solid: id, field, code } => {
            let s = solid(p, id)?;
            // A fn's part: the field as its call passes it (the fn's code is every call's).
            if s.call.is_some() {
                return out(syn.replace(&part_site(p, s, field, &format!("{id}.{field}"))?, code));
            }
            let call = part_call(p, s)?;
            match (field_at(p, s, field)?, options_loc(syn, &call)) {
                (Some(at), _) => out(syn.replace(at.loc(), code)),
                (None, Some(opts)) => out(syn.set_field(&opts, field, code)),
                (None, None) => out(syn.insert_element(&call, At::End, &format!("{{{field}: {code}}}"))),
            }
        }
        Edit::Unset { solid: id, field } => {
            let s = solid(p, id)?;
            if s.call.is_some() {
                return Err(fn_value(&format!("{id}.{field}"), &maker(p, s)));
            }
            let call = part_call(p, s)?;
            match field_at(p, s, field)? {
                Some(FieldAt::Opt(_)) => out(syn.remove_field(&options_loc(syn, &call).unwrap(), field)),
                Some(FieldAt::Arg(at)) => {
                    let proto = super::protos::proto(s.proto).unwrap();
                    if proto.args.contains(&field.as_str()) && proto.field(field).is_some_and(|f| f.required) {
                        return Err(e(format!("'{field}' is needed by {}(..): replace it instead", proto.call)));
                    }
                    // A named argument: its entry is the call's element.
                    let index = at.0[call.0.len()] - 1;
                    out(syn.remove_element(&call, index))
                }
                None => Ok(p.source().to_string()),
            }
        }
        Edit::Replace { solid: id, code } => {
            let s = match (p.solid(id), let_value(syn, id)) {
                (None, Some(value)) => return out(syn.replace(&value, code)),
                _ => solid(p, id)?,
            };
            // A fn's part is its call's: the fn's code is every call's (and the new code may
            // call the fn itself, which inside it would never end).
            let span = match &s.call {
                Some(call) if call.value => call.span.clone(),
                Some(_) => return Err(fn_made(p, s)),
                None => s.span.clone().ok_or_else(|| e(format!("'{id}' is made by code: replace that code")))?,
            };
            let loc = loc_at(syn, &span).ok_or_else(|| e(format!("'{id}' has no node")))?;
            out(syn.replace(&loc, code))
        }
        Edit::Operand { solid: id, code } => {
            let s = solid(p, id)?;
            if !matches!(s.proto, "Union" | "Difference" | "Intersect") {
                return Err(e(format!("'{id}' is no union, subtract or intersect: it has no operand list")));
            }
            if s.call.is_some() {
                let f = maker(p, s);
                return Err(e(format!("'{id}''s operands are the code of {f}(..), which every call of it shares: join the part at the top level (m.union({id}, ..)), or change {f} itself")));
            }
            let at = field_at(p, s, "b")?.ok_or_else(|| e(format!("'{id}' has no written operands")))?;
            let mut list = at.loc().clone();
            if let Some(Kind::Ident(name)) = syn.node(&list).map(|n| &n.kind) {
                if let Some(l) = syn.find_let(name).map(|l| l.child(0)).filter(|l| matches!(syn.node(l).map(|n| &n.kind), Some(Kind::Array { .. }))) {
                    list = l;
                }
            }
            match syn.node(&list) {
                Some(n) if matches!(n.kind, Kind::Array { .. }) => out(syn.insert_element(&list, At::End, code)),
                Some(n) => {
                    let one = n.text(p.source()).to_string();
                    out(syn.replace(&list, &format!("[{one} {code}]")))
                }
                None => Err(e(format!("'{id}' has no written operands"))),
            }
        }
        Edit::Add { code, before } => add(p.source(), code, before.as_deref(), &p.roots.iter().map(|r| r.kind).collect::<Vec<_>>()),
        Edit::Remove { solid: id } => remove(p, id),
        Edit::Rename { solid: id, to } => {
            if let Some(s) = p.solid(id).filter(|s| !s.named && !is_call_value(s)) {
                return Err(e(format!("'{}' has no name of its own to rename", s.id)));
            }
            if !valid_name(to) || let_stmt(syn, to).is_some() {
                return Err(e(format!("'{to}' is not a free name")));
            }
            let index = let_stmt(syn, id).ok_or_else(|| e(format!("no statement `let {id}`")))?;
            let Kind::Let { pattern_span, .. } = &syn.statements()[index].kind else { unreachable!() };
            let mut edits = vec![syntax::edit::Replace::new(pattern_span.clone(), to.clone())];
            syn.walk(&mut |_, node: &Node| {
                if node.ident() == Some(id.as_str()) {
                    edits.push(syntax::edit::Replace::new(node.span.clone(), to.clone()));
                }
            });
            syn.apply(&edits).map(|x| x.source().to_string()).map_err(|x| x.message)
        }
        Edit::RootArg { kind, solid: id, name, code } => {
            let s = solid(p, id)?;
            let index = p.solids.iter().position(|x| std::ptr::eq(x, s)).unwrap();
            let call = root_call(p, kind, index).ok_or_else(|| e(format!("'{id}' has no {kind}(...) call")))?;
            let node = syn.node(&call).unwrap();
            let Kind::Call { args, .. } = &node.kind else { unreachable!() };
            let at = args.iter().position(|a| matches!(&a.kind, Kind::Entry { name: n, .. } if n == name));
            match (at, code) {
                // Call children: the callee, then the arguments; an entry's child is its value.
                (Some(i), Some(code)) => out(syn.replace(&call.child(i + 1).child(0), code)),
                (Some(i), None) => out(syn.remove_element(&call, i)),
                (None, Some(code)) => out(syn.insert_element(&call, At::End, &format!("{name}: {code}"))),
                (None, None) => Ok(p.source().to_string()),
            }
        }
        Edit::Root { kind, solid: id, add } => {
            let s = solid(p, id)?;
            let index = p.solids.iter().position(|x| std::ptr::eq(x, s)).unwrap();
            match (root_call(p, kind, index), add) {
                (Some(_), true) => Ok(p.source().to_string()),
                (None, false) => Ok(p.source().to_string()),
                (None, true) => {
                    if !s.named {
                        return Err(e(format!("'{id}' has no name: give it a `let` first")));
                    }
                    out(syn.insert_statement(&Loc::default(), At::End, &format!("{kind}({id})")))
                }
                (Some(call), false) => {
                    // The call is a statement of its own: delete it.
                    if call.0.len() != 1 {
                        return Err(e(format!("'{id}''s {kind}(...) call is inside other code")));
                    }
                    out(syn.delete_statement(&call))
                }
            }
        }
    }
}

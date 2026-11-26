//! `fab.apply`'s edits: Splash statements, read by the Splash syntax tree, each
//! one structured edit of an object script.
//!
//! ```splash
//! set(height, 80mm)                      // a value site: a param, or part.field[.i]
//! set(bolts[1].height, 6mm)
//! set(ears.ellipsoid#2.radii, [3mm 6mm 2mm])   // any key as fab.params prints it, or quoted
//! field(hole, segments, 64)              // set or add one field of a part's call
//! unset(hole, segments)
//! replace(lid, m.box({size: [60mm 4mm 40mm]}))
//! replace(width, param(30mm, min: 20mm, max: 40mm))   // a top-level `let` of a plain value
//! remove(lid)
//! rename(lid, cover)
//! after(body)                            // a following `let body = code` builds on body, otherwise additions go after body's definition in order
//! let body = m.brep.fillet(body, 2mm, {edges: m.edge(body.top, body.front)})   // what body was is now body_<kind>
//! set(fill#2.material, red)              // a fill, by number in script order (fab.params lists them)
//! remove(fill#1)
//! move(fill#3, fill#1)                   // fill#3 takes fill#1's place: fills paint in order, the last on top
//! let lid = m.box({size: [60mm 4mm 40mm]})   // any other statement is added
//! before(lid)                            // the statements added after it go before `let lid`
//! object(lid)                            // the script emits it (a statement like any other)
//! unobject(lid)                          // it no longer does
//! ```
//!
//! Where an object prints is the project's (`fab.place`), not the script's.
//!
//! Values and code are taken as the nodes they are written as (their exact
//! text), never assembled from pieces.

use fab_model::{Anchor, Edit};
use makepad_script::syntax::{self, Kind, Node};

fn arg_text<'a>(src: &'a str, n: &Node) -> &'a str {
    match &n.kind {
        Kind::Entry { value, .. } => value.text(src),
        _ => n.text(src),
    }
}

/// One argument as written. The lexer reads some paths in pieces (`size.1` is `size` then the
/// number `.1`; `hair.ellipsoid#2.radii` is a path, the colour `#2` and `.radii`), so the pieces
/// written against each other, with nothing between, are one argument.
struct Word<'n> {
    nodes: &'n [Node],
    span: std::ops::Range<usize>,
}

impl Word<'_> {
    /// As a name or a path (a site, a solid, a field), exactly as fab.params and fab.tree print
    /// it: quoted, its text.
    fn path(&self, src: &str) -> String {
        match self.nodes {
            [Node { kind: Kind::Str(text), .. }] => text.clone(),
            _ => src[self.span.clone()].chars().filter(|c| !c.is_whitespace()).collect(),
        }
    }

    /// As code: a value, a call.
    fn code<'a>(&self, src: &'a str) -> &'a str {
        match self.nodes {
            [one] => arg_text(src, one),
            _ => &src[self.span.clone()],
        }
    }

    /// A solid id, optionally introduced by a named argument such as `to:`.
    fn solid_id(&self, src: &str, keyword: Option<&str>) -> Option<String> {
        let first = match &self.nodes.first()?.kind {
            Kind::Entry { name, op, value, .. } if Some(name.as_str()) == keyword && op == ":" => value.as_ref(),
            _ => &self.nodes[0],
        };
        match &first.kind {
            Kind::Str(name) if self.nodes.len() == 1 && !name.is_empty() => Some(name.clone()),
            Kind::Ident(_) | Kind::Index { .. } => Some(src[first.span.start..self.span.end].split_whitespace().collect()),
            _ => None,
        }
    }
}

fn words<'n>(args: &'n [Node]) -> Vec<Word<'n>> {
    let mut out: Vec<Word<'n>> = Vec::new();
    let mut from = 0;
    for k in 1..=args.len() {
        if k == args.len() || args[k].span.start != args[k - 1].span.end {
            out.push(Word { nodes: &args[from..k], span: args[from].span.start..args[k - 1].span.end });
            from = k;
        }
    }
    out
}

/// The edits `text` writes, in order.
pub fn parse(text: &str) -> Result<Vec<Edit>, String> {
    let tree = syntax::parse(text).map_err(|e| {
        let (line, col) = makepad_model_script::lang::names::Names::line_col_of(text, e.at);
        format!("edits {line}:{col}: {}", e.message)
    })?;
    let src = tree.source();
    let mut out = Vec::new();
    let mut anchor = Anchor::Auto;
    let mut unused_before = false;
    let mut statements = tree.statements().iter().enumerate().peekable();
    while let Some((i, stmt)) = statements.next() {
        let at = |m: String| {
            let (line, col) = makepad_model_script::lang::names::Names::line_col_of(src, stmt.span.start);
            format!("edit {} ({line}:{col}): {m}", i + 1)
        };
        let call = stmt.as_call();
        let args = words(call.map(|(_, a)| a).unwrap_or(&[]));
        let need = |k: usize, what: &str| -> Result<&Word, String> { args.get(k).ok_or_else(|| at(format!("{} takes {what}", call.unwrap().0))) };
        let edit = match call.map(|(c, _)| c) {
            Some("set") => match args.as_slice() {
                [site, value] => Edit::Set { site: site.path(src), value: value.code(src).to_string() },
                _ => return Err(at("set takes (site, value), the site as fab.params prints it: set(height, 80mm), set(lid.size, [60mm 4mm 40mm]), set(lid.size.1, 4mm), set(ears.ellipsoid#2.radii, [3mm 5mm 2mm])".into())),
            },
            Some("field") => Edit::Field { solid: need(0, "(solid, name, value)")?.path(src), field: need(1, "(solid, name, value)")?.path(src), code: need(2, "(solid, name, value)")?.code(src).to_string() },
            Some("unset") => Edit::Unset { solid: need(0, "(solid, name)")?.path(src), field: need(1, "(solid, name)")?.path(src) },
            Some("replace") => Edit::Replace { solid: need(0, "(solid or value, code)")?.path(src), code: need(1, "(solid or value, code)")?.code(src).to_string() },
            Some("operand") => Edit::Operand { solid: need(0, "(solid, operand)")?.path(src), code: need(1, "(solid, operand)")?.code(src).to_string() },
            Some("remove") => Edit::Remove { solid: need(0, "(solid)")?.path(src) },
            Some("move") => Edit::Move { solid: need(0, "(what, to): move(fill#3, fill#1)")?.path(src), to: need(1, "(what, to): move(fill#3, fill#1)")?.path(src) },
            Some("rename") => Edit::Rename { solid: need(0, "(solid, new_name)")?.path(src), to: need(1, "(solid, new_name)")?.path(src) },
            Some("rollback") => {
                let usage = || at("rollback takes a part and a step: rollback(bracket, to: eased)".into());
                let [part, to] = args.as_slice() else { return Err(usage()) };
                Edit::Truncate { solid: part.solid_id(src, None).ok_or_else(usage)?, to: to.solid_id(src, Some("to")).ok_or_else(usage)? }
            }
            Some("unobject") => Edit::Root { kind: "object".into(), solid: need(0, "(solid)")?.path(src), add: false },
            Some("place" | "unplace") => return Err(at("where an object prints is the project's: fab.place with place(\"instance\", at: [..])".into())),
            Some("before") => {
                anchor = Anchor::Before(need(0, "(name): the statements added after it go before that name's `let` or `fn`")?.path(src));
                unused_before = true;
                continue;
            }
            Some("after") => {
                let [arg] = args.as_slice() else { return Err(at("after takes one name: after(name)".into())) };
                let name = arg.path(src);
                if name.contains(['.', '[', '#', '@']) {
                    return Err(at(format!("after({name}): an inline step has no `let` to build on: rename it first in the same batch: rename({name}, <name>) then after(<name>) and `let <name> = ...`")));
                }
                match statements.peek().map(|(_, next)| &next.kind) {
                    Some(Kind::Let { keyword, names, pattern_span, value: Some(value) }) if keyword == "let" && names.len() == 1 && src[pattern_span.clone()] == *name => {
                        if let Anchor::Before(before) = &anchor {
                            return Err(at(format!("after cannot follow before({before}) while it places this batch's added statements and `let {name}` would build on {name}")));
                        }
                        out.push(Edit::After { solid: name, code: value.text(src).to_string() });
                        statements.next();
                        anchor = Anchor::Auto;
                    }
                    _ => anchor = Anchor::After { name, nth: 0 },
                }
                unused_before = false;
                continue;
            }
            _ => {
                let edit = Edit::Add { code: stmt.text(src).to_string(), at: anchor.clone() };
                unused_before = false;
                if let Anchor::After { nth, .. } = &mut anchor {
                    *nth += 1;
                }
                edit
            }
        };
        out.push(edit);
    }
    match anchor {
        Anchor::Before(name) if unused_before => return Err(format!("before({name}) places the statements added after it: none follows")),
        Anchor::After { name, nth: 0 } => return Err(format!("after({name}) takes either `let {name} = code` to build on {name} or statements to add after {name}: none follows")),
        _ => {}
    }
    if out.is_empty() {
        return Err("no edits: write Splash statements (set(height, 80mm), let lid = m.box({..}), object(lid), ...)".into());
    }
    Ok(out)
}

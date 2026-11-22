//! Item names and source spans from call sites and their enclosing bindings.

use makepad_script::syntax::{self, Kind, Loc, Node, Syntax, SyntaxError};
use makepad_script::units::UnitTable;
use std::collections::HashMap;
use std::ops::Range;

/// One place an item can be made at.
#[derive(Clone, Debug)]
pub struct Site {
    /// The node's bytes.
    pub span: Range<usize>,
    /// The callee as written, when the node is a call.
    pub callee: Option<String>,
    /// The enclosing bindings, outermost first, stopping at the containing fn.
    pub binds: Vec<Bind>,
    /// No call lies between this node and its innermost retained binding.
    pub owns: bool,
    /// The enclosing `for` loops, stopping at the containing fn.
    pub loops: u8,
}

/// One binding in an item's name.
#[derive(Clone, Debug, PartialEq)]
pub struct Bind {
    /// The binding's identifier.
    pub name: String,
    /// Loops down to the next binding or call, plus any unbound outer loops.
    pub looped: u8,
}

/// Creation sites by 1-based line and character column.
pub struct Names {
    /// The document these sites belong to.
    pub syntax: Syntax,
    /// The node made at each byte a VM may report (the first in walk order); its bindings are
    /// read when a site is asked for, since a run asks for a few of the document's many nodes.
    anchors: HashMap<usize, Loc>,
    starts: Vec<usize>,
}

impl Names {
    /// Index the creation sites of a parsed document.
    pub fn new(syntax: Syntax) -> Names {
        let mut starts = vec![0];
        for (i, b) in syntax.source().bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        let mut anchors: HashMap<usize, Loc> = HashMap::new();
        syntax.walk(&mut |loc, node: &Node| {
            for at in made_at_anchors(node) {
                anchors.entry(at).or_insert_with(|| loc.clone());
            }
        });
        Names { syntax, anchors, starts }
    }

    /// Byte offset of a 1-based line and character column.
    pub fn offset(&self, (line, col): (u32, u32)) -> Option<usize> {
        let start = *self.starts.get((line as usize).checked_sub(1)?)?;
        let rest = &self.syntax.source()[start..];
        let mut n = 1;
        for (i, _) in rest.char_indices() {
            if n == col {
                return Some(start + i);
            }
            n += 1;
        }
        (n == col).then_some(self.syntax.source().len())
    }

    /// The node made at a reported VM location.
    fn anchored(&self, loc: (u32, u32)) -> Option<(&Loc, &Node)> {
        let l = self.anchors.get(&self.offset(loc)?)?;
        Some((l, self.syntax.node(l)?))
    }

    fn site_of(&self, l: &Loc, node: &Node) -> Site {
        let (binds, owns, loops) = binds_of(&self.syntax, l);
        let callee = match &node.kind {
            Kind::Call { callee, .. } => Some(self.syntax.source()[callee.span.clone()].to_string()),
            _ => None,
        };
        Site { span: node.span.clone(), callee, binds, owns, loops }
    }

    /// The creation site at a reported VM location.
    pub fn site(&self, loc: Option<(u32, u32)>) -> Option<Site> {
        self.anchored(loc?).map(|(l, node)| self.site_of(l, node))
    }

    /// The byte span of the node made at a reported VM location.
    pub fn node_span(&self, loc: (u32, u32)) -> Option<Range<usize>> {
        self.anchored(loc).map(|(_, node)| node.span.clone())
    }

    /// The call made at `loc`, else the innermost call written round it.
    pub fn call_site(&self, loc: (u32, u32)) -> Option<Site> {
        if let Some((l, node)) = self.anchored(loc).filter(|(_, node)| matches!(node.kind, Kind::Call { .. })) {
            return Some(self.site_of(l, node));
        }
        let at = self.offset(loc)?;
        let mut best: Option<(Loc, Range<usize>)> = None;
        self.syntax.walk(&mut |l, node: &Node| {
            if matches!(node.kind, Kind::Call { .. }) && node.span.start <= at && at <= node.span.end && best.as_ref().is_none_or(|b| node.span.len() <= b.1.len()) {
                best = Some((l.clone(), node.span.clone()));
            }
        });
        let (l, _) = best?;
        Some(self.site_of(&l, self.syntax.node(&l)?))
    }
}

/// The bytes a VM may report as a creation site.
fn made_at_anchors(node: &Node) -> Vec<usize> {
    match &node.kind {
        Kind::Object { open, .. } => vec![node.span.start, *open],
        Kind::Call { open, .. } => vec![node.span.start, *open],
        Kind::Chain { rest, .. } => {
            // An operator's object is reported just past the operator.
            rest.iter().flat_map(|l| [l.op_span.start, l.op_span.end]).collect()
        }
        _ => Vec::new(),
    }
}

/// Whether this binding is returned by its enclosing `for` body.
fn loop_value(syntax: &Syntax, loc: &Loc, name: &str) -> bool {
    let Some(block_loc) = loc.parent() else { return false };
    let Some(block_node) = syntax.node(&block_loc) else { return false };
    let Kind::Block(block) = &block_node.kind else { return false };
    if block.stmts.last().and_then(Node::ident) != Some(name) {
        return false;
    }
    let Some(parent) = block_loc.parent().and_then(|l| syntax.node(&l)) else { return false };
    if !matches!(&parent.kind, Kind::For { body, .. } if body.span == block_node.span) {
        return false;
    }
    let Some(&index) = loc.0.last() else { return false };
    // A later binding of the same name is the one the body returns.
    !block.stmts.iter().skip(index + 1).any(|stmt| match &stmt.kind {
        Kind::Let { names, .. } => names.iter().any(|bound| bound == name),
        Kind::Fn { name: Some(bound), .. } => bound == name,
        _ => false,
    })
}

/// Enclosing bindings, ownership and loop count, stopping at the containing fn.
fn binds_of(syntax: &Syntax, loc: &Loc) -> (Vec<Bind>, bool, u8) {
    let mut out: Vec<Bind> = Vec::new();
    let mut looped = 0u8;
    let mut loops = 0u8;
    let mut owns = None;
    let mut through_call = false;
    let mut at = loc.parent();
    while let Some(l) = at {
        let Some(node) = syntax.node(&l) else { break };
        match &node.kind {
            Kind::Fn { .. } => break,
            // The iterator is evaluated outside this loop's iterations.
            Kind::For { .. } if loc.0.get(l.0.len()) == Some(&1) => {
                looped = looped.saturating_add(1);
                loops = loops.saturating_add(1);
            }
            Kind::Let { names, .. } if names.len() == 1 => {
                if !loop_value(syntax, &l, &names[0]) {
                    out.push(Bind { name: names[0].clone(), looped });
                    looped = 0;
                    owns.get_or_insert(!through_call);
                }
            }
            Kind::Call { .. } => through_call = true,
            _ => {}
        }
        at = l.parent();
    }
    // Loops outside all bindings still index the outermost name.
    if let Some(bind) = out.last_mut() {
        bind.looped = bind.looped.saturating_add(looped);
    }
    out.reverse();
    (out, owns.unwrap_or(false), loops)
}

/// The dotted item name, using outermost-first loop positions.
/// Missing positions are written as `[?]`; no bindings gives `None`.
pub fn path(binds: &[Bind], positions: &[u32]) -> Option<String> {
    if binds.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut positions = positions.iter();
    for (index, bind) in binds.iter().enumerate() {
        if index != 0 {
            out.push('.');
        }
        out.push_str(&bind.name);
        for _ in 0..bind.looped {
            out.push('[');
            match positions.next() {
                Some(position) => out.push_str(&position.to_string()),
                None => out.push('?'),
            }
            out.push(']');
        }
    }
    Some(out)
}

/// Parse a document for its names with the supplied unit suffixes.
pub fn parse_with(source: &str, units: &UnitTable) -> Result<Names, SyntaxError> {
    Ok(Names::new(syntax::parse_with(source, units)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_and_loop_positions() {
        let source = r#"let cc = for pin in [a, b] { e.r(1, pin, g) }
let dec = for v in xs { let c = e.c(1uF, v, g); e.near(c, u, 3mm); c }
for i in 0..2 { for j in 0..3 { let x = e.c(2uF, i, j) } }
for turn in 0..2 { let make = fn(p, o) { let j = e.part(p, o) } }
let note = "µ"; let u = e.part(a, b)
let grid = for i in 0..2 { for j in 0..3 { e.c(3uF, i, j) } }
let a = if true { let b = for pin in xs { e.r(2, pin, g) } }
let rack = for pin in xs { let element = e.r(3, pin, g); e.near(element, u, 3mm) }
while true { loop { let w = e.r(4, a, g) } }
for pin in e.pins() { e.r(5, pin, g) }
let wrapped = e.part(e.r(6, a, g))
"#;
        let units = UnitTable::standard().with("uF", 1.0, 1e6);
        let names = parse_with(source, &units).expect("the naming fixture parses with unit suffixes");
        let line_col = |at: usize| crate::source::line_col(source, at as u32).expect("a character boundary of the fixture");
        for (call, expected, loops, positions, expected_path, rule) in [
            ("e.r(1, pin, g)", &[("cc", 1)][..], 1, &[2][..], Some("cc[2]"), "a loop's final call names its element"),
            ("e.c(1uF, v, g)", &[("dec", 1)][..], 1, &[2][..], Some("dec[2]"), "a returned binding collapses into its list"),
            ("e.c(2uF, i, j)", &[("x", 2)][..], 2, &[1, 3][..], Some("x[1][3]"), "unbound outer loops index the binding"),
            ("e.part(p, o)", &[("j", 0)][..], 0, &[][..], Some("j"), "bindings and loops stop at the fn"),
            ("e.part(a, b)", &[("u", 0)][..], 0, &[][..], Some("u"), "a top-level binding names a scalar"),
            ("e.c(3uF, i, j)", &[("grid", 2)][..], 2, &[1, 3][..], Some("grid[1][3]"), "nested loop values keep both positions"),
            ("e.r(2, pin, g)", &[("a", 0), ("b", 1)][..], 1, &[4][..], Some("a.b[4]"), "only the list binding gets an index"),
            ("e.r(3, pin, g)", &[("rack", 1), ("element", 0)][..], 1, &[4][..], Some("rack[4].element"), "a nonreturned binding stays inside its list"),
            ("e.r(4, a, g)", &[("w", 0)][..], 0, &[][..], Some("w"), "while and loop do not make lists"),
            ("e.pins()", &[][..], 0, &[][..], None, "an iterator call is outside its loop"),
            ("e.r(5, pin, g)", &[][..], 1, &[0][..], None, "unnamed calls retain their loop count"),
        ] {
            let at = source.find(call).expect(rule);
            let open = at + call.find('(').expect(rule);
            let expected: Vec<Bind> = expected.iter().map(|&(name, looped)| Bind { name: name.into(), looped }).collect();
            for anchor in [at, open] {
                let loc = line_col(anchor);
                let site = names.site(Some(loc)).expect(rule);
                assert_eq!(site.binds, expected, "{rule}: bindings at either call anchor");
                assert_eq!(site.loops, loops, "{rule}: total loop count");
                assert_eq!(path(&site.binds, positions).as_deref(), expected_path, "{rule}: indexed name");
                assert_eq!(site.owns, !expected.is_empty(), "{rule}: binding owns its call");
                assert_eq!(names.offset(loc), Some(anchor), "{rule}: character locations round trip");
                assert_eq!(names.node_span(loc), Some(at..at + call.len()), "{rule}: source span at either call anchor");
            }
            let fallback = names.call_site(line_col(at + call.len() - 1)).expect(rule);
            assert_eq!(fallback.binds, expected, "{rule}: enclosing-call lookup keeps bindings");
            assert_eq!(fallback.loops, loops, "{rule}: enclosing-call lookup keeps loops");
        }
        let grid = names.site(Some(line_col(source.find("e.c(3uF").unwrap()))).unwrap();
        assert_eq!(path(&grid.binds, &[1]).as_deref(), Some("grid[1][?]"), "missing positions keep the known prefix");
        assert_eq!(path(&grid.binds, &[]).as_deref(), Some("grid[?][?]"), "each missing position gets a marker");
        let top = names.site(Some(line_col(source.find("e.part(a, b)").unwrap()))).unwrap();
        assert_eq!(top.binds, [Bind { name: "u".into(), looped: 0 }], "a direct top-level call keeps its binding name");
        assert_eq!(line_col(top.span.start).1, 25, "columns count characters before the call");
        let inner = source.find("e.r(6, a, g)").unwrap();
        let nested = names.call_site(line_col(inner)).unwrap();
        assert!(!nested.owns, "a call argument does not own the enclosing binding");
        assert_eq!(names.call_site(line_col(inner + 5)).unwrap().span, nested.span, "fallback finds the innermost call");
    }
}

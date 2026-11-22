//! Where each object of a program was written: the syntax node a VM object's
//! creation site points at, the top-level `let` that names it, and the `let`s
//! round it that name a part (`let bolts = for .. { m.cylinder(..) }`,
//! `fn leg() { let body = m.box(..) }`).

use makepad_script::syntax::{self, Kind, Loc, Node, Syntax};
use std::collections::HashMap;
use std::ops::Range;

/// One place an object can be made at.
#[derive(Clone, Debug)]
pub struct Site {
    /// The node's bytes.
    pub span: Range<usize>,
    /// The top-level `let` whose value it is.
    pub name: Option<String>,
    /// The `let`s it is written inside, outermost first, up to the fn it is in.
    pub binds: Vec<Bind>,
    /// It is the innermost `let`'s value (no call lies between them): the
    /// binding names it, not a call it is an argument of.
    pub owns: bool,
}

/// A `let` round a node.
#[derive(Clone, Debug, PartialEq)]
pub struct Bind {
    pub name: String,
    /// A loop lies between the `let` and the node, or round the `let` (inside the next
    /// one out): the `let` names a list.
    pub looped: bool,
}

/// Creation sites by 1-based (line, column in characters).
pub struct Names {
    pub syntax: Syntax,
    sites: HashMap<(u32, u32), Site>,
    starts: Vec<usize>,
}

impl Names {
    pub fn new(syntax: Syntax) -> Names {
        let source = syntax.source();
        let mut starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        let mut names = Names { syntax: syntax.clone(), sites: HashMap::new(), starts };
        let mut found: Vec<(usize, Site)> = Vec::new();
        // Values of top-level lets: the node itself, or the last operator of a chain.
        let mut named: HashMap<usize, String> = HashMap::new();
        for stmt in syntax.statements() {
            if let Kind::Let { names: lets, value: Some(value), .. } = &stmt.kind {
                if lets.len() == 1 {
                    for at in made_at_anchors(value, true) {
                        named.insert(at, lets[0].clone());
                    }
                }
            }
        }
        syntax.walk(&mut |loc, node: &Node| {
            let anchors = made_at_anchors(node, false);
            if anchors.is_empty() {
                return;
            }
            let (binds, owns) = binds_of(&syntax, loc);
            for at in anchors {
                found.push((at, Site { span: node.span.clone(), name: named.get(&at).cloned(), binds: binds.clone(), owns }));
            }
        });
        for (at, site) in found {
            let key = names.line_col(at);
            names.sites.entry(key).or_insert(site);
        }
        names
    }

    /// 1-based line and character column of byte `at`.
    pub fn line_col(&self, at: usize) -> (u32, u32) {
        let line = self.starts.partition_point(|s| *s <= at).max(1);
        let start = self.starts[line - 1];
        let col = self.syntax.source()[start..at.min(self.syntax.source().len())].chars().count() + 1;
        (line as u32, col as u32)
    }

    /// Byte offset of a 1-based (line, column).
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

    /// Each creation site a top-level `let` names, at its (line, column).
    pub fn named(&self) -> impl Iterator<Item = ((u32, u32), &str)> {
        self.sites.iter().filter_map(|(loc, s)| Some((*loc, s.name.as_deref()?)))
    }

    pub fn site(&self, loc: Option<(u32, u32)>) -> Option<&Site> {
        self.sites.get(&loc?)
    }

    /// The call a script fn call's instruction belongs to: the one made at `loc`, else
    /// the innermost call written round it.
    pub fn call_site(&self, loc: (u32, u32)) -> Option<Site> {
        if let Some(s) = self.sites.get(&loc).filter(|s| self.is_call(&s.span)) {
            return Some(s.clone());
        }
        let at = self.offset(loc)?;
        let mut best: Option<(Loc, Range<usize>)> = None;
        self.syntax.walk(&mut |l, node: &Node| {
            if matches!(node.kind, Kind::Call { .. }) && node.span.start <= at && at <= node.span.end && best.as_ref().is_none_or(|b| node.span.len() <= b.1.len()) {
                best = Some((l.clone(), node.span.clone()));
            }
        });
        let (l, span) = best?;
        let (binds, owns) = binds_of(&self.syntax, &l);
        Some(Site { span, name: None, binds, owns })
    }

    fn is_call(&self, span: &Range<usize>) -> bool {
        let mut yes = false;
        self.syntax.walk(&mut |_, node: &Node| {
            if !yes && node.span == *span && matches!(node.kind, Kind::Call { .. }) {
                yes = true;
            }
        });
        yes
    }

    /// 1-based line and character column of byte `at` in `source`.
    pub fn line_col_of(source: &str, at: usize) -> (u32, u32) {
        let at = at.min(source.len());
        let line = source[..at].bytes().filter(|b| *b == b'\n').count() + 1;
        let start = source[..at].rfind('\n').map_or(0, |i| i + 1);
        (line as u32, (source[start..at].chars().count() + 1) as u32)
    }

    /// The bytes of the first argument of the call that starts (or opens) at byte `at`.
    pub fn param_value_span(&self, at: usize) -> Option<Range<usize>> {
        let mut out = None;
        self.syntax.walk(&mut |_, node: &Node| {
            if out.is_some() {
                return;
            }
            if let Kind::Call { open, args, .. } = &node.kind {
                if node.span.start == at || *open == at {
                    out = args.first().map(|a| match &a.kind {
                        Kind::Entry { value, .. } => value.span.clone(),
                        _ => a.span.clone(),
                    });
                }
            }
        });
        out
    }
}

/// The bytes a VM may report as an object's creation site: the start of the node,
/// an object's `{`, a call's `(`, and for a chain the operators (`last`: only the
/// last one, whose result is the chain's value).
fn made_at_anchors(node: &Node, last: bool) -> Vec<usize> {
    match &node.kind {
        Kind::Object { open, .. } => vec![node.span.start, *open],
        Kind::Call { open, .. } => vec![node.span.start, *open],
        Kind::Chain { rest, .. } => {
            // The VM reports an operator's object just past the operator.
            if last {
                rest.last().map(|l| vec![l.op_span.start, l.op_span.end]).unwrap_or_default()
            } else {
                rest.iter().flat_map(|l| [l.op_span.start, l.op_span.end]).collect()
            }
        }
        _ => Vec::new(),
    }
}

/// The `let`s round the node at `loc`, outermost first, up to its fn, and
/// whether the node is the innermost one's value (no call between them).
fn binds_of(syntax: &Syntax, loc: &Loc) -> (Vec<Bind>, bool) {
    let mut out: Vec<Bind> = Vec::new();
    let mut looped = false;
    let mut owns = None;
    let mut through_call = false;
    let mut at = loc.parent();
    while let Some(l) = at {
        let Some(node) = syntax.node(&l) else { break };
        match &node.kind {
            Kind::Fn { .. } => break,
            // A loop between the node and a `let` (`let bolts = for ..`), or round a `let`
            // below the next one out (`for .. { let band = .. }`): that `let` names a list.
            Kind::For { .. } | Kind::While { .. } | Kind::Loop { .. } => match out.last_mut() {
                Some(b) if !b.looped && !looped => b.looped = true,
                _ => looped = true,
            },
            Kind::Let { names, .. } if names.len() == 1 => {
                out.push(Bind { name: names[0].clone(), looped });
                looped = false;
                owns.get_or_insert(!through_call);
            }
            Kind::Call { .. } => through_call = true,
            _ => {}
        }
        at = l.parent();
    }
    out.reverse();
    (out, owns.unwrap_or(false))
}

/// Parse a program for its names.
pub fn parse(source: &str) -> Result<Names, syntax::SyntaxError> {
    Ok(Names::new(syntax::parse(source)?))
}

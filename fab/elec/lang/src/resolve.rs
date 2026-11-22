//! Static numeric terms and block parameters. No VM execution or source mutation.

use crate::map::Origin;
use crate::units::{circuit_units, qty_of};
use fab_elec_circuit::QtyUnit;
use makepad_script::syntax::{Grouped, Kind, Node, Syntax};
use std::ops::Range;

/// A written number or an instance parameter, optionally selecting an item of its list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeafKind {
    Number,
    Param { name: String, item: Option<usize> },
}

/// A written input, retaining its location and the dimension of a numeric suffix.
#[derive(Clone, Debug, PartialEq)]
pub struct Leaf {
    pub at: Range<usize>,
    pub kind: LeafKind,
    /// SI value for Number; NaN for Param, whose value is supplied by its instance at runtime.
    /// Kept here for numeric resolver callers; parameter consumers must inspect `kind`.
    pub value: f64,
    pub unit: Option<QtyUnit>,
    /// Enclosing loop depths used to select this written address, rather than its value.
    pub address_loops: Vec<usize>,
}

/// References retain their direct-input form for circuit parameter consumers. Arithmetic
/// folds the written value and an independent affine inverse for each input; None means
/// non-affine in that input, not that the other inputs cannot be edited.
#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    Affine {
        at: Range<usize>,
        constant: f64,
        inputs: Vec<(Leaf, f64)>,
    },
    Arithmetic {
        at: Range<usize>,
        value: f64,
        inputs: Vec<(Leaf, Option<(f64, f64)>)>,
    },
    Opaque {
        at: Range<usize>,
    },
}

impl Term {
    /// The unique invertible written number. Parameters are never numeric edit candidates.
    pub fn leaf(&self) -> Option<(&Leaf, f64)> {
        let candidates = self.candidates();
        match candidates.as_slice() {
            [(leaf, _, b)] => Some((leaf, *b)),
            _ => None,
        }
    }

    /// The unique input, including a parameter. No ranking or arithmetic is performed here.
    pub(crate) fn input(&self) -> Option<(&Leaf, f64)> {
        match self {
            Self::Affine { inputs, .. } => match inputs.as_slice() {
                [(leaf, scale)] if *scale != 0.0 => Some((leaf, *scale)),
                _ => None,
            },
            Self::Arithmetic { .. } | Self::Opaque { .. } => None,
        }
    }

    /// Every invertible numeric input, with T = a + b * L and all other inputs held fixed.
    pub fn candidates(&self) -> Vec<(&Leaf, f64, f64)> {
        match self {
            Self::Affine { constant, inputs, .. } => inputs.iter().filter_map(|(leaf, b)| {
                (leaf.kind == LeafKind::Number && *b != 0.0).then_some((leaf, *constant, *b))
            }).collect(),
            Self::Arithmetic { inputs, .. } => inputs.iter().filter_map(|(leaf, affine)| {
                let (a, b) = (*affine)?;
                (leaf.kind == LeafKind::Number && b != 0.0 && a.is_finite() && b.is_finite())
                    .then_some((leaf, a, b))
            }).collect(),
            Self::Opaque { .. } => Vec::new(),
        }
    }

    pub(crate) fn value(&self) -> Option<f64> {
        match self {
            Self::Affine { constant, inputs, .. } => inputs.iter().try_fold(*constant, |sum, (leaf, b)| {
                (leaf.kind == LeafKind::Number).then_some(sum + b * leaf.value)
            }),
            Self::Arithmetic { value, .. } => Some(*value),
            Self::Opaque { .. } => None,
        }
    }

    /// Unit filtering is strict: bare coefficients cannot become a unit-bearing target.
    pub(crate) fn choose<'a>(
        &'a self,
        tree: &Syntax,
        origin: &Origin,
        unit: Option<QtyUnit>,
    ) -> Result<(&'a Leaf, f64, f64), String> {
        let mut candidates: Vec<_> = self.candidates().into_iter()
            .filter(|(leaf, _, _)| unit.is_none() || leaf.unit == unit).collect();
        if candidates.is_empty() {
            let reason = match self {
                Self::Arithmetic { inputs, .. } if inputs.iter().any(|(leaf, affine)| {
                    (unit.is_none() || leaf.unit == unit) && affine.is_none()
                }) => "the expression is non-affine in its editable inputs",
                Self::Arithmetic { inputs, .. } if inputs.iter().any(|(leaf, affine)| {
                    (unit.is_none() || leaf.unit == unit) && affine.is_some_and(|(_, b)| b == 0.0)
                }) => "zero coefficient: the bound does not depend on any editable value with the target's unit",
                _ => "no invertible written number has the target's unit",
            };
            return Err(reason.into());
        }
        if candidates.len() > 1 {
            let mut ranked = Vec::new();
            for candidate in candidates {
                let count = siblings(tree, origin, candidate.0)
                    .ok_or("the enclosing loop's sibling count is not statically known")?;
                ranked.push((count, candidate));
            }
            let best = ranked.iter().map(|(count, _)| *count).min().expect("candidates");
            candidates = ranked.into_iter().filter(|(count, _)| *count == best)
                .map(|(_, candidate)| candidate).collect();
        }
        if candidates.len() != 1 {
            return Err(format!("tied editable leaves: {}", candidates.iter()
                .map(|(leaf, _, _)| format!("`{}`", &tree.source()[leaf.at.clone()]))
                .collect::<Vec<_>>().join(", ")));
        }
        Ok(candidates[0])
    }
}

pub fn resolve(tree: &Syntax, expr: &Node, origin: &Origin) -> Term {
    match (Resolver {
        tree,
        origin,
        check_mutations: true,
    })
    .value(expr, &mut Vec::new())
    {
        // Library numbers are not editable; parameters can still be overridden at a project call.
        Some(Value::Leaf(leaf, scale))
            if origin.call.doc.0 & crate::source::LIB_DOC == 0 || leaf.kind != LeafKind::Number =>
        {
            Term::Affine {
                at: expr.span.clone(),
                constant: 0.0,
                inputs: vec![(leaf, scale)],
            }
        }
        Some(Value::Arithmetic { value, inputs, .. }) if origin.call.doc.0 & crate::source::LIB_DOC == 0 => {
            Term::Arithmetic { at: expr.span.clone(), value, inputs }
        }
        _ => Term::Opaque {
            at: expr.span.clone(),
        },
    }
}

/// Inputs for a refusal, including references inside unsupported expressions. Resolving an
/// indexed reference stops the walk at its selected item, rather than naming the index as input.
pub(crate) fn inputs(tree: &Syntax, expr: &Node, origin: &Origin) -> Vec<Leaf> {
    fn visit(tree: &Syntax, node: &Node, origin: &Origin, out: &mut Vec<Leaf>) {
        let term = resolve(tree, node, origin);
        let leaves: Vec<_> = match &term {
            Term::Affine { inputs, .. } => inputs.iter().map(|(leaf, _)| leaf).collect(),
            Term::Arithmetic { inputs, .. } => inputs.iter().map(|(leaf, _)| leaf).collect(),
            Term::Opaque { .. } => Vec::new(),
        };
        if !leaves.is_empty() {
            for leaf in leaves {
                if !out.iter().any(|old| old.at == leaf.at && old.kind == leaf.kind) {
                    out.push(leaf.clone());
                }
            }
        } else {
            node.for_each_child(&mut |child| visit(tree, child, origin, out));
        }
    }
    let mut out = Vec::new();
    visit(tree, expr, origin, &mut out);
    out.sort_by_key(|leaf| leaf.at.start);
    out
}

fn contains(outer: &Range<usize>, inner: &Range<usize>) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Ancestors, including the node itself, in source nesting order. O(depth x log(fan-out)): a
/// native call asks this once per call, so a scan of a document's statements here made a
/// circuit's evaluation quadratic in its size.
pub(crate) fn ancestors<'a>(tree: &'a Syntax, span: &Range<usize>) -> Vec<&'a Node> {
    let mut out = Vec::new();
    let mut node = tree.root();
    if !contains(&node.span, span) {
        return out;
    }
    loop {
        out.push(node);
        match child_holding(node, span) {
            Some(child) => node = child,
            None => return out,
        }
    }
}

/// The first child of `node` (in `Node::children` order) whose span holds `span`. Statements,
/// call arguments, list items and object fields are disjoint and in source order, so the first
/// child that ends at or after `span` is the only candidate; other nodes have a few children.
fn child_holding<'a>(node: &'a Node, span: &Range<usize>) -> Option<&'a Node> {
    fn first_ending_after<'a, T>(list: &'a [T], span: &Range<usize>, node: impl for<'b> Fn(&'b T) -> &'b Node) -> Option<&'a Node> {
        let at = list.partition_point(|item| node(item).span.end < span.end);
        list.get(at).map(node).filter(|n| contains(&n.span, span))
    }
    match &node.kind {
        Kind::Block(block) => first_ending_after(&block.stmts, span, |n| n),
        Kind::Array { items, .. } | Kind::Paren { items, .. } => first_ending_after(items, span, |n| n),
        Kind::Object { fields, .. } => first_ending_after(fields, span, |f| &f.value),
        Kind::Call { callee, args, .. } => {
            if contains(&callee.span, span) { Some(&**callee) } else { first_ending_after(args, span, |n| n) }
        }
        _ => {
            let mut found = None;
            node.for_each_child(&mut |child| {
                if found.is_none() && contains(&child.span, span) {
                    found = Some(child);
                }
            });
            found
        }
    }
}

/// For-loop bodies enclosing a call in its innermost function, outermost first.
pub(crate) fn enclosing_loops<'a>(tree: &'a Syntax, call: &Range<u32>) -> Vec<&'a Node> {
    let span = call.start as usize..call.end as usize;
    let path = ancestors(tree, &span);
    let start = path
        .iter()
        .rposition(|n| matches!(n.kind, Kind::Fn { .. }))
        .unwrap_or(0);
    path[start..]
        .iter()
        .copied()
        .filter(|node| matches!(&node.kind, Kind::For { body, .. } if contains(&body.span, &span)))
        .collect()
}

/// The VM reports all frames; only the last k positions belong to this call's function.
pub(crate) fn loop_positions(tree: &Syntax, call: &Range<u32>, positions: &[u32]) -> Vec<u32> {
    let count = enclosing_loops(tree, call).len();
    positions[positions.len().saturating_sub(count)..].to_vec()
}

/// The enclosing conditional supplies diagnostic context and possible alternative drive arms.
pub(crate) fn expression_at_origin<'a>(tree: &'a Syntax, origin: &Origin) -> Option<&'a Node> {
    let span = origin.call.span.start as usize..origin.call.span.end as usize;
    let path = ancestors(tree, &span);
    let start = path
        .iter()
        .rposition(|n| matches!(n.kind, Kind::Fn { .. }))
        .unwrap_or(0);
    path[start..]
        .iter()
        .copied()
        .find(|n| matches!(n.kind, Kind::If { .. }))
        .or_else(|| path.into_iter().find(|n| n.span == span))
}

enum Value<'a> {
    Leaf(Leaf, f64),
    Arithmetic {
        value: f64,
        inputs: Vec<(Leaf, Option<(f64, f64)>)>,
        loops: Vec<usize>,
    },
    Container(&'a Node, Vec<usize>),
    /// A range-loop binding is a constant index, never an editable leaf.
    Index(f64, Vec<usize>),
}

impl Value<'_> {
    fn number(&self) -> Option<f64> {
        match self {
            Self::Leaf(leaf, scale) if leaf.kind == LeafKind::Number => Some(leaf.value * scale),
            Self::Index(value, _) | Self::Arithmetic { value, .. } => Some(*value),
            Self::Leaf(..) | Self::Container(..) => None,
        }
    }

    fn loops(&self) -> &[usize] {
        match self {
            Self::Leaf(leaf, _) => &leaf.address_loops,
            Self::Arithmetic { loops, .. } | Self::Index(_, loops) | Self::Container(_, loops) => loops,
        }
    }

    fn addressed(mut self, depths: &[usize], selected: &Range<usize>) -> Self {
        match &mut self {
            Self::Leaf(leaf, _) if contains(selected, &leaf.at) => union(&mut leaf.address_loops, depths),
            Self::Container(node, loops) if contains(selected, &node.span) => union(loops, depths),
            Self::Arithmetic { inputs, loops, .. } => {
                for (leaf, _) in inputs {
                    if contains(selected, &leaf.at) {
                        union(&mut leaf.address_loops, depths);
                    }
                    union(loops, &leaf.address_loops);
                }
            }
            _ => {}
        }
        self
    }

    fn affine_inputs(&self) -> Vec<(Leaf, Option<(f64, f64)>)> {
        match self {
            Self::Leaf(leaf, b) => vec![(leaf.clone(), Some((0.0, *b)))],
            Self::Arithmetic { inputs, .. } => inputs.clone(),
            _ => Vec::new(),
        }
    }
}

fn union(out: &mut Vec<usize>, depths: &[usize]) {
    for depth in depths {
        if !out.contains(depth) {
            out.push(*depth);
        }
    }
}

/// Count iterations of this call site, excluding loops which address the selected item.
fn siblings(tree: &Syntax, origin: &Origin, leaf: &Leaf) -> Option<usize> {
    let resolver = Resolver { tree, origin, check_mutations: true };
    let mut count = 1usize;
    for (depth, node) in enclosing_loops(tree, &origin.call.span).into_iter().enumerate() {
        if leaf.address_loops.contains(&depth) {
            continue;
        }
        let Kind::For { iter, .. } = &node.kind else { return None };
        let len = if let Some((start, end)) = written_range(iter) {
            if !start.is_finite() || !end.is_finite() || start.fract() != 0.0 || end.fract() != 0.0 {
                return None;
            }
            (end - start).max(0.0) as usize
        } else {
            let Value::Container(array, _) = resolver.value(iter, &mut Vec::new())? else { return None };
            if !matches!(array.kind, Kind::Array { .. }) {
                return None;
            }
            array.items()?.len()
        };
        count = count.checked_mul(len)?;
    }
    Some(count)
}

fn written_range(node: &Node) -> Option<(f64, f64)> {
    let Grouped::Binary { op: "..", lhs, rhs, .. } = node.grouped() else { return None };
    let (Grouped::Operand(start), Grouped::Operand(end)) = (*lhs, *rhs) else { return None };
    Some((written_number(start)?, written_number(end)?))
}

#[derive(Clone, Copy)]
enum Binding<'a> {
    Let {
        node: &'a Node,
        scope: &'a Node,
        value: &'a Node,
    },
    Loop(&'a Node),
    Other,
}

struct Resolver<'a> {
    tree: &'a Syntax,
    origin: &'a Origin,
    check_mutations: bool,
}

impl<'a> Resolver<'a> {
    fn value(&self, node: &'a Node, active: &mut Vec<Range<usize>>) -> Option<Value<'a>> {
        if active.contains(&node.span) {
            return None;
        }
        active.push(node.span.clone());
        let result = self.value_inner(node, active);
        active.pop();
        if self.check_mutations {
            if let Some(Value::Container(container, _)) = &result {
                if self.container_assigned(container) {
                    return None;
                }
            }
        }
        result
    }

    fn value_inner(&self, node: &'a Node, active: &mut Vec<Range<usize>>) -> Option<Value<'a>> {
        if let Some(name) = crate::value_site::param_name(self.tree, node) {
            return Some(Value::Leaf(
                Leaf {
                    at: node.span.clone(),
                    kind: LeafKind::Param { name: name.to_string(), item: None },
                    value: f64::NAN,
                    unit: None,
                    address_loops: Vec::new(),
                },
                1.0,
            ));
        }
        match &node.kind {
            Kind::Number(value) => {
                let text = self.tree.text(node).replace('_', "");
                let unit = circuit_units().entries().iter().find_map(|entry| {
                    text.strip_suffix(entry.suffix.as_str())?
                        .parse::<f64>()
                        .ok()?;
                    qty_of(&entry.suffix).map(|(unit, _, _)| unit)
                });
                Some(Value::Leaf(
                    Leaf {
                        at: node.span.clone(),
                        kind: LeafKind::Number,
                        value: *value,
                        unit,
                        address_loops: Vec::new(),
                    },
                    1.0,
                ))
            }
            Kind::Paren { items, .. } if items.len() == 1 => self.value(&items[0], active),
            Kind::Unary { op, operand } if op == "-" => match self.value(operand, active)? {
                Value::Leaf(leaf, scale) => Some(Value::Leaf(leaf, -scale)),
                Value::Index(value, loops) => Some(Value::Index(-value, loops)),
                Value::Arithmetic { value, inputs, loops } => Some(Value::Arithmetic {
                    value: -value,
                    inputs: inputs.into_iter().map(|(leaf, affine)| {
                        (leaf, affine.map(|(a, b)| (-a, -b)))
                    }).collect(),
                    loops,
                }),
                Value::Container(..) => None,
            },
            Kind::Chain { .. } => self.arithmetic(node.grouped(), active),
            Kind::Array { .. } | Kind::Object { ty: None, .. } => Some(Value::Container(node, Vec::new())),
            Kind::Ident(path) => {
                let mut parts = path.split('.');
                let name = parts.next()?;
                let mut value = self.bound(name, node, active)?;
                for key in parts {
                    value = self.field(value, key, active)?;
                }
                Some(value)
            }
            Kind::Member { base, name, .. } => self.field(self.value(base, active)?, name, active),
            Kind::Index { base, index, .. } => {
                let base = self.value(base, active)?;
                let index = self.value(index, active)?;
                let number = index.number()?;
                if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
                    return None;
                }
                self.index(base, number as usize, index.loops(), active)
            }
            _ => None,
        }
    }

    fn arithmetic(&self, grouped: Grouped<'a>, active: &mut Vec<Range<usize>>) -> Option<Value<'a>> {
        let Grouped::Binary { op, lhs, rhs, .. } = grouped else {
            let Grouped::Operand(node) = grouped else { unreachable!() };
            return self.value(node, active);
        };
        if !matches!(op, "+" | "-" | "*" | "/") {
            return None;
        }
        let lhs = self.arithmetic(*lhs, active)?;
        let rhs = self.arithmetic(*rhs, active)?;
        let (x, y) = (lhs.number()?, rhs.number()?);
        let value = match op { "+" => x + y, "-" => x - y, "*" => x * y, "/" => x / y, _ => unreachable!() };
        if !value.is_finite() {
            return None;
        }
        let mut loops = lhs.loops().to_vec();
        union(&mut loops, rhs.loops());
        let (left, right) = (lhs.affine_inputs(), rhs.affine_inputs());
        let mut leaves: Vec<Leaf> = Vec::new();
        for (leaf, _) in left.iter().chain(&right) {
            if let Some(old) = leaves.iter_mut().find(|old| old.at == leaf.at && old.kind == leaf.kind) {
                // Reading the same leaf through a shared address defeats an indexed address.
                old.address_loops.retain(|depth| leaf.address_loops.contains(depth));
            } else {
                leaves.push(leaf.clone());
            }
        }
        let inputs = leaves.into_iter().map(|leaf| {
            let affine = |inputs: &[(Leaf, Option<(f64, f64)>)], value| {
                inputs.iter().find(|(other, _)| other.at == leaf.at && other.kind == leaf.kind)
                    .map_or(Some((value, 0.0)), |(_, affine)| *affine)
            };
            let result = affine(&left, x).zip(affine(&right, y)).and_then(|((a, b), (c, d))| {
                match op {
                    "+" => Some((a + c, b + d)),
                    "-" => Some((a - c, b - d)),
                    "*" if b == 0.0 || d == 0.0 => Some((a * c, a * d + b * c)),
                    "/" if d == 0.0 && c != 0.0 => Some((a / c, b / c)),
                    _ => None,
                }
            });
            (leaf, result)
        }).collect();
        Some(Value::Arithmetic { value, inputs, loops })
    }

    fn index(
        &self,
        value: Value<'a>,
        index: usize,
        depths: &[usize],
        active: &mut Vec<Range<usize>>,
    ) -> Option<Value<'a>> {
        match value {
            Value::Container(array, mut loops) if matches!(array.kind, Kind::Array { .. }) => {
                union(&mut loops, depths);
                let item = array.items()?.get(index)?;
                Some(self.value(item, active)?.addressed(&loops, &item.span))
            }
            Value::Leaf(mut leaf, scale) if scale == 1.0 => {
                let LeafKind::Param { item, .. } = &mut leaf.kind else { return None };
                if item.is_some() {
                    return None;
                }
                *item = Some(index);
                Some(Value::Leaf(leaf, 1.0))
            }
            _ => None,
        }
    }

    fn field(
        &self,
        value: Value<'a>,
        key: &str,
        active: &mut Vec<Range<usize>>,
    ) -> Option<Value<'a>> {
        let Value::Container(object, loops) = value else {
            return None;
        };
        let (_, fields) = object.object()?;
        // Prototype operators, duplicate fields and bare entries are outside the written-object subset.
        if fields.iter().any(|f| f.name.is_none() || f.op != ":") {
            return None;
        }
        let mut found = fields
            .iter()
            .filter(|field| field.name.as_deref() == Some(key));
        let field = found.next()?;
        if found.next().is_some() {
            return None;
        }
        Some(self.value(&field.value, active)?.addressed(&loops, &field.value.span))
    }

    fn bound(
        &self,
        name: &str,
        used: &'a Node,
        active: &mut Vec<Range<usize>>,
    ) -> Option<Value<'a>> {
        match self.binding(name, used)? {
            Binding::Let { node, scope, value } => {
                if self.check_mutations && self.assigned(name, node, scope) {
                    return None;
                }
                self.value(value, active)
            }
            Binding::Loop(node) => {
                let Kind::For { iter, body, .. } = &node.kind else {
                    return None;
                };
                if self.check_mutations && self.assigned(name, node, body) {
                    return None;
                }
                let loops = enclosing_loops(self.tree, &self.origin.call.span);
                let depth = loops.iter().position(|other| other.span == node.span)?;
                let position = *self.origin.loops.get(depth)? as usize;
                match &iter.kind {
                    Kind::Chain { .. } => {
                        let (start, end) = written_range(iter)?;
                        let index = start + position as f64;
                        (start.is_finite() && end.is_finite() && index < end)
                            .then_some(Value::Index(index, vec![depth]))
                    }
                    _ => self.index(self.value(iter, active)?, position, &[depth], active),
                }
            }
            Binding::Other => None,
        }
    }

    /// Look up from the use, not the origin: initializers and captured names keep their own scope.
    /// Multiple declarations in a scope are ambiguous even if one occurs after the use.
    fn binding(&self, name: &str, used: &Node) -> Option<Binding<'a>> {
        for owner in ancestors(self.tree, &used.span).into_iter().rev() {
            // Conditional braces do not push a VM scope. Functions and loop bodies do.
            let scope = match &owner.kind {
                Kind::Block(block) if block.open.is_none() => owner,
                Kind::Fn { body, .. }
                | Kind::For { body, .. }
                | Kind::While { body, .. }
                | Kind::Loop { body }
                    if contains(&body.span, &used.span) =>
                {
                    body
                }
                _ => continue,
            };
            let mut declarations = Vec::new();
            declarations_in(scope, name, &mut declarations);
            if declarations.len() > 1 {
                return Some(Binding::Other);
            }
            if let Some(&node) = declarations
                .first()
                .filter(|n| n.span.end <= used.span.start)
            {
                // A conditional declaration cannot be followed from outside its arm.
                let visible = ancestors(self.tree, &node.span)
                    .iter()
                    .filter(|n| matches!(n.kind, Kind::Block(_)) && contains(&scope.span, &n.span))
                    .all(|block| contains(&block.span, &used.span));
                return Some(match &node.kind {
                    Kind::Let {
                        keyword,
                        names,
                        value: Some(value),
                        ..
                    } if visible && keyword == "let" && names.len() == 1 => {
                        Binding::Let { node, scope, value }
                    }
                    _ => Binding::Other,
                });
            }
            match &owner.kind {
                Kind::For { binding, .. } => {
                    let binding = self.tree.source()[binding.clone()].trim();
                    if binding == name {
                        return Some(Binding::Loop(owner));
                    }
                    if binding
                        .split(|c: char| !c.is_alphanumeric() && c != '_')
                        .any(|part| part == name)
                    {
                        return Some(Binding::Other);
                    }
                }
                Kind::Fn { params, .. } if params.iter().any(|p| p.name == name) => {
                    return Some(Binding::Other);
                }
                _ => {}
            }
        }
        None
    }

    /// Aliases can mutate a written container without assigning its original name. Trace
    /// assignment roots without mutation checks to detect those aliases, then refuse the
    /// entire container (including nested members). Verification remains the final check.
    fn container_assigned(&self, container: &Node) -> bool {
        let unchecked = Resolver {
            tree: self.tree,
            origin: self.origin,
            check_mutations: false,
        };
        let mut assigned = false;
        self.tree.walk(&mut |_, node| {
            let Kind::Assign { target, .. } = &node.kind else {
                return;
            };
            // Rebinding a bare name changes that binding, not the aliased container.
            if matches!(&target.kind, Kind::Ident(name) if !name.contains('.')) {
                return;
            }
            let Some((name, base)) = assignment_root(target) else {
                return;
            };
            let Some(Value::Container(written, _)) = unchecked.bound(name, base, &mut Vec::new())
            else {
                return;
            };
            assigned |= contains(&container.span, &written.span)
                || contains(&written.span, &container.span);
        });
        assigned
    }

    fn assigned(&self, name: &str, declaration: &Node, scope: &Node) -> bool {
        let mut assigned = false;
        scope.walk(&Default::default(), &mut |_, node| {
            let Kind::Assign { target, .. } = &node.kind else {
                return;
            };
            let Some((root, base)) = assignment_root(target) else {
                return;
            };
            if root != name {
                return;
            }
            let binding = self.binding(root, base);
            assigned |= match binding {
                Some(Binding::Let { node, .. }) | Some(Binding::Loop(node)) => {
                    node.span == declaration.span
                }
                _ => false,
            };
        });
        assigned
    }
}

/// Gather declarations in one VM scope without descending into nested scopes.
fn declarations_in<'a>(node: &'a Node, name: &str, out: &mut Vec<&'a Node>) {
    match &node.kind {
        Kind::Let { names, .. } if names.iter().any(|n| n == name) => out.push(node),
        Kind::Fn { name: Some(n), .. } if n == name => out.push(node),
        _ => {}
    }
    if matches!(
        node.kind,
        Kind::Fn { .. } | Kind::For { .. } | Kind::While { .. } | Kind::Loop { .. }
    ) {
        return;
    }
    node.for_each_child(&mut |child| declarations_in(child, name, out));
}

fn assignment_root(node: &Node) -> Option<(&str, &Node)> {
    match &node.kind {
        Kind::Ident(name) => Some((name.split('.').next()?, node)),
        Kind::Index { base, .. } | Kind::Member { base, .. } => assignment_root(base),
        _ => None,
    }
}

fn written_number(node: &Node) -> Option<f64> {
    match &node.kind {
        Kind::Number(value) => Some(*value),
        Kind::Unary { op, operand } if op == "-" => Some(-written_number(operand)?),
        _ => None,
    }
}

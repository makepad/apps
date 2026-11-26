use super::{Kind, Parsed, Stmt};
use std::collections::HashMap;

/// Count the nodes in the top-level values after substituting their `let`s.
/// Definitions contribute at each use. An unresolved name or a reference that
/// closes a cycle counts as one node. Counts saturate at `u64::MAX`.
pub fn expanded_size(parsed: &Parsed) -> u64 {
    expansion(&parsed.stmts, |_, _| {})
}

pub(super) fn expansion(stmts: &[Stmt], mut cycle: impl FnMut(usize, &str)) -> u64 {
    // Prefer the preceding binding. Until one is available, a forward reference
    // resolves to the first definition, including a possible self-reference.
    let mut bindings = HashMap::new();
    for (i, stmt) in stmts.iter().enumerate() {
        if let Some(name) = stmt.name.as_deref() {
            bindings.entry(name).or_insert(i);
        }
    }

    struct Body {
        nodes: u64,
        references: Vec<(usize, usize)>, // (target statement, source offset)
    }
    let mut bodies = Vec::with_capacity(stmts.len());
    let mut nodes = Vec::new();
    for (i, stmt) in stmts.iter().enumerate() {
        let mut body = Body { nodes: 0, references: Vec::new() };
        nodes.push(&stmt.value);
        while let Some(node) = nodes.pop() {
            match &node.kind {
                Kind::Name(name) => {
                    if let Some(&target) = bindings.get(name.as_str()) {
                        body.references.push((target, node.start));
                        continue;
                    }
                }
                Kind::Arr(items) | Kind::Call { args: items, .. } => nodes.extend(items.iter().rev()),
                Kind::Obj { fields, .. } => nodes.extend(fields.iter().rev().map(|f| &f.value)),
                _ => {}
            }
            body.nodes = body.nodes.saturating_add(1);
        }
        bodies.push(body);
        if let Some(name) = stmt.name.as_deref() {
            bindings.insert(name, i);
        }
    }

    // Visit each body once. The heap stack also handles long chains of
    // shallow definitions, whose length is independent of syntax nesting.
    #[derive(Clone, Copy)]
    enum State { Unseen, Visiting, Sized(u64) }
    let mut states = vec![State::Unseen; stmts.len()];
    let mut stack = Vec::new();
    for i in 0..stmts.len() {
        if matches!(states[i], State::Sized(_)) {
            continue;
        }
        states[i] = State::Visiting;
        stack.push((i, 0, bodies[i].nodes));
        while let Some((index, next, subtotal)) = stack.last_mut() {
            if let Some(&(target, at)) = bodies[*index].references.get(*next) {
                *next += 1;
                match states[target] {
                    State::Sized(count) => *subtotal = subtotal.saturating_add(count),
                    State::Visiting => {
                        cycle(at, stmts[target].name.as_deref().unwrap());
                        *subtotal = subtotal.saturating_add(1);
                    }
                    State::Unseen => {
                        states[target] = State::Visiting;
                        stack.push((target, 0, bodies[target].nodes));
                    }
                }
            } else {
                let count = *subtotal;
                states[*index] = State::Sized(count);
                stack.pop();
                if let Some((_, _, parent)) = stack.last_mut() {
                    *parent = parent.saturating_add(count);
                }
            }
        }
    }
    stmts.iter().zip(states).fold(0u64, |sum, (stmt, state)| match state {
        State::Sized(count) if stmt.name.is_none() => sum.saturating_add(count),
        _ => sum,
    })
}

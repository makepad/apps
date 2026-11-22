//! Source ranges remain in the bench document, including objects built before a native call.

use crate::circuit::{Shared, read::loc_of_ip};
use crate::map::{Origin, SourceRange};
use makepad_script::{
    ScriptValue, ScriptVm,
    syntax::{Kind, Node},
};

pub(super) fn call(vm: &ScriptVm, docs: &Shared) -> SourceRange {
    let mut ips = Vec::new();
    vm.call_sites(&mut ips, 64);
    let docs = docs.borrow();
    for ip in ips {
        let Some((file, line, col)) = loc_of_ip(vm, ip) else {
            continue;
        };
        let Some(d) = docs.docs.iter().find(|d| d.file == file && !d.lib) else {
            continue;
        };
        if let Some(site) = d.names().and_then(|n| n.call_site((line, col))) {
            return SourceRange {
                doc: d.id,
                span: site.span.start as u32..site.span.end as u32,
            };
        }
    }
    SourceRange {
        doc: docs.docs.last().expect("the bench document").id,
        span: 0..0,
    }
}

/// The VM reports all frames; only the last k positions belong to this call's function.
pub(super) fn origin(vm: &ScriptVm, docs: &Shared, call: &SourceRange) -> Origin {
    let docs = docs.borrow();
    let mut positions = Vec::new();
    vm.loop_positions(&mut positions);
    let loops = docs
        .docs
        .iter()
        .find(|d| d.id == call.doc)
        .and_then(|d| d.names())
        .map_or_else(Vec::new, |names| {
            crate::resolve::loop_positions(&names.syntax, &call.span, &positions)
        });
    Origin {
        call: call.clone(),
        loops,
    }
}

pub(super) fn node(docs: &Shared, at: &SourceRange) -> Option<Node> {
    let docs = docs.borrow();
    let syntax = &docs.docs.iter().find(|d| d.id == at.doc)?.names()?.syntax;
    let mut found = None;
    syntax.walk(&mut |_, n| {
        if n.span.start == at.span.start as usize && n.span.end == at.span.end as usize {
            found = Some(n.clone());
        }
    });
    found
}

pub(super) fn range(at: &SourceRange, node: &Node) -> SourceRange {
    SourceRange {
        doc: at.doc,
        span: node.span.start as u32..node.span.end as u32,
    }
}

pub(super) fn argument(docs: &Shared, call: &SourceRange, index: usize) -> Option<Node> {
    let Kind::Call { args, .. } = node(docs, call)?.kind else {
        return None;
    };
    args.get(index).cloned()
}

pub(super) fn object(vm: &ScriptVm, docs: &Shared, value: ScriptValue) -> Option<SourceRange> {
    let ip = vm.bx.heap_ref().made_at(value.as_object()?);
    let (file, line, col) = loc_of_ip(vm, ip)?;
    let docs = docs.borrow();
    let d = docs.docs.iter().find(|d| d.file == file && !d.lib)?;
    let span = d.names()?.node_span((line, col))?;
    Some(SourceRange {
        doc: d.id,
        span: span.start as u32..span.end as u32,
    })
}

pub(super) fn field(
    docs: &Shared,
    object: &SourceRange,
    key: &str,
    value_only: bool,
) -> Option<SourceRange> {
    let n = node(docs, object)?;
    let f = n.field(key)?;
    let span = if value_only { &f.value.span } else { &f.span };
    Some(SourceRange {
        doc: object.doc,
        span: span.start as u32..span.end as u32,
    })
}

/// A drive may have been copied out of a literal object into a generated pin map.
/// Keep that written field; a generated key has only the expression that made its drive.
pub(super) fn drive_field(docs: &Shared, drive: &SourceRange, pin: &str) -> SourceRange {
    let docs = docs.borrow();
    let Some(names) = docs
        .docs
        .iter()
        .find(|d| d.id == drive.doc)
        .and_then(|d| d.names())
    else {
        return drive.clone();
    };
    let mut span = None;
    names.syntax.walk(&mut |_, node| {
        if let Kind::Object { fields, .. } = &node.kind {
            for field in fields {
                if field.name.as_deref() == Some(pin)
                    && field.value.span.start <= drive.span.start as usize
                    && field.value.span.end >= drive.span.end as usize
                    && span
                        .as_ref()
                        .is_none_or(|old: &std::ops::Range<usize>| field.span.len() < old.len())
                {
                    span = Some(field.span.clone());
                }
            }
        }
    });
    span.map_or_else(
        || drive.clone(),
        |s| SourceRange {
            doc: drive.doc,
            span: s.start as u32..s.end as u32,
        },
    )
}

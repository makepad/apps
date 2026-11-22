//! Apply a proposed splice to a snapshot, evaluate once, and check the requested bench values.

use crate::eval::{BenchOut, BenchScope, EvalError, Evaluated, UnitState, evaluate_from};
use crate::bench::Stimulus;
use crate::map::{BenchKey, SourceRange};
use crate::source::{Libs, Sources, located};
use crate::splice::{SpanEdit, splice_inner};
use fab_elec_core::{Diag, DocId, Sel, Severity, drive::Drive};
use fab_elec_design::{AssetStep, BenchOp, Design, DesignOp, FileOp, TextEdit};
use std::sync::atomic::AtomicBool;

#[derive(Clone, Debug)]
pub struct Verified {
    pub edit: SpanEdit,
    pub sources: Sources,
    pub evaluated: Evaluated,
    /// Other changed expectations and stub pins, in bench and evaluation order.
    pub also: Vec<(String, BenchKey)>,
}

/// Add a commit's files to `sources` before its other ops are spliced against them, returning
/// the presence and availability the step sets: each document and asset the project does not
/// hold yet. A present document with the same text, or a listed asset, is the project's copy and
/// stays as it is (the host checks an asset's key); a present document with other text refuses
/// the commit, as a copy is never replaced. Cost: O(documents + assets) per file op.
fn add_files(sources: &mut Sources, ops: &[DesignOp]) -> Result<(Vec<(fab_elec_core::DocId, bool)>, Vec<AssetStep>), Diag> {
    let (mut present, mut assets) = (Vec::new(), Vec::new());
    for op in ops {
        let DesignOp::File(op) = op else { continue };
        match op {
            FileOp::AddDoc { name, text } => match sources.find(name) {
                Some(id) => {
                    if sources.doc(id).is_some_and(|doc| doc.text != *text) {
                        return Err(located(id, 0..0, format!("{name} is in the project with other text: its copy is kept, never replaced")));
                    }
                }
                None => present.push((sources.add(name.clone(), text.clone())?, true)),
            },
            FileOp::AddAsset { name, key, bytes } => {
                if sources.assets().binary_search(name).is_err() {
                    sources.add_asset(name.clone());
                    assets.push(AssetStep { name: name.clone(), key: key.clone(), bytes: bytes.clone(), available: true });
                }
            }
        }
    }
    Ok((present, assets))
}

/// `sources` with the documents and assets a commit's file ops add, as its splice saw them before
/// its edits: the text [`SpanEdit::inverse`](crate::SpanEdit::inverse) inverts against when the
/// commit edits a document it adds (a part's first `e.body` in a new models document). A file op
/// the commit could not add is left out, as its splice refused it. Cost: a clone of the document
/// list (texts are shared) and O(documents) per file op.
pub fn with_files(sources: &Sources, ops: &[DesignOp]) -> Sources {
    let mut out = sources.clone();
    let _ = add_files(&mut out, ops);
    out
}

/// Derived edits are returned only after the resulting bench values satisfy every request.
/// Circuit operations are spliced and evaluated, but their values are not verified here. A refusal
/// or a failed request is one mistake (`EvalError::Diags`); `cancel` stops the evaluation as
/// `evaluate_from` does.
pub fn splice_verified(
    sources: &Sources,
    libs: &Libs,
    prev: &Evaluated,
    ops: &[DesignOp],
    cancel: &AtomicBool,
) -> Result<Verified, EvalError> {
    let one = |diag: Diag| EvalError::Diags(vec![diag]);
    let mut sources = sources.clone();
    let (present, assets) = add_files(&mut sources, ops).map_err(one)?;
    let mut edit = splice_inner(&sources, &prev.map, &prev.design, ops, true).map_err(one)?;
    sources.apply(&edit.edits).map_err(one)?;
    edit.reevaluate |= !present.is_empty() || !assets.is_empty();
    (edit.present, edit.assets) = (present, assets);
    // The evaluation reads board item ids from the map at the edited text's positions.
    let mut base = prev.clone();
    base.map.follow(&edit);
    let evaluated = evaluate_from(Some(&base), &sources, libs, cancel).map_err(|e| match e {
        EvalError::Cancelled => EvalError::Cancelled,
        e => {
            let mut diag = e.into_diag();
            diag.message = format!("splice verification failed: {}", diag.message);
            one(diag)
        }
    })?;
    let targets: Vec<_> = ops
        .iter()
        .filter_map(|op| match op {
            DesignOp::Bench { bench, op } => Some((bench.clone(), key(op))),
            _ => None,
        })
        .collect();
    for op in ops {
        let DesignOp::Bench { bench, op } = op else {
            continue;
        };
        let out = evaluated.benches.iter().find(|b| &b.name == bench);
        let valid = out.is_some_and(|out| {
            out.state == UnitState::Built
                && match &out.scope {
                    BenchScope::Design(id) => evaluated
                        .units
                        .iter()
                        .any(|unit| unit.id == *id && unit.state == UnitState::Built),
                    BenchScope::Own { .. } => true,
                }
                && matches_request(out, op, &evaluated, prev)
        });
        if !valid {
            let detail = out
                .and_then(|b| b.state.diags().first())
                .map(|d| d.message.as_str())
                .unwrap_or("the evaluated value does not match the request");
            return Err(one(refusal(prev, bench, &key(op), &edit, detail)));
        }
    }
    let mut also = Vec::new();
    let mut names: Vec<&str> = Vec::new();
    for out in prev.benches.iter().chain(&evaluated.benches) {
        if !names.contains(&out.name.as_str()) {
            names.push(&out.name);
        }
    }
    for name in names {
        let before = prev.benches.iter().find(|b| b.name == name);
        let after = evaluated.benches.iter().find(|b| b.name == name);
        let affected = targets.iter().any(|(bench, _)| bench == name)
            || before
                .into_iter()
                .chain(after)
                .any(|b| edit.edits.iter().any(|e| e.doc == b.doc));
        if !affected {
            continue;
        }
        if let Some(out) = after.filter(|out| out.state != UnitState::Built) {
            return Err(one(refusal(
                prev,
                name,
                &BenchKey::Body,
                &edit,
                out.state
                    .diags()
                    .first()
                    .map_or("the affected bench did not build", |d| d.message.as_str()),
            )));
        }
        let mut keys = Vec::new();
        for out in before.into_iter().chain(after) {
            for expect in &out.expects {
                let key = BenchKey::Expect(expect.name.clone());
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
            for stub in &out.spec.stubs {
                for (pin, _) in &stub.pins {
                    let key = BenchKey::Stub(stub.path.clone(), pin.clone());
                    if !keys.contains(&key) {
                        keys.push(key);
                    }
                }
            }
        }
        for key in keys {
            if targets.contains(&(name.to_string(), key.clone())) {
                continue;
            }
            let changed = match &key {
                BenchKey::Expect(expect) => {
                    before.and_then(|b| b.expects.iter().find(|e| &e.name == expect))
                        != after.and_then(|b| b.expects.iter().find(|e| &e.name == expect))
                }
                BenchKey::Stub(stub, pin) => {
                    pin_drive(before, stub, pin) != pin_drive(after, stub, pin)
                }
                _ => false,
            };
            if changed {
                also.push((name.to_string(), key));
            }
        }
    }
    Ok(Verified {
        edit,
        sources,
        evaluated,
        also,
    })
}

/// A caller's change of document text, evaluated before anything commits it: what `fab.elec_text`
/// sends for the AI. It verifies when it adds no mistake: no unit or bench gets a mistake it did
/// not have, and no new error is noted in an edited document, so a document already broken can be
/// fixed a step at a time. (A person's typing commits first
/// and is evaluated after, as the pane shows it; it never comes here.) The edit's `written`
/// ranges are its replacements in the text after it, so the host finds what it defines.
/// `Err`: the mistakes it adds, located in the text after it. Cost: one evaluation of the
/// changed units, as a commit's verification.
pub fn text_verified(sources: &Sources, libs: &Libs, prev: &Evaluated, edits: Vec<TextEdit>, cancel: &AtomicBool) -> Result<Verified, EvalError> {
    let one = |diag: Diag| EvalError::Diags(vec![diag]);
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|edit| (edit.doc.ix(), edit.span.start));
    let mut written = Vec::with_capacity(sorted.len());
    let mut shift = (None::<DocId>, 0i64);
    for edit in sorted {
        if shift.0 != Some(edit.doc) {
            shift = (Some(edit.doc), 0);
        }
        let start = (edit.span.start as i64 + shift.1) as u32;
        written.push(SourceRange { doc: edit.doc, span: start..start + edit.text.len() as u32 });
        shift.1 += edit.text.len() as i64 - (edit.span.end - edit.span.start) as i64;
    }
    let edited: Vec<DocId> = edits.iter().map(|edit| edit.doc).collect();
    let edit = SpanEdit { edits, written, reevaluate: true, ..SpanEdit::default() };
    let mut sources = sources.clone();
    sources.apply(&edit.edits).map_err(one)?;
    let mut base = prev.clone();
    base.map.follow(&edit);
    let evaluated = evaluate_from(Some(&base), &sources, libs, cancel)?;
    // A mistake is new when the unit or bench did not have its message before.
    let new = |after: &UnitState, before: Option<&UnitState>| -> Vec<Diag> {
        let had = before.map_or(&[][..], UnitState::diags);
        after.diags().iter().filter(|diag| !had.iter().any(|old| old.message == diag.message)).cloned().collect()
    };
    let mut mistakes = Vec::new();
    for unit in &evaluated.units {
        mistakes.extend(new(&unit.state, prev.units.iter().find(|old| old.id == unit.id).map(|old| &old.state)));
    }
    for bench in &evaluated.benches {
        mistakes.extend(new(&bench.state, prev.benches.iter().find(|old| old.name == bench.name).map(|old| &old.state)));
    }
    let in_edited = |diag: &Diag| diag.sel.iter().any(|sel| matches!(sel, Sel::Source { doc, .. } if edited.contains(doc)));
    for note in &evaluated.notes {
        if note.severity == Severity::Error && in_edited(note) && !prev.notes.iter().any(|old| old.message == note.message) {
            mistakes.push(note.clone());
        }
    }
    if !mistakes.is_empty() {
        return Err(EvalError::Diags(mistakes));
    }
    Ok(Verified { edit, sources, evaluated, also: Vec::new() })
}

fn key(op: &BenchOp) -> BenchKey {
    match op {
        BenchOp::SetDrive { stub, pin, .. } | BenchOp::WriteDrive { stub, pin, .. } => {
            BenchKey::Stub(stub.clone(), pin.clone())
        }
        BenchOp::SetRange { expect, .. } => BenchKey::Expect(expect.clone()),
        BenchOp::SetPressed { comp, .. } => BenchKey::Press(comp.clone()),
        BenchOp::SetSupply { net, .. } | BenchOp::AddSupply { net, .. } | BenchOp::RemoveSupply { net } => {
            BenchKey::Supply(net.clone())
        }
        BenchOp::SetSource { a, b, .. } | BenchOp::AddSource { a, b, .. } | BenchOp::RemoveSource { a, b } => {
            BenchKey::Source(a.clone(), b.clone())
        }
        BenchOp::AddProbe { of, .. }
        | BenchOp::RemoveProbe { of }
        | BenchOp::SetProbeRef { of, .. }
        | BenchOp::SetProbeTip { of, .. }
        | BenchOp::SetProbeLabel { of, .. }
        | BenchOp::SetProbeAt { of, .. } => BenchKey::Probe(of.clone()),
    }
}

fn pin_drive<'a>(bench: Option<&'a BenchOut>, stub: &str, pin: &str) -> Option<&'a Drive> {
    bench?
        .spec
        .stubs
        .iter()
        .find(|s| s.path == stub)?
        .pins
        .iter()
        .find(|(name, _)| name == pin)
        .map(|(_, drive)| drive)
}

fn matching_sources(bench: &BenchOut, design: &Design, kind: &str, a: &str, b: &str, opts: &[(String, f64)]) -> usize {
    let Some(circuit) = bench.circuit(design) else { return 0 };
    let (Some(a), Some(b)) = (circuit.net(a), circuit.net(b)) else { return 0 };
    bench.spec.sources.iter().filter(|source| {
        if source.a.net != a.id || source.b.net != b.id {
            return false;
        }
        let stimulus = Stimulus::Source(source.kind.clone());
        opts.iter().all(|(key, value)| {
            matches!(stimulus.option(key), Ok((call, _, Some(got))) if call == kind && close(got, *value))
        })
    }).count()
}

fn matches_request(bench: &BenchOut, op: &BenchOp, evaluated: &Evaluated, prev: &Evaluated) -> bool {
    let map = &evaluated.map;
    match op {
        BenchOp::SetDrive { stub, pin, drive } | BenchOp::WriteDrive { stub, pin, drive } => {
            pin_drive(Some(bench), stub, pin).is_some_and(|got| same_drive(got, drive))
        }
        BenchOp::SetRange { expect, min, max } => bench
            .expects
            .iter()
            .find(|e| &e.name == expect)
            .and_then(|got| got.range())
            .is_some_and(|(got_min, got_max)| close(got_min, *min) && close(got_max, *max)),
        BenchOp::SetPressed { comp, pressed } => bench.spec.pressed.contains(comp) == *pressed,
        BenchOp::SetSupply { key: field, value, .. } | BenchOp::SetSource { key: field, value, .. } => {
            map.bench_stimuli.get(&(bench.name.clone(), key(op)))
                .and_then(|stimulus| stimulus.option(field).ok()?.2)
                .is_some_and(|got| close(got, *value))
        }
        BenchOp::AddSupply { v, .. } => {
            matches!(map.bench_stimuli.get(&(bench.name.clone(), key(op))), Some(Stimulus::Supply(s)) if close(s.v, *v))
        }
        BenchOp::AddSource { kind, a, b, opts } => {
            let before = prev.benches.iter().find(|old| old.name == bench.name)
                .map_or(0, |old| matching_sources(old, &prev.design, kind, a, b, opts));
            matching_sources(bench, &evaluated.design, kind, a, b, opts) > before
        }
        BenchOp::RemoveSupply { .. } | BenchOp::RemoveSource { .. } => {
            map.bench_site(&bench.name, &key(op)).is_none()
        }
        BenchOp::AddProbe { of, label } => bench
            .probes
            .iter()
            .any(|p| &p.of == of && &p.label == label),
        BenchOp::RemoveProbe { of } => !bench.probes.iter().any(|p| &p.of == of),
        BenchOp::SetProbeLabel { of, label } => bench.probes.iter().any(|p| &p.of == of && &p.label == label),
        BenchOp::SetProbeAt { of, at } => bench.probes.iter().any(|p| &p.of == of && &p.at == at),
        BenchOp::SetProbeRef { of, reference } => bench.probes.iter().any(|p| {
            p.of == fab_elec_design::ProbeOf {
                reference: reference.clone(),
                ..of.clone()
            }
        }),
        BenchOp::SetProbeTip { of, tip } => bench.probes.iter().any(|p| {
            &p.of == of
                && [(p.tip.r, tip.r), (p.tip.c, tip.c), (p.tip.bw, tip.bw)]
                    .into_iter()
                    .all(|(a, b)| match (a, b) {
                        (None, None) => true,
                        (Some(a), Some(b)) => close(a, b),
                        _ => false,
                    })
        }),
    }
}

/// Equal, or within 1e-9 of each other relative to the larger magnitude.
pub(crate) fn close(a: f64, b: f64) -> bool {
    a == b || (a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-9 * a.abs().max(b.abs()))
}

pub(crate) fn same_drive(a: &Drive, b: &Drive) -> bool {
    let normalise = |drive: &Drive| match drive {
        Drive::Pwm { duty, .. } if *duty == 0.0 || *duty == 1.0 => Drive::Level(*duty == 1.0),
        drive => drive.clone(),
    };
    match (normalise(a), normalise(b)) {
        (Drive::Pwm { freq: a, duty: x }, Drive::Pwm { freq: b, duty: y }) => {
            close(a, b) && close(x, y)
        }
        (a, b) => a == b,
    }
}

fn refusal(prev: &Evaluated, bench: &str, key: &BenchKey, edit: &SpanEdit, detail: &str) -> Diag {
    let at = prev
        .map
        .bench_origin(bench, key)
        .map(|origin| &origin.call)
        .or_else(|| prev.map.bench_site(bench, key))
        .or_else(|| prev.map.bench_site(bench, &BenchKey::Body));
    let what = match key {
        BenchKey::Stub(stub, pin) => format!("{stub}.{pin}"),
        BenchKey::Expect(name) => format!("expectation {name}"),
        BenchKey::Press(comp) => format!("press {comp}"),
        BenchKey::Supply(net) => format!("supply {net}"),
        BenchKey::Source(a, b) => format!("source ({a}, {b})"),
        BenchKey::Probe(of) => format!("probe {of}"),
        BenchKey::Body => "the bench".to_string(),
    };
    let message = format!("{bench}: {what}: splice verification failed: {detail}");
    let mut diag = match at {
        Some(at) => located(at.doc, at.span.clone(), message),
        None => Diag::error(message, vec![]),
    };
    for change in &edit.edits {
        let sel = Sel::Source {
            doc: change.doc,
            start: change.span.start,
            end: change.span.end,
        };
        if !diag.sel.contains(&sel) {
            diag.sel.push(sel);
        }
    }
    diag
}

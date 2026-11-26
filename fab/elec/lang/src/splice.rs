//! Span splices (C5): the text side of a commit of design ops.
//!
//! The host applies a commit's board or system ops to the model and asks for their splice: the
//! edits of exactly the statements the ops changed, written with the person's formatting kept, and
//! the source map's update. The document is not evaluated again for an edit the ops already told
//! the model about (`reevaluate` false). A circuit op is text by nature (the circuit is code): its
//! splice is the whole change, and the host evaluates the circuit after it (`reevaluate` true).
//!
//! One `SpanEdit` per commit (architect change 5): a router commit of hundreds of board ops is one
//! pass over the board document and one `follow` of the map.
//!
//! The host's sequence for one commit: `let e = splice(..)?; sources.apply(&e.edits)?; map.follow(&e)`.
//!
//! `SetValue` writes where the value is written (`SourceMap::value_site`, architect change 3): the
//! literal itself (every component that reads it changes: the chip counted them before the drop),
//! an override at the instance's call for a block parameter, never a library document, and never
//! an expression (a located refusal naming it).

use crate::bench_splice;
use crate::circuit_splice;
use crate::hint_splice;
use crate::pin_splice;
use crate::series_splice;
use crate::map::{BenchKey, PinSite, SourceMap, SourceRange, ValueSite};
use crate::source::{located, Sources};
use crate::units::{circuit_units, literal_text};
use crate::value_site::{call_at, literal_for, override_edit, tol_arg, tolerance_percent, two_pin_net_arg};
use fab_elec_board::BoardOp;
use fab_elec_circuit::{CircuitOp, NetStyle, ParamValue, Quantity, SheetHint, Value};
use fab_elec_core::{BoardId, CompId, CompRef, Diag, DocId, PinIx, Sel};
use fab_elec_parts::Tol;
use fab_elec_design::{AssetStep, BenchOp, Design, DesignOp, SysOp, TextEdit, TextStep};
use makepad_script::syntax::{self, Kind, Syntax};
use std::collections::BTreeMap;
use std::ops::Range;

/// All components made by exactly the same source call, including instances on other boards.
#[derive(Default)]
pub struct SharedCalls {
    members: BTreeMap<CompRef, std::sync::Arc<[CompRef]>>,
}
impl SharedCalls {
    pub fn new(design: &Design, map: &SourceMap) -> Self {
        let mut by_call = BTreeMap::new();
        for unit in &design.units {
            for c in &unit.circuit.comps {
                let part = CompRef { board: unit.id, comp: c.id };
                if let Some(at) = map.locate(&Sel::Comp(part)) {
                    by_call.entry((at.doc, at.span.start, at.span.end)).or_insert_with(Vec::new).push(part);
                }
            }
        }
        let mut members = BTreeMap::new();
        for group in by_call.into_values().filter(|g| g.len() > 1) {
            let group: std::sync::Arc<[CompRef]> = group.into();
            for part in group.iter() { members.insert(*part, group.clone()); }
        }
        Self { members }
    }

    pub fn group(&self, part: CompRef) -> &[CompRef] {
        self.members.get(&part).map_or(&[], |g| &g[..])
    }

    /// A partial selection cannot write the call without also changing unselected instances.
    pub fn partial_groups(&self, parts: &[CompRef]) -> Vec<&[CompRef]> {
        let mut counts = BTreeMap::new();
        for part in parts.iter().copied().collect::<std::collections::BTreeSet<_>>() {
            if let Some(&first) = self.group(part).first() {
                *counts.entry(first).or_insert(0) += 1;
            }
        }
        counts.into_iter().filter_map(|(first, count)| {
            let group = self.group(first);
            (count < group.len()).then_some(group)
        }).collect()
    }
}

/// The text side of one commit.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SpanEdit {
    /// Edits in the text as it is before them, disjoint (`Sources::apply` takes them as one).
    pub edits: Vec<TextEdit>,
    /// Statements the edit writes, at their ranges in the text after it (a board `Add`).
    pub adds: Vec<(Sel, SourceRange)>,
    /// The ranges of the statements the edit writes, in the text after it, which the host
    /// resolves to selections from the evaluated map (AddPart and InsertSeries).
    pub written: Vec<SourceRange>,
    /// Items whose statement the edit removes.
    pub removes: Vec<Sel>,
    /// Presence set after the edits, in order: the documents a commit's `FileOp::AddDoc` adds
    /// (`splice_verified`).
    pub present: Vec<(DocId, bool)>,
    /// Asset availability set after presence, in order: the assets its `FileOp::AddAsset` adds.
    pub assets: Vec<AssetStep>,
    /// The edit changes what evaluation gives beyond what the ops did to the model (every circuit
    /// op): the host evaluates the document again.
    pub reevaluate: bool,
}

impl SpanEdit {
    /// Undo a splice made against `before` and `map_before`. Text spans address the edited
    /// documents; saved selections address the restored text. Evaluation rebuilds value,
    /// pin and bench sites when `reevaluate` is set, just as it does for the forward splice.
    pub fn inverse(&self, before: &Sources, map_before: &SourceMap) -> SpanEdit {
        let mut out = SpanEdit {
            removes: self.adds.iter().filter(|(sel, _)| map_before.locate(sel).is_none()).map(|(sel, _)| sel.clone()).collect(),
            present: self.present.iter().rev().map(|&(doc, present)| (doc, !present)).collect(),
            assets: self.assets.iter().rev().map(|asset| AssetStep { available: !asset.available, ..asset.clone() }).collect(),
            reevaluate: self.reevaluate,
            ..SpanEdit::default()
        };
        let mut docs: Vec<_> = self.edits.iter().map(|e| e.doc).collect();
        docs.sort();
        docs.dedup();
        for doc in docs {
            let text = &before.doc(doc).expect("a splice edits a document in its sources").text;
            let mut edits: Vec<_> = self.edits.iter().filter(|e| e.doc == doc).collect();
            edits.sort_by_key(|e| (e.span.start, e.span.end));
            let mut delta = 0i64;
            for edit in &edits {
                let start = (i64::from(edit.span.start) + delta) as u32;
                out.edits.push(TextEdit {
                    doc, span: start..start + edit.text.len() as u32,
                    text: text[edit.span.start as usize..edit.span.end as usize].to_string(),
                });
                delta += edit.text.len() as i64 - i64::from(edit.span.end - edit.span.start);
            }
            for (sel, range) in map_before.of_doc(doc) {
                // shift clamps interior endpoints and gives starts and ends different boundary
                // affinities. Save both closed boundaries, including insertion points. This also
                // covers every contained Item that shift deletes. Ranges enclosing an edit with
                // neither endpoint touching it recover through the inverse length deltas alone.
                let touched = edits.iter().any(|e| {
                    let inside = |p| e.span.start <= p && p <= e.span.end;
                    inside(range.span.start) || inside(range.span.end)
                });
                if touched || self.removes.contains(&sel) || self.adds.iter().any(|(added, _)| *added == sel) {
                    out.adds.push((sel, range));
                }
            }
        }
        out
    }
}

/// The undo log keeps a splice below the language as a [`TextStep`], dropping `written`.
impl From<SpanEdit> for TextStep {
    fn from(e: SpanEdit) -> TextStep {
        TextStep {
            edits: e.edits,
            present: e.present,
            assets: e.assets,
            adds: e.adds.into_iter().map(|(sel, r)| (sel, r.doc, r.span)).collect(),
            removes: e.removes,
            reevaluate: e.reevaluate,
        }
    }
}

impl From<TextStep> for SpanEdit {
    fn from(t: TextStep) -> SpanEdit {
        SpanEdit {
            edits: t.edits,
            adds: t.adds.into_iter().map(|(sel, doc, span)| (sel, SourceRange { doc, span })).collect(),
            written: vec![],
            removes: t.removes,
            present: t.present,
            assets: t.assets,
            reevaluate: t.reevaluate,
        }
    }
}

/// The splice of a commit's `ops` against the documents and map that `design` was evaluated
/// from. `design` is the state before the commit (the host applies board ops to the model).
/// Bench references beyond literal sites require [`crate::splice_verified`], which verifies
/// their targets and reports other changed values before returning the edit.
pub fn splice(sources: &Sources, map: &SourceMap, design: &Design, ops: &[DesignOp]) -> Result<SpanEdit, Diag> {
    if ops.iter().any(|op| matches!(op, DesignOp::File(_))) {
        return Err(Diag::error("documents and assets are added with the ops that use them, verified (splice_verified)", vec![]));
    }
    splice_inner(sources, map, design, ops, false)
}

/// One sheet table edit per unit, including the hints of newly named parts.
#[derive(Default)]
struct SheetUpdates<'a> {
    hints: Vec<(String, Option<&'a SheetHint>)>,
    styles: Vec<(&'a str, Option<NetStyle>)>,
    part_anchor: Option<SourceRange>,
}

/// Derived edits are available only to the entry point that verifies them.
pub(crate) fn splice_inner(
    sources: &Sources,
    map: &SourceMap,
    design: &Design,
    ops: &[DesignOp],
    resolve_drives: bool,
) -> Result<SpanEdit, Diag> {
    let edits = pin_splice::assembly_edits(sources, map, design, ops)?;
    let mut out = SpanEdit { reevaluate: !edits.is_empty(), edits, ..SpanEdit::default() };
    let mut written = Vec::new();
    // Board ops and factory headers: one splice per unit, the unit's ops in commit order.
    let mut units: Vec<BoardId> = Vec::new();
    for op in ops {
        if let DesignOp::Board { unit, .. } | DesignOp::Sys(SysOp::SetFactory { unit, .. }) = op {
            if !units.contains(unit) {
                units.push(*unit);
            }
        }
    }
    for unit in units {
        let u = design.units.iter().find(|u| u.id == unit).ok_or_else(|| Diag::error(format!("the design has no unit {}", unit.0), vec![Sel::Unit(unit)]))?;
        let board = u.board.as_deref().ok_or_else(|| Diag::error(format!("unit {} has no board", u.name), vec![Sel::Unit(unit)]))?;
        let unit_ops: Vec<BoardOp> = ops.iter().filter_map(|op| match op {
            DesignOp::Board { unit: of, op } if *of == unit => Some(op.clone()),
            _ => None,
        }).collect();
        let factory = ops.iter().rev().find_map(|op| match op {
            DesignOp::Sys(SysOp::SetFactory { unit: of, profile }) if *of == unit => Some(profile.clone()),
            _ => None,
        });
        merge(&mut out, crate::board_splice::splice_board(sources, map, board, &u.circuit, &unit_ops, factory)?)?;
    }
    // Link and mount ops: one splice for the commit, in commit order.
    let sys: Vec<&SysOp> = ops.iter().filter_map(|op| match op {
        DesignOp::Sys(op) if !matches!(op, SysOp::SetFactory { .. }) => Some(op),
        _ => None,
    }).collect();
    if !sys.is_empty() {
        merge(&mut out, crate::sys_splice::splice_system(sources, map, design, &sys)?)?;
    }
    let mut parts = circuit_splice::add_parts(sources, map, design, ops)?.into_iter();
    let mut statements = BTreeMap::<(DocId, u32), usize>::new();
    let mut sheets = BTreeMap::<BoardId, SheetUpdates<'_>>::new();
    // A pin a later op of this commit connects: its Connect rewrites the pin's one site, which
    // already replaces the old connection, so a Disconnect of it writes nothing (the plan's "onto
    // a loose pin" disconnects first; both edits would rewrite the same text).
    for (i, op) in ops.iter().enumerate() {
        if let DesignOp::Circuit { unit, op: CircuitOp::Disconnect { pin } } = op {
            let later = ops[i + 1..].iter().any(|later| matches!(later,
                DesignOp::Circuit { unit: u, op: CircuitOp::Connect { pin: p, .. } | CircuitOp::ConnectNamed { pin: p, .. } }
                    if u == unit && p == pin));
            if later { continue; }
        }
        match op {
            // File ops are on the sources already (`splice_verified` adds them before the splice).
            DesignOp::Board { .. } | DesignOp::Sys(_) | DesignOp::File(_) => {}
            DesignOp::Circuit { unit, op: CircuitOp::Hint { path, hint } } =>
                sheets.entry(*unit).or_default().hints.push((path.clone(), hint.as_ref())),
            DesignOp::Circuit { unit, op: CircuitOp::NetStyle { name, style } } =>
                sheets.entry(*unit).or_default().styles.push((name.as_str(), *style)),
            DesignOp::Circuit { unit, op: CircuitOp::SetValue { comp, value } } => {
                let edits = set_value(sources, map, design, *unit, *comp, value)?;
                merge(&mut out, SpanEdit { edits, reevaluate: true, ..SpanEdit::default() })?;
            }
            DesignOp::Circuit { unit, op: CircuitOp::AddPart { hint, .. } } => {
                let part = parts.next().expect("one filled statement per AddPart, in op order");
                merge(&mut out, SpanEdit { edits: part.other, reevaluate: true, ..SpanEdit::default() })?;
                let key = (part.statement.doc, part.statement.span.start);
                // All statements at this boundary form one undoable insertion, in op order.
                let (index, offset) = if let Some(&index) = statements.get(&key) {
                    let text = &mut out.edits[index].text;
                    let offset = text.len();
                    text.push_str(&part.statement.text);
                    (index, offset)
                } else {
                    let index = out.edits.len();
                    merge(&mut out, SpanEdit { edits: vec![part.statement], ..SpanEdit::default() })?;
                    statements.insert(key, index);
                    (index, 0)
                };
                let base = key.1 + offset as u32;
                written.push((index..index + 1, vec![SourceRange { doc: key.0,
                    span: base + part.written.start as u32..base + part.written.end as u32 }]));
                if let (Some(path), Some(hint)) = (part.path, hint.as_ref()) {
                    let sheet = sheets.entry(*unit).or_default();
                    sheet.hints.push((path, Some(hint)));
                    sheet.part_anchor = Some(part.anchor);
                }
            }
            DesignOp::Circuit { unit, op: CircuitOp::InsertSeries { net, stmt, moved } } => {
                let mut change = series_splice::insert_series(sources, map, design, *unit, *net, stmt, moved)?;
                written.push((out.edits.len()..out.edits.len() + change.edits.len(), std::mem::take(&mut change.written)));
                merge(&mut out, change)?;
            }
            DesignOp::Circuit { unit, op } => {
                let edits = match op {
                    CircuitOp::Remove(comp) => circuit_splice::remove(sources, map, design, *unit, *comp)?,
                    CircuitOp::Rename { what, to } => circuit_splice::rename(sources, map, design, *unit, what, to)?,
                    CircuitOp::Connect { pin: (comp, pin), net } => pin_splice::connect(sources, map, design, *unit, *comp, *pin, *net)?,
                    CircuitOp::ConnectNamed { pin: (comp, pin), name } => {
                        let edits = pin_splice::connect_named(sources, map, design, *unit, *comp, *pin, name)?;
                        // A new sheet can follow the last part's e.connect at EOF.
                        for (index, edit) in edits.iter().enumerate() {
                            if edit.span.is_empty() && sources.doc(edit.doc).is_some_and(|d| edit.span.start as usize == d.text.len()) {
                                statements.insert((edit.doc, edit.span.start), out.edits.len() + index);
                            }
                        }
                        edits
                    }
                    CircuitOp::ConnectPins { a, b } => pin_splice::connect_pins(sources, map, design, *unit, *a, *b)?,
                    CircuitOp::Disconnect { pin: (comp, pin) } => pin_splice::disconnect(sources, map, design, *unit, *comp, *pin)?,
                    CircuitOp::NoConnect { pin: (comp, pin), nc } => {
                        let freed = ops[..i].iter().any(|earlier| matches!(earlier,
                            DesignOp::Circuit { unit: u, op: CircuitOp::Disconnect { pin: (c, p) } } if u == unit && c == comp && p == pin));
                        pin_splice::no_connect(sources, map, design, *unit, *comp, *pin, *nc, freed)?
                    }
                    CircuitOp::SetField { .. } | CircuitOp::SetDnp { .. } => continue, // spliced by expression above
                    CircuitOp::SetRef { comp, to } => circuit_splice::set_ref(sources, map, design, *unit, *comp, to.as_deref())?,
                    CircuitOp::SetBody { part, body } => circuit_splice::set_body(sources, map, design, *unit, part, body.as_deref())?,
                    CircuitOp::AddPart { .. } | CircuitOp::InsertSeries { .. } | CircuitOp::Hint { .. } | CircuitOp::NetStyle { .. } |
                    CircuitOp::SetValue { .. } => unreachable!("statement insertions, sheet hints and SetValue are handled above"),
                };
                merge(&mut out, SpanEdit { edits, reevaluate: true, ..SpanEdit::default() })?;
            }
            DesignOp::Bench { bench, op } => {
                let edits = match op {
                    BenchOp::AddProbe { of, label } => {
                        bench_splice::add_probe(sources, map, bench, of, label)?
                    }
                    BenchOp::RemoveProbe { of } => {
                        bench_splice::remove_probe(sources, map, bench, of)?
                    }
                    BenchOp::SetProbeRef { of, reference } => {
                        bench_splice::set_probe_ref(sources, map, bench, of, reference.as_deref())?
                    }
                    BenchOp::SetProbeTip { of, tip } => {
                        bench_splice::set_probe_tip(sources, map, bench, of, tip)?
                    }
                    BenchOp::SetProbeLabel { of, label } => {
                        bench_splice::set_probe_label(sources, map, bench, of, label)?
                    }
                    BenchOp::SetProbeAt { of, at } => {
                        bench_splice::set_probe_at(sources, map, bench, of, at.as_deref())?
                    }
                    BenchOp::SetRange { expect, min, max } => {
                        bench_splice::set_range(sources, map, bench, expect, *min, *max, resolve_drives)?
                    }
                    BenchOp::SetSupply { net, key, value } => {
                        bench_splice::set_stimulus(sources, map, bench, BenchKey::Supply(net.clone()), key, *value, resolve_drives)?
                    }
                    BenchOp::SetSource { a, b, key, value } => {
                        bench_splice::set_stimulus(sources, map, bench, BenchKey::Source(a.clone(), b.clone()), key, *value, resolve_drives)?
                    }
                    BenchOp::AddSupply { net, v } => bench_splice::add_supply(sources, map, bench, net, *v)?,
                    BenchOp::RemoveSupply { net } => bench_splice::remove_supply(sources, map, bench, net)?,
                    BenchOp::AddSource { kind, a, b, opts } => bench_splice::add_source(sources, map, bench, kind, a, b, opts)?,
                    BenchOp::RemoveSource { a, b } => bench_splice::remove_source(sources, map, bench, a, b)?,
                    BenchOp::SetDrive { stub, pin, drive } => {
                        bench_splice::set_drive(sources, map, bench, stub, pin, drive, resolve_drives)?
                    }
                    BenchOp::WriteDrive { stub, pin, drive } => {
                        bench_splice::write_drive(sources, map, bench, stub, pin, drive)?
                    }
                    BenchOp::SetPressed { comp, pressed } => {
                        bench_splice::set_pressed(sources, map, bench, comp, *pressed)?
                    }
                };
                // the statement the op inserts (+ Add probe, a supply, a source) is written where
                // it went in, as an added part's is: its range in the inserted text
                let inserted = op.inserts().then(|| edits.iter().filter(|edit| edit.span.is_empty()).find_map(|edit| {
                    let lead = (edit.text.len() - edit.text.trim_start().len()) as u32;
                    let len = edit.text.trim().len() as u32;
                    (len > 0).then(|| SourceRange { doc: edit.doc, span: edit.span.start + lead..edit.span.start + lead + len })
                })).flatten();
                if let Some(range) = inserted {
                    written.push((out.edits.len()..out.edits.len() + edits.len(), vec![range]));
                }
                merge(
                    &mut out,
                    SpanEdit {
                        edits,
                        reevaluate: true,
                        ..SpanEdit::default()
                    },
                )?;
            }
        }
    }
    // All hinted paths, including generated part names, go into the same literal sheet.
    for (unit, sheet) in sheets {
        let hints: Vec<_> = sheet.hints.iter().map(|(path, hint)| (path.as_str(), *hint)).collect();
        let mut edits = hint_splice::hints(sources, map, design, unit, &hints, &sheet.styles).map_err(|error| {
            match sheet.part_anchor {
                Some(at) => located(at.doc, at.span, error.message),
                None => error,
            }
        })?;
        edits.retain(|edit| {
            if edit.span.is_empty() {
                if let Some(&index) = statements.get(&(edit.doc, edit.span.start)) {
                    out.edits[index].text.push_str(&edit.text);
                    return false;
                }
            }
            true
        });
        merge(&mut out, SpanEdit { edits, reevaluate: true, ..SpanEdit::default() })?;
    }
    // Each insertion knows only its own edits. Recover the original anchor in that op's
    // document, then shift its result by every other op's preceding edit exactly once.
    for (own, ranges) in written {
        for mut range in ranges {
            let mut edits: Vec<_> = out.edits[own.clone()].iter().filter(|edit| edit.doc == range.doc).collect();
            edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
            let mut delta = 0i64;
            let mut anchor = None;
            for edit in edits {
                let start = i64::from(edit.span.start) + delta;
                if start <= i64::from(range.span.start) && i64::from(range.span.end) <= start + edit.text.len() as i64 {
                    anchor = Some((edit.span.start, edit.span.end));
                    break;
                }
                delta += edit.text.len() as i64 - i64::from(edit.span.end - edit.span.start);
            }
            let anchor = anchor.expect("a written statement belongs to one of its op's edits");
            let shift: i64 = out.edits.iter().enumerate().filter(|(index, edit)| !own.contains(index)
                && edit.doc == range.doc && (edit.span.start, edit.span.end) < anchor)
                .map(|(_, edit)| edit.text.len() as i64 - i64::from(edit.span.end - edit.span.start)).sum();
            range.span = (i64::from(range.span.start) + shift) as u32..(i64::from(range.span.end) + shift) as u32;
            out.written.push(range);
        }
    }
    Ok(out)
}

/// Add one op's edits to the commit's; two ops may not edit the same text.
fn merge(out: &mut SpanEdit, e: SpanEdit) -> Result<(), Diag> {
    for a in &e.edits {
        let clash = out.edits.iter().find(|b| b.doc == a.doc && (a.span.start < b.span.end && b.span.start < a.span.end || a.span == b.span));
        if let Some(b) = clash {
            return Err(located(a.doc, a.span.start.min(b.span.start)..a.span.end.max(b.span.end), "two ops of one commit edit the same text: commit them one at a time"));
        }
    }
    out.edits.extend(e.edits);
    out.adds.extend(e.adds);
    out.written.extend(e.written);
    out.removes.extend(e.removes);
    out.present.extend(e.present);
    out.assets.extend(e.assets);
    out.reevaluate |= e.reevaluate;
    Ok(())
}

/// The edits of `SetValue`: where the component's value is written.
fn set_value(sources: &Sources, map: &SourceMap, design: &Design, unit: BoardId, comp: CompId, value: &Value) -> Result<Vec<TextEdit>, Diag> {
    let cref = CompRef { board: unit, comp };
    let circuit = &design.units.iter().find(|u| u.id == unit).ok_or_else(|| Diag::error(format!("the design has no unit {}", unit.0), vec![Sel::Unit(unit)]))?.circuit;
    let c = circuit.comps.get(comp.ix()).ok_or_else(|| Diag::error(format!("unit {} has no component {}", circuit.name, comp.0), vec![Sel::Comp(cref)]))?;
    let fail = |m: String| Diag::error(format!("{}: {m}", c.path), vec![Sel::Comp(cref)]);
    let site = map.value_site(&cref).ok_or_else(|| fail("its call writes no value (give it one: `value: ..`)".into()))?;
    let text_of = |r: &SourceRange| sources.doc(r.doc).and_then(|d| d.text.get(r.span.start as usize..r.span.end as usize));
    let mut edits = match site {
        ValueSite::Literal { at, .. } => {
            let written = text_of(at).ok_or_else(|| fail("its document is not in the project".into()))?;
            Ok(vec![TextEdit { doc: at.doc, span: at.span.clone(), text: literal(value, written).map_err(fail)? }])
        }
        ValueSite::Param { name, item, call, default, .. } => {
            let doc = sources.doc(call.doc).ok_or_else(|| fail("its instance's document is not in the project".into()))?;
            let syntax = syntax::parse_with(&doc.text, &circuit_units()).map_err(|e| located(call.doc, e.at as u32..e.at as u32, e.message))?;
            let span = call.span.start as usize..call.span.end as usize;
            let current = given(&syntax, &span, name);
            let text = match item {
                None => {
                    let written = current.clone().or_else(|| default.as_ref().and_then(text_of).map(str::to_string)).unwrap_or_default();
                    literal(value, &written).map_err(fail)?
                }
                Some(k) => {
                    let block = &circuit.tree[c.block.ix()];
                    let Some((_, ParamValue::List(list))) = block.params.iter().find(|(n, _)| n == name) else {
                        return Err(fail(format!("parameter {name} is not a list")));
                    };
                    let written = current.as_deref().and_then(|t| list_items(t).get(*k).map(|s| s.to_string())).unwrap_or_default();
                    let mut items = Vec::new();
                    for (i, p) in list.iter().enumerate() {
                        items.push(if i == *k { literal(value, &written).map_err(fail)? } else { param_text(p).map_err(fail)? });
                    }
                    let sep = if current.as_deref().is_some_and(|t| t.contains(',')) { ", " } else { " " };
                    format!("[{}]", items.join(sep))
                }
            };
            let (span, text) = override_edit(&syntax, span, name, &text).ok_or_else(|| fail("its instance's call takes no options object to override in".into()))?;
            Ok(vec![TextEdit { doc: call.doc, span: span.start as u32..span.end as u32, text }])
        }
        ValueSite::Library { lib } => Err(fail(format!("the value is written in the {lib} library, which is not edited here"))),
        ValueSite::Derived { at, inputs, .. } => {
            let mut diag = located(
                at.doc,
                at.span.clone(),
                format!("{}: the value is computed by `{}`: edit the expression", c.path, text_of(at).unwrap_or("..")),
            );
            if !inputs.is_empty() {
                diag.message.push_str("; inputs: ");
                diag.message.push_str(&inputs.iter().map(|input| {
                    let written = text_of(&input.at).unwrap_or("..");
                    match (&input.site, input.value) {
                        (ValueSite::Param { name, .. }, _) => format!("`{written}` (parameter {name})"),
                        (_, Some(value)) => format!("`{written}` = {value}"),
                        _ => format!("`{written}`"),
                    }
                }).collect::<Vec<_>>().join(", "));
                for input in inputs {
                    let sel = Sel::Source { doc: input.at.doc, start: input.at.span.start, end: input.at.span.end };
                    if !diag.sel.contains(&sel) {
                        diag.sel.push(sel);
                    }
                }
            }
            Err(diag)
        }
    }?;
    let current = c.value.as_ref().and_then(|v| v.tol);
    if current != value.tol {
        edits.extend(tolerance_edits(sources, map, cref, current, value)?);
    }
    // Inserting a tolerance directly after an edited value, or removing its preceding space,
    // shares that value's boundary. Emit a single replacement for such adjacent edits.
    edits.sort_by_key(|e| (e.doc, e.span.start, e.span.end));
    let mut out: Vec<TextEdit> = Vec::new();
    for edit in edits {
        if let Some(previous) = out.last_mut().filter(|e| e.doc == edit.doc) {
            if previous.span.end > edit.span.start {
                return Err(located(edit.doc, edit.span, "the value and tolerance need overlapping edits: edit the expression"));
            }
            if previous.span.end == edit.span.start {
                previous.span.end = edit.span.end;
                previous.text.push_str(&edit.text);
                continue;
            }
        }
        out.push(edit);
    }
    Ok(out)
}

/// Change only the call's explicit tolerance; absence keeps the part's default tolerance.
fn tolerance_edits(sources: &Sources, map: &SourceMap, comp: CompRef, current: Option<Tol>, value: &Value) -> Result<Vec<TextEdit>, Diag> {
    let at = map.locate(&Sel::Comp(comp)).ok_or_else(|| Diag::error("the component has no mapped call", vec![Sel::Comp(comp)]))?;
    let fail = |message| located(at.doc, at.span.clone(), message);
    let doc = sources.doc(at.doc).ok_or_else(|| fail("the tolerance is written in a library or a document outside the project"))?;
    let syntax = syntax::parse_with(&doc.text, &circuit_units()).map_err(|e| located(at.doc, e.at as u32..e.at as u32, e.message))?;
    let call = at.span.start as usize..at.span.end as usize;
    let node = call_at(&syntax, &call).ok_or_else(|| fail("the component's mapped call is missing"))?;
    let Kind::Call { args, open, close, .. } = &node.kind else { unreachable!() };
    if !matches!(node.callee_path(), Some("e.r" | "e.c" | "e.l")) {
        return Err(fail("this builder does not take a tolerance"));
    }
    let positional = map.pin_sites(&comp, PinIx(0)).iter().find_map(|site| match site {
        PinSite::Item { positional_tol, .. } => Some(*positional_tol),
        _ => None,
    }).unwrap_or(false);
    let arg = tol_arg(&syntax, call.clone(), positional);
    let options = args.get(two_pin_net_arg(1, positional) + 1);
    let computed = |span: Range<usize>| located(at.doc, span.start as u32..span.end as u32,
        format!("the tolerance is computed by `{}`: edit the expression", &doc.text[span]));
    if let Some(arg) = &arg {
        if let Some(item) = arg.items.iter().find(|item| item.suffix.is_none()) {
            return Err(computed(item.span.clone()));
        }
    } else if current.is_some() {
        return Err(computed(options.map_or_else(|| call.clone(), |o| o.span.clone())));
    }
    let edit = |(span, text): (Range<usize>, String)| TextEdit { doc: at.doc, span: span.start as u32..span.end as u32, text };
    let mut out = Vec::new();
    let remove_positional = || {
        let spans: Vec<_> = args.iter().map(|a| a.span.clone()).collect();
        edit(pin_splice::entry_removal(&doc.text, *open, *close, &spans, 1, 1))
    };
    let remove_option = || -> Option<TextEdit> {
        let Kind::Object { fields, open, close, .. } = &options?.kind else { return None };
        let index = fields.iter().position(|f| f.name.as_deref() == Some("tol"))?;
        let spans: Vec<_> = fields.iter().map(|f| f.span.clone()).collect();
        Some(edit(pin_splice::entry_removal(&doc.text, *open, *close, &spans, index, index)))
    };
    let Some(tol) = value.tol else {
        if positional { out.push(remove_positional()); }
        else if let Some(edit) = remove_option() { out.push(edit); }
        return Ok(out);
    };
    let (minus, plus) = match tol { Tol::Pct { minus, plus } | Tol::Abs { minus, plus } => (minus, plus) };
    if !minus.is_finite() || !plus.is_finite() || minus < 0.0 || plus < 0.0 {
        return Err(fail("a tolerance must have finite, non-negative magnitudes"));
    }
    let q = value.qty.as_ref().ok_or_else(|| fail("an absolute tolerance needs a quantity unit"))?;
    let value_arg = args.first().ok_or_else(|| fail("the component has no value argument"))?;
    let bound = |magnitude: f64, index: usize| -> Result<String, Diag> {
        let written = arg.as_ref().and_then(|arg| arg.items.get(index).or_else(|| arg.items.first()))
            .map(|item| &doc.text[item.span.clone()]);
        match tol {
            Tol::Pct { .. } => percent_literal(magnitude, written).map_err(|m| located(at.doc, at.span.clone(), m)),
            Tol::Abs { .. } => Ok(literal_for(&Quantity { si: magnitude, unit: q.unit }, written.unwrap_or_else(|| syntax.text(value_arg)))),
        }
    };
    if minus == plus {
        let text = bound(plus, 0)?;
        if positional {
            out.push(edit((arg.as_ref().expect("a positional tolerance has syntax").span.clone(), text)));
        } else {
            out.push(edit((value_arg.span.end..value_arg.span.end, format!(" {text}"))));
            if let Some(edit) = remove_option() { out.push(edit); }
        }
    } else {
        if let Some(options) = options.filter(|o| !matches!(o.kind, Kind::Object { .. })) {
            return Err(computed(options.span.clone()));
        }
        let text = format!("[-{}, {}]", bound(minus, 0)?, bound(plus, 1)?);
        out.push(edit(override_edit(&syntax, call, "tol", &text).ok_or_else(|| fail("the call takes no options object to override in"))?));
        if positional { out.push(remove_positional()); }
    }
    Ok(out)
}

/// The shortest decimal at the reader's percent precision, retaining a written ppm suffix.
fn percent_literal(percent: f64, written: Option<&str>) -> Result<String, String> {
    let ppm = written.map_or(percent < 0.1, |text| text.trim().ends_with("ppm"));
    let suffix = if ppm { "ppm" } else { "%" };
    let units = circuit_units();
    let entry = units.entry(units.lookup(suffix).expect("tolerance suffix is registered"));
    let number = if ppm { percent * 1e4 } else { percent };
    for places in 0..=17 {
        let mut digits = format!("{number:.places$}");
        if digits.contains('.') {
            digits.truncate(digits.trim_end_matches('0').trim_end_matches('.').len());
        }
        if tolerance_percent(entry.literal(&digits)) == percent {
            return Ok(format!("{digits}{suffix}"));
        }
    }
    Err("the tolerance cannot be written exactly at 1e-9 percent precision".into())
}

/// The text of field `name` of the options object of the call at `span`, when written.
fn given(syntax: &Syntax, span: &Range<usize>, name: &str) -> Option<String> {
    let mut out = None;
    syntax.walk(&mut |_, n| {
        if out.is_none() && n.span == *span {
            if let Kind::Call { args, .. } = &n.kind {
                out = args.first().and_then(|o| o.field(name)).map(|f| syntax.text(&f.value).to_string());
            }
        }
    });
    out
}

/// The item texts of a list literal (`[100nF 100nF 4.7uF]`, `[1, 2]`).
fn list_items(text: &str) -> Vec<&str> {
    let inner = text.trim().trim_start_matches('[').trim_end_matches(']');
    inner.split([',', ' ', '\n', '\t']).map(str::trim).filter(|s| !s.is_empty()).collect()
}

/// A value as a literal written like `written` (its suffix; `@name` or a string for a label).
fn literal(value: &Value, written: &str) -> Result<String, String> {
    match &value.qty {
        Some(q) if q.si.is_finite() => Ok(literal_for(q, written)),
        Some(q) => Err(format!("{} is not a finite value", q.si)),
        None if written.trim_start().starts_with('@') => {
            let ok = !value.text.is_empty() && value.text.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') && !value.text.starts_with(|ch: char| ch.is_ascii_digit());
            if ok { Ok(format!("@{}", value.text)) } else { Err(format!("{:?} is not a name (@{})", value.text, value.text)) }
        }
        None => Ok(syntax::write::quote(&value.text)),
    }
}

/// A parameter value as a literal.
pub(crate) fn param_text(p: &ParamValue) -> Result<String, String> {
    Ok(match p {
        ParamValue::Qty(q) if q.si.is_finite() => literal_text(q),
        ParamValue::Qty(q) => return Err(format!("{} has no quantity literal", q.si)),
        ParamValue::Int(i) => i.to_string(),
        ParamValue::Num(n) => syntax::write::number(*n)?,
        ParamValue::Bool(b) => b.to_string(),
        ParamValue::Text(t) => syntax::write::quote(t),
        ParamValue::Sym(s) => format!("@{s}"),
        ParamValue::List(l) => format!("[{}]", l.iter().map(param_text).collect::<Result<Vec<_>, _>>()?.join(" ")),
    })
}

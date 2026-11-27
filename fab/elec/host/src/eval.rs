//! Evaluation work for the pool; the strand adopts only results of its current documents.

use crate::TaskResult;
use fab_elec_board::BoardOp;
use fab_elec_circuit::CircuitOp;
use fab_elec_core::{Angle, BoardId, Canon, CompRef, Diag, Sel, Side};
use fab_elec_design::{Design, DesignOp, PartRef, Place, TextEdit, TextStep};
use fab_elec_lang::{bench_views, catalog, evaluate_from, splice_verified, text_verified, BenchView, EvalError, Evaluated, Libs, Sources, SpanEdit, Verified, NO_CANCEL};
use fab_elec_view::board::{plan::placement_labels, tray::placement_op};
use fab_elec_view::snapshot::RunClock;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A pool task: evaluate the documents again, or splice and verify a canvas commit against them.
#[derive(Clone)]
pub struct EvalJob {
    /// The engine's job identity, so late results can be ignored.
    pub id: u64,
    /// The preview sample sequence; None for committed evaluation and verification.
    pub preview: Option<u64>,
    /// The documents as the engine held them when it made the job.
    pub sources: Sources,
    /// The project's library snapshot, shared with the pool.
    pub libs: Arc<Libs>,
    /// The engine's own evaluation state (its design, the map followed to `sources`, units, benches,
    /// notes, system pass): `evaluate_from` reuses every unit whose texts are unchanged.
    pub prev: Arc<Evaluated>,
    /// The views of `prev`'s benches, kept as the result's Arc while the views stay equal.
    pub benches: Arc<Vec<BenchView>>,
    /// A canvas commit's circuit, bench and board ops to splice and verify; None evaluates `sources` as they are.
    pub verify: Option<Vec<DesignOp>>,
    /// A checked text change (`Cmd::CheckedText`) to apply and verify instead.
    pub text: Option<Vec<TextEdit>>,
    /// A document set already planned against `sources`, with the clock shared by its row.
    pub plan: Option<(Sources, SpanEdit, Arc<RunClock>)>,
    /// Footprints resolved after verification has added the sample's or commit's parts.
    pub place: Vec<Place>,
}

/// One pool result, carrying the board comparisons that keep hashing off the strand.
pub struct EvalDone {
    /// The job identity to match against the engine's in-flight evaluation.
    pub id: u64,
    /// The preview sample sequence echoed from the job.
    pub preview: Option<u64>,
    /// Evaluated documents or a verified commit, including any diagnostics.
    pub out: EvalOut,
    /// New board items produced by placement, beyond the circuit's written selections.
    pub placed: Vec<Sel>,
    /// Units whose newly lowered board has the same content (`Canon::content_hash`) as `prev`'s
    /// board: adoption keeps the engine's board Arc (no generation move, no refill).
    /// Cost: O(board) hashing per changed unit, on the pool.
    pub same_boards: Vec<BoardId>,
    /// Every evaluated bench's view (`fab_elec_lang::bench_views`): the job's Arc when they equal
    /// its views or the evaluation failed. Cost: one parse of each bench document and one
    /// comparison of the views, on the pool.
    pub benches: Arc<Vec<BenchView>>,
}

/// Evaluation and verification have different adoption paths and refusal diagnostics.
pub enum EvalOut {
    /// The documents evaluated as they stood at job creation.
    Evaluated(Result<Evaluated, Vec<Diag>>),
    /// The verified splice and the text step that undoes its edit (`SpanEdit::inverse` against `sources` and `prev.map`).
    Verified(Result<(Verified, TextStep), Diag>),
}

impl EvalJob {
    /// Mark a proposal running as the pool enters the job. Cost: evaluation of changed units (and a splice for verification),
    /// plus O(board) hashing per changed unit, the bench views and the Library panel's catalog
    /// (one enumeration VM when a definition document changed, else one cache read), all on the pool.
    pub fn run(&self) -> TaskResult {
        if let Some((_, _, clock)) = &self.plan { clock.start(); }
        let mut placed = Vec::new();
        let out = match (&self.verify, &self.text, &self.plan) {
            (_, _, Some((sources, edit, _))) => EvalOut::Verified(
                fab_elec_lang::docs_verified(sources.clone(), &self.libs, &self.prev, edit.clone(), &NO_CANCEL)
                    .map(|verified| {
                        let inverse = verified.edit.inverse(&self.sources, &self.prev.map).into();
                        (verified, inverse)
                    }).map_err(first_mistake)),
            (_, Some(edits), None) => EvalOut::Verified(text_verified(&self.sources, &self.libs, &self.prev, edits.clone(), &NO_CANCEL)
                .map(|verified| {
                    let inverse = verified.edit.inverse(&self.sources, &self.prev.map).into();
                    (verified, inverse)
                })
                .map_err(first_mistake)),
            (None, None, None) => EvalOut::Evaluated(evaluate_from(Some(&self.prev), &self.sources, &self.libs, &NO_CANCEL).map_err(EvalError::into_diags)),
            (Some(ops), None, None) => EvalOut::Verified(self.verify(ops).map(|(verified, items)| {
                // A commit that adds a document and edits it (a part's first e.body, in a new
                // models document) inverts against the text with that document added.
                let inverse = if ops.iter().any(|op| matches!(op, DesignOp::File(_))) {
                    verified.edit.inverse(&fab_elec_lang::with_files(&self.sources, ops), &self.prev.map)
                } else {
                    verified.edit.inverse(&self.sources, &self.prev.map)
                }.into();
                placed = items;
                (verified, inverse)
            })),
        };
        let evaluated = match &out {
            EvalOut::Evaluated(Ok(ev)) => Some((ev, &self.sources)),
            EvalOut::Verified(Ok((verified, _))) => Some((&verified.evaluated, &verified.sources)),
            _ => None,
        };
        let same_boards = evaluated.into_iter().flat_map(|(ev, _)| &ev.design.units).filter_map(|unit| {
            let board = unit.board.as_ref()?;
            let before = self.prev.design.unit(unit.id)?.board.as_ref()?;
            (Arc::ptr_eq(board, before) || board.content_hash() == before.content_hash()).then_some(unit.id)
        }).collect();
        if let Some((ev, _)) = evaluated {
            // Fill the catalog the result carries, so its adoption reads it (`Docs::adopt_evaluated`).
            catalog(&ev.libs, ev);
        }
        let benches = match evaluated.map(|(ev, sources)| bench_views(ev, sources)) {
            Some(views) if views != *self.benches => Arc::new(views),
            _ => self.benches.clone(),
        };
        TaskResult::Eval(Box::new(EvalDone { id: self.id, preview: self.preview, out, placed, same_boards, benches }))
    }

    fn verify(&self, ops: &[DesignOp]) -> Result<(Verified, Vec<Sel>), Diag> {
        let first = splice_verified(&self.sources, &self.libs, &self.prev, ops, &NO_CANCEL).map_err(EvalError::into_diag)?;
        if self.place.is_empty() {
            return Ok((first, vec![]));
        }
        let mut added = Vec::new();
        let mut written = first.edit.written.iter();
        for op in ops {
            let DesignOp::Circuit { unit, op } = op else {
                // a bench op's new statement is written too, in op order (`splice`)
                if matches!(op, DesignOp::Bench { op, .. } if op.inserts()) {
                    let _ = written.next();
                }
                continue;
            };
            let CircuitOp::AddPart { stmt, .. } = op else {
                if matches!(op, CircuitOp::InsertSeries { .. }) {
                    let _ = written.next();
                }
                continue;
            };
            let at = written.next();
            let fail = || Diag::error(
                format!("placing {stmt} in the same step needs exactly one new part (a block is placed after it is added)"),
                vec![at.map_or(Sel::Unit(*unit), |at| Sel::Source {
                    doc: at.doc, start: at.span.start, end: at.span.end,
                })],
            );
            let at = at.ok_or_else(fail)?;
            let mut parts = first.evaluated.map.of_doc(at.doc).into_iter().filter_map(|(sel, range)| {
                if range.span.start < at.span.start || range.span.end > at.span.end || self.prev.map.locate(&sel).is_some() {
                    return None;
                }
                match sel {
                    Sel::Comp(comp) => Some(comp),
                    _ => None,
                }
            });
            let part = parts.next().ok_or_else(fail)?;
            if parts.next().is_some() {
                return Err(fail());
            }
            added.push(part);
        }
        let ops = place_ops(&first.evaluated.design, &self.place, &added).map_err(|mut diag| {
            if let Some(at) = diag.sel.iter().find_map(|sel| first.evaluated.map.locate(sel)) {
                diag.sel.push(Sel::Source { doc: at.doc, start: at.span.start, end: at.span.end });
            }
            diag
        })?;
        let placed = ops.iter().filter_map(|op| match op {
            DesignOp::Board { unit, op: BoardOp::Add(item) } => Some(Sel::Item(*unit, item.id())),
            _ => None,
        }).collect();
        let second = splice_verified(&first.sources, &self.libs, &first.evaluated, &ops, &NO_CANCEL).map_err(EvalError::into_diag)?;
        let mut also = first.also;
        also.extend(second.also);
        Ok((Verified {
            edit: compose(first.edit, second.edit)?, sources: second.sources, evaluated: second.evaluated, also,
        }, placed))
    }
}

/// How a checked text change's refusal for a mistake it adds begins (`Cmd::CheckedText`), so its
/// caller tells it from a refusal of the change itself (a document that moved on).
pub const MISTAKE: &str = "the change adds a mistake: ";

/// A checked text change's refusal: its first new mistake where it is, and the others' words, so
/// one answer names them all. O(mistakes).
fn first_mistake(error: EvalError) -> Diag {
    let mut diags = error.into_diags().into_iter();
    let mut first = diags.next().unwrap_or_else(|| Diag::error("the documents do not evaluate", vec![]));
    let rest: Vec<String> = diags.map(|diag| diag.message).collect();
    first.message = if rest.is_empty() {
        format!("{MISTAKE}{}", first.message)
    } else {
        format!("{MISTAKE}{}; and {} more: {}", first.message, rest.len(), rest.join("; "))
    };
    first
}

/// Resolve placements in order against a working design, then put each placed part's reference
/// label, and the rows of the parts around it, beside their parts in the same step: the rule the
/// board pane's own drops follow (`plan::placement_labels`), so a tray drop, the palette and the AI
/// place alike.
/// Cost: O(places x (components + one board op)), plus per board one clone and one label pass over
/// what lies within reach of the placed parts' labels.
pub(crate) fn place_ops(before: &Design, place: &[Place], added: &[CompRef]) -> Result<Vec<DesignOp>, Diag> {
    if place.is_empty() {
        return Ok(vec![]);
    }
    let mut design = before.clone();
    let mut ops = Vec::with_capacity(place.len());
    for place in place {
        let unit = design.unit(place.unit).ok_or_else(|| Diag::error(
            format!("the design has no unit {}", place.unit.0), vec![Sel::Unit(place.unit)],
        ))?;
        let comp = match &place.part {
            PartRef::Added(k) => *added.get(*k as usize).ok_or_else(|| Diag::error(
                format!("Added({k}) names no AddPart of this commit"), vec![Sel::Unit(unit.id)],
            ))?,
            PartRef::Path(path) => CompRef {
                board: unit.id,
                comp: unit.circuit.comp(path).ok_or_else(|| Diag::error(
                    format!("{path} is not a part of {}", unit.name), vec![Sel::Unit(unit.id)],
                ))?.id,
            },
        };
        let part = unit.circuit.comps.iter().find(|part| comp.board == unit.id && part.id == comp.comp)
            .ok_or_else(|| Diag::error(
                format!("{:?} is not a part of {}", place.part, unit.name), vec![Sel::Comp(comp), Sel::Unit(unit.id)],
            ))?;
        let fail = |why: String| Diag::error(format!("{} cannot be placed: {why}", part.path), vec![Sel::Comp(comp)]);
        let op = placement_op(&design, unit.id, comp.comp, place.at, place.rot.unwrap_or(Angle::ZERO), place.side.unwrap_or(Side::Top))
            .ok_or_else(|| fail(match &unit.board {
                None => format!("{} has no board", unit.name),
                Some(board) if board.placed.iter().any(|placed| placed.comp == comp.comp) => "already placed".into(),
                Some(_) => "no footprint".into(),
            }))?;
        let selection = Sel::Comp(comp);
        design.apply(&op, false).map_err(|error| Diag::error(format!("{error}"), vec![selection]))?;
        ops.push(op);
    }
    let labels = placement_labels(before, &ops);
    ops.extend(labels);
    Ok(ops)
}

/// Move second edits back to original coordinates and first adds/written forward to final coordinates.
/// Cost: O(edits x edits of the same document).
fn compose(mut first: SpanEdit, second: SpanEdit) -> Result<SpanEdit, Diag> {
    let mut docs = BTreeMap::new();
    for edit in &first.edits {
        docs.entry(edit.doc).or_insert_with(Vec::new).push(edit);
    }
    let mut writes = BTreeMap::new();
    for (doc, mut edits) in docs {
        edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
        let mut shift = 0i64;
        let ranges = edits.into_iter().map(|edit| {
            let start = i64::from(edit.span.start) + shift;
            let delta = edit.text.len() as i64 - i64::from(edit.span.end - edit.span.start);
            shift += delta;
            (start..start + edit.text.len() as i64, delta)
        }).collect::<Vec<_>>();
        writes.insert(doc, ranges);
    }
    let mut following = BTreeMap::new();
    for edit in &second.edits {
        following.entry(edit.doc).or_insert_with(Vec::new).push(edit);
    }
    for range in first.adds.iter_mut().map(|(_, range)| range).chain(&mut first.written) {
        let shift: i64 = following.get(&range.doc).into_iter().flatten()
            .filter(|edit| edit.span.end < range.span.start)
            .map(|edit| edit.text.len() as i64 - i64::from(edit.span.end - edit.span.start)).sum();
        range.span = (i64::from(range.span.start) + shift) as u32..(i64::from(range.span.end) + shift) as u32;
    }
    for mut edit in second.edits {
        let mut shift = 0i64;
        for (written, delta) in writes.get(&edit.doc).into_iter().flatten() {
            if i64::from(edit.span.start) <= written.end && written.start <= i64::from(edit.span.end) {
                return Err(Diag::error(
                    "placing a part and editing the same text in one step: commit them one at a time",
                    vec![Sel::Source { doc: edit.doc, start: edit.span.start, end: edit.span.end }],
                ));
            }
            if written.end < i64::from(edit.span.start) {
                shift += delta;
            }
        }
        edit.span = (i64::from(edit.span.start) - shift) as u32..(i64::from(edit.span.end) - shift) as u32;
        first.edits.push(edit);
    }
    first.adds.extend(second.adds);
    first.written.extend(second.written);
    first.present.extend(second.present);
    first.assets.extend(second.assets);
    for sel in second.removes {
        if !first.removes.contains(&sel) {
            first.removes.push(sel);
        }
    }
    first.reevaluate |= second.reevaluate;
    Ok(first)
}

//! Resolve a board edit's requested poses on the engine strand, before it commits.

use crate::edit_actions::{arg, error, Args};
use crate::Plan;
use fab_elec_board::{Board, BoardOp, Field, Item};
use fab_elec_core::{CompId, Diag, ItemId, P};
use fab_elec_design::action::{FieldDef, FieldKind};
use fab_elec_design::{DesignOp, Resolved, Unit};
use fab_elec_route::place::{edge_parts, legalise, LegalOpts, PlaceProblem};
use fab_elec_view::board::plan::label_ops;
use makepad_widgets_core::makepad_script::syntax::write::quote;
use std::collections::{HashMap, HashSet};

pub(crate) const LEGALISE: FieldDef = FieldDef {
    name: "legalise", kind: FieldKind::Choice(&["nearest", "off"]), required: false, default: "nearest",
    help: "nearest: each part moved goes to the legal spot nearest where it was asked; off: exactly where asked.",
};

#[derive(Clone, Copy)]
pub(crate) enum PlacementEdit<'a> {
    Move(ItemId),
    /// The deferred Place's add op, built by the tray's placement_op.
    Tray(&'a DesignOp),
    /// The view's group plan already includes label ops for its selection.
    Group(&'a [CompId]),
}

pub(crate) fn legal_plan(
    unit: &Unit, args: &Args, mut edit: Resolved, placement: PlacementEdit<'_>,
) -> Result<Plan, Diag> {
    let original = unit.board.as_deref().expect("a placement plan has a board");
    let notes = if arg(args, "legalise").is_some_and(|mode| mode.trim().trim_start_matches('@') == "off") {
        Vec::new()
    } else {
        adjust(unit, &mut edit, placement)?
    };
    if let PlacementEdit::Move(item) = placement {
        edit.ops.extend(label_ops(unit.id, original, &edit.ops, &[item]));
    }
    Ok(if notes.is_empty() { Plan::Edit(edit) } else { Plan::Noted { edit, notes } })
}

fn adjust(unit: &Unit, edit: &mut Resolved, placement: PlacementEdit<'_>) -> Result<Vec<(String, String)>, Diag> {
    let original = unit.board.as_deref().expect("a placement plan has a board");
    let mut board = original.clone();
    let mut move_ops = HashMap::new();
    let mut moved_comps = Vec::new();
    let drop = match placement { PlacementEdit::Tray(op) => Some(op), _ => None };
    for (i, op) in edit.ops.iter().chain(drop).enumerate() {
        let DesignOp::Board { op, .. } = op else { unreachable!("placement actions plan board ops") };
        board.apply(op).map_err(|why| error("part", why))?;
        // Only pose ops make a part movable. Labels, released ends and stationary alignment
        // references or distribution endpoints must not bring their owners into the scope.
        let item = match op {
            BoardOp::Move { item, .. } => {
                move_ops.insert(*item, i);
                *item
            }
            BoardOp::SetField { item, field: Field::Rot(_) | Field::Side(_) } => *item,
            BoardOp::Add(Item::Placed(p)) => p.id,
            _ => continue,
        };
        let part = board.placed_item(item).expect("a pose op names a placement");
        if part.locked { return Err(error("part", format_args!("{} is locked", part.refdes))); }
        moved_comps.push(part.comp);
    }
    // An empty scope means the whole board to PlaceProblem.
    if moved_comps.is_empty() { return Ok(Vec::new()); }
    let corrections = corrections(unit, &board, &moved_comps, Vec::new());
    corrections.apply(unit, edit, &move_ops)?;
    if let PlacementEdit::Group(selected) = placement {
        if !corrections.moves.is_empty() {
            // The view arranged labels around the requested poses. Use the final poses instead.
            edit.ops.retain(|op| !matches!(op, DesignOp::Board { op: BoardOp::SetField { field: Field::Label(..), .. }, .. }));
            let selected: HashSet<_> = selected.iter().copied().collect();
            let ids: Vec<_> = original.placed.iter().filter(|p| selected.contains(&p.comp)).map(|p| p.id).collect();
            edit.ops.extend(label_ops(unit.id, original, &edit.ops, &ids));
        }
    }
    Ok(corrections.notes)
}

/// Tidy existing placements directly on the committed board. An empty scope covers the board.
pub(crate) fn legalise_scope(unit: &Unit, scope: &[CompId], pin: Vec<CompId>) -> Result<Plan, Diag> {
    let board = unit.board.as_deref().ok_or_else(|| error("unit", format_args!("{} has no board", unit.name)))?;
    let corrections = corrections(unit, board, scope, pin);
    if corrections.moves.is_empty() {
        return Err(Diag::error("Every part already sits on a legal spot.", vec![]));
    }
    let mut edit = Resolved { label: "Tidy parts".into(), ..Default::default() };
    corrections.apply(unit, &mut edit, &HashMap::new())?;
    // label_ops visits the moved items and their neighbours.
    edit.ops.extend(label_ops(unit.id, board, &edit.ops, &[]));
    Ok(Plan::Noted { edit, notes: corrections.notes })
}

struct Corrections {
    moves: Vec<(ItemId, P)>,
    notes: Vec<(String, String)>,
}

fn corrections(unit: &Unit, board: &Board, scope: &[CompId], pin: Vec<CompId>) -> Corrections {
    let problem = PlaceProblem::new(board, scope);
    let mut poses: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
    let opts = LegalOpts { pin, edge: edge_parts(&unit.circuit), ..Default::default() };
    let report = legalise(board, &problem, &mut poses, &opts);
    let part = |i: usize| board.placed_item(problem.movables[i].item).expect("a movable has a placement");
    let mut moves = Vec::new();
    let mut notes = Vec::new();
    if !report.moved.is_empty() {
        let mut rows = Vec::new();
        for (i, distance) in report.moved {
            let p = part(i);
            moves.push((p.id, poses[i].at));
            rows.push(format!("{{part: {} moved_mm: {:.3}}}", quote(&p.refdes), f64::from(distance) / 1_000_000.0));
        }
        notes.push(("legalised".into(), format!("[{}]", rows.join(" "))));
    }
    if !report.unplaced.is_empty() {
        let names: Vec<_> = report.unplaced.into_iter().map(|i| quote(&part(i).refdes)).collect();
        notes.push(("no_room".into(), format!("[{}]", names.join(" "))));
    }
    Corrections { moves, notes }
}

impl Corrections {
    /// Write moves relative to the committed board, replacing any requested move in place.
    fn apply(&self, unit: &Unit, edit: &mut Resolved, move_ops: &HashMap<ItemId, usize>) -> Result<(), Diag> {
        let original = unit.board.as_deref().expect("a placement plan has a board");
        for &(item, at) in &self.moves {
            if let Some(old) = original.placed_item(item) {
                let delta = |a: i32, b| a.checked_sub(b).ok_or_else(|| error("at", "move is outside the board coordinate range"));
                let by = P::new(delta(at.x, old.at.x)?, delta(at.y, old.at.y)?);
                let op = DesignOp::Board { unit: unit.id, op: BoardOp::Move { item, by } };
                if let Some(&index) = move_ops.get(&item) {
                    edit.ops[index] = op;
                } else {
                    edit.ops.push(op);
                }
            } else {
                edit.place[0].at = at;
            }
        }
        Ok(())
    }
}

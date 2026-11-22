//! One press order across the model and electronics histories, independent of the UI.

use std::collections::VecDeque;

/// Which history a press goes to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// The model worker owns this step.
    Model,
    /// The electronics engine owns this step.
    Elec,
}

/// A history's ends, numbered by the shared counter so presses can choose between histories.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Tops {
    /// The newest undo step, compared with the other history's newest step.
    pub undo: Option<u64>,
    /// The next redo step, compared with the other history's earliest undone step.
    pub redo: Option<u64>,
}

/// Undo and redo presses in order, one in flight so each uses the tops the previous one left.
#[derive(Default)]
pub struct UndoOrder {
    /// Presses still owed, with true for undo, kept in arrival order.
    owed: VecDeque<bool>,
    /// The sent press that must finish before routing another.
    waiting: Option<Wait>,
}

/// The history a sent press went to: it is over when that host is no longer busy, so the next
/// press cannot overtake it. A model press makes the model host busy at once (its history job
/// counts in `ModelHost::building` from the queue until the app takes its end, after its Done),
/// an electronics press the electronics host (its command waits in the mailbox), so a press that
/// never answers cannot hold the ones after it.
enum Wait {
    Model,
    Elec,
}

/// Undo picks the larger newest step; redo picks the smaller next step (earliest undone).
/// When neither has a step, the model answers that there is nothing to undo or redo.
pub fn route(undo: bool, model: Tops, elec: Tops) -> Target {
    let (model, elec) = if undo { (model.undo, elec.undo) } else { (model.redo, elec.redo) };
    match (model, elec) {
        (Some(model), Some(elec)) if if undo { elec > model } else { elec < model } => Target::Elec,
        (None, Some(_)) => Target::Elec,
        _ => Target::Model,
    }
}

impl UndoOrder {
    /// Keep a press until both histories can route it against settled tops.
    pub fn press(&mut self, undo: bool) {
        self.owed.push_back(undo);
    }

    /// The next press, or None while either host is busy: a sent press or other work. Model work
    /// stays busy through its untaken result (`ModelHost::building`), so its tops are those its
    /// Done brought; electronics through queued or running work, whose step brings its tops.
    pub fn next(&mut self, model: Tops, elec: Tops, model_busy: bool, elec_busy: bool) -> Option<(bool, Target)> {
        match self.waiting {
            Some(Wait::Model) if !model_busy => self.waiting = None,
            Some(Wait::Elec) if !elec_busy => self.waiting = None,
            _ => {}
        }
        if self.waiting.is_some() || model_busy || elec_busy {
            return None;
        }
        self.owed.pop_front().map(|undo| (undo, route(undo, model, elec)))
    }

    /// Presses are owed, or the one sent has not finished: the histories' tops are on their way.
    pub fn busy(&self, model_busy: bool, elec_busy: bool) -> bool {
        !self.owed.is_empty()
            || match self.waiting {
                Some(Wait::Model) => model_busy,
                Some(Wait::Elec) => elec_busy,
                None => false,
            }
    }

    /// The caller sent the press `next` returned to `target`.
    pub fn sent(&mut self, target: Target) {
        self.waiting = Some(match target {
            Target::Model => Wait::Model,
            Target::Elec => Wait::Elec,
        });
    }
}

/// Routing and queue properties, plus the real histories sharing one counter.
#[cfg(test)]
mod tests {
    use super::*;

    /// Undo and redo choose opposite ends of the shared order, including empty histories.
    #[test]
    fn route_uses_step_order_and_empty_history_fallbacks() {
        let tops = |step| Tops { undo: step, redo: step };
        for (model, elec, undo, redo) in [
            (Some(3), Some(2), Target::Model, Target::Elec),
            (Some(2), Some(3), Target::Elec, Target::Model),
            (Some(2), None, Target::Model, Target::Model),
            (None, Some(2), Target::Elec, Target::Elec),
            (None, None, Target::Model, Target::Model),
        ] {
            assert_eq!(route(true, tops(model), tops(elec)), undo, "undo picks the larger available top, or Model when empty: {model:?}, {elec:?}");
            assert_eq!(route(false, tops(model), tops(elec)), redo, "redo picks the smaller available top, or Model when empty: {model:?}, {elec:?}");
        }
        let model = Tops { undo: Some(3), redo: Some(1) };
        let elec = Tops { undo: Some(2), redo: Some(4) };
        assert_eq!(route(true, model, elec), Target::Model, "undo reads undo tops independently of redo tops");
        assert_eq!(route(false, model, elec), Target::Model, "redo reads redo tops independently of undo tops");
    }

    /// A rapid second press waits for completion and uses the newly supplied history tops.
    #[test]
    fn double_presses_wait_and_route_against_the_new_tops() {
        for undo in [true, false] {
            let first = Tops { undo: Some(3), redo: Some(1) };
            let other = Tops { undo: Some(2), redo: Some(2) };
            let mut order = UndoOrder::default();
            order.press(undo);
            order.press(undo);
            assert_eq!(order.next(first, other, false, false), Some((undo, Target::Model)), "the first model press is returned once");
            order.sent(Target::Model);
            assert_eq!(order.next(first, other, true, false), None, "the model's own history job holds the second press");
            assert_eq!(order.next(other, first, false, false), Some((undo, Target::Elec)), "the second press uses the tops after the model's job");
            assert_eq!(order.next(first, other, false, false), None, "the model double press consumes exactly two presses");

            let mut order = UndoOrder::default();
            order.press(undo);
            order.press(undo);
            assert_eq!(order.next(other, first, false, false), Some((undo, Target::Elec)), "the first electronics press is returned once");
            order.sent(Target::Elec);
            assert_eq!(order.next(first, other, false, true), None, "electronics work holds the second press");
            assert_eq!(order.next(first, other, false, false), Some((undo, Target::Model)), "the second press uses the tops after electronics becomes idle");
            assert_eq!(order.next(other, first, false, false), None, "the electronics double press consumes exactly two presses");
        }
    }

    /// Unrelated host work holds presses too, without losing their arrival order or direction.
    #[test]
    fn busy_hosts_gate_presses_without_one_in_flight() {
        let mut order = UndoOrder::default();
        let tops = Tops::default();
        order.press(true);
        order.press(false);
        for (model_busy, elec_busy) in [(true, false), (false, true), (true, true)] {
            assert_eq!(order.next(tops, tops, model_busy, elec_busy), None, "either busy host gates an unsent press");
        }
        assert_eq!(order.next(tops, tops, false, false), Some((true, Target::Model)), "busy gating retains the first press");
        order.sent(Target::Model);
        assert_eq!(order.next(tops, tops, true, false), None, "a sent model press holds the next while the model is busy");
        assert_eq!(order.next(tops, tops, false, false), Some((false, Target::Model)), "busy gating retains the second press's direction and order");
        assert_eq!(order.next(tops, tops, false, false), None, "the queue is empty after both retained presses");
    }

    /// Real model and board commits undo and redo in one order, and a board commit kills model redo.
    #[test]
    fn model_and_engine_share_history_order_and_redo_invalidation() {
        use fab_elec_board::{BoardOp, Factories};
        use fab_elec_core::{BoardId, Canon, P};
        use fab_elec_design::{By, Design, DesignOp};
        use fab_elec_host::{Cmd, Engine, Mailbox};
        use fab_model::{eval::Evaluator, Change, Edit, FabModel, HistoryAction};
        use fab_plate::machine::Machine;
        use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
        use std::sync::Arc;

        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let source = |width| format!("{}object(m.box({{size: [{width}mm 1mm 1mm]}}))\n", fab_model::program::EMPTY);
        let mut model = FabModel::with_script("shared history", "block.cad.splash", &source(1));
        model.set_steps(steps.clone(), logged.clone());
        let mut eval = Evaluator::new(Machine::dual_nozzle_350());
        let model_tops = |model: &FabModel| Tops { undo: model.undo_top(), redo: model.redo_top() };
        let commit_object = |model: &mut FabModel, eval: &mut Evaluator, id, width| {
            let changes = [Change::Script { file: "block.cad.splash".into(), edit: Edit::Program { source: source(width) } }];
            let (_, built) = model.apply(eval, id, model.head(), &changes, None).expect("the object edit commits");
            assert!(built.is_some(), "a fresh object commit evaluates its changed source");
        };

        let design = fab_elec_fixtures::ledctl();
        let unit = BoardId(0);
        let via = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board").vias[0].clone();
        let board_hash = |design: &Design| design.unit(unit).expect("unit 0 remains present").board.as_ref().expect("unit 0 keeps its board").content_hash();
        let mut engine = Engine::new(design, Arc::new(Factories::new()), steps.clone(), logged.clone(), "fixture".into());
        let mailbox = Mailbox::default();
        let board_move = || Cmd::Commit {
            by: By::Person, ticket: None, place: vec![],
            ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item: via.id, by: P::mm(0.3, -0.2) } }],
            label: "move via".into(), circuits: vec![], expect: None,
        };
        let initial_model = model.state().clone();
        let initial_board = board_hash(engine.design());

        commit_object(&mut model, &mut eval, "first object edit", 2);
        let first_model = model.state().clone();
        mailbox.push(board_move());
        let mut last = engine.step(mailbox.take(), &mut |_| {});
        assert!(last.rejected.is_empty(), "the board move commits between the object edits");
        assert_eq!(last.undo, Some(2), "the board move uses the counter after the first object edit");
        let moved_board = board_hash(engine.design());
        assert_ne!(moved_board, initial_board, "the committed via move changes board state");
        commit_object(&mut model, &mut eval, "second object edit", 3);
        let final_model = model.state().clone();
        assert_ne!(first_model, initial_model, "the first object edit changes model state");
        assert_ne!(final_model, first_model, "the second object edit changes model state");
        assert_eq!(model.undo_top(), Some(3), "the second object edit follows the board move in the shared counter");

        for undo in [true, false] {
            let expected_boards = if undo { [moved_board, initial_board, initial_board] } else { [initial_board, moved_board, moved_board] };
            for (index, expected) in [Target::Model, Target::Elec, Target::Model].into_iter().enumerate() {
                let target = route(undo, model_tops(&model), Tops { undo: last.undo, redo: last.redo });
                assert_eq!(target, expected, "three undos and earliest-undone redos route model, engine, model: undo={undo}, index={index}");
                match target {
                    Target::Model => {
                        let action = if undo { HistoryAction::Undo } else { HistoryAction::Redo };
                        model.history(&mut eval, model.head(), action, None).expect("the routed model history step applies");
                    }
                    Target::Elec => {
                        mailbox.push(if undo { Cmd::Undo } else { Cmd::Redo });
                        last = engine.step(mailbox.take(), &mut |_| {});
                        assert!(last.rejected.is_empty(), "the routed electronics history step applies");
                    }
                }
                let expected_model = if index < 2 { &first_model } else if undo { &initial_model } else { &final_model };
                assert_eq!(model.state(), expected_model, "each routed press restores its model state: undo={undo}, index={index}");
                assert_eq!(board_hash(engine.design()), expected_boards[index], "each routed press restores its board state: undo={undo}, index={index}");
            }
        }
        assert_eq!(steps.load(SeqCst), 3, "undo and redo keep the original shared step numbers");

        model.history(&mut eval, model.head(), HistoryAction::Undo, None).expect("the latest object edit undoes before a board commit");
        assert_eq!(model.redo_top(), Some(3), "model redo is available before the board commit");
        mailbox.push(board_move());
        last = engine.step(mailbox.take(), &mut |_| {});
        assert!(last.rejected.is_empty(), "the new board move commits after the model undo");
        assert_eq!(last.undo, Some(4), "the new board commit advances the shared counter");
        assert_eq!(model.redo_top(), None, "a board commit immediately invalidates model redo");
        // Both redos are empty now, which routes to the model's no-history answer. Undo the
        // board commit to offer a real redo and prove the invalidated model step cannot win.
        mailbox.push(Cmd::Undo);
        last = engine.step(mailbox.take(), &mut |_| {});
        assert!(last.rejected.is_empty(), "the new board commit undoes to offer electronics redo");
        assert_eq!(last.redo, Some(4), "electronics offers its newly undone step");
        assert_eq!(model.redo_top(), None, "undoing the board does not revive invalidated model redo");
        assert_eq!(route(false, model_tops(&model), Tops { undo: last.undo, redo: last.redo }), Target::Elec, "redo skips the invalidated model history");
        assert_eq!(model.state(), &first_model, "invalidating redo leaves the undone model state intact");
        assert_eq!(board_hash(engine.design()), moved_board, "undoing the new board move restores the earlier board state");
    }
}

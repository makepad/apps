//! The AI's turns and the steps each committed in both histories (the model's and the
//! electronics', numbered from one counter), undone or redone a turn at a time: Edit > Undo the
//! AI's Last Turn and the buttons under each answer in the chat. A turn is undone as that many
//! presses of Undo while its steps are the newest steps, and redone as that many presses of Redo
//! while they are the next to redo. A step of anyone else's after them, or between them, ends the
//! offer, as those presses would take it too: a turn under a later turn's steps waits until that
//! turn is undone, so no turn is undone out of order. Cost: O(the turns) per event to follow the
//! histories' newest step; O(the turns' steps) when the offers are read (when a top moved).

use crate::undo_order::Tops;

/// One turn the AI worked in and the steps it committed.
struct Turn {
    /// The person's turn (the chat's line it answers, from 1).
    turn: u64,
    /// Its chat calls, by the id each answers under.
    calls: Vec<u64>,
    /// The steps they committed, by number.
    steps: Vec<u64>,
    /// Its steps are in the histories (false once the newest step went below its first: undone,
    /// and gone for good when a step newer than its last came after that without a redo).
    applied: bool,
}

/// What a turn's steps allow now ([`TurnUndo::offer`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Offer {
    /// That many presses of Undo take them back.
    Undo(usize),
    /// That many presses of Redo put them back.
    Redo(usize),
    /// Newer steps are on them: a later turn's (its number), or someone else's (None).
    Under(Option<u64>),
    /// They are undone and earlier undone steps are redone first.
    After,
    /// Some of them are undone and some not, or another's step lies between them.
    Partly,
}

#[derive(Default)]
pub struct TurnUndo {
    /// The turns that made calls, oldest first.
    turns: Vec<Turn>,
    /// Newest steps seen since the oldest turn's first step that no turn claimed: someone else's.
    others: Vec<u64>,
    /// One more each time a turn's calls, steps or state change: the offers are read again.
    pub rev: u64,
}

impl TurnUndo {
    /// Call `call` belongs to turn `turn`. Turn 0 (a call from outside the conversation) is not
    /// kept.
    pub fn call(&mut self, call: u64, turn: u64) {
        if turn == 0 {
            return;
        }
        match self.turns.iter_mut().find(|t| t.turn == turn) {
            Some(t) => t.calls.push(call),
            None => self.turns.push(Turn { turn, calls: vec![call], steps: Vec::new(), applied: true }),
        }
        self.rev += 1;
    }

    /// The turn of chat call `call`.
    pub fn turn_of(&self, call: u64) -> Option<u64> {
        self.turns.iter().rev().find(|t| t.calls.contains(&call)).map(|t| t.turn)
    }

    /// Turn `turn` committed step `seq`.
    pub fn committed(&mut self, turn: u64, seq: u64) {
        let Some(t) = self.turns.iter_mut().find(|t| t.turn == turn) else { return };
        if !t.steps.contains(&seq) {
            t.steps.push(seq);
        }
        t.applied = true;
        self.others.retain(|other| *other != seq);
        self.rev += 1;
    }

    /// The newest step of the two histories now: one no turn has recorded may be another's, and
    /// each turn's steps are in or out.
    pub fn follow(&mut self, newest: Option<u64>) {
        let Some(top) = newest else {
            for t in self.turns.iter_mut().filter(|t| t.applied && !t.steps.is_empty()) {
                t.applied = false;
                self.rev += 1;
            }
            return;
        };
        let oldest = self.turns.iter().filter_map(|t| t.steps.iter().min()).min().copied();
        if oldest.is_some_and(|first| top > first) && !self.others.contains(&top) && !self.turns.iter().any(|t| t.steps.contains(&top)) {
            self.others.push(top);
        }
        for t in &mut self.turns {
            let Some(first) = t.steps.iter().min().copied() else { continue };
            let applied = if top < first {
                false
            } else if t.steps.contains(&top) {
                true
            } else {
                t.applied
            };
            if applied != t.applied {
                t.applied = applied;
                self.rev += 1;
            }
        }
    }

    /// The turns kept, oldest first.
    pub fn turns(&self) -> Vec<u64> {
        self.turns.iter().map(|t| t.turn).collect()
    }

    /// The last turn the AI worked in.
    pub fn last(&self) -> Option<u64> {
        self.turns.last().map(|t| t.turn)
    }

    /// Forget the turns `keep` says no; the steps seen before the oldest left go too.
    pub fn retain(&mut self, keep: impl Fn(u64) -> bool) {
        let before = self.turns.len();
        self.turns.retain(|t| keep(t.turn));
        if self.turns.len() != before {
            let oldest = self.turns.iter().filter_map(|t| t.steps.iter().min()).min().copied();
            self.others.retain(|other| oldest.is_some_and(|first| *other > first));
            self.rev += 1;
        }
    }

    /// What turn `turn`'s steps allow with the histories' tops (`model`, `elec`: each one's
    /// newest step and next step to redo). None: it committed nothing, or its steps are gone
    /// (undone, then a newer step came instead of a redo).
    pub fn offer(&self, turn: u64, model: Tops, elec: Tops) -> Option<Offer> {
        let t = self.turns.iter().find(|t| t.turn == turn)?;
        let first = *t.steps.iter().min()?;
        let last = *t.steps.iter().max()?;
        let between = |s: u64| s > first && s < last && !t.steps.contains(&s);
        let interleaved = self.others.iter().any(|other| between(*other)) || self.turns.iter().any(|u| u.turn != turn && u.steps.iter().any(|s| between(*s)));
        let top = model.undo.max(elec.undo);
        if !t.applied {
            // a step newer than its first came since its undo: its redo went with it
            if top.is_some_and(|top| top >= first) {
                return None;
            }
            let next = [model.redo, elec.redo].into_iter().flatten().min()?;
            return Some(if next == first && !interleaved {
                Offer::Redo(t.steps.len())
            } else if next < first {
                Offer::After
            } else {
                Offer::Partly
            });
        }
        let top = top?;
        Some(if top > last {
            Offer::Under(self.turns.iter().find(|u| u.steps.contains(&top)).map(|u| u.turn))
        } else if top == last && !interleaved && [model.undo, elec.undo].into_iter().flatten().all(|newest| newest < first || t.steps.contains(&newest)) {
            Offer::Undo(t.steps.len())
        } else {
            Offer::Partly
        })
    }
}

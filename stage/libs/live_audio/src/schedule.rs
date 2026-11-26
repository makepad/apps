//! Stage's audio owner's schedule: the engine's (`makepad-audio-engine`)
//! with the deck as the owner's own node type.
//!
//! Continuity across an edit is state transfer, stated exactly: a node with
//! the same qualified id and type as in the base generation, at the same
//! device rate, keeps its DSP state; a deck keeps it across a rate change
//! too (its position is in source frames).

use crate::deck::DeckState;
use makepad_flow::live::plan::PlanError;
use makepad_audio_engine::schedule::{continuity_of, Ext, ExtCx};
use makepad_audio_engine::schedule as engine;
pub use makepad_audio_engine::schedule::{
    migrate_state, trigger_id, AudioBudget, BudgetPermit, ClockChunk, ControlBank, Layout, LayoutNode, Route, SPAN,
};
use crate::board::{board, BoardReader, BoardWriter};
use makepad_flow::live::plan::OwnerSlice;
use makepad_flow::live::owner::PrepareControl;
use std::sync::Arc;

/// A deck node's ops: its span, controls and probes.
pub struct DeckOp {
    pub state: u16,
    pub out: u16,
    pub play: u16,
    pub speed: u16,
    pub loop_in: u16,
    pub loop_out: u16,
    pub loop_enabled: u16,
    pub scratch: u16,
    pub scratch_at: u16,
    pub brake: u16,
    pub fling: u16,
    pub key: u16,
    pub keylock: u16,
    pub cue_at: u16,
    pub probes: (u16, u16, u16),
}

pub type NodeState = engine::NodeState<Box<DeckState>>;
pub type Op = engine::Op<DeckOp>;
pub type Schedule = engine::Schedule<Box<DeckState>, DeckOp, Option<BoardWriter>>;
pub type Compiled = engine::Compiled<Box<DeckState>, DeckOp, Option<BoardWriter>>;

/// The deck as the engine's extension.
pub struct Decks;

impl Ext for Decks {
    type State = Box<DeckState>;
    type Op = DeckOp;

    fn demand(type_name: &str) -> Option<(usize, usize, usize)> {
        (type_name == "AudioDeck").then_some((12, 3, 5))
    }

    fn compile(cx: &mut ExtCx) -> Result<Option<(Box<DeckState>, DeckOp)>, PlanError> {
        let node = cx.node;
        let (state, rate, generation, base) = (cx.state, cx.rate, cx.generation, cx.base);
        match node.type_name.as_str() {
            "AudioDeck" => {
                cx.layout.decks.insert(node.id.clone(), cx.observed.len() as u16);
                cx.observed.push(state);
                let out = cx.spans.len() as u16;
                for (lane, port) in ["audio", "vocals", "drums", "bass", "other"].into_iter().enumerate() {
                    cx.spans.insert((node.id.clone(), port.into()), out + lane as u16);
                }
                let play = cx.control("play")?;
                let speed = cx.control("speed")?;
                let loop_in = cx.control("loop_in")?;
                let loop_out = cx.control("loop_out")?;
                let loop_enabled = cx.control("loop_enabled")?;
                let cue_at = cx.control("cue_at")?;
                let scratch = cx.control("scratch")?;
                let scratch_at = cx.control("scratch_at")?;
                let brake = cx.control("brake")?;
                let fling = cx.control("fling")?;
                let key = cx.control("key")?;
                let keylock = cx.control("keylock")?;
                *cx.sounding = true;
                // A deck's continuity survives a rate change: its state does.
                let continuity = continuity_of(base, base.map_or(rate, |base| base.rate), node, generation);
                cx.layout.triggers.insert(node.id.clone(), (trigger_id(&node.id), continuity));
                let probes = (cx.probe("position"), cx.probe("end"), cx.probe("stems_available"));
                Ok(Some((Box::new(DeckState::fresh()), DeckOp { state, out, play, speed, loop_in, loop_out, loop_enabled, scratch, scratch_at, brake, fling, key, keylock, cue_at, probes })))
            }
            _ => Ok(None),
        }
    }

    fn keeps_across_rates(type_name: &str) -> bool {
        type_name == "AudioDeck"
    }

    fn control_range(kind: &str, name: &str) -> Option<(f64, f64, bool)> {
        Some(match (kind, name) {
            ("AudioDeck", "play" | "loop_enabled" | "scratch" | "brake" | "keylock") => (0.0, 1.0, true),
            ("AudioDeck", "key") => (-crate::deck::KEY_SHIFT_MAX, crate::deck::KEY_SHIFT_MAX, false),
            ("AudioDeck", "speed") => (-crate::deck::SPEED_MAX, crate::deck::SPEED_MAX, false),
            ("AudioDeck", "fling") => (-crate::deck::SCRATCH_RATE_MAX, crate::deck::SCRATCH_RATE_MAX, false),
            ("AudioDeck", "loop_in" | "loop_out" | "cue_at" | "scratch_at") => (0.0, crate::deck::MAX_SECONDS, false),
            _ => return None,
        })
    }
}

/// The engine's controls and the deck's: the contract the owner checks.
pub fn control_range(kind: &str, name: &str) -> Option<(f64, f64, bool)> {
    engine::control_range(kind, name).or_else(|| <Decks as Ext>::control_range(kind, name))
}

/// The schedule, and the board the host reads what it publishes from.
pub fn compile(slice: &OwnerSlice, generation: u64, incarnation: u64, format: u64, base: Option<&Layout>, budget: &Arc<AudioBudget>) -> Result<(Compiled, BoardReader), PlanError> {
    compile_controlled(slice, generation, incarnation, format, base, budget, None)
}

pub(crate) fn compile_controlled(slice: &OwnerSlice, generation: u64, incarnation: u64, format: u64, base: Option<&Layout>, budget: &Arc<AudioBudget>, control: Option<&PrepareControl>) -> Result<(Compiled, BoardReader), PlanError> {
    let mut compiled = engine::compile_controlled::<Decks, Option<BoardWriter>>(slice, generation, incarnation, format, base, budget, control)?;
    let schedule = &mut compiled.schedule;
    let (writer, reader) = board(schedule.clocks.len(), schedule.probes.len(), schedule.decks.len());
    schedule.board = Some(writer);
    Ok((compiled, reader))
}

/// Deck states of a schedule with a swap still waiting: each holds one
/// reserved return slot the engine releases when the schedule itself is
/// returned instead.
pub(crate) fn pending_returns(schedule: &Schedule) -> usize {
    schedule.states.iter().filter(|state| matches!(state, NodeState::Ext(deck) if deck.next.is_some())).count()
}

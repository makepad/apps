//! Stage's DECK model: a deck is an ordered list of slides, each
//! a looping cut with its own transitions, live apps and demo steps.
//! [`DeckDoc::load`] evaluates a `*.deck.splash` file and checks it,
//! collecting every problem with its source location. No GPU, no UI, no
//! clocks: the player lives in the app.

mod deck;
mod doc;
mod eval;
pub mod ops;
pub mod playback;
mod scan;
pub mod sketch;

pub use deck::{Deck, Incoming, MAX_CHANGE_SECONDS, MAX_JUMP_SECONDS, OUTSIDE, RUSH_SECONDS, stops};
pub use doc::*;

/// The `mod.deck` constructors: `Deck`, `Slide`, `App`, `transition`
/// and the demo steps. They return records and run nothing.
pub const PRELUDE: &str = include_str!("prelude.splash");

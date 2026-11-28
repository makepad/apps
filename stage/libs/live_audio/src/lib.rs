//! The live audio owner (`stage.audio`).
//!
//! - [`nodes`] registers the connection types and node descriptors, so the
//!   catalog, the inspector and tools see audio nodes like any other.
//! - [`schedule`] prepares an owner slice into a [`schedule::Schedule`]: ops
//!   in order, every buffer and DSP state allocated, off the audio thread.
//! - [`engine`] is the realtime half: one [`engine::AudioEngine`] renders
//!   every live instance into the device buffer. It allocates nothing, takes
//!   no lock, frees nothing and renders every frame it is asked for.
//! - [`deck`] is the deck node's clip, budget lease, command and render.
//! - [`runtime`] is the host half: it stages and commits schedules, carries
//!   control values across generations, writes endpoints, reads telemetry
//!   and drops what the engine retires.
//!
//! What this crate does not claim: the platform's device callback takes a
//! lock and may size its buffer before it calls [`engine::AudioEngine::render`].
//! That is outside this crate and is not cured here.

pub mod board;
pub mod bundle;
pub mod deck;
pub mod embedded;
pub mod feed;
pub mod engine;
pub mod nodes;
pub mod paged;
pub mod runtime;
pub mod schedule;

pub use bundle::{stem_bundle_bytes, PreparedStemBundle, StemModel, STEM_SLOTS};
pub use deck::{ClipTelemetry, DeckCommand, DeckSource, DeckTrack, TrackLease};
pub use paged::{
    AudioNote, AudioNoteKind, AudioNotes, DurableGap, LoopRefusal, LoopRejection, LoopRequest, PageOp, PageOpKind, PageOutcome, PageRefusal, PagedCommand, PagedCounts, PagedFrontier, PagedLoop,
    PagedReadiness, PagedSource, PagedSourceKey, PagedStart, PagedTerminal, PrepareRefusal, StemPolicy, stem_cushion_limit, MAX_STEM_CUSHION_SECS, MAX_PAGED_NOTES, PAGED_GROUP_PAGES, PAGE_OPS_PER_BUFFER, PAGE_ROUTES,
};
pub use engine::{AudioEngine, ClockTelemetry, DeckObservation, DeckStatus, RenderChunk, SlotTelemetry, Telemetry};
pub use nodes::{register, OWNER};
pub use runtime::{audio_owner, AudioDevice, AudioOwner, AudioDeckObservation, AudioRuntime, AudioTransportClock, DeckExchange, DeckExchangeCancel, PagedReceipt, EXCHANGED_CONTROLS};

pub use makepad_audio_engine::{clock, rest};
pub use makepad_audio_engine::CHUNK;
pub use makepad_audio_engine::feed as engine_feed;

#[cfg(test)]
mod golden;

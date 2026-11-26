//! What one schedule publishes from the device callback: its probes, its
//! Transports' clocks and its decks' observations, with the callback that
//! rendered them. Sized when the schedule is prepared (off the audio
//! thread), so a graph has as many of each as it needs and the callback
//! only overwrites them in place ([`Writer::update`]).

use crate::engine::{ClockTelemetry, DeckObservation};
use makepad_lock_free::{triple_buffer, Reader, Writer};

#[derive(Clone)]
pub struct BoardValues {
    /// The callback (`Telemetry::callbacks`) that rendered these; 0 before
    /// the schedule first rendered.
    pub publication: u64,
    /// Output frames counted at the end of that callback, and its rate.
    pub frame: u64,
    pub rate: u32,
    /// When that callback finished (the engine's clock), where the host
    /// installed a sampler.
    pub at: Option<f64>,
    /// By the schedule's clock, probe and deck observation indices.
    pub clocks: Box<[ClockTelemetry]>,
    pub probes: Box<[f32]>,
    pub decks: Box<[DeckObservation]>,
}

pub type BoardWriter = Writer<BoardValues>;
pub type BoardReader = Reader<BoardValues>;

/// A board for `clocks` clocks, `probes` probes and `decks` decks.
pub fn board(clocks: usize, probes: usize, decks: usize) -> (BoardWriter, BoardReader) {
    triple_buffer(BoardValues {
        publication: 0,
        frame: 0,
        rate: 0,
        at: None,
        clocks: vec![ClockTelemetry::default(); clocks].into_boxed_slice(),
        probes: vec![0.0; probes].into_boxed_slice(),
        decks: vec![DeckObservation::EMPTY; decks].into_boxed_slice(),
    })
}

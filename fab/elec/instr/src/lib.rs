//! Blocking source drivers. No UI, transport, or thread runtime dependencies.
pub mod prbs;
pub mod synth;
mod signal;
mod drivers;
pub use signal::Signal;
pub use drivers::{Limits, Record, Replay, Synth, SynthChannel, SynthConfig, MEMBER_CYCLE};

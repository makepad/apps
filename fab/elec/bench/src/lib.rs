//! Fab electronics benches (E3 sections 1.8 to 1.9 and 3, contract C11).
//!
//! - `audio`: a board's output jacks, found by their parts' data.
//! - `bench`, `fixture`: stimulus added to a design, including stage-0 MCU stubs.
//! - `expect`: E-5 expectations judged at every accepted step (live: indications).
//! - `bind`, `input`, `models`: classify a design unit using explicit model-library data.
//! - `lower`: lower the classified input to kernel devices and their live-value plan.
//! - `net`: a firmware part's connections: the fake server the twin plays, and the real one.
//! - `measure`: the scope's measurements on sampled waveforms (one implementation for the
//!   instruments and the expectations).
//! - `run`: the counted bench run: a transient over the committed lowering with its
//!   expectations judged at every accepted point.
//! - `trigger`: edge triggers and the acquisition window, streamed sample by sample.
//! - `values`: the design's `NetRef` / `PinRef` / `CompRef` onto a live simulation's
//!   `LiveValues`, and the bench's `SimCmd` onto the kernel's `LiveCmd`.

/// Pure edge alignment, clock fitting and located differences.
pub mod align;
pub mod audio;
pub mod bench;
pub mod bind;
pub mod bus;
pub mod decode;
pub mod edges;
pub mod expect;
pub mod fixture;
pub mod input;
pub mod judge;
pub mod limited;
pub mod logic;
pub mod lower;
pub mod measure;
/// Immutable indices built by the same machines as expectations.
pub mod measure_index;
pub mod models;
pub mod net;
pub mod run;
pub mod source;
pub mod tolerance;
mod timing;
pub mod trigger;
pub mod uart;
pub mod values;

pub use bench::{BenchSpec, Firmware};
pub use input::LowerInput;
pub use lower::{lower, lower_circuit, lower_input, FwBinding, LedTap, LogicMode, LowerDiag, Lowered, PinFacts};
pub use models::{Bind, LedCard, ModelLibs};
pub use run::{bench_run, bench_spec, BenchReport};
pub use values::{BoardInput, SimCmd, SimMode, Touches, ValueIndex, EMPTY_GENERATION};

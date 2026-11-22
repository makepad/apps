//! Fixed firmware runtime assembled with the instruction oracle's assembler.
//!
//! The image builder owns memory, vectors, the clock, dispatch and watch output.
//! Verified modules supply entries through the E1 integer/bool emitter.

mod bound;
pub mod chip;
pub mod emit;
pub mod firmware;
pub mod helpers;
pub mod image;
pub mod range;
pub mod runtime;

pub use image::{build, Image, Layout, PinDef, Program, Role, SerialDef, Stubs, Symbols, TimerDef};
pub use emit::{FUNCTIONS_UNSUPPORTED, build_module, Built, EntryReport, OpSpan, Options, Refusal, SerialReport, SourceMap, Where};
pub use firmware::{firmware, firmware_with_watch, Diagnostic, Firmware};
pub use runtime::Bounds;

//! Fab's G-code.
//!
//! - `time`: the trapezoid planner with the corner rules (R1 §10.3).
//! - `process`: the line interpreter that feeds it and produces the
//!   preview records, per-line times and statistics.
//! - `record`: the per-move preview records (R1 §10.7).
//! - `packed`: records as a stored layer timeline keeps them, a third of their size.
//! - `firmware`: what the printer's firmware takes for what standard G-code leaves out.

pub mod arcfit;
pub mod cal_bands;
pub mod firmware;
pub mod fmt;
pub mod heads;
pub mod layers;
pub mod packed;
pub mod preheat;
pub mod process;
pub mod record;
pub mod sequential;
pub mod time;
pub mod tower;
pub mod vars;
pub mod writer;

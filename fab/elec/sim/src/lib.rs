//! Fab electronics simulation kernel (E3 section 1).
//!
//! A circuit is `SimCircuit` (plain data). The solver formulates it as modified nodal analysis in
//! charge form, `f(x) + d/dt q(x) = 0`: devices return currents, charges and their derivatives,
//! and the integrator turns `dq/dt` into `a0 * q + history` in one place.
//!
//! - `sparse`: the dense LU below 8 unknowns, the sparse LU (ordering, factor, refactor) above.
//! - `newton`: assembly, Newton with limiting, located diagnostics.
//! - `tiers`: the three-tier load: folded linear stamps, iterating devices on top.
//! - `integ`: trapezoidal, Gear 1-6, backward Euler, truncation error, the predictor.
//! - `analysis`: op (with its four aids), dc, ac, tran, and `run`.
//! - `dev`: the device banks.
//! - `live`: live mode's seam: `LiveValues`, `ValuePlan`, `LiveCmd`, the `Latest` publication.
//! - `livesim`: live mode's worker: `LiveSim` and `run_live`, the loop a host runs on its thread.
//! - `trace`: the recorded store and its deltas (C10).
//! - `expr`: the expression dialect: constant values, and tapes with exact partials for behavioural
//!   sources.
//! - `spice`: classic SPICE text decks, parameters, subcircuits and located diagnostics.
//! - `wave`: the wave mode's kernels: a block as a wave digital filter on a fixed sample grid.

pub mod alloc;
pub mod analysis;
pub mod blocks;
pub mod budget;
pub mod circuit;
pub mod cosim;
pub mod dev;
pub mod device_audio;
pub mod expr;
pub mod fft;
pub mod heat;
pub mod input;
pub mod integ;
pub mod listen;
pub mod recording;
pub mod rounds;
pub mod live;
pub mod livesim;
pub mod newton;
pub mod num;
pub mod sparse;
pub mod spill;
pub mod spice;
pub mod stamp;
pub mod tiers;
pub mod trace;
pub mod wave;

pub use analysis::{operating_point_aids, run, AcSweep, Analysis, Control, DcSweep, GminLadder, HoldKind, OpAid, OpSeed, OpStage, OpTry, Record, Sink, SinkState, Summary, TranSpec, OP_AIDS};
pub use circuit::{BKind, Cell, Change, CtlKind, Gpio, GpioParams, Method, ModelKind, Options, PinEdge, PinMode, PinPull, Retuned, SimCircuit, Source, Wave};
pub use newton::{DiagKind, Profile, Reject, Rejection, Rescue, SimDiag, Solver};
pub use num::C64;

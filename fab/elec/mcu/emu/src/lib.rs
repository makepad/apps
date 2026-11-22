//! The emulator core microsplash's virtual MCUs share (the AVR, the ESP32-C3's RV32IMC and the
//! ESP32-S3's Xtensa LX7 models): a memory bus, exact rational clocks, tick/time conversion,
//! marked code addresses, and the common machine interface. The co-simulation adapter comes later.

pub mod bus;
pub mod clock;
pub mod time;
pub mod pcset;
pub mod stepmap;
pub mod fw;
pub mod machine;
pub mod counts;
pub mod board;
pub mod lockstep;
#[cfg(feature = "jit")] pub mod jit;

pub use clock::Clock;
pub use time::{time, tick_at, TIME_EXACT_LIMIT};
pub use pcset::PcSet;
pub use stepmap::{Owner, StepMap};
pub use fw::{FwCond, FwDebug, FwFaultCell, FwFunc, FwHandler, FwScalar, FwStmt, FwTy, FwValue, FwVar, PinUse};
pub use machine::*;
pub use lockstep::Lockstep;
pub use counts::{Block, CodeMap, Cond, CondKind, ExecCounts, Tally};

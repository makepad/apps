//! Byte producers for the windowed radio island. Runtime installation is separate.

/// The pack registers exactly one island ISR line, line 0: decoded
/// ic_set_interrupt_handler calls _set_isr(n = 0), matching IDF ETS_WMAC_INUM 0.
/// esp-radio esp-radio/src/wifi/os_adapter/esp32s3.rs:47-72 shares one handler
/// between MAC and PWR. R4c-3 sizes Radio::isr_capacity from this count; with
/// that sizing, ISR_TABLE_FULL can only indicate a sizing bug.
pub const PACK_ISR_LINES: u32 = 1;

pub const PHY_FUNS_OBJECT: &str = "g_phyFuns";

/// The dispatcher drains the ring every time it sleeps, so it holds reports
/// raised between two sleeps. Overflow is never silent: it counts into the
/// ring's lost word, which the drain folds into runtime LOST accounting;
/// Fab shows LOST. Capacity is a power of two for free-running u32 indices.
pub const REPORT_RING_ENTRIES: u32 = 8;
const _: () = assert!(REPORT_RING_ENTRIES.is_power_of_two());

pub mod vectors;
pub use vectors::*;
pub mod task;
pub use task::*;
pub mod shim;
pub use shim::*;
pub mod thunk;
pub use thunk::*;
pub mod sched;
pub use sched::*;
pub mod boot;
pub use boot::*;
pub mod fault;
pub use fault::*;
pub mod ftm_words;
pub mod island;
pub use island::{island_glue, link_pack};
pub mod kernel;
pub mod phy_init_data;
pub mod regulatory;
pub mod phy_glue;
pub mod event_glue;
pub mod start_glue;
pub mod stand_in;
pub mod helpers;
pub mod adapter;

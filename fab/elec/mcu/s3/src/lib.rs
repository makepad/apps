//! ESP32-S3 CPU0 machine: Rust ROM handoff, aliased SRAM1, flash/MMU,
//! CPU clock selection, WAITI time jumps, journalled rewind, and the
//! peripheral blocks in periph/ with their events and resets. No cache
//! timing, ROM execution, PSRAM or CPU1: accesses outside the implemented
//! map stop with an Unmodeled halt and access details.
//!
//! Sources: ESP32-S3 TRM v1.8; ESP-IDF v6.1.1 (Copyright Espressif Systems
//! (Shanghai) CO LTD): register addresses and fields ported with attribution
//! from its soc headers (Apache-2.0 OR MIT) and hal mmu_ll.h (Apache-2.0);
//! Xtensa core-isa.h (its MIT-style notice, in config.rs). Citations below use
//! core-isa.h line numbers.

mod bus;
mod input;
pub mod config;
pub mod foreign;
pub mod machine;
pub mod rom;
mod periph;

pub use machine::{S3Machine, StopDetail};
pub use foreign::{Ctx, ForeignBus, ForeignCode, Next, VirtualFn, VirtualFns};
pub use periph::{PinsProfile, ResetRequest, ResetScope, TimeProfile};

/// Clock selection fields, not a measured ROM handoff guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuClock {
    /// XTAL / div, with div in 1..=1024 (PRE_DIV_CNT + 1).
    Xtal { div: u32 },
    /// CPUPERIOD_SEL 0/1/2 selects 80/160/240 MHz.
    Pll480 { period: u32 },
    /// CPUPERIOD_SEL 0/1 selects 80/160 MHz.
    Pll320 { period: u32 },
}

/// Explicit board and ROM handoff assumptions. M denotes a chip-probe fact
/// still to be measured, rather than a documented reset value.
#[derive(Clone, Debug)]
pub struct S3Profile {
    /// Board NOR size; 16 MiB on the voicebox.
    pub flash_bytes: usize,
    /// M: CPU clock at the ROM jump.
    pub handoff_cpu: CpuClock,
    /// M: linearly mapped flash pages, 64 (4 MiB) for Direct Boot.
    pub handoff_mapped_pages: u32,
    /// M: Direct Boot entry, 0x4200_0408 (emcu-s3.md:27–30).
    pub entry: u32,
    /// Reproducible undocumented handoff state.
    pub seed: u64,
    /// SYSTIMER, watchdog and reset assumptions (periph/time.rs).
    pub time: TimeProfile,
    /// GPIO, strap and USB-Serial-JTAG assumptions (periph/pins.rs).
    pub pins: PinsProfile,
    /// Factory base MAC in network byte order, exposed by the read-only
    /// eFuse shadow (TRM §5.3.3, Registers 5.18/5.19). This is a board input.
    pub mac: [u8; 6],
    /// The core-regulator calibration (PVT) fields of the chip's eFuse words RD_MAC_SPI_SYS_3, _4, _5 and
    /// RD_SYS_PART1_DATA4 (0x60007050, 0x54, 0x58, 0x6c; efuse_reg.h:775–1020), by word; the words' other bits
    /// are seeded junk. A board input, as `mac`: [`NO_PVT`] for a chip that carries none.
    pub pvt: [u32; 4],
}

/// The PVT words of a chip with eFuse block version 0.0, which carries no regulator calibration: IDF's
/// rtc_set_stored_dbias (esp_hw_support/port/esp32s3/rtc_init.c:386-409) keeps its defaults. For profiles
/// that model no particular chip.
pub const NO_PVT: [u32; 4] = [0; 4];

// SplitMix64: deterministic junk only, not a source of hardware values.
struct Junk(u64);

impl Junk {
    fn word(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn u32(&mut self) -> u32 { self.word() as u32 }
}

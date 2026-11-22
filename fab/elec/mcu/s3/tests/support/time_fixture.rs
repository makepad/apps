//! Time-block assumptions and firmware sequences. No measured chip claims.

use fab_elec_mcu_emit32::xtensa::enc;
use fab_elec_mcu_s3::TimeProfile;

use super::{Program, ENTRY};

// facts.md §3; facts-machine.md §§1,5; TRM p.408 Table 4.3-3.
pub const SYSTIMER: u32 = 0x6002_3000;
pub const RTC: u32 = 0x6000_8000;
pub const TIMG0: u32 = 0x6001_f000;
pub const TIMG1: u32 = 0x6002_0000;
pub const TICK_HZ: u64 = 480_000_000;
// TRM pp.677–678 §§13.2.2.3,13.3.2.2.
pub const WKEY: u32 = 0x50d8_3aa1;
pub const SWKEY: u32 = 0x8f1d_312a;

/// Every M field is explicit; individual properties can override one value.
pub fn time_profile() -> TimeProfile {
    TimeProfile {
        systimer_first_long: true,
        rc_slow_hz: (136_000, 1),
        wdt_delay_sel: 0,
        swd_timeout_ticks: TICK_HZ * 9 / 10,
        swd_request_lead_ticks: TICK_HZ / 10,
        wdt_sync_cycles: 2,
        mwdt_rephase_on_feed: false,
        mwdt_rephase_on_config: true,
        wdt_restart_on_enable: true,
    }
}

impl Program {
    /// Reads an MMIO word into `reg`, using a1 for its address.
    pub fn read_time(&mut self, addr: u32, reg: u8) -> u32 {
        self.load(1, addr);
        self.emit(enc::l32i(reg, 1, 0))
    }

    /// RMW with a1/a2/a3; preserves all unrelated documented fields.
    pub fn time_bits(&mut self, addr: u32, clear: u32, set: u32) -> u32 {
        self.read_time(addr, 2);
        self.load(3, !clear);
        self.emit(enc::and(2, 2, 3));
        self.load(3, set);
        self.emit(enc::or(2, 2, 3));
        self.emit(enc::s32i(2, 1, 0))
    }

    /// facts.md §3 disable recipe; TRM pp.613,615,677. Clear EN and
    /// FLASHBOOT_MOD_EN under the unlock key, then relock with non-key zero.
    pub fn disable_rwdt(&mut self) {
        self.store(RTC + 0xb0, WKEY);
        self.time_bits(RTC + 0x98, (1 << 31) | (1 << 12), 0);
        self.store(RTC + 0xb0, 0);
    }

    /// Same recipe for each MWDT: TRM pp.665,667,677; facts.md §3.
    pub fn disable_mwdt(&mut self, base: u32) {
        self.store(base + 0x64, WKEY);
        self.time_bits(base + 0x48, (1 << 31) | (1 << 14), 0);
        self.store(base + 0x64, 0);
    }

    /// TRM pp.616,678: unlock and enable automatic SWD feeding.
    pub fn auto_feed_swd(&mut self) {
        self.store(RTC + 0xb8, SWKEY);
        self.time_bits(RTC + 0xb4, 0, 1 << 31);
    }

    /// Only fixtures that sleep for long periods or change APB need this.
    pub fn disable_watchdogs(&mut self) {
        self.disable_rwdt();
        self.disable_mwdt(TIMG0);
        self.disable_mwdt(TIMG1);
        self.auto_feed_swd();
    }

    /// WAITI 15 masks CPU wakeups (Xtensa ISA p.644). Peripheral events
    /// still cut the sleep, so no instruction loop is needed for long waits.
    pub fn time_sleep(&mut self) -> u32 {
        self.sr(230, 0);
        self.emit(enc::rrr(0, 0, 0, 7, 15, 0))
    }
}

/// Start tick for straight-line fixtures at the default XTAL/2 CPU clock.
pub fn access_tick(pc: u32) -> u64 { u64::from((pc - ENTRY) / 3) * 24 }

/// Closed-form SYSTIMER oracle, TRM pp.634–635 (two edges per 60 ticks).
pub fn systimer_edges(tick: u64, first_long: bool) -> u64 {
    tick / 60 * 2 + u64::from(tick % 60 >= if first_long { 36 } else { 24 })
}

pub struct HandoffTimeJunk {
    pub rwdt_key: u32,
    pub rwdt_count: u64,
    pub swd_elapsed: u64,
    pub mwdt0_count: u64,
}

/// Independent replay of the emulator's seed contract, not a hardware fact.
/// Prefix: machine.rs::handoff_cpu (49 words), bus.rs::Registers::handoff
/// (2 clock + 512 MMU + 4 gate words), 416 KiB SRAM in eight-byte chunks,
/// then Time's two 52-bit junk counters. Checking WKEY also checks alignment.
pub fn handoff_time_junk(seed: u64, p: &TimeProfile) -> HandoffTimeJunk {
    let mut state = seed;
    let mut word = || {
        // lib.rs::Junk: SplitMix64, each u32 consumes a whole output word.
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) as u32
    };
    for _ in 0..49 + 2 + 512 + 4 + 416 * 1024 / 8 + 4 { word(); }
    let rwdt_key = word();
    // facts-machine.md §5: defaults HOLD0=200000 and 26000000; RWDT scaling.
    let rwdt_count = u64::from(word()) % (200_000u64 << (p.wdt_delay_sel + 1));
    word(); // SWD WKEY
    let swd_elapsed = u64::from(word()) % p.swd_timeout_ticks;
    word(); // MWDT0 WKEY
    let mwdt0_count = u64::from(word()) % 26_000_000;
    HandoffTimeJunk { rwdt_key, rwdt_count, swd_elapsed, mwdt0_count }
}

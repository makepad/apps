// map_flash_pages ports ESP-IDF v6.1.1 components/hal/esp32s3/include/hal/mmu_ll.h (Apache-2.0);
// the startup sequences are adapted from esp-hal (MIT OR Apache-2.0, taken under MIT). See
// fab/CREDITS.md.
// SPDX-FileCopyrightText: 2022-2023 Espressif Systems (Shanghai) CO LTD
// SPDX-FileCopyrightText: 2021 esp-rs
// SPDX-License-Identifier: MIT AND Apache-2.0
//! ESP32-S3 boot stage v1: confirmed records only, with RWDT recovery.
//!
//! Policy and record format: electronics/emcu/s3/boot-stage.md (2026-11-16).
//! `facts.md` and `facts-machine.md` citations below refer to that same directory.
//! IDF paths are relative to ESP-IDF v6.1.1 components/soc/esp32s3/ unless
//! otherwise stated; those headers and the MMU LL port are Apache-2.0.
//! Startup sequences are adapted from esp-hal commit
//! 4059aa584b579ac2e78903c627b6daa9d5d2d90c, esp-bootloader-direct-boot
//! (MIT OR Apache-2.0), with the lead's PS-first, CALL0-only policy. The runtime's
//! CPU clock switch follows esp-hal's esp-hal/src/soc/esp32s3/clocks.rs at that commit.

use fab_elec_mcu_emit32::xtensa::{assemble, cc, ccz, reg::*, Asm};

pub mod chip;
mod debug;
pub mod emit;
pub mod firmware;
pub mod image;
mod insns;
mod panel;
mod i2c;
mod motion;
mod touch;
mod wiring;
pub mod radio;
pub mod runtime;
pub mod wdt;
mod float;
pub use debug::debug_table;
pub use firmware::{firmware, flash_regions, island_regions, Firmware, SimRadio};
pub use emit::{build_handlers, Code, EntryKind, EntryReport, OpSpan, Options, PortLabel, Refusal, SourceMap, Where};
pub use radio::kernel::{build_kernel, KernelCode, KernelOptions, KernelRegion};
pub use image::{build_image, build_slot, Board, BootTask, BufferPlace, BufferRegion, EthSram, Handler, Image, MicSram, Radio, RadioSram, RadioStack, RuntimeRoutine, Slot, SlotMap, SlotOptions, SlotWhere, SpeakerSram, SramLayout};
pub use runtime::{MAX_OP_INSNS, RADIO_KERNEL_LINES};
pub use panel::{Panel, PanelSram, VOICEBOX_PANEL};
pub use i2c::{I2cBus, VOICEBOX_I2C};
pub use motion::{Motion, MotionSram, MOTION_PROFILE, VOICEBOX_MOTION};
pub use touch::{Touch, TouchSram, VOICEBOX_TOUCH};
pub use wdt::{feed_quarter_per_hold, wdt_hold, BoardTime};
pub use wiring::{wiring, AmpEnable, Audio, Wiring};

// Board policy: boot-stage.md, Flash layout. MMU facts: facts-machine.md:285–300;
// IDF include/soc/ext_mem_defs.h:46–73; hal/esp32s3/include/hal/mmu_ll.h:66–84.
const PAGE: u32 = 0x1_0000;
const SECTOR: u32 = 0x1000;
const ROM_PAGES: u32 = 64;
const MMU_ENTRIES: u32 = 512;
/// The voicebox reserves the top of the shared MMU table for its island.
pub const ISLAND_MMU_PAGE: u32 = MMU_ENTRIES - VOICEBOX.island_pages;
const PHYSICAL_PAGES: u32 = 16384;
const IROM: u32 = 0x4200_0000;
/// Both slots execute at shared MMU entry 64 (boot-stage.md, Flash layout).
pub const SLOT_BASE: u32 = 0x4240_0000;
/// Data alias of the same slot mapping (ext_mem_defs.h:72–73,105–124).
pub const SLOT_DATA_BASE: u32 = 0x3c40_0000;
/// The data alias of MMU entry 0 (ext_mem_defs.h:72–73): entry n reads at this plus n pages.
const DROM: u32 = 0x3c00_0000;

pub struct Layout {
    pub flash_bytes: u32,
    pub records: [u32; 2],
    pub slots: [u32; 2],
    pub slot_pages: u32,
    /// Each slot's file bundle (`fab_elec_mcu_rt::bundle`), read in place through the MMU entries
    /// after the slot's: slot A with bundle A, slot B with bundle B, so an update switches both.
    pub bundles: [u32; 2],
    pub bundle_pages: u32,
    /// The radio island's flash (its text, rodata, RAM images and header), page aligned, held
    /// apart from the slots so an app update never moves it (fable-answers.md, Q5 section 5).
    pub island: u32,
    pub island_pages: u32,
    /// One sector each, written with the record scheme: two for the radio calibration store and
    /// two for the settings (network name, key, bridge address).
    pub calibration: [u32; 2],
    pub settings: [u32; 2],
}

/// Two 32-page slots (2 MB of code each) and their two 48-page bundles (3 MB of files each); the
/// initial page contains the stage and records. 4.75 MB stay free past bundle B for either to
/// grow. Then the island's 16 pages (1 MB; ERADIO confirmed its place, 11-19), then the four store
/// sectors; the rest of the last page is free (firmware-files.md, Flash layout).
pub const VOICEBOX: Layout = Layout {
    flash_bytes: 16 * 1024 * 1024,
    records: [0x1000, 0x2000],
    slots: [0x1_0000, 0x21_0000],
    slot_pages: 32,
    bundles: [0x41_0000, 0x71_0000],
    bundle_pages: 48,
    island: 0xed_0000,
    island_pages: 16,
    calibration: [0xfd_0000, 0xfd_1000],
    settings: [0xfd_2000, 0xfd_3000],
};

impl Layout {
    fn island_mmu_page(&self) -> Result<u32, &'static str> {
        MMU_ENTRIES.checked_sub(self.island_pages).ok_or("island pages exceed the 512-entry MMU table")
    }

    /// The first MMU entry of the running slot's bundle: right after the slot's.
    pub fn bundle_mmu_page(&self) -> u32 {
        ROM_PAGES + self.slot_pages
    }

    /// Where the running slot's bundle reads, through the data cache.
    pub fn bundle_data_base(&self) -> u32 {
        DROM + self.bundle_mmu_page() * PAGE
    }

    /// Refuse unaddressable regions, overlaps, and non-sector/page alignment.
    /// Records must be readable through the initial 4 MiB IROM mapping; the island is mapped by
    /// pages, the store sectors are reached through the flash controller.
    pub fn check(&self) -> Result<(), &'static str> {
        if self.flash_bytes == 0 || self.flash_bytes % PAGE != 0
            || self.flash_bytes > PHYSICAL_PAGES * PAGE
        {
            return Err("flash size must be a nonzero MMU-addressable number of pages");
        }
        if self.slot_pages == 0 || self.slot_pages > MMU_ENTRIES - ROM_PAGES {
            return Err("slot capacity must fit above MMU entry 63");
        }
        let first = self.island_mmu_page()?;
        if first < ROM_PAGES { return Err("island MMU pages overlap the ROM mapping"); }
        if first < ROM_PAGES + self.slot_pages { return Err("island MMU pages overlap the slot mapping"); }
        if self.bundle_pages == 0 || self.bundle_pages > u32::from(u16::MAX)
            || first < self.bundle_mmu_page().saturating_add(self.bundle_pages)
        {
            return Err("bundle capacity must be nonzero and its MMU pages fit between the slot's and the island's");
        }
        let mut ranges = [(0, SECTOR); 12];
        for (n, &start) in self.records.iter().enumerate() {
            let end = start.checked_add(SECTOR).ok_or("record end overflows")?;
            if start % SECTOR != 0 || end > ROM_PAGES * PAGE || end > self.flash_bytes {
                return Err("records must be aligned sectors inside flash and the ROM mapping");
            }
            ranges[1 + n] = (start, end);
        }
        for (n, &start) in self.slots.iter().enumerate() {
            let end = start.checked_add(self.slot_pages * PAGE).ok_or("slot end overflows")?;
            if start % PAGE != 0 || end > self.flash_bytes {
                return Err("slots must be page aligned and fit in flash");
            }
            ranges[3 + n] = (start, end);
        }
        for (n, &start) in self.bundles.iter().enumerate() {
            let end = start.checked_add(self.bundle_pages * PAGE).ok_or("bundle end overflows")?;
            if start % PAGE != 0 || end > self.flash_bytes {
                return Err("bundles must be page aligned and fit in flash");
            }
            ranges[10 + n] = (start, end);
        }
        let island_end = self.island_pages.checked_mul(PAGE)
            .and_then(|bytes| self.island.checked_add(bytes)).ok_or("island end overflows")?;
        if self.island_pages == 0 || self.island % PAGE != 0 || island_end > self.flash_bytes {
            return Err("the island must be nonempty, page aligned and fit in flash");
        }
        ranges[5] = (self.island, island_end);
        for (n, &start) in self.calibration.iter().chain(&self.settings).enumerate() {
            let end = start.checked_add(SECTOR).ok_or("store end overflows")?;
            if start % SECTOR != 0 || end > self.flash_bytes {
                return Err("store sectors must be aligned and fit in flash");
            }
            ranges[6 + n] = (start, end);
        }
        for (n, &(start, end)) in ranges.iter().enumerate() {
            for &(other_start, other_end) in &ranges[n + 1..] {
                if start < other_end && other_start < end {
                    return Err("stage, records, slots, bundles, island and stores must not overlap");
                }
            }
        }
        Ok(())
    }
}

// Software format, not chip facts: boot-stage.md, Record.
const RECORD_MAGIC: u32 = 0x4d50_4231;
const CRC_POLY: u32 = 0xedb8_8320;
const CRC_INIT: u32 = u32::MAX;
const CRC_BYTES: usize = 52;
/// `crc32`'s byte step as a table: entry n is n's eight reflected shifts, so one table step
/// equals eight bit steps. The slot carries it for the boot's image CRC (`runtime::crc32`).
pub(crate) const CRC32_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut n = 0;
    while n < 256 {
        let mut crc = n as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { CRC_POLY } else { 0 };
            bit += 1;
        }
        table[n] = crc;
        n += 1;
    }
    table
};

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub seq: u32,
    pub slot: u8,
    pub prev: u8,
    pub wdt: u32,
    pub length: u32,
    /// The slot's bundle, in 64 KiB pages the stage maps after the slot; 0: none.
    pub bundle_pages: u16,
    pub sha256: [u8; 32],
    pub tries: u8,
    pub confirmed: bool,
}

impl Record {
    pub fn bytes(&self) -> [u8; 58] {
        let mut bytes = [0xff; 58];
        bytes[0..4].copy_from_slice(&RECORD_MAGIC.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.seq.to_le_bytes());
        bytes[8] = self.slot;
        bytes[9] = self.prev;
        bytes[10..12].copy_from_slice(&self.bundle_pages.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.wdt.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.length.to_le_bytes());
        bytes[20..52].copy_from_slice(&self.sha256);
        let crc = crc32(&bytes[..CRC_BYTES]);
        bytes[52..56].copy_from_slice(&crc.to_le_bytes());
        bytes[56] = self.tries;
        bytes[57] = if self.confirmed { 0 } else { 0xff };
        bytes
    }
}

/// CRC-32/ISO-HDLC: reflected polynomial, initial value and xorout per spec.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = CRC_INIT;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { CRC_POLY } else { 0 };
        }
    }
    crc ^ CRC_INIT
}

// esp-hal direct-boot ld/esp32s3/direct_boot_memory.x:1–3 and
// direct_boot_layout.x:59–60: magic offset, doubled magic, entry at +8.
const MAGIC_OFFSET: usize = 0x400;
const DIRECT_MAGIC: u32 = 0xaedb_041d;
// facts.md:87–88,96, TRM pp.403–405: SRAM1's data alias ends at 0x3FCF0000, and its last page
// holds the ROM's data: IDF v6.1.1 components/esp_rom/esp32s3/ld/esp32s3.rom.ld places all 159
// ROM data symbols in 0x3FCEF81C..=0x3FCEFFFC, which ROM code the radio island calls writes. One
// stack top below that page for every S3 image, so every slot shares one layout.
pub const STACK_TOP: u32 = 0x3fce_f000;
/// Start of SRAM1 through its data alias.
pub const SRAM_START: u32 = 0x3fc8_8000;
/// The CPU clock of every S3 image once init's first lines have run: the 480 MHz PLL divided by 2
/// (TRM p.530 Table 7.2-2; facts.md section 3). The ROM hands over on XTAL/2 with that PLL already
/// running for USB-Serial-JTAG (esp-hal soc/esp32s3/clocks.rs: the chip boots with the 480 MHz PLL),
/// so no PLL bring-up comes first: init moves the CPU to the PLL at 160 MHz, then raises the core
/// regulators to this chip's eFuse settings as IDF does and switches to 240 MHz (runtime.rs,
/// raise_240; on the board, sessions 19-21). Init's lines before the switch run at 160 MHz, which
/// the boot stage's hold covers.
pub const CPU_HZ: u32 = 240_000_000;
// ISA Summary pp.284–289 (PS),289–290 (windows),305 (INTENABLE).
const SR_PS: u32 = 230;
const SR_INTENABLE: u32 = 228;
const SR_WINDOWBASE: u32 = 72;
const SR_WINDOWSTART: u32 = 73;
const INTLEVEL_MAX: u8 = 15; // ISA p.644, WAITI's four-bit interrupt level.
const BOOT_PS: i32 = INTLEVEL_MAX as i32; // EXCM/UM/WOE clear, per spec step 0.
// IDF register/soc/reg_base.h:62; extmem_reg.h:1366–1380.
const CACHE_STATE: u32 = 0x600c_4130;
const ICACHE_SHIFT: u8 = 0;
const DCACHE_SHIFT: u8 = 12;
const CACHE_WIDTH: u8 = 12;
const CACHE_IDLE: i32 = 1;
// IDF register/soc/reg_base.h:14; rtc_cntl_reg.h:473,1091.
const RESET_STATE: u32 = 0x6000_8038;
const STORE0: u32 = 0x6000_8050;
// IDF register/soc/timer_group_reg.h:318–395,468–475; bases facts.md:109–110.
const MWDT_CONFIG: [u32; 2] = [0x6001_f048, 0x6002_0048];
const MWDT_PROTECT: [u32; 2] = [0x6001_f064, 0x6002_0064];
const MWDT_EN: u32 = 1 << 31;
const MWDT_FLASHBOOT: u32 = 1 << 14;
// IDF register/soc/rtc_cntl_reg.h:1988–2115; unlock procedure facts.md:115–131.
const RWDT_CONFIG: u32 = 0x6000_8098;
const RWDT_HOLD: u32 = 0x6000_809c;
const RWDT_FEED: u32 = 0x6000_80ac;
const RWDT_PROTECT: u32 = 0x6000_80b0;
const WDT_KEY: u32 = 0x50d8_3aa1;
const RWDT_EN: u32 = 1 << 31;
const RWDT_FLASHBOOT: u32 = 1 << 12;
const RWDT_STAGES: u32 = (7 << 28) | (7 << 25) | (7 << 22) | (7 << 19);
// facts-machine.md:409–410, TRM p.676: action 3 excludes RTC (Core reset).
const RWDT_CORE: u32 = 3 << 28;
const RWDT_FEED_BIT: u32 = 1 << 31; // rtc_cntl_reg.h:2101–2107.
// rtc_cntl_reg.h:2117–2173; facts.md:132: unlock, disable, no assumed relock.
const SWD_CONFIG: u32 = 0x6000_80b4;
const SWD_PROTECT: u32 = 0x6000_80b8;
const SWD_KEY: u32 = 0x8f1d_312a;
const SWD_DISABLE: u32 = 1 << 30;
// reg_base.h:59; system_reg.h:15–33. Clock bit 1 is clear in our write.
const CPU1_CONTROL: u32 = 0x600c_0000;
const CPU1_RESETING: u32 = 1 << 2;
const CPU1_RUNSTALL: u32 = 1;
// include/soc/soc.h:24; ext_mem_defs.h:46–73; mmu_ll.h:176–183.
const MMU_TABLE: u32 = 0x600c_5000;
const MMU_SLOT: u32 = MMU_TABLE + ROM_PAGES * 4;

// Port of IDF components/hal/esp32s3/include/hal/mmu_ll.h:176–183
// (Apache-2.0). A6 points to the first entry, A7 holds the physical page and A9 the nonzero count.
fn map_flash_pages(a: &mut Asm, page_loop: usize) {
    a.base.label_assign(page_loop);
    a.s32i(A7, A6, 0);
    a.addi(A7, A7, 1);
    a.addi(A6, A6, 4);
    a.addi(A9, A9, -1);
    a.bccz_reg_label(ccz::NE, A9, page_loop);
    a.memw();
    a.isync();
}
// reg_base.h:49; usb_serial_jtag_reg.h:14–46; facts-machine.md:639–646.
const USB_EP1: u32 = 0x6003_8000;
const USB_EP1_CONF: u32 = USB_EP1 + 4;
const USB_WR_DONE: u32 = 1;
const NO_IMAGE: &[u8] = b"makepad-boot: no image\r\n";

// These helpers use a6–a8 only, preserving record selection and saved arguments.
fn write(a: &mut Asm, addr: u32, value: u32) {
    a.mov_reg_i32(A6, addr);
    a.mov_reg_i32_optimised(A7, value);
    a.s32i(A7, A6, 0);
}

fn change_bits(a: &mut Asm, addr: u32, clear: u32, set: u32) {
    a.mov_reg_i32(A6, addr);
    a.l32i(A7, A6, 0);
    if clear != 0 {
        a.mov_reg_i32(A8, !clear);
        a.and(A7, A7, A8);
    }
    if set != 0 {
        a.mov_reg_i32(A8, set);
        a.or(A7, A7, A8);
    }
    a.s32i(A7, A6, 0);
}

// a4/a5: best record pointer/sequence. Each copy owns five labels.
fn record(a: &mut Asm, offset: u32, capacity: u32, bundle_capacity: u32, label: usize) {
    let (next, word, bit, no_xor, select) = (label, label + 1, label + 2, label + 3, label + 4);
    a.mov_reg_i32(A2, IROM + offset);
    a.l32i(A3, A2, 0);
    a.mov_reg_i32(A6, RECORD_MAGIC);
    a.bcc_reg_reg_label(cc::NE, A3, A6, next);
    a.l32i(A3, A2, 8);
    a.extui(A3, A3, 0, 8);
    a.movi_n(A6, 2);
    a.bcc_reg_reg_label(cc::GEU, A3, A6, next);
    // the bundle's pages (bytes 10..12) fit its region
    a.l32i(A3, A2, 8);
    a.extui(A3, A3, 16, 16);
    a.mov_reg_i32(A6, bundle_capacity);
    a.bcc_reg_reg_label(cc::LTU, A6, A3, next);
    a.l32i(A3, A2, 56);
    a.extui(A3, A3, 8, 8);
    a.bccz_reg_label(ccz::NE, A3, next);
    a.l32i(A3, A2, 16);
    a.bccz_reg_label(ccz::EQ, A3, next);
    a.mov_reg_i32(A6, capacity);
    a.bcc_reg_reg_label(cc::LTU, A6, A3, next);

    // Little-endian reflected CRC: XOR one aligned word, then consume 32 bits.
    // This equals four byte updates and avoids forbidden IROM byte accesses
    // (facts.md:98, TRM pp.401–402). No DROM access is needed.
    a.movi_n(A6, -1);
    a.mov_n(A7, A2);
    a.movi_n(A8, (CRC_BYTES / 4) as i32);
    a.mov_reg_i32(A11, CRC_POLY);
    a.movi_n(A10, 1);
    a.ssr(A10);
    a.base.label_assign(word);
    a.l32i(A3, A7, 0);
    a.xor(A6, A6, A3);
    a.movi_n(A9, 32);
    a.base.label_assign(bit);
    a.extui(A10, A6, 0, 1);
    a.srl(A6, A6);
    a.bccz_reg_label(ccz::EQ, A10, no_xor);
    a.xor(A6, A6, A11);
    a.base.label_assign(no_xor);
    a.addi(A9, A9, -1);
    a.bccz_reg_label(ccz::NE, A9, bit);
    a.addi(A7, A7, 4);
    a.addi(A8, A8, -1);
    a.bccz_reg_label(ccz::NE, A8, word);
    a.movi_n(A3, -1);
    a.xor(A6, A6, A3);
    a.l32i(A3, A2, CRC_BYTES as u32);
    a.bcc_reg_reg_label(cc::NE, A3, A6, next);
    a.l32i(A3, A2, 4);
    a.bccz_reg_label(ccz::EQ, A4, select);
    a.bcc_reg_reg_label(cc::GEU, A5, A3, next); // Equal sequence retains R0.
    a.base.label_assign(select);
    a.mov_n(A4, A2);
    a.mov_n(A5, A3);
    a.base.label_assign(next);
}

/// Flash bytes starting at offset zero, ending after the stage (at most 4 KiB).
/// Panics for an invalid layout. The stage does not modify VECBASE or flash.
pub fn boot_stage(layout: &Layout) -> Vec<u8> {
    layout.check().expect("invalid S3 boot layout");
    const CACHE_WAIT: usize = 0;
    const NO_RECORD: usize = 11;
    const SLEEP: usize = 12;
    const MAP: usize = 13;
    const SLOT_CHOSEN: usize = 14;
    const PAGE_LOOP: usize = 15;
    const BUNDLE_CHOSEN: usize = 16;
    const BUNDLE_LOOP: usize = 17;
    const NO_BUNDLE: usize = 18;
    let code = assemble(&mut Asm::new(19, A15), |a| {
        // First executed instructions, before even the jump over literals.
        // Only a0–a3 may be used until the register window is normalized.
        a.movi_n(A0, BOOT_PS);
        a.wsr(A0, SR_PS);
        a.rsync();
        a.literal_table(); // offset 8, aligned; its J skips the literal pool.
        // Port: esp-hal direct-boot src/xtensa.rs:32–34.
        a.movi_n(A0, 0);
        a.wsr(A0, SR_INTENABLE);
        a.rsync();
        // Port: esp-hal direct-boot src/xtensa.rs:89–96, whole-field idle check.
        a.mov_reg_i32(A0, CACHE_STATE);
        a.movi_n(A2, CACHE_IDLE);
        a.base.label_assign(CACHE_WAIT);
        a.l32i(A1, A0, 0);
        a.extui(A3, A1, ICACHE_SHIFT, CACHE_WIDTH);
        a.bcc_reg_reg_label(cc::NE, A3, A2, CACHE_WAIT);
        a.extui(A3, A1, DCACHE_SHIFT, CACHE_WIDTH);
        a.bcc_reg_reg_label(cc::NE, A3, A2, CACHE_WAIT);
        // Port: esp-hal direct-boot src/xtensa.rs:55–60; retain our PS.
        a.movi_n(A0, 0);
        a.wsr(A0, SR_WINDOWBASE);
        a.rsync();
        a.movi_n(A0, 1);
        a.wsr(A0, SR_WINDOWSTART);
        a.rsync();
        a.mov_reg_i32(A1, STACK_TOP);
        // No VECBASE write or vectors (v1 step 4). Two save slots in SRAM1.
        a.addi(A1, A1, -16);
        a.mov_reg_i32(A0, RESET_STATE);
        a.l32i(A2, A0, 0);
        a.s32i(A2, A1, 0);
        a.mov_reg_i32(A0, STORE0);
        a.l32i(A3, A0, 0);
        a.s32i(A3, A1, 4);
        for n in 0..2 {
            write(a, MWDT_PROTECT[n], WDT_KEY);
            change_bits(a, MWDT_CONFIG[n], MWDT_EN | MWDT_FLASHBOOT, 0);
            write(a, MWDT_PROTECT[n], 0);
        }
        write(a, SWD_PROTECT, SWD_KEY);
        change_bits(a, SWD_CONFIG, 0, SWD_DISABLE);
        write(a, RWDT_PROTECT, WDT_KEY);
        change_bits(a, RWDT_CONFIG, RWDT_EN | RWDT_FLASHBOOT, 0);
        write(a, RWDT_PROTECT, 0);
        write(a, CPU1_CONTROL, CPU1_RESETING | CPU1_RUNSTALL);
        a.movi_n(A4, 0);
        a.movi_n(A5, 0);
        record(a, layout.records[0], layout.slot_pages * PAGE, layout.bundle_pages, 1);
        record(a, layout.records[1], layout.slot_pages * PAGE, layout.bundle_pages, 6);
        a.bccz_reg_label(ccz::EQ, A4, NO_RECORD);
        a.l32i(A9, A4, 12);
        a.bccz_reg_label(ccz::EQ, A9, MAP);
        write(a, RWDT_PROTECT, WDT_KEY);
        a.mov_reg_i32(A6, RWDT_HOLD);
        a.s32i(A9, A6, 0);
        change_bits(a, RWDT_CONFIG, RWDT_STAGES | RWDT_FLASHBOOT, RWDT_EN | RWDT_CORE);
        write(a, RWDT_FEED, RWDT_FEED_BIT);
        write(a, RWDT_PROTECT, 0);

        a.base.label_assign(MAP);
        // the bundle's pages, before a4 takes the sequence
        a.l32i(A10, A4, 8);
        a.extui(A10, A10, 16, 16);
        a.l32i(A9, A4, 16);
        a.l32i(A5, A4, 8);
        a.extui(A5, A5, 0, 8);
        a.l32i(A4, A4, 4);
        // ceil(length / 64 KiB), with no overflowing length + 65535.
        a.addi(A9, A9, -1);
        a.extui(A9, A9, 16, 16);
        a.addi(A9, A9, 1);
        a.mov_reg_i32(A7, layout.slots[0] / PAGE);
        a.bccz_reg_label(ccz::EQ, A5, SLOT_CHOSEN);
        a.mov_reg_i32(A7, layout.slots[1] / PAGE);
        a.base.label_assign(SLOT_CHOSEN);
        a.mov_reg_i32(A6, MMU_SLOT);
        map_flash_pages(a, PAGE_LOOP);
        // the slot's own bundle, at the entries after the slot's
        a.bccz_reg_label(ccz::EQ, A10, NO_BUNDLE);
        a.mov_n(A9, A10);
        a.mov_reg_i32(A7, layout.bundles[0] / PAGE);
        a.bccz_reg_label(ccz::EQ, A5, BUNDLE_CHOSEN);
        a.mov_reg_i32(A7, layout.bundles[1] / PAGE);
        a.base.label_assign(BUNDLE_CHOSEN);
        a.mov_reg_i32(A6, MMU_TABLE + layout.bundle_mmu_page() * 4);
        map_flash_pages(a, BUNDLE_LOOP);
        a.base.label_assign(NO_BUNDLE);
        a.l32i(A2, A1, 0);
        a.l32i(A3, A1, 4);
        a.addi(A1, A1, 16);
        // The slot's first 0x400 bytes are its vector block (VECBASE = SLOT_BASE): enter at INIT.
        a.mov_reg_i32(A0, runtime::INIT);
        a.jx(A0);

        a.base.label_assign(NO_RECORD);
        a.mov_reg_i32(A6, USB_EP1);
        for &byte in NO_IMAGE {
            a.movi(A7, i32::from(byte));
            a.s32i(A7, A6, 0);
        }
        write(a, USB_EP1_CONF, USB_WR_DONE);
        a.base.label_assign(SLEEP);
        a.waiti(INTLEVEL_MAX);
        a.j_label(SLEEP);
    });
    let mut bytes = vec![0xff; MAGIC_OFFSET];
    bytes.extend_from_slice(&DIRECT_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&DIRECT_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&code);
    assert!(bytes.len() <= SECTOR as usize, "S3 boot stage exceeds its sector");
    bytes
}

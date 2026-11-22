//! Shared S3 test support: the Direct Boot image builder and run helpers.
#![allow(dead_code)]

use std::sync::Arc;

use fab_elec_mcu_emit32::xtensa::{ccz, enc};
use fab_elec_mcu_emu::{HaltKind, Machine, PcSet, Ran, Status, Until};
use fab_elec_mcu_s3::{CpuClock, S3Machine, S3Profile, StopDetail};

// Each block's fixtures (its profile and program helpers) live in its own file.
mod pins_fixture;
mod time_fixture;
// Only tests/i2c.rs and tests/speaker.rs use it; a glob re-export would be an unused import elsewhere.
pub mod i2c_fixture;
pub mod spi_fixture;
pub use pins_fixture::*;
pub use time_fixture::*;

// emcu-s3.md:27–30, corrected Direct Boot magic/entry/4 MiB handoff.
pub const FLASH: u32 = 0x4200_0000;
pub const ENTRY: u32 = FLASH + 0x408;
// Program::new()'s PS-first prologue: three 3-byte instructions, 24 ticks each.
pub const BODY: u32 = ENTRY + 9;
pub const PROLOGUE_TICKS: u64 = 3 * 24;
// facts.md §2, TRM pp.403–405, Tables 4.3-1/2.
pub const RAM: u32 = 0x3fc8_8000;
pub const IRAM: u32 = 0x4037_8000;
pub const XIP: u32 = 0x4240_0000;
pub const DROM: u32 = 0x3c40_0000;
// facts.md §3, TRM pp.409,830,840; facts-machine.md §3, soc.h:24.
pub const SYSCLK: u32 = 0x600c_0060;
pub const CPU_PER: u32 = 0x600c_0010;
pub const MMU64: u32 = 0x600c_5000 + 64 * 4;

pub fn profile(seed: u64) -> S3Profile {
    S3Profile {
        mac: [2, 0, 0, 0, 0, 1], pvt: fab_elec_mcu_s3::NO_PVT,
        flash_bytes: 16 * 1024 * 1024, handoff_cpu: CpuClock::Xtal { div: 2 },
        handoff_mapped_pages: 64, entry: ENTRY, seed,
        time: time_profile(), pins: pins_profile(),
    }
}


// Firmware fixtures use emit32's encoder, including its checked L32R
// displacement. Literal words occupy flash below the Direct Boot magic.
pub struct Program { pub image: Vec<u8>, literal: usize }

impl Program {
    /// The image starts as every real image does: the boot stage's step 0
    /// writes PS = 15 (EXCM, UM and WOE clear) before any other register is
    /// touched, so junk handoff PS.WOE and WindowStart cannot raise a window
    /// overflow (emit_s3/src/lib.rs BOOT_PS; emit_s3/tests/boot.rs). It uses
    /// only a0, which no window check covers (ISA p. 246). Body code starts
    /// at BODY, PROLOGUE_TICKS after the handoff.
    pub fn new() -> Self {
        let mut p = Self::raw();
        p.emit(enc::movi(0, 15));
        p.emit(enc::wsr(0, 230)); // WSR a0, PS: ISA pp. 284–289, 646.
        p.emit(enc::rsync());
        p
    }

    /// No prologue: the first instruction is at ENTRY with handoff junk in
    /// PS. Only for fixtures that use a0..a3 alone, so that no window check
    /// applies (ISA p. 246).
    pub fn raw() -> Self {
        let mut image = vec![0xff; 0x408];
        for n in [0x400, 0x404] {
            image[n..n + 4].copy_from_slice(&0xaedb_041du32.to_le_bytes());
        }
        Self { image, literal: 0 }
    }

    pub fn pc(&self) -> u32 { FLASH + self.image.len() as u32 }
    pub fn emit(&mut self, bits: u32) -> u32 {
        let pc = self.pc();
        self.image.extend_from_slice(&bits.to_le_bytes()[..3]);
        pc
    }
    /// L32R from the literal pool, reusing an equal literal: the pool's 0x400 bytes hold 256 distinct
    /// values, and every literal lies behind every instruction, within L32R's reach.
    pub fn load(&mut self, reg: u8, value: u32) {
        let bytes = value.to_le_bytes();
        let at = match (0..self.literal).step_by(4).find(|&at| self.image[at..at + 4] == bytes) {
            Some(at) => at,
            None => {
                assert!(self.literal + 4 <= 0x400, "fixture literals leave magic intact");
                self.image[self.literal..self.literal + 4].copy_from_slice(&bytes);
                self.literal += 4;
                self.literal - 4
            }
        };
        self.emit(enc::l32r(reg, self.pc(), FLASH + at as u32));
    }
    pub fn store(&mut self, addr: u32, value: u32) -> u32 {
        self.load(1, addr);
        self.load(2, value);
        self.emit(enc::s32i(2, 1, 0))
    }
    pub fn sr(&mut self, number: u32, value: u32) -> u32 {
        self.load(2, value);
        self.emit(enc::rsr(0, 3, 1, number, 2))
    }
    pub fn halt(&mut self) -> u32 { self.emit(enc::rrr(0, 0, 0, 4, 0, 0)) }
    pub fn counted_loop(&mut self, n: i32) {
        self.emit(enc::movi(4, n));
        let start = self.emit(enc::addi(4, 4, -1));
        self.emit(enc::bccz(ccz::NE, 4, (i64::from(start) - i64::from(self.pc()) - 4) as i32));
    }
    pub fn finish(self) -> Arc<Vec<u8>> { Arc::new(self.image) }
}

pub fn run_to(machine: &mut S3Machine, tick: u64, marks: &PcSet) -> Ran {
    machine.run(&Until { tick, marks, stops: &[], stops_after: 0, on_drive: false })
}

pub fn at_pc(machine: &mut S3Machine, pc: u32) {
    let mut marks = PcSet::new(pc, 1);
    marks.insert(pc);
    let after = machine.now();
    assert_eq!(machine.run(&Until { tick: 100_000, marks: &marks, stops: &[pc], stops_after: after, on_drive: false }),
        Ran::Pc, "firmware reaches the requested instruction boundary");
}

pub fn assert_stop(machine: &mut S3Machine, pc: u32, what: &'static str, detail: Option<StopDetail>) {
    assert_eq!(run_to(machine, 100_000, &PcSet::default()), Ran::Halted, "unsupported operation halts: {what}");
    let Status::Halted(halt) = machine.status() else { panic!("halt must remain inspectable: {what}"); };
    assert_eq!((halt.pc, halt.kind, halt.what), (pc, HaltKind::Unmodeled, what), "stop retains faulting PC and class: {what}");
    assert_eq!(halt.tick, machine.now(), "halt timestamp uses machine ticks: {what}");
    assert_eq!(machine.stop_detail(), detail, "stop retains address, width, access kind and MMU entry: {what}");
    assert_eq!(run_to(machine, 0, &PcSet::default()), Ran::Halted, "halt outranks an expired deadline: {what}");
}

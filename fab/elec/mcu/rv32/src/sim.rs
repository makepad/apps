//! Retired-instruction execution on the emulator core's bus ([`fab_elec_mcu_emu::bus`]).

use crate::insn::{decode, BranchOp, CsrOp, ImmOp, Insn, LoadOp, Reg, RegOp, StoreOp};

pub use fab_elec_mcu_emu::bus::{Bus, Kind, Memory, Perm, Width};

pub mod cause {
    pub const INSN_MISALIGNED: u32 = 0;
    pub const INSN_ACCESS: u32 = 1;
    pub const ILLEGAL: u32 = 2;
    pub const BREAKPOINT: u32 = 3;
    pub const LOAD_MISALIGNED: u32 = 4;
    pub const LOAD_ACCESS: u32 = 5;
    pub const STORE_MISALIGNED: u32 = 6;
    pub const STORE_ACCESS: u32 = 7;
    pub const ECALL_M: u32 = 11;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapMode { Stop, Take }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Ebreak { pc: u32 },
    /// WFI has retired; this is the address of the following instruction.
    Wfi { pc: u32 },
    Trap { cause: u32, pc: u32, tval: u32 },
    /// The instruction at `pc` needs behaviour this model does not have; nothing changed.
    Unmodeled { pc: u32, what: &'static str },
    Limit,
}

pub struct Cpu {
    /// Instructions always read x0 as zero, even if a caller edits this array.
    /// Retiring an instruction also normalizes the exposed x[0] slot to zero.
    pub x: [u32; 32],
    pub pc: u32,
    pub instret: u64,
    pub mstatus: u32,
    pub mtvec: u32,
    pub mepc: u32,
    pub mcause: u32,
    pub mtval: u32,
    pub mscratch: u32,
    pub traps: TrapMode,
}

const MIE: u32 = 1 << 3;
const MPIE: u32 = 1 << 7;
const MPP: u32 = 3 << 11;

fn status(value: u32) -> u32 {
    // C3 fact: MPP's upper bit is tied to its lower bit (bit 11).
    (value & (MIE | MPIE)) | if value & (1 << 11) != 0 { MPP } else { 0 }
}

impl Cpu {
    pub fn new(pc: u32) -> Cpu {
        Self {
            x: [0; 32], pc, instret: 0, mstatus: 0, mtvec: 1,
            mepc: 0, mcause: 0, mtval: 0, mscratch: 0, traps: TrapMode::Stop,
        }
    }

    fn reg(&self, reg: Reg) -> u32 { if reg == 0 { 0 } else { self.x[reg as usize] } }

    fn trap(&mut self, cause: u32, tval: u32) -> Result<(), Stop> {
        if self.traps == TrapMode::Stop {
            return Err(if cause == cause::BREAKPOINT {
                Stop::Ebreak { pc: self.pc }
            } else {
                Stop::Trap { cause, pc: self.pc, tval }
            });
        }
        // Privileged §§3.1.6.1, 3.1.7, 3.1.14–16: synchronous exceptions go
        // to BASE even in vectored mode. C3 BASE is bits 31:8.
        // Also mask an externally supplied odd PC: mepc[0] is always zero
        // (§3.1.14), while mtval preserves the original misaligned address.
        self.mepc = self.pc & !1;
        self.mcause = cause;
        self.mtval = tval;
        self.mstatus = (status(self.mstatus) & !(MIE | MPIE | MPP))
            | ((self.mstatus & MIE) << 4) | MPP;
        self.pc = self.mtvec & !0xff;
        Ok(())
    }

    fn csr_known(csr: u16) -> bool {
        matches!(csr, 0x300 | 0x305 | 0x340 | 0x341 | 0x342 | 0x343 | 0xf14)
    }

    fn csr_read(&self, csr: u16) -> u32 {
        // Mask reads too: CSR fields are public for test setup/inspection.
        match csr {
            0x300 => status(self.mstatus),
            0x305 => (self.mtvec & !0xff) | 1,
            0x340 => self.mscratch,
            0x341 => self.mepc & !1,
            0x342 => self.mcause & 0x8000001f,
            0x343 => self.mtval,
            0xf14 => 0,
            _ => unreachable!("CSR validated before access"),
        }
    }

    fn csr_write(&mut self, csr: u16, value: u32) {
        match csr {
            0x300 => self.mstatus = status(value),
            0x305 => self.mtvec = (value & !0xff) | 1,
            0x340 => self.mscratch = value,
            0x341 => self.mepc = value & !1,
            0x342 => self.mcause = value & 0x8000001f,
            0x343 => self.mtval = value,
            _ => unreachable!("CSR write permission validated before access"),
        }
    }

    /// Executes one instruction, or takes an exception without retiring it.
    /// In Stop mode exceptions leave all CPU fields unchanged. A Bus must
    /// uphold its no-partial-write contract to provide the same for memory.
    pub fn step(&mut self, bus: &mut impl Bus) -> Result<(), Stop> {
        // §7.1: IALIGN=16 makes odd targets impossible via instructions.
        // Choice for an externally supplied odd pc: trap before fetching.
        if self.pc & 1 != 0 { return self.trap(cause::INSN_MISALIGNED, self.pc); }
        let low = match bus.read(self.pc, Width::H, Kind::Fetch, self.instret) {
            Ok(value) => value & 0xffff,
            Err(()) => return self.trap(cause::INSN_ACCESS, self.pc),
        };
        let bits = if low & 3 == 3 {
            let addr = self.pc.wrapping_add(2);
            match bus.read(addr, Width::H, Kind::Fetch, self.instret) {
                Ok(value) => low | (value & 0xffff) << 16,
                // Privileged §3.1.16: tval identifies the failing portion.
                Err(()) => return self.trap(cause::INSN_ACCESS, addr),
            }
        } else { low };
        let (insn, len) = match decode(bits) {
            Ok(decoded) => decoded,
            Err(()) => return self.trap(cause::ILLEGAL, bits),
        };
        let mut next = self.pc.wrapping_add(len);
        let mut result = None;
        match insn {
            Insn::Lui { rd, imm } => result = Some((rd, imm)),
            Insn::Auipc { rd, imm } => result = Some((rd, self.pc.wrapping_add(imm))),
            Insn::Jal { rd, offset } => {
                result = Some((rd, next));
                next = self.pc.wrapping_add(offset as u32);
            }
            Insn::Jalr { rd, rs1, offset } => {
                result = Some((rd, next));
                next = self.reg(rs1).wrapping_add(offset as u32) & !1;
            }
            Insn::Branch { op, rs1, rs2, offset } => {
                let (a, b) = (self.reg(rs1), self.reg(rs2));
                let taken = match op {
                    BranchOp::Beq => a == b, BranchOp::Bne => a != b,
                    BranchOp::Blt => (a as i32) < (b as i32),
                    BranchOp::Bge => (a as i32) >= (b as i32),
                    BranchOp::Bltu => a < b, BranchOp::Bgeu => a >= b,
                };
                if taken { next = self.pc.wrapping_add(offset as u32); }
            }
            Insn::Load { op, rd, rs1, offset } => {
                let addr = self.reg(rs1).wrapping_add(offset as u32);
                let width = match op { LoadOp::Lb | LoadOp::Lbu => Width::B, LoadOp::Lh | LoadOp::Lhu => Width::H, LoadOp::Lw => Width::W };
                if addr % width.bytes() as u32 != 0 { return self.trap(cause::LOAD_MISALIGNED, addr); }
                // §2.1.6: rd=x0 still performs the access and can fault.
                let value = match bus.read(addr, width, Kind::Load, self.instret) {
                    Ok(v) => v,
                    Err(()) => return self.trap(cause::LOAD_ACCESS, addr),
                };
                result = Some((rd, match op {
                    LoadOp::Lb => value as u8 as i8 as i32 as u32,
                    LoadOp::Lh => value as u16 as i16 as i32 as u32,
                    LoadOp::Lbu => value & 0xff, LoadOp::Lhu => value & 0xffff,
                    LoadOp::Lw => value,
                }));
            }
            Insn::Store { op, rs1, rs2, offset } => {
                let addr = self.reg(rs1).wrapping_add(offset as u32);
                let width = match op { StoreOp::Sb => Width::B, StoreOp::Sh => Width::H, StoreOp::Sw => Width::W };
                if addr % width.bytes() as u32 != 0 { return self.trap(cause::STORE_MISALIGNED, addr); }
                if bus.write(addr, width, self.reg(rs2), self.instret).is_err() {
                    return self.trap(cause::STORE_ACCESS, addr);
                }
            }
            Insn::OpImm { op, rd, rs1, imm } => {
                let a = self.reg(rs1);
                let b = imm as u32;
                result = Some((rd, match op {
                    ImmOp::Addi => a.wrapping_add(b),
                    ImmOp::Slti => ((a as i32) < imm) as u32,
                    ImmOp::Sltiu => (a < b) as u32,
                    ImmOp::Xori => a ^ b, ImmOp::Ori => a | b, ImmOp::Andi => a & b,
                    ImmOp::Slli => a << b, ImmOp::Srli => a >> b,
                    ImmOp::Srai => ((a as i32) >> b) as u32,
                }));
            }
            Insn::Op { op, rd, rs1, rs2 } => result = Some((rd, reg_op(op, self.reg(rs1), self.reg(rs2)))),
            Insn::Fence | Insn::FenceI | Insn::Wfi => {}
            Insn::Ecall => return self.trap(cause::ECALL_M, 0),
            Insn::Ebreak => return self.trap(cause::BREAKPOINT, self.pc),
            Insn::Mret => {
                // Only M is modeled: a return to U (MPP 0) would run code in a mode this model
                // does not have, so it stops before changing anything.
                if self.mstatus & MPP != MPP {
                    return Err(Stop::Unmodeled { pc: self.pc, what: "MRET to user mode" });
                }
                next = self.mepc & !1;
                // Privileged §3.1.6.1: MPIE becomes 1. The C3 manual's claim
                // that MRET clears MPIE contradicts this; the ISA manual wins.
                // MPP becomes U (0), the least privileged mode the C3 has (§3.1.6.1).
                self.mstatus = ((self.mstatus & MPIE) >> 4) | MPIE;
            }
            Insn::Csr { op, rd, src, csr } => {
                let swap = matches!(op, CsrOp::Rw | CsrOp::Rwi);
                let write = swap || src != 0;
                if !Self::csr_known(csr) || (write && csr == 0xf14) {
                    return self.trap(cause::ILLEGAL, bits);
                }
                // Zicsr §4.2.1 Table 21: suppression depends on register
                // NUMBER/uimm, not the register's value; swaps always write.
                let old = if swap && rd == 0 { 0 } else { self.csr_read(csr) };
                let value = if matches!(op, CsrOp::Rwi | CsrOp::Rsi | CsrOp::Rci) {
                    src as u32
                } else { self.reg(src) };
                if write {
                    self.csr_write(csr, match op {
                        CsrOp::Rw | CsrOp::Rwi => value,
                        CsrOp::Rs | CsrOp::Rsi => old | value,
                        CsrOp::Rc | CsrOp::Rci => old & !value,
                    });
                }
                result = Some((rd, old));
            }
        }
        if let Some((rd, value)) = result {
            if rd != 0 { self.x[rd as usize] = value; }
        }
        self.x[0] = 0;
        self.pc = next;
        // Choice: counter wraps modulo 2^64, like a hardware counter.
        self.instret = self.instret.wrapping_add(1);
        if insn == Insn::Wfi { Err(Stop::Wfi { pc: next }) } else { Ok(()) }
    }

    /// Steps until a stop, or Limit after `max` instructions retire. Trap entry
    /// does not consume this budget: a Take-mode loop that only faults can run
    /// forever. Use `step` when a caller also needs to bound exception attempts.
    pub fn run(&mut self, bus: &mut impl Bus, max: u64) -> Stop {
        let start = self.instret;
        while self.instret.wrapping_sub(start) < max {
            if let Err(stop) = self.step(bus) { return stop; }
        }
        Stop::Limit
    }
}

fn reg_op(op: RegOp, a: u32, b: u32) -> u32 {
    match op {
        RegOp::Add => a.wrapping_add(b), RegOp::Sub => a.wrapping_sub(b),
        RegOp::Sll => a << (b & 31), RegOp::Srl => a >> (b & 31),
        RegOp::Sra => ((a as i32) >> (b & 31)) as u32,
        RegOp::Slt => ((a as i32) < (b as i32)) as u32, RegOp::Sltu => (a < b) as u32,
        RegOp::Xor => a ^ b, RegOp::Or => a | b, RegOp::And => a & b,
        // §4.5.1: MULHSU interprets only rs1 as signed. All products fit
        // these 64-bit intermediates, including MIN * unsigned MAX.
        RegOp::Mul => a.wrapping_mul(b),
        RegOp::Mulh => (((a as i32 as i64) * (b as i32 as i64)) >> 32) as u32,
        RegOp::Mulhsu => (((a as i32 as i64) * i64::from(b)) >> 32) as u32,
        RegOp::Mulhu => ((u64::from(a) * u64::from(b)) >> 32) as u32,
        // §4.5.2 Table 22: zero divisor gives all-one quotient/original
        // dividend remainder. Widen signed division to make MIN/-1 defined;
        // truncating the result gives MIN and remainder zero, with no panic.
        RegOp::Div => if b == 0 { u32::MAX } else { ((a as i32 as i64) / (b as i32 as i64)) as u32 },
        RegOp::Divu => if b == 0 { u32::MAX } else { a / b },
        RegOp::Rem => if b == 0 { a } else { ((a as i32 as i64) % (b as i32 as i64)) as u32 },
        RegOp::Remu => if b == 0 { a } else { a % b },
    }
}

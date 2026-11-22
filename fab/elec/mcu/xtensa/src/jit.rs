//! Inline lowering of decoded instructions for native code units (`emu::jit`, feature `jit`).
//!
//! A unit runs a straight-line run of inline instructions with the guest registers it touches
//! held in virtual registers (`Regs`), stored back at the run's end or at a side exit. Each
//! lowering has exactly `Cpu::execute`'s effect on the registers it names, for an instruction
//! of lane class 0 with the window gate off (PS.WOE clear or PS.EXCM set, so `execute_decoded`
//! checks no window): the caller counts the retirement and sets the PC from the `Flow`, and
//! runs every other instruction on its interpreter.

use std::mem::offset_of;

use fab_elec_mcu_emu::jit::rir::{Cls, Cond, Conv, IOp, Inst, Mem, VReg, I32, I64, U32, U64};
use fab_elec_mcu_emu::jit::UnitBuilder;

use crate::insn::{target, Insn, Op};
use crate::sim::Cpu;

/// The instruction forms `lower` takes, each covered by the differential test against
/// `Cpu::execute` (`tests/jit.rs`), which fails for a form it cannot find an encoding of.
pub const FORMS: &[Op] = {
    use Op::*;
    &[
        Add, AddN, Addi, Addmi, AddiN, Sub, And, Or, Xor, Movi, MoviN, MovN, Extui, L32r,
        Addx2, Addx4, Addx8, Subx2, Subx4, Subx8, Neg, Abs, Salt, Saltu, Min, Max, Minu, Maxu,
        Moveqz, Movnez, Movltz, Movgez, Mull, Muluh, Mulsh,
        Slli, Srli, Srai, Ssl, Ssr, Ssai, Ssa8l, Ssa8b, Sll, Srl, Sra, Src,
        L8ui, L16ui, L16si, L32i, L32iN, S8i, S16i, S32i, S32iN,
        J, Jx, Ret, RetN, Call0, Callx0,
        Beq, Bne, Blt, Bge, Bltu, Bgeu, Beqz, Bnez, BeqzN, BnezN, Bgez, Bltz,
        Beqi, Bnei, Blti, Bgei, Bltui, Bgeui,
        Ball, Bany, Bnall, Bnone, Bbc, Bbs, Bbci, Bbsi,
    ]
};

/// Where control goes after an inline instruction.
#[derive(Clone, Copy, Debug)]
pub enum Flow {
    /// The next instruction.
    Next,
    /// `target` when `taken` is nonzero, else the next instruction.
    Branch { taken: VReg, target: u32 },
    /// `target`.
    Jump(u32),
    /// The address in the register (a 32-bit value, zero-extended).
    Indirect(VReg),
}

/// What a lowering asks of the unit around it.
pub trait Host {
    /// A block that leaves the run before the instruction being lowered, after storing what
    /// `regs` holds: the interpreter then runs that instruction.
    fn side_exit(&mut self, b: &mut UnitBuilder, regs: &Regs) -> u32;
    /// The value a load of `bytes` bytes at `addr` reads, zero-extended (sign-extended from 16
    /// bits and then zero-extended from 32 when `signed`). The host emits the access it can make
    /// inline into the current block and leaves by `side_exit` for every other: whatever
    /// `Cpu::execute` would not make as a plain memory read (alignment, devices, faults).
    fn load(&mut self, b: &mut UnitBuilder, regs: &Regs, addr: VReg, bytes: u8, signed: bool) -> VReg;
    /// Stores the low `bytes` bytes of `value` at `addr`, as `load` does its reads.
    fn store(&mut self, b: &mut UnitBuilder, regs: &Regs, addr: VReg, bytes: u8, value: VReg);
}

/// One guest register's value in a run, and whether the run wrote it.
#[derive(Clone, Copy)]
struct Held {
    v: VReg,
    dirty: bool,
}

/// The guest registers a run has read or written so far, each in a vreg as a zero-extended
/// 32-bit value.
#[derive(Clone)]
pub struct Regs {
    base: VReg,
    cpu: i32,
    ar: [Option<Held>; 16],
    sar: Option<Held>,
}

impl Regs {
    /// The registers of the `Cpu` at byte offset `cpu` from the pointer in `base`, none read yet.
    pub fn new(base: VReg, cpu: i32) -> Regs {
        Regs { base, cpu, ar: [None; 16], sar: None }
    }

    fn ar_offset(&self, n: u8) -> i32 {
        self.cpu + (offset_of!(Cpu, ar) + 4 * usize::from(n)) as i32
    }

    fn sar_offset(&self) -> i32 {
        self.cpu + offset_of!(Cpu, sar) as i32
    }

    /// AR `n` of the current window.
    pub fn ar(&mut self, b: &mut UnitBuilder, n: u8) -> VReg {
        let n = usize::from(n & 15);
        if let Some(held) = self.ar[n] { return held.v; }
        let v = b.vreg(Cls::I);
        b.emit(Inst::Load(Mem::Int(4, false), v, self.base, self.ar_offset(n as u8)));
        self.ar[n] = Some(Held { v, dirty: false });
        v
    }

    /// Writes AR `n` (`v` zero-extended).
    pub fn set_ar(&mut self, n: u8, v: VReg) {
        self.ar[usize::from(n & 15)] = Some(Held { v, dirty: true });
    }

    /// SAR.
    pub fn sar(&mut self, b: &mut UnitBuilder) -> VReg {
        if let Some(held) = self.sar { return held.v; }
        let v = b.vreg(Cls::I);
        b.emit(Inst::Load(Mem::Int(4, false), v, self.base, self.sar_offset()));
        self.sar = Some(Held { v, dirty: false });
        v
    }

    /// Writes SAR (`v` zero-extended).
    pub fn set_sar(&mut self, v: VReg) {
        self.sar = Some(Held { v, dirty: true });
    }

    /// Stores every register the run wrote into the current block, leaving this state as it is
    /// (for a side path).
    pub fn store(&self, b: &mut UnitBuilder) {
        for (n, held) in self.ar.iter().enumerate() {
            if let Some(Held { v, dirty: true }) = *held {
                b.emit(Inst::Store(Mem::Int(4, false), self.base, self.ar_offset(n as u8), v));
            }
        }
        if let Some(Held { v, dirty: true }) = self.sar {
            b.emit(Inst::Store(Mem::Int(4, false), self.base, self.sar_offset(), v));
        }
    }

    /// Stores every register the run wrote into the current block and marks them stored.
    pub fn flush(&mut self, b: &mut UnitBuilder) {
        self.store(b);
        for held in self.ar.iter_mut().flatten().chain(self.sar.as_mut()) {
            held.dirty = false;
        }
    }
}

/// Whether `lower` takes `i`. `literal` is the word an L32R at this address loads, when the
/// caller could read it now and it cannot change while the unit lives.
pub fn inline(i: &Insn, literal: Option<u32>) -> bool {
    FORMS.contains(&i.op) && (i.op != Op::L32r || literal.is_some())
}

/// Whether `i` ends a run: it may leave the sequential path, or the run by a side exit.
pub fn ends_run(i: &Insn) -> bool {
    use Op::*;
    matches!(i.op, Jx | Ret | RetN | Callx0) || (i.op != L32r && target(i, 0).is_some()) || leaves(i)
}

/// Whether `i` may leave its run by a side exit, so that its host gives it a way to the
/// interpreter (and the next instruction a block of its own).
pub fn leaves(i: &Insn) -> bool {
    use Op::*;
    matches!(i.op, L8ui | L16ui | L16si | L32i | L32iN | S8i | S16i | S32i | S32iN
        | Sll | Srl | Sra | Src)
}

/// `a cond k` as 0 or 1, `k` taken as the 64-bit value of an operand of that signedness.
fn cmp_k(b: &mut UnitBuilder, cond: Cond, signed: bool, a: VReg, k: i64) -> VReg {
    let d = b.vreg(Cls::I);
    if i32::try_from(k).is_ok() {
        b.emit(Inst::ICmpI(cond, signed, d, a, k));
    } else {
        let k = b.iconst(k);
        b.emit(Inst::ICmp(cond, signed, d, a, k));
    }
    d
}

fn cmp(b: &mut UnitBuilder, cond: Cond, signed: bool, x: VReg, y: VReg) -> VReg {
    let d = b.vreg(Cls::I);
    b.emit(Inst::ICmp(cond, signed, d, x, y));
    d
}

fn select(b: &mut UnitBuilder, cond: VReg, yes: VReg, no: VReg) -> VReg {
    let d = b.vreg(Cls::I);
    b.emit(Inst::Sel(d, cond, yes, no));
    d
}

/// `a` sign-extended from 32 bits, for signed compares.
fn signed(b: &mut UnitBuilder, a: VReg) -> VReg {
    let d = b.vreg(Cls::I);
    b.emit(Inst::Conv(Conv::IntToInt(I32), d, a));
    d
}

fn bin(b: &mut UnitBuilder, op: IOp, x: VReg, y: VReg) -> VReg {
    let d = b.vreg(Cls::I);
    b.emit(Inst::IBin(op, U32, d, x, y));
    d
}

fn bin_k(b: &mut UnitBuilder, op: IOp, x: VReg, k: i64) -> VReg {
    let d = b.vreg(Cls::I);
    b.emit(Inst::IBinI(op, U32, d, x, k));
    d
}

fn in_high_word(b: &mut UnitBuilder, s: VReg) -> VReg {
    let d = b.vreg(Cls::I);
    b.emit(Inst::IBinI(IOp::Shl, U64, d, s, 32));
    d
}

fn low_word(b: &mut UnitBuilder, value: VReg) -> VReg {
    let d = b.vreg(Cls::I);
    b.emit(Inst::Conv(Conv::IntToInt(U32), d, value));
    d
}

/// Emits `i` (`len` bytes at `pc`, `inline` true) into the current block, which it may end and
/// replace (a side exit's test): the code after it goes into `b.current()`.
pub fn lower(b: &mut UnitBuilder, regs: &mut Regs, host: &mut dyn Host, i: &Insn, pc: u32, len: u8, literal: Option<u32>) -> Flow {
    use Op::*;
    let next = pc.wrapping_add(u32::from(len));
    let r = i.r;
    match i.op {
        Add | AddN | Sub | And | Or | Xor => {
            let (s, t) = (regs.ar(b, i.s), regs.ar(b, i.t));
            let op = match i.op { Add | AddN => IOp::Add, Sub => IOp::Sub, And => IOp::And, Or => IOp::Or, _ => IOp::Xor };
            let d = bin(b, op, s, t);
            regs.set_ar(r, d);
        }
        Addi | Addmi => {
            let s = regs.ar(b, i.s);
            let d = bin_k(b, IOp::Add, s, i64::from(i.imm));
            regs.set_ar(i.t, d);
        }
        AddiN => {
            let s = regs.ar(b, i.s);
            let d = bin_k(b, IOp::Add, s, i64::from(i.imm));
            regs.set_ar(r, d);
        }
        Addx2 | Addx4 | Addx8 | Subx2 | Subx4 | Subx8 => {
            let (s, t) = (regs.ar(b, i.s), regs.ar(b, i.t));
            let (op, shift) = match i.op {
                Addx2 => (IOp::Add, 1),
                Addx4 => (IOp::Add, 2),
                Addx8 => (IOp::Add, 3),
                Subx2 => (IOp::Sub, 1),
                Subx4 => (IOp::Sub, 2),
                _ => (IOp::Sub, 3),
            };
            let scaled = bin_k(b, IOp::Shl, s, shift);
            let d = bin(b, op, scaled, t);
            regs.set_ar(r, d);
        }
        Neg | Abs => {
            let t = regs.ar(b, i.t);
            let neg = b.vreg(Cls::I);
            b.emit(Inst::INeg(U32, neg, t));
            let d = if i.op == Abs {
                let signed_t = signed(b, t);
                let negative = cmp_k(b, Cond::Lt, true, signed_t, 0);
                select(b, negative, neg, t)
            } else {
                neg
            };
            regs.set_ar(r, d);
        }
        Salt | Saltu | Min | Max | Minu | Maxu => {
            let (s, t) = (regs.ar(b, i.s), regs.ar(b, i.t));
            let less = if matches!(i.op, Salt | Min | Max) {
                let (x, y) = (signed(b, s), signed(b, t));
                cmp(b, Cond::Lt, true, x, y)
            } else {
                cmp(b, Cond::Lt, false, s, t)
            };
            let d = match i.op {
                Min | Minu => select(b, less, s, t),
                Max | Maxu => select(b, less, t, s),
                _ => less,
            };
            regs.set_ar(r, d);
        }
        Moveqz | Movnez | Movltz | Movgez => {
            let (s, t) = (regs.ar(b, i.s), regs.ar(b, i.t));
            let old = regs.ar(b, r);
            let take = match i.op {
                Moveqz => cmp_k(b, Cond::Eq, false, t, 0),
                Movnez => cmp_k(b, Cond::Ne, false, t, 0),
                _ => {
                    let t = signed(b, t);
                    cmp_k(b, if i.op == Movltz { Cond::Lt } else { Cond::Ge }, true, t, 0)
                }
            };
            let d = select(b, take, s, old);
            regs.set_ar(r, d);
        }
        Mull => {
            let (s, t) = (regs.ar(b, i.s), regs.ar(b, i.t));
            let d = bin(b, IOp::Mul, s, t);
            regs.set_ar(r, d);
        }
        Muluh | Mulsh => {
            let (s, t) = (regs.ar(b, i.s), regs.ar(b, i.t));
            let (ty, x, y) = if i.op == Mulsh {
                (I64, signed(b, s), signed(b, t))
            } else {
                (U64, s, t)
            };
            let product = b.vreg(Cls::I);
            b.emit(Inst::IBin(IOp::Mul, ty, product, x, y));
            let high = b.vreg(Cls::I);
            b.emit(Inst::IBinI(IOp::Shr, ty, high, product, 32));
            let d = if i.op == Mulsh {
                let word = b.vreg(Cls::I);
                b.emit(Inst::Conv(Conv::IntToInt(U32), word, high));
                word
            } else {
                high
            };
            regs.set_ar(r, d);
        }
        Movi => {
            let d = b.iconst(i64::from(i.imm as u32));
            regs.set_ar(i.t, d);
        }
        MoviN => {
            let d = b.iconst(i64::from(i.imm as u32));
            regs.set_ar(i.s, d);
        }
        MovN => {
            let s = regs.ar(b, i.s);
            regs.set_ar(i.t, s);
        }
        Extui => {
            let t = regs.ar(b, i.t);
            let shifted = bin_k(b, IOp::Shr, t, i64::from(i.imm));
            let d = bin_k(b, IOp::And, shifted, (1i64 << (i.r + 1)) - 1);
            regs.set_ar(((i.bits >> 12) & 15) as u8, d);
        }
        Ssl | Ssr | Ssa8l | Ssa8b => {
            let s = regs.ar(b, i.s);
            let amount = if matches!(i.op, Ssa8l | Ssa8b) {
                let bytes = bin_k(b, IOp::And, s, 3);
                bin_k(b, IOp::Shl, bytes, 3)
            } else {
                bin_k(b, IOp::And, s, 31)
            };
            let sa = if matches!(i.op, Ssl | Ssa8b) {
                let width = b.iconst(32);
                bin(b, IOp::Sub, width, amount)
            } else {
                amount
            };
            regs.set_sar(sa);
        }
        Ssai => {
            let sa = b.iconst(i64::from(i.imm as u32));
            regs.set_sar(sa);
        }
        Slli => {
            let s = regs.ar(b, i.s);
            let high = in_high_word(b, s);
            let shifted = b.vreg(Cls::I);
            b.emit(Inst::IBinI(IOp::Shr, U64, shifted, high, i64::from(i.imm)));
            regs.set_ar(r, low_word(b, shifted));
        }
        Srli => {
            let t = regs.ar(b, i.t);
            let d = bin_k(b, IOp::Shr, t, i64::from(i.imm));
            regs.set_ar(r, d);
        }
        Srai => {
            let t = regs.ar(b, i.t);
            let t = signed(b, t);
            let shifted = b.vreg(Cls::I);
            b.emit(Inst::IBinI(IOp::Shr, I32, shifted, t, i64::from(i.imm)));
            regs.set_ar(r, low_word(b, shifted));
        }
        Sll | Srl | Sra | Src => {
            let sar = regs.sar(b);
            let sa = bin_k(b, IOp::And, sar, 63);
            let undefined = cmp_k(b, Cond::Gt, false, sa, 32);
            let side = host.side_exit(b, regs);
            let ok = b.block();
            b.branch(undefined, side, ok);
            b.switch_to(ok);

            let (ty, value) = match i.op {
                Sll | Src => {
                    let s = regs.ar(b, i.s);
                    let high = in_high_word(b, s);
                    let value = if i.op == Src {
                        let t = regs.ar(b, i.t);
                        let pair = b.vreg(Cls::I);
                        b.emit(Inst::IBin(IOp::Or, U64, pair, high, t));
                        pair
                    } else {
                        high
                    };
                    (U64, value)
                }
                Sra => {
                    let t = regs.ar(b, i.t);
                    (I64, signed(b, t))
                }
                _ => (U64, regs.ar(b, i.t)), // Srl
            };
            let shifted = b.vreg(Cls::I);
            b.emit(Inst::IBin(IOp::Shr, ty, shifted, value, sa));
            let d = if i.op == Srl { shifted } else { low_word(b, shifted) };
            regs.set_ar(r, d);
        }
        L32r => {
            let d = b.iconst(i64::from(literal.expect("inline L32R has its literal")));
            regs.set_ar(i.t, d);
        }
        L8ui | L16ui | L16si | L32i | L32iN => {
            let s = regs.ar(b, i.s);
            let addr = bin_k(b, IOp::Add, s, i64::from(i.imm));
            let (bytes, signed) = match i.op { L8ui => (1, false), L16ui => (2, false), L16si => (2, true), _ => (4, false) };
            let d = host.load(b, regs, addr, bytes, signed);
            regs.set_ar(i.t, d);
        }
        S8i | S16i | S32i | S32iN => {
            let (s, t) = (regs.ar(b, i.s), regs.ar(b, i.t));
            let addr = bin_k(b, IOp::Add, s, i64::from(i.imm));
            let bytes = match i.op { S8i => 1, S16i => 2, _ => 4 };
            host.store(b, regs, addr, bytes, t);
        }
        J => return Flow::Jump(target(i, pc).expect("J has a target")),
        Jx => return Flow::Indirect(regs.ar(b, i.s)),
        Ret | RetN => return Flow::Indirect(regs.ar(b, 0)),
        Call0 => {
            let d = b.iconst(i64::from(next));
            regs.set_ar(0, d);
            return Flow::Jump(target(i, pc).expect("CALL0 has a target"));
        }
        Callx0 => {
            // The target is read before a0 is written (CALLX0 a0 jumps to the old a0).
            let to = regs.ar(b, i.s);
            let d = b.iconst(i64::from(next));
            regs.set_ar(0, d);
            return Flow::Indirect(to);
        }
        Beq | Bne | Blt | Bge | Bltu | Bgeu => {
            let (s, t) = (regs.ar(b, i.s), regs.ar(b, i.t));
            let taken = match i.op {
                Beq => cmp(b, Cond::Eq, false, s, t),
                Bne => cmp(b, Cond::Ne, false, s, t),
                Bltu => cmp(b, Cond::Lt, false, s, t),
                Bgeu => cmp(b, Cond::Ge, false, s, t),
                _ => {
                    let (s, t) = (signed(b, s), signed(b, t));
                    cmp(b, if i.op == Blt { Cond::Lt } else { Cond::Ge }, true, s, t)
                }
            };
            return Flow::Branch { taken, target: target(i, pc).expect("a branch has a target") };
        }
        Beqz | Bnez | BeqzN | BnezN | Bgez | Bltz | Beqi | Bnei | Blti | Bgei | Bltui | Bgeui => {
            let s = regs.ar(b, i.s);
            let k = i64::from(i.imm as u32);
            let taken = match i.op {
                Beqz | BeqzN => cmp_k(b, Cond::Eq, false, s, 0),
                Bnez | BnezN => cmp_k(b, Cond::Ne, false, s, 0),
                Beqi => cmp_k(b, Cond::Eq, false, s, k),
                Bnei => cmp_k(b, Cond::Ne, false, s, k),
                Bltui => cmp_k(b, Cond::Lt, false, s, k),
                Bgeui => cmp_k(b, Cond::Ge, false, s, k),
                _ => {
                    let s = signed(b, s);
                    let (cond, k) = match i.op {
                        Bgez => (Cond::Ge, 0),
                        Bltz => (Cond::Lt, 0),
                        Blti => (Cond::Lt, i64::from(i.imm)),
                        _ => (Cond::Ge, i64::from(i.imm)),
                    };
                    cmp_k(b, cond, true, s, k)
                }
            };
            return Flow::Branch { taken, target: target(i, pc).expect("a branch has a target") };
        }
        Ball | Bany | Bnall | Bnone => {
            let (s, t) = (regs.ar(b, i.s), regs.ar(b, i.t));
            let masked = bin(b, IOp::And, s, t);
            let taken = match i.op {
                Ball => cmp(b, Cond::Eq, false, masked, t),
                Bnall => cmp(b, Cond::Ne, false, masked, t),
                Bnone => cmp_k(b, Cond::Eq, false, masked, 0),
                _ => masked,
            };
            return Flow::Branch { taken, target: target(i, pc).expect("a branch has a target") };
        }
        Bbc | Bbs | Bbci | Bbsi => {
            let s = regs.ar(b, i.s);
            let masked = if matches!(i.op, Bbc | Bbs) {
                let t = regs.ar(b, i.t);
                let bit = bin_k(b, IOp::And, t, 31);
                let shifted = bin(b, IOp::Shr, s, bit);
                bin_k(b, IOp::And, shifted, 1)
            } else {
                let bit = ((i.r & 1) << 4) | i.t;
                // IBinI takes a signed i32 immediate, including the mask for bit 31.
                bin_k(b, IOp::And, s, i64::from(1i32 << bit))
            };
            let taken = if matches!(i.op, Bbc | Bbci) { cmp_k(b, Cond::Eq, false, masked, 0) } else { masked };
            return Flow::Branch { taken, target: target(i, pc).expect("a branch has a target") };
        }
        op => unreachable!("{op:?} is not an inline form"),
    }
    Flow::Next
}

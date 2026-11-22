//! The inline lowering (`jit::lower`) against `Cpu::execute`: runs of random encodings of every
//! form in `jit::FORMS`, from random register states, compiled into units and compared field by
//! field with the interpreter, memory and side exits included. The test units reach one RAM
//! inline and leave by a side exit for every other access, as for an undefined shift.

use std::mem::offset_of;

use fab_elec_mcu_emu::bus::{Bus, Kind, Width};
use fab_elec_mcu_emu::jit::rir::{Cls, Cond, Conv, Inst, IOp, Mem, VReg, U32, U64};
use fab_elec_mcu_emu::jit::UnitBuilder;
use fab_elec_mcu_xtensa::insn::{target, Op};
use fab_elec_mcu_xtensa::jit::{ends_run, inline, leaves, lower, Flow, Host, Regs, FORMS};
use fab_elec_mcu_xtensa::sim::{Config, Cpu, Decoded, IntType, Unaligned, Vectors};

const FLASH: u32 = 0x4200_0000;
/// The RAM the test units load and store inline.
const RAM: u32 = 0x3fc8_8000;
const RAM_LEN: usize = 4096;
/// A unit's code for "left before an instruction".
const SIDE: u32 = 2;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize { (self.next() % n as u64) as usize }
    /// Register values that reach every compare's edges: random words, small values, signs,
    /// and addresses in and just around the RAM.
    fn value(&mut self) -> u32 {
        const EDGES: [u32; 8] = [0, 1, 2, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 0xffff_fff0, 0x4200_0100];
        match self.below(5) {
            0 => EDGES[self.below(EDGES.len())],
            1 => self.below(40) as u32,
            2 | 3 => (RAM - 16).wrapping_add(self.below(RAM_LEN + 32) as u32),
            _ => self.next() as u32,
        }
    }
}

/// The word an L32R loads from `addr` (outside the RAM).
fn literal(addr: u32) -> u32 { addr.wrapping_mul(0x9e37_79b9) ^ 0x5a5a_5a5a }

/// The interpreter's bus: the RAM, and a literal word for any other word load. `outside` notes
/// an access the units leave for (any outside the RAM).
struct Memory {
    ram: Vec<u8>,
    outside: bool,
}

impl Memory {
    fn at(&self, addr: u32, width: Width) -> Option<usize> {
        let n = addr.wrapping_sub(RAM) as usize;
        (n + width.bytes() <= RAM_LEN).then_some(n)
    }
}

impl Bus for Memory {
    fn read(&mut self, addr: u32, width: Width, kind: Kind, _now: u64) -> Result<u32, ()> {
        if let Some(n) = self.at(addr, width) {
            return Ok((0..width.bytes()).fold(0, |v, k| v | u32::from(self.ram[n + k]) << (8 * k)));
        }
        self.outside = true;
        if width == Width::W && kind == Kind::Load { Ok(literal(addr)) } else { Err(()) }
    }
    fn write(&mut self, addr: u32, width: Width, value: u32, _now: u64) -> Result<(), ()> {
        let Some(n) = self.at(addr, width) else { self.outside = true; return Err(()) };
        for k in 0..width.bytes() { self.ram[n + k] = (value >> (8 * k)) as u8; }
        Ok(())
    }
}

fn cpu() -> Cpu {
    let config = Config {
        mul32_high: true, div32: true, unaligned: Unaligned::Hardware, excm_level: 3,
        mac16: true, s32c1i: true, threadptr: true, vecbase_reset: 0x4037_8000,
        vectors: Vectors { kernel: 0x300, user: 0x340, window: [0, 0x40, 0x80, 0xc0, 0x100, 0x140] },
        int_type: [IntType::None; 32], int_level: [0; 32], prid: None,
    };
    let mut cpu = Cpu::new(config, FLASH);
    cpu.ps = 0;
    cpu
}

/// The test units' host: side exits store what the run holds, the PC of the instruction left
/// before and the retirements before it; loads and stores inside the RAM (at `ram`) run inline.
struct Exits {
    pc: u32,
    done: u64,
    ram: u64,
}

impl Exits {
    /// The RAM address of an access of `bytes` at `addr`, the current block going on with it;
    /// any other access leaves by a side exit.
    fn in_ram(&mut self, b: &mut UnitBuilder, regs: &Regs, addr: VReg, bytes: u8) -> VReg {
        let n = b.vreg(Cls::I);
        b.emit(Inst::IBinI(IOp::Sub, U32, n, addr, i64::from(RAM)));
        let fits = b.vreg(Cls::I);
        b.emit(Inst::ICmpI(Cond::Le, false, fits, n, (RAM_LEN - usize::from(bytes)) as i64));
        let side = self.side_exit(b, regs);
        let on = b.block();
        b.branch(fits, on, side);
        b.switch_to(on);
        let ram = b.iconst(self.ram as i64);
        let at = b.vreg(Cls::I);
        b.emit(Inst::IBin(IOp::Add, U64, at, ram, n));
        at
    }
}

impl Host for Exits {
    fn side_exit(&mut self, b: &mut UnitBuilder, regs: &Regs) -> u32 {
        let (back, side) = (b.current(), b.block());
        b.switch_to(side);
        regs.store(b);
        finish(b, self.pc, self.done, None);
        ret(b, SIDE);
        b.switch_to(back);
        side
    }
    fn load(&mut self, b: &mut UnitBuilder, regs: &Regs, addr: VReg, bytes: u8, signed: bool) -> VReg {
        let at = self.in_ram(b, regs, addr, bytes);
        let v = b.vreg(Cls::I);
        b.emit(Inst::Load(Mem::Int(bytes, signed), v, at, 0));
        if !signed { return v; }
        let word = b.vreg(Cls::I);
        b.emit(Inst::Conv(Conv::IntToInt(U32), word, v));
        word
    }
    fn store(&mut self, b: &mut UnitBuilder, regs: &Regs, addr: VReg, bytes: u8, value: VReg) {
        let at = self.in_ram(b, regs, addr, bytes);
        b.emit(Inst::Store(Mem::Int(bytes, false), at, 0, value));
    }
}

/// Stores `pc` (or the register in `to`) and adds `retired` to INSTRET.
fn finish(b: &mut UnitBuilder, pc: u32, retired: u64, to: Option<VReg>) {
    let ctx = b.ctx();
    let count = b.vreg(Cls::I);
    b.emit(Inst::Load(Mem::Int(8, false), count, ctx, offset_of!(Cpu, instret) as i32));
    let added = b.vreg(Cls::I);
    b.emit(Inst::IBinI(IOp::Add, U64, added, count, retired as i64));
    b.emit(Inst::Store(Mem::Int(8, false), ctx, offset_of!(Cpu, instret) as i32, added));
    let pc = to.unwrap_or_else(|| b.iconst(i64::from(pc)));
    b.emit(Inst::Store(Mem::Int(4, false), ctx, offset_of!(Cpu, pc) as i32, pc));
}

fn ret(b: &mut UnitBuilder, code: u32) {
    let v = b.iconst(i64::from(code));
    b.ret(v);
}

struct Run {
    pc: u32,
    insns: Vec<Decoded>,
    start: Cpu,
}

#[test]
fn inline_forms_match_the_interpreter() {
    let base = cpu();
    let mut rng = Rng(0x5eed_1e55_0bad_cafe);
    let start_ram: Vec<u8> = (0..RAM_LEN).map(|_| rng.next() as u8).collect();
    let mut native_ram = start_ram.clone();
    let ram = native_ram.as_mut_ptr() as u64;
    // Every low half-word (all of op0, t, s and r, so every narrow encoding and every
    // ST0-group word with a zero high byte), each with a zero and three random high bytes.
    // Then every high byte (op1 and op2) with every op0, r and t and a random s, for the RRR
    // forms whose opcode lies in all of them (SSL, SLL and the like).
    let mut pool: Vec<Vec<Decoded>> = vec![Vec::new(); FORMS.len()];
    let add = |pool: &mut Vec<Vec<Decoded>>, word: u32| {
        let Some(d) = base.decode_lane(word) else { return };
        let Some(form) = FORMS.iter().position(|&op| op == d.insn.op) else { return };
        if d.class == 0 && pool[form].len() < 64 && !pool[form].contains(&d) {
            pool[form].push(d);
        }
    };
    for low in 0..0x1_0000u32 {
        for n in 0..4 {
            let high = if n == 0 { 0 } else { rng.next() as u32 & 0xff };
            add(&mut pool, high << 16 | low);
        }
    }
    for opcode in 0..0x1_0000u32 {
        let (high, r, op0) = (opcode >> 8, (opcode >> 4) & 15, opcode & 15);
        for t in 0..16 {
            let s = rng.next() as u32 & 15;
            add(&mut pool, high << 16 | r << 12 | s << 8 | t << 4 | op0);
        }
    }
    for (form, found) in FORMS.iter().zip(&pool) {
        // RET has one encoding; most forms have dozens here.
        assert!(!found.is_empty(), "{form:?}: no encoding of lane class 0 found");
    }
    let straight: Vec<usize> = (0..FORMS.len()).filter(|&f| !ends_run(&pool[f][0].insn)).collect();
    let pick = |rng: &mut Rng, form: usize| pool[form][rng.below(pool[form].len())];
    // Runs: each form in turn, at a random place in a straight line of up to six, a control
    // form last and the rest straight-line forms.
    let mut runs = Vec::new();
    for k in 0..FORMS.len() * 24 {
        let form = k % FORMS.len();
        let mut insns: Vec<Decoded> = (0..rng.below(6)).map(|_| {
            let f = straight[rng.below(straight.len())];
            pick(&mut rng, f)
        }).collect();
        let d = pick(&mut rng, form);
        if ends_run(&d.insn) { insns.push(d) } else { let at = rng.below(insns.len() + 1); insns.insert(at, d) }
        if !ends_run(&insns[insns.len() - 1].insn) && rng.below(2) == 0 {
            let f = rng.below(FORMS.len());
            let d = pick(&mut rng, f);
            if ends_run(&d.insn) { insns.push(d); }
        }
        let mut start = base.clone();
        for a in &mut start.ar { *a = rng.value(); }
        start.sar = rng.below(41) as u32;
        // Most accesses aim at the RAM (some just below it), whatever their offset.
        for d in &insns {
            if leaves(&d.insn) && rng.below(4) != 0 {
                start.ar[usize::from(d.insn.s)] = (RAM - 8).wrapping_add(rng.below(RAM_LEN) as u32).wrapping_sub(d.insn.imm as u32);
            }
        }
        runs.push(Run { pc: FLASH + 0x1000 + 64 * k as u32, insns, start });
    }
    let mut compared = vec![0u32; FORMS.len()];
    // Retirements of forms that may leave (an access outside the RAM, an undefined shift).
    let mut stayed = vec![0u32; FORMS.len()];
    for chunk in runs.chunks(128) {
        let mut b = UnitBuilder::new("xtensa_lowering_test");
        let ctx = b.ctx();
        for run in chunk {
            let head = b.block();
            b.switch_to(head);
            b.entry(run.pc, head);
            let mut regs = Regs::new(ctx, 0);
            let mut pc = run.pc;
            for (j, d) in run.insns.iter().enumerate() {
                let lit = (d.insn.op == Op::L32r).then(|| literal(target(&d.insn, pc).unwrap()));
                assert!(inline(&d.insn, lit), "{:?} is a form", d.insn.op);
                let mut exits = Exits { pc, done: j as u64, ram };
                let flow = lower(&mut b, &mut regs, &mut exits, &d.insn, pc, d.len, lit);
                let next = pc.wrapping_add(u32::from(d.len));
                if j + 1 < run.insns.len() {
                    assert!(matches!(flow, Flow::Next), "{:?} goes on", d.insn.op);
                    pc = next;
                    continue;
                }
                regs.flush(&mut b);
                let retired = run.insns.len() as u64;
                match flow {
                    Flow::Next => { finish(&mut b, next, retired, None); ret(&mut b, 0); }
                    Flow::Jump(to) => { finish(&mut b, to, retired, None); ret(&mut b, 0); }
                    Flow::Indirect(to) => { finish(&mut b, 0, retired, Some(to)); ret(&mut b, 0); }
                    Flow::Branch { taken, target } => {
                        let (yes, no) = (b.block(), b.block());
                        b.branch(taken, yes, no);
                        b.switch_to(yes);
                        finish(&mut b, target, retired, None);
                        ret(&mut b, 0);
                        b.switch_to(no);
                        finish(&mut b, next, retired, None);
                        ret(&mut b, 0);
                    }
                }
            }
        }
        let (unit, _) = b.finish(99).expect("the host compiles and maps a unit");
        for run in chunk {
            let mut reference = run.start.clone();
            reference.pc = run.pc;
            let mut memory = Memory { ram: start_ram.clone(), outside: false };
            let mut expected = 0;
            for d in &run.insns {
                let before = reference.clone();
                memory.outside = false;
                let retired = reference.execute(&d.insn, u32::from(d.len), &mut memory) == Ok(true);
                let form = FORMS.iter().position(|&op| op == d.insn.op).unwrap();
                if !retired || leaves(&d.insn) && memory.outside {
                    // A faulting access leaves the RAM as it was.
                    reference = before;
                    expected = SIDE;
                    break;
                }
                compared[form] += 1;
                if leaves(&d.insn) { stayed[form] += 1; }
            }
            let mut native = run.start.clone();
            native.pc = run.pc;
            native_ram.copy_from_slice(&start_ram);
            // SAFETY: the unit's code touches only the Cpu it is given and the RAM at `ram`
            // (native_ram, which stays where it is), and `run.pc` is an entry.
            let code = unsafe { unit.run(&mut native as *mut Cpu as *mut u8, run.pc) };
            let listing: Vec<String> = run.insns.iter().map(|d| format!("{:?} {:06x}", d.insn.op, d.insn.bits)).collect();
            assert_eq!(code, expected, "exit code of {listing:?} from {:x?}", run.start.ar);
            if let Some(field) = native.same_state(&reference) {
                panic!("{field} differs after {listing:?} from ar {:x?} sar {}: native ar {:x?} pc {:x}, interpreter ar {:x?} pc {:x}",
                    run.start.ar, run.start.sar, native.ar, native.pc, reference.ar, reference.pc);
            }
            if let Some(n) = (0..RAM_LEN).find(|&n| native_ram[n] != memory.ram[n]) {
                panic!("RAM byte {n:#x} differs after {listing:?} from ar {:x?}: native {:#x}, interpreter {:#x}",
                    run.start.ar, native_ram[n], memory.ram[n]);
            }
        }
    }
    for (k, form) in FORMS.iter().enumerate() {
        assert!(compared[k] >= 12, "{form:?}: compared only {} times", compared[k]);
        assert!(!leaves(&pool[k][0].insn) || stayed[k] >= 6, "{form:?}: ran inline only {} times", stayed[k]);
    }
}

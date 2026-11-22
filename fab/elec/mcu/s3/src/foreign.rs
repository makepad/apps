//! Instructions supplied by a part at immutable ranges of guest PCs.

use std::ops::Range;

use fab_elec_mcu_emu::bus::{Bus, Kind, Width};
use fab_elec_mcu_xtensa::insn::{decode, Insn};
use fab_elec_mcu_xtensa::sim::{Cpu, Stop};

use crate::bus::{SRAM_DATA, SRAM_LEN};

/// A part can use the ordinary data bus. Inspection of a suspended frame is
/// side-effect free, and frame stores take the ordinary SRAM journal/DMA path.
pub trait ForeignBus: Bus {
    fn sram_word(&self, address: u32) -> Option<u32>;
    fn writable_frame(&self, address: u32, bytes: usize) -> bool;
    /// Register `reg` of analog I2C block `block` through host `host`, as the ROM's
    /// rom_i2c_readReg reads it: its byte, or why the model does not serve it. Side-effect free.
    fn regi2c(&self, block: u8, host: u8, reg: u8) -> Result<u8, &'static str>;
    /// Whether the ROM's rom_i2c_writeReg of `data` to that register is one the analog model takes, or
    /// why not. Side-effect free.
    fn regi2c_writable(&self, block: u8, host: u8, reg: u8, data: u8) -> Result<(), &'static str>;
    /// The write `regi2c_writable` allowed: the register reads `data` from now on.
    fn regi2c_set(&mut self, block: u8, reg: u8, data: u8);
    /// A stand-in's model choice, noted once a run at the current tick.
    fn note(&mut self, what: &'static str);
}

impl ForeignBus for crate::bus::S3Bus {
    fn sram_word(&self, address: u32) -> Option<u32> {
        self.sram_load(address, Width::W)
    }

    fn writable_frame(&self, address: u32, bytes: usize) -> bool {
        let offset = address.wrapping_sub(SRAM_DATA) as usize;
        address & 3 == 0 && offset < SRAM_LEN && bytes <= SRAM_LEN - offset
            && !self.periph.dma_in_flight(offset, bytes)
    }

    fn regi2c(&self, block: u8, host: u8, reg: u8) -> Result<u8, &'static str> {
        self.periph.regi2c(block, host, reg)
    }

    fn regi2c_writable(&self, block: u8, host: u8, reg: u8, data: u8) -> Result<(), &'static str> {
        self.periph.regi2c_writable(block, host, reg, data)
    }

    fn regi2c_set(&mut self, block: u8, reg: u8, data: u8) {
        self.periph.regi2c_set(block, reg, data)
    }

    fn note(&mut self, what: &'static str) {
        self.periph.note_once(self.tick, what);
    }
}

/// The address classification is immutable for the lifetime of the table.
/// A stop's short static category travels in the machine halt. The detailed
/// explanation is computed on demand from that same PC, CPU and SRAM.
pub trait ForeignCode: Send + Sync {
    fn contains(&self, pc: u32) -> bool;
    fn at(&self, pc: u32, cpu: &mut Cpu, bus: &mut dyn ForeignBus) -> Result<(Insn, u32), Stop>;
    fn explain(&self, pc: u32, cpu: &Cpu, bus: &dyn ForeignBus) -> Option<String>;
}

pub enum Next {
    /// One preemptible body instruction per cycle. The count must be positive.
    Burn(u32),
    /// Changes the machine once, where this step commits (after every check), then one body
    /// instruction as `Burn(1)`: the next decision sees the change.
    Effect(fn(&Ctx, &mut dyn ForeignBus)),
    Call { target_at: u32, args: [u32; 6] },
    Return(u32),
    Stop(&'static str),
}

pub struct Ctx<'a> {
    pub step: u32,
    pub args: [u32; 6],
    /// The most recent CALL8 result, retained through intervening burns.
    pub result: Option<u32>,
    /// The table's immutable words (`VirtualFns::with_env`): the same for every function of
    /// the table, every step and every clone. Their meaning belongs to the part that made them.
    pub env: &'a [u32],
}

pub struct VirtualFn {
    pub name: String,
    pub address: u32,
    pub bytes: u32,
    pub frame: u32,
    /// A pure decision on a side-effect-free view of the machine. Window retries and stop
    /// explanations may ask again.
    pub body: fn(&Ctx, &dyn ForeignBus) -> Next,
}

/// Five-instruction stand-ins entered through the Xtensa windowed calling convention.
pub struct VirtualFns {
    ranges: Vec<Range<u32>>,
    functions: Vec<VirtualFn>,
    env: Vec<u32>,
}

const UNKNOWN: &str = "foreign code with no stand-in";
const CANARY: &str = "foreign activation canary";
const RULE: &str = "foreign body rule";
const FRAME: &str = "foreign activation frame";
const TARGET: &str = "foreign call target";

// For SP=S the top 32 bytes below the predecessor SP are the ABI base
// and extra save areas. Leave the outgoing area at S alone as well.
const RECORD: u32 = 32;
const WORDS: usize = 17;
/// The smallest ENTRY frame a stand-in takes.
pub(crate) const MIN_FRAME: u32 = (RECORD + WORDS as u32 * 4 + 32 + 15) & !15;
const STEP: usize = 0;
const LINK: usize = 1;
const SP: usize = 2;
const ARGS: usize = 3;
const REMAINING: usize = 9;
const RESULT: usize = 10;
const FLAGS: usize = 11; // result-valid bit and the pre-burn PS.CALLINC bits
const CALLEE: usize = 12;
const LOCALS: usize = 13;
const CALLINC: u32 = 3 << 16;
const RESULT_VALID: u32 = 1;

impl VirtualFns {
    pub fn new(mut ranges: Vec<Range<u32>>, mut functions: Vec<VirtualFn>) -> Result<Self, String> {
        ranges.sort_by_key(|r| r.start);
        if ranges.iter().any(|r| r.start >= r.end) || ranges.windows(2).any(|w| w[0].end > w[1].start) {
            return Err("foreign ranges must be nonempty and disjoint".into());
        }
        functions.sort_by_key(|f| f.address);
        let mut previous_end = 0;
        for f in &functions {
            if f.bytes < 15 { return Err(format!("{} needs 15 bytes, has {}", f.name, f.bytes)); }
            let end = f.address.checked_add(f.bytes).ok_or_else(|| format!("{} wraps", f.name))?;
            if previous_end > f.address || !ranges.iter().any(|r| r.start <= f.address && end <= r.end) {
                return Err(format!("{} overlaps another symbol or exceeds its foreign range", f.name));
            }
            if f.frame < MIN_FRAME || f.frame > 32760 || f.frame % 16 != 0 {
                return Err(format!("{} needs an aligned ENTRY frame of {MIN_FRAME}..32760 bytes", f.name));
            }
            previous_end = end;
        }
        Ok(Self { ranges, functions, env: Vec::new() })
    }

    /// Words every body reads as `Ctx::env`, such as the addresses of one image's objects.
    /// A table built for one image carries that image's words, so machines of different images
    /// in one process never share them.
    pub fn with_env(mut self, env: Vec<u32>) -> Self {
        self.env = env;
        self
    }

    fn symbol(&self, pc: u32) -> Option<&VirtualFn> {
        let n = self.functions.partition_point(|f| f.address <= pc);
        n.checked_sub(1).map(|i| &self.functions[i]).filter(|f| pc - f.address < f.bytes)
    }
}

// These words are our stand-in instructions. No guest instruction memory
// is involved; the decoder gives the same operands/bits as a planted word.
fn instruction(bits: u32) -> (Insn, u32) {
    decode(bits).expect("valid stand-in instruction")
}

fn jump(pc: u32, target: u32) -> (Insn, u32) {
    instruction(6 | (target.wrapping_sub(pc.wrapping_add(4)) & 0x3ffff) << 6)
}

fn pattern(sp: u32, step: u32, index: usize) -> u32 {
    // Two independent preserved registers locate the record if a gate
    // breaks a1. The record itself still checks both of these registers.
    if index == 2 { return sp ^ step.rotate_left(11); }
    if index == 3 { return step ^ (sp ^ step.rotate_left(11)).rotate_left(7); }
    sp.rotate_left(11) ^ step.wrapping_mul(0x9e37_79b9) ^ (index as u32 + 1).wrapping_mul(0x85eb_ca6b)
}

const A: [&str; 16] = ["a0","a1","a2","a3","a4","a5","a6","a7","a8","a9","a10","a11","a12","a13","a14","a15"];
const FP: [&str; 16] = ["f0","f1","f2","f3","f4","f5","f6","f7","f8","f9","f10","f11","f12","f13","f14","f15"];
const SPECIAL: [&str; 7] = ["SAR","LBEG","LEND","LCOUNT","BR","FCR","FSR"];

// LEND is outside both executable windows, including the body J's
// sequential successor. Mask each special register as the ISA writes it.
fn specials(sp: u32, step: u32, function: u32) -> [u32; 7] {
    let p = |i: usize| pattern(sp, step, 32 + i);
    let mut lend = p(2) | 0x8000_0000;
    if lend == function + 6 || lend == function + 12 { lend ^= 0x4000_0000; }
    [p(0) & 63, p(1), lend, p(3) | 1, p(4) & 0xffff, p(5) & 0x7f, p(6) & 0xf80]
}

type Record = [u32; WORDS];

fn record(bus: &dyn ForeignBus, sp: u32) -> Option<Record> {
    let at = sp.checked_add(RECORD)?;
    let mut words = [0; WORDS];
    for (i, w) in words.iter_mut().enumerate() {
        *w = bus.sram_word(at.checked_add(i as u32 * 4)?)?;
    }
    Some(words)
}

fn mismatch(cpu: &Cpu, r: &Record, burning: bool, function: u32) -> Option<&'static str> {
    let sp = r[SP];
    if cpu.ar[1] != sp { return Some("a1"); }
    if cpu.ar[0] != r[LINK] { return Some("a0"); }
    let step = r[STEP].wrapping_add(r[REMAINING]);
    let end = if burning { 16 } else { 8 };
    for i in 2..end {
        if cpu.ar[i] != pattern(sp, step, i) { return Some(A[i]); }
    }
    for (i, word) in r[LOCALS..].iter().enumerate() {
        if *word != pattern(sp, step, 48 + i) { return Some(["local0","local1","local2","local3"][i]); }
    }
    if burning {
        for i in 0..16 {
            if cpu.fr[i] != pattern(sp, step, 16 + i) { return Some(FP[i]); }
        }
        let actual = [cpu.sar, cpu.lbeg, cpu.lend, cpu.lcount, cpu.br.into(), cpu.fcr, cpu.fsr];
        let expected = specials(sp, step, function);
        for i in 0..actual.len() {
            if actual[i] != expected[i] { return Some(SPECIAL[i]); }
        }
    }
    None
}

fn fill(cpu: &mut Cpu, r: &mut Record, burning: bool, function: u32) {
    let (sp, step) = (r[SP], r[STEP].wrapping_add(r[REMAINING]));
    let end = if burning { 16 } else { 8 };
    for i in 2..end { cpu.ar[i] = pattern(sp, step, i); }
    for (i, word) in r[LOCALS..].iter_mut().enumerate() { *word = pattern(sp, step, 48 + i); }
    if burning {
        for i in 0..16 { cpu.fr[i] = pattern(sp, step, 16 + i); }
        let [sar, lbeg, lend, lcount, br, fcr, fsr] = specials(sp, step, function);
        (cpu.sar, cpu.lbeg, cpu.lend, cpu.lcount) = (sar, lbeg, lend, lcount);
        (cpu.br, cpu.fcr, cpu.fsr) = (br as u16, fcr, fsr);
        cpu.fsr_underflow_unknown = false;
    }
}

fn context<'a>(r: &Record, env: &'a [u32]) -> Ctx<'a> {
    Ctx { args: r[ARGS..ARGS+6].try_into().unwrap(), step: r[STEP],
        result: (r[FLAGS] & RESULT_VALID != 0).then_some(r[RESULT]), env }
}

// A windowed call sets a nonzero PS.CALLINC before ENTRY. Mark only the
// self-jumps of a burn with zero, restoring CALLINC on leaving that burn.
// Thus a new ENTRY can reuse arbitrary (even abandoned) stack contents.
fn burning(pc: u32, f: &VirtualFn, cpu: &Cpu) -> bool {
    pc == f.address + 3 && cpu.ps & CALLINC == 0
}

fn suspended_record(bus: &dyn ForeignBus, cpu: &Cpu) -> Option<Record> {
    let witness = cpu.ar[3] ^ cpu.ar[2].rotate_left(7);
    let sp = cpu.ar[2] ^ witness.rotate_left(11);
    if sp != cpu.ar[1] {
        if let Some(r) = record(bus, sp) {
            if r[SP] == sp && r[STEP].wrapping_add(r[REMAINING]) == witness { return Some(r); }
        }
    }
    record(bus, cpu.ar[1])
}

fn resume(pc: u32, f: &VirtualFn, cpu: &Cpu, bus: &dyn ForeignBus) -> Result<Record, &'static str> {
    let burn = burning(pc, f, cpu);
    if !burn && pc == f.address + 3 {
        let mut r = [0; WORDS];
        r[LINK] = cpu.ar[0];
        r[SP] = cpu.ar[1];
        r[ARGS..ARGS+6].copy_from_slice(&cpu.ar[2..8]);
        return Ok(r);
    }
    let mut r = suspended_record(bus, cpu).ok_or(FRAME)?;
    if mismatch(cpu, &r, burn, f.address).is_some() { return Err(CANARY); }
    if burn {
        r[REMAINING] = r[REMAINING].checked_sub(1).ok_or(FRAME)?;
    } else {
        r[RESULT] = cpu.ar[10];
        r[FLAGS] |= RESULT_VALID;
    }
    Ok(r)
}

impl ForeignCode for VirtualFns {
    fn contains(&self, pc: u32) -> bool {
        let n = self.ranges.partition_point(|r| r.end <= pc);
        self.ranges.get(n).is_some_and(|r| r.start <= pc)
    }

    fn at(&self, pc: u32, cpu: &mut Cpu, bus: &mut dyn ForeignBus) -> Result<(Insn, u32), Stop> {
        let stop = |what| Stop::Unmodeled { pc, what };
        let f = self.symbol(pc).ok_or_else(|| stop(UNKNOWN))?;
        let offset = pc - f.address;
        match offset {
            0 => return Ok(instruction(0x136 | (f.frame / 8) << 12)), // ENTRY a1, frame
            6 => return Ok(instruction(0x8e0)), // CALLX8 a8
            12 => return Ok(instruction(0x90)), // RETW
            3 | 9 => {}
            _ => return Err(stop(UNKNOWN)),
        }
        // A pending loop-back stops in finish_step before any body effects.
        let retry = jump(pc, pc);
        if cpu.ps & (1 << 4) == 0 && cpu.lcount != 0 && cpu.lend == pc + 3 { return Ok(retry); }
        let burn = burning(pc, f, cpu);
        let highest = if burn { 15 } else if offset == 9 { 10 } else { 7 };
        if cpu.window_check_reg(highest) { return Ok(retry); }
        let mut r = resume(pc, f, cpu, bus).map_err(stop)?;
        let continuing = r[REMAINING] != 0;
        let ctx = context(&r, &self.env);
        let next = if continuing { Next::Burn(r[REMAINING]) } else { (f.body)(&ctx, &*bus) };
        if matches!(next, Next::Call { .. } | Next::Burn(_) | Next::Effect(_)) && highest < 15 && cpu.window_check_reg(15) {
            return Ok(retry);
        }
        if let Next::Stop(_) = next { return Err(stop(RULE)); }
        let (effect, next) = match next { Next::Effect(effect) => (Some(effect), Next::Burn(1)), next => (None, next) };
        let sp = cpu.ar[1];
        let at = sp.checked_add(RECORD).ok_or_else(|| stop(FRAME))?;
        if sp & 15 != 0 || !bus.writable_frame(at, WORDS * 4) { return Err(stop(FRAME)); }
        if !continuing { r[STEP] = r[STEP].wrapping_add(1); }
        let callinc = if burn { r[FLAGS] & CALLINC } else { cpu.ps & CALLINC };
        let destination = match next {
            Next::Call { target_at, args } => {
                if target_at & 3 != 0 { return Err(stop(TARGET)); }
                let target = bus.read(target_at, Width::W, Kind::Load, cpu.instret).map_err(|_| stop(TARGET))?;
                r[CALLEE] = target;
                fill(cpu, &mut r, false, f.address);
                cpu.ar[10..16].copy_from_slice(&args);
                cpu.ar[8] = target;
                cpu.ps = (cpu.ps & !CALLINC) | callinc;
                6
            }
            Next::Burn(cycles) => {
                if cycles == 0 { return Err(stop(RULE)); }
                r[FLAGS] = (r[FLAGS] & RESULT_VALID) | callinc;
                r[REMAINING] = cycles;
                fill(cpu, &mut r, true, f.address);
                cpu.ps &= !CALLINC;
                3
            }
            Next::Return(value) => {
                cpu.ar[2] = value;
                cpu.ps = (cpu.ps & !CALLINC) | callinc;
                12
            }
            Next::Effect(_) | Next::Stop(_) => unreachable!(),
        };
        if let Some(effect) = effect { effect(&ctx, bus); }
        // Preflight covered every word. No device event runs during one body step.
        for (i, word) in r.iter().enumerate() {
            bus.write(at + i as u32 * 4, Width::W, *word, cpu.instret).map_err(|_| stop(FRAME))?;
        }
        Ok(jump(pc, f.address + destination))
    }

    fn explain(&self, pc: u32, cpu: &Cpu, bus: &dyn ForeignBus) -> Option<String> {
        if !self.contains(pc) || !cpu.will_execute() { return None; }
        let Some(f) = self.symbol(pc) else { return Some(format!("{UNKNOWN} at {pc:#010x}")); };
        let offset = pc - f.address;
        if !matches!(offset, 0 | 3 | 6 | 9 | 12) {
            return Some(format!("{UNKNOWN} at {}+{offset:#x}", f.name));
        }
        if cpu.ps & (1 << 4) == 0 && cpu.lcount != 0 && cpu.lend == pc + 3 {
            return Some(format!("{}+{offset:#x} reaches LEND with LCOUNT={}", f.name, cpu.lcount));
        }
        if !matches!(offset, 3 | 9) { return None; }
        let burn = burning(pc, f, cpu);
        if burn || offset == 9 {
            if let Some(r) = suspended_record(bus, cpu) {
                if let Some(reg) = mismatch(cpu, &r, burn, f.address) {
                    let across = if burn { "burn".into() } else if let Some(callee) = self.symbol(r[CALLEE]) {
                        callee.name.clone()
                    } else { format!("CALLX8 to {:#010x}", r[CALLEE]) };
                    return Some(format!("{reg} changed across {across} in {} at step {}", f.name, r[STEP].wrapping_sub(1)));
                }
            }
        }
        let r = match resume(pc, f, cpu, bus) {
            Ok(r) => r,
            Err(_) => return Some(format!("{}+{offset:#x} cannot recover its frame at {:#010x}", f.name, cpu.ar[1])),
        };
        let next = if r[REMAINING] != 0 { Next::Burn(r[REMAINING]) } else { (f.body)(&context(&r, &self.env), bus) };
        let sp = cpu.ar[1];
        let detail = if let Next::Stop(why) = next {
            why.to_owned()
        } else if sp & 15 != 0 || !sp.checked_add(RECORD).is_some_and(|at| bus.writable_frame(at, WORDS * 4)) {
            format!("frame at {sp:#010x} needs an aligned, writable SRAM record")
        } else {
            match next {
                Next::Burn(0) => "burn needs a positive cycle count".into(),
                Next::Call { target_at, .. } => format!("cannot load the call target word at {target_at:#010x}"),
                _ => return None,
            }
        };
        Some(format!("{}+{offset:#x}, step {}: {detail}", f.name, r[STEP]))
    }
}

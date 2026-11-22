//! Allocation-free execution. The caller owns all persistent run state.
use crate::{fmath, format::*, module::{Entry, Func, Module}, ports};

/// Observes definitions and writes without changing execution. Bits retain the
/// value's runtime representation (including f64's high word).
pub trait Observe {
    fn value(&mut self, _entry: usize, _value: u32, _bits: u64) {}
    fn set(&mut self, _entry: usize, _var: u32, _bits: u64) {}
    fn store(&mut self, _region: u32, _word: u32, _bits: u32) {}
}

impl Observe for () {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortCall { Done, Suspend }

pub trait Ports {
    /// `ops` excludes this port statement. Result words arrive zeroed.
    fn call(&mut self, port: u16, site: u32, ops: u64, args: &[u32], rets: &mut [u32]) -> PortCall;
    /// The device's allocation for buffer `index`, with at least the verified
    /// row's `Buffer::words()` words. Buffer-free callers need not implement
    /// this; absent storage reads as zero and ignores writes.
    fn buffer(&mut self, _index: u32) -> &mut [u32] { &mut [] }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit { Done { ops: u64 }, Suspended { ops: u64 } }

const ENTRY: usize = 0;
const PC: usize = 1;
const OPS_LO: usize = 2;
const OPS_HI: usize = 3;
const DEPTH: usize = 4;
const RET_PC: usize = 5;
const RET_COUNT: usize = 6;
const STATUS: usize = 7;
const RUNNING: u32 = 1;
const SUSPENDED: u32 = 2;

const fn port_words(results: bool) -> usize {
    let mut n = 0;
    let mut k = 0;
    while k < ports::PORTS.len() {
        let len = if results { ports::PORTS[k].rets.len() } else { ports::PORTS[k].params.len() };
        if len > n { n = len; }
        k += 1;
    }
    n
}

const ARGS: usize = 8;
const RETS: usize = ARGS + port_words(false);
const HEADER: usize = RETS + port_words(true);
// A block's end, continuation, loop start, remaining iterations, and kind.
const STACK_WORDS: usize = 5;
const BLOCK_END: usize = 0;
const BLOCK_RETURN: usize = 1;
const LOOP_START: usize = 2;
const LOOP_LEFT: usize = 3;
const BLOCK_KIND: usize = 4;
const ROOT: u32 = 0;
const BRANCH: u32 = 1;
const LOOP: u32 = 2;

#[derive(Clone, Copy)]
struct Layout { base: usize, regs: usize, hi: usize, frame: usize, stack: usize, words: usize }

impl Layout {
    fn new(e: &Entry) -> Option<Self> {
        let regs = (e.n_vals as usize).checked_add(e.n_vars as usize)?;
        let hi = HEADER.checked_add(regs)?;
        let frame = hi.checked_add(if e.wide { regs } else { 0 })?;
        let stack = frame.checked_add(e.frame_words as usize)?;
        let words = stack.checked_add((e.depth as usize).checked_mul(STACK_WORDS)?)?;
        Some(Self { base: HEADER, regs, hi, frame, stack, words })
    }
    fn function(e: &Entry, start: usize, frame: usize) -> Option<Self> {
        // Saved caller function (NONE for entry), call PC and block depth.
        let base = start.checked_add(3)?;
        let regs = (e.n_vals as usize).checked_add(e.n_vars as usize)?;
        let hi = base.checked_add(regs)?;
        let stack = hi.checked_add(if e.wide { regs } else { 0 })?;
        let words = stack.checked_add((e.depth as usize).checked_mul(STACK_WORDS)?)?;
        Some(Self { base, regs, hi, frame, stack, words })
    }
    fn reg(&self, scratch: &[u32], wide: bool, index: usize) -> u64 {
        if index >= self.regs { return 0; }
        u64::from(read(scratch, self.base + index))
            | if wide { u64::from(read(scratch, self.hi + index)) << 32 } else { 0 }
    }
    fn set_reg(&self, scratch: &mut [u32], wide: bool, index: usize, value: u64) {
        if index >= self.regs { return; }
        write(scratch, self.base + index, value as u32);
        if wide { write(scratch, self.hi + index, (value >> 32) as u32); }
    }
}

pub(crate) fn scratch_size(e: &Entry, funcs: &[Func<'_>]) -> Option<usize> {
    let root = Layout::new(e)?;
    if funcs.is_empty() { return Some(root.words); }
    let mut end = root.words.checked_add(1)?; // active function, saved across suspension
    for f in funcs { end = Layout::function(&f.program, end, root.frame)?.words; }
    Some(end)
}

fn function_layout(m: &Module<'_>, root: Layout, func: u32) -> Option<Layout> {
    let mut end = root.words.checked_add(1)?;
    for (k, f) in m.funcs().iter().enumerate() {
        let layout = Layout::function(&f.program, end, root.frame)?;
        if k == func as usize { return Some(layout); }
        end = layout.words;
    }
    None
}

fn read(words: &[u32], at: usize) -> u32 { words.get(at).copied().map_or(0, |w| w) }
fn write(words: &mut [u32], at: usize, value: u32) { if let Some(w) = words.get_mut(at) { *w = value; } }
fn saved_ops(scratch: &[u32]) -> u64 { read(scratch, OPS_LO) as u64 | (read(scratch, OPS_HI) as u64) << 32 }

/// Starts an entry with zeroed registers and frame. Invalid entry indices or
/// undersized caller buffers return Done with zero operations.
pub fn run(m: &Module<'_>, k: usize, state: &mut [u32], scratch: &mut [u32], ports: &mut dyn Ports) -> Exit {
    run_observed(m, k, state, scratch, ports, &mut ())
}

/// Like [`run`], with a statically dispatched observer.
pub fn run_observed<O: Observe>(m: &Module<'_>, k: usize, state: &mut [u32], scratch: &mut [u32], ports: &mut dyn Ports, observer: &mut O) -> Exit {
    let Some(e) = m.entries().get(k) else { return Exit::Done { ops: 0 } };
    let Some(layout) = Layout::new(e) else { return Exit::Done { ops: 0 } };
    if scratch.len() < m.scratch_words(k) || state.len() < m.state_words() as usize { return Exit::Done { ops: 0 }; }
    scratch[..m.scratch_words(k)].fill(0);
    if !m.funcs().is_empty() { write(scratch, layout.words, NONE); }
    write(scratch, ENTRY, k as u32);
    write(scratch, RET_PC, NONE);
    write(scratch, STATUS, RUNNING);
    let mut vm = Vm { m, e, observer, layout, root: layout, active: NONE, state, scratch, pc: e.body + 1, depth: 0, ops: 0 };
    vm.push(e.body + 1 + vm.code(e.body), e.code + e.len, 0, 0, ROOT);
    vm.execute(ports)
}

/// Continues a suspended port. Missing result words are zero; surplus words
/// are ignored. The module, state, and scratch must belong to the same run.
pub fn resume(m: &Module<'_>, state: &mut [u32], scratch: &mut [u32], ports: &mut dyn Ports, rets: &[u32]) -> Exit {
    resume_observed(m, state, scratch, ports, rets, &mut ())
}

/// Like [`resume`]; suspended port results are observed when delivered, once.
pub fn resume_observed<O: Observe>(m: &Module<'_>, state: &mut [u32], scratch: &mut [u32], ports: &mut dyn Ports, rets: &[u32], observer: &mut O) -> Exit {
    let ops = saved_ops(scratch);
    if read(scratch, STATUS) != SUSPENDED { return Exit::Done { ops }; }
    let Some(e) = m.entries().get(read(scratch, ENTRY) as usize) else { return Exit::Done { ops } };
    let Some(layout) = Layout::new(e) else { return Exit::Done { ops } };
    let root = layout;
    let active = if m.funcs().is_empty() { NONE } else { read(scratch, root.words) };
    let (e, layout) = if active == NONE { (e, root) } else {
        let Some(f) = m.funcs().get(active as usize) else { return Exit::Done { ops }; };
        let Some(layout) = function_layout(m, root, active) else { return Exit::Done { ops }; };
        (&f.program, layout)
    };
    let depth = read(scratch, DEPTH);
    let pc = read(scratch, PC);
    if scratch.len() < m.scratch_words(read(scratch, ENTRY) as usize) || state.len() < m.state_words() as usize || depth == 0 || depth > e.depth || pc < e.body || pc > e.code + e.len {
        return Exit::Done { ops };
    }
    let at = read(scratch, RET_PC);
    let n = read(scratch, RET_COUNT);
    if n as usize > port_words(true) || at.checked_add(n).is_none_or(|end| end as usize > m.code().len()) { return Exit::Done { ops }; }
    let mut vm = Vm { m, e, observer, layout, root, active, state, scratch, pc, depth, ops };
    for k in 0..n {
        vm.set_reg(vm.code(at + k) as usize, read(rets, k as usize) as u64);
    }
    write(vm.scratch, RET_PC, NONE);
    write(vm.scratch, RET_COUNT, 0);
    write(vm.scratch, STATUS, RUNNING);
    vm.execute(ports)
}

struct Vm<'a, 'm, O: Observe> {
    observer: &'a mut O,
    m: &'a Module<'m>,
    e: &'a Entry,
    layout: Layout,
    root: Layout,
    active: u32,
    state: &'a mut [u32],
    scratch: &'a mut [u32],
    pc: u32,
    depth: u32,
    ops: u64,
}

impl<O: Observe> Vm<'_, '_, O> {
    fn code(&self, at: u32) -> u32 { read(self.m.code(), at as usize) }
    fn reg(&self, index: usize) -> u64 { self.layout.reg(self.scratch, self.e.wide, index) }
    fn set_reg(&mut self, index: usize, value: u64) {
        self.layout.set_reg(self.scratch, self.e.wide, index, value);
        self.observe(index, value);
    }
    /// Reports a write to one of the entry's registers. Inside a shared function the ids name the
    /// function's own registers, which are not entry facts and are not reported.
    fn observe(&mut self, index: usize, value: u64) {
        if self.active != NONE || index >= self.layout.regs { return; }
        let entry = read(self.scratch, ENTRY) as usize;
        if index < self.e.n_vals as usize { self.observer.value(entry, index as u32, value); }
        else { self.observer.set(entry, index as u32 - self.e.n_vals, value); }
    }
    fn value(&self, index: u32) -> u64 { self.reg(index as usize) }
    fn field(&self, level: u32, field: usize) -> u32 { read(self.scratch, self.layout.stack + level as usize * STACK_WORDS + field) }
    fn set_field(&mut self, level: u32, field: usize, value: u32) { write(self.scratch, self.layout.stack + level as usize * STACK_WORDS + field, value); }
    fn push(&mut self, end: u32, ret: u32, start: u32, left: u32, kind: u32) {
        self.set_field(self.depth, BLOCK_END, end);
        self.set_field(self.depth, BLOCK_RETURN, ret);
        self.set_field(self.depth, LOOP_START, start);
        self.set_field(self.depth, LOOP_LEFT, left);
        self.set_field(self.depth, BLOCK_KIND, kind);
        self.depth += 1;
    }
    fn address(&self, base: u32, extent: u32, off: u32) -> usize {
        let delta = if off == NONE { 0 } else { (self.value(off) as u32).min(extent.saturating_sub(1)) };
        // Verification proved base + extent is within its region.
        base as usize + delta as usize
    }
    fn load(&self, region: u32, at: usize) -> u32 {
        match region {
            REGION_STATE => read(self.state, at),
            REGION_SHARED => read(self.m.shared(), at),
            REGION_FRAME => read(self.scratch, self.layout.frame + at),
            _ => 0,
        }
    }
    fn store(&mut self, region: u32, at: usize, value: u32) {
        match region {
            REGION_STATE => write(self.state, at, value),
            REGION_FRAME => write(self.scratch, self.layout.frame + at, value),
            _ => return,
        }
        self.observer.store(region, at as u32, value);
    }
    fn finish(&mut self, suspended: bool) -> Exit {
        if !self.m.funcs().is_empty() { write(self.scratch, self.root.words, self.active); }
        write(self.scratch, PC, self.pc);
        write(self.scratch, DEPTH, self.depth);
        write(self.scratch, OPS_LO, self.ops as u32);
        write(self.scratch, OPS_HI, (self.ops >> 32) as u32);
        write(self.scratch, STATUS, if suspended { SUSPENDED } else { 0 });
        if suspended { Exit::Suspended { ops: self.ops } } else { Exit::Done { ops: self.ops } }
    }
    fn execute(mut self, ports: &mut dyn Ports) -> Exit {
        while self.depth != 0 {
            let level = self.depth - 1;
            if self.pc == self.field(level, BLOCK_END) {
                let left = self.field(level, LOOP_LEFT);
                if self.field(level, BLOCK_KIND) == LOOP && left != 0 {
                    self.set_field(level, LOOP_LEFT, left - 1);
                    self.pc = self.field(level, LOOP_START);
                    self.ops = self.ops.saturating_add(1);
                } else if self.field(level, BLOCK_KIND) == ROOT && self.active != NONE {
                    let f = &self.m.funcs()[self.active as usize];
                    let saved = self.layout.base - 3;
                    let caller = read(self.scratch, saved);
                    let call = read(self.scratch, saved + 1);
                    let depth = read(self.scratch, saved + 2);
                    let (e, layout) = if caller == NONE {
                        (&self.m.entries()[read(self.scratch, ENTRY) as usize], self.root)
                    } else {
                        (&self.m.funcs()[caller as usize].program, function_layout(self.m, self.root, caller).expect("verified caller"))
                    };
                    let rets = call + 4 + self.code(call + 2);
                    for (k, &result) in f.results.iter().enumerate() {
                        let value = self.value(result);
                        layout.set_reg(self.scratch, e.wide, self.code(rets + k as u32) as usize, value);
                    }
                    self.pc = rets + f.results.len() as u32;
                    self.depth = depth;
                    self.e = e;
                    self.layout = layout;
                    self.active = caller;
                    // The results are the caller's definitions: an entry's are reported.
                    for k in 0..f.results.len() as u32 {
                        let index = self.code(rets + k) as usize;
                        let value = self.reg(index);
                        self.observe(index, value);
                    }
                } else {
                    self.pc = self.field(level, BLOCK_RETURN);
                    self.depth -= 1;
                }
                continue;
            }
            let at = self.pc;
            let instruction = self.code(at);
            let op = instruction & OP_MASK;
            let sub = instruction >> SUB_SHIFT;
            let before = self.ops;
            if op != OP_LOOP { self.ops = self.ops.saturating_add(1); }
            // Operand counts and all code additions were proved by load.
            match op {
                OP_CONST | OP_CONST_D | OP_GET | OP_UN | OP_BIN | OP_CMP_F | OP_CMP_I | OP_CMP_D | OP_SEL | OP_WRAP | OP_LOAD | OP_FMA => {
                    let dst = self.code(at + 1);
                    let a = self.code(at + 2);
                    let (result, len) = match op {
                        OP_CONST => (a as u64, 3),
                        OP_CONST_D => (a as u64 | (self.code(at + 3) as u64) << 32, 4),
                        OP_GET => (self.reg(self.e.n_vals as usize + a as usize), 3),
                        OP_UN => (unary(sub, self.value(a)), 3),
                        OP_BIN => (binary(sub, self.value(a), self.value(self.code(at + 3))), 4),
                        OP_CMP_F => (compare(sub, f(self.value(a)), f(self.value(self.code(at + 3)))), 4),
                        OP_CMP_I => (compare(sub, i(self.value(a)), i(self.value(self.code(at + 3)))), 4),
                        OP_CMP_D => (compare(sub, d(self.value(a)), d(self.value(self.code(at + 3)))), 4),
                        OP_SEL => (self.value(self.code(at + if self.value(a) != 0 { 3 } else { 4 })), 5),
                        OP_WRAP => (wrap(self.value(a) as u32, self.code(at + 3)) as u64, 4),
                        OP_LOAD => {
                            let value = if sub >= REGION_BUF_BASE {
                                let k = if sub == REGION_BUF_LONG { self.code(at + 5) } else { sub - REGION_BUF_BASE };
                                let buffer = self.m.buffers()[k as usize];
                                let off = self.code(at + 4);
                                let index = buffer.index(a, if off == NONE { 0 } else { self.value(off) as u32 });
                                buffer.load(ports.buffer(k), index)
                            } else {
                                self.load(sub, self.address(a, self.code(at + 3), self.code(at + 4)))
                            };
                            (u64::from(value), 5 + u32::from(sub == REGION_BUF_LONG))
                        }
                        OP_FMA => (iw(i(self.value(a)).wrapping_mul(i(self.value(self.code(at + 3)))).wrapping_add(i(self.value(self.code(at + 4))))), 5),
                        _ => (0, 1),
                    };
                    self.set_reg(dst as usize, result);
                    self.pc += len;
                }
                OP_SET => {
                    self.set_reg(self.e.n_vals as usize + self.code(at + 1) as usize, self.value(self.code(at + 2)));
                    self.pc += 3;
                }
                OP_STORE => {
                    let value = self.value(self.code(at + 4)) as u32;
                    if sub >= REGION_BUF_BASE {
                        let k = if sub == REGION_BUF_LONG { self.code(at + 5) } else { sub - REGION_BUF_BASE };
                        let buffer = self.m.buffers()[k as usize];
                        let off = self.code(at + 3);
                        let index = buffer.index(self.code(at + 1), if off == NONE { 0 } else { self.value(off) as u32 });
                        let words = ports.buffer(k);
                        buffer.store(words, index, value);
                        let word = index / (4 / buffer.elem.bytes() as usize);
                        if let Some(&bits) = words.get(word) {
                            self.observer.store(REGION_BUF_BASE + k, word as u32, bits);
                        }
                    } else {
                        let address = self.address(self.code(at + 1), self.code(at + 2), self.code(at + 3));
                        self.store(sub, address, value);
                    }
                    self.pc += 5 + u32::from(sub == REGION_BUF_LONG);
                }
                OP_IF => {
                    let then_at = at + 2;
                    let else_at = then_at + 1 + self.code(then_at);
                    let end = else_at + 1 + self.code(else_at);
                    let (start, stop) = if self.value(self.code(at + 1)) != 0 { (then_at + 1, else_at) } else { (else_at + 1, end) };
                    self.push(stop, end, 0, 0, BRANCH);
                    self.pc = start;
                }
                OP_LOOP => {
                    let cap = self.code(at + 1);
                    let body = at + 2;
                    let end = body + 1 + self.code(body);
                    if cap == 0 { self.pc = end; } else {
                        self.push(end, end, body + 1, cap - 1, LOOP);
                        self.pc = body + 1;
                        self.ops = self.ops.saturating_add(1);
                    }
                }
                OP_BREAK | OP_CONTINUE => {
                    let mut depth = self.code(at + 1);
                    let mut level = self.depth;
                    while level != 0 {
                        level -= 1;
                        if self.field(level, BLOCK_KIND) == LOOP {
                            if depth == 0 {
                                if op == OP_BREAK {
                                    self.pc = self.field(level, BLOCK_RETURN);
                                    self.depth = level;
                                } else {
                                    self.pc = self.field(level, BLOCK_END);
                                    self.depth = level + 1;
                                }
                                break;
                            }
                            depth -= 1;
                        }
                    }
                }
                OP_CALL => {
                    let func = self.code(at + 1);
                    let f = &self.m.funcs()[func as usize];
                    let layout = function_layout(self.m, self.root, func).expect("verified function layout");
                    let saved = layout.base - 3;
                    // The DAG ensures this static area is not already active.
                    // Only registers and control state reset; the shared frame survives.
                    self.scratch[saved..layout.words].fill(0);
                    write(self.scratch, saved, self.active);
                    write(self.scratch, saved + 1, at);
                    write(self.scratch, saved + 2, self.depth);
                    for (k, &param) in f.params.iter().enumerate() {
                        let value = self.value(self.code(at + 4 + k as u32));
                        layout.set_reg(self.scratch, f.program.wide, param as usize, value);
                    }
                    self.ops = self.ops.saturating_add(3 + f.params.len() as u64);
                    self.active = func;
                    self.e = &f.program;
                    self.layout = layout;
                    self.depth = 0;
                    self.pc = self.e.body + 1;
                    self.push(self.e.code + self.e.len, self.e.code + self.e.len, 0, 0, ROOT);
                }
                OP_PORT => {
                    let port = self.code(at + 1);
                    let site = self.code(at + 2);
                    let nargs = self.code(at + 3) as usize;
                    let nrets = self.code(at + 4) as usize;
                    for k in 0..nargs {
                        let arg = self.value(self.code(at + 5 + k as u32)) as u32;
                        write(self.scratch, ARGS + k, arg);
                    }
                    for k in 0..nrets { write(self.scratch, RETS + k, 0); }
                    let ret_pc = at + 5 + nargs as u32;
                    self.pc = ret_pc + nrets as u32;
                    let result = {
                        // The fixed port table and the verified arity bound both
                        // slices; Layout includes these buffers even for no ports.
                        let (args, rets) = self.scratch.split_at_mut(RETS);
                        ports.call(port as u16, site, before, &args[ARGS..ARGS + nargs], &mut rets[..nrets])
                    };
                    if result == PortCall::Suspend {
                        write(self.scratch, RET_PC, ret_pc);
                        write(self.scratch, RET_COUNT, nrets as u32);
                        return self.finish(true);
                    }
                    for k in 0..nrets {
                        self.set_reg(self.code(ret_pc + k as u32) as usize, read(self.scratch, RETS + k) as u64);
                    }
                }
                _ => return self.finish(false),
            }
        }
        self.finish(false)
    }
}

fn f(x: u64) -> f32 { f32::from_bits(x as u32) }
fn d(x: u64) -> f64 { f64::from_bits(x) }
fn i(x: u64) -> i32 { x as u32 as i32 }
fn fw(x: f32) -> u64 { x.to_bits() as u64 }
fn iw(x: i32) -> u64 { x as u32 as u64 }

fn unary(op: u32, a: u64) -> u64 {
    match op {
        UN_NEG_F => fw(-f(a)),
        UN_ABS_F => fw(fmath::abs_f32(f(a))),
        UN_SQRT_F => fw(fmath::sqrt_f32(f(a))),
        UN_FLOOR_F => fw(fmath::floor_f32(f(a))),
        UN_CEIL_F => fw(fmath::ceil_f32(f(a))),
        UN_TRUNC_F => fw(fmath::trunc_f32(f(a))),
        UN_ROUND_F => fw(fmath::round_f32(f(a))),
        UN_F2I => iw(f(a) as i32),
        UN_I2F => fw(i(a) as f32),
        UN_BITS_FI | UN_BITS_IF => a & 0xFFFF_FFFF,
        UN_NEG_I => iw(i(a).wrapping_neg()),
        UN_NOT_B => (a as u32 == 0) as u64,
        UN_NEG_D => (-d(a)).to_bits(),
        UN_ABS_D => fmath::abs_f64(d(a)).to_bits(),
        UN_SQRT_D => fmath::sqrt_f64(d(a)).to_bits(),
        UN_FLOOR_D => fmath::floor_f64(d(a)).to_bits(),
        UN_CEIL_D => fmath::ceil_f64(d(a)).to_bits(),
        UN_TRUNC_D => fmath::trunc_f64(d(a)).to_bits(),
        UN_ROUND_D => fmath::round_f64(d(a)).to_bits(),
        UN_F2D => (f(a) as f64).to_bits(),
        UN_D2F => fw(d(a) as f32),
        UN_D2I => iw(d(a) as i32),
        UN_I2D => (i(a) as f64).to_bits(),
        UN_HI_D => a >> 32,
        UN_LO_D => a & 0xFFFF_FFFF,
        _ => 0,
    }
}

fn binary(op: u32, a: u64, b: u64) -> u64 {
    match op {
        BIN_ADD_F | BIN_SUB_F | BIN_MUL_F => {
            let am = a as u32 & 0x7FFF_FFFF;
            let bm = b as u32 & 0x7FFF_FFFF;
            let opposite = (a ^ b) & 0x8000_0000 != 0;
            let invalid = match op {
                BIN_ADD_F => am == 0x7F80_0000 && bm == am && opposite,
                BIN_SUB_F => am == 0x7F80_0000 && bm == am && !opposite,
                _ => (am == 0 && bm == 0x7F80_0000) || (bm == 0 && am == 0x7F80_0000),
            };
            fw(fmath::binary_nan_f32(f(a), f(b), invalid).unwrap_or_else(|| match op {
                BIN_ADD_F => f(a) + f(b),
                BIN_SUB_F => f(a) - f(b),
                _ => f(a) * f(b),
            }))
        }
        BIN_DIV_F => fw(fmath::div_f32(f(a), f(b))),
        BIN_MIN_F => if f(a) < f(b) { a } else { b },
        BIN_MAX_F => if f(a) > f(b) { a } else { b },
        BIN_ADD_I => iw(i(a).wrapping_add(i(b))),
        BIN_SUB_I => iw(i(a).wrapping_sub(i(b))),
        BIN_MUL_I => iw(i(a).wrapping_mul(i(b))),
        BIN_DIV_I => if b as u32 == 0 { 0 } else { iw(i(a).wrapping_div(i(b))) },
        BIN_REM_I => if b as u32 == 0 { a } else { iw(i(a).wrapping_rem(i(b))) },
        BIN_AND_I | BIN_AND_B => a & b,
        BIN_OR_I | BIN_OR_B => a | b,
        BIN_XOR_I => a ^ b,
        BIN_SHL_I => iw(i(a).wrapping_shl(b as u32)),
        BIN_SHR_I => iw(i(a).wrapping_shr(b as u32)),
        BIN_SHR_UI => (a as u32).wrapping_shr(b as u32) as u64,
        BIN_ADD_D | BIN_SUB_D | BIN_MUL_D | BIN_DIV_D => {
            let am = a & 0x7FFF_FFFF_FFFF_FFFF;
            let bm = b & 0x7FFF_FFFF_FFFF_FFFF;
            let infinity = 0x7FF0_0000_0000_0000;
            let opposite = (a ^ b) & 0x8000_0000_0000_0000 != 0;
            let invalid = match op {
                BIN_ADD_D => am == infinity && bm == am && opposite,
                BIN_SUB_D => am == infinity && bm == am && !opposite,
                BIN_MUL_D => (am == 0 && bm == infinity) || (bm == 0 && am == infinity),
                _ => (am == 0 && bm == 0) || (am == infinity && bm == infinity),
            };
            fmath::binary_nan_f64(d(a), d(b), invalid).unwrap_or_else(|| match op {
                BIN_ADD_D => d(a) + d(b),
                BIN_SUB_D => d(a) - d(b),
                BIN_MUL_D => d(a) * d(b),
                _ => d(a) / d(b),
            }).to_bits()
        }
        BIN_MIN_D => if d(a) < d(b) { a } else { b },
        BIN_MAX_D => if d(a) > d(b) { a } else { b },
        BIN_MAKE_D => (a << 32) | (b & 0xFFFF_FFFF),
        _ => 0,
    }
}

fn compare<T: PartialOrd>(op: u32, a: T, b: T) -> u64 {
    (match op {
        CMP_LT => a < b,
        CMP_LE => a <= b,
        CMP_GT => a > b,
        CMP_GE => a >= b,
        CMP_EQ => a == b,
        CMP_NE => a != b,
        _ => false,
    }) as u64
}

fn wrap(x: u32, len: u32) -> u32 {
    if len == 0 { return 0; }
    if len.is_power_of_two() { x & (len - 1) } else {
        let r = (x as i32).wrapping_rem(len as i32);
        (if r < 0 { r.wrapping_add(len as i32) } else { r }) as u32
    }
}

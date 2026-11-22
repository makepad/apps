//! Kernel objects through windowed import thunks, on R3's real switch/pick
//! path in the S3 model. Time advances only when the host changes snapshot.
use std::sync::{Arc, OnceLock};

use fab_elec_mcu::compile_kernel;
use fab_elec_mcu_emit32::xtensa::{assemble, enc, reg::*, Asm};
use fab_elec_mcu_emit_s3::{build_kernel, chip, KernelOptions, KernelRegion, RADIO_KERNEL_LINES};
use fab_elec_mcu_emit_s3::radio::{self, *, kernel::{kernel_consts, KernelLayout, OBJECTS, TICKS_PER_US,
    gates::{gates, EntryRegs, BLOCKING_EXPORTS}}};
use fab_elec_mcu_emu::bus::{Bus, Kind, Width};
use fab_elec_mcu_rt::{kernel::RegionKind, module::load_kernel};
use fab_elec_mcu_s3::config::s3_config;
use fab_elec_mcu_xtensa::{insn::{decode, Op}, sim::{cause, Cpu, TrapMode}};

#[path = "support/heap_check.rs"] mod heap_check;

const VECBASE: u32 = 0x4000_0000;
const SWITCH: u32 = VECBASE + 0x8000;
const SCHED: u32 = VECBASE + 0x1_0000;
const SHIM: u32 = VECBASE + 0x1_4000;
const TASK_CODE: u32 = VECBASE + 0x2_0000;
const OBJECT_CODE: u32 = VECBASE + 0x10_0000;
const RAM: u32 = 0x3ffb_0000;
const RAM_BYTES: usize = 0x3_0000;
const CURRENT: u32 = RAM;
const TASK_COUNT: u32 = RAM + 8;
const ISR_COUNT: u32 = RAM + 12;
const ISLAND_GUARD_ADDR: u32 = RAM + 28;
const ISLAND_STATUS_ADDR: u32 = RAM + 32;
const TASKS: u32 = RAM + 0x80;
const FAULT: u32 = RAM + 0x100;
const TCBS: u32 = RAM + 0x400;
const LOG_CURSOR: u32 = RAM + 0x700;
const LOG: u32 = RAM + 0x800;
const SEM: u32 = RAM + 0x1800;
const QUEUE: u32 = SEM + 4;
const MUTEX: u32 = SEM + 8;
const SEND: u32 = RAM + 0x1901;
const RECV: u32 = RAM + 0x1981;
const EXPIRE_OUT: u32 = RAM + 0x1a00;
const ISR_DONE: u32 = EXPIRE_OUT + 8;
const ISR_WOKEN: u32 = EXPIRE_OUT + 12;
const STATE: u32 = RAM + 0x2000;
const SHARED: u32 = RAM + 0x4000;
const HEAP: u32 = RAM + 0x5000;
const HEAP_BYTES: u32 = 0xa000;
const T0: u32 = RAM + 0xfff0;
const STACKS: u32 = RAM + 0x1_0000;
const STACK_BYTES: u32 = 0x6000;
const IRQ_BASE: u32 = RAM + 0x2_e000;
const IRQ_TOP: u32 = RAM + RAM_BYTES as u32;
const TIMER: u32 = 0x6002_3000;
const WOE: u32 = 1 << 18;
const UM: u32 = 1 << 5;
const IDLE: u32 = 0;
const L: u32 = 1;
const M: u32 = 2;
const H: u32 = 3;
const FOREVER: u32 = u32::MAX;
const X: u32 = 4;
const HANDLE: u32 = EXPIRE_OUT + 16;
const HEAP_BEFORE: u32 = HANDLE + 4;
const HEAP_AFTER: u32 = HANDLE + 8;

fn tcb(task: u32) -> u32 { TCBS + task * TCB_SIZE as u32 }
fn bottom(task: u32) -> u32 { STACKS + task * STACK_BYTES }
fn top(task: u32) -> u32 { bottom(task) + STACK_BYTES }
fn aligned(address: u32) -> u32 { (address + 3) & !3 }

struct GateSite {
    name: String,
    entry: u32,
    mask: u32,
    restore: u32,
    retw: u32,
    bound: u64,
    blocks: bool,
}

struct Fixture {
    code: Vec<(u32, Arc<[u8]>)>,
    kernel: radio::KernelCode,
    sched: SchedCode,
    block_lower: u32,
    gates: Vec<GateSite>,
    state: Vec<u32>,
    shared: Vec<u32>,
}

impl Fixture {
    fn gate(&self, name: &str) -> &GateSite {
        self.gates.iter().find(|g| g.name == name).expect("named object gate")
    }
}

fn regions() -> Vec<KernelRegion> {
    use radio::kernel::{EFUSE_MAC_0, EFUSE_MAC_1, INTR_MATRIX, RNG_DATA, RTC_STORE1, SYSCON_WIFI_CLK_EN, SYSCON_WIFI_RST_EN, WIFI_SOURCES};
    vec![
        KernelRegion { base: RAM, len: RAM_BYTES as u32, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: SYSCON_WIFI_CLK_EN, len: SYSCON_WIFI_RST_EN - SYSCON_WIFI_CLK_EN + 4, writable: true, kind: RegionKind::Device },
        KernelRegion { base: INTR_MATRIX, len: WIFI_SOURCES * 4, writable: true, kind: RegionKind::Device },
        KernelRegion { base: EFUSE_MAC_0, len: EFUSE_MAC_1 - EFUSE_MAC_0 + 4, writable: false, kind: RegionKind::Device },
        KernelRegion { base: RTC_STORE1, len: 4, writable: false, kind: RegionKind::Device },
        KernelRegion { base: RNG_DATA, len: 4, writable: false, kind: RegionKind::Device },
    ]
}

// The same addresses and compiled objects serve all tables; only SRAM and
// the small task programs differ. No compiler work is repeated per table.
fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let shim = shim_code(SHIM);
        let sched = sched_code(SCHED, &SchedLayout {
            current: CURRENT, cursor: CURRENT + CURSOR_OFFSET, task_count: TASK_COUNT, tasks: TASKS,
            isr_count: ISR_COUNT, isrs: RAM + 0x600, stray: RAM + 16,
            dispatcher: tcb(H), wake_lines: 0, vector_lines: 0, shim: shim.call, marks: None,
        });
        let kernel = kernel_code(SWITCH, VECBASE, &RadioLayout {
            current: CURRENT, irq_stack_base: IRQ_BASE, irq_stack_top: IRQ_TOP, pick: sched.pick,
            tasks: TASKS, task_count: TASK_COUNT, dispatcher: tcb(H), idle: tcb(IDLE),
            isr_count: ISR_COUNT, wake_lines: 0, vector_lines: 0, shim_start: SHIM,
            shim_end: SHIM + shim.bytes.len() as u32, radio_state: RAM + 20,
            island_guard: ISLAND_GUARD_ADDR, island_status: ISLAND_STATUS_ADDR,
            fault: FAULT, trace_head: RAM + 24, trace: RAM + 0x1b00, trace_entries: 0,
        });
        let compiled = compile_kernel(OBJECTS, &chip::profile(), &RADIO_KERNEL_LINES,
            &kernel_consts(&KernelLayout { current: CURRENT, tasks: TASKS, task_count: TASK_COUNT,
                phy_cal_data: RAM + 0xf000, phy_dig_regs: RAM + 0xf800, phy_mutex: RAM + 0x680,
                isrs: RAM + 0x600, isr_count: ISR_COUNT, isr_capacity: PACK_ISR_LINES, code_region: VECBASE >> 30,
                max_tasks: 5, heap_base: HEAP, heap_size: HEAP_BYTES, timer_reserve: 64 }))
            .unwrap_or_else(|errors| panic!("objects.splash: {errors:?}"));
        let module = load_kernel(&compiled.module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
        // Match the device stop slot. An ordinary report retires 15+5 with room,
        // 11+5 on loss or 9+5 on saturated loss, including the comparison suffix.
        let objects = build_kernel(&module, &KernelOptions { base: OBJECT_CODE, state: STATE, t0: T0,
            reports: Some(RAM + 0x1c00), stop_pending: Some(FAULT + STOP_PENDING_OFFSET),
            regions: regions() }).unwrap();
        let gate_base = aligned(objects.base + objects.bytes.len() as u32);
        let routines = gates(gate_base, &objects, EntryRegs { state: STATE, shared: SHARED });
        assert_eq!(routines.gates.len(), objects.entries.len(), "one routine per object export");
        let imports: Vec<_> = routines.gates.iter().enumerate().map(|(index, g)|
            Gate { name: &g.name, call: g.address, blocks: g.blocks,
                exit_level: None, export: index.try_into().unwrap() }).collect();
        let thunk_base = aligned(gate_base + routines.bytes.len() as u32);
        let thunks = exports(thunk_base, &[], &imports, None,
            Some(Blocking { current: CURRENT, block: sched.block, crit_depth: None,
                kernel_fault: objects.symbols.iter().find(|s| s.0 == "kernel_fault").unwrap().1 }));
        let mut sites = Vec::new();
        for (index, g) in routines.gates.iter().enumerate() {
            let e = &objects.entries[index];
            assert_eq!(g.stack_bytes, 16 + e.stack_bytes, "{}: CALL0 gate frame bound", g.name);
            assert!(64 + 64 + 32 + 16 + g.stack_bytes + CONTEXT_SIZE as u32 + 16 < STACK_BYTES,
                "{}: task has spill and interrupt headroom below its deepest frame", g.name);
            assert_eq!(g.blocks, BLOCKING_EXPORTS.contains(&g.name.as_str()), "{}: blocking flag", g.name);
            if g.name.ends_with("_isr") { assert!(!g.blocks, "{}: ISR must not check current", g.name); }
            let entry = thunks.thunks[index];
            let end = thunks.thunks.get(index + 1).copied().unwrap_or(thunk_base + thunks.bytes.len() as u32);
            let mut offset = (entry - thunk_base) as usize;
            let mut own = 0;
            let (mut mask, mut restore, mut retw) = (0, 0, 0);
            while offset < (end - thunk_base) as usize {
                let mut bytes = [0; 4];
                let n = (thunks.bytes.len() - offset).min(3);
                bytes[..n].copy_from_slice(&thunks.bytes[offset..offset + n]);
                let (insn, len) = decode(u32::from_le_bytes(bytes)).expect("thunk decodes");
                let pc = thunk_base + offset as u32;
                if insn.op == Op::Rsil && insn.imm == 15 { mask = pc; }
                if insn.op == Op::Wsr && insn.imm == 230 { restore = pc; }
                if insn.op == Op::RetwN { retw = pc; }
                offset += len as usize;
                own += 1;
            }
            assert!(mask != 0 && restore != 0 && retw != 0, "{}: complete thunk bracket", g.name);
            sites.push(GateSite { name: g.name.clone(), entry, mask, restore, retw,
                bound: own + g.max_insns, blocks: g.blocks });
        }
        let mut block_lower = 0;
        let mut offset = (sched.block - SCHED) as usize;
        while offset < (sched.idle - SCHED) as usize {
            let mut bytes = [0; 4];
            let n = (sched.bytes.len() - offset).min(3);
            bytes[..n].copy_from_slice(&sched.bytes[offset..offset + n]);
            let (insn, len) = decode(u32::from_le_bytes(bytes)).unwrap();
            if insn.op == Op::Rsil && insn.imm == 0 { block_lower = SCHED + offset as u32; }
            offset += len as usize;
        }
        assert_ne!(block_lower, 0, "block has its unmasking boundary");
        let mut vectors = vec![0; 0x400];
        vectors[..WINDOW_BLOCK_SIZE].copy_from_slice(&window_block());
        let user = user_vector(VECBASE, kernel.irq_entry, kernel.fault_entry);
        vectors[USER_VECTOR_OFFSET..USER_VECTOR_OFFSET + user.len()].copy_from_slice(&user);
        let mut code: Vec<(u32, Arc<[u8]>)> = vec![
            (VECBASE, vectors.into()), (SWITCH, kernel.bytes.clone().into()),
            (SCHED, sched.bytes.clone().into()), (SHIM, shim.bytes.into()),
            (objects.base, objects.bytes.into()), (gate_base, routines.bytes.into()),
            (thunk_base, thunks.bytes.into()),
        ];
        code.sort_by_key(|c| c.0);
        for pair in code.windows(2) {
            assert!(pair[0].0 + pair[0].1.len() as u32 <= pair[1].0, "fixture code regions are disjoint");
        }
        assert!(module.state_words() * 4 <= SHARED - STATE, "state fits its fixture allocation");
        assert!(module.shared().len() * 4 <= (HEAP - SHARED) as usize, "shared data fits its allocation");
        let mut state = module.state_init().to_vec();
        state[((objects.task_exit_link.unwrap() - STATE) / 4) as usize] = windowed_exit_link(kernel.task_exit);
        state[((objects.phy_init_data.unwrap() - STATE) / 4) as usize] = 0; // No PHY table in this fixture.
        Fixture { code, kernel, sched, block_lower, gates: sites, state, shared: module.shared().to_vec() }
    })
}

struct SwitchMemory {
    code: Vec<(u32, Arc<[u8]>)>,
    ram: Vec<u8>,
    snapshot: u64,
    latched: u64,
}

impl SwitchMemory {
    fn bytes(&self, address: u32, len: usize) -> &[u8] {
        &self.ram[(address - RAM) as usize..(address - RAM) as usize + len]
    }
    fn word(&self, address: u32) -> u32 {
        u32::from_le_bytes(self.bytes(address, 4).try_into().unwrap())
    }
    fn put(&mut self, address: u32, value: u32) {
        self.copy(address, &value.to_le_bytes());
    }
    fn copy(&mut self, address: u32, bytes: &[u8]) {
        self.ram[(address - RAM) as usize..(address - RAM) as usize + bytes.len()].copy_from_slice(bytes);
    }
}

impl Bus for SwitchMemory {
    fn read(&mut self, address: u32, width: Width, kind: Kind, _: u64) -> Result<u32, ()> {
        // Same unit-0 snapshot mapping as tests/kernel.rs: UPDATE makes
        // VALID immediate, and 0x40/0x44 hold high/low respectively.
        if width == Width::W && kind == Kind::Load {
            match address {
                a if a == TIMER + 4 => return Ok(1 << 29),
                a if a == TIMER + 0x40 => return Ok((self.latched >> 32) as u32),
                a if a == TIMER + 0x44 => return Ok(self.latched as u32),
                _ => {}
            }
        }
        let len = width.bytes();
        let bytes = if address >= RAM && u64::from(address) + len as u64 <= u64::from(IRQ_TOP) {
            if kind == Kind::Fetch { return Err(()); }
            self.bytes(address, len)
        } else {
            if kind == Kind::Store { return Err(()); }
            let (base, code) = self.code.iter().find(|(base, code)| address >= *base
                && u64::from(address) + len as u64 <= u64::from(*base) + code.len() as u64).ok_or(())?;
            &code[(address - *base) as usize..(address - *base) as usize + len]
        };
        Ok(bytes.iter().enumerate().fold(0, |v, (i, &b)| v | (u32::from(b) << (8 * i))))
    }
    fn write(&mut self, address: u32, width: Width, value: u32, _: u64) -> Result<(), ()> {
        if address == TIMER + 4 {
            assert_eq!((width, value), (Width::W, 1 << 30), "unit-0 UPDATE protocol");
            self.latched = self.snapshot;
            return Ok(());
        }
        let len = width.bytes();
        if address < RAM || u64::from(address) + len as u64 > u64::from(IRQ_TOP) { return Err(()); }
        self.copy(address, &value.to_le_bytes()[..len]);
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Arg { Value(u32), Word(u32) }
use Arg::{Value, Word};

struct Task { bytes: Vec<u8>, entry: u32 }
struct TaskWriter<'a> { a: &'a mut Asm, task: u32, fixture: &'a Fixture }

impl TaskWriter<'_> {
    fn call(&mut self, name: &str, args: &[Arg]) {
        assert!(args.len() <= 6, "task uses the six-argument island ABI");
        for (n, arg) in args.iter().enumerate() {
            let reg = A10 + n as u8;
            match *arg {
                Value(value) => { self.a.mov_reg_i32(reg, value); }
                Word(address) => { self.a.mov_reg_i32(reg, address); self.a.l32i(reg, reg, 0); }
            }
        }
        self.a.mov_reg_i32(A8, self.fixture.gate(name).entry);
        self.a.callx8(A8);
        self.a.mov_n(A2, A10);
    }
    fn save(&mut self, address: u32) {
        self.a.mov_reg_i32(A3, address);
        self.a.s32i(A2, A3, 0);
    }
    fn log(&mut self, step: u32) {
        self.a.mov_reg_i32(A3, LOG_CURSOR);
        self.a.l32i(A4, A3, 0);
        self.a.movi(A5, self.task as i32);
        self.a.s32i(A5, A4, 0);
        self.a.movi(A5, step as i32);
        self.a.s32i(A5, A4, 4);
        self.a.s32i(A2, A4, 8);
        self.a.addi(A4, A4, 12);
        self.a.s32i(A4, A3, 0); // publish a complete (task, step, a2)
    }
    fn finish(&mut self) {
        // A completed fixture task exits the READY set under the same
        // software-yield protocol, keeping its root frame live for spills.
        self.a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        self.a.mov_reg_i32(A2, tcb(self.task));
        self.a.movi(A3, DEAD as i32);
        self.a.s32i(A3, A2, TCB_STATE);
        self.a.movi(A3, (1 << YIELD_LINE) as i32);
        self.a.wsr(A3, 226);
        self.a.op24(enc::rrr(0, 0, 0, 6, 0, A3));
        self.a.rsync();
        self.a.j(-4);
    }
}

fn task(id: u32, body: impl Fn(&mut TaskWriter<'_>)) -> Task {
    let fixture = fixture();
    let base = TASK_CODE + id * 0x2000;
    let mut entry = 0;
    let bytes = assemble(&mut Asm::new(0, A8), |a| {
        a.literal_table();
        entry = base + a.base.offset();
        a.entry(A1, 64);
        let mut w = TaskWriter { a, task: id, fixture };
        body(&mut w);
        w.finish();
    });
    assert!(bytes.len() < 0x2000, "task fits its code allocation");
    Task { bytes, entry }
}

struct SwitchMachine {
    cpu: Cpu,
    memory: SwitchMemory,
    fixture: &'static Fixture,
    masked: Option<(usize, u64)>,
    starts: usize,
    spans: usize,
    largest: (u64, usize),
    blocked: Vec<(u32, String)>,
    picks: usize,
}

impl SwitchMachine {
    fn new(tasks: &[(u32, Task)]) -> Self {
        let f = fixture();
        let mut memory = SwitchMemory { code: f.code.clone(), ram: vec![0; RAM_BYTES], snapshot: 0, latched: 0 };
        for (id, task) in tasks { memory.code.push((TASK_CODE + id * 0x2000, task.bytes.clone().into())); }
        for (base, words) in [(STATE, &f.state), (SHARED, &f.shared)] {
            for (index, &value) in words.iter().enumerate() { memory.put(base + index as u32 * 4, value); }
        }
        for (address, value) in [(CURRENT, tcb(H)), (CURRENT + CURSOR_OFFSET, 0), (TASK_COUNT, 4),
            (RAM + 20, RADIO_RUNNING), (ISLAND_GUARD_ADDR, GUARD), (ISLAND_STATUS_ADDR, ISLAND_READY),
            (LOG_CURSOR, LOG), (IRQ_BASE, GUARD)]
        { memory.put(address, value); }
        for (index, id) in [H, IDLE, L, M].into_iter().enumerate() { memory.put(TASKS + index as u32 * 4, tcb(id)); }
        for id in [IDLE, L, M, H] {
            let entry = if id == IDLE { f.sched.idle }
                else { tasks.iter().find(|t| t.0 == id).map_or(f.kernel.task_exit, |t| t.1.entry) };
            let abi = if id == IDLE { TaskAbi::Call0 } else { TaskAbi::Windowed };
            let frame = initial_frame(bottom(id), top(id), entry, f.kernel.task_exit, abi, [0; 6]);
            memory.copy(frame.address, &frame.bytes);
            memory.put(bottom(id), GUARD);
            for (offset, value) in [(TCB_FRAME, frame.address), (TCB_STACK_BASE, bottom(id)),
                (TCB_STACK_TOP, top(id)), (TCB_PRIORITY, id),
                (TCB_STATE, if id == IDLE || tasks.iter().any(|t| t.0 == id) { READY } else { DEAD })]
            { memory.put(tcb(id) + offset, value); }
        }
        assert!(memory.bytes(HEAP, HEAP_BYTES as usize).iter().all(|&b| b == 0),
            "SwitchMachine::new must supply the zeroed heap required by objects.splash");
        let mut cpu = Cpu::new(s3_config(), f.kernel.restore);
        cpu.vecbase = VECBASE;
        cpu.traps = TrapMode::Take;
        cpu.window_base = 15; // initial CALL8/ENTRY crosses the physical wrap
        cpu.intenable = 1 << YIELD_LINE;
        cpu.ps = UM | 3;
        cpu.ar[2] = tcb(H);
        Self { cpu, memory, fixture: f, masked: None, starts: 0, spans: 0,
            largest: (0, 0), blocked: Vec::new(), picks: 0 }
    }
    fn step(&mut self) {
        let pc = self.cpu.pc;
        let f = self.fixture;
        assert_ne!(pc, f.kernel.fault_entry, "object task must not fault: cause {}", self.cpu.exccause);
        assert_ne!(pc, f.kernel.stack_fault, "task stack guard survives spills");
        assert_ne!(pc, f.sched.no_task, "idle is always READY");
        if pc == VECBASE + USER_VECTOR_OFFSET as u32 {
            assert_eq!(self.cpu.exccause, cause::LEVEL1_INTERRUPT, "only level-1 scheduling exceptions");
        }
        if pc == f.sched.block {
            let current = self.memory.word(CURRENT);
            let id = if (HEAP..HEAP + HEAP_BYTES).contains(&current) { X } else { (current - TCBS) / TCB_SIZE as u32 };
            let gate = &f.gates[self.masked.expect("block called inside a gate span").0];
            assert!(gate.blocks, "{}: only blocking gates call block", gate.name);
            assert_eq!(self.cpu.ar[2], current, "block receives the running TCB");
            assert_eq!(self.cpu.ar[1], self.memory.word(current + TCB_STACK_TOP) - 64 - 64 - 32,
                "block sees T, with the caller save area restored");
            assert_eq!(self.cpu.ps & (WOE | 15), WOE | 15, "block enters with WOE=1 and INTLEVEL=15");
            assert_eq!(self.memory.word(current + TCB_STATE), BLOCKED, "object published BLOCKED before the check");
            self.blocked.push((id, gate.name.clone()));
        }
        if let Some(gate) = f.gates.iter().find(|g| pc == g.retw) {
            let saved = self.memory.word(self.cpu.ar[1] + 4);
            // Window underflow may update OWB while retrying RETW. All other
            // entry fields, including ISR INTLEVEL=3, must be restored.
            assert_eq!(self.cpu.ps & !(15 << 8), saved & !(15 << 8), "{}: thunk restores entry PS", gate.name);
            assert_ne!(self.cpu.ps & WOE, 0, "{}: thunk returns to a windowed caller", gate.name);
        }
        let before = self.cpu.instret;
        self.cpu.step(&mut self.memory).unwrap_or_else(|stop|
            panic!("object task stopped at {pc:#x}: {stop:?}; PS={:#x}, WB={}, WS={:#x}",
                self.cpu.ps, self.cpu.window_base, self.cpu.window_start));
        if self.cpu.instret == before { return; } // exception entry does not retire the attempted instruction
        if pc == f.sched.pick { self.picks += 1; }
        if let Some(index) = f.gates.iter().position(|g| pc == g.mask) {
            // Inspect runtime stacks before an export can reclaim them.
            for i in 0..self.memory.word(TASK_COUNT) {
                let task = self.memory.word(TASKS + i * 4);
                if (HEAP..HEAP + HEAP_BYTES).contains(&task) {
                    let base = self.memory.word(task + TCB_STACK_BASE);
                    assert_eq!(self.memory.word(base), GUARD, "runtime stack guard before {}", f.gates[index].name);
                }
            }
            assert!(self.masked.replace((index, before)).is_none(), "masked thunk spans cannot nest");
            self.starts += 1;
        }
        if let Some((index, start)) = self.masked {
            let gate = &f.gates[index];
            if pc == gate.restore || pc == f.block_lower {
                let count = self.cpu.instret - start;
                assert!(count <= gate.bound, "{}: masked span {count} exceeds thunk + gate bound {}", gate.name, gate.bound);
                if count > self.largest.0 { self.largest = (count, index); }
                self.spans += 1;
                self.masked = None;
            }
        }
    }
    fn until(&mut self, reason: &str, reached: impl Fn(&Self) -> bool) {
        for _ in 0..200_000 {
            if reached(self) { return; }
            self.step();
        }
        panic!("{reason}: did not reach checkpoint; PC={:#x}, log={:?}", self.cpu.pc, self.log());
    }
    fn log_len(&self) -> usize { ((self.memory.word(LOG_CURSOR) - LOG) / 12) as usize }
    fn log(&self) -> Vec<(u32, u32, u32)> {
        assert!(self.log_len() <= 64, "task log stays inside SRAM allocation");
        (0..self.log_len()).map(|i| {
            let at = LOG + i as u32 * 12;
            (self.memory.word(at), self.memory.word(at + 4), self.memory.word(at + 8))
        }).collect()
    }
    fn finish(&mut self, table: &str) {
        self.until("all table tasks finish", |m| m.cpu.pc == m.fixture.sched.idle && m.memory.word(CURRENT) == tcb(IDLE));
        assert!(self.masked.is_none(), "{table}: no unfinished masked span");
        assert_eq!(self.spans, self.starts, "{table}: every thunk mask has a measured end");
        assert!(self.spans > 0, "{table}: exercised gates");
        for id in [IDLE, L, M, H] { assert_eq!(self.memory.word(bottom(id)), GUARD, "{table}: task {id} stack guard"); }
        assert_eq!(self.memory.word(IRQ_BASE), GUARD, "{table}: interrupt stack guard");
        assert_eq!(heap_check::heap_check(|at| self.memory.word(at), |at| self.memory.bytes(at, 1)[0],
            HEAP, HEAP_BYTES), 0, "{table}: machine heap invariants");
        let gate = &self.fixture.gates[self.largest.1];
        eprintln!("{table}: largest masked thunk span = {} retired instructions ({}; thunk + gate bound {}), {} spans",
            self.largest.0, gate.name, gate.bound, self.spans);
    }
}

#[test]
fn runtime_create_frame_delay_self_delete_and_reap() {
    const ARG: u32 = 0x7654_3210;
    const US: u32 = 19;
    const NOW: u64 = 0x7_ffff_fff0;
    let runtime = task(X, |w| {
        w.log(1); // initial a2 is the argument delivered by ENTRY
        w.call("task_delay", &[Value(US)]); w.log(2);
        w.call("task_delete", &[Value(0)]); w.log(3); // must never return
    });
    let runtime_entry = runtime.entry;
    let bootstrap = task(H, |_| {});
    let low = task(L, |w| {
        w.call("heap_free_bytes", &[]); w.save(HEAP_BEFORE); w.log(0);
        w.call("task_create", &[Value(runtime_entry), Value(SEND), Value(STACK_BYTES),
            Value(ARG), Value(H), Value(HANDLE)]); w.log(1);
        w.call("kernel_expire", &[Value(EXPIRE_OUT)]); w.log(2);
        w.call("task_reap", &[]); w.log(3);
        w.call("heap_free_bytes", &[]); w.save(HEAP_AFTER); w.log(4);
        w.call("task_reap", &[]); w.log(5);
    });
    let mut m = SwitchMachine::new(&[(H, bootstrap), (L, low)]);
    m.memory.code.push((TASK_CODE + X * 0x2000, runtime.bytes.into()));
    m.memory.snapshot = NOW;
    m.until("runtime task reaches its first instruction", |m| m.cpu.pc == runtime_entry);
    let before = m.memory.word(HEAP_BEFORE);
    let runtime_tcb = m.memory.word(HANDLE);
    assert!((HEAP..HEAP + HEAP_BYTES).contains(&runtime_tcb), "runtime TCB allocated in heap");
    assert_eq!(m.memory.word(CURRENT), runtime_tcb);
    assert_eq!(m.memory.word(TASK_COUNT), 5);
    assert_eq!(m.memory.word(TASKS + 4 * 4), runtime_tcb, "new task appended");
    assert_eq!(m.log(), [(L, 0, before)], "X preempts before create returns to L's next step");
    let base = m.memory.word(runtime_tcb + TCB_STACK_BASE);
    let top = m.memory.word(runtime_tcb + TCB_STACK_TOP);
    let frame = initial_frame(base, top, runtime_entry, m.fixture.kernel.task_exit,
        TaskAbi::Windowed, [ARG, 0, 0, 0, 0, 0]);
    assert_eq!(m.memory.word(runtime_tcb + TCB_FRAME), frame.address);
    assert_eq!(m.memory.bytes(frame.address, CONTEXT_SIZE), frame.bytes,
        "runtime context at first run equals every byte of R3 initial_frame");
    assert_eq!(m.memory.word(base), GUARD);
    assert_eq!(m.memory.word(runtime_tcb + TCB_NAME), SEND);
    m.until("X sleeps and L finishes create", |m| m.log_len() == 3);
    assert_eq!(m.log(), [(L, 0, before), (X, 1, ARG), (L, 1, 1)]);
    assert_eq!(m.memory.word(runtime_tcb + TCB_STATE), BLOCKED);
    let deadline = u64::from_le_bytes(m.memory.bytes(runtime_tcb + TCB_DEADLINE, 8).try_into().unwrap());
    assert_eq!(deadline, NOW + u64::from(US) * u64::from(TICKS_PER_US));
    assert_eq!(m.memory.word(base), GUARD, "sleeping stack guard");
    m.memory.snapshot = deadline;
    m.until("X resumes then self-deletes before expire returns", |m| m.log_len() == 5);
    assert_eq!(m.log(), [(L, 0, before), (X, 1, ARG), (L, 1, 1), (X, 2, 1), (L, 2, 0)]);
    assert_eq!(m.memory.word(runtime_tcb + TCB_STATE), BLOCKED, "zombie stays blocked");
    assert_eq!(m.memory.bytes(runtime_tcb + TCB_DEADLINE, 8), &[255; 8], "zombie has forever deadline");
    assert_eq!(m.memory.word(base), GUARD, "zombie stack retained until reap");
    assert_eq!(m.memory.word(TASK_COUNT), 5, "self-deletion did not free the active stack");
    m.finish("runtime create, delay, self-delete and reap");
    assert_eq!(m.log(), [(L, 0, before), (X, 1, ARG), (L, 1, 1), (X, 2, 1),
        (L, 2, 0), (L, 3, 1), (L, 4, before), (L, 5, 0)], "X never logs after deleting itself");
    assert_eq!(m.memory.word(HEAP_AFTER), before, "runtime stack and TCB bytes reclaimed");
    assert_eq!(m.memory.word(TASK_COUNT), 4);
    assert_eq!(m.blocked, [(X, "task_delay".to_owned()), (X, "task_delete".to_owned())]);
    assert_eq!(m.spans, 8, "every lifecycle gate measured, including the nonreturning delete");
}

#[test]
fn runtime_delete_other_unlinks_semaphore_waiter() {
    // X is the runtime high-priority task; the boot dispatcher TCB remains
    // separate and is never passed to task_delete.
    let high = task(X, |w| {
        w.call("sem_take", &[Word(SEM), Value(FOREVER)]); w.log(1); // must never return
    });
    let high_entry = high.entry;
    let bootstrap = task(H, |_| {});
    let low = task(L, |w| {
        w.call("heap_free_bytes", &[]); w.save(HEAP_BEFORE);
        w.call("sem_create", &[Value(2), Value(0)]); w.save(SEM);
        w.call("task_create", &[Value(high_entry), Value(0), Value(STACK_BYTES),
            Value(0), Value(H), Value(HANDLE)]); w.log(1);
        w.call("task_delete", &[Word(HANDLE)]); w.log(2);
        w.call("sem_give", &[Word(SEM)]); w.log(3);
        w.call("sem_count", &[Word(SEM)]); w.log(4);
        w.call("sem_delete", &[Word(SEM)]);
        w.call("heap_free_bytes", &[]); w.save(HEAP_AFTER); w.log(5);
    });
    let mut m = SwitchMachine::new(&[(H, bootstrap), (L, low)]);
    m.memory.code.push((TASK_CODE + X * 0x2000, high.bytes.into()));
    m.until("high task blocks before create returns", |m| m.log_len() == 1);
    let high_tcb = m.memory.word(HANDLE);
    let sem = m.memory.word(SEM);
    let base = m.memory.word(high_tcb + TCB_STACK_BASE);
    assert_eq!(m.log(), [(L, 1, 1)], "blocked high task has not returned");
    assert_eq!(m.memory.word(sem), high_tcb);
    assert_eq!(m.memory.word(high_tcb + TCB_STATE), BLOCKED);
    assert_eq!(m.memory.word(base), GUARD, "waiter stack guard before deletion");
    m.until("delete-other returns", |m| m.log_len() == 2);
    assert_eq!(m.memory.word(sem), 0, "wait list detached before storage is freed");
    assert_eq!(m.memory.word(TASK_COUNT), 4);
    let picks = m.picks;
    m.until("give and count return without waking anything", |m| m.log_len() == 4);
    assert_eq!(m.picks, picks, "give does not wake the deleted task");
    assert_eq!(m.log(), [(L, 1, 1), (L, 2, 1), (L, 3, 1), (L, 4, 1)]);
    m.finish("runtime delete-other");
    let before = m.memory.word(HEAP_BEFORE);
    assert_eq!(m.memory.word(HEAP_AFTER), before, "semaphore, runtime stack and TCB reclaimed");
    assert_eq!(m.log(), [(L, 1, 1), (L, 2, 1), (L, 3, 1), (L, 4, 1), (L, 5, before)]);
    assert_eq!(m.blocked, [(X, "sem_take".to_owned())]);
    assert_eq!(m.spans, 9, "every delete-other gate measured");
}

#[test]
fn semaphore_priority_and_immediate_preemption() {
    let high = task(H, |w| {
        w.call("sem_create", &[Value(2), Value(0)]); w.save(SEM); w.log(1);
        w.call("sem_take", &[Word(SEM), Value(FOREVER)]); w.log(2);
    });
    let middle = task(M, |w| { w.call("sem_take", &[Word(SEM), Value(FOREVER)]); w.log(1); });
    let low = task(L, |w| {
        for step in 1..=3 { w.call("sem_give", &[Word(SEM)]); w.log(step); }
        w.call("sem_count", &[Word(SEM)]); w.log(4);
    });
    let low_entry = low.entry;
    let mut m = SwitchMachine::new(&[(H, high), (M, middle), (L, low)]);
    // OSI's maximum pack priority (24) maps to 25; exercise the real picker
    // and waiter ordering at that value, with the 56-byte TCB stride.
    m.memory.put(tcb(H) + TCB_PRIORITY, 25);
    m.until("both semaphore takers block", |m| m.cpu.pc == low_entry);
    let sem = m.memory.word(SEM);
    assert_ne!(sem, 0, "H created the semaphore through its gate");
    assert_eq!(m.log(), [(H, 1, sem)], "neither blocking take has returned early");
    assert_eq!(m.memory.word(sem), tcb(H), "highest-priority semaphore waiter is first");
    assert_eq!(m.memory.word(tcb(H) + TCB_WAIT_NEXT), tcb(M), "middle priority follows H");
    for id in [H, M] {
        assert_eq!(m.memory.word(tcb(id) + TCB_STATE), BLOCKED, "task {id} waits in BLOCKED");
        assert_eq!(m.memory.bytes(tcb(id) + TCB_DEADLINE, 8), &[255; 8], "task {id} waits forever");
    }
    m.until("second give returns to L", |m| m.log_len() == 5);
    assert_eq!(m.log(), [(H, 1, sem), (H, 2, 1), (L, 1, 1), (M, 1, 1), (L, 2, 1)],
        "each higher-priority waiter runs before the giver's next logged step");
    let picks = m.picks;
    m.until("give without waiter and count return", |m| m.log_len() == 7);
    assert_eq!(m.picks, picks, "give with no waiter must not switch");
    assert_eq!(m.log(), [(H, 1, sem), (H, 2, 1), (L, 1, 1), (M, 1, 1), (L, 2, 1), (L, 3, 1), (L, 4, 1)],
        "no-waiter give returns pdTRUE and increments the count");
    assert_eq!(m.blocked, [(H, "sem_take".to_owned()), (M, "sem_take".to_owned())], "exact blocking calls");
    m.finish("semaphore priority");
    assert_eq!(m.spans, 7, "every semaphore gate call measured");
}

#[test]
fn queue_item_copy_and_mutex_handoff() {
    let high = task(H, |w| {
        w.call("queue_create", &[Value(1), Value(7)]); w.save(QUEUE); w.log(1);
        w.call("mutex_create", &[Value(0)]); w.save(MUTEX); w.log(2);
        w.call("queue_recv", &[Word(QUEUE), Value(RECV), Value(FOREVER)]); w.log(3);
        w.call("mutex_lock", &[Word(MUTEX), Value(FOREVER)]); w.log(4);
        w.a.mov_reg_i32(A3, MUTEX); w.a.l32i(A3, A3, 0); w.a.l32i(A2, A3, 4); w.log(5);
    });
    let low = task(L, |w| {
        w.call("mutex_lock", &[Word(MUTEX), Value(0)]); w.log(1);
        w.call("queue_send_back", &[Word(QUEUE), Value(SEND), Value(FOREVER)]); w.log(2);
        w.call("mutex_unlock", &[Word(MUTEX)]); w.log(3);
        w.call("mutex_unlock", &[Word(MUTEX)]); w.log(4);
        w.call("queue_waiting", &[Word(QUEUE)]); w.log(5);
    });
    let low_entry = low.entry;
    let mut m = SwitchMachine::new(&[(H, high), (L, low)]);
    let item = [0x81, 0, 0x7f, 0x42, 0xa5, 0xfe, 0x19];
    m.memory.copy(SEND, &item);
    m.memory.copy(RECV - 1, &[0xcc; 9]);
    m.until("H blocks receiving from the empty queue", |m| m.cpu.pc == low_entry);
    let queue = m.memory.word(QUEUE);
    let mutex = m.memory.word(MUTEX);
    assert!(queue != 0 && mutex != 0, "objects were created through gates");
    assert_eq!(m.memory.word(queue), tcb(H), "H is linked as the queue receiver");
    assert_eq!(m.memory.word(tcb(H) + TCB_WAIT_ARG), RECV, "receive retains the byte-aligned buffer");
    m.until("send returns after H tries the owned mutex", |m| m.log_len() == 5);
    assert_eq!(m.log(), [(H, 1, queue), (H, 2, mutex), (L, 1, 1), (H, 3, 1), (L, 2, 1)],
        "queue wake preempts the sender and preserves its successful non-blocking result");
    assert_eq!(m.memory.bytes(RECV, item.len()), item, "queue copies every item byte into H's retained buffer");
    assert_eq!((m.memory.bytes(RECV - 1, 1)[0], m.memory.bytes(RECV + 7, 1)[0]), (0xcc, 0xcc),
        "queue copy respects both buffer edges");
    assert_eq!(m.memory.word(mutex + 4), tcb(L), "L owns the mutex until unlock");
    assert_eq!(m.memory.word(mutex), tcb(H), "H is queued on the mutex");
    assert_eq!(m.memory.word(tcb(H) + TCB_STATE), BLOCKED, "H's mutex call has really blocked");
    m.finish("queue copy and mutex handoff");
    assert_eq!(m.log(), [(H, 1, queue), (H, 2, mutex), (L, 1, 1), (H, 3, 1), (L, 2, 1),
        (H, 4, 1), (H, 5, tcb(H)), (L, 3, 1), (L, 4, 0), (L, 5, 0)],
        "handoff publishes H's ownership before waking it; L's second unlock fails");
    assert_eq!(m.memory.word(mutex + 4), tcb(H), "failed unlock leaves H as owner");
    assert_eq!(m.memory.word(tcb(H) + TCB_WAIT_OBJ), 0, "handoff unlinks H");
    assert_eq!(m.blocked, [(H, "queue_recv".to_owned()), (H, "mutex_lock".to_owned())], "only the two waiting calls block");
    assert_eq!(m.spans, 9, "every queue/mutex gate call measured");
}

#[test]
fn finite_timeout_unlinks_and_returns_zero() {
    // Exercise both the product's high word and the low-word carry without
    // waiting in wall time. now64 subtracts the fixture's zero boot origin.
    const TIMEOUT: u32 = 0x1000_0002;
    const NOW: u64 = 0x7_ffff_fff0;
    let high = task(H, |w| {
        w.call("sem_create", &[Value(2), Value(0)]); w.save(SEM); w.log(1);
        w.call("sem_take", &[Word(SEM), Value(TIMEOUT)]); w.log(2);
    });
    let low = task(L, |w| {
        w.a.movi(A2, 0); w.log(0); // host advances snapshot at this checkpoint
        w.call("kernel_expire", &[Value(EXPIRE_OUT)]); w.log(1);
        w.call("sem_give", &[Word(SEM)]); w.log(2);
        w.call("sem_count", &[Word(SEM)]); w.log(3);
    });
    let mut m = SwitchMachine::new(&[(H, high), (L, low)]);
    m.memory.snapshot = NOW;
    m.until("H's finite wait has yielded to L", |m| m.log_len() == 2);
    let sem = m.memory.word(SEM);
    let deadline = u64::from_le_bytes(m.memory.bytes(tcb(H) + TCB_DEADLINE, 8).try_into().unwrap());
    assert_eq!(deadline, NOW + u64::from(TIMEOUT) * u64::from(TICKS_PER_US), "full-width microsecond deadline");
    assert_eq!(m.memory.word(tcb(H) + TCB_STATE), BLOCKED, "finite wait suspends H");
    assert_eq!(m.memory.word(sem), tcb(H), "H remains on the semaphore list until expiry");
    assert_eq!(m.log(), [(H, 1, sem), (L, 0, 0)], "timed take has not returned early");
    assert!(!m.fixture.gate("kernel_expire").blocks, "expiry itself is a non-blocking gate");
    m.memory.snapshot = deadline + 1;
    m.until("expiry returns after preempting to H", |m| m.log_len() == 4);
    assert_eq!(m.log(), [(H, 1, sem), (L, 0, 0), (H, 2, 0), (L, 1, 0)],
        "expiry wakes H with zero before L's next step");
    for offset in [TCB_WAIT_NEXT, TCB_WAIT_PREV, TCB_WAIT_OBJ, TCB_WAIT_ARG, TCB_RESULT] {
        assert_eq!(m.memory.word(tcb(H) + offset), 0, "expired TCB word at {offset} cleared");
    }
    assert_eq!(m.memory.word(sem), 0, "expiry removed H from the wait list");
    assert_eq!(m.memory.bytes(EXPIRE_OUT, 8), &[255; 8], "no finite deadline remains");
    let picks = m.picks;
    m.until("later give and count return", |m| m.log_len() == 6);
    assert_eq!(m.picks, picks, "later give must not wake the expired waiter again");
    assert_eq!(m.memory.word(tcb(H) + TCB_STATE), DEAD, "completed H is not readied by later give");
    m.finish("finite timeout");
    assert_eq!(m.log(), [(H, 1, sem), (L, 0, 0), (H, 2, 0), (L, 1, 0), (L, 2, 1), (L, 3, 1)],
        "later give succeeds and increments the count instead of waking H");
    assert_eq!(m.blocked, [(H, "sem_take".to_owned())], "one finite blocking call");
    assert_eq!(m.spans, 5, "every timeout-table gate call measured");
}

#[test]
fn isr_give_while_current_is_blocked() {
    let high = task(H, |w| {
        w.call("sem_create", &[Value(1), Value(0)]); w.save(SEM); w.log(1);
        w.call("sem_take", &[Word(SEM), Value(FOREVER)]); w.log(2);
    });
    let low = task(L, |w| { w.a.movi(A2, 0); w.log(1); });
    let mut m = SwitchMachine::new(&[(H, high), (L, low)]);
    let base = TASK_CODE + 0x8000;
    let mut entry = 0;
    let bytes = assemble(&mut Asm::new(0, A8), |a| {
        a.literal_table();
        entry = base + a.base.offset();
        a.entry(A1, 64);
        let mut w = TaskWriter { a, task: 4, fixture: m.fixture };
        w.call("sem_give_isr", &[Word(SEM), Value(ISR_WOKEN)]); w.log(1);
        w.a.mov_reg_i32(A3, ISR_DONE); w.a.movi(A2, 1); w.a.s32i(A2, A3, 0);
        w.a.retw_n();
    });
    m.memory.code.push((base, bytes.into()));
    m.memory.put(ISR_COUNT, 1);
    m.memory.put(RAM + 0x600, 1); // line 0 is level 1, distinct from yield
    m.memory.put(RAM + 0x604, entry);
    m.memory.put(ISR_WOKEN, 0xa5a5_a500);
    m.cpu.intenable |= 1;
    m.until("H has enqueued but block has not unmasked", |m| m.cpu.pc == m.fixture.block_lower);
    let sem = m.memory.word(SEM);
    assert_eq!(m.memory.word(CURRENT), tcb(H), "interrupted current is H");
    assert_eq!(m.memory.word(tcb(H) + TCB_STATE), BLOCKED, "ISR will see an interrupted BLOCKED task");
    assert!(!m.fixture.gate("sem_give_isr").blocks, "ISR import has no blocking check");
    m.cpu.set_lines(1);
    m.until("pick ran the island ISR slot", |m| m.memory.word(ISR_DONE) == 1);
    m.cpu.set_lines(0); // deassert the external level before returning from the ISR
    assert_eq!(m.memory.word(CURRENT), tcb(H), "pick has not published a different current during the ISR");
    assert_eq!(m.memory.word(tcb(H) + TCB_STATE), READY, "ISR readies its waiter");
    assert_eq!(m.memory.word(ISR_WOKEN), 0xa5a5_a500,
        "same-priority interrupted task is not a higher-priority wake; adjacent bytes stay intact");
    m.finish("ISR give");
    assert_eq!(m.log(), [(H, 1, sem), (4, 1, 1), (H, 2, 1), (L, 1, 0)],
        "pick runs ISR before selection, then resumes H's blocking gate with success");
    assert_eq!(m.blocked, [(H, "sem_take".to_owned())], "ISR gate never calls block");
    assert_eq!(m.spans, 3, "both task gates and the ISR gate measured");
}

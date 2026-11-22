//! Independent executable ABI scenarios. Handlers come only from emit_s3;
//! no model-test handler listings or S3Machine are used.
use std::collections::BTreeMap;
use std::sync::Arc;

use fab_elec_mcu_emit32::xtensa::{assemble, assemble_relaxed, cc, ccz, enc, reg::*, Asm};
use fab_elec_mcu_emit_s3::radio::*;
use fab_elec_mcu_emit_s3::radio::kernel::{INTR_MATRIX, MODEM_RESET_FIELD, SYSCON_WIFI_RST_EN, WIFI_SOURCES};
use fab_elec_mcu_emu::bus::{Bus, Kind, Memory, Perm, Width};
use fab_elec_mcu_island::{self as island, Archive, ImportMap, Island, IslandLayout, Region, SegmentKind};
use fab_elec_mcu_s3::config::s3_config;
use fab_elec_mcu_xtensa::{insn::{decode, Op}, sim::{cause, Cpu, TrapMode}};

#[allow(dead_code)]
#[path = "../../island/tests/support/mod.rs"]
mod island_elf;

const VECBASE: u32 = 0x4000_0000;
const CODE: u32 = 0x4001_0000;
const RAM: u32 = 0x3ffb_0000;
const RAM_SIZE: usize = 0x8000;
const STACK: u32 = RAM + RAM_SIZE as u32 - 0x100;
const FRAME: u32 = 128;
const ALLOCATION: u32 = 64;
const WOE: u32 = 1 << 18;
const UM: u32 = 1 << 5;
const EXCM: u32 = 1 << 4;
const OWB: u32 = 15 << 8;

#[derive(Clone, Copy)]
struct Edge {
    node: usize,
    n: u8,
    indirect: bool,
}

struct Node {
    tag: u32,
    depth: u32,
    children: Vec<Edge>,
}

impl Node {
    fn live(&self) -> u8 {
        4 * self.children.iter().map(|e| e.n).min().unwrap_or(3)
    }

    fn value(&self, reg: u8) -> u32 {
        self.tag.rotate_left(u32::from(reg)) ^ (0x9e37_79b9u32.wrapping_mul(u32::from(reg)))
    }
}

fn random(seed: &mut u32) -> u32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    *seed
}

// A 25-frame spine guarantees repeated wraps of the 64-register file. Its
// three increments and alternating direct/indirect calls guarantee all six
// call forms; random side branches, tags and phase vary the tree and live sets.
fn tree(nodes: &mut Vec<Node>, seed: &mut u32, depth: u32, remaining: u32, phase: u32) -> usize {
    let id = nodes.len();
    nodes.push(Node { tag: random(seed), depth, children: Vec::new() });
    if remaining != 0 {
        let child = tree(nodes, seed, depth + 1, remaining - 1, phase);
        nodes[id].children.push(Edge {
            node: child,
            n: ((remaining + phase) % 3 + 1) as u8,
            indirect: (remaining + phase) & 1 != 0,
        });
        for _ in 0..random(seed) % 3 {
            let child = tree(nodes, seed, depth + 1, 0, phase);
            nodes[id].children.push(Edge {
                node: child, n: (random(seed) % 3 + 1) as u8,
                indirect: random(seed) & 1 != 0,
            });
        }
    }
    id
}

struct Check {
    node: usize,
    reg: u8,
    expected: u32,
    property: &'static str,
}

#[derive(Clone, Copy)]
struct AllocaSite {
    node: usize,
    n: u8,
    pc: u32,
    after: u32,
}

struct Program {
    bytes: Vec<u8>,
    checks: BTreeMap<u32, Check>,
    entries: Vec<u32>,
    done: u32,
    fail: u32,
    result: u32,
    alloca: Option<AllocaSite>,
    start: u32,
}

fn check(a: &mut Asm, code: u32, checks: &mut BTreeMap<u32, Check>, fail: usize,
    node: usize, reg: u8, scratch: u8, expected: u32, property: &'static str)
{
    assert_ne!(reg, scratch, "comparison must not overwrite its subject");
    a.mov_reg_i32(scratch, expected);
    checks.insert(code + a.base.offset(), Check { node, reg, expected, property });
    a.bcc_reg_reg_label(cc::NE, reg, scratch, fail);
}

fn check_live(a: &mut Asm, code: u32, checks: &mut BTreeMap<u32, Check>, fail: usize,
    nodes: &[Node], id: usize, ra: u32, sp: u32, accumulator: u32)
{
    let node = &nodes[id];
    for reg in 0..node.live() {
        let (expected, property) = match reg {
            0 => (ra, "return address survived"),
            1 => (sp, "stack pointer survived"),
            2 => (accumulator, "accumulator survived"),
            _ => (node.value(reg), "node tag/register survived"),
        };
        check(a, code, checks, fail, id, reg, node.live(), expected, property);
    }
}

fn call(a: &mut Asm, code: u32, edge: Edge) -> u32 {
    if edge.indirect {
        // Loading an absolute label through the literal pool preserves a0.
        // The CALLX source is its own link register, as permitted by §6.1.4
        // (p. 247); it never requests registers above CALLn's window check.
        let target = code + a.base.label_offset(edge.node).unwrap_or(0);
        a.mov_reg_i32(4 * edge.n, target);
        match edge.n {
            1 => a.callx4(A4),
            2 => a.callx8(A8),
            3 => a.callx12(A12),
            _ => unreachable!(),
        }
    } else {
        // Label CALL helpers require a call site at 1 mod 4. Executable
        // three-byte self-moves align it without changing any live value.
        while a.base.offset() & 3 != 1 { a.or(A2, A2, A2); }
        match edge.n {
            1 => a.call4(edge.node),
            2 => a.call8(edge.node),
            3 => a.call12(edge.node),
            _ => unreachable!(),
        }
    }
    (u32::from(edge.n) << 30) | ((code + a.base.offset()) & 0x3fff_ffff)
}

fn compile(nodes: &[Node], alloca_node: Option<(usize, u8)>) -> Program {
    compile_at(nodes, alloca_node, CODE, STACK, None)
}

fn compile_at(nodes: &[Node], alloca_node: Option<(usize, u8)>, code: u32, stack: u32,
    context: Option<&ContextTask>) -> Program
{
    // Nodes are in parent-before-child order. Rust computes the tree sum
    // independently of generated execution, including every partial sum.
    let mut results = vec![0u32; nodes.len()];
    for id in (0..nodes.len()).rev() {
        results[id] = nodes[id].children.iter().fold(nodes[id].tag,
            |sum, edge| sum.wrapping_add(results[edge.node]));
    }
    let done_label = nodes.len();
    let fail_label = nodes.len() + 1;
    let mut program = Program {
        bytes: Vec::new(), checks: BTreeMap::new(), entries: vec![0; nodes.len()],
        done: 0, fail: 0, result: results[0], alloca: None, start: 0,
    };
    let bytes = assemble(&mut Asm::new(nodes.len() + 2, A15), |a| {
        program.checks.clear();
        let mut returns = vec![0; nodes.len()];
        a.literal_table();
        program.start = code + a.base.offset();
        if let Some(task) = context {
            a.entry(A1, FRAME);
            task.initialize(a, program.start);
        }
        // The bootstrap hides only four registers, so its first spill needs
        // no synthetic predecessor/backlink. All later backlinks are written
        // by real overflow handlers into the ABI save areas (pp. 250–253).
        returns[0] = call(a, code, Edge { node: 0, n: 1, indirect: false });
        if let Some(task) = context { task.finish(a); }
        a.base.label_assign(done_label);
        program.done = code + a.base.offset();
        a.j(-4);
        a.base.label_assign(fail_label);
        program.fail = code + a.base.offset();
        a.j(-4);

        for (id, node) in nodes.iter().enumerate() {
            a.base.align(4); // unreachable padding after the previous RETW
            a.base.label_assign(id);
            a.entry(A1, FRAME);
            program.entries[id] = code + a.base.offset();
            let mut sp = stack - node.depth * FRAME;
            let mut accumulator = node.tag;
            a.mov_reg_i32(A2, accumulator);
            for reg in 3..node.live() { a.mov_reg_i32(reg, node.value(reg)); }
            if let Some(task) = context {
                a.s32i(A2, A1, 16); // a real local, outside both ABI save areas
                task.work(a, id, node.live());
            }
            for &edge in &node.children {
                returns[edge.node] = call(a, code, edge);
                check_live(a, code, &mut program.checks, fail_label, nodes, id,
                    returns[id], sp, accumulator);
                check(a, code, &mut program.checks, fail_label, id, 4 * edge.n + 2,
                    node.live(), results[edge.node], "child result in ABI return register");
                a.add_n(A2, A2, 4 * edge.n + 2);
                accumulator = accumulator.wrapping_add(results[edge.node]);
            }
            if let Some((target, n)) = alloca_node {
                if target == id {
                    // Put distinct values in the entire MOVSP window. A2
                    // retains the tree sum; A15 supplies a genuinely new SP.
                    for reg in 3..15 { a.mov_reg_i32(reg, node.value(reg)); }
                    a.mov_reg_i32(A15, sp - ALLOCATION);
                    let pc = code + a.base.offset();
                    a.movsp(A1, A15);
                    program.alloca = Some(AllocaSite { node: id, n, pc, after: code + a.base.offset() });
                    sp -= ALLOCATION;
                }
            }
            check_live(a, code, &mut program.checks, fail_label, nodes, id,
                returns[id], sp, accumulator);
            if context.is_some() {
                a.l32i(node.live(), A1, 16);
                check(a, code, &mut program.checks, fail_label, id, node.live(),
                    node.live() + 1, node.tag, "live stack local survived");
            }
            if id & 1 == 0 { a.retw(); } else { a.retw_n(); }
        }
    });
    program.bytes = bytes;
    program
}

struct Harness {
    cpu: Cpu,
    memory: Memory,
}

impl Harness {
    fn new(program: &Program, initial_base: u32) -> Self {
        let mut memory = Memory::new();
        let vectors = memory.store(0x400);
        let block = window_block();
        let user = user_vector(VECBASE, CODE, VECBASE + USER_UNHANDLED_OFFSET as u32);
        memory.bytes_mut(vectors)[..block.len()].copy_from_slice(&block);
        memory.bytes_mut(vectors)[USER_VECTOR_OFFSET..USER_VECTOR_OFFSET + user.len()].copy_from_slice(&user);
        let rx = Perm { read: true, write: false, exec: true };
        memory.map(VECBASE, vectors, 0, 0x400, rx);
        let code = memory.store(program.bytes.len());
        memory.bytes_mut(code).copy_from_slice(&program.bytes);
        memory.map(CODE, code, 0, program.bytes.len(), rx);
        let ram = memory.store(RAM_SIZE);
        memory.bytes_mut(ram).fill(0xa5);
        memory.map(RAM, ram, 0, RAM_SIZE, Perm { read: true, write: true, exec: false });
        let mut cpu = Cpu::new(s3_config(), CODE);
        cpu.vecbase = VECBASE;
        cpu.ps = WOE | UM | 2;
        cpu.window_base = initial_base;
        cpu.window_start = 1 << initial_base;
        cpu.traps = TrapMode::Take;
        cpu.sar = 23;
        for (reg, value) in cpu.ar.iter_mut().enumerate() { *value = 0xdead_0000 | reg as u32; }
        cpu.ar[1] = STACK;
        Self { cpu, memory }
    }

    fn run(&mut self, program: &Program, mut observe: impl FnMut(&Cpu, Option<usize>)) -> [usize; 6] {
        let initial_base = self.cpu.window_base;
        let mut vectors = [0; 6];
        let mut checks = BTreeMap::<u32, usize>::new();
        // A corrupt handler must fail promptly instead of hanging the suite.
        // These finite trees normally execute only thousands of instructions.
        for _ in 0..100_000 {
            let cpu = &self.cpu;
            let vector = WINDOW_VECTOR_OFFSETS.iter().position(|&offset| cpu.pc == VECBASE + offset as u32);
            if let Some(index) = vector { vectors[index] += 1; }
            observe(cpu, vector);
            if let Some(check) = program.checks.get(&cpu.pc) {
                assert_eq!(cpu.ar[check.reg as usize], check.expected,
                    "node {} a{}: {} at {:#x}", check.node, check.reg, check.property, cpu.pc);
                *checks.entry(cpu.pc).or_default() += 1;
            }
            assert_ne!(cpu.pc, program.fail, "generated register-survival branch failed");
            if cpu.pc == program.done {
                assert_eq!(checks.len(), program.checks.len(), "every Rust/guest register check was reached");
                assert!(checks.values().all(|&count| count == 1), "each register check must run exactly once");
                assert_eq!(cpu.ar[6], program.result, "whole-tree result returned through CALL4's a6");
                assert_eq!(cpu.ar[1], STACK, "whole chain restored bootstrap SP");
                assert_eq!(cpu.window_base, initial_base, "whole chain restored bootstrap WindowBase");
                assert_eq!(cpu.window_start, 1 << initial_base, "whole chain retired all callee windows");
                assert_eq!(cpu.ps & (WOE | UM | EXCM), WOE | UM, "returned with WOE/UM set and EXCM clear");
                assert_eq!(cpu.sar, 23, "handlers preserved SAR");
                return vectors;
            }
            self.cpu.step(&mut self.memory).unwrap_or_else(|stop|
                panic!("unexpected model stop {stop:?} at {:#x}", self.cpu.pc));
        }
        panic!("program did not finish; pc={:#x}, PS={:#x}, WB={}, WS={:#x}",
            self.cpu.pc, self.cpu.ps, self.cpu.window_base, self.cpu.window_start);
    }
}

#[test]
fn window_vectors_run_random_call_trees() {
    for (seed, initial_base) in [(0x1234_5678, 0), (0x9e37_79b9, 5), (0xa341_316c, 15)] {
        let mut rng = seed;
        let mut nodes = Vec::new();
        tree(&mut nodes, &mut rng, 1, 24, seed % 3);
        assert!(nodes.iter().any(|node| node.depth >= 20), "seed {seed:#x}: deep path");
        let mut forms = [false; 6];
        for edge in nodes.iter().flat_map(|node| &node.children) {
            forms[2 * (edge.n as usize - 1) + usize::from(edge.indirect)] = true;
        }
        assert!(forms.into_iter().all(|seen| seen), "seed {seed:#x}: all CALL/CALLX forms");
        let program = compile(&nodes, None);
        let vectors = Harness::new(&program, initial_base).run(&program, |cpu, _| {
            assert_ne!(cpu.pc, VECBASE + USER_VECTOR_OFFSET as u32,
                "ordinary call tree must not raise a general exception, cause {}", cpu.exccause);
        });
        for (i, count) in vectors.into_iter().enumerate() {
            assert!(count != 0, "seed {seed:#x}: vector {:#x} was never entered", WINDOW_VECTOR_OFFSETS[i]);
        }
    }
}

#[test]
fn alloca_reloads_the_spilled_caller() {
    for n in 1..=3u8 {
        let nodes: Vec<_> = (0..25).map(|id| Node {
            tag: 0xfedc_ba98u32.wrapping_mul(id as u32 + 1),
            depth: id as u32 + 1,
            children: if id == 24 { Vec::new() } else { vec![Edge {
                node: id + 1,
                n: if id == 0 { n } else { (id % 3 + 1) as u8 },
                indirect: id & 1 != 0,
            }] },
        }).collect();
        let program = compile(&nodes, Some((1, n)));
        let site = program.alloca.expect("MOVSP site");
        // Both clearing and setting OWB bits, plus backwards WB wraparound.
        for movsp_base in [0u32, 15] {
            let initial_base = movsp_base.wrapping_sub(1 + u32::from(n)) & 15;
            let caller_base = movsp_base.wrapping_sub(u32::from(n)) & 15;
            let mut harness = Harness::new(&program, initial_base);
            let mut caller = None::<Vec<u32>>;
            let mut spilled = false;
            let mut before = None::<([u32; 16], u32, u32)>;
            let mut attempts = 0;
            let mut exceptions = 0;
            let mut completed = false;
            harness.run(&program, |cpu, vector| {
                if cpu.pc == program.entries[site.node] {
                    assert_eq!(cpu.window_base, movsp_base, "CALL{n}: target window location");
                    let physical = cpu.physical_ar();
                    caller = Some((0..4 * usize::from(n))
                        .map(|r| physical[(caller_base as usize * 4 + r) & 63]).collect());
                }
                if vector.is_some_and(|v| v % 2 == 0) && cpu.window_base == caller_base && !spilled {
                    if let Some(saved) = &caller {
                        assert_eq!(vector, Some(2 * (usize::from(n) - 1)), "CALL{n}: matching caller overflow");
                        assert_eq!(&cpu.ar[..saved.len()], saved.as_slice(), "CALL{n}: values before actual spill");
                        spilled = true;
                    }
                }
                if cpu.pc == site.pc {
                    attempts += 1;
                    if let Some((own, ps, ws)) = before {
                        assert_eq!(attempts, 2, "CALL{n}: MOVSP retries exactly once");
                        assert_eq!(cpu.ar, own, "CALL{n}: all 16 MOVSP registers survive recovery");
                        assert_eq!(cpu.window_base, movsp_base, "CALL{n}: RFWU returns to MOVSP window");
                        assert_eq!(cpu.window_start, ws | (1 << caller_base), "CALL{n}: only caller becomes live");
                        assert_eq!(cpu.ps, (ps & !OWB) | (movsp_base << 8), "CALL{n}: only PS.OWB changes");
                        assert_eq!(cpu.sar, 23, "CALL{n}: alloca preserves SAR");
                        assert_eq!(cpu.excsave1, own[0], "CALL{n}: original a0 saved in EXCSAVE1");
                        let physical = cpu.physical_ar();
                        let reloaded: Vec<_> = (0..4 * usize::from(n))
                            .map(|r| physical[(caller_base as usize * 4 + r) & 63]).collect();
                        assert_eq!(reloaded, *caller.as_ref().unwrap(), "CALL{n}: caller restored from its real save areas");
                    } else {
                        assert!(spilled, "CALL{n}: caller really passed through an overflow handler");
                        assert_eq!(cpu.ar[0] >> 30, u32::from(site.n), "CALL{n}: a0 carries the increment under test");
                        for distance in 1..=3 {
                            assert_eq!(cpu.window_start & (1 << (movsp_base.wrapping_sub(distance) & 15)), 0,
                                "CALL{n}: preceding WS bit {distance} must be clear before MOVSP");
                        }
                        assert_ne!((cpu.ps & OWB) >> 8, movsp_base, "CALL{n}: stale OWB exercises replacement");
                        before = Some((cpu.ar, cpu.ps, cpu.window_start));
                    }
                }
                if cpu.pc == VECBASE + USER_VECTOR_OFFSET as u32 {
                    exceptions += 1;
                    assert_eq!(cpu.exccause, cause::ALLOCA, "CALL{n}: cause 5 reached user vector");
                    assert_eq!(cpu.epc1, site.pc, "CALL{n}: EPC1 points at faulting MOVSP");
                    assert_eq!(cpu.ps & (UM | EXCM), UM | EXCM, "CALL{n}: user-mode exception entry");
                }
                if cpu.pc == site.after {
                    let (mut expected, _, _) = before.expect("MOVSP must first fault");
                    expected[1] = expected[15];
                    assert_eq!(cpu.ar, expected, "CALL{n}: MOVSP changes only a1 to its source value");
                    assert_eq!(cpu.ar[1], STACK - nodes[site.node].depth * FRAME - ALLOCATION,
                        "CALL{n}: new SP has the requested allocation");
                    completed = true;
                }
            });
            assert_eq!(exceptions, 1, "CALL{n}: exactly one alloca exception at WB {movsp_base}");
            assert_eq!(attempts, 2, "CALL{n}: fault and successful retry at WB {movsp_base}");
            assert!(completed, "CALL{n}: MOVSP completed and whole chain returned");
        }
    }
}

#[test]
fn vector_sizes_and_padding() {
    let block = window_block();
    let irq_entry = CODE;
    let fault_entry = CODE + 0x100;
    let user = user_vector(VECBASE, irq_entry, fault_entry);
    assert_eq!(block.len(), 0x300, "reserved window block size");
    assert_eq!(user.len(), USER_VECTOR_SIZE, "user dispatcher size");
    assert!(user.len() <= 0x80, "user dispatcher fits reserved slot");
    let config = s3_config();
    assert_eq!(config.vectors.window, WINDOW_VECTOR_OFFSETS.map(|offset| offset as u32), "S3 window vector placement");
    assert_eq!(config.vectors.user, USER_VECTOR_OFFSET as u32, "S3 user vector placement");
    let mut used = [false; WINDOW_BLOCK_SIZE];
    for (i, &start) in WINDOW_VECTOR_OFFSETS.iter().enumerate() {
        let mut at = start;
        loop {
            assert!(at + 3 <= start + WINDOW_VECTOR_SIZE, "vector {start:#x}: no return inside slot");
            let bits = u32::from_le_bytes([block[at], block[at + 1], block[at + 2], 0]);
            let (insn, len) = decode(bits).expect("window handler instruction decodes");
            at += len as usize;
            if matches!(insn.op, Op::Rfwo | Op::Rfwu) {
                assert_eq!(insn.op, if i % 2 == 0 { Op::Rfwo } else { Op::Rfwu }, "vector {start:#x}: return kind");
                break;
            }
        }
        assert_eq!(at - start, WINDOW_HANDLER_SIZES[i], "vector {start:#x}: actual handler size");
        assert!(at - start <= 64, "vector {start:#x}: handler fits 64 bytes");
        used[start..at].fill(true);
    }
    assert!(ALLOCA_OFFSET >= 0x180 && ALLOCA_OFFSET + ALLOCA_SIZE <= 0x300, "alloca placement");
    used[ALLOCA_OFFSET..ALLOCA_OFFSET + ALLOCA_SIZE].fill(true);
    for (offset, (&byte, used)) in block.iter().zip(used).enumerate() {
        assert!(used || byte == 0, "unused block byte at {offset:#x} must be zero");
    }

    // Cause 4 dispatches with just a0 saved; other non-alloca causes fault.
    // No stack is needed for either path.
    let mut memory = Memory::new();
    let store = memory.store(user.len());
    memory.bytes_mut(store).copy_from_slice(&user);
    memory.map(VECBASE + USER_VECTOR_OFFSET as u32, store, 0, user.len(),
        Perm { read: true, write: false, exec: true });
    for cause in (0..64).filter(|&cause| cause != 5) {
        let mut cpu = Cpu::new(config, VECBASE + USER_VECTOR_OFFSET as u32);
        cpu.ps = WOE | UM | EXCM;
        cpu.exccause = cause;
        cpu.ar = std::array::from_fn(|r| 0xabcd_0000 | r as u32);
        let saved = cpu.ar;
        let destination = if cause == 4 { irq_entry } else { fault_entry };
        for _ in 0..12 {
            if cpu.pc == destination { break; }
            cpu.step(&mut memory).expect("dispatch must neither fault nor access a stack");
        }
        assert_eq!(cpu.pc, destination, "cause {cause}: interrupt or fault entry");
        assert_eq!(cpu.excsave1, saved[0], "cause {cause}: a0 saved");
        assert_eq!(&cpu.ar[1..], &saved[1..], "cause {cause}: other registers untouched");
        assert_eq!(cpu.exccause, cause, "cause {cause}: fault cause available to future path");
    }
}

const KERNEL: u32 = VECBASE + 0x8000;
const B_CODE: u32 = CODE + 0x4000;
const PICK: u32 = CODE + 0x8000;
const CURRENT: u32 = RAM;
const TCB_A: u32 = RAM + 0x20;
const TCB_B: u32 = RAM + 0x40;
const A_RESULT: u32 = RAM + 0x100;
const B_RESULT: u32 = RAM + 0x200;
const A_WORK: u32 = RAM + 0x300;
const B_WORK: u32 = RAM + 0x340;
const A_STACK_BASE: u32 = RAM + 0x1000;
const A_STACK_TOP: u32 = RAM + 0x4000;
const B_STACK_BASE: u32 = A_STACK_TOP;
const B_STACK_TOP: u32 = RAM + 0x7000;
const IRQ_TOP: u32 = RAM + RAM_SIZE as u32;
const SOFT_IRQ: u32 = 1 << 7;
const FAULT_STATE: u32 = RAM + 0x14;
const TRACE_HEAD: u32 = RAM + 0x18;
const ISLAND_GUARD_ADDR: u32 = RAM + 0x1c;
const FAULT_TASKS: u32 = RAM + 0x540;
const FAULT_RECORD: u32 = RAM + 0x600;
const ISLAND_STATUS_ADDR: u32 = FAULT_RECORD - 4;
const TRACE_RING: u32 = RAM + 0xa00;
const IDLE_TCB: u32 = RAM + 0x60;

fn switch_layout(pick: u32, trace_entries: u32) -> RadioLayout {
    let shim = shim_code(SHIM_BASE);
    RadioLayout {
        current: CURRENT, irq_stack_base: B_STACK_TOP, irq_stack_top: IRQ_TOP, pick,
        tasks: FAULT_TASKS, task_count: RAM + 8, dispatcher: TCB_B, idle: IDLE_TCB,
        isr_count: RAM + 12, wake_lines: 6, vector_lines: 0, shim_start: SHIM_BASE,
        shim_end: SHIM_BASE + shim.bytes.len() as u32, radio_state: FAULT_STATE,
        island_guard: ISLAND_GUARD_ADDR, island_status: ISLAND_STATUS_ADDR,
        fault: FAULT_RECORD, trace_head: TRACE_HEAD, trace: TRACE_RING, trace_entries,
    }
}

fn read_sr(a: &mut Asm, reg: u8, sr: u32) { a.op24(enc::rsr(0, 3, 0, sr, reg)); }
fn read_ur(a: &mut Asm, reg: u8, ur: u8) {
    a.op24(enc::rrr(0, 3, 14, reg, ur >> 4, ur & 15));
}
fn write_ur(a: &mut Asm, reg: u8, ur: u32) { a.op24(enc::rsr(0, 3, 15, ur, reg)); }

struct ContextTask { result: u32, work: u32, other: bool }

impl ContextTask {
    fn initialize(&self, a: &mut Asm, entry: u32) {
        // Every ordinary AR is used; a0/a1 were set by the synthetic CALL8
        // and real ENTRY. The deep tree subsequently changes physical windows.
        for reg in 2..16 { a.mov_reg_i32(reg, 0xcafe_0000 | (u32::from(self.other) << 8) | u32::from(reg)); }
        a.s32i(A2, A1, 16); // wrapper's live application local
        for (sr, value) in [
            (3, if self.other { 7 } else { 23 }), (4, if self.other { 0xaa55 } else { 0x55aa }),
            (0, entry), (1, PICK + if self.other { 6 } else { 3 }), (2, if self.other { 13 } else { 7 }),
            (12, if self.other { 0x8765_4321 } else { 0x1234_5678 }),
            (16, if self.other { 0xaabb_ccdd } else { 0x1020_3040 }),
            (17, if self.other { 0x7f } else { 0xffff_ff80 }),
            (32, 0x1234 + u32::from(self.other)), (33, 0x4567 + u32::from(self.other)),
            (34, 0x89ab + u32::from(self.other)), (35, 0xcdef + u32::from(self.other)),
        ] {
            a.mov_reg_i32(A2, value);
            a.wsr(A2, sr);
        }
        a.isync(); // loop writes, pp. 278–280; LEND is outside task code
        for (ur, value) in [(231, self.work), (232, if self.other { 1 } else { 2 }),
            (233, if self.other { 0x100 } else { 0x80 })]
        {
            a.mov_reg_i32(A2, value);
            write_ur(a, A2, ur);
        }
        for reg in 0..16 {
            a.mov_reg_i32(A2, (if self.other { 0xbf80_0000 } else { 0x3f80_0000 }) + u32::from(reg) * 0x80000);
            a.wfr(reg, A2);
        }
    }

    fn work(&self, a: &mut Asm, node: usize, scratch: u8) {
        // Exact, finite binary FP values, plus a real SAR-dependent result.
        a.add_s(0, 0, 1);
        a.movi(scratch, node as i32 + if self.other { 17 } else { 1 });
        a.ssr(scratch);
        a.srl(scratch, A2);
        a.mov_reg_i32(scratch + 1, self.work);
        a.s32i(scratch, scratch + 1, node as u32 * 4);
    }

    fn finish(&self, a: &mut Asm) {
        a.mov_reg_i32(A4, self.result);
        a.s32i(A6, A4, 0); // whole tree's CALL4 return value
        let mut offset = 4;
        for sr in [3, 4, 0, 1, 2, 12, 16, 17, 32, 33, 34, 35, 224] {
            read_sr(a, A3, sr);
            a.s32i(A3, A4, offset);
            offset += 4;
        }
        for ur in [231, 232, 233] {
            read_ur(a, A3, ur);
            a.s32i(A3, A4, offset);
            offset += 4;
        }
        for reg in 0..16 {
            a.rfr(A3, reg);
            a.s32i(A3, A4, offset);
            offset += 4;
        }
        // Define otherwise-dead exit ARs so the entire visible register file
        // has a meaningful differential comparison. Live tree ARs/locals
        // have already been checked at each return, including every caller.
        for reg in 2..16 {
            a.mov_reg_i32(reg, (if self.other { 0xbeef_0000 } else { 0xface_0000 }) | u32::from(reg));
        }
        if self.other {
            a.movi(A3, 0);
            a.wsr(A3, 224); // switch-out must save FP even with CP0 disabled
            a.rsync();
            a.movi(A3, SOFT_IRQ as i32);
            a.wsr(A3, 226);
            a.rsync();
        }
    }
}

// Immutable executable images are shared across clones; each boundary copies
// only CPU state and 32 KiB SRAM. No changes to the emulator's Memory API.
#[derive(Clone)]
struct SwitchMemory {
    code: Arc<Vec<(u32, Vec<u8>)>>,
    ram: Vec<u8>,
    // Reset register followed by the four routes. Start with unrelated reset bits and live routes.
    wifi: [u32; 1 + WIFI_SOURCES as usize],
}

const ROUTED_WIFI: [u32; 5] = [1 << 30, 0, 0, 3, 3];

impl SwitchMemory {
    fn wifi_word(address: u32) -> Option<usize> {
        if address == SYSCON_WIFI_RST_EN { return Some(0); }
        let off = address.checked_sub(INTR_MATRIX)?;
        (off < WIFI_SOURCES * 4 && off & 3 == 0).then_some(1 + (off / 4) as usize)
    }
    fn bytes(&self, address: u32, len: usize) -> &[u8] {
        let offset = (address - RAM) as usize;
        &self.ram[offset..offset + len]
    }
    fn word(&self, address: u32) -> u32 {
        u32::from_le_bytes(self.bytes(address, 4).try_into().unwrap())
    }
    fn put(&mut self, address: u32, value: u32) {
        let offset = (address - RAM) as usize;
        self.ram[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn install(&mut self, frame: &InitialFrame) {
        let offset = (frame.address - RAM) as usize;
        self.ram[offset..offset + frame.bytes.len()].copy_from_slice(&frame.bytes);
    }
}

impl Bus for SwitchMemory {
    fn read(&mut self, address: u32, width: Width, kind: Kind, _: u64) -> Result<u32, ()> {
        if let Some(index) = Self::wifi_word(address) {
            return (width == Width::W && kind == Kind::Load).then_some(self.wifi[index]).ok_or(());
        }
        let len = width.bytes();
        let mut value = 0;
        let bytes = if address >= RAM && u64::from(address) + len as u64 <= u64::from(IRQ_TOP) {
            if kind == Kind::Fetch { return Err(()); }
            self.bytes(address, len)
        } else {
            if kind == Kind::Store { return Err(()); }
            let (base, code) = self.code.iter().find(|(base, code)|
                address >= *base && u64::from(address) + len as u64 <= u64::from(*base) + code.len() as u64)
                .ok_or(())?;
            &code[(address - *base) as usize..(address - *base) as usize + len]
        };
        for (i, &byte) in bytes.iter().enumerate() { value |= u32::from(byte) << (8 * i); }
        Ok(value)
    }
    fn write(&mut self, address: u32, width: Width, value: u32, _: u64) -> Result<(), ()> {
        if let Some(index) = Self::wifi_word(address) {
            if width != Width::W { return Err(()); }
            self.wifi[index] = value;
            return Ok(());
        }
        let len = width.bytes();
        if address < RAM || u64::from(address) + len as u64 > u64::from(IRQ_TOP) { return Err(()); }
        let offset = (address - RAM) as usize;
        self.ram[offset..offset + len].copy_from_slice(&value.to_le_bytes()[..len]);
        Ok(())
    }
}

#[derive(Clone)]
struct SwitchMachine { cpu: Cpu, memory: SwitchMemory }

impl SwitchMachine {
    fn step(&mut self) {
        let pc = self.cpu.pc;
        self.cpu.step(&mut self.memory).unwrap_or_else(|stop|
            panic!("context execution stopped at {pc:#x}: {stop:?}; PS={:#x}, WB={}, WS={:#x}",
                self.cpu.ps, self.cpu.window_base, self.cpu.window_start));
    }
    fn start(&mut self, kernel: &KernelCode, tcb: u32, target: u32) {
        self.cpu.pc = kernel.restore;
        self.cpu.ps = UM | 3;
        self.cpu.ar[2] = tcb;
        self.memory.put(CURRENT, tcb);
        for _ in 0..256 {
            if self.cpu.pc == target { return; }
            if self.cpu.pc == kernel.stack_fault - 3 {
                assert_eq!(self.cpu.window_start, 1 << self.cpu.window_base, "initial switch-in has one live window");
            }
            self.step();
        }
        panic!("initial restore failed to reach {target:#x}");
    }
}

fn picker() -> Vec<u8> {
    assemble(&mut Asm::new(0, A3), |a| {
        // PICK+3 really is executed, with EXCM=0. The first instruction's
        // sequential successor equals task LEND, exercising the model's
        // precise pre-instruction loop stop if the save forgot to clear it.
        a.addi(A1, A1, -16);
        a.s32i(A0, A1, 0);
        a.movi(A0, 17);
        for reg in 3..16 { a.movi(reg, 0x100 + i32::from(reg)); }
        a.movi(A3, (TCB_A ^ TCB_B) as i32);
        a.xor(A2, A2, A3);
        a.movi(A3, SOFT_IRQ as i32);
        a.wsr(A3, 227);
        a.rsync(); // INTCLEAR, p. 304
        a.movi(A3, 0);
        a.wsr(A3, 224);
        a.rsync();
        a.l32i(A0, A1, 0);
        a.addi(A1, A1, 16);
        a.ret_n();
    })
}

fn switch_fixture() -> (SwitchMachine, KernelCode, Program, Program) {
    switch_fixture_trace(0)
}

fn switch_fixture_trace(trace_entries: u32) -> (SwitchMachine, KernelCode, Program, Program) {
    let layout = switch_layout(PICK, trace_entries);
    let kernel = kernel_code(KERNEL, VECBASE, &layout);
    let make_tree = |seed: u32| -> Vec<Node> {
        // Eight real frames, all six call forms, plus the CALL8 root and
        // wrapper. The leaf uses through a13, crossing the 16-group file;
        // actual overflow and underflow are asserted below.
        (0..8).map(|id| Node {
            tag: seed.wrapping_mul(id as u32 + 1), depth: id as u32 + 1,
            children: if id == 7 { Vec::new() } else { vec![Edge {
                node: id + 1, n: (id % 3 + 1) as u8, indirect: id & 1 != 0,
            }] },
        }).collect()
    };
    let a = compile_at(&make_tree(0x9e37_79b9), None, CODE, A_STACK_TOP - 64 - FRAME,
        Some(&ContextTask { result: A_RESULT, work: A_WORK, other: false }));
    let b = compile_at(&make_tree(0x1234_5678), None, B_CODE, B_STACK_TOP - 64 - FRAME,
        Some(&ContextTask { result: B_RESULT, work: B_WORK, other: true }));
    let mut vectors = vec![0; 0x400];
    vectors[..WINDOW_BLOCK_SIZE].copy_from_slice(&window_block());
    let user = user_vector(VECBASE, kernel.irq_entry, kernel.fault_entry);
    vectors[USER_VECTOR_OFFSET..USER_VECTOR_OFFSET + user.len()].copy_from_slice(&user);
    let memory = SwitchMemory {
        code: Arc::new(vec![(VECBASE, vectors), (KERNEL, kernel.bytes.clone()),
            (CODE, a.bytes.clone()), (B_CODE, b.bytes.clone()), (PICK, picker())]),
        ram: vec![0xa5; RAM_SIZE], wifi: ROUTED_WIFI,
    };
    let config = s3_config();
    assert_eq!(config.int_type[7], fab_elec_mcu_xtensa::sim::IntType::Software, "S3 interrupt 7 is software");
    assert_eq!(config.int_level[7], 1, "S3 software interrupt 7 is level 1");
    let mut machine = SwitchMachine { cpu: Cpu::new(config, kernel.restore), memory };
    for word in 0..FAULT_WORDS { machine.memory.put(FAULT_RECORD + word * 4, 0); }
    machine.memory.put(FAULT_RECORD + STOP_PENDING_OFFSET, 0);
    for word in 0..trace_entries * TRACE_ENTRY_WORDS { machine.memory.put(TRACE_RING + word * 4, 0); }
    for (address, value) in [(ISLAND_GUARD_ADDR, GUARD), (ISLAND_STATUS_ADDR, ISLAND_READY),
        (FAULT_STATE, RADIO_RUNNING), (TRACE_HEAD, 0), (layout.task_count, 3),
        (layout.isr_count, 0), (FAULT_TASKS, TCB_B), (FAULT_TASKS + 4, IDLE_TCB),
        (FAULT_TASKS + 8, TCB_A), (TCB_A + TCB_STATE, READY), (TCB_B + TCB_STATE, READY),
        (IDLE_TCB + TCB_STATE, READY)]
    { machine.memory.put(address, value); }
    machine.cpu.vecbase = VECBASE;
    machine.cpu.traps = TrapMode::Take;
    machine.cpu.intenable = SOFT_IRQ;
    machine.cpu.window_base = 15; // initial CALL8 also crosses physical wrap
    for (tcb, bottom, top, entry, exit) in [(TCB_A, A_STACK_BASE, A_STACK_TOP, a.start, a.done),
        (TCB_B, B_STACK_BASE, B_STACK_TOP, b.start, b.done)]
    {
        let frame = initial_frame(bottom, top, entry, exit, TaskAbi::Windowed, [0; 6]);
        machine.memory.install(&frame);
        machine.memory.put(bottom, GUARD);
        machine.memory.put(tcb + TCB_FRAME, frame.address);
        machine.memory.put(tcb + TCB_STACK_BASE, bottom);
        machine.memory.put(tcb + TCB_STACK_TOP, top);
    }
    machine.start(&kernel, TCB_A, a.start);
    (machine, kernel, a, b)
}

fn observe_program(cpu: &Cpu, program: &Program, seen: &mut BTreeMap<u32, usize>) {
    assert_ne!(cpu.pc, program.fail, "tree's guest check failed at {:#x}", cpu.pc);
    if let Some(check) = program.checks.get(&cpu.pc) {
        assert_eq!(cpu.ar[check.reg as usize], check.expected,
            "node {} a{}: {} at {:#x}", check.node, check.reg, check.property, cpu.pc);
        *seen.entry(cpu.pc).or_default() += 1;
    }
}

fn assert_task_state(actual: &Cpu, expected: &Cpu, boundary: usize) {
    assert_eq!(actual.pc, expected.pc, "boundary {boundary}: resumed PC");
    assert_eq!(actual.ar, expected.ar, "boundary {boundary}: all visible ARs");
    assert_eq!(actual.fr, expected.fr, "boundary {boundary}: all FP words");
    assert_eq!((actual.sar, actual.br, actual.lbeg, actual.lend, actual.lcount),
        (expected.sar, expected.br, expected.lbeg, expected.lend, expected.lcount),
        "boundary {boundary}: shift, Boolean and loop state");
    assert_eq!((actual.acclo, actual.acchi, actual.m, actual.scompare1, actual.threadptr),
        (expected.acclo, expected.acchi, expected.m, expected.scompare1, expected.threadptr),
        "boundary {boundary}: MAC16, Conditional Store and Thread Pointer state");
    assert_eq!((actual.cpenable, actual.fcr, actual.fsr, actual.fsr_underflow_unknown),
        (expected.cpenable, expected.fcr, expected.fsr, expected.fsr_underflow_unknown),
        "boundary {boundary}: coprocessor enable, FP control and status");
    // OWB describes the last exception, and physical WB/WS can legitimately
    // differ after extra spills/underflows. They are fault metadata, not task
    // values. CALLINC and all other PS fields must agree.
    assert_eq!(actual.ps & !OWB, expected.ps & !OWB, "boundary {boundary}: task PS fields");
}

#[test]
fn interrupt_at_every_boundary_switches_and_returns_exactly() {
    let (mut straight, kernel, a, b) = switch_fixture();
    let mut boundaries = Vec::new();
    let mut straight_checks = BTreeMap::new();
    let start = straight.cpu.instret;
    let mut straight_windows = [0usize; 6];
    for _ in 0..2000 {
        observe_program(&straight.cpu, &a, &mut straight_checks);
        if straight.cpu.pc == a.done {
            boundaries.push(straight.clone()); // include the boundary at the marked exit
            break;
        }
        assert_ne!(straight.cpu.pc, VECBASE + USER_VECTOR_OFFSET as u32, "straight run has no general exception");
        if let Some(v) = WINDOW_VECTOR_OFFSETS.iter().position(|&v| straight.cpu.pc == VECBASE + v as u32) {
            straight_windows[v] += 1;
        }
        boundaries.push(straight.clone());
        straight.step();
    }
    assert_eq!(straight.cpu.pc, a.done, "straight task reaches its marked exit");
    assert!(straight.cpu.instret - start <= 1000, "straight task stays within 1000 retired instructions");
    assert_eq!(straight_checks.len(), a.checks.len(), "straight run reaches every tree/local check");
    assert!(straight_windows.iter().step_by(2).sum::<usize>() > 0, "straight run really overflows");
    assert!(straight_windows.iter().skip(1).step_by(2).sum::<usize>() > 0, "straight run really underflows");
    assert_eq!(straight.memory.word(A_RESULT), a.result, "straight tree's independently calculated result");
    assert_eq!((straight.cpu.lbeg, straight.cpu.lend, straight.cpu.lcount), (a.start, PICK + 3, 7),
        "nonzero loop state remains unchanged at exit");

    // Application-owned data, guard and the still-live wrapper local.
    // Dead frames, ABI spill areas and interrupt contexts are deliberately
    // excluded: forcing extra spills changes their bytes by definition.
    let owned = [(A_RESULT, 160), (A_WORK, 64), (A_STACK_BASE, 4),
        (A_STACK_TOP - 64 - FRAME + 16, 4)];
    assert!(interrupt_boundary_sweep(&straight, &kernel, &a, &b, boundaries, &owned, |_, _| {}),
        "nonzero-LCOUNT run must execute the picker instruction at LEND");
}

// Count every contiguous masked stretch, including RSIL itself, window-check
// retries, and the WSR/RFE that unmasks. Kernel-only accounting below retains
// its original assertions; this also catches masking in task code and thunks.
#[derive(Default)]
struct MaskedInstructions {
    attempts: u64,
    retired: u64,
    thunk: bool,
    largest_attempts: u64,
    largest_retired: u64,
    largest_thunk_attempts: u64,
    largest_thunk_retired: u64,
}

impl MaskedInstructions {
    fn step(&mut self, machine: &mut SwitchMachine) {
        let masked = machine.cpu.interrupt_level() != 0;
        let thunk = machine.cpu.ps & 15 == 15;
        let retired = machine.cpu.instret;
        machine.step();
        let still_masked = machine.cpu.interrupt_level() != 0;
        if masked || still_masked {
            self.attempts += 1;
            self.retired += machine.cpu.instret - retired;
            self.thunk |= thunk || machine.cpu.ps & 15 == 15;
            self.largest_attempts = self.largest_attempts.max(self.attempts);
            self.largest_retired = self.largest_retired.max(self.retired);
            if self.thunk {
                self.largest_thunk_attempts = self.largest_thunk_attempts.max(self.attempts);
                self.largest_thunk_retired = self.largest_thunk_retired.max(self.retired);
            }
        }
        if !still_masked {
            self.attempts = 0;
            self.retired = 0;
            self.thunk = false;
        }
    }
}

// Both proofs use exactly the same interrupt, context, task B and latency
// assertions. The observer adds scenario checks at every attempted boundary.
fn interrupt_boundary_sweep(straight: &SwitchMachine, kernel: &KernelCode,
    a: &Program, b: &Program, boundaries: Vec<SwitchMachine>, owned: &[(u32, usize)],
    mut observe: impl FnMut(&SwitchMachine, usize)) -> bool
{
    // Fixed save/restore/vector path <256 instructions; picker <=32.
    // At most 15 occupied groups: OF4/8/12 cost 5/10/14 instructions for
    // 1/2/3 groups; charging one retry per frame bounds spills by 15*6=90.
    // Thus 384 covers 256+32+90=378, including failed WindowCheck attempts.
    // Also enforce this bound on complete masked stretches in task code.
    const MASKED_BOUND: u64 = 384;
    let mut largest_masked = 0;
    let mut largest_retired = 0;
    let mut largest_thunk_attempts = 0;
    let mut largest_thunk_retired = 0;
    let mut hazard_executed = false;
    for (boundary, mut machine) in boundaries.into_iter().enumerate() {
        let mut masked = MaskedInstructions::default();
        machine.cpu.interrupt |= SOFT_IRQ;
        let mut entries = 0;
        let mut switches = 0;
        let mut saved_a = None;
        let mut masked_start = None;
        let mut masked_attempts = 0;
        let mut a_checks = BTreeMap::new();
        let mut b_checks = BTreeMap::new();
        let mut b_windows = [0usize; 6];
        let mut b_ran = false;
        for _ in 0..6000 {
            observe(&machine, boundary);
            observe_program(&machine.cpu, a, &mut a_checks);
            observe_program(&machine.cpu, b, &mut b_checks);
            assert_ne!(machine.cpu.pc, kernel.stack_fault, "boundary {boundary}: valid guards must not fault");
            assert_ne!(machine.cpu.pc, VECBASE + USER_UNHANDLED_OFFSET as u32,
                "boundary {boundary}: no unhandled cause");
            if machine.cpu.pc == a.done && switches == 2 { break; }
            if machine.cpu.pc == b.start { b_ran = true; }
            if machine.memory.word(CURRENT) == TCB_B && masked_start.is_none() {
                if let Some(v) = WINDOW_VECTOR_OFFSETS.iter().position(|&v| machine.cpu.pc == VECBASE + v as u32) {
                    b_windows[v] += 1;
                }
            }
            if machine.cpu.pc == VECBASE + USER_VECTOR_OFFSET as u32 {
                entries += 1;
                assert_eq!(machine.cpu.exccause, cause::LEVEL1_INTERRUPT,
                    "boundary {boundary}: each switch has one cause-4 entry");
                assert!(masked_start.is_none(), "boundary {boundary}: no nested general exception");
                masked_start = Some(machine.cpu.instret);
                masked_attempts = 0;
            }
            if masked_start.is_some() {
                assert!(machine.cpu.interrupt_level() >= 3, "boundary {boundary}: interrupt mask has no gap");
                assert!(machine.cpu.lcount == 0 || machine.cpu.ps & EXCM != 0,
                    "boundary {boundary}: kernel may hold a live loop count only with EXCM set");
                masked_attempts += 1;
                if machine.cpu.pc == PICK + 3 {
                    assert_eq!(machine.cpu.ps & EXCM, 0, "picker really executes outside exception mode");
                    assert_eq!(machine.cpu.lcount, 0, "picker cannot inherit a task's live loop count");
                }
            }
            if machine.cpu.pc == kernel.stack_fault - 3 { // RFE
                assert_eq!(machine.cpu.window_start, 1 << machine.cpu.window_base,
                    "boundary {boundary}: each switch-in has exactly one live window");
                let since = masked_start.take().expect("RFE belongs to an interrupt switch");
                let retired = machine.cpu.instret - since + 1;
                largest_retired = largest_retired.max(retired);
                largest_masked = largest_masked.max(masked_attempts);
                assert!(retired <= masked_attempts, "instruction-attempt count includes exception retries");
                assert!(largest_masked < MASKED_BOUND,
                    "largest masked instruction counts: {largest_retired} retired, {largest_masked} attempted; bound {MASKED_BOUND}, boundary {boundary}");
                switches += 1;
                masked.step(&mut machine);
                if machine.memory.word(CURRENT) == TCB_A {
                    let saved = saved_a.as_ref().expect("A must first have been interrupted");
                    assert_task_state(&machine.cpu, saved, boundary);
                    assert_eq!(machine.cpu.ps, saved.ps, "boundary {boundary}: PS restored before resumed instruction");
                }
                continue;
            }
            // Capture the actual interrupted instruction, including requests
            // injected into a window handler and deferred until its RFWO/RFWU.
            if saved_a.is_none() && machine.cpu.interrupt & SOFT_IRQ != 0 && machine.cpu.interrupt_level() == 0 {
                saved_a = Some(machine.cpu.clone());
            }
            let hazard_site = machine.cpu.pc == PICK + 3
                && saved_a.as_ref().is_some_and(|cpu: &Cpu| cpu.lcount != 0);
            let before = machine.cpu.instret;
            masked.step(&mut machine);
            if hazard_site {
                assert_eq!(machine.cpu.instret, before + 1, "instruction at task LEND really retired in kernel code");
                hazard_executed = true;
            }
        }
        assert_eq!((entries, switches), (2, 2), "boundary {boundary}: exactly one general exception per switch");
        assert!(b_ran, "boundary {boundary}: B actually ran");
        assert_eq!(b_checks.len(), b.checks.len(), "boundary {boundary}: all B tree/local checks ran");
        assert!(b_checks.values().all(|&n| n == 1), "boundary {boundary}: B checked each site once");
        assert!(b_windows.iter().step_by(2).sum::<usize>() > 0, "boundary {boundary}: B overflowed");
        assert!(b_windows.iter().skip(1).step_by(2).sum::<usize>() > 0, "boundary {boundary}: B underflowed");
        assert_eq!(machine.memory.word(B_RESULT), b.result, "boundary {boundary}: B tree result");
        assert_task_state(&machine.cpu, &straight.cpu, boundary);
        for &(address, len) in owned {
            assert_eq!(machine.memory.bytes(address, len), straight.memory.bytes(address, len),
                "boundary {boundary}: A-owned memory at {address:#x}");
        }
        assert_eq!(machine.memory.word(B_STACK_BASE), GUARD, "boundary {boundary}: B's guard survives");
        largest_masked = largest_masked.max(masked.largest_attempts);
        largest_retired = largest_retired.max(masked.largest_retired);
        largest_thunk_attempts = largest_thunk_attempts.max(masked.largest_thunk_attempts);
        largest_thunk_retired = largest_thunk_retired.max(masked.largest_thunk_retired);
    }
    eprintln!("largest thunk masked instruction counts: {largest_thunk_retired} retired, {largest_thunk_attempts} attempted (not cycles)");
    eprintln!("largest masked instruction counts: {largest_retired} retired, {largest_masked} attempted; bound {MASKED_BOUND} attempts (not cycles)");
    assert!(largest_masked < MASKED_BOUND,
        "largest masked instruction counts: {largest_retired} retired, {largest_masked} attempted; bound {MASKED_BOUND}");
    hazard_executed
}

const SHIM_BASE: u32 = CODE + 0xa000;
const EXPORT_BASE: u32 = CODE + 0xb000;
const HOST_MIX: u32 = CODE + 0xc000;
const HOST_RESULT: u32 = RAM + 0x380;
const ISLAND_DRAM: u32 = RAM + 0x400;
const DEPTH_N: u32 = 12;
const DEPTH_X: u32 = 0xffff_fff0;
const DEPTH_Y: u32 = 0x89ab_cdef;
const DEPTH_SCALE: u32 = 0x9e37_79b9;
const LEAF_MAGIC: u32 = 0xa5c3_7e19;
const COUNTER_INITIAL: u32 = 0xffff_fff8;
const UNUSED_ARGS: [u32; 2] = [0x1357_9bdf, 0x2468_ace0];
const SHIM_SENTINELS: [u32; 4] = [0x1234_5678, 0x9abc_def0, 0x0fed_cba9, 0x8765_4321];

fn host_mix_mirror(v: u32, magic: u32) -> (u32, [u32; 4]) {
    let mut r8 = v ^ magic;
    let mut r9 = v.wrapping_add(magic);
    let mut r10 = r8.wrapping_add(0x31);
    let mut r11 = r9 ^ r10;
    let r12 = r10.wrapping_add(r11);
    let r13 = r11 ^ magic;
    let r14 = r12.wrapping_add(r13);
    let r15 = r14 ^ v;
    for _ in 0..3 {
        r8 = r8.wrapping_add(r15);
        r9 ^= r8;
        r10 = r10.wrapping_add(r9);
        r11 ^= r10;
    }
    let words = [(r12 ^ r8) & 0xffff, r13.wrapping_add(r9) | 1,
        r14 ^ r10, r15.wrapping_add(r11)];
    ((words[0].wrapping_add(words[1]) ^ words[2]).wrapping_add(words[3]), words)
}

// The entry jump skips the assembler's literal table. All executable body
// instructions are CALL0, including the prologue/epilogue's ordinary ADDIs.
fn host_mix_code() -> (Vec<u8>, u32) {
    let mut late_store = 0;
    let bytes = assemble(&mut Asm::new(0, A8), |a| {
        a.literal_table();
        a.addi(A1, A1, -32);
        for reg in 12..16 { a.s32i(reg, A1, u32::from(reg - 12) * 4); }
        read_sr(a, A8, 230);
        // OWB is window-exception metadata and may differ across the sweep.
        // Record every other PS bit, including WOE and all of INTLEVEL.
        a.mov_reg_i32(A9, !OWB);
        a.and(A8, A8, A9);
        a.mov_reg_i32(A9, HOST_RESULT);
        a.s32i(A8, A9, 0);

        a.xor(A8, A2, A3);
        a.add_n(A9, A2, A3);
        a.addi(A10, A8, 0x31);
        a.xor(A11, A9, A10);
        a.add_n(A12, A10, A11);
        a.xor(A13, A11, A3);
        a.add_n(A14, A12, A13);
        a.xor(A15, A14, A2);
        for _ in 0..3 {
            a.add_n(A8, A8, A15);
            a.xor(A9, A9, A8);
            a.add_n(A10, A10, A9);
            a.xor(A11, A11, A10);
        }
        a.xor(A12, A12, A8);
        a.extui(A12, A12, 0, 16); // cannot be mistaken for a windowed return link
        a.add_n(A13, A13, A9);
        a.movi(A8, 1);
        a.or(A13, A13, A8); // cannot be mistaken for an aligned caller SP
        a.xor(A14, A14, A10);
        a.add_n(A15, A15, A11);
        late_store = HOST_MIX + a.base.offset();
        // Over a dozen body instructions precede these stores. Without the
        // thunk's -16, these are T-16..T: an earlier interrupt's leaf spill.
        for reg in 12..16 { a.s32i(reg, A1, 16 + u32::from(reg - 12) * 4); }
        for reg in 8..12 { a.l32i(reg, A1, 16 + u32::from(reg - 8) * 4); }
        a.add_n(A2, A8, A9);
        a.xor(A2, A2, A10);
        a.add_n(A2, A2, A11);
        a.mov_reg_i32(A3, HOST_RESULT);
        for reg in 8..12 { a.s32i(reg, A3, 4 + u32::from(reg - 8) * 4); }
        a.s32i(A2, A3, 20);
        for reg in 12..16 { a.l32i(reg, A1, u32::from(reg - 12) * 4); }
        a.addi(A1, A1, 32);
        a.ret_n();
    });
    (bytes, late_store)
}

// Return both the independently computed result and the leaf's input. Every
// addition and shift has explicit ELF32/ISA wrapping semantics.
fn depth_mirror(n: u32, x: u32, y: u32) -> (u32, u32) {
    if n == 0 {
        let v = x ^ y;
        (host_mix_mirror(v, LEAF_MAGIC).0, v)
    } else {
        let (r, leaf) = depth_mirror(n.wrapping_sub(1), x.wrapping_add(n), y ^ DEPTH_SCALE);
        ((r ^ x).wrapping_add(n.wrapping_shl(3)).wrapping_add(y), leaf)
    }
}

fn island_literal(code: &mut Vec<u8>, slots: &mut Vec<(u32, u32)>, reg: u8, literal: u32) -> u32 {
    let pc = code.len() as u32;
    island_elf::emit(code, island_elf::l32r(reg, pc, literal));
    slots.push((pc, literal));
    pc
}

// Only our std-only fixture encoder supplies archive bytes. No vendor object
// is linked or executed. Literal/instruction properties and all PC-relative
// relocations have the same shapes as island_elf::pack().
fn depth_island(gate: u32) -> Island {
    assert!(gate <= 1);
    use island_elf::{emit, Object};
    let mut depth = vec![0; 12]; // &counter, &leaf, &scale
    let mut depth_slots = Vec::new();
    emit(&mut depth, enc::entry(A1, 48));
    island_literal(&mut depth, &mut depth_slots, A5, 0);
    emit(&mut depth, enc::l32i(A6, A5, 0));
    emit(&mut depth, enc::addi(A6, A6, 1));
    emit(&mut depth, enc::s32i(A6, A5, 0));
    let branch = depth.len() as u32;
    emit(&mut depth, island_elf::beqz(A2, branch, branch + 8)); // patched below

    emit(&mut depth, enc::addi(A10, A2, -1));
    emit(&mut depth, u32::from(enc::add_n(A11, A3, A2)));
    island_literal(&mut depth, &mut depth_slots, A5, 8);
    emit(&mut depth, enc::l32i(A5, A5, 0));
    emit(&mut depth, enc::xor(A12, A4, A5));
    let recurse = depth.len() as u32;
    emit(&mut depth, island_elf::call(8, recurse, 12));
    // Use n/x/y from the restored a2/a3/a4, never copies in stack locals.
    emit(&mut depth, enc::xor(A10, A10, A3));
    emit(&mut depth, u32::from(enc::add_n(A5, A2, A2)));
    for _ in 0..2 { emit(&mut depth, u32::from(enc::add_n(A5, A5, A5))); }
    emit(&mut depth, u32::from(enc::add_n(A10, A10, A5)));
    emit(&mut depth, u32::from(enc::add_n(A2, A10, A4)));
    emit(&mut depth, island_elf::retwn());

    let base_case = depth.len() as u32;
    depth[branch as usize..branch as usize + 3]
        .copy_from_slice(&island_elf::beqz(A2, branch, base_case).to_le_bytes()[..3]);
    depth_slots.push((branch, base_case));
    emit(&mut depth, enc::xor(A10, A3, A4));
    let long_call = island_literal(&mut depth, &mut depth_slots, A8, 4);
    emit(&mut depth, island_elf::callx(8, A8));
    emit(&mut depth, u32::from(enc::mov_n(A2, A10)));
    emit(&mut depth, island_elf::retwn());
    let depth_size = depth.len() as u32;

    let mut leaf = vec![0; 12]; // &leaf_seen (COMMON), &magic, &host_mix
    let mut leaf_slots = Vec::new();
    emit(&mut leaf, enc::entry(A1, 48)); // CALL8 extra save area
    island_literal(&mut leaf, &mut leaf_slots, A3, 0);
    emit(&mut leaf, enc::s32i(A2, A3, 0));
    island_literal(&mut leaf, &mut leaf_slots, A3, 4);
    emit(&mut leaf, enc::l32i(A11, A3, 0));
    emit(&mut leaf, u32::from(enc::mov_n(A10, A2)));
    let import_call = island_literal(&mut leaf, &mut leaf_slots, A8, 8);
    emit(&mut leaf, island_elf::callx(8, A8));
    emit(&mut leaf, u32::from(enc::mov_n(A2, A10)));
    emit(&mut leaf, island_elf::retwn());
    let leaf_size = leaf.len() as u32;

    let mut object = Object::new();
    let text = object.section(".text.depth", 1, 6, depth);
    let iram = object.section(".iram1.leaf", 1, 6, leaf);
    let data = object.section(".data.depth", 1, 3, COUNTER_INITIAL.to_le_bytes().to_vec());
    let scale = object.section(".rodata.depth", 1, 2, DEPTH_SCALE.to_le_bytes().to_vec());
    let magic = object.section(".rodata.leaf", 1, 2, LEAF_MAGIC.to_le_bytes().to_vec());
    let depth = object.symbol("depth", 1, 2, text as u16, 12, depth_size - 12);
    let leaf = object.symbol("leaf", 1, 2, iram as u16, 12, leaf_size - 12);
    let host_mix = object.symbol("host_mix", 1, 0, 0, 0, 0);
    let counter = object.symbol("counter", 1, 1, data as u16, 0, 4);
    let scale = object.symbol("scale", 1, 1, scale as u16, 0, 4);
    let magic = object.symbol("magic", 1, 1, magic as u16, 0, 4);
    let seen = object.symbol("leaf_seen", 1, 1, 0xfff2, 4, 4);
    let text_symbol = object.section_symbol(text);
    let iram_symbol = object.section_symbol(iram);
    for (offset, symbol) in [(0, counter), (4, leaf), (8, scale)] {
        object.relocation(text, offset, 1, symbol, 0);
    }
    for (offset, symbol) in [(0, seen), (4, magic), (8, host_mix)] {
        object.relocation(iram, offset, 1, symbol, 0);
    }
    for (offset, target) in depth_slots { object.relocation(text, offset, 20, text_symbol, target as i32); }
    for (offset, target) in leaf_slots { object.relocation(iram, offset, 20, iram_symbol, target as i32); }
    object.relocation(text, recurse, 20, depth, 0);
    object.relocation(text, long_call, 11, leaf, 0);
    object.relocation(iram, import_call, 11, host_mix, 0);
    object.property(text, 0, 12, 1);
    object.property(text, 12, depth_size - 12, 2);
    object.property(iram, 0, 12, 1);
    object.property(iram, 12, leaf_size - 12, 2);

    let bytes = island_elf::archive(&[("depth.o", object.encode())]);
    let archives = [Archive { name: "depth.a", data: &bytes }];
    let layout = IslandLayout {
        mmu_page: 0, flash_bytes: 0x1_0000,
        window: Region { start: ISLAND_DRAM, bytes: 0x100 }, window_exec: 0x4037_8000,
    };
    let imports = if gate == 0 { "gate 0 host_mix" } else { "gate 0 host_unused\ngate 1 host_mix" };
    island::link(&archives, &["depth".into()], &ImportMap::parse(imports).unwrap(), &[], &layout)
        .expect("synthetic recursive island passes the audit and links")
}

fn island_address(island: &Island, name: &str) -> u32 {
    island.map.lines().find_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields[3] == name {
            Some(u32::from_str_radix(&fields[0][2..], 16).expect("map address is hex"))
        } else { None }
    }).unwrap_or_else(|| panic!("island map contains {name}"))
}

// One record is {id, arg0, arg1, arg2}; unused arguments are zero. Scripts,
// log and gate table are disjoint from the windowed frames at the top of RAM.
const PHY_SCRIPT: u32 = RAM + 0x100; // step, due, result, out[4], glue entry
const PHY_LOG_HEAD: u32 = RAM + 0x140;
const PHY_LOG: u32 = RAM + 0x200;
const PHY_GATE_TABLE: u32 = RAM + 0x20;
const PHY_OUT_WORDS: [u32; 4] = [0x3ffb_5000, 0x3ffb_5100, 2, 0x3ffb_5900];

fn phy_record(a: &mut Asm, id: u32, args: usize) {
    // Literal 0 is &log_head in both kinds of fixture routine. a2..a4 are
    // their ABI arguments; CALL0's a12..a15 and link a0 remain untouched.
    a.op_l32r(A5, a.base.offset(), 0);
    a.l32i(A6, A5, 0);
    a.movi(A7, id as i32);
    a.s32i(A7, A6, 0);
    a.movi(A7, 0);
    for index in 0..3 {
        a.s32i(if index < args { A2 + index as u8 } else { A7 }, A6, 4 + index as u32 * 4);
    }
    a.addi(A6, A6, 16);
    a.s32i(A6, A5, 0);
}

fn phy_scripted_gate(id: usize) -> Vec<u8> {
    let mut bytes = assemble(&mut Asm::new(0, A8), |a| {
        a.base.reserve(8); // log_head, script; CALL0 entry at +8
        phy_record(a, id as u32, if id == 0 { 0 } else { 2 });
        a.op_l32r(A5, a.base.offset(), 4);
        match id {
            1 => {
                for i in 0..4 {
                    a.l32i(A6, A5, 12 + i * 4);
                    a.s32i(A6, A3, i * 4);
                }
                a.l32i(A2, A5, 0); // phy_enter's step
            }
            2 => a.l32i(A2, A5, 4), // phy_init_done's due
            _ => a.movi(A2, 0),
        }
        a.ret_n();
    });
    bytes[..4].copy_from_slice(&PHY_LOG_HEAD.to_le_bytes());
    bytes[4..8].copy_from_slice(&PHY_SCRIPT.to_le_bytes());
    bytes
}

fn phy_standins() -> Vec<u8> {
    use phy_glue::PHY_PACK_SYMBOLS;
    let mut object = island_elf::Object::new();
    // Ten nested calls force the glue and its caller to spill and reload.
    // This helper has no log entry and makes no PHY or host call.
    let helper = assemble_relaxed(&mut Asm::new(1, A8), |a| {
        a.base.reserve(4); // relocated self entry
        a.entry(A1, 64);
        a.bccz_reg_label(ccz::EQ, A2, 0);
        a.addi(A10, A2, -1);
        a.op_l32r(A8, a.base.offset(), 0);
        a.callx8(A8);
        a.base.label_assign(0);
        a.retw_n();
    });
    let size = helper.len() as u32;
    let section = object.section(".text.phy_spill", 1, 6, helper);
    let helper = object.symbol("phy_fixture_spill", 1, 2, section as u16, 4, size - 4);
    object.relocation(section, 0, 1, helper, 0);
    object.property(section, 0, 4, 1);
    object.property(section, 4, size - 4, 2);
    for (id, name) in PHY_PACK_SYMBOLS.iter().enumerate() {
        let args = match *name {
            "register_chipv7_phy" => 3,
            "phy_dig_reg_backup" | "phy_param_track_tot" => 2,
            "phy_init_param_set" | "phy_bbpll_en_usb" | "phy_wifi_enable_set" => 1,
            _ => 0,
        };
        let mut bytes = assemble(&mut Asm::new(0, A8), |a| {
            a.base.reserve(12); // log_head, result word, helper entry
            a.entry(A1, 64);
            phy_record(a, 4 + id as u32, args);
            a.movi(A10, 10);
            a.op_l32r(A8, a.base.offset(), 8);
            a.callx8(A8);
            if *name == "register_chipv7_phy" {
                a.op_l32r(A5, a.base.offset(), 4);
                a.l32i(A2, A5, 0);
            } else { a.movi(A2, 0); }
            a.retw_n();
        });
        bytes[..4].copy_from_slice(&PHY_LOG_HEAD.to_le_bytes());
        bytes[4..8].copy_from_slice(&(PHY_SCRIPT + 8).to_le_bytes());
        let size = bytes.len() as u32;
        let section = object.section(&format!(".text.phy_standin_{id}"), 1, 6, bytes);
        object.symbol(name, 1, 2, section as u16, 12, size - 12);
        object.relocation(section, 8, 1, helper, 0);
        object.property(section, 0, 12, 1);
        object.property(section, 12, size - 12, 2);
    }
    island_elf::archive(&[("phy_fixture.o", object.encode())])
}

/// Glue and stand-ins are our own code; no Espressif bytes execute. As in
/// host_mix_code, CALL0 host routines sit behind real exports(...) thunks.
/// All four use blocks:false: the blocking protocol is covered by adapter.rs
/// and kernel_tasks.rs, and a resumed phy_lock is just a returning call here.
/// Together with 2d-2's kernel tests this checks Fable's falsifier: phy_enter
/// sets CLK_COMMON before every RF init/wake/shutdown call. The disable
/// wrapper's phy_wifi_enable_set(0) deliberately precedes the lock and enter.
#[test]
fn phy_glue_call_order_through_linked_island_and_gates() {
    use kernel::{PHY_FIRST, PHY_LAST, PHY_NOT_LAST, PHY_ON, PHY_OP_DISABLE, PHY_OP_ENABLE,
        PHY_SKIP, PHY_WAKE, PHY_WAKE_RESTORE};
    let phy = phy_glue::phy_glue();
    let archive = phy_standins();
    let mut map = String::new();
    for (index, name) in phy.gates.iter().enumerate() { map.push_str(&format!("gate {index} {name}\n")); }
    for object in &phy.objects { map.push_str(&format!("glue {}\n", object.name)); }
    let island = island::link(&[Archive { name: "phy_fixture.a", data: &archive }], &[],
        &ImportMap::parse(&map).unwrap(), &phy.objects, &IslandLayout {
            mmu_page: 0, flash_bytes: 0x1_0000,
            window: Region { start: PHY_GATE_TABLE, bytes: PHY_SCRIPT - PHY_GATE_TABLE }, window_exec: 0x4037_8000,
        }).expect("PHY glue retains all stand-ins through its literal relocations and links");
    for name in phy.pack_symbols { island_address(&island, name); }
    let entries: Vec<_> = phy.objects.iter().map(|object| {
        let entry = island.glue[&object.name];
        assert_eq!(island::symbolize(&island.map, entry + 3),
            Some((object.name.as_str(), object.entry + 3)), "PC within {} symbolizes to glue", object.name);
        eprintln!("{}: {} bytes, {} literals, entry +{}", object.name, object.bytes.len(),
            object.relocations.len(), object.entry);
        entry
    }).collect();

    let gates: Vec<_> = phy.gates.iter().enumerate().map(|(i, &name)| Gate {
        name, call: HOST_MIX + i as u32 * 0x100 + 8, blocks: false, exit_level: None, export: 0,
    }).collect();
    let exported = exports(EXPORT_BASE, &[], &gates, None, None);
    assert_eq!(exported.abi_hash, island.header.abi_hash, "PHY gate order agrees with the linked island");
    let mut done = 0;
    let driver = assemble(&mut Asm::new(1, A8), |a| {
        a.literal_table();
        // CALL4 bootstrap needs only its base save area; all subsequent
        // backlinks are made by real window handlers, with no SRAM patching.
        call(a, CODE, Edge { node: 0, n: 1, indirect: true });
        done = CODE + a.base.offset();
        a.j(-4);
        a.base.align(4);
        a.base.label_assign(0);
        a.entry(A1, 64);
        a.mov_reg_i32(A8, PHY_SCRIPT);
        a.l32i(A8, A8, 28);
        a.callx8(A8);
        a.retw_n();
    });
    let mut code = vec![(VECBASE, window_block().to_vec()), (CODE, driver), (EXPORT_BASE, exported.bytes)];
    for i in 0..4 { code.push((HOST_MIX + i as u32 * 0x100, phy_scripted_gate(i))); }
    for segment in &island.segments {
        assert!(matches!(segment.kind, SegmentKind::Text | SegmentKind::Iram), "PHY fixture has only executable segments");
        code.push((segment.address, island.image[segment.file_offset as usize..][..segment.bytes as usize].to_vec()));
    }
    // A fetch at the final RETW.N may read a full word.
    for (_, bytes) in &mut code { bytes.extend_from_slice(&[0; 4]); }
    let mut fixture = SwitchMachine { cpu: Cpu::new(s3_config(), CODE),
        memory: SwitchMemory { code: Arc::new(code), ram: vec![0; RAM_SIZE], wifi: ROUTED_WIFI } };
    fixture.cpu.vecbase = VECBASE;
    fixture.cpu.ps = WOE | UM;
    fixture.cpu.window_base = 15;
    fixture.cpu.window_start = 1 << 15;
    fixture.cpu.traps = TrapMode::Take;
    fixture.cpu.ar[1] = STACK;
    fixture.memory.put(PHY_LOG_HEAD, PHY_LOG);
    for (i, &word) in PHY_OUT_WORDS.iter().enumerate() { fixture.memory.put(PHY_SCRIPT + 12 + i as u32 * 4, word); }
    for (i, &thunk) in exported.thunks.iter().enumerate() { fixture.memory.put(island.header.gate_table + i as u32 * 4, thunk); }

    // Independent expected call logs, using only each function's ABI arguments.
    let record = |name: &str, args: &[u32]| {
        let id = phy.gates.iter().chain(phy.pack_symbols.iter()).position(|&n| n == name).expect("fixture callee");
        let mut words = [id as u32, 0, 0, 0];
        words[1..1 + args.len()].copy_from_slice(args);
        words
    };
    let (init, cal, mode, regs) = (PHY_OUT_WORDS[0], PHY_OUT_WORDS[1], PHY_OUT_WORDS[2], PHY_OUT_WORDS[3]);
    let out = STACK - 64 - 64; // caller frame, then glue's frame; out at offset 0
    let lock = record("phy_lock", &[]);
    let enter_en = record("phy_enter", &[PHY_OP_ENABLE, out]);
    let enter_dis = record("phy_enter", &[PHY_OP_DISABLE, out]);
    let wifi_on = record("phy_wifi_enable_set", &[1]);
    let wifi_off = record("phy_wifi_enable_set", &[0]);
    let wake = record("phy_wakeup_init", &[]);
    let track = record("phy_param_track_tot", &[1, 0]);
    let result = 0x1234_5678;
    let cases = [
        ("FIRST, due, failed calibration", 0, PHY_FIRST, 1, vec![lock, enter_en,
            record("phy_init_param_set", &[1]), record("phy_bbpll_en_usb", &[1]),
            record("register_chipv7_phy", &[init, cal, mode]), record("phy_init_done", &[PHY_FIRST, result]),
            track, record("phy_leave", &[PHY_OP_ENABLE, PHY_FIRST]), wifi_on]),
        ("WAKE, not due", 0, PHY_WAKE, 0, vec![lock, enter_en, wake,
            record("phy_init_done", &[PHY_WAKE, 0]), record("phy_leave", &[PHY_OP_ENABLE, PHY_WAKE]), wifi_on]),
        ("WAKE_RESTORE, due", 0, PHY_WAKE_RESTORE, 1, vec![lock, enter_en, wake,
            record("phy_dig_reg_backup", &[0, regs]), record("phy_init_done", &[PHY_WAKE_RESTORE, 0]),
            track, record("phy_leave", &[PHY_OP_ENABLE, PHY_WAKE_RESTORE]), wifi_on]),
        ("ON, not due", 0, PHY_ON, 0, vec![lock, enter_en, record("phy_init_done", &[PHY_ON, 0]),
            record("phy_leave", &[PHY_OP_ENABLE, PHY_ON]), wifi_on]),
        ("enable SKIP", 0, PHY_SKIP, 1, vec![lock, enter_en]),
        ("unknown enable step", 0, 0x8000_0007, 0, vec![lock, enter_en,
            record("phy_init_done", &[0x8000_0007, 0]), record("phy_leave", &[PHY_OP_ENABLE, 0x8000_0007]), wifi_on]),
        ("LAST", 1, PHY_LAST, 0, vec![wifi_off, lock, enter_dis, record("phy_dig_reg_backup", &[1, regs]),
            record("phy_close_rf", &[]), record("phy_xpd_tsens", &[]), record("phy_wait_freq_hw_hop_done", &[]),
            record("phy_leave", &[PHY_OP_DISABLE, PHY_LAST])]),
        ("NOT_LAST", 1, PHY_NOT_LAST, 0, vec![wifi_off, lock, enter_dis,
            record("phy_leave", &[PHY_OP_DISABLE, PHY_NOT_LAST])]),
        ("disable SKIP", 1, PHY_SKIP, 0, vec![wifi_off, lock, enter_dis]),
        ("unknown disable step", 1, 0xffff_ffff, 0, vec![wifi_off, lock, enter_dis,
            record("phy_leave", &[PHY_OP_DISABLE, 0xffff_ffff])]),
    ];
    for (name, entry, step, due, expected) in cases {
        let mut machine = fixture.clone();
        for (offset, value) in [(0, step), (4, due), (8, result), (28, entries[entry])] {
            machine.memory.put(PHY_SCRIPT + offset, value);
        }
        let mut windows = [0; 6];
        for _ in 0..20_000 {
            if machine.cpu.pc == done { break; }
            if let Some(i) = WINDOW_VECTOR_OFFSETS.iter().position(|&off| machine.cpu.pc == VECBASE + off as u32) {
                windows[i] += 1;
            }
            if gates.iter().any(|gate| gate.call == machine.cpu.pc) {
                assert_eq!(machine.cpu.ps & 15, 15, "{name}: scripted gate runs masked through its thunk");
            }
            machine.step();
        }
        assert_eq!(machine.cpu.pc, done, "{name}: glue returns within fixture bound");
        assert_eq!(machine.cpu.ar[1], STACK, "{name}: both windowed frames unwind");
        assert_eq!(machine.cpu.ps & (WOE | UM | EXCM | 15), WOE | UM, "{name}: gate restores caller PS");
        let end = machine.memory.word(PHY_LOG_HEAD);
        assert!((PHY_LOG..=PHY_LOG + 32 * 16).contains(&end) && (end - PHY_LOG) % 16 == 0,
            "{name}: log stays within its DRAM allocation");
        let actual: Vec<[u32; 4]> = (PHY_LOG..end).step_by(16).map(|address|
            std::array::from_fn(|i| machine.memory.word(address + i as u32 * 4))).collect();
        assert_eq!(actual, expected, "{name}: complete call order, out words, calibration result, due and wrapper flag order");
        if name != "enable SKIP" {
            assert!(windows[2] != 0 && windows[3] != 0, "{name}: CALL8 spills and reloads exercise glue's frame");
        }
    }
}

fn shim_driver(entry: u32, shim: u32) -> Program {
    let mut program = Program {
        bytes: Vec::new(), checks: BTreeMap::new(), entries: vec![0],
        done: 0, fail: 0, result: depth_mirror(DEPTH_N, DEPTH_X, DEPTH_Y).0,
        alloca: None, start: 0,
    };
    program.bytes = assemble(&mut Asm::new(0, A8), |a| {
        a.literal_table();
        program.start = CODE + a.base.offset();
        for (i, value) in SHIM_SENTINELS.into_iter().enumerate() {
            a.mov_reg_i32(A12 + i as u8, value);
        }
        for (i, value) in [entry, DEPTH_N, DEPTH_X, DEPTH_Y, UNUSED_ARGS[0], UNUSED_ARGS[1]]
            .into_iter().enumerate()
        { a.mov_reg_i32(A2 + i as u8, value); }
        read_sr(a, A8, 230);
        a.mov_reg_i32(A9, A_RESULT);
        a.s32i(A8, A9, 20); // PS immediately before CALLX0; CALL0 leaves it alone
        a.mov_reg_i32(A8, shim);
        a.callx0(A8);
        program.entries[0] = CODE + a.base.offset(); // first instruction after the shim
        a.mov_reg_i32(A9, A_RESULT);
        a.s32i(A2, A9, 0);
        for reg in 12..16 { a.s32i(reg, A9, 4 + u32::from(reg - 12) * 4); }
        read_sr(a, A8, 230);
        a.s32i(A8, A9, 24);
        // Define dead caller-saved ARs for the shared full-context comparison.
        // The return observer checks a0/a1/a2/a12..a15 before these writes.
        for reg in 3..12 { a.movi(reg, 0x200 + i32::from(reg)); }
        program.done = CODE + a.base.offset();
        a.j(-4);
        program.fail = CODE + a.base.offset();
        a.j(-4);
    });
    program
}

struct ThunkSites { entry: u32, sync: u32, late_store: u32 }

fn shim_fixture_trace(trace_entries: u32) -> (SwitchMachine, KernelCode, Program, Program, Island, u32, ThunkSites) {
    shim_fixture_gate(trace_entries, 0)
}

fn shim_fixture_gate(trace_entries: u32, gate: u32) -> (SwitchMachine, KernelCode, Program, Program, Island, u32, ThunkSites) {
    // Reuse the kernel, picker, pattern-filled RAM and unchanged task B.
    // The original A has only been restored; none of its instructions ran.
    let (mut machine, kernel, _, b) = switch_fixture_trace(trace_entries);
    let island = depth_island(gate);
    let shim = shim_code(SHIM_BASE);
    let gates = if gate == 0 { vec![Gate { name: "host_mix", call: HOST_MIX, blocks: false, exit_level: None, export: 0 }] }
        else { vec![Gate { name: "host_unused", call: HOST_MIX, blocks: false, exit_level: None, export: 0 },
            Gate { name: "host_mix", call: HOST_MIX, blocks: false, exit_level: None, export: 0 }] };
    let exported = exports(EXPORT_BASE, &["depth"], &gates,
        (trace_entries != 0).then_some(kernel.trace_record), None);
    assert_eq!(exported.abi_hash, island.header.abi_hash, "runtime and linker share the ABI hash");
    assert_eq!(exported.thunks.len(), island.header.gate_count as usize);
    let (host, late_store) = host_mix_code();
    let mut thunk = ThunkSites { entry: exported.thunks[gate as usize], sync: 0, late_store };
    let mut offset = (thunk.entry - EXPORT_BASE) as usize;
    while offset < exported.bytes.len() {
        let mut bytes = [0; 4];
        let len = (exported.bytes.len() - offset).min(3);
        bytes[..len].copy_from_slice(&exported.bytes[offset..offset + len]);
        let (insn, len) = decode(u32::from_le_bytes(bytes)).expect("thunk instruction decodes");
        if insn.op == Op::Rsync { thunk.sync = EXPORT_BASE + offset as u32; }
        offset += len as usize;
    }
    let a = shim_driver(island.entries[0], shim.call);
    let mut offset = (shim.call - SHIM_BASE) as usize;
    let mut instructions = 0;
    let mut restore_sync = 0;
    while offset < shim.bytes.len() {
        let mut bytes = [0; 4];
        let len = (shim.bytes.len() - offset).min(3);
        bytes[..len].copy_from_slice(&shim.bytes[offset..offset + len]);
        let (insn, len) = decode(u32::from_le_bytes(bytes)).expect("shim instruction decodes");
        if insn.op == Op::Rsync { restore_sync = SHIM_BASE + offset as u32; }
        offset += len as usize;
        instructions += 1;
    }
    assert_eq!(instructions, 30, "shim CALL0 entry through RET.N instruction count");
    assert_ne!(restore_sync, 0, "shim has a PS restore synchronization boundary");
    let code = Arc::make_mut(&mut machine.memory.code);
    code.iter_mut().find(|(base, _)| *base == CODE).unwrap().1 = a.bytes.clone();
    code.push((SHIM_BASE, shim.bytes));
    code.push((EXPORT_BASE, exported.bytes));
    code.push((HOST_MIX, host));
    assert!(island.header.gate_table >= RAM
        && island.header.gate_table + island.header.gate_count * 4 <= A_STACK_BASE,
        "gate table is in writable RAM below the stacks");
    for (index, &address) in exported.thunks.iter().enumerate() {
        machine.memory.put(island.header.gate_table + index as u32 * 4, address);
    }
    assert_eq!(island.segments.iter().map(|s| s.kind).collect::<Vec<_>>(),
        [SegmentKind::Text, SegmentKind::Rodata, SegmentKind::Iram, SegmentKind::Data, SegmentKind::Bss],
        "fixture reaches every loadable segment kind");
    for segment in &island.segments {
        let len = segment.bytes as usize;
        if matches!(segment.kind, SegmentKind::Data | SegmentKind::Bss) {
            assert!(segment.address >= ISLAND_DRAM && segment.address + segment.bytes <= ISLAND_DRAM + 0x100);
            let offset = (segment.address - RAM) as usize;
            let target = &mut machine.memory.ram[offset..offset + len];
            if segment.kind == SegmentKind::Bss { target.fill(0); }
            else { target.copy_from_slice(&island.image[segment.file_offset as usize..][..len]); }
        } else {
            Arc::make_mut(&mut machine.memory.code).push((segment.address,
                island.image[segment.file_offset as usize..][..len].to_vec()));
        }
    }
    assert_eq!(machine.memory.word(island_address(&island, "counter")), COUNTER_INITIAL, "DATA loaded");
    assert_eq!(machine.memory.word(island_address(&island, "leaf_seen")), 0, "COMMON zero-filled");
    let frame = initial_frame(A_STACK_BASE, A_STACK_TOP, a.start, a.done, TaskAbi::Call0, [0; 6]);
    machine.memory.install(&frame);
    // Nonzero CALLINC and OWB distinguish exact restoration from clearing WOE.
    machine.memory.put(frame.address + CONTEXT_PS, UM | (1 << 16) | (9 << 8));
    machine.memory.put(TCB_A + TCB_FRAME, frame.address);
    machine.start(&kernel, TCB_A, a.start);
    (machine, kernel, a, b, island, restore_sync, thunk)
}

#[test]
fn linked_island_calls_in_and_out_at_every_boundary() {
    for trace_entries in [0, 4] { linked_boundary_sweep(trace_entries); }
}

fn linked_boundary_sweep(trace_entries: u32) {
    let (mut straight, kernel, a, b, island, restore_sync, thunk) = shim_fixture_trace(trace_entries);
    let roots = ["depth".into()];
    assert_eq!(island::abi_hash(&roots, &["host_mix"]), island.header.abi_hash);
    assert_ne!(island::abi_hash(&roots, &["host_other"]), island.header.abi_hash, "gate name is hashed");
    assert_ne!(island::abi_hash(&["other_root".into()], &["host_mix"]), island.header.abi_hash,
        "root name is hashed");
    assert_ne!(island::abi_hash(&roots, &["host_mix", "host_other"]),
        island::abi_hash(&roots, &["host_other", "host_mix"]), "gate order is hashed");
    let caller_sp = straight.cpu.ar[1];
    let entry_ps = straight.cpu.ps;
    assert_eq!(entry_ps & (UM | WOE | EXCM), UM, "driver starts as a CALL0 task");
    assert_eq!(straight.cpu.window_start, 1 << straight.cpu.window_base);
    let caller_memory = straight.memory.bytes(caller_sp, (IRQ_TOP - caller_sp) as usize).to_vec();
    let counter = island_address(&island, "counter");
    let seen = island_address(&island, "leaf_seen");
    let (expected, leaf) = depth_mirror(DEPTH_N, DEPTH_X, DEPTH_Y);
    let check_return = |machine: &SwitchMachine, boundary: usize| {
        if machine.cpu.pc == thunk.sync || machine.cpu.pc == thunk.sync + 3 {
            assert_eq!(machine.cpu.ps, machine.memory.word(machine.cpu.ar[1] + 4),
                "boundary {boundary}: thunk restores the whole saved PS");
        }
        if machine.cpu.pc == restore_sync || machine.cpu.pc == a.entries[0] || machine.cpu.pc == a.done {
            assert_eq!(machine.cpu.ps, entry_ps, "boundary {boundary}: exact entry PS, including CALLINC and OWB");
        }
        if machine.cpu.pc == a.entries[0] {
            assert_eq!(machine.cpu.ar[0], a.entries[0], "boundary {boundary}: CALL0 link preserved");
            assert_eq!(machine.cpu.ar[1], caller_sp, "boundary {boundary}: caller SP restored");
            assert_eq!(machine.cpu.ar[2], expected, "boundary {boundary}: island result");
            assert_eq!(&machine.cpu.ar[12..16], &SHIM_SENTINELS, "boundary {boundary}: CALL0 callee-saved ARs");
        }
        if machine.cpu.pc == a.done {
            assert_eq!(machine.cpu.window_start, 1 << machine.cpu.window_base,
                "boundary {boundary}: only the CALL0 window remains live");
            assert_eq!(machine.memory.word(A_RESULT + 20), machine.memory.word(A_RESULT + 24),
                "boundary {boundary}: PS after the call equals PS before it exactly");
        }
    };
    let mut boundaries = Vec::new();
    let mut windows = [0usize; 6];
    let start = straight.cpu.instret;
    for _ in 0..2000 {
        check_return(&straight, boundaries.len());
        boundaries.push(straight.clone());
        if straight.cpu.pc == a.done { break; }
        assert_ne!(straight.cpu.pc, VECBASE + USER_VECTOR_OFFSET as u32, "straight shim has no general exception");
        if let Some(v) = WINDOW_VECTOR_OFFSETS.iter().position(|&v| straight.cpu.pc == VECBASE + v as u32) {
            windows[v] += 1;
        }
        straight.step();
    }
    assert_eq!(straight.cpu.pc, a.done, "linked island returns to the marked CALL0 exit");
    assert!(straight.cpu.instret - start <= 1000, "straight shim stays within 1000 retired instructions");
    assert!(boundaries.iter().any(|m| m.cpu.pc == restore_sync), "sweep includes WSR PS -> RSYNC");
    assert!(boundaries.iter().any(|m| m.cpu.pc == a.entries[0]), "straight run reaches the CALL0 return observer");
    assert!(boundaries.iter().any(|m| m.cpu.pc == island.entries[0]), "linked TEXT entry is reached");
    let leaf_entry = island_address(&island, "leaf");
    assert!(boundaries.iter().any(|m| m.cpu.pc == leaf_entry), "linked IRAM leaf is reached");
    let stub = island_address(&island, "host_mix");
    assert!(boundaries.iter().any(|m| m.cpu.pc == stub), "linked gate stub is reached");
    assert!(boundaries.iter().any(|m| m.cpu.pc == thunk.entry), "installed thunk is reached");
    let host_entry = boundaries.iter().find(|m| m.cpu.pc == HOST_MIX).expect("CALL0 implementation is reached");
    let late_store = boundaries.iter().find(|m| m.cpu.pc == thunk.late_store).expect("late stores are reached");
    assert!(late_store.cpu.instret - host_entry.cpu.instret >= 12, "late stores follow at least a dozen instructions");
    let (host_result, host_words) = host_mix_mirror(leaf, LEAF_MAGIC);
    for (index, word) in host_words.into_iter().enumerate() {
        assert_eq!(straight.memory.word(HOST_RESULT + 4 + index as u32 * 4), word, "host frame word {index}");
    }
    assert_eq!(straight.memory.word(HOST_RESULT + 20), host_result, "host result depends on all four frame words");
    assert_eq!(straight.memory.word(A_RESULT), expected, "independent Rust result");
    for (i, value) in SHIM_SENTINELS.into_iter().enumerate() {
        assert_eq!(straight.memory.word(A_RESULT + 4 + i as u32 * 4), value, "saved a{}", 12 + i);
    }
    assert_eq!(straight.cpu.ar[1], caller_sp);
    assert_eq!(&straight.cpu.ar[12..16], &SHIM_SENTINELS);
    assert_eq!(straight.cpu.ps, entry_ps, "full entry PS survives the island");
    assert_eq!(straight.cpu.ps & WOE, 0);
    assert_eq!(straight.cpu.window_start, 1 << straight.cpu.window_base);
    assert_eq!(straight.memory.word(counter), COUNTER_INITIAL.wrapping_add(DEPTH_N + 1));
    assert_eq!(straight.memory.word(seen), leaf);
    assert!(windows[2] > 0, "OF8 really ran");
    assert!(windows[3] > 0, "UF8 really ran");
    let s = caller_sp - SHIM_FRAME;
    for (i, value) in [DEPTH_X, DEPTH_Y, UNUSED_ARGS[0], UNUSED_ARGS[1]].into_iter().enumerate() {
        assert_eq!(straight.memory.word(s + 32 + i as u32 * 4), value,
            "shim a{} was spilled into its own extra save area", 4 + i);
    }
    assert_eq!(straight.memory.word(s - 12), caller_sp, "synthetic root backlink survives");
    assert_eq!(straight.memory.word(s + 16), entry_ps, "entry PS occupies the spare word");
    assert_eq!(straight.memory.bytes(caller_sp, caller_memory.len()), caller_memory,
        "every byte at or above caller SP is unchanged; result words are below SP");

    // Compare A's data and entire live caller frame. Dead call frames and
    // interrupt scratch vary with extra spills, as in the original proof.
    let owned = [(A_RESULT, 0x100), (A_WORK, 64), (HOST_RESULT, 24), (ISLAND_DRAM, 0x100),
        (island.header.gate_table, island.header.gate_count as usize * 4),
        (A_STACK_BASE, 4), (caller_sp, (A_STACK_TOP - caller_sp) as usize)];
    let thunk_sp = boundaries.iter().find(|m| m.cpu.pc == thunk.entry + 3)
        .expect("sweep includes the boundary immediately after ENTRY").cpu.ar[1];
    let pre_mask: Vec<Option<[u32; 4]>> = boundaries.iter().map(|m| {
        if m.cpu.pc != thunk.entry + 3 && m.cpu.pc != thunk.entry + 6 { return None; }
        assert_eq!(m.cpu.interrupt_level(), 0, "pre-RSIL boundary is interruptible");
        let caller = m.cpu.window_base.wrapping_sub(2) & 15;
        assert_ne!(m.cpu.window_start & (1 << caller), 0, "CALL8 caller is live before RSIL");
        let physical = m.cpu.physical_ar();
        Some(std::array::from_fn(|r| physical[(caller as usize * 4 + r) & 63]))
    }).collect();
    // RSIL's a8 reference can itself overflow an older frame and retry, so
    // the same PC may occur more than once; every attempt enters the sweep.
    assert!(pre_mask.iter().filter(|caller| caller.is_some()).count() >= 2,
        "both boundaries between ENTRY and RSIL exercise a live caller");
    let deferred: Vec<_> = boundaries.iter().map(|m| m.cpu.ps & 15 == 15).collect();
    let mut spill_checked = vec![false; boundaries.len()];
    let mut deferred_taken = vec![false; boundaries.len()];
    interrupt_boundary_sweep(&straight, &kernel, &a, &b, boundaries, &owned, |machine, boundary| {
        check_return(machine, boundary);
        if machine.cpu.pc == HOST_MIX || machine.cpu.pc == thunk.late_store {
            assert_eq!(machine.cpu.ps & (WOE | EXCM | 15), WOE | 15,
                "boundary {boundary}: CALL0 implementation runs with WOE and INTLEVEL 15");
        }
        if machine.cpu.pc == a.done {
            assert_eq!(machine.memory.word(HOST_RESULT) & (WOE | EXCM | 15), WOE | 15,
                "boundary {boundary}: implementation recorded WOE and INTLEVEL 15");
        }
        if machine.cpu.pc == HOST_MIX || machine.cpu.pc == thunk.sync {
            if let Some(caller) = pre_mask[boundary] {
                for (r, value) in caller.into_iter().enumerate() {
                    assert_eq!(machine.memory.word(thunk_sp - 16 + r as u32 * 4), value,
                        "boundary {boundary}: leaf a{r} spill survives the CALL0 frame's late stores");
                }
                if machine.cpu.pc == thunk.sync { spill_checked[boundary] = true; }
            }
        }
        if deferred[boundary] && machine.cpu.pc == VECBASE + USER_VECTOR_OFFSET as u32
            && machine.memory.word(CURRENT) == TCB_A
        {
            // The model applies WSR immediately; hardware may defer visibility
            // until RSYNC. Both sites are after SP is back at T.
            assert!([thunk.sync, thunk.sync + 3].contains(&machine.cpu.epc1),
                "boundary {boundary}: masked request is taken at the thunk's unmask boundary");
            assert_eq!(machine.cpu.ar[1], thunk_sp, "boundary {boundary}: SP restored before deferred interrupt");
            deferred_taken[boundary] = true;
        }
    });
    for (boundary, caller) in pre_mask.iter().enumerate() {
        if caller.is_some() { assert!(spill_checked[boundary], "boundary {boundary}: checked the pre-mask spill after late stores"); }
    }
    assert!(deferred.iter().filter(|&&masked| masked).count() >= 12,
        "sweep requests interrupts throughout the adjusted-SP masked stretch");
    for (boundary, masked) in deferred.into_iter().enumerate() {
        if masked { assert!(deferred_taken[boundary], "boundary {boundary}: masked request was eventually taken"); }
    }
}

fn patch_instruction(machine: &mut SwitchMachine, pc: u32, instruction: u32) {
    let (base, bytes) = Arc::make_mut(&mut machine.memory.code).iter_mut()
        .find(|(base, bytes)| pc >= *base && pc + 3 <= *base + bytes.len() as u32).unwrap();
    bytes[(pc - *base) as usize..(pc - *base) as usize + 3].copy_from_slice(&instruction.to_le_bytes()[..3]);
}

fn reach(machine: &mut SwitchMachine, pc: u32) {
    for _ in 0..4096 {
        if machine.cpu.pc == pc { return; }
        machine.step();
    }
    panic!("did not reach {pc:#x}, stopped at {:#x}", machine.cpu.pc);
}

fn fault_word(machine: &SwitchMachine, word: u32) -> u32 {
    machine.memory.word(FAULT_RECORD + word * 4)
}

fn check_fault_header(machine: &SwitchMachine, fault: &Cpu, task: u32, index: u32, context: u32, head: u32) {
    let expected = [FAULT_WORDS, fault.exccause, fault.epc1, fault.excvaddr,
        fault.ps, task, index, context, fault.window_base, fault.window_start, head];
    for (word, expected) in expected.into_iter().enumerate() {
        assert_eq!(fault_word(machine, word as u32), expected, "fault header word {word}");
    }
    let physical = fault.physical_ar();
    for reg in 0..64 {
        assert_eq!(fault_word(machine, FAULT_AR + reg), physical[((fault.window_base * 4 + reg) & 63) as usize],
            "physical AR {reg} relative to fault WINDOWBASE");
    }
}

fn check_dead(machine: &SwitchMachine, tasks: &[u32]) {
    for &task in tasks {
        assert_eq!(machine.memory.word(task + TCB_STATE), DEAD, "island TCB {task:#x} is DEAD");
    }
    assert_eq!(machine.memory.word(TCB_B + TCB_STATE), READY, "dispatcher stays READY");
    assert_eq!(machine.memory.word(IDLE_TCB + TCB_STATE), READY, "idle stays READY");
    assert_eq!(machine.memory.word(RAM + 12), 0, "ISR table disabled");
    assert_eq!(machine.cpu.intenable & !(6 | SOFT_IRQ), 0, "island interrupt enables removed");
    assert_eq!(machine.memory.wifi[0], ROUTED_WIFI[0] | MODEM_RESET_FIELD, "modem reset held, unrelated bits preserved");
    assert_eq!(machine.memory.wifi[1..], [16; WIFI_SOURCES as usize], "all Wi-Fi sources unrouted");
    assert_eq!(machine.memory.word(FAULT_STATE), RADIO_DEAD, "radio state published DEAD");
    assert_eq!(machine.memory.word(CURRENT), TCB_B, "dispatcher selected");
}

#[test]
fn adapter_trace_wraps_and_fault_snapshot_keeps_newest_calls() {
    // Reconstruct the established, untraced thunk shape as the byte oracle.
    let legacy = assemble(&mut Asm::new(0, A8), |a| {
        a.literal_table();
        a.entry(A1, 32);
        a.s32i(A0, A1, 0);
        a.op24(enc::rrr(0, 0, 0, 6, 15, A8));
        a.s32i(A8, A1, 4);
        a.addi(A1, A1, -16);
        a.mov_reg_i32(A8, HOST_MIX);
        a.callx0(A8);
        a.addi(A1, A1, 16);
        a.l32i(A8, A1, 4);
        a.wsr(A8, 230);
        a.rsync();
        a.l32i(A0, A1, 0);
        a.retw_n();
    });
    assert_eq!(exports(EXPORT_BASE, &["depth"], &[Gate { name: "host_mix", call: HOST_MIX, blocks: false, exit_level: None, export: 0 }], None, None).bytes, legacy,
        "trace-off thunk bytes are unchanged");
    for (entries, calls, gate) in [(4u32, 11u32, 1u32), (8, 3, 0)] {
        let (mut machine, kernel, a, b, _, _, _) = shim_fixture_gate(entries, gate);
        let mut history = Vec::<[u32; 8]>::new();
        let mut masked = MaskedInstructions::default();
        for call in 0..calls {
            machine.cpu.pc = a.start;
            let (clock, retired) = (machine.cpu.ccount, machine.cpu.instret);
            let mut trace = None;
            let mut expected = [0; 8];
            expected[0] = gate;
            let mut recorded = false;
            for _ in 0..2048 {
                if machine.cpu.pc == a.done { break; }
                if machine.cpu.pc == kernel.trace_record {
                    assert_eq!(machine.cpu.ar[8], gate, "linked import preserves its configured gate index");
                    expected[1..7].copy_from_slice(&machine.cpu.ar[2..8]);
                    trace = Some(machine.cpu.clone());
                }
                if let Some(before) = &trace {
                    if machine.cpu.pc == before.ar[0] {
                        assert_eq!(&machine.cpu.ar[..8], &before.ar[..8], "trace preserves link, SP and all six arguments");
                        assert_eq!(&machine.cpu.ar[12..], &before.ar[12..], "trace preserves a12..a15");
                        assert_eq!(machine.cpu.ps, before.ps, "trace preserves PS");
                        assert_eq!(machine.cpu.instret - before.instret, 19, "CALL0 trace routine retires 19 instructions");
                    }
                }
                if machine.cpu.pc >= kernel.trace_record && machine.cpu.pc < KERNEL + kernel.bytes.len() as u32 {
                    let at = (machine.cpu.pc - KERNEL) as usize;
                    let bytes = &kernel.bytes[at..];
                    let mut word = [0; 4];
                    let n = bytes.len().min(3); word[..n].copy_from_slice(&bytes[..n]);
                    let (insn, _) = decode(u32::from_le_bytes(word)).unwrap();
                    assert!(!(insn.op == Op::Wsr && insn.imm == 234), "trace never writes CCOUNT");
                    if insn.op == Op::Rsr && insn.imm == 234 { expected[7] = machine.cpu.ccount; }
                }
                if machine.cpu.pc == HOST_MIX {
                    assert_eq!(&machine.cpu.ar[2..8], &expected[1..7], "implementation receives unchanged arguments");
                    assert_eq!(machine.memory.word(TRACE_HEAD), call + 1, "trace head counts every adapter call");
                    let at = TRACE_RING + (call & (entries - 1)) * 32;
                    for word in 0..8 {
                        assert_eq!(machine.memory.word(at + word * 4), expected[word as usize],
                            "call {call} ring word {word}: gate, arguments and exact CCOUNT");
                    }
                    if let Some(previous) = history.last() {
                        assert!(expected[7] > previous[7], "CCOUNT strictly increases between adapter calls");
                    }
                    history.push(expected);
                    recorded = true;
                }
                masked.step(&mut machine);
            }
            assert_eq!(machine.cpu.pc, a.done, "traced linked call returns");
            assert!(recorded, "every linked run reaches the implementation");
            assert_eq!(machine.cpu.ccount.wrapping_sub(clock) as u64, machine.cpu.instret - retired,
                "adapter trace leaves the running CCOUNT untouched");
        }
        for call in calls.saturating_sub(entries)..calls {
            for word in 0..8 {
                assert_eq!(machine.memory.word(TRACE_RING + (call & (entries - 1)) * 32 + word * 4),
                    history[call as usize][word as usize], "wrapped ring retains call {call} word {word}");
            }
        }
        eprintln!("traced thunk masked maximum: {} retired instructions; previous untraced maximum: 69",
            masked.largest_thunk_retired);
        patch_instruction(&mut machine, a.done, 0);
        machine.step();
        reach(&mut machine, b.start);
        check_dead(&machine, &[TCB_A]);
        assert_eq!(fault_word(&machine, FAULT_TRACE_HEAD), calls, "fault records total adapter call count");
        for entry in 0..FAULT_TRACE_ENTRIES {
            for word in 0..8 {
                let expected = if entry < calls.min(entries).min(FAULT_TRACE_ENTRIES) {
                    history[(calls - entry - 1) as usize][word as usize]
                } else { 0 };
                assert_eq!(fault_word(&machine, FAULT_TRACE + entry * 8 + word), expected,
                    "fault trace {entry} word {word}: newest first, unused entries zero");
            }
        }
    }
}

#[test]
fn deep_task_fault_captures_live_and_spilled_callers_and_restores_dispatcher() {
    let (mut machine, kernel, _, b, island, _, _) = shim_fixture_trace(8);
    let entry = island.entries[0];
    let leaf = island_address(&island, "leaf");
    let fault_pc = leaf + 3; // after ENTRY, with all windowed callers intact
    patch_instruction(&mut machine, fault_pc, 0);
    machine.start(&kernel, TCB_B, b.start);
    reach(&mut machine, b.entries[2]);
    let dispatcher = machine.cpu.clone();
    let frame = initial_frame(A_STACK_BASE, A_STACK_TOP, entry, kernel.task_exit,
        TaskAbi::Windowed, [DEPTH_N, DEPTH_X, DEPTH_Y, UNUSED_ARGS[0], UNUSED_ARGS[1], 0]);
    machine.memory.install(&frame);
    machine.memory.put(TCB_A + TCB_FRAME, frame.address);
    machine.cpu.interrupt |= SOFT_IRQ;
    reach(&mut machine, entry);
    let mut callers = Vec::new();
    for _ in 0..2048 {
        if machine.cpu.pc == fault_pc { break; }
        if machine.cpu.pc == entry + 3 && callers.last().map_or(true, |&(_, sp)| sp != machine.cpu.ar[1]) {
            // A high-register instruction here can overflow and retry at
            // the same PC/SP. Observe each recursive frame only once.
            callers.push((machine.cpu.ar[0], machine.cpu.ar[1]));
        }
        machine.step();
    }
    assert_eq!(machine.cpu.pc, fault_pc, "deep task reaches ILL");
    assert_eq!(callers.len(), DEPTH_N as usize + 1, "harness observed every recursive frame");
    assert!(machine.cpu.window_start.count_ones() > 1, "some callers are live");
    assert!((machine.cpu.window_start.count_ones() as usize) < callers.len(), "some callers are spilled");
    let mut expected = vec![(fault_pc, machine.cpu.ar[1])];
    let mut ra = machine.cpu.ar[0];
    for &(caller_ra, caller_sp) in callers.iter().rev() {
        expected.push(((ra & 0x3fff_ffff) | (fault_pc & 0xc000_0000), caller_sp));
        ra = caller_ra;
    }
    expected.truncate(BACKTRACE_DEPTH as usize);
    // Include READY and BLOCKED peers in the kill scan, and a wrapped full ring.
    for (index, tcb) in [(3, RAM + 0x80), (4, RAM + 0xa0)] {
        machine.memory.put(FAULT_TASKS + index * 4, tcb);
        machine.memory.put(tcb + TCB_STATE, if index == 3 { READY } else { BLOCKED });
    }
    machine.memory.put(RAM + 8, 5);
    machine.memory.put(RAM + 12, 2);
    machine.memory.put(TRACE_HEAD, 11);
    let damaged = GUARD ^ 0x100;
    machine.memory.put(ISLAND_GUARD_ADDR, damaged);
    for call in 3..11 {
        for word in 0..8 { machine.memory.put(TRACE_RING + (call & 7) * 32 + word * 4, call * 100 + word); }
    }
    machine.cpu.intenable = SOFT_IRQ | 7;
    machine.cpu.excvaddr = 0xbadc_0de0; // ILL leaves EXCVADDR intact
    let stack = machine.memory.bytes(A_STACK_BASE, (A_STACK_TOP - A_STACK_BASE) as usize).to_vec();
    let corruptible = machine.clone();
    machine.step(); // exception entry, before the vector can change a0
    let fault = machine.cpu.clone();
    assert_eq!(fault.pc, VECBASE + USER_VECTOR_OFFSET as u32, "capture precedes the vector");
    let begin = machine.cpu.instret;
    let bound = 1097 + 34 * 5; // Keep the existing, tighter ceiling for this five-task fixture.
    for _ in 0..bound {
        if fault_word(&machine, FAULT_LENGTH) != 0 { break; }
        machine.step();
    }
    assert_eq!(fault_word(&machine, FAULT_LENGTH), FAULT_WORDS, "fault record published");
    assert_eq!([0, 1, 2, 3].map(|n| fault_word(&machine, FAULT_ISLAND_GUARD + n)), [damaged, ISLAND_READY, 0, 0],
        "guard, status and formerly dirty padding are complete when the length is first published");
    machine.memory.put(ISLAND_GUARD_ADDR, GUARD);
    machine.memory.put(ISLAND_STATUS_ADDR, ISLAND_MISMATCH);
    reach(&mut machine, dispatcher.pc);
    eprintln!("deep task fault through dispatcher RFE: {} retired instructions", machine.cpu.instret - begin);
    assert!(machine.cpu.instret - begin <= bound, "fault path stays within its derived retired-instruction bound");
    check_fault_header(&machine, &fault, TCB_A, 2, 0, 11);
    assert_eq!((fault_word(&machine, FAULT_ISLAND_GUARD), fault_word(&machine, FAULT_ISLAND_STATUS)),
        (damaged, ISLAND_READY), "later SRAM changes leave the snapshot intact");
    assert_eq!(fault_word(&machine, FAULT_DEPTH), BACKTRACE_DEPTH, "bounded full backtrace");
    for (index, (pc, sp)) in expected.into_iter().enumerate() {
        assert_eq!((fault_word(&machine, FAULT_BACKTRACE + index as u32 * 2),
            fault_word(&machine, FAULT_BACKTRACE + index as u32 * 2 + 1)), (pc, sp),
            "known caller {index}: return address and SP across live/spilled boundary");
    }
    for entry in 0..8 {
        for word in 0..8 {
            assert_eq!(fault_word(&machine, FAULT_TRACE + entry * 8 + word), (10 - entry) * 100 + word,
                "fault ring entry {entry} word {word}: newest first across wrap");
        }
    }
    check_dead(&machine, &[TCB_A, RAM + 0x80, RAM + 0xa0]);
    assert_task_state(&machine.cpu, &dispatcher, 0);
    assert_eq!(machine.cpu.ps, dispatcher.ps, "dispatcher PS restored exactly");
    assert_eq!(machine.memory.bytes(A_STACK_BASE, stack.len()), stack, "fault path never stores on abandoned task stack");
    for sp in [0, A_STACK_BASE + 4, u32::MAX] {
        let mut broken = corruptible.clone();
        broken.cpu.ar[1] = sp;
        broken.cpu.window_start = 1 << broken.cpu.window_base; // force the memory fallback
        broken.memory.put(TCB_B + TCB_STATE, BLOCKED); // sleeping dispatcher must be readied by quiesce
        broken.step();
        reach(&mut broken, dispatcher.pc);
        check_dead(&broken, &[TCB_A, RAM + 0x80, RAM + 0xa0]);
        assert_eq!(fault_word(&broken, FAULT_LENGTH), FAULT_WORDS, "corrupt SP {sp:#x} still publishes a record");
        assert_eq!(fault_word(&broken, FAULT_AR + 1), sp, "corrupt SP is preserved in the dump");
        assert_eq!(fault_word(&broken, FAULT_BACKTRACE + 3), 0, "out-of-bounds spill load is rejected before access");
        assert_task_state(&broken.cpu, &dispatcher, 0);
    }
}

#[test]
fn fault_backtrace_reads_lazily_reserved_callers_at_distances_14_and_15() {
    let (fresh, kernel, _, b) = switch_fixture();
    let mut expected_dispatcher = fresh.clone();
    expected_dispatcher.start(&kernel, TCB_B, b.start);
    // Seven CALL8s give the WB=14/WS=0x5555 counterexample. Prepending
    // CALL4 reaches d=15 within the twelve-frame record; a pure CALL4
    // chain's d=15 caller would lie beyond that independent depth limit.
    for calls in [vec![2u8; 7], vec![1, 2, 2, 2, 2, 2, 2, 2]] {
        let mut entries = vec![0; calls.len() + 1];
        let mut returns = vec![0; calls.len()];
        let mut fault_pc = 0;
        let bytes = assemble(&mut Asm::new(calls.len() + 1, A2), |a| {
            for frame in 0..=calls.len() {
                a.base.align(4);
                a.base.label_assign(frame);
                entries[frame] = HOST_MIX + a.base.offset();
                a.entry(A1, 64);
                if frame == calls.len() {
                    fault_pc = HOST_MIX + a.base.offset();
                    a.op24(0); // fault before any high-register reference
                } else {
                    while a.base.offset() & 3 != 1 { a.or(A2, A2, A2); }
                    match calls[frame] {
                        1 => a.call4(frame + 1),
                        2 => a.call8(frame + 1),
                        _ => unreachable!(),
                    }
                    returns[frame] = HOST_MIX + a.base.offset();
                    a.retw_n();
                }
            }
        });
        let mut machine = fresh.clone();
        Arc::make_mut(&mut machine.memory.code).push((HOST_MIX, bytes));
        let frame = initial_frame(A_STACK_BASE, A_STACK_TOP, entries[0], kernel.task_exit,
            TaskAbi::Windowed, [0; 6]);
        machine.memory.install(&frame);
        machine.memory.put(TCB_A + TCB_FRAME, frame.address);
        // The synthetic CALL8 caller is WB=14, so the root's ENTRY sets WB=0.
        machine.cpu.window_base = 14;
        machine.start(&kernel, TCB_A, entries[0]);
        let initial_sp = machine.cpu.ar[1];
        reach(&mut machine, fault_pc);
        let distance: u32 = calls.iter().map(|&n| u32::from(n)).sum();
        let expected_ws = if distance == 14 { 0x5555 } else { 0xaaab };
        assert_eq!((machine.cpu.window_base, machine.cpu.window_start), (distance, expected_ws),
            "d={distance}: real calls preserve the oldest live window immediately after ENTRY");
        assert_eq!(fault_pc, *entries.last().unwrap() + 3, "ILL immediately follows leaf ENTRY");
        let dump_index = 64 - 4 * distance;
        let physical = machine.cpu.physical_ar();
        let root_ra = (2 << 30) | (kernel.task_exit & 0x3fff_ffff);
        assert_eq!((physical[0], physical[1]), (root_ra, initial_sp - 64), "oldest caller still lives at WB=0");
        // Deliberately stale memory for that live root. Reading its presumed
        // spill would produce SP=0 and stop; the dump must supply the real SP.
        let root_callee_sp = initial_sp - 2 * 64;
        machine.memory.put(root_callee_sp - 16, 0x1234_5678);
        machine.memory.put(root_callee_sp - 12, 0);
        let mut expected = vec![(fault_pc, initial_sp - (calls.len() as u32 + 1) * 64)];
        for frame in (0..calls.len()).rev() {
            expected.push((returns[frame], initial_sp - (frame as u32 + 1) * 64));
        }
        expected.push((kernel.task_exit, initial_sp));
        machine.step();
        let fault = machine.cpu.clone();
        let begin = machine.cpu.instret;
        reach(&mut machine, b.start);
        eprintln!("lazy d={distance} fault through dispatcher RFE: {} retired instructions", machine.cpu.instret - begin);
        check_fault_header(&machine, &fault, TCB_A, 2, 0, 0);
        assert_eq!(fault_word(&machine, FAULT_DEPTH) as usize, expected.len(), "complete lazy caller chain through task_exit");
        for (index, (pc, sp)) in expected.into_iter().enumerate() {
            assert_eq!((fault_word(&machine, FAULT_BACKTRACE + index as u32 * 2),
                fault_word(&machine, FAULT_BACKTRACE + index as u32 * 2 + 1)), (pc, sp),
                "d={distance}: known caller {index} uses its live registers until the synthetic caller spills");
        }
        assert_eq!((fault_word(&machine, FAULT_AR + dump_index), fault_word(&machine, FAULT_AR + dump_index + 1)),
            (root_ra, initial_sp - 64), "d={distance}: aliased a0/a1 survive in the register dump");
        assert_eq!(machine.memory.word(root_callee_sp - 12), 0, "fault capture never repairs or uses the stale spill slot");
        check_dead(&machine, &[TCB_A]);
        assert_task_state(&machine.cpu, &expected_dispatcher.cpu, 0);
    }
}

#[test]
fn island_isr_fault_stops_backtrace_at_shim_and_resumes_interrupted_task() {
    let (mut machine, _, _, b) = switch_fixture();
    let shim = shim_code(SHIM_BASE);
    let layout = SchedLayout {
        current: CURRENT, cursor: RAM + 4, task_count: RAM + 8, tasks: FAULT_TASKS,
        isr_count: RAM + 12, isrs: RAM + 0xc0, stray: RAM + 16,
        dispatcher: TCB_B, wake_lines: 6, vector_lines: 0, shim: shim.call, marks: None,
    };
    let sched = sched_code(PICK, &layout);
    let kernel = kernel_code(KERNEL, VECBASE, &switch_layout(sched.pick, 0));
    let mut fault_pc = 0;
    let isr = assemble(&mut Asm::new(0, A3), |a| {
        a.entry(A1, 64);
        a.movi(A3, 0);
        fault_pc = HOST_MIX + a.base.offset();
        a.quos(A4, A2, A3);
        a.retw_n();
    });
    let code = Arc::make_mut(&mut machine.memory.code);
    code.iter_mut().find(|(base, _)| *base == KERNEL).unwrap().1 = kernel.bytes.clone();
    code.iter_mut().find(|(base, _)| *base == PICK).unwrap().1 = sched.bytes;
    let vector = &mut code.iter_mut().find(|(base, _)| *base == VECBASE).unwrap().1;
    let user = user_vector(VECBASE, kernel.irq_entry, kernel.fault_entry);
    vector[USER_VECTOR_OFFSET..USER_VECTOR_OFFSET + user.len()].copy_from_slice(&user);
    code.push((SHIM_BASE, shim.bytes.clone()));
    code.push((HOST_MIX, isr));
    for (address, value) in [(layout.cursor, 0), (layout.isr_count, 1), (layout.isrs, 1),
        (layout.isrs + 4, HOST_MIX), (layout.isrs + 8, 17), (TCB_A + TCB_STATE, BLOCKED),
        (TCB_B + TCB_PRIORITY, 2), (IDLE_TCB + TCB_PRIORITY, 0), (layout.stray, 0)]
    { machine.memory.put(address, value); }
    machine.start(&kernel, TCB_B, b.start);
    reach(&mut machine, b.entries[2]);
    let dispatcher = machine.cpu.clone();
    machine.cpu.intenable = SOFT_IRQ | 7;
    machine.cpu.set_lines(1);
    reach(&mut machine, fault_pc);
    let fault_sp = machine.cpu.ar[1];
    let shim_return = (machine.cpu.ar[0] & 0x3fff_ffff) | (fault_pc & 0xc000_0000);
    assert!((shim.call..SHIM_BASE + shim.bytes.len() as u32).contains(&shim_return), "ISR returns into shim");
    machine.step();
    let fault = machine.cpu.clone();
    assert_eq!(fault.exccause, 6, "QUOS zero divisor cause");
    let begin = machine.cpu.instret;
    reach(&mut machine, dispatcher.pc);
    eprintln!("ISR fault through dispatcher RFE: {} retired instructions", machine.cpu.instret - begin);
    check_fault_header(&machine, &fault, TCB_B, 0, 1, 0);
    assert_eq!(fault_word(&machine, FAULT_DEPTH), 2, "ISR backtrace stops at shim");
    assert_eq!((fault_word(&machine, FAULT_BACKTRACE), fault_word(&machine, FAULT_BACKTRACE + 1)),
        (fault_pc, fault_sp), "ISR frame zero");
    assert_eq!((fault_word(&machine, FAULT_BACKTRACE + 2), fault_word(&machine, FAULT_BACKTRACE + 3)),
        (shim_return, IRQ_TOP - 16 - SHIM_FRAME), "shim caller frame");
    assert!((FAULT_TRACE..FAULT_ISLAND_GUARD).all(|w| fault_word(&machine, w) == 0), "trace-off snapshot is zero");
    assert_eq!(fault_word(&machine, FAULT_ISLAND_GUARD), GUARD);
    check_dead(&machine, &[TCB_A]);
    assert_task_state(&machine.cpu, &dispatcher, 0);
    assert_eq!(machine.cpu.ps, dispatcher.ps, "interrupted PS restored exactly after ISR fault");
}

#[test]
fn task_return_faults_at_kernel_exit_and_fatal_faults_preserve_record() {
    let (fresh, kernel, _, b) = switch_fixture();
    // Reuse one setup for both root ABIs and both fatal classifications.
    let mut expected = fresh.clone();
    expected.start(&kernel, TCB_B, b.start);
    for abi in [TaskAbi::Call0, TaskAbi::Windowed] {
        let mut machine = fresh.clone();
        let bytes = assemble(&mut Asm::new(0, A3), |a| {
            if matches!(abi, TaskAbi::Windowed) { a.entry(A1, 64); a.retw_n(); }
            else { a.ret_n(); }
        });
        Arc::make_mut(&mut machine.memory.code).push((HOST_MIX, bytes));
        let frame = initial_frame(A_STACK_BASE, A_STACK_TOP, HOST_MIX, kernel.task_exit, abi, [0; 6]);
        machine.memory.install(&frame);
        machine.memory.put(TCB_A + TCB_FRAME, frame.address);
        machine.start(&kernel, TCB_A, HOST_MIX);
        reach(&mut machine, kernel.task_exit);
        machine.step();
        assert_eq!(machine.cpu.exccause, 0, "{abi:?}: root return raises ILL");
        reach(&mut machine, b.start);
        assert_eq!(fault_word(&machine, FAULT_PC), kernel.task_exit, "{abi:?}: task_exit is reported");
        assert_eq!(fault_word(&machine, FAULT_CAUSE), 0, "{abi:?}: task-return cause");
        check_dead(&machine, &[TCB_A]);
        assert_task_state(&machine.cpu, &expected.cpu, 0);
        let record = machine.memory.bytes(FAULT_RECORD, FAULT_WORDS as usize * 4).to_vec();
        // Classify a second island fault with DEAD set, even on the IRQ stack.
        machine.cpu.ar[1] = IRQ_TOP - 64;
        machine.memory.put(CURRENT, TCB_A);
        patch_instruction(&mut machine, b.start, 0);
        machine.step();
        let exception = (machine.cpu.exccause, machine.cpu.epc1, machine.cpu.ar[0]);
        reach(&mut machine, kernel.fatal);
        assert_eq!((machine.cpu.exccause, machine.cpu.epc1, machine.cpu.excsave1), exception,
            "second fault retains exception diagnostics at fatal");
        assert_eq!(machine.memory.bytes(FAULT_RECORD, record.len()), record, "second fault cannot overwrite completed record");
    }
    let mut dispatcher_fault = expected;
    patch_instruction(&mut dispatcher_fault, b.start, 0);
    let before = dispatcher_fault.memory.ram.clone();
    dispatcher_fault.step();
    reach(&mut dispatcher_fault, kernel.fatal);
    assert_eq!(dispatcher_fault.memory.ram, before, "dispatcher fault outside ISR writes no radio state or record");
    for _ in 0..3 { dispatcher_fault.step(); }
    assert_eq!(dispatcher_fault.cpu.pc, kernel.fatal, "fatal is a marked J .");
}

#[test]
fn guard_word_and_initial_frames() {
    let (fresh, kernel, a, b) = switch_fixture();
    let mut broken = fresh.clone();
    broken.step(); // ENTRY changes SP: a premature TCB.frame store is observable.
    assert_ne!(broken.cpu.ar[1] - CONTEXT_SIZE as u32, broken.memory.word(TCB_A + TCB_FRAME),
        "guard scenario uses a different outgoing frame address");
    broken.memory.put(A_STACK_BASE, GUARD ^ 1);
    let control = broken.memory.bytes(RAM, 0x100).to_vec();
    let b_memory = broken.memory.bytes(B_STACK_BASE, (B_STACK_TOP - B_STACK_BASE) as usize).to_vec();
    let b_result = broken.memory.bytes(B_RESULT, 160).to_vec();
    broken.cpu.interrupt |= SOFT_IRQ;
    for _ in 0..512 {
        if broken.cpu.pc == kernel.stack_fault { break; }
        broken.step();
    }
    assert_eq!(broken.cpu.pc, kernel.stack_fault, "corrupt guard reaches the marked fault entry");
    assert_eq!(broken.memory.bytes(RAM, 0x100), control, "guard fault publishes neither a frame nor a new current TCB");
    assert_eq!(broken.memory.bytes(B_STACK_BASE, b_memory.len()), b_memory, "guard fault leaves B's stack untouched");
    assert_eq!(broken.memory.bytes(B_RESULT, b_result.len()), b_result, "guard fault never runs B");
    let saved_frame = broken.cpu.ar[0];
    let saved_pc = broken.memory.word(saved_frame + CONTEXT_PC);
    let saved_ps = broken.memory.word(saved_frame + CONTEXT_PS);
    let saved_ar: Vec<_> = (0..16).map(|r| broken.memory.word(saved_frame + CONTEXT_AR + r * 4)).collect();
    for _ in 0..2048 {
        if broken.cpu.pc == b.start { break; }
        broken.step();
    }
    assert_eq!(broken.memory.word(FAULT_STATE), RADIO_DEAD, "guard fault shuts down radio");
    check_dead(&broken, &[TCB_A]);
    assert_eq!(broken.memory.word(FAULT_RECORD), FAULT_WORDS, "guard record published");
    assert_eq!(broken.memory.word(FAULT_RECORD + FAULT_CAUSE * 4), FAULT_GUARD, "guard cause");
    assert_eq!(broken.memory.word(FAULT_RECORD + FAULT_ADDRESS * 4), GUARD ^ 1, "found guard value");
    assert_eq!(broken.memory.word(FAULT_RECORD + FAULT_PC * 4), saved_pc, "guard saved PC");
    assert_eq!(broken.memory.word(FAULT_RECORD + FAULT_PS * 4), saved_ps, "guard saved PS");
    for reg in 0..64 {
        assert_eq!(broken.memory.word(FAULT_RECORD + (FAULT_AR + reg) * 4),
            saved_ar.get(reg as usize).copied().unwrap_or(0), "guard dump word {reg}");
    }
    assert_eq!(broken.memory.word(FAULT_RECORD + FAULT_DEPTH * 4), 0, "guard does not walk damaged stack");
    assert_eq!(broken.memory.word(TCB_A + TCB_STATE), DEAD, "guard task cannot run again");
    assert_eq!(broken.cpu.pc, b.start, "dispatcher restored after guard fault");

    for abi in [TaskAbi::Call0, TaskAbi::Windowed] {
        let code_base = CODE + 0xc000;
        let mut entry = 0;
        let mut exit = 0;
        let bytes = assemble(&mut Asm::new(0, A3), |asm| {
            entry = code_base + asm.base.offset();
            if matches!(abi, TaskAbi::Windowed) { asm.entry(A1, FRAME); }
            for reg in 3..8 { asm.add_n(A2, A2, reg); }
            if matches!(abi, TaskAbi::Windowed) { asm.retw(); } else { asm.ret_n(); }
            exit = code_base + asm.base.offset();
            asm.j(-4);
        });
        let arguments = [3, 5, 7, 11, 13, 17];
        let frame = initial_frame(A_STACK_BASE, A_STACK_TOP, entry, exit, abi, arguments);
        assert_eq!(frame.address, A_STACK_TOP - 64 - CONTEXT_SIZE as u32, "{abi:?}: frame is exactly SP-256");
        assert!(CONTEXT_PAYLOAD_SIZE <= CONTEXT_SIZE - 16, "context payload leaves ABI base save area alone");
        let mut machine = fresh.clone();
        Arc::make_mut(&mut machine.memory.code).push((code_base, bytes));
        machine.memory.install(&frame);
        machine.memory.put(TCB_A + TCB_FRAME, frame.address);
        assert_eq!(machine.memory.word(frame.address + CONTEXT_CPENABLE), 1, "{abi:?}: initial frame enables only FP");
        assert_eq!(machine.memory.word(A_STACK_TOP - 64 - 12), A_STACK_TOP,
            "{abi:?}: synthetic caller backlink locates root extra save area inside the stack");
        for cpenable in [1, 0] {
            // Also exercise restore with saved CP0 disabled. The fresh API
            // still initializes it to one; this is a later saved task state.
            machine.memory.put(frame.address + CONTEXT_CPENABLE, cpenable);
            machine.start(&kernel, TCB_A, entry);
            let initial = machine.cpu.clone();
            assert_eq!(initial.ps & (UM | WOE | 15),
                UM | if matches!(abi, TaskAbi::Windowed) { WOE } else { 0 },
                "{abi:?}: initial task PS selects ABI and unmasks level 1");
            assert_eq!(initial.cpenable, cpenable, "{abi:?}: restore writes CPENABLE after FP state");
            if matches!(abi, TaskAbi::Windowed) {
                assert_eq!((initial.ps >> 16) & 3, 2, "CALL8 initial CALLINC");
                assert_eq!(initial.ar[8], (2 << 30) | (exit & 0x3fff_ffff), "CALL8 initial link");
                assert_eq!(&initial.ar[10..16], &arguments, "CALL8 initial arguments");
            } else {
                assert_eq!(initial.ar[0], exit, "CALL0 initial link");
                assert_eq!(&initial.ar[2..8], &arguments, "CALL0 initial arguments");
            }
            for _ in 0..128 {
                if machine.cpu.pc == exit { break; }
                machine.step();
            }
            assert_eq!(machine.cpu.pc, exit, "{abi:?}: task returns to the supplied exit trampoline");
            let result_reg = if matches!(abi, TaskAbi::Windowed) { 10 } else { 2 };
            assert_eq!(machine.cpu.ar[result_reg], arguments.into_iter().sum::<u32>(), "{abi:?}: six argument sum returned");
            assert_eq!(machine.cpu.ar[1], A_STACK_TOP - 64, "{abi:?}: return restores root SP");
            assert_eq!(machine.cpu.cpenable, cpenable, "{abi:?}: return preserves FP enable");
        }
    }
    assert_eq!(fresh.cpu.pc, a.start, "independent initial-frame checks preserve the shared fixture");
}

// Scheduler proof: all new helpers live below the existing boundary sweeps.
const SCHED_TCBS: [u32; 5] = [RAM + 0x20, RAM + 0x40, RAM + 0x60, RAM + 0xc0, RAM + 0xe0];
const SCHED_DATA: u32 = RAM + 0x200;

fn scheduler_island() -> Island {
    use island_elf::{emit, Object};
    // Both entries receive &SCHED_DATA in a2. Words 32/36 hold D/W's TCBs.
    // W is a real windowed root; its loop never returns to the synthetic caller.
    let mut worker = Vec::new();
    emit(&mut worker, island_elf::entry());
    let loop_pc = worker.len() as u32;
    emit(&mut worker, enc::l32i(A3, A2, 12));
    emit(&mut worker, enc::addi(A3, A3, 1));
    emit(&mut worker, enc::s32i(A3, A2, 12));
    emit(&mut worker, enc::l32i(A3, A2, 32));
    emit(&mut worker, enc::l32i(A3, A3, TCB_FRAME));
    emit(&mut worker, enc::l32i(A3, A3, CONTEXT_PC));
    emit(&mut worker, enc::s32i(A3, A2, 16));
    emit(&mut worker, enc::l32i(A3, A2, 36));
    emit(&mut worker, enc::movi(A4, BLOCKED as i32));
    emit(&mut worker, enc::s32i(A4, A3, TCB_STATE));
    emit(&mut worker, enc::movi(A4, (1 << YIELD_LINE) as i32));
    emit(&mut worker, enc::wsr(A4, 226));
    emit(&mut worker, enc::rsync()); // INTSET, p. 304.
    let jump = worker.len() as u32;
    emit(&mut worker, island_elf::jump(jump, loop_pc));
    let worker_size = worker.len() as u32;

    let mut isr = Vec::new();
    emit(&mut isr, island_elf::entry());
    emit(&mut isr, enc::rsr(0, 3, 0, 230, A3));
    emit(&mut isr, enc::s32i(A3, A2, 20));
    emit(&mut isr, enc::l32i(A3, A2, 36));
    emit(&mut isr, enc::movi(A4, READY as i32));
    emit(&mut isr, enc::s32i(A4, A3, TCB_STATE));
    emit(&mut isr, enc::movi(A4, 1));
    emit(&mut isr, enc::s32i(A4, A2, 24)); // harness deasserts the level line
    // Loop registers are caller-saved. An early loop exit need not leave
    // LCOUNT zero; the picker must restore its kernel-facing invariant.
    emit(&mut isr, enc::movi(A4, 7));
    emit(&mut isr, enc::wsr(A4, 2));
    emit(&mut isr, enc::isync());
    emit(&mut isr, island_elf::retwn());
    let isr_size = isr.len() as u32;

    let mut object = Object::new();
    let text = object.section(".text.scheduler_worker", 1, 6, worker);
    let iram = object.section(".iram1.scheduler_isr", 1, 6, isr);
    object.symbol("scheduler_worker", 1, 2, text as u16, 0, worker_size);
    object.symbol("scheduler_isr", 1, 2, iram as u16, 0, isr_size);
    let section = object.section_symbol(text);
    object.relocation(text, jump, 20, section, loop_pc as i32);
    object.property(text, 0, worker_size, 2);
    object.property(iram, 0, isr_size, 2);
    let bytes = island_elf::archive(&[("scheduler.o", object.encode())]);
    let archives = [Archive { name: "scheduler.a", data: &bytes }];
    let layout = IslandLayout {
        mmu_page: 0, flash_bytes: 0x1_0000,
        window: Region { start: ISLAND_DRAM, bytes: 0x100 }, window_exec: 0x4037_8000,
    };
    island::link(&archives, &["scheduler_worker".into(), "scheduler_isr".into()],
        &ImportMap::parse("").unwrap(), &[], &layout)
        .expect("synthetic scheduler island passes the audit and links")
}

struct SchedulerTask {
    bytes: Vec<u8>,
    entry: u32,
    loop_pc: u32,
    middle: u32,
    after_block: u32,
    end: u32,
}

fn scheduler_task(base: u32, counter: u32, dispatcher: Option<(u32, u32)>) -> SchedulerTask {
    let mut task = SchedulerTask { bytes: Vec::new(), entry: 0, loop_pc: 0, middle: 0, after_block: 0, end: 0 };
    task.bytes = assemble(&mut Asm::new(1, A3), |a| {
        a.literal_table();
        task.entry = base + a.base.offset();
        if dispatcher.is_some() {
            // Seed every task-state family for the exact preemption comparison.
            ContextTask { result: SCHED_DATA, work: A_WORK, other: false }.initialize(a, task.entry);
        }
        a.base.label_assign(0);
        task.loop_pc = base + a.base.offset();
        a.mov_reg_i32(A4, counter);
        a.l32i(A5, A4, 0);
        task.middle = base + a.base.offset();
        a.addi(A5, A5, 1);
        a.s32i(A5, A4, 0);
        if let Some((tcb, block)) = dispatcher {
            a.op24(enc::rrr(0, 0, 0, 6, 15, A3)); // RSIL a3,15
            read_sr(a, A3, 228);
            a.movi(A2, 1 << 1);
            a.or(A3, A3, A2);
            a.wsr(A3, 228); // dispatcher re-arms its wake before sleeping
            a.rsync();
            a.mov_reg_i32(A2, tcb);
            a.mov_reg_i32(A3, block);
            a.callx0(A3);
            task.after_block = base + a.base.offset();
            read_sr(a, A3, 230);
            a.mov_reg_i32(A2, SCHED_DATA);
            a.s32i(A3, A2, 28);
        }
        a.j_label(0);
        task.end = base + a.base.offset();
    });
    task
}

#[derive(Default)]
struct SchedulerTrace {
    scenario: &'static str,
    irq_start: Option<u64>,
    pick_entry: Option<(u32, u32, u32, u32, u32, u64)>,
    irq_counts: BTreeMap<&'static str, Vec<u64>>,
    pick_counts: BTreeMap<&'static str, Vec<u64>>,
    switches: usize,
    returns: usize,
    waits: usize,
    masked: u64,
    masked_retired: u64,
    longest_masked: u64,
    longest_masked_retired: u64,
}

impl SchedulerTrace {
    fn step(&mut self, machine: &mut SwitchMachine, kernel: &KernelCode, sched: &SchedCode) {
        let pc = machine.cpu.pc;
        assert_ne!(pc, kernel.stack_fault, "{}: valid task guards", self.scenario);
        assert_ne!(pc, sched.no_task, "{}: idle must always be READY", self.scenario);
        assert_ne!(pc, VECBASE + USER_UNHANDLED_OFFSET as u32, "{}: no unhandled exception", self.scenario);
        if pc == VECBASE + USER_VECTOR_OFFSET as u32 {
            assert_eq!(machine.cpu.exccause, cause::LEVEL1_INTERRUPT, "scheduler entry is cause 4");
            assert!(self.irq_start.replace(machine.cpu.instret).is_none(), "no nested switch");
        }
        if pc == kernel.restore {
            assert!(machine.cpu.interrupt_level() >= 1, "restore requires level 1 masked");
            assert_eq!(machine.cpu.ps & WOE, 0, "restore requires WOE=0");
            assert_eq!(machine.cpu.lcount, 0, "restore requires LCOUNT=0");
        }
        if pc == sched.pick {
            assert_eq!(machine.cpu.ps, UM | 3, "picker entry has the kernel's exact PS");
            assert_eq!(machine.cpu.lcount, 0);
            assert_eq!(machine.cpu.window_start, 1 << machine.cpu.window_base);
            assert_eq!(machine.cpu.ar[2], machine.memory.word(CURRENT));
            assert!(self.pick_entry.replace((machine.cpu.ar[0], machine.cpu.ps,
                machine.cpu.window_base, machine.cpu.window_start, machine.cpu.ar[1],
                machine.cpu.instret)).is_none());
        }
        let masked = machine.cpu.interrupt_level() != 0;
        if masked {
            self.masked += 1;
            self.longest_masked = self.longest_masked.max(self.masked);
        } else { self.masked = 0; self.masked_retired = 0; }
        let retired = machine.cpu.instret;
        match machine.cpu.step(&mut machine.memory) {
            Ok(()) => {}
            Err(fab_elec_mcu_xtensa::sim::Stop::Waiti { .. }) => {
                assert_eq!(machine.memory.word(CURRENT), SCHED_TCBS[1], "only idle sleeps");
                assert!(machine.cpu.sleeping());
                if machine.cpu.instret != retired {
                    assert_eq!(pc, sched.idle, "idle entry is the WAITI instruction");
                    self.waits += 1;
                }
            }
            Err(stop) => panic!("{}: scheduler stopped at {pc:#x}: {stop:?}", self.scenario),
        }
        if masked {
            self.masked_retired += machine.cpu.instret - retired;
            self.longest_masked_retired = self.longest_masked_retired.max(self.masked_retired);
        }
        if machine.memory.word(SCHED_DATA + 24) != 0 && machine.cpu.interrupt & 1 != 0 {
            // Only 0..2 are driven externally. Software requests ignore set_lines.
            machine.cpu.set_lines(machine.cpu.interrupt & 6);
        }
        if let Some((link, ps, wb, ws, sp, start)) = self.pick_entry {
            if machine.cpu.pc == link {
                assert_eq!(machine.cpu.ps, ps, "{}: pick preserves PS exactly", self.scenario);
                assert_eq!((machine.cpu.window_base, machine.cpu.window_start), (wb, ws),
                    "{}: pick preserves both window registers", self.scenario);
                assert_eq!(machine.cpu.window_start, 1 << machine.cpu.window_base);
                assert_eq!(machine.cpu.ar[1], sp, "pick restores interrupt SP");
                assert_eq!(machine.cpu.lcount, 0, "pick returns with loops disabled");
                let count = machine.cpu.instret - start;
                // This fixture's shim + twelve-instruction leaf ISR costs <64.
                assert!(count <= 64 + 20 * 5 + 20 + 64, "picker's n/m instruction bound");
                self.pick_counts.entry(self.scenario).or_default().push(count);
                self.pick_entry = None;
                self.returns += 1;
            }
        }
        if pc == kernel.stack_fault - 3 {
            let start = self.irq_start.take().expect("RFE belongs to the user vector");
            self.irq_counts.entry(self.scenario).or_default().push(machine.cpu.instret - start);
            assert_eq!(machine.cpu.window_start, 1 << machine.cpu.window_base);
            self.switches += 1;
        }
    }

    fn until(&mut self, machine: &mut SwitchMachine, kernel: &KernelCode, sched: &SchedCode,
        reached: impl Fn(&SwitchMachine) -> bool)
    {
        for _ in 0..4096 {
            if reached(machine) { return; }
            self.step(machine, kernel, sched);
        }
        panic!("{}: scheduler scenario did not reach its boundary", self.scenario);
    }

    fn switch(&mut self, machine: &mut SwitchMachine, kernel: &KernelCode, sched: &SchedCode) {
        let before = self.switches;
        for _ in 0..4096 {
            self.step(machine, kernel, sched);
            if self.switches != before { return; }
        }
        panic!("{}: scheduler failed to return through RFE", self.scenario);
    }
}

#[test]
fn scheduler_marks_measure_each_isr_and_append_through_the_real_report_port() {
    use fab_elec_mcu::compile_kernel;
    use fab_elec_mcu_emit_s3::{build_kernel, chip, KernelOptions, KernelRegion, RADIO_KERNEL_LINES};
    use fab_elec_mcu_rt::{kernel::RegionKind, module::load_kernel, watch::{FAULT_KERNEL_CRIT_SPAN, FAULT_KERNEL_ISR_DEPTH, SITE_RADIO_SCHEDULER}};
    const MARKS: u32 = RAM + 0x400;
    const DEPTH: u32 = MARKS + 12;
    const RING: u32 = RAM + 0x500;
    const PORTS: u32 = 0x4009_0000;
    const HANDLERS: u32 = 0x4008_0000;
    const RETURN: u32 = 0x400a_0000;
    let (mut baseline, _, _, mut layout, _) = scheduler_fixture();
    assert!(layout.marks.is_none());
    let compiled = compile_kernel("let record = export(fn(arg) { report(96, arg) })",
        &chip::profile(), &RADIO_KERNEL_LINES, &[]).unwrap();
    let module = load_kernel(&compiled.module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
    let ports = build_kernel(&module, &KernelOptions { base: PORTS, state: RAM + 0x700,
        t0: RAM + 0x780, reports: Some(RING), stop_pending: None,
        regions: vec![KernelRegion { base: RAM, len: RAM_SIZE as u32, writable: true, kind: RegionKind::Kernel }] }).unwrap();
    let report = ports.symbols.iter().find(|s| s.0 == "report").unwrap().1;
    let mut handlers = [0; 2];
    let isrs = assemble(&mut Asm::new(0, A3), |a| {
        a.literal_table();
        for (index, padding) in [0, 24].into_iter().enumerate() {
            a.base.align(4); // unreachable padding after the preceding RETW
            handlers[index] = HANDLERS + a.base.offset();
            a.entry(A1, 64);
            // This handler deliberately returns with the requested depth.
            // Both slots still pass their real argument through the shim.
            a.mov_reg_i32(A3, DEPTH); a.l32i(A4, A2, 40); a.s32i(A4, A3, 0);
            for _ in 0..padding { a.mov_n(A5, A5); }
            a.movi(A4, 7); a.wsr(A4, 2); a.isync(); a.retw_n();
        }
    });
    let code = Arc::make_mut(&mut baseline.memory.code);
    code.push((PORTS, ports.bytes)); code.push((HANDLERS, isrs));
    let mut measured = [0u32; 2];
    let mut worst_added = 0;
    // 0 measures both bodies; 1 is exactly at threshold; 2 reports; 3 reports
    // both kinds for both slots; 4/5 keep an equal/larger unsigned maximum;
    // 6 wraps CCOUNT; 7 fills the ring; 8 skips an unmatched slot; 9 reports
    // depth even when the span is not a new maximum.
    for scenario in 0..10 {
        let two = matches!(scenario, 0 | 3 | 4 | 7 | 8);
        let depth = if matches!(scenario, 3 | 7 | 9) { 2 } else { 0 };
        let threshold = match scenario { 0 | 8 => u32::MAX, 1 => measured[0], _ => measured[0] - 1 };
        let old_max = match scenario { 4 => measured[1], 5 | 9 => 0x8000_0000, _ => 0 };
        layout.marks = Some(SchedMarks { isr_start: MARKS, isr_max_span: MARKS + 4,
            isr_max_entry: MARKS + 8, crit_depth: DEPTH, report, threshold });
        let sched = sched_code(PICK, &layout);
        let mut stamps = Vec::new();
        let mut reset = 0;
        let mut at = (sched.pick - PICK) as usize;
        while at < (sched.block - PICK) as usize {
            let mut word = [0; 4]; let n = 3.min(sched.bytes.len() - at);
            word[..n].copy_from_slice(&sched.bytes[at..at + n]);
            let (insn, len) = decode(u32::from_le_bytes(word)).unwrap();
            if insn.op == Op::Rsr && insn.imm == 234 { stamps.push(PICK + at as u32); }
            if insn.op == Op::Wsr && insn.imm == 2 { reset = PICK + at as u32 - 3; }
            at += len as usize;
        }
        assert_eq!(stamps.len(), 2);
        assert_ne!(reset, 0);
        let mut machine = baseline.clone();
        Arc::make_mut(&mut machine.memory.code).iter_mut().find(|c| c.0 == PICK).unwrap().1 = sched.bytes.clone();
        for (address, value) in [(layout.isr_count, if two { 2 } else { 1 }),
            (layout.isrs, 1), (layout.isrs + 4, handlers[0]), (layout.isrs + 8, SCHED_DATA),
            (layout.isrs + ISR_SLOT, 8), (layout.isrs + ISR_SLOT + 4, handlers[1]),
            (layout.isrs + ISR_SLOT + 8, SCHED_DATA), (SCHED_DATA + 40, depth),
            (MARKS + 4, old_max), (MARKS + 8, if old_max == 0 { 0 } else { 0x400b_0000 }),
            (RING + 4, if scenario == 7 { REPORT_RING_ENTRIES } else { 0 }),
            (RING + 8, if scenario == 7 { u32::MAX - 1 } else { 0 })] { machine.memory.put(address, value); }
        machine.cpu = Cpu::new(s3_config(), sched.pick);
        machine.cpu.ps = UM | 3; machine.cpu.window_base = 15; machine.cpu.window_start = 1 << 15;
        machine.cpu.ar[0] = RETURN; machine.cpu.ar[1] = IRQ_TOP - 16;
        machine.cpu.ar[2] = machine.memory.word(CURRENT);
        machine.cpu.intenable = 9;
        machine.cpu.set_lines(if two && scenario != 8 { 9 } else { 1 });
        machine.cpu.ccount = if scenario == 6 { u32::MAX - 40 } else { 1000 };
        let mut start = 0;
        let mut elapsed = Vec::new();
        let mut shim = None;
        let mut shim_cost = 0;
        let mut shim_total = 0;
        let mut mark_begin = 0;
        let mut appending = None;
        for _ in 0..2048 {
            let pc = machine.cpu.pc;
            if pc == RETURN { break; }
            if pc == stamps[0] - 3 { mark_begin = machine.cpu.instret; }
            if pc == stamps[0] { start = machine.cpu.ccount; }
            if pc == stamps[1] {
                assert_eq!(machine.memory.word(MARKS), start);
                if scenario == 6 { assert!(machine.cpu.ccount < start, "ISR span crosses CCOUNT wrap"); }
                elapsed.push(machine.cpu.ccount.wrapping_sub(start));
            }
            if pc == layout.shim { shim = Some((machine.cpu.ar[0], machine.cpu.instret)); }
            if let Some((link, begin)) = shim {
                if pc == link { shim_cost = machine.cpu.instret - begin; shim_total += shim_cost; shim = None; }
            }
            if pc == report {
                assert_eq!(machine.cpu.ps & 15, 3, "pick appends masked");
                assert_eq!(machine.cpu.ar[10], u32::from(SITE_RADIO_SCHEDULER));
                appending = Some((machine.cpu.ar[0], machine.cpu.instret, machine.cpu.ar[12..16].to_vec()));
            }
            if let Some((link, begin, saved)) = &appending {
                if pc == *link {
                    assert_eq!(&machine.cpu.ar[12..16], saved.as_slice(), "report preserves the slot loop state");
                    assert!(machine.cpu.instret - *begin <= 15, "emitted report path fits the per-slot allowance");
                    appending = None;
                }
            }
            if pc == reset {
                let added = machine.cpu.instret - mark_begin - shim_cost - 1; // omit the existing CALLX0 and shim/ISR
                assert!(added <= MARKED_ISR_SLOT_INSNS - 20);
                worst_added = worst_added.max(added);
            }
            machine.step();
        }
        assert_eq!(machine.cpu.pc, RETURN, "pick returns in every reporting case");
        assert_eq!(machine.cpu.ar[2], layout.dispatcher);
        assert_eq!((machine.cpu.ps, machine.cpu.window_base, machine.cpu.window_start), (UM | 3, 15, 1 << 15));
        assert_eq!(machine.cpu.ar[1], IRQ_TOP - 16);
        assert_eq!(machine.cpu.lcount, 0, "marks run before the existing loop-register reset");
        assert_eq!(machine.memory.word(layout.stray), 0, "matching slots still claim both owned lines");
        let called = if two && scenario != 8 { 2 } else { 1 };
        assert_eq!(elapsed.len(), called);
        if scenario == 0 { measured.copy_from_slice(&elapsed); assert!(measured[1] > measured[0]); }
        assert_eq!(elapsed, measured[..called]);
        let (max_span, max_entry) = if old_max != 0 { (old_max, 0x400b_0000) }
            else { (measured[called - 1], handlers[called - 1]) };
        assert_eq!((machine.memory.word(MARKS + 4), machine.memory.word(MARKS + 8)), (max_span, max_entry));
        assert_eq!(machine.memory.word(DEPTH), depth, "reporting never repairs an ISR's critical depth");
        let expected: Vec<(u32, u32)> = match scenario {
            2 | 6 => vec![(u32::from(FAULT_KERNEL_CRIT_SPAN), measured[0])],
            3 => vec![(u32::from(FAULT_KERNEL_CRIT_SPAN), measured[0]), (u32::from(FAULT_KERNEL_ISR_DEPTH), depth),
                (u32::from(FAULT_KERNEL_CRIT_SPAN), measured[1]), (u32::from(FAULT_KERNEL_ISR_DEPTH), depth)],
            9 => vec![(u32::from(FAULT_KERNEL_ISR_DEPTH), depth)],
            _ => vec![],
        };
        if scenario == 7 {
            assert_eq!(machine.memory.word(RING + 4), REPORT_RING_ENTRIES);
            assert_eq!(machine.memory.word(RING + 8), u32::MAX, "full-ring reports saturate lost and return");
            assert!(machine.memory.bytes(RING + 12, REPORT_RING_ENTRIES as usize * 12).iter().all(|&b| b == 0));
        } else {
            assert_eq!(machine.memory.word(RING + 4), expected.len() as u32);
            assert_eq!(machine.memory.word(RING + 8), 0);
            for (i, &(code, arg)) in expected.iter().enumerate() {
                let slot = RING + 12 + i as u32 * 12;
                assert_eq!([machine.memory.word(slot), machine.memory.word(slot + 4), machine.memory.word(slot + 8)],
                    [code, u32::from(SITE_RADIO_SCHEDULER), arg]);
            }
        }
        assert!(machine.cpu.instret <= 64 + 20 * 5 + MARKED_ISR_SLOT_INSNS * (if two { 2 } else { 1 }) + shim_total);
    }
    assert_eq!(worst_added, 57, "longest emitted mark path includes both successful report calls");
    assert_eq!(MARKED_ISR_SLOT_INSNS, 20 + worst_added);
    eprintln!("ISR marks: {worst_added} worst-path instructions per matching slot; marked slot bound {MARKED_ISR_SLOT_INSNS}");
}

fn scheduler_fixture() -> (SwitchMachine, KernelCode, SchedCode, SchedLayout, SchedulerTask) {
    let shim = shim_code(SHIM_BASE);
    let layout = SchedLayout {
        current: CURRENT, cursor: RAM + 4, task_count: RAM + 8, tasks: RAM + 0x80,
        isr_count: RAM + 12, isrs: RAM + 0xa0, stray: RAM + 16,
        dispatcher: SCHED_TCBS[0], wake_lines: (1 << 1) | (1 << 2), vector_lines: 0, shim: shim.call, marks: None,
    };
    let sched = sched_code(PICK, &layout);
    let kernel = kernel_code(KERNEL, VECBASE, &RadioLayout {
        tasks: layout.tasks, task_count: layout.task_count, dispatcher: layout.dispatcher,
        idle: SCHED_TCBS[1], isr_count: layout.isr_count, wake_lines: layout.wake_lines, vector_lines: 0,
        ..switch_layout(sched.pick, 0)
    });
    let d = scheduler_task(CODE, SCHED_DATA, Some((SCHED_TCBS[0], sched.block)));
    let x = scheduler_task(CODE + 0x2000, SCHED_DATA + 4, None);
    let y = scheduler_task(CODE + 0x4000, SCHED_DATA + 8, None);
    let island = scheduler_island();
    let w = island_address(&island, "scheduler_worker");
    let isr = island_address(&island, "scheduler_isr");
    assert_eq!(w >> 30, shim.call >> 30);
    assert_eq!(isr >> 30, shim.call >> 30);

    let mut vectors = vec![0; 0x400];
    vectors[..WINDOW_BLOCK_SIZE].copy_from_slice(&window_block());
    let user = user_vector(VECBASE, kernel.irq_entry, kernel.fault_entry);
    vectors[USER_VECTOR_OFFSET..USER_VECTOR_OFFSET + user.len()].copy_from_slice(&user);
    let mut code = vec![(VECBASE, vectors), (KERNEL, kernel.bytes.clone()),
        (PICK, sched.bytes.clone()), (SHIM_BASE, shim.bytes),
        (CODE, d.bytes.clone()), (CODE + 0x2000, x.bytes), (CODE + 0x4000, y.bytes)];
    for segment in &island.segments {
        if segment.bytes == 0 { continue; }
        assert!(matches!(segment.kind, SegmentKind::Text | SegmentKind::Iram), "fixture has only executable sections");
        code.push((segment.address, island.image[segment.file_offset as usize..][..segment.bytes as usize].to_vec()));
    }
    let config = s3_config();
    for line in 0..4 {
        assert_eq!(config.int_type[line], fab_elec_mcu_xtensa::sim::IntType::Level);
        assert_eq!(config.int_level[line], 1);
    }
    assert_eq!(config.int_type[YIELD_LINE as usize], fab_elec_mcu_xtensa::sim::IntType::Software);
    let mut machine = SwitchMachine { cpu: Cpu::new(config, kernel.restore),
        memory: SwitchMemory { code: Arc::new(code), ram: vec![0; RAM_SIZE], wifi: ROUTED_WIFI } };
    machine.cpu.vecbase = VECBASE;
    machine.cpu.traps = TrapMode::Take;
    machine.cpu.window_base = 15; // the ISR's CALL8/ENTRY crosses physical wrap
    machine.cpu.intenable = (1 << YIELD_LINE) | 15;
    for (address, value) in [(ISLAND_GUARD_ADDR, GUARD), (ISLAND_STATUS_ADDR, ISLAND_READY),
        (layout.cursor, 0), (layout.task_count, 5), (layout.isr_count, 1),
        (layout.stray, 0), (layout.isrs, 1), (layout.isrs + 4, isr), (layout.isrs + 8, SCHED_DATA),
        (SCHED_DATA + 28, u32::MAX), (SCHED_DATA + 32, SCHED_TCBS[0]), (SCHED_DATA + 36, SCHED_TCBS[4])]
    { machine.memory.put(address, value); }
    // A non-valid slot must neither claim stray line 3 nor be invoked.
    machine.memory.put(layout.isrs + ISR_SLOT, 1 << 3);
    machine.memory.put(layout.isrs + ISR_SLOT + 4, 0);
    let entries = [d.entry, sched.idle, x.entry, y.entry, w];
    for (i, (&tcb, &entry)) in SCHED_TCBS.iter().zip(&entries).enumerate() {
        let bottom = RAM + 0x1000 + i as u32 * 0x1000;
        let top = bottom + 0x1000;
        let abi = if i == 4 { TaskAbi::Windowed } else { TaskAbi::Call0 };
        let frame = initial_frame(bottom, top, entry, entry, abi, [SCHED_DATA, 0, 0, 0, 0, 0]);
        machine.memory.install(&frame);
        machine.memory.put(bottom, GUARD);
        for (offset, value) in [(TCB_FRAME, frame.address), (TCB_STACK_BASE, bottom),
            (TCB_STACK_TOP, top), (TCB_PRIORITY, [2, 0, 1, 1, 5][i]),
            (TCB_STATE, if i == 4 { BLOCKED } else { READY })]
        { machine.memory.put(tcb + offset, value); }
        machine.memory.put(layout.tasks + i as u32 * 4, tcb);
    }
    machine.start(&kernel, SCHED_TCBS[0], d.entry);
    (machine, kernel, sched, layout, d)
}

#[test]
fn idle_interrupts_skip_the_save_and_only_pure_wakes_skip_pick() {
    let (mut asleep, kernel, sched, layout, _) = scheduler_fixture();
    let idle = SCHED_TCBS[1];
    let frame = asleep.memory.word(idle + TCB_FRAME);
    let boot = asleep.memory.bytes(frame, CONTEXT_SIZE).to_vec();
    let protected = [CONTEXT_PC, CONTEXT_PS, CONTEXT_AR + 4, CONTEXT_CPENABLE];
    let check_idle = |machine: &SwitchMachine| {
        assert_eq!(machine.memory.word(idle + TCB_FRAME), frame, "idle keeps its boot frame pointer");
        for offset in protected {
            assert_eq!(machine.memory.bytes(frame + offset, 4), &boot[offset as usize..offset as usize + 4],
                "idle boot frame word {offset} never changes");
        }
    };
    for index in [2, 3] { asleep.memory.put(SCHED_TCBS[index] + TCB_STATE, BLOCKED); }
    let mut setup = SchedulerTrace { scenario: "idle wake setup", ..Default::default() };
    setup.until(&mut asleep, &kernel, &sched, |m|
        m.cpu.ps & (EXCM | 15) == 0 && m.cpu.interrupt & (1 << YIELD_LINE) != 0);
    // Exact pre-exception task state, including block's resumable RSYNC PC.
    let dispatcher = asleep.cpu.clone();
    setup.switch(&mut asleep, &kernel, &sched);
    assert_eq!(asleep.memory.word(CURRENT), idle);
    assert_eq!(asleep.cpu.pc, sched.idle);
    let idle_entry = asleep.clone();
    setup.until(&mut asleep, &kernel, &sched, |m| m.cpu.sleeping());
    check_idle(&asleep);
    for index in [0, 2, 3, 4] {
        assert_eq!(asleep.memory.word(SCHED_TCBS[index] + TCB_STATE), BLOCKED);
    }

    for (scenario, lines, ready_worker, spurious, next) in [
        ("idle (i): TARGET0 only", 1 << 1, false, false, 0),
        ("idle (ii): TARGET0 plus island", (1 << 1) | 1, true, false, 4),
        ("idle (iii): island only", 1, false, false, 1),
        ("idle (iv): spurious p=0", 1 << 1, false, true, 1),
    ] {
        let mut machine = asleep.clone();
        // The same ISR can make an already-READY idle READY again, exercising
        // an ISR that acknowledges without making any new task runnable.
        if !ready_worker { machine.memory.put(SCHED_DATA + 36, idle); }
        let enabled = machine.cpu.intenable;
        machine.cpu.set_lines(lines);
        if spurious {
            let retired = machine.cpu.instret;
            machine.step(); // take cause 4, then withdraw the source before p is sampled
            assert_eq!(machine.cpu.pc, VECBASE + USER_VECTOR_OFFSET as u32);
            assert_eq!(machine.cpu.instret, retired, "exception entry retires no instruction");
            machine.cpu.set_lines(0);
        }
        let mut trace = SchedulerTrace { scenario, ..Default::default() };
        let mut clears = Vec::new();
        for _ in 0..4096 {
            check_idle(&machine);
            if trace.switches != 0 { break; }
            // Observe the actual acknowledgement operand. Level inputs cannot
            // be cleared by INTCLEAR, so the source remains asserted until the
            // ISR or the fixture lowers it (ISA pp. 153–156, 304).
            if machine.cpu.will_execute() {
                let pc = machine.cpu.pc;
                let (base, bytes) = machine.memory.code.iter().find(|(base, bytes)|
                    pc >= *base && pc < *base + bytes.len() as u32).unwrap();
                let bytes = &bytes[(pc - *base) as usize..];
                let mut word = [0; 4];
                let len = bytes.len().min(3);
                word[..len].copy_from_slice(&bytes[..len]);
                let (insn, _) = decode(u32::from_le_bytes(word)).unwrap();
                if insn.op == Op::Wsr && insn.imm == 227 {
                    clears.push(machine.cpu.ar[insn.t as usize]);
                }
            }
            if next == 0 && trace.irq_start.is_some() {
                assert_ne!(machine.cpu.ps & EXCM, 0, "fast wake keeps EXCM set until RFE");
            }
            trace.step(&mut machine, &kernel, &sched);
        }
        assert_eq!(trace.switches, 1, "{scenario}: one vector-to-RFE switch");
        check_idle(&machine);
        // Only the two deliberately saved scratch registers may differ.
        for offset in (0..CONTEXT_SIZE as u32).step_by(4) {
            if [CONTEXT_AR + 8, CONTEXT_AR + 12].contains(&offset) { continue; }
            assert_eq!(machine.memory.bytes(frame + offset, 4), &boot[offset as usize..offset as usize + 4],
                "{scenario}: idle boot word {offset} preserved");
        }
        let pending = if spurious { 0 } else { lines };
        assert_eq!(clears, [pending], "{scenario}: INTCLEAR gets exactly p");
        assert_eq!(machine.memory.word(CURRENT), SCHED_TCBS[next], "{scenario}: correct task selected");
        assert_eq!(machine.memory.word(layout.cursor), next as u32, "{scenario}: selected task index stored");
        let wake = pending & layout.wake_lines != 0;
        assert_eq!(machine.memory.word(SCHED_TCBS[0] + TCB_STATE), if wake { READY } else { BLOCKED });
        assert_eq!(machine.cpu.intenable, if wake { enabled & !layout.wake_lines } else { enabled },
            "{scenario}: mask all and only dispatcher wake lines on a wake");
        let isr = pending & 1 != 0;
        assert_eq!(machine.memory.word(SCHED_DATA + 24), u32::from(isr), "{scenario}: ISR ran iff pending");
        assert_eq!(machine.memory.word(SCHED_DATA + 12), 0, "no task instruction runs before this RFE boundary");
        if isr {
            assert_eq!(machine.memory.word(SCHED_DATA + 20) & (WOE | EXCM | 15), WOE | 3,
                "island ISR uses the shim at level 3 with EXCM clear");
            assert_eq!(machine.cpu.interrupt & 1, 0, "ISR acknowledgement lowers the island source");
        }
        let count = trace.irq_counts[scenario][0];
        println!("{scenario}: user vector through RFE retired {count}; pick retired {:?}",
            trace.pick_counts.get(scenario).map(Vec::as_slice).unwrap_or(&[]));
        if next == 0 {
            assert_eq!(trace.returns, 0, "pure wake bypasses the picker");
            assert_eq!(count, fab_elec_mcu_emit_s3::runtime::RADIO_IDLE_WAKE_INSNS,
                "fast wake cost includes the complete vector and restore");
            assert_task_state(&machine.cpu, &dispatcher, 0);
            assert_eq!(machine.cpu.ps, dispatcher.ps, "complete saved dispatcher PS restored");
        } else {
            assert_eq!(trace.returns, 1, "unusual pending bits take the general picker exactly once");
            if next == 1 {
                assert_eq!(machine.cpu.pc, sched.idle, "idle restarts at its boot WAITI");
                assert_eq!(machine.cpu.ps, UM);
            } else {
                let selected = machine.memory.word(SCHED_TCBS[next] + TCB_FRAME);
                assert_eq!(machine.cpu.pc, machine.memory.word(selected + CONTEXT_PC));
            }
        }
        if wake {
            assert_ne!(machine.cpu.interrupt & (1 << 1), 0, "INTCLEAR cannot lower held TARGET0");
        }
        machine.cpu.set_lines(0);
        assert_eq!(machine.cpu.interrupt & lines, 0, "pending bits clear once their level sources lower");
        if ready_worker {
            trace.until(&mut machine, &kernel, &sched, |m| m.memory.word(SCHED_DATA + 12) == 1);
            assert_eq!(machine.memory.word(CURRENT), SCHED_TCBS[4], "ISR-readied worker actually runs");
        } else if next == 1 {
            trace.until(&mut machine, &kernel, &sched, |m| m.cpu.sleeping());
            assert_eq!(trace.waits, 1, "unchanged idle context resumes tickless sleep");
        }
        check_idle(&machine);
    }

    // The idle shortcut applies only to cause 4. A real fault while idle is
    // current must still enter R3-7a's fatal path without any frame/record save.
    let mut fatal = idle_entry;
    patch_instruction(&mut fatal, sched.idle, 0); // ILL, before executing WAITI
    let ram = fatal.memory.ram.clone();
    fatal.step();
    assert_eq!(fatal.cpu.exccause, 0);
    reach(&mut fatal, kernel.fatal);
    assert_eq!(fatal.memory.ram, ram, "idle fault remains fatal without writing RAM");
}

#[test]
fn scheduler_picks_wakes_and_idles_without_a_tick() {
    let (mut machine, kernel, sched, layout, d) = scheduler_fixture();
    // Locate the block boundaries by decoding, not by guessing byte offsets.
    let mut lower = 0;
    let mut resume = 0;
    let mut offset = (sched.block - PICK) as usize;
    while PICK + (offset as u32) < sched.idle {
        let mut bytes = [0; 4];
        let len = (sched.bytes.len() - offset).min(3);
        bytes[..len].copy_from_slice(&sched.bytes[offset..offset + len]);
        let (insn, len) = decode(u32::from_le_bytes(bytes)).expect("block instruction decodes");
        if insn.op == Op::Rsil { lower = PICK + offset as u32; resume = lower + len; }
        offset += len as usize;
    }
    assert_ne!(lower, 0);
    let mut trace = SchedulerTrace { scenario: "a: D blocks, X/Y rotate", ..Default::default() };
    trace.until(&mut machine, &kernel, &sched, |m| m.cpu.pc == sched.block);
    let blocked_entry = machine.cpu.clone();
    assert_eq!(blocked_entry.ps & 15, 15);
    for (turn, index) in [2, 3, 2, 3].into_iter().enumerate() {
        let counter = SCHED_DATA + if index == 2 { 4 } else { 8 };
        let before = machine.memory.word(counter);
        if turn != 0 { machine.cpu.interrupt |= 1 << YIELD_LINE; }
        trace.switch(&mut machine, &kernel, &sched);
        assert_eq!(machine.memory.word(CURRENT), SCHED_TCBS[index], "equal-priority scan rotates X,Y,X,Y");
        assert_eq!(machine.memory.word(layout.cursor), index as u32);
        assert_eq!(machine.cpu.interrupt & (1 << YIELD_LINE), 0, "yield was acknowledged");
        trace.until(&mut machine, &kernel, &sched, |m| m.memory.word(counter) > before);
        assert_eq!(trace.waits, 0, "idle never executes while workers are READY");
    }
    assert_eq!(machine.memory.word(SCHED_TCBS[0] + TCB_STATE), BLOCKED);
    assert_eq!(machine.memory.word(SCHED_DATA), 1);
    let d_frame = machine.memory.word(SCHED_TCBS[0] + TCB_FRAME);
    assert_eq!(machine.memory.word(d_frame + CONTEXT_PC), resume, "D suspended inside block");

    trace.scenario = "b: idle, no tick";
    for index in [2, 3] { machine.memory.put(SCHED_TCBS[index] + TCB_STATE, BLOCKED); }
    machine.cpu.interrupt |= 1 << YIELD_LINE;
    trace.switch(&mut machine, &kernel, &sched);
    assert_eq!(machine.memory.word(CURRENT), SCHED_TCBS[1], "priority-zero idle is eligible");
    trace.until(&mut machine, &kernel, &sched, |m| m.cpu.sleeping());
    assert_eq!(trace.waits, 1);
    assert_eq!(machine.cpu.next_timer(), None, "no enabled comparator supplies a tick");
    let asleep = machine.cpu.clone();
    let switches = trace.switches;
    machine.cpu.advance(1_000_000);
    for _ in 0..64 { trace.step(&mut machine, &kernel, &sched); }
    assert_task_state(&machine.cpu, &asleep, 0);
    assert_eq!(machine.cpu.instret, asleep.instret, "sleep retires nothing without an external line");
    assert_eq!(trace.switches, switches, "elapsed time takes no interrupt");

    trace.scenario = "c: wake dispatcher";
    assert_eq!(machine.cpu.intenable & layout.wake_lines, layout.wake_lines);
    machine.cpu.set_lines(1 << 1);
    trace.switch(&mut machine, &kernel, &sched);
    assert_eq!(machine.memory.word(CURRENT), SCHED_TCBS[0]);
    assert_eq!(machine.memory.word(SCHED_TCBS[0] + TCB_STATE), READY);
    assert_eq!(machine.cpu.pc, resume, "wake resumes inside block");
    assert_eq!(machine.cpu.intenable & layout.wake_lines, 0, "line 1 wake masks both dispatcher lines");
    assert_ne!(machine.cpu.interrupt & 2, 0, "INTCLEAR cannot deassert a level input");
    machine.cpu.set_lines(0);
    trace.until(&mut machine, &kernel, &sched, |m| m.cpu.pc == d.after_block);
    let mut returned = blocked_entry.clone();
    returned.pc = d.after_block;
    returned.ps &= !15;
    returned.ar[2..4].copy_from_slice(&machine.cpu.ar[2..4]);
    assert_task_state(&machine.cpu, &returned, 0);
    assert_eq!(machine.cpu.ps, returned.ps, "block changes only INTLEVEL and returns at zero");

    trace.scenario = "c: line 2 alone wakes dispatcher";
    trace.until(&mut machine, &kernel, &sched, |m| m.cpu.pc == lower);
    assert_eq!(machine.cpu.ps & 15, 15);
    assert_eq!(machine.memory.word(SCHED_TCBS[0] + TCB_STATE), BLOCKED);
    machine.cpu.intenable |= layout.wake_lines;
    machine.cpu.set_lines(1 << 2);
    trace.switch(&mut machine, &kernel, &sched);
    assert_eq!(machine.memory.word(CURRENT), SCHED_TCBS[0]);
    assert_eq!(machine.memory.word(SCHED_TCBS[0] + TCB_STATE), READY);
    assert_eq!(machine.cpu.pc, resume, "line 2 wake resumes inside block");
    assert_eq!(machine.cpu.intenable & layout.wake_lines, 0, "line 2 wake masks both dispatcher lines");
    assert_ne!(machine.cpu.interrupt & (1 << 2), 0, "INTCLEAR cannot deassert line 2");
    machine.cpu.set_lines(0);
    trace.until(&mut machine, &kernel, &sched, |m| m.cpu.pc == d.after_block);
    assert_eq!(machine.cpu.ps & 15, 0);

    trace.scenario = "d: ISR readies W, then W blocks";
    trace.until(&mut machine, &kernel, &sched, |m| m.cpu.pc == d.middle);
    let mut straight = machine.clone();
    machine.cpu.set_lines(1);
    trace.switch(&mut machine, &kernel, &sched);
    assert_eq!(machine.memory.word(CURRENT), SCHED_TCBS[4], "ISR-readied priority 5 wins this same pick");
    let isr_ps = machine.memory.word(SCHED_DATA + 20);
    assert_eq!(isr_ps & (WOE | EXCM | 15), WOE | 3, "windowed ISR runs through shim at level 3");
    assert_eq!(machine.memory.word(SCHED_DATA + 24), 1, "ISR got its slot argument and acknowledged");
    assert_eq!(machine.cpu.interrupt & 1, 0, "harness lowered the acknowledged level line");
    trace.switch(&mut machine, &kernel, &sched);
    assert_eq!(machine.memory.word(SCHED_DATA + 12), 1, "W actually ran");
    let interrupted_pc = machine.memory.word(SCHED_DATA + 16);
    assert!((d.loop_pc..d.end).contains(&interrupted_pc), "W saw D preempted in its loop");
    assert_eq!(interrupted_pc, d.middle);
    assert_eq!(machine.memory.word(SCHED_TCBS[4] + TCB_STATE), BLOCKED);
    assert_eq!(machine.memory.word(CURRENT), SCHED_TCBS[0]);
    assert_task_state(&machine.cpu, &straight.cpu, 0);
    assert_eq!(machine.cpu.ps, straight.cpu.ps, "D's complete PS survives W");
    let sp = straight.cpu.ar[1];
    let top = straight.memory.word(SCHED_TCBS[0] + TCB_STACK_TOP);
    assert_eq!(machine.memory.bytes(sp, (top - sp) as usize), straight.memory.bytes(sp, (top - sp) as usize),
        "D's live stack survives ISR and W");
    for _ in 0..2 { straight.step(); trace.step(&mut machine, &kernel, &sched); }
    assert_task_state(&machine.cpu, &straight.cpu, 1);
    assert_eq!(machine.memory.word(SCHED_DATA), straight.memory.word(SCHED_DATA), "resumed D matches straight work");

    trace.scenario = "e: quarantine stray line";
    let before = machine.cpu.clone();
    machine.cpu.set_lines(1 << 3);
    trace.switch(&mut machine, &kernel, &sched);
    assert_eq!(machine.cpu.intenable & (1 << 3), 0, "unowned line is masked against storms");
    assert_ne!(machine.cpu.interrupt & (1 << 3), 0, "stray source remains asserted");
    assert_eq!(machine.memory.word(layout.stray), 1, "one stray pick is reported");
    assert_eq!(machine.memory.word(CURRENT), SCHED_TCBS[0]);
    assert_task_state(&machine.cpu, &before, 0);

    trace.scenario = "f: wake already pending at block";
    trace.until(&mut machine, &kernel, &sched, |m| m.cpu.pc == lower);
    assert_eq!(machine.cpu.ps & 15, 15);
    assert_eq!(machine.memory.word(SCHED_TCBS[0] + TCB_STATE), BLOCKED);
    assert_ne!(machine.cpu.interrupt & (1 << YIELD_LINE), 0);
    assert_ne!(machine.cpu.intenable & 2, 0, "D re-armed its wake before block");
    let switches = trace.switches;
    machine.cpu.set_lines((1 << 1) | (1 << 3)); // just before block's RSIL 0
    trace.switch(&mut machine, &kernel, &sched);
    assert_eq!(trace.switches, switches + 1, "one pick handles yield and wake together");
    assert_eq!(machine.cpu.pc, resume);
    assert_eq!(machine.memory.word(CURRENT), SCHED_TCBS[0], "D is picked straight back");
    assert_eq!(machine.memory.word(SCHED_TCBS[0] + TCB_STATE), READY, "pending wake was not lost");
    assert_eq!(machine.cpu.intenable & (layout.wake_lines | (1 << 3)), 0);
    machine.cpu.set_lines(1 << 3); // leave the stray asserted to prove it cannot storm
    trace.until(&mut machine, &kernel, &sched, |m| m.cpu.pc == d.after_block);
    assert_eq!(machine.cpu.ps & 15, 0);
    trace.until(&mut machine, &kernel, &sched, |m| m.cpu.pc == d.loop_pc);
    assert_eq!(machine.memory.word(SCHED_DATA + 28) & 15, 0, "dispatcher recorded an unmasked block return");
    assert_eq!(machine.memory.word(layout.stray), 1, "held stray is counted once");
    assert_eq!(trace.switches, switches + 1, "neither wake nor stray caused a second interrupt");
    assert!(!trace.pick_counts.contains_key("c: wake dispatcher"), "pure idle wake bypasses pick");
    assert_eq!(trace.returns + 1, trace.switches, "every general picker return checked PS, windows, SP and LCOUNT");
    assert_eq!(trace.waits, 1, "idle ran only when every other task was blocked");
    for (scenario, counts) in &trace.irq_counts {
        eprintln!("{scenario}: user vector through RFE retired {counts:?}; pick retired {:?}",
            trace.pick_counts.get(scenario).map(Vec::as_slice).unwrap_or(&[]));
    }
    // Kernel/vector <256, spill retries <=90 (R3-3), this pick <=248,
    // plus <32 task instructions from RSIL 15 through interrupt entry.
    const MASKED_BOUND: u64 = 256 + 90 + (64 + 20 * 5 + 20 + 64) + 32;
    eprintln!("scheduler longest masked instruction counts: {} retired, {} attempted; bound {MASKED_BOUND} attempts (not cycles)",
        trace.longest_masked_retired, trace.longest_masked);
    assert!(trace.longest_masked_retired <= trace.longest_masked);
    assert!(trace.longest_masked < MASKED_BOUND);
}

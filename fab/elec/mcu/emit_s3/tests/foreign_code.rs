#[path = "support/island_fixture.rs"]
mod island_fixture;
#[path = "../../s3/tests/support/mod.rs"]
mod s3_fixture;

use std::sync::{Arc, OnceLock};

use fab_elec_mcu::{compile, Compiled};
use fab_elec_mcu_emit32::xtensa::{assemble, ccz, enc, Asm};
use fab_elec_mcu_emit_s3::{build_image, chip, radio, Board, BoardTime, BootTask, Image, Radio,
    SlotOptions, SRAM_START, STACK_TOP, VOICEBOX};
use fab_elec_mcu_emit_s3::image::RestartPolicy;
use fab_elec_mcu_emu::{HaltKind, Machine, PcSet, Ran, Status, Tier, Until};
use fab_elec_mcu_rt::module;
use fab_elec_mcu_s3::{Ctx, ForeignBus, ForeignCode, Next, S3Machine, VirtualFn, VirtualFns};
use fab_elec_mcu_xtensa::insn::{decode, Insn};
use fab_elec_mcu_xtensa::sim::{Cpu, Stop};

const IRAM: u32 = 0x4037_8000;
const fn iram(data: u32) -> u32 { IRAM + (data - SRAM_START) }
const DRIVER_DATA: u32 = STACK_TOP - 0x6000;
const DRIVER: u32 = iram(DRIVER_DATA);
const TABLE: u32 = DRIVER_DATA + 0x1000;
const FN_DATA: u32 = DRIVER_DATA + 0x1400;
const ROOT: u32 = iram(FN_DATA);
const CHILD: u32 = ROOT + 32;
const LEAF: u32 = CHILD + 32;
const END: u32 = LEAF + 32;
const GATE_DATA: u32 = FN_DATA + 128;
const GATE: u32 = iram(GATE_DATA);
const DONE: u32 = GATE + 128;
const VECTORS_DATA: u32 = DRIVER_DATA + 0x2000;
const VECTORS: u32 = iram(VECTORS_DATA);
const DEADLINE: u64 = 480_000_000;

fn reach(m: &mut S3Machine, pc: u32) {
    let mut marks = PcSet::new(pc, 1);
    marks.insert(pc);
    assert_eq!(m.run(&Until { tick: DEADLINE, marks: &marks, stops: &[pc], stops_after: m.now(),
        on_drive: false }), Ran::Pc, "reach {pc:#010x}: {:?} {:?}", m.status(), m.explain_foreign());
}

fn tick(m: &mut S3Machine, end: u64, marks: &PcSet) -> Ran {
    m.run(&Until { tick: end, marks, stops: &[], stops_after: 0, on_drive: false })
}

fn marked(start: u32, end: u32) -> PcSet {
    let mut marks = PcSet::new(start, end - start);
    for pc in start..end { marks.insert(pc); }
    marks
}

fn word(m: &S3Machine, address: u32) -> u32 {
    let mut b = [0; 4];
    assert!(m.read_data(address, &mut b));
    u32::from_le_bytes(b)
}

fn root(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
    match ctx.step {
        0 => Next::Call { target_at: TABLE, args: [10, 0, 0, 0, 0, 0] },
        1 => Next::Burn(5),
        2 => Next::Return(ctx.result.unwrap()),
        _ => Next::Stop("root advanced past its return"),
    }
}

fn child(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
    match ctx.step {
        0 if ctx.args[0] != 0 => Next::Call { target_at: TABLE, args: [ctx.args[0]-1, 0, 0, 0, 0, 0] },
        0 => Next::Call { target_at: TABLE+4, args: [0; 6] },
        1 => Next::Return(ctx.result.unwrap()+1),
        _ => Next::Stop("child advanced past its return"),
    }
}

fn leaf(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
    match ctx.step {
        0 => Next::Call { target_at: TABLE+8, args: [37, 0, 0, 0, 0, 0] },
        1 => Next::Return(ctx.result.unwrap()),
        _ => Next::Stop("leaf advanced past its return"),
    }
}

fn part(entry: fn(&Ctx, &dyn ForeignBus) -> Next) -> Arc<VirtualFns> { Arc::new(fns(entry)) }

fn fns(entry: fn(&Ctx, &dyn ForeignBus) -> Next) -> VirtualFns {
    VirtualFns::new(vec![ROOT..END], [
        ("root", ROOT, entry), ("child", CHILD, child), ("leaf", LEAF, leaf),
    ].into_iter().map(|(name, address, body)| VirtualFn {
        name: name.into(), address, bytes: 32, frame: 144, body,
    }).collect()).unwrap()
}

fn task_image(entry: u32, abi: radio::TaskAbi, args: [u32; 6], task_capacity: u32) -> Image {
    let radio = Radio {
        start: None,
        island: island_fixture::small_island().binding(), restart: RestartPolicy::PROVISIONAL, dispatcher_priority: 1,
        irq_stack_bytes: radio::MIN_STACK_BYTES, task_capacity, isr_capacity: radio::PACK_ISR_LINES,
        heap_bytes: 16 * 1024, timer_reserve: 8, trace_entries: 0, burst_insns: 8192,
        boot_tasks: vec![BootTask { entry, abi, priority: 2, stack_bytes: 4096, args }],
    };
    let board = Board { radio: Some(radio), layout: VOICEBOX, panel: None, i2c: None, motion: None,
        touch: None, time: BoardTime { rc_slow_hz: (136_000, 1), wdt_delay_sel: 0 } };
    let app: Compiled = compile("fn setup() {}", &chip::profile(), &[], &[]).unwrap();
    let module = module::load(&app.module, &chip::profile()).unwrap();
    build_image(&module, &board, &SlotOptions::default()).unwrap()
}

fn image() -> &'static Image {
    static IMAGE: OnceLock<Image> = OnceLock::new();
    IMAGE.get_or_init(|| task_image(DRIVER, radio::TaskAbi::Call0, [0; 6], 3))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault { None, Gate, Spill, EveryCycle, Interrupt }

fn rsr(a: &mut Asm, reg: u8, sr: u32) {
    a.op24(enc::rsr(0, 3, 0, sr, reg));
}

fn timer_handler(clobber: bool) -> Vec<u8> {
    assemble(&mut Asm::new(0, 8), |a| {
        a.wsr(2, 209); // EXCSAVE1
        if clobber {
            a.movi(2, 0);
            a.wsr(2, 2); // LCOUNT
        }
        rsr(a, 2, 234); // CCOUNT at the beginning of this instruction
        a.addi(2, 2, 6);
        a.wsr(2, 240);
        rsr(a, 2, 209);
        a.rfe(); // The comparator matches after one guest instruction.
    })
}

fn machine(fill: u8, fault: Fault, entry: u32) -> S3Machine { machine_on(image(), fill, fault, entry) }

fn machine_on(image: &Image, fill: u8, fault: Fault, entry: u32) -> S3Machine {
    let slot = &image.slot;
    let symbols = slot.symbols.radio.as_ref().unwrap();
    let island = island_fixture::small_island();
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &s3_fixture::profile(53));
    let code = assemble(&mut Asm::new(0, 8), |a| {
        a.literal_table();
        if matches!(fault, Fault::Spill | Fault::EveryCycle | Fault::Interrupt) {
            a.op24(enc::rrr(0, 0, 0, 6, 15, 6)); // RSIL a6, 15 while installing VECBASE
            a.mov_reg_i32(4, VECTORS);
            a.wsr(4, 231);
            a.rsync();
        }
        if matches!(fault, Fault::EveryCycle | Fault::Interrupt) {
            a.mov_reg_i32(6, 1 << 6);
            a.wsr(6, 228);
            rsr(a, 6, 234);
            a.addi(6, 6, 8);
            a.wsr(6, 240);
            a.op24(enc::rrr(0, 0, 0, 6, 0, 6)); // RSIL a6, 0
        }
        a.mov_reg_i32(2, entry);
        a.mov_reg_i32(8, symbols.shim);
        a.callx0(8);
        // Equalise the intentionally different input bytes for the whole-machine comparison.
        a.mov_reg_i32(5, FN_DATA);
        a.mov_reg_i32(6, 0);
        for off in (0..END-ROOT).step_by(4) { a.s32i(6, 5, off); }
        a.mov_reg_i32(8, DONE);
        a.jx(8);
    });
    assert!(slot.layout.regions[0].used <= u64::from(DRIVER_DATA));
    assert!(VECTORS_DATA+0x400 <= STACK_TOP-slot.layout.stack_bytes);
    assert!(code.len() <= (TABLE-DRIVER_DATA) as usize && m.seed_data(DRIVER_DATA, &code));
    assert!(m.seed_data(FN_DATA, &[fill; (END-ROOT) as usize]));
    let gate = assemble(&mut Asm::new(0, 8), |a| {
        // CALLX8 has written the link but ENTRY has yet to rotate. This is
        // the caller's a7, exactly the register the gate promises to keep.
        if fault == Fault::Gate { a.addi(7, 7, 1); }
        a.entry(1, 32);
        a.addi(2, 2, 4);
        a.retw();
    });
    assert!(m.seed_data(GATE_DATA, &gate));
    assert!(m.seed_data(GATE_DATA + 128, &enc::j(-4).to_le_bytes()[..3]));
    if matches!(fault, Fault::Spill | Fault::EveryCycle | Fault::Interrupt) {
        let mut vectors = slot.bytes[..radio::WINDOW_BLOCK_SIZE].to_vec();
        if fault == Fault::Spill {
            // Redirect the real OF8's last spill from the a7 slot to the a6 slot.
            let at = radio::OF8_OFFSET + 8 * 3;
            assert_eq!(&vectors[at..at+3], &enc::s32e(7, 0, -20).to_le_bytes()[..3]);
            vectors[at..at+3].copy_from_slice(&enc::s32e(7, 0, -24).to_le_bytes()[..3]);
        }
        assert!(m.seed_data(VECTORS_DATA, &vectors));
    }
    if matches!(fault, Fault::EveryCycle | Fault::Interrupt) {
        let handler = timer_handler(fault == Fault::Interrupt);
        for vector in [0x300, 0x340] { assert!(m.seed_data(VECTORS_DATA+vector, &handler)); }
    }
    for (i, val) in [CHILD, LEAF, GATE].into_iter().enumerate() {
        assert!(m.seed_data(TABLE+i as u32*4, &val.to_le_bytes()));
    }
    m
}

#[test]
fn each_machine_reads_its_own_tables_env_in_every_step_and_clone() {
    // Burn first, so the clone below is taken inside the activation and reads env afterwards.
    fn env_root(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
        match ctx.step {
            0 => Next::Burn(3),
            1 => Next::Return(ctx.env[0]),
            _ => Next::Stop("env root advanced past its return"),
        }
    }
    let other = task_image(DRIVER, radio::TaskAbi::Call0, [0; 6], 4);
    assert_ne!(other.flash, image().flash, "the two machines run different images");
    let mut a = machine(0xff, Fault::None, ROOT).with_foreign(Arc::new(fns(env_root).with_env(vec![41])));
    let mut b = machine_on(&other, 0xff, Fault::None, ROOT).with_foreign(Arc::new(fns(env_root).with_env(vec![73])));
    reach(&mut a, ROOT + 3);
    reach(&mut b, ROOT + 3);
    let mut clone = a.clone();
    for (m, env) in [(&mut a, 41), (&mut b, 73), (&mut clone, 41)] {
        reach(m, DONE);
        assert_eq!(m.cpu().ar[2], env, "each table's own env word");
    }
}

#[test]
fn nested_calls_burn_and_real_window_vectors_replay_on_both_tiers() {
    let mut reference = machine(0xff, Fault::None, ROOT).with_foreign(part(root));
    reach(&mut reference, ROOT);
    let mut native = reference.clone();
    reference.set_tier(Tier::Ref);
    native.set_tier(Tier::Native);
    let vb = reference.cpu().vecbase;
    let stops = [vb+0x80, vb+0xc0];
    for (i, vector) in stops.into_iter().enumerate() {
        reach(&mut reference, vector);
        reach(&mut native, vector);
        assert!((ROOT..END).contains(&reference.cpu().epc1), "window vector {i} interrupted a virtual PC");
        assert_eq!(reference.same_state(&native), None);
    }
    reach(&mut reference, DONE);
    reach(&mut native, DONE);
    assert_eq!(reference.cpu().ar[2], 52);
    assert_eq!(reference.same_state(&native), None, "CPU, counts, ticks, trace and SRAM");
}

#[test]
fn virtual_call_reads_the_bound_word_and_runs_the_slots_real_thunk_and_gate() {
    fn via_gate(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
        let island = island_fixture::small_island();
        let index = island.gate_names.iter().position(|name| name == "free").expect("fixture free gate");
        match ctx.step {
            0 => Next::Call { target_at: island.header.gate_table + index as u32 * 4, args: [0; 6] },
            1 => Next::Return(71),
            _ => Next::Stop("unexpected step after free(NULL)"),
        }
    }
    let mut m = machine(0xff, Fault::None, ROOT).with_foreign(part(via_gate));
    let island = island_fixture::small_island();
    let index = island.gate_names.iter().position(|name| name == "free").unwrap();
    let symbols = image().slot.symbols.radio.as_ref().unwrap();
    let thunk = symbols.island_thunks[index].1;
    let gate = symbols.gate_reports.iter().find(|g| g.name == "free").unwrap().address;
    reach(&mut m, ROOT);
    assert_eq!(word(&m, island.header.gate_table + index as u32 * 4), thunk);
    reach(&mut m, thunk);
    reach(&mut m, gate);
    reach(&mut m, ROOT+9);
    reach(&mut m, DONE);
    assert_eq!(m.cpu().ar[2], 71);
}

#[test]
fn windowed_task_blocks_in_a_semaphore_gate_and_resumes_at_the_virtual_return_pc() {
    // Three virtual tasks, so no host state: the boot task makes a semaphore and a
    // higher-priority waiter; the waiter makes a lower-priority giver, then blocks
    // in take; the giver's give wakes it. Handles travel only as task arguments.
    // A task never returns (task_exit is a fault), so the boot task and the giver
    // park in an endless delay; only the waiter's final return reaches task_exit.
    const FOREVER: u32 = u32::MAX;
    fn fixture() -> &'static Image {
        static IMAGE: OnceLock<Image> = OnceLock::new();
        // Dispatcher, idle, the boot task, waiter and giver.
        IMAGE.get_or_init(|| task_image(ROOT, radio::TaskAbi::Windowed, [0; 6], 5))
    }
    fn osi(name: &str) -> u32 {
        let row = radio::adapter::OSI_FIELDS.iter().position(|&(field, _)| field == name).unwrap();
        fixture().slot.layout.radio.as_ref().unwrap().osi_table + row as u32 * 4
    }
    fn boot(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
        match ctx.step {
            0 => Next::Call { target_at: osi("_semphr_create"), args: [1, 0, 0, 0, 0, 0] },
            // IDF priority 5 is above the boot task's: the waiter runs at once.
            1 => Next::Call { target_at: osi("_task_create"), args: [CHILD, 0, 2048, ctx.result.unwrap(), 5, 0] },
            2 => Next::Call { target_at: osi("_task_delay"), args: [FOREVER, 0, 0, 0, 0, 0] },
            _ => Next::Stop("boot task woke from its endless delay"),
        }
    }
    fn waiter(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
        match ctx.step {
            // IDF priority 0 is below the boot task's: the giver waits its turn.
            0 => Next::Call { target_at: osi("_task_create"), args: [LEAF, 0, 2048, ctx.args[0], 0, 0] },
            1 => Next::Call { target_at: osi("_semphr_take"), args: [ctx.args[0], FOREVER, 0, 0, 0, 0] },
            2 => Next::Return(73),
            _ => Next::Stop("waiter advanced past its return"),
        }
    }
    fn giver(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
        match ctx.step {
            0 => Next::Call { target_at: osi("_semphr_give"), args: [ctx.args[0], 0, 0, 0, 0, 0] },
            1 => Next::Call { target_at: osi("_task_delay"), args: [FOREVER, 0, 0, 0, 0, 0] },
            _ => Next::Stop("giver woke from its endless delay"),
        }
    }
    let part = Arc::new(VirtualFns::new(vec![ROOT..END], [("boot", ROOT, boot as fn(&Ctx, &dyn ForeignBus) -> Next),
        ("waiter", CHILD, waiter), ("giver", LEAF, giver)].into_iter().map(|(name, address, body)| VirtualFn {
        name: name.into(), address, bytes: 32, frame: 144, body,
    }).collect()).unwrap());
    let image = fixture();
    let r = image.slot.layout.radio.as_ref().unwrap();
    let symbols = image.slot.symbols.radio.as_ref().unwrap();
    let boot_task = r.boot_tasks[0];
    let thunk = |name: &str| symbols.thunks.iter().find(|(n, _)| n == name).unwrap().1;
    let take = thunk("osi_semphr_take");
    assert!(symbols.gate_reports.iter().find(|g| g.name == "osi_semphr_take").unwrap().blocks);
    let mut reference = island_fixture::machine_with_island(&image.flash, &island_fixture::small_island().image,
        &s3_fixture::profile(53)).with_foreign(part);
    assert!(reference.seed_data(FN_DATA, &[0xff; (END - ROOT) as usize]));
    reference.set_tier(Tier::Ref);
    reach(&mut reference, ROOT);
    assert_eq!(word(&reference, r.current), boot_task);
    assert_eq!(word(&reference, osi("_semphr_take")), take);
    let mut native = reference.clone();
    native.set_tier(Tier::Native);
    for m in [&mut reference, &mut native] {
        reach(m, take);
        let waiter = word(m, r.current);
        assert_ne!(waiter, boot_task, "the waiter preempted the boot task at its creation");
        let semaphore = m.cpu().ar[10];
        assert_ne!(semaphore, 0, "the semaphore travelled as the waiter's argument");
        reach(m, symbols.block);
        let thunk_continuation = m.cpu().ar[0]; // block's CALL0 return address in the thunk
        assert_eq!(word(m, waiter + radio::TCB_STATE), radio::BLOCKED);
        reach(m, LEAF);
        assert_ne!(word(m, r.current), waiter);
        assert_eq!(word(m, waiter + radio::TCB_STATE), radio::BLOCKED, "the giver runs while the waiter is blocked");
        assert_eq!(m.cpu().ar[10], semaphore);
        reach(m, thunk_continuation);
        assert_eq!(word(m, r.current), waiter);
        reach(m, CHILD + 9);
        assert_eq!(m.cpu().ar[10], 1, "take returned pdTRUE through the thunk to F+9");
        reach(m, symbols.task_exit);
        assert_eq!(m.cpu().ar[10], 73, "virtual RETW returned to the task exit trampoline");
    }
    assert_eq!(reference.same_state(&native), None, "switch, wake, thunk return and task exit");
}

#[test]
fn binding_and_unclaimed_pc_name_the_symbol() {
    let tiny = VirtualFn { name: "too_short".into(), address: ROOT, bytes: 14, frame: 144, body: root };
    assert!(VirtualFns::new(vec![ROOT..END], vec![tiny]).err().unwrap().contains("too_short"));
    let mut m = machine(0xff, Fault::None, ROOT+17).with_foreign(part(root));
    assert_eq!(tick(&mut m, DEADLINE, &marked(ROOT, END)), Ran::Halted);
    assert!(matches!(m.status(), Status::Halted(h) if h.pc == ROOT+17 && h.what == "foreign code with no stand-in"));
    assert!(m.explain_foreign().unwrap().contains("root+0x11"));
}

#[test]
fn clobbered_gate_register_spill_slot_and_interrupt_state_are_named() {
    for (fault, reg) in [(Fault::Gate, "a7"), (Fault::Spill, "a6"), (Fault::Interrupt, "LCOUNT")] {
        let mut m = machine(0xff, fault, ROOT).with_foreign(part(root));
        assert_eq!(tick(&mut m, DEADLINE, &marked(ROOT, END)), Ran::Halted);
        assert!(matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Unmodeled && h.what == "foreign activation canary"));
        let why = m.explain_foreign().unwrap();
        let call = match fault {
            Fault::Gate => format!("CALLX8 to {GATE:#010x} in leaf"),
            Fault::Spill => "child in ".to_string(),
            Fault::Interrupt => "burn in root".to_string(),
            _ => unreachable!(),
        };
        assert!(why.starts_with(reg) && why.contains(&call), "{why}");
    }
}

#[test]
fn illegal_and_executable_backing_bytes_give_identical_state_and_trace() {
    let mut illegal = machine(0xff, Fault::None, ROOT).with_foreign(part(root));
    let mut nops = illegal.clone();
    let mut bytes = [0; (END-ROOT) as usize];
    for insn in bytes.chunks_exact_mut(3) { insn.copy_from_slice(&[0xf0, 0x20, 0x00]); }
    assert!(nops.seed_data(FN_DATA, &bytes));
    let marks = marked(ROOT, DONE+1);
    for m in [&mut illegal, &mut nops] {
        reach(m, ROOT);
        assert_eq!(m.run(&Until { tick: DEADLINE, marks: &marks, stops: &[DONE], stops_after: 0,
            on_drive: false }), Ran::Pc);
        assert_eq!(m.cpu().ar[2], 52);
        // The two inputs also differed in the old page bytes the cleanup store
        // journaled. Mark the common end state before comparing whole machines.
        m.mark();
    }
    assert_eq!(illegal.same_state(&nops), None, "includes trace, retired instructions and cycles");
}

#[test]
fn timer_offered_between_instructions_at_all_five_virtual_pcs() {
    let mut r = machine(0xff, Fault::EveryCycle, ROOT).with_foreign(part(root));
    reach(&mut r, DRIVER);
    let mut n = r.clone();
    r.set_tier(Tier::Ref);
    n.set_tier(Tier::Native);
    let marks = marked(ROOT, VECTORS+0x380);
    let stops = [DONE, VECTORS+0x300, VECTORS+0x340];
    let mut interrupted = [false; 5];
    let mut burn_interrupts = Vec::new();
    loop {
        for m in [&mut r, &mut n] {
            assert_eq!(m.run(&Until { tick: DEADLINE, marks: &marks, stops: &stops,
                stops_after: m.now(), on_drive: false }), Ran::Pc, "{:?}", m.status());
        }
        assert_eq!((r.now(), r.pc(), r.cpu().epc1), (n.now(), n.pc(), n.cpu().epc1));
        if r.pc() == DONE { break; }
        if r.cpu().epc1 == ROOT + 3 && r.cpu().ps & (3 << 16) == 0 {
            burn_interrupts.push(r.cpu().ccount);
        }
        for base in [ROOT, CHILD, LEAF] {
            let off = r.cpu().epc1.wrapping_sub(base);
            if off <= 12 && off % 3 == 0 { interrupted[(off/3) as usize] = true; }
        }
    }
    assert_eq!(interrupted, [true; 5]);
    assert_eq!(burn_interrupts.len(), 5, "an interrupt after each burned cycle");
    assert!(burn_interrupts.windows(2).all(|w| w[1].wrapping_sub(w[0]) == 7),
        "six handler instructions and one burn between interrupt entries");
    assert_eq!(r.cpu().ar[2], 52);
    assert_eq!(r.same_state(&n), None, "interrupt trace, SRAM and all execution state");
}

#[test]
fn each_step_rewinds_and_a_clone_inside_a_burn_runs_independently() {
    let mut m = machine(0xff, Fault::None, ROOT).with_foreign(part(root));
    reach(&mut m, ROOT);
    let vb = m.cpu().vecbase;
    let mut vectors: Vec<_> = radio::WINDOW_VECTOR_OFFSETS.iter().map(|&off| vb + off as u32).collect();
    vectors.extend([vb + m.cpu().config.vectors.kernel, vb + m.cpu().config.vectors.user]);
    vectors.sort_unstable();
    let marks = marked(vb, *vectors.last().unwrap() + 1);
    let step = |m: &mut S3Machine| {
        let start = m.now();
        // Vector entry costs zero ticks. Expose that boundary before the first
        // handler instruction; when resuming it, allow that instruction to run.
        let stops_after = if vectors.binary_search(&m.pc()).is_ok() { start } else { 0 };
        let ran = m.run(&Until { tick: start + 1, marks: &marks, stops: &vectors, stops_after, on_drive: false });
        assert!(matches!(ran, Ran::Tick | Ran::Pc), "{:?}", m.status());
        ran
    };
    let mut sibling: Option<S3Machine> = None;
    let mut retried_vector = false;
    while m.pc() != DONE {
        assert!(m.now() < DEADLINE);
        m.mark();
        let start = m.now();
        let ran = step(&mut m);
        retried_vector |= m.now() == start && ran == Ran::Pc;
        let expected = m.clone();
        m.rewind();
        assert_eq!(step(&mut m), ran);
        assert_eq!(m.same_state(&expected), None, "step ending at {:#010x}", m.pc());
        if let Some(clone) = &mut sibling {
            clone.mark();
            assert_eq!(step(clone), ran);
            assert_eq!(m.same_state(clone), None, "original and mid-burn clone");
        } else if m.pc() == ROOT+3 && m.cpu().ps & (3 << 16) == 0 {
            sibling = Some(m.clone());
        }
    }
    assert!(retried_vector && sibling.is_some());
}

struct SuppliedWords { start: u32, words: Vec<u32> }
impl ForeignCode for SuppliedWords {
    fn contains(&self, pc: u32) -> bool { pc.wrapping_sub(self.start) < self.words.len() as u32 * 3 }
    fn at(&self, pc: u32, _: &mut Cpu, _: &mut dyn ForeignBus) -> Result<(Insn, u32), Stop> {
        let offset = pc - self.start;
        if offset % 3 != 0 { return Err(Stop::Unmodeled { pc, what: "unexpected fixture offset" }); }
        Ok(decode(self.words[(offset / 3) as usize]).unwrap())
    }
    fn explain(&self, _: u32, _: &Cpu, _: &dyn ForeignBus) -> Option<String> { None }
}

#[test]
fn flash_instruction_starting_outside_the_range_cannot_decode_across_it() {
    let p = s3_fixture::profile(61);
    let mut flash = vec![0xff; 0x500];
    for off in [0x400, 0x404] { flash[off..off+4].copy_from_slice(&0xaedb_041du32.to_le_bytes()); }
    flash[0x408] = 0x06; // three-byte J whose operand straddles the boundary
    let initial = S3Machine::new(Arc::new(flash), &p);
    let f = Arc::new(VirtualFns::new(vec![p.entry+2..p.entry+16], vec![]).unwrap());
    let mut reference = initial.clone().with_foreign(f.clone());
    let mut native = initial.with_foreign(f);
    reference.set_tier(Tier::Ref);
    native.set_tier(Tier::Native);
    for m in [&mut reference, &mut native] {
        assert_eq!(tick(m, 1000, &marked(p.entry, p.entry+16)), Ran::Halted);
        assert!(matches!(m.status(), Status::Halted(h) if h.pc == p.entry
            && h.what == "instruction crosses foreign code boundary"));
        assert_eq!(m.cpu().instret, 0);
    }
    assert_eq!(reference.same_state(&native), None);
}

#[test]
fn supplied_waiti_and_empty_table_preserve_timer_wake_counts_and_trace() {
    let p = s3_fixture::profile(61);
    let base = s3_fixture::FLASH;
    let mut program = s3_fixture::Program::new();
    program.sr(231, base + 0x1000);
    program.sr(228, 1 << 6);
    program.emit(enc::rsr(0, 3, 0, 234, 2));
    program.emit(enc::addi(2, 2, 100));
    program.emit(enc::wsr(2, 240));
    let wait = program.emit(enc::waiti(0));
    let successor = enc::addi(5, 5, 0);
    program.emit(successor);
    let done = program.halt();
    program.image.resize(0x2000, 0xff);
    let irq = assemble(&mut Asm::new(0, 8), |a| {
        a.movi(3, 0);
        a.wsr(3, 228);
        a.rfe();
    });
    for off in [0x1300, 0x1340] { program.image[off..off+irq.len()].copy_from_slice(&irq); }
    let initial = S3Machine::new(program.finish(), &p);
    let sleep = Arc::new(SuppliedWords { start: wait, words: vec![enc::waiti(0), successor] });
    let mut plain = initial.clone();
    let mut empty = initial.clone().with_foreign(Arc::new(VirtualFns::new(vec![], vec![]).unwrap()));
    let mut reference = initial.clone().with_foreign(sleep.clone());
    let mut native = initial.with_foreign(sleep);
    plain.set_tier(Tier::Native);
    empty.set_tier(Tier::Native);
    reference.set_tier(Tier::Ref);
    native.set_tier(Tier::Native);
    let marks = marked(p.entry, base+0x1380);
    let mut wakes = Vec::new();
    for m in [&mut plain, &mut empty, &mut reference, &mut native] {
        reach(m, wait);
        let before = (m.now(), m.cpu().ccount, m.cpu().instret);
        assert_eq!(tick(m, before.0+1, &marks), Ran::Tick);
        let Status::Sleeping { wake: Some(wake) } = m.status() else { panic!("expected timer wake: {:?}", m.status()) };
        assert!(wake > m.now()+1);
        assert_eq!(tick(m, wake, &marks), Ran::Tick);
        assert_eq!(m.cpu().instret, before.2+1, "only WAITI retired during sleep");
        let counts = (wake, m.cpu().ccount.wrapping_sub(before.1), m.cpu().instret);
        assert!(counts.1 > 1, "CCOUNT advanced across the WAITI span");
        wakes.push(counts);
        reach(m, done);
    }
    assert!(wakes.windows(2).all(|w| w[0] == w[1]), "{wakes:?}");
    for other in [&empty, &reference, &native] { assert_eq!(plain.same_state(other), None); }
}

#[test]
fn shared_native_unit_is_never_taken_or_built_for_a_mixed_page() {
    const TRIPS: u32 = 5000;
    let mut program = s3_fixture::Program::new();
    program.load(4, TRIPS);
    program.emit(enc::movi(5, 0));
    let head = program.emit(enc::addi(4, 4, -1));
    let supplied = program.emit(enc::addi(5, 5, 1));
    let branch = program.pc();
    program.emit(enc::bccz(ccz::NE, 4, head as i32 - branch as i32 - 4));
    let done = program.halt();
    let mut marks = PcSet::new(branch, done - branch + 1);
    marks.insert(branch);
    marks.insert(done);
    let initial = S3Machine::new(program.finish(), &s3_fixture::profile(67));
    let finish = |m: &mut S3Machine| {
        assert_eq!(m.run(&Until { tick: DEADLINE, marks: &marks, stops: &[done],
            stops_after: 0, on_drive: false }), Ran::Pc, "{:?}", m.status());
    };
    let mut producer = initial.clone();
    producer.set_tier(Tier::Native);
    let previous = fab_elec_mcu_emu::jit::inline_compile();
    fab_elec_mcu_emu::jit::set_inline_compile(true);
    finish(&mut producer);
    fab_elec_mcu_emu::jit::set_inline_compile(previous);
    assert_eq!(producer.cpu().ar[5], TRIPS);
    assert!(producer.has_native_entry(head, &marks), "the ordinary loop compiled with both instructions");

    // Same flash Arc, MMU and marks. A cold ordinary consumer can obtain the
    // published unit long before it has executed the 4096 instructions to build one.
    let mut consumer = initial.clone();
    consumer.set_tier(Tier::Native);
    assert_eq!(tick(&mut consumer, 1_000, &marks), Ran::Tick);
    assert!(consumer.cpu().instret < 4096 && consumer.has_native_entry(head, &marks));

    // Enter the page at an owned instruction. The next instruction must add
    // seven on every trip, even though the cached unit inlines an add of one.
    let part = Arc::new(SuppliedWords { start: supplied, words: vec![enc::addi(5, 5, 7)] });
    let mut native = initial.with_foreign(part);
    let mut reference = native.clone();
    native.set_tier(Tier::Native);
    reference.set_tier(Tier::Ref);
    finish(&mut native);
    finish(&mut reference);
    assert_eq!(native.cpu().ar[5], 7 * TRIPS);
    assert_eq!(native.cpu().instret, producer.cpu().instret);
    assert_eq!(native.same_state(&reference), None);
    assert!(!native.native_page_tracked(head), "no mixed-page count or compile job");
    assert!(!native.has_native_entry(head, &marks), "no shared or locally built mixed-page unit");
}


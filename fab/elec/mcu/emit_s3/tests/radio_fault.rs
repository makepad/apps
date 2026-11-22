use fab_elec_mcu_emit_s3::radio::*;
use fab_elec_mcu_emit_s3::{runtime::DISPATCHER_LINES, Slot, SLOT_BASE};
use fab_elec_mcu_emu::{Machine, PcSet, Ran, Until};
use fab_elec_mcu_island::symbolize;
use fab_elec_mcu_rt::watch::Event;
use fab_elec_mcu_s3::S3Machine;

#[path = "support/island_fixture.rs"]
mod island_fixture;
#[path = "../../s3/tests/support/mod.rs"]
mod s3_fixture;

#[test]
fn collector_orders_repeats_and_checks_length_and_padding() {
    let first = Event::RadioRecord { index: 0, words: [6, 11, 12, 13] };
    let last = Event::RadioRecord { index: 1, words: [14, 15, 0, 0] };
    let mut collector = RecordCollector::new();
    assert_eq!(collector.push(Event::Time { tick_hi: 1 }), Ok(None), "unrelated watch events do not start a record");
    assert_eq!(collector.push(last), Ok(None), "later frames may precede the length word");
    assert_eq!(collector.push(last), Ok(None), "an equal repeat before completion is harmless");
    assert_eq!(collector.push(first), Ok(Some(vec![6, 11, 12, 13, 14, 15])), "completion orders frames and removes zero padding");
    assert_eq!(collector.push(first), Ok(None), "a complete record is yielded only once");
    assert_eq!(collector.push(last), Ok(None), "equal repeats remain valid after completion");
    assert!(collector.push(Event::RadioRecord { index: 1, words: [14, 16, 0, 0] }).is_err(),
        "a differing repeat is an error even after completion");

    let bad_padding = Event::RadioRecord { index: 1, words: [14, 15, 1, 0] };
    let mut before_length = RecordCollector::new();
    assert_eq!(before_length.push(bad_padding), Ok(None), "padding cannot be judged until the length arrives");
    assert!(before_length.push(first).is_err(), "the length validates padding in already buffered frames");
    let mut after_length = RecordCollector::new();
    assert_eq!(after_length.push(first), Ok(None), "length alone does not complete a missing frame");
    assert!(after_length.push(bad_padding).is_err(), "new frames must have zero padding");
    assert!(after_length.push(Event::RadioRecord { index: 2, words: [0; 4] }).is_err(), "indices beyond the declared length are refused");
    assert_eq!(after_length.push(last), Ok(Some(vec![6, 11, 12, 13, 14, 15])), "invalid input does not replace accepted words");
    let mut outside = RecordCollector::new();
    assert_eq!(outside.push(Event::RadioRecord { index: 2, words: [0; 4] }), Ok(None), "an early index is buffered before length");
    assert!(outside.push(first).is_err(), "length also validates already buffered indices");
    for length in [0, 262_145, u32::MAX] {
        assert!(RecordCollector::new().push(Event::RadioRecord { index: 0, words: [length, 0, 0, 0] }).is_err(),
            "zero or unframeable length {length} is refused");
    }
    let mut gap = RecordCollector::new();
    assert_eq!(gap.push(Event::RadioRecord { index: 0, words: [12, 1, 2, 3] }), Ok(None), "a twelve-word record needs three frames");
    assert_eq!(gap.push(Event::RadioRecord { index: 2, words: [8, 9, 10, 11] }), Ok(None), "a hole prevents premature completion");
    assert_eq!(gap.push(Event::RadioRecord { index: 1, words: [4, 5, 6, 7] }), Ok(Some(vec![12, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11])),
        "filling the hole completes the record in word order");
}

#[test]
fn decoder_preserves_registers_trace_and_return_addresses() {
    let mut record = vec![0; FAULT_WORDS as usize];
    record[FAULT_ISLAND_GUARD as usize] = GUARD;
    record[FAULT_ISLAND_STATUS as usize] = ISLAND_READY;
    record[..11].copy_from_slice(&[FAULT_WORDS, 6, 0x4000_1004, 0x1234, 0x40020, 0x2000, 2, 0x3000, 7, 0x81, 2]);
    for (i, word) in record[FAULT_AR as usize..FAULT_DEPTH as usize].iter_mut().enumerate() { *word = 0x100 + i as u32; }
    record[FAULT_DEPTH as usize] = 2;
    record[FAULT_BACKTRACE as usize..FAULT_BACKTRACE as usize + 4].copy_from_slice(&[0x4000_1004, 0x5000, 0x4000_100c, 0x5040]);
    for entry in 0..FAULT_TRACE_ENTRIES as usize {
        let at = FAULT_TRACE as usize + entry * TRACE_ENTRY_WORDS as usize;
        record[at..at + 8].copy_from_slice(&[100 - entry as u32, 1, 2, 3, 4, 5, 6, 900 - entry as u32]);
    }
    let fault = RadioFault::decode(&record).expect("valid fault decodes");
    assert_eq!(fault.island_status, ISLAND_READY);
    assert_eq!((fault.cause, fault.pc, fault.address, fault.ps), (6, 0x4000_1004, 0x1234, 0x40020), "exception header fields retain their values");
    assert_eq!((fault.task, fault.task_index, fault.context, fault.window_base, fault.window_start, fault.trace_head),
        (0x2000, 2, 0x3000, 7, 0x81, 2), "task and window header fields retain their values");
    assert_eq!(fault.ars.as_slice(), &record[FAULT_AR as usize..FAULT_DEPTH as usize], "ARs stay WB-relative without rotation");
    assert_eq!(fault.backtrace, [(0x4000_1004, 0x5000), (0x4000_100c, 0x5040)], "PC/SP pairs keep their captured order and unadjusted return PC");
    assert_eq!(fault.trace, [TraceEntry { gate: 100, args: [1, 2, 3, 4, 5, 6], ccount: 900 },
        TraceEntry { gate: 99, args: [1, 2, 3, 4, 5, 6], ccount: 899 }], "only head-counted entries appear, newest first");
    let map = "0x40001004 12 TEXT .text.fixture fixture\n0x40001000 32 TEXT/FUNC fixture_call fixture\n";
    let printed = fault.with_map(map).to_string();
    let intact = format!("island_guard={GUARD:#010x}");
    assert!(printed.lines().any(|line| line == intact) && !printed.contains("damaged"), "an intact armed guard needs no annotation");
    assert!(printed.contains("divide by zero"), "Display uses a known cause name");
    assert!(printed.contains("fixture_call+0x4") && printed.contains("fixture_call+0xc"), "Display uses function offsets for both PCs without subtracting the call size");
    assert!(printed.contains("a return address points after its call"), "Display explains the captured return address");
    record[FAULT_CAUSE as usize] = 77;
    assert!(RadioFault::decode(&record).unwrap().to_string().contains("cause 77"), "unknown causes retain their numeric value");
    for (head, count) in [(0, 0), (1, 1), (8, 8), (9, 8), (u32::MAX, 8)] {
        record[FAULT_TRACE_HEAD as usize] = head;
        assert_eq!(RadioFault::decode(&record).unwrap().trace.len(), count, "head {head} controls trace count capped at eight");
    }
    for (cause, name) in [(0, "illegal"), (1, "syscall"), (2, "instruction fetch"), (3, "load/store error"),
        (6, "divide by zero"), (9, "alignment"), (32, "coprocessor 0 disabled"), (FAULT_GUARD, "guard")]
    { assert_eq!(cause_name(cause), Some(name), "model cause {cause} has its ISA name"); }
    assert_eq!(cause_name(77), None, "an unknown cause has no static name");

    assert!(RadioFault::decode(&record[..record.len() - 1]).is_err(), "a short record is refused");
    let mut long = record.clone(); long.push(0);
    assert!(RadioFault::decode(&long).is_err(), "an overlong record is refused");
    for old_length in [164, 165] {
        record[FAULT_LENGTH as usize] = old_length;
        assert!(RadioFault::decode(&record).is_err(), "the length word must match the fixed layout");
        assert!(RadioFault::decode(&record[..old_length as usize]).is_err(), "the former {old_length}-word record is refused");
    }
    record[FAULT_LENGTH as usize] = FAULT_WORDS;
    record[FAULT_DEPTH as usize] = BACKTRACE_DEPTH + 1;
    assert!(RadioFault::decode(&record).is_err(), "depth greater than twelve is refused");
    record[FAULT_DEPTH as usize] = 1;
    assert!(RadioFault::decode(&record).is_err(), "nonzero words beyond the declared backtrace depth are refused");
    record[FAULT_DEPTH as usize] = BACKTRACE_DEPTH;
    assert_eq!(RadioFault::decode(&record).unwrap().backtrace.len(), 12, "the maximum backtrace depth is accepted");

    record[FAULT_ISLAND_GUARD as usize] = 0;
    for (status, name) in [(ISLAND_NOT_LOADED, "not loaded"), (ISLAND_ABSENT, "absent"), (ISLAND_MISMATCH, "mismatch")] {
        record[FAULT_ISLAND_STATUS as usize] = status;
        let fault = RadioFault::decode(&record).unwrap();
        assert_eq!(fault.island_status, status);
        let printed = fault.to_string();
        let expected = format!("island_guard=0x00000000 (not armed: island {name})");
        assert!(printed.lines().any(|line| line == expected) && !printed.contains("damaged"),
            "the unarmed guard retains its value and reports island {name}");
    }
    record[FAULT_ISLAND_STATUS as usize] = 4;
    let error = RadioFault::decode(&record).unwrap_err();
    assert!(error.contains("island status 4"), "{error}");
}

fn word(m: &S3Machine, address: u32) -> u32 {
    let mut bytes = [0; 4];
    assert!(m.read_data(address, &mut bytes), "read {address:#010x}");
    u32::from_le_bytes(bytes)
}

// Observe the real reset and route stores and WSR.INTENABLE through state publication.
fn check_quiesce_writes(m: &mut S3Machine, slot: &Slot, state: u32, initial_reset: u32) {
    use kernel::{INTR_MATRIX, MODEM_RESET_FIELD, SYSCON_WIFI_RST_EN, WIFI_SOURCES};
    use fab_elec_mcu_xtensa::insn::{decode, Op};
    let r = slot.layout.radio.as_ref().unwrap();
    let s = slot.symbols.radio.as_ref().unwrap();
    let dispatcher_call = m.pc() == s.radio_stop;
    let pending = word(m, r.stop_pending);
    let entry_ps = m.cpu().ps;
    let keep = m.cpu().intenable & (DISPATCHER_LINES | (1 << YIELD_LINE));
    let mut expected = vec![(SYSCON_WIFI_RST_EN, initial_reset | MODEM_RESET_FIELD)];
    expected.extend((0..WIFI_SOURCES).map(|i| (INTR_MATRIX + i * 4, 16)));
    expected.push((228, keep));
    let mut writes = Vec::new();
    let deadline = m.now() + m.tick_hz() / 100;
    while word(m, r.radio_state) != state {
        assert!(m.now() < deadline, "missing publication of radio state {state}");
        assert_ne!(m.pc(), s.pick, "quiesce finishes before any pick");
        if dispatcher_call { assert_eq!(m.cpu().ps, entry_ps, "dispatcher stop preserves PS throughout"); }
        if let Some(bytes) = m.pc().checked_sub(SLOT_BASE).and_then(|at| slot.bytes.get(at as usize..))
            .filter(|b| b.len() >= 3 && m.cpu().will_execute()) {
            let (i, _) = decode(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])).unwrap();
            let ar = &m.cpu().ar;
            if matches!(i.op, Op::S32i | Op::S32iN) {
                let address = ar[i.s as usize].wrapping_add(i.imm as u32);
                if state == RADIO_STOPPED {
                    assert!(!(r.fault..r.stop_cause).contains(&address), "stop wrote the fault record at {address:#010x}");
                    if !dispatcher_call && address == SYSCON_WIFI_RST_EN {
                        assert_eq!((word(m, r.stop_pending), word(m, r.stop_cause)), (0, pending),
                            "the handler consumes the request before quiescing the modem");
                    }
                }
                if address == SYSCON_WIFI_RST_EN || (INTR_MATRIX..INTR_MATRIX + 4 * WIFI_SOURCES).contains(&address) {
                    writes.push((address, ar[i.t as usize]));
                }
            } else if i.op == Op::Wsr && i.imm == 228 {
                writes.push((228, ar[i.t as usize]));
            }
        }
        assert_eq!(m.run(&Until { tick: m.now() + 1, marks: &PcSet::default(), stops: &[],
            stops_after: 0, on_drive: false }), Ran::Tick, "quiesce executes: {:?}", m.status());
    }
    assert_eq!(writes, expected, "modem reset, all four unroutes, then INTENABLE");
    assert_eq!(m.cpu().intenable, keep, "only enabled dispatcher and yield lines survive");
}

#[test]
fn fault_record_reports_observed_island_guard() {
    use fab_elec_mcu_emit32::xtensa::{assemble, reg::*, Asm};
    use fab_elec_mcu_emit_s3::{build_image, chip, Board, BoardTime, BootTask, Radio, SlotOptions, STACK_TOP, VOICEBOX};
    use fab_elec_mcu_rt::{module, watch};

    let island = island_fixture::small_island();
    let damaged = GUARD ^ 0xff;
    let gate = island.gate_names.iter().position(|name| name == "free").unwrap() as u32;
    let data = STACK_TOP - 0x4000;
    let entry = s3_fixture::IRAM + (data - s3_fixture::RAM);
    let mut fault_pc = 0;
    let task = assemble(&mut Asm::new(0, A8), |a| {
        a.entry(A1, 64);
        a.s32i(A3, A2, 0);
        a.l32i(A8, A4, 0);
        for reg in A10..=A15 { a.movi(reg, i32::from(reg - A10)); }
        a.callx8(A8); // free(NULL) records all six argument registers.
        fault_pc = entry + a.base.offset(); a.op24(0);
    });
    let p = s3_fixture::profile(61);
    let radio = Radio { start: None, restart: fab_elec_mcu_emit_s3::image::RestartPolicy { hold_after_fault: true, ..fab_elec_mcu_emit_s3::image::RestartPolicy::PROVISIONAL },
        island: island.binding(), dispatcher_priority: 1, irq_stack_bytes: 2048,
        task_capacity: 3, isr_capacity: PACK_ISR_LINES, heap_bytes: 4096, timer_reserve: 8,
        trace_entries: 2, burst_insns: 0, boot_tasks: vec![BootTask { entry, abi: TaskAbi::Windowed,
            priority: 2, stack_bytes: 2048, args: [island.guard, damaged, island.header.gate_table + 4 * gate, 0, 0, 0] }] };
    let board = Board { radio: Some(radio), layout: VOICEBOX, panel: None, i2c: None, motion: None, touch: None,
        time: BoardTime { rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: p.time.wdt_delay_sel } };
    let app = fab_elec_mcu::compile("let tick = every(ms(2), fn() {})", &chip::profile(), &[], &[]).unwrap();
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let image = build_image(&module, &board, &SlotOptions::default()).unwrap();
    let l = &image.slot.layout;
    assert!(l.regions[0].used <= u64::from(data) && data + task.len() as u32 <= STACK_TOP - l.stack_bytes);
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &p);
    assert!(m.seed_data(data, &task));
    let mut marks = PcSet::new(fault_pc, 1); marks.insert(fault_pc);
    let deadline = m.tick_hz();
    assert_eq!(m.run(&Until { tick: deadline, marks: &marks, stops: &[fault_pc], stops_after: 0, on_drive: false }), Ran::Pc);
    let cpu = m.cpu();
    let physical = cpu.physical_ar();
    let ars: [u32; 64] = std::array::from_fn(|i| physical[(cpu.window_base as usize * 4 + i) % 64]);
    let sp = cpu.ar[1];
    check_quiesce_writes(&mut m, &image.slot, RADIO_DEAD, 0);
    let symbols = image.slot.symbols.radio.as_ref().unwrap();
    let mut marks = PcSet::new(symbols.idle, 1); marks.insert(symbols.idle);
    let mut reader = watch::Reader::new();
    let mut collector = RecordCollector::new();
    let record = 'collect: loop {
        assert_eq!(m.run(&Until { tick: deadline, marks: &marks, stops: &[symbols.idle],
            stops_after: m.now(), on_drive: false }), Ran::Pc, "dispatcher drains the complete record");
        let mut out = Vec::new(); m.take_out(&mut out); m.mark();
        for byte in out {
            assert_eq!(byte.link, 0);
            if let Some(frame) = reader.push(byte.byte) {
                let event = frame.expect("valid USB watch frame").event;
                if let Event::RadioRecord { index: 41, words } = event {
                    assert_eq!(words, [damaged, ISLAND_READY, 0, 0], "the final USB frame carries the observed guard, status and zero padding");
                }
                if let Some(record) = collector.push(event).unwrap() { break 'collect record; }
            }
        }
    };
    assert_eq!(record.len(), 166);
    let fault = RadioFault::decode(&record).unwrap();
    assert_eq!(fault.island_status, ISLAND_READY);
    assert_eq!((fault.island_guard, fault.pc, fault.ars), (damaged, fault_pc, ars));
    assert_eq!(fault.backtrace[0], (fault_pc, sp));
    assert_eq!(fault.backtrace.last().unwrap().0, symbols.task_exit);
    assert_eq!(fault.trace.len(), 1);
    assert_eq!((fault.trace[0].gate, fault.trace[0].args), (gate, [0, 1, 2, 3, 4, 5]));
    assert!(fault.to_string().contains(&format!("island_guard={damaged:#010x} damaged")));
}

#[test]
fn dispatcher_stop_quiesces_radio_and_returns_masked() {
    exercise_stop(StopCase::DispatcherCall);
}

#[test]
fn island_task_stop_quiesces_without_a_fault_record() {
    exercise_stop(StopCase::IslandTask);
}

#[test]
fn island_stop_while_dead_goes_to_fatal() {
    exercise_stop(StopCase::AlreadyDead);
}

#[test]
fn island_stop_in_dispatcher_task_context_goes_to_fatal() {
    exercise_stop(StopCase::DispatcherTrap);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StopCase { DispatcherCall, IslandTask, AlreadyDead, DispatcherTrap }

fn exercise_stop(case: StopCase) {
    use fab_elec_mcu_emit32::xtensa::{assemble, enc, reg::*, Asm};
    use fab_elec_mcu_emit_s3::{build_image, chip, Board, BoardTime, BootTask, Radio, SlotOptions, STACK_TOP, VOICEBOX};
    use kernel::{INTR_MATRIX, MODEM_RESET_FIELD, SYSCON_WIFI_RST_EN, WIFI_SOURCES};

    let island = island_fixture::small_island();
    let data = STACK_TOP - 0x4000;
    let entry = s3_fixture::IRAM + (data - s3_fixture::RAM);
    // Island return value, saved dispatcher PC, live route, then post-stop reset and route readbacks.
    let log = data - (4 + WIFI_SOURCES) * 4;
    // The policy never restarts on 108, so the stopped state stays observable.
    let cause = 108;
    let old_cause = if case == StopCase::AlreadyDead { 106 } else { 0 };
    let reset = 1 << 30;
    let p = s3_fixture::profile(63);
    static IMAGE: std::sync::OnceLock<fab_elec_mcu_emit_s3::Image> = std::sync::OnceLock::new();
    let image = IMAGE.get_or_init(|| {
        let task = |entry, priority| BootTask { entry, priority, abi: TaskAbi::Windowed, stack_bytes: 2048, args: [0; 6] };
        let radio = Radio { start: None, restart: fab_elec_mcu_emit_s3::image::RestartPolicy::PROVISIONAL,
            island: island.binding(), dispatcher_priority: 2, irq_stack_bytes: 2048,
            task_capacity: 4, isr_capacity: PACK_ISR_LINES, heap_bytes: 4096, timer_reserve: 8,
            trace_entries: 0, burst_insns: 0, boot_tasks: vec![task(entry, 3), task(island.entries[0], 1)] };
        let board = Board { radio: Some(radio), layout: VOICEBOX, panel: None, i2c: None, motion: None, touch: None,
            time: BoardTime { rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: p.time.wdt_delay_sel } };
        let app = fab_elec_mcu::compile("let tick = every(ms(2), fn() {})", &chip::profile(), &[], &[]).unwrap();
        let module = fab_elec_mcu_rt::module::load(&app.module, &chip::profile()).unwrap();
        build_image(&module, &board, &SlotOptions::default()).unwrap()
    });
    let slot = &image.slot;
    let r = slot.layout.radio.as_ref().unwrap();
    let s = slot.symbols.radio.as_ref().unwrap();
    let stub = s.kernel_symbols.iter().find(|(name, _, _)| name == "kernel_fault").unwrap().1;
    let in_worker = matches!(case, StopCase::IslandTask | StopCase::AlreadyDead);
    let trap = |a: &mut Asm| {
        a.mov_reg_i32(A2, cause);
        a.mov_reg_i32(A4, r.stop_pending); a.s32i(A2, A4, 0); a.memw();
        a.mov_n(A3, A10);
        a.mov_reg_i32(A4, stub); a.jx(A4);
    };
    let mut worker_end = 0;
    let mut returned = 0;
    let caller = assemble(&mut Asm::new(1, A8), |a| {
        a.literal_table(); a.entry(A1, 64);
        a.mov_reg_i32(A8, island.entries[1]); a.callx8(A8);
        a.mov_reg_i32(A3, log); a.s32i(A10, A3, 0);
        for (name, args) in [("osi_set_intr", [0, 0, 3, 1]), ("osi_set_isr", [3, island.entries[0], 0, 0]),
            ("osi_ints_on", [1 << 3, 0, 0, 0])] {
            for (i, value) in args.into_iter().enumerate() { a.mov_reg_i32(A10 + i as u8, value); }
            a.mov_reg_i32(A8, s.thunks.iter().find(|(n, _)| n == name).unwrap().1); a.callx8(A8);
        }
        a.op24(enc::rrr(0, 0, 0, 6, 15, A2)); // RSIL a2,15
        a.mov_reg_i32(A3, INTR_MATRIX); a.l32i(A4, A3, 0);
        a.mov_reg_i32(A5, log); a.s32i(A4, A5, 8);
        a.mov_reg_i32(A3, SYSCON_WIFI_RST_EN); a.mov_reg_i32(A4, reset); a.s32i(A4, A3, 0);
        if case == StopCase::AlreadyDead {
            a.mov_reg_i32(A3, r.radio_state); a.movi(A4, RADIO_DEAD as i32); a.s32i(A4, A3, 0);
            a.mov_reg_i32(A3, r.stop_cause); a.mov_reg_i32(A4, old_cause); a.s32i(A4, A3, 0);
        }
        if in_worker {
            trap(a);
            worker_end = entry + a.base.offset();
            return;
        }

        // Interpose the test caller on the dispatcher's next real restore. Block
        // this boot task so the dispatcher runs while a lower-priority peer is READY.
        a.mov_reg_i32(A3, r.dispatcher); a.l32i(A3, A3, TCB_FRAME);
        a.l32i(A4, A3, CONTEXT_PC); a.s32i(A4, A5, 4);
        a.mov_reg_i32(A4, entry + a.base.label_offset(0).unwrap_or(0)); a.s32i(A4, A3, CONTEXT_PC);
        a.mov_reg_i32(A3, r.boot_tasks[0]); a.movi(A4, BLOCKED as i32); a.s32i(A4, A3, TCB_STATE);
        a.movi(A4, 1 << YIELD_LINE); a.wsr(A4, 226); a.rsync();
        a.wsr(A2, 230); a.rsync(); a.j(-4);

        a.base.label_assign(0); worker_end = entry + a.base.offset();
        a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        if case == StopCase::DispatcherTrap { trap(a); return; }
        a.mov_reg_i32(A2, cause); a.mov_reg_i32(A3, s.radio_stop); a.callx0(A3);
        returned = entry + a.base.offset();
        a.mov_reg_i32(A5, log);
        a.mov_reg_i32(A3, SYSCON_WIFI_RST_EN); a.l32i(A4, A3, 0); a.s32i(A4, A5, 12);
        a.mov_reg_i32(A3, INTR_MATRIX);
        for i in 0..WIFI_SOURCES { a.l32i(A4, A3, 4 * i); a.s32i(A4, A5, 16 + 4 * i); }
        // Restore the original PC and registers and let the app run its timer.
        a.mov_reg_i32(A2, r.dispatcher); a.l32i(A3, A2, TCB_FRAME);
        a.l32i(A4, A5, 4); a.s32i(A4, A3, CONTEXT_PC);
        a.mov_reg_i32(A3, s.restore); a.jx(A3);
    });
    assert!(slot.layout.regions[0].used <= u64::from(log) && data + caller.len() as u32 <= STACK_TOP - slot.layout.stack_bytes);
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &p);
    assert!(m.seed_data(data, &caller));
    let deadline = m.tick_hz();
    let start = if case == StopCase::DispatcherCall { s.radio_stop } else { stub };
    let mut marks = PcSet::new(start, 1); marks.insert(start);
    assert_eq!(m.run(&Until { tick: deadline, marks: &marks, stops: &[start], stops_after: 0, on_drive: false }),
        Ran::Pc, "stop entry: {:?}", m.status());
    let ps = m.cpu().ps;
    let begin = m.cpu().instret;
    assert_eq!((ps & 15, m.cpu().ar[2]), (15, cause));
    if case == StopCase::DispatcherCall { assert_eq!(m.cpu().ar[0], returned); }
    assert_eq!((word(&m, r.island_status), word(&m, log)), (ISLAND_READY, 1));
    assert_eq!(word(&m, r.current), if in_worker { r.boot_tasks[0] } else { r.dispatcher });
    assert_eq!((word(&m, r.isr_count), word(&m, r.isrs + 4), word(&m, log + 8)), (1, island.entries[0], 3));
    assert_ne!(m.cpu().intenable & (1 << 3), 0, "registered island line enabled");
    assert_eq!([0, 1].map(|i| word(&m, r.boot_tasks[i] + TCB_STATE)), [if in_worker { READY } else { BLOCKED }, READY]);
    let initial_state = if case == StopCase::AlreadyDead { RADIO_DEAD } else { RADIO_RUNNING };
    let pending = if case == StopCase::DispatcherCall { 0 } else { cause };
    let stop_words = |m: &S3Machine| (word(m, r.radio_state), word(m, r.stop_cause), word(m, r.stop_pending));
    assert_eq!(stop_words(&m), (initial_state, old_cause, pending));
    if matches!(case, StopCase::AlreadyDead | StopCase::DispatcherTrap) {
        let mut marks = PcSet::new(s.fatal, 1); marks.insert(s.fatal);
        assert_eq!(m.run(&Until { tick: deadline, marks: &marks, stops: &[s.fatal], stops_after: m.now(), on_drive: false }),
            Ran::Pc, "rejected stop reaches fatal");
        assert_eq!(stop_words(&m), (initial_state, old_cause, cause), "fatal leaves the request untouched");
        assert!((0..FAULT_FRAMES * 4).all(|i| word(&m, r.fault + 4 * i) == 0));
        return;
    }
    let n = word(&m, r.task_count);
    check_quiesce_writes(&mut m, slot, RADIO_STOPPED, reset);
    assert_eq!(word(&m, r.stop_cause), cause, "cause is visible at STOPPED publication");
    assert_eq!(word(&m, r.stop_pending), 0, "no pending request at STOPPED publication");
    assert!((0..FAULT_FRAMES * 4).all(|i| word(&m, r.fault + 4 * i) == 0), "stop leaves the whole fault record empty");
    assert_eq!(word(&m, r.isr_count), 0);
    for i in 0..n {
        let tcb = word(&m, r.tasks + 4 * i);
        assert_eq!(word(&m, tcb + TCB_STATE), if [r.dispatcher, r.idle].contains(&tcb) { READY } else { DEAD });
    }
    if case == StopCase::DispatcherCall {
        let mut stops = [returned, s.pick]; stops.sort_unstable();
        let mut marks = PcSet::new(stops[0], stops[1] - stops[0] + 1);
        for pc in stops { marks.insert(pc); }
        assert_eq!(m.run(&Until { tick: deadline, marks: &marks, stops: &stops, stops_after: m.now(), on_drive: false }), Ran::Pc);
        assert_eq!((m.pc(), m.cpu().ps), (returned, ps), "dispatcher returns directly with PS preserved");
        assert!(m.cpu().instret - begin <= 40 + 8 * u64::from(n), "masked dispatcher stop bound");
    } else {
        let mut marks = PcSet::new(s.restore, 1); marks.insert(s.restore);
        assert_eq!(m.run(&Until { tick: deadline, marks: &marks, stops: &[s.restore], stops_after: m.now(), on_drive: false }), Ran::Pc);
        assert_eq!(m.cpu().ar[2], r.dispatcher, "dispatcher is the first task restored after the island stop");
        assert!(m.cpu().instret - begin <= 151 + 28 * u64::from(n), "island stop bound from ILL to first restore");
    }

    // Catch execution anywhere in the island image, its SRAM alias, or the boot
    // worker. Observe every selected TCB at restore, including the fast idle wake.
    let iram = s3_fixture::IRAM;
    let flash = s3_fixture::FLASH + island.header.mmu_page * 65536;
    let handler = slot.handlers[0].address;
    let mut stops = vec![s.restore, handler];
    stops.extend(iram..iram + (island.window_end - s3_fixture::RAM));
    stops.extend(flash..flash + island.header.image_size);
    stops.extend(entry..worker_end);
    stops.sort_unstable();
    let mut marks = PcSet::new(stops[0], stops.last().unwrap() - stops[0] + 1);
    for &pc in &stops { marks.insert(pc); }
    let until = m.now() + m.tick_hz() / 100;
    let mut saw_idle = false;
    let mut woke_dispatcher = false;
    let mut firings = 0;
    loop {
        match m.run(&Until { tick: until, marks: &marks, stops: &stops, stops_after: m.now(), on_drive: false }) {
            Ran::Tick => break,
            Ran::Pc if m.pc() == s.restore => {
                let chosen = m.cpu().ar[2];
                assert!([r.dispatcher, r.idle].contains(&chosen), "stopped radio selected {chosen:#010x}");
                if chosen == r.idle { saw_idle = true; } else if saw_idle { woke_dispatcher = true; }
            }
            Ran::Pc if m.pc() == handler => { firings += 1; }
            ran => panic!("after stop: {ran:?} at {:#010x}, {:?}", m.pc(), m.status()),
        }
    }
    assert!(saw_idle && woke_dispatcher && firings >= 2, "idle wakes the dispatcher and app timers keep firing");
    if case == StopCase::DispatcherCall {
        assert_eq!(word(&m, log + 12), reset | MODEM_RESET_FIELD, "modem reset remains asserted");
        for i in 0..WIFI_SOURCES { assert_eq!(word(&m, log + 16 + 4 * i), 16, "Wi-Fi source {i} remains unrouted"); }
    }
}

#[test]
fn symbolization_prefers_functions_and_respects_range_boundaries() {
    let map = "0x40001004 4 TEXT .text.fixture fixture\n0x40001000 32 TEXT/FUNC fixture_call fixture\n";
    assert_eq!(symbolize(map, 0x4000_1004), Some(("fixture_call", 4)), "a function wins even over a smaller overlapping section row");
    let reversed = map.lines().rev().collect::<Vec<_>>().join("\n");
    assert_eq!(symbolize(&reversed, 0x4000_1004), Some(("fixture_call", 4)), "function preference does not depend on map order");
    assert_eq!(symbolize(map, 0x4000_1000), Some(("fixture_call", 0)), "the first function byte is included");
    assert_eq!(symbolize(map, 0x4000_101f), Some(("fixture_call", 31)), "the last function byte is included");
    assert_eq!(symbolize(map, 0x4000_1020), None, "the exclusive end address is outside");
    assert_eq!(symbolize(map, 0x4000_0fff), None, "addresses below the function are outside");
    assert_eq!(symbolize("bad row\n0xffffffff 1 TEXT/FUNC last fixture\n", u32::MAX), Some(("last", 0)), "a range ending at 2^32 works and malformed rows are ignored");
    assert_eq!(symbolize("0x40001000 0 TEXT/FUNC empty fixture\n", 0x4000_1000), None, "zero-sized rows contain no PC");
}

use std::sync::Arc;

use fab_elec_mcu_emit32::xtensa::{ccz, enc};
use fab_elec_mcu_emu::bus::{Kind, Width};
use fab_elec_mcu_cosim::{LogicMachine, LogicPlan};
use fab_elec_mcu_emu::{time, ChipState, Drive, Lockstep, Machine, PcSet, Pull, Ran, Status, Until};
use fab_elec_mcu_s3::{CpuClock, S3Machine, StopDetail};

mod support;
use support::*;

#[test]
fn handoff_junk_is_seeded_and_missing_magic_executes_nothing() {
    let mut p = Program::new();
    p.halt();
    let image = p.finish();
    let first = S3Machine::new(image.clone(), &profile(1));
    let same = S3Machine::new(image.clone(), &profile(1));
    let other = S3Machine::new(image.clone(), &profile(2));
    assert_eq!(first.pc(), ENTRY, "handoff PC is the profile's byte address");
    assert_eq!(first.cpu().ar, same.cpu().ar, "same seed reproduces all argument registers");
    assert_ne!(first.cpu().ar, other.cpu().ar, "different seeds change argument registers");
    let mut bytes = [[0; 64]; 3];
    for (machine, ram) in [&first, &same, &other].into_iter().zip(bytes.iter_mut()) {
        assert!(machine.read_data(RAM, ram), "handoff SRAM is inspectable");
    }
    assert_eq!(bytes[0], bytes[1], "same seed reproduces SRAM junk");
    assert_ne!(bytes[0], bytes[2], "different seeds change SRAM junk");
    assert_eq!((first.cpu().intenable, first.cpu().interrupt), (0, 0), "handoff cannot interrupt before firmware starts");
    assert_eq!(first.pins().len(), 45, "physical pin set has 45 pins");
    assert_eq!((first.pins()[21], first.pins()[22], first.pins()[44]), ("GPIO21", "GPIO26", "GPIO48"), "absent GPIO22–25 are omitted");
    for pin in 0..first.pins().len() {
        let gpio = if pin < 22 { pin } else { pin + 4 };
        // M: pins_fixture adopts Table 6.12-1's pulls at ROM handoff;
        // its handoff_enable disables every output, including GPIO43.
        let pull = match gpio {
            // facts-machine.md §6 / TRM p.493 Table 6.12-1: RST 3, WPU.
            0 | 26..=32 => Pull::Up,
            // Same table p.493: MTCK RST 1*, fixture EFUSE_DIS_PAD_JTAG=0.
            39 => Pull::Up,
            // Same table p.494: GPIO43 RST 4 and GPIO44 RST 3 both have WPU.
            43 | 44 => Pull::Up,
            // Same table p.494: GPIO45/46 RST 2, WPD.
            45 | 46 => Pull::Down,
            // Same table pp.493–494: other RST 0/1 entries list no pulls;
            // pins_fixture explicitly chooses omitted pull flags as zero.
            _ => Pull::None,
        };
        assert_eq!(first.drive(pin), Drive::Input(pull), "GPIO{gpio} handoff uses the fixture's cited pull");
    }
    let mut copy = first.clone();
    copy.set_input(0, true);
    assert_eq!(copy.drive_gen(), 0, "sampled inputs cannot change drive yet");
    let mut out = Vec::new();
    copy.take_out(&mut out);
    assert!(out.is_empty(), "no links produce output yet");
    let mut invalid_read = [0x55; 2];
    for addr in [RAM - 1, RAM + 416 * 1024 - 1, IRAM, u32::MAX] {
        assert!(!copy.read_data(addr, &mut invalid_read), "inspection rejects non-SRAM1 data spans");
        assert_eq!(invalid_read, [0x55; 2], "rejected inspection leaves destination unchanged");
    }
    for n in [0x400, 0x404] {
        let mut bad = (*image).clone();
        bad[n] ^= 1;
        let mut machine = S3Machine::new(Arc::new(bad), &profile(1));
        assert_stop(&mut machine, ENTRY,
            "no Direct Boot magic at flash 0x400: the ROM would run the second-stage loader, which is never simulated", None);
        assert_eq!((machine.now(), machine.cpu().instret), (0, 0), "bad magic executes no instruction");
    }
    assert_eq!(Arc::strong_count(&image), 5, "whole-machine clones share the supplied flash allocation");
}

#[test]
fn clock_write_changes_the_next_instruction_and_time_conversion() {
    let mut p = Program::new();
    p.disable_watchdogs(); // MWDT0's running APB divider cannot switch clocks.
    let slow_start = access_tick(p.pc());
    p.counted_loop(7);
    let slow_end = p.pc();
    // facts.md §3 Table 7.2-2: set PLL_FREQ_SEL=1, CPUPERIOD_SEL=1 (160 MHz)
    // while still on XTAL, then SOC_CLK_SEL=1. Do not depend on junk fields.
    p.store(CPU_PER, 5);
    p.store(SYSCLK, 1 << 10);
    let fast_start = p.pc();
    p.counted_loop(7);
    let fast_end = p.halt();
    let mut machine = S3Machine::new(p.finish(), &profile(3));
    at_pc(&mut machine, slow_end);
    assert_eq!(machine.now() - slow_start, 15 * 24, "XTAL / 2 charges 24 ticks for each retired instruction");
    let slow_retired = machine.cpu().instret;
    let slow_tick = machine.now();
    at_pc(&mut machine, fast_start);
    assert_eq!(machine.now() - slow_tick, (machine.cpu().instret - slow_retired) * 24,
        "the source-selection store itself still uses the old clock");
    let fast_tick = machine.now();
    let fast_retired = machine.cpu().instret;
    at_pc(&mut machine, fast_end);
    assert_eq!(machine.now() - fast_tick, (machine.cpu().instret - fast_retired) * 3,
        "PLL480 period 1 charges three ticks per instruction");
    assert_eq!(time(0.0, machine.now(), machine.tick_hz()), machine.now() as f64 / 480_000_000.0,
        "co-simulation time uses the 480 MHz grid");

    for (clock, ticks) in [(CpuClock::Pll480 { period: 0 }, 6), (CpuClock::Pll480 { period: 1 }, 3),
        (CpuClock::Pll320 { period: 0 }, 6), (CpuClock::Pll320 { period: 1 }, 3)]
    {
        let mut p = Program::new();
        p.emit(enc::movi(3, 1));
        p.halt();
        let mut board = profile(4);
        board.handoff_cpu = clock;
        let mut m = S3Machine::new(p.finish(), &board);
        assert_eq!(run_to(&mut m, 1, &PcSet::default()), Ran::Tick, "each documented PLL selection executes");
        assert_eq!(m.now(), ticks, "handoff clock encodes the documented PLL selection");
    }
}

#[test]
fn mmu_remap_is_shared_by_fetch_and_data_load_and_rewinds_known_bits() {
    let mut p = Program::new();
    p.load(1, MMU64);
    p.emit(enc::l32i(7, 1, 0)); // Unknown table words themselves are readable junk.
    let before_map = p.pc();
    p.store(MMU64, 2); // facts-machine.md §3: physical page 2, valid flash.
    p.load(1, DROM);
    p.emit(enc::l32i(8, 1, 0));
    p.load(1, XIP);
    p.emit(enc::jx(1));
    p.image.resize(0x20000, 0xff);
    let routine = enc::movi(9, 77);
    p.emit(routine);
    p.halt();
    let mut machine = S3Machine::new(p.finish(), &profile(5));
    at_pc(&mut machine, before_map);
    machine.mark();
    let finish = |m: &mut S3Machine| {
        assert_eq!(run_to(m, 10_000, &PcSet::default()), Ran::Halted, "remapped routine reaches BREAK");
        assert_eq!(m.cpu().ar[8], routine, "data alias reads the same routine bytes (BREAK starts with zero)");
        assert_eq!(m.cpu().ar[9], 77, "instruction alias executes the remapped physical page");
        assert_eq!(m.pc(), XIP + 3, "BREAK occurs in the remapped virtual routine");
        assert!(m.stop_detail().is_none(), "successful MMU translations latch no access stop");
        (m.cpu().ar, m.now())
    };
    let first = finish(&mut machine);
    machine.rewind();
    assert_eq!(machine.status(), Status::Running, "rewind clears the routine's halt");
    assert_eq!(finish(&mut machine), first, "mapping and CPU replay agree after rewind");

    // Unknown beats random invalid/target bits, and invalid beats target.
    for (entry, what) in [(None, "MMU entry not written since reset"),
        (Some(1 << 14), "access through an invalid MMU entry"),
        (Some(1 << 15), "PSRAM is not modelled"),
        (Some(256), "flash address beyond the board's flash")]
    {
        let mut p = Program::new();
        if let Some(entry) = entry { p.store(MMU64, entry); }
        p.load(1, XIP);
        p.emit(enc::jx(1));
        let mut m = S3Machine::new(p.finish(), &profile(6));
        m.mark();
        let detail = Some(StopDetail { addr: XIP, width: Width::B, kind: Kind::Fetch, mmu_entry: Some(64) });
        assert_stop(&mut m, XIP, what, detail);
        m.rewind();
        assert!(m.stop_detail().is_none(), "rewind clears the latched bus detail");
        assert_stop(&mut m, XIP, what, detail);
    }
}

#[test]
fn register_reads_erased_flash_and_page_crossing_use_the_documented_map() {
    let mut p = Program::new();
    // facts-machine.md §3; extmem_reg.h:1366–1380 gives idle 0x1001.
    p.load(1, 0x600c_4130);
    p.emit(enc::l32i(6, 1, 0));
    p.load(1, SYSCLK);
    p.emit(enc::l32i(7, 1, 0));
    p.store(MMU64, 1);
    p.load(1, MMU64);
    p.emit(enc::l32i(8, 1, 0));
    p.load(1, DROM);
    p.emit(enc::l32i(9, 1, 0));
    // facts-machine.md §3: fixed 64 KiB pages; the next entry is unknown.
    p.load(1, DROM + 0xffff);
    let fault = p.emit(enc::l32i(10, 1, 0));
    let mut m = S3Machine::new(p.finish(), &profile(15));
    let old_destination = m.cpu().ar[10];
    assert_stop(&mut m, fault, "MMU entry not written since reset",
        Some(StopDetail { addr: DROM + 0xffff, width: Width::W, kind: Kind::Load, mmu_entry: Some(65) }));
    assert_eq!(m.cpu().ar[6], 0x1001, "cache state reports both caches idle");
    assert_eq!(m.cpu().ar[7] & 0xfff, 1, "handoff SYSCLK fields encode XTAL / 2");
    assert_eq!(m.cpu().ar[8], 1, "MMU word read returns the written mapping");
    assert_eq!(m.cpu().ar[9], u32::MAX, "flash beyond the image reads erased bytes within board size");
    assert_eq!(m.cpu().ar[10], old_destination, "cross-page load does not commit a partial value");
}

#[test]
fn unsupported_memory_and_peripherals_retain_the_faulting_pc() {
    // facts.md §2 ROM/SRAM2/windows; §4a GPIO base. Width cases enforce
    // §2 instruction-bus alignment and facts-machine.md §3 MMU word access.
    for (addr, kind, width, what) in [
        (0x3ff0_0400, Kind::Load, Width::W, "ROM access"),
        (0x3fcf_0000, Kind::Load, Width::W, "SRAM0/2 can be cache memory; the cache configuration is not modelled"),
        (DROM, Kind::Store, Width::W, "store to a flash window"),
        (0x6000_4000, Kind::Load, Width::W, "unmodelled peripheral or address"),
        (IRAM, Kind::Load, Width::B, "instruction-bus data access is not an aligned word"),
        (IRAM + 1, Kind::Store, Width::W, "instruction-bus data access is not an aligned word"),
        (FLASH, Kind::Load, Width::B, "instruction-bus data access is not an aligned word"),
        (MMU64, Kind::Load, Width::H, "register access is not an aligned word load or store"),
        (0x600c_4130, Kind::Store, Width::W, "write to read-only cache state"),
    ] {
        let mut p = Program::new();
        p.load(1, addr);
        let pc = match (kind, width) {
            (Kind::Store, _) => p.emit(enc::s32i(2, 1, 0)),
            (_, Width::B) => p.emit(enc::l8ui(2, 1, 0)),
            (_, Width::H) => p.emit(enc::l16ui(2, 1, 0)),
            _ => p.emit(enc::l32i(2, 1, 0)),
        };
        let mut m = S3Machine::new(p.finish(), &profile(7));
        assert_stop(&mut m, pc, what, Some(StopDetail { addr, width, kind, mmu_entry: None }));
    }
    // The ROM's code is foreign (rom::table, on every machine): an entry with no stand-in stops where it starts.
    let mut p = Program::new();
    p.load(1, 0x4000_0400);
    p.emit(enc::callx0(1));
    let flash = p.finish();
    let mut m = S3Machine::new(flash.clone(), &profile(7));
    assert_stop(&mut m, 0x4000_0400, "foreign code with no stand-in", None);
    assert_eq!(m.explain_foreign().as_deref(), Some("foreign code with no stand-in at 0x40000400"));
    // The Firmware pane reads that reason through `Machine::halt_detail`, also when a wrapper holds
    // the chip: a binding's logic side (`LogicMachine`) and the lockstep pair forward it.
    assert_eq!(m.halt_detail(), m.explain_foreign(), "the chip gives its stand-in's reason");
    fn halted_detail<M: Machine>(mut machine: M) -> Option<String> {
        assert_eq!(machine.run(&Until { tick: 100_000, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false }), Ran::Halted);
        machine.halt_detail()
    }
    let logic = LogicMachine::new(S3Machine::new(flash.clone(), &profile(7)), LogicPlan::default()).expect("an empty logic side");
    assert_eq!(halted_detail(logic), m.explain_foreign(), "the logic side's machine forwards the reason");
    assert_eq!(halted_detail(Lockstep::new(S3Machine::new(flash, &profile(7)))), m.explain_foreign(), "the lockstep pair forwards the reason");
    // ESP-IDF v6.1.1 (Apache-2.0), system_reg.h:15–41; facts.md:159.
    // Enabling the clock alone releases CPU1. Reset or stall keeps it parked.
    for (value, what) in [(2, "CPU1 is not modelled"), (8, "reserved CPU1 control fields are undocumented")] {
        let mut p = Program::new();
        for parked in [0, 4, 6, 1, 3, 5, 7] { p.store(0x600c_0000, parked); }
        p.load(1, 0x600c_0000);
        p.emit(enc::l32i(6, 1, 0));
        let pc = p.store(0x600c_0000, value);
        let mut m = S3Machine::new(p.finish(), &profile(7));
        assert_stop(&mut m, pc, what,
            Some(StopDetail { addr: 0x600c_0000, width: Width::W, kind: Kind::Store, mmu_entry: None }));
        assert_eq!(m.cpu().ar[6], 7, "CPU1 reset/clock/stall fields store every parked combination");
    }
}

#[test]
fn waiti_keeps_partial_cycles_and_enters_at_the_compare_tick() {
    let mut p = Program::new();
    // core-isa.h:575 kernel offset, :468 timer0 line. SRAM aliases: facts.md §2.
    let handler = IRAM + 0x300;
    p.store(RAM + 0x300, enc::movi(9, 55));
    p.store(RAM + 0x304, 0x40); // BREAK's remaining bytes after the first word.
    p.sr(231, IRAM); // VECBASE
    p.sr(230, 0); // PS: clear EXCM and choose kernel vector
    let count_write = p.sr(234, 0); // retirement advances the replacement count to 1
    p.sr(240, 40);
    p.sr(228, 1 << 6);
    p.emit(enc::rrr(0, 0, 0, 7, 0, 0)); // WAITI 0, ISA p.644 / emit32 RRR
    let return_pc = p.halt();
    let match_tick = u64::from((count_write - ENTRY) / 3) * 24 + 40 * 24;
    let mut m = S3Machine::new(p.finish(), &profile(8));
    let mut marks = PcSet::new(return_pc, 1);
    marks.insert(return_pc);
    let mut handler_marks = PcSet::new(handler, 1);
    handler_marks.insert(handler);
    let mut until = Until { tick: match_tick - 5, marks: &marks, stops: &[return_pc], stops_after: 0, on_drive: true };
    assert_eq!(m.run(&until), Ran::Tick, "WAITI clips to a deadline within a CPU cycle");
    assert_eq!(m.now(), match_tick - 5, "sleep reaches the requested tick exactly");
    assert_eq!(m.cpu().ccount, 39, "partial CPU cycle has not advanced CCOUNT");
    assert_eq!(m.status(), Status::Sleeping { wake: Some(match_tick) }, "sleep projects the enabled compare match");
    let mut chip = ChipState::default();
    m.chip(&mut chip);
    assert_eq!(chip.wake.map(|w| (w.tick, w.source, w.vector)), Some((match_tick, "CCOMPARE0", 6)),
        "chip inspection reports comparator and CPU line");
    assert!(chip.timers.is_empty() && chip.pending.is_empty(), "unimplemented peripheral lists stay empty");
    let mut trace = Vec::new();
    m.take_trace(&mut trace);
    assert!(trace.is_empty(), "sleeping return PC is neither traced nor stopped at");
    m.mark();
    until.tick = match_tick;
    assert_eq!(m.run(&until), Ran::Tick, "remaining partial cycle reaches the match exactly");
    assert_eq!((m.now(), m.cpu().ccount), (match_tick, 40), "one complete cycle elapses across split runs");
    assert_eq!(m.status(), Status::Sleeping { wake: Some(match_tick) }, "pending match remains visible before entry");
    until.tick = match_tick + 24;
    until.marks = &handler_marks;
    until.stops = &[];
    assert_eq!(m.run(&until), Ran::Tick, "resumption enters and executes the first handler instruction");
    m.take_trace(&mut trace);
    assert_eq!(trace, [(match_tick, handler)], "handler execution starts at match tick with no entry charge");
    assert_eq!(m.cpu().ar[9], 55, "handler was fetched through SRAM1's instruction alias");
    m.rewind();
    assert_eq!(m.run(&until), Ran::Tick, "rewound partial sleep resumes");
    let mut again = Vec::new();
    m.take_trace(&mut again);
    assert_eq!(again, trace, "rewind restores partial-cycle phase and pending interrupt state");

    let mut p = Program::new();
    p.store(RAM + 0x300, enc::movi(9, 66));
    p.sr(231, IRAM);
    p.sr(230, 0);
    p.sr(240, 2);
    p.sr(228, 1 << 6);
    p.sr(234, 0);
    let wait_pc = p.emit(enc::rrr(0, 0, 0, 7, 0, 0));
    p.halt();
    let match_tick = u64::from((wait_pc + 3 - ENTRY) / 3) * 24;
    let mut m = S3Machine::new(p.finish(), &profile(16));
    let mut marks = PcSet::new(handler, 1);
    marks.insert(handler);
    assert_eq!(run_to(&mut m, match_tick + 24, &marks), Ran::Tick,
        "a match during WAITI retirement enters without a time jump");
    let mut trace = Vec::new();
    m.take_trace(&mut trace);
    assert_eq!(trace, [(match_tick, handler)], "WAITI retirement match is not postponed by a counter wrap");
    assert_eq!(m.cpu().ar[9], 66, "immediate wake executes the SRAM handler");
}

#[test]
fn unwakeable_sleep_advances_full_wraps_without_instruction_execution() {
    let mut p = Program::new();
    p.disable_watchdogs(); // TRM p.677 boot watchdogs otherwise reset this sleep.
    p.sr(230, 0);
    p.sr(234, 0);
    // Timer0's line 6 is enabled at level 1 (core-isa.h:427,468), but WAITI 1
    // raises PS.INTLEVEL to 1, so its matches latch without ending the sleep.
    p.sr(228, 1 << 6);
    p.emit(enc::rrr(0, 0, 0, 7, 1, 0));
    let sleeping = access_tick(p.pc());
    p.halt();
    let mut m = S3Machine::new(p.finish(), &profile(9));
    m.set_usb_host_listening(false); // This fixture needs no periodic host polls.
    let marks = PcSet::default();
    assert_eq!(run_to(&mut m, sleeping, &marks), Ran::Tick, "firmware reaches WAITI");
    let before = (m.cpu().ccount, m.cpu().instret);
    let deadline = m.now() + (1u64 << 32) * 24 + 7;
    assert_eq!(run_to(&mut m, deadline, &marks), Ran::Tick, "sleep without takeable timers crosses a full counter wrap");
    assert_eq!((m.now(), m.cpu().ccount, m.cpu().instret), (deadline, before.0, before.1),
        "full wrap preserves count modulo 2^32 and retires nothing");
    assert_eq!(m.status(), Status::Sleeping { wake: None }, "disabled and masked timers provide no wake");
    assert_eq!(m.cpu().interrupt & ((1 << 6) | (1 << 15) | (1 << 16)), (1 << 6) | (1 << 15) | (1 << 16),
        "disabled comparator requests still latch during a full wrap");
}

#[test]
fn mark_rewind_replays_stores_clock_and_trace_and_barrier_resumes() {
    let mut p = Program::new();
    p.disable_watchdogs(); // The clock-switch part needs MWDT0 stopped first.
    p.load(1, RAM + 1); // hardware unaligned data words, unlike the instruction bus
    p.emit(enc::movi(2, 0));
    p.emit(enc::movi(4, 4));
    let start = p.emit(enc::s32i(2, 1, 0));
    p.emit(enc::addi(2, 2, 1));
    p.emit(enc::addi(4, 4, -1));
    p.emit(enc::bccz(ccz::NE, 4, (i64::from(start) - i64::from(p.pc()) - 4) as i32));
    p.store(CPU_PER, 5);
    p.store(SYSCLK, 1 << 10);
    p.store(IRAM + 4, 0x1234_5678); // overlapping undo across the two aliases
    let end = p.halt();
    let mut marks = PcSet::new(ENTRY, end - ENTRY + 1);
    for pc in (ENTRY..=end).step_by(3) { marks.insert(pc); }
    let mut m = S3Machine::new(p.finish(), &profile(10));
    let mut initial = [0; 16];
    assert!(m.read_data(RAM, &mut initial), "pre-mark SRAM is readable");
    m.mark();
    let finish = |m: &mut S3Machine| {
        assert_eq!(run_to(m, 10_000, &marks), Ran::Halted, "store/clock fixture reaches BREAK");
        let mut regs = vec![("stale", 0)];
        m.regs(&mut regs);
        assert_eq!(regs.len(), 36, "register inspection replaces stale entries with every named register");
        let mut ram = [0; 16];
        assert!(m.read_data(RAM, &mut ram), "written SRAM is inspectable");
        let mut trace = Vec::new();
        m.take_trace(&mut trace);
        (regs, ram, m.now(), trace)
    };
    let first = finish(&mut m);
    assert_eq!(&first.1[4..8], &0x1234_5678u32.to_le_bytes(), "instruction alias writes the data store");
    m.rewind();
    let mut restored = [0; 16];
    assert!(m.read_data(RAM, &mut restored), "rewound SRAM is readable");
    assert_eq!(restored, initial, "reverse journal restores overlapping writes through both aliases");
    assert_eq!(finish(&mut m), first, "replay preserves registers, SRAM, time and trace");

    // Stores never end a run: the undo saves each page a run writes, whatever the count of
    // stores, and an unaligned word straddling two pages saves both.
    let mut p = Program::new();
    p.load(1, RAM + 1);
    p.emit(enc::movi(2, 0));
    let store_pc = p.emit(enc::s32i(2, 1, 0));
    p.emit(enc::addi(2, 2, 1));
    p.emit(enc::addi(1, 1, 4));
    p.emit(enc::j((i64::from(store_pc) - i64::from(p.pc()) - 4) as i32));
    let mut m = S3Machine::new(p.finish(), &profile(11));
    let sram = |m: &S3Machine| {
        let mut bytes = vec![0; 3 * 4096];
        assert!(m.read_data(RAM, &mut bytes), "the three written pages are readable");
        bytes
    };
    let initial = sram(&m);
    m.mark();
    let start = m.now();
    let marks = PcSet::default();
    // 2500 rounds of four instructions write 10,000 bytes over three pages; the store at
    // RAM + 4093 straddles the first two.
    let deadline = PROLOGUE_TICKS + 2500 * 4 * 24;
    assert_eq!(run_to(&mut m, deadline, &marks), Ran::Tick, "2500 stores run to the deadline in one run");
    assert_eq!((m.cpu().ar[2], m.cpu().instret), (2500, 3 + 2500 * 4), "every store retires, none repeated or skipped");
    let written = sram(&m);
    let mut clone = m.clone();
    m.rewind();
    assert_eq!((m.now(), sram(&m)), (start, initial.clone()), "rewind restores time and every page the run wrote");
    assert_eq!(run_to(&mut m, deadline, &marks), Ran::Tick, "the replay reaches the same deadline");
    assert_eq!(sram(&m), written, "the replay writes the same memory");
    clone.rewind();
    assert_eq!((clone.now(), sram(&clone)), (start, initial), "a whole-machine clone carries the saved pages");
    m.mark();
    m.rewind();
    assert_eq!((m.now(), sram(&m)), (deadline, written), "a mark is the new rewind point");
}

#[test]
fn pc_stops_use_byte_addresses_and_trace_only_executed_marks() {
    let mut p = Program::new();
    p.emit(enc::movi(3, 3));
    let stop = p.emit(enc::addi(3, 3, 1));
    let trace_pc = p.emit(enc::movi(4, 1));
    p.halt();
    let mut m = S3Machine::new(p.finish(), &profile(12));
    let mut marks = PcSet::new(BODY, 12);
    marks.insert(stop);
    marks.insert(trace_pc);
    let mut until = Until { tick: PROLOGUE_TICKS + 24, marks: &marks, stops: &[stop], stops_after: 0, on_drive: false };
    assert_eq!(m.run(&until), Ran::Pc, "PC stop precedes simultaneous tick deadline");
    assert_eq!((m.pc(), m.cpu().ar[3]), (BODY + 3, 3), "byte-address stop precedes the marked instruction");
    let mut trace = Vec::new();
    m.take_trace(&mut trace);
    assert!(trace.is_empty(), "stopping itself adds no trace");
    until.stops_after = m.now();
    until.tick = PROLOGUE_TICKS + 72;
    assert_eq!(m.run(&until), Ran::Tick, "stops_after permits one execution at the stopped PC");
    m.take_trace(&mut trace);
    assert_eq!(trace, [(PROLOGUE_TICKS + 24, stop), (PROLOGUE_TICKS + 48, trace_pc)], "executed marks appear once with machine ticks");
    assert_eq!((m.cpu().ar[3], m.cpu().ar[4]), (4, 1), "both resumed instructions execute");
}

#[test]
fn unknown_prid_and_unsupported_clock_sources_stop_atomically() {
    // Raw: the stop must be atomic at the first instruction, which uses a3 only.
    let mut p = Program::raw();
    p.emit(enc::rsr(0, 3, 0, 235, 3)); // PRID, ISA pp.310–311
    let mut m = S3Machine::new(p.finish(), &profile(13));
    let mut before = Vec::new();
    m.regs(&mut before);
    assert_stop(&mut m, ENTRY, "PRID value not documented for this core", None);
    let mut after = Vec::new();
    m.regs(&mut after);
    assert_eq!(after, before, "unknown PRID stops before architectural writes");
    assert_eq!((m.now(), m.cpu().instret), (0, 0), "unknown PRID retires nothing");

    // facts.md §3: source 2 RC_FAST, source 3 reserved; CPUPERIOD_SEL 3
    // reserved, and PLL320 period 2 has no documented frequency.
    for (sysclk, cpu_per) in [(2 << 10, 0), (3 << 10, 0), (1 << 10, 3), (1 << 10, 2)] {
        let mut p = Program::new();
        p.store(CPU_PER, cpu_per);
        let fault = p.store(SYSCLK, sysclk);
        let mut m = S3Machine::new(p.finish(), &profile(14));
        assert_stop(&mut m, fault, "CPU clock source not modelled",
            Some(StopDetail { addr: SYSCLK, width: Width::W, kind: Kind::Store, mmu_entry: None }));
    }
    // IDF rtc_clk.c:233-240: 240 MHz follows the core regulators' writes; the ROM's setting is not modelled there.
    let mut p = Program::new();
    p.store(CPU_PER, 6);
    let fault = p.store(SYSCLK, 1 << 10);
    let mut m = S3Machine::new(p.finish(), &profile(14));
    assert_stop(&mut m, fault, "CPU at 240 MHz before both core regulators were written is not modelled",
        Some(StopDetail { addr: SYSCLK, width: Width::W, kind: Kind::Store, mmu_entry: None }));
}

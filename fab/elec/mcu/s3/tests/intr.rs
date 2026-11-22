//! CORE0 register and firmware interrupt properties. No wall-clock waits.

use fab_elec_mcu_emit32::xtensa::enc;
use fab_elec_mcu_emu::bus::{Kind, Width};
use fab_elec_mcu_emu::{ChipState, HaltKind, Machine, PcSet, Ran, Status, Until};
use fab_elec_mcu_s3::{config::s3_config, ResetScope, S3Machine, StopDetail};
use fab_elec_mcu_xtensa::sim::cause;

mod support;
use support::*;

// Exercise refused writes against the actual bank without adding a public
// machine mutation/inspection API merely for tests. The bus-level stop and
// all interrupt delivery properties below use the complete S3 machine.
type Access<T> = Option<Result<T, &'static str>>;
#[path = "../src/periph/intr.rs"]
mod intr;

// facts-machine.md:104,108,171,210,220–225 (§2).
const MATRIX: u32 = 0x600c_2000;
const TARGET0_MAP: u32 = MATRIX + 57 * 4;
const USB_MAP: u32 = MATRIX + 96 * 4;
const GPIO_MAP: u32 = MATRIX + 16 * 4;
const STATUS0: u32 = MATRIX + 0x18c;
const STATUS1: u32 = MATRIX + 0x190;
const STATUS3: u32 = MATRIX + 0x198;
const HANDLER: u32 = IRAM + 0x300; // config.rs kernel level-1 vector

fn bank_words(bank: &intr::Intr) -> Vec<Access<u32>> {
    (0..0x800).step_by(4).map(|off| bank.read(MATRIX + off, 0)).collect()
}

#[test]
fn register_fields_status_routing_and_reset_scopes() {
    let mut bank = intr::Intr::new();
    let config = s3_config();
    for source in 0..99 {
        let addr = MATRIX + source * 4;
        assert_eq!(bank.read(addr, 0), Some(Ok(16)), "source {source} map resets disconnected");
        let line = if source % 32 == 14 { 31 } else { source % 32 };
        assert_eq!(bank.write(addr, line), Some(Ok(())), "source {source} accepts a documented five-bit destination");
        assert_eq!(bank.read(addr, 0), Some(Ok(line)), "source {source} map reads back independently");
    }
    assert_eq!(bank.read(MATRIX + 0x19c, 0), Some(Ok(1)), "matrix clock gate resets enabled");
    assert_eq!(bank.write(MATRIX + 0x19c, 1), Some(Ok(())), "keeping the clock enabled is supported");
    assert_eq!(bank.read(MATRIX + 0x7fc, 0), Some(Ok(0x0201_2300)), "DATE has its documented reset");
    assert_eq!(bank.write(MATRIX + 0x7fc, 0x0abc_def0), Some(Ok(())), "DATE accepts all documented field bits");
    assert_eq!(bank.read(MATRIX + 0x7fc, 0), Some(Ok(0x0abc_def0)), "DATE is readable and writable");
    let before = bank_words(&bank);
    for (off, value, why) in [
        (57 * 4, 32, "reserved interrupt-matrix field is undocumented"),
        (57 * 4, 14, "routing a source to the NMI interrupt is not modelled"),
        (0x18c, 0, "write to read-only interrupt-matrix register"),
        (0x190, 1, "write to read-only interrupt-matrix register"),
        (0x194, u32::MAX, "write to read-only interrupt-matrix register"),
        (0x198, 1, "write to read-only interrupt-matrix register"),
        (0x19c, 0, "interrupt-matrix clock gating is undocumented"),
        (0x19c, 3, "reserved interrupt-matrix field is undocumented"),
        (0x7fc, 1 << 28, "reserved interrupt-matrix field is undocumented"),
        (0x1a0, 0, "undecoded interrupt-matrix register"),
    ] {
        assert_eq!(bank.write(MATRIX + off, value), Some(Err(why)), "unsupported register write stops: {why}");
        assert_eq!(bank_words(&bank), before, "rejected write preserves every matrix register: {why}");
    }
    let sources = (1u128 << 57) | (1u128 << 96);
    for (word, want) in [(0, 0), (1, 1 << 25), (2, 0), (3, 1)] {
        assert_eq!(bank.read(STATUS0 + word * 4, sources), Some(Ok(want)), "status word {word} reports source numbering independent of routing");
    }
    assert_eq!(bank.read(STATUS1, 0), Some(Ok(0)), "status is live rather than a matrix latch");
    assert_eq!(bank.read(MATRIX + 0x1a0, 0), Some(Err("undecoded interrupt-matrix register")), "unknown CORE0 offsets refuse reads");
    assert_eq!(bank.read(MATRIX - 4, 0), None, "matrix does not claim another block's address");
    for line in 0..32 {
        if line == 14 { continue; }
        assert_eq!(bank.write(TARGET0_MAP, line), Some(Ok(())), "all non-NMI destinations store");
        let want = if matches!(line, 6 | 7 | 11 | 15 | 16 | 29) { 0 } else { 1 << line };
        assert_eq!(bank.lines(1 << 57, &config), want, "destination {line} routes only external Level/Edge inputs");
    }
    assert_eq!(bank.write(TARGET0_MAP, 10), Some(Ok(())), "target routes to an edge line");
    assert_eq!(bank.write(USB_MAP, 10), Some(Ok(())), "USB shares the same edge line");
    assert_eq!(bank.lines(sources, &config), 1 << 10, "multiple sources OR into one CPU input");
    assert_eq!(bank.lines(1 << 96, &config), 1 << 10, "dropping one contributor leaves the shared input high");
    assert_eq!(bank.source_name(10, 1 << 96), "USB_DEVICE_INT", "wake names a currently high contributor");
    let retained = bank_words(&bank);
    bank.reset(ResetScope::Cpu);
    assert_eq!(bank_words(&bank), retained, "CPU-only reset retains the complete matrix bank");
    for scope in [ResetScope::Core, ResetScope::System] {
        assert_eq!(bank.write(TARGET0_MAP, 10), Some(Ok(())), "reset fixture starts with a non-default route");
        assert_eq!(bank.write(MATRIX + 0x7fc, 7), Some(Ok(())), "reset fixture starts with non-default DATE");
        bank.reset(scope);
        assert_eq!(bank_words(&bank), bank_words(&intr::Intr::new()), "{scope:?} reset restores every matrix register");
        assert_eq!(bank.lines(sources, &config), 0, "{scope:?} reset disconnects even already-high sources");
    }
}

#[test]
fn syscon_wifi_words_follow_digital_reset_scope() {
    const CLK: u32 = 0x6002_6014;
    const RST: u32 = 0x6002_6018;
    let defaults = [0xfffc_e030, 0];
    let written = [0x1357_9bdf, 0xeca8_6420];
    for (scope, request) in [(ResetScope::Cpu, 1 << 5), (ResetScope::Core, 1 << 31)] {
        let mut p = Program::new();
        p.read_time(CLK, 8); p.read_time(RST, 9);
        let sampled = p.pc();
        p.store(CLK, written[0]); p.store(RST, written[1]);
        p.read_time(CLK, 8); p.read_time(RST, 9);
        let updated = p.pc();
        let reset = p.store(RTC, request);
        p.halt();
        let mut m = S3Machine::new(p.finish(), &profile(80));
        at_pc(&mut m, sampled);
        assert_eq!(m.cpu().ar[8..10], defaults);
        at_pc(&mut m, updated);
        assert_eq!(m.cpu().ar[8..10], written);
        assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Reset { at: access_tick(reset) });
        assert_eq!(m.last_reset().unwrap().scope, scope);
        at_pc(&mut m, sampled);
        let expected = if scope == ResetScope::Cpu { written } else { defaults };
        assert_eq!(m.cpu().ar[8..10], expected, "SYSCON words at {scope:?} handoff");
    }
}

#[test]
fn a_refused_map_write_reaches_the_bus_stop() {
    let mut p = Program::new();
    p.read_time(TARGET0_MAP, 8);
    p.store(TARGET0_MAP, 31);
    p.read_time(TARGET0_MAP, 9);
    let pc = p.store(TARGET0_MAP, 32);
    let mut m = S3Machine::new(p.finish(), &profile(80));
    assert_stop(&mut m, pc, "reserved interrupt-matrix field is undocumented",
        Some(StopDetail { addr: TARGET0_MAP, width: Width::W, kind: Kind::Store, mmu_entry: None }));
    assert_eq!((m.cpu().ar[8], m.cpu().ar[9]), (16, 31), "bus dispatch reaches reset and writable matrix fields");
}

fn handler(p: &mut Program, clear: u32, value: u32, edge_mask: u32) {
    // marker; inspect source status and map; clear peripheral; INTCLEAR;
    // BREAK. Entry itself has no retirement/tick cost in the CPU contract.
    let code = [
        enc::movi(9, 55), enc::l32i(8, 6, 0), enc::l32i(10, 7, 0),
        enc::s32i(5, 4, 0), enc::rsr(0, 3, 1, 227, 11), enc::rrr(0, 0, 0, 4, 0, 0),
    ];
    let mut bytes = Vec::new();
    for insn in code { bytes.extend_from_slice(&insn.to_le_bytes()[..3]); }
    for (n, chunk) in bytes.chunks(4).enumerate() {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        p.store(RAM + 0x300 + n as u32 * 4, u32::from_le_bytes(word));
    }
    p.load(4, clear);
    p.load(5, value);
    p.load(6, STATUS1);
    p.load(7, TARGET0_MAP);
    p.load(11, edge_mask);
    p.sr(231, IRAM);
    p.sr(230, 0);
}

fn alarm(p: &mut Program) -> u64 {
    // facts-machine.md:11,19,21,25,40,46,52–57,75,78,80,86:
    // stop/load UNIT0; load target; enable RAW delivery and comparison.
    p.store(SYSTIMER, 0);
    p.store(SYSTIMER + 0x0c, 0);
    p.store(SYSTIMER + 0x10, 0);
    p.store(SYSTIMER + 0x5c, 1);
    p.store(SYSTIMER + 0x1c, 0);
    p.store(SYSTIMER + 0x20, 200);
    p.store(SYSTIMER + 0x50, 1);
    p.store(SYSTIMER + 0x64, 1);
    let enable = p.store(SYSTIMER, (1 << 30) | (1 << 24));
    let edge = systimer_edges(access_tick(enable), true) + 200;
    edge / 2 * 60 + if edge % 2 == 0 { 0 } else { 36 }
}

fn waiti(p: &mut Program) -> u64 {
    p.emit(enc::rrr(0, 0, 0, 7, 0, 0));
    let sleeping = access_tick(p.pc());
    p.halt();
    sleeping
}

fn stop_at(m: &mut S3Machine, pc: u32, deadline: u64) {
    let mut marks = PcSet::new(pc, 1);
    marks.insert(pc);
    assert_eq!(m.run(&Until {
        tick: deadline, marks: &marks, stops: &[pc], stops_after: 0, on_drive: false,
    }), Ran::Pc, "interrupt firmware reaches the requested boundary before its deadline");
}

fn wake(m: &S3Machine) -> Option<(u64, &'static str, u32)> {
    let mut chip = ChipState::default();
    m.chip(&mut chip);
    chip.wake.map(|w| (w.tick, w.source, w.vector))
}

#[test]
fn level_alarm_wakes_waiti_at_its_tick_and_clear_drops_the_line() {
    let mut p = Program::new();
    p.disable_watchdogs();
    handler(&mut p, SYSTIMER + 0x6c, 1, 0);
    p.store(TARGET0_MAP, 0); // facts-machine.md:239: line 0, level 1
    p.sr(228, 1);
    let due = alarm(&mut p);
    let sleeping = waiti(&mut p);
    let mut m = S3Machine::new(p.finish(), &profile(81));
    assert_eq!(run_to(&mut m, sleeping, &PcSet::default()), Ran::Tick, "WAITI retires before the target alarm");
    assert_eq!(m.status(), Status::Sleeping { wake: Some(due) }, "sleep projects the exact SYSTIMER alarm tick");
    assert_eq!(wake(&m), Some((due, "SYSTIMER_TARGET0", 0)), "snapshot names the future routed target and CPU line");
    assert_eq!(run_to(&mut m, due - 1, &PcSet::default()), Ran::Tick, "alarm has not woken the CPU a tick early");
    assert_eq!(m.cpu().interrupt & 1, 0, "CPU level input stays low before the alarm");
    assert_eq!(run_to(&mut m, due, &PcSet::default()), Ran::Tick, "event is processed at its exact tick");
    assert_eq!(m.cpu().interrupt & 1, 1, "alarm drives the routed CPU level input");
    assert_eq!(wake(&m), Some((due, "SYSTIMER_TARGET0", 0)), "an already-high enabled peripheral wakes now");
    stop_at(&mut m, HANDLER, due + 24);
    assert_eq!((m.now(), m.cpu().exccause), (due, cause::LEVEL1_INTERRUPT), "level-1 entry occurs at the alarm tick without retirement cost");
    stop_at(&mut m, HANDLER + 12, due + 5 * 24);
    assert_eq!(m.cpu().ar[8], 1 << 25, "live matrix status reports source 57 in word 1 bit 25");
    assert_eq!(m.cpu().interrupt & 1, 0, "clearing SYSTIMER immediately drops the CPU level INTERRUPT bit");
}

#[test]
fn disabled_alarm_stays_asleep_and_gpio_host_input_can_wake_it() {
    let mut p = Program::new();
    p.disable_watchdogs();
    handler(&mut p, SYSTIMER + 0x6c, 1, 0);
    p.gpio_setup(4, MUX_IE);
    // facts-machine.md:514,521,524,619: clear stale status, select a
    // rising edge, then enable its normal (not NMI) GPIO contribution.
    p.store(GPIO_STATUS + 8, 1 << 4);
    p.store(pin_reg(4), (1 << 13) | (1 << 7)); // normal enable, rising edge
    p.store(GPIO_MAP, 1);
    p.store(TARGET0_MAP, 0);
    p.sr(228, 1 << 1); // target's CPU line 0 remains disabled
    let due = alarm(&mut p);
    let sleeping = waiti(&mut p);
    let mut m = S3Machine::new(p.finish(), &profile(82));
    m.set_input(4, false); // external input is otherwise seeded handoff junk
    assert_eq!(run_to(&mut m, sleeping, &PcSet::default()), Ran::Tick, "masked-target fixture reaches WAITI");
    let retired = m.cpu().instret;
    let until = due + 501;
    assert_eq!(run_to(&mut m, until, &PcSet::default()), Ran::Tick, "disabled target cannot end the run before its until tick");
    assert_eq!((m.now(), m.cpu().instret), (until, retired), "disabled target enters no handler and retires no instruction");
    assert!(matches!(m.status(), Status::Sleeping { .. }), "CPU remains sleeping after the disabled alarm");
    assert_eq!(m.cpu().interrupt & 1, 1, "INTENABLE masks delivery without discarding the asserted level");
    m.set_input(4, true);
    assert_eq!(wake(&m), Some((until, "GPIO_INTERRUPT_CPU", 1)), "host GPIO input updates the routed line at the input tick");
    stop_at(&mut m, HANDLER, until + 24);
    assert_eq!(m.now(), until, "GPIO takes level-1 entry at the delivery tick");
    stop_at(&mut m, HANDLER + 9, until + 4 * 24);
    assert_eq!(m.cpu().ar[8], 1 << 25, "disabled alarm remains visible in matrix source status");
}

#[test]
fn edge_alarm_latches_until_intclear_after_the_source_drops() {
    let mut p = Program::new();
    p.disable_watchdogs();
    handler(&mut p, SYSTIMER + 0x6c, 1, 1 << 10);
    p.store(TARGET0_MAP, 10); // facts-machine.md:249: peripheral edge, level 1
    p.sr(228, 1 << 10);
    let due = alarm(&mut p);
    waiti(&mut p);
    let mut m = S3Machine::new(p.finish(), &profile(83));
    stop_at(&mut m, HANDLER + 9, due + 4 * 24);
    assert_eq!(m.cpu().interrupt & (1 << 10), 1 << 10, "rising source latches the CPU edge bit");
    stop_at(&mut m, HANDLER + 12, due + 5 * 24);
    assert_eq!(m.cpu().interrupt & (1 << 10), 1 << 10, "dropping the peripheral source preserves the CPU edge latch");
    stop_at(&mut m, HANDLER + 15, due + 6 * 24);
    assert_eq!(m.cpu().interrupt & (1 << 10), 0, "INTCLEAR clears the latched edge after the input falls");
}

#[test]
fn usb_packet_delivery_wakes_the_sleeping_cpu() {
    let mut p = Program::new();
    p.disable_watchdogs();
    handler(&mut p, USB_CLR, 4, 0);
    p.load(6, STATUS3);
    p.store(USB_MAP, 0);
    p.store(USB_ENA, 4); // facts-machine.md:658,666,677: RECV_PKT -> source 96
    p.sr(228, 1);
    let sleeping = waiti(&mut p);
    let mut m = S3Machine::new(p.finish(), &profile(84));
    assert_eq!(run_to(&mut m, sleeping, &PcSet::default()), Ran::Tick, "USB fixture sleeps before host input");
    let retired = m.cpu().instret;
    m.link_in(0, 0x51);
    assert_eq!(wake(&m), Some((USB_FRAME, "USB_DEVICE_INT", 0)), "queued host input projects the existing USB delivery grid");
    assert_eq!(run_to(&mut m, USB_FRAME - 1, &PcSet::default()), Ran::Tick, "USB delivery has not happened early");
    assert_eq!(m.cpu().instret, retired, "queued bytes alone do not retire sleeping instructions");
    stop_at(&mut m, HANDLER, USB_FRAME + 24);
    assert_eq!((m.now(), m.cpu().exccause), (USB_FRAME, cause::LEVEL1_INTERRUPT), "USB packet delivery wakes and enters at its delivery tick");
    stop_at(&mut m, HANDLER + 12, USB_FRAME + 5 * 24);
    assert_eq!(m.cpu().ar[8], 1, "USB source 96 appears at status word 3 bit 0");
    assert_eq!(m.cpu().interrupt & 1, 0, "clearing RECV_PKT drops the routed USB level");
}

#[test]
fn rewind_restores_map_source_and_cpu_edge_input_history() {
    let mut p = Program::new();
    p.disable_watchdogs();
    p.sr(230, 0);
    p.sr(228, 0);
    let due = alarm(&mut p);
    let mark_pc = p.pc();
    p.read_time(TARGET0_MAP, 8);
    let mapped = p.store(TARGET0_MAP, 10) + 3;
    p.read_time(TARGET0_MAP, 9);
    waiti(&mut p);
    let mut m = S3Machine::new(p.finish(), &profile(85));
    at_pc(&mut m, mark_pc);
    m.mark();
    for pass in 0..2 {
        stop_at(&mut m, mapped, due);
        assert_eq!(m.cpu().ar[8], 16, "pass {pass}: map at the mark was restored before firmware changes it");
        assert_eq!(run_to(&mut m, due, &PcSet::default()), Ran::Tick, "pass {pass}: replay reaches the alarm");
        assert_eq!(m.cpu().ar[9], 10, "pass {pass}: firmware changed the map after the mark");
        assert_eq!(m.cpu().interrupt & (1 << 10), 1 << 10, "pass {pass}: restored CPU input history admits the same rising edge");
        m.rewind();
        assert_eq!((m.pc(), m.cpu().interrupt & (1 << 10)), (mark_pc, 0), "pass {pass}: rewind restores PC and pending CPU lines");
    }
    assert!(!matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Unmodeled), "rewind leaves a runnable pre-alarm machine");
}

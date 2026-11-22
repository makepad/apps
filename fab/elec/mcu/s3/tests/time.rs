//! Firmware-level time properties. Register facts: facts-machine.md §§1,4,5;
//! timing conventions and M values: periph/time.rs and support/time_fixture.rs.

use fab_elec_mcu_emit32::xtensa::enc;
use fab_elec_mcu_emu::bus::{Kind, Width};
use fab_elec_mcu_emu::{Clock, HaltKind, Machine, PcSet, Ran, Status};
use fab_elec_mcu_s3::{ResetScope, S3Machine, StopDetail};

mod support;
use support::*;

fn finish(m: &mut S3Machine) {
    assert_eq!(run_to(m, 100_000, &PcSet::default()), Ran::Halted, "time fixture reaches BREAK");
    let Status::Halted(h) = m.status() else { panic!("BREAK stays inspectable"); };
    assert_eq!(h.kind, HaltKind::Break, "time fixture must not halt Unmodeled");
}

fn pad_to(p: &mut Program, tick: u64) {
    assert!(access_tick(p.pc()) <= tick && tick % 24 == 0, "fixture target is a future CPU boundary");
    while access_tick(p.pc()) < tick { p.emit(enc::movi(0, 0)); }
}

/// VALID follows UPDATE by the board's latency (periph/time.rs, sessions 20-21: 2-3 polls in
/// a 0.43 us read): clear at the first access after UPDATE and set 192 ticks (0.4 us) after it.
/// A VALUE read before VALID stops, so fixtures read captured words from here on.
const VALID_TICKS: u64 = 192;

fn pad_to_valid(p: &mut Program, update: u32) {
    pad_to(p, access_tick(update) + VALID_TICKS - 24);
}

fn reset_at(m: &mut S3Machine, tick: u64, cause: u8, scope: ResetScope) {
    assert_eq!(run_to(m, tick - 1, &PcSet::default()), Ran::Tick, "watchdog has not expired a tick early");
    assert_eq!(m.now(), tick - 1, "WAITI reaches the pre-expiry tick exactly");
    assert_eq!(run_to(m, tick, &PcSet::default()), Ran::Reset { at: tick }, "expiry interrupts WAITI at its exact tick");
    assert_eq!(m.now(), 0, "reset starts a new machine epoch");
    let reset = m.last_reset().expect("reset request remains inspectable");
    assert_eq!((reset.tick, reset.cause, reset.scope), (tick, cause, scope), "reset records its source and domain");
}

// ESP-IDF v6.1.1 (Apache-2.0), rtc_cntl_reg.h:1091–1121,2189–2219;
// system_reg.h:15–41; bases: facts.md:108,111.
const STORES: [u32; 8] = [0x50, 0x54, 0x58, 0x5c, 0xc0, 0xc4, 0xc8, 0xcc];
const CORE1: u32 = 0x600c_0000;

fn read_boot_storage(p: &mut Program) {
    for (n, off) in STORES.into_iter().enumerate() { p.read_time(RTC + off, 8 + n as u8); }
    p.read_time(CORE1, 4);
    p.read_time(CORE1 + 4, 5);
}

fn write_boot_storage(p: &mut Program) {
    for (n, off) in STORES.into_iter().enumerate() { p.store(RTC + off, 0xa500_0000 + n as u32); }
    p.store(CORE1, 5); // RESETING | RUNSTALL; CLKGATE_EN clear.
    p.store(CORE1 + 4, 0x1234_5678);
}

fn assert_boot_storage(m: &S3Machine, rtc_retained: bool, system_retained: bool) {
    for n in 0..8 {
        assert_eq!(m.cpu().ar[8 + n], if rtc_retained { 0xa500_0000 + n as u32 } else { 0 },
            "STORE{n} has its documented reset value or retains its independent word by reset scope");
    }
    assert_eq!((m.cpu().ar[4], m.cpu().ar[5]), if system_retained { (5, 0x1234_5678) } else { (4, 0) },
        "CPU1 control words reset with SYSTEM and survive a CPU0-only reset");
}

#[test]
fn systimer_load_capture_and_alternating_clock() {
    // TRM pp.634–636,642–645: 52-bit load, UPDATE, VALID and captured words.
    for first_long in [true, false] {
        let mut p = Program::new();
        p.disable_watchdogs();
        p.store(SYSTIMER, 0); // stop both units before loading
        p.store(SYSTIMER + 0x0c, 0x12345);
        p.store(SYSTIMER + 0x10, 0xffff_fffe);
        p.store(SYSTIMER + 0x5c, 1);
        p.read_time(SYSTIMER + 4, 7);
        let start = p.store(SYSTIMER, 1 << 30);
        let mut captures = Vec::new();
        for n in 0..6 {
            let update = p.store(SYSTIMER + 4, 1 << 30);
            p.read_time(SYSTIMER + 4, 8);
            pad_to_valid(&mut p, update);
            p.read_time(SYSTIMER + 4, 13);
            p.read_time(SYSTIMER + 0x44, 9);
            p.read_time(SYSTIMER + 0x40, 10);
            p.load(1, RAM + n * 8);
            p.emit(enc::s32i(9, 1, 0));
            p.emit(enc::s32i(10, 1, 4));
            captures.push(access_tick(update));
        }
        p.read_time(SYSTIMER + 0x44, 11);
        p.read_time(SYSTIMER + 0x40, 12);
        p.halt();
        let mut board = profile(21);
        board.time.systimer_first_long = first_long;
        let mut m = S3Machine::new(p.finish(), &board);
        finish(&mut m);
        assert_eq!(m.cpu().ar[7], 0, "VALID starts clear before the first UPDATE");
        assert_eq!(m.cpu().ar[8], 0, "UPDATE clears VALID until its capture completes");
        assert_eq!(m.cpu().ar[13], 1 << 29, "VALID is set 0.4 us after UPDATE, with the pulse clear");
        let mut bytes = [0; 48];
        assert!(m.read_data(RAM, &mut bytes), "captured time samples are in SRAM");
        let base = 0x12345_ffff_fffeu64;
        for (n, tick) in captures.into_iter().enumerate() {
            let got = u64::from_le_bytes(bytes[n * 8..n * 8 + 8].try_into().unwrap());
            let want = base + systimer_edges(tick, first_long) - systimer_edges(access_tick(start), first_long);
            assert_eq!(got, want, "capture {n} follows 24/36 tick edges and carries into the high word");
        }
        assert_eq!((m.cpu().ar[11], m.cpu().ar[12]), (m.cpu().ar[9], m.cpu().ar[10]),
            "reads leave the last captured value intact while the counter runs");
    }
    // Firmware polls VALID before it reads the capture; a read that does not stops.
    let mut p = Program::new();
    p.disable_watchdogs();
    p.store(SYSTIMER + 4, 1 << 30);
    let pc = p.read_time(SYSTIMER + 0x44, 9);
    let mut m = S3Machine::new(p.finish(), &profile(21));
    assert_stop(&mut m, pc, "SYSTIMER VALUE read before VALUE_VALID is undocumented",
        Some(StopDetail { addr: SYSTIMER + 0x44, width: Width::W, kind: Kind::Load, mmu_entry: None }));
}

#[test]
fn target_raw_changes_at_the_alarm_tick_and_status_masks_it() {
    // facts-machine.md §1; TRM pp.636–638: immediate load, target alarm,
    // held RAW, ENA and ST. Cover all three comparator register banks.
    for n in 0..3 {
        let mut p = Program::new();
        p.disable_watchdogs();
        p.store(SYSTIMER, 0);
        p.store(SYSTIMER + 0x0c, 0);
        p.store(SYSTIMER + 0x10, 0);
        p.store(SYSTIMER + 0x5c, 1);
        let enable_tick = access_tick(p.pc()) + 11 * 24;
        let edge = systimer_edges(enable_tick, true);
        let target = 100 + ((4 - edge % 4) % 4); // alarm on a CPU boundary
        p.store(SYSTIMER + 0x1c + n * 8, 0);
        p.store(SYSTIMER + 0x20 + n * 8, target as u32);
        p.store(SYSTIMER + 0x50 + n * 4, 1);
        let enable = p.store(SYSTIMER, (1 << 30) | (1 << (24 - n)));
        assert_eq!(access_tick(enable), enable_tick, "target oracle uses the enabling store's start");
        let due = (edge + target) / 2 * 60;
        p.load(1, SYSTIMER + 0x68);
        pad_to(&mut p, due - 24);
        p.emit(enc::l32i(8, 1, 0));
        p.emit(enc::l32i(9, 1, 0));
        p.read_time(SYSTIMER + 0x70, 10);
        p.store(SYSTIMER + 0x64, 1 << n);
        p.read_time(SYSTIMER + 0x70, 11);
        p.store(SYSTIMER + 0x6c, 1 << n);
        p.read_time(SYSTIMER + 0x68, 12);
        p.halt();
        let mut m = S3Machine::new(p.finish(), &profile(22));
        finish(&mut m);
        assert_eq!(m.cpu().ar[8], 0, "target {n} RAW is clear before the exact alarm tick");
        assert_eq!(m.cpu().ar[9], 1 << n, "target {n} RAW is set before the access at the alarm tick");
        assert_eq!((m.cpu().ar[10], m.cpu().ar[11]), (0, 1 << n), "ST equals RAW & ENA for target {n}");
        assert_eq!(m.cpu().ar[12], 0, "CLR clears the held target {n} alarm");
    }
}

#[test]
fn period_on_unit1_fires_twice_without_reloading_the_unit() {
    // TRM pp.637–639: load period, clear/set PERIOD_MODE, enable; alarms at
    // start+n*period. UNIT_SEL=1 and UNIT1_WORK_EN select the second unit.
    let mut p = Program::new();
    p.disable_watchdogs();
    p.store(SYSTIMER, 0);
    p.store(SYSTIMER + 0x14, 0);
    p.store(SYSTIMER + 0x18, 42);
    p.store(SYSTIMER + 0x60, 1);
    p.store(SYSTIMER + 0x34, (1 << 31) | 32);
    p.store(SYSTIMER + 0x50, 1);
    p.store(SYSTIMER + 0x34, (1 << 31) | (1 << 30) | 32);
    // Align enable's oscillator index to a multiple of four.
    while systimer_edges(access_tick(p.pc()) + 48, true) % 4 != 0 { p.emit(enc::movi(0, 0)); }
    let enable = p.store(SYSTIMER, (1 << 29) | (1 << 24));
    let first = (systimer_edges(access_tick(enable), true) + 32) / 2 * 60;
    p.load(1, SYSTIMER + 0x68);
    pad_to(&mut p, first - 24);
    p.emit(enc::l32i(7, 1, 0));
    p.emit(enc::l32i(8, 1, 0));
    p.store(SYSTIMER + 0x6c, 1);
    p.read_time(SYSTIMER + 0x74, 9);
    p.load(1, SYSTIMER + 0x68);
    pad_to(&mut p, first + 960 - 24);
    p.emit(enc::l32i(10, 1, 0));
    p.emit(enc::l32i(11, 1, 0));
    p.read_time(SYSTIMER + 0x74, 12);
    let update = p.store(SYSTIMER + 8, 1 << 30);
    pad_to_valid(&mut p, update);
    p.read_time(SYSTIMER + 0x4c, 13);
    p.halt();
    let mut m = S3Machine::new(p.finish(), &profile(23));
    finish(&mut m);
    assert_eq!((m.cpu().ar[7], m.cpu().ar[8], m.cpu().ar[10], m.cpu().ar[11]), (0, 1, 0, 1),
        "both period alarms occur at their exact ticks, even after clearing the first");
    assert_eq!((m.cpu().ar[9], m.cpu().ar[12]), (42 + 64, 42 + 96), "real target advances one period per alarm");
    assert!(m.cpu().ar[13] >= 42 + 64, "period alarms never reload UNIT1");
}

#[test]
fn boot_rwdt_uses_junk_count_resets_retains_sram_and_rewinds() {
    let board = profile(24);
    let junk = handoff_time_junk(board.seed, &board.time);
    // TRM p.676 stage0 scaling and p.677 boot action; p.528 cause 0x10.
    let due = Clock::new(board.time.rc_slow_hz.0, board.time.rc_slow_hz.1, 0, TICK_HZ)
        .edge((200_000 << (board.time.wdt_delay_sel + 1)) - junk.rwdt_count);
    let mut p = Program::new();
    p.read_time(RTC + 0x38, 6);
    read_boot_storage(&mut p);
    let cause_read = p.pc();
    p.read_time(RTC + 0xb0, 7);
    p.disable_mwdt(TIMG0);
    p.auto_feed_swd();
    p.store(RAM, 0x1357_2468);
    write_boot_storage(&mut p);
    p.time_sleep();
    let asleep = access_tick(p.pc());
    let mut m = S3Machine::new(p.finish(), &board);
    assert_eq!(run_to(&mut m, asleep, &PcSet::default()), Ran::Tick, "boot watchdog fixture enters WAITI");
    assert_eq!(m.cpu().ar[6] & 63, 1, "power-on cause is 0x01");
    assert_eq!(m.cpu().ar[7], junk.rwdt_key, "independent junk oracle matches the readable handoff key");
    assert_boot_storage(&m, false, false);
    m.mark();
    reset_at(&mut m, due, 0x10, ResetScope::System);
    let mut bytes = [0; 4];
    assert!(m.read_data(RAM, &mut bytes), "SRAM remains readable after reset");
    assert_eq!(u32::from_le_bytes(bytes), 0x1357_2468, "System reset preserves SRAM");
    at_pc(&mut m, cause_read);
    assert_eq!(m.cpu().ar[6] & 63, 0x10, "RESET_STATE reports the RWDT System reset after rehandoff");
    assert_boot_storage(&m, false, false);
    m.rewind();
    assert_eq!(m.now(), asleep, "rewind restores the old epoch");
    assert!(m.last_reset().is_none(), "rewind removes the later reset record");
    reset_at(&mut m, due, 0x10, ResetScope::System);
}

#[test]
fn boot_mwdt0_and_swd_have_their_own_reset_causes() {
    for swd in [false, true] {
        let board = profile(25);
        let junk = handoff_time_junk(board.seed, &board.time);
        let due = if swd { board.time.swd_timeout_ticks - junk.swd_elapsed }
            else { (26_000_000 - junk.mwdt0_count) * 24 };
        let mut p = Program::new();
        p.read_time(RTC + 0x38, 6);
        let cause_read = p.pc();
        p.disable_rwdt();
        if swd { p.disable_mwdt(TIMG0); } else { p.auto_feed_swd(); }
        p.time_sleep();
        let mut m = S3Machine::new(p.finish(), &board);
        let (cause, scope) = if swd { (0x12, ResetScope::System) } else { (0x07, ResetScope::Core) };
        reset_at(&mut m, due, cause, scope);
        at_pc(&mut m, cause_read);
        assert_eq!(m.cpu().ar[6] & 63, u32::from(cause), "RESET_STATE distinguishes SWD and MWDT0");
    }
}

#[test]
fn documented_disables_wrong_keys_and_auto_feeding() {
    let mut p = Program::new();
    // TRM p.677: wrong key ignores both configuration and feed, key stays writable.
    p.store(RTC + 0xb0, 0);
    p.store(RTC + 0x98, u32::MAX);
    p.store(RTC + 0xac, 0);
    p.read_time(RTC + 0x98, 7);
    p.store(TIMG0 + 0x64, 0);
    p.store(TIMG0 + 0x48, u32::MAX);
    p.store(TIMG0 + 0x4c, 0);
    p.read_time(TIMG0 + 0x48, 8);
    p.disable_watchdogs();
    p.read_time(RTC + 0x98, 9);
    p.read_time(TIMG0 + 0x48, 10);
    p.read_time(RTC + 0xb0, 11);
    p.time_sleep();
    let mut m = S3Machine::new(p.finish(), &profile(26));
    assert_eq!(run_to(&mut m, 10 * TICK_HZ, &PcSet::default()), Ran::Tick, "disabled digital watchdogs and auto-fed SWD permit a long sleep");
    assert_eq!((m.cpu().ar[7], m.cpu().ar[8]), (0x13214, 0x4c000), "locked writes leave documented configurations intact");
    assert_eq!((m.cpu().ar[9], m.cpu().ar[10]), (0x12214, 0x48000), "disable clears only EN and flash-boot mode");
    assert_eq!(m.cpu().ar[11], 0, "disable helper restores digital write protection");
    assert!(m.last_reset().is_none(), "auto-fed SWD never resets the sleeping core");
}

#[test]
fn wifi_power_and_isolation_follow_rtc_reset_scope() {
    const PWC: u32 = RTC + 0x90;
    const ISO: u32 = RTC + 0x94;
    const RESET: [u32; 2] = [0x0054_5010, 0xaa80_5080];
    const FORCED: [u32; 2] = [RESET[0] | (1 << 17), RESET[1] | (1 << 28)];

    for scope in [ResetScope::Cpu, ResetScope::Core, ResetScope::System] {
        let mut p = Program::new();
        p.read_time(PWC, 8);
        p.read_time(ISO, 9);
        let sampled = p.pc();
        p.store(PWC, FORCED[0]);
        p.store(ISO, FORCED[1]);
        p.read_time(PWC, 8);
        p.read_time(ISO, 9);
        let written = p.pc();
        match scope {
            ResetScope::Cpu => { p.store(RTC, 1 << 5); }
            ResetScope::Core => { p.store(RTC, 1 << 31); }
            ResetScope::System => {
                p.disable_rwdt(); p.disable_mwdt(TIMG0); p.time_sleep(); // SWD expires in this epoch.
            }
        }
        p.halt();
        let mut m = S3Machine::new(p.finish(), &profile(25));
        at_pc(&mut m, sampled);
        assert_eq!(m.cpu().ar[8..10], RESET, "power-on RTC words");
        at_pc(&mut m, written);
        assert_eq!(m.cpu().ar[8..10], FORCED, "both Wi-Fi overrides read back");
        assert!(matches!(run_to(&mut m, TICK_HZ, &PcSet::default()), Ran::Reset { .. }));
        assert_eq!(m.last_reset().unwrap().scope, scope);
        at_pc(&mut m, sampled);
        let expected = if scope == ResetScope::System { RESET } else { FORCED };
        assert_eq!(m.cpu().ar[8..10], expected, "RTC words at {scope:?} handoff");
    }

    for (addr, value, why) in [
        (PWC, FORCED[0] | (1 << 21), "RTC_CNTL DIG_PWC field other than WIFI_FORCE_PD not modelled"),
        (ISO, FORCED[1] | (1 << 26), "RTC_CNTL DIG_ISO field other than WIFI_FORCE_ISO not modelled"),
        (ISO, FORCED[1] | (1 << 9), "RTC_CNTL DIG_ISO field other than WIFI_FORCE_ISO not modelled"),
        (ISO, FORCED[1] | (1 << 10), "RTC_CNTL DIG_ISO pad autohold clear not modelled"),
    ] {
        let mut p = Program::new();
        let pc = p.store(addr, value);
        let mut m = S3Machine::new(p.finish(), &profile(25));
        at_pc(&mut m, pc);
        let retired = m.cpu().instret;
        assert_stop(&mut m, pc, why, Some(StopDetail { addr, width: Width::W, kind: Kind::Store, mmu_entry: None }));
        assert_eq!(m.cpu().instret, retired, "the rejected store does not retire");
    }
}

#[test]
fn software_core_reset_records_cause_and_keeps_rtc_configuration() {
    // facts-machine.md:363,369,383–384; rtc_cntl_reg.h:22–28,131–136:
    // SW_SYS_RST is Core (cause 3); SW_PROCPU_RST is CPU0 (cause 12).
    for (request, cause, scope) in [(1 << 31, 3, ResetScope::Core), (1 << 5, 12, ResetScope::Cpu)] {
        let mut p = Program::new();
        p.read_time(RTC + 0x38, 6);
        p.read_time(RTC + 0x9c, 7);
        read_boot_storage(&mut p);
        let readback = p.pc();
        p.disable_watchdogs();
        p.store(RTC + 0xb0, WKEY);
        p.store(RTC + 0x9c, 12345);
        write_boot_storage(&mut p);
        let reset_pc = p.store(RTC, request);
        p.halt();
        let mut m = S3Machine::new(p.finish(), &profile(27));
        assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Reset { at: access_tick(reset_pc) }, "software reset uses its store's start tick");
        assert_eq!(m.now(), 0, "software reset starts a new epoch");
        at_pc(&mut m, readback);
        assert_eq!(m.cpu().ar[6] & 63, cause, "software reset reports its scope's cause");
        assert_eq!(m.cpu().ar[7], 12345, "CPU/Core reset retains RTC watchdog configuration");
        assert_boot_storage(&m, true, scope == ResetScope::Cpu);
    }
}

#[test]
fn seeds_change_handoff_junk_but_not_documented_reset_fields() {
    let mut p = Program::new();
    p.read_time(RTC + 0xb0, 7);
    p.read_time(SYSTIMER, 8);
    p.read_time(SYSTIMER + 0xfc, 9);
    p.read_time(RTC + 0x98, 10);
    p.read_time(TIMG0 + 0x48, 11);
    p.read_time(TIMG1 + 0x48, 12);
    let update = p.store(SYSTIMER + 4, 1 << 30);
    pad_to_valid(&mut p, update);
    p.read_time(SYSTIMER + 0x44, 13);
    p.halt();
    let image = p.finish();
    let mut a = S3Machine::new(image.clone(), &profile(28));
    let mut b = S3Machine::new(image, &profile(29));
    finish(&mut a);
    finish(&mut b);
    assert_ne!(a.cpu().ar[7], b.cpu().ar[7], "undocumented handoff keys come from seed junk");
    assert_ne!(a.cpu().ar[13], b.cpu().ar[13], "elapsed handoff SYSTIMER counts come from seed junk");
    assert_eq!(&a.cpu().ar[8..13], &[0x46000000, 0x02012251, 0x13214, 0x4c000, 0x4c000], "header reset fields retain their documented values");
    assert_eq!(&a.cpu().ar[8..13], &b.cpu().ar[8..13], "seed does not change documented reset fields");
}

#[test]
fn feed_outside_the_sync_window_postpones_reset_inside_lands_late() {
    // facts-machine.md §5's explicit model convention: inclusive two-cycle
    // window on RC_SLOW / prescaled APB. At the expiry tick events run first.
    for base in [RTC, TIMG0, TIMG1] {
        for cycles_before in [3, 2, 1, 0] {
            let mut board = profile(30);
            board.time.rc_slow_hz = (2_000_000, 1); // M: 240-tick RC_SLOW for this property
            let mut p = Program::new();
            p.disable_watchdogs();
            let rtc = base == RTC;
            let (key, config, hold, feed) = if rtc { (0xb0, 0x98, 0x9c, 0xac) }
                else { (0x64, 0x48, 0x50, 0x60) };
            p.store(base + key, WKEY);
            let phase = if rtc { 0 } else {
                // TRM p.675: MWDT source APB / prescale; 24 * 10 = 240 ticks.
                access_tick(p.store(base + 0x4c, 10 << 16))
            };
            // Stage0 lasts 20 cycles for either kind (RWDT holds are doubled).
            p.store(base + hold, if rtc { 10 } else { 20 });
            p.store(base + feed, 1 << 31);
            let enabled = p.store(base + config, (1 << 31) | if rtc { 4 << 28 } else { 3 << 29 });
            let due = phase + ((access_tick(enabled) - phase) / 240 + 20) * 240;
            p.load(1, base + feed);
            p.load(2, 1 << 31);
            let fed_at = due - cycles_before * 240;
            pad_to(&mut p, fed_at);
            p.emit(enc::s32i(2, 1, 0));
            p.time_sleep();
            let mut m = S3Machine::new(p.finish(), &board);
            m.mark();
            let expected = if cycles_before > 2 { fed_at + 20 * 240 } else { due };
            let (cause, scope) = if rtc { (0x10, ResetScope::System) }
                else { (if base == TIMG0 { 7 } else { 8 }, ResetScope::Core) };
            // The late feed is noted once, at its write; a feed outside the window, or one the
            // expiry overtakes, is not.
            let noted: &[u64] = if (1..=2).contains(&cycles_before) { &[fed_at] } else { &[] };
            for pass in 0..2 {
                // On the same-tick case the CPU is executing padding until expiry;
                // only WAITI can stop at due-1 exactly, so inspect the reset directly.
                assert_eq!(run_to(&mut m, expected + 24, &PcSet::default()), Ran::Reset { at: expected },
                    "watchdog {base:#x} feed {cycles_before} cycles before expiry follows the late-write convention");
                let reset = m.last_reset().expect("sync-window reset remains inspectable");
                assert_eq!((reset.cause, reset.scope), (cause, scope), "late feeding preserves the expiring action");
                let mut notes = Vec::new();
                m.take_notes(&mut notes);
                assert_eq!(notes.iter().map(|n| n.tick).collect::<Vec<_>>(), noted,
                    "watchdog {base:#x} feed {cycles_before} cycles before expiry, pass {pass}: {notes:?}");
                assert!(notes.iter().all(|n| n.what.contains("sync window")), "the note names the race: {notes:?}");
                // a rewind to before the feed takes its note back with it; the re-run notes it again
                m.rewind();
            }
        }
    }
}

#[test]
fn undocumented_register_accesses_stop_with_address_and_message() {
    // Q6: reserved fields, WO/RO accesses, unsupported blocks and source
    // discrepancies stop rather than obtaining invented behavior.
    for (addr, value, what) in [
        (RTC + 4, None, "RTC_CNTL register not modelled"),
        (RTC + 4, Some(0), "RTC_CNTL register not modelled"),
        (TIMG0, None, "timer group general timer not modelled"),
        (TIMG1, Some(0), "timer group general timer not modelled"),
        (SYSTIMER + 0x8c, None, "SYSTIMER register not modelled"),
        (SYSTIMER + 0x8c, Some(0), "SYSTIMER register not modelled"),
        (SYSTIMER + 0x50, None, "read of write-only time register is undocumented"),
        (RTC + 0xac, None, "read of write-only time register is undocumented"),
        (TIMG0 + 0x60, None, "read of write-only time register is undocumented"),
        (SYSTIMER + 0x40, Some(0), "write to read-only time register is undocumented"),
        (RTC + 0x44, Some(0), "write to read-only time register is undocumented"),
        (TIMG1 + 0x78, Some(0), "write to read-only time register is undocumented"),
        (SYSTIMER + 0x64, Some(8), "reserved time-register field is undocumented"),
        (SYSTIMER, Some(1), "SYSTIMER CLK_FO semantics are undocumented"),
        (SYSTIMER + 4, Some(1 << 29), "direct SYSTIMER VALID write is undocumented"),
        (SYSTIMER + 0x68, Some(1), "direct SYSTIMER RAW write is undocumented"),
        (TIMG0 + 0x74, Some(4), "direct MWDT RAW write is undocumented"),
        (TIMG0 + 0x4c, Some(0), "MWDT prescale of zero is undocumented"),
        (RTC + 0x98, Some(1 << 8), "RWDT CHIP_RESET_EN scope is undocumented"),
        (RTC + 0xac, Some(0), "RWDT feed value without bit 31 is undocumented"),
        (RTC, Some(1 << 29), "RTC_CNTL OPTIONS0 field not modelled"),
        (RTC, Some((1 << 31) | (1 << 5)), "simultaneous software reset causes are undocumented"),
        (RTC + 0x38, Some(0), "RTC_CNTL reset mask, vector or OCD halt not modelled"),
        (RTC + 0x4c, Some(8), "RTC_CNTL interrupt clear write-value rule is undocumented"),
        (RTC + 0xb4, Some((300 << 18) | (1 << 17)), "SWD BYPASS_RST truth table is undocumented"),
        (RTC + 0xb4, Some(0), "SWD SIGNAL_WIDTH units are undocumented"),
        (RTC + 0xb4, Some(300 << 18), "SWD re-enable timing without a feed is undocumented"),
    ] {
        let mut p = Program::new();
        p.disable_watchdogs();
        p.store(RTC + 0xb0, WKEY);
        p.store(TIMG0 + 0x64, WKEY);
        p.store(TIMG1 + 0x64, WKEY);
        let pc = if let Some(value) = value { p.store(addr, value) } else { p.read_time(addr, 7) };
        let mut m = S3Machine::new(p.finish(), &profile(31));
        assert_stop(&mut m, pc, what, Some(StopDetail {
            addr, width: Width::W, kind: if value.is_some() { Kind::Store } else { Kind::Load }, mmu_entry: None,
        }));
    }
}

#[test]
fn watchdog_protection_gaps_and_clock_switch_stop() {
    for (addr, value, what) in [
        (RTC + 0x40, 8, "shared watchdog interrupt-register write protection is undocumented"),
        (RTC + 0x40, 1 << 15, "shared watchdog interrupt-register write protection is undocumented"),
        (TIMG0 + 0x70, 4, "shared watchdog interrupt-register write protection is undocumented"),
        (RTC + 0xb4, (300 << 18) | (1 << 29), "SWD feed without its unlock key is undocumented"),
        (RTC + 0xb4, (300 << 18) | (1 << 31), "SWD configuration write protection is undocumented"),
    ] {
        let mut p = Program::new();
        p.disable_watchdogs();
        p.store(RTC + 0xb8, 0);
        let pc = p.store(addr, value);
        let mut m = S3Machine::new(p.finish(), &profile(32));
        assert_stop(&mut m, pc, what, Some(StopDetail { addr, width: Width::W, kind: Kind::Store, mmu_entry: None }));
    }
    for divider in [false, true] {
        let mut p = Program::new();
        p.store(TIMG0 + 0x64, WKEY);
        // Boot MWDT0 is still counting, so both switches are undocumented (§5).
        let (addr, value, what) = if divider {
            (TIMG0 + 0x4c, 2 << 16, "MWDT divider change while counting is undocumented")
        } else { (SYSCLK, 0, "APB clock change while MWDT counts is undocumented") };
        let pc = p.store(addr, value);
        let mut m = S3Machine::new(p.finish(), &profile(33));
        assert_stop(&mut m, pc, what, Some(StopDetail { addr, width: Width::W, kind: Kind::Store, mmu_entry: None }));
    }
}

#[test]
fn ambiguous_comparisons_zero_period_and_one_shot_readback_stop() {
    // TRM Table 11.4-2 / facts-machine.md §1 NOT FOUND cases.
    for (count, target, mode, what) in [
        ((1u64 << 51) - 1, 0, 0, "SYSTIMER comparison at the disputed Table 11.4-2 boundary"),
        (1u64 << 51, (1u64 << 51) - 1, 0, "SYSTIMER comparison across the undocumented half-range boundary"),
        (0, 0, 1 << 30, "SYSTIMER zero period is undocumented"),
        (0, 0, 0, "SYSTIMER WORK_EN after one-shot firing is undocumented"),
    ] {
        let mut p = Program::new();
        p.disable_watchdogs();
        p.store(SYSTIMER, 0);
        p.store(SYSTIMER + 0x0c, (count >> 32) as u32);
        p.store(SYSTIMER + 0x10, count as u32);
        p.store(SYSTIMER + 0x5c, 1);
        p.store(SYSTIMER + 0x1c, (target >> 32) as u32);
        p.store(SYSTIMER + 0x20, target as u32);
        p.store(SYSTIMER + 0x34, mode);
        p.store(SYSTIMER + 0x50, 1);
        let enable = p.store(SYSTIMER, 1 << 24);
        let readback = p.read_time(SYSTIMER, 7);
        let read = count == 0 && mode == 0;
        let mut m = S3Machine::new(p.finish(), &profile(34));
        assert_stop(&mut m, if read { readback } else { enable }, what, Some(StopDetail {
            addr: SYSTIMER, width: Width::W, kind: if read { Kind::Load } else { Kind::Store }, mmu_entry: None,
        }));
    }
}

#[test]
fn invalid_stage_configuration_and_unenabled_cpu_reset_stop() {
    for (hold, action, what) in [
        (0, 0, "watchdog off stage with zero hold is undocumented"),
        (0, 1, "watchdog zero stage hold is undocumented"),
        (u32::MAX, 1, "RWDT scaled hold exceeds its documented 32-bit counter"),
        (10, 5, "RWDT stage action is undocumented"),
        (10, 2, "watchdog CPU reset action with PROCPU_RESET_EN clear is undocumented"),
    ] {
        let mut p = Program::new();
        p.disable_watchdogs();
        p.store(RTC + 0xb0, WKEY);
        p.store(RTC + 0x9c, hold);
        p.store(RTC + 0xac, 1 << 31);
        let enabled = p.store(RTC + 0x98, (1 << 31) | (action << 28));
        p.time_sleep();
        let after_wait = p.pc();
        let mut m = S3Machine::new(p.finish(), &profile(35));
        assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Halted, "unsupported stage behavior stops: {what}");
        let Status::Halted(h) = m.status() else { panic!("stage failure remains inspectable"); };
        assert_eq!((h.kind, h.what), (HaltKind::Unmodeled, what), "stage failure names the missing fact");
        assert_eq!(h.pc, if action == 2 { after_wait } else { enabled }, "stage failure identifies access or event boundary");
    }
}

#[test]
fn enabled_cpu_reset_actions_use_the_documented_cause_codes() {
    // TRM p.528 Table 7.1-1, pp.613,665 reset-enable fields.
    for (base, cause) in [(RTC, 0x0d), (TIMG0, 0x0b), (TIMG1, 0x11)] {
        let rtc = base == RTC;
        let mut p = Program::new();
        p.read_time(RTC + 0x38, 7);
        let readback = p.pc();
        p.disable_watchdogs();
        p.store(base + if rtc { 0xb0 } else { 0x64 }, WKEY);
        p.store(base + if rtc { 0x9c } else { 0x50 }, 40);
        p.store(base + if rtc { 0xac } else { 0x60 }, 1 << 31);
        let enabled = p.store(base + if rtc { 0x98 } else { 0x48 },
            (1 << 31) | if rtc { (2 << 28) | (1 << 11) } else { (2 << 29) | (1 << 13) });
        p.time_sleep();
        let board = profile(36);
        let due = if rtc {
            let clock = Clock::new(board.time.rc_slow_hz.0, board.time.rc_slow_hz.1, 0, TICK_HZ);
            clock.edge(clock.edges_through(access_tick(enabled)) - 1 + 80)
        } else { access_tick(enabled) + 40 * 24 };
        let mut m = S3Machine::new(p.finish(), &board);
        reset_at(&mut m, due, cause, ResetScope::Cpu);
        at_pc(&mut m, readback);
        assert_eq!(m.cpu().ar[7] & 63, u32::from(cause), "CPU watchdog reset has its source-specific cause");
    }
}

#[test]
fn software_cpu_reset_cannot_guess_a_retained_mwdt_clock_switch() {
    let mut p = Program::new();
    p.disable_watchdogs();
    p.store(SYSCLK, 0); // facts.md §3: XTAL/1, changes APB from 24 to 12 ticks
    p.store(TIMG0 + 0x64, WKEY);
    p.store(TIMG0 + 0x48, 1 << 31);
    let pc = p.store(RTC, 1 << 5);
    let mut m = S3Machine::new(p.finish(), &profile(37));
    assert_stop(&mut m, pc, "MWDT APB switch across CPU reset is undocumented",
        Some(StopDetail { addr: RTC, width: Width::W, kind: Kind::Store, mmu_entry: None }));
}

#[test]
fn coinciding_watchdog_resets_are_modelled_as_the_widest_then_first() {
    let mut p = Program::new();
    p.disable_watchdogs();
    for (base, hold) in [(TIMG0, 100), (TIMG1, 97)] {
        p.store(base + 0x64, WKEY);
        p.store(base + 0x50, hold);
    }
    // Three CPU/APB cycles between enable accesses; expiry ticks coincide.
    p.store(TIMG0 + 0x48, (1 << 31) | (3 << 29));
    p.store(TIMG1 + 0x48, (1 << 31) | (3 << 29));
    p.time_sleep();
    let after_wait = p.pc();
    let mut m = S3Machine::new(p.finish(), &profile(38));
    // A race: periph/time.rs notes it and takes the widest scope, ties in
    // block order (MWDT0 before MWDT1).
    let Ran::Reset { at } = run_to(&mut m, 100_000, &PcSet::default()) else {
        panic!("coinciding expiries after {after_wait:#x} reset the machine instead of stopping");
    };
    let reset = m.last_reset().expect("the reset remains inspectable");
    assert_eq!((reset.tick, reset.cause, reset.scope), (at, 7, ResetScope::Core), "equal Core scopes resolve to MWDT0's cause");
}

#[test]
fn shortening_timeout_behind_count_expires_at_the_config_write() {
    let mut p = Program::new();
    p.disable_watchdogs();
    p.store(TIMG0 + 0x64, WKEY);
    p.store(TIMG0 + 0x50, 100);
    p.store(TIMG0 + 0x48, (1 << 31) | (3 << 29));
    let pc = p.store(TIMG0 + 0x50, 1); // count is now 3; far outside sync window
    p.time_sleep();
    let mut m = S3Machine::new(p.finish(), &profile(39));
    // A race: modelled as immediate expiry, with a note.
    assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Reset { at: access_tick(pc) },
        "a hold behind the count expires at the write's tick");
    let reset = m.last_reset().expect("the reset remains inspectable");
    assert_eq!((reset.cause, reset.scope), (7, ResetScope::Core), "the expiring stage's action applies");
}

#[test]
fn swd_feed_request_is_exact_and_a_feed_leaves_the_request_pending() {
    let board = profile(40);
    let junk = handoff_time_junk(board.seed, &board.time);
    let expiry = board.time.swd_timeout_ticks - junk.swd_elapsed;
    let request = expiry.saturating_sub(board.time.swd_request_lead_ticks);
    let mut p = Program::new();
    p.disable_rwdt();
    p.disable_mwdt(TIMG0);
    p.store(RTC + 0xb8, SWKEY);
    p.store(RTC + 0x40, 1 << 15);
    p.time_sleep();
    let mut m = S3Machine::new(p.finish(), &board);
    if request > m.now() + 4096 {
        assert_eq!(run_to(&mut m, request - 1, &PcSet::default()), Ran::Tick, "SWD request has not interrupted the sleeping CPU");
        assert_eq!(m.now(), request - 1, "WAITI reaches the tick before SWD request exactly");
        assert_eq!(run_to(&mut m, request, &PcSet::default()), Ran::Tick, "SWD request is a peripheral event, not a reset");
    }
    assert_eq!(run_to(&mut m, expiry, &PcSet::default()), Ran::Reset { at: expiry }, "unserviced request still expires on schedule");

    // A profile with an early request makes its observation a short firmware
    // test. Timeout exceeds any u32 handoff elapsed junk, so startup cannot
    // race SWD expiry. Feed starts a full fresh timeout (TRM p.678).
    for raw in [true, false] {
        let mut board = profile(41);
        board.time.swd_timeout_ticks = (1u64 << 32) + 100_000;
        board.time.swd_request_lead_ticks = board.time.swd_timeout_ticks - 10_000;
        let mut p = Program::new();
        p.disable_watchdogs();
        // Clear AUTO_FEED and feed before any request; FEED_INT may already
        // be set at handoff, so first let the next request establish it again.
        p.store(RTC + 0xb4, (300 << 18) | (1 << 29));
        let fed = access_tick(p.pc()) - 24;
        p.load(1, RTC + 0x44);
        let request = ((fed + 10_000 + 23) / 24) * 24;
        pad_to(&mut p, request);
        p.emit(enc::l32i(7, 1, 0));
        p.store(RTC + 0xb4, (300 << 18) | (1 << 29));
        let addr = RTC + if raw { 0x44 } else { 0xb4 };
        p.read_time(addr, 8);
        p.halt();
        let mut m = S3Machine::new(p.finish(), &board);
        finish(&mut m);
        assert_eq!(m.cpu().ar[7], 1 << 15, "SWD request asserts bit 15 before it is fed");
        // A race: the feed's effect on a pending request is modelled as none.
        let pending = if raw { 1 << 15 } else { 2 };
        assert_eq!(m.cpu().ar[8] & pending, pending, "a feed leaves the SWD request pending ({addr:#x})");
    }
}

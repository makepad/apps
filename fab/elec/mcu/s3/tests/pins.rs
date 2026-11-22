use fab_elec_mcu_emit32::xtensa::{ccz, enc};
use fab_elec_mcu_emu::board::BusEvent;
use fab_elec_mcu_emu::bus::{Kind, Width};
use fab_elec_mcu_emu::{Drive, DriveEdge, HaltKind, LinkOut, Machine, PcSet, Pull, Ran, Status, Until};
use fab_elec_mcu_parts::{BoardDesc, NetDesc, Netlist, PartDesc, PartKind, PinRef, Stimulus};
use fab_elec_mcu_s3::{ResetScope, S3Machine, StopDetail};

mod support;
use support::*;

#[test]
fn board_levels_cross_the_pads_and_rewind_with_the_machine() {
    let part_pin = |part, pin: &str| PinRef::Part { part, pin: pin.into() };
    let desc = BoardDesc {
        seed: 0,
        parts: vec![
            PartDesc { refdes: "T1".into(), kind: PartKind::Not },
            PartDesc { refdes: "T2".into(), kind: PartKind::Not },
            PartDesc { refdes: "K1".into(), kind: PartKind::Key },
        ],
        nets: vec![
            NetDesc { rise_ps: None, name: "OUT".into(), pull: Pull::None,
                pins: vec![PinRef::Chip("GPIO4".into()), part_pin(0, "G"), part_pin(1, "G")] },
            NetDesc { rise_ps: None, name: "BACK".into(), pull: Pull::Up,
                pins: vec![part_pin(0, "D"), PinRef::Chip("GPIO5".into())] },
            NetDesc { rise_ps: None, name: "KEY".into(), pull: Pull::Up,
                pins: vec![part_pin(2, "A"), PinRef::Chip("GPIO6".into())] },
            NetDesc { rise_ps: None, name: "FIGHT".into(), pull: Pull::Up,
                pins: vec![part_pin(1, "D"), PinRef::Chip("GPIO7".into())] },
        ],
    };
    let mut p = Program::new();
    for gpio in [4, 7] { p.gpio_setup(gpio, 0); }
    for gpio in [5, 6] { p.gpio_setup(gpio, MUX_IE); }
    p.store(GPIO_ENABLE, (1 << 4) | (1 << 7));
    p.load(3, RAM);
    let low_pc = p.store(GPIO_OUT, 0);
    p.pin_read(5, GPIO_IN);
    p.emit(enc::s32i(5, 3, 0));
    let high_pc = p.store(GPIO_OUT, 1 << 4);
    p.pin_read(5, GPIO_IN);
    p.emit(enc::s32i(5, 3, 4));
    p.emit(enc::movi(4, 8));
    let loop_pc = p.pc();
    p.pin_read(5, GPIO_IN);
    p.emit(enc::s32i(5, 3, 8));
    let sampled = p.emit(enc::addi(4, 4, -1));
    p.emit(enc::bccz(ccz::NE, 4, (i64::from(loop_pc) - i64::from(p.pc()) - 4) as i32));
    let fight_pc = p.store(GPIO_OUT + 4, 1 << 7);
    p.halt();
    let image = p.finish();
    let mut handoff = profile(123);
    // Start OUT high so both GPIO4 stores change its level. GPIO7 is
    // released at handoff, then driven low until the final contention.
    handoff.pins.handoff_enable[0] = 1 << 4;
    handoff.pins.handoff_out[0] = 1 << 4;
    let fresh = || S3Machine::with_board(
        image.clone(), &handoff,
        Netlist::new(desc.clone()).expect("the inverter and key wiring must form a valid board"),
    ).expect("the board must bind and accept the handoff drives");
    let run = |m: &mut S3Machine<Netlist>, tick| m.run(&Until {
        tick, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false,
    });

    let mut straight = fresh();
    assert_eq!(straight.nets().iter().map(|net| &*net.name).collect::<Vec<_>>(),
        ["OUT", "BACK", "KEY", "FIGHT"], "the machine exposes the board's net names in board order");
    pins_at_pc(&mut straight, sampled);
    let before = received_words(&straight, RAM, 3);
    assert_eq!(before[0] & (1 << 5), 1 << 5, "OUT low releases T1 and firmware reads BACK pulled high");
    assert_eq!(before[1] & (1 << 5), 0, "OUT high makes T1 sink BACK before the next firmware read");
    assert_eq!(before[2] & (1 << 6), 1 << 6, "the counted loop reads KEY high before the press");
    let press_tick = straight.now();
    straight.board_in(Stimulus::Key { part: 2, down: true });
    assert_eq!(run(&mut straight, 100_000), Ran::Halted, "the final GPIO7 store stops the straight run");
    assert_eq!(received_words(&straight, RAM, 3)[2] & (1 << 6), 0,
        "a later counted-loop read sees the pressed key through GPIO6");
    let Status::Halted(halt) = straight.status() else { panic!("board contention must latch a halt"); };
    assert_eq!((halt.kind, halt.pc, halt.what),
        (HaltKind::Unmodeled, fight_pc, "contention on a board net"),
        "contention retains the GPIO7 store's PC and the board's reason");

    let (low_tick, high_tick) = (access_tick(low_pc), access_tick(high_pc));
    let mut drives = Vec::new();
    straight.take_drives(&mut drives);
    assert_eq!(drives.iter().filter(|edge| edge.pin == 4).copied().collect::<Vec<_>>(), vec![
        DriveEdge { tick: low_tick, pin: 4, drive: Drive::Low },
        DriveEdge { tick: high_tick, pin: 4, drive: Drive::High },
    ], "GPIO4 drive records retain both stores' start ticks");
    let mut straight_bus = Vec::new();
    straight.take_bus(&mut straight_bus);
    straight.mark();
    let out = straight.board().net("OUT").expect("OUT must be named");
    let back = straight.board().net("BACK").expect("BACK must be named");
    let levels: Vec<_> = straight_bus.iter().filter(|event| matches!(event,
        BusEvent::Level { t, net, .. } if *t != 0 && (*net == out || *net == back)
    )).copied().collect();
    assert_eq!(levels, vec![
        BusEvent::Level { t: low_tick, net: out, high: false },
        BusEvent::Level { t: low_tick, net: back, high: true },
        BusEvent::Level { t: high_tick, net: out, high: true },
        BusEvent::Level { t: high_tick, net: back, high: false },
    ], "OUT then BACK settle in order at each GPIO4 store's drive tick");

    let mut split = fresh();
    let mut split_bus = Vec::new();
    while split.now() < straight.now() {
        split.take_bus(&mut split_bus);
        split.mark();
        let start = split.now();
        let deadline = (start + 5 * 24).min(straight.now());
        let press = start <= press_tick && press_tick < deadline;
        let piece = |m: &mut S3Machine<Netlist>| {
            if press {
                assert_eq!(run(m, press_tick), Ran::Tick, "each replay reaches the stimulus boundary");
                assert_eq!(m.now(), press_tick, "each replay presses the key at the same machine tick");
                m.board_in(Stimulus::Key { part: 2, down: true });
            }
            run(m, deadline)
        };
        let ahead = piece(&mut split);
        let status = split.status();
        let tick = split.now();
        assert!(matches!(ahead, Ran::Tick | Ran::Halted), "each short piece reaches its deadline or contention");
        assert!(tick > start, "each short piece advances firmware execution");
        split.rewind();
        assert_eq!(piece(&mut split), ahead, "rewinding and replaying a piece preserves its stop reason");
        assert_eq!((split.now(), split.status()), (tick, status),
            "rewinding and replaying a piece preserves its tick and halt state");
    }
    split.take_bus(&mut split_bus);
    split.mark();
    assert_eq!(split_bus, straight_bus, "split replay preserves every board record without loss or duplication");
    let (mut straight_words, mut split_words) = ([0; 12], [0; 12]);
    assert!(straight.read_data(RAM, &mut straight_words) && split.read_data(RAM, &mut split_words),
        "both runs expose all three sampled RAM words");
    assert_eq!(split_words, straight_words, "split replay preserves all bits of RAM words 0 through 2");
    assert_eq!(split.status(), straight.status(), "split replay reaches the identical contention halt");
}

#[test]
fn output_aliases_open_drain_pulls_and_inversions_report_the_store_start_tick() {
    let mut p = Program::new();
    p.gpio_setup(4, 0);
    let start = p.pc();
    // facts.md §4a / TRM pp.477–478,501–505,507–508,514–515.
    let mut steps = Vec::new();
    for (addr, value, drive) in [
        (GPIO_ENABLE + 4, 1 << 4, Drive::Low),
        (GPIO_OUT, 1 << 4, Drive::High),
        (GPIO_OUT + 8, 1 << 4, Drive::Low),
        (GPIO_OUT + 4, 1 << 4, Drive::High),
        (pin_reg(4), 4, Drive::OpenDrain { low: false, pull: Pull::None }),
        (GPIO_OUT + 8, 1 << 4, Drive::OpenDrain { low: true, pull: Pull::None }),
        (mux_reg(4), MUX_GPIO | MUX_UP, Drive::OpenDrain { low: true, pull: Pull::Up }),
        (GPIO_OUT + 4, 1 << 4, Drive::OpenDrain { low: false, pull: Pull::Up }),
        (GPIO_ENABLE + 8, 1 << 4, Drive::Input(Pull::Up)),
        (mux_reg(4), MUX_GPIO | MUX_DOWN, Drive::Input(Pull::Down)),
        (mux_reg(4), MUX_GPIO, Drive::Input(Pull::None)),
        (pin_reg(4), 0, Drive::Input(Pull::None)),
        (out_sel_reg(4), 256 | (1 << 9), Drive::Input(Pull::None)),
        (GPIO_ENABLE, 1 << 4, Drive::Low),
        (out_sel_reg(4), 256 | (1 << 11), Drive::Input(Pull::None)),
        (GPIO_ENABLE + 8, 1 << 4, Drive::High),
        (out_sel_reg(4), 256 | (1 << 10) | (1 << 11), Drive::High),
        (GPIO_OUT + 4, 1 << 4, Drive::High),
    ] {
        steps.push((p.store(addr, value), drive));
    }
    p.halt();
    let mut m = S3Machine::new(p.finish(), &profile(101));
    at_pc(&mut m, start);
    let mut edges = Vec::new();
    m.take_drives(&mut edges);
    edges.clear();
    let (mut last, mut want) = (m.drive(4), Vec::new());
    for (pc, expected) in steps {
        let generation = m.drive_gen();
        let at = u64::from((pc - ENTRY) / 3) * 24;
        assert_eq!(pins_drive(&mut m), Ran::Drive { at },
            "drive notification timestamps the writing instruction, including unchanged writes at {pc:#x}");
        assert_eq!(m.pc(), pc + 3, "drive return follows exactly the writing instruction");
        assert_eq!(m.drive(4), expected, "GPIO4 drive follows its latch, enable, selector, driver and pull fields");
        assert_eq!(m.drive_gen(), generation + 1, "each potentially driving store increments the generation once");
        if expected != last {
            want.push(DriveEdge { tick: at, pin: 4, drive: expected });
            last = expected;
        }
    }
    m.take_drives(&mut edges);
    assert_eq!(edges, want, "the drive log has one edge per change, at its store's start tick, none for an unchanged write");
    let generation = m.drive_gen();
    m.set_input(4, false);
    assert_eq!(m.drive(4), Drive::High, "sampled external input does not replace the output latch");
    assert_eq!(m.drive_gen(), generation, "external input changes do not change drive generation");
}

#[test]
fn high_bank_and_pin_order_store_mux_fields_and_matrix_inputs() {
    let mut p = Program::new();
    for gpio in [26, 32, 48] { p.gpio_setup(gpio, 0); }
    p.store(GPIO_OUT + 4, 1 << 26);
    p.store(GPIO_ENABLE + 4, 1 << 26);
    // facts.md §4a / TRM pp.502–505: bank 1 begins at GPIO32.
    p.store(GPIO_BASE + 0x10, 1 | (1 << 16));
    p.store(GPIO_BASE + 0x2c, 1 | (1 << 16));
    let high = p.pc();
    p.store(GPIO_BASE + 0x18, 1 << 16);
    p.store(GPIO_BASE + 0x30, 1 << 16);
    let low = p.pc();
    p.store(GPIO_BASE + 0x14, 1 << 16);
    p.store(GPIO_BASE + 0x34, 1);
    // facts-machine.md §6: SLP_OE/SEL/PD/IE/DRV and FUN_DRV are stored;
    // sleep configuration does not replace normal fields outside Light-sleep.
    let mux = MUX_GPIO | MUX_IE | (3 << 10) | 1 | 2 | 4 | 16 | (2 << 5);
    p.store(mux_reg(48), mux);
    p.pin_read(6, mux_reg(48));
    p.pin_read(7, GPIO_BASE + 0x10);
    p.pin_read(8, GPIO_BASE + 0x2c);
    // TRM p.507: 256 input routes. Consumers do not exist yet; store even
    // absent-pin routes without inventing their peripheral input values.
    p.store(GPIO_BASE + 0x154, 0xc0 | 22);
    p.store(GPIO_BASE + 0x550, 0x80 | 0x38);
    p.pin_read(9, GPIO_BASE + 0x154);
    p.pin_read(10, GPIO_BASE + 0x550);
    p.store(GPIO_BASE + 0x62c, 0);
    p.pin_read(11, GPIO_BASE + 0x62c);
    p.store(GPIO_BASE + 0x62c, 1);
    p.pin_read(12, GPIO_BASE + 0x62c);
    p.halt();
    let mut m = S3Machine::new(p.finish(), &profile(102));
    at_pc(&mut m, high);
    assert_eq!((m.drive(22), m.drive(28), m.drive(44)), (Drive::High, Drive::High, Drive::High),
        "GPIO26, GPIO32 and GPIO48 follow the machine pin order across holes and banks");
    at_pc(&mut m, low);
    assert_eq!(m.drive(44), Drive::Low, "OUT1_W1TC clears GPIO48 without changing GPIO32");
    assert_eq!(m.drive(28), Drive::High, "high-bank clear leaves unrelated output bits intact");
    assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Halted, "bank/storage program reaches BREAK");
    assert_eq!(m.drive(28), Drive::Input(Pull::None), "ENABLE1_W1TC releases GPIO32");
    assert_eq!(m.drive(44), Drive::High, "OUT1_W1TS restores GPIO48 despite stored sleep configuration");
    assert_eq!(m.cpu().ar[6] & 0xffff, mux, "IO_MUX normal and sleep fields round-trip");
    assert_eq!(m.cpu().ar[7] & 0x1ffff, 1 | (1 << 16), "OUT1 valid bits read their latches");
    assert_eq!(m.cpu().ar[8] & 0x1ffff, 1 << 16, "ENABLE1 readback follows its aliases");
    assert_eq!((m.cpu().ar[9] & 255, m.cpu().ar[10] & 255), (0xc0 | 22, 0x80 | 0x38),
        "first and last matrix input selectors retain all eight fields bits");
    assert_eq!((m.cpu().ar[11] & 1, m.cpu().ar[12] & 1), (0, 1), "CLOCK_GATE stores its force-running bit");
}

#[test]
fn input_enable_edges_and_cpu_interrupt_qualification() {
    let mut p = Program::new();
    p.gpio_setup(4, 0);
    p.pin_read(6, GPIO_IN);
    let disabled = p.pc();
    p.pin_read(7, GPIO_IN);
    p.store(mux_reg(4), MUX_GPIO | MUX_IE);
    // facts-machine.md §6 / TRM p.507: rising=1, CPU enable=13, NMI=14.
    p.store(pin_reg(4), (1 << 7) | (1 << 13));
    p.store(GPIO_STATUS, 0);
    let armed = p.pc();
    p.pin_read(8, GPIO_IN);
    p.pin_read(9, GPIO_STATUS);
    p.pin_read(10, GPIO_BASE + 0x5c);
    p.pin_read(11, GPIO_BASE + 0x60);
    let latched = p.pc();
    p.store(GPIO_STATUS + 8, 1 << 4);
    p.pin_read(12, GPIO_STATUS);
    p.store(GPIO_STATUS + 4, 1 << 4);
    p.store(pin_reg(4), (1 << 7) | (1 << 14));
    p.pin_read(13, GPIO_BASE + 0x5c);
    p.pin_read(14, GPIO_BASE + 0x60);
    p.halt();
    let mut m = S3Machine::new(p.finish(), &profile(103));
    m.set_input(4, false);
    at_pc(&mut m, disabled);
    m.set_input(4, true);
    at_pc(&mut m, armed);
    assert_eq!(m.cpu().ar[6] & 16, m.cpu().ar[7] & 16, "FUN_IE=0 reads stable junk rather than the changing input");
    m.set_input(4, false);
    m.set_input(4, true);
    at_pc(&mut m, latched);
    assert_eq!(m.cpu().ar[8] & 16, 16, "FUN_IE=1 makes the sampled input immediately visible");
    assert_eq!(m.cpu().ar[9] & 16, 16, "a rising edge latches raw STATUS at the input tick");
    assert_eq!(m.cpu().ar[10] & 16, 16, "CPU_INT qualifies raw STATUS with PINn bit 13");
    assert_eq!(m.cpu().ar[11] & 16, 0, "disabled NMI does not see the same raw status");
    m.set_input(4, false);
    assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Halted, "IRQ register program reaches BREAK");
    assert_eq!(m.cpu().ar[12] & 16, 0, "STATUS_W1TC clears the latched rising edge");
    assert_eq!(m.cpu().ar[13] & 16, 0, "disabling CPU qualification leaves raw status independent");
    assert_eq!(m.cpu().ar[14] & 16, 16, "STATUS_W1TS feeds NMI qualification through PINn bit 14");
}

#[test]
fn remaining_edge_and_level_types_in_both_banks() {
    let mut p = Program::new();
    let mut boundaries = Vec::new();
    // facts-machine.md §6 / TRM p.507: 2 falling, 3 either, 4 low, 5 high.
    for kind in [2, 3, 4, 5] {
        p.store(pin_reg(48), 0);
        p.store(GPIO_BASE + 0x50, 0);
        p.store(pin_reg(48), (kind << 7) | (1 << 13) | (1 << 14));
        let before = p.pc();
        p.pin_read(6, GPIO_BASE + 0x50);
        p.pin_read(7, GPIO_BASE + 0x68);
        p.pin_read(8, GPIO_BASE + 0x6c);
        let sampled = p.pc();
        p.store(GPIO_BASE + 0x58, 1 << 16);
        p.pin_read(9, GPIO_BASE + 0x50);
        boundaries.push((kind, before, sampled, p.pc()));
    }
    p.halt();
    let mut m = S3Machine::new(p.finish(), &profile(104));
    for (kind, before, sampled, cleared) in boundaries {
        at_pc(&mut m, before);
        if kind == 5 { m.set_input(44, false); m.set_input(44, true); }
        else { m.set_input(44, true); m.set_input(44, false); }
        at_pc(&mut m, sampled);
        for reg in [6, 7, 8] {
            assert_eq!(m.cpu().ar[reg] & (1 << 16), 1 << 16, "INT_TYPE {kind} latches GPIO48 in raw, CPU and NMI banks");
        }
        // Deactivate level to exercise a lasting clear; the active-level
        // reassertion assumption is covered separately.
        m.set_input(44, kind == 4);
        at_pc(&mut m, cleared);
        assert_eq!(m.cpu().ar[9] & (1 << 16), 0, "inactive level/edge STATUS1_W1TC clears GPIO48 for type {kind}");
    }
}

#[test]
fn handoff_mux_choices_and_undocumented_inputs_are_explicit_and_seeded() {
    let mut p = Program::new();
    p.pin_read(6, GPIO_IN);
    p.pin_read(7, GPIO_BASE + 0x40);
    p.pin_read(8, GPIO_STATUS);
    p.pin_read(9, GPIO_BASE + 0x154);
    p.pin_read(10, USB_BASE + 0x18);
    p.halt();
    let image = p.finish();
    let fixture = profile(105);
    let m = S3Machine::new(image.clone(), &fixture);
    for (pin, pull) in [(0, Pull::Up), (4, Pull::None), (22, Pull::Up), (35, Pull::Up), (41, Pull::Down), (42, Pull::Down)] {
        assert_eq!(m.drive(pin), Drive::Input(pull), "fixture handoff adopts cited per-pin pulls at index {pin}");
    }
    assert_eq!((fixture.pins.handoff_io_mux[19] >> 10 & 3, fixture.pins.handoff_io_mux[20] >> 10 & 3), (3, 3),
        "GPIO19/20 use the table's drive code 3, not the generic diagram's 2");
    let mut results = Vec::new();
    for (seed, level) in [(105, false), (105, true), (106, false)] {
        let mut board = profile(seed);
        // Explicitly disable every input; documented reset flags are not used
        // to pretend that disabled-input values or ROM state are zero.
        board.pins.handoff_io_mux = [MUX_GPIO; 45];
        let mut m = S3Machine::new(image.clone(), &board);
        for pin in 1..45 { m.set_input(pin, level); }
        assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Halted, "junk readback fixture reaches BREAK");
        results.push(m.cpu().ar[6..=10].to_vec());
    }
    assert_eq!(results[0], results[1], "same seed preserves disabled inputs and undocumented handoff words despite external levels");
    assert_ne!(results[0], results[2], "a different seed changes undocumented handoff state");
}

#[test]
fn usb_firmware_packets_backpressure_interrupts_and_rewind() {
    let mut p = Program::new();
    p.gpio_setup(4, 0);
    p.store(GPIO_ENABLE, 16);
    p.store(GPIO_OUT, 16);
    p.pin_read(6, USB_RAW);
    p.pin_read(7, USB_EP1_CONF);
    p.usb_tx_sequence(0xa0, 3);
    p.pin_read(8, USB_EP1_CONF);
    // facts-machine.md §7: clear IN_EMPTY once nonempty, then short flush.
    p.store(USB_CLR, 8);
    p.store(USB_EP1_CONF, 1);
    p.pin_read(9, USB_EP1_CONF);
    let pending = p.pc();
    p.store(GPIO_OUT + 8, 16);
    p.usb_wait_bit(1);
    p.pin_read(10, USB_RAW);
    p.pin_read(11, USB_EP1_CONF);
    let first_packet = p.pc();
    p.usb_rx_words(RAM, 64);
    p.pin_read(12, USB_EP1_CONF);
    p.usb_tx_sequence(0, 64); // TRM §33.3.3: no WR_DONE needed at 64 bytes.
    p.store(USB_RAW, 8); // RAW itself is WTC, not a normal writable latch.
    p.pin_read(13, USB_EP1_CONF);
    p.usb_wait_bit(2);
    p.pin_read(14, USB_RAW);
    p.usb_rx_words(RAM + 256, 16);
    p.pin_read(15, USB_EP1_CONF);
    p.halt();
    let flash = p.finish();
    // Preserve the original 1 ms case; a slower service with a phase beyond
    // the old fixed period proves both grid fields govern delivery/rewind.
    for (period, phase) in [(USB_FRAME, 40_000), (2 * USB_FRAME, USB_FRAME + 40_000)] {
        let mut board = profile(107);
        board.pins.usb_service_ticks = period;
        board.pins.usb_frame_phase_ticks = phase;
        let mut m = S3Machine::new(flash.clone(), &board);
        pins_at_pc(&mut m, pending);
        assert_eq!(m.cpu().ar[6] & 0xfff, 8, "profile explicitly selects hardware RAW reset 0x8 at handoff");
        assert_eq!(m.cpu().ar[7] & 6, 2, "empty handoff EP1 has FREE=1 and AVAIL=0");
        assert_eq!(m.cpu().ar[8] & 6, 2, "a short unflushed IN packet still has free room");
        assert_eq!(m.cpu().ar[9] & 6, 0, "WR_DONE consumes FREE before any host service");
        for byte in 0..80 { m.link_in(0, byte); }
        m.mark(); // Saves driven pin, flushed packet and all 80 queued host bytes.
        let generation = m.drive_gen();
        let finish = |m: &mut S3Machine| {
            pins_at_pc(m, first_packet);
            assert_eq!(m.drive(4), Drive::Low, "firmware after the mark changes the pin drive");
            assert_eq!(m.cpu().ar[10] & 12, 12, "host consumption and reception latch IN_EMPTY and OUT_RECV_PKT");
            assert_eq!(m.cpu().ar[11] & 6, 6, "first grid tick frees IN and delivers a firmware OUT packet");
            assert_eq!(run_to(m, 3 * period, &PcSet::default()), Ran::Halted, "byte-loop firmware drains both queued packets");
            assert_eq!(m.cpu().ar[12] & 6, 2, "draining 64 bytes clears AVAIL before the next packet's frame");
            assert_eq!(m.cpu().ar[13] & 6, 0, "64th transmit byte auto-flushes and consumes FREE");
            assert_eq!(m.cpu().ar[14] & 12, 12, "second service tick relatches both packet interrupt events");
            assert_eq!(m.cpu().ar[15] & 6, 2, "last OUT read drains AVAIL while the IN buffer is free");
            assert_eq!(received_words(m, RAM, 80), (0..80).collect::<Vec<u8>>(), "host queue preserves byte order across the 64-byte boundary");
            assert!(matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Break), "successful USB fixture stops at BREAK, not Unmodeled");
            let mut snapshot = m.clone();
            let mut out = Vec::new();
            snapshot.take_out(&mut out);
            snapshot.mark();
            out
        };
        let first = finish(&mut m);
        let expected: Vec<_> = (0xa0..0xa3).map(|byte| LinkOut { link: 0, tick: phase, byte })
            .chain((0..64).map(|byte| LinkOut { link: 0, tick: phase + period, byte })).collect();
        assert_eq!(first, expected, "WR_DONE and auto-flush emit ordered LinkOut bytes at exact grid ticks");
        m.rewind();
        assert_eq!(m.drive(4), Drive::High, "rewind restores the marked output latch");
        assert_eq!(m.drive_gen(), generation, "rewind restores drive generation");
        assert_eq!(finish(&mut m), first, "rewind restores packets, pending frame phase, host queue, output and SRAM replay");
    }
}

#[test]
fn usb_partial_packet_waits_for_flush_and_interrupt_enable_masks_raw() {
    let mut p = Program::new();
    p.store(USB_ENA, 0);
    p.pin_read(6, USB_ST);
    p.store(USB_ENA, 8);
    p.pin_read(7, USB_ST);
    p.store(USB_BASE, 0x5a);
    p.store(USB_CLR, 8);
    p.pin_read(8, USB_RAW);
    p.pin_read(9, USB_ST);
    // facts-machine.md §7 / usb_serial_jtag_reg.h:353–445: CONF0 storage,
    // including PHY_TX_EDGE_SEL[15], USB_PAD_ENABLE[14], DP_PULLUP[9].
    p.store(USB_BASE + 0x18, (1 << 15) | (1 << 14) | (1 << 9));
    p.pin_read(10, USB_BASE + 0x18);
    p.pin_read(11, USB_ENA);
    let waiting = p.pc();
    p.pins_spin();
    let mut m = S3Machine::new(p.finish(), &profile(108));
    pins_at_pc(&mut m, waiting);
    assert_eq!((m.cpu().ar[6] & 0xfff, m.cpu().ar[7] & 0xfff), (0, 8), "USB ST is RAW AND ENA, and reads do not clear RAW");
    assert_eq!((m.cpu().ar[8] & 0xfff, m.cpu().ar[9] & 0xfff), (0, 0), "INT_CLR clears the corresponding RAW and masked status");
    assert_eq!(m.cpu().ar[10] & 0x1ffff, (1 << 15) | (1 << 14) | (1 << 9), "CONF0 fields retain firmware writes");
    assert_eq!(m.cpu().ar[11] & 0xfff, 8, "USB ENA readback retains the interrupt mask");
    assert_eq!(run_to(&mut m, 2 * USB_FRAME, &PcSet::default()), Ran::Tick, "firmware can wait across several frame ticks");
    let mut out = Vec::new();
    m.take_out(&mut out);
    m.mark();
    assert!(out.is_empty(), "elapsed frames alone never flush a partial transmit packet");
}

#[test]
fn usb_out_backpressure_keeps_an_unread_packet_across_frame_ticks() {
    let mut p = Program::new();
    p.usb_wait_bit(2);
    p.usb_rx_words(RAM, 1);
    // Leave 63 bytes unread across the following frame. TRM §33.3.3 says
    // the host cannot deliver another packet until this one is drained.
    for _ in 0..6 { p.counted_loop(2047); }
    p.pin_read(6, USB_EP1_CONF);
    p.pin_read(7, USB_RAW);
    p.usb_rx_words(RAM + 4, 63);
    p.usb_wait_bit(2);
    p.usb_rx_words(RAM + 256, 16);
    p.halt();
    let mut board = profile(119);
    board.pins.usb_frame_phase_ticks = 10_000;
    let mut m = S3Machine::new(p.finish(), &board);
    for byte in 0..80 { m.link_in(0, byte); }
    assert_eq!(run_to(&mut m, 4 * USB_FRAME, &PcSet::default()), Ran::Halted,
        "firmware drains queued OUT data after waiting across a blocked frame");
    assert_eq!(m.cpu().ar[6] & 6, 6, "OUT AVAIL stays set while unread bytes apply host backpressure");
    assert_eq!(m.cpu().ar[7] & 4, 0, "a blocked frame cannot generate a new receive-packet interrupt");
    assert!(m.now() >= 10_000 + 2 * USB_FRAME, "the second packet waits for the grid after the final read");
    assert_eq!(received_words(&m, RAM, 80), (0..80).collect::<Vec<u8>>(),
        "unread OUT bytes are neither overwritten nor duplicated by later host frames");
}

#[test]
fn straps_latch_at_handoff_and_low_gpio0_selects_download() {
    let mut p = Program::new();
    p.pin_read(6, GPIO_BASE + 0x38);
    let sampled = p.pc();
    p.pin_read(7, GPIO_BASE + 0x38);
    // facts-machine.md §4, TRM pp.528,587: SW_SYS_RST[31] requests Core.
    p.store(0x6000_8000, 1 << 31);
    p.halt();
    let image = p.finish();
    let mut m = S3Machine::new(image.clone(), &profile(109));
    at_pc(&mut m, sampled);
    m.set_input(0, false);
    assert!(matches!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Reset { .. }),
        "software Core reset reaches the next strap handoff");
    assert_eq!(m.last_reset().map(|r| r.scope), Some(ResetScope::Core), "reset request has the digital-system scope");
    assert_stop(&mut m, ENTRY, "GPIO0 low at reset selects download boot, which is not modelled", None);
    let mut board = profile(109);
    board.pins.power_on_gpio0_high = false;
    let mut power_on = S3Machine::new(image, &board);
    assert_stop(&mut power_on, ENTRY, "GPIO0 low at reset selects download boot, which is not modelled", None);
    assert_eq!(power_on.cpu().instret, 0, "low power-on strap executes no image instruction");

    let mut p = Program::new();
    p.pin_read(6, GPIO_BASE + 0x38);
    let first = p.pc();
    p.pin_read(7, GPIO_BASE + 0x38);
    p.halt();
    let mut m = S3Machine::new(p.finish(), &profile(110));
    at_pc(&mut m, first);
    m.set_input(0, false);
    assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Halted, "ordinary strap-read firmware reaches BREAK");
    assert_eq!(m.cpu().ar[6] & 8, 8, "first handoff samples the profile's GPIO0 pull-up");
    assert_eq!(m.cpu().ar[6], m.cpu().ar[7], "STRAP remains latched when ordinary sampled inputs change");

    let mut p = Program::new();
    p.pin_read(6, GPIO_BASE + 0x38);
    let reset_pc = p.store(0x6000_8000, 1 << 31);
    let mut m = S3Machine::new(p.finish(), &profile(120));
    // facts.md §1 / TRM p.506: STRAP bits 2,3,4,5 map GPIO46,0,45,3.
    m.set_input(42, true); // GPIO46
    m.set_input(41, false); // GPIO45
    m.set_input(3, true);
    assert!(matches!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Reset { .. }),
        "high GPIO0 permits another flash handoff with changed auxiliary straps");
    at_pc(&mut m, reset_pc);
    assert_eq!(m.cpu().ar[6] & 0x3c, (1 << 2) | (1 << 3) | (1 << 5),
        "reset latches all four physical strap inputs into their documented bit positions");
}

#[test]
fn cpu_reset_retains_registers_and_packets_core_reset_restores_handoff() {
    // facts-machine.md §4 / TRM §7.1: CPU reset preserves digital blocks;
    // SW_PROCPU_RST[5] vs SW_SYS_RST[31], RTC_OPTIONS0 at RTC + 0.
    for (bit, scope) in [(5, ResetScope::Cpu), (31, ResetScope::Core)] {
        let mut p = Program::new();
        let entry_read = p.pin_read(6, GPIO_OUT);
        p.pin_read(7, USB_EP1_CONF);
        p.pin_read(8, USB_RAW);
        p.gpio_setup(4, 0);
        p.store(GPIO_ENABLE, 16);
        p.store(GPIO_OUT, 16);
        p.store(USB_BASE, 0x77);
        p.store(USB_EP1_CONF, 1);
        let prepared = p.pc();
        p.store(0x6000_8000, 1 << bit);
        let mut m = S3Machine::new(p.finish(), &profile(111));
        pins_at_pc(&mut m, prepared);
        m.link_in(0, 0x33);
        assert!(matches!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Reset { .. }), "software reset performs scope {scope:?}");
        assert_eq!(m.drive(4), if scope == ResetScope::Cpu { Drive::High } else { Drive::Input(Pull::None) },
            "reset scope controls pin retention");
        // Stop after the three reads at the re-entered image, before setup.
        pins_at_pc(&mut m, entry_read + 15);
        assert_eq!(m.cpu().ar[6] & 16, if scope == ResetScope::Cpu { 16 } else { 0 }, "OUT retention follows reset scope");
        assert_eq!(m.cpu().ar[7] & 2, if scope == ResetScope::Cpu { 0 } else { 2 }, "flushed IN packet survives CPU but not Core reset");
        assert_eq!(m.cpu().ar[8] & 0xfff, 8, "RAW uses retained or explicitly profiled state after reset");
    }
}

#[test]
fn host_bytes_waiting_behind_a_packet_survive_cpu_and_core_resets() {
    // TRM §7.1 controls device state, not the external host's queue. After
    // draining one packet, reset before the next host frame can deliver it.
    for bit in [5, 31] {
        let mut p = Program::new();
        p.usb_wait_bit(2);
        p.usb_rx_words(RAM, 64);
        let drained = p.pc();
        p.store(0x6000_8000, 1 << bit);
        let mut m = S3Machine::new(p.finish(), &profile(117));
        for byte in 0..128 { m.link_in(0, byte); }
        pins_at_pc(&mut m, drained);
        assert_eq!(received_words(&m, RAM, 64), (0..64).collect::<Vec<u8>>(),
            "first host packet drains before reset bit {bit}");
        assert!(matches!(run_to(&mut m, 2 * USB_FRAME, &PcSet::default()), Ran::Reset { .. }),
            "reset interrupts the wait for the next queued host packet");
        m.mark();
        pins_at_pc(&mut m, drained);
        assert_eq!(received_words(&m, RAM, 64), (64..128).collect::<Vec<u8>>(),
            "remaining host bytes survive reset bit {bit} without loss or duplication");
    }
}

#[test]
fn gpio_unsupported_accesses_stop_without_changing_drive() {
    let mut cases = vec![
        (mux_reg(4), Some(MUX_GPIO | MUX_UP | MUX_DOWN), "simultaneous IO_MUX pull-up and pull-down is undocumented"),
        (mux_reg(4), Some(MUX_GPIO | 12), "simultaneous IO_MUX pull-up and pull-down is undocumented"),
        (mux_reg(4), Some(0), "IO_MUX non-GPIO function on a driven pin is not modelled"),
        (mux_reg(4), Some(MUX_GPIO | MUX_IE | (1 << 15)), "IO_MUX input filter timing and observation point are undocumented"),
        (pin_reg(4), Some(6 << 7), "GPIO INT_TYPE 6 or 7 is undocumented"),
        (pin_reg(4), Some(7 << 7), "GPIO INT_TYPE 6 or 7 is undocumented"),
        (pin_reg(4), Some(1 << 15), "GPIO INT_ENA bits 17:15 are undocumented"),
        (out_sel_reg(4), Some(257), "GPIO output selector above 256 is undocumented"),
        (out_sel_reg(4), Some(511), "GPIO output selector above 256 is undocumented"),
        (out_sel_reg(4), Some(73), "GPIO output selector names an unmodelled peripheral signal"),
        (out_sel_reg(4), Some(0), "GPIO output selector names an unmodelled peripheral signal"),
    ];
    // facts-machine.md §6: WO reads, absent bits/pins and CPUSDIO are NOT
    // FOUND. The same policy covers OUT, ENABLE, STATUS and both banks.
    for base in [GPIO_OUT, GPIO_ENABLE, GPIO_STATUS] {
        for bank in [0, 12] {
            for alias in [4, 8] { cases.push((base + bank + alias, None, "GPIO write-only alias readback is undocumented")); }
            cases.push((base + bank, Some(if bank == 0 { 1 << 22 } else { 1 << 17 }), "GPIO bank write names an unimplemented pin"));
        }
    }
    for addr in [pin_reg(22), mux_reg(25), out_sel_reg(23)] {
        cases.push((addr, None, "GPIO register names an unimplemented pin"));
        cases.push((addr, Some(0), "GPIO register names an unimplemented pin"));
    }
    for offset in [0x64, 0x70] {
        cases.push((GPIO_BASE + offset, None, "GPIO CPUSDIO status behavior is undocumented"));
        cases.push((GPIO_BASE + offset, Some(0), "GPIO CPUSDIO status behavior is undocumented"));
    }
    for offset in [0x38, 0x3c, 0x40, 0x5c, 0x60, 0x68, 0x6c] {
        cases.push((GPIO_BASE + offset, Some(0), "write to read-only GPIO register"));
    }
    for (addr, value, what) in cases {
        let mut p = Program::new();
        let fault = if let Some(value) = value { p.store(addr, value) } else { p.pin_read(6, addr) };
        let mut board = profile(112);
        board.pins.handoff_enable[0] = 16;
        let mut m = S3Machine::new(p.finish(), &board);
        let drive = m.drive(4);
        assert_stop(&mut m, fault, what, Some(StopDetail {
            addr, width: Width::W, kind: if value.is_some() { Kind::Store } else { Kind::Load }, mmu_entry: None,
        }));
        assert_eq!(m.drive(4), drive, "unsupported access leaves the previously driven pin unchanged: {what}");
        assert_eq!(m.drive_gen(), 0, "unsupported write does not commit a drive generation: {what}");
    }
}

#[test]
fn gpio_active_level_clear_reasserts_and_same_tick_edge_clear_loses_the_edge() {
    for offset in [0, 8] {
        let mut p = Program::new();
        let addr = GPIO_STATUS + offset;
        p.store(addr, if offset == 0 { 0 } else { 16 });
        p.pin_read(6, GPIO_STATUS);
        p.halt();
        let mut board = profile(113);
        board.pins.handoff_io_mux[4] = MUX_GPIO | MUX_IE;
        board.pins.handoff_pin[4] = 5 << 7;
        let mut m = S3Machine::new(p.finish(), &board);
        m.set_input(4, true);
        assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Halted, "active-level clear allows firmware to finish");
        assert!(matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Break), "active-level clear reaches BREAK");
        assert_eq!(m.cpu().ar[6] & 16, 16, "active level reasserts STATUS before firmware's next read");
        let mut notes = Vec::new();
        m.take_notes(&mut notes);
        assert!(notes.len() == 1 && notes[0].what.contains("level is active"), "the race reaches the machine's notes once: {notes:?}");
    }
    for offset in [0, 8] {
        let mut p = Program::new();
        let clear = p.store(GPIO_STATUS + offset, if offset == 0 { 0 } else { 16 });
        p.pin_read(6, GPIO_STATUS);
        p.halt();
        let mut board = profile(118);
        board.pins.handoff_io_mux[4] = MUX_GPIO | MUX_IE;
        board.pins.handoff_pin[4] = 1 << 7;
        let mut m = S3Machine::new(p.finish(), &board);
        at_pc(&mut m, clear);
        m.set_input(4, false);
        m.set_input(4, true);
        assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Halted, "same-tick edge clear allows firmware to finish");
        assert!(matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Break), "same-tick edge clear reaches BREAK");
        assert_eq!(m.cpu().ar[6] & 16, 0, "the clear lands after the edge and loses its status bit");
        let mut notes = Vec::new();
        m.take_notes(&mut notes);
        assert!(notes.len() == 1 && notes[0].what.contains("tick of a new edge"), "the race reaches the machine's notes once: {notes:?}");
    }
}

#[test]
fn usb_unsourced_fifo_pointer_and_interrupt_accesses_stop() {
    for (addr, value, busy, what) in [
        (USB_BASE, None, false, "USB EP1 read with no received byte is undocumented"),
        (USB_BASE, Some(9), true, "USB EP1 write while SERIAL_IN_EP_DATA_FREE is 0 is undocumented"),
        (USB_EP1_CONF, Some(1), true, "USB WR_DONE while SERIAL_IN_EP_DATA_FREE is 0 is undocumented"),
        (USB_BASE + 0x2c, None, false, "USB EP1 pointer and state encodings are undocumented"),
        (USB_BASE + 0x3c, None, false, "USB EP1 pointer and state encodings are undocumented"),
        (USB_CLR, None, false, "USB INT_CLR write-only readback is undocumented"),
        (USB_ST, Some(0), false, "write to read-only USB register"),
        (USB_BASE + 0x2c, Some(0), false, "write to read-only USB register"),
        (USB_BASE + 0x3c, Some(0), false, "write to read-only USB register"),
        (USB_BASE + 0x1c, None, false, "USB-Serial-JTAG register is not modelled"),
        (USB_BASE + 0x1c, Some(0), false, "USB-Serial-JTAG register is not modelled"),
    ] {
        let mut p = Program::new();
        let fault = if let Some(value) = value { p.store(addr, value) } else { p.pin_read(6, addr) };
        let mut board = profile(114);
        if busy { board.pins.handoff_usb_tx = vec![1]; board.pins.handoff_usb_tx_flushed = true; }
        let mut m = S3Machine::new(p.finish(), &board);
        assert_stop(&mut m, fault, what, Some(StopDetail {
            addr, width: Width::W, kind: if value.is_some() { Kind::Store } else { Kind::Load }, mmu_entry: None,
        }));
    }
}

#[test]
fn usb_clear_races_continue_with_lost_events_and_reasserted_empty_status() {
    // Arrange a receive event at the start tick of the CLR instruction.
    // M: facts-machine.md §7 leaves clear/new-event priority open; clear wins.
    for clear in [USB_CLR, USB_RAW] {
        let mut p = Program::new();
        p.store(clear, 4);
        p.pin_read(6, USB_RAW);
        p.pin_read(7, USB_BASE);
        p.halt();
        let mut board = profile(115);
        board.pins.usb_frame_phase_ticks = 48;
        let mut m = S3Machine::new(p.finish(), &board);
        m.link_in(0, 7);
        assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Halted, "same-tick USB clear allows firmware to finish");
        assert!(matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Break), "same-tick USB clear reaches BREAK");
        assert_eq!(m.cpu().ar[6] & 4, 0, "clear loses the new receive interrupt");
        assert_eq!(m.cpu().ar[7] & 255, 7, "losing the interrupt leaves its byte readable");
    }
    for clear in [USB_CLR, USB_RAW] {
        let mut p = Program::new();
        p.store(clear, 8);
        p.pin_read(6, USB_RAW);
        p.halt();
        let mut m = S3Machine::new(p.finish(), &profile(121));
        assert_eq!(run_to(&mut m, 100_000, &PcSet::default()), Ran::Halted, "empty-source USB clear allows firmware to finish");
        assert!(matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Break), "empty-source USB clear reaches BREAK");
        assert_eq!(m.cpu().ar[6] & 8, 8, "empty FIFO reasserts IN_EMPTY before firmware's next read");
    }
}

#[test]
fn usb_final_read_with_receive_interrupt_pending_preserves_packet_order() {
    let mut p = Program::new();
    p.pin_read(6, USB_BASE);
    p.usb_wait_bit(2);
    p.usb_rx_words(RAM, 3);
    p.halt();
    let mut board = profile(122);
    board.pins.handoff_usb_rx = vec![2];
    board.pins.handoff_usb_int_raw = 12;
    let mut m = S3Machine::new(p.finish(), &board);
    for byte in [3, 4, 5] { m.link_in(0, byte); }
    assert_eq!(run_to(&mut m, 2 * USB_FRAME, &PcSet::default()), Ran::Halted, "pending receive interrupt allows the final read and following packet");
    assert!(matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Break), "pending-packet firmware reaches BREAK");
    assert_eq!(m.cpu().ar[6] & 255, 2, "the first read returns the old packet's last byte");
    assert_eq!(received_words(&m, RAM, 3), vec![3, 4, 5], "pending bytes follow in arrival order");
}

#[test]
#[should_panic(expected = "S3 USB-Serial-JTAG only provides link 0")]
fn other_link_numbers_are_rejected() {
    let mut p = Program::new();
    p.emit(enc::movi(3, 1));
    let mut m = S3Machine::new(p.finish(), &profile(116));
    m.link_in(1, 0);
}

use fab_elec_mcu_emit32::xtensa::enc;
use fab_elec_mcu_emu::board::{BusEvent, I2cWhat};
use fab_elec_mcu_emu::bus::{Kind, Width};
use fab_elec_mcu_emu::{Machine, PcSet, Pull, Ran, Status, Until};
use fab_elec_mcu_parts::{BoardDesc, NetDesc, Netlist, PartDesc, PartKind, PinRef};
use fab_elec_mcu_s3::{S3Machine, StopDetail};

mod support;
use support::*;
use support::i2c_fixture::*;

#[test]
fn i2c_refuses_nonpositive_data_setup_before_starting_a_list() {
    for low in [20, 19] {
        let mut p = Program::new();
        p.disable_watchdogs();
        p.i2c_setup();
        // The existing fixture programs HOLD=20. Both choices still satisfy
        // its I13 constraints, but I12's LOW-HOLD window is not positive.
        p.store(I2C0, low);
        p.store(I2C_CTR, I2C_MASTER | (1 << 11));
        let start = p.i2c_list(&[], &[I2C_RSTART, I2C_STOP]);
        p.pins_spin();
        let mut machine = S3Machine::new(p.finish(), &profile(201));
        assert_stop(&mut machine, start,
            "I2C data setup (SCL_LOW_PERIOD - SDA_HOLD_TIME) is not positive; behaviour undocumented (I12)",
            Some(StopDetail { addr: I2C_CTR, width: Width::W, kind: Kind::Store, mmu_entry: None }));
        let mut records = Vec::new();
        machine.take_bus(&mut records);
        assert!(records.is_empty(), "LOW={low}, HOLD=20: a nonpositive setup stops before any board operation");
    }
}

#[test]
fn i2c_master_writes_and_reads_the_tca9554_on_the_board() {
    let part = |pin: &str| PinRef::Part { part: 0, pin: pin.into() };
    let mut desc = BoardDesc {
        seed: 0,
        parts: vec![PartDesc { refdes: "U7".into(), kind: PartKind::Tca9554 { addr: 0x20 } }],
        nets: vec![
            NetDesc { rise_ps: None, name: "SCL".into(), pull: Pull::Up, pins: vec![PinRef::Chip("GPIO14".into()), part("SCL")] },
            NetDesc { rise_ps: None, name: "SDA".into(), pull: Pull::Up, pins: vec![PinRef::Chip("GPIO15".into()), part("SDA")] },
            NetDesc { rise_ps: None, name: "TP_RESET".into(), pull: Pull::None, pins: vec![part("P2")] },
        ],
    };
    for name in ["P0", "P1", "P3", "P4", "P5", "P6", "P7", "INT"] {
        desc.nets.push(NetDesc { rise_ps: None, name: name.into(), pull: Pull::Up, pins: vec![part(name)] });
    }
    let mut p = Program::new();
    p.disable_watchdogs();
    p.i2c_setup();
    let output = p.i2c_list(&[0x40, 0x01, 0xfb], &[I2C_RSTART, i2c_write(3), I2C_STOP]);
    p.i2c_wait();
    p.store(I2C_CLR, I2C_COMPLETE);
    let config = p.i2c_list(&[0x40, 0x03, 0xfb], &[I2C_RSTART, i2c_write(3), I2C_STOP]);
    p.i2c_wait();
    p.store(I2C_CLR, I2C_COMPLETE);
    let read = p.i2c_list(&[0x40, 0x03, 0x41], &[
        I2C_RSTART, i2c_write(2), I2C_RSTART, i2c_write(1), I2C_READ_NACK, I2C_STOP,
    ]);
    p.i2c_wait();
    p.pin_read(5, I2C_DATA);
    // I06: DATA defines only the low byte; reserved upper bits remain junk.
    p.emit(enc::extui(5, 5, 0, 8));
    p.load(3, RAM);
    p.emit(enc::s32i(5, 3, 0));
    p.store(I2C_CLR, I2C_COMPLETE);
    let nack = p.i2c_list(&[0x42], &[I2C_RSTART, i2c_write(1), I2C_STOP]);
    p.i2c_wait();
    p.pin_read(5, I2C_STATUS);
    p.load(3, RAM);
    p.emit(enc::s32i(5, 3, 4));
    let spin = p.pins_spin();
    let image = p.finish();
    let handoff = profile(201);
    let fresh = || S3Machine::with_board(
        image.clone(), &handoff,
        Netlist::new(desc.clone()).expect("U7 and its fully resolved port form a valid board"),
    ).expect("the I2C board binds and accepts S3 handoff drives");
    let mut marks = PcSet::new(ENTRY, spin - ENTRY + 3);
    for pc in [output, config, read, nack, spin] { marks.insert(pc); }
    let run = |m: &mut S3Machine<Netlist>, tick| m.run(&Until {
        tick, marks: &marks, stops: &[spin], stops_after: 0, on_drive: false,
    });
    let words = |m: &S3Machine<Netlist>| {
        let mut bytes = [0; 8];
        assert!(m.read_data(RAM, &mut bytes), "both firmware result words are readable SRAM");
        [u32::from_le_bytes(bytes[..4].try_into().unwrap()), u32::from_le_bytes(bytes[4..].try_into().unwrap())]
    };

    let mut straight = fresh();
    assert_eq!(run(&mut straight, 1_000_000), Ran::Pc, "all four I2C lists complete and firmware reaches its spin");
    assert_eq!(straight.status(), Status::Running, "the master FIFO path does not latch an Unmodeled halt");
    let result = words(&straight);
    assert_eq!(result[0], 0xfb, "Configuration reads back through the chip RX FIFO into RAM word 0");
    assert_eq!(result[1] & (I2C_NACK | I2C_COMPLETE), I2C_NACK | I2C_COMPLETE,
        "the absent address latches NACK and the generated STOP latches TRANS_COMPLETE in RAM word 1");
    let mut trace = Vec::new();
    straight.take_trace(&mut trace);
    assert_eq!(trace.len(), 4, "each TRANS_START store is traced once before the final spin");
    assert_eq!(trace[0], (access_tick(output), output), "the first TRANS_START tick follows the straight-line setup instructions");
    assert_eq!(trace.iter().map(|&(_, pc)| pc).collect::<Vec<_>>(), [output, config, read, nack],
        "all four list start ticks belong to their firmware stores");
    // Independent I12 duration calculation from the fixture's programmed fields.
    let t = 12 * u64::from(I2C_DIV + 1);
    let start_ticks = u64::from(I2C_RSTART_SETUP + 1 + I2C_START_HOLD + 1) * t;
    let byte_ticks = 9 * u64::from(I2C_LOW + I2C_HIGH + I2C_WAIT_HIGH + 3) * t;
    let stop_ticks = u64::from(I2C_STOP_SETUP + 1 + I2C_STOP_HOLD + 1) * t;
    let mut straight_bus = Vec::new();
    straight.take_bus(&mut straight_bus);
    let transfers: Vec<_> = straight_bus.iter().filter_map(|event| match *event {
        BusEvent::I2c { t, bus, what } => Some((t, bus, what)),
        _ => None,
    }).collect();
    assert_eq!(transfers.len(), 20, "the four lists emit exactly twenty board I2C operations");
    let first = trace[0].0 + start_ticks;
    assert_eq!(&transfers[..5], &[
        (first, 0, I2cWhat::Start),
        (first + byte_ticks, 0, I2cWhat::Byte { value: 0x40, ack: true }),
        (first + 2 * byte_ticks, 0, I2cWhat::Byte { value: 0x01, ack: true }),
        (first + 3 * byte_ticks, 0, I2cWhat::Byte { value: 0xfb, ack: true }),
        (first + 3 * byte_ticks + stop_ticks, 0, I2cWhat::Stop),
    ], "the first write reaches U7 at exact command and byte end ticks computed from TRANS_START");
    let config_byte = trace[1].0 + start_ticks + 3 * byte_ticks;
    assert_eq!(transfers[8], (config_byte, 0, I2cWhat::Byte { value: 0xfb, ack: true }),
        "the Configuration byte is ACKed at the third byte end of the second list");
    let reset_net = straight.board().net("TP_RESET").expect("the board exposes TP_RESET");
    let falls: Vec<_> = straight_bus.iter().filter_map(|event| match *event {
        BusEvent::Level { t, net, high: false } if net == reset_net => Some(t),
        _ => None,
    }).collect();
    let output_delay = (350 * TICK_HZ + 999_999_999) / 1_000_000_000;
    assert_eq!(output_delay, 168, "TCA9554's 350 ns maximum converts to 168 ticks at 480 MHz");
    assert_eq!(falls, [config_byte + output_delay], "TP_RESET falls once, exactly 168 ticks after the Configuration ACK");
    let last = trace[3].0 + start_ticks;
    assert_eq!(&transfers[17..], &[
        (last, 0, I2cWhat::Start),
        (last + byte_ticks, 0, I2cWhat::Byte { value: 0x42, ack: false }),
        (last + byte_ticks + stop_ticks, 0, I2cWhat::Stop),
    ], "address 0x21 has no responder and generates exactly one timed STOP after its NACK");
    assert!(!straight_bus.iter().any(|event| matches!(event, BusEvent::Note { what, .. } if what.contains("no driver"))),
        "all U7 port inputs and INT are resolved without floating-net notes");

    let mut split = fresh();
    let mut split_bus = Vec::new();
    let piece_ticks = byte_ticks / 32;
    assert!(piece_ticks > 24 && piece_ticks < byte_ticks / 2, "replay pieces put many marks inside each byte");
    while split.now() < straight.now() {
        split.take_bus(&mut split_bus);
        split.mark();
        let start = split.now();
        let deadline = (start + piece_ticks).min(straight.now());
        let ahead = run(&mut split, deadline);
        let (tick, status) = (split.now(), split.status());
        assert!(matches!(ahead, Ran::Tick | Ran::Pc), "each piece reaches its deadline or the final spin");
        assert!(tick > start, "every short replay piece advances firmware execution");
        split.rewind();
        assert_eq!(run(&mut split, deadline), ahead, "rewind preserves each piece's stop reason");
        assert_eq!((split.now(), split.status()), (tick, status), "rewind preserves each piece's tick and status");
    }
    split.take_bus(&mut split_bus);
    split.mark();
    assert_eq!(split_bus, straight_bus, "split replay preserves every byte, level and board note without loss or duplication");
    assert_eq!(words(&split), result, "split replay preserves all bits of both RAM words");
    assert_eq!((split.now(), split.pc(), split.status()), (straight.now(), straight.pc(), straight.status()),
        "split replay reaches the identical final machine boundary");
    let (mut straight_regs, mut split_regs) = (Vec::new(), Vec::new());
    straight.regs(&mut straight_regs);
    split.regs(&mut split_regs);
    assert_eq!(split_regs, straight_regs, "split replay preserves the complete exposed CPU register state");
    for net in 0..straight.nets().len() {
        assert_eq!(split.board().level(net as u16), straight.board().level(net as u16),
            "split replay preserves final board net {net}");
    }
    let (mut straight_notes, mut split_notes) = (Vec::new(), Vec::new());
    straight.take_notes(&mut straight_notes);
    split.take_notes(&mut split_notes);
    assert_eq!(split_notes, straight_notes, "split replay preserves controller model-choice notes exactly once");
    let mut split_trace = Vec::new();
    split.take_trace(&mut split_trace);
    assert_eq!(split_trace, trace, "split replay preserves all TRANS_START instruction ticks");
}

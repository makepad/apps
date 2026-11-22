use fab_elec_mcu_emu::board::{fnv1a, Board, BusEvent, I2cAns, I2cLine, I2cOp, I2cTiming, NetKind, Payload, Res, SpiXfer};
use fab_elec_mcu_emu::{Drive, Pull};
use fab_elec_mcu_parts::{BoardDesc, NetDesc, Netlist, Part, PartDesc, PartKind, PinRef, Stimulus};

fn pin(part: u16, pin: &'static str) -> PinRef {
    PinRef::Part { part, pin: pin.into() }
}

fn net(name: &'static str, pull: Pull, pins: Vec<PinRef>) -> NetDesc {
    NetDesc { rise_ps: None, name: name.into(), pull, pins }
}

#[test]
fn i2c_rise_windows_round_up_stop_and_retain_records() {
    const STOP: &str = "an I2C line rises slower than the controller's window; see the rise record";
    const UNKNOWN: &str = "I2C rise time not checked: a bus line's pull-up, pin capacitance or input threshold is unknown";
    const ASSUMED: &str = "I2C rise check: pin capacitance only (no trace capacitance); the controller's input threshold (targets' thresholds and data-setup times are not in part data)";
    let mut desc = BoardDesc {
        seed: 0,
        parts: vec![PartDesc { refdes: "U1".into(), kind: PartKind::Tca9554 { addr: 0x20 } }],
        nets: vec![
            net("SCL", Pull::Up, vec![PinRef::Chip("CLK".into()), pin(0, "SCL")]),
            net("SDA", Pull::Up, vec![PinRef::Chip("DATA".into()), pin(0, "SDA")]),
        ],
    };
    let timing = I2cTiming { setup: 432, allowance: 144 };
    for (property, scl, sda, failure) in [
        ("SCL first when both lines are slow", Some(300_001), Some(900_001), Some((0, I2cLine::Scl, 145, 144))),
        ("SDA uses setup", Some(100_000), Some(900_001), Some((1, I2cLine::Sda, 433, 432))),
        ("faster pull passes", Some(100_000), Some(100_000), None),
        ("equality passes", Some(300_000), Some(900_000), None),
        ("unknown rises pass", None, None, None),
        ("unknown SDA leaves SCL checked", Some(300_001), None, Some((0, I2cLine::Scl, 145, 144))),
        ("unknown SCL leaves SDA checked", None, Some(900_001), Some((1, I2cLine::Sda, 433, 432))),
        ("one unknown line passes with a fast peer", None, Some(100_000), None),
    ] {
        desc.nets[0].rise_ps = scl;
        desc.nets[1].rise_ps = sda;
        let mut board = Netlist::new(desc.clone()).expect("rise-check fixture is a valid bus");
        // Pad order deliberately differs from net order; records must name nets.
        board.bind(&["DATA", "CLK", "UNWIRED"], 480_000_000).expect("rise-check pins bind at 480 MHz");
        let mut records = Vec::new();
        board.take_bus(&mut records);
        records.clear();
        assert_eq!(board.i2c_timing(5, 2, 0, timing), Ok(()), "{property}: a non-bus pair defers to i2c");
        assert_eq!(board.i2c(5, 2, 0, I2cOp::Start), Err("I2C controller on a net without a bus"),
            "{property}: the first operation keeps the existing non-bus refusal");
        board.take_bus(&mut records);
        assert!(records.is_empty(), "{property}: a non-bus pair produces no rise notes or records");
        board.mark();
        let expected = failure.map_or(Ok(()), |_| Err(STOP));
        assert_eq!(board.i2c_timing(10, 1, 0, timing), expected, "{property}: rise windows decide the outcome");
        board.take_bus(&mut records);
        let rises: Vec<_> = records.iter().copied().filter(|e| matches!(e, BusEvent::Rise { .. })).collect();
        let expected_rises: Vec<_> = failure.into_iter().map(|(net, line, rise_ticks, window_ticks)| {
            BusEvent::Rise { t: 10, net, line, rise_ticks, window_ticks }
        }).collect();
        assert_eq!(rises, expected_rises, "{property}: the stop retains exactly one Rise with upward-rounded ticks and the net index");
        board.rewind();
        assert_eq!(board.i2c_timing(10, 1, 0, timing), expected, "{property}: rewind repeats the same decision");
        let mut replay = Vec::new();
        board.take_bus(&mut replay);
        assert_eq!(replay, records, "{property}: rewind reproduces the rise record and assumption notes");
        board.mark();
        assert_eq!(board.i2c_timing(20, 1, 0, I2cTiming { setup: 433, allowance: 145 }), Ok(()),
            "{property}: a later list checks its own windows");
        board.take_bus(&mut records);
        let notes = |what| records.iter().filter(|e| matches!(e, BusEvent::Note { what: w, .. } if *w == what)).count();
        assert_eq!(notes(UNKNOWN), usize::from(scl.is_none() || sda.is_none()),
            "{property}: unknown rise is noted once per board across lists and drains");
        assert_eq!(notes(ASSUMED), usize::from(scl.is_some() || sda.is_some()),
            "{property}: pin-only capacitance and controller-only threshold are noted once when checked");
    }
}

fn step(board: &mut Netlist, t: &mut u64, op: I2cOp, answers: &mut Vec<I2cAns>) -> I2cAns {
    let answer = board.i2c(*t, 0, 1, op).expect("documented I2C framing must succeed");
    *t += 1;
    answers.push(answer);
    answer
}

fn expect_step(
    board: &mut Netlist,
    t: &mut u64,
    op: I2cOp,
    expected: I2cAns,
    answers: &mut Vec<I2cAns>,
) {
    assert_eq!(step(board, t, op, answers), expected, "I2C step {op:?} must answer as documented");
}

fn write(board: &mut Netlist, t: &mut u64, pointer: u8, value: u8, answers: &mut Vec<I2cAns>) -> u64 {
    expect_step(board, t, I2cOp::Start, I2cAns::None, answers);
    expect_step(board, t, I2cOp::Write(0x40), I2cAns::Ack(true), answers);
    expect_step(board, t, I2cOp::Write(pointer), I2cAns::Ack(true), answers);
    let ack_tick = *t;
    expect_step(board, t, I2cOp::Write(value), I2cAns::Ack(true), answers);
    expect_step(board, t, I2cOp::Stop, I2cAns::None, answers);
    ack_tick
}

fn begin_read(board: &mut Netlist, t: &mut u64, pointer: u8, answers: &mut Vec<I2cAns>) {
    expect_step(board, t, I2cOp::Start, I2cAns::None, answers);
    expect_step(board, t, I2cOp::Write(0x40), I2cAns::Ack(true), answers);
    expect_step(board, t, I2cOp::Write(pointer), I2cAns::Ack(true), answers);
    expect_step(board, t, I2cOp::Start, I2cAns::None, answers);
    expect_step(board, t, I2cOp::Write(0x41), I2cAns::Ack(true), answers);
}

fn read(board: &mut Netlist, t: &mut u64, pointer: u8, answers: &mut Vec<I2cAns>) -> u8 {
    begin_read(board, t, pointer, answers);
    let I2cAns::Byte(value) = step(board, t, I2cOp::Read { ack: false }, answers) else {
        panic!("a selected TCA9554 read must return a byte");
    };
    expect_step(board, t, I2cOp::Stop, I2cAns::None, answers);
    value
}

#[derive(Debug, PartialEq, Eq)]
struct Recording {
    answers: Vec<I2cAns>,
    bus: Vec<BusEvent>,
    levels: Vec<bool>,
    inputs: Vec<(u16, bool)>,
    next_event: Option<u64>,
}

fn replay(board: &mut Netlist, start: u64, tp_reset: u16) -> Recording {
    let mut t = start;
    let mut answers = Vec::new();
    board.input(t, Stimulus::Key { part: 1, down: true }).expect("replayed key press must succeed");
    t += 1;
    let value = read(board, &mut t, 0, &mut answers);
    assert_eq!(value & 0x14, 0x10, "replayed read must see SYS_OUT high and TP_RESET low");
    let ack_tick = write(board, &mut t, 1, 0xff, &mut answers);
    let due = ack_tick + 168;
    assert_eq!(board.next_event(), Some(due), "replayed output event must retain its exact deadline");
    board.event(due).expect("replayed output event must succeed");
    assert!(board.level(tp_reset), "replayed output write must raise TP_RESET");
    let mut bus = Vec::new();
    let mut inputs = Vec::new();
    board.take_bus(&mut bus);
    board.pad_inputs(&mut inputs);
    Recording {
        answers,
        bus,
        inputs,
        levels: (0..board.nets().len()).map(|net| board.level(net as u16)).collect(),
        next_event: board.next_event(),
    }
}

#[test]
fn tca9554_board_without_a_chip() {
    let mut board = Netlist::new(BoardDesc {
        seed: 0,
        parts: vec![
            PartDesc { refdes: "U7".into(), kind: PartKind::Tca9554 { addr: 0x20 } },
            PartDesc { refdes: "Key3".into(), kind: PartKind::Key },
            PartDesc { refdes: "T1".into(), kind: PartKind::Not },
        ],
        nets: vec![
            net("ESP32_SCL", Pull::Up, vec![PinRef::Chip("GPIO14".into()), pin(0, "SCL")]),
            net("ESP32_SDA", Pull::Up, vec![PinRef::Chip("GPIO15".into()), pin(0, "SDA")]),
            net("LCD_RESET", Pull::None, vec![pin(0, "P0")]),
            net("DSI_PWR_EN", Pull::None, vec![pin(0, "P1")]),
            net("TP_RESET", Pull::None, vec![pin(0, "P2")]),
            net("RTC_INT", Pull::None, vec![pin(0, "P3")]),
            net("SYS_OUT", Pull::Up, vec![pin(0, "P4"), pin(2, "D")]),
            net("AXP_IRQ", Pull::None, vec![pin(0, "P5")]),
            net("QMI_INT1", Pull::None, vec![pin(0, "P6")]),
            net("SDCS", Pull::None, vec![pin(0, "P7")]),
            net("TCA_INT", Pull::Up, vec![pin(0, "INT")]),
            net("PWRON", Pull::Up, vec![pin(1, "A"), pin(2, "G")]),
        ],
    }).expect("the voicebox subset must be a valid netlist");
    board.bind(&["GPIO14", "GPIO15"], 480_000_000).expect("the two chip pads must bind");
    let tp_reset = board.net("TP_RESET").expect("TP_RESET must be named");
    let sys_out = board.net("SYS_OUT").expect("SYS_OUT must be named");
    let pwron = board.net("PWRON").expect("PWRON must be named");
    let int = board.net("TCA_INT").expect("TCA_INT must be named");
    assert!(board.level(tp_reset), "E05: power-up inputs must pull TP_RESET high");
    assert!(!board.level(sys_out), "the idle-high PWRON gate must make T1 sink SYS_OUT");

    let mut answers = Vec::new();
    let mut t = 1;
    expect_step(&mut board, &mut t, I2cOp::Start, I2cAns::None, &mut answers);
    expect_step(&mut board, &mut t, I2cOp::Write(0x42), I2cAns::Ack(false), &mut answers);
    expect_step(&mut board, &mut t, I2cOp::Stop, I2cAns::None, &mut answers);
    expect_step(&mut board, &mut t, I2cOp::Start, I2cAns::None, &mut answers);
    expect_step(&mut board, &mut t, I2cOp::Write(0x43), I2cAns::Ack(false), &mut answers);
    expect_step(&mut board, &mut t, I2cOp::Read { ack: false }, I2cAns::Byte(0xff), &mut answers);
    expect_step(&mut board, &mut t, I2cOp::Stop, I2cAns::None, &mut answers);
    expect_step(&mut board, &mut t, I2cOp::Start, I2cAns::None, &mut answers);
    assert_eq!(
        board.i2c(t, 0, 1, I2cOp::Write(0x41)),
        Err("TCA9554: register access before its pointer is set; the pointer's power-up value is undocumented"),
        "E10: a read before a command must stop on the poisoned pointer",
    );
    t += 1;
    expect_step(&mut board, &mut t, I2cOp::Stop, I2cAns::None, &mut answers);

    write(&mut board, &mut t, 1, 0xfb, &mut answers);
    assert_eq!(board.next_event(), None, "E03: an input pin must not drive its output latch");
    let config_ack = write(&mut board, &mut t, 3, 0xfb, &mut answers);
    let due = config_ack + 168;
    assert!(board.level(tp_reset), "E08: configuration must not drive TP_RESET before its delay");
    assert_eq!(board.next_event(), Some(due), "E08: 350 ns must be exactly 168 ticks");
    board.event(due - 1).expect("an event call before the deadline must be harmless");
    assert!(board.level(tp_reset), "E08: TP_RESET must remain high one tick before output valid");
    board.event(due).expect("the delayed output must settle");
    assert!(!board.level(tp_reset), "E03/E05: TP_RESET must fall at the output deadline");
    let mut bus = Vec::new();
    board.take_bus(&mut bus);
    assert!(bus.contains(&BusEvent::Level { t: due, net: tp_reset, high: false }),
        "the bus trace must contain TP_RESET's edge at its exact deadline");

    let press = due + 10;
    board.input(press, Stimulus::Key { part: 1, down: true }).expect("Key3 must accept a press");
    assert!(!board.level(pwron), "pressing Key3 must ground PWRON");
    assert!(board.level(sys_out), "PWRON must turn T1 off and let SYS_OUT pull high");
    assert!(board.level(int), "E08: INT must remain released before its delay");
    assert_eq!(board.next_event(), Some(press + 1920), "E08: INT valid must wait exactly 4 us");
    board.event(press + 1920).expect("the input transition must assert INT");
    assert!(!board.level(int), "E08: INT must sink at its 4 us deadline");
    t = press + 1921;
    let input_ack = t + 4;
    let value = read(&mut board, &mut t, 0, &mut answers);
    assert_eq!(value & 0x14, 0x10, "E02: Input Port must report SYS_OUT high and TP_RESET low");
    assert_eq!(board.next_event(), Some(input_ack + 1920), "E07/E08: Input Port's address ACK starts INT reset");
    assert!(!board.level(int), "E08: the read must not release INT before its delay");
    board.event(input_ack + 1920).expect("the input sample must release INT");
    assert!(board.level(int), "E08: INT must release at its reset deadline");
    t = input_ack + 1921;
    board.input(t, Stimulus::Key { part: 1, down: false }).expect("Key3 must accept a release");
    t += 1;
    assert!(!board.level(sys_out), "releasing Key3 must turn T1 on again");
    let value = read(&mut board, &mut t, 0, &mut answers);
    assert_eq!(value & 0x10, 0, "E02: Input Port must report SYS_OUT low after release");

    begin_read(&mut board, &mut t, 0, &mut answers);
    board.input(t, Stimulus::Key { part: 1, down: true }).expect("sampling check must press Key3");
    t += 1;
    let I2cAns::Byte(first) = step(&mut board, &mut t, I2cOp::Read { ack: true }, &mut answers) else {
        panic!("E07: the first sampled read must return a byte");
    };
    assert_eq!(first & 0x10, 0, "E07: the first byte must use the preceding address ACK's levels");
    board.input(t, Stimulus::Key { part: 1, down: false }).expect("sampling check must release Key3");
    t += 1;
    let I2cAns::Byte(second) = step(&mut board, &mut t, I2cOp::Read { ack: false }, &mut answers) else {
        panic!("E07: the next sampled read must return a byte");
    };
    assert_eq!(second & 0x10, 0x10, "E07: the next byte must use the preceding master ACK's levels");
    expect_step(&mut board, &mut t, I2cOp::Stop, I2cAns::None, &mut answers);

    board.take_bus(&mut bus);
    let noted = |what: &str| bus.iter().filter(|event| matches!(event, BusEvent::Note { what: w, .. } if *w == what)).count();
    assert_eq!(noted("TCA9554 output valid at its 350 ns maximum after the ACK"), 1,
        "the adverse output timing choice must be noted only once, in the bus records");
    assert_eq!(noted("TCA9554 interrupt valid and reset at their 4 us maximum"), 1,
        "the adverse interrupt timing choice must be noted only once, in the bus records");
    board.mark();
    let first = replay(&mut board, t + 10, tp_reset);
    board.rewind();
    let second = replay(&mut board, t + 10, tp_reset);
    assert_eq!(first, second, "rewind must reproduce every answer, bus record and note, net level, input and deadline");

    t += 500;
    expect_step(&mut board, &mut t, I2cOp::Start, I2cAns::None, &mut answers);
    expect_step(&mut board, &mut t, I2cOp::Write(0x40), I2cAns::Ack(true), &mut answers);
    assert_eq!(board.i2c(t, 0, 1, I2cOp::Write(0x04)),
        Err("TCA9554: command byte outside its four registers; undocumented"),
        "O02: an undocumented command byte must stop instead of choosing a register");

    let mut ring = Netlist::new(BoardDesc {
        seed: 0,
        parts: vec![PartDesc { refdes: "T2".into(), kind: PartKind::Not }],
        nets: vec![net("RING", Pull::Up, vec![pin(0, "G"), pin(0, "D")])],
    }).expect("a switch whose drain drives its own gate is a valid netlist");
    assert_eq!(ring.bind(&[], 480_000_000), Err("a zero-delay loop on the board's nets oscillates".into()),
        "a net that keeps changing must stop instead of looping or guessing a level");
}

#[test]
fn co5300_panel_without_a_chip() {
    struct Bytes<'a>(&'a [u8]);

    impl Payload for Bytes<'_> {
        fn len(&self) -> usize {
            self.0.len()
        }

        fn chunks(&self, f: &mut dyn FnMut(&[u8])) {
            // Deliberately split a pixel: payload chunk boundaries are not SPI boundaries.
            let split = self.0.len().min(3);
            f(&self.0[..split]);
            f(&self.0[split..]);
        }
    }

    fn frame(board: &Netlist) -> &[u16] {
        let Some(Part::Co5300(panel)) = board.part(0) else {
            panic!("the board's U1 must be a CO5300");
        };
        panel.frame()
    }

    fn xfer(t0: u64, cmd: u8, len: usize) -> SpiXfer {
        SpiXfer {
            t0, t1: t0 + (32 + len as u64 * 8) * 10,
            sck: 1, d: [Some(2), Some(3), Some(4), Some(5)],
            bit_ticks: 10, mode: 0, lsb_first: false,
            cmd: (8, 0x02), addr: (24, u32::from(cmd) << 8), dummy: 0,
            lines: [1, 1, 1], tx_bits: len as u32 * 8, rx_bits: 0, full_duplex: false,
        }
    }

    fn transfer(board: &mut Netlist, x: &SpiXfer, bytes: &[u8], setup: u64) -> Res {
        board.pad_drive(x.t0 - setup, 0, Drive::Low)
            .expect("the host must be able to select U1");
        let mut rx = vec![0xff];
        let result = board.spi(x, &Bytes(bytes), &mut rx);
        assert!(rx.is_empty(), "the write-only panel must return no receive bytes");
        board.pad_drive(x.t1 + 5, 0, Drive::High)
            .expect("P03: five ticks must satisfy CS hold at 480 MHz");
        result
    }

    fn command(board: &mut Netlist, t: &mut u64, cmd: u8, bytes: &[u8]) -> SpiXfer {
        let x = xfer(*t, cmd, bytes.len());
        assert_eq!(transfer(board, &x, bytes, 5), Ok(()),
            "documented CMD {cmd:#04x} must accept its parameters and timing");
        *t = x.t1 + 20;
        x
    }

    const US: u64 = 480;
    const MS: u64 = 480_000;
    let mut board = Netlist::new(BoardDesc {
        seed: 0,
        parts: vec![PartDesc { refdes: "U1".into(), kind: PartKind::Co5300 { glass: [0, 0, 480, 480] } }],
        nets: vec![
            net("LCD_CS", Pull::Up, vec![PinRef::Chip("GPIO12".into()), pin(0, "CSX")]),
            net("LCD_SCLK", Pull::None, vec![PinRef::Chip("GPIO11".into()), pin(0, "SCLK")]),
            net("LCD_SIO0", Pull::None, vec![PinRef::Chip("GPIO4".into()), pin(0, "SIO0")]),
            net("LCD_SIO1", Pull::None, vec![PinRef::Chip("GPIO5".into()), pin(0, "SIO1")]),
            net("LCD_SIO2", Pull::None, vec![PinRef::Chip("GPIO6".into()), pin(0, "SIO2")]),
            net("LCD_SIO3", Pull::None, vec![PinRef::Chip("GPIO7".into()), pin(0, "SIO3")]),
            net("LCD_RESET", Pull::Up, vec![PinRef::Chip("GPIO8".into()), pin(0, "RESX")]),
        ],
    }).expect("the CO5300 board must have valid SPI nets");
    board.bind(&["GPIO12", "GPIO11", "GPIO4", "GPIO5", "GPIO6", "GPIO7", "GPIO8"], 480_000_000)
        .expect("the seven panel pads must bind at 480 MHz");
    assert_eq!(board.nets()[1].kind, NetKind::SpiSck { bus: 0 },
        "the SCLK net must identify the inferred SPI bus");
    for line in 0..4 {
        assert_eq!(board.nets()[line + 2].kind, NetKind::SpiData { bus: 0, line: line as u8 },
            "each SIO net must retain its bus and lane number");
    }
    board.mark();

    let before_reset = "CO5300 command before a hardware reset: the power-on sequence is not modelled (P07)";
    assert_eq!(transfer(&mut board, &xfer(US, 0x20, 0), &[], 5), Err(before_reset),
        "P07: a command before any hardware reset must stop");
    board.pad_drive(2 * US, 6, Drive::Low).expect("RESX must accept the rejected pulse's falling edge");
    board.pad_drive(5 * US, 6, Drive::High).expect("P06: a 3 us RESX pulse must be ignored");
    assert_eq!(transfer(&mut board, &xfer(6 * US, 0x20, 0), &[], 5), Err(before_reset),
        "P06/P07: a rejected 3 us pulse must not establish hardware reset");

    board.rewind();
    board.pad_drive(US, 6, Drive::Low).expect("the fresh board must accept RESX falling");
    assert_eq!(board.pad_drive(8 * US, 6, Drive::High),
        Err("CO5300 RESX pulse of 5-10 us is a 'Reset Start' with no documented outcome (P06)"),
        "P06: a 7 us pulse on the fresh board must stop at Reset Start");

    board.rewind();
    board.pad_drive(US, 6, Drive::Low).expect("the full reset pulse must begin");
    let reset = 21 * US;
    board.pad_drive(reset, 6, Drive::High).expect("P06: a 20 us RESX pulse must reset");
    let quiet = "CO5300 command within 5 ms of reset or a sleep command (P06/P09/P10/P10a)";
    assert_eq!(transfer(&mut board, &xfer(reset + 4 * MS, 0x20, 0), &[], 5), Err(quiet),
        "P06: a command 4 ms after RESX rises must stop");
    let mut t = reset + 5 * MS;
    command(&mut board, &mut t, 0x20, &[]);

    // P06 bars Sleep Out for 120 ms after hardware reset; SWRESET with the sleep state unknown
    // (D-P1) assumes Sleep Out, so P09 bars it for 120 ms after the SWRESET as well.
    let swreset = command(&mut board, &mut t, 0x01, &[]);
    let early = xfer(reset + 120 * MS, 0x11, 0);
    assert_eq!(transfer(&mut board, &early, &[], 5),
        Err("CO5300 SLPOUT before the 120 ms reset/SLPIN/SWRESET wait (P06/P09/P10)"),
        "D-P1/P09: SLPOUT within 120 ms of SWRESET from an unknown sleep state must stop");
    t = swreset.t1 + 120 * MS;
    let sleep_out = command(&mut board, &mut t, 0x11, &[]);
    assert_eq!(transfer(&mut board, &xfer(sleep_out.t1 + 4 * MS, 0x29, 0), &[], 5), Err(quiet),
        "P10: SLPOUT must bar every command for 5 ms");
    t = sleep_out.t1 + 5 * MS;
    let disabled = xfer(t, 0x2c, 2);
    assert_eq!(transfer(&mut board, &disabled, &[0xf8, 0x00], 5),
        Err("CO5300 RAMWR/RAMWRC with SPI_WRAM clear: SPI writes to frame RAM are disabled (P23)"),
        "P23: a reset leaves SPI writes to frame RAM disabled until SetSPIMode sets SPI_WRAM");
    t = disabled.t1 + 20;
    let unlit = xfer(t, 0x29, 0);
    assert_eq!(transfer(&mut board, &unlit, &[], 5),
        Err("CO5300 DISPON without WRCTRLD's BCTRL set: the luminance is not documented (P24/D-P6)"),
        "P24/D-P6: DISPON must stop until WRCTRLD sets BCTRL");
    t = unlit.t1 + 20;
    command(&mut board, &mut t, 0xc4, &[0x80]);
    let poisoned = xfer(t, 0x2c, 2);
    assert_eq!(transfer(&mut board, &poisoned, &[0xf8, 0x00], 5),
        Err("CO5300 RAMWR/RAMWRC before COLMOD: conflicting reset defaults (D-P2/P08)"),
        "D-P2: RAMWR must stop until COLMOD resolves the conflicting defaults");
    t = poisoned.t1 + 20;
    command(&mut board, &mut t, 0x3a, &[0x55]);
    command(&mut board, &mut t, 0x2a, &[0, 0, 0, 3]);
    command(&mut board, &mut t, 0x2b, &[0, 0, 0, 1]);
    let pixels = [0xf800, 0x07e0, 0x001f, 0xffff, 0x0000, 0x1234, 0xabcd, 0x5678];
    let bytes = [0xf8, 0x00, 0x07, 0xe0, 0x00, 0x1f, 0xff, 0xff,
        0x00, 0x00, 0x12, 0x34, 0xab, 0xcd, 0x56, 0x78];
    let mut ramwr = xfer(t, 0x2c, bytes.len());
    ramwr.cmd.1 = 0x32;
    ramwr.lines = [1, 1, 4];
    ramwr.t1 = t + (32 + bytes.len() as u64 * 2) * 10;
    assert_eq!(transfer(&mut board, &ramwr, &bytes, 5), Ok(()),
        "P05a: 0x32 must write RGB565 on four data lanes");
    assert_eq!(&frame(&board)[..4], &pixels[..4],
        "P05a/P18a: the first four big-endian pixels must fill row 0, columns 0..3");
    assert_eq!(&frame(&board)[480..484], &pixels[4..],
        "P18a: the next four pixels must fill row 1, columns 0..3");

    t = ramwr.t1 + 20;
    let extra = xfer(t, 0x3c, 2);
    assert_eq!(transfer(&mut board, &extra, &[0, 1], 5),
        Err("CO5300 pixels beyond the window have no documented behavior (P18/O03)"),
        "P18: RAMWRC's ninth pixel must stop instead of wrapping the window");
    t = extra.t1 + 20;
    let mut bad_mode = xfer(t, 0x20, 0);
    bad_mode.mode = 1;
    assert_eq!(transfer(&mut board, &bad_mode, &[], 5),
        Err("CO5300 SPI mode does not sample on rising SCL (P02)"),
        "P02: SPI mode 1 must stop");
    t = bad_mode.t1 + 20;
    let mut fast = xfer(t, 0x20, 0);
    fast.bit_ticks = 9;
    assert_eq!(transfer(&mut board, &fast, &[], 5),
        Err("CO5300 SPI write cycle shorter than 20 ns (P03)"),
        "P03: nine ticks at 480 MHz must fail the 20 ns write-cycle minimum");
    t = fast.t1 + 20;
    let setup = xfer(t, 0x20, 0);
    assert_eq!(transfer(&mut board, &setup, &[], 4),
        Err("CO5300 CSX setup shorter than 10 ns (P03)"),
        "P03: four ticks at 480 MHz must fail the 10 ns CS setup minimum");

    let mut records = Vec::new();
    board.take_bus(&mut records);
    let written = records.iter().find(|event| matches!(event,
        BusEvent::Spi { t0, .. } if *t0 == ramwr.t0));
    assert_eq!(written, Some(&BusEvent::Spi {
        t0: ramwr.t0, t1: ramwr.t1, bus: 0, bit_ticks: 10, mode: 0, lines: [1, 1, 4],
        head: bytes, len: 16, hash: fnv1a(&Bytes(&bytes)),
    }), "the successful RAMWR record must retain its times, lanes, length, first bytes and FNV-1a payload hash");
    assert_eq!(records.iter().filter(|event| matches!(event,
        BusEvent::Note { what, .. } if what.starts_with("CO5300 accepts SPI modes"))).count(), 1,
        "P02: acceptance of rising-edge SPI modes must be noted once");
    assert_eq!(records.iter().filter(|event| matches!(event,
        BusEvent::Note { what, .. } if what.starts_with("CO5300 row rule"))).count(), 1,
        "D-P3: the inferred row-window rule must be noted once");
    assert_eq!(records.iter().filter(|event| matches!(event,
        BusEvent::Note { what, .. } if what.starts_with("CO5300 pixel order"))).count(), 1,
        "P18a: the assumed pixel order must be noted once");
    assert_eq!(records.iter().filter(|event| matches!(event,
        BusEvent::Note { what, .. } if what.starts_with("CO5300 SWRESET with the sleep state unknown"))).count(), 1,
        "D-P1: SWRESET's adverse Sleep Out assumption must be noted once");

    let mut ports = board.ports();
    let (_, reader) = &mut ports.screens[0];
    board.mark();
    board.publish(0.125);
    let published = reader.latest().bgra.clone();
    let published_mean = reader.latest().mean;
    let generation = reader.latest().generation;
    t = setup.t1 + 20;
    command(&mut board, &mut t, 0x2c, &[0x12; 16]);
    assert_eq!(&frame(&board)[..4], &[0x1212; 4],
        "RAMWR after the mark must replace the first row");
    assert_eq!(&frame(&board)[480..484], &[0x1212; 4],
        "RAMWR after the mark must replace the second row");
    assert_eq!(reader.latest().generation, generation, "an unpublished strip never reaches the reader");
    board.rewind();
    assert_eq!(&frame(&board)[..4], &pixels[..4],
        "rewind must restore the first row's original pixel words");
    assert_eq!(&frame(&board)[480..484], &pixels[4..],
        "rewind must restore the second row's original pixel words");
    board.mark();
    board.publish(0.125);
    assert_eq!(reader.latest().bgra, published, "dirty over-marking after rewind publishes only restored pixels");
    assert_eq!(reader.latest().mean, published_mean, "rewind publication must restore the glass mean with the pixels");
    records.clear();
    board.take_bus(&mut records);
    assert!(records.is_empty(), "rewind must discard every bus record after the mark");
}

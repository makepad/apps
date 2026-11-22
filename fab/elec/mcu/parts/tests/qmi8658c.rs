use fab_elec_mcu_emu::board::{Board, BusEvent, I2cAns, I2cOp, Res};
use fab_elec_mcu_emu::Pull;
use fab_elec_mcu_parts::{BoardDesc, NetDesc, Netlist, PartDesc, PartKind, PinRef, Qmi8658c, Stimulus};

fn pin(part: u16, pin: &'static str) -> PinRef {
    PinRef::Part { part, pin: pin.into() }
}

fn description() -> BoardDesc {
    BoardDesc {
        seed: 0,
        parts: vec![
            PartDesc { refdes: "U5".into(), kind: PartKind::Qmi8658c { addr: 0x6b } },
            PartDesc { refdes: "U6".into(), kind: PartKind::Qmi8658c { addr: 0x6a } },
        ],
        nets: vec![
            NetDesc { rise_ps: None, name: "SCL".into(), pull: Pull::Up, pins: vec![PinRef::Chip("SCL".into()), pin(0, "SCL"), pin(1, "SCL")] },
            NetDesc { rise_ps: None, name: "SDA".into(), pull: Pull::Up, pins: vec![PinRef::Chip("SDA".into()), pin(0, "SDA"), pin(1, "SDA")] },
            NetDesc { rise_ps: None, name: "INT1".into(), pull: Pull::None, pins: vec![pin(0, "INT1"), PinRef::Chip("INT1".into())] },
            NetDesc { rise_ps: None, name: "INT2".into(), pull: Pull::None, pins: vec![pin(0, "INT2")] },
            NetDesc { rise_ps: None, name: "OTHER_INT1".into(), pull: Pull::None, pins: vec![pin(1, "INT1")] },
        ],
    }
}

fn step(board: &mut Netlist, t: u64, op: I2cOp) -> Res<I2cAns> {
    board.i2c(t, 0, 1, op)
}

fn select(board: &mut Netlist, t: u64, addr: u8, reg: u8) -> Res {
    step(board, t, I2cOp::Start)?;
    assert_eq!(step(board, t, I2cOp::Write(addr << 1))?, I2cAns::Ack(true), "selected address must ACK");
    assert_eq!(step(board, t, I2cOp::Write(reg))?, I2cAns::Ack(true), "documented register pointer must ACK");
    Ok(())
}

fn write(board: &mut Netlist, t: u64, addr: u8, reg: u8, value: u8) -> Res {
    select(board, t, addr, reg)?;
    assert_eq!(step(board, t, I2cOp::Write(value))?, I2cAns::Ack(true), "accepted register data must ACK");
    step(board, t, I2cOp::Stop)?;
    Ok(())
}

fn read<const N: usize>(board: &mut Netlist, t: u64, addr: u8, reg: u8) -> Res<[u8; N]> {
    select(board, t, addr, reg)?;
    step(board, t, I2cOp::Start)?;
    assert_eq!(step(board, t, I2cOp::Write((addr << 1) | 1))?, I2cAns::Ack(true), "repeated-start read address must ACK");
    let mut bytes = [0; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let I2cAns::Byte(value) = step(board, t, I2cOp::Read { ack: index + 1 < N })? else {
            panic!("selected read must return a data byte");
        };
        *byte = value;
    }
    step(board, t, I2cOp::Stop)?;
    Ok(bytes)
}

fn word(board: &mut Netlist, t: u64, reg: u8) -> i16 {
    i16::from_le_bytes(read(board, t, 0x6b, reg).expect("little-endian word read must succeed"))
}

fn timestamp(board: &mut Netlist, t: u64) -> u32 {
    let [lo, mid, hi] = read(board, t, 0x6b, 0x30).expect("timestamp burst must succeed");
    u32::from_le_bytes([lo, mid, hi, 0])
}

fn motion(board: &mut Netlist, t: u64, accel_ug: [i32; 3], gyro_mdps: [i32; 3]) {
    board.input(t, Stimulus::Motion { part: 0, accel_ug, gyro_mdps })
        .expect("motion stimulus must be accepted");
}

#[test]
fn register_read_timing_motion_and_rewind() {
    // Microsecond ticks make the selected nominal ODR deadlines exact.
    let mut board = Netlist::new(description()).expect("two distinct IMU addresses must share I2C");
    board.bind(&["SCL", "SDA", "INT1"], 1_000_000).expect("IMU board must bind");
    let int1 = board.net("INT1").expect("INT1 net must exist");
    let int2 = board.net("INT2").expect("isolated INT2 net must exist");
    let other_int1 = board.net("OTHER_INT1").expect("second INT1 net must exist");
    const BOOT: u64 = 1_750_000;
    assert_eq!(board.next_event(), Some(BOOT), "startup must end 1.75 seconds after tick zero");
    assert!(!board.level(int1) && !board.level(int2), "both interrupts must start low");
    step(&mut board, 0, I2cOp::Start).expect("unaddressed START must work during startup");
    assert_eq!(step(&mut board, 0, I2cOp::Write(0x68 << 1)), Ok(I2cAns::Ack(false)), "unselected addresses must NACK during startup");
    assert_eq!(read::<1>(&mut board, BOOT - 1, 0x6b, 0), Err("QMI8658C: accessed during power-on startup or a soft reset"), "last startup tick must refuse access");
    board.event(BOOT - 1).expect("early event must do nothing");
    assert!(!board.level(int1), "INT1 must stay low until startup ends");
    board.event(BOOT).expect("startup events must assert INT1");
    assert!(board.level(int1) && board.level(other_int1), "both devices must signal startup completion");
    assert_eq!(board.next_event(), Some(BOOT + 4_000), "startup INT1 pulse must last 4 ms");
    assert_eq!(read::<1>(&mut board, BOOT, 0x6b, 0), Ok([5]), "WHO_AM_I must be 0x05");
    assert_eq!(read::<1>(&mut board, BOOT, 0x6a, 0), Ok([5]), "alternate SA0 address must identify the same device");
    let revision = read::<1>(&mut board, BOOT, 0x6b, 1).expect_err("conflicting revision must stop");
    assert!(revision.contains("0x68 vs 0x79") && revision.contains("893") && revision.contains("1099"), "revision error must identify both conflicting values and lines");
    assert_eq!(read::<1>(&mut board, BOOT, 0x6b, 0x2d), Ok([2]), "STATUSINT must mirror startup INT1 and omit INT2");
    assert_eq!(read::<1>(&mut board, BOOT + 1, 0x6b, 0x2f), Ok([0]), "STATUS1 has no command completion at startup");
    assert!(!board.level(int1), "STATUS1 read must clear INT1 early");
    assert!(board.level(other_int1), "clearing one device must leave the other device's INT1 asserted");
    board.event(BOOT + 4_000).expect("uncleared pulse must expire");
    assert!(!board.level(other_int1), "uncleared INT1 must fall exactly at 4 ms");
    assert_eq!(board.next_event(), None, "startup requires at most two events per IMU");

    const ENABLE: u64 = BOOT + 10_000;
    assert_eq!(read::<1>(&mut board, ENABLE, 0x6b, 2), Ok([0x20]), "CTRL1 must reset big-endian with increment disabled");
    // Every readable mapped reset register, excluding the disputed revision and
    // dynamic STATUSINT already checked above. CTRL8 and gaps are not UI storage.
    for reg in (3..=8).chain(0x0a..=0x17).chain(0x2c..=0x58) {
        // 0x4D/0x4E: the reset flag, read little-endian below (the byte-order decision swaps them).
        if matches!(reg, 0x47 | 0x48 | 0x4d | 0x4e) { continue; }
        assert_eq!(read::<1>(&mut board, ENABLE, 0x6b, reg), Ok([0]), "mapped register {reg:#04x} must retain its zero reset value before enable");
    }
    assert_eq!(read::<3>(&mut board, ENABLE, 0x6b, 0), Ok([5; 3]), "AI=0 must repeat the selected register");
    assert_eq!(write(&mut board, ENABLE, 0x6b, 8, 1), Err("QMI8658C: accel high ODR requires both sensors (lines 657–660)"), "reset high accel ODR must not run alone");
    write(&mut board, ENABLE, 0x6b, 3, 6).expect("accel 125 Hz must configure");
    write(&mut board, ENABLE, 0x6b, 4, 7).expect("gyro 62.5 Hz must configure");
    write(&mut board, ENABLE, 0x6b, 2, 0x60).expect("big-endian increment must configure");
    write(&mut board, ENABLE, 0x6b, 8, 3).expect("both sensors must enable");
    assert_eq!(board.next_event(), None, "sampling must not schedule ODR events");
    const FIRST_A: u64 = ENABLE + 3_000 + 3 * 8_000;
    const FIRST_G: u64 = ENABLE + 60_000 + 3 * 16_000;
    assert_eq!(read::<1>(&mut board, FIRST_A - 1, 0x6b, 0x2e), Ok([0]), "settling must keep STATUS0 clear");
    assert_eq!(read::<12>(&mut board, FIRST_A - 1, 0x6b, 0x35), Ok([0; 12]), "settling must preserve reset output bytes");
    assert_eq!(read::<12>(&mut board, FIRST_A, 0x6b, 0x35), Ok([0, 0, 0, 0, 0x40, 0, 0, 0, 0, 0, 0, 0]), "first accel sample must publish +1g in big-endian 12-byte burst");
    assert_eq!(read::<1>(&mut board, FIRST_A, 0x6b, 0x2e), Ok([1]), "only the settled accelerometer must have data");
    assert_eq!(read::<1>(&mut board, FIRST_A, 0x6b, 0x2e), Ok([0]), "STATUS0 must clear exactly the returned flags");
    assert_eq!(timestamp(&mut board, FIRST_A), 1, "first fastest-sensor publication must increment TIMESTAMP once");
    write(&mut board, FIRST_A, 0x6b, 2, 0x40).expect("little-endian increment must configure");
    assert_eq!(word(&mut board, FIRST_A, 0x39), 16_384, "CTRL1 BE=0 must expose low byte first without resampling");
    assert_eq!(word(&mut board, FIRST_A, 0x33), 6_400, "25 C must use the host's 256 LSB/C scale");
    motion(&mut board, FIRST_A + 1, [1_000_000, -1_000_000, 0], [1_000, -1_000, 500]);
    assert_eq!(word(&mut board, FIRST_A + 7_999, 0x39), 16_384, "stimulus between samples must leave old Z output intact");
    assert_eq!(read::<6>(&mut board, FIRST_A + 8_000, 0x6b, 0x35), Ok([0, 0x40, 0, 0xc0, 0, 0]), "new acceleration must publish exactly at next ODR deadline");
    assert_eq!(read::<1>(&mut board, FIRST_A + 8_000, 0x6b, 0x2e), Ok([1]), "the next accel publication must reassert its flag");
    motion(&mut board, FIRST_A + 16_000, [500_000, 0, 0], [1_000, -1_000, 500]);
    assert_eq!(word(&mut board, FIRST_A + 16_000, 0x35), 16_384, "stimulus at a sample tick must first latch the preceding stimulus");
    assert_eq!(word(&mut board, FIRST_A + 24_000, 0x35), 8_192, "boundary stimulus must first affect the following sample");
    assert_eq!(word(&mut board, FIRST_G - 1, 0x3b), 0, "gyro must retain zero through wake-up and settling");
    assert_eq!(read::<6>(&mut board, FIRST_G, 0x6b, 0x3b), Ok([0, 8, 0, 0xf8, 0, 4]), "gyro first sample must use signed 2048 LSB/dps");
    assert_eq!(timestamp(&mut board, FIRST_G), 11, "TIMESTAMP must count accel's eleven samples, not add gyro's first sample");
    assert_eq!(read::<1>(&mut board, FIRST_G, 0x6b, 0x2e), Ok([3]), "separate sensor publications must set both availability bits");

    // Full-scale changes preserve the last output until another publication.
    write(&mut board, FIRST_G, 0x6b, 3, 0x16).expect("accel +/-4g must configure while sampling");
    write(&mut board, FIRST_G, 0x6b, 4, 0x17).expect("gyro +/-32dps must configure while sampling");
    assert_eq!(word(&mut board, FIRST_G, 0x35), 8_192, "full-scale write must not retroactively rescale accel output");
    assert_eq!(word(&mut board, FIRST_G + 7_000, 0x35), 4_096, "next accel sample must use the new 8192 LSB/g scale");
    assert_eq!(word(&mut board, FIRST_G + 16_000, 0x3b), 1_024, "next gyro sample must use the new 1024 LSB/dps scale");
    const SAT: u64 = FIRST_G + 16_001;
    motion(&mut board, SAT, [i32::MAX, i32::MIN, 0], [i32::MAX, i32::MIN, 0]);
    const SETTLED_SAT: u64 = FIRST_G + 32_000;
    assert_eq!(read::<12>(&mut board, SETTLED_SAT, 0x6b, 0x35), Ok([0xff, 0x7f, 0, 0x80, 0, 0, 0xff, 0x7f, 0, 0x80, 0, 0]), "both sensors must saturate at signed 16-bit endpoints");
    // Check the largest ranges and rounding on either side of half an LSB.
    write(&mut board, SETTLED_SAT, 0x6b, 3, 0x36).expect("largest accel range must configure");
    write(&mut board, SETTLED_SAT, 0x6b, 4, 0x77).expect("largest gyro range must configure");
    motion(&mut board, SETTLED_SAT + 1, [15_625, -15_625, 0], [31, -31, 32]);
    const QUANTISED: u64 = SETTLED_SAT + 16_000;
    assert_eq!(read::<6>(&mut board, QUANTISED, 0x6b, 0x35), Ok([32, 0, 0xe0, 0xff, 0, 0]), "largest accel range must use 2048 LSB/g with signed conversion");
    assert_eq!(read::<6>(&mut board, QUANTISED, 0x6b, 0x3b), Ok([0, 0, 0, 0, 1, 0]), "gyro conversion must round below and above half an LSB");
    // 16 LSB/dps * integer mdps cannot land exactly on a half LSB; at 64
    // LSB/dps, 125 mdps is exact. All sensitivities are powers of two, so
    // neither integer stimulus unit admits an exact half-LSB tie.

    assert_eq!(write(&mut board, QUANTISED, 0x6b, 3, 9), Err("QMI8658C: reserved CTRL2 ODR (lines 1142–1144)"), "reserved accel ODR must stop");
    assert_eq!(write(&mut board, QUANTISED, 0x6b, 4, 15), Err("QMI8658C: reserved CTRL3 ODR (lines 1181–1187)"), "reserved gyro ODR must stop");
    assert_eq!(write(&mut board, QUANTISED, 0x6b, 3, 0x37), Err("QMI8658C: ODR change while sampling or stopping not modelled; disable and wait 2/ODR"), "live ODR changes must refuse an undocumented rephasing");
    for (reg, value, feature) in [
        (3, 0x80, "self test"), (4, 0x80, "self test"), (5, 1, "magnetometer"),
        (7, 0x80, "motion on demand"), (7, 1, "AttitudeEngine"), (8, 4, "magnetometer"),
        (8, 8, "AttitudeEngine"), (8, 0x10, "snooze"), (8, 0x40, "high-speed"),
        (0x0a, 0, "CTRL9"), (0x14, 1, "FIFO"),
    ] {
        let error = write(&mut board, QUANTISED, 0x6b, reg, value).expect_err("unsupported feature must fail");
        assert!(error.contains(feature) && error.contains("not modelled"), "unsupported feature error must name {feature}");
    }
    for reg in [9, 0x18, 0x2b, 0x47, 0x48, 0x59, 0x61] {
        let error = read::<1>(&mut board, QUANTISED, 0x6b, reg).expect_err("reserved address must fail");
        assert!(error.contains(&format!("0x{reg:02X}")), "reserved address error must identify {reg:#04x}");
        assert_eq!(write(&mut board, QUANTISED, 0x6b, reg, 0), Err(error), "reserved address must reject writes as well as reads");
    }
    assert_eq!(read::<1>(&mut board, QUANTISED, 0x6b, 0xb5), Err("QMI8658C: address bit 7 increment convention (line 2580) conflicts with CTRL1 bit 6; not modelled"), "address-bit-7 convention must fail explicitly");
    assert!(read::<2>(&mut board, QUANTISED, 0x6b, 0x58).expect_err("burst into a hole must fail").contains("0x59"), "burst must not wrap across the last contiguous data register");
    assert!(write(&mut board, QUANTISED, 0x6b, 0, 0).is_err(), "read-only WHO_AM_I must reject writes");
    assert!(read::<1>(&mut board, QUANTISED, 0x6b, 0x60).is_err(), "write-only RESET must reject reads");
    // 0x0B is Rev A 7.4's text; its register table's 0xB0 resets, as the chip does (checked at the end).
    for value in [0, 1, 0x0b, 0xff] {
        assert_eq!(write(&mut board, QUANTISED, 0x6b, 0x60, value), Err("QMI8658C: RESET takes 0xB0 (Rev A Table 27, as the chip does; its 7.4 text's 0x0B is not a reset)"),
            "every other reset value must fail without resetting state");
    }
    assert_eq!(read::<1>(&mut board, QUANTISED, 0x6b, 3), Ok([0x36]), "failed writes must preserve accepted configuration");

    // Save an active read transaction, unread flags, sampling phase, stimulus and
    // unissued assumption notes. Replaying includes the adverse shutdown tail.
    select(&mut board, QUANTISED, 0x6b, 0x35).expect("saved read must select AX");
    step(&mut board, QUANTISED, I2cOp::Start).expect("saved repeated start must work");
    step(&mut board, QUANTISED, I2cOp::Write(0xd7)).expect("saved read address must work");
    let mut discarded = Vec::new();
    board.take_bus(&mut discarded);
    let note_count = |fragment: &str| discarded.iter().filter(|event| matches!(event, BusEvent::Note { what, .. } if what.contains(fragment))).count();
    assert_eq!(note_count("STATUS0 read clears"), 1, "STATUS0 assumption must be noted once per model");
    assert_eq!(note_count("face-up default"), 1, "default orientation must be noted on first use only");
    assert_eq!(note_count("constant 25 C"), 1, "temperature assumption must be noted once");
    assert_eq!(note_count("nominal ODR"), 1, "nominal ODR assumption must be noted once across both sensors");
    let mut inputs = Vec::new();
    board.pad_inputs(&mut inputs);
    board.mark();
    let replay = |board: &mut Netlist| {
        let mut bytes = vec![step(board, QUANTISED, I2cOp::Read { ack: false }).expect("saved I2C read must resume")];
        write(board, QUANTISED, 0x6b, 6, 0x77).expect("all documented filter modes must be accepted");
        write(board, QUANTISED, 0x6b, 6, 0x77).expect("repeated filter enable must be accepted");
        write(board, QUANTISED, 0x6b, 8, 0).expect("sensors must disable");
        let stopped_count = timestamp(board, QUANTISED);
        motion(board, QUANTISED + 1, [1_000_000, 0, 0], [1_000, 0, 0]);
        assert_eq!(word(board, QUANTISED + 7_000, 0x35), 2_048, "disabled accel must still publish during its 2/ODR tail");
        assert_eq!(write(board, QUANTISED + 7_000, 0x6b, 8, 3), Err("QMI8658C: re-enable during 2/ODR shutdown not modelled (lines 2093–2096)"), "overlapping restart must stop rather than invent a phase");
        read::<1>(board, QUANTISED + 31_999, 0x6b, 0x2e).expect("tail flags must be readable");
        motion(board, QUANTISED + 31_999, [0; 3], [2_000, 0, 0]);
        assert_eq!(word(board, QUANTISED + 32_000, 0x3b), 32, "gyro must publish a sample exactly at its full 2/ODR stop deadline");
        assert_eq!(read::<1>(board, QUANTISED + 32_000, 0x6b, 0x2e), Ok([2]), "only gyro must publish at the last shutdown boundary");
        assert_eq!(timestamp(board, QUANTISED + 32_000), stopped_count, "disabled sensor tails must not clock the enabled-sensor timestamp");
        motion(board, QUANTISED + 32_001, [0; 3], [0; 3]);
        for byte in read::<12>(board, QUANTISED + 64_000, 0x6b, 0x35).expect("stopped output burst must succeed") {
            bytes.push(I2cAns::Byte(byte));
        }
        assert_eq!(word(board, QUANTISED + 64_000, 0x35), 2_048, "stopped accel must retain its last tail sample");
        assert_eq!(word(board, QUANTISED + 64_000, 0x3b), 32, "stopped gyro must retain its last tail sample");
        assert_eq!(read::<1>(board, QUANTISED + 64_000, 0x6b, 0x2e), Ok([0]), "stopped sensors must not reassert cleared flags");
        let mut bus = Vec::new();
        board.take_bus(&mut bus);
        assert_eq!(bus.iter().filter(|event| matches!(event, BusEvent::Note { what, .. } if what.contains("filter response"))).count(), 1, "filter omission must be noted once per replay");
        (bytes, bus, board.next_event())
    };
    let first = replay(&mut board);
    board.rewind();
    assert_eq!(first, replay(&mut board), "rewind must reproduce I2C phase, samples, status, timing and one-time bus notes");

    const RESTART: u64 = QUANTISED + 64_000;
    write(&mut board, RESTART, 0x6b, 3, 0x0c).expect("low-power accel must configure with gyro off");
    assert_eq!(write(&mut board, RESTART, 0x6b, 8, 3), Err("QMI8658C: accel low-power mode requires gyro off (line 1205)"), "low-power accel must refuse enabling gyro");
    write(&mut board, RESTART, 0x6b, 8, 1).expect("low-power accel alone must enable");
    assert_eq!(write(&mut board, RESTART, 0x6b, 8, 3), Err("QMI8658C: accel low-power mode requires gyro off (line 1205)"), "enabling gyro after low-power accel must also fail");
    const LP_FIRST: u64 = RESTART + 3_000 + 23_438; // ceil(3/128 seconds).
    assert_eq!(read::<1>(&mut board, LP_FIRST - 1, 0x6b, 0x2e), Ok([0]), "fractional first sample must round up, never publish early");
    assert_eq!(word(&mut board, LP_FIRST - 1, 0x35), 2_048, "restart settling must preserve the last output from the prior run");
    let before = timestamp(&mut board, LP_FIRST - 1);
    assert_eq!(timestamp(&mut board, LP_FIRST), before + 1, "128 Hz first sample must publish at the rounded rational deadline");
    // One large lazy jump checks 24-bit wrap without iterating over millions of samples.
    let after_wrap = RESTART + 3_000 + ((3_u64 + (1 << 24)) * 1_000_000).div_ceil(128);
    assert_eq!(timestamp(&mut board, after_wrap), before + 1, "TIMESTAMP must wrap at 24 bits without summing or scheduling samples");
    assert_eq!(board.next_event(), None, "a multi-million-sample jump must leave the event queue empty");
    assert!(!board.level(int2), "isolated INT2 must stay low throughout sensor operation");

    // Reuse the already-bound second device to check equal-rate tie-breaking,
    // the 6DOF-only high rates, and switching the timestamp source to gyro.
    write(&mut board, after_wrap, 0x6a, 2, 0x40).expect("second device must enable little-endian bursts");
    assert_eq!(read::<1>(&mut board, after_wrap, 0x6a, 0x4d), Ok([0x80]), "after power-on 0x4D reads 0x80 (Rev A 7.4)");
    write(&mut board, after_wrap, 0x6a, 8, 3).expect("8000 Hz must be permitted with both sensors enabled");
    assert_eq!(write(&mut board, after_wrap, 0x6a, 8, 1), Err("QMI8658C: accel high ODR requires both sensors (lines 657–660)"), "high-rate accel must refuse disabling gyro alone");
    let equal_first_a = after_wrap + 3_375;
    assert_eq!(read::<3>(&mut board, equal_first_a, 0x6a, 0x30), Ok([1, 0, 0]), "equal ODR must clock TIMESTAMP from accel's earlier first sample");
    let equal_first_g = after_wrap + 60_375;
    assert_eq!(read::<3>(&mut board, equal_first_g, 0x6a, 0x30), Ok([0xc9, 1, 0]), "equal ODR must count 457 accel samples, not add gyro's first");
    write(&mut board, equal_first_g, 0x6a, 8, 0).expect("both high-rate sensors must disable together");
    let gyro_restart = equal_first_g + 250; // Full 2/8000-second shutdown.
    write(&mut board, gyro_restart, 0x6a, 3, 8).expect("stopped accel may change ODR to 31.25 Hz");
    write(&mut board, gyro_restart, 0x6a, 4, 3).expect("stopped gyro may change ODR to 1000 Hz");
    write(&mut board, gyro_restart, 0x6a, 8, 3).expect("unequal rates must restart");
    assert_eq!(read::<3>(&mut board, gyro_restart + 63_000, 0x6a, 0x30), Ok([0xca, 1, 0]), "faster gyro must clock TIMESTAMP at its first settled sample");
    assert_eq!(read::<3>(&mut board, gyro_restart + 99_000, 0x6a, 0x30), Ok([0xee, 1, 0]), "faster gyro must contribute 37 samples while slower accel contributes none to TIMESTAMP");

    // Soft reset (Rev A): the running first device returns to its reset state and is busy for 15.2 ms.
    let soft = gyro_restart + 100_000;
    write(&mut board, soft, 0x6b, 0x60, 0xb0).expect("0xB0 to RESET must reset, acknowledged");
    let ready = soft + 15_200;
    assert_eq!(read::<1>(&mut board, ready - 1, 0x6b, 0x4d), Err("QMI8658C: accessed during power-on startup or a soft reset"),
        "the reset's initialization must refuse access to its last tick");
    assert_eq!(board.next_event(), Some(ready), "the reset ends 200 us + 15 ms after the command");
    board.event(ready).expect("the reset's end must assert INT1");
    assert!(board.level(int1), "INT1 must signal the reset's end as at power-on");
    assert_eq!(read::<1>(&mut board, ready, 0x6b, 2), Ok([0x20]), "CTRL1 must return to big-endian, no increment");
    write(&mut board, ready, 0x6b, 2, 0x40).expect("little-endian auto-increment after the reset");
    assert_eq!(read::<1>(&mut board, ready, 0x6b, 0x4d), Ok([0x80]), "0x4D must read 0x80 after a successful soft reset");
    assert_eq!(read::<1>(&mut board, ready, 0x6b, 8), Ok([0]), "the reset must turn both sensors off");
}

#[test]
fn int2_wiring_and_address_guards() {
    assert!(Qmi8658c::new(0x69).expect_err("undocumented address must fail").contains("0x6a or 0x6b"), "address error must name both permitted straps");
    let desc = description();
    for peer in [PinRef::Chip("IRQ".into()), pin(1, "INT2"), pin(0, "INT1")] {
        let mut wired = desc.clone();
        wired.nets[3].pins.push(peer);
        assert_eq!(Netlist::new(wired).expect_err("connected INT2 must be refused"), "QMI8658C INT2 data-ready pulses not modelled", "INT2 must refuse every chip or part peer");
    }
    let mut wired = desc.clone();
    wired.nets[3].pins.insert(0, PinRef::Chip("IRQ".into()));
    assert_eq!(Netlist::new(wired).expect_err("pin order must not bypass INT2 guard"), "QMI8658C INT2 data-ready pulses not modelled", "INT2 guard must not depend on pin order");
    assert!(Netlist::new(desc).is_ok(), "isolated INT2 must be accepted");
}

#[test]
fn sync_sample_holds_a_burst_across_an_odr_update() {
    const BOOT: u64 = 1_750_000;
    // Both sensors at 8000 Hz: gyro settles at 60 ms + 3/ODR. The
    // accelerometer's earlier phase differs by exactly 456 periods here.
    const FIRST: u64 = BOOT + 60_375;
    const UPDATE: u64 = FIRST + 125;
    for locked in [false, true] {
        let mut board = Netlist::new(description()).expect("IMU netlist");
        board.bind(&["SCL", "SDA", "INT1"], 1_000_000).expect("microsecond board ticks");
        write(&mut board, BOOT, 0x6b, 2, 0x40).expect("CTRL1 AI, little endian");
        write(&mut board, BOOT, 0x6b, 8, if locked { 0x83 } else { 3 }).expect("enable 6DOF and selected sync mode");
        motion(&mut board, BOOT, [1_000_061; 3], [1_000; 3]);
        assert_eq!(word(&mut board, FIRST, 0x35), 16_385, "old generation has a nonzero low byte");
        motion(&mut board, FIRST + 1, [500_000; 3], [-1_000; 3]);
        assert_eq!(read::<1>(&mut board, UPDATE - 2, 0x6b, 0x2e), Ok([3]), "STATUS0 observes both sensors' old data");
        select(&mut board, UPDATE - 1, 0x6b, 0x35).expect("axis pointer");
        step(&mut board, UPDATE - 1, I2cOp::Start).expect("repeated START");
        step(&mut board, UPDATE - 1, I2cOp::Write(0xd7)).expect("read address");
        board.mark();
        let burst = |board: &mut Netlist| {
            let mut bytes = [0; 12];
            for (i, byte) in bytes.iter_mut().enumerate() {
                let I2cAns::Byte(value) = step(board, UPDATE - 1 + i as u64,
                    I2cOp::Read { ack: i != 11 }).expect("burst across ODR") else { panic!("data byte") };
                *byte = value;
            }
            step(board, UPDATE + 11, I2cOp::Stop).expect("STOP after data");
            bytes
        };
        let bytes = burst(&mut board);
        board.rewind();
        assert_eq!(burst(&mut board), bytes, "rewind restores the lock and in-flight burst");
        let values: [i16; 6] = core::array::from_fn(|i| i16::from_le_bytes([bytes[2*i], bytes[2*i+1]]));
        if locked {
            assert_eq!(values, [16_385, 16_385, 16_385, 2_048, 2_048, 2_048], "every locked byte belongs to the old generation");
        } else {
            assert_eq!(values, [8_193, 8_192, 8_192, -2_048, -2_048, -2_048], "unlocked AX tears between its low/high bytes; later axes are new");
        }
        assert_eq!(read::<12>(&mut board, UPDATE + 125, 0x6b, 0x35),
            Ok([0, 0x20, 0, 0x20, 0, 0x20, 0, 0xf8, 0, 0xf8, 0, 0xf8]),
            "reading GZ_H releases the lock for the next ODR generation");
    }
}

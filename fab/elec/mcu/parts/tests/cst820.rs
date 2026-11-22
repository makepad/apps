use fab_elec_mcu_emu::board::{Board, BusEvent, I2cAns, I2cOp, Res};
use fab_elec_mcu_emu::{Drive, Pull};
use fab_elec_mcu_parts::{BoardDesc, Cst820, Model, NetDesc, Netlist, PartDesc, PartKind, PinRef, Stimulus};

const BOOT: u64 = 100_000;
const PERIOD: u64 = 10_000;

fn board() -> Netlist {
    // Owned schematic strings may go away immediately after Netlist::new.
    let nets = ["SCL", "SDA", "RST", "INT"].into_iter().map(|pin| NetDesc {
        rise_ps: None,
        name: format!("TP_{pin}").into(), pull: Pull::Up,
        pins: vec![PinRef::Chip(pin.to_owned().into()),
            PinRef::Part { part: 0, pin: pin.to_owned().into() }],
    }).collect();
    let mut b = Netlist::new(BoardDesc { seed: 0,
        parts: vec![PartDesc { refdes: format!("U{}", 1).into(), kind: PartKind::Cst820 { addr: 0x15 } }],
        nets }).expect("owned schematic names must resolve into a touch board");
    b.bind(&["SCL", "SDA", "RST", "INT"], 1_000_000).expect("microsecond touch clock must bind");
    b
}

fn advance(b: &mut Netlist, to: u64) {
    while let Some(t) = b.next_event().filter(|&t| t <= to) {
        b.event(t).expect("documented touch deadline must execute");
    }
}

fn step(b: &mut Netlist, t: u64, op: I2cOp) -> Res<I2cAns> { b.i2c(t, 0, 1, op) }

fn select(b: &mut Netlist, t: u64, reg: u8) -> Res {
    advance(b, t);
    step(b, t, I2cOp::Start)?;
    assert_eq!(step(b, t, I2cOp::Write(0x2a))?, I2cAns::Ack(true), "T01: board-supplied touch address must ACK");
    assert_eq!(step(b, t, I2cOp::Write(reg))?, I2cAns::Ack(true), "T02: documented pointer must ACK");
    Ok(())
}

fn write(b: &mut Netlist, t: u64, reg: u8, value: u8) -> Res {
    select(b, t, reg)?;
    assert_eq!(step(b, t, I2cOp::Write(value))?, I2cAns::Ack(true), "configuration data must ACK");
    step(b, t, I2cOp::Stop)?;
    Ok(())
}

fn read<const N: usize>(b: &mut Netlist, t: u64, reg: u8) -> Res<[u8; N]> {
    select(b, t, reg)?;
    step(b, t, I2cOp::Start)?;
    assert_eq!(step(b, t, I2cOp::Write(0x2b))?, I2cAns::Ack(true), "read address must ACK after a pointer");
    let mut bytes = [0; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let I2cAns::Byte(value) = step(b, t, I2cOp::Read { ack: index + 1 < N })? else {
            panic!("selected CST820 read must return a byte");
        };
        *byte = value;
    }
    step(b, t, I2cOp::Stop)?;
    Ok(bytes)
}

fn touch(b: &mut Netlist, t: u64, x: u16, y: u16, down: bool) {
    advance(b, t); // S1's board-before-input ordering, including equal ticks.
    b.input(t, Stimulus::Touch { part: 0, x, y, down }).expect("12-bit touch stimulus must be accepted");
}

fn reset_pin(b: &mut Netlist, t: u64, high: bool) {
    advance(b, t);
    b.pad_drive(t, 2, if high { Drive::High } else { Drive::Low }).expect("RST edge must settle");
}

fn unpointed(b: &mut Netlist, t: u64) -> Res<I2cAns> {
    advance(b, t);
    step(b, t, I2cOp::Start)?;
    step(b, t, I2cOp::Write(0x2b))
}

#[test]
fn touch_scans_interrupts_resets_sleep_and_rewind() {
    let mut b = board();
    let int = b.net("TP_INT").expect("owned interrupt name must survive description drop");
    assert_eq!(b.next_event(), Some(BOOT), "first scan must wait for the full 100 ms startup");
    assert_eq!(b.part(0).unwrap().drive(2), Drive::OpenDrain { low: false, pull: Pull::None },
        "idle INT must be high-Z without an internal pull-up");
    assert!(read::<1>(&mut b, BOOT - 1, 0xfe).expect_err("startup must refuse access").contains("initialization"),
        "startup refusal must explain the initialization delay");
    for (reg, value) in [(0xed, 10), (0xee, 1), (0xf9, 2), (0xfa, 0), (0xfb, 0), (0xfc, 10), (0xfe, 0)] {
        assert_eq!(read::<1>(&mut b, BOOT, reg), Ok([value]), "T09-T14: register {reg:#04x} must have its chosen reset value");
    }
    write(&mut b, BOOT, 0xfe, 1).expect("firmware must explicitly disable auto-sleep");
    write(&mut b, BOOT, 0xfa, 0x60).expect("EnTouch and EnChange must enable");
    touch(&mut b, BOOT, 0x321, 0x654, false);
    assert!(b.level(int), "a release with no finger on the glass must not pulse EnChange");
    assert!(read::<1>(&mut b, BOOT, 3).expect_err("a release with no finger must sample no coordinates")
        .contains("before a sampled touch"), "a release with no finger on the glass must leave coordinates unsampled");
    touch(&mut b, BOOT + 1, 0x123, 0x456, true);
    assert_eq!(read::<1>(&mut b, BOOT + PERIOD - 1, 2), Ok([0]), "touch must not change FingerNum before a scan");
    assert!(b.level(int), "INT must remain released before the touch scan");
    assert_eq!(read::<5>(&mut b, BOOT + PERIOD, 2), Ok([1, 1, 0x23, 4, 0x56]),
        "five-byte scan must encode finger and both 12-bit coordinates with zero upper nibbles");
    touch(&mut b, BOOT + PERIOD, 0x124, 0x457, true);
    assert_eq!(read::<5>(&mut b, BOOT + PERIOD, 2), Ok([1, 1, 0x24, 4, 0x57]),
        "a move at the same scan tick must replace coordinates without consuming another edge scan");
    assert!(!b.level(int), "touch scan must sink INT");
    advance(&mut b, 110_999);
    assert!(!b.level(int), "default INT pulse must last through 999 microseconds");
    advance(&mut b, 111_000);
    assert!(b.level(int), "default INT pulse must release exactly after 1 ms");
    touch(&mut b, 111_001, 77, 88, true);
    assert_eq!(read::<5>(&mut b, 119_999, 2), Ok([1, 1, 0x24, 4, 0x57]), "move must wait for its scan");
    assert_eq!(read::<5>(&mut b, 120_000, 2), Ok([1, 0, 77, 0, 88]), "next scan must publish the move");
    touch(&mut b, 121_001, 77, 88, false);
    assert_eq!(read::<5>(&mut b, 130_000, 2), Ok([0, 0, 77, 0, 88]), "release must publish at its scan");
    assert!(!b.level(int), "EnChange must pulse on release");

    for (dt, x, down) in [(1, 20, true), (2, 30, true), (3, 30, false),
        (4, 50, true), (5, 60, true), (6, 60, false)]
    {
        touch(&mut b, 130_000 + dt, x, x + 1, down);
    }
    let mut old_bus = Vec::new();
    b.take_bus(&mut old_bus);
    b.mark();
    let mut cloned = b.clone();
    let replay = |b: &mut Netlist| {
        let mut bytes = Vec::new();
        for (t, down, x) in [(140_000, 1, 30), (150_000, 0, 30), (160_000, 1, 60), (170_000, 0, 60)] {
            let answer = read::<5>(b, t, 2).expect("queued contact must be readable");
            assert_eq!(answer, [down, 0, x, 0, x + 1], "every contact edge must survive at scan {t}; only consecutive moves coalesce");
            bytes.push(answer);
        }
        let mut bus = Vec::new();
        b.take_bus(&mut bus);
        (bytes, bus, b.next_event())
    };
    let first = replay(&mut b);
    b.rewind();
    assert_eq!(replay(&mut b), first, "mark/rewind must restore queued transitions, pulses, I2C and notes");
    assert_eq!(replay(&mut cloned), first, "cloning must preserve the complete touch schedule independently");
    write(&mut b, 171_100, 0xfd, 2).expect("IOCtl write must establish its readable value");
    reset_pin(&mut b, 171_100, false);
    reset_pin(&mut b, 171_199, true);
    assert_eq!(read::<1>(&mut b, 171_199, 0xfe), Ok([1]), "99 us reset pulse must preserve configuration");
    assert_eq!(read::<1>(&mut b, 171_199, 0xfd), Ok([2]), "short reset must preserve written IOCtl");
    reset_pin(&mut b, 172_000, false);
    advance(&mut b, 172_100);
    assert_eq!(b.next_event(), None, "RST must qualify exactly at 100 us and pause scanning while held low");
    assert!(read::<1>(&mut b, 172_100, 0xfe).expect_err("held reset must refuse access").contains("RST"),
        "access while reset is asserted must explain the held RST pin");
    reset_pin(&mut b, 172_101, true);
    const READY: u64 = 272_101;
    touch(&mut b, 172_102, 101, 102, true);
    touch(&mut b, 172_103, 101, 102, false);
    assert!(read::<1>(&mut b, READY - 1, 0xfe).expect_err("reset reinitialization must refuse").contains("initialization"),
        "long reset must require 100 ms after release");
    assert!(unpointed(&mut b, READY).expect_err("reset must unset pointer").contains("pointer"), "qualified reset must poison the pointer");
    assert_eq!(read::<1>(&mut b, READY, 0xfe), Ok([0]), "qualified reset must restore auto-sleep default");
    assert_eq!(read::<1>(&mut b, READY, 0xfa), Ok([0]), "qualified reset must disable pulses again");
    assert!(read::<1>(&mut b, READY, 0xfd).expect_err("reset IOCtl must be unknown again").contains("0xFD"), "reset must clear IOCtl write knowledge");
    assert_eq!(read::<5>(&mut b, READY, 2), Ok([1, 0, 101, 0, 102]), "touch down during reset must survive to its first scan");
    assert!(b.level(int), "reset IrqCtl=0 must suppress pulses despite a finger");
    assert_eq!(read::<1>(&mut b, READY + PERIOD, 2), Ok([0]), "touch up during reset must survive to the following scan");
    write(&mut b, READY + PERIOD, 0xfe, 1).expect("reinitialized firmware must disable sleep again");
    write(&mut b, READY + PERIOD, 0xfa, 0x40).expect("EnTouch alone must enable");
    write(&mut b, READY + PERIOD, 0xfc, 1).expect("one-second long press must configure");
    touch(&mut b, READY + PERIOD + 1, 9, 10, true);
    const DOWN: u64 = READY + 2 * PERIOD;
    advance(&mut b, DOWN + PERIOD);
    assert!(!b.level(int), "EnTouch must pulse again on an unchanged held-finger scan");
    write(&mut b, DOWN + PERIOD + 1, 0xed, 200).expect("maximum documented pulse width must configure");
    const LONG: u64 = DOWN + 1_000_000;
    assert_eq!(read::<1>(&mut b, LONG - 1, 0xfc), Ok([1]), "long press must not reset one tick early");
    assert!(!b.level(int), "overlapping 20 ms pulses from 10 ms scans must remain asserted");
    assert!(read::<1>(&mut b, LONG, 0xfc).expect_err("long press must reset").contains("initialization"), "long press deadline must begin reinitialization");
    assert!(b.level(int), "long-press reset must release an active interrupt");
    touch(&mut b, LONG + 1, 9, 10, false);
    const AWAKE: u64 = LONG + BOOT;
    assert!(unpointed(&mut b, AWAKE).expect_err("long press must also unset pointer").contains("pointer"), "long-press reset must poison pointer");
    assert_eq!(read::<1>(&mut b, AWAKE, 0xfc), Ok([10]), "long-press reset must restore the documented 10 s default");
    write(&mut b, AWAKE, 0xfc, 0).expect("zero must disable long-press reset");
    write(&mut b, AWAKE, 0xf9, 1).expect("one-second auto-sleep must configure");
    write(&mut b, AWAKE, 0xfe, 0).expect("zero must enable auto-sleep");
    write(&mut b, AWAKE, 0xfa, 0x20).expect("EnChange alone must enable");
    // Consecutive writes: 0xED width and 0xEE period, with no pointer reset.
    select(&mut b, AWAKE, 0xed).expect("width pointer must select");
    step(&mut b, AWAKE, I2cOp::Write(7)).expect("width write must ACK");
    step(&mut b, AWAKE, I2cOp::Write(1)).expect("consecutive period write must ACK");
    step(&mut b, AWAKE, I2cOp::Stop).expect("write must stop");
    assert_eq!(read::<2>(&mut b, AWAKE, 0xed), Ok([7, 1]), "multi-byte writes and reads must increment the pointer");
    const ASLEEP: u64 = AWAKE + 1_000_000;
    assert_eq!(read::<1>(&mut b, ASLEEP - 1, 0xfe), Ok([0]), "auto-sleep must wait the full no-finger interval");
    assert!(read::<1>(&mut b, ASLEEP, 0xfe).expect_err("standby must refuse I2C").contains("standby"), "standby refusal must name the missing I2C behavior");
    touch(&mut b, ASLEEP + 1, 400, 479, true);
    assert!(read::<1>(&mut b, ASLEEP + PERIOD - 1, 0xfe).is_err(), "touch must not wake the controller before its next scan");
    const WAKE: u64 = ASLEEP + PERIOD;
    assert_eq!(read::<5>(&mut b, WAKE, 2), Ok([1, 1, 144, 1, 223]), "wake scan must publish coordinates and permit I2C");
    advance(&mut b, WAKE + 699);
    assert!(!b.level(int), "configured pulse must stay low for 699 us");
    advance(&mut b, WAKE + 700);
    assert!(b.level(int), "IrqPluseWidth=7 must release INT at 700 us");
    advance(&mut b, WAKE + PERIOD);
    assert!(b.level(int), "EnChange alone must not pulse on an unchanged finger");
    touch(&mut b, WAKE + PERIOD, 400, 479, false);
    assert_eq!(read::<1>(&mut b, WAKE + PERIOD, 2), Ok([0]), "equal-tick stimulus must affect an already-processed idle scan");
    touch(&mut b, WAKE + PERIOD, 7, 8, true);
    touch(&mut b, WAKE + PERIOD, 7, 8, false);
    assert_eq!(read::<1>(&mut b, WAKE + 2 * PERIOD, 2), Ok([1]), "same-tick down must survive the prior release scan");
    assert_eq!(read::<1>(&mut b, WAKE + 3 * PERIOD, 2), Ok([0]), "same-tick up must survive the queued down scan");

    let mut notes = old_bus;
    notes.extend(first.1);
    b.take_bus(&mut notes);
    let texts: Vec<_> = notes.iter().filter_map(|ev| match ev { BusEvent::Note { what, .. } => Some(*what), _ => None }).collect();
    for (index, text) in texts.iter().enumerate() {
        assert!(!texts[..index].contains(text), "assumption note must appear only once despite resets: {text}");
    }
    for fragment in ["shorter than", "LongPressTime", "standby touch", "IOCtl stores", "NorScanPer write", "overlapping INT"] {
        assert!(texts.iter().any(|text| text.contains(fragment)), "exercised assumption must be noted: {fragment}");
    }
}

#[test]
fn undocumented_accesses_are_refused_by_name() {
    let mut b = board();
    assert!(unpointed(&mut b, BOOT).expect_err("unset pointer must refuse").contains("pointer"), "pointer refusal must explain the unknown default");
    for reg in [0x07, 0xa7, 0xa8, 0xa9, 0xff] {
        let error = read::<1>(&mut b, BOOT, reg).expect_err("unknown register read must refuse");
        assert!(error.contains(&format!("0x{reg:02X}")), "unknown read must name register {reg:#04x}");
        assert_eq!(write(&mut b, BOOT, reg, 0), Err(error), "unknown write must also refuse register {reg:#04x}");
    }
    assert!(read::<1>(&mut b, BOOT, 0xfd).expect_err("IOCtl needs a write").contains("0xFD"), "unwritten IOCtl must not invent a reset byte");
    assert!(read::<1>(&mut b, BOOT, 3).expect_err("initial coordinates unknown").contains("reset values undocumented"), "coordinates need a real sample");
    for (reg, value, fragment) in [(0xfa, 0x10, "EnMotion"), (0xfa, 1, "OnceWLP"),
        (0xfa, 0x80, "EnTest"), (0xfb, 1, "valid-gesture"), (0xed, 0, "0xED"),
        (0xed, 201, "0xED"), (0xee, 0, "0xEE"), (0xee, 31, "0xEE"), (2, 1, "0x02")]
    {
        assert!(write(&mut b, BOOT, reg, value).expect_err("unsupported setting must refuse").contains(fragment),
            "refusal must name unsupported setting {fragment}");
    }
    touch(&mut b, BOOT + 1, 0xfff, 0xfff, true);
    assert_eq!(read::<5>(&mut b, BOOT + PERIOD, 2), Ok([1, 15, 255, 15, 255]), "all 12 coordinate bits must survive without event bits");
    assert!(read::<6>(&mut b, BOOT + PERIOD, 2).expect_err("sixth byte is undocumented").contains("0x07"), "burst must refuse its undocumented sixth byte");
    assert!(b.input(BOOT + PERIOD, Stimulus::Touch { part: 0, x: 4096, y: 0, down: true }).is_err(),
        "unrepresentable coordinates must be refused, never truncated");
    assert!(Cst820::new(128).expect_err("address must fit seven bits").contains("addr"), "constructor must name invalid address");
}

use std::time::Instant;

use fab_elec_mcu_emu::board::BusEvent;
use fab_elec_mcu_emu::{HaltKind, Machine, PcSet, Pull, Ran, Status, Until};
use fab_elec_mcu_parts::{BoardDesc, NetDesc, Netlist, Part, PartDesc, PartKind, PinRef};
use fab_elec_mcu_s3::S3Machine;

mod support;
use support::{profile, Program, ENTRY, RAM};
use support::spi_fixture::*;

fn hash(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3))
}

fn frame(machine: &S3Machine<Netlist>) -> &[u16] {
    let Some(Part::Co5300(panel)) = machine.board().part(0) else {
        panic!("U1 remains a CO5300 panel");
    };
    panel.frame()
}

fn rows(machine: &S3Machine<Netlist>, first: usize, end: usize) -> Vec<u8> {
    (first..end).flat_map(|row| frame(machine)[row * 480..row * 480 + 368].iter()
        .flat_map(|pixel| pixel.to_be_bytes())).collect()
}

fn run_to(machine: &mut S3Machine<Netlist>, marks: &PcSet, stop: u32, bus: &mut Vec<BusEvent>) {
    loop {
        match machine.run(&Until { tick: 70_000_000, marks, stops: &[stop], stops_after: 0, on_drive: false }) {
            Ran::Pc => return,
            Ran::Barrier => {
                // A soft journal limit changes only where a mark is required.
                machine.take_bus(bus);
                machine.mark();
            }
            result => panic!("firmware must reach the requested SPI boundary: {result:?}, {:?}", machine.status()),
        }
    }
}

#[test]
fn spi2_dma_pushes_a_strip_into_the_co5300_and_rewinds() {
    let host_start = Instant::now();
    let desc = BoardDesc {
        seed: 0x5300,
        parts: vec![PartDesc { refdes: "U1".into(), kind: PartKind::Co5300 { glass: [0, 0, 480, 480] } }],
        nets: [("LCD_CS", "GPIO12", "CSX", Pull::Up),
            ("LCD_CLK", "GPIO11", "SCLK", Pull::None),
            ("LCD_D0", "GPIO4", "SIO0", Pull::None),
            ("LCD_D1", "GPIO5", "SIO1", Pull::None),
            ("LCD_D2", "GPIO6", "SIO2", Pull::None),
            ("LCD_D3", "GPIO7", "SIO3", Pull::None),
            ("LCD_RESET", "GPIO8", "RESX", Pull::Up)]
            .into_iter().map(|(name, chip, pin, pull)| NetDesc { rise_ps: None, name: name.into(), pull,
                pins: vec![PinRef::Chip(chip.into()), PinRef::Part { part: 0, pin: pin.into() }] }).collect(),
    };
    let mut p = Program::new();
    p.disable_watchdogs();
    p.spi_setup();
    p.spi_panel_reset();
    p.spi_transfer(0x11, &[], None);
    p.spi_wait();
    p.store(SPI_CLR, SPI_DONE);
    p.spi_delay_us(5_000);
    // C4h bit 7 (SPI_WRAM) lets SPI write frame RAM (CO5300 P23).
    for (cmd, bytes) in [(0xc4, &[0x80][..]), (0x3a, &[0x55][..]), (0x2a, &[0, 0, 1, 0x6f][..]), (0x2b, &[0, 0, 0, 63][..])] {
        p.spi_transfer(cmd, bytes, None);
        p.spi_wait();
        p.store(SPI_CLR, SPI_DONE);
    }
    let first = p.spi_transfer(0x2c, &[], Some(0));
    p.spi_wait();
    p.spi_result(RAM);
    let between = p.pc();
    let second = p.spi_transfer(0x3c, &[], Some(1));
    p.spi_wait();
    p.spi_result(RAM + 4);
    let end = p.pins_spin();
    let mut marks = PcSet::new(ENTRY, end - ENTRY + 3);
    for pc in [first, between, second, end] { marks.insert(pc); }
    let handoff = profile(0x535049);
    let mut machine = S3Machine::with_board(p.finish(), &handoff,
        Netlist::new(desc.clone()).expect("U1 and its seven connected pads form a valid board"))
        .expect("the panel board accepts the S3 handoff");
    let mut random = 0x5300_1234u32;
    let source: Vec<u8> = (0..368 * 64).flat_map(|_| {
        random = random.wrapping_mul(1664525).wrapping_add(1013904223);
        ((random >> 16) as u16).to_be_bytes()
    }).collect();
    assert_eq!(source.len(), 2 * HALF_BYTES, "the fixed LCG generates a 368 by 64 RGB565 strip");
    assert!(machine.seed_data(STRIP, &source), "the source strip is seeded in shared internal SRAM before running");
    let descriptors = descriptor_bytes();
    assert!(machine.seed_data(DESCRIPTORS, &descriptors), "both little-endian descriptor lists are seeded before running");
    let pre_second = rows(&machine, 32, 64);
    let mut bus = Vec::new();
    let mut trace = Vec::new();
    run_to(&mut machine, &marks, between, &mut bus);
    assert_eq!(rows(&machine, 0, 32), source[..HALF_BYTES], "RAMWR stores the first half-strip before the rewind mark");
    machine.take_bus(&mut bus);
    machine.take_trace(&mut trace);
    machine.mark();
    let committed_bus = bus.len();
    let marked_tick = machine.now();
    let marked_instret = machine.cpu().instret;
    run_to(&mut machine, &marks, end, &mut bus);
    let simulated_instructions = machine.cpu().instret;
    let replay_instructions = simulated_instructions - marked_instret;
    assert_eq!(machine.status(), Status::Running, "SPI/GDMA panel firmware completes without an Unmodeled stop");
    let expected_hash = hash(source.iter().copied());
    assert_eq!(hash(rows(&machine, 0, 64)), expected_hash, "all 64 written rows hash to the source's big-endian RGB565 bytes");
    machine.take_bus(&mut bus);
    let strips: Vec<_> = bus.iter().filter_map(|event| match *event {
        BusEvent::Spi { t0, t1, len, lines, bit_ticks, hash, .. } if len as usize == HALF_BYTES =>
            Some((t0, t1, len, lines, bit_ticks, hash)),
        _ => None,
    }).collect();
    assert_eq!(strips.len(), 2, "RAMWR and RAMWRC produce exactly two strip BusEvent::Spi records");
    for (n, &(t0, t1, len, lines, bit_ticks, payload_hash)) in strips.iter().enumerate() {
        assert_eq!(len as usize, HALF_BYTES, "strip transfer {n} carries exactly 32 rows below 32 KiB (S03)");
        assert_eq!(lines, [1, 1, 4], "strip transfer {n} uses the 0x32 instruction's one/one/four lanes");
        assert_eq!(bit_ticks, 12, "strip transfer {n} runs at PLL_F80M divided to 40 MHz (S08)");
        assert_eq!(t1 - t0, (8 + 24 + HALF_BYTES as u64 * 2 - 1) * 12,
            "strip transfer {n} spans its command, address and quad data latch edges (S10/S12)");
        assert_eq!(payload_hash, hash(source[n * HALF_BYTES..(n + 1) * HALF_BYTES].iter().copied()),
            "strip transfer {n} hashes the exact descriptor buffer bytes");
    }
    let cs = machine.board().net("LCD_CS").expect("the board exposes the CSX net");
    let first_rise = bus.iter().find_map(|event| match *event {
        BusEvent::Level { t, net, high: true } if net == cs && t > strips[0].1 => Some(t),
        _ => None,
    }).expect("the first strip has a CSX rising edge after its last clock");
    assert_eq!(first_rise, strips[0].1 + 6, "CS hold is one half of a 40 MHz clock (S10)");
    let second_fall = bus.iter().find_map(|event| match *event {
        BusEvent::Level { t, net, high: false } if net == cs && t > first_rise => Some(t),
        _ => None,
    }).expect("the second strip has a distinct CSX falling edge");
    assert!(second_fall > first_rise, "RAMWRC starts after RAMWR's CSX rising edge");
    assert_eq!(second_fall + 6, strips[1].0, "the second strip's first edge follows half a clock of CS setup (S10)");
    machine.take_trace(&mut trace);
    let starts: Vec<_> = trace.iter().filter(|(_, pc)| *pc == first || *pc == second).copied().collect();
    assert_eq!(starts.len(), 2, "firmware sets USR once for each strip");
    assert_eq!((starts[0].0 + 6, starts[1].0), (strips[0].0, second_fall),
        "SPI asserts CS at each firmware USR store tick with no startup overhead (Q03)");
    let mut done = [0; 8];
    assert!(machine.read_data(RAM, &mut done), "the two firmware completion poll results are readable");
    assert_eq!([u32::from_le_bytes(done[..4].try_into().unwrap()), u32::from_le_bytes(done[4..].try_into().unwrap())],
        [SPI_DONE, SPI_DONE],
        "both distinct strip polls observed TRANS_DONE after clearing between transfers");
    let mut owners = vec![0; descriptors.len()];
    assert!(machine.read_data(DESCRIPTORS, &mut owners), "completed descriptors remain readable in SRAM");
    assert!(owners.chunks_exact(12).all(|d| d[3] & 0x80 == 0), "OUT_AUTO_WRBACK clears every completed descriptor owner (G06)");

    machine.rewind();
    assert_eq!(machine.now(), marked_tick, "rewind restores the boundary between the two transfers");
    assert_eq!(rows(&machine, 32, 64), pre_second, "rewind restores the second half's pre-strip frame memory");
    assert_eq!(rows(&machine, 0, 32), source[..HALF_BYTES], "rewind retains the committed first half-strip");
    assert!(machine.read_data(DESCRIPTORS, &mut owners), "rewound descriptor owner words remain readable");
    assert!(owners.chunks_exact(12).skip(DESCRIPTORS_PER_HALF).all(|d| d[3] & 0x80 != 0),
        "rewind restores GDMA ownership of the second descriptor list");
    bus.truncate(committed_bus);
    run_to(&mut machine, &marks, end, &mut bus);
    assert_eq!(hash(rows(&machine, 0, 64)), expected_hash, "replaying RAMWRC restores the identical final panel hash");
    assert_eq!(machine.cpu().instret, simulated_instructions, "replay retires the same deterministic instruction count");

    // The in-flight guard: a CPU store into the strip while its GDMA transfer runs stops the run
    // (which bytes go out is undocumented), before the transfer reaches the panel.
    let mut p = Program::new();
    p.disable_watchdogs();
    p.spi_setup();
    p.spi_transfer(0x2c, &[], Some(0));
    let racing = p.store(STRIP + 64, 0);
    p.pins_spin();
    let mut racer = S3Machine::with_board(p.finish(), &handoff,
        Netlist::new(desc).expect("the same board accepts the racing firmware"))
        .expect("the panel board accepts the S3 handoff");
    assert!(racer.seed_data(STRIP, &source) && racer.seed_data(DESCRIPTORS, &descriptors),
        "the racing run seeds the same strip and descriptors");
    let ran = racer.run(&Until { tick: 70_000_000, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false });
    assert_eq!(ran, Ran::Halted, "a CPU store into an in-flight GDMA buffer stops the run");
    let Status::Halted(halt) = racer.status() else { panic!("the guard's stop stays inspectable") };
    assert_eq!((halt.kind, halt.what, halt.pc), (HaltKind::Unmodeled,
        "CPU store into a GDMA descriptor or buffer during its transfer (S05, G03)", racing),
        "the guard names the race at the racing store");
    let mut racer_bus = Vec::new();
    racer.take_bus(&mut racer_bus);
    assert!(!racer_bus.iter().any(|event| matches!(event, BusEvent::Spi { .. })),
        "the raced transfer never reaches the panel");

    eprintln!("spi2 strip: {simulated_instructions} simulated instructions; {replay_instructions} replay instructions; host {:?}", host_start.elapsed());
}

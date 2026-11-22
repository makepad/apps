//! A short real SPI program: timer-driven reset waits, two pixels, then WAITI.

use std::sync::Arc;

use fab_elec_mcu_cosim::{CoMachine, Config, FwState, LogicMachine, LogicPlan};
use fab_elec_mcu_emit32::xtensa::enc;
use fab_elec_mcu_emu::board::BusEvent;
use fab_elec_mcu_emu::{time, Machine, PcSet, Pull, Ran, Status, Until};
use fab_elec_mcu_parts::{BoardDesc, BoardPorts, NetDesc, Netlist, Part, PartDesc, PartKind, PinRef, Stimulus};
use fab_elec_mcu_s3::S3Machine;
use fab_elec_sim::cosim::{Bus, CoRun, CoSim};
use fab_elec_sim::{PinMode, SimDiag};

mod support;
use support::{pin_reg, profile, Program, ENTRY, GPIO_BASE, IRAM, RAM};
use support::spi_fixture::{SPI_CLR, SPI_DONE};

fn board() -> Netlist {
    let mut nets: Vec<_> = [("CS", "GPIO12", "CSX", Pull::Up),
        ("CLK", "GPIO11", "SCLK", Pull::None), ("D0", "GPIO4", "SIO0", Pull::None),
        ("D1", "GPIO5", "SIO1", Pull::None), ("D2", "GPIO6", "SIO2", Pull::None),
        ("D3", "GPIO7", "SIO3", Pull::None), ("RESET", "GPIO8", "RESX", Pull::Up)]
        .into_iter().map(|(name, chip, pin, pull)| NetDesc { rise_ps: None, name: name.into(), pull,
            pins: vec![PinRef::Chip(chip.into()), PinRef::Part { part: 0, pin: pin.into() }] }).collect();
    nets[6].pins.push(PinRef::Part { part: 1, pin: "A".into() });
    Netlist::new(BoardDesc { seed: 0x5300,
        parts: vec![PartDesc { refdes: "U1".into(), kind: PartKind::Co5300 { glass: [0, 0, 480, 480] } },
            PartDesc { refdes: "SW1".into(), kind: PartKind::Key }], nets }).unwrap()
}

fn sleep(p: &mut Program, cycles: u32) {
    p.sr(234, 0); // CCOUNT, with one cycle charged to this instruction.
    p.sr(240, cycles); // CCOMPARE0; its handler clears the latch and returns.
    p.emit(enc::waiti(0));
}

fn image() -> (Arc<Vec<u8>>, u32, Vec<u8>) {
    let mut p = Program::new();
    p.disable_watchdogs();
    p.spi_setup();
    p.store(pin_reg(8), 1 << 2); // Open drain: the reset key and GPIO can both sink.
    p.sr(231, IRAM);
    p.sr(230, 0);
    p.sr(228, 1 << 6); // The core's timer0 line.
    p.store(GPIO_BASE + 0x0c, 1 << 8);
    sleep(&mut p, 2_000); // 25 us > the panel's 10 us reset pulse.
    p.store(GPIO_BASE + 0x08, 1 << 8);
    sleep(&mut p, 400_100); // > 5 ms at 80 MHz, without an instruction loop.
    // C4h bit 7 (SPI_WRAM) lets SPI write frame RAM (CO5300 P23).
    for (cmd, bytes) in [(0xc4, &[0x80][..]), (0x3a, &[0x55][..]), (0x2a, &[0, 0, 0, 1][..]), (0x2b, &[0, 0, 0, 1][..])] {
        p.spi_transfer(cmd, bytes, None);
        p.spi_wait();
        p.store(SPI_CLR, SPI_DONE);
    }
    let start = p.spi_transfer(0x2c, &[0xf8, 0, 0x07, 0xe0], None);
    p.spi_wait();
    p.sr(228, 0);
    p.emit(enc::waiti(0));
    p.halt();
    let handler = [enc::movi(15, 0), enc::rsr(0, 3, 1, 240, 15), enc::rfe()]
        .into_iter().flat_map(|word| word.to_le_bytes()[..3].to_vec()).collect();
    (p.finish(), start, handler)
}

fn machine(image: Arc<Vec<u8>>, handler: &[u8], board: Netlist) -> S3Machine<Netlist> {
    let mut m = S3Machine::with_board(image, &profile(0x5300), board).unwrap();
    assert!(m.seed_data(RAM + 0x300, handler), "the timer vector is ordinary SRAM code");
    m
}

fn advance(m: &mut S3Machine<Netlist>, tick: u64) {
    assert_eq!(m.run(&Until { tick, marks: &PcSet::default(), stops: &[], stops_after: 0,
        on_drive: false }), Ran::Tick, "short panel firmware runs: {:?}", m.status());
}

fn pixels(m: &S3Machine<Netlist>) -> &[u16] {
    let Some(Part::Co5300(panel)) = m.board().part(0) else { panic!("panel exists") };
    panel.frame()
}

fn hash(m: &S3Machine<Netlist>) -> u64 {
    pixels(m).iter().flat_map(|pixel| pixel.to_be_bytes())
        .fold(0xcbf29ce484222325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3))
}

fn commit(m: &mut S3Machine<Netlist>, bus: &mut Vec<BusEvent>) {
    m.take_bus(bus);
    m.mark();
}

#[test]
fn live_inputs_replay_with_other_marks_at_the_spi_completion_tick() {
    let (image, start, handler) = image();
    let mut b = board();
    let mut ports = b.ports();
    let mut live = machine(image.clone(), &handler, b);
    let mut marks = PcSet::new(ENTRY, start - ENTRY + 1);
    marks.insert(start);
    assert_eq!(live.run(&Until { tick: 3_000_000, marks: &marks, stops: &[start], stops_after: 0,
        on_drive: false }), Ran::Pc, "firmware reaches the RAMWR USR store");
    // S10/S12: CS setup = 6 ticks; last of 32 header + 32 data clocks, period 12.
    let done = live.now() + 6 + 63 * 12;
    let mut live_bus = Vec::new();
    commit(&mut live, &mut live_bus);
    advance(&mut live, done);
    assert_eq!(live.now(), done, "the publication is exactly the transfer's final edge");
    assert_eq!(&pixels(&live)[..2], &[0xf800, 0x07e0]);
    assert_eq!(ports.screens[0].1.latest().generation, 0, "marks never publish");
    let down = Stimulus::Key { part: 1, down: true };
    let up = Stimulus::Key { part: 1, down: false };
    commit(&mut live, &mut live_bus); // The sync commit.
    assert!(live.host_input(&down));
    assert!(live.host_input(&up));
    let log = live.inputs().to_vec();
    assert_eq!(log, [(done, down.clone()), (done, up.clone())], "equal-tick stimuli retain insertion order before commit");
    commit(&mut live, &mut live_bus);
    live.publish(0.125);
    let committed = ports.screens[0].1.latest().clone();
    assert_eq!((committed.t, &committed.bgra[..2]), (0.125, &[0xffff0000, 0xff00ff00][..]));
    advance(&mut live, done + 500);
    assert!(live.host_input(&down));
    advance(&mut live, done + 537);
    assert!(live.host_input(&up));
    let log = live.inputs().to_vec();
    assert_eq!(log, [(done, down.clone()), (done, up.clone()), (done + 500, down), (done + 537, up)],
        "live input records each chosen board tick");
    advance(&mut live, done + 1000);
    commit(&mut live, &mut live_bus);

    let mut replay = machine(image, &handler, board()); // No ports made.
    replay.install_input_schedule(log.clone()).unwrap();
    let mut replay_bus = Vec::new();
    assert_eq!(replay.inputs(), log.as_slice(), "inputs includes every installed entry before execution");
    // Rewind a speculative application: the schedule survives and its cursor rewinds.
    for tick in [97, done - 24] {
        advance(&mut replay, tick);
        commit(&mut replay, &mut replay_bus);
    }
    advance(&mut replay, done + 6);
    assert_eq!(replay.inputs(), log.as_slice(), "speculation never changes the schedule");
    replay.rewind();
    assert_eq!(replay.inputs(), log.as_slice(), "rewind never truncates entries");
    for tick in [done + 18, done + 333, done + 1000] {
        advance(&mut replay, tick);
        commit(&mut replay, &mut replay_bus);
    }
    assert_eq!(replay.inputs(), log.as_slice(), "replay never duplicates entries");
    assert_eq!(hash(&replay), hash(&live), "other marks reproduce every panel pixel");
    assert_eq!(replay_bus, live_bus, "chip completion precedes reset-key input at equal ticks");
    assert_eq!(replay.status(), live.status());
}

struct Analog { t: f64 }
impl Bus for Analog {
    fn t(&self) -> f64 { self.t }
    fn v(&self, _pin: u32) -> f64 { f64::NAN }
    fn supply(&self, _pin: u32) -> f64 { f64::NAN }
    fn advance(&mut self, t: f64) -> Result<(), SimDiag> { self.t = t; Ok(()) }
    fn write(&mut self, _pin: u32, _mode: PinMode) -> bool { false }
    fn hits(&self) -> &[(u32, f64)] { &[] }
}

#[test]
fn state_publishes_the_final_panel_and_new_devices_have_fresh_ports() {
    let (image, _start, handler) = image();
    let mut b = board();
    let BoardPorts { mut screens } = b.ports();
    let m = machine(image.clone(), &handler, b);
    let mut dev = CoMachine::new(m, Config { stretch: 1_000_000, debug: None }, &[]).unwrap();
    let mut analog = Analog { t: 0.0 };
    assert_eq!(dev.run(&mut analog).unwrap(), CoRun::Go);
    assert_eq!(screens[0].1.latest().generation, 0, "attach/lookahead marks do not publish");
    let paused = 3_000_000;
    analog.t = time(0.0, paused, dev.machine().tick_hz());
    assert!(dev.state(analog.t).is_some(), "the worker's final state call commits and publishes");
    let final_frame = screens[0].1.latest().clone();
    assert_eq!(final_frame.t, analog.t);
    assert_eq!(&final_frame.bgra[..2], &[0xffff0000, 0xff00ff00]);
    assert!(matches!(dev.machine().status(), Status::Sleeping { .. }));
    // No reader request or later machine work is needed to open the pane on a paused run.
    assert_eq!(screens[0].1.latest(), &final_frame);

    let down = Stimulus::Key { part: 1, down: true };
    let up = Stimulus::Key { part: 1, down: false };
    assert!(dev.input(analog.t, &down));
    analog.t = time(0.0, paused + 37, dev.machine().tick_hz());
    assert!(dev.input(analog.t, &up));
    assert_eq!(dev.machine().inputs(), &[(paused, down.clone()), (paused + 37, up)],
        "timed input synchronizes before stamping, independent of publication");
    assert_eq!(screens[0].1.latest(), &final_frame, "input commits without publishing");
    assert!(dev.state(analog.t).is_some());
    assert_eq!(dev.machine().inputs().len(), 2, "state exposes the schedule through its device");
    assert!(dev.state(analog.t).is_some());
    assert_eq!(dev.machine().inputs().len(), 2, "host reads and empty publications never duplicate input");

    let mut b = board();
    let mut fresh = b.ports();
    let mut next = machine(image, &handler, b);
    assert!(next.host_input(&down));
    assert!(dev.state(analog.t).is_some());
    assert_eq!(dev.machine().inputs().len(), 2, "new-device input cannot reach the old machine");
    assert_eq!(fresh.screens[0].1.latest().generation, 0, "the old worker cannot publish into the new panel");
    next.mark();
    next.publish(0.0);
    assert_eq!(next.inputs(), &[(0, down)]);
    assert_ne!(fresh.screens[0].1.latest().bgra, final_frame.bgra, "the new device starts with its own unwritten panel");
    assert_eq!(screens[0].1.latest().bgra, final_frame.bgra, "a new device cannot overwrite the old reader");
}

#[test]
fn boardless_machine_rejects_host_input_without_changing_execution() {
    let mut p = Program::new();
    p.disable_watchdogs();
    p.time_sleep();
    let m = S3Machine::new(p.finish(), &profile(0x5300));
    let cfg = Config { stretch: 10_000, debug: None };
    let mut reference = CoMachine::new(m.clone(), cfg.clone(), &[]).unwrap();
    let mut dev = CoMachine::new(m, cfg, &[]).unwrap();
    let mut analog = Analog { t: 0.0 };
    assert_eq!(reference.run(&mut analog).unwrap(), CoRun::Go);
    assert_eq!(dev.run(&mut analog).unwrap(), CoRun::Go);
    for tick in [197, 10_001, 13_003] {
        analog.t = time(0.0, tick, dev.machine().tick_hz());
        assert!(!dev.input(analog.t, &Stimulus::Key { part: 0, down: true }));
        assert!(dev.machine().inputs().is_empty(), "a type mismatch never enters the schedule");
        let expected = reference.state(analog.t).unwrap();
        let actual = dev.state(analog.t).unwrap();
        let expected = expected.get::<FwState>().unwrap();
        let actual = actual.get::<FwState>().unwrap();
        assert_eq!((actual.cycle, actual.pc, actual.at), (expected.cycle, expected.pc, expected.at),
            "rejected input preserves the instruction boundary and execution state");
        assert_eq!(actual.regs, expected.regs, "rejected input preserves registers");
        assert_eq!(actual.timers, expected.timers, "rejected input preserves peripheral timing");
        assert_eq!(actual.pending, expected.pending, "rejected input preserves pending interrupts");
        assert_eq!(actual.wake, expected.wake, "rejected input preserves wake timing");
        assert_eq!(dev.next(), reference.next(), "input synchronization keeps the existing stretch pace");
    }
}

#[test]
fn logic_wrapper_forwards_input_and_committed_publication() {
    let (image, _start, handler) = image();
    let mut b = board();
    let mut ports = b.ports();
    let mut m = LogicMachine::new(machine(image, &handler, b), LogicPlan::default()).unwrap();
    let mut bus = Vec::new();
    m.take_bus(&mut bus);
    bus.clear();
    assert!(!m.host_input(&0u32), "the wrapper preserves rejection of another input type");
    assert!(m.host_input(&Stimulus::Key { part: 1, down: true }));
    m.take_bus(&mut bus);
    assert_eq!(bus, [BusEvent::Level { t: 0, net: 6, high: false }],
        "the wrapped board receives the input at the current tick");
    m.mark();
    assert_eq!(ports.screens[0].1.latest().generation, 0, "a wrapper mark never publishes");
    m.publish(0.0);
    assert_eq!(ports.screens[0].1.latest().generation, 1, "publication reaches the inner panel");
}

// A timed board reaction at the same tick as input, including a zero-delay reaction
// to the down. The real chip-block/CO5300 tie is covered by the SPI test above.
#[derive(Clone)]
struct TimedBoard {
    nets: [fab_elec_mcu_emu::board::NetInfo; 1],
    due: Option<u64>,
    saved_due: Option<u64>,
    bus: Vec<BusEvent>,
    saved_len: usize,
}

impl fab_elec_mcu_emu::board::Board for TimedBoard {
    type In = Stimulus;
    fn bind(&mut self, _pins: &[&'static str], _hz: u64) -> Result<(), String> { Ok(()) }
    fn pad_drive(&mut self, _t: u64, _pad: u16, _drive: fab_elec_mcu_emu::Drive) -> fab_elec_mcu_emu::board::Res { Ok(()) }
    fn pad_inputs(&mut self, _out: &mut Vec<(u16, bool)>) {}
    fn chip_reset(&mut self, _t: u64) -> fab_elec_mcu_emu::board::Res { Ok(()) }
    fn i2c(&mut self, _t: u64, _scl: u16, _sda: u16, _op: fab_elec_mcu_emu::board::I2cOp)
        -> fab_elec_mcu_emu::board::Res<fab_elec_mcu_emu::board::I2cAns> { Err("no I2C target") }
    fn spi(&mut self, _x: &fab_elec_mcu_emu::board::SpiXfer, _tx: &dyn fab_elec_mcu_emu::board::Payload,
        _rx: &mut Vec<u8>) -> fab_elec_mcu_emu::board::Res { Err("no SPI target") }
    fn next_event(&self) -> Option<u64> { self.due }
    fn event(&mut self, t: u64) -> fab_elec_mcu_emu::board::Res {
        assert_eq!(self.due, Some(t));
        self.due = None;
        self.bus.push(BusEvent::Note { t, what: "board reaction" });
        Ok(())
    }
    fn input(&mut self, t: u64, ev: Stimulus) -> fab_elec_mcu_emu::board::Res {
        assert_eq!(self.due, None, "all board events at this tick precede the next input");
        let Stimulus::Key { part, down } = ev else { panic!("key fixture") };
        self.bus.push(BusEvent::Level { t, net: part, high: !down });
        if down { self.due = Some(t); }
        Ok(())
    }
    fn mark(&mut self) { self.saved_due = self.due; self.saved_len = self.bus.len(); }
    fn rewind(&mut self) { self.due = self.saved_due; self.bus.truncate(self.saved_len); }
    fn barrier(&self) -> bool { false }
    fn nets(&self) -> &[fab_elec_mcu_emu::board::NetInfo] {
        &self.nets
    }
    fn take_bus(&mut self, out: &mut Vec<BusEvent>) { out.append(&mut self.bus); }
    fn publish(&mut self, _t: f64) {}
}

#[test]
fn board_events_and_zero_delay_reactions_precede_equal_tick_inputs() {
    let mut p = Program::new();
    p.disable_watchdogs();
    p.time_sleep();
    let image = p.finish();
    let tick = 10_001; // Intentionally between CPU cycles; WAITI lands exactly here.
    let b = TimedBoard {
        nets: [fab_elec_mcu_emu::board::NetInfo { name: "KEY".into(), kind: fab_elec_mcu_emu::board::NetKind::Level }],
        due: Some(tick), saved_due: None, bus: Vec::new(), saved_len: 0,
    };
    let mut live = S3Machine::with_board(image.clone(), &profile(0x5300), b.clone()).unwrap();
    let mut replay = S3Machine::with_board(image, &profile(0x5300), b).unwrap();
    let down = Stimulus::Key { part: 0, down: true };
    let up = Stimulus::Key { part: 0, down: false };
    let run = |m: &mut S3Machine<TimedBoard>, t| {
        assert_eq!(m.run(&Until { tick: t, marks: &PcSet::default(), stops: &[],
            stops_after: 0, on_drive: false }), Ran::Tick);
    };
    run(&mut live, tick);
    live.mark();
    assert!(live.host_input(&down));
    assert!(live.host_input(&up));
    let log = live.inputs().to_vec();
    live.rewind();
    assert_eq!(live.inputs(), log.as_slice(), "live entries added after the mark survive rewind");
    run(&mut live, tick);
    live.mark();
    replay.install_input_schedule(log).unwrap();
    run(&mut replay, tick - 1);
    replay.mark();
    run(&mut replay, tick + 1);
    replay.rewind();
    run(&mut replay, tick + 2);
    replay.mark();
    let mut live_bus = Vec::new();
    let mut replay_bus = Vec::new();
    live.take_bus(&mut live_bus);
    replay.take_bus(&mut replay_bus);
    assert_eq!(replay_bus, live_bus, "mark placement cannot change board/input order");
    assert_eq!(replay_bus, [BusEvent::Note { t: tick, what: "board reaction" },
        BusEvent::Level { t: tick, net: 0, high: false },
        BusEvent::Note { t: tick, what: "board reaction" },
        BusEvent::Level { t: tick, net: 0, high: true }]);
    assert_eq!(replay.inputs(), live.inputs());
    let mut snapshot = live.clone();
    assert_eq!(snapshot.inputs(), live.inputs(), "a snapshot copies the complete schedule");
    run(&mut snapshot, tick + 3);
    let mut snapshot_bus = Vec::new();
    snapshot.take_bus(&mut snapshot_bus);
    assert!(snapshot_bus.is_empty(), "a snapshot copies the cursor and never reapplies consumed input");
}

#[test]
fn halted_machine_records_input_without_applying_or_resuming() {
    let mut p = Program::new();
    p.halt();
    let b = TimedBoard {
        nets: [fab_elec_mcu_emu::board::NetInfo { name: "KEY".into(), kind: fab_elec_mcu_emu::board::NetKind::Level }],
        due: None, saved_due: None, bus: Vec::new(), saved_len: 0,
    };
    let mut m = S3Machine::with_board(p.finish(), &profile(0x5300), b).unwrap();
    let until = Until { tick: 10_000, marks: &PcSet::default(), stops: &[],
        stops_after: 0, on_drive: false };
    assert_eq!(m.run(&until), Ran::Halted);
    let (tick, status) = (m.board_time(), m.status());
    m.mark();
    let down = Stimulus::Key { part: 0, down: true };
    assert!(m.host_input(&down), "a halted machine still accepts its board's input type");
    assert_eq!(m.inputs(), &[(tick, down.clone())], "halted input is recorded immediately");
    assert!(m.board().bus.is_empty(), "halted input cannot reach the board");
    m.rewind();
    assert_eq!(m.inputs(), &[(tick, down)], "rewind retains even unapplied input");
    assert_eq!(m.run(&until), Ran::Halted, "input cannot resume a latched halt");
    assert_eq!((m.board_time(), m.status()), (tick, status), "input never changes the halt or its tick");
    assert!(m.board().bus.is_empty(), "a later run cannot apply input while halted");
}

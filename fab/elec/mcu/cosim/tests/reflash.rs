//! A new image flashed into the running chip keeps its EEPROM: the live run's attach of the new
//! process carries the one it replaces' non-volatile memory as committed then
//! (`CoSim::nonvolatile`, `set_nonvolatile`), and a machine takes stored bytes into its EEPROM
//! only (`Machine::eeprom_bytes`, `load_eeprom`), never into the firmware's state.

use fab_elec_mcu_avr::machine::AvrMachine;
use fab_elec_mcu_cosim::{CoMachine, Config, FwState};
use fab_elec_mcu_emit_avr::Firmware;
use fab_elec_mcu_emu::{FwValue, Machine};
use fab_elec_sim::cosim::{CoSimDevice, CoSimPin};
use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::{GpioParams, PinMode, PinPull, SimCircuit, Sink, Source};
use std::sync::Arc;

const HZ: u64 = 16_000_000;
const PARAMS: GpioParams = GpioParams { r_on_h: 26.0, r_on_l: 23.5, r_pull: 36e3, r_leak: 5.5e6, t_edge: 5e-9, vih: 0.524, vil: 0.418 };

/// Stores a byte at boot; an EEPROM write takes about 3.3 ms to commit.
const WRITER: &str = "\
let led = output(PB5)
fn setup() {
    eeprom_write(3, 42)
}
";

/// Stores nothing: what it reads at boot is what the chip kept.
const READER: &str = "\
let led = output(PB5)
var kept: i32 = 0
fn setup() {
    kept = eeprom_read(3)
}
";

struct Quiet;
impl Sink for Quiet {}

fn build(source: &str) -> Firmware {
    fab_elec_mcu_emit_avr::firmware(source, HZ as f64, &[]).expect("the firmware builds")
}

fn device(fw: &Firmware) -> CoSimDevice {
    let pins = vec![CoSimPin { name: "PB5".into(), gpio: "u1#PB5.io".into(), pad: "led".into(), vss: "0".into(), vdd: "vdd".into(), vih: PARAMS.vih, vil: PARAMS.vil }];
    let dev = CoMachine::new(AvrMachine::new(&fw.words, HZ), Config { stretch: 16_000, debug: Some(Arc::new(fw.debug.clone())) }, &pins).expect("the co-simulated MCU");
    CoSimDevice { dev: Box::new(dev), pins }
}

/// A 5 V supply and the LED pin's load, with `fw`'s chip attached.
fn live(fw: &Firmware) -> LiveSim {
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(5.0));
    c.gpio("u1#PB5.io", "led", "vdd", "0", PARAMS, PinMode::Input(PinPull::None));
    c.r("load", "led", "0", 10e3);
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 50e-6, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the rig's operating point converges");
    sim.attach(Some(device(fw)));
    sim
}

fn run_to(sim: &mut LiveSim, t: f64) {
    sim.apply(LiveCmd::Step(t - sim.t()));
    let mut still = 0;
    while sim.t() < t {
        let before = sim.t();
        sim.advance(f64::INFINITY, None, &mut Quiet).unwrap_or_else(|e| panic!("the run goes on: {}", e.message));
        still = if sim.t() > before { 0 } else { still + 1 };
        assert!(still < 1000, "the run moves (stuck at {before} s)");
    }
}

/// The published device state: its EEPROM byte 3 and the reader's `kept`, when it has one.
fn read(sim: &mut LiveSim) -> (u8, Option<FwValue>) {
    let values = sim.values(1.0);
    let state = values.co.as_ref().and_then(|co| co.get::<FwState>()).expect("the device publishes its state");
    let eeprom = state.eeprom.as_ref().expect("the chip publishes its EEPROM");
    assert_eq!(eeprom.len(), 1024, "the whole EEPROM is published");
    (eeprom[3], state.vars.first().cloned())
}

#[test]
fn a_reflash_keeps_the_eeprom_and_stored_bytes_reach_only_the_eeprom() {
    // The machine: stored bytes of its size go into its EEPROM and nowhere else; others change nothing.
    let (writer, reader) = (build(WRITER), build(READER));
    let mut chip = AvrMachine::new(&reader.words, HZ);
    let blank = chip.eeprom_bytes().expect("the chip has an EEPROM");
    assert!(blank.iter().all(|&b| b == 0xFF), "a new chip's EEPROM is erased");
    assert!(Arc::ptr_eq(&blank, &chip.eeprom_bytes().unwrap()), "the bytes are shared, not copied");
    assert!(!chip.load_eeprom(&[0; 512]), "a short image is refused");
    assert!(Arc::ptr_eq(&blank, &chip.eeprom_bytes().unwrap()), "a refused image changes nothing");
    let (mut regs, mut regs_after) = (Vec::new(), Vec::new());
    chip.regs(&mut regs);
    let (mut ram, mut ram_after) = (vec![0; 256], vec![0; 256]);
    assert!(chip.read_data(0x100, &mut ram), "the chip's RAM reads");
    let stored: Vec<u8> = (0..1024).map(|i| (i * 7) as u8).collect();
    assert!(chip.load_eeprom(&stored), "an image of the chip's size loads");
    assert_eq!(&chip.eeprom_bytes().unwrap()[..], &stored[..], "the EEPROM holds the stored bytes");
    chip.regs(&mut regs_after);
    assert!(chip.read_data(0x100, &mut ram_after), "the chip's RAM reads");
    assert_eq!((regs, ram), (regs_after, ram_after), "loading the EEPROM leaves the registers and RAM as they were");

    // A run: the writer's byte commits; a reader flashed into the running chip finds it.
    let mut sim = live(&writer);
    run_to(&mut sim, 4.5e-3);
    assert_eq!(read(&mut sim).0, 42, "the writer's byte is committed");
    sim.attach(Some(device(&reader)));
    run_to(&mut sim, 6e-3);
    assert_eq!(read(&mut sim), (42, Some(FwValue::I32(42))), "the reflashed chip keeps its EEPROM and its firmware reads it");

    // The same reader on a new chip: the byte came from the reflash, not from the reader.
    let mut fresh = live(&reader);
    run_to(&mut fresh, 1.5e-3);
    assert_eq!(read(&mut fresh), (0xFF, Some(FwValue::I32(255))), "a new chip reads its erased EEPROM");
}

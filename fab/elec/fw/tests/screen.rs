//! A lowered panel keeps its reader through two edits back to the same live snapshot.

use fab_elec_bench::bench::Supply;
use fab_elec_bench::{lower_circuit, BenchSpec, Firmware, ModelLibs};
use fab_elec_circuit::{Circuit, Component, Net, NetKind, QtyUnit, Quantity, Value};
use fab_elec_core::{BlockId, BoardId, ClassId, CompId, CompRef, NetId, NetRef, PinIx};
use fab_elec_fw::{compile, maker, s3_handoff, Built, Chip, FwInput};
use fab_elec_mcu_cosim::{CoMachine, Config, FwFrame};
use fab_elec_mcu_emit_s3::wiring;
use fab_elec_mcu_parts::screen::{ScreenFrame, ScreenReader};
use fab_elec_mcu_parts::Netlist;
use fab_elec_mcu_s3::S3Machine;
use fab_elec_parts::{Fields, GpioCard, PartDef, PartKind, PinDef, PinKind, Symbol};
use fab_elec_sim::cosim::CoSimDevice;
use fab_elec_sim::live::LiveCmd;
use fab_elec_sim::livesim::{LiveSim, LiveSpec, Snapshots};
use fab_elec_sim::Sink;
use fab_elec_simhost::events::{EventLog, EventSource};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const GLASS: [usize; 4] = [16, 12, 8, 8];
const RED: u32 = 0xffff_0000;
const BLUE: u32 = 0xff00_00ff;
const END: f64 = 3.0;

// Restart the one-shot from setup's timestamp, after the real panel initialization waits.
const SOURCE: &str = r#"
let screen = display()
let change = after(ms(100), fn() {
    fill_rect(screen, 0, 0, display_w, display_h, rgb(0, 0, 255))
})
fn setup() {
    fill_rect(screen, 0, 0, display_w, display_h, rgb(255, 0, 0))
    start(change)
}
"#;

fn component(id: u32, path: &str, sim: &str, pins: &[(&str, u32)]) -> Component {
    Component {
        id: CompId(id), path: path.into(), refdes: path.into(), ref_kind: Default::default(),
        part: Arc::new(PartDef {
            name: path.into(), kind: PartKind::Ic, symbol: Symbol::default(), footprint: None,
            body: None, emitter: None, gpio: None, lifecycle: None, approved: false, fields: Fields(vec![("sim".into(), sim.into())]),
            pins: pins.iter().map(|&(name, _)| PinDef {
                name: name.into(), pads: Vec::new(), unit: 0, limits: None, alternates: Vec::new(), delay: None,
                kind: if ["VDD", "GND"].contains(&name) { PinKind::PowerIn } else { PinKind::Bidi },
            }).collect(),
        }),
        value: None, conns: pins.iter().enumerate().map(|(i, &(_, n))| (PinIx(i as u16), NetId(n))).collect(),
        nc: Vec::new(), block: BlockId(0), dnp: false, fields: Fields::default(),
    }
}

fn value(comp: &mut Component, si: f64, unit: QtyUnit) {
    comp.value = Some(Value { text: si.to_string(), qty: Some(Quantity { si, unit }), tol: None });
}

fn card() -> GpioCard {
    GpioCard { r_high: 25.0, r_low: 25.0, pull_up: 30e3, vih: 0.7, vil: 0.3, leak: 1e-6, edge: None, c_pin: Some(5e-12) }
}

fn schematic() -> (Circuit, BenchSpec) {
    let mut mcu = component(0, "mcu", "mcu", &[
        ("GPIO15", 2), ("GPIO14", 3), ("GPIO12", 4), ("GPIO11", 5),
        ("GPIO4", 6), ("GPIO5", 7), ("GPIO6", 8), ("GPIO7", 9),
        ("VDD", 1), ("GND", 0), ("XTAL1", 11), ("XTAL2", 12),
    ]);
    let def = Arc::make_mut(&mut mcu.part);
    def.kind = PartKind::Mcu;
    def.fields.0.push(("model".into(), "ESP32-S3".into()));
    def.gpio = Some(card());
    let mut panel = component(1, "panel", "bus", &[
        ("CSX", 4), ("SCLK", 5), ("SIO0", 6), ("SIO1", 7), ("SIO2", 8), ("SIO3", 9),
        ("RESX", 10), ("VDD", 1), ("GND", 0),
    ]);
    let def = Arc::make_mut(&mut panel.part);
    def.fields.0.push(("model".into(), "CO5300".into()));
    for (key, n) in ["bus.glass_x", "bus.glass_y", "bus.glass_w", "bus.glass_h"].into_iter().zip(GLASS) {
        def.fields.0.push((key.into(), n.to_string()));
    }
    def.gpio = Some(card());
    let mut expander = component(2, "expander", "bus", &[
        ("SDA", 2), ("SCL", 3), ("P0", 10),
        ("P1", 1), ("P2", 1), ("P3", 1), ("P4", 1), ("P5", 1), ("P6", 1), ("P7", 1),
        ("INT", 1), ("VDD", 1), ("GND", 0),
    ]);
    let def = Arc::make_mut(&mut expander.part);
    def.fields.0.extend([("model".into(), "TCA9554".into()), ("bus.addr".into(), "32".into())]);
    def.gpio = Some(card());
    let mut parts = vec![mcu, panel, expander];
    for (path, a, b) in [("pull_sda", 2, 1), ("pull_scl", 3, 1), ("pull_cs", 4, 1), ("load", 1, 0)] {
        let mut r = component(parts.len() as u32, path, "resistor", &[("1", a), ("2", b)]);
        Arc::make_mut(&mut r.part).kind = PartKind::Resistor;
        value(&mut r, 10e3, QtyUnit::Ohm);
        parts.push(r);
    }
    let mut crystal = component(parts.len() as u32, "crystal", "passive", &[("1", 11), ("2", 12)]);
    Arc::make_mut(&mut crystal.part).kind = PartKind::Crystal;
    value(&mut crystal, 40e6, QtyUnit::Hertz);
    parts.push(crystal);
    let circuit = Circuit {
        unit: BoardId(0), name: "screen_rewind".into(), comps: parts,
        nets: ["gnd", "vdd", "sda", "scl", "panel_cs", "panel_clock", "panel_data0", "panel_data1",
            "panel_data2", "panel_data3", "panel_reset", "xtal1", "xtal2"]
            .into_iter().enumerate().map(|(i, name)| Net {
                id: NetId(i as u32), name: name.into(), class: ClassId(0), volts: None, scope: None,
                kind: if i == 0 { NetKind::Ground } else if i == 1 { NetKind::Power } else { NetKind::Signal },
            }).collect(),
        ..Circuit::default()
    };
    let bench = BenchSpec {
        supplies: vec![Supply { net: NetRef { board: BoardId(0), net: NetId(1) }, v: 3.3, limit: None, ramp: None }],
        firmware: vec![Firmware { path: "mcu".into(), doc: "screen.fw.splash".into(), params: Vec::new() }],
        ..BenchSpec::default()
    };
    (circuit, bench)
}

struct Discard;
impl Sink for Discard {}

fn has_colour(frame: &ScreenFrame, colour: u32) -> bool {
    let [x, y, w, h] = GLASS;
    frame.generation != 0 && frame.on && frame.brightness == 0xff
        && (y..y + h).all(|row| frame.bgra[row * frame.width + x..row * frame.width + x + w]
            .iter().all(|&pixel| pixel == colour))
}

fn until_colour(sim: &mut LiveSim, reader: &mut ScreenReader, colour: u32, phase: &str) {
    while sim.t() < END && !has_colour(reader.latest(), colour) {
        let first = reader.latest().generation == 0;
        // Publish every simulated millisecond, independently of host speed. values() syncs
        // the machine and publishes its committed panel memory, just as the live worker does.
        sim.advance((sim.t() + 1e-3).min(END), None, &mut Discard)
            .unwrap_or_else(|d| panic!("{phase}: the live panel circuit advances: {d}"));
        sim.values(0.0);
        let frame = reader.latest();
        if first && frame.generation != 0 {
            eprintln!("first screen publication: {:.6} simulated seconds", frame.t);
        }
    }
    assert!(has_colour(reader.latest(), colour),
        "{phase}: the reader must show the complete colour {colour:#010x} by {END} simulated seconds");
}

#[test]
fn two_edits_restore_the_screen_without_an_event_sink() {
    let (circuit, bench) = schematic();
    let lowered = lower_circuit(&circuit, &bench, 1, &ModelLibs::generic());
    let binding = lowered.firmware.first().expect("the MCU has a firmware binding");
    assert_eq!(binding.bus.parts.len(), 2, "the panel and reset expander reach the bus: {:?}", lowered.diags);
    let input = FwInput {
        part: binding.part.clone(), doc: binding.doc.clone(), rev: 1, text: SOURCE.into(),
        f_clk: binding.f_clk.expect("the crystal supplies the firmware clock"), params: binding.params.clone(),
        chip: binding.chip.as_deref().and_then(Chip::from_model).expect("the model selects the firmware chip"),
        wiring: Some(wiring(&binding.bus.board(0)).expect("the circuit's panel wiring resolves")),
        island: None,
        files: Vec::new(), net_files: Vec::new(),
    };
    let Built::S3(firmware) = compile(&input).expect("the panel firmware builds") else {
        panic!("the MCU model must build an S3 image");
    };
    {
        let (_log, feed) = EventLog::<FwFrame>::new(END, |frame| {
            if frame.is_image() { EventSource::Image } else { EventSource::Trace }
        }, Arc::new(|| {}));
        let sets = Arc::new(Mutex::new(Vec::new()));
        let received = sets.clone();
        let (make, _) = maker(&input, Built::S3(firmware.clone()), feed, false,
            Arc::new(move |readers| received.lock().unwrap().push(readers)), None);
        assert!(make(&lowered, 0).is_some(), "the nominal maker builds the panel device");
        {
            let sets = sets.lock().unwrap();
            assert_eq!(sets.len(), 1, "the nominal device hands exactly one set to the host");
            assert_eq!(sets[0].len(), 1, "the nominal set has exactly one panel reader");
            let (comp, reader) = &sets[0][0];
            assert_eq!(*comp, CompRef { board: circuit.unit, comp: CompId(1) },
                "the reader is keyed by the panel's component, not its bus index");
            assert_eq!(reader.glass(), GLASS, "the reader carries the lowered glass window");
            assert_eq!(reader.size(), (480, 480), "the reader carries the whole frame memory size");
        }
        assert!(make(&lowered, 1).is_some(), "an ensemble member builds the panel device");
        assert_eq!(sets.lock().unwrap().len(), 1, "an ensemble member hands no set to the host");
    }
    let mut netlist = Netlist::new(binding.bus.board(0)).expect("the lowered bus forms a panel board");
    let mut ports = netlist.ports();
    assert_eq!(ports.screens.len(), 1, "the panel must expose exactly one screen reader");
    let (_, mut reader) = ports.screens.pop().expect("the panel reader is retained outside the device");
    let machine = S3Machine::with_board(Arc::new(firmware.flash), &s3_handoff(0), netlist)
        .expect("the image binds to the lowered panel board");
    let pins = lowered.cosim_pins(&binding.part);
    let machine = CoMachine::new(machine, Config { stretch: 96_000, debug: Some(Arc::new(firmware.debug)) }, &pins)
        .expect("the panel machine forms a co-simulated device");
    // No event sink: the screen alone must cause take_outputs to return a handoff.
    let snapshots = Snapshots { span: END, every: Duration::ZERO, share: f64::INFINITY, ..Snapshots::default() };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 1e-3, res: 0.0, trace: false,
        snapshots, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .expect("the panel circuit's operating point converges");
    sim.attach(Some(CoSimDevice { dev: Box::new(machine), pins }));
    assert!(sim.apply(LiveCmd::Play), "the run must play to retain its snapshot");

    until_colour(&mut sim, &mut reader, RED, "setup");
    eprintln!("first drawn panel frame: {:.6} simulated seconds", reader.latest().t);
    let first = reader.latest().clone();
    assert!(0.0 < first.t && first.t <= sim.t(), "the first drawn frame is stamped in seconds inside the run");
    sim.keep_snapshot(&Discard);
    let saved_at = sim.t();
    assert_eq!(sim.stats().snapshots_kept, 1, "one snapshot must be retained before the colour change");
    until_colour(&mut sim, &mut reader, BLUE, "before the first edit");

    // Keep only that snapshot. go_back restores a clone of it on both edits, so a writer
    // accidentally left in the saved device cannot make the second restore pass.
    for (edit, ohms) in [(1, 2.2e3), (2, 4.7e3)] {
        let generation = reader.latest().generation;
        assert!(sim.apply(LiveCmd::Edit(vec![(saved_at, LiveCmd::SetParam {
            generation: lowered.index.generation, device: "load".into(), key: "value".into(), value: ohms,
        })])), "edit {edit}: the load edit behind the front must be accepted");
        assert_eq!(sim.stats().rewinds, edit, "edit {edit}: the edit must restore a snapshot");
        assert_eq!(sim.stats().late_edits, 0, "edit {edit}: the edit must not be applied late at the front");
        assert_eq!(sim.t(), saved_at, "edit {edit}: both edits must restore the same snapshot time");

        // Request the first publication before advancing: firmware cannot repaint the RAM
        // and disguise a missing whole-state invalidation in put_screen.
        sim.values(0.0);
        let restored = reader.latest();
        assert!(restored.generation > generation, "edit {edit}: the original reader must receive a new publication");
        assert!(has_colour(restored, RED), "edit {edit}: the first publication must show the snapshot's first colour");
        assert!(restored.bgra == first.bgra, "edit {edit}: the first publication must send the snapshot's whole memory");
        let generation = restored.generation;
        until_colour(&mut sim, &mut reader, BLUE, &format!("after edit {edit}"));
        assert!(reader.latest().generation > generation, "edit {edit}: the later second colour must be a new publication");
        assert_eq!((sim.stats().stale, sim.stats().unknown), (0, 0),
            "edit {edit}: the scheduled edit must address the live load successfully");
    }
}

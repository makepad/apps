use fab_elec_design::ops::DesignOpError;
use fab_elec_design::{BenchOp, DesignOp, ProbeOf, Tip};
use fab_elec_circuit::NetKind;
use fab_elec_core::drive::Drive;
use fab_elec_core::{Canon, NetRef, PinRef};

#[test]
fn bench_ops_are_text_and_have_content_bytes() {
    let mut design = fab_elec_fixtures::ledctl();
    let hash = design.content_hash();
    let generation = design.generation;
    let add_source = |kind: &str, a: &str, b: &str, opts: &[(&str, f64)]| BenchOp::AddSource {
        kind: kind.into(), a: a.into(), b: b.into(),
        opts: opts.iter().map(|&(key, value)| (key.into(), value)).collect(),
    };
    let pulse = &[("v", 5.0), ("width", 0.002)];
    let variants = || [
        ("SetRange", BenchOp::SetRange { expect: "led_current".into(), min: 0.005, max: 0.020 }),
        ("SetDrive", BenchOp::SetDrive { stub: "mcu.u".into(), pin: "PA3".into(), drive: Drive::Pwm { freq: 1000.0, duty: 0.5 } }),
        ("SetPressed", BenchOp::SetPressed { comp: "user.button".into(), pressed: true }),
        ("AddProbe", BenchOp::AddProbe { of: ProbeOf::v("led0.a"), label: "LED 0 anode".into() }),
        ("RemoveProbe", BenchOp::RemoveProbe { of: ProbeOf::v("led0.a") }),
        ("WriteDrive", BenchOp::WriteDrive { stub: "mcu.u".into(), pin: "PA3".into(), drive: Drive::Pwm { freq: 1000.0, duty: 0.5 } }),
        ("SetProbeRef", BenchOp::SetProbeRef { of: ProbeOf::v("led0.a"), reference: Some("pa0".into()) }),
        ("SetProbeTip", BenchOp::SetProbeTip { of: ProbeOf::v("led0.a"), tip: Tip { r: Some(10e6), c: Some(12e-12), bw: Some(200e6) } }),
        ("SetProbeLabel", BenchOp::SetProbeLabel { of: ProbeOf::v("led0.a"), label: "LED 0".into() }),
        ("SetProbeAt", BenchOp::SetProbeAt { of: ProbeOf::v("led0.a"), at: Some("R1.2".into()) }),
        ("SetSupply", BenchOp::SetSupply { net: "VBUS".into(), key: "v".into(), value: 5.0 }),
        ("SetSource", BenchOp::SetSource { a: "OUT".into(), b: "GND".into(), key: "r".into(), value: 100.0 }),
        ("AddSupply", BenchOp::AddSupply { net: "VBUS".into(), v: 5.0 }),
        ("RemoveSupply", BenchOp::RemoveSupply { net: "VBUS".into() }),
        ("AddSource", add_source("pulse", "OUT", "GND", pulse)),
        ("RemoveSource", BenchOp::RemoveSource { a: "OUT".into(), b: "GND".into() }),
    ];
    let originals = variants();
    for ((name, op), (_, equal)) in originals.iter().zip(variants()) {
        assert_eq!(op.canon_bytes(), equal.canon_bytes(), "{name}: independently built equal ops have equal bytes");
        for host in [false, true] {
            let edit = DesignOp::Bench { bench: "ledctl".into(), op: op.clone() };
            assert_eq!(design.apply(&edit, host), Err(DesignOpError::BenchIsText), "{name}, host={host}: bench ops are text");
            assert_eq!(design.content_hash(), hash, "{name}, host={host}: rejecting a bench op preserves the design content hash");
            assert_eq!(design.generation, generation, "{name}, host={host}: rejecting a bench op preserves the design generation");
        }
    }

    let tags = originals.each_ref().map(|(_, op)| op.canon_bytes()[0]);
    assert_eq!(tags, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15], "the sixteen bench variants have distinct stable tags");
    let [(_, range), (_, drive), (_, pressed), (_, add), (_, remove), (_, write), (_, reference), (_, tip),
        (_, label), (_, place), (_, supply), (_, source), (_, new_supply), (_, removed_supply), (_, new_source), (_, removed_source)] = originals;
    for (field, original, changed) in [
        ("expect", &range, BenchOp::SetRange { expect: "supply_current".into(), min: 0.005, max: 0.020 }),
        ("min", &range, BenchOp::SetRange { expect: "led_current".into(), min: 0.006, max: 0.020 }),
        ("max", &range, BenchOp::SetRange { expect: "led_current".into(), min: 0.005, max: 0.021 }),
        ("stub", &drive, BenchOp::SetDrive { stub: "other.u".into(), pin: "PA3".into(), drive: Drive::Pwm { freq: 1000.0, duty: 0.5 } }),
        ("pin", &drive, BenchOp::SetDrive { stub: "mcu.u".into(), pin: "PA4".into(), drive: Drive::Pwm { freq: 1000.0, duty: 0.5 } }),
        ("drive frequency", &drive, BenchOp::SetDrive { stub: "mcu.u".into(), pin: "PA3".into(), drive: Drive::Pwm { freq: 2000.0, duty: 0.5 } }),
        ("drive duty", &drive, BenchOp::SetDrive { stub: "mcu.u".into(), pin: "PA3".into(), drive: Drive::Pwm { freq: 1000.0, duty: 0.6 } }),
        ("drive variant", &drive, BenchOp::SetDrive { stub: "mcu.u".into(), pin: "PA3".into(), drive: Drive::Level(true) }),
        ("written stub", &write, BenchOp::WriteDrive { stub: "other.u".into(), pin: "PA3".into(), drive: Drive::Pwm { freq: 1000.0, duty: 0.5 } }),
        ("written pin", &write, BenchOp::WriteDrive { stub: "mcu.u".into(), pin: "PA4".into(), drive: Drive::Pwm { freq: 1000.0, duty: 0.5 } }),
        ("written drive frequency", &write, BenchOp::WriteDrive { stub: "mcu.u".into(), pin: "PA3".into(), drive: Drive::Pwm { freq: 2000.0, duty: 0.5 } }),
        ("written drive duty", &write, BenchOp::WriteDrive { stub: "mcu.u".into(), pin: "PA3".into(), drive: Drive::Pwm { freq: 1000.0, duty: 0.6 } }),
        ("written drive variant", &write, BenchOp::WriteDrive { stub: "mcu.u".into(), pin: "PA3".into(), drive: Drive::Level(true) }),
        ("comp", &pressed, BenchOp::SetPressed { comp: "other.button".into(), pressed: true }),
        ("pressed", &pressed, BenchOp::SetPressed { comp: "user.button".into(), pressed: false }),
        ("probe net", &add, BenchOp::AddProbe { of: ProbeOf::v("pa0"), label: "LED 0 anode".into() }),
        ("probe label", &add, BenchOp::AddProbe { of: ProbeOf::v("led0.a"), label: "anode".into() }),
        ("removed net", &remove, BenchOp::RemoveProbe { of: ProbeOf::v("pa0") }),
        ("probe quantity", &add, BenchOp::AddProbe { of: ProbeOf::i("led0.a"), label: "LED 0 anode".into() }),
        ("probe reference", &add, BenchOp::AddProbe { of: ProbeOf::vdiff("led0.a", "pa0"), label: "LED 0 anode".into() }),
        ("set reference", &reference, BenchOp::SetProbeRef { of: ProbeOf::v("led0.a"), reference: None }),
        ("tip bandwidth", &tip, BenchOp::SetProbeTip { of: ProbeOf::v("led0.a"), tip: Tip { r: Some(10e6), c: Some(12e-12), bw: Some(50e6) } }),
        ("renamed probe", &label, BenchOp::SetProbeLabel { of: ProbeOf::v("pa0"), label: "LED 0".into() }),
        ("new label", &label, BenchOp::SetProbeLabel { of: ProbeOf::v("led0.a"), label: "anode".into() }),
        ("placed probe", &place, BenchOp::SetProbeAt { of: ProbeOf::v("pa0"), at: Some("R1.2".into()) }),
        ("new place", &place, BenchOp::SetProbeAt { of: ProbeOf::v("led0.a"), at: Some("R1.1".into()) }),
        ("no place", &place, BenchOp::SetProbeAt { of: ProbeOf::v("led0.a"), at: None }),
        ("supply net", &supply, BenchOp::SetSupply { net: "3V3".into(), key: "v".into(), value: 5.0 }),
        ("supply key", &supply, BenchOp::SetSupply { net: "VBUS".into(), key: "limit".into(), value: 5.0 }),
        ("supply value", &supply, BenchOp::SetSupply { net: "VBUS".into(), key: "v".into(), value: 3.3 }),
        ("source a", &source, BenchOp::SetSource { a: "IN".into(), b: "GND".into(), key: "r".into(), value: 100.0 }),
        ("source b", &source, BenchOp::SetSource { a: "OUT".into(), b: "IN".into(), key: "r".into(), value: 100.0 }),
        ("source key", &source, BenchOp::SetSource { a: "OUT".into(), b: "GND".into(), key: "i".into(), value: 100.0 }),
        ("source value", &source, BenchOp::SetSource { a: "OUT".into(), b: "GND".into(), key: "r".into(), value: 200.0 }),
        ("added supply net", &new_supply, BenchOp::AddSupply { net: "3V3".into(), v: 5.0 }),
        ("added supply voltage", &new_supply, BenchOp::AddSupply { net: "VBUS".into(), v: 3.3 }),
        ("removed supply net", &removed_supply, BenchOp::RemoveSupply { net: "3V3".into() }),
        ("added source kind", &new_source, add_source("vsrc", "OUT", "GND", pulse)),
        ("added source a", &new_source, add_source("pulse", "IN", "GND", pulse)),
        ("added source b", &new_source, add_source("pulse", "OUT", "IN", pulse)),
        ("added source option key", &new_source, add_source("pulse", "OUT", "GND", &[("low", 5.0), ("width", 0.002)])),
        ("added source option value", &new_source, add_source("pulse", "OUT", "GND", &[("v", 3.3), ("width", 0.002)])),
        ("added source option order", &new_source, add_source("pulse", "OUT", "GND", &[pulse[1], pulse[0]])),
        ("removed source a", &removed_source, BenchOp::RemoveSource { a: "IN".into(), b: "GND".into() }),
        ("removed source b", &removed_source, BenchOp::RemoveSource { a: "OUT".into(), b: "IN".into() }),
    ] {
        assert_ne!(original.canon_bytes(), changed.canon_bytes(), "changing {field} changes canonical bytes");
    }
}

/// Every pane names a probe's net with `Design::probe_net`: the board pane's `BoardNet.name` is that
/// name, a name finds exactly its own net again (the bench resolves it by name), a pin's net probes
/// by the sheet's net name, and the ground net keeps its circuit name.
#[test]
fn probe_net_is_the_circuit_net_name() {
    let design = fab_elec_fixtures::ledctl();
    let unit = &design.units[0];
    let board = unit.board.as_ref().expect("the fixture has a board");
    for (i, net) in unit.circuit.nets.iter().enumerate() {
        let name = design.probe_net(NetRef { board: unit.id, net: net.id }).expect("every net has a probe name");
        assert_eq!(name, board.nets[i].name, "{name}: the board pane's net name is the probe name");
        assert_eq!(unit.circuit.net(name).map(|n| n.id), Some(net.id), "{name}: a probe name names exactly this net");
    }
    let lamp = unit.circuit.comp("lamps[0].d").expect("the fixture has lamp 0");
    let anode = PinRef { board: unit.id, comp: lamp.id, pin: lamp.part.pin("A").expect("a lamp has an anode") };
    let net = design.net_of(anode).expect("the anode is connected");
    assert_eq!(design.probe_net(net), Some("lamps[0].d.A"), "a pin's net probes by the sheet's net name");
    let ground = unit.circuit.nets.iter().find(|n| n.kind == NetKind::Ground).expect("the fixture has a ground net");
    assert_eq!(design.probe_net(NetRef { board: unit.id, net: ground.id }), Some("GND"), "the ground net probes by its circuit name, not the solver's 0");
}

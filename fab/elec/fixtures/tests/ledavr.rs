//! ledavr: ledctl's LED board on an ATmega328P at 5 V and 16 MHz. It evaluates clean, its LEDs carry
//! ledctl's currents at the new rail, its clock is the crystal's value, and its bench runs the
//! firmware with a scope card per LED and one for the button.

use fab_elec_bench::{bench::Firmware, lower, ModelLibs};
use fab_elec_circuit::{Circuit, QtyUnit};
use fab_elec_core::Severity;
use fab_elec_lang::{evaluate, led::forward_volts_at, std_libs, Sources, UnitState};

const PART: (&str, &str) = ("parts/avr/ATMEGA328P_TQFP32.part.splash", include_str!("../parts/avr/ATMEGA328P_TQFP32.part.splash"));

fn circuit(docs: &[(&str, &str)], name: &str) -> Circuit {
    let mut sources = Sources::new();
    for (file, text) in docs {
        sources.add(*file, *text).unwrap_or_else(|e| panic!("{file}: an electronics document: {e:?}"));
    }
    let evaluated = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("{name} evaluates: {diags:?}"));
    let unit = evaluated.units.iter().find(|u| u.name == name).unwrap_or_else(|| panic!("{name} has a unit"));
    assert_eq!(unit.state, UnitState::Built, "{name} builds");
    let errors: Vec<_> = evaluated.notes.iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(errors.is_empty(), "{name} evaluates with no errors: {errors:?}");
    evaluated.design.units.iter().find(|u| u.name == name).expect("the unit lowers").circuit.as_ref().clone()
}

#[test]
fn ledavr_keeps_ledctls_led_currents_at_5v_and_clocks_from_its_crystal() {
    let avr = circuit(&[("ledavr.ckt.splash", include_str!("../ledavr.ckt.splash")), PART], "ledavr");
    let ctl = circuit(&[("ledctl.ckt.splash", include_str!("../ledctl.ckt.splash"))], "ledctl");

    // One LED's current through its resistor and the sinking pin (the drivers' 25 ohm `sink`).
    let current = |c: &Circuit, i: usize, rail: f64| {
        let ohms = c.comp(&format!("lamps[{i}].r")).and_then(|r| r.value.as_ref()).and_then(|v| v.qty)
            .filter(|q| q.unit == QtyUnit::Ohm).unwrap_or_else(|| panic!("lamps[{i}] has its resistor")).si;
        let vf = forward_volts_at("green", 5e-3).expect("the green LED has a forward voltage");
        (rail - vf) / (ohms + 25.0)
    };
    for i in 0..8 {
        let (at5, at33) = (current(&avr, i, 5.0), current(&ctl, i, 3.3));
        // Adjacent E24 values are at most 10 % apart.
        assert!((at5 / at33 - 1.0).abs() <= 0.10,
            "LED{i}: {:.2} mA at 5 V is ledctl's {:.2} mA within one E24 step", at5 * 1e3, at33 * 1e3);
    }

    let crystal = avr.comp("mcu.y").expect("the MCU core's crystal is mcu.y");
    let f_clk = crystal.value.as_ref().and_then(|v| v.qty).map(|q| (q.si, q.unit));
    assert_eq!(f_clk, Some((16e6, QtyUnit::Hertz)), "the crystal's value is the 16 MHz MCU clock");
    let mcu = avr.comp("mcu.u").expect("the MCU is mcu.u");
    let net_of = |comp: &fab_elec_circuit::Component, pin: &str| {
        let pin = comp.part.pin(pin).unwrap_or_else(|| panic!("{} has pin {pin}", comp.path));
        comp.conns.iter().find(|(p, _)| *p == pin).map(|(_, net)| *net)
    };
    for (xtal, port) in [("1", "PB6"), ("2", "PB7")] {
        assert!(net_of(crystal, xtal).is_some() && net_of(crystal, xtal) == net_of(mcu, port),
            "crystal pin {xtal} is on the MCU's {port}");
    }
}

#[test]
fn ledavr_bench_runs_its_firmware_on_the_crystal_clock_with_a_card_per_led() {
    let sources = fab_elec_fixtures::docs("ledavr").expect("ledavr is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("ledavr evaluates: {diags:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "chase").expect("ledavr has the chase bench");
    assert_eq!(bench.state, UnitState::Built, "the chase bench builds: {:?}", bench.state.diags());
    assert_eq!(bench.spec.firmware, [Firmware {
        path: "mcu.u".into(), doc: "ledavr.fw.splash".into(), params: vec![("chase_ms".into(), 20.0)],
    }], "the MCU runs ledavr's firmware with a 20 ms chase");
    assert_eq!(bench.probes.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["D1 PD6", "D2 PD5", "D3 PB1", "D4 PB2", "D5 PB3", "D6 PD3", "D7 PD4", "D8 PD7", "Button"],
        "the bench's nine probes (each LED's cathode, then the button) are the scope's cards, in written order");

    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    assert_eq!(lowered.firmware.len(), 1, "one firmware binding");
    let fw = &lowered.firmware[0];
    assert_eq!(fw.f_clk, Some(16e6), "the firmware's clock is the crystal's 16 MHz");
    // The clock is ideal and no oscillator is solved; the amplifier's bias through its feedback
    // resistor gives the crystal's two nets their DC path. Nothing on the board is diagnosed.
    assert!(lowered.diags.is_empty(), "the board lowers without diagnostics: {:?}", lowered.diags);
    let rows: Vec<_> = lowered.cosim_pins(&fw.part).iter().map(|p| p.name.clone()).collect();
    for pin in ["PD6", "PD5", "PB1", "PB2", "PB3", "PD3", "PD4", "PD7", "PD2"] {
        assert!(rows.iter().any(|r| r == pin), "{pin}: an LED or button pin is a co-simulated GPIO row: {rows:?}");
    }
    for pin in ["VCC", "AVCC", "GND", "PB6", "PB7", "PD0", "PD1"] {
        assert!(!rows.iter().any(|r| r == pin), "{pin}: supply, crystal and unconnected pins are no GPIO rows: {rows:?}");
    }
}

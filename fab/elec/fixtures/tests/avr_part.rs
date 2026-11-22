//! The ATMEGA328P part file: it evaluates with its pad electrics, every named pin lands on the
//! package's pad, and pad 1 sits where the package's 3D body puts it.

use fab_elec_lang::{evaluate, std_libs, Sources, UnitState};
use fab_elec_circuit::GpioCard;

const PART: &str = include_str!("../parts/avr/ATMEGA328P_TQFP32.part.splash");

#[test]
fn atmega328p_part_evaluates_and_maps_every_pin_to_its_pad() {
    let mut sources = Sources::new();
    sources.add("parts/avr/ATMEGA328P_TQFP32.part.splash", PART).expect("the part file has an electronics name");
    sources.add("probe.ckt.splash", "let e = mod.elec\nlet u = e.part(\"ATMEGA328P_TQFP32\", {})\n")
        .expect("the probe circuit has an electronics name");
    let evaluated = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("the part file evaluates: {diags:?}"));
    assert_eq!(evaluated.units[0].state, UnitState::Built, "a circuit using the part builds");
    let part = &evaluated.design.units[0].circuit.comps[0].part;
    assert_eq!(part.name, "ATMEGA328P_TQFP32", "the part keeps its name");
    assert_eq!(part.gpio, Some(GpioCard {
        r_high: 26.0, r_low: 23.5, pull_up: 36_000.0, vih: 0.524, vil: 0.418, leak: 1e-6, edge: None, c_pin: None,
    }), "the part carries its 5 V pad electrics in SI units");

    // Our pin names on the package's pad numbering.
    let expected: [(&str, &[&str]); 29] = [
        ("PD3", &["1"]), ("PD4", &["2"]), ("GND", &["3", "5", "21"]), ("VCC", &["4", "6"]),
        ("PB6", &["7"]), ("PB7", &["8"]), ("PD5", &["9"]), ("PD6", &["10"]), ("PD7", &["11"]),
        ("PB0", &["12"]), ("PB1", &["13"]), ("PB2", &["14"]), ("PB3", &["15"]), ("PB4", &["16"]),
        ("PB5", &["17"]), ("AVCC", &["18"]), ("ADC6", &["19"]), ("AREF", &["20"]), ("ADC7", &["22"]),
        ("PC0", &["23"]), ("PC1", &["24"]), ("PC2", &["25"]), ("PC3", &["26"]), ("PC4", &["27"]),
        ("PC5", &["28"]), ("PC6", &["29"]), ("PD0", &["30"]), ("PD1", &["31"]), ("PD2", &["32"]),
    ];
    for (name, pads) in expected {
        let pin = part.pins.iter().find(|pin| pin.name == name).unwrap_or_else(|| panic!("the part has pin {name}"));
        assert_eq!(pin.pads, pads, "pin {name} lands on the package's pads {pads:?}");
    }
    assert_eq!(part.pins.len(), expected.len(), "the part has no pin beyond the named ones");
    let mut used: Vec<u32> = part.pins.iter().flat_map(|pin| &pin.pads).map(|pad| pad.parse().expect("pads are numbered")).collect();
    used.sort_unstable();
    assert_eq!(used, (1..=32).collect::<Vec<_>>(), "every one of the 32 pads is used exactly once");

    // Pad 1 at the left of the bottom edge, then anticlockwise: the corner the 3D body marks.
    let footprint = part.footprint.as_ref().expect("the part has a footprint");
    let at = |no: &str| footprint.pads.iter().find(|pad| pad.no == no).unwrap_or_else(|| panic!("pad {no} exists")).at;
    for (no, side) in [("1", "bottom"), ("8", "bottom"), ("9", "right"), ("16", "right"), ("17", "top"), ("24", "top"), ("25", "left"), ("32", "left")] {
        let p = at(no);
        let on = match side {
            "bottom" => p.y < 0 && p.y.abs() > p.x.abs(),
            "right" => p.x > 0 && p.x.abs() > p.y.abs(),
            "top" => p.y > 0 && p.y.abs() > p.x.abs(),
            _ => p.x < 0 && p.x.abs() > p.y.abs(),
        };
        assert!(on, "pad {no} is on the {side} edge: {p:?}");
    }
    assert!(at("1").x < at("8").x && at("9").y < at("16").y && at("17").x > at("24").x && at("25").y > at("32").y,
        "pads run anticlockwise from pad 1 at the left of the bottom edge");
}

//! Circuit suffixes preserve quantities through reading, display and writing.

use fab_elec_circuit::{QtyUnit, Quantity};
use fab_elec_lang::units::{circuit_units, literal_text, qty_of, value_text};
use makepad_script::tokenizer::{lex_with, ScriptToken, ScriptTokenTag, ScriptTokenizer};
use makepad_script::ScriptHeap;

#[test]
fn circuit_units_read_write_and_show() {
    use QtyUnit::{Ampere, Farad, Henry, Hertz, Metre, Ohm, Second, Volt, Watt};

    let units = circuit_units();
    let mut heap = ScriptHeap::default();
    let mut tokenizer = ScriptTokenizer::with_units(units.clone());
    let mut tokens = |source: &str| {
        tokenizer.clear();
        tokenizer.tokenize(source, &mut heap);
        tokenizer.finish(&mut heap);
        tokenizer.tokens.clone()
    };
    let cases = [
        ("1", "m", 1.0),
        ("1", "cm", 0.01),
        ("1", "mm", 0.001),
        ("1", "um", 1e-6),
        ("1", "nm", 1e-9),
        ("25", "mil", 0.000635),
        ("1", "inch", 0.0254),
        ("1", "mohm", 0.001),
        ("270", "ohm", 270.0),
        ("8.06", "kohm", 8060.0),
        ("1", "Mohm", 1e6),
        ("33", "pF", 33e-12),
        ("100", "nF", 1e-7),
        ("0.1", "uF", 1e-7),
        ("4.7", "uF", 4.7e-6),
        ("1", "mF", 0.001),
        ("1", "F", 1.0),
        ("2.2", "nH", 2.2e-9),
        ("1", "uH", 1e-6),
        ("1", "mH", 0.001),
        ("1", "H", 1.0),
        ("1", "uV", 1e-6),
        ("1", "mV", 0.001),
        ("10", "V", 10.0),
        ("1", "kV", 1000.0),
        ("1", "nA", 1e-9),
        ("1", "uA", 1e-6),
        ("600", "mA", 0.6),
        ("1", "A", 1.0),
        ("1", "Hz", 1.0),
        ("1", "kHz", 1000.0),
        ("12", "MHz", 12e6),
        ("1", "GHz", 1e9),
        ("1", "uW", 1e-6),
        ("1", "mW", 0.001),
        ("1", "W", 1.0),
        ("1", "kW", 1000.0),
        ("1", "ps", 1e-12),
        ("10", "ns", 1e-8),
        ("1", "us", 1e-6),
        ("1", "ms", 0.001),
        ("1", "s", 1.0),
        ("90", "deg", std::f64::consts::FRAC_PI_2),
        ("90", "°", std::f64::consts::FRAC_PI_2),
        ("1", "rad", 1.0),
        ("1", "px", 1.0),
        ("1", "%", 0.01),
        ("20", "ppm", 0.00002),
    ];
    for entry in units.entries() {
        assert!(cases.iter().any(|&(_, suffix, _)| suffix == entry.suffix), "{}: suffix has lexical coverage", entry.suffix);
    }
    for (digits, suffix, expected) in cases {
        let source = format!("{digits}{suffix}");
        let lexed = lex_with(&source, &units);
        assert_eq!(lexed.len(), 1, "{source}: one literal token");
        let ScriptToken::F64(value) = lexed[0].token else {
            panic!("{source}: registered suffix must emit F64");
        };
        assert_eq!(value.to_bits(), expected.to_bits(), "{source}: exact SI value");
        let positioned = tokens(&source);
        assert_eq!(positioned.len(), 1, "{source}: one token with metadata");
        let index = positioned[0].unit.expect("registered suffix has a unit index");
        assert_eq!(units.entry(index).suffix, suffix, "{source}: unit index names the entire suffix");
    }

    for (source, kind, expected) in [
        ("1e3", ScriptTokenTag::F64, 1000.0),
        ("1u", ScriptTokenTag::U32, 1.0),
        ("1f", ScriptTokenTag::F32, 1.0),
    ] {
        let lexed = tokens(source);
        assert_eq!(lexed.len(), 1, "{source}: one numeric token without a unit");
        assert_eq!(lexed[0].token.kind(), kind, "{source}: number syntax keeps its token kind");
        assert_eq!(lexed[0].token.number_f64(), Some(expected), "{source}: number syntax keeps its value");
        assert!(lexed[0].unit.is_none(), "{source}: number syntax has no unit metadata");
    }
    // Splash has no hex numbers: `0x1F` lexes as it does with the standard table, though `F`
    // (a hex letter) is a circuit suffix.
    let standard = makepad_script::UnitTable::standard();
    let shape = |t: &[makepad_script::tokenizer::LexedToken]| t.iter().map(|t| (t.token.kind(), t.span())).collect::<Vec<_>>();
    assert_eq!(shape(&lex_with("0x1F", &units)), shape(&lex_with("0x1F", &standard)), "0x1F: a hex-letter suffix changes nothing");
    let unknown = lex_with("5Vx", &units);
    assert_eq!(unknown.len(), 2, "unknown suffix splits into number and identifier");
    assert_eq!(unknown[0].token.as_u40(), Some(5), "unknown suffix preserves the number");
    assert!(matches!(unknown[1].token, ScriptToken::Identifier(_)), "unknown suffix remains an identifier");
    assert_eq!(&"5Vx"[unknown[1].span()], "Vx", "unknown suffix is not partially consumed");
    assert!(tokens("5Vx").iter().all(|token| token.unit.is_none()), "unknown suffix has no unit metadata");

    for (suffix, expected) in [
        ("kohm", (Ohm, 1e3, 1.0)),
        ("mohm", (Ohm, 1.0, 1e3)),
        ("Mohm", (Ohm, 1e6, 1.0)),
        ("mm", (Metre, 1.0, 1e3)),
        ("mil", (Metre, 254.0, 10_000_000.0)),
        ("ms", (Second, 1.0, 1e3)),
        ("uF", (Farad, 1.0, 1e6)),
        ("MHz", (Hertz, 1e6, 1.0)),
    ] {
        assert_eq!(qty_of(suffix), Some(expected), "{suffix}: quantity kind and ratio");
    }
    for suffix in ["deg", "rad", "px", "°", "Vx", "u", "f", "%", "ppm"] {
        assert_eq!(qty_of(suffix), None, "{suffix}: not a circuit quantity");
    }
    assert_eq!(shape(&lex_with("10%3", &units)), shape(&lex_with("10%3", &standard)), "a percent operator between numbers remains modulo");

    for (si, unit, expected) in [
        (5100.0, Ohm, "5.1k"),
        (270.0, Ohm, "270"),
        (10_000.0, Ohm, "10k"),
        (1e-6, Farad, "1uF"),
        (100e-9, Farad, "100nF"),
        (4.7e-6, Farad, "4.7uF"),
        (10.0, Volt, "10V"),
        (1e6, Ohm, "1M"),
        (4.7e-3, Ohm, "4.7m"),
        (0.0, Ohm, "0"),
        (0.0, Farad, "0F"),
        (-5.0, Volt, "-5V"),
        (33e-12, Farad, "33pF"),
        (2.2e-9, Henry, "2.2nH"),
        (12e6, Hertz, "12MHz"),
        (0.6, Ampere, "600mA"),
        (0.003, Metre, "3mm"),
        (2e-6, Second, "2us"),
        (1.25e3, Watt, "1.25kW"),
        (1.23456789e-12, Farad, "1.235pF"),
        (12.3456, Volt, "12.35V"),
        (123.456, Volt, "123.5V"),
        (999.99, Ohm, "1k"),
    ] {
        assert_eq!(value_text(&Quantity { si, unit }), expected, "{si} {unit:?}: schematic text");
    }

    for (si, unit, expected) in [
        (5100.0, Ohm, "5.1kohm"),
        (270.0, Ohm, "270ohm"),
        (1e-7, Farad, "100nF"),
        (4.7e-6, Farad, "4.7uF"),
        (10.0, Volt, "10V"),
        (0.6, Ampere, "600mA"),
        (0.003, Metre, "3mm"),
        (1.5e9, Ohm, "1500Mohm"),
        (2e3, Farad, "2000F"),
        (1e-9, Ohm, "0.000001mohm"),
        (0.0, Ohm, "0ohm"),
    ] {
        assert_eq!(literal_text(&Quantity { si, unit }), expected, "{si} {unit:?}: registered literal spelling");
    }
    for (si, unit) in [
        (8060.0, Ohm),
        (1e-7, Farad),
        (4.7e-6, Farad),
        (0.1, Ohm),
        (1.5e9, Ohm),
        (3.3, Volt),
        (0.0005, Ampere),
        (1.23456789e-12, Farad),
        (2200.0, Ohm),
        (0.003, Metre),
        (2.2e-9, Henry),
        (12e6, Hertz),
        (1.25e3, Watt),
        (1e-8, Second),
        (2e3, Farad),
        (1e-9, Ohm),
        (0.0, Ohm),
    ] {
        let quantity = Quantity { si, unit };
        let text = literal_text(&quantity);
        let lexed = lex_with(&text, &units);
        assert_eq!(lexed.len(), 1, "{text}: literal round trip is one token");
        let ScriptToken::F64(value) = lexed[0].token else {
            panic!("{text}: written quantity must emit F64");
        };
        assert_eq!(value.to_bits(), si.to_bits(), "{text}: literal round trip preserves every SI bit");
        let positioned = tokens(&text);
        let suffix = &units.entry(positioned[0].unit.expect("written quantity has a unit index")).suffix;
        assert_eq!(qty_of(suffix).map(|(kind, _, _)| kind), Some(unit), "{text}: literal round trip preserves the quantity kind");
    }
}

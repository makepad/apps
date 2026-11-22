use fab_elec_circuit::Tol;
use fab_elec_core::canon::Canon;
use fab_elec_parts::PartKind;

#[test]
fn value_tolerance_is_additive_content_with_class_defaults() {
    let design = fab_elec_fixtures::ledctl();
    let circuit = &design.units[0].circuit;

    for value in circuit.comps.iter().filter_map(|comp| comp.value.as_ref()) {
        assert_eq!(value.tol, None, "every fixture value has no declared tolerance");
        let mut old_bytes = value.text.canon_bytes();
        old_bytes.extend(value.qty.canon_bytes());
        assert_eq!(value.canon_bytes(), old_bytes, "an undeclared tolerance preserves the old canonical bytes and hash");

        let mut declared = value.clone();
        declared.tol = Some(Tol::pct(1.0));
        let declared_bytes = declared.canon_bytes();
        assert_ne!(declared_bytes, old_bytes, "a declared tolerance adds canonical content");
        declared.tol = Some(Tol::pct(2.0));
        assert_ne!(declared.canon_bytes(), declared_bytes, "different tolerances have different canonical content");
    }

    let close = |actual: f64, expected: f64| (actual - expected).abs() <= expected.abs() * 1e-12;
    for (tol, nominal, expected, property) in [
        (Tol::pct(1.0), 10_000.0, (9900.0, 10_100.0), "symmetric percentage bounds"),
        (Tol::Pct { minus: 20.0, plus: 80.0 }, 10e-6, (8e-6, 18e-6), "asymmetric percentage bounds"),
        (Tol::abs(0.25e-12), 10e-12, (9.75e-12, 10.25e-12), "absolute SI bounds"),
        (Tol::Pct { minus: 20.0, plus: 80.0 }, -10_000.0, (-18_000.0, -8000.0), "negative nominal bounds"),
    ] {
        let (lo, hi) = tol.bounds(nominal);
        assert!(lo <= hi, "{property}: lowest bound precedes highest bound");
        assert!(close(lo, expected.0), "{property}: lower bound agrees within relative epsilon");
        assert!(close(hi, expected.1), "{property}: upper bound agrees within relative epsilon");
    }

    let mut resistor = circuit.comps.iter().find(|comp| comp.part.kind == PartKind::Resistor)
        .expect("the fixture has a resistor").clone();
    assert_eq!(resistor.tolerance(), Some(Tol::pct(1.0)), "an undeclared resistor tolerance uses the common class default");
    let declared = Tol::Abs { minus: 2.0, plus: 3.0 };
    resistor.value.as_mut().expect("the fixture resistor has a value").tol = Some(declared);
    assert_eq!(resistor.tolerance(), Some(declared), "a declared absolute tolerance wins over the class default");

    let connector = circuit.comps.iter().find(|comp| comp.part.kind == PartKind::Connector)
        .expect("the fixture has a connector");
    assert_eq!(connector.tolerance(), None, "a fixture connector has no tolerance");

    resistor.value.as_mut().expect("the resistor still has a value").qty = None;
    assert_eq!(resistor.tolerance(), None, "a value without a quantity has no tolerance even when one is declared");
    resistor.value = None;
    assert_eq!(resistor.tolerance(), None, "a component without a value has no tolerance or class fallback");
}

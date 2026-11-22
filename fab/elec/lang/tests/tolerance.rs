//! Tolerance syntax retains its units, bounds and source locations.

use fab_elec_core::Sel;
use fab_elec_lang::{Sources, evaluate, std_libs};
use fab_elec_parts::Tol;

const PRELUDE: &str = "use mod.elec.*\nuse mod.std.*\nlet e = mod.elec\nlet a = e.net({})\nlet b = e.net({})\n";

#[test]
fn tolerances_keep_written_units_and_locate_invalid_bounds() {
    let source = format!("{PRELUDE}{}", concat!(
        "let percent = e.r(10kohm 1%, a, b)\n",
        "let ppm = e.r(1kohm 20ppm, a, b)\n",
        "let absolute = e.c(10pF 0.5pF, a, b)\n",
        "let packaged = e.c(100nF 10%, a, b, {package: \"0603\"})\n",
        "let asymmetric = e.c(10uF, a, b, {tol: [-20%, 80%]})\n",
        "let t = 5%\nlet computed = e.r(4.7kohm t, a, b)\n",
        "let omitted = e.r(10kohm, a, b)\n",
        "let option = e.r(10kohm, a, b, {tol: 5%})\n",
        "let zero = e.r(10kohm 0%, a, b)\n",
        "let absolute_pair = e.c(10pF, a, b, {tol: [-0.5pF, 1pF]})\n",
    ));
    let libs = std_libs();
    let mut sources = Sources::new();
    sources.add("tolerance.ckt.splash", source).expect("the circuit is added");
    let ev = evaluate(&sources, &libs).expect("valid tolerance forms evaluate");
    let circuit = &ev.design.units[0].circuit;
    for (path, tol, text) in [
        ("percent", Some(Tol::pct(1.0)), "10k"),
        ("ppm", Some(Tol::pct(0.002)), "1k"),
        ("absolute", Some(Tol::abs(5e-13)), "10pF"),
        ("packaged", Some(Tol::pct(10.0)), "100nF"),
        ("asymmetric", Some(Tol::Pct { minus: 20.0, plus: 80.0 }), "10uF"),
        ("computed", Some(Tol::pct(5.0)), "4.7k"),
        ("omitted", None, "10k"),
        ("option", Some(Tol::pct(5.0)), "10k"),
        ("zero", Some(Tol::pct(0.0)), "10k"),
        ("absolute_pair", Some(Tol::Abs { minus: 5e-13, plus: 1e-12 }), "10pF"),
    ] {
        let component = circuit.comps.iter().find(|c| c.path == path).expect("the component exists");
        let value = component.value.as_ref().expect("the component has a value");
        assert_eq!(value.tol, tol, "{path}: tolerance has the written kind and exact magnitudes");
        assert_eq!(value.text, text, "{path}: the value text excludes tolerance");
        assert_eq!(component.conns.len(), 2, "{path}: both shifted net arguments connect");
    }

    let cases = [
        ("e.r(10kohm 1V, a, b)", "1V", "a tolerance is written with"),
        ("e.r(10kohm 0.01, a, b)", "0.01", "a tolerance is written with"),
        ("e.r(10kohm 1%, a, b, {tol: 5%})", "e.r(10kohm 1%, a, b, {tol: 5%})", "both positionally"),
        ("e.r(10kohm, a, b, {tol: [20%, 80%]})", "20%", "downward"),
        ("e.r(10kohm, -1%, a, b)", "-1%", "non-negative"),
        ("e.r(10kohm, a, b, {tol: [-20%, -80%]})", "-80%", "upward"),
        ("e.r(10kohm 1e309%, a, b)", "1e309%", "finite"),
    ];
    let mut text = PRELUDE.to_string();
    let mut expected = Vec::new();
    for (call, selected, message) in cases {
        let start = text.len() + call.find(selected).expect("the selected argument occurs");
        expected.push((start as u32, (start + selected.len()) as u32, message));
        text.push_str(call);
        text.push('\n');
    }
    let mut invalid = Sources::new();
    let doc = invalid.add("invalid.ckt.splash", text).expect("invalid circuit source is added");
    let errors = evaluate(&invalid, &libs).expect_err("invalid tolerances are rejected");
    for (start, end, message) in expected {
        assert!(errors.iter().any(|d| d.message.contains(message) && d.sel.contains(&Sel::Source { doc, start, end })),
            "{message}: the diagnostic selects precisely the offending argument (or duplicate call): {errors:?}");
    }
}

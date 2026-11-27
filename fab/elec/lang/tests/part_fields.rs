//! A part definition's `lifecycle` and `approved` lower onto the part, refuse bad values at their
//! source expressions, and leave a part that states neither with its exact prior canonical form.

use fab_elec_core::{Canon, DocId, Sel, Severity, Writer};
use fab_elec_lang::{evaluate, std_libs, Sources, UnitState};
use fab_elec_parts::Lifecycle;

fn add_circuit(sources: &mut Sources, name: &str, text: &str) -> DocId {
    sources.add(format!("{name}.ckt.splash"), format!(r#"let e = mod.elec
let symbol = e.f.ic_symbol("IC", {{left: [["IO", @bidi]]}})
{text}
"#)).expect("each part-field case has a distinct circuit document")
}

#[test]
fn define_parses_lifecycle_and_approved_and_locates_invalid_values() {
    let mut sources = Sources::new();
    add_circuit(&mut sources, "fields", r#"
let active = e.part(e.define("IC_ACTIVE", {kind: @ic symbol: symbol lifecycle: @active approved: true}), {})
let nrnd = e.part(e.define("IC_NRND", {kind: @ic symbol: symbol lifecycle: @nrnd}), {})
let obsolete = e.part(e.define("IC_OBSOLETE", {kind: @ic symbol: symbol lifecycle: @obsolete approved: false}), {})
let spelled = e.part(e.define("IC_SPELLED", {kind: @ic symbol: symbol lifecycle: "nrnd"}), {})
let plain = e.part(e.define("IC_PLAIN", {kind: @ic symbol: symbol}), {})
"#);
    let cases = [
        ("bad_tag", "lifecycle: @eol", "lifecycle: expected @active, @nrnd or @obsolete, got @eol", "@eol"),
        ("bad_text", r#"lifecycle: "old""#, r#"lifecycle: expected @active, @nrnd or @obsolete, got "old""#, r#""old""#),
        ("bad_approved", r#"approved: "yes""#, r#"approved: expected true or false, got "yes""#, r#""yes""#),
    ];
    let invalid_text = cases.iter().map(|(name, opt, _, _)| {
        format!("e.define(\"{name}\", {{kind: @ic symbol: symbol {opt}}})")
    }).collect::<Vec<_>>().join("\n");
    let invalid_doc = add_circuit(&mut sources, "invalid_fields", &invalid_text);
    let evaluated = evaluate(&sources, &std_libs()).expect("valid part fields build beside malformed ones");
    assert_eq!(evaluated.units.len(), 2, "both the valid and the invalid circuits are recorded");
    assert_eq!(evaluated.units[0].state, UnitState::Built, "the circuit with valid part fields builds");

    let comps = &evaluated.design.units.iter().find(|u| u.circuit.name == "fields")
        .expect("the valid circuit lowers").circuit.comps;
    let part = |name: &str| comps.iter().find(|c| c.part.name == name)
        .unwrap_or_else(|| panic!("{name}: the component keeps its defined part")).part.clone();
    for (name, lifecycle, approved) in [
        ("IC_ACTIVE", Some(Lifecycle::Active), true),
        ("IC_NRND", Some(Lifecycle::Nrnd), false),
        ("IC_OBSOLETE", Some(Lifecycle::Obsolete), false),
        ("IC_SPELLED", Some(Lifecycle::Nrnd), false),
        ("IC_PLAIN", None, false),
    ] {
        let p = part(name);
        assert_eq!(p.lifecycle, lifecycle, "{name}: the lifecycle lowers onto the part");
        assert_eq!(p.approved, approved, "{name}: approved lowers onto the part");
    }

    // A part stating neither writes exactly what it wrote before the fields existed.
    let plain = part("IC_PLAIN");
    let bytes = |def: &dyn Canon| { let mut w = Writer::bytes(); def.canon(&mut w); w.into_bytes() };
    let mut before = Writer::bytes();
    let fields: [&dyn Canon; 8] = [&plain.name, &plain.kind, &plain.symbol, &plain.footprint, &plain.body,
        &plain.pins, &plain.fields, &plain.emitter];
    for field in fields {
        field.canon(&mut before);
    }
    assert_eq!(bytes(&*plain), before.into_bytes(), "a part without lifecycle or approved keeps its canonical form");
    let mut stated = (*plain).clone();
    stated.lifecycle = Some(Lifecycle::Active);
    assert_ne!(bytes(&stated), bytes(&*plain), "a lifecycle is part of the canonical form");
    let mut approved = (*plain).clone();
    approved.approved = true;
    assert_ne!(bytes(&approved), bytes(&*plain), "approval is part of the canonical form");
    assert_ne!(bytes(&approved), bytes(&stated), "approval and a lifecycle write distinct forms");

    let invalid = &evaluated.units[1];
    assert!(matches!(invalid.state, UnitState::Failed(_)), "malformed part fields fail their circuit");
    let diags = invalid.state.diags();
    assert_eq!(diags.len(), cases.len(), "each malformed field is one refusal: {diags:?}");
    let text = &sources.doc(invalid_doc).expect("the malformed source stays available").text;
    for (name, opt, message, selected) in cases {
        let message = format!("{name}: {message}");
        let diag = diags.iter().find(|d| d.message == message)
            .unwrap_or_else(|| panic!("{name}: expected {message:?}, got {diags:?}"));
        assert_eq!(diag.severity, Severity::Error, "{name}: the refusal is an error");
        let line = format!("e.define(\"{name}\", {{kind: @ic symbol: symbol {opt}}})");
        let start = (text.find(&line).expect("the definition is in the source")
            + line.find(selected).expect("the selected value is in its definition")) as u32;
        assert_eq!(diag.sel.first(), Some(&Sel::Source { doc: invalid_doc, start, end: start + selected.len() as u32 }),
            "{name}: the refusal selects the offending value");
    }
}

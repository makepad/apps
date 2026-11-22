//! Body overrides preserve part definitions and follow each circuit or bench session.

use fab_elec_circuit::Circuit;
use fab_elec_core::{mm, Diag, DocId, Sel, Severity, P};
use fab_elec_lang::{evaluate, std_libs, BenchScope, Evaluated, Sources, UnitState};
use fab_elec_parts::{Body3d, BodyColour, BodySource, BodySurface};
use std::sync::Arc;

const FIRST: &str = r#"let first = e.r(1kohm, nil, nil, {package: "0402"})"#;
const SECOND: &str = r#"let second = e.r(2kohm, nil, nil, {package: "0402"})"#;
const OTHERS: &str = r#"
let capacitor = e.c(1nF, nil, nil, {package: "0402"})
let larger = e.r(3kohm, nil, nil, {package: "0603"})
"#;
const BODY: &str = r#"e.body("R_0402", {mesh: "r.glb" height: 0.35mm})"#;
const LED: &str = r#"let lamp = e.led({color: @green package: "0603"})"#;

fn add_circuit(sources: &mut Sources, name: &str, text: &str) -> DocId {
    sources.add(format!("{name}.ckt.splash"), format!("let e = mod.elec\n{text}\n"))
        .expect("the case has a distinct circuit document")
}

fn circuit<'a>(evaluated: &'a Evaluated, name: &str) -> &'a Circuit {
    &evaluated.design.units.iter().find(|u| u.circuit.name == name)
        .unwrap_or_else(|| panic!("{name}: the circuit must lower")).circuit
}

fn error<'a>(diags: &'a [Diag], message: &str) -> &'a Diag {
    let diag = diags.iter().find(|d| d.message.contains(message))
        .unwrap_or_else(|| panic!("{message}: the error must be reported: {diags:?}"));
    assert_eq!(diag.severity, Severity::Error, "{message}: the diagnostic must be an error");
    diag
}

fn assert_at(diag: &Diag, doc: DocId, text: &str, selected: &str) {
    let start = text.rfind(selected).expect("the expected selection is written in the document") as u32;
    assert_eq!(diag.sel.first(), Some(&Sel::Source {
        doc, start, end: start + selected.len() as u32,
    }), "{}: the diagnostic must select the exact offending call or field", diag.message);
}

#[test]
fn body_overrides_apply_before_between_and_after_use() {
    let mut sources = Sources::new();
    sources.add_asset("r.glb");
    for (name, text) in [
        ("base", format!("{FIRST}\n{SECOND}\n{OTHERS}")),
        ("before", format!("let result = {BODY}\nif result != nil {{ e.f.error(\"a pending body must return nil\") }}\n{FIRST}\n{SECOND}\n{OTHERS}")),
        ("between", format!("{FIRST}\nlet result = {BODY}\nif result == nil {{ e.f.error(\"a defined body must return a part\") }}\n{SECOND}\n{OTHERS}")),
        ("after", format!("{FIRST}\n{SECOND}\n{BODY}\n{OTHERS}")),
    ] {
        add_circuit(&mut sources, name, &text);
    }
    let evaluated = evaluate(&sources, &std_libs()).expect("body overrides evaluate in every order");
    assert!(evaluated.units.iter().all(|u| u.state == UnitState::Built),
        "every order must build: {:?}", evaluated.units);
    assert!(evaluated.notes.is_empty(), "applied overrides must not warn");
    let baseline = &circuit(&evaluated, "base").comps;
    assert_eq!(baseline.len(), 4, "the baseline must contain both resistors and both unaffected parts");
    for name in ["before", "between", "after"] {
        let comps = &circuit(&evaluated, name).comps;
        assert_eq!(comps.len(), baseline.len(), "{name}: the override must not create components");
        for (comp, original) in comps.iter().zip(baseline) {
            let part = &comp.part;
            assert_eq!(part.symbol, original.part.symbol, "{name}: {} must keep its symbol", part.name);
            assert_eq!(part.footprint, original.part.footprint, "{name}: {} must keep its footprint and pads", part.name);
            assert_eq!(part.pins, original.part.pins, "{name}: {} must keep its pins", part.name);
            assert_eq!(part.kind, original.part.kind, "{name}: {} must keep its kind", part.name);
            assert_eq!(part.fields, original.part.fields, "{name}: {} must keep its fields", part.name);
            assert_eq!(comp.refdes, original.refdes, "{name}: {} must keep its prefix", part.name);
            let body = part.body.as_ref().expect("each passive part must have a body");
            if part.name == "R_0402" {
                assert_eq!(body.source, BodySource::Mesh("r.glb".into()), "{name}: every R_0402 must use the mesh");
                assert_eq!(body.height, mm(0.35), "{name}: the overridden height must be in nanometres");
            } else {
                let own = original.part.body.as_ref().map(|body| &body.source);
                assert_eq!(Some(&body.source), own, "{name}: {} must keep the body source it has without the override", part.name);
                assert_ne!(body.source, BodySource::Mesh("r.glb".into()), "{name}: {} must not take the R_0402 override's mesh", part.name);
                assert_eq!(part, &original.part, "{name}: {} must remain unchanged", part.name);
            }
        }
        assert!(Arc::ptr_eq(&comps[0].part, &comps[1].part), "{name}: existing and later components must share the final part");
    }
}

#[test]
fn body_accepts_handles_and_rejects_other_values() {
    let mut sources = Sources::new();
    sources.add_asset("r.glb");
    add_circuit(&mut sources, "handle", &format!(r#"{FIRST}
let part = e.body(e.lookup("R_0402"), {{mesh: "r.glb" height: 0.35mm}})
let placed = e.part(part, {{}})
"#));
    let calls = [
        ("e.body(nil, {height: 1mm})", "got nothing"),
        ("e.body(3, {height: 1mm})", "got a number"),
    ];
    let bad = add_circuit(&mut sources, "invalid", &calls.iter().map(|(call, _)| *call).collect::<Vec<_>>().join("\n"));
    let evaluated = evaluate(&sources, &std_libs()).expect("the valid handle circuit must build");
    let comps = &circuit(&evaluated, "handle").comps;
    assert_eq!(comps.len(), 2, "the returned handle must place the same part again");
    assert!(Arc::ptr_eq(&comps[0].part, &comps[1].part), "the handle must resolve the existing part");
    assert_eq!(comps[0].part.body.as_ref().expect("the handle part has a body").source,
        BodySource::Mesh("r.glb".into()), "the handle override must reach existing and later components");
    let invalid = &evaluated.units[1];
    assert!(matches!(invalid.state, UnitState::Failed(_)), "invalid part values must fail their unit");
    assert_eq!(invalid.state.diags().len(), calls.len(), "each invalid value must fail independently");
    for (call, detail) in calls {
        let diag = error(invalid.state.diags(), &format!("e.body: expected a part or a part name, {detail}"));
        assert_at(diag, bad, &sources.doc(bad).expect("the invalid circuit exists").text, call);
    }
}

#[test]
fn duplicate_body_overrides_select_the_second_call() {
    let mut sources = Sources::new();
    let pending = r#"e.body("R_0402", {height: 0.4mm})"#;
    let defined = r#"e.body(e.lookup("C_0402"), {height: 0.7mm})"#;
    let doc = add_circuit(&mut sources, "twice", &format!(r#"
e.body("R_0402", {{height: 0.35mm}})
{pending}
let capacitor = e.c(1nF, nil, nil, {{package: "0402"}})
e.body("C_0402", {{height: 0.5mm}})
{defined}
"#));
    let diags = evaluate(&sources, &std_libs()).expect_err("duplicate overrides must refuse the circuit");
    assert_eq!(diags.len(), 2, "pending and applied overrides must each reject a second call");
    for (name, call) in [("R_0402", pending), ("C_0402", defined)] {
        let diag = error(&diags, &format!("{name}: e.body is given already (at the first call)"));
        assert_at(diag, doc, &sources.doc(doc).expect("the duplicate circuit exists").text, call);
    }
}

#[test]
fn library_documents_cannot_override_bodies() {
    let mut sources = Sources::new();
    add_circuit(&mut sources, "use_part", FIRST);
    let mut libs = std_libs();
    let call = r#"e.body("R_0402", {height: 0.35mm})"#;
    let text = format!("let e = mod.elec\n{call}\n");
    let doc = libs.add("extra", "bodies.part.splash", text.clone()).expect("the extra library document is distinct");
    let diags = evaluate(&sources, &libs).expect_err("a library cannot override a part body");
    assert_eq!(diags.len(), 1, "a library override must give one refusal");
    let diag = error(&diags, "e.body belongs in a project document; a library gives its part a body in e.define");
    assert_at(diag, doc, &text, call);
}

#[test]
fn body_specs_keep_validation_and_field_locations() {
    let cases = [
        (r#"{mesh: "" height: 0.35mm}"#, "mesh: a file name is needed", r#""""#),
        ("nil", "expected {height: .. ports: [..]}, got nothing", "nil"),
        ("{height: false}", "height: a length is needed", "false"),
        ("{height: 1mm unknown: 99}", "unknown option unknown", "99"),
        ("{height: 1mm mesh: 3}", "mesh: a string, got a number", "3"),
        ("{height: 1mm offset: [1mm, 2mm]}", "offset: [x, y, z] lengths", "[1mm, 2mm]"),
        ("{height: 1mm rotation: [true, 0deg, 0deg]}", "rotation: [rx, ry, rz] angles", "[true, 0deg, 0deg]"),
        ("{height: 1mm ports: [{kind: @missing}]}", "port kind: @plug, @light, @press, @air, @cable or @shaft", "[{kind: @missing}]"),
        (
            "{height: 1mm ports: [{kind: @press profile: {circle: 1mm} at: [0mm, 0mm, 1mm] dir: [0, 0, 1] reach: 1mm margin: 0mm travel: -0.1mm}]}",
            "port travel: a length, not negative",
            "[{kind: @press profile: {circle: 1mm} at: [0mm, 0mm, 1mm] dir: [0, 0, 1] reach: 1mm margin: 0mm travel: -0.1mm}]",
        ),
        (
            "{height: 1mm ports: [{kind: @press profile: {circle: 1mm} at: [0mm, 0mm, 1mm] dir: [0, 0, 1] reach: 1mm margin: 0mm stand: -0.1mm}]}",
            "port stand: a length, not negative",
            "[{kind: @press profile: {circle: 1mm} at: [0mm, 0mm, 1mm] dir: [0, 0, 1] reach: 1mm margin: 0mm stand: -0.1mm}]",
        ),
    ];
    let text = cases.iter().map(|(spec, _, _)| format!("e.body(\"R_0402\", {spec})")).collect::<Vec<_>>().join("\n");
    let mut sources = Sources::new();
    let doc = add_circuit(&mut sources, "invalid_specs", &text);
    let diags = evaluate(&sources, &std_libs()).expect_err("invalid body specs must refuse the circuit");
    assert_eq!(diags.len(), cases.len(), "each malformed body must give its own parser error");
    for (_, message, selected) in cases {
        let diag = error(&diags, &format!("R_0402: body: {message}"));
        assert_at(diag, doc, &sources.doc(doc).expect("the invalid spec circuit exists").text, selected);
    }

    let mut sources = Sources::new();
    let swatch = fab_materials::swatch::colour_by_name("smoke").expect("the smoke swatch exists");
    let cases = [
        ("finished", r#"{material: "stainless_303" finish: "spi_a1"}"#,
            BodySurface { material: "stainless_303".into(), finish: Some("spi_a1".into()), colour: None }),
        ("natural", r#"{material: "tin_lead"}"#,
            BodySurface { material: "tin_lead".into(), finish: None, colour: None }),
        ("hex_colour", r##"{material: "pla" colour: "#1234ab"}"##,
            BodySurface { material: "pla".into(), finish: None,
                colour: Some(BodyColour { srgb: [18.0 / 255.0, 52.0 / 255.0, 171.0 / 255.0], translucency: 0.0, ral: None }) }),
        ("swatch_colour", r#"{material: "pla" colour: "smoke"}"#,
            BodySurface { material: "pla".into(), finish: None,
                colour: Some(BodyColour { srgb: swatch.srgb, translucency: swatch.translucency, ral: swatch.ral }) }),
    ];
    for (name, surface, _) in &cases {
        add_circuit(&mut sources, name, &format!("{FIRST}\ne.body(\"R_0402\", {{height: 1mm surface: {surface}}})"));
    }
    let evaluated = evaluate(&sources, &std_libs()).expect("valid body surfaces evaluate");
    assert!(evaluated.units.iter().all(|u| u.state == UnitState::Built), "every valid surface must build");
    assert!(evaluated.notes.is_empty(), "valid surface declarations must not warn");
    for (name, _, expected) in cases {
        let body = circuit(&evaluated, name).comps[0].part.body.as_ref().expect("the surface override has a body");
        assert_eq!(body.surface.as_ref(), Some(&expected), "{name}: the body retains exactly its stated surface");
    }
}

#[test]
fn project_body_overrides_reach_units_and_block_benches() {
    let mut sources = Sources::new();
    // A top-level part document, as a project saves it; the mesh path is the project's.
    sources.add_asset("parts/example/r.glb");
    sources.add("bodies.part.splash", "let e = mod.elec\ne.body(\"R_0402\", {mesh: \"parts/example/r.glb\" height: 0.35mm})\n")
        .expect("the project body document is added");
    for name in ["first_unit", "second_unit"] {
        add_circuit(&mut sources, name, FIRST);
    }
    sources.add("bodies.bench.splash", r#"let e = mod.elec
for name in ["first_check", "second_check"] {
    bench(name, {circuit: fn() {
        let gnd = e.net({kind: @ground})
        let r = e.r(1kohm, gnd, gnd, {package: "0402"})
    } run: {}}, fn(b, c) {})
}
"#).expect("the block bench document is added");
    let evaluated = evaluate(&sources, &std_libs()).expect("project overrides must load for circuits and benches");
    assert_eq!(evaluated.design.units.len(), 2, "both units must build independently");
    assert_eq!(evaluated.benches.len(), 2, "both block circuits must use the bench session");
    assert!(evaluated.notes.is_empty(), "project part overrides must not give unit notes");
    for unit in &evaluated.design.units {
        assert_eq!(unit.circuit.comps[0].part.body.as_ref().expect("the unit part has a body").source,
            BodySource::Mesh("parts/example/r.glb".into()), "{}: the mesh must resolve against the project part document", unit.circuit.name);
    }
    for bench in &evaluated.benches {
        assert_eq!(bench.state, UnitState::Built, "{}: the block bench must build", bench.name);
        let BenchScope::Own { circuit, .. } = &bench.scope else {
            panic!("{}: a block bench must own its circuit", bench.name);
        };
        assert_eq!(circuit.comps.len(), 1, "{}: the block circuit must contain its resistor", bench.name);
        assert_eq!(circuit.comps[0].part.body.as_ref().expect("the bench part has a body").source,
            BodySource::Mesh("parts/example/r.glb".into()), "{}: the override must outlive individual circuit builds", bench.name);
    }
}

#[test]
fn led_overrides_update_geometry_and_reject_missing_light_ports() {
    let good = r#"e.body("LED_0603_green", {mesh: "led.glb" height: 0.6mm
    ports: [{kind: @light profile: {circle: 0.5mm} at: [0.1mm, 0mm, 0.6mm]
        dir: [0, 1, 0] reach: 10mm margin: 0.2mm}]})"#;
    let bad = r#"e.body("LED_0603_green", {mesh: "led.glb" height: 0.6mm ports: []})"#;
    let mut sources = Sources::new();
    sources.add_asset("led.glb");
    add_circuit(&mut sources, "base", LED);
    add_circuit(&mut sources, "before", &format!("{good}\n{LED}"));
    add_circuit(&mut sources, "after", &format!("{LED}\n{good}"));
    let mut bad_docs = Vec::new();
    for (order, calls) in [("before", format!("{bad}\n{LED}")), ("after", format!("{LED}\n{bad}"))] {
        // A later block circuit observes the same session after the first circuit's error.
        let text = format!(r#"let e = mod.elec
bench("bad_{order}", {{circuit: fn() {{
    let gnd = e.net({{kind: @ground}})
    {calls}
}} run: {{}}}}, fn(b, c) {{}})
bench("kept_{order}", {{circuit: fn() {{
    let gnd = e.net({{kind: @ground}})
    let kept = e.part(e.lookup("LED_0603_green"), {{}})
}} run: {{}}}}, fn(b, c) {{}})
"#);
        let doc = sources.add(format!("{order}.bench.splash"), text).expect("each bad order has its own bench document");
        bad_docs.push((order, doc));
    }
    let evaluated = evaluate(&sources, &std_libs()).expect("valid LED circuits must build beside failing benches");
    assert!(evaluated.units.iter().all(|u| u.state == UnitState::Built), "both valid LED override orders must build");
    let original = &circuit(&evaluated, "base").comps[0].part;
    let original_emitter = original.emitter.as_ref().expect("the original LED has an emitter");
    for name in ["before", "after"] {
        let part = &circuit(&evaluated, name).comps[0].part;
        let body = part.body.as_ref().expect("the overridden LED has a body");
        let emitter = part.emitter.as_ref().expect("the overridden LED keeps its emitter");
        assert_eq!(body.source, BodySource::Mesh("led.glb".into()), "{name}: the LED must take the mesh body");
        assert_eq!(emitter.at, [100_000, 0, 600_000], "{name}: the light port position must be in nanometres");
        assert_eq!(emitter.dir, [0.0, 1.0, 0.0], "{name}: the emitter must take the new direction");
        let open = body.ports[0].profile.bbox();
        assert_eq!(emitter.size, P::new(open.width() as i32, open.height() as i32), "{name}: the emitter face must follow the new opening");
        assert_eq!(emitter.angle, original_emitter.angle, "{name}: the viewing angle must stay unchanged");
        assert_eq!(emitter.light, original_emitter.light, "{name}: the light spectrum must stay unchanged");
        assert_eq!(emitter.flux_per_a, original_emitter.flux_per_a, "{name}: the flux per ampere must stay unchanged");
        assert!(original_emitter.faces.len() == 4 && emitter.faces.is_empty(), "{name}: a moved light port leaves the dome's walls behind");
    }
    for (order, doc) in bad_docs {
        let failed = evaluated.benches.iter().find(|b| b.name == format!("bad_{order}")).expect("the invalid LED bench is recorded");
        assert!(matches!(failed.state, UnitState::Failed(_)), "{order}: a missing light port must fail");
        assert_eq!(failed.state.diags().len(), 1, "{order}: the missing port must give exactly one error");
        let diag = error(failed.state.diags(), "LED_0603_green emits light: its body needs a @light port");
        assert_at(diag, doc, &sources.doc(doc).expect("the invalid LED document exists").text, bad);
        let kept = evaluated.benches.iter().find(|b| b.name == format!("kept_{order}")).expect("the following circuit is recorded");
        let BenchScope::Own { circuit, .. } = &kept.scope else {
            panic!("{order}: the following circuit must still lower the defined LED");
        };
        assert_eq!(circuit.comps.len(), 1, "{order}: a rejected override must leave the LED defined");
        let part = &circuit.comps[0].part;
        assert_eq!(part.body, original.body, "{order}: a missing light port must leave the original body intact");
        assert_eq!(part.emitter, original.emitter, "{order}: a missing light port must leave the original emitter intact");
    }
}

#[test]
fn unused_body_notes_are_local_and_suggest_sorted_parts() {
    let libs = std_libs();
    let mut sources = Sources::new();
    sources.add_asset("r.glb");
    let unused = r#"e.body("R_0420", {mesh: "r.glb" height: 0.35mm})"#;
    let doc = add_circuit(&mut sources, "unused", &format!("{unused}\n{FIRST}"));
    let no_match = r#"e.body("Absent", {height: 1mm})"#;
    let absent_doc = add_circuit(&mut sources, "absent", &format!("{no_match}\n{FIRST}"));
    let many_call = r#"e.body("R_missing", {height: 1mm})"#;
    let many_doc = add_circuit(&mut sources, "many", &format!(r#"{many_call}
for name in ["R_a9", "R_a3", "R_a1", "R_a7", "R_a0", "R_a8", "R_a2", "R_a6", "R_a5", "R_a4", "RX_a0", "C_a0"] {{
    e.define(name, {{kind: @resistor symbol: e.f.two_terminal([["1", @passive], ["2", @passive]])}})
}}
"#));
    let evaluated = evaluate(&sources, &libs).expect("unused overrides must remain warnings");
    assert!(evaluated.units.iter().all(|u| u.state == UnitState::Built), "unused overrides must not fail any unit");
    assert_eq!(evaluated.notes.len(), 3, "each unused circuit override must produce one note");
    for (index, doc, call, message) in [
        (0, doc, unused, "e.body: no part named R_0420 was made in this circuit; parts made: R_0402"),
        (1, absent_doc, no_match, "e.body: no part named Absent was made in this circuit"),
        (2, many_doc, many_call, "e.body: no part named R_missing was made in this circuit; parts made: R_a0, R_a1, R_a2, R_a3, R_a4, R_a5, R_a6, R_a7"),
    ] {
        let notes = &evaluated.units[index].notes;
        assert_eq!(notes.len(), 1, "{message}: the note must belong only to its unit");
        assert_eq!(notes[0].severity, Severity::Warning, "{message}: unused bodies must warn");
        assert_eq!(notes[0].message, message, "suggestions must match the prefix, sort, and stop at eight");
        assert_at(&notes[0], doc, &sources.doc(doc).expect("the warning document exists").text, call);
    }
    let mut project = Sources::new();
    project.add_asset("r.glb");
    project.add("bodies.part.splash", format!("let e = mod.elec\n{unused}\n"))
        .expect("the unused project part document is added");
    add_circuit(&mut project, "uses_other_part", FIRST);
    let evaluated = evaluate(&project, &libs).expect("an unused project override must remain available to other units");
    assert!(evaluated.notes.is_empty() && evaluated.units[0].notes.is_empty(),
        "the same unused override in a project part document must not produce notes");
}

#[test]
fn missing_meshes_keep_the_parts_body_until_the_project_holds_them() {
    // A project models document as Fab's demos ship it: each override names its mesh and leaves the
    // part its own height and ports.
    let models = r#"let e = mod.elec
e.body("R_0402", {mesh: "parts/lcsc/C1.glb"})
e.body("LED_0603_green", {mesh: "parts/lcsc/C2.glb"})
"#;
    let base = {
        let mut sources = Sources::new();
        add_circuit(&mut sources, "lamp", &format!("{FIRST}\n{LED}"));
        evaluate(&sources, &std_libs()).expect("the plain circuit evaluates")
    };
    let plain = &circuit(&base, "lamp").comps;
    let mut sources = Sources::new();
    let doc = sources.add("models.part.splash", models).expect("the models document is a project part document");
    add_circuit(&mut sources, "lamp", &format!("{FIRST}\n{LED}"));

    // Without the meshes: the design builds, says what it lacks, and every part keeps its own body.
    let waiting = evaluate(&sources, &std_libs()).expect("missing meshes do not stop the design");
    assert_eq!(waiting.units[0].state, UnitState::Built, "the unit builds without its meshes");
    assert_eq!(waiting.notes.len(), 2, "one warning per missing mesh: {:?}", waiting.notes);
    for (note, path) in waiting.notes.iter().zip(["parts/lcsc/C1.glb", "parts/lcsc/C2.glb"]) {
        assert_eq!(note.severity, Severity::Warning, "{path}: a missing mesh warns, it does not refuse");
        assert!(note.message.contains(&format!("mesh {path} is not in the project")), "{path}: the warning names the path: {}", note.message);
        assert_at(note, doc, models, &format!("\"{path}\""));
    }
    assert_eq!(waiting.design.missing_meshes(), ["parts/lcsc/C1.glb", "parts/lcsc/C2.glb"], "the design lists what the project lacks");
    for (comp, original) in circuit(&waiting, "lamp").comps.iter().zip(plain) {
        let body = comp.part.body.as_ref().expect("the part keeps its body");
        let own = original.part.body.as_ref().expect("std parts have bodies");
        assert_eq!(&Body3d { missing_mesh: None, ..body.clone() }, own, "{}: the part keeps its own body", comp.part.name);
        assert!(body.missing_mesh.is_some(), "{}: the body names the mesh it waits for", comp.part.name);
        assert_eq!(comp.part.emitter, original.part.emitter, "{}: the emitter is unchanged", comp.part.name);
    }

    // With them: each part takes its mesh and keeps the height and ports the override leaves.
    sources.add_asset("parts/lcsc/C1.glb");
    sources.add_asset("parts/lcsc/C2.glb");
    let held = evaluate(&sources, &std_libs()).expect("held meshes evaluate");
    assert!(held.notes.is_empty() && held.design.missing_meshes().is_empty(), "nothing is missing once the project holds the meshes");
    for ((comp, original), path) in circuit(&held, "lamp").comps.iter().zip(plain).zip(["parts/lcsc/C1.glb", "parts/lcsc/C2.glb"]) {
        let body = comp.part.body.as_ref().expect("the part has its mesh body");
        let own = original.part.body.as_ref().expect("std parts have bodies");
        assert_eq!(body.source, BodySource::Mesh(path.into()), "{}: the body is the mesh", comp.part.name);
        assert_eq!((body.height, &body.ports), (own.height, &own.ports), "{}: an unstated height and ports stay the part's", comp.part.name);
        assert_eq!(body.missing_mesh, None, "{}: nothing is missing", comp.part.name);
        assert_eq!(comp.part.emitter, original.part.emitter, "{}: the kept light port keeps the emitter", comp.part.name);
    }

    // A part with no body of its own needs the override to state the height.
    let mut sources = Sources::new();
    sources.add_asset("x.glb");
    let bare = r#"e.body("X", {mesh: "x.glb"})"#;
    let define = r#"e.define("X", {kind: @resistor symbol: e.f.two_terminal([["1", @passive], ["2", @passive]])})"#;
    let refused = add_circuit(&mut sources, "bare", &format!("{bare}\n{define}\nlet x = e.part(\"X\", {{}})"));
    add_circuit(&mut sources, "stated", &format!("e.body(\"X\", {{mesh: \"x.glb\" height: 1mm}})\n{define}\nlet x = e.part(\"X\", {{}})"));
    let evaluated = evaluate(&sources, &std_libs()).expect("the stated override builds");
    let failed = &evaluated.units[0];
    assert!(matches!(failed.state, UnitState::Failed(_)), "an override without a height fails a part that has no body");
    let diag = error(failed.state.diags(), "X: e.body: the part has no body of its own, so the override states its height");
    assert_at(diag, refused, &sources.doc(refused).expect("the refused circuit exists").text, bare);
    let body = circuit(&evaluated, "stated").comps[0].part.body.as_ref().expect("the stated override gives a body");
    assert_eq!((&body.source, body.height), (&BodySource::Mesh("x.glb".into()), mm(1.0)), "a stated override is the whole body");
}

/// Why: a model's override poses its mesh (an offset and a turn onto the footprint), but the part's
/// own ports were kept unchanged in the overridden body's frame, so the pose moved them too. Each
/// pot's shaft stood 3.5 mm behind its model's shaft and a header's plug port turned off the header,
/// and their cases cut their holes there (11-17).
#[test]
fn a_model_override_keeps_the_parts_ports_where_the_part_put_them() {
    let mut sources = Sources::new();
    sources.add_asset("key.glb");
    add_circuit(&mut sources, "plain", "let key = e.part(mod.std.key_switch_low(), {})");
    add_circuit(&mut sources, "keys", r#"let key = e.part(mod.std.key_switch_low(), {})
e.body("SW_KEY_LOWPROFILE", {mesh: "key.glb" offset: [1mm, 2mm, 0mm] rotation: [0deg, 0deg, 30deg]})"#);
    let evaluated = evaluate(&sources, &std_libs()).expect("the override evaluates");
    let body_of = |name: &str| circuit(&evaluated, name).comps[0].part.body.clone().expect("the key switch has a body");
    let (plain, body) = (body_of("plain"), body_of("keys"));
    assert!(matches!(&body.source, BodySource::Mesh(path) if path == "key.glb"), "the override's model is the body: {:?}", body.source);
    assert_ne!(body.offset, plain.offset, "the override poses its model");
    // Each port on the footprint: its mouth, its direction and its section's extent there.
    let on_footprint = |body: &fab_elec_parts::Body3d| -> Vec<([i32; 3], [f64; 3], [f64; 4])> {
        body.ports.iter().map(|port| {
            let (u, v) = fab_elec_parts::port_axes(port.dir);
            let posed: Vec<[f64; 3]> = port.profile.rings().flatten()
                .map(|p| body.offset.rotate(std::array::from_fn(|i| (p.x as f64 * u[i] + p.y as f64 * v[i]) * 1e-6))).collect();
            let lo = |k: usize| posed.iter().map(|p| p[k]).fold(f64::MAX, f64::min);
            let hi = |k: usize| posed.iter().map(|p| p[k]).fold(f64::MIN, f64::max);
            (body.offset.apply(port.at), body.offset.rotate(port.dir), [lo(0), lo(1), hi(0), hi(1)])
        }).collect()
    };
    let (want, got) = (on_footprint(&plain), on_footprint(&body));
    assert!(!want.is_empty() && want.len() == got.len(), "the part keeps its ports: {want:?} vs {got:?}");
    for ((at_w, dir_w, ext_w), (at, dir, ext)) in want.iter().zip(&got) {
        assert!((0..3).all(|i| (at[i] - at_w[i]).abs() <= 2), "the mouth stays where the part put it: {at:?}, not {at_w:?}");
        assert!((0..3).all(|i| (dir[i] - dir_w[i]).abs() < 1e-9), "the port keeps its direction: {dir:?}, not {dir_w:?}");
        assert!((0..4).all(|i| (ext[i] - ext_w[i]).abs() < 1e-3), "the section lies as the part drew it, not turned with the model: {ext:?}, not {ext_w:?}");
    }
}

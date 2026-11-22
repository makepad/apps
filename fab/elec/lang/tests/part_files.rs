//! Imported part documents, asset-only meshes, drawing options and input reuse.

use fab_elec_core::{mm, Angle, LayerSet, PolySet, Sel, Severity, Shape, Xform3, P};
use fab_elec_design::TextEdit;
use fab_elec_lang::{NO_CANCEL, evaluate, evaluate_from, load_dir, std_libs, DocKind, Sources, UnitState, SYSTEM_DOC};
use fab_elec_parts::{BodySource, BodySurface, PadKind, PadShape, PartKind};
use std::sync::Arc;

const CIRCUIT: &str = r#"let e = mod.elec
let first = e.part("example:C1", {})
let second = e.part(e.lookup("example:C1"), {})
"#;

const PART: &str = r#"let e = mod.elec
let f = e.f
let make = fn() {
    let existing = e.lookup("example:C1")
    if existing != nil { return existing }
    let pins = [["B", @passive], ["C", @passive], ["E", @passive]]
    if pins.len() != 3 { return f.error("C1: three pins are required") }
    return e.define("example:C1", {
        kind: @transistor prefix: "Q"
        fields: {converter: "example" note: "three pins"}
        symbol: f.ic_symbol("C1", {left: pins})
        footprint: f.footprint({
            pads: [
                f.smd("1", [-1mm, 0mm], [0.6mm, 1mm], {rot: 15deg ratio: 0.2}),
                f.smd("2", [1mm, 0mm], [0.6mm, 1mm], {
                    shape: @rect rot: -30deg
                    outline: [[-0.3mm, -0.5mm], [0.8mm, 0mm], [-0.3mm, 0.5mm]]
                }),
                f.smd("3", [0mm, 1mm], [0.5mm, 0.5mm], {
                    shape: @circle rot: 45deg
                    outline: [[-0.4mm, -0.4mm], [0.4mm, -0.4mm], [0mm, 0.4mm]]
                })
            ]
            body: [[-1mm, -1mm], [1mm, 1mm]] mount: @smd text: 0.6mm graphics: []
        })
        pads: {B: "1" C: "2" E: "3"}
        body: {
            mesh: "C1.glb" offset: [0.1mm, -0.2mm, 0.3mm]
            rotation: [15deg, -30deg, 45deg] height: 1.2mm ports: []
            surface: {material: "stainless_303" finish: "spi_a1"}
        }
    })
}
make()
make()
"#;

#[test]
fn imported_part_files_load_and_evaluate() {
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock after epoch").as_nanos();
    let dir = std::env::temp_dir().join(format!("part-files-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(dir.join("parts/example/nested")).expect("create project folders");
    std::fs::write(dir.join("t.ckt.splash"), CIRCUIT).expect("write circuit");
    std::fs::write(dir.join("parts/example/C1.part.splash"), PART).expect("write imported part");
    std::fs::write(dir.join("parts/example/C1.glb"), [0xff, 0, 0xfe]).expect("write opaque mesh bytes");
    std::fs::write(dir.join("parts/example/nested/C2.part.splash"), "").expect("write nested document");
    std::fs::write(dir.join("parts/example/nested/C2.glb"), [0xff]).expect("write nested asset");
    std::fs::write(dir.join("ignored.glb"), [0xff]).expect("write unrelated top-level file");
    let loaded = load_dir(&dir);
    std::fs::remove_dir_all(&dir).expect("remove temp project before pure evaluation");
    let sources = loaded.expect("load documents without reading binary assets");
    // A folder without a system file gets the empty `SYSTEM_DOC`, after the files.
    assert_eq!(sources.docs().iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["parts/example/C1.part.splash", "parts/example/nested/C2.part.splash", "t.ckt.splash", SYSTEM_DOC], "document paths retain folders and sorted order, followed by the fixed system name");
    let doc = sources.find("parts/example/C1.part.splash").expect("find imported part by project-relative path");
    assert_eq!(sources.doc(doc).unwrap().kind, DocKind::Part, "folder names preserve part classification");
    assert_eq!(sources.assets(), ["parts/example/C1.glb", "parts/example/nested/C2.glb"], "only assets below parts are recorded");
    let evaluated = evaluate(&sources, &std_libs()).expect("loaded part evaluates after the project directory is removed");
    assert_eq!(evaluated.units[0].name, "t", "the circuit retains its bare stem");
    let comps = &evaluated.design.units[0].circuit.comps;
    assert_eq!(comps.len(), 2, "string and lookup placement both resolve the imported name");
    assert!(Arc::ptr_eq(&comps[0].part, &comps[1].part), "lookup returns the same colon-named definition");
    assert_eq!((&comps[0].refdes[..], &comps[1].refdes[..]), ("Q1", "Q2"), "colon names preserve reference designators");
    let part = &comps[0].part;
    assert_eq!(part.name, "example:C1", "the part name retains its colon");
    assert_eq!(part.kind, PartKind::Transistor, "the imported part retains its kind");
    assert_eq!(part.pins.iter().map(|p| (p.name.as_str(), p.pads[0].as_str())).collect::<Vec<_>>(),
        [("B", "1"), ("C", "2"), ("E", "3")], "symbol pins retain their pad mapping");
    assert_eq!(part.fields.0, [("converter".into(), "example".into()), ("note".into(), "three pins".into())], "fields retain written order and string values");
    let body = part.body.as_ref().expect("the part has a body");
    assert_eq!(body.source, BodySource::Mesh("parts/example/C1.glb".into()), "mesh resolves in the defining document's folder");
    assert_eq!(body.offset.t, [mm(0.1), mm(-0.2), mm(0.3)], "body translation is in nanometres");
    assert_eq!(body.height, mm(1.2), "mesh bodies retain their required height");
    assert!(body.ports.is_empty(), "an empty ports list stays empty");
    assert_eq!(body.surface, Some(BodySurface {
        material: "stainless_303".into(), finish: Some("spi_a1".into()), colour: None,
    }), "an imported part retains its stated material and finish without inventing a colour");
    let rx = Xform3::axis_angle([0; 3], [1.0, 0.0, 0.0], Angle::deg(15.0));
    let ry = Xform3::axis_angle([0; 3], [0.0, 1.0, 0.0], Angle::deg(-30.0));
    let rz = Xform3::axis_angle([0; 3], [0.0, 0.0, 1.0], Angle::deg(45.0));
    for (i, (actual, expected)) in body.offset.q.iter().zip(rz.then(&ry).then(&rx).q).enumerate() {
        assert!((actual - expected).abs() < 1e-12, "body quaternion component {i} must compose Rz * Ry * Rx");
    }
    let pads = &part.footprint.as_ref().expect("the part has a footprint").pads;
    assert_eq!(pads.iter().map(|p| p.rot).collect::<Vec<_>>(), [Angle::deg(15.0), Angle::deg(-30.0), Angle::deg(45.0)], "each surface pad retains its rotation");
    assert_eq!(pads[0].stack.front, PadShape::RoundRect { size: P::mm(0.6, 1.0), ratio_ppm: 200_000 }, "the default rounded land accepts a fractional ratio");
    let outlines = [
        (PadShape::Rect { size: P::mm(0.6, 1.0) }, vec![P::mm(-0.3, -0.5), P::mm(0.8, 0.0), P::mm(-0.3, 0.5)]),
        (PadShape::Circle { d: mm(0.5) }, vec![P::mm(-0.4, -0.4), P::mm(0.4, -0.4), P::mm(0.0, 0.4)]),
    ];
    for (pad, (anchor, points)) in pads[1..].iter().zip(outlines) {
        assert_eq!(pad.stack.front, PadShape::Custom { anchor: Box::new(anchor), shapes: vec![Shape::Poly(PolySet::from_ring_nonzero(points))] },
            "pad {} retains its anchor and one polygon in pad coordinates", pad.no);
    }
}

#[test]
fn invalid_part_options_are_located() {
    let libs = std_libs();
    let mut sources = Sources::new();
    sources.add("t.ckt.splash", CIRCUIT).expect("circuit name");
    let doc = sources.add("parts/example/C1.part.splash", PART).expect("part name");
    sources.add_asset("parts/example/C1.glb");
    let cases = [
        ("mesh: \"C1.glb\"", "mesh: \"../../../x.glb\"", "\"../../../x.glb\"", "body: mesh: the path leaves the project"),
        ("mesh: \"C1.glb\"", "mesh: \"/x.glb\"", "\"/x.glb\"", "body: mesh: an absolute path"),
        ("converter: \"example\"", "converter: 1", "1", "fields: converter: a string, got 1"),
        ("ratio: 0.2", "shape: @rect ratio: 0.2", "0.2", "ratio: only an @roundrect"),
        ("ratio: 0.2", "ratio: 0.6", "0.6", "ratio: a fraction from 0 to 0.5"),
        ("ratio: 0.2", "ratio: -0.1", "-0.1", "ratio: a fraction from 0 to 0.5"),
        ("outline: [[-0.3mm, -0.5mm], [0.8mm, 0mm], [-0.3mm, 0.5mm]]", "outline: [[0mm, 0mm], [1mm, 0mm]]", "[[0mm, 0mm], [1mm, 0mm]]", "outline: at least three"),
        ("outline: [[-0.3mm, -0.5mm], [0.8mm, 0mm], [-0.3mm, 0.5mm]]", "outline: [[0mm, 0mm], [1mm, 1mm], [0mm, 1mm], [1mm, 0mm]]", "[[0mm, 0mm], [1mm, 1mm], [0mm, 1mm], [1mm, 0mm]]", "without self-intersection"),
        ("shape: @rect rot:", "shape: @oval rot:", "[[-0.3mm, -0.5mm], [0.8mm, 0mm], [-0.3mm, 0.5mm]]", "outline: the anchor shape"),
        ("ratio: 0.2", "unknown_pad: 123", "123", "unknown option unknown_pad"),
        ("offset: [0.1mm, -0.2mm, 0.3mm]", "offset: [1mm, 2mm]", "[1mm, 2mm]", "body: offset: [x, y, z] lengths"),
        ("rotation: [15deg, -30deg, 45deg]", "rotation: [true, 0deg, 0deg]", "[true, 0deg, 0deg]", "body: rotation: [rx, ry, rz] angles"),
        (r#"material: "stainless_303""#, r#"material: "missing_material""#, r#""missing_material""#, "body: surface: material: unknown material missing_material"),
        (r#"finish: "spi_a1""#, r#"finish: "missing_finish""#, r#""missing_finish""#, "body: surface: finish: unknown finish missing_finish"),
        (r#"surface: {material: "stainless_303" finish: "spi_a1"}"#, "surface: false", "false", "body: surface: expected {material:"),
        (r#"surface: {material: "stainless_303" finish: "spi_a1"}"#, r#"surface: {finish: "spi_a1"}"#, r#"{finish: "spi_a1"}"#, "body: surface: material: a material id string is needed"),
        (r#"material: "stainless_303""#, "material: 3", "3", "body: surface: material: a material id string is needed"),
        (r#"finish: "spi_a1""#, "finish: false", "false", "body: surface: finish: a finish id string"),
        (r#"finish: "spi_a1""#, r#"colour: "missing_colour""#, r#""missing_colour""#, "body: surface: colour: a #rrggbb string or swatch name, got missing_colour"),
        (r#"finish: "spi_a1""#, r##"colour: "#12345g""##, r##""#12345g""##, "body: surface: colour: a #rrggbb string or swatch name, got #12345g"),
        (r#"finish: "spi_a1""#, "colour: true", "true", "body: surface: colour: a #rrggbb string or swatch name, got true"),
        (r#"finish: "spi_a1""#, "unknown_surface: 123", "123", "body: surface: unknown option unknown_surface"),
        ("rot: 15deg", "rot: false", "false", "rot: a finite angle"),
    ];
    for (from, to, value, message) in cases {
        let text = PART.replacen(from, to, 1);
        let option = PART.find(from).expect("case replaces a written option");
        let start = option + text[option..].find(value).expect("case names its invalid value");
        let old_len = sources.doc(doc).unwrap().text.len() as u32;
        sources.apply(&[TextEdit { doc, span: 0..old_len, text }]).expect("replace the part for one refusal");
        let errors = evaluate(&sources, &libs).expect_err("invalid part options must refuse evaluation");
        let error = errors.iter().find(|e| e.message.contains(message)).unwrap_or_else(|| panic!("{message}: expected refusal, got {errors:#?}"));
        assert!(error.sel.contains(&Sel::Source { doc, start: start as u32, end: (start + value.len()) as u32 }),
            "{message}: refusal must locate the invalid value, got {:?}", error.sel);
    }
}

#[test]
fn through_pads_keep_land_drill_and_rotation() {
    let mut sources = Sources::new();
    let doc = sources.add("t.ckt.splash", r#"let e = mod.elec
let f = e.f
let part = e.define("example:C2", {
    kind: @connector
    symbol: f.ic_symbol("C2", {left: [["1", @passive], ["2", @passive], ["3", @passive], ["4", @passive]]})
    footprint: f.footprint({
        pads: [
            f.thru("1", [0mm, 0mm], [2mm, 1.2mm], [1.2mm, 0.6mm], {rot: 30deg}),
            f.thru("2", [3mm, 0mm], [2mm, 1.2mm], [1.2mm, 0.6mm], {square: true rot: -45deg}),
            f.thru("3", [6mm, 0mm], [2mm, 1.2mm], [1.2mm, 0.6mm], {square: true plated: false rot: 90deg}),
            f.thru("4", [9mm, 0mm], 2mm, 0.6mm, {plated: false})
        ]
        body: [[-1mm, -1mm], [10mm, 1mm]] mount: @thru text: 0.6mm graphics: []
    })
    pads: [["1", "1"], ["2", "2"], ["3", "3"], ["4", "4"]]
    body: {height: 2mm}
})
let placed = e.part(part, {})
"#).expect("through pad circuit");
    let evaluated = evaluate(&sources, &std_libs()).expect("round and slotted drills evaluate");
    let part = &evaluated.design.units[0].circuit.comps[0].part;
    let pads = &part.footprint.as_ref().unwrap().pads;
    assert_eq!(pads[0].stack.front, PadShape::Oval { size: P::mm(2.0, 1.2) }, "a pair of land dimensions makes an oval");
    assert_eq!(pads[1].stack.front, PadShape::Rect { size: P::mm(2.0, 1.2) }, "square keeps unequal rectangular land dimensions");
    assert_eq!(pads[2].stack.front, PadShape::Oval { size: P::mm(1.2, 0.6) }, "an unplated slot's land follows its drill, ignoring square");
    assert_eq!(pads[3].stack.front, PadShape::Circle { d: mm(0.6) }, "an unplated round hole's land remains the drill");
    for (i, pad) in pads.iter().enumerate() {
        let expected = if i == 3 { P::mm(0.6, 0.6) } else { P::mm(1.2, 0.6) };
        let drill = pad.drill.as_ref().expect("through pads have a drill");
        assert_eq!(drill.size, expected, "pad {i}: drill dimensions retain the slot or round hole");
        assert_eq!(drill.offset, P::ZERO, "pad {i}: the drill remains centred");
        assert_eq!(pad.kind, if i < 2 { PadKind::Thru } else { PadKind::Npth }, "pad {i}: plating controls pad kind");
        assert_eq!(pad.stack.copper, if i < 2 { LayerSet::ALL_COPPER } else { LayerSet::EMPTY }, "pad {i}: only plated pads have copper");
    }
    assert_eq!(pads.iter().map(|p| p.rot).collect::<Vec<_>>(), [Angle::deg(30.0), Angle::deg(-45.0), Angle::deg(90.0), Angle::ZERO], "through rotations and the zero default are retained");
    let body = part.body.as_ref().unwrap();
    assert_eq!(body.source, BodySource::Extrude, "a body without a mesh remains extruded");
    assert_eq!(body.offset, Xform3::IDENTITY, "omitted body transforms default to zero");
    assert_eq!(body.surface, None, "an omitted body surface remains unspecified");
    let text = sources.doc(doc).unwrap().text.clone();
    sources.apply(&[TextEdit { doc, span: 0..text.len() as u32, text: text.replacen("rot: 30deg", "slot: true", 1) }]).expect("replace a through pad option");
    let errors = evaluate(&sources, &std_libs()).expect_err("a separate slot option must be refused");
    let error = errors.iter().find(|d| d.message.contains("unknown option slot")).expect("through pads still reject unknown options");
    let start = sources.doc(doc).unwrap().text.find("slot: true").unwrap() as u32 + "slot: ".len() as u32;
    assert!(error.sel.contains(&Sel::Source { doc, start, end: start + 4 }), "unknown through pad option locates its value");
}

#[test]
fn assets_and_part_edits_invalidate_reuse() {
    let libs = std_libs();
    let mut sources = Sources::new();
    sources.add("t.ckt.splash", CIRCUIT).expect("circuit name");
    let doc = sources.add("parts/example/C1.part.splash", PART).expect("part name");
    sources.add_asset("parts/example/C1.glb");
    let first = evaluate(&sources, &libs).expect("initial evaluation");
    sources.add_asset("parts/example/C2.glb");
    sources.add_asset("parts/example/A1.glb");
    sources.add_asset("parts/example/C1.glb");
    assert_eq!(sources.assets(), ["parts/example/A1.glb", "parts/example/C1.glb", "parts/example/C2.glb"], "host-added asset paths are sorted and deduplicated");
    let added = evaluate_from(Some(&first), &sources, &libs, &NO_CANCEL).expect("evaluation with added assets");
    assert!(!added.units[0].reused, "adding an unused asset must invalidate the unit");
    assert_eq!(added.assets, sources.assets(), "evaluation retains the asset snapshot");
    assert_eq!(first.assets, ["parts/example/C1.glb"], "later assets do not mutate the earlier snapshot");
    let same = evaluate_from(Some(&added), &sources.clone(), &libs, &NO_CANCEL).expect("evaluation with identical assets");
    assert!(same.units[0].reused, "equal asset lists reuse the unit");
    assert!(Arc::ptr_eq(&same.design.units[0].circuit, &added.design.units[0].circuit), "reusing assets retains the lowering allocation");
    sources.apply(&[TextEdit { doc, span: 0..PART.len() as u32, text: PART.replace("three pins", "updated pins") }]).expect("edit the part document");
    let edited = evaluate_from(Some(&same), &sources, &libs, &NO_CANCEL).expect("evaluation with an edited part");
    assert!(!edited.units[0].reused, "changing a project part must invalidate its units");
    assert_eq!(edited.design.units[0].circuit.comps[0].part.fields.0[1].1, "updated pins", "part edits reach the rebuilt definition");
    let removed = evaluate_from(Some(&edited), &{
        let mut without = Sources::new();
        for d in sources.docs() {
            without.add(&d.name, d.text.clone()).expect("copy documents without assets");
        }
        without
    }, &libs, &NO_CANCEL).expect("a missing mesh does not stop the design");
    assert!(!removed.units[0].reused, "removing assets also invalidates the unit");
    assert_eq!(removed.units[0].state, UnitState::Built, "a part whose mesh is gone still builds");
    assert!(removed.units[0].notes.iter().any(|d| d.severity == Severity::Warning
        && d.message.contains("mesh parts/example/C1.glb is not in the project")), "asset removal warns of the missing mesh: {:?}", removed.units[0].notes);
    let body = removed.design.units[0].circuit.comps[0].part.body.as_ref().expect("the part keeps a body");
    assert_eq!((&body.source, body.offset, body.height), (&BodySource::Extrude, Xform3::IDENTITY, mm(1.2)),
        "without its mesh the body is the courtyard extruded to the stated height");
    assert_eq!(body.missing_mesh.as_deref(), Some("parts/example/C1.glb"), "the body names the mesh it waits for");
    assert_eq!(removed.design.missing_meshes(), ["parts/example/C1.glb"], "the design lists the missing mesh once");
}

#[test]
fn library_parts_cannot_name_meshes() {
    let mut sources = Sources::new();
    sources.add("t.ckt.splash", CIRCUIT).expect("circuit name");
    sources.add_asset("example/C1.glb");
    let mut libs = std_libs();
    let doc = libs.add("example", "C1.part.splash", PART).expect("library part document");
    let errors = evaluate(&sources, &libs).expect_err("library mesh references are refused even if an asset matches");
    let error = errors.iter().find(|e| e.message.contains("library parts may not name a mesh")).expect("specific library mesh refusal");
    let start = PART.find("\"C1.glb\"").unwrap() as u32;
    assert!(error.sel.contains(&Sel::Source { doc, start, end: start + "\"C1.glb\"".len() as u32 }), "library mesh refusal locates the value in its defining document");
}

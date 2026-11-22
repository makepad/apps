//! Synthetic conversions must retain their meaning when loaded as project part documents.

use fab_elec_import::easyeda::{Lcsc, COMPONENT, MODEL};
use fab_elec_import::glb::read_glb;
use fab_elec_import::library::{Raw, Source};
use fab_elec_lang::{evaluate, load_dir, std_libs};

const RESISTOR: &str = r#"{
  "success": true,
  "result": {
    "dataStr": {
      "head": {"c_para": {"pre": "R?", "Manufacturer Part": "TEST-R", "package": "symbol-package"}},
      "shape": [
        "P~show~1~2~40~0~0~p2^^40~0^^M40 0 L30 0^^1~30~0~0~",
        "P~show~1~1~0~0~180~p1^^0~0^^M0 0 L10 0^^1~10~0~0~1",
        "R~5~-5~30~10"
      ]
    },
    "packageDetail": {"dataStr": {
      "head": {"x": "10", "y": 20.0, "c_para": {"package": "test-package"}},
      "BBox": {"x": 6, "y": "16", "width": 8.0, "height": "8"},
      "shape": [
        "PAD~RECT~8~20~2~1~1~~1~0~~0~pad1~0~~Y",
        "PAD~RECT~12~20~2~1~1~~2~0~~90~pad2~0~~Y",
        "TRACK~0.5~3~~7 18 13 18~silk",
        "SOLIDREGION~100~~M8 19 L12,19 L12,21 Z~solid~drawing",
        "SVGNODE~{\"attrs\":{\"c_etype\":\"outline3D\",\"c_rotation\":\"0,0,90\",\"c_origin\":\"12,19\",\"z\":0}}"
      ]
    }}
  }
}"#;

const CONNECTOR: &str = r#"{
  "success": true,
  "result": {
    "dataStr": {
      "head": {"c_para": {"pre": "USBC?", "package": "fallback-package"}},
      "shape": [
        "P~show~1~A~0~0~180~pa^^0~0^^M0 0 L5 0^^1~5~0~0~GND",
        "P~show~2~2~10~0~90~p2^^10~0^^M10 0 L10 5^^1~10~5~0~GND",
        "P~show~3~B~100~0~~pb^^100~0^^M100 0 L95 0^^1~95~0~0~DATA"
      ]
    },
    "packageDetail": {"dataStr": {
      "head": {"x": 10, "y": "20", "c_para": {}},
      "BBox": {"x": "4", "y": 16, "width": "16", "height": 12.0},
      "shape": [
        "PAD~POLYGON~10~20~4~4~1~~A~0~8 18 12 18 12 22 8 22~90~pa~0~~Y",
        "PAD~OVAL~16~22~4~8~11~~2~1~~0~p2~6~16 20 16 24~Y",
        "HOLE~6~24~0.5~hole",
        "PAD~ELLIPSE~8~20~1~1~1~~B~0~~~pb~0~~Y",
        "SOLIDREGION~99~~M8 18 L18,18 L18,22 L8,22 Z~solid~body",
        "TRACK~1~99~~6 16 14 24~body-edge",
        "SVGNODE~{\"attrs\":{\"c_etype\":\"outline3D\",\"c_rotation\":\"0,0,0\",\"c_origin\":\"1000,-1000\",\"z\":\"-1\"}}"
      ]
    }}
  }
}"#;

const CUBE: &str = "newmtl body
Kd 0.2 0.3 0.4
d 0.0
endmtl
usemtl body
v 0 0 -1
v 2 0 -1
v 2 4 -1
v 0 4 -1
v 0 0 0
v 2 0 0
v 2 4 0
v 0 4 0
f 1// 3// 2//
f 1// 4// 3//
f 5// 6// 7//
f 5// 7// 8//
f 1// 2// 6//
f 1// 6// 5//
f 2// 3// 7//
f 2// 7// 6//
f 3// 4// 8//
f 3// 8// 7//
f 4// 1// 5//
f 4// 5// 8//
";

fn raw(component: &str, model: Option<&str>) -> Raw {
    let mut raw = Raw::default();
    raw.put(COMPONENT, component.as_bytes().to_vec());
    if let Some(model) = model { raw.put(MODEL, model.as_bytes().to_vec()); }
    raw
}

// The footprint values checked in convert.rs, expressed independently of write_part. Evaluating
// these native calls gives typed shapes (including the custom polygon) without another dependency.
const RESISTOR_PADS: &str = r#"
    f.smd("1", [-0.508mm, 0mm], [0.508mm, 0.254mm], {shape: @rect rot: 0deg}),
    f.smd("2", [0.508mm, 0mm], [0.508mm, 0.254mm], {shape: @rect rot: 270deg})
"#;
const CONNECTOR_PADS: &str = r#"
    f.smd("A", [0mm, 0mm], [0.508mm, 0.508mm], {shape: @rect rot: 0deg
        outline: [[-0.508mm, 0.508mm], [0.508mm, 0.508mm], [0.508mm, -0.508mm], [-0.508mm, -0.508mm]]}),
    f.thru("2", [1.524mm, -0.508mm], [1.016mm, 2.032mm], [0.508mm, 1.524mm], {rot: 0deg}),
    f.thru("", [-1.016mm, -1.016mm], 0.254mm, 0.254mm, {rot: 0deg plated: false}),
    f.smd("B", [-0.508mm, 0mm], [0.254mm, 0.254mm], {shape: @circle rot: 0deg})
"#;

fn close_nm(actual: i32, expected: i32, property: &str) {
    assert!((i64::from(actual) - i64::from(expected)).abs() <= 1,
        "{property}: expected {expected} nm within 1 nm, got {actual} nm");
}

#[test]
fn converted_part_files_evaluate() {
    let libs = std_libs();
    // Supplier polygons repeat corners, join collinear runs and close on their first point (a
    // USB-C receptacle's merged pads): the outline evaluates as the clean square's.
    let ragged = CONNECTOR.replace("~0~8 18 12 18 12 22 8 22~90~", "~0~8 18 8 18 10 18 12 18 12 22 12 22 8 22 8 18~90~");
    assert_ne!(ragged, CONNECTOR, "the ragged case changes the polygon pad");
    for (case, input, component, model, kind, prefix, expected_pads) in [
        ("resistor", " c900001 ", RESISTOR, Some(CUBE), "resistor", "R", RESISTOR_PADS),
        ("connector", "C900002", CONNECTOR, Some(CUBE), "connector", "USBC", CONNECTOR_PADS),
        ("connector without model", "C900002", CONNECTOR, None, "connector", "USBC", CONNECTOR_PADS),
        ("ragged polygon pad", "C900002", ragged.as_str(), None, "connector", "USBC", CONNECTOR_PADS),
    ] {
        let raw = raw(component, model);
        let converted = Lcsc.convert(input, &raw).unwrap_or_else(|e| panic!("{case}: conversion failed: {e}"));
        let number = Lcsc.number(input).expect("synthetic part number is valid");
        let name = format!("lcsc:{number}");
        assert_eq!(converted.mesh.is_some(), model.is_some(), "{case}: converted model presence");
        let mesh = converted.mesh.as_deref().map(|bytes| {
            let (_, rest) = converted.part.split_once("body: {mesh: \"").expect("modeled part writes body.mesh");
            let (file, _) = rest.split_once('"').expect("body.mesh has a closing quote");
            assert_eq!(file, format!("{number}.glb"), "{case}: body.mesh names the converted asset");
            let mesh = read_glb(bytes).expect("converted mesh is a valid GLB").mesh;
            let (_, hi) = mesh.bounds().expect("converted model has bounds");
            (file, (hi[2] as f64).max(0.01))
        });
        let reference_body = mesh.map_or(String::new(), |(file, height)| {
            format!("body: {{mesh: \"parts/lcsc/{file}\" height: {height}mm}}")
        });
        let first_pad = if kind == "resistor" { "1" } else { "A" };
        // The reference uses the landed natives directly, not the writer under test. Its mesh path
        // is project-relative because this definition lives in the circuit, not the parts folder.
        let circuit = format!(r#"let e = mod.elec
let f = e.f
let imported = e.part("{name}", {{}})
let reference = e.part(e.define("reference", {{
    kind: @ic
    symbol: f.two_terminal([["a", @passive], ["b", @passive]])
    footprint: f.footprint({{
        pads: [{expected_pads}]
        body: [[-3mm, -3mm], [3mm, 3mm]]
    }})
    pads: [["a", "{first_pad}"], ["b", "2"]]
    {reference_body}
}}), {{}})
"#);
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .expect("clock is after the epoch").as_nanos();
        let dir = std::env::temp_dir().join(format!("part-eval-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(dir.join("parts/lcsc")).expect("create temporary part folder");
        std::fs::write(dir.join("parts/lcsc").join(format!("{number}.part.splash")), &converted.part)
            .expect("write converted part document");
        if let Some((file, _)) = mesh {
            std::fs::write(dir.join("parts/lcsc").join(file), converted.mesh.as_ref().expect("modeled part has GLB bytes"))
                .expect("write converted GLB beside its part");
        }
        std::fs::write(dir.join("t.ckt.splash"), circuit).expect("write placement circuit");
        let loaded = load_dir(&dir);
        std::fs::remove_dir_all(&dir).expect("remove temporary project before evaluation");
        let sources = loaded.unwrap_or_else(|e| panic!("{case}: loading converted part failed: {e:?}"));
        let assets: Vec<String> = mesh.iter().map(|(file, _)| format!("parts/lcsc/{file}")).collect();
        assert_eq!(sources.assets(), assets.as_slice(), "{case}: converted meshes load as project assets");
        let evaluated = evaluate(&sources, &libs)
            .unwrap_or_else(|e| panic!("{case}: converted part failed evaluation: {e:#?}"));
        assert_eq!(evaluated.design.units.len(), 1, "{case}: placement circuit evaluates to one unit");
        let comps = &evaluated.design.units[0].circuit.comps;
        assert_eq!(comps.len(), 2, "{case}: imported and reference parts both place");
        let (part, reference) = (&comps[0].part, &comps[1].part);
        assert_eq!(part.name, name, "{case}: defined part name");
        assert_eq!(part.kind.name(), kind, "{case}: part kind");
        assert_eq!(comps[0].refdes, format!("{prefix}1"), "{case}: reference prefix");
        let pin_map = if kind == "resistor" {
            vec![("2", vec!["2"]), ("1", vec!["1"])]
        } else {
            vec![("GND", vec!["A", "2"]), ("DATA", vec!["B"])]
        };
        let pins: Vec<_> = part.pins.iter()
            .map(|p| (p.name.as_str(), p.pads.iter().map(String::as_str).collect::<Vec<_>>())).collect();
        assert_eq!(pins, pin_map, "{case}: every pin retains its name, order and complete pad mapping");
        let symbol_pins: Vec<_> = part.symbol.units.iter().flat_map(|u| &u.pins).map(|p| p.pin.ix()).collect();
        assert_eq!(symbol_pins, (0..pin_map.len()).collect::<Vec<_>>(), "{case}: symbol pins refer to the mapped pins");
        let mut fields: Vec<(String, String)> = vec![
            ("supplier".into(), "lcsc".into()), ("supplier_part".into(), number.clone()),
        ];
        if kind == "resistor" { fields.push(("mpn".into(), "TEST-R".into())); }
        let package = if kind == "resistor" { "test-package" } else { "fallback-package" };
        fields.extend([("package".into(), package.into()), ("source".into(), "lcsc".into()),
            ("converter".into(), Lcsc.version().to_string().into()), ("raw_sha256".into(), raw.digest())]);
        assert_eq!(part.fields.0, fields, "{case}: supplier number and all fields retain written order and values");
        if let Some((file, height)) = mesh {
            let body = part.body.as_ref().expect("modeled part retains its body");
            let expected = reference.body.as_ref().expect("reference defines a mesh body");
            assert_eq!(body.source, expected.source, "{case}: body is Mesh(parts/lcsc/{file})");
            close_nm(body.height, (height * 1e6).round() as i32, &format!("{case}: converted body height"));
            assert_eq!(body.offset, expected.offset, "{case}: already placed mesh keeps the identity body transform");
            assert!(body.ports.is_empty(), "{case}: omitted ports evaluate as empty");
        } else {
            assert!(part.body.is_none(), "{case}: writer omits the body when no model is available");
        }
        let pads = &part.footprint.as_ref().expect("converted part retains its footprint").pads;
        let expected = &reference.footprint.as_ref().expect("reference defines a footprint").pads;
        assert_eq!(pads.len(), expected.len(), "{case}: footprint retains the converted pad count");
        for (i, (pad, wanted)) in pads.iter().zip(expected).enumerate() {
            assert_eq!(pad.no, wanted.no, "{case}: pad {i} number and order");
            assert_eq!(pad.kind, wanted.kind, "{case}: pad {i} surface, plated or unplated kind");
            assert_eq!(pad.stack.front, wanted.stack.front, "{case}: pad {i} shape, size and custom outline");
            close_nm(pad.at.x, wanted.at.x, &format!("{case}: pad {i} X position"));
            close_nm(pad.at.y, wanted.at.y, &format!("{case}: pad {i} Y position"));
            assert_eq!(pad.rot, wanted.rot, "{case}: pad {i} rotation");
            assert_eq!(pad.drill, wanted.drill, "{case}: pad {i} round or slotted drill");
        }
    }
}

#[test]
fn hostile_text_reaches_only_string_literals() {
    use fab_elec_import::part::{write_part, BodyOut, PadKindOut, PadOut, PartOut, PinOut, Side, Size, StrokeOut, SymbolOut};

    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .expect("clock follows the epoch").as_nanos();
    let root = Scratch(std::env::temp_dir().join(format!("part-quoted-text-{}-{unique}", std::process::id())));
    std::fs::create_dir_all(root.0.join("parts/example")).expect("create synthetic project folder");
    let hostile = "\"\\}{\n\t\u{1}let mod.elec #(1) //";
    // Newline and tab have literal escapes; other control characters are intentionally dropped.
    let clean = |text: &str| text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect::<String>();
    let pins = [format!("pin a {hostile}"), format!("pin b {hostile}")];
    let pads = [format!("pad a {hostile}"), format!("pad b {hostile}"), format!("pad c {hostile}")];
    let mut out = PartOut {
        supplier: hostile.into(), number: "C900003".into(), kind: "ic".into(), prefix: hostile.into(),
        symbol: SymbolOut::Box,
        pins: vec![
            PinOut { name: pins[0].clone(), kind: "passive", side: Side::Left },
            PinOut { name: pins[1].clone(), kind: "passive", side: Side::Right },
        ],
        pads: pads.iter().enumerate().map(|(i, name)| PadOut {
            number: name.clone(), at: [i as f64, 0.0],
            kind: if i == 2 {
                PadKindOut::Thru { d: Size::Round(1.0), drill: Size::Round(0.5), square: false, plated: true }
            } else { PadKindOut::Smd { size: [0.5, 0.5] } },
            shape: None, rot: 0.0, ratio: None, outline: vec![],
        }).collect(),
        courtyard: [[-1.0, -1.0], [3.0, 1.0]], thru: true,
        graphics: vec![StrokeOut { layer: "F.SilkS".into(), points: vec![[-1.0, -1.0], [3.0, -1.0]], w: 0.1 }],
        body: Some(BodyOut { mesh: "C900003.glb".into(), height: 1.0, offset: [0.0; 3], rotation: [0.0; 3] }),
        // Exercise both a single pad string and the list of pad strings written for a shared pin.
        pad_map: vec![(pins[0].clone(), vec![pads[0].clone()]), (pins[1].clone(), vec![pads[1].clone(), pads[2].clone()])],
        fields: ["supplier", "supplier_part", "mpn", "package", "source", "converter", "raw_sha256"].iter()
            .map(|key| (key.to_string(), format!("{key} {hostile}"))).collect(),
        note: "Synthetic quoted supplier text.".into(),
    };
    std::fs::write(root.0.join("parts/example/C900003.glb"), b"synthetic asset").expect("write synthetic mesh asset");
    // The sentinel is defined in the circuit after the part document. Native handles carry the
    // definition index, so it counts even definitions that an injected statement never placed.
    let name = clean(&out.name());
    let circuit = |definitions: usize| format!(r#"let e = mod.elec
let counted = e.define("definition_count", {{kind: @ic symbol: e.f.ic_symbol("count", {{}})}})
if counted.ix != {definitions} {{ e.f.error("unexpected part definition count") }}
let imported = e.part({:?}, {{}})
"#, name);
    let libs = std_libs();
    for symbol in [SymbolOut::Box, SymbolOut::TwoTerminal] {
        out.symbol = symbol;
        std::fs::write(root.0.join("parts/example/C900003.part.splash"), write_part(&out))
            .expect("write hostile text through the part writer");
        std::fs::write(root.0.join("t.ckt.splash"), circuit(1)).expect("write placement and definition-count check");
        let sources = load_dir(&root.0).expect("load synthetic part and asset");
        let evaluated = evaluate(&sources, &libs).expect("quoted supplier text evaluates as data");
        assert_eq!(evaluated.design.units.len(), 1, "hostile text adds no circuit units");
        let comps = &evaluated.design.units[0].circuit.comps;
        assert_eq!(comps.len(), 1, "the document defines exactly one part and the circuit places only that part");
        let part = &comps[0].part;
        assert_eq!(part.name, clean(&out.name()), "part name preserves every non-control character");
        assert_eq!(comps[0].refdes, format!("{}1", clean(&out.prefix)), "reference prefix round-trips exactly");
        let fields: Vec<_> = out.fields.iter().map(|(key, value)| (key.clone(), clean(value))).collect();
        assert_eq!(part.fields.0, fields, "every field value round-trips in its original order");
        let pin_map: Vec<_> = part.pins.iter().map(|pin| (pin.name.clone(), pin.pads.clone())).collect();
        let expected: Vec<_> = out.pad_map.iter()
            .map(|(pin, pads)| (clean(pin), pads.iter().map(|p| clean(p)).collect::<Vec<_>>())).collect();
        assert_eq!(pin_map, expected, "symbol pin names and every pad-map string round-trip exactly");
        let footprint = part.footprint.as_ref().expect("written footprint survives evaluation");
        assert_eq!(footprint.pads.iter().map(|p| p.no.clone()).collect::<Vec<_>>(),
            pads.iter().map(|p| clean(p)).collect::<Vec<_>>(), "SMD and through-pad numbers round-trip exactly");
        assert!(part.body.is_some(), "plain validated mesh name retains the part's body");
    }

    // Converters use constant layers: hostile layer text is data too, but is not a valid layer name.
    out.graphics[0].layer = hostile.into();
    std::fs::write(root.0.join("parts/example/C900003.part.splash"), write_part(&out))
        .expect("write hostile layer through the same string quoting");
    std::fs::write(root.0.join("t.ckt.splash"), "let e = mod.elec\n").expect("write a circuit that places nothing");
    let sources = load_dir(&root.0).expect("load the refused-layer project");
    let diags = evaluate(&sources, &libs).expect_err("hostile layer must return diagnostics, not panic or define a part");
    let error_severity = fab_elec_lang::Diag::error("", vec![]).severity;
    assert!(diags.iter().any(|d| d.severity == error_severity && d.message.contains("is not a layer")),
        "hostile layer is refused with an error diagnostic: {diags:?}");
}

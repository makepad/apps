//! Converter and writer proofs use only small components and geometry written here.

use fab_elec_import::easyeda::{supplier_finish, Lcsc, COMPONENT, MODEL};
use fab_elec_import::glb::read_glb;
use fab_elec_import::library::{Raw, Source};
use fab_elec_import::mesh::srgb_to_linear;
use fab_elec_import::part::{write_part, PadKindOut, PadOut, PartOut, PinOut, Side, Size, StrokeOut, SymbolOut};
use fab_elec_import::{Issue, Material};

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

// Unequal X/Y spans make a missed quarter-turn visible in the bounds.
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

fn close(actual: [f32; 3], wanted: [f32; 3]) {
    assert!(actual.iter().zip(wanted).all(|(a, w)| (*a - w).abs() < 1e-5), "{actual:?} != {wanted:?}");
}

#[test]
fn resistor_text_and_placed_opaque_model() -> Result<(), Issue> {
    let converted = Lcsc.convert(" c900001 ", &raw(RESISTOR, Some(CUBE)))?;
    // The text names the converter's own version: a bump re-converts the library's parts.
    assert_eq!(converted.part, r#"let __lcsc_c900001_e = mod.elec
let __lcsc_c900001_f = mod.elec.f
// Converted by fab-elec-import converter {version} from the supplier's data.
let lcsc_c900001 = fn() {
    let e = __lcsc_c900001_e
    let f = __lcsc_c900001_f
    let existing = e.lookup("lcsc:C900001")
    if existing != nil { return existing }
    return e.define("lcsc:C900001", {
        kind: @resistor
        symbol: f.two_terminal([["2", @passive], ["1", @passive]])
        footprint: f.footprint({
            pads: [
                f.smd("1", [-0.508mm, 0mm], [0.508mm, 0.254mm], {shape: @rect rot: 0deg}),
                f.smd("2", [0.508mm, 0mm], [0.508mm, 0.254mm], {shape: @rect rot: 270deg}),
            ]
            body: [[-1.492mm, -0.746mm], [2.508mm, 1.254mm]]
            mount: @smd
            graphics: [
                f.stroke("F.SilkS", [[-0.762mm, 0.508mm], [0.762mm, 0.508mm]], 0.127mm)
            ]
        })
        body: {mesh: "C900001.glb" offset: [0mm, 0mm, 0mm] rotation: [0deg, 0deg, 0deg] height: 1mm}
        pads: [["2", "2"], ["1", "1"]]
        prefix: "R"
        fields: {supplier: "lcsc" supplier_part: "C900001" mpn: "TEST-R" package: "test-package" source: "lcsc" converter: "{version}" raw_sha256: "efd759f83aca930187549a8f5867b4b2e8e822a7ab3d198901e81a986f148dcc"}
    })
}
lcsc_c900001()
"#.replace("{version}", &Lcsc.version().to_string()), "converted part text preserves geometry and provenance and names its converter");
    assert_eq!(converted.warnings, vec![
        Issue::new(0, "SOLIDREGION", "SOLIDREGION on layer 100: 1 left out (drawings only)"),
        Issue::new(0, "symbol", "the drawn symbol is replaced by the standard symbol"),
    ]);
    let bytes = converted.mesh.as_deref().ok_or_else(|| Issue::new(0, MODEL, "missing test model"))?;
    let back = read_glb(bytes)?;
    assert!(back.warnings.is_empty());
    assert_eq!(back.mesh.triangle_count(), 12);
    assert_eq!(back.mesh.materials.len(), 1);
    assert!(back.mesh.materials.iter().all(|m| m.base_color == [srgb_to_linear(0.2), srgb_to_linear(0.3), srgb_to_linear(0.4), 1.0]),
        "converted GLB preserves decoded linear diffuse and forces opaque alpha");
    let (lo, hi) = back.mesh.bounds().ok_or_else(|| Issue::new(0, MODEL, "missing model bounds"))?;
    close(lo, [-1.492, -0.746, 0.0]);
    close(hi, [2.508, 1.254, 1.0]);
    Ok(())
}

#[test]
fn supplier_material_finish() -> Result<(), Issue> {
    for (name, kd, ratio, metallic, roughness) in [
        ("gold", [0.95, 0.76, 0.18], 0.80, 1.0, 0.25),
        ("bright tin", [0.85; 3], 0.88, 1.0, 0.25),
        ("plastic", [0.18; 3], 0.50, 0.0, 0.6),
    ] {
        let ks = kd.map(|v| v * ratio);
        let base = kd.map(srgb_to_linear);
        let base_color = [base[0], base[1], base[2], 1.0];
        let mut material = Material {
            base_color, specular: Some(ks.map(srgb_to_linear)), metallic: 0.7, roughness: 0.6,
            ..Material::named(name)
        };
        supplier_finish(&mut material);
        assert_eq!(material.metallic, metallic, "{name}: supplier finish metallic follows the display Ks/Kd ratio");
        assert_eq!(material.roughness, roughness, "{name}: supplier finish roughness is 0.25 for metal and unchanged for plastic");
        assert_eq!(material.base_color, base_color, "{name}: supplier finish base colour is exactly decoded Kd");

        let model = CUBE.replace("Kd 0.2 0.3 0.4", &format!(
            "Kd {} {} {}\nKs {} {} {}\nPr 0.6", kd[0], kd[1], kd[2], ks[0], ks[1], ks[2]));
        let converted = Lcsc.convert("C900001", &raw(RESISTOR, Some(&model)))?;
        let back = read_glb(converted.mesh.as_deref().expect("supplier model is written"))?;
        let material = &back.mesh.materials[0];
        assert_eq!(material.metallic, metallic, "{name}: converted GLB metallic includes the supplier finish");
        assert_eq!(material.roughness, roughness, "{name}: converted GLB roughness includes the supplier finish");
        assert_eq!(material.base_color, base_color, "{name}: converted GLB base colour is exactly decoded Kd with opaque alpha");
    }
    let mut material = Material {
        base_color: [srgb_to_linear(0.3), srgb_to_linear(0.2), srgb_to_linear(0.1), 0.4],
        metallic: 0.7, roughness: 0.6, emissive: [srgb_to_linear(0.1); 3],
        ..Material::named("no specular")
    };
    let unchanged = material.clone();
    supplier_finish(&mut material);
    assert_eq!(material, unchanged, "all material properties stay unchanged without specular");
    Ok(())
}

/// The model sits on the outline the supplier draws for it, where its own footprint shows the
/// model, not on `c_origin`: on some parts that lies off the drawn outline (here 2 and 1 units off
/// the pads' centre, head 10, 20). An outline whose points do not pair up is refused.
#[test]
fn the_model_sits_on_its_drawn_outline() -> Result<(), Issue> {
    let node = r#"SVGNODE~{\"attrs\":{\"c_etype\":\"outline3D\",\"c_rotation\":\"0,0,90\",\"c_origin\":\"12,19\",\"z\":0}}"#;
    let drawn = |points: &str| RESISTOR.replace(node, &format!(
        r#"SVGNODE~{{\"attrs\":{{\"c_etype\":\"outline3D\",\"c_rotation\":\"0,0,90\",\"c_origin\":\"12,19\",\"z\":0}},\"childNodes\":[{{\"attrs\":{{\"points\":\"{points}\"}}}},{{\"attrs\":{{\"points\":\"14 22 6 22 6 18\"}}}}]}}"#));
    let component = drawn("6 18 14 18 14 22");
    assert_ne!(component, RESISTOR, "the test gives the resistor's model node a drawn outline");
    let converted = Lcsc.convert("C900001", &raw(&component, Some(CUBE)))?;
    let bytes = converted.mesh.as_deref().ok_or_else(|| Issue::new(0, MODEL, "missing test model"))?;
    let (lo, hi) = read_glb(bytes)?.mesh.bounds().ok_or_else(|| Issue::new(0, MODEL, "missing model bounds"))?;
    close(lo, [-2.0, -1.0, 0.0]);
    close(hi, [2.0, 1.0, 1.0]);
    let error = Lcsc.convert("C900001", &raw(&drawn("6 18 14"), Some(CUBE))).expect_err("unpaired outline points are refused");
    assert!(error.to_string().contains("x, y pairs"), "the refusal says why: {error}");
    Ok(())
}

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

#[test]
fn connector_geometry_mapping_and_box_writer() -> Result<(), Issue> {
    let converted = Lcsc.convert("C900002", &raw(CONNECTOR, Some(CUBE)))?;
    for text in [
        concat!("f.smd(\"A\", [0mm, 0mm], [0.508mm, 0.508mm], {shape: @rect rot: 0deg outline: ",
            "[[-0.508mm, 0.508mm], [0.508mm, 0.508mm], [0.508mm, -0.508mm], [-0.508mm, -0.508mm]]}),"),
        "f.thru(\"2\", [1.524mm, -0.508mm], [1.016mm, 2.032mm], [0.508mm, 1.524mm], {rot: 0deg}),",
        "f.thru(\"\", [-1.016mm, -1.016mm], 0.254mm, 0.254mm, {rot: 0deg plated: false}),",
        "f.smd(\"B\", [-0.508mm, 0mm], [0.254mm, 0.254mm], {shape: @circle rot: 0deg}),",
        "body: [[-1.016mm, -1.016mm], [2.032mm, 1.016mm]]",
        "mount: @thru",
        "pads: [[\"GND\", [\"A\", \"2\"]], [\"DATA\", \"B\"]]",
        "symbol: f.ic_symbol(\"lcsc:C900002\", {left: [[\"GND\", @passive]] right: [[\"DATA\", @passive]]})",
        "prefix: \"USBC\"",
        "package: \"fallback-package\"",
    ] { assert!(converted.part.contains(text), "missing {text}\n{}", converted.part); }
    assert_eq!(converted.warnings, vec![
        Issue::new(0, "SOLIDREGION", "SOLIDREGION on layer 99: 1 left out (drawings only)"),
        Issue::new(0, "TRACK", "TRACK on layer 99: 1 left out (drawings only)"),
        Issue::new(7, "SVGNODE", "the model origin (1000, -1000) is outside the footprint; the model is centred on the footprint origin"),
        Issue::new(7, "SVGNODE", "the 3D model does not reach pad 2 (within 0.1mm)"),
    ]);
    let bytes = converted.mesh.as_deref().ok_or_else(|| Issue::new(0, MODEL, "missing test model"))?;
    let back = read_glb(bytes)?;
    let (lo, hi) = back.mesh.bounds().ok_or_else(|| Issue::new(0, MODEL, "missing model bounds"))?;
    close(lo, [-1.0, -2.0, -0.254]);
    close(hi, [1.0, 2.0, 0.746]);
    let rotated_slot = CONNECTOR.replace("~~0~p2~6~16 20 16 24", "~~90~p2~6~16 20 16 24");
    let rotated = Lcsc.convert("C900002", &raw(&rotated_slot, None))?;
    assert!(rotated.part.contains("[1.016mm, 2.032mm], [1.524mm, 0.508mm], {rot: 270deg}"),
        "slot dimensions must be in the pad's local frame");
    // The plated flag as suppliers spell it: `yes` is plated like `Y` (a USB-C's soldered shell legs),
    // `N` and `no` are bare holes, and anything else is refused rather than guessed.
    let slot = "~~0~p2~6~16 20 16 24~Y";
    for (flag, plated) in [("yes", true), ("YES", true), ("N", false), ("no", false)] {
        let flagged = CONNECTOR.replace(slot, &slot.replace("~Y", &format!("~{flag}")));
        let text = Lcsc.convert("C900002", &raw(&flagged, None))?.part;
        let line = text.lines().find(|l| l.contains("f.thru(\"2\"")).expect("the slot pad");
        assert_eq!(!line.contains("plated: false"), plated, "plated flag {flag:?}: {line}");
    }
    let error = Lcsc.convert("C900002", &raw(&CONNECTOR.replace(slot, &slot.replace("~Y", "~maybe")), None))
        .expect_err("an unknown plated flag is refused");
    assert_eq!(error, Issue::new(2, "PAD", "C900002 pad 2: plated flag \"maybe\" is neither yes nor no"));
    let curved = CONNECTOR.replace("L18,22", "Q100,100 100,100 L18,22");
    let converted = Lcsc.convert("C900002", &raw(&curved, None))?;
    assert!(converted.part.contains("body: [[-1.016mm, -1.016mm], [2.032mm, 1.016mm]]"));
    assert!(converted.warnings.contains(&Issue::new(5, "SOLIDREGION", "path commands Q are left out of the body box")));
    // A polygon that crosses itself (a bow tie) refuses the part, never a land of another shape:
    // the message names the part, the pad and where the outline crosses, in footprint mm.
    let bow = CONNECTOR.replace("~0~8 18 12 18 12 22 8 22~90~", "~0~8 18 12 22 12 18 8 22~90~");
    assert_ne!(bow, CONNECTOR, "the bow-tie case changes the polygon pad");
    let error = Lcsc.convert("C900002", &raw(&bow, None)).expect_err("a self-crossing pad outline is refused");
    assert_eq!(error, Issue::new(1, "PAD", "C900002 pad A: outline crosses itself at (0mm, 0mm)"));
    box_writer();
    Ok(())
}

/// An independent writer example keeps converter mistakes from defining the expected language.
fn box_writer() {
    let out = PartOut {
        supplier: "example".into(), number: "C7".into(), kind: "ic".into(), prefix: "U".into(), symbol: SymbolOut::Box,
        pins: vec![PinOut { name: "IN".into(), kind: "in", side: Side::Left },
            PinOut { name: "OUT".into(), kind: "out", side: Side::Right }],
        pads: vec![PadOut {
            number: "1".into(), at: [0.0, -1.0],
            kind: PadKindOut::Thru { d: Size::Round(2.0), drill: Size::Round(0.8), square: false, plated: true },
            shape: None, rot: 0.0, ratio: None, outline: Vec::new(),
        }],
        courtyard: [[-1.0, -2.0], [1.0, 2.0]], thru: true,
        graphics: vec![StrokeOut { layer: "F.SilkS".into(), points: vec![[-1.0, 0.0], [1.0, 0.0]], w: 0.15 }],
        body: None, pad_map: vec![("IN".into(), vec!["1".into()]), ("OUT".into(), vec!["1".into()])],
        fields: vec![("mpn".into(), "test IC".into())], note: "A synthetic IC.".into(),
    };
    assert_eq!(write_part(&out), r#"let __example_c7_e = mod.elec
let __example_c7_f = mod.elec.f
// A synthetic IC.
let example_c7 = fn() {
    let e = __example_c7_e
    let f = __example_c7_f
    let existing = e.lookup("example:C7")
    if existing != nil { return existing }
    return e.define("example:C7", {
        kind: @ic
        symbol: f.ic_symbol("example:C7", {left: [["IN", @in]] right: [["OUT", @out]]})
        footprint: f.footprint({
            pads: [
                f.thru("1", [0mm, -1mm], 2mm, 0.8mm, {rot: 0deg}),
            ]
            body: [[-1mm, -2mm], [1mm, 2mm]]
            mount: @thru
            graphics: [
                f.stroke("F.SilkS", [[-1mm, 0mm], [1mm, 0mm]], 0.15mm)
            ]
        })
        pads: [["IN", "1"], ["OUT", "1"]]
        prefix: "U"
        fields: {mpn: "test IC"}
    })
}
example_c7()
"#);
}

#[test]
fn refuses_bad_components_and_locates_the_record() -> Result<(), Issue> {
    for (component, field, line, message) in [
        (RESISTOR.replacen("\"success\": true", "\"success\": false", 1), "success", 0, "without a component"),
        (RESISTOR.replacen("P~show~1~2~", "P~show~1~9~", 1), "P", 1, "pin 9 (9) has no pad"),
        (RESISTOR.replacen("PAD~RECT~8~20~2~1~1", "PAD~RECT~8~20~2~1~2", 1), "PAD", 1,
            "a pad on layer 2 is not supported"),
        (RESISTOR.replacen("PAD~RECT~8~20~2~1~1~~1~0~~0~pad1~0~~Y", "PAD~RECT~8", 1), "PAD", 1,
            "PAD needs at least 16 fields"),
        (RESISTOR.replacen("\"x\": \"10\"", "\"x\": \"NaN\"", 1), "x", 0, "not finite"),
        (RESISTOR.replacen("PAD~RECT~8~20", "PAD~RECT~inf~20", 1), "PAD", 1, "not finite"),
        (RESISTOR.replacen("\"dataStr\":", "\"absent\":", 1), "dataStr", 0, "missing object"),
    ] {
        let issue = Lcsc.convert("C900003", &raw(&component, None)).expect_err("malformed component must be refused");
        assert_eq!((issue.field.as_str(), issue.line), (field, line), "{issue}");
        assert!(issue.message.contains(message), "{issue}");
    }
    let issue = Lcsc.convert("../bad", &raw(RESISTOR, None)).expect_err("bad part number");
    assert_eq!((issue.field.as_str(), issue.line), ("number", 0));
    let issue = Lcsc.convert("C900003", &raw(RESISTOR, Some("v invalid 0 0\nf 1 1 1\n")))
        .expect_err("bad model");
    assert_eq!((issue.field.as_str(), issue.line), (MODEL, 0));
    assert!(issue.message.contains("line 1"));
    let component = RESISTOR.replace("\"R?\"", "\"U?\"")
        .replace("~0~\",", "~0~VDD\",");
    let converted = Lcsc.convert("C900003", &raw(&component, None))?;
    assert!(converted.mesh.is_none());
    assert!(converted.part.contains("[\"VDD\", @power_in]"));
    assert!(converted.warnings.iter().any(|w| w.message == "1 pins named as supplies are power inputs"));
    let no_node = RESISTOR.replace("outline3D", "other");
    let converted = Lcsc.convert("C900003", &raw(&no_node, Some(CUBE)))?;
    assert!(converted.mesh.is_none(), "an unplaced model is not exported");
    assert!(converted.part.contains("body: [[-0.762mm, -0.127mm], [0.762mm, 0.127mm]]"), "pad bounds fallback");
    assert!(converted.warnings.contains(&Issue::new(0, "SVGNODE", "the footprint places no 3D model")));
    let no_pads = no_node.replace("PAD~", "DRAWING~");
    let issue = Lcsc.convert("C900003", &raw(&no_pads, None)).expect_err("no pads");
    assert_eq!((issue.field.as_str(), issue.line, issue.message.as_str()), ("shape", 0, "the footprint has no pads"));
    Ok(())
}

// Boxes as OBJ text, one material each: (name, low corner, high corner).
fn boxes(parts: &[(&str, [f32; 3], [f32; 3])]) -> String {
    let mut obj = String::new();
    for (name, _, _) in parts { obj += &format!("newmtl {name}\nKd 0.5 0.5 0.5\nd 0.0\nendmtl\n"); }
    for (k, (name, lo, hi)) in parts.iter().enumerate() {
        obj += &format!("usemtl {name}\n");
        for c in 0..8 {
            let p = [0, 1, 2].map(|i| if c >> i & 1 == 1 { hi[i] } else { lo[i] });
            obj += &format!("v {} {} {}\n", p[0], p[1], p[2]);
        }
        let b = k * 8 + 1;
        for f in [[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]] {
            obj += &format!("f {}// {}// {}//\n", b + f[0], b + f[1], b + f[2]);
        }
    }
    obj
}

#[test]
fn an_led_names_its_lens() -> Result<(), Issue> {
    let led = RESISTOR.replace(r#""pre": "R?""#, r#""pre": "LED?""#);
    let names = |component: &str, model: &str| -> Result<Vec<String>, Issue> {
        let converted = Lcsc.convert("C900003", &raw(component, Some(model)))?;
        let read = read_glb(&converted.mesh.expect("a model"))?;
        Ok(read.mesh.materials.iter().map(|m| m.name.clone()).collect())
    };
    // A base with terminals, a clear dome reaching the top, a die inside the dome.
    let dome = boxes(&[("base", [0.0, 0.0, 0.0], [2.0, 1.0, 0.2]), ("dome", [0.2, 0.1, 0.2], [1.8, 0.9, 0.6]),
        ("die", [0.9, 0.4, 0.3], [1.1, 0.6, 0.4])]);
    assert_eq!(names(&led, &dome)?, ["base", "lens", "die"], "an LED's material reaching the model's top is its lens");
    assert_eq!(names(RESISTOR, &dome)?, ["base", "dome", "die"], "a part that is not an LED keeps its material names");
    // Two materials reach the top: the one spanning the larger area is the lens; a supplier `lens` is renamed.
    let tie = boxes(&[("lens", [0.9, 0.4, 0.0], [1.1, 0.6, 0.6]), ("wide", [0.0, 0.0, 0.0], [2.0, 1.0, 0.6])]);
    assert_eq!(names(&led, &tie)?, ["lens 0", "lens"], "on a tie at the top the larger area is the lens, and one material carries the name");
    Ok(())
}

#[test]
fn a_model_sits_on_its_pads() -> Result<(), Issue> {
    // The resistor's footprint with its model drawn at `outline` (canvas units; the footprint origin
    // is (10, 20), 0.254 mm a unit, canvas Y down) and turned 0, sinking `z` units into the board.
    let node = r#"SVGNODE~{\"attrs\":{\"c_etype\":\"outline3D\",\"c_rotation\":\"0,0,90\",\"c_origin\":\"12,19\",\"z\":0}}"#;
    let part = |pads: [&str; 2], outline: &str, z: &str| {
        RESISTOR
            .replace("PAD~RECT~8~20~2~1~1~~1~0~~0~pad1~0~~Y", pads[0])
            .replace("PAD~RECT~12~20~2~1~1~~2~0~~90~pad2~0~~Y", pads[1])
            .replace(node, &format!(
                r#"SVGNODE~{{\"attrs\":{{\"c_etype\":\"outline3D\",\"c_rotation\":\"0,0,0\",\"c_origin\":\"10,20\",\"z\":{z}}},\"childNodes\":[{{\"attrs\":{{\"points\":\"{outline}\"}}}}]}}"#))
    };
    let fit_warnings = |component: &str, model: &str| -> Result<Vec<String>, Issue> {
        let converted = Lcsc.convert("C900004", &raw(component, Some(model)))?;
        Ok(converted.warnings.into_iter().filter(|w| w.message.starts_with("the 3D model")).map(|w| w.message).collect())
    };
    let centred = "6 18 14 18 14 22";

    // Surface pads 1.016 mm square at x = -/+1.524 mm; terminals 0.3 mm square on their centres, the
    // body between them clear of the board.
    let smd = ["PAD~RECT~4~20~4~4~1~~1~0~~0~pad1~0~~Y", "PAD~RECT~16~20~4~4~1~~2~0~~0~pad2~0~~Y"];
    let chip = boxes(&[("end", [-1.674, -0.15, 0.0], [-1.374, 0.15, 0.2]), ("body", [-1.374, -0.15, 0.15], [1.374, 0.15, 0.2]),
        ("end", [1.374, -0.15, 0.0], [1.674, 0.15, 0.2])]);
    assert!(fit_warnings(&part(smd, centred, "0"), &chip)?.is_empty(), "terminals on their pads sit");
    // Moved 4 units (1.016 mm) along X, neither terminal reaches its pad.
    assert_eq!(fit_warnings(&part(smd, "10 18 18 18 18 22", "0"), &chip)?,
        ["the 3D model does not reach pad 1, pad 2 (within 0.1mm)"], "a model moved off its pads names them");
    // Moved 2 units (0.508 mm) along Y, both terminals still touch their pads but sit off their centres.
    assert_eq!(fit_warnings(&part(smd, "6 16 14 16 14 20", "0"), &chip)?,
        ["the 3D model sits 0.48mm off its pads on average (more than 0.25mm)"], "a model off its pads' centres is reported");

    // Through pads with 0.762 mm drills at x = -/+1.016 mm; legs 0.4 mm square sink 0.762 mm (3 units).
    let tht = ["PAD~ELLIPSE~6~20~6~6~11~~1~1.5~~0~pad1~0~~Y", "PAD~ELLIPSE~14~20~6~6~11~~2~1.5~~0~pad2~0~~Y"];
    let legs = [("leg", [-1.216, -0.2, -0.762], [-0.816, 0.2, 0.0]), ("leg", [0.816, -0.2, -0.762], [1.216, 0.2, 0.0])];
    let body = ("body", [-1.6, -0.6, 0.0], [1.6, 0.6, 1.0]);
    let header = boxes(&[legs[0], body, legs[1]]);
    assert!(fit_warnings(&part(tht, centred, "-3"), &header)?.is_empty(), "legs in their holes sit");
    // Moved 4 units along X, the body still covers both holes at the board, but its legs enter neither.
    assert_eq!(fit_warnings(&part(tht, "10 18 18 18 18 22", "-3"), &header)?,
        ["the 3D model does not reach pad 1, pad 2 (within 0.1mm)"], "a model with legs must reach its holes with them");
    // A model whose pins stop at the surface reaches its holes at the board.
    assert!(fit_warnings(&part(tht, centred, "0"), &boxes(&[body]))?.is_empty(), "a model without legs sits over its holes");
    Ok(())
}

//! Edge softness is common to drawn shapes, independent of post blur.

use makepad_motion::*;

const SHAPES: &[(&str, &str)] = &[
    ("Rect", "size: vec2(80, 60) fill: #fff"),
    ("Ellipse", "size: vec2(80, 60) fill: #fff"),
    ("Path", "svg: \"M0 0 L80 0 L80 60 L0 60 Z\" fill: #fff"),
    ("Text", "text: \"Soft edges\" size: 32"),
    ("Svg", "svg: \"<svg width='80' height='60'><rect width='80' height='60'/></svg>\""),
    ("Vector", "svg: \"<svg width='80' height='60'><rect width='80' height='60'/></svg>\""),
];

fn source(kind: &str, fields: &str, common: &str) -> String {
    format!("use mod.motion.*; Motion{{size: vec2(320, 180) duration: 4 bpm: 120 layers: [{kind}{{{fields} {common}}}]}}")
}

#[test]
fn all_edge_kinds_keep_feather_and_side_on_the_item() {
    for &(kind, fields) in SHAPES {
        for (side, expected) in [("both", FeatherSide::Both), ("inner", FeatherSide::Inner), ("outer", FeatherSide::Outer)] {
            let doc = MotionDoc::load(&source(kind, fields, &format!("feather: 24 feather_side: @{side} blur: 3"))).unwrap();
            assert!(doc.warnings.is_empty(), "{kind}: {:?}", doc.warnings);
            assert_eq!(doc.layers[0].feather.constant(), Some([24.0]));
            assert_eq!(doc.layers[0].feather_side, expected);
            let frame = doc.eval(0.5);
            assert!(frame.errors.is_empty(), "{kind}: {:?}", frame.errors);
            let item = &frame.items[0];
            assert_eq!(item.feather, 24.0, "{kind}");
            assert_eq!(item.feather_side, expected, "{kind}");
            assert_eq!(item.blur, 3.0, "{kind}: post blur is independent");
            assert!(item.is_static, "{kind}");
        }
    }
}

#[test]
fn feather_defaults_to_zero_and_both_sides() {
    for &(kind, fields) in SHAPES {
        for common in ["", "feather: 0", "feather: 12"] {
            let doc = MotionDoc::load(&source(kind, fields, common)).unwrap();
            let frame = doc.eval(0.5);
            assert_eq!(frame.items[0].feather_side, FeatherSide::Both, "{kind}");
            assert_eq!(frame.items[0].feather, if common == "feather: 12" { 12.0 } else { 0.0 }, "{kind}");
            assert!(doc.warnings.is_empty(), "{kind}: {:?}", doc.warnings);
        }
    }
}

#[test]
fn keyed_feather_animates_each_kind_and_resolves_beat_times() {
    for &(kind, fields) in SHAPES {
        let doc = MotionDoc::load(&source(kind, fields, "span: [1, 3] feather: keys([beat(2), 0], [beat(4), 40]) feather_side: @outer")).unwrap();
        assert!(doc.layers[0].animated, "{kind}");
        assert!(uses_musical_time(&doc), "{kind}");
        // At 120 bpm the absolute beat keys are at 1 and 2 s; resolving
        // the layer's span moves them to 0 and 1 on its own clock.
        for (t, expected) in [(1.0, 0.0), (1.5, 20.0), (2.0, 40.0)] {
            let frame = doc.eval(t);
            assert!(frame.errors.is_empty(), "{kind}: {:?}", frame.errors);
            assert!((frame.items[0].feather - expected).abs() < 1e-6, "{kind} at {t}: {:?}", frame.items[0]);
            assert_eq!(frame.items[0].feather_side, FeatherSide::Outer);
            assert!(!frame.items[0].is_static, "{kind}");
        }
        assert_eq!(doc.eval(1.5), doc.eval(1.5), "{kind}: evaluation stays pure");
    }
}

#[test]
fn split_text_keeps_its_layers_edge_softness() {
    let doc = MotionDoc::load(&source("Text", "text: \"Soft\" by: @letter stagger: 0.1", "feather: tween(0, 20, 0, 1) feather_side: @inner blur: 2")).unwrap();
    let frame = doc.eval(0.5);
    let item = &frame.items[0];
    assert_eq!(item.feather, 10.0);
    assert_eq!(item.feather_side, FeatherSide::Inner);
    assert_eq!(item.blur, 0.0);
    let ItemKind::Text { units, .. } = &item.kind else { panic!() };
    assert_eq!(units.len(), 4);
    assert!(units.iter().all(|unit| unit.blur == 2.0));
}

#[test]
fn text_animators_keep_edge_fields_on_the_texts_they_make() {
    let doc = MotionDoc::load(&source("Text", "text: \"Soft|Edges\" animate: @word_swap", "feather: 12 feather_side: @outer")).unwrap();
    let frame = doc.eval(0.8);
    let ItemKind::Group { children, .. } = &frame.items[0].kind else { panic!() };
    assert!(!children.is_empty());
    assert!(children.iter().all(|item| item.feather == 12.0 && item.feather_side == FeatherSide::Outer));
    let errors = MotionDoc::load(&source("Text", "text: \"Soft\" animate: @scramble", "feather: 12")).unwrap_err();
    assert!(errors.iter().any(|d| d.is_error() && d.path == "layers[0].feather" && d.message.contains("use blur for KineticText")), "{errors:?}");
}

#[test]
fn negative_feather_constants_and_keys_are_load_errors() {
    for &(kind, fields) in SHAPES {
        for feather in ["-1", "tween(0, -12, 0, 1)", "keys([0, 4], [1, -2], [2, 4])"] {
            let errors = MotionDoc::load(&source(kind, fields, &format!("feather: {feather}"))).unwrap_err();
            assert!(errors.iter().any(|d| d.is_error() && d.path == "layers[0].feather" && d.message.contains("non-negative")), "{kind}: {errors:?}");
        }
    }
}

#[test]
fn other_layer_kinds_reject_feather_and_name_the_field() {
    for kind in layer_types().filter(|kind| !SHAPES.iter().any(|(shape, _)| shape == kind)) {
        let errors = MotionDoc::load(&source(kind, "", "feather: 12")).unwrap_err();
        let message = format!("feather softens shape, path, text and SVG edges; use blur for {kind}");
        assert!(errors.iter().any(|d| d.is_error() && d.path == "layers[0].feather" && d.message == message), "{kind}: {errors:?}");
    }
}

#[test]
fn a_side_without_feather_warns_and_bad_sides_are_errors() {
    for &(kind, fields) in SHAPES {
        let doc = MotionDoc::load(&source(kind, fields, "feather_side: @inner")).unwrap();
        assert!(doc.warnings.iter().any(|d| d.path == "layers[0].feather_side" && d.message.contains("without `feather`")), "{kind}: {:?}", doc.warnings);
        assert_eq!(doc.eval(0.5).items[0].feather, 0.0);
        let errors = MotionDoc::load(&source(kind, fields, "feather: 12 feather_side: @outside")).unwrap_err();
        assert!(errors.iter().any(|d| d.is_error() && d.path == "layers[0].feather_side" && d.to_string().contains("@both") && d.to_string().contains("@inner") && d.to_string().contains("@outer")), "{kind}: {errors:?}");
    }
    let doc = MotionDoc::load(&source("Group", "layers: []", "feather_side: @outer")).unwrap();
    assert!(doc.warnings.iter().any(|d| d.path == "layers[0].feather_side" && d.message.contains("without `feather`")), "{:?}", doc.warnings);
}

#[test]
fn existing_fixture_documents_still_load_and_evaluate() {
    for name in ["kinetic_title", "logo_reveal", "lower_third", "match_morph"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(format!("{name}.splash"));
        let doc = MotionDoc::load(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(doc.warnings.is_empty(), "{name}: {:?}", doc.warnings);
        for t in [0.0, doc.duration * 0.5, doc.duration - 0.01] {
            let frame = doc.eval(t);
            assert!(frame.errors.is_empty(), "{name} at {t}: {:?}", frame.errors);
        }
    }
}

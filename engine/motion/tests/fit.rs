//! `fit:` and `wrap: @safe` (text kept inside the frame's side margins) and
//! the bundled serif and handwritten faces.

use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

#[test]
fn fit_and_wrap_take_a_width_or_the_safe_width() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1080, 1920) duration: 1.0 layers: [
        Text{text: "A" fit: @safe wrap: 900}
        Text{text: "B" fit: 700 wrap: @safe}
    ]}"#);
    let frame = doc.eval(0.5);
    let fits: Vec<(Option<f64>, Option<f64>)> = frame.items.iter().map(|item| match &item.kind {
        ItemKind::Text { fit, wrap, .. } => (*fit, *wrap),
        _ => panic!(),
    }).collect();
    assert_eq!(fits, vec![(Some(1080.0 * SAFE_WIDTH), Some(900.0)), (Some(700.0), Some(1080.0 * SAFE_WIDTH))]);
    let diags = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(1080, 1920) duration: 1.0 layers: [Text{text: "A" fit: "wide"}]}"#).unwrap_err();
    assert!(diags.iter().any(|d| d.message.contains("@safe")), "{diags:?}");
}

#[test]
fn a_kinetic_text_fits_by_drawing_its_picture_smaller() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 1.0 layers: [KineticText{preset: "318_letter_flip" text: "HI" fit: @safe}]}"#);
    let item = &doc.eval(0.5).items[0];
    assert!((item.local.scale[0] - SAFE_WIDTH).abs() < 1e-9 && (item.local.scale[1] - SAFE_WIDTH).abs() < 1e-9, "{:?}", item.local);
    // An explicit scale wins.
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 1.0 layers: [KineticText{preset: "318_letter_flip" text: "HI" fit: @safe scale: 0.5}]}"#);
    assert_eq!(doc.eval(0.5).items[0].local.scale, [0.5, 0.5]);
}

#[test]
fn serif_display_and_handwritten_faces_are_reachable_by_name() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 1.0 layers: [
        Text{text: "A" font: Font{family: "serif"}} Text{text: "C" font: Font{family: "LXGW WenKai"}}
        Text{text: "D" font: Font{family: "Playfair Display"}} Text{text: "E" font: Font{family: "editorial"}} Text{text: "F" font: Font{family: "condensed"}}
    ]}"#);
    let frame = doc.eval(0.5);
    let fonts: Vec<String> = frame.items.iter().map(|item| match &item.kind {
        ItemKind::Text { font, .. } => font.to_string(),
        _ => panic!(),
    }).collect();
    assert_eq!(fonts, vec!["New Computer Modern", "LXGW WenKai", "Playfair Display", "Cormorant Garamond", "Anton"]);
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    // A family that is not a document font is an error, not a look-alike. (Not a family of the
    // external-font catalogue: whether one of those loads depends on what this machine installed.)
    for name in ["Georgia", "Times New Roman"] {
        let errors = MotionDoc::load(&format!("use mod.motion.*; Motion{{size: vec2(1920, 1080) duration: 1.0 layers: [Text{{text: \"A\" font: Font{{family: \"{name}\"}}}}]}}")).unwrap_err();
        assert!(errors.iter().any(|d| d.message.contains(name) && d.message.contains("no document font")), "{name}: {errors:?}");
    }
}

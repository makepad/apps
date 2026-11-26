//! Gaps found authoring the reference-film plates (FP1): template keys at
//! time 0, outlined text.

use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}\n{source}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

fn wrap(layers: &str) -> String {
    format!("use mod.motion.*; Motion{{size: vec2(1920, 1080) duration: 3.0 layers: [\n{layers}\n]}}")
}

fn texts(frame: &MotionFrame) -> Vec<Item> {
    frame.walk().into_iter().filter(|(_, i)| matches!(i.kind, ItemKind::Text { .. })).map(|(_, i)| i.clone()).collect()
}

/// A key at 0 s in a template's own schedule repeated its "nothing yet"
/// key at 0 s: "key times must increase: 0 s comes after 0 s".
#[test]
fn templates_take_a_key_at_time_zero() {
    let doc = load(&wrap(r#"TokenStream{prompt: "Moonlit, harbor" tokens: ["Moon", "lit, ", "harbor"] times: [0.0, 0.5, 1.0] enter: true}"#));
    let typed = |t: f64| texts(&doc.eval(t)).iter().find_map(|i| match &i.kind {
        ItemKind::Text { text, reveal: Some(n), caret: Some(_), .. } if text.starts_with("Moonlit") => Some(*n),
        _ => None,
    }).unwrap();
    // The first token is there from 0.
    assert_eq!((typed(0.0), typed(0.3), typed(0.6), typed(1.1)), (4, 4, 9, 15));
    for burst in ["0.0", "0", "-0.2", "0.01", "1.0"] {
        let doc = load(&wrap(&format!("Rays{{burst: {burst} count: 12}}")));
        for t in [0.0, 0.3, 1.0, 2.0] {
            let frame = doc.eval(t);
            assert!(frame.errors.is_empty(), "burst {burst} at {t}: {:?}", frame.errors);
        }
    }
}

/// `Text{stroke: … width: …}` was accepted and drew nothing: the frame now
/// carries the outline (motion_draw strokes the glyph outlines).
#[test]
fn text_stroke_reaches_the_frame() {
    let doc = load(&wrap(r#"
        Text{text: "HOLLOW" font: Font{family: "Roboto Flex" weight: 900} size: 150 color: #0000 stroke: #ff5a1f width: 3}
        Text{text: "RIM" size: 90 stroke: #ffffff}
        Text{text: "keyed" size: 90 stroke: #ffffff width: tween(0, 6, 0, 2)}
        Text{text: "PLAIN" size: 90}"#));
    let frame = doc.eval(1.0);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    let outline = |item: &Item| match &item.kind { ItemKind::Text { outline, .. } => *outline, _ => unreachable!() };
    let items = texts(&frame);
    assert_eq!(outline(&items[0]), Some(([1.0, 90.0 / 255.0, 31.0 / 255.0, 1.0], 3.0)));
    assert_eq!(outline(&items[1]).map(|o| o.1), Some(2.0), "width defaults to 2");
    assert!((outline(&items[2]).unwrap().1 - 3.0).abs() < 1e-9, "width is keyable");
    assert_eq!(outline(&items[3]), None);
    // A look that draws its own outline from `stroke` does not outline its copies.
    let doc = load(&wrap(r#"Text{text: "POW" size: 120 color: #ffd400 stroke: #111111 animate: @outline_fill start: 0}"#));
    assert!(texts(&doc.eval(2.5)).iter().all(|i| outline(i).is_none()));
    // The outlines, as contours where a Text draws the same text.
    let contours = text_outline_contours("HOLLOW", "Roboto Flex", 150.0, 900.0, false, &[]).unwrap();
    assert!(contours.len() >= 8 && contours.iter().all(|c| c.len() >= 3), "{} contours", contours.len());
    assert!(text_outline_contours("x", "No Such Face", 50.0, 400.0, false, &[]).is_none());
}

/// A property function may return `emit(colour, k)`: an HDR colour keyed
/// over time (it failed "returned object").
#[test]
fn property_functions_return_emissive_colours() {
    let doc = load(&wrap(r#"Rect{size: vec2(10, 10) fill: fn(t) { if t < 1.0 { #ff0000 } else { emit(#ff0000, 4) } }}"#));
    let fill = |t: f64| {
        let frame = doc.eval(t);
        assert!(frame.errors.is_empty(), "{:?}", frame.errors);
        match &frame.items[0].kind { ItemKind::Rect { fill: Some(c), .. } => *c, other => panic!("{other:?}") }
    };
    assert_eq!(fill(0.5), [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(fill(2.0), [4.0, 0.0, 0.0, 1.0]);
    // Keyed over time, as a written emit() is.
    let doc = load(&wrap(r#"Ellipse{size: vec2(10, 10) fill: fn(t) { emit(#ffffff, 1.0 + t) }}"#));
    let frame = doc.eval(2.0);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    assert!(matches!(&frame.items[0].kind, ItemKind::Ellipse { fill: Some(c), .. } if (c[0] - 3.0).abs() < 1e-9 && c[3] == 1.0));
}

/// `log(x)` is the natural log in every part of a document: a document that
/// imports `mod.std` (whose `log` prints) still gets the maths, as its
/// shaders and scenes do; `ln(x)` is the same function; printing stays
/// `print(…)` and `mod.std.log(…)`.
#[test]
fn log_is_the_natural_log_even_after_use_mod_std() {
    for (name, head) in [("plain", ""), ("std", "use mod.std.*\n"), ("std_comment", "use mod.std.* // printing\n"), ("std_log", "use mod.std.log\n")] {
        let doc = load(&format!("{head}let e = 2.718281828\nlet k = log(e * e)\n{}", wrap("Rect{size: vec2(10, 10) fill: #fff opacity: fn(t) { log(e) * 0.5 + ln(e) * 0.25 + k * 0.0 }}")));
        let frame = doc.eval(1.0);
        assert!(frame.errors.is_empty(), "{name}: {:?}", frame.errors);
        assert!((frame.items[0].opacity - 0.75).abs() < 1e-4, "{name}: {}", frame.items[0].opacity);
    }
    // Printing is still there.
    let doc = load(&format!("use mod.std.*\nprint(\"\")\nmod.std.log(1)\n{}", wrap("Rect{size: vec2(10, 10) fill: #fff}")));
    assert!(doc.eval(0.0).errors.is_empty());
}

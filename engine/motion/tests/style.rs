//! Style kits (`Motion{style: @kit}`): every kit lowers to plain layers
//! that load and evaluate, the author's fields and the style's own
//! overrides win, and a kit's printed parts load as ordinary layers.

use makepad_motion::plain::{PrimitiveFrame, TextProps};
use makepad_motion::style::{detached_source, kits};
use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}\n{source}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

fn doc(style: &str, layers: &str) -> String {
    format!("use mod.motion.*; Motion{{size: vec2(1280, 720) duration: 3.0 style: {style} layers: [{layers}]}}")
}

const LAYERS: &str = r#"Text{text: "Make things\nthat move" size: 96 at: vec2(80, 200)} Text{text: "A kit in one line" size: 28 at: vec2(84, 520)}"#;

fn names(layers: &[Layer], out: &mut Vec<String>) {
    for layer in layers {
        if let Some(name) = &layer.name {
            out.push(name.clone());
        }
        if let LayerKind::Group { layers, .. } = &layer.kind {
            names(layers, out);
        }
    }
}

#[test]
fn every_kit_lowers_loads_and_evaluates() {
    let kits = kits();
    assert!(kits.len() >= 12);
    for kit in kits {
        for style in [format!("@{}", kit.name), format!("{{kit: @{} fx: true}}", kit.name)] {
            let doc = load(&doc(&style, LAYERS));
            let mut found = Vec::new();
            names(&doc.layers, &mut found);
            assert!(found.iter().any(|n| n == "style.plate"), "{}: {found:?}", kit.name);
            assert!(doc.background.is_some(), "{}: the kit's paper is the background", kit.name);
            for t in [0.0, 0.4, 1.2, 2.9] {
                let frame = doc.eval(t);
                assert!(frame.errors.is_empty(), "{} at {t}: {:?}", kit.name, frame.errors);
            }
        }
    }
}

#[test]
fn text_takes_the_kits_type_and_motion_where_the_author_left_it_open() {
    let d = load(&doc("@swiss", r#"Text{text: "Title" size: 96} Text{text: "Mine" size: 96 font: Font{family: "Anton"} color: #00ff00 animate: @none}"#));
    let mut texts = Vec::new();
    fn walk<'a>(layers: &'a [Layer], out: &mut Vec<&'a Layer>) {
        for l in layers {
            match &l.kind {
                LayerKind::Group { layers, .. } => walk(layers, out),
                LayerKind::Text { .. } => out.push(l),
                _ => {}
            }
        }
    }
    walk(&d.layers, &mut texts);
    let fonts: Vec<(String, f64, f64)> = texts.iter().filter_map(|l| match &l.kind {
        LayerKind::Text { props: TextProps { font, weight, tracking: Prop::Const([tracking]), .. }, .. } => Some((font.clone(), *weight, *tracking)),
        _ => None,
    }).collect();
    assert!(fonts.iter().any(|(f, w, t)| f == "Inter" && *w == 900.0 && *t < 0.0), "the kit's display face: {fonts:?}");
    assert!(fonts.iter().any(|(f, _, _)| f == "Anton"), "the author's font wins: {fonts:?}");
    // The author's own colour survives, and `animate: @none` leaves it still.
    let frame = d.eval(0.0);
    fn find<'a>(items: &'a [Item], name: &str) -> Option<&'a Item> {
        items.iter().find_map(|i| match &i.kind {
            ItemKind::Primitive(PrimitiveFrame::Text { text, .. }) if &**text == name => Some(i),
            ItemKind::Group { children, .. } => find(children, name),
            _ => None,
        })
    }
    let mine = find(&frame.items, "Mine").expect("Mine drawn");
    assert!(mine.opacity > 0.99, "no animator on `animate: @none`");
}

#[test]
fn a_style_takes_its_own_overrides() {
    let plain = load(&doc("@bauhaus", LAYERS));
    let bare = load(&doc("{kit: @bauhaus plate: false post: false ornaments: false}", LAYERS));
    let mut a = Vec::new();
    names(&plain.layers, &mut a);
    let mut b = Vec::new();
    names(&bare.layers, &mut b);
    assert!(a.iter().any(|n| n == "style.post") && a.iter().any(|n| n == "style.plate"));
    assert!(!b.iter().any(|n| n.starts_with("style.")), "{b:?}");
    let red = load(&doc("{kit: @swiss bg: #ff0000}", LAYERS));
    assert_eq!(red.background.map(|c| (c[0] * 255.0).round() as u32), Some(255));
    let err = MotionDoc::load(&doc("@swis", LAYERS)).unwrap_err();
    assert!(err.iter().any(|d| d.message.contains("Did you mean `swiss`")), "{err:?}");
    let err = MotionDoc::load(&doc("{kit: @swiss colour: #ff0000}", LAYERS)).unwrap_err();
    assert!(err.iter().any(|d| d.message.contains("`style.colour`")), "{err:?}");
}

#[test]
fn a_kits_printed_parts_load_as_plain_layers() {
    for kit in kits() {
        let text = detached_source(&kit.name, [1080.0, 1920.0]).unwrap();
        let source = format!("use mod.motion.*; {text}\nMotion{{size: vec2(1080, 1920) duration: 2.0 layers: parts.plate}}");
        let doc = load(&source);
        assert!(!doc.layers.is_empty(), "{}", kit.name);
        assert!(doc.eval(1.0).errors.is_empty());
    }
}

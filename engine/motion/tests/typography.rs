//! Named typography animators (`Text{animate: @…}`): each one loads, and
//! the ones that are the Text's own fields leave what the author set.

use makepad_motion::typography::animators;
use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

#[test]
fn every_animator_loads_and_evaluates() {
    let all = animators();
    assert!(all.len() >= 30, "{}", all.len());
    for (name, about, suits) in &all {
        assert!(!about.is_empty() && !suits.is_empty(), "{name}");
        let extra = match name.as_str() {
            "on_path" => " path: \"M 0 300 C 400 0, 800 600, 1200 300\"",
            "word_swap" => " words: [\"fast\", \"simple\", \"yours\"]",
            _ => "",
        };
        let doc = load(&format!("use mod.motion.*; Motion{{size: vec2(1280, 720) duration: 4.0 layers: [Text{{text: \"Make things that move\" size: 64 at: vec2(640, 360) animate: @{name}{extra}}}]}}"));
        for t in [0.0, 0.5, 1.5, 3.9] {
            let frame = doc.eval(t);
            assert!(frame.errors.is_empty(), "{name} at {t}: {:?}", frame.errors);
        }
    }
}

#[test]
fn animators_set_only_what_the_author_left_open() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 3.0 layers: [Text{text: "Hello world" animate: @fade_up by: @letter start: 1.0}]}"#);
    let ItemKind::Text { units, .. } = &doc.eval(0.5).items[0].kind else { panic!() };
    assert_eq!(units.len(), 10, "the author's `by: @letter` wins over the look's @word");
    assert!(units.iter().all(|u| u.opacity < 0.01), "nothing before `start`");
    let ItemKind::Text { units, .. } = &doc.eval(2.9).items[0].kind else { panic!() };
    assert!(units.iter().all(|u| u.opacity > 0.99));
}

#[test]
fn looks_become_the_layers_that_draw_them() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 6.0 layers: [
        Text{text: "Launch" animate: @scramble at: vec2(100, 100)}
        Text{text: "x" animate: @word_swap words: ["fast", "simple", "yours"] hold: 1.0}
        Text{text: "Make things that move" animate: @stack size: vec2(900, 600)}
        Text{text: "Hello" animate: @typewriter}
        Text{text: "Key" animate: @underline}
    ]}"#);
    let frame = doc.eval(1.5);
    assert!(matches!(&frame.items[0].kind, ItemKind::Shader { text: Some(_), .. }), "a scramble is vj_fx's decode preset");
    let ItemKind::Group { children, .. } = &frame.items[1].kind else { panic!() };
    let shown: Vec<String> = children.iter().filter(|c| c.opacity > 0.5).filter_map(|c| match &c.kind { ItemKind::Text { text, .. } => Some(text.clone()), _ => None }).collect();
    assert_eq!(shown, vec!["simple"], "the second word holds at 1.5 s");
    let ItemKind::Group { children, .. } = &frame.items[2].kind else { panic!() };
    assert_eq!(children.len(), 4, "one big word a line");
    let ItemKind::Text { caret, reveal, .. } = &doc.eval(0.3).items[3].kind else { panic!() };
    assert!(caret.is_some() && reveal.is_some_and(|r| r < 5));
    let ItemKind::Text { plate: Some(plate), .. } = &doc.eval(2.0).items[4].kind else { panic!() };
    assert!(plate.underline && plate.reveal > 0.99);
    let diags = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 1.0 layers: [Text{text: "x" animate: @fade_upp}]}"#).unwrap_err();
    assert!(diags.iter().any(|d| d.message.contains("@fade_up") || d.hint.as_deref().is_some_and(|h| h.contains("fade_up"))), "{diags:?}");
}

#[test]
fn mattes_line_masks_outlines_and_text_as_a_matte() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 4.0 layers: [
        Text{text: "Ideas\ndon't wait" size: 90 at: vec2(40, 60) animate: @mask_up}
        Text{text: "OUTLINE" size: 110 at: vec2(660, 70) animate: @outline_fill stroke: #ffd400}
        Text{text: "INSIDE" size: 200 at: vec2(40, 300) inside: Pattern{kind: @stripes size: vec2(1280, 720)}}
        Group{size: vec2(460, 320) mask_mode: @luma_inverse mask_feather: 12 mask: Path{svg: "M 0 0 L 100 0 L 100 100 Z"} layers: [Rect{size: vec2(460, 320) fill: #ffffff}]}
    ]}"#);
    let frame = doc.eval(1.0);
    // A line matte: the moving text inside its own line boxes.
    let ItemKind::Group { children: lines, .. } = &frame.items[0].kind else { panic!("{:?}", frame.items[0].kind) };
    assert_eq!(lines.len(), 2, "one matte group per line");
    let ItemKind::Group { children, mask: Some(mask), mask_mode, .. } = &lines[1].kind else { panic!() };
    assert_eq!(*mask_mode, MaskMode::Alpha);
    // The whole text, laid out once, showing only its second line; the
    // matte is that line's own plate.
    let shown = |item: &Item| match &item.kind {
        ItemKind::Text { text, units, .. } => units.iter().filter(|u| u.opacity > 0.0).map(|u| text[u.range.clone()].to_string()).collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    assert!(matches!(&children[0].kind, ItemKind::Text { text, .. } if text == "Ideas\ndon't wait"));
    assert_eq!(shown(&children[0]), vec!["don't wait".to_string()]);
    assert!(matches!(&mask.kind, ItemKind::Text { plate: Some(p), .. } if p.each), "the matte is the line's own box");
    assert_eq!(shown(mask), vec!["don't wait".to_string()]);
    // An outline: nudged copies with the text cut out, then the fill.
    let ItemKind::Group { children, .. } = &frame.items[1].kind else { panic!() };
    let ItemKind::Group { children: ring, mask: Some(_), mask_mode, .. } = &children[0].kind else { panic!() };
    assert_eq!((ring.len(), *mask_mode), (12, MaskMode::Inverse));
    // Footage inside type.
    let ItemKind::Group { children, mask: Some(mask), .. } = &frame.items[2].kind else { panic!() };
    assert!(matches!(&children[0].kind, ItemKind::Shader { .. }) && matches!(&mask.kind, ItemKind::Text { .. }));
    // A luma matte of a feathered shape: the feather is the mask's blur, and
    // a mask Path without a fill shows its whole area.
    let ItemKind::Group { mask: Some(mask), mask_mode, .. } = &frame.items[3].kind else { panic!() };
    assert_eq!(*mask_mode, MaskMode::LumaInverse);
    assert_eq!(mask.blur, 12.0);
    assert!(matches!(&mask.kind, ItemKind::Path { fill: Some(_), .. }));
}

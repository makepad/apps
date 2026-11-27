use makepad_motion::plain::PrimitiveFrame;
use makepad_motion::*;
use std::path::PathBuf;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn load(name: &str) -> MotionDoc {
    let source = std::fs::read_to_string(dir().join(format!("{name}.splash"))).unwrap();
    match MotionDoc::load(&source) {
        Ok(doc) => doc,
        Err(diags) => panic!("{name}:\n{}", diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")),
    }
}

fn frames(doc: &MotionDoc) -> Vec<u64> {
    let mut out = Vec::new();
    let mut t = 0.0;
    while t < doc.duration {
        let frame = doc.eval(t);
        assert!(frame.errors.is_empty(), "{:?}", frame.errors);
        out.push(frame.hash());
        t += 0.5;
    }
    out
}

#[test]
fn fixtures_load_cleanly_and_evaluate_the_same_every_time() {
    for name in ["kinetic_title", "logo_reveal", "lower_third", "match_morph"] {
        let doc = load(name);
        assert!(doc.warnings.is_empty(), "{name}: {}", doc.warnings.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"));
        let got = frames(&doc);
        assert_eq!(got, frames(&doc), "{name}: a second evaluation differs");
        assert_eq!(got, frames(&load(name)), "{name}: a second load differs");
    }
}

#[test]
fn the_kinetic_title_moves_letter_by_letter() {
    let doc = load("kinetic_title");
    assert_eq!(doc.duration, 6.0);
    assert_eq!(doc.inputs.len(), 3);
    let at = |t: f64| doc.eval(t);
    let headline = |frame: &MotionFrame| frame.items.iter().find(|item| item.path == "layers[1]").cloned().unwrap();
    let ItemKind::Primitive(PrimitiveFrame::Text { units, text, .. }) = headline(&at(0.3)).kind else { panic!() };
    assert_eq!(text, "Notes, instantly");
    assert_eq!(units.len(), "Notes,instantly".chars().count(), "letters, spaces skipped");
    // Staggered: an early letter is further along than a late one.
    assert!(units[0].opacity > units[10].opacity, "{} vs {}", units[0].opacity, units[10].opacity);
    assert!(units[0].offset[1] < units[10].offset[1]);
    // Settled mid-way: every letter in place.
    let ItemKind::Primitive(PrimitiveFrame::Text { units, .. }) = headline(&at(3.0)).kind else { panic!() };
    assert!(units.iter().all(|u| u.opacity == 1.0 && u.offset[1].abs() < 1e-9 && u.rotate.abs() < 1e-9));
    // The bar draws itself between 0.9 s and 1.7 s.
    let bar = |t: f64| match at(t).items.iter().find(|i| i.path == "layers[2]").unwrap().kind.clone() {
        ItemKind::Primitive(PrimitiveFrame::Path { trim, .. }) => trim,
        _ => panic!(),
    };
    assert_eq!(bar(0.5), [0.0, 0.0]);
    assert!(bar(1.3)[1] > 0.3 && bar(1.3)[1] < 0.7);
    assert_eq!(bar(3.0), [0.0, 1.0]);
    // Only the glow's first key is before 0.8 s: it is animated, as is everything here.
    assert!(doc.layers.iter().all(|l| l.animated));
    // Inputs fill in.
    let other = doc.with_inputs(&[(LiveId::from_str("headline"), InputValue::Text("Hi".into())), (LiveId::from_str("accent"), InputValue::Color([0.0, 1.0, 0.0, 1.0]))]).unwrap();
    let ItemKind::Primitive(PrimitiveFrame::Text { text, .. }) = other.eval(1.0).items[1].kind.clone() else { panic!() };
    assert_eq!(text, "Hi");
    let ItemKind::Primitive(PrimitiveFrame::Path { stroke, .. }) = other.eval(1.0).items[2].kind.clone() else { panic!() };
    assert_eq!(stroke, Some([0.0, 1.0, 0.0, 1.0]));
}

#[test]
fn the_logo_reveal_springs_its_squares_in_turn() {
    let doc = load("logo_reveal");
    let squares = |t: f64| {
        let frame = doc.eval(t);
        let group = frame.items.iter().find(|i| i.path == "layers[1]").unwrap().clone();
        let ItemKind::Group { children, .. } = group.kind else { panic!() };
        children.iter().map(|c| c.local.scale[0]).collect::<Vec<_>>()
    };
    let early = squares(1.55);
    assert!(early[0] > early[1] && early[1] > early[2], "{early:?}");
    // The spring overshoots before it settles.
    assert!((0..40).map(|k| squares(1.4 + k as f64 * 0.02)[0]).any(|s| s > 1.02));
    assert!(squares(3.9).iter().all(|s| (s - 1.0).abs() < 0.01));
    // The prototype's fields reach its instances.
    let frame = doc.eval(3.0);
    let ItemKind::Group { children, .. } = &frame.items[1].kind else { panic!() };
    assert!(matches!(children[0].kind, ItemKind::Primitive(PrimitiveFrame::Rect { size: [56.0, 56.0], radius: 10.0, .. })));
    // The ring's group turns from -90 degrees: its children carry the turn.
    let ItemKind::Group { children, .. } = &doc.eval(0.0).items[0].kind else { panic!() };
    let p = children[0].world.apply([100.0, 0.0]);
    assert!((p[0] - 540.0).abs() < 1e-6 && (p[1] - 320.0).abs() < 1e-6, "{p:?}");
}

#[test]
fn the_lower_third_is_masked_and_leaves() {
    let doc = load("lower_third");
    let plate = |t: f64| doc.eval(t).items[0].clone();
    let ItemKind::Group { mask, children, .. } = plate(1.0).kind else { panic!() };
    assert!(mask.is_some());
    assert_eq!(children.len(), 4);
    let panel_x = |t: f64| match &plate(t).kind {
        ItemKind::Group { children, .. } => children[0].world.e,
        _ => panic!(),
    };
    assert_eq!(panel_x(0.0), 120.0 - 720.0);
    assert_eq!(panel_x(2.0), 120.0);
    assert!(panel_x(4.8) < 0.0);
}

#[test]
fn the_built_in_title_centres_its_text_on_the_position() {
    let doc = MotionDoc::load(builtin::TITLE).unwrap();
    let doc = doc.with_inputs(&[
        (LiveId::from_str("text"), InputValue::Text("Hi".into())),
        (LiveId::from_str("frame"), InputValue::Vec2([640.0, 360.0])),
        (LiveId::from_str("position"), InputValue::Vec2([320.0, 300.0])),
        (LiveId::from_str("font_size"), InputValue::Number(28.0)),
    ]).unwrap();
    assert_eq!(doc.size, [640.0, 360.0]);
    let frame = doc.eval(10.0);
    let ItemKind::Primitive(PrimitiveFrame::Text { text, size, align, valign, .. }) = &frame.items[0].kind else { panic!() };
    assert_eq!((text.as_str(), *size, *align, *valign), ("Hi", 28.0, Align::Center, VAlign::Center));
    assert_eq!(frame.items[0].world.apply([0.0, 0.0]), [320.0, 300.0]);
}

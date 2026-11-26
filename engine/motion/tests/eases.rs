//! Eases as written: every ease prints to source that loads back to the
//! same ease, exactly (the graph editor writes dragged curves this way).

use makepad_motion::*;

fn loaded(ease_source: &str) -> Ease {
    let doc = MotionDoc::load(&format!("use mod.motion.*; Motion{{size: vec2(100, 100) duration: 2.0 layers: [Rect{{size: vec2(10, 10) fill: #fff opacity: tween(0, 1, 0, 1, {ease_source})}}]}}"))
        .unwrap_or_else(|d| panic!("{ease_source}: {d:?}"));
    match &doc.layers[0].opacity {
        Prop::Track(track) => track.keys[1].ease,
        other => panic!("{other:?}"),
    }
}

#[test]
fn eases_print_and_load_back_exactly() {
    let mut all: Vec<Ease> = Ease::ids().iter().filter_map(|id| Ease::from_id(id)).collect();
    all.push(Ease::Spring { k: 170.0, d: 26.0 });
    all.push(Ease::Spring { k: 97.25, d: 9.5 });
    for (x1, y1, x2, y2) in [(0.7, 0.0, 0.3, 1.0), (0.25, 0.1, 0.25, 1.0), (0.34, 1.56, 0.64, 1.0), (0.123456789, -0.5, 0.987654321, 1.75), (1.0 / 3.0, 0.1 + 0.2, 0.5, 1.0)] {
        all.push(Ease::Bezier { x1, y1, x2, y2 });
    }
    for ease in all {
        let source = ease.source();
        assert_eq!(loaded(&source), ease, "{source}");
    }
    assert_eq!(Ease::Bezier { x1: 0.7, y1: 0.0, x2: 0.3, y2: 1.0 }.source(), "bezier(0.7, 0, 0.3, 1)");
    // Wrong arity is an error with the form; x outside 0..1 is clamped with a warning.
    assert!(MotionDoc::load("use mod.motion.*; Motion{size: vec2(100, 100) duration: 2.0 layers: [Rect{size: vec2(10, 10) fill: #fff opacity: tween(0, 1, 0, 1, bezier(0.7, 0, 0.3))}]}").is_err());
    let doc = MotionDoc::load("use mod.motion.*; Motion{size: vec2(100, 100) duration: 2.0 layers: [Rect{size: vec2(10, 10) fill: #fff opacity: tween(0, 1, 0, 1, bezier(1.5, 0, 0.3, 1))}]}").unwrap();
    assert!(doc.warnings.iter().any(|w| w.message.contains("clamped")));
    // In a document the curve shapes the track.
    let doc = MotionDoc::load("use mod.motion.*; Motion{size: vec2(100, 100) duration: 2.0 layers: [Rect{size: vec2(10, 10) fill: #fff opacity: tween(0, 1, 0, 1, bezier(0.25, 0.1, 0.25, 1))}]}").unwrap();
    assert!((doc.eval(0.5).items[0].opacity - 0.802_403_387_7).abs() < 1e-8);
}

#[test]
fn rate_shapers_load_by_id_and_by_call_and_print_back() {
    for id in Ease::shaper_ids() {
        let ease = Ease::from_id(&id).unwrap();
        assert_eq!(loaded(&format!("@{id}")), ease, "{id}");
        assert_eq!(loaded(&ease.source()), ease, "{id}");
    }
    // The call forms with parameters, over any ease, print back exactly.
    for source in [
        "there_and_back(@ease_in_out_cubic)",
        "there_and_back_with_pause(@ease_out, 0.5)",
        "running_start(@linear, -0.8)",
        "wiggle(spring(170, 26), 3)",
        "squish(@ease_in_out, 0.2, 0.9)",
        "not_quite_there(bezier(0.7, 0, 0.3, 1), 0.4)",
        "lingering(@ease_out_quad, 0.6)",
        "exponential_decay(@linear, 0.2)",
        "double_smooth(@ease_in_out_sine)",
    ] {
        let ease = loaded(source);
        assert!(matches!(ease, Ease::Shaped(_)), "{source}");
        assert_eq!(loaded(&ease.source()), ease, "{source}");
    }
    // Defaults match the id form.
    assert_eq!(loaded("wiggle()"), Ease::from_id("wiggle").unwrap());
    assert_eq!(loaded("squish(@ease_out)"), Ease::Out(makepad_tween::named::Curve::Quad).shaped(makepad_tween::named::Shape::Squish).unwrap());
    // A shaper over `@hold` or another shaper is an error, as is a reversed squish.
    for bad in ["wiggle(@hold)", "wiggle(there_and_back())", "squish(@linear, 0.9, 0.2)"] {
        assert!(MotionDoc::load(&format!("use mod.motion.*; Motion{{size: vec2(100, 100) duration: 2.0 layers: [Rect{{size: vec2(10, 10) fill: #fff opacity: tween(0, 1, 0, 1, {bad})}}]}}")).is_err(), "{bad}");
    }
    // In a document: there_and_back returns to the start value.
    let doc = MotionDoc::load("use mod.motion.*; Motion{size: vec2(100, 100) duration: 2.0 layers: [Rect{size: vec2(10, 10) fill: #fff opacity: tween(0, 1, 0, 1, @there_and_back)}]}").unwrap();
    assert!((doc.eval(0.5).items[0].opacity - 1.0).abs() < 1e-9);
    assert!((doc.eval(0.25).items[0].opacity - 0.5).abs() < 1e-9);
}

#[test]
fn lagged_and_then_give_per_child_alphas_from_group_time() {
    let wrap = |body: &str| format!("use mod.motion.*; Motion{{size: vec2(100, 100) duration: 4.0 layers: [{body}]}}");
    // Four dots, each fading in over its own window of the first two seconds.
    let dot = |i: usize| format!("Rect{{size: vec2(10, 10) fill: #fff opacity: fn(t) {{ lagged(t / 2.0, 4, 1.0)[{i}] }}}}");
    let doc = MotionDoc::load(&wrap(&(0..4).map(dot).collect::<Vec<_>>().join(", "))).unwrap();
    let at = |t: f64| doc.eval(t).items.iter().map(|i| (i.opacity * 100.0).round() / 100.0).collect::<Vec<_>>();
    assert_eq!(at(0.0), vec![0.0, 0.0, 0.0, 0.0]);
    assert_eq!(at(0.25), vec![0.5, 0.0, 0.0, 0.0]);
    assert_eq!(at(1.0), vec![1.0, 1.0, 0.0, 0.0]);
    assert_eq!(at(2.0), vec![1.0, 1.0, 1.0, 1.0]);
    // Succession over a list of durations (the second child takes twice the time).
    let step = |i: usize| format!("Rect{{size: vec2(10, 10) fill: #fff opacity: fn(t) {{ then(t / 4.0, [1, 2, 1])[{i}] }}}}");
    let doc = MotionDoc::load(&wrap(&(0..3).map(step).collect::<Vec<_>>().join(", "))).unwrap();
    let at = |t: f64| doc.eval(t).items.iter().map(|i| (i.opacity * 100.0).round() / 100.0).collect::<Vec<_>>();
    assert_eq!(at(1.0), vec![1.0, 0.0, 0.0]);
    assert_eq!(at(2.0), vec![1.0, 0.5, 0.0]);
    assert_eq!(at(4.0), vec![1.0, 1.0, 1.0]);
}

//! Crowds, neon tubes, comic pages and photo freezes: they load, are pure
//! functions of time (the same frame twice), and react as documented.

use makepad_motion::plain::PrimitiveFrame;
use makepad_motion::templates::comic_boxes;
use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}\n{source}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

/// Every leaf item, with its groups' opacity multiplied in.
fn flat(items: &[Item], out: &mut Vec<Item>) {
    fn walk(items: &[Item], opacity: f64, out: &mut Vec<Item>) {
        for item in items {
            if let ItemKind::Group { children, .. } = &item.kind {
                walk(children, opacity * item.opacity, out);
            } else {
                let mut leaf = item.clone();
                leaf.opacity *= opacity;
                out.push(leaf);
            }
        }
    }
    walk(items, 1.0, out);
}

fn items(doc: &MotionDoc, t: f64) -> Vec<Item> {
    let frame = doc.eval(t);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    let mut out = Vec::new();
    flat(&frame.items, &mut out);
    out
}

const CROWD: &str = r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 6.0 bpm: 120 layers: [
    Crowd{count: 200 rows: 4 at: vec2(0, 560) size: vec2(1920, 520) seed: 7 eyes: #ff8a1f eyes2: #b6ff3b
          nod: 1 arms: keys([2.0, 0], [2.6, 1]) sign_up: keys([4.0, 0], [4.4, 1]) sign: "AGREED" signs: 0.1}
]}"#;

#[test]
fn a_crowd_is_seeded_exact_and_reacts() {
    let doc = load(CROWD);
    for t in [0.0, 1.3, 2.9, 4.7] {
        assert_eq!(format!("{:?}", doc.eval(t).items), format!("{:?}", doc.eval(t).items), "the same frame twice at {t}");
    }
    let calm = items(&doc, 1.0);
    let heads = calm.iter().filter(|i| i.path.ends_with(".head")).count();
    assert_eq!(heads, 200, "one head a member");
    assert!(!calm.iter().any(|i| i.path.ends_with(".arm_l")), "arms down before `arms` rises");
    let up = items(&doc, 3.0);
    assert!(up.iter().filter(|i| i.path.ends_with(".arm_l")).count() > 150, "arms up");
    let signs = items(&doc, 5.0).iter().filter(|i| i.path.ends_with(".sign")).count();
    assert!((8..=40).contains(&signs), "about a tenth hold signs: {signs}");
    // Eyes by side: orange on the left, green on the right.
    let eye = |item: &Item| match &item.kind {
        ItemKind::Primitive(PrimitiveFrame::Ellipse { fill: Some(c), .. }) => Some((item.world.apply([0.0, 0.0])[0], *c)),
        _ => None,
    };
    let eyes: Vec<(f64, Color)> = calm.iter().filter(|i| i.path.ends_with(".eye")).filter_map(eye).collect();
    assert!(eyes.iter().filter(|(x, _)| *x < 700.0).all(|(_, c)| c[0] > 0.9 && c[1] < 0.7));
    assert!(eyes.iter().filter(|(x, _)| *x > 1200.0).all(|(_, c)| c[1] > 0.9));
    // `variants` shares shapes; `start` pops the crowd up; @ring is 3D only.
    let few = load(&CROWD.replace("seed: 7", "seed: 7 variants: 4 start: 1.0"));
    let heads = |t: f64| items(&few, t).iter().filter(|i| i.path.ends_with(".head")).map(|i| match &i.kind {
        ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) | ItemKind::Primitive(PrimitiveFrame::Ellipse { size, .. }) => (size[1] / size[0] * 1e6).round() as i64,
        _ => 0,
    }).collect::<Vec<_>>();
    assert!(heads(0.5).is_empty(), "nobody before `start`");
    let shapes: std::collections::BTreeSet<i64> = heads(3.0).into_iter().collect();
    assert!(shapes.len() <= 4, "four head shapes: {}", shapes.len());
    assert!(MotionDoc::load(&CROWD.replace("seed: 7", "seed: 7 shape: @ring")).is_err());
    // Members differ: not every head is the same size.
    let sizes: std::collections::BTreeSet<i64> = calm.iter().filter(|i| i.path.ends_with(".head")).map(|i| match &i.kind {
        ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) | ItemKind::Primitive(PrimitiveFrame::Ellipse { size, .. }) => (size[0] * 10.0) as i64,
        _ => 0,
    }).collect();
    assert!(sizes.len() > 50, "seeded variety: {} head sizes", sizes.len());
}

#[test]
fn neon_letters_flicker_and_the_failing_one_fails() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 6.0 layers: [
        Text{text: "OPEN" size: 120 at: vec2(640, 360) align: @center animate: @neon_fail}]}"#);
    let opacities = |t: f64| -> Vec<f64> {
        let mut out = vec![0.0; 4];
        for item in items(&doc, t) {
            if let ItemKind::Primitive(PrimitiveFrame::Text { units, .. }) = &item.kind {
                for (k, u) in units.iter().enumerate().take(4) {
                    out[k] = f64::max(out[k], u.opacity);
                }
            }
        }
        out
    };
    let mut n_dim = 0;
    let mut steady = true;
    for k in 0..60 {
        let t = 2.0 + k as f64 * 0.05;
        let o = opacities(t);
        assert_eq!(o, opacities(t), "exact at {t}");
        if o[3] < 0.5 {
            n_dim += 1;
        }
        steady &= o[0] > 0.2 && o[1] > 0.2 && o[2] > 0.2;
    }
    assert!(n_dim >= 6, "the last letter keeps failing ({n_dim} of 60 dim)");
    assert!(steady, "the other tubes stay lit");
}

#[test]
fn comic_pages_lay_out_and_load() {
    for (n, size, layout) in [(5, [1080.0, 1920.0], "hero"), (5, [1920.0, 1080.0], "hero"), (4, [1080.0, 1920.0], "grid"), (7, [1920.0, 1080.0], "grid")] {
        let boxes = comic_boxes(n, size, layout, None, 20.0);
        assert_eq!(boxes.len(), n);
        for (i, a) in boxes.iter().enumerate() {
            assert!(a[0] >= 0.0 && a[1] >= 0.0 && a[0] + a[2] <= size[0] + 1e-6 && a[1] + a[3] <= size[1] + 1e-6, "{layout} {i} inside the page");
            for b in &boxes[i + 1..] {
                let apart = a[0] + a[2] <= b[0] || b[0] + b[2] <= a[0] || a[1] + a[3] <= b[1] || b[1] + b[3] <= a[1];
                assert!(apart, "{layout}: panels do not overlap");
            }
        }
    }
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1080, 1920) duration: 4.0 layers: [Comic{layout: @hero panels: [
        {time: 0.2 caption: "hero"} {time: 0.8 caption: "two" burst: "RING RING"} {time: 1.4 burst: "ERROR"}]}]}"#);
    let at = |t: f64| items(&doc, t).iter().filter(|i| i.opacity > 0.0 && matches!(i.kind, ItemKind::Primitive(PrimitiveFrame::Path { .. }))).count();
    assert_eq!(at(0.5), 0, "no burst before its time");
    assert_eq!(at(3.0), 2, "both bursts after");
    let err = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(1080, 1920) duration: 4.0 layers: [Comic{panels: [{time: 0.2 colour: #ff0000}]}]}"#).unwrap_err();
    assert!(err.iter().any(|d| d.path.contains("panels[0].colour")), "{err:?}");
}

#[test]
fn a_freeze_holds_the_picture_under_it_at_its_time() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 layers: [
        Rect{size: vec2(1920, 1080) fill: #1d2a44}
        Ellipse{name: "sun" at: vec2(200, 300) x: tween(0, 1200, 0, 4) size: vec2(400, 400) fill: #ffc23a}
        Freeze{time: 1.0 size: vec2(960, 540) at: vec2(480, 270)}
    ]}"#);
    let sun_in_freeze = |t: f64| -> [f64; 2] {
        let frame = doc.eval(t);
        let ItemKind::Group { children, .. } = &frame.items[2].kind else { panic!() };
        children.iter().find(|i| matches!(i.kind, ItemKind::Primitive(PrimitiveFrame::Ellipse { .. }))).map(|i| i.world.apply([0.0, 0.0])).unwrap()
    };
    // The live sun moves; the frozen one stays where the sun was at 1 s,
    // at half size inside the box.
    assert_eq!(sun_in_freeze(1.5), sun_in_freeze(3.5));
    let expected = [480.0 + (200.0 + 300.0) * 0.5, 270.0 + 300.0 * 0.5];
    let got = sun_in_freeze(2.0);
    assert!((got[0] - expected[0]).abs() < 1e-6 && (got[1] - expected[1]).abs() < 1e-6, "{got:?} vs {expected:?}");
    let polaroid = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 layers: [
        Rect{size: vec2(1920, 1080) fill: #1d2a44}
        Polaroid{time: 1.5 span: [1.5, 2.5] caption: "along the pier"}]}"#);
    for t in [1.5, 1.6, 2.5, 3.9] {
        assert_eq!(format!("{:?}", polaroid.eval(t).items), format!("{:?}", polaroid.eval(t).items));
    }
    assert!(MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 layers: [Polaroid{caption: "x"}]}"#).is_err(), "a Polaroid needs its time");
}

#[test]
fn a_freeze_with_a_moving_time_shows_the_picture_at_each_frames_time() {
    // The freeze runs its source backwards: at t it shows the sun as it was at 4 - t.
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 layers: [
        Ellipse{name: "sun" at: vec2(200, 300) x: tween(0, 1200, 0, 4) size: vec2(400, 400) fill: #ffc23a}
        Freeze{time: fn(t) { 4.0 - t } size: vec2(1920, 1080)}
    ]}"#);
    let sun = |t: f64, root: usize| -> f64 {
        let frame = doc.eval(t);
        let item = match &frame.items[root].kind {
            ItemKind::Group { children, .. } => children.iter().find(|i| matches!(i.kind, ItemKind::Primitive(PrimitiveFrame::Ellipse { .. }))).unwrap().clone(),
            _ => frame.items[root].clone(),
        };
        item.world.apply([0.0, 0.0])[0]
    };
    for t in [0.5, 1.0, 3.0] {
        assert!((sun(t, 1) - sun(4.0 - t, 0)).abs() < 1e-6, "at {t}: frozen {} vs live at {} {}", sun(t, 1), 4.0 - t, sun(4.0 - t, 0));
    }
    // keys move it too
    let keyed = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 layers: [
        Ellipse{at: vec2(200, 300) x: tween(0, 1200, 0, 4) size: vec2(400, 400) fill: #ffc23a}
        Freeze{time: keys([0, 3.0], [2, 1.0]) size: vec2(1920, 1080)}
    ]}"#);
    let ItemKind::Group { children, .. } = &keyed.eval(2.0).items[1].kind else { panic!() };
    assert!((children[0].world.apply([0.0, 0.0])[0] - (200.0 + 300.0)).abs() < 1e-6);
}

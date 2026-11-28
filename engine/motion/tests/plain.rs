use makepad_motion::*;

#[test]
fn plain_doc_samples_without_a_vm() {
    use plain::{Layer, LayerKind, RectProps, EllipseProps};
    let layer = |path: &str, kind, x: Prop<1>| Layer {
        name: None, path: path.into(), loc: Loc::default(), span: None,
        animated: x.is_animated(), transform: Transform { x, ..Transform::default() },
        opacity: Prop::scalar(1.0), blur: Prop::scalar(0.0), feather: Prop::scalar(0.0),
        feather_side: FeatherSide::Both, blend: Blend::Normal,
        kind, visible: true, locked: false, hard_cut: false,
    };
    let key = |t, x| Key { t: Time::seconds(t), v: [x], ease: Ease::Linear, bez: None };
    let x = Track::new(vec![key(0.0, 0.0), key(2.0, 16.0)], None, Extrap::Constant, Extrap::Constant).unwrap();
    let rect = LayerKind::Rect(RectProps {
        size: Prop::finite([8.0, 10.0]).unwrap(), radius: Prop::scalar(0.0),
        fill: Some(Prop::finite([1.0; 4]).unwrap()), stroke: None, width: Prop::scalar(2.0),
        trim_start: Prop::scalar(0.0), trim_end: Prop::scalar(1.0), cap: Cap::Butt,
    });
    let ellipse = LayerKind::Ellipse(EllipseProps {
        size: Prop::finite([6.0, 4.0]).unwrap(), fill: Some(Prop::finite([1.0, 0.0, 0.0, 1.0]).unwrap()),
        stroke: None, width: Prop::scalar(2.0), trim_start: Prop::scalar(0.0), trim_end: Prop::scalar(1.0),
        cap: Cap::Butt,
    });
    let mut plain = PlainDoc {
        size: [64.0, 32.0], duration: 3.0, fps: 60.0, bpm: 120.0, background: None, stops: Vec::new(),
        inputs: Vec::new(), input_values: Vec::new(),
        layers: vec![layer("layers[0]", rect, Prop::Track(x)), layer("layers[1]", ellipse, Prop::scalar(0.0))],
    };
    let loaded = MotionDoc::load("use mod.motion.*; Motion{size: vec2(64, 32) duration: 3 layers: [
        Rect{size: vec2(8, 10) fill: #fff x: keys([0, 0], [2, 16])} Ellipse{size: vec2(6, 4) fill: #f00}
    ]}").unwrap();
    for t in [0.0, 0.5, 2.0, 3.0] {
        let sampled = plain.eval(t);
        assert_eq!(sampled.frame().hash(), loaded.eval(t).hash(), "hand-built layers at {t} s");
        assert!(sampled.frame().errors.is_empty(), "sampling data at {t} s: {:?}", sampled.frame().errors);
    }

    plain.layers[0].transform.x = Prop::Fn(FnProp { index: 0, loc: Loc { line: 4, col: 9 } });
    let frame = plain.eval(1.0).into_frame();
    assert_eq!(frame.items[0].world.e, 0.0, "function fallback");
    assert_eq!(frame.errors.len(), 1, "one diagnostic for the function");
    assert_eq!(frame.errors[0].loc, Loc { line: 4, col: 9 }, "function location");
    assert!(frame.errors[0].message.contains("a property function needs the document's VM; this document was read as data"), "{}", frame.errors[0]);
    let frame = loaded.plain.eval(1.0).into_frame();
    assert!(frame.camera.is_none() && frame.post.is_none() && frame.motion_blur.is_none() && !frame.hdr && frame.data_layers.is_empty());
}

#[test]
fn loaded_documents_sample_the_same_without_their_vm() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut sampled = 0;
    for entry in std::fs::read_dir(fixtures).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("splash") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let Ok(doc) = MotionDoc::load(&source) else { continue };
        // weight_wave.splash has a frame-time property function. The plot
        // and camera fixtures have already lowered their functions into data.
        if doc.has_functions { continue; }
        for i in 0..5 {
            let t = doc.duration * f64::from(i) / 4.0;
            let plain = doc.plain.eval(t);
            let story = doc.eval(t);
            assert_eq!(plain.frame().hash(), story.hash(), "{} at {t} s", path.display());
            assert_eq!(plain.frame().errors, story.errors, "{} diagnostics at {t} s", path.display());
        }
        sampled += 1;
    }
    assert!(sampled > 0, "expected a fixture with data-only properties");
}

#[test]
fn stops_are_read_from_the_root_and_checked() {
    let load = |field: &str| MotionDoc::load(&format!("use mod.motion.*; Motion{{duration: 3 {field} layers: [Rect{{size: vec2(1, 1) fill: #fff}}]}}"));
    assert_eq!(load("stops: [1.0 2.5]").unwrap().stops, [1.0, 2.5], "presentation holds");
    assert_eq!(load("stops: [0 3]").unwrap().stops, [0.0, 3.0], "inclusive endpoints");
    assert!(load("").unwrap().stops.is_empty(), "absent stops");
    assert!(load("stops: []").unwrap().stops.is_empty(), "empty stops");
    for invalid in ["[2 1]", "[1 1]", "[4]", "[-1]", "[1 @two]", "2", "nil", "vec2(1, 2)"] {
        let errors = load(&format!("stops: {invalid}")).expect_err(invalid);
        assert!(errors.iter().any(|d| d.is_error() && d.path == "stops" && d.hint.as_deref() ==
            Some("`stops` lists the seconds where a presentation holds, increasing and within the duration: `stops: [2.0 5.5 9.0]`")),
            "stops: {invalid}: {errors:?}");
    }
}

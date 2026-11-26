//! Camera and inset geometry through the unchanged frame evaluator.
use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|ds| panic!("{}", ds.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")))
}

fn document(layers: &str) -> MotionDoc {
    load(&format!("use mod.motion.*\nMotion{{size: vec2(1920, 1080) duration: 4 layers: [{layers}]}}"))
}

fn named<'a>(doc: &MotionDoc, frame: &'a MotionFrame, name: &str) -> Vec<&'a Item> {
    let paths: Vec<_> = doc.all_layers().into_iter().filter(|l| l.name.as_deref() == Some(name)).map(|l| l.path.as_str()).collect();
    frame.walk().into_iter().filter(|(_, i)| paths.contains(&i.path.as_str())).map(|(_, i)| i).collect()
}

fn point(actual: [f64; 2], expected: [f64; 2]) {
    assert!((actual[0] - expected[0]).hypot(actual[1] - expected[1]) < 1e-6, "{actual:?} != {expected:?}");
}

fn camera_map(doc: &MotionDoc, t: f64) -> Affine {
    let frame = doc.eval(t);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    named(doc, &frame, "point")[0].transform([10.0, 10.0])
}

#[test]
fn camera_centers_zoom_and_clockwise_view_roll() {
    for (roll, expected) in [(0, [980.0, 540.0]), (90, [960.0, 520.0])] {
        let doc = document(&format!(r#"Camera{{center: vec2(1200, 300) zoom: 2 roll: {roll} layers: [
            Rect{{name: "point" at: vec2(1200, 300) size: vec2(10, 10) fill: #fff}}
        ]}}"#));
        let map = camera_map(&doc, 0.0);
        point(map.apply([0.0, 0.0]), [960.0, 540.0]);
        // Positive view roll turns the world counter-clockwise in y-down units:
        // a point ten world units to the right ends twenty pixels above centre.
        point(map.apply([10.0, 0.0]), expected);
        assert_eq!(doc.layers[0].transform.anchor, [0.0, 0.0]);
        assert!(matches!(&doc.layers[0].kind, LayerKind::Group { size: None, .. }));
    }
    let doc = document(r#"Camera{center: vec2(1200, 300) screen: vec2(300, 200) zoom: 2 layers: [
        Rect{name: "point" at: vec2(1200, 300) size: vec2(10, 10) fill: #fff}
    ]}"#);
    point(camera_map(&doc, 0.0).apply([0.0, 0.0]), [300.0, 200.0]);
    point(camera_map(&doc, 0.0).apply([10.0, 0.0]), [320.0, 200.0]);
}

#[test]
fn center_tween_pans_exactly_without_mutating_shared_tracks() {
    let doc = load(r#"use mod.motion.*
        let pan = tween(vec2(1000, 300), vec2(1200, 300), 0, 2, @ease_in_quad)
        Motion{size: vec2(1920, 1080) duration: 4 layers: [
            Camera{center: pan zoom: 2 layers: [Rect{name: "point" at: vec2(1200, 300) size: vec2(10, 10) fill: #fff}]}
            Rect{name: "shared" at: pan size: vec2(10, 10) fill: #fff}
        ]}"#);
    point(camera_map(&doc, 0.0).apply([0.0, 0.0]), [1360.0, 540.0]);
    point(camera_map(&doc, 1.0).apply([0.0, 0.0]), [1260.0, 540.0]);
    point(camera_map(&doc, 2.0).apply([0.0, 0.0]), [960.0, 540.0]);
    let frame = doc.eval(2.0);
    point(named(&doc, &frame, "shared")[0].world.apply([0.0, 0.0]), [1200.0, 300.0]);
    let LayerKind::Group { layers, .. } = &doc.layers[0].kind else { panic!("camera group") };
    let Prop::Track(track) = &layers[0].transform.at else { panic!("negated tween") };
    assert_eq!(track.keys.len(), 2, "numeric tweens retain their exact track");
    assert_eq!(track.keys[0].v, [-1000.0, -300.0]);
    assert_eq!(track.keys[1].v, [-1200.0, -300.0]);
}

#[test]
fn center_and_roll_keys_keep_times_easing_and_independent_values() {
    let doc = load(r#"use mod.motion.*
        let pan = keys([0, vec2(1000, 300)], [2, vec2(1200, 300), @ease_in_quad])
        let turn = keys([0, 0], [2, 90, @linear])
        Motion{duration: 4 layers: [
            Camera{center: pan zoom: 2 roll: turn layers: [Rect{name: "point" at: vec2(1200, 300) size: vec2(10, 10) fill: #fff}]}
            Rect{name: "shared" at: pan rotate: turn size: vec2(10, 10) fill: #fff}
        ]}"#);
    point(camera_map(&doc, 2.0).apply([10.0, 0.0]), [960.0, 520.0]);
    let frame = doc.eval(1.0);
    let shared = named(&doc, &frame, "shared")[0];
    point(shared.world.apply([0.0, 0.0]), [1050.0, 300.0]);
    assert_eq!(shared.local.rotate, 45.0);
    let LayerKind::Group { layers, .. } = &doc.layers[0].kind else { panic!("camera group") };
    let Prop::Track(track) = &layers[0].transform.at else { panic!("negated keys") };
    assert_eq!(track.keys.len(), 2);
    assert_eq!(track.keys[1].t.value, 2.0);
}

#[test]
fn function_center_is_sampled_and_seeking_is_pure() {
    let doc = document(r#"Camera{center: fn(t) { vec2(1200 + 10 * t, 300) } zoom: 2 layers: [
        Rect{name: "point" at: vec2(1200, 300) size: vec2(10, 10) fill: #fff}
    ]}"#);
    point(camera_map(&doc, 0.0).apply([0.0, 0.0]), [960.0, 540.0]);
    point(camera_map(&doc, 1.5).apply([0.0, 0.0]), [930.0, 540.0]);
    point(camera_map(&doc, 4.0).apply([0.0, 0.0]), [880.0, 540.0]);
    let earlier = doc.eval(0.5).hash();
    let _later = doc.eval(3.5);
    assert_eq!(earlier, doc.eval(0.5).hash());
    let LayerKind::Group { layers, .. } = &doc.layers[0].kind else { panic!("camera group") };
    let Prop::Track(track) = &layers[0].transform.at else { panic!("sampled keys") };
    assert_eq!(track.keys.len(), 121);
    let doc = document(r#"Camera{center: vec2(1200, 300) zoom: fn(t) { 1 + t / 2 } roll: fn(t) { 45 * t } layers: [
        Rect{name: "point" at: vec2(1200, 300) size: vec2(10, 10) fill: #fff}
    ]}"#);
    point(camera_map(&doc, 2.0).apply([10.0, 0.0]), [960.0, 520.0]);
}

#[test]
fn camera_keeps_common_fields_and_children_use_their_own_clock() {
    let doc = document(r#"Camera{name: "view" span: [1, 2] opacity: 0.6 center: vec2(1200, 300) zoom: 2 layers: [
        Rect{name: "point" at: vec2(1200, 300) x: tween(0, 10, 0, 1) size: vec2(10, 10) fill: #fff}
    ]}"#);
    assert!(doc.eval(0.5).items.is_empty());
    let frame = doc.eval(1.0);
    assert_eq!(named(&doc, &frame, "view")[0].opacity, 0.6);
    point(camera_map(&doc, 1.0).apply([0.0, 0.0]), [960.0, 540.0]);
    point(camera_map(&doc, 2.0).apply([0.0, 0.0]), [980.0, 540.0]);
}

fn rect_bounds(item: &Item) -> [[f64; 2]; 2] {
    let ItemKind::Rect { size, .. } = &item.kind else { panic!("expected rectangle") };
    let map = item.transform(*size);
    [map.apply([0.0, 0.0]), map.apply(*size)]
}

#[test]
fn zoomed_rect_fills_the_inset_and_has_a_box_mask() {
    let doc = document(r#"
        Rect{name: "source" at: vec2(200, 300) size: vec2(100, 60) fill: #ff9a52}
        Zoomed{name: "detail" of: "source" region: [200, 300, 100, 60]
            at: vec2(1000, 200) size: vec2(400, 240) radius: 12}
    "#);
    let frame = doc.eval(0.0);
    let copies = named(&doc, &frame, "source");
    assert_eq!(copies.len(), 2);
    assert_eq!(rect_bounds(copies[0]), [[200.0, 300.0], [300.0, 360.0]]);
    assert_eq!(rect_bounds(copies[1]), [[1000.0, 200.0], [1400.0, 440.0]]);
    let ItemKind::Group { mask: Some(mask), .. } = &named(&doc, &frame, "zoom clip")[0].kind else { panic!("box mask") };
    assert_eq!(rect_bounds(mask), [[1000.0, 200.0], [1400.0, 440.0]]);
    assert!(matches!(&mask.kind, ItemKind::Rect { radius, .. } if *radius == 12.0));
    assert_eq!(named(&doc, &frame, "zoom border").len(), 1);
}

#[test]
fn zoomed_centres_uniform_scale_in_a_differently_shaped_box() {
    let doc = document(r#"
        Rect{name: "source" at: vec2(200, 300) size: vec2(100, 60) fill: #fff}
        Zoomed{of: "source" region: [200, 300, 100, 60] at: vec2(1000, 200) size: vec2(400, 400)}
    "#);
    let frame = doc.eval(0.0);
    assert_eq!(rect_bounds(named(&doc, &frame, "source")[1]), [[1000.0, 280.0], [1400.0, 520.0]]);
}

#[test]
fn zoomed_accumulates_ancestor_scale_translation_and_sized_pivots_in_either_order() {
    for (size, origin) in [("", [340, 260]), ("size: vec2(200, 100)", [240, 210])] {
        let source = format!(r#"Group{{at: vec2(300, 200) scale: 2 {size} layers: [
            Rect{{name: "source" at: vec2(20, 30) size: vec2(100, 60) fill: #fff}}
        ]}}"#);
        let inset = format!(r#"Zoomed{{of: "source" region: [{}, {}, 200, 120] at: vec2(1000, 200) size: vec2(400, 240)}}"#, origin[0], origin[1]);
        for forward in [false, true] {
            let doc = document(&if forward { format!("{inset}\n{source}") } else { format!("{source}\n{inset}") });
            let frame = doc.eval(0.0);
            let copies = named(&doc, &frame, "source");
            assert_eq!(copies.len(), 2);
            let copy = copies[usize::from(!forward)];
            assert_eq!(rect_bounds(copy), [[1000.0, 200.0], [1400.0, 440.0]], "forward={forward}, {size}");
        }
    }
}

#[test]
fn copies_keep_their_source_ancestors_clock() {
    let doc = document(r#"
        Group{span: [1, 2] at: vec2(100, 100) layers: [
            Rect{name: "source" at: fn(t) { vec2(100 + 20 * t, 200) } size: vec2(100, 50) fill: #fff}
        ]}
        Zoomed{of: "source" region: [200, 300, 100, 50] at: vec2(1000, 200) size: vec2(400, 200)}
    "#);
    assert!(named(&doc, &doc.eval(0.5), "source").is_empty());
    let frame = doc.eval(1.0);
    assert_eq!(rect_bounds(named(&doc, &frame, "source")[1]), [[1000.0, 200.0], [1400.0, 400.0]]);
    let frame = doc.eval(2.0);
    assert_eq!(rect_bounds(named(&doc, &frame, "source")[1]), [[1080.0, 200.0], [1480.0, 400.0]]);
}

#[test]
fn repeated_forward_insets_find_the_original_instead_of_an_earlier_copy() {
    let doc = document(r#"
        Zoomed{of: "source" region_at: fn(t) { vec2(200 + t, 300) } region_size: vec2(100, 60)
            at: vec2(600, 200) size: vec2(200, 120)}
        Zoomed{of: "source" region: [200, 300, 100, 60] at: vec2(1000, 200) size: vec2(400, 240)}
        Rect{name: "source" at: vec2(200, 300) size: vec2(100, 60) fill: #fff}
    "#);
    let frame = doc.eval(0.0);
    let sources = named(&doc, &frame, "source");
    assert_eq!(sources.len(), 3);
    assert_eq!(rect_bounds(sources[0]), [[600.0, 200.0], [800.0, 320.0]]);
    assert_eq!(rect_bounds(sources[1]), [[1000.0, 200.0], [1400.0, 440.0]]);
    assert_eq!(rect_bounds(sources[2]), [[200.0, 300.0], [300.0, 360.0]]);
}

#[test]
fn nested_zoomed_uses_document_coordinates() {
    let doc = document(r#"
        Rect{name: "source" at: vec2(200, 300) size: vec2(100, 60) fill: #fff}
        Group{at: vec2(100, 50) scale: 2 size: vec2(600, 400) layers: [
            Zoomed{of: "source" region: [200, 300, 100, 60] at: vec2(1000, 200) size: vec2(400, 240)}
        ]}
    "#);
    let frame = doc.eval(0.0);
    assert_eq!(rect_bounds(named(&doc, &frame, "source")[1]), [[1000.0, 200.0], [1400.0, 440.0]]);
}

#[test]
fn plot_and_named_data_items_can_be_copied_before_or_after_lowering() {
    for target in ["plot", "curve"] {
        for forward in [false, true] {
            let source = r#"Plot{name: "plot" at: vec2(200, 300) size: vec2(100, 100)
                x: [0, 1] y: [0, 1] grid: @none layers: [Graph{name: "curve" f: fn(x) { x }}]}"#;
            let inset = format!(r#"Zoomed{{of: "{target}" region: [200, 300, 100, 100] at: vec2(1000, 200) size: vec2(400, 400)}}"#);
            let doc = document(&if forward { format!("{inset}\n{source}") } else { format!("{source}\n{inset}") });
            let frame = doc.eval(0.0);
            let curves = named(&doc, &frame, "curve");
            assert_eq!(curves.len(), 2);
            let copy = curves[usize::from(!forward)];
            let ItemKind::Path { path: data, .. } = &copy.kind else { panic!("lowered curve") };
            let contours = geometry::flatten(data, 0.01);
            point(copy.world.apply(contours[0].0[0]), [1000.0, 600.0]);
            point(copy.world.apply(*contours[0].0.last().unwrap()), [1400.0, 200.0]);
        }
    }
}

#[test]
fn a_zoomed_child_sees_its_plots_lowered_parent_and_siblings() {
    let doc = document(r#"Plot{name: "plot" at: vec2(200, 300) size: vec2(100, 100)
        x: [0, 1] y: [0, 1] grid: @none layers: [
        Graph{name: "curve" f: fn(x) { x }}
        Zoomed{of: "curve" region: [200, 300, 100, 100] at: vec2(1000, 200) size: vec2(400, 400)}
    ]}"#);
    let frame = doc.eval(0.0);
    let curves = named(&doc, &frame, "curve");
    assert_eq!(curves.len(), 2);
    point(curves[1].world.apply([0.0, 100.0]), [1000.0, 600.0]);
    point(curves[1].world.apply([100.0, 0.0]), [1400.0, 200.0]);
}

#[test]
fn zoomed_region_tweens_keys_and_functions_map_to_the_same_box() {
    for at in ["tween(vec2(200, 200), vec2(220, 220), 0, 2)", "fn(t) { vec2(200 + 10 * t, 200 + 10 * t) }"] {
        let doc = document(&format!(r#"
            Rect{{name: "source" at: vec2(200, 200) size: vec2(100, 100) fill: #fff}}
            Zoomed{{of: "source" region_at: {at} region_size: keys([0, vec2(100, 100)], [2, vec2(50, 50)])
                at: vec2(600, 150) size: vec2(200, 200)}}
        "#));
        let frame = doc.eval(0.0);
        assert_eq!(rect_bounds(named(&doc, &frame, "source")[1]), [[600.0, 150.0], [800.0, 350.0]]);
        let frame = doc.eval(2.0);
        let copy = named(&doc, &frame, "source")[1];
        point(copy.world.apply([20.0, 20.0]), [600.0, 150.0]);
        point(copy.world.apply([70.0, 70.0]), [800.0, 350.0]);
        let before = doc.eval(1.0).hash();
        let _after = doc.eval(3.0);
        assert_eq!(before, doc.eval(1.0).hash());
    }
}

#[test]
fn connectors_use_near_corners_and_do_not_cross_the_boxes() {
    let doc = document(r#"
        Rect{name: "source" at: vec2(200, 200) size: vec2(100, 100) fill: #fff}
        Zoomed{of: "source" region: [200, 200, 100, 100] at: vec2(600, 150) size: vec2(200, 200)}
    "#);
    let frame = doc.eval(0.0);
    let ItemKind::Path { path: data, .. } = &named(&doc, &frame, "zoom connectors")[0].kind else { panic!("connector path") };
    let lines = geometry::flatten(data, 0.01);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].0, vec![[300.0, 200.0], [600.0, 150.0]]);
    assert_eq!(lines[1].0, vec![[300.0, 300.0], [600.0, 350.0]]);
}

#[test]
fn missing_names_animated_ancestors_and_cycles_have_teaching_errors() {
    let errors = MotionDoc::load(r#"use mod.motion.*; Motion{layers: [
        Rect{name: "box" size: vec2(20, 20)}
        Plot{name: "plot" x: [0, 1] y: [0, 1] grid: @none layers: [Graph{name: "curve" f: fn(x) { x }}]}
        Zoomed{of: "missing" region: [0, 0, 10, 10] size: vec2(100, 100)}
    ]}"#).err().unwrap();
    assert!(errors.iter().any(|d| d.path.ends_with("of") && ["known names", "box", "plot", "curve"].iter().all(|s| d.message.contains(s))), "{errors:?}");
    for source in [
        r#"Group{at: tween(vec2(0, 0), vec2(10, 0), 0, 1) layers: [Rect{name: "target" size: vec2(20, 20)}]}"#,
        r#"Camera{zoom: tween(1, 2, 0, 1) layers: [Rect{name: "target" size: vec2(20, 20)}]}"#,
    ] {
        let errors = MotionDoc::load(&format!(r#"use mod.motion.*; Motion{{duration: 2 layers: [
            {source} Zoomed{{of: "target" region: [0, 0, 10, 10] size: vec2(100, 100)}}
        ]}}"#)).err().unwrap();
        assert!(errors.iter().any(|d| d.message.contains("animated") && d.message.contains("top-level layer")), "{errors:?}");
    }
    let errors = MotionDoc::load(r#"use mod.motion.*; Motion{layers: [
        Group{name: "self" layers: [Zoomed{of: "self" region: [0, 0, 10, 10] size: vec2(100, 100)}]}
    ]}"#).err().unwrap();
    assert!(errors.iter().any(|d| d.message.contains("circular")), "{errors:?}");
}

#[test]
fn comp_camera_record_stays_available_alongside_camera_layers() {
    let doc = load(r#"use mod.motion.*; Motion{duration: 2 camera: Camera{fov: 45 focus: 1000} layers: [
        Camera{layers: [Rect{name: "point" at: vec2(1200, 300) size: vec2(10, 10) fill: #fff}]}
    ]}"#);
    assert!(doc.eval(0.0).camera.is_some());
    point(camera_map(&doc, 0.0).apply([0.0, 0.0]), [1200.0, 300.0]);
}

#[test]
fn fixture_and_camera_authoring_example_have_clear_labels() {
    let skill = include_str!("../guide/SKILL.md");
    let section = skill.split("## 2j. Math explainers:").nth(1).unwrap().split("\n## 3.").next().unwrap();
    let examples: Vec<_> = section.split("```splash\n").skip(1).map(|b| b.split("```").next().unwrap())
        .filter(|s| s.contains("Camera{") && s.contains("Zoomed{")).collect();
    assert_eq!(examples.len(), 1);
    for source in std::iter::once(include_str!("../fixtures/camera.splash")).chain(examples) {
        assert!(source.starts_with("use mod.motion.*\nMotion{"));
        let doc = load(source);
        for t in [0.0, 0.5, 1.5, 2.5, 2.99, 3.0, 3.4, 4.5, 5.9] {
            assert!(doc.eval(t).errors.is_empty());
        }
        let issues = inspect::layout_checks(&doc, &[0.0, 0.5, 1.5, 2.5, 2.99, 3.0, 3.4, 4.5, 5.9], 16.0);
        let collisions: Vec<_> = issues.iter().filter(|d| matches!(d.code, "text.line_through" | "text.overlap" | "text.crowded")).collect();
        assert!(collisions.is_empty(), "{collisions:?}");
    }
}

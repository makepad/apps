//! Plot lowering and time sampling, exercised through ordinary Motion frames.
use makepad_motion::plain::{GroupProps, PathProps, PrimitiveFrame};
use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    let doc = MotionDoc::load(source).unwrap_or_else(|ds| panic!("{}", ds.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")));
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    doc
}

fn document(items: &str) -> MotionDoc {
    load(&format!("use mod.motion.*; Motion{{size: vec2(900, 650) duration: 3 bpm: 120 layers: [Plot{{at: vec2(100, 100) size: vec2(600, 400) x: [-3, 3] y: [-1, 9] layers: [{items}]}}]}}"))
}

fn named<'a>(layers: &'a [Layer], name: &str) -> &'a Layer {
    fn find<'a>(layers: &'a [Layer], name: &str) -> Option<&'a Layer> {
        for layer in layers {
            if layer.name.as_deref() == Some(name) { return Some(layer); }
            if let LayerKind::Group { layers, .. } = &layer.kind {
                if let Some(found) = find(layers, name) { return Some(found); }
            }
        }
        None
    }
    find(layers, name).unwrap_or_else(|| panic!("missing {name}"))
}

fn frame_item<'a>(items: &'a [Item], path: &str) -> &'a Item {
    fn find<'a>(items: &'a [Item], path: &str) -> Option<&'a Item> {
        for item in items {
            if item.path == path { return Some(item); }
            if let ItemKind::Group { children, .. } = &item.kind {
                if let Some(found) = find(children, path) { return Some(found); }
            }
        }
        None
    }
    find(items, path).unwrap_or_else(|| panic!("missing frame item {path}"))
}

fn svg(doc: &MotionDoc, name: &str, t: f64) -> String {
    let frame = doc.eval(t);
    match &frame_item(&frame.items, &named(&doc.layers, name).path).kind {
        ItemKind::Primitive(PrimitiveFrame::Path { path, .. }) => path
            .cmds
            .iter()
            .filter_map(|c| match c {
                makepad_svg::path::PathCmd::MoveTo(x, y) => Some(format!("M{x} {y} ")),
                makepad_svg::path::PathCmd::LineTo(x, y) => Some(format!("L{x} {y} ")),
                makepad_svg::path::PathCmd::BezierTo(a, b, c, d, e, f) => Some(format!("C{a} {b} {c} {d} {e} {f} ")),
                makepad_svg::path::PathCmd::Close => Some("Z ".to_string()),
                _ => None,
            })
            .collect(),
        other => panic!("{name} is not a Path: {other:?}"),
    }
}

fn contours(doc: &MotionDoc, name: &str, t: f64) -> Vec<(Vec<[f64; 2]>, bool)> {
    geometry::flatten(&geometry::parse_path(&svg(doc, name, t)), 0.01)
}

fn data_point(p: [f64; 2]) -> [f64; 2] { [p[0] / 100.0 - 3.0, (400.0 - p[1]) / 40.0 - 1.0] }

fn tangent(doc: &MotionDoc, t: f64) -> ([f64; 2], f64) {
    let lines = contours(doc, "tangent", t);
    let a = data_point(lines[0].0[0]);
    let b = data_point(*lines[0].0.last().unwrap());
    ([(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5], (b[1] - a[1]) / (b[0] - a[0]))
}

#[test]
fn every_item_lowers_and_geometry_is_in_the_plot_box() {
    let doc = document(r#"
        Graph{name: "graph" f: fn(x) { x*x } from: -2 to: 2 color: #ff7a1a width: 4}
        Parametric{name: "parametric" f: fn(s) { vec2(cos(s), 2+sin(s)) } from: 0 to: 6.283185307}
        Implicit{name: "implicit" f: fn(x, y) { x*x + (y-2)*(y-2) - 4 } cells: 100}
        VectorField{name: "field" f: fn(x, y) { vec2(-y, x) } step: 1 scale: 0.4}
        Riemann{name: "rectangles" f: fn(x) { x*x } from: 0 to: 2 n: 8 side: @mid fill: #3fa9f566 stroke: #3fa9f5 width: 1.5}
        Area{name: "area" f: fn(x) { x*x+2 } g: fn(x) { 1 } from: -1 to: 1 fill: #ff7a1a44}
        Tangent{name: "tangent" f: fn(x) { x*x } x: 1 length: 1 color: #ff7a1a dot: true}
        Dot{name: "dot" at: vec2(2, 6) radius: 8 color: #ff7a1a label: "point" label_size: 22}
        Dot{name: "tex dot" at: vec2(-2, 6) tex: "P"}
        Rect{name: "pixel child" at: vec2(5, 7) size: vec2(10, 10) fill: #fff}
    "#);
    let line = contours(&doc, "graph", 0.0);
    assert_eq!(line.len(), 1);
    assert!(line[0].0.iter().any(|p| (p[0] - 400.0).hypot(p[1] - 320.0) < 1.0));
    let rects = contours(&doc, "rectangles", 0.0);
    assert_eq!(rects.len(), 8);
    assert!(rects.iter().all(|(_, closed)| *closed));
    let first_height = rects[0].0.iter().map(|p| data_point(*p)[1]).fold(f64::NEG_INFINITY, f64::max);
    assert!((first_height - 0.125_f64.powi(2)).abs() < 1e-5, "midpoint rectangle height");
    let circle = contours(&doc, "implicit", 0.0);
    assert_eq!(circle.len(), 1);
    assert!(circle[0].1);
    for p in &circle[0].0 {
        let p = data_point(*p);
        assert!((p[0].hypot(p[1] - 2.0) - 2.0).abs() < 0.02);
    }
    assert!((tangent(&doc, 0.0).1 - 2.0).abs() < 1e-4);
    let area = contours(&doc, "area", 0.0);
    assert_eq!(area.len(), 1);
    assert!(area[0].1);
    assert!((area[0].0.iter().map(|p| data_point(*p)[1]).fold(f64::INFINITY, f64::min) - 1.0).abs() < 1e-5, "Area uses g(x), not the default baseline");
    let field = contours(&doc, "field", 0.0);
    let lengths: Vec<_> = field.iter().map(|(p, _)| {
        let a = data_point(p[0]);
        let b = data_point(*p.last().unwrap());
        (b[0] - a[0]).hypot(b[1] - a[1])
    }).collect();
    assert!(lengths.iter().all(|n| *n <= 0.90001));
    assert!(lengths.iter().any(|n| (*n - 0.9).abs() < 1e-5));
    let parametric = contours(&doc, "parametric", 0.0);
    assert_eq!(parametric.len(), 1);
    assert!((parametric[0].0[0][0] - 400.0).hypot(parametric[0].0[0][1] - 280.0) < 1e-4);
    let dot = geometry::bounds(&geometry::parse_path(&svg(&doc, "dot", 0.0))).unwrap();
    assert_eq!(dot, [492.0, 112.0, 508.0, 128.0]);
    let frame = doc.eval(0.0);
    let child = frame_item(&frame.items, &named(&doc.layers, "pixel child").path);
    assert_eq!(child.world.apply([0.0, 0.0]), [105.0, 107.0]);
}

#[test]
fn animated_tangent_samples_beats_ease_and_moves() {
    let doc = document(r#"Tangent{name: "tangent" f: fn(x) { x*x } x: tween(0, 2, beat(0), beat(4), @ease_in_quad) length: 0.4 fps: 30}"#);
    let (a, slope_a) = tangent(&doc, 0.0);
    let (b, slope_b) = tangent(&doc, 2.0);
    assert!(a[0].abs() < 1e-5 && slope_a.abs() < 1e-4);
    assert!((b[0] - 2.0).abs() < 1e-5 && (slope_b - 4.0).abs() < 1e-4);
    let (mid, slope) = tangent(&doc, 1.0);
    assert!((mid[0] - 0.5).abs() < 1e-4 && (slope - 1.0).abs() < 1e-4);
    assert!(matches!(named(&doc.layers, "tangent").kind, LayerKind::Path(PathProps { data: PathShape::Morph { .. }, .. })));
    let before = svg(&doc, "tangent", 0.5);
    let _later = svg(&doc, "tangent", 2.0);
    assert_eq!(before, svg(&doc, "tangent", 0.5), "scrubbing backwards is stateless");
}

#[test]
fn function_time_changes_one_path_and_unused_time_is_static() {
    let doc = document(r#"
        Graph{name: "wave" f: fn(x, t) { sin(x-t) } fps: 12}
        Graph{name: "static" f: fn(x, t) { x*x } from: -2 to: 2}
        Graph{name: "one argument" f: fn(x) { x*x } from: -2 to: 2}
    "#);
    assert_ne!(svg(&doc, "wave", 0.0), svg(&doc, "wave", 1.0));
    if let LayerKind::Path(PathProps { data: PathShape::Morph { track, shapes, .. }, .. }) = &named(&doc.layers, "wave").kind {
        assert_eq!(track.keys.len(), 37);
        assert!(shapes.windows(2).all(|p| p[0].len() == p[1].len()));
    } else { panic!("wave must be one keyed Path"); }
    for name in ["static", "one argument"] {
        assert!(matches!(named(&doc.layers, name).kind, LayerKind::Path(PathProps { data: PathShape::Static(_), .. })));
        assert_eq!(svg(&doc, name, 0.0), svg(&doc, name, 2.0));
        assert!(!named(&doc.layers, name).animated);
    }
}

#[test]
fn dot_vector_keys_functions_and_integer_parameters_animate() {
    let doc = document(r#"
        Dot{name: "key dot" at: keys([0, vec2(0, 1)], [2, vec2(2, 3), @linear]) radius: 8}
        Dot{name: "function dot" at: fn(t) { vec2(t, 1+t) } radius: 8}
        Dot{name: "tween dot" at: tween(vec2(0, 1), vec2(2, 3), 0, 2) radius: 8}
        Riemann{name: "rectangles" f: fn(x) { x+1 } from: 0 to: 2 n: keys([0, 2], [2, 4])}
    "#);
    for name in ["key dot", "function dot", "tween dot"] {
        let p = geometry::bounds(&geometry::parse_path(&svg(&doc, name, 1.0))).unwrap();
        assert!(((p[0] + p[2]) * 0.5 - 400.0).abs() < 0.01);
        assert!(((p[1] + p[3]) * 0.5 - 280.0).abs() < 0.01);
    }
    assert_ne!(svg(&doc, "rectangles", 0.0), svg(&doc, "rectangles", 2.0));
}

#[test]
fn plot_and_item_spans_keep_function_time_on_the_plot_clock() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(900, 650) duration: 6 layers: [
        Plot{span: [2, 3] at: vec2(100, 100) size: vec2(600, 400) x: [-3, 3] y: [-1, 9] layers: [
            Graph{name: "clock" f: fn(x, t) { t } span: [1, 2] fps: 10}
        ]}
    ]}"#);
    // Document 3.5 -> plot 1.5 -> item's clock 0.5, but f sees plot 1.5.
    let lines = contours(&doc, "clock", 3.5);
    assert!((data_point(lines[0].0[0])[1] - 1.5).abs() < 1e-4);
    if let LayerKind::Path(PathProps { data: PathShape::Morph { track, .. }, .. }) = &named(&doc.layers, "clock").kind {
        assert_eq!(track.keys.len(), 31);
    } else { panic!("expected geometry keys"); }
}

#[test]
fn graph_poles_break_and_clip_at_the_box() {
    let doc = load(r#"use mod.motion.*; Motion{duration: 2 layers: [Plot{x: [-3, 3] y: [-4, 4] size: vec2(600, 400) layers: [Graph{name: "poles" f: fn(x) { tan(x) }}]}]}"#);
    let lines = contours(&doc, "poles", 0.0);
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().flat_map(|l| &l.0).all(|p| p[0] >= 0.0 && p[0] <= 600.0 && p[1] >= 0.0 && p[1] <= 400.0));
}

#[test]
fn path_look_common_fields_and_reveal_survive_lowering() {
    let doc = document(r#"Graph{name: "look" f: fn(x) { x*x } from: -2 to: 2
        stroke: #ff0000 width: 5 fill: #11223344 cap: @round join: @bevel
        trim_start: 0.2 trim_end: tween(0.2, 1, 0, 2) opacity: 0.6 span: [0, 3] at: vec2(7, 9)}"#);
    let frame = doc.eval(1.0);
    let item = frame_item(&frame.items, &named(&doc.layers, "look").path);
    assert_eq!(item.world.apply([0.0, 0.0]), [107.0, 109.0]);
    assert!((item.opacity - 0.6).abs() < 1e-6);
    match &item.kind {
        ItemKind::Primitive(PrimitiveFrame::Path { width, trim, fill, stroke, cap, join, .. }) => {
            assert_eq!(*width, 5.0);
            assert!((trim[0] - 0.2).abs() < 1e-6 && (trim[1] - 0.6).abs() < 1e-6);
            assert!(fill.is_some() && stroke.is_some());
            assert_eq!(*cap, Cap::Round); assert_eq!(*join, Join::Bevel);
        }
        _ => panic!("expected Path"),
    }
}

#[test]
fn teaching_errors_name_the_item_and_field() {
    for ty in ["Graph", "Parametric", "Implicit", "VectorField", "Riemann", "Area", "Tangent", "Dot"] {
        let errors = MotionDoc::load(&format!("use mod.motion.*; Motion{{layers: [{ty}{{}}]}}")).err().unwrap();
        assert!(errors.iter().any(|d| d.message.contains(&format!("`{ty}` goes inside a Plot's `layers`")) && d.message.contains("Plot{x:")), "{errors:?}");
    }
    for (bad, field, fix) in [
        ("Plot{x: [3, -3] y: [-1, 9]}", "x", "min < max"),
        ("Plot{x: [-3, 3] y: [-1, 9] layers: [Graph{f: 2}]}", "f", "f: fn(x)"),
        (r#"Plot{x: [-3, 3] y: [-1, 9] layers: [Graph{f: fn(x) { "bad" }}]}"#, "f", "expected a number"),
        ("Plot{x: [-3, 3] y: [-1, 9] layers: [Graph{f: fn(x) { x } to: tween(0, 2, onset(0), 2)}]}", "to", "use seconds"),
    ] {
        let errors = MotionDoc::load(&format!("use mod.motion.*; Motion{{layers: [{bad}]}}")).err().unwrap();
        assert!(errors.iter().any(|d| d.path.ends_with(field) && d.message.contains(fix)), "{errors:?}");
    }
}

#[test]
fn fixture_and_axes_have_no_crossed_or_overlapping_labels() {
    let doc = load(include_str!("../fixtures/plot.splash"));
    let issues = inspect::layout_checks(&doc, &[0.0, 0.7, 1.5, 3.5, 6.5, 7.9], 16.0);
    let collisions: Vec<_> = issues.iter().filter(|d| d.code == "text.line_through" || d.code == "text.overlap").collect();
    assert!(collisions.is_empty(), "{collisions:?}");
    let axes = load(r#"use mod.motion.*; Motion{size: vec2(1000, 700) duration: 2 layers: [
        Plot{x: [-0.01, 0.09, 0.01] y: [-0.1, 0.9, 0.1] grid: @plane at: vec2(140, 100) size: vec2(700, 500) x_label: "x" y_label: "y"}
    ]}"#);
    assert!(inspect::layout_checks(&axes, &[0.0], 16.0).iter().all(|d| d.code != "text.line_through" && d.code != "text.overlap"));
}

#[test]
fn default_box_edge_axes_and_decoration_reveal() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1000, 700) duration: 3 layers: [
        Plot{x: [2, 8] y: [1, 7] reveal: [0.5, 1]}
    ]}"#);
    let frame = doc.eval(1.0);
    assert_eq!(frame.items[0].world.apply([0.0, 0.0]), [100.0, 70.0]);
    assert!(matches!(&doc.layers[0].kind, LayerKind::Group { props: GroupProps { size: Some(Prop::Const(size)), .. }, .. } if *size == [800.0, 560.0]));
    let axes = frame_item(&frame.items, &named(&doc.layers, "axes").path);
    assert!(matches!(&axes.kind, ItemKind::Primitive(PrimitiveFrame::Path { trim, .. }) if (trim[1] - 0.5).abs() < 1e-6));
    let lines = contours(&doc, "axes", 1.0);
    assert!(lines.iter().any(|(p, _)| p.first() == Some(&[0.0, 560.0]) && p.last() == Some(&[800.0, 560.0])));
    assert!(lines.iter().any(|(p, _)| p.first() == Some(&[0.0, 560.0]) && p.last() == Some(&[0.0, 0.0])));
    let early = frame_item(&frame.items, &named(&doc.layers, "x tick 4").path);
    assert_eq!(early.opacity, 0.0);
    let later = doc.eval(2.0);
    assert_eq!(frame_item(&later.items, &named(&doc.layers, "x tick 4").path).opacity, 1.0);
}

#[test]
fn native_samplers_are_available_as_data_to_draw2d() {
    let doc = load(r#"use mod.motion.*;
        let ticks = plot_ticks(-3, 3)
        let circle = implicit_curve(fn(x, y) { x*x+y*y-4 }, -3, 3, -3, 3, 50)
        let curve = sample_curve(fn(x) { x*x }, -2, 2)
        Motion{duration: 1 layers: [
            Draw2D{draw: fn(c, t) { c.stroke(circle[0], 2, #fff) c.stroke(curve[0], 2, #fff) }}
            Text{name: "tick count" number: ticks.len() at: vec2(100, 100)}
        ]}
    "#);
    let frame = doc.eval(0.0);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    assert!(frame.items.iter().any(|item| matches!(&item.kind, ItemKind::Draw2d { picture, .. } if !picture.list.ops.is_empty())));
    assert!(frame.walk().into_iter().any(|(_, item)| matches!(&item.kind, ItemKind::Primitive(PrimitiveFrame::Text { text, .. }) if text == "7")));
}

#[test]
fn authoring_examples_are_complete_documents() {
    let skill = include_str!("../guide/SKILL.md");
    let section = skill.split("## 2j. Math explainers:").nth(1).unwrap().split("\n## 3.").next().unwrap();
    let examples: Vec<_> = section.split("```splash\n").skip(1).map(|b| b.split("```").next().unwrap()).collect();
    assert!(examples.len() >= 8, "{} examples", examples.len());
    for example in examples {
        let doc = load(example);
        assert!(doc.eval(1.0).errors.is_empty());
    }
}

fn map_document(map: &str, items: &str) -> MotionDoc {
    let grid = if map.contains("grid:") { "" } else { "grid: @plane" };
    load(&format!(r#"use mod.motion.*; Motion{{size: vec2(1920, 1080) duration: 4 layers: [
        Plot{{x: [-2, 2, 1] y: [-2, 2, 1] {grid} at: vec2(640, 200) size: vec2(640, 640)
            {map} layers: [{items}]}}
    ]}}"#))
}

fn commands(svg: &str) -> String { svg.chars().filter(|c| c.is_ascii_alphabetic() && *c != 'e' && *c != 'E').collect() }

fn assert_point(actual: [f64; 2], expected: [f64; 2], tolerance: f64) {
    assert!((actual[0] - expected[0]).hypot(actual[1] - expected[1]) <= tolerance, "{actual:?} != {expected:?}");
}

fn map_box(p: [f64; 2]) -> [f64; 2] { [(p[0] + 2.0) * 160.0, (2.0 - p[1]) * 160.0] }
fn map_data(p: [f64; 2]) -> [f64; 2] { [p[0] / 160.0 - 2.0, 2.0 - p[1] / 160.0] }

#[test]
fn matrix_maps_every_data_item_grid_axes_and_dot_before_box_conversion() {
    let items = r#"
        Graph{name: "graph" f: fn(x) { x * x } from: -1 to: 1}
        Parametric{name: "parametric" f: fn(s) { vec2(cos(s), sin(s)) }}
        Implicit{name: "implicit" f: fn(x, y) { x * x + y * y - 1 } cells: 24}
        VectorField{name: "field" f: fn(x, y) { vec2(-y, x) } step: 1}
        Riemann{name: "rectangles" f: fn(x) { x + 1 } from: 0 to: 1 n: 2}
        Area{name: "area" f: fn(x) { x * x } from: -1 to: 1}
        Tangent{name: "tangent" f: fn(x) { x * x } x: 0.5 length: 1 dot: true}
        Dot{name: "dot" at: vec2(0, 1) radius: 8}
        Rect{name: "pixels" at: vec2(5, 7) size: vec2(10, 10) fill: #fff}
    "#;
    let identity = map_document("map: [[1, 1], [0, 1]] map_at: 0", items);
    let mapped = map_document("map: [[1, 1], [0, 1]]", items);
    for name in ["grid", "minor grid", "axes", "graph", "parametric", "implicit", "field", "rectangles", "area", "tangent"] {
        let (a, b) = (contours(&identity, name, 0.0), contours(&mapped, name, 0.0));
        assert_eq!(a.len(), b.len(), "{name}");
        for ((a, ac), (b, bc)) in a.iter().zip(&b) {
            assert_eq!(ac, bc);
            assert_eq!(a.len(), b.len(), "{name}");
            for (&source, &target) in a.iter().zip(b) {
                let [x, y] = map_data(source);
                assert_point(target, map_box([x + y, y]), 0.001);
            }
        }
    }
    let (a, b) = (contours(&identity, "grid", 0.0), contours(&mapped, "grid", 0.0));
    let (line, point) = a.iter().enumerate().find_map(|(i, (line, _))| line.iter().position(|p| *p == map_box([0.0, 1.0])).map(|j| (i, j))).unwrap();
    assert_point(b[line].0[point], map_box([1.0, 1.0]), 1e-6);
    let dot = geometry::bounds(&geometry::parse_path(&svg(&mapped, "dot", 0.0))).unwrap();
    assert_eq!(dot, [472.0, 152.0, 488.0, 168.0]);
    let frame = mapped.eval(0.0);
    assert_eq!(frame_item(&frame.items, &named(&mapped.layers, "pixels").path).world.apply([0.0; 2]), [645.0, 207.0]);
    assert!(frame.walk().iter().all(|(_, item)| !matches!(&item.kind, ItemKind::Primitive(PrimitiveFrame::Text { .. }))), "mapped tick labels default off");
}

#[test]
fn map_tween_uses_plot_fps_and_fixed_svg_commands_for_every_key() {
    let items = r#"Graph{name: "graph" f: fn(x) { x * x } from: -1 to: 1}
        Dot{name: "dot" at: vec2(0, 1)}"#;
    let doc = map_document("map: [[1, 1], [0, 1]] map_at: tween(0, 1, 1, 3, @ease_in_out_cubic) fps: 12", items);
    let identity = map_document("map: [[1, 1], [0, 1]] map_at: 0", items);
    let mapped = map_document("map: [[1, 1], [0, 1]]", items);
    for name in ["grid", "minor grid", "axes", "graph", "dot"] {
        assert_eq!(contours(&doc, name, 1.0), contours(&identity, name, 0.0));
        assert_eq!(contours(&doc, name, 3.0), contours(&mapped, name, 0.0));
        let LayerKind::Path(PathProps { data: PathShape::Morph { track, shapes, template }, .. }) = &named(&doc.layers, name).kind else { panic!("{name} needs SVG keys"); };
        assert_eq!(track.keys.len(), 25);
        assert_eq!(track.keys.first().unwrap().t.value, 1.0);
        assert_eq!(track.keys.last().unwrap().t.value, 3.0);
        assert!(shapes.iter().all(|s| s.len() == shapes[0].len()));
        let _ = template; // the template is private now; equal command sequences across keys are asserted below
        let expected = commands(&svg(&doc, name, 1.0));
        for key in &track.keys { assert_eq!(commands(&svg(&doc, name, key.t.value)), expected); }
    }
    let early = geometry::bounds(&geometry::parse_path(&svg(&doc, "dot", 1.5))).unwrap();
    assert_point([(early[0] + early[2]) * 0.5, (early[1] + early[3]) * 0.5], map_box([0.0625, 1.0]), 1e-6);
    let before = svg(&doc, "grid", 1.5);
    let _later = doc.eval(3.5);
    assert_eq!(before, svg(&doc, "grid", 1.5), "map scrubbing is stateless");
}

#[test]
fn nonlinear_plane_subdivision_bounds_every_mapped_midpoint_error() {
    let doc = map_document("map: fn(z) { cmul(z, z) } map_at: tween(0, 1, 0, 1) fps: 4", "");
    for name in ["grid", "minor grid", "axes"] {
        let (source, mapped) = (contours(&doc, name, 0.0), contours(&doc, name, 1.0));
        assert_eq!(source.len(), mapped.len());
        assert_eq!(commands(&svg(&doc, name, 0.0)), commands(&svg(&doc, name, 1.0)));
        for ((a, _), (b, _)) in source.iter().zip(&mapped) {
            assert_eq!(a.len(), b.len());
            for (source, mapped) in a.windows(2).zip(b.windows(2)) {
                let a = map_data(source[0]);
                let b = map_data(source[1]);
                let [x, y] = plotting::mix(a, b, 0.5);
                let true_midpoint = map_box([x * x - y * y, 2.0 * x * y]);
                assert_point(plotting::mix(mapped[0], mapped[1], 0.5), true_midpoint, 0.5);
                assert!((a[0] - b[0]).hypot(a[1] - b[1]) <= 4.0 / 16.0 + 1e-6);
            }
        }
        assert!(mapped.iter().map(|(line, _)| line.len()).sum::<usize>() > 100);
    }
}

#[test]
fn map_functions_translation_labels_keys_and_local_clocks() {
    let mapped = map_document("map: [[1, 0, 0.5], [0, 1, -0.25]] labels: true map_at: keys([1, 0], [3, 1]) fps: 4", r#"Dot{name: "dot" at: vec2(0, 1)}"#);
    let a = mapped.eval(1.0);
    let b = mapped.eval(3.0);
    for name in ["x tick 1", "y tick 1"] {
        let label = &named(&mapped.layers, name).path;
        let start = frame_item(&a.items, label).world.apply([0.0; 2]);
        let end = frame_item(&b.items, label).world.apply([0.0; 2]);
        assert_point([end[0] - start[0], end[1] - start[1]], [80.0, 40.0], 1e-6);
    }
    let function = map_document("map: fn(p) { vec2(p.x + 0.5, p.y - 0.25) } map_at: fn(t) { t / 4 } fps: 4", r#"Dot{name: "dot" at: vec2(0, 1)}"#);
    assert_eq!(svg(&function, "dot", 4.0), svg(&mapped, "dot", 3.0));
    let local = load(r#"use mod.motion.*; Motion{duration: 6 layers: [
        Plot{x: [-2, 2] y: [-2, 2] size: vec2(640, 640) span: [2, 4] grid: @none
            map: [[1, 1], [0, 1]] map_at: tween(0, 1, 1, 3) fps: 4 layers: [
            Dot{name: "dot" at: vec2(0, 1) span: [1, 3]}
        ]}
    ]}"#);
    let bounds = geometry::bounds(&geometry::parse_path(&svg(&local, "dot", 4.0))).unwrap();
    assert_point([(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5], map_box([0.5, 1.0]), 1e-6);
}

#[test]
fn map_and_item_animations_share_fixed_subdivisions() {
    let doc = map_document("map: fn(z) { cmul(z, z) } map_at: tween(0, 1, 1, 3) fps: 4", r#"
        Graph{name: "graph" f: fn(x, t) { 0.5 * sin(x - t) } to: tween(0, 1, 0, 4)}
        Riemann{name: "rectangles" f: fn(x) { x + 1 } from: 0 to: 1 n: tween(2, 4, 0, 4)}
    "#);
    for name in ["graph", "rectangles"] {
        let expected = commands(&svg(&doc, name, 0.0));
        for i in 1..=16 { assert_eq!(commands(&svg(&doc, name, i as f64 / 4.0)), expected); }
        assert_ne!(svg(&doc, name, 0.0), svg(&doc, name, 4.0));
    }
}

#[test]
fn non_finite_map_results_break_paths_and_keep_key_templates() {
    let doc = map_document("map: fn(z) { cdiv([1, 0], z) } map_at: tween(0, 1, 0, 1) fps: 4 grid: @none", r#"Graph{name: "pole" f: fn(x) { 0 } from: -1 to: 1}"#);
    for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let lines = contours(&doc, "pole", t);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().flat_map(|(line, _)| line).all(|p| plotting::finite(*p)));
        assert_eq!(commands(&svg(&doc, "pole", t)), commands(&svg(&doc, "pole", 0.0)));
    }
}

#[test]
fn complex_natives_preserve_unit_values_and_double_precision() {
    for (expression, expected) in [
        ("cmul([0, 1], [0, 1])", [-1.0, 0.0]),
        ("cexp([0, 3.141592653589793])", [-1.0, 0.0]),
        ("cdiv(cmul([1.25, -0.75], [2.5, 3.25]), [2.5, 3.25])", [1.25, -0.75]),
        ("clog([0, 1])", [0.0, std::f64::consts::FRAC_PI_2]),
        ("cpow([0, 1], 2)", [-1.0, 0.0]),
        ("csqrt([-1, 0])", [0.0, 1.0]),
        ("conj([1.25, -0.75])", [1.25, 0.75]),
        ("[cabs([3, 4]), carg([0, 1])]", [5.0, std::f64::consts::FRAC_PI_2]),
    ] {
        let doc = load(&format!("use mod.motion.*; Motion{{duration: 1.0 layers: [Rect{{name: \"value\" at: {expression} size: vec2(1, 1) fill: #fff}}]}}"));
        let Prop::Const(actual) = &named(&doc.layers, "value").transform.at else { panic!("native result must be constant"); };
        assert_point(*actual, expected, 1e-12);
    }
}

#[test]
fn map_points_subdivides_curves_and_accepts_lists_of_polylines() {
    let doc = load(r#"use mod.motion.*;
        let f = fn(z) { cmul(z, z) }
        let line = [[-4, 1], [4, 1]]
        let curve = map_points(line, f)
        let fine = map_points([line, [[-4, -1], [4, -1]]], f, 0.01)
        let gaps = map_points([[-1, 0], [1, 0]], fn(z) { cdiv([1, 0], z) })
        Motion{duration: 1 layers: [
            Rect{name: "counts" at: [curve[0].len(), fine[0].len()] size: vec2(1, 1) fill: #fff}
            Rect{name: "lines" at: [fine.len(), gaps.len()] size: vec2(1, 1) fill: #fff}
            Draw2D{name: "curve" draw: fn(c, t) { c.stroke(curve[0], 1, #fff) }}
        ]}
    "#);
    let Prop::Const(counts) = &named(&doc.layers, "counts").transform.at else { panic!("constant counts"); };
    assert!(counts[0] > 2.0 && counts[1] > counts[0]);
    assert_eq!(named(&doc.layers, "lines").transform.at, Prop::Const([2.0, 2.0]));
    let frame = doc.eval(0.0);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    let ItemKind::Draw2d { picture, .. } = &frame_item(&frame.items, &named(&doc.layers, "curve").path).kind else { panic!("Draw2D curve"); };
    assert!(picture.list.segs.len() > 2);
    // The endpoints have x=15, while the mapped source midpoint is (-1,0).
    assert!(picture.list.segs.iter().any(|s| (s.a[0] + 1.0).abs() < 1e-6 && s.a[1].abs() < 1e-6));
    for segment in &picture.list.segs {
        let a = [segment.a[0] as f64, segment.a[1] as f64];
        let b = [segment.b[0] as f64, segment.b[1] as f64];
        let x = (a[1] + b[1]) * 0.25;
        assert_point(plotting::mix(a, b, 0.5), [x * x - 1.0, 2.0 * x], 0.35);
    }
}

#[test]
fn map_teaching_errors_name_map_and_expected_point() {
    for (map, expected) in [
        ("fn(p) { 7 }", "expected vec2"),
        ("fn(p) { vec3(1, 2, 3) }", "expected vec2"),
        ("[[1, 2], [3]]", "[[a, b], [c, d]]"),
    ] {
        let errors = MotionDoc::load(&format!("use mod.motion.*; Motion{{layers: [Plot{{x: [-1, 1] y: [-1, 1] map: {map}}}]}}" )).err().unwrap();
        assert!(errors.iter().any(|d| d.path.ends_with("map") && d.message.contains("map") && d.message.contains(expected)), "{errors:?}");
    }
}

#[test]
fn map_fixture_and_authoring_example_have_no_layout_warnings() {
    let fixture = load(include_str!("../fixtures/plot_map.splash"));
    let skill = include_str!("../guide/SKILL.md");
    let example = skill.split("```splash\n").skip(1).map(|b| b.split("```").next().unwrap()).find(|s| s.contains("map: [[1, 1], [0, 1]]")).unwrap();
    for doc in [fixture, load(example)] {
        let issues = inspect::layout_checks(&doc, &[0.0, 1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 5.9], 16.0);
        assert!(issues.is_empty(), "{issues:?}");
    }
}

/// A function that only starts to move late in the plot's clock is still a
/// moving curve (the change is looked for over the whole clock).
#[test]
fn a_function_that_moves_late_is_baked_as_moving() {
    let doc = document(r#"Graph{name: "late" f: fn(x, t) { x * x + max(0, t - 2.5) * 8 } color: #fff}"#);
    assert_ne!(svg(&doc, "late", 0.5), svg(&doc, "late", 2.95));
}

/// A moving curve that leaves the box breaks at the edge: no piece of it runs
/// along the box's top or bottom.
#[test]
fn a_moving_curve_breaks_at_the_box_instead_of_running_along_its_edge() {
    let doc = document(r#"Graph{name: "tall" f: fn(x, t) { 4 + 12 * sin(x - t) } color: #fff}"#);
    for t in [0.0, 1.0, 2.0] {
        for (line, _) in contours(&doc, "tall", t) {
            for w in line.windows(2) {
                let on_edge = |p: [f64; 2]| p[1].abs() < 1e-6 || (p[1] - 400.0).abs() < 1e-6;
                let len = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
                assert!(!(on_edge(w[0]) && on_edge(w[1]) && len > 1e-6 && (w[0][1] - w[1][1]).abs() < 1e-6), "a piece along the edge at t={t}: {w:?}");
            }
        }
    }
}

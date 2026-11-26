//! Dense vector work: Repeat, Vector, Pattern, Chart, icons and morphs
//! between any outlines.

use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

#[test]
fn a_repeat_lays_out_its_copies_each_with_its_index_and_delay() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(800, 600) duration: 2.0 layers: [
        Repeat{count: 6 layout: @grid columns: 3 spacing: vec2(100, 50) delay: 0.1 step: {rotate: 10}
            layer: Rect{size: vec2(10, 10) fill: fn(t, i) { rgb(i / 5.0, 0, 0) } opacity: tween(0, 1, 0, 0.1)}}
    ]}"#);
    let frame = doc.eval(0.25);
    let ItemKind::Group { children, .. } = &frame.items[0].kind else { panic!() };
    assert_eq!(children.len(), 6);
    let paths: std::collections::HashSet<&str> = children.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths.len(), 6, "every copy has its own path");
    // Copy 4 sits in row 1, column 1; copy 5's colour is its index.
    let origin = |i: usize| children[i].world.apply([0.0, 0.0]);
    assert!((origin(4)[0] - 100.0).abs() < 1e-6 && (origin(4)[1] - 50.0).abs() < 1e-6, "{:?}", origin(4));
    let ItemKind::Rect { fill: Some(fill), .. } = &children[5].kind else { panic!() };
    assert!((fill[0] - 1.0).abs() < 1e-6, "{fill:?}");
    // The delay: copy 0 has faded in, copy 5 (0.5 s later) not yet.
    assert!(children[0].opacity > 0.99 && children[5].opacity < 0.01);
    let radial = load(r#"use mod.motion.*; Motion{size: vec2(800, 600) duration: 1.0 layers: [Repeat{count: 4 layout: @radial radius: 100 face: true layer: Rect{size: vec2(4, 4)}}]}"#);
    let ItemKind::Group { children, .. } = &radial.eval(0.5).items[0].kind else { panic!() };
    let p = children[1].world.apply([0.0, 0.0]);
    assert!((p[0] - 100.0).abs() < 1e-6 && p[1].abs() < 1e-6, "a quarter turn round: {p:?}");
}

#[test]
fn a_vector_takes_an_icon_and_its_parts() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(800, 600) duration: 2.0 layers: [
        Vector{icon: "stage-app/audio" draw_on: tween(0, 1, 0, 1) parts: {"x": {rotate: tween(0, 90, 0, 1)}}}
    ]}"#);
    let ItemKind::Vector { draw_on, parts, size, .. } = &doc.eval(0.5).items[0].kind else { panic!() };
    assert_eq!(*draw_on, Some(0.5));
    assert_eq!(parts[0].1.rotate, 45.0);
    assert!(size[0] > 0.0 && size[1] > 0.0, "its own size: {size:?}");
    let diags = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(800, 600) duration: 2.0 layers: [Vector{icon: "no-such-icon-at-all"}]}"#).unwrap_err();
    assert!(diags.iter().any(|d| d.hint.as_deref().is_some_and(|h| h.contains("closest"))), "{diags:?}");
}

#[test]
fn any_two_outlines_morph_and_icons_give_paths() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(800, 600) duration: 2.0 layers: [
        Path{svg: tween("M 0 0 L 100 0 L 100 100 Z", "M 50 50 C 80 0, 100 80, 50 100 C 0 80, 20 0, 50 50 Z M 10 10 L 20 10 L 20 20 Z", 0, 1) fill: #ffffff}
        Path{svg: tween(icon_path("stage-app/audio", 100), icon_path("stage-app/check", 100), 0, 1) fill: #ffffff}
    ]}"#);
    for t in [0.0, 0.5, 1.0] {
        for item in &doc.eval(t).items {
            let ItemKind::Path { path, .. } = &item.kind else { panic!() };
            assert!(matches!(path.cmds.first(), Some(geometry::PathCmd::MoveTo(..))) && path.cmds.len() > 2, "{path:?}");
            assert!(geometry::bounds(path).is_some_and(|b| b.iter().all(|n| n.is_finite())), "{path:?}");
        }
    }
}

#[test]
fn path_frames_share_static_geometry_and_interpolate_svg_operands() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(800, 600) duration: 2.0 layers: [
        Path{svg: "m10 20 h30 v40 z" fill: #ffffff}
        Path{svg: tween(
            "m10 20 4 6 h8 v10 q6 8 12 14 t16 18 a20 30 40 0110 20 z m2 4 l6 8",
            "m20 40 8 12 h16 v20 q12 16 24 28 t32 36 a40 60 80 0120 40 z m4 8 l12 16", 0, 1) stroke: #ffffff}
    ]}"#);
    let first = doc.eval(0.0);
    let mid = doc.eval(0.5);
    let last = doc.eval(1.0);
    let get = |frame: &MotionFrame, index: usize| match &frame.items[index].kind {
        ItemKind::Path { path, .. } => path.clone(),
        _ => panic!(),
    };
    assert!(std::sync::Arc::ptr_eq(&get(&first, 0), &get(&mid, 0)));
    assert!(std::sync::Arc::ptr_eq(&get(&first, 0), &get(&last, 0)));
    assert_eq!(*get(&first, 0), geometry::parse_path("m10 20 h30 v40 z"));
    assert_eq!(*get(&first, 1), geometry::parse_path("m10 20 4 6 h8 v10 q6 8 12 14 t16 18 a20 30 40 0110 20 z m2 4 l6 8"));
    assert_eq!(*get(&mid, 1), geometry::parse_path("m15 30 6 9 h12 v15 q9 12 18 21 t24 27 a30 45 60 0115 30 z m3 6 l9 12"));
    assert_eq!(*get(&last, 1), geometry::parse_path("m20 40 8 12 h16 v20 q12 16 24 28 t32 36 a40 60 80 0120 40 z m4 8 l12 16"));
    assert_eq!(get(&mid, 1), get(&doc.eval(0.5), 1), "evaluation order cannot change a frame");
}

#[test]
fn a_pattern_is_an_inline_shader_and_a_chart_a_group_of_layers() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(800, 600) duration: 3.0 layers: [
        Pattern{kind: @dots color: #ffffff cell: 20 size: vec2(200, 100)}
        Chart{type: @bar data: [["a", 1], ["b", 3], ["c", 2]] size: vec2(600, 400) land: [0.5, 1.0, 1.5] annotate: [[1, "top"]]}
        Chart{type: @donut data: [1, 2, 3] size: vec2(400, 400)}
        Chart{type: @treemap csv: "name,value\nx,5\ny,3\nz,2" size: vec2(400, 300)}
    ]}"#);
    let frame = doc.eval(2.9);
    assert!(matches!(frame.items[0].kind, ItemKind::Shader { .. }));
    let ItemKind::Group { children, .. } = &frame.items[1].kind else { panic!() };
    // Bar "b" has landed at its value: the tallest bar fills the plot.
    let bars: Vec<f64> = children.iter().filter_map(|c| match &c.kind { ItemKind::Rect { size, .. } if size[0] > 50.0 && size[1] > 20.0 => Some(size[1]), _ => None }).collect();
    assert_eq!(bars.len(), 3, "{bars:?}");
    assert!(bars[1] > bars[2] && bars[2] > bars[0]);
    let diags = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(800, 600) duration: 1.0 layers: [Chart{type: @barz data: [1]}]}"#).unwrap_err();
    assert!(diags.iter().any(|d| d.message.contains("@bar")), "{diags:?}");
}

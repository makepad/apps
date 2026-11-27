//! `Shape`: children's outlines combined by a boolean operation into one Path.

use makepad_motion::plain::PrimitiveFrame;
use makepad_motion::frame::ItemKind;
use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    let doc = MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    doc
}

/// The outline the Shape made, as contours, and its signed area (y down).
fn contours(doc: &MotionDoc) -> Vec<Vec<[f64; 2]>> {
    let frame = doc.eval(0.5);
    let ItemKind::Primitive(PrimitiveFrame::Path { path: data, .. }) = &frame.items[0].kind else { panic!("not a Path: {:?}", frame.items[0].kind) };
    geometry::flatten(data, 0.1).into_iter().map(|(c, _)| c).collect()
}

fn area(contours: &[Vec<[f64; 2]>]) -> f64 {
    makepad_motion::boolean::area(contours).abs()
}

fn shape(op: &str) -> MotionDoc {
    load(&format!(r#"use mod.motion.*
Motion{{duration: 1.0 layers: [Shape{{op: @{op} fill: #fff layers: [
    Rect{{size: vec2(200, 200)}}
    Rect{{at: vec2(100, 100) size: vec2(200, 200)}}
]}}]}}"#))
}

#[test]
fn the_four_operations_on_two_squares() {
    for (op, expected, count) in [("union", 70000.0, 1), ("intersection", 10000.0, 1), ("difference", 30000.0, 1), ("xor", 60000.0, 2)] {
        let c = contours(&shape(op));
        assert_eq!(c.len(), count, "{op}");
        // Holes and outlines are signed; xor's two L shapes touch at corners.
        let total: f64 = c.iter().map(|c| makepad_motion::boolean::area(std::slice::from_ref(c)).abs()).sum();
        assert!((total - expected).abs() < 1.0, "{op}: {total}");
    }
}

#[test]
fn a_ring_from_two_ellipses_and_letters_cut_from_a_plate() {
    let ring = load(r#"use mod.motion.*
Motion{duration: 1.0 layers: [Shape{op: @difference fill: #ff7a1a layers: [
    Ellipse{size: vec2(400, 400)}
    Ellipse{at: vec2(100, 100) size: vec2(200, 200)}
]}]}"#);
    let c = contours(&ring);
    assert_eq!(c.len(), 2);
    let expected = std::f64::consts::PI * (200.0f64.powi(2) - 100.0f64.powi(2));
    assert!((area(&c) - expected).abs() / expected < 0.01, "{}", area(&c));
    let plate = load(r#"use mod.motion.*
Motion{duration: 1.0 layers: [Shape{op: @difference fill: #fff layers: [
    Rect{size: vec2(600, 200) radius: 20}
    Text{text: "HI" size: 96 at: vec2(300, 40) align: @center}
]}]}"#);
    let c = contours(&plate);
    assert!(c.len() >= 3, "the plate with two letter holes: {}", c.len());
    assert!(area(&c) < 600.0 * 200.0);
}

#[test]
fn placement_and_teaching_errors() {
    let rotated = load(r#"use mod.motion.*
Motion{duration: 1.0 layers: [Shape{op: @union fill: #fff layers: [
    Rect{at: vec2(100, 100) size: vec2(100, 100) rotate: 45}
]}]}"#);
    let c = contours(&rotated);
    let xs: Vec<f64> = c[0].iter().map(|p| p[0]).collect();
    let (lo, hi) = (xs.iter().copied().fold(f64::MAX, f64::min), xs.iter().copied().fold(f64::MIN, f64::max));
    assert!((hi - lo - 100.0 * 2f64.sqrt()).abs() < 0.5, "{lo} {hi}");
    for (source, needle) in [
        (r#"Shape{op: @merge layers: [Rect{size: vec2(10, 10)}]}"#, "@union, @intersection, @difference or @xor"),
        (r#"Shape{layers: [Rect{size: vec2(10, 10) x: tween(0, 10, 0, 1)}]}"#, "animate the Shape itself"),
        (r#"Shape{layers: [Image{size: vec2(10, 10)}]}"#, "Rect, Ellipse, Path and Text"),
        (r#"Shape{layers: []}"#, "combines its `layers`"),
    ] {
        let err = MotionDoc::load(&format!("use mod.motion.*\nMotion{{duration: 1.0 layers: [{source}]}}")).err().unwrap_or_else(|| panic!("{source} loaded"));
        let text = err.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n");
        assert!(text.contains(needle), "{source}: {text}");
    }
}

#[test]
fn path_boolean_for_draw2d() {
    let doc = load(r#"use mod.motion.*
let a = [[0, 0], [100, 0], [100, 100], [0, 100]]
let b = [[50, 50], [150, 50], [150, 150], [50, 150]]
let u = path_boolean(@union, a, b)
let i = path_boolean(@intersection, [a], [b])
Motion{duration: 1.0 layers: [
    Rect{name: "counts" at: vec2(u.len(), i[0].len()) size: vec2(1, 1) fill: #fff}
    Draw2D{draw: fn(c, t) { c.fill(u[0], #fff) }}
]}"#);
    let counts = doc.eval(0.0);
    assert_eq!(counts.items[0].world.apply([0.0, 0.0]), [1.0, 4.0]);
}

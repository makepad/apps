//! Aligned path morphs: a square into a circle into a glyph, two contours
//! into one, and `mix_paths` / `align_paths` on point lists from Splash.

use makepad_motion::plain::PrimitiveFrame;
use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}\n{source}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

fn data_at(doc: &MotionDoc, t: f64) -> String {
    let ItemKind::Primitive(PrimitiveFrame::Path { path, .. }) = &doc.eval(t).items[0].kind else { panic!() };
    icons::path_data(path)
}

#[test]
fn square_into_circle_into_a_glyph_shares_its_commands_and_stays_in_the_hull() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(400, 400) duration: 4.0 layers: [
        Path{fill: #ffffff svg: keys(
            [0.0, "M 0 0 L 100 0 L 100 100 L 0 100 Z"],
            [1.0, "M 50 0 C 77.6 0 100 22.4 100 50 C 100 77.6 77.6 100 50 100 C 22.4 100 0 77.6 0 50 C 0 22.4 22.4 0 50 0 Z"],
            [2.0, text_path("O", Font{family: "Inter"}, 100)]
        )}
    ]}"#);
    let commands = |d: &str| d.split_whitespace().filter(|w| w.chars().all(|c| c.is_ascii_alphabetic())).collect::<String>();
    let reference = commands(&data_at(&doc, 0.0));
    assert!(reference.starts_with("MCCCC"), "{reference}");
    assert!(reference.matches('M').count() == 2, "an O has two contours: {reference}");
    for t in [0.0, 0.3, 0.5, 1.0, 1.4, 2.0, 3.0] {
        let d = data_at(&doc, t);
        assert_eq!(commands(&d), reference, "t {t}");
        assert!(!d.contains("NaN"), "{d}");
    }
}

#[test]
fn two_contours_into_one_morph_through_a_point() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(400, 400) duration: 2.0 layers: [
        Path{fill: #ffffff svg: tween(text_path("O", Font{family: "Inter"}, 100), text_path("I", Font{family: "Inter"}, 100), 0, 1)}
    ]}"#);
    for t in [0.0, 0.5, 1.0] {
        let d = data_at(&doc, t);
        assert_eq!(d.matches('M').count(), 2, "t {t}: {d}");
    }
}

#[test]
fn mix_paths_aligns_point_lists_of_any_shape() {
    let doc = load(r#"use mod.motion.*
use mod.motion.modules.paths.*
Motion{size: vec2(400, 400) duration: 2.0 layers: [
    Rect{size: vec2(10, 10) fill: #fff opacity: fn(t) {
        let sq = poly([pt(0, 0), pt(100, 0), pt(100, 100), pt(0, 100)])
        let tri = poly([pt(10, 10), pt(60, 10), pt(30, 80)])
        let m = mix_paths(sq, tri, 0.5)
        let pair = align_paths(sq, tri)
        let ends = mix_paths(sq, tri, 1.0)
        // Equal point lists after alignment (a quarter), and the mix has as many (a half).
        (if pair[0].len() == pair[1].len() { 0.25 } else { 0.0 }) + (if ends.len() == m.len() && m.len() == pair[0].len() { 0.5 } else { 0.0 })
    }}
]}"#);
    assert_eq!(doc.eval(0.0).items[0].opacity, 0.75);
}

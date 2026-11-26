//! Whether a document's picture can change with time at all
//! (`MotionDoc::is_time_independent`): the fact a deck uses to let a slide
//! whose picture is still stop asking for frames.

use makepad_motion::*;

fn load(layers: &str) -> MotionDoc {
    let source = format!("use mod.motion.*; Motion{{duration: 8 bpm: 120 layers: [{layers}]}}");
    MotionDoc::load(&source).unwrap_or_else(|errors| panic!("{errors:#?}\n{layers}"))
}

#[test]
fn a_document_of_plain_layers_is_still() {
    assert!(load("Rect{size: vec2(100, 50) fill: #ff0000 x: 10 y: 10}, Text{text: \"hi\" size: 40 color: #fff x: 50 y: 50}").is_time_independent());
}

#[test]
fn the_built_in_title_is_still() {
    let doc = MotionDoc::load(builtin::TITLE).unwrap_or_else(|errors| panic!("{errors:#?}"));
    assert!(doc.is_time_independent());
}

#[test]
fn a_keyed_property_moves() {
    assert!(!load("Rect{size: vec2(20, 20) fill: #fff x: keys([0, 0], [2, 100])}").is_time_independent());
}

#[test]
fn a_layer_with_a_span_moves() {
    assert!(!load("Rect{size: vec2(20, 20) fill: #fff span: [1, 3]}").is_time_independent());
}

#[test]
fn a_property_function_moves() {
    assert!(!load("Rect{size: vec2(20, 20) fill: #fff opacity: fn(t) { return 0.5 + sin(t) * 0.5 }}").is_time_independent());
}

#[test]
fn a_moving_child_of_a_still_group_moves() {
    assert!(!load("Group{layers: [Rect{size: vec2(20, 20) fill: #fff x: keys([0, 0], [2, 100])}]}").is_time_independent());
}

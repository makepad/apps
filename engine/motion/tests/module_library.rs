//! The application's module library: a document loaded without a resolver
//! of its own (Stage's checks, its Motion editor) imports from it.

use makepad_motion::plain::PrimitiveFrame;
use makepad_motion::*;
use std::sync::Arc;

#[test]
fn documents_import_from_the_applications_library() {
    let src = "use mod.motion.*; let lib = import(asset(\"ast_widths\", \"r1\"))\nMotion{duration: 1.0 layers: [Rect{size: vec2(lib.w, 4) fill: #ffffff}]}";
    assert!(MotionDoc::load(src).is_err(), "no library yet");
    let mut library = modules::MemoryModules::default();
    library.add_asset("ast_widths", "r1", "{w: 12.0}");
    modules::set_library(Arc::new(library));
    let doc = MotionDoc::load(src).unwrap_or_else(|d| panic!("{d:?}"));
    let ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) = &doc.eval(0.0).items[0].kind else { panic!("not a rect") };
    assert_eq!(size[0], 12.0);
}

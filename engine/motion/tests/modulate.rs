//! `modulate(base, @source, {rate depth phase})` and live lets: a top-level
//! `let` whose value calls modulate (and every later one naming it) is
//! worked out again each frame at the frame's time; drawing code reads
//! plain numbers.

use makepad_motion::plain::PrimitiveFrame;
use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

/// The first layer's width at `t` (a Rect sized from the live let).
fn width(doc: &MotionDoc, t: f64) -> f64 {
    let ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) = &doc.eval(t).items[0].kind else { panic!("not a rect") };
    size[0]
}

#[test]
fn a_modulated_field_moves_with_the_frame_and_what_reads_it_follows() {
    let doc = load(
        "use mod.motion.*\n/// the swing\nlet SWING = {lift: modulate(100.0, @lfo, {rate: 0.25 depth: 40.0}) other: 3.0}
let WIDE = SWING.lift * 2.0
let STILL = {n: 7.0}
Motion{size: vec2(400, 400) duration: 4.0 layers: [Rect{size: fn(t) { vec2(WIDE + STILL.n, 10) } fill: #ffffff}]}",
    );
    // sin(2π · 0.25 · t): 0 at t 0, 1 at t 1, 0 at t 2, -1 at t 3.
    for (t, lift) in [(0.0, 100.0), (1.0, 140.0), (2.0, 100.0), (3.0, 60.0)] {
        let w = width(&doc, t);
        assert!((w - (lift * 2.0 + 7.0)).abs() < 1e-3, "t {t}: {w}");
    }
}

#[test]
fn time_and_noise_sources_and_their_errors() {
    let doc = load("use mod.motion.*\nlet L = modulate(10.0, @time, {rate: 2.0 depth: 5.0})\nMotion{size: vec2(400, 400) duration: 4.0 layers: [Rect{size: fn(t) { vec2(L, 10) } fill: #ffffff}]}");
    assert!((width(&doc, 1.5) - (10.0 + 5.0 * 3.0)).abs() < 1e-3);
    let doc = load("use mod.motion.*\nlet L = modulate(50.0, @noise, {rate: 3.0 depth: 10.0})\nMotion{size: vec2(400, 400) duration: 4.0 layers: [Rect{size: fn(t) { vec2(L, 10) } fill: #ffffff}]}");
    for t in [0.1, 0.7, 2.3] {
        let w = width(&doc, t);
        assert!((40.0..=60.0).contains(&w), "t {t}: {w}");
    }
    let errors = MotionDoc::load("use mod.motion.*\nlet L = modulate(1.0, @wobble, {})\nMotion{size: vec2(400, 400) duration: 1.0 layers: []}").unwrap_err();
    assert!(errors.iter().any(|d| d.message.contains("@lfo, @beat, @word, @noise or @time")), "{errors:?}");
}

#[test]
fn only_live_lets_run_again() {
    // `S` is not live: its side effect happens once, at load, however many frames run.
    let doc = load("use mod.motion.*\nlet LOG = []\nlet side = fn() { LOG.push(1.0) return 2.0 }\nlet S = side()\nlet M = modulate(5.0, @lfo, {rate: 1.0 depth: 1.0})\nMotion{size: vec2(400, 400) duration: 4.0 layers: [Rect{size: fn(t) { vec2(LOG.len() * 100.0 + S, 10) } fill: #ffffff}]}");
    for t in [0.0, 0.5, 1.0] {
        assert_eq!(width(&doc, t), 102.0, "t {t}");
    }
}

/// An editor's patch of a modulation's literals (its base, its depth) reaches
/// the live let that runs each frame, with no reload (SC1's literal map
/// treats the `motion.splash#live` copy as the document at the same place).
#[test]
fn modulated_literals_patch_live() {
    use makepad_motion::schema::{CodeFile, Literal};
    let src = "use mod.motion.*\nlet SWING = {lift: modulate(100.0, @lfo, {rate: 0.25 depth: 40.0}) other: 3.0}\nlet WIDE = SWING.lift * 2.0\nlet STILL = {n: 7.0}\nMotion{size: vec2(400, 400) duration: 4.0 layers: [Rect{size: fn(t) { vec2(WIDE + STILL.n, 10) } fill: #ffffff}]}";
    let doc = makepad_motion::editable(|| MotionDoc::load(src)).unwrap();
    let at = src.find("100.0").unwrap();
    assert!(!doc.patch_literal(&CodeFile::Doc, src, at..at + 5, Literal::Number(120.0)).needs_reload());
    let at = src.find("40.0").unwrap();
    assert!(!doc.patch_literal(&CodeFile::Doc, src, at..at + 4, Literal::Number(30.0)).needs_reload());
    assert!((width(&doc, 1.0) - ((120.0 + 30.0) * 2.0 + 7.0)).abs() < 1e-3);
}

/// A plate a tree gave a path to (an edit's plate, a deck's slide) and the
/// module it imports evaluate under their paths: an editor's patch reaches
/// both without a reload (it once looked for `motion.splash` and
/// `module:<key>` and reached nothing, so every drag reloaded), and the
/// values patched back give the frame they gave before.
#[test]
fn a_tree_loaded_plate_and_its_module_patch_live() {
    use makepad_motion::schema::{CodeFile, Literal};
    let plate = "use mod.motion.*\nlet look = import(\"look.splash\")\nlet W = 30.0\nMotion{size: vec2(400, 400) duration: 2.0 layers: [Rect{size: fn(t) { vec2(W + look.PAD, 10) } fill: #ffffff}]}";
    let look = "use mod.motion.*\n{PAD: 5.0}\n";
    let files = [("plate.splash", plate), ("look.splash", look)].into_iter().map(|(path, text)| (path.to_string(), text.as_bytes().to_vec())).collect();
    let tree = std::sync::Arc::new(makepad_motion::tree::memory_tree(files, "plate.splash", makepad_asset_data::tree::TreeRole::Cut).unwrap());
    let modules = makepad_motion::modules::Modules::from_tree(makepad_motion::tree::TreeContext { resolver: tree, document: "plate.splash".into() });
    let doc = makepad_motion::editable(|| MotionDoc::load_with(plate, &[], MotionContext { modules, ..Default::default() })).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
    let before = doc.eval(1.0);
    assert_eq!(width(&doc, 1.0), 35.0);
    let w = plate.find("30.0").unwrap()..plate.find("30.0").unwrap() + 4;
    let pad = look.find("5.0").unwrap()..look.find("5.0").unwrap() + 3;
    let module = CodeFile::Module("look.splash".to_string());
    for (file, text, at, value) in [(&CodeFile::Doc, plate, &w, 40.0), (&module, look, &pad, 7.0)] {
        let reach = doc.patch_literal(file, text, at.clone(), Literal::Number(value));
        assert!(reach.places > 0 && reach.reload.is_empty(), "{file:?} {value}: {} places, reload {:?}", reach.places, reach.reload);
    }
    assert_eq!(width(&doc, 1.0), 47.0, "both patched");
    doc.patch_literal(&CodeFile::Doc, plate, w, Literal::Number(30.0));
    doc.patch_literal(&module, look, pad, Literal::Number(5.0));
    assert!(doc.eval(1.0) == before, "the values patched back give the same frame");
}

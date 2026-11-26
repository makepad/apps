//! The reference film's timing toolbox in the prelude (LK1): spring_step,
//! window01, frame_idx, the beat grid over analysed beat times; and its look in
//! the `film` module: film_post (the reference's post chain) and detect_box
//! (the ML-detector HUD box).

use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

/// The opacity of a Rect whose `opacity` is `expr` (a function of t in 0..1), at `t`.
fn value_at(expr: &str, t: f64) -> f64 {
    let doc = load(&format!("use mod.motion.*; Motion{{size: vec2(100, 100) duration: 10.0 layers: [Rect{{size: vec2(10, 10) fill: #ffffff opacity: fn(t) {{ {expr} }}}}]}}"));
    doc.eval(t).items[0].opacity as f64
}

#[test]
fn spring_step_overshoots_then_settles_like_the_reference() {
    // 3.4 Hz, damping 0.42: 0 at 0, the first peak (+23 %) at 0.16 s, settled by 0.6 s.
    assert_eq!(value_at("spring_step(t, 3.4, 0.42) * 0.5", 0.0), 0.0);
    let peak = value_at("spring_step(t, 3.4, 0.42) * 0.5", 0.162);
    assert!((peak - 0.617).abs() < 0.002, "{peak}");
    let late = value_at("spring_step(t, 3.4, 0.42) * 0.5", 0.6);
    assert!((late - 0.5).abs() < 0.008, "{late}");
}

#[test]
fn window_and_frame_index() {
    assert!((value_at("window01(t, 1.0, 3.0, 0.5, 0.5)", 1.25) - 0.5).abs() < 1e-6);
    assert!((value_at("window01(t, 1.0, 3.0, 0.5, 0.5)", 2.0) - 1.0).abs() < 1e-6);
    assert!((value_at("window01(t, 1.0, 3.0, 0.5, 0.5)", 2.75) - 0.5).abs() < 1e-6);
    // the frame a time belongs to is constant over its shutter
    assert_eq!(value_at("frame_idx(t) / 100.0", 1.0 + 0.4 / 60.0), 0.6);
    assert_eq!(value_at("frame_idx(t) / 100.0", 1.0 - 0.4 / 60.0), 0.6);
}

#[test]
fn the_beat_grid_round_trips() {
    let b = "[1.0, 1.5, 2.1, 2.6, 3.0]";
    assert!((value_at(&format!("beat_at({b}, t) / 4.0"), 1.8) - 0.375).abs() < 1e-6);
    assert!((value_at(&format!("time_of_beat({b}, beat_at({b}, t)) / 4.0"), 2.35) - 0.5875).abs() < 1e-6);
    // outside the analysed beats: the mean period, extrapolated
    assert!((value_at(&format!("time_of_beat({b}, 5.0) / 4.0"), 0.0) - 0.875).abs() < 1e-6);
    // snapping to the next eighth
    assert!((value_at(&format!("time_of_beat({b}, ceil(beat_at({b}, t) * 2.0) / 2.0) / 4.0"), 1.6) - 0.45).abs() < 1e-6);
}

#[test]
fn film_post_is_the_reference_chain() {
    let doc = load(r#"use mod.motion.*; use mod.motion.modules.film.*
Motion{duration: 2.0 color: @hdr post: film_post({bloom: 0.7 threshold: 0.82})
        layers: [detect_box({geo: fn(t) { vec4(960.0, 540.0, 120.0, 90.0) } label: "EYE  0.97" t0: 0.5 close: 1.5 sub: "t-00:02.45"})]}"#);
    let frame = doc.eval(1.0);
    let (decls, _) = frame.post.clone().expect("post passes");
    // Aberration (1), Bloom with halation (14), Shoulder, Vignette (1 each), Grain (1, after the tone map).
    assert_eq!(decls.len(), 18);
    assert_eq!(decls[17].stage, makepad_render_graph::Stage::Final);
    assert!(decls[..17].iter().all(|d| d.stage == makepad_render_graph::Stage::Hdr));
    // The detection box is drawn (brackets, outline, tag, sub-line) and has popped in by 1.0 s.
    assert!(!frame.items.is_empty());
}

/// film_post's frame moves and flashes are passes of their own, in the
/// reference's order: the frame (zoom, shake) first, the flash after the
/// shoulder, the fade after the vignette.
#[test]
fn film_post_moves_flashes_and_fades_the_frame() {
    let doc = load(r#"use mod.motion.*; use mod.motion.modules.film.*
Motion{duration: 2.0 color: @hdr layers: [Rect{size: vec2(100, 100) fill: #ffffff}]
        post: film_post({shake: fn(t) { vec2(3.0 * t, 0.0) } zoom: 1.1 flash: fn(t) { pulse(t, 0.5, 0.1) } flash_color: #eee9df fade: 0.2})}"#);
    let (decls, _) = doc.eval(1.0).post.clone().expect("post passes");
    assert_eq!(decls.len(), 21);
    // The frame pass reads zoom and shake; the flash pass adds a colour; the fade pass scales by 1 - fade.
    let uses = |d: &makepad_render_graph::PassDecl, n: &str| d.uniforms.iter().any(|u| u.name == n);
    assert!(uses(&decls[0], "shake") && uses(&decls[0], "zoom"));
    let flash = decls.iter().position(|d| uses(d, "flash")).expect("a flash pass");
    let fade = decls.iter().position(|d| uses(d, "fade")).expect("a fade pass");
    assert!(flash < fade && fade < decls.len() - 1);
}

/// A document's own pass beside film_post's list: the list's passes take its
/// place, after the document's pass.
#[test]
fn a_pass_list_sits_beside_a_pass_in_post() {
    let doc = load(r#"use mod.motion.*; use mod.post; use mod.motion.modules.film.*
Motion{duration: 2.0 color: @hdr
        post: [post.Pass{at: @hdr reads: [@color] pixel: fn() -> vec4 { return self.color.sample(self.uv()) * 0.5 }}, film_post({bloom: 0.7})]
        layers: [Rect{size: vec2(10, 10) fill: #ffffff}]}"#);
    let (decls, _) = doc.eval(1.0).post.clone().expect("post passes");
    // the document's pass, then film_post's 18
    assert_eq!(decls.len(), 19);
    assert_eq!(decls[18].stage, makepad_render_graph::Stage::Final);
}

/// A generated post written out as its passes (the structure's Expand):
/// the same chain, and a function the scene wrote copied as written.
#[test]
fn film_post_expands_into_its_passes() {
    let doc = load(r#"use mod.motion.*; use mod.motion.modules.film.*
Motion{duration: 2.0 color: @hdr post: film_post({bloom: 0.7 ca: fn(t) { 1.0 + t }})
        layers: [Rect{size: vec2(10, 10) fill: #ff0000}]}"#);
    let entries = doc.post_expansion(None).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.split('{').next().unwrap().trim_start_matches("mod.post.")).collect();
    assert_eq!(names, ["Aberration", "Bloom", "Shoulder", "Vignette", "Grain"], "{entries:?}");
    assert!(entries[0].contains("fn(t) { 1.0 + t }"), "{}", entries[0]);
    assert!(entries[1].contains("strength: 0.7"), "{}", entries[1]);
    let written = format!("use mod.motion.*; Motion{{duration: 2.0 color: @hdr post: [{}] layers: [Rect{{size: vec2(10, 10) fill: #ff0000}}]}}", entries.join("\n"));
    let again = load(&written);
    let a = doc.eval(1.0).post.unwrap().0;
    let b = again.eval(1.0).post.unwrap().0;
    assert_eq!(a.len(), b.len());
    // A flash is a Pass made inside film_post: not written out.
    let flash = load(r#"use mod.motion.*; use mod.motion.modules.film.*
Motion{duration: 2.0 color: @hdr post: film_post({flash: fn(t) { 0.0 }}) layers: []}"#);
    assert!(flash.post_expansion(None).is_err());
}

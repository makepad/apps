//! Keys as curves: bezier segments with handles (auto, clamped, vector,
//! written), and what a track does before and after its keys.

use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

fn x_at(doc: &MotionDoc, t: f64) -> f64 {
    doc.eval(t).items[0].world.e
}

fn doc(x: &str) -> MotionDoc {
    load(&format!("use mod.motion.*; Motion{{duration: 10.0 layers: [Rect{{size: vec2(10, 10) fill: #ff0000 x: {x}}}]}}"))
}

#[test]
fn a_bezier_segment_passes_through_its_keys_and_eases_between_them() {
    let d = doc("keys([0, 0], [1, 100, @bezier])");
    assert!((x_at(&d, 0.0)).abs() < 1e-6);
    assert!((x_at(&d, 1.0) - 100.0).abs() < 1e-6);
    // Clamped ends are flat: slow out, slow in, symmetric.
    let q = x_at(&d, 0.25);
    assert!(q > 0.0 && q < 25.0, "{q}");
    assert!((x_at(&d, 0.5) - 50.0).abs() < 1e-3);
}

#[test]
fn written_handles_shape_the_segment() {
    let flat = doc("keys([0, 0], [1, 100, {in: [-0.33, 0]}])");
    let steep = doc("keys([0, 0], [1, 100, {in: [-0.33, -60]}])");
    assert!(x_at(&steep, 0.8) < x_at(&flat, 0.8));
    // A handle past the previous key is cut to it (time runs forward).
    let long = doc("keys([0, 0], [1, 100, {in: [-5, 0]}])");
    let v = x_at(&long, 0.5);
    assert!(v.is_finite() && (0.0..=100.0).contains(&v), "{v}");
}

#[test]
fn clamped_never_overshoots_and_auto_may() {
    let clamped = doc("keys([0, 0], [1, 100, @bezier], [1.1, 100, @bezier], [2, 0, @bezier])");
    for i in 0..=40 {
        let v = x_at(&clamped, i as f64 * 0.05);
        assert!(v <= 100.0 + 1e-6, "{v}");
    }
}

#[test]
fn extrapolation_before_and_after_the_keys() {
    let cyc = doc("keys([0, 0], [1, 10], {after: @cycle})");
    assert!((x_at(&cyc, 1.5) - 5.0).abs() < 1e-6);
    let lin = doc("keys([0, 0], [1, 10], {before: @linear after: @linear})");
    assert!((x_at(&lin, 2.0) - 20.0).abs() < 1e-6);
    let constant = doc("keys([1, 0], [2, 10])");
    assert!((x_at(&constant, 0.0)).abs() < 1e-6 && (x_at(&constant, 3.0) - 10.0).abs() < 1e-6);
}

#[test]
fn a_bad_handle_mode_is_a_diagnostic() {
    let d = MotionDoc::load("use mod.motion.*; Motion{duration: 2.0 layers: [Rect{size: vec2(10, 10) x: keys([0, 0], [1, 1, {handle: @wobbly}])}]}").unwrap_err();
    assert!(d.iter().any(|d| d.to_string().contains("@wobbly")));
}

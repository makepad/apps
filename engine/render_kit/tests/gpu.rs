//! The kit's pixel tests on the real GPU: the `makepad-render-kit test`
//! binary in a hidden window, every effect checked against analytic or CPU
//! reference values. Needs a GPU and a window server; run with
//! `cargo test --release -p makepad-stage-render-kit --test gpu`.

use std::process::Command;

#[test]
fn every_effect_passes_its_pixel_test_on_the_gpu() {
    let out = std::env::temp_dir().join("render_kit_gpu_test");
    let result = Command::new(env!("CARGO_BIN_EXE_makepad-render-kit"))
        .args(["test", "--out"])
        .arg(&out)
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .output()
        .expect("run makepad-render-kit");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "pixel tests failed:\n{stdout}\n{stderr}");
    assert!(stdout.contains(" 0 failed"), "{stdout}");
}

#[test]
fn the_demo_strip_renders() {
    let out = std::env::temp_dir().join("render_kit_gpu_demo");
    let result = Command::new(env!("CARGO_BIN_EXE_makepad-render-kit"))
        .args(["demo", "--frames", "4", "--out"])
        .arg(&out)
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .output()
        .expect("run makepad-render-kit");
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(out.join("strip.png").exists());
}

#[test]
fn effect_strip_renders_an_effect_over_time() {
    let out = std::env::temp_dir().join("render_kit_gpu_strip.png");
    let _ = std::fs::remove_file(&out);
    let result = Command::new(env!("CARGO_BIN_EXE_makepad-render-kit"))
        .args(["strip", "--effect", "ripple", "--set", "amplitude=10", "--times", "0,0.5", "--out"])
        .arg(&out)
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .output()
        .expect("run makepad-render-kit");
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(String::from_utf8_lossy(&result.stdout).contains("1024x256 (2 frames)"));
    assert!(out.exists());
    // A misspelt effect is refused with the fix.
    let bad = Command::new(env!("CARGO_BIN_EXE_makepad-render-kit"))
        .args(["strip", "--effect", "glw", "--out"])
        .arg(&out)
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .output()
        .expect("run makepad-render-kit");
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("Did you mean `glow`"));
}

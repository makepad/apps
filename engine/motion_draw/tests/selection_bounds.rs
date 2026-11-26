//! An editor's selection box of a layer is what the layer draws (lane M5):
//! `MotionDraw::item_bounds` of a wrapped, tracked, multi-line Text (and
//! one turned and scaled) holds all of its rendered ink and hugs it, with
//! no more room than the line pitch leaves above and below the glyphs.
//! Drawn on the real GPU through `makepad-motion` in a hidden window:
//!
//! `cargo test --release -p makepad-motion-draw --features scene3d --test selection_bounds`

use std::path::PathBuf;
use std::process::Command;

const SIZE: f64 = 48.0;

fn frame_and_bounds(name: &str, layer: &str) -> (Vec<u8>, u32, u32, [f64; 4]) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("motion_selection_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let doc = dir.join("doc.splash");
    std::fs::write(&doc, format!("use mod.motion.*; Motion{{size: vec2(640, 640) duration: 1.0 background: #000000 layers: [{layer}]}}")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_makepad-motion"))
        .args(["frames", &doc.to_string_lossy(), "--at", "0.5", "--width", "640", "--out", &dir.join("f").to_string_lossy()])
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .env("MAKEPAD_MOTION_BOUNDS", "1")
        .output()
        .expect("makepad-motion runs");
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{text}");
    let bounds = text.lines().filter(|line| line.starts_with("bounds ")).last().unwrap_or_else(|| panic!("no bounds: {text}"));
    let v: Vec<f64> = bounds.split_whitespace().skip(3).map(|n| n.parse().unwrap()).collect();
    let png = std::fs::read_dir(dir.join("f")).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|x| x == "png")).expect("a frame");
    use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
    use makepad_zune_png::makepad_zune_core::options::DecoderOptions;
    let bytes = std::fs::read(&png).unwrap();
    let mut decoder = makepad_zune_png::PngDecoder::new_with_options(ZCursor::new(&bytes), DecoderOptions::default().png_set_strip_to_8bit(true));
    let pixels = decoder.decode_raw().unwrap();
    let (w, h) = decoder.dimensions().unwrap();
    (pixels, w as u32, h as u32, [v[0], v[1], v[2], v[3]])
}

/// The ink's box in pixels (= document units at this width): every pixel
/// brighter than the dark background, and how many rows of ink it has.
fn ink(pixels: &[u8], w: u32, h: u32) -> ([f64; 4], usize) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    let mut row_runs = 0;
    let mut was_ink = false;
    for y in 0..h {
        let mut row_ink = false;
        for x in 0..w {
            let at = ((y * w + x) * 4) as usize;
            if pixels[at].max(pixels[at + 1]).max(pixels[at + 2]) > 64 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
                row_ink = true;
            }
        }
        if row_ink && !was_ink {
            row_runs += 1;
        }
        was_ink = row_ink;
    }
    ([x0 as f64, y0 as f64, x1 as f64, y1 as f64], row_runs)
}

/// `turn`: the room a turned box's axis-aligned bounds add at its corners.
fn check(name: &str, layer: &str, lines: usize, turn: f64) {
    let (pixels, w, h, bounds) = frame_and_bounds(name, layer);
    let (ink, runs) = ink(&pixels, w, h);
    assert!(runs >= lines, "{name}: {runs} lines of ink, want {lines}");
    // All the ink is inside the box (a pixel of antialiasing either side)…
    assert!(ink[0] >= bounds[0] - 1.5 && ink[1] >= bounds[1] - 1.5 && ink[2] <= bounds[2] + 1.5 && ink[3] <= bounds[3] + 1.5, "{name}: ink {ink:?} outside the selection {bounds:?}");
    // …and the box hugs it: side bearings and the line pitch's room only.
    let slack = [ink[0] - bounds[0], ink[1] - bounds[1], bounds[2] - ink[2], bounds[3] - ink[3]];
    assert!(slack[0] <= 0.2 * SIZE + turn && slack[2] <= 0.5 * SIZE + turn, "{name}: loose sides {slack:?} (ink {ink:?}, selection {bounds:?})");
    assert!(slack[1] <= 0.5 * SIZE + turn && slack[3] <= 0.6 * SIZE + turn, "{name}: loose top or bottom {slack:?} (ink {ink:?}, selection {bounds:?})");
}

#[test]
fn a_wrapped_tracked_multi_line_text_is_selected_where_it_draws() {
    let text = "Text{text: \"A MARKER ON EVERY BEAT\" font: Font{family: \"Inter\" weight: 800} size: 48 tracking: 6 wrap: 420 line_height: 1.05 color: #ffffff at: vec2(60, 40)}";
    check("wrapped", text, 3, 0.0);
    let lines = "Text{text: \"A MARKER\\nON EVERY\\nBEAT\" size: 48 tracking: 4 color: #ffffff at: vec2(320, 40) align: @center}";
    check("lines", lines, 3, 0.0);
    let turned = "Text{text: \"TURNED\\nAND SCALED\" size: 48 tracking: 2 color: #ffffff at: vec2(120, 90) scale: 1.2 rotate: 8}";
    // Turned 8°: its box's corners stand out by about sin 8° of its size.
    check("turned", turned, 2, 30.0);
}

/// A click selects the layer drawn under it, and naming the layers costs nothing. Six shapes
/// batch into one vector draw (two of them overlap; boolean shapes, so the vector path, not the
/// plain rect and ellipse quads) that the Text after them flushes; once it named every shape as
/// the Text. Drawn with pick ids and without:
/// - the same draw calls and the same colour frame (the shapes stay one draw);
/// - each place names the item drawn on top there, by its path; the ring's hole names nothing;
/// - the frame drawn after the pick is the frame before it.
#[test]
fn a_click_selects_the_layer_drawn_there() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("motion_pick_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let doc = dir.join("doc.splash");
    std::fs::write(&doc, r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 background: #000000 layers: [
        Shape{name: "ring" op: @difference at: vec2(20, 60) fill: #ff7a1a layers: [Ellipse{size: vec2(240, 240)} Ellipse{at: vec2(60, 60) size: vec2(120, 120)}]}
        Shape{op: @union at: vec2(300, 20) fill: #e04040 layers: [Rect{size: vec2(100, 100)}]}
        Shape{op: @union at: vec2(350, 70) fill: #4060e0 layers: [Rect{size: vec2(100, 100)}]}
        Shape{op: @union at: vec2(500, 20) fill: #40c060 layers: [Ellipse{size: vec2(100, 100)}]}
        Shape{op: @union at: vec2(480, 150) fill: #e0c040 layers: [Rect{size: vec2(140, 60)}]}
        Shape{op: @union at: vec2(300, 200) fill: #c040c0 layers: [Ellipse{size: vec2(100, 100)}]}
        Text{text: "HHHHH" size: 96 font: Font{family: "Inter" weight: 900} color: #ffffff at: vec2(430, 240)}
    ]}"#).unwrap();
    // Each place and the paths it names (a pick reads ids within 2 pixels of it).
    let places: [(&str, &[&str]); 9] = [
        ("50,180", &["layers[0]"]),
        ("140,180", &[]),
        ("320,40", &["layers[1]"]),
        ("430,150", &["layers[2]"]),
        ("375,95", &["layers[2]"]),
        ("550,70", &["layers[3]"]),
        ("550,180", &["layers[4]"]),
        ("350,250", &["layers[5]"]),
        ("550,300", &["layers[6]"]),
    ];
    let run = |name: &str, picks: &[&str]| -> String {
        let mut args = vec!["frames".to_string(), doc.to_string_lossy().to_string(), "--at".into(), "0.5".into(), "--width".into(), "160".into(), "--out".into(), dir.join(name).to_string_lossy().to_string()];
        for p in picks {
            args.extend(["--pick".to_string(), p.to_string()]);
        }
        let output = Command::new(env!("CARGO_BIN_EXE_makepad-motion")).args(&args).env("MAKEPAD_HIDE_WINDOWS", "1").output().expect("makepad-motion runs");
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{text}");
        text
    };
    let off = run("off", &[]);
    let on = run("on", &places.map(|(at, _)| at));
    let line = |text: &str, prefix: &str| -> String { text.lines().find(|l| l.starts_with(prefix)).unwrap_or_else(|| panic!("no `{prefix}` line:\n{text}")).to_string() };
    let word = |text: &str, prefix: &str, n: usize| -> String { line(text, prefix).split_whitespace().nth(n).unwrap_or_default().to_string() };
    assert_eq!(line(&on, "draw calls "), line(&off, "draw calls "), "pick ids keep the shapes in one draw: as many draw calls as without them\n{on}");
    assert_eq!(word(&on, "frame ", 2), word(&off, "frame ", 2), "pick ids leave the colour frame as it is without them\n{on}");
    assert_eq!(word(&on, "after the pick: frame ", 5), word(&on, "frame ", 2), "the frame drawn after the pick is the frame before it\n{on}");
    // Each place's report: its `pick at` line, then a `pick id N: <path> op ...` line per id.
    let lines: Vec<&str> = on.lines().collect();
    for (at, want) in places {
        let from = lines.iter().position(|l| l.starts_with(&format!("pick at {at} "))).unwrap_or_else(|| panic!("no pick at {at}:\n{on}"));
        let named: Vec<&str> = lines[from + 1..].iter().take_while(|l| l.starts_with("pick id ")).filter_map(|l| l.split_once(": ").and_then(|(_, rest)| rest.split(" op ").next())).collect();
        assert_eq!(named, want, "the pick at {at} names the item drawn on top there\n{on}");
    }
}

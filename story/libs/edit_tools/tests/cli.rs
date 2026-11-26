//! The `makepad-edit-tools` commands end to end on a generated fixture: a
//! PNG still, a WAV tone, titles and a transition. `inspect`, `timeline` and
//! `audio analyze` run here; the picture commands draw on the GPU in a
//! hidden window and need a GPU and a window server.

use makepad_asset_data::AssetId;
use makepad_story_edit_tools::image::{encode_png, Picture};
use makepad_strict_json::{parse, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_makepad-edit-tools")
}

fn id(seed: u8) -> String {
    AssetId::from_bytes([seed; 16]).to_string()
}

fn rev(seed: u8) -> String {
    format!("arev_{}", format!("{seed:02x}").repeat(32))
}

/// A fixture folder: `still.png` (a gradient with a ring), `tone.wav` (1 s
/// of 440 Hz at -12 dBFS), `media.txt`, a clean edit and a broken one.
fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edit_tools_cli_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut picture = Picture::new(320, 180, [0, 0, 0, 255]);
    for y in 0..180u32 {
        for x in 0..320u32 {
            let d = ((x as f64 - 160.0).powi(2) + (y as f64 - 90.0).powi(2)).sqrt();
            let i = ((y * 320 + x) * 4) as usize;
            let ring = (d - 60.0).abs() < 8.0;
            picture.rgba[i..i + 4].copy_from_slice(&if ring { [250, 250, 250, 255] } else { [(x * 255 / 320) as u8, (y * 255 / 180) as u8, 160, 255] });
        }
    }
    std::fs::write(dir.join("still.png"), encode_png(&picture).unwrap()).unwrap();
    let frames: Vec<i16> = (0..48_000).flat_map(|i| {
        let v = ((i as f64 / 48_000.0 * 440.0 * std::f64::consts::TAU).sin() * 0.25 * 32767.0) as i16;
        [v, v]
    }).collect();
    write_wav(&dir.join("tone.wav"), &frames);
    std::fs::write(dir.join("media.txt"), format!("{}=still.png\n{}=tone.wav\n", id(0x11), id(0x22))).unwrap();
    let clean = format!(
        "use mod.flow.*\nuse mod.cut.*\n\n\
         let card  = Still{{src: asset(\"{s}\", \"{sr}\") span: [0.0, 2.0] fit: @fit}}\n\
         let card2 = Still{{src: asset(\"{s}\", \"{sr}\") span: [1.0, 1.0] fit: @fill track: 1}}\n\
         let fade  = Transition{{a: card.out(@frame) b: card2.out(@frame) effect: \"\" span: [1.0, 0.5]}}\n\
         let title = Title{{text: \"Edit tools\" span: [0.5, 1.5] anchor: vec2(0.5, 0.5) font_size: 28 track: 2}}\n\
         let top   = Stack{{layers: [fade.out(@frame), title.out(@frame)]}}\n\
         let tone  = AudioClip{{src: asset(\"{a}\", \"{ar}\") span: [0.0, 1.0]}}\n\
         let out   = CutOutput{{frame: top.out(@frame) audio: [conn(1, tone.out(@audio))]}}\n\n\
         Flow{{\n    kind: @cut\n    label: \"tools fixture\"\n    output: {{width: 320 height: 180 fps: 30}}\n    duration: 2.0\n    bpm: 120\n    card, card2, fade, title, top, tone, out\n}}\n",
        s = id(0x11), sr = rev(0x11), a = id(0x22), ar = rev(0x22)
    );
    std::fs::write(dir.join("clean.splash"), clean).unwrap();
    // A missing image, an unknown effect and a title off the frame.
    let broken = format!(
        "use mod.flow.*\nuse mod.cut.*\n\n\
         let card  = Still{{src: asset(\"{m}\", \"{mr}\") span: [0.0, 2.0]}}\n\
         let glow  = Fx{{input: card.out(@frame) effect: \"vjfx/does_not_exist\" span: [0.0, 2.0]}}\n\
         let title = Title{{text: \"Off\" span: [0.0, 2.0] anchor: vec2(0.5, 1.3) track: 1}}\n\
         let top   = Stack{{layers: [glow.out(@frame), title.out(@frame)]}}\n\
         let out   = CutOutput{{frame: top.out(@frame)}}\n\n\
         Flow{{\n    kind: @cut\n    label: \"broken\"\n    output: {{width: 320 height: 180 fps: 30}}\n    duration: 2.0\n    card, glow, title, top, out\n}}\n",
        m = id(0x33), mr = rev(0x33)
    );
    std::fs::write(dir.join("broken.splash"), broken).unwrap();
    dir
}

fn write_wav(path: &Path, interleaved: &[i16]) {
    let data: Vec<u8> = interleaved.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut out = Vec::new();
    out.extend(b"RIFF");
    out.extend((36 + data.len() as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(48_000u32.to_le_bytes());
    out.extend((48_000u32 * 4).to_le_bytes());
    out.extend(4u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    std::fs::write(path, out).unwrap();
}

/// Run the tool; (exit code, stdout parsed as JSON).
fn run_json(dir: &Path, args: &[&str]) -> (i32, Value) {
    let output = Command::new(bin()).current_dir(dir).args(args).arg("--json").output().unwrap();
    // stdout is the report and nothing else: log lines go to stderr.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value = parse(stdout.trim().as_bytes()).unwrap_or_else(|e| panic!("{args:?}: not JSON ({e}): {stdout}\nstderr: {}", String::from_utf8_lossy(&output.stderr)));
    (output.status.code().unwrap_or(-1), value)
}

fn codes(report: &Value) -> Vec<String> {
    report.get("issues").and_then(Value::as_arr).unwrap_or(&[]).iter().filter_map(|i| i.get("code")?.as_str().map(str::to_string)).collect()
}

#[test]
fn inspect_passes_a_clean_edit_and_fails_a_broken_one() {
    let dir = fixture("inspect");
    let (code, report) = run_json(&dir, &["inspect", "clean.splash"]);
    assert_eq!(code, 0, "{}", report.to_json());
    assert_eq!(report.get("errors").and_then(Value::as_i64), Some(0), "{}", report.to_json());
    assert_eq!(report.get("warnings").and_then(Value::as_i64), Some(0), "{}", report.to_json());
    let (code, report) = run_json(&dir, &["inspect", "broken.splash"]);
    assert_eq!(code, 1);
    let found = codes(&report);
    for want in ["media.missing", "effect.unknown", "layer.outside_frame"] {
        assert!(found.iter().any(|c| c == want), "{want} missing: {}", report.to_json());
    }
    // Human output: place, code and fix.
    let output = Command::new(bin()).current_dir(&dir).args(["inspect", "broken.splash"]).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("broken.splash:4:") && text.contains("error[media.missing]") && text.contains("fix:"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timeline_and_audio_analyze_report_the_fixture() {
    let dir = fixture("timeline");
    let (code, timeline) = run_json(&dir, &["timeline", "clean.splash"]);
    assert_eq!(code, 0);
    assert_eq!(timeline.get("frames").and_then(Value::as_i64), Some(60));
    let blocks = timeline.get("blocks").and_then(Value::as_arr).unwrap();
    assert_eq!(blocks.len(), 5, "{}", timeline.to_json());
    let (code, audio) = run_json(&dir, &["audio", "analyze", "clean.splash"]);
    assert_eq!(code, 0, "{}", audio.to_json());
    let segments = audio.get("segments").and_then(Value::as_arr).unwrap();
    assert_eq!(segments.len(), 2, "{}", audio.to_json());
    // 440 Hz at -12 dBFS peak, stereo: about -12.6 LUFS.
    let lufs = match segments[0].get("lufs") {
        Some(Value::F64(v)) => *v,
        other => panic!("{other:?}"),
    };
    assert!((lufs - -12.6).abs() < 1.0, "{lufs}");
    let silence = audio.get("silence").and_then(Value::as_arr).unwrap();
    assert_eq!(silence.len(), 1, "{}", audio.to_json());
    // Usage errors exit 2.
    let output = Command::new(bin()).args(["frame"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}

fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[1..4], b"PNG");
    (u32::from_be_bytes(bytes[16..20].try_into().unwrap()), u32::from_be_bytes(bytes[20..24].try_into().unwrap()))
}

#[test]
fn frame_strip_onion_draft_render_and_effect_strips_on_the_gpu() {
    let dir = fixture("gpu");
    let (code, frame) = run_json(&dir, &["frame", "clean.splash", "--at", "@30", "--out", "f30.png"]);
    assert_eq!(code, 0, "{}", frame.to_json());
    assert_eq!(png_size(&dir.join("f30.png")), (320, 180));
    let frames = frame.get("frames").and_then(Value::as_arr).unwrap();
    assert_eq!(frames[0].get("frame").and_then(Value::as_i64), Some(30));
    // Not black: the still and the title are drawn.
    let decoded = {
        let bytes = std::fs::read(dir.join("f30.png")).unwrap();
        let mut decoder = makepad_zune_png::PngDecoder::new(makepad_zune_png::makepad_zune_core::bytestream::ZCursor::new(&bytes[..]));
        decoder.decode_raw().unwrap()
    };
    let lit = decoded.chunks_exact(4).filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 60).count();
    assert!(lit > 320 * 180 / 2, "{lit} lit pixels");

    let (code, strip) = run_json(&dir, &["strip", "clean.splash", "--count", "6", "--width", "160", "--out", "strip.png"]);
    assert_eq!(code, 0, "{}", strip.to_json());
    assert_eq!(strip.get("frames").and_then(Value::as_arr).unwrap().len(), 6);
    let (w, h) = png_size(&dir.join("strip.png"));
    assert!(w > 160 && h > 90, "{w}x{h}");

    let (code, trail) = run_json(&dir, &["onion", "clean.splash", "--from", "0.9", "--to", "1.6", "--steps", "5", "--out", "onion.png"]);
    assert_eq!(code, 0, "{}", trail.to_json());
    assert_eq!(png_size(&dir.join("onion.png")), (320, 180));

    let (code, render) = run_json(&dir, &["render", "clean.splash", "--draft", "--out", "draft.mp4"]);
    assert_eq!(code, 0, "{}", render.to_json());
    assert_eq!(render.get("frames").and_then(Value::as_i64), Some(60));
    assert_eq!(render.get("width").and_then(Value::as_i64), Some(160));
    assert!(std::fs::metadata(dir.join("draft.mp4")).unwrap().len() > 1000);

    // An effect strip: a render-kit effect over the test card.
    let (code, glow) = run_json(&dir, &["effect-strip", "kit/glow", "--param", "radius=16", "--count", "3", "--size", "320x180", "--width", "160", "--out", "glow.png"]);
    assert_eq!(code, 0, "{}", glow.to_json());
    assert!(png_size(&dir.join("glow.png")).0 > 160);
    // A motion document, drawn as a one-Graphic edit.
    std::fs::write(
        dir.join("title.motion.splash"),
        "use mod.motion.*; Motion{\n    size: vec2(320, 180) duration: 1.0 fps: 30 background: #0b0d12\n    layers: [\n        Text{text: \"Hi\" font: Font{family: \"Inter\"} size: 48 at: vec2(160, 60) align: @center opacity: tween(0, 1, 0.0, 0.5, @linear)}\n    ]\n}\n",
    )
    .unwrap();
    let (code, report) = run_json(&dir, &["inspect", "title.motion.splash"]);
    assert_eq!(code, 0, "{}", report.to_json());
    assert_eq!(report.get("kind").and_then(Value::as_str), Some("motion"));
    let (code, strip) = run_json(&dir, &["strip", "title.motion.splash", "--count", "3", "--out", "motion.png"]);
    assert_eq!(code, 0, "{}", strip.to_json());
    let (code, frame) = run_json(&dir, &["frame", "title.motion.splash", "--at", "0.9", "--out", "motion_frame.png"]);
    assert_eq!(code, 0, "{}", frame.to_json());
    assert_eq!(png_size(&dir.join("motion_frame.png")), (320, 180));
    // Exposure, from the frame read back: a card with its left half white
    // on black is half clipped, half near black, mean luma one half.
    std::fs::write(
        dir.join("card.motion.splash"),
        "use mod.motion.*; Motion{\n    size: vec2(320, 180) duration: 1.0 fps: 30 background: #000000\n    layers: [Rect{size: vec2(160, 180) at: vec2(0, 0) fill: #ffffff}]\n}\n",
    )
    .unwrap();
    let (code, card) = run_json(&dir, &["frame", "card.motion.splash", "--at", "0.5", "--width", "80", "--out", "card.png"]);
    assert_eq!(code, 0, "{}", card.to_json());
    let exposure = card.get("exposure").unwrap_or_else(|| panic!("no exposure: {}", card.to_json()));
    let share = |key: &str| match exposure.get(key) {
        Some(Value::F64(v)) => *v,
        Some(Value::Int(v)) => *v as f64,
        _ => panic!("no {key}: {}", card.to_json()),
    };
    assert!((share("clipped") - 0.5).abs() < 0.01, "half the card is clipped: {}", card.to_json());
    assert!((share("near_black") - 0.5).abs() < 0.01, "half the card is near black: {}", card.to_json());
    assert!((share("mean_luma") - 0.5).abs() < 0.01, "the card's mean luma is one half: {}", card.to_json());
    // 3D coverage: per node, its share of the scene's picture and its box.
    // The hero box in the middle is seen; a ball right behind it is hidden
    // by it; a box far to the side is off screen.
    std::fs::write(
        dir.join("scene.motion.splash"),
        "use mod.motion.*; use mod.scene; Motion{size: vec2(320, 180) duration: 1.0 fps: 30 background: #000000 layers: [\n\
         Scene3D{name: \"set\" size: vec2(320, 180) camera: Camera{position: vec3(0, 0, 6) target: vec3(0, 0, 0)} lights: [scene.AmbientLight{}] children: [\n\
         scene.Mesh{name: \"hero\" geometry: scene.BoxGeometry{} scale: vec3(2, 2, 2)}\n\
         scene.Mesh{name: \"behind\" geometry: scene.SphereGeometry{} scale: vec3(0.3, 0.3, 0.3) position: vec3(0, 0, -3)}\n\
         scene.Mesh{name: \"away\" geometry: scene.BoxGeometry{} position: vec3(40, 0, 0)}\n\
         ]}]}\n",
    )
    .unwrap();
    let (code, scene) = run_json(&dir, &["frame", "scene.motion.splash", "--at", "0.5", "--out", "scene.png"]);
    assert_eq!(code, 0, "{}", scene.to_json());
    let covers = scene.get("coverage").and_then(Value::as_arr).unwrap_or_else(|| panic!("no coverage: {}", scene.to_json()));
    assert_eq!(covers.len(), 1, "{}", scene.to_json());
    let cover = &covers[0];
    assert_eq!((cover.get("width"), cover.get("height")), (Some(&Value::Int(320)), Some(&Value::Int(180))), "{}", scene.to_json());
    let node = |name: &str| cover.get("nodes").and_then(Value::as_arr).unwrap().iter().find(|n| n.get("name").and_then(Value::as_str) == Some(name)).unwrap_or_else(|| panic!("no {name}: {}", scene.to_json())).clone();
    let number = |v: &Value| match v {
        Value::F64(v) => *v,
        Value::Int(v) => *v as f64,
        _ => panic!("not a number: {}", v.to_json()),
    };
    let hero = node("hero");
    let share = number(&hero.get("share").and_then(Value::as_arr).unwrap()[0]);
    assert!(share > 0.03 && share < 0.5, "the hero covers part of the picture: {}", scene.to_json());
    let bounds: Vec<f64> = hero.get("box").and_then(Value::as_arr).unwrap_or_else(|| panic!("the hero has a box: {}", scene.to_json())).iter().map(number).collect();
    assert!(bounds[0] < 160.0 && bounds[0] + bounds[2] > 160.0 && bounds[1] < 90.0 && bounds[1] + bounds[3] > 90.0, "the hero's box is round the middle: {}", scene.to_json());
    assert_eq!(hero.get("line"), Some(&Value::Int(3)), "the hero's line in the document: {}", scene.to_json());
    for hidden in ["behind", "away"] {
        let n = node(hidden);
        assert_eq!(n.get("box"), Some(&Value::Null), "{hidden} is not seen: {}", scene.to_json());
    }
    assert!(number(&cover.get("background").and_then(Value::as_arr).unwrap()[0]) > 0.5, "{}", scene.to_json());
    // A strip names its worst frames; the title fades in over a dark ground.
    let exposure = strip.get("exposure").unwrap_or_else(|| panic!("no exposure: {}", strip.to_json()));
    assert!(exposure.get("most_clipped").and_then(|w| w.get("frame")).is_some(), "{}", strip.to_json());
    assert!(strip.get("frames").and_then(Value::as_arr).unwrap().iter().all(|f| f.get("exposure").is_some()), "{}", strip.to_json());

    let (code, bad) = run_json(&dir, &["effect-strip", "kit/glw", "--out", "bad.png"]);
    assert_eq!(code, 1);
    assert!(bad.get("error").and_then(Value::as_str).unwrap().contains("glow"), "{}", bad.to_json());
    let _ = std::fs::remove_dir_all(&dir);
}

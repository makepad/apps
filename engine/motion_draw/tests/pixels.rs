//! The fixtures drawn on the real GPU, through `makepad-motion` in a hidden
//! window: two runs give byte-identical frames, the frames move (no two
//! times look alike where the document animates), and the pictures hold
//! what the documents say (colours where shapes are). Needs a GPU and a window server:
//!
//! `cargo test --release -p makepad-motion-draw --features scene3d --test pixels`

use std::path::PathBuf;
use std::process::Command;

fn run(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_makepad-motion"))
        .args(args)
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .output()
        .expect("makepad-motion runs");
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "makepad-motion {args:?} failed:\n{text}");
    text
}

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../motion/fixtures").join(format!("{name}.splash")).to_string_lossy().to_string()
}

fn hashes(out: &str) -> Vec<String> {
    out.lines().filter(|l| l.starts_with("frame ")).map(|l| l.split_whitespace().nth(2).unwrap().to_string()).collect()
}

/// The one frame a `frames` run wrote into `dir`.
fn only_png(dir: &std::path::Path) -> PathBuf {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|x| x == "png")).expect("a frame")
}

/// A PNG's 8-bit channel values.
fn pixels(path: &std::path::Path) -> Vec<u8> {
    use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
    use makepad_zune_png::makepad_zune_core::options::DecoderOptions;
    let bytes = std::fs::read(path).unwrap();
    let mut decoder = makepad_zune_png::PngDecoder::new_with_options(ZCursor::new(&bytes), DecoderOptions::default().png_set_strip_to_8bit(true));
    decoder.decode_raw().unwrap()
}

fn dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("motion_pixels_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fixtures_render_identically_twice_and_move() {
    for name in ["kinetic_title", "logo_reveal", "lower_third"] {
        let out = dir(name);
        let times = "0.2,0.6,1.0,1.4,2.0,3.0,4.6";
        let a = run(&["frames", &fixture(name), "--at", times, "--width", "640", "--out", &out.join("a").to_string_lossy()]);
        let b = run(&["frames", &fixture(name), "--at", times, "--width", "640", "--out", &out.join("b").to_string_lossy()]);
        let (ha, hb) = (hashes(&a), hashes(&b));
        assert_eq!(ha.len(), 7, "{a}");
        assert_eq!(ha, hb, "{name}: two runs differ");
        let distinct: std::collections::HashSet<_> = ha.iter().collect();
        assert!(distinct.len() >= 5, "{name}: only {} distinct frames: {ha:?}", distinct.len());
        assert!(!a.contains("error "), "{name}: {a}");
    }
}

#[test]
fn a_contact_sheet_is_written() {
    let out = dir("strip");
    let png = out.join("strip.png");
    let report = run(&["strip", &fixture("logo_reveal"), "--count", "8", "--width", "240", "--out", &png.to_string_lossy()]);
    assert!(report.contains("strip 8 frames 960x480"), "{report}");
    assert!(std::fs::metadata(&png).unwrap().len() > 1000);
}

#[test]
fn effects_blur_trims_and_text_draw() {
    let out = dir("effects");
    let doc = out.join("effects.splash");
    std::fs::write(&doc, r#"use mod.motion.*; Motion{
    size: vec2(320, 180) duration: 2.0 background: #000000
    layers: [
        Rect{at: vec2(20, 20) size: vec2(100, 60) fill: #ff0000}
        Rect{at: vec2(180, 20) size: vec2(100, 60) fill: #00ff00 feather: 16}
        Ellipse{at: vec2(40, 100) size: vec2(60, 60) stroke: #ffffff width: 6 trim_end: 0.5}
        Text{text: "Hi" size: 40 font: Font{family: "Inter" weight: 800} at: vec2(240, 110) align: @center color: #0000ff
             effects: [Effect{name: @glow radius: 12}]}
    ]
}"#).unwrap();
    let a = run(&["frames", &doc.to_string_lossy(), "--at", "1", "--width", "320", "--out", &out.join("a").to_string_lossy()]);
    let b = run(&["frames", &doc.to_string_lossy(), "--at", "1", "--width", "320", "--out", &out.join("b").to_string_lossy()]);
    assert_eq!(hashes(&a), hashes(&b), "effects render identically twice");
    assert!(!a.contains("error "), "{a}");
}

/// Every font a Text can name draws ink: its face resolves wherever the
/// crates live (the display faces are makepad-motion's resources, named
/// to the VM by `makepad_motion::register_resources`), so moving a crate
/// cannot silently blank a font again.
#[test]
fn every_bundled_font_draws_ink() {
    let out = dir("fonts");
    for font in makepad_doc_fonts::bundled() {
        let stem = font.replace(' ', "_");
        let doc = out.join(format!("{stem}.splash"));
        std::fs::write(&doc, format!("use mod.motion.*; Motion{{size: vec2(640, 320) duration: 1.0 background: #000000 layers: [Text{{text: \"Xg\" font: Font{{family: \"{font}\"}} size: 180 color: #ffffff at: vec2(320, 160) align: @center}}]}}")).unwrap();
        let dir = out.join(&stem);
        let report = run(&["frames", &doc.to_string_lossy(), "--at", "0.5", "--width", "320", "--out", &dir.to_string_lossy()]);
        assert!(!report.contains("empty font family"), "{font}: {report}");
        let rgba = pixels(&only_png(&dir));
        let ink = rgba.chunks_exact(4).filter(|px| px[0] > 128).count();
        assert!(ink > 500, "{font} drew {ink} bright pixels");
    }
}

/// `makepad-motion` with extra environment (another font folder).
fn run_env(args: &[&str], env: &[(&str, &std::path::Path)]) -> String {
    let (ok, text) = run_env_status(args, env);
    assert!(ok, "makepad-motion {args:?} failed:\n{text}");
    text
}

/// `makepad-motion` with `env`: whether it succeeded, and what it printed.
fn run_env_status(args: &[&str], env: &[(&str, &std::path::Path)]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_makepad-motion"));
    command.args(args).env("MAKEPAD_HIDE_WINDOWS", "1");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("makepad-motion runs");
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

/// The columns holding bright ink in a frame: [first, last].
fn ink_columns(path: &std::path::Path, width: usize) -> [usize; 2] {
    let rgba = pixels(path);
    let columns: Vec<usize> = rgba.chunks_exact(4).enumerate().filter(|(_, px)| px[0] > 128).map(|(i, _)| i % width).collect();
    [*columns.iter().min().expect("ink"), *columns.iter().max().expect("ink")]
}

/// An external font (Stage's Resources panel) draws in its own face in a
/// Text, a Draw2D `c.text` and a KineticText once its files are on the
/// machine; when they are not, the text draws in the default face, exactly
/// as the document naming Inter, and the run says so once. The installed
/// face here is
/// Anton (bundled, condensed) placed where Archivo's variable files go
/// under `MAKEPAD_FONTS_DIR`: a face whose advances are nothing like Inter's.
#[test]
fn an_external_font_draws_in_its_own_face_once_installed() {
    let out = dir("external_font");
    let installed = out.join("installed");
    std::fs::create_dir_all(installed.join("archivo")).unwrap();
    let anton = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fonts/resources/fonts/anton/Anton-Regular.ttf");
    for file in ["Archivo-Variable.ttf", "Archivo-Italic-Variable.ttf"] {
        std::fs::copy(&anton, installed.join("archivo").join(file)).unwrap();
    }
    let missing = out.join("missing");
    std::fs::create_dir_all(&missing).unwrap();
    let docs: [(&str, fn(&str) -> String); 3] = [
        ("text", |font: &str| format!("use mod.motion.*; Motion{{size: vec2(640, 320) duration: 1.0 background: #000000 layers: [Text{{text: \"MOVE\" font: Font{{family: \"{font}\"}} size: 120 color: #ffffff at: vec2(40, 100)}}]}}")),
        ("draw2d", |font: &str| format!("use mod.motion.*; Motion{{size: vec2(640, 320) duration: 1.0 background: #000000 layers: [Draw2D{{draw: fn(c, t, edit) {{ c.text(\"MOVE\", 40, 160, {{px: 160 font: Font{{family: \"{font}\"}} color: #ffffff}}) }}}}]}}")),
        ("kinetic", |font: &str| format!("use mod.motion.*; Motion{{size: vec2(640, 360) duration: 4.0 background: #000000 layers: [KineticText{{preset: \"321_typewriter\" text: \"MOVE TYPE\" font: Font{{family: \"{font}\"}}}}]}}")),
    ];
    for (name, doc) in docs {
        let frame = |font: &str, fonts: &std::path::Path, tag: &str| {
            let path = out.join(format!("{name}_{tag}.splash"));
            std::fs::write(&path, doc(font)).unwrap();
            let dir = out.join(format!("{name}_{tag}"));
            let env = [("MAKEPAD_FONTS_DIR", fonts)];
            let frames = run_env(&["frames", &path.to_string_lossy(), "--at", "3.0", "--width", "320", "--out", &dir.to_string_lossy()], &env);
            (hashes(&frames), run_env(&["check", &path.to_string_lossy()], &env), only_png(&dir))
        };
        let (inter, _, inter_png) = frame("Inter", &missing, "inter");
        let (own, own_report, own_png) = frame("Archivo", &installed, "own");
        assert!(!own_report.contains("not installed"), "{name}: {own_report}");
        assert_ne!(own, inter, "{name}: the installed face draws, not Inter");
        // Not installed: drawn in the default face, the very picture of the
        // document naming Inter, and said once.
        let path = out.join(format!("{name}_missing.splash"));
        std::fs::write(&path, doc("Archivo")).unwrap();
        let dir = out.join(format!("{name}_missing"));
        let drawn = run_env(&["frames", &path.to_string_lossy(), "--at", "3.0", "--width", "320", "--out", &dir.to_string_lossy()], &[("MAKEPAD_FONTS_DIR", missing.as_path())]);
        assert_eq!(hashes(&drawn), inter, "{name}: not installed, the text draws in the default face");
        let said = "font Archivo is not installed: drawn with the default face; install it in Resources for the intended look";
        assert_eq!(drawn.matches(said).count(), 1, "{name}: said once: {drawn}");
        if name != "kinetic" {
            // Anton is condensed: the same word is far narrower than Inter's.
            let (a, b) = (ink_columns(&own_png, 320), ink_columns(&inter_png, 320));
            assert!((a[1] - a[0]) * 10 < (b[1] - b[0]) * 8, "{name}: the installed face's own advances: {a:?} vs Inter {b:?}");
        }
    }
}

/// A Text's plate is drawn under its glyphs, not over them: the frame
/// with the words differs from the same plate with invisible words.
#[test]
fn a_plate_stays_under_its_text() {
    let out = dir("plate");
    let doc = |color: &str| format!("use mod.motion.*; Motion{{size: vec2(640, 360) duration: 1.0 background: #101010 layers: [Text{{text: \"Plate\" size: 64 font: Font{{family: \"Inter\" weight: 800}} color: {color} at: vec2(80, 120) plate: #ff5a36}}]}}");
    let (a, b) = (out.join("a.splash"), out.join("b.splash"));
    std::fs::write(&a, doc("#ffffff")).unwrap();
    std::fs::write(&b, doc("#ffffff00")).unwrap();
    let ha = hashes(&run(&["frames", &a.to_string_lossy(), "--at", "0.5", "--width", "320", "--out", &out.join("fa").to_string_lossy()]));
    let hb = hashes(&run(&["frames", &b.to_string_lossy(), "--at", "0.5", "--width", "320", "--out", &out.join("fb").to_string_lossy()]));
    assert_ne!(ha, hb, "the words show on their plate");
}

/// A line-mask reveal at rest draws exactly the plain text: its matte
/// covers every glyph's ink (descenders, ascenders, the full stop) and
/// its lines sit where the text engine sets them (E1 round 3:
/// "evervwhere" clipped, two-line headlines colliding).
#[test]
fn a_mask_reveal_at_rest_is_the_plain_text() {
    let out = dir("maskrest");
    for (name, text, extra) in [
        ("descenders", "everywhere, quickly.", ""),
        ("two_lines", "Folders that\\nfill themselves.", " line_height: 1.0 tracking: -3"),
        ("wrapped", "Folders that fill themselves by the rules you set", " wrap: 900 line_height: 1.0"),
    ] {
        for animate in ["mask_up", "mask_down", "rise"] {
            let doc = |anim: &str| format!("use mod.motion.*; Motion{{size: vec2(1920, 1080) duration: 3.0 background: #f4f1ea layers: [Text{{text: \"{text}\" font: Font{{family: \"Inter\" weight: 800}} size: 112 color: #16181d at: vec2(192, 300){extra}{anim}}}]}}");
            let (a, b) = (out.join(format!("{name}_plain.splash")), out.join(format!("{name}_{animate}.splash")));
            std::fs::write(&a, doc("")).unwrap();
            std::fs::write(&b, doc(&format!(" animate: @{animate} start: 0.0"))).unwrap();
            let (da, db) = (out.join(format!("{name}_fa")), out.join(format!("{name}_{animate}_fb")));
            run(&["frames", &a.to_string_lossy(), "--at", "2.9", "--width", "960", "--out", &da.to_string_lossy()]);
            run(&["frames", &b.to_string_lossy(), "--at", "2.9", "--width", "960", "--out", &db.to_string_lossy()]);
            let (pa, pb) = (pixels(&only_png(&da)), pixels(&only_png(&db)));
            assert_eq!(pa.len(), pb.len());
            // The isolated group's compositing may round a level; a clipped
            // glyph or a moved line differs by far more.
            let off = pa.iter().zip(&pb).filter(|(x, y)| x.abs_diff(**y) > 4).count();
            assert_eq!(off, 0, "{name} with @{animate} at rest differs from the plain text in {off} channel values");
        }
    }
}

/// 2D layers anchored to 3D nodes: the same pictures twice, and the HUD
/// moves with the camera's flight.
#[test]
fn anchored_huds_render_identically_twice() {
    let out = dir("anchors");
    let doc = out.join("anchors.splash");
    std::fs::write(&doc, r#"use mod.motion.*; use mod.scene; Motion{size: vec2(640, 360) duration: 2.0 background: #0b0b0d layers: [
        Scene3D{name: "set" size: vec2(640, 360)
            camera: Camera{shots: [scene.CameraShot{at: 0 hold: 0.2 position: vec3(0, 0.5, 6) target: vec3(0, 0, 0)} scene.CameraShot{at: 1.8 position: vec3(4, 1.5, 3) target: vec3(1, 0, 0)}]}
            children: [scene.Mesh{name: "eye" geometry: scene.SphereGeometry{} scale: vec3(0.4, 0.4, 0.4) position: vec3(1.5, 0.3, 0)} scene.DirectionalLight{position: vec3(2, 4, 3) intensity: 3}]}
        Rect{at: anchor("set", "eye") x: -30 y: -30 size: vec2(60, 60) stroke: #ff5a1f width: 3}]}"#).unwrap();
    let a = run(&["frames", &doc.to_string_lossy(), "--at", "0.1,1.0,1.9", "--width", "640", "--out", &out.join("a").to_string_lossy()]);
    let b = run(&["frames", &doc.to_string_lossy(), "--at", "0.1,1.0,1.9", "--width", "640", "--out", &out.join("b").to_string_lossy()]);
    let (ha, hb) = (hashes(&a), hashes(&b));
    assert_eq!(ha.len(), 3, "{a}");
    assert_eq!(ha, hb, "two runs differ");
    assert_ne!(ha[0], ha[2], "the HUD moves with the flight");
}

/// A Scene3D's 2D layer that a pass samples with `sample_grad` carries a mip
/// chain: one-pixel stripes read over an eight-pixel footprint are an even
/// grey, while a plain `sample` of the same layer (level 0) still shows
/// every stripe.
#[test]
fn a_layer_sampled_with_gradients_is_minified_through_its_mips() {
    let doc = |read: &str| {
        format!(
            "use mod.motion.*; use mod.post; let s = Scene3D{{size: vec2(640, 360) background: #000000 children: []
                layers: {{stripes: Draw2D{{size: vec2(640, 360) draw: fn(c, t, edit) {{ for i in 0..320 {{ c.rect(i * 2.0, 0.0, 1.0, 360.0, #ffffff, 1.0) }} }}}}}}
                post: [post.Pass{{reads: [\"stripes\"] pixel: fn() -> vec4 {{ let uv = self.uv() {read} }}}}]}}
            Motion{{size: vec2(640, 360) duration: 1.0 layers: [s]}}"
        )
    };
    let row = |read: &str, name: &str| {
        let out = dir(name);
        let path = out.join("doc.splash");
        std::fs::write(&path, doc(read)).unwrap();
        run(&["frames", &path.to_string_lossy(), "--at", "0.5", "--width", "640", "--out", &out.to_string_lossy()]);
        let px = pixels(&only_png(&out));
        // the middle row's green channel over the middle half of the frame
        (160..480).map(|x| px[(180 * 640 + x) * 4 + 1] as i32).collect::<Vec<_>>()
    };
    let spread = |r: &[i32]| r.iter().max().unwrap() - r.iter().min().unwrap();
    let minified = row("return self.stripes.sample_grad(uv, vec2(8.0 / 640.0, 0.0), vec2(0.0, 8.0 / 360.0))", "mips_grad");
    let plain = row("return self.stripes.sample(uv)", "mips_plain");
    assert!(spread(&plain) > 90, "level 0 shows the stripes: {plain:?}");
    assert!(spread(&minified) < 24, "the mips average them: {minified:?}");
    assert!(minified[0] > 50, "not black: {minified:?}");
}

/// A weight travelling through the letters, drawn as a playing preview
/// shows it (`--realtime`: each frame painted once, exact or not), 60
/// consecutive frames: no letter vanishes for a frame. Each new weight is
/// a new font instance whose slug curves come from the host a moment
/// later; the frame records once, so before, a letter at a weight not
/// drawn yet drew as its zero-curve box (the motion deck's weight wave
/// flickered: 167 one-frame dips and pops over 3 s). Now the curves of the
/// nearest weight stand in until its own arrive.
#[test]
fn an_animated_weight_never_drops_a_letter() {
    let out = dir("weight_wave");
    let times: Vec<String> = (0..60).map(|k| format!("{:.5}", 1.0 + k as f64 / 60.0)).collect();
    let report = run(&["frames", &fixture("weight_wave"), "--realtime", "--at", &times.join(","), "--width", "1280", "--out", &out.to_string_lossy()]);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "png")).collect();
    files.sort();
    assert_eq!(files.len(), 60, "{report}");
    // Ink per 16-pixel column band over the text's rows (60..200 of 240).
    let (w, bands) = (1280usize, 1280 / 16);
    let ink: Vec<Vec<u32>> = files.iter().map(|f| {
        let px = pixels(f);
        let ch = px.len() / (w * 240);
        let mut b = vec![0u32; bands];
        for y in 60..200 {
            for x in 0..w {
                let at = (y * w + x) * ch;
                b[x / 16] += (px[at] as u32 + px[at + 1] as u32 + px[at + 2] as u32) / 3;
            }
        }
        b
    }).collect();
    let floor = *ink.iter().flatten().min().unwrap();
    let mut dips = Vec::new();
    for i in 1..ink.len() - 1 {
        for b in 0..bands {
            let (p, c, n) = (ink[i - 1][b] - floor, ink[i][b] - floor, ink[i + 1][b] - floor);
            // a band with letters in the frames before and after, and (almost) none in this one
            if p.min(n) > 4000 && c * 3 < p.min(n) {
                dips.push(format!("{} x {}..{}: {p} {c} {n}", files[i].file_name().unwrap().to_string_lossy(), b * 16, b * 16 + 16));
            }
        }
    }
    assert!(dips.is_empty(), "letters vanished for a frame:\n{}", dips.join("\n"));
}

/// A Shader layer reads its `data` list exactly on the GPU: floats and
/// texels read with `self.data_float(i)` / `self.data_vec4(i)` across a
/// list longer than one texture row (5000 floats, 1250 texels, rows of
/// 1024) draw the same pixels as the same values computed in the shader.
#[test]
fn a_shader_reads_its_data_exactly() {
    let n = 5000;
    let values: Vec<String> = (0..n).map(|k| format!("{}", (k % 256) as f64 / 256.0)).collect();
    let read = format!("fn() -> vec4 {{ let i = floor(self.pos.x * {n}.0) let v = self.data_float(i) let q = self.data_vec4(floor(i / 4.0)) return vec4(v, q.y, q.w, 1.0) }}");
    let by_hand = format!("fn() -> vec4 {{ let i = floor(self.pos.x * {n}.0) let t = floor(i / 4.0) * 4.0 let v = (i - 256.0 * floor(i / 256.0)) / 256.0 let y = (t + 1.0 - 256.0 * floor((t + 1.0) / 256.0)) / 256.0 let w = (t + 3.0 - 256.0 * floor((t + 3.0) / 256.0)) / 256.0 return vec4(v, y, w, 1.0) }}");
    let doc = format!("use mod.motion.*\nMotion{{size: vec2(1280, 720) duration: 1.0 background: #000000 layers: [\n    Shader{{at: vec2(0, 0) size: vec2(640, 720) data: [{}] pixel: {read}}}\n    Shader{{at: vec2(640, 0) size: vec2(640, 720) pixel: {by_hand}}}\n]}}", values.join(", "));
    let out = dir("shader_data");
    let src = out.join("doc.splash");
    std::fs::write(&src, doc).unwrap();
    let frames = out.join("out");
    run(&["frames", &src.to_string_lossy(), "--at", "0.5", "--width", "1280", "--out", &frames.to_string_lossy()]);
    let px = pixels(&only_png(&frames));
    let (w, h) = (1280usize, 720usize);
    let ch = px.len() / (w * h);
    let mut seen = std::collections::HashSet::new();
    for y in (0..h).step_by(37) {
        for x in 0..w / 2 {
            let (a, b) = ((y * w + x) * ch, (y * w + x + w / 2) * ch);
            assert_eq!(px[a..a + 3], px[b..b + 3], "pixel ({x}, {y}): data against the values computed");
            seen.insert(px[a]);
        }
    }
    assert!(seen.len() > 200, "the data's values show ({} levels)", seen.len());
}

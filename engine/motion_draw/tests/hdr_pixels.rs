//! The HDR Motion frame (`color: @hdr`) on the real GPU, through
//! `makepad-motion` in a hidden window: ordinary colours show as authored,
//! an emissive colour blooms through the render graph's Glow while white
//! type does not, and whole-frame motion blur smooths a fast edge. Needs a GPU and a window server:
//!
//! `cargo test --release -p makepad-motion-draw --features scene3d --test hdr_pixels`

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_makepad-motion")).args(args).env("MAKEPAD_HIDE_WINDOWS", "1").output().expect("makepad-motion runs");
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "makepad-motion {args:?} failed:\n{text}");
    text
}

fn dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("hdr_pixels_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Render `doc` at `t` (960 px wide) and decode it: (width, height, rgba).
fn frame(name: &str, doc: &str, t: f32) -> (usize, usize, Vec<u8>) {
    let d = dir(name);
    let src = d.join("doc.splash");
    std::fs::write(&src, doc).unwrap();
    let out = d.join("out");
    run(&["frames", &src.to_string_lossy(), "--at", &t.to_string(), "--out", &out.to_string_lossy()]);
    let png = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|x| x == "png")).unwrap();
    decode(&png)
}

fn decode(path: &Path) -> (usize, usize, Vec<u8>) {
    use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
    use makepad_zune_png::makepad_zune_core::options::DecoderOptions;
    let bytes = std::fs::read(path).unwrap();
    let mut decoder = makepad_zune_png::PngDecoder::new_with_options(ZCursor::new(&bytes), DecoderOptions::default().png_set_strip_to_8bit(true));
    let px = decoder.decode_raw().unwrap();
    let (w, h) = decoder.dimensions().unwrap();
    (w, h, px)
}

fn at(img: &(usize, usize, Vec<u8>), fx: f32, fy: f32) -> [u8; 3] {
    let (w, h, px) = img;
    let ch = px.len() / (w * h);
    let (x, y) = ((fx * *w as f32) as usize, (fy * *h as f32) as usize);
    let i = (y * w + x) * ch;
    [px[i], px[i + 1], px[i + 2]]
}

const BODY: &str = r#"background: #101018 layers: [
    Rect{size: vec2(1280, 720) fill: #101018}
    Rect{size: vec2(200, 200) fill: #3080ff at: vec2(100, 100)}
    Text{text: "TYPE" size: 120 at: vec2(500, 80) color: #ffffff}
    Path{svg: "M 100 500 L 1180 500" stroke: STROKE width: 8}
    Rect{size: vec2(60, 200) fill: #ffffff at: tween(vec2(100, 540), vec2(1100, 540), 0, 1)}
]}"#;

fn doc(head: &str, stroke: &str) -> String {
    format!("use mod.motion.*; use mod.post; Motion{{size: vec2(1280, 720) duration: 1.0 {head} {}", BODY.replace("STROKE", stroke))
}

#[test]
fn an_hdr_frame_shows_colours_as_authored_and_blooms_what_is_emissive() {
    let display = frame("display", &doc("", "#ff5a1f"), 0.5);
    let hdr = frame("hdr", &doc("color: @hdr", "#ff5a1f"), 0.5);
    // A flat colour: the same picture in linear light, tone mapped.
    let (a, b) = (at(&display, 0.12, 0.2), at(&hdr, 0.12, 0.2));
    assert!((0..3).all(|k| (a[k] as i32 - b[k] as i32).abs() <= 1), "flat colour {a:?} vs {b:?}");
    // Glow: an emissive line (4x) gets a halo; white type (1.0) does not.
    let glow = frame("glow", &doc("color: @hdr post: [post.Glow{strength: 1.2 threshold: 1}]", "emit(#ff5a1f, 4)"), 0.5);
    let halo = at(&glow, 0.3, 500.0 / 720.0 - 0.03);
    let plain = at(&hdr, 0.3, 500.0 / 720.0 - 0.03);
    assert!(halo[0] as i32 > plain[0] as i32 + 20, "halo above the line {halo:?} vs {plain:?}");
    let (ty, ty_plain) = (at(&glow, 0.45, 0.07), at(&hdr, 0.45, 0.07));
    assert!((0..3).all(|k| (ty[k] as i32 - ty_plain[k] as i32).abs() <= 2), "white type does not glow: {ty:?} vs {ty_plain:?}");
}

#[test]
fn whole_frame_motion_blur_smears_a_fast_edge() {
    let still = frame("still", &doc("color: @hdr", "#ff5a1f"), 0.5);
    let blurred = frame("blurred", &doc("color: @hdr motion_blur: post.MotionBlur{shutter: 360}", "#ff5a1f"), 0.5);
    // The bar moves 1000 units a second: 360 degrees at 60 fps smear it by
    // ~17 units; count partly covered pixels along its row.
    let row = |img: &(usize, usize, Vec<u8>)| (0..img.0).filter(|&x| {
        let c = at(img, x as f32 / img.0 as f32, 640.0 / 720.0);
        c[0] > 40 && c[0] < 220
    }).count();
    let (a, b) = (row(&still), row(&blurred));
    assert!(b > a + 8, "partly covered pixels {b} blurred vs {a} still");
}

#[test]
fn a_data_layer_is_read_by_a_pass_with_its_values_as_written() {
    // 0.5 written into the data layer reaches the pass as 0.5 (not decoded
    // to 0.21), so the frame shows linear 0.5, sRGB 188.
    let doc = |post: &str| format!(r#"use mod.motion.*; use mod.post; Motion{{size: vec2(1280, 720) duration: 1.0 color: @hdr background: #000000 {post}
        layers: [Rect{{name: "field" data_layer: true size: vec2(1280, 720) fill: rgba(0.5, 0.0, 0.0, 1.0)}}]}}"#);
    let img = frame("data", &doc(r#"post: [post.Pass{reads: [@color, "field"] pixel: fn() -> vec4 { let f = self.field.sample(self.uv()) return vec4(f.x, f.x, f.x, 1.0) }}]"#), 0.5);
    let c = at(&img, 0.5, 0.5);
    assert!((c[0] as i32 - 188).abs() <= 3 && c[1] == c[0], "{c:?}");
    // Without the pass the data layer does not draw.
    let plain = frame("data_plain", &doc(""), 0.5);
    assert!(at(&plain, 0.5, 0.5)[0] < 5, "{:?}", at(&plain, 0.5, 0.5));
}

#[test]
fn render_kit_effects_carry_light_over_one() {
    // A blurred emissive square (4x white): the render kit's blur runs in
    // half float in an HDR frame, so its halo is brighter than a plain
    // white square's instead of both clamping at 1.
    let doc = |fill: &str| format!(r#"use mod.motion.*; use mod.post; Motion{{size: vec2(1280, 720) duration: 1.0 color: @hdr background: #000000 layers: [
        Rect{{size: vec2(200, 200) at: vec2(540, 260) fill: {fill} blur: 30}}]}}"#);
    let (bright, plain) = (frame("kit_emit", &doc("emit(#ffffff, 4)"), 0.5), frame("kit_plain", &doc("#ffffff"), 0.5));
    let (a, b) = (at(&bright, 500.0 / 1280.0, 0.5), at(&plain, 500.0 / 1280.0, 0.5));
    assert!(a[0] as i32 > b[0] as i32 + 40, "halo {a:?} vs {b:?}");
}

#[test]
fn a_vj_effect_in_an_hdr_frame_sees_and_gives_colours_as_authored() {
    // A vj_fx effect over the frame is display-referred: in an HDR frame it
    // reads its input sRGB-encoded and hands linear light back, so a flat
    // colour through it shows as it does in a display frame.
    let doc = |head: &str| format!(r#"use mod.motion.*; use mod.post; Motion{{size: vec2(1280, 720) duration: 1.0 {head} background: #000000 layers: [
        Rect{{size: vec2(1280, 720) fill: #3080ff}}
        Shader{{effect: "vjfx/18_video_tiltshift" input: @below size: vec2(1280, 720)}}]}}"#);
    let (display, hdr) = (frame("vj_display", &doc(""), 0.5), frame("vj_hdr", &doc("color: @hdr"), 0.5));
    let (a, b) = (at(&display, 0.5, 0.5), at(&hdr, 0.5, 0.5));
    assert!(a[2] > 150, "the effect shows its input: {a:?}");
    assert!((0..3).all(|k| (a[k] as i32 - b[k] as i32).abs() <= 6), "display {a:?} vs hdr {b:?}");
}

#[test]
fn an_hdr_frame_with_a_scene3d_and_post_in_two_stages_finishes() {
    // A Scene3D re-attaches its passes each paint; the post graph's stages
    // (Glow at @hdr, Vignette at @final) keep their own order through it
    // instead of never painting.
    let doc = r#"use mod.motion.*; use mod.scene; use mod.post; Motion{size: vec2(1280, 720) duration: 1.0 color: @hdr background: #0a0a0b
        layers: [Scene3D{name: "set" size: vec2(1280, 720)
            camera: Camera{position: vec3(0, 0.5, 4) target: vec3(0, 0, 0) fov: 38}
            lights: [scene.AmbientLight{}]
            children: [scene.Mesh{name: "ball" geometry: scene.SphereGeometry{radius: 0.6} material: scene.MeshBasicMaterial{color: #ff4d12}}]}]
        post: [post.Glow{strength: 0.8}, post.Vignette{}]}"#;
    let img = frame("scene3d_post", doc, 0.5);
    let c = at(&img, 0.5, 0.5);
    assert!(c[0] > 150 && c[2] < 120, "the ball shows: {c:?}");
}

#[test]
fn a_scene_reading_its_2d_layer_paints_each_frame_once() {
    // A Scene3D pass reading the scene's 2D `layers:` picture: the picture
    // paints before the chain samples it, so a preview frame paints once
    // (it painted twice when the picture hung under the container, after
    // the pass sampling it).
    let d = dir("layer_once");
    let src = d.join("doc.splash");
    std::fs::write(&src, r#"use mod.motion.*; use mod.scene; use mod.post; Motion{size: vec2(1280, 720) duration: 2.0 color: @hdr background: #000000
        layers: [Scene3D{name: "s" size: vec2(1280, 720)
            camera: Camera{position: vec3(0, 0, 10) target: vec3(0, 0, 0) fov: 40}
            layers: {atlas: Group{layers: [Text{text: "HI" size: 100 at: vec2(50, 50) color: #ff0000}]}}
            children: [scene.Mesh{geometry: scene.PlaneGeometry{width: 3 height: 3} material: scene.MeshBasicMaterial{color: vec4(0.5, 0.5, 0.5, 1.0)}}]
            post: [post.Pass{reads: [@color, "atlas"] uniforms: {k: fn(t) { vec4(t, 0.0, 0.0, 1.0) }}
                pixel: fn() -> vec4 { let c = self.color.sample(self.uv()) + self.atlas.sample(self.uv()) return vec4(c.xyz + self.k.x * 0.01, 1.0) }}]}]}"#).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_makepad-motion"))
        .args(["bench", &src.to_string_lossy(), "--realtime", "--width", "480"])
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .env("MAKEPAD_MOTION_GPU", "1")
        .output()
        .expect("makepad-motion runs");
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "bench failed:\n{text}");
    // "gpu per frame by pass (ms, N frames):" then "  ms  samples  name".
    let frames: usize = text.split("gpu per frame by pass (ms, ").nth(1).and_then(|s| s.split(' ').next()).and_then(|n| n.parse().ok()).expect(&text);
    let samples = |name: &str| text.lines().find(|l| l.trim_end().ends_with(name)).and_then(|l| l.split_whitespace().nth(2)).and_then(|n| n.parse::<usize>().ok()).expect(&text);
    let pass = samples("graph Scene3D.post[0] Pass");
    // (A few first frames paint again while their glyphs and shaders land.)
    assert!(pass * 2 <= frames * 3, "the scene's pass painted {pass} times in {frames} frames:\n{text}");
}

#[test]
fn the_film_post_kits_do_what_they_say() {
    let doc = |layers: &str, post: &str| format!(r#"use mod.motion.*; use mod.post; Motion{{size: vec2(1280, 720) duration: 1.0 color: @hdr background: #000000 layers: [{layers}] post: [{post}]}}"#);
    // Bloom: a small hot square spreads far (the 1/128 level), and its
    // halation is red-orange.
    let square = "Rect{size: vec2(40, 40) at: vec2(620, 340) fill: emit(#ffffff, 6)}";
    let plain = frame("film_plain", &doc(square, ""), 0.5);
    let bloom = frame("film_bloom", &doc(square, "post.Bloom{strength: 0.7 threshold: 0.82 halation: 0.25}"), 0.5);
    let (far, far_plain) = (at(&bloom, 800.0 / 1280.0, 0.5), at(&plain, 800.0 / 1280.0, 0.5));
    assert!(far[0] as i32 > far_plain[0] as i32 + 6, "a wide halo 160 px out: {far:?} vs {far_plain:?}");
    assert!(far[0] > far[2], "halation warms the halo: {far:?}");
    // Shoulder: linear 0.896 (#f3f3f3) rolls off to 0.853 (sRGB 238).
    let grey = "Rect{size: vec2(1280, 720) fill: #f3f3f3}";
    let s = at(&frame("film_shoulder", &doc(grey, "post.Shoulder{}"), 0.5), 0.5, 0.5);
    assert!((236..=240).contains(&s[0]), "shoulder {s:?}");
    // Vignette in linear light: a white frame's corner falls to 0.795
    // (sRGB 231), its centre stays white.
    let white = "Rect{size: vec2(1280, 720) fill: #ffffff}";
    let v = frame("film_vignette", &doc(white, "post.Vignette{mode: 1}"), 0.5);
    let (corner, centre) = (at(&v, 0.001, 0.001), at(&v, 0.5, 0.5));
    assert!((227..=235).contains(&corner[0]) && centre[0] >= 253, "corner {corner:?} centre {centre:?}");
    // Grain: strongest in the mid-tones, and the blacks still have some.
    let spread = |img: &(usize, usize, Vec<u8>), fx: f32| {
        let vals: Vec<f32> = (0..200).map(|k| at(img, fx + k as f32 * 0.0005, 0.5)[1] as f32).collect();
        let m = vals.iter().sum::<f32>() / vals.len() as f32;
        (vals.iter().map(|v| (v - m) * (v - m)).sum::<f32>() / vals.len() as f32).sqrt()
    };
    let halves = "Rect{size: vec2(640, 720) fill: #000000} Rect{size: vec2(640, 720) at: vec2(640, 0) fill: #767676}";
    let g = frame("film_grain", &doc(halves, "post.Grain{amount: 0.1}"), 0.5);
    let (black, mid) = (spread(&g, 0.1), spread(&g, 0.7));
    assert!(black > 1.5 && mid > black * 1.5, "grain spread black {black:.2} mid {mid:.2}");
}

#[test]
fn shape_fills_keep_light_over_one() {
    // Plain Rect fills in an HDR frame, read back through a pass that
    // scales the linear light by 1/4: emit() (written and from fn(t)), a
    // raw colour over 1, and an additive fill at half opacity.
    let doc = r#"use mod.motion.*; use mod.post; Motion{size: vec2(1280, 720) duration: 1.0 color: @hdr background: #000000
        post: [post.Pass{at: @hdr reads: [@color] pixel: fn() -> vec4 { let c = self.color.sample(self.uv()) return vec4(c.xyz * 0.25, 1.0) }}]
        layers: [
            Rect{size: vec2(200, 200) at: vec2(40, 260) fill: emit(#ff4d12, 4.0)}
            Rect{size: vec2(200, 200) at: vec2(280, 260) fill: fn(t) { emit(#ff4d12, 4.0) }}
            Rect{size: vec2(200, 200) at: vec2(520, 260) fill: fn(t) { {color: [2.0, 2.0, 2.0, 1.0]} }}
            Rect{size: vec2(200, 200) at: vec2(760, 260) fill: emit(#ffffff, 3.0) blend: @add opacity: 0.5}
            Ellipse{size: vec2(200, 200) at: vec2(1000, 260) fill: emit(#ffffff, 2.0)}
            Rect{size: vec2(200, 120) at: vec2(40, 560) fill: rgba(3.0, 0.0, 0.0, 1.0)}
            Draw2D{name: "d2" draw: fn(c, t, edit) { c.rect(280, 560, 200, 120, emit(#ffffff, 2.0)) }}
        ]}"#;
    let img = frame("shape_hdr", doc, 0.5);
    let lin = |v: u8| { let c = v as f64 / 255.0; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } };
    let read = |x: f32, y: f32| { let c = at(&img, x / 1280.0, y / 720.0); [lin(c[0]) * 4.0, lin(c[1]) * 4.0, lin(c[2]) * 4.0] };
    // (An additive layer among them: it used to clip everything under it.)
    let rows = [(140.0, 360.0, 4.0, "emit"), (380.0, 360.0, 4.0, "fn emit"), (620.0, 360.0, 2.0, "raw 2"), (860.0, 360.0, 1.5, "additive 3 x 0.5"), (1100.0, 360.0, 2.0, "ellipse emit 2"), (140.0, 620.0, 3.0, "rgba(3, 0, 0)"), (380.0, 620.0, 2.0, "Draw2D rect emit 2")];
    for (x, y, want, what) in rows {
        let got = read(x, y);
        assert!((got[0] - want).abs() < want * 0.06, "{what}: red {:.3}, want {want}", got[0]);
    }
}

#[test]
fn fused_post_passes_match_the_passes_run_apart() {
    // Shoulder and a linear Vignette run as one fused pass (both map kits);
    // the same two written as ordinary passes run apart, with a half-float
    // frame between them. They agree to 2 levels on under 0.1% of pixels.
    let scene = r#"layers: [
        Rect{size: vec2(1280, 720) fill: #20242c}
        Rect{size: vec2(500, 300) at: vec2(390, 210) fill: emit(#ff4d12, 3.0)}
        Text{text: "FUSE" size: 160 at: vec2(420, 260) color: #efe8dc font: Font{family: "Inter" weight: 900}}
    ]"#;
    let fused = format!("use mod.motion.*; use mod.post; Motion{{size: vec2(1280, 720) duration: 1.0 color: @hdr background: #000000 {scene} post: [post.Shoulder{{}} post.Vignette{{mode: 1}}]}}");
    let shoulder = r#"post.Pass{at: @hdr reads: [@color] pixel: fn() -> vec4 { let c = self.color.sample(self.uv()) let a = clamp(c.w, 0.0, 1.0) if a < 0.0001 { return c } let x = max(c.xyz / a, vec3(0.0, 0.0, 0.0)) let k = 0.72 let r = vec3(k, k, k) + (1.0 - k) * (vec3(1.0, 1.0, 1.0) - exp((vec3(k, k, k) - x) / (1.0 - k))) var y = mix(x, r, step(vec3(k, k, k), x)) let m = max(x.x, max(x.y, x.z)) y = mix(y, vec3(1.0, 1.0, 1.0), smoothstep(2.0, 12.0, m) * 0.85) return vec4(y * a, c.w) }}"#;
    let vignette = r#"post.Pass{at: @hdr reads: [@color] pixel: fn() -> vec4 { let c = self.color.sample(self.uv()) let dc = (self.uv() - vec2(0.5, 0.5)) * vec2(1.0, 0.8) let v = mix(1.0, smoothstep(0.95, 0.25, length(dc)), 0.35) return vec4(c.xyz * v, c.w) }}"#;
    let apart = format!("use mod.motion.*; use mod.post; Motion{{size: vec2(1280, 720) duration: 1.0 color: @hdr background: #000000 {scene} post: [{shoulder} {vignette}]}}");
    let (a, b) = (frame("fused", &fused, 0.5), frame("apart", &apart, 0.5));
    let (mut over1, mut worst) = (0usize, 0i32);
    let n = a.2.len() / 4;
    for (pa, pb) in a.2.chunks(4).zip(b.2.chunks(4)) {
        let d = (0..3).map(|k| (pa[k] as i32 - pb[k] as i32).abs()).max().unwrap();
        worst = worst.max(d);
        if d > 1 {
            over1 += 1;
        }
    }
    assert!(worst <= 2 && (over1 as f64) < n as f64 * 0.001, "worst {worst}, {over1} of {n} pixels over 1 level");
}

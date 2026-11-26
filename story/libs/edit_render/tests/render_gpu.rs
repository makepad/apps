//! The renderer on the real GPU, end to end, through the standalone binary
//! in a hidden window: the fixture edit rendered twice must give the same
//! 120 distinct frames and the same decoded pictures and sound, and random
//! preview seeks must show exactly the render's frames; and flashes and
//! clicks must stay in step. Needs a GPU and a window server:
//!
//! `cargo test --release -p makepad-story-edit-render --test render_gpu`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Condvar, Mutex};

/// Runs of the binary at once. Under the JIT each is a whole run of it, its program loaded: 1.3
/// to 2.2 GB. The tests' threads started one each, ten at once (17.4 GB, 11-08).
const SLOTS: usize = 2;

static FREE: Mutex<usize> = Mutex::new(SLOTS);
static FREED: Condvar = Condvar::new();

/// A slot, held while its run of the binary goes on.
struct Slot;

impl Slot {
    fn take() -> Slot {
        let mut free = FREE.lock().unwrap();
        while *free == 0 {
            free = FREED.wait(free).unwrap();
        }
        *free -= 1;
        Slot
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        *FREE.lock().unwrap() += 1;
        FREED.notify_one();
    }
}

fn run(args: &[&str]) -> String {
    let slot = Slot::take();
    let output = Command::new(env!("CARGO_BIN_EXE_makepad-edit-render"))
        .args(args)
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .output()
        .expect("makepad-edit-render runs");
    drop(slot);
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "makepad-edit-render {args:?} failed:\n{text}");
    text
}

fn hashes(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path).expect("hashes").lines().map(|line| line.split_once(' ').expect("n hash").1.to_string()).collect()
}

#[test]
fn two_renders_are_identical_and_the_preview_shows_their_frames() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_gpu_{}", std::process::id()));
    let s = |p: &str| dir.join(p).to_string_lossy().to_string();
    run(&["fixture", &s("fixture")]);
    for n in ["1", "2"] {
        let out = run(&["render", "--edit", &s("fixture/fixture.splash"), "--media-file", &s("fixture/media.txt"),
            "--out", &s(&format!("r{n}.mp4")), "--hashes", &s(&format!("r{n}.txt"))]);
        assert!(out.contains("done 120 frames 640x360 at 30/1 fps, audio true"), "{out}");
    }
    let (a, b) = (hashes(&dir.join("r1.txt")), hashes(&dir.join("r2.txt")));
    assert_eq!(a.len(), 120);
    assert_eq!(a, b);
    // Frame 0 shows clip A: an all-black 640x360 frame (what a paint with
    // its draws left out reads back) hashes to this.
    assert_ne!(a[0], "a9df1d5340c2c325", "frame 0 is black");
    let distinct: std::collections::HashSet<_> = a.iter().collect();
    assert!(distinct.len() > 100, "only {} distinct frames", distinct.len());
    let probe = run(&["probe", &s("r1.mp4")]);
    assert!(probe.contains("640x360 at 30/1 fps, 120 frames"), "{probe}");
    // Renders are compared by their frame hashes (above), not decoded: the
    // hardware H.264 encoder is not bit-exact from run to run.
    let preview = run(&["preview", "--edit", &s("fixture/fixture.splash"), "--media-file", &s("fixture/media.txt"),
        "--at", "@0,@45,@60,@67,@119,@66", "--hashes", &s("r1.txt")]);
    assert_eq!(preview.matches("= render frame").count(), 12, "{preview}");
    // One thumbnail engine, retargeted: each time is the render's frame.
    let thumbs = run(&["thumbnail", "--edit", &s("fixture/fixture.splash"), "--media-file", &s("fixture/media.txt"),
        "--at", "@45,@10,@119,@45", "--width", "640", "--out", &s("t.jpg")]);
    for n in [45, 10, 119] {
        assert!(thumbs.contains(&format!("RGBA hash {}", a[n])), "frame {n}: {thumbs}");
    }
    assert_eq!(thumbs.matches("edit-render: thumbnail").count(), 4, "{thumbs}");
    // Its captions were written beside it.
    assert!(dir.join("r1.subs.srt").is_file() && dir.join("r1.subs.vtt").is_file());
}

/// Flashes and clicks placed at the same instants in a source stay
/// together through a cut that moves them (`in: 0.5` at 0.2 s): every
/// flash frame of the render has its click within 1 ms.
#[test]
fn pictures_and_sound_stay_in_step() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_av_{}", std::process::id()));
    let s = |p: &str| dir.join(p).to_string_lossy().to_string();
    run(&["fixture", &s("fixture")]);
    let source = run(&["av-check", &s("fixture/av_clip.mp4"), "--flashes", "20,47,95,131"]);
    assert!(source.contains("av aligned, 4 flashes"), "{source}");
    run(&["render", "--edit", &s("fixture/av.splash"), "--media-file", &s("fixture/media.txt"), "--out", &s("av.mp4")]);
    let render = run(&["av-check", &s("av.mp4"), "--flashes", "11,38,86,122"]);
    assert!(render.contains("av aligned, 4 flashes"), "{render}");
}

/// Compound clips (§3.1): a cut nesting a cut nesting clip A renders
/// byte-identically twice, and the same as the cut written flat (the
/// deep edit's own captions stay off: no parent opts in). Preview seeks
/// into the nest show the render's frames.
#[test]
fn nested_edits_render_like_their_flat_equivalent() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_nest_{}", std::process::id()));
    let s = |p: &str| dir.join(p).to_string_lossy().to_string();
    run(&["fixture", &s("fixture")]);
    for (edit, out) in [("nested", "n1"), ("nested", "n2"), ("nested_flat", "flat")] {
        let done = run(&["render", "--edit", &s(&format!("fixture/{edit}.splash")), "--media-file", &s("fixture/media.txt"),
            "--out", &s(&format!("{out}.mp4")), "--hashes", &s(&format!("{out}.txt"))]);
        assert!(done.contains("done 120 frames 640x360 at 30/1 fps, audio true"), "{done}");
    }
    assert_eq!(hashes(&dir.join("n1.txt")), hashes(&dir.join("n2.txt")));
    // The thumbnail API draws the same frame: at full width it is the
    // render's frame 45, pixel for pixel; smaller, it is scaled from it.
    let thumb = run(&["thumbnail", "--edit", &s("fixture/nested.splash"), "--media-file", &s("fixture/media.txt"),
        "--at", "@45", "--width", "640", "--out", &s("t45.jpg")]);
    assert!(thumb.contains(&format!("RGBA hash {}", hashes(&dir.join("n1.txt"))[45])), "{thumb}");
    let small = run(&["thumbnail", "--edit", &s("fixture/nested.splash"), "--media-file", &s("fixture/media.txt"),
        "--at", "1.5", "--width", "160", "--out", &s("t160.jpg")]);
    assert!(small.contains("thumbnail 160x90 of the frame at 1.5 s") && dir.join("t160.jpg").is_file(), "{small}");
    let flat = run(&["compare", &s("n1.mp4"), &s("flat.mp4"), "--tolerance", "1.0"]);
    assert!(flat.contains("compare within 1"), "{flat}");
    let preview = run(&["preview", "--edit", &s("fixture/nested.splash"), "--media-file", &s("fixture/media.txt"),
        "--at", "@60,@15,@45,@119", "--hashes", &s("n1.txt")]);
    assert_eq!(preview.matches("= render frame").count(), 8, "{preview}");
    // Opted in at both clips (`subtitles: 1`): the deep edit's captions
    // burn in (the frames they show on differ from the uncaptioned render,
    // the others do not) and come out as the root's sidecars, mapped to its
    // time and clipped at the outer clip's start (0.5 s).
    for out in ["s1", "s2"] {
        run(&["render", "--edit", &s("fixture/nested_subs.splash"), "--media-file", &s("fixture/media.txt"),
            "--out", &s(&format!("{out}.mp4")), "--hashes", &s(&format!("{out}.txt"))]);
    }
    let (plain, captioned) = (hashes(&dir.join("n1.txt")), hashes(&dir.join("s1.txt")));
    assert_eq!(captioned, hashes(&dir.join("s2.txt")));
    let differ: Vec<usize> = (0..120).filter(|&n| plain[n] != captioned[n]).collect();
    assert_eq!(differ, (15..116).collect::<Vec<_>>(), "captioned frames");
    let srt = std::fs::read_to_string(dir.join("s1.outer.inner.subs.srt")).expect("nested captions sidecar");
    assert!(srt.starts_with("1\n00:00:00,500 --> 00:00:01,900\nEvery frame lands exactly"), "{srt}");
    assert!(srt.contains("00:00:02,800 --> 00:00:03,840\nwith it"), "{srt}");
    assert!(dir.join("s1.outer.inner.subs.vtt").is_file());
    assert!(!dir.join("n1.outer.inner.subs.srt").exists(), "no captions without the opt-in");
    let preview = run(&["preview", "--edit", &s("fixture/nested_subs.splash"), "--media-file", &s("fixture/media.txt"),
        "--at", "@30,@60,@90", "--hashes", &s("s1.txt")]);
    assert_eq!(preview.matches("= render frame").count(), 6, "{preview}");
}

/// Burned-in captions stay inside each platform's safe zone (§8.3): a
/// long caption anchored in the bottom band and one in the top-left corner,
/// over black, for @reels, @tiktok and @shorts. No bright pixel may land in
/// a band, and the captions must be there at all.
#[test]
fn captions_stay_out_of_every_platform_band() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_bands_{}", std::process::id()));
    let s = |p: &str| dir.join(p).to_string_lossy().to_string();
    run(&["fixture", &s("fixture")]);
    for preset in ["reels", "tiktok", "shorts"] {
        run(&["render", "--edit", &s(&format!("fixture/safe_{preset}.splash")), "--media-file", &s("fixture/media.txt"),
            "--out", &s(&format!("{preset}.mp4"))]);
        let check = run(&["band-check", &s(&format!("{preset}.mp4")), "--safe", preset]);
        // The fixture's [0.05, 0.95) caption covers frames 2 through 28 at 30 fps.
        assert!(check.contains("band-check clear") && check.contains(" 0 caption px in the bands") && check.contains("text on 27/30 frames"), "{check}");
    }
}

/// A Graphic of a library Motion document (the media table's
/// `<id>=<doc>.splash`, what Stage's render preparation writes), on a
/// musical span, next to a Title (the built-in title document): two
/// renders are identical, the graphic shows exactly over its span, and
/// preview seeks show the render's frames.
#[test]
fn motion_graphics_render_and_preview_exactly() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_graphics_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let s = |p: &str| dir.join(p).to_string_lossy().to_string();
    let id = "ast_stufm3o7yer634iv7s5tliljom";
    let rev = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";
    let doc = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../apps/engine/motion/fixtures/lower_third.splash");
    std::fs::write(dir.join("media.txt"), format!("{id}={}\n", doc.display())).unwrap();
    // 120 bpm: beat(2) is 1.0 s, beats(4) 2.0 s.
    std::fs::write(dir.join("cut.splash"), format!(
        "use mod.flow.*\nuse mod.cut.*\nlet g = Graphic{{src: asset(\"{id}\", \"{rev}\") span: [beat(2), beats(4)] params: {{name: \"From the library\" accent: \"#36c2ff\"}}}}\nlet t = Title{{text: \"Title\" anchor: vec2(0.5, 0.25) font_size: 48}}\nlet st = Stack{{layers: [g.out(@frame), t.out(@frame)]}}\nlet out = CutOutput{{frame: st.out(@frame)}}\nFlow{{\n    kind: @cut\n    output: {{width: 640 height: 360 fps: 30}}\n    duration: 4.0\n    bpm: 120\n    g, t, st, out\n}}\n"
    )).unwrap();
    for n in ["1", "2"] {
        let out = run(&["render", "--edit", &s("cut.splash"), "--media-file", &s("media.txt"), "--out", &s(&format!("g{n}.mp4")), "--hashes", &s(&format!("g{n}.txt"))]);
        assert!(out.contains("done 120 frames 640x360"), "{out}");
    }
    let (a, b) = (hashes(&dir.join("g1.txt")), hashes(&dir.join("g2.txt")));
    assert_eq!(a, b, "two renders of the graphics differ");
    // Before the span only the title: frames 0..30 alike; the graphic
    // animates in over its span; after it, the title alone again.
    assert!(a[..30].iter().all(|h| *h == a[0]), "the title alone is still");
    assert_ne!(a[45], a[0], "the graphic draws on its span");
    assert!(a[90..].iter().all(|h| *h == a[0]), "the graphic ends with its span");
    let distinct: std::collections::HashSet<_> = a[30..90].iter().collect();
    assert!(distinct.len() > 10, "the graphic moves: {} distinct frames", distinct.len());
    let preview = run(&["preview", "--edit", &s("cut.splash"), "--media-file", &s("media.txt"), "--at", "@0,@45,@31,@89,@90", "--hashes", &s("g1.txt")]);
    assert_eq!(preview.matches("= render frame").count(), 10, "{preview}");
}

/// An HDR Motion graphic with whole-frame motion blur over a Scene3D that
/// has graph passes finishes every frame: the cut renderer re-attaches
/// the graphic's passes each paint while its sub-frames accumulate, and
/// the accumulator's and the graphs' own pass order survives that, and
/// the pictures of the scene's 2D `layers:` are attached again with the
/// rest (they went stale and the frame waited forever).
#[test]
fn an_hdr_graphic_with_motion_blur_and_a_scene_pass_finishes() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_hdr_blur_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let s = |p: &str| dir.join(p).to_string_lossy().to_string();
    let id = "ast_grhdrblurscenepassaaaaaaaa";
    let rev = "arev_0000000000000000000000000000000000000000000000000000000000000001";
    std::fs::write(dir.join("doc.splash"), r#"use mod.motion.*; use mod.scene; use mod.post; Motion{size: vec2(640, 360) duration: 1.0 color: @hdr background: #000000
    motion_blur: post.MotionBlur{shutter: 180 samples: 108 tol: 3}
    post: [post.Bloom{strength: 0.5}]
    layers: [
        Scene3D{name: "set" size: vec2(640, 360) antialias: 1
            camera: Camera{position: vec3(0, 0, 4) target: vec3(0, 0, 0) fov: 40}
            lights: [scene.AmbientLight{}]
            layers: {plate: Group{size: vec2(640, 360) layers: [Rect{size: vec2(80, 80) at: fn(t) { vec2(40 + t * 300, 140) } fill: #36c2ff} Text{text: "PLATE" size: 40 at: vec2(400, 40) color: #ffffff}]}}
            post: [
                post.Pass{name: "half" scale: 0.5 reads: [@color] pixel: fn() -> vec4 { return self.color.sample(self.uv()) }}
                post.Pass{reads: [@color, "half", "plate"] pixel: fn() -> vec4 { let p = self.plate.sample(self.uv()) return self.color.sample(self.uv()) * 0.5 + self.half.sample(self.uv()) * 0.5 + p }}
            ]
            children: [scene.Mesh{geometry: scene.BoxGeometry{} rotation: fn(t) { vec3(t * 500, t * 1400, 0) } material: scene.MeshBasicMaterial{color: #ff5a1f}}]}
        Rect{size: vec2(40, 40) at: fn(t) { vec2(100 + t * 400, 300) } fill: emit(#ffffff, 3)}
    ]}"#).unwrap();
    std::fs::write(dir.join("media.txt"), format!("{id}={}\n", s("doc.splash"))).unwrap();
    std::fs::write(dir.join("cut.splash"), format!(
        "use mod.flow.*\nuse mod.cut.*\nlet g = Graphic{{src: asset(\"{id}\", \"{rev}\") span: [0.0, 0.5]}}\nlet out = CutOutput{{frame: g.out(@frame)}}\nFlow{{\n    kind: @cut\n    output: {{width: 640 height: 360 fps: 30}}\n    duration: 0.5\n    g, out\n}}\n"
    )).unwrap();
    let out = run(&["render", "--edit", &s("cut.splash"), "--media-file", &s("media.txt"), "--out", &s("h.mp4"), "--hashes", &s("h.txt")]);
    assert!(out.contains("done 15 frames 640x360"), "{out}");
    let h = hashes(&dir.join("h.txt"));
    let distinct: std::collections::HashSet<_> = h.iter().collect();
    assert!(distinct.len() > 5, "the graphic moves: {} distinct frames", distinct.len());
}

/// A graphic's `Freeze` shows the cut under it at the freeze's time
/// (Motion's `Need::CutFrame`): over the fixture, a full-frame freeze at
/// its own start (the frame's own layers, not drawn twice) and one half a
/// second later (that time's layers, drawn first) both show exactly the
/// plain render's frame at that time for the whole span, and the preview
/// shows the render's frames. The span starts in the fixture's dissolve
/// into its fitted clip, whose bars are part transparent while it mixes:
/// the frozen frame is the frame as shown, so the live layers under the
/// graphic do not show through it.
#[test]
fn a_freeze_shows_the_edit_at_its_time() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_freeze_{}", std::process::id()));
    let s = |p: &str| dir.join(p).to_string_lossy().to_string();
    run(&["fixture", &s("fixture")]);
    run(&["render", "--edit", &s("fixture/fixture.splash"), "--media-file", &s("fixture/media.txt"), "--out", &s("plain.mp4"), "--hashes", &s("plain.txt")]);
    let plain = hashes(&dir.join("plain.txt"));
    let fixture = std::fs::read_to_string(dir.join("fixture/fixture.splash")).unwrap();
    let media = std::fs::read_to_string(dir.join("fixture/media.txt")).unwrap();
    let (id, rev) = ("ast_stufm3o7yer634iv7s5tliljom", "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f");
    // The graphic over everything for 2.0..3.0 s.
    let edit = fixture
        .replace("let out   = ", &format!("let g     = Graphic{{src: asset(\"{id}\", \"{rev}\") span: [2.0, 1.0]}}\nlet top2  = Stack{{layers: [top.out(@frame), g.out(@frame)]}}\nlet out   = "))
        .replace("CutOutput{frame: top.out(@frame)", "CutOutput{frame: top2.out(@frame)")
        .replace("subs, top, music", "subs, top, g, top2, music");
    std::fs::write(dir.join("fixture/freeze.splash"), edit).unwrap();
    for (time, at) in [("0.0", 60), ("0.5", 75)] {
        std::fs::write(dir.join(format!("freeze_{time}.splash")), format!("use mod.motion.*; Motion{{size: vec2(640, 360) duration: 1.0 layers: [Freeze{{time: {time} size: vec2(640, 360)}}]}}")).unwrap();
        std::fs::write(dir.join(format!("fixture/media_{time}.txt")), format!("{media}{id}={}\n", s(&format!("freeze_{time}.splash")))).unwrap();
        let out = run(&["render", "--edit", &s("fixture/freeze.splash"), "--media-file", &s(&format!("fixture/media_{time}.txt")), "--out", &s(&format!("f{time}.mp4")), "--hashes", &s(&format!("f{time}.txt"))]);
        assert!(out.contains("done 120 frames 640x360"), "{out}");
        let frozen = hashes(&dir.join(format!("f{time}.txt")));
        assert_eq!(frozen[..60], plain[..60], "before the graphic");
        assert_eq!(frozen[90..], plain[90..], "after the graphic");
        assert!(frozen[60..90].iter().all(|h| *h == plain[at]), "freeze at {time}: every frame of the span is the cut's frame {at}");
        let preview = run(&["preview", "--edit", &s("fixture/freeze.splash"), "--media-file", &s(&format!("fixture/media_{time}.txt")),
            "--at", "@70,@20,@89,@60", "--hashes", &s(&format!("f{time}.txt"))]);
        assert_eq!(preview.matches("= render frame").count(), 8, "{preview}");
    }
}

/// A motion-blurred graphic's `Freeze` of another motion-blurred graphic
/// (a film's rewind of its plates): in locked time both gather their
/// shutters in the same frame. The frozen picture is the other graphic's
/// finished frame (the freeze's shutter waits for it, its own held once
/// done, not gathered again), so the render finishes, and a plain Freeze
/// over it shows exactly the render's frame at the freeze's time.
#[test]
fn a_freeze_of_a_motion_blurred_graphic_waits_for_its_shutter() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_freeze_blur_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let s = |p: &str| dir.join(p).to_string_lossy().to_string();
    let rev = "arev_0000000000000000000000000000000000000000000000000000000000000001";
    let (a, b) = ("ast_grfreezeblurplateaaaaaaaaa", "ast_grfreezeblurrewindaaaaaaaa");
    // A plate that moves fast under a 180-degree shutter: many sub-frames.
    std::fs::write(dir.join("plate.splash"), r#"use mod.motion.*; use mod.post; Motion{size: vec2(640, 360) duration: 2.0 color: @hdr background: #102030
    motion_blur: post.MotionBlur{shutter: 180 samples: 108 tol: 3}
    layers: [
        Rect{size: vec2(80, 80) at: fn(t) { vec2(40 + t * 900, 140) } fill: #36c2ff}
        Rect{size: vec2(40, 40) at: fn(t) { vec2(600 - t * 500, 60) } fill: emit(#ffffff, 2)}
    ]}"#).unwrap();
    let cut = |with_rewind: bool| format!(
        "use mod.flow.*\nuse mod.cut.*\nlet p = Graphic{{src: asset(\"{a}\", \"{rev}\") span: [0.0, 2.0]}}\n{}let out = CutOutput{{frame: {}}}\nFlow{{\n    kind: @cut\n    output: {{width: 640 height: 360 fps: 30}}\n    duration: 2.0\n    p, {}out\n}}\n",
        if with_rewind { format!("let r = Graphic{{src: asset(\"{b}\", \"{rev}\") span: [1.0, 1.0]}}\nlet top = Stack{{layers: [p.out(@frame), r.out(@frame)]}}\n") } else { String::new() },
        if with_rewind { "top.out(@frame)" } else { "p.out(@frame)" },
        if with_rewind { "r, top, " } else { "" },
    );
    std::fs::write(dir.join("plain.splash"), cut(false)).unwrap();
    std::fs::write(dir.join("rewind.splash"), cut(true)).unwrap();
    let media = |rewind: &str| format!("{a}={}\n{b}={}\n", s("plate.splash"), s(rewind));
    std::fs::write(dir.join("media.txt"), media("plate.splash")).unwrap();
    let out = run(&["render", "--edit", &s("plain.splash"), "--media-file", &s("media.txt"), "--out", &s("plain.mp4"), "--hashes", &s("plain.txt")]);
    assert!(out.contains("done 60 frames 640x360"), "{out}");
    let plain = hashes(&dir.join("plain.txt"));
    // The rewind (from 1.0 s) freezes the plate at 0.5 s (its time -0.5):
    // plain, and motion-blurred itself (HDR, its own shutter gathering).
    for (name, doc) in [
        ("plain", "use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [Freeze{time: -0.5 size: vec2(640, 360)}]}"),
        ("blurred", "use mod.motion.*; use mod.post; Motion{size: vec2(640, 360) duration: 1.0 color: @hdr background: #000000 motion_blur: post.MotionBlur{shutter: 72 samples: 4} layers: [Freeze{time: -0.5 size: vec2(640, 360)}]}"),
    ] {
        std::fs::write(dir.join(format!("rewind_{name}.splash")), doc).unwrap();
        std::fs::write(dir.join(format!("media_{name}.txt")), media(&format!("rewind_{name}.splash"))).unwrap();
        let out = run(&["render", "--edit", &s("rewind.splash"), "--media-file", &s(&format!("media_{name}.txt")), "--out", &s(&format!("{name}.mp4")), "--hashes", &s(&format!("{name}.txt"))]);
        assert!(out.contains("done 60 frames 640x360"), "{name}: {out}");
        let frozen = hashes(&dir.join(format!("{name}.txt")));
        assert_eq!(frozen[..30], plain[..30], "{name}: before the rewind");
        assert!(frozen[30..].iter().all(|h| *h == frozen[30]), "{name}: the frozen picture holds");
        if name == "plain" {
            assert!(frozen[30..].iter().all(|h| *h == plain[15]), "the frozen picture is the render's frame 15");
        }
    }
}

/// A cold first play (a fresh process: no glyph, shader or document seen
/// yet) through the preview shows no black picture where the render has
/// content, and every picture it shows as exact is the render's frame:
/// a frame recorded again because it first drew before a glyph or shader
/// was ready goes to the other output, never into the picture on screen.
/// Three Motion titles from the motion fixtures, entering at 0, 1 and 2 s.
#[test]
fn a_cold_first_play_shows_only_whole_pictures() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_cold_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let s = |p: &str| dir.join(p).to_string_lossy().to_string();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../apps/engine/motion/fixtures");
    let ids = [
        ("ast_stufm3o7yer634iv7s5tliljom", "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f", "kinetic_title.splash"),
        ("ast_ugq2dinbugq2dinbugq2dinbue", "arev_aff5428cc4632a926e7a5b3da12ac9217186396bfdd667ac102d6c93b45eae21", "lower_third.splash"),
        ("ast_wkzlfmvswkzlfmvswkzlfmvswi", "arev_b11f5c20aea563481c99bd4db9446b7a2ac56add5444e9067ca6c654aa9fe90b", "logo_reveal.splash"),
    ];
    std::fs::write(dir.join("media.txt"), ids.iter().map(|(id, _, file)| format!("{id}={}\n", fixtures.join(file).display())).collect::<String>()).unwrap();
    let graphic = |name: &str, (id, rev, _): (&str, &str, &str), start: f64| format!("let {name} = Graphic{{src: asset(\"{id}\", \"{rev}\") span: [{start}, 2.0]}}\n");
    std::fs::write(dir.join("cut.splash"), format!(
        "use mod.flow.*\nuse mod.cut.*\n{}{}{}let st = Stack{{layers: [k.out(@frame), l.out(@frame), g.out(@frame)]}}\nlet out = CutOutput{{frame: st.out(@frame)}}\nFlow{{\n    kind: @cut\n    output: {{width: 640 height: 360 fps: 30}}\n    duration: 4.0\n    bpm: 120\n    k, l, g, st, out\n}}\n",
        graphic("k", ids[0], 0.0), graphic("l", ids[1], 1.0), graphic("g", ids[2], 2.0),
    )).unwrap();
    let out = run(&["render", "--edit", &s("cut.splash"), "--media-file", &s("media.txt"), "--out", &s("r.mp4"), "--hashes", &s("r.txt")]);
    assert!(out.contains("done 120 frames 640x360"), "{out}");
    let play = run(&["scrub", "--edit", &s("cut.splash"), "--media-file", &s("media.txt"), "--pattern", "play@0", "--seconds", "4", "--cold", "--black", "--hashes", &s("r.txt")]);
    let line = play.lines().find(|line| line.trim_start().starts_with("black:")).unwrap_or_else(|| panic!("{play}"));
    assert!(line.contains("black: 0 frames"), "{line}");
    assert!(line.contains("not the render's frame: []"), "{line}");
}

/// A song's aligned lyrics (its music bake's `vj-lyrics` file) reach the
/// edit's graphics as `voice(@<clip id>)`, on the cut's clock where the
/// clip places the track: a lyric Text stays all ghost until the first sung
/// word (track 1.2 s, the clip at 1.0 s with `in: 0.5`, so edit 1.7 s),
/// changes as each word is sung, and the preview shows the render's frames.
#[test]
fn a_songs_lyrics_lock_a_graphics_words() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_lyrics_{}", std::process::id()));
    let s = |p: &str| dir.join(p).to_string_lossy().to_string();
    run(&["fixture", &s("fixture")]);
    let media = std::fs::read_to_string(dir.join("fixture/media.txt")).unwrap();
    let song = media.lines().find(|line| line.ends_with("=clip_a.mp4")).and_then(|line| line.split_once('=')).map(|(id, _)| id.to_string()).unwrap();
    std::fs::write(dir.join("fixture/lyrics.json"), r#"{"format":"vj-lyrics","version":5,"lines":[{"t0":1.2,"t1":2.4,"text":"sing it out","w":[1.2,1.6,2.0],"e":[1.5,1.9,2.4],"c":true}]}"#).unwrap();
    std::fs::write(dir.join("fixture/lyric.splash"), "use mod.motion.*; Motion{size: vec2(640, 360) duration: 4.0 fps: 30 background: #x0b0b0d layers: [\n    Text{source: voice(@song) line: 0 by: @word font: Font{family: \"Inter\" weight: 800} size: 48 color: #xf2efe8 ghost: #xf2efe826 highlight: @active highlight_color: #xff5a1f at: vec2(40, 150)}\n]}\n").unwrap();
    let (id, rev) = ("ast_stufm3o7yer634iv7s5tliljom", "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f");
    std::fs::write(dir.join("fixture/lyric_media.txt"), format!("{media}lyrics:{song}=lyrics.json\n{id}=lyric.splash\n")).unwrap();
    let rev_a = std::fs::read_to_string(dir.join("fixture/fixture.splash")).unwrap().split(&format!("asset(\"{song}\", \"")).nth(1).and_then(|rest| rest.split('"').next()).unwrap().to_string();
    std::fs::write(dir.join("fixture/lyric_edit.splash"), format!(
        "use mod.flow.*\nuse mod.cut.*\nlet song = AudioClip{{src: asset(\"{song}\", \"{rev_a}\") span: [1.0, 3.0] in: 0.5}}\nlet g = Graphic{{src: asset(\"{id}\", \"{rev}\") span: [0.0, 4.0]}}\nlet st = Stack{{layers: [g.out(@frame)]}}\nlet out = CutOutput{{frame: st.out(@frame) audio: [conn(1, song.out(@audio))]}}\nFlow{{\n    kind: @cut\n    output: {{width: 640 height: 360 fps: 30}}\n    duration: 4.0\n    bpm: 120\n    song, g, st, out\n}}\n"
    )).unwrap();
    let out = run(&["render", "--edit", &s("fixture/lyric_edit.splash"), "--media-file", &s("fixture/lyric_media.txt"), "--out", &s("l.mp4"), "--hashes", &s("l.txt")]);
    assert!(out.contains("done 120 frames 640x360"), "{out}");
    let h = hashes(&dir.join("l.txt"));
    // Edit 1.7 s is frame 51: all ghost before, the first word lit from it.
    assert!(h[..51].iter().all(|x| *x == h[0]), "ghost until the first sung word");
    assert_ne!(h[51], h[50], "the first word lights when it is sung");
    // Each word starts and ends on its sung time: track 1.2–1.5, 1.6–1.9,
    // 2.0–2.4 s are edit frames 51–60, 63–72, 75–87.
    let changes: Vec<usize> = (1..h.len()).filter(|&n| h[n] != h[n - 1]).collect();
    assert_eq!(changes, vec![51, 60, 63, 72, 75, 87], "word changes");
    let preview = run(&["preview", "--edit", &s("fixture/lyric_edit.splash"), "--media-file", &s("fixture/lyric_media.txt"),
        "--at", "@52,@10,@64,@90", "--hashes", &s("l.txt")]);
    assert_eq!(preview.matches("= render frame").count(), 8, "{preview}");
}

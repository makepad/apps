//! Motion frames on the real GPU, through the frame tool in one hidden
//! window: every case is a job of one batch (each with a renderer of its
//! own), and the check reads their reports and frames.
//!
//! The display frame: the fixtures give the same frames twice and move,
//! effects and every bundled font draw, an installed external font draws in
//! its own face, a plate stays under its text, a mask reveal at rest is the
//! plain text, anchored HUDs follow the camera, a layer a pass minifies has
//! mips, an animated weight never drops a letter, and a Shader reads its
//! data exactly, and an editor's selection box and a click's pick find the
//! layer drawn there. The HDR frame (`color: @hdr`): colours show as authored,
//! what is emissive blooms, motion blur smears, data layers, the render kit,
//! Scene3D post stages, the film kits, fills over one and fused passes.
//! A frame read as data (`--plain`) draws the document's exact pixels, and
//! its shader, widget and post pass are refused and tiled, never run.
//!
//! A harness=false test: it runs in the default test runs, frames at the
//! smallest size that still shows each property.

use makepad_motion_draw::frame_tool::{set_batch, App};
use makepad_widgets::*;
use std::path::{Path, PathBuf};

mod entry {
    use super::*;
    app_main!(App);

    pub fn on_app_thread() {
        app_main();
    }
}

/// The batch being built: its jobs' command lines, each case's check.
struct Batch {
    out: PathBuf,
    jobs: Vec<Vec<String>>,
    cases: Vec<(&'static str, Check)>,
}

/// A case's check over the reports: each failure says which property broke.
type Check = Box<dyn FnOnce(&Reports, &mut Vec<String>) + Send>;

impl Batch {
    /// Write `source` as the document `name` and give its path.
    fn doc(&self, name: &str, source: &str) -> String {
        let path = self.out.join(format!("{name}.splash"));
        std::fs::write(&path, source).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn job(&mut self, args: Vec<String>) -> usize {
        self.jobs.push(args);
        self.jobs.len() - 1
    }

    /// `frames` of the document at `path` at `at`, `width` pixels wide.
    fn frames(&mut self, name: &str, path: &str, at: &str, width: u32, extra: &[&str]) -> usize {
        let dir = self.out.join(name).to_string_lossy().into_owned();
        let mut args: Vec<String> = vec!["frames".into(), path.into(), "--at".into(), at.into(), "--width".into(), width.to_string(), "--out".into(), dir];
        args.extend(extra.iter().map(|s| s.to_string()));
        self.job(args)
    }

    /// `frames` of `source` (written as `name`).
    fn source(&mut self, name: &str, source: &str, at: &str, width: u32) -> usize {
        let path = self.doc(name, source);
        self.frames(name, &path, at, width, &[])
    }

    fn case(&mut self, name: &'static str, check: impl FnOnce(&Reports, &mut Vec<String>) + Send + 'static) {
        self.cases.push((name, Box::new(check)));
    }
}

/// The batch's reports, by job.
struct Reports(Vec<String>);

impl Reports {
    fn text(&self, job: usize) -> &str {
        &self.0[job]
    }

    fn hashes(&self, job: usize) -> Vec<String> {
        self.0[job].lines().filter(|l| l.starts_with("frame ")).filter_map(|l| l.split_whitespace().nth(2)).map(str::to_string).collect()
    }

    fn files(&self, job: usize) -> Vec<PathBuf> {
        self.0[job].lines().filter(|l| l.starts_with("frame ")).filter_map(|l| l.split_whitespace().nth(3)).map(PathBuf::from).collect()
    }

    /// The job's `k`th frame, decoded.
    fn image(&self, job: usize, k: usize) -> Image {
        decode(&self.files(job)[k])
    }

    /// The report's `error` lines.
    fn errors(&self, job: usize) -> Vec<&str> {
        self.0[job].lines().filter(|l| l.starts_with("error ")).collect()
    }
}

/// A frame: width, height, channels per pixel and its 8-bit values.
struct Image {
    w: usize,
    h: usize,
    ch: usize,
    px: Vec<u8>,
}

impl Image {
    /// The colour at a fraction of the frame.
    fn at(&self, fx: f32, fy: f32) -> [u8; 3] {
        let (x, y) = (((fx * self.w as f32) as usize).min(self.w - 1), ((fy * self.h as f32) as usize).min(self.h - 1));
        let i = (y * self.w + x) * self.ch;
        [self.px[i], self.px[i + 1], self.px[i + 2]]
    }

    /// The columns with bright ink in rows `rows`: [first, last].
    fn ink_columns(&self, rows: std::ops::Range<usize>) -> Option<[usize; 2]> {
        let columns: Vec<usize> = rows.flat_map(|y| (0..self.w).filter(move |&x| self.px[(y * self.w + x) * self.ch] > 128)).collect();
        Some([*columns.iter().min()?, *columns.iter().max()?])
    }
}

fn decode(path: &Path) -> Image {
    use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
    use makepad_zune_png::makepad_zune_core::options::DecoderOptions;
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut decoder = makepad_zune_png::PngDecoder::new_with_options(ZCursor::new(&bytes), DecoderOptions::default().png_set_strip_to_8bit(true));
    let px = decoder.decode_raw().unwrap();
    let (w, h) = decoder.dimensions().unwrap();
    Image { ch: px.len() / (w * h), w, h, px }
}

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../motion/fixtures").join(format!("{name}.splash")).to_string_lossy().into_owned()
}

fn main() {
    let start = std::time::Instant::now();
    std::env::set_var("MAKEPAD_HIDE_WINDOWS", "1");
    // GPU time per pass name, for the case that counts a pass's paints.
    std::env::set_var("MAKEPAD_MOTION_GPU", "1");
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("motion_pixels_{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let mut batch = Batch { out: out.clone(), jobs: Vec::new(), cases: Vec::new() };
    display_cases(&mut batch);
    hdr_cases(&mut batch);
    plain_cases(&mut batch);
    let Batch { jobs, cases, .. } = batch;
    set_batch(jobs, move |reports| {
        let reports = Reports(reports.to_vec());
        let count = cases.len();
        let mut failed = Vec::new();
        for (name, check) in cases {
            let mut broke = Vec::new();
            check(&reports, &mut broke);
            failed.extend(broke.into_iter().map(|b| format!("{name}: {b}")));
        }
        if !failed.is_empty() {
            return Err(failed.join("\n"));
        }
        let _ = std::fs::remove_dir_all(&out);
        Ok(format!("pixels: {count} cases in {:.1} s", start.elapsed().as_secs_f64()))
    });
    makepad_widgets::makepad_platform::run_app(entry::on_app_thread);
}

fn display_cases(b: &mut Batch) {
    // Two renderers give the same frames, which move where the document
    // animates.
    for name in ["kinetic_title", "logo_reveal", "lower_third"] {
        let times = "0.2,0.6,1.0,1.4,2.0,3.0,4.6";
        let (one, two) = (b.frames(&format!("{name}_a"), &fixture(name), times, 320, &[]), b.frames(&format!("{name}_b"), &fixture(name), times, 320, &[]));
        b.case(name, move |r, f| {
            let (ha, hb) = (r.hashes(one), r.hashes(two));
            if ha.len() != 7 {
                f.push(format!("{} frames drawn: {}", ha.len(), r.text(one)));
                return;
            }
            if ha != hb {
                f.push("two renderers differ".into());
            }
            let distinct: std::collections::HashSet<_> = ha.iter().collect();
            if distinct.len() < 5 {
                f.push(format!("does not move: only {} distinct frames", distinct.len()));
            }
            if !r.errors(one).is_empty() {
                f.push(r.errors(one).join("; "));
            }
        });
    }

    let strip_png = b.out.join("strip.png");
    let strip = b.job(vec!["strip".into(), fixture("logo_reveal"), "--count".into(), "8".into(), "--width".into(), "240".into(), "--out".into(), strip_png.to_string_lossy().into_owned()]);
    b.case("a contact sheet", move |r, f| {
        if !r.text(strip).contains("strip 8 frames 960x480") {
            f.push(format!("not the 4x2 sheet of 240 px frames: {}", r.text(strip)));
        }
        if std::fs::metadata(&strip_png).map_or(0, |m| m.len()) < 1000 {
            f.push("no sheet written".into());
        }
    });

    let effects = b.doc("effects", r#"use mod.motion.*; Motion{
    size: vec2(320, 180) duration: 2.0 background: #000000
    layers: [
        Rect{at: vec2(20, 20) size: vec2(100, 60) fill: #ff0000}
        Rect{at: vec2(180, 20) size: vec2(100, 60) fill: #00ff00 feather: 16}
        Ellipse{at: vec2(40, 100) size: vec2(60, 60) stroke: #ffffff width: 6 trim_end: 0.5}
        Text{text: "Hi" size: 40 font: Font{family: "Inter" weight: 800} at: vec2(240, 110) align: @center color: #0000ff
             effects: [Effect{name: @glow radius: 12}]}
    ]
}"#);
    let (one, two) = (b.frames("effects_a", &effects, "1", 320, &[]), b.frames("effects_b", &effects, "1", 320, &[]));
    b.case("blur, feather, trims and a glowing text", move |r, f| {
        if r.hashes(one).len() != 1 || r.hashes(one) != r.hashes(two) {
            f.push(format!("two renderers differ: {:?} {:?}", r.hashes(one), r.hashes(two)));
        }
        if !r.errors(one).is_empty() {
            f.push(r.errors(one).join("; "));
        }
    });

    // Every font a Text can name draws ink, wherever the crates live: one
    // frame, a cell per bundled family.
    let fonts: Vec<&'static str> = makepad_doc_fonts::bundled().collect();
    let (cols, cell) = (4usize, (320.0, 160.0));
    let rows = fonts.len().div_ceil(cols);
    let layers: Vec<String> = fonts.iter().enumerate().map(|(i, font)| {
        let (x, y) = ((i % cols) as f64 * cell.0 + cell.0 / 2.0, (i / cols) as f64 * cell.1 + cell.1 / 2.0);
        format!("Text{{text: \"Xg\" font: Font{{family: \"{font}\"}} size: 90 color: #ffffff at: vec2({x}, {y}) align: @center}}")
    }).collect();
    let all_fonts = b.source("fonts", &format!("use mod.motion.*; Motion{{size: vec2({}, {}) duration: 1.0 background: #000000 layers: [{}]}}", cols as f64 * cell.0, rows as f64 * cell.1, layers.join(" ")), "0.5", 640);
    b.case("every bundled font", move |r, f| {
        if r.text(all_fonts).contains("empty font family") {
            f.push(format!("a family resolves to no face: {}", r.text(all_fonts)));
        }
        let img = r.image(all_fonts, 0);
        let (cw, chh) = (img.w / cols, img.h / rows);
        for (i, font) in fonts.iter().enumerate() {
            let (cx, cy) = ((i % cols) * cw, (i / cols) * chh);
            let ink = (cy..cy + chh).flat_map(|y| (cx..cx + cw).map(move |x| (x, y))).filter(|&(x, y)| img.px[(y * img.w + x) * img.ch] > 128).count();
            if ink < 100 {
                f.push(format!("{font} draws no ink ({ink} bright pixels)"));
            }
        }
    });

    // An external font (Stage's Resources panel) draws in its own face in a
    // Text and a Draw2D `c.text` once its files are on the machine; one that
    // is not draws in the default face, the very picture of the document
    // naming Inter. Installed here: Anton (bundled, condensed) where
    // Archivo's files go under MAKEPAD_FONTS_DIR. (The notice a missing font
    // gives is motion's external_fonts test.)
    let fonts_dir = b.out.join("fonts");
    std::fs::create_dir_all(fonts_dir.join("archivo")).unwrap();
    let anton = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fonts/resources/fonts/anton/Anton-Regular.ttf");
    for file in ["Archivo-Variable.ttf", "Archivo-Italic-Variable.ttf"] {
        std::fs::copy(&anton, fonts_dir.join("archivo").join(file)).unwrap();
    }
    std::env::set_var("MAKEPAD_FONTS_DIR", &fonts_dir);
    let in_font = |font: &str| format!("use mod.motion.*; Motion{{size: vec2(640, 640) duration: 1.0 background: #000000 layers: [
        Text{{text: \"MOVE\" font: Font{{family: \"{font}\"}} size: 120 color: #ffffff at: vec2(40, 100)}}
        Draw2D{{draw: fn(c, t, edit) {{ c.text(\"MOVE\", 40, 480, {{px: 160 font: Font{{family: \"{font}\"}} color: #ffffff}}) }}}}]}}");
    let inter = b.source("font_inter", &in_font("Inter"), "0.5", 320);
    let own = b.source("font_own", &in_font("Archivo"), "0.5", 320);
    let missing = b.source("font_missing", &in_font("Montserrat"), "0.5", 320);
    b.case("an external font", move |r, f| {
        if r.hashes(own).is_empty() || r.hashes(own) == r.hashes(inter) {
            f.push("installed, it does not draw in its own face".into());
        }
        if r.hashes(missing) != r.hashes(inter) {
            f.push("not installed, it does not draw exactly the default face".into());
        }
        let (a, b) = (r.image(own, 0), r.image(inter, 0));
        for (what, rows) in [("Text", 0..b.h / 2), ("Draw2D", b.h / 2..b.h)] {
            match (a.ink_columns(rows.clone()), b.ink_columns(rows)) {
                // Anton is condensed: the same word is far narrower than Inter's.
                (Some(a), Some(b)) if (a[1] - a[0]) * 10 < (b[1] - b[0]) * 8 => {}
                (a, b) => f.push(format!("{what}: not the installed face's own advances: {a:?} vs Inter {b:?}")),
            }
        }
    });

    // A Text's plate is drawn under its glyphs, not over them.
    let plate = |color: &str| format!("use mod.motion.*; Motion{{size: vec2(640, 360) duration: 1.0 background: #101010 layers: [Text{{text: \"Plate\" size: 64 font: Font{{family: \"Inter\" weight: 800}} color: {color} at: vec2(80, 120) plate: #ff5a36}}]}}");
    let (words, none) = (b.source("plate_words", &plate("#ffffff"), "0.5", 160), b.source("plate_none", &plate("#ffffff00"), "0.5", 160));
    b.case("a plate", move |r, f| {
        if r.hashes(words).is_empty() || r.hashes(words) == r.hashes(none) {
            f.push("the words do not show on their plate".into());
        }
    });

    // A line-mask reveal at rest draws exactly the plain text: its matte
    // covers every glyph's ink (descenders, ascenders, the full stop) and
    // its lines sit where the text engine sets them ("evervwhere" clipped,
    // two-line headlines colliding).
    let texts = |anim: &str| {
        let layers = [
            ("everywhere, quickly.", 100, ""),
            ("Folders that\\nfill themselves.", 330, " line_height: 1.0 tracking: -3"),
            ("Folders that fill themselves by the rules you set", 640, " wrap: 900 line_height: 1.0"),
        ]
        .map(|(text, y, extra)| format!("Text{{text: \"{text}\" font: Font{{family: \"Inter\" weight: 800}} size: 112 color: #16181d at: vec2(192, {y}){extra}{anim}}}"));
        format!("use mod.motion.*; Motion{{size: vec2(1920, 1200) duration: 3.0 background: #f4f1ea layers: [{}]}}", layers.join(" "))
    };
    let plain = b.source("mask_plain", &texts(""), "2.9", 960);
    let revealed: Vec<(&'static str, usize)> = ["mask_up", "mask_down", "rise"].into_iter().map(|anim| (anim, b.source(&format!("mask_{anim}"), &texts(&format!(" animate: @{anim} start: 0.0")), "2.9", 960))).collect();
    b.case("a mask reveal at rest", move |r, f| {
        let base = r.image(plain, 0);
        for (anim, job) in revealed {
            let img = r.image(job, 0);
            // The isolated group's compositing may round a level; a clipped
            // glyph or a moved line differs by far more.
            let off = base.px.iter().zip(&img.px).filter(|(x, y)| x.abs_diff(**y) > 4).count();
            if base.px.len() != img.px.len() || off > 0 {
                f.push(format!("@{anim} differs from the plain text in {off} channel values"));
            }
        }
    });

    // 2D layers anchored to 3D nodes: the same pictures twice, and the HUD
    // moves with the camera's flight.
    let anchors = b.doc("anchors", r#"use mod.motion.*; use mod.scene; Motion{size: vec2(640, 360) duration: 2.0 background: #0b0b0d layers: [
        Scene3D{name: "set" size: vec2(640, 360)
            camera: Camera{shots: [scene.CameraShot{at: 0 hold: 0.2 position: vec3(0, 0.5, 6) target: vec3(0, 0, 0)} scene.CameraShot{at: 1.8 position: vec3(4, 1.5, 3) target: vec3(1, 0, 0)}]}
            children: [scene.Mesh{name: "eye" geometry: scene.SphereGeometry{} scale: vec3(0.4, 0.4, 0.4) position: vec3(1.5, 0.3, 0)} scene.DirectionalLight{position: vec3(2, 4, 3) intensity: 3}]}
        Rect{at: anchor("set", "eye") x: -30 y: -30 size: vec2(60, 60) stroke: #ff5a1f width: 3}]}"#);
    let (one, two) = (b.frames("anchors_a", &anchors, "0.1,1.0,1.9", 320, &[]), b.frames("anchors_b", &anchors, "0.1,1.0,1.9", 320, &[]));
    b.case("anchored HUDs", move |r, f| {
        let (ha, hb) = (r.hashes(one), r.hashes(two));
        if ha.len() != 3 || ha != hb {
            f.push(format!("two renderers differ: {ha:?} {hb:?}"));
        } else if ha[0] == ha[2] {
            f.push("the HUD does not move with the flight".into());
        }
    });

    // The batch's first use of this generator preset takes the cold load path.
    let preset = b.source("cold_scene_preset", r#"use mod.motion.*; use mod.scene;
        Motion{size: vec2(64, 64) duration: 1.0 background: #000000 layers: [
            Scene3D{name: "cold_preset" size: vec2(64, 64) environment: @hdri_three_point
                camera: Camera{position: vec3(0, 0, 4) target: vec3(0, 0, 0) fov: 38}
                children: [scene.Mesh{geometry: scene.SphereGeometry{radius: 0.6} material: scene.MeshBasicMaterial{color: #ff0000}}]}]}"#, "0", 64);
    b.case("a scene's first preset bake", move |r, f| {
        for (operation, count) in makepad_core_util::ui_thread::violations() {
            if operation == "a generator preset's environment bake" {
                f.push(format!("{operation} ran on the UI thread {count} time(s)"));
            }
        }
        if r.files(preset).len() != 1 || !r.errors(preset).is_empty() {
            f.push(format!("expected one rendered scene frame: {}", r.text(preset)));
            return;
        }
        let center = r.image(preset, 0).at(0.5, 0.5);
        if center[0] < 150 || center[1] > 100 || center[2] > 100 {
            f.push(format!("the red sphere should cover the black backdrop at the center, got {center:?}"));
        }
    });

    // A point light by a mirror floor: the floor's reflection draws the
    // scene again from the mirrored camera, as a view of the scene's draw.
    // The sphere above the mirror is lit as without it, its image in the
    // mirror is lit by the lamp too, and a plate under the lamp shades the
    // floor below it as deeply with the mirror as without. The camera looks
    // at the sphere's centre; its lit side faces the light, up and to the
    // right.
    let lamp = |intensity: &str| format!("scene.PointLight{{position: vec3(1.5, 1.8, 2) color: #ffffff intensity: {intensity} distance: 8 cast_shadow: true}}");
    let lit = |reflectivity: &str, lamp: &str| format!("use mod.motion.*; use mod.scene; Motion{{size: vec2(320, 180) duration: 1.0 background: #000000 layers: [
        Scene3D{{size: vec2(320, 180) background: #000000
            camera: Camera{{position: vec3(0, 1.2, 5) target: vec3(0, 0.8, 0) fov: 40}}
            lights: [scene.AmbientLight{{intensity: 0.02}} {lamp}]
            children: [scene.Mesh{{geometry: scene.SphereGeometry{{radius: 0.6}} position: vec3(0, 0.8, 0) material: scene.MeshStandardMaterial{{color: #c8c8c8 roughness: 0.5}}}}
                scene.Mesh{{geometry: scene.BoxGeometry{{width: 0.4 height: 0.05 depth: 0.4}} position: vec3(1.5, 1.0, 2) material: scene.MeshStandardMaterial{{color: #c8c8c8 roughness: 0.5}}}}
                scene.Ground{{y: 0 reflectivity: {reflectivity}}}]}}]}}");
    // The mirror's 0.5 s is drawn first, then after a seek to 0.1 s.
    let mirrored = b.source("lamp_mirror", &lit("0.6", &lamp("25")), "0.5,0.1,0.5", 320);
    let no_lamp = b.doc("no_lamp", &lit("0.6", ""));
    let first = b.frames("no_lamp_first", &no_lamp, "0.5", 320, &[]);
    let plain = b.source("lamp_plain", &lit("0", &lamp("25")), "0.5", 320);
    let unlit = b.source("lamp_unlit", &lit("0.6", &lamp("0")), "0.5", 320);
    let later = b.frames("no_lamp_later", &no_lamp, "0.5", 320, &[]);
    b.case("a point light by a mirror", move |r, f| {
        // the mean luminance of a 5x5 block round a fraction of the frame
        let lum = |img: &Image, fx: f32, fy: f32| {
            let (cx, cy) = ((fx * img.w as f32) as i32, (fy * img.h as f32) as i32);
            let px = |x: i32, y: i32| { let c = img.at((x as f32 + 0.5) / img.w as f32, (y as f32 + 0.5) / img.h as f32); 0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32 };
            (-2..=2).flat_map(|dy| (-2..=2).map(move |dx| (dx, dy))).map(|(dx, dy)| px(cx + dx, cy + dy)).sum::<f32>() / 25.0
        };
        let (m, p, u) = (r.image(mirrored, 0), r.image(plain, 0), r.image(unlit, 0));
        let (lit_plain, dark_plain, lit_mirror) = (lum(&p, 0.555, 0.435), lum(&p, 0.445, 0.565), lum(&m, 0.555, 0.435));
        if lit_plain < dark_plain + 40.0 {
            f.push(format!("the point light does not light the sphere's near side: {lit_plain:.0} vs {dark_plain:.0} on the far side"));
        }
        if (lit_mirror - lit_plain).abs() > 6.0 {
            f.push(format!("the mirror changes how the point light lights the sphere above it: {lit_mirror:.0} with the mirror, {lit_plain:.0} without"));
        }
        // the sphere's lower near side, lit by the lamp, in the mirror below it
        let (image_lit, image_unlit) = (lum(&m, 0.55, 0.92), lum(&u, 0.55, 0.92));
        if image_lit < image_unlit + 40.0 {
            f.push(format!("the point light does not light the sphere's image in the mirror: {image_lit:.0}, and {image_unlit:.0} with the light off"));
        }
        // the floor in the plate's shadow, against the lit floor beside it
        let depth = |img: &Image| 1.0 - lum(img, 0.83, 0.92) / lum(img, 0.67, 0.92).max(1.0);
        let (depth_plain, depth_mirror) = (depth(&p), depth(&m));
        if depth_plain < 0.75 {
            f.push(format!("the plate casts no shadow from the point light: the floor below it is {:.0}% darker than beside it", depth_plain * 100.0));
        }
        if (depth_mirror - depth_plain).abs() > 0.1 {
            f.push(format!("the mirror changes the plate's shadow: {:.0}% darker with the mirror, {:.0}% without", depth_mirror * 100.0, depth_plain * 100.0));
        }
        let frames = r.hashes(mirrored);
        if frames.len() != 3 || frames[0] != frames[2] {
            f.push(format!("0.5 s after a seek to 0.1 s is not 0.5 s drawn first: {frames:?}"));
        }
    });
    b.case("a mirror floor after shadowed frames", move |r, f| {
        let base = r.image(later, 0);
        for (job, why) in [
            (first, "a frame drawn while a material warms was read back as final"),
            (unlit, "a point light at intensity 0 changed the frame"),
        ] {
            let img = r.image(job, 0);
            if (img.w, img.h, img.ch) != (base.w, base.h, base.ch) {
                f.push(format!("{why}: frame dimensions or channel count differ"));
                continue;
            }
            let n = img.px.iter().zip(&base.px).filter(|(x, y)| x.abs_diff(**y) > 2).count();
            if n != 0 {
                f.push(format!("{why}: {n} values differ"));
            }
        }
    });

    // A Scene3D's 2D layer a post pass samples with `sample_grad` carries a
    // mip chain: one-pixel stripes read over an eight-pixel footprint are an
    // even grey, while a plain `sample` of the layer (level 0) shows every
    // stripe.
    let mips = |read: &str| format!(
        "use mod.motion.*; use mod.post; let s = Scene3D{{size: vec2(320, 180) background: #000000 children: []
            layers: {{stripes: Draw2D{{size: vec2(320, 180) draw: fn(c, t, edit) {{ for i in 0..160 {{ c.rect(i * 2.0, 0.0, 1.0, 180.0, #ffffff, 1.0) }} }}}}}}
            post: [post.Pass{{reads: [\"stripes\"] pixel: fn() -> vec4 {{ let uv = self.uv() {read} }}}}]}}
        Motion{{size: vec2(320, 180) duration: 1.0 layers: [s]}}");
    let minified = b.source("mips_grad", &mips("return self.stripes.sample_grad(uv, vec2(8.0 / 320.0, 0.0), vec2(0.0, 8.0 / 180.0))"), "0.5", 320);
    let level0 = b.source("mips_plain", &mips("return self.stripes.sample(uv)"), "0.5", 320);
    b.case("a minified layer", move |r, f| {
        let row = |job: usize| {
            let img = r.image(job, 0);
            // the middle row's green channel over the middle half
            (img.w / 4..img.w * 3 / 4).map(|x| img.px[(img.h / 2 * img.w + x) * img.ch + 1] as i32).collect::<Vec<_>>()
        };
        let spread = |r: &[i32]| r.iter().max().unwrap() - r.iter().min().unwrap();
        let (minified, plain) = (row(minified), row(level0));
        if spread(&plain) <= 90 {
            f.push(format!("level 0 does not show the stripes: {plain:?}"));
        }
        if spread(&minified) >= 24 || minified[0] <= 50 {
            f.push(format!("the mips do not average the stripes to grey: {minified:?}"));
        }
    });

    // A weight travelling through the letters, drawn as a playing preview
    // shows it (`--realtime`: each frame painted once, exact or not), 30
    // consecutive frames: no letter vanishes for a frame. A new weight is a
    // new font instance whose curves arrive a moment later; the nearest
    // weight's stand in until they do.
    let times: Vec<String> = (0..30).map(|k| format!("{:.5}", 1.0 + k as f64 / 60.0)).collect();
    let wave = b.frames("weight_wave", &fixture("weight_wave"), &times.join(","), 1280, &["--realtime"]);
    b.case("an animated weight", move |r, f| {
        let files = r.files(wave);
        if files.len() != 30 {
            f.push(format!("{} frames drawn", files.len()));
            return;
        }
        // Ink per 16-pixel column band over the text's rows (60..200 of 240).
        let bands = 1280 / 16;
        let ink: Vec<Vec<u32>> = files.iter().map(|file| {
            let img = decode(file);
            let mut b = vec![0u32; bands];
            for y in 60..200 {
                for x in 0..img.w {
                    let at = (y * img.w + x) * img.ch;
                    b[x / 16] += (img.px[at] as u32 + img.px[at + 1] as u32 + img.px[at + 2] as u32) / 3;
                }
            }
            b
        }).collect();
        let floor = *ink.iter().flatten().min().unwrap();
        for i in 1..ink.len() - 1 {
            for band in 0..bands {
                let (p, c, n) = (ink[i - 1][band] - floor, ink[i][band] - floor, ink[i + 1][band] - floor);
                // a band with letters in the frames before and after, and (almost) none in this one
                if p.min(n) > 4000 && c * 3 < p.min(n) {
                    f.push(format!("letters vanished in frame {i} at x {}..{}: {p} {c} {n}", band * 16, band * 16 + 16));
                }
            }
        }
    });

    // A Shader layer reads its `data` list exactly: floats and texels read
    // with `self.data_float(i)` / `self.data_vec4(i)` across a list longer
    // than one texture row (5000 floats, 1250 texels, rows of 1024) draw the
    // same pixels as the same values computed in the shader.
    let n = 5000;
    let values: Vec<String> = (0..n).map(|k| format!("{}", (k % 256) as f64 / 256.0)).collect();
    let read = format!("fn() -> vec4 {{ let i = floor(self.pos.x * {n}.0) let v = self.data_float(i) let q = self.data_vec4(floor(i / 4.0)) return vec4(v, q.y, q.w, 1.0) }}");
    let by_hand = format!("fn() -> vec4 {{ let i = floor(self.pos.x * {n}.0) let t = floor(i / 4.0) * 4.0 let v = (i - 256.0 * floor(i / 256.0)) / 256.0 let y = (t + 1.0 - 256.0 * floor((t + 1.0) / 256.0)) / 256.0 let w = (t + 3.0 - 256.0 * floor((t + 3.0) / 256.0)) / 256.0 return vec4(v, y, w, 1.0) }}");
    let data = b.source("shader_data", &format!("use mod.motion.*\nMotion{{size: vec2(1280, 64) duration: 1.0 background: #000000 layers: [\n    Shader{{at: vec2(0, 0) size: vec2(640, 64) data: [{}] pixel: {read}}}\n    Shader{{at: vec2(640, 0) size: vec2(640, 64) pixel: {by_hand}}}\n]}}", values.join(", ")), "0.5", 1280);
    b.case("a Shader's data", move |r, f| {
        let img = r.image(data, 0);
        let mut seen = std::collections::HashSet::new();
        for y in (0..img.h).step_by(7) {
            for x in 0..img.w / 2 {
                let (a, c) = ((y * img.w + x) * img.ch, (y * img.w + x + img.w / 2) * img.ch);
                if img.px[a..a + 3] != img.px[c..c + 3] {
                    f.push(format!("pixel ({x}, {y}): the data reads {:?}, the values computed {:?}", &img.px[a..a + 3], &img.px[c..c + 3]));
                    return;
                }
                seen.insert(img.px[a]);
            }
        }
        if seen.len() <= 200 {
            f.push(format!("the data's values do not show ({} levels)", seen.len()));
        }
    });

    // An editor's selection box of a layer is what the layer draws:
    // `MotionDraw::item_bounds` (the `--bounds` lines) of a wrapped,
    // tracked, multi-line Text, and of one turned and scaled, holds all of
    // its ink and hugs it, with no more room than the line pitch leaves above
    // and below the glyphs. Drawn 640 wide, a pixel is a document unit.
    const SIZE: f64 = 48.0;
    let selections: [(&'static str, &str, usize, f64); 3] = [
        ("a wrapped, tracked text's selection", "Text{text: \"A MARKER ON EVERY BEAT\" font: Font{family: \"Inter\" weight: 800} size: 48 tracking: 6 wrap: 420 line_height: 1.05 color: #ffffff at: vec2(60, 40)}", 3, 0.0),
        ("a three-line text's selection", "Text{text: \"A MARKER\\nON EVERY\\nBEAT\" size: 48 tracking: 4 color: #ffffff at: vec2(320, 40) align: @center}", 3, 0.0),
        // Turned 8 degrees: its box's corners stand out by about sin 8 of its size.
        ("a turned, scaled text's selection", "Text{text: \"TURNED\\nAND SCALED\" size: 48 tracking: 2 color: #ffffff at: vec2(120, 90) scale: 1.2 rotate: 8}", 2, 30.0),
    ];
    for (k, (what, layer, lines, turn)) in selections.into_iter().enumerate() {
        let doc = b.doc(&format!("select_{k}"), &format!("use mod.motion.*; Motion{{size: vec2(640, 640) duration: 1.0 background: #000000 layers: [{layer}]}}"));
        let job = b.frames(&format!("select_{k}"), &doc, "0.5", 640, &["--bounds"]);
        b.case(what, move |r, f| {
            let Some(bounds) = r.text(job).lines().filter(|l| l.starts_with("bounds ")).last() else {
                f.push(format!("no bounds: {}", r.text(job)));
                return;
            };
            let v: Vec<f64> = bounds.split_whitespace().skip(3).filter_map(|n| n.parse().ok()).collect();
            let img = r.image(job, 0);
            // The ink's box: every pixel brighter than the dark background, and its rows of ink.
            let (mut ink, mut runs, mut was_ink) = ([f64::MAX, f64::MAX, 0.0f64, 0.0f64], 0, false);
            for y in 0..img.h {
                let mut row_ink = false;
                for x in 0..img.w {
                    let at = (y * img.w + x) * img.ch;
                    if img.px[at].max(img.px[at + 1]).max(img.px[at + 2]) > 64 {
                        ink = [ink[0].min(x as f64), ink[1].min(y as f64), ink[2].max(x as f64 + 1.0), ink[3].max(y as f64 + 1.0)];
                        row_ink = true;
                    }
                }
                runs += (row_ink && !was_ink) as usize;
                was_ink = row_ink;
            }
            if runs < lines {
                f.push(format!("{runs} lines of ink, want {lines}"));
            }
            // All the ink is inside the box (a pixel of antialiasing either side)...
            if !(ink[0] >= v[0] - 1.5 && ink[1] >= v[1] - 1.5 && ink[2] <= v[2] + 1.5 && ink[3] <= v[3] + 1.5) {
                f.push(format!("ink {ink:?} outside the selection {v:?}"));
            }
            // ...and the box hugs it: side bearings and the line pitch's room only.
            let slack = [ink[0] - v[0], ink[1] - v[1], v[2] - ink[2], v[3] - ink[3]];
            if slack[0] > 0.2 * SIZE + turn || slack[2] > 0.5 * SIZE + turn {
                f.push(format!("loose sides {slack:?} (ink {ink:?}, selection {v:?})"));
            }
            if slack[1] > 0.5 * SIZE + turn || slack[3] > 0.6 * SIZE + turn {
                f.push(format!("loose top or bottom {slack:?} (ink {ink:?}, selection {v:?})"));
            }
        });
    }

    // A click selects the layer drawn under it, and naming the layers costs
    // nothing. Six shapes batch into one vector draw (two of them overlap;
    // boolean shapes, so the vector path, not the plain rect and ellipse
    // quads) that the Text after them flushes; once it named every shape as
    // the Text. Drawn with pick ids and without: the same draw calls and the
    // same colour frame (the shapes stay one draw); each place names the item
    // drawn on top there, by its path, and the ring's hole names nothing; the
    // frame drawn after the pick is the frame before it.
    let shapes = b.doc("pick", r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 background: #000000 layers: [
        Shape{name: "ring" op: @difference at: vec2(20, 60) fill: #ff7a1a layers: [Ellipse{size: vec2(240, 240)} Ellipse{at: vec2(60, 60) size: vec2(120, 120)}]}
        Shape{op: @union at: vec2(300, 20) fill: #e04040 layers: [Rect{size: vec2(100, 100)}]}
        Shape{op: @union at: vec2(350, 70) fill: #4060e0 layers: [Rect{size: vec2(100, 100)}]}
        Shape{op: @union at: vec2(500, 20) fill: #40c060 layers: [Ellipse{size: vec2(100, 100)}]}
        Shape{op: @union at: vec2(480, 150) fill: #e0c040 layers: [Rect{size: vec2(140, 60)}]}
        Shape{op: @union at: vec2(300, 200) fill: #c040c0 layers: [Ellipse{size: vec2(100, 100)}]}
        Text{text: "HHHHH" size: 96 font: Font{family: "Inter" weight: 900} color: #ffffff at: vec2(430, 240)}
    ]}"#);
    // Each place and the paths it names (a pick reads ids within 2 pixels of it).
    let places: [(&'static str, &'static [&'static str]); 9] = [
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
    let picks: Vec<&str> = places.iter().flat_map(|(at, _)| ["--pick", *at]).collect();
    let (off, on) = (b.frames("pick_off", &shapes, "0.5", 160, &[]), b.frames("pick_on", &shapes, "0.5", 160, &picks));
    b.case("a click's pick", move |r, f| {
        let (off, on) = (r.text(off), r.text(on));
        let line = |text: &str, prefix: &str| text.lines().find(|l| l.starts_with(prefix)).unwrap_or_default().to_string();
        let word = |text: &str, prefix: &str, n: usize| line(text, prefix).split_whitespace().nth(n).unwrap_or_default().to_string();
        if line(on, "draw calls ").is_empty() || line(on, "draw calls ") != line(off, "draw calls ") {
            f.push(format!("pick ids split the shapes' draw: {:?} vs {:?}", line(on, "draw calls "), line(off, "draw calls ")));
        }
        if word(on, "frame ", 2).is_empty() || word(on, "frame ", 2) != word(off, "frame ", 2) {
            f.push("pick ids change the colour frame".into());
        }
        if word(on, "after the pick: frame ", 5) != word(on, "frame ", 2) {
            f.push(format!("the frame drawn after the pick is not the frame before it:\n{on}"));
        }
        // Each place's report: its `pick at` line, then a `pick id N: <path> op ...` line per id.
        let lines: Vec<&str> = on.lines().collect();
        for (at, want) in places {
            let Some(from) = lines.iter().position(|l| l.starts_with(&format!("pick at {at} "))) else {
                f.push(format!("no pick at {at}:\n{on}"));
                continue;
            };
            let named: Vec<&str> = lines[from + 1..].iter().take_while(|l| l.starts_with("pick id ")).filter_map(|l| l.split_once(": ").and_then(|(_, rest)| rest.split(" op ").next())).collect();
            if named != want {
                f.push(format!("the pick at {at} names {named:?}, not the item drawn on top there {want:?}"));
            }
        }
    });
}

/// An HDR document 1280 x 720, drawn 640 wide unless a property needs more.
fn hdr(head: &str, layers: &str) -> String {
    format!("use mod.motion.*; use mod.post; use mod.scene; Motion{{size: vec2(1280, 720) duration: 1.0 {head} layers: [{layers}]}}")
}

fn hdr_cases(b: &mut Batch) {
    const W: u32 = 640;
    let near = |a: [u8; 3], b: [u8; 3], by: i32| (0..3).all(|k| (a[k] as i32 - b[k] as i32).abs() <= by);
    let body = |stroke: &str| format!(r#"Rect{{size: vec2(1280, 720) fill: #101018}}
        Rect{{size: vec2(200, 200) fill: #3080ff at: vec2(100, 100)}}
        Text{{text: "TYPE" size: 120 at: vec2(500, 80) color: #ffffff}}
        Path{{svg: "M 100 500 L 1180 500" stroke: {stroke} width: 8}}
        Rect{{size: vec2(60, 200) fill: #ffffff at: tween(vec2(100, 540), vec2(1100, 540), 0, 1)}}"#);
    let display = b.source("hdr_display", &hdr("background: #101018", &body("#ff5a1f")), "0.5", W);
    let linear = b.source("hdr_linear", &hdr("background: #101018 color: @hdr", &body("#ff5a1f")), "0.5", W);
    let glow = b.source("hdr_glow", &hdr("background: #101018 color: @hdr post: [post.Glow{strength: 1.2 threshold: 1}]", &body("emit(#ff5a1f, 4)")), "0.5", W);
    b.case("an HDR frame", move |r, f| {
        let (display, linear, glow) = (r.image(display, 0), r.image(linear, 0), r.image(glow, 0));
        // A flat colour: the same picture in linear light, tone mapped.
        let (a, c) = (display.at(0.12, 0.2), linear.at(0.12, 0.2));
        if !near(a, c, 1) {
            f.push(format!("a flat colour is not as authored: {a:?} vs display {c:?}"));
        }
        // Glow: an emissive line (4x) gets a halo; white type (1.0) does not.
        let (halo, plain) = (glow.at(0.3, 500.0 / 720.0 - 0.03), linear.at(0.3, 500.0 / 720.0 - 0.03));
        if halo[0] as i32 <= plain[0] as i32 + 20 {
            f.push(format!("no halo above the emissive line: {halo:?} vs {plain:?}"));
        }
        let (ty, ty_plain) = (glow.at(0.45, 0.07), linear.at(0.45, 0.07));
        if !near(ty, ty_plain, 2) {
            f.push(format!("white type glows: {ty:?} vs {ty_plain:?}"));
        }
    });

    // The bar moves 1000 units a second: 360 degrees at 60 fps smear it by
    // ~17 units, 12 pixels at 960 wide; count partly covered pixels along
    // its row.
    let still = b.source("blur_still", &hdr("background: #101018 color: @hdr", &body("#ff5a1f")), "0.5", 960);
    let blurred = b.source("blur_smeared", &hdr("background: #101018 color: @hdr motion_blur: post.MotionBlur{shutter: 360}", &body("#ff5a1f")), "0.5", 960);
    b.case("whole-frame motion blur", move |r, f| {
        let row = |img: &Image| (0..img.w).filter(|&x| {
            let c = img.at(x as f32 / img.w as f32, 640.0 / 720.0);
            c[0] > 40 && c[0] < 220
        }).count();
        let (a, c) = (row(&r.image(still, 0)), row(&r.image(blurred, 0)));
        if c <= a + 8 {
            f.push(format!("the fast edge is not smeared: {c} partly covered pixels blurred vs {a} still"));
        }
    });

    // 0.5 written into a data layer reaches the pass as 0.5 (not decoded to
    // 0.21): the frame shows linear 0.5, sRGB 188. Without the pass the data
    // layer does not draw.
    let field = r#"Rect{name: "field" data_layer: true size: vec2(1280, 720) fill: rgba(0.5, 0.0, 0.0, 1.0)}"#;
    let read = b.source("data_read", &hdr(r#"color: @hdr background: #000000 post: [post.Pass{reads: [@color, "field"] pixel: fn() -> vec4 { let f = self.field.sample(self.uv()) return vec4(f.x, f.x, f.x, 1.0) }}]"#, field), "0.5", W);
    let unread = b.source("data_unread", &hdr("color: @hdr background: #000000", field), "0.5", W);
    b.case("a data layer", move |r, f| {
        let c = r.image(read, 0).at(0.5, 0.5);
        if (c[0] as i32 - 188).abs() > 3 || c[1] != c[0] {
            f.push(format!("the pass does not read the value as written: {c:?}"));
        }
        let plain = r.image(unread, 0).at(0.5, 0.5);
        if plain[0] >= 5 {
            f.push(format!("unread, the data layer draws: {plain:?}"));
        }
    });

    // A blurred emissive square (4x white): the render kit's blur runs in
    // half float in an HDR frame, so its halo is brighter than a plain white
    // square's instead of both clamping at 1.
    let square = |fill: &str| hdr("color: @hdr background: #000000", &format!("Rect{{size: vec2(200, 200) at: vec2(540, 260) fill: {fill} blur: 30}}"));
    let (bright, white) = (b.source("kit_emit", &square("emit(#ffffff, 4)"), "0.5", W), b.source("kit_plain", &square("#ffffff"), "0.5", W));
    b.case("the render kit's blur", move |r, f| {
        let (a, c) = (r.image(bright, 0).at(500.0 / 1280.0, 0.5), r.image(white, 0).at(500.0 / 1280.0, 0.5));
        if a[0] as i32 <= c[0] as i32 + 40 {
            f.push(format!("light over one clamps: halo {a:?} vs {c:?}"));
        }
    });

    // A Scene3D re-attaches its passes each paint; the post graph's stages
    // (Glow at @hdr, Vignette at @final) keep their order through it.
    let staged = b.source("scene3d_post", &hdr("color: @hdr background: #0a0a0b post: [post.Glow{strength: 0.8}, post.Vignette{}]", r#"Scene3D{name: "set" size: vec2(1280, 720)
            camera: Camera{position: vec3(0, 0.5, 4) target: vec3(0, 0, 0) fov: 38}
            lights: [scene.AmbientLight{}]
            children: [scene.Mesh{name: "ball" geometry: scene.SphereGeometry{radius: 0.6} material: scene.MeshBasicMaterial{color: #ff4d12}}]}"#), "0.5", W);
    b.case("a Scene3D under two post stages", move |r, f| {
        let c = r.image(staged, 0).at(0.5, 0.5);
        if c[0] <= 150 || c[2] >= 120 {
            f.push(format!("the ball does not show: {c:?}"));
        }
    });

    // A Scene3D pass reading the scene's 2D `layers:` picture: the picture
    // paints before the chain samples it, so a preview frame paints once
    // (it painted twice when the picture hung under the container).
    let once = b.doc("layer_once", r#"use mod.motion.*; use mod.scene; use mod.post; Motion{size: vec2(1280, 720) duration: 1.0 color: @hdr background: #000000
        layers: [Scene3D{name: "s" size: vec2(1280, 720)
            camera: Camera{position: vec3(0, 0, 10) target: vec3(0, 0, 0) fov: 40}
            layers: {atlas: Group{layers: [Text{text: "HI" size: 100 at: vec2(50, 50) color: #ff0000}]}}
            children: [scene.Mesh{geometry: scene.PlaneGeometry{width: 3 height: 3} material: scene.MeshBasicMaterial{color: vec4(0.5, 0.5, 0.5, 1.0)}}]
            post: [post.Pass{reads: [@color, "atlas"] uniforms: {k: fn(t) { vec4(t, 0.0, 0.0, 1.0) }}
                pixel: fn() -> vec4 { let c = self.color.sample(self.uv()) + self.atlas.sample(self.uv()) return vec4(c.xyz + self.k.x * 0.01, 1.0) }}]}]}"#);
    let bench = b.job(vec!["bench".into(), once, "--realtime".into(), "--width".into(), "240".into()]);
    b.case("a scene reading its 2D layer", move |r, f| {
        let text = r.text(bench);
        // "gpu per frame by pass (ms, N frames):" then "  ms  samples  name".
        let frames: Option<usize> = text.split("gpu per frame by pass (ms, ").nth(1).and_then(|s| s.split(' ').next()).and_then(|n| n.parse().ok());
        let pass = text.lines().find(|l| l.trim_end().ends_with("graph Scene3D.post[0] Pass")).and_then(|l| l.split_whitespace().nth(2)).and_then(|n| n.parse::<usize>().ok());
        match (frames, pass) {
            // (A few first frames paint again while their glyphs and shaders land.)
            (Some(frames), Some(pass)) if pass * 2 <= frames * 3 => {}
            _ => f.push(format!("the scene's pass paints more than once a frame:\n{text}")),
        }
    });

    // The film kits do what they say. (Grain is measured per pixel at 960
    // wide: a smaller frame averages the grain of neighbouring pixels away.)
    let film = |name: &str, layers: &str, post: &str, width: u32| (name.to_string(), hdr(&format!("color: @hdr background: #000000 post: [{post}]"), layers), width);
    let square = "Rect{size: vec2(40, 40) at: vec2(620, 340) fill: emit(#ffffff, 6)}";
    let jobs: Vec<usize> = [
        film("film_plain", square, "", W),
        film("film_bloom", square, "post.Bloom{strength: 0.7 threshold: 0.82 halation: 0.25}", W),
        film("film_shoulder", "Rect{size: vec2(1280, 720) fill: #f3f3f3}", "post.Shoulder{}", W),
        film("film_vignette", "Rect{size: vec2(1280, 720) fill: #ffffff}", "post.Vignette{mode: 1}", W),
        film("film_grain", "Rect{size: vec2(640, 720) fill: #000000} Rect{size: vec2(640, 720) at: vec2(640, 0) fill: #767676}", "post.Grain{amount: 0.1}", 960),
    ]
    .iter()
    .map(|(name, doc, width)| b.source(name, doc, "0.5", *width))
    .collect();
    b.case("the film post kits", move |r, f| {
        let [plain, bloom, shoulder, vignette, grain] = [0, 1, 2, 3, 4].map(|k| r.image(jobs[k], 0));
        // Bloom: a small hot square spreads far (the 1/128 level), and its
        // halation is red-orange.
        let (far, far_plain) = (bloom.at(800.0 / 1280.0, 0.5), plain.at(800.0 / 1280.0, 0.5));
        if far[0] as i32 <= far_plain[0] as i32 + 6 || far[0] <= far[2] {
            f.push(format!("Bloom: no warm halo 160 px out: {far:?} vs {far_plain:?}"));
        }
        // Shoulder: linear 0.896 (#f3f3f3) rolls off to 0.853 (sRGB 238).
        let s = shoulder.at(0.5, 0.5);
        if !(236..=240).contains(&s[0]) {
            f.push(format!("Shoulder: {s:?}"));
        }
        // Vignette in linear light: a white frame's corner falls to 0.795
        // (sRGB 231), its centre stays white.
        let (corner, centre) = (vignette.at(0.001, 0.001), vignette.at(0.5, 0.5));
        if !(227..=235).contains(&corner[0]) || centre[0] < 253 {
            f.push(format!("Vignette: corner {corner:?} centre {centre:?}"));
        }
        // Grain: strongest in the mid-tones, and the blacks still have some.
        let spread = |fx: f32| {
            let vals: Vec<f32> = (0..200).map(|k| grain.at(fx + k as f32 * 0.0005, 0.5)[1] as f32).collect();
            let m = vals.iter().sum::<f32>() / vals.len() as f32;
            (vals.iter().map(|v| (v - m) * (v - m)).sum::<f32>() / vals.len() as f32).sqrt()
        };
        let (black, mid) = (spread(0.1), spread(0.7));
        if black <= 1.5 || mid <= black * 1.5 {
            f.push(format!("Grain: spread black {black:.2} mid {mid:.2}"));
        }
    });

    // Plain fills in an HDR frame, read back through a pass that scales the
    // linear light by 1/4: emit() (written and from fn(t)), a raw colour
    // over 1, and an additive fill at half opacity (it used to clip
    // everything under it).
    let fills = b.source("shape_hdr", &hdr(r#"color: @hdr background: #000000
        post: [post.Pass{at: @hdr reads: [@color] pixel: fn() -> vec4 { let c = self.color.sample(self.uv()) return vec4(c.xyz * 0.25, 1.0) }}]"#, r#"
            Rect{size: vec2(200, 200) at: vec2(40, 260) fill: emit(#ff4d12, 4.0)}
            Rect{size: vec2(200, 200) at: vec2(280, 260) fill: fn(t) { emit(#ff4d12, 4.0) }}
            Rect{size: vec2(200, 200) at: vec2(520, 260) fill: fn(t) { {color: [2.0, 2.0, 2.0, 1.0]} }}
            Rect{size: vec2(200, 200) at: vec2(760, 260) fill: emit(#ffffff, 3.0) blend: @add opacity: 0.5}
            Ellipse{size: vec2(200, 200) at: vec2(1000, 260) fill: emit(#ffffff, 2.0)}
            Rect{size: vec2(200, 120) at: vec2(40, 560) fill: rgba(3.0, 0.0, 0.0, 1.0)}
            Draw2D{name: "d2" draw: fn(c, t, edit) { c.rect(280, 560, 200, 120, emit(#ffffff, 2.0)) }}"#), "0.5", W);
    b.case("fills over one", move |r, f| {
        let img = r.image(fills, 0);
        let lin = |v: u8| { let c = v as f64 / 255.0; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } };
        for (x, y, want, what) in [(140.0, 360.0, 4.0, "emit"), (380.0, 360.0, 4.0, "fn emit"), (620.0, 360.0, 2.0, "raw 2"), (860.0, 360.0, 1.5, "additive 3 x 0.5"), (1100.0, 360.0, 2.0, "ellipse emit 2"), (140.0, 620.0, 3.0, "rgba(3, 0, 0)"), (380.0, 620.0, 2.0, "Draw2D rect emit 2")] {
            let got = lin(img.at(x / 1280.0, y / 720.0)[0]) * 4.0;
            if (got - want).abs() >= want * 0.06 {
                f.push(format!("{what}: red {got:.3}, want {want}"));
            }
        }
    });

    // Shoulder and a linear Vignette run as one fused pass (both map kits);
    // the same two written as ordinary passes run apart, with a half-float
    // frame between them. They agree to 2 levels on under 0.1% of pixels.
    let scene = r#"Rect{size: vec2(1280, 720) fill: #20242c}
        Rect{size: vec2(500, 300) at: vec2(390, 210) fill: emit(#ff4d12, 3.0)}
        Text{text: "FUSE" size: 160 at: vec2(420, 260) color: #efe8dc font: Font{family: "Inter" weight: 900}}"#;
    let shoulder = r#"post.Pass{at: @hdr reads: [@color] pixel: fn() -> vec4 { let c = self.color.sample(self.uv()) let a = clamp(c.w, 0.0, 1.0) if a < 0.0001 { return c } let x = max(c.xyz / a, vec3(0.0, 0.0, 0.0)) let k = 0.72 let r = vec3(k, k, k) + (1.0 - k) * (vec3(1.0, 1.0, 1.0) - exp((vec3(k, k, k) - x) / (1.0 - k))) var y = mix(x, r, step(vec3(k, k, k), x)) let m = max(x.x, max(x.y, x.z)) y = mix(y, vec3(1.0, 1.0, 1.0), smoothstep(2.0, 12.0, m) * 0.85) return vec4(y * a, c.w) }}"#;
    let vignette = r#"post.Pass{at: @hdr reads: [@color] pixel: fn() -> vec4 { let c = self.color.sample(self.uv()) let dc = (self.uv() - vec2(0.5, 0.5)) * vec2(1.0, 0.8) let v = mix(1.0, smoothstep(0.95, 0.25, length(dc)), 0.35) return vec4(c.xyz * v, c.w) }}"#;
    let fused = b.source("fused", &hdr("color: @hdr background: #000000 post: [post.Shoulder{} post.Vignette{mode: 1}]", scene), "0.5", W);
    let apart = b.source("apart", &hdr(&format!("color: @hdr background: #000000 post: [{shoulder} {vignette}]"), scene), "0.5", W);
    b.case("fused post passes", move |r, f| {
        let (a, c) = (r.image(fused, 0), r.image(apart, 0));
        let (mut over1, mut worst) = (0usize, 0i32);
        let n = a.px.len() / a.ch;
        for (pa, pb) in a.px.chunks(a.ch).zip(c.px.chunks(c.ch)) {
            let d = (0..3).map(|k| (pa[k] as i32 - pb[k] as i32).abs()).max().unwrap();
            worst = worst.max(d);
            if d > 1 {
                over1 += 1;
            }
        }
        if worst > 2 || over1 as f64 >= n as f64 * 0.001 {
            f.push(format!("fused and apart differ: worst {worst}, {over1} of {n} pixels over 1 level"));
        }
    });
}

fn plain_cases(b: &mut Batch) {
    // A frame read as data (`--plain`) draws exactly what the document
    // draws, and logs nothing.
    let picture = b.doc("plain_picture", r#"use mod.motion.*; Motion{
        size: vec2(64, 32) duration: 1 background: #000000 layers: [
            Rect{size: vec2(4, 4) fill: #ff0000 x: keys([0, 0], [0.5, 12], [1, 18])}
            Ellipse{at: vec2(24, 2) size: vec2(8, 8) fill: #00ff00}
            Path{svg: "M 1 20 L 12 25 L 1 30 Z" fill: #ff8800}
            Text{text: "A" at: vec2(40, 8) size: 12 color: #ffffff}
            Group{at: vec2(16, 16) mask: Ellipse{size: vec2(16, 12) fill: #ffffff}
                layers: [Rect{size: vec2(20, 12) fill: #0088ff opacity: tween(0.2, 1, 0, 1)}]}
        ]
    }"#);
    let times = "0,0.5,0.75";
    let (normal, data) = (b.frames("plain_normal", &picture, times, 64, &[]), b.frames("plain_data", &picture, times, 64, &["--plain"]));
    b.case("a frame read as data", move |r, f| {
        if !r.text(data).lines().any(|line| line == "drawing a peer data frame") {
            f.push("the pixel job did not draw the peer reader's PlainFrame".into());
        }
        if !r.errors(data).is_empty() {
            f.push(format!("a data frame reported a refusal or an error: {}", r.errors(data).join("; ")));
        }
        let distinct: std::collections::HashSet<_> = r.hashes(normal).into_iter().collect();
        if distinct.len() != 3 {
            f.push(format!("the keys and the tween give {} distinct reference frames, not 3: {}", distinct.len(), r.text(normal)));
            return;
        }
        let (want, got) = (r.files(normal), r.files(data));
        if got.len() != want.len() {
            f.push(format!("{} data frames drawn: {}", got.len(), r.text(data)));
            return;
        }
        for (k, (want, got)) in want.iter().zip(&got).enumerate() {
            if std::fs::read(want).ok() != std::fs::read(got).ok() {
                f.push(format!("the data frame {k} differs from the document's PNG"));
            }
        }
    });

    // Its shader, widget and post pass never run: each is refused once by
    // item path over two frames, and drawn as a grey tile in its place.
    let refused = b.doc("plain_refused", r#"use mod.motion.*; use mod.post; Motion{
        size: vec2(64, 24) duration: 1 background: #000000 layers: [
            Shader{at: vec2(2, 6) size: vec2(12, 12) input: @below pixel: fn() { return vec4(1.0, 0.0, 1.0, 1.0) }}
            Group{post: [post.Pass{reads: [@color] pixel: fn() -> vec4 { return vec4(1.0, 0.0, 0.0, 1.0) }}] layers: [
                Widget{at: vec2(18, 6) size: vec2(12, 12) ui: fn(props) { View{} }}
                Rect{at: vec2(34, 6) size: vec2(12, 12) fill: #00ff00}
            ]}
            Rect{at: vec2(50, 6) size: vec2(12, 12) fill: #808080 opacity: 0.25}
        ]
    }"#);
    let job = b.frames("plain_refused", &refused, "0,0.5", 64, &["--plain"]);
    b.case("a data frame's code is refused", move |r, f| {
        let refusals: Vec<_> = r.text(job).lines().filter(|line| line.contains("is not drawn")).collect();
        if refusals.len() != 3 {
            f.push(format!("{} refusals over two frames, not 3: {}", refusals.len(), r.text(job)));
        }
        for (path, reason) in [("layers[0]", ": a shader"), ("layers[1].layers[0]", ": a widget"), ("layers[1]", " with its post pass: a post pass")] {
            let message = format!("motion: {path} is not drawn{reason} runs code, and this picture was read as data");
            if refusals.iter().filter(|line| line.contains(&message)).count() != 1 {
                f.push(format!("not one refusal for {path}"));
            }
        }
        let frames = r.files(job).len();
        if frames != 2 {
            f.push(format!("{frames} frames drawn: {}", r.text(job)));
            return;
        }
        for k in 0..2 {
            let img = r.image(job, k);
            let at = |x: usize| {
                let i = (12 * img.w + x) * img.ch;
                [img.px[i], img.px[i + 1], img.px[i + 2], if img.ch == 4 { img.px[i + 3] } else { 255 }]
            };
            let grey = at(56);
            if !(grey[0] > 0 && grey[0] == grey[1] && grey[1] == grey[2]) {
                f.push(format!("frame {k}: the reference grey rectangle is missing: {grey:?}"));
                continue;
            }
            for (left, kind) in [(2, "shader"), (18, "widget")] {
                let center = at(left + 6);
                if !center.iter().zip(grey).all(|(a, b)| a.abs_diff(b) <= 1) {
                    f.push(format!("frame {k}: the {kind} tile is {center:?}, not the 25% grey {grey:?}"));
                }
                if !(at(left)[0] < center[0] && at(left + 1) == center) {
                    f.push(format!("frame {k}: the {kind} tile has no one-pixel darker outline"));
                }
            }
            if at(40) != [0, 255, 0, 255] {
                f.push(format!("frame {k}: the post group's green child is {:?}, not its own colour", at(40)));
            }
            if at(0) != [0, 0, 0, 255] {
                f.push(format!("frame {k}: a refused pass painted outside the item boxes: {:?}", at(0)));
            }
        }
    });
}

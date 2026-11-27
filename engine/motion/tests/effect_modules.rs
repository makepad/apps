//! The effects library's Draw2D modules (camera2d, sparks, print) record
//! what the hand-written code they stand for records, imported as a
//! document imports them.

use makepad_motion::plain::PrimitiveFrame;
use makepad_motion::draw2d::Draw2dList;
use makepad_motion::frame::ItemKind;
use makepad_motion::*;

fn list_at(draw: &str, t: f64) -> Draw2dList {
    let doc = MotionDoc::load(&format!("use mod.motion.*; use mod.motion.modules.camera2d.*\nuse mod.motion.modules.sparks.*\nuse mod.motion.modules.print.*\nMotion{{duration: 2.0 layers: [Draw2D{{draw: fn(c, t, edit) {{ {draw} }}}}]}}")).unwrap_or_else(|d| panic!("{d:?}"));
    let frame = doc.eval(t);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    let ItemKind::Draw2d { picture, .. } = &frame.items[0].kind else { panic!("not a Draw2D item") };
    (*picture.list).clone()
}

fn same(kit: &str, by_hand: &str, t: f64) {
    let (a, b) = (list_at(kit, t), list_at(by_hand, t));
    assert!(!a.segs.is_empty() || !a.ops.is_empty(), "the kit drew nothing");
    assert_eq!(format!("{:?}", a.segs), format!("{:?}", b.segs));
    assert_eq!(format!("{:?}", a.ops), format!("{:?}", b.ops));
}

#[test]
fn cam2d_maps_both_ways_and_draws_through_its_transform() {
    let cam = "let cam = {x: 300.0 y: -120.0 zoom: 1.7 rot: 0.3}";
    // A stroke in world units under the camera lands where cam2d_to_screen puts its points.
    let under = list_at(&format!("{cam} cam2d_apply(c, cam) c.stroke([[10, 20], [60, -5]], 2, #ffffff, 1)"), 0.5);
    let mapped = list_at(&format!("{cam} let a = cam2d_to_screen(cam, vec2(10, 20)) let b = cam2d_to_screen(cam, vec2(60, -5))
        c.seg_rgb(a.x, a.y, b.x, b.y, 2 * 1.7, 1, 1, 1, 1)"), 0.5);
    let (u, m) = (under.segs[0], mapped.segs[0]);
    let near = |p: [f32; 2], q: [f32; 2]| (p[0] - q[0]).abs() < 1e-2 && (p[1] - q[1]).abs() < 1e-2;
    assert!(near(u.a, m.a) && near(u.b, m.b) && (u.width - m.width).abs() < 1e-3, "{u:?} vs {m:?}");
    // to_world undoes to_screen; mix blends zoom geometrically.
    let l = list_at(&format!("{cam} let p = cam2d_to_world(cam, cam2d_to_screen(cam, vec2(12.5, 80.0)))
        let m = cam2d_mix(cam, {{x: 0.0 y: 0.0 zoom: 0.425 rot: 0.0}}, 0.5)
        c.seg_rgb(p.x, p.y, m.zoom, m.x, 1, 1, 1, 1, 1)"), 0.0);
    let s = l.segs[0];
    assert!((s.a[0] - 12.5).abs() < 1e-3 && (s.a[1] - 80.0).abs() < 1e-3, "round trip {s:?}");
    assert!((s.b[0] - 0.85).abs() < 1e-5 && (s.b[1] - 150.0).abs() < 1e-3, "zoom blends geometrically {s:?}");
}

#[test]
fn spark_head_is_the_hand_written_head() {
    same(
        "spark_head(c, 400, 300, t, {scale: 0.8 heat: 1.2})",
        "let x = 400.0 let y = 300.0 let scale = 0.8
         let k = 1.2 * (0.85 + 0.15 * sin(t * 91.7) * sin(t * 57.3))
         c.seg_rgb(x, y, x + 0.01, y, 26.0 * scale, 1.0 * 0.5 * k, 0.074214 * 0.5 * k, 0.006049 * 0.5 * k, 0.35)
         c.seg_rgb(x, y, x + 0.01, y, 12.0 * scale, 1.0 * 2.5 * k, 0.254152 * 2.5 * k, 0.046665 * 2.5 * k, 0.8)
         c.seg_rgb(x, y, x + 0.01, y, 5.0 * scale, 6.0 * k, 5.0 * k, 4.0 * k, 1.0)
         for i in 0..4 {
             let a = i * (6.283185307179586 / 4.0) + t * 3.0 + 0.4
             let r = (9.0 + 5.0 * hashn(floor(t * 30.0), i)) * scale
             c.seg_rgb(x, y, x + cos(a) * r, y + sin(a) * r, 1.2 * scale, 3.0 * k, 1.2 * k, 0.4 * k, 0.8)
         }",
        0.73,
    );
}

#[test]
fn spark_particles_trail_a_moving_head() {
    let head = "let head = fn(tb) { if tb < 0.2 { return nil } return vec2(100.0 + tb * 300.0, 200.0) }";
    let a = list_at(&format!("{head} spark_particles(c, t, head, {{rate: 90 seed: 3}})"), 0.8);
    // Births in the last `life` seconds, none before the head exists, none past their own life.
    assert!(a.segs.len() > 10 && a.segs.len() <= 42, "{}", a.segs.len());
    let thinned = list_at(&format!("{head} spark_particles(c, t, head, {{rate: fn(tb) {{ 30.0 }} rate_max: 90 seed: 3}})"), 0.8);
    assert!(thinned.segs.len() < a.segs.len(), "rate(tb) thins births: {} vs {}", thinned.segs.len(), a.segs.len());
    assert!(list_at(&format!("{head} spark_particles(c, t, head, {{}})"), 0.1).segs.is_empty(), "no head, no sparks");
}

/// spark_particles throws exactly the sparks the film's hand-written
/// shower threw (the Sydney smile's: rate 110, life 0.35, speed 200).
#[test]
fn spark_particles_are_the_hand_written_shower() {
    let head = "let head = fn(tb) { if tb < 0.1 { return nil } return vec2(300.0 + tb * 500.0, 400.0 - tb * 120.0) }";
    same(
        &format!("{head} spark_particles(c, t, head, {{rate: 110 life: 0.35 speed: 200 heat: 0.9 seed: 5}})"),
        &format!("{head}
         let life = 0.35 let speed = 200.0 let g = 520.0 let I = 0.9 let seed = 5.0 let rate = 110.0
         var n = floor((t - life) * rate)
         while n <= floor(t * rate) {{
             let tb = n / rate
             let h = head(tb)
             if tb <= t && h != nil {{
                 let age = t - tb
                 let a = hashn(n, seed) * 6.283185307179586
                 let hh = hashn(n, seed + 1.0)
                 let sp = speed * (0.25 + hh * hh * 1.2)
                 let lf = life * (0.35 + 0.65 * hashn(n, seed + 2.0))
                 if age <= lf {{
                     let vx = cos(a) * sp
                     let vy = sin(a) * sp - speed * 0.3
                     let a0 = max(0.0, age - 0.018)
                     let k = 1.0 - age / lf
                     let heat = k * k
                     c.seg_rgb(h.x + vx * a0, h.y + vy * a0 + 0.5 * g * a0 * a0, h.x + vx * age, h.y + vy * age + 0.5 * g * age * age, 1.6 * (0.5 + k * 0.7),
                         (1.0 + (1.0 - 1.0) * heat) * 2.2 * I, (0.074214 + (0.8 - 0.074214) * heat) * 2.2 * I, (0.006049 + (0.5 - 0.006049) * heat) * 2.2 * I, min(1.0, k * 1.4))
                 }}
             }}
             n = n + 1.0
         }}"),
        0.93,
    );
}

#[test]
fn crop_marks_are_the_four_corners() {
    same(
        "crop_marks(c, {inset: 30})",
        "let m = 30.0 let l = 22.0
         for q in [[m, m, 1.0, 1.0], [1920.0 - m, m, -1.0, 1.0], [m, 1080.0 - m, 1.0, -1.0], [1920.0 - m, 1080.0 - m, -1.0, -1.0]] {
             c.stroke([[q[0] + q[2] * l, q[1] + 0.5 * q[3]], [q[0], q[1] + 0.5 * q[3]], [q[0], q[1] + q[3] * l]], 1.25, #eee9df, 0.34)
         }",
        0.0,
    );
}

#[test]
fn a_document_imports_a_pinned_library_module() {
    let modules = modules::Modules::from_sources([
        ("grain_lib", "r3", "use mod.motion.*; // A number module\nlet base = import(asset(\"base_lib\", \"r1\"))\nlet twice = fn(x) { x * 2.0 * base.k }\n{twice: twice}"),
        ("base_lib", "r1", "let k = 1.5\n{k: k}"),
    ]);
    let context = MotionContext { modules, ..Default::default() };
    let src = "use mod.motion.*; let g = import(asset(\"grain_lib\", \"r3\"))\nMotion{duration: 1.0 layers: [Rect{size: vec2(g.twice(10.0), 4) fill: #ffffff}]}";
    let doc = MotionDoc::load_with(src, &[], context).unwrap_or_else(|d| panic!("{d:?}"));
    let ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) = &doc.eval(0.0).items[0].kind else { panic!("not a rect") };
    assert_eq!(size[0], 30.0);
    // Without the host's module, the load says what to do.
    let missing = MotionDoc::load(src).unwrap_err();
    assert!(missing.iter().any(|d| d.message.contains("grain_lib@r3 was not found") && d.message.contains("--module")), "{missing:?}");
    assert_eq!(modules::asset_imports(src), vec![AssetRef { id: "grain_lib".into(), rev: "r3".into() }]);
}

/// A plate's folder import (`import("shared/…")`) resolved where there is
/// no folder (the plate loaded outside its tree) stops the load at that
/// import: one error naming it, never the document evaluated without its
/// modules (one error per name they would have given). In its folder the
/// same text loads.
#[test]
fn a_folder_import_without_its_folder_is_one_error() {
    let src = "use mod.motion.*\nuse mod.motion.modules.paths.*\nlet song = import(\"shared/song_timing.splash\")\nMotion{duration: 1.0 layers: [Rect{size: vec2(song.w, 4) fill: #ffffff}]}";
    let errors = MotionDoc::load(src).unwrap_err();
    assert_eq!(errors.len(), 1, "one error, not a cascade: {errors:?}");
    assert!(errors[0].message.contains("shared/song_timing.splash"), "the error names the import: {errors:?}");
    struct Folder;
    impl modules::ModuleResolver for Folder {
        fn path(&self, _from: &str, path: &str) -> Result<modules::ModuleSource, String> {
            (path == "shared/song_timing.splash").then(|| modules::ModuleSource::new(path, "1", "{w: 3.0}")).ok_or_else(|| format!("no {path} in the folder"))
        }
    }
    let context = MotionContext { modules: modules::Modules(std::sync::Arc::new(Folder)), ..Default::default() };
    let doc = MotionDoc::load_with(src, &[], context).unwrap_or_else(|d| panic!("in its folder the plate loads: {d:?}"));
    let ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) = &doc.eval(0.0).items[0].kind else { panic!("not a rect") };
    assert_eq!(size[0], 3.0, "the imported module's value");
}

#[test]
fn a_misspelt_library_module_is_named() {
    let e = MotionDoc::load("use mod.motion.*; use mod.motion.modules.spark.*\nMotion{duration: 1.0 layers: []}").unwrap_err();
    assert!(e.iter().any(|d| d.message.contains("Did you mean `mod.motion.modules.sparks`")), "{e:?}");
}

/// Every module in the library has a one-line description and exports what
/// its header shows; the kinetic showcase loads and records its example frames.
#[test]
fn the_library_modules_describe_themselves() {
    for (name, _, text) in modules::BUILTIN {
        let info = modules::module_info(text);
        assert!(!info.doc.is_empty() && !info.exports.is_empty(), "{name}: {info:?}");
        let doc = MotionDoc::load(&format!("use mod.motion.*; use mod.motion.modules.{name}.*\nMotion{{duration: 1.0 layers: [Rect{{size: vec2(4, 4) fill: #ffffff}}]}}")).unwrap_or_else(|d| panic!("{name}: {d:?}"));
        assert!(doc.warnings.is_empty(), "{name}: {:?}", doc.warnings);
    }
    for (name, source) in [("kinetic", include_str!("../showcase/kinetic.motion.splash"))] {
        let doc = MotionDoc::load(source).unwrap_or_else(|d| panic!("{name}: {d:?}"));
        assert!(doc.warnings.is_empty(), "{name}: {:?}", doc.warnings);
        for t in [0.0, 0.55, 1.35, 2.3, 3.0, 4.2, 4.4, 4.6, 5.0, 6.0, 7.05, 8.2, 8.5, 8.9, 9.6, 10.4, 11.2, 11.5, 12.0] {
            let frame = doc.eval(t);
            assert!(frame.errors.is_empty(), "{name} at {t}: {:?}", frame.errors);
        }
    }
}

/// A library checks a module before publishing it, as a document would load it.
#[test]
fn a_module_is_checked_before_it_is_published() {
    let good = "// Doubles\nlet twice = fn(x) { x * 2.0 }\n{twice: twice}";
    assert!(modules::is_module(good) && !modules::is_module("use mod.motion.*; Motion{duration: 1.0 layers: []}"));
    let info = modules::check(good, Default::default()).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!((info.doc.as_str(), info.exports.clone()), ("Doubles", vec!["twice".to_string()]));
    let broken = modules::check("let twice = fn(x) { x * }\n{twice: twice}", Default::default()).unwrap_err();
    assert!(broken.iter().any(|e| e.contains("module:module_under_check")), "{broken:?}");
    let not_exports = modules::check("let x = 3\nx", Default::default()).unwrap_err();
    assert!(not_exports.iter().any(|e| e.contains("object of its exports")), "{not_exports:?}");
}

/// The song module reads a song's timing data as the lyric film's own
/// timing code did: the beat grid (as mkit's beat_at), bars, drum hits,
/// lines by content and sung progress over syllables.
#[test]
fn song_timing_lookups() {
    let data = "{beats: [0.5, 1.0, 1.5, 2.0, 2.5] downbeats: [0.5, 2.5] sections: [{name: \"verse\" start: 0.0 end: 2.2}]
        onsets: {kick: [{time: 1.0 strength: 0.8}, {time: 1.5 strength: 0.5}]}
        lines: [{words: [{w: \"Our\" start: 1.0 end: 1.2}, {w: \"harbor\" start: 1.2 end: 1.8 syl: [{start: 1.2 end: 1.4}, {start: 1.4 end: 1.8}]}]}]}";
    let probe = |expr: &str| -> f64 {
        let doc = MotionDoc::load(&format!("use mod.motion.*; use mod.motion.modules.song.*\nlet S = {data}\nMotion{{duration: 1.0 layers: [Rect{{size: vec2({expr}, 1) fill: #ffffff}}]}}")).unwrap_or_else(|d| panic!("{expr}: {d:?}"));
        let ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) = &doc.eval(0.0).items[0].kind else { panic!("not a rect") };
        size[0]
    };
    for t in [0.2, 1.25, 1.99, 3.1] {
        assert_eq!(probe(&format!("100.0 + song_beat(S, {t})")), probe(&format!("100.0 + beat_at(S.beats, {t})")), "t {t}");
    }
    assert_eq!(probe("song_time_of_beat(S, 2.5)"), 1.75);
    assert_eq!(probe("song_bar(S, 1.5)"), 0.5);
    assert_eq!(probe("song_hit(S, \"kick\", 1.5, 0.11) * 100.0"), (0.8f64 * 0.5f64.powf(0.5 / 0.11)).max(0.5) * 100.0);
    assert_eq!(probe("line_start(song_line(S, \"harbor\", 0))"), 1.0);
    assert_eq!(probe("line_end(song_line(S, \"harbor\", 0)) * 10.0"), 18.0, "a line ends with its last word");
    assert_eq!(probe("line_text(song_line(S, \"harbor\", 0)).len()"), 10.0, "\"Our harbor\": its words joined");
    assert_eq!(probe("song_line_at(S, 1.5).words.len()"), 2.0);
    assert_eq!(probe("song_word_progress(song_line(S, \"harbor\", 0).words[1], 1.6) * 100.0"), 75.0, "the second syllable half sung");
    assert_eq!(probe("song_line_chars(song_line(S, \"harbor\", 0), 1.5)"), 4.0 + 6.0 * 0.625, "\"Our \" and 0.625 of \"harbor\"");
}

/// stepped_value is the lyric film's P(doom) instrument (its hand-written
/// value(t): roll, creep, jitter), noise1 its value noise.
#[test]
fn stepped_value_is_the_hand_written_instrument() {
    let by_hand = "let PD_T = [-1.0, 23.66, 60.02, 96.38, 125.46]
        let PD_V = [0.02, 0.15, 0.42, 0.81, 0.99]
        let o_clamp = fn(x) { if x < 0.0 { return 0.0 } if x > 1.0 { return 1.0 } return x }
        let o_prog = fn(x, a, b) { return o_clamp((x - a) / (b - a)) }
        let o_out_expo = fn(t) { if t >= 1.0 { return 1.0 } return 1.0 - pow(2.0, -10.0 * t) }
        let o_smoothstep = fn(a, b, x) { let t = o_clamp((x - a) / (b - a)) return t * t * (3.0 - 2.0 * t) }
        let o_fade = fn(t) { return t * t * t * (t * (t * 6.0 - 15.0) + 10.0) }
        let o_lerp = fn(a, b, t) { return a + (b - a) * t }
        let o_noise1 = fn(x, seed) { let i = floor(x) return o_lerp(hashn(i, seed) * 2.0 - 1.0, hashn(i + 1.0, seed) * 2.0 - 1.0, o_fade(x - i)) }
        let o_pdoom = fn(t) {
            var i = 0
            while i + 1 < 5 && PD_T[i + 1] <= t { i = i + 1 }
            let pi = max(0, i - 1)
            let k = if i == 0 { 1.0 } else { o_out_expo(o_prog(t, PD_T[i], PD_T[i] + 0.9)) }
            let base = PD_V[pi] + (PD_V[i] - PD_V[pi]) * k
            var creep = 0.0
            if i + 1 < 5 { creep = (PD_V[i + 1] - PD_V[i]) * 0.2 * o_smoothstep(PD_T[i] + 1.0, PD_T[i + 1], t) }
            let jitter = o_noise1(t * 3.1, 7.0) * 0.004 * (1.0 - k * 0.5)
            return o_clamp(base + creep + jitter)
        }";
    let value = |prelude: &str, expr: &str| -> f64 {
        let doc = MotionDoc::load(&format!("use mod.motion.*; use mod.motion.modules.numbers.*\n{prelude}\nMotion{{duration: 1.0 layers: [Rect{{size: vec2(1000.0 * {expr}, 1) fill: #ffffff}}]}}")).unwrap_or_else(|d| panic!("{expr}: {d:?}"));
        let ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) = &doc.eval(0.0).items[0].kind else { panic!("not a rect") };
        size[0]
    };
    let steps = "let STEPS = [[-1.0, 0.02], [23.66, 0.15], [60.02, 0.42], [96.38, 0.81], [125.46, 0.99]]";
    for t in [3.0, 23.7, 24.1, 40.0, 59.9, 60.5, 97.0, 130.0] {
        assert_eq!(value(steps, &format!("stepped_value(STEPS, {t}, {{}})")), value(by_hand, &format!("o_pdoom({t})")), "t {t}");
        assert_eq!(value("", &format!("noise1({t} * 3.1, 7.0)")), value(by_hand, &format!("o_noise1({t} * 3.1, 7.0)")), "noise t {t}");
    }
}

/// A Shader layer's `data: [f, …]` is read once into a shared list that
/// every frame's item holds (the same list, never copied), and a wrong
/// `data` is named: a non-number, a vj_fx effect, a param called `data`.
#[test]
fn shader_data_is_one_shared_list() {
    let values: Vec<String> = (0..5000).map(|k| format!("{}", (k % 256) as f64 / 256.0)).collect();
    let source = format!("use mod.motion.*\nMotion{{duration: 1.0 layers: [Shader{{data: [{}] pixel: fn() {{ return vec4(self.data_float(3.0), 0.0, 0.0, 1.0) }}}}]}}", values.join(", "));
    let doc = MotionDoc::load(&source).unwrap_or_else(|d| panic!("{d:?}"));
    let data = |t: f64| {
        let frame = doc.eval(t);
        let ItemKind::Shader { data: Some(data), .. } = &frame.items[0].kind else { panic!("no data on the shader item") };
        data.clone()
    };
    let (a, b) = (data(0.2), data(0.7));
    assert!(std::sync::Arc::ptr_eq(&a.0, &b.0), "two frames hold the same list");
    assert_eq!(a.0.len(), 5000);
    assert!(a.0.iter().enumerate().all(|(k, x)| *x == (k % 256) as f32 / 256.0));
    let none = MotionDoc::load("use mod.motion.*\nMotion{duration: 1.0 layers: [Shader{pixel: fn() { return vec4(1.0, 1.0, 1.0, 1.0) }}]}").unwrap_or_else(|d| panic!("{d:?}"));
    assert!(matches!(&none.eval(0.5).items[0].kind, ItemKind::Shader { data: None, .. }));
    let errors = |source: &str| match MotionDoc::load(&format!("use mod.motion.*\nMotion{{duration: 1.0 layers: [{source}]}}")) {
        Ok(doc) => doc.warnings.iter().map(|w| w.message.clone()).collect::<Vec<_>>().join("; "),
        Err(diags) => diags.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join("; "),
    };
    let bad = errors("Shader{data: [1.0, \"two\"] pixel: fn() { return vec4(1.0, 1.0, 1.0, 1.0) }}");
    assert!(bad.contains("`data` is a list of numbers; item 1"), "{bad}");
    let effect = errors("Shader{effect: \"vjfx/grain\" data: [1.0]}");
    assert!(effect.contains("a vj_fx effect does not take it"), "{effect}");
    let param = errors("Shader{params: {data: 1.0} pixel: fn() { return vec4(1.0, 1.0, 1.0, 1.0) }}");
    assert!(param.contains("`data` is already a shader uniform"), "{param}");
}

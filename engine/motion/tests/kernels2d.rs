//! Kernels in 2D documents: a document-level Kernel writes Segment records
//! and a Draw2D draws them with `c.segs(k.out)`, per frame at the layer's
//! time, exactly as the flat array the Splash loop would have built.
#![cfg(feature = "scene3d")]

use makepad_motion::draw2d::Draw2dList;
use makepad_motion::frame::ItemKind;
use makepad_motion::*;

fn segs(src: &str, t: f64) -> Draw2dList {
    let doc = MotionDoc::load(src).unwrap_or_else(|d| panic!("{d:?}"));
    let frame = doc.eval(t);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    let ItemKind::Draw2d { picture, .. } = &frame.items[0].kind else { panic!("not a Draw2D item") };
    (*picture.list).clone()
}

#[test]
fn a_kernels_segments_draw_like_the_loop_they_replace() {
    let kernel = r#"
use mod.motion.*; use mod.kernel; var PTS = []
for k in 0..5 { PTS.push(vec2(100.0 * k, 50.0)) }
let strokes = Kernel{
    count: 4
    pts: kernel.input(vec2, PTS)
    lift: kernel.param(10.0)
    out: kernel.output(kernel.Segment)
    element: fn(i) {
        out[i].a = vec3(pts[i].x, pts[i].y + time * lift, 0.0)
        out[i].b = vec3(pts[i + 1].x, pts[i + 1].y, 0.0)
        out[i].color = vec4(1.0, 0.5, 0.25, 0.75)
        out[i].width = 2.0 + float(i)
    }
}
Motion{duration: 2.0 layers: [Draw2D{draw: fn(c, t, edit) { c.segs(strokes.out) }}]}"#;
    let by_hand = r#"
use mod.motion.*; Motion{duration: 2.0 layers: [Draw2D{draw: fn(c, t, edit) {
    let data = []
    for i in 0..4 { data.push(100.0 * i) data.push(50.0 + t * 10.0) data.push(100.0 * (i + 1)) data.push(50.0) data.push(2.0 + i) data.push(1.0) data.push(0.5) data.push(0.25) data.push(0.75) }
    c.segs(data)
}}]}"#;
    let (a, b) = (segs(kernel, 0.5), segs(by_hand, 0.5));
    assert_eq!(a.segs.len(), 4);
    assert_eq!(format!("{:?}", a.segs), format!("{:?}", b.segs));
    // Per frame: at another time the kernel ran again.
    assert_ne!(format!("{:?}", segs(kernel, 1.5).segs), format!("{:?}", a.segs));
}

/// The strokes module's kernel draws each stroke on by length: a stroke half
/// way through its time shows its first half, glowing where the pen is.
#[test]
fn library_strokes_draw_on_by_length() {
    let src = r#"use mod.motion.*; use mod.motion.modules.strokes.*
let ART = strokes_data([
    {pts: [0.0, 0.0, 1.0, 0.0, 2.0, 0.0, 3.0, 0.0, 4.0, 0.0] t0: 0.0 t1: 1.0 ease: @linear width: 2.0 alpha: 1.0 color: #808080 glow: 0.0},
    {pts: [0.0, 1.0, 0.0, 2.0] t0: 5.0 t1: 6.0 ease: @linear}
])
Motion{duration: 2.0 layers: [Draw2D{draw: fn(c, t, edit) { draw_strokes(c, ART, {camera: {x: 0.0 y: 0.0 zoom: 100.0 rot: 0.0}}) }}]}"#;
    let l = segs(src, 0.5);
    // At t 0.5 the head is at length 2: two whole segments, the second stroke not started.
    assert_eq!(l.segs.len(), 2, "{:?}", l.segs);
    let s = l.segs[1];
    assert!((s.a[0] - 1060.0).abs() < 1e-3 && (s.b[0] - 1160.0).abs() < 1e-3 && (s.a[1] - 540.0).abs() < 1e-3, "{s:?}");
    assert!((s.width - 2.0).abs() < 1e-5 && (s.color[0] - 0.2158605).abs() < 1e-4 && (s.color[3] - 1.0).abs() < 1e-5, "{s:?}");
    // Part way into a segment, the last one is cut at the head.
    let l = segs(src, 0.625);
    assert_eq!(l.segs.len(), 3);
    assert!((l.segs[2].b[0] - 1210.0).abs() < 1e-2, "{:?}", l.segs[2]);
}

/// A kernel's hashn2/hashn3 and vnoise are the documents' hashn and vnoise
/// (24 bits of the same hash, the same lattice).
#[test]
fn kernel_hashn_and_vnoise_match_the_documents() {
    let src = r#"
use mod.motion.*; use mod.kernel; let probe = Kernel{
    count: 64
    out: kernel.output(kernel.Segment)
    element: fn(i) {
        let x = float(i) * 0.37 - 9.1
        let y = float(i) * 0.53 + 2.2
        // on screen (off-screen segments are culled): x, y and the width carry the three values
        let a = vec3(100.0 + 1000.0 * hashn3(float(i) - 20.0, 7.0, 3.0), 100.0 + 800.0 * hashn2(float(i) * 13.0, -4.0), 0.0)
        out[i].a = a
        out[i].b = a + vec3(1.0, 0.0, 0.0)
        out[i].color = vec4(1.0, 1.0, 1.0, 1.0)
        out[i].width = 5.0 + vnoise(x, y, 5.0)
    }
}
Motion{duration: 1.0 layers: [Draw2D{draw: fn(c, t, edit) { c.segs(probe.out) }}]}"#;
    let l = segs(src, 0.0);
    assert_eq!(l.segs.len(), 64);
    for (i, s) in l.segs.iter().enumerate() {
        let f = i as f64;
        let want = [
            makepad_script_compute::rand::hashn(&[f - 20.0, 7.0, 3.0]),
            makepad_script_compute::rand::hashn(&[f * 13.0, -4.0]),
            makepad_script_compute::rand::vnoise(f * 0.37 - 9.1, f * 0.53 + 2.2, 5.0),
        ];
        let got = [(s.a[0] as f64 - 100.0) / 1000.0, (s.a[1] as f64 - 100.0) / 800.0, s.width as f64 - 5.0];
        for k in 0..3 {
            assert!((got[k] - want[k]).abs() < 1e-5, "element {i} value {k}: {} vs {}", got[k], want[k]);
        }
    }
}

/// A kernel that runs at load (no time, no keyed param) still draws with a
/// draw's own data: `with:` runs it again with that data.
#[test]
fn a_load_time_kernel_draws_with_the_draws_data() {
    let src = r#"
use mod.motion.*; use mod.kernel; let bars = Kernel{
    count: 1
    xs: kernel.input(f32, [0.0])
    lift: kernel.param(0.0)
    out: kernel.output(kernel.Segment)
    element: fn(i) {
        out[i].a = vec3(xs[i], 100.0 + lift, 0.0)
        out[i].b = vec3(xs[i] + 10.0, 100.0 + lift, 0.0)
        out[i].color = vec4(1.0, 1.0, 1.0, 1.0)
        out[i].width = 2.0
    }
}
Motion{duration: 1.0 layers: [Draw2D{draw: fn(c, t, edit) { c.segs(bars.out, {with: {count: 3 xs: [100.0, 200.0, 300.0] lift: 50.0}}) }}]}"#;
    let l = segs(src, 0.0);
    assert_eq!(l.segs.len(), 3, "{:?}", l.segs);
    assert!((l.segs[2].a[0] - 300.0).abs() < 1e-3 && (l.segs[2].a[1] - 150.0).abs() < 1e-3, "{:?}", l.segs[2]);
}

/// An editor's patch of a literal compiled into a kernel (its fn's, or a
/// `let` it reads as a constant) or read into it at load (its `count`,
/// written there or through a `let`) shows once the document loads again:
/// the patch asks for that reload. A literal the kernel does not compile in
/// patches live as before.
#[test]
fn a_kernels_literals_reload_the_document() {
    use makepad_motion::schema::{CodeFile, Literal};
    let src = "use mod.motion.*; use mod.kernel; let LIFT = 10.0\nlet OTHER = 3.0\nlet strokes = Kernel{\n    count: 2\n    out: kernel.output(kernel.Segment)\n    element: fn(i) {\n        out[i].a = vec3(float(i) * 7.5, LIFT, 0.0)\n        out[i].b = vec3(1.0, 2.0, 0.0)\n        out[i].color = vec4(1.0, 0.5, 0.25, 0.75)\n        out[i].width = 2.0\n    }\n}\nMotion{duration: 2.0 layers: [Draw2D{draw: fn(c, t, edit) { c.segs(strokes.out) }} Rect{size: fn(t) { vec2(OTHER, 10) } fill: #ffffff}]}";
    let doc = makepad_motion::editable(|| MotionDoc::load(src)).unwrap();
    let at = src.find("7.5").unwrap();
    assert!(doc.patch_literal(&CodeFile::Doc, src, at..at + 3, Literal::Number(8.0)).needs_reload(), "a literal in the kernel's fn");
    let at = src.find("10.0").unwrap();
    assert!(doc.patch_literal(&CodeFile::Doc, src, at..at + 4, Literal::Number(12.0)).needs_reload(), "a let the kernel reads");
    let at = src.find("3.0").unwrap();
    assert!(!doc.patch_literal(&CodeFile::Doc, src, at..at + 3, Literal::Number(4.0)).needs_reload(), "a let only the layers read");
    let at = src.find("count: 2").unwrap() + 7;
    assert!(doc.patch_literal(&CodeFile::Doc, src, at..at + 1, Literal::Number(3.0)).needs_reload(), "a kernel's count");
    let src = src.replace("let OTHER = 3.0", "let OTHER = 3.0\nlet N = 2").replace("count: 2", "count: N");
    let doc = makepad_motion::editable(|| MotionDoc::load(&src)).unwrap();
    let at = src.find("N = 2").unwrap() + 4;
    assert!(doc.patch_literal(&CodeFile::Doc, &src, at..at + 1, Literal::Number(3.0)).needs_reload(), "a let a kernel's count copied");
}

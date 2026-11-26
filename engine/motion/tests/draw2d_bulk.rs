//! Draw2D's bulk `c.segs(data)` records exactly what one `c.seg_rgb` call
//! per segment records.

use makepad_motion::draw2d::Draw2dList;
use makepad_motion::frame::ItemKind;
use makepad_motion::*;

fn list(draw: &str) -> Draw2dList {
    let doc = MotionDoc::load(&format!("use mod.motion.*; Motion{{size: vec2(640, 360) duration: 1.0 layers: [Draw2D{{draw: fn(c, t, edit) {{ {draw} }}}}]}}")).unwrap_or_else(|d| panic!("{d:?}"));
    let frame = doc.eval(0.5);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    let ItemKind::Draw2d { picture, .. } = &frame.items[0].kind else { panic!("not a Draw2D item") };
    (*picture.list).clone()
}

#[test]
fn bulk_segments_are_the_same_as_one_call_each() {
    let one = list("c.seg_rgb(10, 20, 30, 40, 2, 1.5, 0.5, 0.25, 1) c.seg_rgb(0, 0, t * 100, 50, 4, 0, 1, 0, 0.5) c.seg_rgb(5, 5, 6, 6, 1, 1, 1, 1, 0)");
    let bulk = list("c.segs([10, 20, 30, 40, 2, 1.5, 0.5, 0.25, 1, 0, 0, t * 100, 50, 4, 0, 1, 0, 0.5, 5, 5, 6, 6, 1, 1, 1, 1, 0])");
    assert_eq!(one.segs.len(), 2);
    assert_eq!(format!("{:?}", one.segs), format!("{:?}", bulk.segs));
}

/// `c.segs(data, {m alpha})` places the ends as the Splash arithmetic of
/// the same affine does, keeps the widths, and multiplies the alphas (a
/// segment faded to 0 is skipped).
#[test]
fn placed_bulk_segments_are_the_splash_placement() {
    let one = list(
        "let m = [0.8 * cos(0.3), 0.8 * sin(0.3), 0.8 * sin(0.3), -0.8 * cos(0.3), 960.0 - 11.0, 540.0 + 7.0]
         let p = fn(x, y) { return vec2(m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]) }
         let a = p(10, 20) let b = p(30, 40) let q = p(t * 100, 50)
         c.seg_rgb(a.x, a.y, b.x, b.y, 2, 1.5, 0.5, 0.25, min(1.0, 0.9 * 0.5))
         c.seg_rgb(b.x, b.y, q.x, q.y, 4, 0, 1, 0, 0.5 * 0.5)",
    );
    let bulk = list(
        "let m = [0.8 * cos(0.3), 0.8 * sin(0.3), 0.8 * sin(0.3), -0.8 * cos(0.3), 960.0 - 11.0, 540.0 + 7.0]
         c.segs([10, 20, 30, 40, 2, 1.5, 0.5, 0.25, 0.9, 30, 40, t * 100, 50, 4, 0, 1, 0, 0.5, 5, 5, 6, 6, 1, 1, 1, 1, 0], {m: m alpha: 0.5})",
    );
    assert_eq!(one.segs.len(), 2);
    assert_eq!(format!("{:?}", one.segs), format!("{:?}", bulk.segs));
}

/// `c.segs(data, {dash})` dashes as `c.stroke` dashes the same polyline:
/// the same pieces, the pattern running on across the segments.
#[test]
fn dashed_bulk_segments_are_the_stroke_dashes() {
    let stroke = list("c.stroke([[10, 20], [50.3, 20], [50.3, 63.3], [12, 70]], 1.5, [1, 1, 1, 1], 1.0, {dash: 5.5 gap: 4.5 offset: 1.25})");
    let bulk = list("c.segs([10, 20, 50.3, 20, 1.5, 1, 1, 1, 1, 50.3, 20, 50.3, 63.3, 1.5, 1, 1, 1, 1, 50.3, 63.3, 12, 70, 1.5, 1, 1, 1, 1], {dash: [5.5, 4.5, 1.25]})");
    let geometry = |l: &Draw2dList| l.segs.iter().map(|s| (s.a, s.b, s.width)).collect::<Vec<_>>();
    assert!(stroke.segs.len() > 10);
    assert_eq!(geometry(&stroke), geometry(&bulk));
}

/// `vnoise(x, y, seed)` is, bit for bit, the Splash value noise over
/// `hashn` it stands for (a draw placing a segment at each).
#[test]
fn vnoise_is_the_splash_value_noise() {
    let pts = "[[0.3, 0.7, 1.0], [12.25, -3.5, 2.0], [-7.9, 44.01, 1.0], [1003.3, 2.0000001, 7.0], [0.0, 0.0, 0.0]]";
    let splash = list(&format!(
        "let fade = fn(t) {{ return t * t * t * (t * (t * 6.0 - 15.0) + 10.0) }}
         let lerp = fn(a, b, t) {{ return a + (b - a) * t }}
         let n2 = fn(x, y, seed) {{
             let ix = floor(x) let iy = floor(y) let fx = fade(x - ix) let fy = fade(y - iy)
             return lerp(lerp(hashn(ix, iy, seed), hashn(ix + 1.0, iy, seed), fx), lerp(hashn(ix, iy + 1.0, seed), hashn(ix + 1.0, iy + 1.0, seed), fx), fy) * 2.0 - 1.0
         }}
         for p in {pts} {{ let v = n2(p[0] * t * 2.0, p[1], p[2]) * 100.0 c.seg_rgb(v, 0, v, 1, 1, 1, 1, 1, 1) }}"
    ));
    let native = list(&format!("for p in {pts} {{ let v = vnoise(p[0] * t * 2.0, p[1], p[2]) * 100.0 c.seg_rgb(v, 0, v, 1, 1, 1, 1, 1, 1) }}"));
    assert_eq!(splash.segs.len(), 5);
    assert_eq!(format!("{:?}", splash.segs), format!("{:?}", native.segs));
}

/// `c.curve` records, bit for bit, the stroke a Splash loop computing the
/// same Bézier points records (a partial draw-on under a transform, with
/// a count of segments that does not divide evenly).
#[test]
fn curve_is_the_same_as_the_splash_loop() {
    let loop_ = list(
        "c.set_transform(0.61, 0, 0, 0.61, 13.5, -7.25)
         let x0 = 300.0 let y0 = 629.0 let x1 = 300.4458 let y1 = 596.29 let x2 = 492.0 let y2 = -11.0 let x3 = 940.0 let y3 = -11.0
         let k = 0.3 * t + 0.137
         let n = 36
         let du = k / n
         let pts = []
         for i in 0..37 {
             if i <= n {
                 let u = i * du
                 let v = 1.0 - u
                 let a0 = v * v * v let a1 = 3.0 * v * v * u let a2 = 3.0 * v * u * u let a3 = u * u * u
                 pts.push([a0 * x0 + a1 * x1 + a2 * x2 + a3 * x3, a0 * y0 + a1 * y1 + a2 * y2 + a3 * y3])
             }
         }
         c.stroke(pts, 1.7, [0.4, 0.5, 0.6, 1.0], 1.0, {cap: @round})",
    );
    let curve = list(
        "c.set_transform(0.61, 0, 0, 0.61, 13.5, -7.25)
         c.curve([300.0, 629.0, 300.4458, 596.29, 492.0, -11.0, 940.0, -11.0, 1.0, 32.0], 1.7, [0.4, 0.5, 0.6, 1.0], 1.0, {n: 36 upto: 0.3 * t + 0.137})",
    );
    assert_eq!(loop_.segs.len(), 36);
    assert_eq!(format!("{:?}", loop_.segs), format!("{:?}", curve.segs));
    assert_eq!(format!("{:?}", loop_.ops), format!("{:?}", curve.ops));
}

/// `c.segs(data, {upto})` draws on by length as a Splash loop over the
/// same polyline does (the first r of its length, the last piece cut), and
/// returns where it stopped, in the data's units.
#[test]
fn drawn_on_bulk_segments_are_the_splash_draw_on() {
    let pts = "[[10, 20], [50.3, 20], [50.3, 63.3], [12, 70], [12, 70], [40, 90]]";
    for r in ["0.0", "0.37", "0.5", "1.0"] {
        let splash = list(&format!(
            "let pts = {pts}
             var total = 0.0
             for i in 1..pts.len() {{ total = total + sqrt(pow(pts[i][0] - pts[i - 1][0], 2.0) + pow(pts[i][1] - pts[i - 1][1], 2.0)) }}
             var left = {r} * total
             var head = vec2(pts[0][0], pts[0][1])
             var done = false
             for i in 1..pts.len() {{
                 if !done {{
                     let ax = pts[i - 1][0] let ay = pts[i - 1][1]
                     let len = sqrt(pow(pts[i][0] - ax, 2.0) + pow(pts[i][1] - ay, 2.0))
                     let f = if len > 0.0 {{ min(1.0, left / len) }} else {{ 1.0 }}
                     let e = vec2(ax + (pts[i][0] - ax) * f, ay + (pts[i][1] - ay) * f)
                     c.seg_rgb(ax, ay, e.x, e.y, 2.2, 1, 0.5, 0.25, 1)
                     head = e
                     left = left - len
                     if left <= 0.0 {{ done = true }}
                 }}
             }}
             c.seg_rgb(head.x, head.y, head.x, head.y + 1, 1, 1, 1, 1, 1)"
        ));
        let native = list(&format!(
            "let pts = {pts}
             let data = []
             for i in 1..pts.len() {{ for v in [pts[i - 1][0], pts[i - 1][1], pts[i][0], pts[i][1], 2.2, 1, 0.5, 0.25, 1] {{ data.push(v) }} }}
             let head = c.segs(data, {{upto: {r}}})
             c.seg_rgb(head.x, head.y, head.x, head.y + 1, 1, 1, 1, 1, 1)"
        ));
        // The same pieces, to float precision (a Splash loop's maths may run
        // in f32 when it is compiled ahead of time).
        assert_eq!(splash.segs.len(), native.segs.len(), "upto {r}");
        for (a, b) in splash.segs.iter().zip(&native.segs) {
            let close = |p: [f32; 2], q: [f32; 2]| (p[0] - q[0]).abs() < 1e-3 && (p[1] - q[1]).abs() < 1e-3;
            assert!(close(a.a, b.a) && close(a.b, b.b) && a.width == b.width && a.color == b.color, "upto {r}: {a:?} vs {b:?}");
        }
    }
}

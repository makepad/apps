//! A document's VM collects the garbage its frames leave (every frame
//! allocates), and what lives across frames (the document, its functions,
//! Draw2D's `c`, memo'd state) survives the collection.

use makepad_motion::frame::ItemKind;
use makepad_motion::*;

#[test]
fn frames_after_collections_are_the_frames_before() {
    // Each frame builds arrays and objects it drops (garbage), reads a
    // memo'd value, and draws through `c`.
    let doc = MotionDoc::load(
        "use mod.motion.*; let path = fn(t) { let pts = [] for i in 0..400 { pts.push({x: i * 1.5 y: sin(t + i * 0.1) * 40.0}) } return pts }
         let shared = fn(t) { return [t * 2.0, {k: t}] }
         Motion{size: vec2(640, 360) duration: 10.0 layers: [Draw2D{draw: fn(c, t, edit) {
             let s = memo(shared, t)
             let pts = path(t)
             for i in 1..pts.len() { c.seg_rgb(pts[i - 1].x, pts[i - 1].y + 180.0, pts[i].x, pts[i].y + 180.0, 1.0 + s[0] * 0.0, 1, 1, 1, 1) }
             c.text(\"t\" + s[1].k, 10, 20, {px: 12})
         }}]}",
    )
    .unwrap_or_else(|d| panic!("{d:?}"));
    let segs = |t: f64| {
        let frame = doc.eval(t);
        assert!(frame.errors.is_empty(), "{:?}", frame.errors);
        let ItemKind::Draw2d { picture, .. } = &frame.items[0].kind else { panic!("not a Draw2D item") };
        format!("{:?}", picture.list.segs)
    };
    let first = segs(1.25);
    // Enough frames for the heap to double several times over.
    for k in 0..300 {
        segs(k as f64 * 0.01);
    }
    assert_eq!(segs(1.25), first, "the same frame after the collections");
}

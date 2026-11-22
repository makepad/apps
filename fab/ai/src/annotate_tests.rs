//! The marker's marks: what a stroke means, where it is kept, and that `fab.pick` agrees.

use crate::annotate::{Anchor, Cam, Kind};
use crate::Session;
use fab_model::params::xf_point;
use fab_plate::machine::Machine;
use makepad_model::json::Value;

fn args(text: &str) -> Value {
    makepad_model::json::parse(text.as_bytes()).unwrap()
}

/// Three parts side by side on the plate: a mesh box each, 20 mm apart.
fn plate() -> Session {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let head = s.model.head().text();
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet a = m.box({size: [20mm 20mm 20mm] at: [0 10mm 0]})\nobject(a)\nlet b = m.box({size: [20mm 20mm 20mm] at: [40mm 10mm 0]})\nobject(b)\nlet c = m.box({size: [20mm 20mm 20mm] at: [80mm 10mm 0]})\nobject(c)\n";
    let request = format!(r#"{{"request_id":"a","expected":{head:?},"script":"three.cad.splash","source":{source:?}}}"#);
    let r = s.call("script", &args(&request), None);
    assert!(r.ok, "{}", r.text);
    s
}

/// A camera in front of the plate, looking along the printer's Y, the plate centre in the middle.
fn front(s: &Session) -> Cam {
    let ev = s.last.as_ref().unwrap();
    let b = ev.plate.placed_parts().map(|p| p.bounds()).reduce(|a, b| a.union(&b)).unwrap();
    let c: [f64; 3] = std::array::from_fn(|k| (b.min[k] + b.max[k]) * 0.5);
    Cam { eye: [c[0], c[1] - 300.0, c[2]], forward: [0.0, 1.0, 0.0], right: [800.0 / 600.0, 0.0, 0.0], up: [0.0, 0.0, 1.0], fov: Some(30.0), across: None, w: 800.0, h: 600.0 }
}

fn solid_centre(s: &Session, solid: &str) -> [f64; 3] {
    let ev = s.last.as_ref().unwrap();
    let b = ev.plate.parts.iter().find(|p| p.object == solid).unwrap().bounds();
    std::array::from_fn(|k| (b.min[k] + b.max[k]) * 0.5)
}

fn pixel(cam: &Cam, p: [f64; 3]) -> [f64; 2] {
    cam.project(p).unwrap()
}

fn circle(c: [f64; 2], r: f64) -> Vec<[f64; 2]> {
    (0..=40).map(|i| {
        let a = i as f64 * std::f64::consts::TAU / 40.0;
        [c[0] + r * a.cos(), c[1] + r * a.sin()]
    }).collect()
}

fn solids(s: &Session, index: usize) -> Vec<String> {
    s.marks.marks[index].covers.iter().map(|c| c.solid.clone()).collect()
}

/// A circle round one part of three names that part alone, with its bounds right to a few mm; a dot
/// is that point; an open stroke is what is under the line; fab.pick on a pixel inside the outline
/// lands in the mark's solid.
#[test]
fn a_circle_names_what_it_encloses_and_a_dot_its_point() {
    let mut s = plate();
    let cam = front(&s);
    let objects: Vec<String> = s.last.as_ref().unwrap().plate.parts.iter().map(|p| p.object.clone()).collect();
    assert_eq!(objects.len(), 3, "{objects:?}");
    let middle = objects.iter().find(|o| o.ends_with("/b")).unwrap().clone();
    let centre = solid_centre(&s, &middle);
    let at = pixel(&cam, centre);
    let edge = pixel(&cam, [centre[0] + 10.0, centre[1], centre[2]]);
    let r = (edge[0] - at[0]).abs() * 1.5;
    let i = s.annotate_add(cam.clone(), circle(at, r), 6.0).unwrap();
    assert_eq!(s.marks.marks[i].kind, Kind::Circle);
    assert_eq!(solids(&s, i), [middle.clone()], "the circle names the middle part and not the others");
    assert!((s.marks.marks[i].covers[0].share - 1.0).abs() < 1e-9);
    let text = s.annotations_tool(&args("{}")).text;
    assert!(text.contains(&middle) && !text.contains(&objects[0]) && !text.contains(&objects[2]), "{text}");
    // the bounds, plate mm, the part's own box to a few mm
    let ev = s.last.clone().unwrap();
    let part = ev.plate.parts.iter().find(|p| p.object == middle).unwrap().bounds();
    let pts: Vec<[f64; 3]> = s.marks.marks[i].covered.iter().filter_map(|a| s.anchor_plate(a)).collect();
    for k in [0, 2] {
        let lo = pts.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min);
        let hi = pts.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max);
        assert!((lo - part.min[k]).abs() < 3.0 && (hi - part.max[k]).abs() < 3.0, "axis {k}: {lo}..{hi} against {}..{}", part.min[k], part.max[k]);
    }
    // fab.pick on the sent camera and a pixel inside the outline
    let pick = |s: &mut Session, p: [f64; 2]| {
        let camera = format!(r#"{{"px":800,"py":600,"eye":{:?},"forward":{:?},"right":{:?},"up":{:?},"fov":30}}"#, cam.eye, cam.forward, cam.right, cam.up);
        s.call("pick", &args(&format!(r#"{{"camera":{camera},"x":{},"y":{}}}"#, p[0], p[1])), None)
    };
    let r = pick(&mut s, at);
    assert!(r.ok && r.text.contains(&middle), "{}", r.text);
    // a dot on a known point of the left part: that point
    let left = objects.iter().find(|o| o.ends_with("/a")).unwrap().clone();
    // a point of its front face, where the view meets it
    let target = {
        let b = s.last.as_ref().unwrap().plate.parts.iter().find(|p| p.object == left).unwrap().bounds();
        let c = solid_centre(&s, &left);
        [c[0], b.min[1], c[2]]
    };
    let d = s.annotate_add(cam.clone(), vec![pixel(&cam, target)], 6.0).unwrap();
    assert_eq!(s.marks.marks[d].kind, Kind::Dot);
    assert_eq!(solids(&s, d), [left.clone()]);
    let hit = s.anchor_plate(&s.marks.marks[d].outline[0]).unwrap();
    assert!((0..3).all(|k| (hit[k] - target[k]).abs() < 0.05), "{hit:?} against {target:?}");
    // an open stroke across the first two parts: what is under the line
    let (p0, p1) = (pixel(&cam, solid_centre(&s, &left)), pixel(&cam, centre));
    let line: Vec<[f64; 2]> = (0..=20).map(|k| [p0[0] + (p1[0] - p0[0]) * k as f64 / 20.0, p0[1] + (p1[1] - p0[1]) * k as f64 / 20.0]).collect();
    let o = s.annotate_add(cam.clone(), line, 6.0).unwrap();
    assert_eq!(s.marks.marks[o].kind, Kind::Open);
    let mut under = solids(&s, o);
    under.sort();
    assert_eq!(under, { let mut w = vec![left, middle.clone()]; w.sort(); w });
    // the numbers run 1, 2, 3 and a pop takes the newest
    assert_eq!(s.marks.marks.iter().map(|m| m.number).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(s.annotate_pop(), Some(3));
}

/// A circle misses the surface along most of its length: its points sit on the plane through its own
/// hits that faces the view, and they follow the part when it moves.
#[test]
fn the_misses_of_a_circle_sit_on_the_plane_of_its_hits_and_follow_the_part() {
    let mut s = plate();
    let cam = front(&s);
    let objects: Vec<String> = s.last.as_ref().unwrap().plate.parts.iter().map(|p| p.object.clone()).collect();
    let middle = objects.iter().find(|o| o.ends_with("/b")).unwrap().clone();
    let centre = solid_centre(&s, &middle);
    let at = pixel(&cam, centre);
    let i = s.annotate_add(cam.clone(), circle(at, 90.0), 6.0).unwrap();
    let m = s.marks.marks[i].clone();
    let off: Vec<&Anchor> = m.outline.iter().filter(|a| !a.surface).collect();
    assert!(!off.is_empty(), "a ring round a small part misses it");
    // the plane: depth along the view equal for every miss, and the mean depth of the hits
    let depth = |p: [f64; 3]| (p[1] - cam.eye[1]) * cam.forward[1];
    let hits: Vec<f64> = m.covered.iter().map(|a| depth(s.anchor_plate(a).unwrap())).collect();
    let mean = hits.iter().sum::<f64>() / hits.len() as f64;
    for a in &off {
        let d = depth(s.anchor_plate(a).unwrap());
        assert!((d - mean).abs() < 0.01, "a miss at depth {d}, the hits' plane at {mean}");
    }
    // the part moves 15 mm: the mark's points move with it, to the mm
    let before: Vec<[f64; 3]> = m.outline.iter().map(|a| s.anchor_plate(a).unwrap()).collect();
    let moved = {
        let ev = s.last.as_ref().unwrap();
        let k = ev.plate.parts.iter().position(|p| p.object == middle).unwrap();
        let mut t = ev.model_to_plate[k];
        t[0][3] += 15.0;
        t
    };
    let old = s.last.as_ref().unwrap().model_to_plate.clone();
    let k = s.last.as_ref().unwrap().plate.parts.iter().position(|p| p.object == middle).unwrap();
    let mut ev = (**s.last.as_ref().unwrap()).clone();
    ev.model_to_plate[k] = moved;
    s.last = Some(std::sync::Arc::new(ev));
    let after: Vec<[f64; 3]> = m.outline.iter().map(|a| s.anchor_plate(a).unwrap()).collect();
    for ((a, b), anchor) in before.iter().zip(&after).zip(&m.outline) {
        let follows = anchor.solid.as_deref() == Some(middle.as_str());
        let dx = b[0] - a[0];
        assert!(if follows { (dx - 15.0).abs() < 1e-6 } else { dx.abs() < 1e-6 }, "{anchor:?} moved {dx}");
    }
    let _ = (old, xf_point);
}

/// A mark that hit nothing says so, and the AI's tools can read marks sent with a line later.
#[test]
fn a_mark_on_nothing_says_so_and_sent_marks_stay_readable() {
    let mut s = plate();
    let cam = front(&s);
    let i = s.annotate_add(cam.clone(), vec![[5.0, 5.0]], 6.0).unwrap();
    assert!(s.marks.marks[i].covers.is_empty());
    let text = s.annotations_tool(&args("{}")).text;
    assert!(text.contains("nothing"), "{text}");
    s.annotate_sent(2);
    s.annotate_clear();
    let now = s.annotations_tool(&args("{}")).text;
    assert!(now.contains("marks: []") && now.contains("line: 2"), "{now}");
    let r = s.annotations_tool(&args(r#"{"line":2}"#)).text;
    assert!(r.contains("camera:"), "{r}");
    let unsent = s.annotate_add(cam, vec![[7.0, 7.0]], 6.0).unwrap();
    s.annotate_clear();
    assert!(s.marks.marks.iter().all(|m| !m.active) && unsent > 0);
    assert_eq!(s.marks.marks.len(), 1, "an unsent mark is gone when cleared");
}

/// Marks are not an edit: drawing, taking back, sending and clearing them leave the program, the
/// build and the model's history exactly as they were (so a slice of the plate has nothing to redo).
#[test]
fn marks_never_touch_the_program_or_the_history() {
    let mut s = plate();
    let cam = front(&s);
    let before = (s.model.head().text(), s.model.project_text().to_string(), s.last.clone().unwrap());
    let at = pixel(&cam, solid_centre(&s, &s.last.as_ref().unwrap().plate.parts[0].object.clone()));
    for call in [
        format!(r#"{{"action":"add","camera":{{"px":800,"py":600,"eye":{:?},"forward":{:?},"right":{:?},"up":{:?},"fov":30}},"line":[[{},{}],[{},{}]]}}"#, cam.eye, cam.forward, cam.right, cam.up, at[0], at[1], at[0] + 40.0, at[1]),
        r#"{"action":"sent","line":1}"#.to_string(),
        r#"{"action":"pop"}"#.to_string(),
        r#"{"action":"clear"}"#.to_string(),
    ] {
        let r = s.call("annotate", &args(&call), None);
        assert!(r.ok && r.update.is_none() && r.render.is_none(), "{call}: {}", r.text);
    }
    let r = s.call("annotations", &args("{}"), None);
    assert!(r.ok, "{}", r.text);
    assert_eq!(s.model.head().text(), before.0, "the head moved");
    assert_eq!(s.model.project_text(), before.1, "the project text changed");
    assert!(std::sync::Arc::ptr_eq(s.last.as_ref().unwrap(), &before.2), "the plate was built again");
}

/// A circle that takes in neighbours names the one with most of it: not all covers are equal.
#[test]
fn a_circle_over_neighbours_names_the_likely_one() {
    let mut s = plate();
    let cam = front(&s);
    let objects: Vec<String> = s.last.as_ref().unwrap().plate.parts.iter().map(|p| p.object.clone()).collect();
    let a = objects.iter().find(|o| o.ends_with("/a")).unwrap();
    let b = objects.iter().find(|o| o.ends_with("/b")).unwrap();
    let (pa, pb) = (pixel(&cam, solid_centre(&s, a)), pixel(&cam, solid_centre(&s, b)));
    // centred on a, wide enough to clip b's near edge
    let centre = [pa[0] + (pb[0] - pa[0]) * 0.1, pa[1]];
    let r = (pb[0] - pa[0]).abs() * 0.62;
    let i = s.annotate_add(cam, circle(centre, r), 6.0).unwrap();
    let covers = &s.marks.marks[i].covers;
    assert!(covers.len() >= 2 && covers[0].solid == *a && covers[0].share > covers[1].share, "{covers:?}");
    let text = s.annotations_tool(&args("{}")).text;
    assert!(text.contains(&format!("most_likely: \"{a}\"")), "{text}");
}

/// A z range of the plate falls in the layers of the part's slice (1-based), the nearest when outside.
#[test]
fn a_mark_names_the_layers_it_runs_through() {
    use crate::annotate::layer_range;
    let layers: Vec<(f64, f64)> = (1..=10).map(|i| (i as f64 * 0.2, 0.2)).collect();
    assert_eq!(layer_range(&layers, 0.05, 0.15), Some((1, 1)));
    assert_eq!(layer_range(&layers, 0.5, 1.1), Some((3, 6)));
    assert_eq!(layer_range(&layers, 5.0, 9.0), Some((10, 10)), "above the part: its last layer");
    assert_eq!(layer_range(&[], 0.0, 1.0), None);
}

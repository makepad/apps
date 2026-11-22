use super::*;
use crate::camera::{gltf_point, Frame, View, ViewCamera};
use crate::raster::DepthMap;
use fab_plate::machine::Machine;

const PRELUDE: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";

fn args(text: &str) -> Value {
    makepad_model::json::parse(text.as_bytes()).unwrap()
}

/// A session with one block `width` wide, 20 mm deep and tall: the width a parameter, `w`.
fn block(tag: &str, width: f64) -> Session {
    let dir = std::env::temp_dir().join(format!("fab-variants-{tag}-{}", std::process::id()));
    let mut s = Session::new(Machine::dual_nozzle_350(), dir);
    s.rebuild().unwrap();
    let head = s.model.head().text();
    let source = format!("{PRELUDE}let w = param({width}mm, min: 5mm, max: 60mm)\nlet b = m.box({{size: [w 20mm 20mm] at: [0 10mm 0]}})\nobject(b)\n");
    let call = Value::Obj(vec![("request_id".into(), Value::Str("a".into())), ("expected".into(), Value::Str(head)), ("source".into(), Value::Str(source))]);
    let r = s.call("script", &call, None);
    assert!(r.ok, "{}", r.text);
    s
}

/// What a plate's meshes cover from a camera.
fn silhouette(ev: &Evaluated, camera: &ViewCamera) -> DepthMap {
    let mut map = DepthMap::new(camera.side, camera.side);
    for rm in &ev.render {
        for t in rm.indices.chunks(3) {
            map.triangle_mm(camera, [0, 1, 2].map(|k| rm.positions[t[k] as usize].map(f64::from)));
        }
    }
    map
}

/// The glTF box a plate's meshes fill: what a review frames.
fn shown(evs: &[&Evaluated]) -> [[f32; 3]; 2] {
    let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for p in evs.iter().flat_map(|ev| ev.render.iter().flat_map(|rm| &rm.positions)) {
        let q = gltf_point(*p);
        for k in 0..3 {
            lo[k] = lo[k].min(q[k]);
            hi[k] = hi[k].max(q[k]);
        }
    }
    [lo, hi]
}

/// The view's camera as the app's review draws a variant job's: its one camera for all.
fn camera(job: &VariantJob, view: &str, side: usize) -> ViewCamera {
    let evs: Vec<&Evaluated> = job.built.iter().map(|v| v.ev.as_ref()).collect();
    let view: View = named_view(view).unwrap();
    Frame::of_render(shown(&evs), std::slice::from_ref(&view), &job.camera).camera(&view, side)
}

/// Columns a silhouette covers.
fn width(map: &DepthMap) -> usize {
    (0..map.width).filter(|&x| (0..map.height).any(|y| map.covered(x, y))).count()
}

fn variants(s: &mut Session, call: &str) -> Arc<VariantJob> {
    let r = s.call(TOOL, &args(call), None);
    assert!(r.ok, "{}", r.text);
    let Some(RenderRequest::Variants(job)) = r.render else { panic!("fab.variants asks for views") };
    job
}

/// Two widths of a block, drawn from one camera: the wider one is as much wider on the sheet,
/// nothing is committed, and each comes with the fab.apply call that commits it.
#[test]
fn two_variants_show_side_by_side_at_one_scale() {
    let mut s = block("two", 20.0);
    let head = s.model.head().text();
    let job = variants(&mut s, r#"{"variants": [{"name": "as is", "edits": "set(w, 20mm)"}, {"name": "wide", "edits": "set(w, 30mm)"}]}"#);
    assert_eq!(s.model.head().text(), head, "nothing was committed");
    assert_eq!(job.built.len(), 2);
    assert!(matches!(job.camera.focus, Some(Focus::Box(lo, hi)) if (hi[0] - lo[0] - 30.0).abs() < 0.5), "one box frames both: {:?}", job.camera.focus);
    let front = camera(&job, "front", 400);
    let [narrow, wide] = [0, 1].map(|i| width(&silhouette(&job.built[i].ev, &front)) as f64);
    assert!((wide / narrow - 1.5).abs() < 0.04, "{narrow} and {wide} px across");
    let drawn: Vec<Vec<CapturedView>> = (0..2).map(|_| vec![CapturedView { camera: front, fields: Vec::new() }]).collect();
    let a = answer(&job, None, &drawn).unwrap();
    assert_eq!(a.order.iter().map(|(i, _, _)| *i).collect::<Vec<_>>(), [0, 1], "as asked");
    assert!(a.text.contains("fab.apply {request_id:") && a.text.contains(&head) && a.text.contains("set(w, 30mm)"), "{}", a.text);
    // its apply call commits it
    let r = s.call("apply", &args(&format!(r#"{{"request_id": "variant wide", "expected": "{head}", "edits": "set(w, 30mm)"}}"#)), None);
    assert!(r.ok, "{}", r.text);
}

/// A variant whose edits do not build is told with why; the rest are drawn.
#[test]
fn a_variant_that_does_not_build_is_told_and_the_rest_drawn() {
    let mut s = block("failed", 20.0);
    let job = variants(&mut s, r#"{"variants": [{"name": "wide", "edits": "set(w, 30mm)"}, {"name": "gone", "edits": "set(nothing_here, 3mm)"}], "views": ["front", "right"]}"#);
    assert_eq!(job.built.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["wide"]);
    assert!(job.failed.len() == 1 && job.failed[0].0 == "gone" && job.failed[0].1.contains("nothing_here"), "{:?}", job.failed);
    assert_eq!(job.rows(None), ["front", "right"]);
    let drawn: Vec<Vec<CapturedView>> = vec![["front", "right"].iter().map(|v| CapturedView { camera: camera(&job, v, 300), fields: Vec::new() }).collect()];
    let a = answer(&job, None, &drawn).unwrap();
    assert!(a.text.contains("failed:") && a.text.contains("gone"), "{}", a.text);
    for (call, says) in [
        (r#"{"variants": [{"name": "one", "edits": "set(w, 30mm)"}]}"#, "2 to 6"),
        (r#"{"variants": [{"name": "a", "edits": "set(w, 30mm)"}, {"name": "a", "edits": "set(w, 25mm)"}]}"#, "two variants are named"),
        (r#"{"variants": [{"name": "a", "edits": "set(w, 30mm)"}, {"name": "b", "edits": "set(w, 25mm)"}], "views": ["front", "right", "top"]}"#, "'views'"),
        (r#"{"variants": [{"name": "a", "edits": "set(w, 30mm)"}, {"name": "b", "edits": "set(w, 25mm)"}], "compare": {"picture": "front.png", "solid": "b"}}"#, "'solid'"),
    ] {
        let r = s.call(TOOL, &args(call), None);
        assert!(!r.ok && r.text.contains(says), "{call}: {}", r.text);
    }
}

/// With a reference picture each variant is read against it from the same camera, and the one
/// the picture is of ranks first.
#[test]
fn with_a_picture_the_variant_that_matches_it_ranks_first() {
    // the picture: a block 40 mm wide, from the front, a brown figure on paper
    let mut pictured = block("picture", 40.0);
    let ev = pictured.review_plate().unwrap();
    let view = named_view("front").unwrap();
    let lens = Frame::of_render(shown(&[ev.as_ref()]), std::slice::from_ref(&view), &Camera::default()).camera(&view, 400);
    let map = silhouette(&ev, &lens);
    let pixels = (0..400 * 400).flat_map(|i| if map.covered(i % 400, i / 400) { [120, 80, 50, 255] } else { [245, 243, 236, 255] }).collect();
    let png = makepad_model::RgbaImage { width: 400, height: 400, pixels }.to_png(&makepad_model::Limits::default()).unwrap();
    let path = std::env::temp_dir().join(format!("fab-variants-picture-{}.png", std::process::id()));
    std::fs::write(&path, png).unwrap();

    let mut s = block("ranked", 20.0);
    let call = format!(r#"{{"variants": [{{"name": "w20", "edits": "set(w, 20mm)"}}, {{"name": "w40", "edits": "set(w, 40mm)"}}, {{"name": "w55", "edits": "set(w, 55mm)"}}], "compare": {{"picture": {:?}}}}}"#, path.display().to_string());
    let job = variants(&mut s, &call);
    let front = camera(&job, "front", 400);
    let drawn: Vec<Vec<CapturedView>> = job.built.iter().map(|_| vec![CapturedView { camera: front, fields: Vec::new() }]).collect();
    let a = answer(&job, None, &drawn).unwrap();
    let ranked: Vec<&str> = a.order.iter().map(|(i, _, _)| job.built[*i].name.as_str()).collect();
    assert_eq!(ranked[0], "w40", "{}", a.text);
    assert!(a.text.contains("iou:") && a.text.contains("differs:"), "{}", a.text);
    assert!(a.order[0].1[0].starts_with("1 w40 IoU"), "the cell's words: {:?}", a.order[0].1);
}

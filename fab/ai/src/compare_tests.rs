use super::*;
use crate::camera::{gltf_point, view_side};
use fab_plate::machine::Machine;

/// The most pixels a side the chat takes of a picture (the hub's tool picture limit).
const SIDE: u32 = 1024;

const PRELUDE: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";
/// A head on a body: a 10 mm ball on a cylinder 12 mm across and 30 mm tall, 50 mm in all.
const FIGURE: &str = "let body = m.cylinder({radius: 6mm height: 30mm at: [0 15mm 0]})\nlet head = m.sphere({radius: 10mm at: [0 40mm 0]})\nobject(m.union(body, head))\n";

fn args(text: &str) -> Value {
    makepad_model::json::parse(text.as_bytes()).unwrap()
}

/// A session with one object script, `source` after the prelude.
fn figure(tag: &str, source: &str) -> Session {
    let dir = std::env::temp_dir().join(format!("fab-compare-{tag}-{}", std::process::id()));
    let mut s = Session::new(Machine::dual_nozzle_350(), dir);
    s.rebuild().unwrap();
    let head = s.model.head().text();
    let call = Value::Obj(vec![
        ("request_id".into(), Value::Str("a".into())),
        ("expected".into(), Value::Str(head)),
        ("source".into(), Value::Str(format!("{PRELUDE}{source}"))),
    ]);
    let r = s.call("script", &call, None);
    assert!(r.ok, "{}", r.text);
    s
}

/// A view of the session's plate as the review frames it (its meshes' box).
fn camera(s: &mut Session, view: &str, views: &[String], side: usize, camera: Camera) -> ViewCamera {
    let ev = s.review_plate().unwrap();
    let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for p in ev.render.iter().flat_map(|rm| &rm.positions) {
        let q = gltf_point(*p);
        for k in 0..3 {
            lo[k] = lo[k].min(q[k]);
            hi[k] = hi[k].max(q[k]);
        }
    }
    let views: Vec<View> = views.iter().map(|v| named_view(v).unwrap()).collect();
    Frame::of_render([lo, hi], &views, &camera).camera(&named_view(view).unwrap(), side)
}

const PAPER: [u8; 3] = [245, 243, 236];
const BROWN: [u8; 3] = [120, 80, 50];

/// What the session's plate covers from a view, `side` pixels square, and the view.
fn covers(s: &mut Session, view: &str, side: usize, lens: Camera) -> (Vec<bool>, ViewCamera) {
    let camera = camera(s, view, &[view.to_string()], side, lens);
    let ev = s.review_plate().unwrap();
    let map = draw_view(&plate_triangles(&ev, &[]), &CapturedView { camera, fields: Vec::new() });
    ((0..side * side).map(|i| map.covered(i % side, i / side)).collect(), camera)
}

/// A picture `w` x `h` of `colour(x, y)`, as a PNG file.
fn paint(w: usize, h: usize, file: &str, colour: impl Fn(usize, usize) -> [u8; 3]) -> String {
    let pixels = (0..w * h).flat_map(|i| {
        let [r, g, b] = colour(i % w, i / w);
        [r, g, b, 255]
    }).collect();
    let png = makepad_model::RgbaImage { width: w as u32, height: h as u32, pixels }.to_png(&makepad_model::Limits::default()).unwrap();
    let path = std::env::temp_dir().join(format!("fab-compare-{file}-{}.png", std::process::id()));
    std::fs::write(&path, png).unwrap();
    path.display().to_string()
}

/// The session's plate as a picture from a view: a brown figure on paper, as a PNG file.
fn picture(s: &mut Session, view: &str, side: usize, lens: Camera, file: &str) -> (String, ViewCamera) {
    let (mask, camera) = covers(s, view, side, lens);
    (paint(side, side, file, |x, y| if mask[y * side + x] { BROWN } else { PAPER }), camera)
}

/// A mask's rows: its first, and past its last.
fn rows(mask: &[bool], w: usize) -> (usize, usize) {
    let filled: Vec<usize> = (0..mask.len() / w).filter(|&y| (0..w).any(|x| mask[y * w + x])).collect();
    (filled[0], filled[filled.len() - 1] + 1)
}

/// Three small grey buttons in the top right corner of a `side` picture, as a screenshot's are.
fn button(side: usize, x: usize, y: usize) -> bool {
    (0..3).any(|k| {
        let x0 = side - 34 - k * 30;
        (x0..x0 + 22).contains(&x) && (12..34).contains(&y)
    })
}

/// A soft shadow on the paper round the figure's feet: the paper's colour up to a third darker,
/// fading out to its edge, its middle a little above the figure's lowest row.
fn floor_shadow(mask: &[bool], side: usize) -> impl Fn(usize, usize) -> Option<[u8; 3]> + '_ {
    let (_, bottom) = rows(mask, side);
    let feet: Vec<usize> = (0..side).filter(|&x| (bottom - 6..bottom).any(|y| mask[y * side + x])).collect();
    let (left, right) = (feet[0] as f64, feet[feet.len() - 1] as f64 + 1.0);
    let (cx, cy, a, b) = ((left + right) * 0.5, bottom as f64 - 4.0, (right - left) * 0.8 + 16.0, 22.0);
    move |x, y| {
        let d = ((x as f64 + 0.5 - cx) / a).powi(2) + ((y as f64 + 0.5 - cy) / b).powi(2);
        (d < 1.0).then(|| PAPER.map(|c| (c as f64 * (1.0 - 0.3 * (1.0 - d).powi(2))).round() as u8))
    }
}

/// fab.compare's call, and the views it asks for as the review would draw them.
fn compare_call(s: &mut Session, call: &str) -> (Arc<CompareJob>, Vec<ViewCamera>) {
    let r = s.call(TOOL, &args(call), None);
    assert!(r.ok, "{}", r.text);
    let Some(RenderRequest::Compare(job)) = r.render else { panic!("fab.compare asks for views") };
    let side = view_side(job.views.len());
    let cameras = job.views.iter().map(|v| camera(s, v, &job.views, side, job.camera)).collect();
    (job, cameras)
}

fn captured(cameras: &[ViewCamera]) -> Vec<CapturedView<'static>> {
    cameras.iter().map(|c| CapturedView { camera: *c, fields: Vec::new() }).collect()
}

fn band(c: &Comparison, b: usize) -> BandRow {
    *c.bands.iter().find(|r| r.band == b).unwrap_or_else(|| panic!("no band {b}: {:?}", c.bands))
}

/// The model against a picture of itself: the silhouettes agree, band by band.
#[test]
fn a_figure_against_its_own_picture_agrees() {
    let mut s = figure("self", FIGURE);
    let (narrow, _) = picture(&mut s, "front", 600, Camera { fov: Some(15.0), ..Camera::default() }, "self");
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {narrow:?}, "view": "front", "fov": 15}}"#));
    let c = measure(&job, &captured(&cameras)).unwrap();
    assert!(c.iou > 0.97, "{c:?}");
    // a narrow lens: what is nearer than the middle looks hardly larger
    assert!((c.figure * c.mm_per_px - 50.0).abs() < 1.0, "the figure is {} mm", c.figure * c.mm_per_px);
    for b in &c.bands {
        assert!((b.model - b.reference).abs() * c.mm_per_px < 0.4 && b.offset.is_some_and(|o| o.abs() * c.mm_per_px < 0.4), "{b:?}");
    }
    let (text, images) = answer(&job, &captured(&cameras), SIDE).unwrap();
    assert!(text.contains("iou:") && text.contains("bands:") && text.contains("mm per reference pixel"), "{text}");
    assert_eq!(images.len(), 2, "the sheet and the figure as read");
    // the three panels of a 600 px picture side by side: no wider than the chat takes
    let sides: Vec<[u32; 2]> = images.iter().map(|(_, png)| [u32::from_be_bytes(png[16..20].try_into().unwrap()), u32::from_be_bytes(png[20..24].try_into().unwrap())]).collect();
    assert!(sides.iter().all(|s| s[0].max(s[1]) <= SIDE) && sides[0][0] % 3 == 0, "three whole panels: {sides:?}");
    // orthographic, as a drawing is: every depth at one scale, so the figure's height is its own
    let (drawing, _) = picture(&mut s, "front", 600, Camera { ortho: true, ..Camera::default() }, "self-ortho");
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {drawing:?}, "view": "front", "ortho": true}}"#));
    let c = measure(&job, &captured(&cameras)).unwrap();
    assert!(c.iou > 0.97 && (c.figure * c.mm_per_px - 50.0).abs() < 0.3, "the figure is {} mm: {c:?}", c.figure * c.mm_per_px);
}

/// A head 20% wider and a body 3 mm to one side: the IoU drops, the widest difference is the
/// head's, and the bands' middles move apart by the shift.
#[test]
fn a_wider_head_and_a_moved_body_show_in_the_bands() {
    let mut reference = figure("changed", "let body = m.cylinder({radius: 6mm height: 30mm at: [3mm 15mm 0]})\nlet head = m.sphere({radius: 10mm at: [0 40mm 0] scale: [1.2 1 1]})\nobject(m.union(body, head))\n");
    let (picture, _) = picture(&mut reference, "front", 600, Camera::default(), "changed");
    let mut s = figure("model", FIGURE);
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {picture:?}, "view": "front"}}"#));
    let c = measure(&job, &captured(&cameras)).unwrap();
    assert!(c.iou < 0.9, "{c:?}");
    assert!(c.bands[0].band <= 6, "the most different band is the head's: {:?}", c.bands[0]);
    // the head's widest band: the reference's 20% wider
    let equator = band(&c, 4);
    assert!((equator.reference / equator.model - 1.2).abs() < 0.05, "{equator:?}");
    // the body's middle 3 mm from the head's, against the reference's
    let mean = |bands: &[usize]| bands.iter().map(|&b| band(&c, b).offset.unwrap()).sum::<f64>() / bands.len() as f64 * c.mm_per_px;
    let shift = mean(&[2, 3, 4, 5]) - mean(&[10, 11, 12, 13, 14, 15]);
    assert!((shift - 3.0).abs() < 0.5, "the body moved {shift} mm against the head");
}

/// A spot of the picture lands on the model where it shows there: on its surface, within a mm.
#[test]
fn a_picture_point_lands_on_the_model_within_a_mm() {
    let mut s = figure("points", FIGURE);
    let ev = s.review_plate().unwrap();
    let top = ev.render.iter().flat_map(|rm| &rm.positions).map(|p| p[2] as f64).fold(f64::NEG_INFINITY, f64::max);
    let head: Vec<[f64; 3]> = ev.render.iter().flat_map(|rm| &rm.positions).filter(|p| p[2] as f64 > top - 19.0).map(|p| p.map(f64::from)).collect();
    let middle = |k: usize| (head.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min) + head.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max)) * 0.5;
    // the head's front, as the front view sees it (from -y)
    let nose = [middle(0), middle(1) - 10.0, top - 10.0];
    let (picture, reference) = picture(&mut s, "front", 600, Camera::default(), "points");
    let [x, y, _] = reference.view_point(nose).unwrap();
    let call = format!(r#"{{"picture": {picture:?}, "view": "front", "points": {{"nose": [{}, {}], "air": [0.03, 0.5]}}}}"#, x / 600.0, y / 600.0);
    let (job, cameras) = compare_call(&mut s, &call);
    let c = measure(&job, &captured(&cameras)).unwrap();
    let at = |name: &str| c.points.iter().find(|(n, _)| n == name).unwrap().1;
    let p = at("nose");
    let off = (0..3).map(|k| (p.at[k] - nose[k]).powi(2)).sum::<f64>().sqrt();
    assert!(p.surface && off < 1.0, "{p:?} is {off} mm from {nose:?}");
    assert!(!at("air").surface, "beside the figure is the view plane");
}

/// The fit finds the view a picture was taken from, within 5 degrees.
#[test]
fn the_fit_finds_the_view_within_five_degrees() {
    // an arm out to one side and a nose to the front: no two views alike
    let mut s = figure("fit", "let body = m.cylinder({radius: 6mm height: 30mm at: [0 15mm 0]})\nlet head = m.sphere({radius: 10mm at: [0 40mm 0]})\nlet arm = m.box({size: [14mm 4mm 4mm] at: [12mm 20mm 0]})\nlet nose = m.box({size: [4mm 4mm 6mm] at: [0, 41mm, -11mm]})\nobject(m.union(m.union(m.union(body, head), arm), nose))\n");
    let (picture, _) = picture(&mut s, "35,-15", 512, Camera::default(), "fit");
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {picture:?}, "fit": true}}"#));
    assert_eq!(job.views, survey_views());
    let fit = fit(&job, &captured(&cameras)).unwrap();
    assert!(wrap(fit.yaw - 35.0).abs() <= 5.0 && (fit.pitch + 15.0).abs() <= 5.0, "{fit:?}");
    assert!(fit.missed.is_empty(), "meshes are whole from every side: {fit:?}");
}

/// A screenshot: the figure on a white card with round corners in a dark frame, its top a couple
/// of pixels under the card's edge (a bow's tips). Cut to the card's edges with 'rect', the frame's
/// corners are left out as specks and the figure is read whole; not cut, the card is the figure
/// and the notes say to cut.
#[test]
fn a_card_in_a_frame_cut_to_its_edges_reads_the_figure_whole() {
    let mut s = figure("card", FIGURE);
    let (side, card, inset, round) = (640usize, 560usize, 40usize, 36.0);
    let (mask, _) = covers(&mut s, "front", card, Camera::default());
    let lift = rows(&mask, card).0 - 2;
    let figure = |x: usize, y: usize| y + lift < card && mask[(y + lift) * card + x];
    let on_card = |x: usize, y: usize| {
        let (x, y) = (x as f64 - inset as f64 + 0.5, y as f64 - inset as f64 + 0.5);
        let off = |v: f64| if v < round { round - v } else if v > card as f64 - round { v - (card as f64 - round) } else { 0.0 };
        (0.0..card as f64).contains(&x) && (0.0..card as f64).contains(&y) && off(x).powi(2) + off(y).powi(2) <= round * round
    };
    let path = paint(side, side, "card", |x, y| match (on_card(x, y), on_card(x, y) && figure(x - inset, y - inset)) {
        (_, true) => BROWN,
        (true, false) => [252, 252, 252],
        (false, _) => [32, 33, 38],
    });
    let rect = [inset, inset, inset + card, inset + card].map(|p| p as f64 / side as f64);
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {path:?}, "view": "front", "rect": {rect:?}}}"#));
    let r = &job.reference;
    assert_eq!((r.width, r.height), (card, card), "the cut's pixels");
    assert!((0..card * card).all(|i| r.mask[i] == figure(i % card, i / card)), "the figure, its top included, and nothing else");
    assert_eq!(r.dropped.len(), 4, "the frame's four corners: {:?}", r.dropped);
    for ([x0, y0, x1, y1], _) in &r.dropped {
        let corner = |a: f64, b: f64| (a - rect[0]).abs() < 1e-9 || (b - rect[2]).abs() < 1e-9;
        assert!(corner(*x0, *x1) && corner(*y0, *y1) && x1 - x0 < 0.07 && y1 - y0 < 0.07, "a corner of the cut: {:?}", r.dropped);
    }
    let c = measure(&job, &captured(&cameras)).unwrap();
    assert!(c.iou > 0.97, "{c:?}");
    let (text, _) = answer(&job, &captured(&cameras), SIDE).unwrap();
    assert!(text.contains("4 smaller one(s) were left out") && text.contains("cut to 'rect'"), "{text}");
    // not cut: the frame is the background and the card the figure
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {path:?}, "view": "front"}}"#));
    let (text, _) = answer(&job, &captured(&cameras), SIDE).unwrap();
    assert!(text.contains("cut to the card with 'rect'"), "{text}");
}

/// Small buttons in a corner, apart from the figure: left out as pieces of their own and said where,
/// the figure read as it is; kept ('islands'), they stretch the alignment and the IoU drops.
#[test]
fn buttons_in_a_corner_are_left_out_and_said() {
    let mut s = figure("buttons", FIGURE);
    let side = 512;
    let (mask, _) = covers(&mut s, "front", side, Camera::default());
    let path = paint(side, side, "buttons", |x, y| if mask[y * side + x] { BROWN } else if button(side, x, y) { [72, 74, 80] } else { PAPER });
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {path:?}, "view": "front"}}"#));
    assert!(job.reference.mask == mask, "the figure alone");
    assert!(job.reference.dropped.len() == 3 && job.reference.dropped.iter().all(|(b, px)| b[0] > 0.75 && b[3] < 0.1 && *px == 22 * 22), "{:?}", job.reference.dropped);
    let kept = measure(&job, &captured(&cameras)).unwrap();
    let (text, _) = answer(&job, &captured(&cameras), SIDE).unwrap();
    assert!(text.contains("3 smaller one(s) were left out") && text.contains("'islands': true"), "{text}");
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {path:?}, "view": "front", "islands": true}}"#));
    assert!(job.reference.dropped.is_empty() && job.reference.mask.iter().filter(|&&m| m).count() == mask.iter().filter(|&&m| m).count() + 3 * 22 * 22);
    let islands = measure(&job, &captured(&cameras)).unwrap();
    assert!(kept.iou > 0.97 && islands.iou + 0.05 < kept.iou, "{} kept, {} with the buttons", kept.iou, islands.iou);
}

/// A soft shadow on the paper touching the figure's feet: background, so the figure is as tall as
/// it is (the shadow's edge against the feet aside, a pixel); kept ('shadows': false), its dark
/// middle joins the figure below its feet.
#[test]
fn a_soft_floor_shadow_under_the_feet_is_background() {
    let mut s = figure("shadow", FIGURE);
    let side = 512;
    let (mask, _) = covers(&mut s, "front", side, Camera::default());
    let shadow = floor_shadow(&mask, side);
    let path = paint(side, side, "shadow", |x, y| if mask[y * side + x] { BROWN } else { shadow(x, y).unwrap_or(PAPER) });
    let (top, bottom) = rows(&mask, side);
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {path:?}, "view": "front"}}"#));
    let r = &job.reference;
    let (read_top, read_bottom) = rows(&r.mask, side);
    let figure = mask.iter().filter(|&&m| m).count();
    let more = (0..side * side).filter(|&i| r.mask[i] && !mask[i]).count();
    assert!(read_top == top && read_bottom <= bottom + 1 && more * 50 < figure && (0..side * side).all(|i| !mask[i] || r.mask[i]), "rows {read_top}..{read_bottom} for {top}..{bottom}, {more} px more");
    assert!(r.shadow > 0.002, "{}", r.shadow);
    let c = measure(&job, &captured(&cameras)).unwrap();
    assert!(c.iou > 0.97, "{c:?}");
    let (text, _) = answer(&job, &captured(&cameras), SIDE).unwrap();
    assert!(text.contains("background's shadow"), "{text}");
    let (job, _) = compare_call(&mut s, &format!(r#"{{"picture": {path:?}, "view": "front", "shadows": false}}"#));
    assert!(rows(&job.reference.mask, side).1 >= bottom + 8, "the shadow joins the figure: {:?} for {top}..{bottom}", rows(&job.reference.mask, side));
}

/// A three-quarter view with buttons in its corner and a soft shadow at its feet: the fit finds
/// the view as it does without them.
#[test]
fn the_fit_finds_the_view_past_corner_buttons_and_a_floor_shadow() {
    let mut s = figure("fit-busy", "let body = m.cylinder({radius: 6mm height: 30mm at: [0 15mm 0]})\nlet head = m.sphere({radius: 10mm at: [0 40mm 0]})\nlet arm = m.box({size: [14mm 4mm 4mm] at: [12mm 20mm 0]})\nlet nose = m.box({size: [4mm 4mm 6mm] at: [0, 41mm, -11mm]})\nobject(m.union(m.union(m.union(body, head), arm), nose))\n");
    let side = 512;
    let (mask, _) = covers(&mut s, "35,-15", side, Camera::default());
    let shadow = floor_shadow(&mask, side);
    let path = paint(side, side, "fit-busy", |x, y| if mask[y * side + x] { BROWN } else if button(side, x, y) { [72, 74, 80] } else { shadow(x, y).unwrap_or(PAPER) });
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {path:?}, "fit": true}}"#));
    let fit = fit(&job, &captured(&cameras)).unwrap();
    assert!(wrap(fit.yaw - 35.0).abs() <= 5.0 && (fit.pitch + 15.0).abs() <= 5.0, "{fit:?}");
}

/// Traced tiles drawn with cracks between them, and a tile's hole they enclose: the cracks close
/// and the hole is seen through, unless the view's whole drawing covers it (another solid there,
/// the silhouette cut to these tiles' solid).
#[test]
fn cracked_tiles_close_and_the_hole_they_enclose_shows_through() {
    let mut map = DepthMap::new(64, 64);
    for ty in 0..6 {
        for tx in 0..6 {
            if (tx, ty) == (2, 2) {
                continue;
            }
            let (x0, y0) = (4.0 + tx as f32 * 9.5, 4.0 + ty as f32 * 9.5);
            let (x1, y1) = (x0 + 8.0, y0 + 8.0);
            map.triangle([[x0, y0, 1.0], [x1, y0, 1.0], [x1, y1, 1.0]], [0.0, 0.0, -1.0]);
            map.triangle([[x0, y0, 1.0], [x1, y1, 1.0], [x0, y1, 1.0]], [0.0, 0.0, -1.0]);
        }
    }
    assert!((4..60).all(|x| !map.covered(x, 12)) && (4..60).all(|y| !map.covered(12, y)), "a crack across and one down");
    let silhouette = solid(&map, None);
    let crack = |x: usize, y: usize| (x == 12 && (4..60).contains(&y)) || (y == 12 && (4..60).contains(&x));
    for y in 0..64 {
        for x in 0..64 {
            let inside = (4..60).contains(&x) && (4..60).contains(&y);
            assert!(!silhouette[y * 64 + x] || inside, "{x},{y}: outside the tiles");
            assert!(silhouette[y * 64 + x] || !(crack(x, y) || map.covered(x, y)), "{x},{y}: a crack or a tile");
        }
    }
    // the missing tile's middle (it spans 23 to 31 each way)
    let hole = |x: usize, y: usize| (24..31).contains(&x) && (24..31).contains(&y);
    assert!((0..64 * 64).all(|i| !(hole(i % 64, i / 64) && silhouette[i])), "the hole is seen through");
    // another solid covers the hole: it is the silhouette's
    let mut whole = DepthMap::new(64, 64);
    whole.triangle([[4.0, 4.0, 2.0], [60.0, 4.0, 2.0], [60.0, 60.0, 2.0]], [0.0, 0.0, -1.0]);
    whole.triangle([[4.0, 4.0, 2.0], [60.0, 60.0, 2.0], [4.0, 60.0, 2.0]], [0.0, 0.0, -1.0]);
    let filled = solid(&map, Some(&whole));
    for y in 0..64 {
        for x in 0..64 {
            let inside = (4..60).contains(&x) && (4..60).contains(&y);
            assert_eq!(filled[y * 64 + x], inside || map.covered(x, y), "{x},{y}");
        }
    }
}

/// A plate point of the session's one part, from its model frame (mm, Y up).
fn on_plate(s: &mut Session, p: [f64; 3]) -> [f64; 3] {
    fab_model::params::xf_point(&s.review_plate().unwrap().model_to_plate[0], p)
}

/// A head above a body with a gap between them: the picture cut to the head ('rect') against
/// the model cut to its head ('solid') agrees, the head 20 mm tall; against the whole model, the
/// model is squeezed into the head's height.
#[test]
fn a_head_cut_from_the_picture_compares_with_the_model_s_head() {
    let mut s = figure("solid", "let body = m.cylinder({radius: 6mm height: 26mm at: [0 13mm 0]})\nlet head = m.sphere({radius: 10mm at: [0 40mm 0]})\nobject(m.union(body, head))\n");
    let side = 600;
    let (mask, _) = covers(&mut s, "front", side, Camera::default());
    let (top, _) = rows(&mask, side);
    let gap = (top..side).find(|&y| (0..side).all(|x| !mask[y * side + x])).expect("a gap under the head");
    let path = paint(side, side, "solid", |x, y| if mask[y * side + x] { BROWN } else { PAPER });
    let rect = format!("[0, 0, 1, {}]", (gap as f64 + 0.5) / side as f64);
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {path:?}, "view": "front", "rect": {rect}, "solid": "head"}}"#));
    let c = measure(&job, &captured(&cameras)).unwrap();
    assert!(c.iou > 0.97 && (c.figure * c.mm_per_px - 20.0).abs() < 0.6, "the head is {} mm: {c:?}", c.figure * c.mm_per_px);
    let (text, _) = answer(&job, &captured(&cameras), SIDE).unwrap();
    assert!(text.contains("solids:") && text.contains("/head"), "{text}");
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {path:?}, "view": "front", "rect": {rect}}}"#));
    let whole = measure(&job, &captured(&cameras)).unwrap();
    assert!(whole.iou < 0.85 && whole.figure * whole.mm_per_px > 40.0, "the whole model in the head's height: {whole:?}");
}

/// A drawing (orthographic: one scale at every depth) with big flat feet under the same body and
/// head: aligned by its whole height the model is stretched (its head too wide); aligned on the
/// crown and the chin, or on the head's band against the model's head, the heads agree and the
/// drawing reads 58 mm tall.
#[test]
fn drawn_feet_are_left_out_of_the_alignment_by_a_band_or_points() {
    let mut drawing = figure("feet", "let body = m.cylinder({radius: 6mm height: 30mm at: [0 23mm 0]})\nlet head = m.sphere({radius: 10mm at: [0 48mm 0]})\nlet feet = m.box({size: [24mm 8mm 10mm] at: [0 4mm 0]})\nobject(m.union(m.union(body, head), feet))\n");
    let side = 600;
    let (mask, seen) = covers(&mut drawing, "front", side, Camera { ortho: true, ..Camera::default() });
    let path = paint(side, side, "feet", |x, y| if mask[y * side + x] { BROWN } else { PAPER });
    let at = |p: [f64; 3]| seen.view_point(p).map(|q| [q[0] as f64 / side as f64, q[1] as f64 / side as f64]).unwrap();
    let (crown, chin) = (at(on_plate(&mut drawing, [0.0, 58.0, 0.0])), at(on_plate(&mut drawing, [0.0, 38.0, 0.0])));
    let (top, bottom) = rows(&mask, side);
    let head_band = (chin[1] * side as f64 - top as f64) / (bottom - top) as f64;
    let mut s = figure("feet-model", FIGURE);
    let (model_crown, model_chin) = (on_plate(&mut s, [0.0, 50.0, 0.0]), on_plate(&mut s, [0.0, 30.0, 0.0]));
    // the head's equator, a tenth of the drawing's height under its top: band 3 of 16
    let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {path:?}, "view": "front", "ortho": true}}"#));
    let whole = measure(&job, &captured(&cameras)).unwrap();
    let equator = band(&whole, 3);
    assert!(equator.model / equator.reference > 1.1, "stretched to the feet: {equator:?}");
    let by_points = format!(r#"{{"points": {{"crown": {{"picture": {crown:?}, "plate": {model_crown:?}}}, "chin": {{"picture": {chin:?}, "plate": {model_chin:?}}}}}}}"#);
    let by_band = format!(r#"{{"band": [0, {head_band}], "solid": "head"}}"#);
    for (how, align, slack) in [("points", by_points, 0.03), ("band", by_band, 0.04)] {
        let (job, cameras) = compare_call(&mut s, &format!(r#"{{"picture": {path:?}, "view": "front", "ortho": true, "align": {align}}}"#));
        let c = measure(&job, &captured(&cameras)).unwrap();
        let equator = band(&c, 3);
        assert!((equator.model / equator.reference - 1.0).abs() < slack && (c.figure * c.mm_per_px - 58.0).abs() < 0.8, "{how}: {equator:?}, the drawing {} mm", c.figure * c.mm_per_px);
        assert!(c.align_off.iter().all(|(_, off)| *off < 0.3), "{how}: {:?}", c.align_off);
        let (text, _) = answer(&job, &captured(&cameras), SIDE).unwrap();
        assert!(text.contains(if how == "points" { "align_off:" } else { "rows 0.000 to" }), "{how}: {text}");
    }
}

/// A field solid's surface on the plate: nothing from it on its own skin, the rest of the
/// field (a body blended with it) far from it.
#[test]
fn a_field_solid_s_surface_is_its_own_part_of_the_field() {
    let mut s = figure("field-skin", "let body = m.ellipsoid({radii: [9mm 14mm 8mm] at: [0 14mm 0]})\nlet head = m.sphere({radius: 7mm at: [0 34mm 0]})\nobject(m.blend([body head], 1.5mm))\n");
    assert!(s.review_plate().unwrap().fields[0].is_some(), "one field part");
    let head = s.solid_skin("head").unwrap();
    let (crown, belly) = (on_plate(&mut s, [0.0, 41.0, 0.0]), on_plate(&mut s, [9.0, 14.0, 0.0]));
    assert!(head.distance(crown) < 0.05 && head.distance(belly) > 5.0, "{} at the crown, {} at the belly", head.distance(crown), head.distance(belly));
}

/// What the call cannot take is refused in words.
#[test]
fn a_compare_without_its_view_or_picture_is_refused() {
    let mut s = figure("refused", FIGURE);
    let (picture, _) = picture(&mut s, "front", 200, Camera::default(), "refused");
    for (call, says) in [
        (format!(r#"{{"picture": {picture:?}}}"#), "give the view"),
        (format!(r#"{{"picture": {picture:?}, "view": "sideways"}}"#), "unknown view"),
        (format!(r#"{{"picture": {picture:?}, "view": "front", "rect": [0.5, 0.2, 0.4, 0.9]}}"#), "'rect'"),
        (format!(r#"{{"picture": {picture:?}, "view": "front", "background": 500}}"#), "'background'"),
        (format!(r#"{{"picture": {picture:?}, "view": "front", "islands": 1}}"#), "'islands'"),
        (format!(r#"{{"picture": {picture:?}, "view": "front", "solid": "nothing_here"}}"#), "'solid'"),
        (format!(r#"{{"picture": {picture:?}, "view": "front", "align": {{"points": {{"a": {{"picture": [0.5, 0.5], "plate": [0, 0, 0]}}}}}}}}"#), "'align'"),
        (format!(r#"{{"picture": {picture:?}, "fit": true, "align": {{"band": [0, 0.3], "solid": "head"}}}}"#), "'align'"),
        (r#"{"picture": "no such picture", "view": "front"}"#.to_string(), "does not read"),
    ] {
        let r = s.call(TOOL, &args(&call), None);
        assert!(!r.ok && r.text.contains(says), "{call}: {}", r.text);
    }
}

/// A wall 40 mm wide and 30 mm tall, 4 mm thick.
const WALL: &str = "let wall = m.box({size: [40mm 30mm 4mm] at: [0 15mm 0]})\n";
/// The wall with a window 16 x 12 mm through it.
const WINDOW: &str = "let pane = m.box({size: [16mm 12mm 10mm] at: [0 16mm 0]})\nobject(m.subtract(wall, pane))\n";

/// A window through a wall is seen through, in the picture and in the model: the paper the wall
/// encloses is background and the model's opening is open, so the wall with its window matches
/// its picture, and a plain wall against it (or the window against a plain wall's picture) does
/// not. A speck of the paper's colour on the wall stays wall; 'holes': false keeps the window.
#[test]
fn a_window_through_a_wall_is_seen_through() {
    let side = 600;
    let mut windowed = figure("window", &format!("{WALL}{WINDOW}"));
    let (mask, _) = covers(&mut windowed, "front", side, Camera::default());
    let (top, bottom) = rows(&mask, side);
    let cols: Vec<usize> = (0..side).filter(|&x| (0..side).any(|y| mask[y * side + x])).collect();
    let (middle, low) = ((cols[0] + cols[cols.len() - 1]) / 2, bottom - (bottom - top) / 10);
    // the window's middle, a third of the way down: the wall's 16 mm up of 30
    let pane = (middle, top + (bottom - top) * 14 / 30);
    assert!(!mask[pane.1 * side + pane.0], "the model's window shows through it");
    let speck = |x: usize, y: usize| x.abs_diff(middle) <= 1 && y.abs_diff(low) <= 1;
    let drawn = paint(side, side, "window", |x, y| if mask[y * side + x] && !speck(x, y) { BROWN } else { PAPER });
    let call = format!(r#"{{"picture": {drawn:?}, "view": "front"}}"#);
    let at = |job: &CompareJob, (x, y): (usize, usize)| job.reference.mask[y * job.reference.width + x];
    let (job, cameras) = compare_call(&mut windowed, &call);
    let own = measure(&job, &captured(&cameras)).unwrap().iou;
    let (window, speck) = (at(&job, pane), at(&job, (middle, low)));
    let (text, _) = answer(&job, &captured(&cameras), SIDE).unwrap();
    // a plain wall against the window's picture: the picture's window is not the model's
    let mut plain = figure("plain", &format!("{WALL}object(wall)\n"));
    let (job, cameras) = compare_call(&mut plain, &call);
    let missing = measure(&job, &captured(&cameras)).unwrap().iou;
    // the window against a plain wall's picture: the model's window is not the picture's
    let (plain_picture, _) = picture(&mut plain, "front", side, Camera::default(), "plain");
    let (job, cameras) = compare_call(&mut windowed, &format!(r#"{{"picture": {plain_picture:?}, "view": "front"}}"#));
    let extra = measure(&job, &captured(&cameras)).unwrap().iou;
    println!("[compare] IoU: the windowed wall against its picture {own:.3}, a plain wall against it {missing:.3}, the windowed wall against a plain wall's picture {extra:.3}; read as figure: the window {window}, the speck {speck}");
    assert!(own > 0.97, "the windowed wall against its own picture: {own:.3}");
    assert!(missing < 0.9, "a plain wall against a window: {missing:.3}");
    assert!(extra < 0.9, "a window against a plain wall: {extra:.3}");
    assert!(!window, "the paper the wall encloses is background");
    assert!(speck, "a speck of the paper's colour on the wall is wall");
    assert!(text.contains("'holes': false"), "the notes say the window was read as seen through: {text}");
    // 'holes': false: the window is figure
    let (job, _) = compare_call(&mut windowed, &format!(r#"{{"picture": {drawn:?}, "view": "front", "holes": false}}"#));
    assert!(at(&job, pane), "'holes': false keeps the paper the wall encloses");
}

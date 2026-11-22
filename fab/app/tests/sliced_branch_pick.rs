//! A tree's branch in the Sliced view, worked as the person works it, in the Printed Surface
//! look their Fab keeps: a click on a branch (a press and a release, no drag) picks that branch and
//! its beads draw tinted; a click elsewhere lets it go; a press on a branch and a drag orbit the
//! camera exactly as a press and a drag anywhere else do.
//!
//! The plate is one mushroom, a cap on a stem, its underside overhanging all round, with tree
//! supports standing under it, framed from the front. What the view does is read from its own
//! report (its camera, the branch under the pointer, what its frame lights) and from its picture.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU.

mod common;
#[path = "common/picture.rs"]
mod picture;

use makepad_strict_json::Value;
use makepad_test::{run_with_config, KeyCode, Role, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::time::{Duration, Instant};

const HANG: Duration = Duration::from_secs(300);

const MUSHROOM: &str = "// A cap on a stem: the cap's underside overhangs all round.
use mod.model.*
use mod.fab.*
let m = mod.model
let stem = m.box({size: [8mm 16mm 8mm] at: [0, 8mm, 0]})
let cap = m.box({size: [32mm 3mm 32mm] at: [0, 17.5mm, 0]})
let mushroom = m.union(stem, cap)
object(mushroom)
";

/// Tree supports; the mushroom prints blue, its supports with it, so the picked branch's orange is
/// the only orange in the picture.
const PLATE: &str = r#"settings({enable_support: "1" support_type: "tree(auto)"})
color("Blue", rgb: #x2060d0)
place("mushroom", object: "mushroom", at: [175mm 160mm], name: "Mushroom", color: "Blue")
"#;

/// The drag both presses make (points): a turn and a tilt.
const DRAG: (f64, f64) = (60.0, 30.0);

fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let argv = vec!["call".to_string(), format!("fab.{tool}"), args.to_string()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("fab.{tool}: {e:?}"))?;
    let line = lines.iter().find(|l| l.contains("\"Result\"")).ok_or_else(|| format!("fab.{tool} returned no result: {lines:?}"))?;
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).map_err(|e| format!("{e:?}"))?;
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    Ok(r.get("text").and_then(Value::as_str).unwrap_or_default().to_string())
}

fn head_of(reply: &str) -> String {
    makepad_script::syntax::parse_value(reply).expect("a Splash reply").field("head").and_then(|f| f.value.string()).expect("a head").to_string()
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    app.try_wait_until_within(HANG, || (!ok()).then(|| format!("waiting for {what}"))).unwrap_or_else(|why| {
        panic!("{why}; timed out waiting for {what}; the viewport reports {}", app.locator(Selector::id("viewport")).snapshot().description);
    });
}

/// What the viewport reports of the recording on display, and its rectangle (points).
fn view(app: &TestApp) -> (Value, [f64; 4]) {
    let node = app.locator(Selector::id("viewport")).snapshot();
    let report = makepad_strict_json::parse_depth(node.description.as_bytes(), 8).unwrap_or(Value::Null);
    (report, [node.x() as f64, node.y() as f64, node.width() as f64, node.height() as f64])
}

fn at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |v, k| v.get(k))
}

fn flag(v: &Value, path: &[&str]) -> bool {
    at(v, path).and_then(Value::as_bool).unwrap_or(false)
}

fn count(v: &Value, path: &[&str]) -> u64 {
    at(v, path).and_then(Value::as_u64).unwrap_or(0)
}

/// Where the camera looks: the unit vector from its eye to its look point.
fn looking(v: &Value) -> [f64; 3] {
    let point = |k: &str| -> [f64; 3] {
        let p: Vec<f64> = at(v, &["camera", k]).and_then(Value::as_arr).map(|a| a.iter().filter_map(|x| match x {
            Value::F64(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }).collect()).unwrap_or_default();
        assert_eq!(p.len(), 3, "the camera's {k}: {}", v.to_json());
        [p[0], p[1], p[2]]
    };
    let (eye, target) = (point("eye"), point("target"));
    let d = [target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]];
    let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    [d[0] / l, d[1] / l, d[2] / l]
}

/// The angle between two directions, in degrees.
fn degrees(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).clamp(-1.0, 1.0).acos().to_degrees()
}

/// How the camera turned from looking along `a` to looking along `b`, in degrees: about the
/// plate's upright (z), and in its tilt. The orbit is a turntable: a drag turns and tilts it by
/// the same angles wherever it looks from.
fn turn(a: [f64; 3], b: [f64; 3]) -> [f64; 2] {
    let about = |d: [f64; 3]| d[0].atan2(d[1]).to_degrees();
    let tilt = |d: [f64; 3]| d[2].clamp(-1.0, 1.0).asin().to_degrees();
    [(about(b) - about(a) + 540.0).rem_euclid(360.0) - 180.0, tilt(b) - tilt(a)]
}

/// The camera once it has stood still for longer than a glide or a coast, and the view has
/// settled on it.
fn camera_at_rest(app: &TestApp, what: &str) -> Value {
    let mut last = view(app).0.get("camera").map(Value::to_json);
    let mut since = Instant::now();
    let mut report = Value::Null;
    wait_until(app, what, || {
        let (v, _) = view(app);
        let camera = v.get("camera").map(Value::to_json);
        if camera != last {
            last = camera;
            since = Instant::now();
        }
        let rest = since.elapsed() >= Duration::from_millis(500) && flag(&v, &["settled"]);
        report = v;
        rest
    });
    report
}

/// A command of the palette (F3), as the person runs it.
fn command(app: &TestApp, name: &str) {
    app.press_key(KeyCode::Escape);
    app.press_key(KeyCode::F3);
    app.type_text(name);
    app.press_key(KeyCode::ReturnKey);
}

/// The pointer moved to `(x, y)`, no button down: whether the view has a branch under it.
fn branch_under(app: &TestApp, (x, y): (f64, f64)) -> bool {
    app.move_to(x, y);
    flag(&view(app).0, &["branches", "hovered"])
}

/// A point on a branch: the view's points `STEP` apart, the nearest to the middle of its lower
/// half first (the stem stands there, its trees beside it), each hovered in turn until a branch is
/// under the pointer; one whose neighbours 6 points away have a branch under them too, so a press
/// there is on the branch, not at its edge.
fn find_branch(app: &TestApp, rect: [f64; 4]) -> (f64, f64) {
    const STEP: f64 = 16.0;
    let [x0, y0, w, h] = rect;
    let mid = (x0 + w * 0.5, y0 + h * 0.6);
    let mut points: Vec<(f64, f64)> = Vec::new();
    let mut y = y0 + STEP;
    while y < y0 + h - STEP {
        let mut x = x0 + STEP;
        while x < x0 + w - STEP {
            points.push((x, y));
            x += STEP;
        }
        y += STEP;
    }
    let far = |p: &(f64, f64)| (p.0 - mid.0).hypot(p.1 - mid.1);
    points.sort_by(|a, b| far(a).total_cmp(&far(b)));
    for p in points {
        if branch_under(app, p) && [(6.0, 0.0), (-6.0, 0.0), (0.0, 6.0), (0.0, -6.0)].iter().all(|d| branch_under(app, (p.0 + d.0, p.1 + d.1))) {
            return p;
        }
    }
    panic!("no branch under any point of the view; it reports {}", view(app).0.to_json());
}

/// A grab of the window, one pixel a point, once the view shows its finished picture.
struct Grab {
    width: usize,
    height: usize,
    rgb: Vec<[u8; 3]>,
}

impl Grab {
    fn take(app: &TestApp) -> Grab {
        let path = picture::finished_screenshot(app);
        let bytes = std::fs::read(&path).expect("the grab");
        let mut dec = PngDecoder::new(ZCursor::new(&bytes));
        let px = dec.decode_raw().expect("png");
        let (width, height) = dec.dimensions().expect("dims");
        let n = dec.colorspace().expect("colorspace").num_components();
        let rgb = (0..width * height).map(|i| [px[i * n], px[i * n + 1], px[i * n + 2]]).collect();
        Grab { width, height, rgb }
    }

    /// The pixels within `r` points of `(x, y)` in the picked branch's orange.
    fn orange_near(&self, (x, y): (f64, f64), r: f64) -> usize {
        self.orange_in([x - r, y - r, x + r, y + r])
    }

    /// The pixels in the box `[min x, min y, max x, max y]` (points) in the picked branch's
    /// orange: red, about half as much green, little blue.
    fn orange_in(&self, [x0, y0, x1, y1]: [f64; 4]) -> usize {
        let (xa, xb) = ((x0.max(0.0)) as usize, (x1.max(0.0) as usize).min(self.width));
        let (ya, yb) = ((y0.max(0.0)) as usize, (y1.max(0.0) as usize).min(self.height));
        (ya..yb).flat_map(|y| (xa..xb).map(move |x| (x, y))).filter(|&(x, y)| {
            let [r, g, b] = self.rgb[y * self.width + x].map(f64::from);
            r >= 110.0 && g >= 0.3 * r && g <= 0.85 * r && b <= 0.4 * r
        }).count()
    }
}

#[test]
fn a_branch_in_sliced_is_clicked_to_pick_it_and_a_press_on_it_still_orbits() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "sliced_branch_pick").unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), "1280x860@1".into()];
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let prefs = std::env::temp_dir().join(format!("fab-branch-pick-{}-{nanos}.prefs", std::process::id()));
    // The code split off (the view's full width) and the printed surface, as the person's Fab has it.
    std::fs::write(&prefs, "code=0\nsurface=1\n").unwrap();
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let result = run_with_config(config, |app: TestApp| {
        let mut head = String::new();
        wait_until(&app, "the fab tools", || call(&app, "inspect", "{}").map(|v| head = head_of(&v)).is_ok());
        let batch = format!(r#"{{"request_id":"s","expected":{head:?},"script":"mushroom.cad.splash","source":{}}}"#, Value::Str(MUSHROOM.to_string()).to_json());
        head = head_of(&call(&app, "script", &batch).expect("script"));
        let batch = format!(r#"{{"request_id":"p","expected":{head:?},"edits":{}}}"#, Value::Str(PLATE.to_string()).to_json());
        call(&app, "place", &batch).expect("place");
        // Framed from the front, selected and then let go; then Sliced, its trees planned.
        command(&app, "View: Front");
        app.locator(Selector::role(Role::Tab).name("Parts")).click();
        app.locator(Selector::role(Role::TreeItem).name("Mushroom")).wait_visible().click();
        app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name("Frame Selected")).wait_visible().press();
        app.locator(Selector::id("rm_slice")).wait_visible().click();
        app.locator(Selector::id("rm_slice")).wait_checked(true);
        wait_until(&app, "the slice", || common::slice_state(&app) == "Sliced");
        let front = looking(&camera_at_rest(&app, "the framed front view"));
        // Frame Selected comes while View: Front may still glide: it frames along where the glide
        // ends, so the view looks straight along the front, the same picture every run (framed
        // along the glide's direction of the moment, it stood degrees off the front, and the
        // branch found and its tint about the click came out different from one run to the next).
        assert!(degrees(front, [0.0, 1.0, 0.0]) < 0.01, "the framed front view looks along {front:?}, not straight along the front");
        let (_, rect) = view(&app);
        let mut failed: Vec<String> = Vec::new();

        // A branch, and a point off the trees and off the part: above the cap.
        let on = find_branch(&app, rect);
        let off = (rect[0] + rect[2] * 0.5, rect[1] + rect[3] * 0.1);
        assert!(!branch_under(&app, off), "a branch above the cap at {off:?}");
        println!("[branch-pick] a branch at {on:?}, nothing at {off:?}, the view {rect:?}");
        // The part let go: nothing selected, nothing picked.
        app.press_at(off.0, off.1).release();

        // A click on the branch picks it, and its beads draw tinted: orange where the picked branch
        // stands that was not there before it. A click on a trunk below its joins picks the branch
        // that joins nearest above the pointer, so the tint is looked for where the view says the
        // picked branch is, not only about the click: which branch the first point found picks
        // changes with the trees' plan.
        wait_until(&app, "nothing lit", || {
            let v = view(&app).0;
            !flag(&v, &["branches", "lit", "hovered"]) && !flag(&v, &["branches", "lit", "picked"]) && flag(&v, &["settled"])
        });
        let before = Grab::take(&app);
        app.press_at(on.0, on.1).release();
        app.move_to(off.0, off.1);
        let mut lit = Value::Null;
        // what the view says once it settles on the pick, or last said before the app's budget
        // ran out, is what the checks below judge
        let _ = app.try_wait_until_within(HANG, || {
            lit = view(&app).0;
            (!(flag(&lit, &["branches", "lit", "picked"]) && !flag(&lit, &["branches", "lit", "hovered"]) && flag(&lit, &["settled"])))
                .then(|| "waiting for the picked branch to settle".to_string())
        });
        let beads = count(&lit, &["branches", "lit", "beads"]);
        let after = Grab::take(&app);
        // its box on the screen, grown by a bead's few points
        let picked_box: Option<[f64; 4]> = at(&lit, &["branches", "lit", "picked_box"]).and_then(Value::as_arr).map(|a| a.iter().filter_map(|x| match x {
            Value::F64(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }).collect::<Vec<f64>>()).filter(|b| b.len() == 4).map(|b| [b[0] - 6.0, b[1] - 6.0, b[2] + 6.0, b[3] + 6.0]);
        let (near_before, near_after) = (before.orange_near(on, 40.0), after.orange_near(on, 40.0));
        let (before, after) = picked_box.map_or((0, 0), |b| (before.orange_in(b), after.orange_in(b)));
        println!("[branch-pick] clicked: lit {}, {beads} beads; orange where it stands {picked_box:?}: {before} before, {after} after; about the click {near_before} before, {near_after} after", lit.get("branches").map(Value::to_json).unwrap_or_default());
        if !flag(&lit, &["branches", "lit", "picked"]) {
            failed.push(format!("a click on the branch picked none: the view reports {}", lit.to_json()));
        } else if beads == 0 {
            failed.push("the picked branch lights no bead".to_string());
        }
        if picked_box.is_none() {
            failed.push(format!("the view does not say where the picked branch stands: {}", lit.to_json()));
        } else if after < before + 60 {
            failed.push(format!("the picked branch is not drawn tinted: {before} orange pixels where it stands {picked_box:?} before the click, {after} after"));
        }

        // A click elsewhere lets it go.
        app.press_at(off.0, off.1).release();
        if let Err(why) = app.try_wait_until_within(HANG, || {
            flag(&view(&app).0, &["branches", "lit", "picked"]).then(|| "waiting for the branch to be deselected".to_string())
        }) {
            failed.push(format!("{why}; a click off the trees kept the picked branch: the view reports {}", view(&app).0.to_json()));
        }

        // A press on the branch and a drag turn the camera; held still before the release, so
        // nothing coasts on after it.
        let start = looking(&camera_at_rest(&app, "the view before the drag on the branch"));
        assert!(degrees(start, front) < 0.01, "the clicks moved the camera: {start:?}, framed {front:?}");
        let mut drag = app.press_at(on.0, on.1);
        drag.move_by(DRAG.0, DRAG.1);
        std::thread::sleep(Duration::from_millis(400));
        drag.release();
        let rest = camera_at_rest(&app, "the camera after the drag on the branch");
        let on_branch = turn(start, looking(&rest));
        if flag(&rest, &["branches", "lit", "picked"]) {
            failed.push("the drag on the branch picked it".to_string());
        }
        // Then from the front again (the point above the cap is off the trees there), the same
        // drag from that point.
        command(&app, "View: Front");
        let again = looking(&camera_at_rest(&app, "the front view again"));
        assert!(!branch_under(&app, off), "a branch at {off:?} in the front view");
        let mut drag = app.press_at(off.0, off.1);
        drag.move_by(DRAG.0, DRAG.1);
        std::thread::sleep(Duration::from_millis(400));
        drag.release();
        let off_branch = turn(again, looking(&camera_at_rest(&app, "the camera after the drag off the trees")));
        println!("[branch-pick] the drag turned and tilted the camera {on_branch:.3?} deg from the branch, {off_branch:.3?} deg from off the trees");
        if off_branch[0].abs() < 5.0 {
            failed.push(format!("the drag off the trees turned the camera only {:.2} deg", off_branch[0]));
        }
        if (on_branch[0] - off_branch[0]).abs() > 0.05 || (on_branch[1] - off_branch[1]).abs() > 0.05 {
            failed.push(format!("a press on a branch and a drag turned and tilted the camera {on_branch:.2?} deg, the same drag off the trees {off_branch:.2?} deg"));
        }
        assert!(failed.is_empty(), "{}", failed.join("\n"));
    });
    let _ = std::fs::remove_file(&prefs);
    result.unwrap();
}

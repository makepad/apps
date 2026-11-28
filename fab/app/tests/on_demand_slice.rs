//! Slicing on demand, the user's camera and the slice's colours, on one small orange block:
//!
//! - nothing slices in the background: in Normal the plate reads Off and an edit slices nothing;
//! - opening Sliced slices right away and an edit there reslices, and Export in Normal asks for
//!   its own slice;
//! - the camera (zoomed by hand, so an unwanted re-frame cannot be invisible) stays through the
//!   switch to Sliced, the reslice in Sliced, the switch back to Normal and Export's slice in
//!   Normal: slice completion never re-frames the view;
//! - Sliced's toolpaths are not near-white; with no printer linked its printed surface draws
//!   the part in its print colour's own orange as Normal does (the toolpaths are coloured by
//!   feature), and the Program panel is never collapsed to its gutter;
//! - a zoom while it slices (the view's exact-detail level changes) never stops the slice, and
//!   on an exact part it changes only how the view shows it: no evaluation, the same plate, the
//!   slice's revision and plan as they were;
//! - a build that fails while it slices stops the slice only until the build is over.

#[path = "common/view_menu.rs"]
mod view_menu;
#[path = "common/export_print.rs"]
mod export_print;
#[path = "common/render_mode.rs"]
mod render_mode;
mod common;
#[path = "common/picture.rs"]
mod picture;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;

use fab::ui::print_settings::control_name;
use makepad_script::syntax::{self, Node};
use makepad_strict_json::Value;
use makepad_test::{makepad_test, Role, Selector, TestApp, UiSnapshot};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn remote(app: &TestApp, args: &[String]) -> Result<Vec<String>, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, args, &mut |line| {
        lines.push(line.to_string());
        true
    }).map_err(|e| format!("{args:?}: {e:?}; {lines:?}"))?;
    if lines.iter().any(|l| l.contains("\"err\":")) {
        return Err(format!("{args:?}: {lines:?}"));
    }
    Ok(lines)
}

fn call(app: &TestApp, tool: &str, args: &str) -> Result<Node, String> {
    let lines = remote(app, &["call".into(), format!("fab.{tool}"), args.into()])?;
    let line = lines.iter().find(|l| l.contains("\"Result\"")).ok_or_else(|| format!("no result: {lines:?}"))?;
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 64).map_err(|e| format!("{e:?}"))?;
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => &a[0],
        other => other,
    };
    let text = r.get("text").and_then(Value::as_str).unwrap_or_default();
    syntax::parse_value(text).map_err(|e| format!("{e:?}: {text}"))
}

fn head_of(reply: &Node) -> String {
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

fn wait_until(app: &TestApp, what: &str, mut ready: impl FnMut() -> bool) {
    wait_poll(app, || {
        if !ready() { Err(format!("timed out waiting for {what}")) } else { Ok(()) }
    });
}

fn seq(app: &TestApp) -> u64 {
    app.log_since(0).map(|t| t.last_seq).unwrap_or(0)
}

/// Since `from`: a slice input was sent (`fab live edit`) and a whole slice of it came back
/// (`fab live work`, logged once a complete slice's toolpaths are in). A slice that traced
/// nothing is the settings-only input: an edit's input of the re-evaluated plate follows it.
fn resliced(app: &TestApp, from: u64) -> bool {
    let lines = app.log_since(from).expect("the app's log").lines;
    lines.iter().rposition(|l| l.contains("fab live edit:")).is_some_and(|edit| lines[edit..].iter().any(|l| l.contains("fab live work:") && !l.contains("traced 0 planned 0")))
}

#[derive(Debug, PartialEq)]
struct Camera {
    eye: [f64; 3],
    target: [f64; 3],
    ortho: bool,
    ortho_height: f64,
}

fn camera(app: &TestApp) -> Camera {
    // The viewport's semantic inspection describes the camera of its last rendered frame.
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    let value = makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).expect("viewport inspection JSON");
    let c = value.get("camera").expect("viewport camera");
    let number = |v: &Value| match v {
        Value::F64(n) => *n,
        Value::Int(n) => *n as f64,
        _ => panic!("camera number: {v:?}"),
    };
    let point = |name| {
        let coords = c.get(name).and_then(Value::as_arr).expect("camera point");
        assert_eq!(coords.len(), 3, "camera {name}");
        [number(&coords[0]), number(&coords[1]), number(&coords[2])]
    };
    Camera {
        eye: point("eye"),
        target: point("target"),
        ortho: c.get("ortho").and_then(Value::as_bool).expect("camera projection"),
        ortho_height: number(c.get("ortho_height").expect("camera orthographic height")),
    }
}

/// The camera once it has not changed for longer than a quarter-second glide.
fn settled_camera(app: &TestApp) -> Camera {
    let mut previous = camera(app);
    let mut since = Instant::now();
    wait_until(app, "the camera to settle", || {
        let current = camera(app);
        if current != previous {
            previous = current;
            since = Instant::now();
        }
        since.elapsed() >= Duration::from_millis(300)
    });
    previous
}

fn assert_camera(app: &TestApp, before: &Camera, step: &str) {
    let after = camera(app);
    assert!(
        before.ortho == after.ortho
            && before.eye.iter().zip(after.eye).all(|(a, b)| (a - b).abs() <= 1e-6)
            && before.target.iter().zip(after.target).all(|(a, b)| (a - b).abs() <= 1e-6)
            && (before.ortho_height - after.ortho_height).abs() <= 1e-6,
        "camera: {step}: the camera moved: before {before:?}, after {after:?}",
    );
}

/// The slice's state (`fab.slice`); the render mode must stay as selected (`sliced`).
fn slice_state(app: &TestApp, sliced: bool) -> String {
    for (mode, selected) in [("Normal", !sliced), ("Sliced", sliced)] {
        assert_eq!(render_mode::shown(app, mode), selected, "render mode: {mode} must stay as selected");
    }
    let state = common::slice_state(app);
    assert_ne!(state, "Failed", "the slice failed");
    state
}

/// Sliced (after a fresh whole slice when `reslice_from` names the edit's log position), and the
/// camera unchanged at every look until half a second after: past the frame that first says
/// Sliced, delayed redraws and a quarter-second frame glide.
fn assert_stays(app: &TestApp, before: &Camera, step: &str, sliced: bool, reslice_from: Option<u64>) {
    let mut finished_since = None;
    app.try_wait_until_within(HANG, || {
        assert_camera(app, before, step);
        let state = slice_state(app, sliced);
        if state != "Sliced" || reslice_from.is_some_and(|from| !resliced(app, from)) {
            finished_since = None;
        } else if finished_since.get_or_insert_with(Instant::now).elapsed() >= Duration::from_millis(500) {
            return None;
        }
        Some(format!("waiting for {step}: the slice reads {state:?}"))
    }).unwrap_or_else(|why| {
        let lines = app.log_since(0).map(|t| t.lines).unwrap_or_default();
        let tail: Vec<&String> = lines.iter().filter(|l| ["fab live input", "fab live edit", "fab live work", "fab live estimate", "fab live cancel", "fab slice"].iter().any(|w| l.contains(w))).rev().take(16).collect();
        panic!("{why}; waiting for {step}: the slice reads {:?}; the log's last lines, newest first: {tail:#?}", slice_state(app, sliced));
    });
    assert_camera(app, before, step);
}

fn committed(app: &TestApp, height: &str) -> bool {
    let reply = call(app, "inspect", "{}").expect("inspect");
    let project = reply.field("project").and_then(|f| f.value.string()).expect("project source");
    project.contains(&format!("layer_height: \"{height}\""))
}

struct Pixels {
    path: PathBuf,
    total: usize,
    orange: usize,
    near_white: usize,
}

/// The viewport's pixels in a grab of its finished picture: the part's orange and near-white.
fn capture(app: &TestApp) -> Pixels {
    let path = picture::finished_screenshot(app);
    let bytes = std::fs::read(&path).expect("the grab");
    let mut decoder = PngDecoder::new(ZCursor::new(&bytes));
    let raw = decoder.decode_raw().expect("png");
    let (w, h) = decoder.dimensions().expect("dims");
    let channels = decoder.colorspace().expect("colorspace").num_components();
    assert!(channels >= 3, "missing RGB channels; see {}", path.display());
    // Layout points to PNG pixels, including Retina. Excludes the side panels' swatches.
    let window = app.locator(Selector::role(Role::Window)).snapshot();
    let view = app.locator(Selector::id("viewport")).wait_visible().snapshot();
    assert!(window.width() > 0 && window.height() > 0, "missing window bounds; see {}", path.display());
    let (sx, sy) = (w as f64 / window.width() as f64, h as f64 / window.height() as f64);
    let x0 = (((view.x() - window.x()) as f64 * sx).floor().max(0.0) as usize).min(w);
    let y0 = (((view.y() - window.y()) as f64 * sy).floor().max(0.0) as usize).min(h);
    let x1 = (((view.x() + view.width() - window.x()) as f64 * sx).ceil().max(0.0) as usize).min(w);
    let y1 = (((view.y() + view.height() - window.y()) as f64 * sy).ceil().max(0.0) as usize).min(h);
    assert!(x1 > x0 && y1 > y0, "missing viewport pixels; see {}", path.display());
    let mut pixels = Pixels { path, total: (x1 - x0) * (y1 - y0), orange: 0, near_white: 0 };
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * w + x) * channels;
            let (r, g, b) = (raw[i], raw[i + 1], raw[i + 2]);
            pixels.orange += (r > 180 && g > 70 && g < 140 && b < 80) as usize;
            pixels.near_white += (r > 215 && g > 215 && b > 215) as usize;
        }
    }
    pixels
}

/// With Code on, the Program panel is a real editor, not a gutter squeezed against the view.
fn assert_code_panel(snapshot: &UiSnapshot, mode: &str, picture: &Path) {
    let nodes = snapshot.nodes();
    // Program's `code` widget is CodeView. Its semantic role is TextArea, not a code role.
    let editor = nodes.iter().find(|n| n.visible() && n.role == Role::TextArea && (n.class == "CodeView" || n.path.ends_with("code")))
        .unwrap_or_else(|| panic!("code panel in {mode}: Code is on: the Program panel must have a visible code editor"));
    assert!(editor.bounds.w >= 300.0, "code panel in {mode}: Code is on but Program is only {} layout points wide: {editor:?}; see {}", editor.bounds.w, picture.display());
    let view = nodes.iter().find(|n| n.visible() && n.path.ends_with("viewport")).expect("visible viewport");
    let right = view.bounds.x + view.bounds.w;
    let stray = nodes.iter().find(|n| {
        let b = n.bounds;
        let name = n.name.trim();
        n.visible() && b.x < right && b.x + b.w > right - 40.0
            && b.y < view.bounds.y + view.bounds.h && b.y + b.h > view.bounds.y
            && !name.is_empty() && name.bytes().all(|c| c.is_ascii_digit())
            && name.parse::<u32>().is_ok_and(|number| (6..=35).contains(&number))
    });
    assert!(stray.is_none(), "code panel in {mode}: stray code gutter at the viewport's right edge: {stray:?}; see {}", picture.display());
}

/// Long enough that a collapsed Program panel's gutter shows numbers past 5.
const BLOCK: &str = r##"// The slice tests' part: a small block, sliced
// in a moment at any layer height, wide enough
// to read its colour in the view.
use mod.model.*
use mod.fab.*
let m = mod.model
let block = m.box({size: [40mm 10mm 40mm] radius: 1mm})
object(block)
"##;

#[makepad_test]
fn slicing_is_on_demand_and_the_camera_and_the_colours_stay(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let mut head = String::new();
    wait_until(&app, "Fab tools", || call(&app, "inspect", "{}").map(|v| head = head_of(&v)).is_ok());
    let batch = format!(r#"{{"request_id":"block","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(BLOCK.to_string()).to_json());
    let head = head_of(&call(&app, "script", &batch).expect("script"));
    let edits = "color(\"Orange\", rgb: #xe2672b)\nplace(\"block\", at: [175mm 160mm], color: \"Orange\")";
    let batch = format!(r#"{{"request_id":"place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    call(&app, "place", &batch).expect("place");
    render_mode::wait_shown(&app, "Normal");
    wait_until(&app, "the plate reads Off in Normal", || slice_state(&app, false) == "Off");

    app.locator(Selector::role(Role::Tab).name("Slicer")).select();
    let height = || Selector::role(Role::SpinButton).name(control_name(fab_plate::print_setting("layer_height").unwrap()));
    let old_height = app.locator(height()).wait_visible().snapshot().range.expect("layer height range").now;
    let (first, second) = if (old_height - 0.3).abs() < 0.001 { ("0.25", "0.3") } else { ("0.3", "0.25") };
    // Exercise the editor-width and gutter assertions in both render modes.
    if app.locator(Selector::id("code")).count() == 0 {
        view_menu::pick(&app, "Code");
    }
    let fitted = settled_camera(&app);
    // A wheel zoom changes the fit distance, so an unwanted FrameBox cannot be invisible.
    let (x, y) = app.locator(Selector::id("viewport")).snapshot().bounds.center();
    for _ in 0..3 {
        app.scroll_at(x as f64, y as f64, 0.0, -80.0, None);
    }
    let before = settled_camera(&app);
    let distance = |c: &Camera| c.eye.iter().zip(c.target).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
    assert!((distance(&before) - distance(&fitted)).abs() > 1e-4 || (before.ortho_height - fitted.ortho_height).abs() > 1e-4, "camera: the manual zoom must change the camera's fit");

    // In Normal: the edit commits, the state stays Off and no slice input is sent.
    let from = seq(&app);
    app.locator(height()).set_value(first);
    wait_until(&app, "the edit to commit", || committed(&app, first));
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(500) {
        assert_eq!(slice_state(&app, false), "Off", "the state must stay Off after an edit in Normal");
        std::thread::sleep(Duration::from_millis(50));
    }
    let sent: Vec<String> = app.log_since(from).expect("the app's log").lines.into_iter().filter(|l| l.contains("fab live edit:")).collect();
    assert!(sent.is_empty(), "an edit in Normal sent a slice input: {sent:?}");
    let normal = capture(&app);
    let normal_widgets = app.widget_snapshot();

    // Opening Sliced slices right away.
    render_mode::pick(&app, "Sliced");
    assert_stays(&app, &before, "the switch to Sliced slices", true, None);
    let sliced = capture(&app);
    let sliced_widgets = app.widget_snapshot();
    assert!(normal.orange > 0, "colour: Normal must show the part's orange design colour; see {}", normal.path.display());
    assert!(sliced.near_white * 100 < sliced.total * 3,
        "colour: Sliced's toolpaths have {} near-white pixels out of {} (must be below 3%); see {}",
        sliced.near_white, sliced.total, sliced.path.display());
    assert_code_panel(&normal_widgets, "Normal", &normal.path);
    assert_code_panel(&sliced_widgets, "Sliced", &sliced.path);
    // The printed surface draws each print colour's spool, else (no printer linked) its own.
    app.locator(Selector::id("show_surface")).select();
    app.locator(Selector::id("show_surface")).wait_checked(true);
    let mut surface = capture(&app);
    wait_until(&app, "the printed surface colour", || {
        surface = capture(&app);
        surface.orange * 4 >= normal.orange
    });
    assert!(surface.orange * 4 >= normal.orange,
        "colour: no printer linked: Sliced's printed surface must show the print colour's own orange, but has {} orange pixels, less than 25% of Normal's {}; see {} and {}",
        surface.orange, normal.orange, normal.path.display(), surface.path.display());
    app.locator(Selector::id("show_paths")).select();
    app.locator(Selector::id("show_paths")).wait_checked(true);

    let from = seq(&app);
    app.locator(height()).set_value(second);
    // The commit lands first: a slice of the committed plate then follows, and it is that one
    // the view settles on (a late commit would otherwise start one after the look below).
    wait_until(&app, "the layer-height edit in Sliced to commit", || committed(&app, second));
    assert_stays(&app, &before, "a layer-height edit reslices in Sliced", true, Some(from));

    render_mode::pick(&app, "Normal");
    assert_stays(&app, &before, "the switch back to Normal", false, None);

    // In Normal again: an edit reads Off, and Export asks for its own slice and writes it.
    app.locator(height()).set_value(first);
    wait_until(&app, "the edit in Normal to read Off", || slice_state(&app, false) == "Off" && committed(&app, first));
    // Export at once: the edited plate evaluates under its slice, which goes on with the new plate
    export_print::request(&app);
    // In Normal the Slice editor can be opened from the area menu to read the result.
    app.locator(Selector::role(Role::Button).name("Slicer: area menu")).wait_visible().click_at_center();
    app.locator(Selector::role(Role::MenuItem).name("Slice")).wait_visible().press();
    wait_until(&app, "Export to slice the plate in Normal and write it", || {
        assert_camera(&app, &before, "Export in Normal");
        slice_state(&app, false) == "Sliced" && app.widget_snapshot().nodes().iter().any(|n| n.visible() && n.node.name.contains(" Wrote "))
    });
}

/// A part made from a distance field (it has no exact parts to refine), tall enough that a
/// fine layer height's slice takes a moment.
const TOWER: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let tower = m.blend([
m.box({size: [30mm 64mm 30mm] radius: 2mm})
m.sphere({radius: 12mm at: [0 16mm 0]})
m.sphere({radius: 12mm at: [0 40mm 0]})
m.sphere({radius: 10mm at: [0 60mm 0]})
], 4mm)
object(tower)
"##;

/// The exact-detail level (mm) the app refines exact parts to at camera `c` (`exact_detail_of`,
/// the view's 40° field of view).
fn exact_level(c: &Camera) -> f64 {
    let height = if c.ortho { c.ortho_height } else { 2.0 * c.eye.iter().zip(c.target).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt() * 20f64.to_radians().tan() };
    [0.0125, 0.025, 0.05, 0.1].into_iter().find(|level| 0.5 * height / 1000.0 <= *level).unwrap_or(0.1)
}

/// Wheel steps of `wheel` at `at` until the view's exact-detail level changes: (from, to).
fn zoom_across(app: &TestApp, at: (f64, f64), wheel: f64) -> (f64, f64) {
    let from = exact_level(&camera(app));
    let mut to = from;
    let mut wheeled: Option<Instant> = None;
    wait_until(app, "a zoom to change the exact-detail level", || {
        if wheeled.is_none_or(|t| t.elapsed() >= Duration::from_millis(100)) {
            app.scroll_at(at.0, at.1, 0.0, wheel, None);
            wheeled = Some(Instant::now());
        }
        to = exact_level(&camera(app));
        to != from
    });
    (from, to)
}

/// The tower on the plate, Sliced at each layer height in turn until `during` came while it
/// sliced (the slice's state right after it still reads a percentage); the slice must go on to
/// Sliced each time. Nothing else may restart a slice that `during` stopped: each height is set
/// in Normal (nothing slices there), the model worker is idle and `set_up` done before Sliced
/// slices it, and while it slices the test asks only the UI thread (`fab.slice`, the viewport's
/// camera) and what `during` asks.
fn while_it_slices(app: &TestApp, what: &str, mut set_up: impl FnMut(), mut during: impl FnMut()) {
    let mut head = String::new();
    wait_until(app, "Fab tools", || call(app, "inspect", "{}").map(|v| head = head_of(&v)).is_ok());
    let batch = format!(r#"{{"request_id":"tower","expected":{head:?},"script":"tower.cad.splash","source":{}}}"#, Value::Str(TOWER.to_string()).to_json());
    let head = head_of(&call(app, "script", &batch).expect("script"));
    let batch = format!(r#"{{"request_id":"place","expected":{head:?},"edits":{}}}"#, Value::Str("place(\"tower\", at: [175mm 160mm])".to_string()).to_json());
    call(app, "place", &batch).expect("place");
    app.locator(Selector::role(Role::Tab).name("Slicer")).select();
    let height = || Selector::role(Role::SpinButton).name(control_name(fab_plate::print_setting("layer_height").unwrap()));
    app.locator(height()).wait_visible();
    let mut landed = false;
    for (round, layer) in ["0.08", "0.1", "0.08", "0.1"].into_iter().enumerate() {
        render_mode::wait_shown(app, "Normal");
        app.locator(height()).set_value(layer);
        wait_until(app, "the layer height to commit", || committed(app, layer));
        // Its answer comes after the work the commit queued (the print setup's rebuild).
        call(app, "inspect", "{}").expect("inspect");
        set_up();
        render_mode::pick(app, "Sliced");
        let mut state = String::new();
        wait_until(app, "Sliced to slice", || {
            state = slice_state(app, true);
            state.ends_with('%') || state == "Sliced"
        });
        if state.ends_with('%') {
            let started = Instant::now();
            during();
            let state = slice_state(app, true);
            landed |= state.ends_with('%');
            println!("[on-demand-slice] round {round}: {layer} mm layers: {what} took {:.0} ms, the slice at {state:?}", started.elapsed().as_secs_f64() * 1e3);
        }
        app.try_wait_until_within(HANG, || (slice_state(app, true) != "Sliced").then(|| format!("waiting for the slice after {what}"))).unwrap_or_else(|why| {
            let lines = app.log_since(0).map(|t| t.lines).unwrap_or_default();
            let tail: Vec<&String> = lines.iter().filter(|l| ["fab live input", "fab live edit", "fab live work", "fab live cancel"].iter().any(|w| l.contains(w))).rev().take(16).collect();
            panic!("{why}; timed out waiting for the slice after {what}: it reads {:?}; the log's last lines, newest first: {tail:#?}", slice_state(app, true));
        });
        if landed {
            break;
        }
        render_mode::pick(app, "Normal");
    }
    assert!(landed, "every {what} came after its slice was done: the tower's slice must be longer");
}

/// A zoom while it slices changes the view's exact-detail level: display work that, with no
/// exact part on the plate, shows nothing new. It never stops the slice.
#[makepad_test]
fn a_zoom_while_it_slices_never_stops_the_slice(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let (x, y) = app.locator(Selector::id("viewport")).snapshot().bounds.center();
    let at = (x as f64, y as f64);
    let wheel = std::cell::Cell::new(0.0);
    while_it_slices(&app, "a zoom across a detail level", || {
        // Just past a level's edge (in, else out from the finest level), so that one wheel step
        // back crosses it while it slices.
        wheel.set(if exact_level(&camera(&app)) > 0.0125 { -80.0 } else { 80.0 });
        zoom_across(&app, at, wheel.get());
        settled_camera(&app);
    }, || {
        let (from, to) = zoom_across(&app, at, -wheel.get());
        println!("[on-demand-slice] the level went from {from} to {to} mm");
    });
}

/// A build that fails while it slices (a script that does not evaluate) stops the slice when it
/// is queued and leaves the plate as it was: it ends with an error and no new plate, so only the
/// end of the job itself lets the slice go on (the slice's hold; its Done resumes nothing).
#[makepad_test]
fn a_failed_build_while_it_slices_lets_the_slice_go_on(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let head = std::cell::RefCell::new(String::new());
    while_it_slices(&app, "a failed build", || *head.borrow_mut() = head_of(&call(&app, "inspect", "{}").expect("inspect")), || {
        let broken = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nobject(m.no_such_shape({size: 10mm}))\n";
        let batch = format!(r#"{{"request_id":"broken","expected":{:?},"script":"tower.cad.splash","source":{}}}"#, head.borrow(), Value::Str(broken.to_string()).to_json());
        let reply = call(&app, "script", &batch).expect("script answers");
        assert!(reply.field("error").and_then(|f| f.value.string()).is_some(), "a script that does not evaluate must fail: {reply:?}");
    });
    let after = head_of(&call(&app, "inspect", "{}").expect("inspect"));
    assert_eq!(after, *head.borrow(), "a failed build must leave the document as it was");
}

/// An exact part, a drum: its mesh follows the chord it is made at.
const DRUM: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let drum = m.brep.cylinder({radius: 18mm height: 24mm})
object(drum)
"##;

/// What a zoom must leave as it is: the model's evaluations so far, the plate part's
/// triangles, and the live slice's (input, served) revisions.
fn untouched(app: &TestApp) -> (f64, f64, (f64, f64)) {
    let model = call(app, "inspect", r#"{"program":false}"#).expect("inspect");
    let number = |n: &Node, k: &str| n.field(k).and_then(|f| f.value.number()).unwrap_or_else(|| panic!("no {k}"));
    let parts = model.field("plate").and_then(|f| f.value.field("parts")).and_then(|f| f.value.items()).expect("plate parts");
    assert_eq!(parts.len(), 1, "one part on the plate");
    let slice = call(app, "slice", "{}").expect("slice");
    (number(&model, "evaluations"), number(&parts[0], "triangles"), (number(&slice, "revision"), number(&slice, "served")))
}

/// A zoom across the view's detail levels on a plate with an exact part changes only how the
/// view shows it: no evaluation, the same plate (its part's mesh is the model's own chord), and
/// the slice keeps its revision and its plan (its tolerance is the job's).
#[makepad_test]
fn a_zoom_never_rebuilds_or_reslices_an_exact_part(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let mut head = String::new();
    wait_until(&app, "Fab tools", || call(&app, "inspect", "{}").map(|v| head = head_of(&v)).is_ok());
    let batch = format!(r#"{{"request_id":"drum","expected":{head:?},"script":"drum.cad.splash","source":{}}}"#, Value::Str(DRUM.to_string()).to_json());
    let head = head_of(&call(&app, "script", &batch).expect("script"));
    let batch = format!(r#"{{"request_id":"place","expected":{head:?},"edits":{}}}"#, Value::Str("place(\"drum\", at: [175mm 160mm])".to_string()).to_json());
    call(&app, "place", &batch).expect("place");
    let edges = call(&app, "edges", r#"{"object":"drum/drum"}"#).expect("edges");
    assert_eq!(edges.field("exact").and_then(|f| f.value.ident()), Some("true"), "the drum is an exact body: {edges:?}");
    render_mode::pick(&app, "Sliced");
    wait_until(&app, "the drum to be Sliced", || slice_state(&app, true) == "Sliced");
    // Sliced shows with the toolpaths; the slice's checks go on after them, and only their end
    // serves the revision. What the zoom must leave is the slice served whole.
    wait_until(&app, "the drum's slice to be served", || {
        let (_, _, (revision, served)) = untouched(&app);
        revision == served
    });
    settled_camera(&app);
    let before = untouched(&app);
    let (x, y) = app.locator(Selector::id("viewport")).snapshot().bounds.center();
    // Two levels one way and back (out first from the finer ones).
    let wheel = if exact_level(&camera(&app)) > 0.025 { -80.0 } else { 80.0 };
    let mut levels = Vec::new();
    for wheel in [wheel, wheel, -wheel, -wheel] {
        let (from, to) = zoom_across(&app, (x as f64, y as f64), wheel);
        levels.push((from, to));
        settled_camera(&app);
    }
    // Its answer comes after the model worker's work the zoom queued.
    call(&app, "inspect", "{}").expect("inspect");
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(500) {
        assert_eq!(slice_state(&app, true), "Sliced", "a zoom ({levels:?}) must not reslice");
        std::thread::sleep(Duration::from_millis(25));
    }
    let after = untouched(&app);
    assert_eq!(before, after, "a zoom across the levels {levels:?} must leave (evaluations, the part's triangles, (slice revision, served)) as they were");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

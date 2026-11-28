//! A part made from a distance field is drawn, checked, sliced and moved from its field, and it
//! stays a field:
//!
//! - nothing in normal use meshes it (only an export or Mesh shading would): the part's height
//!   handle is dragged and released: one evaluation (the commit), the part drawn by the engine
//!   the whole time (never as a mesh), no "Rebuilding..." (its field view is current all along);
//!   the part is moved (the Part panel's position field); a part is selected, the workspaces are
//!   switched and the plate is sliced: no evaluation, and no field is meshed (`fab.inspect`'s
//!   `field_meshes`) across all of it;
//! - an idle view draws no frames; meshes are only the person's choice, kept per machine
//!   (`FAB_VIEW_PREFS`), and a chosen field look stays.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/part_editor.rs"]
mod part_editor;
mod common;
#[path = "common/wait.rs"]
mod idle_wait;
#[path = "common/workspace_row.rs"]
mod workspace_row;
#[path = "common/parts_tab.rs"]
mod parts_tab;
use idle_wait::wait_poll;

use makepad_script::syntax::{self, Node};
use makepad_strict_json::Value;
use makepad_test::{makepad_test, run_with_config, KeyCode, KeyModifiers, Role, Selector, TestApp, TestConfig};
use std::time::{Duration, Instant};

fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let argv = vec!["call".to_string(), format!("fab.{tool}"), args.to_string()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("fab.{tool}: {e:?}"))?;
    lines.iter().find(|l| l.contains("\"Result\"")).cloned().ok_or_else(|| format!("fab.{tool} returned no result: {lines:?}"))
}

fn text(line: &str) -> String {
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("result line is JSON");
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    r.get("text").and_then(Value::as_str).unwrap_or_default().to_string()
}

fn head_of(reply: &str) -> String {
    let reply = syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

fn inspect(app: &TestApp) -> Node {
    syntax::parse_value(&text(&call(app, "inspect", r#"{"program":false}"#).expect("inspect"))).expect("a Splash reply")
}

/// The Fab tools are linked on the window's first frames: the first inspect reply.
fn wait_tools(app: &TestApp) -> String {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait_poll(app, || call(app, "inspect", "{}").map(|line| text(&line)).map_err(|e| format!("the tools never appeared: {e}")))
}

/// A script and its placement, from the head the last reply gave; returns the new head.
fn place_script(app: &TestApp, head: &str, request: &str, file: &str, source: &str, edits: &str) -> String {
    let batch = format!(r#"{{"request_id":"{request}-script","expected":{head:?},"script":{file:?},"source":{}}}"#, Value::Str(source.to_string()).to_json());
    let head = head_of(&text(&call(app, "script", &batch).expect("script")));
    let batch = format!(r#"{{"request_id":"{request}-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    text(&call(app, "place", &batch).expect("place"));
    head_of(&text(&call(app, "inspect", r#"{"program":false}"#).expect("inspect")))
}

/// (model evaluations, fields meshed) so far.
fn counts(app: &TestApp) -> (f64, f64) {
    let v = inspect(app);
    let n = |k: &str| v.field(k).and_then(|f| f.value.number()).unwrap_or_else(|| panic!("fab.inspect has no {k}"));
    (n("evaluations"), n("field_meshes"))
}

/// A declared parameter's value in mm (fab.params).
fn param(app: &TestApp, key: &str) -> f64 {
    let v = syntax::parse_value(&text(&call(app, "params", "{}").expect("params"))).expect("a Splash reply");
    for part in v.field("parts").and_then(|f| f.value.items()).expect("parts") {
        for p in part.field("params").and_then(|f| f.value.items()).expect("params") {
            if p.field("key").and_then(|f| f.value.string()) == Some(key) {
                return p.field("value").and_then(|f| f.value.number()).expect("a value") * 1000.0;
            }
        }
    }
    panic!("no parameter {key}");
}

/// A part's x on the plate (mm) and whether fab.inspect says it is a field part.
fn part_x(app: &TestApp, name: &str) -> (f64, bool) {
    let v = inspect(app);
    let parts = v.field("plate").and_then(|p| p.value.field("parts")).and_then(|f| f.value.items()).expect("plate parts");
    let p = parts.iter().find(|p| p.field("name").and_then(|f| f.value.string()) == Some(name)).unwrap_or_else(|| panic!("no part {name}"));
    let at = p.field("at").and_then(|f| f.value.items()).expect("at");
    (at[0].number().expect("x") * 1000.0, p.field("field").is_some())
}

fn log_lines(app: &TestApp, from: u64) -> Vec<String> {
    app.log_since(from).expect("the app's log").lines
}

fn count(app: &TestApp, from: u64, needle: &str) -> usize {
    log_lines(app, from).iter().filter(|l| l.contains(needle)).count()
}

fn seq(app: &TestApp) -> u64 {
    app.log_since(0).map(|t| t.last_seq).unwrap_or(0)
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    app.try_wait_until_within(HANG, || (!ok()).then(|| format!("waiting for {what}"))).unwrap_or_else(|why| {
        for l in log_lines(app, 0).iter().filter(|l| l.contains("fab")).rev().take(30).collect::<Vec<_>>().into_iter().rev() {
            println!("[app] {l}");
        }
        panic!("{why}; timed out waiting for {what}");
    });
}

/// The evaluations stop: the counts hold still for a moment.
fn settled(app: &TestApp) -> (f64, f64) {
    let mut last = counts(app);
    let mut since = Instant::now();
    wait_poll(app, || {
        let now = counts(app);
        if now != last {
            last = now;
            since = Instant::now();
        } else if since.elapsed() > Duration::from_millis(500) {
            return Ok(now);
        }
        Err(format!("the evaluations never settled: {now:?}"))
    })
}

fn rebuilding(app: &TestApp) -> bool {
    app.widget_snapshot().nodes().iter().any(|w| w.visible() && w.node.role == Role::Status && w.node.name.starts_with("Rebuilding"))
}

fn pick_part(app: &TestApp, name: &str) {
    parts_tab::open(&app);
    app.locator(Selector::role(Role::TreeItem).name(name)).wait_visible().click();
    workspace_row::press(app, "3D modeling");
    app.locator(Selector::id("t_shape")).wait_visible().click();
}

#[makepad_test]
fn field_parts_are_never_meshed_in_normal_use(app: TestApp) {
    let head = head_of(&wait_tools(&app));
    // One small field part (a blend of a box and a sphere) with a height parameter and its handle.
    place_script(&app, &head, "field", "field.cad.splash", FIELD, r#"place("field", at: [222mm 165mm], name: "Field part")"#);
    wait_until(&app, "the part drawn by the engine", || count(&app, 0, "drawn by the engine") > 0);
    assert!(part_x(&app, "Field part").1, "the part is a field part");
    let (e0, m0) = settled(&app);
    assert_eq!(m0, 0.0, "placing the part meshed no field");
    let log0 = seq(&app);

    // ---- a field part's parameter: drag its handle and let go ----
    pick_part(&app, "Field part");
    app.locator(Selector::id("t_shape")).wait_checked(true);
    let height = Selector::role(Role::Slider).name("Height handle");
    app.locator(height.clone()).wait_visible();
    let h0 = param(&app, "field/h");
    app.locator(height).drag_by(0.0, -60.0);
    let mut busy = false;
    wait_until(&app, "the part to grow", || {
        busy |= rebuilding(&app);
        param(&app, "field/h") > h0 + 2.0
    });
    let (e1, m1) = settled(&app);
    busy |= rebuilding(&app);
    assert_eq!(e1 - e0, 1.0, "the release evaluates once (no draft previews of a field part)");
    assert_eq!(m1, 0.0, "the drag and its evaluation meshed no field");
    assert!(!busy, "a field part never shows Rebuilding: its field view is current");
    let meshed: Vec<String> = log_lines(&app, log0).into_iter().filter(|l| l.contains("fab vp sdf") && (l.contains("cannot draw its field") || l.contains("look: mesh"))).collect();
    assert!(meshed.is_empty(), "the part stays drawn as its field: {meshed:?}");

    // ---- a field part moves ----
    pick_part(&app, "Field part");
    part_editor::open(&app);
    let (x0, _) = part_x(&app, "Field part");
    app.locator(Selector::role(Role::SpinButton).name("X")).wait_visible().set_value(x0.round() + 15.0);
    wait_until(&app, "the part to move", || (part_x(&app, "Field part").0 - (x0.round() + 15.0)).abs() < 0.5);
    let (e2, m2) = settled(&app);
    assert_eq!(m2, 0.0, "a move meshed no field");

    // ---- select, switch the workspaces, slice: nothing evaluates (the count only grows, so
    // one reading at the end covers every step) ----
    parts_tab::open(&app);
    app.locator(Selector::role(Role::TreeItem).name("Field part")).wait_visible().click();
    for ws in ["Monitor", "Mechanics", "3D modeling"] {
        workspace_row::press(&app, ws);
    }
    let (e_ws, m_ws) = settled(&app);
    assert_eq!((e_ws, m_ws), (e2, 0.0), "selecting parts and switching workspaces evaluates and meshes nothing");
    let from = seq(&app);
    let primary = if cfg!(target_os = "macos") { KeyModifiers { logo: true, ..Default::default() } } else { KeyModifiers { control: true, ..Default::default() } };
    app.press_key_with_modifiers(KeyCode::KeyR, primary);
    wait_until(&app, "the slice", || common::slice_state(&app) == "Sliced");
    let (e3, m3) = settled(&app);
    assert_eq!(e3, e2, "slicing evaluates nothing");
    assert_eq!(m3, 0.0, "nothing meshed a field");
    assert_eq!(count(&app, from, "cannot draw its field"), 0, "the field parts stay drawn as fields");
}

const FIELD: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let h = param(8mm, min: 4mm, max: 30mm, step: 0.5mm, label: "Height")
let look = m.blend([m.box({size: [30mm h 8mm] radius: 1mm}) m.sphere({radius: 7mm at: [10mm 0 6mm]})], 3mm)
handle(h, Arrow{from: [16mm 0 0] to: [16mm h 0]})
object(look)
"##;

const PART: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let look = m.blend([m.box({size: [30mm 8mm 8mm] radius: 1mm}) m.sphere({radius: 7mm at: [10mm 0 6mm]})], 3mm)
object(look)
"##;

/// The app runs with a tiny frame budget and its own prefs file (the test sets both).
#[test]
fn the_field_look_is_safe_on_the_gpu() {
    let dir = std::env::temp_dir().join(format!("fab-sdf-safety-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let prefs = dir.join("view.prefs");
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "the_field_look_is_safe_on_the_gpu").unwrap();
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let r = run_with_config(config, |app| safety(app, &prefs));
    let _ = std::fs::remove_dir_all(&dir);
    r.unwrap();
}

fn safety(app: TestApp, prefs: &std::path::Path) {
    let head = head_of(&wait_tools(&app));
    place_script(&app, &head, "safety", "look.cad.splash", PART, r#"place("look")"#);
    wait_until(&app, "the part drawn by the engine", || count(&app, 0, "drawn by the engine") > 0);

    // At rest: no frames. Wait for the first trace's frames to stop, then watch a moment.
    let mut frames = count(&app, 0, "fab vp sdf frame:");
    wait_until(&app, "the first frames to stop", || {
        std::thread::sleep(Duration::from_millis(300));
        let now = count(&app, 0, "fab vp sdf frame:");
        let quiet = now == frames;
        frames = now;
        quiet
    });
    let from = seq(&app);
    std::thread::sleep(Duration::from_millis(1200));
    let idle = count(&app, from, "fab vp sdf frame:");
    assert!(idle <= 2, "an idle view drew {idle} frames");

    // The person's own choice of meshes: kept per machine.
    let choose = |look: &str| {
        app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name("Shading")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name(look)).wait_visible().press();
    };
    let saved = |want: &str| {
        wait_until(&app, &format!("{want} to be saved"), || std::fs::read_to_string(prefs).unwrap_or_default().contains(want));
    };
    let from = seq(&app);
    choose("Mesh");
    wait_until(&app, "the mesh look", || count(&app, from, "fab vp sdf look: mesh") > 0);
    saved("look=Mesh");
    // and back to the field: judged afresh at full quality
    let from = seq(&app);
    choose("Field");
    wait_until(&app, "the field look", || count(&app, from, "fab vp sdf look: field") > 0);
    saved("look=Field");
    let from = seq(&app);
    for _ in 0..8 {
        app.locator(Selector::id("viewport")).drag_by(6.0, 0.0);
    }
    assert_eq!(count(&app, from, "fab vp sdf look: mesh"), 0, "a chosen field look stays");
}

/// An imported STL carved by a field is traced into tiles from its exact mesh leaf.
#[makepad_test]
fn an_imported_stl_carves_as_a_field(app: TestApp) {
    wait_tools(&app);
    // A 20 mm box through the real STL writer, file import and script tools.
    let verts = (0..8).map(|i| [if i & 1 == 0 { -10.0 } else { 10.0 }, if i & 2 == 0 { -10.0 } else { 10.0 }, if i & 4 == 0 { 0.0 } else { 20.0 }]).collect();
    let tris = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]].into_iter().flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect();
    let mesh = makepad_fab_slicer::mesh::Mesh { verts, tris, paint: Vec::new() };
    let path = std::env::temp_dir().join("mesh_query_cube.stl");
    std::fs::write(&path, makepad_fab_slice::bench::stl_bytes(&mesh)).unwrap();
    let args = format!(r#"{{"path":{},"new_plate":true}}"#, Value::Str(path.to_string_lossy().into()).to_json());
    let reply = text(&call(&app, "import", &args).expect("STL import"));
    std::fs::remove_file(path).unwrap();
    let head = head_of(&reply);
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet body = m.import(\"mesh_query_cube\")\nlet mesh_query_cube = m.carve(body, m.cylinder({radius: 3mm height: 30mm at: [0 10mm 0]}), 0.5mm, {cell: 0.5mm})\nobject(mesh_query_cube)\n";
    let args = format!(r#"{{"request_id":"mesh-carve","expected":{head:?},"script":"mesh_query_cube.cad.splash","source":{}}}"#, Value::Str(source.into()).to_json());
    head_of(&text(&call(&app, "script", &args).expect("carve script")));
    wait_poll(&app, || {
        if !log_lines(&app, 0).iter().any(|l| l.contains("mesh_query_cube") && l.contains("drawn by the engine")) { Err("the imported carve was never drawn by the engine".to_string()) } else { Ok(()) }
    });
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

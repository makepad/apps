//! Painted toolpaths reach the exported file through the palette and a virtual printer's Jobs
//! tab. A local write failure stays visible on the plate and preserves the slice.

#[path = "common/render_mode.rs"]
mod render_mode;
mod common;
#[path = "common/export_print.rs"]
mod export_print;
#[path = "common/simulated_profiles.rs"]
mod simulated_profiles;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;

use makepad_script::syntax;
use makepad_strict_json::Value;
use makepad_test::{makepad_test, run_with_config, TestConfig, KeyCode, KeyModifiers, Role, Selector, TestApp};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const VASE: &str = include_str!("../resources/vase.cad.splash");

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
    syntax::parse_value(reply).expect("a Splash reply").field("head").and_then(|f| f.value.string()).expect("a head").to_string()
}

fn head(app: &TestApp) -> String {
    head_of(&call(app, "inspect", r#"{"program":false}"#).expect("inspect"))
}

fn seq(app: &TestApp) -> u64 {
    app.log_since(0).map(|t| t.last_seq).unwrap_or(0)
}

fn wait<T>(app: &TestApp, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    wait_poll(app, || f().ok_or_else(|| format!("waiting for {what}")))
}

/// A real drag across the view's middle at `row` of its height.
fn stroke(app: &TestApp, row: f64) {
    let view = app.locator(Selector::id("viewport")).wait_visible().snapshot();
    let (x0, w, y) = (view.x() as f64, view.width() as f64, view.y() as f64 + view.height() as f64 * row);
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect");
    let mouse = |kind: &str, x: f64| {
        let wait = (kind != "move") as u8;
        let argv: Vec<String> = ["mouse".to_string(), format!("k={kind}"), format!("x={x:.1}"), format!("y={y:.1}"), "b=0".into(), format!("wait={wait}")].into();
        makepad_remote::app::run(&client, &argv, &mut |_| true).expect("mouse");
    };
    let at = |i: usize| x0 + w * (0.42 + 0.16 * i as f64 / 24.0);
    mouse("move", at(0));
    mouse("down", at(0));
    for i in 1..=24 {
        mouse("move", at(i));
    }
    mouse("up", at(24));
}

/// What a stroke (or its undo) did to the slice, from the log.
#[derive(Debug, Default)]
struct Reslice {
    /// The newest input's revision.
    revision: Option<u64>,
    /// When the newest input's whole slice came.
    whole: Option<Duration>,
    planned: u64,
    traced: u64,
    /// Inputs sent for it.
    inputs: usize,
}

fn field(line: &str, name: &str) -> Option<u64> {
    let at = line.find(name)? + name.len();
    let digits: String = line[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Watches the log from `from`, up to `secs`, until the newest input has been whole for a moment
/// and the view reads Sliced: an edit's first colour lands twice (its tools are mapped after it).
fn follow(app: &TestApp, from: u64) -> Reslice {
    let started = Instant::now();
    let mut r = Reslice::default();
    let mut read = 0;
    let mut whole_since = None;
    wait_poll(app, || {
        let lines = app.log_since(from).expect("the app's log").lines;
        for line in &lines[read.min(lines.len())..] {
            if let Some(revision) = field(line, "fab live input: revision=") {
                r = Reslice { revision: Some(revision), inputs: r.inputs + 1, ..Default::default() };
                whole_since = None;
            }
            if line.contains("fab live work:") && field(line, "revision=") == r.revision && r.revision.is_some() {
                r.whole = Some(started.elapsed());
                r.traced = field(line, "traced ").unwrap_or(0);
                r.planned = field(line, "planned ").unwrap_or(0);
                whole_since = Some(Instant::now());
            }
        }
        read = lines.len();
        if whole_since.is_some_and(|t| t.elapsed() > Duration::from_millis(500)) && common::slice_state(app) == "Sliced" {
            return Ok(());
        }
        Err("waiting for the latest slice to settle".to_string())
    });
    r
}

/// Read the output named by this editor's status once this export has written it.
fn written(app: &TestApp, panel: &str, status: Selector, asked: SystemTime) -> PathBuf {
    let bounds = app.locator(Selector::role(Role::TabPanel).name(panel)).wait_visible().snapshot().bounds;
    let status = app.locator(status);
    wait(app, "the export's fresh path in the editor status", || {
        let label = status.try_snapshot().ok()?;
        let (x, y) = label.bounds.center();
        if !bounds.contains(x, y) { return None; }
        let (_, path) = label.name.split_once("Wrote ")?;
        let path = PathBuf::from(path.trim());
        std::fs::metadata(&path).and_then(|m| m.modified()).is_ok_and(|m| m >= asked).then_some(path)
    })
}

fn palette_export(app: &TestApp) -> BTreeMap<u32, f64> {
    let asked = SystemTime::now();
    export_print::request(app);
    extrusion(&written(app, "Slice", Selector::raw("path=body.none name~=Wrote"), asked))
}

fn extrusion(wrote: &Path) -> BTreeMap<u32, f64> {
    let bytes = std::fs::read(wrote).expect("the exported file");
    let mut zip = std::io::Cursor::new(bytes);
    let dir = makepad_zip_file::zip_read_central_directory(&mut zip).expect("a package");
    let entry = dir.file_headers.iter().find(|f| f.file_name.ends_with(".gcode")).expect("the package's G-code");
    let gcode = String::from_utf8(entry.extract(&mut zip).expect("G-code")).expect("text");
    let mut by_tool = BTreeMap::new();
    let mut tool = 0;
    for line in gcode.lines() {
        if let Some(t) = line.strip_prefix('T').and_then(|t| t.trim().parse::<u32>().ok()) {
            tool = t;
        } else if line.starts_with("G1") || line.starts_with("G2") || line.starts_with("G3") {
            let e = line.split_whitespace().find_map(|w| w.strip_prefix('E')).and_then(|e| e.parse::<f64>().ok());
            if let Some(e) = e.filter(|e| *e > 0.0) {
                *by_tool.entry(tool).or_default() += e;
            }
        }
    }
    by_tool
}

fn sliced_vase(app: &TestApp, x: u32, y: u32) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait(app, "the fab tools", || call(app, "inspect", "{}").ok());
    let batch = format!(r#"{{"request_id":"vase","expected":"{}","script":"vase.cad.splash","source":{}}}"#, head(app), Value::Str(VASE.to_string()).to_json());
    let h = head_of(&call(app, "script", &batch).expect("script"));
    let edits = Value::Str(format!("color(\"Teal\", rgb: #x2a9d8f)\nplace(\"vase\", object: \"vase\", at: [{x}mm {y}mm], name: \"Vase\", color: \"Teal\")")).to_json();
    call(app, "place", &format!(r#"{{"request_id":"place","expected":"{h}","edits":{edits}}}"#)).expect("place");
    wait(app, "the part", || (common::slice_state(app) == "Off").then_some(()));
    wait(app, "the field part drawn", || app.log_since(0).map(|t| t.lines).unwrap_or_default().iter().any(|l| l.contains("drawn by the engine")).then_some(()));
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).name("Vase")).wait_visible().click();
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Frame Selected")).wait_visible().press();
    render_mode::pick(app, "Sliced");
    wait(app, "the first slice", || (common::slice_state(app) == "Sliced").then_some(()));
}

fn paint(app: &TestApp, row: f64) -> Reslice {
    let from = seq(app);
    let before = head(app);
    stroke(app, row);
    wait(app, "the brush stroke", || (head(app) != before).then_some(()));
    follow(app, from)
}

fn painted(by: &BTreeMap<u32, f64>) -> f64 {
    by.iter().skip(1).map(|(_, e)| *e).sum()
}

#[makepad_test]
fn a_stroke_in_sliced_reslices_around_the_brush_and_export_writes_it(app: TestApp) {
    sliced_vase(&app, 175, 160);
    app.locator(Selector::id("t_paint")).click();
    let first = paint(&app, 0.45);
    println!("[paint-in-sliced] stroke 1: {first:?}");
    assert!(first.whole.is_some(), "the first stroke never reached the slice");
    let one = palette_export(&app);
    println!("[paint-in-sliced] export after stroke 1: {one:?}");

    // The same colour again: the script already reads the strokes file, so nothing but the
    // strokes file's records changes.
    let second = paint(&app, 0.55);
    println!("[paint-in-sliced] stroke 2: {second:?}");
    let two = palette_export(&app);
    println!("[paint-in-sliced] export after stroke 2: {two:?}");

    // Undone, in the view and the file alike.
    let from = seq(&app);
    let primary = if cfg!(target_os = "macos") { KeyModifiers { logo: true, ..Default::default() } } else { KeyModifiers { control: true, ..Default::default() } };
    app.press_key_with_modifiers(KeyCode::KeyZ, primary);
    let undone = follow(&app, from);
    println!("[paint-in-sliced] undo: {undone:?}");
    let three = palette_export(&app);
    println!("[paint-in-sliced] export after the undo: {three:?}");

    assert!(second.whole.is_some(), "a stroke of the same colour never reached the slice");
    assert!(second.traced == 0 && second.planned > 0, "a stroke plans its layers and traces nothing: {second:?}");
    assert!(one.len() > 1, "the first stroke prints in a second tool: {one:?}");
    assert!(painted(&two) > painted(&one) + 1.0, "Export wrote no paint of the second stroke: {one:?} then {two:?}");
    assert!((painted(&three) - painted(&one)).abs() < 0.01, "Export after the undo: {one:?} then {three:?}");
}

fn isolated_config(test: &str) -> (TestConfig, PathBuf) {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), test).unwrap();
    let nonce = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fab-{test}-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    (config, dir)
}

fn jobs_export(app: &TestApp) -> BTreeMap<u32, f64> {
    app.locator(Selector::role(Role::Tab).name("Printers")).press();
    app.locator(Selector::id("jobs_tab")).wait_visible().press();
    let panel = app.locator(Selector::role(Role::TabPanel).name("Printers")).wait_visible().snapshot().bounds;
    let button = app.locator(Selector::id("export"));
    app.wait_until(|| {
        if button.try_snapshot().is_ok() { return None; }
        let (x, y) = panel.center();
        app.scroll_at(x.into(), y.into(), 0.0, f64::from(panel.h) * 0.5, None);
        Some("scrolling to Export in the active printer's Slice section".into())
    });
    assert!(button.snapshot().path.split('.').any(|s| s == "jobs_active"), "Export belongs to the active printer");
    let asked = SystemTime::now();
    button.press();
    extrusion(&written(app, "Printers", Selector::path("status_row.status"), asked))
}

#[test]
fn export_from_virtual_twin_jobs_tab_writes_painted_gcode() {
    use makepad_fab_settings::virtual_printer::{self, Saved, Source};
    let (config, dir) = isolated_config("export_from_virtual_twin_jobs_tab_writes_painted_gcode");
    simulated_profiles::install(&dir);
    let mut saved = Saved::default();
    let twin = saved.printers.iter_mut().find(|p| p.name == "dual-nozzle").unwrap();
    twin.source = Source::Twin { preset: simulated_profiles::MACHINE.into(), model: simulated_profiles::model(), source: None };
    // Load the vase's teal and the brush's red on separate heads so each has its own spool.
    for slot in &mut twin.slots {
        slot.colour = match slot.tray.as_str() { "A1" => Some(0x2a9d8f), "B1" => Some(0xe73f3f), _ => None };
        slot.material = slot.colour.map(|_| "PLA".into());
    }
    saved.picked = Some(format!("virtual:{}", twin.name));
    virtual_printer::save_to(&dir.join(".makepad/fab/virtual_printers.txt"), &saved).unwrap();
    let result = run_with_config(config, |app| {
        // This twin's bed is 100 mm square.
        sliced_vase(&app, 50, 50);
        app.locator(Selector::id("t_paint")).click();
        paint(&app, 0.45);
        let one = jobs_export(&app);
        assert!(one.len() > 1, "the twin's first export prints in two tools: {one:?}");
        paint(&app, 0.55);
        let two = jobs_export(&app);
        assert!(painted(&two) > painted(&one) + 1.0, "the twin exported the second stroke: {one:?} then {two:?}");
    });
    std::fs::remove_dir_all(dir).unwrap();
    result.unwrap();
}

#[test]
fn palette_export_failure_shows_in_normal_without_slice_editor() {
    let (config, dir) = isolated_config("palette_export_failure_shows_in_normal_without_slice_editor");
    let documents = dir.join("Documents");
    std::fs::create_dir(&documents).unwrap();
    let blocked = documents.join("Fab");
    std::fs::write(&blocked, b"").unwrap();
    let result = run_with_config(config, |app| {
        sliced_vase(&app, 175, 160);
        render_mode::pick(&app, "Normal");
        app.locator(Selector::role(Role::Tab).name("Slice")).wait_hidden();
        export_print::request(&app);
        let failure = app.locator(Selector::raw("path=slice_failed_text name~=\"Export failed:\"")).wait_visible().snapshot();
        let path = blocked.join("vase.gcode.3mf");
        let why = std::fs::metadata(&path).unwrap_err();
        assert_eq!(failure.name, format!("Export failed: {}: {why}", path.display()), "the strip gives only the local file and write error");
        assert_eq!(common::slice_state(&app), "Sliced", "a failed export preserves the slice");
    });
    std::fs::remove_dir_all(dir).unwrap();
    result.unwrap();
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

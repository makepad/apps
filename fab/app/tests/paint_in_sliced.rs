//! The brush in Sliced, in a real window, one boot on one small vase: each stroke of the same
//! colour reaches the slice and only the layers it paints are planned again; Export after it
//! writes the new paint; an undo takes it out of both.

#[path = "common/render_mode.rs"]
mod render_mode;
mod common;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;

use makepad_script::syntax;
use makepad_strict_json::Value;
use makepad_test::{makepad_test, KeyCode, KeyModifiers, Role, Selector, TestApp};
use std::collections::BTreeMap;
use std::path::PathBuf;
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

/// Scroll the Job panel to its foot, where Export sits under the plate's colours (Parameters
/// shares the column, and the colours can push Export below the fold).
fn job_to_foot(app: &TestApp) {
    let panel = app.locator(Selector::role(Role::TabPanel).name("Job")).wait_visible().snapshot().bounds;
    let bar = app.widget_snapshot().nodes().into_iter().find(|n| {
        let (x, y) = n.bounds.center();
        n.role == Role::ScrollBar && n.visible() && panel.contains(x, y)
    });
    if let Some((bar, range)) = bar.and_then(|b| Some((b.id.short(), b.range?))) {
        app.locator(Selector::raw(format!("id={bar}"))).set_value(range.max);
    }
}

/// Export on the Job tab, once its file is written: the printed G-code's extrusion per tool.
fn export(app: &TestApp) -> BTreeMap<u32, f64> {
    app.locator(Selector::role(Role::Tab).name("Job")).click();
    job_to_foot(app);
    let asked = SystemTime::now();
    app.locator(Selector::id("export")).wait_visible().click();
    let wrote = wait(app, "Export to write the plate", || {
        app.widget_snapshot().nodes().iter().filter(|n| n.visible()).find_map(|n| n.node.name.split_once(" Wrote ").map(|(_, p)| PathBuf::from(p.trim())))
            .filter(|p| std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|m| m >= asked))
    });
    let bytes = std::fs::read(&wrote).expect("the exported file");
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

#[makepad_test]
fn a_stroke_in_sliced_reslices_around_the_brush_and_export_writes_it(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait(&app, "the fab tools", || call(&app, "inspect", "{}").ok());
    let batch = format!(r#"{{"request_id":"vase","expected":"{}","script":"vase.cad.splash","source":{}}}"#, head(&app), Value::Str(VASE.to_string()).to_json());
    let h = head_of(&call(&app, "script", &batch).expect("script"));
    let edits = Value::Str("color(\"Teal\", rgb: #x2a9d8f)\nplace(\"vase\", object: \"vase\", at: [175mm 160mm], name: \"Vase\", color: \"Teal\")".to_string()).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"place","expected":"{h}","edits":{edits}}}"#)).expect("place");
    wait(&app, "the part", || (common::slice_state(&app) == "Off").then_some(()));
    wait(&app, "the field part drawn", || app.log_since(0).map(|t| t.lines).unwrap_or_default().iter().any(|l| l.contains("drawn by the engine")).then_some(()));
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).name("Vase")).wait_visible().click();
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Frame Selected")).wait_visible().press();
    render_mode::pick(&app, "Sliced");
    wait(&app, "the first slice", || (common::slice_state(&app) == "Sliced").then_some(()));
    app.locator(Selector::id("t_paint")).click();

    let from = seq(&app);
    let h = head(&app);
    stroke(&app, 0.45);
    wait(&app, "the first stroke", || (head(&app) != h).then_some(()));
    let first = follow(&app, from);
    println!("[paint-in-sliced] stroke 1: {first:?}");
    assert!(first.whole.is_some(), "the first stroke never reached the slice");
    let one = export(&app);
    println!("[paint-in-sliced] export after stroke 1: {one:?}");

    // The same colour again: the script already reads the strokes file, so nothing but the
    // strokes file's records changes.
    let from = seq(&app);
    let h = head(&app);
    stroke(&app, 0.55);
    wait(&app, "the second stroke", || (head(&app) != h).then_some(()));
    let second = follow(&app, from);
    println!("[paint-in-sliced] stroke 2: {second:?}");
    let two = export(&app);
    println!("[paint-in-sliced] export after stroke 2: {two:?}");

    // Undone, in the view and the file alike.
    let from = seq(&app);
    let primary = if cfg!(target_os = "macos") { KeyModifiers { logo: true, ..Default::default() } } else { KeyModifiers { control: true, ..Default::default() } };
    app.press_key_with_modifiers(KeyCode::KeyZ, primary);
    let undone = follow(&app, from);
    println!("[paint-in-sliced] undo: {undone:?}");
    let three = export(&app);
    println!("[paint-in-sliced] export after the undo: {three:?}");

    assert!(second.whole.is_some(), "a stroke of the same colour never reached the slice");
    assert!(second.traced == 0 && second.planned > 0, "a stroke plans its layers and traces nothing: {second:?}");
    assert!(one.len() > 1, "the first stroke prints in a second tool: {one:?}");
    let painted = |by: &BTreeMap<u32, f64>| by.iter().skip(1).map(|(_, e)| *e).sum::<f64>();
    assert!(painted(&two) > painted(&one) + 1.0, "Export wrote no paint of the second stroke: {one:?} then {two:?}");
    assert!((painted(&three) - painted(&one)).abs() < 0.01, "Export after the undo: {one:?} then {three:?}");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

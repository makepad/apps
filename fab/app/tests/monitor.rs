//! The Monitor workspace with a SIMULATED printer (`FAB_PRINTER_SIM`, a file of status reports
//! parsed like the printer's own; no real printer is touched). The installed-profile test presses
//! Print through the real safety gate, commits and queues a job, then starts it on the simulator.
//! Other sessions cover:
//!
//! - a job Print committed is on the disk (as after a restart) and synthetic reports step the
//!   printer through it: in Monitor the view follows (the playback bar's "Layer n / m" is the
//!   printer's layer at every report); scrubbing looks ahead or back (a later report leaves the
//!   view alone, and it says where the printer is), "Back to live" catches up; Monitor's view is
//!   its own (Design's slider and an edit that changes the plate's slice don't reach it, nor it
//!   them); a job Fab has no copy of gets the card that says so;
//! - jobs on the disk and an empty plate: Monitor comes back on the printer's running job, its
//!   layer, its elapsed time and its look-ahead; its end reaches the disk, the queued job stays
//!   queued, and after another start it waits for a press, which asks about the plate first;
//! - calibration in the printer's Settings makes the clearance test part, which becomes the
//!   plate and slices (the slice's layers show), and a result entered for it is stored for the
//!   printer and filament (`FAB_CALIBRATION_DIR`);
//! - in another session, every press of the top bar's Print answers: the confirm card opens at
//!   the top of the Printer panel from each workspace, says how far the slice is and what stops
//!   Start print as the printer's reports change, and only Cancel closes it; printability errors
//!   alone stop it until "Print anyway" is ticked, and the tick is off on every opening;
//! - a job printed anyway says which printability errors it went with, also after restarts.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

mod common;
#[path = "common/simulated_profiles.rs"]
mod simulated_profiles;

#[path = "common/render_mode.rs"]
mod render_mode;
#[path = "common/workspace_row.rs"]
mod workspace_row;
use fab::print::jobs::wall_now;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn calibration_item(app: &TestApp, selector: Selector) -> makepad_test::Locator {
    calibration_item_toward(app, selector, true)
}

fn calibration_item_toward(app: &TestApp, selector: Selector, down: bool) -> makepad_test::Locator {
    let panel = app.locator(Selector::role(Role::TabPanel).name("Printers")).snapshot().bounds;
    let query = selector.as_query();
    let item = app.locator(selector);
    app.wait_until(|| {
        if item.try_snapshot().is_ok() { return None; }
        app.scroll_at(f64::from(panel.x + panel.w * 0.5), f64::from(panel.y + panel.h * 0.5), 0.0, f64::from(panel.h * if down { 0.5 } else { -0.5 }), None);
        Some(format!("scrolling the printer's Settings to {query}"))
    });
    // The other steps have the same buttons in their folded bodies. Act on the exact
    // control we found on screen in the open step.
    app.locator(Selector::path(item.snapshot().node.path))
}

fn open_clearance(app: &TestApp) {
    calibration_item(app, Selector::role(Role::DisclosureButton).name("Settings")).expand();
    calibration_item(app, Selector::role(Role::DisclosureButton).name("Clearance")).expand();
}

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
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

fn names(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible()).map(|w| w.node.name).collect()
}

/// The playback bar's "Layer n / m" label.
fn layer_label(app: &TestApp) -> Option<(u32, u32)> {
    // The job card also says "Layer n / m". Only the preview label includes its height.
    names(app).into_iter().filter(|t| t.ends_with(" mm")).find_map(|t| {
        let rest = t.strip_prefix("Layer ")?;
        let mut it = rest.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty());
        Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
    })
}

/// Waits for `look` on the app's budget ([`TestApp::wait_until`]); until then it says what is missing.
fn wait_for<T>(app: &TestApp, mut look: impl FnMut() -> Result<T, String>) -> T {
    let mut found = None;
    app.wait_until(|| match look() {
        Ok(v) => {
            found = Some(v);
            None
        }
        Err(missing) => Some(missing),
    });
    found.expect("the wait ends on a look that found it")
}

/// [`wait_for`] for what takes the app long (a slice, a restart): `budget` of its main thread's
/// CPU ([`TestApp::wait_until_within`]).
fn wait_for_within<T>(app: &TestApp, budget: Duration, mut look: impl FnMut() -> Result<T, String>) -> T {
    let mut found = None;
    app.wait_until_within(budget, || match look() {
        Ok(v) => {
            found = Some(v);
            None
        }
        Err(missing) => Some(missing),
    });
    found.expect("the wait ends on a look that found it")
}

/// A deliberately tight look on the wall clock: `look` finds it within `timeout`.
fn within<T>(timeout: Duration, mut look: impl FnMut() -> Result<T, String>) -> T {
    let started = Instant::now();
    loop {
        match look() {
            Ok(v) => return v,
            Err(missing) => assert!(started.elapsed() < timeout, "{missing}"),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The view's layer, when it is `want` (any layer for `None`).
fn layer_at(app: &TestApp, want: Option<u32>) -> Result<(u32, u32), String> {
    let l = layer_label(app);
    l.filter(|l| want.is_none_or(|w| w == l.0)).ok_or_else(|| format!("the view is not at layer {want:?}: {l:?}"))
}

/// The first shown name that starts with `prefix`.
fn named(app: &TestApp, prefix: &str) -> Result<String, String> {
    let names = names(app);
    names.iter().find(|t| t.starts_with(prefix)).cloned().ok_or_else(|| format!("no node named {prefix:?}: {names:?}"))
}

fn wait_layer(app: &TestApp, want: Option<u32>) -> (u32, u32) {
    wait_for(app, || layer_at(app, want))
}

fn wait_name(app: &TestApp, prefix: &str) -> String {
    wait_for(app, || named(app, prefix))
}

/// The slice's state (`fab.slice`, not the render mode's "Sliced") says the plate's slice settled,
/// and the frame on screen shows it: the ui tree is that frame's, which can be a frame behind.
fn sliced(app: &TestApp) -> bool {
    let Ok(reply) = call(app, "slice", "{}") else { return false };
    let Ok(slice) = makepad_script::syntax::parse_value(&text(&reply)) else { return false };
    slice.field("state").and_then(|f| f.value.string()) == Some("Sliced") && slice.field("on_screen").is_some_and(|f| f.value.ident() == Some("true"))
}

/// One status report, appended as the printer would publish it.
fn report(sim: &Path, json: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(sim).expect("the report file");
    writeln!(f, "{json}").unwrap();
}

/// Wait until the app consumed this report before checking which workspace it kept.
fn wait_report(app: &TestApp, layer: u32) {
    wait_for(app, || {
        let reply = text(&call(app, "printer", "{}").expect("printer"));
        let value = makepad_strict_json::parse(reply.as_bytes()).expect("printer JSON");
        if value.get("layer").and_then(Value::as_u64) == Some(layer as u64) {
            return Ok(());
        }
        Err(format!("the printer never reached layer {layer}: {reply}"))
    })
}

const IDLE: &str = r#"{"print":{"command":"push_status","msg":0,"gcode_state":"IDLE","bed_temper":24,"bed_target_temper":0,"sdcard":true,"device":{"extruder":{"info":[{"id":0,"temp":22},{"id":1,"temp":23}]},"nozzle":{"info":[{"id":0,"type":"HS01","diameter":0.4},{"id":1,"type":"HS01","diameter":0.4}]}},"ams":{"tray_exist_bits":"1","ams":[{"id":0,"info":"1003","tray":[{"id":0,"tray_type":"PLA","tray_color":"E2672BFF","remain":80}]}]}}}"#;

/// The fab tools answer: the plate's head revision.
fn tools_up(app: &TestApp) -> String {
    let line = wait_for_within(app, Duration::from_secs(30), || call(app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}")));
    head_of(&text(&line))
}

/// A 20 x 12 x 20 mm block on the plate.
fn place_block(app: &TestApp, head: String) {
    let source = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let block = m.box({size: [20mm 12mm 20mm]})
object(block)
"##;
    let batch = format!(
        r#"{{"request_id":"monitor-1","expected":{head:?},"script":"block.cad.splash","source":{}}}"#,
        Value::Str(source.to_string()).to_json(),
    );
    let reply = text(&call(app, "script", &batch).expect("script"));
    let head = head_of(&reply);
    let edits = r#"place("block", name: "Block")"#;
    let batch = format!(r#"{{"request_id":"monitor-1-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let reply = text(&call(app, "place", &batch).expect("place"));
    assert!(reply.contains("solid: \"block/block\""), "{reply}");
}

fn monitor(app: TestApp, sim: &Path, results: &Path) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    workspace_row::press(&app, "3D modeling");
    place_block(&app, tools_up(&app));

    // Slice (the Slice render mode slices and shows it): the plate's slice, Design's business. The
    // job the simulated printer runs is the committed one on the disk ("block").
    render_mode::pick(&app, "Sliced");
    wait_for_within(&app, Duration::from_secs(120), || match sliced(&app) {
        true => Ok(()),
        false => Err(format!("the block never sliced: {:?}", names(&app))),
    });
    let (top, plate_count) = wait_layer(&app, None);
    assert_eq!(top, plate_count, "a fresh slice shows the whole print");
    assert!(plate_count > 8, "a 12 mm block has more than eight layers: {plate_count}");
    assert_ne!(plate_count, JOB_LAYERS, "the job is not the plate's slice");
    let count = JOB_LAYERS;

    // ---- Monitor while the printer prepares, then prints: the view follows its layer ----
    workspace_row::press(&app, "Monitor");
    let top_layer = Selector::role(Role::Slider).name("Top layer");
    let back = Selector::role(Role::Button).name("Back to live");
    // The job at the top of the view: the printer's stage, the time left, a way out that asks first.
    wait_name(&app, "Heating the bed");
    let times = wait_name(&app, "Elapsed 10:0");
    assert!(times.contains("Left 40:00"), "{times}");
    app.locator(Selector::role(Role::Button).name("Stop…")).wait_visible().press();
    wait_name(&app, "Stop the print?");
    app.locator(Selector::role(Role::Button).name("Keep printing")).wait_visible().press();
    wait_no_name(&app, "Stop the print?");
    assert!(names(&app).iter().any(|t| t == "Pause"), "the print can be paused");
    let printing = |layer: u32| {
        let left = 40 - 40 * layer / count;
        report(sim, &format!(r#"{{"print":{{"msg":1,"gcode_state":"RUNNING","stg_cur":0,"layer_num":{layer},"mc_percent":{},"mc_remaining_time":{left}}}}}"#, 100 * layer / count));
    };
    printing(2);
    assert_eq!(wait_layer(&app, Some(2)), (2, count), "the view follows the printer's layer");
    wait_name(&app, "Print layers");
    wait_name(&app, "block  ·  Printing  ·  head position estimated from Fab's slice timing");
    assert!(!names(&app).iter().any(|t| t == "Back to live"), "following, there's nothing to go back to");

    // Looking ahead: the view stays where it's put when the printer moves on ...
    app.locator(top_layer.clone()).set_value("1");
    assert_eq!(wait_layer(&app, Some(count)), (count, count));
    wait_name(&app, &format!("Looking ahead at layer {count}: the printer is on layer 2 / {count}"));
    printing(3);
    wait_name(&app, &format!("Looking ahead at layer {count}: the printer is on layer 3 / {count}"));
    assert_eq!(layer_label(&app), Some((count, count)), "looking ahead, the view is the user's");
    // ... and one press catches up.
    app.locator(back.clone()).wait_visible().press();
    assert_eq!(wait_layer(&app, Some(3)), (3, count), "back to live catches up");
    wait_no_name(&app, "Back to live");
    printing(count - 1);
    assert_eq!(wait_layer(&app, Some(count - 1)), (count - 1, count));

    // Monitor's view is its own: it looks back at a layer, Design's slider goes elsewhere and an
    // edit changes Design's slice, and Monitor is where it was, on the job that was sent.
    app.locator(top_layer.clone()).set_value("0.5");
    wait_name(&app, "Looking back at layer");
    let looked = wait_layer(&app, None);
    workspace_row::press(&app, "3D modeling");
    assert_eq!(wait_layer(&app, Some(plate_count)), (plate_count, plate_count), "Design's slider was not moved");
    let head = tools_up(&app);
    let taller = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let block = m.box({size: [20mm 16mm 20mm]})
object(block)
"##;
    let batch = format!(r#"{{"request_id":"monitor-2","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(taller.to_string()).to_json());
    call(&app, "script", &batch).expect("script");
    wait_for_within(&app, Duration::from_secs(120), || match sliced(&app) && layer_label(&app).is_some_and(|(_, m)| m > plate_count) {
        true => Ok(()),
        false => Err(format!("the taller block's slice never showed: {:?}", layer_label(&app))),
    });
    let taller_count = layer_label(&app).expect("the taller block's layers").1;
    workspace_row::press(&app, "Monitor");
    assert_eq!(wait_layer(&app, Some(looked.0)), looked, "the job in Monitor is the committed one, where it was");
    assert!(!names(&app).iter().any(|t| t.starts_with("Slicing") || t.starts_with("Waiting to slice")), "the plate's slice doesn't show here");
    assert!(!names(&app).iter().any(|t| t == "Sliced"), "a held job has no other look");
    printing(count);
    app.locator(back).wait_visible().press();
    assert_eq!(wait_layer(&app, Some(count)), (count, count), "back to live catches up");

    // A job Fab has no copy of: said, with what the printer reports.
    report(sim, r#"{"print":{"msg":1,"subtask_name":"keychain","layer_num":41,"total_layer_num":120,"mc_percent":30,"mc_remaining_time":52}}"#);
    wait_name(&app, "The printer is printing keychain");
    wait_name(&app, "Fab has no copy of this job");

    // ---- Calibrate (the printer idle again): the clearance part becomes the plate and slices ----
    report(sim, IDLE);
    calibration_item(&app, Selector::role(Role::DisclosureButton).name("Settings")).expand();
    calibration_item(&app, Selector::role(Role::StaticText).name("Calibrating Simulated printer with Fab PLA"));
    calibration_item(&app, Selector::role(Role::DisclosureButton).name("Pressure advance")).expand();
    calibration_item(&app, Selector::role(Role::StaticText).name("K 0.035 (from Fab dual nozzle 0.4)"));
    let clearance = calibration_item_toward(&app, Selector::role(Role::DisclosureButton).name("Clearance"), false);
    let section = format!("{}.", clearance.snapshot().node.path);
    clearance.expand();
    let make = app.widget_snapshot().nodes().into_iter().find(|item|
        item.node.path.starts_with(&section) && item.node.role == Role::Button && item.node.name == "Make test part")
        .expect("the Clearance section has its own Make test part button");
    calibration_item(&app, Selector::path(make.node.path)).press();
    wait_for_within(&app, Duration::from_secs(60), || match call(&app, "inspect", r#"{"program":false}"#).map(|l| text(&l)).expect("inspect").contains("block/block") {
        true => Err("the test part never became the plate".to_string()),
        false => Ok(()),
    });
    // The test part's own slice shows (its playback bar, not the block's layers), not the model.
    wait_for_within(&app, Duration::from_secs(120), || match sliced(&app) && layer_label(&app).is_some_and(|(_, m)| m != plate_count && m != taller_count) {
        true => Ok(()),
        false => Err(format!("the test part's slice never showed: {:?}", layer_label(&app))),
    });
    // Back in Monitor: the check text, the numbered variants, and the answer.
    workspace_row::press(&app, "Monitor");
    open_clearance(&app);
    calibration_item(&app, Selector::role(Role::StaticText).text_contains("Hold each hub"));
    wait_name(&app, "Hold each hub");
    // the answer field stands at the row height, as the button under it does (a desktop sheet
    // gave a text input a field's height of its own, taller than Fab's rows)
    let answer = calibration_item(&app, Selector::id("first.input"));
    let field = answer.snapshot().node.bounds;
    let save_button = calibration_item(&app, Selector::role(Role::Button).name("Save result"));
    let save = save_button.snapshot().node.bounds;
    assert_eq!(field.h, save.h, "the answer field {field:?} and Save result {save:?} stand at one row height");
    answer.fill("5");
    save_button.press();
    calibration_item(&app, Selector::role(Role::StaticText).text_contains("Saved for Mechanics"));
    wait_name(&app, "Saved for Mechanics");
    let files: Vec<PathBuf> = std::fs::read_dir(results).unwrap().flatten().flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten().map(|f| f.path())).collect();
    use fab_calibrate::{results::names, Results};
    let owner = Results::owner_path_in(results, "printer:Simulated printer", "Fab PLA");
    let preset = Results::path_in(results, "Fab dual nozzle 0.4", "Fab PLA");
    assert!(files.len() == 2 && files.contains(&owner) && files.contains(&preset), "only the original preset and the new owner file exist: {files:?}");
    let stored = std::fs::read_to_string(owner).unwrap();
    assert!(stored.contains("c_xy = 0.3\n"), "the answer 5 is stored as its clearance: {stored}");
    let preset = Results::load_in(results, "Fab dual nozzle 0.4", "Fab PLA");
    assert_eq!(preset.values.len(), 1, "the preset still contains just its original measurement");
    assert_eq!(preset.get(names::PRESSURE_ADVANCE), Some(0.035), "the preset pressure advance survives the owner's save");
    let other = Results::load_effective_in(results, Some("printer:Other"), "Fab dual nozzle 0.4", "Fab PLA");
    assert_eq!(other.get(names::C_XY), None, "another printer does not inherit the owner's clearance");
    assert_eq!(other.get(names::PRESSURE_ADVANCE), Some(0.035), "another printer shares the preset fallback");
}

fn wait_no_name(app: &TestApp, prefix: &str) {
    wait_for(app, || {
        let names = names(app);
        match names.iter().any(|t| t.starts_with(prefix)) {
            true => Err(format!("a node named {prefix:?} stays: {names:?}")),
            false => Ok(()),
        }
    })
}

/// The Simulated model has no installed profile: what stops Start print once the printer is fine.
fn wait_profile_refusal(app: &TestApp) {
    wait_for(app, || {
        let names = names(app);
        match names.iter().any(|t| t.starts_with("Slice with your installed") || t.starts_with("The slice is for")) {
            true => Ok(()),
            false => Err(format!("no word about the printer profile: {names:?}")),
        }
    })
}

/// The layers of the job [`saved_job`] writes for the Monitor test.
const JOB_LAYERS: u32 = 12;

/// A job as Print commits it, on the disk of the app's home: a small G-code, `layers` layers of a
/// square, in a package the time model reads like any other.
fn saved_job(home: &Path, name: &str, layers: u32, saved: fab::print::jobs::SavedState, created: f64, accepted: &[fab::print::jobs::Accepted]) {
    use fab::print::jobs;
    let mut gcode = String::from("G90\nM83\n; MACHINE_START_GCODE_END\n");
    for layer in 0..layers {
        let z = 0.2 * (layer + 1) as f64;
        gcode.push_str(&format!("; CHANGE_LAYER\n; Z_HEIGHT: {z:.2}\n; LAYER_HEIGHT: 0.2\nG1 Z{z:.2} F600\n; FEATURE: Outer wall\n; LINE_WIDTH: 0.42\nG1 X150 Y150 F6000\nG1 X170 Y150 E1 F1800\nG1 X170 Y170 E1\nG1 X150 Y170 E1\nG1 X150 Y150 E1\n"));
    }
    let mut zip = makepad_zip_file::ZipWriter::new();
    zip.add("Metadata/plate_1.gcode", gcode.as_bytes(), makepad_zip_file::ZipMethod::Deflate).unwrap();
    let package = zip.finish().unwrap();
    let meta = jobs::JobMeta {
        id: jobs::JobMeta::new_id(name, created), name: name.into(), created, planned: 40.0 * 60.0, layers,
        colours: vec![[0.8, 0.4, 0.2, 1.0]], used: vec![0], trays: vec![None], head_offset_x: 26.0, head_lift: 2.0, dual: true,
        // Saved before jobs named their printer: the model, name and layers find it again.
        printer_model: simulated_profiles::model(), accepted: accepted.to_vec(), ..Default::default()
    };
    jobs::commit(&home.join(".makepad/fab/jobs"), &meta, &saved, &package, &Default::default()).unwrap();
}

/// What Monitor says of "block", printed anyway with five floating islands from layer 59.
const ACCEPTED: &str = "Started with 5 accepted problems: floating island ×5 from layer 59";

/// The printer's report of being on "block".
fn on_the_job(state: &str, stage: i32, layer: u32, percent: u32, left: u32) -> String {
    format!(r#"{{"print":{{"msg":1,"gcode_state":"{state}","stg_cur":{stage},"subtask_name":"block","layer_num":{layer},"total_layer_num":{JOB_LAYERS},"mc_percent":{percent},"mc_remaining_time":{left}}}}}"#)
}

/// The app starts with the printer on a job Fab committed earlier, and nothing on the plate.
fn comes_back(app: TestApp, sim: &Path, jobs_dir: &Path) {
    use fab::print::jobs::{read_all, JobState};
    let top_layer = Selector::role(Role::Slider).name("Top layer");
    app.locator(Selector::role(Role::Window)).wait_visible();
    workspace_row::wait_shown(&app, "Monitor");
    // The slice, the printer's layer, and the time from the job's real start.
    assert_eq!(wait_for_within(&app, Duration::from_secs(60), || layer_at(&app, Some(5))), (5, JOB_LAYERS), "Monitor comes back on the job, at the printer's layer");
    wait_name(&app, "block  ·  Printing");
    wait_name(&app, "Elapsed 10:0");
    // It was printed anyway: the printability errors it went with, as committed.
    wait_name(&app, ACCEPTED);
    // It looks ahead, and comes back.
    app.locator(top_layer).set_value("1");
    assert_eq!(wait_layer(&app, Some(JOB_LAYERS)), (JOB_LAYERS, JOB_LAYERS));
    wait_name(&app, &format!("Looking ahead at layer {JOB_LAYERS}: the printer is on layer 5 / {JOB_LAYERS}"));
    app.locator(Selector::role(Role::Button).name("Back to live")).wait_visible().press();
    assert_eq!(wait_layer(&app, Some(5)), (5, JOB_LAYERS));
    // The queued job is listed and stays where it is when the first ends.
    wait_name(&app, "next: Queued");
    // Leaving Monitor is final for this run, even when another busy report arrives.
    workspace_row::press(&app, "3D modeling");
    report(sim, &on_the_job("RUNNING", 0, 6, 50, 19));
    wait_report(&app, 6);
    workspace_row::assert_shown(&app, "3D modeling");
    workspace_row::press(&app, "Monitor");
    report(sim, &on_the_job("FINISH", -1, JOB_LAYERS, 100, 0));
    wait_name(&app, "block: Finished");
    wait_for(&app, || {
        let on_disk = read_all(jobs_dir);
        let state_of = |name: &str| on_disk.iter().find(|(m, _)| m.name == name).map(|(_, s)| s.state.clone());
        if state_of("block") == Some(JobState::Finished) {
            assert_eq!(state_of("next"), Some(JobState::Queued), "nothing starts the next job");
            return Ok(());
        }
        Err("the job's end never reached the disk".to_string())
    });
}

/// The app starts again: the job's end is remembered, and the next job waits for its press.
fn queue_waits(app: TestApp, sim: &Path, jobs_dir: &Path) {
    use fab::print::jobs::{read_all, JobState};
    app.locator(Selector::role(Role::Window)).wait_visible();
    workspace_row::press(&app, "Monitor");
    wait_for_within(&app, Duration::from_secs(60), || named(&app, "block: Finished"));
    wait_name(&app, "Finished");
    wait_name(&app, ACCEPTED);
    // A look at the finished job leaves it finished: it draws, on its own layers.
    assert_eq!(wait_layer(&app, None), (JOB_LAYERS, JOB_LAYERS));
    // The queued job is the person's to start, and asks about the plate first.
    app.locator(Selector::role(Role::ListItem).name("next: Queued")).wait_visible().press();
    wait_name(&app, "Queued: it starts when you press Start");
    app.locator(Selector::role(Role::Button).name("Start…")).wait_visible().press();
    wait_name(&app, "Is the plate clear?");
    app.locator(Selector::role(Role::Button).name("Not yet")).wait_visible().press();
    wait_no_name(&app, "Is the plate clear?");
    app.locator(Selector::role(Role::Button).name("Start…")).wait_visible().press();
    app.locator(Selector::role(Role::Button).name("Start print")).wait_visible().press();
    // This machine has no installed profile to check the job against: it stays queued, and nothing went out.
    wait_name(&app, "next: Queued");
    std::thread::sleep(Duration::from_secs(1));
    assert!(!fab_printer_link::sim::started_path(sim).exists(), "a refused job sends nothing");
    let on_disk = read_all(jobs_dir);
    assert_eq!(on_disk.iter().find(|(m, _)| m.name == "next").map(|(_, s)| s.state.clone()), Some(JobState::Queued));
}

fn json_number(v: &Value) -> f64 {
    match v { Value::F64(n) => *n, Value::Int(n) => *n as f64, _ => panic!("expected a number: {v:?}") }
}

/// Read the geometry submitted by the viewport, not just the requested issue.
fn issue_spot(app: &TestApp) -> Value {
    let node = app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.name == "Model viewport").expect("visible viewport");
    let description = makepad_strict_json::parse(node.node.description.as_bytes()).expect("viewport recording");
    description.get("issue_spot").expect("recording includes issue spot").clone()
}

const QUESTION: &str = "Print the sliced plate on Simulated printer?";

fn print_button(app: TestApp, sim: &Path) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let head = tools_up(&app);
    let print = Selector::id("print");
    let cancel = Selector::role(Role::Button).name("Cancel");

    // An empty plate: the press says so.
    app.locator(print.clone()).click();
    wait_name(&app, "The plate is empty");
    app.locator(cancel.clone()).press();
    wait_no_name(&app, "The plate is empty");

    // A plate not sliced: the card opens at once and asks the moment the slice is there.
    place_block(&app, head);
    app.locator(print.clone()).click();
    wait_for_within(&app, Duration::from_secs(120), || named(&app, QUESTION));
    wait_profile_refusal(&app);
    assert!(!names(&app).iter().any(|t| t == "Start print"), "nothing offers Start print while something stops it");

    // Another press leaves it open; only Cancel closes it.
    app.locator(print.clone()).click();
    std::thread::sleep(Duration::from_millis(300));
    within(Duration::from_secs(1), || named(&app, QUESTION));
    app.locator(cancel.clone()).press();
    wait_no_name(&app, QUESTION);

    // From every workspace.
    for ws in ["Mechanics", "Monitor", "3D modeling"] {
        workspace_row::press(&app, ws);
        app.locator(print.clone()).click();
        wait_name(&app, QUESTION);
        wait_profile_refusal(&app);
        app.locator(cancel.clone()).press();
        wait_no_name(&app, QUESTION);
    }

    // The card follows the printer's reports: what stops Start print now, as it changes.
    app.locator(print.clone()).click();
    wait_name(&app, QUESTION);
    report(sim, r#"{"print":{"msg":1,"gcode_state":"RUNNING","stg_cur":0,"layer_num":3,"total_layer_num":9}}"#);
    wait_name(&app, "Simulated printer is busy. Queue the sliced plate?");
    report(sim, r#"{"print":{"msg":1,"gcode_state":"IDLE","sdcard":false}}"#);
    wait_name(&app, "Insert a USB drive");
    report(sim, r#"{"print":{"msg":1,"sdcard":true,"gcode_state":"FINISH"}}"#);
    wait_name(&app, "Confirm the plate is clear first");
    app.locator(Selector::role(Role::CheckBox).name("The plate is clear: the last print is off the bed")).check(true);
    wait_profile_refusal(&app);
    wait_no_name(&app, "Confirm the plate is clear first");

    // A plate that changes under the open card slices again, and its printability error is named
    // with a press that goes to the island's layer.
    let head = head_of(&text(&call(&app, "inspect", "{}").expect("inspect")));
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet base = m.box({size: [20mm 6mm 20mm] at: [0mm 3mm 0mm]})\nlet high = m.box({size: [10mm 4mm 10mm] at: [25mm 12mm 0mm]})\nlet block = m.union(base, high)\nobject(block)\n";
    let batch = format!(r#"{{"request_id":"monitor-island","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
    text(&call(&app, "script", &batch).expect("script"));
    let why = wait_for_within(&app, Duration::from_secs(120), || named(&app, "Not printable: floating island"));
    assert!(why.contains("on layer"), "the refusal says where: {why}");
    // Printability errors alone refuse until the person ticks "Print anyway", off on every opening
    // of the card. Ticked, the errors stay named and Start print waits only for what else stops
    // it: here the printer profile the simulated printer has none of.
    let anyway = wait_name(&app, "Print anyway, with th");
    let tick = Selector::role(Role::CheckBox).name(anyway.as_str());
    app.locator(tick.clone()).assert_checked(false);
    let next_check = |app: &TestApp| names(app).iter().any(|t| t.contains("Slice with your installed"));
    assert!(!next_check(&app), "unticked, the printability error is what refuses: {:?}", names(&app));
    app.locator(tick.clone()).check(true);
    wait_for(&app, || match next_check(&app) {
        true => Ok(()),
        false => Err(format!("ticked, the printability errors still refuse: {:?}", names(&app))),
    });
    assert!(names(&app).iter().any(|t| t.starts_with("Not printable: floating island")), "the accepted errors stay named");
    app.locator(cancel.clone()).press();
    wait_no_name(&app, QUESTION);
    app.locator(print.clone()).click();
    wait_name(&app, QUESTION);
    app.locator(tick).wait_visible().assert_checked(false);
    assert!(!next_check(&app), "opened again, the plate is refused again: {:?}", names(&app));
    let show = wait_name(&app, "Show layer");
    let layer: u32 = show.trim_start_matches("Show layer ").split(' ').next().unwrap().parse().expect("the button names its layer");
    assert!(layer > 1, "the island starts above the bed: {show}");
    assert!(!names(&app).iter().any(|t| t == "Start print"), "a refused plate offers no Start print");
    // The mesh detector independently identifies the island; its bounds must be the
    // same region the slice's Show layer frames, including the part's plate placement.
    let check = text(&call(&app, "check", "{}").expect("check"));
    let parsed = makepad_script::syntax::parse_value(&check).expect("check reply");
    let issues = parsed.field("issues").unwrap().value.items().unwrap();
    let island = issues.iter().find(|i| i.field("kind").and_then(|f| f.value.name()) == Some("floating_island")).expect("the separate high block");
    let bounds = island.field("where").unwrap().value.field("footprint").unwrap().value.items().unwrap();
    let xy = |i: usize| -> [f64; 2] {
        let v = bounds[i].items().unwrap();
        std::array::from_fn(|d| v[d].text(&check).trim_end_matches("mm").parse().unwrap())
    };
    let (lo, hi) = (xy(0), xy(1));
    for d in 0..2 { assert!((hi[d] - lo[d] - 10.0).abs() < 0.05, "the ten mm island"); }
    app.locator(Selector::role(Role::Button).name(show.as_str())).press();
    assert_eq!(wait_layer(&app, Some(layer)).0, layer, "the press shows the island's layer");
    let frame = issue_spot(&app);
    let edges = frame.get("segments").and_then(Value::as_arr).expect("Show layer submitted a marker to the preview draw path");
    assert_eq!(edges.len(), 4, "four retained segments");
    let corners = [[lo[0]-1.0, lo[1]-1.0], [hi[0]+1.0, lo[1]-1.0], [hi[0]+1.0, hi[1]+1.0], [lo[0]-1.0, hi[1]+1.0]];
    for (i, edge) in edges.iter().enumerate() {
        let point = |end: &str| -> Vec<f64> { edge.get(end).and_then(Value::as_arr).expect("an edge's ends").iter().map(json_number).collect() };
        let (a, b) = (point("a"), point("b"));
        for d in 0..2 {
            assert!((a[d] - corners[i][d]).abs() < 0.05, "start of edge {i}, axis {d}");
            assert!((b[d] - corners[(i+1)%4][d]).abs() < 0.05, "end of edge {i}, axis {d}");
        }
        assert_eq!(json_number(edge.get("layer").unwrap()) as u32 + 1, layer, "the UI's displayed layer");
        assert_eq!(a[2], b[2], "the rectangle lies in the slice plane");
        assert!(a[2] >= 10.0 && a[2] <= 10.4, "the island's first print plane");
        assert_eq!(json_number(edge.get("role").unwrap()), 0.0, "error severity colour");
    }
    let opacity = json_number(frame.get("opacity").unwrap());
    assert!(opacity > 0.0 && opacity <= 1.0);
    std::thread::sleep(Duration::from_millis(400));
    let fading = issue_spot(&app);
    assert!(json_number(fading.get("opacity").expect("still fading")) < opacity, "opacity decreases in the actual draw");
    if let Some(dir) = std::env::var_os("FAB_ISSUE_SPOT_GRAB_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(app.screenshot(), dir.join("issue-spot.png")).unwrap();
    }
    std::thread::sleep(Duration::from_secs(2));
    assert!(issue_spot(&app).is_null(), "the composite removes the marker when its fade ends");
}

#[test]
fn every_press_of_print_answers_with_what_stands_in_the_way() {
    let dir: PathBuf = std::env::temp_dir().join(format!("fab-print-press-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sim = dir.join("reports.jsonl");
    std::fs::write(&sim, format!("{IDLE}\n")).unwrap();
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "every_press_of_print_answers_with_what_stands_in_the_way").unwrap();
    config.env.insert("FAB_PRINTER_SIM".into(), sim.display().to_string());
    config.env.insert("FAB_PRINTER_SIM_MODEL".into(), simulated_profiles::model());
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    let result = run_with_config(config, |app| print_button(app, &sim));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = result {
        panic!("{}", e.message());
    }
}

/// A few layers are enough to prove that a real slice reaches the printer and survives a restart.
fn small_plate(app: &TestApp, floating: bool) {
    let head = tools_up(app);
    let high = if floating {
        "let high = m.box({size: [2mm 0.4mm 2mm] at: [8mm 2.2mm 0mm]})\nlet block = m.union(base, high)"
    } else {
        "let block = base"
    };
    let source = format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet base = m.box({{size: [4mm 1.2mm 4mm] at: [0mm 0.6mm 0mm]}})\n{high}\nobject(block)\n");
    let batch = format!(r#"{{"request_id":"small-plate","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(source).to_json());
    let head = head_of(&text(&call(app, "script", &batch).expect("script")));
    if !floating {
        let batch = format!(r#"{{"request_id":"small-place","expected":{head:?},"edits":"place(\"block\", name: \"Block\")"}}"#);
        call(app, "place", &batch).expect("place");
    }
    app.locator(Selector::id("print")).click();
    wait_for_within(app, Duration::from_secs(120), || match common::slice_state(app) == "Sliced" {
        true => Ok(()),
        false => Err(format!("the small plate never sliced: {:?}", names(app))),
    });
}

fn job_report(sim: &Path, meta: &fab::print::jobs::JobMeta, state: &str, layer: u32) {
    report(sim, &format!(r#"{{"print":{{"msg":1,"gcode_state":{state:?},"stg_cur":0,"subtask_name":{},"layer_num":{layer},"total_layer_num":{},"mc_percent":25,"mc_remaining_time":1}}}}"#, Value::Str(meta.name.clone()).to_json(), meta.layers));
}

fn committed_job(app: &TestApp, jobs_dir: &Path, number: usize) -> fab::print::jobs::JobMeta {
    wait_for(app, || {
        // Do not call read_all while Print is writing: it removes unfinished job directories.
        let finished: Vec<_> = std::fs::read_dir(jobs_dir).into_iter().flatten().flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.')).collect();
        if finished.len() == number {
            for entry in &finished {
                assert!(entry.path().join("job.txt").is_file(), "Print must commit job.txt");
            }
            let jobs = fab::print::jobs::read_all(jobs_dir);
            assert_eq!(jobs.len(), number, "every committed job must be readable");
            let (meta, _) = &jobs[0];
            assert_eq!(meta.printer_model, simulated_profiles::model());
            assert_eq!(meta.machine_preset, simulated_profiles::MACHINE);
            assert_eq!(meta.filament_presets, [simulated_profiles::FILAMENT]);
            assert!(meta.layers >= 3, "the committed slice has layers to follow");
            assert!(!fab::print::jobs::read_package(jobs_dir, &meta.id).expect("committed package").is_empty());
            return Ok(meta.clone());
        }
        Err(format!("Print never committed job {number}: {:?}", names(app)))
    })
}

fn received_job(app: &TestApp, sim: &Path, meta: &fab::print::jobs::JobMeta) {
    // The real link only sends project_file after the simulated storage upload succeeds, and
    // only reports Accepted after the simulator answers that command.
    wait_name(app, "The printer took the job: waiting for it to begin");
    let command = std::fs::read_to_string(fab_printer_link::sim::started_path(sim)).expect("the simulated printer received the start command");
    let command = makepad_strict_json::parse(command.as_bytes()).expect("start JSON");
    let print = command.get("print").expect("print command");
    assert_eq!(print.get("command").and_then(Value::as_str), Some("project_file"));
    assert_eq!(print.get("subtask_name").and_then(Value::as_str), Some(meta.name.as_str()), "the simulator received this job");
    assert!(print.get("md5").and_then(Value::as_str).is_some_and(|s| s.len() == 32), "the start command identifies the uploaded package");
}

fn running_job(app: &TestApp, meta: &fab::print::jobs::JobMeta, layer: u32) {
    workspace_row::wait_shown(app, "Monitor");
    assert_eq!(wait_layer(app, Some(layer)), (layer, meta.layers), "Monitor follows the committed job's reported layer");
    wait_name(app, &format!("{}  ·  Printing", meta.name));
    wait_name(app, "Print layers");
}

/// The ticked test prints commit as one job through Print, named for them and on the test
/// slice's layers, and the simulated printer gets it; untick one and the next job is the other's.
#[test]
fn ticked_test_prints_commit_as_one_job_named_for_them() {
    let dir = std::env::temp_dir().join(format!("fab-monitor-test-prints-{}", std::process::id()));
    simulated_profiles::install(&dir);
    let sim = dir.join("reports.jsonl");
    std::fs::write(&sim, format!("@read-wait 5\n@upload 50\n{IDLE}\n")).unwrap();
    let jobs_dir = dir.join(".makepad/fab/jobs");
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "ticked_test_prints_commit_as_one_job_named_for_them").unwrap();
    config.env.insert("FAB_PRINTER_SIM".into(), sim.display().to_string());
    config.env.insert("FAB_PRINTER_SIM_MODEL".into(), simulated_profiles::model());
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    let result = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        let head = tools_up(&app);
        let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet base = m.box({size: [20mm 6mm 6mm] at: [0mm 3mm 0mm]})\nlet block = base\nobject(block)\n";
        let batch = format!(r#"{{"request_id":"test-prints-script-{head}","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
        let head = head_of(&text(&call(&app, "script", &batch).expect("the test block script")));
        let batch = format!(r#"{{"request_id":"test-prints-place-{head}","expected":{head:?},"edits":"place(\"block\", at: [50mm 50mm], name: \"Block\")"}}"#);
        let head = head_of(&text(&call(&app, "place", &batch).expect("the test block placement")));
        let reply = text(&call(&app, "inspect", r#"{"program":false}"#).expect("inspect the placed block"));
        let plate = makepad_script::syntax::parse_value(&reply).expect("the plate inspection is Splash");
        let parts = plate.field("plate").and_then(|f| f.value.field("parts")).and_then(|f| f.value.items()).expect("the placed block is on the plate");
        assert_eq!(parts.len(), 1, "the fixture must contain exactly one block");
        let object = parts[0].field("solid").and_then(|f| f.value.string()).expect("the block has a solid identifier");
        // Four mm cubes on opposite halves of a six mm tall block: the plate has more layers.
        let batch = format!(r#"{{"request_id":"test-prints-add-{head}","expected":{head:?},"action":"add","boxes":[{{"object":{object:?},"at":[45,50,3],"size":4,"name":"Left"}},{{"object":{object:?},"at":[55,50,3],"size":4,"name":"Right"}}]}}"#);
        let added = text(&call(&app, "test_print", &batch).expect("add both test cubes in one edit"));
        assert!(!added.contains("error:"), "both test cubes must be added: {added}");
        let head = tools_up(&app);
        let batch = format!(r#"{{"request_id":"test-prints-mode-{head}","expected":{head:?},"action":"mode","tests":true}}"#);
        call(&app, "test_print", &batch).expect("choose Test prints");
        render_mode::pick(&app, "Sliced");

        let mut previous_slice = None;
        for (index, expected_names) in [&["Left", "Right"][..], &["Left"][..]].into_iter().enumerate() {
            let read = wait_for(&app, || {
                let reply = text(&call(&app, "test_print", r#"{"action":"read"}"#)?);
                let read = makepad_script::syntax::parse_value(&reply).expect("the test-print read is Splash");
                let current = read.field("current").is_some_and(|f| f.value.ident() == Some("true"));
                let sliced = read.field("state").and_then(|f| f.value.string()) == Some("Sliced");
                let id = read.field("slice_id").and_then(|f| f.value.number());
                if current && sliced && id.is_some() && id != previous_slice {
                    Ok(read)
                } else {
                    Err(format!("test print {} needs a fresh current slice: {reply}", index + 1))
                }
            });
            previous_slice = read.field("slice_id").and_then(|f| f.value.number());
            assert_eq!(read.field("slicing").and_then(|f| f.value.string()), Some("test prints"), "Print must use Test prints mode");
            let pieces = read.field("pieces").and_then(|f| f.value.items()).expect("the test-print read lists its pieces");
            assert_eq!(pieces.len(), 2, "both cubes remain listed after changing their ticks");
            let ticked: Vec<_> = pieces.iter().filter(|p| p.field("on").is_some_and(|f| f.value.ident() == Some("true"))).collect();
            let names: Vec<_> = ticked.iter().map(|p| p.field("name").and_then(|f| f.value.string()).expect("a ticked piece has a name")).collect();
            assert_eq!(names, expected_names, "only the requested cubes must be ticked for this print");
            let layers = ticked.iter().map(|p| {
                let span = p.field("layers").and_then(|f| f.value.items()).expect("every ticked piece has sliced layers");
                assert_eq!(span.len(), 2, "piece layers must contain first and last");
                span[1].number().expect("the piece's last layer is numeric") as u32 + 1
            }).max().expect("the test slice contains a ticked piece");
            let expected_name = format!("test prints {}", names.join(", "));
            app.locator(Selector::id("print")).click();
            wait_name(&app, "Start print");
            app.locator(Selector::role(Role::Button).name("Start print")).press();
            let meta = committed_job(&app, &jobs_dir, index + 1);
            assert_eq!(meta.name, expected_name, "one committed job must name exactly its ticked test prints");
            assert_eq!(meta.layers, layers, "the committed job must use the test slice's layers, not the whole plate's");
            received_job(&app, &sim, &meta);

            if index == 0 {
                job_report(&sim, &meta, "RUNNING", 2);
                running_job(&app, &meta, 2);
                job_report(&sim, &meta, "FINISH", meta.layers);
                wait_name(&app, &format!("{}: Finished", meta.name));
                std::fs::remove_file(fab_printer_link::sim::started_path(&sim)).unwrap();
                report(&sim, IDLE);
                workspace_row::press(&app, "3D modeling");
                let head = tools_up(&app);
                let batch = format!(r#"{{"request_id":"test-prints-untick-{head}","expected":{head:?},"action":"set","name":"Right","on":false}}"#);
                call(&app, "test_print", &batch).expect("untick Right for the next print");
            }
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = result { panic!("{}", e.message()); }
}

#[test]
fn installed_profile_prints_queued_and_accepted_jobs_and_monitor_resumes() {
    use fab::print::jobs::{read_all, JobState};
    let dir = std::env::temp_dir().join(format!("fab-monitor-profile-{}", std::process::id()));
    simulated_profiles::install(&dir);
    let sim = dir.join("reports.jsonl");
    // Harness controls, not app switches: shorten the fake upload and its empty reads.
    std::fs::write(&sim, format!("@read-wait 5\n@upload 50\n{IDLE}\n")).unwrap();
    let jobs_dir = dir.join(".makepad/fab/jobs");
    let config = || {
        let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "installed_profile_prints_queued_and_accepted_jobs_and_monitor_resumes").unwrap();
        config.env.insert("FAB_PRINTER_SIM".into(), sim.display().to_string());
        config.env.insert("FAB_PRINTER_SIM_MODEL".into(), simulated_profiles::model());
        config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
        config
    };
    let mut queued = None;
    let first = run_with_config(config(), |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        small_plate(&app, false);
        wait_name(&app, "Start print");
        // A busy printer queues, without sending anything to it.
        report(&sim, r#"{"print":{"msg":1,"gcode_state":"RUNNING","subtask_name":"earlier","layer_num":1,"total_layer_num":3}}"#);
        app.locator(Selector::role(Role::Button).name("Queue print")).wait_visible().press();
        let meta = committed_job(&app, &jobs_dir, 1);
        workspace_row::press(&app, "Monitor");
        wait_name(&app, &format!("{}: Queued", meta.name));
        assert_eq!(read_all(&jobs_dir)[0].1.state, JobState::Queued, "the job is queued on disk");
        assert!(meta.accepted.is_empty(), "a sound plate accepts no problems");
        assert!(!fab_printer_link::sim::started_path(&sim).exists(), "queueing must not start a print");
        report(&sim, IDLE);
        app.locator(Selector::role(Role::ListItem).name(format!("{}: Queued", meta.name))).press();
        app.locator(Selector::role(Role::Button).name("Start…")).wait_visible().press();
        wait_name(&app, "Is the plate clear?");
        app.locator(Selector::role(Role::Button).name("Start print")).press();
        received_job(&app, &sim, &meta);
        job_report(&sim, &meta, "RUNNING", 2);
        running_job(&app, &meta, 2);
        queued = Some(meta);
    });
    let mut accepted = None;
    let second = first.and_then(|_| run_with_config(config(), |app| {
        let meta = queued.as_ref().unwrap();
        app.locator(Selector::role(Role::Window)).wait_visible();
        running_job(&app, meta, 2);
        job_report(&sim, meta, "RUNNING", 3);
        running_job(&app, meta, 3);
        job_report(&sim, meta, "FINISH", meta.layers);
        wait_name(&app, &format!("{}: Finished", meta.name));
        std::fs::remove_file(fab_printer_link::sim::started_path(&sim)).unwrap();
        report(&sim, IDLE);
        workspace_row::press(&app, "3D modeling");
        small_plate(&app, true);
        wait_for_within(&app, Duration::from_secs(120), || named(&app, "Not printable: floating island"));
        assert!(!names(&app).iter().any(|n| n == "Start print"), "the problem blocks printing until accepted");
        let anyway = wait_name(&app, "Print anyway, with th");
        app.locator(Selector::role(Role::CheckBox).name(anyway)).check(true);
        app.locator(Selector::role(Role::Button).name("Start print")).wait_visible().press();
        let meta = committed_job(&app, &jobs_dir, 2);
        assert!(!meta.accepted.is_empty(), "Print anyway commits its accepted problems");
        assert!(meta.accepted.iter().any(|a| a.kind == "mid_air" && a.count > 0 && a.layer.is_some()), "the accepted slice island is recorded with its layer: {:?}", meta.accepted);
        workspace_row::press(&app, "Monitor");
        received_job(&app, &sim, &meta);
        job_report(&sim, &meta, "RUNNING", 2);
        running_job(&app, &meta, 2);
        let count: u32 = meta.accepted.iter().map(|a| a.count).sum();
        let line = wait_name(&app, &format!("Started with {count} accepted problem"));
        assert!(line.contains("mid air"), "Monitor names the accepted slice problem: {line}");
        accepted = Some((meta, line));
    }));
    let result = second.and_then(|_| run_with_config(config(), |app| {
        let (meta, line) = accepted.as_ref().unwrap();
        app.locator(Selector::role(Role::Window)).wait_visible();
        running_job(&app, meta, 2);
        assert_eq!(wait_name(&app, "Started with "), *line, "the accepted problems survive restart");
        job_report(&sim, meta, "RUNNING", 3);
        running_job(&app, meta, 3);
    }));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = result { panic!("{}", e.message()); }
}

#[test]
fn monitor_follows_a_simulated_printer_and_calibrate_stores_the_answer() {
    let dir: PathBuf = std::env::temp_dir().join(format!("fab-monitor-sim-{}", std::process::id()));
    let results = dir.join("results");
    std::fs::create_dir_all(&results).unwrap();
    let mut preset = fab_calibrate::Results::new("Fab dual nozzle 0.4", "Fab PLA");
    preset.values.insert(fab_calibrate::results::names::PRESSURE_ADVANCE.into(), 0.035);
    preset.save_in(&results).unwrap();
    let sim = dir.join("reports.jsonl");
    std::fs::write(&sim, format!("{IDLE}\n{}\n", on_the_job("PREPARE", 2, 0, 0, 40))).unwrap();
    let started = wall_now() - 36000.0;
    saved_job(&dir, "block", JOB_LAYERS, fab::print::jobs::SavedState { state: fab::print::jobs::JobState::Sent, started: Some(started), ..Default::default() }, started, &[]);
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "monitor_follows_a_simulated_printer_and_calibrate_stores_the_answer").unwrap();
    config.env.insert("FAB_PRINTER_SIM".into(), sim.display().to_string());
    config.env.insert("FAB_PRINTER_SIM_MODEL".into(), simulated_profiles::model());
    config.env.insert("FAB_CALIBRATION_DIR".into(), results.display().to_string());
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    let result = run_with_config(config, |app| monitor(app, &sim, &results));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = result {
        panic!("{}", e.message());
    }
}

#[test]
fn a_committed_job_comes_back_after_a_restart_and_the_next_one_waits_for_its_press() {
    use fab::print::jobs::{JobState, SavedState};
    let dir: PathBuf = std::env::temp_dir().join(format!("fab-monitor-resume-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sim = dir.join("reports.jsonl");
    std::fs::write(&sim, format!("{IDLE}\n{}\n", on_the_job("RUNNING", 0, 5, 40, 20))).unwrap();
    let started = wall_now() - 36000.0;
    // "block" went with the confirm card's "Print anyway" ticked.
    let accepted = [fab::print::jobs::Accepted { kind: "floating_island".into(), layer: Some(58), count: 5 }];
    saved_job(&dir, "block", JOB_LAYERS, SavedState { state: JobState::Sent, started: Some(started), ..Default::default() }, started, &accepted);
    saved_job(&dir, "next", 8, SavedState { state: JobState::Queued, ..Default::default() }, started + 60.0, &[]);
    let jobs_dir = dir.join(".makepad/fab/jobs");
    let config = || {
        let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "a_committed_job_comes_back_after_a_restart_and_the_next_one_waits_for_its_press").unwrap();
        config.env.insert("FAB_PRINTER_SIM".into(), sim.display().to_string());
        config.env.insert("FAB_PRINTER_SIM_MODEL".into(), simulated_profiles::model());
        config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
        config
    };
    let first = run_with_config(config(), |app| comes_back(app, &sim, &jobs_dir));
    let mut result = first.and_then(|_| run_with_config(config(), |app| queue_waits(app, &sim, &jobs_dir)));
    // Each start has one opportunity. Unknown jobs and idle reports use it up; an action
    // before a delayed first report cancels it, even if the person returns to Design.
    for case in ["no copy", "idle", "workspace", "edit"] {
        if result.is_err() { break; }
        let initial = match case {
            "no copy" => on_the_job("RUNNING", 0, 1, 10, 30).replace("block", "unknown").replace("\"msg\":1", "\"msg\":0"),
            "idle" => IDLE.to_string(),
            _ => String::new(),
        };
        std::fs::write(&sim, format!("{initial}\n")).unwrap();
        result = run_with_config(config(), |app| {
            app.locator(Selector::role(Role::Window)).wait_visible();
            let head = tools_up(&app);
            match case {
                "no copy" => wait_report(&app, 1),
                "idle" => {
                    // A layer marker makes consumption of the idle report observable.
                    report(&sim, r#"{"print":{"msg":1,"gcode_state":"IDLE","layer_num":2}}"#);
                    wait_report(&app, 2);
                }
                "workspace" => {
                    workspace_row::press(&app, "Mechanics");
                    workspace_row::press(&app, "3D modeling");
                }
                "edit" => place_block(&app, head),
                _ => unreachable!(),
            }
            workspace_row::assert_shown(&app, "3D modeling");
            report(&sim, &on_the_job("RUNNING", 0, 7, 60, 18).replace("\"msg\":1", "\"msg\":0"));
            wait_report(&app, 7);
            workspace_row::assert_shown(&app, "3D modeling");
        });
    }
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = result {
        panic!("{}", e.message());
    }
}

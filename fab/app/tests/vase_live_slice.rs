//! Distance-field parts' numbers dragged in the Sliced view, by real pointer events at display
//! rate: the acceptance numbers of realtime slicing from editing a part. One plate holds the demo's
//! drags, the vase's Belly and Height and the lattice's Period; a part of its own holds the blend's.
//!
//! - A drag only grows its number, so the value let go at was never sliced before. Every pointer
//!   move asks for the part at a new value. The time from each edit to the first frame that draws
//!   the slider's layer of the vase at that edit is its latency; edits a newer one replaced before
//!   they were drawn count as dropped. (The test client delivers pointer moves at about 11 per
//!   second, slower than a display: a real drag replaces more edits.)
//! - After the release, the time to the last new layer of the vase drawn is the full reslice.
//!
//! Prints `[fab-vase-live]` lines; asserts only what must hold however fast the slicer is: the
//! slider's layer is drawn for the final value, every layer of the vase comes back, nothing up
//! to the slider is ever shown empty, the other parts do no work.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

mod common;
#[path = "common/drawn_runs.rs"]
mod drawn_runs;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;

use common::slice_state;
use makepad_test::{makepad_test, KeyCode, KeyModifiers, Role, Selector, TestApp};
use std::time::{Duration, Instant};

/// Steps of the drag, each `STEP_PX` further out (the client splits each into a few moves).
const STEPS: usize = 15;
const STEP_PX: f64 = 3.0;
/// How far the first drag moves (px).
const FIRST_PX: f64 = 9.0;

fn remote(app: &TestApp, args: &[String]) -> Result<Vec<String>, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, args, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("{args:?}: {e:?}; {lines:?}"))?;
    if lines.iter().any(|l| l.contains("\"err\":")) {
        return Err(format!("{args:?}: {lines:?}"));
    }
    Ok(lines)
}

fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let lines = remote(app, &["call".into(), format!("fab.{tool}"), args.into()])?;
    lines.iter().find(|l| l.contains("\"Result\"")).cloned().ok_or_else(|| format!("fab.{tool} returned no result: {lines:?}"))
}

thread_local! {
    /// What the slice last read, for a wait that times out on it.
    static SEEN: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    /// Log lines the app's ring dropped before a drag's reads got them.
    static LOST: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The slice's state, kept for the message of a wait that times out.
fn seen_slice(app: &TestApp) -> String {
    let state = slice_state(app);
    SEEN.with(|s| s.borrow_mut().clone_from(&state));
    state
}

fn wait_until(app: &TestApp, what: &str, mut ready: impl FnMut() -> bool) {
    wait_poll(app, || {
        if !ready() { Err(format!("timed out waiting for {what} (the slice last read {:?})", SEEN.with(|s| s.borrow().clone()))) } else { Ok(()) }
    });
}

fn collect_logs(app: &TestApp, cursor: &mut u64, lines: &mut Vec<String>) {
    let tail = app.log_since(*cursor).expect("trace telemetry remains available");
    LOST.with(|l| l.set(l.get() + (tail.last_seq - *cursor).saturating_sub(tail.lines.len() as u64)));
    *cursor = tail.last_seq;
    lines.extend(tail.lines);
}

fn number_after(l: &str, key: &str) -> Option<f64> {
    let at = l.find(key)? + key.len();
    l[at..].trim_start().split(|c: char| !(c.is_ascii_digit() || c == '.')).next()?.parse().ok()
}

fn shown(app: &TestApp, label: &str) -> String {
    app.widget_snapshot()
        .nodes()
        .iter()
        .find(|w| w.visible() && w.node.role == Role::SpinButton && w.node.name == label)
        .map(|w| w.node.value.clone())
        .unwrap_or_else(|| panic!("no control {label}"))
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

#[makepad_test]
fn the_demo_plate_slice_follows_its_vase_and_lattice_drags(app: TestApp) {
    follows_drags(app, &[("Bud vase", "Belly", STEP_PX), ("Bud vase", "Height", 1.0), ("Lattice cube", "Period", 1.0)]);
}

#[makepad_test]
fn the_blend_slice_follows_a_radius_drag(app: TestApp) {
    follows_drags(app, &[("blend50", "Blend", 1.0)]);
}

fn reply_head(line: &str) -> String {
    use makepad_strict_json::Value;
    let reply = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("tool JSON");
    let result = match reply.get("Result").expect("Result") {
        Value::Arr(a) => &a[0], result => result,
    };
    let text = result.get("text").and_then(Value::as_str).expect("tool text");
    makepad_script::syntax::parse_value(text).expect("Splash reply").field("head").and_then(|f| f.value.string()).expect("head").to_string()
}

fn install_blend(app: &TestApp) {
    use makepad_strict_json::Value;
    let mut source = String::from("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet blend_radius = param(3mm, min: 1mm, max: 8mm, step: 0.05mm, label: \"Blend\")\nlet body = m.blend([\n");
    for z in 0..2 { for y in 0..5 { for x in 0..5 {
        source.push_str(&format!("m.sphere({{radius: 4mm at: [{}mm, {}mm, {}mm]}})\n", (x - 2) * 3, y * 3 + 4, (z as f64 - 0.5) * 3.0));
    }}}
    source.push_str("], blend_radius)\nobject(body)\n");
    let head = reply_head(&call(app, "inspect", "{}").unwrap());
    let args = format!(r#"{{"request_id":"blend-script","expected":{head:?},"script":"blend50.cad.splash","source":{}}}"#, Value::Str(source).to_json());
    let head = reply_head(&call(app, "script", &args).unwrap());
    let edits = Value::Str("place(\"blend50\", name: \"blend50\", at: [150mm, 165mm])".into()).to_json();
    let reply = call(app, "place", &format!(r#"{{"request_id":"blend-place","expected":{head:?},"edits":{edits}}}"#)).unwrap();
    println!("[fab-vase-live] blend placement: {reply}");
}

/// Each of `drags` is (part, number, pixels per step of its drag), on one plate brought to Sliced.
fn follows_drags(app: TestApp, drags: &[(&str, &str, f64)]) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait_until(&app, "Fab tools", || call(&app, "inspect", "{}").is_ok());
    let mut startup_cursor = 0;
    let mut startup = Vec::new();
    collect_logs(&app, &mut startup_cursor, &mut startup);
    if drags[0].0 == "blend50" { install_blend(&app); } else {
        let primary = if cfg!(target_os = "macos") { KeyModifiers { shift: true, logo: true, ..Default::default() } } else { KeyModifiers { shift: true, control: true, ..Default::default() } };
        app.press_key_with_modifiers(KeyCode::KeyO, primary);
    }
    app.locator(Selector::role(Role::TreeItem).name(drags[0].0)).wait_visible();
    app.locator(Selector::id("rm_slice")).select();
    app.locator(Selector::id("rm_slice")).wait_checked(true);
    wait_until(&app, "the demo plate to be Sliced", || {
        collect_logs(&app, &mut startup_cursor, &mut startup);
        seen_slice(&app) == "Sliced"
    });
    app.locator(Selector::role(Role::Slider).name("Top layer")).set_value("0.5");
    collect_logs(&app, &mut startup_cursor, &mut startup);
    for line in startup.iter().filter(|l| l.contains("sdf trace")) {
        println!("[fab-vase-live] startup: {}", line.split(" - ").last().unwrap_or(line));
    }
    for &(object, parameter, step_px) in drags {
        app.locator(Selector::role(Role::Tab).name("Parts")).click();
        app.locator(Selector::role(Role::TreeItem).name(object)).wait_visible().click();
        app.locator(Selector::id("t_shape")).wait_visible().click();
        app.locator(Selector::role(Role::SpinButton).name(parameter)).wait_visible();
        // The first drag moves FIRST_PX in all: past the control's drag threshold, so it edits even
        // with a small step (3 steps of 1 px never started a drag and nothing was ever sliced). So
        // does the one after it, which goes on from there.
        let first = (FIRST_PX / step_px).ceil() as usize;
        measure_drag(&app, parameter, step_px, first, "first");
        measure_drag(&app, parameter, step_px, STEPS.saturating_sub(first).max(first), "warm");
    }
}

fn measure_drag(app: &TestApp, parameter: &str, step_px: f64, steps: usize, phase: &str) {
    println!("[fab-vase-live] sample parameter={parameter} phase={phase}");
    remote(app, &["trace".into(), "topics=frames".into()]).expect("frame profile starts");
    println!("[fab-vase-live] frame profile started");
    let before = shown(&app, parameter);
    let mut cursor = app.log_since(0).map(|t| t.last_seq).unwrap_or(0);
    LOST.with(|l| l.set(0));
    let mut all = Vec::new();
    let mut drag = app.locator(Selector::role(Role::SpinButton).name(parameter)).press_and_drag();
    println!("[fab-vase-live] pointer down");
    let drag_start = Instant::now();
    for step in 1..=steps {
        // `move_by` is from where the drag started.
        drag.move_by(step_px * step as f64, 0.0);
        collect_logs(&app, &mut cursor, &mut all);
    }
    let drag_ms = drag_start.elapsed().as_secs_f64() * 1e3;
    drag.release();
    println!("[fab-vase-live] pointer released");
    remote(app, &["trace".into(), "off=1".into()]).expect("drag frame profile stops");
    remote(app, &["trace".into(), "topics=frames".into()]).expect("reslice frame profile starts");
    wait_until(app, &format!("{parameter}/{phase}: the drag to edit the part"), || {
        collect_logs(&app, &mut cursor, &mut all);
        all.iter().any(|l| l.contains("fab live input:"))
    });
    // The whole slice of the value let go at: the commit's input is the last, and the revision
    // answering it reports its work (none, when the last drag step's slice of this value is done).
    let finished = || {
        collect_logs(&app, &mut cursor, &mut all);
        let Some(committed) = all.iter().rposition(|l| l.contains(": committed in ")) else { return false };
        let Some(revision) = all[committed..].iter().rev().find(|l| l.contains("fab live input:")).and_then(|l| number_after(l, "revision=")) else { return false };
        all.iter().any(|l| l.contains("fab live work:") && number_after(l, "revision=") == Some(revision))
            && seen_slice(&app) == "Sliced"
    };
    wait_until(app, "the slice of the final value", finished);
    // The slider's layer for the final value: the frame that draws it writes its ledger line. The
    // last revision that worked drew it; one after that has the same layers and draws nothing.
    let slicing = |all: &[String]| all.iter().rev().filter(|l| l.contains("fab live work:") && !l.contains("traced 0 planned 0")).find_map(|l| number_after(l, "revision="));
    app.try_wait_until_within(HANG, || {
        collect_logs(&app, &mut cursor, &mut all);
        let last = slicing(&all);
        if last.is_some() && all.iter().any(|l| l.contains("fab: viewed layer") && number_after(l, "revision=") == last) {
            return None;
        }
        Some(format!("waiting for {parameter}/{phase}: the slider's layer drawn for the final value"))
    }).unwrap_or_else(|why| {
        // The revisions' ledger, to tell a line the ledger never wrote from a layer never drawn.
        let ledger = ["fab live edit:", "fab live input:", "fab live work:", "fab: viewed layer", "fab live drawn:", ": committed in "];
        for l in all.iter().filter(|l| ledger.iter().any(|k| l.contains(k))).rev().take(120).collect::<Vec<_>>().into_iter().rev() {
            println!("[fab-vase-live] ledger {parameter}/{phase}: {}", l.split(" - ").last().unwrap_or(l));
        }
        panic!("{why}; timed out waiting for {parameter}/{phase}: the slider's layer drawn for the final value (the slice last read {:?}, {} log lines lost)", SEEN.with(|s| s.borrow().clone()), LOST.with(|l| l.get()));
    });

    remote(app, &["trace".into(), "off=1".into()]).expect("frame profile stops");
    collect_logs(&app, &mut cursor, &mut all);
    assert!(!all.iter().any(|l| l.contains("sdf trace lower:")), "{parameter}/{phase} triggered a trace pipeline compile after the drag started");
    for line in all.iter().filter(|l| l.contains("[frame-profile]")) { println!("[fab-vase-live] {parameter}/{phase} {line}"); }
    let edits = all.iter().filter(|l| l.contains("fab live edit:")).count();
    let work: Vec<_> = all.iter().filter(|l| l.contains("fab live work:")).cloned().collect();
    println!("[fab-vase-live] {parameter} {before} -> {}", shown(&app, parameter));
    let Some(edited) = work.iter().find(|l| !l.contains("traced 0 planned 0")).and_then(|l| number_after(l, "part ")).map(|p| p as usize) else {
        for l in all.iter().filter(|l| l.contains("fab ")).rev().take(60).collect::<Vec<_>>().into_iter().rev() {
            println!("[fab-vase-live] {}", l.split(" - ").last().unwrap_or(l));
        }
        panic!("the vase did no work ({edits} edits)");
    };
    // What frames drew: the tiles of the vase and the age of the edit each answers.
    let runs = drawn_runs::runs(&app);
    let tiles: Vec<(u32, f64)> = runs.iter().flat_map(|r| &r.tiles).filter(|t| t.part == edited).map(|t| (t.layer, t.ms)).collect();
    assert!(!tiles.is_empty(), "no layer of the vase was drawn after the drag");
    let view = tiles[0].0;
    // Height changes can move the slider to a different part-layer index.
    // The viewed ledger follows the requested layer and identifies each edit.
    let viewed: std::collections::BTreeMap<u64, (u32, f64)> = all.iter().filter(|l| l.contains("fab: viewed layer")).filter_map(|l| {
        (number_after(l, "part=")? as usize == edited).then(|| Some((number_after(l, "revision=")? as u64,
            (number_after(l, "layer=")? as u32, number_after(l, "total_ms=")?))))?
    }).collect();
    let mut at_view: Vec<f64> = viewed.values().map(|(_, ms)| *ms).collect();
    println!("[fab-vase-live] viewed ledger: {} distinct revisions drawn of {edits} edits", viewed.len());
    let last_edit = all.iter().rposition(|l| l.contains("fab live edit:")).expect("an edit request was logged");
    let since: std::collections::BTreeSet<u64> = all[last_edit..].iter().filter(|l| l.contains("fab live input:")).filter_map(|l| number_after(l, "revision=")).map(|r| r as u64).collect();
    let after_last: Vec<(u32, f64)> = runs.iter().filter(|r| since.contains(&r.revision)).flat_map(|r| &r.tiles).filter(|t| t.part == edited).map(|t| (t.layer, t.ms)).collect();
    let shown = at_view.len();
    at_view.sort_by(f64::total_cmp);
    println!("[fab-vase-live] drag of {steps} steps over {drag_ms:.0} ms: {edits} edits, slider layer {view} drawn for {shown} of them");
    if !at_view.is_empty() {
        println!("[fab-vase-live] edit -> slider layer drawn: median {:.0} ms, p90 {:.0} ms, max {:.0} ms", quantile(&at_view, 0.5), quantile(&at_view, 0.9), at_view[at_view.len() - 1]);
    }
    let last_focus = viewed.last_key_value().map(|(_, (_, ms))| *ms);
    if let (Some(first), Some(last)) = (last_focus, after_last.last()) {
        println!("[fab-vase-live] after the last edit: slider layer at {:.0} ms, {} layers, the last at {:.0} ms (full reslice)", first, after_last.len(), last.1);
    }
    for l in &work {
        println!("[fab-vase-live] {}", l.split(" - ").last().unwrap_or(l));
    }
    let mut distinct: Vec<u32> = after_last.iter().map(|t| t.0).collect();
    distinct.sort_unstable();
    distinct.dedup();
    println!("[fab-vase-live] {} distinct layers drawn after the last edit ({} tiles)", distinct.len(), after_last.len());
    let frames: u64 = runs.iter().filter(|r| since.contains(&r.revision)).map(|r| r.frames).sum();
    let before_whole: u64 = runs.iter().filter(|r| since.contains(&r.revision)).map(|r| r.before_whole).sum();
    let last_tile_revision = runs.iter().flat_map(|r| &r.tiles).last().map(|t| t.revision);
    println!("[fab-vase-live] {frames} frames drew previews after the last edit, {before_whole} tiles before the whole slice, last tile revision {last_tile_revision:?}");
    for line in all.iter().filter(|l| ["fab trace request:", "sdf trace submit:", "sdf trace GPU:", "sdf trace coarse GPU:", "sdf trace work:", "sdf trace lower:", "sdf trace warm:", "sdf trace program:", "sdf trace readback:", "sdf trace chain:", "sdf trace arrival:", "fab: viewed layer"].iter().any(|stage| l.contains(stage))) {
        println!("[fab-vase-live] path {parameter}: {}", line.split(" - ").last().unwrap_or(line));
    }
    let gaps: Vec<_> = all.iter().filter(|l| l.contains("fab live gap:")).collect();
    assert!(gaps.is_empty(), "a frame showed a layer up to the slider empty: {gaps:?}");
    let last_revision = slicing(&all).expect("a revision that worked") as u64;
    assert!(viewed.contains_key(&last_revision), "the slider's layer was not drawn for the final {parameter} revision");
    assert!(work.iter().filter(|l| !l.contains(&format!("part {edited} "))).all(|l| l.contains("traced 0 planned 0")), "an untouched part did work: {work:?}");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

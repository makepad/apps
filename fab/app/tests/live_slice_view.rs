//! The Sliced view playing the print and under edits, as the person sees it. A tower and a post
//! (exact, meshed parts) on two filaments, and so both nozzles: the Slice render mode slices them
//! and names the layer on top. Once the slice's print time is
//! estimated and on screen (the playback bar's total: its timed preview is what plays), the print
//! plays: scrubbed along the middle layer's moves it says what prints there, playing from there
//! the playhead moves on and it still says what prints now.
//!
//! Then a wavy cube (a distance-field part) joins the plate (never emptied while it slices) and
//! the slider goes halfway up. The wavy cube's Size handle is dragged in steps. What the view
//! must do by construction:
//! - the plate stays shown whole while it reslices (a reslice never empties it to regrow it
//!   bottom up; only a settings change regrows it, by design),
//! - the slider layer of the edited part is the first of its new layers drawn,
//! - the layers below it are drawn down to the plate, top down (the layers above it, hidden by
//!   the slider, rank after them),
//! - every frame shows each layer up to the slider (new, or its last beads, in their colours),
//! - the untouched parts cost nothing (no layer traced or planned),
//! - the camera never moves.
//!
//! Then the post's radius is dragged in the part panel: the slice completes (Sliced, never
//! stuck), the post's new layers are drawn, the other parts cost nothing, the plate stays whole
//! and no layer up to the slider goes blank.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

mod common;
#[path = "common/drawn_runs.rs"]
mod drawn_runs;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;

use makepad_strict_json::Value;
use makepad_test::{makepad_test, Role, Selector, TestApp};
use std::time::{Duration, Instant};

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

fn wait_until(app: &TestApp, what: &str, mut ready: impl FnMut() -> bool) {
    wait_poll(app, || {
        if !ready() { Err(format!("timed out waiting for {what}")) } else { Ok(()) }
    });
}

fn log(app: &TestApp, from: u64) -> Vec<String> {
    app.log_since(from).map(|t| t.lines).unwrap_or_default()
}

fn seq(app: &TestApp) -> u64 {
    app.log_since(0).map(|t| t.last_seq).unwrap_or(0)
}

fn number_after(l: &str, key: &str) -> Option<f64> {
    let at = l.find(key)? + key.len();
    l[at..].trim_start().split(|c: char| !(c.is_ascii_digit() || c == '.')).next()?.parse().ok()
}

/// What the view says: the caption's slice state, the playback bar's layer readout
/// (`Layer n / m`) and every visible name.
struct Readout {
    state: String,
    layer: Option<(u32, u32)>,
    names: Vec<String>,
}

fn readout(app: &TestApp) -> Readout {
    let nodes: Vec<_> = app.widget_snapshot().nodes().into_iter().filter(|n| n.visible()).collect();
    let state = common::slice_state(app);
    let names: Vec<String> = nodes.into_iter().map(|n| n.node.name).collect();
    let layer = names.iter().find_map(|n| {
        let (shown, total) = n.strip_prefix("Layer ")?.split_once(" / ")?;
        Some((shown.parse().ok()?, total.split(' ').next()?.parse().ok()?))
    });
    Readout { state, layer, names }
}

/// The playback bar's clock, `shown / total` (`10:13 / 21:01`): the playhead and the print time
/// of the preview on screen. The total reads `0:00` while the preview is the untimed live one.
fn clock(names: &[String]) -> Option<(&str, &str)> {
    let is_clock = |t: &str| !t.is_empty() && t.contains(':') && t.chars().all(|c| c.is_ascii_digit() || c == ':');
    names.iter().find_map(|n| {
        let (shown, total) = n.split_once(" / ")?;
        (is_clock(shown) && is_clock(total)).then_some((shown, total))
    })
}

/// What the view says prints now (`<doing> on the left nozzle  ·  filament 1  ·  ...`), if it says.
fn printing_now(names: &[String]) -> bool {
    names.iter().any(|n| n.contains(" nozzle  ·  filament"))
}

fn camera(app: &TestApp) -> String {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    let value = makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).expect("viewport inspection JSON");
    value.get("camera").expect("viewport camera").to_json()
}

/// The camera once it has not changed for longer than a quarter-second glide.
fn settled_camera(app: &TestApp) -> String {
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

/// The engine revision of the slice input sent at log line `edit` (`fab live edit`, then `fab
/// live input: revision=`).
fn revision_at(lines: &[String], edit: usize) -> Option<u64> {
    lines[edit..].iter().find(|l| l.contains("fab live input:")).and_then(|l| number_after(l, "revision=")).map(|r| r as u64)
}

/// After an edit (log position `from`): waits until the newest slice input sent (`fab live
/// edit`) is sliced whole (its revision's `fab live work`, once a complete slice's toolpaths are
/// in) and the caption says Sliced, checking at every look that the plate is shown whole (its
/// layer count never below `whole - 1`). The log since `from`.
fn wait_whole_slice(app: &TestApp, from: u64, whole: u32, what: &str) -> Vec<String> {
    let mut after = Vec::new();
    wait_until(app, what, || {
        let r = readout(app);
        assert!(!r.state.starts_with("Failed"), "{what}: the slice failed");
        if let Some((_, total)) = r.layer {
            assert!(total + 1 >= whole, "whole plate: {what}: the view shows {total} layers of the {whole} the plate had: a reslice emptied the plate");
        }
        let all = log(app, from);
        let Some(edit) = all.iter().rposition(|l| l.contains("fab live edit:")) else { return false };
        let Some(revision) = revision_at(&all, edit) else { return false };
        let worked = format!("revision={revision}");
        if !all[edit..].iter().any(|l| l.contains("fab live work:") && l.trim_end().ends_with(&worked)) || r.state != "Sliced" {
            return false;
        }
        after = all;
        true
    });
    after
}

/// (part, layer, ms after the edit) of every tile of `revision` frames drew. A newer edit's run can
/// still draw an older revision's tiles (planning already under way when it came).
fn drawn(runs: &[drawn_runs::Run], revision: u64) -> Vec<(usize, u32, f64)> {
    runs.iter().flat_map(|r| &r.tiles).filter(|t| t.revision == revision).map(|t| (t.part, t.layer, t.ms)).collect()
}

const TOWER: &str = r#"use mod.model.*
use mod.fab.*
let m = mod.model
let tower = m.box({size: [24mm 24mm 30mm] radius: 2mm})
object(tower)
"#;

const WAVY: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let wavy_size = param(36mm, min: 20mm, max: 60mm, step: 0.5mm, label: "Size", group: "Wavy cube")
let wavy = m.sdf({
    bounds: [[-wavy_size / 2 - 2mm, -1mm, -wavy_size / 2 - 2mm], [wavy_size / 2 + 2mm, wavy_size + 1mm, wavy_size / 2 + 2mm]]
    cell: 0.5mm
    field: fn(p) {
        let k = 6.2832 / 8mm
        let cube = sdf_box(p - vec3(0, wavy_size / 2, 0), vec3(wavy_size, wavy_size, wavy_size))
        return cube + 0.8mm * sin(p.x * k) * sin(p.y * k)
    }
})
handle(wavy_size, Arrow{from: [wavy_size / 2, 0, wavy_size / 2] to: [wavy_size / 2, wavy_size, wavy_size / 2]})
object(wavy)
"##;

const POST: &str = r#"use mod.model.*
use mod.fab.*
let m = mod.model
let post_radius = param(8mm, min: 4mm, max: 14mm, step: 0.5mm, label: "Radius", group: "Post")
let post = m.cylinder({radius: post_radius height: 24mm segments: 48 at: [0mm 12mm 0mm]})
object(post)
"#;

#[makepad_test]
fn the_print_plays_and_edits_under_the_slider_draw_top_down_keeping_the_plate_whole(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let mut head = String::new();
    wait_until(&app, "Fab tools", || call(&app, "inspect", "{}").map(|l| head = head_of(&text(&l))).is_ok());
    for (file, source) in [("tower.cad.splash", TOWER), ("post.cad.splash", POST)] {
        let batch = format!(r#"{{"request_id":"{file}","expected":{head:?},"script":"{file}","source":{}}}"#, Value::Str(source.to_string()).to_json());
        head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    }
    let edits = "color(\"Orange\", rgb: #xe07020)\ncolor(\"Blue\", rgb: #x2080e0)\nplace(\"tower\", at: [100mm 160mm], color: \"Orange\")\nplace(\"post\", at: [220mm 160mm], color: \"Blue\")";
    let batch = format!(r#"{{"request_id":"place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let reply = text(&call(&app, "place", &batch).expect("place"));
    assert!(reply.contains("solid: \"tower/tower\"") && reply.contains("solid: \"post/post\""), "{reply}");
    // The colours then map onto the tools in a follow-up job of the model's, which sends the plate to
    // the slicer once more and replaces the preview on screen (a new preview parks the playhead at its
    // end). A tool call goes through the model's queue after that job, so once it answers every slice
    // input of this plate has been sent.
    call(&app, "inspect", "{}").expect("inspect");

    // Sliced slices two parts on two filaments; the playback bar names the layer on top.
    // The runtime's actions wait on the app's progress, not a clock: the first Sliced can keep a
    // cold JIT's main thread compiling for longer than a fixed wait gives it to take the select.
    app.locator(Selector::id("rm_slice")).select();
    app.locator(Selector::id("rm_slice")).wait_checked(true);
    wait_until(&app, "the plate Sliced, its layer named", || {
        let r = readout(&app);
        assert!(!r.state.starts_with("Failed") && !r.names.iter().any(|n| n.starts_with("The slice failed")), "two filaments: the slice failed: {:?}", r.names);
        r.state == "Sliced" && r.layer.is_some()
    });

    // ---- the print plays ---------------------------------------------------
    // Once its print time is estimated and on screen (not when the estimate is logged: the log line
    // comes from the slicer's thread, the preview reaches the view a frame or more later, and the
    // slider and the scrub would land on the untimed one): along the middle layer's moves it says
    // what prints there; playing from there the playhead moves on and it says what prints now.
    // The plate's layers are the timed preview's, read off the frame that shows its clock: the
    // caption's state comes from a tool call after the view's snapshot, and next to a state that
    // already says Sliced the snapshot can still show the slice growing (its first layers).
    let mut whole = 0;
    wait_until(&app, "play: the slice's timed preview on screen (the playback bar's total)", || {
        let r = readout(&app);
        let timed = r.state == "Sliced" && clock(&r.names).is_some_and(|(_, total)| total != "0:00");
        whole = r.layer.filter(|_| timed).map_or(0, |(_, total)| total);
        whole > 0
    });
    let slider = Selector::role(Role::Slider).name("Top layer");
    app.locator(slider.clone()).set_value("0.5");
    wait_poll(&app, || {
        let r = readout(&app);
        if r.layer.is_some_and(|(n, _)| n < whole) {
            return Ok(());
        }
        Err(format!("timed out waiting for play: the slider halfway: the view shows layer {:?} (shown / of all) of the {whole} the timed preview had, the caption {:?}, the clock {:?}", r.layer, r.state, clock(&r.names)))
    });
    let (layer, _) = readout(&app).layer.expect("play: the layer readout");
    assert!(layer > 1, "play: the middle of the print is above the first layer, not layer {layer}");
    app.locator(Selector::id("time_scrub")).set_value("0.5");
    let mut scrubbed = String::new();
    wait_until(&app, "play: a readout of what prints at the middle of the layer", || {
        let r = readout(&app);
        let there = printing_now(&r.names);
        if there {
            scrubbed = clock(&r.names).map(|(shown, _)| shown.to_string()).unwrap_or_default();
        }
        there
    });
    app.locator(Selector::id("print_play")).press();
    wait_until(&app, "play: the playhead to move on from the scrub, saying what prints now", || {
        let r = readout(&app);
        printing_now(&r.names) && clock(&r.names).is_some_and(|(shown, _)| shown != scrubbed)
    });
    app.locator(Selector::id("print_pause")).wait_visible();
    app.locator(Selector::id("print_pause")).press();
    app.locator(Selector::id("print_play")).wait_visible();

    // ---- the wavy cube joins the plate; the slider goes halfway --------------
    let from = seq(&app);
    let head = head_of(&text(&call(&app, "inspect", "{}").expect("inspect")));
    let batch = format!(r#"{{"request_id":"wavy","expected":{head:?},"script":"wavy.cad.splash","source":{}}}"#, Value::Str(WAVY.to_string()).to_json());
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    let edits = "place(\"wavy\", at: [160mm 160mm], color: \"Orange\")";
    let batch = format!(r#"{{"request_id":"place-wavy","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let reply = text(&call(&app, "place", &batch).expect("place"));
    assert!(reply.contains("solid: \"wavy/wavy\""), "{reply}");
    wait_whole_slice(&app, from, whole, "the wavy cube added and sliced whole");
    let whole = readout(&app).layer.expect("the layer readout").1;
    app.locator(slider.clone()).set_value("0.5");
    wait_until(&app, "the slider halfway", || readout(&app).layer.is_some_and(|(n, _)| n < whole));

    app.locator(Selector::role(Role::Tab).name("Parts")).press();
    app.locator(Selector::role(Role::TreeItem).name("wavy")).wait_visible();
    app.locator(Selector::role(Role::TreeItem).name("wavy")).press();
    app.locator(Selector::id("t_shape")).wait_visible();
    app.locator(Selector::id("t_shape")).press();
    app.locator(Selector::id("t_shape")).wait_checked(true);
    let handle = Selector::role(Role::Slider).name("Size handle");
    app.locator(handle.clone()).wait_visible();
    let camera_before = settled_camera(&app);

    // ---- the distance-field part's handle, dragged in steps ----------------
    let from = seq(&app);
    for _ in 0..3 {
        app.locator(handle.clone()).drag_by(0.0, -6.0);
        // A look right after each step, while it reslices.
        if let Some((_, total)) = readout(&app).layer {
            assert!(total + 1 >= whole, "whole plate: right after a drag step the view shows {total} layers of the {whole} the plate had: a reslice emptied the plate");
        }
    }
    // Each step is a gesture of its own, committed in the model's queue (a second or more each for
    // this field), after the test went on: a tool call queued behind the commits answers once
    // every one's slice input is sent, so the wait below is for the last.
    call(&app, "inspect", "{}").expect("inspect");
    let all = wait_whole_slice(&app, from, whole, "the dragged wavy cube to be sliced whole");
    assert_eq!(camera(&app), camera_before, "camera: it moved during the edit or its slice");
    // The drawn tiles of the newest edit request that drew any (a later request can be a
    // distance-field re-evaluation that hits every cache and draws nothing), by its revision.
    let edits: Vec<usize> = all.iter().enumerate().filter(|(_, l)| l.contains("fab live edit:")).map(|(i, _)| i).collect();
    assert!(!edits.is_empty(), "an edit request was logged");
    let runs = drawn_runs::runs(&app);
    let revision = edits.into_iter().rev().filter_map(|e| revision_at(&all, e))
        .find(|&r| !drawn(&runs, r).is_empty()).expect("draw order: no tile of the edit was drawn");
    let tiles = drawn(&runs, revision);
    // The plate was on screen while it resliced: frames drew the edit's new layers before its whole
    // slice was in, and no published preview dropped a layer it showed before (`fab live gap`,
    // below).
    for run in runs.iter().filter(|r| r.revision == revision) {
        println!("[fab-live-slice] revision {revision}: {} frames, {} tiles before the whole slice", run.frames, run.before_whole);
    }
    assert!(runs.iter().any(|r| r.revision == revision && r.before_whole > 0), "whole plate: no frame drew the wavy cube's new layers before its whole slice was in");
    let edited = tiles[0].0;
    let own: Vec<&(usize, u32, f64)> = tiles.iter().filter(|d| d.0 == edited).collect();
    let view = own[0].1;
    // What shows: the slider hides the layers above it. Those are planned when no layer below
    // is ready to be (one still waits on its contours), ranked after every layer below.
    let below: Vec<u32> = own.iter().map(|d| d.1).filter(|&l| l < view).collect();
    // Top down: a lower layer is never on screen before a higher one. A band's layers are planned
    // in parallel (up to 64 at a time) and arrive in any order within it: a layer is early when it
    // is on screen before one more than a band above it.
    let at = |l: u32| own.iter().find(|d| d.1 == l).map(|d| d.2).unwrap_or(f64::MAX);
    let early = below.iter().filter(|&&l| below.iter().any(|&h| h > l + 64 && at(l) + 1.0 < at(h))).count();
    let gaps: Vec<&String> = all.iter().filter(|l| l.contains("fab live gap:")).collect();
    assert!(gaps.is_empty(), "never blank: a frame showed a layer up to the slider empty: {gaps:?}");
    let other = tiles.iter().filter(|d| d.0 != edited).count();
    assert!(other == 0, "untouched part: only the edited part drew new tiles, but {other} of {} were another part's", tiles.len());
    assert!(below.len() + 1 >= view as usize, "draw order: every layer below the slider was drawn ({} of {view})", below.len());
    assert!(early == 0, "draw order: the layers below the slider came top down ({early} of {} early)", below.len());
    let work: Vec<&String> = all.iter().filter(|l| l.contains("fab live work:")).collect();
    assert!(work.iter().filter(|l| !l.contains(&format!("part {edited} "))).all(|l| l.contains("traced 0 planned 0")), "untouched parts: another part did work: {work:?}");

    // ---- the exact part's radius, dragged in the part panel ----------------
    let whole = readout(&app).layer.expect("the layer readout").1;
    app.locator(Selector::role(Role::TreeItem).name("post")).wait_visible();
    app.locator(Selector::role(Role::TreeItem).name("post")).press();
    let control = Selector::role(Role::SpinButton).name("Radius");
    let radius = app.locator(control.clone()).wait_visible().snapshot().node.value;
    let from = seq(&app);
    let mut drag = app.locator(control.clone()).press_and_drag();
    drag.move_by(30.0, 0.0);
    wait_until(&app, "the post's radius to follow the drag", || app.locator(control.clone()).snapshot().node.value != radius);
    drag.release();
    // A distance-field part's late re-evaluations also send slice inputs (each costing nothing):
    // the post's new radius has reached the slicer once a part is traced again.
    wait_until(&app, "mesh edit: the post's new radius sliced (a part traced again)", || {
        log(&app, from).iter().any(|l| l.contains("fab live work:") && !l.contains("traced 0 planned 0"))
    });
    let all = wait_whole_slice(&app, from, whole, "mesh edit: the post's slice to complete (Sliced, never stuck)");
    assert_eq!(camera(&app), camera_before, "camera: it moved during the mesh edit or its slice");
    let work: Vec<&String> = all.iter().filter(|l| l.contains("fab live work:")).collect();
    let busy: Vec<&&String> = work.iter().filter(|l| !l.contains("traced 0 planned 0")).collect();
    let edited = busy.first().and_then(|l| number_after(l, "part ")).map(|p| p as usize).expect("mesh edit: the edited post did work");
    // From the gesture's first request: its commit may hit every cache and draw nothing new.
    let revisions: std::collections::BTreeSet<u64> = all.iter().enumerate().filter(|(_, l)| l.contains("fab live edit:")).filter_map(|(e, _)| revision_at(&all, e)).collect();
    let runs = drawn_runs::runs(&app);
    let own: Vec<(usize, u32, f64)> = revisions.iter().flat_map(|&r| drawn(&runs, r)).filter(|d| d.0 == edited).collect();
    let gaps: Vec<&String> = all.iter().filter(|l| l.contains("fab live gap:")).collect();
    assert!(gaps.is_empty(), "never blank: a frame showed a layer up to the slider empty: {gaps:?}");
    assert!(!own.is_empty(), "mesh edit: the edited post's new layers were never drawn");
    assert!(busy.iter().all(|l| l.contains(&format!("part {edited} "))), "untouched parts: another part did work: {work:?}");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

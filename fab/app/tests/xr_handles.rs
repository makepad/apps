//! A shape handle dragged in the headset, end to end, on a synthetic headset: the XR server's
//! selftest head (`MAKEPAD_XR_REMOTE_SELFTEST=1`) and Fab's scripted handle drag
//! (`FAB_XR_HANDLE_SYNTH=1`, set here; see `HandleSynth` in `src/xr.rs`). The demo vase goes on
//! the plate as its second object, with its own placement (the bracket is the first); the right
//! controller selects it with a trigger click, takes its belly ring by the grip, carries it 10 mm
//! outward and lets go.
//!
//! It asserts what a person sees: the ring stays with the hand every frame of the drag (the
//! drawn grip against the hand), the part rebuilds while it moves (the newest-wins parameter
//! previews), the release commits the value (the belly is wider, the vase's footprint follows),
//! and the handles sit on the part as the headset draws it: each grip on the part's surface,
//! and in both eyes the grip as the handles draw it lands where the part draws that surface
//! point.
//!
//! The test starts its own synthetic headset on the native GPU.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_script::syntax::{self, Node};
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Selector, TestApp, TestConfig};
use std::time::Duration;

const VASE: &str = include_str!("../resources/vase.cad.splash");
const BRACKET: &str = include_str!("../resources/bracket.cad.splash");

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

fn splash(text: &str) -> Node {
    syntax::parse_value(text).expect("a Splash reply")
}

fn head_of(reply: &str) -> String {
    splash(reply).field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// A declared parameter's value in millimetres (fab.params; Splash lengths are metres).
fn param(app: &TestApp, key: &str) -> Option<f64> {
    let v = splash(&text(&call(app, "params", "{}").ok()?));
    for part in v.field("parts")?.value.items()? {
        for p in part.field("params")?.value.items()? {
            if p.field("key").and_then(|f| f.value.string()) == Some(key) {
                return p.field("value")?.value.number().map(|x| x * 1000.0);
            }
        }
    }
    None
}

/// The vase's size on the plate (mm).
fn plate_parts(app: &TestApp) -> usize {
    call(app, "inspect", r#"{"program":false}"#).ok().and_then(|l| splash(&text(&l)).field("plate").and_then(|p| p.value.field("parts")).and_then(|f| f.value.items()).map(|p| p.len())).unwrap_or(0)
}

fn size(app: &TestApp) -> Option<[f64; 3]> {
    let v = splash(&text(&call(app, "inspect", r#"{"program":false}"#).ok()?));
    let parts = v.field("plate")?.value.field("parts")?.value.items()?;
    let vase = parts.iter().find(|p| p.field("solid").and_then(|f| f.value.string()) == Some("vase/vase"))?;
    let s = vase.field("size")?.value.items()?;
    Some([s[0].number()? * 1000.0, s[1].number()? * 1000.0, s[2].number()? * 1000.0])
}

fn wait<T>(app: &TestApp, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    wait_poll(app, || f().ok_or_else(|| format!("waiting for {what}")))
}

fn log(app: &TestApp) -> Vec<String> {
    app.log_since(0).map(|t| t.lines).unwrap_or_default()
}

/// The number after `prefix` in `line`.
fn number_after(line: &str, prefix: &str) -> Option<f64> {
    let rest = &line[line.find(prefix)? + prefix.len()..];
    rest.trim_start().split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).next()?.parse().ok()
}

/// The vase as the plate's second object, placed on its own beside the bracket.
fn drag(app: TestApp) {
    app.locator(Selector::role(makepad_test::Role::Window)).wait_visible();
    let line = wait(&app, "the fab tools", || call(&app, "inspect", "{}").ok());
    let mut head = head_of(&text(&line));
    for (file, source, edits) in [
        ("bracket.cad.splash", BRACKET, r#"place("bracket", at: [150mm, 165mm], name: "Shelf bracket")"#),
        ("vase.cad.splash", VASE, r#"place("vase", at: [222mm, 165mm], name: "Bud vase")"#),
    ] {
        let batch = format!(r#"{{"request_id":"xrh-{file}","expected":{head:?},"script":{file:?},"source":{}}}"#, Value::Str(source.to_string()).to_json());
        head = head_of(&text(&call(&app, "script", &batch).expect("script")));
        let batch = format!(r#"{{"request_id":"xrh-place-{file}","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
        let reply = text(&call(&app, "place", &batch).expect("place"));
        head = head_of(&reply);
    }
    wait(&app, "the two parts", || (plate_parts(&app) == 2).then_some(()));
    let belly0 = wait(&app, "the belly", || param(&app, "vase/vase_belly"));
    let size0 = wait(&app, "the vase on the plate", || size(&app));
    println!("[fab-xr-handles] belly {belly0} mm, size {size0:?}");

    // the script: select at 1.2 s, take the ring at 2.4 s, carry it to 4.6 s, let go at 5 s
    let mut got = None;
    app.try_wait_until_within(HANG, || {
        if let Some(b) = param(&app, "vase/vase_belly").filter(|b| *b > belly0 + 0.5) {
            got = Some(b);
            return None;
        }
        Some("waiting for the drag's commit".to_string())
    }).unwrap_or_else(|why| {
        for l in log(&app).iter().filter(|l| l.contains("fab xr") || l.contains("fab param") || l.contains("fab: ")) {
            println!("[app] {}", &l[l.find(" - ").map_or(0, |i| i + 3)..]);
        }
        panic!("{why}; timed out waiting for the drag's commit");
    });
    let belly1 = got.expect("the wait ended on an answer");
    let size1 = wait(&app, "the wider vase", || size(&app).filter(|s| s[0] > size0[0] + 1.0));
    println!("[fab-xr-handles] belly {belly0} -> {belly1} mm, size {size0:?} -> {size1:?}");
    // the hand went 10 mm out and the belly ring moves 1 mm per mm (snapped to 0.5 mm)
    assert!((belly1 - belly0 - 10.0).abs() <= 1.0, "the belly followed the hand 10 mm out: {belly0} -> {belly1}");
    assert!((size1[0] - size0[0] - 2.0 * (belly1 - belly0)).abs() < 2.5, "the footprint is the belly's diameter: {size0:?} -> {size1:?}");
    // the handles' placement is measured after the commit
    wait(&app, "the handles' placement measured after the commit", || {
        log(&app).iter().any(|l| l.contains("fab xr handle align:") && !l.contains("(building)") && l.contains("vase/vase_belly ") && !l.contains("21.0 mm")).then_some(())
    });

    let lines = log(&app);
    for l in lines.iter().filter(|l| l.contains("fab xr handle") || l.contains("fab param preview") || l.contains("fab: parameter")) {
        println!("[app] {}", &l[l.find(" - ").map_or(0, |i| i + 3)..]);
    }
    // the ring stays with the hand: every frame logged while it moves (after the first 0.3 s of
    // the hold, which waits for the hand's first step), the drawn grip is at the hand
    let follow: Vec<(f64, f64)> = lines
        .iter()
        .filter(|l| l.contains("fab xr handle synth: t=") && l.contains("vase/vase_belly"))
        .filter_map(|l| Some((number_after(l, "t=")?, number_after(l, "drawn grip")?)))
        .collect();
    assert!(follow.len() >= 5, "the drag was logged while it moved: {follow:?}");
    // A frame draws the value the hand set in the frame before it: within the drag's 1 mm snap
    // steps plus a frame's travel (the synth carries the hand 10 mm over 2.6..4.6 s, half a mm a
    // 0.1 s frame), 1.5 mm. A stalled main thread (a loaded box, cold code) leaves a gap with no
    // frame, the hand going on: what it moved past a frame's half mm since the frame before (an
    // `all` line, logged a 0.1 s at most) is the ring's to catch up.
    let frames: Vec<f64> = lines.iter().filter(|l| l.contains("fab xr handle synth: all t=")).filter_map(|l| number_after(l, "t=")).collect();
    let carried = |t: f64| 10.0 * ((t - 2.6) / 2.0).clamp(0.0, 1.0);
    for (t, d) in follow.iter().filter(|(t, _)| *t >= 2.8) {
        // the ring is taken at 2 s: no frame before that one counts
        let before = frames.iter().copied().filter(|f| f < t).fold(2.0, f64::max);
        let trail = 1.5 + (carried(*t) - carried(before) - 0.5).max(0.0);
        assert!(*d < trail, "at t={t} the drawn ring is {d} mm from the hand (the frame before at t={before}: it may trail {trail:.2} mm)");
    }
    // the part rebuilt while it moved: the live value compiled, for its field and every handle
    let previews = lines.iter().filter(|l| l.contains("fab param live vase/vase_belly")).count();
    assert!(previews >= 2, "the vase rebuilt live while the ring moved ({previews} live compiles)");
    assert!(lines.iter().any(|l| l.contains("fab: parameter vase/vase_belly: committed")), "the release committed the belly");
    // the handles sit on the part as drawn, in both eyes
    // (a value not built yet is ahead of the part: those frames are not compared)
    let align: Vec<&String> = lines.iter().filter(|l| l.contains("fab xr handle align:") && !l.contains("(building)")).collect();
    assert!(align.iter().any(|l| l.contains("vase/vase_belly ") && !l.contains("21.0 mm")), "the handles' placement was measured after the commit");
    for l in &align {
        let surface = number_after(l, "surface").expect("a surface distance");
        // the handles the script places (`handle(...)`) are on the vase; Wall's is the mouth
        // cutter's ring (a tool body, above the rim)
        if ["vase/vase_height ", "vase/vase_belly ", "vase/vase_neck "].iter().any(|k| l.contains(k)) {
            assert!(surface < 3.0, "a grip off the part's surface: {l}");
        }
        let eyes = &l[l.find("eyes [").expect("eyes") + 6..];
        let eyes = &eyes[..eyes.find(']').unwrap()];
        for px in eyes.split(", ") {
            let px: f64 = px.parse().unwrap_or_else(|_| panic!("a grip not in view: {l}"));
            assert!(px <= 3.0 + 3.0 * surface, "the handle is drawn off the part in an eye: {l}");
        }
    }
}

#[test]
fn a_ring_dragged_in_the_headset_follows_the_hand_rebuilds_and_commits_where_the_vase_is() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "a_ring_dragged_in_the_headset_follows_the_hand_rebuilds_and_commits_where_the_vase_is").unwrap();
    config.env.insert("MAKEPAD_XR_REMOTE_SELFTEST".into(), "1".into());
    config.env.insert("MAKEPAD_XR_STREAM".into(), "on".into());
    config.env.insert("MAKEPAD_LAN".into(), "1".into());
    config.env.insert("FAB_XR_HANDLE_SYNTH".into(), "1".into());
    config.env.insert("FAB_PARAM_LOG".into(), "1".into());
    config.env.insert("FAB_XR_HANDLE_KEY".into(), "vase/vase_belly".into());
    config.env.insert("FAB_XR_HANDLE_PART".into(), "1".into());
    if let Err(e) = run_with_config(config, drag) {
        panic!("{}", e.message());
    }
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

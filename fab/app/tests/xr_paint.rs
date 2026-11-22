//! A painted part keeps its paint in the headset, whichever look the plate has: the part's
//! script paints a region red, the XR pass (a synthetic headset looking down at the plate) has
//! clearly coloured pixels where the part is, as its field and as its mesh.
//!
//! The test sets the variables itself and starts its own synthetic headset on the native GPU.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};

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

/// A part with a red region on it (a box of the part's own size, the print colour grey).
const RED: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let a = m.box({size: [40mm 40mm 40mm]})
object(a)
m.fill(m.box({size: [100mm 100mm 100mm]}), Material{albedo: [0.95 0.05 0.05] name: "Red"})
"##;

/// The coloured pixels the XR pass shows in the left eye, at most, over the run.
fn coloured(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || call(&app, "inspect", "{}"));
    let head = head_of(&text(&line));
    let batch = format!(r#"{{"request_id":"paint","expected":{head:?},"script":"red.cad.splash","source":{}}}"#, Value::Str(RED.to_string()).to_json());
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    let edits = Value::Str(r#"place("red", at: [175mm 160mm])"#.to_string()).to_json();
    let reply = text(&call(&app, "place", &format!(r#"{{"request_id":"paint-place","expected":{head:?},"edits":{edits}}}"#)).expect("place"));
    assert!(reply.contains("red/"), "the part is placed: {reply}");
    let (mut seq, mut best) = (0, 0usize);
    wait_poll(&app, || {
        if let Ok(t) = app.log_since(seq) {
            seq = t.last_seq;
            for l in &t.lines {
                if let Some(n) = l.split("fab xr pass: left eye ").nth(1).and_then(|r| r.split(" solid px, ").nth(1)).and_then(|r| r.split(' ').next()).and_then(|n| n.parse::<usize>().ok()) {
                    best = best.max(n);
                }
            }
        }
        if best >= 50 { Ok(()) } else { Err(format!("the red part never showed in the headset: at most {best} coloured pixels")) }
    });
}

fn run(look: &str, name: &str) {
    let dir = std::env::temp_dir().join(format!("fab-xr-paint-{look}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let prefs = dir.join("view.prefs");
    std::fs::write(&prefs, format!("look={look}\n")).unwrap();
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), name).unwrap();
    for (k, v) in [
        ("MAKEPAD_XR_REMOTE_SELFTEST", "1"),
        ("MAKEPAD_XR_REMOTE_SELFTEST_STILL", "1"),
        ("MAKEPAD_XR_REMOTE_SELFTEST_PITCH", "-0.7"),
        ("MAKEPAD_XR_STREAM", "on"),
        ("MAKEPAD_LAN", "1"),
        ("FAB_XR_PASS_STATS", "1"),
    ] {
        config.env.insert(k.into(), v.into());
    }
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let r = run_with_config(config, coloured);
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = r {
        panic!("{}", e.message());
    }
}

#[test]
fn a_painted_part_is_coloured_in_the_headset_as_a_field() {
    run("Field", "a_painted_part_is_coloured_in_the_headset_as_a_field");
}

#[test]
fn a_painted_part_is_coloured_in_the_headset_as_a_mesh() {
    run("Mesh", "a_painted_part_is_coloured_in_the_headset_as_a_mesh");
}

/// The app main thread's CPU budget per wait.
const HANG: std::time::Duration = std::time::Duration::from_secs(300);

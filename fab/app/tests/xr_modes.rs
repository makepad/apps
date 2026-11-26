//! The headset's wrist menu, on a synthetic headset: the XR server's selftest head
//! (`MAKEPAD_XR_REMOTE_SELFTEST=1`) and Fab's scripted controllers (`FAB_XR_MECH_SYNTH=pace`,
//! the script's Mechanics-alone variant, see `MechSynth` in `src/xr.rs`): the right hand aims at
//! the wrist menu's rows and pulls the trigger, as a person does. On a plate with a small
//! chainmail sheet the Real size row takes the view to 1:1, and the Mechanics row switches the
//! workspace (the desktop's follows) and brings the sim up running the sheet's links.
//!
//! The test sets the variables itself and starts its own synthetic headset on the native GPU.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use std::time::Duration;

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

fn bodies(line: &str) -> usize {
    line.split("bodies=").nth(1).and_then(|r| r.split(' ').next()).and_then(|n| n.parse().ok()).unwrap_or_else(|| panic!("{line}"))
}

fn wrist_menu(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || {
        if let Ok(l) = call(&app, "inspect", "{}") {
            return Ok(l);
        }
        Err("the fab tools never appeared".to_string())
    });
    let head = head_of(&text(&line));
    // the Add menu's chainmail sheet at its smallest: a few links are bodies enough
    let source = include_str!("../resources/chainmail.cad.splash").replace("param(80mm", "param(30mm").replace("param(60mm", "param(30mm");
    let batch = format!(r#"{{"request_id":"xr-modes","expected":{head:?},"script":"chainmail.cad.splash","source":{}}}"#, Value::Str(source).to_json());
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    let batch = format!(r#"{{"request_id":"xr-modes-place","expected":{head:?},"edits":{}}}"#, Value::Str(r#"place("chainmail", at: [175mm 160mm])"#.to_string()).to_json());
    let reply = text(&call(&app, "place", &batch).expect("place"));
    assert!(reply.contains("solid: \"chainmail/"), "the sheet is placed: {reply}");

    // the script's marks, as the app logs them
    let (mut seq, mut log) = (0, Vec::<String>::new());
    let rest = wait_poll(&app, || {
        { let t = app.log_since(seq).expect("the app's log");
            seq = t.last_seq;
            log.extend(t.lines);
        }
        if let Some(l) = log.iter().find(|l| l.contains("fab xr mech synth: mechanics_rest:")) {
            return Ok(l.clone());
        }
        Err(format!("the wrist menu's Mechanics row never came: {:?}", log.iter().filter(|l| l.contains("fab xr")).collect::<Vec<_>>()))
    });
    let real = log.iter().filter(|l| l.contains("fab xr: real size, scale 1.000")).count();
    assert_eq!(real, 1, "the Real size row takes the view to 1:1 once: {:?}", log.iter().filter(|l| l.contains("fab xr: real size")).collect::<Vec<_>>());
    assert!(rest.contains("workspace Mechanics") && rest.contains("ready=true"), "the Mechanics row chose the desktop's workspace and the sim came up: {rest}");
    assert!(bodies(&rest) > 1, "the sheet's links are bodies: {rest}");
    assert!(rest.contains("running=true"), "Mechanics runs the sheet as soon as it is up: {rest}");
}

/// A shelf off a wall, its supports planned: what the desktop's Supports test uses.
const SHELF: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet wall = m.box({size: [5mm 25mm 20mm] at: [2.5mm 12.5mm 10mm]})\nlet slab = m.box({size: [40mm 5mm 20mm] at: [20mm 27.5mm 10mm]})\nlet shelf = m.union(wall, slab)\nobject(shelf)\n";

/// The Supports row, and then the hands: the left takes the part up and round, the right pulls a
/// piece out, and each goes on while the other lets go.
fn two_hands(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || {
        if let Ok(l) = call(&app, "inspect", "{}") {
            return Ok(l);
        }
        Err("the fab tools never appeared".to_string())
    });
    let head = head_of(&text(&line));
    let batch = format!(r#"{{"request_id":"xr-shelf","expected":{head:?},"script":"shelf.cad.splash","source":{}}}"#, Value::Str(SHELF.to_string()).to_json());
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    let edits = Value::Str(r#"settings({enable_support: "1" support_threshold_angle: "45"})"#.to_string()).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"xr-shelf-settings","expected":{head:?},"edits":{edits}}}"#)).expect("settings");

    let (mut seq, mut log) = (0, Vec::<String>::new());
    let line = wait_poll(&app, || {
        { let t = app.log_since(seq).expect("the app's log");
            seq = t.last_seq;
            log.extend(t.lines);
        }
        if let Some(l) = log.iter().find(|l| l.contains("fab xr mech synth: hands:")) {
            return Ok(l.clone());
        }
        Err(format!("the hands never came: {:?}", log.iter().filter(|l| l.contains("fab xr") || l.contains("fab mech") || l.contains("fab loose")).collect::<Vec<_>>()))
    });
    println!("{line}");
    let number = |after: &str| -> f32 { line.split(after).nth(1).and_then(|r| r.split(' ').next()).and_then(|n| n.parse().ok()).unwrap_or_else(|| panic!("no {after:?} in {line}")) };
    let (rose, turned, away, stays, back) = (number("part rose "), number("turned "), number("piece "), number("let go at "), number("went back "));
    assert!(back < 1.0, "the hand takes the part where the sim has it, and it never goes back to where the view showed it a step before: {line}");
    assert!((rose - 80.0).abs() < 8.0, "the part goes up with the hand: {line}");
    assert!((turned - 30.0).abs() < 8.0, "the part turns with the hand: {line}");
    assert!(away > 50.0, "the piece comes out with the other hand: {line}");
    assert!((stays - rose).abs() < 8.0, "the part stays where the hand left it: {line}");
}

#[test]
fn one_hand_carries_the_part_and_the_other_pulls_a_support_piece_out() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "one_hand_carries_the_part_and_the_other_pulls_a_support_piece_out").unwrap();
    config.env.insert("MAKEPAD_XR_REMOTE_SELFTEST".into(), "1".into());
    config.env.insert("MAKEPAD_XR_STREAM".into(), "on".into());
    config.env.insert("MAKEPAD_LAN".into(), "1".into());
    config.env.insert("FAB_XR_MECH_SYNTH".into(), "supports".into());
    if let Err(e) = run_with_config(config, two_hands) {
        panic!("{}", e.message());
    }
}

#[test]
fn the_wrist_menu_takes_the_headset_to_real_size_and_mechanics() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "the_wrist_menu_takes_the_headset_to_real_size_and_mechanics").unwrap();
    config.env.insert("MAKEPAD_XR_REMOTE_SELFTEST".into(), "1".into());
    config.env.insert("MAKEPAD_XR_STREAM".into(), "on".into());
    config.env.insert("MAKEPAD_LAN".into(), "1".into());
    config.env.insert("FAB_XR_MECH_SYNTH".into(), "pace".into());
    config.env.insert("FAB_XR_MECH_PACE_S".into(), "0".into());
    if let Err(e) = run_with_config(config, wrist_menu) {
        panic!("{}", e.message());
    }
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

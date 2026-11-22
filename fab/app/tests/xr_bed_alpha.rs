//! The ground plate is half see-through in the headset: in the XR pass (a synthetic headset
//! looking down at the plate) its pixels are about half covered, their colour premultiplied for
//! that alpha (not darkened twice), the part standing on it stays fully covered, and the plate's
//! faces do not add up to more than half where they meet; as the parts' fields and as their meshes.
//! The plate shows only with the stream panel's "Ground plate" switch on: off, the default, the
//! real table is the ground and no plate is drawn. One launch checks both: the default first,
//! then the switch turned on in the panel as a person turns it on.
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

/// One of the pass's left-eye lines (four a second).
#[derive(Default)]
struct Pass {
    solid: usize,
    half: usize,
    between: usize,
    /// The half covered pixels' mean red, green, blue and alpha (bytes, colour premultiplied).
    half_mean: [u64; 4],
}

/// The pass with the Ground plate switch off (the default), and then on.
fn pass_numbers(app: TestApp) -> (Pass, Pass) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || call(&app, "inspect", "{}"));
    let head = head_of(&text(&line));
    let batch = format!(r#"{{"request_id":"bed","expected":{head:?},"script":"red.cad.splash","source":{}}}"#, Value::Str(RED.to_string()).to_json());
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    let edits = Value::Str(r#"place("red", at: [175mm 160mm])"#.to_string()).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"bed-place","expected":{head:?},"edits":{edits}}}"#)).expect("place");
    let mut seq = 0;
    // the part drawn: what the pass shows of the default
    let off = settled(&app, &mut seq, |p| p.solid >= 500);
    app.locator(Selector::role(Role::Switch).name("Stream to a device")).show_menu();
    app.locator(Selector::role(Role::Switch).name("Ground plate")).wait_visible().toggle();
    // the plate drawn under the part
    let on = settled(&app, &mut seq, |p| p.half >= 20_000 && p.solid >= 500);
    (off, on)
}

/// The first pass line from the log's `seq` on that `ready` takes: the pass settled into what
/// the checks read. None in 40 lines (10 s): the one with the most half covered pixels, for the
/// checks to say what is missing.
fn settled(app: &TestApp, seq: &mut u64, ready: impl Fn(&Pass) -> bool) -> Pass {
    let (mut best, mut lines) = (Pass::default(), 0);
    wait_poll(app, || {
        if let Ok(t) = app.log_since(*seq) {
            *seq = t.last_seq;
            for l in &t.lines {
                let Some(r) = l.split("fab xr pass: left eye ").nth(1) else { continue };
                let nums = |s: &str| -> Vec<u64> { s.split(|c: char| !c.is_ascii_digit()).filter(|w| !w.is_empty()).filter_map(|w| w.parse().ok()).collect() };
                let (head, mean) = r.split_once("half mean rgba").unwrap_or((r, ""));
                let (n, m) = (nums(head), nums(mean));
                if n.len() < 4 || m.len() != 4 {
                    continue;
                }
                lines += 1;
                let p = Pass { solid: n[0] as usize, half: n[2] as usize, between: n[3] as usize, half_mean: [m[0], m[1], m[2], m[3]] };
                if ready(&p) {
                    return Ok(p);
                }
                if p.half >= best.half {
                    best = p;
                }
            }
        }
        if lines >= 40 { Ok(std::mem::take(&mut best)) } else { Err(format!("{lines} pass lines so far, none settled")) }
    })
}

fn check((off, p): (Pass, Pass)) {
    // the default: the switch off, the part drawn and no plate under it
    assert!(off.solid >= 500, "with the Ground plate switch off (the default) the part is drawn: {} px", off.solid);
    assert!(off.half < 2_000, "with the Ground plate switch off (the default) no plate is drawn: {} px half covered", off.half);
    // the switch on, the plate: its pixels about half covered, a lot of them
    assert!(p.half >= 20_000, "the plate shows half covered: {} px", p.half);
    let a = p.half_mean[3];
    assert!((120..=136).contains(&a), "the plate's alpha is about 0.5: {a}/255");
    // premultiplied for that alpha: no channel above alpha, and the straight colour is the plate's
    // lit grey (a flat 0.2 lit about 0.85), not darkened twice nor a bright fringe
    for c in &p.half_mean[..3] {
        let straight = c * 255 / a.max(1);
        assert!((30..=64).contains(&straight), "the plate's colour is right for its alpha: straight {straight}/255 of {:?}", p.half_mean);
    }
    // the part stands on it fully covered, and no pass pixels add up past a half where the plate's faces meet
    assert!(p.solid >= 500, "the part on the plate stays fully covered: {} px", p.solid);
    assert!(p.between <= p.half / 100 + 200, "pixels between half and full cover are only edges: {} against {} half", p.between, p.half);
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
    let r = run_with_config(config, |app| check(pass_numbers(app)));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = r {
        panic!("{}", e.message());
    }
}

#[test]
fn the_ground_plate_is_half_see_through_in_the_headset_under_field_parts() {
    run("Field", "the_ground_plate_is_half_see_through_in_the_headset_under_field_parts");
}

#[test]
fn the_ground_plate_is_half_see_through_in_the_headset_under_mesh_parts() {
    run("Mesh", "the_ground_plate_is_half_see_through_in_the_headset_under_mesh_parts");
}

/// The app main thread's CPU budget per wait.
const HANG: std::time::Duration = std::time::Duration::from_secs(300);

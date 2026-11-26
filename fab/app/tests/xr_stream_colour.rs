//! The stream keeps the headset's colour and its coverage band through the real encode and
//! decode: a painted part in the synthetic headset, the server's own decoder (macOS,
//! `MAKEPAD_XR_REMOTE_SELFTEST_DECODE=1`) decodes the stream and measures the picture the host's
//! dump measures packed; the decoded picture's chroma and coverage band match the packed one's,
//! the ground plate's half cover (about 0.5 in the band) included: the test turns the stream's
//! "Ground plate" switch on, which is off by default.
//!
//! The dumped picture is the first one packed after the XR pass has drawn the painted part: the
//! test asks for it then (`MAKEPAD_XR_REMOTE_DUMP_AT=asked`, a `dump-request` file in the dump
//! folder), counted in pictures. A fixed 300th picture came before the scene had drawn on a box
//! with two cores, and its blank band failed the half-cover check (11-22). The decoder measures
//! that very picture (its pts, in the left eye's stream alone), so a scene still settling compares
//! like with like.
//!
//! The test sets the variables itself and starts its own synthetic headset on the native GPU.

#![cfg(target_os = "macos")]

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

/// The coloured pixels the XR pass shows in the left eye, at most, over the run; once they show,
/// the dump is asked for (`request`), and the packed and decoded numbers of that picture compared.
fn coloured(app: TestApp, request: &std::path::Path) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || call(&app, "inspect", "{}"));
    let head = head_of(&text(&line));
    let batch = format!(r#"{{"request_id":"paint","expected":{head:?},"script":"red.cad.splash","source":{}}}"#, Value::Str(RED.to_string()).to_json());
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    let edits = Value::Str(r#"place("red", at: [175mm 160mm])"#.to_string()).to_json();
    let reply = text(&call(&app, "place", &format!(r#"{{"request_id":"paint-place","expected":{head:?},"edits":{edits}}}"#)).expect("place"));
    assert!(reply.contains("red/"), "the part is placed: {reply}");
    let (mut seq, mut best, mut asked, mut log_seen) = (0, 0usize, false, Vec::<String>::new());
    wait_poll(&app, || {
        { let t = app.log_since(seq).expect("the app's log");
            seq = t.last_seq;
            log_seen.extend(t.lines.iter().filter(|l| l.contains("xr selftest: ") || l.contains("xr host: dump") || l.contains("tile")).cloned());
            for l in &t.lines {
                if let Some(n) = l.split("fab xr pass: left eye ").nth(1).and_then(|r| r.split(" solid px, ").nth(1)).and_then(|r| r.split(' ').next()).and_then(|n| n.parse::<usize>().ok()) {
                    best = best.max(n);
                }
            }
        }
        // the scene has drawn: the next picture packed is the one to measure
        if best >= 50 && !asked {
            std::fs::write(request, b"").expect("the dump request is written");
            asked = true;
        }
        if let Some(l) = log_seen.iter().find(|l| l.contains("the dumped picture was not decoded") || l.contains("xr host: dump: ")) {
            panic!("the dumped picture was not measured: {l}");
        }
        if asked && log_seen.iter().any(|l| l.contains("xr selftest: decoded")) && log_seen.iter().any(|l| l.contains("xr host: dumped")) {
            let find = |what: &str| log_seen.iter().find(|l| l.contains(what)).cloned().unwrap_or_default();
            compare(&find("xr host: dumped"), &find("xr selftest: decoded"));
            Ok(())
        } else { Err(format!("not yet: {best} coloured px, dump asked: {asked}, {} dump/decode lines", log_seen.len())) }
    });
}

/// The number after `key` in the left eye's part of `line`.
fn left(line: &str, key: &str) -> f64 {
    let eye = line.split("left eye chroma: ").nth(1).and_then(|r| r.split("; right eye").next()).unwrap_or_default();
    eye.split(key).nth(1).and_then(|r| r.trim_start().split(|c: char| !(c.is_ascii_digit() || c == '.')).next()).and_then(|n| n.parse().ok()).unwrap_or_else(|| panic!("no {key:?} in {line}"))
}

/// What the encoder and decoder did to the packed picture's left eye: its strongest chroma and its
/// coverage band stay.
fn compare(packed: &str, decoded: &str) {
    let (p_dev, d_dev) = (left(packed, "largest "), left(decoded, "largest "));
    assert!(p_dev >= 40.0, "the packed picture carries strong colour: {packed}");
    assert!(d_dev >= p_dev * 0.8, "the decoded picture keeps the colour: chroma {p_dev} -> {d_dev}\n{packed}\n{decoded}");
    let (p_cov, d_cov) = (left(packed, "coverage mean "), left(decoded, "coverage mean "));
    assert!((d_cov - p_cov).abs() <= p_cov * 0.15, "the decoded picture keeps the coverage band: mean {p_cov} -> {d_cov}\n{packed}\n{decoded}");
    // the ground plate is half see-through: its share of the band reads half, packed and decoded
    let (p_half, d_half) = (left(packed, "half "), left(decoded, "half "));
    assert!(p_half >= 1.0, "the packed band has the plate's half cover: {p_half}%\n{packed}");
    assert!((d_half - p_half).abs() <= p_half * 0.3, "the decoded band keeps the half cover: {p_half}% -> {d_half}%\n{packed}\n{decoded}");
    let (p_drawn, d_drawn) = (left(packed, "drawn "), left(decoded, "drawn "));
    assert!((d_drawn - p_drawn).abs() <= 2.0, "the coverage's reach stays: {p_drawn}% -> {d_drawn}%");
}

fn run(name: &str) {
    let dir = std::env::temp_dir().join(format!("fab-xr-decode-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let prefs = dir.join("view.prefs");
    std::fs::write(&prefs, "look=Field\n").unwrap();
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), name).unwrap();
    for (k, v) in [
        ("MAKEPAD_XR_REMOTE_SELFTEST", "1"),
        ("MAKEPAD_XR_REMOTE_SELFTEST_STILL", "1"),
        ("MAKEPAD_XR_REMOTE_SELFTEST_PITCH", "-0.7"),
        ("MAKEPAD_XR_STREAM", "on"),
        ("MAKEPAD_LAN", "1"),
        ("FAB_XR_PASS_STATS", "1"),
        ("MAKEPAD_XR_REMOTE_SELFTEST_DECODE", "1"),
        ("MAKEPAD_XR_REMOTE_DUMP_AT", "asked"),
    ] {
        config.env.insert(k.into(), v.into());
    }
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    // The stream's "Ground plate" switch on (off by default, the real table being the ground):
    // the band's half cover is the plate's. Kept in a test home of the test's own, so the box's
    // own stream settings never reach it.
    let home = dir.join("home");
    let data = home.join("Library/Application Support/Makepad/fab");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("xr-stream"), "ground=on\n").unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), home.display().to_string());
    config.env.insert("MAKEPAD_XR_REMOTE_DUMP".into(), dir.display().to_string());
    let request = dir.join("dump-request");
    let r = run_with_config(config, |app| coloured(app, &request));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = r {
        panic!("{}", e.message());
    }
}

#[test]
fn the_stream_keeps_a_painted_parts_colour_and_the_coverage_band_through_the_encode_and_decode() {
    run("the_stream_keeps_a_painted_parts_colour_and_the_coverage_band_through_the_encode_and_decode");
}

/// The app main thread's CPU budget per wait.
const HANG: std::time::Duration = std::time::Duration::from_secs(300);

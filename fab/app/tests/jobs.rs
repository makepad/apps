//! The caption's job strip, and requests that cannot be done saying why, in one session
//! driven through the semantic tree and the app's tools:
//!
//! - a build that fails says why, in words, on the strip and in the list (no Retry: the same
//!   script fails the same way), and × clears it;
//! - a quick rebuild never shows; a long one (the motor mount's script) shows only after it
//!   has run 0.4 s, says what it is doing, and its Cancel in the list stops it with the document
//!   left as it was (a plate loading shows on the view's card instead: `plate_loading.rs`);
//! - `fab.image_to_mesh` refuses what is not a picture and a missing file with the reason, and
//!   with the local network off (`MAKEPAD_LAN=0`) the person is told no AI Hub has the model.
//!
//! HAL offscreen, native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, KeyCode, Role, Selector, TestApp, TestConfig};
use std::time::{Duration, Instant};

fn call_on(endpoint: &str, tool: &str, args: &str) -> Result<String, String> {
    let client = makepad_remote::Client::connect(endpoint).map_err(|e| format!("connect: {e:?}"))?;
    let argv = vec!["call".to_string(), format!("fab.{tool}"), args.to_string()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("fab.{tool}: {e:?}"))?;
    lines.iter().find(|l| l.contains("\"Result\"")).cloned().ok_or_else(|| format!("fab.{tool} returned no result: {lines:?}"))
}

/// The result's words: its text, error and message.
fn text(line: &str) -> String {
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("result line is JSON");
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    [r.get("text"), r.get("error"), r.get("message")].into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")
}

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// The document's head, once the app's tools answer.
fn ready(app: &TestApp) -> String {
    wait_poll(app, || {
        call_on(&app.remote_endpoint(), "inspect", "{}").map(|line| head_of(&text(&line))).map_err(|e| format!("the fab tools never appeared: {e}"))
    })
}

/// The visible names of the window's nodes.
fn names(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible()).map(|w| w.node.name).collect()
}

/// The strip's line, when it shows one with these words.
fn strip_line(app: &TestApp, words: &str) -> Option<String> {
    names(app).into_iter().find(|n| n.contains(words))
}

fn script_batch(id: &str, head: &str, file: &str, source: &str) -> String {
    format!(r#"{{"request_id":"{id}","expected":{head:?},"script":"{file}","source":{}}}"#, Value::Str(source.to_string()).to_json())
}

fn jobs(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let head = ready(&app);
    let endpoint = app.remote_endpoint();

    // ---- a build that fails says why, on the strip and in the list ----
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nobject(m.no_such_shape({size: 1mm}))\n";
    let reply = text(&call_on(&endpoint, "script", &script_batch("jobs-fail", &head, "broken.cad.splash", source)).expect("the call answers"));
    assert!(reply.contains("error"), "the broken script built: {reply}");
    let line = wait_poll(&app, || {
        if let Some(line) = strip_line(&app, "Rebuilding the plate failed:") {
            return Ok(line);
        }
        Err(format!("no failure on the strip: {:?}", names(&app)))
    });
    // The strip's slot is short: its reason leads with the why, not the wrapper every build
    // failure shares.
    let why = line.split_once(" failed: ").map(|(_, w)| w.trim_end_matches('…')).unwrap_or_default().to_string();
    assert!(!why.is_empty() && !why.starts_with("the project"), "the strip's reason is not the why: {line}");
    app.locator(Selector::role(Role::Button).text_contains("Rebuilding the plate failed")).wait_visible().press();
    // The list has the whole reason, the strip its start; no retry: the same script fails the same way.
    app.locator(Selector::role(Role::List).name("Jobs")).wait_visible();
    let whole = names(&app).into_iter().find(|n| n.contains("no_such_shape")).unwrap_or_else(|| panic!("the list does not name the failure: {:?}", names(&app)));
    assert!(whole.starts_with(&why), "the strip's reason {why:?} is not the start of the list's {whole:?}");
    assert_eq!(app.locator(Selector::role(Role::Button).name("Retry")).count(), 0, "a failed build offers a Retry");
    // Escape closes the list; × on a failure clears it from the strip.
    app.press_key(KeyCode::Escape);
    app.locator(Selector::role(Role::List).name("Jobs")).wait_hidden();
    app.locator(Selector::role(Role::Button).name("Cancel job")).press();
    wait_poll(&app, || {
        if strip_line(&app, "Rebuilding the plate failed:").is_some() { Err("the failure stayed on the strip after ×".to_string()) } else { Ok(()) }
    });

    // ---- a quick rebuild never shows ----
    let head = ready(&app);
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nobject(m.box({size: [10mm 10mm 10mm]}))\n";
    let batch = script_batch("jobs-quick", &head, "quick.cad.splash", source);
    let quick_endpoint = endpoint.clone();
    let quick = std::thread::spawn(move || {
        let t = Instant::now();
        let reply = call_on(&quick_endpoint, "script", &batch);
        (t.elapsed(), reply)
    });
    // what the strip shows while the quick rebuild runs, a look every 20 ms (not a wait: the
    // call ends it)
    let mut flashed = None;
    while !quick.is_finished() {
        if let Some(line) = strip_line(&app, "Rebuilding the plate") {
            flashed = Some(line);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (took, reply) = quick.join().unwrap();
    reply.expect("the quick script answers");
    if took < Duration::from_millis(280) {
        assert!(flashed.is_none(), "a {:.0} ms rebuild flashed on the strip: {flashed:?}", took.as_secs_f64() * 1000.0);
    }

    // ---- a long rebuild, the motor mount's script: shown after 0.4 s, with its stage ----
    // (with no rebuild on the strip first, the quick one's or a refinement after it: the next
    // line is the long one's)
    wait_poll(&app, || {
        if strip_line(&app, "Rebuilding the plate").is_some() { Err(format!("a rebuild stayed on the strip: {:?}", names(&app))) } else { Ok(()) }
    });
    let head = ready(&app);
    let batch = script_batch("jobs-long", &head, "motor_mount.cad.splash", include_str!("../resources/motor_mount.cad.splash"));
    let long_endpoint = endpoint.clone();
    let started = Instant::now();
    let long = std::thread::spawn(move || call_on(&long_endpoint, "script", &batch));
    // when the strip shows it, a look every 10 ms (what the 0.4 s check below measures; the call
    // ends the looks)
    let mut shown_after = None;
    while shown_after.is_none() && !long.is_finished() {
        if strip_line(&app, "Rebuilding the plate").is_some() {
            shown_after = Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let shown_after = shown_after.unwrap_or_else(|| panic!("the motor mount's rebuild never showed while it ran: {:?}", names(&app)));
    assert!(shown_after >= Duration::from_millis(300), "the rebuild showed before its 0.4 s (measured from the call): {shown_after:?}");
    // it says what it is doing: the part it builds
    wait_poll(&app, || {
        if strip_line(&app, "Rebuilding the plate · building mount").is_none() { Err(format!("the rebuild never said it builds the mount: {:?}", names(&app))) } else { Ok(()) }
    });

    // ---- the list: open it, cancel the rebuild from its row while it runs ----
    // The line's words reach the tree a frame before its button does.
    app.locator(Selector::role(Role::Button).text_contains("Rebuilding the plate")).wait_visible().press();
    app.locator(Selector::role(Role::List).name("Jobs")).wait_visible();
    // The running jobs list newest first; the motor mount's rebuild is the oldest, so its row is
    // the last Cancel (a refinement of the exact parts may run beside it).
    let cancels = wait_poll(&app, || {
        let n = app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::Button && w.node.name == "Cancel").count();
        if n > 0 {
            return Ok(n);
        }
        Err("no Cancel button in the jobs list".to_string())
    });
    app.locator(Selector::role(Role::Button).name("Cancel").nth(cancels - 1)).wait_visible().press();
    wait_poll(&app, || {
        if !names(&app).iter().any(|n| n.contains("cancelled, stopped in")) { Err(format!("the rebuild did not stop: {:?}", names(&app))) } else { Ok(()) }
    });
    // its call answers (with the cancel)
    let _ = long.join().unwrap();
    // The document stayed what it was: the quick part, not the motor mount.
    let inspect = text(&call_on(&endpoint, "inspect", "{}").expect("inspect"));
    assert!(!inspect.contains("motor_mount"), "a cancelled build replaced the document: {inspect}");

    // ---- fab.image_to_mesh: what it cannot read is refused with the reason ----
    let fab = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let image = |path: &std::path::Path| text(&call_on(&endpoint, "image_to_mesh", &format!("{{\"image\":{:?}}}", path.display().to_string())).expect("image_to_mesh answers"));
    let r = image(&fab.join("Cargo.toml"));
    assert!(r.contains("not a picture Fab reads"), "not a picture: {r}");
    let r = image(&fab.join("no-such-picture.png"));
    assert!(r.contains("no picture file"), "no such file: {r}");
    // A picture it reads, with the local network off: the person is told, in words.
    let r = image(&fab.join("resources/icon_1024.png"));
    assert!(r.contains("local network") || r.contains("AI Hub"), "the network is off: {r}");
}

#[test]
fn the_job_strip_shows_late_cancels_and_says_why_and_image_to_mesh_says_why() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "the_job_strip_shows_late_cancels_and_says_why_and_image_to_mesh_says_why").unwrap();
    // The local network off whatever the machine says: the image-to-3D hub is not this test's.
    config.env.insert("MAKEPAD_LAN".into(), "0".into());
    if let Err(e) = run_with_config(config, jobs) {
        panic!("{}", e.message());
    }
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

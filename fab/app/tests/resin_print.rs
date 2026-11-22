//! Print and Monitor on a resin printer (the simulated one, `fab_sla::printer`), against the fake
//! printer the test starts on the loopback address (`fab_sla::link::fake`): nothing reaches a real
//! printer, and the printer's file is in the test's own home.
//!
//! - With no resin printer linked, Print's confirm card says how to link one and offers no Start
//!   print.
//! - The printer's file links the fake: the card asks once the layers are cut and checked, and
//!   until Start print the printer hears nothing.
//! - Start print sends every layer and the manifest and starts the job; Monitor's job card follows
//!   the printer: the layers cured and the time left. Pause, Resume and Stop go to the printer,
//!   and the card says each.
//! - Monitor's view follows the printer: the stack cut at the layer it cured last. Moving the
//!   layers by hand keeps the view there as the printer goes on, until Back to live.
//! - A second print runs to its last layer and finishes; Close takes its card away.
//! - Fab restarted while its job printed: the job its record says hasn't ended is found again.
//!   The card says it reconnects while the printer is asked how far the job is, then follows it;
//!   the printer is asked nothing but how far it is until Pause is pressed. A printer with no
//!   such job: the card says it ended while Fab was closed and keeps what it showed last.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/simulated_profiles.rs"]
mod simulated_profiles;

#[path = "common/workspace_row.rs"]
mod workspace_row;
use fab::print::resin::{read_record, save_record, ResinJob, ResinRecord, ResinStage};
use fab_sla::estimate::{print_time, LayerTimes};
use fab_sla::link::fake::FakePrinter;
use fab_sla::link::State;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use std::time::{Duration, Instant};

fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let argv = vec!["call".to_string(), format!("fab.{tool}"), args.to_string()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("fab.{tool}: {e:?}"))?;
    match lines.iter().find(|l| l.contains("\"Result\"")) {
        Some(l) if !l.contains("\"err\":") => Ok(l.clone()),
        _ => Err(format!("fab.{tool}: {lines:?}")),
    }
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
    let value = makepad_script::syntax::parse_value(reply).unwrap_or_else(|e| panic!("not a Splash reply ({e:?}): {reply}"));
    value.field("head").and_then(|f| f.value.string()).unwrap_or_else(|| panic!("the reply has no head: {reply}")).to_string()
}

fn names(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible()).map(|w| w.node.name).collect()
}

/// What the app said of the resin printer, for a failure's message.
fn said(app: &TestApp) -> Vec<String> {
    app.log_since(0).map(|l| l.lines.into_iter().filter(|l| l.contains("fab resin")).collect()).unwrap_or_default()
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

/// The first shown name `ok` takes.
fn shown(app: &TestApp, what: &str, ok: impl Fn(&str) -> bool) -> Result<String, String> {
    let names = names(app);
    names.iter().find(|n| ok(n)).cloned().ok_or_else(|| format!("{what}: on screen {names:?}; the app said {:#?}", said(app)))
}

/// Waits for a shown name `ok` takes.
fn wait_name(app: &TestApp, what: &str, ok: impl Fn(&str) -> bool) -> String {
    wait_for(app, || shown(app, what, &ok))
}

fn wait_gone(app: &TestApp, what: &str, ok: impl Fn(&str) -> bool) {
    wait_for(app, || {
        let names = names(app);
        match names.iter().any(|n| ok(n)) {
            true => Err(format!("{what}: still on screen {names:?}")),
            false => Ok(()),
        }
    })
}

fn press(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Button).name(name)).wait_visible().press();
}

/// Waits for the printer to have heard `request` `times` times from the app.
fn wait_heard(app: &TestApp, printer: &FakePrinter, request: &str, times: usize) {
    wait_for(app, || {
        let requests = printer.requests();
        match requests.iter().filter(|r| *r == request).count() < times {
            true => Err(format!("the printer never heard {request} {times} times: {requests:?}")),
            false => Ok(()),
        }
    })
}

/// What the printer was asked besides how far its job is (and goodbye).
fn commands(printer: &FakePrinter, from: usize) -> Vec<String> {
    printer.requests().split_off(from).into_iter().filter(|r| !matches!(r.as_str(), "HELLO 1" | "STATUS" | "BYE")).collect()
}

/// The resin layer panel's layer and layers: "Layer 10 of 160  ·  …".
fn panel(app: &TestApp) -> Option<(u32, u32)> {
    panel_of(&names(app))
}

/// The resin layer panel's layer and layers among the shown names.
fn panel_of(names: &[String]) -> Option<(u32, u32)> {
    names.iter().find_map(|t| {
        let (layer, rest) = t.strip_prefix("Layer ")?.split_once(" of ")?;
        let (count, _) = rest.split_once("  ·  ")?;
        Some((layer.parse().ok()?, count.parse().ok()?))
    })
}

fn wait_panel(app: &TestApp, what: &str, ok: impl Fn((u32, u32)) -> bool) -> (u32, u32) {
    wait_for(app, || {
        let names = names(app);
        let p = panel_of(&names);
        p.filter(|p| ok(*p)).ok_or_else(|| format!("{what}: the panel shows {p:?}, on screen {names:?}"))
    })
}

/// The layers the confirm card asks to print: "Print the 160 layers on vat? …".
fn asked_layers(question: &str) -> u32 {
    question.strip_prefix("Print the ").and_then(|q| q.split(' ').next()).and_then(|n| n.parse().ok()).unwrap_or_else(|| panic!("{question}"))
}

#[test]
fn print_and_monitor_drive_the_resin_printer_over_its_link() {
    let dir = std::env::temp_dir().join(format!("fab-resin-print-{}", std::process::id()));
    simulated_profiles::install(&dir);
    simulated_profiles::select(&dir, fab_sla::printer::SIMULATED_PRINTER.0, fab_sla::printer::SIMULATED_PROCESS.0);
    // the simulated printer's screen, unturned: 2400 x 1600 images
    let printer = FakePrinter::start("Simulated resin", [2400, 1600]);
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "print_and_monitor_drive_the_resin_printer_over_its_link").unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    let result = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        let head = head_of(&text(&wait_for_within(&app, Duration::from_secs(30), || call(&app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}")))));
        // a 6 x 6 mm block 2 mm tall, on its supports
        let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet block = m.box({size: [6mm 2mm 6mm] at: [0mm 1mm 0mm]})\nobject(block)\n";
        let batch = format!(r#"{{"request_id":"print-block","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(source.into()).to_json());
        let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
        let batch = format!(r#"{{"request_id":"print-place","expected":{head:?},"edits":"place(\"block\", name: \"Block\")"}}"#);
        call(&app, "place", &batch).expect("place");
        let print = Selector::id("print");

        // no resin printer linked: the card says how to link one, and nothing offers to print
        app.locator(print.clone()).click();
        wait_for_within(&app, Duration::from_secs(30), || shown(&app, "the card says no printer is linked", |n| n.starts_with("No resin printer is linked.")));
        assert!(!names(&app).iter().any(|n| n == "Start print"), "no Start print without a printer: {:?}", names(&app));
        press(&app, "Cancel");

        // the person's file links the fake printer: read again on the next press of Print
        let printers = dir.join(".makepad").join("fab").join("printers");
        std::fs::create_dir_all(&printers).unwrap();
        std::fs::write(printers.join("vat.resin"), format!("host = 127.0.0.1\nport = {}\n", printer.addr().port())).unwrap();
        app.locator(print.clone()).click();
        let question = wait_for_within(&app, Duration::from_secs(120), || shown(&app, "the card asks to print", |n| n.starts_with("Print the ") && n.contains(" layers on vat?")));
        let layers = asked_layers(&question);
        assert!(printer.requests().is_empty(), "nothing goes to the printer before Start print: {:?}", printer.requests());

        // Start print: every layer and the manifest go, the job starts, Monitor follows it
        press(&app, "Start print");
        wait_for_within(&app, Duration::from_secs(60), || shown(&app, "printing", |n| n == "Printing on Simulated resin"));
        assert_eq!(printer.files().len(), layers as usize + 1, "every layer and the manifest");
        assert_eq!(printer.requests().iter().filter(|r| *r == "PRINT").count(), 1);
        workspace_row::wait_shown(&app, "Monitor");
        printer.step(10);
        let detail = wait_name(&app, "10 layers cured", |n| n.contains(&format!("  ·  Layer 10 / {layers}  ·  ")));
        assert!(detail.ends_with(&format!("  ·  {:.0} %", 1000.0 / layers as f64)), "{detail}");
        // the time left: the profile's time for the layers still to cure
        let p = fab_sla::printer::simulated();
        let left = format!("  ·  Left {}  ·  ", fab::print::format_time(print_time(&p, layers) - print_time(&p, 10)));
        let times = wait_name(&app, "the time left", |n| n.starts_with("Elapsed ") && n.contains("  ·  Left "));
        assert!(times.contains(&left) && times.contains("  ·  Planned "), "{times}: {left}");

        // the view follows the printer: the stack cut at the layer it cured last
        wait_panel(&app, "the view on the printer's layer", |p| p == (10, layers));
        assert!(!names(&app).iter().any(|n| n == "Back to live"), "it follows: {:?}", names(&app));
        // the layers moved by hand: the view stays there as the printer goes on, until Back to live
        app.locator(Selector::role(Role::Slider).name("Top layer")).set_value("0.5");
        let moved = wait_panel(&app, "the layer moved by hand", |p| p.0 != 10);
        printer.step(3);
        wait_name(&app, "13 layers cured", |n| n.contains(&format!("  ·  Layer 13 / {layers}  ·  ")));
        assert_eq!(panel(&app), Some(moved), "the view stays where it was moved to");
        press(&app, "Back to live");
        wait_panel(&app, "back on the printer's layer", |p| p == (13, layers));

        // pause: the printer holds, curing nothing, until Resume
        press(&app, "Pause");
        wait_heard(&app, &printer, "PAUSE", 1);
        wait_name(&app, "paused", |n| n == "Paused");
        printer.step(5);
        press(&app, "Resume");
        wait_heard(&app, &printer, "RESUME", 1);
        wait_name(&app, "printing again", |n| n == "Printing on Simulated resin");
        assert!(names(&app).iter().any(|n| n.contains(&format!("  ·  Layer 13 / {layers}  ·  "))), "a held job cured nothing: {:?}", names(&app));
        // stop asks first
        press(&app, "Stop…");
        press(&app, "Stop print");
        wait_heard(&app, &printer, "STOP", 1);
        wait_name(&app, "stopped", |n| n == "Stopped.");
        press(&app, "Close");
        wait_gone(&app, "the stopped job's card", |n| n == "Stopped.");

        // a second print runs to its last layer
        app.locator(print.clone()).click();
        wait_for_within(&app, Duration::from_secs(30), || shown(&app, "the card asks again", |n| n.starts_with("Print the ") && n.contains(" layers on vat?")));
        press(&app, "Start print");
        wait_for_within(&app, Duration::from_secs(60), || shown(&app, "printing", |n| n == "Printing on Simulated resin"));
        printer.step(layers);
        wait_name(&app, "finished", |n| n == "Finished");
        assert!(names(&app).iter().any(|n| n.ends_with(&format!("  ·  Layer {layers} / {layers}  ·  100 %"))), "{:?}", names(&app));
        assert_eq!(printer.requests().iter().filter(|r| *r == "PRINT").count(), 2, "two prints, each on its Start print");
        press(&app, "Close");
        wait_gone(&app, "the finished job's card", |n| n == "Finished");
    });
    drop(printer);
    let _ = std::fs::remove_dir_all(&dir);
    result.unwrap();
}

/// A job of 40 layers Fab sent to `vat` before it restarted, `layer` of them cured when it was
/// seen last, in the test home's jobs folder.
fn left_behind(jobs: &std::path::Path, layer: u32) {
    let times = LayerTimes::of(&fab_sla::printer::simulated());
    let wall = fab::print::jobs::wall_now();
    let job = ResinJob { name: "Block".into(), printer: "Simulated resin".into(), stage: ResinStage::Printing, layer, layers: 40, left: None, planned: Some(times.until(40)), started: Some(wall - 600.0), ended: None, note: String::new(), stack: None };
    let record = ResinRecord { link: "vat".into(), job, bottom_layers: times.bottom_layers, bottom: times.bottom, layer: times.layer, seen: wall - 30.0 };
    save_record(jobs, &record.text(wall, wall)).unwrap();
}

fn started(dir: &std::path::Path, name: &str) -> TestConfig {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), name).unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    config
}

/// Fab restarted while its job printed: Monitor finds it again from its record, reconnecting
/// while the printer's answer waits, and follows it from the printer's layer. Nothing but how far
/// the job is goes to the printer until Pause, Resume or Stop is pressed, and the record keeps
/// how it ended. A printer with no such job: the job ended while Fab was closed.
#[test]
fn monitor_finds_its_job_again_after_a_restart() {
    let dir = std::env::temp_dir().join(format!("fab-resin-again-{}", std::process::id()));
    simulated_profiles::install(&dir);
    simulated_profiles::select(&dir, fab_sla::printer::SIMULATED_PRINTER.0, fab_sla::printer::SIMULATED_PROCESS.0);
    let printer = FakePrinter::start("Simulated resin", [2400, 1600]);
    let printers = dir.join(".makepad").join("fab").join("printers");
    std::fs::create_dir_all(&printers).unwrap();
    std::fs::write(printers.join("vat.resin"), format!("host = 127.0.0.1\nport = {}\n", printer.addr().port())).unwrap();
    let jobs = dir.join(".makepad").join("fab").join("jobs");
    let kept = || read_record(&jobs, 0.0, 0.0).expect("the job's record").job.stage;

    // the job printed on while Fab was closed: seen last at layer 8 of its 40, at 10 now. The
    // printer's answers to how far it is wait until the card says Fab reconnects
    left_behind(&jobs, 8);
    printer.set_job(State::Printing, 10, 40);
    printer.hold(true);
    let result = run_with_config(started(&dir, "monitor_finds_its_job_again_after_a_restart"), |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        workspace_row::press(&app, "Monitor");
        within(Duration::from_secs(8), || shown(&app, "reconnecting", |n| n == "Reconnecting to Simulated resin…"));
        assert!(names(&app).iter().any(|n| n == "Block  ·  Layer 8 / 40  ·  20 %"), "what it showed last: {:?}", names(&app));
        assert!(!names(&app).iter().any(|n| ["Pause", "Stop…", "Cancel", "Close"].contains(&n.as_str())), "nothing to press while it reconnects: {:?}", names(&app));
        assert_eq!(commands(&printer, 0), Vec::<String>::new(), "only asked how far it is");
        printer.hold(false);
        wait_name(&app, "printing", |n| n == "Printing on Simulated resin");
        wait_name(&app, "the printer's layer", |n| n == "Block  ·  Layer 10 / 40  ·  25 %");
        printer.step(6);
        wait_name(&app, "6 more layers", |n| n == "Block  ·  Layer 16 / 40  ·  40 %");
        let times = LayerTimes::of(&fab_sla::printer::simulated());
        let left = format!("  ·  Left {}  ·  ", fab::print::format_time(times.until(40) - times.until(16)));
        let shown = wait_name(&app, "the time left", |n| n.starts_with("Elapsed ") && n.contains("  ·  Left "));
        assert!(shown.contains(&left), "{shown}: {left}");
        wait_for(&app, || {
            let requests = printer.requests();
            match requests.iter().filter(|r| *r == "STATUS").count() < 4 {
                true => Err(format!("asked how far it is again and again: {requests:?}")),
                false => Ok(()),
            }
        });
        assert_eq!(commands(&printer, 0), Vec::<String>::new(), "no command before a press");
        assert_eq!(kept(), ResinStage::Printing);

        // Pause, Resume and Stop go once pressed
        press(&app, "Pause");
        wait_heard(&app, &printer, "PAUSE", 1);
        wait_name(&app, "paused", |n| n == "Paused");
        press(&app, "Resume");
        wait_heard(&app, &printer, "RESUME", 1);
        wait_name(&app, "printing again", |n| n == "Printing on Simulated resin");
        press(&app, "Stop…");
        press(&app, "Stop print");
        wait_heard(&app, &printer, "STOP", 1);
        wait_name(&app, "stopped", |n| n == "Stopped.");
        assert_eq!(commands(&printer, 0), ["PAUSE", "RESUME", "STOP"], "each on its press");
        // the record says it ended: the next start leaves it be
        wait_for(&app, || match kept() {
            ResinStage::Stopped => Ok(()),
            other => Err(format!("the record kept {other:?}")),
        });
        press(&app, "Close");
        wait_gone(&app, "the stopped job's card", |n| n == "Stopped.");
    });
    result.unwrap();

    // a printer with no such job: the job ended while Fab was closed, as it was seen last
    left_behind(&jobs, 12);
    printer.set_job(State::Idle, 0, 0);
    let from = printer.requests().len();
    let result = run_with_config(started(&dir, "monitor_finds_its_job_again_after_a_restart"), |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        workspace_row::press(&app, "Monitor");
        wait_for_within(&app, Duration::from_secs(30), || shown(&app, "ended unseen", |n| n == "It ended while Fab was closed: how isn't known."));
        assert!(names(&app).iter().any(|n| n == "Block  ·  Layer 12 / 40  ·  30 %"), "what it showed last: {:?}", names(&app));
        assert_eq!(commands(&printer, from), Vec::<String>::new(), "only asked how far it is");
        wait_for(&app, || match kept() {
            ResinStage::EndedUnseen => Ok(()),
            other => Err(format!("the record kept {other:?}")),
        });
        press(&app, "Close");
        wait_gone(&app, "the ended job's card", |n| n == "It ended while Fab was closed: how isn't known.");
    });
    drop(printer);
    let _ = std::fs::remove_dir_all(&dir);
    result.unwrap();
}

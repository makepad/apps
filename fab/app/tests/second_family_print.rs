//! Print commits a second-family job, sends it to an in-memory peer, and Monitor follows it.
mod common;
#[path = "common/second_family_profiles.rs"]
mod second_family_profiles;

#[path = "common/workspace_row.rs"]
mod workspace_row;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use std::io::Write;
use std::path::Path;
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
    let value = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    value.field("head").and_then(|f| f.value.string()).unwrap_or_else(|| panic!("reply has no head: {reply}")).to_string()
}

fn names(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible()).map(|w| w.node.name).collect()
}

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

/// [`wait_for`] for what takes the app long (a slice, the first start): `budget` of its main
/// thread's CPU ([`TestApp::wait_until_within`]).
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

fn wait_layer(app: &TestApp, want: Option<u32>) -> (u32, u32) {
    wait_for(app, || {
        let l = layer_label(app);
        l.filter(|l| want.is_none_or(|w| w == l.0)).ok_or_else(|| format!("the view is not at layer {want:?}: {l:?}; {:?}", names(app)))
    })
}

fn wait_name(app: &TestApp, prefix: &str) -> String {
    wait_for(app, || {
        let names = names(app);
        names.iter().find(|t| t.starts_with(prefix)).cloned().ok_or_else(|| format!("no node named {prefix:?}: {names:?}"))
    })
}

fn report(sim: &Path, json: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(sim).expect("the report file");
    writeln!(f, "{json}").unwrap();
}

fn tools_up(app: &TestApp) -> String {
    head_of(&text(&wait_for_within(app, Duration::from_secs(30), || call(app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}")))))
}

fn small_plate(app: &TestApp) {
    let head = tools_up(app);
    let source = String::from("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet base = m.box({size: [4mm 1.2mm 4mm] at: [0mm 0.6mm 0mm]})\nlet block = base\nobject(block)\n");
    let batch = format!(r#"{{"request_id":"small-plate","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(source).to_json());
    let head = head_of(&text(&call(app, "script", &batch).expect("script")));
    let batch = format!(r#"{{"request_id":"small-place","expected":{head:?},"edits":"place(\"block\", name: \"Block\")"}}"#);
    call(app, "place", &batch).expect("place");
    app.locator(Selector::id("print")).click();
    wait_for_within(app, Duration::from_secs(120), || match common::slice_state(app) == "Sliced" {
        true => Ok(()),
        false => Err(format!("the small plate never sliced: {:?}", names(app))),
    });
}

fn committed_job(app: &TestApp, jobs_dir: &Path) -> fab::print::jobs::JobMeta {
    wait_for(app, || {
        // Do not call read_all while Print is writing: it removes unfinished job directories.
        let finished: Vec<_> = std::fs::read_dir(jobs_dir).into_iter().flatten().flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.')).collect();
        if finished.len() == 1 {
            for entry in &finished {
                assert!(entry.path().join("job.txt").is_file(), "Print must commit job.txt");
            }
            let jobs = fab::print::jobs::read_all(jobs_dir);
            assert_eq!(jobs.len(), 1, "every committed job must be readable");
            let (meta, _) = &jobs[0];
            assert_eq!(meta.printer_model, second_family_profiles::model());
            assert_eq!(meta.machine_preset, second_family_profiles::MACHINE);
            assert_eq!(meta.filament_presets, [second_family_profiles::FILAMENT]);
            assert!(meta.layers >= 3, "the committed slice has layers to follow");
            let package = fab::print::jobs::read_package(jobs_dir, &meta.id).expect("committed package");
            assert_eq!(package.first(), Some(&b';'), "plain G-code begins with a comment");
            assert!(!package.starts_with(b"PK"), "the package is plain G-code");
            assert!(meta.accepted.is_empty(), "a sound job accepts no problems");
            return Ok(meta.clone());
        }
        Err(format!("Print never committed job: {:?}", names(app)))
    })
}

fn wait_started(app: &TestApp, sim: &Path) -> String {
    wait_for(app, || {
        if let Ok(started) = std::fs::read_to_string(fab_printer_link::sim::started_path(sim)) {
            if started.ends_with('\n') { return Ok(started); }
        }
        Err(format!("the simulator never received the job: {:?}", names(app)))
    })
}

#[test]
fn second_family_print_commits_starts_and_monitor_finishes() {
    use fab::print::jobs::{read_all, JobState};
    let dir = std::env::temp_dir().join(format!("fab-second-family-print-{}", std::process::id()));
    second_family_profiles::install(&dir);
    let sim = dir.join("reports.jsonl");
    std::fs::write(&sim, "# the report file drives progress\n").unwrap();
    let jobs_dir = dir.join(".makepad/fab/jobs");
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "second_family_print_commits_starts_and_monitor_finishes").unwrap();
    config.env.insert("FAB_PRINTER_SIM".into(), sim.display().to_string());
    config.env.insert("FAB_PRINTER_SIM_MODEL".into(), second_family_profiles::model());
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    let result = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        small_plate(&app);
        wait_name(&app, "Start print");
        for head in 1..=4 { wait_name(&app, &format!("Tool {head}")); }
        assert!(!names(&app).iter().any(|n| n == "Left" || n == "Right"), "the panel numbers all four heads");
        app.locator(Selector::role(Role::Button).name("Start print")).press();
        let meta = committed_job(&app, &jobs_dir);
        let started = wait_started(&app, &sim);
        assert_eq!(started.lines().count(), 1, "one accepted start");
        let line = started.lines().next().unwrap();
        let filename = line.strip_prefix("SDCARD_PRINT_FILE_WITH_PARAMETERS FILENAME=\"")
            .and_then(|s| s.split_once('"')).map(|(name, _)| name).expect("the start names its uploaded file");
        assert_eq!(filename, fab_printer_link::job::plain_file_name(&meta.name));
        assert!(filename.ends_with(".gcode"), "the uploaded file is G-code");
        assert!(line.contains(r#"MAP_TABLE="[[0, 0]]""#), "the first filament uses the first head");
        assert!(line.contains(r#"FILAMENT_TYPE="['PLA']""#), "the start carries the job's material");
        report(&sim, &format!(r#"{{"print_stats":{{"state":"printing","info":{{"current_layer":2,"total_layer":{}}}}},"virtual_sdcard":{{"progress":0.25}}}}"#, meta.layers));
        workspace_row::press(&app, "Monitor");
        assert_eq!(wait_layer(&app, Some(2)), (2, meta.layers));
        wait_name(&app, &format!("{}  ·  Printing", meta.name));
        // The line of the head names the printing head and every head's temperature.
        let line = wait_for(&app, || {
            let names = names(&app);
            names.iter().find(|t| t.contains("  ·  filament 1  ·  ")).cloned().ok_or_else(|| format!("no line of the head: {names:?}"))
        });
        assert!(line.contains(" on Tool 1  ·  ") && !line.contains("nozzle"), "{line}");
        // The start heats the first head only (the fixture's start G-code).
        assert!(line.contains("Tool 1 200 °C  ·  Tool 2 0 °C  ·  Tool 3 0 °C  ·  Tool 4 0 °C"), "{line}");
        report(&sim, r#"{"print_stats":{"state":"complete"},"virtual_sdcard":{"progress":1.0}}"#);
        wait_name(&app, &format!("{}: Finished", meta.name));
        wait_for(&app, || match read_all(&jobs_dir)[0].1.state == JobState::Finished {
            true => Ok(()),
            false => Err("the finished state never reached disk".to_string()),
        });
        assert_eq!(std::fs::read_to_string(fab_printer_link::sim::started_path(&sim)).unwrap().lines().count(), 1);
    });
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = result { panic!("{}", e.message()); }
}

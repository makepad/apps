//! A plate an earlier Fab kept, in that Fab's format, as the person's Fab meets it at launch: the
//! first start folds it in, and the plate list reads why it does not read. Neither read is a run:
//! what they meet is the plate's state (its row says this Fab cannot read its file, its tooltip
//! says why), never a script error, neither an error event nor an error site.
//!
//! Both reads evaluate the file `unreported`, and the VM files what it meets where its ops run,
//! in the loader under `makepad run`: the flag it reads there is the one the read set.

use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use std::time::{SystemTime, UNIX_EPOCH};

/// A plate in an earlier format: its `place` took `filament:` and `nozzle:`.
const OLD_PLATE: &str = "use mod.fab.*\n\nscript(\"vase.cad.splash\")\n\nplace(\"vase\", object: \"vase\", filament: 3, color: #x3f89da, nozzle: @right)\n";
const OLD_SCRIPT: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet vase = m.box({size: [10mm 10mm 10mm]})\nobject(vase)\n";

/// The answer lines of a command to the app's remote (the JIT's inspect's too).
fn ask(app: &TestApp, argv: &[&str]) -> Vec<String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect");
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .unwrap_or_else(|e| panic!("{argv:?}: {e:?}"));
    lines
}

#[test]
fn an_old_plate_says_on_its_row_that_it_does_not_read_and_files_no_script_error() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    // Outside artifacts_dir: TestApp clears that directory before launching.
    let home = std::env::temp_dir().join(format!("fab-unread-plate-{}-{nonce}", std::process::id()));
    let kept = home.join(".makepad/fab/previous/1793600982-vase");
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::write(kept.join("vase.fab.splash"), OLD_PLATE).unwrap();
    std::fs::write(kept.join("vase.cad.splash"), OLD_SCRIPT).unwrap();
    std::fs::create_dir_all(home.join("Documents/Fab")).unwrap();

    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "unread_plate").unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), home.display().to_string());
    config.env.insert("FAB_PLATES".into(), "1".into());
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    let result = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        app.locator(Selector::role(Role::Tab).name("Plates")).click();
        // The row is the list's, sent once the fold-in and the list's read of the file are done.
        let row = app.locator(Selector::role(Role::ListItem).name("vase")).wait_visible().snapshot();
        assert!(row.node.description.contains("this Fab cannot read its file"), "the old plate's row: {:?}", row.node.description);
        let met = |l: &String| l.contains("unknown named arg");
        let events: Vec<String> = ask(&app, &["events", "since=0", "on=scripterr"]).into_iter().filter(|l| l.contains("scripterr")).collect();
        let sites: Vec<String> = ask(&app, &["script", "errors"]).into_iter().filter(|l| l.contains("\"text\"")).collect();
        println!("[unread-plate] row {:?}; script error events {events:?}; error sites {sites:?}", row.node.description);
        assert!(!events.iter().any(met) && !sites.iter().any(met), "the old plate's reads filed script errors: events {events:?}, error sites {sites:?}");
    });
    let _ = std::fs::remove_dir_all(&home);
    result.unwrap();
}

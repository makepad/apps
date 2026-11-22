//! The recording bar runs the same handheld app with the chosen bench's starting page.

#[path = "common/workspace_row.rs"]
mod workspace_row;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};

#[test]
fn picking_the_second_bench_runs_the_voice_page() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "elec_bench_choice").unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), "1600x1000@1".into(), "--demo".into(), "handheld".into()];
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let prefs = std::env::temp_dir().join(format!("fab-bench-choice-{}-{nanos}.prefs", std::process::id()));
    std::fs::write(&prefs, "").unwrap();
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let result = run_with_config(config, |app: TestApp| {
        let chooser = || app.locator(Selector::role(Role::ComboBox).name("Bench"));
        let rows = |text| app.locator(Selector::id("first.rows").text_contains(text));
        // The demo opens in Electronics (the recording bar shows); the Firmware workspace shows
        // the variables read from the running S3.
        chooser().wait_visible().wait_value("sand");
        workspace_row::press(&app, "Firmware");
        workspace_row::wait_shown(&app, "Firmware");
        app.locator(Selector::id("head.doc_title").name("handheld.fw.splash")).wait_visible();
        rows("shown = 0\n").wait_visible();
        chooser().expand();
        app.locator(Selector::role(Role::MenuItem).name("voice")).wait_visible();
        chooser().select_option("voice");
        chooser().wait_value("voice");
        app.locator(Selector::id("head.status").text_contains("running")).wait_visible();
        rows("shown = 1\n").wait_visible();
        rows("meter_shown = true").wait_visible();
    });
    let saved = std::fs::read_to_string(&prefs).unwrap();
    std::fs::remove_file(prefs).unwrap();
    result.unwrap();
    assert!(saved.lines().any(|line| line.starts_with("elec_benches=") && line.contains("\"voice\"")),
        "the project's choice is kept in the view preferences: {saved}");
}

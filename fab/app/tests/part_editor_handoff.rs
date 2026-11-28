//! The picked part stays editable as the plate trades room with Code, with one editor at a time.
//! At the default window Code leaves the plate 581 points, too narrow for the Part sidebar, so
//! the editor is a Part tab beside Parts. With Code closed it is the plate's sidebar, and its X
//! moves the part. In Sliced the sidebar clears the slice pad. One held sash drag crosses 640
//! (to the sidebar) and 624 (back to the tab), and the band between keeps the editor where it
//! is. The saved layout never holds the transient Part tab. In Test prints the tests take the
//! plate's side, so the editor is the tab alone, with no X while the test pieces stand in for
//! the plate; back in Design the X is back once, in the sidebar.

#[path = "common/render_mode.rs"]
mod render_mode;

use makepad_strict_json::Value;
use makepad_test::{run_with_config, KeyCode, Rect, Role, Selector, TestApp, TestConfig};
use std::time::Duration;

fn tab(name: &str) -> Selector {
    Selector::role(Role::Tab).name(name)
}

fn x_field() -> Selector {
    Selector::role(Role::SpinButton).name("X")
}

fn viewport(app: &TestApp, width: f32) -> Rect {
    let view = app.locator(Selector::id("viewport")).wait_visible().snapshot().bounds;
    assert!((view.w - width).abs() <= 1.0, "expected a {width}-point plate, measured {view:?}");
    view
}

fn editor(app: &TestApp, sidebar: bool) -> Rect {
    let role = if sidebar {
        app.locator(Selector::id("part_toggle")).wait_visible().wait_checked(true);
        app.locator(tab("Part")).wait_hidden();
        Role::Group
    } else {
        app.locator(tab("Part").state("selected")).wait_visible();
        app.locator(Selector::id("part_toggle")).wait_hidden();
        Role::TabPanel
    };
    let panel = app.locator(Selector::role(role).name("Part")).wait_count(1).snapshot().bounds;
    let x = app.locator(x_field()).wait_count(1).snapshot().bounds;
    assert!(panel.contains(x.center().0, x.center().1), "X {x:?} belongs in the visible Part editor {panel:?}");
    panel
}

/// The placed box's actual X on the plate, in millimetres.
fn box_x(app: &TestApp) -> f64 {
    let lines = app.call_tool(Duration::from_secs(30), "fab.inspect", r#"{"program":false}"#);
    let line = lines.iter().find(|line| line.contains("\"Result\"")).expect("fab.inspect answered");
    let json = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("inspect result");
    let result = match json.get("Result").expect("Result") {
        Value::Arr(items) => &items[0],
        value => value,
    };
    let text = result.get("text").and_then(Value::as_str).expect("inspect text");
    let report = makepad_script::syntax::parse_value(text).expect("inspect is Splash");
    let parts = report.field("plate").and_then(|f| f.value.field("parts"))
        .and_then(|f| f.value.items()).expect("plate parts");
    let part = parts.iter().find(|p| p.field("name").and_then(|f| f.value.string()) == Some("box1"))
        .expect("box1 on the plate");
    let at = part.field("at").and_then(|f| f.value.items()).expect("the box's placement");
    at[0].number().expect("the box's X") * 1000.0
}

#[test]
fn the_picked_parts_editor_follows_the_plate_width() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"),
        "the_picked_parts_editor_follows_the_plate_width").expect("Fab's test config");
    let prefs = config.artifacts_dir.join("view.prefs");
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    run_with_config(config, |app| {
        app.locator(Selector::role(Role::Button).name("Add")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name("Box")).wait_visible().press();
        app.locator(Selector::role(Role::TreeItem).name("box1")).wait_visible().press();
        viewport(&app, 581.0);
        app.locator(tab("Parts").state("selected")).wait_visible();
        app.locator(Selector::id("part_toggle")).wait_hidden();
        app.locator(tab("Part")).wait_visible().press();
        editor(&app, false);

        app.press_key(KeyCode::Backtick);
        editor(&app, true);
        viewport(&app, 1021.0);
        app.locator(tab("Parts").state("selected")).wait_visible();
        let target = box_x(&app) + 5.0;
        app.locator(x_field()).set_value(target);
        app.wait_until(|| {
            let x = box_x(&app);
            ((x - target).abs() > 1e-5).then(|| format!("box1 is at {x} mm, expected {target} mm"))
        });

        render_mode::pick(&app, "Sliced");
        let panel = editor(&app, true);
        let pad = app.locator(Selector::id("time_scrub")).wait_visible().snapshot().bounds;
        assert!(pad.x + pad.w <= panel.x, "the slice pad {pad:?} must clear the editor {panel:?}");
        render_mode::pick(&app, "Normal");
        editor(&app, true);
        app.press_key(KeyCode::Backtick);
        editor(&app, false);
        let view = viewport(&app, 581.0);
        let workspaces = prefs.with_extension("workspaces.ron");
        let before_drag = std::fs::read_to_string(&workspaces).expect("saved workspaces before the sash drag");

        // Keep the same pointer down across both thresholds and the return through the band.
        let mut sash = app.press_at(f64::from(view.x + view.w + 1.5), f64::from(view.center().1));
        for (dx, width, sidebar) in [
            (56.0, 637.0, false), (62.0, 643.0, true),
            (50.0, 631.0, true), (38.0, 619.0, false),
        ] {
            sash.move_by(dx, 0.0);
            viewport(&app, width);
            editor(&app, sidebar);
        }
        sash.release();
        app.wait_until(|| {
            let saved = match std::fs::read_to_string(&workspaces) {
                Ok(text) => text,
                Err(err) => return Some(format!("waiting to read the workspace after the sash drag: {err}")),
            };
            if saved == before_drag {
                return Some("waiting for the sash drag to change the saved workspace".to_owned());
            }
            assert!(!saved.split(|c: char| !c.is_alphanumeric()).any(|word| word == "Part"),
                "the saved workspace contains the transient Part tab");
            None
        });

        app.press_key(KeyCode::Backtick);
        editor(&app, true);
        app.locator(Selector::id("plate_mode")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name("Test prints")).wait_visible().press();
        // Test prints: the tests take the plate's side, so the editor is the Part tab alone. The
        // plate holds the test pieces and keeps the whole plate to restore, so nothing can be
        // moved and the editor has no X: one editor (the tab's panel, no sidebar group), no X.
        app.locator(tab("Part").state("selected")).wait_visible();
        app.locator(Selector::id("part_toggle")).wait_hidden();
        let panel = app.locator(Selector::role(Role::TabPanel).name("Part")).wait_count(1).snapshot().bounds;
        app.locator(Selector::role(Role::Group).name("Part")).wait_count(0);
        app.locator(x_field()).wait_count(0);
        app.locator(Selector::id("tests_toggle")).wait_visible().check(true);
        app.locator(Selector::role(Role::Button).name("Close tests")).wait_visible();
        // tests_side fills the plate, with its card aligned inside the lower right corner.
        let plate = viewport(&app, 1021.0);
        assert!(panel.x + panel.w <= plate.x, "the Part editor {panel:?} must clear the test sidebar in {plate:?}");
        assert_eq!(app.locator(x_field()).count(), 0, "Test prints has no X, the tests' sidebar open");

        // Back to Design: the whole plate is restored, and the part's X is back once, in the sidebar.
        app.locator(Selector::id("plate_mode")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name("Design")).wait_visible().press();
        app.locator(Selector::id("tests_toggle")).wait_hidden();
        app.locator(Selector::role(Role::Button).name("Close tests")).wait_hidden();
        editor(&app, true);
        app.locator(Selector::role(Role::TabPanel).name("Part")).wait_count(0);
        app.locator(tab("Parts").state("selected")).wait_visible();
    }).unwrap();
}

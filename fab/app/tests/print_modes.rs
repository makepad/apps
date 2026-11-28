//! Drive the generated slicer rows through the app's event path, then inspect document history:
//! one boot, one project. The global tab and an object's override, two objects edited as one
//! undo step, and every print mode's toggle and a parameter (commit, preserved, two undos).

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use fab::ui::print_settings::control_name;
use fab_plate::{print_setting, PrintSetting, SettingKind, PRINT_SETTINGS};
use makepad_script::syntax::{self, Kind, Node};
use makepad_strict_json::Value;
use makepad_test::{run_with_config, KeyCode, Role, Selector, States, TestApp, TestConfig};
use std::time::Duration;

fn remote(app: &TestApp, args: &[String]) -> Result<Vec<String>, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, args, &mut |line| {
        lines.push(line.to_string());
        true
    }).map_err(|e| format!("{args:?}: {e:?}; {lines:?}"))?;
    if lines.iter().any(|l| l.contains("\"err\":")) {
        return Err(format!("{args:?}: {lines:?}"));
    }
    Ok(lines)
}

fn call(app: &TestApp, tool: &str, args: &str) -> Result<Node, String> {
    let lines = remote(app, &["call".into(), format!("fab.{tool}"), args.into()])?;
    let line = lines.iter().find(|l| l.contains("\"Result\"")).ok_or_else(|| format!("no result: {lines:?}"))?;
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 64).map_err(|e| format!("{e:?}"))?;
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => &a[0],
        other => other,
    };
    let text = r.get("text").and_then(Value::as_str).unwrap_or_default();
    syntax::parse_value(text).map_err(|e| format!("{e:?}: {text}"))
}

fn project(app: &TestApp) -> String {
    call(app, "inspect", "{}").expect("inspect").field("project").and_then(|f| f.value.string()).expect("project source").to_string()
}

/// The `settings: {..}` argument of the `place(.., name: <name>)` call.
fn settings(source: &str, name: &str) -> Option<Node> {
    let tree = syntax::parse(source).expect("project is Splash");
    for loc in tree.find_calls("place") {
        let args = tree.node(&loc).expect("place node").call("place").expect("place arguments");
        let named = args[1..].iter().any(|arg| matches!(&arg.kind, Kind::Entry { name: n, value, .. } if n == "name" && value.string() == Some(name)));
        if !named {
            continue;
        }
        return args[1..].iter().find_map(|arg| match &arg.kind {
            Kind::Entry { name, value, .. } if name == "settings" => Some((**value).clone()),
            _ => None,
        });
    }
    panic!("no place(.., name: {name:?}) call: {source}");
}

fn setting(source: &str, name: &str, key: &str) -> Option<String> {
    settings(source, name)?.field(key)?.value.string().map(str::to_string)
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    wait_poll(app, || {
        if !ok() { Err(format!("timed out waiting for {what}")) } else { Ok(()) }
    });
}

fn panel_bar(app: &TestApp) -> Option<makepad_test::NodeMatch> {
    let panel = app.locator(Selector::widget_type("FabSlicerPanel")).wait_visible().snapshot().bounds;
    app.widget_snapshot().nodes().into_iter().find(|n| {
        let (x, y) = n.bounds.center();
        n.role == Role::ScrollBar && n.visible() && panel.contains(x, y)
    })
}

/// Walk the panel's scroll range; rows outside the clip are absent from the mirror, and a
/// mode's rows appear only once the plate that turned it on reaches the panel.
fn reveal(app: &TestApp, name: &str) {
    let shown = || app.widget_snapshot().nodes().iter().any(|n| n.name == name && n.visible());
    let panel_h = app.locator(Selector::widget_type("FabSlicerPanel")).wait_visible().snapshot().bounds.h as f64;
    wait_poll(app, || {
        if shown() { return Ok(()); }
        let Some(node) = panel_bar(app) else {
            return Err(format!("{name} did not appear in the slicer panel"));
        };
        let range = node.range.expect("scroll range");
        let next = if range.now >= range.max { 0.0 } else { (range.now + panel_h * 0.5).min(range.max) };
        app.locator(Selector::raw(format!("id={}", node.id.short()))).set_value(&next.to_string());
        // (the range can shrink under the move when rows hide: the bar then rests at its end)
        wait_until(app, "scroll position", || panel_bar(app).and_then(|b| b.range).is_none_or(|r| (r.now - next).abs() < 0.1 || r.now >= r.max - 0.1));
        Err(format!("{name} did not appear in the slicer panel"))
    });
}

fn set(app: &TestApp, setting: &PrintSetting, value: &str) {
    let name = control_name(setting);
    match setting.kind {
        SettingKind::Bool => {
            reveal(app, &name);
            app.locator(Selector::role(Role::CheckBox).name(name)).toggle();
        }
        SettingKind::Number { .. } => {
            reveal(app, &name);
            app.locator(Selector::role(Role::SpinButton).name(name)).set_value(value.trim_end_matches('%'));
        }
        SettingKind::Choice(choices) => {
            let label = choices.iter().find(|(v, _)| *v == value).expect("the choice").1;
            reveal(app, &name);
            let dropdown = app.widget_snapshot().nodes().iter()
                .any(|n| n.visible() && n.role == Role::ComboBox && n.name == name);
            if dropdown {
                let combo = Selector::role(Role::ComboBox).name(&name);
                app.locator(combo.clone()).expand();
                let mut item = label.to_string();
                if !setting.common.is_empty() {
                    app.locator(Selector::role(Role::MenuItem).name("More patterns…")).wait_visible();
                    if let Some((_, note)) = setting.common.iter().find(|(v, _)| *v == value) {
                        item = format!("{label}: {note}");
                    }
                    if !app.widget_snapshot().nodes().iter().any(|n| n.visible() && n.role == Role::MenuItem && n.name == item) {
                        app.locator(combo.clone()).select_option("More patterns…");
                        item = label.to_string();
                    }
                }
                app.locator(Selector::role(Role::MenuItem).name(&item)).wait_visible();
                app.locator(combo).select_option(item);
            } else {
                let tab = format!("{name} {label}");
                app.locator(Selector::role(Role::Radio).name(&tab)).wait_visible();
                app.locator(Selector::role(Role::Radio).name(tab)).select();
            }
        }
    }
}

/// Waits for a node; when it never shows, says what does.
fn see(app: &TestApp, selector: Selector, what: &str) {
    if app.locator(selector).try_wait_visible().is_err() {
        let nodes: Vec<String> = app.widget_snapshot().nodes().iter().filter(|n| n.visible()).take(100).map(|n| format!("{:?} {:?}", n.role, n.name)).collect();
        panic!("{what} never showed; visible: {nodes:?}");
    }
}

/// The Parts list showing the part, then the Slicer panel.
fn show_slicer(app: &TestApp, name: &str) {
    see(app, Selector::role(Role::Tab).name("Parts"), "the Parts tab");
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    see(app, Selector::role(Role::TreeItem).name(name), &format!("the part {name}"));
    see(app, Selector::role(Role::Tab).name("Slicer"), "the Slicer tab");
    app.locator(Selector::role(Role::Tab).name("Slicer")).click();
}

const BOX: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet box = m.box({size: [20mm 20mm 20mm]})\nobject(box)\n";

/// The box script on the plate as a box named `name`, the Slicer panel showing.
fn one_box(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait_until(app, "Fab tools", || call(app, "inspect", "{}").is_ok());
    let reply = call(app, "inspect", "{}").unwrap();
    let head = reply.field("head").and_then(|f| f.value.string()).expect("head");
    let batch = format!(r#"{{"request_id":"print-modes","expected":{head:?},"script":"box.cad.splash","source":{}}}"#, Value::Str(BOX.into()).to_json());
    let reply = call(app, "script", &batch).expect("one-box script");
    let head = reply.field("head").and_then(|f| f.value.string()).expect("head");
    let edits = format!(r#"place("box", name: "{name}")"#);
    let batch = format!(r#"{{"request_id":"print-modes-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits).to_json());
    call(app, "place", &batch).expect("place the box");
    wait_until(app, "one-box script", || {
        let reply = call(app, "inspect", "{}").expect("inspect");
        reply.field("scripts").and_then(|f| f.value.items()).and_then(|scripts| scripts.iter().find(|s| s.field("file").and_then(|f| f.value.string()) == Some("box.cad.splash")))
            .and_then(|s| s.field("source")).and_then(|f| f.value.string()) == Some(BOX)
    });
    show_slicer(app, name);
}

/// Another box from the same script, as its own instance called `instance`, named `name`.
fn add_box(app: &TestApp, instance: &str, name: &str) {
    let reply = call(app, "inspect", "{}").unwrap();
    let head = reply.field("head").and_then(|f| f.value.string()).expect("head");
    let edits = format!("script(\"box.cad.splash\", name: \"{instance}\")\nplace(\"{instance}\", name: \"{name}\")");
    let batch = format!(r#"{{"request_id":"print-modes-{instance}","expected":{head:?},"edits":{}}}"#, Value::Str(edits).to_json());
    call(app, "place", &batch).expect("another box");
    show_slicer(app, name);
}

fn select_box(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::TreeItem).name(name)).select();
    see(app, Selector::widget_type("FabSlicerPanel"), "the slicer panel");
    see(app, Selector::role(Role::CheckBox).name("Advanced"), "the Advanced check box");
}

fn show_advanced(app: &TestApp) {
    app.locator(Selector::role(Role::CheckBox).name("Advanced")).toggle();
}

/// One mode: its toggle and one of its parameters land in the project text (the parameter edit
/// keeps the toggle), and two undos walk both back. With `full`, redo restores both and setting
/// each back to its default removes it, the last one taking the whole `settings` argument with it.
fn round_trip(app: &TestApp, name: &str, mode: &str, parameter_key: &str, parameter_value: &str, full: bool) {
    let toggle = PRINT_SETTINGS.iter().find(|s| s.mode == mode && s.needs.is_empty()).expect("mode toggle");
    let parameter = print_setting(parameter_key).expect("parameter");
    let enabled = match toggle.kind {
        SettingKind::Bool => "1",
        SettingKind::Choice(c) => c[1].0,
        _ => panic!("toggle kind"),
    };
    let before = project(app);
    set(app, toggle, enabled);
    wait_until(app, &format!("{mode}: toggle commit"), || setting(&project(app), name, toggle.key).as_deref() == Some(enabled));
    let toggled = project(app);
    set(app, parameter, parameter_value);
    wait_until(app, &format!("{mode}: parameter commit"), || setting(&project(app), name, parameter.key).as_deref() == Some(parameter_value));
    let after = project(app);
    assert_eq!(setting(&after, name, toggle.key).as_deref(), Some(enabled), "{mode}: a parameter edit preserves the toggle");
    app.locator(Selector::id("undo").nth(0)).press();
    wait_until(app, &format!("{mode}: the first undo removes only the parameter"), || project(app) == toggled);
    app.locator(Selector::id("undo").nth(0)).press();
    wait_until(app, &format!("{mode}: the second undo restores the original project"), || project(app) == before);
    if !full {
        return;
    }
    // Redo restores both values. Back at the inherited value a key goes, the last one too.
    app.locator(Selector::id("redo").nth(0)).press();
    wait_until(app, "redo toggle", || project(app) == toggled);
    app.locator(Selector::id("redo").nth(0)).press();
    wait_until(app, "redo parameter", || project(app) == after);
    set(app, parameter, parameter.default);
    wait_until(app, "default removes parameter", || setting(&project(app), name, parameter.key).is_none());
    assert_eq!(setting(&project(app), name, toggle.key).as_deref(), Some(enabled), "setting the parameter back keeps the toggle");
    set(app, toggle, toggle.default);
    app.try_wait_until_within(HANG, || settings(&project(app), name).is_some().then(|| format!("waiting for {mode}: the settings argument to go"))).unwrap_or_else(|why| {
        for l in project(app).lines().filter(|l| l.contains("place(") || l.contains("settings")) {
            println!("[print-modes] project: {l}");
        }
        panic!("{why}; {mode}: the settings argument was not removed");
    });
}

/// The global tab edits `settings(...)`; an object's override shows on its row and in the
/// parts list, its reset goes back to global, undo walks all of it, and a plate-only row is
/// read-only for an object.
fn global_and_override(app: &TestApp, name: &str) {
    // Nothing selected: the rows are the plate's. The wipe tower's switch is one, saved with it.
    set(app, print_setting("enable_prime_tower").unwrap(), "1");
    wait_until(app, "the tower's switch", || project(app).contains("settings({enable_prime_tower: \"1\"})"));
    app.locator(Selector::id("undo").nth(0)).press();
    wait_until(app, "undo the switch", || !project(app).contains("enable_prime_tower"));
    let density = print_setting("sparse_infill_density").unwrap();
    set(app, density, "40%");
    wait_until(app, "global commit", || project(app).contains("settings({sparse_infill_density: \"40%\"})"));
    select_box(app, name);
    app.locator(Selector::path("head.note").name("No overrides: the global settings apply.")).wait_visible();
    // The object inherits it: 40, and a value equal to the global one is no override; a new
    // value is (edits land in order, so the first would have shown by the time the second has).
    set(app, density, "40%");
    let walls = print_setting("wall_loops").unwrap();
    set(app, walls, "5");
    wait_until(app, "override commit", || setting(&project(app), name, "wall_loops").as_deref() == Some("5"));
    assert_eq!(setting(&project(app), name, "sparse_infill_density"), None, "a value equal to the global one is no override");
    // the override's row in the Overrides editor's list (Design's, under the Part's stack), the
    // object's
    let row = format!("{} 5 (global ", walls.label);
    app.locator(Selector::role(Role::Tab).name("Overrides")).click();
    wait_until(app, "the override's row", || {
        app.widget_snapshot().nodes().iter().any(|n| n.role == Role::ListItem && n.visible() && n.name.starts_with(&row) && n.description == name)
    });
    // The row's reset.
    app.locator(Selector::id("reset").nth(0)).press();
    wait_until(app, "reset goes back to global", || settings(&project(app), name).is_none());
    // Undo brings it back; the plate's text keeps the global set throughout.
    app.locator(Selector::id("undo").nth(0)).press();
    wait_until(app, "undo the reset", || setting(&project(app), name, "wall_loops").as_deref() == Some("5"));
    assert!(project(app).contains("settings({sparse_infill_density: \"40%\"})"), "the global set stayed");
    // The search finds an advanced row; the plate-only row takes no edit.
    let first = print_setting("initial_layer_print_height").unwrap();
    reveal(app, &control_name(first));
    // The row stays in the panel but takes no edit: its control is read-only (the reason is its
    // tooltip, which the semantic tree does not carry).
    wait_until(app, "the plate-only row is read-only for an object", || {
        app.widget_snapshot().nodes().iter().any(|n| n.visible() && n.role == Role::SpinButton && n.name == control_name(first) && n.node.states.contains(States::READ_ONLY))
    });
}

/// Two objects selected: one edit lands on both, as one undo step.
fn multi_select(app: &TestApp, twin: &str, other: &str) {
    add_box(app, "other", other);
    app.locator(Selector::role(Role::TreeItem).name(twin)).select();
    app.locator(Selector::role(Role::TreeItem).name(other)).add_selection();
    wait_until(app, "two objects", || app.widget_snapshot().nodes().iter().any(|n| n.name == "2 objects" && n.visible()));
    let before = project(app);
    set(app, print_setting("wall_loops").unwrap(), "6");
    wait_until(app, "both objects", || project(app).matches("wall_loops: \"6\"").count() == 2);
    app.locator(Selector::id("undo").nth(0)).press();
    wait_until(app, "one undo takes both back", || project(app) == before);
}

fn with_app(name: &str, body: impl FnOnce(TestApp)) {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), format!("print_modes::{name}")).unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    run_with_config(config, body).unwrap();
}

/// The generated slicer rows through the app's event path, in one boot: the global tab and an
/// object's override, two objects edited as one, then every mode's toggle and a parameter.
#[test]
fn slicer_rows_commit_to_the_project_and_undo() {
    with_app("rows", |app| {
        println!("[print-modes] global");
        one_box(&app, "Global box");
        global_and_override(&app, "Global box");

        let pattern = print_setting("sparse_infill_pattern").unwrap();
        set(&app, pattern, "lightning");
        wait_until(&app, "the common pattern in the project", || setting(&project(&app), "Global box", pattern.key).as_deref() == Some("lightning"));

        let before = project(&app);
        let combo = Selector::role(Role::ComboBox).name(control_name(pattern));
        app.locator(combo.clone()).expand();
        let noted = app.locator(Selector::role(Role::MenuItem).name("Lightning: least plastic, only holds up the top")).wait_visible().snapshot().bounds;
        let field = app.locator(combo.clone()).snapshot().bounds;
        assert!(noted.w > field.w, "the short list's notes are cut to the field: its menu is {} wide, the field {}", noted.w, field.w);
        assert!(!app.widget_snapshot().nodes().iter().any(|n| n.visible() && n.role == Role::MenuItem && n.name == "Honeycomb"), "Honeycomb belongs under More");
        app.locator(combo.clone()).select_option("More patterns…");
        app.locator(Selector::role(Role::MenuItem).name("Honeycomb")).wait_visible();
        assert_eq!(app.locator(combo.clone()).snapshot().value, "Lightning", "More keeps the current selection");
        app.locator(combo.clone()).press_key(KeyCode::Escape);
        assert_eq!(project(&app), before, "dismissing More keeps the project");

        set(&app, pattern, "honeycomb");
        wait_until(&app, "the full-list pattern in the project", || setting(&project(&app), "Global box", pattern.key).as_deref() == Some("honeycomb"));
        wait_until(&app, "Honeycomb selected on the closed pattern dropdown", || {
            let face = app.locator(combo.clone()).snapshot();
            face.states.contains(States::COLLAPSED) && face.value == "Honeycomb"
        });

        println!("[print-modes] multi");
        multi_select(&app, "Global box", "Other box");

        println!("[print-modes] modes");
        add_box(&app, "mode", "Mode box");
        println!("[print-modes] select");
        select_box(&app, "Mode box");
        show_advanced(&app);
        // The first mode also walks redo and the removal at the default.
        println!("[print-modes] Ironing");
        round_trip(&app, "Mode box", "Ironing", "ironing_flow", "15%", true);
        for (mode, parameter, value) in [
            ("Wall texture", "fuzzy_skin_thickness", "0.5"),
            ("Spiral vase", "fab_vase_variant", "double_wall"),
            ("Scarf seams", "seam_slope_min_length", "15"),
            ("Adaptive layers", "fab_adaptive_cusp", "0.05"),
            ("Material interlock", "interlocking_beam_width", "1.2"),
            ("Arc overhangs", "fab_arc_overhang_speed", "7.5"),
            ("Non-planar top", "fab_nonplanar_shells", "3"),
        ] {
            println!("[print-modes] {mode}");
            round_trip(&app, "Mode box", mode, parameter, value, false);
        }
    });
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

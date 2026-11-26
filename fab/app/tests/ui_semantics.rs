//! Fab is driven through its semantic tree, the way a screen reader, an
//! automation client or the AI drives it: `ui act` on named nodes, never on
//! coordinates. A part goes on the plate through the tools, then:
//!
//! - the parts tree nests linked duplicates under their script, collapses, expands and
//!   offers its context menu; a copy stays flat; F2 renames;
//! - a click on a row of the parts list shows the selection in the next frame, also between
//!   two placements of one script: the code panel does not change there, so nothing but the
//!   selection asks for a redraw (the row used to stay unselected until some other input
//!   redrew the panel);
//! - the workspace switch is selected by name (the plate view's header has no Plate mode control
//!   of its own: the tabs set the mode), the Slice render mode by its switch;
//! - a checkbox (Supports) toggles through the
//!   action path and read back;
//! - the Slice render mode slices the plate, and the layer slider takes a value: the
//!   view's top layer moves to it, as the playback bar's label says.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
#[path = "common/workspace_row.rs"]
mod workspace_row;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{makepad_test, KeyCode, KeyModifiers, NodeId, Role, Selector, States, TestApp};
use std::time::{Duration, Instant};

/// How long a list click may take to show (it is a frame or two; the bound
/// leaves room for a loaded CI box, the bug never showed it at all).
const SHOWN_WITHIN: Duration = Duration::from_millis(400);

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

/// The playback bar's "Layer n / m" label.
fn wait_layer(app: &TestApp) -> String {
    wait_poll(app, || {
        let names = app.widget_snapshot().nodes().into_iter().filter(|w| w.visible()).map(|w| w.node.name);
        if let Some(t) = names.into_iter().find(|t| t.strip_prefix("Layer ").is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))) {
            return Ok(t);
        }
        Err("no layer readout".to_string())
    })
}

/// "Layer 12 / 40 · …" -> (12, 40)
fn layer_of(label: &str) -> (u32, u32) {
    let rest = label.trim_start_matches("Layer ");
    let mut it = rest.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty());
    let a = it.next().and_then(|s| s.parse().ok()).expect("a layer number");
    let b = it.next().and_then(|s| s.parse().ok()).expect("a layer count");
    (a, b)
}

#[makepad_test]
fn experimental_toggle_removes_closed_menu_rows_and_keeps_actions_working(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    // Toggle both ways: each close must remove the rows, including the flyout,
    // and the next menu must still dispatch through its semantic actions.
    for _ in 0..2 {
        app.locator(Selector::role(Role::Button).name("View")).click();
        app.locator(Selector::role(Role::MenuItem).name("Experimental")).click();
        app.locator(Selector::role(Role::MenuItem).name("Arm")).click();
        let rows: Vec<_> = app.widget_snapshot().nodes().into_iter()
            .filter(|w| matches!(w.node.role, Role::Menu | Role::MenuItem))
            .map(|w| w.node.name)
            .collect();
        assert!(rows.is_empty(), "closed menus must leave the tree, including hidden rows: {rows:?}");
        app.locator(Selector::role(Role::Button).name("View")).click();
        app.locator(Selector::role(Role::MenuItem).name("Frame All")).click();
        assert!(!app.widget_snapshot().nodes().into_iter().any(|w| matches!(w.node.role, Role::Menu | Role::MenuItem)),
            "the next menu action must close and remove its rows too");
    }
}

#[makepad_test]
fn the_view_and_its_controls_are_driven_through_the_semantic_tree(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || {
        call(&app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}"))
    });
    let head = head_of(&text(&line));
    let source = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let block = m.box({size: [20mm 12mm 20mm]})
object(block)
"##;
    let batch = format!(
        r#"{{"request_id":"a11y-1","expected":{head:?},"script":"block.cad.splash","source":{}}}"#,
        Value::Str(source.to_string()).to_json(),
    );
    let reply = text(&call(&app, "script", &batch).expect("script"));
    let head = head_of(&reply);
    let edits = r#"place("block", name: "Block")"#;
    let batch = format!(r#"{{"request_id":"a11y-1-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let reply = text(&call(&app, "place", &batch).expect("place"));
    assert!(reply.contains("solid: \"block/block\""), "{reply}");
    // The part is a named row of the parts list.
    // The parts list is a tab beside the program's solids.
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    let block = Selector::role(Role::TreeItem).name("Block");
    let base_id = app.locator(block.clone()).wait_visible().snapshot().node.id;
    assert_eq!(app.locator(block.clone()).snapshot().node.level, 1, "one instance is flat");
    app.locator(block.clone()).select();
    let primary = KeyModifiers { logo: cfg!(target_vendor = "apple"), control: !cfg!(target_vendor = "apple"), ..Default::default() };
    app.press_key_with_modifiers(KeyCode::KeyD, primary);
    let group = Selector::role(Role::TreeItem).name("block");
    let clone = Selector::role(Role::TreeItem).name("Block 2");
    app.locator(clone.clone()).wait_visible();
    let group_node = app.locator(group.clone()).wait_visible().snapshot().node;
    let base = app.locator(block.clone()).snapshot().node;
    let linked = app.locator(clone.clone()).snapshot().node;
    assert_eq!(base.id, base_id, "nesting keeps the part's accessible identity");
    assert_eq!((base.level, linked.level), (2, 2), "linked placements nest under their script");
    assert!(group_node.children.contains(&base.id) && group_node.children.contains(&linked.id), "the script owns its part rows");
    assert!(group_node.states.contains(States::EXPANDED), "the script's row starts expanded");
    app.locator(group.clone()).select();
    app.locator(block.clone().state("selected")).wait_visible();
    app.locator(clone.clone().state("selected")).wait_visible();
    semantic_action(&app, group_node.id, "collapse");
    app.locator(block.clone()).wait_hidden();
    app.locator(clone.clone()).wait_hidden();
    app.locator(group.clone().state("collapsed")).wait_visible();
    semantic_action(&app, group_node.id, "expand");
    app.locator(block.clone()).wait_visible();
    assert_eq!(app.locator(clone.clone()).wait_visible().snapshot().node.id, linked.id, "expand brings the same rows back");
    // ---- a click on a row shows in the next frame, between two placements of one script ----
    // Nothing slices in Normal, so nothing else redraws the panel.
    // Both rows are selected (their script's row was): a click on one leaves it alone selected.
    app.locator(block.clone()).click_at_center();
    wait_poll(&app, || {
        if app.locator(clone.clone().state("selected")).count() > 0 { Err("a click on 'Block' did not leave it alone selected".to_string()) } else { Ok(()) }
    });
    for name in ["Block 2", "Block", "Block 2", "Block"] {
        let took = click_row(&app, name);
        assert!(took < SHOWN_WITHIN, "the click on '{name}' showed after {} ms", took.as_millis());
    }
    semantic_action(&app, linked.id, "show_menu");
    for name in ["Duplicate (linked)", "Duplicate as copy", "Delete", "Delete script (all linked instances)"] {
        app.locator(Selector::role(Role::MenuItem).name(name)).wait_visible();
    }
    app.press_key(KeyCode::Escape);
    // One gesture duplicates both selected placements, once each.
    app.locator(group.clone()).select();
    app.press_key_with_modifiers(KeyCode::KeyD, primary);
    app.locator(Selector::role(Role::TreeItem)).wait_count(5);
    app.locator(block.clone()).select();
    app.press_key_with_modifiers(KeyCode::KeyD, KeyModifiers { shift: true, ..primary });
    let copied = app.locator(Selector::role(Role::TreeItem).name("Block copy")).wait_visible().snapshot().node;
    assert_eq!(copied.level, 1, "a copy has its own script and stays flat");
    assert!(!app.locator(group.clone()).snapshot().node.children.contains(&copied.id), "a copy is not under the script's row");
    // F2 on the script node edits its base part through the same Rename action.
    app.locator(group.clone()).select();
    app.press_key(KeyCode::F2);
    app.locator(Selector::role(Role::TextField).name("Rename part")).wait_visible().fill("Renamed block").press_key(KeyCode::ReturnKey);
    assert_eq!(app.locator(Selector::role(Role::TreeItem).name("Renamed block")).wait_visible().snapshot().node.id, base_id, "F2 on the script renames its base part");

    // A part row exposes its rename field by role and name, too.
    app.locator(clone.clone()).select();
    app.press_key(KeyCode::F2);
    app.locator(Selector::role(Role::TextField).name("Rename part")).wait_visible().fill("Renamed placement").press_key(KeyCode::ReturnKey);
    assert_eq!(app.locator(Selector::role(Role::TreeItem).name("Renamed placement")).wait_visible().snapshot().node.id, linked.id, "F2 on a part renames that placement");

    // ---- the workspace switch, then the Slice render mode ----
    // The workspace tabs set the plate's mode: the plate view's header, drawn with its render
    // switch, has no Plate mode control of its own beside it (it once doubled the tabs).
    for ws in ["Monitor", "3D modeling"] {
        workspace_row::press(&app, ws);
        app.locator(Selector::id("rm_slice")).wait_visible();
        assert_eq!(app.locator(Selector::all().name("Plate mode")).count(), 0, "the plate view's header in {ws} has no Plate mode control");
    }
    app.locator(Selector::id("rm_slice")).click();
    app.locator(Selector::id("rm_slice")).wait_checked(true);
    // Sliced brings the Slice panel forward beside the parts.
    app.locator(Selector::role(Role::Tab).name("Slice")).wait_visible();
    app.locator(Selector::widget_type("FabSlicePanel")).wait_visible();
    // ---- the caption's Code toggle hides and shows the code split ----
    // the code is an area of its own: alone in it, its nub and no tab
    let code_tab = |app: &TestApp| app.locator(Selector::id("code")).count() > 0;
    let shown = code_tab(&app);
    app.locator(Selector::role(Role::ToggleButton).name("Code")).click();
    wait_poll(&app, || {
        if code_tab(&app) == shown { Err(format!("the Code toggle did not change the code split (shown: {shown})")) } else { Ok(()) }
    });
    app.locator(Selector::role(Role::ToggleButton).name("Code")).click();
    wait_poll(&app, || {
        if code_tab(&app) != shown { Err("the Code toggle did not bring the code split back".to_string()) } else { Ok(()) }
    });
    // ---- the caption's menus: a click on a title's last letter opens that title's menu ----
    for (title, row) in [("File", "Open…"), ("Edit", "Undo"), ("View", "Frame All"), ("Help", "Keymap Reference")] {
        let b = app.locator(Selector::role(Role::Button).name(title)).wait_visible().snapshot().node.bounds;
        let (x, y) = (b.x + b.w - 9.0, b.y + b.h * 0.5);
        click_at(&app, x as f64, y as f64);
        app.locator(Selector::role(Role::MenuItem).name(row)).wait_visible();
        app.press_key(makepad_test::KeyCode::Escape);
    }
    // ---- every submenu of View is reachable: hover its row, go diagonally into the flyout ----
    for (parent, child) in [("Viewpoint", "Top"), ("Shading", "Mesh"), ("Overlays", "Grid"), ("Measure", "Measure Distance"), ("Workspace", "Monitor")] {
        let b = app.locator(Selector::role(Role::Button).name("View")).wait_visible().snapshot().node.bounds;
        click_at(&app, (b.x + b.w * 0.5) as f64, (b.y + b.h * 0.5) as f64);
        let row = app.locator(Selector::role(Role::MenuItem).name(parent)).wait_visible().snapshot().node.bounds;
        let (x0, y0) = ((row.x + row.w * 0.4) as f64, (row.y + row.h * 0.5) as f64);
        mouse(&app, "move", x0, y0);
        let target = app.locator(Selector::role(Role::MenuItem).name(child)).wait_visible().snapshot().node.bounds;
        let (x1, y1) = ((target.x + target.w * 0.3) as f64, (target.y + target.h * 0.5) as f64);
        // A person's path, a move every frame (each move waits for its frame), across the rows
        // below and the bubbles' padding.
        for k in 1..=12 {
            let f = k as f64 / 12.0;
            mouse(&app, "move", x0 + (x1 - x0) * f, y0 + (y1 - y0) * f);
        }
        let still = app.widget_snapshot().nodes().into_iter().any(|w| w.visible() && w.node.role == Role::MenuItem && w.node.name.starts_with(child));
        assert!(still, "View › {parent}: its flyout closed on the way into it");
        app.press_key(makepad_test::KeyCode::Escape);
        app.press_key(makepad_test::KeyCode::Escape);
    }
    // ---- a checkbox, in the Slicer panel ----
    app.locator(Selector::role(Role::Tab).name("Slicer")).wait_visible().click();
    app.locator(Selector::id("search.row.input")).wait_visible().fill("supports");
    let supports = Selector::role(Role::CheckBox).name("Support and brim Supports");
    app.locator(supports.clone()).wait_visible();
    app.locator(supports.clone()).check(true);
    app.locator(supports.clone()).wait_checked(true);
    app.locator(supports.clone()).check(false);
    app.locator(supports).wait_checked(false);

    // ---- slice, then the layer slider ----
    let first = wait_layer(&app);
    let (top, count) = layer_of(&first);
    assert_eq!(top, count, "a fresh slice shows the whole print: {first}");
    assert!(count > 4, "a 12 mm block has more than four layers: {first}");
    let slider = Selector::role(Role::Slider).name("Top layer");
    app.locator(slider).set_value(0.5);
    let moved = wait_poll(&app, || {
        let label = wait_layer(&app);
        let (now, _) = layer_of(&label);
        if now < count {
            return Ok(now);
        }
        Err(format!("the layer slider did not move the view: {label}"))
    });
    let expected = (count as f64 * 0.5).round() as i64;
    assert!((moved as i64 - expected).abs() <= 1, "the top layer is {moved}, expected about {expected} of {count}");
}

/// Click the row (real pointer input) and time until it reads as selected.
fn click_row(app: &TestApp, name: &str) -> Duration {
    let selected = Selector::role(Role::TreeItem).name(name).state("selected");
    assert_eq!(app.locator(selected.clone()).count(), 0, "'{name}' was selected before the click");
    app.locator(Selector::role(Role::TreeItem).name(name)).wait_visible().click_at_center();
    let clicked = Instant::now();
    // the app's own wait sees the row selected on the frame it shows
    app.locator(selected).wait_visible();
    clicked.elapsed()
}

/// A raw click at window coordinates (the point a person would press).
fn click_at(app: &TestApp, x: f64, y: f64) {
    mouse(app, "click", x, y);
}

/// Raw mouse input at window coordinates (`move`, `click`, `down`, `up`).
fn mouse(app: &TestApp, kind: &str, x: f64, y: f64) {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect");
    let args = vec!["mouse".to_string(), format!("k={kind}"), format!("x={x:.1}"), format!("y={y:.1}"), "wait=1".to_string()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &args, &mut |l| { lines.push(l.to_string()); true }).expect("click");
    assert!(!lines.iter().any(|l| l.contains("\"err\":")), "mouse {kind} at {x},{y}: {lines:?}");
}

/// Exercise the tree's expand/collapse and context-menu semantic actions directly.
fn semantic_action(app: &TestApp, node: NodeId, action: &str) {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect");
    let args = vec!["ui".into(), "act".into(), action.into(), format!("node={}", node.short()), "wait=1".into()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &args, &mut |line| { lines.push(line.to_string()); true }).expect("semantic action");
    assert!(!lines.iter().any(|l| l.contains("\"err\":")), "{action}: {lines:?}");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

//! Edits in the Sliced view once a plan is out, in a real window on the user's plate: the demo's
//! three parts, the bracket and the vase not printing, tree supports on the lattice cube. The
//! first plan goes out; an edit made while its supports' report is still measured, a setting
//! toggled, the overhang angle stepped and a no-trees box drawn on the bed each commit (the
//! plate's head moves) and plan again (their own revision's toolpaths come).
//!
//! Pins the model worker's wait for the supports' report (the overhang list's): an edit queued
//! behind it held the slice, which stopped the measure it waited for, and no edit applied again.
//! The tools (`fab.inspect` too) are served on that worker, behind the overhang list: from the
//! first plan until the edit's own, the test reads only the log and the window, as the person
//! only edits; one call would let the report in before the edit.

#[path = "common/render_mode.rs"]
mod render_mode;
use makepad_script::syntax;
use makepad_strict_json::Value;
use makepad_test::{makepad_test, KeyCode, KeyModifiers, Role, Selector, TestApp};
use std::time::Duration;

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let argv = vec!["call".to_string(), format!("fab.{tool}"), args.to_string()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("fab.{tool}: {e:?}"))?;
    let line = lines.iter().find(|l| l.contains("\"Result\"")).ok_or_else(|| format!("fab.{tool} returned no result: {lines:?}"))?;
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).map_err(|e| format!("{e:?}"))?;
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    Ok(r.get("text").and_then(Value::as_str).unwrap_or_default().to_string())
}

fn inspect(app: &TestApp) -> syntax::Node {
    syntax::parse_value(&call(app, "inspect", r#"{"program":false}"#).expect("inspect")).expect("a Splash reply")
}

fn head(app: &TestApp) -> String {
    inspect(app).field("head").and_then(|f| f.value.string()).expect("a head").to_string()
}

fn lines(app: &TestApp, from: u64) -> Vec<String> {
    app.log_since(from).expect("the app's log").lines
}

fn seq(app: &TestApp) -> u64 {
    app.log_since(0).map(|t| t.last_seq).unwrap_or(0)
}

/// The number after `name` in `line`.
fn field(line: &str, name: &str) -> Option<u64> {
    let at = line.find(name)? + name.len();
    let digits: String = line[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn wait<T>(app: &TestApp, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let mut got = None;
    app.try_wait_until_within(HANG, || {
        got = f();
        got.is_none().then(|| format!("waiting for {what}"))
    }).unwrap_or_else(|why| {
        for l in lines(app, 0).iter().filter(|l| l.contains("fab")).rev().take(40).collect::<Vec<_>>().into_iter().rev() {
            println!("[app] {l}");
        }
        panic!("{why}; timed out waiting for {what}");
    });
    got.expect("the wait ended on an answer")
}

/// The slice's newest input since log line `from`, once its toolpaths are out: its revision.
fn planned(app: &TestApp, from: u64, what: &str) -> u64 {
    wait(app, what, || {
        let lines = lines(app, from);
        let newest = lines.iter().filter_map(|l| field(l, "fab live input: revision=")).last()?;
        lines.iter().any(|l| l.contains("fab live work:") && field(l, "revision=") == Some(newest)).then_some(newest)
    })
}

/// An edit, made by `edit`: it commits (the head moves) and the slice plans again (a newer
/// revision than `after`'s toolpaths come). The new revision.
fn commits_and_plans_again(app: &TestApp, what: &str, after: u64, edit: impl FnOnce()) -> u64 {
    let (from, was) = (seq(app), head(app));
    edit();
    wait(app, &format!("{what} to commit"), || (head(app) != was).then_some(()));
    let revision = planned(app, from, &format!("{what}'s plan"));
    assert!(revision > after, "{what}: planned revision {revision}, not one after {after}");
    println!("[edits-in-sliced] {what}: committed, revision {revision} planned");
    revision
}

/// The Slicer tab's row named `name`, its scroll walked until it shows: a row out of the
/// panel's clip is not in the tree.
fn reveal(app: &TestApp, name: &str) {
    wait(app, &format!("the Slicer tab's {name:?}"), || {
        let nodes = app.widget_snapshot().nodes();
        if nodes.iter().any(|n| n.node.name == name && n.visible()) {
            return Some(());
        }
        let panel = app.locator(Selector::widget_type("FabSlicerPanel")).wait_visible().snapshot().bounds;
        let bar = nodes.into_iter().find(|n| { let (x, y) = n.bounds.center(); n.node.role == Role::ScrollBar && n.visible() && panel.contains(x, y) })?;
        let range = bar.range.expect("the scroll bar's range");
        let next = if range.now >= range.max { 0.0 } else { (range.now + f64::from(panel.h) * 0.5).min(range.max) };
        app.locator(Selector::raw(format!("id={}", bar.id.short()))).set_value(next);
        None
    });
}

#[makepad_test]
fn edits_in_sliced_commit_and_plan_again_once_a_plan_is_out(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait(&app, "the fab tools", || call(&app, "inspect", "{}").ok());
    let primary = if cfg!(target_os = "macos") { KeyModifiers { shift: true, logo: true, ..Default::default() } } else { KeyModifiers { shift: true, control: true, ..Default::default() } };
    app.press_key_with_modifiers(KeyCode::KeyO, primary);
    wait(&app, "the demo plate's three parts", || {
        inspect(&app).field("plate").and_then(|p| p.value.field("parts")).and_then(|f| f.value.items()).is_some_and(|p| p.len() == 3).then_some(())
    });
    // The user's plate: trees, only the lattice cube printing.
    let edits = Value::Str([
        r#"settings({enable_support: "1" support_type: "tree(auto)" fab_tree_outside_footprint: "1" support_object_xy_distance: "2" support_threshold_angle: "46"})"#,
        r#"place("bracket", print: false)"#,
        r#"place("vase", print: false, settings: {support_on_build_plate_only: "1" support_threshold_angle: "35" support_type: "coral(auto)"})"#,
        r#"place("lattice_cube", settings: {fab_tree_outside_footprint: "0" sparse_infill_density: "100%" support_on_build_plate_only: "1"})"#,
    ].join("\n")).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"user-plate","expected":"{}","edits":{edits}}}"#, head(&app))).expect("the user's plate");

    // The support settings at hand; then Sliced, and its first plan out.
    app.locator(Selector::role(Role::Tab).name("Slicer")).click();
    const ANGLE: &str = "Support and brim Overhang angle";
    let angle = || app.locator(Selector::role(Role::SpinButton).name(ANGLE));
    reveal(&app, ANGLE);
    // The head before any plan, while nothing waits on the model worker.
    let was = head(&app);
    let from = seq(&app);
    render_mode::pick(&app, "Sliced");
    let first = planned(&app, from, "the first plan");

    // At once, while what the first plan's supports tell is still measured (the sliced checks,
    // which read it, are not whole yet) and the overhang list waits for it.
    let from = seq(&app);
    let measuring = !lines(&app, 0).iter().any(|l| field(l, "fab live checked: revision=") == Some(first));
    reveal(&app, ANGLE);
    angle().set_value(45);
    let r = planned(&app, from, "the plan of an edit made while the report is measured");
    assert!(r > first, "an edit while the report is measured: planned revision {r}, not one after {first}");
    assert_ne!(head(&app), was, "an edit while the report is measured planned again but did not commit");
    assert!(measuring, "the first plan's supports' report was in before the edit: no edit was made while it was measured");
    println!("[edits-in-sliced] an edit while the report is measured: committed, revision {r} planned");

    let r = commits_and_plans_again(&app, "the footprint setting", r, || {
        app.locator(Selector::role(Role::CheckBox).text_contains("Only outside the part")).wait_visible().click();
    });
    let r = commits_and_plans_again(&app, "the overhang angle", r, || {
        reveal(&app, ANGLE);
        angle().set_value(44);
    });
    commits_and_plans_again(&app, "a no-trees box", r, || {
        app.locator(Selector::id("t_no_trees")).wait_visible().click();
        app.locator(Selector::id("viewport")).wait_visible().drag_by_and_hold(80.0, 40.0);
    });
}

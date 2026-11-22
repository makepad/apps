//! Print settings here, by hand in the Sliced view on a 20 mm block placed with two part-wide
//! settings, then a plate setting equal to one of them (kept: never dropped unasked, it reads
//! "same as global: no effect"): the Overrides editor's list names the plate's and the
//! block's two and, once drawn, the box (the Parts list lists the block alone). The rail's tool,
//! dragged across the shown layer, writes a box the block's script marks with `m.settings` (a
//! solid infill to start), picks it and slices again. One pick lights one row: with the box
//! picked, a click on the block's supports row lights that row alone, types nothing into the
//! search and shows the block's settings with its Supports field in view, marked, with its
//! remove, and editable; the plate's row shows the global settings; the box's row the box's:
//! its size and place under the list, then its settings, its density marked as its own and the
//! rest as the block's. The box's Density field writes the statement alone and slices again,
//! with no script error; an inherited Wall loops set there is written in and its remove takes
//! it out; the box is a print settings feature of the block in the Features tree and takes a
//! name there; a row's delete puts a setting back on what it inherits or takes the box away;
//! and undo takes each back, one step each. The view's
//! right-click menu and the Parts tab's Add menu each put one volume over the whole block, one
//! undo step.

mod common;

use makepad_script::syntax::{self, Kind, Node};
use makepad_strict_json::Value;
use makepad_test::{makepad_test, KeyCode, NodeMatch, RemoteMouseDown, RemoteMouseUp, Role, Selector, States, StudioToApp, TestApp};
use std::time::{Duration, Instant};

const BLOCK: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let block = m.box({size: [20mm 20mm 20mm]})
object(block)
"##;

/// The block's placement: two settings of its own; then the plate's supports, the same as the
/// block's (which stays: the block's reads "same as global: no effect").
const PLACE: &str = "place(\"block\", at: [175mm 160mm], name: \"Block\", settings: {enable_support: \"1\" sparse_infill_density: \"30%\"})\nsettings({enable_support: \"1\"})";

/// Only a hang fails a wait: a cold or loaded box gets there late, not never. It is the main
/// thread's CPU a wait may spend; an app that sits idle with nothing pending fails the wait in
/// seconds ([`try_wait`]).
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

/// The answer lines of a command to the app's remote.
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

/// The script errors the app has filed: its error events and its error sites.
fn script_errors(app: &TestApp) -> Vec<String> {
    let mut out: Vec<String> = ask(app, &["events", "since=0", "on=scripterr"]).into_iter().filter(|l| l.contains("scripterr")).collect();
    out.extend(ask(app, &["script", "errors"]).into_iter().filter(|l| l.contains("\"text\"")));
    out
}

fn inspect(app: &TestApp) -> syntax::Node {
    syntax::parse_value(&call(app, "inspect", "{}").expect("inspect")).expect("a Splash reply")
}

fn head(app: &TestApp) -> String {
    inspect(app).field("head").and_then(|f| f.value.string()).expect("a head").to_string()
}

fn head_of(reply: &str) -> String {
    syntax::parse_value(reply).expect("a Splash reply").field("head").and_then(|f| f.value.string()).expect("a head").to_string()
}

/// The block's script as the app holds it.
fn source(app: &TestApp) -> String {
    let v = inspect(app);
    let scripts = v.field("scripts").and_then(|f| f.value.items()).expect("the scripts");
    let block = scripts.iter().find(|s| s.field("file").and_then(|f| f.value.string()) == Some("block.cad.splash")).expect("the block's script");
    block.field("source").and_then(|f| f.value.string()).expect("its source").to_string()
}

fn project(app: &TestApp) -> String {
    inspect(app).field("project").and_then(|f| f.value.string()).expect("the project's source").to_string()
}

/// The `settings: {..}` argument of the `place(.., name: <name>)` call.
fn place_settings(source: &str, name: &str) -> Option<Node> {
    let tree = syntax::parse(source).expect("the project is Splash");
    for loc in tree.find_calls("place") {
        let args = tree.node(&loc).expect("place node").call("place").expect("place arguments");
        let named = args[1..].iter().any(|arg| matches!(&arg.kind, Kind::Entry { name: n, value, .. } if n == "name" && value.string() == Some(name)));
        if named {
            return args[1..].iter().find_map(|arg| match &arg.kind {
                Kind::Entry { name, value, .. } if name == "settings" => Some((**value).clone()),
                _ => None,
            });
        }
    }
    panic!("no place(.., name: {name:?}) call: {source}");
}

/// The block's own value for `key`.
fn block_setting(app: &TestApp, key: &str) -> Option<String> {
    place_settings(&project(app), "Block")?.field(key)?.value.string().map(str::to_string)
}

/// The shown nodes of `role`, by name.
fn shown(app: &TestApp, role: Role) -> Vec<NodeMatch> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == role).collect()
}

/// The Overrides editor's rows (the list's own, in its bounds: a row is a part of the list's
/// node, without a path of its own).
fn override_items(app: &TestApp) -> Vec<NodeMatch> {
    let Some(list) = shown(app, Role::List).into_iter().find(|w| w.node.name == "Overrides") else { return Vec::new() };
    shown(app, Role::ListItem).into_iter().filter(|w| { let (x, y) = w.bounds.center(); list.bounds.contains(x, y) }).collect()
}

/// The Overrides rows: what each says, whose it is and whether it is picked.
fn override_rows(app: &TestApp) -> Vec<(String, String, bool)> {
    override_items(app).into_iter().map(|w| (w.node.name.to_string(), w.node.description.to_string(), w.node.states.contains(States::SELECTED))).collect()
}

/// The Parts list's rows (the Features tree's are list items): name and the line under it.
fn tree_rows(app: &TestApp) -> Vec<(String, String)> {
    shown(app, Role::TreeItem).into_iter().map(|w| (w.node.name.to_string(), w.node.description.to_string())).collect()
}

/// A print settings box's row in the Features tree: its kind, not its shape, and no eye.
const BOX_FEATURE: &str = "base print settings settings1 level 1";

/// The Parts list once it shows the block: the block alone, no row or count of its overrides
/// or boxes.
fn parts_rows(app: &TestApp) -> Vec<(String, String)> {
    let rows = wait(app, "the block in the Parts list", || Some(tree_rows(app)).filter(|t| t.iter().any(|(n, _)| n == "Block")));
    assert!(rows.len() == 1 && !rows[0].1.contains("setting"), "the Parts list lists the block alone: {rows:?}");
    rows
}

/// The Overrides row whose text starts with `what`, once it shows.
fn override_row(app: &TestApp, what: &str) -> (String, String, bool) {
    app.locator(Selector::role(Role::Tab).name("Overrides")).click();
    try_wait(app, &format!("the Overrides row {what:?}"), || override_rows(app).into_iter().find(|(name, _, _)| name.starts_with(what))).unwrap_or_else(|why| {
        // what shows instead: the lists, their rows and the Overrides editor's widgets
        for w in app.widget_snapshot().nodes().into_iter().filter(|w| matches!(w.node.role, Role::List | Role::ListItem) || w.node.path.contains("volumes")) {
            println!("[settings-here] {:?} {:?} {:?} visible={} bounds={:?} path={}", w.node.role, w.node.name, w.node.description, w.visible(), w.bounds, w.node.path);
        }
        println!("[settings-here] the project:\n{}", project(app));
        panic!("timed out {why}");
    })
}

/// The delete button of the Overrides row whose text starts with `what`.
fn delete_override(app: &TestApp, what: &str) {
    let row = override_items(app).into_iter().find(|w| w.node.name.starts_with(what)).expect("the row");
    let button = shown(app, Role::Button).into_iter()
        .find(|w| w.node.name == "Delete override" && { let (x, y) = w.bounds.center(); row.bounds.contains(x, y) })
        .unwrap_or_else(|| panic!("no delete button on the row {what:?}"));
    app.locator(Selector::raw(format!("id={}", button.id.short()))).press();
}

/// The picked box's Density field, in the Slicer's rows: its path.
fn box_density(app: &TestApp) -> Option<String> {
    shown(app, Role::StaticText).into_iter().find(|w| w.node.name.starts_with("Print settings box 1, on "))?;
    let fields: Vec<NodeMatch> = shown(app, Role::SpinButton).into_iter().filter(|w| w.node.name.ends_with("Density")).collect();
    match fields.as_slice() {
        [one] => Some(one.node.path.to_string()),
        _ => None,
    }
}

/// The shown fields of the picked box's size and place, under the Overrides list.
fn box_fields(app: &TestApp) -> Vec<String> {
    shown(app, Role::SpinButton).into_iter().filter(|w| w.node.path.split('.').any(|s| s == "volumes")).map(|w| w.node.name.to_string()).collect()
}

/// Whether a setting's field shows its remove (it is an override where the panel edits).
fn removable(app: &TestApp, name: &str) -> bool {
    shown(app, Role::Button).into_iter().any(|w| w.node.name == format!("Remove override: {name}"))
}

/// The lit rows: the Overrides list's, the Parts list's.
fn lit(app: &TestApp) -> (Vec<String>, Vec<String>) {
    let list = override_rows(app).into_iter().filter(|r| r.2).map(|r| r.0).collect();
    let tree = shown(app, Role::TreeItem).into_iter().filter(|w| w.node.states.contains(States::SELECTED)).map(|w| w.node.name.to_string()).collect();
    (list, tree)
}

/// The Slicer panel's search text.
fn search_text(app: &TestApp) -> String {
    shown(app, Role::TextField).into_iter().find(|w| w.node.path.contains("search")).map(|w| w.node.value.to_string()).expect("the Slicer panel's search")
}

/// A setting's field, shown (scrolled into view: a row out of the panel's clip is not in the
/// semantic tree) and not disabled.
fn field_in_view(app: &TestApp, role: Role, name: &str) -> Option<NodeMatch> {
    shown(app, role).into_iter().find(|w| w.node.name == name && !w.node.states.contains(States::DISABLED))
}

/// [`field_in_view`] once it is; else what the panel shows, and the wait's failure.
fn wait_field_in_view(app: &TestApp, what: &str, role: Role, name: &str) -> NodeMatch {
    try_wait(app, what, || field_in_view(app, role, name)).unwrap_or_else(|why| {
        for w in app.widget_snapshot().nodes().into_iter().filter(|w| w.node.role == role && w.node.name.contains(name.rsplit(' ').next().unwrap_or(name))) {
            println!("[settings-here] {:?} {:?} visible={} bounds={:?} states={:?} path={}", w.node.role, w.node.name, w.visible(), w.bounds, w.node.states, w.node.path);
        }
        panic!("timed out {why}");
    })
}

/// Click an Overrides row and wait until it is the one row lit there; then the lit rows (the
/// Parts list's lights the part the pick selects, if any).
fn pick_row(app: &TestApp, what: &str) -> (Vec<String>, Vec<String>) {
    let (row, _, _) = override_row(app, what);
    app.locator(Selector::role(Role::ListItem).name(row.as_str())).press();
    wait(app, &format!("the row {what:?} alone to be lit"), || Some(lit(app).0).filter(|l| *l == [row.clone()]));
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    let both = lit(app);
    app.locator(Selector::role(Role::Tab).name("Slicer")).click();
    both
}

fn seq(app: &TestApp) -> u64 {
    app.log_since(0).map(|t| t.last_seq).unwrap_or(0)
}

fn lines(app: &TestApp, from: u64) -> Vec<String> {
    app.log_since(from).map(|t| t.lines).unwrap_or_default()
}

/// The number after `name` in `line`.
fn field(line: &str, name: &str) -> Option<u64> {
    let at = line.find(name)? + name.len();
    let digits: String = line[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// `f`'s answer, once it has one; else what was waited for and why the wait ended: the app's
/// main thread spent [`HANG`] on it, or the app sat idle with nothing pending (the runtime's
/// rule, its action timeout), so a wait on an idle app fails in seconds, not minutes.
fn try_wait<T>(app: &TestApp, what: &str, mut f: impl FnMut() -> Option<T>) -> Result<T, String> {
    let mut got = None;
    app.try_wait_until_within(HANG, || {
        got = f();
        got.is_none().then(|| format!("waiting for {what}"))
    }).map_err(|e| e.message().to_string())?;
    Ok(got.expect("the wait ended on an answer"))
}

fn wait<T>(app: &TestApp, what: &str, f: impl FnMut() -> Option<T>) -> T {
    try_wait(app, what, f).unwrap_or_else(|why| {
        for l in lines(app, 0).iter().filter(|l| l.contains("fab")).rev().take(40).collect::<Vec<_>>().into_iter().rev() {
            println!("[app] {l}");
        }
        println!("[settings-here] grab {}", app.screenshot().display());
        panic!("timed out {why}");
    })
}

/// The slice's newest input since log line `from`, once its toolpaths are out: its revision.
fn planned(app: &TestApp, from: u64, what: &str) -> u64 {
    wait(app, what, || {
        let lines = lines(app, from);
        let newest = lines.iter().filter_map(|l| field(l, "fab live input: revision=")).last()?;
        lines.iter().any(|l| l.contains("fab live work:") && field(l, "revision=") == Some(newest)).then_some(newest)
    })
}

/// An edit made by `edit` writes the script to what `done` accepts and plans again (a newer
/// revision than `after`'s): the script and the revision.
fn edits_and_plans_again(app: &TestApp, what: &str, after: u64, edit: impl FnOnce(), done: impl Fn(&str) -> bool) -> (String, u64) {
    let from = seq(app);
    edit();
    let script = wait(app, &format!("{what} to write the script"), || Some(source(app)).filter(|s| done(s)));
    let revision = planned(app, from, &format!("{what}'s plan"));
    assert!(revision > after, "{what}: planned revision {revision}, not one after {after}");
    println!("[settings-here] {what}: written, revision {revision} planned");
    (script, revision)
}

fn undo(app: &TestApp) {
    app.locator(Selector::id("undo").nth(0)).press();
}

/// A right-click (the secondary button, down and up) at the middle of the 3D view.
fn right_click_view(app: &TestApp) {
    let r = app.locator(Selector::id("viewport")).wait_visible().snapshot().bounds;
    let (x, y) = (f64::from(r.x + 0.5 * r.w), f64::from(r.y + 0.5 * r.h));
    let secondary = 1 << 1;
    app.forward(vec![
        StudioToApp::MouseDown(RemoteMouseDown { button_raw_bits: secondary, x, y, ..Default::default() }),
        StudioToApp::MouseUp(RemoteMouseUp { button_raw_bits: secondary, x, y, ..Default::default() }),
    ]);
}

/// The volume a menu's "Print settings here" puts over the whole block: a box 1 mm past its
/// sides and top, alone after the block's lines, a solid infill to start.
fn over_the_block(s: &str) -> bool {
    s.strip_prefix(BLOCK).is_some_and(|added| {
        added.starts_with("let settings1 = m.box({size: [22mm, 21mm, 22mm]") && added.matches("m.settings(").count() == 1
            && added.ends_with("\nm.settings(settings1, {sparse_infill_density: \"100%\"})\n") && added.lines().count() == 2
    })
}

#[makepad_test]
fn the_overrides_rows_name_each_override_and_a_drawn_volume_edits_and_deletes_one_undo_step_each(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait(&app, "the fab tools", || call(&app, "inspect", "{}").ok());
    let batch = format!(r#"{{"request_id":"block","expected":"{}","script":"block.cad.splash","source":{}}}"#, head(&app), Value::Str(BLOCK.to_string()).to_json());
    let h = head_of(&call(&app, "script", &batch).expect("script"));
    let edits = Value::Str(PLACE.to_string()).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"place","expected":"{h}","edits":{edits}}}"#)).expect("place");
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).name("Block")).wait_visible().click();
    frame(&app);

    let from = seq(&app);
    app.locator(Selector::id("rm_slice")).wait_visible().click();
    app.locator(Selector::id("rm_slice")).wait_checked(true);
    let first = planned(&app, from, "the first plan");
    wait(&app, "the slice's layers", || (common::slice_state(&app) == "Sliced").then_some(()));
    let plain = source(&app);
    assert_eq!(plain, BLOCK, "the block's script before the tool");

    // The block's own settings, each against what it inherits, and the plate's: in the
    // Overrides editor; the Parts list lists the block alone. (The block's supports, the same as
    // the plate's once the plate's came, stay: no effect, listed with its delete.)
    assert_eq!(block_setting(&app, "enable_support").as_deref(), Some("1"), "a setting the plate comes to equal is never dropped unasked:\n{}", project(&app));
    assert_eq!(block_setting(&app, "sparse_infill_density").as_deref(), Some("30%"));
    let (_, whose, _) = override_row(&app, "Density 30 % (global ");
    assert_eq!(whose, "Block", "the block's density override is the block's");
    let (_, whose, _) = override_row(&app, "Supports on (same as global: no effect)");
    assert_eq!(whose, "Block", "the block's supports are the block's");
    let (_, whose, _) = override_row(&app, "Supports on (profile ");
    assert_eq!(whose, "All objects", "the plate's supports setting is every object's");
    let rows = override_rows(&app);
    assert_eq!(rows.len(), 3, "the Overrides rows: {rows:?}");
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    let tree = parts_rows(&app);
    println!("[settings-here] Overrides: {rows:?}; Parts: {tree:?}");

    // A setting's row picks it: the row is picked, its object selected.
    app.locator(Selector::role(Role::Tab).name("Overrides")).click();
    let (density_row, _, _) = override_row(&app, "Density 30 % (global ");
    app.locator(Selector::role(Role::ListItem).name(density_row.as_str())).press();
    wait(&app, "the density row to be picked", || override_row(&app, "Density 30 % (global ").2.then_some(()));

    // The rail's tool, a drag across the shown layer over the block.
    let errors = script_errors(&app);
    let (drawn, r) = edits_and_plans_again(&app, "the drawn volume", first, || {
        app.locator(Selector::id("t_settings_here")).wait_visible().click();
        app.locator(Selector::id("viewport")).wait_visible().drag_by_and_hold(80.0, 40.0);
    }, |s| s.contains("m.settings(settings1, {sparse_infill_density: \"100%\"})"));
    assert!(drawn.starts_with(BLOCK), "the volume goes after the block's own lines:\n{drawn}");
    assert!(drawn[BLOCK.len()..].starts_with("let settings1 = m.box({size: ["), "the volume is a box:\n{drawn}");

    // The volume is picked: its row in the Overrides, the one lit (the density row lit before
    // it is not), its Density field at 100 %.
    let (box_row, whose, picked) = wait(&app, "the box's row, picked", || Some(override_row(&app, "Print settings box 1: Density 100 %")).filter(|r| r.2));
    assert!(picked && whose == "Block", "the box's row is the block's and picked");
    assert_eq!(lit(&app).0, [box_row.clone()], "one row lit");
    let path = try_wait(&app, "the box's Density field", || box_density(&app)).unwrap_or_else(|why| {
        for w in app.widget_snapshot().nodes().into_iter().filter(|w| w.node.role == Role::SpinButton || w.node.path.contains("volumes")) {
            println!("[settings-here] {:?} {:?} visible={} path={}", w.node.role, w.node.name, w.visible(), w.node.path);
        }
        panic!("the drawn volume's Density field never showed in the Slicer tab: {why}");
    });
    let density = || app.locator(Selector::path(path.as_str()));
    let now = density().snapshot().range.expect("the density's range").now;
    assert!((now - 100.0).abs() < 1e-6, "the drawn volume's density shows {now}, not 100 %");
    // The panel is the box's: its title says so, its size and place under the list, then the
    // settings a box sets in their groups, its density marked as its own (with its remove) and
    // the rest the block's; the block's other settings, which would change the whole block,
    // are not there.
    wait(&app, "the panel's title to name the box", || shown(&app, Role::StaticText).into_iter().find(|w| w.node.name == "Print settings box 1, on Block"));
    let densities: Vec<String> = shown(&app, Role::SpinButton).into_iter().filter(|w| w.node.name.ends_with("Density")).map(|w| w.node.path.to_string()).collect();
    assert_eq!(densities, [path.clone()], "only the box's Density shows while it is picked");
    let fields = box_fields(&app);
    assert!(fields.iter().any(|n| n == "Size X") && fields.iter().any(|n| n == "Position X") && !fields.iter().any(|n| n.ends_with("Density")), "the box's own fields: {fields:?}");
    assert!(removable(&app, "Infill Density"), "the box's density is its own");
    for inherited in ["Infill Pattern", "Walls Wall loops"] {
        assert!(!removable(&app, inherited), "{inherited} is the block's, not the box's");
    }
    assert!(shown(&app, Role::ComboBox).iter().any(|w| w.node.name == "Infill Pattern"), "the box's pattern row shows");
    assert!(!shown(&app, Role::CheckBox).iter().any(|w| w.node.name == "Support and brim Supports"), "a box sets no supports");

    // The box picked, then the block's supports row clicked: that row alone is lit (the Parts
    // list lights the block it selects); nothing is typed into the search; the panel is the
    // block's, its Supports field in view, marked, with its remove; no box fields.
    let (list, tree) = pick_row(&app, "Supports on (same as global: no effect)");
    assert_eq!(tree, ["Block"], "the Parts list lights the block alone (list: {list:?})");
    assert_eq!(search_text(&app), "", "a row's click types nothing into the search");
    let supports = wait_field_in_view(&app, "the Supports field in view", Role::CheckBox, "Support and brim Supports");
    assert!(removable(&app, "Support and brim Supports"), "the block's supports field is marked as its own");
    assert!(box_fields(&app).is_empty(), "no box fields with the box unpicked: {:?}", box_fields(&app));
    wait(&app, "the panel's title to name the block", || shown(&app, Role::StaticText).into_iter().find(|w| w.node.name == "Block"));
    // ... and it edits: off, then undo
    app.locator(Selector::raw(format!("id={}", supports.id.short()))).click();
    wait(&app, "the Supports field to turn the block's supports off", || (block_setting(&app, "enable_support").as_deref() == Some("0")).then_some(()));
    undo(&app);
    wait(&app, "undo to turn them back on", || (block_setting(&app, "enable_support").as_deref() == Some("1")).then_some(()));
    // The plate's row: the global settings, its Supports field in view; no box fields.
    let (_, tree) = pick_row(&app, "Supports on (profile ");
    assert!(tree.is_empty(), "the plate's row selects no part; none lit: {tree:?}");
    wait_field_in_view(&app, "the plate's Supports field in view", Role::CheckBox, "Support and brim Supports");
    assert!(removable(&app, "Support and brim Supports"), "the plate's supports field is marked as the plate's");
    wait(&app, "the panel's title to say all objects", || shown(&app, Role::StaticText).into_iter().find(|w| w.node.name == "All objects"));
    assert!(box_fields(&app).is_empty(), "nothing picked: no box fields");
    assert_eq!(search_text(&app), "", "a row's click types nothing into the search");
    // The box's row: the box's panel again, it alone lit.
    let (_, tree) = pick_row(&app, "Print settings box 1: Density 100 %");
    assert_eq!(tree, ["Block"], "the Parts list lights the box's block alone");
    let path = wait(&app, "the box's Density field again", || box_density(&app));
    let density = || app.locator(Selector::path(path.as_str()));

    let (half, _) = edits_and_plans_again(&app, "the box's Density field", r, || {
        density().set_value(50);
    }, |s| s.contains("m.settings(settings1, {sparse_infill_density: \"50%\"})"));
    assert_eq!(half, drawn.replace("\"100%\"", "\"50%\""), "the box's Density field writes its value alone");
    assert_eq!(block_setting(&app, "sparse_infill_density").as_deref(), Some("30%"), "the box's field leaves the block's own density alone");
    // Its pattern row went dim (a solid infill's pattern is not in effect) and back: no script
    // error on the way.
    let filed: Vec<String> = script_errors(&app).into_iter().filter(|e| !errors.contains(e)).collect();
    assert!(filed.is_empty(), "the box's rows filed script errors: {filed:?}");
    override_row(&app, "Print settings box 1: Density 50 %");
    // A setting the box inherits, set there, is written in; its remove takes it out; one undo
    // step each.
    let walls = shown(&app, Role::SpinButton).into_iter().find(|w| w.node.name == "Walls Wall loops").map(|w| w.node.path.to_string()).expect("the box's Wall loops");
    let walled = half.replace("\"50%\"})", "\"50%\" wall_loops: 7})");
    edits_and_plans_again(&app, "the box's Wall loops field", r, || {
        app.locator(Selector::path(walls.as_str())).set_value(7);
    }, |s| s == walled);
    wait(&app, "the box's Wall loops to be marked its own", || removable(&app, "Walls Wall loops").then_some(()));
    app.locator(Selector::role(Role::Button).name("Remove override: Walls Wall loops")).press();
    wait(&app, "the remove to take the box's wall loops out", || (source(&app) == half).then_some(()));
    undo(&app);
    wait(&app, "undo to put them back", || (source(&app) == walled).then_some(()));
    undo(&app);
    wait(&app, "undo to take them out", || (source(&app) == half).then_some(()));
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    parts_rows(&app);

    // The box is the block's print settings feature in the Features tree, picked there as in
    // the Overrides list; it takes a name there: its binding and its uses, one undo step.
    app.locator(Selector::role(Role::Tab).name("Features")).click();
    let row = app.locator(Selector::role(Role::ListItem).name(BOX_FEATURE)).wait_visible().snapshot();
    assert!(row.node.states.contains(States::SELECTED), "the picked box's Features row is not picked");
    app.locator(Selector::role(Role::ListItem).name(BOX_FEATURE)).select();
    app.press_key(KeyCode::F2);
    app.locator(Selector::role(Role::TextField).name("Feature name")).wait_visible().fill("neck_core").press_key(KeyCode::ReturnKey);
    let named = wait(&app, "the box's name to be written", || Some(source(&app)).filter(|s| s.contains("let neck_core = m.box(")));
    assert_eq!(named, half.replace("settings1", "neck_core"), "the name takes the binding and its uses alone");
    override_row(&app, "neck core: Density 50 %");
    undo(&app);
    wait(&app, "undo to take the name back", || (source(&app) == half).then_some(()));
    println!("[settings-here] the box took a name and undo took it back");

    // One undo step each: the density, then the volume.
    undo(&app);
    wait(&app, "undo to take the density back", || (source(&app) == drawn).then_some(()));
    undo(&app);
    wait(&app, "undo to take the volume back", || (source(&app) == plain).then_some(()));
    println!("[settings-here] undo took the density and then the volume back");

    // A row's delete: a box goes, a setting goes back to what it inherits; one undo step each.
    let (again, r) = edits_and_plans_again(&app, "a volume again", r, || {
        right_click_view(&app);
        app.locator(Selector::role(Role::MenuItem).name("Print settings here")).wait_visible().press();
    }, over_the_block);
    override_row(&app, "Print settings box 1: Density 100 %");
    delete_override(&app, "Print settings box 1");
    wait(&app, "the box's delete to take it away", || (source(&app) == plain).then_some(()));
    wait(&app, "its row to go", || (!override_rows(&app).iter().any(|(n, _, _)| n.starts_with("Print settings box 1"))).then_some(()));
    undo(&app);
    wait(&app, "one undo to bring the box back", || (source(&app) == again).then_some(()));
    undo(&app);
    wait(&app, "undo to take the volume back", || (source(&app) == plain).then_some(()));
    override_row(&app, "Density 30 % (global ");
    delete_override(&app, "Density 30 % (global ");
    wait(&app, "the density row's delete to put the block on the plate's", || block_setting(&app, "sparse_infill_density").is_none().then_some(()));
    let rows = wait(&app, "the density row to go", || Some(override_rows(&app)).filter(|rows| rows.len() == 2));
    assert!(!rows.iter().any(|r| r.0.starts_with("Density")), "a row's delete takes only its own setting: {rows:?}");
    undo(&app);
    wait(&app, "one undo to bring the density override back", || (block_setting(&app, "sparse_infill_density").as_deref() == Some("30%")).then_some(()));
    override_row(&app, "Density 30 % (global ");
    // the setting with no effect: its delete takes it, one undo brings it back
    delete_override(&app, "Supports on (same as global: no effect)");
    wait(&app, "the no-effect row's delete to take the block's supports", || block_setting(&app, "enable_support").is_none().then_some(()));
    undo(&app);
    wait(&app, "one undo to bring them back", || (block_setting(&app, "enable_support").as_deref() == Some("1")).then_some(()));
    println!("[settings-here] each row's delete took one undo step to bring back");

    // The view's right-click menu on the block: one volume over the whole of it, picked.
    let (_, r) = edits_and_plans_again(&app, "the right-click menu's volume", r, || {
        right_click_view(&app);
        app.locator(Selector::role(Role::MenuItem).name("Print settings here")).wait_visible().press();
    }, over_the_block);
    app.locator(Selector::role(Role::Tab).name("Slicer")).click();
    if let Err(why) = try_wait(&app, "the right-click menu's volume's Density field", || box_density(&app)) {
        panic!("the right-click menu's volume is not picked: {why}");
    }
    undo(&app);
    wait(&app, "one undo to take the right-click menu's volume back", || (source(&app) == plain).then_some(()));

    // The Parts tab's Add menu with the block picked: the same, one step.
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).name("Block")).wait_visible().click();
    edits_and_plans_again(&app, "the Add menu's volume", r, || {
        app.locator(Selector::role(Role::Button).name("Add")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name("Print settings here")).wait_visible().press();
    }, over_the_block);
    undo(&app);
    wait(&app, "one undo to take the Add menu's volume back", || (source(&app) == plain).then_some(()));
    println!("[settings-here] the right-click and Add menus each put one volume over the block, one undo step each");
}

/// One mouse event of `button` (0 the primary, 1 the secondary) at `(x, y)` through the app's
/// real event path, answered once a frame has drawn it.
fn mouse(app: &TestApp, kind: &str, x: f64, y: f64, button: u32) {
    ask(app, &["mouse", &format!("k={kind}"), &format!("x={x:.1}"), &format!("y={y:.1}"), &format!("b={button}"), "wait=1"]);
}

/// The middle of a node.
fn middle(w: &NodeMatch) -> (f64, f64) {
    let (x, y) = w.bounds.center();
    (f64::from(x), f64::from(y))
}

/// The middle of the grip of `role` named `name` once it stays put for two reads (the camera,
/// the view and the panels settled).
fn steady(app: &TestApp, role: Role, name: &str) -> (f64, f64) {
    let mut last = None;
    wait(app, &format!("the {name} to stay put"), || {
        let now = shown(app, role).into_iter().find(|w| w.node.name == name).map(|w| middle(&w));
        let still = now.is_some() && now == last;
        last = now;
        if !still {
            std::thread::sleep(Duration::from_millis(100));
        }
        now.filter(|_| still)
    })
}

/// The print settings box the view draws: its node (its screen rect, its size on the plate as
/// drawn in its description); none while none is drawn.
fn drawn_box(app: &TestApp) -> Option<NodeMatch> {
    shown(app, Role::Image).into_iter().find(|w| w.node.name.starts_with("Drawn print settings box "))
}

/// The drawn box on the plate (mm): its size along the plate's X, Y and Z and its middle, from
/// its node's "w x d x h mm at x, y, z".
fn drawn_on_plate(w: &NodeMatch) -> ([f64; 3], [f64; 3]) {
    let text = w.node.description.to_string();
    let nums = |s: &str, by: &str| -> Option<[f64; 3]> {
        let v: Vec<f64> = s.split(by).map(|n| n.trim().parse().ok()).collect::<Option<_>>()?;
        v.try_into().ok()
    };
    text.split_once(" mm at ").and_then(|(size, at)| Some((nums(size, " x ")?, nums(at, ",")?))).unwrap_or_else(|| panic!("the drawn box's node says {text:?}"))
}

/// The drawn box's height on the plate (mm).
fn drawn_height(w: &NodeMatch) -> f64 {
    drawn_on_plate(w).0[2]
}

/// The picked box's Size Z field in the Overrides editor: its path.
fn box_size_z(app: &TestApp) -> Option<String> {
    shown(app, Role::SpinButton).into_iter().find(|w| w.node.name == "Size Z" && w.node.path.split('.').any(|s| s == "volumes")).map(|w| w.node.path.to_string())
}

fn number(app: &TestApp, path: &str) -> f64 {
    app.locator(Selector::path(path)).snapshot().range.expect("a number's range").now
}

/// The box's height in the view against its Size Z field (the plate's Z, up), in the frame the
/// app answers in.
fn drawn_matches_field(app: &TestApp, path: &str, what: &str) -> f64 {
    let field = number(app, path);
    let drawn = drawn_box(app).unwrap_or_else(|| panic!("{what}: no box drawn"));
    let height = drawn_height(&drawn);
    assert!((height - field).abs() < 0.06, "{what}: the drawn box is {height} mm high ({}), its Size Z field says {field} mm", drawn.node.description);
    field
}

/// A point inside the drawn box's screen rect clear of every grip in the view (the box's
/// handles, a gizmo's), the nearest the view's middle (the framed part's).
fn on_the_box_off_its_grips(app: &TestApp) -> (f64, f64) {
    let r = drawn_box(app).expect("the box drawn").bounds;
    let view = app.locator(Selector::id("viewport")).snapshot().bounds;
    let (mx, my) = view.center();
    let grips: Vec<(f32, f32)> = shown(app, Role::Slider).into_iter().chain(shown(app, Role::Button)).filter(|w| view.contains(w.bounds.center().0, w.bounds.center().1)).map(|w| w.bounds.center()).collect();
    let clear = |(x, y): (f32, f32)| grips.iter().all(|g| (g.0 - x).hypot(g.1 - y) > 24.0);
    let points = (1..10).flat_map(|i| (1..10).map(move |j| (r.x + r.w * i as f32 / 10.0, r.y + r.h * j as f32 / 10.0)));
    let (x, y) = points.filter(|p| clear(*p)).min_by(|a, b| (a.0 - mx).hypot(a.1 - my).total_cmp(&(b.0 - mx).hypot(b.1 - my))).unwrap_or_else(|| panic!("no point of the drawn box {r:?} clear of the grips {grips:?}"));
    (f64::from(x), f64::from(y))
}

fn selected_tree_rows(app: &TestApp) -> Vec<String> {
    shown(app, Role::TreeItem).into_iter().filter(|w| w.node.states.contains(States::SELECTED)).map(|w| w.node.name.to_string()).collect()
}

/// The block's placement line in the project.
fn block_place(app: &TestApp) -> String {
    project(app).lines().find(|l| l.contains("name: \"Block\"")).expect("the block's place line").to_string()
}

/// The box in the Normal view: with no part picked the rail's tool and Add box are off; with
/// the part picked the tool draws the box on it there and stays there, the box picked and drawn; its Size Z field, scrubbed or typed, sizes the drawn box in the frame it changes
/// (one undo step a scrub); the secondary button's drag over its handle is the camera's, the
/// primary one sizes it; a click on the model through it picks the part, and the box goes from
/// the view; the Scale tool then scales the part, not the box; nothing picked, no box.
#[makepad_test]
fn a_box_is_drawn_while_picked_follows_its_fields_and_a_click_on_the_model_picks_the_part(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait(&app, "the fab tools", || call(&app, "inspect", "{}").ok());
    let batch = format!(r#"{{"request_id":"block","expected":"{}","script":"block.cad.splash","source":{}}}"#, head(&app), Value::Str(BLOCK.to_string()).to_json());
    let h = head_of(&call(&app, "script", &batch).expect("script"));
    let edits = Value::Str("place(\"block\", at: [175mm 160mm], name: \"Block\")".to_string()).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"place","expected":"{h}","edits":{edits}}}"#)).expect("place");
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).name("Block")).wait_visible();
    wait(&app, "nothing picked", || selected_tree_rows(&app).is_empty().then_some(()));
    // A box goes on the picked part: with none, the tool and Add box wait for one.
    let tool = || app.locator(Selector::id("t_settings_here")).wait_visible().snapshot();
    wait(&app, "the box tool off with no part picked", || (!tool().enabled()).then_some(()));
    app.locator(Selector::role(Role::Tab).name("Overrides")).click();
    let add = || app.locator(Selector::role(Role::Button).name("Add print settings volume")).wait_visible().snapshot();
    wait(&app, "Add box off with no part picked", || (!add().enabled()).then_some(()));
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).name("Block")).wait_visible().click();
    wait(&app, "the box tool on with the part picked", || tool().enabled().then_some(()));
    app.locator(Selector::role(Role::Tab).name("Overrides")).click();
    wait(&app, "Add box on with the part picked", || add().enabled().then_some(()));
    frame(&app);
    app.locator(Selector::id("rm_normal")).wait_visible().wait_checked(true);
    assert!(drawn_box(&app).is_none(), "a box is drawn with none on the plate");

    // In Normal, the rail's tool across the block: the view stays Normal, the box is picked
    // and drawn over the model.
    let plain = source(&app);
    app.locator(Selector::id("t_settings_here")).wait_visible().click();
    app.locator(Selector::id("viewport")).wait_visible().drag_by_and_hold(160.0, 120.0);
    let drawn = wait(&app, "the drawn box in the script", || Some(source(&app)).filter(|s| s.contains("m.settings(settings1, {sparse_infill_density: \"100%\"})")));
    assert!(drawn.starts_with(&plain), "the box goes after the block's own lines:\n{drawn}");
    assert_eq!(app.locator(Selector::id("rm_normal")).snapshot().checked(), Some(true), "the box's tool left the Normal view");
    assert_eq!(app.locator(Selector::id("rm_slice")).snapshot().checked(), Some(false), "the box's tool went to the Sliced view");
    wait(&app, "the box's row, picked", || Some(override_row(&app, "Print settings box 1: Density 100 %")).filter(|r| r.2));
    wait(&app, "the picked box drawn", || drawn_box(&app));
    assert_eq!(app.locator(Selector::id("t_select")).snapshot().checked(), Some(true), "the box drawn, the tool is the Select one");
    println!("[settings-here] in Normal the tool drew {:?}", drawn_box(&app).map(|w| w.node.description.to_string()));

    // Its Size Z field (the box's height on the plate, its rows reading in the plate's axes),
    // scrubbed: the drawn box grows upwards with it in every frame, one undo step for the whole
    // scrub; typed, the same.
    let size_z = wait(&app, "the box's Size Z field", || box_size_z(&app));
    let first = drawn_matches_field(&app, &size_z, "before the scrub");
    let mut scrub = app.locator(Selector::path(size_z.as_str())).wait_visible().press_and_drag();
    scrub.move_by(3.0, 0.0);
    let mut seen = vec![first];
    let top = |app: &TestApp| drawn_box(app).map(|w| { let (size, at) = drawn_on_plate(&w); at[2] + 0.5 * size[2] }).expect("the box drawn");
    let top_before = top(&app);
    for step in 1..=6 {
        scrub.move_by(3.0 + 8.0 * step as f64, 0.0);
        seen.push(drawn_matches_field(&app, &size_z, &format!("scrub step {step}")));
    }
    assert!(top(&app) > top_before + 0.2, "the drawn box's top did not rise with its Size Z: {top_before} -> {}", top(&app));
    assert!(seen.last().is_some_and(|z| *z > first + 0.5), "the scrub did not change the size: {seen:?}");
    scrub.release();
    wait(&app, "the scrub's commit", || Some(source(&app)).filter(|s| *s != drawn));
    drawn_matches_field(&app, &size_z, "after the scrub");
    undo(&app);
    wait(&app, "one undo to take the scrub back", || (source(&app) == drawn).then_some(()));
    wait(&app, "the drawn box back at its height", || ((drawn_height(&drawn_box(&app)?) - first).abs() < 0.06).then_some(()));
    println!("[settings-here] scrubbed {seen:?}, one undo step");
    app.locator(Selector::path(size_z.as_str())).set_value(format!("{:.1}", first + 4.0));
    assert!((drawn_matches_field(&app, &size_z, "typed") - (first + 4.0)).abs() < 0.06, "the typed Size Z");
    let typed = wait(&app, "the typed size's commit", || Some(source(&app)).filter(|s| *s != drawn));

    // With the box picked and a handle of its under the pointer, the secondary button's drag is
    // the camera's: the box moves on the screen, its script stays.
    let (gx, gy) = steady(&app, Role::Slider, "Size Z handle");
    let before = drawn_box(&app).expect("the box drawn").bounds;
    mouse(&app, "down", gx, gy, 1);
    for i in 1..=8 {
        mouse(&app, "move", gx + 6.0 * f64::from(i), gy + 3.0 * f64::from(i), 1);
    }
    mouse(&app, "up", gx + 48.0, gy + 24.0, 1);
    let after = wait(&app, "the box drawn after the camera's drag", || drawn_box(&app)).bounds;
    let moved = (after.x - before.x).abs() + (after.y - before.y).abs() + (after.w - before.w).abs() + (after.h - before.h).abs();
    assert!(moved > 5.0, "a secondary drag over the box's handle left the camera: {before:?} -> {after:?}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(source(&app), typed, "a secondary drag over the box's handle changed the box");
    // The primary button on the handle sizes the box.
    let mut hold = app.locator(Selector::role(Role::Slider).name("Size Z handle")).wait_visible().press_and_drag();
    hold.move_by(0.0, -20.0);
    hold.move_by(0.0, -40.0);
    std::thread::sleep(Duration::from_millis(200));
    hold.release();
    let sized = wait(&app, "the handle's drag to size the box", || Some(source(&app)).filter(|s| *s != typed));
    println!("[settings-here] the camera took the secondary drag ({moved:.0} px), the handle sized the box");

    // A click on the model through the box picks the part: the box is unpicked and no longer
    // drawn, and the Scale tool scales the part, not the box.
    let (cx, cy) = on_the_box_off_its_grips(&app);
    mouse(&app, "down", cx, cy, 0);
    mouse(&app, "up", cx, cy, 0);
    wait(&app, "the box to go from the view", || drawn_box(&app).is_none().then_some(()));
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    let rows = wait(&app, "the block picked in the Parts list", || Some(selected_tree_rows(&app)).filter(|r| r.iter().any(|n| n == "Block")));
    assert_eq!(rows, ["Block"], "a click on the model picks the part alone");
    app.locator(Selector::role(Role::Tab).name("Overrides")).click();
    let lit: Vec<(String, String, bool)> = override_rows(&app).into_iter().filter(|r| r.2).collect();
    assert!(lit.is_empty(), "an Overrides row stays picked: {lit:?}");
    let placed = block_place(&app);
    // (framed again: the camera's drag above may have left the Scale grip past the view's edge)
    frame(&app);
    app.locator(Selector::id("t_scale")).wait_visible().click();
    let (hx, hy) = steady(&app, Role::Button, "Scale");
    let view = app.locator(Selector::id("viewport")).snapshot().bounds;
    assert!(view.contains(hx as f32, hy as f32), "the Scale grip at ({hx:.0}, {hy:.0}) is off the view {view:?}");
    mouse(&app, "down", hx, hy, 0);
    for i in 1..=10 {
        mouse(&app, "move", hx + 3.0 * f64::from(i), hy - 5.0 * f64::from(i), 0);
    }
    std::thread::sleep(Duration::from_millis(200));
    mouse(&app, "up", hx + 30.0, hy - 50.0, 0);
    let scaled = wait(&app, "the Scale tool to scale the part", || Some(block_place(&app)).filter(|l| *l != placed));
    assert!(scaled.contains("scale:"), "the part's scale: {scaled}");
    assert_eq!(source(&app), sized, "the Scale tool on the part changed the box");
    assert!(drawn_box(&app).is_none(), "the part picked, a box is drawn");
    println!("[settings-here] the click picked the part and the Scale tool scaled it: {scaled}");

    // Esc in the view: nothing picked, no box drawn.
    app.locator(Selector::id("t_select")).click();
    let (cx, cy) = middle(&app.locator(Selector::id("viewport")).snapshot());
    mouse(&app, "down", cx, cy, 0);
    mouse(&app, "up", cx, cy, 0);
    app.press_key(KeyCode::Escape);
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    wait(&app, "Esc to pick nothing", || selected_tree_rows(&app).is_empty().then_some(()));
    assert!(drawn_box(&app).is_none(), "nothing picked, a box is drawn");
}

/// A project edit through the fab tools (`id` its request's), at the current head.
fn place_edit(app: &TestApp, id: &str, edits: &str) {
    let edits = Value::Str(edits.to_string()).to_json();
    call(app, "place", &format!(r#"{{"request_id":"{id}","expected":"{}","edits":{edits}}}"#, head(app))).expect("place");
}

/// The view framed on what is selected.
fn frame(app: &TestApp) {
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Frame Selected")).wait_visible().press();
}

fn near(a: [f64; 3], b: [f64; 3], by: f64) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() < by)
}

/// The drawn box on the plate once it reads as `want` (size, middle) to within `by` mm.
fn drawn_as(app: &TestApp, what: &str, want: ([f64; 3], [f64; 3]), by: f64) -> ([f64; 3], [f64; 3]) {
    let mut last = None;
    let drawn = try_wait(app, &format!("the drawn box {what}"), || {
        last = drawn_box(app).map(|w| drawn_on_plate(&w));
        last.filter(|(size, at)| near(*size, want.0, by) && near(*at, want.1, by))
    });
    drawn.unwrap_or_else(|why| panic!("{what}: the drawn box reads {last:?}, not {want:?}: {why}"))
}

/// A box picked in the Features tree is drawn and its Overrides row is the one picked, and the
/// other way about: one pick, in both lists. The Parts list lists its block alone. The drawn
/// box keeps to its part's frame: a move of the block takes it as far, a scale of the block
/// (about its origin, dropped on the plate) scales it on the same region of the model.
#[makepad_test]
fn a_box_picked_in_either_list_is_drawn_and_follows_its_parts_move_and_scale(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait(&app, "the fab tools", || call(&app, "inspect", "{}").ok());
    let script = format!("{BLOCK}let settings1 = m.box({{size: [12mm 10mm 14mm] at: [3mm 2mm 1mm]}})\nm.settings(settings1, {{sparse_infill_density: \"100%\"}})\n");
    let batch = format!(r#"{{"request_id":"block-box","expected":"{}","script":"block.cad.splash","source":{}}}"#, head(&app), Value::Str(script).to_json());
    call(&app, "script", &batch).expect("script");
    place_edit(&app, "block-box-place", "place(\"block\", at: [175mm 160mm], name: \"Block\")");
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    parts_rows(&app);
    app.locator(Selector::role(Role::TreeItem).name("Block")).wait_visible().click();
    frame(&app);
    assert!(drawn_box(&app).is_none(), "a box is drawn while none is picked");

    // Picked in the Features tree: drawn, and its Overrides row is the one picked.
    app.locator(Selector::role(Role::Tab).name("Features")).click();
    app.locator(Selector::role(Role::ListItem).name(BOX_FEATURE)).wait_visible().select();
    wait(&app, "the box picked in the Features tree, drawn", || drawn_box(&app));
    app.locator(Selector::role(Role::Tab).name("Overrides")).click();
    let lit = wait(&app, "its Overrides row, picked", || Some(override_rows(&app).into_iter().filter(|r| r.2).collect::<Vec<_>>()).filter(|l| !l.is_empty()));
    assert!(lit.len() == 1 && lit[0].0.starts_with("Print settings box 1"), "one pick, the box's: {lit:?}");

    // Unpicked (a click on the model through it picks the block), then picked in the Overrides
    // list: drawn, and its Features row is the one picked.
    let (cx, cy) = on_the_box_off_its_grips(&app);
    mouse(&app, "down", cx, cy, 0);
    mouse(&app, "up", cx, cy, 0);
    wait(&app, "the box unpicked, no longer drawn", || drawn_box(&app).is_none().then_some(()));
    let (row, _, _) = override_row(&app, "Print settings box 1");
    app.locator(Selector::role(Role::ListItem).name(row.as_str())).press();
    wait(&app, "the box picked in the Overrides list, drawn", || drawn_box(&app));
    app.locator(Selector::role(Role::Tab).name("Features")).click();
    // (the tree's rows, each named with its level)
    let picked = wait(&app, "its Features row, picked", || Some(shown(&app, Role::ListItem).into_iter().filter(|w| w.node.name.contains(" level ") && w.node.states.contains(States::SELECTED)).map(|w| w.node.name.to_string()).collect::<Vec<_>>()).filter(|p| !p.is_empty()));
    assert_eq!(picked, [BOX_FEATURE], "one pick, the box's");

    // The block moved 20 mm along the plate's X: the box goes as far, its size as it was.
    let (size, at) = drawn_on_plate(&drawn_box(&app).expect("the box drawn"));
    place_edit(&app, "block-box-move", "place(\"block\", at: [195mm 160mm])");
    frame(&app);
    let moved = drawn_as(&app, "after the move", (size, [at[0] + 20.0, at[1], at[2]]), 0.11);
    // Scaled twice about its origin (at [195, 160] on the plate) and dropped on the plate (its
    // foot stays at 0): the box twice its size, twice as far from that origin and that foot.
    place_edit(&app, "block-box-scale", "place(\"block\", scale: 2)");
    frame(&app);
    let (size, at) = moved;
    let scaled = drawn_as(&app, "after the scale", (size.map(|s| 2.0 * s), [195.0 + 2.0 * (at[0] - 195.0), 160.0 + 2.0 * (at[1] - 160.0), 2.0 * at[2]]), 0.25);
    println!("[settings-here] the box drawn at {at:?} ({size:?}) followed the block: moved {moved:?}, scaled {scaled:?}");
}

/// A wait on an app that sits idle with nothing pending ends on the runtime's idle rule, in
/// seconds, not after [`HANG`]: a failing wait here costs the tests queued behind it no minutes.
#[makepad_test]
fn a_wait_on_an_idle_app_ends_in_seconds(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait(&app, "the fab tools", || call(&app, "inspect", "{}").ok());
    let started = Instant::now();
    let why = try_wait(&app, "what never comes", || None::<()>).expect_err("a wait for what never comes ended on an answer");
    let took = started.elapsed();
    println!("[settings-here] a wait on the idle app ended in {took:?}: {why}");
    assert!(why.contains("sat idle"), "the wait ended on {why:?}, not the idle rule");
    assert!(took < HANG / 5, "the wait on an idle app took {took:?}");
}

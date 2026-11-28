//! Test cubes by hand in the Normal view, on a 20 mm block: "Add test cube" in Test prints puts a
//! true cube on the picked part and picks it, one undo step, with no change of render mode; it is
//! a feature of the block in the Parts tree, not a part in the Parts list. In Design its fields and its
//! gizmo edit it, one undo step each, and a face's drag keeps the opposite face. A click on the
//! model picks the part, not the cube, and its handles go; a row in the list or the tree picks
//! that one alone. Its tick, its name and its delete write the project; `fab.tree` lists it. It
//! moves with its part.

#[path = "common/render_mode.rs"]
mod render_mode;
#[path = "common/picture.rs"]
mod picture;
#[path = "common/parts_tab.rs"]
mod parts_tab;
use fab::ui::print_settings::control_name;
use makepad_script::syntax;
use makepad_strict_json::Value;
use makepad_test::{makepad_test, KeyCode, NodeMatch, Role, Selector, States, TestApp};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::time::Duration;

const BLOCK: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let block = m.box({size: [20mm 20mm 20mm]})
object(block)
"##;

const PLACE: &str = "place(\"block\", at: [175mm 160mm], name: \"Block\")\n";

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
    let reply = call(app, "inspect", "{}").expect("inspect");
    syntax::parse_value(&reply).unwrap_or_else(|e| panic!("inspect's reply: {e:?}: {reply}"))
}

fn head(app: &TestApp) -> String {
    inspect(app).field("head").and_then(|f| f.value.string()).expect("a head").to_string()
}

fn wait<T>(app: &TestApp, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let mut got = None;
    let done = app.try_wait_until_within(HANG, || {
        got = f();
        got.is_none().then(|| format!("waiting for {what}"))
    });
    if let Err(why) = done {
        for l in app.log_since(0).map(|t| t.lines).unwrap_or_default().iter().filter(|l| l.contains("fab")).rev().take(40).collect::<Vec<_>>().into_iter().rev() {
            println!("[app] {l}");
        }
        for w in app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && matches!(w.node.role, Role::Button | Role::List | Role::ListItem | Role::SpinButton | Role::CheckBox) && !w.node.name.is_empty()) {
            println!("[test-cubes] {:?} {:?} {:?} {:?} {}", w.node.role, w.node.name, w.bounds, w.node.states, w.node.path);
        }
        println!("[test-cubes] the project:\n{}", inspect(app).field("project").and_then(|f| f.value.string()).unwrap_or_default());
        panic!("timed out {}", why.message());
    }
    got.expect("the wait ended on an answer")
}

/// The app answers its tools (until then a call says no instance runs here).
fn fab_tools(app: &TestApp) {
    wait(app, "the fab tools", || call(app, "inspect", "{}").ok().filter(|reply| syntax::parse_value(reply).is_ok()).map(|_| ()));
}

/// A test print as `fab.inspect` lists it.
#[derive(Clone, Debug, PartialEq)]
struct Listed {
    name: String,
    on: bool,
    at: [f64; 3],
    size: [f64; 3],
}

fn three(n: &syntax::Node, key: &str) -> [f64; 3] {
    let items = n.field(key).and_then(|f| f.value.items()).unwrap_or_else(|| panic!("no {key}"));
    // (a length reads in metres)
    std::array::from_fn(|k| 1000.0 * items[k].number().unwrap_or_else(|| panic!("{key}[{k}]")))
}

fn listed(app: &TestApp) -> Vec<Listed> {
    let v = inspect(app);
    let Some(tests) = v.field("test_prints").and_then(|f| f.value.items()) else { return Vec::new() };
    tests.iter().map(|t| Listed {
        name: t.field("name").and_then(|f| f.value.string()).expect("a name").to_string(),
        on: t.field("on").is_some_and(|f| f.value.ident() == Some("true")),
        at: three(t, "at"),
        size: three(t, "size"),
    }).collect()
}

fn near(a: [f64; 3], b: [f64; 3], by: f64) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() < by)
}

fn shown(app: &TestApp, role: Role) -> Vec<NodeMatch> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == role).collect()
}

fn rows(app: &TestApp) -> Vec<(String, bool)> {
    let snapshot = app.widget_snapshot();
    let nodes = snapshot.nodes();
    let Some(list) = nodes.iter().find(|w| w.visible() && w.role == Role::List && w.name == "Test prints") else { return Vec::new() };
    let tree = &snapshot.windows.iter().find(|(window, _)| *window == list.window).unwrap().1;
    nodes.iter().filter(|w| w.visible() && w.window == list.window && w.role == Role::DisclosureButton && tree.ancestors(w.id).contains(&list.id))
        .map(|w| (w.name.to_string(), w.states.contains(States::SELECTED))).collect()
}

fn handle(app: &TestApp, name: &str) -> bool {
    shown(app, Role::Button).iter().any(|w| w.node.name == name)
}

fn undo(app: &TestApp) {
    app.locator(Selector::id("undo").nth(0)).press();
}

fn close_tests_sidebar(app: &TestApp) {
    let close = app.locator(Selector::role(Role::Button).name("Close tests"));
    if close.count() > 0 { close.press(); }
    app.locator(Selector::role(Role::Button).name("Close tests")).wait_hidden();
    app.locator(Selector::role(Role::Tooltip).name("Close the tests")).wait_hidden();
}

fn field(app: &TestApp, name: &str) -> makepad_test::Locator {
    app.locator(Selector::role(Role::SpinButton).name(name)).wait_visible()
}

/// Fields typed one after another as fast as a hand can: each value committed with no frame
/// between it and the next, so each field's edit is still on its way when the next one comes.
fn type_fields(app: &TestApp, values: &[(&str, &str)]) {
    for (name, _) in values {
        field(app, name);
    }
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect");
    for (name, value) in values {
        let q = Selector::role(Role::SpinButton).name(*name).as_query();
        let argv = ["ui", "act", "set_value", &format!("q={q}"), &format!("v={value}")].map(String::from);
        let mut said = Vec::new();
        let refused = makepad_remote::app::run(&client, &argv, &mut |l| {
            said.push(l.to_string());
            true
        })
        .expect("the field set");
        assert!(!refused, "{name}: {said:?}");
    }
}

const SIZE_X: &str = "Test cube: size +X";

#[makepad_test]
fn a_test_cube_is_added_edited_and_picked_like_a_settings_box_one_undo_step_each(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    fab_tools(&app);
    let batch = format!(r#"{{"request_id":"block","expected":"{}","script":"block.cad.splash","source":{}}}"#, head(&app), Value::Str(BLOCK.to_string()).to_json());
    call(&app, "script", &batch).expect("script");
    let edits = Value::Str(PLACE.to_string()).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"place","expected":"{}","edits":{edits}}}"#, head(&app))).expect("place");
    parts_tab::open(&app);
    app.locator(Selector::role(Role::TreeItem).name("Block")).wait_visible().click();
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Frame Selected")).wait_visible().press();
    // Add in the pieces view, then edit in place in Design. Both stay Normal.
    let cube = add_cube_in_test_prints(&app);
    assert_eq!((cube.name.as_str(), cube.on), ("Test 1", true), "{cube:?}");
    assert!(near(cube.size, [20.0; 3], 1e-3), "a true cube: {cube:?}");
    wait(&app, "its row, lit", || (rows(&app) == [("Test 1".to_string(), true)]).then_some(()));
    wait(&app, "its handles", || handle(&app, SIZE_X).then_some(()));
    render_mode::wait_not_shown(&app, "Sliced");

    // A feature of the block in the tree, listed by fab.tree; not in the Parts list.
    let tree = call(&app, "tree", "{}").expect("tree");
    assert!(tree.contains("test print") && tree.contains("Test 1"), "{tree}");
    let row = app.locator(Selector::role(Role::ListItem).name("base test print Test 1 level 1")).wait_visible().snapshot();
    assert!(row.node.states.contains(States::SELECTED), "its row in the tree is picked");
    parts_tab::open(&app);
    let parts = wait(&app, "the Parts list", || Some(shown(&app, Role::TreeItem).into_iter().map(|w| w.node.name.to_string()).collect::<Vec<_>>()).filter(|p| !p.is_empty()));
    assert_eq!(parts, ["Block"], "the Parts list lists the block alone");

    // Typed: one undo step.
    field(&app, "Test print size Z").set_value("30");
    wait(&app, "the typed side", || listed(&app).first().filter(|t| (t.size[2] - 30.0).abs() < 1e-3).cloned());
    undo(&app);
    wait(&app, "undo of the typed side", || listed(&app).first().filter(|t| (t.size[2] - 20.0).abs() < 1e-3).cloned());

    // A face dragged: its side changes, the opposite face stays; one undo step.
    wait(&app, "its handles", || handle(&app, SIZE_X).then_some(()));
    app.locator(Selector::role(Role::Button).name(SIZE_X)).drag_by(40.0, 0.0);
    let dragged = wait(&app, "the dragged face", || listed(&app).first().filter(|t| (t.size[0] - cube.size[0]).abs() > 0.5).cloned());
    let foot = |t: &Listed| t.at[0] - 0.5 * t.size[0];
    assert!((foot(&dragged) - foot(&cube)).abs() < 1e-2, "the opposite face stays: {dragged:?} from {cube:?}");
    assert!(near([dragged.size[1], dragged.size[2], dragged.at[1]], [cube.size[1], cube.size[2], cube.at[1]], 1e-3), "only that side: {dragged:?} from {cube:?}");
    undo(&app);
    wait(&app, "undo of the drag", || listed(&app).first().filter(|t| near(t.size, cube.size, 1e-3) && near(t.at, cube.at, 1e-3)).cloned());

    // A click on the model picks the part, through the cube: its handles go, its row is not lit.
    // (between two of its handles, on the block)
    let spot = |name: &str| {
        let w = shown(&app, Role::Button).into_iter().find(|w| w.node.name == name).unwrap_or_else(|| panic!("no handle {name}"));
        w.bounds.center()
    };
    let (a, b) = (spot("Test cube: move freely"), spot(SIZE_X));
    app.press_at(0.5 * f64::from(a.0 + b.0), 0.5 * f64::from(a.1 + b.1)).release();
    wait(&app, "the handles to go", || (!handle(&app, SIZE_X)).then_some(()));
    assert_eq!(rows(&app), [("Test 1".to_string(), false)], "unpicked");
    app.locator(Selector::role(Role::DisclosureButton).name("Test 1").state("collapsed")).wait_visible();

    // Two: a click on a row picks that one alone.
    app.locator(Selector::id("side_add").name("Add test cube")).wait_visible().press();
    wait(&app, "the second, lit", || (rows(&app) == [("Test 1".to_string(), false), ("Test 2".to_string(), true)]).then_some(()));
    app.locator(Selector::role(Role::DisclosureButton).name("Test 1")).select();
    wait(&app, "the first alone", || (rows(&app) == [("Test 1".to_string(), true), ("Test 2".to_string(), false)]).then_some(()));
    // ... and in the tree.
    app.locator(Selector::role(Role::ListItem).name("base test print Test 2 level 1")).wait_visible().select();
    wait(&app, "the tree's pick", || (rows(&app) == [("Test 1".to_string(), false), ("Test 2".to_string(), true)]).then_some(()));

    // Its tick, its name, its delete, each undone.
    let tick = || app.locator(Selector::role(Role::CheckBox).name("Print this test").nth(1)).wait_visible();
    tick().click();
    wait(&app, "the second unticked", || listed(&app).get(1).filter(|t| !t.on).cloned());
    undo(&app);
    wait(&app, "the second ticked", || listed(&app).get(1).filter(|t| t.on).cloned());
    app.locator(Selector::role(Role::TextField).name("Test print name").state("visible")).wait_count(1).fill("Corner").press_key(KeyCode::ReturnKey);
    wait(&app, "the new name, still lit", || (rows(&app) == [("Test 1".to_string(), false), ("Corner".to_string(), true)]).then_some(()));
    app.locator(Selector::role(Role::Button).name("Delete test print").nth(1)).wait_visible().press();
    wait(&app, "the delete", || (listed(&app).len() == 1).then_some(()));
    wait(&app, "its handles to go", || (!handle(&app, SIZE_X)).then_some(()));

    // It moves with its part.
    let before = listed(&app)[0].clone();
    let moved = Value::Str("place(\"block\", at: [195mm 150mm], name: \"Block\")\n".to_string()).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"move","expected":"{}","edits":{moved}}}"#, head(&app))).expect("the move");
    let after = wait(&app, "the cube to follow", || listed(&app).first().filter(|t| (t.at[0] - before.at[0]).abs() > 1.0).cloned());
    assert!(near(after.at, [before.at[0] + 20.0, before.at[1] - 10.0, before.at[2]], 1e-3), "{after:?} from {before:?}");
}

/// The view's rectangle in the window.
/// `fab.test_print`'s read.
fn read(app: &TestApp) -> syntax::Node {
    let reply = call(app, "test_print", r#"{"action":"read"}"#).expect("the read");
    syntax::parse_value(&reply).unwrap_or_else(|e| panic!("the read: {e:?}: {reply}"))
}

fn text_of(n: &syntax::Node, key: &str) -> String {
    n.field(key).and_then(|f| f.value.string()).unwrap_or_default().to_string()
}

fn num_of(n: &syntax::Node, key: &str) -> Option<f64> {
    n.field(key).and_then(|f| f.value.number())
}

/// The slice standing, of what's chosen: its id, once it is whole and timed.
fn sliced_id(app: &TestApp, what: &str) -> f64 {
    wait(app, what, || {
        let r = read(app);
        let whole = text_of(&r, "slicing") == "whole plate";
        let timed = if whole { num_of_figure(&r, "whole_plate").is_some() } else { flag_of(&r, "current") };
        (text_of(&r, "state") == "Sliced" && timed).then(|| num_of(&r, "slice_id")).flatten()
    })
}

fn flag_of(n: &syntax::Node, key: &str) -> bool {
    n.field(key).is_some_and(|f| f.value.ident() == Some("true"))
}

/// A figure's grams: None while the read says there's none.
fn num_of_figure(n: &syntax::Node, key: &str) -> Option<f64> {
    n.field(key).and_then(|f| num_of(&f.value, "grams"))
}

/// The Plate mode menu chooses what the Slicer slices.
fn choose(app: &TestApp, tests: bool) {
    app.locator(Selector::role(Role::Button).name("Plate mode")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name(if tests { "Test prints" } else { "Design" })).wait_visible().press();
    wait(app, "the choice", || (text_of(&read(app), "slicing") == if tests { "test prints" } else { "whole plate" }).then_some(()));
}

/// Add through the toolbar once the test pieces have replaced the original plate.
fn add_cube_in_test_prints(app: &TestApp) -> Listed {
    choose(app, true);
    wait(app, "the pieces view before Add", || view_pieces(app));
    app.locator(Selector::id("add_test").name("Add test cube")).wait_visible().press();
    let cube = wait(app, "the cube", || listed(app).into_iter().next());
    choose(app, false);
    cube
}

/// The plate of one block with a test cube, in Design's Sliced view.
fn block_with_a_cube(app: &TestApp) {
    part_with_a_cube(app, BLOCK);
}

/// The plate of one part, `source`'s, with a test cube on it, in Design's Sliced view.
fn part_with_a_cube(app: &TestApp, source: &str) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    fab_tools(app);
    let batch = format!(r#"{{"request_id":"block","expected":"{}","script":"block.cad.splash","source":{}}}"#, head(app), Value::Str(source.to_string()).to_json());
    call(app, "script", &batch).expect("script");
    let edits = Value::Str(PLACE.to_string()).to_json();
    call(app, "place", &format!(r#"{{"request_id":"place","expected":"{}","edits":{edits}}}"#, head(app))).expect("place");
    parts_tab::open(&app);
    app.locator(Selector::role(Role::TreeItem).name("Block")).wait_visible().click();
    add_cube_in_test_prints(app);
    render_mode::pick(app, "Sliced");
}

/// The cube set to a true cube of `side` by the tool, and on the list so.
fn cube_side(app: &TestApp, side: f64) {
    // A request's id is its own: the same id again is answered as before, nothing done.
    let at = head(app);
    let set = format!(r#"{{"request_id":"side{side}-{at}","expected":"{at}","action":"set","name":"Test 1","size":{side}}}"#);
    call(app, "test_print", &set).expect("the cube's side");
    wait(app, "the cube's side", || listed(app).first().filter(|t| near(t.size, [side; 3], 1e-3)).cloned());
}

/// Whole plate / Test prints in the Slicer: the test prints slice by themselves with their time and
/// grams next to the plate's, and each way back shows the slice left, with no slice, until what it
/// came from changed. A cube edited on Test prints reslices at once; settings changed behind a
/// kept slice mean it never shows again (the guard against a kept slice standing in for another).
#[makepad_test]
fn the_slicer_switches_to_test_prints_and_back_with_no_fresh_slice_until_something_changed(app: TestApp) {
    block_with_a_cube(&app);
    cube_side(&app, 8.0);
    let whole = sliced_id(&app, "the whole plate's slice");
    let plate = read(&app);
    let (plate_grams, plate_seconds) = {
        let figure = &plate.field("whole_plate").expect("the plate's figures").value;
        (num_of(figure, "grams").expect("grams"), num_of(figure, "seconds").expect("seconds"))
    };

    // Test prints: its own slice, its piece's figures by name, the plate's next to them.
    choose(&app, true);
    let piece = sliced_id(&app, "the test print's slice");
    assert_ne!(piece, whole, "the test prints have a slice of their own");
    let r = read(&app);
    let total = num_of_figure(&r, "total").expect("the test plate's total");
    assert!(total < plate_grams, "the cube is less than the block: {total} g of {plate_grams} g");
    assert_eq!(num_of_figure(&r, "whole_plate"), Some(plate_grams), "the plate's figures stand next to it");
    let printing = |r: &syntax::Node| r.field("pieces").and_then(|f| f.value.items()).and_then(|a| a.first().and_then(|p| num_of_figure(p, "print")));
    let grams = printing(&r).expect("the piece's figures");
    assert!(grams > 0.0 && grams <= total, "the piece's grams: {grams} of {total}");
    let time = r.field("total").and_then(|f| num_of(&f.value, "seconds")).expect("the test plate's time");
    assert!(time < plate_seconds, "the cube takes less time than the block: {time} s of {plate_seconds} s");

    // Back: at once the slice left, not a fresh one. So too the other way.
    let served = |app: &TestApp| syntax::parse_value(&call(app, "slice", "{}").expect("slice")).expect("a Splash reply");
    choose(&app, false);
    let back = read(&app);
    assert_eq!(text_of(&back, "state"), "Sliced", "the plate shows again at once");
    assert_eq!(num_of(&back, "slice_id"), Some(whole), "the slice left, no fresh one");
    let held = num_of(&served(&app), "revision").expect("a revision");
    choose(&app, true);
    let back = read(&app);
    assert_eq!(text_of(&back, "state"), "Sliced", "the test print shows again at once");
    assert_eq!(num_of(&back, "slice_id"), Some(piece), "the test print's slice left, no fresh one");
    assert!(flag_of(&back, "current"), "it reads as current");
    assert!(num_of(&served(&app), "revision").is_some_and(|r| r > held), "the engine holds the input it shows");

    // A cube edited there reslices without another word, and the plate's slice is not touched by it.
    cube_side(&app, 6.0);
    wait(&app, "the smaller cube's slice", || {
        let r = read(&app);
        (flag_of(&r, "current") && num_of(&r, "slice_id").is_some_and(|id| id != piece) && printing(&r).is_some_and(|g| g < grams)).then_some(())
    });
    let edited = sliced_id(&app, "the smaller cube's slice");
    choose(&app, false);
    let back = read(&app);
    assert_eq!(num_of(&back, "slice_id"), Some(whole), "a cube's edit does not date the plate's slice");
    assert_eq!(text_of(&back, "state"), "Sliced");

    // On the plate, the cube edited behind its kept slice: that slice does not show again.
    cube_side(&app, 5.0);
    choose(&app, true);
    assert_ne!(num_of(&read(&app), "slice_id"), Some(edited), "the slice of another cube does not stand in for this one");
    let moved = sliced_id(&app, "the edited cube's slice");
    assert_ne!(moved, edited, "the edited cube has its own slice");

    // Settings changed on Test prints: the plate's kept slice is of the old ones and never shows again.
    app.locator(Selector::role(Role::Tab).name("Slicer")).click();
    let height = || Selector::role(Role::SpinButton).name(control_name(fab_plate::print_setting("layer_height").unwrap()));
    let old = app.locator(height()).wait_visible().snapshot().range.expect("layer height range").now;
    let to = if (old - 0.3).abs() < 0.001 { "0.25" } else { "0.3" };
    app.locator(height()).wait_visible().set_value(to);
    wait(&app, "the setting in the project", || text_of(&inspect(&app), "project").contains(&format!("layer_height: \"{to}\"")).then_some(()));
    wait(&app, "the test print under the new settings", || {
        let r = read(&app);
        (flag_of(&r, "current") && num_of(&r, "slice_id").is_some_and(|id| id != moved)).then_some(())
    });
    let tests_now = sliced_id(&app, "the test print under the new settings");
    choose(&app, false);
    assert_ne!(num_of(&read(&app), "slice_id"), Some(whole), "a slice of the old settings does not show as the plate's");
    let fresh = sliced_id(&app, "the plate under the new settings");
    assert_ne!(fresh, whole, "the plate slices anew under the new settings");

    // In Normal nothing slices, so no later slice covers for a kept one: unchanged, each shows again ...
    render_mode::pick(&app, "Normal");
    choose(&app, true);
    assert_eq!(num_of(&read(&app), "slice_id"), Some(tests_now), "the test print's slice left shows in Normal too");
    choose(&app, false);
    assert_eq!(num_of(&read(&app), "slice_id"), Some(fresh), "the plate's slice left shows in Normal too");
    // ... but with the cube edited behind it, the kept slice reads as nothing sliced, not as current ...
    cube_side(&app, 4.0);
    choose(&app, true);
    let r = read(&app);
    assert_ne!(num_of(&r, "slice_id"), Some(tests_now), "another cube's slice does not stand as this one's");
    assert!(!flag_of(&r, "current") && text_of(&r, "state") != "Sliced", "nothing is sliced: {}", text_of(&r, "state"));
    // ... and with the settings changed behind it, likewise.
    // (the plate's own setting: a part's would change the plate's identity, and only the kept
    // slice's settings can tell this one from what would slice now)
    let edits = Value::Str("settings({wall_loops: \"5\"})\n".to_string()).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"walls","expected":"{}","edits":{edits}}}"#, head(&app))).expect("the plate's setting");
    wait(&app, "the setting in the project", || text_of(&inspect(&app), "project").contains("wall_loops: \"5\"").then_some(()));
    choose(&app, false);
    let r = read(&app);
    assert_ne!(num_of(&r, "slice_id"), Some(fresh), "a slice of other settings does not stand as the plate's");
    assert_ne!(text_of(&r, "state"), "Sliced", "nothing is sliced");
}

/// The tool and the hand make the same program and the same G-code of the same cube.
#[makepad_test]
fn the_tool_and_the_hand_give_the_same_program_and_gcode(app: TestApp) {
    block_with_a_cube(&app);
    // A settled slice of what's chosen: `fab.print` asks for it once (no printer is linked here),
    // and the confirm row is all it opens.
    let settled = |app: &TestApp, what: &str| -> String {
        sliced_id(app, what);
        call(app, "print", "{}").expect("the print asked");
        wait(app, what, || {
            let r = read(app);
            let key = text_of(&r, "gcode_key");
            (flag_of(&r, "current") && !key.is_empty()).then_some(key)
        })
    };
    let line = |app: &TestApp| -> String {
        text_of(&inspect(app), "project").lines().find(|l| l.contains("test_print(")).expect("the cube in the project").trim().to_string()
    };

    // By hand: the fields, typed back to back (all three must stand), then Test prints.
    type_fields(&app, &[("Test print size X", "8"), ("Test print size Y", "8"), ("Test print size Z", "8")]);
    wait(&app, "the fields", || listed(&app).first().filter(|t| near(t.size, [8.0; 3], 1e-3)).cloned());
    choose(&app, true);
    let by_hand = settled(&app, "the settled slice by hand");
    let program = line(&app);
    let at = listed(&app).remove(0).at;
    let object = read(&app).field("pieces").and_then(|f| f.value.items()).and_then(|a| a.first().map(|p| text_of(p, "object"))).expect("the piece's part");
    choose(&app, false);
    let remove = format!(r#"{{"request_id":"gone","expected":"{}","action":"remove","name":"Test 1"}}"#, head(&app));
    call(&app, "test_print", &remove).expect("the cube removed");
    wait(&app, "the cube gone", || listed(&app).is_empty().then_some(()));

    // By tool, sliced at another size first: what it comes to is its own slice, not the one left.
    let add = format!(r#"{{"request_id":"again","expected":"{}","action":"add","boxes":[{{"name":"Test 1","object":{},"at":[{},{},{}],"size":9}}]}}"#, head(&app), Value::Str(object).to_json(), at[0], at[1], at[2]);
    call(&app, "test_print", &add).expect("the cube added");
    wait(&app, "the cube", || listed(&app).into_iter().next());
    call(&app, "test_print", r#"{"action":"mode","tests":true}"#).expect("the mode");
    sliced_id(&app, "the larger cube's slice");
    cube_side(&app, 8.0);
    let by_tool = settled(&app, "the settled slice by tool");
    assert_eq!(line(&app), program, "the same program");
    assert_eq!(by_tool, by_hand, "the same G-code");
}

/// How the read says the first test print's piece lies: "as printed", or empty for a pose.
fn pose_of(app: &TestApp) -> String {
    read(app).field("pieces").and_then(|f| f.value.items()).and_then(|a| a.first().map(|p| text_of(p, "pose"))).expect("a piece")
}

fn pose_degrees(app: &TestApp) -> Option<[f64; 3]> {
    let r = read(app);
    let piece = r.field("pieces").and_then(|f| f.value.items()).and_then(|a| a.first()).expect("a piece");
    let pose = &piece.field("pose").expect("the piece's pose").value;
    if pose.string() == Some("as printed") {
        return None;
    }
    let degrees = pose.items().expect("pose degrees");
    assert_eq!(degrees.len(), 3, "three pose angles");
    Some(std::array::from_fn(|k| degrees[k].number().expect("a pose angle")))
}

#[makepad_test]
fn a_test_prints_pose_is_typed_and_put_back_as_printed_one_undo_step_each(app: TestApp) {
    block_with_a_cube(&app);
    let note_visible = || app.widget_snapshot().nodes().into_iter().any(|w| w.visible() && w.node.name == "Laid flat: its own layers. Tests this orientation, not the print's.");
    assert_eq!(pose_degrees(&app), None, "starts as printed");
    // Typed back to back: all three angles stand, each its own undo step.
    type_fields(&app, &[("Test print pose X", "90"), ("Test print pose Y", "30"), ("Test print pose Z", "15")]);
    wait(&app, "the typed pose and its note", || (pose_degrees(&app).is_some_and(|p| near(p, [90.0, 30.0, 15.0], 1e-3)) && note_visible()).then_some(()));
    app.locator(Selector::role(Role::Button).name("Test print as printed")).wait_visible().press();
    wait(&app, "as printed with no pose note", || (pose_degrees(&app).is_none() && !note_visible()).then_some(()));
    for (step, pose) in [("as printed", [90.0, 30.0, 15.0]), ("Z", [90.0, 30.0, 0.0]), ("Y", [90.0, 0.0, 0.0])] {
        undo(&app);
        wait(&app, &format!("undo takes back {step}"), || pose_degrees(&app).filter(|p| near(*p, pose, 1e-3)));
    }
    undo(&app);
    wait(&app, "undo restores as printed", || pose_degrees(&app).is_none().then_some(()));
}

/// A raw click at window coordinates, where a person would press.
fn click_at(app: &TestApp, x: f64, y: f64) {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect");
    let args = vec!["mouse".to_string(), "k=click".to_string(), format!("x={x:.1}"), format!("y={y:.1}"), "wait=1".to_string()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &args, &mut |l| { lines.push(l.to_string()); true }).expect("click");
    assert!(!lines.iter().any(|l| l.contains("\"err\":")), "click at {x},{y}: {lines:?}");
}

/// A test print's piece laid flat: by the tool on a cut side it slices in that pose, other G-code;
/// undo puts it back as printed and gives back its G-code byte for byte. By hand, its row's Lay
/// flat and a click on the piece, framed in the view's middle, lay it on the face the click meets.
#[makepad_test]
fn a_test_prints_piece_lays_flat_by_tool_and_by_hand_and_undo_gives_its_gcode_back(app: TestApp) {
    block_with_a_cube(&app);
    parts_tab::open(&app);
    app.locator(Selector::role(Role::TreeItem).name("Block")).wait_visible().click();
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Frame Selected")).wait_visible().press();
    // A slab in the block's middle: on its front it is another shape on the bed.
    let at = head(&app);
    call(&app, "test_print", &format!(r#"{{"request_id":"slab-{at}","expected":"{at}","action":"set","name":"Test 1","size":[8,8,4]}}"#)).expect("the slab");
    wait(&app, "the slab", || listed(&app).first().filter(|t| near(t.size, [8.0, 8.0, 4.0], 1e-3)).cloned());
    choose(&app, true);
    let settled = |app: &TestApp, what: &str| -> String {
        sliced_id(app, what);
        call(app, "print", "{}").expect("the print asked");
        wait(app, what, || {
            let r = read(app);
            let key = text_of(&r, "gcode_key");
            (flag_of(&r, "current") && !key.is_empty()).then_some(key)
        })
    };
    let as_printed = settled(&app, "the piece as printed");
    let at = head(&app);
    call(&app, "test_print", &format!(r#"{{"request_id":"lay-{at}","expected":"{at}","action":"lay","name":"Test 1","face":"front"}}"#)).expect("the lay");
    wait(&app, "the pose", || (pose_of(&app) != "as printed").then_some(()));
    let laid = settled(&app, "the piece on its front");
    assert_ne!(laid, as_printed, "on its front the piece prints otherwise");
    undo(&app);
    wait(&app, "as printed again", || (pose_of(&app) == "as printed").then_some(()));
    assert_eq!(settled(&app, "the piece as printed again"), as_printed, "undo gives back the piece's G-code byte for byte");

    // By hand, on the piece in place in Normal: Lay flat, then a click on it in the view's middle.
    choose(&app, false);
    render_mode::pick(&app, "Normal");
    app.locator(Selector::role(Role::DisclosureButton).name("Test 1")).wait_visible().select();
    wait(&app, "its handles", || handle(&app, SIZE_X).then_some(()));
    app.locator(Selector::role(Role::Button).name("Test print lay flat")).wait_visible().press();
    let (x0, y0, x1, y1) = viewport_rect(&app);
    click_at(&app, (x0 + x1) as f64 / 2.0, (y0 + y1) as f64 / 2.0);
    wait(&app, "the pose by hand", || (pose_of(&app) != "as printed").then_some(()));
    undo(&app);
    wait(&app, "as printed after the hand's undo", || (pose_of(&app) == "as printed").then_some(()));
}

/// A pin 3 mm square and 30 mm tall: a tall piece on a small footing.
const PIN: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
object(m.box({size: [3mm 30mm 3mm] at: [0 15mm 0]}))
"##;

/// A tall piece on a small footing is flagged as Fab's checks word it, in its row and in the read,
/// and the brim it is offered prints: its own brim, wider than the profile's, one undo step.
#[makepad_test]
fn a_tall_test_piece_on_a_small_footing_is_flagged_and_takes_a_brim_of_its_own(app: TestApp) {
    part_with_a_cube(&app, PIN);
    choose(&app, true);
    let piece = |r: &syntax::Node| r.field("pieces").and_then(|f| f.value.items()).and_then(|a| a.first().cloned()).expect("a piece");
    // The tests' settled slice after the one of id `after`, read.
    let settled = |app: &TestApp, what: &str, after: Option<f64>| wait(app, what, || {
        let r = read(app);
        (text_of(&r, "state") == "Sliced" && flag_of(&r, "current") && num_of(&r, "slice_id") != after).then_some(r)
    });
    let footing = |p: &syntax::Node| p.field("flags").and_then(|f| f.value.items()).is_some_and(|a| a.iter().any(|f| text_of(f, "kind") == "plate_contact"));
    let row = |app: &TestApp, says: &str| shown(app, Role::StaticText).iter().any(|w| w.node.name.contains("on the plate holds a part") && w.node.name.contains(says));
    let r = settled(&app, "the test slice", None);
    let p = piece(&r);
    assert!(footing(&p), "a pin of 3 mm by 3 mm, 30 mm tall, is flagged for its footing");
    assert_eq!(text_of(&p, "brim"), "the profile's");
    let profiles = num_of(&p, "brim_grams").expect("its brim's grams");
    wait(&app, "its row's flag", || row(&app, if profiles > 0.0 { "the profile's brim rings it" } else { "give it a brim" }).then_some(()));

    app.locator(Selector::role(Role::DisclosureButton).name("Test 1")).wait_visible().select();
    app.locator(Selector::role(Role::CheckBox).name("Test print brim")).wait_visible().click();
    wait(&app, "its own brim", || (text_of(&piece(&read(&app)), "brim") == "its own").then_some(()));
    assert!(text_of(&inspect(&app), "project").lines().any(|l| l.contains("test_print(") && l.contains("brim: true")), "the project keeps it");
    let r = settled(&app, "the slice with its own brim", num_of(&r, "slice_id"));
    let p = piece(&r);
    assert!(footing(&p), "its footing is flagged still: a brim does not change it");
    let own = num_of(&p, "brim_grams").expect("its brim's grams");
    assert!(own > profiles, "its own brim, 5 mm where the profile's auto brim takes no width, is more than that auto brim (capped at its 3 mm side): {own} g against {profiles} g");
    wait(&app, "its row says so", || row(&app, "it has a brim of its own").then_some(()));
    undo(&app);
    wait(&app, "the profile's brim again", || (text_of(&piece(&read(&app)), "brim") == "the profile's").then_some(()));
}

fn viewport_rect(app: &TestApp) -> (usize, usize, usize, usize) {
    let view = app.locator(Selector::id("viewport")).wait_visible().snapshot();
    (view.x().max(0) as usize, view.y().max(0) as usize, (view.x() + view.width()) as usize, (view.y() + view.height()) as usize)
}

/// The view's finished picture: width, components, 8-bit samples.
fn settled_picture(app: &TestApp) -> (usize, usize, Vec<u8>, std::path::PathBuf) {
    decoded(picture::finished_screenshot(app))
}

/// The view's finished picture of a frame cut to the test cube at `at`, `side` wide. A set's
/// reply and the session's answer come before the view has the cube: until then the view's
/// finished picture is of the cube before, so the frame's own description must name this one.
fn picture_cut_to(app: &TestApp, at: [f64; 3], side: f64) -> (usize, usize, Vec<u8>, std::path::PathBuf) {
    let num = |v: &Value| match v { Value::Int(i) => Some(*i as f64), Value::F64(x) => Some(*x), _ => None };
    let three = |v: Option<&Value>| -> Option<[f64; 3]> {
        let a = v?.as_arr()?;
        Some([num(a.first()?)?, num(a.get(1)?)?, num(a.get(2)?)?])
    };
    app.wait_until(|| {
        let view = picture::view(app);
        let cut = view.get("test_cut");
        let this = three(cut.and_then(|c| c.get("at"))).is_some_and(|c| near(c, at, 1e-3)) && three(cut.and_then(|c| c.get("size"))).is_some_and(|s| near(s, [side; 3], 1e-3));
        (!(this && picture::finished_in(&view))).then(|| format!("waiting for the finished picture cut to the {side} mm cube at {at:?}: the view's frame is cut to {cut:?}"))
    });
    decoded(app.screenshot())
}

fn decoded(path: std::path::PathBuf) -> (usize, usize, Vec<u8>, std::path::PathBuf) {
    let bytes = std::fs::read(&path).expect("the grab");
    let mut dec = PngDecoder::new(ZCursor::new(&bytes));
    let px = dec.decode_raw().expect("png");
    let (w, _) = dec.dimensions().expect("dims");
    (w, dec.colorspace().expect("colorspace").num_components(), px, path)
}

#[makepad_test]
fn a_picked_test_cube_cuts_the_view_to_its_piece_and_leaves_the_bed_whole(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    fab_tools(&app);
    let batch = format!(r#"{{"request_id":"block","expected":"{}","script":"block.cad.splash","source":{}}}"#, head(&app), Value::Str(BLOCK.to_string()).to_json());
    call(&app, "script", &batch).expect("script");
    let edits = Value::Str(PLACE.to_string()).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"place","expected":"{}","edits":{edits}}}"#, head(&app))).expect("place");
    parts_tab::open(&app);
    app.locator(Selector::role(Role::TreeItem).name("Block")).wait_visible().click();
    // Seen from the top, framed: the view's middle is the block's top face, the face the key
    // lights. A side face in the key's shade reads the same grey as its ghost over the bed does at
    // the studio's exposure, so a middle on that face could not tell the cut from the block.
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Viewpoint")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Top")).wait_visible().press();
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Frame Selected")).wait_visible().press();
    let (x0, y0, x1, y1) = viewport_rect(&app);
    let (w, n, whole, _) = settled_picture(&app);

    // Picked, a cube at a corner away from the view's middle: only that much of the block stays
    // solid; the view stays Normal.
    let cube = add_cube_in_test_prints(&app);
    close_tests_sidebar(&app);
    let corner = [cube.at[0] + 8.0, cube.at[1] + 8.0, cube.at[2] + 8.0];
    let set = format!(r#"{{"request_id":"small","expected":"{}","action":"set","name":"Test 1","at":[{},{},{}],"size":6}}"#, head(&app), corner[0], corner[1], corner[2]);
    call(&app, "test_print", &set).expect("the small cube");
    wait(&app, "the small cube", || listed(&app).first().filter(|t| near(t.size, [6.0; 3], 1e-3)).cloned());
    wait(&app, "its handles", || handle(&app, SIZE_X).then_some(()));
    render_mode::wait_not_shown(&app, "Sliced");
    let (_, _, cut, path) = picture_cut_to(&app, corner, 6.0);
    let same = |x0: usize, y0: usize, x1: usize, y1: usize| {
        let at = |x: usize, y: usize| (y * w + x) * n;
        (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| (0..3).all(|k| (cut[at(x, y) + k] as i32 - whole[at(x, y) + k] as i32).abs() <= 8)).count()
    };

    // The block, framed in the view's middle, has faded to its ghost ...
    let (mx, my, cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2, (x1 - x0) / 16, (y1 - y0) / 16);
    let middle = same(mx - cx, my - cy, mx + cx, my + cy);
    assert!(middle * 4 < 4 * cx * cy, "the block past the cube: {middle} of {} px as before; see {}", 4 * cx * cy, path.display());
    // ... and the bed it stands on is drawn as before: the view's bottom corners, far from it.
    let (cw, ch) = ((x1 - x0) / 6, (y1 - y0) / 6);
    let corners = same(x0, y1 - ch, x0 + cw, y1) + same(x1 - cw, y1 - ch, x1, y1);
    assert!(corners * 10 > 9 * 2 * cw * ch, "the bed: {corners} of {} px as before; see {}", 2 * cw * ch, path.display());
}

/// What the view shows of the test prints, once it is them: (drawn, its bounds on the bed).
fn view_pieces(app: &TestApp) -> Option<Vec<(bool, [[f64; 3]; 2])>> {
    view_pieces_of(&read(app))
}

fn view_pieces_of(r: &syntax::Node) -> Option<Vec<(bool, [[f64; 3]; 2])>> {
    let view = r.field("view")?;
    Some(view.value.items().unwrap_or_default().iter().map(|p| {
        let bed = p.field("bed").and_then(|f| f.value.items()).expect("its bounds");
        let corner = |n: &syntax::Node| -> [f64; 3] { let a = n.items().expect("a corner"); std::array::from_fn(|k| a[k].number().expect("mm")) };
        (flag_of(p, "drawn"), [corner(&bed[0]), corner(&bed[1])])
    }).collect())
}

/// The pieces once the scene installed is their own (made at the plate revision the read is of),
/// with that scene in words for a failure: the pieces and their scene install one action apart,
/// and a read between the two finds the pieces' parts missing from the plate's scene.
fn pieces_in_their_scene(app: &TestApp) -> Option<(Vec<(bool, [[f64; 3]; 2])>, String)> {
    let r = read(app);
    let pieces = view_pieces_of(&r)?;
    let plate = num_of(&r, "plate_revision")?;
    (num_of(&r, "scene_for") == Some(plate)).then(|| {
        let names: Vec<String> = r.field("scene_elements").and_then(|f| f.value.items()).unwrap_or_default().iter().filter_map(|n| n.string().map(str::to_string)).collect();
        (pieces, format!("scene elements {names:?}, plate revision {plate}, scene revision {}", num_of(&r, "scene_revision").unwrap_or(-1.0)))
    })
}

/// Test prints in Normal: the ticked pieces stand on the bed and nothing else does, the view stays
/// Normal, and nothing slices for it. Whole plate brings the plate back as it was, the same pixels,
/// and a cube edited there shows at once.
#[makepad_test]
fn test_prints_show_on_the_bed_in_normal_and_whole_plate_brings_the_plate_back(app: TestApp) {
    block_with_a_cube(&app);
    cube_side(&app, 8.0);
    render_mode::pick(&app, "Normal");
    // The block picked, framed: the plate shows whole, nothing cut.
    parts_tab::open(&app);
    app.locator(Selector::role(Role::TreeItem).name("Block")).wait_visible().click();
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Frame Selected")).wait_visible().press();
    close_tests_sidebar(&app);
    assert!(view_pieces(&app).is_none(), "Whole plate: the view is the plate's");
    let (x0, y0, x1, y1) = viewport_rect(&app);
    let (w, n, plate, _) = settled_picture(&app);
    let (_, _, again, _) = settled_picture(&app);

    choose(&app, true);
    let (pieces, scene) = wait(&app, "the pieces on the bed in their scene", || pieces_in_their_scene(&app).filter(|(v, _)| !v.is_empty()));
    assert_eq!(pieces.len(), 1, "one cube on one part: one piece");
    let (drawn, [lo, hi]) = pieces[0];
    assert!(drawn, "its part draws in the scene: {scene}");
    assert!(lo[2].abs() < 1e-6, "it stands on the bed: {lo:?}");
    for k in 0..3 {
        assert!(hi[k] - lo[k] <= 8.0 + 1e-6, "no more of the block than its cube: {lo:?} {hi:?}");
    }
    render_mode::wait_shown(&app, "Normal");
    let (_, _, tests, path) = settled_picture(&app);
    // The pixels of the view two pictures differ at: how many, and the box they lie in.
    let moved = |a: &[u8], b: &[u8]| {
        let at = |x: usize, y: usize| (y * w + x) * n;
        let off: Vec<(usize, usize)> = (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| a[at(x, y)..at(x, y) + 3] != b[at(x, y)..at(x, y) + 3]).collect();
        let span = off.iter().fold((usize::MAX, usize::MAX, 0, 0), |(a, b, c, d), &(x, y)| (a.min(x), b.min(y), c.max(x), d.max(y)));
        (off.len(), span)
    };
    // A finished picture repeats exactly: the same view drawn again, nothing changed, is the same
    // pixels.
    let ((by_itself, span), (changed, _)) = (moved(&plate, &again), moved(&plate, &tests));
    assert_eq!(by_itself, 0, "the same view's finished pictures differ by themselves within {span:?}");
    assert!(changed > 200, "the view shows the piece, not the block: {changed} px changed; see {}", path.display());

    // A cube edited shows there, with no hand on the plate.
    let was = hi;
    cube_side(&app, 6.0);
    let (smaller, scene) = wait(&app, "the smaller piece in its scene", || pieces_in_their_scene(&app).filter(|(v, _)| v.len() == 1 && v[0].1[1][2] < was[2] - 1.0));
    assert!(smaller[0].0, "its part draws in the scene: {scene}");
    assert!(smaller[0].1[0][2].abs() < 1e-6, "it stands on the bed: {smaller:?}");
    cube_side(&app, 8.0);
    wait(&app, "the piece as it was", || view_pieces(&app).filter(|v| v.len() == 1 && (v[0].1[1][2] - was[2]).abs() < 1e-6));

    // Unticked: the bed has nothing on it, and the view is still the pieces'.
    let off = format!(r#"{{"request_id":"off","expected":"{}","action":"set","name":"Test 1","on":false}}"#, head(&app));
    call(&app, "test_print", &off).expect("untick");
    wait(&app, "the bed empty", || view_pieces(&app).filter(|v| v.is_empty()));
    let on = format!(r#"{{"request_id":"on","expected":"{}","action":"set","name":"Test 1","on":true}}"#, head(&app));
    call(&app, "test_print", &on).expect("tick");
    wait(&app, "the piece back", || view_pieces(&app).filter(|v| v.len() == 1));

    // Back: the plate, the same picture (the block picked again), in the same render mode.
    choose(&app, false);
    wait(&app, "the plate", || view_pieces(&app).is_none().then_some(()));
    render_mode::wait_shown(&app, "Normal");
    let (_, _, back, path) = settled_picture(&app);
    // Back, the plate's parts are drawn afresh, and their finished picture is the first one, pixel
    // for pixel: anything left of the pieces, a part drawn otherwise or the block's pick lost shows.
    let (off, span) = moved(&plate, &back);
    assert_eq!(off, 0, "the plate as it was: {off} px differ (the pieces' view {changed}), within {span:?} of the view {:?}; see {}", (x0, y0, x1, y1), path.display());
}

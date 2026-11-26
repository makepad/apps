//! The Design workspace edits the model as code: the feature tree lists the
//! program, the code panel holds its text, and a typed edit rebuilds the
//! plate and commits one undo step. The Add menu inserts a ready solid as one
//! step. A text that does not build commits nothing (the panel says where it
//! fails), and undo takes back the last committed step while the panel keeps
//! the text typed into it.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_script::syntax::{self, Node};
use makepad_strict_json::Value;
use makepad_test::{makepad_test, KeyCode, KeyModifiers, Role, Selector, TestApp};
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
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

fn inspect(app: &TestApp) -> Node {
    let reply = text(&call(app, "inspect", "{}").expect("inspect"));
    syntax::parse_value(&reply).expect("inspect is Splash")
}

fn script(v: &Node) -> &Node {
    v.field("scripts").and_then(|f| f.value.items()).and_then(|scripts| scripts.iter().find(|s| s.field("file").and_then(|f| f.value.string()) == Some("block.cad.splash"))).expect("the block script")
}

/// The built box's height: model Y becomes printer Z, reported in millimetres.
fn block_height(v: &Node) -> Option<f64> {
    let parts = v.field("plate")?.value.field("parts")?.value.items()?;
    let block = parts.iter().find(|p| p.field("solid").and_then(|f| f.value.string()) == Some("block/block"))?;
    block.field("size")?.value.items()?.get(2)?.number().map(|h| h * 1000.0)
}

fn wait_height(app: &TestApp, want: f64, what: &str) {
    wait_poll(app, || {
        let h = block_height(&inspect(app));
        if h.is_some_and(|h| (h - want).abs() < 0.01) {
            return Ok(());
        }
        Err(format!("{what}: the box is {h:?} high, expected {want}"))
    })
}

fn wait_name(app: &TestApp, prefix: &str) -> String {
    wait_poll(app, || {
        let found = app.widget_snapshot().nodes().into_iter().filter(|w| w.visible()).map(|w| w.node.name).find(|n| n.starts_with(prefix));
        if let Some(n) = found {
            return Ok(n);
        }
        Err(format!("nothing says \"{prefix}…\""))
    })
}

/// Replace the whole code with `code`, as typed (the code is an area of its own).
fn retype(app: &TestApp, code: &str) {
    app.locator(Selector::id("code")).wait_visible().click();
    app.press_key_with_modifiers(KeyCode::KeyA, KeyModifiers { logo: cfg!(target_os = "macos"), control: !cfg!(target_os = "macos"), ..Default::default() });
    app.type_text(code);
}

/// Bring a dock panel forward: a tab of its area's stack (Features shares one with Plates and Parts).
fn show_tab(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Tab).name(name)).wait_visible().click();
}

#[makepad_test]
fn the_model_is_edited_as_code(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || {
        call(&app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}"))
    });
    let reply = text(&line);
    let head = head_of(&reply);
    let source = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let block = m.box({size: [40mm 30mm 40mm]})
object(block)
"##;
    let batch = format!(
        r#"{{"request_id":"code-1","expected":{head:?},"script":"block.cad.splash","source":{}}}"#,
        Value::Str(source.to_string()).to_json(),
    );
    let reply = text(&call(&app, "script", &batch).expect("script"));
    let head = head_of(&reply);
    let edits = r#"place("block", name: "Block")"#;
    let batch = format!(r#"{{"request_id":"code-1-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let reply = text(&call(&app, "place", &batch).expect("place"));
    assert!(reply.contains("solid: \"block/block\""), "{reply}");

    // ---- the tree lists the program; picking a feature selects it ----
    // Design is the workspace the app opens in.
    show_tab(&app, "Features");
    let row = Selector::role(Role::ListItem).name("base Box block level 1");
    app.locator(row.clone()).wait_visible().select();
    wait_name(&app, "1 feature");

    // ---- a typed edit rebuilds and commits ----
    let code = script(&inspect(&app)).field("source").and_then(|f| f.value.string()).expect("the script source").to_string();
    let taller = code.replace("[40mm 30mm 40mm]", "[40mm 45mm 40mm]");
    assert_ne!(taller, code, "the box's size is in the code: {code}");
    retype(&app, &taller);
    wait_height(&app, 45.0, "after typing");
    let built = inspect(&app);
    assert_eq!(script(&built).field("source").and_then(|f| f.value.string()), Some(taller.as_str()), "the typed code is the script's source");
    assert_eq!(built.field("can_undo").and_then(|f| f.value.ident()), Some("true"), "a typed edit commits an undo step");

    // ---- the Add menu inserts a ready solid (sketch + extrude) as one step ----
    show_tab(&app, "Features");
    app.locator(Selector::role(Role::Button).name("Add feature")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("From a sketch")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Extrude")).wait_visible().press();
    let extruded = |v: &Node| script(v).field("solids").and_then(|f| f.value.items()).expect("the solids").iter().any(|s| s.field("type").and_then(|f| f.value.name()) == Some("Extrude"));
    let added = wait_poll(&app, || {
        let reply = inspect(&app);
        if extruded(&reply) {
            return Ok(reply);
        }
        Err("no Extrude solid was added".to_string())
    });
    wait_name(&app, "2 features");
    let added_code = script(&added).field("source").and_then(|f| f.value.string()).expect("the script source").to_string();
    let added_height = block_height(&added);

    // ---- a text that does not build commits nothing and says so ----
    // ("Not built" is said once its build has failed: a commit would have come before it)
    retype(&app, &added_code.replace("m.box({", "m.boxx({"));
    wait_name(&app, "Not built");
    let failed = inspect(&app);
    assert_eq!(block_height(&failed), added_height, "a failing text must not commit");
    assert_eq!(script(&failed).field("source").and_then(|f| f.value.string()), Some(added_code.as_str()), "the failed source was not committed");

    // ---- undo takes back the last committed step (the Add); the panel keeps the typed text ----
    app.locator(Selector::id("undo").nth(0)).press();
    let undone = wait_poll(&app, || {
        let reply = inspect(&app);
        if !extruded(&reply) {
            return Ok(reply);
        }
        Err("undo did not take back the Add".to_string())
    });
    assert_eq!(script(&undone).field("source").and_then(|f| f.value.string()), Some(taller.as_str()), "undo goes back to the typed edit's program");
    assert!(block_height(&undone).is_some_and(|h| (h - 45.0).abs() < 0.01), "undo keeps the typed edit's 45 mm box");
    wait_name(&app, "Not built");
    // typing the program again leaves nothing uncommitted
    retype(&app, &taller);
    wait_name(&app, "1 feature");
}

/// The Steps strip's chips by their names, with whether each is selected.
fn step_chips(app: &TestApp) -> Vec<(String, bool)> {
    app.widget_snapshot().nodes().into_iter()
        .filter(|w| w.visible() && w.node.role == Role::ListItem && w.node.name.starts_with("Step "))
        .map(|w| (w.node.name.clone(), w.node.states.contains(makepad_test::States::SELECTED)))
        .collect()
}

/// The strip's chip `n` of `of`, once the strip lists them.
fn step_chip(app: &TestApp, n: usize, of: usize) -> (String, bool) {
    let head = format!("Step {n} of {of}: ");
    wait_poll(app, || step_chips(app).into_iter().find(|(name, _)| name.starts_with(&head)).ok_or_else(|| format!("no chip says \"{head}…\": {:?}", step_chips(app))))
}

#[makepad_test]
fn a_step_shows_the_part_as_it_was_built(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || call(&app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}")));
    let head = head_of(&text(&line));
    let source = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let base = m.brep.box({size: [40mm 30mm 40mm]})
let eased = m.brep.fillet(base, 2mm)
let hole = m.brep.cylinder({radius: 4mm height: 50mm})
let bracket = m.brep.subtract(eased, hole)
object(bracket)
"##;
    let batch = format!(r#"{{"request_id":"steps-1","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    let edits = r#"place("block", name: "Bracket")"#;
    let batch = format!(r#"{{"request_id":"steps-2","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let reply = text(&call(&app, "place", &batch).expect("place"));
    assert!(reply.contains("solid: \"block/bracket\""), "{reply}");

    // ---- the strip lists the selected part's steps, named as a person says them ----
    show_tab(&app, "Features");
    let root = wait_poll(&app, || {
        app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.role == Role::ListItem && w.node.name.starts_with("base ") && w.node.name.ends_with("bracket level 1")).map(|w| w.node.name.clone()).ok_or("no feature row for the bracket".to_string())
    });
    app.locator(Selector::role(Role::ListItem).name(root)).select();
    let (first, _) = step_chip(&app, 1, 3);
    let (second, _) = step_chip(&app, 2, 3);
    let (third, _) = step_chip(&app, 3, 3);
    assert!(first.starts_with("Step 1 of 3: Box") && first.contains(", base, line 4"), "the first step is the box: {first}");
    assert!(second.starts_with("Step 2 of 3: Round 2 mm, eased, line 5"), "the second step is the round, in the tools' words and display units: {second}");
    assert!(third.starts_with("Step 3 of 3: Cut") && third.contains(", bracket, line 7") && third.contains("−hole ×1"), "the last step is the cut, with its operand badge: {third}");
    wait_name(&app, "Showing all 3 steps");

    // ---- pressing a step shows the part as it was there, once its mesh is made ----
    app.locator(Selector::role(Role::ListItem).name(second.clone())).select();
    wait_name(&app, "Showing step 2 of 3");
    assert!(step_chip(&app, 2, 3).1, "the pressed step is selected in the strip");
    wait_poll(&app, || {
        let rows: Vec<_> = app.widget_snapshot().nodes().into_iter().filter(|w| w.node.role == Role::ListItem && !w.node.name.starts_with("Step ") && w.node.name.contains(" eased level ")).collect();
        match rows.first() {
            Some(row) if row.node.states.contains(makepad_test::States::SELECTED) => Ok(()),
            Some(_) => Err("the Features row of the step is not selected yet".to_string()),
            None => Err("no Features row for the step".to_string()),
        }
    });

    // ---- pressing the last step goes back to the whole part ----
    app.locator(Selector::role(Role::ListItem).name(third)).select();
    wait_name(&app, "Showing all 3 steps");

    // ---- a committed edit that keeps the step's id keeps it shown, in its new words ----
    app.locator(Selector::role(Role::ListItem).name(second)).select();
    wait_name(&app, "Showing step 2 of 3");
    let before = head_of(&text(&call(&app, "inspect", r#"{"program":false}"#).expect("inspect")));
    app.press_key(KeyCode::ReturnKey);
    wait_name(&app, "Editing step 2 of 3: Round 2 mm · Esc: done");
    let radius = Selector::role(Role::SpinButton).name("Radius");
    let controls = app.widget_snapshot().nodes();
    let radii: Vec<_> = controls.iter().filter(|w| w.visible() && w.node.role == Role::SpinButton && w.node.name == "Radius").collect();
    assert_eq!(radii.len(), 1, "only the round's radius is shown");
    assert_eq!(radii[0].node.range.unwrap().now, 2.0, "the 4 mm hole's radius is filtered out");
    app.locator(radius.clone()).set_value(3);
    wait_name(&app, "Step 2 of 3: Round 3 mm, eased,");
    wait_name(&app, "Editing step 2 of 3: Round 3 mm · Esc: done");
    wait_name(&app, "Showing step 2 of 3");
    let after = head_of(&text(&call(&app, "inspect", r#"{"program":false}"#).expect("inspect")));
    let generation = |head: &str| fab_model::Head::parse(head).unwrap().0;
    assert_eq!(generation(&after), generation(&before) + 1, "the radius edit committed once");
    app.press_key(KeyCode::Escape);
    let hole = Selector::role(Role::DisclosureButton).name("Hole");
    app.locator(hole).wait_visible().expand();
    wait_poll(&app, || {
        let nodes = app.widget_snapshot().nodes();
        nodes.iter().any(|w| w.node.role == Role::SpinButton && w.node.name == "Radius" && w.node.range.is_some_and(|r| r.now == 4.0))
            .then_some(()).ok_or_else(|| "Esc should restore the hole's 4 mm radius row".to_string())
    });
    app.locator(Selector::id("undo").nth(0)).press();
    wait_name(&app, "Step 2 of 3: Round 2 mm, eased,");
    assert_eq!(fab_model::Head::parse(&head_of(&text(&call(&app, "inspect", "{}").unwrap()))).unwrap().1,
        fab_model::Head::parse(&before).unwrap().1, "one undo restores the exact pre-edit document");

    // A code edit can keep the step and connect its radius to a later cutter.
    let code = script(&inspect(&app)).field("source").and_then(|f| f.value.string()).expect("the script source").to_string();
    let rounder = code.replace("let eased = m.brep.fillet(base, 2mm)", "let r = param(3mm, min: 1mm, max: 4mm, label: \"Round radius\")\nlet eased = m.brep.fillet(base, r)")
        .replace("height: 50mm", "height: (4mm - r) * 50");
    assert_ne!(rounder, code, "the round's radius is in the code: {code}");
    retype(&app, &rounder);
    let (renamed, _) = wait_poll(&app, || step_chips(&app).into_iter().find(|(name, _)| name.starts_with("Step 2 of 3: Round 3 mm")).ok_or_else(|| format!("the chip does not say 3 mm yet: {:?}", step_chips(&app))));
    assert!(renamed.contains(", eased,"), "the step is the same solid: {renamed}");
    wait_name(&app, "Showing step 2 of 3");
    app.locator(Selector::role(Role::ListItem).name(renamed)).select();
    app.press_key(KeyCode::ReturnKey);
    wait_name(&app, "Editing step 2 of 3: Round 3 mm");
    let round_radius = Selector::role(Role::SpinButton).name("Round radius");
    let before = head_of(&text(&call(&app, "inspect", "{}").unwrap()));
    // The round itself fits at 4 mm. Its cut's cylinder now has zero height.
    app.locator(round_radius.clone()).wait_visible().set_value(4);
    wait_poll(&app, || step_chips(&app).into_iter().find(|(name, _)| name.starts_with("Step 3 of 3: Cut") && name.contains("breaks: "))
        .ok_or_else(|| format!("the downstream cut should name its failure: {:?}", step_chips(&app))));
    wait_name(&app, "Round 4 mm would break step 3 (Cut, bracket):");
    assert_eq!(head_of(&text(&call(&app, "inspect", "{}").unwrap())), before, "a refused typed value adds no undo step");
    app.locator(round_radius.clone()).set_value(2);
    wait_name(&app, "Editing step 2 of 3: Round 2 mm");
    assert!(!step_chip(&app, 3, 3).0.contains("breaks: "), "a good commit clears the cut's failure");

    // Drag to the same failure. Only releasing it commits the greatest buildable tenth.
    let row = app.locator(round_radius);
    let width = row.snapshot().node.bounds.w as f64;
    let before = head_of(&text(&call(&app, "inspect", "{}").unwrap()));
    let mut drag = row.press_and_drag();
    drag.move_by(width, 0.0);
    wait_name(&app, "Showing step 2 of 3");
    assert_eq!(head_of(&text(&call(&app, "inspect", "{}").unwrap())), before, "the live drag adds no history");
    drag.release();
    wait_name(&app, "Round held at 3.9 mm: 4 mm would break step 3 (Cut, bracket):");
    wait_name(&app, "Editing step 2 of 3: Round 3.9 mm");
    assert!(step_chip(&app, 3, 3).0.contains("breaks: "), "the failure mark survives the held value's plate refresh");
    app.press_key(KeyCode::Escape);
    wait_poll(&app, || (!step_chip(&app, 3, 3).0.contains("breaks: ")).then_some(()).ok_or_else(|| "Esc should clear the held value's failure mark".to_string()));

    app.locator(Selector::role(Role::ListItem).name(step_chip(&app, 2, 3).0)).select();
    wait_name(&app, "Showing step 2 of 3");
    app.locator(Selector::role(Role::Button).name("Roll back to step 2 of 3")).wait_visible().press();
    wait_name(&app, "Showing all 2 steps");
    assert_eq!(step_chips(&app).len(), 2);
    assert!(step_chip(&app, 2, 2).0.contains("Round 3.9 mm, bracket"));
    app.locator(Selector::id("undo").nth(0)).press();
    wait_name(&app, "Showing all 3 steps");
    assert_eq!(step_chips(&app).len(), 3);
}

#[makepad_test]
fn round_at_a_marked_step_and_undo(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    // A marked box with another box added later. Plan with a named step pick, then run
    // the person's Round command. The viewport hit and the ghost miss are checked live.
    let before = head_of(&text(&wait_poll(&app, || call(&app, "inspect", "{}"))));
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet leg1 = m.brep.box({size: [10mm 10mm 10mm] at: [-20mm 0 0]})\nlet leg2 = m.brep.box({size: [10mm 10mm 10mm] at: [20mm 0 0]})\nlet bracket = m.brep.union(leg1, leg2)\nobject(bracket)\n";
    let batch = format!(r#"{{"request_id":"step-round","expected":{before:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
    let reply = text(&call(&app, "script", &batch).expect("two legs"));
    let committed = head_of(&reply);
    assert_eq!(script(&inspect(&app)).field("source").unwrap().value.string(), Some(source), "the two-leg fixture is the committed script: {reply}");
    show_tab(&app, "Features");
    let root = wait_poll(&app, || app.widget_snapshot().nodes().into_iter()
        .find(|w| w.visible() && w.node.role == Role::ListItem && w.node.name.ends_with("bracket level 1"))
        .map(|w| w.node.name).ok_or_else(|| "no root feature for the two legs".to_string()));
    app.locator(Selector::role(Role::ListItem).name(root)).select();
    let (first, _) = step_chip(&app, 1, 2);
    app.locator(Selector::role(Role::ListItem).name(first)).select();
    wait_name(&app, "Showing step 1 of 2");
    let pick = text(&call(&app, "select", r#"{"set":{"solid":"block/bracket","on":"leg1","select":"m.edge(leg1.front, leg1.top)"}}"#).expect("select on leg1"));
    assert!(pick.contains("block/leg1"), "the edge is on the marked leg: {pick}");
    app.press_key(KeyCode::F3);
    app.type_text("Round the Picked Edges");
    app.press_key(KeyCode::ReturnKey);
    wait_name(&app, "Round handle value");
    app.type_text("1");
    app.press_key(KeyCode::ReturnKey);
    wait_name(&app, "Showing step 2 of 3");
    let (rounded, selected) = step_chip(&app, 2, 3);
    assert!(selected && rounded.contains("Round 1 mm, leg1"), "the marker shows the new round at the reborn leg1: {rounded}");
    let edited = inspect(&app);
    let code = script(&edited).field("source").unwrap().value.string().unwrap();
    assert!(code.contains("let leg1_box = m.brep.box") && code.contains("let leg1 = m.brep.fillet(leg1_box, 1mm"), "Round inserts at the marked leg: {code}");
    app.locator(Selector::id("undo").nth(0)).press();
    wait_name(&app, "Showing step 1 of 2");
    let (original, selected) = step_chip(&app, 1, 2);
    assert!(selected && original.starts_with("Step 1 of 2: Box") && original.contains(", leg1,"), "undo shows the box where Round began: {original}");
    let restored = inspect(&app);
    assert_eq!(script(&restored).field("source").unwrap().value.string(), Some(source), "one undo restores the exact script");
    assert_eq!(fab_model::Head::parse(restored.field("head").unwrap().value.string().unwrap()).unwrap().1, fab_model::Head::parse(&committed).unwrap().1, "one undo restores the same content");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

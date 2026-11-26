//! One document history for every edit path: an AI program, the part
//! panel's generated parameter control, a typed program edit, the feature
//! tree's delete, the Add menu, the Part panel's position field and the move
//! gizmo each commit one step. The caption's undo takes that step back (the
//! model, the program's text and the feature tree's solids) and redo
//! puts it back.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
#[path = "common/workspace_row.rs"]
mod workspace_row;
use idle_wait::wait_poll;
use makepad_script::syntax::{self, Kind};
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

/// The exact script and project sources and the solid rows the feature tree must show.
#[derive(Debug, PartialEq)]
struct Program {
    source: String,
    project: String,
    rows: Vec<String>,
}

fn program(app: &TestApp) -> Program {
    let reply = text(&call(app, "inspect", "{}").expect("inspect"));
    let v = syntax::parse_value(&reply).expect("inspect is Splash");
    let script = v.field("scripts").and_then(|f| f.value.items()).expect("the scripts").iter().find(|s| s.field("file").and_then(|f| f.value.string()) == Some("post.cad.splash"));
    let source = script.map(|s| s.field("source").and_then(|f| f.value.string()).expect("the script source")).unwrap_or_default().to_string();
    let project = v.field("project").and_then(|f| f.value.string()).expect("the project source").to_string();
    let rows = script.into_iter().flat_map(|s| s.field("solids").and_then(|f| f.value.items()).expect("the solids")).map(|s| {
        let kind = s.field("type").and_then(|f| f.value.name()).expect("solid type");
        let id = s.field("id").and_then(|f| f.value.string()).expect("solid id");
        let proto = makepad_model_script::lang::protos::proto(kind).expect("the inspected solid has a prototype");
        format!("{} {id}", fab_model::steps::verb(proto.call))
    }).collect();
    Program { source, project, rows }
}

/// A declared length's value in millimetres, from its instance override or param call.
fn param_value(p: &Program, key: &str) -> Option<f64> {
    let project = syntax::parse(&p.project).ok()?;
    for loc in project.find_calls("script") {
        let args = project.node(&loc)?.call("script")?;
        if args.first()?.string() != Some("post.cad.splash") {
            continue;
        }
        for arg in &args[1..] {
            if let Kind::Entry { name, value, .. } = &arg.kind {
                if name == "params" {
                    if let Some(v) = value.field(key).and_then(|f| f.value.number()) {
                        return Some(v * 1000.0);
                    }
                }
            }
        }
    }
    let tree = syntax::parse(&p.source).ok()?;
    let loc = tree.find_let(key)?;
    let Kind::Let { value: Some(value), .. } = &tree.node(&loc)?.kind else { return None };
    value.call("param")?.first()?.number().map(|v| v * 1000.0)
}

/// The post's explicit placement on the bed, in millimetres.
fn post_x(source: &str) -> Option<f64> {
    let tree = syntax::parse(source).ok()?;
    for loc in tree.find_calls("place") {
        let args = tree.node(&loc)?.call("place")?;
        if args.first()?.string() != Some("post") {
            continue;
        }
        for arg in &args[1..] {
            if let Kind::Entry { name, value, .. } = &arg.kind {
                if name == "at" {
                    return value.items()?.first()?.number().map(|x| x * 1000.0);
                }
            }
        }
    }
    None
}

/// The feature tree's rows ("type id"): the program's solids.
fn tree(app: &TestApp) -> Vec<String> {
    // The tree is the Design workspace's.
    if !show_tab(app, "Features") {
        workspace_row::press(app, "3D modeling");
        show_tab(app, "Features");
    }
    app.widget_snapshot()
        .nodes()
        .into_iter()
        .filter(|w| w.visible() && w.node.role == Role::ListItem)
        .map(|w| w.node.name)
        .collect()
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    wait_poll(app, || {
        if !ok() { Err(format!("timed out waiting for {what}")) } else { Ok(()) }
    });
}

/// The tree's row for a solid ("type id"): its label is "<role> <type> <id> level <n>".
fn lists(shown: &[String], row: &str) -> bool {
    let wanted = format!("{row} level ");
    shown.iter().any(|label| label.contains(&wanted))
}

/// The tree's label of the row for a solid.
fn label_of(app: &TestApp, row: &str) -> String {
    let wanted = format!("{row} level ");
    tree(app).into_iter().find(|label| label.contains(&wanted)).unwrap_or_else(|| panic!("no tree row for {row}"))
}

/// The program is `want`, and the tree lists it.
fn wait_program(app: &TestApp, want: &Program, other: &Program, what: &str) {
    wait_until(app, &format!("the program {what}"), || &program(app) == want);
    // The tree lists exactly `want`: its rows all show, and none of `other`'s that `want` lacks.
    let rows = &want.rows;
    let gone: Vec<&String> = other.rows.iter().filter(|r| !rows.contains(r)).collect();
    app.try_wait_until_within(HANG, || {
        let shown = tree(app);
        if rows.iter().all(|r| lists(&shown, r)) && !gone.iter().any(|r| lists(&shown, r)) {
            return None;
        }
        Some(format!("waiting for the tree {what}"))
    }).unwrap_or_else(|why| {
        for w in app.widget_snapshot().nodes().into_iter().filter(|w| w.node.role == Role::ListItem || w.node.role == Role::Tab) {
            println!("[fab-undo] {:?} {:?} visible={} path={}", w.node.role, w.node.name, w.visible(), w.node.path);
        }
        let shot = app.screenshot();
        println!("[fab-undo] grab {}", shot.display());
        panic!("{why}; the tree {what}: {:?}, expected {rows:?}", tree(app));
    });
}

/// Bring a dock panel forward: a tab of its area's stack (Features shares one with Plates and Parts).
fn show_tab(app: &TestApp, name: &str) -> bool {
    let shown = app.widget_snapshot().nodes().into_iter().any(|w| w.visible() && w.node.role == Role::Tab && w.node.name == name);
    if shown {
        app.locator(Selector::role(Role::Tab).name(name)).click();
    }
    shown
}

fn undo(app: &TestApp) {
    app.locator(Selector::id("undo").nth(0)).press();
}

fn redo(app: &TestApp) {
    app.locator(Selector::id("redo").nth(0)).press();
}

/// One edit path: `before` the edit, the edit, then undo restores `before` and redo restores
/// what the edit made.
fn round_trip(app: &TestApp, name: &str, edit: impl FnOnce(), changed: impl Fn(&Program) -> bool) {
    let before = program(app);
    edit();
    wait_until(app, &format!("{name} to change the program"), || changed(&program(app)));
    // The edit's last write: the program reads the same twice in a row.
    let mut after = program(app);
    wait_until(app, &format!("{name} to finish"), || {
        let now = program(app);
        let same = now == after;
        after = now;
        same
    });
    assert_ne!(after, before, "{name}: the edit changed nothing");
    undo(app);
    wait_program(app, &before, &after, &format!("restored after undoing {name}"));
    redo(app);
    wait_program(app, &after, &before, &format!("re-applied after redoing {name}"));
    println!("[fab-undo] {name}: undo and redo ok ({} -> {} solids)", before.rows.len(), after.rows.len());
}

fn pick_part(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).name(name)).wait_visible().click();
    // The tools start at Select: the move gizmo is Move's.
    app.locator(Selector::id("t_move")).wait_visible().click();
}

#[makepad_test]
fn every_edit_path_undoes_and_redoes_one_step(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || {
        call(&app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}"))
    });
    let reply = text(&line);
    let head = head_of(&reply);

    // ---- an AI program ----
    let source = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let h = param(30mm, min: 5mm, max: 120mm, step: 0.5mm, label: "Post height")
let r = param(8mm, min: 2mm, max: 40mm, step: 0.1mm, label: "Post radius", kind: "radius")
let post = m.cylinder({radius: r height: h segments: 48})
object(post)
"##;
    let batch = format!(
        r#"{{"request_id":"undo-1","expected":{head:?},"script":"post.cad.splash","source":{}}}"#,
        Value::Str(source.to_string()).to_json(),
    );
    round_trip(&app, "an AI program", || {
        let reply = text(&call(&app, "script", &batch).expect("script"));
        assert!(reply.contains("solid: \"post/post\""), "{reply}");
    }, |p| p.source == source && p.rows == ["Cylinder post"]);
    let head = head_of(&text(&call(&app, "inspect", "{}").expect("inspect")));
    let edits = r#"place("post", name: "Post")"#;
    let batch = format!(r#"{{"request_id":"undo-1-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let reply = text(&call(&app, "place", &batch).expect("place"));
    assert!(reply.contains("solid: \"post/post\""), "{reply}");

    // ---- the part panel's generated parameter control ----
    pick_part(&app, "Post");
    round_trip(&app, "the parameter control", || {
        app.locator(Selector::role(Role::SpinButton).name("Post radius")).wait_visible().set_value(11);
    }, |p| param_value(p, "r").is_some_and(|v| (v - 11.0).abs() < 1e-6));

    // ---- a typed program edit ----
    workspace_row::press(&app, "3D modeling");
    round_trip(&app, "a typed program edit", || {
        let before = program(&app).source;
        let code = before.replace("param(30mm", "param(40mm");
        assert_ne!(code, before, "the height's param call is in the source");
        app.locator(Selector::id("code")).wait_visible().click();
        let primary = KeyModifiers { logo: cfg!(target_os = "macos"), control: !cfg!(target_os = "macos"), ..Default::default() };
        app.press_key_with_modifiers(KeyCode::KeyA, primary);
        app.type_text(&code);
    }, |p| param_value(p, "h").is_some_and(|v| (v - 40.0).abs() < 1e-6));

    // ---- the feature tree's delete removes the solid and its emission ----
    let before_delete = program(&app);
    round_trip(&app, "the tree's delete", || {
        let _ = show_tab(&app, "Features");
        let label = label_of(&app, "Cylinder post");
        app.locator(Selector::role(Role::ListItem).name(label)).wait_visible().select();
        app.locator(Selector::role(Role::Button).name("Delete feature")).wait_visible().press();
    }, |p| p.rows.is_empty() && !p.source.contains("let post =") && !p.source.contains("object(post)"));
    undo(&app);
    wait_until(&app, "the post and its placement back", || program(&app) == before_delete);

    // ---- the Add menu ----
    round_trip(&app, "the Add menu", || {
        let _ = show_tab(&app, "Features");
        app.locator(Selector::role(Role::Button).name("Add feature")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name("From a sketch")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name("Extrude")).wait_visible().press();
    }, |p| p.rows.iter().any(|r| r.starts_with("Extrude ")));

    // ---- the Part panel's position field ----
    pick_part(&app, "Post");
    round_trip(&app, "the position field", || {
        app.locator(Selector::role(Role::SpinButton).name("X")).wait_visible().set_value(120);
    }, |p| post_x(&p.project).is_some_and(|x| (x - 120.0).abs() < 1e-6));

    // ---- the move gizmo ----
    pick_part(&app, "Post");
    round_trip(&app, "the move gizmo", || {
        // The gizmo starts as Move.
        app.locator(Selector::role(Role::Button).name("Move freely")).wait_visible().drag_by(60.0, 0.0);
    }, |p| post_x(&p.project).is_some_and(|x| (x - 120.0).abs() > 1e-3));

}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

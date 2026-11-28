//! The Part panel's Colours list: one row per colour the part prints with, its base colour
//! first and marked as the base, then each painted colour once. A toy of 1, 4 and 9 colours
//! shows 1, 4 and 9 rows. A row's slot is a dropdown of the printer's trays: a pick there
//! prints that colour of the part from the tray (as the part menu's Material does: in the
//! colour already there, or a new one saved there), and one undo step takes it back.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
#[path = "common/part_editor.rs"]
mod part_editor;
#[path = "common/parts_tab.rs"]
mod parts_tab;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{makepad_test, Role, Selector, TestApp};
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
    let reply = makepad_script::syntax::parse_value(reply).unwrap_or_else(|e| panic!("a Splash reply: {e:?}\n{reply}"));
    reply.field("head").and_then(|f| f.value.string()).unwrap_or_else(|| panic!("reply has a head: {reply:?}")).to_string()
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    wait_poll(app, || {
        if !ok() { Err(format!("timed out waiting for {what}")) } else { Ok(()) }
    });
}

fn head(app: &TestApp) -> String {
    wait_poll(app, || {
        call(app, "inspect", "{}").map(|line| head_of(&text(&line))).map_err(|e| format!("the fab tools never appeared: {e}"))
    })
}

/// The toy's script: a box, and `paints` painted colours, each a small sphere filled into its top.
fn toy(paints: usize) -> String {
    let mut s = String::from("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet toy = m.box({size: [60mm, 40mm, 40mm]})\nobject(toy)\n");
    let hues = [[0.9, 0.3, 0.05], [0.4, 0.1, 0.7], [0.1, 0.3, 0.9], [0.1, 0.7, 0.2], [0.9, 0.8, 0.1], [0.8, 0.1, 0.4], [0.2, 0.7, 0.8], [0.5, 0.3, 0.1]];
    for (i, [r, g, b]) in hues.iter().take(paints).enumerate() {
        let x = -24.0 + 6.5 * i as f64;
        s.push_str(&format!("let p{i} = Material{{albedo: [{r}, {g}, {b}] name: \"Paint {}\"}}\nm.fill(m.sphere({{radius: 2.5mm at: [{x}mm, 20mm, 0]}}), p{i})\n", i + 2));
    }
    s
}

fn script(app: &TestApp, paints: usize) {
    let h = head(app);
    let batch = format!(r#"{{"request_id":"colours-{paints}","expected":{h:?},"script":"toy.cad.splash","source":{}}}"#, Value::Str(toy(paints)).to_json());
    let reply = call(app, "script", &batch).expect("script");
    assert!(text(&reply).contains("head"), "{reply}");
}

/// The Colours list's rows in the list's order, top to bottom: each row's name and its spoken
/// text, also the rows the Part panel's scroll holds below its fold (there to read, without a
/// place on screen; Parameters shares the column).
fn rows(app: &TestApp) -> Vec<(String, String)> {
    app.widget_snapshot()
        .nodes()
        .into_iter()
        .filter(|n| !n.states.contains(makepad_test::States::HIDDEN))
        .filter(|n| n.node.role == Role::Group && n.node.path.contains("colours") && n.node.value.contains(": "))
        .map(|n| (n.node.name.clone(), n.node.value.clone()))
        .collect()
}

/// Wait until the rows read so; a timeout shows them.
fn wait_rows(app: &TestApp, what: &str, ok: impl Fn(&[(String, String)]) -> bool) {
    wait_poll(app, || {
        let rows = rows(app);
        if ok(&rows) { Ok(()) } else { Err(format!("waiting for {what}: {rows:#?}")) }
    });
}

fn names(app: &TestApp) -> Vec<String> {
    rows(app).into_iter().map(|(name, _)| name).collect()
}

#[makepad_test]
fn the_colours_list_has_a_row_per_colour_base_first(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    script(&app, 0);
    let h = head(&app);
    let edits = r#"color("Base", rgb: #xe8e0d0)
place("toy", name: "Toy", color: "Base")"#;
    call(&app, "place", &format!(r#"{{"request_id":"colours-place","expected":{h:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json())).expect("place");
    parts_tab::open(&app);
    app.locator(Selector::role(Role::TreeItem).name("Toy")).wait_visible().click();
    part_editor::open(&app);

    // One colour: the base, marked so.
    wait_until(&app, "the base row", || names(&app) == ["Base"]);
    assert!(rows(&app)[0].1.contains("(the part's base colour)"), "{:?}", rows(&app));
    app.screenshot();

    // Four and nine colours: the base first, then the painted ones in order, each once.
    for paints in [3, 8] {
        script(&app, paints);
        let want: Vec<String> = std::iter::once("Base".to_string()).chain((0..paints).map(|i| format!("Paint {}", i + 2))).collect();
        wait_until(&app, &format!("{} rows", paints + 1), || names(&app) == want);
        let rows = rows(&app);
        assert!(rows[0].1.contains("(the part's base colour)") && !rows[1..].iter().any(|r| r.1.contains("base colour")), "{rows:?}");
        app.screenshot();
    }

    // Each ! says why, a ≠ that the spool is not the design's colour: both read as buttons.
    for mark in app.widget_snapshot().nodes().into_iter().filter(|n| n.visible() && n.node.role == Role::Button && n.node.path.contains("colours")) {
        if let Some((_, why)) = mark.node.name.split_once(" warning: ") {
            assert!(!why.trim().is_empty(), "a warning mark without its reason: {:?}", mark.node.name);
        }
    }

    // The slot of "Paint 3" is a dropdown of the trays; a pick prints that paint from the tray
    // (in a colour saved there), an undo takes it back.
    let before = rows(&app)[2].1.clone();
    app.locator(Selector::role(Role::Button).text_contains("Paint 3: ")).wait_visible().press();
    let item = app.locator(Selector::role(Role::MenuItem).any_window().text_contains("A4")).wait_visible();
    app.screenshot();
    item.click();
    // It prints in the colour on A4 (one the automatic mapping put there, now saved there), in
    // Paint 3's place in the list.
    wait_rows(&app, "Paint 3's paint on A4", |rows| rows.len() >= 8 && rows[2].1.contains("A4, saved") && !rows.iter().any(|r| r.0 == "Paint 3"));
    app.locator(Selector::id("undo").nth(0)).press();
    wait_rows(&app, "the undo", |rows| rows.get(2).is_some_and(|r| r.1 == before));
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

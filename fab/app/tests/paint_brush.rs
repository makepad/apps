//! The brush in a real window, one boot: a small exact part takes a stroke; then the demo plate
//! opened, the paint tool chosen, strokes dragged across the parts (the bracket is a CSG mesh, the
//! vase a field): every stroke lands as one record of the part's strokes file (its script reads it
//! with `m.strokes`) and the grab shows the paint; a smooth-region flood of the vase (`fab.fill`)
//! shows in blue; and the noise vase's procedural paint shows.
//!
//! Visible: `MAKEPAD_TEST_VISIBLE=1` on a machine with a screen; offscreen otherwise.

#[path = "common/picture.rs"]
mod picture;
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

fn splash(text: &str) -> Node {
    syntax::parse_value(text).expect("a Splash reply")
}

/// One mouse event; moves are queued without waiting for a frame (the events keep their order),
/// a press or a release is answered once a frame has drawn it.
fn mouse(client: &makepad_remote::Client, kind: &str, x: f64, y: f64) {
    let wait = (kind != "move") as u8;
    let argv: Vec<String> = ["mouse".to_string(), format!("k={kind}"), format!("x={x:.1}"), format!("y={y:.1}"), "b=0".into(), format!("wait={wait}")].into();
    makepad_remote::app::run(client, &argv, &mut |_| true).expect("mouse");
}

fn seq(app: &TestApp) -> u64 {
    app.log_since(0).map(|t| t.last_seq).unwrap_or(0)
}

/// A grab's pixels (RGB) and size.
fn rgb(path: &std::path::Path) -> (usize, usize, Vec<[u8; 3]>) {
    use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
    let bytes = std::fs::read(path).expect("grab");
    let mut d = makepad_zune_png::PngDecoder::new(ZCursor::new(&bytes));
    let px = d.decode_raw().expect("decode");
    let (w, h) = d.dimensions().expect("size");
    let c = px.len() / (w * h);
    (w, h, px.chunks_exact(c).map(|p| [p[0], p[1], p[2]]).collect())
}

/// Blue paint as lit in the view: blue clearly over red and green (the sky lifts it, so it is
/// not the albedo's deep blue).
fn is_blue(p: &[u8; 3]) -> bool {
    let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
    b > 90 && b > r + 60 && b > g + 30
}

/// The grab once the view shows its finished picture (the camera glides to a framing first).
fn still_grab(app: &TestApp) -> Vec<[u8; 3]> {
    rgb(&picture::finished_screenshot(app)).2
}

/// Percent of the grab that is blue now and was not in `before`.
fn new_blue(before: &[[u8; 3]], now: &[[u8; 3]]) -> f64 {
    100.0 * now.iter().zip(before).filter(|(n, b)| is_blue(n) && !is_blue(b)).count() as f64 / now.len() as f64
}

/// Percent of the grab that is blue, and that is yellow (the noise vase's two filaments).
fn noise_shares(px: &[[u8; 3]]) -> (f64, f64) {
    let yellow = |p: &[u8; 3]| p[0] > 150 && p[1] > 130 && p[2] < 100 && p[0] as i32 > p[2] as i32 + 70;
    let (blue, yellow) = (px.iter().filter(|p| is_blue(p)).count(), px.iter().filter(|p| yellow(p)).count());
    (100.0 * blue as f64 / px.len() as f64, 100.0 * yellow as f64 / px.len() as f64)
}

/// The share of the grab that is paint red.
fn red_share(path: &std::path::Path) -> f64 {
    use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
    let bytes = std::fs::read(path).expect("grab");
    let mut d = makepad_zune_png::PngDecoder::new(ZCursor::new(&bytes));
    let px = d.decode_raw().expect("decode");
    let (w, h) = d.dimensions().expect("size");
    let c = px.len() / (w * h);
    let red = px.chunks_exact(c).filter(|p| { let (r, g, b) = (p[0] as f64, p[1] as f64, p[2] as f64); r > 60.0 && r > 1.8 * g && r > 1.8 * b }).count();
    100.0 * red as f64 / (w * h) as f64
}

fn inspect(app: &TestApp) -> Node {
    splash(&text(&call(app, "inspect", r#"{"program":false}"#).expect("inspect")))
}

fn wait_parts(app: &TestApp, n: usize) {
    wait_poll(app, || {
        if inspect(app).field("plate").and_then(|p| p.value.field("parts")).and_then(|f| f.value.items()).is_some_and(|p| p.len() >= n) {
            return Ok(());
        }
        Err("the plate never came".to_string())
    })
}

/// Strokes across the viewport at a few heights; the paint tool keeps the hits on parts.
fn brush(app: &TestApp, rows: &[f64], dabs_per_row: usize) {
    let view = app.locator(Selector::id("viewport")).wait_visible().snapshot();
    let (x0, w) = (view.x() as f64, view.width() as f64);
    let (y0, h) = (view.y() as f64, view.height() as f64);
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect");
    for &row in rows {
        let y = y0 + h * row;
        // short drags across the row (a stroke stays on the part it starts on), from a
        // quarter in: the paint popup sits by the toolbar on the left
        let drags = DRAGS_PER_ROW;
        let per = dabs_per_row.div_ceil(drags);
        for d in 0..drags {
            let at = |i: usize| x0 + w * (0.25 + 0.6 * (d * per + i) as f64 / (drags * per) as f64);
            mouse(&client, "move", at(0), y);
            mouse(&client, "down", at(0), y);
            for i in 0..per {
                mouse(&client, "move", at(i), y + (i as f64 * 0.7).sin() * 6.0);
            }
            mouse(&client, "up", at(per), y);
        }
    }
}

/// What the viewport reports of the recording on display.
fn recording(app: &TestApp) -> Value {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).expect("viewport inspection JSON")
}

/// The drags a brush of `rows` makes (see [`brush`]).
const DRAGS_PER_ROW: usize = 6;

/// What the paint tool did with the drags since log line `mark`, as it logged each: the
/// instances whose parts took a stroke (`bracket` for `bracket/bracket`), and the drags that
/// reached no part.
fn brushed(app: &TestApp, mark: u64) -> (Vec<String>, usize) {
    let lines = app.log_since(mark).expect("the app's log").lines;
    let on = lines.iter().filter_map(|l| l.split_once("fab paint: stroke of ")?.1.split_once(" points on ")).map(|(_, o)| o.split('/').next().unwrap_or(o).to_string()).collect();
    (on, lines.iter().filter(|l| l.contains("fab paint: the drag reached no part")).count())
}

/// Brush `rows`, then wait for what the drags did: each stroke the tool handed the model in its
/// part's strokes file (`bracket.strokes`). The instances a stroke went to, and the sources
/// then. A paint edit the model refuses fails here, saying why.
fn brush_and_land(app: &TestApp, rows: &[f64], dabs_per_row: usize) -> (Vec<String>, String) {
    let mark = seq(app);
    brush(app, rows, dabs_per_row);
    let drags = rows.len() * DRAGS_PER_ROW;
    let on = wait(app, "the paint tool's answer to every drag", || {
        let (on, missed) = brushed(app, mark);
        (on.len() + missed >= drags).then_some(on)
    });
    let sources = wait(app, &format!("the strokes on {on:?} in their scripts"), || {
        let lines = app.log_since(mark).expect("the app's log").lines;
        if let Some(refused) = lines.iter().find(|l| l.contains("fab: paint edit refused")) {
            panic!("the brush's stroke was refused: {refused}");
        }
        let all = sources(app);
        on.iter().all(|i| all.contains(&format!("{i}.strokes"))).then_some(all)
    });
    (on, sources)
}

/// The project as fab.inspect reads it, every file's source included.
fn sources(app: &TestApp) -> String {
    text(&call(app, "inspect", "{}").expect("inspect"))
}

fn wait<T>(app: &TestApp, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    wait_poll(app, || f().ok_or_else(|| format!("waiting for {what}")))
}

fn head_of(reply: &str) -> String {
    splash(reply).field("head").and_then(|f| f.value.string()).expect("a head").to_string()
}

/// An exact part, wide enough for a stroke across the middle of the plate view to find: a rounded
/// block, its top filleted.
const EXACT: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet block = m.brep.extrude({sketch: Rect{size: [120mm 120mm] corner: 10mm} height: 20mm})\nlet housing = m.brep.fillet(block, 3mm, {edges: {top: true}})\nobject(housing)\n";

/// The example opened from the command palette (Cmd/Ctrl+Shift+P), once its script is in the project.
fn open_example(app: &TestApp, query: &str, file: &str) {
    app.press_key(KeyCode::Escape);
    app.press_key_with_modifiers(KeyCode::KeyP, if cfg!(target_os = "macos") { KeyModifiers { shift: true, logo: true, ..Default::default() } } else { KeyModifiers { shift: true, control: true, ..Default::default() } });
    app.type_text(query);
    app.press_key(KeyCode::ReturnKey);
    wait(app, file, || sources(app).contains(file).then_some(()));
}

#[makepad_test]
fn the_brush_paints_mesh_field_and_exact_parts_and_a_flood_and_noise_show(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait(&app, "the fab tools", || call(&app, "inspect", "{}").ok());
    // An exact part takes strokes like the others, once it is drawn: its model worker may still be
    // refining the part, so the strokes queue behind it.
    let head = head_of(&sources(&app));
    let batch = format!(r#"{{"request_id":"exact-script","expected":"{head}","script":"housing.cad.splash","source":{}}}"#, Value::Str(EXACT.to_string()).to_json());
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    let edits = Value::Str(r#"place("housing", name: "Housing", at: [175mm, 160mm])"#.to_string()).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"exact-place","expected":"{head}","edits":{edits}}}"#)).expect("place");
    wait_parts(&app, 1);
    wait(&app, "the housing (exact) drawn, the view settled", || {
        let r = recording(&app);
        (r.get("parts_drawn").and_then(Value::as_u64).is_some_and(|n| n >= 1) && r.get("settled").and_then(Value::as_bool) == Some(true)).then_some(())
    });
    app.locator(Selector::id("t_paint")).click();
    let mark = seq(&app);
    let (on, _) = brush_and_land(&app, &[0.5], 12);
    assert!(on.iter().any(|i| i == "housing"), "a brush across the drawn housing (exact) put no stroke on it: {:?}", brushed(&app, mark));
    app.press_key(KeyCode::Escape);

    let primary = if cfg!(target_os = "macos") { KeyModifiers { shift: true, logo: true, ..Default::default() } } else { KeyModifiers { shift: true, control: true, ..Default::default() } };
    app.press_key_with_modifiers(KeyCode::KeyO, primary);
    wait_parts(&app, 3);
    // The field parts are drawn by the engine before they can be hit.
    wait(&app, "the field parts drawn", || (app.log_since(0).map(|t| t.lines).unwrap_or_default().iter().filter(|l| l.contains("drawn by the engine")).count() >= 2).then_some(()));
    app.locator(Selector::id("t_paint")).click();
    // A row at a time until both the bracket (a CSG mesh) and the vase (a field) have a stroke.
    let mut all = String::new();
    for row in [0.45, 0.5, 0.55, 0.35, 0.62] {
        all = brush_and_land(&app, &[row], 30).1;
        if all.contains("bracket.strokes") && all.contains("vase.strokes") {
            break;
        }
    }
    assert!(all.contains("bracket.strokes"), "the bracket (a CSG mesh part) took strokes");
    assert!(all.contains("vase.strokes"), "the vase (a field part) took strokes");
    assert!(all.matches("m.strokes(").count() > 0, "no stroke landed");
    assert!(!all.contains("m.sphere({radius: 3mm"), "strokes are polylines");
    let red = wait(&app, "the paint on screen", || {
        let red = red_share(&picture::finished_screenshot(&app));
        (red > 0.05).then_some(red)
    });
    println!("[fab-brush] grab: {red:.2}% paint red");

    // A smooth-region flood of the vase's body from its belly (fab.fill with a seed), in blue.
    // Zoom to the vase first (on the whole plate its paint covers too few pixels), and grab it
    // before the flood: the flood is the blue that was not there.
    app.press_key(KeyCode::Escape);
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).name("Bud vase")).wait_visible().click();
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Frame Selected")).wait_visible().press();
    let before = still_grab(&app);
    let head = head_of(&sources(&app));
    let from = seq(&app);
    let reply = call(&app, "fill", &format!(r#"{{"request_id":"flood","expected":"{head}","script":"vase.cad.splash","seed":[0,23.8,21],"angle":30,"albedo":[0.05,0.2,0.85]}}"#)).expect("fab.fill");
    assert!(!text(&reply).contains("error:"), "{}", text(&reply));
    // until the flood's paint is baked for the views
    wait(&app, "the flood baked", || app.log_since(from).expect("the app's log").lines.iter().find(|l| l.contains("fab vp paint: baked")).map(|_| ()));
    let blue = wait(&app, "the flood on screen", || {
        let blue = new_blue(&before, &still_grab(&app));
        (blue > 0.05).then_some(blue)
    });
    println!("[fab-brush] flood grab: {blue:.2}% new blue");

    // Procedural paint: the noise vase demo, two filaments mixed by a Splash noise field. Zoom to
    // the part first: on the whole plate its paint covers too few pixels.
    open_example(&app, "example noise vase", "noise_vase.cad.splash");
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).nth(0)).wait_visible().click();
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Frame Selected")).wait_visible().press();
    let (blue, yellow) = wait(&app, "the noise vase's two filaments on screen", || {
        let (blue, yellow) = noise_shares(&still_grab(&app));
        println!("[fab-brush] noise vase: {blue:.3}% blue, {yellow:.3}% yellow");
        (blue > 0.05 && yellow > 0.05).then_some((blue, yellow))
    });
    println!("[fab-brush] noise vase grab: {blue:.2}% blue, {yellow:.2}% yellow");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

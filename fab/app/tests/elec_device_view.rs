//! The device's 3D view in Electronics, three properties over one start of the tone box demo (its
//! Device + Scope workspace, the bench paused so its LED holds still):
//!
//! 1. The pointer moving over the view changes nothing in it: the part under the pointer takes no
//!    tint, and the view's still average does not start again. Why (user, 11-19): "can you also
//!    not make this annoying bright mouse over on the device view it flickers the UI". The view
//!    tinted the part under the pointer, the whole board at once, and each part the pointer
//!    crossed recorded the composite again and started the still average over.
//!    The pointer crosses the device on a grid of points, each at least `CLEAR` points from every
//!    live control (a ring marks the control under the pointer, and its name shows by it) and off
//!    every other widget over the view; the pointer jumps to each. The view must report each
//!    move (it takes one without drawing, so its description comes from a tree that the frame
//!    drawing only the status bar after it must not mark current), and the pointer must have
//!    been on at least two different elements. The view must never have recorded its composite
//!    alone (a hover tint's recording) nor started its still average again for it. Over a sweep
//!    in which nothing else recorded the view's scene (models or art still arriving: such a
//!    sweep is crossed again, up to `SWEEPS` times), the view's pixels equal those from before.
//! 2. The device keeps its case across the workspaces (user, 11-19: the drum machine's lid, keys
//!    and knobs showed at startup, then were gone). Design's Sliced mode, once on, swapped every
//!    other workspace's scene for the bed alone and drew the slice there too. After Design with a
//!    whole slice shown, then Mechanics, the view still draws the case's parts and places its
//!    knob and key as it did from the start.
//! 3. A part model that arrives while Fab runs shows at once, with what depends on it (user, 11-19:
//!    "it didn't have the models but it already downloaded them"). The test's part library already
//!    holds every model the tone box names, but EasyEDA is off at the start, so its parts have
//!    their plain bodies. Turning EasyEDA on in 3D Model Sources (accepting its terms) hands the
//!    library's models to the open design without a fetch: the key's model then reads as present,
//!    the view draws each model's body from its file, and the case keeps its parts, knob and key.
//!
//! No network: the test's own home holds that part library (each model a small pyramid, at this
//! converter's version), and KiCad's model sources are off.
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/render_mode.rs"]
mod render_mode;
use fab_elec_import::library::Source;
use fab_elec_import::{glb::write_glb, Material, MeshBuilder, Normals};
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::path::Path;
use std::time::Duration;

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
/// How far (points) a pointer stop keeps from every live control's grip.
const CLEAR: f64 = 40.0;
/// The grid the pointer crosses the device on.
const GRID: (usize, usize) = (6, 4);
/// How far (points) the grid reaches past the controls' box, over the rest of the device.
const AROUND: f64 = 120.0;
/// Sweeps tried for one with no scene recording from elsewhere in it.
const SWEEPS: usize = 5;
const VIEW: &str = "device_viewport";
const MODELS: &str = include_str!("../../elec/fixtures/tonebox.models.part.splash");

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    app.try_wait_until_within(HANG, || (!ok()).then(|| format!("waiting for {what}"))).unwrap_or_else(|why| {
        panic!("{why}; timed out waiting for {what}; the key's model reads {:?}; the device view reports {}",
            key_model(app), report(app).to_json());
    });
}

/// What the device view reports of itself (`FabViewport::semantics`). A read the busy app does not
/// answer is read again until it answers, never taken for an empty report (one taken so read as a
/// view without its controls); a read lost for the whole wait fails, naming it.
fn report(app: &TestApp) -> Value {
    let text = match app.locator(Selector::id(VIEW)).try_snapshot() {
        Ok(node) => node.description.clone(),
        Err(_) => {
            let mut text = String::new();
            app.try_wait_until_within(HANG, || match app.locator(Selector::id(VIEW)).try_snapshot() {
                Ok(node) => {
                    text = node.description.clone();
                    None
                }
                Err(err) => Some(format!("a read of {VIEW} the app did not answer ({err})")),
            }).unwrap_or_else(|why| panic!("{why}"));
            text
        }
    };
    makepad_strict_json::parse_depth(text.as_bytes(), 8).unwrap_or_else(|err| panic!("{VIEW} reports no JSON ({err}): {text}"))
}

fn workspace(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    for item in ["Workspace", name] {
        app.locator(Selector::role(Role::MenuItem).name(item)).wait_visible().press();
    }
}

/// A JSON number, whole or not.
fn num(v: &Value) -> Option<f64> {
    match v {
        Value::F64(f) => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

fn point(v: Option<&Value>) -> Option<(f64, f64)> {
    match v?.as_arr()? {
        [x, y] => Some((num(x)?, num(y)?)),
        _ => None,
    }
}

/// The view's scene recordings, its still average's starts, and those for its composite alone,
/// since the start.
fn records(app: &TestApp) -> (u64, u64, u64) {
    let r = report(app);
    let n = |k: &str| r.get("records").and_then(|v| v.get(k)).and_then(Value::as_u64).unwrap_or_else(|| panic!("records.{k}: {}", r.to_json()));
    (n("scene"), n("still"), n("comp"))
}

/// Settled with its whole still average.
fn finished(app: &TestApp) -> bool {
    let r = report(app);
    r.get("settled").and_then(Value::as_bool) == Some(true)
        && r.get("still_samples").and_then(Value::as_u64) == Some(u64::from(makepad_render::STILL_AA_SAMPLES))
}

/// The bounds (window points) of the node `selector` finds: x, y, width, height.
fn bounds(app: &TestApp, selector: Selector) -> (f64, f64, f64, f64) {
    let b = app.locator(selector).wait_visible().snapshot().bounds;
    (b.x as f64, b.y as f64, b.w as f64, b.h as f64)
}

/// The visible widgets smaller than the view that hold the point (window points): what lies over
/// the view there (its bars, a tip), by name.
fn over_view(app: &TestApp, view: (f64, f64, f64, f64), (x, y): (f64, f64)) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|n| n.visible()).filter(|n| {
        let b = n.node.bounds;
        let (bx, by, bw, bh) = (b.x as f64, b.y as f64, b.w as f64, b.h as f64);
        bw * bh < view.2 * view.3 && x >= bx && x <= bx + bw && y >= by && y <= by + bh
    }).map(|n| format!("{:?} {:?}", n.node.role, n.node.name)).collect()
}

/// Every visible node that holds the point (window points), smallest first: role, name, bounds.
fn nodes_at(app: &TestApp, (x, y): (f64, f64)) -> Vec<String> {
    let mut nodes: Vec<(f64, String)> = app.widget_snapshot().nodes().into_iter().filter(|n| n.visible()).filter_map(|n| {
        let b = n.node.bounds;
        let (bx, by, bw, bh) = (b.x as f64, b.y as f64, b.w as f64, b.h as f64);
        (x >= bx && x <= bx + bw && y >= by && y <= by + bh)
            .then(|| (bw * bh, format!("{:?} {:?} ({bx:.0}, {by:.0}, {bw:.0}, {bh:.0})", n.node.role, n.node.name)))
    }).collect();
    nodes.sort_by(|a, b| a.0.total_cmp(&b.0));
    nodes.into_iter().map(|(_, n)| n).collect()
}

/// The device view's pixels in a grab of the window (RGB, row by row).
fn view_pixels(app: &TestApp) -> Vec<u8> {
    let bytes = std::fs::read(app.screenshot()).expect("the grab");
    let mut dec = PngDecoder::new(ZCursor::new(&bytes));
    let px = dec.decode_raw().expect("png");
    let (w, h) = dec.dimensions().expect("dims");
    let n = dec.colorspace().expect("colorspace").num_components();
    let window = bounds(app, Selector::role(Role::Window));
    let scale = w as f64 / window.2;
    let b = bounds(app, Selector::id(VIEW));
    let (x0, y0) = (((b.0 - window.0) * scale).max(0.0) as usize, ((b.1 - window.1) * scale).max(0.0) as usize);
    let (x1, y1) = ((((b.0 + b.2 - window.0) * scale) as usize).min(w), (((b.1 + b.3 - window.1) * scale) as usize).min(h));
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0) * 3);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * w + x) * n;
            out.extend_from_slice(&px[i..i + 3]);
        }
    }
    out
}

/// The finished picture: its records, and its pixels when no recording came between them.
fn finished_picture(app: &TestApp, what: &str) -> ((u64, u64, u64), Vec<u8>) {
    loop {
        wait_until(app, what, || finished(app));
        let before = records(app);
        let pixels = view_pixels(app);
        if records(app) == before && finished(app) {
            return (before, pixels);
        }
    }
}

/// What the view shows of the closed case: the parts it draws, and its knobs and keys each at a
/// finite point (bound to their parts and posed).
#[derive(Debug)]
struct Case {
    parts: u64,
    knobs: usize,
    keys: usize,
}

fn case(app: &TestApp) -> Case {
    case_of(&report(app))
}

/// The controls a report places at finite points (window points): whether each is a knob, where.
fn placed(r: &Value) -> Vec<(bool, (f64, f64))> {
    r.get("controls").and_then(Value::as_arr).unwrap_or(&[]).iter().filter_map(|c| {
        let at = point(c.get("at")).filter(|(x, y)| x.is_finite() && y.is_finite())?;
        Some((c.get("knob").and_then(Value::as_bool)?, at))
    }).collect()
}

fn case_of(r: &Value) -> Case {
    let placed = placed(r);
    let count = |knob: bool| placed.iter().filter(|c| c.0 == knob).count();
    Case { parts: r.get("parts_drawn").and_then(Value::as_u64).unwrap_or(0), knobs: count(true), keys: count(false) }
}

/// A report that shows the closed case, for a value read once: a read the busy app did not answer
/// comes back as no report, which a one-shot read would take for a view without its controls.
fn closed_report(app: &TestApp) -> Value {
    let mut r = Value::Null;
    wait_until(app, "the closed case", || {
        r = report(app);
        closed(&case_of(&r))
    });
    r
}

/// The case's parts drawn around its knobs and keys.
fn closed(c: &Case) -> bool {
    c.knobs > 0 && c.keys > 0 && c.parts as usize > c.knobs + c.keys
}

/// The text a remote tool call answers, or why there is none.
fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let mut lines = Vec::new();
    let argv = vec!["call".to_string(), tool.to_string(), args.to_string()];
    makepad_remote::app::run(&client, &argv, &mut |line| { lines.push(line.to_string()); true }).map_err(|e| format!("{tool}: {e:?}"))?;
    let v = lines.iter().find(|l| l.contains("\"Result\"")).and_then(|l| makepad_strict_json::parse_depth(l.as_bytes(), 32).ok())
        .ok_or_else(|| format!("{tool}: no result in {lines:?}"))?;
    let r = match v.get("Result") {
        Some(Value::Arr(a)) => a.first().cloned().unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => Value::Null,
    };
    r.get("text").and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("{tool}: no text in {lines:?}"))
}

/// The slice tool says the whole plate is sliced.
fn whole_slice(app: &TestApp) -> bool {
    let Ok(text) = call(app, "fab.slice", "{}") else { return false };
    let Ok(reply) = makepad_script::syntax::parse_value(&text) else { return false };
    let value = |name: &str| reply.field(name).map(|f| &f.value);
    value("state").and_then(|v| v.string()) == Some("Sliced") && value("whole").is_some_and(|v| v.text(&text) == "true")
}

/// The key's model as `elec.part.model` reads it: (body, here), or the answer that says why not.
fn key_model(app: &TestApp) -> Result<(String, bool), String> {
    let text = call(app, "fab.elec_read", r#"{"action":"elec.part.model","args":{"part":"key"}}"#)?;
    let reply = makepad_script::syntax::parse_value(&text).map_err(|_| text.clone())?;
    let body = reply.field("body").and_then(|f| f.value.string()).ok_or_else(|| text.clone())?.to_string();
    Ok((body, reply.field("here").is_some_and(|f| f.value.text(&text) == "true")))
}

/// The view's part bodies drawn from a model file.
fn model_bodies(app: &TestApp) -> u64 {
    report(app).get("model_bodies").and_then(Value::as_u64).unwrap_or(0)
}

/// A part library entry for each model the tone box names: a closed 2 mm square pyramid, as this
/// converter writes it. The model numbers.
fn fill_library(parts: &Path) -> Vec<String> {
    let mut numbers: Vec<String> = MODELS.split("parts/lcsc/").skip(1)
        .filter_map(|rest| rest.split_once(".glb")).map(|(number, _)| number.to_string()).collect();
    numbers.sort();
    numbers.dedup();
    let mut builder = MeshBuilder::new();
    let grey = builder.add_material(Material::named("body"));
    let base = [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]];
    let normals = Normals::Crease { domain: 0, angle: 0.5 };
    assert!(builder.polygon(grey, &[base[3], base[2], base[1], base[0]], normals), "the base is a polygon");
    for i in 0..4 {
        assert!(builder.polygon(grey, &[base[i], base[(i + 1) % 4], [0.0, 0.0, 2.0]], normals), "pyramid side {i}");
    }
    let glb = write_glb(&builder.finish());
    let source = fab_elec_import::easyeda::Lcsc;
    for number in &numbers {
        let dir = parts.join(source.folder()).join(number);
        std::fs::create_dir_all(&dir).expect("the library entry");
        std::fs::write(dir.join(format!("{number}.glb")), &glb).expect("the entry's model");
        std::fs::write(dir.join("convert.txt"), format!("converter {}\n", source.version())).expect("the entry's version");
    }
    numbers
}

#[test]
fn the_device_view_holds_still_keeps_its_case_and_shows_models_as_they_arrive() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "elec_device_view").expect("the test's config");
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), "1600x1000@1".into(), "--demo".into(), "tonebox".into()];
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("the clock").as_nanos();
    let dir = std::env::temp_dir().join(format!("fab-elec-device-view-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("a fresh folder for the test's home");
    let numbers = fill_library(&dir.join(".makepad/fab/parts"));
    assert!(numbers.len() >= 5, "the tone box names its models: {numbers:?}");
    let prefs = dir.join("view.prefs");
    std::fs::write(&prefs, "code=0\n").expect("the test's view preferences");
    let sources = dir.join("model_sources.prefs");
    std::fs::write(&sources, "kicad_local=0\nkicad_download=0\neasyeda=0\n").expect("the test's model sources");
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    config.env.insert("FAB_MODEL_SOURCES".into(), sources.display().to_string());
    let result = run_with_config(config, |app: TestApp| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        workspace(&app, "Device + Scope");
        wait_until(&app, "the closed case", || closed(&case(&app)));

        // The bench paused: its LED holds still.
        let button = |name: &str| app.locator(Selector::role(Role::Button).name(name)).try_snapshot().is_ok();
        wait_until(&app, "the bench's play or pause", || button("play") || button("pause"));
        if button("pause") {
            app.locator(Selector::role(Role::Button).name("pause")).wait_visible().press();
        }
        wait_until(&app, "the bench paused", || button("play"));

        // 1. Over the device, clear of the controls and of what lies over the view.
        let grips: Vec<(f64, f64)> = placed(&closed_report(&app)).into_iter().map(|c| c.1).collect();
        let b = bounds(&app, Selector::id(VIEW));
        // Over the device: the controls' box and `AROUND` points about it, inside the view.
        let (lo, hi) = grips.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(lo, hi), g| ((lo.0.min(g.0), lo.1.min(g.1)), (hi.0.max(g.0), hi.1.max(g.1))));
        let (x0, x1) = ((lo.0 - AROUND).max(b.0 + 5.0), (hi.0 + AROUND).min(b.0 + b.2 - 5.0));
        let (y0, y1) = ((lo.1 - AROUND).max(b.1 + 5.0), (hi.1 + AROUND).min(b.1 + b.3 - 5.0));
        let mut stops = Vec::new();
        for j in 0..GRID.1 {
            for i in 0..GRID.0 {
                let x = x0 + (x1 - x0) * i as f64 / (GRID.0 - 1) as f64;
                let y = y0 + (y1 - y0) * j as f64 / (GRID.1 - 1) as f64;
                if grips.iter().all(|g| (g.0 - x).hypot(g.1 - y) >= CLEAR) && over_view(&app, b, (x, y)).is_empty() {
                    stops.push((x, y));
                }
            }
        }
        assert!(stops.len() >= 8 && stops.iter().all(|(x, y)| x.is_finite() && y.is_finite()),
            "at least eight stops, at finite points clear of the controls and the view's bars: {stops:?} beside {grips:?}");

        let mut hovered = Vec::new();
        for sweep in 1..=SWEEPS {
            let (before, pixels) = finished_picture(&app, "the device view's finished picture");
            // Each move taken, the element under it noted.
            for &(x, y) in &stops {
                app.move_to(x, y);
                app.try_wait_until_within(HANG, || {
                    (!point(report(&app).get("pointer")).is_some_and(|p| (p.0 - x).abs() < 0.5 && (p.1 - y).abs() < 0.5)).then(|| format!("waiting for the move to ({x:.0}, {y:.0})"))
                }).unwrap_or_else(|why| {
                    panic!("{why}: the device view never took it; every node there: {:?}; the view's bounds {b:?}; it reports {}",
                        nodes_at(&app, (x, y)), report(&app).to_json());
                });
                if let Some(element) = report(&app).get("hover").and_then(Value::as_u64) {
                    if !hovered.contains(&element) {
                        hovered.push(element);
                    }
                }
            }
            let (after, moved) = finished_picture(&app, "the device view after the moves");
            assert_eq!(after.2, before.2, "the moves recorded the composite alone (a tint) and started its still average again");
            if after.0 != before.0 {
                assert!(sweep < SWEEPS, "every sweep met a scene recording from elsewhere (scene, still, comp): {before:?} then {after:?}");
                continue;
            }
            assert_eq!(after.1, before.1, "the moves started no still average again");
            let changed = pixels.iter().zip(&moved).filter(|(a, b)| a != b).count();
            assert!(pixels.len() == moved.len() && changed == 0,
                "the moves changed no pixel of the device view: {changed} of {} channels differ ({} then {})", pixels.len(), pixels.len(), moved.len());
            break;
        }
        assert!(hovered.len() >= 2, "the pointer crossed at least two parts: {hovered:?} at {stops:?}");

        // 2. Design with its whole slice shown, then Mechanics, then back.
        let fresh = case_of(&closed_report(&app));
        workspace(&app, "3D modeling");
        render_mode::pick(&app, "Sliced");
        wait_until(&app, "the whole slice", || whole_slice(&app));
        workspace(&app, "Mechanics");
        app.locator(Selector::id("viewport")).wait_visible();
        workspace(&app, "Device + Scope");
        wait_until(&app, &format!("the case as it was ({fresh:?}) after Sliced and Mechanics"), || {
            let c = case(&app);
            c.knobs == fresh.knobs && c.keys == fresh.keys && c.parts >= fresh.parts
        });

        // 3. EasyEDA turned on: the library's models reach the design and the view.
        let plain = closed_report(&app).get("model_bodies").and_then(Value::as_u64).expect("the view reports its bodies drawn from a model");
        assert_eq!(key_model(&app), Ok(("easyeda".to_string(), false)), "with EasyEDA off the key keeps its plain body");
        app.locator(Selector::role(Role::Button).name("Edit")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name("3D Model Sources…")).wait_visible().press();
        app.locator(Selector::id("easyeda")).wait_visible().click();
        app.locator(Selector::id("accept")).wait_visible().click();
        app.locator(Selector::id("close")).wait_visible().click();
        let want = plain + numbers.len() as u64;
        wait_until(&app, &format!("the key's model here and at least {want} bodies drawn from a model"), || {
            key_model(&app) == Ok(("easyeda".to_string(), true)) && model_bodies(&app) >= want
        });
        wait_until(&app, &format!("the case as it was ({fresh:?}) with the models"), || {
            let c = case(&app);
            c.knobs == fresh.knobs && c.keys == fresh.keys && c.parts >= fresh.parts
        });
    });
    let cleanup = std::fs::remove_dir_all(&dir);
    result.expect("the device view");
    cleanup.expect("remove the test's home");
}

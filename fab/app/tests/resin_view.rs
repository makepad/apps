//! The Slice render mode on a resin printer (the simulated one, `fab_sla::printer`, installed as
//! the installed app's printer): instead of toolpaths, the plate's layers as images on the
//! screen's pixels. A block on the plate stands on its supports and shows its top layer at
//! first, the layer panel names the layer, its height and its lit pixels, and the layer slider
//! walks the stack: the pad, the supports, the block; the view hangs the stack from the plate,
//! as it prints, until it is put upright; nothing slices for filament.
//! With a field part beside it, a new layer moves the cut in the view's retained passes without
//! recording them again, and the pixels are a fresh recording's. A ball shut in a hollow box has
//! no way down for its supports: Printability names it, an island that cures onto the film.
//! Export writes the stack: a greyscale PNG per layer as the screen shows it, and a manifest.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/export_print.rs"]
mod export_print;
#[path = "common/render_mode.rs"]
mod render_mode;
mod common;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
#[path = "common/simulated_profiles.rs"]
mod simulated_profiles;

use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::time::Duration;

/// The viewport's widget path, for its live diagnostics.
const VIEWPORT: &str = "body.chat_split.shell.main.dock.tab_viewport.viewport";

fn remote(app: &TestApp, args: &[String]) -> Result<Vec<String>, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, args, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("{args:?}: {e:?}"))?;
    match lines.iter().any(|l| l.contains("\"err\":")) {
        true => Err(format!("{args:?}: {lines:?}")),
        false => Ok(lines),
    }
}

fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let lines = remote(app, &["call".to_string(), format!("fab.{tool}"), args.to_string()])?;
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

fn head_of(tool: &str, reply: &str) -> String {
    let value = makepad_script::syntax::parse_value(reply).unwrap_or_else(|e| panic!("fab.{tool}: not a Splash reply ({e:?}): {reply}"));
    value.field("head").and_then(|f| f.value.string()).unwrap_or_else(|| panic!("fab.{tool}: the reply has no head: {reply}")).to_string()
}

fn names(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible()).map(|w| w.node.name).collect()
}

/// The layer panel's line: (layer, of layers, lit pixels).
fn panel(app: &TestApp) -> Option<(u32, u32, usize)> {
    names(app).into_iter().find_map(|t| {
        let rest = t.strip_prefix("Layer ")?;
        let (layer, rest) = rest.split_once(" of ")?;
        let (count, rest) = rest.split_once("  ·  ")?;
        let lit = rest.split("  ·  ").nth(1)?.strip_suffix(" px lit")?;
        Some((layer.parse().ok()?, count.parse().ok()?, lit.parse().ok()?))
    })
}

fn wait_panel(app: &TestApp, what: &str, ok: impl Fn((u32, u32, usize)) -> bool) -> (u32, u32, usize) {
    wait_poll(app, || {
        if let Some(p) = panel(app).filter(|p| ok(*p)) {
            return Ok(p);
        }
        Err(format!("{what}: the layer panel says {:?}; on screen: {:?}", panel(app), names(app)))
    })
}

/// The viewport's camera as last drawn: hung, and its up hint's Z.
fn camera_up(app: &TestApp) -> (bool, f64) {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    let value = makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).expect("viewport inspection JSON");
    let c = value.get("camera").expect("viewport camera");
    let up = c.get("up").and_then(Value::as_arr).expect("camera up");
    let z = match &up[2] {
        Value::F64(n) => *n,
        Value::Int(n) => *n as f64,
        v => panic!("camera up: {v:?}"),
    };
    (c.get("hung").and_then(Value::as_bool).expect("camera hung"), z)
}

/// Waits for the camera to have turned (over when `hung`) all the way.
fn wait_turned(app: &TestApp, what: &str, hung: bool) {
    let want = if hung { -1.0 } else { 1.0 };
    wait_poll(app, || {
        let (h, z) = camera_up(app);
        if h == hung && (z - want).abs() < 1e-3 {
            return Ok(());
        }
        Err(format!("{what}: the camera is hung {h}, its up's Z {z}"))
    })
}

/// The viewport's inspection: what it reports of the recording on display.
fn recording(app: &TestApp) -> Value {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).expect("viewport inspection JSON")
}

/// How often the view recorded its scene for a change, and moved a resin cut in place.
fn records(app: &TestApp) -> (u64, u64) {
    let r = recording(app);
    let n = |k: &str| r.get("records").and_then(|v| v.get(k)).and_then(Value::as_u64).unwrap_or_else(|| panic!("records.{k}: {r:?}"));
    (n("scene"), n("cut_moved"))
}

/// Waits for the view's finished picture: settled, its still average complete.
fn wait_finished(app: &TestApp, what: &str) {
    wait_poll(app, || {
        let r = recording(app);
        if r.get("settled").and_then(Value::as_bool) == Some(true) && r.get("still_samples").and_then(Value::as_u64) == Some(makepad_render::STILL_AA_SAMPLES as u64) {
            return Ok(());
        }
        Err(format!("{what}: the view never finished its picture: {r:?}"))
    })
}

/// The window once nothing changes (its records and two grabs alike): its records and pixels.
fn quiet(app: &TestApp, what: &str) -> ((u64, u64), Vec<u8>) {
    wait_poll(app, || {
        let (before, first) = (records(app), std::fs::read(app.screenshot()).expect("the grab"));
        std::thread::sleep(Duration::from_millis(300));
        let (after, second) = (records(app), std::fs::read(app.screenshot()).expect("the grab"));
        if before == after && first == second {
            return Ok((after, pixels(&second)));
        }
        Err(format!("{what}: the window never stood still (records {before:?} then {after:?})"))
    })
}

fn pixels(png: &[u8]) -> Vec<u8> {
    PngDecoder::new(ZCursor::new(png)).decode_raw().expect("png")
}

/// The viewport's live `debug_view` (0 normal; 4 and 5 the composite with no still average).
fn debug_view(app: &TestApp, value: u32) {
    remote(app, &["tweak".into(), "apply".into(), format!("path={VIEWPORT}"), "prop=debug_view".into(), format!("value={value}")]).expect("the viewport's debug_view");
}


#[test]
fn a_resin_printers_slice_is_layer_images_the_slider_walks() {
    let dir = std::env::temp_dir().join(format!("fab-resin-view-{}", std::process::id()));
    simulated_profiles::install(&dir);
    simulated_profiles::select(&dir, fab_sla::printer::SIMULATED_PRINTER.0, fab_sla::printer::SIMULATED_PROCESS.0);
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "a_resin_printers_slice_is_layer_images_the_slider_walks").unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    let result = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        let head = wait_poll(&app, || call(&app, "inspect", "{}").map(|line| head_of("inspect", &text(&line))).map_err(|e| format!("the tools never appeared: {e}")));
        // a 10 x 10 mm block 5 mm tall: 100 layers of 0.05 mm, 200 x 200 pixels of 0.05 mm
        let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet block = m.box({size: [10mm 5mm 10mm] at: [0mm 2.5mm 0mm]})\nobject(block)\n";
        let batch = format!(r#"{{"request_id":"resin-block","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(source.into()).to_json());
        let head = head_of("script", &text(&call(&app, "script", &batch).expect("script")));
        let batch = format!(r#"{{"request_id":"resin-place","expected":{head:?},"edits":"place(\"block\", name: \"Block\")"}}"#);
        let head = head_of("place", &text(&call(&app, "place", &batch).expect("place")));

        render_mode::pick(&app, "Sliced");
        let full = 200 * 200;
        // the top layer at first, the block whole under it: the block stands on its supports,
        // 6 mm up (5 mm over a 1 mm pad), so its 5 mm are layers 121 to 220
        let (layer, count, lit) = wait_panel(&app, "the top layer", |p| p.0 == p.1);
        assert_eq!(count, 220, "the block's 5 mm 6 mm up, in 0.05 mm layers");
        assert!(lit.abs_diff(full) <= 4 * 201, "layer {layer} lights the block's 200 x 200 pixels, give or take its edge: {lit}");
        // the job's time and resin: 4 bottom layers of 0.5 + 25 s light and a 6 mm peel at 1 mm/s
        // back at 3 mm/s, 216 of 0.5 + 2.5 s and a 5 mm peel at 2 mm/s, 1678 s; the block's
        // 10 x 10 x 5 mm, and the supports' own resin once the rules checked them
        let mut got = None;
        app.try_wait_until_within(HANG, || {
            if let Some(s) = names(&app).into_iter().find(|n| n.contains(" in the supports.")) {
                got = Some(s);
                return None;
            }
            Some("waiting for an estimate with the supports' resin".to_string())
        }).unwrap_or_else(|why| {
            let said: Vec<String> = app.log_since(0).map(|l| l.lines.into_iter().filter(|l| l.contains("fab resin")).collect()).unwrap_or_default();
            panic!("{why}; no estimate with the supports' resin: {:?}; the app said {said:#?}", names(&app).into_iter().filter(|n| n.contains("resin")).collect::<Vec<_>>());
        });
        let status = got.expect("the wait ended on an answer");
        assert!(status.contains("About 27:58 to print, 0.50 ml of resin in the parts and "), "{status}");
        let supports: f64 = status.rsplit_once(" and ").and_then(|(_, s)| s.strip_suffix(" ml in the supports.")).and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("{status}"));
        assert!(supports > 0.0 && supports < 0.5, "the supports' resin, a fraction of the block's: {status}");
        // it hangs from the plate as it prints: the camera turns over; upright turns it back
        wait_turned(&app, "the plate on top", true);
        app.locator(Selector::role(Role::CheckBox).name("Plate on top")).check(false);
        wait_turned(&app, "upright", false);
        // the slider walks the stack: low down the pad, wider than the block; halfway the
        // supports' heads, a few small rounds; higher the block's same square
        app.locator(Selector::role(Role::Slider).name("Top layer")).set_value("0.05");
        let (layer, _, lit) = wait_panel(&app, "the pad's layer", |p| p.0 < p.1);
        assert_eq!(layer, 12, "a twentieth along 220 layers, 0.6 mm up");
        assert!(lit > full, "the pad, its bases closed and a brim past them, is wider than the block: {lit}");
        app.locator(Selector::role(Role::Slider).name("Top layer")).set_value("0.5");
        let (layer, _, lit) = wait_panel(&app, "the heads' layer", |p| p.0 > 12 && p.0 < p.1);
        assert_eq!(layer, 111, "halfway along 220 layers, 5.55 mm up");
        assert!(lit > 0 && lit < full / 4, "the heads under the block: {lit}");
        app.locator(Selector::role(Role::Slider).name("Top layer")).set_value("0.7");
        let (layer, _, lit) = wait_panel(&app, "a layer of the block", |p| p.0 > 111 && p.0 < p.1);
        assert_eq!(layer, 154, "0.7 along 220 layers, 7.7 mm up");
        assert!(lit.abs_diff(full) <= 4 * 201, "the block's square: {lit}");
        // the slice is the layers: nothing slices for filament
        assert_ne!(common::slice_state(&app), "Sliced", "no toolpaths for a resin printer");

        // a field part beside it, 8 x 8 mm and 4 mm tall: its tiles cut with the block's mesh
        let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet pad = m.sdf({\n bounds: [[-4.5mm, 0, -4.5mm], [4.5mm, 4.5mm, 4.5mm]]\n cell: 0.1mm\n field: fn(p) { return sdf_box(p - vec3(0, 2mm, 0), vec3(8mm, 4mm, 8mm)) }\n})\nobject(pad)\n";
        let batch = format!(r#"{{"request_id":"resin-pad","expected":{head:?},"script":"pad.cad.splash","source":{}}}"#, Value::Str(source.into()).to_json());
        let head = head_of("script", &text(&call(&app, "script", &batch).expect("script")));
        let batch = format!(r#"{{"request_id":"resin-place-pad","expected":{head:?},"edits":"place(\"pad\", at: [45mm 40mm], name: \"Pad\")"}}"#);
        let head = head_of("place", &text(&call(&app, "place", &batch).expect("place")));
        // (where the plate puts it, it may overlap the block: half its 160 x 160 pixels at least)
        let pad = 160 * 160;
        let (layer, _, _) = wait_panel(&app, "the field part's layers", |p| p.2 > full + pad / 2);
        wait_finished(&app, "the block and the pad");
        // over a still view's average the passes hold its last jittered sample: a new layer
        // records them again
        let (scene, _) = records(&app);
        app.locator(Selector::role(Role::Slider).name("Top layer")).set_value("0.8");
        wait_panel(&app, "a higher layer", |p| p.0 > layer);
        wait_finished(&app, "the higher layer");
        assert!(records(&app).0 > scene, "a cut over a still average's sample records the scene again");
        // with no still average, a new layer only moves the cut: nothing records
        debug_view(&app, 4);
        let ((scene, moved), _) = quiet(&app, "the composite alone");
        app.locator(Selector::role(Role::Slider).name("Top layer")).set_value("0.3");
        wait_panel(&app, "a lower layer", |p| p.0 < layer);
        let ((scene_after, moved_after), patched) = quiet(&app, "the lower layer, its cut moved");
        assert_eq!(scene_after, scene, "a new layer records no scene pass");
        assert!(moved_after > moved, "a new layer moves the cut in place");
        // and what it shows is a fresh recording's, pixel for pixel
        debug_view(&app, 5);
        let ((fresh_scene, _), fresh) = quiet(&app, "the lower layer recorded afresh");
        assert!(fresh_scene > scene_after, "the debug view's change records the scene");
        let differ = patched.iter().zip(&fresh).filter(|(a, b)| a != b).count();
        assert!(patched.len() == fresh.len() && differ == 0, "the moved cut's picture is not a fresh recording's: {differ} of {} bytes differ", fresh.len());
        debug_view(&app, 0);

        // a 4 mm ball shut in a 12 mm box with 1 mm walls: no support reaches it, so its first layer
        // has nothing on the plate's side
        let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet outer = m.box({size: [12mm 12mm 12mm] at: [0 6mm 0]})\nlet inner = m.box({size: [10mm 10mm 10mm] at: [0 6mm 0]})\nlet ball = m.sphere({radius: 2mm at: [0 6mm 0]})\nobject(m.union(m.subtract(outer, inner), ball))\n";
        let batch = format!(r#"{{"request_id":"resin-cage","expected":{head:?},"script":"cage.cad.splash","source":{}}}"#, Value::Str(source.into()).to_json());
        let head = head_of("script", &text(&call(&app, "script", &batch).expect("script")));
        let batch = format!(r#"{{"request_id":"resin-place-cage","expected":{head:?},"edits":"place(\"cage\", at: [95mm 40mm], name: \"Cage\")"}}"#);
        call(&app, "place", &batch).expect("place");
        let printability = || app.locator(Selector::widget_type("FabIssueSummary").name("Printability")).try_snapshot().ok().map(|w| w.node.value);
        app.try_wait_until_within(HANG, || (!printability().is_some_and(|v| v.contains("Unconnected island, 1 (error)"))).then(|| "waiting for Printability to name the ball".to_string())).unwrap_or_else(|why| {
            let said: Vec<String> = app.log_since(0).map(|l| l.lines.into_iter().filter(|l| l.contains("fab resin")).collect()).unwrap_or_default();
            let status: Vec<String> = names(&app).into_iter().filter(|n| n.contains("supports")).collect();
            panic!("{why}; Printability never named the ball: {:?}; {status:?}; the app said {said:#?}", printability());
        });

        // Export writes the stack: a greyscale PNG per layer as the screen shows it (the simulated
        // screen unturned, 2400 x 1600), and the manifest names them all
        app.locator(Selector::role(Role::Tab).name("Slice")).select();
        export_print::request(&app);
        let wrote = wait_poll(&app, || {
            if let Some(s) = names(&app).into_iter().find(|n| n.contains(" Wrote ") || n.contains("Export failed")) {
                return Ok(s);
            }
            Err(format!("Export wrote nothing: {:?}", names(&app).into_iter().filter(|n| n.contains("layers")).collect::<Vec<_>>()))
        });
        let (count, to) = wrote.split_once(" Wrote ").and_then(|(_, w)| w.split_once(" layers to ")).unwrap_or_else(|| panic!("{wrote}"));
        let (count, to): (usize, std::path::PathBuf) = (count.parse().unwrap_or_else(|_| panic!("{wrote}")), to.trim_end_matches('.').into());
        assert!(to.starts_with(&dir) && to.extension().is_some_and(|e| e == "layers"), "a layer folder in the test home's documents: {wrote}");
        let pngs = std::fs::read_dir(&to).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".png")).count();
        assert_eq!(pngs, count, "a PNG per layer in {}", to.display());
        let manifest = makepad_strict_json::parse(&std::fs::read(to.join("manifest.json")).unwrap()).expect("the manifest is JSON");
        assert_eq!(manifest.get("layers").and_then(Value::as_arr).map(|l| l.len()), Some(count));
        let png = std::fs::read(to.join("layer_00000.png")).unwrap();
        let mut decoder = PngDecoder::new(ZCursor::new(&png));
        let first = decoder.decode_raw().expect("the first layer decodes");
        assert_eq!(decoder.dimensions(), Some((2400, 1600)), "the screen's pixels");
        assert!(first.iter().filter(|g| **g > 0).count() > full, "the pad's first layer, wider than the block, is lit");
    });
    let _ = std::fs::remove_dir_all(&dir);
    result.unwrap();
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

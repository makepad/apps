//! The orthographic view as a person drives it: the laptop's `5` and the view bar's button
//! toggle it, the choice is kept for the next start, and zoomed in close it still shows the
//! whole part, in the Normal view and the Sliced one, and draws a box on the bed.
//!
//! Why (user): "also we need an orthographic projection mode in our viewer". Every ray comes
//! from the camera's one ray function, and orthographic the eye stands back behind all the
//! view shows, so near and far hold the scene however close the view is zoomed in.
//!
//! One plate: a 60 x 40 mm slab 6 mm thick with a 12 mm square post 40 mm tall on its right end.
//! Zoomed in by the wheel onto the slab's top left of the post, as close as a perspective view
//! goes (20 mm), then made orthographic by `5` and turned to the Right view, the post stands
//! between the eye's old plane and the view's right: before, that plane cut the post and the
//! slab's end away. Zoomed back out (orthographic) till the part fits:
//!
//! - the post shows whole and lit: over the middle of where it projects, no pixel is the sky
//!   beside it, and the white part is brighter than that sky (cut or seen from inside, it was
//!   drawn darker than the sky);
//! - the Sliced view shows the part where the Normal view does (their outlines overlap), and
//!   from the side the post's middle is its outer wall's orange, not the green inner walls a
//!   cut shows;
//! - from the Top view the post's top is its top surface's red, not the magenta sparse infill a
//!   cut through its top layers shows, and a drag on the bed with the No Trees tool writes its
//!   area.
//!
//! Each step's picture is kept (`DEVHUB_KEEP`). HAL offscreen, native GPU, no window.

#[path = "common/view_options.rs"]
mod view_options;
#[path = "common/render_mode.rs"]
mod render_mode;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, KeyCode, KeyModifiers, Selector, TestApp, TestConfig};
use makepad_widgets::{vec3, vec4, Vec3f};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::time::{Duration, Instant};

/// Only a hang fails a wait: a cold or loaded box gets there late, not never.
const HANG: Duration = Duration::from_secs(300);

const PART: &str = "// A slab with a post standing on its right end.
use mod.model.*
use mod.fab.*
let m = mod.model
let slab = m.box({size: [60mm 6mm 40mm] at: [0, 3mm, 0]})
let post = m.box({size: [12mm 40mm 12mm] at: [22mm, 26mm, 0]})
let block = m.union(slab, post)
object(block)
";

const PLATE: &str = r#"place("block", object: "block", at: [120mm 100mm], name: "Block")"#;

/// The window, in logical pixels, at device scale 2.
const WINDOW: (f64, f64, f64) = (1600.0, 1000.0, 2.0);

fn remote(app: &TestApp, args: &[String]) -> Result<Vec<String>, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, args, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("{args:?}: {e:?}; {lines:?}"))?;
    if lines.iter().any(|l| l.contains("\"err\":")) {
        return Err(format!("{args:?}: {lines:?}"));
    }
    Ok(lines)
}

/// A fab tool's reply text; an error when the tool refused (its edit not committed).
fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let lines = remote(app, &["call".into(), format!("fab.{tool}"), args.into()])?;
    let line = lines.iter().find(|l| l.contains("\"Result\"")).ok_or_else(|| format!("fab.{tool}: no result: {lines:?}"))?;
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).map_err(|e| format!("{e:?}"))?;
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    let text = r.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
    match r.get("outcome").and_then(|o| o.get("Ok")) {
        Some(_) => Ok(text),
        None => Err(format!("fab.{tool} refused: {text}")),
    }
}

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// `fab.slice` says the slice is current and whole (no live tiles left in it).
fn whole_slice(text: &str) -> bool {
    let Ok(reply) = makepad_script::syntax::parse_value(text) else { return false };
    let value = |name: &str| reply.field(name).map(|f| &f.value);
    value("state").and_then(|v| v.string()) == Some("Sliced") && value("whole").is_some_and(|v| v.text(text) == "true")
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ok() {
        if started.elapsed() > HANG {
            panic!("timed out waiting for {what}; the viewport reports {}", app.locator(Selector::id("viewport")).snapshot().description);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// What the viewport reports of the recording on display.
fn recording(app: &TestApp) -> Value {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).unwrap_or(Value::Null)
}

fn camera_json(app: &TestApp) -> Option<String> {
    recording(app).get("camera").map(Value::to_json)
}

fn settled(app: &TestApp) -> bool {
    recording(app).get("settled").and_then(Value::as_bool) == Some(true)
}

/// The camera the view drew with, as its recording tells it.
fn camera(app: &TestApp) -> fab::api::Camera {
    let r = recording(app);
    let c = r.get("camera").expect("the recording's camera");
    let num = |v: &Value| match v {
        Value::F64(f) => Some(*f as f32),
        Value::Int(i) => Some(*i as f32),
        _ => None,
    };
    let v3 = |k: &str| {
        let a: Vec<f32> = c.get(k).and_then(Value::as_arr).map(|a| a.iter().filter_map(num).collect()).unwrap_or_default();
        assert_eq!(a.len(), 3, "the camera's {k}: {}", c.to_json());
        vec3(a[0], a[1], a[2])
    };
    fab::api::Camera {
        eye: v3("eye"),
        target: v3("target"),
        up: v3("up"),
        hung: c.get("hung").and_then(Value::as_bool).unwrap_or(false),
        ortho: c.get("ortho").and_then(Value::as_bool).unwrap_or(false),
        ortho_height: c.get("ortho_height").and_then(num).unwrap_or(0.5),
        ..Default::default()
    }
}

/// The viewport's rectangle (logical pixels): x, y, w, h.
fn view_rect(app: &TestApp) -> (f64, f64, f64, f64) {
    let b = app.locator(Selector::id("viewport")).snapshot().bounds;
    (b.x as f64, b.y as f64, b.w as f64, b.h as f64)
}

/// Where a plate point (mm) is in the window (logical pixels), through the view's camera.
fn on_screen(app: &TestApp, mm: [f64; 3]) -> (f64, f64) {
    let (x, y, w, h) = view_rect(app);
    let cam = camera(app);
    let c = cam.view_projection((w / h) as f32).transform_vec4(vec4((mm[0] * 0.001) as f32, (mm[1] * 0.001) as f32, (mm[2] * 0.001) as f32, 1.0));
    let (nx, ny) = ((c.x / c.w) as f64, (c.y / c.w) as f64);
    (x + (nx * 0.5 + 0.5) * w, y + (0.5 - ny * 0.5) * h)
}

/// A grab's pixels.
struct Grab {
    width: usize,
    rgb: Vec<[u8; 3]>,
}

impl Grab {
    fn decode(path: &std::path::Path) -> Grab {
        let bytes = std::fs::read(path).expect("the grab");
        let mut dec = PngDecoder::new(ZCursor::new(&bytes));
        let px = dec.decode_raw().expect("png");
        let (width, height) = dec.dimensions().expect("dims");
        let n = dec.colorspace().expect("colorspace").num_components();
        let rgb = (0..width * height).map(|i| [px[i * n], px[i * n + 1], px[i * n + 2]]).collect();
        Grab { width, rgb }
    }

    fn at(&self, x: usize, y: usize) -> [u8; 3] {
        self.rgb[y * self.width + x]
    }

    /// The pixels over a rectangle (device pixels) unlike `sky` (each channel within 12).
    fn unlike(&self, x0: usize, y0: usize, x1: usize, y1: usize, sky: impl Fn(usize) -> [u8; 3]) -> Vec<bool> {
        let mut out = Vec::new();
        for y in y0..y1 {
            let s = sky(y);
            for x in x0..x1 {
                let p = self.at(x, y);
                out.push((0..3).any(|k| (p[k] as i32 - s[k] as i32).abs() > 12));
            }
        }
        out
    }

    /// The mean colour over a rectangle (device pixels).
    fn mean(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> [f64; 3] {
        let mut sum = [0.0; 3];
        for y in y0..y1 {
            for x in x0..x1 {
                let p = self.at(x, y);
                (0..3).for_each(|k| sum[k] += p[k] as f64);
            }
        }
        let n = ((x1 - x0) * (y1 - y0)).max(1) as f64;
        sum.map(|c| c / n)
    }
}

/// How much brighter `a` is than `b` (the mean of the channels).
fn brighter(a: [f64; 3], b: [u8; 3]) -> f64 {
    (a.iter().sum::<f64>() - b.iter().map(|c| *c as f64).sum::<f64>()) / 3.0
}

/// Red over the larger of green and blue: the Feature legend's outer wall (orange) is 2.1, its
/// top surface (red) 3.8; its inner walls (green) are 0.8 and its sparse infill (magenta) 1.2.
fn redness(c: [f64; 3]) -> f64 {
    c[0] / c[1].max(c[2]).max(1.0)
}

/// The view's finished picture (settled, its still average complete, two grabs alike), kept as
/// `name` in a run that keeps its outputs.
fn finished_grab(app: &TestApp, name: &str) -> Grab {
    loop {
        wait_until(app, "the view's finished picture", || {
            let r = recording(app);
            r.get("settled").and_then(Value::as_bool) == Some(true) && r.get("still_samples").and_then(Value::as_u64) == Some(makepad_render::STILL_AA_SAMPLES as u64)
        });
        let path = app.screenshot();
        let first = std::fs::read(&path).expect("the grab");
        let again = app.screenshot();
        if std::fs::read(&again).expect("the grab") == first {
            if let Some(dir) = std::env::var_os("DEVHUB_KEEP") {
                std::fs::copy(&again, std::path::Path::new(&dir).join(format!("{name}.png"))).expect("keep the grab");
            }
            return Grab::decode(&again);
        }
    }
}

/// A command from the palette, and the camera's move to rest after it.
fn command(app: &TestApp, text: &str) {
    let before = camera_json(app);
    app.press_key(KeyCode::Escape);
    app.press_key_with_modifiers(KeyCode::KeyP, if cfg!(target_os = "macos") { KeyModifiers { shift: true, logo: true, ..Default::default() } } else { KeyModifiers { shift: true, control: true, ..Default::default() } });
    app.type_text(text);
    app.press_key(KeyCode::ReturnKey);
    wait_until(app, &format!("{text}: the camera"), || camera_json(app) != before);
    wait_until(app, &format!("{text}: the camera at rest"), || settled(app));
}

/// The wheel at a window point, once, and the camera's change after it.
fn wheel(app: &TestApp, at: (f64, f64), dy: f64) {
    let before = camera_json(app);
    app.scroll_at(at.0, at.1, 0.0, dy, None);
    wait_until(app, "the wheel's zoom", || camera_json(app) != before);
}

/// The kept view preferences, one `key=value` a line.
fn pref(prefs: &std::path::Path, key: &str) -> Option<String> {
    std::fs::read_to_string(prefs).ok()?.lines().find_map(|l| l.strip_prefix(&format!("{key}=")).map(str::to_string))
}

#[test]
fn an_orthographic_view_shows_and_slices_the_whole_part_close_in() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "ortho_view").unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), format!("{}x{}@{}", WINDOW.0, WINDOW.1, WINDOW.2)];
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let prefs = std::env::temp_dir().join(format!("fab-ortho-view-{}-{nanos}.prefs", std::process::id()));
    // the code split off (the view's full width)
    std::fs::write(&prefs, "code=0\n").unwrap();
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let kept = prefs.clone();
    let result = run_with_config(config, move |app: TestApp| {
        let prefs = kept;
        let mut failed: Vec<String> = Vec::new();
        let mut head = String::new();
        wait_until(&app, "the fab tools", || call(&app, "inspect", "{}").map(|v| head = head_of(&v)).is_ok());
        let batch = format!(r#"{{"request_id":"s","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(PART.to_string()).to_json());
        head = head_of(&call(&app, "script", &batch).expect("script"));
        let batch = format!(r#"{{"request_id":"p","expected":{head:?},"edits":{}}}"#, Value::Str(PLATE.to_string()).to_json());
        call(&app, "place", &batch).expect("place");
        wait_until(&app, "the part drawn", || recording(&app).get("parts_drawn").and_then(Value::as_u64).is_some_and(|n| n >= 1));
        wait_until(&app, "the camera at rest", || settled(&app));
        let scale = finished_grab(&app, "1-perspective").width as f64 / WINDOW.0;

        // Zoomed in by the wheel onto the slab's top left of the post, as close as it goes.
        let mut step = 0;
        while camera(&app).distance() > 0.021 {
            assert!(step < 120, "the wheel did not close in: {:?}", camera(&app));
            wheel(&app, on_screen(&app, [115.0, 100.0, 6.0]), -80.0);
            step += 1;
        }
        wait_until(&app, "the camera at rest", || settled(&app));
        finished_grab(&app, "2-close-perspective");

        // The laptop's 5: orthographic, kept for the next start.
        let (x, y, w, h) = view_rect(&app);
        let persp = camera(&app);
        let before = camera_json(&app);
        app.locator(Selector::id("viewport")).press_key(KeyCode::Key5);
        wait_until(&app, "5: orthographic", || camera_json(&app) != before && camera(&app).ortho);
        wait_until(&app, "the camera at rest", || settled(&app));
        // Back and forth: the perspective view comes back as it was, and the orthographic one.
        let ortho = camera(&app);
        for (to_ortho, was) in [(false, persp), (true, ortho)] {
            let before = camera_json(&app);
            app.locator(Selector::id("viewport")).press_key(KeyCode::Key5);
            wait_until(&app, "5 again", || camera_json(&app) != before && camera(&app).ortho == to_ortho);
            wait_until(&app, "the camera at rest", || settled(&app));
            let now = camera(&app);
            // the angle between the looks from both their sine and cosine: an arc cosine near 1
            // reads a one-ulp dot of the same look as 3.5e-4 rad, far over the bound
            let (f, g) = (now.forward(), was.forward());
            let turn = Vec3f::cross(f, g).length().atan2(f.dot(g));
            let (moved, size) = if to_ortho {
                ((now.target - was.target).length(), (now.ortho_height - was.ortho_height).abs())
            } else {
                ((now.eye - was.eye).length().max((now.target - was.target).length()), (now.distance() - was.distance()).abs())
            };
            println!("[fab-ortho] round trip to {}: the look turned {turn:.2e} rad, moved {moved:.2e} m, its size {size:.2e} m off", if to_ortho { "orthographic" } else { "perspective" });
            if turn > 1e-5 || moved > 1e-6 || size > 1e-6 {
                failed.push(format!("perspective, orthographic and back: the {} view came back {turn:.2e} rad, {moved:.2e} m, {size:.2e} m off: {was:?} -> {now:?}", if to_ortho { "orthographic" } else { "perspective" }));
            }
        }
        finished_grab(&app, "3-close-orthographic");
        if pref(&prefs, "ortho").as_deref() != Some("1") {
            failed.push(format!("5 left the view prefs {:?}", std::fs::read_to_string(&prefs)));
        }

        // The Right view, the post between the eye's old plane and the view's right, zoomed back
        // out till the whole part fits.
        command(&app, "View: Right");
        let mut step = 0;
        while camera(&app).ortho_height < 0.1 {
            assert!(step < 120, "the wheel did not zoom out: {:?}", camera(&app));
            wheel(&app, (x + w * 0.5, y + h * 0.5), 80.0);
            step += 1;
        }
        wait_until(&app, "the camera at rest", || settled(&app));
        let normal = finished_grab(&app, "4-right-orthographic");
        // The post's middle (its width's middle half, its height's above the slab), and the sky at
        // the same rows left of the part.
        let dev = |p: (f64, f64)| ((p.0 * scale) as usize, (p.1 * scale) as usize);
        let (a, b) = (dev(on_screen(&app, [148.0, 97.0, 16.0])), dev(on_screen(&app, [148.0, 103.0, 40.0])));
        let (x0, x1, y0, y1) = (a.0.min(b.0), a.0.max(b.0), a.1.min(b.1), a.1.max(b.1));
        let sky_x = dev(on_screen(&app, [148.0, 70.0, 20.0])).0;
        let post = normal.unlike(x0, y0, x1, y1, |row| normal.at(sky_x, row));
        let shown = post.iter().filter(|p| **p).count();
        println!("[fab-ortho] the post's middle: {shown} of {} pixels unlike the sky ({x0}..{x1}, {y0}..{y1}), the camera {:?}", post.len(), camera(&app));
        if post.is_empty() || shown < post.len() {
            failed.push(format!("orthographic, close in: the post shows {shown} of {} pixels", post.len()));
        }
        let lit = brighter(normal.mean(x0, y0, x1, y1), normal.at(sky_x, (y0 + y1) / 2));
        println!("[fab-ortho] the post's middle: {lit:.0} levels brighter than the sky");
        if lit < 40.0 {
            failed.push(format!("orthographic, close in: the white post is {lit:.0} levels brighter than the sky"));
        }

        // The Sliced view: the part where the Normal view has it.
        let (c, d) = (dev(on_screen(&app, [150.0, 74.0, -4.0])), dev(on_screen(&app, [150.0, 126.0, 50.0])));
        let (bx0, bx1, by0, by1) = (c.0.min(d.0), c.0.max(d.0), c.1.min(d.1), c.1.max(d.1));
        let sky = |g: &Grab| -> Vec<[u8; 3]> { (0..by1 + 1).map(|row| g.at(bx0.saturating_sub(8), row)).collect() };
        let normal_sky = sky(&normal);
        let outline_normal = normal.unlike(bx0, by0, bx1, by1, |row| normal_sky[row]);
        render_mode::pick(&app, "Sliced");
        wait_until(&app, "the slice", || call(&app, "slice", "{}").is_ok_and(|t| whole_slice(&t)));
        let sliced = finished_grab(&app, "5-sliced-orthographic");
        let sliced_sky = sky(&sliced);
        let outline_sliced = sliced.unlike(bx0, by0, bx1, by1, |row| sliced_sky[row]);
        let both = outline_normal.iter().zip(&outline_sliced).filter(|(a, b)| **a && **b).count();
        let either = outline_normal.iter().zip(&outline_sliced).filter(|(a, b)| **a || **b).count();
        let iou = both as f64 / either.max(1) as f64;
        println!("[fab-ortho] Normal and Sliced outlines: {both} of {either} pixels shared ({iou:.3})");
        if iou < 0.85 {
            failed.push(format!("orthographic: the Sliced view's outline shares {iou:.3} of the Normal view's"));
        }
        let wall = redness(sliced.mean(x0, y0, x1, y1));
        println!("[fab-ortho] the sliced post's middle: red {wall:.2} times green or blue");
        if wall < 1.6 {
            failed.push(format!("orthographic: from the side the sliced post is not its outer wall (red {wall:.2} times green or blue)"));
        }

        // From the Top view, a box drawn on the bed beside the part with the No Trees tool.
        command(&app, "View: Top");
        app.locator(Selector::id("t_no_trees")).wait_visible().click();
        let (from, to) = (on_screen(&app, [70.0, 85.0, 0.0]), on_screen(&app, [82.0, 115.0, 0.0]));
        let mut drag = app.press_at(from.0, from.1);
        drag.move_by((to.0 - from.0) * 0.5, (to.1 - from.1) * 0.5);
        drag.move_by((to.0 - from.0) * 0.5, (to.1 - from.1) * 0.5);
        drag.release();
        wait_until(&app, "the box drawn on the bed: its no-trees area", || call(&app, "inspect", "{}").is_ok_and(|t| t.contains("no_trees")));
        wait_until(&app, "the camera at rest", || settled(&app));
        let top = finished_grab(&app, "6-no-trees-orthographic");
        // The post's top, its middle half (the post is 136..148 by 94..106 mm, 46 mm up).
        let (e, f) = (dev(on_screen(&app, [139.0, 97.0, 46.0])), dev(on_screen(&app, [145.0, 103.0, 46.0])));
        let surface = redness(top.mean(e.0.min(f.0), e.1.min(f.1), e.0.max(f.0), e.1.max(f.1)));
        println!("[fab-ortho] the sliced post's top: red {surface:.2} times green or blue");
        if surface < 2.5 {
            failed.push(format!("orthographic: from the top the sliced post's top is not its top surface (red {surface:.2} times green or blue)"));
        }

        // The view bar's View options tile: perspective and back, each kept.
        view_options::toggle(&app);
        for (on, word) in [(false, "0"), (true, "1")] {
            if app.locator(view_options::tile("Orthographic")).try_wait_visible().is_err() {
                failed.push("no Orthographic tile in the view bar's View options".into());
                break;
            }
            app.locator(view_options::tile("Orthographic")).click();
            wait_until(&app, "the button's projection", || camera(&app).ortho == on);
            app.locator(view_options::tile("Orthographic")).wait_checked(on);
            if pref(&prefs, "ortho").as_deref() != Some(word) {
                failed.push(format!("the button left the view prefs {:?}", std::fs::read_to_string(&prefs)));
            }
        }
        assert!(failed.is_empty(), "{}", failed.join("\n"));
    });
    let _ = std::fs::remove_file(&prefs);
    result.unwrap();
}

//! Realtime's screen AO shows in both views: an inside corner reads darker than the flat face
//! beside it under the same light, and the flat face away from it reads as without the AO.
//!
//! Why (user): "can you start up the SSAO for our normal/sliced renderer? ... do it just like you
//! do it with normal meshes". The renderer's screen AO runs over the depth of everything the
//! viewport draws (traced parts, print beads, meshes) and each surface takes it by the model
//! lanes' rule: its ambient fill times 1 - (1 - ao) * strength, its direct light times the
//! lanes' `ao_direct`.
//!
//! One plate: a 70 mm square slab, 2 mm thick, with a 20 mm square block 12 mm tall in its middle.
//! The Top view made orthographic and zoomed in by the wheel till the slab is 560 px across
//! (device scale 2), so the slab's top reads square on: one normal, one light. Down each of the
//! block's four sides, across the middle half of it, two strips of the slab's top: `NEAR` (the
//! inside corner at the block's foot) and `FAR` (the open face, beyond the AO's reach of the
//! block). Read in the Normal view (traced tiles) and the Sliced view (the printed beads), each
//! with the screen AO on and with it off (the viewport's `debug_off` bit 32), so each strip is
//! the same pixels under the same light both times:
//!
//! - the AO darkens the inside corner on every side by at least `CORNER` of its brightness;
//! - and the open face by at most `FLAT` (luma, 0..255): no band across a flat face;
//! - in the Normal view the open face reads the same on all four sides, within `EVEN`, AO on or
//!   off: one flat face under one light shows no highlight. The sun's lobe takes the view's
//!   direction: an orthographic view's axis, not the way to where its eye stands.
//!
//! Measured (the Product table studio): the inside corner 2.6 % darker on the block's sunlit
//! sides, 8 % in its shadow (Normal), 2.8 % on every side (Sliced: beads take no cascades); the
//! open face the same with the AO and without in both views; the open face 208.0 on all four
//! sides (Normal), 188.3 to 188.4 (Sliced). The Sliced view's skirt rings the slab past a gap of
//! bed: taken for the slab, it put the far strip over the slab's edge onto the bed at its foot,
//! an inside corner the AO darkens (1.1 on one side). The orthographic view's eye stands just
//! behind what it shows: the AO measures depth along the view's axis, or a texel's offset off the
//! axis would read as depth and band the open face (1.8 darker on one side); the sun's lobe seen
//! from the eye's place lit a highlight on it (217.7 on one side, 208.0 on another).
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

use makepad_strict_json::Value;
use makepad_test::{run_with_config, KeyCode, KeyModifiers, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::time::Duration;

/// How much the AO darkens the inside corner at least, a fraction of its brightness (on the sunlit
/// sides the AO takes the direct light's share, in the block's shadow the fill's).
const CORNER: f64 = 0.02;
/// And the open face at most (luma, 0..255).
const FLAT: f64 = 1.0;
/// How far apart the open face reads on the block's four sides at most, in the Normal view (luma).
const EVEN: f64 = 1.0;

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

/// The slab's and the block's half widths, the near and far strips' distance from the block
/// (mm): the far one past the AO's 20 mm radius.
const SLAB: f64 = 35.0;
const BLOCK: f64 = 10.0;
const NEAR: (f64, f64) = (0.5, 1.5);
const FAR: (f64, f64) = (21.0, 23.0);
/// How far (mm) the Sliced view's slab may lie off the Normal view's: the printed edge against
/// the traced one, at the same zoom.
const SAME: f64 = 0.5;

const PART: &str = "// A slab with a block in its middle: an inside corner all round the block's foot.
use mod.model.*
use mod.fab.*
let m = mod.model
let slab = m.box({size: [70mm 2mm 70mm] at: [0, 1mm, 0]})
let rise = m.box({size: [20mm 12mm 20mm] at: [0, 8mm, 0]})
let block = m.union(slab, rise)
object(block)
";

/// The part on the plate where the camera finds it (`fab.script` added its script, and placed it
/// in the default filament).
const PLATE: &str = r#"place("block", object: "block", at: [120mm 100mm], name: "Block")"#;

/// The viewport's widget path, for its live diagnostics.
const VIEWPORT: &str = "body.chat_split.shell.main.dock.tab_viewport.viewport";

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
    app.try_wait_until_within(HANG, || (!ok()).then(|| format!("waiting for {what}"))).unwrap_or_else(|why| {
        panic!("{why}; timed out waiting for {what}; the viewport reports {}", app.locator(Selector::id("viewport")).snapshot().description);
    });
}

/// What the viewport reports of the recording on display.
fn recording(app: &TestApp) -> Value {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).unwrap_or(Value::Null)
}

/// The recording on display is the view's finished picture (settled, its still average
/// complete), with the screen AO `ao` (None: either).
fn finished(app: &TestApp, ao: Option<bool>) -> bool {
    let r = recording(app);
    r.get("settled").and_then(Value::as_bool) == Some(true)
        && r.get("still_samples").and_then(Value::as_u64) == Some(makepad_render::STILL_AA_SAMPLES as u64)
        && ao.is_none_or(|on| r.get("screen_ao").and_then(Value::as_bool) == Some(on))
}

/// The view's finished picture: settled, its still average complete, two grabs alike.
fn finished_grab(app: &TestApp, ao: Option<bool>) -> (std::path::PathBuf, Grab) {
    wait_poll(app, || {
        if !finished(app, ao) { return Err("waiting for the finished picture".to_string()); }
        let first = std::fs::read(app.screenshot()).expect("the grab");
        let path = app.screenshot();
        if std::fs::read(&path).expect("the grab") == first {
            let grab = Grab::decode(&path);
            return Ok((path, grab));
        }
        Err("waiting for two matching grabs".to_string())
    })
}

/// The screen AO on or off: the viewport's live `debug_off` bit 32.
fn screen_ao(app: &TestApp, on: bool) {
    let value = if on { "0" } else { "32" };
    remote(app, &["tweak".into(), "apply".into(), format!("path={VIEWPORT}"), "prop=debug_off".into(), format!("value={value}")]).expect("the viewport's debug_off");
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

    fn luma(&self, x: usize, y: usize) -> f64 {
        let [r, g, b] = self.rgb[y * self.width + x];
        0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64
    }

    /// The part: its light filament, lit from above; not the plate's greys (its face, its grid,
    /// its rim).
    fn part(&self, x: usize, y: usize) -> bool {
        self.luma(x, y) > 150.0
    }

    /// The slab's square in `x0..x1` by `y0..y1`: the longest run of columns at least half full
    /// of the part (first, last), then of rows across those columns. The longest run, not the
    /// first and the last such column: in the Sliced view the skirt rings the slab past a gap of
    /// bed, and its sides are full columns of the same filament.
    fn slab(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> Option<Slab> {
        let span = |counts: Vec<usize>, base: usize| {
            let full = counts.iter().copied().max().unwrap_or(0) / 2;
            let filled: Vec<bool> = counts.iter().map(|&c| full > 0 && c > full).collect();
            let (mut best, mut i) = (None::<(usize, usize)>, 0);
            while i < filled.len() {
                if !filled[i] {
                    i += 1;
                    continue;
                }
                let start = i;
                while i < filled.len() && filled[i] {
                    i += 1;
                }
                if best.is_none_or(|(a, b)| i - start > b + 1 - a) {
                    best = Some((start, i - 1));
                }
            }
            best.map(|(a, b)| (base + a, base + b))
        };
        let cols = span((x0..x1).map(|x| (y0..y1).filter(|&y| self.part(x, y)).count()).collect(), x0)?;
        let rows = span((y0..y1).map(|y| (cols.0..=cols.1).filter(|&x| self.part(x, y)).count()).collect(), y0)?;
        Some((cols, rows))
    }

    /// The mean luma over a rectangle (device pixels, ends excluded).
    fn mean(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> f64 {
        let (xa, xb) = (x0.min(x1).round() as usize, x0.max(x1).round() as usize);
        let (ya, yb) = (y0.min(y1).round() as usize, y0.max(y1).round() as usize);
        let mut sum = 0.0;
        for y in ya..yb {
            for x in xa..xb {
                sum += self.luma(x, y);
            }
        }
        sum / ((xb - xa) * (yb - ya)).max(1) as f64
    }
}

/// What one picture shows: per side of the block (left, right, up, down the screen) the near
/// and the far strip's mean luma.
fn read(grab: &Grab, slab: Slab) -> Vec<(f64, f64)> {
    let ((cx0, cx1), (cy0, cy1)) = slab;
    let (cx, cy) = ((cx0 + cx1) as f64 * 0.5, (cy0 + cy1) as f64 * 0.5);
    let mm = ((cx1 - cx0) + (cy1 - cy0)) as f64 * 0.5 / (2.0 * SLAB);
    // across the middle half of the block's side
    let half = BLOCK * 0.5 * mm;
    let strip = |(a, b): (f64, f64), sx: f64, sy: f64| {
        let (d0, d1) = ((BLOCK + a) * mm, (BLOCK + b) * mm);
        if sx != 0.0 {
            grab.mean(cx + sx * d0, cy - half, cx + sx * d1, cy + half)
        } else {
            grab.mean(cx - half, cy + sy * d0, cx + half, cy + sy * d1)
        }
    };
    [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)].iter().map(|&(sx, sy)| (strip(NEAR, sx, sy), strip(FAR, sx, sy))).collect()
}

/// The slab's square in a picture: its first and last column, its first and last row.
type Slab = ((usize, usize), (usize, usize));

/// The window, in logical pixels, at device scale 2.
const WINDOW: (f64, f64, f64) = (1600.0, 1000.0, 2.0);

/// The finished picture (with the screen AO `ao`, None: either), the slab's square in it.
fn look(app: &TestApp, ao: Option<bool>) -> (std::path::PathBuf, Grab, f64, Option<Slab>) {
    let (path, grab) = finished_grab(app, ao);
    let b = app.locator(Selector::id("viewport")).snapshot().bounds;
    let scale = grab.width as f64 / WINDOW.0;
    let (x0, y0) = ((b.x as f64 * scale) as usize, (b.y as f64 * scale) as usize);
    let (x1, y1) = (((b.x + b.w) as f64 * scale) as usize, ((b.y + b.h) as f64 * scale) as usize);
    // the view's middle, where the wheel keeps the slab: not the toolbar, the gizmo or the
    // view's buttons over its edges
    let (w, h) = ((x1 - x0) / 5, (y1 - y0) / 5);
    let slab = grab.slab(x0 + w, y0 + h, x1 - w, y1 - h);
    (path, grab, scale, slab)
}

/// One view's readings with the AO on and off, and their checks; the slab found in its pictures.
/// `normal`: the slab the Normal view found, which this view must find too (same zoom).
fn check(app: &TestApp, view: &str, normal: Option<Slab>) -> Slab {
    let mut readings = Vec::new();
    let mut found = None;
    for on in [true, false] {
        screen_ao(app, on);
        let (path, grab, _, slab) = look(app, Some(on));
        let Some(slab) = slab else { panic!("{view}: the slab not found; see {path:?}") };
        // The strips are measured from the slab's square: another square (the skirt round the
        // slab) puts them on other pixels, the far one over the slab's edge onto the bed.
        if let Some(n) = normal {
            let ((cx0, cx1), (cy0, cy1)) = n;
            let px = SAME * ((cx1 - cx0) + (cy1 - cy0)) as f64 * 0.5 / (2.0 * SLAB);
            let ((x0, x1), (y0, y1)) = slab;
            let apart = [x0.abs_diff(cx0), x1.abs_diff(cx1), y0.abs_diff(cy0), y1.abs_diff(cy1)].into_iter().max().unwrap_or(0);
            assert!(apart as f64 <= px, "{view}: the slab found at {slab:?}, {apart} px off the Normal view's {n:?}; see {path:?}");
        }
        found = Some(slab);
        let sides = read(&grab, slab);
        println!(
            "[fab-screen-ao] {view}, AO {}: slab {:?} (columns, rows); near/far per side {}",
            if on { "on" } else { "off" },
            slab,
            sides.iter().map(|(n, f)| format!("{n:.1}/{f:.1}")).collect::<Vec<_>>().join(", ")
        );
        readings.push((path, sides));
    }
    let [(on_path, on), (off_path, off)] = &readings[..] else { unreachable!() };
    for (k, side) in ["left", "right", "up", "down"].iter().enumerate() {
        let ((near, far), (near_off, far_off)) = (on[k], off[k]);
        assert!(near <= near_off * (1.0 - CORNER), "{view}: the inside corner on the block's {side} side reads {near:.1} with the AO and {near_off:.1} without, under {:.0} % darker; see {on_path:?} and {off_path:?}", CORNER * 100.0);
        assert!((far - far_off).abs() <= FLAT, "{view}: the open face on the {side} side reads {far:.1} with the AO and {far_off:.1} without, a band across a flat face; see {on_path:?} and {off_path:?}");
    }
    if view == "Normal" {
        for (sides, path) in [(on, on_path), (off, off_path)] {
            let (lo, hi) = sides.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &(_, far)| (lo.min(far), hi.max(far)));
            assert!(hi - lo <= EVEN, "{view}: the open face reads {lo:.1} to {hi:.1} round the block, a highlight on a flat face; see {path:?}");
        }
    }
    found.expect("the slab of both pictures")
}

#[test]
fn an_inside_corner_reads_darker_than_the_open_face_in_both_views() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "screen_ao").unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), format!("{}x{}@{}", WINDOW.0, WINDOW.1, WINDOW.2)];
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let prefs = std::env::temp_dir().join(format!("fab-screen-ao-{}-{nanos}.prefs", std::process::id()));
    // The code split off (the view's full width), the printed surface (each bead in its filament).
    std::fs::write(&prefs, "code=0\nsurface=1\n").unwrap();
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let result = run_with_config(config, |app: TestApp| {
        let mut head = String::new();
        wait_until(&app, "the fab tools", || call(&app, "inspect", "{}").map(|v| head = head_of(&v)).is_ok());
        let batch = format!(r#"{{"request_id":"s","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(PART.to_string()).to_json());
        head = head_of(&call(&app, "script", &batch).expect("script"));
        let batch = format!(r#"{{"request_id":"p","expected":{head:?},"edits":{}}}"#, Value::Str(PLATE.to_string()).to_json());
        call(&app, "place", &batch).expect("place");
        // drawn from its field once that is compiled, the camera's glide to it over
        wait_until(&app, "the part drawn", || recording(&app).get("parts_drawn").and_then(Value::as_u64).is_some_and(|n| n >= 1));
        wait_until(&app, "the camera at rest", || recording(&app).get("settled").and_then(Value::as_bool) == Some(true));
        // From the command palette: the Top view, orthographic (the slab's top square on).
        for command in ["View: Top", "Toggle Orthographic"] {
            let camera = recording(&app).get("camera").map(Value::to_json);
            app.press_key(KeyCode::Escape);
            app.press_key_with_modifiers(KeyCode::KeyP, if cfg!(target_os = "macos") { KeyModifiers { shift: true, logo: true, ..Default::default() } } else { KeyModifiers { shift: true, control: true, ..Default::default() } });
            app.type_text(command);
            app.press_key(KeyCode::ReturnKey);
            wait_until(&app, &format!("{command}: the camera"), || recording(&app).get("camera").map(Value::to_json) != camera);
            wait_until(&app, &format!("{command}: the camera at rest"), || recording(&app).get("settled").and_then(Value::as_bool) == Some(true));
        }
        let camera = recording(&app).get("camera").cloned().unwrap_or(Value::Null);
        assert_eq!(camera.get("ortho").and_then(Value::as_bool), Some(true), "the Top view, orthographic: {}", camera.to_json());
        // looking straight down (the view's up is +z)
        let num = |v: &Value| match v {
            Value::F64(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        };
        let at = |k: &str| camera.get(k).and_then(Value::as_arr).map(|a| a.iter().filter_map(num).collect::<Vec<_>>()).unwrap_or_default();
        let (eye, target) = (at("eye"), at("target"));
        assert!(eye.len() == 3 && target.len() == 3 && (eye[0] - target[0]).hypot(eye[1] - target[1]) < 1e-3 * (eye[2] - target[2]), "the Top view: {}", camera.to_json());
        // Zoomed in by the wheel at the slab's middle till it is 560 px across.
        let mut step = 0;
        loop {
            let (path, _, scale, slab) = look(&app, None);
            let Some(((x0, x1), (y0, y1))) = slab else {
                panic!("step {step}: the slab not found; the viewport reports {}; see {path:?}", app.locator(Selector::id("viewport")).snapshot().description)
            };
            if x1 - x0 >= 560 {
                break;
            }
            assert!(step < 24, "the wheel did not zoom in to the slab");
            let at = ((x0 + x1) as f64 * 0.5 / scale, (y0 + y1) as f64 * 0.5 / scale);
            let camera = recording(&app).get("camera").map(Value::to_json);
            app.scroll_at(at.0, at.1, 0.0, -80.0, None);
            wait_until(&app, "the wheel's zoom", || recording(&app).get("camera").map(Value::to_json) != camera);
            step += 1;
        }
        let normal = check(&app, "Normal", None);
        app.locator(Selector::id("rm_slice")).wait_visible().click();
        app.locator(Selector::id("rm_slice")).wait_checked(true);
        // the whole slice, not its live tiles before it (they have no skirt yet)
        wait_until(&app, "the slice", || call(&app, "slice", "{}").is_ok_and(|t| whole_slice(&t)));
        check(&app, "Sliced", Some(normal));
    });
    let _ = std::fs::remove_file(&prefs);
    result.unwrap();
}

fn wait_poll<T>(app: &TestApp, mut look: impl FnMut() -> Result<T, String>) -> T {
    let mut got = None;
    app.try_wait_until_within(HANG, || match look() {
        Ok(value) => {
            got = Some(value);
            None
        }
        Err(why) => Some(why),
    }).unwrap_or_else(|why| {
        panic!("{why}; the viewport reports {}", app.locator(Selector::id("viewport")).snapshot().description);
    });
    got.expect("the wait ended on an answer")
}

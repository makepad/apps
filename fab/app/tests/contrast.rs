//! The words in Fab's chrome read on what they are drawn on: measured in the
//! grab, not in the token table.
//!
//! Why (user, on a Mac): "The UI colours on mac are all out of whack: it's
//! grey with no contrast." Standalone on a Mac no sheet was worn, so Fab ran
//! under the library's plain base theme: a mid-grey ground, and second and
//! disabled inks that are white at a few percent. The feature kinds, the
//! feature count, the caption's "3 parts · 21 solids", the inactive "Parts"
//! tab and the Add chevron were grey on grey.
//!
//! Each element is found by its accessibility node; inside its box the
//! ground is the most common colour and the ink the pixel that stands
//! furthest from it (WCAG contrast). Words are held to 4.5:1 (AA), the
//! chevron, a graphic, to 3:1. Run under the host's own sheet (what a
//! standalone start wears: macOS dark on a Mac, Omarchy on Linux) and under
//! macOS light, macOS dark and Omarchy named outright. A sheet is worn from
//! the start, so each is its own boot, with one small part of three features.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, NodeMatch, Role, Selector, TestApp, TestConfig};
use makepad_widgets::theme_tokens::contrast;
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::collections::HashMap;
use std::time::Duration;

const WORDS: f64 = 4.5;
const GRAPHIC: f64 = 3.0;

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

struct Frame {
    width: usize,
    height: usize,
    rgb: Vec<[u8; 3]>,
}

impl Frame {
    fn decode(path: &std::path::Path) -> Frame {
        let bytes = std::fs::read(path).expect("the grab");
        let mut dec = PngDecoder::new(ZCursor::new(&bytes));
        let px = dec.decode_raw().expect("png");
        let (width, height) = dec.dimensions().expect("dims");
        let n = dec.colorspace().expect("colorspace").num_components();
        let rgb = (0..width * height).map(|i| [px[i * n], px[i * n + 1], px[i * n + 2]]).collect();
        Frame { width, height, rgb }
    }

    /// The ground (the commonest colour) and the ink (the pixel furthest from
    /// it) inside a box in device pixels, and their contrast.
    fn reading(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> (u32, u32, f64) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        let mut counts: HashMap<[u8; 3], usize> = HashMap::new();
        for y in y0..y1 {
            for x in x0..x1 {
                *counts.entry(self.rgb[y * self.width + x]).or_default() += 1;
            }
        }
        let rgba = |c: [u8; 3]| ((c[0] as u32) << 24) | ((c[1] as u32) << 16) | ((c[2] as u32) << 8) | 0xff;
        let ground = counts.iter().max_by_key(|(_, n)| **n).map(|(c, _)| rgba(*c)).expect("an empty box");
        let (ink, ratio) = counts.keys().map(|c| (rgba(*c), contrast(ground, rgba(*c)))).fold((ground, 1.0), |best, c| if c.1 > best.1 { c } else { best });
        (ground, ink, ratio)
    }
}

/// `what` under the bar, or None.
fn check(frame: &Frame, dpi: f64, what: &str, node: &NodeMatch, left: Option<f64>, need: f64) -> Option<String> {
    let d = |v: f64| (v * dpi).round().max(0.0) as usize;
    let (x, y, w, h) = (node.x() as f64, node.y() as f64, node.width() as f64, node.height() as f64);
    let w = left.map_or(w, |l| l.min(w));
    let (ground, ink, ratio) = frame.reading(d(x), d(y), d(x + w), d(y + h));
    (ratio < need).then(|| format!("{what}: ink #{:06x} on #{:06x} {ratio:.2}:1 (need {need})", ink >> 8, ground >> 8))
}

const PART: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let slab = m.box({size: [40mm 6mm 30mm]})
let hole = m.cylinder({radius: 4mm height: 10mm})
let plate = m.subtract(slab, hole)
object(plate)
"##;

fn measure(app: TestApp, sheet: &str) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let head = wait_poll(&app, || call(&app, "inspect", "{}").map(|line| head_of(&text(&line))).map_err(|e| format!("the tools never appeared: {e}")));
    // One part with a feature tree of several kinds.
    let batch = format!(r#"{{"request_id":"part","expected":{head:?},"script":"plate.cad.splash","source":{}}}"#, Value::Str(PART.to_string()).to_json());
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    let batch = format!(r#"{{"request_id":"place","expected":{head:?},"edits":{}}}"#, Value::Str(r#"place("plate", at: [150mm 165mm])"#.to_string()).to_json());
    call(&app, "place", &batch).expect("place");
    app.locator(Selector::role(Role::Tab).name("Features")).wait_visible().click();
    // The feature rows, once two looks in a row agree on them (the layout has settled).
    let rows_of = |app: &TestApp| -> Vec<NodeMatch> { app.widget_snapshot().nodes().into_iter().filter(|n| n.visible() && n.node.role == Role::ListItem && n.width() > 0).collect() };
    let mut rows = rows_of(&app);
    wait_poll(&app, || {
        std::thread::sleep(Duration::from_millis(50));
        let now = rows_of(&app);
        let same = now.len() == rows.len() && now.iter().zip(&rows).all(|(a, b)| a.node.name == b.node.name && a.bounds == b.bounds);
        rows = now;
        if same && rows.len() >= 3 {
            return Ok(());
        }
        Err(format!("the feature tree shows {} rows", rows.len()))
    });
    let nodes = app.widget_snapshot().nodes();
    let by_id = |id: &str| nodes.iter().find(|n| n.visible() && n.path.rsplit('.').next() == Some(id)).cloned().unwrap_or_else(|| panic!("no visible `{id}`"));
    let count = by_id("count");
    let caption = by_id("doc_state");
    let chevron = by_id("chevron");
    let parts_tab = app.locator(Selector::role(Role::Tab).name("Parts")).wait_visible().snapshot();
    assert!(parts_tab.width() > 0 && parts_tab.height() > 0, "the Parts tab has no box");
    let shot = app.screenshot();
    let frame = Frame::decode(&shot);
    let widest = nodes.iter().map(|n| n.width()).max().unwrap_or(1) as f64;
    let dpi = (frame.width as f64 / widest).round().max(1.0);

    let mut failed = Vec::new();
    // The kind column: the row's first 78 points (8 in, 74 for the kind).
    for row in rows.iter().filter(|r| !r.node.states.contains(makepad_test::States::SELECTED)) {
        failed.extend(check(&frame, dpi, &format!("kind of {:?}", row.node.name), row, Some(78.0), WORDS));
    }
    failed.extend(check(&frame, dpi, "feature count", &count, None, WORDS));
    failed.extend(check(&frame, dpi, "caption subtitle", &caption, None, WORDS));
    failed.extend(check(&frame, dpi, "Parts tab (inactive)", &parts_tab, None, WORDS));
    failed.extend(check(&frame, dpi, "Add chevron", &chevron, None, GRAPHIC));
    assert!(failed.is_empty(), "contrast under {sheet}: under the bar: {failed:?}; see {shot:?}");
}

fn run(sheet: &str) {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), format!("contrast::{sheet}")).unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.env.insert("MAKEPAD_TEST_DPI".into(), "2".into());
    if sheet != "host" {
        config.env.insert("MAKEPAD_WIDGET_STYLE".into(), sheet.into());
    }
    let label = sheet.to_string();
    run_with_config(config, move |app| measure(app, &label)).unwrap();
}

/// What a standalone start wears: the host's own sheet.
#[test]
fn host_sheet() { run("host"); }
#[test]
fn macos_light() { run("macos"); }
#[test]
fn macos_dark() { run("macos-dark"); }
#[test]
fn omarchy() { run("omarchy"); }

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

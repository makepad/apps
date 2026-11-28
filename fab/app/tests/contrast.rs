//! The words in Fab's chrome read on what they are drawn on: measured in the
//! grab, not in the token table.
//!
//! Why (user, on a Mac): "The UI colours on mac are all out of whack: it's
//! grey with no contrast." Standalone on a Mac no sheet was worn, so Fab ran
//! under the library's plain base theme: a mid-grey ground, and second and
//! disabled inks that are white at a few percent. The feature kinds, the
//! feature count, the status bar's "3 parts · 21 solids", the inactive "Parts"
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
#[path = "common/parts_tab.rs"]
mod parts_tab;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, NodeMatch, Role, Selector, TestApp, TestConfig};
use makepad_widgets::theme_tokens::contrast;
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::collections::BTreeMap;
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

fn rgb_to_rgba(c: [u8; 3]) -> u32 {
    ((c[0] as u32) << 24) | ((c[1] as u32) << 16) | ((c[2] as u32) << 8) | 0xff
}

fn rgb_dist_sq(a: [u8; 3], b: [u8; 3]) -> u32 {
    let d = |i: usize| a[i] as i32 - b[i] as i32;
    (d(0) * d(0) + d(1) * d(1) + d(2) * d(2)) as u32
}

fn luminance(c: [u8; 3]) -> f64 {
    let ch = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(c[0]) + 0.7152 * ch(c[1]) + 0.0722 * ch(c[2])
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

    /// The worst-case non-glyph ground (modal colour and the 5th/95th
    /// luminance percentiles outside a 2-device-pixel glyph halo) and the ink
    /// (the pixel furthest from modal ground) inside a box in device pixels,
    /// and their minimum WCAG contrast.
    fn reading(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> (u32, u32, f64) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        assert!(x0 < x1 && y0 < y1, "an empty box");
        let mut counts: BTreeMap<[u8; 3], usize> = BTreeMap::new();
        for y in y0..y1 {
            let row = y * self.width;
            for x in x0..x1 {
                *counts.entry(self.rgb[row + x]).or_default() += 1;
            }
        }
        // Deterministic ground mode via 4-bit (16-level) coarse RGB bins so a
        // shaded or noisy background cannot shatter across hundreds of exact
        // triplets and lose the mode to a flat solid-colour glyph. Within the
        // winning coarse bin, take the exact mode (ties toward lowest RGB),
        // which matches raw mode on flat grounds.
        let mut coarse: BTreeMap<[u8; 3], usize> = BTreeMap::new();
        for (&c, &n) in &counts {
            *coarse.entry([c[0] >> 4, c[1] >> 4, c[2] >> 4]).or_default() += n;
        }
        let best_bin = coarse
            .iter()
            .max_by(|(ba, na), (bb, nb)| na.cmp(nb).then_with(|| bb.cmp(ba)))
            .map(|(&b, _)| b)
            .expect("an empty box");
        let mut modal_rgb = counts
            .iter()
            .filter(|(&c, _)| [c[0] >> 4, c[1] >> 4, c[2] >> 4] == best_bin)
            .max_by(|(ca, na), (cb, nb)| na.cmp(nb).then_with(|| cb.cmp(ca)))
            .map(|(&c, _)| c)
            .expect("winning coarse bin is non-empty");
        let mut modal = rgb_to_rgba(modal_rgb);
        // Deterministic ink: furthest WCAG contrast from modal ground, breaking
        // ties toward lowest RGB (BTreeMap ascending key order).
        let furthest_from = |base: u32, base_rgb: [u8; 3]| {
            counts
                .keys()
                .map(|&c| {
                    let rgba = rgb_to_rgba(c);
                    (c, rgba, contrast(base, rgba))
                })
                .fold((base_rgb, base, 1.0_f64), |best, cand| {
                    if cand.2 > best.2 { cand } else { best }
                })
        };
        let (mut ink_rgb, mut ink, mut modal_ratio) = furthest_from(modal, modal_rgb);
        if modal_rgb == ink_rgb {
            return (modal, ink, modal_ratio);
        }

        // Mark core glyph pixels (at least as close to ink as to modal ground),
        // then exclude a 2-device-pixel halo so anti-aliased glyph fringes
        // cannot land in the 5th/95th-percentile background extremes.
        let bw = x1 - x0;
        let bh = y1 - y0;
        let mut core_glyph = vec![false; bw * bh];
        let mut core_count = 0usize;
        for dy in 0..bh {
            let row = (y0 + dy) * self.width;
            for dx in 0..bw {
                let c = self.rgb[row + x0 + dx];
                if rgb_dist_sq(c, ink_rgb) <= rgb_dist_sq(c, modal_rgb) {
                    core_glyph[dy * bw + dx] = true;
                    core_count += 1;
                }
            }
        }
        // Polarity guard: in every text/icon box the background is the majority
        // of pixels. If even the coarse-bin mode landed on a flat glyph over a
        // wide multi-bin gradient, invert back to the majority side.
        if core_count * 2 > bw * bh {
            modal_rgb = ink_rgb;
            modal = ink;
            (ink_rgb, ink, modal_ratio) = furthest_from(modal, modal_rgb);
            for dy in 0..bh {
                let row = (y0 + dy) * self.width;
                for dx in 0..bw {
                    let c = self.rgb[row + x0 + dx];
                    core_glyph[dy * bw + dx] = rgb_dist_sq(c, ink_rgb) <= rgb_dist_sq(c, modal_rgb);
                }
            }
        }
        let collect_bg = |pad: usize| -> Vec<[u8; 3]> {
            let mut out = Vec::with_capacity(bw * bh);
            for dy in 0..bh {
                let ny0 = dy.saturating_sub(pad);
                let ny1 = (dy + pad + 1).min(bh);
                let row = (y0 + dy) * self.width;
                for dx in 0..bw {
                    let nx0 = dx.saturating_sub(pad);
                    let nx1 = (dx + pad + 1).min(bw);
                    let mut near_glyph = false;
                    'halo: for ny in ny0..ny1 {
                        let base = ny * bw;
                        for nx in nx0..nx1 {
                            if core_glyph[base + nx] {
                                near_glyph = true;
                                break 'halo;
                            }
                        }
                    }
                    if !near_glyph {
                        out.push(self.rgb[row + x0 + dx]);
                    }
                }
            }
            out
        };
        let mut bg = collect_bg(2);
        if bg.is_empty() {
            bg = collect_bg(1);
        }
        if bg.is_empty() {
            bg = collect_bg(0);
        }
        if bg.is_empty() {
            return (modal, ink, modal_ratio);
        }

        bg.sort_unstable_by(|a, b| {
            luminance(*a)
                .total_cmp(&luminance(*b))
                .then_with(|| a.cmp(b))
        });
        let lo = rgb_to_rgba(bg[(bg.len() - 1) * 5 / 100]);
        let hi = rgb_to_rgba(bg[(bg.len() - 1) * 95 / 100]);

        let mut worst_ground = modal;
        let mut worst_ratio = modal_ratio;
        for cand in [lo, hi] {
            let r = contrast(cand, ink);
            if r < worst_ratio {
                worst_ground = cand;
                worst_ratio = r;
            }
        }
        (worst_ground, ink, worst_ratio)
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

fn check_inset(frame: &Frame, dpi: f64, what: &str, node: &NodeMatch, pad_l: f64, pad_r: f64, pad_v: f64, need: f64) -> Option<String> {
    let d = |v: f64| (v * dpi).round().max(0.0) as usize;
    let (x, y, w, h) = (node.x() as f64, node.y() as f64, node.width() as f64, node.height() as f64);
    let (x0, y0) = (d(x + pad_l), d(y + pad_v));
    let (x1, y1) = (d((x + w - pad_r).max(x + pad_l + 1.0)), d((y + h - pad_v).max(y + pad_v + 1.0)));
    let (ground, ink, ratio) = frame.reading(x0, y0, x1, y1);
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
    parts_tab::open(&app);
    app.locator(Selector::role(Role::Tab).name("plate")).wait_visible().click();
    // The feature rows, once two looks in a row agree on them (the layout has settled).
    let feature_children = |nodes: &[NodeMatch]| {
        nodes.iter().find(|n| n.visible() && n.node.role == Role::ListItem && n.node.name == "Features").map(|f| f.children.clone())
    };
    let visible_row = |n: &NodeMatch| n.visible() && n.node.role == Role::ListItem && n.width() > 0 && n.height() > 0;
    let rows_of = |app: &TestApp| -> Vec<NodeMatch> {
        let nodes = app.widget_snapshot().nodes();
        let Some(children) = feature_children(&nodes) else { return Vec::new(); };
        nodes.into_iter().filter(|n| visible_row(n) && children.contains(&n.id)).collect()
    };
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
    let count = app.locator(Selector::path("editor_bar.state").name("3 features")).wait_visible().snapshot();
    let caption = wait_poll(&app, || {
        let n = app.locator(Selector::path("status_bar.info")).wait_visible().snapshot();
        if !n.node.name.is_empty() && n.width() > 0 && n.height() > 0 { Ok(n) } else { Err("the status bar's counts are empty".to_string()) }
    });
    let chevron = app.locator(Selector::path("add.chevron_slot.chevron")).wait_visible().snapshot();
    assert!(chevron.width() > 0 && chevron.height() > 0, "the Add chevron has no box");
    let nodes = app.widget_snapshot().nodes();
    let children = feature_children(&nodes).expect("the visible Features tree");
    let rows: Vec<_> = nodes.iter().filter(|n| visible_row(n) && children.contains(&n.id)).collect();
    let window = app.locator(Selector::role(Role::Window)).wait_visible().snapshot();
    let rail_select = app.locator(Selector::role(Role::Radio).name("Select")).wait_visible().snapshot();
    let rail_move = app.locator(Selector::role(Role::Radio).name("Move")).wait_visible().snapshot();
    let settings_btn = app.locator(Selector::role(Role::Button).name("Settings")).wait_visible().snapshot();
    let print_btn = app.locator(Selector::role(Role::Button).name("Print")).wait_visible().snapshot();
    let shot = app.screenshot();
    let frame = Frame::decode(&shot);
    let dpi = (frame.width as f64 / window.width() as f64).round().max(1.0);

    let mut failed = Vec::new();
    let mut checked_kinds = 0usize;
    for row in rows.iter().filter(|r| !r.node.states.contains(makepad_test::States::SELECTED)) {
        let kind = nodes.iter()
            .find(|c| c.visible() && c.node.role == Role::StaticText && row.children.contains(&c.id) && c.width() > 0 && c.height() > 0)
            .unwrap_or_else(|| panic!("no visible kind child for feature row {:?}", row.node.name));
        failed.extend(check(&frame, dpi, &format!("kind {:?} of {:?}", kind.node.name, row.node.name), kind, None, WORDS));
        checked_kinds += 1;
    }
    assert!(checked_kinds >= 3, "expected at least 3 unselected feature kinds, checked {checked_kinds}");
    failed.extend(check(&frame, dpi, "feature count", &count, None, WORDS));
    failed.extend(check(&frame, dpi, "status bar counts", &caption, None, WORDS));
    failed.extend(check(&frame, dpi, "Add chevron", &chevron, None, GRAPHIC));
    failed.extend(check_inset(&frame, dpi, "3D rail Select (active)", &rail_select, 5.0, 5.0, 5.0, GRAPHIC));
    failed.extend(check_inset(&frame, dpi, "3D rail Move (inactive)", &rail_move, 5.0, 5.0, 5.0, GRAPHIC));
    failed.extend(check_inset(&frame, dpi, "Topbar Settings icon button", &settings_btn, 5.0, 5.0, 4.0, GRAPHIC));
    failed.extend(check_inset(&frame, dpi, "Topbar Print button", &print_btn, 5.0, 5.0, 4.0, WORDS));
    // Open Parts' FabSearch: measure focused placeholder, then unfocused typed text and magnifier.
    app.locator(Selector::role(Role::Button).name("Filter parts")).wait_visible().click();
    let search_sel = Selector::role(Role::TextField).name("Filter parts");
    let search_box = wait_poll(&app, || {
        std::thread::sleep(Duration::from_millis(60));
        let a = app.locator(search_sel.clone()).wait_visible().snapshot();
        std::thread::sleep(Duration::from_millis(60));
        let b = app.locator(search_sel.clone()).wait_visible().snapshot();
        if a.bounds == b.bounds && b.width() >= 60 && b.height() > 0 { Ok(b) }
        else { Err(format!("open search still animating: {:?}", b.bounds)) }
    });
    std::thread::sleep(Duration::from_millis(250)); // TextInputFlat empty:on is 0.2s
    let shot_search_empty = app.screenshot();
    let frame_search_empty = Frame::decode(&shot_search_empty);
    failed.extend(check_inset(&frame_search_empty, dpi, "open search placeholder", &search_box, 30.0, 4.0, 4.0, WORDS));
    app.locator(search_sel.clone()).fill("plate");
    app.locator(Selector::role(Role::Tab).name("Parts")).click_at_center();
    std::thread::sleep(Duration::from_millis(300)); // TextInputFlat focus:off is 0.25s
    let search_typed = app.locator(search_sel).wait_visible().snapshot();
    let search_glyph = app.locator(Selector::path("filter.row.glyph_slot.glyph")).wait_visible().snapshot();
    let shot_search_typed = app.screenshot();
    let frame_search_typed = Frame::decode(&shot_search_typed);
    failed.extend(check_inset(&frame_search_typed, dpi, "open search text", &search_typed, 26.0, 4.0, 4.0, WORDS));
    failed.extend(check(&frame_search_typed, dpi, "open search magnifier", &search_glyph, None, GRAPHIC));
    app.locator(Selector::role(Role::Tab).name("Plates")).wait_visible().click();
    wait_poll(&app, || {
        let t = app.locator(Selector::role(Role::Tab).name("Parts")).wait_visible().snapshot();
        if !t.node.states.contains(makepad_test::States::SELECTED) { Ok(()) } else { Err("Parts tab is still selected".to_string()) }
    });
    std::thread::sleep(Duration::from_millis(400)); // TabFlat active:off is 0.3s, hover:off is 0.2s
    let parts_tab = app.locator(Selector::role(Role::Tab).name("Parts")).wait_visible().snapshot();
    assert!(!parts_tab.node.states.contains(makepad_test::States::SELECTED), "Parts tab must be inactive");
    assert!(parts_tab.width() > 0 && parts_tab.height() > 0, "the Parts tab has no box");
    let shot_inactive = app.screenshot();
    let frame_inactive = Frame::decode(&shot_inactive);
    failed.extend(check(&frame_inactive, dpi, "Parts tab (inactive)", &parts_tab, None, WORDS));
    assert!(failed.is_empty(), "contrast under {sheet}: under the bar: {failed:?}; see {shot:?} and {shot_inactive:?}");
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
#[test]
fn safety_yellow() { run("safety-yellow"); }
#[test]
fn futuristic_plastic() { run("futuristic-plastic"); }
#[test]
fn kids_toy() { run("kids-toy"); }

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

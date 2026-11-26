//! The Sliced view draws even layers even, and an uneven one uneven: what a wall shows is what
//! the printer makes, so a print is judged by eye on its real layers, not on the view's shading.
//!
//! Why (user): "the layers seem uneven there because of the shading but on close inspection that
//! doesn't appear to be the problem ... if it's rendering bad it makes the print look worse than
//! it really is." The beads were flat; the view swung each bead's normal toward the corner at
//! either end over the bead's whole length, so a short bead between sharp turns shaded like a
//! cylinder, and layers whose moves broke at other places shaded differently.
//!
//! One plate in Printed surface: a square post (its walls short beads between right-angle
//! corners, like a lattice's corner pole) and a block with one layer standing out 0.3 mm all
//! round. Sliced, the Front view made orthographic (each face square on), then zoomed by the
//! wheel off any pixel (each step another scale, a fractional camera) at device scale 2: read
//! once the post's layers are 4 px apart, and two and four steps further in (about 4, 7 and
//! 11 px). Each finished (anti-aliased) picture is read down columns of each part's face: the
//! layer lines are the creases (local minima, each placed at the middle of its dip), a layer
//! row is the stretch between two, its brightness the profile's mean over exactly that stretch.
//!
//! - the post: every row's pitch within `PITCH_SPREAD` of the rows' median, every row's
//!   brightness within `ROW_SPREAD` of theirs (luma, 0..255), and along each row the face reads
//!   flat: across its middle 80 % a row's brightness varies by under `ALONG_SPREAD`;
//! - the block: the proud layer reads as uneven, a row off the median by over `UNEVEN_PITCH`
//!   in pitch or `UNEVEN_ROW` in brightness, and nothing else does (the proud layer's row and
//!   the two either side, whose creases it moves).
//!
//! Measured (three GPUs alike): the rows' pitch within 0.2 px and brightness within 2 of
//! theirs; along a row 0.2 to 0.4, against 42 to 47 with the normal swung over whole beads
//! (each row of the post shaded as a cylinder).
//!
//! Corners (user, on a lattice cube with that fixed: "as you can see still some corners mess
//! the shading"): one right angle in six was drawn as a sharp corner's tip (the moves' rounding
//! left it a hair past square), a dark tooth on that layer; and an inside corner swung its
//! faces' normals as an outside one does, so the shading of a corner that is sharp ran onto the
//! flat faces beside it. The first is the preview's joints' to test (its unit test,
//! `preview_beads_meet_in_one_tube_at_corners`). For the second a block with a slot down its
//! front stands left of the post: the slot's back face, square on to the view, runs between two
//! inside corners and reads flat right into them: from 2 px off either, a row's brightness
//! varies by under `ALONG_SPREAD` (measured 0.3 to 0.4, against 8 to 32 with the inside swung).
//!
//! These readings are the shading's, so they are taken with the screen AO off (the viewport's
//! `debug_off` bit 32). The AO darkens an inside corner by design, the screen_ao test's to check:
//! with it the slot's back face reads 2.38 to 2.81 along a row, as a correct AO does there (the
//! sky the slot leaves open is 41 % of it 2 px off a corner, 55 % in its middle). The AO's own
//! risk here is banding the even layers: in the last picture, with the AO on, the post's face
//! (no inside corner in its rows) reads as even and as flat along a row as without it, within
//! the same spreads.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

use makepad_strict_json::Value;
use makepad_test::{run_with_config, KeyCode, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::time::Duration;

/// A row's pitch off the median that still reads even (device pixels).
const PITCH_SPREAD: f64 = 0.3;
/// A row's brightness off the median that still reads even (luma).
const ROW_SPREAD: f64 = 3.0;
/// The brightness a flat row may vary by along its middle 80 % (luma).
const ALONG_SPREAD: f64 = 2.0;
/// The proud layer's row: off the median by this fraction in pitch, or this much in brightness.
const UNEVEN_PITCH: f64 = 0.25;
const UNEVEN_ROW: f64 = 8.0;

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

const POST: &str = "// A square post: its walls are short beads between right-angle corners.
use mod.model.*
use mod.fab.*
let m = mod.model
let post = m.box({size: [3mm 8mm 3mm] at: [0, 4mm, 0]})
object(post)
";

const PROUD: &str = "// A block with one layer standing out by 0.3 mm all round.
use mod.model.*
use mod.fab.*
let m = mod.model
let body = m.box({size: [12mm 8mm 12mm] at: [0, 4mm, 0]})
let ring = m.box({size: [12.6mm 0.2mm 12.6mm] at: [0, 4.1mm, 0]})
let block = m.union(body, ring)
object(block)
";

const NOTCH: &str = "// A block with a slot 2 mm wide and 1.5 mm deep down its front and its back.
use mod.model.*
use mod.fab.*
let m = mod.model
let body = m.box({size: [6mm 8mm 4mm] at: [0, 4mm, 0]})
let front = m.box({size: [2mm 10mm 3mm] at: [0, 4mm, 2mm]})
let back = m.box({size: [2mm 10mm 3mm] at: [0, 4mm, -2mm]})
let notch = m.subtract(m.subtract(body, front), back)
object(notch)
";

/// The notched block's width and its slot's (mm).
const NOTCH_WIDTH: f64 = 6.0;
const SLOT_WIDTH: f64 = 2.0;

/// The three parts: fab.script put each script's instance on the plate, and these place them. (A
/// `script(..)` line here would add a second instance of each, a copy elsewhere on the plate.)
const PLATE: &str = r#"color("Colour 1", rgb: #xa03cf7)
place("notch", object: "notch", at: [112mm 100mm], name: "Notch", color: "Colour 1")
place("post", object: "post", at: [120mm 100mm], name: "Post", color: "Colour 1")
place("proud", object: "block", at: [131mm 100mm], name: "Proud", color: "Colour 1")
"#;

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

fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let lines = remote(app, &["call".into(), format!("fab.{tool}"), args.into()])?;
    let line = lines.iter().find(|l| l.contains("\"Result\"")).ok_or_else(|| format!("fab.{tool}: no result: {lines:?}"))?;
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).map_err(|e| format!("{e:?}"))?;
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    Ok(r.get("text").and_then(Value::as_str).unwrap_or_default().to_string())
}

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    app.try_wait_until_within(HANG, || (!ok()).then(|| format!("waiting for {what}"))).unwrap_or_else(|why| {
        panic!("{why}; timed out waiting for {what}; the viewport reports {}", app.locator(Selector::id("viewport")).snapshot().description);
    });
}

/// The viewport's widget path, for its live diagnostics.
const VIEWPORT: &str = "body.chat_split.shell.main.dock.tab_viewport.viewport";

/// The screen AO on, or off by the viewport's `debug_off` bit 32.
fn screen_ao(app: &TestApp, on: bool) {
    let value = if on { "0" } else { "32" };
    remote(app, &["tweak".into(), "apply".into(), format!("path={VIEWPORT}"), "prop=debug_off".into(), format!("value={value}")]).expect("the viewport's debug_off");
}

/// What the viewport reports of the recording on display.
fn recording(app: &TestApp) -> Value {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).unwrap_or(Value::Null)
}

/// The view's finished picture with the screen AO `ao`: settled, its still average complete, two
/// grabs alike.
fn finished_grab(app: &TestApp, ao: bool) -> (std::path::PathBuf, Grab) {
    wait_poll(app, || {
        let r = recording(app);
        if !(r.get("settled").and_then(Value::as_bool) == Some(true)
            && r.get("still_samples").and_then(Value::as_u64) == Some(makepad_render::STILL_AA_SAMPLES as u64)
            && r.get("screen_ao").and_then(Value::as_bool) == Some(ao))
        {
            return Err("waiting for the finished picture".to_string());
        }
        let first = std::fs::read(app.screenshot()).expect("the grab");
        let path = app.screenshot();
        if std::fs::read(&path).expect("the grab") == first {
            let grab = Grab::decode(&path);
            return Ok((path, grab));
        }
        Err("waiting for two matching grabs".to_string())
    })
}

/// A grab's pixels.
struct Grab {
    width: usize,
    height: usize,
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
        Grab { width, height, rgb }
    }

    fn luma(&self, x: usize, y: usize) -> f64 {
        let [r, g, b] = self.rgb[y * self.width + x];
        0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64
    }

    /// The print's purple (lit or in a crease), not the plate's grey.
    fn print(&self, x: usize, y: usize) -> bool {
        let [r, g, b] = self.rgb[y * self.width + x];
        r as i32 > g as i32 + 15 && b as i32 > g as i32 + 40
    }

    /// The stretches of columns from `x0` to `x1` with at least half as many rows of print in
    /// `y0..y1` as the tallest column (the parts, not the skirt's or the brim's lines): (first,
    /// last) column of each.
    fn part_columns(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> Vec<(usize, usize)> {
        let counts: Vec<usize> = (x0..x1).map(|x| (y0..y1).filter(|&y| self.print(x, y)).count()).collect();
        let tall = counts.iter().copied().max().unwrap_or(0) / 2;
        let mut runs = Vec::new();
        let mut start = None;
        for (i, c) in counts.iter().copied().chain([0]).enumerate() {
            match (tall > 0 && c > tall, start) {
                (true, None) => start = Some(i),
                (false, Some(s)) => {
                    runs.push((x0 + s, x0 + i - 1));
                    start = None;
                }
                _ => {}
            }
        }
        runs.retain(|r: &(usize, usize)| r.1 - r.0 > 4);
        runs
    }

    /// The longest stretch of print down column `x` in `y0..y1` (gaps of a few rows bridged).
    fn tallest(&self, x: usize, y0: usize, y1: usize) -> (usize, usize) {
        let (mut best, mut run) = ((y0, y0), None::<(usize, usize)>);
        for y in y0..y1 {
            if self.print(x, y) {
                run = Some(match run {
                    Some((a, b)) if y <= b + 6 => (a, y),
                    _ => (y, y),
                });
                if let Some(r) = run.filter(|r| r.1 - r.0 > best.1 - best.0) {
                    best = r;
                }
            }
        }
        best
    }

    /// Down column `x` from `y`: the last row of print before the plate shows (or the grab ends).
    fn run_down(&self, x: usize, y: usize, step: isize) -> usize {
        let mut at = y as isize;
        while at + step >= 0 && ((at + step) as usize) < self.height && (0..6).any(|k| self.print(x, (at + step * (k + 1)).clamp(0, self.height as isize - 1) as usize)) {
            at += step;
        }
        at as usize
    }

    /// The mean luma of columns `x..x + 3` from `y0` to `y1`.
    fn column(&self, x: usize, y0: usize, y1: usize) -> Vec<f64> {
        (y0..y1).map(|y| (x..x + 3).map(|x| self.luma(x, y)).sum::<f64>() / 3.0).collect()
    }
}

/// The creases down a profile: its local minima at least 12 % of its range below the highs on
/// either side, each at the middle of its dip at half depth (sub-pixel, between the samples
/// either side of that level: a zoomed crease is a few pixels of dark, its lowest pixel no
/// better than whole).
fn creases(col: &[f64]) -> Vec<f64> {
    let (lo, hi) = col.iter().fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
    let prominence = (hi - lo) * 0.12;
    let n = col.len();
    let mut out = Vec::new();
    let mut i = 1;
    while i + 1 < n {
        if col[i] < col[i - 1] && col[i] <= col[i + 1] {
            let mut j = i;
            while j + 2 < n && col[j + 1] == col[i] {
                j += 1;
            }
            let (mut l, mut r) = (i, j);
            while l > 0 && col[l - 1] >= col[l] {
                l -= 1;
            }
            while r + 1 < n && col[r + 1] >= col[r] {
                r += 1;
            }
            let left = col[l..=i].iter().cloned().fold(f64::MIN, f64::max);
            let right = col[j..=r].iter().cloned().fold(f64::MIN, f64::max);
            let top = left.min(right);
            if top - col[i] >= prominence {
                let level = col[i] + 0.5 * (top - col[i]);
                let mut a = i;
                while a > l && col[a - 1] < level {
                    a -= 1;
                }
                let mut b = j;
                while b < r && col[b + 1] < level {
                    b += 1;
                }
                // a - 1 and b + 1 are at or above the level (l and r, the highs either side,
                // are above it)
                let ya = (a - 1) as f64 + (col[a - 1] - level) / (col[a - 1] - col[a]);
                let yb = b as f64 + (level - col[b]) / (col[b + 1] - col[b]);
                out.push((ya + yb) / 2.0);
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// The mean of the profile, linear between its samples, over `a..b` (sample units).
fn mean_over(col: &[f64], a: f64, b: f64) -> f64 {
    let at = |y: f64| {
        let i = (y.floor() as usize).min(col.len() - 2);
        let f = y - i as f64;
        col[i] * (1.0 - f) + col[i + 1] * f
    };
    // the integral by the trapezoid rule over a fine grid: exact for a piecewise-linear profile
    // up to the grid, which is 1/16 px
    let steps = (((b - a) * 16.0).ceil() as usize).max(1);
    let h = (b - a) / steps as f64;
    (0..steps).map(|k| (at(a + h * k as f64) + at(a + h * (k + 1) as f64)) * 0.5).sum::<f64>() / steps as f64
}

/// Each crease's darkest sample down a profile: how dark its line reads. A hard crease's darkest
/// pixel goes with where the line falls in it, so from layer to layer (the place walking down the
/// pixels) the lines' darkness beats; a crease read over two pixels reads alike at any place.
fn crease_darkness(col: &[f64], creases: &[f64]) -> Vec<f64> {
    creases.iter().map(|&y| {
        let i = y.round() as usize;
        col[i.saturating_sub(2)..(i + 3).min(col.len())].iter().cloned().fold(f64::MAX, f64::min)
    }).collect()
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[s.len() / 2]
}

/// One face's rows: (pitch, brightness) of each, down its middle column.
fn face_rows(grab: &Grab, x: usize, y0: usize, y1: usize) -> Vec<(f64, f64)> {
    let col = grab.column(x, y0, y1);
    let m = creases(&col);
    m.windows(2).map(|w| (w[1] - w[0], mean_over(&col, w[0], w[1]))).collect()
}

/// The layers' pitch (px) the readings start at: between two wheel steps' (about 3.1 and 4.0 px),
/// so a pixel's rounding of the post's width never moves them a step, where the last would zoom
/// the proud layer out of the picture.
const FIRST_PITCH: f64 = 3.5;

/// The window, in logical pixels, at device scale 2.
const WINDOW: (f64, f64, f64) = (1600.0, 1000.0, 2.0);

/// What one picture shows of the three parts.
struct Reading {
    /// the post's columns (device pixels)
    post: (usize, usize),
    /// the post's rows: their count, median pitch (px) and brightness, the worst row's pitch
    /// (px) and brightness off them, and the median of its rows' brightness spread along the
    /// face
    rows: usize,
    pitch: f64,
    bright: f64,
    worst_pitch: f64,
    worst_row: f64,
    along: f64,
    /// the post's lines: their darkness's median and the worst line's off it (luma)
    dark: f64,
    worst_dark: f64,
    /// the block's rows (pitch, brightness) and those off its median as uneven
    proud: Vec<(f64, f64)>,
    odd: Vec<usize>,
    /// the notched block's slot: its back face's brightness spread along a row between the
    /// inside corners (median over its rows)
    inside: f64,
}

/// The notched block, the post and the block in a picture (device pixels): their columns, and
/// the row midway down the post.
fn locate(grab: &Grab, x0: usize, y0: usize, x1: usize, y1: usize) -> Option<((usize, usize), (usize, usize), (usize, usize), usize)> {
    let runs = grab.part_columns(x0, y0, x1, y1);
    let [notch, post, block] = runs[..] else { return None };
    if post.1 - post.0 >= block.1 - block.0 || post.1 - post.0 >= notch.1 - notch.0 {
        return None;
    }
    let (top, bottom) = grab.tallest((post.0 + post.1) / 2, y0, y1);
    Some((notch, post, block, (top + bottom) / 2))
}

/// Reads one face of a part at column run `run`: from `row` up and down to its ends, `top` and
/// `bottom` of that height left out.
fn face(grab: &Grab, run: (usize, usize), row: usize, top: usize, bottom: usize) -> (usize, usize, usize) {
    let mid = (run.0 + run.1) / 2;
    let (t, b) = (grab.run_down(mid, row, -1), grab.run_down(mid, row, 1));
    (mid, t + (b - t) / top, b - (b - t) / bottom)
}

/// The slot's back face of the notched block at column run `run`, read in rows by the creases
/// down its middle: along each row from 2 px off one inside corner to 2 px off the other, the
/// brightness spread; the median over its rows.
fn slot_face(grab: &Grab, run: (usize, usize), row: usize) -> Option<f64> {
    let ppm = (run.1 - run.0) as f64 / NOTCH_WIDTH;
    let c = (run.0 + run.1) as f64 * 0.5;
    let (x0, x1) = ((c - SLOT_WIDTH * 0.5 * ppm) as usize + 2, (c + SLOT_WIDTH * 0.5 * ppm) as usize - 4);
    let (mid, y0, y1) = face(grab, (x0, x1), row, 10, 6);
    let m = creases(&grab.column(mid, y0, y1));
    if m.len() < 9 || x1 <= x0 + 4 {
        return None;
    }
    let cols: Vec<Vec<f64>> = (x0..=x1).map(|x| grab.column(x, y0, y1)).collect();
    let spread: Vec<f64> = m
        .windows(2)
        .map(|pair| {
            let (lo, hi) = cols.iter().map(|col| mean_over(col, pair[0] + 1.0, pair[1] - 1.0)).fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(v), h.max(v)));
            hi - lo
        })
        .collect();
    Some(median(&spread))
}

fn read(grab: &Grab, x0: usize, y0: usize, x1: usize, y1: usize) -> Option<Reading> {
    let (notch, post, block, row) = locate(grab, x0, y0, x1, y1)?;
    // The post's front face, the top layer and the brim's rounds left out.
    let (mid, fy0, fy1) = face(grab, post, row, 10, 6);
    let rows = face_rows(grab, mid, fy0, fy1);
    if rows.len() < 8 {
        return None;
    }
    let pitch = median(&rows.iter().map(|r| r.0).collect::<Vec<_>>());
    let bright = median(&rows.iter().map(|r| r.1).collect::<Vec<_>>());
    let col = grab.column(mid, fy0, fy1);
    let m = creases(&col);
    let dark = crease_darkness(&col, &m[1..m.len() - 1]);
    let dark_median = median(&dark);
    let w = post.1 - post.0;
    let (fx0, fx1) = (post.0 + w / 10, post.1 - w / 10 - 3);
    let along: Vec<f64> = m
        .windows(2)
        .map(|pair| {
            let row: Vec<f64> = (fx0..fx1).step_by(2).map(|x| mean_over(&grab.column(x, fy0, fy1), pair[0] + 1.0, pair[1] - 1.0)).collect();
            let (lo, hi) = row.iter().fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
            hi - lo
        })
        .collect();
    // The block's front face.
    let (mid, by0, by1) = face(grab, block, row, 10, 10);
    let proud = face_rows(grab, mid, by0, by1);
    if proud.len() < 8 {
        return None;
    }
    let p_pitch = median(&proud.iter().map(|r| r.0).collect::<Vec<_>>());
    let p_bright = median(&proud.iter().map(|r| r.1).collect::<Vec<_>>());
    let odd = (0..proud.len()).filter(|&k| (proud[k].0 - p_pitch).abs() > UNEVEN_PITCH * p_pitch || (proud[k].1 - p_bright).abs() > UNEVEN_ROW).collect();
    let inside = slot_face(grab, notch, row)?;
    Some(Reading {
        post,
        rows: rows.len(),
        pitch,
        bright,
        worst_pitch: rows.iter().map(|r| (r.0 - pitch).abs()).fold(0.0, f64::max),
        worst_row: rows.iter().map(|r| (r.1 - bright).abs()).fold(0.0, f64::max),
        along: median(&along),
        dark: dark_median,
        worst_dark: dark.iter().map(|d| (d - dark_median).abs()).fold(0.0, f64::max),
        proud,
        odd,
        inside,
    })
}

/// The finished picture with the screen AO `ao` read, with the viewport's place in it.
fn look(app: &TestApp, name: &str, ao: bool) -> (std::path::PathBuf, f64, Option<((usize, usize), (usize, usize), (usize, usize), usize)>, Option<Reading>) {
    let (path, grab) = finished_grab(app, ao);
    let b = app.locator(Selector::id("viewport")).snapshot().bounds;
    let scale = grab.width as f64 / WINDOW.0;
    let (x0, y0) = ((b.x as f64 * scale) as usize, (b.y as f64 * scale) as usize);
    let (x1, y1) = (((b.x + b.w) as f64 * scale) as usize, ((b.y + b.h) as f64 * scale) as usize);
    let reading = read(&grab, x0, y0, x1, y1);
    let found = locate(&grab, x0, y0, x1, y1);
    if let Some(r) = &reading {
        println!(
            "[fab-even-layers] {name}: post {} px wide, {} rows, pitch {:.2} px (worst {:.2} off), brightness {:.1} (worst {:.2}), along a row {:.2}, lines {:.1} dark (worst {:.2} off); the slot's face between its inside corners {:.2}; block uneven rows {:?} of {:?}",
            r.post.1 - r.post.0,
            r.rows,
            r.pitch,
            r.worst_pitch,
            r.bright,
            r.worst_row,
            r.along,
            r.dark,
            r.worst_dark,
            r.inside,
            r.odd,
            r.proud.iter().map(|p| format!("{:.1}/{:.0}", p.0, p.1)).collect::<Vec<_>>()
        );
    } else {
        println!("[fab-even-layers] {name}: the parts' layer lines not read (too small yet)");
    }
    (path, scale, found, reading)
}

#[test]
fn even_layers_read_even_and_a_proud_layer_reads_uneven() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "even_layers").unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), format!("{}x{}@{}", WINDOW.0, WINDOW.1, WINDOW.2)];
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let prefs = std::env::temp_dir().join(format!("fab-even-layers-{}-{nanos}.prefs", std::process::id()));
    // The code split off (the view's full width), the printed surface (each bead in its filament).
    std::fs::write(&prefs, "code=0\nsurface=1\n").unwrap();
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let result = run_with_config(config, |app: TestApp| {
        let mut head = String::new();
        wait_until(&app, "the fab tools", || call(&app, "inspect", "{}").map(|v| head = head_of(&v)).is_ok());
        for (i, (file, source)) in [("post.cad.splash", POST), ("proud.cad.splash", PROUD), ("notch.cad.splash", NOTCH)].iter().enumerate() {
            let batch = format!(r#"{{"request_id":"s{i}","expected":{head:?},"script":"{file}","source":{}}}"#, Value::Str(source.to_string()).to_json());
            head = head_of(&call(&app, "script", &batch).expect("script"));
        }
        let batch = format!(r#"{{"request_id":"p","expected":{head:?},"edits":{}}}"#, Value::Str(PLATE.to_string()).to_json());
        call(&app, "place", &batch).expect("place");
        app.locator(Selector::id("rm_slice")).wait_visible().click();
        app.locator(Selector::id("rm_slice")).wait_checked(true);
        wait_until(&app, "the slice", || call(&app, "slice", "{}").is_ok_and(|t| t.contains("Sliced")));
        // The shading read alone: the screen AO off till the last picture.
        screen_ao(&app, false);
        // From the command palette: the Front view, orthographic (a face square on, no side of
        // it in view).
        for command in ["View: Front", "Toggle Orthographic"] {
            let camera = recording(&app).get("camera").map(Value::to_json);
            app.press_key(KeyCode::Escape);
            app.press_key(KeyCode::F3);
            app.type_text(command);
            app.press_key(KeyCode::ReturnKey);
            wait_until(&app, &format!("{command}: the camera"), || recording(&app).get("camera").map(Value::to_json) != camera);
            // the glide there over
            wait_until(&app, &format!("{command}: the camera at rest"), || recording(&app).get("settled").and_then(Value::as_bool) == Some(true));
        }
        let ortho = recording(&app).get("camera").and_then(|c| c.get("ortho")).and_then(Value::as_bool);
        assert_eq!(ortho, Some(true), "the Front view, orthographic");
        // Zoomed in by the wheel at the gap between the two, off any pixel (each step another
        // scale, a fractional camera) until the post's layers are `FIRST_PITCH` apart; then read
        // there and two and four steps further in.
        let mut readings = Vec::new();
        let mut step = 0;
        while readings.len() < 3 {
            let (path, scale, found, reading) = look(&app, &format!("step{step}"), false);
            let Some((_, post, block, row)) = found else { panic!("step {step}: the notched block, the post and the block not found; see {path:?}") };
            let mut steps = 1;
            // the post is 3 mm wide, its layers 0.2 mm: 15 layers to its width
            if (post.1 - post.0) as f64 / 15.0 >= FIRST_PITCH {
                readings.push((step, path, reading));
                if readings.len() == 3 {
                    break;
                }
                steps = 2;
            }
            assert!(step < 24, "the wheel did not zoom in to the post's layers");
            for _ in 0..steps {
                let at = ((post.1 + block.0) as f64 * 0.5 / scale + 0.37, row as f64 / scale + 0.61);
                let camera = recording(&app).get("camera").map(Value::to_json);
                app.scroll_at(at.0, at.1, 0.0, -80.0, None);
                wait_until(&app, "the wheel's zoom", || recording(&app).get("camera").map(Value::to_json) != camera);
                step += 1;
            }
        }
        // The last picture again with the AO on: the post's face, away from any inside corner.
        screen_ao(&app, true);
        let (ao_path, _, _, ao_reading) = look(&app, &format!("step{step}, the AO on"), true);
        let Some(on) = ao_reading else { panic!("step {step}, the AO on: the post's layer lines not found; see {ao_path:?}") };
        assert!(on.worst_row <= ROW_SPREAD, "step {step}: with the AO on the post's layers band: a row's brightness is {:.2} off the median {:.1}; see {ao_path:?}", on.worst_row, on.bright);
        assert!(on.along <= ALONG_SPREAD, "step {step}: with the AO on the post's face is not flat along a row: it varies by {:.2} (median over its rows); see {ao_path:?}", on.along);
        for (step, path, reading) in readings {
            let name = format!("step {step}");
            let Some(r) = reading else { panic!("{name}: the notched block's, the post's or the block's layer lines not found; see {path:?}") };
            assert!(r.worst_pitch <= PITCH_SPREAD, "{name}: the post's layers are 0.2 mm each, but a row's pitch is {:.2} px off the median {:.2} px; see {path:?}", r.worst_pitch, r.pitch);
            assert!(r.worst_row <= ROW_SPREAD, "{name}: the post's layers are alike, but a row's brightness is {:.2} off the median {:.1}; see {path:?}", r.worst_row, r.bright);
            assert!(r.along <= ALONG_SPREAD, "{name}: the post's faces are flat, but along a row the brightness varies by {:.2} (median over its rows); see {path:?}", r.along);
            assert!(r.inside <= ALONG_SPREAD, "{name}: the slot's back face is flat into its inside corners, but along a row its brightness varies by {:.2} (median over its rows); see {path:?}", r.inside);
            assert!(!r.odd.is_empty(), "{name}: the block's proud layer does not read as uneven: rows {:?}; see {path:?}", r.proud);
            // the proud layer's own row, and the rows its creases share
            assert!(r.odd.len() <= 3 && r.odd.windows(2).all(|w| w[1] - w[0] == 1), "{name}: only the proud layer reads as uneven, but rows {:?} do of {:?}; see {path:?}", r.odd, r.proud);
        }
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

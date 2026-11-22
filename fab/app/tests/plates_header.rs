//! The Plates panel's header row: New plate, then the search field. Measured in the pixels of
//! one grab at dpi 1 and one at dpi 2 (the window laid out at 2 device pixels a point):
//!
//! 1. The placeholder sits on the field's centre: the ink of its first capital (the `S` of
//!    `Search plates`, whose curves overshoot the cap line and the baseline alike, so its ink
//!    band is centred where the cap band is) is centred within 0.5 logical px of the field's box,
//!    and the magnifier's box is centred on the field too. The bug this pins: the input in the
//!    search well was a TextInput, which a desktop sheet gives a field's height of its own, so it
//!    stood taller than the well and its line sat about 3 px low.
//!    The field's semantics give its line box there as well (`text.lines`, which a widget
//!    measures as it draws and its parents' alignment moves afterwards).
//! 2. The two controls share a top, a bottom and a baseline: the button's face and the well's
//!    outline start and end within 0.5 logical px of each other, and so do the cap bands of the
//!    two labels (`N` of New plate, `S` of the placeholder). Before, the well filled its whole
//!    rect where a button keeps a ring's width clear for its focus ring, so the field read taller.

use makepad_test::{run_with_config, NodeMatch, Role, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const TOLERANCE_LPX: f64 = 0.5;

struct Frame {
    width: usize,
    height: usize,
    luma: Vec<f64>,
}

impl Frame {
    fn decode(path: &Path) -> Frame {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let mut dec = PngDecoder::new(ZCursor::new(&bytes));
        let px = dec.decode_raw().unwrap_or_else(|e| panic!("decode {path:?}: {e:?}"));
        let (width, height) = dec.dimensions().expect("png dimensions");
        let n = dec.colorspace().expect("png colorspace").num_components();
        assert!(n >= 3, "grab is not a colour image ({n} components)");
        let luma = (0..width * height).map(|i| (px[i * n] as f64 * 54.0 + px[i * n + 1] as f64 * 183.0 + px[i * n + 2] as f64 * 19.0) / 256.0).collect();
        Frame { width, height, luma }
    }

    fn at(&self, x: usize, y: usize) -> f64 {
        self.luma[y.min(self.height - 1) * self.width + x.min(self.width - 1)]
    }
}

/// A widget's rect in device pixels.
#[derive(Clone, Copy, Debug)]
struct Dev {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Dev {
    fn of(node: &NodeMatch, dpi: f64) -> Dev {
        let b = node.node.bounds;
        Dev { x0: b.x as f64 * dpi, y0: b.y as f64 * dpi, x1: (b.x + b.w) as f64 * dpi, y1: (b.y + b.h) as f64 * dpi }
    }

    fn center_y(&self) -> f64 {
        (self.y0 + self.y1) / 2.0
    }
}

/// Where a control's outermost paint starts, read down (`step` 1) or up (`step` -1) one column
/// from `outside`, a background row: each row of the first ramp away from the background counts
/// as its share of the ramp's peak, and the edge sits that many rows out from the peak row's far
/// side. A face (the button's) and an outline (the well's) read alike.
fn rim(frame: &Frame, x: usize, outside: usize, step: isize) -> f64 {
    let bg = frame.at(x, outside);
    let (mut y, mut ramp) = (outside, Vec::<f64>::new());
    loop {
        let next = (y as isize + step) as usize;
        let d = (frame.at(x, next) - bg).abs();
        if ramp.last().is_some_and(|last| d <= *last && *last > 2.0) {
            break;
        }
        assert!(ramp.len() < 16, "no edge in column {x} from row {outside}");
        ramp.push(d);
        y = next;
    }
    let peak = ramp.last().copied().unwrap_or(0.0);
    assert!(peak > 8.0, "no contrast between a control and the panel in column {x} from row {outside}");
    let covered: f64 = ramp.iter().map(|d| d / peak).sum();
    if step > 0 { (y + 1) as f64 - covered } else { y as f64 + covered }
}

/// The ink band (top, bottom, in device px) of the first glyph in a control's text area: per row,
/// the most ink of the glyph's first four points, as a share of the label's strongest ink; the
/// edges are the first and last rows past half of it, each moved out by the rest of the ramp.
fn first_glyph_band(frame: &Frame, area: Dev, dpi: f64, what: &str) -> (f64, f64) {
    let (x0, x1) = (area.x0.ceil() as usize, area.x1.floor() as usize);
    let (y0, y1) = ((area.y0 + 2.5 * dpi).ceil() as usize, (area.y1 - 2.5 * dpi).floor() as usize);
    assert!(x1 > x0 + 8 && y1 > y0 + 4 && x1 <= frame.width && y1 <= frame.height, "{what}: rect {area:?} is not on the frame");
    let mut tally = std::collections::HashMap::<i64, usize>::new();
    for y in y0..y1 {
        for x in x0..x1 {
            *tally.entry(frame.at(x, y).round() as i64).or_default() += 1;
        }
    }
    let fill = tally.into_iter().max_by_key(|(_, n)| *n).map(|(v, _)| v as f64).expect("non-empty area");
    let ink = |x: usize, y: usize| (frame.at(x, y) - fill).abs();
    let peak = (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| ink(x, y)).fold(0.0, f64::max);
    assert!(peak > 40.0, "{what}: no legible ink over fill {fill} (peak {peak})");
    let first = (x0..x1).find(|x| (y0..y1).any(|y| ink(*x, y) > peak * 0.25)).unwrap_or_else(|| panic!("{what}: no ink column"));
    let columns = first..(first + (4.0 * dpi).round() as usize).min(x1);
    let cov = |y: usize| columns.clone().map(|x| ink(x, y)).fold(0.0, f64::max) / peak;
    let solid: Vec<usize> = (y0..y1).filter(|y| cov(*y) >= 0.5).collect();
    let (a, b) = (*solid.first().unwrap_or_else(|| panic!("{what}: the first glyph has no solid row")), *solid.last().unwrap());
    let top = (a + 1) as f64 - cov(a) - (y0..a).map(cov).sum::<f64>();
    let bottom = b as f64 + cov(b) + (b + 1..y1).map(cov).sum::<f64>();
    (top, bottom)
}

/// Every measurement of the header row, in logical px.
#[derive(Debug)]
pub struct Header {
    pub dpi: f64,
    pub grab: PathBuf,
    /// The placeholder's ink centre less the field's box centre.
    pub placeholder_off: f64,
    /// The magnifier's box centre less the field's box centre.
    pub glyph_off: f64,
    /// The centre of the field's line box as its semantics give it (`text.lines`) less the
    /// field's box centre: a single-line field centres its line box.
    pub line_off: f64,
    /// The button's face top and bottom, then the well's outline top and bottom.
    pub button: (f64, f64),
    pub well: (f64, f64),
    /// The cap bands (top, bottom) of `N` (New plate) and `S` (the placeholder).
    pub label: (f64, f64),
    pub placeholder: (f64, f64),
}

pub fn measure(app: &TestApp) -> Header {
    app.locator(Selector::role(Role::Tab).name("Plates")).click();
    app.locator(Selector::role(Role::Button).name("New plate")).wait_visible();
    let field = app.locator(Selector::role(Role::TextField).name("Search plates")).wait_visible().snapshot();
    let window = app.locator(Selector::role(Role::Window)).wait_visible().snapshot();
    let nodes = app.widget_snapshot().nodes();
    let button = nodes.iter().find(|w| w.visible() && w.node.role == Role::Button && w.node.name == "New plate").expect("the New plate button");
    let field = nodes.iter().find(|w| w.visible() && w.node.path == field.path).expect("the search field");
    let glyph = nodes.iter().find(|w| w.visible() && w.node.role == Role::Image && w.node.path.starts_with(field.node.path.trim_end_matches(".input"))).expect("the search field's magnifier");
    let grab = app.screenshot();
    let frame = Frame::decode(&grab);
    let dpi = (frame.width as f64 / window.width() as f64).round().max(1.0);
    let (b, f, g) = (Dev::of(button, dpi), Dev::of(field, dpi), Dev::of(glyph, dpi));
    let line = field.node.text.as_ref().and_then(|t| t.lines.first().copied()).expect("the search field's line box");
    assert!((b.y0 - f.y0).abs() < 0.01 && (b.y1 - f.y1).abs() < 0.01, "the button {b:?} and the field {f:?} are laid out in different rows");
    let edges = |x: f64| {
        let x = x.round() as usize;
        let (above, below) = ((b.y0 - 2.0 * dpi).floor() as usize, (b.y1 + 2.0 * dpi).ceil() as usize);
        (rim(&frame, x, above, 1) / dpi, rim(&frame, x, below, -1) / dpi)
    };
    // the button between its rounded corner and its label; the well past the placeholder
    let button_edges = edges(b.x0 + 7.5 * dpi);
    let well_edges = edges(f.x1 - 6.5 * dpi);
    let label_area = Dev { x0: b.x0 + 4.0 * dpi, x1: b.x1 - 4.0 * dpi, ..b };
    let (lt, lb) = first_glyph_band(&frame, label_area, dpi, "New plate");
    let (pt, pb) = first_glyph_band(&frame, f, dpi, "the placeholder");
    Header {
        dpi,
        grab,
        placeholder_off: ((pt + pb) / 2.0 - f.center_y()) / dpi,
        glyph_off: (g.center_y() - f.center_y()) / dpi,
        line_off: (line.rect.y + line.rect.h / 2.0) as f64 - f.center_y() / dpi,
        button: button_edges,
        well: well_edges,
        label: (lt / dpi, lb / dpi),
        placeholder: (pt / dpi, pb / dpi),
    }
}

pub fn check(h: &Header) {
    let near = |a: f64, b: f64| (a - b).abs() <= TOLERANCE_LPX;
    assert!(h.placeholder_off.abs() <= TOLERANCE_LPX, "dpi {}: the placeholder's ink sits {:+.3} logical px off the field's centre (tolerance {TOLERANCE_LPX}); {h:?}", h.dpi, h.placeholder_off);
    assert!(h.glyph_off.abs() <= TOLERANCE_LPX, "dpi {}: the magnifier sits {:+.3} logical px off the field's centre; {h:?}", h.dpi, h.glyph_off);
    assert!(h.line_off.abs() <= TOLERANCE_LPX, "dpi {}: the field's line box, as its semantics give it, sits {:+.3} logical px off the field's centre (the row's alignment left out?); {h:?}", h.dpi, h.line_off);
    assert!(near(h.button.0, h.well.0) && near(h.button.1, h.well.1), "dpi {}: the button's face spans {:.3}..{:.3} but the search well {:.3}..{:.3} (tolerance {TOLERANCE_LPX}); {h:?}", h.dpi, h.button.0, h.button.1, h.well.0, h.well.1);
    let center = |(t, b): (f64, f64)| (t + b) / 2.0;
    assert!(near(center(h.label), center(h.placeholder)) && near(h.label.1, h.placeholder.1), "dpi {}: New plate's cap band {:.3?} and the placeholder's {:.3?} do not share a centre and a baseline; {h:?}", h.dpi, h.label, h.placeholder);
}

pub fn run(dpi: u32, test: &str, body: impl FnOnce(&TestApp, Header)) {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let home = std::env::temp_dir().join(format!("fab-plates-header-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), test).unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), home.display().to_string());
    config.env.insert("FAB_PLATES".into(), "1".into());
    if dpi != 1 {
        // the first window lays out 1600 by 1000 points at `dpi` device pixels a point
        config.app_args.extend(["--window".to_string(), format!("1600x1000@{dpi}")]);
    }
    let result = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        let header = measure(&app);
        assert_eq!(header.dpi, dpi as f64, "the grab is not at dpi {dpi}: {}", header.grab.display());
        body(&app, header);
    });
    let _ = std::fs::remove_dir_all(&home);
    result.unwrap();
}

#[test]
fn the_search_field_shares_the_new_plate_row_at_1x() {
    run(1, "plates_header::the_search_field_shares_the_new_plate_row_at_1x", |_, h| check(&h));
}

#[test]
fn the_search_field_shares_the_new_plate_row_at_2x() {
    run(2, "plates_header::the_search_field_shares_the_new_plate_row_at_2x", |_, h| check(&h));
}

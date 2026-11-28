//! The window's first frame, from one native GPU grab at dpi 1:
//!
//! 1. Ink-centre of the current workspace's name in the caption's All workspaces button. Same
//!    contract as `examples/text_center/tests/ui.rs`: the cap-height band of a one-line label
//!    sits within 0.5 logical px of the widget box centre.
//! 2. A viewport's composite stays inside its own walk rect, and its rect is never empty. The
//!    bug this pins: the composite quad was drawn before any `DrawList` had been begun inside
//!    the offscreen composite pass, so it landed in the window's draw list at pass-local (0,0)
//!    (on top of the top bar and the tool column) while the viewport's own rect showed an empty
//!    composite target. One cause, both symptoms: the top-bar band above the viewports is still
//!    chrome, and every viewport's rect is not one flat colour.
//! 3. The composite is not an 8x8 AO/cavity grid (an un-upsampled cavity/SSAO).
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`) renders on the native GPU at dpi 1. No visible
//! window is touched and screenshot pixels are layout points.

#[path = "common/workspace_row.rs"]
mod workspace_row;
use makepad_test::{makepad_test, NodeMatch, Selector, TestApp};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::path::Path;

const TOLERANCE_LPX: f64 = 0.5;

/// How far two chrome samples may drift and still count as the same paint.
/// The composite's own background gradient alone is 20 levels wide, and the
/// lit image is far further off than that.
const CHROME_TOLERANCE: i32 = 6;

struct Frame {
    width: usize,
    height: usize,
    rgb: Vec<[u8; 3]>,
}

impl Frame {
    fn decode(path: &Path) -> Frame {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let mut dec = PngDecoder::new(ZCursor::new(&bytes));
        let px = dec.decode_raw().unwrap_or_else(|e| panic!("decode {path:?}: {e:?}"));
        let (width, height) = dec.dimensions().expect("png dimensions");
        let n = dec.colorspace().expect("png colorspace").num_components();
        assert!(n >= 3, "grab is not a colour image ({n} components)");
        let rgb = (0..width * height).map(|i| [px[i * n], px[i * n + 1], px[i * n + 2]]).collect();
        Frame { width, height, rgb }
    }

    fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        self.rgb[y.min(self.height - 1) * self.width + x.min(self.width - 1)]
    }

    /// Luminance-ish for the grey chrome labels: the red channel is enough.
    fn at(&self, x: usize, y: usize) -> f64 {
        self.rgb[y * self.width + x][0] as f64
    }

    /// The most common colour in a row span: for a chrome bar that is its
    /// background, with the text and icons outvoted.
    fn row_mode(&self, y: usize, x0: usize, x1: usize) -> [u8; 3] {
        let x1 = x1.min(self.width);
        assert!(x0 < x1, "empty row span {x0}..{x1} at y={y}");
        let mut tally: std::collections::HashMap<[u8; 3], usize> = std::collections::HashMap::new();
        for x in x0..x1 {
            *tally.entry(self.pixel(x, y)).or_default() += 1;
        }
        tally.into_iter().max_by_key(|(_, n)| *n).map(|(c, _)| c).unwrap()
    }

    /// Distinct colours (5 bits per channel) inside a rect.
    fn distinct_colors_in(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> usize {
        let mut seen = std::collections::HashSet::new();
        for y in y0..y1.min(self.height) {
            for x in x0..x1.min(self.width) {
                let p = self.pixel(x, y);
                seen.insert([p[0] >> 3, p[1] >> 3, p[2] >> 3]);
            }
        }
        seen.len()
    }

    fn luma(&self, x: usize, y: usize) -> i32 {
        let p = self.pixel(x, y);
        (p[0] as i32 * 54 + p[1] as i32 * 183 + p[2] as i32 * 19) / 256
    }

    /// Mean |Δluma| of 8-pixel-aligned block boundaries over interior
    /// adjacent pixels in the same span. ≈1 on a smooth face; ≫1 when the
    /// composite is an un-upsampled 8×8 AO/cavity grid.
    fn blockiness_8(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> f32 {
        let x1 = x1.min(self.width);
        let y1 = y1.min(self.height);
        if x1 <= x0 + 16 || y1 <= y0 + 16 {
            return 0.0;
        }
        let mut boundary = 0i64;
        let mut interior = 0i64;
        let mut n = 0i64;
        for y in y0..y1 {
            let mut x = x0 + 8;
            while x < x1 {
                boundary += (self.luma(x, y) - self.luma(x - 1, y)).abs() as i64;
                interior += (self.luma(x - 4, y) - self.luma(x - 5, y)).abs() as i64;
                n += 1;
                x += 8;
            }
        }
        for x in x0..x1 {
            let mut y = y0 + 8;
            while y < y1 {
                boundary += (self.luma(x, y) - self.luma(x, y - 1)).abs() as i64;
                interior += (self.luma(x, y - 4) - self.luma(x, y - 5)).abs() as i64;
                n += 1;
                y += 8;
            }
        }
        if n == 0 || interior == 0 {
            return 0.0;
        }
        boundary as f32 / interior as f32
    }

    /// Mean and variance of luma in a rect. Used to pick a flat lit face.
    fn luma_stats(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> (f32, f32) {
        let x1 = x1.min(self.width);
        let y1 = y1.min(self.height);
        if x1 <= x0 || y1 <= y0 {
            return (0.0, 0.0);
        }
        let mut sum = 0i64;
        let mut n = 0i64;
        for y in y0..y1 {
            for x in x0..x1 {
                sum += self.luma(x, y) as i64;
                n += 1;
            }
        }
        let mean = sum as f32 / n as f32;
        let mut var = 0.0f32;
        for y in y0..y1 {
            for x in x0..x1 {
                let d = self.luma(x, y) as f32 - mean;
                var += d * d;
            }
        }
        (mean, var / n as f32)
    }
}

fn band_edges(frame: &Frame, x: usize, y0: usize, y1: usize, inside: f64, outside: f64) -> (f64, f64) {
    let span = inside - outside;
    assert!(span.abs() > 8.0, "no contrast between band and surround");
    let cov = |y: usize| ((frame.at(x, y) - outside) / span).clamp(0.0, 1.0);
    let mid = (y0..=y1).max_by(|a, b| cov(*a).partial_cmp(&cov(*b)).unwrap()).expect("non-empty window");
    assert!(cov(mid) > 0.5, "no band on column {x} between {y0} and {y1}");
    let mut top = mid as f64;
    for y in y0..mid {
        top -= cov(y);
    }
    let mut bottom = mid as f64;
    for y in mid..=y1 {
        bottom += cov(y);
    }
    (top, bottom)
}

#[derive(Clone, Copy, Debug)]
struct DevRect {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

fn fill_value(frame: &Frame, r: &DevRect) -> f64 {
    let mut hist = [0usize; 256];
    let y0 = (r.y0 + 2).min(r.y1);
    let y1 = r.y1.saturating_sub(2).max(y0);
    let x0 = (r.x0 + 2).min(r.x1);
    let x1 = r.x1.saturating_sub(2).max(x0);
    for y in y0..y1 {
        for x in x0..x1 {
            hist[frame.at(x, y) as usize] += 1;
        }
    }
    hist.iter().enumerate().max_by_key(|(_, n)| **n).map(|(v, _)| v as f64).unwrap_or(0.0)
}

/// How far (logical px) a label's ink centre sits from its box centre.
fn ink_offset(frame: &Frame, snap: &NodeMatch, dpi: f64) -> f64 {
    let rect = DevRect {
        x0: (snap.x() as f64 * dpi).round() as usize,
        y0: (snap.y() as f64 * dpi).round() as usize,
        x1: ((snap.x() + snap.width()) as f64 * dpi).round() as usize,
        y1: ((snap.y() + snap.height()) as f64 * dpi).round() as usize,
    };
    assert!(rect.x1 <= frame.width && rect.y1 <= frame.height && rect.x1 > rect.x0 + 8, "widget {} rect {rect:?} is not on the frame", snap.path);
    let fill = fill_value(frame, &rect);
    let rim = (2.0 * dpi).ceil() as usize;
    let corner = (2.0 * dpi).ceil() as usize;
    let columns = (rect.x0 + corner)..(rect.x1.saturating_sub(corner).max(rect.x0 + corner + 1));
    let span = columns.len().max(1);
    let lit = |y: usize| columns.clone().filter(|x| (frame.at(*x, y) - fill).abs() > 40.0).count();
    let text_rows: Vec<usize> = ((rect.y0 + rim)..(rect.y1.saturating_sub(rim).max(rect.y0 + rim + 1)))
        .filter(|y| {
            let n = lit(*y);
            n > 0 && n * 2 < span
        })
        .collect();
    assert!(!text_rows.is_empty(), "widget {} has no legible ink over fill {fill}", snap.path);
    let window = (text_rows[0].saturating_sub(2))..=(text_rows[text_rows.len() - 1] + 2).min(frame.height - 1);
    // The cap-height band of the first glyph, as the `H` of `text_center`. The column of most
    // ink anywhere in the word can be an `i` (its dot, a gap, its stem) or a descender, neither
    // of which is the cap band. So: the first glyph's column with the longest unbroken run of
    // ink, the flattest of equal runs (a stem's own antialiased edge, not where a bar joins it).
    let inked = |x: usize| window.clone().any(|y| (frame.at(x, y) - fill).abs() > 40.0);
    let first = columns.clone().find(|x| inked(*x)).unwrap_or_else(|| panic!("widget {} has no ink column", snap.path));
    let glyph = first..columns.clone().skip_while(|x| *x <= first).find(|x| !inked(*x)).unwrap_or(columns.end);
    let run = |x: usize| {
        let (mut best, mut start) = ((0usize, 0.0f64), None);
        for y in window.clone() {
            if (frame.at(x, y) - fill).abs() <= 40.0 {
                start = None;
                continue;
            }
            let from = *start.get_or_insert(y);
            let ink = (from..=y).map(|r| (frame.at(x, r) - fill).abs());
            let spread = ink.clone().fold(f64::MIN, f64::max) - ink.fold(f64::MAX, f64::min);
            if y + 1 - from > best.0 || (y + 1 - from == best.0 && spread < best.1) {
                best = (y + 1 - from, spread);
            }
        }
        best
    };
    let stem = glyph.max_by(|a, b| {
        let (ra, rb) = (run(*a), run(*b));
        ra.0.cmp(&rb.0).then(rb.1.partial_cmp(&ra.1).unwrap())
    }).expect("non-empty glyph");
    let peak = window.clone().map(|y| frame.at(stem, y)).fold(fill, |acc, v| if (v - fill).abs() > (acc - fill).abs() { v } else { acc });
    assert!((peak - fill).abs() > 40.0, "widget {} has no legible ink (peak {peak}, fill {fill})", snap.path);
    let (ink_top, ink_bottom) = band_edges(frame, stem, *window.start(), *window.end(), peak, fill);
    let box_center = (snap.y() as f64 + snap.height() as f64 / 2.0) * dpi;
    ((ink_top + ink_bottom) / 2.0 - box_center) / dpi
}

fn distinct_inset(frame: &Frame, x0: usize, y0: usize, x1: usize, y1: usize) -> usize {
    let inset = 6usize;
    frame.distinct_colors_in(x0 + inset, y0 + inset, x1.saturating_sub(inset), y1.saturating_sub(inset))
}

#[makepad_test]
fn the_first_frame_centres_its_labels_and_keeps_composites_in_their_rects(app: TestApp) {
    workspace_row::wait_shown(&app, "3D modeling");
    // `main` is the shell's root view: its rect is the window's content area,
    // in the same layout points every widget rect below uses.
    let shell = app.locator(Selector::id("main")).wait_visible().snapshot();
    assert!(shell.width() > 0 && shell.height() > 0, "the shell root has no rect: {shell:?}");
    let viewports = Selector::id("viewport");
    let count = app.locator(viewports.clone()).wait_visible().count();
    assert!(count > 0, "no visible FabViewport in the shell: the layout changed, fix the selector");
    let rects: Vec<_> = (0..count).map(|i| app.locator(viewports.clone().nth(i)).snapshot()).collect();

    let path = app.screenshot();
    let frame = Frame::decode(&path);
    // dpi 1 captures, but derive the factor anyway so a retina visible-mode
    // run (MAKEPAD_TEST_VISIBLE=1) measures the same thing.
    let dpi = (frame.width as f64 / shell.width() as f64).round().max(1.0);
    let to_px = |v: i64| ((v as f64) * dpi).round().max(0.0) as usize;

    // The caption draws the current workspace name inside the All workspaces button.
    let caption = app.locator(Selector::role(makepad_test::Role::Button).name("All workspaces")).snapshot();
    let delta = ink_offset(&frame, &caption, dpi);
    assert!(delta.abs() <= TOLERANCE_LPX, "label centring: the current workspace's ink sits {delta:+.3} logical px off the box center (tolerance {TOLERANCE_LPX}); see {path:?}");

    // ---- 2a. the chrome above the viewports is still chrome ----------------
    // A misplaced composite is an opaque quad at pass-local (0,0), the window
    // origin, sized like its viewport, so it covers the top bar from x=0 out
    // to the viewport's width and stops. Comparing the two halves of the same
    // top-bar row needs no colour constant: the bar is one paint.
    let top = rects.iter().map(|r| to_px(r.y())).min().unwrap_or(0);
    assert!(top >= 4, "the first viewport starts at y={top}px: there is no chrome band to test");
    let covered = rects.iter().map(|r| to_px(r.width())).max().unwrap_or(0);
    let untouched = covered + (frame.width - covered) / 2;
    assert!(untouched + 8 < frame.width, "no chrome to the right of the viewports to compare against");
    // Rows above the caption's controls (they sit from y=6 in its 38 px band).
    for y in [2usize, 3, 4] {
        let over = frame.row_mode(y, 0, (frame.width / 4).max(8));
        let clear = frame.row_mode(y, untouched, frame.width);
        let drift = (0..3).map(|c| (over[c] as i32 - clear[c] as i32).abs()).max().unwrap();
        assert!(drift <= CHROME_TOLERANCE, "composite placement: top-bar row y={y} reads {over:?} above the viewports but {clear:?} where no viewport can reach: a viewport composite is painted at the window origin (grab {})", path.display());
    }

    // ---- 2b. every viewport painted something into its own rect ------------
    for (i, r) in rects.iter().enumerate() {
        let (x0, y0) = (to_px(r.x()), to_px(r.y()));
        let (x1, y1) = (x0 + to_px(r.width()), y0 + to_px(r.height()));
        assert!(x1 > x0 + 8 && y1 > y0 + 8, "viewport {i} has a degenerate rect {r:?}");
        // Inset past the 1 px area border and any overlay chrome at the edge.
        let colors = distinct_inset(&frame, x0, y0, x1, y1);
        assert!(colors > 1, "composite placement: viewport {i} rect {r:?} is one flat colour: its composite pass rendered nothing into its own target (grab {})", path.display());
    }

    // ---- 3. the composite is not an 8×8 AO/cavity grid ---------------------
    // The plate view (the leftmost viewport). On a flat lit face the
    // 8-pixel-aligned block-boundary jump must not dwarf the interior 1 px
    // jump: that was the un-upsampled cavity/SSAO (or the 256-wide element
    // LUT sampled as if it were spatial).
    let left = rects.iter().min_by_key(|r| r.x()).expect("no viewport rect");
    let (x0, y0) = (to_px(left.x()), to_px(left.y()));
    let (x1, y1) = (x0 + to_px(left.width()), y0 + to_px(left.height()));
    let inset_x = ((x1 - x0) / 6).max(12);
    let inset_y = ((y1 - y0) / 6).max(12);
    let (ix0, iy0) = (x0 + inset_x, y0 + inset_y);
    let (ix1, iy1) = (x1.saturating_sub(inset_x), y1.saturating_sub(inset_y));
    let patch = 48usize;
    let mut best: Option<(f32, usize, usize)> = None;
    if ix1 > ix0 + patch && iy1 > iy0 + patch {
        let mut y = iy0;
        while y + patch <= iy1 {
            let mut x = ix0;
            while x + patch <= ix1 {
                let (mean, var) = frame.luma_stats(x, y, x + patch, y + patch);
                if mean > 28.0 && mean < 230.0 {
                    match best {
                        Some((v, _, _)) if var >= v => {}
                        _ => best = Some((var, x, y)),
                    }
                }
                x += 16;
            }
            y += 16;
        }
    }
    if let Some((var, x, y)) = best {
        let ratio = frame.blockiness_8(x, y, x + patch, y + patch);
        assert!(ratio < 2.2, "composite upsample: solid viewport has 8×8 blockiness {ratio:.2} (patch variance {var:.1}) at ({x},{y}): cavity/SSAO is composited without a full-res (or bilateral) upsample (grab {})", path.display());
    }
}

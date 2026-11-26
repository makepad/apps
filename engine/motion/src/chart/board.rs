//! Boards: `@scoreboard` (ranked rows that swap ranks between datasets),
//! `@versus` (a head-to-head big-number card whose digits roll and land on
//! a time, e.g. a spoken word) and `@sticker` (price tags and bursts), with
//! their number faces: `@led` (a 5×7 dot matrix whose cells spin like a
//! slot machine), `@roll` (font digits rolling in a masked column, an
//! odometer), `@plain` (count-ups), and `@led3d` (T3's `Text3D` LED preset).

use super::build::*;
use makepad_script::*;
use super::scale::{text_width, PX};
use super::Chart;

/// A 5×7 dot-matrix font: seven rows of five bits, top first.
const FONT: &[(char, [u8; 7])] = &[
    ('0', [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110]),
    ('1', [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('2', [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111]),
    ('3', [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110]),
    ('4', [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010]),
    ('5', [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110]),
    ('6', [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110]),
    ('7', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000]),
    ('8', [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110]),
    ('9', [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100]),
    ('A', [0b01110, 0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001]),
    ('B', [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110]),
    ('C', [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110]),
    ('D', [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100]),
    ('E', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111]),
    ('F', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('G', [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111]),
    ('H', [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
    ('I', [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('J', [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100]),
    ('K', [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001]),
    ('L', [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111]),
    ('M', [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001]),
    ('N', [0b10001, 0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001]),
    ('O', [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('P', [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('Q', [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101]),
    ('R', [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001]),
    ('S', [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110]),
    ('T', [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100]),
    ('U', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('V', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100]),
    ('W', [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010]),
    ('X', [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001]),
    ('Y', [0b10001, 0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100]),
    ('Z', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111]),
    ('.', [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100]),
    (',', [0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b00100, 0b01000]),
    ('-', [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000]),
    ('+', [0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000]),
    (':', [0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000]),
    ('%', [0b11000, 0b11001, 0b00010, 0b00100, 0b01000, 0b10011, 0b00011]),
    ('$', [0b00100, 0b01111, 0b10100, 0b01110, 0b00101, 0b11110, 0b00100]),
    ('/', [0b00000, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b00000]),
    ('?', [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b00000, 0b00100]),
    ('!', [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100]),
    ('#', [0b01010, 0b01010, 0b11111, 0b01010, 0b11111, 0b01010, 0b01010]),
    ('\'', [0b00100, 0b00100, 0b01000, 0b00000, 0b00000, 0b00000, 0b00000]),
    (' ', [0; 7]),
];

fn glyph(c: char) -> [u8; 7] {
    let c = c.to_ascii_uppercase();
    FONT.iter().find(|(g, _)| *g == c).map_or_else(|| glyph('?'), |(_, bits)| *bits)
}

/// A cell's 35 dots as one outline with a fixed command sequence (lit dots
/// round, unlit ones collapsed to their centre), so glyph changes key
/// without resampling.
fn cell_path(bits: [u8; 7], x: f64, y: f64, p: f64) -> String {
    dots(bits, x, y, p, 0.42)
}

/// The dots of a cell, each `radius` × the pitch.
fn dots(bits: [u8; 7], x: f64, y: f64, p: f64, radius: f64) -> String {
    let mut out = String::with_capacity(35 * 90);
    for row in 0..7 {
        for col in 0..5 {
            let lit = bits[row] & (1 << (4 - col)) != 0;
            let (cx, cy) = (x + (col as f64 + 0.5) * p, y + (row as f64 + 0.5) * p);
            let r = if lit { p * radius } else { 0.0 };
            let k = r * 0.5523;
            out.push_str(&format!(
                "M {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} Z ",
                cx + r, cy, cx + r, cy + k, cx + k, cy + r, cx, cy + r, cx - k, cy + r, cx - r, cy + k, cx - r, cy, cx - r, cy - k, cx - k, cy - r, cx, cy - r,
                cx + k, cy - r, cx + r, cy - k, cx + r, cy
            ));
        }
    }
    out
}

/// A deterministic hash for the slot machine's passing glyphs.
fn hash(a: u64, b: u64, c: u64) -> u64 {
    let mut x = a.wrapping_mul(0x9e3779b97f4a7c15) ^ b.wrapping_mul(0xbf58476d1ce4e5b9) ^ c.wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    x = x.wrapping_mul(0xd6e8feb86659fd39);
    x ^ (x >> 29)
}

/// The width of `n` LED cells at dot pitch `p` (a dot column between cells).
pub(crate) fn led_width(n: usize, p: f64) -> f64 {
    (n as f64 * 6.0 - 1.0).max(0.0) * p
}

/// One LED line through its states: `(text, when it lands, colour)`. Every
/// cell whose glyph changes spins through `spin` passing glyphs over
/// `roll` seconds and lands on its new glyph, the rightmost first.
pub(crate) fn led_line(c: &mut Chart, name: &str, states: &[(String, ScriptValue, u32)], at: [f64; 2], p: f64, roll: f64, seed: u64) {
    let b = c.b;
    let cells = states.iter().map(|(t, _, _)| t.chars().count()).max().unwrap_or(0);
    if cells == 0 {
        return;
    }
    // Callouts keep off the board's lines.
    c.avoid.push([at[0], at[1], at[0] + led_width(cells, p), at[1] + 7.0 * p]);
    // The unlit dots, faint and static.
    let dim = alpha(c.th.ink, 0x1c);
    let mut unlit = String::new();
    for i in 0..cells {
        unlit.push_str(&dots([0b11111; 7], at[0] + i as f64 * 6.0 * p, at[1], p, 0.3));
    }
    c.out.push(layer(b, "Path", vec![("name", s(b, &format!("led dots {name}"))), ("svg", s(b, &unlit)), ("fill", color(dim)), ("opacity", fade_in(b, c.start, 0.3))]));
    let digits: Vec<char> = "0123456789".chars().collect();
    let letters: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZ".chars().collect();
    for i in 0..cells {
        let (x, y) = (at[0] + i as f64 * 6.0 * p, at[1]);
        let mut shapes = vec![(shift(b, states[0].1, -roll - 1.0), s(b, &cell_path(glyph(' '), x, y, p)))];
        let mut fills = vec![(shift(b, states[0].1, -roll - 1.0), color(states[0].2))];
        let mut prev = ' ';
        for (k, (text, time, ink)) in states.iter().enumerate() {
            let target = text.chars().nth(i).unwrap_or(' ').to_ascii_uppercase();
            // The rightmost cell lands first, the leftmost on `time`.
            let lands = shift(b, *time, -(i as f64) * (0.25 / cells as f64).min(0.035));
            let starts = shift(b, lands, -roll);
            fills.push((starts.clone(), color(*ink)));
            if target != prev {
                let pool = if target.is_ascii_digit() || prev.is_ascii_digit() { &digits } else { &letters };
                let spin = 7;
                for step in 0..spin {
                    let pick = pool[(hash(seed, (i * 31 + k) as u64, step as u64) % pool.len() as u64) as usize];
                    shapes.push((shift(b, starts, roll * step as f64 / spin as f64), s(b, &cell_path(glyph(pick), x, y, p))));
                }
                shapes.push((lands, s(b, &cell_path(glyph(target), x, y, p))));
            }
            prev = target;
        }
        if shapes.len() == 1 {
            continue;
        }
        c.out.push(layer(b, "Path", vec![("name", s(b, &format!("led {name} {i}"))), ("svg", keys(b, shapes, id("hold"))), ("fill", keys(b, fills, id("hold")))]));
    }
}

/// Font digits rolling in masked columns (an odometer) to `text` at
/// `lands`, each digit turning once through 0–9 on its way; other
/// characters fade in. `at` is the line's left top.
#[allow(clippy::too_many_arguments)]
pub(crate) fn roll_line(c: &mut Chart, name: &str, text: &str, at: [f64; 2], size: f64, weight: f64, ink: u32, lands: ScriptValue, roll: f64) {
    let b = c.b;
    let digit_w = text_width("0", size, weight);
    let h = size * 1.21;
    let mut x = at[0];
    let n = text.chars().count();
    for (i, ch) in text.chars().enumerate() {
        let w = if ch.is_ascii_digit() { digit_w } else { text_width(&ch.to_string(), size, weight) };
        if ch.is_ascii_digit() {
            let target = ch.to_digit(10).unwrap() as usize;
            // The wheel: one turn or two from 0 past the target, landing on
            // it. Two number Texts do it (the digit in the window and the
            // next one above it): their values step at each row and their
            // offset saws through one row, on the column's eased travel
            // (ease_out_cubic), so nothing moves outside the cell.
            let turns = 1 + (n - i) % 2;
            let strip: Vec<usize> = (0..=(turns * 10 + target)).map(|j| j % 10).collect();
            let last = strip.len() - 1;
            let cell_lands = shift(b, lands, -((n - 1 - i) as f64) * 0.06);
            let row_at = |j: usize| -roll + roll * (1.0 - (1.0 - j as f64 / last as f64).cbrt());
            let mut digit = vec![(shift(b, cell_lands, row_at(0)), num(strip[0] as f64))];
            let mut next = vec![(shift(b, cell_lands, row_at(0)), num(strip[1.min(last)] as f64))];
            let mut offset = vec![(shift(b, cell_lands, row_at(0)), num(0.0))];
            for j in 1..=last {
                let tj = row_at(j);
                digit.push((shift(b, cell_lands, tj), num(strip[j] as f64)));
                next.push((shift(b, cell_lands, tj), num(strip[(j + 1).min(last)] as f64)));
                offset.push((shift(b, cell_lands, tj - 0.0005), num(h)));
                offset.push((shift(b, cell_lands, tj), num(0.0)));
            }
            let face = |role: &str, values: Vec<(ScriptValue, ScriptValue)>, dy: f64| layer(b, "Text", vec![
                ("name", s(b, &format!("{role} · {name} {ch}"))), ("number", keys(b, values, id("hold"))), ("decimals", num(0.0)), ("font", font_face(b, &c.th.title_font, weight)),
                 ("size", num(size / PX)), ("color", color(ink)), ("at", v2(b, x + w * 0.5, at[1] + dy)), ("align", id("center")),
            ]);
            let mut saw = Vec::new();
            for (k, (t, v)) in offset.into_iter().enumerate() {
                // Linear down one row, then back at once on the step.
                saw.push((t, v, if k % 2 == 1 { "linear" } else { "hold" }));
            }
            let saw = layer(b, "Keys", vec![("points", list(b, saw.into_iter().enumerate().map(|(k, (t, v, e))| if k == 0 { list(b, vec![t, v]) } else { list(b, vec![t, v, id(e)]) }).collect()))]);
            // The next digit comes down from above (inside the frame, under
            // the label), and only while the wheel turns.
            let incoming = face("roll", next, -h);
            set(b, incoming, "opacity", keys(b, vec![(shift(b, cell_lands, -roll), num(1.0)), (cell_lands.clone(), num(1.0)), (shift(b, cell_lands, 0.001), num(0.0))], id("linear")));
            let column = layer(b, "Group", vec![("name", s(b, "roll column")), ("layers", list(b, vec![face("value", digit, 0.0), incoming])), ("y", saw)]);
            c.out.push(layer(b, "Group", vec![
                ("name", s(b, &format!("digit {name} {i}"))),
                ("mask", layer(b, "Rect", vec![("at", v2(b, x - 2.0, at[1])), ("size", v2(b, w + 4.0, h)), ("fill", color(0xffffffff))])),
                ("layers", list(b, vec![column])), ("opacity", fade_in(b, shift(b, cell_lands, -roll), 0.15)),
            ]));
        } else if ch != ' ' {
            c.out.push(layer(b, "Text", vec![
                ("name", s(b, &format!("label · {ch}"))), ("text", s(b, &ch.to_string())), ("font", font_face(b, &c.th.title_font, weight)),  ("size", num(size / PX)),
                ("color", color(ink)), ("at", v2(b, x, at[1])), ("opacity", fade_in(b, lands, 0.3)),
            ]));
        }
        x += w;
    }
}

fn line_width(text: &str, size: f64, weight: f64) -> f64 {
    let digit_w = text_width("0", size, weight);
    text.chars().map(|ch| if ch.is_ascii_digit() { digit_w } else { text_width(&ch.to_string(), size, weight) }).sum()
}

/// The chart's number face: `face:`, else the theme's.
fn face(c: &Chart, default: &str) -> String {
    c.face.clone().unwrap_or_else(|| if c.theme == "benchmark" { "led".into() } else { default.into() })
}

/// A board's panel behind everything.
fn panel(c: &mut Chart, rect: [f64; 4]) {
    let b = c.b;
    let start = c.start.clone();
    c.out.push(layer(b, "Rect", vec![
        ("name", s(b, "board")), ("at", v2(b, rect[0], rect[1])), ("size", v2(b, rect[2] - rect[0], rect[3] - rect[1])), ("radius", num(14.0)),
        ("fill", color(c.th.card)), ("stroke", color(alpha(c.th.ink, 0x30))), ("width", num(2.0)), ("opacity", fade_in(b, start, 0.35)),
    ]));
}

/// `@scoreboard`: ranked slots through the datasets. With `@led` every
/// slot's line spins to its new standing (a slot machine); with `@plain`
/// or `@roll` the rows move to their new ranks and their values count.
/// `layout: @line` puts the slots on one line ("1 BRUTE 2 SPARK"). A row's
/// `text` ("LEAD") shows in place of its value.
pub(super) fn scoreboard(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let sets = c.sets.clone();
    let face = face(c, "plain");
    let line = c.layout == "line";
    let ranked: Vec<Vec<super::Datum>> = sets.iter().map(|t| {
        let mut rows = t.rows.clone();
        if c.sort || !rows.iter().any(|d| d.text.is_some()) {
            rows.sort_by(|a, b| b.value.total_cmp(&a.value));
        }
        rows
    }).collect();
    let mut names: Vec<String> = Vec::new();
    for rows in &ranked {
        for d in rows {
            if !names.contains(&d.label) {
                names.push(d.label.clone());
            }
        }
    }
    c.keys = names.clone();
    let slots = c.top.unwrap_or_else(|| ranked.iter().map(Vec::len).max().unwrap_or(1)).max(1);
    let all: Vec<f64> = ranked.iter().flatten().map(|d| d.value).collect();
    let shown = c.fmt.shown(all.iter().fold(0.0f64, |a, v| a.max(v.abs())), 0.0, &all);
    let cell = |d: &super::Datum| d.text.clone().unwrap_or_else(|| shown.text(d.value)).to_uppercase();
    let row_ink = |c: &Chart, name: &str| {
        let i = names.iter().position(|n| n == name).unwrap_or(0);
        if !c.highlight.is_empty() { if c.highlighted(i) { c.th.accent } else { c.th.ink } } else { c.th.color(i) }
    };
    c.header(&[]);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    panel(c, [px0, py0, px1, py1]);
    let pad = ls * 0.9;
    let (x0, y0, w, h) = (px0 + pad, py0 + pad, px1 - px0 - 2.0 * pad, py1 - py0 - 2.0 * pad);
    let times: Vec<ScriptValue> = (0..sets.len()).map(|k| c.set_time(k, 0)).collect();
    let name_w = names.iter().map(|n| n.chars().count()).max().unwrap_or(1);
    let value_w = ranked.iter().flatten().map(|d| cell(d).chars().count()).max().unwrap_or(1);
    if face == "led" || face == "led3d" {
        // Each slot (or the whole line) is one LED line through the states.
        let texts: Vec<Vec<(String, u32)>> = if line {
            ranked.iter().map(|rows| {
                let text = rows.iter().take(slots).enumerate().map(|(r, d)| match &d.text {
                    Some(t) => format!("{} {} {}", r + 1, d.label.to_uppercase(), t.to_uppercase()),
                    None => format!("{} {}", r + 1, d.label.to_uppercase()),
                }).collect::<Vec<_>>().join("  ");
                vec![(text, rows.first().map_or(c.th.ink, |d| row_ink(c, &d.label)))]
            }).collect()
        } else {
            ranked.iter().map(|rows| (0..slots).map(|r| match rows.get(r) {
                Some(d) => {
                    let value = if c.values { cell(d) } else { d.text.clone().unwrap_or_default().to_uppercase() };
                    (format!("{} {:<nw$} {:>vw$}", r + 1, d.label.to_uppercase(), value, nw = name_w, vw = value_w), row_ink(c, &d.label))
                }
                None => (String::new(), c.th.ink),
            }).collect()).collect()
        };
        let lines = texts.iter().map(Vec::len).max().unwrap_or(1);
        let chars = texts.iter().flatten().map(|(t, _)| t.chars().count()).max().unwrap_or(1);
        let p = (w / (chars as f64 * 6.0 - 1.0)).min(h / (lines as f64 * 9.0 - 2.0));
        let block_h = (lines as f64 * 9.0 - 2.0) * p;
        let top = y0 + (h - block_h) * 0.5;
        let left = x0 + (w - led_width(chars, p)) * 0.5;
        for l in 0..lines {
            let states: Vec<(String, ScriptValue, u32)> = texts.iter().zip(&times).map(|(lines, t)| {
                let (text, ink) = lines.get(l).cloned().unwrap_or_default();
                (text, t.clone(), ink)
            }).collect();
            if face == "led3d" {
                // T3's LED reels, one Text3D per state: its digits roll and
                // land on the state's time, and it shows until the next.
                let dur = c.grow.max(0.6);
                let (bw, bh) = (led_width(chars, p), 7.0 * p);
                for (k, (text, time, ink)) in states.iter().enumerate() {
                    if text.trim().is_empty() {
                        continue;
                    }
                    let begin = shift(b, *time, -dur);
                    // Until the next state takes over (a later reel draws its
                    // own board over this one when the times are references).
                    let span = match (number_of(b, begin), c.switch.get(k).filter(|_| k + 1 < states.len()).and_then(|t| number_of(b, *t))) {
                        (Some(a), Some(z)) => list(b, vec![num(a), num((z - a).max(0.0))]),
                        _ => list(b, vec![begin, num(3600.0)]),
                    };
                    c.out.push(layer(b, "Text3D", vec![
                        ("name", s(b, &format!("led3d row {l} {k}"))), ("preset", id("led_roll")), ("text", s(b, text)), ("color", color(*ink)), ("background", id("none")),
                        ("at", v2(b, left, top + l as f64 * 9.0 * p)), ("size", v2(b, bw, bh)), ("duration", num(dur)), ("fit", num(1.0)), ("span", span),
                    ]));
                }
                continue;
            }
            led_line(c, &format!("row {l}"), &states, [left, top + l as f64 * 9.0 * p], p, c.grow.max(0.3) * 0.7, 7 + l as u64);
        }
        for (k, rows) in ranked.iter().enumerate() {
            for (r, d) in rows.iter().enumerate().take(slots) {
                let i = names.iter().position(|n| n == &d.label).unwrap_or(0);
                let y = if line { top } else { top + r as f64 * 9.0 * p };
                c.mark(k, i, [left + led_width(chars, p) * 0.5, y]);
            }
        }
        return Ok(());
    }
    // Rows that move: rank numbers stay, each name's row travels to its slot.
    let row_h = h / slots as f64;
    let size = (row_h * 0.55).min(ls * 1.6);
    for r in 0..slots {
        let label = c.text("label", &format!("{}", r + 1), [x0, y0 + row_h * (r as f64 + 0.5)], "left", "center", size, 800.0, c.th.muted, c.start, None);
        c.out.push(label);
    }
    let ease = c.ease("ease_in_out_cubic");
    for (i, name) in names.iter().enumerate() {
        let slot = |k: usize| ranked[k].iter().position(|d| &d.label == name).filter(|r| *r < slots);
        let y_of = |k: usize| y0 + row_h * (slot(k).unwrap_or(slots) as f64 + 0.5);
        let mut ys = vec![(shift(b, times[0], -c.grow), num(y_of(0)))];
        let mut op = vec![(shift(b, times[0], -c.grow), num(0.0)), (times[0].clone(), num(if slot(0).is_some() { 1.0 } else { 0.0 }))];
        for k in 1..sets.len() {
            ys.push((c.switch[k - 1].clone(), num(y_of(k - 1))));
            ys.push((times[k].clone(), num(y_of(k))));
            op.push((c.switch[k - 1].clone(), num(if slot(k - 1).is_some() { 1.0 } else { 0.0 })));
            op.push((times[k].clone(), num(if slot(k).is_some() { 1.0 } else { 0.0 })));
        }
        let ink = row_ink(c, name);
        let label = c.text("label", &name.to_uppercase(), [x0 + size * 1.6, 0.0], "left", "center", size, 800.0, ink, times[0], Some(w * 0.5));
        set(b, label, "y", keys(b, ys.clone(), ease));
        set(b, label, "opacity", keys(b, op.clone(), ease));
        c.out.push(label);
        let values: Vec<f64> = (0..sets.len()).map(|k| ranked[k].iter().find(|d| &d.label == name).map_or(0.0, |d| d.value)).collect();
        let note = ranked.last().and_then(|rows| rows.iter().find(|d| &d.label == name)).and_then(|d| d.text.clone());
        let value = match note {
            Some(note) => c.text("value", &note.to_uppercase(), [x0 + w, 0.0], "right", "center", size, 800.0, ink, *times.last().unwrap(), None),
            None => c.number("value", &values, &times, &shown, v2(b, x0 + w, 0.0), "right", "center", size, 800.0, ink, None),
        };
        set(b, value, "y", keys(b, ys, ease));
        set(b, value, "opacity", keys(b, op, ease));
        c.out.push(value);
        for k in 0..sets.len() {
            if let Some(r) = slot(k) {
                c.mark(k, i, [x0 + w * 0.5, y0 + row_h * r as f64]);
            }
        }
    }
    Ok(())
}

/// `@versus`: a head-to-head card, one column per row (name over a big
/// number) with a "/" between, the numbers landing at `land` (a word).
pub(super) fn versus(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let rows = c.sets[0].rows.clone();
    let n = rows.len().max(1);
    c.keys = rows.iter().map(|d| d.label.clone()).collect();
    let face = face(c, "roll");
    let title = c.title.take();
    c.header(&[]);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    panel(c, [px0, py0, px1, py1]);
    let pad = ls;
    let (x0, mut y0, w, y1) = (px0 + pad, py0 + pad, px1 - px0 - 2.0 * pad, py1 - pad);
    let start = c.start.clone();
    if let Some(title) = &title {
        let text = title.to_uppercase();
        if face == "led" {
            let p = (w * 0.7 / (text.chars().count() as f64 * 6.0 - 1.0)).min((y1 - y0) * 0.16 / 7.0);
            led_line(c, "title", &[(text.clone(), shift(b, start, 0.6), c.th.ink)], [x0 + (w - led_width(text.chars().count(), p)) * 0.5, y0], p, 0.5, 3);
            y0 += 7.0 * p + ls;
        } else {
            let size = ((y1 - y0) * 0.14).min(ls * 2.2);
            let t = c.text("title", &text, [x0 + w * 0.5, y0], "center", "top", size, 800.0, c.th.ink, start, Some(w));
            set(b, t, "font", font_face(b, &c.th.title_font, 800.0));
            c.out.push(t);
            y0 += size * 1.35;
        }
    }
    let col_w = w / n as f64;
    let shown = c.fmt.shown(rows.iter().fold(0.0f64, |a, d| a.max(d.value.abs())), 0.0, &rows.iter().map(|d| d.value).collect::<Vec<_>>());
    for (i, d) in rows.iter().enumerate() {
        let ink = if c.colors_given || c.theme == "benchmark" || c.highlight.is_empty() { c.th.color(i) } else if c.highlighted(i) { c.th.accent } else { c.th.muted };
        let cx = x0 + col_w * (i as f64 + 0.5);
        let t = c.land(i);
        let name_size = ls * 1.1;
        let label = c.text("label", &d.label.to_uppercase(), [cx, y0], "center", "top", name_size, 700.0, ink, shift(b, t, -c.grow), Some(col_w * 0.9));
        set(b, label, "font", font_face(b, &c.th.title_font, 700.0));
        c.out.push(label);
        let top = y0 + name_size * 1.5;
        let room = y1 - top;
        let text = d.text.clone().unwrap_or_else(|| shown.text(d.value));
        match face.as_str() {
            "led" => {
                let chars = text.chars().count();
                let p = (col_w * 0.8 / (chars as f64 * 6.0 - 1.0)).min(room * 0.8 / 7.0);
                led_line(c, &d.label, &[(text.clone(), t.clone(), ink)], [cx - led_width(chars, p) * 0.5, top + (room - 7.0 * p) * 0.5], p, c.grow.max(0.6) * 0.8, 11 + i as u64);
                c.mark(0, i, [cx, top + (room - 7.0 * p) * 0.5]);
            }
            "led3d" => {
                let (bw, bh) = (col_w * 0.85, room * 0.8);
                c.out.push(layer(b, "Text3D", vec![
                    ("name", s(b, &format!("led3d {}", d.label))), ("preset", id("led_roll")), ("text", s(b, &text)), ("color", color(ink)), ("background", id("none")),
                    ("at", v2(b, cx - bw * 0.5, top + (room - bh) * 0.5)), ("size", v2(b, bw, bh)), ("duration", num(c.grow.max(0.6))), ("span", list(b, vec![shift(b, t, -c.grow.max(0.6)), num(3600.0)])),
                ]));
                c.mark(0, i, [cx, top + (room - bh) * 0.5]);
            }
            "plain" => {
                let size = (col_w * 0.85 / text_width(&text, 1.0, 800.0)).min(room * 0.7);
                let label = c.number("value", &[d.value], &[t.clone()], &shown, v2(b, cx, top + room * 0.5), "center", "center", size, 800.0, ink, Some(col_w * 0.9));
                c.out.push(label);
                c.mark(0, i, [cx, top + room * 0.5 - size * 0.6]);
            }
            _ => {
                let size = (col_w * 0.85 / line_width(&text, 1.0, 800.0)).min(room * 0.7 / 1.21);
                let lw = line_width(&text, size, 800.0);
                let y = top + (room - size * 1.21) * 0.5;
                roll_line(c, &d.label, &text, [cx - lw * 0.5, y], size, 800.0, ink, t, c.grow.max(0.9));
                c.mark(0, i, [cx, y]);
            }
        }
        if i + 1 < n {
            let x = x0 + col_w * (i as f64 + 1.0);
            let slash = c.text("label", "/", [x, top + room * 0.5], "center", "center", (room * 0.4).min(ls * 4.0), 400.0, c.th.muted, start, None);
            c.out.push(slash);
        }
    }
    c.title = title;
    Ok(())
}

/// A sticker's shape, text and stamp-in, as one group centred on
/// `centre`: `@tag` (a price tag with its hole), `@burst` (a starburst) or
/// `@circle`. Returns the group and its box size.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sticker(c: &Chart, shape: &str, main: &str, sub: Option<&str>, centre: [f64; 2], size: f64, fill: u32, t: ScriptValue, tilt: f64) -> (ScriptValue, [f64; 2]) {
    let b = c.b;
    let ink = c.th.ink_on(fill);
    let main_w = text_width(main, size, 800.0);
    let sub_size = size * 0.45;
    let sub_w = sub.map_or(0.0, |s| text_width(s, sub_size, 700.0));
    let text_w = main_w.max(sub_w);
    let text_h = size * 1.21 + sub.map_or(0.0, |_| sub_size * 1.25);
    let (w, h, outline, notch) = match shape {
        "burst" | "circle" => {
            let r = (text_w * 0.5 + size * 0.5).max(text_h * 0.5 + size * 0.5) * if shape == "burst" { 1.18 } else { 1.0 };
            let (cx, cy) = (r, r);
            let path = if shape == "burst" {
                let spikes = 16;
                (0..spikes * 2).map(|k| {
                    let a = k as f64 / (spikes * 2) as f64 * std::f64::consts::TAU;
                    let rr = if k % 2 == 0 { r } else { r * 0.82 };
                    format!("{} {:.2} {:.2}", if k == 0 { "M" } else { "L" }, cx + rr * a.sin(), cy - rr * a.cos())
                }).collect::<Vec<_>>().join(" ") + " Z"
            } else {
                let k = r * 0.5523;
                format!("M {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} Z",
                    cx + r, cy, cx + r, cy + k, cx + k, cy + r, cx, cy + r, cx - k, cy + r, cx - r, cy + k, cx - r, cy, cx - r, cy - k, cx - k, cy - r, cx, cy - r, cx + k, cy - r, cx + r, cy - k, cx + r, cy)
            };
            (2.0 * r, 2.0 * r, path, 0.0)
        }
        _ => {
            let (h, notch) = (text_h + size * 0.7, text_h * 0.35 + size * 0.3);
            let w = text_w + size * 1.1 + notch;
            (w, h, format!("M {:.2} 0 L {:.2} 0 L {:.2} {:.2} L {:.2} {:.2} L 0 {:.2} Z", notch, w, w, h, notch, h, h * 0.5), notch)
        }
    };
    // Centred in the tag's body, right of its notch.
    let text_cx = (w + notch) * 0.5;
    // Its texts are named `sticker · …` (not a checked role): a tilted
    // sticker's boxes overlap each other by design.
    let mut layers = vec![
        layer(b, "Path", vec![("name", s(b, "sticker shadow")), ("svg", s(b, &outline)), ("fill", color(0x00000066)), ("x", num(size * 0.12)), ("y", num(size * 0.16))]),
        layer(b, "Path", vec![("name", s(b, "sticker")), ("svg", s(b, &outline)), ("fill", color(fill))]),
    ];
    if !matches!(shape, "burst" | "circle") {
        let r = size * 0.16;
        layers.push(layer(b, "Ellipse", vec![("name", s(b, "tag hole")), ("at", v2(b, notch * 0.75 - r, h * 0.5 - r)), ("size", v2(b, 2.0 * r, 2.0 * r)), ("fill", color(if c.th.card & 0xff == 0xff { c.th.card } else { 0x000000aa }))]));
    }
    let mut y = (h - text_h) * 0.5;
    if let Some(sub) = sub {
        layers.push(layer(b, "Text", vec![
            ("name", s(b, &format!("sticker · {sub}"))), ("text", s(b, sub)), ("font", font_face(b, &c.th.font, 700.0)),  ("size", num(sub_size / PX)), ("color", color(ink)),
            ("at", v2(b, text_cx, y)), ("align", id("center")),
        ]));
        y += sub_size * 1.25;
    }
    layers.push(layer(b, "Text", vec![
        ("name", s(b, &format!("sticker · {main}"))), ("text", s(b, main)), ("font", font_face(b, &c.th.title_font, 800.0)),  ("size", num(size / PX)), ("color", color(ink)),
        ("at", v2(b, text_cx, y)), ("align", id("center")),
    ]));
    let group = layer(b, "Group", vec![
        ("name", s(b, &format!("sticker {main}"))), ("at", v2(b, centre[0] - w * 0.5, centre[1] - h * 0.5)), ("size", v2(b, w, h)), ("layers", list(b, layers)),
        ("scale", keys(b, vec![(shift(b, t, -0.25), num(1.6)), (t.clone(), num(1.0))], id("ease_out_back"))),
        ("rotate", keys(b, vec![(shift(b, t, -0.25), num(tilt * 2.5)), (t.clone(), num(tilt))], id("ease_out_back"))),
        ("opacity", keys(b, vec![(shift(b, t, -0.26), num(0.0)), (shift(b, t, -0.2), num(1.0))], id("linear"))),
    ]);
    (group, [w, h])
}

/// `@sticker`: stickers that stamp in at their `land` times, laid out
/// across the box. `data: [["CACHE READS", "$$$"]]`: a label and its big
/// text (or a value, formatted). `shape: @tag | @burst | @circle`.
pub(super) fn stickers(c: &mut Chart) -> Result<(), String> {
    let rows = c.sets[0].rows.clone();
    c.keys = rows.iter().map(|d| d.label.clone()).collect();
    c.header(&[]);
    let [px0, py0, px1, py1] = c.plot;
    let n = rows.len().max(1);
    let shown = c.fmt.shown(rows.iter().fold(0.0f64, |a, d| a.max(d.value.abs())), 0.0, &rows.iter().map(|d| d.value).collect::<Vec<_>>());
    let shape = c.shape.clone().unwrap_or_else(|| "tag".into());
    for (i, d) in rows.iter().enumerate() {
        let main = d.text.clone().unwrap_or_else(|| shown.text(d.value));
        let sub = (!d.label.is_empty()).then(|| d.label.to_uppercase());
        let slot_w = (px1 - px0) / n as f64;
        let centre = [px0 + slot_w * (i as f64 + 0.5), (py0 + py1) * 0.5];
        // As big as the slot allows.
        let (_, [w1, h1]) = sticker(c, &shape, &main, sub.as_deref(), centre, 100.0, 0, num(0.0), 0.0);
        let size = (100.0 * (slot_w * 0.8 / w1).min((py1 - py0) * 0.8 / h1)).min(c.ls * 4.0);
        let fill = if c.colors_given || c.theme == "benchmark" { c.th.color(i) } else { c.th.accent };
        let tilt = if i % 2 == 0 { -6.0 } else { 5.0 };
        let (group, [_, h]) = sticker(c, &shape, &main, sub.as_deref(), centre, size, fill, c.land(i), tilt);
        c.out.push(group);
        c.mark(0, i, [centre[0], centre[1] - h * 0.5]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn led_cells_share_one_command_sequence() {
        let a = cell_path(glyph('5'), 0.0, 0.0, 4.0);
        let b = cell_path(glyph('B'), 10.0, 0.0, 4.0);
        fn commands(d: &str) -> Vec<&str> {
            d.split_whitespace().filter(|t| t.chars().all(|c| c.is_ascii_alphabetic())).collect()
        }
        assert_eq!(commands(&a), commands(&b));
        assert_eq!(commands(&a).iter().filter(|c| **c == "M").count(), 35);
        assert_eq!(glyph('x'), glyph('X'));
        assert_eq!(glyph('€'), glyph('?'));
        assert_eq!(led_width(3, 2.0), 34.0);
    }
}

//! `face: @drum`: a mechanical odometer. Each digit is a wheel in a
//! housing, shaded like a cylinder; the count runs on `ease_out_cubic` and
//! each wheel turns only as the one to its right wraps (the carry), the
//! fast wheels blurring through their digits. `overflow: @grow` starts
//! with the wheels the first value needs and adds one at each new power of
//! ten, so a count to 1e30 runs out of the frame.

use super::build::*;
use makepad_script::*;
use super::scale::{text_width, PX};
use super::Chart;

/// The fraction of the count's time at which it reaches `v` of `total`
/// (the inverse of ease_out_cubic).
fn when(v: f64, total: f64) -> f64 {
    let f = (v / total.max(1e-300)).clamp(0.0, 1.0);
    1.0 - (1.0 - f).cbrt()
}

/// `value` written out in full with 15 significant digits and zeros after
/// them ("1e30" → "1,000,…,000"), grouped, with `decimals` digits: no
/// floating-point noise in the low wheels of a huge count.
pub(crate) fn digits(value: f64, decimals: u32) -> String {
    let value = value.max(0.0);
    if value < 1e15 {
        return crate::format_number(value, decimals, Some(","));
    }
    let sci = format!("{:.14e}", value);
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: usize = exp.parse().unwrap_or(0);
    let mut int: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    while int.len() < exp + 1 {
        int.push('0');
    }
    int.truncate(exp + 1);
    let mut out = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// The width a drum of `value` takes at digit size `size`.
pub(crate) fn drum_width(value: f64, decimals: u32, size: f64) -> f64 {
    let text = digits(value, decimals);
    let (cell_w, sep_w, gap, pad) = (text_width("0", size, 800.0) * 1.35, text_width(",", size, 800.0) * 0.9, size * 0.12, size * 0.25);
    text.chars().map(|ch| if ch.is_ascii_digit() { cell_w } else { sep_w } + gap).sum::<f64>() - gap + 2.0 * pad
}

/// A drum counting from 0 to `value` over `[lands − roll, lands]`, its
/// right edge at `right`, its top at `top`, wheels `size` tall (the digit's
/// em). Returns its width.
#[allow(clippy::too_many_arguments)]
pub(crate) fn drum(c: &mut Chart, name: &str, value: f64, decimals: u32, right: f64, top: f64, size: f64, lands: ScriptValue, roll: f64, grow: bool) -> f64 {
    let b = c.b;
    let value = value.max(0.0);
    let text = digits(value, decimals);
    // The wheels' final digits, from the clean text (place 0 first).
    let finals: Vec<f64> = text.chars().rev().filter(|c| c.is_ascii_digit()).map(|c| c.to_digit(10).unwrap() as f64).collect();
    let scaled = value * 10f64.powi(decimals as i32);
    let cell_w = text_width("0", size, 800.0) * 1.35;
    let h = size * 1.21;
    let gap = size * 0.12;
    let sep_w = text_width(",", size, 800.0) * 0.9;
    let pad = size * 0.25;
    let begin = shift(b, lands, -roll);
    let at_frac = |f: f64| shift(b, begin, roll * f);
    // Characters right to left: digits are wheels (place p), others plates.
    let chars: Vec<char> = text.chars().rev().collect();
    let mut x = right - pad;
    let mut place = 0i32;
    // (x, the wheel's place (None for a separator), the place it waits for, char)
    let mut cells: Vec<(f64, Option<i32>, i32, char)> = Vec::new();
    for ch in &chars {
        let w = if ch.is_ascii_digit() { cell_w } else { sep_w };
        x -= w;
        cells.push((x, ch.is_ascii_digit().then_some(place), place, *ch));
        if ch.is_ascii_digit() {
            place += 1;
        }
        x -= gap;
    }
    let left = x - pad + gap;
    let width = right - left;
    let wheel_ink = c.th.ink;
    let digit_ink = 0x111318ff;
    // The housing, dark, behind the wheels.
    c.out.push(layer(b, "Rect", vec![
        ("name", s(b, &format!("drum {name}"))), ("at", v2(b, left, top - pad)), ("size", v2(b, width, h + 2.0 * pad)), ("radius", num(size * 0.18)),
        ("fill", color(0x0b0c0aff)), ("stroke", color(alpha(c.th.ink, 0x40))), ("width", num(2.0)), ("opacity", fade_in(b, begin, 0.3)),
    ]));
    for (x, place, waits, ch) in cells {
        // A wheel (or a separator) appears when the count first needs it
        // (overflow: @grow).
        let appear = if grow && waits > 0 { at_frac(when(10f64.powi(waits), scaled)) } else { begin.clone() };
        let Some(p) = place else {
            c.out.push(layer(b, "Text", vec![
                ("name", s(b, "drum separator")), ("text", s(b, &ch.to_string())), ("font", font_face(b, &c.th.title_font, 800.0)),  ("size", num(size * 0.8 / PX)),
                ("color", color(wheel_ink)), ("at", v2(b, x + sep_w * 0.5, top + h * 0.95)), ("align", id("center")), ("valign", id("bottom")), ("opacity", fade_in(b, appear, 0.2)),
            ]));
            continue;
        };
        // The wheel: ivory, darker at its top and bottom edges.
        let bands = [(0.0, 0.18, 0x5a), (0.18, 0.12, 0x28), (0.70, 0.12, 0x28), (0.82, 0.18, 0x5a)];
        let mut layers = vec![layer(b, "Rect", vec![("name", s(b, "wheel")), ("at", v2(b, x, top)), ("size", v2(b, cell_w, h)), ("radius", num(size * 0.06)), ("fill", color(wheel_ink))])];
        // The digits, turning: the one in the window and the next above it.
        let unit = 10f64.powi(p);
        let steps = (scaled / unit).floor() as usize;
        let target = finals.get(p as usize).copied().unwrap_or((steps % 10) as f64);
        let mut digit = vec![(begin.clone(), num(0.0))];
        let mut next = vec![(begin.clone(), num(1.0))];
        let mut saw = vec![(begin.clone(), num(0.0))];
        if steps <= 60 {
            for j in 1..=steps {
                // Wheel p turns from j−1 to j as the count goes from j·10^p − 1 to j·10^p.
                let (a, b) = (when(j as f64 * unit - 1.0, scaled), when(j as f64 * unit, scaled));
                let (a, b) = (a.min(b - 0.002).max(0.0), b);
                let d = if j == steps { target } else { (j % 10) as f64 };
                digit.push((at_frac(b), num(d)));
                next.push((at_frac(b), num((d + 1.0) % 10.0)));
                saw.push((at_frac(a), num(0.0)));
                saw.push((at_frac(b - 0.0005), num(h)));
                saw.push((at_frac(b), num(0.0)));
            }
        } else {
            // A fast wheel blurs: a new digit every frame or so, settling on its own.
            let frames = 36;
            for f in 1..frames {
                let u = f as f64 / frames as f64;
                // Past f64's exact integers the count's own digits are noise,
                // so a spinning wheel shows a seeded blur instead.
                let v = scaled * (1.0 - (1.0 - u).powi(3));
                let d = if v / unit < 1e15 { ((v / unit).floor() as u64 % 10) as f64 } else { ((f as u64 * 7 + p as u64 * 13).wrapping_mul(2654435761) >> 7) as f64 % 10.0 };
                digit.push((at_frac(u), num(d)));
                next.push((at_frac(u), num((d + 1.0) % 10.0)));
            }
            digit.push((lands.clone(), num(target)));
            next.push((lands.clone(), num((target + 1.0) % 10.0)));
        }
        let face = |role: &str, values: Vec<(ScriptValue, ScriptValue)>, dy: f64| layer(b, "Text", vec![
            ("name", s(b, &format!("{role} · {name} {ch}"))), ("number", keys(b, values, id("hold"))), ("decimals", num(0.0)), ("font", font_face(b, &c.th.title_font, 800.0)), 
            ("size", num(size / PX)), ("color", color(digit_ink)), ("at", v2(b, x + cell_w * 0.5, top + dy)), ("align", id("center")),
        ]);
        let saw_track = if saw.len() > 1 {
            let points = saw.into_iter().enumerate().map(|(k, (t, v))| if k == 0 { list(b, vec![t, v]) } else { list(b, vec![t, v, id(if k % 3 == 2 { "linear" } else { "hold" })]) }).collect();
            layer(b, "Keys", vec![("points", list(b, points))])
        } else {
            num(0.0)
        };
        let incoming = face("roll", next, -h);
        // A growing drum runs out of the frame on purpose: its digits are
        // not checked as labels.
        let role = if grow { "drum" } else { "value" };
        set(b, incoming, "opacity", keys(b, vec![(begin.clone(), num(1.0)), (lands.clone(), num(1.0)), (shift(b, lands, 0.001), num(0.0))], id("linear")));
        let column = layer(b, "Group", vec![("name", s(b, "drum digits")), ("layers", list(b, vec![face(role, digit, 0.0), incoming])), ("y", saw_track)]);
        layers.push(layer(b, "Group", vec![
            ("name", s(b, "wheel window")), ("mask", layer(b, "Rect", vec![("at", v2(b, x, top)), ("size", v2(b, cell_w, h)), ("fill", color(0xffffffff))])), ("layers", list(b, vec![column])),
        ]));
        for (y, hh, a) in bands {
            layers.push(layer(b, "Rect", vec![("name", s(b, "shade")), ("at", v2(b, x, top + h * y)), ("size", v2(b, cell_w, h * hh)), ("fill", color(a))]));
        }
        c.out.push(layer(b, "Group", vec![
            ("name", s(b, &format!("wheel {name} {p}"))), ("layers", list(b, layers)), ("opacity", fade_in(b, appear, 0.15)),
        ]));
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huge_counts_write_out_without_float_noise() {
        assert_eq!(digits(1e30, 0), "1,000,000,000,000,000,000,000,000,000,000");
        assert_eq!(digits(1284000.0, 0), "1,284,000");
        assert_eq!(when(0.0, 10.0), 0.0);
        assert!((when(10.0, 10.0) - 1.0).abs() < 1e-12);
    }
}

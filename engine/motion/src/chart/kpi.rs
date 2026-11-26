//! `@kpi` (a grid of cards: label, a counting number, a change chip and a
//! sparkline drawing on) and `@counter` (one big number and its label).

use super::build::*;
use makepad_script::*;
use super::scale::{text_box, text_width};
use super::Chart;

pub(super) fn build(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let sets = c.sets.clone();
    let rows = sets[0].rows.clone();
    let n = rows.len();
    c.keys = rows.iter().map(|d| d.label.clone()).collect();
    c.header(&[]);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    let (pw, ph) = (px1 - px0, py1 - py0);
    let value_of = |k: usize, i: usize| sets[k].rows.iter().find(|d| d.label == rows[i].label && !d.label.is_empty()).or(sets[k].rows.get(i)).map_or(0.0, |d| d.value);
    let counter = c.kind == "counter";
    let wide = c.w >= c.h * 1.2;
    let cols = if counter { 1 } else if wide { n.min(4) } else { n.min(2) }.max(1);
    let grid_rows = if counter { 1 } else { n.div_ceil(cols) };
    let gap = ls;
    let card_w = (pw - gap * (cols - 1) as f64) / cols as f64;
    let card_h = ((ph - gap * (grid_rows - 1) as f64) / grid_rows as f64).min(if counter { ph } else { card_w * 0.85 });
    let top = py0 + (ph - card_h * grid_rows as f64 - gap * (grid_rows - 1) as f64) * 0.5;
    // Cards scale their type with their size (a dashboard strip of cards
    // in a short box still reads).
    let ls = if counter { ls } else { (card_h * 0.1).min(card_w * 0.085).max(ls) };
    let shown_n = if counter { 1 } else { n };
    for i in 0..shown_n {
        let (col, row) = (i % cols, i / cols);
        let (x, y) = (px0 + col as f64 * (card_w + gap), top + row as f64 * (card_h + gap));
        let t = c.land(i);
        let enter = c.enter(i);
        let hl = c.highlighted(i);
        let rise = tween(b, num(ls), num(0.0), enter.clone(), shift(b, enter, 0.6), id("ease_out_cubic"));
        let values: Vec<f64> = (0..sets.len()).map(|k| value_of(k, i)).collect();
        // A card may carry its own format (`{format: @currency}`).
        let fmt = match &rows[i].format {
            Some((kind, prefix, suffix)) => super::scale::NumFmt {
                kind: kind.clone(),
                prefix: prefix.clone().unwrap_or_default(),
                suffix: suffix.clone().unwrap_or_default(),
                decimals: None,
                ..c.fmt.clone()
            },
            None => c.fmt.clone(),
        };
        let shown = fmt.shown(values.iter().fold(0.0f64, |a, v| a.max(v.abs())), 0.0, &values);
        let times: Vec<ScriptValue> = (0..sets.len()).map(|k| c.set_time(k, i)).collect();
        let d = &rows[i];
        if counter {
            // One number: as large as the box allows, its label under it.
            let widest = values.iter().map(|v| text_width(&shown.text(*v), 100.0, 800.0)).fold(1.0, f64::max);
            let size = (card_w * 0.92 / widest * 100.0).min(card_h * 0.55);
            let cy = y + card_h * 0.42;
            if c.face.as_deref() == Some("drum") {
                // A mechanical odometer: wheels sized to the box, centred (or
                // spilling out of the frame to the left with `overflow: @grow`).
                let grow = c.overflow.as_deref() == Some("grow");
                // Growing, the drum is sized for its first wheels and runs
                // out of the frame as the count climbs.
                let digits = super::drum::digits(values[values.len() - 1], shown.decimals).chars().count().max(1) as f64;
                let digits = if grow { digits.min(9.0) } else { digits };
                let size = (card_w * 0.9 / (digits * 0.85 + 0.6)).min(card_h * 0.5);
                let probe = super::drum::drum_width(values[values.len() - 1], shown.decimals, size);
                let right = if grow { x + card_w * 0.5 + probe.min(card_w) * 0.5 } else { x + card_w * 0.5 + probe * 0.5 };
                let top = y + card_h * 0.42 - size * 0.6;
                super::drum::drum(c, "count", values[values.len() - 1], shown.decimals, right, top, size, t, c.grow.max(1.2), grow);
                if !d.label.is_empty() {
                    let label = c.text("label", &d.label, [x + card_w * 0.5, top + size * 1.21 + size * 0.5], "center", "top", ls * 1.2, 500.0, c.th.muted, shift(b, t, -0.2), Some(card_w * 0.9));
                    c.out.push(label);
                }
                c.mark(0, i, [x + card_w * 0.5, top]);
                continue;
            }
            let number = c.number("value", &values, &times, &shown, v2(b, x + card_w * 0.5, cy), "center", "center", size, 800.0, if hl { c.th.accent } else { c.th.ink }, Some(card_w * 0.95));
            // A clock (`clock: playhead("reel")`): the number reads it.
            if let Some(clock) = &c.clock {
                set(b, number, "number", clock.clone());
                set(b, number, "opacity", fade_in(b, c.start, 0.3));
            }
            set(b, number, "tracking", num(-size * 0.02));
            c.out.push(number);
            let mut caption_y = cy + size * 0.62;
            if !d.label.is_empty() {
                let label = c.text("label", &d.label, [x + card_w * 0.5, caption_y], "center", "top", ls * 1.2, 500.0, c.th.muted, shift(b, t, -0.2), Some(card_w * 0.9));
                c.out.push(label);
                caption_y += ls * 1.9;
            }
            if let Some(delta) = d.delta {
                delta_chip(c, ls, delta, [x + card_w * 0.5, caption_y], true, shift(b, t, 0.15), None);
            }
            c.mark(0, i, [x + card_w * 0.5, cy - size * 0.55]);
            continue;
        }
        let pad = (card_w * 0.08).min(ls * 1.4);
        let card = layer(b, "Rect", vec![
            ("name", s(b, &format!("card {}", d.label))), ("at", v2(b, x, y)), ("size", v2(b, card_w, card_h)), ("radius", num(18.0)), ("fill", color(c.th.card)),
            ("opacity", tween(b, num(0.0), num(1.0), enter.clone(), shift(b, enter, 0.35), id("ease_out_cubic"))), ("y", rise.clone()),
        ]);
        if hl {
            set(b, card, "stroke", color(c.th.accent));
            set(b, card, "width", num(2.0));
        }
        c.out.push(card);
        let label = c.text("label", &d.label, [x + pad, y + pad], "left", "top", ls * 0.9, 500.0, c.th.muted, shift(b, enter, 0.3), Some(card_w - 2.0 * pad));
        set(b, label, "y", rise.clone());
        c.out.push(label);
        let number_y = y + pad + ls * 1.6;
        let has_trend = !d.trend.is_empty();
        let room = card_h - (number_y - y) - pad - if has_trend { card_h * 0.24 } else { 0.0 } - if d.delta.is_some() { ls * 1.6 } else { 0.0 };
        let widest = values.iter().map(|v| text_width(&shown.text(*v), 100.0, 800.0)).fold(1.0, f64::max);
        let size = ((card_w - 2.0 * pad) / widest * 100.0).min(room / 1.25).min(ls * 3.6).max(ls);
        let number = c.number("value", &values, &times, &shown, v2(b, x + pad, number_y), "left", "top", size, 800.0, if hl { c.th.accent } else { c.th.ink }, Some(card_w - 2.0 * pad));
        set(b, number, "y", rise.clone());
        c.out.push(number);
        if let Some(delta) = d.delta {
            delta_chip(c, ls, delta, [x + pad, number_y + size * 1.25 + ls * 0.2], false, shift(b, t, 0.15), Some(rise.clone()));
        }
        if has_trend {
            let (sx, sw, sh) = (x + pad, card_w - 2.0 * pad, card_h * 0.2);
            let sy = y + card_h - pad - sh;
            let (lo, hi) = d.trend.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
            let span = (hi - lo).max(1e-9);
            let pts: Vec<[f64; 2]> = d.trend.iter().enumerate().map(|(k, v)| [sx + sw * k as f64 / (d.trend.len().max(2) - 1) as f64, sy + sh - (v - lo) / span * sh]).collect();
            let rising = d.trend.last() >= d.trend.first();
            let stroke = if hl || c.highlight.is_empty() { if rising { c.th.positive } else { c.th.negative } } else { c.th.muted };
            let line = layer(b, "Path", vec![
                ("name", s(b, &format!("trend {}", d.label))), ("svg", s(b, &path_through(&pts, true))), ("stroke", color(stroke)), ("width", num(3.0)),
                ("cap", id("round")), ("join", id("round")), ("trim_end", tween(b, num(0.0), num(1.0), shift(b, t, -c.grow * 0.6), shift(b, t, 0.8), id("ease_in_out_cubic"))),
            ]);
            set(b, line, "y", rise.clone());
            c.out.push(line);
        }
        c.mark(0, i, [x + card_w * 0.5, y]);
    }
    Ok(())
}

/// `▲ +12%` in the positive colour (or `▼ −3%` in the negative one): a
/// drawn triangle and the change as a percentage.
fn delta_chip(c: &mut Chart, ls: f64, delta: f64, at: [f64; 2], centred: bool, t: ScriptValue, rise: Option<ScriptValue>) {
    let b = c.b;
    let up = delta >= 0.0;
    let ink = if up { c.th.positive } else { c.th.negative };
    let pct = delta * 100.0;
    let text = format!("{}{}%", if up { "+" } else { "−" }, crate::format_number(pct.abs(), if pct.abs() < 10.0 && pct.fract().abs() > 1e-9 { 1 } else { 0 }, Some(",")));
    let size = ls * 0.9;
    let tri = size * 0.62;
    let [tw, th] = text_box(&text, size, 700.0, None, None, 1.0);
    let total = tri + size * 0.35 + tw;
    let x = if centred { at[0] - total * 0.5 } else { at[0] };
    let cy = at[1] + th * 0.5;
    let svg = if up {
        format!("M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2} Z", x, cy + tri * 0.45, x + tri * 0.5, cy - tri * 0.45, x + tri, cy + tri * 0.45)
    } else {
        format!("M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2} Z", x, cy - tri * 0.45, x + tri * 0.5, cy + tri * 0.45, x + tri, cy - tri * 0.45)
    };
    let arrow = layer(b, "Path", vec![("name", s(b, "delta arrow")), ("svg", s(b, &svg)), ("fill", color(ink)), ("opacity", fade_in(b, t, 0.3))]);
    let label = c.text("delta", &text, [x + tri + size * 0.35, at[1]], "left", "top", size, 700.0, ink, t, None);
    if let Some(rise) = rise {
        set(b, arrow, "y", rise.clone());
        set(b, label, "y", rise);
    }
    c.out.push(arrow);
    c.out.push(label);
}

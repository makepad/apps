//! `@timeline`: an axis drawing on, each milestone popping as the axis
//! reaches it, its date and event alternating above and below (or down
//! one side in a portrait frame).

use super::build::*;
use super::scale::{text_box, text_width};
use super::Chart;

pub(super) fn build(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let rows = c.sets[0].rows.clone();
    let n = rows.len();
    c.keys = rows.iter().map(|d| d.label.clone()).collect();
    c.header(&[]);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    let vertical = c.h > c.w * 0.95;
    let proportional = rows.iter().all(|d| d.x.is_some()) && n > 1;
    let fraction = |i: usize| -> f64 {
        if n < 2 {
            return 0.5;
        }
        if proportional {
            let (lo, hi) = rows.iter().fold((f64::MAX, f64::MIN), |(a, b), d| (a.min(d.x.unwrap()), b.max(d.x.unwrap())));
            (rows[i].x.unwrap() - lo) / (hi - lo).max(1e-9)
        } else {
            i as f64 / (n - 1) as f64
        }
    };
    let ease = c.ease("ease_in_out_cubic");
    let date_size = ls * 1.3;
    let text_size = ls * 0.95;
    let (a0, a1, cross, item_w);
    if vertical {
        let date_w = rows.iter().map(|d| text_width(&d.label, date_size, 800.0)).fold(0.0, f64::max).min((px1 - px0) * 0.3);
        cross = px0 + date_w + ls * 1.2;
        a0 = py0 + ls;
        a1 = py1 - ls * 2.5;
        item_w = px1 - cross - ls * 1.2;
    } else {
        let slot = (px1 - px0) / n.max(1) as f64;
        item_w = (slot * 1.8).min((px1 - px0) * 0.32);
        a0 = px0 + (item_w * 0.5).min(slot);
        a1 = px1 - (item_w * 0.5).min(slot);
        cross = py0 + (py1 - py0) * 0.5;
    }
    let at_of = |i: usize| a0 + (a1 - a0) * fraction(i);
    let times: Vec<_> = (0..n).map(|i| c.land(i)).collect();
    // The axis: its head reaches each milestone as it lands.
    let len = a1 - a0 + 2.0 * ls;
    let mut trim = vec![(shift(b, times[0], -c.grow), num(0.0))];
    for (i, t) in times.iter().enumerate() {
        trim.push((t.clone(), num((at_of(i) - a0 + ls) / len)));
    }
    trim.push((shift(b, times[n - 1], c.grow * 0.6), num(1.0)));
    let svg = if vertical {
        format!("M {:.2} {:.2} L {:.2} {:.2}", cross, a0 - ls, cross, a1 + ls)
    } else {
        format!("M {:.2} {:.2} L {:.2} {:.2}", a0 - ls, cross, a1 + ls, cross)
    };
    c.out.push(layer(b, "Path", vec![
        ("name", s(b, "axis")), ("svg", s(b, &svg)), ("stroke", color(c.th.muted)), ("width", num(3.0)), ("cap", id("round")), ("trim_end", keys(b, trim, ease)),
    ]));
    for (i, d) in rows.iter().enumerate() {
        let t = times[i].clone();
        let p = at_of(i);
        let hl = c.highlighted(i);
        let dot = if hl || c.highlight.is_empty() { c.th.accent } else { c.th.ink };
        let r = ls * 0.42;
        let centre = if vertical { [cross, p] } else { [p, cross] };
        c.out.push(layer(b, "Ellipse", vec![
            ("name", s(b, &format!("milestone {}", d.label))), ("at", v2(b, centre[0] - r, centre[1] - r)), ("size", v2(b, 2.0 * r, 2.0 * r)), ("fill", color(dot)),
            ("scale", tween(b, num(0.0), num(1.0), shift(b, t, -0.2), shift(b, t, 0.2), id("ease_out_back"))),
        ]));
        c.out.push(layer(b, "Ellipse", vec![
            ("name", s(b, "pulse")), ("at", v2(b, centre[0] - r, centre[1] - r)), ("size", v2(b, 2.0 * r, 2.0 * r)), ("stroke", color(dot)), ("width", num(2.0)),
            ("scale", tween(b, num(1.0), num(2.4), t.clone(), shift(b, t, 0.6), id("ease_out_cubic"))),
            ("opacity", keys(b, vec![(shift(b, t, -0.01), num(0.0)), (t.clone(), num(0.8)), (shift(b, t, 0.6), num(0.0))], id("ease_out_cubic"))),
        ]));
        let date_ink = if hl || c.highlight.is_empty() { c.th.accent } else { c.th.ink };
        let body = d.text.clone().unwrap_or_default();
        if vertical {
            let date = c.text("label", &d.label, [cross - ls * 1.0, p], "right", "center", date_size, 800.0, date_ink, t, Some(cross - px0 - ls * 1.2));
            c.out.push(date);
            if !body.is_empty() {
                let text = c.text("caption", &body, [cross + ls * 1.0, p - date_size * 0.6], "left", "top", text_size, 500.0, c.th.ink, shift(b, t, 0.1), None);
                set(b, text, "wrap", num(item_w));
                set(b, text, "x", tween(b, num(-ls * 0.6), num(0.0), shift(b, t, -0.1), shift(b, t, 0.4), id("ease_out_cubic")));
                c.out.push(text);
            }
        } else {
            let above = i % 2 == 0;
            let stem = ls * 1.0;
            let sign = if above { -1.0 } else { 1.0 };
            c.out.push(layer(b, "Rect", vec![
                ("name", s(b, "stem")), ("at", v2(b, p - 1.0, if above { cross - r - stem } else { cross + r })), ("size", v2(b, 2.0, stem)), ("fill", color(alpha(c.th.muted, 0xa0))),
                ("opacity", fade_in(b, shift(b, t, 0.1), 0.25)),
            ]));
            let valign = if above { "bottom" } else { "top" };
            let date_y = cross + sign * (r + stem + 4.0);
            let date = c.text("label", &d.label, [p, date_y], "center", valign, date_size, 800.0, date_ink, shift(b, t, 0.05), Some(item_w));
            c.out.push(date);
            if !body.is_empty() {
                let [_, dh] = text_box(&d.label, date_size, 800.0, None, Some(item_w), 1.0);
                let lines = super::scale::wrap_lines(&body, text_size, 500.0, item_w);
                let text = c.text("caption", &lines, [p, date_y + sign * (dh + 4.0)], "center", valign, text_size, 500.0, c.th.ink, shift(b, t, 0.15), Some(item_w));
                set(b, text, "y", tween(b, num(sign * -ls * 0.5), num(0.0), shift(b, t, -0.05), shift(b, t, 0.45), id("ease_out_cubic")));
                c.out.push(text);
            }
        }
        c.mark(0, i, [centre[0], centre[1] - r]);
    }
    Ok(())
}

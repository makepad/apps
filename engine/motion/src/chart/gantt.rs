//! `@gantt`: tasks as bars on a time axis, milestones as diamonds, and a
//! review: a "today" line sweeps across (`review: {from: 0 to: 9 at: 2.0
//! for: 3.0}`) and each task takes its status colour (done, late, risk,
//! planned) as the line passes its end.

use super::build::*;
use makepad_script::*;
use super::scale::{nice_ticks, text_width};
use super::Chart;

pub(super) fn build(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let rows = c.sets[0].rows.clone();
    let n = rows.len();
    c.keys = rows.iter().map(|d| d.label.clone()).collect();
    let span = |d: &super::Datum| {
        let s = d.start.or(d.x).unwrap_or(d.value);
        (s, d.end.unwrap_or(s).max(s))
    };
    let (lo, hi) = rows.iter().map(span).fold((f64::MAX, f64::MIN), |(a, b), (s, e)| (a.min(s), b.max(e)));
    // A review without its bounds sweeps the whole axis once the tasks are in.
    let last = c.land(n.saturating_sub(1));
    let review = c.review.clone().map(|mut r| {
        if r.from.is_nan() {
            r.from = lo;
        }
        if r.to.is_nan() {
            r.to = hi;
        }
        if r.at.as_number().is_some_and(f64::is_nan) {
            r.at = shift(b, last, 0.5);
        }
        r
    });
    let hi = review.as_ref().map_or(hi, |r| hi.max(r.to));
    let ticks = nice_ticks(c.min.unwrap_or(lo), c.max.unwrap_or(hi), 6);
    let (d0, d1) = (ticks[0], *ticks.last().unwrap());
    let status_color = |c: &Chart, status: &str| match status {
        "done" | "shipped" | "ok" => c.th.positive,
        "late" | "slipped" | "blocked" => c.th.negative,
        "risk" | "at_risk" | "at-risk" => 0xffd23fff,
        _ => c.th.neutral,
    };
    c.header(&[]);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    let label_w = (rows.iter().map(|d| text_width(&d.label, ls * 0.85, 600.0)).fold(0.0, f64::max) + ls).min((px1 - px0) * 0.32);
    // A column at the right for the status chips (and room for the last
    // tick's and the "today" tag's half widths).
    let has_status = rows.iter().any(|d| d.status.is_some()) && review.is_some();
    let chip_w = if has_status { text_width("PLANNED", ls * 0.6, 800.0) + ls * 1.2 } else { 0.0 };
    let edge = text_width("TODAY", ls * 0.7, 800.0) * 0.5 + ls * 0.6;
    let (x0, x1) = (px0 + label_w, px1 - chip_w.max(edge));
    // Two bands over the rows: the review's "today" tag, then the ticks.
    let axis_h = if review.is_some() { ls * 2.8 } else { ls * 1.6 };
    let tick_y = py0 + axis_h - ls * 0.8;
    let (y0, y1) = (py0 + axis_h, py1);
    let row_h = (y1 - y0) / n.max(1) as f64;
    let bar_h = (row_h * 0.56).min(ls * 1.4);
    let x_of = |v: f64| x0 + (v - d0) / (d1 - d0).max(1e-9) * (x1 - x0);
    let start = c.start.clone();
    let shown = c.fmt.shown(d1.abs().max(d0.abs()), ticks.get(1).map_or(0.0, |t| t - ticks[0]), &ticks);
    for t in &ticks {
        let x = x_of(*t);
        let rule = c.rule([x - 0.5, y0], [1.0, y1 - y0], c.th.grid, start);
        c.out.push(rule);
        let label = c.text("tick", &shown.text(*t), [x, tick_y], "center", "center", ls * 0.75, 500.0, c.th.muted, start, None);
        c.out.push(label);
    }
    // The review's sweep: the line's x over time, and when it reaches x.
    let sweep = review.as_ref().map(|r| (x_of(r.from), x_of(r.to), r.at.clone(), r.length));
    let reaches = |x: f64| -> Option<ScriptValue> {
        let (xa, xb, at, len) = sweep.clone()?;
        Some(shift(b, at, len * ((x - xa) / (xb - xa).max(1e-9)).clamp(0.0, 1.0)))
    };
    for (i, d) in rows.iter().enumerate() {
        let (s, e) = span(d);
        let y = y0 + row_h * (i as f64 + 0.5);
        let t = c.land(i);
        let hl = c.highlighted(i);
        let label = c.text("label", &d.label, [x0 - ls * 0.5, y], "right", "center", ls * 0.85, if hl { 700.0 } else { 500.0 }, if hl { c.th.ink } else { c.th.muted }, c.enter(i), Some(label_w - ls * 0.6));
        c.out.push(label);
        let status = d.status.clone().unwrap_or_default().to_lowercase();
        let final_fill = if hl { c.th.accent } else if status.is_empty() { c.th.color(0) } else { status_color(c, &status) };
        // Under review, a task shows neutral until the line passes its end.
        let fill = match reaches(x_of(e)) {
            Some(when) => keys(b, vec![(shift(b, when, -0.01), color(c.th.neutral)), (shift(b, when, 0.25), color(final_fill))], id("ease_out_cubic")),
            None => color(final_fill),
        };
        let milestone = (e - s).abs() < 1e-9;
        if milestone {
            let r = bar_h * 0.6;
            let x = x_of(s);
            c.out.push(layer(b, "Path", vec![
                ("name", s_(b, &format!("milestone {}", d.label))), ("svg", s_(b, &format!("M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2} Z", x, y - r, x + r, y, x, y + r, x - r, y))),
                ("fill", fill), ("scale", tween(b, num(0.0), num(1.0), shift(b, t, -0.3), t.clone(), id("ease_out_back"))),
            ]));
            c.mark(0, i, [x, y - r]);
        } else {
            let (xa, xb) = (x_of(s), x_of(e));
            c.out.push(layer(b, "Rect", vec![
                ("name", s_(b, &format!("task {}", d.label))), ("at", v2(b, xa, y - bar_h * 0.5)), ("radius", num(bar_h * 0.3)), ("fill", fill),
                ("size", tween(b, v2(b, 0.0, bar_h), v2(b, xb - xa, bar_h), c.enter(i), t.clone(), c.ease("ease_out_cubic"))),
            ]));
            if let Some(note) = &d.text {
                let ink = c.th.ink_on(final_fill);
                let fits = text_width(note, ls * 0.7, 600.0) < xb - xa - ls * 0.6;
                let (at, align, ink) = if fits { ([xa + ls * 0.3, y], "left", ink) } else { ([xb + ls * 0.4, y], "left", c.th.muted) };
                let label = c.text("caption", note, at, align, "center", ls * 0.7, 600.0, ink, shift(b, t, 0.1), Some((x1 - at[0]).max(ls)));
                c.out.push(label);
            }
            c.mark(0, i, [xb, y - bar_h * 0.5]);
        }
        // The status chip, when the review reaches the task.
        if let (Some(when), false) = (reaches(x_of(e)), status.is_empty()) {
            let chip = status.replace('_', " ").to_uppercase();
            let label = c.text("delta", &chip, [px1 - ls * 0.4, y], "right", "center", ls * 0.6, 800.0, c.th.ink_on(status_color(c, &status)), shift(b, when, 0.3), None);
            set(b, label, "plate", color(status_color(c, &status)));
            set(b, label, "plate_radius", num(ls * 0.3));
            set(b, label, "plate_pad", v2(b, ls * 0.35, ls * 0.12));
            c.out.push(label);
        }
    }
    if let Some((xa, xb, at, len)) = sweep {
        let end = shift(b, at, len);
        c.out.push(layer(b, "Rect", vec![
            ("name", s_(b, "today")), ("at", tween(b, v2(b, xa - 1.5, y0 - ls * 0.3), v2(b, xb - 1.5, y0 - ls * 0.3), at.clone(), end.clone(), id("linear"))),
            ("size", v2(b, 3.0, y1 - y0 + ls * 0.3)), ("fill", color(c.th.accent)), ("opacity", fade_in(b, at, 0.2)),
        ]));
        let today = c.text("label", c.review.as_ref().map_or("TODAY", |r| r.label.as_str()), [0.0, py0 + ls * 0.6], "center", "center", ls * 0.7, 800.0, c.th.ink_on(c.th.accent), at, None);
        set(b, today, "at", tween(b, v2(b, xa, py0 + ls * 0.6), v2(b, xb, py0 + ls * 0.6), at.clone(), end, id("linear")));
        set(b, today, "plate", color(c.th.accent));
        set(b, today, "plate_radius", num(ls * 0.3));
        c.out.push(today);
    }
    Ok(())
}

fn s_(b: &B, text: &str) -> ScriptValue {
    s(b, text)
}

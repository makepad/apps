//! `@axis`: one vertical axis, linear or logarithmic (`y_scale: @log`,
//! the default when the values span more than two decades), with a marker
//! that travels to each value at its `land` time, its label changing on
//! the way ("basically zero" → "uh oh" → "doom"). With `window: 4` the axis
//! shows four decades around the marker and scrolls, its tick labels
//! sliding by, so a climb over 30 orders of magnitude stays readable.

use super::build::*;
use makepad_script::*;
use super::scale::nice_ticks;
use super::Chart;

/// A power of ten as an axis label: "0.001", "1", "1,000", then "1e9".
pub(crate) fn decade_label(e: i32) -> String {
    match e {
        -3..=-1 => format!("{}", 10f64.powi(e)),
        0..=6 => crate::format_number(10f64.powi(e), 0, Some(",")),
        _ => format!("1e{e}"),
    }
}

/// A value up to 3 significant digits, without trailing zeros.
pub(crate) fn plain(v: f64) -> String {
    let digits = (3 - v.abs().max(1e-300).log10().floor() as i32 - 1).clamp(0, 9) as u32;
    let text = crate::format_number(v, digits, Some(","));
    if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_string() } else { text }
}

pub(super) fn build(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let rows = c.sets[0].rows.clone();
    let n = rows.len();
    if n == 0 {
        return Err("an axis needs at least one value for its marker".into());
    }
    c.keys = rows.iter().map(|d| d.label.clone()).collect();
    let values: Vec<f64> = rows.iter().map(|d| d.value).collect();
    let positive = values.iter().all(|v| *v > 0.0);
    let (vmin, vmax) = values.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    let log = c.log_y || (positive && vmax / vmin.max(1e-300) > 100.0);
    if log && !positive {
        return Err("a log axis needs values above 0".into());
    }
    let f = |v: f64| if log { v.max(1e-300).log10() } else { v };
    let (lo, hi) = if log {
        (f(c.min.unwrap_or(vmin)).floor(), f(c.max.unwrap_or(vmax)).ceil().max(f(vmin).floor() + 1.0))
    } else {
        let t = nice_ticks(c.min.unwrap_or(vmin.min(0.0)), c.max.unwrap_or(vmax), 5);
        (t[0], *t.last().unwrap())
    };
    let ticks: Vec<f64> = if log { (lo as i32..=hi as i32).map(|e| e as f64).collect() } else { nice_ticks(lo, hi, 5) };
    let shown = c.fmt.shown(vmax.abs().max(vmin.abs()), 0.0, &values);
    c.header(&[]);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    let (y0, y1) = (py0 + ls, py1 - ls);
    let ax = px0 + (px1 - px0) * 0.3;
    // The visible range while the marker sits at value k (scrolling window).
    let window = c.window;
    let range = |k: Option<usize>| -> (f64, f64) {
        match k {
            Some(k) if window > 0.0 && window < hi - lo => {
                let mid = f(values[k]).clamp(lo + window * 0.5, hi - window * 0.5);
                (mid - window * 0.5, mid + window * 0.5)
            }
            _ if window > 0.0 && window < hi - lo => (lo, lo + window),
            _ => (lo, hi),
        }
    };
    let y_at = |x: f64, (a, b): (f64, f64)| y1 - (x - a) / (b - a).max(1e-9) * (y1 - y0);
    let times: Vec<ScriptValue> = (0..n).map(|i| c.land(i)).collect();
    let ease = c.ease("ease_in_out_cubic");
    // A value per step: the first before the marker moves, then each at its time.
    let track = |g: &dyn Fn(Option<usize>) -> ScriptValue| -> ScriptValue {
        let mut points = vec![(shift(b, times[0], -c.grow), g(None))];
        for k in 0..n {
            if k > 0 {
                points.push((shift(b, times[k], -c.grow), g(Some(k - 1))));
            }
            points.push((times[k].clone(), g(Some(k))));
        }
        keys(b, points, ease)
    };
    let start = c.start.clone();
    c.out.push(c.rule([ax - 1.5, y0], [3.0, y1 - y0], c.th.ink, start));
    for &tv in &ticks {
        let inside = |k: Option<usize>| { let r = range(k); tv >= r.0 - 1e-9 && tv <= r.1 + 1e-9 };
        let y = track(&|k| num(y_at(tv, range(k))));
        let opacity = track(&|k| num(if inside(k) { 1.0 } else { 0.0 }));
        let label = if log { decade_label(tv as i32) } else { shown.text(tv) };
        let tick = layer(b, "Rect", vec![("name", s(b, "tick")), ("at", v2(b, ax - ls * 0.5, -1.0)), ("size", v2(b, ls * 0.5, 2.0)), ("fill", color(c.th.ink))]);
        set(b, tick, "y", y.clone());
        set(b, tick, "opacity", opacity.clone());
        c.out.push(tick);
        let text = c.text("tick", &label, [ax - ls * 0.8, 0.0], "right", "center", ls * 0.8, 500.0, c.th.muted, start, Some(ax - px0 - ls));
        set(b, text, "y", y);
        set(b, text, "opacity", opacity);
        c.out.push(text);
        if log && tv < hi {
            // Minor ticks at 2..9 within the decade.
            for m in 2..10 {
                let mv = tv + (m as f64).log10();
                let inside = |k: Option<usize>| { let r = range(k); mv >= r.0 && mv <= r.1 };
                let minor = layer(b, "Rect", vec![("name", s(b, "minor tick")), ("at", v2(b, ax - ls * 0.25, -0.5)), ("size", v2(b, ls * 0.25, 1.0)), ("fill", color(alpha(c.th.ink, 0x80)))]);
                set(b, minor, "y", track(&|k| num(y_at(mv, range(k)))));
                set(b, minor, "opacity", track(&|k| num(if inside(k) { 1.0 } else { 0.0 })));
                c.out.push(minor);
            }
        }
    }
    // The marker: a pointer on the axis, the value beside it, and the
    // step's label in a bubble.
    let marker_y = track(&|k| num(y_at(f(values[k.unwrap_or(0)]), range(k))));
    let r = ls * 0.6;
    let pointer = layer(b, "Path", vec![
        ("name", s(b, "marker")), ("svg", s(b, &format!("M {:.2} 0 L {:.2} {:.2} L {:.2} {:.2} Z", ax + 3.0, ax + 3.0 + r * 1.6, -r, ax + 3.0 + r * 1.6, r))),
        ("fill", color(c.th.accent)), ("opacity", fade_in(b, times[0], 0.3)),
    ]);
    set(b, pointer, "y", marker_y.clone());
    c.out.push(pointer);
    let readout_x = ax + r * 2.4;
    // The readout: counting on a linear axis; on a log axis each step's
    // value is written out (1e-9, 0.00001, 0.99) and swaps as it lands.
    if !log {
        let value = c.number("value", &values, &times, &shown, v2(b, readout_x, 0.0), "left", "bottom", ls * 1.5, 800.0, c.th.accent, Some(px1 - readout_x));
        set(b, value, "y", marker_y.clone());
        c.out.push(value);
    } else {
        for k in 0..n {
            let text = if values[k] < 1e-3 { format!("{:e}", values[k]) } else { super::axis::plain(values[k]) };
            let mut points = vec![(shift(b, times[k], -0.05), num(0.0)), (times[k].clone(), num(1.0))];
            if k + 1 < n {
                points.push((shift(b, times[k + 1], -c.grow), num(1.0)));
                points.push((shift(b, times[k + 1], -c.grow + 0.05), num(0.0)));
            }
            let value = c.text("value", &text, [readout_x, 0.0], "left", "bottom", ls * 1.5, 800.0, c.th.accent, times[k], Some(px1 - readout_x));
            set(b, value, "y", marker_y.clone());
            set(b, value, "opacity", keys(b, points, id("linear")));
            c.out.push(value);
        }
    }
    for (k, d) in rows.iter().enumerate() {
        if d.label.is_empty() {
            continue;
        }
        let mut points = vec![(shift(b, times[k], -0.05), num(0.0)), (shift(b, times[k], 0.2), num(1.0))];
        if k + 1 < n {
            points.push((shift(b, times[k + 1], -c.grow), num(1.0)));
            points.push((shift(b, times[k + 1], -c.grow + 0.2), num(0.0)));
        }
        let label = c.text("label", &d.label, [readout_x, 0.0], "left", "top", ls * 0.9, 600.0, c.th.ink, times[k], Some(px1 - readout_x));
        set(b, label, "y", marker_y.clone());
        set(b, label, "opacity", keys(b, points, id("linear")));
        c.out.push(label);
        let yk = y_at(f(values[k]), range(Some(k)));
        c.mark(0, k, [ax, yk]);
    }
    Ok(())
}

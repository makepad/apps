//! `@meter`: a running device whose value carries across the edit — a
//! thermometer (`style: @thermometer`, the default), a gauge (`@gauge`) or
//! a progress bar (`@bar`). Its values are keyed at story beats (`data:
//! [9, 10, 35, 64, 99]` with `land: [section(@chorus), …]`) and hold
//! between them; time references resolve on the edit's clock, so every
//! shot that shows the meter shows the same value. `clock: progress("reel")`
//! (or `playhead(…)`) drives it from a clip's time instead.

use super::build::*;
use makepad_script::*;
use super::scale::PX;
use super::Chart;

/// A track's numbers mapped through `f` (a number, or a `tween` / `keys`
/// of numbers, as `playhead(…)` / `progress(…)` bind to).
pub(crate) fn map_track(b: &B, v: ScriptValue, f: &dyn Fn(f64) -> ScriptValue) -> ScriptValue {
    if let Some(n) = number_of(b, v) {
        return f(n);
    }
    let Some(o) = v.as_object() else { return v };
    if items_of(b, v).is_some() {
        return v;
    }
    let own: Vec<(String, ScriptValue)> = b.with(|vm| crate::obj::fields(vm, o).into_iter().filter(|(_, _, level)| *level == o).map(|(k, x, _)| (k, x)).collect());
    let mut mapped: Vec<(String, ScriptValue)> = Vec::new();
    for (key, value) in own {
        match key.as_str() {
            "from" | "to" => mapped.push((key, map_track(b, value, f))),
            "points" => {
                if let Some(points) = items_of(b, value) {
                    let rebuilt: Vec<ScriptValue> = points
                        .into_iter()
                        .map(|point| match items_of(b, point) {
                            Some(mut p) if p.len() >= 2 => {
                                p[1] = map_track(b, p[1], f);
                                list(b, p)
                            }
                            _ => point,
                        })
                        .collect();
                    mapped.push((key, list(b, rebuilt)));
                }
            }
            _ => {}
        }
    }
    if mapped.is_empty() {
        return v;
    }
    b.with(|vm| {
        let copy = crate::obj::copy_without(vm, o, &[]);
        for (k, x) in &mapped {
            crate::obj::set(vm, copy, k, *x);
        }
        copy.into()
    })
}

/// The meter's colour at `f` (0..1 of `max`) along its stops.
fn ramp(stops: &[u32], f: f64) -> u32 {
    if stops.len() < 2 {
        return stops.first().copied().unwrap_or(0xffffffff);
    }
    let x = f.clamp(0.0, 1.0) * (stops.len() - 1) as f64;
    let i = (x.floor() as usize).min(stops.len() - 2);
    mix(stops[i], stops[i + 1], x - i as f64)
}

pub(super) fn build(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let rows = c.sets[0].rows.clone();
    let values: Vec<f64> = rows.iter().map(|d| d.value).collect();
    c.keys = rows.iter().map(|d| d.label.clone()).collect();
    let max = c.max.unwrap_or_else(|| if c.fmt.kind == "percent" { 100.0 } else { values.iter().copied().fold(1e-9, f64::max) });
    let stops: Vec<u32> = if c.colors_given { c.th.series.clone() } else { vec![c.th.positive, 0xffd23fff, c.th.negative] };
    let style = c.shape.clone().unwrap_or_else(|| "thermometer".into());
    let badge = c.title.take();
    c.header(&[]);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    let ease = c.ease("ease_in_out_cubic");
    // The value through the beats: 0, then each value at its time, held.
    let times: Vec<ScriptValue> = (0..values.len()).map(|i| c.land(i)).collect();
    let clock = c.clock.clone();
    let track = |f: &dyn Fn(f64) -> ScriptValue| -> ScriptValue {
        if let Some(clock) = &clock {
            return map_track(b, *clock, f);
        }
        let mut points = vec![(shift(b, times[0], -c.grow), f(0.0))];
        for (k, v) in values.iter().enumerate() {
            if k > 0 {
                points.push((shift(b, times[k], -c.grow), f(values[k - 1])));
            }
            points.push((times[k].clone(), f(*v)));
        }
        keys(b, points, ease)
    };
    let shown = c.fmt.shown(max, 0.0, &values);
    // The readout: counts through the values (or reads the clock).
    let readout = |c: &Chart, at: ScriptValue, align: &str, valign: &str, size: f64, ink: u32| -> ScriptValue {
        match &c.clock {
            Some(clock) => layer(b, "Text", vec![
                ("name", s(b, &format!("value · {}", shown.text(max)))), ("number", clock.clone()), ("decimals", num(shown.decimals as f64)), ("prefix", s(b, &shown.prefix)),
                ("suffix", s(b, &shown.suffix)), ("format", id(shown.time.as_deref().unwrap_or("plain"))), ("total", shown.total.map_or(NIL, num)), ("font", font_face(b, &c.th.font, 800.0)),  ("size", num(size / PX)), ("color", color(ink)), ("at", at),
                ("align", id(align)), ("valign", id(valign)), ("tabular", boolean(true)),
            ]),
            None => c.number("value", &values, &times, &shown, at, align, valign, size, 800.0, ink, None),
        }
    };
    let fill_color = track(&|v| color(ramp(&stops, v / max)));
    let start = c.start.clone();
    match style.as_str() {
        "gauge" => {
            let r = ((px1 - px0) * 0.5).min((py1 - py0) * 0.8) * 0.9;
            let (cx, cy) = ((px0 + px1) * 0.5, py0 + (py1 - py0) * 0.5 + r * 0.45);
            let k = r * 0.5523;
            // Left, over the top, to the right: two quarter arcs.
            let arc = format!("M {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
                cx - r, cy, cx - r, cy - k, cx - k, cy - r, cx, cy - r, cx + k, cy - r, cx + r, cy - k, cx + r, cy);
            let thick = r * 0.16;
            c.out.push(layer(b, "Path", vec![("name", s(b, "gauge track")), ("svg", s(b, &arc)), ("stroke", color(c.th.grid | 0x30)), ("width", num(thick)), ("cap", id("round")), ("opacity", fade_in(b, start, 0.4))]));
            let fraction = track(&|v| num((v / max).clamp(0.0, 1.0)));
            c.out.push(layer(b, "Path", vec![("name", s(b, "gauge value")), ("svg", s(b, &arc)), ("stroke", fill_color.clone()), ("width", num(thick)), ("cap", id("round")), ("trim_end", fraction)]));
            let needle = r * 0.78;
            let angle = track(&|v| num(-90.0 + 180.0 * (v / max).clamp(0.0, 1.0)));
            c.out.push(layer(b, "Rect", vec![
                ("name", s(b, "needle")), ("at", v2(b, cx - 3.0, cy - needle)), ("size", v2(b, 6.0, needle)), ("radius", num(3.0)), ("fill", color(c.th.ink)), ("anchor", v2(b, 0.5, 1.0)), ("rotate", angle),
            ]));
            c.out.push(layer(b, "Ellipse", vec![("name", s(b, "hub")), ("at", v2(b, cx - ls * 0.5, cy - ls * 0.5)), ("size", v2(b, ls, ls)), ("fill", color(c.th.ink))]));
            let value = readout(c, v2(b, cx, cy + ls * 0.9), "center", "top", ls * 2.0, c.th.ink);
            c.out.push(value);
            if let Some(badge) = &badge {
                let label = c.text("title", badge, [cx, cy - r - thick - ls * 0.4], "center", "bottom", ls * 1.1, 800.0, c.th.ink, start, Some(r * 2.0));
                c.out.push(label);
            }
            c.mark(0, 0, [cx, cy - r]);
        }
        "bar" => {
            // Type scales with the box's height (a meta band is short).
            let ls = ls.max((py1 - py0) * 0.2);
            let h = (ls * 0.6).max(8.0);
            let y = (py0 + py1) * 0.5 - h * 0.5;
            let w = px1 - px0;
            c.out.push(layer(b, "Rect", vec![("name", s(b, "meter track")), ("at", v2(b, px0, y)), ("size", v2(b, w, h)), ("radius", num(h * 0.5)), ("fill", color(c.th.grid | 0x30))]));
            let size = track(&|v| v2(b, w * (v / max).clamp(0.0, 1.0), h));
            c.out.push(layer(b, "Rect", vec![("name", s(b, "meter fill")), ("at", v2(b, px0, y)), ("size", size), ("radius", num(h * 0.5)), ("fill", fill_color)]));
            let value = readout(c, v2(b, px1, y - ls * 0.3), "right", "bottom", ls * 1.1, c.th.ink);
            c.out.push(value);
            if let Some(badge) = &badge {
                let label = c.text("label", badge, [px0, y - ls * 0.3], "left", "bottom", ls * 0.9, 700.0, c.th.muted, start, Some(w * 0.6));
                c.out.push(label);
            }
            c.mark(0, 0, [px1, y]);
        }
        _ => {
            // Thermometer: a tube over a bulb; the fill rises, its colour
            // runs green → yellow → red; the value rides in a disc.
            let ph = py1 - py0;
            let badge_h = if badge.is_some() { ls * 2.0 } else { 0.0 };
            let tube_w = ((px1 - px0) * 0.3).min(ph * 0.12);
            let bulb = tube_w * 1.9;
            let cx = px0 + (px1 - px0) * 0.35;
            let (top, bottom) = (py0 + badge_h + tube_w * 0.5, py1 - bulb);
            let inner = tube_w * 0.62;
            // The badge and the value disc scale with the tube.
            let ls = ls.max(tube_w * 0.45);
            let span = bottom - top;
            c.out.push(layer(b, "Rect", vec![
                ("name", s(b, "tube")), ("at", v2(b, cx - tube_w * 0.5, top - tube_w * 0.5)), ("size", v2(b, tube_w, span + tube_w)), ("radius", num(tube_w * 0.5)),
                ("fill", color(c.th.card)), ("stroke", color(alpha(c.th.ink, 0xc0))), ("width", num(3.0)), ("opacity", fade_in(b, start, 0.4)),
            ]));
            c.out.push(layer(b, "Ellipse", vec![
                ("name", s(b, "bulb")), ("at", v2(b, cx - bulb * 0.5, bottom - bulb * 0.15)), ("size", v2(b, bulb, bulb)), ("fill", fill_color.clone()), ("stroke", color(alpha(c.th.ink, 0xc0))),
                ("width", num(3.0)), ("opacity", fade_in(b, start, 0.4)),
            ]));
            for k in 0..=10 {
                let y = bottom - span * k as f64 / 10.0;
                let long = k % 5 == 0;
                c.out.push(layer(b, "Rect", vec![("name", s(b, "tick")), ("at", v2(b, cx + tube_w * 0.5 + 6.0, y - 1.0)), ("size", v2(b, if long { tube_w * 0.5 } else { tube_w * 0.3 }, 2.0)), ("fill", color(alpha(c.th.ink, 0x80))), ("opacity", fade_in(b, start, 0.4))]));
            }
            let level = |v: f64| bottom + tube_w * 0.3 - (span + tube_w * 0.3) * (v / max).clamp(0.0, 1.0);
            let fill_at = track(&|v| num(level(v)));
            let size = track(&|v| v2(b, inner, bottom + tube_w * 0.3 - level(v)));
            let fill = layer(b, "Rect", vec![("name", s(b, "fill")), ("at", v2(b, cx - inner * 0.5, 0.0)), ("size", size), ("radius", num(inner * 0.5)), ("fill", fill_color.clone())]);
            set(b, fill, "y", fill_at.clone());
            c.out.push(fill);
            // The disc with the value, beside the fill's top.
            let disc = ls * 2.6;
            let disc_layer = layer(b, "Ellipse", vec![("name", s(b, "value disc")), ("at", v2(b, cx + tube_w * 0.5 + tube_w * 0.6, -disc * 0.5)), ("size", v2(b, disc, disc)), ("fill", fill_color)]);
            set(b, disc_layer, "y", fill_at.clone());
            c.out.push(disc_layer);
            let value = readout(c, v2(b, cx + tube_w * 1.1 + disc * 0.5, 0.0), "center", "center", ls * 0.95, 0x111318ff);
            set(b, value, "y", fill_at);
            set(b, value, "fit", num(disc * 0.85));
            c.out.push(value);
            if let Some(badge) = &badge {
                let label = c.text("title", badge, [cx, py0], "center", "top", ls * 0.9, 800.0, 0x111318ff, start, Some((px1 - px0) * 0.9));
                set(b, label, "plate", color(c.th.ink));
                set(b, label, "plate_radius", num(ls));
                c.out.push(label);
            }
            c.mark(0, 0, [cx, top]);
        }
    }
    c.title = badge;
    Ok(())
}

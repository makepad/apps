//! `@line` / `@area`: one or more series drawing on (to each point's
//! `land` time when given, so the line reaches a value as it is spoken),
//! a head label counting along, direct end labels, and morphs between
//! datasets.

use super::build::*;
use makepad_script::*;
use super::scale::{inverse_in_out_sine, nice_ticks, spread, text_width};
use super::{Chart, Land, Table};

pub(super) fn build(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let sets = c.sets.clone();
    let n = sets[0].rows.len();
    if n < 2 {
        return Err("a line needs at least two points".into());
    }
    for (k, t) in sets.iter().enumerate().skip(1) {
        if t.rows.len() != n {
            return Err(format!("dataset {} has {} points and the first has {n}; datasets morph point by point, so give them the same count", k + 1, t.rows.len()));
        }
    }
    let m = sets.iter().map(Table::series_count).max().unwrap_or(1);
    let area = c.kind == "area";
    let val = |k: usize, i: usize, s: usize| sets[k].rows[i].values.get(s).copied().unwrap_or(if s == 0 { sets[k].rows[i].value } else { 0.0 });
    let all: Vec<f64> = (0..sets.len()).flat_map(|k| (0..n).flat_map(move |i| (0..m).map(move |s| (k, i, s)))).map(|(k, i, s)| val(k, i, s)).collect();
    let (mut lo, mut hi) = all.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    if area || c.min == Some(0.0) {
        lo = lo.min(0.0);
    }
    if let Some(v) = c.min {
        lo = v;
    }
    if let Some(v) = c.max {
        hi = v;
    }
    // `y_scale: @log`: decades as ticks, positions by log10.
    let log = c.log_y;
    if log && lo <= 0.0 {
        return Err("`y_scale: @log` needs every value above 0".into());
    }
    let ticks = if log {
        (lo.log10().floor() as i32..=hi.log10().ceil().max(lo.log10().floor() + 1.0) as i32).map(|e| 10f64.powi(e)).collect::<Vec<f64>>()
    } else {
        nice_ticks(lo, hi.max(lo + 1e-9), 4)
    };
    let (dlo, dhi) = (ticks[0], *ticks.last().unwrap());
    let tick_text = |shown: &super::scale::Shown, t: f64| if log { super::axis::decade_label(t.log10().round() as i32) } else { shown.text(t) };
    let max_abs = all.iter().fold(0.0f64, |a, v| a.max(v.abs()));
    let shown = c.fmt.shown(max_abs, 0.0, &all);
    let tick_shown = c.fmt.shown(max_abs, ticks.get(1).map_or(0.0, |t| t - ticks[0]), &ticks);
    let numeric_x = sets[0].rows.iter().all(|d| d.x.is_some());
    let xs: Vec<f64> = (0..n).map(|i| if numeric_x { sets[0].rows[i].x.unwrap() } else { i as f64 }).collect();
    let (xmin, xmax) = (xs.iter().copied().fold(f64::MAX, f64::min), xs.iter().copied().fold(f64::MIN, f64::max).max(xs[0] + 1e-9));
    let names: Vec<String> = (0..m).map(|s| sets[0].series.get(s).cloned().unwrap_or_else(|| format!("Series {}", s + 1))).collect();
    c.keys = sets[0].rows.iter().map(|d| d.label.clone()).collect();
    let series_color = |c: &Chart, s: usize| if m == 1 && !c.highlight.is_empty() { c.th.accent } else { c.th.color(s) };
    let legend: Vec<(String, u32)> = if c.legend == Some(true) { (0..m).map(|s| (names[s].clone(), series_color(c, s))).collect() } else { Vec::new() };
    c.header(&legend);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    let axes = c.axes.unwrap_or(true);
    let grid = c.grid.unwrap_or(true);
    let labels = c.labels.unwrap_or(true);
    let tick_w = if axes { ticks.iter().map(|t| text_width(&tick_text(&tick_shown, *t), ls * 0.8, 400.0)).fold(0.0, f64::max) + ls * 0.7 } else { 0.0 };
    let end_w = if m > 1 {
        names.iter().map(|nm| text_width(nm, ls * 0.85, 600.0)).fold(0.0, f64::max) + ls * 0.8
    } else if c.values {
        all.iter().map(|v| text_width(&shown.text(*v), ls * 1.1, 700.0)).fold(0.0, f64::max) * 0.5 + ls * 0.3
    } else {
        ls * 0.5
    };
    let headroom = if c.notes.is_empty() { 0.0 } else { ls * 3.0 };
    let (x0, x1) = (px0 + tick_w, px1 - end_w);
    let (y0, y1) = (py0 + headroom + if m == 1 && c.values { ls * 2.0 } else { ls * 0.5 }, py1 - if labels { ls * 1.9 } else { 0.0 });
    if x1 - x0 < 10.0 || y1 - y0 < 10.0 {
        return Err(format!("the chart's box ({:.0}x{:.0}) is too small for its labels; make `size` larger or `label_size` smaller", c.w, c.h));
    }
    let y_of = |v: f64| if log {
        y1 - (v.max(1e-300).log10() - dlo.log10()) / (dhi.log10() - dlo.log10()).max(1e-9) * (y1 - y0)
    } else {
        y1 - (v - dlo) / (dhi - dlo) * (y1 - y0)
    };
    let pt = |k: usize, i: usize, s: usize| [x0 + (xs[i] - xmin) / (xmax - xmin) * (x1 - x0), y_of(val(k, i, s))];
    let start = c.start.clone();
    if grid || axes {
        for t in &ticks {
            let y = y_of(*t);
            if grid {
                let rule = c.rule([x0, y - 0.5], [x1 - x0, 1.0], if t.abs() < 1e-12 { c.th.muted } else { c.th.grid }, start);
                c.out.push(rule);
            }
            if axes {
                let label = c.text("tick", &tick_text(&tick_shown, *t), [x0 - ls * 0.5, y], "right", "center", ls * 0.8, 400.0, c.th.muted, start, None);
                c.out.push(label);
            }
        }
    }
    // The draw-on: each point's time. Without `land` the whole line draws
    // on one in-out curve; with it, the head reaches point i at land(i).
    let lengths: Vec<f64> = {
        let p: Vec<[f64; 2]> = (0..n).map(|i| pt(0, i, 0)).collect();
        let mut acc = vec![0.0];
        for i in 1..n {
            let d = ((p[i][0] - p[i - 1][0]).powi(2) + (p[i][1] - p[i - 1][1]).powi(2)).sqrt();
            acc.push(acc[i - 1] + d);
        }
        let total = acc[n - 1].max(1e-9);
        acc.iter().map(|a| a / total).collect()
    };
    let auto = matches!(c.land, Land::Auto);
    let duration = (c.stagger * n as f64).clamp(1.0, 4.0);
    let point_time = |c: &Chart, i: usize, s: usize| -> ScriptValue {
        let lag = 0.25 * s as f64;
        if auto { shift(b, c.start, lag + duration * inverse_in_out_sine(lengths[i])) } else { shift(b, c.land(i), lag) }
    };
    let seg_ease = c.ease("ease_in_out_cubic");
    let morph_ease = c.ease("ease_in_out_cubic");
    // A value per dataset, keyed: held until each switch, moved over `grow`.
    let morph = |c: &Chart, first_at: ScriptValue, f: &dyn Fn(usize) -> ScriptValue| -> ScriptValue {
        if sets.len() == 1 {
            return f(0);
        }
        let mut points = vec![(first_at, f(0))];
        for k in 1..sets.len() {
            points.push((c.switch[k - 1].clone(), f(k - 1)));
            points.push((shift(b, c.switch[k - 1], c.grow), f(k)));
        }
        keys(b, points, morph_ease)
    };
    let dots = c.dots.unwrap_or(n <= 24);
    let mut ends: Vec<Vec<f64>> = vec![Vec::new(); sets.len()];
    for s in 0..m {
        let stroke = series_color(c, s);
        let end_t = point_time(c, n - 1, s);
        let line_of = |k: usize| path_through(&(0..n).map(|i| pt(k, i, s)).collect::<Vec<_>>(), c.smooth);
        if area {
            let area_of = |k: usize| format!("{} L {:.2} {:.2} L {:.2} {:.2} Z", line_of(k), pt(k, n - 1, s)[0], y_of(dlo.max(0.0).min(dhi)), pt(k, 0, s)[0], y_of(dlo.max(0.0).min(dhi)));
            let svg = morph(c, end_t.clone(), &|k| s_(b, &area_of(k)));
            let fill = alpha(stroke, if m > 1 { 0x38 } else { 0x55 });
            let shape = layer(b, "Path", vec![("name", s_(b, &format!("area {}", names[s]))), ("svg", svg), ("fill", color(fill))]);
            // The fill follows the line's head: a mask widening point by point.
            let reach = |i: usize| v2(b, pt(0, i, s)[0] - x0 + c.thickness * 2.0, c.h + 4.0);
            let mut widths = vec![(shift(b, point_time(c, 0, s), -0.01), v2(b, 0.0, c.h + 4.0))];
            for i in 0..n {
                widths.push((point_time(c, i, s), reach(i)));
            }
            let mask = layer(b, "Rect", vec![("at", v2(b, x0 - c.thickness * 2.0, -2.0)), ("size", keys(b, widths, if auto { id("linear") } else { seg_ease.clone() })), ("fill", color(0xffffffff))]);
            c.out.push(layer(b, "Group", vec![("name", s_(b, &format!("area reveal {}", names[s]))), ("mask", mask), ("layers", list(b, vec![shape]))]));
        }
        let trim = if auto {
            tween(b, num(0.0), num(1.0), shift(b, c.start, 0.25 * s as f64), shift(b, c.start, 0.25 * s as f64 + duration), id("ease_in_out_sine"))
        } else {
            let mut points = vec![(point_time(c, 0, s), num(0.0))];
            for i in 1..n {
                points.push((point_time(c, i, s), num(lengths[i])));
            }
            keys(b, points, seg_ease)
        };
        let svg = morph(c, end_t.clone(), &|k| s_(b, &line_of(k)));
        c.out.push(layer(b, "Path", vec![
            ("name", s_(b, &format!("line {}", names[s]))), ("svg", svg), ("stroke", color(stroke)), ("width", num(c.thickness)), ("cap", id("round")), ("join", id("round")),
            ("trim_end", trim),
        ]));
        if dots {
            let r = c.thickness * 1.5;
            for i in 0..n {
                let t = point_time(c, i, s);
                let at = morph(c, end_t.clone(), &|k| { let p = pt(k, i, s); v2(b, p[0] - r, p[1] - r) });
                c.out.push(layer(b, "Ellipse", vec![
                    ("name", s_(b, &format!("dot {} {}", names[s], i))), ("at", at), ("size", v2(b, 2.0 * r, 2.0 * r)), ("fill", color(stroke)),
                    ("scale", tween(b, num(0.0), num(1.0), shift(b, t, -0.05), shift(b, t, 0.3), id("ease_out_back"))),
                ]));
            }
        }
        for (k, e) in ends.iter_mut().enumerate() {
            e.push(pt(k, n - 1, s)[1]);
        }
        // The head's readout: rides the tip, counting to each point's value.
        if m == 1 && c.values {
            let lift = ls * 0.7;
            let mut at_points = Vec::new();
            let mut number_points = Vec::new();
            for i in 0..n {
                let p = pt(0, i, s);
                at_points.push((point_time(c, i, s), v2(b, p[0], p[1] - lift)));
                number_points.push((point_time(c, i, s), num(val(0, i, s) / shown.div)));
            }
            for k in 1..sets.len() {
                let (pa, pb) = (pt(k - 1, n - 1, s), pt(k, n - 1, s));
                at_points.push((c.switch[k - 1].clone(), v2(b, pa[0], pa[1] - lift)));
                at_points.push((shift(b, c.switch[k - 1], c.grow), v2(b, pb[0], pb[1] - lift)));
                number_points.push((c.switch[k - 1].clone(), num(val(k - 1, n - 1, s) / shown.div)));
                number_points.push((shift(b, c.switch[k - 1], c.grow), num(val(k, n - 1, s) / shown.div)));
            }
            // Linear between points: the tip moves along straight segments.
            let path_ease = if auto { id("linear") } else { seg_ease.clone() };
            let t0 = point_time(c, 0, s);
            c.out.push(layer(b, "Text", vec![
                ("name", s_(b, &format!("value · {}", shown.text(val(sets.len() - 1, n - 1, s))))), ("number", keys(b, number_points, path_ease)),
                ("decimals", num(shown.decimals as f64)), ("grouping", boolean(true)), ("prefix", s_(b, &shown.prefix)), ("suffix", s_(b, &shown.suffix)),
                ("tabular", boolean(true)), ("font", font_face(b, &c.th.font, 700.0)),  ("size", num(ls * 1.1 / super::scale::PX)), ("color", color(c.th.ink)),
                ("at", keys(b, at_points, path_ease)), ("align", id("center")), ("valign", id("bottom")),
                ("opacity", tween(b, num(0.0), num(1.0), t0.clone(), shift(b, t0, 0.3), id("ease_out_cubic"))),
            ]));
        }
    }
    // A rider (a character, an icon) on the first series' head: placed by
    // arc length along the line (`along`), so it stays on the stroke,
    // turned to the tangent with `rider_rotate`.
    if let Some((rider, rotate, offset)) = c.rider.clone() {
        let pts: Vec<[f64; 2]> = (0..n).map(|i| pt(0, i, 0)).collect();
        let svg = path_through(&pts, c.smooth);
        let progress = if auto {
            tween(b, num(0.0), num(1.0), c.start.clone(), shift(b, c.start, duration), id("ease_in_out_sine"))
        } else {
            let mut points = vec![(point_time(c, 0, 0), num(0.0))];
            for i in 1..n {
                points.push((point_time(c, i, 0), num(lengths[i])));
            }
            keys(b, points, seg_ease)
        };
        let node = rider;
        set(b, node, "at", layer(b, "Along", vec![("path", s(b, &svg)), ("progress", progress)]));
        if field_of(b, node, "name").is_none() {
            set(b, node, "name", s(b, "rider"));
        }
        if offset != [0.0, 0.0] {
            set(b, node, "x", num(offset[0]));
            set(b, node, "y", num(offset[1]));
        }
        if rotate {
            let angle = |i: usize| {
                let (a, b) = (pts[i.min(n - 2)], pts[(i + 1).min(n - 1)]);
                (b[1] - a[1]).atan2(b[0] - a[0]).to_degrees()
            };
            let mut turns = vec![(point_time(c, 0, 0), num(angle(0)))];
            for i in 1..n - 1 {
                let t = point_time(c, i, 0);
                turns.push((shift(b, t, -0.08), num(angle(i - 1))));
                turns.push((shift(b, t, 0.08), num(angle(i))));
            }
            set(b, node, "rotate", keys(b, turns, id("ease_in_out_sine")));
        }
        c.out.push(node);
    }
    // Direct labels at the lines' ends, spread so they never collide.
    if m > 1 {
        let placed = spread(&ends[0], ls * 1.25, y0, y1);
        for s in 0..m {
            let at = morph(c, point_time(c, n - 1, s), &|k| { let p = spread(&ends[k], ls * 1.25, y0, y1); v2(b, x1 + ls * 0.5, p[s]) });
            let label = c.text("label", &names[s], [x1 + ls * 0.5, placed[s]], "left", "center", ls * 0.85, 600.0, series_color(c, s), point_time(c, n - 1, s), Some(end_w - ls * 0.6));
            set(b, label, "at", at);
            c.out.push(label);
        }
    }
    if labels {
        let dx = (x1 - x0) / (n - 1) as f64;
        let widest = sets[0].rows.iter().map(|d| text_width(&d.label, ls * 0.85, 500.0)).fold(0.0, f64::max);
        let step = ((widest + ls) / dx).ceil().max(1.0) as usize;
        for i in (0..n).filter(|i| i % step == 0) {
            let p = pt(0, i, 0);
            let label = c.text("label", &sets[0].rows[i].label, [p[0], y1 + ls * 0.5], "center", "top", ls * 0.85, 500.0, c.th.muted, shift(b, c.start, 0.2), Some(dx * step as f64 * 0.95));
            c.out.push(label);
        }
    }
    // Callouts clear the head's readout.
    let clear = if m == 1 && c.values { ls * 2.2 } else { ls * 0.4 };
    c.lifted = m == 1 && c.values;
    for k in 0..sets.len() {
        for i in 0..n {
            let p = pt(k, i, 0);
            c.mark(k, i, [p[0], p[1] - clear]);
        }
    }
    Ok(())
}

fn s_(b: &B, text: &str) -> ScriptValue {
    s(b, text)
}

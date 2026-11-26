//! `@bar`: vertical or horizontal, single, grouped or stacked series, and
//! sorted races (`sort: true`) whose bars change rank between datasets.

use super::build::*;
use makepad_script::*;
use super::scale::{nice_ticks, text_width};
use super::{Chart, Table};

pub(super) fn build(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let sets = c.sets.clone();
    // Categories: matched by label across datasets when labels are unique.
    let by_label = sets.iter().all(|t| {
        let mut seen = std::collections::HashSet::new();
        t.rows.iter().all(|d| !d.label.is_empty() && seen.insert(d.label.clone()))
    });
    let mut cats: Vec<String> = Vec::new();
    if by_label {
        for t in &sets {
            for d in &t.rows {
                if !cats.contains(&d.label) {
                    cats.push(d.label.clone());
                }
            }
        }
    } else {
        let n = sets.iter().map(|t| t.rows.len()).max().unwrap_or(0);
        cats = (0..n).map(|i| sets[0].rows.get(i).map_or_else(|| format!("{}", i + 1), |d| d.label.clone())).collect();
    }
    c.keys = cats.clone();
    let n = cats.len();
    let m = sets.iter().map(Table::series_count).max().unwrap_or(1);
    let row = |k: usize, i: usize| if by_label { sets[k].rows.iter().find(|d| d.label == cats[i]) } else { sets[k].rows.get(i) };
    let val = |k: usize, i: usize, s: usize| row(k, i).map_or(0.0, |d| d.values.get(s).copied().unwrap_or(if s == 0 { d.value } else { 0.0 }));
    let present = |k: usize, i: usize| row(k, i).is_some();
    let stacked = c.stacked && m > 1;
    let grouped = !stacked && m > 1;
    let race = c.sort;
    let horizontal = c.horizontal || race;
    let top = c.top.unwrap_or(if race { n.min(10) } else { n }).min(n).max(1);
    // Each bar's value span, per dataset.
    let span = |k: usize, i: usize, s: usize| -> (f64, f64) {
        if !stacked {
            return (0.0, val(k, i, s));
        }
        let v = val(k, i, s);
        let below: f64 = (0..s).map(|q| val(k, i, q)).filter(|q| (*q >= 0.0) == (v >= 0.0)).sum();
        (below, below + v)
    };
    let total = |k: usize, i: usize| if stacked { (0..m).map(|s| val(k, i, s)).filter(|v| *v > 0.0).sum() } else { (0..m).map(|s| val(k, i, s)).fold(f64::MIN, f64::max) };
    let extent = |k: usize| {
        let mut lo = 0.0f64;
        let mut hi = 0.0f64;
        for i in 0..n {
            for s in 0..m {
                let (a, b) = span(k, i, s);
                lo = lo.min(a.min(b));
                hi = hi.max(a.max(b));
            }
        }
        (lo, hi)
    };
    let (mut lo, mut hi) = (0.0f64, 0.0f64);
    for k in 0..sets.len() {
        let (a, b) = extent(k);
        lo = lo.min(a);
        hi = hi.max(b);
    }
    let lo = c.min.unwrap_or(lo).min(0.0);
    let hi = c.max.unwrap_or(hi).max(lo + 1e-9);
    let ticks = nice_ticks(lo, hi, if horizontal { 4 } else { 5 });
    // A race rescales to each dataset's leader; other charts keep one axis.
    let cmax = c.max;
    let grow = c.grow;
    let domain = |k: usize| -> (f64, f64) {
        if race {
            (0.0, cmax.unwrap_or_else(|| (0..n).map(|i| total(k, i)).fold(1e-9, f64::max)) * 1.04)
        } else {
            (ticks[0], *ticks.last().unwrap())
        }
    };
    let axes = c.axes.unwrap_or(!race);
    let grid = c.grid.unwrap_or(!race);
    let labels = c.labels.unwrap_or(true);
    let all: Vec<f64> = (0..sets.len()).flat_map(|k| (0..n).map(move |i| (k, i))).map(|(k, i)| if stacked { total(k, i) } else { val(k, i, 0) }).collect();
    let max_abs = all.iter().fold(0.0f64, |a, v| a.max(v.abs())).max(hi.abs());
    let shown = c.fmt.shown(max_abs, 0.0, &all);
    let tick_step = if ticks.len() > 1 { ticks[1] - ticks[0] } else { 0.0 };
    let tick_shown = c.fmt.shown(max_abs, tick_step, &ticks);

    let legend: Vec<(String, u32)> = if m > 1 {
        (0..m).map(|s| (sets[0].series.get(s).cloned().unwrap_or_else(|| format!("Series {}", s + 1)), c.th.color(s))).collect()
    } else {
        Vec::new()
    };
    c.header(&legend);
    let ls = c.ls;
    let [px0, mut py0, px1, mut py1] = c.plot;
    let start = c.start.clone();
    if let Some(t) = c.y_title.clone() {
        let label = c.text("label", &t, [px0, py0], "left", "top", ls * 0.8, 500.0, c.th.muted, start, Some(px1 - px0));
        c.out.push(label);
        py0 += ls * 1.5;
    }
    if let Some(t) = c.x_title.clone() {
        let label = c.text("label", &t, [(px0 + px1) * 0.5, py1], "center", "bottom", ls * 0.8, 500.0, c.th.muted, start, Some(px1 - px0));
        c.out.push(label);
        py1 -= ls * 1.5;
    }
    let headroom = if c.notes.is_empty() { 0.0 } else { ls * 3.0 };
    let value_size = ls;
    let (x0, y0, x1, y1, slot, thick);
    if horizontal {
        let cat_w = if labels { (cats.iter().map(|l| text_width(l, ls * 0.9, 500.0)).fold(0.0, f64::max) * 1.1).min((px1 - px0) * 0.3) + ls * 0.8 } else { 0.0 };
        let val_w = if c.values { all.iter().map(|v| text_width(&shown.text(*v), value_size, 700.0)).fold(0.0, f64::max) + ls * 0.6 } else { 0.0 };
        x0 = px0 + cat_w;
        x1 = px1 - val_w;
        y0 = py0 + headroom;
        // A race's period ("2021") gets its own strip under the bars.
        let period = if sets.iter().any(|t| t.key.is_some()) { ls * 3.4 * 1.25 } else { 0.0 };
        y1 = py1 - if axes { ls * 1.6 } else { 0.0 } - period;
        slot = (y1 - y0) / top as f64;
        thick = slot * 0.72;
    } else {
        let tick_w = if axes { ticks.iter().map(|t| text_width(&tick_shown.text(*t), ls * 0.8, 400.0)).fold(0.0, f64::max) + ls * 0.7 } else { 0.0 };
        x0 = px0 + tick_w;
        x1 = px1;
        y0 = py0 + headroom + if c.values { ls * 1.5 } else { 0.0 };
        y1 = py1 - if labels { ls * 1.9 } else { 0.0 };
        slot = (x1 - x0) / n.max(1) as f64;
        thick = slot * 0.68;
    }
    if x1 - x0 < 10.0 || y1 - y0 < 10.0 {
        return Err(format!("the chart's box ({:.0}x{:.0}) is too small for its labels; make `size` larger or `label_size` smaller", c.w, c.h));
    }
    let bw = if grouped { thick / m as f64 } else { thick };
    // Value to the value axis's coordinate, for dataset k.
    let at_value = |k: usize, v: f64| -> f64 {
        let (dlo, dhi) = domain(k);
        let f = (v - dlo) / (dhi - dlo);
        if horizontal { x0 + f * (x1 - x0) } else { y1 - f * (y1 - y0) }
    };
    // Rank of each category in dataset k.
    let ranks: Vec<Vec<usize>> = (0..sets.len()).map(|k| {
        let mut order: Vec<usize> = (0..n).collect();
        if race {
            order.sort_by(|a, b| total(k, *b).total_cmp(&total(k, *a)).then(a.cmp(b)));
        }
        let mut rank = vec![0; n];
        for (r, i) in order.into_iter().enumerate() {
            rank[i] = r;
        }
        rank
    }).collect();
    let slot_at = |k: usize, i: usize| -> f64 {
        let r = ranks[k][i].min(top) as f64;
        if horizontal { y0 + slot * r + (slot - thick) * 0.5 } else { x0 + slot * r + (slot - thick) * 0.5 }
    };
    // A bar's box in dataset k: `[x, y, w, h]`.
    let geom = |k: usize, i: usize, s: usize| -> [f64; 4] {
        let (a, b) = span(k, i, s);
        let (pa, pb) = (at_value(k, a), at_value(k, b));
        let cross = slot_at(k, i) + if grouped { bw * s as f64 } else { 0.0 };
        let inner = if grouped { bw * 0.9 } else { bw };
        if horizontal {
            [pa.min(pb), cross, (pb - pa).abs(), inner]
        } else {
            [cross, pa.min(pb), inner, (pb - pa).abs()]
        }
    };
    let zero = |k: usize, i: usize, s: usize| -> [f64; 4] {
        let g = geom(k, i, s);
        let (a, _) = span(k, i, s);
        let p = at_value(k, a);
        if horizontal { [p, g[1], 0.0, g[3]] } else { [g[0], p, g[2], 0.0] }
    };
    let ease = c.ease(if race { "ease_in_out_cubic" } else { "ease_out_expo" });
    let times: Vec<Vec<ScriptValue>> = (0..sets.len()).map(|k| (0..n).map(|i| c.set_time(k, i)).collect()).collect();
    // A track through the datasets: `f(k)` at each dataset's time, held
    // until the next switch starts moving; entering from `from`.
    let track = |i: usize, from: ScriptValue, f: &dyn Fn(usize) -> ScriptValue| -> ScriptValue {
        let mut points = vec![(shift(b, times[0][i], -grow), from)];
        for k in 0..sets.len() {
            if k > 0 {
                points.push((shift(b, times[k][i], -grow), f(k - 1)));
            }
            points.push((times[k][i].clone(), f(k)));
        }
        keys(b, points, ease)
    };
    let visible = |k: usize, i: usize| present(k, i) && ranks[k][i] < top;

    // Gridlines and ticks.
    if !race && (grid || axes) {
        for t in &ticks {
            let p = at_value(0, *t);
            if grid && t.abs() > 1e-12 {
                c.out.push(if horizontal { c.rule([p - 0.5, y0], [1.0, y1 - y0], c.th.grid, start) } else { c.rule([x0, p - 0.5], [x1 - x0, 1.0], c.th.grid, start) });
            }
            if axes {
                let label = if horizontal {
                    c.text("tick", &tick_shown.text(*t), [p, y1 + ls * 0.4], "center", "top", ls * 0.8, 400.0, c.th.muted, start, None)
                } else {
                    c.text("tick", &tick_shown.text(*t), [x0 - ls * 0.5, p], "right", "center", ls * 0.8, 400.0, c.th.muted, start, None)
                };
                c.out.push(label);
            }
        }
    }
    // The period a race is at ("2021"), large in the corner.
    if sets.iter().any(|t| t.key.is_some()) {
        for (k, t) in sets.iter().enumerate() {
            let Some(key) = &t.key else { continue };
            let t_in = c.set_start(k);
            let mut points = vec![(t_in.clone(), num(0.0)), (shift(b, t_in, 0.3), num(1.0))];
            if k + 1 < sets.len() {
                points.push((c.switch[k].clone(), num(1.0)));
                points.push((shift(b, c.switch[k], 0.3), num(0.0)));
            }
            let (px, py) = if horizontal { (x1, py1) } else { (x1, y1 - ls * 0.2) };
            let label = c.text("period", key, [px, py], "right", "bottom", ls * 3.4, 800.0, alpha(c.th.muted, 0xb0), t_in, None);
            set(b, label, "opacity", keys(b, points, id("ease_in_out_cubic")));
            c.out.push(label);
        }
    }
    let mut marks = Vec::new();
    let mut avoid = Vec::new();
    for i in 0..n {
        let hl = c.highlighted(i);
        for s in 0..m {
            let fill = if m > 1 {
                c.th.color(s)
            } else if !c.highlight.is_empty() {
                if hl { c.th.accent } else { c.th.neutral }
            } else if race || (c.colors_given && c.th.series.len() > 1) {
                c.th.color(i)
            } else {
                c.th.color(0)
            };
            let g0 = zero(0, i, s);
            let at = track(i, v2(b, g0[0], g0[1]), &|k| { let g = geom(k, i, s); v2(b, g[0], g[1]) });
            let size = track(i, v2(b, g0[2], g0[3]), &|k| { let g = geom(k, i, s); v2(b, g[2], g[3]) });
            let bar = layer(b, "Rect", vec![
                ("name", s_(b, &format!("bar {}", cats[i]))), ("at", at), ("size", size), ("radius", num((bw * 0.1).min(8.0))), ("fill", color(fill)),
            ]);
            if sets.len() > 1 || !present(0, i) || !visible(0, i) {
                set(b, bar, "opacity", track(i, num(0.0), &|k| num(if visible(k, i) { 1.0 } else { 0.0 })));
            }
            c.out.push(bar);
        }
        // The value at the end of the bar (or of the stack).
        let per_bar_values = c.values && (!grouped || bw >= all.iter().map(|v| text_width(&shown.text(*v), ls * 0.75, 700.0)).fold(0.0, f64::max) + 4.0);
        let value_slots: Vec<Option<usize>> = if !per_bar_values { Vec::new() } else if grouped { (0..m).map(Some).collect() } else { vec![None] };
        for s in value_slots {
            let end = |k: usize| -> [f64; 2] {
                let g = match s {
                    Some(s) => geom(k, i, s),
                    None if stacked => {
                        let boxes: Vec<[f64; 4]> = (0..m).map(|q| geom(k, i, q)).collect();
                        let x = boxes.iter().map(|b| b[0]).fold(f64::MAX, f64::min);
                        let y = boxes.iter().map(|b| b[1]).fold(f64::MAX, f64::min);
                        let r = boxes.iter().map(|b| b[0] + b[2]).fold(f64::MIN, f64::max);
                        let btm = boxes.iter().map(|b| b[1] + b[3]).fold(f64::MIN, f64::max);
                        [x, y, r - x, btm - y]
                    }
                    None => geom(k, i, 0),
                };
                let negative = if stacked { false } else { val(k, i, s.unwrap_or(0)) < 0.0 };
                if horizontal {
                    [if negative { g[0] - ls * 0.4 } else { g[0] + g[2] + ls * 0.4 }, g[1] + g[3] * 0.5]
                } else {
                    [g[0] + g[2] * 0.5, if negative { g[1] + g[3] + ls * 0.3 } else { g[1] - ls * 0.3 }]
                }
            };
            let values: Vec<f64> = (0..sets.len()).map(|k| if s.is_none() && stacked { total(k, i) } else { val(k, i, s.unwrap_or(0)) }).collect();
            let e0 = zero(0, i, s.unwrap_or(0));
            let from = if horizontal { v2(b, e0[0] + ls * 0.4, e0[1] + e0[3] * 0.5) } else { v2(b, e0[0] + e0[2] * 0.5, e0[1] - ls * 0.3) };
            let at = track(i, from, &|k| { let p = end(k); v2(b, p[0], p[1]) });
            let size = if grouped { ls * 0.75 } else { value_size };
            let (align, valign) = if horizontal { ("left", "center") } else { ("center", "bottom") };
            let ink = if hl { c.th.accent } else { c.th.ink };
            let fit = if horizontal { None } else { Some(if grouped { bw * 1.1 } else { slot * 0.96 }) };
            let label = c.number("value", &values, &times.iter().map(|t| t[i].clone()).collect::<Vec<_>>(), &shown, at, align, valign, size, if hl { 800.0 } else { 700.0 }, ink, fit);
            if sets.len() > 1 || !visible(0, i) {
                set(b, label, "opacity", track(i, num(0.0), &|k| num(if visible(k, i) { 1.0 } else { 0.0 })));
            }
            c.out.push(label);
        }
        if labels {
            let ink = if hl { c.th.ink } else { c.th.muted };
            let weight = if hl { 700.0 } else { 500.0 };
            let label = if horizontal {
                let at = track(i, v2(b, x0 - ls * 0.5, slot_at(0, i) + thick * 0.5), &|k| v2(b, x0 - ls * 0.5, slot_at(k, i) + thick * 0.5));
                let l = c.text("label", &cats[i], [0.0, 0.0], "right", "center", ls * 0.9, weight, ink, c.enter(i), Some((x0 - px0 - ls * 0.8).max(10.0)));
                set(b, l, "at", at);
                l
            } else {
                c.text("label", &cats[i], [slot_at(0, i) + thick * 0.5, y1 + ls * 0.5], "center", "top", ls * 0.85, weight, ink, c.enter(i), Some(slot * 0.92))
            };
            if sets.len() > 1 || !visible(0, i) {
                set(b, label, "opacity", track(i, num(0.0), &|k| num(if visible(k, i) { 1.0 } else { 0.0 })));
            }
            c.out.push(label);
        }
        for k in 0..sets.len() {
            let g: Vec<[f64; 4]> = (0..m).map(|s| geom(k, i, s)).collect();
            let p = if horizontal {
                // Past the value label, so a callout beside it is clear.
                let label_w = if c.values { ls * 0.4 + text_width(&shown.text(if stacked { total(k, i) } else { val(k, i, 0) }), value_size, 700.0) } else { 0.0 };
                [g.iter().map(|b| b[0] + b[2]).fold(f64::MIN, f64::max) + label_w, g[0][1] + thick * 0.5]
            } else {
                [g[0][0] + thick * 0.5, g.iter().map(|b| b[1]).fold(f64::MAX, f64::min) - if c.values { ls * 1.5 } else { 0.0 }]
            };
            marks.push((k, i, p));
            // Callouts keep off the bars as they finally stand.
            if k + 1 == sets.len() && visible(k, i) {
                for b in &g {
                    avoid.push([b[0], b[1], b[0] + b[2], b[1] + b[3]]);
                }
            }
        }
    }
    c.lifted = c.values;
    c.avoid.extend(avoid);
    for (k, i, p) in marks {
        c.mark(k, i, p);
    }
    // The zero line over the bars' feet.
    let z = at_value(0, 0.0);
    let zero_line = if horizontal { c.rule([z - 1.0, y0], [2.0, y1 - y0], c.th.muted, start) } else { c.rule([x0, z - 1.0], [x1 - x0, 2.0], c.th.muted, start) };
    c.out.push(zero_line);
    Ok(())
}

fn s_(b: &B, text: &str) -> ScriptValue {
    s(b, text)
}


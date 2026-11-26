//! `@scatter` / `@bubble` (points on two value axes, coloured by group,
//! moving between datasets) and `@map` (lon/lat dots over the continents,
//! and a choropleth of continent values).

use super::build::*;
use makepad_script::*;
use super::scale::{nice_ticks, text_width};
use super::{world, Chart};

fn groups_of(c: &Chart) -> Vec<String> {
    let mut g: Vec<String> = Vec::new();
    for t in &c.sets {
        for d in &t.rows {
            if let Some(name) = &d.group {
                if !g.contains(name) {
                    g.push(name.clone());
                }
            }
        }
    }
    g
}

/// Rows matched across datasets by label (or index).
fn matcher(c: &Chart) -> impl Fn(usize, usize) -> Option<super::Datum> {
    let sets = c.sets.clone();
    let by_label = sets[0].rows.iter().all(|d| !d.label.is_empty());
    move |k: usize, i: usize| {
        let first = sets[0].rows.get(i)?;
        if by_label { sets[k].rows.iter().find(|d| d.label == first.label).cloned() } else { sets[k].rows.get(i).cloned() }
    }
}

pub(super) fn scatter(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let bubble = c.kind == "bubble";
    let sets = c.sets.clone();
    let n = sets[0].rows.len();
    let groups = groups_of(c);
    let color_of = |c: &Chart, g: &Option<String>, i: usize| -> u32 {
        if !c.highlight.is_empty() {
            return if c.highlighted(i) { c.th.accent } else { c.th.neutral };
        }
        match g {
            Some(g) => c.th.color(groups.iter().position(|x| x == g).unwrap_or(0)),
            None => c.th.color(0),
        }
    };
    c.keys = sets[0].rows.iter().map(|d| d.label.clone()).collect();
    let legend: Vec<(String, u32)> = groups.iter().enumerate().map(|(k, g)| (g.clone(), c.th.color(k))).collect();
    c.header(&legend);
    let ls = c.ls;
    // `x_scale` / `y_scale: @log`: positions in decades (log10).
    let (log_x, log_y) = (c.log_x, c.log_y);
    let lg = |on: bool, v: f64| if on { v.max(1e-300).log10() } else { v };
    let pos = |d: &super::Datum, i: usize| (lg(log_x, d.x.unwrap_or(i as f64)), lg(log_y, d.y.unwrap_or(d.value)));
    let all: Vec<(f64, f64)> = sets.iter().flat_map(|t| t.rows.iter().enumerate().map(|(i, d)| pos(d, i))).collect();
    let (xlo, xhi) = all.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
    let (ylo, yhi) = all.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let decades = |a: f64, b: f64| (a.floor() as i32..=b.ceil().max(a.floor() + 1.0) as i32).map(|e| e as f64).collect::<Vec<f64>>();
    let xt = if log_x { decades(xlo, xhi) } else { nice_ticks(xlo, xhi, 5) };
    let yt = if log_y { decades(c.min.map_or(ylo, |m| lg(true, m)), c.max.map_or(yhi, |m| lg(true, m))) } else { nice_ticks(c.min.unwrap_or(ylo), c.max.unwrap_or(yhi), 4) };
    let fmt = c.fmt.clone();
    let xs = super::scale::NumFmt { kind: "plain".into(), ..fmt.clone() }.shown(xt.iter().fold(0.0f64, |a, v| a.max(v.abs())), xt[1] - xt[0], &xt);
    let ys = fmt.shown(yt.iter().fold(0.0f64, |a, v| a.max(v.abs())), yt[1] - yt[0], &yt);
    let rmax = sets.iter().flat_map(|t| &t.rows).map(|d| d.r.unwrap_or(d.value).abs()).fold(0.0, f64::max).max(1e-9);
    let [px0, mut py0, px1, mut py1] = c.plot;
    let start = c.start.clone();
    let axes = c.axes.unwrap_or(true);
    if let Some(t) = c.y_title.clone() {
        let label = c.text("label", &t, [px0, py0], "left", "top", ls * 0.8, 500.0, c.th.muted, start, Some(px1 - px0));
        c.out.push(label);
        py0 += ls * 1.5;
    }
    if let Some(t) = c.x_title.clone() {
        let label = c.text("label", &t, [px1, py1], "right", "bottom", ls * 0.8, 500.0, c.th.muted, start, Some(px1 - px0));
        c.out.push(label);
        py1 -= ls * 1.5;
    }
    let big = if bubble { ls * 3.2 } else { c.thickness * 1.8 };
    let y_label = |t: f64| if log_y { super::axis::decade_label(t.round() as i32) } else { ys.text(t) };
    let x_label = |t: f64| if log_x { super::axis::decade_label(t.round() as i32) } else { xs.text(t) };
    let tick_w = if axes { yt.iter().map(|t| text_width(&y_label(*t), ls * 0.8, 400.0)).fold(0.0, f64::max) + ls * 0.7 } else { 0.0 };
    let headroom = if c.notes.is_empty() { 0.0 } else { ls * 3.0 };
    let (x0, x1, y0, y1) = (px0 + tick_w + big * 0.5, px1 - big, py0 + big + headroom, py1 - if axes { ls * 1.7 } else { 0.0 } - big * 0.5);
    if x1 - x0 < 10.0 || y1 - y0 < 10.0 {
        return Err(format!("the chart's box ({:.0}x{:.0}) is too small for its labels; make `size` larger or `label_size` smaller", c.w, c.h));
    }
    let (xd0, xd1, yd0, yd1) = (xt[0], *xt.last().unwrap(), yt[0], *yt.last().unwrap());
    let map = |p: (f64, f64)| [x0 + (p.0 - xd0) / (xd1 - xd0).max(1e-9) * (x1 - x0), y1 - (p.1 - yd0) / (yd1 - yd0).max(1e-9) * (y1 - y0)];
    if axes {
        for t in &yt {
            let y = map((xd0, *t))[1];
            let rule = c.rule([x0, y - 0.5], [x1 - x0, 1.0], c.th.grid, start);
            c.out.push(rule);
            let label = c.text("tick", &y_label(*t), [x0 - ls * 0.5 - big * 0.5, y], "right", "center", ls * 0.8, 400.0, c.th.muted, start, None);
            c.out.push(label);
        }
        for t in &xt {
            let x = map((*t, yd0))[0];
            let label = c.text("tick", &x_label(*t), [x, y1 + big * 0.5 + ls * 0.4], "center", "top", ls * 0.8, 400.0, c.th.muted, start, None);
            c.out.push(label);
        }
    }
    let rows: Vec<Vec<Option<super::Datum>>> = {
        let row = matcher(c);
        (0..sets.len()).map(|k| (0..n).map(|i| row(k, i)).collect()).collect()
    };
    let ease = c.ease("ease_in_out_cubic");
    let labels = c.labels.unwrap_or(n <= 12 && sets[0].rows.iter().all(|d| !d.label.is_empty()));
    for i in 0..n {
        let d0 = sets[0].rows[i].clone();
        let radius = |d: &super::Datum| if bubble { (d.r.unwrap_or(d.value).abs() / rmax).sqrt() * big + 3.0 } else { big * 0.5 };
        let t = c.land(i);
        let fill = color_of(c, &d0.group, i);
        let place = |k: usize| -> [f64; 3] {
            let d = rows[k][i].clone().unwrap_or_else(|| d0.clone());
            let p = map(pos(&d, i));
            [p[0], p[1], radius(&d)]
        };
        let switch = c.switch.clone();
        let set_times: Vec<ScriptValue> = (0..sets.len()).map(|k| c.set_time(k, i)).collect();
        let track = |f: &dyn Fn([f64; 3]) -> ScriptValue| -> ScriptValue {
            let mut points = vec![(t.clone(), f(place(0)))];
            for k in 1..sets.len() {
                points.push((switch[k - 1].clone(), f(place(k - 1))));
                points.push((set_times[k].clone(), f(place(k))));
            }
            keys(b, points, ease)
        };
        let dot = layer(b, "Ellipse", vec![
            ("name", s(b, &format!("point {}", if d0.label.is_empty() { i.to_string() } else { d0.label.clone() }))),
            ("at", track(&|p| v2(b, p[0] - p[2], p[1] - p[2]))), ("size", track(&|p| v2(b, 2.0 * p[2], 2.0 * p[2]))), ("fill", color(fill)),
            ("opacity", num(if bubble { 0.85 } else { 1.0 })), ("scale", tween(b, num(0.0), num(1.0), c.enter(i), t.clone(), id("ease_out_back"))),
        ]);
        c.out.push(dot);
        if labels && !d0.label.is_empty() {
            // Beside the point, on the side with room in every dataset.
            let tw = text_width(&d0.label, ls * 0.8, 500.0).min(c.w * 0.25);
            let right = (0..sets.len()).all(|k| { let p = place(k); p[0] + p[2] + ls * 0.3 + tw <= c.w });
            let at = track(&|p| if right { v2(b, p[0] + p[2] + ls * 0.3, p[1]) } else { v2(b, p[0] - p[2] - ls * 0.3, p[1]) });
            let label = c.text("label", &d0.label, [0.0, 0.0], if right { "left" } else { "right" }, "center", ls * 0.8, 500.0, c.th.ink, shift(b, t, 0.15), Some(c.w * 0.25));
            set(b, label, "at", at);
            c.out.push(label);
        }
        for k in 0..sets.len() {
            let p = place(k);
            c.mark(k, i, [p[0], p[1] - p[2]]);
        }
    }
    Ok(())
}

pub(super) fn map(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let sets = c.sets.clone();
    let groups = groups_of(c);
    c.keys = sets[0].rows.iter().map(|d| d.label.clone()).collect();
    // Choropleth values: `regions: {europe: 12}` and rows naming a region.
    let mut region_values: Vec<(usize, f64, usize)> = Vec::new();
    for (name, v) in &c.regions {
        match world::region(name) {
            Some(r) => region_values.push((r, *v, usize::MAX)),
            None => c.warnings.push(("regions".into(), format!("`{name}` is not a map region (north_america, south_america, europe, africa, asia, oceania, greenland, antarctica)"))),
        }
    }
    for (i, d) in sets[0].rows.iter().enumerate() {
        if d.x.is_none() && d.y.is_none() {
            if let Some(r) = d.region.as_deref().and_then(world::region).or_else(|| world::region(&d.label)) {
                region_values.push((r, d.value, i));
            }
        }
    }
    let legend: Vec<(String, u32)> = groups.iter().enumerate().map(|(k, g)| (g.clone(), c.th.color(k))).collect();
    c.header(&legend);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    let (pw, ph) = (px1 - px0, py1 - py0 - if region_values.is_empty() { 0.0 } else { ls * 2.0 });
    let (mw, mh) = if pw / ph > 2.0 { (ph * 2.0, ph) } else { (pw, pw * 0.5) };
    let (ox, oy) = (px0 + (pw - mw) * 0.5, py0 + (ph - mh) * 0.5);
    let start = c.start.clone();
    let vmax = region_values.iter().map(|r| r.1.abs()).fold(0.0, f64::max).max(1e-9);
    let ramp = |v: f64| mix(c.th.land, c.th.accent, 0.15 + 0.85 * (v.abs() / vmax));
    for (r, (name, data)) in world::REGIONS.iter().enumerate() {
        let fill = c.th.land;
        let shape = layer(b, "Path", vec![
            ("name", s(b, &format!("region {name}"))), ("svg", s(b, &world::fit(data, [ox, oy, mw, mh]))), ("fill", color(fill)), ("opacity", fade_in(b, shift(b, start, 0.4), 0.5)),
        ]);
        if let Some((_, v, i)) = region_values.iter().find(|(q, _, _)| *q == r) {
            let t = if *i == usize::MAX { shift(b, start, c.grow + 0.3) } else { c.land(*i) };
            set(b, shape, "fill", keys(b, vec![(shift(b, t, -c.grow), color(fill)), (t.clone(), color(ramp(*v)))], id("ease_in_out_cubic")));
        }
        c.out.push(shape);
    }
    if c.grid.unwrap_or(false) {
        for lon in (-180..=180).step_by(30) {
            let x = ox + (lon as f64 + 180.0) / 360.0 * mw;
            let rule = c.rule([x - 0.5, oy], [1.0, mh], c.th.grid, start);
            c.out.push(rule);
        }
        for lat in (-90..=90).step_by(30) {
            let y = oy + (90.0 - lat as f64) / 180.0 * mh;
            let rule = c.rule([ox, y - 0.5], [mw, 1.0], c.th.grid, start);
            c.out.push(rule);
        }
    }
    // The choropleth's key: low and high ends of the ramp.
    if !region_values.is_empty() {
        let shown = c.fmt.shown(vmax, 0.0, &region_values.iter().map(|r| r.1).collect::<Vec<_>>());
        let y = py1 - ls * 1.2;
        let steps = 5;
        let sw = ls * 1.6;
        for k in 0..steps {
            let rect = layer(b, "Rect", vec![
                ("name", s(b, "ramp")), ("at", v2(b, px0 + text_width(&shown.text(0.0), ls * 0.75, 400.0) + ls * 0.5 + sw * k as f64, y)),
                ("size", v2(b, sw, ls * 0.6)), ("fill", color(ramp(vmax * k as f64 / (steps - 1) as f64))), ("opacity", fade_in(b, start, 0.4)),
            ]);
            c.out.push(rect);
        }
        let lo = c.text("tick", &shown.text(0.0), [px0, y + ls * 0.3], "left", "center", ls * 0.75, 400.0, c.th.muted, start, None);
        c.out.push(lo);
        let hi = c.text("tick", &shown.text(vmax), [px0 + text_width(&shown.text(0.0), ls * 0.75, 400.0) + ls + sw * steps as f64, y + ls * 0.3], "left", "center", ls * 0.75, 400.0, c.th.muted, start, None);
        c.out.push(hi);
    }
    let dots: Vec<(usize, super::Datum)> = sets[0].rows.iter().cloned().enumerate().filter(|(_, d)| d.x.is_some() && d.y.is_some()).collect();
    let dmax = dots.iter().map(|(_, d)| d.value.abs()).fold(0.0, f64::max).max(1e-9);
    let labels = c.labels.unwrap_or(dots.len() <= 12);
    for (i, d) in &dots {
        let (x, y) = (ox + (d.x.unwrap() + 180.0) / 360.0 * mw, oy + (90.0 - d.y.unwrap()) / 180.0 * mh);
        let r = (d.value.abs() / dmax).sqrt() * ls * 0.9 + 3.0;
        let t = c.land(*i);
        let fill = match &d.group {
            Some(g) => c.th.color(groups.iter().position(|x| x == g).unwrap_or(0)),
            None if c.highlighted(*i) && !region_values.is_empty() => c.th.accent,
            // Over a choropleth the dots are ink, so they read on the ramp.
            None if !region_values.is_empty() => c.th.ink,
            None if c.highlighted(*i) || c.highlight.is_empty() => c.th.accent,
            None => c.th.muted,
        };
        c.out.push(layer(b, "Ellipse", vec![
            ("name", s(b, &format!("dot {}", d.label))), ("at", v2(b, x - r, y - r)), ("size", v2(b, 2.0 * r, 2.0 * r)), ("fill", color(fill)), ("opacity", num(0.9)),
            ("scale", tween(b, num(0.0), num(1.0), c.enter(*i), t.clone(), id("ease_out_back"))),
        ]));
        // A pulse ring as it lands.
        c.out.push(layer(b, "Ellipse", vec![
            ("name", s(b, "pulse")), ("at", v2(b, x - r, y - r)), ("size", v2(b, 2.0 * r, 2.0 * r)), ("stroke", color(fill)), ("width", num(2.0)),
            ("scale", tween(b, num(1.0), num(2.6), t.clone(), shift(b, t, 0.7), id("ease_out_cubic"))),
            ("opacity", keys(b, vec![(shift(b, t, -0.01), num(0.0)), (t.clone(), num(0.8)), (shift(b, t, 0.7), num(0.0))], id("ease_out_cubic"))),
        ]));
        if labels && !d.label.is_empty() {
            let label = c.text("label", &d.label, [x + r + ls * 0.3, y], "left", "center", ls * 0.8, 600.0, c.th.ink, shift(b, t, 0.1), Some(c.w * 0.2));
            c.out.push(label);
        }
        c.mark(0, *i, [x, y - r]);
    }
    Ok(())
}

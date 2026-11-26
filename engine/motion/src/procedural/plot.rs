//! Declarative data plots, sampled once while loading into ordinary Paths.
//! All coordinate conversion goes through `PlotBox::to_box`.

use super::*;
use super::point_map::PointMap;
use crate::plotting::{self, Bounds, Point};
use super::anim::{anim_at, anim_components, components, keyed, sample_times, seconds, window};

type Error = (String, String);
type ResultOf<T> = Result<T, Error>;
type Contours = Vec<(Vec<Point>, bool)>;

pub(super) const ITEMS: &[&str] = &["Graph", "Parametric", "Implicit", "VectorField", "Riemann", "Area", "Tangent", "Dot"];

fn given(cx: &Cx, o: ScriptObject, field: &str) -> Option<ScriptValue> {
    obj::opt(cx.vm, o, field).filter(|v| !v.is_nil())
}

fn bad(field: &str, message: impl Into<String>) -> Error { (field.into(), message.into()) }

fn number(cx: &mut Cx, o: ScriptObject, field: &str, default: f64, t: f64) -> ResultOf<f64> {
    let v = given(cx, o, field).unwrap_or(default.into());
    anim_at(cx, v, t).map_err(|(_, m)| bad(field, format!("`{field}`: {m}")))
}

fn positive(field: &str, n: f64) -> ResultOf<f64> {
    if n.is_finite() && n > 0.0 { Ok(n) } else { Err(bad(field, format!("`{field}` must be a positive finite number"))) }
}

fn function(cx: &Cx, o: ScriptObject, field: &str, vector: bool, two: bool) -> ResultOf<ScriptValue> {
    given(cx, o, field).filter(|v| obj::is_fn(cx.vm, *v)).ok_or_else(|| bad(field, format!("`{field}` is a function: `{field}: fn({}) {{ {} }}`", if two { "x, y" } else { "x" }, if vector { "vec2(-x, x)" } else { "x * x" })))
}

fn call<const N: usize>(cx: &mut Cx, f: ScriptValue, args: &[f64], t: f64, field: &str) -> ResultOf<[f64; N]> {
    let mut values: Vec<ScriptValue> = args.iter().map(|x| (*x).into()).collect();
    values.push(t.into());
    let result = cx.vm.call(f, &values);
    components(cx, result).ok_or_else(|| bad(field, format!("{field}({}) gave {}, expected {}", args.iter().map(|n| format!("{n:.5}").trim_end_matches('0').trim_end_matches('.').to_string()).collect::<Vec<_>>().join(", "), kind(cx, result), if N == 1 { "a number" } else { "vec2(x, y)" })))
}

#[derive(Clone)]
struct PlotBox {
    data: Bounds,
    size: Point,
    ticks: [Vec<f64>; 2],
    steps: Point,
    color: u32,
    label_size: f64,
    duration: f64,
    fps: f64,
    map: Option<PointMap>,
    map_at: ScriptValue,
    map_window: Option<[f64; 2]>,
}

impl PlotBox {
    /// All displayed points, including mapped points, pass through here.
    fn to_box(&self, p: Point) -> Point {
        [(p[0] - self.data.x[0]) / (self.data.x[1] - self.data.x[0]) * self.size[0],
            self.size[1] - (p[1] - self.data.y[0]) / (self.data.y[1] - self.data.y[0]) * self.size[1]]
    }
    fn screen(&self) -> Bounds { Bounds { x: [0.0, self.size[0]], y: [0.0, self.size[1]] } }
    /// Pixel-sized decorations are first constructed in the original plane.
    fn to_data(&self, p: Point) -> Point {
        [self.data.x[0] + p[0] / self.size[0] * (self.data.x[1] - self.data.x[0]),
            self.data.y[0] + (self.size[1] - p[1]) / self.size[1] * (self.data.y[1] - self.data.y[0])]
    }
    fn map_point(&self, cx: &mut Cx, point: Point) -> ResultOf<Point> {
        match self.map {
            Some(map) => map.apply(cx.vm, point).map_err(|m| bad("map", m)),
            None => Ok(point),
        }
    }
    fn amount(&self, cx: &mut Cx, t: f64) -> ResultOf<f64> {
        anim_at(cx, self.map_at, t).map(|v| v.clamp(0.0, 1.0)).map_err(|(_, m)| bad("map_at", format!("`map_at`: {m}")))
    }
    fn svg(&self, lines: Contours) -> String {
        plotting::polylines_to_svg(&lines.into_iter().map(|(line, closed)| (line.into_iter().map(|p| self.to_box(p)).collect(), closed)).collect::<Vec<_>>())
    }
}

fn range(cx: &Cx, o: ScriptObject, field: &str) -> ResultOf<([f64; 2], f64, Vec<f64>)> {
    let error = || bad(field, format!("`{field}` is [min, max] or [min, max, step] with min < max and a positive step"));
    let values = given(cx, o, field).and_then(|v| obj::items(cx.vm, v)).ok_or_else(error)?;
    if values.len() != 2 && values.len() != 3 { return Err(error()); }
    let values: Vec<f64> = values.into_iter().map(|v| obj::num(cx.vm, v)).collect::<Option<_>>().ok_or_else(error)?;
    let (lo, hi) = (values[0], values[1]);
    if !lo.is_finite() || !hi.is_finite() || lo >= hi || !(hi - lo).is_finite() { return Err(error()); }
    let (step, ticks) = if let Some(&step) = values.get(2) {
        if !step.is_finite() || step <= 0.0 { return Err(error()); }
        (step, plotting::ticks_at(lo, hi, step))
    } else { plotting::nice_ticks(lo, hi, 8) };
    Ok(([lo, hi], step, ticks))
}

fn span(cx: &Cx, o: ScriptObject, default: f64) -> ResultOf<[f64; 2]> {
    let Some(v) = given(cx, o, "span") else { return Ok([0.0, default]); };
    let p = obj::items(cx.vm, v).filter(|p| p.len() == 2).ok_or_else(|| bad("span", "`span` is [start, duration], in seconds or beat references"))?;
    let start = seconds(cx, p[0]).map_err(|(_, m)| bad("span", m))?;
    let length = positive("span", seconds(cx, p[1]).map_err(|(_, m)| bad("span", m))?)?;
    Ok([start, length])
}

pub(super) fn lower(cx: &mut Cx, o: ScriptObject) -> Made {
    let (x, dx, xt) = range(cx, o, "x")?;
    let (y, dy, yt) = range(cx, o, "y")?;
    let size = get_vec2(cx, o, "size", [cx.doc[0] * 0.8, cx.doc[1] * 0.8])?;
    positive("size.x", size[0])?; positive("size.y", size[1])?;
    let map = given(cx, o, "map").map(|v| PointMap::parse(cx.vm, v).map_err(|m| bad("map", m))).transpose()?;
    let map_at = given(cx, o, "map_at").unwrap_or(1.0.into());
    let duration = span(cx, o, cx.duration)?[1];
    let map_window = if map.is_some() { window(cx, map_at, duration).map_err(|(_, m)| bad("map_at", m))? } else { None };
    let plot = PlotBox { data: Bounds { x, y }, size, steps: [dx, dy], ticks: [xt, yt],
        color: get_color(cx, o, "color", 0xf2efe6ff)?, label_size: positive("label_size", get_num(cx, o, "label_size", 20.0)?)?,
        duration, fps: positive("fps", get_num(cx, o, "fps", 30.0)?)?, map, map_at, map_window };
    if map.is_some() { plot.amount(cx, 0.0)?; }
    let mut layers = axes(cx, o, &plot)?;
    if let Some(items) = given(cx, o, "layers") {
        let items = obj::items(cx.vm, items).ok_or_else(|| bad("layers", "`layers` is a list: [Graph{f: fn(x) { x * x }}]"))?;
        for (i, value) in items.into_iter().enumerate() {
            let ty = obj::type_name(cx.vm, value).unwrap_or("");
            if ITEMS.contains(&ty) {
                layers.push(item(cx, value.as_object().unwrap(), ty, &plot).map_err(|(field, message)| (format!("layers[{i}].{field}"), message))?);
            } else { layers.push(value); }
        }
    }
    // x/y on Plot are ranges, not Common's pixel offsets.
    let mut extra = vec![("size", v2(size[0], size[1])), ("x", 0.0.into()), ("y", 0.0.into())];
    if given(cx, o, "at").is_none() { extra.push(("at", v2((cx.doc[0] - size[0]) * 0.5, (cx.doc[1] - size[1]) * 0.5))); }
    Ok(group(cx, o, layers, extra))
}

fn tick_text(value: f64, step: f64) -> String {
    let digits = (0..=8).find(|&n| {
        let scaled = step * 10.0_f64.powi(n);
        (scaled - scaled.round()).abs() < 1e-8
    });
    let text = match digits {
        Some(digits) if value.abs() < 1e9 && step >= 1e-8 => format!("{value:.digits$}", digits = digits as usize),
        _ => format!("{value:.2e}"),
    };
    text.replace('-', "−")
}

/// Split decoration strokes around the measured labels, including plane lines.
fn clear_labels(lines: Vec<Vec<Point>>, labels: &[Bounds]) -> Contours {
    let mut out = Vec::new();
    for line in lines {
        for pair in line.windows(2) {
            let mut intervals = vec![(0.0, 1.0)];
            for &label in labels {
                if let Some((lo, hi)) = plotting::clip_segment(pair[0], pair[1], label) {
                    intervals = intervals.into_iter().flat_map(|(a, b)| {
                        let mut remain = Vec::new();
                        if hi <= a || lo >= b { remain.push((a, b)); }
                        else { if lo > a { remain.push((a, lo)); } if hi < b { remain.push((hi, b)); } }
                        remain
                    }).collect();
                }
            }
            for (a, b) in intervals { out.push((vec![plotting::mix(pair[0], pair[1], a), plotting::mix(pair[0], pair[1], b)], false)); }
        }
    }
    out
}

fn decoration(cx: &mut Cx, p: &PlotBox, contours: Contours) -> ResultOf<ScriptValue> {
    if p.map.is_none() { return Ok(obj::text(cx.vm, &plotting::polylines_to_svg(&contours))); }
    let times = sample_times(p.map_window, p.fps);
    let amounts = times.iter().map(|&t| p.amount(cx, t)).collect::<ResultOf<Vec<_>>>()?;
    let contours = contours.into_iter().map(|(line, closed)| (line.into_iter().map(|q| p.to_data(q)).collect(), closed)).collect();
    let frames = mapped_geometry(cx, p, vec![DataGeometry { contours, point: None, radius: None }], &amounts)?;
    let animated = times.len() > 1;
    Ok(keyed(cx, times.into_iter().zip(frames).map(|(t, g)| (t, g.svg.into())).collect(), animated))
}

/// Labels keep their pixel offset from a mapped data anchor.
fn map_label(cx: &mut Cx, p: &PlotBox, label: ScriptValue, anchor: Point, at: Point) -> ResultOf<()> {
    if p.map.is_none() { return Ok(()); }
    let source = p.to_data(anchor);
    let target = p.map_point(cx, source)?;
    let times = sample_times(p.map_window, p.fps);
    let mut positions = Vec::new();
    for &t in &times {
        let amount = p.amount(cx, t)?;
        let q = p.to_box(plotting::mix(source, target, amount));
        let q = if plotting::finite(q) { [q[0] + at[0] - anchor[0], q[1] + at[1] - anchor[1]] } else { [0.0; 2] };
        positions.push((t, v2(q[0], q[1])));
    }
    let at = keyed(cx, positions, times.len() > 1);
    let label = label.as_object().unwrap();
    obj::set(cx.vm, label, "at", at);
    if !plotting::finite(p.to_box(target)) { obj::set(cx.vm, label, "scale", 0.0.into()); }
    Ok(())
}

fn axes(cx: &mut Cx, o: ScriptObject, p: &PlotBox) -> ResultOf<Vec<ScriptValue>> {
    let grid = match given(cx, o, "grid") {
        None => "axes".into(),
        Some(v) => obj::ident(v).ok_or_else(|| bad("grid", "`grid` is @axes, @plane or @none"))?,
    };
    if !["axes", "plane", "none"].contains(&grid.as_str()) { return Err(bad("grid", "`grid` is @axes, @plane or @none")); }
    if grid == "none" { return Ok(Vec::new()); }
    let width = positive("width", get_num(cx, o, "width", 2.0)?)?;
    let [w, h] = p.size;
    let [ax, ay] = p.to_box([0.0_f64.clamp(p.data.x[0], p.data.x[1]), 0.0_f64.clamp(p.data.y[0], p.data.y[1])]);
    let px = p.label_size * 4.0 / 3.0;
    let gap = 10.0 + width * 0.5;
    let mut text_layers = Vec::new();
    let mut boxes = Vec::<Bounds>::new();
    for field in ["x_label", "y_label"] {
        if let Some(value) = given(cx, o, field) {
            let tex = obj::string(cx.vm, value).ok_or_else(|| bad(field, format!("`{field}` is TeX source, e.g. {field}: \"x\"")))?;
            let set = crate::math::typeset(&tex, "STIX Two Math", px, true).map_err(|m| bad(field, m))?;
            let at = if field == "x_label" { [w + 16.0, ay - set.height - 8.0] } else { [ax + 14.0, -set.height - 16.0] };
            boxes.push(Bounds { x: [at[0] - 3.0, at[0] + set.width + 3.0], y: [at[1] - 3.0, at[1] + set.height + 3.0] });
            let label = layer(cx, "Math", vec![("tex", tex.into()), ("at", v2(at[0], at[1])), ("size", p.label_size.into()), ("color", Lit::Color(p.color)), ("name", field.into())]);
            map_label(cx, p, label, if field == "x_label" { [w, ay] } else { [ax, 0.0] }, at)?;
            text_layers.push(label);
        }
    }
    if get_bool(cx, o, "labels", p.map.is_none()) {
        for axis in 0..2 {
            let mut last_end = f64::NEG_INFINITY;
            let mut ticks = p.ticks[axis].clone();
            if axis == 1 { ticks.reverse(); }
            for value in ticks {
                if value.abs() < p.steps[axis] * 1e-9 { continue; }
                let text = tick_text(value, p.steps[axis]);
                let tw = crate::text_width(&text, "Inter", px, 400.0, false, &[]).map_err(|m| bad("label_size", m))?;
                let point = p.to_box(if axis == 0 { [value, 0.0] } else { [0.0, value] });
                let at = if axis == 0 { [point[0] - tw * 0.5, ay + gap] } else { [ax - gap - tw, point[1] - px * 0.5] };
                let b = Bounds { x: [at[0] - 3.0, at[0] + tw + 3.0], y: [at[1] - 3.0, at[1] + px * 1.2 + 3.0] };
                // Never cross the other axis, or the previous number on this axis.
                let (start, end, cross) = if axis == 0 { (b.x[0], b.x[1], b.x[0] <= ax + width && b.x[1] >= ax - width) }
                    else { (b.y[0], b.y[1], b.y[0] <= ay + width && b.y[1] >= ay - width) };
                let overlap = boxes.iter().any(|a| a.x[0] < b.x[1] && b.x[0] < a.x[1] && a.y[0] < b.y[1] && b.y[0] < a.y[1]);
                if cross || overlap || start < last_end + 5.0 { continue; }
                last_end = end;
                boxes.push(b);
                let label = layer(cx, "Text", vec![("text", text.into()), ("at", v2(at[0], at[1])), ("size", p.label_size.into()), ("color", Lit::Color(p.color)), ("name", format!("{} tick {value}", if axis == 0 { "x" } else { "y" }).into())]);
                map_label(cx, p, label, if axis == 0 { [point[0], ay] } else { [ax, point[1]] }, at)?;
                text_layers.push(label);
            }
        }
    }
    let mut axis_lines = plotting::arrow_segments([0.0, ay], [w, ay], 10.0);
    axis_lines.extend(plotting::arrow_segments([ax, h], [ax, 0.0], 10.0));
    for &x in &p.ticks[0] { let x = p.to_box([x, 0.0])[0]; axis_lines.push(vec![[x, ay - 4.0], [x, ay + 4.0]]); }
    for &y in &p.ticks[1] { let y = p.to_box([0.0, y])[1]; axis_lines.push(vec![[ax - 4.0, y], [ax + 4.0, y]]); }
    let mut layers = Vec::new();
    if grid == "plane" {
        let color = get_color(cx, o, "grid_color", with_alpha(p.color, (p.color & 255) as f64 / 255.0 * 0.18))?;
        for minor in [true, false] {
            let mut lines = Vec::new();
            for axis in 0..2 {
                let r = if axis == 0 { p.data.x } else { p.data.y };
                for tick in plotting::ticks_at(r[0], r[1], p.steps[axis] * if minor { 0.5 } else { 1.0 }) {
                    if minor && (tick / p.steps[axis] - (tick / p.steps[axis]).round()).abs() < 1e-8 { continue; }
                    let q = p.to_box([tick, tick]);
                    lines.push(if axis == 0 { vec![[q[0], 0.0], [q[0], h]] } else { vec![[0.0, q[1]], [w, q[1]]] });
                }
            }
            let color = if minor { with_alpha(color, (color & 255) as f64 / 255.0 * 0.45) } else { color };
            let svg = decoration(cx, p, clear_labels(lines, &boxes))?;
            layers.push(layer(cx, "Path", vec![("svg", svg.into()), ("stroke", Lit::Color(color)), ("width", 1.0.into()), ("name", if minor { "minor grid".into() } else { "grid".into() })]));
        }
    }
    let svg = decoration(cx, p, clear_labels(axis_lines, &boxes))?;
    layers.push(layer(cx, "Path", vec![("svg", svg.into()), ("stroke", Lit::Color(p.color)), ("width", width.into()), ("name", "axes".into())]));
    if let Some(reveal) = given(cx, o, "reveal") {
        let r = obj::items(cx.vm, reveal).filter(|r| r.len() == 2).ok_or_else(|| bad("reveal", "`reveal` is [start, duration], in seconds"))?;
        let start = seconds(cx, r[0]).map_err(|(_, m)| bad("reveal", m))?;
        let duration = positive("reveal", seconds(cx, r[1]).map_err(|(_, m)| bad("reveal", m))?)?;
        let trim = tween(cx, 0.0.into(), 1.0.into(), start.into(), duration, "linear");
        for v in &layers { obj::set(cx.vm, v.as_object().unwrap(), "trim_end", trim); }
        let alpha = tween(cx, 0.0.into(), 1.0.into(), (start + duration).into(), 0.25, "linear");
        for v in &text_layers { obj::set(cx.vm, v.as_object().unwrap(), "opacity", alpha); }
    }
    layers.extend(text_layers);
    Ok(layers)
}

#[derive(Clone, Copy)]
struct Functions { f: Option<ScriptValue>, g: Option<ScriptValue> }

fn animated_fields(ty: &str) -> &'static [&'static str] {
    match ty {
        "Graph" | "Parametric" | "Area" => &["from", "to"],
        "Implicit" => &["cells"],
        "VectorField" => &["scale", "step"],
        "Riemann" => &["from", "to", "n"],
        "Tangent" => &["x", "length"],
        "Dot" => &["at", "radius"],
        _ => &[],
    }
}

fn time_dependent(cx: &mut Cx, o: ScriptObject, ty: &str, p: &PlotBox, fs: Functions) -> ResultOf<bool> {
    let Some(f) = fs.f else { return Ok(false); };
    let probe = p.duration * 0.6180339887498949;
    let equal = |a: f64, b: f64| a == b || (a.is_nan() && b.is_nan());
    let mut args = Vec::<Vec<f64>>::new();
    if ty == "Implicit" {
        let step = (p.data.x[1] - p.data.x[0]).max(p.data.y[1] - p.data.y[0]) / positive("cells", number(cx, o, "cells", 100.0, 0.0)?.round())?;
        let nx = ((p.data.x[1] - p.data.x[0]) / step).ceil() as usize;
        let ny = ((p.data.y[1] - p.data.y[0]) / step).ceil() as usize;
        for j in 0..=ny {
            for i in 0..=nx {
                let x = (p.data.x[0] + i as f64 * step).min(p.data.x[1]);
                let y = (p.data.y[0] + j as f64 * step).min(p.data.y[1]);
                args.push(vec![x, y]);
                if i < nx && j < ny {
                    args.push(vec![(x + (x + step).min(p.data.x[1])) * 0.5, (y + (y + step).min(p.data.y[1])) * 0.5]);
                }
            }
        }
    } else if ty == "VectorField" {
        let step = positive("step", number(cx, o, "step", p.steps[0] * 0.5, 0.0)?)?;
        for y in plotting::ticks_at(p.data.y[0], p.data.y[1], step) {
            for x in plotting::ticks_at(p.data.x[0], p.data.x[1], step) { args.push(vec![x, y]); }
        }
        // Also probe the centre when the chosen lattice has no point in the box.
        args.push(vec![(p.data.x[0] + p.data.x[1]) * 0.5, (p.data.y[0] + p.data.y[1]) * 0.5]);
    } else {
        let from = number(cx, o, "from", if ty == "Parametric" { 0.0 } else { p.data.x[0] }, 0.0)?;
        let to = number(cx, o, "to", if ty == "Parametric" { std::f64::consts::TAU } else { p.data.x[1] }, 0.0)?;
        for i in 0..=240 { args.push(vec![from + (to - from) * i as f64 / 240.0]); }
    }
    // A function may wait before it moves (a wave that starts at 5 s): compare
    // the start with times spread over the whole clock, not one probe.
    let probes: Vec<f64> = (1..=12).map(|k| p.duration * k as f64 / 12.0).chain([probe]).collect();
    for args in args {
        if ty == "Parametric" || ty == "VectorField" {
            let a = call::<2>(cx, f, &args, 0.0, "f")?;
            for &t in &probes {
                let b = call::<2>(cx, f, &args, t, "f")?;
                if !a.into_iter().zip(b).all(|(a, b)| equal(a, b)) { return Ok(true); }
            }
        } else {
            let a = call::<1>(cx, f, &args, 0.0, "f")?[0];
            let ga = match fs.g { Some(g) => Some(call::<1>(cx, g, &args, 0.0, "g")?[0]), None => None };
            for &t in &probes {
                if !equal(a, call::<1>(cx, f, &args, t, "f")?[0]) { return Ok(true); }
                if let (Some(g), Some(ga)) = (fs.g, ga) {
                    if !equal(ga, call::<1>(cx, g, &args, t, "g")?[0]) { return Ok(true); }
                }
            }
        }
    }
    Ok(false)
}

/// The part of segment `a`-`b` inside `box_` (both ends clipped), if any.
fn clip_segment(a: Point, b: Point, box_: Bounds) -> Option<[Point; 2]> {
    if !(a.iter().chain(&b).all(|v| v.is_finite())) { return None; }
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    let d = [b[0] - a[0], b[1] - a[1]];
    for (axis, r) in [(0, box_.x), (1, box_.y)] {
        for (edge, sign) in [(r[0], 1.0), (r[1], -1.0)] {
            let dist = (a[axis] - edge) * sign;
            let rate = d[axis] * sign;
            if rate == 0.0 {
                if dist < 0.0 { return None; }
            } else {
                let t = -dist / rate;
                if rate > 0.0 { t0 = t0.max(t); } else { t1 = t1.min(t); }
            }
        }
    }
    (t0 <= t1).then(|| [[a[0] + d[0] * t0, a[1] + d[1] * t0], [a[0] + d[0] * t1, a[1] + d[1] * t1]])
}

/// A moving curve as one two-point line per sample interval, clipped to the
/// box: the svg's commands stay the same in every key, and a curve that
/// leaves the box breaks there instead of running along its edge. Pieces with
/// nothing inside sit, with no length, on the nearest visible end.
fn fixed_segments(points: &[Point], box_: Bounds) -> Vec<Vec<Point>> {
    let clipped: Vec<Option<[Point; 2]>> = points.windows(2).map(|w| clip_segment(w[0], w[1], box_)).collect();
    let fallback = clipped.iter().flatten().next().map(|s| s[0]).unwrap_or_else(|| box_.clamp(points.first().copied().unwrap_or([0.0, 0.0])));
    let mut last = fallback;
    clipped.into_iter().map(|s| match s {
        Some(seg) => { last = seg[1]; seg.to_vec() }
        None => vec![last, last],
    }).collect()
}

/// `count`: the points of a curve sampled on a fixed grid (a moving curve, whose keys share their commands).
fn sample_curve(cx: &mut Cx, f: ScriptValue, domain: [f64; 2], t: f64, p: &PlotBox, parametric: bool, fixed: bool, count: usize) -> ResultOf<Vec<Vec<Point>>> {
    let mut error = None;
    let mut sample = |s| {
        let point = if parametric { call::<2>(cx, f, &[s], t, "f") }
            else { call::<1>(cx, f, &[s], t, "f").map(|v| [s, v[0]]) };
        match point { Ok(p) => Some(p), Err(e) => { if error.is_none() { error = Some(e); } None } }
    };
    let lines = if fixed {
        // Fixed commands and correspondence across keys, even through invalid
        // samples. Clamping prevents NaN/inf or out-of-box SVG coordinates.
        let points: Vec<Point> = (0..=count).map(|i| {
            let s = domain[0] + (domain[1] - domain[0]) * i as f64 / count as f64;
            sample(s).unwrap_or([s, f64::NAN])
        }).collect();
        fixed_segments(&points, p.data)
    } else { plotting::adaptive_curve(&mut sample, domain[0], domain[1], |q| p.to_box(q), p.screen()) };
    if let Some(e) = error { return Err(e); }
    Ok(lines)
}

/// Both sides share adaptive x samples, so filling does not join across a
/// missing sample, nor lose a region whose top is outside the visible box.
fn area(cx: &mut Cx, fs: Functions, domain: [f64; 2], t: f64, p: &PlotBox, fixed: bool, count: usize) -> ResultOf<Contours> {
    type Pair = [Point; 2];
    fn subdivide(f: &mut impl FnMut(f64) -> ResultOf<Option<Pair>>, map: &impl Fn(Point) -> Point,
        range: [f64; 2], ends: [Option<Pair>; 2], depth: usize, out: &mut Vec<Option<Pair>>) -> ResultOf<()> {
        let mid = (range[0] + range[1]) * 0.5;
        let m = f(mid)?;
        let refine = match (ends[0], m, ends[1]) {
            (Some(a), Some(m), Some(b)) => (0..2).any(|i| {
                let q = plotting::mix(map(a[i]), map(b[i]), 0.5);
                let r = map(m[i]);
                (q[0] - r[0]).hypot(q[1] - r[1]) > 0.35
            }),
            (None, None, None) => false,
            _ => true,
        };
        if refine && depth < 10 {
            subdivide(f, map, [range[0], mid], [ends[0], m], depth + 1, out)?;
            subdivide(f, map, [mid, range[1]], [m, ends[1]], depth + 1, out)?;
        } else { out.push(m); out.push(ends[1]); }
        Ok(())
    }
    let mut sample = |x| -> ResultOf<Option<Pair>> {
        let a = call::<1>(cx, fs.f.unwrap(), &[x], t, "f")?[0];
        let b = match fs.g { Some(g) => call::<1>(cx, g, &[x], t, "g")?[0], None => 0.0 };
        let pair = [[x, a], [x, b]];
        Ok(if fixed { Some(pair.map(|q| p.data.clamp(q))) } else { (a.is_finite() && b.is_finite()).then_some(pair) })
    };
    let n = if fixed { count } else { 64 };
    let mut a = sample(domain[0])?;
    let mut samples = vec![a];
    for i in 0..n {
        let range = [domain[0] + (domain[1] - domain[0]) * i as f64 / n as f64, domain[0] + (domain[1] - domain[0]) * (i + 1) as f64 / n as f64];
        let b = sample(range[1])?;
        if fixed { samples.push(b); } else { subdivide(&mut sample, &|q| p.to_box(q), range, [a, b], 0, &mut samples)?; }
        a = b;
    }
    let mut contours = Vec::new();
    for part in samples.split(Option::is_none) {
        if part.len() < 2 { continue; }
        let points: Vec<Point> = part.iter().map(|v| v.unwrap()[0]).chain(part.iter().rev().map(|v| v.unwrap()[1])).collect();
        contours.push((if fixed { points } else { plotting::clip_polygon(points, p.data) }, true));
    }
    Ok(contours)
}

#[derive(Clone)]
struct DataGeometry { contours: Contours, point: Option<Point>, radius: Option<f64> }

struct Geometry { svg: String, point: Option<Point> }

/// A union of full-strength subdivisions across geometry samples. Missing
/// contours collapse to a point; extra vertices repeat the last one. Thus
/// changing item parameters cannot change a mapped Path's command template.
fn mapped_geometry(cx: &mut Cx, p: &PlotBox, mut frames: Vec<DataGeometry>, amounts: &[f64]) -> ResultOf<Vec<Geometry>> {
    let count = frames.iter().map(|f| f.contours.len()).max().unwrap_or(0);
    for contour in 0..count {
        let n = frames.iter().filter_map(|f| f.contours.get(contour)).map(|c| c.0.len()).max().unwrap_or(0);
        let closed = frames.iter().filter_map(|f| f.contours.get(contour)).any(|c| c.1);
        let mixed = closed && frames.iter().filter_map(|f| f.contours.get(contour)).any(|c| !c.1);
        let seed = frames.iter().filter_map(|f| f.contours.get(contour)).find_map(|c| c.0.first()).copied().unwrap_or([0.0; 2]);
        for frame in &mut frames {
            if frame.contours.len() <= contour { frame.contours.push((Vec::new(), closed)); }
            let line = &mut frame.contours[contour];
            line.0.resize(n, line.0.last().copied().unwrap_or(seed));
            // If a level set opens during animation, its closing edge shrinks
            // to the last vertex instead of closing the open frame by force.
            if mixed && n > 0 { line.0.push(if line.1 { line.0[0] } else { line.0[n - 1] }); }
            line.1 = closed && !mixed;
        }
    }
    let mut sources = vec![Contours::new(); frames.len()];
    let mut targets = sources.clone();
    for contour in 0..count {
        let (line, closed) = &frames[0].contours[contour];
        let n = line.len();
        for i in 0..frames.len() {
            sources[i].push((Vec::new(), *closed));
            targets[i].push((Vec::new(), *closed));
        }
        if n == 0 { continue; }
        let edges = if n == 1 { 1 } else { n - 1 + usize::from(*closed) };
        for edge in 0..edges {
            let next = (edge + 1) % n;
            let mut fractions = Vec::new();
            for frame in &frames {
                let line = &frame.contours[contour].0;
                fractions.extend(plotting::map_subdivision(line[edge], line[next], &mut |q| {
                    Ok::<_, Error>(p.to_box(p.map_point(cx, q)?))
                }, 0.35, (p.data.x[1] - p.data.x[0]) / 16.0)?);
            }
            fractions.sort_by(f64::total_cmp);
            fractions.dedup();
            for u in fractions.into_iter().skip(usize::from(edge > 0)) {
                let mut pairs = Vec::with_capacity(frames.len());
                for frame in &frames {
                    let line = &frame.contours[contour].0;
                    let q = plotting::mix(line[edge], line[next], u);
                    pairs.push((q, p.map_point(cx, q)?));
                }
                // A pole stays a break in every key, including k=0. This keeps
                // M/L commands aligned and never draws a bridge over a pole.
                let valid = pairs.iter().all(|&(a, b)| plotting::finite(p.to_box(a)) && plotting::finite(p.to_box(b)));
                for (i, (a, b)) in pairs.into_iter().enumerate() {
                    sources[i][contour].0.push(if valid { a } else { [f64::NAN; 2] });
                    targets[i][contour].0.push(if valid { b } else { [f64::NAN; 2] });
                }
            }
        }
    }
    let points = frames.iter().map(|frame| frame.point.map(|q| p.map_point(cx, q).map(|mapped| (q, mapped))).transpose()).collect::<ResultOf<Vec<_>>>()?;
    let mut out = Vec::with_capacity(amounts.len());
    for (index, &amount) in amounts.iter().enumerate() {
        let i = if frames.len() == 1 { 0 } else { index };
        let contours = sources[i].iter().zip(&targets[i]).map(|((a, closed), (b, _))| {
            (a.iter().zip(b).map(|(&a, &b)| plotting::mix(a, b, amount)).collect(), *closed)
        }).collect();
        let point = points[i].and_then(|(a, b)| {
            let point = p.to_box(plotting::mix(a, b, amount));
            plotting::finite(point).then_some(point)
        });
        let svg = match frames[i].radius {
            Some(radius) => circle(point.unwrap_or([f64::NAN; 2]), radius),
            None => p.svg(contours),
        };
        out.push(Geometry { svg, point });
    }
    Ok(out)
}

fn unmapped_geometry(p: &PlotBox, frame: DataGeometry) -> Geometry {
    let point = frame.point.map(|q| p.to_box(q));
    let svg = match frame.radius {
        Some(radius) => circle(point.unwrap_or([f64::NAN; 2]), radius),
        None => p.svg(frame.contours),
    };
    Geometry { svg, point }
}

fn circle(point: Point, radius: f64) -> String {
    // An invisible marker has the same commands as a visible one.
    let (point, radius) = if plotting::finite(point) { (point, radius) } else { ([0.0; 2], 0.0) };
    let points = (0..64).map(|i| {
        let a = i as f64 * std::f64::consts::TAU / 64.0;
        [point[0] + radius * a.cos(), point[1] + radius * a.sin()]
    }).collect();
    // Markers keep their pixel radius even when their centre is on an axis
    // along the box edge. Like labels they may extend beyond that edge.
    plotting::polylines_to_svg(&[(points, true)])
}

/// Points of a moving curve (`samples`, 400): it is drawn on a fixed grid so every key has the same commands.
fn samples(cx: &Cx, o: ScriptObject) -> ResultOf<usize> {
    let n = get_num(cx, o, "samples", 400.0)?;
    if !(n.is_finite() && (16.0..=4000.0).contains(&n)) { return Err(bad("samples", "`samples` is a number of points from 16 to 4000 (400 by default)")); }
    Ok(n.round() as usize)
}

fn geometry(cx: &mut Cx, o: ScriptObject, ty: &str, p: &PlotBox, fs: Functions, t: f64, fixed: bool) -> ResultOf<DataGeometry> {
    let domain = if ["Graph", "Parametric", "Area", "Riemann"].contains(&ty) {
        [number(cx, o, "from", if ty == "Parametric" { 0.0 } else { p.data.x[0] }, t)?,
            number(cx, o, "to", if ty == "Parametric" { std::f64::consts::TAU } else { p.data.x[1] }, t)?]
    } else { p.data.x };
    let mut point = None;
    let contours = match ty {
        "Graph" | "Parametric" => sample_curve(cx, fs.f.unwrap(), domain, t, p, ty == "Parametric", fixed, samples(cx, o)?)?.into_iter().map(|l| (l, false)).collect(),
        "Area" => area(cx, fs, domain, t, p, fixed, samples(cx, o)?)?,
        "Implicit" => {
            let cells = positive("cells", number(cx, o, "cells", 100.0, t)?.round())? as usize;
            let mut error = None;
            let lines = plotting::marching_squares(|x, y| match call::<1>(cx, fs.f.unwrap(), &[x, y], t, "f") {
                Ok(v) => v[0], Err(e) => { if error.is_none() { error = Some(e); } f64::NAN }
            }, p.data, cells);
            if let Some(e) = error { return Err(e); }
            lines
        }
        "VectorField" => {
            let step = positive("step", number(cx, o, "step", p.steps[0] * 0.5, t)?)?;
            let scale = number(cx, o, "scale", 0.4, t)?;
            let mut lines = Vec::new();
            for y in plotting::ticks_at(p.data.y[0], p.data.y[1], step) {
                for x in plotting::ticks_at(p.data.x[0], p.data.x[1], step) {
                    let v = call::<2>(cx, fs.f.unwrap(), &[x, y], t, "f")?;
                    let length = v[0].hypot(v[1]);
                    if !length.is_finite() || length <= 1e-12 || scale == 0.0 {
                        if fixed { lines.extend((0..3).map(|_| (vec![[x, y]; 2], false))); }
                        continue;
                    }
                    let factor = (length * scale.abs()).min(0.9 * step) / length * scale.signum();
                    let end = [x + v[0] * factor, y + v[1] * factor];
                    for line in plotting::arrow_segments([x, y], end, step * 0.22) {
                        for pair in line.windows(2) {
                            if let Some((a, b)) = plotting::clip_segment(p.to_box(pair[0]), p.to_box(pair[1]), p.screen()) {
                                lines.push((vec![plotting::mix(pair[0], pair[1], a), plotting::mix(pair[0], pair[1], b)], false));
                            } else if fixed {
                                lines.push((vec![p.data.clamp(pair[1]); 2], false));
                            }
                        }
                    }
                }
            }
            lines
        }
        "Riemann" => {
            let n = positive("n", number(cx, o, "n", 8.0, t)?.round())? as usize;
            let side = match given(cx, o, "side") {
                None => "left".into(),
                Some(v) => obj::ident(v).ok_or_else(|| bad("side", "`side` is @left, @right or @mid"))?,
            };
            let offset = match side.as_str() { "left" => 0.0, "right" => 1.0, "mid" => 0.5, _ => return Err(bad("side", "`side` is @left, @right or @mid")) };
            let mut lines = Vec::new();
            for i in 0..n {
                let a = domain[0] + (domain[1] - domain[0]) * i as f64 / n as f64;
                let b = domain[0] + (domain[1] - domain[0]) * (i + 1) as f64 / n as f64;
                let y = call::<1>(cx, fs.f.unwrap(), &[a + (b - a) * offset], t, "f")?[0];
                if !y.is_finite() && !fixed { continue; }
                let y = if y.is_finite() { y } else { 0.0 };
                let rect = vec![[a, 0.0], [b, 0.0], [b, y], [a, y]];
                lines.push((if fixed { rect.into_iter().map(|q| p.data.clamp(q)).collect() } else { plotting::clip_polygon(rect, p.data) }, true));
            }
            lines
        }
        "Tangent" => {
            let x = number(cx, o, "x", 0.0, t)?;
            let length = positive("length", number(cx, o, "length", 3.0, t)?)?;
            let h = (p.data.x[1] - p.data.x[0]) * 1e-4;
            let y = call::<1>(cx, fs.f.unwrap(), &[x], t, "f")?[0];
            let slope = (call::<1>(cx, fs.f.unwrap(), &[x + h], t, "f")?[0] - call::<1>(cx, fs.f.unwrap(), &[x - h], t, "f")?[0]) / (2.0 * h);
            if y.is_finite() { point = Some([x, y]); }
            let a = [x - length * 0.5, y - slope * length * 0.5];
            let b = [x + length * 0.5, y + slope * length * 0.5];
            match plotting::clip_segment(p.to_box(a), p.to_box(b), p.screen()) {
                Some((u, v)) => vec![(vec![plotting::mix(a, b, u), plotting::mix(a, b, v)], false)],
                None if fixed => vec![(vec![p.data.clamp([x, y]); 2], false)],
                None => Vec::new(),
            }
        }
        "Dot" => {
            let at = given(cx, o, "at").ok_or_else(|| bad("at", "`at` is a data point: at: vec2(1, 1)"))?;
            let data: Point = anim_components(cx, at, t).map_err(|(_, m)| bad("at", format!("`at`: {m}")))?;
            let radius = positive("radius", number(cx, o, "radius", 8.0, t)?)?;
            return Ok(DataGeometry { contours: Vec::new(), point: Some(data), radius: Some(radius) });
        }
        _ => unreachable!(),
    };
    Ok(DataGeometry { contours, point, radius: None })
}

/// Preserve look and Common values, consuming only the fields whose meaning
/// is in data coordinates on this particular item.
fn copy_common(cx: &mut Cx, o: ScriptObject, target: ScriptValue, ty: &str) {
    let target = target.as_object().unwrap();
    for (key, value) in common(cx, o) {
        if (ty == "Dot" && key == "at") || (ty == "Tangent" && key == "x") || (ty == "VectorField" && key == "scale") || value.is_nil() { continue; }
        obj::set(cx.vm, target, &key, value);
    }
    let at = cx.vm.bx.heap_ref().made_at(o);
    cx.vm.bx.heap_mut().set_made_at(target, at);
}

fn path(cx: &mut Cx, o: ScriptObject, ty: &str, p: &PlotBox, svg: ScriptValue, dot: bool) -> ScriptValue {
    let color = given(cx, o, "color").unwrap_or(obj::color(p.color));
    let mut fields = vec![("svg", svg.into()), ("width", 2.0.into())];
    if dot { fields.push(("fill", color.into())); }
    else {
        fields.push(("stroke", color.into()));
        if ty == "Area" || ty == "Riemann" { fields.push(("fill", Lit::Color(with_alpha(p.color, 0.2)))); }
    }
    // Curves end round: a moving curve is drawn as clipped two-point pieces.
    if (ty == "Graph" || ty == "Parametric") && given(cx, o, "cap").is_none() {
        fields.push(("cap", Lit::Id("round")));
    }
    for field in ["stroke", "width", "fill", "trim_start", "trim_end", "cap", "join"] {
        if let Some(v) = given(cx, o, field) { fields.push((field, v.into())); }
    }
    let made = layer(cx, "Path", fields);
    copy_common(cx, o, made, ty);
    if given(cx, o, "name").is_none() {
        let name = obj::text(cx.vm, ty);
        obj::set(cx.vm, made.as_object().unwrap(), "name", name);
    }
    made
}

fn item(cx: &mut Cx, o: ScriptObject, ty: &str, p: &PlotBox) -> Made {
    let fs = Functions {
        f: if ty == "Dot" { None } else { Some(function(cx, o, "f", ty == "Parametric" || ty == "VectorField", ty == "Implicit" || ty == "VectorField")?) },
        g: if ty == "Area" && given(cx, o, "g").is_some() { Some(function(cx, o, "g", false, false)?) } else { None },
    };
    let dependent = time_dependent(cx, o, ty, p, fs)?;
    let mut times = if dependent { Some([0.0, p.duration]) } else { None };
    for &field in animated_fields(ty) {
        if let Some(v) = given(cx, o, field) {
            if let Some(w) = window(cx, v, p.duration).map_err(|(_, m)| bad(field, format!("`{field}`: {m}")))? {
                times = Some(match times { Some(r) => [r[0].min(w[0]), r[1].max(w[1])], None => w });
            }
        }
    }
    let moving_geometry = times.is_some();
    if let Some(w) = p.map_window {
        times = Some(match times { Some(r) => [r[0].min(w[0]), r[1].max(w[1])], None => w });
    }
    // A moving plane shares Plot's sampling clock across decoration and items.
    let fps = if p.map_window.is_some() { p.fps } else { positive("fps", get_num(cx, o, "fps", p.fps)?)? };
    let times = sample_times(times, fps);
    let animated = times.len() > 1;
    let clock_offset = span(cx, o, p.duration)?[0];
    let mut shapes = Vec::new();
    let mut dots = Vec::new();
    let mut places = Vec::new();
    let frames = if p.map.is_some() {
        let amounts = times.iter().map(|&t| p.amount(cx, t)).collect::<ResultOf<Vec<_>>>()?;
        let geometry_times = if moving_geometry { times.as_slice() } else { &times[..1] };
        let frames = geometry_times.iter().map(|&t| geometry(cx, o, ty, p, fs, t, dependent || moving_geometry)).collect::<ResultOf<Vec<_>>>()?;
        mapped_geometry(cx, p, frames, &amounts)?
    } else {
        times.iter().map(|&t| geometry(cx, o, ty, p, fs, t, dependent).map(|g| unmapped_geometry(p, g))).collect::<ResultOf<Vec<_>>>()?
    };
    for (t, g) in times.into_iter().zip(frames) {
        let local = t - clock_offset;
        shapes.push((local, g.svg.into()));
        if let Some(point) = g.point {
            places.push((local, v2(point[0] + 12.0, point[1] - 12.0)));
            dots.push((local, circle(point, 7.0).into()));
        } else {
            places.push((local, v2(0.0, 0.0)));
            dots.push((local, circle([f64::NAN; 2], 7.0).into()));
        }
    }
    let svg = keyed(cx, shapes, animated);
    let main = path(cx, o, ty, p, svg, ty == "Dot");
    let mut layers = vec![main];
    if ty == "Tangent" && get_bool(cx, o, "dot", false) {
        let svg = keyed(cx, dots, animated);
        let dot = path(cx, o, ty, p, svg, true);
        let name = format!("{} dot", get_text(cx, o, "name").unwrap_or_else(|| ty.into()));
        let name = obj::text(cx.vm, &name);
        obj::set(cx.vm, dot.as_object().unwrap(), "name", name);
        layers.push(dot);
    }
    if ty == "Dot" {
        let text_field = |field| given(cx, o, field).map(|v| obj::string(cx.vm, v).ok_or_else(|| bad(field, format!("`{field}` is a string: {field}: \"P\"")))).transpose();
        let tex = text_field("tex")?;
        let label_text = text_field("label")?;
        if let Some(text) = tex.clone().or(label_text) {
            let size = positive("label_size", get_num(cx, o, "label_size", p.label_size)?)?;
            let height = if tex.is_some() {
                crate::math::typeset(&text, "STIX Two Math", size * 4.0 / 3.0, true).map_err(|m| bad("tex", m))?.height
            } else { 0.0 };
            let at = keyed(cx, places, animated);
            let label = layer(cx, if tex.is_some() { "Math" } else { "Text" }, vec![
                (if tex.is_some() { "tex" } else { "text" }, text.into()), ("at", at.into()), ("size", size.into()), ("color", Lit::Color(p.color)),
            ]);
            copy_common(cx, o, label, ty);
            let name = format!("{} label", get_text(cx, o, "name").unwrap_or_else(|| ty.into()));
            let name = obj::text(cx.vm, &name);
            obj::set(cx.vm, label.as_object().unwrap(), "name", name);
            if tex.is_some() {
                let offset = obj::v2(cx.vm, 0.0, -height);
                // Math has a top-left origin. Offset the keyed position itself
                // with an inner group, keeping the label above the point.
                let children = array(cx, vec![label]);
                let inner = layer(cx, "Group", vec![("at", offset.into()), ("layers", children.into())]);
                layers.push(inner);
            } else {
                obj::set(cx.vm, label.as_object().unwrap(), "valign", obj::id("bottom"));
                layers.push(label);
            }
        }
    }
    if layers.len() == 1 { Ok(main) } else {
        let children = array(cx, layers);
        Ok(layer(cx, "Group", vec![("layers", children.into())]))
    }
}

/// Data samplers for Draw2D. No document state or per-frame cache is involved.
pub(crate) fn install(vm: &mut ScriptVm, module: ScriptObject) {
    super::point_map::install(vm, module);
    fn fail(vm: &mut ScriptVm, message: &str) -> ScriptValue { script_err_invalid_args!(vm.trap(), "{message}") }
    fn lists(vm: &mut ScriptVm, lines: Vec<Vec<Point>>) -> ScriptValue {
        let mut out = Vec::new();
        for line in lines {
            let points: Vec<ScriptValue> = line.into_iter().map(|[x, y]| obj::array(vm, [x.into(), y.into()])).collect();
            out.push(obj::array(vm, points));
        }
        obj::array(vm, out)
    }
    vm.add_method(module, id_lut!(plot_ticks), script_args!(lo = NIL, hi = NIL), |vm, args| {
        let (Some(lo), Some(hi)) = (obj::num(vm, obj::get(vm, args, "lo")), obj::num(vm, obj::get(vm, args, "hi"))) else { return fail(vm, "plot_ticks(lo, hi) needs finite numbers with lo < hi"); };
        if lo >= hi { return fail(vm, "plot_ticks(lo, hi) needs lo < hi"); }
        obj::array(vm, plotting::nice_ticks(lo, hi, 8).1.into_iter().map(ScriptValue::from))
    });
    vm.add_method(module, id_lut!(implicit_curve), script_args!(f = NIL, x0 = NIL, x1 = NIL, y0 = NIL, y1 = NIL, cells = 100.0), |vm, args| {
        let n = |k| obj::num(vm, obj::get(vm, args, k)).unwrap_or(f64::NAN);
        let bounds = Bounds { x: [n("x0"), n("x1")], y: [n("y0"), n("y1")] };
        let cells = n("cells").round();
        let f = obj::get(vm, args, "f");
        if !bounds.valid() || !cells.is_finite() || cells < 1.0 { return fail(vm, "implicit_curve(f, x0, x1, y0, y1, cells): ranges increase and cells is positive"); }
        if !obj::is_fn(vm, f) { return fail(vm, "`f` is a function: fn(x, y) { x * x + y * y - 4 }"); }
        let mut cx = Cx { vm, doc: [1000.0; 2], bpm: 120.0, duration: 1.0 };
        let mut error = None;
        let lines = plotting::marching_squares(|x, y| match call::<1>(&mut cx, f, &[x, y], 0.0, "f") {
            Ok(v) => v[0], Err(e) => { error = Some(e.1); f64::NAN }
        }, bounds, cells as usize);
        if let Some(e) = error { return fail(cx.vm, &e); }
        lists(cx.vm, lines.into_iter().map(|(mut points, closed)| { if closed { points.push(points[0]); } points }).collect())
    });
    vm.add_method(module, id_lut!(sample_curve), script_args!(f = NIL, x0 = NIL, x1 = NIL), |vm, args| {
        let (Some(x0), Some(x1)) = (obj::num(vm, obj::get(vm, args, "x0")), obj::num(vm, obj::get(vm, args, "x1"))) else { return fail(vm, "sample_curve(f, x0, x1) needs finite x0 < x1"); };
        if x0 >= x1 { return fail(vm, "sample_curve(f, x0, x1) needs x0 < x1"); }
        let f = obj::get(vm, args, "f");
        if !obj::is_fn(vm, f) { return fail(vm, "`f` is a function: fn(x) { x * x }"); }
        let mut cx = Cx { vm, doc: [1000.0; 2], bpm: 120.0, duration: 1.0 };
        let mut ys = Vec::new();
        for i in 0..=64 {
            match call::<1>(&mut cx, f, &[x0 + (x1 - x0) * i as f64 / 64.0], 0.0, "f") {
                Ok(v) if v[0].is_finite() => ys.push(v[0]), Ok(_) => {}, Err(e) => return fail(cx.vm, &e.1),
            }
        }
        if ys.is_empty() { return lists(cx.vm, Vec::new()); }
        let lo = ys.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let pad = ((hi - lo) * 0.05).max(1e-6);
        let p = PlotBox { data: Bounds { x: [x0, x1], y: [lo - pad, hi + pad] }, size: [1000.0; 2], ticks: [vec![], vec![]], steps: [1.0; 2], color: 0, label_size: 20.0, duration: 1.0,
            fps: 30.0, map: None, map_at: 1.0.into(), map_window: None };
        match sample_curve(&mut cx, f, [x0, x1], 0.0, &p, false, false, 0) { Ok(lines) => lists(cx.vm, lines), Err(e) => fail(cx.vm, &e.1) }
    });
}

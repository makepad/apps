//! Frame-level modifiers: `tear` (the layer rips in two along a seeded
//! jagged line and the halves fall away) and `droste` (the layer nested
//! inside itself, zooming in forever: a recursive zoom). Both lower to
//! ordinary masked and scaled Groups of copies of the layer.

use super::*;

/// The tear line, top to bottom: `steps + 1` points across a `w`×`h` box,
/// wandering about the middle by `jag` (a fraction of the width), seeded.
pub(crate) fn tear_line(w: f64, h: f64, jag: f64, seed: u32, steps: usize) -> Vec<[f64; 2]> {
    (0..=steps)
        .map(|k| {
            let v = k as f64 / steps as f64;
            // A slow wander plus a fine zigzag.
            let wander = (v * 3.1 + hash01(seed, 0, 1) * 6.28).sin() * 0.5 + (v * 7.3 + hash01(seed, 0, 2) * 6.28).sin() * 0.25;
            let zig = (hash01(seed, k, 3) - 0.5) * 0.9;
            [w * (0.5 + jag * (wander + zig)), h * v]
        })
        .collect()
}

/// The two halves' outlines (left, right) as polygons that share the line.
pub(crate) fn tear_halves(w: f64, h: f64, line: &[[f64; 2]]) -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
    // Past the box, so the halves still cover it when they turn.
    let m = w.max(h) * 0.05;
    let mut left = vec![[-m, -m], [line[0][0], -m]];
    left.extend(line.iter().copied());
    left.extend([[line[line.len() - 1][0], h + m], [-m, h + m]]);
    let mut right = vec![[w + m, -m], [line[0][0], -m]];
    right.extend(line.iter().copied());
    right.extend([[line[line.len() - 1][0], h + m], [w + m, h + m]]);
    (left, right)
}

fn poly(points: &[[f64; 2]], close: bool) -> String {
    let mut s = String::new();
    for (i, p) in points.iter().enumerate() {
        s.push_str(&format!("{} {:.1} {:.1} ", if i == 0 { "M" } else { "L" }, p[0], p[1]));
    }
    if close {
        s.push('Z');
    }
    s
}

fn hold(cx: &mut Cx, start: ScriptValue, before: f64, after: f64) -> ScriptValue {
    obj::keys(cx.vm, vec![(0.0.into(), before.into(), None), (start, after.into(), Some("hold"))])
}

/// `tear: {time: 1.0 seed: 3 jag: 0.04 fall: 1.0 edge: #f4efe4}` on a
/// layer: at `time` the layer (in frame space) rips in two along a seeded
/// jagged vertical line, each half with a paper-white fibre edge, and the
/// halves swing apart and fall out of frame over `fall` seconds.
pub(super) fn tear(cx: &mut Cx, inner: ScriptObject, spec: ScriptValue) -> Made {
    let o = settings(cx, spec, "tear", &["time", "seed", "jag", "fall", "edge"])?;
    if o.is_none() && spec.as_number().is_none() {
        return Err(("tear".into(), format!("`tear` is `{{time: 1.0 seed: 3}}`, not {}", kind(cx, spec))));
    }
    let time = if spec.as_number().is_some() {
        spec
    } else {
        match o.and_then(|o| obj::opt(cx.vm, o, "time")).filter(|t| !t.is_nil()) {
            Some(t) => t,
            None => return Err(("tear".into(), "`tear` needs its `time`: `tear: {time: 1.0}`".into())),
        }
    };
    let number = |cx: &Cx, k: &str, d: f64| -> Result<f64, (String, String)> {
        match o {
            Some(o) => get_num(cx, o, k, d),
            None => Ok(d),
        }
    };
    let seed = number(cx, "seed", 1.0)? as u32;
    let jag = number(cx, "jag", 0.04)?.clamp(0.0, 0.3);
    let fall = number(cx, "fall", 1.0)?.max(0.1);
    let edge = match o {
        Some(o) => get_color(cx, o, "edge", 0xf4efe4ff)?,
        None => 0xf4efe4ff,
    };
    let [w, h] = cx.doc;
    let line = tear_line(w, h, jag, seed, 36);
    let (left, right) = tear_halves(w, h, &line);
    let copy = obj::copy_without(cx.vm, inner, &["span"]);
    let whole_opacity = hold(cx, time, 1.0, 0.0);
    let whole_layers = array(cx, vec![copy.into()]);
    let mut layers = vec![layer(cx, "Group", vec![("name", "whole".into()), ("opacity", whole_opacity.into()), ("layers", whole_layers.into())])];
    for (side, shape, sign) in [("left", &left, -1.0), ("right", &right, 1.0)] {
        let motion = |cx: &mut Cx, to: f64, ease: &str| {
            let end = at_time(cx, time, fall);
            obj::keys(cx.vm, vec![(time, 0.0.into(), None), (end, to.into(), Some(ease))])
        };
        let opacity = hold(cx, time, 0.0, 1.0);
        let mx = motion(cx, sign * w * 0.12, "ease_in_quad");
        let my = motion(cx, h * 1.25, "ease_in_cubic");
        let mrot = motion(cx, sign * 14.0, "ease_in_quad");
        let mask = layer(cx, "Path", vec![("svg", poly(shape, true).into()), ("fill", Lit::Color(0xffffffff))]);
        let masked_layers = array(cx, vec![copy.into()]);
        let masked = layer(cx, "Group", vec![("mask", mask.into()), ("layers", masked_layers.into())]);
        let edge_line = layer(cx, "Path", vec![("svg", poly(&line, false).into()), ("stroke", Lit::Color(edge)), ("width", (w * 0.004).into()), ("join", Lit::Id("round"))]);
        let half_layers = array(cx, vec![masked, edge_line]);
        layers.push(layer(cx, "Group", vec![
            ("name", format!("{side} half").into()),
            ("size", v2(w, h)),
            // Each half turns about its outer bottom corner.
            ("anchor", v2(if sign < 0.0 { 0.0 } else { 1.0 }, 1.0)),
            ("opacity", opacity.into()),
            ("x", mx.into()),
            ("y", my.into()),
            ("rotate", mrot.into()),
            ("layers", half_layers.into()),
        ]));
    }
    Ok(wrapper(cx, inner, "tear", vec![], layers))
}

/// Copy `k`'s scale at `u` (0..1 through a period) of a Droste zoom with
/// nesting `scale`: `scale^(k - u)`, so each copy grows into the one
/// outside it and the loop is seamless.
pub(crate) fn droste_scale(scale: f64, k: usize, u: f64) -> f64 {
    scale.powf(k as f64 - u)
}

/// `droste: {depth: 5 scale: 0.4 period: 2.0 frame: #f0ebe0 center: vec2(0.5, 0.5)}`
/// on a layer: the layer nested inside itself `depth` times, each copy
/// `scale` of the one outside it, zooming in continuously (a copy grows to
/// the next size every `period` seconds, forever). `frame` outlines each
/// copy (the screen within the screen); `center` is the vanishing point.
pub(super) fn droste(cx: &mut Cx, inner: ScriptObject, spec: ScriptValue) -> Made {
    let o = settings(cx, spec, "droste", &["depth", "scale", "period", "frame", "center", "width"])?;
    if o.is_none() && spec.as_bool() != Some(true) {
        return Err(("droste".into(), format!("`droste` is `{{depth: 5 scale: 0.4 period: 2}}`, not {}", kind(cx, spec))));
    }
    let number = |cx: &Cx, k: &str, d: f64| -> Result<f64, (String, String)> {
        match o {
            Some(o) => get_num(cx, o, k, d),
            None => Ok(d),
        }
    };
    let depth = number(cx, "depth", 5.0)?.clamp(2.0, 12.0) as usize;
    let scale = number(cx, "scale", 0.4)?.clamp(0.05, 0.9);
    let period = number(cx, "period", 2.0)?.max(0.1);
    let center = match o {
        Some(o) => get_vec2(cx, o, "center", [0.5, 0.5])?,
        None => [0.5, 0.5],
    };
    let frame = o.and_then(|o| obj::opt(cx.vm, o, "frame")).and_then(|v| v.as_color());
    let [w, h] = cx.doc;
    let line = number(cx, "width", w * 0.003)?;
    let copy = obj::copy_without(cx.vm, inner, &["span"]);
    let mut layers = Vec::new();
    for k in 0..depth {
        // One period of exponential zoom as keys, looped.
        let steps = 12;
        let mut points = Vec::new();
        for i in 0..=steps {
            let u = i as f64 / steps as f64;
            let s = droste_scale(scale, k, u);
            points.push((ScriptValue::from(u * period), obj::v2(cx.vm, s, s), None));
        }
        let keyed = obj::keys(cx.vm, points);
        let zoom = obj::make(cx.vm, "Loop", &[("inner", keyed), ("period", period.into()), ("mode", obj::id("restart")), ("count", NIL)]);
        let mut shown = vec![ScriptValue::from(copy)];
        if let Some(c) = frame {
            // Thicker on the small copies so it reads at every size.
            shown.push(layer(cx, "Rect", vec![("size", v2(w, h)), ("stroke", Lit::Color(c)), ("width", (line / scale.powi(k as i32).max(0.05)).into())]));
        }
        let mask = layer(cx, "Rect", vec![("size", v2(w, h))]);
        let shown = array(cx, shown);
        layers.push(layer(cx, "Group", vec![
            ("name", format!("droste {k}").into()),
            ("size", v2(w, h)),
            ("anchor", v2(center[0], center[1])),
            ("scale", zoom.into()),
            ("mask", mask.into()),
            ("layers", shown.into()),
        ]));
    }
    Ok(wrapper(cx, inner, "droste", vec![], layers))
}

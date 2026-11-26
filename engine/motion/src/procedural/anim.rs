//! Animation sampling shared by load-time procedural lowerings.
//! Samples become ordinary keys; evaluation has no additional state.

use super::*;
use makepad_tween::named::Ease;

type Error = (String, String);
type ResultOf<T> = Result<T, Error>;

fn bad(field: &str, message: impl Into<String>) -> Error { (field.into(), message.into()) }

/// Sampling times are resolved at load. References needing external events cannot
/// be frozen here; say so instead of guessing a time.
pub(super) fn seconds(cx: &Cx, value: ScriptValue) -> ResultOf<f64> {
    if let Some(n) = obj::num(cx.vm, value).filter(|n| n.is_finite()) { return Ok(n); }
    if let (Some(o), Some(ty)) = (value.as_object(), obj::type_name(cx.vm, value)) {
        let n = |key| obj::num(cx.vm, obj::get(cx.vm, o, key));
        let beats = match ty {
            "Beat" | "Beats" => n("n"),
            "Bar" => n("n").map(|v| 4.0 * v + n("beat").unwrap_or(0.0)),
            "Downbeat" => n("n").map(|v| 4.0 * v),
            _ => None,
        };
        if let Some(beats) = beats.filter(|_| cx.bpm.is_finite() && cx.bpm > 0.0) {
            let t = beats * 60.0 / cx.bpm + n("offset").unwrap_or(0.0);
            if t.is_finite() { return Ok(t); }
        }
    }
    Err(bad("time", "sampled animation times use seconds, or beat(n)/beats(n)/bar(n)/downbeat(n) with the document bpm; use seconds for event references"))
}

fn ease_at(value: ScriptValue, u: f64, duration: f64) -> ResultOf<f64> {
    if value.is_nil() { return Ok(u); }
    let name = obj::ident(value).ok_or_else(|| bad("ease", "sampled parameter easing uses an id, e.g. @linear or @ease_in_out_cubic"))?;
    let ease = Ease::from_id(&name).ok_or_else(|| bad("ease", format!("unknown ease @{name}; use @linear or @ease_in_out_cubic")))?;
    Ok(ease.apply(u, duration))
}

pub(super) fn components<const N: usize>(cx: &Cx, value: ScriptValue) -> Option<[f64; N]> {
    let mut out = [0.0; N];
    if N == 1 {
        out[0] = value.as_number().or_else(|| obj::num(cx.vm, value))?;
    } else {
        let parts = obj::items(cx.vm, value)?;
        if parts.len() != N { return None; }
        for (slot, v) in out.iter_mut().zip(parts) { *slot = v.as_number().or_else(|| obj::num(cx.vm, v))?; }
    }
    Some(out)
}

fn key_points(cx: &Cx, o: ScriptObject) -> ResultOf<Vec<(f64, ScriptValue, ScriptValue)>> {
    let list = obj::items(cx.vm, obj::get(cx.vm, o, "points")).ok_or_else(|| bad("keys", "use keys([0, value], [1, value, @linear])"))?;
    let mut points = Vec::new();
    for p in list {
        let p = obj::items(cx.vm, p).filter(|p| p.len() == 2 || p.len() == 3).ok_or_else(|| bad("keys", "each key is [seconds, value] or [seconds, value, @ease]"))?;
        let t = seconds(cx, p[0])?;
        if points.last().is_some_and(|&(last, _, _)| t <= last) { return Err(bad("keys", "key times must increase; use seconds in [time, value]")); }
        points.push((t, p[1], p.get(2).copied().unwrap_or(NIL)));
    }
    if points.is_empty() { return Err(bad("keys", "keys needs at least one [seconds, value]")); }
    Ok(points)
}

pub(super) fn anim_components<const N: usize>(cx: &mut Cx, value: ScriptValue, t: f64) -> ResultOf<[f64; N]> {
    if let Some(v) = components(cx, value).filter(|v| v.iter().all(|n| n.is_finite())) { return Ok(v); }
    if obj::is_fn(cx.vm, value) {
        let result = cx.vm.call(value, &[t.into()]);
        return components(cx, result).filter(|v| v.iter().all(|n| n.is_finite())).ok_or_else(|| bad("value", format!("fn({t}) gave {}, expected {}", kind(cx, result), if N == 1 { "a finite number" } else { "vec2(x, y) with finite numbers" })));
    }
    // Read the object before borrowing the VM to evaluate any endpoint.
    match (value.as_object(), obj::type_name(cx.vm, value)) {
        (Some(o), Some("Tween")) => {
            let (a, b, start, end, ease) = (obj::get(cx.vm, o, "from"), obj::get(cx.vm, o, "to"), obj::get(cx.vm, o, "start"), obj::get(cx.vm, o, "end"), obj::get(cx.vm, o, "ease"));
            let (start, end) = (seconds(cx, start)?, seconds(cx, end)?);
            if end <= start { return Err(bad("tween", "tween end must be after start; use seconds")); }
            interpolate_anim(cx, [a, b], [start, end], ease, t)
        }
        (Some(o), Some("Keys")) => {
            let points = key_points(cx, o)?;
            for pair in points.windows(2) {
                if t < pair[1].0 { return interpolate_anim(cx, [pair[0].1, pair[1].1], [pair[0].0, pair[1].0], pair[1].2, t); }
            }
            anim_components(cx, points.last().unwrap().1, t)
        }
        _ => Err(bad("value", format!("expected {}, tween(...), keys(...) or fn(t) {{ ... }}, got {}", if N == 1 { "a number" } else { "vec2(x, y)" }, kind(cx, value)))),
    }
}

fn interpolate_anim<const N: usize>(cx: &mut Cx, values: [ScriptValue; 2], times: [f64; 2], ease: ScriptValue, t: f64) -> ResultOf<[f64; N]> {
    let a: [f64; N] = anim_components(cx, values[0], t)?;
    let b: [f64; N] = anim_components(cx, values[1], t)?;
    let eased = ease_at(ease, ((t - times[0]) / (times[1] - times[0])).clamp(0.0, 1.0), times[1] - times[0])?;
    let u = if t <= times[0] { 0.0 } else if t >= times[1] { 1.0 } else { eased };
    Ok(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * u))
}

/// The same parameter at any time, without state or function-arity inspection.
pub(super) fn anim_at(cx: &mut Cx, value: ScriptValue, t: f64) -> ResultOf<f64> { Ok(anim_components::<1>(cx, value, t)?[0]) }

pub(super) fn window(cx: &Cx, value: ScriptValue, duration: f64) -> ResultOf<Option<[f64; 2]>> {
    if obj::is_fn(cx.vm, value) { return Ok(Some([0.0, duration])); }
    match (value.as_object(), obj::type_name(cx.vm, value)) {
        (Some(o), Some("Tween")) => {
            let a = seconds(cx, obj::get(cx.vm, o, "start"))?;
            let b = seconds(cx, obj::get(cx.vm, o, "end"))?;
            if a >= b { return Err(bad("tween", "tween end must be after start; use seconds")); }
            Ok(Some([a, b]))
        }
        (Some(o), Some("Keys")) => {
            let points = key_points(cx, o)?;
            Ok(Some([points[0].0, points.last().unwrap().0]))
        }
        _ => Ok(None),
    }
}

pub(super) fn sample_times(window: Option<[f64; 2]>, fps: f64) -> Vec<f64> {
    let [start, end] = window.unwrap_or([0.0; 2]);
    let count = ((end - start) * fps).ceil() as usize;
    let mut times: Vec<_> = (0..=count).map(|i| (start + i as f64 / fps).min(end)).collect();
    times.dedup();
    times
}

pub(super) fn keyed(cx: &mut Cx, values: Vec<(f64, Lit)>, animated: bool) -> ScriptValue {
    if animated { keys(cx, 0.0.into(), values) } else { obj::lit(cx.vm, values.into_iter().next().unwrap().1) }
}


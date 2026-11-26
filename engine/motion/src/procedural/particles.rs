//! `ParticleText{text: "flow"}`: thousands of particles swirl in a vortex,
//! converge onto points inside the letters (coloured by where they land),
//! hold, then blow away on a wind. Every particle is a closed-form function
//! of its index, the seed and time (no integration), sampled into keys at
//! 20 per second, so any frame renders on its own.
//!
//! `stages: ["A gust of wind", {svg: "M…" outline: true}, "scattering"]`
//! morphs instead: the vortex settles on the first stage, then every
//! `hold` seconds the particles fly on seeded curves (staggered, `move`
//! seconds) to the next text or shape, and the wind takes them after the
//! last.

use super::*;

/// The whole look's timing (seconds after `start`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Flow {
    pub converge: f64,
    pub release: f64,
    pub centre: [f64; 2],
    pub swirl: f64,
    pub wind: [f64; 2],
}

/// Where particle `i` (landing on `target`, `downwind` 0..1 how far along
/// the wind it sits) is at `t`, and its opacity.
pub(crate) fn particle(flow: &Flow, seed: u32, i: usize, target: [f64; 2], downwind: f64, t: f64) -> ([f64; 2], f64) {
    let h = |k: u32| hash01(seed, i, k);
    let tau = std::f64::consts::TAU;
    // The vortex: an ellipse (a ring seen at an angle), inner orbits faster.
    let a0 = tau * h(1);
    let r0 = flow.swirl * (0.45 + 0.55 * h(2));
    let omega = 1.4 + 1.8 * (1.0 - h(2));
    let breathe = 1.0 + 0.07 * (3.0 * t + tau * h(3)).sin();
    let a = a0 + omega * t;
    let swirl = [flow.centre[0] + a.cos() * r0 * breathe, flow.centre[1] + a.sin() * r0 * breathe * 0.55];
    // Converging: each leaves its orbit at its own time and eases in.
    let arrive = flow.converge - 0.3 * h(4);
    let leave = arrive - 0.55;
    let u = ((t - leave) / (arrive - leave)).clamp(0.0, 1.0);
    let e = if u < 0.5 { 4.0 * u * u * u } else { 1.0 - (-2.0 * u + 2.0).powi(3) * 0.5 };
    let shimmer = flow.swirl * 0.004;
    let settled = [target[0] + shimmer * (7.0 * t + tau * h(5)).sin(), target[1] + shimmer * (6.0 * t + tau * h(6)).cos()];
    let mut p = [swirl[0] + (settled[0] - swirl[0]) * e, swirl[1] + (settled[1] - swirl[1]) * e];
    // Released: downwind particles first, blown with a curl of turbulence.
    let go = flow.release + 0.35 * (1.0 - downwind) + 0.08 * h(5);
    let mut opacity = (t / 0.25).clamp(0.0, 1.0);
    if t > go {
        let dt = t - go;
        let gust = 0.7 + 0.6 * h(3);
        let len = flow.wind[0].hypot(flow.wind[1]).max(1e-9);
        let perp = [-flow.wind[1] / len, flow.wind[0] / len];
        let curl = (dt * 5.0 + tau * h(1)).sin() * dt * flow.swirl * 0.18;
        p[0] += 0.5 * flow.wind[0] * dt * dt * gust + perp[0] * curl;
        p[1] += 0.5 * flow.wind[1] * dt * dt * gust + perp[1] * curl;
        opacity *= (1.0 - dt / 0.9).clamp(0.0, 1.0);
    }
    (p, opacity)
}

/// Particle `i` on a morph through `targets` (one point per stage,
/// absolute): the vortex into the first, then from each to the next on a
/// seeded curve (leaving `hold` seconds after arriving, staggered, `mv`
/// seconds a move), and the wind after the last. One stage is [`particle`].
pub(crate) fn staged(flow: &Flow, seed: u32, i: usize, targets: &[[f64; 2]], downwind: f64, hold: f64, mv: f64, t: f64) -> ([f64; 2], f64) {
    let n = targets.len();
    if n <= 1 {
        return particle(flow, seed, i, targets[0], downwind, t);
    }
    let (leave, arrive) = stage_times(flow.converge, hold, mv, seed, i, n);
    let still = Flow { release: f64::MAX, ..*flow };
    if t < leave[0] {
        return particle(&still, seed, i, targets[0], downwind, t);
    }
    for k in 1..n {
        if t < leave[k - 1] {
            // Holding on stage k - 1.
            return particle(&still, seed, i, targets[k - 1], downwind, t);
        }
        if t < arrive[k - 1] {
            let u = ((t - leave[k - 1]) / (arrive[k - 1] - leave[k - 1])).clamp(0.0, 1.0);
            let e = if u < 0.5 { 4.0 * u * u * u } else { 1.0 - (-2.0 * u + 2.0).powi(3) * 0.5 };
            let (a, b) = (targets[k - 1], targets[k]);
            let len = (b[0] - a[0]).hypot(b[1] - a[1]);
            let bend = (hash01(seed, i, 12 + k as u32) - 0.5) * 0.8;
            let c = [(a[0] + b[0]) * 0.5 - (b[1] - a[1]) / len.max(1e-9) * len * bend, (a[1] + b[1]) * 0.5 + (b[0] - a[0]) / len.max(1e-9) * len * bend];
            let q = |i: usize| (1.0 - e) * (1.0 - e) * a[i] + 2.0 * (1.0 - e) * e * c[i] + e * e * b[i];
            return ([q(0), q(1)], 1.0);
        }
    }
    particle(flow, seed, i, targets[n - 1], downwind, t)
}

/// When particle `i` leaves each stage and arrives at the next (`n - 1`
/// moves): the moves start `hold` after each stage's arrival, staggered by
/// up to a quarter of `mv`.
pub(crate) fn stage_times(converge: f64, hold: f64, mv: f64, seed: u32, i: usize, n: usize) -> (Vec<f64>, Vec<f64>) {
    let lag = hash01(seed, i, 11) * mv * 0.25;
    let leave: Vec<f64> = (1..n).map(|k| converge + (k - 1) as f64 * (hold + mv) + hold + lag).collect();
    let arrive = leave.iter().map(|l| l + mv * 0.75).collect();
    (leave, arrive)
}

/// A morph stage's targets: a text, or `{svg: "M…"}` / `{icon: "…"}` (points
/// inside the shape, or along it with `outline: true`), fitted to `fit`.
fn stage_points(cx: &Cx, stage: ScriptValue, i: usize, font: &str, weight: f64, italic: bool, size: f64, fit: f64, count: usize, seed: u32) -> Result<Vec<[f64; 2]>, (String, String)> {
    let field = format!("stages[{i}]");
    if let Some(text) = obj::string(cx.vm, stage) {
        return glyph_points(&text, font, weight, italic, size, count, seed).map_err(|m| (field, m));
    }
    match stage.as_object().filter(|o| !cx.vm.bx.heap_ref().is_fn(*o) && obj::items(cx.vm, stage).is_none()) {
        Some(o) => {
            let outline = get_bool(cx, o, "outline", false);
            let d = match (get_text(cx, o, "svg"), get_text(cx, o, "icon"), get_text(cx, o, "text")) {
                (Some(d), _, _) => d,
                (None, Some(name), _) => {
                    let markup = crate::icons::icon(&name).ok_or_else(|| (field.clone(), format!("no icon `{name}` (`makepad-motion icons` lists them)")))?;
                    crate::icons::svg_path_data(markup, 100.0)
                }
                (None, None, Some(text)) => return glyph_points(&text, font, weight, italic, get_num(cx, o, "size", size)?, count, seed).map_err(|m| (field, m)),
                _ => return Err((field, "a stage is a text, or `{svg: \"M…\"}` / `{icon: \"…\"}` (with `outline: true` for its line)".into())),
            };
            shape_points(&d, fit, count, seed, outline).map_err(|m| (field, m))
        }
        None => Err((field, format!("a stage is a text or `{{svg: …}}` / `{{icon: …}}`, not {}", kind(cx, stage)))),
    }
}

pub(super) fn lower(cx: &mut Cx, o: ScriptObject) -> Made {
    let listed = obj::opt(cx.vm, o, "stages");
    let word = get_text(cx, o, "text").filter(|t| !t.trim().is_empty());
    let stages: Vec<ScriptValue> = match (listed, word) {
        (Some(v), _) if obj::items(cx.vm, v).is_some_and(|items| !items.is_empty()) => obj::items(cx.vm, v).unwrap(),
        (Some(other), _) if !other.is_nil() => return Err(("stages".into(), format!("`stages` is a list of texts and shapes, not {}", kind(cx, other)))),
        (_, Some(word)) => vec![obj::text(cx.vm, &word)],
        _ => return Err(("text".into(), "a ParticleText needs its `text: \"flow\"` (or `stages: [\"…\", {svg: \"M…\"}, \"…\"]`)".into())),
    };
    let [w, h] = get_vec2(cx, o, "box", cx.doc)?;
    let face = get_face(cx, o, "font", "Playfair Display")?;
    let (font, weight, italic) = (face.family, face.weight, face.italic);
    let size = get_num(cx, o, "size", h * 0.42)?.max(4.0);
    let count = get_num(cx, o, "count", 2000.0)?.clamp(1.0, 8000.0) as usize;
    let radius = get_num(cx, o, "radius", h * 0.0032)?.max(0.3);
    let seed = get_num(cx, o, "seed", 1.0)?.max(0.0) as u32;
    let start = get_start(cx, o);
    let converge = get_num(cx, o, "converge", 0.9)?.max(0.4);
    let hold = get_num(cx, o, "hold", 0.6)?.max(0.0);
    let mv = get_num(cx, o, "move", 0.9)?.max(0.1);
    let last_arrival = converge + (stages.len() - 1) as f64 * (hold + mv) + mv * 0.25;
    let release = get_num(cx, o, "release", last_arrival + hold)?.max(converge);
    let wind = get_vec2(cx, o, "wind", [-w * 3.5, -h * 0.4])?;
    let swirl = get_num(cx, o, "swirl", w.min(h) * 0.42)?;
    let colors = get_colors(cx, o, "colors", &[0xff6a1aff, 0xfff1d6ff, 0xc6ff3aff])?;
    let fit = w.min(h) * 0.6;
    let per_stage = stages.iter().enumerate().map(|(k, st)| stage_points(cx, *st, k, font, weight, italic, size, fit, count, seed.wrapping_add(k as u32 * 101))).collect::<Result<Vec<_>, _>>()?;
    let targets = per_stage.last().unwrap().clone();

    let centre = [w * 0.5, h * 0.5];
    let flow = Flow { converge, release, centre, swirl, wind };
    let spans: Vec<(f64, f64)> = per_stage.iter().map(|ts| ts.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])))).collect();
    let tint_of = |k: usize, x: f64| {
        let (lo, hi) = spans[k];
        gradient(&colors, if hi > lo { (x - lo) / (hi - lo) } else { 0.5 })
    };
    let len = wind[0].hypot(wind[1]).max(1e-9);
    let along: Vec<f64> = targets.iter().map(|p| (p[0] * wind[0] + p[1] * wind[1]) / len).collect();
    let (lo, hi) = along.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    let end = release + 0.35 + 0.08 + 0.95;
    let steps = (end * 20.0).ceil() as usize;
    let mut particles = Vec::with_capacity(count);
    for i in 0..count {
        let path: Vec<[f64; 2]> = per_stage.iter().map(|ts| [centre[0] + ts[i][0], centre[1] + ts[i][1]]).collect();
        let downwind = if hi > lo { (along[i] - lo) / (hi - lo) } else { 0.5 };
        let r = radius * (0.6 + 0.8 * hash01(seed, i, 10));
        let mut at: Vec<(f64, Lit)> = Vec::with_capacity(steps + 1);
        let mut opacity = Vec::with_capacity(steps + 1);
        for k in 0..=steps {
            let t = end * k as f64 / steps as f64;
            let (p, o) = staged(&flow, seed, i, &path, downwind, hold, mv, t);
            at.push((t, v2(p[0] - r, p[1] - r)));
            opacity.push((t, o));
        }
        // Opacity keys only where it changes (and the ends of each hold).
        let same = |a: f64, b: f64| (a - b).abs() < 1e-6;
        let fade: Vec<(f64, Lit)> = (0..opacity.len())
            .filter(|&k| k == 0 || k + 1 == opacity.len() || !same(opacity[k].1, opacity[k - 1].1) || !same(opacity[k].1, opacity[k + 1].1))
            .map(|k| (opacity[k].0, opacity[k].1.into()))
            .collect();
        // Coloured by where it lands on each stage, changing on the way.
        let tint: Lit = if per_stage.len() == 1 {
            Lit::Color(tint_of(0, per_stage[0][i][0]))
        } else {
            let (leave, arrive) = stage_times(converge, hold, mv, seed, i, per_stage.len());
            let mut stops = vec![(0.0, Lit::Color(tint_of(0, per_stage[0][i][0])))];
            for k in 1..per_stage.len() {
                stops.push((leave[k - 1], Lit::Color(tint_of(k - 1, per_stage[k - 1][i][0]))));
                stops.push((arrive[k - 1], Lit::Color(tint_of(k, per_stage[k][i][0]))));
            }
            keys(cx, start, stops).into()
        };
        let at = keys(cx, start, at);
        let fade = keys(cx, start, fade);
        particles.push(layer(cx, "Ellipse", vec![
            ("size", v2(r * 2.0, r * 2.0)),
            ("at", at.into()),
            ("opacity", fade.into()),
            ("fill", tint),
        ]));
    }
    let mut extra: Vec<(&str, Lit)> = vec![("size", v2(w, h))];
    if obj::opt(cx.vm, o, "glow").and_then(|v| v.as_bool()) != Some(false) {
        let glow = layer(cx, "Effect", vec![("name", Lit::Id("glow")), ("threshold", 0.2.into()), ("radius", (h * 0.008).into()), ("intensity", 1.2.into())]);
        let effects = array(cx, vec![glow]);
        extra.push(("effects", effects.into()));
    }
    Ok(group(cx, o, particles, extra))
}

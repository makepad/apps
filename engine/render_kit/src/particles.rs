//! Deterministic particles: nothing is simulated step by step. Particle `i`
//! is born at a time fixed by its index and the rate, with random choices
//! drawn from a hash of (seed, i), and moves in closed form (gravity with
//! linear drag, plus a smooth per-particle wander), so the particles alive
//! at any `t` are computed directly and every frame renders on its own.

use crate::stack::ResolvedEffect;

/// A particle as drawn at one time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    /// Centre in pixels.
    pub pos: [f64; 2],
    pub velocity: [f64; 2],
    /// Diameter in pixels.
    pub size: f64,
    /// Straight RGBA, fade included in alpha.
    pub color: [f64; 4],
    /// Radians.
    pub rotation: f64,
    /// Birth point in pixels (a layer emitter keeps only births inside its
    /// alpha; the shader tests it there).
    pub birth: [f64; 2],
    /// Age over life, 0..1.
    pub age: f64,
    /// A stable identity (index, burst flag) for tests and tracing.
    pub id: u64,
}

/// SplitMix64: a fixed, well-mixed hash, the same on every platform.
fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Uniform in [0, 1) for (seed, particle, draw).
pub fn rand(seed: u64, i: u64, k: u64) -> f64 {
    (mix64(mix64(seed ^ mix64(i)) ^ k.wrapping_mul(0xd1b5_4a32_d192_ed03)) >> 11) as f64 / (1u64 << 53) as f64
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Emitter {
    Point,
    Line,
    Circle,
    Rect,
    Points,
    Layer,
}

/// The particles alive at `t` in a frame of `size` pixels.
pub fn particles_at(effect: &ResolvedEffect, t: f64, size: [f64; 2]) -> Vec<Particle> {
    let emitter = match effect.choice("emitter") {
        "line" => Emitter::Line,
        "circle" => Emitter::Circle,
        "rect" => Emitter::Rect,
        "points" => Emitter::Points,
        "layer" => Emitter::Layer,
        _ => Emitter::Point,
    };
    let seed = effect.num("seed").max(0.0) as u64;
    let rate = effect.num("rate");
    let life = effect.num("life").max(0.01);
    let life_var = effect.num("life_var");
    let longest = life * (1.0 + life_var);
    let start = effect.num("start");
    let end = effect.num("end").max(start);
    let mut out = Vec::new();
    // Continuous emission: particle i is born in [start + i/rate, start + (i+1)/rate).
    if rate > 0.0 && t >= start {
        let first = (((t - longest - start) * rate).floor() - 1.0).max(0.0) as u64;
        let last_birth = t.min(end);
        let last = ((last_birth - start) * rate).floor().max(0.0) as u64;
        for i in first..=last {
            let born = start + (i as f64 + rand(seed, i, 0) * 0.999) / rate;
            if born > t || born >= end {
                continue;
            }
            if let Some(p) = particle(effect, emitter, seed, i, born, t, life, life_var, size) {
                out.push(p);
            }
        }
    }
    // A burst: `burst` particles born together at `burst_time`.
    let burst = effect.num("burst").round() as u64;
    let at = effect.num("burst_time");
    if burst > 0 && t >= at && t < at + longest {
        for j in 0..burst {
            let i = (1u64 << 62) | j;
            if let Some(p) = particle(effect, emitter, seed, i, at, t, life, life_var, size) {
                out.push(p);
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn particle(effect: &ResolvedEffect, emitter: Emitter, seed: u64, i: u64, born: f64, t: f64, life: f64, life_var: f64, size: [f64; 2]) -> Option<Particle> {
    let r = |k: u64| rand(seed, i, k);
    let life = life * (1.0 - life_var * r(1));
    let age = t - born;
    if age < 0.0 || age >= life {
        return None;
    }
    let frac = age / life;
    let center = effect.vec2("position");
    let center = [center[0] * size[0], center[1] * size[1]];
    let extent = effect.vec2("size");
    let angle = effect.num("angle").to_radians();
    let (ca, sa) = (angle.cos(), angle.sin());
    let birth = match emitter {
        Emitter::Point => center,
        Emitter::Line => {
            let s = (r(2) - 0.5) * extent[0] * size[0];
            [center[0] + ca * s, center[1] + sa * s]
        }
        Emitter::Circle => {
            let radius = extent[0] * size[0] * r(2).sqrt();
            let a = r(3) * std::f64::consts::TAU;
            [center[0] + radius * a.cos(), center[1] + radius * a.sin()]
        }
        Emitter::Rect | Emitter::Layer => {
            let (x, y) = ((r(2) - 0.5) * extent[0] * size[0], (r(3) - 0.5) * extent[1] * size[1]);
            [center[0] + ca * x - sa * y, center[1] + sa * x + ca * y]
        }
        Emitter::Points => point_on_path(effect.points("points"), r(2)).unwrap_or(center),
    };
    let direction = (effect.num("direction") + (r(4) - 0.5) * effect.num("spread")).to_radians();
    let speed = effect.num("speed") * (1.0 - effect.num("speed_var") * r(5));
    let v0 = [direction.cos() * speed, direction.sin() * speed];
    let g = effect.vec2("gravity");
    let k = effect.num("drag");
    let (pos, velocity) = if k > 1e-6 {
        let e = (-k * age).exp();
        let term = |v: f64, g: f64| (g * age / k + (v - g / k) * (1.0 - e) / k, g / k + (v - g / k) * e);
        let (x, vx) = term(v0[0], g[0]);
        let (y, vy) = term(v0[1], g[1]);
        ([birth[0] + x, birth[1] + y], [vx, vy])
    } else {
        ([birth[0] + v0[0] * age + 0.5 * g[0] * age * age, birth[1] + v0[1] * age + 0.5 * g[1] * age * age], [v0[0] + g[0] * age, v0[1] + g[1] * age])
    };
    // Wander: two incommensurate sines per axis with random phases, eased in
    // from zero so a particle leaves its birth point cleanly.
    let turb = effect.num("turbulence");
    let pos = if turb > 0.0 {
        let ramp = age / (age + 0.5);
        let wave = |k: u64| {
            let (f1, f2) = (1.3 + r(k) * 1.7, 3.1 + r(k + 1) * 2.3);
            ((age * f1 + r(k + 2) * 6.283).sin() * 0.65 + (age * f2 + r(k + 3) * 6.283).sin() * 0.35) * turb * ramp
        };
        [pos[0] + wave(10), pos[1] + wave(20)]
    } else {
        pos
    };
    let lerp = |a: f64, b: f64| a + (b - a) * frac;
    let c0 = effect.color("color");
    let c1 = effect.color("color_end");
    let fade_in = effect.num("fade_in");
    let fade_out = effect.num("fade_out");
    let fade = (if fade_in > 0.0 { (frac / fade_in).min(1.0) } else { 1.0 }) * (if fade_out > 0.0 { ((1.0 - frac) / fade_out).min(1.0) } else { 1.0 });
    let color = [lerp(c0[0], c1[0]), lerp(c0[1], c1[1]), lerp(c0[2], c1[2]), lerp(c0[3], c1[3]) * fade];
    let spin = effect.num("spin").to_radians() * if r(6) < 0.5 { -1.0 } else { 1.0 };
    Some(Particle {
        pos,
        velocity,
        size: lerp(effect.num("particle_size"), effect.num("size_end")).max(0.0),
        color,
        rotation: r(7) * std::f64::consts::TAU + spin * age,
        birth,
        age: frac,
        id: i,
    })
}

/// The point at fraction `s` of a polyline's length.
pub fn point_on_path(points: &[[f64; 2]], s: f64) -> Option<[f64; 2]> {
    match points.len() {
        0 => return None,
        1 => return Some(points[0]),
        _ => {}
    }
    let lens: Vec<f64> = points.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).collect();
    let total: f64 = lens.iter().sum();
    if total <= 0.0 {
        return Some(points[0]);
    }
    let mut want = s.clamp(0.0, 1.0) * total;
    for (k, len) in lens.iter().enumerate() {
        if want <= *len || k == lens.len() - 1 {
            let u = if *len > 0.0 { (want / len).min(1.0) } else { 0.0 };
            let (a, b) = (points[k], points[k + 1]);
            return Some([a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u]);
        }
        want -= len;
    }
    points.last().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::Value;
    use crate::catalog::EffectKind;

    fn fountain() -> ResolvedEffect {
        ResolvedEffect::new(EffectKind::Particles).with("rate", Value::Num(100.0)).with("life", Value::Num(1.0)).with("life_var", Value::Num(0.0))
    }

    #[test]
    fn frames_are_pure_functions_of_t() {
        let effect = fountain().with("turbulence", Value::Num(30.0)).with("burst", Value::Num(20.0));
        let a = particles_at(&effect, 1.37, [640.0, 360.0]);
        // Visit other times first: nothing carries over.
        let _ = particles_at(&effect, 0.2, [640.0, 360.0]);
        let _ = particles_at(&effect, 3.0, [640.0, 360.0]);
        let b = particles_at(&effect, 1.37, [640.0, 360.0]);
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn rate_and_life_give_the_count() {
        // 100 per second living exactly 1 s: 100 alive once warmed up.
        let alive = particles_at(&fountain(), 5.0, [640.0, 360.0]).len();
        assert!((99..=101).contains(&alive), "{alive}");
        assert!(particles_at(&fountain(), -1.0, [640.0, 360.0]).is_empty());
        let stopped = fountain().with("end", Value::Num(2.0));
        assert!(particles_at(&stopped, 3.5, [640.0, 360.0]).is_empty());
        let burst = fountain().with("rate", Value::Num(0.0)).with("burst", Value::Num(50.0)).with("burst_time", Value::Num(1.0));
        assert_eq!(particles_at(&burst, 1.5, [640.0, 360.0]).len(), 50);
        assert!(particles_at(&burst, 0.9, [640.0, 360.0]).is_empty());
    }

    #[test]
    fn closed_form_motion_matches_integration() {
        let effect = fountain().with("speed_var", Value::Num(0.0)).with("spread", Value::Num(0.0)).with("drag", Value::Num(2.0)).with("turbulence", Value::Num(0.0));
        let p = particles_at(&effect, 3.0, [1000.0, 1000.0]).into_iter().min_by(|a, b| b.age.total_cmp(&a.age)).unwrap();
        let age = p.age * 1.0;
        // Integrate the same particle numerically from its birth.
        let (mut x, mut v) = ([p.birth[0], p.birth[1]], [0.0f64, -200.0f64]);
        let steps = 100_000;
        let dt = age / steps as f64;
        for _ in 0..steps {
            for k in 0..2 {
                let g = if k == 1 { 300.0 } else { 0.0 };
                v[k] += (g - 2.0 * v[k]) * dt;
                x[k] += v[k] * dt;
            }
        }
        assert!((x[0] - p.pos[0]).abs() < 0.05 && (x[1] - p.pos[1]).abs() < 0.05, "{x:?} vs {:?}", p.pos);
    }

    #[test]
    fn emitters_place_births() {
        let size = [1000.0, 500.0];
        let line = fountain().with("emitter", Value::Choice("line".into())).with("size", Value::Vec2([0.5, 0.0]));
        for p in particles_at(&line, 2.0, size) {
            assert!((p.birth[1] - 250.0).abs() < 1e-9 && p.birth[0] >= 250.0 && p.birth[0] <= 750.0);
        }
        let path = fountain().with("emitter", Value::Choice("points".into())).with("points", Value::Points(vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]));
        for p in particles_at(&path, 2.0, size) {
            assert!((p.birth[1] == 0.0 && p.birth[0] <= 100.0) || (p.birth[0] == 100.0 && p.birth[1] <= 100.0), "{:?}", p.birth);
        }
        assert_eq!(point_on_path(&[[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]], 0.75), Some([100.0, 50.0]));
    }

    #[test]
    fn the_hash_is_uniform_enough() {
        let n = 100_000;
        let mean: f64 = (0..n).map(|i| rand(7, i, 3)).sum::<f64>() / n as f64;
        assert!((mean - 0.5).abs() < 0.005, "{mean}");
        assert!((0..n).all(|i| (0.0..1.0).contains(&rand(1, i, 0))));
    }
}

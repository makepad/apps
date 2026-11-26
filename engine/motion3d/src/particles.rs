//! Deterministic particles: every particle's state at `t` is computed from
//! its birth in closed form, so a frame is a pure function of `t` (scrub,
//! render out of order, render segments in parallel — the same pixels).
//!
//! The system has `count` particle slots. Slot `i` first spawns at
//! `start + i / rate` (all at `start` when `rate` is 0: a burst). With
//! `looping`, slot `i` spawns again every `period = max(count / rate,
//! longest lifetime)` seconds, each spawn (a "generation") with fresh
//! randoms, so the emission stays at `rate` per second and never more than
//! `count` are alive; without it every slot spawns once.
//!
//! Motion from birth: `v0` from the emitter direction cone, gravity `g`
//! and linear drag `k` solved exactly (`v(a) = g/k + (v0 - g/k)·e^(-k·a)`),
//! plus turbulence integrated from birth with a fixed 1/60 s step through a
//! static noise field when `turbulence.amount > 0`, then `swirl` turns the
//! particle about the y axis by `swirl · age`.

use crate::layout::InstanceBase;
use makepad_geom3d::math::{hash64, noise3, rand01, rand11, DEG};
use makepad_geom3d::mesh::{add3, any_perpendicular, cross3, dot3, len3, normalize_or_up, scale3, sub3};
use std::f32::consts::TAU;

/// Where particles are born (in the particle node's space).
#[derive(Clone, Debug, PartialEq)]
pub enum Emitter {
    /// At the origin.
    Point,
    /// Inside a sphere of `radius`, or on it with `surface: true`.
    Sphere { radius: f32, surface: bool },
    /// Inside a centred box of `size`.
    Box { size: [f32; 3] },
    /// On a disc of `radius` in the XZ plane.
    Disc { radius: f32 },
    /// On a circle of `radius` in the XZ plane.
    Ring { radius: f32 },
    /// On the segment `a`..`b`.
    Line { a: [f32; 3], b: [f32; 3] },
    /// On the points passed to [`ParticleSystem::eval`] (a mesh surface or
    /// any layout); the origin when none are passed.
    Points,
}

/// Noise-field turbulence (`amount` world units per second of added
/// velocity, `frequency` cycles per world unit).
#[derive(Clone, Debug, PartialEq)]
pub struct Turbulence {
    pub amount: f32,
    pub frequency: f32,
    pub seed: u64,
}

impl Default for Turbulence {
    fn default() -> Self {
        Self { amount: 0.0, frequency: 1.0, seed: 1 }
    }
}

/// A particle system (see the module docs for the timing model).
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleSystem {
    /// Particle slots = the most alive at once. Default 1000; clamped to 1..=1_000_000.
    pub count: u32,
    /// Spawns per second; 0 = every slot at `start` (a burst). Default 200.
    pub rate: f32,
    /// Seconds before the first spawn. Default 0.
    pub start: f32,
    /// Seconds a particle lives. Default 2.
    pub lifetime: f32,
    /// Lifetime varies by ± this fraction (0..1). Default 0.25.
    pub lifetime_jitter: f32,
    pub emitter: Emitter,
    /// Launch direction (need not be unit); `[0,0,0]` = away from the
    /// emitter's centre (for `Points`: along each point's normal).
    /// Default up.
    pub direction: [f32; 3],
    /// Half-angle of the launch cone, degrees (0 = a ray, 180 = everywhere).
    /// Default 20.
    pub spread: f32,
    /// Launch speed, world units per second. Default 2.
    pub speed: f32,
    /// Speed varies by ± this fraction. Default 0.2.
    pub speed_jitter: f32,
    /// Acceleration, world units / s². Default `[0, -1, 0]`.
    pub gravity: [f32; 3],
    /// Linear drag, 1/s (velocity decays as e^(-drag·t) toward g/drag). Default 0.
    pub drag: f32,
    pub turbulence: Turbulence,
    /// Turn about the y axis, degrees per second of age. Default 0.
    pub swirl: f32,
    /// Size at birth and at death (world units). Defaults 0.05 → 0.0.
    pub size_start: f32,
    pub size_end: f32,
    /// Colour at birth and death (linear RGBA), used when `gradient` is empty.
    pub color_start: [f32; 4],
    pub color_end: [f32; 4],
    /// Colour over life: `(life 0..1, rgba)` stops, sorted by the system.
    pub gradient: Vec<(f32, [f32; 4])>,
    /// Alpha fades in over this fraction of life and out over the last
    /// `fade_out` fraction. Defaults 0.1 / 0.3.
    pub fade_in: f32,
    pub fade_out: f32,
    /// Sprite rotation speed, degrees per second (each particle also starts
    /// at a random angle). Default 0.
    pub spin: f32,
    pub seed: u64,
    /// Slots re-spawn every period (default true).
    pub looping: bool,
}

impl Default for ParticleSystem {
    fn default() -> Self {
        Self {
            count: 1000,
            rate: 200.0,
            start: 0.0,
            lifetime: 2.0,
            lifetime_jitter: 0.25,
            emitter: Emitter::Point,
            direction: [0.0, 1.0, 0.0],
            spread: 20.0,
            speed: 2.0,
            speed_jitter: 0.2,
            gravity: [0.0, -1.0, 0.0],
            drag: 0.0,
            turbulence: Turbulence::default(),
            swirl: 0.0,
            size_start: 0.05,
            size_end: 0.0,
            color_start: [1.0, 1.0, 1.0, 1.0],
            color_end: [1.0, 1.0, 1.0, 1.0],
            gradient: Vec::new(),
            fade_in: 0.1,
            fade_out: 0.3,
            spin: 0.0,
            seed: 1,
            looping: true,
        }
    }
}

/// One live particle at the evaluated time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub position: [f32; 3],
    /// World units per second (for velocity-stretched sprites and motion blur).
    pub velocity: [f32; 3],
    pub size: f32,
    /// Linear RGBA with the fades applied to alpha.
    pub color: [f32; 4],
    /// Seconds since this particle's spawn.
    pub age: f32,
    /// age / lifetime, 0..1.
    pub life01: f32,
    /// Sprite rotation, radians.
    pub rotation: f32,
    /// Slot index 0..count.
    pub id: u32,
    /// Spawn number of the slot (0 for the first).
    pub generation: u32,
}

const TURB_STEP: f32 = 1.0 / 60.0;

impl ParticleSystem {
    fn slots(&self) -> u32 {
        self.count.clamp(1, 1_000_000)
    }

    fn max_life(&self) -> f32 {
        self.lifetime.max(1e-4) * (1.0 + self.lifetime_jitter.clamp(0.0, 1.0))
    }

    /// Seconds between spawns of one slot when looping.
    pub fn period(&self) -> f32 {
        let emit = if self.rate > 0.0 { self.slots() as f32 / self.rate } else { 0.0 };
        emit.max(self.max_life())
    }

    /// The particles alive at `t` (seconds on the node's clock).
    /// `emitter_points` feed [`Emitter::Points`].
    pub fn eval(&self, t: f32, emitter_points: Option<&[InstanceBase]>) -> Vec<Particle> {
        let mut out = Vec::new();
        let rel0 = t - self.start;
        if rel0 < 0.0 || !rel0.is_finite() {
            return out;
        }
        let period = self.period();
        let points = emitter_points.filter(|p| !p.is_empty());
        let mut stops = self.gradient.clone();
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        for i in 0..self.slots() {
            let offset = if self.rate > 0.0 { i as f32 / self.rate } else { 0.0 };
            let rel = rel0 - offset;
            if rel < 0.0 {
                // Later slots are born later still.
                if self.rate > 0.0 {
                    break;
                }
                continue;
            }
            let generation = if self.looping { (rel / period).floor() as u32 } else { 0 };
            let age = rel - generation as f32 * period;
            let key = hash64(self.seed ^ hash64(((i as u64) << 32) | generation as u64));
            let life = self.lifetime.max(1e-4) * (1.0 + self.lifetime_jitter.clamp(0.0, 1.0) * rand11(key, 0));
            if age > life {
                continue;
            }
            out.push(self.particle(i, generation, key, age, life, points, &stops));
        }
        out
    }

    fn particle(&self, id: u32, generation: u32, key: u64, age: f32, life: f32, points: Option<&[InstanceBase]>, stops: &[(f32, [f32; 4])]) -> Particle {
        let r = |k: u64| rand01(key, k);
        // Spawn position and the radial direction the emitter implies.
        let (p0, radial) = match &self.emitter {
            Emitter::Point => ([0.0; 3], [0.0, 1.0, 0.0]),
            Emitter::Sphere { radius, surface } => {
                let d = unit_from(r(1), r(2));
                let s = if *surface { 1.0 } else { r(3).cbrt() };
                (scale3(d, radius * s), d)
            }
            Emitter::Box { size } => {
                let p = [(r(1) - 0.5) * size[0], (r(2) - 0.5) * size[1], (r(3) - 0.5) * size[2]];
                (p, normalize_or_up(p))
            }
            Emitter::Disc { radius } => {
                let a = TAU * r(1);
                let d = [a.cos(), 0.0, -a.sin()];
                (scale3(d, radius * r(2).sqrt()), d)
            }
            Emitter::Ring { radius } => {
                let a = TAU * r(1);
                let d = [a.cos(), 0.0, -a.sin()];
                (scale3(d, *radius), d)
            }
            Emitter::Line { a, b } => (add3(*a, scale3(sub3(*b, *a), r(1))), [0.0, 1.0, 0.0]),
            Emitter::Points => match points {
                Some(p) => {
                    let pick = &p[((r(1) * p.len() as f32) as usize).min(p.len() - 1)];
                    (pick.position, pick.normal)
                }
                None => ([0.0; 3], [0.0, 1.0, 0.0]),
            },
        };
        let axis = if dot3(self.direction, self.direction) < 1e-12 { radial } else { normalize_or_up(self.direction) };
        let dir = cone(axis, self.spread.clamp(0.0, 180.0) * DEG, r(4), r(5));
        let speed = self.speed * (1.0 + self.speed_jitter * rand11(key, 6));
        let v0 = scale3(dir, speed);
        let (mut p, mut v) = ballistic(p0, v0, self.gravity, self.drag, age);
        if self.turbulence.amount > 0.0 {
            // Integrate the turbulence displacement from birth on a fixed
            // step: the same steps for the same age, whatever was sampled before.
            let mut disp = [0.0; 3];
            let steps = (age / TURB_STEP).ceil() as u32;
            let mut w = [0.0; 3];
            for s in 0..steps {
                let a0 = s as f32 * TURB_STEP;
                let dt = (age - a0).min(TURB_STEP);
                let (base, _) = ballistic(p0, v0, self.gravity, self.drag, a0);
                w = turbulence(&self.turbulence, add3(base, disp));
                disp = add3(disp, scale3(w, dt));
            }
            p = add3(p, disp);
            v = add3(v, w);
        }
        if self.swirl != 0.0 {
            let ang = self.swirl * DEG * age;
            let (s, c) = ang.sin_cos();
            let rot = |q: [f32; 3]| [c * q[0] + s * q[2], q[1], -s * q[0] + c * q[2]];
            p = rot(p);
            v = rot(v);
            // Plus the tangential velocity of the turning frame.
            v = add3(v, cross3([0.0, self.swirl * DEG, 0.0], p));
        }
        let life01 = (age / life).clamp(0.0, 1.0);
        let mut color = if stops.is_empty() {
            lerp4(self.color_start, self.color_end, life01)
        } else {
            gradient_at(stops, life01)
        };
        let mut fade = 1.0;
        if self.fade_in > 0.0 {
            fade *= smoothstep(0.0, self.fade_in, life01);
        }
        if self.fade_out > 0.0 {
            fade *= smoothstep(0.0, self.fade_out, 1.0 - life01);
        }
        color[3] *= fade;
        Particle {
            position: p,
            velocity: v,
            size: self.size_start + (self.size_end - self.size_start) * life01,
            color,
            age,
            life01,
            rotation: TAU * r(7) + self.spin * DEG * age,
            id,
            generation,
        }
    }
}

/// Position and velocity after `a` seconds under gravity `g` and linear
/// drag `k` (exact).
pub fn ballistic(p0: [f32; 3], v0: [f32; 3], g: [f32; 3], k: f32, a: f32) -> ([f32; 3], [f32; 3]) {
    if k.abs() < 1e-6 {
        let p = add3(add3(p0, scale3(v0, a)), scale3(g, 0.5 * a * a));
        return (p, add3(v0, scale3(g, a)));
    }
    let e = (-k * a).exp();
    let term = scale3(g, 1.0 / k);
    let dv = sub3(v0, term);
    let p = add3(add3(p0, scale3(term, a)), scale3(dv, (1.0 - e) / k));
    (p, add3(term, scale3(dv, e)))
}

fn turbulence(t: &Turbulence, p: [f32; 3]) -> [f32; 3] {
    let q = scale3(p, t.frequency);
    [
        noise3(t.seed, q) * t.amount,
        noise3(t.seed.wrapping_add(101), add3(q, [17.3, 5.1, 9.7])) * t.amount,
        noise3(t.seed.wrapping_add(202), add3(q, [3.7, 29.9, 13.1])) * t.amount,
    ]
}

/// Uniform unit vector from two randoms.
fn unit_from(u: f32, v: f32) -> [f32; 3] {
    let z = 1.0 - 2.0 * u;
    let r = (1.0 - z * z).max(0.0).sqrt();
    let a = TAU * v;
    [r * a.cos(), r * a.sin(), z]
}

/// A direction uniformly in the cone of half-angle `half` about `axis`.
fn cone(axis: [f32; 3], half: f32, u: f32, v: f32) -> [f32; 3] {
    if half <= 0.0 {
        return axis;
    }
    let cos_max = half.cos();
    let z = 1.0 - u * (1.0 - cos_max);
    let r = (1.0 - z * z).max(0.0).sqrt();
    let a = TAU * v;
    let t = any_perpendicular(axis);
    let b = cross3(axis, t);
    let d = add3(add3(scale3(t, r * a.cos()), scale3(b, r * a.sin())), scale3(axis, z));
    let l = len3(d);
    scale3(d, 1.0 / l.max(1e-12))
}

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

/// Colour of sorted `(position, rgba)` stops at `x` (clamped at the ends).
pub fn gradient_at(stops: &[(f32, [f32; 4])], x: f32) -> [f32; 4] {
    if stops.is_empty() {
        return [1.0; 4];
    }
    if x <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        if x <= w[1].0 {
            let span = (w[1].0 - w[0].0).max(1e-6);
            return lerp4(w[0].1, w[1].1, (x - w[0].0) / span);
        }
    }
    stops[stops.len() - 1].1
}

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layout;

    #[test]
    fn nothing_before_start() {
        let s = ParticleSystem { start: 1.0, ..Default::default() };
        assert!(s.eval(0.99, None).is_empty());
        assert!(!s.eval(1.01, None).is_empty());
    }

    #[test]
    fn burst_spawns_every_slot_at_once() {
        let s = ParticleSystem { count: 250, rate: 0.0, lifetime_jitter: 0.0, looping: false, ..Default::default() };
        assert_eq!(s.eval(0.0, None).len(), 250);
        assert_eq!(s.eval(1.0, None).len(), 250);
        assert!(s.eval(2.01, None).is_empty());
    }

    #[test]
    fn history_independent() {
        let s = ParticleSystem {
            turbulence: Turbulence { amount: 1.5, frequency: 0.7, seed: 3 },
            swirl: 45.0,
            drag: 0.5,
            emitter: Emitter::Sphere { radius: 0.5, surface: false },
            ..Default::default()
        };
        let fresh = s.eval(2.37, None);
        let _ = s.eval(0.5, None);
        let _ = s.eval(5.0, None);
        assert_eq!(s.eval(2.37, None), fresh);
        assert_eq!(s.clone().eval(2.37, None), fresh);
        assert!(fresh.iter().all(|p| p.position.iter().all(|v| v.is_finite())));
    }

    #[test]
    fn gravity_parabola() {
        let g = [0.0, -9.81, 0.0];
        let s = ParticleSystem {
            count: 1,
            rate: 0.0,
            lifetime: 10.0,
            lifetime_jitter: 0.0,
            direction: [1.0, 1.0, 0.0],
            spread: 0.0,
            speed: 5.0,
            speed_jitter: 0.0,
            gravity: g,
            looping: false,
            ..Default::default()
        };
        let v0 = scale3(normalize_or_up([1.0, 1.0, 0.0]), 5.0);
        for t in [0.0, 0.3, 1.1, 2.5] {
            let p = s.eval(t, None)[0];
            let want = add3(scale3(v0, t), scale3(g, 0.5 * t * t));
            assert!(len3(sub3(p.position, want)) < 1e-4, "{t}: {:?} vs {want:?}", p.position);
            assert!(len3(sub3(p.velocity, add3(v0, scale3(g, t)))) < 1e-4);
        }
    }

    #[test]
    fn drag_matches_ode() {
        // Integrate numerically and compare with the closed form.
        let (g, k) = ([0.0, -3.0, 1.0], 0.8);
        let (mut p, mut v) = ([0.0; 3], [2.0, 4.0, 0.0]);
        let dt = 1e-4;
        for _ in 0..20000 {
            let a = sub3(g, scale3(v, k));
            v = add3(v, scale3(a, dt));
            p = add3(p, scale3(v, dt));
        }
        let (cp, cv) = ballistic([0.0; 3], [2.0, 4.0, 0.0], g, k, 2.0);
        assert!(len3(sub3(cp, p)) < 2e-3, "{cp:?} {p:?}");
        assert!(len3(sub3(cv, v)) < 2e-3);
    }

    #[test]
    fn looping_keeps_count_steady() {
        let s = ParticleSystem { count: 400, rate: 100.0, lifetime: 2.0, lifetime_jitter: 0.0, ..Default::default() };
        // period = max(4 s, 2 s) = 4: steady state ~ rate * lifetime = 200 alive.
        for t in [5.0, 7.3, 11.9, 40.2] {
            let n = s.eval(t, None).len() as i32;
            assert!((n - 200).abs() <= 2, "{t}: {n}");
        }
        // Capped by count: lifetime longer than count / rate.
        let c = ParticleSystem { count: 50, rate: 100.0, lifetime: 2.0, lifetime_jitter: 0.0, ..Default::default() };
        for t in [3.0, 9.1] {
            assert!(c.eval(t, None).len() <= 50);
        }
        // Generations differ.
        let a = s.eval(1.0, None).into_iter().find(|p| p.id == 10).unwrap();
        let b = s.eval(5.0, None).into_iter().find(|p| p.id == 10).unwrap();
        assert_eq!((a.generation, b.generation), (0, 1));
        assert_ne!(a.position, b.position);
    }

    #[test]
    fn seeds_are_deterministic_and_distinct() {
        let a = ParticleSystem { seed: 1, spread: 90.0, ..Default::default() };
        let b = ParticleSystem { seed: 2, spread: 90.0, ..Default::default() };
        assert_eq!(a.eval(1.5, None), a.eval(1.5, None));
        assert_ne!(a.eval(1.5, None)[5].position, b.eval(1.5, None)[5].position);
    }

    #[test]
    fn cone_spread_and_colours() {
        let s = ParticleSystem { spread: 30.0, gravity: [0.0; 3], color_start: [1.0, 0.0, 0.0, 1.0], color_end: [0.0, 0.0, 1.0, 1.0], fade_in: 0.0, fade_out: 0.0, ..Default::default() };
        for p in s.eval(1.0, None) {
            let d = normalize_or_up(p.velocity);
            assert!(d[1] >= (30.0 * DEG).cos() - 1e-4, "{d:?}");
            assert!((p.color[0] - (1.0 - p.life01)).abs() < 1e-4);
            assert_eq!(p.color[3], 1.0);
        }
        let stops: Vec<(f32, [f32; 4])> = vec![(1.0, [0.0, 1.0, 0.0, 1.0]), (0.0, [1.0, 0.0, 0.0, 1.0])];
        let mut sorted = stops.clone();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(gradient_at(&sorted, 0.5), [0.5, 0.5, 0.0, 1.0]);
    }

    #[test]
    fn point_emitter_uses_supplied_points() {
        let pts = Layout::Circle { count: 8, radius: 3.0 }.generate(None);
        let s = ParticleSystem { emitter: Emitter::Points, direction: [0.0; 3], spread: 0.0, gravity: [0.0; 3], speed: 1.0, speed_jitter: 0.0, ..Default::default() };
        for p in s.eval(0.5, Some(&pts)) {
            // Launched outward along the circle's normals: radius = 3 + age.
            let r = (p.position[0].powi(2) + p.position[2].powi(2)).sqrt();
            assert!((r - (3.0 + p.age)).abs() < 1e-3, "{r} {}", p.age);
        }
    }
}

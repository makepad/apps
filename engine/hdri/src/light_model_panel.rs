//! The panels' 3D models (light_model.rs): an LED panel (bare, diffused, or the nude COB in its reflector), a ring
//! light, a collapsible reflector and a gradient scrim on its frame. Each is built as light_model.rs's umbrella is: in
//! the face's own frame, box units (x and y the face's half sizes, z their mean), its emitting surface `Part::Cloth`
//! so head-on it is the flat face point for point, its housing, frame, yoke and stand dull solids.
//!
//! - An LED panel (RLS030-034): the face on the housing's front, the cloth of a diffused one (`baffle` from 0.8, the
//!   pillow RLS034 shows) bulging gently out of its frame; the housing as deep as a 26 cm panel's 4 cm, its back a
//!   heatsink's fins over a control strip with two knobs, a carry handle on top, the U of its yoke round it from
//!   the middle of its sides (each pivot a lock knob) down to the spigot and the stand.
//! - A nude COB (`ribs` 0, RLS037): the face's chip, holder (a single-chip head's with its two screw ears) and fan
//!   slots on the head's front at the bottom of its reflector, a cone from 0.7 of the radius to the rolled lip, its
//!   outside bare metal; behind it the Bowens ring and the round body with its cooling rings, in a yoke on its stand.
//! - A ring light (RLS035-036): the face on the ring's front, the hole open, its outer and inner walls rounding to
//!   a flat back, the bracket under it with its tilt knob and ball head on the stand.
//! - A reflector (RLS038-042): the cloth, a slight saddle as a sprung hoop holds it, the hoop in its binding round
//!   the back of the outline, its back the cover's plain black fabric (`Reflector::back`, the front's folds lit by the
//!   room); its holder arm behind it, a clip on the hoop at the top and the bottom, the swivel at its middle on the
//!   stand. A silver or gold cloth mirrors its source, so its hot zone moves with the view (`Sheen`).
//! - A gradient (RLS043-054): the scrim stretched over the front of its frame of round tubes, which its edges wrap,
//!   so head-on only the scrim shows; its back glows with what passes the cloth (`Modifier::back`); a grip head
//!   on the bottom tube on the stand.
use std::sync::Arc;

use makepad_draw::*;

use super::super::params::{ModifierKind, ModifierParams};
use super::super::modifier::{SINGLE_EAR_ANGLE, SINGLE_EAR_AT};
use super::super::reflector::Reflector;
use super::{Albedo, Built, Mesh, Modifier, Part};

/// An LED panel's housing depth (a 26 x 26 cm panel's 4 cm), its diffuser cloth's bulge at the middle (RLS034's
/// pillow), the heatsink's fins (how many, half thick, how far they stand off the back) and the yoke (its gap to
/// the housing, its bar's thickness and width, how far under the housing its bottom runs).
const LED_DEPTH: f32 = 0.3;
const LED_BULGE: f32 = 0.07;
const LED_FINS: usize = 11;
const LED_FIN_HALF: f32 = 0.012;
const LED_FIN_DEPTH: f32 = 0.1;
const YOKE_GAP: f32 = 0.07;
const YOKE_BAR: f32 = 0.04;
const YOKE_WIDTH: f32 = 0.14;
const YOKE_DROP: f32 = 0.2;
/// A light stand's top section and how far it reaches, box units of a small light's half size (a 9 mm tube).
const STAND_R: f32 = 0.07;
const STAND_REACH: f32 = 1.9;
/// A nude COB's reflector (RLS037, 15 cm): its base's radius (the face's reflector lights from 0.72), its depth, its
/// rolled lip's tube; the body behind it (radius, length) and its Bowens ring.
const COB_BASE: f32 = 0.7;
const COB_DEPTH: f32 = 0.55;
const COB_LIP: f32 = 0.022;
const COB_BODY_R: f32 = 0.72;
const COB_BODY: f32 = 1.5;
/// A ring light's body (RLS035, 20 cm): its depth and the rounding of its back edges.
const RING_DEPTH: f32 = 0.3;
const RING_ROUND: f32 = 0.08;
/// A reflector's hoop in its binding (a 4 mm spring steel band, its binding round it), the cloth's saddle (how far
/// its corners leave the plane, box units), the holder arm's bar and the stand's reach below.
const HOOP_R: f32 = 0.009;
const REFL_SADDLE: f32 = 0.03;
const HOLDER_R: f32 = 0.012;
const REFL_STAND_REACH: f32 = 1.4;
/// A gradient's frame: its tubes' radius (2.2 cm on a 1 m scrim).
const FRAME_R: f32 = 0.022;

/// Steps round a ring or a reflector's outline, and sides of the round parts.
const ROUND_STEPS: usize = 128;
const TUBE_SIDES: usize = 12;

/// The model of a panel kind, None for every other kind.
pub(super) fn build(kind: ModifierKind, params: &ModifierParams, modifier: &Modifier) -> Option<Built> {
    match kind {
        ModifierKind::LedPanel if params.ribs < 1 => Some(nude_cob(params)),
        ModifierKind::LedPanel => Some(led_panel(params, modifier)),
        ModifierKind::RingLight => Some(ring_light(params)),
        ModifierKind::Reflector => Some(reflector(modifier.reflector().map_or(2.8, Reflector::outline_power))),
        ModifierKind::Gradient => Some(gradient()),
        _ => None,
    }
}

/// A cylinder of radius `r` from `a` to `b`, `sides` round, closed at both ends when `caps`.
fn tube(mesh: &mut Mesh, a: Vec3f, b: Vec3f, r: f32, sides: usize, caps: bool, part: Part) {
    let axis = b - a;
    let len = axis.length();
    if !(len > 1.0e-6) {
        return;
    }
    let w = axis / len;
    let e1 = if w.x.abs() < 0.9 { Vec3f::cross(w, vec3f(1.0, 0.0, 0.0)) } else { Vec3f::cross(w, vec3f(0.0, 1.0, 0.0)) }.normalize();
    let e2 = Vec3f::cross(w, e1);
    let round = |k: usize| {
        let t = std::f32::consts::TAU * k as f32 / sides as f32;
        e1 * t.cos() + e2 * t.sin()
    };
    for k in 0..sides {
        let (n0, n1) = (round(k), round(k + 1));
        mesh.quad([a + n0 * r, a + n1 * r, b + n1 * r, b + n0 * r], [n0, n1, n1, n0], part);
        if caps {
            mesh.tri([a, a + n1 * r, a + n0 * r], [-w, -w, -w], part);
            mesh.tri([b, b + n0 * r, b + n1 * r], [w, w, w], part);
        }
    }
}

/// The surface the profile `(radius, z)` sweeps turning round the z axis, `ROUND_STEPS` round, its normals the
/// profile's own, `toward` +1 where the profile's left (seen walking along it in the r-z plane, r right, z up)
/// faces out of the part, -1 its right.
fn revolve(mesh: &mut Mesh, profile: &[(f32, f32)], toward: f32, part: Part) {
    let at = |k: usize| {
        let t = std::f32::consts::TAU * k as f32 / ROUND_STEPS as f32;
        (t.sin(), t.cos())
    };
    for w in profile.windows(2) {
        let ((r0, z0), (r1, z1)) = (w[0], w[1]);
        let (dr, dz) = (r1 - r0, z1 - z0);
        let len = (dr * dr + dz * dz).sqrt();
        if !(len > 1.0e-7) {
            continue;
        }
        // The profile's normal in the r-z plane.
        let (nr, nz) = (-dz / len * toward, dr / len * toward);
        for k in 0..ROUND_STEPS {
            let ((s0, c0), (s1, c1)) = (at(k), at(k + 1));
            let p = [vec3f(r0 * s0, r0 * c0, z0), vec3f(r1 * s0, r1 * c0, z1), vec3f(r1 * s1, r1 * c1, z1), vec3f(r0 * s1, r0 * c1, z0)];
            let (n0, n1) = (vec3f(nr * s0, nr * c0, nz), vec3f(nr * s1, nr * c1, nz));
            mesh.quad(p, [n0, n0, n1, n1], part);
        }
    }
}

/// A yoke round a head `half_w` wide each side of its axis, pivoting at the middle of its sides `z` deep: its arms
/// from just above the pivot down past `bottom`, the bar under the head, a lock knob at each pivot (the panel's
/// handles), the spigot block under the bar and the stand's top section from it.
fn yoke(mesh: &mut Mesh, half_w: f32, bottom: f32, z: f32, scale: f32) {
    let solid = Part::Solid(Albedo::Stand);
    let (x0, x1) = (half_w + YOKE_GAP * scale, half_w + (YOKE_GAP + YOKE_BAR) * scale);
    let (z0, z1) = (z - 0.5 * YOKE_WIDTH * scale, z + 0.5 * YOKE_WIDTH * scale);
    let low = bottom - YOKE_DROP * scale;
    for side in [-1.0f32, 1.0] {
        mesh.cuboid(vec3f((side * x0).min(side * x1), low, z0), vec3f((side * x0).max(side * x1), 0.1 * scale, z1), solid);
        // The lock knob, a ribbed disc on the arm's outside, and its lever.
        let knob = Part::Solid(Albedo::Head);
        tube(mesh, vec3f(side * x1, 0.0, z), vec3f(side * (x1 + 0.07 * scale), 0.0, z), 0.085 * scale, 16, true, knob);
        tube(mesh, vec3f(side * (x1 + 0.07 * scale), 0.0, z), vec3f(side * (x1 + 0.1 * scale), 0.0, z), 0.04 * scale, 10, true, knob);
    }
    mesh.cuboid(vec3f(-x1, low - YOKE_BAR * scale, z0), vec3f(x1, low, z1), solid);
    // The spigot's block and the stand's clamp.
    let stand = Part::Solid(Albedo::Stand);
    let top = low - YOKE_BAR * scale;
    mesh.cuboid(vec3f(-0.06 * scale, top - 0.16 * scale, z - 0.06 * scale), vec3f(0.06 * scale, top, z + 0.06 * scale), stand);
    let clamp = top - 0.16 * scale;
    mesh.cuboid(vec3f(-0.1 * scale, clamp - 0.22 * scale, z - 0.1 * scale), vec3f(0.1 * scale, clamp, z + 0.1 * scale), stand);
    tube(mesh, vec3f(0.0, clamp - 0.22 * scale, z), vec3f(0.0, clamp - STAND_REACH, z), STAND_R * scale, 10, true, stand);
}

/// An LED panel (`ribs` diodes across): the face on the housing's front, a diffuser cloth's pillow bulging out of
/// its frame (as `Modifier::led_diffuser` lays the cloth out: inset its frame's width inside the rim, its corners
/// rounded by 0.45 of the shorter half), the housing, the fins, the control strip, the carry handle and the yoke.
fn led_panel(params: &ModifierParams, modifier: &Modifier) -> Built {
    let mut mesh = Mesh::new();
    let cloth = smooth(0.8, 1.0, params.baffle);
    let half = modifier.half();
    let short = half.x.min(half.y);
    let aspect = vec2f(half.x / short, half.y / short);
    let inset = params.rim + 0.03 * cloth;
    let bulge = |x: f32, y: f32| {
        if cloth <= 0.0 {
            return 0.0;
        }
        let p = vec2f(x * aspect.x, y * aspect.y);
        let sdf = rounded_box(p, vec2f(aspect.x - inset, aspect.y - inset), 0.45 * cloth);
        // A pillow: its height grows as the square root of the depth in from the frame, flat at the middle.
        let t = (-sdf / (0.8 * aspect.x.min(aspect.y))).clamp(0.0, 1.0);
        LED_BULGE * cloth * (1.0 - (1.0 - t) * (1.0 - t)).sqrt()
    };
    let n = if cloth > 0.0 { 40 } else { 1 };
    let at = |i: usize, j: usize| {
        let (x, y) = (-1.0 + 2.0 * i as f32 / n as f32, -1.0 + 2.0 * j as f32 / n as f32);
        vec3f(x, y, bulge(x, y))
    };
    let normal = |p: Vec3f| {
        let e = 1.0e-3;
        let (hx, hy) = ((bulge(p.x + e, p.y) - bulge(p.x - e, p.y)) / (2.0 * e), (bulge(p.x, p.y + e) - bulge(p.x, p.y - e)) / (2.0 * e));
        vec3f(-hx, -hy, 1.0).normalize()
    };
    for j in 0..n {
        for i in 0..n {
            let p = [at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)];
            mesh.quad(p, p.map(normal), Part::Cloth);
        }
    }
    // The housing behind the face: its sides and its back.
    let housing = Part::Solid(Albedo::Head);
    let d = LED_DEPTH;
    mesh.cuboid(vec3f(-1.0, -1.0, -d), vec3f(1.0, 1.0, -0.002), housing);
    // The heatsink's fins down the back, over the control strip with its two knobs.
    for k in 0..LED_FINS {
        let x = -0.8 + 1.6 * k as f32 / (LED_FINS - 1) as f32;
        mesh.cuboid(vec3f(x - LED_FIN_HALF, -0.38, -d - LED_FIN_DEPTH), vec3f(x + LED_FIN_HALF, 0.86, -d), Part::Solid(Albedo::Metal));
    }
    mesh.cuboid(vec3f(-0.8, -0.86, -d - 0.05), vec3f(0.8, -0.5, -d), housing);
    for x in [-0.45f32, 0.45] {
        mesh.drum(x, -0.68, 0.075, -d - 0.12, -d - 0.05, Part::Solid(Albedo::Stand));
    }
    // The carry handle on top, its two posts and its grip.
    let zh = -0.5 * d;
    let stand = Part::Solid(Albedo::Stand);
    for x in [-0.42f32, 0.42] {
        tube(&mut mesh, vec3f(x, 1.0, zh), vec3f(x, 1.16, zh), 0.035, 8, true, stand);
    }
    tube(&mut mesh, vec3f(-0.46, 1.16, zh), vec3f(0.46, 1.16, zh), 0.04, 10, true, stand);
    yoke(&mut mesh, 1.0, -1.0, zh, 1.0);
    (mesh, false, None)
}

/// A nude COB in its reflector (`Modifier::nude_led_raw`): the head's front at the reflector's foot drawn as the
/// face (the chip, its holder a little proud, the fan slots), the reflector's cone up to the rolled lip at the
/// face's `rim`, its outside metal; the Bowens ring, the body with its cooling rings and its back, the yoke and
/// the stand.
fn nude_cob(params: &ModifierParams) -> Built {
    let mut mesh = Mesh::new();
    let lip = 1.0 - params.rim.max(0.01);
    let chip = if params.plate < 0.05 { 0.22 } else { params.plate.min(0.6) };
    // The head's front, and the chip's holder standing off it: a single-chip head's (`diffusion` above 0) wider,
    // screwed down by its two ears.
    revolve(&mut mesh, &[(0.0, -COB_DEPTH), (COB_BASE, -COB_DEPTH)], 1.0, Part::Cloth);
    let single = params.diffusion.unwrap_or(0.0) > 0.0;
    let holder = if single { 1.74 * chip } else { 1.36 * chip };
    revolve(&mut mesh, &[(0.0, -COB_DEPTH + 0.03), (holder, -COB_DEPTH + 0.03), (holder, -COB_DEPTH)], 1.0, Part::Face);
    if single {
        for a in [SINGLE_EAR_ANGLE, SINGLE_EAR_ANGLE + std::f32::consts::PI] {
            let c = vec2f(a.cos(), a.sin()) * (SINGLE_EAR_AT * chip);
            mesh.drum(c.x, c.y, 0.31 * chip, -COB_DEPTH, -COB_DEPTH + 0.035, Part::Face);
        }
    }
    // The reflector: its inside the face, then the rolled lip (the face's lip and dark rim head-on), its outside metal.
    revolve(&mut mesh, &[(COB_BASE, -COB_DEPTH), (lip, 0.0)], 1.0, Part::Cloth);
    let lip_r = 0.5 * (1.0 - lip).max(COB_LIP);
    let lip_c = (1.0 - lip_r, -lip_r);
    let roll: Vec<(f32, f32)> = (0..=12)
        .map(|k| {
            let t = std::f32::consts::PI * (0.5 + 1.25 * k as f32 / 12.0);
            (lip_c.0 + lip_r * t.cos(), lip_c.1 + lip_r * t.sin())
        })
        .collect();
    revolve(&mut mesh, &roll, -1.0, Part::Face);
    let metal = Part::Solid(Albedo::Metal);
    revolve(&mut mesh, &[(1.0, -lip_r), (COB_BASE + 0.03, -COB_DEPTH - 0.02)], 1.0, metal);
    // The Bowens ring, the body and its cooling rings, its back.
    let head = Part::Solid(Albedo::Head);
    let z_body = -COB_DEPTH - 0.02;
    mesh.drum(0.0, 0.0, COB_BODY_R + 0.07, z_body - 0.18, z_body, metal);
    mesh.drum(0.0, 0.0, COB_BODY_R, z_body - 0.18 - COB_BODY, z_body - 0.18, head);
    for k in 0..6 {
        let z = z_body - 0.45 - 0.14 * k as f32;
        mesh.drum(0.0, 0.0, COB_BODY_R + 0.03, z - 0.05, z, metal);
    }
    yoke(&mut mesh, COB_BODY_R, -COB_BODY_R, z_body - 0.18 - 0.55 * COB_BODY, 1.0);
    (mesh, false, None)
}

/// A ring light (`plate` its hole's radius, as the face's): the face on the ring's front, the hole open, its outer
/// and inner walls rounding to the flat back; the bracket under it, its tilt knob, the ball head and the stand.
fn ring_light(params: &ModifierParams) -> Built {
    let mut mesh = Mesh::new();
    let hole = params.plate.min(0.9);
    let steps = 8;
    let front: Vec<(f32, f32)> = (0..=steps).map(|k| (hole + (1.0 - hole) * k as f32 / steps as f32, 0.0)).collect();
    revolve(&mut mesh, &front, 1.0, Part::Cloth);
    let body = Part::Solid(Albedo::Head);
    let corner = |r: f32, out: f32| -> Vec<(f32, f32)> {
        (0..=6)
            .map(|k| {
                let t = std::f32::consts::FRAC_PI_2 * k as f32 / 6.0;
                (r - out * RING_ROUND * (1.0 - t.cos()), -(RING_DEPTH - RING_ROUND) - RING_ROUND * t.sin())
            })
            .collect()
    };
    // Outer wall and its rounding, the back, the inner rounding and wall, walking round the section.
    let mut section = vec![(1.0, 0.0)];
    section.extend(corner(1.0, 1.0));
    let mut inner = corner(hole, -1.0);
    inner.reverse();
    section.extend(inner);
    section.push((hole, 0.0));
    revolve(&mut mesh, &section, 1.0, body);
    // The bracket under the ring, its tilt knob, the ball head and the stand.
    let zc = -0.5 * RING_DEPTH;
    let stand = Part::Solid(Albedo::Stand);
    mesh.cuboid(vec3f(-0.1, -1.17, zc - 0.12), vec3f(0.1, -0.97, zc + 0.12), stand);
    tube(&mut mesh, vec3f(0.1, -1.07, zc), vec3f(0.19, -1.07, zc), 0.07, 16, true, body);
    mesh.cuboid(vec3f(-0.07, -1.37, zc - 0.07), vec3f(0.07, -1.17, zc + 0.07), stand);
    tube(&mut mesh, vec3f(0.0, -1.37, zc), vec3f(0.0, -1.37 - STAND_REACH, zc), 0.09, 10, true, stand);
    (mesh, false, None)
}

/// The reflector's outline (reflector.rs: |x|^n + |y|^n = 1 over the box, n 2.8 a panel's, 2 a round card's) at the
/// angle `t`, and its outward normal there in the box's plane.
fn outline_at(t: f32, n: f32) -> (Vec2f, Vec2f) {
    let e = 2.0 / n;
    let (s, c) = t.sin_cos();
    let p = vec2f(c.signum() * c.abs().powf(e), s.signum() * s.abs().powf(e));
    let g = vec2f(p.x.signum() * p.x.abs().powf(n - 1.0), p.y.signum() * p.y.abs().powf(n - 1.0));
    (p, if g.length() > 1.0e-9 { g.normalize() } else { vec2f(c, s) })
}

/// A collapsible reflector: the cloth over the outline in a slight saddle (its back `Modifier::back`'s plain
/// fabric), the hoop in its binding round the back of its edge, the holder arm behind it along its height with a
/// clip on the hoop at each end, the swivel at its middle and the stand from it.
fn reflector(power: f32) -> Built {
    let mut mesh = Mesh::new();
    let saddle = |x: f32, y: f32| REFL_SADDLE * x * y;
    let rings = 24;
    let point = |s: f32, k: usize| {
        let (p, _) = outline_at(std::f32::consts::TAU * k as f32 / ROUND_STEPS as f32, power);
        vec3f(p.x * s, p.y * s, saddle(p.x * s, p.y * s))
    };
    let normal = |p: Vec3f| vec3f(-REFL_SADDLE * p.y, -REFL_SADDLE * p.x, 1.0).normalize();
    for i in 0..rings {
        let (s0, s1) = (i as f32 / rings as f32, (i + 1) as f32 / rings as f32);
        for k in 0..ROUND_STEPS {
            let p = [point(s0, k), point(s1, k), point(s1, k + 1), point(s0, k + 1)];
            if i == 0 {
                mesh.tri([p[0], p[1], p[2]], [p[0], p[1], p[2]].map(normal), Part::Cloth);
            } else {
                mesh.quad(p, p.map(normal), Part::Cloth);
            }
        }
    }
    // The hoop in its binding, its outer edge on the outline, just behind the cloth.
    let binding = Part::Solid(Albedo::Stand);
    let hoop = |k: usize| {
        let (p, n) = outline_at(std::f32::consts::TAU * k as f32 / ROUND_STEPS as f32, power);
        let c = p - n * HOOP_R;
        (vec3f(c.x, c.y, saddle(p.x, p.y) - HOOP_R), vec3f(n.x, n.y, 0.0))
    };
    for k in 0..ROUND_STEPS {
        let ((c0, n0), (c1, n1)) = (hoop(k), hoop(k + 1));
        let z = vec3f(0.0, 0.0, 1.0);
        for j in 0..8 {
            let (a0, a1) = (std::f32::consts::TAU * j as f32 / 8.0, std::f32::consts::TAU * (j + 1) as f32 / 8.0);
            let round = |n: Vec3f, a: f32| n * a.cos() + z * a.sin();
            let p = [c0 + round(n0, a0) * HOOP_R, c1 + round(n1, a0) * HOOP_R, c1 + round(n1, a1) * HOOP_R, c0 + round(n0, a1) * HOOP_R];
            mesh.quad(p, [round(n0, a0), round(n1, a0), round(n1, a1), round(n0, a1)], binding);
        }
    }
    // The holder arm: its bar behind the cloth, a clip on the hoop at each end, the swivel and the stand.
    let (zb, metal, stand) = (-0.05, Part::Solid(Albedo::Metal), Part::Solid(Albedo::Stand));
    tube(&mut mesh, vec3f(0.0, -0.96, zb), vec3f(0.0, 0.96, zb), HOLDER_R, 10, true, metal);
    for side in [-1.0f32, 1.0] {
        mesh.cuboid(vec3f(-0.035, (side * 0.93).min(side * 1.02), zb - 0.025), vec3f(0.035, (side * 0.93).max(side * 1.02), -0.004), stand);
        tube(&mut mesh, vec3f(0.0, side * 0.96, zb), vec3f(0.0, side * 0.96, -0.004), 0.016, 8, true, stand);
    }
    mesh.cuboid(vec3f(-0.045, -0.06, zb - 0.07), vec3f(0.045, 0.06, zb - 0.01), stand);
    tube(&mut mesh, vec3f(0.0, 0.0, zb - 0.07), vec3f(0.0, 0.0, zb - 0.1), 0.04, 12, true, stand);
    tube(&mut mesh, vec3f(0.0, 0.0, zb - 0.1), vec3f(0.0, -1.0 - REFL_STAND_REACH, zb - 0.1), 0.02, 10, true, stand);
    (mesh, true, None)
}

/// A gradient scrim on its frame: the scrim over the frame's front (its back `Modifier::back`'s), the round tubes
/// behind its edges with a joint at each corner, a grip head on the bottom tube and the stand.
fn gradient() -> Built {
    let mut mesh = Mesh::new();
    mesh.quad([vec3f(-1.0, -1.0, 0.0), vec3f(1.0, -1.0, 0.0), vec3f(1.0, 1.0, 0.0), vec3f(-1.0, 1.0, 0.0)], [vec3f(0.0, 0.0, 1.0); 4], Part::Cloth);
    let (metal, stand) = (Part::Solid(Albedo::Metal), Part::Solid(Albedo::Stand));
    let (e, z) = (1.0 - FRAME_R, -FRAME_R - 0.002);
    for (a, b) in [((-e, -e), (e, -e)), ((e, -e), (e, e)), ((e, e), (-e, e)), ((-e, e), (-e, -e))] {
        tube(&mut mesh, vec3f(a.0, a.1, z), vec3f(b.0, b.1, z), FRAME_R, TUBE_SIDES, false, metal);
    }
    for (x, y) in [(-e, -e), (e, -e), (e, e), (-e, e)] {
        let c = 1.25 * FRAME_R;
        mesh.cuboid(vec3f(x - c, y - c, z - c), vec3f((x + c).min(1.0), (y + c).min(1.0), (z + c).min(-0.002)), stand);
    }
    // The grip head on the bottom tube, its knob, and the stand under it.
    mesh.cuboid(vec3f(-0.05, -e - 0.07, z - 0.06), vec3f(0.05, -e + 0.03, z - 0.012), stand);
    tube(&mut mesh, vec3f(0.05, -e - 0.03, z - 0.035), vec3f(0.1, -e - 0.03, z - 0.035), 0.03, 12, true, stand);
    tube(&mut mesh, vec3f(0.0, -e - 0.07, z - 0.035), vec3f(0.0, -e - 0.07 - 1.6, z - 0.035), 0.018, 10, true, stand);
    (mesh, true, None)
}

/// A reflector's metallised cloth mirrors its source (reflector.rs): seen along another ray than head-on, each
/// point shows the source where its folded normal turns that ray. The face carries the panel as it reads head-on,
/// so the model scales it by the panel seen along the ray over the panel seen head-on (`Reflector::seen`), the view
/// turned by as much as the ray turns from the head-on camera's ray to the same point: 1 head-on, and turned the
/// hot zone leaves (the source's image moves off the panel) while the glints of the folds that face it stay.
pub(super) struct Sheen {
    reflector: Reflector,
}

impl Sheen {
    /// The sheen of a light of `kind` with the face `modifier`: a reflector's, None for every other kind.
    pub(super) fn of(kind: ModifierKind, modifier: &Modifier) -> Option<Arc<Sheen>> {
        if kind != ModifierKind::Reflector {
            return None;
        }
        modifier.reflector().map(|r| Arc::new(Sheen { reflector: r.clone() }))
    }

    /// The panel at `p` (metres, the model's frame, `scale` metres per box unit) seen along `d` (unit) over the
    /// panel there seen from the camera head-on, `dist` metres in front of it.
    pub(super) fn ratio(&self, p: Vec3f, d: Vec3f, dist: f32, scale: Vec3f) -> f32 {
        let head_on = (p - vec3f(0.0, 0.0, dist)).normalize();
        let turn = d - head_on;
        if turn.length() < 1.0e-4 {
            return 1.0;
        }
        let view = (vec3f(0.0, 0.0, -1.0) + turn).normalize();
        self.reflector.seen(vec2f(p.x / scale.x, p.y / scale.y), view)
    }
}

use crate::patterns::smooth;

use crate::modifier::rounded_box;

#[cfg(test)]
mod tests {
    use super::super::{LightModel, ModelHit};
    use crate::image_light::ImageSet;
    use crate::light_fx::LightFx;
    use crate::modifier::apply_template;
    use crate::params::{LightParams, ModifierKind, SpaceParams};
    use makepad_draw::*;

    fn panel_light(kind: ModifierKind, ribs: Option<u32>, yaw: f32) -> LightParams {
        let mut light = LightParams::default();
        apply_template(&mut light, kind);
        if let (Some(ribs), Some(m)) = (ribs, light.modifier.as_mut()) {
            m.ribs = ribs;
            light.shape = "disc".into();
        }
        light.space = Some(SpaceParams { position_m: [0.0, 0.0, -2.5], yaw_deg: yaw, pitch_deg: 0.0, roll_deg: 0.0, width_m: 1.0, height_m: 1.0, beam: 0.0 });
        light
    }

    fn model(light: &LightParams) -> Option<LightModel> {
        let fx = LightFx::new(light, &ImageSet::default());
        LightModel::new(light, fx.modifier())
    }

    const KINDS: [(ModifierKind, Option<u32>); 6] = [
        (ModifierKind::LedPanel, None),
        (ModifierKind::LedPanel, Some(0)),
        (ModifierKind::RingLight, None),
        (ModifierKind::Reflector, None),
        (ModifierKind::Reflector, Some(1)),
        (ModifierKind::Gradient, None),
    ];

    /// Each panel kind in 3D has a model; head-on a ray through its face meets it at the very point of the flat
    /// face it travels along, at the mirror 1; a ray well past it meets nothing; in 2D it has none.
    #[test]
    fn head_on_a_panel_is_its_flat_face() {
        for (kind, ribs) in KINDS {
            let mut light = panel_light(kind, ribs, 0.0);
            if kind == ModifierKind::Reflector {
                if let (Some(ribs), Some(m)) = (ribs, light.modifier.as_mut()) {
                    m.ribs = ribs;
                }
                light.shape = "rect".into();
            }
            let m = model(&light).unwrap_or_else(|| panic!("{kind:?} {ribs:?}: no model"));
            assert!(m.triangles() > 50 && m.triangles() < 60000, "{kind:?}: {}", m.triangles());
            // a quarter of the way out, up and to the right: the face (a ring's ring, a COB's reflector)
            let p = if kind == ModifierKind::RingLight { vec3f(0.27, 0.2, -2.5) } else { vec3f(0.12, 0.1, -2.5) };
            let dir = p.normalize();
            match m.cast(dir) {
                Some(ModelHit::Face { at, mirror }) => {
                    assert!((at - dir).length() < 2.0e-5, "{kind:?} {ribs:?}: {at:?} for {dir:?}");
                    assert!((mirror - 1.0).abs() < 1.0e-3, "{kind:?} {ribs:?}: mirror {mirror}");
                }
                other => panic!("{kind:?} {ribs:?}: {other:?}"),
            }
            assert!(m.cast(vec3f(1.6, 0.0, -2.5).normalize()).is_none(), "{kind:?}: past the face");
            let mut flat = light.clone();
            flat.space = None;
            assert!(model(&flat).is_none(), "{kind:?} in 2D");
        }
    }

    /// From behind, a panel shows its housing or its back: a solid, or a reflector's and a scrim's back.
    #[test]
    fn from_behind_a_panel_shows_its_back() {
        for (kind, ribs) in KINDS {
            let mut light = panel_light(kind, ribs, 180.0);
            if kind == ModifierKind::Reflector {
                light.shape = "rect".into();
                if let (Some(ribs), Some(m)) = (ribs, light.modifier.as_mut()) {
                    m.ribs = ribs;
                }
            }
            let m = model(&light).unwrap();
            let dir = if kind == ModifierKind::RingLight { vec3f(-0.27, 0.22, -2.5) } else { vec3f(-0.15, 0.25, -2.5) }.normalize();
            match (kind, m.cast(dir)) {
                (ModifierKind::Reflector | ModifierKind::Gradient, Some(ModelHit::Back { .. })) => {}
                (_, Some(ModelHit::Solid { level })) => assert!(level > 0.0 && level < 0.1, "{kind:?}: {level}"),
                other => panic!("{other:?}"),
            }
        }
    }

    /// Turned 45 degrees, a silver reflector's source image moves: somewhere on the cloth the sheen is well off 1;
    /// a ring light's hole is open.
    #[test]
    fn a_silver_reflectors_image_moves_and_a_ring_is_open() {
        let mut light = panel_light(ModifierKind::Reflector, None, 45.0);
        light.shape = "rect".into();
        light.modifier.as_mut().unwrap().ribs = 1;
        let m = model(&light).unwrap();
        let (mut lo, mut hi) = (f32::INFINITY, 0.0f32);
        for i in -15..=15 {
            for j in -15..=15 {
                if let Some(ModelHit::Face { mirror, .. }) = m.cast(vec3f(i as f32 * 0.012, j as f32 * 0.016, -2.5).normalize()) {
                    lo = lo.min(mirror);
                    hi = hi.max(mirror);
                }
            }
        }
        assert!(lo < 0.7 || hi > 1.5, "the sheen stays at {lo}..{hi}");
        let ring = model(&panel_light(ModifierKind::RingLight, None, 0.0)).unwrap();
        assert!(ring.cast(vec3f(0.0, 0.0, -1.0)).is_none(), "the ring's hole");
    }
}

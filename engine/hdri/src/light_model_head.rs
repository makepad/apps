//! The heads and dishes as small 3D models (light_model.rs): a bare flash head (`bulb`) with and without its
//! reflector bowl, a beauty dish bare and with its sock, and a fresnel spot.
//!
//! Each is built as light_model.rs builds an umbrella: in the face's own frame (x to the viewer's right, y up, z out
//! of its front), box units with the face's rim at radius 1, the parts that show the face (`Part::Cloth`) exactly
//! where the camera head-on sees the flat face, everything round and behind them a solid. What faces the camera:
//! - a bare head: its front, flat, the face's whole disc; its body behind it;
//! - a head in a reflector: the head's front at the bowl's throat and the spun bowl round it out to the rolled lip,
//!   the bowl's inside a mirror (the face's ring of the head's image moves with the view);
//! - a beauty dish: its bowl out to the rolled lip, a mirror too, and the deflector plate in front of the flash, a
//!   dull solid on three rods (the face under it, `Modifier::factor_bare`, the bowl the plate hides head-on);
//!   with its sock, the sock's taut front and where it wraps over the lip;
//! - a fresnel: the lens a little inside the barrel's front ring.
//! Behind: the bowl's spun outside (`Albedo::Spun`), the head's body, its yoke, the block and spigot, a stand's top
//! section; a fresnel's housing with its cooling fins, its focus knob and its yoke.
use std::f32::consts::{PI, TAU};

use makepad_draw::*;

use super::{Albedo, Built, Mesh, Part, Turn};
use crate::modifier::{bulb_glass, Modifier, BULB_REFLECTOR_BELOW, DOME_R, EDGE, GLOBE_R, LENS_DISC, LENS_R, SPOT_CAP_R, SPOT_COIL_GAP, SPOT_COIL_R, SPOT_COIL_W, SPOT_MIDDLE, TUBE_GAP, TUBE_GLASS};
use crate::params::ModifierParams;

/// Sides round a bowl, a lip, a head's body and a fresnel's housing: a bowl a metre off spans some hundreds of
/// texels of an 8K map, 64 sides leave its outline under half a texel off round.
const ROUND: usize = 64;
/// Steps from a bowl's throat to its lip.
const BOWL_STEPS: usize = 40;
/// A spun bowl's wall, box units.
const WALL: f32 = 0.012;
/// How far below its face a light's stand reaches, metres: its top section.
const STAND_DROP_M: f32 = 1.0;
/// A stand's tube, radius in metres.
const STAND_R_M: f32 = 0.013;
/// A bare head's glass, in its tube ring's units (D): how far the flash tube's glass reaches past the face's
/// (the clip round its bottom, 0.18, and the soft edge), how far its glass stands off the plate, the modelling
/// lamp's dome's and the big globe's heights over their radii, the sockets the tube's ends stand in (round the
/// face's lit lip at 0.225) and how far they stand up.
const TUBE_PAST: f32 = 0.18 - TUBE_GLASS + 0.004;
const TUBE_OFF: f32 = 0.06;
const DOME_TALL: f32 = 0.95;
const GLOBE_TALL: f32 = 0.8;
const SOCKET_R: f32 = 0.235;
const SOCKET_UP: f32 = 0.08;
/// A clear-glass reflector spot bulb (an R80, 80 mm across), box units: its bowl's depth (a spherical cap to the
/// rim) and its neck's radius; how far in front of the neck's throat its filament's C and its capsule's front stand;
/// where its mirror starts (the face's dark neck inside it); how far back its glass reaches; its screw base (an E27's
/// radius, length and threads); how far up its cord runs.
const SPOT_BOWL_DEPTH: f32 = 0.9;
const SPOT_NECK: f32 = 0.56;
const SPOT_COIL_DEPTH: f32 = 0.3;
const SPOT_CAP_DEPTH: f32 = 0.38;
const SPOT_MIRROR_FROM: f32 = 0.625;
const SPOT_BACK: f32 = 1.55;
const SPOT_BASE_R: f32 = 0.34;
const SPOT_BASE_LEN: f32 = 0.6;
const SPOT_THREADS: usize = 5;
const SPOT_CORD: f32 = 4.0;
/// Steps along a flash tube and round its glass.
const TUBE_STEPS: usize = 96;
const TUBE_SIDES: usize = 16;
/// A beauty dish: its bowl's depth and its throat (the head's mount), box units; how far in front of the bowl's
/// bottom its plate stands; a head's body behind it (the dish's 41 cm over a 12 cm head).
const DISH_DEPTH: f32 = 0.6;
const DISH_THROAT: f32 = 0.15;
const DISH_PLATE_Z: f32 = -0.3;
const DISH_HEAD_R: f32 = 0.3;
/// How far a beauty dish's sock bows out in front of its lip, and the round it makes over the lip, box units.
const SOCK_BOW: f32 = 0.035;
const SOCK_WRAP: f32 = 0.045;
/// A fresnel: how far its lens's back sits inside the barrel's front ring, how far its middle disc bulges and each
/// step's riser stands (an Arri Junior's lens, 11 cm across, steps of about 2 mm), the barrel's length, the
/// housing's radius and length behind it, its fins (radius, thickness, how many).
const LENS_INSET: f32 = 0.14;
const LENS_DOME: f32 = 0.035;
const LENS_STEP: f32 = 0.025;
const BARREL_LEN: f32 = 0.45;
const HOUSING_R: f32 = 1.3;
const HOUSING_LEN: f32 = 2.1;
const FIN_R: f32 = 1.5;
const FIN_THICK: f32 = 0.035;
const FINS: usize = 7;

/// A surface of revolution round the z axis through the profile `prof` ((radius, z) points), `sides` round. Its
/// normals are the profile's (-dz, d radius) turned round, so a profile running out and forward faces the axis and
/// the front, one running back along a wall faces out; `flip` turns them.
fn revolve(mesh: &mut Mesh, prof: &[(f32, f32)], flip: bool, part: Part, sides: usize) {
    let n = prof.len();
    if n < 2 {
        return;
    }
    let normals: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let (a, b) = (prof[i.saturating_sub(1)], prof[(i + 1).min(n - 1)]);
            let (dr, dz) = (b.0 - a.0, b.1 - a.1);
            let len = (dr * dr + dz * dz).sqrt().max(1.0e-9);
            let s = if flip { -1.0 } else { 1.0 };
            (-dz / len * s, dr / len * s)
        })
        .collect();
    // Where the profile meets the axis its triangles all share one corner, which a ray straight along the axis can
    // slip between: a small square across the axis there, in the same plane, closes it.
    for j in [0, n - 1] {
        if prof[j].0 == 0.0 {
            let (z, e) = (prof[j].1, 0.01 * prof[if j == 0 { 1 } else { n - 2 }].0.max(0.0));
            let nz = vec3f(0.0, 0.0, normals[j].1.signum());
            mesh.quad([vec3f(-e, -e, z), vec3f(e, -e, z), vec3f(e, e, z), vec3f(-e, e, z)], [nz; 4], part);
        }
    }
    for k in 0..sides {
        let (a0, a1) = (TAU * k as f32 / sides as f32, TAU * (k + 1) as f32 / sides as f32);
        let (s0, c0, s1, c1) = (a0.sin(), a0.cos(), a1.sin(), a1.cos());
        let p = |j: usize, s: f32, c: f32| vec3f(prof[j].0 * s, prof[j].0 * c, prof[j].1);
        let q = |j: usize, s: f32, c: f32| vec3f(normals[j].0 * s, normals[j].0 * c, normals[j].1);
        for i in 0..n - 1 {
            mesh.quad([p(i, s0, c0), p(i + 1, s0, c0), p(i + 1, s1, c1), p(i, s1, c1)], [q(i, s0, c0), q(i + 1, s0, c0), q(i + 1, s1, c1), q(i, s1, c1)], part);
        }
    }
}

/// A ring of tube round the z axis: its middle `r` out at depth `z`, `thick` its radius.
fn torus(mesh: &mut Mesh, r: f32, z: f32, thick: f32, part: Part) {
    let prof: Vec<(f32, f32)> = (0..=16).map(|k| {
        let t = TAU * k as f32 / 16.0 - 0.5 * PI;
        (r + thick * t.cos(), z + thick * t.sin())
    }).collect();
    // the circle runs round counter-clockwise in (radius, z): its (-dz, dr) faces in, so flip
    revolve(mesh, &prof, true, part, ROUND);
}

/// A parabolic bowl from its throat `throat` out at depth `-depth` to its lip `lip` out at depth 0, (radius, z).
fn bowl_profile(throat: f32, lip: f32, depth: f32) -> Vec<(f32, f32)> {
    (0..=BOWL_STEPS)
        .map(|i| {
            let rho = throat + (lip - throat) * i as f32 / BOWL_STEPS as f32;
            (rho, -depth * (lip * lip - rho * rho) / (lip * lip - throat * throat).max(1.0e-6))
        })
        .collect()
}

/// A spun metal bowl: its inside the face (`Part::Cloth`) from `throat` to `lip`, its outside a wall `WALL` behind
/// it (`Albedo::Spun`), and the rolled lip round it out to radius 1 (the face's own lip at its place).
fn spun_bowl(mesh: &mut Mesh, throat: f32, lip: f32, depth: f32) {
    let inside = bowl_profile(throat, lip, depth);
    revolve(mesh, &inside, false, Part::Cloth, ROUND);
    let n = inside.len();
    let outside: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let (a, b) = (inside[i.saturating_sub(1)], inside[(i + 1).min(n - 1)]);
            let (dr, dz) = (b.0 - a.0, b.1 - a.1);
            let len = (dr * dr + dz * dz).sqrt().max(1.0e-9);
            (inside[i].0 + WALL * dz / len, inside[i].1 - WALL * dr / len)
        })
        .collect();
    revolve(mesh, &outside, true, Part::Solid(Albedo::Spun), ROUND);
    let roll = 0.5 * (1.0 - lip);
    torus(mesh, lip + roll, 0.0, roll, Part::Cloth);
}

/// A tube bent round a line along z through `mid` from angle `a0` to `a1` (from up toward +x, as the face's angles
/// run): its centre line `r` out at `mid`'s depth, `thick` its radius, open at its ends.
fn tube_arc(mesh: &mut Mesh, mid: Vec3f, r: f32, thick: f32, a0: f32, a1: f32, part: Part) {
    let at = |i: usize, j: usize| {
        let a = a0 + (a1 - a0) * i as f32 / TUBE_STEPS as f32;
        let t = TAU * j as f32 / TUBE_SIDES as f32;
        let out = vec3f(a.sin(), a.cos(), 0.0);
        let n = out * t.cos() + vec3f(0.0, 0.0, t.sin());
        (mid + out * r + n * thick, n)
    };
    for i in 0..TUBE_STEPS {
        for j in 0..TUBE_SIDES {
            let (p00, n00) = at(i, j);
            let (p10, n10) = at(i + 1, j);
            let (p11, n11) = at(i + 1, j + 1);
            let (p01, n01) = at(i, j + 1);
            mesh.quad([p00, p10, p11, p01], [n00, n10, n11, n01], part);
        }
    }
}

/// A ball `radius` round `c`.
fn ball(mesh: &mut Mesh, c: Vec3f, radius: f32, part: Part) {
    const ROWS: usize = 12;
    const SIDES: usize = 24;
    let at = |i: usize, j: usize| {
        let (v, t) = (PI * i as f32 / ROWS as f32, TAU * j as f32 / SIDES as f32);
        let n = vec3f(v.sin() * t.cos(), v.sin() * t.sin(), v.cos());
        (c + n * radius, n)
    };
    for i in 0..ROWS {
        for j in 0..SIDES {
            let (p00, n00) = at(i, j);
            let (p10, n10) = at(i + 1, j);
            let (p11, n11) = at(i + 1, j + 1);
            let (p01, n01) = at(i, j + 1);
            mesh.quad([p00, p10, p11, p01], [n00, n10, n11, n01], part);
        }
    }
}

/// A bare head's glass standing on its front at depth `z0` (box units), for a tube ring `d` (box units) on a face
/// `unit` metres to its rim `dist` metres from the camera (`modifier::bulb_glass` says which): the flash tube, a
/// horseshoe of glass standing off the plate, its ends round bulbs down into their sockets; the modelling lamp's
/// frosted dome; a reflector head's big frosted globe. Each shows the whole face (`Part::Whole`), turned with the
/// view (the tube round its centre line, the dome, the globe and the tube's ends round their middles), and stands
/// where its outline covers what the face draws of it head-on (the tube a little larger than the face's glass, by
/// its soft edge and the clip round it; the dome and the globe exactly over their cores, their soft edges left on the
/// plate): head-on the model is the flat face; the plate behind shows the face without its glass
/// (`Modifier::factor_bare`), so turned there is no painted tube on it.
fn bulb_front(mesh: &mut Mesh, p: &ModifierParams, d: f32, z0: f32, unit: f32, dist: f32) {
    let glass = bulb_glass(p.plate, p.baffle);
    let e = EDGE / d;
    // A point `z` deep stands this much nearer the axis than the face's point the camera head-on sees it on.
    let near = |z: f32| 1.0 - z * unit / dist.max(1.0e-3);
    let lamp = Part::Whole(Turn::Ball(0));
    mesh.balls.push(vec3f(0.0, 0.0, z0));
    if glass.globe {
        let rg = GLOBE_R * d * near(z0);
        let prof: Vec<(f32, f32)> = (0..=24).map(|k| {
            let t = 0.5 * PI * k as f32 / 24.0;
            (rg * t.sin(), z0 + GLOBE_TALL * rg * t.cos())
        }).collect();
        revolve(mesh, &prof, false, lamp, ROUND);
    }
    if glass.dome {
        let rd = DOME_R * d * near(z0);
        let prof: Vec<(f32, f32)> = (0..=20).map(|k| {
            let t = 0.5 * PI * k as f32 / 20.0;
            (rd * t.sin(), z0 + DOME_TALL * rd * t.cos())
        }).collect();
        revolve(mesh, &prof, false, lamp, ROUND);
    }
    if glass.tube {
        let thick = (TUBE_GLASS + TUBE_PAST + e) * d;
        let z = z0 + thick + TUBE_OFF * d;
        let r = d * near(z);
        mesh.ring = (vec3f(0.0, 0.0, z), r);
        tube_arc(mesh, vec3f(0.0, 0.0, z), r, thick, TUBE_GAP, TAU - TUBE_GAP, Part::Whole(Turn::Ring));
        for side in [1.0f32, -1.0] {
            let (x, y) = (side * TUBE_GAP.sin(), TUBE_GAP.cos());
            let end = vec3f(x * r, y * r, z);
            mesh.balls.push(end);
            let i = (mesh.balls.len() - 1) as u8;
            ball(mesh, end, thick, Part::Whole(Turn::Ball(i)));
            // the end's leg down into its socket, the socket's collar on the plate
            mesh.rod(end, vec3f(end.x, end.y, z0 + 0.5 * SOCKET_UP * d), 0.7 * thick, false, Part::Whole(Turn::None));
            let k = near(z0 + SOCKET_UP * d);
            mesh.drum(x * d * k, y * d * k, (SOCKET_R + e) * d * k, z0 - 0.01 * d, z0 + SOCKET_UP * d, Part::Whole(Turn::None));
        }
    }
}

/// A light's body behind its face, what a monolight's head is: a can `r` round from `z0` back `len`, three cooling
/// grooves near its back, a handle on top; its yoke from pivots at its sides a little ahead of its middle (`yoke`).
fn body(mesh: &mut Mesh, r: f32, z0: f32, len: f32, foot: f32, stand_r: f32) {
    let head = Part::Solid(Albedo::Coat);
    let z1 = z0 - len;
    // the can's front (behind what the face shows), its side with the grooves, its back edge rounded, its back
    revolve(mesh, &[(0.0, z0), (r, z0)], false, head, ROUND);
    let mut side = vec![(r, z0)];
    for g in 0..3 {
        let zg = z1 + (0.45 + 0.18 * g as f32) * r;
        side.extend_from_slice(&[(r, zg + 0.05 * r), (0.96 * r, zg + 0.03 * r), (0.96 * r, zg - 0.03 * r), (r, zg - 0.05 * r)]);
    }
    side.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    side.extend_from_slice(&[(r, z1 + 0.12 * r), (0.97 * r, z1 + 0.03 * r), (0.88 * r, z1)]);
    revolve(mesh, &side, false, head, ROUND);
    revolve(mesh, &[(0.88 * r, z1), (0.0, z1)], false, head, ROUND);
    // the back panel: its display, the dial and buttons under it, the power socket low on one side
    mesh.cuboid(vec3f(-0.42 * r, 0.05 * r, z1 - 0.025 * r), vec3f(0.42 * r, 0.42 * r, z1), Part::Solid(Albedo::Head));
    mesh.drum(0.0, -0.22 * r, 0.16 * r, z1 - 0.08 * r, z1, head);
    for x in [-0.45f32, -0.25, 0.25, 0.45] {
        mesh.cuboid(vec3f((x - 0.06) * r, -0.3 * r, z1 - 0.04 * r), vec3f((x + 0.06) * r, -0.2 * r, z1), Part::Solid(Albedo::Metal));
    }
    mesh.drum(0.5 * r, -0.6 * r, 0.1 * r, z1 - 0.1 * r, z1, Part::Solid(Albedo::Stand));
    // the handle on top, behind the middle
    let hz = (z0 - 0.5 * len, z0 - 0.9 * len);
    for z in [hz.0, hz.1] {
        mesh.cuboid(vec3f(-0.08 * r, 0.9 * r, z - 0.08 * r), vec3f(0.08 * r, 1.35 * r, z + 0.08 * r), head);
    }
    mesh.cuboid(vec3f(-0.09 * r, 1.3 * r, hz.1 - 0.08 * r), vec3f(0.09 * r, 1.45 * r, hz.0 + 0.08 * r), head);
    yoke(mesh, r, r, z0 - 0.42 * len, foot, stand_r);
}

/// A yoke round a light `wide` to each side, its parts sized to a head `r` round: the U from pivots at depth `zp`
/// (their knobs out to the sides) down round its underside, the block under it with its tilt knob and its umbrella
/// mount, the spigot in the
/// stand's clamp and the stand down to `foot`, `stand_r` round (box units).
fn yoke(mesh: &mut Mesh, r: f32, wide: f32, zp: f32, foot: f32, stand_r: f32) {
    let head = Part::Solid(Albedo::Coat);
    let (arm_in, arm_out) = (wide + 0.06 * r, wide + 0.16 * r);
    let bottom = -(wide + 0.3 * r);
    for s in [-1.0f32, 1.0] {
        mesh.cuboid(vec3f((s * arm_in).min(s * arm_out), bottom, zp - 0.16 * r), vec3f((s * arm_in).max(s * arm_out), 0.12 * r, zp + 0.16 * r), head);
        mesh.rod(vec3f(s * arm_out, 0.0, zp), vec3f(s * (arm_out + 0.14 * r), 0.0, zp), 0.17 * r, true, head);
    }
    mesh.cuboid(vec3f(-arm_out, bottom - 0.14 * r, zp - 0.16 * r), vec3f(arm_out, bottom, zp + 0.16 * r), head);
    let stand = Part::Solid(Albedo::Stand);
    let block = bottom - 0.14 * r;
    let bh = 0.45 * r;
    let bw = (0.22 * r).max(1.4 * stand_r);
    mesh.cuboid(vec3f(-bw, block - bh, zp - 0.24 * r), vec3f(bw, block, zp + 0.24 * r), head);
    mesh.rod(vec3f(bw, block - 0.5 * bh, zp), vec3f(bw + 0.23 * r, block - 0.5 * bh, zp), 0.1 * r, true, head);
    // the umbrella mount: a sleeve through the block, slanted up toward the front
    mesh.rod(vec3f(0.0, block - 0.75 * bh, zp - 0.4 * r), vec3f(0.0, block - 0.3 * bh, zp + 0.4 * r), 0.08 * r, true, head);
    let clamp = 1.3 * stand_r;
    let top = block - bh;
    mesh.cuboid(vec3f(-clamp, top - 4.0 * clamp, zp - clamp), vec3f(clamp, top, zp + clamp), stand);
    mesh.rod(vec3f(0.0, top - 4.0 * clamp, zp), vec3f(0.0, foot.min(top - 5.0 * clamp), zp), stand_r, true, stand);
}

/// The stand's foot below the face and its tube's radius, box units, for a face `unit` metres to its rim.
fn stand(unit: f32) -> (f32, f32) {
    let unit = unit.max(1.0e-3);
    (-STAND_DROP_M / unit, STAND_R_M / unit)
}

/// A bare flash head (`bulb`): with `plate` (the tube's ring, a share of the radius) under `BULB_REFLECTOR_BELOW`
/// in its reflector bowl, else its front the whole face; its glass on its front (`bulb_front`). `unit` is the face's
/// radius in metres, `dist` its distance from the camera.
pub(super) fn bulb(p: &ModifierParams, modifier: &Modifier, unit: f32, dist: f32) -> Option<Built> {
    if modifier.spot_bulb() {
        return Some(spot_bulb(unit, dist));
    }
    let d = p.plate.max(0.15);
    let (foot, stand_r) = stand(unit);
    let mut mesh = Mesh::new();
    mesh.bowl_lit = false;
    if d >= BULB_REFLECTOR_BELOW {
        // The head's front, flat: the face's disc (the plate and the housing round it), its glass on it.
        revolve(&mut mesh, &[(0.0, 0.0), (1.0, 0.0)], false, Part::Cloth, ROUND);
        bulb_front(&mut mesh, p, d, 0.0, unit, dist);
        // a bezel round it, then the body
        torus(&mut mesh, 1.02, -0.03, 0.03, Part::Solid(Albedo::Coat));
        body(&mut mesh, 1.03, -0.03, 3.4, foot, stand_r);
        return Some((mesh, false, None));
    }
    // In a reflector: the head's front at the bowl's throat (round a lit globe the holder's edge, 1.68 D, else
    // the housing's, 1.75 D), the bowl out to the lip. Its depth by its finish: a Zoom's fluted bowl, a Widezoom's
    // satin, else from a shallow 18 cm bowl round a large head to a deep Magnum round a small one.
    let lit = p.baffle;
    let throat = (1.75 + (1.68 - 1.75) * lit) * d;
    let deep = 1.0 - ((d - 0.17) / 0.07).clamp(0.0, 1.0);
    let depth = if p.ribs >= 3 {
        0.85
    } else if p.diffusion.unwrap_or(0.0) >= 0.2 {
        0.6
    } else {
        0.65 + 0.45 * deep
    };
    revolve(&mut mesh, &[(0.0, -depth), (throat, -depth)], false, Part::Cloth, ROUND);
    bulb_front(&mut mesh, p, d, -depth, unit, dist);
    spun_bowl(&mut mesh, throat, 0.975, depth);
    // the head behind the bowl, as wide as its housing (2.2 D) but never past 0.6 of the bowl (a 12 cm head in an
    // 18-20 cm reflector)
    let rb = (2.2 * d).min(0.6);
    body(&mut mesh, rb, -depth - WALL, 2.8 * rb, foot, stand_r);
    mesh.mirror_inner = throat;
    let mirror = depth.min(0.95);
    Some((mesh, false, Some((mirror, (1.0 + mirror * mirror) / (2.0 * mirror)))))
}

/// A clear-glass reflector spot bulb (`Modifier::spot_bulb`), box units, its front's rim at radius 1: the bowl's
/// silvered inside a spherical cap `SPOT_BOWL_DEPTH` deep, from its neck (`SPOT_NECK`) out to the rim, a mirror of the
/// filament (its hot ring moves with the view); the neck's inside a disc across its throat; in front of that the
/// filament's C (`Turn::Ring`) and the capsule's hot front (`Turn::Ball`), the glass the face draws them on, each a
/// little larger than the face's by its soft edge, so the neck behind shows the face without them; the clear front
/// glass is not drawn. Behind: the bowl's silvered back down its neck, the screw base, its black insulator and
/// contact, in a black socket on a short cord.
fn spot_bulb(unit: f32, dist: f32) -> Built {
    let mut mesh = Mesh::new();
    mesh.bowl_lit = false;
    let sphere = (1.0 + SPOT_BOWL_DEPTH * SPOT_BOWL_DEPTH) / (2.0 * SPOT_BOWL_DEPTH);
    let zs = |rho: f32| -SPOT_BOWL_DEPTH + sphere - (sphere * sphere - rho * rho).max(0.0).sqrt();
    let throat = zs(SPOT_NECK);
    let inside: Vec<(f32, f32)> = (0..=BOWL_STEPS).map(|i| {
        let rho = SPOT_NECK + (1.0 - SPOT_NECK) * i as f32 / BOWL_STEPS as f32;
        (rho, zs(rho))
    }).collect();
    revolve(&mut mesh, &inside, false, Part::Cloth, ROUND);
    revolve(&mut mesh, &[(0.0, throat), (SPOT_NECK, throat)], false, Part::Cloth, ROUND);
    // the filament and the capsule, round the middle the face draws them round
    let near = |z: f32| 1.0 - z * unit / dist.max(1.0e-3);
    let e = EDGE;
    let zc = throat + SPOT_COIL_DEPTH;
    let k = near(zc);
    let mid = vec3f(SPOT_MIDDLE.x * k, SPOT_MIDDLE.y * k, zc);
    let (r, thick) = (SPOT_COIL_R * k, (SPOT_COIL_W + e + 0.004) * k);
    mesh.ring = (mid, r);
    let open = 0.5 * PI;
    tube_arc(&mut mesh, mid, r, thick, open + SPOT_COIL_GAP, open - SPOT_COIL_GAP + TAU, Part::Whole(Turn::Ring));
    let zf = throat + SPOT_CAP_DEPTH;
    let kf = near(zf);
    let front = vec3f(SPOT_MIDDLE.x * kf, SPOT_MIDDLE.y * kf, zf);
    mesh.balls.push(front);
    let cap = (SPOT_CAP_R + e + 0.004) * kf;
    let capsule = Part::Whole(Turn::Ball(0));
    mesh.drum(front.x, front.y, cap, throat, zf, capsule);
    for s in [1.0f32, -1.0] {
        let a = open + s * SPOT_COIL_GAP;
        let end = mid + vec3f(a.sin(), a.cos(), 0.0) * r;
        mesh.balls.push(end);
        let i = (mesh.balls.len() - 1) as u8;
        ball(&mut mesh, end, thick, Part::Whole(Turn::Ball(i)));
        // its lead back to the neck
        mesh.rod(end, vec3f(end.x, end.y, throat), 0.3 * thick, false, Part::Whole(Turn::None));
    }
    // the rim's glass edge, the silvered back down to the neck, the screw base, the insulator and the contact
    let glass = Part::Solid(Albedo::Spun);
    revolve(&mut mesh, &[(1.0, 0.0), (1.0, -0.04), (1.0 - WALL, -0.04)], false, glass, ROUND);
    let back: Vec<(f32, f32)> = (0..=BOWL_STEPS).map(|i| {
        let t = 0.5 * PI * i as f32 / BOWL_STEPS as f32;
        (SPOT_BASE_R + 0.03 + (0.97 - SPOT_BASE_R) * t.cos(), -0.04 - (SPOT_BACK - 0.04) * t.sin())
    }).collect();
    revolve(&mut mesh, &back, false, glass, ROUND);
    let zb = -SPOT_BACK;
    let mut screw = vec![(SPOT_BASE_R + 0.03, zb)];
    for k in 0..SPOT_THREADS {
        let z = zb - SPOT_BASE_LEN * (k as f32 + 0.5) / SPOT_THREADS as f32;
        screw.push((SPOT_BASE_R + 0.035, z + 0.25 * SPOT_BASE_LEN / SPOT_THREADS as f32));
        screw.push((SPOT_BASE_R - 0.005, z - 0.25 * SPOT_BASE_LEN / SPOT_THREADS as f32));
    }
    screw.push((SPOT_BASE_R, zb - SPOT_BASE_LEN));
    revolve(&mut mesh, &screw, false, Part::Solid(Albedo::Metal), ROUND);
    let zi = zb - SPOT_BASE_LEN;
    revolve(&mut mesh, &[(SPOT_BASE_R, zi), (0.7 * SPOT_BASE_R, zi - 0.12), (0.0, zi - 0.12)], false, Part::Solid(Albedo::Head), ROUND);
    mesh.drum(0.0, 0.0, 0.25 * SPOT_BASE_R, zi - 0.17, zi - 0.11, Part::Solid(Albedo::Metal));
    // the socket over the base's back half and its cord up
    let socket = Part::Solid(Albedo::Head);
    let zs0 = zb - 0.45 * SPOT_BASE_LEN;
    revolve(&mut mesh, &[(SPOT_BASE_R + 0.06, zs0), (SPOT_BASE_R + 0.09, zs0 - 0.05), (SPOT_BASE_R + 0.09, zi - 0.45), (0.3 * SPOT_BASE_R, zi - 0.6), (0.0, zi - 0.6)], false, socket, ROUND);
    mesh.rod(vec3f(0.0, 0.0, zi - 0.55), vec3f(0.0, 0.5, zi - 0.9), 0.06, true, socket);
    mesh.rod(vec3f(0.0, 0.5, zi - 0.9), vec3f(0.0, SPOT_CORD, zi - 0.9), 0.06, true, socket);
    mesh.mirror_inner = SPOT_MIRROR_FROM;
    (mesh, false, Some((SPOT_BOWL_DEPTH, sphere)))
}

/// A beauty dish: the spun bowl (`DISH_DEPTH`, its throat the head's mount) out to its rolled lip at the face's
/// housing; bare, the deflector plate (`plate` of the radius) in front of the flash on three rods; with its sock
/// (`baffle` over a half) the sock's front, taut and a little bowed, and where it wraps over the lip. The head behind.
pub(super) fn beauty_dish(p: &ModifierParams, modifier: &Modifier, unit: f32) -> Option<Built> {
    let _ = modifier;
    let (foot, stand_r) = stand(unit);
    let edge = 1.0 - p.rim.max(0.01);
    let mut mesh = Mesh::new();
    mesh.bowl_lit = false;
    revolve(&mut mesh, &[(0.0, -DISH_DEPTH), (DISH_THROAT, -DISH_DEPTH)], false, Part::Cloth, ROUND);
    spun_bowl(&mut mesh, DISH_THROAT, edge, DISH_DEPTH);
    let sock = p.baffle > 0.5;
    if sock {
        // The sock's front: taut over the lip, bowed out a little in the middle, rounding over the lip's roll
        // (an arc `SOCK_WRAP` round, whose outer edge is the face's rim) and down its outside, held behind it.
        let w = SOCK_WRAP;
        let r0 = 1.0 - w;
        let front: Vec<(f32, f32)> = (0..=24).map(|i| {
            let rho = r0 * i as f32 / 24.0;
            (rho, w + SOCK_BOW * (1.0 - (rho / r0) * (rho / r0)))
        }).collect();
        revolve(&mut mesh, &front, false, Part::Cloth, ROUND);
        let mut wrap: Vec<(f32, f32)> = (0..=8).map(|k| {
            let t = 0.5 * PI * k as f32 / 8.0;
            (r0 + w * t.sin(), w * t.cos())
        }).collect();
        wrap.push((1.0, -2.0 * w));
        wrap.push((1.0 - 0.6 * w, -2.6 * w));
        revolve(&mut mesh, &wrap, false, Part::Cloth, ROUND);
    } else {
        // The plate, its front a little domed, and its three rods back to the mount behind it (the face's clips at
        // its rim: spokes at -0.73 + k 120 degrees clockwise from up).
        let pr = p.plate.max(0.05);
        let plate = Part::Solid(Albedo::Head);
        revolve(&mut mesh, &[(0.0, DISH_PLATE_Z + 0.012), (0.7 * pr, DISH_PLATE_Z + 0.008), (pr, DISH_PLATE_Z)], false, plate, ROUND);
        revolve(&mut mesh, &[(pr, DISH_PLATE_Z), (pr, DISH_PLATE_Z - 0.012), (0.0, DISH_PLATE_Z - 0.012)], false, plate, ROUND);
        // its rolled rim, lit
        torus(&mut mesh, pr - 0.008, DISH_PLATE_Z - 0.002, 0.008, Part::Solid(Albedo::Spun));
        for k in 0..3 {
            let a = -0.73 + TAU * k as f32 / 3.0;
            let (s, c) = (a.sin(), a.cos());
            // the clip over the plate's rim
            mesh.rod(vec3f((pr - 0.07) * s, (pr - 0.07) * c, DISH_PLATE_Z + 0.016), vec3f((pr + 0.006) * s, (pr + 0.006) * c, DISH_PLATE_Z + 0.004), 0.005, true, Part::Solid(Albedo::Spun));
            let from = vec3f(0.94 * pr * s, 0.94 * pr * c, DISH_PLATE_Z - 0.012);
            let to = vec3f(0.85 * DISH_THROAT * s, 0.85 * DISH_THROAT * c, -DISH_DEPTH + 0.01);
            mesh.rod(from, to, 0.006, true, Part::Solid(Albedo::Metal));
        }
    }
    // the mount collar behind the bowl's throat, then the head
    let collar = Part::Solid(Albedo::Spun);
    revolve(&mut mesh, &[(DISH_THROAT + WALL, -DISH_DEPTH - WALL), (DISH_THROAT + WALL, -DISH_DEPTH - 0.12)], false, collar, ROUND);
    revolve(&mut mesh, &[(DISH_THROAT + WALL, -DISH_DEPTH - 0.12), (0.0, -DISH_DEPTH - 0.12)], false, collar, ROUND);
    body(&mut mesh, DISH_HEAD_R, -DISH_DEPTH - 0.12, 4.0 * DISH_HEAD_R, foot, stand_r);
    let mirror = (!sock).then_some((DISH_DEPTH, (1.0 + DISH_DEPTH * DISH_DEPTH) / (2.0 * DISH_DEPTH)));
    if !sock {
        mesh.mirror_inner = DISH_THROAT;
    }
    Some((mesh, false, mirror))
}

/// A fresnel's stepped lens, (radius, z) from its middle out, its back `LENS_INSET` deep: the middle disc a low dome,
/// then `steps` rings as the face lays them out (modifier.rs `fresnel_lens`: the n-th's outer edge at
/// `LENS_DISC` + (1 - `LENS_DISC`) (n / steps)^0.78 of the lens's radius), each a riser at its inner edge and its
/// face sloping back outward to the groove at its outer edge.
fn lens_profile(steps: usize) -> Vec<(f32, f32)> {
    let back = -LENS_INSET;
    let edge = |k: usize| LENS_R * (LENS_DISC + (1.0 - LENS_DISC) * (k as f32 / steps as f32).powf(0.78));
    let mut prof: Vec<(f32, f32)> = (0..=8).map(|k| {
        let t = k as f32 / 8.0;
        (LENS_R * LENS_DISC * t, back + LENS_DOME * (1.0 - t * t))
    }).collect();
    for k in 1..=steps {
        let (r0, r1) = (edge(k - 1), edge(k));
        for i in 0..=4 {
            let t = i as f32 / 4.0;
            prof.push((r0 + (r1 - r0) * t, back + LENS_STEP * (1.0 - t)));
        }
    }
    prof
}

/// A fresnel spot: its stepped lens (`lens_profile`) inside the barrel's front ring, the barrel's inner wall between
/// them (each shows the face where it stands, `Part::Whole`: head-on it is the face's lens and barrel, turned the
/// steps' risers and the grooves behind them show and hide), the barrel back to the housing, the housing with its
/// cooling fins, its back with the focus knob, on its yoke and stand.
pub(super) fn fresnel(p: &ModifierParams, modifier: &Modifier, unit: f32) -> Option<Built> {
    let _ = modifier;
    let (foot, stand_r) = stand(unit);
    let mut mesh = Mesh::new();
    mesh.bowl_lit = false;
    let head = Part::Solid(Albedo::Coat);
    // the lens, the barrel's front ring round it (its retainer, clips, slots and lip are the face's), the barrel's
    // inner wall between them
    let glass = Part::Whole(Turn::None);
    revolve(&mut mesh, &lens_profile(p.ribs.max(3) as usize), false, glass, ROUND);
    revolve(&mut mesh, &[(LENS_R, 0.0), (0.985, 0.0)], false, Part::Cloth, ROUND);
    revolve(&mut mesh, &[(LENS_R, -LENS_INSET), (LENS_R, 0.0)], true, glass, ROUND);
    // the barrel's rounded front edge and its wall back to the housing's front plate
    let edge: Vec<(f32, f32)> = (0..=6).map(|k| {
        let t = 0.5 * PI * k as f32 / 6.0;
        (0.985 + 0.015 * t.sin(), -0.015 + 0.015 * t.cos())
    }).collect();
    revolve(&mut mesh, &edge, false, Part::Cloth, ROUND);
    revolve(&mut mesh, &[(1.0, -0.015), (1.0, -BARREL_LEN)], false, head, ROUND);
    // the front plate the barn doors clip to, square
    let fz = -BARREL_LEN;
    mesh.cuboid(vec3f(-1.3, -1.3, fz - 0.1), vec3f(1.3, 1.3, fz), head);
    for (x, y) in [(-1.15, 1.15), (1.15, 1.15), (-1.15, -1.15), (1.15, -1.15)] {
        mesh.rod(vec3f(x, y, fz), vec3f(x, y, fz + 0.08), 0.05, true, Part::Solid(Albedo::Metal));
    }
    // the housing, its fins round its back two thirds, its back
    let hz0 = fz - 0.1;
    let hz1 = hz0 - HOUSING_LEN;
    revolve(&mut mesh, &[(HOUSING_R, hz0), (HOUSING_R, hz1 + 0.1), (HOUSING_R - 0.1, hz1)], false, head, ROUND);
    revolve(&mut mesh, &[(HOUSING_R - 0.1, hz1), (0.0, hz1)], false, head, ROUND);
    for k in 0..FINS {
        let z = hz0 - 0.55 - k as f32 * (HOUSING_LEN - 0.75) / (FINS - 1) as f32;
        revolve(&mut mesh, &[(HOUSING_R, z), (FIN_R, z), (FIN_R, z - FIN_THICK), (HOUSING_R, z - FIN_THICK)], false, head, ROUND);
    }
    // the vent hood on top, the focus knob out of the back, low
    mesh.cuboid(vec3f(-0.5, FIN_R - 0.05, hz0 - 0.4), vec3f(0.5, FIN_R + 0.12, hz1 + 0.3), head);
    mesh.rod(vec3f(0.0, -0.8, hz1), vec3f(0.0, -0.8, hz1 - 0.35), 0.06, true, Part::Solid(Albedo::Metal));
    mesh.drum(0.0, -0.8, 0.16, hz1 - 0.55, hz1 - 0.35, head);
    // the yoke round the fins, from pivots at the housing's middle
    yoke(&mut mesh, 0.6, FIN_R, hz0 - 0.5 * HOUSING_LEN, foot, stand_r);
    Some((mesh, false, None))
}

#[cfg(test)]
mod tests {
    use super::super::{LightModel, ModelHit};
    use crate::image_light::ImageSet;
    use crate::light_fx::LightFx;
    use crate::modifier::apply_template;
    use crate::params::{LightParams, ModifierKind, SpaceParams};
    use makepad_draw::*;

    /// A 3D light of `kind` `size` metres across, 1 m in front of the camera, turned by `yaw`; `set` changes its
    /// modifier's knobs.
    fn light(kind: ModifierKind, size: f32, yaw: f32, set: impl Fn(&mut crate::params::ModifierParams)) -> LightParams {
        let mut light = LightParams::default();
        apply_template(&mut light, kind);
        set(light.modifier.as_mut().unwrap());
        light.space = Some(SpaceParams { position_m: [0.0, 0.0, -1.0], yaw_deg: yaw, pitch_deg: 0.0, roll_deg: 0.0, width_m: size, height_m: size, beam: 0.0 });
        light
    }

    fn model(light: &LightParams) -> Option<LightModel> {
        let fx = LightFx::new(light, &ImageSet::default());
        LightModel::new(light, fx.modifier())
    }

    /// The heads: a bare one, one with its modelling lamp lit, one in a reflector, one in a reflector with its big
    /// globe lit, a bare beauty dish, one in its sock, a fresnel, a spot bulb.
    fn heads(yaw: f32) -> Vec<(&'static str, LightParams)> {
        vec![
            ("bare head", light(ModifierKind::Bulb, 0.11, yaw, |m| m.plate = 0.45)),
            ("lit head", light(ModifierKind::Bulb, 0.11, yaw, |m| {
                m.plate = 0.45;
                m.baffle = 1.0;
            })),
            ("globe", light(ModifierKind::Bulb, 0.18, yaw, |m| {
                m.plate = 0.25;
                m.baffle = 1.0;
            })),
            ("spot bulb", light(ModifierKind::Bulb, 0.08, yaw, |m| m.ribs = 1)),
            ("reflector", light(ModifierKind::Bulb, 0.3, yaw, |m| {
                m.plate = 0.3;
                m.ribs = 40;
            })),
            ("beauty dish", light(ModifierKind::BeautyDish, 0.41, yaw, |m| m.baffle = 0.0)),
            ("sock", light(ModifierKind::BeautyDish, 0.41, yaw, |m| m.baffle = 1.0)),
            ("fresnel", light(ModifierKind::Fresnel, 0.25, yaw, |_| {})),
        ]
    }

    /// Each has a model in 3D and none in 2D.
    #[test]
    fn heads_and_dishes_have_models_in_3d_only() {
        for (name, light) in heads(0.0) {
            let m = model(&light).unwrap_or_else(|| panic!("{name}: no model"));
            assert!(m.triangles() > 2000 && m.triangles() < 60000, "{name}: {}", m.triangles());
            let mut flat = light.clone();
            flat.space = None;
            assert!(model(&flat).is_none(), "{name} in 2D");
        }
    }

    /// Head-on, a ray through the face meets what shows it at the very point of the flat face it travels along, its
    /// mirror 1 (a bare dish's plate is a dull solid); from behind, a ray at the face meets a solid.
    #[test]
    fn head_on_each_is_its_flat_face() {
        for (name, light) in heads(0.0) {
            let m = model(&light).unwrap();
            let half = 0.5 * light.space.as_ref().unwrap().width_m;
            for (x, y) in [(0.55, 0.3), (-0.2, 0.7), (0.1, -0.85), (0.0, 0.0)] {
                let dir = vec3f(x * half, y * half, -1.0).normalize();
                match m.cast(dir) {
                    Some(ModelHit::Face { at, mirror }) => {
                        assert!((at - dir).length() < 2.0e-5, "{name} at ({x}, {y}): {at:?} for {dir:?}");
                        assert!((mirror - 1.0).abs() < 1.0e-4, "{name} at ({x}, {y}): mirror {mirror}");
                    }
                    Some(ModelHit::Whole { at }) => assert!((at - dir).length() < 2.0e-5, "{name} at ({x}, {y}): {at:?} for {dir:?}"),
                    Some(ModelHit::Solid { .. }) if name == "beauty dish" && x * x + y * y < 0.3 * 0.3 => {}
                    other => panic!("{name} at ({x}, {y}): {other:?}"),
                }
            }
            let back = model(&{
                let mut l = light.clone();
                l.space.as_mut().unwrap().yaw_deg = 180.0;
                l
            })
            .unwrap();
            match back.cast(vec3f(0.3 * half, 0.2 * half, -1.0).normalize()) {
                Some(ModelHit::Solid { level }) => assert!(level > 0.0 && level < 0.2, "{name} behind: {level}"),
                other => panic!("{name} behind: {other:?}"),
            }
        }
    }

    /// Turned 45 degrees a metal bowl's image of the flash moves (somewhere on it the mirror is well off 1), and a
    /// head in a reflector's throat is no mirror.
    #[test]
    fn metal_bowls_mirror_with_the_view() {
        for (name, light) in heads(45.0) {
            if !matches!(name, "reflector" | "beauty dish" | "spot bulb") {
                continue;
            }
            let m = model(&light).unwrap();
            let (mut lo, mut hi) = (f32::INFINITY, 0.0f32);
            for i in -30..=30 {
                for j in -30..=30 {
                    let dir = vec3f(i as f32 * 0.008, j as f32 * 0.008, -1.0).normalize();
                    if let Some(ModelHit::Face { mirror, .. }) = m.cast(dir) {
                        lo = lo.min(mirror);
                        hi = hi.max(mirror);
                    }
                }
            }
            assert!(lo < 0.7 || hi > 1.4, "{name}: the mirror stays at {lo}..{hi}");
        }
    }

    /// The glass in front of a bare head's face, and a spot bulb's filament and capsule: head-on a ray through the
    /// tube, the dome, the globe, the coil or the capsule meets it at the flat face's very point; the plate behind
    /// draws the face without it (the tube's middle on the bare face well under a fifth of the whole face's), and
    /// the same face as the whole one where nothing stands; turned 45 degrees the glass stands off the plate: some
    /// rays meet it, some the plate round it.
    #[test]
    fn glass_stands_in_front_of_the_face() {
        // (name, the face point the glass stands over, a face point on the plate beside it, in the face's half units)
        let cases = [
            ("bare head", (-0.45, 0.0), (0.0, 0.62)),
            ("lit head", (0.05, 0.1), (0.0, 0.62)),
            ("globe", (0.1, 0.05), (0.0, 0.47)),
            ("spot bulb", (0.03, -0.03), (-0.2, 0.7)),
        ];
        for (name, light) in heads(0.0) {
            let Some(&(_, glass, plate)) = cases.iter().find(|c| c.0 == name) else { continue };
            let fx = LightFx::new(&light, &ImageSet::default());
            let m = LightModel::new(&light, fx.modifier()).unwrap();
            let half = 0.5 * light.space.as_ref().unwrap().width_m;
            let dir = |p: (f32, f32)| vec3f(p.0 * half, p.1 * half, -1.0).normalize();
            match m.cast(dir(glass)) {
                Some(ModelHit::Whole { at }) => assert!((at - dir(glass)).length() < 2.0e-5, "{name}: {at:?}"),
                other => panic!("{name} over its glass: {other:?}"),
            }
            let (whole, bare) = (fx.glow(dir(glass)), fx.bare_glow(dir(glass)));
            assert!(bare < 0.2 * whole, "{name}: the bare face under the glass {bare} of {whole}");
            assert_eq!(fx.glow(dir(plate)), fx.bare_glow(dir(plate)), "{name}: beside the glass");
            assert!(matches!(m.cast(dir(plate)), Some(ModelHit::Face { .. })), "{name}: the plate beside the glass");
            let mut turned = light.clone();
            turned.space.as_mut().unwrap().yaw_deg = 45.0;
            let fx = LightFx::new(&turned, &ImageSet::default());
            let m = LightModel::new(&turned, fx.modifier()).unwrap();
            let (mut glass_hits, mut plate_hits) = (0, 0);
            for i in -40..=40 {
                for j in -40..=40 {
                    match m.cast(vec3f(i as f32 * 0.02 * half, j as f32 * 0.02 * half, -1.0).normalize()) {
                        Some(ModelHit::Whole { .. }) => glass_hits += 1,
                        Some(ModelHit::Face { .. }) => plate_hits += 1,
                        _ => {}
                    }
                }
            }
            assert!(glass_hits > 20 && plate_hits > 20, "{name} turned: {glass_hits} on the glass, {plate_hits} on the plate");
        }
    }
}

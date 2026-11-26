//! The boxes' models (light_model.rs): a softbox, a strip and an octabox as small 3D models, closed (the front
//! diffuser), with the inner baffle alone, and open (the silver lining round the bare tube).
//!
//! A box is a pyramid frustum of black nylon, silver inside: four walls (eight round an octabox) from the opening to
//! the back panel round the speed ring, the strobe head in the ring behind it, its mount and the stand under the head.
//! The three faces are three cuts through that one box:
//! - closed: the front diffuser, a flat cloth over the opening;
//! - the inner baffle: the opening bare, the inner cloth across the box where the face draws it (its outline as the
//!   face's, `modifier::BAFFLE_SIDE` and the strip's and octabox's own), the walls' silver between it and the opening;
//! - open: the walls running back to the back panel (`OpenBox::back_panel`), the tube on the panel in its middle.
//!
//! What a ray meets inside the box (the front cloth, the inner cloth, the walls' and the panel's silver) is the light's
//! own face at the point the camera head-on sees there (`Part::Cloth`), so head-on the model is the flat face point for
//! point. The box's sections are laid out so that seen head-on they fall where the face draws them: a section `z`
//! metres behind the opening is widened by the camera's perspective, (dist + z) / dist, so the walls' band, the inner
//! cloth's edge and the back panel each land on their own part of the face, and turned each part of the face stays on
//! its own surface. Outside, the nylon is a black cloth's back (`LightModel::black_back`): no light passes it. It is a
//! shell of its own, bowed out between the rods over the flat silver inside, the rods' sleeves along its corners and
//! the seam round its back just outside it, so seen from outside the box is closed and seen from inside only the face
//! shows. Behind: the speed ring, the head with its rear cap, the mount with its tilt knob, the spigot in the stand's
//! clamp with its T-knob, the stand.
//!
//! The box is built in metres (a speed ring and a strobe head have their real sizes whatever the box's) and handed to
//! the mesh in box units.
use makepad_draw::*;

use super::{Albedo, Built, Mesh, Part};
use crate::modifier::{BAFFLE_SIDE, OCTA_INNER_EDGE, STRIP_BAFFLE_END, STRIP_BAFFLE_SIDE};
use crate::open_box::OpenBox;
use crate::params::ModifierKind;

/// How deep a box is from its opening to its back panel, metres: a share of its shorter half size and a floor
/// (Profoto RFi: the 60 x 90 about 0.38 deep, the 30 x 120 strip about 0.29, the 60 cm octa about 0.38).
const DEPTH_FLOOR_M: f32 = 0.2;
const DEPTH_PER_HALF: f32 = 0.6;
/// The inner baffle's depth in the box, a share of the depth, held between these (the walls run straight from the
/// opening to it and from it to the back panel).
const BAFFLE_DEPTH: [f32; 2] = [0.25, 0.6];
/// The speed ring, metres: its radius and length (behind the back panel).
const RING_R_M: f32 = 0.1;
const RING_LEN_M: f32 = 0.035;
/// The strobe head in the ring: its body's radius and length, its rear cap's.
const HEAD_R_M: f32 = 0.066;
const HEAD_LEN_M: f32 = 0.27;
const CAP_R_M: f32 = 0.055;
const CAP_LEN_M: f32 = 0.02;
/// The mount under the head, half its width, its depth along the head and its height, metres; the spigot's and the
/// stand's radii and how far below the box's middle the stand reaches.
const MOUNT_HALF_M: f32 = 0.028;
const MOUNT_DEPTH_M: f32 = 0.05;
const MOUNT_HEIGHT_M: f32 = 0.06;
const SPIGOT_R_M: f32 = 0.022;
const STAND_R_M: f32 = 0.016;
const STAND_FOOT_M: f32 = -1.4;
/// How far a wall bows out between its corners and its ends, a share of its width, and its quads across and along.
const WALL_BULGE: f32 = 0.035;
const WALL_ACROSS: usize = 8;
const WALL_ALONG: usize = 6;
/// The tilt knob on the head's mount and the stand's clamp with its knob, metres.
const KNOB_R_M: f32 = 0.018;
const CLAMP_HALF_M: f32 = 0.03;
const CLAMP_HEIGHT_M: f32 = 0.07;
/// The rods' sleeves along the box's corners and the seam round its back panel, metres.
const SLEEVE_R_M: f32 = 0.005;
/// Sides of a drum and of a rod.
const DRUM_SIDES: usize = 24;
const ROD_SIDES: usize = 8;

/// Which of the three cuts the face is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cut {
    Closed,
    Baffle,
    Open,
}

use crate::patterns::smooth;

fn div(p: Vec3f, s: Vec3f) -> Vec3f {
    vec3f(p.x / s.x, p.y / s.y, p.z / s.z)
}

/// Triangles in metres, handed to a `Mesh` in box units: corners divided by the scale, normals times it (the inverse
/// transpose, as `LightModel::cast` reads them back).
struct Metres {
    mesh: Mesh,
    scale: Vec3f,
}

impl Metres {
    fn tri(&mut self, p: [Vec3f; 3], n: Vec3f, part: Part) {
        let s = self.scale;
        let nb = (n * s).normalize();
        self.mesh.tri([div(p[0], s), div(p[1], s), div(p[2], s)], [nb; 3], part);
    }

    fn tri_n(&mut self, p: [Vec3f; 3], n: [Vec3f; 3], part: Part) {
        let s = self.scale;
        self.mesh.tri([div(p[0], s), div(p[1], s), div(p[2], s)], [(n[0] * s).normalize(), (n[1] * s).normalize(), (n[2] * s).normalize()], part);
    }

    /// The flat quad a, b, c, d (in order round it), its normal `n`.
    fn quad(&mut self, p: [Vec3f; 4], n: Vec3f, part: Part) {
        self.tri([p[0], p[1], p[2]], n, part);
        self.tri([p[0], p[2], p[3]], n, part);
    }

    /// A cylinder of radius `r` round the z axis line through (`x`, `y`), from `z0` to `z1` (`z0` < `z1`), closed.
    fn drum(&mut self, x: f32, y: f32, r: f32, z0: f32, z1: f32, part: Part) {
        let (c0, c1) = (vec3f(x, y, z0), vec3f(x, y, z1));
        let round = |k: usize| {
            let t = std::f32::consts::TAU * k as f32 / DRUM_SIDES as f32;
            vec3f(t.cos(), t.sin(), 0.0)
        };
        for k in 0..DRUM_SIDES {
            let (n0, n1) = (round(k), round(k + 1));
            let (p00, p01, p11, p10) = (c0 + n0 * r, c0 + n1 * r, c1 + n1 * r, c1 + n0 * r);
            self.tri_n([p00, p01, p11], [n0, n1, n1], part);
            self.tri_n([p00, p11, p10], [n0, n1, n0], part);
            self.tri([c0, c0 + n1 * r, c0 + n0 * r], vec3f(0.0, 0.0, -1.0), part);
            self.tri([c1, c1 + n0 * r, c1 + n1 * r], vec3f(0.0, 0.0, 1.0), part);
        }
    }

    /// A rod of radius `r` from `a` to `b`, closed.
    fn rod(&mut self, a: Vec3f, b: Vec3f, r: f32, part: Part) {
        let axis = b - a;
        let len = axis.length();
        if !(len > 1.0e-6) {
            return;
        }
        let w = axis / len;
        let e1 = if w.x.abs() < 0.9 { Vec3f::cross(w, vec3f(1.0, 0.0, 0.0)) } else { Vec3f::cross(w, vec3f(0.0, 1.0, 0.0)) }.normalize();
        let e2 = Vec3f::cross(w, e1);
        let round = |k: usize| {
            let t = std::f32::consts::TAU * k as f32 / ROD_SIDES as f32;
            e1 * t.cos() + e2 * t.sin()
        };
        for k in 0..ROD_SIDES {
            let (n0, n1) = (round(k), round(k + 1));
            self.tri_n([a + n0 * r, a + n1 * r, b + n1 * r], [n0, n1, n1], part);
            self.tri_n([a + n0 * r, b + n1 * r, b + n0 * r], [n0, n1, n0], part);
            self.tri([a, a + n1 * r, a + n0 * r], -w, part);
            self.tri([b, b + n0 * r, b + n1 * r], w, part);
        }
    }

    /// The box from `lo` to `hi`.
    fn cuboid(&mut self, lo: Vec3f, hi: Vec3f, part: Part) {
        let p = |i: usize| vec3f(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
        let faces: [([usize; 4], Vec3f); 6] = [
            ([0, 2, 6, 4], vec3f(-1.0, 0.0, 0.0)),
            ([1, 5, 7, 3], vec3f(1.0, 0.0, 0.0)),
            ([0, 4, 5, 1], vec3f(0.0, -1.0, 0.0)),
            ([2, 3, 7, 6], vec3f(0.0, 1.0, 0.0)),
            ([0, 1, 3, 2], vec3f(0.0, 0.0, -1.0)),
            ([4, 6, 7, 5], vec3f(0.0, 0.0, 1.0)),
        ];
        for (c, n) in faces {
            self.quad([p(c[0]), p(c[1]), p(c[2]), p(c[3])], n, part);
        }
    }

    /// A flat polygon (`corners` round it, metres) facing +z, as a fan from its middle.
    fn cap(&mut self, corners: &[Vec3f], part: Part) {
        // the fan's middle a little off the axis, so a ray straight down it meets a triangle's inside, not a corner
        let n = corners.len();
        let mid = corners.iter().fold(Vec3f::default(), |a, c| a + *c) / n as f32;
        let mid = mid + (corners[0] - mid) * 0.0137 + (corners[1 % n] - mid) * 0.0071;
        for k in 0..n {
            let (a, b) = (corners[k], corners[(k + 1) % n]);
            // wound so the normal is +z whichever way round the corners run
            let tri = if Vec3f::cross(a - mid, b - mid).z >= 0.0 { [mid, a, b] } else { [mid, b, a] };
            self.tri(tri, vec3f(0.0, 0.0, 1.0), part);
        }
    }

    /// The walls round a box of `n` corners, `corner(k, t)` corner k's place at `t` from the opening (0) to the back
    /// (1), metres, as quads between the `ts` (increasing) and `across` across each wall; their normals inward, each
    /// wall bowed out between its corners and its ends by `bulge` of its width (the rods bend, the nylon pulls round
    /// them), exactly not at all on its edges so neighbours meet.
    fn walls(&mut self, n: usize, corner: &dyn Fn(usize, f32) -> Vec3f, ts: &[f32], across: usize, bulge: f32, part: Part) {
        let hump = |v: f32| if v <= 0.0 || v >= 1.0 { 0.0 } else { (std::f32::consts::PI * v).sin() };
        for k in 0..n {
            let j = (k + 1) % n;
            let at = |s: f32, t: f32| {
                let (ck, cj) = (corner(k, t), corner(j, t));
                let p = ck * (1.0 - s) + cj * s;
                if bulge == 0.0 {
                    return p;
                }
                // outward: away from the axis, square to the wall's edge
                let edge = cj - ck;
                let out = vec3f(edge.y, -edge.x, 0.0);
                let out = if out.x * p.x + out.y * p.y < 0.0 { -out } else { out };
                p + out.normalize() * (bulge * edge.length() * hump(s) * hump(t))
            };
            let normal = |s: f32, t: f32| {
                let e = 1.0e-3;
                let ds = at((s + e).min(1.0), t) - at((s - e).max(0.0), t);
                let dt = at(s, (t + e).min(1.0)) - at(s, (t - e).max(0.0));
                let mut nn = Vec3f::cross(ds, dt);
                let p = at(s, t);
                if nn.x * p.x + nn.y * p.y > 0.0 {
                    nn = -nn;
                }
                if nn.length() > 1.0e-12 { nn.normalize() } else { vec3f(0.0, 0.0, 1.0) }
            };
            for i in 0..across {
                let (s0, s1) = (i as f32 / across as f32, (i + 1) as f32 / across as f32);
                for w in ts.windows(2) {
                    let (t0, t1) = (w[0], w[1]);
                    let p = [at(s0, t0), at(s1, t0), at(s1, t1), at(s0, t1)];
                    let nn = [normal(s0, t0), normal(s1, t0), normal(s1, t1), normal(s0, t1)];
                    self.tri_n([p[0], p[1], p[2]], [nn[0], nn[1], nn[2]], part);
                    self.tri_n([p[0], p[2], p[3]], [nn[0], nn[2], nn[3]], part);
                }
            }
        }
    }
}

/// A box's model: `kind` a softbox, a strip or an octabox, its face's `baffle` (`ModifierParams::baffle`: closed, the
/// inner baffle, open), half its width and height `half_m` and the camera's distance `dist_m`, metres. None for any
/// other kind.
pub(super) fn build(kind: ModifierKind, baffle: f32, half_m: Vec2f, dist_m: f32) -> Option<Built> {
    let octa = match kind {
        ModifierKind::Softbox | ModifierKind::Strip => false,
        ModifierKind::Octabox => true,
        _ => return None,
    };
    let (rx, ry) = (half_m.x.max(1.0e-3), half_m.y.max(1.0e-3));
    let scale = vec3f(rx, ry, 0.5 * (rx + ry));
    let dist = dist_m.max(0.05);
    // The cut, as the face's mix reads (modifier.rs `diffused`, `octabox`).
    let open = smooth(0.75, 1.0, baffle);
    let inner = smooth(0.0, 0.7, baffle) * (1.0 - open);
    let cut = if open >= 0.5 {
        Cut::Open
    } else if inner >= 0.5 {
        Cut::Baffle
    } else {
        Cut::Closed
    };
    let short = rx.min(ry);
    let depth = DEPTH_FLOOR_M + DEPTH_PER_HALF * short;
    // The sections' outlines in box units: the opening, the inner cloth, the back panel.
    let outline = |h: Vec2f| -> Vec<Vec2f> {
        if octa {
            (0..8)
                .map(|k| {
                    let (s, c) = (k as f32 * std::f32::consts::TAU / 8.0).sin_cos();
                    vec2f(s * h.x, c * h.y)
                })
                .collect()
        } else {
            vec![vec2f(h.x, h.y), vec2f(-h.x, h.y), vec2f(-h.x, -h.y), vec2f(h.x, -h.y)]
        }
    };
    let back_at = OpenBox::back_panel(octa);
    let back = if octa { vec2f(back_at[0], back_at[0]) } else { vec2f(back_at[0], back_at[1]) };
    let inner_half = if octa {
        // The octagon OCTA_INNER_EDGE in from the opening's edge (in apothems): its corners at 1 - that.
        let r = 1.0 - OCTA_INNER_EDGE;
        vec2f(r, r)
    } else {
        let (ax, ay) = (rx / short, ry / short);
        let (deep_x, deep_y) = if kind == ModifierKind::Strip { (STRIP_BAFFLE_SIDE, STRIP_BAFFLE_END) } else { (BAFFLE_SIDE, 2.0 * BAFFLE_SIDE) };
        vec2f((1.0 - deep_x / ax).max(0.1), (1.0 - deep_y / ay).max(0.1))
    };
    // The inner cloth's depth: where straight walls from the opening to the back panel would meet its outline, the
    // mean of both axes'.
    let share = |i: f32, b: f32| (1.0 - i) / (1.0 - b).max(1.0e-3);
    let at_baffle = (0.5 * (share(inner_half.x, back.x) + share(inner_half.y, back.y))).clamp(BAFFLE_DEPTH[0], BAFFLE_DEPTH[1]);
    // A section `z` metres behind the opening in metres, widened by the camera head-on's perspective.
    let section = |h: Vec2f, z: f32| -> Vec<Vec3f> {
        let widen = (dist + z) / dist;
        outline(h).into_iter().map(|c| vec3f(c.x * rx * widen, c.y * ry * widen, -z)).collect()
    };
    let front = section(vec2f(1.0, 1.0), 0.0);
    let z_b = at_baffle * depth;
    let mid = section(inner_half, z_b);
    let rear = section(back, depth);
    let mut m = Metres { mesh: Mesh::new(), scale };
    // The walls: their silver inside flat from section to section (each part of the face on its own plane), their
    // black nylon outside one shell bowed out over them from the opening to the back, meeting them only at the
    // corners, where the rods' sleeves run (and round the back panel), so no ray slips between two walls.
    let n = front.len();
    let (fr, mi, re) = (&front, &mid, &rear);
    let tb = at_baffle;
    let corner = move |k: usize, t: f32| {
        if t <= tb {
            fr[k] + (mi[k] - fr[k]) * (t / tb)
        } else {
            mi[k] + (re[k] - mi[k]) * ((t - tb) / (1.0 - tb))
        }
    };
    m.walls(n, &corner, &[0.0, tb, 1.0], 1, 0.0, Part::Cloth);
    let ts: Vec<f32> = (0..=2 * WALL_ALONG).map(|l| if l <= WALL_ALONG { tb * l as f32 / WALL_ALONG as f32 } else { tb + (1.0 - tb) * (l - WALL_ALONG) as f32 / WALL_ALONG as f32 }).collect();
    m.walls(n, &corner, &ts, WALL_ACROSS, WALL_BULGE, Part::Cloth);
    // The sleeves sit just outside the corners and just behind the back panel: from inside only the face shows.
    let sleeve = Part::Solid(Albedo::Stand);
    let out = |c: Vec3f| {
        let r = vec3f(c.x, c.y, 0.0);
        c + r.normalize() * SLEEVE_R_M
    };
    let behind = vec3f(0.0, 0.0, -SLEEVE_R_M);
    for k in 0..n {
        m.rod(out(front[k]), out(mid[k]), SLEEVE_R_M, sleeve);
        m.rod(out(mid[k]), out(rear[k]) + behind, SLEEVE_R_M, sleeve);
        m.rod(out(rear[k]) + behind, out(rear[(k + 1) % n]) + behind, SLEEVE_R_M, sleeve);
    }
    m.cap(&rear, Part::Cloth);
    match cut {
        Cut::Closed => m.cap(&front, Part::Cloth),
        Cut::Baffle => m.cap(&mid, Part::Cloth),
        Cut::Open => {}
    }
    // The speed ring behind the back panel, the strobe head in it, its rear cap; the mount under the head's middle,
    // the spigot and the stand.
    let z_ring = -depth;
    let solid = Part::Solid(Albedo::Head);
    let back_r = rear.iter().map(|c| (c.x * c.x + c.y * c.y).sqrt()).fold(f32::INFINITY, f32::min);
    let ring_r = RING_R_M.min(0.95 * back_r);
    m.drum(0.0, 0.0, ring_r, z_ring - RING_LEN_M, z_ring - 0.001, solid);
    let head_r = HEAD_R_M.min(0.9 * ring_r);
    let (z_h0, z_h1) = (z_ring - RING_LEN_M - HEAD_LEN_M, z_ring - RING_LEN_M + 0.002);
    m.drum(0.0, 0.0, head_r, z_h0, z_h1, solid);
    m.drum(0.0, 0.0, CAP_R_M.min(0.85 * head_r), z_h0 - CAP_LEN_M, z_h0 + 0.002, solid);
    let z_mid = 0.5 * (z_h0 + z_h1);
    let mount_top = -0.8 * head_r;
    let mount_bottom = -head_r - MOUNT_HEIGHT_M;
    m.cuboid(vec3f(-MOUNT_HALF_M, mount_bottom, z_mid - MOUNT_DEPTH_M), vec3f(MOUNT_HALF_M, mount_top, z_mid + MOUNT_DEPTH_M), solid);
    // the tilt knob on the mount's side
    let knob_y = 0.5 * (mount_top + mount_bottom);
    m.rod(vec3f(MOUNT_HALF_M - 0.002, knob_y, z_mid), vec3f(MOUNT_HALF_M + 0.03, knob_y, z_mid), KNOB_R_M, solid);
    // the spigot down into the stand's clamp, its T-knob, and the stand
    let stand = Part::Solid(Albedo::Stand);
    let clamp_top = mount_bottom - 0.05;
    m.rod(vec3f(0.0, mount_bottom + 0.005, z_mid), vec3f(0.0, clamp_top + 0.005, z_mid), SPIGOT_R_M, stand);
    let clamp_bottom = clamp_top - CLAMP_HEIGHT_M;
    m.cuboid(vec3f(-CLAMP_HALF_M, clamp_bottom, z_mid - CLAMP_HALF_M), vec3f(CLAMP_HALF_M, clamp_top, z_mid + CLAMP_HALF_M), stand);
    let t_y = clamp_top - 0.3 * CLAMP_HEIGHT_M;
    m.rod(vec3f(0.0, t_y, z_mid + CLAMP_HALF_M - 0.002), vec3f(0.0, t_y, z_mid + CLAMP_HALF_M + 0.035), 0.006, stand);
    m.rod(vec3f(-0.025, t_y, z_mid + CLAMP_HALF_M + 0.035), vec3f(0.025, t_y, z_mid + CLAMP_HALF_M + 0.035), 0.007, stand);
    m.rod(vec3f(0.0, clamp_bottom + 0.005, z_mid), vec3f(0.0, STAND_FOOT_M.min(-ry - 0.3), z_mid), STAND_R_M, stand);
    Some((m.mesh, false, None))
}

#[cfg(test)]
mod tests {
    use super::super::{LightModel, ModelHit};
    use crate::image_light::ImageSet;
    use crate::light_fx::LightFx;
    use crate::modifier::apply_template;
    use crate::params::{LightParams, ModifierKind, SpaceParams};
    use makepad_draw::*;

    fn box_light(kind: ModifierKind, baffle: f32, yaw: f32) -> LightParams {
        let mut light = LightParams::default();
        apply_template(&mut light, kind);
        if let Some(m) = light.modifier.as_mut() {
            m.baffle = baffle;
        }
        let (w, h) = match kind {
            ModifierKind::Strip => (0.3, 1.2),
            ModifierKind::Octabox => (0.6, 0.6),
            _ => (0.6, 0.9),
        };
        light.space = Some(SpaceParams { position_m: [0.0, 0.0, -2.0], yaw_deg: yaw, pitch_deg: 0.0, roll_deg: 0.0, width_m: w, height_m: h, beam: 0.0 });
        light
    }

    fn model(light: &LightParams) -> Option<LightModel> {
        let fx = LightFx::new(light, &ImageSet::default());
        LightModel::new(light, fx.modifier())
    }

    const KINDS: [ModifierKind; 3] = [ModifierKind::Softbox, ModifierKind::Strip, ModifierKind::Octabox];

    /// Every box in 3D has a model, closed, with its inner baffle and open; in 2D it has none.
    #[test]
    fn a_3d_box_has_a_model() {
        for kind in KINDS {
            for baffle in [0.0, 0.7, 1.0] {
                let light = box_light(kind, baffle, 0.0);
                let m = model(&light).unwrap_or_else(|| panic!("{kind:?} {baffle}"));
                assert!(m.triangles() > 100 && m.triangles() < 4000, "{kind:?} {baffle}: {}", m.triangles());
                let mut flat = light.clone();
                flat.space = None;
                assert!(model(&flat).is_none());
            }
        }
    }

    /// Head-on, a ray into the face meets the cloth (the front, the inner cloth or the silver inside) at the very
    /// point of the flat face it travels along; past the outline it meets nothing (the stand below aside).
    #[test]
    fn head_on_a_box_is_its_flat_face() {
        for kind in KINDS {
            for baffle in [0.0, 0.7, 1.0] {
                let m = model(&box_light(kind, baffle, 0.0)).unwrap();
                let (hx, hy) = match kind {
                    ModifierKind::Strip => (0.15, 0.6),
                    _ => (0.3, if kind == ModifierKind::Octabox { 0.3 } else { 0.45 }),
                };
                for (fx, fy) in [(0.0, 0.0), (0.3, 0.4), (-0.6, 0.2), (0.12, -0.55), (0.5, 0.42)] {
                    let p = vec3f(fx * hx, fy * hy, -2.0);
                    let dir = p.normalize();
                    match m.cast(dir) {
                        Some(ModelHit::Face { at, mirror }) => {
                            assert!((at - dir).length() < 2.0e-5, "{kind:?} {baffle} {fx},{fy}: {at:?} for {dir:?}");
                            assert_eq!(mirror, 1.0);
                        }
                        other => panic!("{kind:?} {baffle} {fx},{fy}: {other:?}"),
                    }
                }
                assert!(m.cast(vec3f(1.2 * hx, 0.0, -2.0).normalize()).is_none(), "{kind:?} {baffle}: past the side");
                assert!(m.cast(vec3f(0.0, 1.2 * hy, -2.0).normalize()).is_none(), "{kind:?} {baffle}: above");
            }
        }
    }

    /// Turned round, the box is black nylon, the speed ring and the head: dull solids, no face.
    #[test]
    fn from_behind_a_box_is_black() {
        for kind in KINDS {
            for baffle in [0.0, 0.7, 1.0] {
                let m = model(&box_light(kind, baffle, 180.0)).unwrap();
                for (x, y) in [(0.0, 0.0), (0.08, 0.1), (-0.1, -0.05), (0.0, 0.12)] {
                    match m.cast(vec3f(x, y, -2.0).normalize()) {
                        Some(ModelHit::Solid { level }) => assert!(level > 0.0005 && level < 0.05, "{kind:?} {baffle}: {level}"),
                        other => panic!("{kind:?} {baffle} {x},{y}: {other:?}"),
                    }
                }
            }
        }
    }

    /// Turned 45 degrees, an open box shows its silver inside at the far wall and the black nylon of the near one.
    #[test]
    fn at_45_an_open_box_shows_inside_and_out() {
        let m = model(&box_light(ModifierKind::Softbox, 1.0, 45.0)).unwrap();
        let (mut face, mut solid) = (0, 0);
        for i in -30..=30 {
            for j in -30..=30 {
                match m.cast(vec3f(i as f32 * 0.01, j as f32 * 0.015, -2.0).normalize()) {
                    Some(ModelHit::Face { .. }) => face += 1,
                    Some(ModelHit::Solid { .. }) => solid += 1,
                    _ => {}
                }
            }
        }
        assert!(face > 100 && solid > 100, "face {face}, solid {solid}");
    }
}

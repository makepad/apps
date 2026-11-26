//! A fluorescent bank's 3D model (light_model.rs), built as the panels' are: in the face's own frame, its face
//! `Part::Cloth` on the housing's front so head-on it is the flat face point for point (its tubes, reflector, cover
//! and housing rim all the face's), everything else behind the face's plane and inside its outline, but for the
//! yoke and the stand.
//!
//! - The housing: a shallow box behind the face, as deep as a four-bank's 9 cm on its 30 cm (`FLUO_DEPTH` short
//!   half sizes), its back a ballast box along its middle.
//! - The end caps: at each end of the tubes a deeper block that holds the sockets (the face's dark ends), its satin
//!   coat standing out of the housing's back.
//! - The yoke: a U round the bank from pivots at the middle of its left and right sides (a lock knob on each), its
//!   bar under the bank, the spigot block and the stand's top section from it.
//!
//! The bank is laid out in short half sizes (the face's shorter half size is 1) and scaled into box units at the
//! end, so a long bank's housing, caps and yoke keep their thickness in metres.
use makepad_draw::*;

use super::super::params::ModifierParams;
use super::{Albedo, Built, Mesh, Modifier, Part};

/// The housing's depth, the end caps' length along the tubes and how far they stand out of the housing's back, the
/// ballast box (its share of the length and width, its depth), short half sizes.
const FLUO_DEPTH: f32 = 0.6;
const CAP_LONG: f32 = 0.16;
const CAP_BACK: f32 = 0.18;
const BALLAST_LONG: f32 = 0.55;
const BALLAST_WIDE: f32 = 0.45;
const BALLAST_DEPTH: f32 = 0.16;
/// The yoke: its gap to the housing, its bar's thickness and width, how far under the bank its bar runs, the stand's
/// tube and how far it reaches below, short half sizes.
const YOKE_GAP: f32 = 0.1;
const YOKE_BAR: f32 = 0.07;
const YOKE_WIDTH: f32 = 0.16;
const YOKE_DROP: f32 = 0.3;
const STAND_R: f32 = 0.06;
const STAND_REACH: f32 = 2.6;

/// A fluorescent bank's model: its face, housing, end caps, ballast and yoke.
pub(super) fn build(_params: &ModifierParams, modifier: &Modifier) -> Built {
    let half = modifier.half();
    let short = half.x.min(half.y).max(1.0e-6);
    // The box's half sizes and its z unit (their mean), in short half sizes.
    let (ax, ay) = (half.x / short, half.y / short);
    let az = 0.5 * (ax + ay);
    let wide = ax >= ay;
    let mut s = Mesh::new();
    s.bowl_lit = false;
    // The face on the housing's front.
    let front = vec3f(0.0, 0.0, 1.0);
    s.quad([vec3f(-ax, -ay, 0.0), vec3f(ax, -ay, 0.0), vec3f(ax, ay, 0.0), vec3f(-ax, ay, 0.0)], [front; 4], Part::Cloth);
    // The housing behind it, a hair behind the face's plane.
    let d = FLUO_DEPTH;
    let body = Part::Solid(Albedo::Head);
    s.cuboid(vec3f(-ax, -ay, -d), vec3f(ax, ay, -0.004), body);
    // The end caps at the tubes' ends, standing out of the back.
    let coat = Part::Solid(Albedo::Coat);
    for sign in [-1.0f32, 1.0] {
        let (lo, hi) = if wide {
            let x = sign * ax;
            (vec3f(x.min(x - sign * CAP_LONG), -ay, -d - CAP_BACK), vec3f(x.max(x - sign * CAP_LONG), ay, -0.003))
        } else {
            let y = sign * ay;
            (vec3f(-ax, y.min(y - sign * CAP_LONG), -d - CAP_BACK), vec3f(ax, y.max(y - sign * CAP_LONG), -0.003))
        };
        s.cuboid(lo, hi, coat);
    }
    // The ballast box along the back's middle.
    let (bl, bw) = if wide { (BALLAST_LONG * ax, BALLAST_WIDE * ay) } else { (BALLAST_WIDE * ax, BALLAST_LONG * ay) };
    s.cuboid(vec3f(-bl, -bw, -d - BALLAST_DEPTH), vec3f(bl, bw, -d), body);
    // The yoke from the middle of the left and right sides, round under the bank, to the stand.
    let z = -0.5 * d;
    let stand = Part::Solid(Albedo::Stand);
    let (x0, x1) = (ax + YOKE_GAP, ax + YOKE_GAP + YOKE_BAR);
    let (z0, z1) = (z - 0.5 * YOKE_WIDTH, z + 0.5 * YOKE_WIDTH);
    let low = -ay - YOKE_DROP;
    for side in [-1.0f32, 1.0] {
        s.cuboid(vec3f((side * x0).min(side * x1), low, z0), vec3f((side * x0).max(side * x1), 0.12, z1), stand);
        // The pivot's bolt through the cap and its lock knob outside the arm.
        let knob = Part::Solid(Albedo::Head);
        s.rod(vec3f(side * ax, 0.0, z), vec3f(side * x0, 0.0, z), 0.04, true, Part::Solid(Albedo::Metal));
        s.rod(vec3f(side * x1, 0.0, z), vec3f(side * (x1 + 0.07), 0.0, z), 0.1, true, knob);
        s.rod(vec3f(side * (x1 + 0.07), 0.0, z), vec3f(side * (x1 + 0.1), 0.0, z), 0.045, true, knob);
    }
    s.cuboid(vec3f(-x1, low - YOKE_BAR, z0), vec3f(x1, low, z1), stand);
    let top = low - YOKE_BAR;
    s.cuboid(vec3f(-0.07, top - 0.18, z - 0.07), vec3f(0.07, top, z + 0.07), stand);
    let clamp = top - 0.18;
    s.cuboid(vec3f(-0.11, clamp - 0.24, z - 0.11), vec3f(0.11, clamp, z + 0.11), stand);
    s.rod(vec3f(0.0, clamp - 0.24, z), vec3f(0.0, clamp - STAND_REACH, z), STAND_R, true, stand);
    // Into box units: lengths divided by each axis's unit, normals by the inverse transpose.
    let mut mesh = Mesh::new();
    mesh.bowl_lit = false;
    let to_box = |p: Vec3f| vec3f(p.x / ax, p.y / ay, p.z / az);
    let normal = |n: Vec3f| {
        let m = vec3f(n.x * ax, n.y * ay, n.z * az);
        if m.length() > 1.0e-9 { m.normalize() } else { n }
    };
    for ((c, n), part) in s.corners.iter().zip(&s.normals).zip(&s.parts) {
        mesh.tri(c.map(to_box), n.map(normal), *part);
    }
    (mesh, false, None)
}

#[cfg(test)]
mod tests {
    use super::super::{LightModel, ModelHit};
    use crate::image_light::ImageSet;
    use crate::light_fx::LightFx;
    use crate::modifier::apply_template;
    use crate::params::{LightParams, ModifierKind, SpaceParams};
    use makepad_draw::*;

    fn bank(yaw: f32, w: f32, h: f32, diffusion: Option<f32>) -> LightParams {
        let mut light = LightParams::default();
        apply_template(&mut light, ModifierKind::Fluorescent);
        light.modifier.as_mut().unwrap().diffusion = diffusion;
        light.space = Some(SpaceParams { position_m: [0.0, 0.0, -2.5], yaw_deg: yaw, pitch_deg: 0.0, roll_deg: 0.0, width_m: w, height_m: h, beam: 0.0 });
        light
    }

    fn model(light: &LightParams) -> Option<LightModel> {
        let fx = LightFx::new(light, &ImageSet::default());
        LightModel::new(light, fx.modifier())
    }

    /// A 3D bank (long, tall, square; open and under an egg-crate) has a model: head-on a ray through its face meets
    /// it at the very point of the flat face it travels along; under the bank the yoke's bar, past its side
    /// nothing; from behind the housing, a dull solid; in 2D it has none.
    #[test]
    fn head_on_a_fluorescent_bank_is_its_flat_face() {
        for (w, h, diffusion) in [(1.2, 0.3, None), (0.3, 1.2, None), (0.6, 0.6, Some(0.4))] {
            let light = bank(0.0, w, h, diffusion);
            let m = model(&light).unwrap_or_else(|| panic!("{w} x {h}: no model"));
            assert!(m.triangles() > 50 && m.triangles() < 5000, "{}", m.triangles());
            for p in [vec3f(0.3 * w, 0.3 * h, -2.5), vec3f(-0.45 * w, -0.45 * h, -2.5), vec3f(0.0, 0.0, -2.5)] {
                let dir = p.normalize();
                match m.cast(dir) {
                    Some(ModelHit::Face { at, mirror }) => {
                        assert!((at - dir).length() < 2.0e-5, "{w} x {h}: {at:?} for {dir:?}");
                        assert!((mirror - 1.0).abs() < 1.0e-6);
                    }
                    other => panic!("{w} x {h}: {other:?}"),
                }
            }
            assert!(m.cast(vec3f(0.5 * w + 0.4, 0.4 * h + 0.3, -2.5).normalize()).is_none(), "{w} x {h}: past the side");
            let back = model(&bank(180.0, w, h, diffusion)).unwrap();
            match back.cast(vec3f(0.1 * w, 0.2 * h, -2.5).normalize()) {
                Some(ModelHit::Solid { level }) => assert!(level > 0.0 && level < 0.1, "{level}"),
                other => panic!("{other:?}"),
            }
            let mut flat = light.clone();
            flat.space = None;
            assert!(model(&flat).is_none());
        }
    }
}

//! Studio lights as small 3D models (the owner's direction, 2026-11-01 and 2026-11-12: "an umbrella becomes a 3D
//! umbrella with umbrella texture material").
//!
//! A light placed in 3D (`LightParams::in_space`, plan P1) of a kind that has a model is drawn as that model instead
//! of as its flat face. The model is generated here in code, small and parametric, from the light's size and its
//! modifier's settings (no model files): a triangle list under the bounding volume hierarchy
//! (tri_bvh.rs `TriBvh`), cast per ray in the studio's draw loop (`Studio::apply_entries`, the bake and the
//! preview alike).
//!
//! The model lives in the face's own frame (`space::space_frame`): x to the viewer's right as the face shows it, y
//! up, z out of its front, in box units (the face's half width, half height and their mean along z: its hem at
//! radius 1, as the modifier's face coordinates). What a ray meets is one of:
//! - the emitting surface (an umbrella's bowl seen from inside, and what lies on it, its ribs and stretchers): drawn
//!   by the light's own code at the point of its flat face that the camera facing it head-on would see there. The
//!   hit is projected from that camera (on the face's axis at the light's distance) onto the face's plane, and the
//!   flat face's direction through that point is drawn as the flat face draws it (its shape, colour, modifier, wear
//!   and gobo), the parts the model draws as geometry left out of the modifier (`Modifier::factor_bare`). So facing
//!   the camera the model is the flat face, point for point, and turned the face's pattern stays on the cloth;
//! - the back of the cloth: a translucent cloth glows with what lights it from inside (`Modifier::back`), an opaque
//!   one is a solid;
//! - a solid (the strobe head, its yoke and stand, the shaft and its runner, the rib tips, a black back): a dull
//!   material lit by the studio round it and by the light's own bowl (`Model::solid`), in the light's colour;
//! - glass standing in front of the emitting surface (a bare head's flash tube and modelling lamp, a fresnel's
//!   stepped lens, a spot bulb's filament): the whole face (`Part::Whole`; the cloth behind draws the face without
//!   it), turned round the glass's own middle so seen turned it shows what the same line through it shows head-on;
//!   head-on it is the flat face.
//!
//! A silver bowl mirrors its flash: where the camera sees the flash's image in the bowl moves with the view. The face
//! carries the flash's image as it reads head-on, so a model scales it by the image's share at this view over its
//! share head-on (`Mirror::ratio`): 1 head-on, and turned the hot zone leaves where the flash's image left.
//!
//! Lights without a model, 2D lights and 3D lights of other kinds, are drawn exactly as before. What a new kind needs
//! to get a model: a builder in `LightModel::new`'s match (its parts in box units, `Part::Face` where the face's
//! pattern shows, `Part::Back` for its cloth's back), and in modifier.rs its face without what the geometry now
//! draws (`factor_bare`) and its back if light passes the cloth (`back`).
//!
//! Its key light (`Studio::key`), its share in a room and its floor mirror stay the flat face's: they integrate
//! energy, which the model and the face share head-on.
use std::sync::Arc;

use makepad_draw::*;

use super::tri_bvh::TriBvh;
use super::modifier::Modifier;
use super::params::{Blend, CandleMount, CandleParams, LightParams, ModifierKind, SpaceParams};
use super::shaped::box_x_sign;
use super::space::{space_frame, space_half};

/// The panels' builders: an LED panel, a nude COB, a ring light, a reflector and a gradient.
#[path = "light_model_panel.rs"]
mod panel;
/// The heads and dishes' builders (a bare flash head, a beauty dish, a fresnel).
#[path = "light_model_head.rs"]
mod head;
/// The boxes' builders (a softbox, a strip, an octabox; closed, inner baffle or open).
#[path = "light_model_box.rs"]
mod boxes;
/// A fluorescent bank's builder.
#[path = "light_model_fluo.rs"]
mod fluo;
/// Candles, their flames, holders and mounts (a model of its own, in metres).
#[path = "light_model_candle.rs"]
pub mod candle;

/// Rays per side of a texel a model is cast with when a lookup stands for one: a triangle's edge is hard, so the
/// head's and the stand's outlines and the hem are anti-aliased over 2 x 2 rays (the flat face drew a soft edge).
const MODEL_RAYS: usize = 2;
/// An umbrella's bowl: steps from the hub to the hem and across each panel.
const BOWL_RINGS: usize = 28;
const BOWL_ACROSS: usize = 8;
/// Sides of a rod (a rib, a stretcher, the shaft, the stand) and segments along a rib.
const ROD_SIDES: usize = 6;
const RIB_SEGMENTS: usize = 14;
/// Radii in box units (the hem's radius 1): a rib, a stretcher and the shaft (RLS024-025: a rib about a hundredth
/// of the radius wide), the hub's and the runner's sleeves.
const RIB_R: f32 = 0.005;
const STRUT_R: f32 = 0.006;
const SHAFT_R: f32 = 0.009;
const HUB_R: f32 = 0.026;
/// How far out along its rib a stretcher meets it (RLS024's joint at 0.55 of the radius), how far in front of the
/// hub the runner rides on the shaft, and how far a rib's tip runs past the hem.
const STRUT_REACH: f32 = 0.55;
const RUNNER_IN: f32 = 0.2;
const RIB_TIP: f32 = 0.025;
/// How far below the hem the stand reaches, box units: its top section (a room's own stand, room.rs, stands under
/// the light from the floor).
const STAND_FOOT: f32 = -2.6;
/// How the solids are lit, in units of the face's mean radiance: the studio's fill round them (a share of pi times
/// it, a little more from above), the light's own bowl on what faces it, and their albedos. A strobe head's back
/// reads 0.015 of the face (RLS024's head; the flat face's silhouette is 1.5 %).
const ROOM_FILL: f32 = 0.19;
const BOWL_FILL: f32 = 0.6;
const HEAD_ALBEDO: f32 = 0.08;
const STAND_ALBEDO: f32 = 0.07;
const METAL_ALBEDO: f32 = 0.25;
const BLACK_CLOTH_ALBEDO: f32 = 0.05;
/// The sheen of a black nylon back, at the fill straight above mirrored in it.
const BLACK_CLOTH_SHEEN: f32 = 0.12;
/// Spun aluminium (a reflector's or a beauty dish's outside): its dull share and its sheen, the studio's fill from
/// above mirrored in it along its spinning.
const SPUN_ALBEDO: f32 = 0.2;
const SPUN_SHEEN: f32 = 0.6;
/// A flash head's satin powder coat: as dark as the head's, with a faint sheen that shows its form.
const COAT_SHEEN: f32 = 0.2;

/// What a triangle of a model is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The emitting surface's cloth: its front shows the face, its back `Part::Back`'s way.
    Cloth,
    /// What lies on the emitting surface (ribs, stretchers): the face's pattern at its place shows on it.
    Face,
    /// A dull solid of the given albedo.
    Solid(Albedo),
    /// What stands in front of the emitting surface and shows the whole face (`Modifier::factor`, which the cloth
    /// round it leaves out: a bare head's flash tube, its modelling lamp's dome, a fresnel's stepped lens): the face
    /// at the point the camera head-on sees, the hit turned first (`Turn`) so glass seen turned shows what the same
    /// line of sight through it shows head-on.
    Whole(Turn),
}

/// How a `Part::Whole` hit is turned before it is drawn: the camera's ray at it turned onto the ray the camera
/// head-on sends through it, round the part's own middle, so its glow (a tube's bright column, a frosted dome's
/// hot middle) stays toward the viewer; head-on there is nothing to turn and it is the flat face point for point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    /// Not turned: drawn where it stands (a tube's legs, a lens's steps).
    None,
    /// A tube bent into a ring facing the camera (`Mesh::ring`): turned round its centre line, across it only.
    Ring,
    /// A bulb round a point (`Mesh::balls`, by index): turned round it.
    Ball(u8),
}

/// A solid's material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Albedo {
    Head,
    Stand,
    Metal,
    /// Spun aluminium: dull and its sheen (`LightModel::glossy`).
    Spun,
    /// A flash head's satin powder coat (`LightModel::glossy`).
    Coat,
}

impl Albedo {
    fn albedo(self) -> f32 {
        match self {
            Albedo::Head => HEAD_ALBEDO,
            Albedo::Stand => STAND_ALBEDO,
            Albedo::Metal => METAL_ALBEDO,
            Albedo::Spun => SPUN_ALBEDO,
            Albedo::Coat => HEAD_ALBEDO,
        }
    }
}

/// What a ray meets on a model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ModelHit {
    /// The emitting surface's front: the light drawn along `at`, the direction of the flat face's point it shows
    /// (`LightModel`'s head), its flash image scaled by `mirror` (1 but on a silver bowl).
    Face { at: Vec3f, mirror: f32 },
    /// A translucent cloth's back, behind the flat face's point along `at`.
    Back { at: Vec3f },
    /// A solid, its radiance as a share of the light's colour.
    Solid { level: f32 },
    /// What stands in front of the face (`Part::Whole`): the whole face drawn along `at`.
    Whole { at: Vec3f },
    /// A candle light's own shading (light_model_candle.rs): covering only `opacity` of what is behind it.
    Candle(candle::CandleHit),
}

impl ModelHit {
    /// How much of what lies behind it the hit hides (a candle's glass and flames pass the rest).
    pub fn opacity(&self) -> f32 {
        match self {
            ModelHit::Candle(hit) => hit.opacity,
            _ => 1.0,
        }
    }
}

/// The triangles of a model, box units, with their corners' normals and parts.
struct Mesh {
    corners: Vec<[Vec3f; 3]>,
    normals: Vec<[Vec3f; 3]>,
    parts: Vec<Part>,
    /// How far out from the middle (box units, as the face draws it head-on) a bowl's mirror starts: what lies
    /// inside it (a head's front in a reflector's throat) is no mirror. 0: the mirror covers the face.
    mirror_inner: f32,
    /// The solids are lit by the light's own bowl in front of them (an umbrella's); false for a light whose solids
    /// all stand behind what it emits.
    bowl_lit: bool,
    /// A tube bent into a ring facing the camera (`Turn::Ring`): its middle and its centre line's radius, box units.
    ring: (Vec3f, f32),
    /// The middles of bulbs (`Turn::Ball`), box units.
    balls: Vec<Vec3f>,
}

impl Mesh {
    fn new() -> Mesh {
        Mesh { corners: Vec::new(), normals: Vec::new(), parts: Vec::new(), mirror_inner: 0.0, bowl_lit: true, ring: (Vec3f::default(), 0.0), balls: Vec::new() }
    }

    fn tri(&mut self, p: [Vec3f; 3], n: [Vec3f; 3], part: Part) {
        self.corners.push(p);
        self.normals.push(n);
        self.parts.push(part);
    }

    /// The quad a, b, c, d (in order round it) as two triangles.
    fn quad(&mut self, p: [Vec3f; 4], n: [Vec3f; 4], part: Part) {
        self.tri([p[0], p[1], p[2]], [n[0], n[1], n[2]], part);
        self.tri([p[0], p[2], p[3]], [n[0], n[2], n[3]], part);
    }

    /// A rod of radius `r` from `a` to `b`, `ROD_SIDES` round, closed at both ends when `caps`.
    fn rod(&mut self, a: Vec3f, b: Vec3f, r: f32, caps: bool, part: Part) {
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
            self.quad([a + n0 * r, a + n1 * r, b + n1 * r, b + n0 * r], [n0, n1, n1, n0], part);
            if caps {
                self.tri([a, a + n1 * r, a + n0 * r], [-w, -w, -w], part);
                self.tri([b, b + n0 * r, b + n1 * r], [w, w, w], part);
            }
        }
    }

    /// A cylinder of radius `r` round the z axis line through (`x`, `y`), from `z0` to `z1`, closed, smoother than a
    /// rod (a strobe head's dish and the hub).
    fn drum(&mut self, x: f32, y: f32, r: f32, z0: f32, z1: f32, part: Part) {
        const SIDES: usize = 20;
        let c0 = vec3f(x, y, z0);
        let c1 = vec3f(x, y, z1);
        let (back, front) = (vec3f(0.0, 0.0, -1.0), vec3f(0.0, 0.0, 1.0));
        for k in 0..SIDES {
            let t0 = std::f32::consts::TAU * k as f32 / SIDES as f32;
            let t1 = std::f32::consts::TAU * (k + 1) as f32 / SIDES as f32;
            let (n0, n1) = (vec3f(t0.cos(), t0.sin(), 0.0), vec3f(t1.cos(), t1.sin(), 0.0));
            self.quad([c0 + n0 * r, c0 + n1 * r, c1 + n1 * r, c1 + n0 * r], [n0, n1, n1, n0], part);
            self.tri([c0, c0 + n1 * r, c0 + n0 * r], [back; 3], part);
            self.tri([c1, c1 + n0 * r, c1 + n1 * r], [front; 3], part);
        }
    }

    /// The box from `lo` to `hi`.
    fn cuboid(&mut self, lo: Vec3f, hi: Vec3f, part: Part) {
        let p = |i: usize| vec3f(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
        // each face: its four corners round it and its normal
        let faces: [([usize; 4], Vec3f); 6] = [
            ([0, 2, 6, 4], vec3f(-1.0, 0.0, 0.0)),
            ([1, 5, 7, 3], vec3f(1.0, 0.0, 0.0)),
            ([0, 4, 5, 1], vec3f(0.0, -1.0, 0.0)),
            ([2, 3, 7, 6], vec3f(0.0, 1.0, 0.0)),
            ([0, 1, 3, 2], vec3f(0.0, 0.0, -1.0)),
            ([4, 6, 7, 5], vec3f(0.0, 0.0, 1.0)),
        ];
        for (c, n) in faces {
            self.quad([p(c[0]), p(c[1]), p(c[2]), p(c[3])], [n; 4], part);
        }
    }
}

/// Where the strobe head and its stand stand in front of an umbrella, box units, as the face draws their silhouette
/// head-on (modifier.rs `head_and_stand`, silver_deep.rs `stand`): the head's reflector a disc of radius `r` round
/// (0, `y`); the yoke's middle and half size; the spigot's; the stand's top and radius.
#[derive(Clone, Copy, Debug)]
struct HeadLayout {
    r: f32,
    y: f32,
    yoke_y: f32,
    yoke: Vec2f,
    spigot_y: f32,
    spigot: Vec2f,
    stand_top: f32,
    stand_r: f32,
}

impl HeadLayout {
    /// The layout the face's silhouette draws: a deep silver bowl's own (silver_deep.rs), else the head's radius
    /// half the modifier's `plate` (modifier.rs `HEAD_DROP` of it under the hub). None without a head.
    fn of(deep: bool, plate: f32) -> Option<HeadLayout> {
        if deep {
            return Some(HeadLayout { r: 0.082, y: -0.072, yoke_y: -0.155, yoke: vec2f(0.083, 0.062), spigot_y: -0.25, spigot: vec2f(0.021, 0.025), stand_top: -0.29, stand_r: 0.02 });
        }
        let h = 0.5 * plate;
        if h < 0.01 {
            return None;
        }
        let y = -1.4 * h;
        Some(HeadLayout { r: h, y, yoke_y: y - 1.75 * h, yoke: vec2f(0.32 * h, 0.45 * h), spigot_y: y - 2.5 * h, spigot: vec2f(0.018, 0.4 * h), stand_top: y - 2.2 * h, stand_r: 0.017 })
    }
}

/// A silver bowl's mirror of its flash. The face draws the flash's image as the camera head-on sees it: round the
/// hub, as far out as the bowl turns the camera's rays back toward the flash. Head-on, the bowl's smooth sphere
/// mirrors the camera's ray off at an angle to the axis that grows with the point's distance from the hub
/// (`angle`, tabled); a point seen along any ray shows the flash as the face does head-on at the distance whose
/// mirrored ray leaves at the same angle (the flash as a far light, a reflection map). The face's smooth hot zone
/// (`envelope`, over the radius) there over its level where the point's head-on ray maps scales the face: 1
/// head-on, and turned, the hot zone moves over the cloth as the bowl's mirror image does, while its folds and
/// foil stay where they are.
#[derive(Clone, Debug)]
struct Mirror {
    /// The bowl's sphere's middle and the camera head-on, metres in the model's frame.
    centre: Vec3f,
    camera: Vec3f,
    /// The mirrored ray's angle to the axis, radians, head-on at MIRROR_STEPS even steps of the face's radius out
    /// to MIRROR_REACH (held increasing), and the face's hot zone at those radii.
    angle: Vec<f32>,
    envelope: Vec<f32>,
    /// Inside this radius (metres, on the face's plane as the camera head-on sees it) there is no mirror
    /// (`Mesh::mirror_inner`).
    inner: f32,
}

/// Steps of a mirror's tables over the face's radius, and how far out they reach (box units, past the hem).
const MIRROR_STEPS: usize = 256;
const MIRROR_REACH: f32 = 1.5;

impl Mirror {
    fn radius_at(k: usize) -> f32 {
        MIRROR_REACH * k as f32 / (MIRROR_STEPS - 1) as f32
    }

    /// The face's hot zone `envelope` at `u` box units from the hub, linear between its steps.
    fn level(&self, u: f32) -> f32 {
        let x = (u / MIRROR_REACH).clamp(0.0, 1.0) * (MIRROR_STEPS - 1) as f32;
        let k = (x as usize).min(MIRROR_STEPS - 2);
        let f = x - k as f32;
        self.envelope[k] + (self.envelope[k + 1] - self.envelope[k]) * f
    }

    /// The distance from the hub (box units, the face's) whose ray head-on leaves at `angle` to the axis: the
    /// table read backward, its last step past its end.
    fn radius_for(&self, angle: f32) -> f32 {
        let i = self.angle.partition_point(|a| *a < angle);
        if i == 0 {
            return 0.0;
        }
        if i >= MIRROR_STEPS {
            return MIRROR_REACH;
        }
        let (a0, a1) = (self.angle[i - 1], self.angle[i]);
        let f = if a1 > a0 { (angle - a0) / (a1 - a0) } else { 0.0 };
        Self::radius_at(i - 1) + (Self::radius_at(i) - Self::radius_at(i - 1)) * f
    }

    /// The angle to the axis the ray along `rd` (unit) leaves at once the bowl at `p` (metres, the model's frame)
    /// mirrors it.
    fn angle(centre: Vec3f, p: Vec3f, rd: Vec3f) -> f32 {
        let n = (centre - p).normalize();
        let r = rd - n * (2.0 * rd.dot(n));
        (r.x * r.x + r.y * r.y).sqrt().atan2(r.z)
    }

    /// The flash image's level at `p` (metres, the model's frame) seen along `rd` (unit, toward `p`), over its
    /// level there seen head-on.
    fn ratio(&self, p: Vec3f, rd: Vec3f) -> f32 {
        if self.inner > 0.0 && (p.x * p.x + p.y * p.y).sqrt() * self.camera.z / (self.camera.z - p.z).max(1.0e-4) < self.inner {
            return 1.0;
        }
        let seen = Self::angle(self.centre, p, rd);
        let head_on = Self::angle(self.centre, p, (p - self.camera).normalize());
        if seen == head_on {
            return 1.0;
        }
        self.level(self.radius_for(seen)) / self.level(self.radius_for(head_on)).max(1.0e-6)
    }
}

/// A light's model, placed: its mesh, where it stands and what its face reads.
#[derive(Clone)]
pub struct LightModel {
    mesh: Arc<(TriBvh, Vec<[Vec3f; 3]>, Vec<Part>)>,
    /// The face's frame: the unit direction to its middle, its axes (x to the viewer's right, y up, z out of its
    /// front) and its middle, metres.
    center: Vec3f,
    x: Vec3f,
    y: Vec3f,
    z: Vec3f,
    pos: Vec3f,
    /// The camera head-on: the face's distance, metres.
    dist: f32,
    /// Metres per box unit along x, y and z.
    scale: Vec3f,
    /// The flat face's tangent half extents (`space::space_half`).
    half: Vec2f,
    /// The model's bounding sphere seen from the camera: its middle's direction, and the cosine of its half angle
    /// (-1 when the camera is inside it).
    bound_dir: Vec3f,
    bound_cos: f32,
    bound_angle: f32,
    /// The cloth's back is drawn by `Modifier::back` (a white umbrella's translucent cloth, a reflector's fabric
    /// back, a scrim), else its back is a black solid.
    translucent: bool,
    mirror: Option<Arc<Mirror>>,
    /// A reflector's metallised cloth's mirror of its source (light_model_panel.rs).
    sheen: Option<Arc<panel::Sheen>>,
    /// The bowl's light on what faces it: a disc at depth `bowl_z` (metres), radius `bowl_r`, facing +z.
    bowl_z: f32,
    bowl_r: f32,
    /// What `Part::Whole` turns round (`Mesh::ring`, `Mesh::balls`), box units.
    ring: (Vec3f, f32),
    balls: Arc<Vec<Vec3f>>,
    /// A candle light's model (its frame is `x`, `y`, `z` at `pos`, in metres) and the scene's light on it.
    candle: Option<Arc<candle::CandleModel>>,
    candle_light: candle::CandleLight,
}

impl std::fmt::Debug for LightModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LightModel({} triangles at {:?})", self.mesh.0.triangles(), self.pos)
    }
}

impl LightModel {
    /// The model of `light` (as clamp() holds it, read as it is) with the face `modifier` (its `LightFx`'s): None
    /// for a 2D light, for a light that is not an emitter, and for a kind that has no model.
    pub fn new(light: &LightParams, modifier: Option<&Modifier>) -> Option<LightModel> {
        let light = light.clamped();
        let space = light.in_space()?;
        if light.blend() != Blend::Add || light.is_caster() {
            return None;
        }
        let params = light.modifier.as_ref()?.clamped();
        let modifier = modifier?;
        let kind = params.kind();
        if kind == ModifierKind::Candle {
            return Some(LightModel::candle(space, &params.candle_params()));
        }
        let (mesh, translucent, mirror_at) = match kind {
            ModifierKind::Umbrella | ModifierKind::SilverUmbrella => umbrella(kind, params.baffle, params.ribs, params.plate, modifier)?,
            ModifierKind::LedPanel | ModifierKind::RingLight | ModifierKind::Reflector | ModifierKind::Gradient => panel::build(kind, &params, modifier)?,
            ModifierKind::Bulb => head::bulb(&params, modifier, 0.25 * (space.width_m + space.height_m), space.distance())?,
            ModifierKind::BeautyDish => head::beauty_dish(&params, modifier, 0.25 * (space.width_m + space.height_m))?,
            ModifierKind::Fresnel => head::fresnel(&params, modifier, 0.25 * (space.width_m + space.height_m))?,
            ModifierKind::Softbox | ModifierKind::Strip | ModifierKind::Octabox => boxes::build(kind, params.baffle, vec2f(0.5 * space.width_m, 0.5 * space.height_m), space.distance())?,
            ModifierKind::Fluorescent => fluo::build(&params, modifier),
            _ => return None,
        };
        let frame = space_frame(space);
        let sx = box_x_sign();
        let (x, y) = (frame.right * sx, frame.up);
        let z = -frame.face?;
        let [px, py, pz] = space.position_m;
        let pos = vec3f(px, py, pz);
        let dist = space.distance().max(1.0e-3);
        let (rx, ry) = (0.5 * space.width_m, 0.5 * space.height_m);
        let scale = vec3f(rx, ry, 0.5 * (rx + ry));
        // The bounding sphere, metres.
        let (mut lo, mut hi) = (Vec3f::all(f32::INFINITY), Vec3f::all(f32::NEG_INFINITY));
        for c in &mesh.corners {
            for p in c {
                let m = *p * scale;
                lo = Vec3f::min_componentwise(lo, m);
                hi = Vec3f::max_componentwise(hi, m);
            }
        }
        let mid = (lo + hi) * 0.5;
        let radius = mesh.corners.iter().flatten().map(|p| (*p * scale - mid).length()).fold(0.0f32, f32::max) * 1.001 + 1.0e-4;
        let world_mid = pos + x * mid.x + y * mid.y + z * mid.z;
        let to = world_mid.length();
        let (bound_dir, bound_angle) = if to > radius { (world_mid / to, (radius / to).asin()) } else { (vec3f(0.0, 0.0, -1.0), std::f32::consts::PI) };
        let mirror = mirror_at.map(|(depth, sphere)| {
            // Head-on, the point of the bowl `rho` (box units) from the hub: where the face draws it, and the angle
            // the camera's ray leaves it at once the sphere mirrors it; tabled as the face's radius steps out.
            let centre = vec3f(0.0, 0.0, (-depth + sphere) * scale.z);
            let camera = vec3f(0.0, 0.0, dist);
            let r = 0.5 * (scale.x + scale.y);
            let seen = |rho: f32| {
                let z = (-depth + sphere - (sphere * sphere - rho * rho).max(0.0).sqrt()) * scale.z;
                let p = vec3f(rho * r, 0.0, z);
                (rho * dist / (dist - z).max(1.0e-4), Mirror::angle(centre, p, (p - camera).normalize()))
            };
            let mut angle: Vec<f32> = Vec::with_capacity(MIRROR_STEPS);
            for k in 0..MIRROR_STEPS {
                // the bowl's radius whose face radius is this step's (bisected; the face's radius grows with it)
                let target = Mirror::radius_at(k);
                let (mut lo, mut hi) = (0.0f32, sphere);
                for _ in 0..32 {
                    let mid = 0.5 * (lo + hi);
                    if seen(mid).0 < target {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                let a = seen(0.5 * (lo + hi)).1;
                let a = angle.last().map_or(a, |last| a.max(*last + 1.0e-6));
                angle.push(a);
            }
            let envelope = (0..MIRROR_STEPS).map(|k| modifier.mirror_envelope(Mirror::radius_at(k)).unwrap_or(1.0)).collect();
            Arc::new(Mirror { centre, camera, angle, envelope, inner: mesh.mirror_inner * r })
        });
        let bowl_lit = mesh.bowl_lit;
        let (ring, balls) = (mesh.ring, Arc::new(mesh.balls));
        let bowl_z = mesh.corners.iter().flatten().map(|p| p.z).fold(0.0f32, f32::min) * 0.5 * scale.z;
        let (corners, normals, parts) = (mesh.corners, mesh.normals, mesh.parts);
        Some(LightModel {
            mesh: Arc::new((TriBvh::new(corners), normals, parts)),
            center: frame.center,
            x,
            y,
            z,
            pos,
            dist,
            scale,
            half: space_half(space),
            bound_dir,
            bound_cos: bound_angle.cos(),
            bound_angle,
            translucent,
            mirror,
            sheen: panel::Sheen::of(kind, modifier),
            bowl_z,
            bowl_r: if bowl_lit { 0.5 * (rx + ry) } else { 0.0 },
            ring,
            balls,
            candle: None,
            candle_light: candle::CandleLight::Dark,
        })
    }

    pub fn triangles(&self) -> usize {
        self.candle.as_ref().map_or_else(|| self.mesh.0.triangles(), |c| c.triangles())
    }

    /// A candle light's model standing at `space`'s place (its mount's point), upright, turned by its yaw.
    fn candle(space: &SpaceParams, params: &CandleParams) -> LightModel {
        let model = candle::CandleModel::new(params);
        let [px, py, pz] = space.position_m;
        let pos = vec3f(px, py, pz);
        let y = vec3f(0.0, 1.0, 0.0);
        let level = vec3f(-pos.x, 0.0, -pos.z);
        let mut z0 = if level.length() > 1.0e-5 { level.normalize() } else { vec3f(0.0, 0.0, 1.0) };
        if params.mount() == CandleMount::Wall {
            // a sconce faces straight out of the wall it hangs on: of the room's four, the one whose face the camera
            // is most in front of
            z0 = if z0.x.abs() > z0.z.abs() { vec3f(z0.x.signum(), 0.0, 0.0) } else { vec3f(0.0, 0.0, z0.z.signum()) };
        }
        let x0 = Vec3f::cross(y, z0);
        let yaw = space.yaw_deg.to_radians();
        let z = z0 * yaw.cos() + x0 * yaw.sin();
        let x = Vec3f::cross(y, z);
        let (mid, radius) = model.bound;
        let world_mid = pos + x * mid.x + y * mid.y + z * mid.z;
        let to = world_mid.length();
        let (bound_dir, bound_angle) = if to > radius { (world_mid / to, (radius / to).asin()) } else { (vec3f(0.0, 0.0, -1.0), std::f32::consts::PI) };
        LightModel {
            mesh: Arc::new((TriBvh::new(Vec::new()), Vec::new(), Vec::new())),
            center: pos / pos.length().max(1.0e-4),
            x,
            y,
            z,
            pos,
            dist: pos.length().max(1.0e-3),
            scale: Vec3f::all(1.0),
            half: space_half(space),
            bound_dir,
            bound_cos: bound_angle.cos(),
            bound_angle,
            translucent: false,
            mirror: None,
            sheen: None,
            bowl_z: 0.0,
            bowl_r: 0.0,
            ring: (Vec3f::default(), 0.0),
            balls: Arc::new(Vec::new()),
            candle: Some(Arc::new(model)),
            candle_light: candle::CandleLight::Dark,
        }
    }

    /// A candle light's flames as lights: each its middle (the map's frame, metres) and its intensity in the
    /// light's colour (radiance x m2). Empty for any other model.
    pub fn candle_flames(&self) -> Vec<(Vec3f, f32)> {
        let Some(c) = &self.candle else { return Vec::new() };
        c.flames().into_iter().map(|(p, i)| (self.pos + self.x * p.x + self.y * p.y + self.z * p.z, i)).collect()
    }

    /// Where a candle light's model stands (the map's frame, metres): its bound's middle. None for another model.
    pub fn candle_middle(&self) -> Option<Vec3f> {
        let c = self.candle.as_ref()?;
        let (m, _) = c.bound;
        Some(self.pos + self.x * m.x + self.y * m.y + self.z * m.z)
    }

    /// Sets the scene's light on a candle light's model (`candle::CandleLight`); nothing for another model.
    pub fn set_candle_light(&mut self, light: candle::CandleLight) {
        if self.candle.is_some() {
            self.candle_light = light;
        }
    }

    /// A candle light's cast of the camera's ray along `dir` (unit, the map's frame).
    fn cast_candle(&self, c: &candle::CandleModel, dir: Vec3f, texel: f32) -> Option<ModelHit> {
        let o = vec3f(-self.pos.dot(self.x), -self.pos.dot(self.y), -self.pos.dot(self.z));
        let d = vec3f(dir.dot(self.x), dir.dot(self.y), dir.dot(self.z));
        let (x, y, z) = (self.x, self.y, self.z);
        let world = move |v: Vec3f| x * v.x + y * v.y + z * v.z;
        c.cast(o, d, texel, &self.candle_light, &world).map(ModelHit::Candle)
    }

    /// Casts the model over a lookup along `dir` (unit) for a map texel `texel` radians tall (0: one ray): 2 x 2
    /// rays over the texel, each that meets the model handed to `each` with its direction, what it met and its
    /// weight. What the model covers of the texel (0..=1).
    pub fn trace(&self, dir: Vec3f, texel: f32, mut each: impl FnMut(Vec3f, ModelHit, f32)) -> f32 {
        // Outside the bounding cone (widened by the texel) nothing is met.
        if self.bound_cos > -1.0 && dir.dot(self.bound_dir) < (self.bound_angle + texel).min(std::f32::consts::PI).cos() {
            return 0.0;
        }
        let n = if texel > 0.0 { MODEL_RAYS } else { 1 };
        let w = 1.0 / (n * n) as f32;
        let (e1, e2) = if n > 1 {
            let e1 = if dir.y.abs() < 0.9 { Vec3f::cross(dir, vec3f(0.0, 1.0, 0.0)) } else { Vec3f::cross(dir, vec3f(1.0, 0.0, 0.0)) }.normalize();
            (e1, Vec3f::cross(dir, e1))
        } else {
            (Vec3f::default(), Vec3f::default())
        };
        let mut cover = 0.0;
        for i in 0..n {
            for j in 0..n {
                let d = if n == 1 {
                    dir
                } else {
                    let (a, b) = (((i as f32 + 0.5) / n as f32 - 0.5) * texel, ((j as f32 + 0.5) / n as f32 - 0.5) * texel);
                    (dir + e1 * a + e2 * b).normalize()
                };
                let hit = match &self.candle {
                    Some(c) => self.cast_candle(c, d, texel / n as f32),
                    None => self.cast(d),
                };
                if let Some(hit) = hit {
                    cover += w * hit.opacity();
                    each(d, hit, w);
                }
            }
        }
        cover
    }

    /// What the ray from the camera along `dir` (unit) first meets on the model, if it meets it.
    pub fn cast(&self, dir: Vec3f) -> Option<ModelHit> {
        if let Some(c) = &self.candle {
            return self.cast_candle(c, dir, 0.0);
        }
        let (bvh, normals, parts) = &*self.mesh;
        // The ray in the model's frame, metres, then box units.
        let o = vec3f(-self.pos.dot(self.x), -self.pos.dot(self.y), -self.pos.dot(self.z));
        let d = vec3f(dir.dot(self.x), dir.dot(self.y), dir.dot(self.z));
        let s = self.scale;
        let hit = bvh.hit(vec3f(o.x / s.x, o.y / s.y, o.z / s.z), vec3f(d.x / s.x, d.y / s.y, d.z / s.z))?;
        let [n0, n1, n2] = normals[hit.tri];
        let nb = n0 * (1.0 - hit.u - hit.v) + n1 * hit.u + n2 * hit.v;
        // The normal in metres (the scale's inverse transpose), toward the ray.
        let mut n = vec3f(nb.x / s.x, nb.y / s.y, nb.z / s.z);
        if !(n.length() > 1.0e-9) {
            n = vec3f(hit.face.x / s.x, hit.face.y / s.y, hit.face.z / s.z);
        }
        let n = n.normalize();
        let p = o + d * hit.t;
        let front = d.dot(n) < 0.0;
        match parts[hit.tri] {
            Part::Cloth if !front => {
                if self.translucent {
                    Some(ModelHit::Back { at: self.face_dir(p) })
                } else {
                    Some(ModelHit::Solid { level: self.black_back(p, -n, d.normalize()) })
                }
            }
            Part::Cloth | Part::Face => {
                let mirror = self.mirror.as_ref().map_or(1.0, |m| m.ratio(p, d.normalize()));
                let mirror = self.sheen.as_ref().map_or(mirror, |s| mirror * s.ratio(p, d.normalize(), self.dist, self.scale));
                Some(ModelHit::Face { at: self.face_dir(p), mirror })
            }
            Part::Solid(Albedo::Spun) => Some(ModelHit::Solid { level: self.glossy(p, if front { n } else { -n }, d.normalize(), SPUN_ALBEDO, SPUN_SHEEN) }),
            Part::Solid(Albedo::Coat) => Some(ModelHit::Solid { level: self.glossy(p, if front { n } else { -n }, d.normalize(), HEAD_ALBEDO, COAT_SHEEN) }),
            Part::Solid(albedo) => Some(ModelHit::Solid { level: self.solid(p, if front { n } else { -n }, albedo.albedo()) }),
            Part::Whole(turn) => {
                // Turned in box units, where the tube is round and the bulbs are spheres.
                let at = vec3f(p.x / s.x, p.y / s.y, p.z / s.z);
                let seen = vec3f(d.x / s.x, d.y / s.y, d.z / s.z);
                let h = p - vec3f(0.0, 0.0, self.dist);
                let head_on = vec3f(h.x / s.x, h.y / s.y, h.z / s.z);
                let q = self.turn(turn, at, seen, head_on);
                Some(ModelHit::Whole { at: self.face_dir(q * s) })
            }
        }
    }

    /// `p` (box units) on a `Part::Whole` seen along `seen`, turned round the part's middle (`turn`) by what turns
    /// `seen` onto `head_on`, the ray the camera head-on sends through `p`.
    fn turn(&self, turn: Turn, p: Vec3f, seen: Vec3f, head_on: Vec3f) -> Vec3f {
        match turn {
            Turn::None => p,
            Turn::Ring => {
                // Across the tube only: in the plane of its centre line's radius and the axis at `p`.
                let (mid, r_axis) = self.ring;
                let (x, y) = (p.x - mid.x, p.y - mid.y);
                let flat = (x * x + y * y).sqrt();
                if !(flat > 1.0e-6) {
                    return p;
                }
                let out = vec3f(x / flat, y / flat, 0.0);
                let (gr, gz) = (flat - r_axis, p.z - mid.z);
                let angle = |v: Vec3f| v.z.atan2(v.x * out.x + v.y * out.y);
                let t = gz.atan2(gr) - (angle(seen) - angle(head_on));
                let g = (gr * gr + gz * gz).sqrt();
                let along = r_axis + g * t.cos();
                vec3f(mid.x + out.x * along, mid.y + out.y * along, mid.z + g * t.sin())
            }
            Turn::Ball(i) => {
                let Some(c) = self.balls.get(i as usize).copied() else { return p };
                let (a, b) = (seen.normalize(), head_on.normalize());
                let axis = Vec3f::cross(a, b);
                let sin = axis.length();
                if !(sin > 1.0e-7) {
                    return p;
                }
                let (k, cos) = (axis / sin, a.dot(b));
                let v = p - c;
                c + v * cos + Vec3f::cross(k, v) * sin + k * (k.dot(v) * (1.0 - cos))
            }
        }
    }

    /// The direction of the flat face's point the camera head-on sees through `p` (metres, the model's frame): `p`
    /// projected from that camera onto the face's plane.
    fn face_dir(&self, p: Vec3f) -> Vec3f {
        let k = self.dist / (self.dist - p.z).max(1.0e-4);
        let q = vec2f(p.x * k / self.scale.x * self.half.x, p.y * k / self.scale.y * self.half.y);
        (self.center + self.x * q.x + self.y * q.y).normalize()
    }

    /// A solid's radiance at `p` (metres, the model's frame) facing `n` (unit, toward the camera's side), a share of
    /// the face's mean: the studio's fill (a little more from above) and the bowl's light on what faces it, a disc
    /// of the hem's radius half way down the bowl seen by its form factor.
    fn solid(&self, p: Vec3f, n: Vec3f, albedo: f32) -> f32 {
        let up = (self.x * n.x + self.y * n.y + self.z * n.z).y;
        let to = vec3f(0.0, 0.0, self.bowl_z) - p;
        let h2 = to.length_squared().max(1.0e-8);
        let w = to / h2.sqrt();
        let bowl = n.dot(w).max(0.0) * (-w.z).max(0.0) * self.bowl_r * self.bowl_r / (self.bowl_r * self.bowl_r + h2);
        albedo * (ROOM_FILL * (0.75 + 0.25 * up) + BOWL_FILL * bowl)
    }

    /// A glossy solid at `p` facing `n` (unit, toward the camera) seen along `d` (unit): dull (`solid`, `albedo`)
    /// and `sheen` of the studio's fill from above mirrored in it, so spun metal or a satin coat reads by how far
    /// each part of it tilts up.
    fn glossy(&self, p: Vec3f, n: Vec3f, d: Vec3f, albedo: f32, sheen: f32) -> f32 {
        let r = d - n * (2.0 * d.dot(n));
        let up = (self.x * r.x + self.y * r.y + self.z * r.z).y.max(0.0);
        self.solid(p, n, albedo) + sheen * ROOM_FILL * up * up
    }

    /// A black cloth's back at `p` facing `n` (unit, toward the camera) seen along `d` (unit): its dull fill
    /// (`solid`) and the sheen of its nylon, the studio's fill from above mirrored in it, so each gore reads by how
    /// far it tilts toward the ceiling.
    fn black_back(&self, p: Vec3f, n: Vec3f, d: Vec3f) -> f32 {
        let r = d - n * (2.0 * d.dot(n));
        let up = (self.x * r.x + self.y * r.y + self.z * r.z).y.max(0.0);
        self.solid(p, n, BLACK_CLOTH_ALBEDO) + BLACK_CLOTH_SHEEN * ROOM_FILL * up * up * up
    }
}

/// A built model before it is placed: its mesh, whether its cloth passes light, and a silver bowl's depth and its
/// sphere's radius (box units), for its mirror.
type Built = (Mesh, bool, Option<(f32, f32)>);

/// An umbrella (white or silver): the bowl, a sphere's cap `depth` deep cut to the face's outline
/// (`Modifier::outline`) between `ribs` ribs, its panels straight from rib to rib; the ribs on its inside with a
/// stretcher from the runner on the shaft to each; the shaft from behind the hub through the strobe head's mount;
/// the head (its reflector, body, yoke and spigot) and its stand as the face's silhouette places them, the flash in
/// the bowl's focus.
fn umbrella(kind: ModifierKind, baffle: f32, ribs: u32, plate: f32, modifier: &Modifier) -> Option<Built> {
    let silver = kind == ModifierKind::SilverUmbrella;
    let deep = silver && baffle >= 0.75;
    let n = if ribs >= 3 { ribs as usize } else if silver { 16 } else { 8 };
    let t = ((baffle - 0.3) / 0.7).clamp(0.0, 1.0);
    // How deep the bowl is, over the hem's radius: a white bounce's or a shallow silver's, a parabolic's (a white
    // umbrella's flat hot zone) deeper, a deep silver's deepest.
    let depth = match (silver, baffle) {
        (true, _) => 0.45 + 0.4 * t,
        (false, b) if b < 0.12 => 0.6,
        _ => 0.45,
    };
    let sphere = (1.0 + depth * depth) / (2.0 * depth);
    let zs = |s: f32| -depth + sphere - (sphere * sphere - s * s).max(0.0).sqrt();
    let outline = |a: f32| modifier.outline(a).unwrap_or(1.0).max(0.05);
    let sector = std::f32::consts::TAU / n as f32;
    let at = |s: f32, a: f32| {
        let rho = s * outline(a);
        vec3f(rho * a.sin(), rho * a.cos(), zs(s))
    };
    // The normal toward the bowl's inside (+z side), from the surface's slopes along s and round a.
    let normal = |s: f32, a: f32, a_lo: f32, a_hi: f32| {
        let ds = 1.0e-3;
        let da = 1.0e-3 * sector;
        let (s0, s1) = ((s - ds).max(0.0), (s + ds).min(1.0));
        let (a0, a1) = ((a - da).max(a_lo), (a + da).min(a_hi));
        let along = at(s1, a) - at(s0, a);
        let round = at(s.max(0.02), a1) - at(s.max(0.02), a0);
        let mut nn = Vec3f::cross(round, along);
        if nn.z < 0.0 {
            nn = -nn;
        }
        if nn.length() > 1.0e-9 { nn.normalize() } else { vec3f(0.0, 0.0, 1.0) }
    };
    let mut mesh = Mesh::new();
    let s_of = |i: usize| 0.015 + (1.0 - 0.015) * i as f32 / BOWL_RINGS as f32;
    for k in 0..n {
        let (a_lo, a_hi) = ((k as f32 - 0.5) * sector, (k as f32 + 0.5) * sector);
        let a_of = |j: usize| a_lo + (a_hi - a_lo) * j as f32 / BOWL_ACROSS as f32;
        for i in 0..BOWL_RINGS {
            for j in 0..BOWL_ACROSS {
                let (s0, s1, a0, a1) = (s_of(i), s_of(i + 1), a_of(j), a_of(j + 1));
                let p = [at(s0, a0), at(s1, a0), at(s1, a1), at(s0, a1)];
                let nn = [normal(s0, a0, a_lo, a_hi), normal(s1, a0, a_lo, a_hi), normal(s1, a1, a_lo, a_hi), normal(s0, a1, a_lo, a_hi)];
                mesh.quad(p, nn, Part::Cloth);
            }
        }
    }
    // The hub's sleeve behind the cloth and the shaft through it.
    let head = HeadLayout::of(deep, plate);
    // The flash in the bowl's focus (half the sphere's radius in front of the hub), a deep bowl's no deeper than
    // just inside its hem, so the head and its stand clear the cloth.
    let focus = -depth + 0.5 * sphere;
    let z_f = if deep { focus.max(-0.15) } else { focus };
    let r_head = head.map_or(0.1, |h| h.r);
    let shaft_end = z_f + 2.2 * r_head;
    mesh.drum(0.0, 0.0, HUB_R, -depth - 0.02, -depth + 0.025, Part::Solid(Albedo::Metal));
    mesh.rod(vec3f(0.0, 0.0, -depth - 0.07), vec3f(0.0, 0.0, shaft_end), SHAFT_R, true, Part::Solid(Albedo::Metal));
    let runner = vec3f(0.0, 0.0, -depth + RUNNER_IN);
    mesh.drum(0.0, 0.0, 0.6 * HUB_R, runner.z - 0.025, runner.z + 0.025, Part::Solid(Albedo::Metal));
    // The ribs on the bowl's inside, out to the hem's corners, a tip past it; a stretcher to each.
    for k in 0..n {
        let a = (k as f32 - 0.5) * sector;
        let on = |s: f32| {
            let c = vec3f(0.0, 0.0, -depth + sphere);
            let p = at(s, a);
            p + (c - p).normalize() * RIB_R
        };
        for i in 0..RIB_SEGMENTS {
            let (s0, s1) = (0.03 + 0.97 * i as f32 / RIB_SEGMENTS as f32, 0.03 + 0.97 * (i + 1) as f32 / RIB_SEGMENTS as f32);
            mesh.rod(on(s0), on(s1), RIB_R, false, Part::Face);
        }
        let (end, before) = (on(1.0), on(0.97));
        let tip = end + (end - before).normalize() * RIB_TIP;
        mesh.rod(end, tip, 1.2 * RIB_R, true, Part::Solid(Albedo::Metal));
        let joint = on(STRUT_REACH);
        let joint = joint + (runner - joint).normalize() * (RIB_R + STRUT_R);
        mesh.rod(runner, joint, STRUT_R, true, Part::Face);
    }
    // The strobe head on its stand, as the face's silhouette places them, its round back toward the camera: the
    // head a drum, the U of its yoke round its underside (RLS024: as wide as the head, open in its middle), the
    // block under it and the spigot, the stand's clamp and the stand; the umbrella mount on its top round the shaft.
    if let Some(h) = head {
        let r = h.r;
        let solid = Part::Solid(Albedo::Head);
        mesh.drum(0.0, h.y, r, z_f, z_f + 2.4 * r, solid);
        mesh.cuboid(vec3f(-0.1 * r, h.y + 0.8 * r, z_f + 1.9 * r), vec3f(0.1 * r, 0.012, z_f + 2.3 * r), solid);
        let (zu0, zu1) = (z_f + 1.15 * r, z_f + 1.65 * r);
        let bottom = h.y - 1.45 * r;
        for side in [-1.0f32, 1.0] {
            let (x0, x1) = (side * 0.72 * r, side * 0.94 * r);
            mesh.cuboid(vec3f(x0.min(x1), bottom, zu0), vec3f(x0.max(x1), h.y - 0.3 * r, zu1), solid);
        }
        mesh.cuboid(vec3f(-0.94 * r, bottom, zu0), vec3f(0.94 * r, bottom + 0.24 * r, zu1), solid);
        let zy = 0.5 * (zu0 + zu1);
        mesh.cuboid(vec3f(-h.yoke.x, h.yoke_y - h.yoke.y, zy - 0.3 * r), vec3f(h.yoke.x, (h.yoke_y + h.yoke.y).min(bottom + 0.1 * r), zy + 0.3 * r), solid);
        let stand = Part::Solid(Albedo::Stand);
        mesh.cuboid(vec3f(-h.spigot.x, h.spigot_y - h.spigot.y, zy - h.spigot.x), vec3f(h.spigot.x, h.spigot_y + h.spigot.y, zy + h.spigot.x), stand);
        let clamp = 1.4 * h.stand_r;
        mesh.cuboid(vec3f(-clamp, h.stand_top - 2.5 * clamp, zy - clamp), vec3f(clamp, h.stand_top, zy + clamp), stand);
        mesh.rod(vec3f(0.0, h.stand_top, zy), vec3f(0.0, STAND_FOOT, zy), h.stand_r, true, stand);
    }
    let mirror = silver.then_some((depth, sphere));
    Some((mesh, !silver, mirror))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::light_fx::LightFx;
    use crate::params::SpaceParams;
    use crate::image_light::ImageSet;
    use crate::modifier::apply_template;

    fn umbrella_light(kind: ModifierKind, yaw: f32) -> LightParams {
        let mut light = LightParams::default();
        apply_template(&mut light, kind);
        light.space = Some(SpaceParams { position_m: [0.0, 0.0, -2.5], yaw_deg: yaw, pitch_deg: 0.0, roll_deg: 0.0, width_m: 1.05, height_m: 1.05, beam: 0.0 });
        light
    }

    fn model(light: &LightParams) -> Option<LightModel> {
        let fx = LightFx::new(light, &ImageSet::default());
        LightModel::new(light, fx.modifier())
    }

    /// A 3D umbrella has a model, a 2D one does not.
    #[test]
    fn only_3d_umbrellas_have_a_model() {
        for kind in [ModifierKind::Umbrella, ModifierKind::SilverUmbrella] {
            let light = umbrella_light(kind, 0.0);
            let m = model(&light).expect("a 3D umbrella's model");
            assert!(m.triangles() > 3000 && m.triangles() < 30000, "{kind:?}: {}", m.triangles());
            let mut flat = light.clone();
            flat.space = None;
            assert!(model(&flat).is_none(), "{kind:?} in 2D");
        }
    }

    /// Head-on, a ray through the face's cloth meets it at the very point of the flat face it travels along, and
    /// a silver bowl's mirror is 1 there; the middle is the head's back, a dull solid at about 1.5 % of the face; a
    /// ray past the hem meets nothing; turned round, the back of a white umbrella glows through and a silver one's
    /// is a black solid.
    #[test]
    fn head_on_the_model_is_the_flat_face() {
        for kind in [ModifierKind::Umbrella, ModifierKind::SilverUmbrella] {
            let light = umbrella_light(kind, 0.0);
            let m = model(&light).unwrap();
            let centre = vec3f(0.0, 0.0, -1.0);
            // half way out, up and to the right: the cloth
            let p = vec3f(0.18, 0.2, -2.5);
            let dir = p.normalize();
            match m.cast(dir) {
                Some(ModelHit::Face { at, mirror }) => {
                    assert!((at - dir).length() < 2.0e-5, "{kind:?}: {at:?} for {dir:?}");
                    assert!((mirror - 1.0).abs() < 1.0e-6, "{kind:?}: mirror {mirror}");
                }
                other => panic!("{kind:?}: {other:?}"),
            }
            // just under the hub: the strobe head's back
            match m.cast(vec3f(0.0, -0.06, -2.5).normalize()) {
                Some(ModelHit::Solid { level }) => assert!(level > 0.005 && level < 0.03, "{kind:?}: {level}"),
                other => panic!("{kind:?}: {other:?}"),
            }
            assert!(m.cast(vec3f(0.7, 0.0, -2.5).normalize()).is_none(), "{kind:?}: past the hem");
            assert!(m.cast(centre).is_some());
            let back = model(&umbrella_light(kind, 180.0)).unwrap();
            let hit = back.cast(vec3f(0.15, 0.25, -2.5).normalize());
            match (kind, hit) {
                (ModifierKind::Umbrella, Some(ModelHit::Back { .. })) => {}
                (ModifierKind::SilverUmbrella, Some(ModelHit::Solid { level })) => assert!(level > 0.002 && level < 0.04, "{level}"),
                other => panic!("{other:?}"),
            }
        }
    }

    /// Turned 45 degrees, a silver bowl's flash image moves: somewhere on the bowl the mirror is well off 1.
    #[test]
    fn a_silver_bowls_image_moves_with_the_view() {
        let m = model(&umbrella_light(ModifierKind::SilverUmbrella, 45.0)).unwrap();
        let (mut lo, mut hi) = (f32::INFINITY, 0.0f32);
        for i in -20..=20 {
            for j in -20..=20 {
                let dir = vec3f(i as f32 * 0.012, j as f32 * 0.012, -2.5).normalize();
                if let Some(ModelHit::Face { mirror, .. }) = m.cast(dir) {
                    lo = lo.min(mirror);
                    hi = hi.max(mirror);
                }
            }
        }
        assert!(lo < 0.7 || hi > 1.5, "the mirror stays at {lo}..{hi}");
    }
}

//! Studio lighting: the backdrop gradient and the light list.
//!
//! A map is a base layer (this backdrop, or the outdoor sky) with an ordered list
//! of lights evaluated over it. Each light lives in its own angular frame (centre,
//! right, up), and its shape is a signed distance in that frame's gnomonic
//! tangent plane. A softbox therefore keeps its shape at the zenith instead of
//! smearing the way an equirect rectangle would.
//!
//! The frame is `ibl::softbox`'s (libs/render_material/src/ibl.rs:414-432):
//! `right = cross(+Y, center)`, `up = cross(center, right)`. The shapes generalise
//! that softbox and the knob's `rect_d` (apps/storybook/src/knob/bake.rs:697):
//! - a rounded rect;
//! - a disc (an ellipse when width != height);
//! - a ring, `abs(r - R) - w`;
//!
//! with a roll, a soft edge of any width, a hotspot and an Add or Multiply blend.
//!
//! All colours are linear Rec.709. A light's colour is its peak radiance: the
//! Kelvin/tint colour (luminance 1) or an explicit rgb, times 2^intensity_ev.

use makepad_draw::*;
use super::{az_el_from_dir, dir_from_az_el, Blend, EnvSun, LightParams, LightShape, RoomParams, StudioParams, CASTER_REFERENCE_WIDTH, KELVIN_RANGE};
use super::room::{disc_daylight, Ground, MirrorProxy, Room, RoomLight, Stand, DEFAULT_REACH};
use super::pattern::PatternLight;
use crate::sky::{luminance, xyz_to_linear_rgb};
use super::shaped::{box_x_sign, face_frame, half_extents, stroke_box, ShapedLight};
use super::space::{beam_exponent, beam_factor};
use super::shape::ArcPath;
use super::noise::hash01;
use super::gels::gel_stack;
use super::spectrum::light_source_rgb;
use super::image_light::{ImageSet, LightImage};
use super::light_fx::LightFx;
use super::light_model::{candle::CandleLight, LightModel, ModelHit};
use super::caster::{caster_drawn, caster_flag, rows_by, serial_rows, BrightBox, CasterBloom, CasterLayer, RowRunner};
use std::borrow::Cow;

/// Strength of a full ±1 tint: at +1 green is cut to about a third of red and blue.
const TINT_STRENGTH: f32 = 0.4;

/// Clamps into [lo, hi]. A NaN becomes `lo`, because `f32::max` ignores NaN; this
/// is unlike `f32::clamp`, which passes NaN through.
fn sane(x: f32, lo: f32, hi: f32) -> f32 {
    x.max(lo).min(hi)
}

/// Linear Rec.709 colour of a blackbody at `kelvin` (1000..=20000), scaled to luminance 1.
///
/// **Source of the fit.** The chromaticity comes from Krystek's rational fit of the
/// Planckian locus in CIE 1960 (u, v): M. Krystek, "An algorithm to calculate
/// correlated colour temperature", Color Research & Application 10(1), 1985. The
/// coefficients are as reproduced in Wikipedia's "Planckian locus" article.
///
/// **Accuracy.** The fit is within about 1e-4 in (u, v) from 1000 K to 15000 K.
/// Extrapolated to 20000 K it drifts by about 5e-4 (measured against the Robertson
/// 1968 isotemperature table and a cubic-spline locus), which cannot be seen at that
/// blue end.
///
/// **Conversion.** (u, v) goes to (x, y), then to XYZ with Y = 1, then to linear
/// Rec.709 through the same XYZ matrix sky.rs uses.
///
/// **Below about 1900 K** the locus leaves the Rec.709 gamut and blue goes
/// negative. Blue is clipped to 0 and the colour rescaled, so the luminance stays 1.
pub fn kelvin_to_rgb(kelvin: f32) -> Vec3f {
    let t = sane(kelvin, KELVIN_RANGE.0, KELVIN_RANGE.1);
    let t2 = t * t;
    let u = (0.860_117_73 + 1.541_182_6e-4 * t + 1.286_412_2e-7 * t2)
        / (1.0 + 8.424_202e-4 * t + 7.081_451_4e-7 * t2);
    let v = (0.317_398_73 + 4.228_062_6e-5 * t + 4.204_816_8e-8 * t2)
        / (1.0 - 2.897_418_2e-5 * t + 1.614_560_6e-7 * t2);
    let d = 2.0 * u - 8.0 * v + 4.0;
    let x = 3.0 * u / d;
    let y = 2.0 * v / d;
    let big_x = x / y;
    let big_z = (1.0 - x - y) / y;
    let rgb = xyz_to_linear_rgb([big_x, 1.0, big_z]);
    rgb / luminance(rgb).max(1.0e-6)
}

/// A green/magenta correction as an RGB multiplier.
/// - -1 is a strong plus-green, +1 a strong minus-green (magenta), 0 is neutral.
/// - It is scaled to luminance 1, so it shifts the hue of a white light without
///   changing its brightness.
pub fn tint_rgb(tint: f32) -> Vec3f {
    let t = sane(tint, -1.0, 1.0) * TINT_STRENGTH;
    let m = vec3f(1.0 + t, 1.0 - t, 1.0 + t);
    m / luminance(m)
}

/// A light's peak radiance, times 2^intensity_ev, behind its gels.
/// - With `rgb` set, that explicit colour is used as given.
/// - Else with a named `source` (1b), the colour is that lamp's spectrum
///   (spectrum.rs) times tint, at luminance 1; the Kelvin is not read, except
///   by the parametric LED (`led`), whose spectrum is built at the Kelvin and
///   the `source_quality` knob.
/// - Otherwise the colour is Kelvin times tint, renormalised to luminance 1 so
///   the tint cannot change the brightness of a coloured light either.
/// - Gels (1b) filter the result, every gel and every light alike, as a sheet
///   in front of a lamp does (gels.rs): a conversion gel is the filter that
///   moves a blackbody by its mireds. So a Kelvin light keeps its colour and
///   level when switched to RGB, and a strong CTB is not stopped at the
///   Kelvin colours' 20000 K end (D10, L:86).
///
/// The light is read as clamp() holds it (`LightParams::clamped`, D17): its
/// level, its RGB, its Kelvin, tint and source.
pub fn light_color(light: &LightParams) -> Vec3f {
    held_light_color(&light.clamped())
}

/// `light_color` of a light clamp() has held already, read as it is: the
/// builders that hold a light once colour it through this (`Studio`'s
/// prepared light, a stroke's Kelvin ramp and each tap's recoloured copy).
pub(super) fn held_light_color(light: &LightParams) -> Vec3f {
    let gels = gel_stack(light.gels.as_deref().unwrap_or(&[]));
    let base = match light.rgb {
        Some(rgb) => vec3f(rgb[0], rgb[1], rgb[2]),
        None => held_light_white(light),
    };
    let base = match light.modifier.as_ref() {
        // A gold reflector's cloth tints the light it gives back (reflector.rs).
        Some(m) if m.kind() == super::params::ModifierKind::Reflector => base * super::reflector::finish_tint(super::reflector::Finish::from_ribs(m.ribs)),
        _ => base,
    };
    base * gels.conversion * gels.filter * 2.0f32.powf(light.intensity_ev)
}

/// The colour a light without an explicit `rgb` has before its gels and its
/// level: its named source's spectrum (the knob LED's built at its Kelvin and
/// quality) or its Kelvin, times its tint, at luminance 1, of the light as
/// clamp() holds it (`LightParams::clamped`), as the CRI readout reads it
/// (`cri::light_spectrum`). `light_color` starts from it, and so does the
/// panel's switch to RGB, so the switch keeps the light's colour exactly
/// (D17, L:305).
pub fn light_white(light: &LightParams) -> Vec3f {
    held_light_white(&light.clamped())
}

/// `light_white` of a light clamp() has held already, read as it is.
fn held_light_white(light: &LightParams) -> Vec3f {
    // A named source's spectrum gives the colour in place of the Kelvin's,
    // tinted the same way (the knob LED's Kelvin is part of its spectrum).
    let white = light_source_rgb(light).unwrap_or_else(|| kelvin_to_rgb(light.kelvin));
    let c = white * tint_rgb(light.tint);
    // Back to luminance 1: a tint on a coloured light (a sodium lamp, 3200 K)
    // moves the product's luminance off 1.
    c / luminance(c).max(1.0e-6)
}

/// A light's angular frame. `center` points at the light. `right` and `up` span the
/// tangent plane its shape is drawn in; both are unit length and perpendicular to
/// `center`, and `right x up = center`.
///
/// Plan P1: a light placed in 3D (`space::space_frame`) has a `face`: the
/// normal of its own plane, out of its back (`right x up`), with `right` and
/// `up` its own axes in that plane, which passes through `center` (its real
/// place over its distance). Its points are still `center + right x + up y`,
/// but a direction's (x, y) is where it meets that plane (`project`). None:
/// the tangent plane at `center`, every 2D light's.
#[derive(Clone, Copy, Debug)]
pub struct LightFrame {
    pub center: Vec3f,
    pub right: Vec3f,
    pub up: Vec3f,
    pub face: Option<Vec3f>,
}

impl LightFrame {
    /// The way the face's front looks: back at the camera for a 2D light
    /// (`-center`), the face's own front for a 3D one.
    pub fn front(&self) -> Vec3f {
        -self.face.unwrap_or(self.center)
    }

    /// The cosine of the cone round `center` that holds the box of half
    /// extents `half` in this frame's plane. In a tangent plane its corner,
    /// 1 / sqrt(1 + hx² + hy²). On a 3D face the farthest of its four corners
    /// (the directions within an angle under 90 degrees of `center` are a
    /// convex cone, so it holds the box when it holds the corners), or -1 (all
    /// round) when a corner lies 90 degrees or more out.
    pub fn cover_cos(&self, half: Vec2f) -> f32 {
        if self.face.is_none() {
            return 1.0 / (1.0 + half.x * half.x + half.y * half.y).sqrt();
        }
        let mut worst = 1.0f32;
        for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let q = self.center + self.right * (sx * half.x) + self.up * (sy * half.y);
            let c = q.dot(self.center) / q.length().max(1.0e-12);
            if !(c > 0.0) {
                return -1.0;
            }
            worst = worst.min(c);
        }
        worst
    }

    /// The angle round `center` that holds everything within `r` of the
    /// middle in this frame's plane: atan(r) in a tangent plane, the cone of
    /// the box round that disc on a 3D face (`cover_cos`).
    pub fn cone_radius(&self, r: f32) -> f32 {
        match self.face {
            None => r.atan(),
            Some(_) => self.cover_cos(vec2f(r, r)).max(-1.0).min(1.0).acos(),
        }
    }
}

/// The frame of a light at (azimuth, elevation), turned by `roll_deg`.
///
/// **Unrolled** it is `ibl::softbox`'s basis: `right = normalize(cross(+Y, center))`,
/// `up = cross(center, right)`. `right` is the horizontal direction of decreasing
/// azimuth, which is the viewer's left when facing the light; `up` tilts back over
/// the viewer's head as the light climbs.
///
/// **At the poles** `cross(+Y, center)` vanishes and any perpendicular will do. The
/// azimuth's own horizontal direction is used, which is the limit of the basis as
/// the light climbs, so the frame stays continuous up to the zenith.
///
/// **Roll.** Positive roll turns the shape counter-clockwise as the viewer sees it.
/// Because `right` points to the viewer's left, that turn carries `right` toward
/// `-up`.
pub fn light_frame(azimuth_deg: f32, elevation_deg: f32, roll_deg: f32) -> LightFrame {
    let center = dir_from_az_el(azimuth_deg, elevation_deg);
    let mut right = Vec3f::cross(vec3f(0.0, 1.0, 0.0), center);
    if right.length_squared() < 1.0e-8 {
        let (sin_az, cos_az) = azimuth_deg.to_radians().sin_cos();
        right = vec3f(-cos_az, 0.0, -sin_az);
    }
    let right = right.normalize();
    let up = Vec3f::cross(center, right);
    let (sin_r, cos_r) = roll_deg.to_radians().sin_cos();
    LightFrame {
        center,
        right: right * cos_r - up * sin_r,
        up: up * cos_r + right * sin_r,
        face: None,
    }
}

/// Gnomonic tangent-plane coordinates of `dir` in `frame`: `dir`'s right and up
/// components divided by its component along the centre. Great circles through the
/// frame map to straight lines, so a rect keeps straight sides.
///
/// Returns None when `dir` is more than about 87 degrees from the centre (dot <
/// [`PROJECT_MIN_COS`]), where the projection runs off to infinity.
///
/// Plan P1: on a 3D face (`LightFrame::face`) the coordinates are where the
/// ray along `dir` meets the face's plane, its offset from `center` along
/// `right` and `up` (the plane's own coordinates, orthonormal in it). None
/// where the ray does not meet it in front of the camera: the plane runs
/// through the camera (edge-on) or lies the other way. A 2D frame's is the
/// same thing for the plane facing the camera.
pub fn project(frame: &LightFrame, dir: Vec3f) -> Option<Vec2f> {
    if let Some(n) = frame.face {
        let t = frame.center.dot(n) / dir.dot(n);
        // Written negated so a NaN (edge-on, or a NaN direction) is rejected too.
        if !(t > 0.0 && t < f32::INFINITY) {
            return None;
        }
        let at = dir * t - frame.center;
        return Some(vec2f(at.dot(frame.right), at.dot(frame.up)));
    }
    let c = dir.dot(frame.center);
    // Written negated so a NaN direction is rejected too.
    if !(c >= PROJECT_MIN_COS) {
        return None;
    }
    Some(vec2f(dir.dot(frame.right) / c, dir.dot(frame.up) / c))
}

/// [`project`]'s limit: the smallest dot with the centre it places, about 87
/// degrees out, where the tangent plane is already 20 units from the centre.
pub const PROJECT_MIN_COS: f32 = 0.05;

/// [`project`], held at its limit: a direction further out than the plane
/// reaches comes back on the limit's circle, along its own bearing round the
/// centre, so what is laid in the plane goes on continuously past it. Only a
/// stroke draws that far from its middle (D3, L:180: a grid or a window there
/// stepped to fully bright where the plane ended). None only straight behind
/// the centre, where there is no bearing, and for NaN.
pub fn project_held(frame: &LightFrame, dir: Vec3f) -> Option<Vec2f> {
    if let Some(t) = project(frame, dir) {
        return Some(t);
    }
    // A 3D face has no limit to hold at: a ray that misses its plane is off it.
    if frame.face.is_some() {
        return None;
    }
    let (x, y) = (dir.dot(frame.right), dir.dot(frame.up));
    let bearing = (x * x + y * y).sqrt();
    if !(bearing > 1.0e-6) {
        return None;
    }
    let c = PROJECT_MIN_COS;
    let limit = (1.0 - c * c).sqrt() / c;
    Some(vec2f(x, y) * (limit / bearing))
}

/// Narrowest soft edge, in tangent units (about 0.006 degrees). A "hard" edge still
/// gets this much, so the step is resolved below a pixel and never becomes a
/// stair-step in an 8K bake.
pub(super) const MIN_EDGE: f32 = 1.0e-4;
/// P2: how deep, as a share of a long disc's depth (its short half axis),
/// its soft edge's ramp starts (Softness / 2 + Penumbra) when the edge
/// begins to read the exact ellipse distance, and by when it reads it
/// alone (`PreparedLight::deep`). The first-order distance's levels have
/// no waist down to half the depth (in the limit of a needle they are
/// convex to exactly half) and are waisted deeper; blending from 0.5 to
/// 0.6 leaves no waist at any depth for aspects from 1.25:1 to 100:1
/// (review round 1 of P2, measured just below the peak). A ramp starting no
/// deeper than half the depth draws as before P2, bit for bit: so does
/// every disc without a picture, whose Softness reaches half the depth at
/// most; only a picture's Penumbra reaches deeper. (At Softness 0 the
/// narrowest edge, `MIN_EDGE`, starts a Penumbra 0.5 ramp a hair past half:
/// a disc whose short side is a degree or more reads a blend under 1 %.)
const DISC_EXACT_FROM: f32 = 0.5;
const DISC_EXACT_BY: f32 = 0.6;
/// With hotspot 1 the edge of the shape is e^-4 (about 2%) of its centre.
const HOTSPOT_FALLOFF: f32 = 4.0;
/// Grid points per axis of the key light's integral over its reach box
/// ([`key_emission`]). A grid four times finer moves the built-in keys by under
/// 0.01 % and the worst case in the tests, a hard ring a twentieth of its radius thick,
/// by 0.09 %.
const KEY_GRID: usize = 256;

/// D2: the widest a stroke or a ring may spread round its middle and still be
/// the key, in degrees: the widest key cone a rect or a disc gets (half their
/// 170 degree limit). A stroke round the whole horizon has no middle to light
/// from, and a ring round the sky lies far out all round its middle: the
/// engine's light would come from straight up, lighting little and no
/// vertical surface, while all of the light left the map's lighting.
pub const KEY_MAX_SPREAD_DEG: f32 = 85.0;

/// Steps across a Ring light's band, soft edge included, and round a
/// quarter of it, of [`PreparedLight::ring_facing`]'s polar sum (a ring is
/// mirrored across both its axes). A circle needs one step round (its light
/// depends on the radius alone).
const RING_FACING_STEPS: usize = 256;
const RING_FACING_TURNS: usize = 32;

/// D2: whether `light` is a stroke or a Ring light that spreads wider round
/// its middle than a key of [`KEY_MAX_SPREAD_DEG`] can, so that marked as the
/// key it is not handed to the engine: it stays a light of the map, lighting
/// the scene through the map like any other, and under a sky the sun leads.
/// The panel says so where the key is chosen. False for every other light
/// (a rect, a disc, a polygon, a scrim).
///
/// A stroke spreads that wide in either of two ways, both measured on its
/// centre line round its mean direction (its middle, the key's axis):
/// - it reaches further than the limit from its middle (its widest angle from
///   it, plus its widest half width: the key cone), as a strip round the
///   horizon or a half circle does;
/// - it lies all round its middle further out than the light of an even disc
///   of the limit's radius does: a surface facing its middle receives less of
///   a thin line along it than (1 + cos 85°) / 2 = 0.544, that disc's share.
///   L:41's ring at elevation 10 reaches only 82 degrees, but every point of
///   it is 80 degrees out (0.17); a ring is refused below about elevation 33,
///   and a 160 degree arch, which runs through its middle, gives 0.71.
///
/// A Ring light is measured the same two ways round its axis (the light's
/// direction, the key's): its key cone, half the larger of its width and
/// height, against the limit (clamp() holds both to 170 degrees, so a ring
/// reaches at most the limit itself), and the share of its own light, its
/// band, soft edge and hotspot, that a surface facing its axis receives
/// (`PreparedLight::ring_facing`). A thin ring (inner 0.9) is refused from
/// about 117 degrees across, and a ring nearly a disc (inner 0.1) from about
/// 158: a whole disc 170 across gives the limit's share itself, and a ring
/// lacks its middle.
pub fn spreads_too_wide_to_be_key(light: &LightParams) -> bool {
    let light = &LightParams { enabled: true, ..light.clone() };
    match light.shape() {
        LightShape::Ring => too_wide_to_lead(&PreparedLight::new(light, &ImageSet::default())),
        // A pattern lies on its face, as a rect does (and is not rastered to be asked).
        LightShape::Pattern => false,
        _ => ShapedLight::new(light).is_some_and(|shaped| stroke_too_wide_to_lead(&shaped)),
    }
}

/// [`spreads_too_wide_to_be_key`] for a prepared light: a stroke by its
/// centre line, a ring by its band, every other light never.
fn too_wide_to_lead(light: &PreparedLight) -> bool {
    match (&light.shaped, light.shape) {
        (Some(shaped), _) => stroke_too_wide_to_lead(shaped),
        (None, LightShape::Ring) => {
            let limit = KEY_MAX_SPREAD_DEG.to_radians().cos();
            !(light.key_cos >= limit) || !(light.ring_facing() >= even_disc_facing(limit))
        }
        (None, _) => false,
    }
}

/// [`spreads_too_wide_to_be_key`] for a light the shape core draws.
fn stroke_too_wide_to_lead(shaped: &ShapedLight) -> bool {
    let limit = KEY_MAX_SPREAD_DEG.to_radians().cos();
    let reaches_too_far = shaped.stroke_cones().is_some_and(|(_, cos_radius, _)| !(cos_radius >= limit));
    let lies_too_far_out = shaped.stroke_line_facing().is_some_and(|facing| !(facing >= even_disc_facing(limit)));
    reaches_too_far || lies_too_far_out
}

/// An even disc of half-angle r, `cos_r` its cosine: a surface facing it
/// receives π sin²r / 2π(1 - cos r) = (1 + cos r) / 2 of its light.
fn even_disc_facing(cos_r: f32) -> f32 {
    0.5 * (1.0 + cos_r)
}

/// Coarse cells per axis of [`over_cone`]'s grid over a key's covering cone.
const CONE_CELLS: usize = 64;
/// The most samples [`over_cone`] takes in all: past it every cell it refines
/// gets fewer, in proportion.
const CONE_BUDGET: usize = 1 << 18;
/// The most jittered samples per axis one coarse cell takes.
const CONE_FINE_MAX: usize = 64;
/// [`over_cone`]'s jitter seed: fixed, so the same light always gives the same key.
const CONE_JITTER_SEED: u32 = 0x636f_6e65;
/// Samples per stroke reach (half its width, soft edge included) along each
/// axis of a cell a stroke key draws in: a hard edge then averages out along
/// the stroke (a hard straight 4 degree stroke gives 0.08049 of its cone
/// against the exact 0.0805), and a tapering tip is still seen.
const STROKE_SAMPLES_PER_REACH: f32 = 8.0;
/// The most samples per axis a cell of a stroke key takes. A stroke thinner
/// than a cell's side over 2 (0.3 degrees on a 60 degree arch) gets fewer per
/// reach, which the jitter along its length still averages (13 ms for that
/// key in release, against 55 ms at 32).
const STROKE_FINE_MAX: usize = 16;
/// Samples per axis of every cell of a window filled with sky as the key (D1):
/// the sky is smooth, and the glass's frame and bars, hard edges at any
/// angle, average out under the jitter (the default window within 0.1 % of
/// what a 1024 wide map draws).
const WINDOW_FINE: usize = 2;
/// Samples per disc radius along each axis of a cell a disc (the sun or the
/// moon) shows through a window in: the disc is far smaller than a cell.
const DISC_SAMPLES_PER_RADIUS: f32 = 8.0;

/// The sampling width a room's window looks out at the sky with (`Env`).
pub const ROOM_SKY_WIDTH: usize = 512;

/// The backdrop gradient and the enabled lights and shadow casters, prepared once per map.
#[derive(Clone, Debug)]
pub struct Studio {
    top: Vec3f,
    horizon: Vec3f,
    floor: Vec3f,
    /// horizon_softness as degrees of elevation over which the horizon colour hands over.
    soft_deg: f32,
    lights: Vec<PreparedLight>,
    /// The key's place in `lights`: the light a lookup without the key skips,
    /// and the one `key` measures.
    key_index: Option<usize>,
    /// What hangs in front of each light (1b: gobo, doors and grid, wear),
    /// in step with `lights`; a caster's is its pattern.
    fx: Vec<LightFx>,
    /// Each shadow caster's own layer (redesign §5, `caster.rs`): its
    /// strength, tint and softness raster, in step with `lights`; None for a light.
    casters: Vec<Option<CasterLayer>>,
    /// False while no light has any extra, no caster a raster and no light a
    /// 3D model: `apply_lights` then runs the loop as it was.
    any_fx: bool,
    /// Each light's 3D model (light_model.rs: a 3D light of a kind that has
    /// one), in step with `lights`; None for every other entry.
    models: Vec<Option<LightModel>>,
    /// What the room (room.rs) reads of each entry, in step with `lights`.
    in_room: Vec<InRoom>,
    /// The room the lights stand in, in place of the gradient (`build_room`).
    /// The key's light on it stays in the map without the key (N3), as the
    /// sun's on the ground does: the engine's key light carries only what
    /// the key itself shows, and the room it lights is the environment's.
    room: Option<std::sync::Arc<Room>>,
    /// The texel, radians, the room's floor mirror is built for: the map's
    /// this studio is prepared for (`new_with`'s width), held to at least
    /// CASTER_REFERENCE_WIDTH's as a caster's raster and a pattern's are, so
    /// every Env at or under that width builds the same room. It sizes the
    /// mirror's layers; a lookup filters the mirror over its own texel where
    /// that is wider (`room_radiance`), as a pattern's lookup does.
    texel: f32,
    /// The entries the room's floor mirrors (`build_room`), in the mirror's
    /// order, and for each the lights after it that dim it ([`dimmers`]).
    mirrored: Vec<usize>,
    mirror_flags: Vec<Vec<usize>>,
}

/// What the room reads of one entry of the list.
#[derive(Clone, Copy, Debug)]
struct InRoom {
    /// It lights the room: an Add light that is not a paint layer.
    lights: bool,
    distance: Option<f32>,
    stand: Option<bool>,
    /// Its gobo patterns its pool (`GoboParams::project_on_backdrop`).
    casts: bool,
}

/// One entry of the list as it stands in the room (`Studio::build_room`):
/// its pieces, each a light, and where its stand would hold it.
struct InRoomLight {
    pieces: Vec<RoomLight>,
    stand: Stand,
}

impl Studio {
    /// `lights` are taken in list order; disabled ones are dropped here. The key is the
    /// first enabled Add light marked `key`, unless it is a stroke or a ring that spreads
    /// too wide to lead (D2, [`spreads_too_wide_to_be_key`]). A flag (Multiply) never leads the
    /// lighting.
    ///
    /// The key carries exactly the light the map draws of it (D1), measured from the
    /// map's own drawing when it is asked for ([`Self::key`]; `Env` asks once, when the
    /// engine does, D15): its shape, soft edge, hotspot, picture and ramp, what hangs in
    /// front of it, and the flags, gels, cloth and windows filled with sky after it.
    /// Building the studio measures nothing. The key's cone covers the larger of width
    /// and height, and its radiance is that emission spread over the cone ([`EnvSun`]'s
    /// one meaning), from the same integral as the share a surface facing the key
    /// receives (`facing`) and the cone that holds its whole reach (`cos_cover`):
    /// - a light in its own box (a rect, disc, ring, polygon or scrim, with or without a
    ///   picture) by [`key_emission`] over that box, times what passes what hangs in
    ///   front of it (`key_share`);
    /// - a stroke (1b), which its points place, over the cones round its centre line,
    ///   by [`over_cone`], where its band runs;
    /// - a window filled with sky, whose light is the sky, by `Env`, which knows that
    ///   sky (`measured_window_key`): `key()` is None for it.
    ///
    /// No pictures: an image light is drawn plain (see `new_with`).
    pub fn new(studio: &StudioParams, lights: &[LightParams]) -> Studio {
        Studio::new_with(studio, lights, &ImageSet::default(), false, CASTER_REFERENCE_WIDTH, &serial_rows)
    }

    /// `new` with the pictures image lights show (`Env::new_with` passes
    /// them through). A light whose key is not in `images` is drawn plain.
    /// `against_sky`: the lights hang against the sky (Sky mode), where a
    /// window filled with sky hides what is behind its frame and bars
    /// (`LightFx::against_sky`, D12).
    ///
    /// A shadow caster (redesign §5, `caster.rs`) is an entry like a light,
    /// in its place in the list. It is prepared from its flag
    /// (`caster::caster_flag`), so nothing here reads the light fields it
    /// keeps but ignores, and it is never the key. One whose image mask is
    /// not in `images` draws as no caster at all and is left out (spec §6).
    /// A soft caster's raster is laid out for a map `width` wide, its rows
    /// spread by `run` (`caster::RowRunner`); `new` builds them for
    /// CASTER_REFERENCE_WIDTH on this thread.
    pub(super) fn new_with(studio: &StudioParams, lights: &[LightParams], images: &ImageSet, against_sky: bool, width: usize, run: RowRunner) -> Studio {
        // The backdrop as clamp() holds it (D17); each light holds its own
        // (`PreparedLight::new`, `LightFx::new`).
        let studio = &studio.clamped();
        let rgb = |c: [f32; 3]| vec3f(c[0], c[1], c[2]);
        // (a lightning layer is drawn into the sky, `weather::Bolts`, not as a layer)
        let enabled: Vec<&LightParams> = lights.iter().filter(|l| l.enabled && !l.is_lightning() && (!l.is_caster() || caster_drawn(l, images))).collect();
        // What the shared code draws: a light as it is, a caster as its flag.
        let drawn: Vec<Cow<LightParams>> = enabled.iter().map(|l| if l.is_caster() { Cow::Owned(caster_flag(l)) } else { Cow::Borrowed(*l) }).collect();
        let prepared: Vec<PreparedLight> = drawn.iter().map(|l| PreparedLight::new_for(l, images, width)).collect();
        // A caster is never the key, whatever it is marked: clamp() clears
        // its key, but the studio may be handed lights clamp() has not held.
        let key_index = enabled
            .iter()
            .zip(&prepared)
            .position(|(l, p)| !l.is_caster() && l.key && l.blend() == Blend::Add && !too_wide_to_lead(p));
        // What hangs in front of each light, built for the same lights in the
        // same order as the prepared lights above. Before the key: a flag's
        // gobo cuts what the flag takes out of the key (B8). A caster's is
        // its pattern.
        let fx: Vec<LightFx> = drawn.iter().map(|l| LightFx::new(l, images).against_sky(against_sky)).collect();
        let casters: Vec<Option<CasterLayer>> = enabled.iter().map(|l| if l.is_caster() { CasterLayer::new(l, images, width, run) } else { None }).collect();
        let models: Vec<Option<LightModel>> = drawn.iter().zip(&fx).map(|(l, fx)| if l.is_caster() { None } else { LightModel::new(l, fx.modifier()) }).collect();
        let any_fx = fx.iter().any(|fx| !fx.is_plain()) || casters.iter().flatten().any(|c| !c.is_plain()) || models.iter().any(Option::is_some);
        let in_room = enabled
            .iter()
            .map(|l| InRoom {
                lights: !l.is_caster() && l.blend() == Blend::Add && l.shape() != LightShape::Paint,
                distance: l.distance_m.filter(|d| *d >= 0.3),
                stand: l.stand,
                casts: l.gobo.as_ref().is_some_and(|g| g.project_on_backdrop && !g.fill_sky),
            })
            .collect();
        let mut built = Studio {
            top: rgb(studio.top),
            horizon: rgb(studio.horizon),
            floor: rgb(studio.floor),
            soft_deg: studio.horizon_softness * 90.0,
            lights: prepared,
            key_index,
            fx,
            casters,
            any_fx,
            models,
            in_room,
            room: None,
            texel: std::f32::consts::TAU / width.max(CASTER_REFERENCE_WIDTH) as f32,
            mirrored: Vec::new(),
            mirror_flags: Vec::new(),
        };
        // A studio's candles take its backdrop's light until a room or the sky lights them (`build_room`,
        // `light_candles_outdoors`).
        if !against_sky {
            built.light_candles_by_backdrop();
        }
        built
    }

    /// Stands the lights in the room `p` (Studio mode, `StudioParams::room`):
    /// each where it is seen, at its distance, as bright on the room as the
    /// map draws it, with its stand, and the floor's mirror of them. A light
    /// longer than a piece stands in the room as its pieces, each a light
    /// where that piece is seen: a stroke's along its centre line
    /// (`stroke_room_lights`), any other light's across its long side
    /// (`box_room_lights`), so a long LED tube or strip lights the room all
    /// along itself. All pieces are one length ([`room_piece_arc`]): every
    /// room texel sums one emitter per piece, so the more the lights' lengths
    /// add up to, the longer the pieces, and no room holds more than
    /// ROOM_PIECES. `sky(d)` is the sky a window filled with sky lets in (the
    /// lightless copy's), and the open walls too; `hot` are the discs in it
    /// (direction, angular radius); `run` spreads the mirror's rows, the
    /// planes' grids and a stroke's samples, the slow part, and `cancelled`
    /// is polled between its calls there. False when it stopped a stroke's
    /// (or the runner skipped some of them): the caller drops the map.
    ///
    /// Plan P3: each open wall is daylight (room.rs `Daylight`): a uniform
    /// area light the size of the opening, the outside's mean light through
    /// it (`Opening::daylight`), whose light the room works out exactly per
    /// lookup (`Opening::on`, no pieces, outside the piece budget), and each
    /// disc of `hot` shining in through the openings on its own (`disc_daylight`).
    /// The floor's mirror takes one layer per light that adds light, each
    /// looked up against its own place (`mirror_proxy`) and drawn there for
    /// this studio's texel (`mirrored_light`).
    ///
    /// A room whose floor side is open stands on `ground` (the scene's ground,
    /// read through the outside it is seen in): its floor takes the ground's
    /// albedo, gloss and roughness, and its bounce the ground's mean albedo.
    pub(super) fn build_room(&mut self, p: &RoomParams, sky: &(dyn Fn(Vec3f) -> Vec3f + Sync), hot: &[(Vec3f, f32)], ground: Option<Ground>, cancelled: &dyn Fn() -> bool, run: RowRunner) -> bool {
        let mut shell = Room::new(p);
        if let Some(ground) = ground {
            shell = shell.with_ground(ground);
        }
        let lengths: Vec<f32> = self.in_room.iter().enumerate().filter(|(_, info)| info.lights).map(|(i, info)| self.room_length(i, info)).collect();
        let piece_arc = room_piece_arc(&lengths);
        if piece_arc > ROOM_PIECE_DEG.to_radians() {
            // A bound that degrades says so: the room's lights are cut coarser.
            log!(
                "hdri room: {} degrees of lights stand as at most {ROOM_PIECES} pieces, each {:.1} degrees long (not {ROOM_PIECE_DEG})",
                lengths.iter().filter(|l| l.is_finite() && **l > 0.0).sum::<f32>().to_degrees().round(),
                piece_arc.to_degrees()
            );
        }
        let mut placed = Vec::new();
        let mut stands = Vec::new();
        for (i, info) in self.in_room.iter().enumerate() {
            if !info.lights {
                continue;
            }
            let light = if let Some((axis, cos_cover, path)) = self.room_stroke(i, info) {
                match self.stroke_room_lights(i, info, &shell, (axis, cos_cover, path), piece_arc, cancelled, run) {
                    Some(light) => light,
                    None => return false,
                }
            } else {
                self.box_room_lights(i, info, &shell, sky, piece_arc)
            };
            let Some(light) = light else { continue };
            // (a candle stands on its own mount)
            if info.stand.unwrap_or(p.stands) && !self.is_candle(i) {
                stands.push(light.stand);
            }
            placed.extend(light.pieces);
        }
        // The mirror's layers, each looked up where its light stands.
        // (a candle light is not mirrored by the floor: its model is not its flat face)
        let layered: Vec<usize> = (0..self.lights.len()).filter(|&i| self.casters[i].is_none() && self.lights[i].blend == Blend::Add && !self.is_candle(i)).collect();
        let far = mean_distance(placed.iter());
        let proxies: Vec<MirrorProxy> = layered.iter().map(|&i| self.mirror_proxy(i, &placed, far)).collect();
        self.mirror_flags = layered
            .iter()
            .map(|&i| (i + 1..self.lights.len()).filter(|&j| dims(&self.lights[j], &self.fx[j], self.casters[j].as_ref())).collect())
            .collect();
        self.mirrored = layered;
        // The daylight: the openings with the outside's light through each, and the discs out there.
        let openings = shell.openings();
        // a real window lets in its panes' share (its frame and bars keep the rest out)
        let windows: Vec<_> = openings.into_iter().map(|opening| (opening, opening.daylight(sky, hot) * opening.glass)).collect();
        let daylight = if windows.is_empty() { Vec::new() } else { hot.iter().map(|&(dir, radius)| disc_daylight(sky, dir, radius)).collect() };
        let light = |k: usize, d: Vec3f, foot: f32| self.mirrored_light(k, d, sky, foot);
        // The pattern lights' housed tubes' bodies (pattern.rs `housings`).
        let housings = self.lights.iter().filter_map(|l| l.shaped.as_ref()?.pattern()).map(|p| p.housings().to_vec()).collect();
        let room = shell
            .with_daylight(windows, daylight)
            .with_lights(placed, &stands)
            .with_housings(housings)
            .with_grids(run)
            .with_mirror(proxies, self.texel, &light, sky, run);
        // The candles' models take the room's light where they stand (their own flames apart).
        let candles: Vec<usize> = (0..self.models.len()).filter(|&i| self.is_candle(i)).collect();
        for &i in &candles {
            let Some(at) = self.models[i].as_ref().and_then(LightModel::candle_middle) else { continue };
            let apart = |entry: usize| candles.contains(&entry);
            let axes = [vec3f(1.0, 0.0, 0.0), vec3f(-1.0, 0.0, 0.0), vec3f(0.0, 1.0, 0.0), vec3f(0.0, -1.0, 0.0), vec3f(0.0, 0.0, 1.0), vec3f(0.0, 0.0, -1.0)];
            let cube = axes.map(|n| room.irradiance_apart(at, n, &apart));
            if let Some(model) = self.models[i].as_mut() {
                model.set_candle_light(CandleLight::Cube(cube));
            }
        }
        self.room = Some(std::sync::Arc::new(room));
        true
    }

    /// Whether entry `i` is a candle light (its model is a candle's).
    fn is_candle(&self, i: usize) -> bool {
        self.models.get(i).and_then(Option::as_ref).is_some_and(|m| m.candle_middle().is_some())
    }

    /// Whether any entry is a candle light.
    pub fn has_candles(&self) -> bool {
        (0..self.models.len()).any(|i| self.is_candle(i))
    }

    /// Lights the candles' models outdoors (Sky mode): the floor's light `lit`, its key `key` (the map's frame), the
    /// ground's albedo. A room's candles are lit by the room instead (`build_room`).
    pub fn light_candles_outdoors(&mut self, lit: crate::floor::FloorLighting, key: Option<Vec3f>, ground: Vec3f) {
        if self.room.is_some() {
            return;
        }
        for model in self.models.iter_mut().flatten() {
            model.set_candle_light(CandleLight::Sky { lit, key, ground });
        }
    }

    /// Lights the candles' models in a studio without a room: the backdrop's top over them, its floor under them and
    /// its horizon round them.
    fn light_candles_by_backdrop(&mut self) {
        let pi = std::f32::consts::PI;
        let (top, horizon, floor) = (self.top * pi, self.horizon * pi, self.floor * pi);
        let side = (top + floor) * 0.25 + horizon * 0.5;
        for model in self.models.iter_mut().flatten() {
            model.set_candle_light(CandleLight::Cube([side, side, top, floor, side, side]));
        }
    }

    /// The floor mirror's light `k` (`mirrored`) alone over black along `d`,
    /// as the map draws it for a footprint `foot` radians wide (a pattern
    /// filters its picture over it), under the lights after it that dim it.
    fn mirrored_light(&self, k: usize, d: Vec3f, sky: &dyn Fn(Vec3f) -> Vec3f, foot: f32) -> Vec3f {
        let w = self.entry_alone(self.mirrored[k], d, sky, foot);
        if w == Vec3f::default() {
            return w;
        }
        let flags = self.mirror_flags[k].iter().map(|&j| (&self.lights[j], &self.fx[j], self.casters[j].as_ref()));
        dimmed_by(flags, d, w)
    }

    /// Entry `i` alone over black along `d`, as the map draws it for a texel
    /// `texel` tall: its own drawing past what hangs in front of it, a
    /// window's sky through its glass (`sky`).
    fn entry_alone(&self, i: usize, d: Vec3f, sky: &dyn Fn(Vec3f) -> Vec3f, texel: f32) -> Vec3f {
        let (light, fx) = (&self.lights[i], &self.fx[i]);
        let black = Vec3f::default();
        if fx.fills_with_sky() {
            let cover = light.cover(d);
            if cover > 0.0 { fx.window(d, cover, black, sky) } else { black }
        } else if fx.is_plain() {
            light.draw_at(d, black, false, texel)
        } else {
            fx.apply_at(d, black, light.draw_at(d, black, false, texel), texel)
        }
    }

    /// Where the floor's mirror looks entry `i` up (room.rs `MirrorProxy`):
    /// the plane its pieces stand on in `placed` (a 2D light's face facing
    /// the camera, a 3D light's own face), at their light-weighted mean; for
    /// a stroke, whose pieces follow its line, the sphere as far off as they
    /// stand on the mean; for a light with no pieces (a paint layer, one
    /// that lights nothing), the sphere `far` off, all the lights' mean.
    fn mirror_proxy(&self, i: usize, placed: &[RoomLight], far: f32) -> MirrorProxy {
        let own: Vec<&RoomLight> = placed.iter().filter(|l| l.entry == i).collect();
        if own.is_empty() || self.room_stroke(i, &self.in_room[i]).is_some() {
            let radius = if own.is_empty() { far } else { mean_distance(own.into_iter()) };
            return MirrorProxy::Sphere { radius };
        }
        let frame = &self.lights[i].frame;
        let normal = frame.face.unwrap_or(frame.center).normalize();
        let (mut sum, mut weight) = (0.0f32, 0.0f32);
        for l in &own {
            let k = luminance(l.intensity).max(1.0e-9);
            sum += l.pos.dot(normal) * k;
            weight += k;
        }
        MirrorProxy::Plane { normal, offset: sum / weight }
    }

    /// The centre line entry `i`'s room pieces follow (its axis, covering
    /// cone and path): a stroke's. None for any other light, for a stroke
    /// filled with sky (a window, whose light is the sky: `box_room_lights`
    /// cuts its box), and for a stroke whose gobo casts, which casts from its
    /// middle (gobo.rs) and so lights the room as one light there.
    fn room_stroke(&self, i: usize, info: &InRoom) -> Option<(Vec3f, f32, &ArcPath)> {
        if self.fx[i].fills_with_sky() || info.casts {
            return None;
        }
        let shaped = self.lights[i].shaped.as_ref()?;
        let (axis, _, cos_cover) = shaped.stroke_cones()?;
        Some((axis, cos_cover, shaped.stroke_path()?))
    }

    /// How long entry `i` is in the room, radians: a stroke's centre line,
    /// else the angle its box spans along its long side. 0 for a light
    /// whose gobo casts (one light, its gobo projected from its middle).
    fn room_length(&self, i: usize, info: &InRoom) -> f32 {
        if info.casts {
            return 0.0;
        }
        if let Some((_, _, path)) = self.room_stroke(i, info) {
            return path.length();
        }
        if self.lights[i].shaped.as_ref().is_some_and(|s| s.pattern().is_some()) {
            // A pattern is cut into a grid (`pattern_room_lights`): its area
            // as the length that stands as as many pieces at ROOM_PIECE_DEG.
            let (across, down) = face_spans(&self.lights[i].frame, self.lights[i].half);
            return across * down / ROOM_PIECE_DEG.to_radians();
        }
        let reach = self.lights[i].reach;
        let frame = &self.lights[i].frame;
        if frame.face.is_some() {
            // A 3D face: the angle between the ends of its long side, as the camera sees them.
            let along = if reach.x >= reach.y { frame.right * reach.x } else { frame.up * reach.y };
            let (a, b) = ((frame.center + along).normalize(), (frame.center - along).normalize());
            return a.dot(b).max(-1.0).min(1.0).acos();
        }
        2.0 * reach.x.max(reach.y).min(BOX_FAR_TAN).atan()
    }

    /// Entry `i`, a light in its own box (or a window or a stroke that casts,
    /// in its stroke's box), as lights in `room`, with what the map draws of
    /// it alone over black (its emission over its reach box, past what hangs
    /// in front of it and under the flags after it) as their intensity. Cut
    /// across its long side into even angles of at most `piece_arc`, each
    /// piece a light that stands where its light is seen (its
    /// luminance-weighted mean direction) on the light's face: the plane
    /// facing the camera at the light's distance (`LightParams::distance_m`,
    /// else DEFAULT_REACH of the way to the surface it faces), as a straight
    /// tube or strip is, short of the room's surface. One light that is no
    /// longer than a piece, or whose gobo casts (projected from its middle),
    /// stands where its centre is seen. Every piece faces the way the light
    /// does, so what shapes its beam (`LightFx::beam`) shapes every piece's.
    /// Its stand is the whole light's, under its centre. None for a light
    /// that puts out nothing.
    fn box_room_lights(&self, i: usize, info: &InRoom, room: &Room, sky: &dyn Fn(Vec3f) -> Vec3f, piece_arc: f32) -> Option<InRoomLight> {
        if let (Some(space), Some(pattern)) = (self.lights[i].space, self.lights[i].shaped.as_ref().and_then(|s| s.pattern())) {
            return self.pattern_room_lights(i, info, space, pattern, piece_arc);
        }
        if let Some(space) = self.lights[i].space {
            return self.box_room_lights_3d(i, info, space, sky, piece_arc);
        }
        let light = &self.lights[i];
        let fx = &self.fx[i];
        let flags = dimmers(&self.lights[i + 1..], &self.fx[i + 1..], &self.casters[i + 1..]);
        let black = Vec3f::default();
        let frame = &light.frame;
        let along_x = light.reach.x >= light.reach.y;
        let count = pieces_over(self.room_length(i, info), piece_arc);
        // The long side's half angle, which the pieces share evenly.
        let half_angle = 0.5 * self.room_length(i, info);
        let n = 24usize;
        let n_long = n.max(BOX_PIECE_SAMPLES * count);
        let (nx, ny) = if along_x { (n_long, n) } else { (n, n_long) };
        let cell = vec2f(2.0 * light.reach.x / nx as f32, 2.0 * light.reach.y / ny as f32);
        let mut pieces = vec![(Vec3f::default(), 0.0f32, Vec3f::default()); count];
        for jy in 0..ny {
            let y = -light.reach.y + (jy as f32 + 0.5) * cell.y;
            for jx in 0..nx {
                let x = -light.reach.x + (jx as f32 + 0.5) * cell.x;
                let r2 = 1.0 + x * x + y * y;
                if !(r2 <= BOX_FAR_TAN * BOX_FAR_TAN) {
                    continue;
                }
                let d = (frame.center + frame.right * x + frame.up * y).normalize();
                let own = if fx.fills_with_sky() {
                    let cover = light.cover(d);
                    if cover > 0.0 { fx.window(d, cover, black, sky) } else { black }
                } else if fx.is_plain() {
                    light.draw(d, black, false)
                } else {
                    fx.apply(d, black, light.draw(d, black, false))
                };
                let seen = dimmed(&flags, d, own);
                let dw = cell.x * cell.y / (r2 * r2.sqrt());
                // The planes through the camera and the short side's axis cut the pieces.
                let k = if count == 1 {
                    0
                } else {
                    let a = (if along_x { x } else { y }).atan();
                    (((a + half_angle) / (2.0 * half_angle) * count as f32).max(0.0) as usize).min(count - 1)
                };
                let piece = &mut pieces[k];
                piece.0 += seen * dw;
                let lum = luminance(seen);
                if lum > 0.0 {
                    piece.1 += dw;
                    piece.2 += d * (lum * dw);
                }
            }
        }
        let center = frame.center;
        let to_surface = room.reach(center);
        let distance = info.distance.unwrap_or(DEFAULT_REACH * to_surface).min(0.95 * to_surface).max(0.3);
        let normal = fx.face_normal();
        let stand = Stand { at: center * distance, half_height: light.half.y.max(light.half.x) * distance, by_bottom: false };
        let mut out = Vec::with_capacity(count);
        for &(emission, solid, toward) in &pieces {
            if !(luminance(emission) > 0.0) || !(solid > 0.0) {
                continue;
            }
            let (dir, at) = if count == 1 {
                (center, distance)
            } else {
                if !(toward.length() > 0.0) {
                    continue;
                }
                let dir = toward.normalize();
                (dir, (distance / dir.dot(center).max(1.0e-3)).min(0.95 * room.reach(dir)).max(0.3))
            };
            // Seen from the camera at this slant, the face's own area is its solid angle's patch over the slant.
            let slant = normal.dot(-dir).max(0.2);
            let area = solid * at * at / slant;
            out.push(RoomLight {
                pos: dir * at,
                normal,
                // A Lambertian face's intensity along its normal: what reaches the camera over the slant.
                intensity: emission * (at * at / slant),
                near: area * std::f32::consts::FRAC_1_PI,
                entry: i,
                shaped_beam: fx.shapes_beam(info.casts),
                omni: false,
            });
        }
        (!out.is_empty()).then_some(InRoomLight { pieces: out, stand })
    }

    /// Plan P1: entry `i`, a light placed in 3D, as lights in the room: where
    /// it is, not by the 70 % rule. Its face is sampled in its own plane over
    /// its reach box, each sample what the map draws of it there over black
    /// (past what hangs in front of it and under the flags after it, as
    /// `box_room_lights` takes a light's) but at its full radiance, not
    /// through its beam toward the camera, so a spot turned away from the
    /// camera still lights the room. A face's patch of area dA and radiance L
    /// is a Lambertian emitter of intensity L dA along its normal, so each
    /// piece's intensity is the sum of its patches' and its area theirs where
    /// it draws. Cut along the face's real long side into even lengths (by
    /// `room_length`'s angle against `piece_arc`), each piece stands at its
    /// light's luminance-weighted mean point of the face and faces the way
    /// the face's front does (`LightFx::face_normal`); its beam
    /// (`LightFx::beam`: the face's beam, grid and doors) shapes what it
    /// throws. Its stand is under the face's middle, as high as the face
    /// reaches up and down. Nothing holds it inside the room: a piece past a
    /// wall still lights what faces it (the room casts no shadows), but not
    /// that wall, whose surface faces in. None for a light that puts out
    /// nothing.
    ///
    /// Cost per bake: (24 x 24 or more, BOX_PIECE_SAMPLES x pieces along the
    /// long side) samples of the light, as a 2D light's.
    fn box_room_lights_3d(&self, i: usize, info: &InRoom, space: InSpace, sky: &dyn Fn(Vec3f) -> Vec3f, piece_arc: f32) -> Option<InRoomLight> {
        let light = &self.lights[i];
        // A candle light: each flame a small light that shines all round, at its intensity in the light's colour.
        if let Some(flames) = self.models[i].as_ref().map(LightModel::candle_flames).filter(|f| !f.is_empty()) {
            let out: Vec<RoomLight> = flames
                .iter()
                .map(|&(pos, intensity)| RoomLight {
                    pos,
                    normal: vec3f(0.0, 1.0, 0.0),
                    intensity: light.color * intensity,
                    near: 0.01 * 0.01,
                    entry: i,
                    shaped_beam: false,
                    omni: true,
                })
                .collect();
            let at = self.models[i].as_ref().and_then(LightModel::candle_middle).unwrap_or(out[0].pos);
            return Some(InRoomLight { pieces: out, stand: Stand { at, half_height: 0.0, by_bottom: true } });
        }
        let fx = &self.fx[i];
        let flags = dimmers(&self.lights[i + 1..], &self.fx[i + 1..], &self.casters[i + 1..]);
        let black = Vec3f::default();
        let frame = &light.frame;
        let along_x = light.reach.x >= light.reach.y;
        let count = pieces_over(self.room_length(i, info), piece_arc);
        let n = 24usize;
        let n_long = n.max(BOX_PIECE_SAMPLES * count);
        let (nx, ny) = if along_x { (n_long, n) } else { (n, n_long) };
        let cell = vec2f(2.0 * light.reach.x / nx as f32, 2.0 * light.reach.y / ny as f32);
        let d = space.distance;
        // A patch's area on the face, square metres: the plane is the face over its distance.
        let da = cell.x * cell.y * d * d;
        // (intensity, area where it draws, luminance-weighted place, the weights)
        let mut pieces = vec![(Vec3f::default(), 0.0f32, Vec3f::default(), 0.0f32); count];
        for jy in 0..ny {
            let y = -light.reach.y + (jy as f32 + 0.5) * cell.y;
            for jx in 0..nx {
                let x = -light.reach.x + (jx as f32 + 0.5) * cell.x;
                let q = frame.center + frame.right * x + frame.up * y;
                let dir = q.normalize();
                let own = if fx.fills_with_sky() {
                    let cover = light.cover(dir);
                    if cover > 0.0 { fx.window(dir, cover, black, sky) } else { black }
                } else if fx.is_plain() {
                    light.draw_beamed(dir, black, false, false)
                } else {
                    fx.apply(dir, black, light.draw_beamed(dir, black, false, false))
                };
                let seen = dimmed(&flags, dir, own);
                let lum = luminance(seen);
                if !(lum > 0.0) {
                    continue;
                }
                let k = if count == 1 {
                    0
                } else {
                    let (at, half) = if along_x { (x, light.reach.x) } else { (y, light.reach.y) };
                    (((at + half) / (2.0 * half) * count as f32).max(0.0) as usize).min(count - 1)
                };
                let piece = &mut pieces[k];
                piece.0 += seen * da;
                piece.1 += da;
                piece.2 += q * (d * lum * da);
                piece.3 += lum * da;
            }
        }
        let normal = fx.face_normal();
        let mut out = Vec::with_capacity(count);
        for &(intensity, area, toward, weight) in &pieces {
            if !(luminance(intensity) > 0.0) || !(area > 0.0) || !(weight > 0.0) {
                continue;
            }
            out.push(RoomLight {
                pos: toward / weight,
                normal,
                intensity,
                near: area * std::f32::consts::FRAC_1_PI,
                entry: i,
                shaped_beam: fx.shapes_beam(info.casts),
                omni: false,
            });
        }
        // The stand holds the face's middle; its half height is how far the face reaches up and down.
        let tall = d * (frame.up.y.abs() * light.half.y + frame.right.y.abs() * light.half.x);
        (!out.is_empty()).then_some(InRoomLight { pieces: out, stand: Stand { at: frame.center * d, half_height: tall, by_bottom: true } })
    }

    /// Plan P2: entry `i`, a pattern light, as lights in the room: its
    /// picture (pattern.rs, level 0) cut into a grid of cells over its face,
    /// about as many as its length (`room_length`, its area) stands as at
    /// `piece_arc`, as long each way as the face is seen. Each cell is a
    /// light of the summed emission x area of its texels (the light's colour
    /// at its full radiance, not through its beam toward the camera), at their
    /// luminance-weighted mean point, facing the face's front through its
    /// beam (`LightFx::beam`), under the flags after it (taken at each of
    /// PATTERN_ROOM_SUB² sub-cells' mean points). Its stand is a 3D face's
    /// (`box_room_lights_3d`): a ceiling grid hangs. None for a pattern that
    /// puts out nothing.
    ///
    /// Cost per bake: one pass over the raster's level 0 (a few operations a
    /// lit texel) and the flags at up to PATTERN_ROOM_SUB² x pieces points.
    fn pattern_room_lights(&self, i: usize, info: &InRoom, space: InSpace, pattern: &PatternLight, piece_arc: f32) -> Option<InRoomLight> {
        let light = &self.lights[i];
        let fx = &self.fx[i];
        let flags = dimmers(&self.lights[i + 1..], &self.fx[i + 1..], &self.casters[i + 1..]);
        let (frame, half) = pattern.face();
        let d = space.distance;
        let (across, down) = face_spans(&frame, half);
        let count = pieces_over(self.room_length(i, info), piece_arc);
        let (nx, ny) = grid_of(count, across, down);
        let (sx, sy) = (nx * PATTERN_ROOM_SUB, ny * PATTERN_ROOM_SUB);
        let sign = box_x_sign();
        // Per sub-cell: emission x area (square metres), lit area, luminance-weighted plane point, the weight.
        let mut subs = vec![(Vec3f::default(), 0.0f32, Vec2f::default(), 0.0f32); sx * sy];
        let da = pattern.texel_area() * d * d;
        pattern.for_each_texel(|p, rgb| {
            let cx = (((p.x * sign + half.x) / (2.0 * half.x) * sx as f32).max(0.0) as usize).min(sx - 1);
            let cy = (((half.y - p.y) / (2.0 * half.y) * sy as f32).max(0.0) as usize).min(sy - 1);
            let lum = luminance(rgb);
            let sub = &mut subs[cy * sx + cx];
            sub.0 += rgb * da;
            sub.1 += da;
            sub.2 += p * (lum * da);
            sub.3 += lum * da;
        });
        let mut pieces = vec![(Vec3f::default(), 0.0f32, Vec3f::default(), 0.0f32); nx * ny];
        for (k, sub) in subs.iter().enumerate() {
            if !(sub.3 > 0.0) {
                continue;
            }
            let at = sub.2 / sub.3;
            let q = frame.center + frame.right * at.x + frame.up * at.y;
            let seen = dimmed(&flags, q.normalize(), sub.0 * light.color);
            let lum = luminance(seen);
            if !(lum > 0.0) {
                continue;
            }
            let piece = &mut pieces[(k / sx / PATTERN_ROOM_SUB) * nx + (k % sx) / PATTERN_ROOM_SUB];
            piece.0 += seen;
            piece.1 += sub.1;
            piece.2 += q * (d * lum);
            piece.3 += lum;
        }
        let normal = fx.face_normal();
        let out: Vec<RoomLight> = pieces
            .iter()
            .filter(|p| luminance(p.0) > 0.0 && p.1 > 0.0 && p.3 > 0.0)
            .map(|&(intensity, area, toward, weight)| RoomLight {
                pos: toward / weight,
                normal,
                intensity,
                near: area * std::f32::consts::FRAC_1_PI,
                entry: i,
                shaped_beam: fx.shapes_beam(info.casts),
                omni: false,
            })
            .collect();
        let tall = d * (frame.up.y.abs() * half.y + frame.right.y.abs() * half.x);
        (!out.is_empty()).then_some(InRoomLight { pieces: out, stand: Stand { at: frame.center * d, half_height: tall, by_bottom: true } })
    }

    /// Entry `i`, a stroke (`room_stroke`: its axis, covering cone and
    /// centre line), as lights in `room`: its centre line cut into even
    /// pieces of at most `piece_arc`, each near enough a compact emitter.
    /// Each piece's light is what the map draws of the stroke nearest it,
    /// alone over black, past what hangs in front of it and under the flags
    /// after it, as `box_room_lights` takes a light's (its width, profile,
    /// soft edge, hotspot and colour ramp as drawn; [`over_stroke_pieces`]);
    /// it stands where its light is seen (its luminance-weighted mean
    /// direction) at the light's distance, else DEFAULT_REACH of the way to
    /// the surface it faces, and faces the camera from there, or the light's
    /// aim. Its stand is under the piece nearest its middle. A piece that
    /// draws nothing is left out; every other piece's light counts. Some(None)
    /// when no piece draws anything; None when `cancelled` stopped it or the
    /// runner skipped some of its samples.
    ///
    /// Cost per bake: up to ROOM_PIECE_SAMPLES samples of the stroke's band,
    /// spread by `run`, each also finding its piece; then every room texel's
    /// irradiance sums one emitter per piece.
    #[allow(clippy::too_many_arguments)]
    fn stroke_room_lights(
        &self,
        i: usize,
        info: &InRoom,
        room: &Room,
        (axis, cos_cover, path): (Vec3f, f32, &ArcPath),
        piece_arc: f32,
        cancelled: &dyn Fn() -> bool,
        run: RowRunner,
    ) -> Option<Option<InRoomLight>> {
        let light = &self.lights[i];
        let fx = &self.fx[i];
        let flags = dimmers(&self.lights[i + 1..], &self.fx[i + 1..], &self.casters[i + 1..]);
        let black = Vec3f::default();
        let drawn = |d: Vec3f| {
            let own = if fx.is_plain() { light.draw(d, black, false) } else { fx.apply(d, black, light.draw(d, black, false)) };
            dimmed(&flags, d, own)
        };
        let count = pieces_over(path.length(), piece_arc);
        let arc = path.length() / count as f32;
        let band = light.shaped.as_ref().and_then(|s| s.stroke_band_reach()).unwrap_or(path.reach());
        let pieces = over_stroke_pieces(axis, cos_cover, path, band, count, &drawn, cancelled, run)?;
        let mut out = Vec::with_capacity(count);
        // (the piece's place along the stroke, 0..1, and its half extent in metres)
        let mut placed = Vec::with_capacity(count);
        for (k, piece) in pieces.iter().enumerate() {
            let emission = vec3f(piece.emission[0] as f32, piece.emission[1] as f32, piece.emission[2] as f32);
            let toward = vec3f(piece.toward[0] as f32, piece.toward[1] as f32, piece.toward[2] as f32);
            let solid = piece.solid as f32;
            if !(luminance(emission) > 0.0) || !(solid > 0.0) || !(toward.length() > 0.0) {
                continue;
            }
            let dir = toward.normalize();
            let to_surface = room.reach(dir);
            let distance = info.distance.unwrap_or(DEFAULT_REACH * to_surface).min(0.95 * to_surface).max(0.3);
            let normal = fx.face_normal_toward(dir);
            // As `box_room_lights`: the face's area is its solid angle's patch over the slant.
            let slant = normal.dot(-dir).max(0.2);
            let area = solid * distance * distance / slant;
            out.push(RoomLight {
                pos: dir * distance,
                normal,
                intensity: emission * (distance * distance / slant),
                near: area * std::f32::consts::FRAC_1_PI,
                entry: i,
                shaped_beam: fx.shapes_beam(info.casts),
                omni: false,
            });
            // The piece's own half extent: half its length and the band's reach.
            placed.push(((k as f32 + 0.5) / count as f32, (0.5 * arc + path.reach()).min(1.5).tan() * distance));
        }
        let middle = placed.iter().enumerate().min_by(|(_, a), (_, b)| (a.0 - 0.5).abs().total_cmp(&(b.0 - 0.5).abs())).map(|(k, _)| k);
        Some(middle.map(|k| InRoomLight { stand: Stand { at: out[k].pos, half_height: placed[k].1, by_bottom: false }, pieces: out }))
    }

    /// Whether the base is a room (`build_room`) instead of the gradient.
    pub fn has_room(&self) -> bool {
        self.room.is_some()
    }

    /// The room along `d` (map frame, unit), the lights' light on it, its
    /// mirror and its rigging, and through an open wall `sky(d)`, for a
    /// lookup whose texel is `texel` radians (0 for a point; the mirror
    /// filters over it, `Room::radiance`). None without a room: the base is
    /// `backdrop(d)`.
    pub fn room_radiance(&self, d: Vec3f, texel: f32, sky: &dyn Fn(Vec3f) -> Vec3f) -> Option<Vec3f> {
        let room = self.room.as_ref()?;
        let beam = |entry: usize, w: Vec3f| self.fx[entry].beam(w, self.in_room[entry].casts);
        let light = |k: usize, w: Vec3f, foot: f32| self.mirrored_light(k, w, sky, foot);
        Some(room.radiance(d, texel, &beam, sky, &light))
    }

    /// The key light `index` hands the engine, measured from what the map
    /// draws of it (D1). None for a window filled with sky: its light is the
    /// sky, which `Env` measures it against (`measured_window_key`).
    fn measured_key(&self, index: usize) -> Option<EnvSun> {
        let light = &self.lights[index];
        let fx = &self.fx[index];
        if fx.fills_with_sky() {
            return None;
        }
        #[cfg(test)]
        super::probe::count(super::probe::Work::Key);
        let stroke = light.shaped.as_ref().and_then(|s| Some((s.stroke_cones()?, s.stroke_path()?)));
        if let Some(((axis, cos_radius, cos_cover), path)) = stroke {
            // A stroke's own light, after what hangs in front of it, under the
            // lights after it: what the map draws of it, sampled where its band
            // runs (its extras are not taken as a share apart, as a boxed key's
            // are: the samples along its band see them).
            let flags = dimmers(&self.lights[index + 1..], &self.fx[index + 1..], &self.casters[index + 1..]);
            let black = Vec3f::default();
            let drawn = |d: Vec3f| dimmed(&flags, d, fx.apply(d, black, light.draw(d, black, false)));
            let band = light.shaped.as_ref().and_then(|s| s.stroke_band_reach()).unwrap_or(path.reach());
            let integral = over_cone(axis, cos_cover, &stroke_cells(path, band, cos_cover), &drawn);
            return Some(EnvSun { dir: axis, radiance: EnvSun::cone_radiance(integral.emission, cos_radius), cos_radius, facing: integral.facing, cos_cover });
        }
        let integral = match light.shaped.as_ref().and_then(|s| s.pattern()) {
            // A pattern's light is summed from its picture, texel for texel (plan P2).
            Some(pattern) => pattern_key_emission(&self.lights[index..], &self.fx[index..], &self.casters[index..], pattern),
            None => key_emission(&self.lights[index..], &self.fx[index..], &self.casters[index..], KEY_GRID),
        };
        let key = EnvSun {
            dir: light.frame.center,
            // Over the cone the f32 cosine names, the one every consumer of
            // the key computes, so the emission comes back exactly however
            // small the light (`EnvSun::cone_radiance`).
            radiance: EnvSun::cone_radiance(integral.emission, light.key_cos),
            cos_radius: light.key_cos,
            facing: integral.facing,
            // The reach box's cone can only be wider than the nominal one
            // (it holds the half-size on both sides); the min keeps rounding
            // from putting it above.
            cos_cover: integral.cos_cover.min(light.key_cos),
        };
        // The engine's key light carries what gets past its own gobo, doors and
        // grid, times its face's wear (its radiance; its cone, facing share and
        // covering cone are its shape's, which the extras only cut).
        // A boxed key's colour is its light's, times the face's mean colour (a dirty cover's stains): a pattern's
        // emission, summed from what the map draws, carries it already.
        let radiance = key.radiance * self.key_share(index);
        let radiance = match self.fx[index].tint_mean() {
            Some(t) if light.shaped.as_ref().and_then(|s| s.pattern()).is_none() => vec3f(radiance.x * t.x, radiance.y * t.y, radiance.z * t.z),
            _ => radiance,
        };
        Some(EnvSun { radiance, ..key })
    }

    /// Whether the key is a window filled with sky, which `Env` measures
    /// against its sky ([`Self::measured_window_key`]).
    pub(super) fn key_is_window(&self) -> bool {
        self.key_index.is_some_and(|i| self.fx[i].fills_with_sky())
    }

    /// The key when it is a window filled with sky (D1, L:136): the sky it
    /// lets in, through its glass at 2^intensity_ev and its gobo, times its
    /// face's wear, under the lights after it, exactly the light the map shows
    /// through it and the keyless copy lacks (the copy keeps the glass and lets
    /// no sky in: `apply_lights_sky`). Not its own Kelvin colour, which the
    /// window never shows. `sky(d)` is the sky the window shows along `d` (the
    /// map's own frame): the map's base in Sky mode, the lightless sky behind
    /// a studio. `hot` are the discs in it (direction, angular radius), which
    /// a bake area-averages and this integral samples finely too. `fine(d,
    /// radius)` is the spacing the sky must be sampled at within `radius` of
    /// `d` where it draws a line thinner than a cell's samples (a storm's
    /// bolt, which a bake averages over its texels), None where it is smooth:
    /// a cell the line may cross takes that spacing. None when the key is no
    /// such window.
    pub(super) fn measured_window_key(
        &self,
        sky: &dyn Fn(Vec3f) -> Vec3f,
        hot: &[(Vec3f, f32)],
        fine: &dyn Fn(Vec3f, f32) -> Option<f32>,
    ) -> Option<EnvSun> {
        let index = self.key_index.filter(|&i| self.fx[i].fills_with_sky())?;
        #[cfg(test)]
        super::probe::count(super::probe::Work::Key);
        let (light, fx) = (&self.lights[index], &self.fx[index]);
        let (axis, cos_radius, cos_cover) = light.key_cones();
        let flags = dimmers(&self.lights[index + 1..], &self.fx[index + 1..], &self.casters[index + 1..]);
        let drawn = |d: Vec3f| {
            let cover = self.cover(index, d);
            // Only the window is looked at: the sky is worked out nowhere else.
            if !(cover > 0.0) {
                return Vec3f::default();
            }
            dimmed(&flags, d, fx.lets_in(d, cover, fx.glass(d, cover), sky))
        };
        let stroke = light.shaped.as_ref().and_then(|s| Some(stroke_cells(s.stroke_path()?, s.stroke_band_reach()?, cos_cover)));
        let side = cone_cell(cos_cover);
        let cells = |centre: Vec3f| {
            let own = match &stroke {
                Some(stroke) => stroke(centre),
                None => WINDOW_FINE,
            };
            // A disc behind the glass: its cell is sampled finely enough to find it.
            let disc = hot
                .iter()
                .filter(|(dir, r)| centre.dot(*dir).clamp(-1.0, 1.0).acos() < side * std::f32::consts::FRAC_1_SQRT_2 + r)
                .map(|(_, r)| (side / (r / DISC_SAMPLES_PER_RADIUS)).ceil() as usize)
                .max()
                .unwrap_or(0);
            // A line behind the glass that may cross the cell: sampled at its own spacing.
            let line = fine(centre, side * std::f32::consts::FRAC_1_SQRT_2).map_or(0, |step| (side / step).ceil() as usize);
            own.max(disc).max(line)
        };
        let integral = over_cone(axis, cos_cover, &cells, &drawn);
        Some(EnvSun { dir: axis, radiance: EnvSun::cone_radiance(integral.emission, cos_radius), cos_radius, facing: integral.facing, cos_cover })
    }

    /// Whether any light is drawn over the base: with none, nothing stands in
    /// front of the sun or the moon.
    pub(super) fn has_lights(&self) -> bool {
        !self.lights.is_empty()
    }

    /// The studio entries that are casters with bloom on (bloom_strength > 0
    /// and bloom_length_deg > 0), in list order: the ones `Env`'s pre-pass
    /// builds a raster for.
    pub(super) fn bloom_casters(&self) -> Vec<usize> {
        self.casters.iter().enumerate().filter_map(|(i, c)| c.as_ref().filter(|c| c.wants_bloom()).map(|_| i)).collect()
    }

    /// Where the lights listed before entry `end` may add light (an Add light, or a window
    /// filled with sky), for a caster's bloom there (`CasterBloom::build`): each one's
    /// [`BrightBox`]. Flags and casters only take light away and are left out.
    pub(super) fn bright_boxes(&self, end: usize) -> Vec<BrightBox> {
        let end = end.min(self.lights.len());
        (0..end)
            .filter(|&i| self.casters[i].is_none() && (self.lights[i].blend == Blend::Add || self.fx[i].fills_with_sky()))
            .map(|i| self.lights[i].bright_box(!self.fx[i].is_plain()))
            .collect()
    }

    /// Hands caster `entry` its bloom. It is no longer plain, so the per-entry
    /// loop runs from now on.
    pub(super) fn set_caster_bloom(&mut self, entry: usize, bloom: CasterBloom) {
        if let Some(Some(caster)) = self.casters.get_mut(entry) {
            caster.set_bloom(bloom);
            self.any_fx = true;
        }
    }

    /// Takes every caster's bloom away, so the studio is as it was built
    /// (`Env::with_skyline` rasters the bloom again over a new base).
    pub(super) fn clear_caster_blooms(&mut self) {
        for caster in self.casters.iter_mut().flatten() {
            caster.clear_bloom();
        }
        self.any_fx = self.fx.iter().any(|fx| !fx.is_plain()) || self.casters.iter().flatten().any(|c| !c.is_plain()) || self.models.iter().any(Option::is_some);
    }

    /// The caster at studio entry `entry`; None for a light.
    pub(super) fn caster(&self, entry: usize) -> Option<&CasterLayer> {
        self.casters.get(entry).and_then(Option::as_ref)
    }

    /// The analytic coverage of the caster at `entry` along `dir`
    /// ([`caster_sharp_coverage`]), which its softness raster softens and its
    /// bloom thins.
    pub(super) fn caster_sharp(&self, entry: usize, dir: Vec3f) -> f32 {
        caster_sharp_coverage(&self.lights[entry], &self.fx[entry], dir)
    }

    /// The share of the key's emission that gets past its gobo, doors and
    /// grid, times its wear (B10): `LightFx::key_share`, weighted by what the
    /// key puts into the map, its own drawing (shape, soft edge, hotspot,
    /// picture) under the flags, the cloth and the windows after it, as
    /// `key_emission` dims it. So the engine's key light loses what the map
    /// loses (plan 2's N3 removes exactly that). Sampled over the key's reach
    /// box. (A stroke's key samples its extras with its own drawing, along
    /// its band.)
    fn key_share(&self, index: usize) -> f32 {
        let light = &self.lights[index];
        let flags = dimmers(&self.lights[index + 1..], &self.fx[index + 1..], &self.casters[index + 1..]);
        let drawn = |dir: Vec3f| luminance(dimmed(&flags, dir, light.draw(dir, Vec3f::default(), false)));
        self.fx[index].key_share(&light.frame, light.reach, &drawn)
    }

    /// The vertical gradient.
    /// - The horizon colour holds exactly at elevation 0.
    /// - Above it the gradient reaches `top`, and below it `floor`, over
    ///   `horizon_softness x 90 degrees` of elevation.
    pub fn backdrop(&self, dir: Vec3f) -> Vec3f {
        let elevation = sane(dir.y, -1.0, 1.0).asin().to_degrees();
        if elevation >= 0.0 {
            self.horizon + (self.top - self.horizon) * smoothstep(0.0, self.soft_deg, elevation)
        } else {
            self.horizon + (self.floor - self.horizon) * smoothstep(0.0, self.soft_deg, -elevation)
        }
    }

    /// Draws the lights over `base` in list order. A window filled with sky
    /// shows `base` through it here; `Env` calls `apply_lights_sky` with the
    /// map's real sky.
    pub fn apply_lights(&self, dir: Vec3f, base: Vec3f) -> Vec3f {
        self.apply_lights_sky(dir, base, &|_| base, false, 0.0)
    }

    /// [`Self::apply_lights`] without the key light (N3: the map without its
    /// key is the source of the engine's lighting copy). Skipping it is the
    /// map with the key disabled, except that a scrim key's cloth stays
    /// (`PreparedLight::hide`) and so does a sky window's glass, letting no
    /// sky in: every other light, the flags after the key included, draws as
    /// before.
    pub fn apply_lights_without_key(&self, dir: Vec3f, base: Vec3f) -> Vec3f {
        self.apply_lights_sky(dir, base, &|_| base, true, 0.0)
    }

    /// `apply_lights` with `sky(dir)`, the sky a window filled with sky
    /// (`GoboParams::fill_sky`) shows. `sky` is called only for a direction
    /// such a window covers. `without_key` leaves the key light out (plan 2's
    /// N3, `apply_lights_without_key`; `Env::radiance_with` under
    /// `KeyTerm::Light`).
    ///
    /// Each light is drawn by its own code (`draw_lights`); what hangs in
    /// front of it then scales what it did (`LightFx::apply`), so a key left
    /// out leaves what it hides scaled as in the map and nothing of its own.
    ///
    /// A shadow caster multiplies what is drawn before it by its
    /// transmission (redesign §5, `draw_caster`), here and over the key alike,
    /// its bloom included (R5).
    ///
    /// `texel` is the map texel the lookup stands for, radians tall (the
    /// bake's `Sampling::texel`; 0 for a lookup of no texel, which takes the
    /// width the studio was built for): a pattern light (plan P2) filters its
    /// picture over that texel's footprint.
    pub(super) fn apply_lights_sky(&self, dir: Vec3f, base: Vec3f, sky: &dyn Fn(Vec3f) -> Vec3f, without_key: bool, texel: f32) -> Vec3f {
        let skip = if without_key { self.key_index } else { None };
        self.apply_entries(dir, base, sky, skip, self.lights.len(), texel)
    }

    /// [`Self::apply_lights_sky`] over the entries 0..end only, nothing left out:
    /// what a caster at entry `end` stands in front of (its bloom's pre-pass,
    /// `Env::content_before`). Entries past the list are none.
    pub(super) fn apply_lights_until(&self, dir: Vec3f, base: Vec3f, sky: &dyn Fn(Vec3f) -> Vec3f, end: usize, texel: f32) -> Vec3f {
        self.apply_entries(dir, base, sky, None, end.min(self.lights.len()), texel)
    }

    /// The draw loop of both: the entries 0..end in list order, `skip` drawn as
    /// the key left out, for a map texel `texel` radians tall.
    fn apply_entries(&self, dir: Vec3f, base: Vec3f, sky: &dyn Fn(Vec3f) -> Vec3f, skip: Option<usize>, end: usize, texel: f32) -> Vec3f {
        if !self.any_fx {
            return self.draw_lights(0..end, dir, base, skip, texel);
        }
        let mut c = base;
        for (i, fx) in self.fx.iter().enumerate().take(end) {
            if let Some(caster) = &self.casters[i] {
                // A caster multiplies what is drawn before it and adds nothing
                // (`draw_caster`). It is never the key, so nothing skips it.
                c = draw_caster(&self.lights[i], fx, caster, dir, c);
            } else if let Some(model) = &self.models[i] {
                c = self.draw_model(i, model, dir, c, skip == Some(i), texel);
            } else if fx.is_plain() {
                c = self.draw_lights(i..i + 1, dir, c, skip, texel);
            } else if fx.fills_with_sky() {
                // A window is its glass showing the sky. Left out as the key it
                // keeps its glass and lets no sky in, as a scrim key keeps its
                // cloth: what is behind the glass (and against the sky, behind
                // its frame and bars: `LightFx::hides`) stays hidden, and the
                // map without it lacks exactly the sky the key carries (D1).
                let cover = self.cover(i, dir);
                if cover > 0.0 {
                    c = if skip == Some(i) { c * (1.0 - fx.hides(dir, cover)) } else { fx.window(dir, cover, c, sky) };
                }
            } else {
                let after = self.draw_lights(i..i + 1, dir, c, skip, texel);
                c = fx.apply_at(dir, c, after, texel);
            }
        }
        c
    }

    /// Entry `i` drawn as its 3D model (light_model.rs) over `acc` along
    /// `dir`, for a map texel `texel` radians tall: what the model covers
    /// hides what is behind it. Its cloth's front shows the light as its flat
    /// face draws it at the point the hit stands for (its own code over
    /// black, then what hangs in front of it, the parts the model draws as
    /// geometry left out of its modifier), through its beam along the ray
    /// itself and with its flash's image moved (`ModelHit::Face`); a
    /// translucent back glows with what passes the cloth; a solid is lit in
    /// the light's colour; what stands in front of the cloth (a flash tube,
    /// a lamp's dome) shows the whole face (`ModelHit::Whole`). `left_out`
    /// (the key left out, N3) keeps only what it hides.
    fn draw_model(&self, i: usize, model: &LightModel, dir: Vec3f, acc: Vec3f, left_out: bool, texel: f32) -> Vec3f {
        let tinted = |c: Vec3f, t: Option<Vec3f>| match t {
            Some(t) => vec3f(c.x * t.x, c.y * t.y, c.z * t.z),
            None => c,
        };
        let (light, fx) = (&self.lights[i], &self.fx[i]);
        let black = Vec3f::default();
        let mut own = black;
        let cover = model.trace(dir, texel, |d, hit, w| {
            if left_out {
                return;
            }
            let c = match hit {
                ModelHit::Face { at, mirror } => {
                    let (glow, tint) = fx.bare_glow_tint(at, texel);
                    tinted(light.draw_beamed(at, black, false, false) * (fx.passes(at) * glow * light.model_beam(d) * mirror), tint)
                }
                ModelHit::Back { at } => light.draw_beamed(at, black, false, false) * fx.back_glow(at).unwrap_or(0.0),
                ModelHit::Solid { level } => light.color * level,
                ModelHit::Candle(h) => vec3f(light.color.x * h.rel.x, light.color.y * h.rel.y, light.color.z * h.rel.z) + h.plain * luminance(light.color) + h.abs,
                ModelHit::Whole { at } => {
                    let (glow, tint) = fx.glow_tint(at, texel);
                    tinted(light.draw_beamed(at, black, false, false) * (fx.passes(at) * glow * light.model_beam(d)), tint)
                }
            };
            own = own + c * w;
        });
        // (a candle's flame alone covers nothing: its glow adds to what is behind it)
        if !(cover > 0.0) && own == black {
            return acc;
        }
        let c = acc * (1.0 - cover) + own;
        vec3f(c.x.max(0.0), c.y.max(0.0), c.z.max(0.0))
    }

    /// How much of light `index` covers `dir` ([`PreparedLight::cover`]).
    fn cover(&self, index: usize, dir: Vec3f) -> f32 {
        self.lights[index].cover(dir)
    }

    /// Draws the lights `range` of the list over `base`, in order, each with its own shape, content and blend.
    /// - **Add** accumulates the light's colour.
    /// - **Multiply** scales everything drawn so far by its colour: fully at the core,
    ///   not at all outside. That is how a black flag cuts a softbox or the sky.
    /// - Polygons, strokes and scrims follow the same rules through the shape core,
    ///   except that an Add scrim is cloth: it hides what is behind it, then glows.
    /// - An image light's colour is its picture at that point of its box, times
    ///   the light's colour; the picture's alpha, when it masks, fades the light in.
    /// - The light at `skip` (the key, for plan 2's keyless copy) loses its own light
    ///   and keeps what it hides (`PreparedLight::hide`).
    /// - A pattern (plan P2) filters its picture over the map texel `texel`.
    fn draw_lights(&self, range: std::ops::Range<usize>, dir: Vec3f, base: Vec3f, skip: Option<usize>, texel: f32) -> Vec3f {
        let mut c = base;
        for (i, light) in self.lights.iter().enumerate().skip(range.start).take(range.len()) {
            c = light.draw_at(dir, c, skip == Some(i), texel);
        }
        c
    }

    /// The key light in the map's own frame (unrotated, unscaled). Its cone covers the
    /// larger of width and height (a stroke's, its centre line and half width), and its
    /// radiance is the light's whole emission as the map draws it, over that cone, not
    /// its peak colour: a thin strip or a ring that fills a tenth of its cone hands over
    /// about a tenth of its colour, and a key half under a flag half of it. `facing` is
    /// the share of that emission a surface facing the key receives (a wide softbox's is
    /// below 1) and `cos_cover` the cone around the key's centre that holds its reach.
    /// None without a key, and for a window filled with sky, which `Env` measures
    /// against its sky.
    ///
    /// Measured on every call (the integrals of `measured_key`: tens of milliseconds
    /// for a key with a grid or a picture), so the preview's bakes, which never ask,
    /// never pay for it (D15, M5); `Env` measures it once, on the engine's first ask.
    pub fn key(&self) -> Option<EnvSun> {
        self.key_index.and_then(|index| self.measured_key(index))
    }

    /// The open boxes' bare tubes (`LightFx::core_radius`) as discs in the
    /// map's own frame, each its direction and angular radius: a crisp disc
    /// a few texels across that texel centres alone hit unevenly (a 34 x 26
    /// degree open softbox's 0.8 degree tube lost 2 % of its light to a 1K
    /// bake), so the bake area-averages its texels as it does the sun's.
    pub fn hot_cores(&self) -> Vec<(Vec3f, f32)> {
        self.lights
            .iter()
            .zip(&self.fx)
            .filter_map(|(light, fx)| {
                let radius = fx.core_radius()?;
                let frame = &light.frame;
                let centre = frame.center.normalize();
                let out = |side: Vec3f| centre.dot((frame.center + side * radius).normalize()).clamp(-1.0, 1.0).acos();
                Some((centre, out(frame.right).max(out(frame.up))))
            })
            .collect()
    }
}

/// What one pass over the key light gives.
struct KeyIntegral {
    /// The key's whole emission, ∫ L dΩ per channel.
    emission: Vec3f,
    /// ∫ L cosθ dΩ / ∫ L dΩ in luminance, θ measured from the key's centre: the
    /// share of the emission a surface facing the key receives. 1 for a key that
    /// draws nothing.
    facing: f32,
    /// The cosine of the half-angle of the cone around the key's centre that holds
    /// the whole reach box, corners and soft edge included.
    cos_cover: f32,
}

/// The key light's whole emission, ∫ L dΩ, exactly as `apply_lights` paints it, with
/// its cosine-weighted share and the cone that covers its reach.
/// - `lights[0]` is the key: its shape, corner, ring, soft edge, hotspot and roll all
///   come in through its own `mask`.
/// - A light the shape core draws (1b: a polygon, B5's scrim) gives its `emit` instead
///   of A3's mask, and a shaped flag dims the key through `ShapedLight::compose`, as
///   `apply_lights` draws both. Its outline lies inside its box, so the grid holds it
///   and the box's corner cone (`cos_cover`) covers it.
/// - A key with a picture (1b, B14) weighs each point by its drawing there, the
///   picture's colour and alpha included (D1, L:123), not by its shape alone.
/// - Every Multiply light after it scales it as it does in the map (its picture,
///   cut-out alpha and all), and so do the two Add lights that hide what is behind
///   them: a scrim's cloth (B5, `ShapedLight::hide`) and a window filled with sky,
///   whose glass shows the sky in its place ([`dimmers`]). Any other Add light after
///   it is a light of its own, and a light before it touches only what came before.
/// - Every shadow caster after it (redesign §5, `casters` in step with
///   `lights`) scales it as the map draws the caster (`draw_caster`), its
///   softness raster included; one before it does not touch it.
/// - What hangs in front of each of those (`fx`, in step with `lights`) scales what it
///   does, as in the map: a flag with a gobo dims the key only behind the gobo's
///   opaque parts (B8).
///
/// A midpoint grid of n x n points over the key's reach box in its own tangent plane,
/// where dΩ = dx dy / (1 + x² + y²)^(3/2) and the cosine of the angle from the centre
/// is 1 / sqrt(1 + x² + y²). Outside that box the mask is exactly zero, so nothing is
/// missed, and the farthest point of the box is its corner: the covering cone
/// (`PreparedLight::box_cover`).
fn key_emission(lights: &[PreparedLight], fx: &[LightFx], casters: &[Option<CasterLayer>], n: usize) -> KeyIntegral {
    let Some((key, after)) = lights.split_first() else {
        return KeyIntegral { emission: Vec3f::default(), facing: 1.0, cos_cover: 1.0 };
    };
    let flags = dimmers(after, fx.get(1..).unwrap_or(&[]), casters.get(1..).unwrap_or(&[]));
    // A picture gives the key its colour point by point: its drawing over black,
    // colour and all, is the weight, and the sum is not scaled by the colour again.
    let pictured = key.image.is_some();
    let n = n.max(1);
    let cell = vec2f(2.0 * key.reach.x / n as f32, 2.0 * key.reach.y / n as f32);
    // Plan P1: a 3D key's grid lies on its face, where dΩ = |n.c| dx dy / |q|³
    // (q the face's point, c its middle's direction) and the cosine from the
    // axis is q.c / |q|; its face reaches as far as it does, no plane limit.
    let face = key.frame.face.map(|normal| normal.dot(key.frame.center).abs());
    let mut sum = [0.0f64; 3];
    let mut weighted = [0.0f64; 3];
    for j in 0..n {
        let y = -key.reach.y + (j as f32 + 0.5) * cell.y;
        let mut row = Vec3f::default();
        let mut row_weighted = Vec3f::default();
        for i in 0..n {
            let x = -key.reach.x + (i as f32 + 0.5) * cell.x;
            // `project`'s limit: past a dot of 0.05 with the centre nothing is drawn,
            // and the dot here is 1 / sqrt(r2). A 3D face's r2 is |q|².
            let r2 = match face {
                None => 1.0 + x * x + y * y,
                Some(_) => (key.frame.center + key.frame.right * x + key.frame.up * y).length_squared(),
            };
            if face.is_none() && !(r2 <= 400.0) {
                continue;
            }
            let dir = || (key.frame.center + key.frame.right * x + key.frame.up * y).normalize();
            // The weight `apply_lights` gives the key's colour here: A3's mask, the
            // shape core's emission for a light it draws (1b: a polygon, B5's scrim),
            // or the whole drawing of a key with a picture.
            let (m, tint) = if pictured {
                let drawn = key.draw(dir(), Vec3f::default(), false);
                (if drawn.max_elem() > 0.0 { 1.0 } else { 0.0 }, drawn)
            } else {
                let m = match &key.shaped {
                    Some(shaped) => shaped.sample(dir()).emit,
                    None => key.mask(vec2f(x, y), key.inset),
                };
                (m, Vec3f::all(1.0))
            };
            // A 3D key shines toward the camera through its beam (its drawing has it already).
            let m = if face.is_some() && !pictured { m * key.beam(dir()) } else { m };
            if !(m > 0.0) {
                continue;
            }
            let (cos, w) = match face {
                None => {
                    let cos = 1.0 / r2.sqrt();
                    (cos, m * cos / r2)
                }
                Some(nc) => {
                    let len = r2.sqrt();
                    let q = key.frame.center + key.frame.right * x + key.frame.up * y;
                    // A turned face can reach past 90 degrees from its middle's
                    // direction; that part faces nothing along the axis (0, not less).
                    ((q.dot(key.frame.center) / len).max(0.0), m * nc / (r2 * len))
                }
            };
            let mut w = Vec3f::all(w) * tint;
            if !flags.is_empty() {
                w = dimmed(&flags, dir(), w);
            }
            row += w;
            row_weighted += w * cos;
        }
        sum[0] += row.x as f64;
        sum[1] += row.y as f64;
        sum[2] += row.z as f64;
        weighted[0] += row_weighted.x as f64;
        weighted[1] += row_weighted.y as f64;
        weighted[2] += row_weighted.z as f64;
    }
    let area = (cell.x * cell.y) as f64;
    let color = if pictured { Vec3f::all(1.0) } else { key.color };
    let scaled = |s: [f64; 3]| color * vec3f((s[0] * area) as f32, (s[1] * area) as f32, (s[2] * area) as f32);
    let emission = scaled(sum);
    // Luminance weighs the channels as the eye does: a coloured gel's key still
    // gets one share.
    let (total, cosine) = (luminance(emission), luminance(scaled(weighted)));
    let facing = if total > 0.0 { (cosine / total).clamp(0.0, 1.0) } else { 1.0 };
    KeyIntegral { emission, facing, cos_cover: key.box_cover() }
}

/// One entry of the lights listed after the key that dims it, with what
/// hangs in front of it and, for a shadow caster, its own layer.
type Dimmer<'a> = (&'a PreparedLight, &'a LightFx, Option<&'a CasterLayer>);

/// What dims the key as the map draws it, of the lights listed after it:
/// the flags and gels, the shadow casters, the scrims' cloth and the windows
/// filled with sky in front of it (a window's glass shows the sky in place of
/// what is behind it), each with what hangs in front of it (`fx`, in step
/// with `after`, and `casters`).
fn dimmers<'a>(after: &'a [PreparedLight], fx: &'a [LightFx], casters: &'a [Option<CasterLayer>]) -> Vec<Dimmer<'a>> {
    after
        .iter()
        .zip(fx)
        .zip(casters)
        .filter(|((l, fx), caster)| dims(l, fx, caster.as_ref()))
        .map(|((l, fx), caster)| (l, fx, caster.as_ref()))
        .collect()
}

/// Whether a light listed after another dims it ([`dimmers`]).
fn dims(l: &PreparedLight, fx: &LightFx, caster: Option<&CasterLayer>) -> bool {
    caster.is_some() || l.blend == Blend::Multiply || l.shaped.as_ref().is_some_and(|s| s.hides_behind()) || fx.fills_with_sky()
}

/// `w`, the key's light along `dir`, as the lights in `flags` ([`dimmers`])
/// leave it in the map.
fn dimmed(flags: &[Dimmer], dir: Vec3f, w: Vec3f) -> Vec3f {
    dimmed_by(flags.iter().copied(), dir, w)
}

/// [`dimmed`] by flags handed one by one.
fn dimmed_by<'a>(flags: impl Iterator<Item = Dimmer<'a>>, dir: Vec3f, w: Vec3f) -> Vec3f {
    let mut w = w;
    for (flag, fx, caster) in flags {
        if let Some(caster) = caster {
            // As `apply_lights_sky` draws a caster.
            w = draw_caster(flag, fx, caster, dir, w);
            continue;
        }
        if fx.fills_with_sky() {
            // As `apply_lights_sky` draws a window: what is behind its glass
            // (its shape's cover, past its gobo's bars) gives way to the sky,
            // and against the sky its frame and bars hide it too, none of it
            // the key's light, so the key keeps 1 - what it hides.
            let cover = flag.cover(dir);
            if cover > 0.0 {
                w = w * (1.0 - fx.hides(dir, cover));
            }
            continue;
        }
        let before = w;
        // As `apply_lights` draws the flag (`PreparedLight::draw`): its shape,
        // its picture and the picture's alpha, and a scrim's cloth without its
        // own glow (the scrim left out, so only what it hides is drawn).
        w = flag.draw(dir, w, flag.blend != Blend::Multiply);
        // Then what hangs in front of it scales what it did, as
        // `apply_lights_sky` does: a flag's gobo leaves the key through its
        // open parts (B8).
        if !fx.is_plain() {
            w = fx.apply(dir, before, w);
        }
    }
    w
}

/// A shadow caster (redesign §5) drawn over `acc` along `dir`, in the map
/// (`apply_lights_sky`) and over the key (`dimmed`) alike: one rule, so the
/// key loses exactly what the map loses (D1).
/// - Plain (no raster): exactly its flag (`caster::caster_flag`), through
///   the flag's own code, its pattern after it (`LightFx::apply`).
/// - Otherwise: `acc * (1 + s * (tint - 1) * m)`, with m its coverage
///   (`CasterLayer::coverage`: softness, and R5's bloom).
fn draw_caster(light: &PreparedLight, fx: &LightFx, caster: &CasterLayer, dir: Vec3f, acc: Vec3f) -> Vec3f {
    if caster.is_plain() {
        let after = light.draw(dir, acc, false);
        return if fx.is_plain() { after } else { fx.apply(dir, acc, after) };
    }
    caster.transmit(acc, caster.coverage(dir, &|d| caster_sharp_coverage(light, fx, d)))
}

/// The share 0..=1 of a caster's flag that covers `dir`, its box under its
/// pattern, whatever its strength and tint: what a black flag of it takes
/// out of white.
/// R5: also what the bloom pre-pass reads (`Studio::caster_sharp`).
fn caster_sharp_coverage(light: &PreparedLight, fx: &LightFx, dir: Vec3f) -> f32 {
    let white = Vec3f::all(1.0);
    let after = light.over(dir, white, Vec3f::default());
    let left = if fx.is_plain() { after } else { fx.apply(dir, white, after) };
    (1.0 - left.x).max(0.0).min(1.0)
}

/// For [`over_cone`] over the covering cone (`cos_cover`) of the stroke whose
/// centre line is `path`: how many samples per axis the cell round a direction
/// takes. Where the stroke may draw (its path widened by a cell's reach finds
/// the cell, its glow included), enough for STROKE_SAMPLES_PER_REACH of them
/// per `band`, the reach of its band without the glow
/// (`ShapedLight::stroke_band_reach`), so a glow does not thin the samples on
/// the band; elsewhere none.
fn stroke_cells(path: &ArcPath, band: f32, cos_cover: f32) -> impl Fn(Vec3f) -> usize {
    let side = cone_cell(cos_cover);
    let fine = ((side * STROKE_SAMPLES_PER_REACH / band.min(path.reach())).ceil() as usize).clamp(1, STROKE_FINE_MAX);
    let scan = path.widened(side * std::f32::consts::FRAC_1_SQRT_2);
    move |centre: Vec3f| if scan.nearest(centre).dist < scan.reach() { fine } else { 0 }
}

/// The side of one of [`over_cone`]'s coarse cells over the cone of
/// `cos_cover`, in radians. Every direction in a cell lies within
/// side / sqrt(2) of its centre.
fn cone_cell(cos_cover: f32) -> f32 {
    2.0 * cos_cover.clamp(-1.0, 1.0).acos() / CONE_CELLS as f32
}

/// ∫ light(d) dΩ over the cone of `cos_cover` round `axis`, which must hold
/// everything `light` draws, with its cosine-weighted share about `axis`: a
/// key as the map draws it, for the keys that do not lie in a box of their
/// own frame (a stroke, which its points place) or whose light is not their
/// colour (a window filled with sky).
///
/// The cone is laid flat by the azimuthal equidistant projection round
/// `axis`, (u, v) = θ (cos φ, sin φ) with θ the angle from it, so its cells
/// are even in angle however wide it is (a tangent plane stretches them past
/// 60 degrees and ends at 90), and dΩ = sin θ / θ du dv. A grid of
/// CONE_CELLS x CONE_CELLS cells ([`cone_cell`]) covers the square round it;
/// `cells(centre)` says how many jittered samples along each axis a cell
/// takes: 0 where nothing is drawn within side / sqrt(2) of its centre, which
/// holds the whole cell (the projection never stretches an angle). So a thin
/// stroke is sampled finely along its band and nowhere else. Over
/// CONE_BUDGET samples in all, every cell takes fewer, in proportion.
fn over_cone(axis: Vec3f, cos_cover: f32, cells: &dyn Fn(Vec3f) -> usize, light: &dyn Fn(Vec3f) -> Vec3f) -> KeyIntegral {
    let grid = ConeGrid::new(axis, cos_cover, cells, CONE_BUDGET);
    let (mut sum, mut weighted) = ([0.0f64; 3], [0.0f64; 3]);
    for &cell in &grid.visited {
        let (mut cell_sum, mut cell_weighted) = (Vec3f::default(), Vec3f::default());
        let step = grid.samples(cell, &mut |d, theta| {
            let c = light(d);
            if !(c.max_elem() > 0.0) {
                return;
            }
            let w = if theta > 1.0e-6 { c * (theta.sin() / theta) } else { c };
            cell_sum += w;
            cell_weighted += w * theta.cos();
        });
        let area = (step * step) as f64;
        for (s, c) in sum.iter_mut().zip([cell_sum.x, cell_sum.y, cell_sum.z]) {
            *s += c as f64 * area;
        }
        for (s, c) in weighted.iter_mut().zip([cell_weighted.x, cell_weighted.y, cell_weighted.z]) {
            *s += c as f64 * area;
        }
    }
    let emission = vec3f(sum[0] as f32, sum[1] as f32, sum[2] as f32);
    let cosine = luminance(vec3f(weighted[0] as f32, weighted[1] as f32, weighted[2] as f32));
    let total = luminance(emission);
    let facing = if total > 0.0 { (cosine / total).clamp(0.0, 1.0) } else { 1.0 };
    KeyIntegral { emission, facing, cos_cover }
}

/// [`over_cone`]'s grid over the cone of `cos_cover` round `axis`, laid
/// flat by the azimuthal equidistant projection round it: the cells that
/// `cells` asks samples of, each with its samples per axis, thinned in
/// proportion past `budget` in all (`ConeGrid::new`). The key's integral
/// (CONE_BUDGET) and a stroke's room lights (`over_stroke_pieces`,
/// ROOM_PIECE_SAMPLES) walk it alike, the room's the same samples as the
/// key's, only fewer of them.
struct ConeGrid {
    frame: LightFrame,
    /// The cone's half-angle, radians.
    reach: f32,
    /// A coarse cell's side, radians ([`cone_cell`]).
    side: f32,
    /// (column, row, samples per axis) of every cell that takes samples.
    visited: Vec<(usize, usize, usize)>,
}

impl ConeGrid {
    fn new(axis: Vec3f, cos_cover: f32, cells: &dyn Fn(Vec3f) -> usize, budget: usize) -> ConeGrid {
        let (azimuth_deg, elevation_deg) = az_el_from_dir(axis);
        let mut grid = ConeGrid { frame: light_frame(azimuth_deg, elevation_deg, 0.0), reach: cos_cover.clamp(-1.0, 1.0).acos(), side: cone_cell(cos_cover), visited: Vec::new() };
        let mut total = 0usize;
        for j in 0..CONE_CELLS {
            for i in 0..CONE_CELLS {
                let (u, v) = (grid.corner(i) + 0.5 * grid.side, grid.corner(j) + 0.5 * grid.side);
                // A cell wholly outside the cone holds nothing.
                if (u * u + v * v).sqrt() - grid.side * std::f32::consts::FRAC_1_SQRT_2 > grid.reach {
                    continue;
                }
                let m = cells(grid.at(u, v)).min(CONE_FINE_MAX);
                if m > 0 {
                    grid.visited.push((i, j, m));
                    total += m * m;
                }
            }
        }
        let thin = if total > budget { (budget as f32 / total as f32).sqrt() } else { 1.0 };
        for cell in &mut grid.visited {
            cell.2 = ((cell.2 as f32 * thin) as usize).max(1);
        }
        grid
    }

    /// Where the flat grid's (u, v) lies on the sphere.
    fn at(&self, u: f32, v: f32) -> Vec3f {
        let theta = (u * u + v * v).sqrt();
        if !(theta > 1.0e-9) {
            return self.frame.center;
        }
        let (sin, cos) = theta.sin_cos();
        self.frame.center * cos + (self.frame.right * u + self.frame.up * v) * (sin / theta)
    }

    /// The low edge of column or row `k`.
    fn corner(&self, k: usize) -> f32 {
        -self.reach + k as f32 * self.side
    }

    /// The side of one of `cell`'s samples' squares in the flat grid,
    /// radians: a sample's dΩ is its square times sin θ / θ.
    fn step(&self, cell: (usize, usize, usize)) -> f32 {
        self.side / cell.2 as f32
    }

    /// Hands `visit` each jittered sample of `cell` inside the cone, its
    /// direction and its angle from the axis, in a fixed order; returns
    /// the cell's `step`.
    fn samples(&self, cell: (usize, usize, usize), visit: &mut dyn FnMut(Vec3f, f32)) -> f32 {
        let (i, j, m) = cell;
        let step = self.step(cell);
        for b in 0..m {
            for a in 0..m {
                // Every sample of the grid has its own place in the hash.
                let (hx, hy) = ((i * CONE_FINE_MAX + a) as i32, (j * CONE_FINE_MAX + b) as i32);
                let u = self.corner(i) + (a as f32 + hash01(hx, hy, 0, CONE_JITTER_SEED)) * step;
                let v = self.corner(j) + (b as f32 + hash01(hx, hy, 1, CONE_JITTER_SEED)) * step;
                let theta = (u * u + v * v).sqrt();
                // Nothing is drawn past the cone.
                if !(theta <= self.reach) {
                    continue;
                }
                visit(self.at(u, v), theta);
            }
        }
        step
    }
}

/// The longest piece of a light that lights a room as one emitter
/// (`Studio::build_room`), degrees of arc: a 24 m LED line under a 5.5 m
/// ceiling, 144 degrees, stands as 15.
const ROOM_PIECE_DEG: f32 = 10.0;
/// The most emitters a room's lights stand as, all pieces together. Every
/// room texel's irradiance sums one per piece (twice on a glossy floor, and
/// once more for each stand or grid pipe over it), so past it the pieces
/// grow longer instead (`room_piece_arc`): sixteen 144 degree tubes stand
/// as about 80 pieces of 29 degrees, where four stand as 60 of 10.
const ROOM_PIECES: usize = 96;
/// The most samples of a stroke's band its room pieces take
/// ([`over_stroke_pieces`]): the room needs the stroke's light where it
/// lies, not the key's last digit (CONE_BUDGET is 16 times as many).
const ROOM_PIECE_SAMPLES: usize = 1 << 14;
/// Samples along a box light's long side per room piece
/// (`Studio::box_room_lights`); never fewer than its 24 across.
const BOX_PIECE_SAMPLES: usize = 6;
/// How far out a box light's tangent plane is sampled for the room: tan of
/// its angle from the centre (about 87 degrees).
const BOX_FAR_TAN: f32 = 20.0;
/// Sub-cells a side of each room piece of a pattern light, each taken under
/// the flags after it at its own point (`Studio::pattern_room_lights`).
const PATTERN_ROOM_SUB: usize = 4;
/// Cells a side of the grid a pattern key's flags are taken over (`pattern_key_emission`).
const PATTERN_KEY_CELLS: usize = 64;

/// The angles a 3D face spans as the camera sees it, across (along its
/// `right`) and down (along its `up`), radians: between the middles of its
/// two opposite edges.
fn face_spans(frame: &LightFrame, half: Vec2f) -> (f32, f32) {
    let span = |along: Vec3f| {
        let (a, b) = ((frame.center + along).normalize(), (frame.center - along).normalize());
        a.dot(b).max(-1.0).min(1.0).acos()
    };
    (span(frame.right * half.x), span(frame.up * half.y))
}

/// Plan P2: the key's emission when it is a pattern light, `key_emission`'s
/// integral summed from its picture: each lit texel of the pattern's raster
/// (level 0), at dΩ = |n.c| dA / |q|³ through its beam toward the camera (q
/// the texel's point of the face, c its middle's direction), its cosine from
/// the axis q.c / |q|; the flags after it taken over a grid of
/// PATTERN_KEY_CELLS² cells at their middles (a flag scales what passes it,
/// so a cell's sum passes as its middle's does). Exact for the picture,
/// whatever its tubes' width against the map's texel. Cost: one pass over
/// the raster's lit texels and the flags at up to PATTERN_KEY_CELLS² points.
fn pattern_key_emission(lights: &[PreparedLight], fx: &[LightFx], casters: &[Option<CasterLayer>], pattern: &PatternLight) -> KeyIntegral {
    let Some((key, after)) = lights.split_first() else {
        return KeyIntegral { emission: Vec3f::default(), facing: 1.0, cos_cover: 1.0 };
    };
    let flags = dimmers(after, fx.get(1..).unwrap_or(&[]), casters.get(1..).unwrap_or(&[]));
    let (frame, half) = pattern.face();
    let nc = frame.face.map_or(1.0, |n| n.dot(frame.center).abs());
    let area = pattern.texel_area();
    let n = PATTERN_KEY_CELLS;
    let sign = box_x_sign();
    let mut cells = vec![(Vec3f::default(), Vec3f::default()); n * n];
    pattern.for_each_texel(|p, rgb| {
        let q = frame.center + frame.right * p.x + frame.up * p.y;
        let len = q.length();
        if !(len > 0.0) {
            return;
        }
        let w = nc * area / (len * len * len) * key.beam(q / len);
        if !(w > 0.0) {
            return;
        }
        let cos = (q.dot(frame.center) / len).max(0.0);
        let cx = (((p.x * sign + half.x) / (2.0 * half.x) * n as f32).max(0.0) as usize).min(n - 1);
        let cy = (((half.y - p.y) / (2.0 * half.y) * n as f32).max(0.0) as usize).min(n - 1);
        let cell = &mut cells[cy * n + cx];
        cell.0 += rgb * w;
        cell.1 += rgb * (w * cos);
    });
    let (mut sum, mut weighted) = ([0.0f64; 3], [0.0f64; 3]);
    for (k, &(s, c)) in cells.iter().enumerate() {
        if !(s.max_elem() > 0.0) {
            continue;
        }
        let (s, c) = if flags.is_empty() {
            (s, c)
        } else {
            let (i, j) = (k % n, k / n);
            let x = sign * (-half.x + (i as f32 + 0.5) / n as f32 * 2.0 * half.x);
            let y = half.y - (j as f32 + 0.5) / n as f32 * 2.0 * half.y;
            let dir = (frame.center + frame.right * x + frame.up * y).normalize();
            (dimmed(&flags, dir, s), dimmed(&flags, dir, c))
        };
        for (acc, v) in sum.iter_mut().zip([s.x, s.y, s.z]) {
            *acc += v as f64;
        }
        for (acc, v) in weighted.iter_mut().zip([c.x, c.y, c.z]) {
            *acc += v as f64;
        }
    }
    let scaled = |s: [f64; 3]| key.color * vec3f(s[0] as f32, s[1] as f32, s[2] as f32);
    let emission = scaled(sum);
    let (total, cosine) = (luminance(emission), luminance(scaled(weighted)));
    let facing = if total > 0.0 { (cosine / total).clamp(0.0, 1.0) } else { 1.0 };
    KeyIntegral { emission, facing, cos_cover: key.box_cover() }
}

/// The length of every room piece, radians, for lights as long as
/// `lengths` (radians; 0 for one that stands whole): ROOM_PIECE_DEG, or
/// longer, so all of them together stand as at most ROOM_PIECES
/// ([`pieces_over`] rounds each light's count up, and each gives one at
/// least).
fn room_piece_arc(lengths: &[f32]) -> f32 {
    let total: f32 = lengths.iter().filter(|l| l.is_finite() && **l > 0.0).sum();
    let left = ROOM_PIECES.saturating_sub(lengths.len()).max(1);
    ROOM_PIECE_DEG.to_radians().max(total / left as f32)
}

/// The light-weighted mean distance from the camera of `pieces`, metres
/// (3 m for none): the sphere the mirror looks a light up on when it has no
/// plane of its own.
fn mean_distance<'a>(pieces: impl Iterator<Item = &'a RoomLight>) -> f32 {
    let (mut sum, mut weight) = (0.0f32, 0.0f32);
    for l in pieces {
        let k = luminance(l.intensity).max(1.0e-9);
        sum += l.pos.length() * k;
        weight += k;
    }
    if weight > 0.0 { sum / weight } else { 3.0 }
}

/// `count` pieces as a grid over a face spanning `across` by `down`
/// (radians), its cells about as long each way: (columns, rows).
fn grid_of(count: usize, across: f32, down: f32) -> (usize, usize) {
    let ratio = if down > 1.0e-6 { (across / down).max(1.0e-3).min(1.0e3) } else { 1.0e3 };
    let nx = (((count as f32) * ratio).sqrt().round() as usize).max(1).min(count);
    (nx, (count / nx).max(1))
}

/// How many even pieces of at most `piece_arc` a light `length` long
/// (radians) stands as in the room: one at least.
fn pieces_over(length: f32, piece_arc: f32) -> usize {
    if !(length > piece_arc) {
        return 1;
    }
    ((length / piece_arc).ceil() as usize).max(1)
}

/// What the map draws of a stroke nearest one piece of its centre line
/// (`over_stroke_pieces`).
#[derive(Clone, Copy, Debug, Default)]
struct StrokePiece {
    /// ∫ light dΩ, per channel.
    emission: [f64; 3],
    /// ∫ dΩ where it draws any light.
    solid: f64,
    /// ∫ d luminance dΩ: its directions weighted by its light, along its mean direction.
    toward: [f64; 3],
}

impl StrokePiece {
    fn add(&mut self, other: &StrokePiece) {
        for k in 0..3 {
            self.emission[k] += other.emission[k];
            self.toward[k] += other.toward[k];
        }
        self.solid += other.solid;
    }
}

/// ∫ light(d) dΩ of a stroke over the cone of `cos_cover` round `axis`,
/// which holds everything it draws, sampled where its band runs
/// ([`stroke_cells`] of `path`, its centre line) as the key's integral is
/// ([`over_cone`]) but over ROOM_PIECE_SAMPLES at most, each sample handed
/// to the piece of `pieces` even pieces of the centre line (by arc length)
/// that lies nearest it: a tight bend's band, drawn once, counts once.
/// Every sample's light goes to one piece, so the pieces together hold the
/// stroke's whole emission. The grid's cells are spread by `run` (through
/// `caster::rows_by`, `cancelled` polled between its calls) and summed in
/// their order, so any runner gives the same pieces. None when `cancelled`
/// stopped it or the runner skipped a cell.
fn over_stroke_pieces(
    axis: Vec3f,
    cos_cover: f32,
    path: &ArcPath,
    band: f32,
    pieces: usize,
    light: &(dyn Fn(Vec3f) -> Vec3f + Sync),
    cancelled: &dyn Fn() -> bool,
    run: RowRunner,
) -> Option<Vec<StrokePiece>> {
    let pieces = pieces.max(1);
    let grid = ConeGrid::new(axis, cos_cover, &stroke_cells(path, band, cos_cover), ROOM_PIECE_SAMPLES);
    // Each cell's light, by the pieces it reaches (a cell touches one or two).
    let cells = rows_by(grid.visited.len(), run, cancelled, &|c| {
        let cell = grid.visited[c];
        let step = grid.step(cell);
        let area = (step * step) as f64;
        let mut touched: Vec<(usize, StrokePiece)> = Vec::new();
        grid.samples(cell, &mut |d, theta| {
            let c = light(d);
            if !(c.max_elem() > 0.0) {
                return;
            }
            // dΩ of this sample.
            let w = area * if theta > 1.0e-6 { (theta.sin() / theta) as f64 } else { 1.0 };
            let k = ((path.nearest(d).t * pieces as f32) as usize).min(pieces - 1);
            let at = match touched.iter().position(|(j, _)| *j == k) {
                Some(at) => at,
                None => {
                    touched.push((k, StrokePiece::default()));
                    touched.len() - 1
                }
            };
            let piece = &mut touched[at].1;
            for (s, c) in piece.emission.iter_mut().zip([c.x, c.y, c.z]) {
                *s += c as f64 * w;
            }
            let lum = luminance(c) as f64;
            if lum > 0.0 {
                piece.solid += w;
                for (s, d) in piece.toward.iter_mut().zip([d.x, d.y, d.z]) {
                    *s += d as f64 * lum * w;
                }
            }
        });
        touched
    })?;
    let mut out = vec![StrokePiece::default(); pieces];
    for touched in &cells {
        for (k, piece) in touched {
            out[*k].add(piece);
        }
    }
    Some(out)
}

/// One enabled light, reduced to what the per-pixel evaluation needs.
#[derive(Clone, Debug)]
struct PreparedLight {
    /// The light's frame; a stroke's is the box round it (`stroke_box`).
    frame: LightFrame,
    shape: LightShape,
    blend: Blend,
    /// Half extents in the tangent plane: tan(width/2), tan(height/2); a
    /// stroke's are its box's.
    half: Vec2f,
    /// Rounded-rect corner radius, in tangent units.
    corner: f32,
    /// The ring's centre line and half thickness, as fractions of the outer radius.
    ring_mid: f32,
    ring_half: f32,
    /// Width of the soft edge, in tangent units.
    band: f32,
    /// How far a picture's Penumbra widens the soft edge inward, tangent
    /// units (P1, stretch and fill; `LightImage::edge_inset` x the depth):
    /// the edge ramps from half the band plus this inside to half the band
    /// outside. 0 without a picture; a shaped light keeps its own.
    inset: f32,
    /// The depth of a disc that is not a circle (its short half axis),
    /// against which `deep` measures how deep a ramp starts (P2, `mask`).
    /// 0 for a circle, whose first-order distance is exact, and for every
    /// other shape: their edges never read the exact ellipse distance.
    long_disc_depth: f32,
    /// Outside this box (the half extents plus the edge) the light is exactly zero.
    reach: Vec2f,
    /// Exponent scale of the Gaussian centre falloff (HOTSPOT_FALLOFF x hotspot).
    hotspot: f32,
    /// The key cone's cosine when this light is the key in its own box: the
    /// cone over the larger of width and height.
    key_cos: f32,
    color: Vec3f,
    /// Polygons, strokes and scrims (1b): the shape core draws the light.
    shaped: Option<ShapedLight>,
    /// A picture as the light's content (1b); None draws the plain colour.
    image: Option<LightImage>,
    /// Plan P1: a light placed in 3D, its frame a face of its own
    /// (`LightFrame::face`); None for a 2D light.
    space: Option<InSpace>,
}

/// What a light placed in 3D (plan P1) adds to its prepared light.
#[derive(Clone, Copy, Debug)]
struct InSpace {
    /// Its beam's exponent (`space::beam_exponent`): 0 for a diffuse face.
    beam: f32,
    /// How far its middle is from the camera, metres: its frame's plane is its face over this.
    distance: f32,
}

impl PreparedLight {
    fn new(l: &LightParams, images: &ImageSet) -> PreparedLight {
        PreparedLight::new_for(l, images, CASTER_REFERENCE_WIDTH)
    }

    /// `new` for a map `width` wide: a pattern light (plan P2) rasters its
    /// pattern at the resolution that map needs.
    fn new_for(l: &LightParams, images: &ImageSet, width: usize) -> PreparedLight {
        // The light as clamp() holds it (D17): its box, inner radius,
        // softness, corner and hotspot are read as they are.
        let l = &l.clamped();
        // A stroke's points place it, not its frame: its picture is laid in
        // the box round the stroke itself (D3, L:119), as its gobo, aim and
        // wear are (`light_fx::face_axes`). Nothing else of a stroke reads its
        // frame or its box: the shape core draws it and its key is measured
        // round its own cones.
        let (frame, half) = match stroke_box(l) {
            Some(stroke) => stroke,
            None => {
                let (hx, hy) = half_extents(l);
                (face_frame(l), vec2f(hx, hy))
            }
        };
        let space = l.in_space().map(|s| InSpace { beam: beam_exponent(s.beam), distance: s.distance().max(super::SPACE_NEAR_M) });
        let short = half.x.min(half.y);
        let inner = l.inner;
        let ring_half = 0.5 * (1.0 - inner);
        let shape = l.shape();
        // The deepest point inside the shape. Softness 1 ramps the edge right to it,
        // so "fully soft" means the same thing for a wide softbox and a thin ring.
        let inradius = match shape {
            LightShape::Ring => ring_half * short,
            // The shape core draws polygons, strokes and scrims (shaped.rs, B2-B5).
            // `short` still sizes the band, and so the reach box key_emission
            // integrates a polygon or scrim key over, which must hold its soft
            // edge (B2, B5). A stroke never reads it: its points place it.
            LightShape::Rect | LightShape::Disc | LightShape::Polygon | LightShape::Stroke | LightShape::Scrim | LightShape::Paint | LightShape::Pattern => short,
        };
        let band = (l.softness * inradius).max(MIN_EDGE);
        let shaped = ShapedLight::new_for(l, images, width);
        // P1: a picture that covers the light (stretch, fill) has the light's
        // edge for its border, and its Penumbra widens the light's soft edge
        // inward by that share of the depth the Softness takes (a shaped
        // light's own, `ShapedLight::depth`). A picture in fit fades inside
        // its own border (`LightImage`). Its Mask softness (P2) is the
        // picture's own, in its texels, whatever the light.
        let depth = shaped.as_ref().map_or(inradius, ShapedLight::depth);
        let image = l.image.as_ref().and_then(|image| LightImage::new(image, images)).map(|image| image.with_depth(depth));
        let inset = image.as_ref().map_or(0.0, LightImage::edge_inset);
        PreparedLight {
            frame,
            shape,
            blend: l.blend(),
            half,
            corner: l.corner * short,
            ring_mid: 0.5 * (1.0 + inner),
            ring_half,
            band,
            inset: inset * inradius,
            long_disc_depth: match shape {
                LightShape::Disc if !is_circle(half) => short,
                _ => 0.0,
            },
            reach: vec2f(half.x + band, half.y + band),
            hotspot: HOTSPOT_FALLOFF * l.hotspot,
            key_cos: match space {
                None => (0.5 * l.width_deg.max(l.height_deg)).to_radians().cos(),
                // A 3D face's cone is the one it spans facing the camera, as a 2D light's,
                // and no narrower than a 2D light's narrowest (width_deg 0.1): a 2 cm key at
                // 60 m spans a cosine f32 rounds to 1, a cone of no solid angle, and
                // `EnvSun::cone_radiance` would hand the engine no light for it.
                Some(_) => half.x.max(half.y).atan().max(0.05f32.to_radians()).cos(),
            },
            color: held_light_color(l),
            shaped: shaped.map(|shaped| shaped.with_inset(inset)),
            image,
            space,
        }
    }

    /// Plan P1: the share of a 3D light's radiance that leaves its face
    /// toward the camera along `dir` (`space::beam_factor`, the angle between
    /// its front and the way back to the camera from where `dir` meets it): 0
    /// from behind. 1 for a 2D light.
    fn beam(&self, dir: Vec3f) -> f32 {
        match (self.space, self.frame.face) {
            (Some(space), Some(n)) => beam_factor(space.beam, dir.dot(n)),
            _ => 1.0,
        }
    }

    /// A 3D light's beam (`beam`) on what its model's cloth sends along `dir`
    /// (light_model.rs): a diffuse face's (exponent 0) is 1 wherever the
    /// camera sees the cloth's inside, past its rim's plane too; a narrower
    /// one's falls with the angle off the face's axis, held just short of 0.
    fn model_beam(&self, dir: Vec3f) -> f32 {
        match (self.space, self.frame.face) {
            (Some(space), Some(n)) if space.beam > 0.0 => beam_factor(space.beam, dir.dot(n).max(1.0e-3)),
            _ => 1.0,
        }
    }

    /// The colour this light draws in along `dir`: an emitter's through its
    /// beam (`beam`, a 3D light's), else its colour.
    fn color_toward(&self, dir: Vec3f) -> Vec3f {
        if self.space.is_some() && self.blend == Blend::Add {
            self.color * self.beam(dir)
        } else {
            self.color
        }
    }

    /// This light drawn over `acc` as the map draws it: its shape, content and
    /// blend, and a picture faded in by its alpha. `left_out` draws it as the
    /// key left out of the map (`skip`, N3): it loses its own light, while
    /// whatever it hides stays hidden (B5's scrim, `hide`); for a light that
    /// hides nothing (an A3 light, a polygon, a stroke, a picture) that is the
    /// same as skipping it.
    fn draw(&self, dir: Vec3f, acc: Vec3f, left_out: bool) -> Vec3f {
        self.draw_full(dir, acc, left_out, true, 0.0)
    }

    /// `draw` for a map texel `texel` radians tall, which a pattern (plan P2)
    /// filters its picture over (0: the width it was built for).
    fn draw_at(&self, dir: Vec3f, acc: Vec3f, left_out: bool, texel: f32) -> Vec3f {
        self.draw_full(dir, acc, left_out, true, texel)
    }

    /// `draw`; `beamed` false draws a 3D light's face at its full radiance,
    /// whichever way it looks (what it puts into a room, `box_room_lights_3d`).
    fn draw_beamed(&self, dir: Vec3f, acc: Vec3f, left_out: bool, beamed: bool) -> Vec3f {
        self.draw_full(dir, acc, left_out, beamed, 0.0)
    }

    /// `draw_beamed` for a map texel `texel` radians tall (`draw_at`).
    fn draw_full(&self, dir: Vec3f, acc: Vec3f, left_out: bool, beamed: bool, texel: f32) -> Vec3f {
        let color = if beamed { self.color_toward(dir) } else { self.color };
        let Some(image) = &self.image else {
            return if left_out { self.hide(dir, acc) } else { self.over_at(dir, acc, color, texel) };
        };
        // The picture is only looked up where the light can reach. Held at the
        // plane's limit: only a stroke reaches past it, and there its picture's
        // edge goes on (a light in its own box draws nothing so far out).
        let Some(p) = project_held(&self.frame, dir) else { return acc };
        if !self.reaches(dir, p) {
            return acc;
        }
        let (texel, alpha) = image.shade(p, self.half);
        if !(alpha > 0.0) {
            return acc;
        }
        let lit = if left_out { self.hide(dir, acc) } else { self.over(dir, acc, color * texel) };
        // Alpha fades the whole light in. For Add and Multiply alike that
        // is a mix toward what the light makes of the pixel.
        if alpha >= 1.0 { lit } else { acc + (lit - acc) * alpha }
    }

    /// How much of this light covers `dir`, 0..=1, whatever its colour: the
    /// glass of a window filled with sky. A plain light drawn in white over
    /// black is its own coverage (shape, soft edge and hotspot). A shaped
    /// light is asked directly: a stroke with a ramp carries its own colour,
    /// so drawing it in white would not give its coverage. Its bare shape:
    /// a picture's Penumbra is the picture's, as its alpha is.
    fn cover(&self, dir: Vec3f) -> f32 {
        match &self.shaped {
            Some(shaped) if shaped.cull().contains(dir) => shaped.sample(dir).emit,
            Some(_) => 0.0,
            None => self.over_with(dir, Vec3f::default(), Vec3f::all(1.0), 0.0).x,
        }
    }

    /// D2: the share of a Ring light's own light that a surface facing its
    /// axis receives, ∫ m cosθ dΩ / ∫ m dΩ with θ from the axis and m its
    /// bare shape as the map draws it (`mask`: the band, its soft edge and
    /// hotspot; not a picture, the extras or the lights after it, which only
    /// cut it). 1 for any other light.
    ///
    /// Summed in the ellipse's own polar coordinates, where the band lies
    /// between radii inner and 1 and the soft edge reaches at most half the
    /// band over the short half axis past either side: p = (hx k cos φ,
    /// hy k sin φ), dx dy = hx hy k dk dφ, and in the tangent plane
    /// dΩ = dx dy / (1 + |p|²)^(3/2) and cosθ = 1 / sqrt(1 + |p|²), over a
    /// quarter turn (the mask reads p's squares only). A circle's light
    /// depends on k alone, so one step round suffices; a hard circular ring
    /// between angles a and b from its axis gives exactly (cos a + cos b) / 2.
    fn ring_facing(&self) -> f32 {
        if self.shape != LightShape::Ring {
            return 1.0;
        }
        let short = self.half.x.min(self.half.y);
        let past = 0.5 * self.band / short;
        let (k_lo, k_hi) = ((1.0 - 2.0 * self.ring_half - past).max(0.0), 1.0 + past);
        let turns = if is_circle(self.half) { 1 } else { RING_FACING_TURNS };
        let (dk, dphi) = ((k_hi - k_lo) / RING_FACING_STEPS as f32, std::f32::consts::FRAC_PI_2 / turns as f32);
        let (mut sum, mut weighted) = (0.0f64, 0.0f64);
        for i in 0..RING_FACING_STEPS {
            let k = k_lo + (i as f32 + 0.5) * dk;
            for j in 0..turns {
                let (sin, cos) = ((j as f32 + 0.5) * dphi).sin_cos();
                let p = vec2f(self.half.x * k * cos, self.half.y * k * sin);
                if let Some(n) = self.frame.face {
                    // Plan P1: on a 3D face dΩ = |n.c| dx dy / |q|³ and the
                    // cosine from the axis is q.c / |q|, q the face's point;
                    // its light is drawn through its beam, and the mask reads
                    // p's squares only, so the quarter still holds it all.
                    let quarter = [vec2f(p.x, p.y), vec2f(-p.x, p.y), vec2f(p.x, -p.y), vec2f(-p.x, -p.y)];
                    for p in quarter {
                        let q = self.frame.center + self.frame.right * p.x + self.frame.up * p.y;
                        let len = q.length();
                        let m = self.mask(p, 0.0) * self.beam(q / len);
                        if !(m > 0.0) {
                            continue;
                        }
                        let solid = (m * k * n.dot(self.frame.center).abs() / (len * len * len)) as f64;
                        sum += solid;
                        // Past 90 degrees from the middle's direction a point faces nothing along the axis.
                        weighted += solid * (q.dot(self.frame.center) / len).max(0.0) as f64;
                    }
                    continue;
                }
                // `project`'s limit, as in `key_emission`.
                let r2 = 1.0 + p.x * p.x + p.y * p.y;
                if !(r2 <= 400.0) {
                    continue;
                }
                let m = self.mask(p, 0.0);
                if !(m > 0.0) {
                    continue;
                }
                let solid = (m * k / (r2 * r2.sqrt())) as f64;
                sum += solid;
                weighted += solid / (r2.sqrt() as f64);
            }
        }
        if sum > 0.0 { (weighted / sum) as f32 } else { 1.0 }
    }

    /// The cone that holds this light's reach box, corners and soft edge
    /// included: 1 / sqrt(1 + rx² + ry²), no smaller than `project`'s limit
    /// (`PROJECT_MIN_COS`), past which nothing is drawn.
    ///
    /// Plan P1: a 3D face's is the cone round its middle that holds its
    /// reach box on the face (`LightFrame::cover_cos`), all round when it
    /// reaches 90 degrees out; no plane limit holds it.
    fn box_cover(&self) -> f32 {
        if self.frame.face.is_some() {
            return self.frame.cover_cos(self.reach);
        }
        (1.0 / (1.0 + self.reach.x * self.reach.x + self.reach.y * self.reach.y).sqrt()).max(PROJECT_MIN_COS)
    }

    /// Where this light may draw and how fine its drawing is (`Studio::bright_boxes`): the
    /// cone that holds its reach (`key_cones`), its box (but a stroke's, which its points
    /// place), and its narrowest feature, the shorter side of its box (a ring's band). It has
    /// detail inside that box when a picture, a shape core or what hangs in front of it
    /// (`hung`) draws it.
    fn bright_box(&self, hung: bool) -> BrightBox {
        let (axis, _, cos_cover) = self.key_cones();
        let short = self.half.x.min(self.half.y);
        let feature = match self.shape {
            LightShape::Ring => 2.0 * self.ring_half * short,
            _ => 2.0 * short,
        };
        // A stroke's or a paint layer's points place it, not its box.
        let stroke = self.shaped.as_ref().is_some_and(|s| s.placed_cones().is_some());
        BrightBox {
            axis,
            cos_cover,
            frame_reach: (!stroke).then_some((self.frame, self.reach)),
            feature,
            detailed: hung || self.image.is_some() || self.shaped.is_some(),
        }
    }

    /// The key's cones when this light is the key, `(axis, cos_radius,
    /// cos_cover)`: a stroke's round its centre line, any other light's round
    /// the centre of its box (the larger of width and height, and the box's
    /// corner).
    fn key_cones(&self) -> (Vec3f, f32, f32) {
        match self.shaped.as_ref().and_then(|s| s.placed_cones()) {
            Some(cones) => cones,
            None => (self.frame.center, self.key_cos, self.box_cover().min(self.key_cos)),
        }
    }

    /// This light drawn over `acc` in the colour `color`, its edge widened
    /// by its picture's Penumbra (`inset`; a shaped light's is its own).
    fn over(&self, dir: Vec3f, acc: Vec3f, color: Vec3f) -> Vec3f {
        self.over_with(dir, acc, color, self.inset)
    }

    /// `over` for a map texel `texel` radians tall: a pattern (plan P2)
    /// filters its picture over it; every other light draws as `over`.
    fn over_at(&self, dir: Vec3f, acc: Vec3f, color: Vec3f, texel: f32) -> Vec3f {
        match &self.shaped {
            Some(shaped) if shaped.pattern().is_some() => shaped.compose_at(dir, acc, color, texel),
            _ => self.over(dir, acc, color),
        }
    }

    /// `over` with the soft edge widened inward by `inset` (an A3 light's;
    /// a shaped light draws with its own).
    fn over_with(&self, dir: Vec3f, acc: Vec3f, color: Vec3f, inset: f32) -> Vec3f {
        if let Some(shaped) = &self.shaped {
            return shaped.compose(dir, acc, color);
        }
        let Some(p) = project(&self.frame, dir) else { return acc };
        let m = self.mask(p, inset);
        if !(m > 0.0) {
            return acc;
        }
        match self.blend {
            Blend::Add => acc + color * m,
            Blend::Multiply => acc * (Vec3f::all(1.0) + (color - Vec3f::all(1.0)) * m),
        }
    }

    /// This light over `acc` with its own light left out: what the key leaves
    /// of the map in the engine's keyless lighting copy (plan 2's N3). What it
    /// hides stays hidden (a scrim's cloth, through the shape core); an A3
    /// light adds only, so it leaves `acc`.
    fn hide(&self, dir: Vec3f, acc: Vec3f) -> Vec3f {
        match &self.shaped {
            Some(shaped) => shaped.hide(dir, acc),
            None => acc,
        }
    }

    /// A cheap test that the light can touch `dir`, whose tangent-plane point
    /// is `p`: the cull cone of a shaped light, the reach box of the others.
    fn reaches(&self, dir: Vec3f, p: Vec2f) -> bool {
        match &self.shaped {
            Some(shaped) => shaped.cull().contains(dir),
            None => p.x.abs() <= self.reach.x && p.y.abs() <= self.reach.y,
        }
    }

    /// How far a disc's edge, its ramp widened inward by `inset`, reads the
    /// exact ellipse distance in place of the first-order one (P2, `mask`):
    /// 0 while the ramp starts no deeper than `DISC_EXACT_FROM` of the
    /// disc's depth, rising smoothly to 1 by `DISC_EXACT_BY` of it and
    /// staying 1 deeper. 0 for a circle and every other shape.
    fn deep(&self, inset: f32) -> f32 {
        if self.long_disc_depth > 0.0 {
            smoothstep(DISC_EXACT_FROM, DISC_EXACT_BY, (0.5 * self.band + inset) / self.long_disc_depth)
        } else {
            0.0
        }
    }

    /// Coverage 0..=1 at tangent-plane point `p`: the soft edge times the
    /// hotspot falloff. `inset` widens the edge inward (P1, a picture's
    /// Penumbra): one smoothstep from half the band plus `inset` inside the
    /// outline to half the band outside it, so the light's reach stays.
    fn mask(&self, p: Vec2f, inset: f32) -> f32 {
        // Cheap box cull first. Most of the sphere is far from any light.
        if p.x.abs() > self.reach.x || p.y.abs() > self.reach.y {
            return 0.0;
        }
        // Signed distance to the edge (negative inside) and the squared radial
        // position used for the hotspot (0 at the centre, 1 at the edge).
        let (distance, r2) = match self.shape {
            LightShape::Rect => {
                let nx = p.x / self.half.x;
                let ny = p.y / self.half.y;
                (sd_round_rect(p, self.half, self.corner), nx * nx + ny * ny)
            }
            LightShape::Disc => {
                // The first-order distance (`ellipse_metric`) is (k0 - 1)
                // times the half axis along the way in, so a long disc's
                // centre reads as deep as its long half axis seen along it
                // and as its short one seen across it, and its levels
                // deeper than about half the disc's depth are waisted. P2:
                // a ramp that reached deeper than the disc (Softness / 2 +
                // Penumbra above 1) pinched the centre into a bow-tie, and
                // one that reached past half its depth had a core with a
                // waist. No point of the disc is deeper than its inradius
                // (the short half axis), so the first-order distance is
                // held there (every way into the centre ends at one value),
                // and as the ramp reaches deeper (`deep`) the edge reads the
                // exact distance instead, whose levels are the disc's inner
                // parallels: convex, no waist, the centre alone deepest. A
                // ramp near the edge has `deep` 0: it draws as before, bit
                // for bit. The ramp is this call's: the bare shape (`cover`,
                // no inset) reads the blend its own ramp gives.
                let (k0, scale) = ellipse_metric(p, self.half);
                let first = ((k0 - 1.0) * scale).max(-self.half.x.min(self.half.y));
                let deep = self.deep(inset);
                let distance = if deep > 0.0 { first + (ellipse_distance(p, self.half) - first) * deep } else { first };
                (distance, k0 * k0)
            }
            LightShape::Ring => {
                // abs(r - R) - w, with the radius measured in the ellipse's own units.
                let (k0, scale) = ellipse_metric(p, self.half);
                let off = (k0 - self.ring_mid).abs();
                let across = off / self.ring_half;
                ((off - self.ring_half) * scale, across * across)
            }
            // Drawn by the shape core. Reaching here means its outline could not
            // be built (a stroke with fewer than two points): it covers nothing.
            LightShape::Polygon | LightShape::Stroke | LightShape::Scrim | LightShape::Paint | LightShape::Pattern => return 0.0,
        };
        let edge = 1.0 - smoothstep(-0.5 * self.band - inset, 0.5 * self.band, distance);
        if edge <= 0.0 {
            return 0.0;
        }
        if self.hotspot > 0.0 {
            edge * (-self.hotspot * r2).exp()
        } else {
            edge
        }
    }
}

/// Signed distance to a rect with half extents `half` and corner radius `radius`
/// (iq's rounded box, the knob's `rect_d`). Everything is in tangent-plane units.
pub(super) fn sd_round_rect(p: Vec2f, half: Vec2f, radius: f32) -> f32 {
    let qx = p.x.abs() - half.x + radius;
    let qy = p.y.abs() - half.y + radius;
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - radius
}

/// Two values for point `p` and an ellipse with half axes `half`:
/// - the normalised elliptical radius k0, which is 1 on the ellipse;
/// - the tangent-plane length of one unit of k0 near `p`, k0 / |grad k0|.
///
/// This is iq's first-order ellipse distance. It is exact for a circle and close
/// near the edge, which is where the soft edge reads it. Deep inside a long
/// ellipse it is not: the centre is as deep as the half axis along the way
/// in, so a disc holds it at its inradius and a ramp that reaches deep reads
/// the exact distance (`PreparedLight::mask`, P2).
fn ellipse_metric(p: Vec2f, half: Vec2f) -> (f32, f32) {
    let nx = p.x / half.x;
    let ny = p.y / half.y;
    let k0 = (nx * nx + ny * ny).sqrt();
    let gx = nx / half.x;
    let gy = ny / half.y;
    let k1 = (gx * gx + gy * gy).sqrt();
    let scale = if k1 > 1.0e-12 { k0 / k1 } else { half.x.min(half.y) };
    (k0, scale)
}

/// Whether an ellipse with half axes `half` is a circle, as far as its
/// distance goes: its first-order distance is then exact (`ellipse_metric`)
/// and its exact one is its radius's (`ellipse_distance`).
fn is_circle(half: Vec2f) -> bool {
    let (ax, ay) = (half.x as f64, half.y as f64);
    (ax - ay).abs() <= 1.0e-6 * ax.max(ay)
}

/// The exact signed distance from `p` to the ellipse with half axes `half`,
/// negative inside (iq's closed-form ellipse distance, worked in f64: it
/// solves a quartic for the nearest point). A circle, where the quartic
/// degenerates, is its radius's distance. P2: a disc whose ramp reaches
/// deep reads it (`PreparedLight::mask`).
fn ellipse_distance(p: Vec2f, half: Vec2f) -> f32 {
    let (mut px, mut py) = ((p.x as f64).abs(), (p.y as f64).abs());
    let (mut ax, mut ay) = (half.x as f64, half.y as f64);
    if is_circle(half) {
        return ((px * px + py * py).sqrt() - 0.5 * (ax + ay)) as f32;
    }
    if px > py {
        std::mem::swap(&mut px, &mut py);
        std::mem::swap(&mut ax, &mut ay);
    }
    let l = ay * ay - ax * ax;
    let m = ax * px / l;
    let m2 = m * m;
    let n = ay * py / l;
    let n2 = n * n;
    let c = (m2 + n2 - 1.0) / 3.0;
    let c3 = c * c * c;
    let q = c3 + m2 * n2 * 2.0;
    let d = c3 + m2 * n2;
    let g = m + m * n2;
    let co = if d < 0.0 {
        let h = (q / c3).clamp(-1.0, 1.0).acos() / 3.0;
        let s = h.cos();
        let t = h.sin() * 3f64.sqrt();
        let rx = (-c * (s + t + 2.0) + m2).max(0.0).sqrt();
        let ry = (-c * (s - t + 2.0) + m2).max(0.0).sqrt();
        (ry + l.signum() * rx + g.abs() / (rx * ry).max(1.0e-300) - m) / 2.0
    } else {
        let h = 2.0 * m * n * d.sqrt();
        let s = (q + h).signum() * (q + h).abs().cbrt();
        let u = (q - h).signum() * (q - h).abs().cbrt();
        let rx = -s - u - c * 4.0 + 2.0 * m2;
        let ry = (s - u) * 3f64.sqrt();
        let rm = (rx * rx + ry * ry).sqrt();
        (ry / (rm - rx).max(1.0e-300).sqrt() + 2.0 * g / rm.max(1.0e-300) - m) / 2.0
    };
    let co = co.clamp(0.0, 1.0);
    let (rx, ry) = (ax * co, ay * (1.0 - co * co).sqrt());
    let length = ((rx - px) * (rx - px) + (ry - py) * (ry - py)).sqrt();
    let inside = (px / ax) * (px / ax) + (py / ay) * (py / ay) < 1.0;
    (if inside { -length } else { length }) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{az_el_from_dir, vec, Env, HdriParams};
    use crate::{arr, CasterParams};
    use crate::caster::fixtures::{caster_at, leaves};

    /// P2: `ellipse_distance` is the exact signed distance to an ellipse,
    /// against the brute-force nearest of 4000 points on it refined by
    /// Newton, over a grid in and round ellipses from a circle to 12:1, the
    /// centre and both axes included.
    #[test]
    fn the_ellipse_distance_is_exact() {
        for (hx, hy) in [(1.0f64, 1.0f64), (1.0, 0.999), (2.0, 1.0), (1.0, 4.0), (4.0, 1.0), (0.3, 3.6), (0.05, 0.02)] {
            let size = hx.max(hy);
            let brute = |x: f64, y: f64| {
                let mut best = (f64::INFINITY, 0.0f64);
                for i in 0..4000 {
                    let t = std::f64::consts::TAU * i as f64 / 4000.0;
                    let e = (hx * t.cos() - x).powi(2) + (hy * t.sin() - y).powi(2);
                    if e < best.0 {
                        best = (e, t);
                    }
                }
                let mut t = best.1;
                for _ in 0..8 {
                    let (c, s) = (t.cos(), t.sin());
                    let (ex, ey) = (hx * c - x, hy * s - y);
                    let f = ex * (-hx * s) + ey * (hy * c);
                    let df = hx * hx * s * s + hy * hy * c * c + ex * (-hx * c) + ey * (-hy * s);
                    if df.abs() > 1.0e-12 {
                        t -= f / df;
                    }
                }
                let dist = ((hx * t.cos() - x).powi(2) + (hy * t.sin() - y).powi(2)).sqrt();
                if (x / hx).powi(2) + (y / hy).powi(2) < 1.0 { -dist } else { dist }
            };
            for j in 0..=40 {
                for i in 0..=40 {
                    let (x, y) = (1.3 * hx * (i as f64 / 20.0 - 1.0), 1.3 * hy * (j as f64 / 20.0 - 1.0));
                    let (x, y) = (x as f32 as f64, y as f32 as f64);
                    let got = ellipse_distance(vec2f(x as f32, y as f32), vec2f(hx as f32, hy as f32)) as f64;
                    let want = brute(x, y);
                    assert!((got - want).abs() < 2.0e-5 * size, "{hx} x {hy} at ({x}, {y}): {got}, the nearest point says {want}");
                }
            }
        }
    }
    use makepad_render_material::ibl::{self, EnvMap};

    #[test]
    fn kelvin_has_luminance_one_and_warms_as_it_drops() {
        for kelvin in [1000.0, 1900.0, 2700.0, 4000.0, 5600.0, 6500.0, 10000.0, 20000.0] {
            let c = kelvin_to_rgb(kelvin);
            assert!(c.is_finite() && c.x >= 0.0 && c.y >= 0.0 && c.z >= 0.0, "{kelvin} K: {c:?}");
            assert!((luminance(c) - 1.0).abs() < 1.0e-4, "{kelvin} K: luminance {}", luminance(c));
        }
        // 6500 K sits on the Planckian locus just below D65: white within a few percent.
        let daylight = kelvin_to_rgb(6500.0);
        for channel in [daylight.x, daylight.y, daylight.z] {
            assert!((channel - 1.0).abs() < 0.06, "6500 K: {daylight:?}");
        }
        let tungsten = kelvin_to_rgb(2700.0);
        assert!(tungsten.x > tungsten.y && tungsten.y > tungsten.z, "2700 K: {tungsten:?}");
        // Red over blue falls steadily as the temperature climbs (blue is unclipped from 2000 K).
        let mut last = f32::MAX;
        for kelvin in (2000..=20000).step_by(500) {
            let c = kelvin_to_rgb(kelvin as f32);
            let red_over_blue = c.x / c.z;
            assert!(red_over_blue < last, "{kelvin} K");
            last = red_over_blue;
        }
        // Out-of-range and NaN inputs clamp to the ends instead of poisoning the map.
        assert_eq!(kelvin_to_rgb(f32::NAN), kelvin_to_rgb(1000.0));
        assert_eq!(kelvin_to_rgb(1.0e9), kelvin_to_rgb(20000.0));
    }

    #[test]
    fn tint_shifts_green_and_magenta_at_constant_luminance() {
        assert!((tint_rgb(0.0) - Vec3f::all(1.0)).length() < 1.0e-5);
        let magenta = tint_rgb(1.0);
        let green = tint_rgb(-1.0);
        assert!(magenta.y < magenta.x && magenta.y < magenta.z, "{magenta:?}");
        assert!(green.y > green.x && green.y > green.z, "{green:?}");
        for c in [magenta, green, tint_rgb(0.3)] {
            assert!((luminance(c) - 1.0).abs() < 1.0e-5, "{c:?}");
        }
    }

    #[test]
    fn light_color_is_kelvin_times_ev_or_explicit_rgb() {
        // The default light: 6500 K, tint 0, +3 EV.
        let mut light = LightParams::default();
        assert!((luminance(light_color(&light)) - 8.0).abs() < 1.0e-3);
        // Tint changes the hue, not the brightness.
        light.tint = 0.5;
        assert!((luminance(light_color(&light)) - 8.0).abs() < 1.0e-3);
        // An explicit colour overrides Kelvin and tint and is used as given.
        light.rgb = Some([0.2, 0.4, 0.6]);
        light.intensity_ev = 1.0;
        assert!((light_color(&light) - vec3f(0.4, 0.8, 1.2)).length() < 1.0e-6);
    }

    #[test]
    fn light_frames_are_orthonormal_and_roll_turns_them() {
        for (az, el, roll) in [(0.0, 0.0, 0.0), (135.0, 30.0, 20.0), (270.0, 89.0, -45.0), (10.0, 90.0, 0.0), (300.0, -60.0, 170.0)] {
            let f = light_frame(az, el, roll);
            for v in [f.center, f.right, f.up] {
                assert!((v.length() - 1.0).abs() < 1.0e-5, "({az}, {el}, {roll})");
            }
            assert!(f.center.dot(f.right).abs() < 1.0e-5, "({az}, {el}, {roll})");
            assert!(f.center.dot(f.up).abs() < 1.0e-5, "({az}, {el}, {roll})");
            assert!(f.right.dot(f.up).abs() < 1.0e-5, "({az}, {el}, {roll})");
            // Right-handed, as ibl::softbox's basis: right x up = center.
            assert!((Vec3f::cross(f.right, f.up) - f.center).length() < 1.0e-5, "({az}, {el}, {roll})");
        }
        // ibl's basis. Facing north on the horizon, right = cross(+Y, -Z) = -X, which is
        // the viewer's LEFT, and up is +Y. Facing east, right = cross(+Y, +X) = -Z.
        let north = light_frame(0.0, 0.0, 0.0);
        assert!((north.right - vec3f(-1.0, 0.0, 0.0)).length() < 1.0e-6);
        assert!((north.up - vec3f(0.0, 1.0, 0.0)).length() < 1.0e-6);
        let east = light_frame(90.0, 0.0, 0.0);
        assert!((east.right - vec3f(0.0, 0.0, -1.0)).length() < 1.0e-6);
        // Positive roll turns the shape counter-clockwise as the viewer sees it: after
        // 90 degrees the axis that pointed to the viewer's left (-X) points down.
        let rolled = light_frame(0.0, 0.0, 90.0);
        assert!((rolled.right - vec3f(0.0, -1.0, 0.0)).length() < 1.0e-6);
        assert!((rolled.up - vec3f(-1.0, 0.0, 0.0)).length() < 1.0e-6);
        // The frame stays continuous as a light climbs to the zenith.
        let top = light_frame(30.0, 90.0, 0.0);
        let near_top = light_frame(30.0, 89.9, 0.0);
        assert!((top.right - near_top.right).length() < 0.01 && (top.up - near_top.up).length() < 0.01);
    }

    #[test]
    fn project_is_gnomonic_and_rejects_the_far_side() {
        let tilted = light_frame(90.0, 20.0, 0.0);
        let p = project(&tilted, tilted.center).expect("the centre projects");
        assert!(p.x.abs() < 1.0e-6 && p.y.abs() < 1.0e-6);
        let east = light_frame(90.0, 0.0, 0.0);
        // 10 degrees further round the horizon (clockwise: the viewer's right) is
        // tan(10) along -right, because ibl's right points to the viewer's left.
        let q = project(&east, dir_from_az_el(100.0, 0.0)).expect("in front");
        assert!((q.x + 10f32.to_radians().tan()).abs() < 1.0e-5 && q.y.abs() < 1.0e-6, "{q:?}");
        // 10 degrees up is tan(10) up.
        let q = project(&east, dir_from_az_el(90.0, 10.0)).expect("in front");
        assert!(q.x.abs() < 1.0e-6 && (q.y - 10f32.to_radians().tan()).abs() < 1.0e-5, "{q:?}");
        // Behind, at 90 degrees, and NaN are all rejected.
        assert!(project(&east, dir_from_az_el(270.0, 0.0)).is_none());
        assert!(project(&east, dir_from_az_el(180.0, 0.0)).is_none());
        assert!(project(&east, vec3f(f32::NAN, 0.0, 0.0)).is_none());
    }

    fn black_studio() -> StudioParams {
        StudioParams {
            top: [0.0; 3],
            horizon: [0.0; 3],
            floor: [0.0; 3],
            ..Default::default()
        }
    }

    /// A hard-edged black flag: a Multiply light whose colour is zero.
    fn flag_at(azimuth_deg: f32, elevation_deg: f32, size_deg: f32) -> LightParams {
        LightParams {
            name: "Flag".to_string(),
            azimuth_deg,
            elevation_deg,
            width_deg: size_deg,
            height_deg: size_deg,
            corner: 0.0,
            softness: 0.0,
            blend: "multiply".to_string(),
            rgb: Some([0.0; 3]),
            ..Default::default()
        }
    }

    #[test]
    fn backdrop_blends_top_horizon_and_floor() {
        let mut s = StudioParams {
            top: [1.0, 0.0, 0.0],
            horizon: [0.0, 1.0, 0.0],
            floor: [0.0, 0.0, 1.0],
            ..Default::default()
        };
        s.horizon_softness = 0.2; // 18 degrees of elevation
        let studio = Studio::new(&s, &[]);
        assert_eq!(studio.backdrop(vec3f(0.0, 1.0, 0.0)), vec3f(1.0, 0.0, 0.0));
        assert_eq!(studio.backdrop(vec3f(0.0, -1.0, 0.0)), vec3f(0.0, 0.0, 1.0));
        assert!((studio.backdrop(vec3f(1.0, 0.0, 0.0)) - vec3f(0.0, 1.0, 0.0)).length() < 1.0e-6);
        // Half way up the 18 degree band the gradient is half way.
        let half = studio.backdrop(dir_from_az_el(0.0, 9.0));
        assert!((half.x - 0.5).abs() < 1.0e-3 && (half.y - 0.5).abs() < 1.0e-3, "{half:?}");
        let half_down = studio.backdrop(dir_from_az_el(0.0, -9.0));
        assert!((half_down.z - 0.5).abs() < 1.0e-3 && (half_down.y - 0.5).abs() < 1.0e-3, "{half_down:?}");
        // Past the band it is all top.
        assert!((studio.backdrop(dir_from_az_el(0.0, 20.0)) - vec3f(1.0, 0.0, 0.0)).length() < 1.0e-6);
    }

    #[test]
    fn a_light_is_its_colour_at_the_centre_and_nothing_outside() {
        for shape in ["rect", "disc", "ring"] {
            let light = LightParams {
                shape: shape.to_string(),
                azimuth_deg: 30.0,
                elevation_deg: 10.0,
                width_deg: 40.0,
                height_deg: 20.0,
                ..Default::default()
            };
            let color = light_color(&light);
            let studio = Studio::new(&black_studio(), &[light]);
            let centre = dir_from_az_el(30.0, 10.0);
            let got = studio.apply_lights(centre, Vec3f::default());
            if shape == "ring" {
                assert_eq!(got, Vec3f::default(), "a ring is dark in its middle");
                // Its centre line (inner 0.5, so 75% of the half width) is at full strength.
                let frame = light_frame(30.0, 10.0, 0.0);
                let x = 0.75 * 20f32.to_radians().tan();
                let on_line = (frame.center + frame.right * x).normalize();
                let lit = studio.apply_lights(on_line, Vec3f::default());
                assert!((lit - color).length() < 1.0e-4 * luminance(color), "ring: {lit:?}");
            } else {
                assert!((got - color).length() < 1.0e-5 * luminance(color), "{shape}: {got:?}");
            }
            // Outside the shape, and behind the viewer, the base passes through untouched.
            assert_eq!(studio.apply_lights(dir_from_az_el(30.0, 40.0), Vec3f::all(0.25)), Vec3f::all(0.25), "{shape}");
            assert_eq!(studio.apply_lights(dir_from_az_el(210.0, -10.0), Vec3f::all(0.25)), Vec3f::all(0.25), "{shape}");
        }
    }

    #[test]
    fn multiply_scales_only_what_came_before_it() {
        // The default light: az 0, el 30, 30x20 degrees, 6500 K at +3 EV.
        let soft = LightParams::default();
        let flag = flag_at(0.0, 30.0, 10.0);
        let centre = dir_from_az_el(0.0, 30.0);
        let grey = Vec3f::all(0.5);
        // A flag listed after the softbox blacks out the backdrop and the softbox behind it.
        let after = Studio::new(&black_studio(), &[soft.clone(), flag.clone()]);
        assert_eq!(after.apply_lights(centre, grey), Vec3f::default());
        // Listed before, it blacks out the backdrop only; the softbox stays on top.
        let before = Studio::new(&black_studio(), &[flag, soft.clone()]);
        let got = before.apply_lights(centre, grey);
        assert!((got - light_color(&soft)).length() < 1.0e-5 * luminance(light_color(&soft)), "{got:?}");
    }

    #[test]
    fn disabled_lights_are_skipped_and_the_first_add_key_leads() {
        let off = LightParams {
            enabled: false,
            key: true,
            ..Default::default()
        };
        let key = LightParams {
            key: true,
            azimuth_deg: 200.0,
            elevation_deg: 35.0,
            width_deg: 30.0,
            height_deg: 20.0,
            ..Default::default()
        };
        let studio = Studio::new(&black_studio(), &[off, key.clone()]);
        // The disabled light at (0, 30) draws nothing.
        assert_eq!(studio.apply_lights(dir_from_az_el(0.0, 30.0), Vec3f::default()), Vec3f::default());
        let sun = studio.key().expect("the enabled key light");
        assert!((sun.dir - dir_from_az_el(200.0, 35.0)).length() < 1.0e-6);
        // Its colour, times the share of its 15 degree cone the 30 x 20 rect fills.
        let fill = sun.radiance.y / light_color(&key).y;
        assert!((0.8..0.86).contains(&fill), "fill {fill}");
        assert!((sun.radiance - light_color(&key) * fill).length() < 1.0e-5 * sun.radiance.length());
        assert!((sun.cos_radius - 15f32.to_radians().cos()).abs() < 1.0e-6);
        let mut flag_key = flag_at(90.0, 0.0, 10.0);
        flag_key.key = true;
        assert!(Studio::new(&black_studio(), &[flag_key]).key().is_none(), "a flag is never the key");
    }

    /// The direction at the centre of an equirect texel, in ibl's convention.
    fn uv_dir(u: f32, v: f32) -> Vec3f {
        vec(ibl::equirect_uv_to_dir([u, v]))
    }

    fn studio_params(lights: Vec<LightParams>) -> HdriParams {
        HdriParams {
            mode: "studio".to_string(),
            studio: black_studio(),
            lights,
            ..Default::default()
        }
    }

    /// A round light with a hotspot, so it has a single brightest direction.
    fn round_light(azimuth_deg: f32, elevation_deg: f32) -> LightParams {
        LightParams {
            shape: "disc".to_string(),
            azimuth_deg,
            elevation_deg,
            width_deg: 20.0,
            height_deg: 20.0,
            softness: 0.5,
            hotspot: 1.0,
            ..Default::default()
        }
    }

    fn peak_pixel(map: &EnvMap) -> (usize, usize) {
        let mut best = (0usize, f32::MIN);
        for (i, t) in map.data.iter().enumerate() {
            let l = luminance(vec3f(t[0], t[1], t[2]));
            if l > best.1 {
                best = (i, l);
            }
        }
        (best.0 % map.width, best.0 / map.width)
    }

    #[test]
    fn a_light_peaks_at_its_pixel() {
        let (w, h) = (64, 32);
        // Aim the light at the centre of pixel (40, 10).
        let (az, el) = az_el_from_dir(uv_dir(40.5 / w as f32, 10.5 / h as f32));
        // ibl's convention: north (-Z) is at u = 0.5, so azimuth a sits at u = 0.5 + a/360.
        assert!((az - (40.5 / 64.0 - 0.5) * 360.0).abs() < 1.0e-3, "az {az}");
        let env = Env::new(&studio_params(vec![round_light(az, el)]));
        let map = env.bake(w);
        assert_eq!((map.width, map.height), (w, h));
        assert_eq!(peak_pixel(&map), (40, 10));
    }

    #[test]
    fn rotation_moves_a_light_by_rotation_deg() {
        let (w, h) = (64, 32);
        let (az, el) = az_el_from_dir(uv_dir(40.5 / w as f32, 10.5 / h as f32));
        let mut p = studio_params(vec![round_light(az, el)]);
        // rotate_y has ibl's sign: +90 takes -Z to -X, so content at azimuth a shows at
        // a - rotation_deg. 45 degrees is 8 of 64 columns, toward -u.
        p.rotation_deg = 45.0;
        assert_eq!(peak_pixel(&Env::new(&p).bake(w)), (32, 10));
        p.rotation_deg = -90.0;
        assert_eq!(peak_pixel(&Env::new(&p).bake(w)), (56, 10));
    }

    #[test]
    fn a_multiply_flag_darkens_only_its_area() {
        let (w, h) = (64, 32);
        let mut p = studio_params(Vec::new());
        p.studio.top = [0.5; 3];
        p.studio.horizon = [0.5; 3];
        p.studio.floor = [0.5; 3];
        let soft = LightParams {
            azimuth_deg: 90.0,
            elevation_deg: 0.0,
            width_deg: 60.0,
            height_deg: 40.0,
            ..Default::default()
        };
        p.lights = vec![soft.clone()];
        let open = Env::new(&p).bake(w);
        p.lights = vec![soft, flag_at(90.0, 0.0, 12.0)];
        let flagged = Env::new(&p).bake(w);
        let frame = light_frame(90.0, 0.0, 0.0);
        let mut darkened = 0;
        for y in 0..h {
            for x in 0..w {
                let dir = uv_dir((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
                let at = project(&frame, dir);
                // The flag's half size is tan(6 degrees) = 0.105.
                let inside = at.is_some_and(|q| q.x.abs() < 0.1 && q.y.abs() < 0.1);
                let outside = at.is_none_or(|q| q.x.abs() > 0.12 || q.y.abs() > 0.12);
                let i = y * w + x;
                for k in 0..3 {
                    assert!(flagged.data[i][k] <= open.data[i][k], "pixel ({x}, {y}) got brighter");
                    if inside {
                        assert_eq!(flagged.data[i][k], 0.0, "pixel ({x}, {y}) is under the flag");
                    }
                    if outside {
                        assert_eq!(flagged.data[i][k], open.data[i][k], "pixel ({x}, {y}) is beside the flag");
                    }
                }
                if inside {
                    assert!(open.data[i][0] > 0.0, "pixel ({x}, {y}) is lit before the flag");
                    darkened += 1;
                }
            }
        }
        assert!(darkened > 0);
    }

    #[test]
    fn a_disabled_light_leaves_the_map_untouched() {
        let mut off = round_light(90.0, 10.0);
        off.enabled = false;
        let mut p = studio_params(Vec::new());
        p.studio.horizon = [0.3; 3];
        let without = Env::new(&p).bake(32);
        p.lights = vec![off];
        assert_eq!(Env::new(&p).bake(32), without);
    }

    /// What the map draws of the key of `lights` over what the engine's key
    /// light carries (light_fx.rs's `map_over_key_among`): ∫ (map - the map
    /// without its key) dΩ in luminance over the key's covering cone, on a
    /// 256 x 256 midpoint grid, over the key's irradiance.
    fn map_over_key(lights: &[LightParams]) -> f32 {
        let studio = Studio::new(&black_studio(), lights);
        let key = studio.key().expect("a key light");
        let (azimuth_deg, elevation_deg) = az_el_from_dir(key.dir);
        let frame = light_frame(azimuth_deg, elevation_deg, 0.0);
        let c = key.cos_cover.max(0.05);
        let reach = (1.0 - c * c).sqrt() / c;
        let n = 256;
        let cell = 2.0 * reach / n as f32;
        let black = Vec3f::default();
        let mut sum = 0.0f64;
        for j in 0..n {
            let y = -reach + (j as f32 + 0.5) * cell;
            for i in 0..n {
                let x = -reach + (i as f32 + 0.5) * cell;
                let dir = (frame.center + frame.right * x + frame.up * y).normalize();
                let drawn = luminance(studio.apply_lights(dir, black)) - luminance(studio.apply_lights_without_key(dir, black));
                sum += (drawn * (1.0 + x * x + y * y).powf(-1.5)) as f64;
            }
        }
        (sum * (cell * cell) as f64) as f32 / luminance(key.irradiance())
    }

    /// Redesign §5: a caster multiplies what is drawn before it (the base and
    /// the lights listed earlier) and leaves the lights listed after it
    /// alone, sharp or soft.
    #[test]
    fn a_caster_darkens_what_is_below_it_and_not_what_is_above() {
        // The default light: az 0, el 30, 30x20 degrees, 6500 K at +3 EV.
        let soft = LightParams::default();
        let centre = dir_from_az_el(0.0, 30.0);
        let grey = Vec3f::all(0.5);
        let level = luminance(light_color(&soft));
        for softness in [0.0f32, 0.5] {
            let c = caster_at(0.0, 30.0, 10.0, 10.0, None, CasterParams { softness, ..CasterParams::default() });
            // Listed after the softbox it hides the backdrop and the softbox behind it.
            let got = Studio::new(&black_studio(), &[soft.clone(), c.clone()]).apply_lights(centre, grey);
            assert!(got.max_elem() <= 1.0e-5 * level, "softness {softness}: listed after, it hides the softbox and the base: {got:?}");
            // Listed before, it hides the backdrop only; the softbox stays on top.
            let got = Studio::new(&black_studio(), &[c, soft.clone()]).apply_lights(centre, grey);
            assert!((got - light_color(&soft)).length() < 1.0e-5 * level, "softness {softness}: listed before, it hides only the base: {got:?}");
        }
    }

    /// Strength 0 changes nothing, bit for bit: not the map, not the key,
    /// sharp or soft, in Studio and Sky mode, whatever the tint.
    #[test]
    fn a_caster_of_strength_zero_changes_nothing() {
        let lights = vec![LightParams { key: true, azimuth_deg: 20.0, elevation_deg: 25.0, ..LightParams::default() }];
        let mut sky = HdriParams { lights: lights.clone(), ..HdriParams::default() };
        sky.sky.sun.mode = "manual".to_string();
        sky.sky.sun.azimuth_deg = 0.0;
        sky.sky.sun.elevation_deg = 30.0;
        for (mode, base) in [("studio", studio_params(lights)), ("sky", sky)] {
            let open = Env::new(&base);
            let (map, key) = (open.bake(64), open.sun().map(|k| k.radiance));
            for softness in [0.0f32, 0.6] {
                let mut p = base.clone();
                p.lights.push(caster_at(10.0, 28.0, 50.0, 50.0, Some(leaves(0.6, 2)), CasterParams { strength: 0.0, tint: [0.3, 0.6, 0.9], softness, ..CasterParams::default() }));
                let env = Env::new(&p);
                assert!(env.bake(64) == map, "{mode}, softness {softness}: strength 0 changed the map");
                assert_eq!(env.sun().map(|k| k.radiance), key, "{mode}, softness {softness}: strength 0 changed the key");
            }
        }
    }

    /// Strength 1: where the caster covers, what is behind it is multiplied
    /// by its tint (acc * tint); beyond the blur's reach it is untouched, and
    /// in between it lies between the two.
    #[test]
    fn a_caster_of_strength_one_leaves_its_tint_where_it_covers() {
        let tint = [0.2f32, 0.5, 0.8];
        let base = vec3f(0.6, 0.3, 0.9);
        let want_inside = arr(base * vec3f(tint[0], tint[1], tint[2]));
        let frame = light_frame(90.0, 0.0, 0.0);
        let half = 15f32.to_radians().tan();
        for softness in [0.0f32, 0.5] {
            let studio = Studio::new(&black_studio(), &[caster_at(90.0, 0.0, 30.0, 30.0, None, CasterParams { tint, softness, ..CasterParams::default() })]);
            let reach = softness * crate::caster::CASTER_SOFT_SCALE * half;
            for j in 0..21 {
                for i in 0..21 {
                    let (x, y) = (-1.5 * half + 3.0 * half * i as f32 / 20.0, -1.5 * half + 3.0 * half * j as f32 / 20.0);
                    let dir = (frame.center + frame.right * x + frame.up * y).normalize();
                    let got = arr(studio.apply_lights(dir, base));
                    let r = x.abs().max(y.abs());
                    for k in 0..3 {
                        if r < half - reach - 0.01 {
                            assert!((got[k] - want_inside[k]).abs() < 1.0e-5, "softness {softness} at ({x}, {y}): {got:?}, inside it is {want_inside:?}");
                        } else if r > half + reach + 0.01 {
                            assert_eq!(got[k], arr(base)[k], "softness {softness} at ({x}, {y}): beside it");
                        } else {
                            assert!(got[k] <= arr(base)[k] + 1.0e-6 && got[k] >= want_inside[k] - 1.0e-6, "softness {softness} at ({x}, {y}): {got:?}");
                        }
                    }
                }
            }
        }
    }

    /// A caster's tint is what it lets through where it covers:
    /// acc * (1 - s*m + s*m*tint), the same coverage m in every channel (a
    /// black caster of the same pattern measures it), sharp and soft.
    #[test]
    fn a_tinted_caster_lets_its_colour_through() {
        let white = Vec3f::all(1.0);
        let tint = [0.9f32, 0.4, 0.1];
        for softness in [0.0f32, 0.5] {
            for strength in [1.0f32, 0.6] {
                let studio_of = |settings: CasterParams| Studio::new(&black_studio(), &[caster_at(0.0, 20.0, 60.0, 45.0, Some(leaves(0.7, 2)), settings)]);
                let tinted = studio_of(CasterParams { strength, tint, softness, ..CasterParams::default() });
                let black = studio_of(CasterParams { strength: 1.0, tint: [0.0; 3], softness, ..CasterParams::default() });
                let mut covered = 0;
                for j in 0..41 {
                    for i in 0..41 {
                        let dir = dir_from_az_el(-35.0 + 70.0 * i as f32 / 40.0, -5.0 + 50.0 * j as f32 / 40.0);
                        let m = 1.0 - black.apply_lights(dir, white).x;
                        let got = arr(tinted.apply_lights(dir, white));
                        for k in 0..3 {
                            let want = 1.0 - strength * m + strength * m * tint[k];
                            assert!((got[k] - want).abs() < 1.0e-5, "softness {softness}, strength {strength}: channel {k} is {}, want {want} (m {m})", got[k]);
                        }
                        if m > 0.5 {
                            covered += 1;
                            assert!(got[0] > got[1] && got[1] > got[2], "the tint's colour shows: {got:?}");
                        }
                    }
                }
                assert!(covered > 50, "softness {softness}, strength {strength}: only {covered} samples covered");
            }
        }
    }

    /// A caster never adds light: every texel of the map with it is at most
    /// the map without it, sharp or soft, whatever its tint, in Sky and
    /// Studio mode; a white tint changes nothing, bit for bit.
    #[test]
    fn a_caster_never_adds_light() {
        let light = LightParams { azimuth_deg: -20.0, elevation_deg: 15.0, ..LightParams::default() };
        let mut sky = HdriParams { lights: vec![light.clone()], ..HdriParams::default() };
        sky.sky.sun.mode = "manual".to_string();
        sky.sky.sun.azimuth_deg = 0.0;
        sky.sky.sun.elevation_deg = 25.0;
        let mut studio = studio_params(vec![light]);
        studio.studio.top = [0.5; 3];
        studio.studio.horizon = [0.5; 3];
        studio.studio.floor = [0.5; 3];
        for (mode, base) in [("sky", sky), ("studio", studio)] {
            let open = Env::new(&base).bake(64);
            for gobo in [None, Some(leaves(0.6, 2))] {
                for softness in [0.0f32, 0.5] {
                    for tint in [[0.0f32; 3], [0.3, 1.0, 0.6], [1.0; 3]] {
                        let mut p = base.clone();
                        p.lights.push(caster_at(0.0, 20.0, 70.0, 50.0, gobo.clone(), CasterParams { softness, tint, ..CasterParams::default() }));
                        let shaded = Env::new(&p).bake(64);
                        if tint == [1.0; 3] {
                            assert!(shaded == open, "{mode}, {gobo:?}, softness {softness}: a white caster changed the map");
                            continue;
                        }
                        let mut darker = 0;
                        for (o, s) in open.data.iter().zip(&shaded.data) {
                            for k in 0..3 {
                                assert!(s[k] <= o[k] * (1.0 + 1.0e-6) + 1.0e-7, "{mode}, {gobo:?}, softness {softness}, tint {tint:?}: {} over {}", s[k], o[k]);
                                if s[k] < o[k] - 1.0e-4 {
                                    darker += 1;
                                }
                            }
                        }
                        assert!(darker > 0, "{mode}, {gobo:?}, softness {softness}, tint {tint:?}: the caster took out nothing");
                    }
                }
            }
        }
    }

    #[test]
    fn a_disabled_caster_leaves_the_map_untouched() {
        let mut p = studio_params(vec![round_light(90.0, 10.0)]);
        p.studio.horizon = [0.3; 3];
        let without = Env::new(&p).bake(32);
        for softness in [0.0f32, 0.7] {
            let mut off = caster_at(90.0, 10.0, 40.0, 40.0, None, CasterParams { softness, ..CasterParams::default() });
            off.enabled = false;
            let mut q = p.clone();
            q.lights.push(off);
            assert_eq!(Env::new(&q).bake(32), without, "softness {softness}");
        }
    }

    /// A caster is never the key, whatever it says: clamp() clears its key,
    /// and the studio, which may be handed lights clamp() has not held,
    /// skips it. The light marked key after it leads.
    #[test]
    fn a_caster_is_never_the_key() {
        let marked = LightParams { key: true, blend: "add".to_string(), intensity_ev: 3.0, ..caster_at(200.0, 35.0, 30.0, 20.0, None, CasterParams::default()) };
        assert!(Studio::new(&black_studio(), &[marked.clone()]).key().is_none(), "a caster alone is no key");
        let key = LightParams { key: true, azimuth_deg: 40.0, elevation_deg: 20.0, ..LightParams::default() };
        let led = Studio::new(&black_studio(), &[marked.clone(), key.clone()]).key().expect("the light leads");
        assert!((led.dir - dir_from_az_el(40.0, 20.0)).length() < 1.0e-6, "{:?}", led.dir);
        let env = Env::new(&studio_params(vec![marked, key]));
        assert!((env.sun().expect("a key").dir - dir_from_az_el(40.0, 20.0)).length() < 1.0e-5);
    }

    /// D1 with a caster (redesign §5, the key): listed after the key it dims
    /// the key by what it takes out of the map (the map draws over the key's
    /// covering cone what the key carries, to 2 %), sharp or soft, solid or
    /// leaves; listed before the key it leaves the key exactly as it was.
    #[test]
    fn a_caster_in_front_of_the_key_dims_it_and_one_behind_does_not() {
        let key = LightParams { key: true, azimuth_deg: 0.0, elevation_deg: 20.0, width_deg: 60.0, height_deg: 45.0, ..LightParams::default() };
        let alone = Studio::new(&black_studio(), &[key.clone()]).key().expect("a key");
        let mut off = Vec::new();
        for softness in [0.0f32, 0.6] {
            for (name, gobo) in [("solid", None), ("leaves", Some(leaves(0.5, 2)))] {
                let c = caster_at(15.0, 25.0, 40.0, 40.0, gobo, CasterParams { softness, tint: [0.2, 0.3, 0.1], ..CasterParams::default() });
                let behind = Studio::new(&black_studio(), &[c.clone(), key.clone()]).key().expect("a key");
                assert_eq!(behind.radiance, alone.radiance, "{name} at softness {softness}: a caster listed before the key changed it");
                let lights = vec![key.clone(), c];
                let front = Studio::new(&black_studio(), &lights).key().expect("a key");
                let share = luminance(front.radiance) / luminance(alone.radiance);
                assert!(share < 0.95, "{name} at softness {softness}: the caster in front leaves {share} of the key");
                let ratio = map_over_key(&lights);
                println!("{name} at softness {softness}: the key keeps {share:.4}; the map draws {ratio:.4} of it");
                if (ratio - 1.0).abs() > 0.02 {
                    off.push(format!("{name} at softness {softness}: the map draws {ratio:.4} of what the key carries"));
                }
            }
        }
        assert!(off.is_empty(), "{}", off.join("\n"));
    }

    #[test]
    fn the_key_light_is_the_env_sun_in_world_space() {
        let key = LightParams {
            key: true,
            azimuth_deg: 200.0,
            elevation_deg: 35.0,
            width_deg: 30.0,
            height_deg: 20.0,
            ..Default::default()
        };
        let mut p = studio_params(vec![LightParams::default(), key.clone()]);
        p.intensity_ev = 1.0;
        let sun = Env::new(&p).sun().expect("a key light");
        assert!((sun.dir - dir_from_az_el(200.0, 35.0)).length() < 1.0e-5);
        let expected = Studio::new(&p.studio, &p.lights).key().expect("a key light").radiance * 2.0;
        assert!((sun.radiance - expected).length() < 1.0e-5 * luminance(expected));
        assert!((sun.cos_radius - 15f32.to_radians().cos()).abs() < 1.0e-6);
        assert!(sun.validate().is_ok(), "the key is a valid scene key: {sun:?}");
        // The map's yaw turns the key with it, counter-clockwise seen from above
        // (ibl's sign): azimuth 200 shows at 170.
        p.rotation_deg = 30.0;
        let turned = Env::new(&p).sun().expect("a key light");
        assert!((turned.dir - dir_from_az_el(170.0, 35.0)).length() < 1.0e-5);
        p.lights[1].enabled = false;
        assert!(Env::new(&p).sun().is_none(), "a studio without a key has no sun");
    }

    /// `params` with the backdrop black and every Add light but the key taken out:
    /// what is left is the key as `apply_lights` paints it, behind the flags. A
    /// scrim (B5) stays as bare cloth, its colour black: it hides what is behind
    /// it, the key too when it is listed after it, and adds nothing.
    fn the_key_alone(params: &HdriParams) -> HdriParams {
        let key = params.lights.iter().position(|l| l.enabled && l.key && l.blend() == Blend::Add).expect("a key light");
        let kept = |(i, l): (usize, &LightParams)| {
            if i == key || l.blend() == Blend::Multiply {
                Some(l.clone())
            } else if l.shape() == LightShape::Scrim {
                Some(LightParams { rgb: Some([0.0; 3]), ..l.clone() })
            } else {
                None
            }
        };
        HdriParams {
            studio: black_studio(),
            lights: params.lights.iter().enumerate().filter_map(kept).collect(),
            ..params.clone()
        }
    }

    /// Runs f(0..n) on every core, for the fine bakes below.
    fn on_all_cores(n: usize, f: &(dyn Fn(usize) + Sync)) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let next = AtomicUsize::new(0);
        let cores = std::thread::available_parallelism().map_or(4, |c| c.get()).min(16);
        std::thread::scope(|scope| {
            for _ in 0..cores {
                scope.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    f(i);
                });
            }
        });
    }

    /// A baked map's ∫ L dΩ: each texel times the exact solid angle of its cell.
    fn map_emission(map: &EnvMap) -> Vec3f {
        use std::f64::consts::{PI, TAU};
        let (w, h) = (map.width, map.height);
        let mut sum = [0.0f64; 3];
        for y in 0..h {
            let (top, bottom) = (PI * y as f64 / h as f64, PI * (y + 1) as f64 / h as f64);
            let cell = TAU / w as f64 * (top.cos() - bottom.cos());
            for t in &map.data[y * w..(y + 1) * w] {
                for k in 0..3 {
                    sum[k] += t[k] as f64 * cell;
                }
            }
        }
        vec3f(sum[0] as f32, sum[1] as f32, sum[2] as f32)
    }

    /// Keys that exercise every term the key's integral must honour, each with
    /// the light list it sits in.
    fn awkward_keys() -> Vec<(&'static str, Vec<LightParams>)> {
        let ring = LightParams {
            key: true,
            shape: "ring".to_string(),
            azimuth_deg: 70.0,
            elevation_deg: 20.0,
            width_deg: 36.0,
            height_deg: 18.0,
            roll_deg: 35.0,
            inner: 0.8,
            softness: 0.0,
            ..Default::default()
        };
        let strip = LightParams {
            key: true,
            azimuth_deg: 250.0,
            elevation_deg: 10.0,
            width_deg: 8.0,
            height_deg: 60.0,
            roll_deg: 60.0,
            corner: 0.5,
            softness: 0.4,
            hotspot: 0.6,
            ..Default::default()
        };
        // A coloured Multiply gel: it keeps red and cuts green and blue.
        let gel = LightParams {
            name: "Gel".to_string(),
            azimuth_deg: 262.0,
            elevation_deg: 4.0,
            width_deg: 14.0,
            height_deg: 14.0,
            softness: 0.5,
            blend: "multiply".to_string(),
            rgb: Some([1.0, 0.5, 0.25]),
            ..Default::default()
        };
        // An Add light over the strip, after it: it is not the key's light.
        let over = LightParams { azimuth_deg: 250.0, elevation_deg: 10.0, ..Default::default() };
        // 1b (B2): a rolled, rounded hexagon key with a soft edge and a hotspot, the
        // shape core's emission, under a star-shaped blue gel, a shaped flag.
        let hexagon = LightParams {
            key: true,
            shape: "polygon".to_string(),
            azimuth_deg: 150.0,
            elevation_deg: 25.0,
            width_deg: 40.0,
            height_deg: 28.0,
            roll_deg: 20.0,
            corner: 0.3,
            softness: 0.2,
            hotspot: 0.5,
            polygon: Some(crate::PolygonParams::default()),
            ..Default::default()
        };
        let star_gel = LightParams {
            name: "Star gel".to_string(),
            shape: "polygon".to_string(),
            azimuth_deg: 152.0,
            elevation_deg: 27.0,
            width_deg: 18.0,
            height_deg: 18.0,
            softness: 0.3,
            blend: "multiply".to_string(),
            rgb: Some([0.25, 0.5, 1.0]),
            polygon: Some(crate::PolygonParams { sides: 5, star: 0.5, points: Vec::new() }),
            ..Default::default()
        };
        // 1b (B5): a soft, rolled rect key whose northern half (toward increasing
        // azimuth) is behind a scrim listed after it. A scrim is the one Add light
        // that hides what is behind it, so the map shows only the key's other half,
        // and the key carries only that.
        let screened = LightParams {
            key: true,
            azimuth_deg: 300.0,
            elevation_deg: 20.0,
            width_deg: 40.0,
            height_deg: 24.0,
            roll_deg: 15.0,
            softness: 0.3,
            hotspot: 0.4,
            ..Default::default()
        };
        let scrim = LightParams {
            name: "Scrim".to_string(),
            shape: "scrim".to_string(),
            azimuth_deg: 314.0,
            elevation_deg: 24.0,
            width_deg: 30.0,
            height_deg: 30.0,
            corner: 0.3,
            softness: 0.4,
            scrim: Some(crate::ScrimParams { source_size: 0.4, frame: 0.1 }),
            ..Default::default()
        };
        vec![
            ("a hard elliptical ring, rolled", vec![ring]),
            (
                "a rolled strip under a flag and a gel, with a flag before it and a light over it",
                vec![flag_at(250.0, 10.0, 30.0), strip, flag_at(250.0, 10.0, 8.0), gel, over],
            ),
            ("a rolled hexagon key under a star-shaped gel", vec![hexagon, star_gel]),
            ("a rolled rect key half behind a scrim listed after it", vec![screened, scrim]),
        ]
    }

    #[test]
    fn the_key_carries_the_lights_emission_over_its_cone() {
        // radiance x 2 pi (1 - cos_radius) is the light's whole ∫ L dΩ as the map
        // draws it: its shape, edge, hotspot and roll, and the flags after it.
        let mut cases: Vec<(String, HdriParams)> = ["Three-point", "Top softbox", "Rim pair", "Overcast dome", "Ring light"]
            .iter()
            .map(|name| (name.to_string(), crate::presets::preset(name).unwrap()))
            .collect();
        for (name, lights) in awkward_keys() {
            cases.push((name.to_string(), studio_params(lights)));
        }
        let mut off = Vec::new();
        for (name, p) in cases {
            let key = Env::new(&p).sun().expect("a key light");
            let cone = std::f32::consts::TAU * (1.0 - key.cos_radius);
            let got = key.radiance * cone;
            let want = map_emission(&Env::new(&the_key_alone(&p)).bake_par(2048, on_all_cores));
            let ratio = [got.x / want.x, got.y / want.y, got.z / want.z];
            if !ratio.iter().all(|r| (r - 1.0).abs() < 0.01) {
                off.push(format!("{name}: {:.4} {:.4} {:.4}", ratio[0], ratio[1], ratio[2]));
            }
        }
        assert!(off.is_empty(), "the key over its cone vs the map's emission (r g b):\n{}", off.join("\n"));
    }

    /// Every built-in key and the awkward ones, as light lists, plus two that are
    /// hardest on the integral's grid: the thinnest hard ring and the widest soft disc.
    fn key_light_lists() -> Vec<(String, Vec<LightParams>)> {
        let mut lists: Vec<(String, Vec<LightParams>)> = ["Three-point", "Top softbox", "Rim pair", "Overcast dome", "Ring light"]
            .iter()
            .map(|name| (name.to_string(), crate::presets::preset(name).unwrap().lights))
            .collect();
        for (name, lights) in awkward_keys() {
            lists.push((name.to_string(), lights));
        }
        let thin = LightParams { key: true, shape: "ring".to_string(), inner: 0.95, softness: 0.0, width_deg: 40.0, height_deg: 40.0, ..Default::default() };
        let wide = LightParams { key: true, shape: "disc".to_string(), width_deg: 170.0, height_deg: 170.0, softness: 1.0, hotspot: 0.3, ..Default::default() };
        lists.push(("the thinnest hard ring".to_string(), vec![thin]));
        lists.push(("the widest soft disc".to_string(), vec![wide]));
        lists
    }

    /// The enabled key's emission on an n x n grid.
    fn emission_on(lights: &[LightParams], n: usize) -> Vec3f {
        let studio = Studio::new(&black_studio(), lights);
        let enabled: Vec<&LightParams> = lights.iter().filter(|l| l.enabled).collect();
        let i = enabled.iter().position(|l| l.key && l.blend() == Blend::Add).expect("a key light");
        key_emission(&studio.lights[i..], &studio.fx[i..], &studio.casters[i..], n).emission
    }

    #[test]
    fn the_keys_integral_has_converged() {
        let mut worst = 0.0f32;
        for (name, lights) in key_light_lists() {
            let (grid, finer) = (emission_on(&lights, KEY_GRID), emission_on(&lights, 4 * KEY_GRID));
            for (g, f) in [(grid.x, finer.x), (grid.y, finer.y), (grid.z, finer.z)] {
                assert!(f > 0.0, "{name}: {finer:?}");
                worst = worst.max((g / f - 1.0).abs());
            }
            println!("{name}: {:.5}", grid.y / finer.y - 1.0);
        }
        assert!(worst < 1.0e-3, "a key moves by {worst} on a four times finer grid");
    }

    #[test]
    fn a_hard_rect_key_carries_its_solid_angle() {
        // A hard, square-cornered rect without a hotspot: its emission is its colour
        // times its exact solid angle, 4 atan(tx ty / sqrt(1 + tx² + ty²)).
        let key = LightParams { key: true, corner: 0.0, softness: 0.0, width_deg: 50.0, height_deg: 16.0, roll_deg: 25.0, ..Default::default() };
        let (tx, ty) = (25f32.to_radians().tan(), 8f32.to_radians().tan());
        let solid = 4.0 * (tx * ty / (1.0 + tx * tx + ty * ty).sqrt()).atan();
        let sun = Studio::new(&black_studio(), std::slice::from_ref(&key)).key().expect("a key light");
        let cone = std::f32::consts::TAU * (1.0 - sun.cos_radius);
        let want = light_color(&key) * solid;
        assert!((sun.radiance * cone - want).length() < 1.0e-3 * want.length(), "{:?} vs {want:?}", sun.radiance * cone);
    }

    /// M6: a key as small as a light may be (0.1 degrees) carries the integral
    /// exactly through the f32 cone it stores, and so does a sun-sized one:
    /// `EnvSun::irradiance` gives `key_emission`'s emission back to the
    /// float's rounding.
    #[test]
    fn a_small_key_carries_its_emission_through_its_f32_cone() {
        for size_deg in [0.1f32, 0.53] {
            let light = LightParams { key: true, shape: "disc".to_string(), width_deg: size_deg, height_deg: size_deg, softness: 0.0, ..Default::default() };
            let key = Studio::new(&black_studio(), std::slice::from_ref(&light)).key().expect("a key light");
            let want = emission_on(std::slice::from_ref(&light), KEY_GRID);
            let got = key.irradiance();
            for (g, w) in [(got.x, want.x), (got.y, want.y), (got.z, want.z)] {
                assert!(w > 0.0 && (g / w - 1.0).abs() < 1.0e-6, "{size_deg} deg: {g} for {w}");
            }
        }
    }

    /// A baked map's cosine-weighted share, Σ L cosθ dΩ / Σ L dΩ in luminance, θ
    /// measured from `dir`: what a surface facing `dir` receives of the map's
    /// whole emission.
    fn map_facing(map: &EnvMap, dir: Vec3f) -> f32 {
        use std::f64::consts::{PI, TAU};
        let (w, h) = (map.width, map.height);
        let (mut all, mut weighted) = (0.0f64, 0.0f64);
        for y in 0..h {
            let (top, bottom) = (PI * y as f64 / h as f64, PI * (y + 1) as f64 / h as f64);
            let cell = TAU / w as f64 * (top.cos() - bottom.cos());
            for x in 0..w {
                let t = map.data[y * w + x];
                let l = luminance(vec3f(t[0], t[1], t[2])) as f64 * cell;
                let d = vec(ibl::equirect_uv_to_dir([(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32]));
                all += l;
                weighted += l * d.dot(dir) as f64;
            }
        }
        (weighted / all) as f32
    }

    #[test]
    fn the_key_faces_with_the_share_of_its_emission_the_map_delivers() {
        // facing is ∫ L cosθ dΩ / ∫ L dΩ over the key's reach, from the same pass
        // as its radiance: checked against the key as the map draws it.
        let mut cases: Vec<(String, HdriParams)> = ["Three-point", "Top softbox", "Rim pair", "Overcast dome", "Ring light"]
            .iter()
            .map(|name| (name.to_string(), crate::presets::preset(name).unwrap()))
            .collect();
        for (name, lights) in awkward_keys() {
            cases.push((name.to_string(), studio_params(lights)));
        }
        let mut off = Vec::new();
        for (name, p) in cases {
            let key = Env::new(&p).sun().expect("a key light");
            assert!(key.validate().is_ok(), "{name}: {key:?}");
            let want = map_facing(&Env::new(&the_key_alone(&p)).bake_par(1024, on_all_cores), key.dir);
            if (key.facing - want).abs() > 0.01 {
                off.push(format!("{name}: facing {:.4}, the map's {want:.4}", key.facing));
            }
        }
        assert!(off.is_empty(), "the key's facing share vs the map's:\n{}", off.join("\n"));
    }

    #[test]
    fn the_built_in_keys_face_as_the_spec_measured() {
        // The numbers the EnvSun doc quotes. The dome is a 110 degree disc, the
        // softbox 70 degrees wide: a surface facing them gets less than the
        // whole of radiance x solid angle.
        for (name, facing) in [("Overcast dome", 0.78), ("Top softbox", 0.93), ("Rim pair", 0.95), ("Three-point", 0.98), ("Ring light", 0.98)] {
            let key = Env::new(&crate::presets::preset(name).unwrap()).sun().expect("a key light");
            assert!((key.facing - facing).abs() < 0.01, "{name}: facing {} vs {facing}", key.facing);
        }
        // A key a fraction of a degree wide faces fully.
        let tiny = LightParams { key: true, width_deg: 0.2, height_deg: 0.2, ..Default::default() };
        let key = Studio::new(&black_studio(), std::slice::from_ref(&tiny)).key().expect("a key light");
        assert!(key.facing > 0.9999 && key.facing <= 1.0, "{}", key.facing);
        // A key that draws nothing (a black light) has no direction to weigh: it faces fully.
        let dark = LightParams { key: true, rgb: Some([0.0; 3]), ..Default::default() };
        let key = Studio::new(&black_studio(), std::slice::from_ref(&dark)).key().expect("a key light");
        assert_eq!((key.radiance, key.facing), (Vec3f::default(), 1.0));
        assert!(key.validate().is_ok(), "{key:?}");
    }

    #[test]
    fn nothing_of_the_key_is_drawn_outside_its_covering_cone() {
        // cos_cover is the cone of the key's reach box: its corners and its soft
        // edge lie inside it, so the keyless bake, which evaluates only that cone
        // again (N3), and remove_sun's fill both take all of the key. The map is
        // black outside the cone, exactly, on a one degree grid over the sphere
        // and on rings just past its edge.
        for (name, lights) in key_light_lists() {
            let alone = the_key_alone(&studio_params(lights));
            let env = Env::new(&alone);
            let key = env.sun().expect("a key light");
            assert!(key.validate().is_ok(), "{name}: {key:?}");
            assert!(key.cos_cover < key.cos_radius, "{name}: the reach box is wider than the cone, {key:?}");
            let mut lit = 0;
            for iy in 0..180 {
                for ix in 0..360 {
                    let d = dir_from_az_el(ix as f32 + 0.5, iy as f32 - 89.5);
                    let seen = env.radiance(d) != Vec3f::default();
                    lit += seen as usize;
                    assert!(!seen || d.dot(key.dir) >= key.cos_cover, "{name}: lit at {:?}, outside the cone", az_el_from_dir(d));
                }
            }
            assert!(lit > 0, "{name}");
            // Rings of directions a hair and a few percent past the cone's edge.
            let side = Vec3f::cross(key.dir, if key.dir.y.abs() < 0.9 { vec3f(0.0, 1.0, 0.0) } else { vec3f(1.0, 0.0, 0.0) }).normalize();
            let up = Vec3f::cross(key.dir, side);
            let edge = key.cos_cover.acos();
            for scale in [1.0005f32, 1.01, 1.05] {
                let theta = (edge * scale).min(std::f32::consts::FRAC_PI_2);
                for i in 0..720 {
                    let phi = std::f32::consts::TAU * i as f32 / 720.0;
                    let d = key.dir * theta.cos() + (side * phi.cos() + up * phi.sin()) * theta.sin();
                    assert_eq!(env.radiance(d), Vec3f::default(), "{name}: lit {scale} x the cone's edge away, at ring angle {i}");
                }
            }
        }
    }

    /// N3: the engine lights a generated map from its keyless copy
    /// (`Env::bake_keyless_par`, through `envmap::bake_generated_env_map`), which
    /// must be the map with exactly the key light switched off: the flags after
    /// it, the light over it and the shape core's lights kept. Texel for texel:
    /// the copy re-evaluates the key's covering cone through the lookup without
    /// it, and outside the cone the key draws nothing.
    #[test]
    fn a_keys_lighting_copy_is_the_map_with_the_key_switched_off() {
        for (name, lights) in awkward_keys() {
            let p = studio_params(lights);
            let baked = crate::envmap::bake_generated_env_map(&p, 64, on_all_cores);
            let mut off = p.clone();
            let key = off.lights.iter().position(|l| l.enabled && l.key && l.blend() == Blend::Add).expect("a key light");
            off.lights[key].enabled = false;
            assert!(baked.keyless.as_ref() == Some(&Env::new(&off).bake(64)), "{name}: the keyless copy is not the map without its key");
            assert!(baked.keyless.as_ref() != Some(&baked.map), "{name}: the key drew nothing");
        }
    }

    /// A stroke key (1b, B3): a soft 60 degree arch with a hotspot and a
    /// brightness ramp to a third.
    fn arch_stroke() -> LightParams {
        LightParams {
            key: true,
            shape: "stroke".to_string(),
            softness: 0.6,
            hotspot: 0.3,
            stroke: Some(crate::StrokeParams {
                points: vec![[60.0, 0.0], [90.0, 8.0], [120.0, 0.0]],
                width_deg: 4.0,
                end_rgb: Some([0.3, 0.3, 0.3]),
                ..crate::StrokeParams::default()
            }),
            ..Default::default()
        }
    }

    /// Plan 2's `facing` and `cos_cover` for a stroke key come from the shape
    /// core, and they are the map's: the facing share within 0.01 of the baked
    /// map's cosine share, and no lit direction outside the covering cone.
    #[test]
    fn a_stroke_keys_facing_and_covering_cone_are_the_maps() {
        let env = Env::new(&the_key_alone(&studio_params(vec![arch_stroke()])));
        let key = env.sun().expect("the stroke is the key light");
        assert!(key.validate().is_ok(), "{key:?}");
        assert!(key.cos_cover < key.cos_radius, "the soft edge reaches past the key cone: {key:?}");
        let want = map_facing(&env.bake_par(1024, on_all_cores), key.dir);
        assert!((key.facing - want).abs() < 0.01, "facing {} vs the map's {want}", key.facing);
        for iy in 0..180 {
            for ix in 0..360 {
                let d = dir_from_az_el(ix as f32 + 0.5, iy as f32 - 89.5);
                assert!(env.radiance(d) == Vec3f::default() || d.dot(key.dir) >= key.cos_cover, "lit at {:?}, outside the covering cone", az_el_from_dir(d));
            }
        }
    }

    /// Stroke keys that exercise every term of the shape core's key integral,
    /// each soft enough for a 2K bake to resolve its edge (a hard edge lying
    /// along a texel row is off by a few per cent at 2K, and the other way at 4K).
    fn stroke_keys() -> Vec<(&'static str, LightParams)> {
        let stroke = |points: Vec<[f32; 2]>, width_deg: f32, softness: f32, hotspot: f32| LightParams {
            key: true,
            shape: "stroke".to_string(),
            softness,
            hotspot,
            stroke: Some(crate::StrokeParams { points, width_deg, ..crate::StrokeParams::default() }),
            ..Default::default()
        };
        let with = |mut light: LightParams, edit: &dyn Fn(&mut crate::StrokeParams)| {
            edit(light.stroke.as_mut().expect("a stroke block"));
            light
        };
        vec![
            ("a soft arch with a hotspot and a brightness ramp", arch_stroke()),
            ("a glowing tube: fully soft, with a full hotspot", stroke(vec![[60.0, -5.0], [120.0, -5.0]], 6.0, 1.0, 1.0)),
            ("a short wide stroke, a third of it its round ends", stroke(vec![[80.0, 10.0], [100.0, 10.0]], 10.0, 0.5, 0.5)),
            (
                "a taper to nothing that also fades to black",
                with(stroke(vec![[60.0, 5.0], [120.0, 5.0]], 6.0, 0.4, 0.0), &|s| {
                    s.profile = vec![1.0, 0.0];
                    s.end_rgb = Some([0.0; 3]);
                }),
            ),
            (
                "a closed loop with a Kelvin ramp",
                with(stroke(vec![[80.0, 0.0], [100.0, 0.0], [90.0, 15.0]], 3.0, 0.5, 0.0), &|s| {
                    s.closed = true;
                    s.end_kelvin = Some(2000.0);
                }),
            ),
            ("a dot: two points half a degree apart", stroke(vec![[90.0, 20.0], [90.5, 20.0]], 8.0, 0.5, 0.4)),
            ("the arch over a light without points", LightParams { key: true, shape: "stroke".to_string(), ..Default::default() }),
            // D1 (L:40): a band wider than the loop it runs round laps over
            // itself, so it draws more than its length times its width.
            (
                "a closed loop 45 degrees wide",
                with(stroke(vec![[80.0, 0.0], [100.0, 0.0], [90.0, 15.0]], 45.0, 0.5, 0.0), &|s| s.closed = true),
            ),
        ]
    }

    /// D2: a stroke that spreads wider than an 85 degree key (the widest a
    /// rect's or a disc's key cone gets) is not handed to the engine as the
    /// key, whichever way it spreads: reaching more than 85 degrees from its
    /// middle (a strip round the whole horizon, which has no middle to light
    /// from, or half way round), or lying all round its middle further out
    /// than an even 85 degree disc's light does (L:41's ring at elevation 10,
    /// every point 80 degrees from the zenith it would light from, facing
    /// 0.17). Its light stays in the map's lighting, and under a sky the sun
    /// leads again. An arch that reaches just under the limit is still the
    /// key, and so is a ring light drawn as a stroke overhead.
    #[test]
    fn a_stroke_that_circles_the_sky_is_not_the_key() {
        let strip = |points: Vec<[f32; 2]>, closed: bool| LightParams {
            key: true,
            shape: "stroke".to_string(),
            stroke: Some(crate::StrokeParams { points, closed, width_deg: 4.0, ..crate::StrokeParams::default() }),
            ..Default::default()
        };
        let ring = |elevation_deg: f32| strip((0..6).map(|i| [i as f32 * 60.0, elevation_deg]).collect(), true);
        let round = ring(0.0);
        let half = strip(vec![[0.0, 5.0], [90.0, 5.0], [180.0, 5.0]], false);
        let arch = strip(vec![[10.0, 0.0], [90.0, 0.0], [170.0, 0.0]], false);
        let low_ring = ring(10.0);
        for (name, light, on_line) in [("round the horizon", &round, 0.0), ("half way round", &half, 5.0), ("a ring at elevation 10", &low_ring, 10.0)] {
            assert!(spreads_too_wide_to_be_key(light), "{name}");
            assert!(Studio::new(&black_studio(), std::slice::from_ref(light)).key().is_none(), "{name}: not the key");
            let sky = HdriParams { lights: vec![light.clone()], ..Default::default() };
            assert!(matches!(Env::new(&sky).key(), Some((crate::KeyTerm::Sun, _))), "{name}: the sun leads under a sky");
            // Drawn as before: it is a light, only not the key.
            assert!(Env::new(&studio_params(vec![light.clone()])).radiance(dir_from_az_el(90.0, on_line)) != Vec3f::default(), "{name}");
        }
        assert!(!spreads_too_wide_to_be_key(&arch), "82 degrees from its middle");
        let key = Studio::new(&black_studio(), &[arch]).key().expect("the arch is the key");
        assert!(key.validate().is_ok() && key.cos_radius > 85f32.to_radians().cos(), "{key:?}");
        // A halo 60 degrees across overhead lights from the zenith as a ring
        // light does: a surface facing up receives 0.87 of it.
        let halo = ring(60.0);
        assert!(!spreads_too_wide_to_be_key(&halo), "a ring at elevation 60");
        // The even 85 degree disc's share, 0.544, is a ring's at elevation 33.
        assert!(spreads_too_wide_to_be_key(&ring(31.0)) && !spreads_too_wide_to_be_key(&ring(35.0)), "the limit lies near elevation 33");
        let key = Studio::new(&black_studio(), &[halo]).key().expect("the halo is the key");
        assert!(key.validate().is_ok() && key.dir.y > 0.99 && key.facing > 0.8, "{key:?}");
        // Only a stroke spreads: a rect as wide as a light may be still leads.
        let wide = LightParams { key: true, width_deg: 170.0, height_deg: 170.0, ..Default::default() };
        assert!(!spreads_too_wide_to_be_key(&wide) && Studio::new(&black_studio(), &[wide]).key().is_some());
    }

    /// D2 for a Ring light, as for a stroke: a thin ring 160 degrees across
    /// overhead lies all round the zenith it would light from, 79 to 80
    /// degrees out, so a surface facing up receives under the even 85 degree
    /// disc's share of its light. Marked as the key it is not handed to the
    /// engine; it is still drawn, and under a sky the sun leads. A small ring
    /// light and a ring that is nearly a disc still lead.
    #[test]
    fn a_ring_that_circles_the_sky_is_not_the_key() {
        let ring = |width_deg: f32, inner: f32| LightParams {
            key: true,
            shape: "ring".to_string(),
            elevation_deg: 90.0,
            width_deg,
            height_deg: width_deg,
            inner,
            ..Default::default()
        };
        let wide = ring(160.0, 0.9);
        assert!(spreads_too_wide_to_be_key(&wide), "a thin ring 160 degrees across");
        assert!(Studio::new(&black_studio(), std::slice::from_ref(&wide)).key().is_none(), "not the key");
        let sky = HdriParams { lights: vec![wide.clone()], ..Default::default() };
        assert!(matches!(Env::new(&sky).key(), Some((crate::KeyTerm::Sun, _))), "the sun leads under a sky");
        // Drawn as before, on the middle of its band (0.95 of its radius).
        let on_band = 90.0 - (0.95 * 80f32.to_radians().tan()).atan().to_degrees();
        assert!(Env::new(&studio_params(vec![wide.clone()])).radiance(dir_from_az_el(90.0, on_band)) != Vec3f::default());
        for (name, light) in [("a small ring light", ring(30.0, 0.9)), ("nearly a disc", ring(60.0, 0.1))] {
            assert!(!spreads_too_wide_to_be_key(&light), "{name}");
            let key = Studio::new(&black_studio(), &[light]).key().expect(name);
            assert!(key.validate().is_ok() && key.dir.y > 0.99, "{name}: {key:?}");
        }
        // The share is the cosine-weighted share of the ring's own light: a
        // hard circle between angles a and b from its axis gives exactly
        // (cos a + cos b) / 2, here 0.18.
        let hard = PreparedLight::new(&LightParams { softness: 0.0, ..wide.clone() }, &ImageSet::default());
        let (a, b) = ((0.9 * 80f32.to_radians().tan()).atan(), 80f32.to_radians());
        assert!((hard.ring_facing() - 0.5 * (a.cos() + b.cos())).abs() < 1.0e-4, "{}", hard.ring_facing());
        // The even 85 degree disc's share, 0.544, is a thin ring's 117 degrees across.
        assert!(!spreads_too_wide_to_be_key(&ring(116.0, 0.9)) && spreads_too_wide_to_be_key(&ring(118.0, 0.9)), "the limit lies near 117 degrees");
        // A long ring 160 by 90 lies nearer its axis along its short side, and leads.
        assert!(!spreads_too_wide_to_be_key(&LightParams { height_deg: 90.0, ..wide.clone() }), "a long ring");
    }

    /// `EnvSun`'s one meaning for a stroke key: radiance x the key cone is the
    /// stroke's whole emission as the map draws it, its round ends, soft edge,
    /// hotspot, taper and ramp included (to half a per cent: the shape core's
    /// sum comes within 0.2 % of a 2K bake for each of these), and its facing
    /// share is the map's.
    #[test]
    fn a_stroke_key_carries_its_emission_over_its_cone() {
        let mut off = Vec::new();
        for (name, light) in stroke_keys() {
            let p = studio_params(vec![light]);
            let key = Env::new(&p).sun().expect("the stroke is the key light");
            assert!(key.validate().is_ok(), "{name}: {key:?}");
            let map = Env::new(&the_key_alone(&p)).bake_par(2048, on_all_cores);
            let got = key.radiance * (std::f32::consts::TAU * (1.0 - key.cos_radius));
            let want = map_emission(&map);
            let ratio = [got.x / want.x, got.y / want.y, got.z / want.z];
            let facing = map_facing(&map, key.dir);
            if !ratio.iter().all(|r| (r - 1.0).abs() < 0.005) || (key.facing - facing).abs() > 0.01 {
                off.push(format!("{name}: emission {:.4} {:.4} {:.4}, facing {:.4} vs the map's {facing:.4}", ratio[0], ratio[1], ratio[2], key.facing));
            }
        }
        assert!(off.is_empty(), "the stroke key over its cone vs the map (r g b):\n{}", off.join("\n"));
    }

    /// N3 for a stroke key: the engine's lighting copy is the map with the stroke
    /// switched off, texel for texel, and a rect inside the stroke's covering
    /// cone (under the arch) stays in it. The arch is taken with each of its
    /// colourings: a ramped stroke takes its colour from its ramp, not from the
    /// colour the light loop hands it, so a ramped key leaves the copy only
    /// through B2's `ShapedLight::hide`.
    #[test]
    fn a_stroke_keys_lighting_copy_is_the_map_with_the_stroke_switched_off() {
        let with_ramp = |end_kelvin: Option<f32>, end_rgb: Option<[f32; 3]>| {
            let mut arch = arch_stroke();
            let stroke = arch.stroke.as_mut().expect("the arch is a stroke");
            stroke.end_kelvin = end_kelvin;
            stroke.end_rgb = end_rgb;
            arch
        };
        let arches = [
            ("the arch's brightness ramp (end_rgb)", arch_stroke()),
            ("a Kelvin ramp (end_kelvin)", with_ramp(Some(2000.0), None)),
            ("one colour all along", with_ramp(None, None)),
        ];
        for (name, arch) in arches {
            let rect = LightParams { azimuth_deg: 90.0, elevation_deg: 12.0, width_deg: 10.0, height_deg: 6.0, ..Default::default() };
            let p = studio_params(vec![arch, rect]);
            let baked = crate::envmap::bake_generated_env_map(&p, 128, on_all_cores);
            let mut off = p.clone();
            off.lights[0].enabled = false;
            assert!(baked.keyless.as_ref() == Some(&Env::new(&off).bake(128)), "{name}: the keyless copy is not the map without the stroke");
            assert!(baked.keyless.as_ref() != Some(&baked.map), "{name}: the stroke drew nothing");
        }
    }

    /// A scrim listed after the key hides it as a flag would (B5): a key wholly
    /// behind the cloth draws nothing in the map, so it hands the engine no light.
    #[test]
    fn a_key_behind_a_scrim_carries_nothing() {
        let key = LightParams { key: true, azimuth_deg: 90.0, elevation_deg: 0.0, width_deg: 20.0, height_deg: 20.0, ..Default::default() };
        let scrim = LightParams { shape: "scrim".to_string(), azimuth_deg: 90.0, elevation_deg: 0.0, width_deg: 40.0, height_deg: 40.0, softness: 0.0, ..Default::default() };
        let sun = Studio::new(&black_studio(), &[key, scrim]).key().expect("the rect is the key");
        assert_eq!(sun.radiance, Vec3f::default(), "{sun:?}");
        assert!(sun.validate().is_ok(), "{sun:?}");
    }

    /// N3 for a scrim key (B5): the engine's lighting copy loses the cloth's
    /// glow and keeps the cloth, so what the scrim hides stays hidden: the copy
    /// is the map with the scrim's colour black, not the map without the scrim.
    #[test]
    fn a_scrim_keys_lighting_copy_keeps_its_cloth() {
        let behind = LightParams { azimuth_deg: 90.0, elevation_deg: 0.0, width_deg: 30.0, height_deg: 30.0, ..Default::default() };
        let scrim = LightParams {
            key: true,
            shape: "scrim".to_string(),
            azimuth_deg: 90.0,
            elevation_deg: 0.0,
            width_deg: 40.0,
            height_deg: 40.0,
            scrim: Some(crate::ScrimParams { source_size: 0.5, frame: 0.05 }),
            ..Default::default()
        };
        let p = studio_params(vec![behind, scrim]);
        let baked = crate::envmap::bake_generated_env_map(&p, 64, on_all_cores);
        let mut dark = p.clone();
        dark.lights[1].rgb = Some([0.0; 3]);
        assert!(baked.keyless.as_ref() == Some(&Env::new(&dark).bake(64)), "the copy is the map with the glow gone and the cloth kept");
        let mut off = p.clone();
        off.lights[1].enabled = false;
        assert!(baked.keyless.as_ref() != Some(&Env::new(&off).bake(64)), "the light behind the cloth stays hidden in the copy");
    }

    #[test]
    fn a_hard_rect_keys_covering_cone_ends_at_its_corner() {
        // Square corners, no soft edge: the farthest lit point is the corner of the
        // box, tan(25) by tan(8) in the tangent plane, and the cone ends there.
        let light = LightParams { key: true, corner: 0.0, softness: 0.0, width_deg: 50.0, height_deg: 16.0, roll_deg: 25.0, ..Default::default() };
        let p = the_key_alone(&studio_params(vec![light.clone()]));
        let env = Env::new(&p);
        let key = env.sun().expect("a key light");
        let frame = light_frame(light.azimuth_deg, light.elevation_deg, light.roll_deg);
        let (tx, ty) = (25f32.to_radians().tan(), 8f32.to_radians().tan());
        let at = |x: f32, y: f32| (frame.center + frame.right * x + frame.up * y).normalize();
        let corner = at(tx * 0.999, ty * 0.999);
        assert!(luminance(env.radiance(corner)) > 0.0, "the corner is lit");
        let corner_deg = corner.dot(key.dir).acos().to_degrees();
        let cover_deg = key.cos_cover.acos().to_degrees();
        assert!(cover_deg >= corner_deg && cover_deg < corner_deg + 0.05, "cone {cover_deg} deg, corner {corner_deg} deg");
        // A hair past the box's long side the map is black.
        let wide = (frame.center + frame.right * (tx * 1.001) + frame.up * (ty * 0.999)).normalize();
        assert_eq!(env.radiance(wide), Vec3f::default());
    }

    #[test]
    fn lights_overlay_the_sky_mode_too() {
        // Sky mode (the default). Whatever the sky layers draw, a light adds its colour on top.
        let mut p = HdriParams::default();
        let dir = dir_from_az_el(60.0, 20.0);
        let without = Env::new(&p).radiance(dir);
        let light = LightParams {
            azimuth_deg: 60.0,
            elevation_deg: 20.0,
            ..Default::default()
        };
        p.lights = vec![light.clone()];
        let with = Env::new(&p).radiance(dir);
        let expected = light_color(&light);
        assert!(((with - without) - expected).length() < 1.0e-3 * luminance(expected), "{with:?} - {without:?}");
    }
}

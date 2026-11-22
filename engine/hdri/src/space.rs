//! Plan P1: lights placed in 3D (`LightParams::space`, `SpaceParams`).
//!
//! A 2D light is drawn in the gnomonic tangent plane of its direction: its
//! box's coordinates are the coordinates of a plane facing the camera at
//! distance 1. A 3D light is drawn by the same code in the coordinates of
//! its own face: the view ray meets the face's plane (its middle, normal,
//! right and up, all scaled by 1 / its distance so that the plane passes
//! through the unit direction to its middle), and the hit's offset from the
//! middle along the face's right and up is the point the shape, its soft
//! edge, hotspot, picture, gobo, grid and doors, wear and modifier read
//! (`studio::project`, the frame's `face`). Half its size over its distance
//! is the box's half extents (`shaped::half_extents`), so its shape's -1..1
//! spans its width and height in metres. Facing the camera, that is the 2D
//! light at the same direction and angular size, bit for bit in the shape's
//! own terms; turned, its outline is the face seen in perspective (a rect a
//! trapezoid, a disc an ellipse, edge-on nothing), and its soft edge, a band
//! on the face, thins with it as the outline does.
//!
//! The face is drawn from both sides: a flag or a scrim's cloth covers what
//! is behind it whichever side the camera sees. What it emits toward the
//! camera is its radiance times its beam (`beam_factor`) at the angle
//! between the face's front and the way from the face to the camera, per
//! ray; from behind that is nothing.
//!
//! Strokes and paint layers stay 2D in this phase (`LightParams::in_space`).
use makepad_draw::*;

use super::params::{LightParams, ModifierKind, RoomParams, SpaceParams, SPACE_NEAR_M};
use super::room::{Room, DEFAULT_REACH};
use super::shaped::box_x_sign;
use super::studio::{light_frame, LightFrame};
use super::{az_el_from_dir, dir_from_az_el};

/// The half-power angle of the narrowest beam (`SpaceParams::beam` 1), degrees.
pub const BEAM_NARROWEST_DEG: f32 = 5.0;
/// Where a 2D light with no distance of its own stands when it is placed in
/// 3D outside a room, metres (`space_from_2d`).
pub const SPACE_DEFAULT_DISTANCE_M: f32 = 3.0;

/// The face of `space`: `center` the unit direction to its middle, `right`
/// and `up` its own axes (unit, in its plane, `right` turned as a 2D frame's
/// is, `box_x_sign` from the camera's right), and `face` its normal pointing
/// out of its back (`right x up`), away from the camera while its front
/// looks at it. Turned from facing the camera in the order a 2D light's aim
/// turns its face (`LightFx::new` hands `Aim::new` the rolled frame): first
/// rolled in the plane facing the camera as `studio::light_frame` rolls a 2D
/// frame, then about its (rolled) up axis by the yaw toward its right, then
/// about its (rolled, yawed) right axis by the pitch, upward. So a 2D light's
/// roll and aim are this face's roll, yaw and pitch, one for one.
pub fn space_frame(space: &SpaceParams) -> LightFrame {
    let [x, y, z] = space.position_m;
    let p = vec3f(x, y, z);
    let center = p / p.length().max(SPACE_NEAR_M);
    let (azimuth_deg, elevation_deg) = az_el_from_dir(center);
    let base = light_frame(azimuth_deg, elevation_deg, space.roll_deg);
    let sx = box_x_sign();
    let (yaw, pitch) = (space.yaw_deg.to_radians(), space.pitch_deg.to_radians());
    // As `Aim::new`: facing the camera, yawed toward its right, pitched up.
    let n0 = -center;
    let seen_right = base.right * sx;
    let n1 = n0 * yaw.cos() + seen_right * yaw.sin();
    let r1 = seen_right * yaw.cos() - n0 * yaw.sin();
    let front = n1 * pitch.cos() + base.up * pitch.sin();
    let up = base.up * pitch.cos() - n1 * pitch.sin();
    LightFrame { center, right: r1 * sx, up, face: Some(-front) }
}

/// The face's tangent half extents: half its width and height over its
/// distance, the face's own coordinates (`space_frame`) at its edge.
pub fn space_half(space: &SpaceParams) -> Vec2f {
    let d = space.distance().max(SPACE_NEAR_M);
    vec2f(0.5 * space.width_m / d, 0.5 * space.height_m / d)
}

/// The exponent k of a beam's falloff cos(θ)^k (`SpaceParams::beam`, held
/// to 0..=1 here): 0 for a diffuse face, else the k at which the radiance
/// halves at the half-power angle, which runs evenly from 90 degrees at
/// beam 0 to BEAM_NARROWEST_DEG at 1. Beam 0.5 halves at 47.5 degrees (k
/// 1.8), 0.8 at 22 (k 8.9), 1 at 5 (k 182).
pub fn beam_exponent(beam: f32) -> f32 {
    let b = beam.max(0.0).min(1.0);
    if !(b > 0.0) {
        return 0.0;
    }
    let half = (90.0 + (BEAM_NARROWEST_DEG - 90.0) * b).to_radians();
    std::f32::consts::LN_2 / -half.cos().ln()
}

/// What of a face's radiance leaves it at `cos`, the cosine between its
/// front and the way out (to the camera, or to a point of the room): 1 for a
/// diffuse face (k 0) anywhere in front of it, cos^k for a spot, 0 behind it.
pub fn beam_factor(k: f32, cos: f32) -> f32 {
    if !(cos > 0.0) {
        0.0
    } else if k == 0.0 {
        1.0
    } else {
        cos.powf(k)
    }
}

/// The beam a template gives a 3D light (`modifier::apply_template`): a
/// diffuser (softbox, strip box, octabox, LED panel, a white umbrella, a
/// ring light, a window) is diffuse, a silver umbrella a little narrower,
/// a beauty dish narrower still and a fresnel spot narrowest; a bare bulb
/// glows all round (diffuse, facing the camera).
pub fn template_beam(kind: ModifierKind) -> f32 {
    match kind {
        ModifierKind::SilverUmbrella => 0.15,
        ModifierKind::BeautyDish => 0.45,
        ModifierKind::Fresnel => 0.75,
        _ => 0.0,
    }
}

/// How far from the camera a 2D light stands: its `distance_m` (0.3 m or
/// more), else in `room` DEFAULT_REACH of the way to the surface it faces
/// (and short of it), as the room stands it (`Studio::build_room`), else
/// SPACE_DEFAULT_DISTANCE_M.
pub fn distance_2d(light: &LightParams, room: Option<&RoomParams>) -> f32 {
    let dir = dir_from_az_el(light.azimuth_deg, light.elevation_deg);
    let own = light.distance_m.filter(|d| *d >= 0.3);
    match room {
        Some(room) => {
            let to_surface = Room::new(&room.clamped()).reach(dir);
            own.unwrap_or(DEFAULT_REACH * to_surface).min(0.95 * to_surface).max(0.3)
        }
        None => own.unwrap_or(SPACE_DEFAULT_DISTANCE_M),
    }
}

/// The 3D placement of the 2D light `light` (as clamp() holds it, read as
/// it is): its middle along its direction at `distance_2d`, as wide and
/// high in metres as its angular size spans there, its face facing the
/// camera and rolled as it is, its beam its template's (`template_beam`).
/// Drawn, it shows the light as it was: the same outline at the same size.
/// An aim's yaw and pitch are kept on the aim (a 3D light does not read
/// them) and come back with `to_2d` while the face is left facing the
/// camera.
///
/// Straight up or down a direction has no azimuth (`az_el_from_dir` reads
/// 0 there), while a 2D frame at a pole takes its turn in its plane from
/// the light's azimuth (`studio::light_frame`): that turn goes into the
/// roll, so the face keeps its turn.
pub fn space_from_2d(light: &LightParams, room: Option<&RoomParams>) -> SpaceParams {
    let d = distance_2d(light, room);
    let p = dir_from_az_el(light.azimuth_deg, light.elevation_deg) * d;
    let size = |deg: f32| 2.0 * d * (0.5 * deg).to_radians().tan();
    let kind = light.modifier.as_ref().map_or(ModifierKind::None, |m| m.kind());
    let (seen_az, seen_el) = az_el_from_dir(p);
    let mut roll_deg = light.roll_deg;
    if seen_el.abs() == 90.0 {
        // The 2D frame's right against the one the 3D face is built on: the roll between them.
        let flat = light_frame(light.azimuth_deg, light.elevation_deg, 0.0);
        let seen = light_frame(seen_az, seen_el, 0.0);
        let turn = (-flat.right.dot(seen.up)).atan2(flat.right.dot(seen.right)).to_degrees();
        roll_deg = (roll_deg + turn + 180.0).rem_euclid(360.0) - 180.0;
    }
    SpaceParams {
        position_m: [p.x, p.y, p.z],
        yaw_deg: 0.0,
        pitch_deg: 0.0,
        roll_deg,
        width_m: size(light.width_deg),
        height_m: size(light.height_deg),
        beam: template_beam(kind),
    }
}

/// Places `light` in 3D (`space_from_2d`); one placed already keeps its place.
pub fn to_3d(light: &mut LightParams, room: Option<&RoomParams>) {
    if light.space.is_none() {
        light.space = Some(space_from_2d(light, room));
    }
}

/// Writes the 2D placement of `space`'s face into `light`: its direction,
/// angular size (held to 0.1 degrees and the light's span) and roll as the
/// camera sees the face facing it. The roll is the face's own: it turns
/// first (`space_frame`), as a 2D light's roll does under its aim.
fn write_flat(light: &mut LightParams, space: &SpaceParams) {
    let d = space.distance().max(SPACE_NEAR_M);
    let [x, y, z] = space.position_m;
    let (azimuth_deg, elevation_deg) = az_el_from_dir(vec3f(x, y, z) / d);
    light.azimuth_deg = azimuth_deg;
    light.elevation_deg = elevation_deg;
    let span = light.max_span_deg();
    light.width_deg = (2.0 * (0.5 * space.width_m / d).atan()).to_degrees().max(0.1).min(span);
    light.height_deg = (2.0 * (0.5 * space.height_m / d).atan()).to_degrees().max(0.1).min(span);
    light.roll_deg = space.roll_deg;
}

/// Keeps a 3D light's 2D placement (azimuth, elevation, width, height and
/// roll, which it does not read) on its face as `to_2d` would set them, so a
/// light that stops reading its block (changed to a stroke, a caster or a
/// bolt) and the assistant's summary of it find it where it stands.
/// clamp() calls it, and changes nothing it reports: these fields follow
/// the face. Its distance and its aim are left to `to_2d`. Per clamp, one
/// direction's azimuth and elevation and two arc tangents a 3D light.
pub fn follow_face(light: &mut LightParams) {
    if let Some(space) = light.in_space().cloned() {
        write_flat(light, &space);
    }
}

/// Takes `light` back to 2D: its direction, angular size and roll as the
/// camera sees its face facing it (`write_flat`), and its distance as
/// `distance_m` (held to its 60 m). A light that stood at the automatic
/// distance (`distance_m` None, `distance_2d` with `room` as `to_3d` had it)
/// and still stands there keeps it automatic. A turned face's turn becomes
/// its aim's yaw and pitch (held to their 90 degrees: a face turned away
/// cannot be told in 2D), shown at its full size as an aimed 2D light is;
/// a face facing the camera leaves the aim's own turn as it was. Its beam
/// is dropped. A light not in 3D is left as it is.
pub fn to_2d(light: &mut LightParams, room: Option<&RoomParams>) {
    let Some(space) = light.space.take() else { return };
    let d = space.distance().max(SPACE_NEAR_M);
    write_flat(light, &space);
    // Automatic as `distance_2d` reads it: none, or under 0.3 m. Kept as it was while it holds.
    let automatic = light.distance_m.filter(|own| *own >= 0.3).is_none() && {
        let rule = distance_2d(light, room);
        (d - rule).abs() <= 1.0e-4 * rule
    };
    if !automatic {
        light.distance_m = Some(d.min(60.0));
    }
    let (yaw_deg, pitch_deg) = (space.yaw_deg.max(-90.0).min(90.0), space.pitch_deg);
    if yaw_deg != 0.0 || pitch_deg != 0.0 {
        let aim = light.aim.get_or_insert_with(Default::default);
        aim.yaw_deg = yaw_deg;
        aim.pitch_deg = pitch_deg;
    }
}

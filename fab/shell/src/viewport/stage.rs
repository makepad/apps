//! The one-frame `World` consumed by Fab's realtime viewport.
//!
//! The plate scene carries the bed and the parts, so this stage has no
//! terrain or extra entities. Both studios use the HDRI generator's Product
//! table lights for fill and reflections, with their key beside the camera.
//! Each is baked once on first use, off the UI thread, and kept by the renderer.
//! Studio keeps a dark set and gaps for metal to reflect. Bright Studio, the
//! Electronics default, adds a dim grey set and a broad source behind the
//! camera: the board's coat mirrors it into vertical pins and shields.
//! Sun and Sky selects the engine's analytic dome, and Fab supplies only
//! the NOAA sun direction and the atmosphere.

use crate::api::*;
use crate::viewport::pack::to_render;
use makepad_hdri_engine::HdriParams;
use makepad_render::env_map::GeneratedEnvMap;
use makepad_scene::{Background, EnvSun, Environment, Fog, Ibl, IblSource, KeyReflection, SkyConfig, SunConfig, TextureRef, World};
use makepad_widgets_core::makepad_draw::*;

/// Both studios start from this product table's key, overhead scrim, strips and bounce card.
/// Studio keeps its dark set; Bright Studio adds a grey set and front scrim. Both keep the
/// same directional key, at the map's strength and aimed from beside the camera.
const STUDIO_PRESET: &str = "product_table";
/// The handle the baked dark studio is registered under with the renderer.
pub(crate) const STUDIO_ENV: TextureRef = TextureRef(0x4641_4253_5455_4449);
/// The bright studio's distinct handle; both maps stay registered after their first bake.
pub(crate) const BRIGHT_STUDIO_ENV: TextureRef = TextureRef(0x4641_4242_5249_4748);

/// The bright set's dim grey ceiling.
const BRIGHT_STUDIO_TOP: [f32; 3] = [0.1; 3];
/// The bright set's dim grey horizon.
const BRIGHT_STUDIO_HORIZON: [f32; 3] = [0.12; 3];
/// The bright set's dim grey floor.
const BRIGHT_STUDIO_FLOOR: [f32; 3] = [0.08; 3];
/// The front scrim sits behind the camera once the map's key hangs 70 degrees beside it.
const FRONT_SCRIM_AZIMUTH_DEG: f32 = 180.0;
/// The front scrim's height above the map's horizon.
const FRONT_SCRIM_ELEVATION_DEG: f32 = 30.0;
/// The broad source's horizontal span, mirrored by the board's coat into pin faces.
const FRONT_SCRIM_WIDTH_DEG: f32 = 140.0;
/// The broad source's vertical span.
const FRONT_SCRIM_HEIGHT_DEG: f32 = 60.0;
/// The front scrim's strength in exposure stops. Its fill reaches a key shadow unoccluded in
/// the raster, so it is the most that keeps the board's shadow: at 3.0 the board key-shadow
/// test reads umbra/lit 0.757 (0.799 at 3.5, against its limit of 0.8; 0.598 without it).
const FRONT_SCRIM_INTENSITY_EV: f32 = 3.0;

/// The two retained studio bakes, indexed in `SceneGpu` by this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StudioSet {
    Dark,
    Bright,
}

impl StudioSet {
    pub(crate) fn from_lighting(lighting: Lighting) -> Option<Self> {
        match lighting {
            Lighting::Studio => Some(Self::Dark),
            Lighting::BrightStudio => Some(Self::Bright),
            Lighting::SunAndSky => None,
        }
    }

    pub(crate) fn environment(self) -> TextureRef {
        match self {
            Self::Dark => STUDIO_ENV,
            Self::Bright => BRIGHT_STUDIO_ENV,
        }
    }
}

/// The studio's own keys, from its bake (the map's frame): the key light the renderer's
/// lighting copy leaves out (the directional light carries it) and the sky's own sun.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct StudioKeys {
    pub sun: Option<EnvSun>,
    pub daylight: Option<Vec3f>,
}

/// The selected studio's generator parameters. A missing preset or bounce card is a bug,
/// never another look.
pub(crate) fn studio_params(set: StudioSet) -> HdriParams {
    let mut p = makepad_hdri_engine::presets::preset(STUDIO_PRESET).unwrap_or_else(|| panic!("the HDRI generator has no '{STUDIO_PRESET}' preset"));
    if set == StudioSet::Bright {
        p.studio.top = BRIGHT_STUDIO_TOP;
        p.studio.horizon = BRIGHT_STUDIO_HORIZON;
        p.studio.floor = BRIGHT_STUDIO_FLOOR;
        let mut front = p.lights.iter().find(|light| light.name == "Bounce card")
            .unwrap_or_else(|| panic!("the HDRI generator's '{STUDIO_PRESET}' preset has no 'Bounce card' for Bright Studio"))
            .clone();
        front.name = "Front scrim".to_string();
        front.azimuth_deg = FRONT_SCRIM_AZIMUTH_DEG;
        front.elevation_deg = FRONT_SCRIM_ELEVATION_DEG;
        front.width_deg = FRONT_SCRIM_WIDTH_DEG;
        front.height_deg = FRONT_SCRIM_HEIGHT_DEG;
        front.intensity_ev = FRONT_SCRIM_INTENSITY_EV;
        p.lights.push(front);
    }
    p
}

/// The studio, baked at the generator's width for a named environment, its rows spread by
/// `run`: the map with its key, the map without it, and its keys.
pub(crate) fn bake_studio(params: &HdriParams, run: impl Fn(usize, &(dyn Fn(usize) + Sync))) -> GeneratedEnvMap {
    makepad_hdri_engine::envmap::bake_generated_env_map(params, makepad_hdri_engine::PROCEDURAL_HDRI_WIDTH, run)
}
/// Where the key hangs: turned this far from the camera about the vertical
/// (to the camera's left), so its shadow falls beside the part in view.
const STUDIO_KEY_TURN_DEG: f32 = 70.0;
/// Height above the horizon: low enough to show the shadow beside the foot.
const STUDIO_KEY_ELEVATION_DEG: f32 = 52.0;

/// The direction toward the studio key (render frame, Y up): above and to
/// the left of the camera, so it turns with every orbit and its shadow
/// stays visible beside the part. From under the bed plane it hangs under the
/// plane as it hangs over it from above: on the camera's side, lighting the
/// print's plate-facing side the view shows then.
pub(crate) fn studio_key_dir(camera: &Camera) -> Vec3f {
    let back = to_render(camera.eye) - to_render(camera.target);
    // toward the camera, level; a view straight down keeps its up as "back"
    let mut level = vec3(back.x, 0.0, back.z);
    if level.length() < 1.0e-6 {
        let up = to_render(camera.up);
        level = vec3(-up.x, 0.0, -up.z);
    }
    if level.length() < 1.0e-6 {
        level = vec3(0.0, 0.0, 1.0);
    }
    let level = level.normalize();
    let (st, ct) = STUDIO_KEY_TURN_DEG.to_radians().sin_cos();
    let turned = vec3(level.x * ct - level.z * st, 0.0, level.x * st + level.z * ct);
    let (se, ce) = STUDIO_KEY_ELEVATION_DEG.to_radians().sin_cos();
    let se = if camera.under_bed() { -se } else { se };
    vec3(turned.x * ce, se, turned.z * ce).normalize()
}

/// The turn (`Ibl::rotation_deg`) that puts the map's own key, `map_key` in the map's frame,
/// where the key light hangs: the whole studio turns with the camera, its scrim, strips
/// and bounce card keep their places round the view. `rotate_y` adds the turn to the
/// azimuth atan2(x, z).
fn studio_turn_deg(key: Vec3f, map_key: Option<Vec3f>) -> f32 {
    let Some(m) = map_key.filter(|m| m.x * m.x + m.z * m.z > 1.0e-8) else { return 0.0 };
    let turn = (key.x.atan2(key.z) - m.x.atan2(m.z)).to_degrees();
    // a turn on a grid of a tenth of a degree: an orbit rewrites one texel, never a jitter
    ((turn.rem_euclid(360.0)) * 10.0).round() / 10.0
}

/// Where the traced studio's map hangs: world directions (Fab's frame, Z up) into the baked map's
/// own frame, turned as the realtime studio turns it (`studio_turn_deg`) and at its strength
/// (`Ibl::intensity` 1), so the map's key sits where the realtime key light hangs.
pub(crate) fn studio_trace_frame(camera: &Camera, map_key: Option<EnvSun>) -> makepad_raytrace::EnvironmentFrame {
    let turn = studio_turn_deg(studio_key_dir(camera), map_key.map(|k| k.dir));
    let to_map = |d: Vec3f| makepad_render::env_map::rotate_y(to_render(d), -turn);
    let (x, y, z) = (to_map(vec3(1.0, 0.0, 0.0)), to_map(vec3(0.0, 1.0, 0.0)), to_map(vec3(0.0, 0.0, 1.0)));
    makepad_raytrace::EnvironmentFrame { world_to_map: [vec3(x.x, y.x, z.x), vec3(x.y, y.y, z.y), vec3(x.z, y.z, z.z)], strength: 1.0 }
}

fn studio_world(world: &mut World, camera: &Camera, set: StudioSet, studio: Option<StudioKeys>, map_key: Option<EnvSun>) {
    let key = studio_key_dir(camera);
    world.environment = Environment {
        // The composite paints the viewport's own backdrop over every pixel the plate
        // leaves; the renderer clears to a plain colour under it.
        background: Background::Color(vec4(0.05, 0.05, 0.05, 1.0)),
        // The studio once baked and registered: its broad key stays in the
        // sharp reflection levels, while the diffuse lighting copy remains keyless.
        ibl: studio.map(|_| Ibl { key_reflection: KeyReflection::Map, source: IblSource::Hdri(set.environment()), intensity: 1.0, rotation_deg: studio_turn_deg(key, map_key.map(|k| k.dir)) }),
        fog: Fog::None,
        sun: studio.and_then(|s| s.sun),
        daylight_sun: studio.and_then(|s| s.daylight),
    };
    world.sky = None;
    world.sun = SunConfig {
        // Fab aims the key beside the camera. Its colour and strength are left to the
        // environment's rig, which lights with the map's own key at the map's strength: a
        // colour authored here is read as a legacy one, which the HDR lane triples.
        dir: Some(key),
        ..SunConfig::default()
    };
}

/// Sun elevation below which the shadow rig is off and the sky takes over.
const NIGHT_DEG: f32 = 0.5;
/// The engine's stock outdoor density is 0.004 inverse metres.
const MAX_HAZE_DENSITY: f32 = 0.004;

pub(crate) fn haze_density(amount: f32) -> f32 {
    amount.clamp(0.0, 1.0) * MAX_HAZE_DENSITY
}

/// The frame's world. `studio`: the selected studio's keys once it is baked and registered (no
/// environment before). `map_key`: the studio map's own key (in the map's frame), once the
/// renderer has prepared it (`Renderer::ibl_sun`), which the studio turns with the camera
/// at the map's own strength.
pub(crate) fn stage_world(state: &AppState, camera: &Camera, studio: Option<StudioKeys>, map_key: Option<EnvSun>) -> World {
    let mut world = World::new();
    world.camera.target = to_render(camera.target);
    world.camera.distance = camera.distance().max(0.5);
    world.camera.fov = camera.fov_y_deg.clamp(20.0, 120.0);

    if let Some(set) = StudioSet::from_lighting(state.view.lighting_for(&state.ui)) {
        studio_world(&mut world, camera, set, studio, map_key);
        world.mark_render_dirty();
        return world;
    }
    let sky = &state.sun;
    let sun_up = sky.elevation_deg() > NIGHT_DEG;
    world.sky = Some(SkyConfig {
        fog: haze_density(sky.haze),
        turbidity: sky.turbidity,
        sky_strength: 1.0,
        sun_strength: 4.0,
        exposure_ev: sky.exposure_ev,
        ..SkyConfig::default()
    });
    world.sun = SunConfig {
        // Time selects the engine's day/night lighting and celestial frame;
        // the NOAA direction then aims the sun for the configured place.
        time_of_day: Some(sky.time_local.clamp(0.0, 24.0)),
        latitude: sky.latitude,
        dir: Some(to_render(sky.direction())),
        // The engine owns the rig — its warm low sun, its twilight and night
        // ramps, its celestial frame. Fab authors only the DAYLIGHT BALANCE:
        // a clear sky that separates a part's faces, not the soft key a
        // stylised game wants.
        color: None,
        ambient: None,
        daylight_balance: Some(SkyState::DAYLIGHT_BALANCE),
        shadow_alpha: Some(if sun_up { 0.85 } else { 0.0 }),
    };
    world.mark_render_dirty();
    world
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dated sun and sky (View › Shading › Sun and Sky).
    fn sun_state() -> AppState {
        let mut state = AppState::default();
        state.view.lighting = Lighting::SunAndSky;
        state
    }

    #[test]
    fn the_noaa_sun_turns_with_the_world() {
        let mut state = sun_state();
        state.sun.time_local = 12.0;
        state.sun.longitude = 0.0;
        state.sun.tz_offset = 0.0;
        state.sun.north_deg = 0.0;
        let world = stage_world(&state, &Camera::default(), None, None);
        let direction = world.sun.dir.expect("noon sun direction");
        assert_eq!(direction, to_render(state.sun.direction()));
        assert_eq!(world.sun.time_of_day, Some(state.sun.time_local));
        assert!(direction.y > 0.5, "{direction:?}");
        assert!(direction.z > 0.0, "{direction:?}");
    }

    #[test]
    fn night_drops_the_shadow_rig() {
        let mut state = sun_state();
        state.sun.time_local = 1.0;
        let world = stage_world(&state, &Camera::default(), None, None);
        assert!(world.sun.dir.expect("night sun direction").y < 0.0);
        assert_eq!(world.sun.shadow_alpha, Some(0.0));
    }

    #[test]
    fn stage_delegates_sky_and_lighting_to_the_engine() {
        let world = stage_world(&sun_state(), &Camera::default(), None, None);
        assert!(world.entities.is_empty());
        assert!(world.terrain.is_none());
        assert!(world.sky.is_some());
        assert_eq!(world.sun.color, None);
        assert_eq!(world.sun.ambient, None);
        // ...with one thing said out loud: the daylight is a CLEAR sky.
        assert_eq!(
            world.sun.daylight_balance,
            Some(SkyState::DAYLIGHT_BALANCE)
        );
    }

    /// The user-visible law: on a clear day the sun IS the light and the sky
    /// is the fill, not the other way round. A viewport that gets this wrong
    /// draws a correct sun direction, correct shadow maps and correct
    /// materials, and still looks overcast.
    #[test]
    fn daylight_is_a_clear_sky_and_dusk_still_has_its_floor() {
        use makepad_render::sky::luminance;
        use makepad_render::sun::resolve_sun;

        for hour in [10.0f32, 12.0, 14.0, 16.0] {
            let mut state = sun_state();
            state.sun.time_local = hour;
            let rig = resolve_sun(&stage_world(&state, &Camera::default(), None, None).sun);
            let ratio = luminance(rig.color) / luminance(rig.sky);
            println!(
                "{hour:>5.1}h: elevation={:>5.1}deg direct={:.4} fill={:.4} ratio={ratio:.2}x",
                state.sun.elevation_deg(),
                luminance(rig.color),
                luminance(rig.sky),
            );
            assert!(ratio > 8.0, "{hour}h: the sun is only {ratio:.2}x the sky");
        }
        // After sunset the balance inverts by itself: no disc, so the fill is
        // all there is — and it must still BE there.
        let mut night = sun_state();
        night.sun.time_local = 1.0;
        let rig = resolve_sun(&stage_world(&night, &Camera::default(), None, None).sun);
        assert!(luminance(rig.color) < luminance(rig.sky));
        assert!(luminance(rig.sky) > 0.05, "night fill {:?}", rig.sky);
    }

    #[test]
    fn haze_zero_is_off_and_the_default_is_a_light_touch() {
        assert_eq!(haze_density(0.0), 0.0);
        let density = haze_density(SkyState::default().haze);
        let near_blend = 1.0 - (-density * 20.0).exp();
        let far_blend = 1.0 - (-density * 120.0).exp();
        assert!(near_blend < 0.04, "haze at 20 m {near_blend}");
        assert!(far_blend > 0.12, "haze at 120 m {far_blend}");
    }

    /// Studio lighting names the built-in studio environment and aims its key from above
    /// the camera's left, so an orbit carries the key round with the view.
    #[test]
    fn the_studio_key_follows_the_camera() {
        let state = AppState::default();
        assert_eq!(state.view.lighting_for(&state.ui), Lighting::Studio, "the studio is the default");
        let camera = Camera::default();
        let keys = StudioKeys { sun: None, daylight: None };
        let world = stage_world(&state, &camera, Some(keys), None);
        let ibl = world.environment.ibl.expect("the studio environment");
        assert_eq!(ibl.source, IblSource::Hdri(STUDIO_ENV));
        assert_eq!(ibl.key_reflection, KeyReflection::Map, "the studio asks the map to draw its softbox reflection");
        let key = world.sun.dir.expect("the key's aim");
        let back = (to_render(camera.eye) - to_render(camera.target)).normalize();
        assert!(key.y > 0.7, "the key is high: {key:?}");
        assert!(key.x * back.x + key.z * back.z > 0.0, "the key is on the camera's side: {key:?} {back:?}");
        let mut turned = camera;
        turned.eye = vec3(2.0 * camera.target.x - camera.eye.x, 2.0 * camera.target.y - camera.eye.y, camera.eye.z);
        let other = stage_world(&state, &turned, Some(keys), None).sun.dir.unwrap();
        assert!((other.x + key.x).abs() < 1.0e-4 && (other.z + key.z).abs() < 1.0e-4, "half an orbit turns the key half round: {key:?} {other:?}");
    }

    /// From under the bed the studio key hangs under it, on the same side of the camera, so the
    /// print's plate-facing side is lit by the same key the top is from above.
    #[test]
    fn under_the_bed_the_studio_key_hangs_under_it() {
        let state = AppState::default();
        let keys = StudioKeys { sun: None, daylight: None };
        let above = Camera::default();
        let mut under = above;
        under.eye.z = 2.0 * above.target.z - above.eye.z;
        assert!(!above.under_bed() && under.under_bed());
        let key = |camera: &Camera| stage_world(&state, camera, Some(keys), None).sun.dir.expect("the key's aim");
        let (a, b) = (key(&above), key(&under));
        assert!(a.y > 0.7 && b.y < -0.7, "{a:?} {b:?}");
        assert!((a.x - b.x).abs() < 1e-5 && (a.z - b.z).abs() < 1e-5, "the key on the camera's side both ways: {a:?} {b:?}");
    }

    /// Both studios are built from one of the generator's presets (a missing preset or bounce
    /// card fails here, never draws as another look).
    #[test]
    fn the_studio_is_a_generator_preset() {
        studio_params(StudioSet::Dark);
        let bright = studio_params(StudioSet::Bright);
        assert!(bright.lights.iter().any(|light| light.name == "Front scrim"), "the bright studio's front scrim");
    }

    /// The product studio keeps its reflections round the camera and its key on the shadow light.
    #[test]
    fn the_product_studio_lays_out_round_the_camera() {
        use makepad_render::env_map::{dir_from_az_el, rotate_y};

        let params = studio_params(StudioSet::Dark);
        let map_key = params.lights.iter().find(|l| l.key).expect("the product studio's key");
        let m = dir_from_az_el(map_key.azimuth_deg, map_key.elevation_deg);
        let level = |dir: Vec3f| vec3(dir.x, 0.0, dir.z).normalize();
        let base = Camera::default();
        for (camera_name, orbit_deg) in [("default", 0.0f32), ("+100 degrees", 100.0), ("-140 degrees", -140.0)] {
            let mut camera = base;
            let offset = base.eye - base.target;
            let (s, c) = orbit_deg.to_radians().sin_cos();
            camera.eye = vec3(
                base.target.x + offset.x * c - offset.y * s,
                base.target.y + offset.x * s + offset.y * c,
                base.eye.z,
            );
            let key = studio_key_dir(&camera);
            let turn = studio_turn_deg(key, Some(m));
            let back = level(to_render(camera.eye) - to_render(camera.target));
            let world_dir = |name: &str| {
                let light = params.lights.iter().find(|l| l.name == name)
                    .unwrap_or_else(|| panic!("{name}, {camera_name} camera: missing light"));
                rotate_y(dir_from_az_el(light.azimuth_deg, light.elevation_deg), turn)
            };

            let world_key = world_dir("Key");
            let key_angle = world_key.dot(key).clamp(-1.0, 1.0).acos().to_degrees();
            assert!(key_angle < 1.0, "Key, {camera_name} camera: map {world_key:?} and directional {key:?} differ by {key_angle} degrees");
            let overhead = level(world_dir("Overhead scrim"));
            assert!(overhead.dot(back) < -0.3, "Overhead scrim, {camera_name} camera: {overhead:?} must point away from {back:?}");
            let bounce = level(world_dir("Bounce card"));
            assert!(bounce.dot(back) > 0.95, "Bounce card, {camera_name} camera: {bounce:?} must point toward {back:?}");
            let sides = ["Strip left", "Strip right"].map(|name| {
                let strip = level(world_dir(name));
                let angle = strip.dot(back).clamp(-1.0, 1.0).acos().to_degrees();
                assert!((80.0..=120.0).contains(&angle), "{name}, {camera_name} camera: {angle} degrees from {back:?}");
                Vec3f::cross(back, strip).y
            });
            assert!(sides[0] * sides[1] < 0.0, "Strip left and Strip right, {camera_name} camera: opposite sides required, got {sides:?}");
        }
    }

    /// The map turns so its own key lies where the key light hangs.
    #[test]
    fn the_studio_map_turns_its_key_to_the_light() {
        let key = studio_key_dir(&Camera::default());
        for map_key in [vec3(0.6, 0.5, -0.62), vec3(-0.9, 0.3, 0.3), vec3(0.0, 0.4, 0.9)] {
            let turned = makepad_render::env_map::rotate_y(map_key, studio_turn_deg(key, Some(map_key)));
            let (a, b) = (turned.x.atan2(turned.z), key.x.atan2(key.z));
            let d = (a - b).sin().abs();
            assert!(d < 0.01 && (a - b).cos() > 0.0, "{map_key:?} turned to {turned:?}, key {key:?}");
        }
    }
}

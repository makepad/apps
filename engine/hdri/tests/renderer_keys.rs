//! The generator's keys as makepad free's renderer reads them (`makepad_render::sun`): a
//! preset's key is its map's sun exactly when the sky's sun is the key, and the resolved
//! rig follows the key over an evening without a jump.

use makepad_draw::*;
use makepad_hdri_engine::{presets, Env, HdriParams};
use makepad_render::sun::{env_key_is_its_sun, env_sun_rig_with, EnvLighting, SunLight};

/// A world whose environment names a registered HDRI and carries `sun`.
fn env_world(sun: Option<makepad_scene::EnvSun>, rotation_deg: f32) -> makepad_scene::World {
    let mut world = makepad_scene::World::new();
    world.environment.ibl = Some(makepad_scene::Ibl {
        key_reflection: makepad_scene::KeyReflection::Light,
        source: makepad_scene::IblSource::Hdri(makepad_scene::TextureRef(1)),
        intensity: 1.0,
        rotation_deg,
    });
    world.environment.sun = sun;
    world
}

/// The lighting of a constant grey map at `level`.
fn grey_lighting(level: f32) -> EnvLighting {
    // 128 wide: sh9's midpoint quadrature is 0.4 % high at 32 wide
    // (E/π = 1.0036 for a constant map), 0.02 % at 128.
    let map = makepad_render_material::ibl::EnvMap::constant(128, [level, level, level]);
    EnvLighting { sh: makepad_render_material::ibl::sh9(&map), mean_luminance: level, gain: 1.0 }
}

/// N1, one sun in the sky: a key is its map's own sun exactly when the
/// generator's sun is the key (every sky preset whose sun is up, faded or
/// clouded), never the moon (keyed only once the sun is 6 degrees down)
/// and never without a report (a studio).
#[test]
fn a_key_is_its_maps_sun_only_when_the_generators_sun_is_the_key() {
    let horizon = -(1.0f32.to_radians().sin());
    for name in presets::PRESET_NAMES {
        let env = Env::new(&presets::preset(name).unwrap());
        let (key, daylight) = (env.sun(), env.sun_dir());
        let want = key.is_some() && daylight.is_some_and(|d| d.y > horizon);
        assert_eq!(env_key_is_its_sun(key, daylight), want, "{name}: {key:?} {daylight:?}");
    }
}

/// The core's key on the default params' evening, as `Env::sun` reports
/// it: 21 June at 45 N under the default clear sky, `hour` o'clock.
fn evening_key(hour: f32) -> Option<makepad_scene::EnvSun> {
    let mut p = HdriParams::default();
    p.sky.sun.hour = hour;
    Env::new(&p).sun()
}

#[test]
fn the_rig_has_no_jump_over_an_evening_and_never_falls_back_to_the_analytic_colour() {
    // Phase 2 amendment, item 3: sampled the way the core's
    // `the_key_has_no_jump_over_an_evening` samples the key (every two
    // minutes from 16:00 to 23:00) but on the resolved rig. The sun fades
    // out over a couple of minutes of sun time and the moon fades in at -6
    // degrees, so the directional colour is continuous; between the two
    // there is no key, and then the colour is zero, not the analytic rig's.
    // The bound is coarse on purpose (two minutes is as long as the sun's
    // fade; the core's elevation sweeps carry the continuity proper).
    let stock = SunLight::default().to_hdr();
    let lum = makepad_render::sky::luminance;
    assert!(lum(stock.color) > 1.0, "premise: the analytic rig has a colour");
    let lighting = grey_lighting(0.2);
    let world = env_world(None, 0.0);
    let rigs: Vec<(f32, Option<makepad_scene::EnvSun>, SunLight)> = (0..211)
        .map(|i| {
            let hour = 16.0 + i as f32 / 30.0;
            let key = evening_key(hour);
            (hour, key, env_sun_rig_with(&world, key, Some(&lighting), stock, true))
        })
        .collect();
    let peak = rigs.iter().map(|r| lum(r.2.color)).fold(0.0, f32::max);
    assert!(peak > 0.5, "a sunny afternoon lights: {peak}");
    let (mut dark, mut last, mut worst) = (0, None::<f32>, 0.0f32);
    for &(hour, key, rig) in &rigs {
        let got = lum(rig.color);
        assert!(rig.color.is_finite() && rig.dir.is_finite(), "{hour} h");
        if key.is_none() {
            dark += 1;
            assert_eq!(rig.color, Vec3f::default(), "{hour} h: no key, no direct light (the analytic colour is {:?})", stock.color);
        }
        if let Some(prev) = last {
            worst = worst.max((got - prev).abs() / peak);
            assert!((got - prev).abs() < 0.05 * peak, "{hour} h: {prev} -> {got} of a peak {peak}");
        }
        last = Some(got);
    }
    assert!(dark > 10, "the key is out for a while between sunset and the moon: {dark} samples");
    println!("largest step of the resolved rig's colour: {:.1} % of the evening's peak", worst * 100.0);
}

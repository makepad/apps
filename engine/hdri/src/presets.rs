//! Built-in presets: seven skies, seven cloud types, three weathers and the studio set-ups, as plain `HdriParams`.
//!
//! The first six studios hang their lights against the gradient backdrop. The next five stand
//! them in a room (`StudioParams::room`, room.rs) after the photographed studio HDRIs the owner
//! collected (a white cyc, a small studio in black drapes, a wooden daylight studio, a neon set,
//! a grey cyc with umbrellas), with the lights as their real modifiers (modifier.rs's templates).
//! The last five are rooms for cars: an automotive photo studio, a showroom under a hexagon LED
//! ceiling, a detailing studio under another, a road tunnel and a corridor of LED portals, their
//! lights largely pattern lights placed in 3D (pattern.rs, space.rs).
//!
//! Each preset starts from `HdriParams::default()` and sets only what makes its look, so fields
//! added later (all `Option`s) keep their defaults. Every value sits inside its documented range,
//! so a preset passes `clamp()` unchanged; a test pins that.
//!
//! Sky presets:
//! - "Clear noon" runs on Time mode, so the date and place controls show something real.
//! - The rest use a Manual sun, which pins the sun height the look depends on. Their clock and
//!   date are still set to a matching time, for anything that reads them.
//! - The weathers (the weather lane, 2026-11-09, after Physical Weather FX's named situations: names, not its
//!   numbers) are combinations of the weather kinds and the cloud layers, on the plain ground as every sky preset:
//!   after rain, overcast and wet, heavy rain, a misty winter morning and a snowy sunny day. Each kind's own cloud layer is added as its pick in the app
//!   adds it (`add_weather_clouds`), and a weather that rains breaks the water's mirror (`rain_on_water`).
//!
//! Studio azimuths assume the camera looks north (−Z, the engine's forward and the centre column
//! of the engine's equirect). Lights on the camera side therefore sit near azimuth 180, and rims
//! and back lights near 0.
//!
//! Levels are in the model's scene units (atmosphere.rs, "Radiance scale"): a clear noon zenith
//! is about half a unit (0.4-0.6) and a white wall in the noon sun about 5.3. A light at EV e
//! has radiance 2^e at luminance 1, so a key at EV 4 is 16. The backdrops stay well below that, so
//! the lights read as lights.
//!
//! These are not the engine's `ibl::EnvPreset`s (studio, softbox, sunset, overcast, night, neon,
//! gradient), which stay as they are; two names overlap and that is intended.

use makepad_draw::{vec3f, Vec3f};

use super::modifier::apply_template;
use super::pattern::{fit_face, hold_face, lattice_m, natural_size_m};
use super::shaped::box_x_sign;
use super::space::{follow_face, template_beam};
use super::studio::light_frame;
use super::{add_weather_clouds, az_el_from_dir, AltocumulusParams, CloudGenus, CloudLayerParams, CloudStackParams, CumulonimbusParams, CumulusParams, FloorParams, GlowParams, GoboParams, HdriParams, LightParams, ModifierKind, PatternParams, PatternSvg, RoomParams, RoomSides, ScrimParams, SpaceParams, StratocumulusParams, StrokeParams, WeatherParams};

pub const PRESET_NAMES: [&str; 42] = [
    "Clear noon",
    "Golden hour",
    "Sunset",
    "Overcast",
    "Blue hour",
    "Moonlit night",
    "Starry night",
    "Stratocumulus low sun",
    "Stratocumulus high sun",
    "Altocumulus",
    "Cumulus",
    "Stratus",
    "Cirrus",
    "Cirrostratus",
    "After rain",
    "Overcast and wet",
    "Heavy rain",
    "Misty winter morning",
    "Snowy sunny day",
    "After the shower",
    "Three-point",
    "Top softbox",
    "Rim pair",
    "Overcast dome",
    "Ring light",
    "Product table",
    "White cyc",
    "Small dark studio",
    "Wooden daylight",
    "Neon studio",
    "Umbrella cyc",
    "Car studio",
    "Car showroom",
    "Hexagon studio",
    "Road tunnel",
    "Bridge",
    "Bridge at night",
    "LED portal",
    "Open boxes",
    "Umbrellas",
    "Open loft",
    "Factory hall",
];

/// A built-in preset by name, or None for an unknown name. Case, surrounding space, and '-'
/// versus '_' versus ' ' do not matter, so "three_point" and "THREE POINT" both find "Three-point".
pub fn preset(name: &str) -> Option<HdriParams> {
    let wanted = normalise(name);
    let canonical = PRESET_NAMES.iter().copied().find(|known| normalise(known) == wanted)?;
    Some(match canonical {
        "Clear noon" => clear_noon(),
        "Golden hour" => golden_hour(),
        "Sunset" => sunset(),
        "Overcast" => overcast(),
        "Blue hour" => blue_hour(),
        "Moonlit night" => moonlit_night(),
        "Starry night" => starry_night(),
        "Stratocumulus low sun" => stratocumulus_low_sun(),
        "Stratocumulus high sun" => stratocumulus_high_sun(),
        "Altocumulus" => altocumulus(),
        "Cumulus" => cumulus(),
        "Stratus" => stratus(),
        "Cirrus" => cirrus(),
        "Cirrostratus" => cirrostratus(),
        "After rain" => after_rain(),
        "Overcast and wet" => overcast_and_wet(),
        "Heavy rain" => heavy_rain(),
        "Misty winter morning" => misty_winter_morning(),
        "Snowy sunny day" => snowy_sunny_day(),
        "After the shower" => after_the_shower(),
        "Three-point" => three_point(),
        "Top softbox" => top_softbox(),
        "Rim pair" => rim_pair(),
        "Overcast dome" => overcast_dome(),
        "Ring light" => ring_light(),
        "Product table" => product_table(),
        "White cyc" => white_cyc(),
        "Small dark studio" => small_dark_studio(),
        "Wooden daylight" => wooden_daylight(),
        "Neon studio" => neon_studio(),
        "Umbrella cyc" => umbrella_cyc(),
        "Car studio" => car_studio(),
        "Car showroom" => car_showroom(),
        "Hexagon studio" => hexagon_studio(),
        "Road tunnel" => road_tunnel(),
        "Bridge" => bridge(),
        "Bridge at night" => bridge_at_night(),
        "LED portal" => led_portal(),
        "Open boxes" => open_boxes(),
        "Umbrellas" => umbrellas(),
        "Open loft" => open_loft(),
        "Factory hall" => factory_hall(),
        _ => return None,
    })
}

fn normalise(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c.to_ascii_lowercase() })
        .collect()
}

/// A Sky-mode starting point with a manual sun. `month`, `day` and `hour` are set to a time that
/// matches the sun at 45°N, so switching to Time mode lands somewhere sensible.
fn sky(seed: u32, elevation_deg: f32, azimuth_deg: f32, month: u32, day: u32, hour: f32) -> HdriParams {
    let mut p = HdriParams {
        mode: "sky".to_string(),
        seed,
        ..Default::default()
    };
    let sun = &mut p.sky.sun;
    sun.mode = "manual".to_string();
    sun.elevation_deg = elevation_deg;
    sun.azimuth_deg = azimuth_deg;
    sun.year = 2026;
    sun.month = month;
    sun.day = day;
    sun.hour = hour;
    sun.tz_offset = 0.0;
    sun.latitude = 45.0;
    sun.longitude = 0.0;
    p
}

/// Midsummer noon at 45°N on the prime meridian: the sun about 68° up, due south, in air a little
/// clearer than the default, with a trace of cirrus for texture.
fn clear_noon() -> HdriParams {
    let mut p = sky(1, 68.4, 180.0, 6, 21, 12.0);
    p.sky.sun.mode = "time".to_string();
    p.sky.atmosphere.haze = 0.8;
    p.sky.clouds.coverage = 0.0;
    p.sky.clouds.cirrus = 0.15;
    p
}

/// A summer evening with the sun 6° up in the west-northwest: long warm light, a little evening
/// haze, scattered cumulus, and cirrus to catch the colour.
fn golden_hour() -> HdriParams {
    let mut p = sky(2, 6.0, 295.0, 6, 21, 19.0);
    p.sky.atmosphere.haze = 1.5;
    p.sky.sun_disc.softness = 0.25;
    p.sky.clouds.coverage = 0.15;
    p.sky.clouds.sharpness = 0.6;
    p.sky.clouds.scale = 1.2;
    p.sky.clouds.altitude_m = 3000.0;
    p.sky.clouds.cirrus = 0.35;
    p
}

/// The sun on the horizon: heavy haze reddens it, and broken cloud and cirrus light up from below.
fn sunset() -> HdriParams {
    let mut p = sky(3, 0.8, 302.0, 6, 21, 19.7);
    p.sky.atmosphere.haze = 2.2;
    p.sky.sun_disc.softness = 0.3;
    p.sky.clouds.coverage = 0.3;
    p.sky.clouds.sharpness = 0.55;
    p.sky.clouds.scale = 1.5;
    p.sky.clouds.altitude_m = 2500.0;
    p.sky.clouds.cirrus = 0.5;
    p
}

/// A full, soft, low cloud deck with the sun somewhere behind it: even light over dim, damp ground.
fn overcast() -> HdriParams {
    let mut p = sky(7, 40.0, 160.0, 3, 13, 11.1);
    p.sky.atmosphere.haze = 3.0;
    p.sky.atmosphere.ground_color = [0.12, 0.13, 0.11];
    p.sky.clouds.coverage = 1.0;
    p.sky.clouds.sharpness = 0.1;
    p.sky.clouds.scale = 1.5;
    p.sky.clouds.altitude_m = 1500.0;
    p.sky.clouds.cirrus = 0.0;
    p
}

/// Civil twilight, with the sun 5° below the horizon: the deep blue after sunset, the first
/// stars, and no moon.
fn blue_hour() -> HdriParams {
    let mut p = sky(4, -5.0, 308.0, 6, 21, 20.3);
    p.sky.atmosphere.haze = 1.2;
    p.sky.clouds.coverage = 0.1;
    p.sky.clouds.cirrus = 0.2;
    let night = &mut p.sky.night;
    night.moon = false;
    night.stars = 0.25;
    night.star_brightness = 0.6;
    night.glow_strength = 1.0;
    p
}

/// A winter evening with the sun 30° under the west-northwest horizon and the moon 30° up
/// opposite it, in the east-southeast. Sitting opposite the sun is what makes the moon nearly
/// full. Moonlight washes out the faint stars, so the star field is sparse, and a few thin clouds
/// catch the moon.
fn moonlit_night() -> HdriParams {
    let mut p = sky(5, -30.0, 285.0, 2, 19, 20.3);
    p.sky.atmosphere.haze = 1.0;
    p.sky.clouds.coverage = 0.2;
    p.sky.clouds.sharpness = 0.5;
    p.sky.clouds.altitude_m = 2500.0;
    let night = &mut p.sky.night;
    night.moon = true;
    night.moon_elevation_deg = 30.0;
    night.moon_azimuth_deg = 105.0;
    night.moon_size_deg = 0.52;
    night.moon_brightness = 1.5;
    night.stars = 0.35;
    night.star_brightness = 0.8;
    night.glow_strength = 0.8;
    p
}

/// A dark, dry, moonless winter night far from town: dense bright stars and a faint airglow.
fn starry_night() -> HdriParams {
    let mut p = sky(6, -50.0, 330.0, 2, 26, 22.9);
    p.sky.atmosphere.haze = 0.6;
    p.sky.clouds.coverage = 0.0;
    p.sky.clouds.cirrus = 0.0;
    let night = &mut p.sky.night;
    night.moon = false;
    night.stars = 1.0;
    night.star_brightness = 2.0;
    night.glow_strength = 1.5;
    p
}


/// One cloud type alone in the sky (2026-11-07): Clear noon's air (haze 0.8, the haze the kept
/// looks were matched at) with no older 2D cloud, a manual sun in the east-southeast at
/// `elevation_deg`, and one layer of `genus` at its defaults, `cover` its share of the sky (None:
/// the type's own), then `rows` sets the rows that make the look. The clock is set to a time that
/// puts the sun there at 45°N.
fn cloud_sky(seed: u32, genus: CloudGenus, elevation_deg: f32, month: u32, day: u32, hour: f32, cover: Option<f32>, rows: impl FnOnce(&mut CloudLayerParams)) -> HdriParams {
    let mut p = sky(seed, elevation_deg, 124.0, month, day, hour);
    p.sky.atmosphere.haze = 0.8;
    p.sky.clouds.coverage = 0.0;
    p.sky.clouds.cirrus = 0.0;
    let mut layer = CloudLayerParams::new(genus, &[], seed);
    if cover.is_some() {
        layer.cover = cover;
    }
    rows(&mut layer);
    p.sky.cloud_stack = Some(CloudStackParams { cover: 1.0, layers: vec![layer], ..CloudStackParams::default() });
    p
}

/// The Stratocumulus' low-sun look (the look runner's round 50): the turbulence deck (Billows
/// under 0.5) 600 m deep, warm light raking its underside.
fn stratocumulus_low_sun() -> HdriParams {
    cloud_sky(31, CloudGenus::Stratocumulus, 8.0, 2, 1, 8.3, Some(0.85), |l| {
        let b = l.stratocumulus.get_or_insert_with(StratocumulusParams::default);
        b.tops_m = 1800.0;
        b.billows = 0.2;
    })
}

/// The Stratocumulus' high-sun look (round 22): the cell deck (Billows 0.5), its rounded lumps
/// with dark crevices between.
fn stratocumulus_high_sun() -> HdriParams {
    cloud_sky(32, CloudGenus::Stratocumulus, 40.0, 9, 1, 9.3, Some(0.8), |l| {
        l.stratocumulus.get_or_insert_with(StratocumulusParams::default).tops_m = 1500.0;
    })
}

/// The Altocumulus' kept look (ac2): small rounded cloudlets in rows at 3.5 km, sunlit edges.
fn altocumulus() -> HdriParams {
    cloud_sky(33, CloudGenus::Altocumulus, 30.0, 9, 22, 8.8, Some(0.42), |l| {
        l.altocumulus.get_or_insert_with(AltocumulusParams::default).base_m = 3500.0;
    })
}

/// The Cumulus' kept look (cu3): separate heaps at Size 1.5 km, white on the sunny sides.
fn cumulus() -> HdriParams {
    cloud_sky(34, CloudGenus::Cumulus, 45.0, 8, 22, 9.6, Some(0.1), |l| {
        l.cumulus.get_or_insert_with(CumulusParams::default).tops_m = 2400.0;
    })
}

/// A closed Stratus sheet (its new-layer Cover), the sun's glow through it.
fn stratus() -> HdriParams {
    cloud_sky(35, CloudGenus::Stratus, 35.0, 4, 1, 9.2, None, |_| {})
}

/// Cirrus: wind-combed streaks with hooked ends, high up.
fn cirrus() -> HdriParams {
    cloud_sky(37, CloudGenus::Cirrus, 35.0, 4, 1, 9.2, Some(0.35), |_| {})
}

/// Cirrostratus: a thin milky veil of ice over the blue.
fn cirrostratus() -> HdriParams {
    cloud_sky(38, CloudGenus::Cirrostratus, 35.0, 4, 1, 9.2, Some(0.9), |_| {})
}

/// A weather over a Manual sky (the weather lane, 2026-11-09): `weather`'s kind adds its own cloud layer as a pick in the
/// app adds it (`add_weather_clouds`, onto the sky without older clouds), then the weather is written. Like every sky
/// preset it brings no floor: its wet ground, puddles and snow lie on the plain ground, or on a floor added after.
fn weathered(mut p: HdriParams, weather: WeatherParams) -> HdriParams {
    p.sky.clouds.coverage = 0.0;
    p.sky.clouds.cirrus = 0.0;
    add_weather_clouds(&mut p, weather.kind());
    p.sky.weather = Some(weather);
    p
}

/// One more cloud layer of `genus` at Cover `cover` over the ones `p` has.
fn with_layer(mut p: HdriParams, genus: CloudGenus, cover: f32) -> HdriParams {
    let seed = p.seed;
    let stack = p.sky.cloud_stack.get_or_insert_with(|| CloudStackParams { cover: 1.0, ..CloudStackParams::default() });
    let mut layer = CloudLayerParams::new(genus, &stack.layers, seed);
    layer.cover = Some(cover);
    stack.layers.push(layer);
    p
}

/// After the rain (Weather FX's "after rain, clearing"): the sun back out at 28 degrees in the afternoon over ground
/// still wet, its hollows holding calm puddles that mirror the clearing sky; the air washed clear (haze 0.6) and a few
/// cumulus left.
fn after_rain() -> HdriParams {
    let p = weathered(sky(41, 28.0, 230.0, 10, 1, 14.65), WeatherParams { wet_ground: 0.6, ..WeatherParams::default() });
    let mut p = with_layer(p, CloudGenus::Cumulus, 0.3);
    p.sky.atmosphere.haze = 0.6;
    p
}

/// Overcast and wet, no rain (Weather FX's "overcast, wet, no rain"): the Overcast kind's grey deck over ground that
/// rained a while ago, its puddles mirroring the deck.
fn overcast_and_wet() -> HdriParams {
    weathered(sky(42, 30.0, 200.0, 2, 15, 13.4), WeatherParams { kind: "overcast".to_string(), wet_ground: 0.7, ..WeatherParams::default() })
}

/// Heavy rain (Weather FX's "heavy rain, misty"): the Rain kind at its heaviest (10.7 mm/h) with a mist, its puddles
/// broken by the rain into a sheen (`rain_on_water`).
fn heavy_rain() -> HdriParams {
    weathered(sky(43, 22.0, 200.0, 11, 22, 13.1), WeatherParams { kind: "rain".to_string(), rain: 1.0, fog: 0.25, rain_on_water: Some(true), ..WeatherParams::default() })
}

/// A misty winter morning (Weather FX's "misty winter morning"): snow on the ground and a mist over it, the sun just up
/// in the south-east, warm through the mist toward it and grey away.
fn misty_winter_morning() -> HdriParams {
    let mut p = weathered(sky(44, 3.0, 125.0, 12, 1, 7.7), WeatherParams { fog: 0.4, snow: true, ..WeatherParams::default() });
    p.sky.atmosphere.haze = 1.2;
    p
}

/// A snowy sunny day (Weather FX's "winter snowy sunny day"): fresh snow under a clear winter sky, the sun 19 degrees
/// up before noon, a few small cumulus.
fn snowy_sunny_day() -> HdriParams {
    let p = weathered(sky(45, 19.0, 160.0, 12, 22, 10.6), WeatherParams { snow: true, ..WeatherParams::default() });
    let mut p = with_layer(p, CloudGenus::Cumulus, 0.12);
    p.sky.atmosphere.haze = 0.7;
    p
}

/// After the shower: the shower that passed is a wide Cumulonimbus whose near edge stands over the viewer, 9 km off to
/// its middle and 18 km across, opposite a morning sun 15 degrees up; its dark base, 2.2 km up, fills the sky over the
/// bow as in photos of showers, its rain falls under the whole base to the ground, the wind carrying it away from the
/// sun, and the whole primary bow stands in that grey curtain from foot to foot with its secondary faint above it; the
/// sky toward the sun clear with a few cumulus, the ground still wet with calm puddles that mirror the bow (no rain falls
/// here any more). A full arc needs that: at a sun of 15 the bow's feet stand about 40 degrees to either side of the
/// antisolar point, so the rain must reach that far at its distance (round 3's storm, 9 km off and 10 km wide, held only
/// the bow's crest, and the white tower over it read nothing like the photos). Its anvil is small (an anvil reaching
/// over the viewer toward the sun read as a hairy disc).
fn after_the_shower() -> HdriParams {
    let p = weathered(sky(34, 15.0, 90.0, 4, 17, 6.71), WeatherParams { wet_ground: 0.8, ..WeatherParams::default() });
    let p = with_layer(p, CloudGenus::Cumulus, 0.1);
    let mut p = with_layer(p, CloudGenus::Cumulonimbus, 1.0);
    if let Some(layer) = p.sky.cloud_stack.as_mut().and_then(|stack| stack.layers.iter_mut().find(|l| l.genus() == Some(CloudGenus::Cumulus))) {
        layer.cumulus.get_or_insert_with(CumulusParams::default).tops_m = 2400.0;
    }
    if let Some(layer) = p.sky.cloud_stack.as_mut().and_then(|stack| stack.layers.last_mut()) {
        layer.wind_deg = Some(270.0);
        let storm = layer.cumulonimbus.get_or_insert_with(CumulonimbusParams::default);
        (storm.azimuth_deg, storm.distance_km, storm.tower_km, storm.base_m, storm.tops_m, storm.anvil, storm.rain_shaft) = (270.0, 9.0, 18.0, 2200.0, 11_000.0, 0.15, 1.0);
    }
    p.sky.atmosphere.haze = 0.8;
    p
}

/// A Studio-mode starting point: the backdrop gradient (linear radiance) and its light list.
fn studio(seed: u32, top: [f32; 3], horizon: [f32; 3], floor: [f32; 3], horizon_softness: f32, lights: Vec<LightParams>) -> HdriParams {
    let mut p = HdriParams {
        mode: "studio".to_string(),
        seed,
        ..Default::default()
    };
    p.studio.top = top;
    p.studio.horizon = horizon;
    p.studio.floor = floor;
    p.studio.horizon_softness = horizon_softness;
    p.lights = lights;
    p
}

/// One light: its shape, placement and size in degrees, level in EV, and colour temperature.
/// Everything else starts from `LightParams::default()`.
#[allow(clippy::too_many_arguments)]
fn light(name: &str, shape: &str, azimuth_deg: f32, elevation_deg: f32, width_deg: f32, height_deg: f32, intensity_ev: f32, kelvin: f32) -> LightParams {
    LightParams {
        name: name.to_string(),
        shape: shape.to_string(),
        azimuth_deg,
        elevation_deg,
        width_deg,
        height_deg,
        intensity_ev,
        kelvin,
        ..LightParams::default()
    }
}

/// Edge and falloff: corner rounding (rect only), edge softness and the centre hotspot.
fn look(mut light: LightParams, corner: f32, softness: f32, hotspot: f32) -> LightParams {
    light.corner = corner;
    light.softness = softness;
    light.hotspot = hotspot;
    light
}

/// Marks the light the engine uses as its sun.
fn as_key(mut light: LightParams) -> LightParams {
    light.key = true;
    light
}

/// Classic portrait lighting on a dark grey set:
/// - a large key 45° to camera right and 30° up;
/// - a broad fill low on the left, 2.5 stops under the key (about 3.4:1 once its larger area is
///   counted);
/// - a narrow strip rim behind the subject.
fn three_point() -> HdriParams {
    studio(8, [0.02, 0.02, 0.022], [0.05, 0.05, 0.05], [0.03, 0.03, 0.03], 0.3, vec![
        as_key(look(light("Key", "rect", 135.0, 30.0, 40.0, 30.0, 4.0, 5600.0), 0.15, 0.3, 0.2)),
        look(light("Fill", "rect", 230.0, 15.0, 50.0, 40.0, 1.5, 5600.0), 0.25, 0.6, 0.0),
        look(light("Rim", "rect", 20.0, 25.0, 8.0, 50.0, 4.0, 6500.0), 0.1, 0.2, 0.1),
    ])
}

/// Product lighting:
/// - a big softbox overhead, tipped toward the camera;
/// - a low white bounce card in front;
/// - a thin kicker strip behind on the left for an edge highlight.
fn top_softbox() -> HdriParams {
    studio(9, [0.03, 0.03, 0.03], [0.12, 0.12, 0.12], [0.08, 0.08, 0.08], 0.25, vec![
        as_key(look(light("Top softbox", "rect", 180.0, 75.0, 70.0, 45.0, 3.5, 5500.0), 0.1, 0.35, 0.15)),
        look(light("Bounce card", "rect", 180.0, 5.0, 70.0, 15.0, -0.5, 5500.0), 0.3, 0.8, 0.0),
        look(light("Kicker", "rect", 300.0, 20.0, 6.0, 45.0, 2.5, 6500.0), 0.1, 0.2, 0.0),
    ])
}

/// Two tall strips behind the subject, 40° either side, carve both edges out of a near-black set.
/// A soft frontal fill, 5.5 stops down, keeps the face from going to black.
fn rim_pair() -> HdriParams {
    studio(10, [0.005, 0.005, 0.006], [0.015, 0.015, 0.015], [0.01, 0.01, 0.01], 0.2, vec![
        as_key(look(light("Rim left", "rect", 320.0, 15.0, 10.0, 70.0, 4.5, 6500.0), 0.1, 0.25, 0.1)),
        look(light("Rim right", "rect", 40.0, 15.0, 10.0, 70.0, 4.5, 6500.0), 0.1, 0.25, 0.1),
        look(light("Front fill", "rect", 180.0, 10.0, 40.0, 30.0, -1.0, 5600.0), 0.3, 0.8, 0.0),
    ])
}

/// A huge diffuse dome overhead and two broad side panels on a pale backdrop: the shadowless,
/// even light of a light tent or a bright cloudy day.
fn overcast_dome() -> HdriParams {
    studio(11, [0.8, 0.82, 0.85], [0.6, 0.6, 0.6], [0.2, 0.19, 0.18], 0.6, vec![
        as_key(look(light("Dome", "disc", 0.0, 85.0, 110.0, 110.0, 0.5, 7000.0), 0.0, 1.0, 0.0)),
        look(light("Side east", "rect", 90.0, 20.0, 80.0, 50.0, -1.5, 6500.0), 0.5, 1.0, 0.0),
        look(light("Side west", "rect", 270.0, 20.0, 80.0, 50.0, -2.0, 6500.0), 0.5, 1.0, 0.0),
    ])
}

/// A beauty ring light on the camera axis (the dark centre is where the lens looks through), with
/// a warm soft wash on the background behind the subject.
fn ring_light() -> HdriParams {
    let mut ring = as_key(look(light("Ring", "ring", 180.0, 0.0, 24.0, 24.0, 4.5, 5600.0), 0.0, 0.15, 0.0));
    ring.inner = 0.75;
    studio(12, [0.04, 0.04, 0.045], [0.1, 0.1, 0.1], [0.06, 0.06, 0.06], 0.2, vec![
        ring,
        look(light("Background", "rect", 0.0, 10.0, 70.0, 30.0, -0.5, 3200.0), 0.2, 0.9, 0.0),
    ])
}

/// Product photography for small metal and plastic objects on a table: a dark set keeps
/// gaps between the lights dark, so metal reflects both bright shapes and dark breaks.
/// - "Key": a broad softbox 70° beside the camera and 52° up models plastic faces and gives
///   metal a broad, bright reflection;
/// - "Overhead scrim": flat tops mirror the far side at the camera's elevation, a top-down
///   view mirrors the zenith; the cloth's glow is graded, so a wide flat shield shows a
///   gradient, not a flat white;
/// - "Strip left" and "Strip right": tall, low strips about 100° either side of the camera
///   draw edge lines down cans, pins and lead flanks;
/// - "Bounce card": a dim, broad card low in front lifts plastic faces and gives metal a
///   soft frontal reflection without filling the dark gaps.
fn product_table() -> HdriParams {
    let overhead = LightParams {
        name: "Overhead scrim".to_string(),
        shape: "scrim".to_string(),
        azimuth_deg: 0.0,
        elevation_deg: 70.0,
        width_deg: 100.0,
        height_deg: 70.0,
        intensity_ev: 2.0,
        kelvin: 6000.0,
        scrim: Some(ScrimParams { source_size: 0.6, frame: 0.02 }),
        ..LightParams::default()
    };
    studio(24, [0.025; 3], [0.03; 3], [0.02; 3], 0.25, vec![
        as_key(look(light("Key", "rect", 250.0, 52.0, 45.0, 35.0, 5.2, 5600.0), 0.15, 0.35, 0.2)),
        overhead,
        look(light("Strip left", "rect", 280.0, 20.0, 8.0, 60.0, 3.0, 6500.0), 0.1, 0.2, 0.0),
        look(light("Strip right", "rect", 80.0, 20.0, 8.0, 60.0, 3.0, 6500.0), 0.1, 0.2, 0.0),
        look(light("Bounce card", "rect", 180.0, 5.0, 70.0, 15.0, -0.5, 5500.0), 0.3, 0.8, 0.0),
    ])
}

/// A studio in a room: the gradient stays as it was (the room replaces it), the lights stand in it.
fn in_room(seed: u32, room: RoomParams, lights: Vec<LightParams>) -> HdriParams {
    let mut p = studio(seed, [0.05; 3], [0.2; 3], [0.1; 3], 0.2, lights);
    p.studio.room = Some(room);
    p
}

/// One studio light of `kind` (`apply_template`) at its place and level.
fn kit(name: &str, kind: ModifierKind, azimuth_deg: f32, elevation_deg: f32, intensity_ev: f32) -> LightParams {
    let mut light = LightParams { name: name.to_string(), azimuth_deg, elevation_deg, ..LightParams::default() };
    apply_template(&mut light, kind);
    light.intensity_ev = intensity_ev;
    light
}

fn sized(mut light: LightParams, width_deg: f32, height_deg: f32) -> LightParams {
    light.width_deg = width_deg;
    light.height_deg = height_deg;
    light
}

fn warmth(mut light: LightParams, kelvin: f32) -> LightParams {
    light.kelvin = kelvin;
    light
}

/// A white cyc rounded into every wall and the corners, under a dark ceiling: an octabox key, a
/// softbox fill on the other side and a strip behind the camera (white_studio, kontrast).
fn white_cyc() -> HdriParams {
    let room = RoomParams {
        width_m: 12.0,
        depth_m: 11.0,
        height_m: 5.5,
        cove_m: 1.8,
        cove_walls: "all".to_string(),
        floor: [0.78; 3],
        wall: [0.8; 3],
        ceiling: [0.05; 3],
        floor_gloss: 0.1,
        floor_roughness: 0.5,
        floor_material: Some("epoxy".to_string()),
        wall_material: Some("plaster".to_string()),
        wear: Some(0.1),
        ..RoomParams::default()
    };
    in_room(13, room, vec![
        as_key(warmth(kit("Octabox", ModifierKind::Octabox, 35.0, 14.0, 5.5), 6000.0)),
        warmth(kit("Softbox", ModifierKind::Softbox, 300.0, 12.0, 4.0), 6000.0),
        warmth(kit("Strip", ModifierKind::Strip, 200.0, 6.0, 4.5), 6000.0),
    ])
}

/// A small studio hung in black drapes round a white cyc floor and back wall, a pipe grid under
/// the ceiling: two bare fresnels either side of the set and a beauty dish hung high (studio_small).
fn small_dark_studio() -> HdriParams {
    let room = RoomParams {
        width_m: 6.5,
        depth_m: 6.0,
        height_m: 3.4,
        camera_height_m: 1.5,
        offset_z: 0.1,
        cove_m: 1.0,
        cove_walls: "back".to_string(),
        floor: [0.8; 3],
        wall: [0.025, 0.025, 0.03],
        ceiling: [0.04, 0.04, 0.045],
        floor_gloss: 0.15,
        floor_roughness: 0.35,
        ceiling_grid: true,
        grid_spacing_m: 1.2,
        floor_material: Some("epoxy".to_string()),
        wear: Some(0.15),
        ..RoomParams::default()
    };
    in_room(14, room, vec![
        as_key(warmth(kit("Fresnel left", ModifierKind::Fresnel, 320.0, 4.0, 10.0), 5600.0)),
        warmth(kit("Fresnel right", ModifierKind::Fresnel, 40.0, 4.0, 10.0), 5600.0),
        kit("Beauty dish", ModifierKind::BeautyDish, 120.0, 38.0, 7.5),
    ])
}

/// A wooden studio with a grey vinyl floor and a truss under its ceiling, daylight through a row
/// of windows behind the camera: a softbox and a strip on stands (wooden_studio, ferndale).
fn wooden_daylight() -> HdriParams {
    let room = RoomParams {
        width_m: 11.0,
        depth_m: 9.0,
        height_m: 4.2,
        cove_m: 1.5,
        cove_walls: "back".to_string(),
        floor: [0.17, 0.17, 0.19],
        wall: [0.5, 0.31, 0.16],
        ceiling: [0.55, 0.36, 0.2],
        floor_gloss: 0.35,
        floor_roughness: 0.25,
        ceiling_grid: true,
        grid_spacing_m: 2.0,
        floor_material: Some("epoxy".to_string()),
        wall_material: Some("planks".to_string()),
        wear: Some(0.2),
        ..RoomParams::default()
    };
    let mut windows = sized(kit("Windows", ModifierKind::Window, 180.0, 8.0, 0.0), 70.0, 40.0);
    windows.gobo = Some(GoboParams { kind: "window".to_string(), panes_x: 6, panes_y: 3, fill_sky: true, ..GoboParams::default() });
    let mut p = in_room(15, room, vec![
        as_key(sized(kit("Softbox", ModifierKind::Softbox, 30.0, 8.0, 5.0), 22.0, 30.0)),
        kit("Strip", ModifierKind::Strip, 300.0, 8.0, 4.5),
        windows,
    ]);
    // The sky the windows look out at.
    p.sky.sun.mode = "manual".to_string();
    p.sky.sun.elevation_deg = 30.0;
    p.sky.sun.azimuth_deg = 200.0;
    p.sky.clouds.coverage = 0.4;
    p
}

/// A dark set on a glossy wooden floor: a zigzag of neon running from red to blue, a window
/// through half-shut blinds and a warm softbox (neon_photostudio).
fn neon_studio() -> HdriParams {
    let room = RoomParams {
        width_m: 9.0,
        depth_m: 8.0,
        height_m: 4.0,
        camera_height_m: 1.5,
        floor: [0.22, 0.09, 0.035],
        wall: [0.04, 0.04, 0.045],
        ceiling: [0.6, 0.6, 0.58],
        floor_gloss: 0.9,
        floor_roughness: 0.08,
        stands: false,
        floor_material: Some("planks".to_string()),
        wear: Some(0.2),
        ..RoomParams::default()
    };
    let neon = LightParams {
        name: "Neon".to_string(),
        shape: "stroke".to_string(),
        azimuth_deg: 0.0,
        elevation_deg: 8.0,
        softness: 0.3,
        intensity_ev: 5.5,
        // Neon's own saturated red and argon blue (the tube clips white at
        // the room's exposure, its glow keeps the colour), and a little glow
        // round it (owner, 2026-11-07).
        rgb: Some([1.0, 0.012, 0.02]),
        stroke: Some(StrokeParams {
            points: vec![[350.0, 0.0], [354.0, 22.0], [358.0, 2.0], [2.0, 20.0], [6.0, 0.0], [10.0, 24.0], [14.0, 4.0]],
            curviness: 0.0,
            width_deg: 0.8,
            closed: false,
            profile: Vec::new(),
            end_kelvin: None,
            end_rgb: Some([0.04, 0.1, 1.0]),
        }),
        glow: Some(GlowParams { strength: 0.1, reach_deg: 0.6 }),
        ..LightParams::default()
    };
    let mut window = sized(kit("Window", ModifierKind::Window, 75.0, 10.0, 0.5), 34.0, 34.0);
    window.gobo = Some(GoboParams { kind: "blinds".to_string(), slats: 18, tilt_deg: 40.0, fill_sky: true, ..GoboParams::default() });
    let mut p = in_room(16, room, vec![neon, window, as_key(warmth(kit("Softbox", ModifierKind::Softbox, 300.0, 12.0, 2.5), 4500.0))]);
    p.sky.sun.mode = "manual".to_string();
    p.sky.sun.elevation_deg = 20.0;
    p.sky.sun.azimuth_deg = 100.0;
    p.sky.clouds.coverage = 0.8;
    p
}

/// A grey cyc on the back wall: a big white shoot-through umbrella, a silver umbrella beside it
/// and a strip behind the camera (studio_kominka).
fn umbrella_cyc() -> HdriParams {
    let room = RoomParams {
        width_m: 10.0,
        depth_m: 12.0,
        height_m: 4.6,
        offset_z: 0.15,
        cove_m: 1.6,
        cove_walls: "back".to_string(),
        floor: [0.62, 0.62, 0.64],
        wall: [0.64, 0.64, 0.66],
        ceiling: [0.6, 0.6, 0.62],
        floor_gloss: 0.05,
        floor_roughness: 0.6,
        floor_material: Some("epoxy".to_string()),
        wall_material: Some("plaster".to_string()),
        wear: Some(0.15),
        ..RoomParams::default()
    };
    in_room(17, room, vec![
        as_key(sized(kit("Umbrella", ModifierKind::Umbrella, 40.0, 16.0, 5.0), 34.0, 34.0)),
        sized(kit("Silver umbrella", ModifierKind::SilverUmbrella, 75.0, 18.0, 4.0), 22.0, 22.0),
        kit("Strip", ModifierKind::Strip, 300.0, 6.0, 3.0),
    ])
}

/// An automotive photo studio: a white cyc rounded into every wall under a dark ceiling, a scrim
/// as long as the car hung over it (the long highlight down a bonnet and roof), a tall strip
/// low on either side for the flanks, a large softbox behind the camera for the front and the
/// cyc, and a light grey floor with some sheen.
fn car_studio() -> HdriParams {
    let room = RoomParams {
        width_m: 14.0,
        depth_m: 16.0,
        height_m: 6.0,
        cove_m: 2.0,
        cove_walls: "all".to_string(),
        floor: [0.7; 3],
        wall: [0.9; 3],
        ceiling: [0.03; 3],
        floor_gloss: 0.3,
        floor_roughness: 0.35,
        floor_material: Some("epoxy".to_string()),
        wall_material: Some("plaster".to_string()),
        wear: Some(0.1),
        ..RoomParams::default()
    };
    let overhead = LightParams {
        name: "Overhead scrim".to_string(),
        shape: "scrim".to_string(),
        azimuth_deg: 0.0,
        elevation_deg: 70.0,
        width_deg: 80.0,
        height_deg: 45.0,
        intensity_ev: 5.5,
        kelvin: 6500.0,
        scrim: Some(ScrimParams { source_size: 0.8, frame: 0.02 }),
        ..LightParams::default()
    };
    in_room(18, room, vec![
        as_key(overhead),
        warmth(sized(kit("Strip left", ModifierKind::Strip, 270.0, 15.0, 7.0), 8.0, 50.0), 6500.0),
        warmth(sized(kit("Strip right", ModifierKind::Strip, 90.0, 15.0, 7.0), 8.0, 50.0), 6500.0),
        warmth(sized(kit("Softbox behind", ModifierKind::Softbox, 180.0, 20.0, 7.0), 60.0, 30.0), 6500.0),
    ])
}

/// A modern car showroom (the detailing studios' hexagon ceilings): a hall in dark greys whose
/// ceiling over the car is a honeycomb of LED tubes in a frame, its joints dark connectors, so a
/// body and its glass mirror the grid; upright LED bars down the west wall and across the back;
/// a glass wall of daylight down the east side; and a dark polished floor that mirrors it all.
fn car_showroom() -> HdriParams {
    let room = RoomParams {
        width_m: 16.0,
        depth_m: 24.0,
        height_m: 4.8,
        floor: [0.03, 0.03, 0.033],
        wall: [0.4, 0.4, 0.41],
        ceiling: [0.3, 0.3, 0.31],
        floor_gloss: 0.9,
        floor_roughness: 0.02,
        stands: false,
        floor_material: Some("polished_concrete".to_string()),
        wall_material: Some("plaster".to_string()),
        wear: Some(0.1),
        ..RoomParams::default()
    };
    let (camera, half_x, half_z) = (room.camera_height_m, 0.5 * room.width_m, 0.5 * room.depth_m);
    let honeycomb = PatternParams {
        cell_m: 0.9,
        tube_m: 0.06,
        rows: 12,
        columns: 11,
        mount_dim: 1.0,
        corner_dim: 1.0,
        ..PatternParams::default()
    };
    // (a little north of straight up: a face right overhead would sit on the pole of its frame)
    let over = [0.0, room.height_m - camera - 0.15, -0.3];
    let border = on_ceiling(pattern_light("Ceiling frame", frame_around(&honeycomb, 0.15), 7.0), over);
    let mut ceiling = on_ceiling(pattern_light("Hexagon ceiling", honeycomb, 7.0), over);
    ceiling.key = true;
    let bars = PatternParams {
        source: "bars".to_string(),
        cell_m: 3.0,
        tube_m: 0.06,
        rows: 1,
        columns: 18,
        gap_m: 0.95,
        mount_dim: 1.0,
        corner_dim: 1.0,
        ..PatternParams::default()
    };
    let west = on_wall(pattern_light("West wall bars", bars.clone(), 6.5), [0.05 - half_x, 2.1 - camera, 0.0]);
    let back = on_wall(pattern_light("Back wall bars", PatternParams { columns: 12, ..bars }, 6.5), [0.0, 2.1 - camera, 0.05 - half_z]);
    let mut glass = kit("Glass wall", ModifierKind::Window, 90.0, 0.0, 2.0);
    glass.gobo = Some(GoboParams { kind: "window".to_string(), panes_x: 8, panes_y: 2, fill_sky: true, ..GoboParams::default() });
    glass.space = Some(SpaceParams { width_m: 20.0, height_m: 3.8, beam: template_beam(ModifierKind::Window), ..SpaceParams::default() });
    let glass = on_wall(glass, [half_x - 0.05, 2.0 - camera, 0.0]);
    let mut p = in_room(19, room, vec![ceiling, border, west, back, glass]);
    // The day outside the glass.
    p.sky.sun.mode = "manual".to_string();
    p.sky.sun.elevation_deg = 35.0;
    p.sky.sun.azimuth_deg = 120.0;
    p.sky.clouds.coverage = 0.3;
    p
}

/// A car detailing studio: a light grey cyc rounded into every wall under a mid grey ceiling, a
/// large honeycomb of LED tubes hung over the car in a rectangular border (the highlights over a
/// roof, bonnet and glass), a smaller honeycomb on the back wall, rows of upright LED bars on the
/// other three walls (a car's flanks see lights whichever way it stands), and a mid grey satin
/// floor that mirrors them softly.
fn hexagon_studio() -> HdriParams {
    let room = RoomParams {
        width_m: 10.0,
        depth_m: 12.0,
        height_m: 4.2,
        cove_m: 0.5,
        cove_walls: "all".to_string(),
        floor: [0.3, 0.3, 0.31],
        wall: [0.85, 0.85, 0.87],
        ceiling: [0.45; 3],
        floor_gloss: 0.45,
        floor_roughness: 0.12,
        stands: false,
        floor_material: Some("epoxy".to_string()),
        wall_material: Some("plaster".to_string()),
        wear: Some(0.1),
        ..RoomParams::default()
    };
    // Hung 15 cm under the ceiling, a little north of straight overhead (off the pole of its frame).
    let above = room.height_m - room.camera_height_m - 0.15;
    let honeycomb = PatternParams { cell_m: 1.2, tube_m: 0.05, rows: 5, columns: 9, corner_dim: 1.0, ..PatternParams::default() };
    let [w, h] = lattice_m(&honeycomb);
    // One frame round the honeycomb, its tubes on the honeycomb's outer edges (frames without a
    // gap: one frame).
    let border_cell = 0.1;
    let border = PatternParams {
        source: "frames".to_string(),
        cell_m: border_cell,
        tube_m: honeycomb.tube_m,
        columns: (w / border_cell).round() as u32,
        rows: (h / border_cell).round() as u32,
        gap_m: 0.0,
        corner_dim: 1.0,
        ..PatternParams::default()
    };
    let wall_honeycomb = PatternParams { cell_m: 0.75, tube_m: 0.04, rows: 3, columns: 11, corner_dim: 1.0, ..PatternParams::default() };
    // Upright bars from knee height to above the doors, low enough for a car's flanks to mirror,
    // wide enough to read in them from across the room, and along most of each wall, so a car
    // turned any way finds them in its flanks.
    let bars = PatternParams { source: "bars".to_string(), cell_m: 2.2, tube_m: 0.1, rows: 1, columns: 11, gap_m: 0.8, ..PatternParams::default() };
    let side_bars = PatternParams { columns: 13, ..bars.clone() };
    // On the walls, 2 cm off them.
    let side = 0.5 * room.width_m - 0.02;
    let back = 0.5 * room.depth_m - 0.02;
    let mut ceiling = flat_pattern("Hexagon ceiling", honeycomb, [0.0, above, -0.2], true, 8.0);
    ceiling.key = true;
    in_room(20, room, vec![
        ceiling,
        flat_pattern("Hexagon border", border, [0.0, above, -0.2], true, 8.0),
        flat_pattern("Hexagon wall", wall_honeycomb, [0.0, 0.7, -back], false, 7.5),
        flat_pattern("Bars east", side_bars.clone(), [side, 0.1, 0.0], false, 7.0),
        flat_pattern("Bars west", side_bars, [-side, 0.1, 0.0], false, 7.0),
        flat_pattern("Bars south", bars, [0.0, 0.1, back], false, 7.0),
    ])
}

/// A pattern light whose face lies flat at `centre_m` (metres from the camera: x east, y up, z
/// south), at the pattern's natural size: facing straight down when `down`, else upright and
/// facing the camera across the level, which lies flat on a wall when the face's middle is
/// straight out from the camera along the wall's normal (x 0 on the north and south walls, z 0 on
/// the east and west). Either way the face, which starts out facing the camera, is pitched by its
/// middle's elevation. LED tubes at 6500 K, without a stand.
fn flat_pattern(name: &str, pattern: PatternParams, centre_m: [f32; 3], down: bool, intensity_ev: f32) -> LightParams {
    let [x, y, z] = centre_m;
    let elevation_deg = y.atan2(x.hypot(z)).to_degrees();
    let mut light = LightParams {
        name: name.to_string(),
        shape: "pattern".to_string(),
        intensity_ev,
        kelvin: 6500.0,
        stand: Some(false),
        pattern: Some(pattern),
        space: Some(SpaceParams {
            position_m: centre_m,
            pitch_deg: if down { elevation_deg - 90.0 } else { elevation_deg },
            ..SpaceParams::default()
        }),
        ..LightParams::default()
    };
    fit_face(&mut light);
    follow_face(&mut light);
    light
}

/// A road tunnel, 60 m of it, 11 m wide and 6.5 m high, with the camera at a car's height on the
/// middle line of a two-lane road, as on a road closed for a shoot: dark asphalt with white edge
/// lines and dashes, a raised walk either side, light tiled walls under a dark sooty ceiling, a row
/// of white LED fittings along the ceiling over each edge of the road, tilted a little toward the
/// road, a green emergency exit sign on each wall, and both ends open on a bright day, far brighter
/// than the tunnel, as a real one is by day. The rows run the tunnel's length, so their fittings
/// close up and shrink toward each mouth and lie as dotted streaks of light along a car's flanks,
/// shoulders and glass.
///
/// A hybrid (2026-11-05): both ends and the floor stand open, so the tunnel stands on the scene's
/// ground, a two-lane Road (the ground's own, running on out of both mouths) with the tunnel's
/// concrete beside it, and the outside is the scene's outdoor one.
fn road_tunnel() -> HdriParams {
    let (width, height, lane) = (11.0, 6.5, 3.75);
    let room = RoomParams {
        width_m: width,
        depth_m: 60.0,
        height_m: height,
        camera_height_m: 1.2,
        floor: [0.32, 0.32, 0.31],
        wall: [0.52, 0.54, 0.56],
        ceiling: [0.06, 0.06, 0.06],
        floor_gloss: 0.1,
        floor_roughness: 0.5,
        stands: false,
        sides: Some(RoomSides { north: true, south: true, floor: true, ..RoomSides::default() }),
        wall_material: Some("form_concrete".to_string()),
        ceiling_material: Some("form_concrete".to_string()),
        wear: Some(0.5),
        ..RoomParams::default()
    };
    let ground = road_ground(lane, "room", room.floor);
    // From the camera: the rows over the road's edges, 0.3 m under the ceiling.
    let y = height - room.camera_height_m - 0.3;
    let west = tunnel_lights("Lights west", [-lane, y, 0.0], 1.0);
    let mut east = tunnel_lights("Lights east", [lane, y, 0.0], -1.0);
    east.key = true;
    let (west_wall, east_wall) = (0.02 - 0.5 * width, 0.5 * width - 0.02);
    let signs = [
        exit_sign("Exit sign west", [west_wall, 1.3, -18.0], 1.0),
        exit_sign("Exit sign east", [east_wall, 1.3, 14.0], -1.0),
    ];
    let mut p = in_room(20, room, [west, east].into_iter().chain(signs).collect());
    p.floor = Some(ground);
    // The day outside both mouths: the sun high in the south-southeast.
    p.sky.sun.mode = "manual".to_string();
    p.sky.sun.elevation_deg = 40.0;
    p.sky.sun.azimuth_deg = 160.0;
    p.sky.clouds.coverage = 0.3;
    p
}

/// A bridge seen from under it (2026-11-05): a very short tunnel, 12 m of grey concrete deck 5.5 m
/// over a two-lane country road, 16 m between its abutments, the camera at a car's height on the
/// road's middle line. Both ends and the floor stand open: the ground's road runs on out of both
/// sides of the bridge, grass beside it, under the scene's own day (a high sun, a few clouds). Two
/// short rows of LED fittings under the deck over the road's edges (the east one the key), dim
/// against the day through its ends and off the road.
fn bridge() -> HdriParams {
    let room = RoomParams {
        width_m: 16.0,
        depth_m: 12.0,
        height_m: 5.5,
        camera_height_m: 1.2,
        floor: [0.32, 0.32, 0.31],
        wall: [0.42, 0.41, 0.39],
        ceiling: [0.30, 0.30, 0.29],
        floor_gloss: 0.05,
        floor_roughness: 0.6,
        stands: false,
        sides: Some(RoomSides { north: true, south: true, floor: true, ..RoomSides::default() }),
        wall_material: Some("form_concrete".to_string()),
        ceiling_material: Some("form_concrete".to_string()),
        wear: Some(0.4),
        ..RoomParams::default()
    };
    let ground = road_ground(3.5, "grass", room.floor);
    // Two short rows under the deck over the road's edges, as the tunnel's (straight over the camera a row lay at the
    // map's pole, where its texels sample it worst).
    let y = room.height_m - room.camera_height_m - 0.2;
    let mut west = lights_row("Underpass lights west", [-3.5, y, 0.0], 1.0, 10.0);
    let mut east = lights_row("Underpass lights east", [3.5, y, 0.0], -1.0, 10.0);
    (west.intensity_ev, east.intensity_ev) = (3.5, 3.5);
    east.key = true;
    let mut p = in_room(21, room, vec![west, east]);
    p.floor = Some(ground);
    p.sky.sun.mode = "manual".to_string();
    p.sky.sun.elevation_deg = 45.0;
    p.sky.sun.azimuth_deg = 200.0;
    p.sky.clouds.coverage = 0.35;
    p
}

/// The Bridge after dark (2026-11-07): the same underpass under a moonlit night, the sun 25
/// degrees under the west-northwest horizon and a bright moon low in the east-southeast, the
/// road bending to the right 40 m ahead of the viewer, past the north mouth (a 120 m radius).
/// The bridge's own rows under the deck are turned up to the tunnel's level (5.5 EV, dim against
/// the day in the Bridge). At night they are the underpass's only light and light the road under the deck.
fn bridge_at_night() -> HdriParams {
    let mut p = bridge();
    for light in &mut p.lights {
        light.intensity_ev = 5.5;
    }
    p.sky.sun.elevation_deg = -25.0;
    p.sky.sun.azimuth_deg = 285.0;
    p.sky.clouds.coverage = 0.15;
    let night = &mut p.sky.night;
    night.moon = true;
    night.moon_elevation_deg = 25.0;
    night.moon_azimuth_deg = 110.0;
    night.moon_size_deg = 0.52;
    night.moon_brightness = 1.5;
    night.stars = 0.35;
    night.star_brightness = 0.8;
    night.glow_strength = 0.8;
    if let Some(ground) = p.floor.as_mut() {
        ground.curve = Some("right".to_string());
        ground.curve_radius_m = Some(120.0);
        ground.curve_start_m = Some(40.0);
    }
    p
}

/// The ground a road tunnel or a bridge stands on: a two-lane Road, `lane_m` a lane, its lines
/// painted, a little worn, `beside` (`ROAD_BESIDE_NAMES`) along it (a room's: `room_floor`).
fn road_ground(lane_m: f32, beside: &str, room_floor: [f32; 3]) -> FloorParams {
    FloorParams {
        kind: "road".to_string(),
        lanes: 2,
        lane_width_m: Some(lane_m),
        wear: 0.2,
        markings: true,
        beside: Some(beside.to_string()),
        beside_color: Some(room_floor),
        ..FloorParams::default()
    }
}

/// One row of tunnel lights along the tunnel (north to south), its middle at `middle_m`: a pattern
/// light of one column of bars, each a tunnel luminaire (2026-11-10): a 1.3 m frosted diffuser 0.18 m
/// wide, square at its ends, in a long, narrow grey housing about 1.5 x 0.3 m (`PatternParams::housing`,
/// drawn and lit by the room), one every 1.8 m, the row as long as the tunnel less a metre at each
/// mouth. Its face lies along the row and looks down, tilted 25 degrees toward the road's middle
/// (`toward` +1 to the east, -1 to the west); diffuse, so the far fittings still glow as they turn
/// edge-on.
fn tunnel_lights(name: &str, middle_m: [f32; 3], toward: f32) -> LightParams {
    lights_row(name, middle_m, toward, 58.0)
}

/// A row of tunnel lights (`tunnel_lights`) `length_m` long: as many fittings as fit.
fn lights_row(name: &str, middle_m: [f32; 3], toward: f32, length_m: f32) -> LightParams {
    let (fitting, between) = (1.3, 0.5);
    let fittings = (length_m / (fitting + between)).floor().max(1.0) as u32;
    let pattern = PatternParams {
        source: "bars".to_string(),
        cell_m: fitting,
        tube_m: 0.18,
        rows: fittings,
        columns: 1,
        gap_m: between,
        look: 1.0,
        mount_spacing_m: 0.0,
        mount_dim: 0.5,
        corner_dim: 0.0,
        housing: Some(0.5),
        ..PatternParams::default()
    };
    let [width_m, height_m] = natural_size_m(&pattern).unwrap_or([0.5, length_m]);
    let tilt = 25f32.to_radians();
    let front = vec3f(toward * tilt.sin(), -tilt.cos(), 0.0);
    let (yaw_deg, pitch_deg, roll_deg) = face_turn(middle_m, front, vec3f(0.0, 0.0, -1.0));
    let mut light = LightParams {
        name: name.to_string(),
        shape: "pattern".to_string(),
        intensity_ev: 5.5,
        kelvin: 5000.0,
        stand: Some(false),
        space: Some(SpaceParams { position_m: middle_m, yaw_deg, pitch_deg, roll_deg, width_m, height_m, beam: 0.0 }),
        pattern: Some(pattern),
        ..LightParams::default()
    };
    // As clamp() holds a pattern light: its face at the pattern's aspect, its 2D placement on it.
    hold_face(&mut light);
    follow_face(&mut light);
    light
}

/// A lit green emergency exit sign, 0.8 x 0.4 m, flat on a wall with its middle at `middle_m`,
/// facing across the tunnel (`toward` +1 to the east, -1 to the west).
fn exit_sign(name: &str, middle_m: [f32; 3], toward: f32) -> LightParams {
    let (yaw_deg, pitch_deg, roll_deg) = face_turn(middle_m, vec3f(toward, 0.0, 0.0), vec3f(0.0, 1.0, 0.0));
    let mut light = LightParams {
        name: name.to_string(),
        shape: "rect".to_string(),
        softness: 0.02,
        intensity_ev: 2.0,
        rgb: Some([0.05, 0.8, 0.25]),
        stand: Some(false),
        space: Some(SpaceParams { position_m: middle_m, yaw_deg, pitch_deg, roll_deg, width_m: 0.8, height_m: 0.4, beam: 0.0 }),
        ..LightParams::default()
    };
    follow_face(&mut light);
    light
}

/// The yaw, pitch and roll (`SpaceParams`) that turn a face whose middle stands at `position_m`
/// so that its front looks along `front` and its up axis runs along `up` (unit, at right angles):
/// `space::space_frame` worked backwards. The roll stands the face's up axis where the yaw (about
/// it) and then the pitch can bring the front round; of its two ways, the one that keeps the pitch
/// within 90 degrees.
fn face_turn(position_m: [f32; 3], front: Vec3f, up: Vec3f) -> (f32, f32, f32) {
    let [x, y, z] = position_m;
    let center = vec3f(x, y, z).normalize();
    let toward_camera = -center;
    let (azimuth_deg, elevation_deg) = az_el_from_dir(center);
    let flat = light_frame(azimuth_deg, elevation_deg, 0.0);
    let sx = box_x_sign();
    // The face's right as space_frame turns it before its sign: right x up is the face's back.
    let right = Vec3f::cross(front, up).normalize() * sx;
    for way in [1.0, -1.0] {
        let rolled_up = Vec3f::cross(toward_camera, right).normalize() * way;
        let roll_deg = rolled_up.dot(flat.right).atan2(rolled_up.dot(flat.up)).to_degrees();
        let base = light_frame(azimuth_deg, elevation_deg, roll_deg);
        let seen_right = base.right * sx;
        let yaw = (-right.dot(toward_camera)).atan2(right.dot(seen_right));
        let yawed = toward_camera * yaw.cos() + seen_right * yaw.sin();
        if front.dot(yawed) >= 0.0 {
            let pitch = front.dot(base.up).atan2(front.dot(yawed));
            return (yaw.to_degrees(), pitch.to_degrees(), roll_deg);
        }
    }
    (0.0, 0.0, 0.0)
}

/// A pattern light (shape "pattern") of `pattern`'s LED tubes at its natural size, white at
/// 6000 K. `on_ceiling` or `on_wall` hangs it.
fn pattern_light(name: &str, pattern: PatternParams, intensity_ev: f32) -> LightParams {
    let mut light = LightParams {
        name: name.to_string(),
        shape: "pattern".to_string(),
        intensity_ev,
        kelvin: 6000.0,
        pattern: Some(pattern),
        space: Some(SpaceParams::default()),
        ..LightParams::default()
    };
    fit_face(&mut light);
    light
}

/// One rectangular frame of LED tube as wide as `pattern`'s tubes round `pattern` at its natural
/// size, about `margin_m` clear of it on every side, its corners dark connectors. Its sides are
/// whole frame cells, each as long as two and a half tube widths (a frame's tube is at most
/// half its cell).
fn frame_around(pattern: &PatternParams, margin_m: f32) -> PatternParams {
    let [w, h] = natural_size_m(pattern).unwrap_or([pattern.cell_m; 2]);
    let step = 2.5 * pattern.tube_m;
    let cells = |side: f32| ((side + 2.0 * margin_m) / step).round() as u32;
    PatternParams {
        source: "frames".to_string(),
        cell_m: step,
        tube_m: pattern.tube_m,
        rows: cells(h),
        columns: cells(w),
        gap_m: 0.0,
        mount_dim: 1.0,
        corner_dim: 1.0,
        ..PatternParams::default()
    }
}

/// Hangs `light`'s face level under the ceiling, its middle at `position_m` (x east, y up from the
/// camera, z south), facing down: pitched from facing the camera by its elevation less 90 degrees
/// (`space::space_frame`), its width running east to west, its height north to south.
fn on_ceiling(light: LightParams, position_m: [f32; 3]) -> LightParams {
    let [x, y, z] = position_m;
    placed(light, position_m, y.atan2(x.hypot(z)).to_degrees() - 90.0)
}

/// Stands `light`'s face upright on a wall, its middle at `position_m`, square to the camera
/// across the floor (straight east or west of it, or north or south): pitched by its elevation,
/// its width running along the wall.
fn on_wall(light: LightParams, position_m: [f32; 3]) -> LightParams {
    let [x, y, z] = position_m;
    placed(light, position_m, y.atan2(x.hypot(z)).to_degrees())
}

/// Moves `light`'s face (it has one) to `position_m` with `pitch_deg`, and its 2D placement with it
/// (`space::follow_face`), as clamp() would.
fn placed(mut light: LightParams, position_m: [f32; 3], pitch_deg: f32) -> LightParams {
    if let Some(space) = light.space.as_mut() {
        space.position_m = position_m;
        space.pitch_deg = pitch_deg;
    }
    follow_face(&mut light);
    light
}

/// A corridor of LED portals the car drives through (the LED tunnels of detailing bays): frames of
/// LED tube across a long dark room, open at the floor and their top corners cut off, standing every
/// PORTAL_STEP_M ahead of the camera and behind it and facing along the corridor, over a black
/// polished floor that mirrors them. Cool white. The key is the first frame ahead clear of a car at
/// the camera (3 m north): the nearer ones stand round the camera, with no one direction to light
/// from. Its 40 portals bake in about 1.1 s at 1K and 6 s at 4K (the app's bake, 16 threads; 16
/// portals 1.2 m apart took 3.3 s at 4K).
fn led_portal() -> HdriParams {
    let room = RoomParams {
        width_m: 6.0,
        depth_m: 32.0,
        height_m: 3.8,
        camera_height_m: 1.4,
        floor: [0.025, 0.025, 0.03],
        wall: [0.14, 0.14, 0.15],
        ceiling: [0.06; 3],
        floor_gloss: 0.9,
        floor_roughness: 0.03,
        stands: false,
        floor_material: Some("epoxy".to_string()),
        wear: Some(0.1),
        ..RoomParams::default()
    };
    let svg = PatternSvg::new(portal_svg());
    let z_m: Vec<f32> = (0..PORTAL_COUNT).map(|i| PORTAL_STEP_M * (i as f32 + 0.5 - 0.5 * PORTAL_COUNT as f32)).collect();
    let mut lights: Vec<LightParams> =
        z_m.iter().enumerate().map(|(i, &z)| portal(&format!("Portal {}", i + 1), z, -room.camera_height_m, &svg)).collect();
    let key = z_m.iter().rposition(|&z| z < -2.5).unwrap_or(0);
    lights[key].key = true;
    in_room(20, room, lights)
}

/// The LED portals along the corridor (`led_portal`): this many, one every PORTAL_STEP_M metres,
/// the camera between the middle two, so the last stand 14.6 m off, 1.4 m short of the end walls.
/// The detailing bays' portal tunnels stand their frames this close, the corridor reading as one
/// ribbed tube.
const PORTAL_COUNT: usize = 40;
const PORTAL_STEP_M: f32 = 0.75;

/// An LED portal's face, metres: 5 m between its legs' tubes and 3.2 m to the top of its frame,
/// with a border of two tube widths at its top and sides (`portal_svg`).
const PORTAL_FACE_M: [f32; 2] = [5.4, 3.4];

/// An LED portal's drawing, millimetres: a 10 cm tube up from the floor (the face's bottom edge),
/// 2.4 m up each side, cut across the top corners at 45 degrees and along the top. The bends are
/// its connectors, which `PatternParams::corner_dim` dims. The legs run on past the face's bottom
/// edge by the border, into the floor: an LED tube's open end is its dark end cap (7.5 cm at a
/// tenth of its light), which ending at the floor drew as a grey band with its mirror image a
/// dark one between each leg and its reflection; under the floor the face cuts it off, so a leg
/// runs straight into its reflection.
fn portal_svg() -> String {
    let [w, h] = PORTAL_FACE_M.map(|v| (v * 1000.0).round() as i32);
    let (margin, shoulder, chamfer) = (200, 2400, 800);
    let (left, right, top, foot) = (margin, w - margin, margin, h + margin);
    let knee = h - shoulder;
    let d = format!("M{left} {foot} L{left} {knee} L{} {top} L{} {top} L{right} {knee} L{right} {foot}", left + chamfer, right - chamfer);
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}mm\" height=\"{h}mm\" viewBox=\"0 0 {w} {h}\">\
<path d=\"{d}\" fill=\"none\" stroke=\"#ffffff\" stroke-width=\"100\" stroke-linecap=\"butt\" stroke-linejoin=\"miter\"/></svg>"
    )
}

/// One LED portal (`led_portal`) `z_m` south of the camera, standing on the floor `floor_y_m`
/// below it, square to the corridor whichever side of the camera it stands: facing the camera
/// would lean the face toward the eye by its middle's elevation, so it is pitched back by as much.
fn portal(name: &str, z_m: f32, floor_y_m: f32, svg: &PatternSvg) -> LightParams {
    let [width_m, height_m] = PORTAL_FACE_M;
    let y = floor_y_m + 0.5 * height_m;
    let pitch_deg = y.atan2(z_m.abs()).to_degrees();
    let mut light = LightParams {
        name: name.to_string(),
        shape: "pattern".to_string(),
        intensity_ev: 6.5,
        kelvin: 6500.0,
        space: Some(SpaceParams { position_m: [0.0, y, z_m], yaw_deg: 0.0, pitch_deg, roll_deg: 0.0, width_m, height_m, beam: 0.0 }),
        pattern: Some(PatternParams { source: "svg".to_string(), svg: svg.clone(), look: 1.0, corner_dim: 0.6, ..PatternParams::default() }),
        ..LightParams::default()
    };
    // The face's height and the light's flat placement as clamp() holds them.
    hold_face(&mut light);
    follow_face(&mut light);
    light
}


/// One studio light of `kind` with its modifier's `baffle` set (modifier.rs: a softbox's 0.7 the
/// inner baffle alone, 1 the open box; an umbrella's 0 the parabolic with its front diffuser, 0.6
/// the white bounce).
fn baffled(mut light: LightParams, baffle: f32) -> LightParams {
    if let Some(modifier) = light.modifier.as_mut() {
        modifier.baffle = baffle;
    }
    light
}

/// The studio lights' cloth boxes up close (2026-11-07), large in front of the camera on a grey
/// cyc under a dark ceiling: an open softbox (its bare tube, crumpled silver walls and flat back
/// panel round the ring, the key), an open octabox beside it and an inner-baffle softbox (the lining's
/// folded walls round its hot core) to the east.
fn open_boxes() -> HdriParams {
    let room = RoomParams {
        width_m: 11.0,
        depth_m: 11.0,
        height_m: 4.8,
        cove_m: 1.5,
        cove_walls: "back".to_string(),
        floor: [0.3, 0.3, 0.31],
        wall: [0.34, 0.34, 0.35],
        ceiling: [0.04; 3],
        floor_gloss: 0.15,
        floor_roughness: 0.45,
        floor_material: Some("epoxy".to_string()),
        wall_material: Some("plaster".to_string()),
        wear: Some(0.1),
        ..RoomParams::default()
    };
    in_room(24, room, vec![
        as_key(baffled(sized(kit("Open softbox", ModifierKind::Softbox, 335.0, 12.0, 5.0), 34.0, 26.0), 1.0)),
        baffled(sized(kit("Open octabox", ModifierKind::Octabox, 25.0, 14.0, 5.0), 34.0, 34.0), 1.0),
        baffled(sized(kit("Inner baffle softbox", ModifierKind::Softbox, 80.0, 10.0, 4.5), 30.0, 22.0), 0.7),
    ])
}

/// The studio lights' umbrellas up close (2026-11-07), large in front of the camera on a grey cyc:
/// a white shoot-through umbrella (its ribs to the hem's corners as metal rods, the folds the rib
/// tips pull), a shallow silver umbrella (its ring, spokes, stretchers and stitched seams into the
/// hub) and a parabolic with its front diffuser stretched over the rib tips, to the east.
fn umbrellas() -> HdriParams {
    let room = RoomParams {
        width_m: 11.0,
        depth_m: 11.0,
        height_m: 4.8,
        cove_m: 1.5,
        cove_walls: "back".to_string(),
        floor: [0.42, 0.42, 0.44],
        wall: [0.46, 0.46, 0.48],
        ceiling: [0.05; 3],
        floor_gloss: 0.08,
        floor_roughness: 0.55,
        floor_material: Some("epoxy".to_string()),
        wall_material: Some("plaster".to_string()),
        wear: Some(0.1),
        ..RoomParams::default()
    };
    in_room(25, room, vec![
        as_key(sized(kit("White umbrella", ModifierKind::Umbrella, 335.0, 14.0, 5.0), 38.0, 38.0)),
        sized(kit("Silver umbrella", ModifierKind::SilverUmbrella, 25.0, 14.0, 4.5), 34.0, 34.0),
        baffled(sized(kit("Parabolic", ModifierKind::Umbrella, 80.0, 12.0, 4.5), 34.0, 34.0), 0.0),
    ])
}


/// A hybrid studio (2026-11-07): a pale loft whose east wall stands open to the outside and whose
/// back wall has two tall sash windows in black classical surrounds (pilasters, brackets, a cornice
/// cap, a sill and a panelled apron, after the apartment facade kit), with a panelled door under a
/// transom in the west wall, the windows and the door one building's set (`RoomWindow::with_kit`,
/// Victorian, one seed): the scene's own day (a sun in the south-west, a few clouds, the skyline and
/// the ground) seen through the windows and lighting the room through them, with one softbox as the key.
fn open_loft() -> HdriParams {
    use super::{RoomWindow, WindowKit};
    let set = 11;
    let window = |along_m: f32| RoomWindow { wall: "north".to_string(), along_m, sill_m: 0.85, width_m: 1.5, height_m: 2.3, ..RoomWindow::default() }.with_kit(WindowKit::Victorian, set, false);
    let door = RoomWindow { wall: "west".to_string(), along_m: 0.0, width_m: 1.05, height_m: 2.65, ..RoomWindow::default() }.with_kit(WindowKit::Victorian, set, true);
    let room = RoomParams {
        width_m: 12.0,
        depth_m: 10.0,
        height_m: 4.0,
        floor: [0.36, 0.27, 0.18],
        wall: [0.72, 0.71, 0.68],
        ceiling: [0.7, 0.7, 0.68],
        floor_gloss: 0.3,
        floor_roughness: 0.3,
        sides: Some(RoomSides { east: true, ..RoomSides::default() }),
        windows: Some(vec![window(-2.6), window(2.6), door]),
        floor_material: Some("herringbone".to_string()),
        wall_material: Some("plaster".to_string()),
        ceiling_material: Some("plaster".to_string()),
        wear: Some(0.35),
        ..RoomParams::default()
    };
    let mut p = in_room(27, room, vec![as_key(sized(kit("Softbox", ModifierKind::Softbox, 300.0, 10.0, 4.0), 22.0, 28.0))]);
    p.sky.sun.mode = "manual".to_string();
    p.sky.sun.elevation_deg = 35.0;
    p.sky.sun.azimuth_deg = 140.0;
    p.sky.clouds.coverage = 0.3;
    p
}

/// A factory hall by day (2026-11-07), after the factory facade kit: a large hall of pale painted
/// brick under a dark roof, tall steel windows of many panes with segmental heads on concrete sills
/// under lintels in its north and east walls, and in its west wall flush steel double doors and a
/// roller shutter under its coil box, all one building's set (`RoomWindow::with_kit`, Factory, one
/// seed). A morning sun in the south-east falls in through the east windows in long patches of
/// their panes on the concrete floor; one softbox high over the floor is the key.
fn factory_hall() -> HdriParams {
    use super::{RoomWindow, WindowKit};
    let set = 7;
    let window = |wall: &str, along_m: f32| RoomWindow { wall: wall.to_string(), along_m, sill_m: 1.3, width_m: 2.8, height_m: 4.4, depth_m: Some(0.35), ..RoomWindow::default() }.with_kit(WindowKit::Factory, set, false);
    let mut windows: Vec<RoomWindow> = [-7.5, -2.5, 2.5, 7.5].iter().map(|&a| window("north", a)).collect();
    windows.extend([-10.0, -5.0, 0.0, 5.0, 10.0].iter().map(|&a| window("east", a)));
    let door = |along_m: f32, width_m: f32, height_m: f32, leaf: &str| {
        RoomWindow { wall: "west".to_string(), along_m, width_m, height_m, depth_m: Some(0.35), leaf: Some(leaf.to_string()), frame_m: 0.06, ..RoomWindow::default() }.with_kit(WindowKit::Factory, set, true)
    };
    windows.push(door(-6.0, 2.0, 2.3, "flush"));
    windows.push(door(4.0, 4.6, 4.4, "shutter"));
    let room = RoomParams {
        width_m: 24.0,
        depth_m: 30.0,
        height_m: 9.0,
        floor: [0.3, 0.29, 0.27],
        wall: [0.62, 0.6, 0.56],
        ceiling: [0.22, 0.22, 0.23],
        floor_gloss: 0.15,
        floor_roughness: 0.5,
        stands: false,
        windows: Some(windows),
        floor_material: Some("polished_concrete".to_string()),
        wall_material: Some("painted_brick".to_string()),
        ceiling_material: Some("form_concrete".to_string()),
        wear: Some(0.45),
        ..RoomParams::default()
    };
    let mut p = in_room(28, room, vec![as_key(sized(kit("Softbox", ModifierKind::Softbox, 200.0, 55.0, 3.0), 20.0, 20.0))]);
    p.sky.sun.mode = "manual".to_string();
    p.sky.sun.elevation_deg = 32.0;
    p.sky.sun.azimuth_deg = 120.0;
    p.sky.clouds.coverage = 0.25;
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atmosphere::sun_direction;
    use crate::{LightShape, Mode, SunMode, LIGHT_LIMIT};

    const SKY: [&str; 20] = [
        "Clear noon",
        "Golden hour",
        "Sunset",
        "Overcast",
        "Blue hour",
        "Moonlit night",
        "Starry night",
        "Stratocumulus low sun",
        "Stratocumulus high sun",
        "Altocumulus",
        "Cumulus",
        "Stratus",
        "Cirrus",
        "Cirrostratus",
        "After rain",
        "Overcast and wet",
        "Heavy rain",
        "Misty winter morning",
        "Snowy sunny day",
        "After the shower",
    ];
    const STUDIO: [&str; 22] = [
        "Three-point",
        "Top softbox",
        "Rim pair",
        "Overcast dome",
        "Ring light",
        "Product table",
        "White cyc",
        "Small dark studio",
        "Wooden daylight",
        "Neon studio",
        "Umbrella cyc",
        "Car studio",
        "Car showroom",
        "Hexagon studio",
        "Road tunnel",
        "Bridge",
        "Bridge at night",
        "LED portal",
        "Open boxes",
        "Umbrellas",
        "Open loft",
        "Factory hall",
    ];

    /// The sun's elevation as the model computes it, in Time mode (NOAA) or Manual mode alike.
    fn sun_elevation(p: &HdriParams) -> f32 {
        sun_direction(&p.sky.sun).y.clamp(-1.0, 1.0).asin().to_degrees()
    }

    #[test]
    fn every_name_resolves_case_insensitively() {
        for name in PRESET_NAMES {
            assert!(preset(name).is_some(), "{name}");
            assert_eq!(preset(&name.to_uppercase()), preset(name), "{name}");
            assert_eq!(preset(&name.to_lowercase()), preset(name), "{name}");
            assert_eq!(preset(&format!("  {name} ")), preset(name), "{name}");
        }
        assert_eq!(preset("three_point"), preset("Three-point"));
        assert_eq!(preset("RING-LIGHT"), preset("Ring light"));
        assert!(preset("Nowhere").is_none());
        assert!(preset("").is_none());
    }

    #[test]
    fn presets_are_already_clamped() {
        for name in PRESET_NAMES {
            let original = preset(name).unwrap();
            let mut clamped = original.clone();
            let changed = clamped.clamp();
            assert!(changed.is_empty(), "{name} changed under clamp: {changed:?}");
            assert_eq!(clamped, original, "{name}");
        }
    }

    #[test]
    fn presets_round_trip_through_json() {
        for name in PRESET_NAMES {
            let p = preset(name).unwrap();
            let back = HdriParams::from_json(&p.to_json()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(back, p, "{name}");
        }
    }

    #[test]
    fn sky_presets_are_sky_and_studio_presets_are_studio() {
        assert_eq!(SKY.len() + STUDIO.len(), PRESET_NAMES.len());
        for name in SKY {
            let p = preset(name).unwrap();
            assert_eq!(p.mode(), Mode::Sky, "{name}");
            assert!(p.lights.is_empty(), "{name}");
        }
        for name in STUDIO {
            let p = preset(name).unwrap();
            assert_eq!(p.mode(), Mode::Studio, "{name}");
            assert!((1..=LIGHT_LIMIT).contains(&p.lights.len()), "{name}: {} lights", p.lights.len());
            assert_eq!(p.lights.iter().filter(|l| l.key).count(), 1, "{name} needs exactly one key light");
            assert!(p.lights.iter().all(|l| l.enabled), "{name}");
        }
        let ring = preset("Ring light").unwrap();
        assert_eq!(ring.lights[0].shape(), LightShape::Ring);
        assert!(ring.lights[0].key);
    }

    #[test]
    fn the_sun_sits_where_each_sky_preset_says() {
        let el = |name: &str| sun_elevation(&preset(name).unwrap());
        assert!(el("Clear noon") > 60.0, "Clear noon: {}", el("Clear noon"));
        assert!((3.0..10.0).contains(&el("Golden hour")), "Golden hour: {}", el("Golden hour"));
        assert!((-1.0..3.0).contains(&el("Sunset")), "Sunset: {}", el("Sunset"));
        assert!((-6.0..-3.0).contains(&el("Blue hour")), "Blue hour: {}", el("Blue hour"));
        for name in ["Moonlit night", "Starry night"] {
            assert!(el(name) < -6.0, "{name}: the sun is at {}", el(name));
        }
        let moonlit = preset("Moonlit night").unwrap();
        assert!(moonlit.sky.night.moon && moonlit.sky.night.moon_elevation_deg > 0.0);
        assert!(!preset("Starry night").unwrap().sky.night.moon);
    }

    #[test]
    fn a_manual_sky_presets_clock_puts_the_sun_where_the_manual_sun_is() {
        let mut manual_presets = 0;
        let mut off = Vec::new();
        for name in SKY {
            let manual = preset(name).unwrap();
            if manual.sky.sun.mode() != SunMode::Manual {
                continue;
            }
            manual_presets += 1;
            let mut timed = manual.clone();
            timed.sky.sun.mode = "time".to_string();
            let (a, b) = (sun_direction(&manual.sky.sun), sun_direction(&timed.sky.sun));
            let angle = a.dot(b).clamp(-1.0, 1.0).acos().to_degrees();
            if angle >= 2.5 {
                off.push(format!("{name} ({angle:.1} deg)"));
            }
        }
        assert_eq!(manual_presets, 19, "every sky preset but Clear noon is Manual");
        assert!(off.is_empty(), "Time mode puts the sun away from the manual sun in: {}", off.join(", "));
    }

    #[test]
    fn presets_are_all_different() {
        let jsons: Vec<String> = PRESET_NAMES.iter().map(|n| preset(n).unwrap().to_json()).collect();
        for i in 0..jsons.len() {
            for j in i + 1..jsons.len() {
                assert_ne!(jsons[i], jsons[j], "{} equals {}", PRESET_NAMES[i], PRESET_NAMES[j]);
            }
        }
    }
}

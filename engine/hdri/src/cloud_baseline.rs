//! The clouds baseline's cases (design §8): the 44 older maps it was recorded over at makepad commercial 4b4d82c2,
//! which the cloud tests run over (the lift, the clamp, the stack's guards). Their bit-for-bit comparison with dumps
//! kept in `local/` is gone: no tree that runs the tests holds those dumps.

use super::*;

/// The presets the clouds baseline recorded (design §8): the seventeen there were at its base. A preset added later is
/// not in it: it has no older map to keep.
const RECORDED_PRESETS: [&str; 17] = [
    "Clear noon",
    "Golden hour",
    "Sunset",
    "Overcast",
    "Blue hour",
    "Moonlit night",
    "Starry night",
    "Three-point",
    "Top softbox",
    "Rim pair",
    "Overcast dome",
    "Ring light",
    "White cyc",
    "Small dark studio",
    "Wooden daylight",
    "Neon studio",
    "Umbrella cyc",
];

/// The 44 maps the clouds baseline holds (design §8): every recorded preset; on Clear noon (its own cirrus taken out)
/// a deck, a cirrus, both, a volume, a volume under cirrus and a volume of no density; each weather kind over Clear
/// noon at coverage 0 and 0.5; a deck over a lake, a deck on wet ground, the floor baselines' skyline strip, the
/// moonlit night under cloud with an aurora; and X2's three thin cases.
pub(crate) fn clouds_baseline_cases() -> Vec<(String, HdriParams, ImageSet)> {
    use super::presets::preset;
    let plain = ImageSet::default();
    let mut cases: Vec<(String, HdriParams, ImageSet)> = Vec::new();
    for name in RECORDED_PRESETS {
        let snake: String = name.chars().map(|c| if c == ' ' || c == '-' { '_' } else { c.to_ascii_lowercase() }).collect();
        cases.push((format!("preset_{snake}"), preset(name).unwrap(), plain.clone()));
    }
    let mut noon = preset("Clear noon").unwrap();
    noon.sky.clouds.cirrus = 0.0;
    let with = |coverage: f32, cirrus: f32, volume: Option<VolumetricParams>| {
        let mut p = noon.clone();
        p.sky.clouds.coverage = coverage;
        p.sky.clouds.cirrus = cirrus;
        p.sky.volumetric = volume;
        p
    };
    let volume = Some(VolumetricParams::default());
    let empty = Some(VolumetricParams { density: 0.0, ..VolumetricParams::default() });
    for (name, p) in [
        ("deck", with(0.5, 0.0, None)),
        ("cirrus", with(0.0, 0.4, None)),
        ("deck_cirrus", with(0.5, 0.4, None)),
        ("volume", with(0.5, 0.0, volume.clone())),
        ("volume_cirrus", with(0.5, 0.4, volume.clone())),
        ("volume_density0", with(0.5, 0.4, empty)),
    ] {
        cases.push((name.to_string(), p, plain.clone()));
    }
    for kind in WEATHER_KIND_NAMES {
        for (tag, coverage) in [("c0", 0.0f32), ("c05", 0.5)] {
            let mut p = preset("Clear noon").unwrap();
            p.sky.clouds.coverage = coverage;
            p.sky.weather = Some(WeatherParams { kind: kind.to_string(), ..WeatherParams::default() });
            cases.push((format!("kind_{kind}_{tag}"), p, plain.clone()));
        }
    }
    let mut lake = with(0.5, 0.0, None);
    lake.floor = Some(FloorParams { kind: "lake".to_string(), ..FloorParams::default() });
    cases.push(("lake".to_string(), lake, plain.clone()));
    let mut wet = with(0.5, 0.0, None);
    wet.sky.weather = Some(WeatherParams { kind: "clear".to_string(), wet_ground: 0.7, ..WeatherParams::default() });
    cases.push(("wet_ground".to_string(), wet, plain.clone()));
    let (_, city, band) = floor::tests::floor_baseline_cases().into_iter().find(|(name, ..)| *name == "city_band").expect("the floor baselines' city_band case");
    cases.push(("skyline".to_string(), city, band));
    let mut moon = preset("Moonlit night").unwrap();
    moon.sky.clouds.coverage = 0.5;
    moon.sky.clouds.cirrus = 0.3;
    // An aurora at its defaults (on): as the clouds let it through, it is pinned in the bake, the ground's gain and the
    // strip's aurora rings.
    moon.sky.night.aurora = Some(AuroraParams::default());
    cases.push(("moon".to_string(), moon, plain.clone()));
    cases.push(("x2_deck".to_string(), with(0.05, 0.0, None), plain.clone()));
    cases.push(("x2_cirrus".to_string(), with(0.0, 0.05, None), plain.clone()));
    cases.push(("x2_volume".to_string(), with(0.05, 0.0, volume), plain));
    cases
}

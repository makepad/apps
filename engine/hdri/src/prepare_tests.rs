//! The generator's tests of makepad free's IBL preparation
//! (`makepad_render::env_map::prepare`): a generated map's key reaches the engine once.

use crate::*;
use crate::prepare::*;
use crate::image::mean_luminance;
use std::borrow::Cow;
use makepad_render_material::ibl::{self, EnvMap};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caster::fixtures::caster_at;
    use crate::sky::luminance;

    /// A w x w/2 map painted by direction at texel centres.
    fn map(w: usize, paint: impl Fn(Vec3f) -> Vec3f) -> EnvMap {
        EnvMap::from_fn(w, |d| arr(paint(vec(d))))
    }
    fn grey(v: f32) -> Vec3f {
        vec3f(v, v, v)
    }



    /// M5: the lighting copy is at most LIGHTING_MAX_WIDTH wide. A wider
    /// source's atlas, SH, meter and band are those of its 1024 wide resize
    /// with the sun filled there (the dome keeps up to 2048), and the
    /// resize does not leave the disc behind: `remove_sun` widens its cone
    /// by one texel of the copy, which holds what the resize smeared. A
    /// narrower source is not copied at all. A keyless copy (N3) is sized
    /// the same way.
    #[test]
    fn the_lighting_copy_is_at_most_1024_wide() {
        let sun_dir = dir_from_az_el(120.0, 35.0);
        let cos_radius = 0.265f32.to_radians().cos();
        let sun = EnvSun { dir: sun_dir, radiance: grey(1.0e5), cos_radius, facing: 1.0, cos_cover: cos_radius };
        let wide = map(2048, |d| if d.dot(sun_dir) >= cos_radius { grey(1.0e5) } else { vec3f(0.5, 0.5 + 0.1 * d.x, 0.5 - 0.1 * d.y) });
        assert!(mean_luminance(&wide) > 0.6, "premise: the disc moves the meter: {}", mean_luminance(&wide));
        assert_eq!(lighting_copy(&wide, KeyRemoval::Nothing, LIGHTING_MAX_WIDTH).width, LIGHTING_MAX_WIDTH);
        let lit = lighting_copy(&wide, KeyRemoval::Fill(&sun), LIGHTING_MAX_WIDTH);
        let mut want = wide.resized(LIGHTING_MAX_WIDTH);
        remove_sun(&mut want, &sun);
        assert!(*lit == want, "the 1024 wide resize with the sun filled there");
        assert!((mean_luminance(&lit) - 0.5).abs() < 0.005, "no disc left in the copy: {}", mean_luminance(&lit));
        let p = prepare_ibl_sized(&wide, Some(&sun), 1.0, 0.0, 2048, 16);
        assert_eq!(p.mean_luminance, mean_luminance(&want));
        assert_eq!(p.horizon_rgb, horizon_band(&want));
        assert_eq!(p.sh, ibl::sh9(&want));
        assert_eq!(p.dome_width, 2048, "the dome is not the lighting copy");
        let narrow = map(512, |_| grey(0.5));
        assert!(matches!(lighting_copy(&narrow, KeyRemoval::Nothing, LIGHTING_MAX_WIDTH), Cow::Borrowed(_)), "a narrower source is not copied");
        // A generated map's keyless copy is box-filtered the same way, and a
        // narrow one is the lighting copy itself.
        assert!(*lighting_copy(&narrow, KeyRemoval::Exact(&wide), LIGHTING_MAX_WIDTH) == wide.resized(LIGHTING_MAX_WIDTH));
        assert!(matches!(lighting_copy(&wide, KeyRemoval::Exact(&narrow), LIGHTING_MAX_WIDTH), Cow::Borrowed(m) if std::ptr::eq(m, &narrow)));
    }




    fn serial(n: usize, f: &(dyn Fn(usize) + Sync)) {
        for i in 0..n {
            f(i)
        }
    }

    /// f(0..n) on every core, for the finer bakes.
    fn all_cores(n: usize, f: &(dyn Fn(usize) + Sync)) {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let cores = std::thread::available_parallelism().map_or(4, |c| c.get()).min(16);
        std::thread::scope(|scope| {
            for _ in 0..cores {
                scope.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    f(i);
                });
            }
        });
    }

    /// A map's whole emission, ∫ L dΩ per channel, with exact texel solid angles.
    fn emission(env: &EnvMap) -> [f64; 3] {
        let (w, h) = (env.width, env.height);
        let pi = std::f64::consts::PI;
        let mut sum = [0.0f64; 3];
        for y in 0..h {
            let cell = std::f64::consts::TAU / w as f64 * ((pi * y as f64 / h as f64).cos() - (pi * (y + 1) as f64 / h as f64).cos());
            for t in &env.data[y * w..(y + 1) * w] {
                for k in 0..3 {
                    sum[k] += t[k] as f64 * cell;
                }
            }
        }
        sum
    }

    /// A preset with its key switched off where the key has a switch of its
    /// own: the key light disabled (a studio's, or a light over a sky), the
    /// sun's disc hidden. The moon's disc has none: `moon: false` takes the
    /// moonlit air and the moonlit ground with it.
    fn switched_off(params: &HdriParams, term: KeyTerm) -> Option<HdriParams> {
        let mut p = params.clone();
        match term {
            // A key in a room (`StudioParams::room`) has no switch of its own,
            // as the moon's disc has none: switched off it would take its light
            // off the room too, which stays in the copy (`Studio::room`).
            KeyTerm::Light if p.studio.room.is_some() => return None,
            KeyTerm::Light => {
                let key = p.lights.iter().position(|l| l.enabled && l.key && l.blend() == Blend::Add)?;
                p.lights[key].enabled = false;
            }
            KeyTerm::Sun => p.sky.sun_disc.visible = false,
            KeyTerm::Moon(_) => return None,
            KeyTerm::Image | KeyTerm::Discs | KeyTerm::DiscProbe => return None,
        }
        Some(p)
    }

    /// N3: a generated map's lighting copy is the map without exactly its
    /// key, so every preset's other lights survive in it: the Overcast dome's
    /// side panels under its 76 degree cover, a three-point's fill and rim,
    /// the sky and the clouds around the sun, the glow around the moon. The
    /// copy is compared with a bake of the preset with its key switched off
    /// (the key light disabled, the sun's disc hidden), its texels on and
    /// around the discs area-averaged as the map's are; the moon's disc has
    /// no switch of its own, so for the moonlit night it is a whole bake
    /// through the lookup without the disc. The sun's reflections (After
    /// rain's puddles) stay in the copy as the map's own light, which hiding
    /// the disc takes away too: below the horizon the copy is the map. Each
    /// preset as it is and turned by 37 degrees (the key's reach is found
    /// in world space). Tolerance: none, texel for texel, because the copy
    /// IS that bake (the same function at the same texel centres, refined
    /// the same way; outside the key's reach, the map).
    #[test]
    fn every_presets_non_key_lights_survive_in_the_lighting_copy() {
        use crate::presets::{preset, PRESET_NAMES};
        let mut off = Vec::new();
        let mut keys = 0;
        for (name, rotation_deg) in PRESET_NAMES.iter().flat_map(|n| [(*n, 0.0f32), (*n, 37.0)]) {
            let params = HdriParams { rotation_deg, ..preset(name).unwrap() };
            let baked = super::super::envmap::bake_generated_env_map(&params, 64, serial);
            let env = Env::new(&params);
            let removal = baked.keyless.as_ref().map_or(KeyRemoval::Nothing, KeyRemoval::Exact);
            let lit = lighting_copy(&baked.map, removal, LIGHTING_MAX_WIDTH);
            let want = match env.key() {
                None => {
                    assert!(baked.keyless.is_none(), "{name}: a map without a key has no keyless copy");
                    baked.map.clone()
                }
                Some((term, _)) => {
                    keys += 1;
                    match switched_off(&params, term) {
                        Some(p) => {
                            // Baked and refined through the bake's lookups (`Sampling::bake`), as the map and its
                            // copy are: they read a cloud layer over a texel's width, an exact lookup at a point, so
                            // the discs' sub-samples differ wherever a cloud stands there (the sun's, or the moon's
                            // in a day sky).
                            let without = Env::new(&p);
                            let cache = without.cloud_cache(64, volumetric::MARCH_MAX_WIDTH);
                            let at = Sampling::bake(64, cache.as_ref());
                            let radiance = |d: Vec3f| without.radiance_with(d, None, at);
                            let mut want = bake_with(64, &radiance);
                            refine_hot_spots(&mut want, &radiance, &env.hot_spots());
                            // The disc's reflections (a wet ground's puddles, a lake) are the map's own light, not
                            // the key's (the owner's ruling, 2026-11-09), so the copy keeps them; hiding the disc
                            // takes them with it. Below the horizon (its dip from a viewer height included), where
                            // nothing but they can tell the two apart, the copy is the map.
                            if term == KeyTerm::Sun {
                                let dip = env.atmo.as_ref().map_or(0.0, |atmo| atmo.horizon_dip());
                                let (w, h) = (want.width, want.height);
                                for y in (0..h).filter(|&y| std::f32::consts::FRAC_PI_2 - std::f32::consts::PI * y as f32 / h as f32 <= -dip) {
                                    want.data[y * w..(y + 1) * w].copy_from_slice(&baked.map.data[y * w..(y + 1) * w]);
                                }
                            }
                            want
                        }
                        None => env.bake_without_key(64).unwrap(),
                    }
                }
            };
            assert_eq!((lit.width, lit.height), (want.width, want.height), "{name}");
            if lit.data != want.data {
                let (got, all) = (emission(&lit), emission(&want));
                let changed = lit.data.iter().zip(&want.data).filter(|(a, b)| a != b).count();
                off.push(format!(
                    "{name} turned {rotation_deg}: {changed} texels differ; the copy holds {:.4} {:.4} {:.4} of the light the map has without its key",
                    got[0] / all[0], got[1] / all[1], got[2] / all[2]
                ));
            }
        }
        assert_eq!(keys, 2 * (PRESET_NAMES.len() - 2), "every preset but blue hour and the starry night has a key");
        assert!(off.is_empty(), "the lighting copy vs the map without its key:\n{}", off.join("\n"));
    }

    /// N3: a generated key's light reaches the engine once. What the lighting
    /// copy lacks against the map, emission(map) - emission(copy), is the
    /// light the directional light carries, `EnvSun::irradiance`: nothing of
    /// the key stays in the copy (no double count) and nothing else leaves it
    /// (no loss); a key the clouds hide entirely (the Overcast preset's)
    /// takes nothing. Measured on 1024 wide maps, the engine's bake width.
    ///
    /// Every preset, and every kind of key plan 1b brings ([`keys_1b`], the
    /// plan owner's D1: the key is measured from the map's own drawing), so
    /// the next feature cannot break it silently. Each case names its
    /// tolerance, the sampling of the key by the bake and by the key's own
    /// integral, which N3 does not change: a studio key's texels hold its
    /// integral to 0.17 % at 1024 (the thin ring; the broad keys to 0.003 %),
    /// a disc a fraction of a degree wide is area-averaged to a few percent
    /// (the moon's 0.52 degree disc, the worst, to 2.2 %), and a key whose
    /// integral is jittered (a stroke's, a window's, what hangs in front of a
    /// key) holds it to its strata. The cone fill fails it: the Overcast
    /// dome's cover held 13-16 % of the light beside the key.
    #[test]
    fn a_generated_keys_light_reaches_the_engine_once() {
        use crate::presets::{preset, PRESET_NAMES};
        let mut cases: Vec<KeyCase> = PRESET_NAMES
            .iter()
            .map(|name| {
                let params = preset(name).unwrap();
                let disc = matches!(Env::new(&params).key(), Some((KeyTerm::Sun | KeyTerm::Moon(_), _)));
                // A key with a modifier (modifier.rs) has a pattern on its face, its
                // rim, ribs or rings point-sampled by the bake: it holds the key to
                // its strata, as the 1b keys with something on or in front of them do.
                let patterned = params.lights.iter().any(|l| l.key && l.modifier.is_some());
                let tolerance = if disc {
                    0.03
                } else if patterned {
                    0.01
                } else {
                    0.002
                };
                KeyCase { name: name.to_string(), params, images: ImageSet::default(), tolerance }
            })
            .collect();
        cases.extend(keys_1b());
        let mut off = Vec::new();
        for case in &cases {
            let name = &case.name;
            let baked = super::super::envmap::bake_generated_env_map_with(&case.params, &case.images, 1024, all_cores);
            let Some(key) = baked.sun else { continue };
            let removal = baked.keyless.as_ref().map_or(KeyRemoval::Nothing, KeyRemoval::Exact);
            let (map, lit) = (emission(&baked.map), emission(&lighting_copy(&baked.map, removal, LIGHTING_MAX_WIDTH)));
            let removed = [map[0] - lit[0], map[1] - lit[1], map[2] - lit[2]];
            let light = key.irradiance();
            let light = [light.x as f64, light.y as f64, light.z as f64];
            if light == [0.0; 3] {
                if removed != [0.0; 3] {
                    off.push(format!("{name}: a key with no light took {removed:?}"));
                }
                continue;
            }
            let tolerance = case.tolerance;
            let ratio = [removed[0] / light[0], removed[1] / light[1], removed[2] / light[2]];
            println!("{name}: {:.4} {:.4} {:.4}", ratio[0], ratio[1], ratio[2]);
            if !ratio.iter().all(|r| (r - 1.0).abs() < tolerance) {
                off.push(format!("{name}: the copy lacks {:.4} {:.4} {:.4} of the key's light (within {tolerance})", ratio[0], ratio[1], ratio[2]));
            }
        }
        assert!(off.is_empty(), "the light the copy lacks vs the light the key carries:\n{}", off.join("\n"));
    }

    /// D1: a key the map does not show carries no light, so it casts no
    /// shadows from a light nobody sees: a stroke under a black flag, a
    /// softbox behind a window filled with sky (its glass shows the sky in
    /// the key's place), a picture that is all transparent, a sun behind
    /// opaque cloth or under the base image's horizon line, the moon behind a
    /// flag, a photographed sun behind a flag (a whole-base image's key).
    /// Each against the same set-up with nothing hiding the key, whose key
    /// has light.
    #[test]
    fn a_key_the_map_hides_carries_no_light() {
        let flag = |azimuth_deg: f32, elevation_deg: f32, size_deg: f32| LightParams {
            name: "Flag".to_string(),
            azimuth_deg,
            elevation_deg,
            width_deg: size_deg,
            height_deg: size_deg,
            corner: 0.0,
            softness: 0.0,
            blend: "multiply".to_string(),
            rgb: Some([0.0; 3]),
            ..LightParams::default()
        };
        let cloth = |azimuth_deg: f32, elevation_deg: f32| LightParams {
            name: "Scrim".to_string(),
            shape: "scrim".to_string(),
            azimuth_deg,
            elevation_deg,
            width_deg: 16.0,
            height_deg: 16.0,
            softness: 0.0,
            ..LightParams::default()
        };
        let photo = images_1b();
        let mut cases: Vec<(&str, HdriParams, HdriParams, &ImageSet)> = Vec::new();
        let open = studio_1b(vec![arch_key()]);
        cases.push(("a stroke under a black flag", studio_1b(vec![arch_key(), flag(90.0, 10.0, 90.0)]), open, &photo));
        // A window's glass shows the sky in place of what is behind it: one
        // pane, no frame, so all of it is glass where the softbox hangs.
        let softbox = LightParams { key: true, ..LightParams::default() };
        let pane = LightParams {
            name: "Window".to_string(),
            width_deg: 60.0,
            height_deg: 50.0,
            softness: 0.0,
            gobo: Some(GoboParams { panes_x: 1, panes_y: 1, frame: 0.0, fill_sky: true, ..GoboParams::default() }),
            ..LightParams::default()
        };
        cases.push(("a softbox behind a window filled with sky", studio_1b(vec![softbox.clone(), pane]), studio_1b(vec![softbox]), &photo));
        let clear = LightParams { image: Some(LightImageParams { key: "clear".to_string(), fit: "stretch".to_string(), alpha_as_mask: true, ..LightImageParams::default() }), ..picture_key() };
        cases.push(("an all-transparent picture", studio_1b(vec![clear]), studio_1b(vec![picture_key()]), &photo));
        let sun = manual_sun(150.0, 35.0);
        cases.push(("the sun behind opaque cloth", HdriParams { lights: vec![cloth(150.0, 35.0)], ..sun.clone() }, sun.clone(), &photo));
        cases.push(("the sun under the image's horizon line", HdriParams { base_image: Some(replace_sky(45.0, 0.0)), ..sun.clone() }, sun.clone(), &photo));
        let night = crate::presets::preset("Moonlit night").unwrap();
        let (moon_az, moon_el) = (night.sky.night.moon_azimuth_deg, night.sky.night.moon_elevation_deg);
        cases.push(("the moon behind a black flag", HdriParams { lights: vec![flag(moon_az, moon_el, 10.0)], ..night.clone() }, night, &photo));
        // A whole-base image brings its own sun: a flag over it in the map hides it from the key too.
        let photo_sun = HdriParams { base_image: Some(BaseImageParams { key: "photo_sun".to_string(), ..BaseImageParams::default() }), ..HdriParams::default() };
        cases.push(("the photographed sun behind a black flag", HdriParams { lights: vec![flag(120.0, 30.0, 10.0)], ..photo_sun.clone() }, photo_sun, &photo));
        // Redesign §5: a solid caster at strength 1 and a black tint hides
        // all of what is behind it, the sun's disc and a softbox key alike.
        let solid = |azimuth_deg: f32, elevation_deg: f32, width_deg: f32, height_deg: f32| caster_at(azimuth_deg, elevation_deg, width_deg, height_deg, None, CasterParams::default());
        cases.push(("4: sun behind a solid caster", HdriParams { lights: vec![solid(150.0, 35.0, 10.0, 10.0)], ..sun.clone() }, sun.clone(), &photo));
        let boxed = LightParams { key: true, ..LightParams::default() };
        cases.push(("4: softbox behind a solid caster", studio_1b(vec![boxed.clone(), solid(0.0, 30.0, 60.0, 50.0)]), studio_1b(vec![boxed]), &photo));
        let mut off = Vec::new();
        for (name, hidden, open, images) in cases {
            let seen = Env::new_with(&open, images).sun().map_or(0.0, |k| crate::sky::luminance(k.irradiance()));
            assert!(seen > 0.0, "{name}: the open set-up has a lit key");
            let hidden = Env::new_with(&hidden, images).sun().map_or(0.0, |k| crate::sky::luminance(k.irradiance()));
            if !(hidden <= 1.0e-6 * seen) {
                off.push(format!("{name}: the key carries {:.3e} of what it carries in the open", hidden / seen));
            }
        }
        assert!(off.is_empty(), "keys the map hides:\n{}", off.join("\n"));
    }

    /// One map of the end-to-end key test: its params, the pictures it
    /// names, and the tolerance its key is held to.
    struct KeyCase {
        name: String,
        params: HdriParams,
        images: ImageSet,
        tolerance: f64,
    }

    /// A studio of the given lights over the default grey backdrop, which a
    /// window or a scrim key hides where it hangs.
    fn studio_1b(lights: Vec<LightParams>) -> HdriParams {
        HdriParams { mode: "studio".to_string(), lights, ..HdriParams::default() }
    }

    /// The default sky with a manual sun.
    fn manual_sun(azimuth_deg: f32, elevation_deg: f32) -> HdriParams {
        let mut p = HdriParams::default();
        p.sky.sun.mode = "manual".to_string();
        p.sky.sun.azimuth_deg = azimuth_deg;
        p.sky.sun.elevation_deg = elevation_deg;
        p
    }

    /// A soft 4 degree arch as the key, with a hotspot.
    fn arch_key() -> LightParams {
        LightParams {
            key: true,
            shape: "stroke".to_string(),
            softness: 0.6,
            hotspot: 0.3,
            stroke: Some(StrokeParams { points: vec![[60.0, 5.0], [90.0, 15.0], [120.0, 5.0]], width_deg: 4.0, ..StrokeParams::default() }),
            ..LightParams::default()
        }
    }

    /// A 60 x 40 degree soft rect key showing the picture "frame".
    fn picture_key() -> LightParams {
        LightParams {
            key: true,
            azimuth_deg: 200.0,
            elevation_deg: 20.0,
            width_deg: 60.0,
            height_deg: 40.0,
            softness: 0.1,
            image: Some(LightImageParams { key: "frame".to_string(), fit: "stretch".to_string(), alpha_as_mask: true, ..LightImageParams::default() }),
            ..LightParams::default()
        }
    }

    /// P1: a 60 x 40 degree soft rect key showing the picture "logo" in fit
    /// (letterboxed above and below), its alpha cutting it out, with a
    /// penumbra, and its cut-out edges softened by `mask_softness` (P2).
    fn logo_key(penumbra: f32, mask_softness: f32) -> LightParams {
        LightParams {
            key: true,
            azimuth_deg: 150.0,
            elevation_deg: 25.0,
            width_deg: 60.0,
            height_deg: 40.0,
            softness: 0.1,
            image: Some(LightImageParams {
                key: "logo".to_string(),
                fit: "fit".to_string(),
                alpha_as_mask: true,
                penumbra: Some(penumbra),
                mask_softness: Some(mask_softness),
            }),
            ..LightParams::default()
        }
    }

    /// Sky replacement over the photograph "photo": its line at `horizon_deg`
    /// with a soft hand-over `feather_deg` wide.
    fn replace_sky(horizon_deg: f32, feather_deg: f32) -> BaseImageParams {
        BaseImageParams { key: "photo".to_string(), replace_sky: true, horizon_deg, feather_deg, ..BaseImageParams::default() }
    }

    /// The pictures the 1b cases name: "frame", a window frame whose frame
    /// and cross bars are transparent and whose glass runs warm to cool;
    /// "stripes", vertical opaque stripes on a transparent ground (a cut-out
    /// flag); "clear", all transparent; "logo" (P1), a 5:2 badge, an
    /// ellipse and four bars running warm to cool on a transparent black
    /// ground, 200 x 80 so Mask softness 1 (P2) blurs it over 4 texels; "photo", a 256 x 128 equirect of a blue sky over brown
    /// ground; and "photo_sun", the same with a 3 degree sun at azimuth 120,
    /// 30 degrees up.
    fn images_1b() -> ImageSet {
        let picture = |w: usize, h: usize, texel: &dyn Fn(usize, usize) -> [f32; 4]| EnvMap {
            width: w,
            height: h,
            data: (0..w * h).map(|i| texel(i % w, i / w)).collect(),
        };
        let mut images = ImageSet::default();
        images.insert(
            "frame",
            picture(48, 32, &|x, y| {
                let bar = x < 4 || x >= 44 || y < 4 || y >= 28 || (22..26).contains(&x) || (14..18).contains(&y);
                let t = x as f32 / 47.0;
                [1.5 - t, 1.0, 0.5 + t, if bar { 0.0 } else { 1.0 }]
            }),
        );
        images.insert("stripes", picture(32, 32, &|x, _| [0.0, 0.0, 0.0, if (x / 4) % 2 == 0 { 1.0 } else { 0.0 }]));
        images.insert("clear", picture(8, 8, &|_, _| [1.0, 1.0, 1.0, 0.0]));
        images.insert(
            "logo",
            picture(200, 80, &|x, y| {
                let (dx, dy) = ((x as f32 - 99.5) / 90.0, (y as f32 - 39.5) / 30.0);
                let bar = x % 50 < 15 && (20..60).contains(&y);
                let t = x as f32 / 199.0;
                if dx * dx + dy * dy < 1.0 || bar { [1.4 - t, 1.0, 0.4 + t, 1.0] } else { [0.0; 4] }
            }),
        );
        images.insert("photo", EnvMap::from_fn(256, |d| if d[1] > 0.0 { [0.6, 0.7, 0.9] } else { [0.12, 0.1, 0.08] }));
        let sun = dir_from_az_el(120.0, 30.0);
        images.insert("photo_sun", EnvMap::from_fn(256, |d| if vec(d).dot(sun) > 1.5f32.to_radians().cos() { [2.0e4; 3] } else if d[1] > 0.0 { [0.6, 0.7, 0.9] } else { [0.12, 0.1, 0.08] }));
        images
    }

    /// Every kind of key plan 1b brings, each where the map does something
    /// to it that the key must follow (D1): keys behind a window filled with
    /// sky, strokes under flags (a polygon flag's hotspot too), gels and
    /// cloth, a wide stroke, pictures, what hangs in front of a key, a scrim
    /// as the key (cut by its gobo, worn, under a flag, against the sky), a
    /// window filled with sky as the key (a storm's bolt in it too, in both
    /// modes), and the sun and the moon behind lights, under the base image's
    /// line, behind a rain shaft, in fog and under a cloud volume; and (R4)
    /// shadow casters in front of a softbox, a stroke, the sun and the moon,
    /// one with an image mask, and one listed before the key.
    fn keys_1b() -> Vec<KeyCase> {
        let images = images_1b();
        let soft_flag = |azimuth_deg: f32, elevation_deg: f32, size_deg: f32, rgb: [f32; 3]| LightParams {
            name: "Flag".to_string(),
            azimuth_deg,
            elevation_deg,
            width_deg: size_deg,
            height_deg: size_deg,
            softness: 0.6,
            blend: "multiply".to_string(),
            rgb: Some(rgb),
            ..LightParams::default()
        };
        let scrim = |azimuth_deg: f32, elevation_deg: f32, size_deg: f32, softness: f32| LightParams {
            name: "Scrim".to_string(),
            shape: "scrim".to_string(),
            azimuth_deg,
            elevation_deg,
            width_deg: size_deg,
            height_deg: size_deg,
            softness,
            scrim: Some(ScrimParams { source_size: 0.5, frame: 0.0 }),
            ..LightParams::default()
        };
        let with_stroke = |edit: &dyn Fn(&mut StrokeParams)| {
            let mut light = arch_key();
            edit(light.stroke.as_mut().unwrap());
            light
        };
        let sun = manual_sun(150.0, 35.0);
        let night = crate::presets::preset("Moonlit night").unwrap();
        let (moon_az, moon_el) = (night.sky.night.moon_azimuth_deg, night.sky.night.moon_elevation_deg);
        let weather = |edit: &dyn Fn(&mut WeatherParams)| {
            let mut w = WeatherParams::default();
            edit(&mut w);
            w
        };
        // A low sun behind the densest rain shaft along its circle.
        let virga_sun = {
            let mut p = manual_sun(0.0, 8.0);
            p.sky.weather = Some(weather(&|w| w.rain = 0.8));
            let rain = crate::weather::Weather::new(&p.sky, dir_from_az_el(0.0, 8.0)).expect("rain is weather");
            let through = |az: f32| rain.veil(dir_from_az_el(az, 8.0), vec3f(1.0, 1.0, 1.0), Vec3f::default(), Vec3f::default()).y;
            let az = (0..360).map(|a| a as f32).min_by(|a, b| through(*a).total_cmp(&through(*b))).unwrap();
            assert!(through(az) < 0.95 * (0..360).map(|a| through(a as f32)).fold(0.0, f32::max), "a shaft darkens the sun's circle");
            p.sky.sun.azimuth_deg = az;
            p
        };
        let window = LightParams {
            key: true,
            azimuth_deg: 0.0,
            elevation_deg: 20.0,
            width_deg: 40.0,
            height_deg: 30.0,
            softness: 0.05,
            intensity_ev: 1.0,
            gobo: Some(GoboParams { fill_sky: true, ..GoboParams::default() }),
            ..LightParams::default()
        };
        let sky_window = LightParams {
            key: false,
            azimuth_deg: 150.0,
            elevation_deg: 35.0,
            width_deg: 20.0,
            height_deg: 20.0,
            softness: 0.0,
            intensity_ev: 1.0,
            gobo: Some(GoboParams { panes_x: 1, panes_y: 1, frame: 0.0, fill_sky: true, ..GoboParams::default() }),
            ..LightParams::default()
        };
        // A window filled with sky listed after a key: its glass replaces
        // what is behind it, the key included, with the sky (its frame and
        // bars leave the key as it was).
        let window_over = |azimuth_deg: f32, elevation_deg: f32, width_deg: f32, height_deg: f32| LightParams {
            name: "Window".to_string(),
            azimuth_deg,
            elevation_deg,
            width_deg,
            height_deg,
            softness: 0.05,
            intensity_ev: 0.0,
            gobo: Some(GoboParams { fill_sky: true, ..GoboParams::default() }),
            ..LightParams::default()
        };
        let softbox = LightParams { key: true, azimuth_deg: 0.0, elevation_deg: 25.0, width_deg: 40.0, height_deg: 30.0, softness: 0.3, ..LightParams::default() };
        // A window filled with sky as the key, 3 degrees beside a storm's
        // bolt (rain with the Lightning tick), in Sky or Studio mode: the
        // channel, 0.12 degrees across, is far thinner than the key's samples
        // and the bake's texels alike (the review's item 4: these two held
        // 1.056 and 0.972 of the key; ten seeds now hold it within 0.6 %).
        let storm_window = |mode: &str, seed: u32, gobo: GoboParams| {
            let mut p = HdriParams { mode: mode.to_string(), ..HdriParams::default() };
            p.sky.weather = Some(weather(&|w| {
                w.kind = "rain".to_string();
                w.lightning = true;
                w.seed = seed;
            }));
            // Where the bolt stands: the brightest azimuth 10 degrees up.
            let storm = crate::weather::Weather::new(&p.sky, dir_from_az_el(180.0, 40.0)).expect("rain is weather");
            let channel = |az: f32| luminance(storm.lightning(dir_from_az_el(az, 10.0), 0.0));
            let bolt_az = (0..3600).map(|a| a as f32 * 0.1).max_by(|a, b| channel(*a).total_cmp(&channel(*b))).unwrap();
            p.lights = vec![LightParams {
                key: true,
                azimuth_deg: bolt_az + 3.0,
                elevation_deg: 14.0,
                width_deg: 30.0,
                height_deg: 30.0,
                softness: 0.05,
                intensity_ev: 0.0,
                gobo: Some(GoboParams { fill_sky: true, ..gobo }),
                ..LightParams::default()
            }];
            p
        };
        let one_pane = GoboParams { panes_x: 1, panes_y: 1, frame: 0.0, ..GoboParams::default() };
        // Shadow casters (redesign §5, `caster_at`): pattern layers that add
        // no light and darken what is listed before them.
        let canopy = GoboParams { kind: "leaves".to_string(), density: 0.5, seed: 4, ..GoboParams::default() };
        let stripes = GoboParams { kind: "image".to_string(), image: "stripes".to_string(), ..GoboParams::default() };
        let case = |name: &str, params: HdriParams, tolerance: f64| KeyCase { name: name.to_string(), params, images: images.clone(), tolerance };
        // The cloth boxes up close in a room on a glossy floor: an open softbox
        // and an open octabox (their bare tubes, a crisp 0.8 degree disc with
        // most of the light, which a 1K bake area-averages: `Studio::hot_cores`)
        // and an inner-baffle softbox, `key` the key.
        let open_boxes = |key: usize| {
            let kit = |kind: ModifierKind, azimuth_deg: f32, elevation_deg: f32, intensity_ev: f32, size: [f32; 2], baffle: f32| {
                let mut light = LightParams { azimuth_deg, elevation_deg, ..LightParams::default() };
                crate::modifier::apply_template(&mut light, kind);
                (light.intensity_ev, light.width_deg, light.height_deg) = (intensity_ev, size[0], size[1]);
                light.modifier.as_mut().unwrap().baffle = baffle;
                light
            };
            let mut lights = vec![
                kit(ModifierKind::Softbox, 335.0, 12.0, 5.0, [34.0, 26.0], 1.0),
                kit(ModifierKind::Octabox, 25.0, 14.0, 5.0, [34.0, 34.0], 1.0),
                kit(ModifierKind::Softbox, 80.0, 10.0, 4.5, [30.0, 22.0], 0.7),
            ];
            lights[key].key = true;
            let mut p = studio_1b(lights);
            p.studio.room = Some(RoomParams {
                width_m: 11.0,
                depth_m: 11.0,
                height_m: 4.8,
                cove_m: 1.5,
                cove_walls: "back".to_string(),
                floor_gloss: 0.15,
                floor_roughness: 0.45,
                ..RoomParams::default()
            });
            p
        };
        vec![
            case("1b: an open softbox key beside an open octabox, in a room", open_boxes(0), 0.01),
            case("1b: an open octabox key beside an open softbox, in a room", open_boxes(1), 0.01),
            case("1b: a softbox key half behind a window filled with sky", studio_1b(vec![softbox.clone(), window_over(18.0, 25.0, 30.0, 40.0)]), 0.01),
            case("1b: a stroke key half behind a window filled with sky", studio_1b(vec![arch_key(), window_over(110.0, 10.0, 30.0, 30.0)]), 0.01),
            case(
                "1b: a window filled with sky as the key, half behind another",
                studio_1b(vec![window.clone(), window_over(15.0, 25.0, 30.0, 30.0)]),
                0.02,
            ),
            case("1b: a glowing stroke key", studio_1b(vec![LightParams { glow: Some(GlowParams { strength: 0.6, reach_deg: 2.0 }), ..arch_key() }]), 0.01),
            case("1b: a stroke key half under a soft black flag", studio_1b(vec![arch_key(), soft_flag(112.0, 8.0, 24.0, [0.0; 3])]), 0.01),
            case("1b: a stroke key under a blue gel", studio_1b(vec![arch_key(), soft_flag(80.0, 12.0, 30.0, [0.3, 0.5, 1.0])]), 0.01),
            case(
                "1b: a ramped stroke key half behind a scrim",
                studio_1b(vec![with_stroke(&|s| s.end_rgb = Some([0.2, 0.4, 1.0])), scrim(72.0, 8.0, 30.0, 0.5)]),
                0.01,
            ),
            case(
                "1b: a wide closed stroke key (45 degrees)",
                studio_1b(vec![with_stroke(&|s| {
                    s.points = vec![[80.0, 0.0], [100.0, 0.0], [90.0, 15.0]];
                    s.width_deg = 45.0;
                    s.closed = true;
                })]),
                0.01,
            ),
            case(
                "1b: a stroke key behind leaves, worn",
                studio_1b(vec![LightParams {
                    gobo: Some(GoboParams { kind: "leaves".to_string(), density: 0.4, distance: 0.3, seed: 2, ..GoboParams::default() }),
                    imperfections: Some(ImperfectionParams { wrinkle: 0.8, dust: 0.4, weave: 0.5, seed: 1 }),
                    ..arch_key()
                }]),
                0.02,
            ),
            case("1b: a picture key with a masked window frame", studio_1b(vec![picture_key()]), 0.005),
            // P1: a picture's penumbra, and its softened cut-out edges, are in
            // the map's drawing, so the key carries them too.
            case("1b: a logo key in fit mode with a penumbra", studio_1b(vec![logo_key(0.5, 0.0)]), 0.005),
            case("1b: a logo key in fit mode, its cut-out edges softened", studio_1b(vec![logo_key(0.3, 1.0)]), 0.005),
            // P2: Mask softness is the picture's own, Penumbra or none.
            case("1b: a logo key, its cut-out edges softened without a penumbra", studio_1b(vec![logo_key(0.0, 0.6)]), 0.005),
            case(
                "1b: a picture key in stretch, its penumbra widening its soft edge",
                studio_1b(vec![{
                    let mut light = picture_key();
                    light.image.as_mut().unwrap().penumbra = Some(0.6);
                    light
                }]),
                0.005,
            ),
            case(
                "1b: a picture key behind a cut-out flag",
                studio_1b(vec![
                    picture_key(),
                    LightParams {
                        image: Some(LightImageParams { key: "stripes".to_string(), fit: "stretch".to_string(), alpha_as_mask: true, ..LightImageParams::default() }),
                        softness: 0.0,
                        ..soft_flag(210.0, 25.0, 30.0, [1.0; 3])
                    },
                ]),
                0.01,
            ),
            case(
                "1b: a rect key behind a window gobo, with barn doors, a grid and wear",
                studio_1b(vec![LightParams {
                    key: true,
                    azimuth_deg: 30.0,
                    elevation_deg: 25.0,
                    width_deg: 50.0,
                    height_deg: 40.0,
                    gobo: Some(GoboParams::default()),
                    aim: Some(AimParams { yaw_deg: 8.0, grid: 0.3, barn_doors: [30.0, 0.0, 45.0, 0.0], ..AimParams::default() }),
                    imperfections: Some(ImperfectionParams { wrinkle: 0.6, dust: 0.3, weave: 0.3, seed: 3 }),
                    ..LightParams::default()
                }]),
                0.02,
            ),
            case(
                "1b: a hexagon key and a scrim's cloth over it",
                studio_1b(vec![
                    LightParams { key: true, shape: "polygon".to_string(), azimuth_deg: 300.0, elevation_deg: 15.0, width_deg: 40.0, height_deg: 30.0, softness: 0.3, ..LightParams::default() },
                    scrim(310.0, 20.0, 24.0, 0.4),
                ]),
                0.002,
            ),
            // A scrim as the key (B5): the copy keeps its cloth and loses its
            // glow, so what the cloth hides stays hidden and the key is the
            // glow alone. Its window gobo cuts the cloth out where it blocks:
            // there the softbox behind shows in the map and the copy alike.
            case(
                "1b: a scrim key over a softbox, cut by a window gobo",
                studio_1b(vec![
                    LightParams { azimuth_deg: 40.0, elevation_deg: 20.0, width_deg: 20.0, height_deg: 16.0, softness: 0.3, ..LightParams::default() },
                    LightParams { key: true, gobo: Some(GoboParams::default()), scrim: Some(ScrimParams { source_size: 0.5, frame: 0.05 }), ..scrim(40.0, 20.0, 36.0, 0.3) },
                ]),
                0.02,
            ),
            case(
                "1b: a worn scrim key half under a soft black flag",
                studio_1b(vec![
                    LightParams { key: true, imperfections: Some(ImperfectionParams { wrinkle: 0.7, dust: 0.4, weave: 0.4, seed: 5 }), ..scrim(250.0, 15.0, 30.0, 0.5) },
                    soft_flag(262.0, 15.0, 20.0, [0.0; 3]),
                ]),
                0.02,
            ),
            case(
                // The cloth hides part of the sun, in the map and the copy alike;
                // the key is the cloth's glow, not the sun.
                "1b: a scrim key against the sky, the sun in its soft edge (Sky mode)",
                {
                    let light = LightParams { key: true, ..scrim(162.0, 35.0, 24.0, 1.0) };
                    let p = HdriParams { lights: vec![light.clone()], ..sun.clone() };
                    let env = Env::new(&p);
                    assert!(matches!(env.key(), Some((KeyTerm::Light, _))), "the scrim is the key");
                    let sun_dir = env.sun_dir().expect("a manual sun");
                    let cloth = crate::shaped::ShapedLight::new(&light).expect("a scrim").sample(sun_dir).mask;
                    assert!(cloth > 0.05 && cloth < 0.95, "the sun is in the cloth's soft edge: {cloth}");
                    p
                },
                0.02,
            ),
            // A polygon flag honours its hotspot (D10, L:26): it dims the key
            // most at its middle, in the map and in the key alike.
            case(
                "1b: a stroke key half under a polygon flag with a hotspot",
                studio_1b(vec![
                    arch_key(),
                    LightParams { shape: "polygon".to_string(), hotspot: 0.5, softness: 0.3, ..soft_flag(108.0, 10.0, 26.0, [0.0; 3]) },
                ]),
                0.01,
            ),
            case("1b: a window filled with sky as the key", studio_1b(vec![window.clone()]), 0.02),
            case(
                // The sun's disc is a fraction of a degree: the window's integral
                // must find it as the bake's refinement does.
                "1b: a window filled with sky as the key, the sun in it",
                {
                    let sun_dir = Env::new(&HdriParams::default()).sun_dir().expect("the default sky has a sun");
                    let (azimuth_deg, elevation_deg) = az_el_from_dir(sun_dir);
                    studio_1b(vec![LightParams { azimuth_deg, elevation_deg: elevation_deg - 4.0, intensity_ev: 0.0, ..window.clone() }])
                },
                0.03,
            ),
            case("1b: the sun in a soft scrim's edge", HdriParams { lights: vec![scrim(160.0, 35.0, 20.0, 1.0)], ..sun.clone() }, 0.03),
            case("1b: the sun behind a soft coloured gel", HdriParams { lights: vec![soft_flag(150.0, 36.0, 16.0, [0.6, 0.4, 0.2])], ..sun.clone() }, 0.03),
            case("1b: the sun in a window filled with sky at +1 EV", HdriParams { lights: vec![sky_window], ..sun.clone() }, 0.03),
            // D12 (M8): against the sky a window's frame and bars hide the sky
            // behind them; its key is still exactly the sky its glass lets in.
            case("1b: a framed window filled with sky as the key, against the sky (Sky mode)", HdriParams { lights: vec![window.clone()], ..HdriParams::default() }, 0.02),
            case("1b: a window filled with sky as the key beside a storm's bolt (Sky mode)", storm_window("sky", 2, one_pane.clone()), 0.02),
            case("1b: a window filled with sky as the key beside a storm's bolt (Studio mode)", storm_window("studio", 1, GoboParams::default()), 0.02),
            case("1b: the moon in a soft scrim's edge", HdriParams { lights: vec![scrim(moon_az, moon_el + 9.0, 16.0, 1.0)], ..night.clone() }, 0.03),
            case(
                "1b: the sun half under the image's horizon line",
                HdriParams { base_image: Some(replace_sky(6.0, 8.0)), ..manual_sun(120.0, 6.0) },
                0.03,
            ),
            case("1b: the sun behind a rain shaft", virga_sun, 0.03),
            case(
                "1b: the sun in fog",
                HdriParams { sky: SkyParams { weather: Some(weather(&|w| w.fog = 0.4)), ..sun.sky.clone() }, ..sun.clone() },
                0.03,
            ),
            case(
                // At the edge of a cloud: about half of the sun gets through.
                "1b: the sun at the edge of a cloud volume",
                {
                    let mut p = manual_sun(220.0, 35.0);
                    p.sky.clouds.coverage = 0.4;
                    p.sky.volumetric = Some(VolumetricParams::default());
                    p
                },
                0.03,
            ),
            case(
                "4: softbox key behind a leaves caster",
                studio_1b(vec![softbox.clone(), caster_at(10.0, 28.0, 50.0, 40.0, Some(canopy.clone()), CasterParams { strength: 0.9, ..CasterParams::default() })]),
                0.02,
            ),
            // Listed before the key, the caster darkens the backdrop under it and leaves the key as it is.
            case("4: caster listed before the key", studio_1b(vec![caster_at(10.0, 28.0, 50.0, 40.0, Some(canopy.clone()), CasterParams::default()), softbox.clone()]), 0.002),
            case(
                "4: sun behind a tinted caster",
                HdriParams {
                    lights: vec![caster_at(150.0, 35.0, 20.0, 20.0, Some(canopy.clone()), CasterParams { strength: 0.8, tint: [0.6, 0.4, 0.2], softness: 0.3, ..CasterParams::default() })],
                    ..sun.clone()
                },
                0.03,
            ),
            case(
                "4: moon behind a caster",
                HdriParams { lights: vec![caster_at(moon_az, moon_el, 16.0, 16.0, Some(canopy.clone()), CasterParams { softness: 0.4, ..CasterParams::default() })], ..night.clone() },
                0.03,
            ),
            case(
                "4: stroke key behind a soft caster",
                studio_1b(vec![arch_key(), caster_at(95.0, 10.0, 40.0, 30.0, Some(canopy.clone()), CasterParams { softness: 0.6, ..CasterParams::default() })]),
                0.02,
            ),
            case("4: key behind a caster with an image mask", studio_1b(vec![softbox.clone(), caster_at(5.0, 25.0, 50.0, 40.0, Some(stripes), CasterParams::default())]), 0.01),
            // R5: the sun half behind a solid caster's soft lower edge (the caster's
            // centre 10 degrees above the sun, 20 degrees tall). Its bloom thins the
            // caster round the disc, in the map and in the key alike: one raster,
            // read by the bake, the keyless copy and disc_share. Emission test only:
            // with the key disabled its bloom goes too, so "keyless == key
            // disabled" does not hold here.
            case(
                "5: sun behind a caster with bloom",
                HdriParams {
                    lights: vec![LightParams {
                        azimuth_deg: 150.0,
                        elevation_deg: 45.0,
                        width_deg: 30.0,
                        height_deg: 20.0,
                        gobo: None,
                        caster: Some(CasterParams { softness: 0.3, bloom_strength: 1.0, bloom_length_deg: 4.0, ..CasterParams::default() }),
                        ..LightParams::new_caster()
                    }],
                    ..sun.clone()
                },
                0.03,
            ),
        ]
    }


}

//! Environment maps for the engine (phase 2 of the HDRI generator): the
//! generator's params or an HDR file become the engine's own `EnvMap`, and
//! a photographed sky gives up its sun as an `EnvSun` for the renderer's
//! directional light.
//!
//! The renderer lights the world from the map without its key (prepare.rs),
//! because the directional light already carries that energy. A generated
//! map leaves exactly the key's own light out ([`bake_generated_env_map`]:
//! the same map baked without it, N3), so the lights around a wide studio
//! key stay; a sun found in a file has no such map, so its covering cone is
//! filled with the sky around it ([`remove_sun`]). `load_env_map(.., true)`
//! fills it too, so a loaded file lights the same way whichever path
//! registers it.

use makepad_render_material::ibl::EnvMap;
use super::*;
use super::image::{load_equirect, PathOrigin};
pub use makepad_render::env_map::{detect_sun, remove_sun, rows_near, GeneratedEnvMap};

/// Bakes `params` into an engine environment map `width` x `width / 2`
/// (texel centres, ibl's equirect) and reports its key light. `run`
/// spreads the work as for [`Env::new_for_width_par`] and then
/// [`Env::bake_par`] (the Env's rasters and cloud volumes, then the rows),
/// so it is called more than once: `pool.fan_out` inside a Heavy job, a
/// plain loop on the UI thread. The map is the same whichever spreads it. The sun is [`Env::sun`]:
/// world space, the rotation applied, already x 2^intensity_ev.
pub fn bake_env_map(
    params: &HdriParams,
    width: usize,
    run: impl Fn(usize, &(dyn Fn(usize) + Sync)),
) -> (EnvMap, Option<EnvSun>) {
    let (map, sun, _) = bake_env_map_with_daylight(params, width, run);
    (map, sun)
}

/// [`bake_env_map`] and the map's own sun for the daylight switches,
/// `Environment.daylight_sun`: [`Env::sun_dir`], the sky's sun wherever it is
/// (below the horizon while the moon is the key, or at twilight with no key at
/// all), world space like the key; `None` for a studio. A host that bakes a
/// map for the engine reports both.
pub fn bake_env_map_with_daylight(
    params: &HdriParams,
    width: usize,
    run: impl Fn(usize, &(dyn Fn(usize) + Sync)),
) -> (EnvMap, Option<EnvSun>, Option<Vec3f>) {
    // A shadow caster's bloom is rastered at this bake's width (R5).
    let env = Env::new_for_width_par(params, &ImageSet::default(), width, &|| false, &run).expect("an Env nothing cancels is built");
    let (sun, daylight) = (env.sun(), env.sun_dir());
    (env.bake_par(width, &run), sun, daylight)
}


/// [`bake_env_map_with_daylight`] and the map without its key: what a host
/// hands the renderer for a generated map
/// (`Renderer::register_generated_environment`; a procedural preset the
/// renderer bakes itself goes the same way). `run` spreads the rows of the
/// bake and then of the key's reach, so it is called twice. No pictures: an
/// image light draws plain. See [`bake_generated_env_map_with`].
pub fn bake_generated_env_map(params: &HdriParams, width: usize, run: impl Fn(usize, &(dyn Fn(usize) + Sync))) -> GeneratedEnvMap {
    bake_generated_env_map_with(params, &ImageSet::default(), width, run)
}

/// [`bake_generated_env_map`] with the pictures the params name by key taken
/// from `images` (1b: image lights). A key the set lacks draws as it does in
/// the app: the plain light. [`ImageStore`] fills such a set from files.
pub fn bake_generated_env_map_with(params: &HdriParams, images: &ImageSet, width: usize, run: impl Fn(usize, &(dyn Fn(usize) + Sync))) -> GeneratedEnvMap {
    // Nothing cancels it, so it is always made.
    bake_generated_env_map_cancellable(params, images, width, run, &|| false).expect("a bake nothing cancels is made")
}

/// [`bake_generated_env_map_with`] for a job a newer request can overtake
/// (the sandbox's map job): its `Env` is built through
/// [`Env::new_for_width_par`] at `width` (a caster's rasters are laid out for
/// it, their rows spread by `run`),
/// so the first build of a seed's cloud volume noise (about 2.2 s in
/// release) and the bloom's pre-pass stop when `cancelled` says so, and the
/// bake is then None (D15, L:229). The rows are `run`'s to skip; a bake that
/// was not stopped is `bake_generated_env_map_with`'s, bit for bit.
pub fn bake_generated_env_map_cancellable(
    params: &HdriParams,
    images: &ImageSet,
    width: usize,
    run: impl Fn(usize, &(dyn Fn(usize) + Sync)),
    cancelled: &dyn Fn() -> bool,
) -> Option<GeneratedEnvMap> {
    let env = Env::new_for_width_par(params, images, width, cancelled, &run)?;
    let map = env.bake_par(width, &run);
    let keyless = env.bake_keyless_par(&map, &run);
    Some(GeneratedEnvMap { map, keyless, sun: env.sun(), daylight: env.sun_dir() })
}

/// A host's loaded pictures: the set [`Env::new_with`] reads, and the keys
/// whose file could not be read. Those are not tried again, so a map that
/// is re-baked often (the sandbox's day cycle) does not go to the disk for
/// a missing file on every bake. `Env` itself still opens no file, and
/// neither does the store: the host reads (`load_missing`'s `read`) and
/// hands `set` to the bake.
#[derive(Clone, Debug, Default)]
pub struct ImageStore {
    pub set: ImageSet,
    failed: std::collections::HashSet<String>,
}

impl ImageStore {
    /// Reads every picture `params` names that is neither loaded nor failed.
    /// `read` gives a key's bytes: the host resolves the key (the app's keys
    /// are full paths; the sandbox reads a relative key inside the game's
    /// folder) and bounds the read; an `Err` (a missing file, a key the host
    /// refuses) is a failed file. A light's picture is decoded as the app
    /// decodes it (`image::decode_image`: the file's orientation, its alpha),
    /// shrunk to `MAX_LIGHT_IMAGE_SIDE`; a base image as an equirect
    /// (`image::decode_equirect`: 2:1, rolled to the engine convention,
    /// opaque). Returns one line per file that failed in this call.
    pub fn load_missing(&mut self, params: &HdriParams, read: &dyn Fn(&str) -> Result<Vec<u8>, String>) -> Vec<String> {
        let mut problems = Vec::new();
        // The base image (1b, B15): an equirect, rolled to the engine
        // convention, at full size (`image::decode_equirect`). Before the
        // light pictures, so a file named both as the base image and as a
        // light's picture is the panorama, as in the app.
        if let Some(key) = params.base_image.as_ref().map(|base| base.key.clone()).filter(|key| !key.is_empty()) {
            if !self.set.contains(&key) && !self.failed.contains(&key) {
                match read(&key).and_then(|bytes| super::image::decode_equirect(&bytes)) {
                    Ok(map) => {
                        self.set.insert(key, map);
                    }
                    Err(error) => {
                        problems.push(format!("base image {key}: {error}"));
                        self.failed.insert(key);
                    }
                }
            }
        }
        for key in super::image_light::referenced_keys(params) {
            if self.set.contains(&key) || self.failed.contains(&key) {
                continue;
            }
            match read(&key).and_then(|bytes| super::image::decode_image(&bytes)) {
                Ok(map) => {
                    let map = super::image_light::shrink_to(map, super::image_light::MAX_LIGHT_IMAGE_SIDE);
                    self.set.insert(key, map);
                }
                Err(error) => {
                    problems.push(format!("picture {key}: {error}"));
                    self.failed.insert(key);
                }
            }
        }
        problems
    }
}

/// Loads an equirect as an engine environment map (EXR, Radiance .hdr, or
/// PNG/JPG as linearised LDR; `image::load_equirect` checks 2:1 and rolls
/// the file's +X-centred columns to the engine convention). With `detect`,
/// the brightest compact region becomes the sun ([`detect_sun`]) and its
/// covering cone is filled with the sky around it ([`remove_sun`]), so the
/// renderer's directional light carries it exactly once. The returned map
/// therefore shows NO disc: a host that also draws the map as its dome
/// loads with `image::load_equirect`, calls [`detect_sun`] and declares the
/// sun on `Environment.sun` instead; the renderer fills the cone in its
/// lighting copy and the dome keeps the disc (the sandbox does this).
pub fn load_env_map(path: &std::path::Path, detect: bool) -> Result<(EnvMap, Option<EnvSun>), String> {
    // The host chose the path (D5: a preset's path rule is the app's to apply).
    let mut env = load_equirect(path, PathOrigin::Picked)?;
    let sun = if detect { detect_sun(&env) } else { None };
    if let Some(sun) = &sun {
        remove_sun(&mut env, sun);
    }
    Ok((env, sun))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sky::luminance;
    use makepad_render_material::ibl;
    use std::f32::consts::{PI, TAU};

    // Texel geometry is written out here so the tests do not lean on the
    // helpers they check. Same formulas as ibl::from_fn (texel centres).
    fn dir_at(x: usize, y: usize, w: usize, h: usize) -> Vec3f {
        vec(ibl::equirect_uv_to_dir([(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32]))
    }
    fn area_at(y: usize, w: usize, h: usize) -> f32 {
        let top = (PI * y as f32 / h as f32).cos();
        let bottom = (PI * (y + 1) as f32 / h as f32).cos();
        TAU / w as f32 * (top - bottom)
    }
    fn angle_deg(a: Vec3f, b: Vec3f) -> f32 {
        a.normalize().dot(b.normalize()).clamp(-1.0, 1.0).acos().to_degrees()
    }
    fn serial(n: usize, f: &(dyn Fn(usize) + Sync)) {
        for i in 0..n {
            f(i)
        }
    }
    fn max_luminance(env: &EnvMap) -> f32 {
        env.data.iter().map(|c| luminance(vec3f(c[0], c[1], c[2]))).fold(0.0, f32::max)
    }
    /// A map's whole emission, ∫ L dΩ per channel, with exact texel solid angles.
    fn emission(env: &EnvMap) -> Vec3f {
        let mut sum = [0.0f64; 3];
        for y in 0..env.height {
            let area = area_at(y, env.width, env.height) as f64;
            for t in &env.data[y * env.width..(y + 1) * env.width] {
                for k in 0..3 {
                    sum[k] += t[k] as f64 * area;
                }
            }
        }
        vec3f(sum[0] as f32, sum[1] as f32, sum[2] as f32)
    }

    /// A grey sky of `sky` with a disc of `radius_deg` around `dir` whose
    /// texels hold `disc`. Returns the map and the disc's energy above the
    /// sky (the sun's true irradiance on a facing surface).
    fn sky_with_disc(w: usize, sky: f32, dir: Vec3f, radius_deg: f32, disc: f32) -> (EnvMap, f32) {
        let h = w / 2;
        let cos_r = radius_deg.to_radians().cos();
        let mut data = Vec::with_capacity(w * h);
        let mut energy = 0.0;
        for y in 0..h {
            for x in 0..w {
                let inside = dir_at(x, y, w, h).dot(dir) >= cos_r;
                let v = if inside { disc } else { sky };
                if inside {
                    energy += (disc - sky) * area_at(y, w, h);
                }
                data.push([v, v, v, 1.0]);
            }
        }
        (EnvMap { width: w, height: h, data }, energy)
    }

    #[test]
    fn a_baked_map_is_the_env_and_its_sun() {
        let p = HdriParams::default();
        let (map, sun) = bake_env_map(&p, 64, serial);
        assert_eq!((map.width, map.height), (64, 32));
        let env = Env::new(&p);
        assert_eq!(map, env.bake(64), "the same texels as the serial bake");
        assert_eq!(sun, env.sun());
        // The default params are a midsummer noon sky: its sun is up.
        assert!(sun.is_some(), "the default noon sky has its sun up");
        assert!(sun.unwrap().dir.y > 0.0);
    }

    /// R5, M5: the bakes that know their width build their Env for it
    /// (`Env::new_for_width`), so a shadow caster's rasters are laid out for
    /// the bake: the map is that Env's, not the reference width's. The caster
    /// is soft, and a softness raster's cells are no finer than the bake's
    /// texel; its bloom's raster is the reference width's at 64 (no map below
    /// it rasters a bloom more coarsely, D6).
    #[test]
    fn a_generated_bake_rasters_a_caster_at_its_width() {
        let mut p = HdriParams::default();
        p.sky.sun.mode = "manual".to_string();
        p.sky.sun.azimuth_deg = 200.0;
        p.sky.sun.elevation_deg = 30.0;
        p.lights.push(LightParams {
            azimuth_deg: 200.0,
            elevation_deg: 47.0,
            width_deg: 40.0,
            height_deg: 30.0,
            gobo: Some(GoboParams { kind: "leaves".to_string(), density: 0.6, distance: 0.0, seed: 3, ..GoboParams::default() }),
            caster: Some(CasterParams { softness: 0.3, bloom_strength: 1.0, bloom_length_deg: 20.0, ..CasterParams::default() }),
            ..LightParams::new_caster()
        });
        let at_64 = Env::new_for_width(&p, &ImageSet::default(), 64, &|| false).expect("nothing cancels it").bake(64);
        assert!(bake_generated_env_map(&p, 64, serial).map == at_64, "the generated bake builds its Env at its width");
        assert!(bake_env_map(&p, 64, serial).0 == at_64, "bake_env_map builds its Env at its width");
        assert!(Env::new(&p).bake(64) != at_64, "the reference width's raster is another");
    }




    #[test]
    fn a_baked_clear_sky_gives_up_the_sun_it_was_baked_with() {
        // The generator's own maps through the photograph's route: the sun
        // `detect_sun` finds is the one `Env::sun` reported, in direction and, up to
        // the tenth of its energy its cone leaves outside, in irradiance. Above it,
        // what is brighter than the sky round the 20 degree search is gathered with
        // it, as in a photograph of a real low sun: the haze's glow round the disc
        // (its aureole) and the clouds near it, lit above the haze: up to 13 % at
        // golden hour.
        for name in ["Clear noon", "Golden hour"] {
            let params = crate::presets::preset(name).unwrap();
            let (map, key) = bake_env_map(&params, 512, serial);
            let key = key.expect("the sun is up");
            let sun = detect_sun(&map).unwrap_or_else(|| panic!("{name}: the baked sun is a sun"));
            assert!(sun.validate().is_ok(), "{name}: {sun:?}");
            assert!(angle_deg(sun.dir, key.dir) < 0.7, "{name}: found {:?}, baked at {:?}", az_el_from_dir(sun.dir), az_el_from_dir(key.dir));
            let ratio = luminance(sun.irradiance()) / luminance(key.irradiance());
            assert!(ratio > 0.85 && ratio < 1.2, "{name}: the detected irradiance is {ratio} of the baked sun's");
            assert!(sun.facing > 0.999, "{name}: {}", sun.facing);
        }
        // A sun on the horizon (Sunset, 0.8 degrees up) carries less light than the
        // haze's glow round it within the 20 degree search, as a real one does, so
        // the cone that holds 90 % of the excess is no sun's: the detector finds
        // none, or one where the key is.
        let (map, key) = bake_env_map(&crate::presets::preset("Sunset").unwrap(), 512, serial);
        let key = key.expect("the sun is up");
        if let Some(sun) = detect_sun(&map) {
            assert!(angle_deg(sun.dir, key.dir) < 0.7, "Sunset: found {:?}, baked at {:?}", az_el_from_dir(sun.dir), az_el_from_dir(key.dir));
        }
        // A closed overcast hides the disc: its key is dimmed to nothing and the map has no sun.
        let (map, _) = bake_env_map(&crate::presets::preset("Overcast").unwrap(), 512, serial);
        assert!(detect_sun(&map).is_none());
    }

    /// M6: a detected sun's radiance is the energy it gathered over the cone
    /// as its f32 cosine gives it, so `irradiance()` hands that energy back,
    /// also for a sun of one texel, whose cone keeps few digits of 1 - cos,
    /// and for one at the zenith of a fine map, whose cosine rounds to 1: its
    /// cone is the narrowest an f32 can name instead of none.
    #[test]
    fn a_detected_sun_carries_its_energy_through_its_f32_cone() {
        for (w, y) in [(1024usize, 40usize), (1024, 256), (2048, 0)] {
            let h = w / 2;
            let mut map = EnvMap::constant(w, [1.0, 1.0, 1.0]);
            map.data[y * w + 300] = [1.0e5, 1.0e5, 1.0e5, 1.0];
            let sun = detect_sun(&map).expect("one hot texel is a sun");
            assert!(sun.validate().is_ok(), "{sun:?}");
            let want = (1.0e5 - 1.0) * area_at(y, w, h);
            let got = sun.irradiance().y;
            assert!((got / want - 1.0).abs() < 1.0e-6, "{w} wide, row {y}: {got} for {want} ({sun:?})");
        }
    }

    /// N1: a bake reports the sky's own sun beside its key: under the
    /// moonlit preset the key is the moon and the sun is 30 degrees down; a
    /// studio reports none.
    #[test]
    fn a_bake_reports_the_skys_own_sun_for_the_daylight_switches() {
        let moonlit = crate::presets::preset("Moonlit night").unwrap();
        let (map, key, daylight) = bake_env_map_with_daylight(&moonlit, 32, serial);
        assert_eq!((map, key), bake_env_map(&moonlit, 32, serial), "the same map and key");
        assert_eq!(daylight, Env::new(&moonlit).sun_dir());
        assert!(daylight.is_some_and(|d| d.y < -0.4) && key.is_some_and(|k| k.dir.y > 0.4), "{daylight:?} {key:?}");
        let (_, _, studio) = bake_env_map_with_daylight(&crate::presets::preset("Three-point").unwrap(), 32, serial);
        assert_eq!(studio, None);
    }





    /// `params` with the backdrop black and every Add light but the key taken
    /// out: what is left is the key as the map draws it, behind its flags.
    fn the_key_alone(params: &HdriParams) -> HdriParams {
        let key = params.lights.iter().position(|l| l.enabled && l.key && l.blend() == Blend::Add).expect("a key light");
        let mut studio = params.studio.clone();
        studio.top = [0.0; 3];
        studio.horizon = [0.0; 3];
        studio.floor = [0.0; 3];
        HdriParams {
            studio,
            lights: params.lights.iter().enumerate().filter(|(i, l)| *i == key || l.blend() == Blend::Multiply).map(|(_, l)| l.clone()).collect(),
            ..params.clone()
        }
    }

    #[test]
    fn removing_a_studio_key_leaves_none_of_it_in_the_map() {
        // The cone `radiance` is averaged over does not hold a rect's corners or a
        // soft light's edge: the covering cone does, and remove_sun fills that one.
        let mut narrow_leaves_more = false;
        for name in ["Three-point", "Top softbox", "Rim pair", "Overcast dome", "Ring light", "Product table"] {
            let alone = the_key_alone(&crate::presets::preset(name).unwrap());
            let (mut map, sun) = bake_env_map(&alone, 256, serial);
            let sun = sun.expect("a key light");
            assert!(sun.cos_cover < sun.cos_radius, "{name}: the reach box is wider than the cone, {sun:?}");
            let whole = emission(&map);
            assert!(whole.y > 0.0, "{name}");
            let mut narrow = map.clone();
            remove_sun(&mut narrow, &EnvSun { cos_cover: sun.cos_radius, ..sun });
            narrow_leaves_more |= emission(&narrow).y > 0.005 * whole.y;
            remove_sun(&mut map, &sun);
            let left = emission(&map);
            for (got, all) in [(left.x, whole.x), (left.y, whole.y), (left.z, whole.z)] {
                assert!(got < 0.005 * all, "{name}: {got} of the key's {all} is left in the map");
            }
        }
        assert!(narrow_leaves_more, "the cone radiance is averaged over leaves part of a wide key behind: the test can see it");
    }

    #[test]
    fn a_loaded_hdr_file_finds_its_sun_and_fills_the_disc_only_when_asked() {
        let sun_dir = dir_from_az_el(200.0, 25.0);
        let (map, _) = sky_with_disc(128, 0.5, sun_dir, 4.0, 2000.0);
        let path = std::env::temp_dir().join(format!("makepad_hdri_envmap_{}.hdr", std::process::id()));
        // encode_hdr rolls to the file convention; load_equirect rolls back.
        std::fs::write(&path, super::super::export::encode_hdr(&map)).unwrap();
        let with = load_env_map(&path, true);
        let without = load_env_map(&path, false);
        let _ = std::fs::remove_file(&path);
        let ((with, sun), (without, none)) = (with.unwrap(), without.unwrap());
        assert_eq!((with.width, with.height), (128, 64));
        let sun = sun.expect("the disc is detected");
        assert!(angle_deg(sun.dir, sun_dir) < 2.0, "{:?}", az_el_from_dir(sun.dir));
        assert!(none.is_none());
        // detect = true also fills the cone (the renderer's light carries it);
        // detect = false keeps the file as it is (RGBE holds 1 % precision).
        assert!(max_luminance(&with) < 0.51, "{}", max_luminance(&with));
        assert!(max_luminance(&without) > 1900.0);
        let far = ibl::dir_to_equirect_uv(arr(dir_from_az_el(20.0, 25.0)));
        let i = ((far[1] * 64.0) as usize) * 128 + (far[0] * 128.0) as usize;
        assert!((with.data[i][1] - 0.5).abs() < 0.01 && (without.data[i][1] - 0.5).abs() < 0.01);
    }

    /// The same through OpenEXR (the spec: `load_env_map` "for .hdr and
    /// .exr"): the decoder is A8's, this pins that the route reaches it and
    /// that float precision keeps the sky exact.
    #[test]
    fn a_loaded_exr_file_finds_its_sun_too() {
        use super::super::export::{encode_exr, ExrPrecision};
        let sun_dir = dir_from_az_el(200.0, 25.0);
        let (map, _) = sky_with_disc(128, 0.5, sun_dir, 4.0, 2000.0);
        let path = std::env::temp_dir().join(format!("makepad_hdri_envmap_{}.exr", std::process::id()));
        let (bytes, report) = encode_exr(&map, ExrPrecision::Float).unwrap();
        assert_eq!(report.clipped, 0);
        std::fs::write(&path, bytes).unwrap();
        let with = load_env_map(&path, true);
        let without = load_env_map(&path, false);
        let _ = std::fs::remove_file(&path);
        let ((with, sun), (without, none)) = (with.unwrap(), without.unwrap());
        assert_eq!((with.width, with.height), (128, 64));
        let sun = sun.expect("the disc is detected");
        assert!(angle_deg(sun.dir, sun_dir) < 2.0, "{:?}", az_el_from_dir(sun.dir));
        assert!(none.is_none());
        assert!(max_luminance(&with) < 0.51 && max_luminance(&without) > 1900.0);
        let far = ibl::dir_to_equirect_uv(arr(dir_from_az_el(20.0, 25.0)));
        let i = ((far[1] * 64.0) as usize) * 128 + (far[0] * 128.0) as usize;
        assert!((with.data[i][1] - 0.5).abs() < 1.0e-4 && (without.data[i][1] - 0.5).abs() < 1.0e-4);
    }


    /// A studio map with one large image light whose picture is `key`.
    fn picture_light(key: &str) -> HdriParams {
        let mut p = HdriParams::default();
        p.mode = "studio".to_string();
        p.lights.push(LightParams {
            width_deg: 60.0,
            height_deg: 60.0,
            image: Some(LightImageParams { key: key.to_string(), ..LightImageParams::default() }),
            ..LightParams::default()
        });
        p
    }

    #[test]
    fn a_host_bakes_with_the_pictures_its_params_name() {
        let dir = std::env::temp_dir().join(format!("makepad_hdri_envmap_images_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A picture that is black on its left half and white on its right.
        let png = Cx::encode_rgba_as_png(2, 1, &[0, 0, 0, 255, 255, 255, 255, 255]).unwrap();
        std::fs::write(dir.join("half.png"), png).unwrap();
        let params = picture_light("half.png");
        // The host reads a key's bytes (here: from the test's folder; the
        // sandbox reads inside the game's folder, bounded); the store decodes.
        let read = |key: &str| std::fs::read(dir.join(key)).map_err(|e| e.to_string());
        let mut store = ImageStore::default();
        // The first call reads the file: the picture is in the set under its key.
        assert!(store.load_missing(&params, &read).is_empty());
        assert!(store.set.contains("half.png"));
        // With the pictures the light shows its picture; without them it is plain.
        let with = bake_generated_env_map_with(&params, &store.set, 64, serial);
        let plain = bake_generated_env_map(&params, 64, serial);
        assert!(with.map != plain.map, "the picture shows in the bake");
        assert!(plain == bake_generated_env_map_with(&params, &ImageSet::default(), 64, serial), "bake_generated_env_map is the empty set");
        // A loaded picture is not read again: the file may go.
        std::fs::remove_file(dir.join("half.png")).unwrap();
        assert!(store.load_missing(&params, &read).is_empty());
        assert!(store.set.contains("half.png"));
        // A file that cannot be read is reported once, and its light stays plain.
        let missing = picture_light("gone.png");
        let problems = store.load_missing(&missing, &read);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("gone.png"), "{problems:?}");
        assert!(store.load_missing(&missing, &read).is_empty(), "a failed file is not tried again");
        assert!(bake_generated_env_map_with(&missing, &store.set, 64, serial) == bake_generated_env_map(&missing, 64, serial));
        // A key the host refuses to read (the sandbox refuses one outside the
        // game's folder) fails like a missing file: reported, its light plain.
        let outside = picture_light("../outside.png");
        let refuse = |key: &str| -> Result<Vec<u8>, String> { Err(format!("`{key}` must be a relative path inside the game's folder")) };
        let problems = store.load_missing(&outside, &refuse);
        assert!(problems.len() == 1 && problems[0].contains("inside the game's folder"), "{problems:?}");
        assert!(!store.set.contains("../outside.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_host_reads_the_base_image_its_params_name() {
        let dir = std::env::temp_dir().join(format!("makepad_hdri_envmap_base_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A panorama: bright above the horizon, dark below it.
        let pano = EnvMap::from_fn(32, |d| if d[1] > 0.0 { [3.0, 3.0, 3.0] } else { [0.2, 0.2, 0.2] });
        std::fs::write(dir.join("pano.hdr"), crate::export::encode_hdr(&pano)).unwrap();
        let mut params = HdriParams::default();
        params.mode = "studio".to_string();
        params.base_image = Some(BaseImageParams { key: "pano.hdr".to_string(), ..BaseImageParams::default() });
        let read = |key: &str| std::fs::read(dir.join(key)).map_err(|e| e.to_string());
        let mut store = ImageStore::default();
        assert!(store.load_missing(&params, &read).is_empty());
        // Loaded as an equirect: 2:1, in the engine convention (up is still the bright half).
        let map = store.set.get("pano.hdr").expect("the base image is in the set");
        assert_eq!((map.width, map.height), (32, 16));
        assert!(map.sample([0.0, 1.0, 0.0])[1] > 2.5 && map.sample([0.0, -1.0, 0.0])[1] < 0.5);
        // The bake shows it; without the set the studio backdrop shows.
        let with = bake_generated_env_map_with(&params, &store.set, 64, serial).map;
        assert!(with != bake_generated_env_map(&params, 64, serial).map, "the base image shows in the bake");
        let up = with.sample([0.0, 1.0, 0.0]);
        assert!((up[1] - 3.0).abs() < 0.1, "{up:?}");
        // A file that is not there is reported once.
        params.base_image.as_mut().unwrap().key = "gone.hdr".to_string();
        let problems = store.load_missing(&params, &read);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("gone.hdr"), "{problems:?}");
        assert!(store.load_missing(&params, &read).is_empty(), "a failed file is not tried again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D13: an EXR whose alpha is 0 is a base image in the colour it stores
    /// (it loaded black since B14, L:124), in the set and in the bake.
    #[test]
    fn a_base_image_with_alpha_0_keeps_its_colour() {
        use makepad_openexr::{write_to_vec, Compression, ExrChannel, ExrImage, ExrPart};
        // An 8 x 4 sky of colour 2.5, written with A = 0 everywhere.
        let plane = vec![2.5f32; 32];
        let part = ExrPart::new(None, 8, 4, Compression::None, vec![
            ExrChannel::float("A", vec![0.0; 32]),
            ExrChannel::float("B", plane.clone()),
            ExrChannel::float("G", plane.clone()),
            ExrChannel::float("R", plane),
        ]);
        let bytes = write_to_vec(&ExrImage::single(part)).unwrap();
        let params = HdriParams {
            mode: "studio".to_string(),
            base_image: Some(BaseImageParams { key: "sky.exr".to_string(), ..BaseImageParams::default() }),
            ..HdriParams::default()
        };
        let mut store = ImageStore::default();
        assert!(store.load_missing(&params, &|_: &str| Ok(bytes.clone())).is_empty());
        let map = store.set.get("sky.exr").expect("the base image is in the set");
        assert!(map.data.iter().all(|t| *t == [2.5, 2.5, 2.5, 1.0]), "{:?}", map.data[0]);
        let baked = bake_generated_env_map_with(&params, &store.set, 32, serial).map;
        assert!(baked.data.iter().all(|t| (t[1] - 2.5).abs() < 1.0e-5), "{:?}", baked.data[0]);
    }

    /// D15 (L:229), the hosts' half: a generated bake a newer request
    /// overtakes (the sandbox's map job) stops in the first build of its
    /// seed's cloud noise, as the app's preview does (`Env::new_cancellable`),
    /// and is None; one that is not stopped is `bake_generated_env_map_with`'s
    /// to the bit. (A seed no other test uses: the kept noise is shared.)
    #[test]
    fn a_stopped_generated_bake_stops_in_its_first_cloud_noise() {
        let mut sky = HdriParams { seed: 0x4837_0232, ..HdriParams::default() };
        sky.sky.clouds.coverage = 0.6;
        sky.sky.volumetric = Some(VolumetricParams { steps: 16, ..VolumetricParams::default() });
        let images = ImageSet::default();
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let stop = || {
            asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            true
        };
        assert!(bake_generated_env_map_cancellable(&sky, &images, 16, serial, &stop).is_none(), "a stopped bake was made");
        assert!(asked.load(std::sync::atomic::Ordering::Relaxed) > 0);
        let made = bake_generated_env_map_cancellable(&sky, &images, 16, serial, &|| false).expect("not stopped");
        assert!(made == bake_generated_env_map_with(&sky, &images, 16, serial));
    }
}

//! What a cloud stack tells the rest of the map (design §2.4, §2.5, §5.7).
//!
//! - **One cover function.** [`CloudStack::transmittance_toward`] is the product over the entries, in the per-Env
//!   order, of what each passes toward a direction: its unfaded cover, or a classic volume's faded alpha as today.
//!   The compositor dims the discs by the same product, so a key and its disc agree (D1).
//! - **The key is a cone mean.** [`CloudStack::disc_transmittance`] averages the cover function over a disc's cone
//!   once a modern entry is built. A classic-only stack keeps today's centre sample, so a lifted sky keeps its key.
//! - **The weather's facts** ([`CloudFacts`]): where the rain falls from, how grey the fog is, how much virga hangs.
//! - **A held bake's settings** ([`held_params`]).

use makepad_draw::*;
use crate::cloud_light::LitEntry;
use crate::cloud_stack::CloudStack;
use crate::{dir_from_az_el, CloudGenus, CloudLayerParams, CloudStackParams, CumulusParams, HdriParams, NimbostratusParams, StratocumulusParams};

/// Below this the cover function stops walking: nothing behind shows.
pub const TRANSMITTANCE_FLOOR: f32 = 1.0e-6;
/// The rain's height when no listed layer gives one (design §2.5): the older deck's default altitude.
const RAIN_HEIGHT_M: f32 = 2000.0;
/// With no Nimbostratus listed, the layers based under this give the rain and hang the virga (design §2.5).
const LOW_BASE_M: f32 = 6000.0;
/// The disc's cone mean (design §2.4) takes this many points of a sunflower spiral over the disc. Each point is the
/// centre of an equal share of the disc's area, weighted by the disc's brightness there as the map draws it: its soft
/// edge times the sun's limb darkening (atmosphere.rs `disc_profile`, LIMB_DARKENING). The moon's disc, hard-edged,
/// takes the same limb weights. 64 (design §12.8 item 3: the key sees the fragments round the sun as the drawn map
/// does), a finer cone than the map's own 8 x 8 a texel over the few texels the disc spans: with 16, a Cumulus whose
/// crisp lobe crossed the disc (task 6c: no mist in its view) gave a key 3.3 % off the disc the map draws.
const DISC_POINTS: usize = 64;
const DISC_LIMB_DARKENING: f32 = 0.6;
const GOLDEN_ANGLE: f32 = 2.399_963;
/// The virga's band (weather.rs `Weather::virga`): sines of elevation up to VIRGA_TOP_SIN, in VIRGA_ROWS rows of
/// VIRGA_COLUMNS looks, weighted by the band's own fade.
const VIRGA_TOP_SIN: f32 = 0.45;
const VIRGA_ROWS: usize = 8;
const VIRGA_COLUMNS: usize = 32;

/// What a stack tells the weather (design §2.5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudFacts {
    /// The base of the lowest listed Nimbostratus (or Cumulonimbus, once this build draws it), at any cover, shown or
    /// not; else the lowest listed layer based under 6 km; else 2000 m. Only types this build draws count.
    pub rain_height_m: f32,
    /// G_total for the fog: 1 - (1 - g_classic)(1 - G_modern), with G_modern = 1 - prod over the modern entries of
    /// (1 - m_j). 0 when nothing is built.
    pub grey_share: f32,
    /// 1 - (1 - p_classic)(1 - s_modern).
    /// - p_classic is the largest presence of a built classic entry.
    /// - s_modern is the stand-in cover share of the virga's band under the modern Nimbostratus and the modern layers
    ///   based below 6 km.
    /// - 0 when nothing is built.
    pub virga_share: f32,
}

impl CloudFacts {
    /// `params` is the map's own (clamped) stack, its listed layers; `built` is what the Env built of it.
    pub fn of(params: &CloudStackParams, built: Option<&CloudStack>) -> CloudFacts {
        CloudFacts { rain_height_m: rain_height(params), grey_share: built.map_or(0.0, grey_share), virga_share: built.map_or(0.0, virga_share) }
    }
}

/// [`CloudFacts::rain_height_m`].
fn rain_height(params: &CloudStackParams) -> f32 {
    let (mut raining, mut low) = (f32::INFINITY, f32::INFINITY);
    for layer in &params.layers {
        let Some(genus) = layer.genus().filter(|genus| genus.is_built()) else {
            continue;
        };
        let base = layer.base_m();
        if matches!(genus, CloudGenus::Nimbostratus | CloudGenus::Cumulonimbus) {
            raining = raining.min(base);
        } else if base < LOW_BASE_M {
            low = low.min(base);
        }
    }
    if raining.is_finite() {
        raining
    } else if low.is_finite() {
        low
    } else {
        RAIN_HEIGHT_M
    }
}

/// [`CloudFacts::grey_share`]: the classic entries by their amounts (today's coverage rule), the modern ones by
/// their measured dome shares.
fn grey_share(stack: &CloudStack) -> f32 {
    let (mut classic_open, mut modern_open) = (1.0f32, 1.0f32);
    for (entry, light) in stack.entries().iter().zip(stack.lights()) {
        if entry.is_classic() {
            if let Some(amount) = entry.classic_amount() {
                classic_open *= 1.0 - amount;
            }
        } else {
            modern_open *= 1.0 - light.dome_share;
        }
    }
    1.0 - classic_open * modern_open
}

/// [`CloudFacts::virga_share`].
fn virga_share(stack: &CloudStack) -> f32 {
    let entries = stack.entries();
    // The older virga hung under the cloud a kind closed: the lifted layers keep it by their presence.
    let classic = entries.iter().filter(|entry| entry.is_classic()).fold(0.0f32, |most, entry| most.max(entry.presence()));
    let raining: Vec<usize> = (0..entries.len())
        .filter(|&i| !entries[i].is_classic() && (entries[i].genus() == CloudGenus::Nimbostratus || entries[i].base_m() < LOW_BASE_M))
        .collect();
    let mut modern = 0.0f32;
    if !raining.is_empty() {
        let (mut drawn, mut weight) = (0.0f32, 0.0f32);
        for j in 0..VIRGA_ROWS {
            let sine = VIRGA_TOP_SIN * (j as f32 + 0.5) / VIRGA_ROWS as f32;
            let band = virga_band(sine);
            let elevation = sine.asin().to_degrees();
            for k in 0..VIRGA_COLUMNS {
                let d = dir_from_az_el((k as f32 + 0.5) * 360.0 / VIRGA_COLUMNS as f32, elevation);
                let open = raining.iter().fold(1.0f32, |open, &i| open * (1.0 - unit(entries[i].stand_in(d).cover)));
                drawn += band * (1.0 - open);
                weight += band;
            }
        }
        if weight > 0.0 {
            modern = drawn / weight;
        }
    }
    1.0 - (1.0 - classic) * (1.0 - modern)
}

/// The virga's band at a sine of elevation `y`: weather.rs `Weather::virga`'s smooth(0, 0.04) x (1 - smooth(0.2,
/// 0.45)).
fn virga_band(y: f32) -> f32 {
    smooth(0.0, 0.04, y) * (1.0 - smooth(0.2, 0.45, y))
}

impl CloudStack {
    /// The one cover function (design §2.4): the product over the entries, in the per-Env order, of
    /// 1 - cover_toward(d), stopped once it is below TRANSMITTANCE_FLOOR. The compositor's Tu is the same product.
    /// 1 at and below the horizon. It never allocates.
    pub fn transmittance_toward(&self, d: Vec3f) -> f32 {
        let mut passed = 1.0f32;
        for entry in self.entries() {
            passed *= 1.0 - entry.cover_toward(d);
            if passed < TRANSMITTANCE_FLOOR {
                break;
            }
        }
        passed
    }

    /// What the stack passes toward a disc at `dir` (design §2.4: the key is a cone mean). `radius` is the angular
    /// radius of its outer edge (radians), and `softness` the width of its soft edge as a share of its own radius,
    /// centred on that radius, as `SunDiscParams::softness` and `Atmosphere::sun_outer_radius` have them (0 for a hard
    /// edge, as the moon's).
    /// - With a modern entry built, it is the mean of `transmittance_toward` over DISC_POINTS points of the disc's
    ///   cone, each weighted by the drawn disc's brightness there: its soft edge times its limb darkening. So it is the
    ///   share of the disc's light the map draws through the clouds, even where an edge crosses the disc.
    /// - On a classic-only stack, or for a point disc, it is the centre sample, as today.
    pub fn disc_transmittance(&self, dir: Vec3f, radius: f32, softness: f32) -> f32 {
        if self.all_classic() || !(radius > 0.0) {
            return self.transmittance_toward(dir);
        }
        let centre = dir.normalize();
        let (right, up) = disc_frame(centre);
        let reach = radius.min(0.5).tan();
        // Where the soft edge starts, as a share of the outer radius; at 1 there is no soft edge.
        let inner = (1.0 - 0.5 * softness) / (1.0 + 0.5 * softness);
        let (mut sum, mut weights) = (0.0f32, 0.0f32);
        for k in 0..DISC_POINTS {
            let r = ((k as f32 + 0.5) / DISC_POINTS as f32).sqrt();
            let (s, c) = (k as f32 * GOLDEN_ANGLE).sin_cos();
            let edge = if inner < 1.0 { 1.0 - smooth(inner, 1.0, r) } else { 1.0 };
            let w = edge * (1.0 - DISC_LIMB_DARKENING * (1.0 - (1.0 - r * r).max(0.0).sqrt()));
            let d = (centre + right * (r * c * reach) + up * (r * s * reach)).normalize();
            sum += w * self.transmittance_toward(d);
            weights += w;
        }
        sum / weights
    }
}

/// Two unit directions across `centre` (unit), at right angles to it and to each other.
fn disc_frame(centre: Vec3f) -> (Vec3f, Vec3f) {
    let helper = if centre.y.abs() < 0.99 { vec3f(0.0, 1.0, 0.0) } else { vec3f(1.0, 0.0, 0.0) };
    let right = Vec3f::cross(helper, centre).normalize();
    (right, Vec3f::cross(centre, right))
}

/// A held bake's settings (design §5.7).
/// - A None map: today's held form (`layer2d_params`), bit for bit.
/// - A stack: every layer's Volume off, so nothing marches while a control is held.
pub fn held_params(p: &HdriParams) -> HdriParams {
    if p.sky.cloud_stack.is_none() {
        return crate::volumetric::layer2d_params(p);
    }
    let mut held = p.clone();
    if let Some(stack) = held.sky.cloud_stack.as_mut() {
        for layer in &mut stack.layers {
            flatten(layer);
        }
    }
    held
}

/// A layer with its Volume off. Each Volume-bearing block present loses its Volume. The layer's own type's block is
/// made from the type's defaults where it has none. A Cumulus with no block marches by default, so every layer with
/// no Cumulus block gets one with Volume off: no block the held form reads, its own type's or a later Type's, marches.
fn flatten(layer: &mut CloudLayerParams) {
    if let Some(block) = layer.nimbostratus.as_mut() {
        block.volume = false;
    }
    if let Some(block) = layer.stratocumulus.as_mut() {
        block.volume = false;
    }
    if let Some(block) = layer.cumulus.as_mut() {
        block.volume = false;
    }
    if layer.cumulus.is_none() {
        layer.cumulus = Some(CumulusParams { volume: false, ..CumulusParams::default() });
    }
    match layer.genus() {
        Some(CloudGenus::Nimbostratus) if layer.nimbostratus.is_none() => layer.nimbostratus = Some(NimbostratusParams { volume: false, ..NimbostratusParams::default() }),
        Some(CloudGenus::Stratocumulus) if layer.stratocumulus.is_none() => layer.stratocumulus = Some(StratocumulusParams { volume: false, ..StratocumulusParams::default() }),
        _ => {}
    }
}

/// A cover held to 0..1, a NaN as 0.
fn unit(x: f32) -> f32 {
    if x > 0.0 { x.min(1.0) } else { 0.0 }
}

use crate::patterns::smooth;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cloud_stack::{built_on_this_thread, tests::busy_stack, StackLook};
    use crate::prepare::{lighting_copy, KeyRemoval, LIGHTING_MAX_WIDTH};
    use crate::presets::{preset, PRESET_NAMES};
    use crate::{CloudParams, Env, FloorParams, ImageSet, SkyParams, VolumetricParams, WeatherParams, CLOUD_GENUS_BUILT};
    use makepad_render_material::ibl::EnvMap;

    /// A preset lifted into layers (`lift_clouds`). With `volume` it first gets a half-covering cloud volume: the guard's
    /// `volume` case's sky over Clear noon, a classic Cumulus that marches, under its cirrus.
    fn lifted(name: &str, volume: bool) -> HdriParams {
        let mut p = preset(name).expect("a built-in preset");
        if volume {
            p.sky.clouds.coverage = 0.5;
            p.sky.volumetric = Some(VolumetricParams::default());
        }
        assert!(p.lift_clouds(), "{name}: a None map lifts");
        p
    }

    /// Looks over the sky, the horizon's fade included.
    fn sky_dirs() -> Vec<Vec3f> {
        let mut dirs = Vec::new();
        for el in [1.0f32, 3.0, 8.0, 15.0, 30.0, 50.0, 70.0, 88.0] {
            for az in (0..360).step_by(15) {
                dirs.push(dir_from_az_el(az as f32, el));
            }
        }
        dirs
    }

    /// A layer as "+ Cloud" makes it, its base, Cover and eye set.
    fn layer(genus: CloudGenus, base_m: f32, cover: f32, enabled: bool) -> CloudLayerParams {
        let mut l = CloudLayerParams::new(genus, &[], 1);
        match genus {
            CloudGenus::Cirrus => l.cirrus.as_mut().expect("its type's block").base_m = base_m,
            CloudGenus::Cirrostratus => l.cirrostratus.as_mut().expect("its type's block").base_m = base_m,
            CloudGenus::Altocumulus => l.altocumulus.as_mut().expect("its type's block").base_m = base_m,
            CloudGenus::Nimbostratus => l.nimbostratus.as_mut().expect("its type's block").base_m = base_m,
            CloudGenus::Stratus => l.stratus.as_mut().expect("its type's block").base_m = base_m,
            CloudGenus::Stratocumulus => l.stratocumulus.as_mut().expect("its type's block").base_m = base_m,
            CloudGenus::Cumulus => l.cumulus.as_mut().expect("its type's block").base_m = base_m,
            other => panic!("this build draws seven types, not {other:?}"),
        }
        l.cover = Some(cover);
        l.enabled = enabled;
        l
    }

    /// Design §2.5. The rain falls from the base of the lowest listed Nimbostratus, at any cover, shown or not; else
    /// from the lowest listed layer based under 6 km; else from 2000 m. Nothing built greys nothing and hangs no
    /// virga. A lifted Rain or Overcast sky keeps today's couplings.
    #[test]
    fn the_rain_falls_from_the_listed_layers() {
        let height = |layers: Vec<CloudLayerParams>| CloudFacts::of(&CloudStackParams { layers, ..CloudStackParams::default() }, None).rain_height_m;
        assert_eq!(height(vec![layer(CloudGenus::Stratus, 400.0, 0.6, true), layer(CloudGenus::Nimbostratus, 1500.0, 1.0, true)]), 1500.0, "the Nimbostratus, a Stratus under it");
        assert_eq!(height(vec![layer(CloudGenus::Nimbostratus, 3000.0, 1.0, true), layer(CloudGenus::Nimbostratus, 1200.0, 0.0, false)]), 1200.0, "the lowest, at Cover 0 and hidden");
        assert_eq!(height(vec![layer(CloudGenus::Cirrus, 9000.0, 0.35, true), layer(CloudGenus::Cumulus, 1200.0, 0.35, true), layer(CloudGenus::Stratus, 400.0, 0.0, true)]), 400.0, "no Nimbostratus: the lowest under 6 km");
        assert_eq!(height(vec![layer(CloudGenus::Stratocumulus, 5999.0, 0.6, true)]), 5999.0);
        assert_eq!(height(vec![layer(CloudGenus::Stratocumulus, 6000.0, 0.6, true), layer(CloudGenus::Cirrus, 9000.0, 0.35, true)]), 2000.0, "nothing under 6 km");
        assert_eq!(height(Vec::new()), 2000.0);
        let mut newer = layer(CloudGenus::Stratus, 400.0, 0.6, true);
        newer.genus = "noctilucent".to_string();
        assert_eq!(height(vec![newer]), 2000.0, "a type this build does not draw gives no rain");
        // Nothing built: no grey, no virga, and the rain still from the listed layer.
        let stack = CloudStackParams { cover: 0.0, layers: vec![layer(CloudGenus::Nimbostratus, 1000.0, 1.0, true)], ..CloudStackParams::default() };
        assert_eq!(CloudFacts::of(&stack, None), CloudFacts { rain_height_m: 1000.0, grey_share: 0.0, virga_share: 0.0 });
        // A lifted Rain or Overcast sky: the fog greyed by its closed deck's amount (the kind's floor, Weather::new's
        // coverage), the virga whole, the rain from the deck's base (today's altitude).
        for (kind, floor) in [("rain", 1.0f32), ("overcast", 0.95)] {
            let mut p = preset("Clear noon").unwrap();
            p.sky.weather = Some(WeatherParams { kind: kind.to_string(), ..WeatherParams::default() });
            let altitude = p.sky.clouds.altitude_m;
            assert!(p.lift_clouds());
            let env = Env::new(&p);
            let facts = CloudFacts::of(p.sky.cloud_stack.as_ref().unwrap(), env.cloud_stack.as_ref());
            assert!((facts.grey_share - floor).abs() < 1.0e-6, "{kind}: {}", facts.grey_share);
            assert_eq!(facts.virga_share, 1.0, "{kind}");
            assert_eq!(facts.rain_height_m, altitude, "{kind}");
        }
    }

    /// Design §2.4. The one cover function is the product over the entries in the per-Env order of what each
    /// passes, stopped below the floor, bit for bit. The compositor dims the discs by the same product, within 1e-6.
    /// Shown on lifted skies here.
    #[test]
    fn the_cover_function_is_the_entries_product() {
        for p in [lifted("Golden hour", false), lifted("Clear noon", true)] {
            let env = Env::new(&p);
            let stack = env.cloud_stack.as_ref().expect("a lifted cloudy sky builds its stack");
            let look = StackLook { held: false, cache: None, gpu: crate::cloud_volume_gpu::VolumeRecords::Attached };
            for d in sky_dirs() {
                let mut want = 1.0f32;
                for entry in stack.entries() {
                    want *= 1.0 - entry.cover_toward(d);
                    if want < TRANSMITTANCE_FLOOR {
                        break;
                    }
                }
                let got = stack.transmittance_toward(d);
                assert_eq!(got.to_bits(), want.to_bits(), "{d:?}");
                assert!((0.0..=1.0).contains(&got), "{d:?}: {got}");
                // The light a disc texel adds per unit of disc (D1): the compositor's Tu.
                let disc = |k: f32| stack.composite(d, Vec3f::default(), Vec3f::all(k), &look).y;
                let tu = (disc(2000.0) - disc(1000.0)) / 1000.0;
                assert!((tu - got).abs() < 1.0e-6, "{d:?}: the compositor's {tu} against {got}");
            }
            assert_eq!(stack.transmittance_toward(vec3f(0.0, -1.0, 0.0)), 1.0, "nothing below the horizon");
        }
    }

    /// Design §2.4. A classic-only stack keys and dims the discs by the centre sample, as today, so a
    /// lifted sky's key is its older key. The cone mean starts with the first modern entry.
    #[test]
    fn a_classic_stack_reads_the_disc_centre() {
        for p in [lifted("Golden hour", false), lifted("Clear noon", true), lifted("Moonlit night", false)] {
            let env = Env::new(&p);
            let stack = env.cloud_stack.as_ref().expect("built");
            assert!(stack.all_classic());
            for d in sky_dirs() {
                for radius in [0.0f32, 0.0046, 0.02, 0.1] {
                    for softness in [0.0f32, 0.2] {
                        assert_eq!(stack.disc_transmittance(d, radius, softness).to_bits(), stack.transmittance_toward(d).to_bits(), "{d:?} radius {radius} softness {softness}");
                    }
                }
            }
        }
    }

    /// Design §5.7. A None map's held bake is today's (layer2d_params). A stack's held bake has every layer's
    /// Volume off: each Volume-bearing block present, and its own type's block made from the type's defaults where
    /// it has none (a Cumulus marches by default). Nothing else changes. The lifted volume marches released
    /// and draws flat held.
    #[test]
    fn held_params_flatten_every_volume() {
        let mut older = preset("Clear noon").unwrap();
        older.sky.clouds.coverage = 0.5;
        older.sky.volumetric = Some(VolumetricParams::default());
        assert_eq!(held_params(&older), crate::volumetric::layer2d_params(&older));
        let mut p = HdriParams::default();
        let mut layers: Vec<CloudLayerParams> = Vec::new();
        for genus in CLOUD_GENUS_BUILT {
            let made = CloudLayerParams::new(genus, &layers, p.seed);
            layers.push(made);
        }
        // CLOUD_GENUS_BUILT is Cirrus, Cirrostratus, Altocumulus, Nimbostratus, Stratus, Stratocumulus, Cumulus.
        layers[3].nimbostratus.as_mut().unwrap().volume = true;
        layers[5].stratocumulus.as_mut().unwrap().volume = true;
        layers[6].cumulus = None;
        layers[0].cumulus = Some(CumulusParams { volume: true, ..CumulusParams::default() });
        p.sky.cloud_stack = Some(CloudStackParams { layers, ..CloudStackParams::default() });
        let held = held_params(&p);
        let (before, after) = (&p.sky.cloud_stack.as_ref().unwrap().layers, &held.sky.cloud_stack.as_ref().unwrap().layers);
        assert_eq!(before.len(), after.len());
        for (b, a) in before.iter().zip(after) {
            assert!(!a.nimbostratus_params().volume && !a.stratocumulus_params().volume && !a.cumulus_params().volume, "{}: a Volume left on", a.name);
            // Put each block's Volume back and take away a block made for the held form: it is the map's own.
            let mut back = a.clone();
            back.nimbostratus = b.nimbostratus.as_ref().map(|block| NimbostratusParams { volume: block.volume, ..a.nimbostratus.clone().expect("a block present stays") });
            back.stratocumulus = b.stratocumulus.as_ref().map(|block| StratocumulusParams { volume: block.volume, ..a.stratocumulus.clone().expect("a block present stays") });
            back.cumulus = b.cumulus.as_ref().map(|block| CumulusParams { volume: block.volume, ..a.cumulus.clone().expect("a block present stays") });
            assert_eq!(&back, b, "{}", a.name);
        }
        assert_eq!(after[6].cumulus, Some(CumulusParams { volume: false, ..CumulusParams::default() }), "a Cumulus with no block: its held form says Volume off");
        assert_eq!(held.sky.cloud_stack.as_ref().unwrap().march_steps, p.sky.cloud_stack.as_ref().unwrap().march_steps);
        let mut lifted = older.clone();
        assert!(lifted.lift_clouds());
        let marched = |q: &HdriParams| Env::new(q).cloud_stack.as_ref().map_or(0, |stack| stack.entries().iter().filter(|entry| entry.marched()).count());
        assert_eq!(marched(&lifted), 1, "the lifted volume marches released");
        assert_eq!(marched(&held_params(&lifted)), 0, "and draws flat held");
    }

    /// Runs every index on four scoped threads, interleaved, as fan_out would (floor.rs:1443).
    fn four_threads(n: usize, f: &(dyn Fn(usize) + Sync)) {
        std::thread::scope(|scope| {
            for t in 0..4 {
                scope.spawn(move || {
                    let mut i = t;
                    while i < n {
                        f(i);
                        i += 4;
                    }
                });
            }
        });
    }

    /// Everything of a map that Cover 0 must leave as the clear sky has it, as bits (design §3.4):
    /// - its 256 x 128 bake;
    /// - its key (a flag and its nine numbers) and the keyless copy;
    /// - the ground gain, the dome and the sun under the clouds;
    /// - the strip's light;
    /// - the glints;
    /// - its held bake (the lake's MirrorSky, the held clouds).
    fn clear_dump(p: &HdriParams, images: &ImageSet) -> Vec<u32> {
        let env = Env::new_with(p, images);
        let mut out: Vec<f32> = Vec::new();
        let map = env.bake_par(256, four_threads);
        out.extend(map.data.iter().flatten());
        match env.sun() {
            Some(k) => {
                out.push(1.0);
                out.extend([k.dir.x, k.dir.y, k.dir.z, k.radiance.x, k.radiance.y, k.radiance.z, k.cos_radius, k.facing, k.cos_cover]);
                if let Some(keyless) = env.bake_keyless_par(&map, four_threads) {
                    out.extend(keyless.data.iter().flatten());
                }
            }
            None => out.push(0.0),
        }
        for v in [env.ground_gain, env.dome, env.weather_sun] {
            out.extend([v.x, v.y, v.z]);
        }
        let l = env.skyline_light();
        out.extend([l.sun_u, l.sun_el_deg]);
        out.extend(l.sun_rgb.iter().chain(&l.ambient_rgb));
        for ring in [&l.ring_low, &l.ring_high, &l.ground_ring, &l.sea_low, &l.sea_high, &l.ground_damp] {
            out.extend(ring.iter().flatten());
        }
        out.push(l.wet);
        for (dir, light, radius) in &env.glints {
            out.extend([dir.x, dir.y, dir.z, light.x, light.y, light.z, *radius]);
        }
        let held = env.with_held_floor().with_held_clouds();
        out.extend(held.bake_par(256, four_threads).data.iter().flatten());
        out.into_iter().map(f32::to_bits).collect()
    }

    /// The maps Cover 0 is held on (design §3.4).
    /// - Every preset lifted, and the busy stack, under no weather, Rain and Storm.
    /// - The busy stack over the floor baselines' skyline strip, a lake and the moon, under the same three.
    /// - The busy stack on wet ground under a clear kind, Rain and Storm.
    fn cover_zero_cases() -> Vec<(String, HdriParams, ImageSet)> {
        let plain = ImageSet::default();
        let busy = busy_stack();
        let mut skies: Vec<(String, HdriParams, ImageSet)> = Vec::new();
        // (the presets without layers: the cloud-type skies are made with one)
        for name in PRESET_NAMES.iter().filter(|name| preset(name).unwrap().sky.cloud_stack.is_none()) {
            let mut p = preset(name).unwrap();
            assert!(p.lift_clouds(), "{name}: a None map lifts");
            skies.push((name.to_string(), p, plain.clone()));
        }
        skies.push(("the busy stack".to_string(), busy.clone(), plain.clone()));
        let (_, city, band) = crate::floor::tests::floor_baseline_cases().into_iter().find(|(name, _, _)| *name == "city_band").expect("the floor baselines' strip");
        skies.push(("the busy stack over a skyline".to_string(), HdriParams { skyline: city.skyline.clone(), ..busy.clone() }, band));
        skies.push(("the busy stack over a lake".to_string(), HdriParams { floor: Some(FloorParams { kind: "lake".to_string(), ..FloorParams::default() }), ..busy.clone() }, plain.clone()));
        let mut moon = preset("Moonlit night").unwrap();
        moon.sky.clouds = CloudParams::default();
        moon.sky.cloud_stack = busy.sky.cloud_stack.clone();
        skies.push(("the busy stack under the moon".to_string(), moon, plain.clone()));
        let mut out = Vec::new();
        for (name, p, images) in skies {
            for kind in [None, Some("rain"), Some("storm")] {
                let mut q = p.clone();
                q.sky.weather = kind.map(|kind| WeatherParams { kind: kind.to_string(), ..WeatherParams::default() });
                out.push((format!("{name}, {}", kind.unwrap_or("no weather")), q, images.clone()));
            }
        }
        for kind in ["clear", "rain", "storm"] {
            let mut q = busy.clone();
            q.sky.weather = Some(WeatherParams { kind: kind.to_string(), wet_ground: 0.7, ..WeatherParams::default() });
            out.push((format!("the busy stack on wet ground, {kind}"), q, plain.clone()));
        }
        out
    }

    /// Design §3.4: master Cover 0 is the same stack with every layer hidden, bit for bit, on every part of the map
    /// `clear_dump` holds, and nothing was built. Without weather it is also the empty stack's map and the None
    /// map's with no cloud at all.
    #[test]
    fn cover_zero_is_the_clear_sky_bit_for_bit() {
        let mut off = Vec::new();
        for (name, p, images) in cover_zero_cases() {
            let mut zero = p.clone();
            zero.sky.cloud_stack.as_mut().expect("a stack map").cover = 0.0;
            let mut hidden = p.clone();
            for layer in &mut hidden.sky.cloud_stack.as_mut().unwrap().layers {
                layer.enabled = false;
            }
            let built = built_on_this_thread();
            let (got, want) = (clear_dump(&zero, &images), clear_dump(&hidden, &images));
            if built_on_this_thread() != built {
                off.push(format!("{name}: something was built"));
            }
            if got != want {
                let first = got.iter().zip(&want).position(|(g, w)| g != w);
                off.push(format!("{name}: {} floats against {}, the first difference at {first:?}", got.len(), want.len()));
            }
            if p.sky.weather.is_none() {
                let empty = HdriParams { sky: SkyParams { cloud_stack: Some(CloudStackParams::default()), ..p.sky.clone() }, ..p.clone() };
                let none = HdriParams { sky: SkyParams { cloud_stack: None, clouds: CloudParams { coverage: 0.0, cirrus: 0.0, ..CloudParams::default() }, volumetric: None, ..p.sky.clone() }, ..p.clone() };
                for (what, other) in [("the empty stack", empty), ("the None map with no cloud", none)] {
                    if clear_dump(&other, &images) != got {
                        off.push(format!("{name}: Cover 0 is not {what}"));
                    }
                }
            }
        }
        assert!(off.is_empty(), "Cover 0 against the clear sky:\n{}", off.join("\n"));
    }

    /// Design §3.4: every layer's own Cover at 0 is the master's 0, bit for bit, and builds nothing. The Cover is set
    /// through the rows' edit, so a lifted layer's amount follows it (`edited`).
    #[test]
    fn every_layer_at_cover_zero_is_cover_zero() {
        let mut maps: Vec<(String, HdriParams)> = PRESET_NAMES
            .iter()
            .filter(|name| preset(name).unwrap().sky.cloud_stack.is_none())
            .map(|name| {
                let mut p = preset(name).unwrap();
                assert!(p.lift_clouds());
                (name.to_string(), p)
            })
            .collect();
        maps.push(("the busy stack".to_string(), busy_stack()));
        let images = ImageSet::default();
        for (name, p) in maps {
            let mut each = p.clone();
            for layer in &mut each.sky.cloud_stack.as_mut().unwrap().layers {
                layer.cover = Some(0.0);
                layer.edited("cover");
            }
            let mut zero = p.clone();
            zero.sky.cloud_stack.as_mut().unwrap().cover = 0.0;
            let built = built_on_this_thread();
            assert!(clear_dump(&each, &images) == clear_dump(&zero, &images), "{name}: every layer at Cover 0 is not the master's 0");
            assert_eq!(built_on_this_thread(), built, "{name}: something was built");
        }
    }

    /// A map's whole emission, the integral of L over the sphere per channel, with exact texel solid angles
    /// (prepare_tests.rs's `emission`).
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

    /// Design §2.4 (D1): the key is the disc the map draws. What the keyless copy lacks against the map is the key's
    /// light, within prepare_tests' 0.03 for a disc (`a_generated_keys_light_reaches_the_engine_once`), on 1024 maps.
    /// - Each of the five types alone at Cover 0.5, with the sun at the first look, in a fixed order, whose cone mean
    ///   lies in 0.1..0.9. The sheets are searched from 35 to 60 degrees up. The two slabs are searched from 5 degrees
    ///   up: they are low, so above 35 degrees a Nimbostratus at 1000 m is seen within 1.4 km and a Stratus at 400 m
    ///   within 0.6 km, less than one of their cells, and no edge can pass the sun there. A placed type (a Cumulonimbus
    ///   25 km out) stands in a few degrees of sky under about 25 degrees: its edge is searched
    ///   along the middle of its sky box (`PlacedVolume::sky_box`), a twentieth of a degree a step.
    /// - A lifted cloud volume with the sun at its edge: prepare_tests' 1b case, which keys by its centre sample.
    #[test]
    fn the_key_is_the_disc_the_map_draws() {
        let mut cases: Vec<(String, HdriParams)> = Vec::new();
        for genus in CLOUD_GENUS_BUILT {
            let mut p = HdriParams::default();
            p.sky.sun.mode = "manual".to_string();
            p.sky.sun.azimuth_deg = 180.0;
            p.sky.sun.elevation_deg = 35.0;
            let mut layer = CloudLayerParams::new(genus, &[], p.seed);
            layer.cover = Some(0.5);
            p.sky.cloud_stack = Some(CloudStackParams { layers: vec![layer], ..CloudStackParams::default() });
            // A layer's cover does not depend on the sun, so one stack serves the search.
            let env = Env::new(&p);
            let stack = env.cloud_stack.as_ref().expect("a layer at Cover 0.5 is built");
            let radius = env.atmo.as_ref().unwrap().sun_outer_radius();
            let lowest = match genus {
                CloudGenus::Stratus | CloudGenus::Nimbostratus => 5,
                _ => 35,
            };
            let mut found = None;
            if let Some(bx) = stack.entries()[0].sky_box() {
                let el = 0.5 * (bx[2] + bx[3]);
                for i in 0..=((bx[1] - bx[0]) / 0.05) as i32 {
                    let az = bx[0] + 0.05 * i as f32;
                    let d = dir_from_az_el(az, el);
                    let cone = stack.disc_transmittance(d, radius, p.sky.sun_disc.softness);
                    if (0.1..=0.9).contains(&cone) {
                        found = Some((az.rem_euclid(360.0), el, cone, stack.transmittance_toward(d)));
                        break;
                    }
                }
            }
            'search: for el in (lowest..=60).step_by(5) {
                if found.is_some() {
                    break;
                }
                for az in (0..360).step_by(4) {
                    let d = dir_from_az_el(az as f32, el as f32);
                    let cone = stack.disc_transmittance(d, radius, p.sky.sun_disc.softness);
                    if (0.1..=0.9).contains(&cone) {
                        found = Some((az as f32, el as f32, cone, stack.transmittance_toward(d)));
                        break 'search;
                    }
                }
            }
            let (az, el, cone, centre) = found.unwrap_or_else(|| panic!("{}: no partly covered sun", genus.label()));
            println!("{}: the sun at ({az}, {el}), cone mean {cone:.4}, centre {centre:.4}", genus.label());
            p.sky.sun.azimuth_deg = az;
            p.sky.sun.elevation_deg = el;
            cases.push((genus.label().to_string(), p));
        }
        let mut volume = HdriParams::default();
        volume.sky.sun.mode = "manual".to_string();
        volume.sky.sun.azimuth_deg = 220.0;
        volume.sky.sun.elevation_deg = 35.0;
        volume.sky.clouds.coverage = 0.4;
        volume.sky.volumetric = Some(VolumetricParams::default());
        assert!(volume.lift_clouds());
        cases.push(("a lifted cloud volume".to_string(), volume));
        let mut off = Vec::new();
        for (name, p) in &cases {
            let baked = crate::envmap::bake_generated_env_map_with(p, &ImageSet::default(), 1024, four_threads);
            let key = baked.sun.expect("the sun is up");
            let removal = baked.keyless.as_ref().map_or(KeyRemoval::Nothing, KeyRemoval::Exact);
            let (map, lit) = (emission(&baked.map), emission(&lighting_copy(&baked.map, removal, LIGHTING_MAX_WIDTH)));
            let light = key.irradiance();
            let light = [light.x as f64, light.y as f64, light.z as f64];
            assert!(light.iter().all(|&l| l > 0.0), "{name}: the premise, a key with light");
            let ratio = [(map[0] - lit[0]) / light[0], (map[1] - lit[1]) / light[1], (map[2] - lit[2]) / light[2]];
            println!("{name}: the copy lacks {:.4} {:.4} {:.4} of the key's light", ratio[0], ratio[1], ratio[2]);
            if !ratio.iter().all(|r| (r - 1.0).abs() < 0.03) {
                off.push(format!("{name}: {:.4} {:.4} {:.4}", ratio[0], ratio[1], ratio[2]));
            }
        }
        assert!(off.is_empty(), "the light the copy lacks against the key's (within 0.03):\n{}", off.join("\n"));
    }

    /// A sky with the sun 40 degrees up in the south, whose cloud volume of `coverage` is lifted into layers: its
    /// classic Cumulus marches.
    fn lifted_volume(coverage: f32) -> HdriParams {
        let mut p = HdriParams::default();
        p.sky.sun.mode = "manual".to_string();
        p.sky.sun.elevation_deg = 40.0;
        p.sky.sun.azimuth_deg = 180.0;
        p.sky.clouds.coverage = coverage;
        p.sky.volumetric = Some(VolumetricParams::default());
        assert!(p.lift_clouds());
        p
    }

    /// Design §2.1 step 4: the light pass reads a marched volume through its stand-in and never marches it,
    /// yet measures the share of the dome the volume hides. The lifted volume's dome share is within 0.03 of its
    /// marched cover over the same 256 looks, at coverage 0.3, 0.5 and 0.75.
    #[test]
    fn the_dome_reads_volumes_through_their_stand_ins() {
        for coverage in [0.3f32, 0.5, 0.75] {
            let env = Env::new(&lifted_volume(coverage));
            let stack = env.cloud_stack.as_ref().expect("the lifted volume is built");
            let i = stack.entries().iter().position(|entry| entry.marched()).expect("it marches released");
            let mut marched = 0.0f32;
            for j in 0..crate::DOME_ROWS {
                for k in 0..crate::DOME_COLUMNS {
                    marched += stack.entries()[i].cover_toward(crate::dome_look(j, k).0);
                }
            }
            let marched = marched / (crate::DOME_ROWS * crate::DOME_COLUMNS) as f32;
            let share = stack.lights()[i].dome_share;
            println!("coverage {coverage}: the stand-in's dome share {share:.4}, the march's {marched:.4}");
            assert!((share - marched).abs() < 0.03, "coverage {coverage}: {share} against {marched}");
        }
    }

    /// Design §8: the stand-in hides what the volume hides. Above 45 degrees their mean covers agree within 0.1.
    /// Above 20 degrees they correlate at 0.8 or more, look by look (seed 1, coverage 0.35 to 0.75).
    #[test]
    fn the_stand_in_hides_what_the_volume_hides() {
        let mut off = Vec::new();
        for coverage in [0.35f32, 0.5, 0.65, 0.75] {
            let env = Env::new(&lifted_volume(coverage));
            let stack = env.cloud_stack.as_ref().expect("the lifted volume is built");
            let entry = stack.entries().iter().find(|entry| entry.marched()).expect("it marches released");
            let (mut stand_ins, mut marched, mut high) = (0.0f32, 0.0f32, 0u32);
            let mut pairs = Vec::new();
            for el in (20..=85).step_by(5) {
                for az in (0..360).step_by(10) {
                    let d = dir_from_az_el(az as f32, el as f32);
                    let (s, v) = (entry.stand_in(d).cover, entry.cover_toward(d));
                    if el >= 45 {
                        stand_ins += s;
                        marched += v;
                        high += 1;
                    }
                    pairs.push((s as f64, v as f64));
                }
            }
            let (s, v) = (stand_ins / high as f32, marched / high as f32);
            let r = correlation(&pairs);
            println!("coverage {coverage}: above 45 degrees the stand-in hides {s:.3}, the volume {v:.3}; above 20 degrees they correlate at {r:.3}");
            if (s - v).abs() > 0.1 || !(r >= 0.8) {
                off.push(format!("coverage {coverage}: {s:.3} against {v:.3}, correlation {r:.3}"));
            }
        }
        assert!(off.is_empty(), "the stand-in against the volume:\n{}", off.join("\n"));
    }

    /// Pearson's correlation of the pairs.
    fn correlation(pairs: &[(f64, f64)]) -> f64 {
        let n = pairs.len() as f64;
        let (mx, my) = (pairs.iter().map(|p| p.0).sum::<f64>() / n, pairs.iter().map(|p| p.1).sum::<f64>() / n);
        let (mut cov, mut vx, mut vy) = (0.0f64, 0.0f64, 0.0f64);
        for &(x, y) in pairs {
            cov += (x - mx) * (y - my);
            vx += (x - mx) * (x - mx);
            vy += (y - my) * (y - my);
        }
        cov / (vx * vy).sqrt()
    }
}

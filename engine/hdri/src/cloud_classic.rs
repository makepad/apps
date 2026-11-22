//! The Classic class (design §1.2): a lifted layer, drawn by today's deck, cirrus or volume code from its parts.
//!
//! - **Amount** presence x classic.coverage (presence: the Clouds Cover times the eye); sharpness classic.sharpness;
//!   the cell size_km x 1000 m (exactly today's 3000 x scale for every preset's scale).
//! - **Where.** A deck at the height its layer stands (`stand_height_m`: its base, or a Cumulus' base + 0.3 of its
//!   thickness), a cirrus at its base, the volume between base_m and tops_m.
//! - **Streams** from cloud_layer_seed (seed 0 is the map seed); the cirrus' wind today's seeded angle while neither
//!   wind is set; Drift moves the pattern downwind. A volume takes the block's density, the erosion (a Cumulus'
//!   Puffiness, 0.5 for a Stratocumulus or Nimbostratus) and the stack's march_steps; at density 0 it builds nothing
//!   (D8). Only the stack's one marched layer (`marched_cloud_layer`) is a Volume; a further layer with Volume on
//!   draws its deck.
//!
//! Light is the stack's (cloud_light.rs): `flat_radiance` is today's shade with the light pass's sun and greyed sky,
//! the sun shaded by the layers above (t_above), times Brightness, premultiplied as sky_layers_on composites it.

use makepad_draw::*;
use crate::clouds::{seeded_wind, CloudLayer, CloudParts, CloudSample};
use crate::caster::RowRunner;
use crate::volumetric::{Cancelled, VolumeParts, VolumetricClouds};
use crate::{cloud_layer_seed, ClassicLook, CloudGenus, CloudLayerParams};

/// Where a layer stands, the stack's order key: a Cumulus, an Altocumulus (its shell), and a Stratocumulus or
/// Nimbostratus with Volume on (marched or drawn flat), at base + 0.3 x (tops - base), where the held form draws a
/// marched layer (design §5.7); every other layer, Cirrus and Stratus included, at its base.
pub fn stand_height_m(layer: &CloudLayerParams, genus: CloudGenus) -> f32 {
    let lifted = |base: f32, tops: f32| base + 0.3 * (tops - base);
    match genus {
        CloudGenus::Cumulus => {
            let block = layer.cumulus_params();
            lifted(block.base_m, block.tops_m)
        }
        CloudGenus::Altocumulus => {
            let (base_m, top_m) = crate::cloud_volume_altocumulus::shell_m(&layer.altocumulus_params());
            lifted(base_m, top_m)
        }
        CloudGenus::Stratocumulus => {
            let block = layer.stratocumulus_params();
            if block.volume { lifted(block.base_m, block.tops_m) } else { block.base_m }
        }
        CloudGenus::Nimbostratus => {
            let block = layer.nimbostratus_params();
            if block.volume { lifted(block.base_m, block.tops_m) } else { block.base_m }
        }
        _ => layer.base_m(),
    }
}

/// The way a wind from `deg` (compass, clockwise from north, -Z) blows: (-sin a, cos a) in (x east, z south).
pub fn downwind(deg: f32) -> Vec2f {
    let a = deg.to_radians();
    vec2f(-a.sin(), a.cos())
}

/// What the stack's build hands a lifted layer.
pub struct ClassicSpec<'a> {
    pub layer: &'a CloudLayerParams,
    pub genus: CloudGenus,
    pub look: &'a ClassicLook,
    pub map_seed: u32,
    /// The Clouds Cover times the eye.
    pub presence: f32,
    /// The layer holds the stack's one march (`marched_cloud_layer`).
    pub marches: bool,
    /// The stack's step budget, and its wind when it is set.
    pub march_steps: u32,
    pub stack_wind_deg: Option<f32>,
}

/// One lifted layer, ready to draw.
pub enum ClassicEntry {
    /// A deck: a lifted Stratocumulus, Nimbostratus or Stratus, or a volume's layer drawn flat.
    Deck(CloudLayer),
    /// A cirrus veil at its base.
    Cirrus(CloudLayer),
    /// The stack's one marched volume, and the deck on its footprint. The light pass reads the volume through its
    /// footprint along each look's slanted path (`stand_in`).
    Volume { volume: VolumetricClouds, footprint: CloudLayer },
}

impl ClassicEntry {
    /// The lifted layer of `spec`, or Ok(None) when it draws nothing (no amount, or a volume of density 0: D8).
    /// Err(Cancelled) when `cancelled` stopped its volume's first noise build (D15), whose rows `run` spreads.
    pub fn build(spec: &ClassicSpec, sun_dir: Vec3f, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<ClassicEntry>, Cancelled> {
        let layer = spec.layer;
        let amount = spec.presence * spec.look.coverage;
        if !(amount > 0.0) {
            return Ok(None);
        }
        let stream = cloud_layer_seed(spec.map_seed, layer.seed, spec.genus);
        let along = match layer.wind_deg.or(spec.stack_wind_deg) {
            Some(deg) => downwind(deg),
            None => seeded_wind(stream),
        };
        let drift_m = layer.drift_km.unwrap_or(0.0) * 1000.0;
        // An exact zero when there is no drift, so the pattern's lookups keep their bits.
        let drift_in = |cell: f32| if drift_m != 0.0 { along * (drift_m / cell) } else { Vec2f::default() };
        if spec.genus == CloudGenus::Cirrus {
            let block = layer.cirrus_params();
            let cell = block.size_km * 1000.0;
            let parts = CloudParts {
                coverage: 0.0,
                sharpness: spec.look.sharpness,
                cell_m: cell,
                altitude_m: block.base_m,
                cirrus: amount,
                cirrus_cell_m: cell,
                cirrus_altitude_m: block.base_m,
                seed: stream,
                wind: along,
                greyed: 0.5,
                drift_cells: Vec2f::default(),
                cirrus_drift_cells: drift_in(cell),
            };
            return Ok(CloudLayer::from_parts(&parts, sun_dir).map(ClassicEntry::Cirrus));
        }
        let (base, tops, size_km, volume, density, erosion) = match spec.genus {
            CloudGenus::Stratocumulus => {
                let b = layer.stratocumulus_params();
                (b.base_m, b.tops_m, b.size_km, b.volume, b.density, 0.5)
            }
            CloudGenus::Nimbostratus => {
                let b = layer.nimbostratus_params();
                (b.base_m, b.tops_m, b.size_km, b.volume, b.density, 0.5)
            }
            CloudGenus::Stratus => {
                let b = layer.stratus_params();
                (b.base_m, b.base_m, b.size_km, false, 1.0, 0.5)
            }
            CloudGenus::Cumulus => {
                let b = layer.cumulus_params();
                (b.base_m, b.tops_m, b.size_km, b.volume, b.density, b.puffiness)
            }
            // The types this build does not draw never reach here: the stack's build gates them out (design §3.3).
            _ => return Ok(None),
        };
        let cell = size_km * 1000.0;
        let height = stand_height_m(layer, spec.genus);
        let deck = CloudParts {
            coverage: amount,
            sharpness: spec.look.sharpness,
            cell_m: cell,
            altitude_m: height,
            cirrus: 0.0,
            cirrus_cell_m: cell,
            cirrus_altitude_m: height,
            seed: stream,
            wind: along,
            greyed: 0.5 + 0.5 * amount,
            drift_cells: drift_in(cell),
            cirrus_drift_cells: Vec2f::default(),
        };
        if volume && spec.marches {
            let parts = VolumeParts {
                footprint: CloudParts { drift_cells: Vec2f::default(), ..deck },
                base_m: base,
                top_m: tops,
                density,
                erosion,
                steps: spec.march_steps,
                noise_scale: size_km / 3.0,
                noise_seed: stream,
                greyed: 0.5 + 0.5 * amount,
                drift_m: if drift_m != 0.0 { along * drift_m } else { Vec2f::default() },
            };
            let Some(volume) = VolumetricClouds::from_parts(&parts, sun_dir, cancelled, run)? else {
                return Ok(None);
            };
            let Some(footprint) = CloudLayer::from_parts(&deck, sun_dir) else {
                return Ok(None);
            };
            return Ok(Some(ClassicEntry::Volume { volume, footprint }));
        }
        Ok(CloudLayer::from_parts(&deck, sun_dir).map(ClassicEntry::Deck))
    }

    /// A deck's or a cirrus' visible sample (today's alpha, self-shadow and sun cosine) and unfaded cover along `d`,
    /// and where the ray met its plane (metres, the flat deck_point) and that plane's height. None for the volume.
    pub fn flat_sample(&self, d: Vec3f) -> Option<(CloudSample, f32, Vec2f, f32)> {
        match self {
            ClassicEntry::Deck(layer) | ClassicEntry::Cirrus(layer) => {
                let (sample, cover) = layer.sample_one(d);
                let (hit_m, height_m) = layer.hit_m(d);
                Some((sample, cover, hit_m, height_m))
            }
            ClassicEntry::Volume { .. } => None,
        }
    }

    /// A flat entry's light, premultiplied by its alpha as sky_layers_on composites it (`lit x (alpha x gain)`): today's
    /// shade with `sun` and the greyed `sky` (cloud_light.rs's EntryLight), the self-shadow times `t_above`, and
    /// Brightness for the older cloud_gain. With t_above 1 the arithmetic is today's.
    pub fn flat_radiance(&self, s: &CloudSample, sun: Vec3f, sky: Vec3f, t_above: f32, brightness: f32) -> Vec3f {
        let layer = match self {
            ClassicEntry::Deck(layer) | ClassicEntry::Cirrus(layer) => layer,
            ClassicEntry::Volume { footprint, .. } => footprint,
        };
        let shaded = CloudSample { light: s.light * t_above, ..*s };
        layer.shade_lit(&shaded, sun, sky) * (s.alpha * brightness)
    }

    /// What it hides of whatever is behind it along `d` (design §2.4): a deck's and a cirrus' unfaded opacity; the
    /// volume's marched faded alpha, as today (volumetric.rs `transmittance_toward`).
    pub fn cover_toward(&self, d: Vec3f) -> f32 {
        match self {
            ClassicEntry::Deck(layer) | ClassicEntry::Cirrus(layer) => layer.cover_toward(d),
            ClassicEntry::Volume { volume, .. } => 1.0 - volume.transmittance_toward(d),
        }
    }

    /// What the light pass reads, and only it (every other lookup marches the volume, as today): a flat part's cover;
    /// the volume's footprint along the look's slanted path through its shell, never a march
    /// (`VolumetricClouds::footprint_cover_toward`).
    pub fn stand_in(&self, d: Vec3f) -> f32 {
        match self {
            ClassicEntry::Deck(layer) | ClassicEntry::Cirrus(layer) => layer.cover_toward(d),
            ClassicEntry::Volume { volume, .. } => volume.footprint_cover_toward(d),
        }
    }

    /// The shade it casts at a ground point of its plane (T_above, design §2.3): a flat part's opacity there from
    /// `octaves` of its noise; nothing from the volume, since marched entries do not cast in phase 1.
    pub fn plane_cover(&self, xz_m: Vec2f, octaves: u32) -> f32 {
        match self {
            ClassicEntry::Deck(layer) | ClassicEntry::Cirrus(layer) => layer.plane_cover(xz_m, octaves),
            ClassicEntry::Volume { .. } => 0.0,
        }
    }

    /// The marched volume, when this entry is it.
    pub fn volume(&self) -> Option<&VolumetricClouds> {
        match self {
            ClassicEntry::Volume { volume, .. } => Some(volume),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caster::serial_rows;
    use crate::{dir_from_az_el, legacy_cloud_layers, CloudParams, CloudStackParams, SkyParams, VolumetricParams};

    fn sun() -> Vec3f {
        dir_from_az_el(200.0, 30.0)
    }

    fn dirs() -> Vec<Vec3f> {
        let mut out = Vec::new();
        for i in 0..12 {
            for el in [1.0f32, 3.0, 8.0, 20.0, 45.0, 80.0] {
                out.push(dir_from_az_el(i as f32 * 30.0 + 7.0, el));
            }
        }
        out.push(vec3f(0.0, 1.0, 0.0));
        out
    }

    /// The lifted layer `i` of `stack`, as the stack's build hands it over (presence: the stack's Cover, the eye on).
    fn entry(stack: &CloudStackParams, i: usize, map_seed: u32, marches: bool) -> ClassicEntry {
        let layer = &stack.layers[i];
        let genus = layer.genus().expect("a lifted layer's genus");
        let look = layer.classic.as_ref().expect("a lifted layer is classic");
        let spec = ClassicSpec { layer, genus, look, map_seed, presence: stack.cover, marches, march_steps: stack.march_steps, stack_wind_deg: stack.wind_deg };
        ClassicEntry::build(&spec, sun(), &|| false, &serial_rows).expect("nothing cancels").expect("a lifted layer draws")
    }

    /// A lifted deck alone, a cirrus alone and a volume alone, drawn from their parts (design §1.2), are the older
    /// constructors' layers within 1e-6: the deck's and the veil's visible alpha, self-shadow and unfaded cover along 73
    /// directions, at the defaults and at Golden hour's shape (scale 1.2, altitude 3000); the volume's march, all four
    /// of its numbers.
    #[test]
    fn a_classic_part_draws_as_its_older_layer() {
        let near = |a: f32, b: f32| (a - b).abs() <= 1.0e-6 * a.abs().max(b.abs()).max(1.0);
        for (coverage, cirrus, sharpness, scale, altitude_m) in [(0.5f32, 0.0f32, 0.5f32, 1.0f32, 2000.0f32), (0.15, 0.0, 0.6, 1.2, 3000.0), (0.0, 0.4, 0.5, 1.0, 2000.0), (0.0, 0.35, 0.6, 1.2, 3000.0)] {
            let sky = SkyParams { clouds: CloudParams { coverage, sharpness, scale, altitude_m, cirrus }, ..SkyParams::default() };
            let older = CloudLayer::new(&sky.clouds, 7, sun()).expect("an older layer");
            let stack = legacy_cloud_layers(&sky);
            assert_eq!(stack.layers.len(), 1, "{:?}", stack.layers);
            let part = entry(&stack, 0, 7, false);
            for d in dirs() {
                let (s, cover, _, _) = part.flat_sample(d).expect("a flat part");
                let want = older.sample(d);
                assert!(near(s.alpha, want.alpha) && near(s.light, want.light) && near(s.cos_sun, want.cos_sun), "{coverage} {cirrus} at {d:?}: {s:?} against {want:?}");
                assert!(near(cover, older.cover_toward(d)), "{coverage} {cirrus} at {d:?}: cover {cover} against {}", older.cover_toward(d));
                assert!(near(part.cover_toward(d), older.cover_toward(d)) && near(part.stand_in(d), older.cover_toward(d)));
            }
        }
        let mut sky = SkyParams::default();
        sky.clouds.coverage = 0.5;
        let vol = VolumetricParams { steps: 24, ..VolumetricParams::default() };
        sky.volumetric = Some(vol.clone());
        let older = VolumetricClouds::new(&sky.clouds, &vol, 7, sun()).expect("an older volume");
        let stack = legacy_cloud_layers(&sky);
        let i = stack.layers.iter().position(|l| l.genus == "cumulus").expect("the lifted volume");
        let part = entry(&stack, i, 7, true);
        let volume = part.volume().expect("a marched part");
        for d in dirs() {
            let (a, b) = (volume.march(d), older.march(d));
            assert!(near(a.alpha, b.alpha) && near(a.sun, b.sun) && near(a.ambient, b.ambient) && near(a.bounce, b.bounce), "{d:?}: {a:?} against {b:?}");
            assert!(near(part.cover_toward(d), 1.0 - older.transmittance_toward(d)));
        }
        assert!(part.flat_sample(vec3f(0.0, 1.0, 0.0)).is_none(), "the volume is marched, not flat");
        assert_eq!(part.plane_cover(vec2f(0.0, 0.0), 3), 0.0, "a marched entry casts nothing in phase 1");
        // A lifted layer's Volume row that does not hold the march draws its deck; density 0 builds nothing (D8).
        assert!(matches!(entry(&stack, i, 7, false), ClassicEntry::Deck(_)));
        let mut thin = stack.clone();
        thin.layers[i].cumulus.as_mut().expect("the lifted block").density = 0.0;
        let layer = &thin.layers[i];
        let spec = ClassicSpec { layer, genus: CloudGenus::Cumulus, look: layer.classic.as_ref().unwrap(), map_seed: 7, presence: 1.0, marches: true, march_steps: 24, stack_wind_deg: None };
        assert!(ClassicEntry::build(&spec, sun(), &|| false, &serial_rows).unwrap().is_none(), "density 0 draws nothing");
    }
}

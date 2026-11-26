//! Older content as pure params code (design §1.2, §1.3 and §5.8):
//! - the layers a map without a stack draws through today's code (`legacy_cloud_layers`);
//! - the lift that writes them, once, at a cloud edit (`HdriParams::lift_clouds`);
//! - the view the app lists either way (`cloud_layers_view`);
//! - the classic layers' share tables and their one rule (`CLASSIC_*_SHARE`, `CloudLayerParams::shown_cover` and
//!   `edited`);
//! - the layer a weather pick adds, and the previous kind's untouched one it takes back (`add_weather_clouds`);
//! - the one layer phase 1 marches (`marched_cloud_layer`), the rule the engine, the app and the assistant read.
//!
//! Nothing here draws. The app calls it on the UI thread, on a click or a refresh, and each call costs a few block
//! copies.

use crate::weather::{kind_cloud_gain, weathered_sky};
use crate::{
    cloud_display_name, CirrusParams, ClassicLook, CloudGenus, CloudLayerParams, CloudParams, CloudStackParams, CumulusParams, HdriParams, NimbostratusParams,
    SkyParams, StratocumulusParams, StratusParams, VolumetricParams, WeatherKind, WeatherParams, CLASSIC_KEYS,
};

/// [k]: the share of sky the older deck's remap (clouds.rs `CloudLayer::cover`, X2 included) covers at its 0.5
/// contour at coverage k/64. It is measured as the share of a 256 x 256-cell plane where the 6-octave deck noise, at
/// sharpness 0.5, makes a density of at least 0.5. Below coverage 0.85 the contour does not move with sharpness: it is
/// the middle of every band. Every type but Cirrus reads it: the deck, and the volume, which stands on the deck's
/// footprint. `the_classic_share_tables_are_measured` recomputes it within 0.01, and prints it as measured when it is
/// off.
pub const CLASSIC_DECK_SHARE: [f32; 65] = [
    0.0, 0.0, 0.0, 0.0, 3.0517578e-5, 0.007019043, 0.03678894, 0.048706055,
    0.05659485, 0.06538391, 0.07537842, 0.086502075, 0.09844971, 0.11151123, 0.12553406, 0.1404419,
    0.15637207, 0.17288208, 0.19102478, 0.20921326, 0.22904968, 0.24873352, 0.26968384, 0.29086304,
    0.31318665, 0.33561707, 0.35876465, 0.38290405, 0.40634155, 0.4296112, 0.45309448, 0.4763031,
    0.5006256, 0.52531433, 0.5489807, 0.57321167, 0.5962677, 0.61912537, 0.6426544, 0.66567993,
    0.6876068, 0.70918274, 0.73005676, 0.750885, 0.7701416, 0.78915405, 0.8070679, 0.82444763,
    0.84129333, 0.8570709, 0.8718872, 0.88645935, 0.8996277, 0.91200256, 0.9239044, 0.9344635,
    0.9485016, 0.9646759, 0.98194885, 0.996933, 1.0, 1.0, 1.0, 1.0,
    1.0,
];

/// [k]: the same for the older cirrus at amount k/64: where its band is at least half on (clouds.rs `cirrus_density`
/// before the amount's opacity 0.4 + 0.6 amount; the warp included). `cirrus_density` itself never reaches 0.5 below
/// an amount of 1/6. A cirrus at amount 1 covers 0.37 of the sky in this sense; Clear noon's 0.15 covers 0.026.
pub const CLASSIC_CIRRUS_SHARE: [f32; 65] = [
    0.0, 0.0, 0.0, 0.0, 0.0, 0.0025482178, 0.015975952, 0.020248413,
    0.02204895, 0.024276733, 0.026535034, 0.028961182, 0.031570435, 0.033935547, 0.03643799, 0.03944397,
    0.042312622, 0.04573059, 0.049301147, 0.052825928, 0.056274414, 0.060272217, 0.06460571, 0.06889343,
    0.07273865, 0.077423096, 0.08213806, 0.08677673, 0.09233093, 0.097976685, 0.10307312, 0.10873413,
    0.11427307, 0.1199646, 0.12608337, 0.13246155, 0.13865662, 0.14477539, 0.15144348, 0.1579895,
    0.16525269, 0.17208862, 0.17919922, 0.18634033, 0.19424438, 0.20217896, 0.2098999, 0.21766663,
    0.22616577, 0.23484802, 0.24336243, 0.25187683, 0.26036072, 0.26860046, 0.27722168, 0.28604126,
    0.29548645, 0.3048401, 0.31414795, 0.3228302, 0.33244324, 0.34207153, 0.3514862, 0.36158752,
    0.37117004,
];

/// The table a classic layer of `genus` reads: the cirrus' for a Cirrus, the deck's for every other type.
fn classic_table(genus: CloudGenus) -> &'static [f32; 65] {
    if genus == CloudGenus::Cirrus {
        &CLASSIC_CIRRUS_SHARE
    } else {
        &CLASSIC_DECK_SHARE
    }
}

/// The share of sky a classic layer's older remap hides at `amount`, its 0.5 contour (design §1.2), straight between
/// the table's entries. 0 for an amount that is not a number or not over 0.
pub fn classic_share(genus: CloudGenus, amount: f32) -> f32 {
    let table = classic_table(genus);
    if !(amount > 0.0) {
        return table[0];
    }
    let x = amount.min(1.0) * 64.0;
    let i = (x as usize).min(63);
    let t = x - i as f32;
    table[i] + (table[i + 1] - table[i]) * t
}

/// The smallest amount whose share is `share` (classic_share's monotone inverse), straight between entries. It is 0
/// at no share (or a NaN). It is 1 at the table's top or above, so a classic deck dragged to Cover 1 closes as
/// coverage 1 does, and a cirrus asked for more than it can hide stops at its full amount.
pub fn classic_amount_for_share(genus: CloudGenus, share: f32) -> f32 {
    let table = classic_table(genus);
    if !(share > table[0]) {
        return 0.0;
    }
    if share >= table[64] {
        return 1.0;
    }
    let k = (1..=64).find(|&k| table[k] >= share).unwrap_or(64);
    let (lo, hi) = (table[k - 1], table[k]);
    let t = if hi > lo { (share - lo) / (hi - lo) } else { 0.0 };
    ((k - 1) as f32 + t) / 64.0
}

/// The older clouds as layers (design §1.2), from the clamped sky with the kind's floors folded in exactly as
/// weathered_sky folds them for the map. Pure: the app shows them, the lift writes them and preset_index compares
/// with them.
/// - The layers are the deck or the volume, then the cirrus; a sky that draws no cloud gives none.
/// - Every layer is classic, with seed 0, "<Label> 1", the share its amount hides as its Cover, and the kind's
///   cloud_gain as its Brightness when that is not 1: the cirrus' and the volume's too, as the gain darkens them.
/// - A deck whose kind's floor raised it is a Nimbostratus (a Stratus under fog), else a Stratocumulus. Its tops are
///   the type's own thickness over the base (the blocks' NaN rule). A volume is a Cumulus with Volume on.
/// - The stack has Cover 1, no wind of its own, and the volume's steps (or 64), already clamped.
pub fn legacy_cloud_layers(sky: &SkyParams) -> CloudStackParams {
    let held = SkyParams {
        clouds: sky.clouds.clamped(),
        volumetric: sky.volumetric.as_ref().map(VolumetricParams::clamped),
        weather: sky.weather.as_ref().map(WeatherParams::clamped),
        cloud_stack: None,
        ..sky.clone()
    };
    let folded = weathered_sky(&held);
    let clouds = &folded.clouds;
    let kind = held.weather.as_ref().map(WeatherParams::kind);
    let gain = kind.map_or(1.0, kind_cloud_gain);
    let lifted = |genus: CloudGenus, amount: f32, light_m: f32| CloudLayerParams {
        name: format!("{} 1", genus.label()),
        genus: genus.as_str().to_string(),
        cover: Some(classic_share(genus, amount)),
        brightness: if gain != 1.0 { Some(gain) } else { None },
        classic: Some(ClassicLook { coverage: amount, sharpness: clouds.sharpness, light_m }),
        ..CloudLayerParams::default()
    };
    let mut layers = Vec::new();
    if clouds.coverage > 0.0 {
        let size_km = 3.0 * clouds.scale;
        match &held.volumetric {
            Some(volume) => layers.push(CloudLayerParams {
                cumulus: Some(CumulusParams {
                    base_m: volume.base_m,
                    tops_m: volume.top_m,
                    size_km,
                    puffiness: volume.erosion,
                    flat_base: 0.0,
                    towers: 0.0,
                    clustering: 0.0,
                    streets: 0.0,
                    lean: 0.0,
                    softness: 1.0 - clouds.sharpness,
                    volume: true,
                    density: volume.density,
                }),
                ..lifted(CloudGenus::Cumulus, clouds.coverage, 0.5 * (volume.base_m + volume.top_m))
            }),
            None => {
                // The kind's floor raised it (coverage_min > coverage): a closed deck of bad weather.
                let raised = clouds.coverage > held.clouds.coverage;
                let genus = match kind {
                    Some(WeatherKind::Fog) if raised => CloudGenus::Stratus,
                    Some(_) if raised => CloudGenus::Nimbostratus,
                    _ => CloudGenus::Stratocumulus,
                };
                let base_m = clouds.altitude_m;
                let mut layer = lifted(genus, clouds.coverage, base_m);
                match genus {
                    CloudGenus::Stratus => layer.stratus = Some(StratusParams { base_m, size_km, ..StratusParams::default() }),
                    CloudGenus::Nimbostratus => layer.nimbostratus = Some(NimbostratusParams { base_m, tops_m: f32::NAN, size_km, ..NimbostratusParams::default() }),
                    _ => {
                        layer.stratocumulus = Some(StratocumulusParams {
                            base_m,
                            tops_m: f32::NAN,
                            size_km,
                            softness: 1.0 - clouds.sharpness,
                            shading: 0.5,
                            billows: 0.0,
                            rolls: 0.0,
                            volume: false,
                            ..StratocumulusParams::default()
                        })
                    }
                }
                layers.push(layer);
            }
        }
    }
    if clouds.cirrus > 0.0 {
        layers.push(CloudLayerParams {
            // The older cirrus' own altitude and cells (`CloudLayer::new`): 6000 m over the deck, at least 8000 m,
            // in cells twice the deck's.
            cirrus: Some(CirrusParams {
                base_m: (clouds.altitude_m + 6000.0).max(8000.0),
                size_km: 6.0 * clouds.scale,
                streak_length: 6.7,
                fineness: 0.0,
                hooks: 0.0,
                fall_streaks: 0.0,
                patchiness: 0.0,
                opacity: 0.55,
                contrails: None,
            }),
            ..lifted(CloudGenus::Cirrus, clouds.cirrus, clouds.altitude_m)
        });
    }
    let stack = CloudStackParams { cover: 1.0, wind_deg: None, march_steps: held.volumetric.as_ref().map_or(64, |volume| volume.steps), layers };
    // A NaN tops lands the type's own thickness over the base; every other field is in range already.
    stack.clamped()
}

impl HdriParams {
    /// The first cloud edit's lift (design §1.3). A map without a stack gets its older clouds as layers
    /// (`legacy_cloud_layers`) and true. Its older deck is reset to its defaults, which an older build still reads,
    /// and its volume goes. A map with a stack is left untouched and gives false. Only a cloud edit calls it: never
    /// opening, a preset, Load, undo or a refresh, and no host.
    pub fn lift_clouds(&mut self) -> bool {
        if self.sky.cloud_stack.is_some() {
            return false;
        }
        self.sky.cloud_stack = Some(legacy_cloud_layers(&self.sky));
        self.sky.clouds = CloudParams::default();
        self.sky.volumetric = None;
        true
    }

    /// The stack a cloud edit writes to, lifting a map without one first.
    pub fn clouds_mut(&mut self) -> &mut CloudStackParams {
        self.lift_clouds();
        self.sky.cloud_stack.get_or_insert_with(CloudStackParams::default)
    }
}

/// The layers the app lists: the stack, or a map without one's `legacy_cloud_layers`. It never writes;
/// Selection::Cloud(i) indexes `.layers`, the same index before and after a lift.
pub fn cloud_layers_view(p: &HdriParams) -> CloudStackParams {
    match &p.sky.cloud_stack {
        Some(stack) => stack.clone(),
        None => legacy_cloud_layers(&p.sky),
    }
}

impl CloudLayerParams {
    /// The Cover the rows, the inspector and the summary show. On a classic layer it is the share its older remap
    /// hides at its exact amount (`classic_share`). Otherwise it is its own Cover or its type's default. A layer of a
    /// genus this build does not know shows its stored Cover, or 0.
    pub fn shown_cover(&self) -> f32 {
        match (self.genus(), self.classic.as_ref()) {
            (Some(genus), Some(classic)) => classic_share(genus, classic.coverage),
            (Some(genus), None) => self.cover.unwrap_or(genus.default_cover()),
            (None, _) => self.cover.unwrap_or(0.0),
        }
    }

    /// The classic rule (design §1.2), called after any field write by the inspector, set_clouds and the rows, with
    /// the field's name ("cover", "softness", a block's own field, "type"). On a classic layer:
    /// - A key in CLASSIC_KEYS keeps it classic, but "density" only on a Stratocumulus, Nimbostratus or Cumulus, where
    ///   it is the Volume's. On any other type it is the type's own row, which the classic drawing does not read.
    /// - "cover" moves its exact amount to the smallest one that hides the share asked
    ///   (`classic_amount_for_share`), and keeps `cover` the share the layer now shows.
    /// - "softness" moves its sharpness to 1 - softness.
    /// - Any other key drops `classic`. From then on the layer draws as a modern layer of its type, at the Cover it
    ///   showed.
    ///
    /// A modern layer, or one of a genus this build does not know, is left as it is.
    pub fn edited(&mut self, key: &str) {
        let Some(genus) = self.genus() else {
            return;
        };
        if self.classic.is_none() {
            return;
        }
        let volume_density = matches!(genus, CloudGenus::Stratocumulus | CloudGenus::Nimbostratus | CloudGenus::Cumulus);
        if !CLASSIC_KEYS.contains(&key) || (key == "density" && !volume_density) {
            self.cover = Some(self.shown_cover());
            self.classic = None;
            return;
        }
        let softness = match genus {
            CloudGenus::Stratocumulus => Some(self.stratocumulus_params().softness),
            CloudGenus::Cumulus => Some(self.cumulus_params().softness),
            _ => None,
        };
        let asked = self.cover.unwrap_or(genus.default_cover());
        let Some(classic) = self.classic.as_mut() else {
            return;
        };
        match key {
            "cover" => {
                classic.coverage = classic_amount_for_share(genus, asked);
                self.cover = Some(classic_share(genus, classic.coverage));
            }
            "softness" => {
                if let Some(softness) = softness {
                    classic.sharpness = 1.0 - softness;
                }
            }
            _ => {}
        }
    }
}

/// What a weather pick did (design §5.8), by the names the rows show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WeatherAdds {
    /// The layer the pick added.
    pub added: Vec<String>,
    /// The kind's type is listed but none of its layers shows: those layers, so the status line can say why nothing
    /// came.
    pub hidden: Vec<String>,
    /// The previous kind's own layer, taken back because nobody had changed it since that kind added it.
    pub taken_back: Vec<String>,
    /// The kind whose layer was taken back; None when nothing was.
    pub previous: Option<WeatherKind>,
}

/// The type a kind brings in phases 1-3 (design §5.8): a Nimbostratus under Overcast, Rain, Storm and Snow, a Stratus
/// under Fog, nothing under Clear and Hazy.
fn kind_genus(kind: WeatherKind) -> Option<CloudGenus> {
    match kind {
        WeatherKind::Overcast | WeatherKind::Rain | WeatherKind::Snow => Some(CloudGenus::Nimbostratus),
        // Phase 4 (design §5.8): a storm cloud stands over its bolt.
        WeatherKind::Storm => Some(CloudGenus::Cumulonimbus),
        WeatherKind::Fog => Some(CloudGenus::Stratus),
        WeatherKind::Clear | WeatherKind::Hazy => None,
    }
}

/// A kind's name as the status line writes it ("Rain").
fn kind_label(kind: WeatherKind) -> &'static str {
    match kind {
        WeatherKind::Clear => "Clear",
        WeatherKind::Hazy => "Hazy",
        WeatherKind::Overcast => "Overcast",
        WeatherKind::Fog => "Fog",
        WeatherKind::Rain => "Rain",
        WeatherKind::Storm => "Storm",
        WeatherKind::Snow => "Snow",
    }
}

/// The layer `kind` adds over `layers` (design §5.8): `CloudLayerParams::new` with the kind's Cover, its gain as
/// Brightness when that is not 1, and its own rows:
/// - Overcast: Darkness 0.3;
/// - Rain: Rain curtains 0.6;
/// - Storm: a Cumulonimbus at `storm_azimuth_deg` (`storm_azimuth_deg`'s rule) at its default distance;
/// - Fog: a Stratus based at 300 m.
///
/// None for a kind that adds nothing.
fn kind_layer(kind: WeatherKind, layers: &[CloudLayerParams], map_seed: u32, storm_azimuth_deg: f32) -> Option<CloudLayerParams> {
    let genus = kind_genus(kind)?;
    let mut layer = CloudLayerParams::new(genus, layers, map_seed);
    let gain = kind_cloud_gain(kind);
    if gain != 1.0 {
        layer.brightness = Some(gain);
    }
    match kind {
        WeatherKind::Overcast => {
            layer.cover = Some(0.95);
            if let Some(block) = layer.nimbostratus.as_mut() {
                block.darkness = 0.3;
            }
        }
        WeatherKind::Rain => {
            layer.cover = Some(1.0);
            if let Some(block) = layer.nimbostratus.as_mut() {
                block.rain_curtains = 0.6;
            }
        }
        WeatherKind::Storm => {
            layer.cover = Some(1.0);
            if let Some(block) = layer.cumulonimbus.as_mut() {
                block.azimuth_deg = storm_azimuth_deg;
            }
        }
        WeatherKind::Snow => layer.cover = Some(0.95),
        WeatherKind::Fog => {
            layer.cover = Some(0.9);
            if let Some(block) = layer.stratus.as_mut() {
                block.base_m = 300.0;
            }
        }
        WeatherKind::Clear | WeatherKind::Hazy => {}
    }
    Some(layer)
}

/// Where `kind`'s own layer is while nobody has changed it: the last layer equal, field for field (name, seed and eye
/// included), to what `kind` adds over the layers before it (`kind_layer`). None for a kind that adds nothing, or when
/// no such layer is listed.
fn untouched_kind_layer(layers: &[CloudLayerParams], kind: WeatherKind, map_seed: u32, storm_azimuth_deg: f32) -> Option<usize> {
    let genus = kind_genus(kind)?;
    (0..layers.len()).rev().find(|&i| layers[i].genus == genus.as_str() && kind_layer(kind, &layers[..i], map_seed, storm_azimuth_deg).is_some_and(|added| added == layers[i]))
}

/// Where Storm's Cumulonimbus stands (design §5.8): over the first lightning layer's bolt when the map has one, else
/// opposite the sun, so its tower is sunlit (`cumulonimbus_azimuth_deg`).
pub fn storm_azimuth_deg(p: &HdriParams) -> f32 {
    match p.lights.iter().find(|light| light.is_lightning()) {
        Some(bolt) => bolt.azimuth_deg,
        None => cumulonimbus_azimuth_deg(p),
    }
}

/// Where a new Cumulonimbus stands (design §4.3): opposite the sun, so its tower is sunlit; north for a sun overhead.
pub fn cumulonimbus_azimuth_deg(p: &HdriParams) -> f32 {
    let sun = crate::atmosphere::sun_direction(&p.sky.sun);
    if !((sun.x * sun.x + sun.z * sun.z).sqrt() >= 1.0e-4) {
        return 0.0;
    }
    let (azimuth, _) = crate::az_el_from_dir(sun);
    (azimuth + 180.0).rem_euclid(360.0)
}

/// A kind pick (design §5.8). `p` is the map as drawn before the pick, so its kind is the previous one; the caller
/// writes the new kind after.
/// - First the previous kind's own layer is taken back while nobody has changed it (`untouched_kind_layer`). The
///   dropdown picks every kind its arrow keys pass, and each would otherwise leave its layer behind. A layer changed in
///   any way (a row, the eye, the name, the Type) stays, as does one the person made, unless it is exactly a kind's.
/// - Then, when `cloud_layers_view(p)` lists no layer of the kind's type, one is added (`kind_layer`), lifting a map
///   without a stack first. A listed layer of the type counts even when hidden; when none of them shows they are
///   named in `hidden`.
///
/// A map without a stack holds no layer a pick added, so nothing is taken back from it, and it lifts only when a
/// layer is added. Nothing else is ever removed.
pub fn add_weather_clouds(p: &mut HdriParams, kind: WeatherKind) -> WeatherAdds {
    let mut adds = WeatherAdds::default();
    let map_seed = p.seed;
    let storm_azimuth = storm_azimuth_deg(p);
    let previous = p.sky.weather.as_ref().map_or(WeatherKind::Clear, WeatherParams::kind);
    if previous != kind {
        if let Some(stack) = p.sky.cloud_stack.as_mut() {
            if let Some(i) = untouched_kind_layer(&stack.layers, previous, map_seed, storm_azimuth) {
                adds.taken_back.push(cloud_display_name(&stack.layers, i));
                adds.previous = Some(previous);
                stack.layers.remove(i);
            }
        }
    }
    let Some(genus) = kind_genus(kind) else {
        return adds;
    };
    let view = cloud_layers_view(p);
    let of_type: Vec<usize> = (0..view.layers.len()).filter(|&i| view.layers[i].genus() == Some(genus)).collect();
    if !of_type.is_empty() {
        if !of_type.iter().any(|&i| view.layers[i].enabled) {
            adds.hidden = of_type.iter().map(|&i| cloud_display_name(&view.layers, i)).collect();
        }
        return adds;
    }
    let stack = p.clouds_mut();
    if let Some(layer) = kind_layer(kind, &stack.layers, map_seed, storm_azimuth) {
        adds.added.push(layer.name.clone());
        stack.layers.push(layer);
    }
    adds
}

/// What the status line and the assistant's reply say of a pick: "Rain added Nimbostratus 1", "Rain took back
/// Overcast's Nimbostratus 1 and added Nimbostratus 1", "Clear took back Fog's Stratus 1", "Rain added nothing:
/// Nimbostratus 1 is hidden", or None when there is nothing to say.
pub fn weather_adds_text(kind: WeatherKind, adds: &WeatherAdds) -> Option<String> {
    let took = (!adds.taken_back.is_empty()).then(|| {
        let whose = adds.previous.map_or(String::new(), |previous| format!("{}'s ", kind_label(previous)));
        format!("took back {whose}{}", adds.taken_back.join(", "))
    });
    let did = if !adds.added.is_empty() {
        Some(format!("added {}", adds.added.join(", ")))
    } else if !adds.hidden.is_empty() {
        let verb = if adds.hidden.len() == 1 { "is" } else { "are" };
        Some(format!("added nothing: {} {verb} hidden", adds.hidden.join(", ")))
    } else {
        None
    };
    let label = kind_label(kind);
    match (took, did) {
        (Some(took), Some(did)) => Some(format!("{label} {took} and {did}")),
        (Some(said), None) | (None, Some(said)) => Some(format!("{label} {said}")),
        (None, None) => None,
    }
}

/// The one layer phase 1 marches: the first in list order that is shown, lifted (`classic`), with an amount over 0
/// (the Clouds Cover times `classic.coverage`), of a type whose Volume row is on (a Stratocumulus, Nimbostratus or
/// Cumulus block's `volume`). Every other layer with its Volume on draws flat, and a modern layer always does in
/// phase 1: joint marching comes in a later phase. The engine's build (cloud_stack.rs), the app's Quality fold and
/// "Drawn flat" note (cloud_rows.rs) and set_clouds' summary (ai_clouds.rs) all read this one rule.
pub fn marched_cloud_layer(stack: &CloudStackParams) -> Option<usize> {
    stack.layers.iter().position(|layer| {
        layer.enabled
            && layer.classic.as_ref().is_some_and(|look| stack.cover * look.coverage > 0.0)
            && match layer.genus() {
                Some(CloudGenus::Stratocumulus) => layer.stratocumulus_params().volume,
                Some(CloudGenus::Nimbostratus) => layer.nimbostratus_params().volume,
                Some(CloudGenus::Cumulus) => layer.cumulus_params().volume,
                _ => false,
            }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clouds::CloudLayer;
    use crate::noise::{fbm2, hash01, hash_u32};
    use crate::presets::{preset, PRESET_NAMES};
    use crate::{dir_from_az_el, fresh_cloud_seed, WEATHER_KIND_NAMES};
    use makepad_draw::*;

    /// The plane the classic shares are measured on: 256 x 256 cells (deck cells for the deck, cirrus cells for the
    /// veil), one point in each at a hashed spot, so a share counts 65,536 looks.
    const SHARE_SIDE: i32 = 256;
    /// The map seed the measured streams come from.
    const SHARE_SEED: u32 = 1;
    /// The seed of the points' spots, apart from the streams', so the spots do not follow the noise's own lattice.
    const SHARE_JITTER: u32 = 0x5ca1_ab1e;

    use crate::patterns::smooth as smoothstep;

    /// clouds.rs's `x2_rise`.
    fn x2_rise(amount: f32) -> f32 {
        0.5 * (1.0 - smoothstep(0.0, 0.1, amount))
    }

    /// The deck's remap (clouds.rs `CloudLayer::cover`, X2 included): checked bit for bit against `deck_density` below.
    fn deck_remap(coverage: f32, sharpness: f32, noise: f32) -> f32 {
        let threshold = 0.78 - 0.56 * coverage + x2_rise(coverage);
        let width = 0.02 + 0.2 * (1.0 - sharpness);
        let density = smoothstep(threshold - width, threshold + width, noise);
        let close = smoothstep(0.85, 1.0, coverage);
        density + (1.0 - density) * close
    }

    /// The cirrus band (clouds.rs `cirrus_density` before its amount's opacity, 0.4 + 0.6 amount), X2 included.
    fn cirrus_band(amount: f32, noise: f32) -> f32 {
        let threshold = 0.7 - 0.25 * amount + x2_rise(amount);
        smoothstep(threshold, threshold + 0.2, noise)
    }

    /// The cirrus' warped noise at `q` (cirrus cells) with every octave (clouds.rs `cirrus_density` at detail 1).
    fn cirrus_noise(q: (f32, f32), wind: (f32, f32), warp_seed: u32, cirrus_seed: u32) -> f32 {
        let along = q.0 * wind.0 + q.1 * wind.1;
        let across = q.1 * wind.0 - q.0 * wind.1;
        let s = (along * 0.3, across * 2.0);
        let wx = fbm2(s.0 * 0.5, s.1 * 0.5, 3, warp_seed) - 0.5;
        let wy = fbm2(s.0 * 0.5 + 5.2, s.1 * 0.5 + 1.3, 3, warp_seed) - 0.5;
        fbm2(s.0 + 1.5 * wx, s.1 + 1.5 * wy, 4, cirrus_seed)
    }

    /// The cirrus streams a map seed gives (clouds.rs `CloudLayer::new`): its wind (x, z), warp seed and cirrus seed.
    fn cirrus_streams(seed: u32) -> ((f32, f32), u32, u32) {
        let angle = hash01(0, 0, 0, hash_u32(seed ^ 0x5bd1_e995)) * std::f32::consts::TAU;
        ((angle.cos(), angle.sin()), hash_u32(seed.wrapping_add(0xb529_7a4d)), hash_u32(seed.wrapping_add(0x68e3_1da4)))
    }

    /// The two tables as measured now. At each amount k/64 they hold the share of the plane's points where the
    /// deck's density (6-octave fbm, sharpness 0.5) is at least 0.5, and where the cirrus band (its warp included,
    /// the wind along x) is.
    fn measured_shares() -> ([f32; 65], [f32; 65]) {
        let (_, warp_seed, cirrus_seed) = cirrus_streams(SHARE_SEED);
        let count = (SHARE_SIDE * SHARE_SIDE) as usize;
        let (mut deck_noise, mut veil_noise) = (Vec::with_capacity(count), Vec::with_capacity(count));
        for j in 0..SHARE_SIDE {
            for i in 0..SHARE_SIDE {
                let (x, y) = (i as f32 + hash01(i, j, 0, SHARE_JITTER), j as f32 + hash01(i, j, 1, SHARE_JITTER));
                deck_noise.push(fbm2(x, y, 6, SHARE_SEED));
                veil_noise.push(cirrus_noise((x, y), (1.0, 0.0), warp_seed, cirrus_seed));
            }
        }
        let share = |noise: &[f32], dense: &dyn Fn(f32) -> bool| noise.iter().filter(|&&n| dense(n)).count() as f32 / noise.len() as f32;
        let (mut deck, mut cirrus) = ([0.0f32; 65], [0.0f32; 65]);
        for k in 0..=64 {
            let amount = k as f32 / 64.0;
            deck[k] = share(&deck_noise, &|n| deck_remap(amount, 0.5, n) >= 0.5);
            cirrus[k] = share(&veil_noise, &|n| cirrus_band(amount, n) >= 0.5);
        }
        (deck, cirrus)
    }

    /// Design §1.2: the copies above are the deck's and the cirrus' own remaps, bit for bit; the tables are their
    /// measurement within 0.01, start at nothing, never fall and close the deck; classic_share reads them and
    /// classic_amount_for_share inverts it.
    #[test]
    fn the_classic_share_tables_are_measured() {
        for k in 0..=64 {
            let amount = k as f32 / 64.0;
            for sharpness in [0.0f32, 0.2, 0.5, 1.0] {
                let Some(deck) = CloudLayer::new(&CloudParams { coverage: amount, sharpness, ..CloudParams::default() }, SHARE_SEED, vec3f(0.0, 1.0, 0.0)) else {
                    assert_eq!(amount, 0.0, "a layer for every amount over 0");
                    continue;
                };
                for (x, y) in [(0.25f32, 0.5f32), (3.7, -1.2), (-20.5, 14.25), (101.0, 7.5)] {
                    let want = deck_remap(amount, sharpness, fbm2(x, y, 4, SHARE_SEED));
                    assert_eq!(deck.deck_density(vec2f(x, y)).to_bits(), want.to_bits(), "the deck at {amount}, sharpness {sharpness}");
                }
            }
            if let Some(veil) = CloudLayer::new(&CloudParams { coverage: 0.0, cirrus: amount, ..CloudParams::default() }, 7, vec3f(0.0, 1.0, 0.0)) {
                let (wind, warp_seed, cirrus_seed) = cirrus_streams(7);
                for (az, el) in [(0.0f32, 90.0f32), (30.0, 60.0), (200.0, 45.0), (310.0, 31.0)] {
                    let d = dir_from_az_el(az, el);
                    let d = d / d.length();
                    // The default deck sits at 2000 m in 3000 m cells, so the cirrus at 8000 m in cells of 6000 m;
                    // every look here is within 4 cells of the point overhead, where the cirrus has all its octaves.
                    let reach = 8000.0 / (d.y.max(0.02) * 6000.0);
                    let q = (d.x * reach, d.z * reach);
                    let density = cirrus_band(amount, cirrus_noise(q, wind, warp_seed, cirrus_seed)) * (0.4 + 0.6 * amount);
                    assert_eq!(veil.cover_toward(d).to_bits(), (density * 0.55).clamp(0.0, 1.0).to_bits(), "the cirrus at {amount} toward ({az}, {el})");
                }
            }
        }
        // Off by more than 0.01 anywhere: the failure prints both tables as measured now, as Rust, for when a remap
        // changes on purpose.
        let (deck, cirrus) = measured_shares();
        let off = (0..=64).filter(|&k| (deck[k] - CLASSIC_DECK_SHARE[k]).abs() > 0.01 || (cirrus[k] - CLASSIC_CIRRUS_SHARE[k]).abs() > 0.01).collect::<Vec<_>>();
        if !off.is_empty() {
            let mut tables = String::new();
            for (name, table) in [("CLASSIC_DECK_SHARE", deck), ("CLASSIC_CIRRUS_SHARE", cirrus)] {
                tables += &format!("pub const {name}: [f32; 65] = [\n");
                for row in table.chunks(8) {
                    tables += &format!("    {},\n", row.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>().join(", "));
                }
                tables += "];\n";
            }
            panic!("the measured shares leave the tables by more than 0.01 at amounts {off:?}; as measured now:\n{tables}");
        }
        for table in [&CLASSIC_DECK_SHARE, &CLASSIC_CIRRUS_SHARE] {
            assert_eq!(table[0], 0.0, "nothing at an amount of 0");
            assert!(table.windows(2).all(|w| w[0] <= w[1]), "a share never falls as the amount grows");
        }
        assert_eq!(CLASSIC_DECK_SHARE[64], 1.0, "a closed deck covers the whole sky");
        assert_eq!(classic_share(CloudGenus::Stratocumulus, 0.5), CLASSIC_DECK_SHARE[32]);
        assert_eq!(classic_share(CloudGenus::Cumulus, 1.0), 1.0);
        assert_eq!(classic_share(CloudGenus::Cirrus, 1.0), CLASSIC_CIRRUS_SHARE[64]);
        assert_eq!((classic_share(CloudGenus::Stratus, -1.0), classic_share(CloudGenus::Stratus, f32::NAN)), (0.0, 0.0));
        let mid = classic_share(CloudGenus::Nimbostratus, 0.5 / 64.0 + 20.0 / 64.0);
        assert!((mid - 0.5 * (CLASSIC_DECK_SHARE[20] + CLASSIC_DECK_SHARE[21])).abs() < 1.0e-6, "straight between entries");
        for genus in [CloudGenus::Cirrus, CloudGenus::Stratocumulus] {
            let top = classic_share(genus, 1.0);
            let mut last = 0.0f32;
            for i in 0..=200 {
                let share = i as f32 / 200.0 * top;
                let amount = classic_amount_for_share(genus, share);
                assert!((0.0..=1.0).contains(&amount) && amount >= last, "{genus:?}: the inverse is monotone");
                last = amount;
                if share > 0.0 && share < top {
                    assert!((classic_share(genus, amount) - share).abs() < 1.0e-5, "{genus:?}: share {share} -> amount {amount}");
                }
            }
            assert_eq!(classic_amount_for_share(genus, 0.0), 0.0);
            assert_eq!(classic_amount_for_share(genus, top), 1.0, "{genus:?}: the table's top is the full amount");
            assert_eq!(classic_amount_for_share(genus, 2.0), 1.0);
            assert_eq!(classic_amount_for_share(genus, f32::NAN), 0.0);
        }
    }

    /// The row the lift makes of an older deck, volume or cirrus: its type, the share it shows, its kind's Brightness
    /// and its classic marker; the caller sets the block.
    fn lifted_row(genus: CloudGenus, classic: ClassicLook, brightness: Option<f32>) -> CloudLayerParams {
        CloudLayerParams {
            name: format!("{} 1", genus.label()),
            genus: genus.as_str().to_string(),
            cover: Some(classic_share(genus, classic.coverage)),
            brightness,
            classic: Some(classic),
            ..CloudLayerParams::default()
        }
    }

    /// The Cirrus block the lift makes of an older cirrus over a deck at `altitude_m` and `scale`.
    fn cirrus_block(altitude_m: f32, scale: f32) -> Option<CirrusParams> {
        Some(CirrusParams { base_m: (altitude_m + 6000.0).max(8000.0), size_km: 6.0 * scale, streak_length: 6.7, fineness: 0.0, hooks: 0.0, fall_streaks: 0.0, patchiness: 0.0, opacity: 0.55, contrails: None })
    }

    /// Design §1.2: what a map without layers lists is exactly what its lift writes, for every preset and every clouds
    /// baseline case. A lifted map is already clamped. The lift table holds, case by case.
    #[test]
    fn the_derived_rows_are_the_lift() {
        let cases = crate::cloud_baseline::clouds_baseline_cases();
        // (the presets without layers: the cloud-type skies are made with one)
        let mut maps: Vec<(String, HdriParams)> = PRESET_NAMES.iter().map(|n| (n.to_string(), preset(n).unwrap())).filter(|(_, p)| p.sky.cloud_stack.is_none()).collect();
        maps.extend(cases.iter().map(|(name, p, _)| (name.clone(), p.clone())));
        for (name, p) in &maps {
            let view = cloud_layers_view(p);
            let mut lifted = p.clone();
            assert!(lifted.lift_clouds(), "{name}");
            assert_eq!(lifted.sky.cloud_stack.as_ref(), Some(&view), "{name}: the rows are the lift");
            assert_eq!(cloud_layers_view(&lifted), view, "{name}: and stay the rows after it");
            assert_eq!(view.clamped(), view, "{name}: the lift is already clamped");
            assert!(lifted.clone().clamp().is_empty(), "{name}: a lifted map reports nothing under clamp");
            assert_eq!((view.cover, view.wind_deg), (1.0, None), "{name}");
            for (i, layer) in view.layers.iter().enumerate() {
                let genus = layer.genus().expect("the lift writes known types");
                assert_eq!((layer.seed, layer.name.clone(), layer.enabled), (0, format!("{} 1", genus.label()), true), "{name} layer {i}");
                let classic = layer.classic.as_ref().expect("every lifted layer is classic");
                assert_eq!(layer.cover, Some(classic_share(genus, classic.coverage)), "{name} layer {i}");
            }
        }
        let rows = |p: &HdriParams| cloud_layers_view(p).layers;
        let case = |name: &str| cases.iter().find(|(n, ..)| n == name).unwrap().1.clone();
        // Clear noon: its cirrus alone, which is why Coverage 0 still showed cloud.
        assert_eq!(rows(&preset("Clear noon").unwrap()), [CloudLayerParams { cirrus: cirrus_block(2000.0, 1.0), ..lifted_row(CloudGenus::Cirrus, ClassicLook { coverage: 0.15, sharpness: 0.5, light_m: 2000.0 }, None) }]);
        // Golden hour: a Stratocumulus deck (its tops the type's 500 m over the base, by the block's NaN rule) and the
        // cirrus.
        let golden = ClassicLook { coverage: 0.15, sharpness: 0.6, light_m: 3000.0 };
        assert_eq!(
            rows(&preset("Golden hour").unwrap()),
            [
                CloudLayerParams {
                    stratocumulus: Some(StratocumulusParams { base_m: 3000.0, tops_m: 3500.0, size_km: 3.0 * 1.2, softness: 1.0 - 0.6, shading: 0.5, billows: 0.0, rolls: 0.0, volume: false, density: 1.0 }),
                    ..lifted_row(CloudGenus::Stratocumulus, golden.clone(), None)
                },
                CloudLayerParams { cirrus: cirrus_block(3000.0, 1.2), ..lifted_row(CloudGenus::Cirrus, ClassicLook { coverage: 0.35, ..golden }, None) },
            ]
        );
        // Overcast closes its own deck with no weather: a Stratocumulus at coverage 1.
        assert_eq!(
            rows(&preset("Overcast").unwrap()),
            [CloudLayerParams {
                stratocumulus: Some(StratocumulusParams { base_m: 1500.0, tops_m: 2000.0, size_km: 3.0 * 1.5, softness: 1.0 - 0.1, shading: 0.5, billows: 0.0, rolls: 0.0, volume: false, density: 1.0 }),
                ..lifted_row(CloudGenus::Stratocumulus, ClassicLook { coverage: 1.0, sharpness: 0.1, light_m: 1500.0 }, None)
            }]
        );
        // A kind's floor that raised the deck makes it a Nimbostratus (a Stratus under fog), with the kind's gain as
        // every lifted layer's Brightness, the cirrus' too; a kind that did not raise it leaves a Stratocumulus.
        assert_eq!(
            rows(&case("kind_rain_c05")),
            [
                CloudLayerParams {
                    nimbostratus: Some(NimbostratusParams { base_m: 2000.0, tops_m: 6000.0, size_km: 3.0, ..NimbostratusParams::default() }),
                    ..lifted_row(CloudGenus::Nimbostratus, ClassicLook { coverage: 1.0, sharpness: 0.2, light_m: 2000.0 }, Some(0.7))
                },
                CloudLayerParams { cirrus: cirrus_block(2000.0, 1.0), ..lifted_row(CloudGenus::Cirrus, ClassicLook { coverage: 0.15, sharpness: 0.2, light_m: 2000.0 }, Some(0.7)) },
            ]
        );
        assert_eq!(
            rows(&case("kind_fog_c0")),
            [
                CloudLayerParams {
                    stratus: Some(StratusParams { base_m: 2000.0, size_km: 3.0, ..StratusParams::default() }),
                    ..lifted_row(CloudGenus::Stratus, ClassicLook { coverage: 0.9, sharpness: 0.2, light_m: 2000.0 }, None)
                },
                CloudLayerParams { cirrus: cirrus_block(2000.0, 1.0), ..lifted_row(CloudGenus::Cirrus, ClassicLook { coverage: 0.15, sharpness: 0.2, light_m: 2000.0 }, None) },
            ]
        );
        assert_eq!(rows(&case("kind_storm_c0"))[0].brightness, Some(0.45));
        assert_eq!(rows(&case("kind_snow_c0"))[0].genus, "nimbostratus");
        assert_eq!(rows(&case("kind_hazy_c05"))[0].genus, "stratocumulus", "Hazy has no cover floor");
        // A volume: a classic Cumulus with Volume on, lit at its middle; the stack takes its steps.
        assert_eq!(
            rows(&case("volume")),
            [CloudLayerParams {
                cumulus: Some(CumulusParams { base_m: 1500.0, tops_m: 4000.0, size_km: 3.0, puffiness: 0.5, flat_base: 0.0, towers: 0.0, clustering: 0.0, streets: 0.0, lean: 0.0, softness: 0.5, volume: true, density: 1.0 }),
                ..lifted_row(CloudGenus::Cumulus, ClassicLook { coverage: 0.5, sharpness: 0.5, light_m: 2750.0 }, None)
            }]
        );
        assert_eq!(rows(&case("volume_density0"))[0].cumulus.as_ref().unwrap().density, 0.0, "a volume of no density is listed and draws nothing (D8)");
        // The one layer phase 1 marches (marched_cloud_layer): the lifted volume, never a lifted deck or cirrus.
        assert_eq!(marched_cloud_layer(&cloud_layers_view(&case("volume_cirrus"))), Some(0), "the lifted volume marches");
        assert_eq!(marched_cloud_layer(&cloud_layers_view(&case("deck_cirrus"))), None, "a lifted deck and cirrus draw flat");
        let mut twice = cloud_layers_view(&case("volume"));
        twice.layers.push(twice.layers[0].clone());
        assert_eq!(marched_cloud_layer(&twice), Some(0), "the first in list order");
        twice.layers[0].enabled = false;
        assert_eq!(marched_cloud_layer(&twice), Some(1), "a hidden one gives the march to the next");
        twice.layers[0].enabled = true;
        twice.layers[0].classic.as_mut().unwrap().coverage = 0.0;
        assert_eq!(marched_cloud_layer(&twice), Some(1), "and so does one at amount 0");
        assert_eq!(marched_cloud_layer(&CloudStackParams { cover: 0.0, ..twice.clone() }), None, "nothing marches at Cover 0");
        let modern = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
        assert!(modern.cumulus_params().volume, "a modern Cumulus has its Volume on");
        assert_eq!(marched_cloud_layer(&CloudStackParams { layers: vec![modern], ..CloudStackParams::default() }), None, "and draws flat in phase 1");
        let mut steps = case("volume");
        steps.sky.volumetric.as_mut().unwrap().steps = 40;
        assert_eq!(cloud_layers_view(&steps).march_steps, 40);
        assert_eq!(cloud_layers_view(&case("deck")).march_steps, 64);
        // A sky that draws no cloud has no layers: the starry night and every studio.
        for name in ["Starry night", "Three-point", "Top softbox", "Rim pair", "Overcast dome", "Ring light", "Product table"] {
            assert!(rows(&preset(name).unwrap()).is_empty(), "{name}");
        }
    }

    /// Design §1.3: a map without layers lifts once, at a cloud edit, and its older deck and volume are reset; a map
    /// with layers never lifts again; the view writes nothing; clouds_mut lifts only a map without layers.
    #[test]
    fn a_lift_happens_once_and_clears_the_older_block() {
        let original = preset("Golden hour").unwrap();
        let mut p = original.clone();
        assert_eq!(cloud_layers_view(&p), legacy_cloud_layers(&p.sky));
        assert_eq!(p, original, "the view writes nothing");
        assert!(p.lift_clouds(), "a map without layers lifts");
        assert_eq!(p.sky.cloud_stack, Some(legacy_cloud_layers(&original.sky)));
        assert_eq!(p.sky.clouds, CloudParams::default(), "the older deck is reset, as older builds still read it");
        assert_eq!(p.sky.volumetric, None);
        let unlifted = HdriParams { sky: SkyParams { clouds: original.sky.clouds.clone(), volumetric: original.sky.volumetric.clone(), cloud_stack: None, ..p.sky.clone() }, ..p.clone() };
        assert_eq!(unlifted, original, "nothing else moved");
        let lifted = p.clone();
        assert!(!p.lift_clouds(), "a stack is never lifted again");
        assert_eq!(p, lifted);
        assert!(p.clone().clamp().is_empty());
        assert_eq!(HdriParams::from_json(&p.to_json()), Ok(p.clone()), "a lifted map round-trips");
        let mut q = original.clone();
        q.clouds_mut().cover = 0.5;
        assert_eq!(q.sky.cloud_stack.as_ref().unwrap().layers, lifted.sky.cloud_stack.as_ref().unwrap().layers, "clouds_mut lifted first");
        q.clouds_mut().cover = 0.25;
        assert_eq!(q.sky.cloud_stack.as_ref().unwrap().cover, 0.25, "and only once");
        let mut volume = crate::cloud_baseline::clouds_baseline_cases().into_iter().find(|(name, ..)| name == "volume").unwrap().1;
        assert!(volume.lift_clouds());
        assert_eq!((volume.sky.volumetric.clone(), volume.sky.cloud_stack.as_ref().unwrap().layers[0].genus.as_str()), (None, "cumulus"));
    }

    /// Design §5.8: each kind adds its type when the map lists none of it (lifting the older clouds first), never
    /// twice; a hidden one counts and is said; Clear and Hazy add nothing. A pick takes back the previous kind's
    /// untouched layer; nothing the person made or changed is removed.
    #[test]
    fn each_kind_adds_its_layer() {
        let noon = preset("Clear noon").unwrap();
        let older = legacy_cloud_layers(&noon.sky).layers;
        assert_eq!(older.len(), 1, "Clear noon lists its cirrus");
        let mut wants: Vec<(WeatherKind, CloudLayerParams)> = Vec::new();
        for (kind, genus, label) in [
            (WeatherKind::Overcast, CloudGenus::Nimbostratus, "Overcast"),
            (WeatherKind::Rain, CloudGenus::Nimbostratus, "Rain"),
            (WeatherKind::Storm, CloudGenus::Cumulonimbus, "Storm"),
            (WeatherKind::Snow, CloudGenus::Nimbostratus, "Snow"),
            (WeatherKind::Fog, CloudGenus::Stratus, "Fog"),
        ] {
            let mut p = noon.clone();
            let adds = add_weather_clouds(&mut p, kind);
            let stack = p.sky.cloud_stack.clone().expect("the pick lifted the map");
            assert_eq!(stack.layers[..1], older[..], "{label}: the older clouds come first, as they were");
            let mut want = CloudLayerParams::new(genus, &older, noon.seed);
            match kind {
                WeatherKind::Overcast => {
                    want.cover = Some(0.95);
                    want.nimbostratus.as_mut().unwrap().darkness = 0.3;
                }
                WeatherKind::Rain => {
                    want.cover = Some(1.0);
                    want.brightness = Some(0.7);
                    want.nimbostratus.as_mut().unwrap().rain_curtains = 0.6;
                }
                // Storm adds a Cumulonimbus (design §5.8), opposite the sun on a map without a bolt.
                WeatherKind::Storm => {
                    want.cover = Some(1.0);
                    want.brightness = Some(0.45);
                    want.cumulonimbus.as_mut().unwrap().azimuth_deg = storm_azimuth_deg(&noon);
                }
                WeatherKind::Snow => {
                    want.cover = Some(0.95);
                    want.brightness = Some(0.85);
                }
                _ => {
                    want.cover = Some(0.9);
                    want.stratus.as_mut().unwrap().base_m = 300.0;
                }
            }
            assert_eq!(stack.layers[1..], [want.clone()], "{label}");
            assert_eq!(want.name, format!("{} 1", genus.label()));
            assert_eq!(want.seed, fresh_cloud_seed(noon.seed, 1, genus));
            assert_eq!(adds, WeatherAdds { added: vec![want.name.clone()], hidden: Vec::new(), ..WeatherAdds::default() });
            assert_eq!(weather_adds_text(kind, &adds), Some(format!("{label} added {}", want.name)));
            assert_eq!(p.sky.clouds, CloudParams::default(), "{label}: the lift reset the older deck");
            // Picking the kind again adds nothing: one of its type is listed.
            let mut again = p.clone();
            let twice = add_weather_clouds(&mut again, kind);
            assert_eq!((again, twice.clone()), (p.clone(), WeatherAdds::default()), "{label}");
            assert_eq!(weather_adds_text(kind, &twice), None);
            // A hidden one counts, and the status line says so.
            let mut hidden = p.clone();
            hidden.clouds_mut().layers.last_mut().unwrap().enabled = false;
            let before = hidden.clone();
            let adds = add_weather_clouds(&mut hidden, kind);
            assert_eq!(hidden, before, "{label}: nothing is added over a hidden layer of its type");
            assert_eq!(adds, WeatherAdds { added: Vec::new(), hidden: vec![want.name.clone()], ..WeatherAdds::default() });
            assert_eq!(weather_adds_text(kind, &adds), Some(format!("{label} added nothing: {} is hidden", want.name)));
            wants.push((kind, want));
        }
        // Clear and Hazy add nothing and lift nothing.
        for kind in [WeatherKind::Clear, WeatherKind::Hazy] {
            let mut p = noon.clone();
            let adds = add_weather_clouds(&mut p, kind);
            assert_eq!((p, adds.clone()), (noon.clone(), WeatherAdds::default()));
            assert_eq!(weather_adds_text(kind, &adds), None);
        }
        // An older map whose kind closed its deck already lists a Nimbostratus: Rain adds nothing and lifts nothing.
        let mut overcast_kind = noon.clone();
        overcast_kind.sky.weather = Some(WeatherParams { kind: "overcast".to_string(), ..WeatherParams::default() });
        let before = overcast_kind.clone();
        assert_eq!(add_weather_clouds(&mut overcast_kind, WeatherKind::Rain), WeatherAdds::default());
        assert_eq!(overcast_kind, before, "a lift only when adding");
        // A pick whose kind is not written takes nothing back: Fog over a rained map that is still Clear keeps the
        // Nimbostratus and adds its Stratus.
        let mut rained = noon.clone();
        add_weather_clouds(&mut rained, WeatherKind::Rain);
        assert_eq!(add_weather_clouds(&mut rained, WeatherKind::Fog).added, ["Stratus 1"]);
        let genera: Vec<&str> = rained.sky.cloud_stack.as_ref().unwrap().layers.iter().map(|l| l.genus.as_str()).collect();
        assert_eq!(genera, ["cirrus", "nimbostratus", "stratus"]);
        // The kinds' cloud gains (weather.rs's presets).
        let gains: Vec<f32> = WEATHER_KIND_NAMES.iter().map(|name| kind_cloud_gain(WeatherParams { kind: name.to_string(), ..WeatherParams::default() }.kind())).collect();
        assert_eq!(gains, [1.0, 1.0, 1.0, 1.0, 0.7, 0.45, 0.85]);
        // The dropdown picks every kind its arrow keys pass. Arrowing from Clear to Snow, each kind written after its
        // pick as the app does, each pick takes back the previous kind's untouched layer: one layer of the kind's type
        // is listed after it, with that kind's rows, and the status line says what went.
        let pick = |p: &mut HdriParams, kind: WeatherKind| {
            let adds = add_weather_clouds(p, kind);
            p.sky.weather = Some(WeatherParams { kind: kind.as_str().to_string(), ..WeatherParams::default() });
            weather_adds_text(kind, &adds)
        };
        let mut arrowed = noon.clone();
        for (kind, text) in [
            (WeatherKind::Hazy, None),
            (WeatherKind::Overcast, Some("Overcast added Nimbostratus 1")),
            (WeatherKind::Fog, Some("Fog took back Overcast's Nimbostratus 1 and added Stratus 1")),
            (WeatherKind::Rain, Some("Rain took back Fog's Stratus 1 and added Nimbostratus 1")),
            (WeatherKind::Storm, Some("Storm took back Rain's Nimbostratus 1 and added Cumulonimbus 1")),
            (WeatherKind::Snow, Some("Snow took back Storm's Cumulonimbus 1 and added Nimbostratus 1")),
            (WeatherKind::Fog, Some("Fog took back Snow's Nimbostratus 1 and added Stratus 1")),
            (WeatherKind::Clear, Some("Clear took back Fog's Stratus 1")),
        ] {
            assert_eq!(pick(&mut arrowed, kind), text.map(str::to_string), "{kind:?}");
            let own: Vec<CloudLayerParams> = wants.iter().filter(|(k, _)| *k == kind).map(|(_, want)| want.clone()).collect();
            assert_eq!(cloud_layers_view(&arrowed).layers, [older.clone(), own].concat(), "{kind:?}: one layer of its type, with its rows");
        }
        // A layer changed in any way stays: Overcast's with Darkness 0.31, or with its eye off. Fog adds its Stratus
        // beside it.
        let changes: [fn(&mut CloudLayerParams); 2] = [|layer| layer.nimbostratus.as_mut().unwrap().darkness = 0.31, |layer| layer.enabled = false];
        for change in changes {
            let mut p = noon.clone();
            pick(&mut p, WeatherKind::Overcast);
            change(p.clouds_mut().layers.last_mut().unwrap());
            let kept = cloud_layers_view(&p).layers;
            assert_eq!(pick(&mut p, WeatherKind::Fog), Some("Fog added Stratus 1".to_string()));
            let layers = cloud_layers_view(&p).layers;
            assert_eq!((&layers[..2], layers[2].genus.as_str(), layers.len()), (&kept[..], "stratus", 3));
        }
    }

    /// Design §1.2: a classic layer keeps through the rows today's code draws ("cover" moving its exact amount,
    /// "softness" its sharpness; "density" only on a type with a Volume) and drops at any other, keeping the Cover it
    /// showed; a modern layer is left alone; the shown Cover is the table's on a classic layer, else the layer's own or
    /// its type's.
    #[test]
    fn the_classic_rule_keeps_and_drops() {
        let lifted = legacy_cloud_layers(&preset("Golden hour").unwrap().sky).layers;
        let (deck, cirrus) = (lifted[0].clone(), lifted[1].clone());
        assert_eq!(deck.shown_cover(), classic_share(CloudGenus::Stratocumulus, 0.15));
        assert_eq!(cirrus.shown_cover(), classic_share(CloudGenus::Cirrus, 0.35));
        for key in CLASSIC_KEYS {
            let mut layer = deck.clone();
            layer.edited(key);
            assert!(layer.classic.is_some(), "{key} keeps the layer classic");
        }
        let mut covered = deck.clone();
        covered.cover = Some(0.5);
        covered.edited("cover");
        assert_eq!(covered.classic.as_ref().unwrap().coverage, classic_amount_for_share(CloudGenus::Stratocumulus, 0.5));
        assert!((covered.shown_cover() - 0.5).abs() < 1.0e-5, "{}", covered.shown_cover());
        assert_eq!(covered.cover, Some(covered.shown_cover()), "the cover is the share it shows");
        let mut far = cirrus.clone();
        far.cover = Some(0.9);
        far.edited("cover");
        assert_eq!((far.classic.as_ref().unwrap().coverage, far.cover), (1.0, Some(CLASSIC_CIRRUS_SHARE[64])), "a share past the older cirrus' reach stops at its full amount");
        let mut softer = deck.clone();
        softer.stratocumulus.as_mut().unwrap().softness = 0.8;
        softer.edited("softness");
        assert_eq!(softer.classic.as_ref().unwrap().sharpness, 1.0 - 0.8);
        for key in ["sky_light", "lining", "shade_below", "billows", "rolls", "shading", "type", "fineness"] {
            let mut layer = deck.clone();
            let shown = layer.shown_cover();
            layer.edited(key);
            assert!(layer.classic.is_none(), "{key} drops classic");
            assert_eq!((layer.cover, layer.shown_cover()), (Some(shown), shown), "{key}: it keeps the Cover it showed");
        }
        // On a lifted fog deck (a classic Stratus) Density is the slab's own, which the classic drawing does not read:
        // it drops classic, at the Cover it showed.
        let mut fog = preset("Clear noon").unwrap();
        fog.sky.weather = Some(WeatherParams { kind: "fog".to_string(), ..WeatherParams::default() });
        let mut stratus = legacy_cloud_layers(&fog.sky).layers[0].clone();
        let shown = stratus.shown_cover();
        stratus.edited("density");
        assert_eq!((stratus.genus(), stratus.classic.is_none(), stratus.cover), (Some(CloudGenus::Stratus), true, Some(shown)), "density drops a classic Stratus");
        let modern = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
        for key in ["cover", "softness", "fineness", "type"] {
            let mut layer = modern.clone();
            layer.edited(key);
            assert_eq!(layer, modern, "{key} on a modern layer");
        }
        assert_eq!(modern.shown_cover(), CloudGenus::Cumulus.default_cover());
        assert_eq!(CloudLayerParams { cover: Some(0.2), ..modern.clone() }.shown_cover(), 0.2);
        assert_eq!(CloudLayerParams { genus: "noctilucent".to_string(), cover: None, ..modern.clone() }.shown_cover(), 0.0);
    }
}

//! The cloud species (WMO; the owner, 2026-11-01: "why not both"): each type's named forms, as data. A layer keeps its
//! sliders, and its Species row sets them to a named form in one go (`CloudLayerParams::set_species`), through the rows'
//! own names as the inspector and set_clouds write them; the sliders stay free afterwards. The row then shows the
//! species whose numbers the rows still hold (`CloudLayerParams::species_of`), or Custom.
//!
//! - One block per type, its default form first. A new layer is its type's first species: the block's defaults hold
//!   its numbers, and a Cover it sets other than the type's default is what a new layer is made with
//!   (`CloudGenus::new_cover`), so a saved layer with no Cover still draws at the default it was saved with.
//! - A species lists only the rows that make its form; the others (Base, the light rows, wind, seed) keep their values.
//!   The species of one type list the same rows, so picking one after another always gives the same layer.
//! - Types with one form have none: Nimbostratus, Altostratus, the Lenticular (not drawn since 2026-11-07) and Mammatus
//!   (a feature of other clouds).
//! - A row holds a species' number when the two are within half the row's step (`row_step`): the number its slider
//!   shows. The number is the species' as the layer's clamp() holds it, so a Tops under a high Base reads as the form.
//! - A layer names a species only while its rows hold it (`settle_species` after an edit).
//! - Each type's numbers are tuned in its own block. A type whose params block this build lacks keeps its rows as first
//!   values, by the names the design gives its rows (design §4.3); they are read once the block is there.
//!
//! Pure params code; the app calls it on the UI thread when it shows or writes a layer: a few layer copies a call.

use crate::{CloudGenus, CloudLayerParams};

/// One named form of a cloud type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudSpecies {
    /// The WMO species' name as files store it ("congestus"), and as the inspector's Species row shows it, under the
    /// Type row; `label` puts its type's before it ("Cumulus congestus").
    pub name: &'static str,
    /// The rows it sets, by their names (`CloudLayerParams::row`), each with the number that makes the form.
    pub rows: &'static [(&'static str, f32)],
}

/// Cirrus. fibratus is the type's defaults: long fibres whose ends curl a little. uncinus hooks them into commas with
/// trailing fall streaks; spissatus thickens them into dense, greyish patches; castellanus gathers them into short
/// tufted turrets in lines; floccus breaks them into small separate tufts trailing fall streaks.
const CIRRUS: &[CloudSpecies] = &[
    CloudSpecies { name: "fibratus", rows: &[("streak_length", 7.0), ("fineness", 0.6), ("hooks", 0.4), ("fall_streaks", 0.2), ("patchiness", 0.3), ("opacity", 0.5)] },
    CloudSpecies { name: "uncinus", rows: &[("streak_length", 9.0), ("fineness", 0.7), ("hooks", 0.9), ("fall_streaks", 0.6), ("patchiness", 0.5), ("opacity", 0.5)] },
    CloudSpecies { name: "spissatus", rows: &[("streak_length", 3.0), ("fineness", 0.2), ("hooks", 0.1), ("fall_streaks", 0.1), ("patchiness", 0.6), ("opacity", 0.9)] },
    CloudSpecies { name: "castellanus", rows: &[("streak_length", 2.0), ("fineness", 0.3), ("hooks", 0.2), ("fall_streaks", 0.3), ("patchiness", 0.7), ("opacity", 0.7)] },
    CloudSpecies { name: "floccus", rows: &[("streak_length", 1.5), ("fineness", 0.4), ("hooks", 0.1), ("fall_streaks", 0.7), ("patchiness", 0.85), ("opacity", 0.6)] },
];

/// Cirrocumulus (no block in this build: design §4.3's rows). stratiformis is the type's defaults, a rippled sheet of
/// grains; castellanus has larger grains rising as turrets; floccus is scattered ragged tufts; lenticularis small
/// lenses of fine, even ripples.
const CIRROCUMULUS: &[CloudSpecies] = &[
    CloudSpecies { name: "stratiformis", rows: &[("cover", 0.4), ("grain_m", 450.0), ("ripples", 0.6), ("regularity", 0.5), ("softness", 0.3), ("patch_km", 8.0)] },
    CloudSpecies { name: "castellanus", rows: &[("cover", 0.35), ("grain_m", 600.0), ("ripples", 0.3), ("regularity", 0.4), ("softness", 0.2), ("patch_km", 6.0)] },
    CloudSpecies { name: "floccus", rows: &[("cover", 0.25), ("grain_m", 350.0), ("ripples", 0.1), ("regularity", 0.15), ("softness", 0.6), ("patch_km", 3.0)] },
    CloudSpecies { name: "lenticularis", rows: &[("cover", 0.2), ("grain_m", 300.0), ("ripples", 0.8), ("regularity", 0.8), ("softness", 0.2), ("patch_km", 4.0)] },
];

/// Cirrostratus (the look lane's, round 7). nebulosus is the type's defaults: a milky veil, mottled, with faint fibres;
/// fibratus a thinner veil whose fibres along the wind show through it.
const CIRROSTRATUS: &[CloudSpecies] = &[
    CloudSpecies { name: "nebulosus", rows: &[("milkiness", 0.45), ("mottling", 0.4), ("fibres", 0.2)] },
    CloudSpecies { name: "fibratus", rows: &[("milkiness", 0.4), ("mottling", 0.2), ("fibres", 0.8)] },
];

/// Altocumulus (the look lane's, round 4). stratiformis is the type's defaults: a sheet of flat soft cloudlets touching
/// in rolls across the wind with narrow lanes of blue (the mackerel sky). castellanus raises some into turrets in
/// lines; floccus is loose ragged tufts at random with blue between; lenticularis a few large smooth lenses with crisp
/// outlines, long across the wind as a mountain wave's crests lie.
const ALTOCUMULUS: &[CloudSpecies] = &[
    CloudSpecies { name: "stratiformis", rows: &[("cover", 0.6), ("cell_m", 400.0), ("regularity", 0.3), ("rows", 0.25), ("turrets", 0.0), ("softness", 0.7)] },
    CloudSpecies { name: "castellanus", rows: &[("cover", 0.5), ("cell_m", 500.0), ("regularity", 0.5), ("rows", 0.4), ("turrets", 0.8), ("softness", 0.35)] },
    CloudSpecies { name: "floccus", rows: &[("cover", 0.35), ("cell_m", 350.0), ("regularity", 0.15), ("rows", 0.0), ("turrets", 0.2), ("softness", 0.9)] },
    CloudSpecies { name: "lenticularis", rows: &[("cover", 0.3), ("cell_m", 1800.0), ("regularity", 0.8), ("rows", 0.7), ("turrets", 0.0), ("softness", 0.15)] },
];

/// Stratus. nebulosus is the featureless sheet the owner made the default (2026-11-01): closed, the veil of the type's
/// defaults. fractus tears it into thin ragged shreds with blue between.
const STRATUS: &[CloudSpecies] = &[
    CloudSpecies { name: "nebulosus", rows: &[("cover", 1.0), ("size_km", 6.0), ("density", 0.35), ("mottling", 0.25), ("streaks", 0.5), ("ragged_edges", 0.4)] },
    CloudSpecies { name: "fractus", rows: &[("cover", 0.25), ("size_km", 2.0), ("density", 0.2), ("mottling", 0.3), ("streaks", 0.6), ("ragged_edges", 0.9)] },
];

/// Stratocumulus (lane sc, round 4: the forms come from the volume's Cover and Billows). stratiformis is the type's
/// defaults, a broken deck of small cells; castellanus is a deck with turrets on its cells (Billows over 0.6 raises
/// them, Cover 0.75 keeps the deck whole under them); floccus breaks into loose, separate soft tufts (at Cover 0.4 the
/// deck opens in many holes; Billows 0.6 raises no turret); lenticularis is a few smooth, flat lens cells drawn out along the
/// wind (Billows under 0.3 draws them out). Each sets the deck's Density too (lane sc round 5): castellanus thicker
/// (1.3), floccus' tufts thin and bright (0.6), the others the default 1.
const STRATOCUMULUS: &[CloudSpecies] = &[
    CloudSpecies { name: "stratiformis", rows: &[("cover", 0.6), ("size_km", 2.5), ("softness", 0.5), ("billows", 0.5), ("rolls", 0.3), ("density", 1.0)] },
    CloudSpecies { name: "castellanus", rows: &[("cover", 0.75), ("size_km", 2.0), ("softness", 0.3), ("billows", 0.9), ("rolls", 0.2), ("density", 1.3)] },
    CloudSpecies { name: "floccus", rows: &[("cover", 0.4), ("size_km", 1.2), ("softness", 0.7), ("billows", 0.6), ("rolls", 0.0), ("density", 0.6)] },
    CloudSpecies { name: "lenticularis", rows: &[("cover", 0.35), ("size_km", 4.0), ("softness", 0.25), ("billows", 0.1), ("rolls", 0.0), ("density", 1.0)] },
];

/// Cumulus, by how far the heaps grow. humilis is the type's defaults: flat-based fair-weather heaps of little height
/// (kloofendal_48d's). mediocris rises to small sprouting tops; congestus towers into cauliflower; fractus is small,
/// low, broken pieces with soft edges and no flat base. (In the GPU look a low Puffiness smooths the heaps, so the most
/// broken outline is Puffiness 1 with Softness 1.)
const CUMULUS: &[CloudSpecies] = &[
    CloudSpecies { name: "humilis", rows: &[("tops_m", 2800.0), ("size_km", 1.5), ("towers", 0.3), ("puffiness", 0.7), ("flat_base", 0.7), ("softness", 0.4)] },
    CloudSpecies { name: "mediocris", rows: &[("tops_m", 4000.0), ("size_km", 1.8), ("towers", 0.55), ("puffiness", 0.8), ("flat_base", 0.7), ("softness", 0.35)] },
    CloudSpecies { name: "congestus", rows: &[("tops_m", 6500.0), ("size_km", 2.5), ("towers", 0.85), ("puffiness", 0.9), ("flat_base", 0.75), ("softness", 0.3)] },
    CloudSpecies { name: "fractus", rows: &[("tops_m", 2000.0), ("size_km", 0.8), ("towers", 0.0), ("puffiness", 1.0), ("flat_base", 0.0), ("softness", 1.0)] },
];

/// Cumulonimbus (the owner's ruling, 2026-11-01: capillatus first). capillatus is the type's defaults: the tower has
/// spread a fibrous anvil, an overshooting dome over it. calvus is the same tower before its anvil: Anvil 0 draws none
/// (`cloud_placed`'s ANVIL_LEAST cuts it under 0.05), and a top with no anvil smooths into a bald white mass, with no
/// overshoot.
const CUMULONIMBUS: &[CloudSpecies] = &[
    CloudSpecies { name: "capillatus", rows: &[("tops_m", 12000.0), ("anvil", 0.6), ("overshoot", 0.3), ("puffiness", 0.7)] },
    CloudSpecies { name: "calvus", rows: &[("tops_m", 12000.0), ("anvil", 0.0), ("overshoot", 0.0), ("puffiness", 0.7)] },
];

/// The species of `genus`, its default form first; none for a type with one form.
pub fn cloud_species(genus: CloudGenus) -> &'static [CloudSpecies] {
    match genus {
        CloudGenus::Cirrus => CIRRUS,
        CloudGenus::Cirrocumulus => CIRROCUMULUS,
        CloudGenus::Cirrostratus => CIRROSTRATUS,
        CloudGenus::Altocumulus => ALTOCUMULUS,
        CloudGenus::Stratus => STRATUS,
        CloudGenus::Stratocumulus => STRATOCUMULUS,
        CloudGenus::Cumulus => CUMULUS,
        CloudGenus::Cumulonimbus => CUMULONIMBUS,
        CloudGenus::Altostratus | CloudGenus::Nimbostratus | CloudGenus::Lenticular | CloudGenus::Mammatus => &[],
    }
}

/// The species of `genus` that `name` names: its name or its label ("congestus", "Cumulus congestus"), trimmed and in
/// any case. None for a name the type does not have.
pub fn find_species(genus: CloudGenus, name: &str) -> Option<&'static CloudSpecies> {
    let name = match name.split_whitespace().collect::<Vec<_>>().as_slice() {
        [species] => *species,
        [type_name, species] if type_name.eq_ignore_ascii_case(genus.label()) => *species,
        _ => return None,
    };
    cloud_species(genus).iter().find(|species| species.name.eq_ignore_ascii_case(name))
}

/// The step of the row `key`'s slider on a `genus` layer: the grid it writes on, or, on a logarithmic one, the
/// precision it shows. Every row not named here runs 0 to 1 (or 0 to 4) in hundredths. The app holds its sliders to
/// it for the rows the species set.
pub fn row_step(genus: CloudGenus, key: &str) -> f32 {
    match key {
        // A Stratus's Base is logarithmic, shown to the metre (a lifted fog deck keeps its altitude).
        "base_m" if genus == CloudGenus::Stratus => 1.0,
        "base_m" | "tops_m" => 50.0,
        "scud_height_m" => 10.0,
        "size_km" | "streak_length" | "drift_km" => 0.1,
        // the placed types' logarithmic rows, shown to a tenth
        "distance_km" | "tower_km" | "length_km" | "width_km" => 0.1,
        "anvil_m" => 50.0,
        "thickness_m" | "gap_m" => 10.0,
        "plates" => 1.0,
                // An Altocumulus' Cell size is logarithmic, shown to the metre.
        "cell_m" => 1.0,
        "wind_deg" | "azimuth_deg" | "lean_deg" => 0.5,
        _ => 0.01,
    }
}

impl CloudSpecies {
    /// "Cumulus congestus": the type's name, then the species'.
    pub fn label(&self, genus: CloudGenus) -> String {
        format!("{} {}", genus.label(), self.name)
    }

    /// Whether the rows of `layer`, a `genus` layer, hold this species: each of its rows within half its step of the
    /// species' number as the layer's own clamp() holds it there. A form whose Tops a high Base will not let it reach
    /// (Tops are held 200 m over the Base) so reads as itself at the Tops it can have.
    pub fn holds(&self, genus: CloudGenus, layer: &CloudLayerParams) -> bool {
        let mut target = layer.clone();
        if !self.rows.iter().all(|&(key, value)| target.set_row(key, value)) {
            return false;
        }
        let target = target.clamped();
        self.rows.iter().all(|&(key, _)| match (layer.row(key), target.row(key)) {
            (Some(row), Some(form)) => (row - form).abs() < 0.5 * row_step(genus, key),
            _ => false,
        })
    }
}

impl CloudLayerParams {
    /// The species whose numbers the layer's rows hold (`CloudSpecies::holds`: each within half its row's step, as the
    /// layer's clamp() holds the species there); None (the row's Custom) when none does. The species named (`species`)
    /// is asked first, then the type's in its table's order. A classic layer draws its older look and holds none, nor
    /// does a type this build does not draw.
    pub fn species_of(&self) -> Option<&'static str> {
        let genus = self.genus().filter(|genus| genus.is_built())?;
        if self.classic.is_some() {
            return None;
        }
        let picked = self.species.as_deref().and_then(|name| find_species(genus, name));
        picked.into_iter().chain(cloud_species(genus)).find(|species| species.holds(genus, self)).map(|species| species.name)
    }

    /// Picks a species (the Species row, set_clouds' `species`): the layer takes its type's modern look (the classic
    /// rule drops `classic` for "species", a row no older look follows), each of the species' rows is written as its
    /// row writes it (`set_row`), and its name is recorded. False, with the layer untouched, for a name its type does
    /// not have (`find_species`) or a row its type has no number for in this build.
    pub fn set_species(&mut self, name: &str) -> bool {
        let Some(species) = self.genus().and_then(|genus| find_species(genus, name)) else {
            return false;
        };
        if species.rows.iter().any(|(key, _)| self.row(key).is_none()) {
            return false;
        }
        self.edited("species");
        for &(key, value) in species.rows {
            self.set_row(key, value);
        }
        self.species = Some(species.name.to_string());
        true
    }

    /// Keeps the name true to the numbers after an edit (a finished slider, set_clouds' fields, a patch): a name this
    /// build reads (a species of the layer's drawn type) that the rows no longer hold becomes the species they do hold,
    /// or none, so a saved file names a species only while its numbers hold it. A name this build does not read is
    /// kept as written, and none is added where none was named.
    pub fn settle_species(&mut self) {
        let Some(genus) = self.genus().filter(|genus| genus.is_built()) else {
            return;
        };
        let Some(named) = self.species.as_deref().and_then(|name| find_species(genus, name)) else {
            return;
        };
        if self.classic.is_some() || !named.holds(genus, self) {
            self.species = self.species_of().map(str::to_string);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cloud_layers_view, CloudStackParams, HdriParams, CLOUD_GENUS_BUILT};
    use makepad_draw::makepad_micro_serde::{DeJson, JsonValue, SerJson};

    /// The layer as clamp() leaves it in a map of its own, and what clamp() reported.
    fn held(layer: &CloudLayerParams) -> (CloudLayerParams, Vec<String>) {
        let mut p = HdriParams::default();
        p.sky.cloud_stack = Some(CloudStackParams { layers: vec![layer.clone()], ..CloudStackParams::default() });
        let report = p.clamp();
        let mut layers = p.sky.cloud_stack.expect("clamp keeps the stack").layers;
        (layers.remove(0), report)
    }

    /// The rows a species sets, sorted: the same for every species of a type.
    fn keys(species: &CloudSpecies) -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = species.rows.iter().map(|(key, _)| *key).collect();
        keys.sort_unstable();
        keys
    }

    /// The species (the owner, 2026-11-01): every species of every type this build draws sets its rows, inside the
    /// ranges clamp() keeps, and reads back, whichever species the layer held before; a type's first species is its
    /// defaults, so a new layer reads as it (a new Stratus is nebulosus, the closed sheet). A row moved one step off reads
    /// Custom (None), under half a step off it still holds; no two species of a type hold at once. A name the type does
    /// not have changes nothing. Classic layers hold none (the lift gives none) until one is picked, which gives the
    /// layer its type's look; a Type change drops the species picked; a patched layer's species gives its rows, but for
    /// the numbers the patch gives.
    #[test]
    fn a_species_sets_its_rows_and_reads_back() {
        // The table: one lower-case word a species, once a type, each row once, the same rows across a type; the types
        // of one form have none; a species is found by its name or its label, in any case.
        for genus in CloudGenus::ALL {
            let table = cloud_species(genus);
            let one_form = matches!(genus, CloudGenus::Nimbostratus | CloudGenus::Altostratus | CloudGenus::Lenticular | CloudGenus::Mammatus);
            assert_eq!(table.is_empty(), one_form, "{genus:?}");
            for (i, species) in table.iter().enumerate() {
                assert!(!species.name.is_empty() && species.name.chars().all(|c| c.is_ascii_lowercase()), "{genus:?}: {}", species.name);
                assert!(table[..i].iter().all(|other| other.name != species.name), "{genus:?}: {} twice", species.name);
                let rows = keys(species);
                assert!(!rows.is_empty() && rows.windows(2).all(|pair| pair[0] != pair[1]), "{genus:?} {}: {rows:?}", species.name);
                assert_eq!(rows, keys(&table[0]), "{genus:?} {}: the rows of its type's first species", species.name);
                assert_eq!(find_species(genus, species.name), Some(species));
                assert_eq!(find_species(genus, &format!("  {}  ", species.label(genus).to_uppercase())), Some(species));
            }
        }
        assert_eq!(cloud_species(CloudGenus::Cumulus)[2].label(CloudGenus::Cumulus), "Cumulus congestus");
        assert_eq!(find_species(CloudGenus::Stratus, "Cumulus fractus"), None, "a label names its own type");
        assert_eq!(find_species(CloudGenus::Cumulus, "nebulosus"), None);
        assert_eq!(find_species(CloudGenus::Cumulus, "cumulus congestus extra"), None);
        // Every species of each type this build draws, from a new layer and from each species of its type.
        for genus in CLOUD_GENUS_BUILT {
            let table = cloud_species(genus);
            let fresh = CloudLayerParams::new(genus, &[], 1);
            assert_eq!(fresh.species_of(), table.first().map(|species| species.name), "{genus:?}: a new layer is its first species");
            for species in table {
                for from in std::iter::once(None).chain(table.iter().map(Some)) {
                    let mut layer = fresh.clone();
                    if let Some(from) = from {
                        assert!(layer.set_species(from.name));
                    }
                    assert!(layer.set_species(species.name), "{genus:?} {}", species.name);
                    assert_eq!(layer.species.as_deref(), Some(species.name), "{genus:?}: the name is recorded");
                    let (kept, report) = held(&layer);
                    assert!(report.is_empty(), "{genus:?} {}: clamp() moved {report:?}", species.name);
                    assert_eq!(kept.species_of(), Some(species.name), "{genus:?} {} after {:?}", species.name, from.map(|f| f.name));
                    for &(key, value) in species.rows {
                        assert_eq!(kept.row(key), Some(value), "{genus:?} {}: {key}", species.name);
                    }
                    for other in table.iter().filter(|other| other.name != species.name) {
                        assert!(!other.holds(genus, &kept), "{genus:?}: {} holds {} too", species.name, other.name);
                    }
                }
                // A row one step off reads Custom; under half a step off it holds (the number its slider shows).
                let mut picked = fresh.clone();
                assert!(picked.set_species(species.name));
                for &(key, value) in species.rows {
                    let step = row_step(genus, key);
                    let mut up = picked.clone();
                    up.set_row(key, value + step);
                    let up = held(&up).0;
                    let sign = if (up.row(key).unwrap() - (value + step)).abs() < 1.0e-4 { 1.0 } else { -1.0 };
                    let mut moved = picked.clone();
                    moved.set_row(key, value + sign * step);
                    let moved = held(&moved).0;
                    assert!((moved.row(key).unwrap() - (value + sign * step)).abs() < 1.0e-4, "{genus:?} {}: {key} moved in range", species.name);
                    assert_eq!(moved.species_of(), None, "{genus:?} {}: {key} one step off", species.name);
                    let mut near = picked.clone();
                    near.set_row(key, value + sign * 0.4 * step);
                    assert_eq!(near.species_of(), Some(species.name), "{genus:?} {}: {key} under half a step off", species.name);
                }
            }
        }
        // A name the type does not have, a type of one form, an unknown type or one with no block in this build: refused,
        // the layer untouched.
        let mut cumulus = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
        let before = cumulus.clone();
        for name in ["nebulosus", "Stratus fractus", "", "congestus extra"] {
            assert!(!cumulus.set_species(name) && cumulus == before, "{name}");
        }
        let mut nimbostratus = CloudLayerParams::new(CloudGenus::Nimbostratus, &[], 1);
        assert!(!nimbostratus.set_species("nebulosus") && nimbostratus.species_of().is_none());
        let mut unknown = CloudLayerParams { genus: "noctilucent".to_string(), species: Some("fractus".to_string()), ..CloudLayerParams::default() };
        assert!(!unknown.set_species("fractus") && unknown.species_of().is_none());
        let mut cirrocumulus = CloudLayerParams::new(CloudGenus::Cirrocumulus, &[], 1);
        let before = cirrocumulus.clone();
        assert!(!cirrocumulus.set_species("stratiformis") && cirrocumulus == before, "no block yet: its rows are first values");
        // Classic layers: the lift gives none and they hold none (they draw their older look). Picked, a species gives
        // the layer its type's look at the species' numbers.
        let golden = crate::presets::preset("Golden hour").unwrap();
        let view = cloud_layers_view(&golden);
        assert!(view.layers.iter().all(|layer| layer.classic.is_some() && layer.species.is_none() && layer.species_of().is_none()), "{view:?}");
        let mut deck = view.layers.iter().find(|layer| layer.genus() == Some(CloudGenus::Stratocumulus)).expect("Golden hour's deck").clone();
        assert!(deck.set_species("castellanus"));
        assert!(deck.classic.is_none(), "the species gives it its type's look");
        assert_eq!(deck.species_of(), Some("castellanus"));
        // A Type change drops the species picked: Cumulus and Stratus both have a fractus. The new type's block shows
        // what it holds, here its defaults: a Stratus at its default Cover is nebulosus.
        let mut shreds = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
        assert!(shreds.set_species("fractus"));
        shreds.set_genus(CloudGenus::Stratus, &[]);
        assert_eq!((shreds.species.as_deref(), shreds.species_of()), (None, Some("nebulosus")));
        // A Tops the species cannot reach over a high Base (each Cumulus species with its Base 175 m under its Tops, and
        // fractus from 2500 m) is held 200 m over the Base, and the form still reads as itself, before clamp() and after.
        let tops = |species: &CloudSpecies| species.rows.iter().find(|(key, _)| *key == "tops_m").map(|&(_, value)| value).unwrap();
        let fractus = find_species(CloudGenus::Cumulus, "fractus").unwrap();
        let high: Vec<(&CloudSpecies, f32)> = cloud_species(CloudGenus::Cumulus).iter().map(|species| (species, tops(species) - 175.0)).chain([(fractus, 2500.0)]).collect();
        for (species, base) in high {
            let mut layer = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
            assert!(layer.set_row("base_m", base) && layer.set_species(species.name));
            assert_eq!(layer.species_of(), Some(species.name), "{} over a Base of {base}", species.name);
            let (kept, report) = held(&layer);
            assert_eq!(report, ["sky.cloud_stack.layers[0].cumulus.tops_m"], "{} over a Base of {base}", species.name);
            assert_eq!(kept.cumulus_params().tops_m, base + 200.0, "{} over a Base of {base}", species.name);
            assert_eq!(kept.species_of(), Some(species.name), "{} over a Base of {base}, held", species.name);
            let mut again = kept.clone();
            again.settle_species();
            assert_eq!(again, kept, "{}: the name holds", species.name);
        }
        // The name follows the numbers after an edit (`settle_species`): off the form it goes, on another species it
        // becomes that one; a name this build does not read stays, and none is added where none was named.
        let mut moved = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
        assert!(moved.set_species("congestus"));
        moved.set_row("towers", 0.5);
        moved.settle_species();
        assert_eq!(moved.species, None, "off congestus' Towers");
        let mut other = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
        assert!(other.set_species("congestus"));
        for &(key, value) in find_species(CloudGenus::Cumulus, "mediocris").unwrap().rows {
            other.set_row(key, value);
        }
        other.settle_species();
        assert_eq!(other.species.as_deref(), Some("mediocris"), "moved onto mediocris' numbers");
        let mut unread = CloudLayerParams { species: Some("pileus".to_string()), ..CloudLayerParams::new(CloudGenus::Cumulus, &[], 1) };
        unread.set_row("towers", 0.5);
        unread.settle_species();
        assert_eq!(unread.species.as_deref(), Some("pileus"), "a name this build does not read is kept");
        let mut unnamed = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
        unnamed.settle_species();
        assert_eq!((unnamed.species.as_deref(), unnamed.species_of()), (None, Some("humilis")), "none is added");
        // A patched layer's species gives its rows, but for the numbers the patch gives, and keeps its name only while
        // they hold it; a name that holds is written as the table has it, and that is said. A species of a type whose
        // block this build lacks sets none of its rows (Cover neither) and keeps its name as written.
        let patch = r#"{"sky": {"cloud_stack": {"layers": [{"genus": "cumulus", "species": "Cumulus Congestus", "cumulus": {"lean": 0.5, "towers": 0.6}}, {"genus": "stratus", "species": "fractus", "cover": 0.5}, {"genus": "cumulus", "species": "Cumulus Mediocris"}, {"genus": "cirrocumulus", "species": "castellanus"}]}}}"#;
        let (p, report) = HdriParams::default().merge_json(patch).unwrap();
        assert_eq!(report, ["sky.cloud_stack.layers[2].species"]);
        let layers = &p.sky.cloud_stack.as_ref().unwrap().layers;
        let c = layers[0].cumulus_params();
        assert_eq!((c.tops_m, c.size_km, c.puffiness, c.flat_base, c.softness, c.towers, c.lean), (6500.0, 2.5, 0.9, 0.75, 0.3, 0.6, 0.5));
        assert_eq!((layers[0].species.as_deref(), layers[0].species_of()), (None, None), "its own Towers is off the species'");
        let s = layers[1].stratus_params();
        assert_eq!((layers[1].cover, s.density, s.ragged_edges), (Some(0.5), 0.2, 0.9));
        assert_eq!((layers[1].species.as_deref(), layers[1].species_of()), (None, None), "its own Cover is off the species'");
        assert_eq!((layers[2].species.as_deref(), layers[2].species_of()), (Some("mediocris"), Some("mediocris")));
        assert_eq!(layers[2].cumulus_params().tops_m, 4000.0);
        assert_eq!((layers[3].species.as_deref(), layers[3].cover), (Some("castellanus"), None), "nothing of the form is set");
    }

    /// Older files and new layers (the owner's species, 2026-11-01: a new Stratus is nebulosus): a Stratus saved without
    /// a Cover (as "+ Cloud", a Type change or set_clouds made one before species) loads and draws at the 0.6 it was
    /// saved with and reads Custom, while a Stratus made now is closed, Cover 1, and reads nebulosus.
    #[test]
    fn a_saved_stratus_keeps_its_cover_and_a_new_one_is_closed() {
        let layer = r#"{"enabled": true, "name": "Stratus 1", "genus": "stratus", "seed": 4242, "stratus": {"base_m": 400, "size_km": 6, "density": 0.35, "mottling": 0.25, "streaks": 0.5, "ragged_edges": 0.4}}"#;
        let mut tree = JsonValue::deserialize_json(&HdriParams::default().to_json()).unwrap();
        let stack = format!(r#"{{"cover": 1, "march_steps": 64, "layers": [{layer}]}}"#);
        tree.get_mut("sky").unwrap().as_object_mut().unwrap().insert("cloud_stack".to_string(), JsonValue::deserialize_json(&stack).unwrap());
        let p = HdriParams::from_json(&tree.serialize_json()).expect("a file from before species loads");
        let saved = &p.sky.cloud_stack.as_ref().unwrap().layers[0];
        assert_eq!((saved.cover, saved.species.as_deref()), (None, None));
        assert_eq!(saved.shown_cover(), 0.6, "it draws at the Cover it was saved with");
        assert_eq!(saved.species_of(), None, "patches at 0.6: Custom");
        let made = CloudLayerParams::new(CloudGenus::Stratus, &[], 1);
        assert_eq!((made.cover, made.shown_cover(), made.species_of()), (Some(1.0), 1.0, Some("nebulosus")));
    }
}

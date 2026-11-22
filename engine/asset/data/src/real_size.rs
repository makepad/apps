//! The one rule for how big a model is when it goes into a game that did not
//! make it: its REAL size, in metres.
//!
//! A model's own metadata says what one of its units is
//! ([`Calibration`]): a pack or classic import states its calibration
//! (`scale_real` anchor, [`crate::Dimensions`]); a makepad-model product (a
//! game's built model, an editable model) is authored in metres; anything
//! else (a dropped GLB, an AI mesh, an older entry that never said) is
//! unitless. The [`Category`] a model is (a car, a kart, a bus, a person, a
//! light plane, a tree, ...) carries the real-world range of its one
//! defining measure. [`real_size`] is the rule:
//!
//! - a model is drawn at what its metadata says its units are;
//! - a vehicle, a person or a weapon that then falls outside its
//!   category's range (a toy-sized race car, a comic kart three metres
//!   long) is brought to the nearest end of the range: a game may play at
//!   its own scale, but its models are the real thing everywhere else;
//! - a unitless model is taken as metres when its measure is a real one,
//!   else fitted to its category's typical measure (a rig keeps its own
//!   units: a classic game's monsters are not people);
//! - a model of no known category keeps its units (or is brought to a
//!   metre and a half when they clearly are not metres).
//!
//! The ranges are real-world facts per category, not tuning: each row
//! below says what it is measured on and where the numbers come from.
//! [`check`] is the drift test the scan tool and the importer tests run.

use crate::asset::AssetKind;

/// Which extent a category is defined by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    /// Up (Y): people, trees, buildings.
    Height,
    /// The longer horizontal extent: road vehicles, boats, trains.
    Length,
    /// The longer horizontal extent of an aircraft: its wingspan or length.
    Span,
    /// The largest extent: hand-held things.
    Largest,
}

impl Measure {
    /// This measure of an extent `[x, y, z]` (Y up).
    pub fn of(self, extent: [f32; 3]) -> f32 {
        match self {
            Measure::Height => extent[1],
            Measure::Length | Measure::Span => extent[0].max(extent[2]),
            Measure::Largest => extent[0].max(extent[1]).max(extent[2]),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Measure::Height => "height",
            Measure::Length => "length",
            Measure::Span => "span",
            Measure::Largest => "largest",
        }
    }
}

/// What a model is, as far as its size goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Person,
    Kart,
    Car,
    Van,
    Truck,
    Bus,
    Motorbike,
    Bicycle,
    Boat,
    Plane,
    Helicopter,
    Train,
    Tree,
    Bush,
    Building,
    Weapon,
}

/// A category's real-world measure, metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub measure: Measure,
    pub lo: f32,
    pub typical: f32,
    pub hi: f32,
}

impl Range {
    /// In the range, to a millimetre in a kilometre (a model brought to an
    /// end of the range lands there to float precision).
    pub fn contains(&self, v: f32) -> bool {
        let slack = 1.0e-6 * self.hi;
        v >= self.lo - slack && v <= self.hi + slack
    }
}

impl Category {
    pub const ALL: [Category; 16] = [
        Category::Person, Category::Kart, Category::Car, Category::Van, Category::Truck, Category::Bus,
        Category::Motorbike, Category::Bicycle, Category::Boat, Category::Plane, Category::Helicopter,
        Category::Train, Category::Tree, Category::Bush, Category::Building, Category::Weapon,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Person => "person",
            Category::Kart => "kart",
            Category::Car => "car",
            Category::Van => "van",
            Category::Truck => "truck",
            Category::Bus => "bus",
            Category::Motorbike => "motorbike",
            Category::Bicycle => "bicycle",
            Category::Boat => "boat",
            Category::Plane => "plane",
            Category::Helicopter => "helicopter",
            Category::Train => "train",
            Category::Tree => "tree",
            Category::Bush => "bush",
            Category::Building => "building",
            Category::Weapon => "weapon",
        }
    }

    /// The real-world range of this category's measure (metres).
    pub fn range(self) -> Range {
        let r = |measure, lo, typical, hi| Range { measure, lo, typical, hi };
        match self {
            // Standing adults: 1.40 m (short adult) to 2.10 m (very tall);
            // the yardstick person is crate::dimensions::PERSON_HEIGHT.
            Category::Person => r(Measure::Height, 1.40, crate::dimensions::PERSON_HEIGHT, 2.10),
            // Go-karts: rental and racing karts are 1.5-2.1 m long (CIK-FIA
            // max 2.1 m); a kart with a bodywork shell up to 2.4 m.
            Category::Kart => r(Measure::Length, 1.5, 1.8, 2.4),
            // Cars: a city microcar is 2.5 m, a compact 4.0, a mid-size
            // sedan or GT3 race car 4.5-4.8, a full-size sedan 5.2-5.8.
            Category::Car => r(Measure::Length, 2.5, 4.5, 5.8),
            // Vans: a compact van 4.2 m to a long-wheelbase panel van 7.0.
            Category::Van => r(Measure::Length, 4.0, 5.2, 7.0),
            // Trucks: a pickup 5.0 m, a box truck 7-10, a tractor-trailer
            // up to 18.75 (EU) .
            Category::Truck => r(Measure::Length, 4.8, 7.5, 18.75),
            // Buses: a minibus 7 m, a city bus 12, an articulated bus 18.
            Category::Bus => r(Measure::Length, 7.0, 12.0, 18.75),
            // Motorbikes and scooters: 1.7-2.5 m long.
            Category::Motorbike => r(Measure::Length, 1.7, 2.1, 2.5),
            // Bicycles: 1.5-1.9 m long.
            Category::Bicycle => r(Measure::Length, 1.5, 1.75, 1.9),
            // Boats: a dinghy or rowing boat 2.5 m, a speedboat 6, a yacht
            // or a sailing ship up to 60.
            Category::Boat => r(Measure::Length, 2.0, 6.0, 60.0),
            // Aircraft by their larger of span and length: a light plane or
            // trainer 8-11 m, a fighter 10-20, an airliner up to 80.
            Category::Plane => r(Measure::Span, 6.0, 10.0, 80.0),
            // Helicopters, rotor tip to tail: 8 m (light) to 20 m.
            Category::Helicopter => r(Measure::Span, 7.0, 12.0, 20.0),
            // Rail vehicles: a locomotive, coach or wagon is 8-27 m.
            Category::Train => r(Measure::Length, 8.0, 20.0, 27.0),
            // Trees: a young tree 2.5 m to a tall forest tree 60 m.
            Category::Tree => r(Measure::Height, 2.5, 10.0, 60.0),
            // Shrubs and bushes: 0.3-4 m.
            Category::Bush => r(Measure::Height, 0.3, 1.2, 4.0),
            // A building: a one-storey shed 2.5 m to a skyscraper; one
            // storey is about 3 m.
            Category::Building => r(Measure::Height, 2.5, 9.0, 500.0),
            // Hand-held weapons: a knife 0.15 m to a long gun or a polearm
            // 2.2 m.
            Category::Weapon => r(Measure::Largest, 0.15, 0.9, 2.2),
        }
    }

    pub fn parse(s: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// Word lists (lower case, whole tokens of a title, alias or tag).
const WORDS: &[(Category, &[&str])] = &[
    (Category::Kart, &["kart", "karts", "gokart", "go-kart"]),
    (Category::Bus, &["bus", "coach", "minibus", "schoolbus"]),
    (Category::Van, &["van", "minivan", "camper", "campervan"]),
    (Category::Truck, &["truck", "trucks", "lorry", "firetruck", "garbage", "dumptruck", "semi"]),
    (Category::Motorbike, &["motorbike", "motorcycle", "scooter", "moped", "dirtbike"]),
    (Category::Bicycle, &["bicycle", "bike", "cycle"]),
    (Category::Helicopter, &["helicopter", "heli", "chopper"]),
    (Category::Plane, &["plane", "planes", "airplane", "aeroplane", "aircraft", "jetliner", "biplane", "glider", "seaplane", "twinprop", "airliner"]),
    (Category::Boat, &["boat", "boats", "ship", "yacht", "canoe", "kayak", "raft", "dinghy", "sailboat", "speedboat", "ferry", "galleon", "rowboat", "submarine"]),
    (Category::Train, &["locomotive", "train", "wagon", "tram", "carriage"]),
    (Category::Car, &["car", "cars", "sedan", "coupe", "hatchback", "hatch", "convertible", "roadster", "saloon", "taxi", "police", "racer", "racecar", "gt3", "suv", "jeep", "buggy", "beetle", "2cv", "limo", "limousine", "ambulance", "automobile", "muscle", "sportscar", "offroad"]),
    (Category::Tree, &["tree", "trees", "pine", "oak", "birch", "fir", "spruce", "conifer", "willow", "maple", "cedar", "cypress", "palm", "beech"]),
    (Category::Bush, &["bush", "bushes", "shrub", "hedge"]),
    (Category::Person, &["person", "people", "human", "pedestrian", "walker", "soldier", "survivor"]),
    (Category::Weapon, &["sword", "rifle", "pistol", "shotgun", "smg", "revolver", "knife", "axe", "dagger", "katana", "crossbow", "spear", "mace", "blaster", "gun"]),
    (Category::Building, &["house", "building", "tower", "church", "barn", "cottage", "warehouse", "factory", "skyscraper", "apartment", "apartments", "hangar", "castle", "shop", "store", "station"]),
];

/// Words that make a rig a creature, not a person: it keeps its own size.
const CREATURES: &[&str] = &[
    "goblin", "orc", "troll", "ogre", "monster", "boss", "dog", "cat", "horse", "deer", "boar", "wolf", "bear", "fox",
    "demon", "fiend", "shambler", "zombie", "skeleton", "creature", "robot", "mech", "alien", "dragon", "slime",
    "spider", "rat", "bird", "fish", "shark", "dinosaur", "animal", "pet", "chicken", "cow", "pig", "sheep",
];

/// Words that make a name a PART or a PLACE, not the thing: a bus stop, a
/// car park, a boat dock, a tree stump, a tyre, a wheel.
const NOT_THE_THING: &[&str] = &[
    "stop", "park", "parking", "dock", "pier", "jetty", "stump", "log", "wheel", "wheels", "tire", "tyre", "tires",
    "tyres", "hub", "hubcap", "seat", "door", "window", "sign", "garage", "lift", "ramp", "track", "rail", "rails",
    "road", "tile", "kit", "part", "parts", "debris", "wreck", "shell", "icon", "billboard", "sprite", "trail",
    "mark", "marker", "crash", "gate", "fence", "wall", "tracks", "arms", "hud", "pickup", "ammo", "wash", "stand",
];

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).map(|t| t.to_ascii_lowercase())
}

/// The category a model is, from its catalog kind, and the words of its
/// alias's last segment, its title and its tags. The kind settles people
/// (a Character is a person) and the default
/// of a Vehicle (a car); words settle the rest. `None` when nothing says.
pub fn categorize(kind: Option<AssetKind>, alias: &str, title: &str, tags: &[String]) -> Option<Category> {
    let stem = alias.rsplit('/').next().unwrap_or(alias);
    // A title's trailing "(Game Name)" names the game, not the thing.
    let title = title.split(" (").next().unwrap_or(title);
    // The thing's own name only: a pack's tags name its theme (every brick
    // of a brick kit is tagged "building"), not the piece.
    let words: Vec<String> = tokens(stem).chain(tokens(title)).collect();
    let has = |list: &[&str]| words.iter().any(|w| list.contains(&w.as_str()));
    let named = |c: Category| WORDS.iter().find(|(k, _)| *k == c).is_some_and(|(_, list)| has(list));
    let named_part = has(NOT_THE_THING);
    let vehicle = |c: Category| !matches!(c, Category::Person | Category::Tree | Category::Bush | Category::Building | Category::Weapon);
    let found = WORDS.iter().map(|(c, _)| *c).find(|c| named(*c));
    match kind {
        // A rig is a person (a kart game's `char-kart-zippy` is its
        // driver), unless its name says creature.
        Some(AssetKind::Character) => (!has(CREATURES)).then_some(Category::Person),
        Some(AssetKind::Vehicle) => Some(match found {
            Some(c) if vehicle(c) => c,
            // The aircraft shelf's tag says what its name may not.
            _ if tags.iter().any(|t| t == "plane") => Category::Plane,
            _ => Category::Car,
        }),
        Some(AssetKind::Weapon) => Some(Category::Weapon),
        // A police STATION is a building, a bus STOP a place.
        _ if named_part => None,
        _ if named(Category::Building) => Some(Category::Building),
        _ => found,
    }
}

/// What a model's metadata says one of its units is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Calibration {
    /// Authored in metres by makepad-model (a game's built model, an
    /// editable model): 1 unit = 1 m.
    Metric,
    /// An importer measured it: metres per unit (a pack's `scale_real`).
    Stated(f32),
    /// Nothing says (or an importer took the authored units as metres
    /// without measuring: a classic converter's baked metres, an
    /// uncalibrated pack).
    Unknown,
}

impl Calibration {
    /// From a manifest's facts: its generator, `units_per_meter`, and its
    /// `scale_real` anchor (the importer's metres per unit) when it has one.
    pub fn of(generator: Option<&str>, units_per_meter: f32, scale_real: Option<f32>) -> Calibration {
        if generator == Some("makepad-model") && units_per_meter == 1.0 {
            return Calibration::Metric;
        }
        // A pack the importer could not calibrate states 1 ("authored units
        // taken as metres"): that is no statement.
        if let Some(k) = scale_real.filter(|k| k.is_finite() && *k > 0.0 && *k != 1.0) {
            return Calibration::Stated(k);
        }
        if units_per_meter.is_finite() && units_per_meter > 0.0 && units_per_meter != 1.0 {
            return Calibration::Stated(1.0 / units_per_meter);
        }
        Calibration::Unknown
    }

    /// The same from a whole manifest.
    pub fn of_manifest(manifest: &crate::asset::AssetManifest) -> Calibration {
        let scale_real = manifest.anchors.iter().find(|a| a.name == crate::dimensions::ANCHOR_SCALE_REAL).map(|a| a.transform.scale.y);
        Calibration::of(manifest.provenance.as_ref().map(|p| p.generator.as_str()), manifest.coordinate_system.units_per_meter, scale_real)
    }

    pub fn metres_per_unit(self) -> Option<f32> {
        match self {
            Calibration::Metric => Some(1.0),
            Calibration::Stated(k) => Some(k),
            Calibration::Unknown => None,
        }
    }
}

/// How a model's real size was decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// Its calibration (an importer's measure, metres a game authored),
    /// inside its category's range or of a category not corrected.
    Calibrated,
    /// Its calibration put a vehicle, a person or a weapon outside its
    /// category's range: brought to the range's nearest end.
    Corrected,
    /// Uncalibrated, but its native measure is a real one: metres.
    AuthoredMetres,
    /// Uncalibrated and off: fitted to its category's typical measure.
    Estimated,
    /// Uncalibrated, no category: its authored units as metres, or brought
    /// to a metre and a half when they clearly are not.
    Unknown,
}

impl Basis {
    pub fn as_str(self) -> &'static str {
        match self {
            Basis::Calibrated => "calibrated",
            Basis::Corrected => "corrected",
            Basis::AuthoredMetres => "authored-metres",
            Basis::Estimated => "estimated",
            Basis::Unknown => "unknown",
        }
    }
}

/// A model's real size: the uniform scale (metres per native unit) to draw
/// it at, and why.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RealSize {
    pub scale: f32,
    pub basis: Basis,
    pub category: Option<Category>,
}

impl RealSize {
    /// The real extent (metres) of a model whose native extent is `extent`.
    pub fn extent(&self, extent: [f32; 3]) -> [f32; 3] {
        extent.map(|e| e * self.scale)
    }
}

/// The largest extent a model of no category may have in its own units
/// and still be taken as metres: a prop from a 5 cm die to a 60 m hall.
const PLAUSIBLE_METRES: std::ops::RangeInclusive<f32> = 0.05..=60.0;
/// What a model of no category and implausible units is brought to.
const UNKNOWN_FIT: f32 = 1.5;

impl Category {
    /// A road vehicle, a boat, an aircraft, a person or a weapon is one
    /// size in the real world: a model drawn outside its range (a comic
    /// kart, a toy race car, a chibi islander) is the real thing anywhere
    /// else. Scenery (trees, bushes, buildings) and rail vehicles (a toy
    /// train is a toy, a coaster car is no locomotive) vary too much to be
    /// corrected; they keep their own size.
    pub fn corrects(self) -> bool {
        !matches!(self, Category::Tree | Category::Bush | Category::Building | Category::Train)
    }
}

/// THE rule: the scale that draws a model of native extent `extent`
/// (`[x, y, z]`, Y up), `calibration` and `category` at its real size.
///
/// - A model whose metadata says its units (an importer's measured
///   calibration, metres a game authored) is drawn at them, unless it is a
///   vehicle, a person or a weapon whose measure then falls outside its
///   category's range: it is brought to the nearest end of that range, the
///   smallest change that makes it real (a 2.94 m comic kart is a 2.4 m
///   kart in another game, a 1.05 m chibi islander a 1.4 m person).
/// - A model whose units nothing states is taken as metres when its
///   category's measure is a real one, else fitted to the category's
///   typical measure; a rig keeps its own units (a classic game's
///   monsters are their own sizes).
/// - A model of no known category keeps its units, or is brought to a
///   metre and a half when they clearly are not metres.
pub fn real_size(calibration: Calibration, category: Option<Category>, extent: [f32; 3]) -> RealSize {
    let ok = |s: f32| s.is_finite() && s > 0.0;
    let make = |scale: f32, basis| RealSize { scale: if ok(scale) { scale } else { 1.0 }, basis, category };
    match (calibration.metres_per_unit(), category) {
        (Some(k), Some(c)) if c.corrects() => {
            let range = c.range();
            let native = range.measure.of(extent);
            let real = native * k;
            if native <= 0.0 || range.contains(real) {
                make(k, Basis::Calibrated)
            } else {
                make(real.clamp(range.lo, range.hi) / native, Basis::Corrected)
            }
        }
        (Some(k), _) => make(k, Basis::Calibrated),
        // A rig whose units nothing states is drawn as authored.
        (None, Some(Category::Person)) => make(1.0, Basis::Unknown),
        (None, Some(c)) => {
            let range = c.range();
            let native = range.measure.of(extent);
            if native <= 0.0 || range.contains(native) {
                make(1.0, Basis::AuthoredMetres)
            } else {
                make(range.typical / native, Basis::Estimated)
            }
        }
        (None, None) => {
            let largest = Measure::Largest.of(extent);
            if largest <= 0.0 || PLAUSIBLE_METRES.contains(&largest) {
                make(1.0, Basis::Unknown)
            } else {
                make(UNKNOWN_FIT / largest, Basis::Unknown)
            }
        }
    }
}

/// The drift check: `None` when a model drawn `scale` times its native
/// `extent` lies in its category's real range, else what is wrong. A model
/// of no category is never flagged.
pub fn check(category: Option<Category>, extent: [f32; 3], scale: f32) -> Option<String> {
    let c = category?;
    let range = c.range();
    let v = range.measure.of(extent) * scale;
    (!range.contains(v)).then(|| format!("{} {} {v:.2} m, a {} is {}..{} m", c.as_str(), range.measure.as_str(), c.as_str(), range.lo, range.hi))
}

/// How far a measure is from its category's range, as a factor (1.0 =
/// inside): 2.0 is twice too big or half too small.
pub fn off_factor(category: Option<Category>, extent: [f32; 3], scale: f32) -> f32 {
    let Some(c) = category else { return 1.0 };
    let range = c.range();
    let v = range.measure.of(extent) * scale;
    if !(v > 0.0) {
        return 1.0;
    }
    if v < range.lo { range.lo / v } else if v > range.hi { v / range.hi } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn categories_come_from_kind_and_words() {
        let none: [String; 0] = [];
        assert_eq!(categorize(Some(AssetKind::Vehicle), "sandbox/models/race/valley-gt3", "GT3 (Valley Ring GT)", &none), Some(Category::Car));
        assert_eq!(categorize(Some(AssetKind::Vehicle), "sandbox/models/kart/kart-zippy", "Zippy kart (Turbo Tots Grand Prix)", &none), Some(Category::Kart));
        assert_eq!(categorize(Some(AssetKind::Vehicle), "sandbox/models/citydrive/vd-bus", "Bus (Veloria Drive)", &none), Some(Category::Bus));
        assert_eq!(categorize(Some(AssetKind::Vehicle), "sandbox/models/flight/kestrel-trainer", "Kestrel trainer aircraft (Skyline Aces)", &none), Some(Category::Plane));
        assert_eq!(categorize(Some(AssetKind::Character), "sandbox/models/kart/char-kart-zippy", "Kart zippy (Turbo Tots Grand Prix)", &none), Some(Category::Person));
        assert_eq!(categorize(Some(AssetKind::Character), "kenney/mini-characters/character-female-a", "character-female-a", &none), Some(Category::Person));
        assert_eq!(categorize(Some(AssetKind::Character), "sandbox/models/frontier/char-fr-goblin", "Goblin (Frontier)", &none), None);
        assert_eq!(categorize(Some(AssetKind::Character), "quake/quake/characters/shambler", "shambler", &none), None);
        assert_eq!(categorize(Some(AssetKind::Prop), "kenney/holiday-kit/gingerbread-man", "gingerbread-man", &none), None);
        assert_eq!(categorize(Some(AssetKind::Mesh), "kenney/car-kit/sedan", "sedan", &none), Some(Category::Car));
        // Places and parts are not the thing.
        assert_eq!(categorize(Some(AssetKind::Mesh), "kenney/city-kit/bus-stop", "bus stop", &none), None);
        assert_eq!(categorize(Some(AssetKind::Prop), "sandbox/models/citydrive/vd-hubcap", "Hubcap (Veloria Drive)", &none), None);
        assert_eq!(categorize(Some(AssetKind::Prop), "kenney/nature-kit/tree-oak", "tree oak", &none), Some(Category::Tree));
        assert_eq!(categorize(Some(AssetKind::Prop), "kenney/nature-kit/rock-large", "rock", &none), None);
        // A police station is a building; a pack's theme tags say nothing.
        assert_eq!(categorize(Some(AssetKind::Prop), "gen/csg/police-station", "Police station", &none), Some(Category::Building));
        let themed = ["building".to_string(), "blaster".to_string()];
        assert_eq!(categorize(Some(AssetKind::Prop), "kenney/brick-kit/round-hq-brick-1x1", "round-hq-brick-1x1", &themed), None);
        assert_eq!(categorize(Some(AssetKind::Vehicle), "sandbox/models/x/stormhawk-blue", "Stormhawk blue", &["plane".to_string()]), Some(Category::Plane));
    }

    #[test]
    fn calibration_reads_the_metadata() {
        assert_eq!(Calibration::of(Some("makepad-model"), 1.0, None), Calibration::Metric);
        assert_eq!(Calibration::of(None, 1.0, Some(1.7647)), Calibration::Stated(1.7647));
        assert_eq!(Calibration::of(None, 0.4, None), Calibration::Stated(2.5));
        assert_eq!(Calibration::of(None, 1.0, None), Calibration::Unknown);
        assert_eq!(Calibration::of(None, 1.0, Some(1.0)), Calibration::Unknown, "an uncalibrated pack states nothing");
        // A unitless publish with makepad-model's name but other units is
        // what its units say.
        assert_eq!(Calibration::of(Some("makepad-model"), 0.5, None), Calibration::Stated(2.0));
    }

    #[test]
    fn the_rule_draws_each_thing_at_its_real_size() {
        // A metric GT3 race car is its own 4.7 m.
        let gt3 = real_size(Calibration::Metric, Some(Category::Car), [2.0, 1.2, 4.7]);
        assert_eq!((gt3.scale, gt3.basis), (1.0, Basis::Calibrated));
        // An older entry that never said its units, 4.7 long: metres.
        let old = real_size(Calibration::Unknown, Some(Category::Car), [2.0, 1.2, 4.7]);
        assert_eq!((old.scale, old.basis), (1.0, Basis::AuthoredMetres));
        // A unitless toy car 0.5 long: fitted to a car's 4.5 m.
        let toy = real_size(Calibration::Unknown, Some(Category::Car), [0.2, 0.15, 0.5]);
        assert!(near(toy.scale * 0.5, 4.5) && toy.basis == Basis::Estimated);
        // A comic kart authored 2.94 m long in its own game: a 2.4 m kart
        // elsewhere (the nearest real one).
        let kart = real_size(Calibration::Metric, Some(Category::Kart), [1.99, 2.1, 2.94]);
        assert!(near(kart.scale * 2.94, 2.4) && kart.basis == Basis::Corrected);
        // A game's toy race car, 1.33 m: at least a small real car.
        let toy_racer = real_size(Calibration::Metric, Some(Category::Car), [0.89, 0.45, 1.33]);
        assert!(near(toy_racer.scale * 1.33, 2.5));
        // A pack's comic kart, calibrated 1.7647 m a unit, 1.43 units long.
        assert!(near(real_size(Calibration::Stated(1.7647), Some(Category::Kart), [0.9, 0.8, 1.43]).extent([0.9, 0.8, 1.43])[2], 2.4));
        // A coaster car is no locomotive: rail vehicles keep their size.
        assert_eq!(real_size(Calibration::Stated(1.8), Some(Category::Train), [0.8, 0.7, 1.3]).scale, 1.8);
        // A game's 1.27 m palm stays its own size: scenery is not corrected.
        assert_eq!(real_size(Calibration::Metric, Some(Category::Tree), [2.2, 1.27, 2.1]).scale, 1.0);
        // A rig in unstated units is its own size: a classic boss monster.
        assert_eq!(real_size(Calibration::Unknown, Some(Category::Person), [10.4, 8.3, 7.5]).scale, 1.0);
        // A Kenney car calibrated 1.7647 m per unit, 2.55 units: 4.5 m.
        let kenney = real_size(Calibration::Stated(1.7647), Some(Category::Car), [1.1, 0.9, 2.55]);
        assert!(near(kenney.extent([1.1, 0.9, 2.55])[2], 4.5) && kenney.basis == Basis::Calibrated);
        // A 12.36 m bus is a bus.
        assert_eq!(real_size(Calibration::Metric, Some(Category::Bus), [3.05, 3.27, 12.36]).basis, Basis::Calibrated);
        // A Kenney mini-character (2.5 m per unit, 0.7 tall): 1.75 m.
        let mini = real_size(Calibration::Stated(2.5), Some(Category::Person), [0.78, 0.7, 0.4]);
        assert!(near(mini.scale * 0.7, 1.75));
        // A light plane's 10.4 m span.
        assert_eq!(real_size(Calibration::Metric, Some(Category::Plane), [10.4, 1.9, 8.5]).scale, 1.0);
        // No category, sensible units: as authored; a 300 unit thing: 1.5 m.
        assert_eq!(real_size(Calibration::Unknown, None, [1.0, 1.0, 1.0]).scale, 1.0);
        assert!(near(real_size(Calibration::Unknown, None, [300.0, 150.0, 300.0]).scale * 300.0, 1.5));
    }

    #[test]
    fn the_check_flags_what_left_its_range() {
        assert_eq!(check(Some(Category::Car), [1.9, 1.2, 4.5], 1.0), None);
        assert!(check(Some(Category::Car), [1.9, 1.2, 4.5], 0.4).is_some());
        assert!(check(Some(Category::Person), [0.8, 0.7, 0.4], 1.0).is_some());
        assert_eq!(check(None, [100.0; 3], 1.0), None);
        assert!(near(off_factor(Some(Category::Car), [1.0, 1.0, 1.25], 1.0), 2.0));
        // Every range is ordered and its typical value inside it.
        for c in Category::ALL {
            let r = c.range();
            assert!(r.lo < r.hi && r.contains(r.typical), "{c:?}");
            assert_eq!(Category::parse(c.as_str()), Some(c));
        }
    }
}

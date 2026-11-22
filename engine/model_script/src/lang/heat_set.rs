//! Heat-set inserts by thread: brass inserts a soldering iron melts into a printed hole after the
//! print. Generic metric sizes, no maker's: the hole a part leaves for one is the script's to
//! change (`heat_insert(.., {hole depth taper lead_in})`), these are its defaults.

/// One size, in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeatSet {
    /// `M3`, as a person names it.
    pub name: &'static str,
    /// The thread it takes and its ISO coarse pitch.
    pub diameter: f64,
    pub pitch: f64,
    /// Across the knurl's crests, its length, and the plain collar at its leading end.
    pub outer: f64,
    pub length: f64,
    pub collar: f64,
    /// The printed hole: across at its mouth and deep (its length and room for the melt pushed
    /// ahead of it).
    pub hole: f64,
    pub depth: f64,
    /// The part it wants round the hole and under it.
    pub wall: f64,
    pub floor: f64,
}

pub const HEAT_SETS: &[HeatSet] = &[
    HeatSet { name: "M2", diameter: 2.0, pitch: 0.4, outer: 3.6, length: 3.0, collar: 0.6, hole: 3.2, depth: 4.0, wall: 1.6, floor: 1.0 },
    HeatSet { name: "M2.5", diameter: 2.5, pitch: 0.45, outer: 4.2, length: 4.0, collar: 0.7, hole: 3.6, depth: 5.0, wall: 1.8, floor: 1.0 },
    HeatSet { name: "M3", diameter: 3.0, pitch: 0.5, outer: 4.6, length: 4.0, collar: 0.8, hole: 4.0, depth: 5.0, wall: 2.0, floor: 1.2 },
    HeatSet { name: "M4", diameter: 4.0, pitch: 0.7, outer: 6.3, length: 6.0, collar: 1.0, hole: 5.6, depth: 7.0, wall: 2.6, floor: 1.5 },
    HeatSet { name: "M5", diameter: 5.0, pitch: 0.8, outer: 7.1, length: 7.0, collar: 1.2, hole: 6.4, depth: 8.0, wall: 3.0, floor: 1.8 },
];

/// The size of a thread diameter (mm).
pub fn by_diameter(d: f64) -> Option<&'static HeatSet> {
    HEAT_SETS.iter().find(|s| (s.diameter - d).abs() < 1e-6)
}

/// The size a person names: `M3`, `M2.5` or `M2_5`.
pub fn by_name(name: &str) -> Option<&'static HeatSet> {
    name.strip_prefix('M').and_then(|d| d.replace('_', ".").parse().ok()).and_then(by_diameter)
}

/// Every size's name, for an error.
pub fn names() -> Vec<&'static str> {
    HEAT_SETS.iter().map(|s| s.name).collect()
}

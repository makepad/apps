//! Machine screws a printed case takes, by thread: the clearance hole a screw passes through, the
//! pilot a self-tapping screw cuts its own thread in, its countersunk head, and the boss a printed
//! part keeps round a pilot. Generic metric sizes, no maker's. The hole and boss a heat-set insert
//! of the same thread wants come from `heat_set`; a case asks for a fixing by size or by the drill
//! a board gives it ([`for_drill`]), so every board picks its fixings from this one table.

use super::heat_set::{self, HeatSet};

/// One size, in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screw {
    /// `M3`, as a person names it.
    pub name: &'static str,
    pub diameter: f64,
    /// The hole it passes through freely (ISO 273 medium).
    pub clear: f64,
    /// The pilot a self-tapping screw cuts its thread into printed plastic, and the boss round it.
    pub pilot: f64,
    pub boss: f64,
    /// Its countersunk head across (90 degrees).
    pub head: f64,
}

pub const SCREWS: &[Screw] = &[
    Screw { name: "M2", diameter: 2.0, clear: 2.4, pilot: 1.6, boss: 5.0, head: 3.8 },
    Screw { name: "M2.5", diameter: 2.5, clear: 2.9, pilot: 2.1, boss: 5.5, head: 4.7 },
    Screw { name: "M3", diameter: 3.0, clear: 3.4, pilot: 2.5, boss: 6.0, head: 5.6 },
];

/// How much narrower than its drill a screw's thread is at the least, to pass through it.
pub const PASS: f64 = 0.1;

impl Screw {
    /// The heat-set insert of this thread.
    pub fn heat_set(&self) -> &'static HeatSet {
        heat_set::by_diameter(self.diameter).expect("every screw size has its heat-set insert")
    }
    /// The boss a heat-set insert of this thread wants: its hole and its wall each side.
    pub fn heat_boss(&self) -> f64 {
        let h = self.heat_set();
        h.hole + 2.0 * h.wall
    }
}

/// The size of a thread diameter (mm).
pub fn by_diameter(d: f64) -> Option<&'static Screw> {
    SCREWS.iter().find(|s| (s.diameter - d).abs() < 1e-6)
}

/// The size a person names: `M3`, `M2.5` or `M2_5`.
pub fn by_name(name: &str) -> Option<&'static Screw> {
    name.strip_prefix('M').and_then(|d| d.replace('_', ".").parse().ok()).and_then(by_diameter)
}

/// The largest screw whose thread passes a drill (mm) with [`PASS`] to spare; none fits a drill
/// under the smallest's.
pub fn for_drill(drill: f64) -> Option<&'static Screw> {
    SCREWS.iter().rev().find(|s| s.diameter + PASS <= drill + 1e-9)
}

/// Every size's name, for an error.
pub fn names() -> Vec<&'static str> {
    SCREWS.iter().map(|s| s.name).collect()
}

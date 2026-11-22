//! Findings of every check (ERC, DRC, mechanical, system ERC) in one form: the Checks list
//! (E4 A16).

use crate::Quantity;
use fab_elec_core::{canon_enum, canon_struct, BoardId, Canon, Nm, Sel, Severity, Writer, P};

/// One finding.
#[derive(Clone, PartialEq, Debug)]
pub struct Violation {
    /// Stable across runs: a hash of kind, sorted items and rule (`Violation::stable_id`), so a
    /// waiver or an incremental merge finds it again.
    pub id: u64,
    pub unit: Option<BoardId>,
    pub kind: CheckKind,
    pub severity: Severity,
    /// Where to show it (board frame for board checks; `None` for circuit-only findings).
    pub at: Option<P>,
    /// What it is about, sorted.
    pub items: Vec<Sel>,
    pub actual: Option<Measure>,
    pub required: Option<Measure>,
    /// The rule that asked (`clearance`, `power:track`, a custom rule's name).
    pub rule: String,
    pub message: String,
    pub fix_hint: String,
}
canon_struct!(Violation { id, unit, kind, severity, at, items, actual, required, rule, message, fix_hint });

/// A measured or required amount: a length, or an electrical quantity (volts, amperes).
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Measure {
    Len(Nm),
    Qty(Quantity),
}
canon_enum!(Measure { 0 => Len(v), 1 => Qty(q) });

/// What kind of finding.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum CheckKind {
    // ERC (E1 4.2)
    PinConflict,
    UndrivenInput,
    PowerNotDriven,
    UnconnectedPin,
    SinglePinNet,
    DuplicateRefdes,
    MissingFootprint,
    VoltageDomain,
    RailBudget,
    // DRC (E1 4.3)
    Clearance,
    EdgeClearance,
    HoleClearance,
    HoleToHole,
    TrackWidth,
    ViaSize,
    AnnularRing,
    Drill,
    CourtyardOverlap,
    SilkOverPad,
    SilkClipped,
    MaskSliver,
    TextSize,
    Unconnected,
    Short,
    Dangling,
    /// A via connected on fewer than two copper layers.
    DanglingVia,
    /// A via drill nearer a placed pad's copper than the pad's mask opening plus a minimum mask
    /// web (`RuleMatrix::via_drill_to_pad`), where the via is not filled and capped.
    ViaInPad,
    ZoneIsland,
    /// Two parts of one pour joined only through a passage narrower than its minimum width.
    PourNeck,
    Keepout,
    PairGap,
    PairUncoupled,
    Length,
    Skew,
    FootprintDrift,
    /// Copper on a layer the board's stackup does not have, such as a track left on a dropped layer.
    MissingLayer,
    // mechanical and system (E2.13)
    Mech,
    LinkPinMap,
    LinkMismatch,
    SystemVolts,
    SystemSource,
    /// A user `check` fn, by name.
    Custom(String),
    /// A drag laid a corner sharper than a right angle with no room to chamfer it.
    /// A session note on that edit, never a DRC result.
    SharpCorner,
}

impl Canon for CheckKind {
    fn canon(&self, w: &mut Writer) {
        match self {
            CheckKind::Custom(s) => {
                w.tag(255);
                s.canon(w)
            }
            k => w.str(&format!("{k:?}")),
        }
    }
}

impl Violation {
    /// The stable id of a finding: kind, sorted items and rule, nothing positional.
    pub fn stable_id(kind: &CheckKind, items: &[Sel], rule: &str) -> u64 {
        let mut w = Writer::hasher();
        kind.canon(&mut w);
        let mut sorted: Vec<&Sel> = items.iter().collect();
        sorted.sort();
        w.count(sorted.len());
        sorted.iter().for_each(|s| s.canon(&mut w));
        rule.canon(&mut w);
        let h = w.finish();
        u64::from_le_bytes(h.0[..8].try_into().unwrap())
    }
    /// A finding with its stable id and sorted items; measures and texts default empty.
    pub fn new(kind: CheckKind, severity: Severity, unit: Option<BoardId>, mut items: Vec<Sel>, rule: &str, message: String) -> Violation {
        items.sort();
        let id = Violation::stable_id(&kind, &items, rule);
        Violation { id, unit, kind, severity, at: None, items, actual: None, required: None, rule: rule.into(), message, fix_hint: String::new() }
    }
    pub fn at(mut self, p: P) -> Violation {
        self.at = Some(p);
        self
    }
    pub fn measures(mut self, actual: Measure, required: Measure) -> Violation {
        self.actual = Some(actual);
        self.required = Some(required);
        self
    }
    pub fn hint(mut self, h: impl Into<String>) -> Violation {
        self.fix_hint = h.into();
        self
    }
}

//! Custom rule files: retained syntax, bounded parsing, and class/kind predicates.

mod expr;
mod mapping;
mod parse;
#[cfg(test)]
mod tests;

pub use expr::{BinaryOp, Expr, Item, UnaryOp, Unit};
pub use mapping::KicadRuleSource;
pub use parse::parse_kicad_dru;
pub(crate) use parse::record_variables;

use crate::report::ImportReport;
use fab_elec_core::LayerSet;

const MAX_DEPTH: usize = 128;

#[derive(Clone, Debug)]
pub struct RuleFile {
    pub version: i64,
    pub too_recent: bool,
    pub rules: Vec<Rule>,
    pub report: ImportReport,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub name: String,
    pub location: (u32, u32),
    pub offset: usize,
    /// Canonical layer identities, before intersection with the board's copper.
    pub layers: Option<LayerSet>,
    pub layer_pattern: Option<String>,
    pub severity: Option<RuleSeverity>,
    pub condition: Option<Expr>,
    pub condition_text: Option<String>,
    pub constraints: Vec<Constraint>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RuleSeverity { Ignore, Warning, Error, Exclusion }

macro_rules! constraint_kinds {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        pub enum ConstraintKind { $($variant),+ }
        impl ConstraintKind {
            pub fn name(self) -> &'static str {
                match self { $(Self::$variant => $name),+ }
            }
            pub(super) fn parse(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)+
                    "mechanical_clearance" => Some(Self::PhysicalClearance),
                    "mechanical_hole_clearance" => Some(Self::PhysicalHoleClearance),
                    "hole" => Some(Self::HoleSize),
                    "text_thickness" => Some(Self::LineWidth),
                    _ => None,
                }
            }
        }
    }
}

constraint_kinds! {
    Clearance => "clearance", Length => "length", Creepage => "creepage",
    TrackWidth => "track_width", DiffPairGap => "diff_pair_gap", SilkClearance => "silk_clearance",
    HoleClearance => "hole_clearance", Skew => "skew", DiffPairUncoupled => "diff_pair_uncoupled",
    EdgeClearance => "edge_clearance", CourtyardClearance => "courtyard_clearance",
    Assertion => "assertion", HoleToHole => "hole_to_hole", Disallow => "disallow",
    ViaDiameter => "via_diameter", HoleSize => "hole_size", PhysicalClearance => "physical_clearance",
    PhysicalHoleClearance => "physical_hole_clearance", ConnectionWidth => "connection_width",
    ViaDangling => "via_dangling", MicroviaStackDepth => "microvia_stack_depth", ViaCount => "via_count",
    MinResolvedSpokes => "min_resolved_spokes", BridgedMask => "bridged_mask",
    HoleAspectRatio => "hole_aspect_ratio", TrackAngle => "track_angle", AnnularWidth => "annular_width",
    LineWidth => "line_width", TextHeight => "text_height", TrackSegmentLength => "track_segment_length",
    ThermalReliefGap => "thermal_relief_gap", ThermalSpokeWidth => "thermal_spoke_width",
    SolderMaskExpansion => "solder_mask_expansion", SolderMaskSliver => "solder_mask_sliver",
    SolderPasteAbsMargin => "solder_paste_abs_margin", SolderPasteRelMargin => "solder_paste_rel_margin",
    ZoneConnection => "zone_connection", NetChainLength => "net_chain_length", StubLength => "stub_length",
    ReturnPath => "return_path",
}

impl ConstraintKind {
    fn time_allowed(self) -> bool {
        matches!(self, Self::Length | Self::Skew | Self::NetChainLength | Self::StubLength)
    }
    fn count(self) -> bool {
        matches!(self, Self::ViaDangling | Self::MicroviaStackDepth | Self::ViaCount | Self::MinResolvedSpokes | Self::BridgedMask)
    }
    fn ratio(self) -> bool { matches!(self, Self::HoleAspectRatio | Self::SolderPasteRelMargin) }
    /// Constraints evaluated for one item at a time; a condition naming B is an error with them.
    fn single_item(self) -> bool {
        matches!(self, Self::HoleSize | Self::TextHeight | Self::LineWidth | Self::TrackSegmentLength
            | Self::AnnularWidth | Self::SolderMaskExpansion | Self::SolderPasteAbsMargin
            | Self::SolderPasteRelMargin | Self::Disallow | Self::ViaDiameter | Self::Length
            | Self::NetChainLength | Self::StubLength | Self::ReturnPath | Self::Skew | Self::ViaCount
            | Self::Assertion | Self::ViaDangling | Self::BridgedMask | Self::MicroviaStackDepth
            | Self::HoleAspectRatio)
    }
    /// Single-item constraints that still accept a condition on the pair (`AB.`) without B.
    fn pair_allowed(self) -> bool { matches!(self, Self::Length | Self::NetChainLength | Self::Skew) }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Constraint {
    pub kind: ConstraintKind,
    pub location: (u32, u32),
    pub min: Option<i64>,
    pub opt: Option<i64>,
    pub max: Option<i64>,
    pub in_time: bool,
    pub within_diff_pairs: bool,
    pub disallow: DisallowKinds,
    pub zone_connection: Option<ZoneConnection>,
    pub assertion: Option<Expr>,
    pub return_path_layer: Option<String>,
    pub return_path_net: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ZoneConnection { Solid, ThermalReliefs, None }

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct DisallowKinds(pub u16);

impl DisallowKinds {
    pub const TRACK: Self = Self(1);
    pub const THROUGH_VIA: Self = Self(1 << 1);
    pub const BLIND_VIA: Self = Self(1 << 2);
    pub const BURIED_VIA: Self = Self(1 << 3);
    pub const MICRO_VIA: Self = Self(1 << 4);
    pub const VIA: Self = Self(Self::THROUGH_VIA.0 | Self::BLIND_VIA.0 | Self::BURIED_VIA.0 | Self::MICRO_VIA.0);
    pub const PAD: Self = Self(1 << 5);
    pub const ZONE: Self = Self(1 << 6);
    pub const TEXT: Self = Self(1 << 7);
    pub const GRAPHIC: Self = Self(1 << 8);
    pub const HOLE: Self = Self(1 << 9);
    pub const FOOTPRINT: Self = Self(1 << 10);
    pub fn contains(self, kinds: Self) -> bool { self.0 & kinds.0 == kinds.0 }
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "track" => Self::TRACK, "via" => Self::VIA, "through_via" => Self::THROUGH_VIA,
            "blind_via" => Self::BLIND_VIA, "buried_via" => Self::BURIED_VIA, "micro_via" => Self::MICRO_VIA,
            "pad" => Self::PAD, "zone" => Self::ZONE, "text" => Self::TEXT, "graphic" => Self::GRAPHIC,
            "hole" => Self::HOLE, "footprint" => Self::FOOTPRINT, _ => return None,
        })
    }
}

/// Case-sensitive Unicode wildcard matching; `?` consumes one character.
pub fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<_> = pattern.chars().collect();
    let text: Vec<_> = text.chars().collect();
    let (mut p, mut t, mut star) = (0, 0, None);
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => { star = Some((p, t)); p += 1; }
            Some(c) if *c == '?' || *c == text[t] => { p += 1; t += 1; }
            _ => match star {
                Some((s, n)) if n < text.len() => { star = Some((s, n + 1)); p = s + 1; t = n + 1; }
                _ => return false,
            },
        }
    }
    while pattern.get(p) == Some(&'*') { p += 1; }
    p == pattern.len()
}

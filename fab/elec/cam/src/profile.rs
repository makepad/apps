//! The resolved factory profile: plain data the writers take (seam of lane ECAM).
//!
//! A profile document (Splash, evaluated by `lang`) becomes this: naming, format variants,
//! capability limits and the BOM and placement tables with their Splash functions already
//! applied to the design. `cam` never calls the VM.

use crate::drill::Zeros;
use fab_elec_core::{Angle, LayerId, Nm, Side, P};

/// What the factory can make (moved to the board crate: rule minima are a board concept).
pub use fab_elec_board::Capabilities;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum GerberVariant {
    /// Layer files with attributes in `G04 #@! ` comments (S46.R48).
    X1,
    /// With file, aperture and object attributes.
    X2,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FileKind {
    Gerber { layer: LayerId, variant: GerberVariant },
    /// `plated`: `Some(true)` plated holes only, `Some(false)` non-plated only, `None` both in one file.
    Drill { plated: Option<bool> },
    Bom,
    Placement,
    Readme,
}

/// One file of the package. `name` is final (the profile's template with `{project}` and
/// `{rev}` filled).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileSpec {
    pub name: String,
    pub kind: FileKind,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GerberFormat {
    /// Fraction digits of coordinates, 5 or 6; other values are refused on export (S46.R34).
    pub decimals: u8,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DrillFormat {
    pub inch: bool,
    pub zeros: Zeros,
    pub int_digits: u8,
    pub frac_digits: u8,
}

/// One BOM line: the cells as the profile's column functions made them, and the references
/// the line stands for (what the validator checks against the circuit).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BomRow {
    pub cells: Vec<String>,
    pub refs: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BomTable {
    pub header: Vec<String>,
    pub rows: Vec<BomRow>,
    pub delimiter: char,
}

/// One placement line. `at`, `rot` and `side` are the values the cells were made from (board
/// frame shifted to the profile's origin, rotation after `rot_offset`); the validator checks
/// them against the model's poses.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlaceRow {
    pub cells: Vec<String>,
    pub refdes: String,
    pub at: P,
    pub rot: Angle,
    /// The factory-specific correction added to the pose's rotation.
    pub rot_offset: Angle,
    pub side: Side,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlacementTable {
    /// The profile's origin in board coordinates: rows are relative to it.
    pub origin: P,
    /// Bottom X convention, applied after subtracting the origin; None leaves X unchanged.
    pub bottom_x: Option<crate::tables::BottomX>,
    pub header: Vec<String>,
    pub rows: Vec<PlaceRow>,
    pub delimiter: char,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ResolvedProfile {
    pub name: String,
    pub project: String,
    pub rev: String,
    pub capabilities: Capabilities,
    pub files: Vec<FileSpec>,
    pub gerber: GerberFormat,
    pub drill: DrillFormat,
    pub bom: BomTable,
    pub placement: PlacementTable,
    pub readme: String,
    /// The package name (`{project}-{rev}.zip` filled), `None` for loose files.
    pub zip: Option<String>,
    /// How far a read-back file may differ from the model, in nm (see `validate`).
    pub tolerance: Nm,
}

/// The built-in factory profiles, by the name a board's `factory(..)` and `elec.factory.set`
/// take, in the order they are offered.
pub const PROFILES: [&str; 2] = ["standard 2-layer", "JLCPCB 2-layer"];

/// The built-in profiles' capabilities by name: the factories whose minima become a board's rules
/// when its document names one (`apply_capabilities`). O(profiles).
pub fn builtin_factories() -> fab_elec_board::Factories {
    PROFILES.iter().filter_map(|name| ResolvedProfile::builtin(name, "", "")).map(|p| (p.name, std::sync::Arc::new(p.capabilities))).collect()
}

impl ResolvedProfile {
    /// A built-in profile by its name in [`PROFILES`] (letter case aside), for `project` at
    /// `rev`; None for any other name. Its tables start empty (`tables::fill_generic_tables`).
    pub fn builtin(name: &str, project: &str, rev: &str) -> Option<ResolvedProfile> {
        let name = name.trim();
        if name.eq_ignore_ascii_case(PROFILES[0]) {
            Some(Self::standard_2_layer(project, rev))
        } else if name.eq_ignore_ascii_case(PROFILES[1]) {
            Some(Self::jlc_2_layer(project, rev))
        } else {
            None
        }
    }

    /// Standard two-layer fabrication files with JLC assembly headers. Fill the empty tables
    /// with `PlacementRules::jlc()` so placement coordinates share the Gerber frame.
    pub fn jlc_2_layer(project: &str, rev: &str) -> ResolvedProfile {
        let mut profile = Self::standard_2_layer(project, rev);
        profile.name = "JLCPCB 2-layer".into();
        profile.bom.header = ["Comment", "Designator", "Footprint", "LCSC Part #"]
            .iter().map(|s| s.to_string()).collect();
        profile.placement.header = ["Designator", "Mid X", "Mid Y", "Layer", "Rotation"]
            .iter().map(|s| s.to_string()).collect();
        profile.readme = format!("{project} revision {rev}: fabrication and assembly files for the JLCPCB 2-layer profile.\n");
        profile
    }

    /// The built-in generic profile "standard 2-layer": copper, mask, paste, silk and edge
    /// layers of both sides as X2 Gerber, plated and non-plated drills, BOM and placement as
    /// CSV, a readme, one zip. Tables start empty; the caller fills them from the design.
    pub fn standard_2_layer(project: &str, rev: &str) -> ResolvedProfile {
        let g = |layer: LayerId, suffix: &str| FileSpec {
            name: format!("{project}-{suffix}.gbr"),
            kind: FileKind::Gerber { layer, variant: GerberVariant::X2 },
        };
        let files = vec![
            g(LayerId::F_CU, "F_Cu"),
            g(LayerId::B_CU, "B_Cu"),
            g(LayerId::F_MASK, "F_Mask"),
            g(LayerId::B_MASK, "B_Mask"),
            g(LayerId::F_PASTE, "F_Paste"),
            g(LayerId::B_PASTE, "B_Paste"),
            g(LayerId::F_SILK, "F_Silk"),
            g(LayerId::B_SILK, "B_Silk"),
            g(LayerId::EDGE, "Edge"),
            FileSpec { name: format!("{project}-PTH.drl"), kind: FileKind::Drill { plated: Some(true) } },
            FileSpec { name: format!("{project}-NPTH.drl"), kind: FileKind::Drill { plated: Some(false) } },
            FileSpec { name: format!("{project}-bom.csv"), kind: FileKind::Bom },
            FileSpec { name: format!("{project}-pos.csv"), kind: FileKind::Placement },
            FileSpec { name: "README.txt".into(), kind: FileKind::Readme },
        ];
        ResolvedProfile {
            name: "standard 2-layer".into(),
            project: project.into(),
            rev: rev.into(),
            capabilities: Capabilities {
                min_track: 150_000,
                min_clearance: 150_000,
                min_drill: 300_000,
                min_annular: 130_000,
                via_in_pad: false,
                layers: vec![1, 2],
                board_min: (5_000_000, 5_000_000),
                board_max: (400_000_000, 500_000_000),
                thickness: vec![800_000, 1_000_000, 1_600_000],
                thickness_tol_ppm: 50_000,
            },
            files,
            gerber: GerberFormat { decimals: 6 },
            drill: DrillFormat { inch: false, zeros: Zeros::Decimal, int_digits: 3, frac_digits: 3 },
            bom: BomTable { header: vec!["Comment".into(), "Designator".into(), "Footprint".into(), "Part".into()], rows: vec![], delimiter: ',' },
            placement: PlacementTable {
                origin: P::ZERO,
                bottom_x: None,
                header: ["Designator", "Mid X", "Mid Y", "Layer", "Rotation"].iter().map(|s| s.to_string()).collect(),
                rows: vec![],
                delimiter: ',',
            },
            readme: format!("{project} revision {rev}: fabrication files for the standard 2-layer profile.\n"),
            zip: Some(format!("{project}-{rev}.zip")),
            tolerance: 1_000,
        }
    }
}

/// A file as written.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OutFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// What `validate` found: one entry per check, each with its cause and a fix hint when it failed.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Validation {
    pub checks: Vec<Check>,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CheckKind {
    FileSet,
    Syntax,
    Attributes,
    Geometry,
    Drill,
    Placement,
    Bom,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Check {
    pub file: String,
    pub kind: CheckKind,
    pub ok: bool,
    /// What was compared when it passed, what differs when it failed.
    pub cause: String,
    /// What to change when it failed (empty when it passed).
    pub hint: String,
    /// Where on the board it failed (the middle of the difference, the hole, the part), board frame.
    pub at: Option<P>,
    /// The fill-lattice tile the failure is in (when the model was tiled).
    pub tile: Option<(i32, i32)>,
}

impl Validation {
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }
    pub fn failures(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| !c.ok)
    }
}

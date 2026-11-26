//! The resolved factory profile: plain data the writers take (seam of lane ECAM).
//!
//! A literal factory profile selects a file set and supplies the capability limits. The resolved
//! profile holds the names, format variants,
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

/// The names of the Rust export formats (file sets) a profile document may select.
pub const PROFILES: [&str; 2] = ["standard 2-layer", "JLCPCB 2-layer"];

/// The built-in factory minima used by the board's effective rules.
pub fn builtin_factories() -> fab_elec_board::Factories {
    crate::profile_doc::builtins().iter().map(|p| (p.name.clone(), p.caps.clone())).collect()
}

impl ResolvedProfile {
    /// A built-in profile by name, ignoring letter case. Its tables start empty.
    pub fn builtin(name: &str, project: &str, rev: &str) -> Option<Self> {
        let def = crate::profile_doc::builtins().iter().find(|p| p.name.eq_ignore_ascii_case(name.trim()))?;
        Some(Self::from_definition(def, project, rev))
    }

    pub fn jlc_2_layer(project: &str, rev: &str) -> Self {
        Self::builtin(PROFILES[1], project, rev).expect("the built-in JLCPCB profile")
    }

    pub fn standard_2_layer(project: &str, rev: &str) -> Self {
        Self::builtin(PROFILES[0], project, rev).expect("the built-in standard profile")
    }

    /// Build the named factory's package using its selected file set.
    pub fn from_definition(def: &crate::profile_doc::ProfileDef, project: &str, rev: &str) -> Self {
        let jlc = def.format == PROFILES[1];
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
            name: def.name.clone(),
            project: project.into(),
            rev: rev.into(),
            capabilities: (*def.caps).clone(),
            files,
            gerber: GerberFormat { decimals: 6 },
            drill: DrillFormat { inch: false, zeros: Zeros::Decimal, int_digits: 3, frac_digits: 3 },
            bom: BomTable { header: vec!["Comment".into(), "Designator".into(), "Footprint".into(), (if jlc { "LCSC Part #" } else { "Part" }).into()], rows: vec![], delimiter: ',' },
            placement: PlacementTable {
                origin: P::ZERO,
                bottom_x: None,
                header: ["Designator", "Mid X", "Mid Y", "Layer", "Rotation"].iter().map(|s| s.to_string()).collect(),
                rows: vec![],
                delimiter: ',',
            },
            readme: format!("{project} revision {rev}: {} files for the {} profile.\n", if jlc { "fabrication and assembly" } else { "fabrication" }, def.name),
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

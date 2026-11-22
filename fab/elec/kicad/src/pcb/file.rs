//! The source board's structure, in nanometres and Y-up coordinates, before net resolution.

use crate::report::ImportReport;
use fab_elec_board::{Finish, Flash, LayerRole, StackLayer, Tent, ViaKind, ZoneSettings};
use fab_elec_core::{Angle, LayerId, LayerSet, Nm, Path, PolySet, Side, P};
use fab_elec_parts::{Footprint, Forbid, Gfx, PadNo, PinKind};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// One-based line and byte column of an item's keyword.
pub type Location = (u32, u32);

/// A model value together with the keyword that introduced it.
#[derive(Clone, Debug)]
pub struct Located<T> { pub at: Location, pub value: T }

/// The header is also copied into the report for standalone reporting.
#[derive(Clone, Debug)]
pub struct VersionInfo {
    pub version: i64,
    pub generator: Option<String>,
    pub generator_version: Option<String>,
    pub too_recent: bool,
}

/// A layer row. Unrepresentable layers have no identity but retain their source name.
#[derive(Clone, Debug)]
pub struct LayerEntry {
    pub location: Location,
    pub name: String,
    pub user_name: Option<String>,
    pub id: Option<LayerId>,
    pub copper: bool,
    pub role: LayerRole,
}

/// Canonical and file-specific names, and the leading copper rows in physical stack order.
#[derive(Clone, Debug, Default)]
pub struct LayerTable {
    pub location: Location,
    pub entries: Vec<LayerEntry>,
    pub names: HashMap<String, LayerId>,
    pub beyond: HashSet<String>,
    pub copper_count: usize,
    pub copper: Vec<LayerId>,
    pub roles: HashMap<LayerId, LayerRole>,
}
impl LayerTable {
    pub fn copper_set(&self) -> LayerSet { LayerSet::of(&self.copper) }
    pub fn role(&self, id: LayerId) -> LayerRole {
        self.roles.get(&id).copied().unwrap_or_default()
    }
}

/// Only supplied setup values are set; defaults belong to the build stage.
#[derive(Clone, Debug, Default)]
pub struct Setup {
    pub legacy_rules: bool,
    pub edge_clearance: Option<Nm>,
    pub location: Option<Location>,
    pub mask_margin: Option<Nm>,
    pub paste_margin: Option<Nm>,
    pub paste_ratio_ppm: Option<i32>,
    pub track: Option<Nm>,
    pub clearance: Option<Nm>,
    pub annular: Option<Nm>,
    pub drill: Option<Nm>,
    pub hole_to_hole: Option<Nm>,
    pub trace_clearance: Option<Nm>,
    pub via_size: Option<Nm>,
    pub via_drill: Option<Nm>,
    pub zone_clearance: Option<Nm>,
    pub tenting: Option<Tent>,
}

/// Physical stack layers and optional board colours/finish, with no synthetic defaults.
#[derive(Clone, Debug, Default)]
pub struct FileStackup {
    pub location: Location,
    pub layers: Vec<Located<StackLayer>>,
    pub finish: Option<Finish>,
    pub mask_color: Option<u32>,
    pub silk_color: Option<u32>,
}

/// A top-level net declaration; codes may be sparse and names are preserved verbatim.
#[derive(Clone, Debug)]
pub struct FileNet { pub location: Location, pub code: i64, pub name: String }

/// An unresolved item net, including a zone's overriding legacy name when present.
#[derive(Clone, Debug, Default)]
pub struct NetRef {
    pub location: Option<Location>,
    pub code: Option<i64>,
    pub name: Option<String>,
    pub zone_name: Option<String>,
}

/// Class dimensions stay separate so the build can inherit missing halves of via/pair rules.
#[derive(Clone, Debug)]
pub struct FileNetClass {
    pub location: Location,
    pub name: String,
    pub description: String,
    pub clearance: Option<Nm>,
    pub trace_width: Option<Nm>,
    pub via_dia: Option<Nm>,
    pub via_drill: Option<Nm>,
    pub diff_pair_width: Option<Nm>,
    pub diff_pair_gap: Option<Nm>,
    pub add_net: Vec<Located<String>>,
}

/// One placement's own data (spec-placement F1..F23); never part of the shared footprint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlacementData {
    /// F1: Reference, Value, Datasheet, Description first, then user fields in file order.
    pub fields: Vec<Field>,
    /// F6: footprint filter patterns in order.
    pub fp_filters: Vec<String>,
    /// F11: the schematic sheet's name (empty for the root) and file.
    pub sheet_name: String,
    pub sheet_file: String,
    /// F12: unit names and their pad numbers in file order.
    pub units: Vec<(String, Vec<PadNo>)>,
    /// F13: class names in file order, without repeats.
    pub classes: Vec<String>,
    /// F23: one entry per footprint pad, parallel to pad_nets.
    pub pins: Vec<Option<PadPin>>,
    /// The footprint's 3D models as KiCad names and places them, in file order (hidden ones too).
    pub models: Vec<Model3d>,
}

/// A footprint's 3D model as KiCad names it and places it in the footprint's own (front, library)
/// frame: KiCad draws it as `T(offset) * Rz(-rotate.z) * Ry(-rotate.y) * Rx(-rotate.x) *
/// S(scale) * p`, with `p` the model's point in millimetres (a `.wrl` unit is 0.1 inch), Y up and Z
/// out of the board.
#[derive(Clone, Debug, PartialEq)]
pub struct Model3d {
    pub at: Location,
    /// The path as written: `${KICAD9_3DMODEL_DIR}/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.wrl`.
    pub path: String,
    /// Millimetres (a file before KiCad 5 writes `at` in inches; the reader converts).
    pub offset: [f64; 3],
    pub scale: [f64; 3],
    /// Degrees, as written.
    pub rotate: [f64; 3],
    /// `(hide)`: KiCad does not draw it.
    pub hidden: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Field { pub name: Arc<str>, pub value: String, pub drawn: bool }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PadPin { pub function: Option<String>, pub kind: Option<PinType>, pub sim: Option<SimHint> }
#[derive(Clone, Debug, PartialEq)]
pub struct PinType { pub kind: PadPinKind, pub no_connect: bool }
#[derive(Clone, Debug, PartialEq)]
pub enum PadPinKind { Known(PinKind), Unknown(String) }
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SimHint { Source, Sink }

/// A finished front-form footprint and its placement; child locations parallel the model lists.
#[derive(Clone, Debug)]
pub struct FileFootprint {
    pub location: Location,
    pub footprint: Footprint,
    pub at: P,
    pub rot: Angle,
    pub side: Side,
    pub locked: bool,
    pub path: String,
    pub refdes: String,
    pub data: PlacementData,
    pub pad_nets: Vec<NetRef>,
    pub pad_locations: Vec<Location>,
    pub graphic_locations: Vec<Location>,
    pub text_locations: Vec<Location>,
    pub keepout_locations: Vec<Location>,
    pub edges: Vec<EdgePiece>,
}

/// One unmerged route segment or arc; a collinear arc has no midpoint.
#[derive(Clone, Debug)]
pub struct TrackPiece {
    pub location: Location,
    pub layer: LayerId,
    pub net: NetRef,
    pub width: Nm,
    pub locked: bool,
    pub start: P,
    pub mid: Option<P>,
    pub end: P,
}

/// A via before net resolution, with defaults and per-layer diameter approximation applied.
#[derive(Clone, Debug)]
pub struct FileVia {
    pub location: Location,
    pub at: P,
    pub size: Nm,
    pub drill: Nm,
    pub span: (LayerId, LayerId),
    pub kind: ViaKind,
    pub tent: Tent,
    pub net: NetRef,
    pub locked: bool,
    /// The span layers that carry a ring, from the via's layer-removal flags.
    pub flash: Flash,
    /// The layers the file lists where a zone connects the via: the writer's cache of its own
    /// fill, compared with ours after a fill (`ZoneFlashList`) and never stored on the board.
    pub zone_layers: Option<LayerSet>,
}

/// A copper pour with polygonised even-odd outlines; stored fills are never retained.
#[derive(Clone, Debug)]
pub struct FileZone {
    pub location: Location,
    pub net: NetRef,
    pub name: String,
    pub layers: LayerSet,
    pub area: PolySet,
    pub priority: u16,
    pub settings: ZoneSettings,
    pub locked: bool,
}

/// A board rule area, or a temporary footprint-local rule area before front-form conversion.
#[derive(Clone, Debug)]
pub struct FileKeepout {
    pub location: Location,
    pub area: PolySet,
    pub layers: LayerSet,
    pub forbid: Forbid,
    pub reason: String,
}

/// One drawing on one layer, without a board item identity.
#[derive(Clone, Debug)]
pub struct FileGraphic { pub location: Location, pub layer: LayerId, pub gfx: Gfx, pub locked: bool }

/// Outline centreline kept apart from graphics, including an independently closed shape.
#[derive(Clone, Debug)]
pub struct EdgePiece {
    pub location: Location,
    pub path: Path,
    pub closed: bool,
    pub width: Nm,
    pub locked: bool,
}

/// A board text with layout properties representable by the board model.
#[derive(Clone, Debug)]
pub struct FileText {
    pub location: Location,
    pub layer: LayerId,
    pub text: String,
    pub at: P,
    pub rot: Angle,
    pub size: Nm,
    pub thick: Nm,
    pub mirrored: bool,
    pub locked: bool,
}

/// Parse output. Lists preserve source order; all points use nm/Y-up, footprints use front form.
#[derive(Clone, Debug)]
pub struct PcbFile {
    pub location: Location,
    pub version: VersionInfo,
    pub layers: LayerTable,
    pub thickness: Option<Located<Nm>>,
    pub setup: Setup,
    pub stackup: Option<FileStackup>,
    pub nets: Vec<FileNet>,
    pub classes: Vec<FileNetClass>,
    pub footprints: Vec<FileFootprint>,
    pub tracks: Vec<TrackPiece>,
    pub vias: Vec<FileVia>,
    pub zones: Vec<FileZone>,
    pub keepouts: Vec<FileKeepout>,
    pub graphics: Vec<FileGraphic>,
    pub edges: Vec<EdgePiece>,
    pub texts: Vec<FileText>,
    pub report: ImportReport,
}

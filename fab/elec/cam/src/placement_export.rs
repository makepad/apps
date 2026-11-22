//! S46.4 placement exports and footprint reports. Like drill_export, this is a
//! pure CAM boundary: the host resolves document text/escapes and supplies the
//! source-board snapshot, variants, physical extents and plot settings. It owns
//! folder expansion, creation and writing; this module returns bytes and paths.
//! Snapshot coordinates are the source document's stored axes (Y down), not
//! pre-negated output coordinates. No editor visibility enters this interface.

use crate::error::CamError;
use crate::gerber_write::{write_gerber, GerberOpts};
use crate::image::{GItem, ObjAttrs};
use crate::profile::OutFile;
use crate::tables::BottomX;
use crate::text::escape_attr;
use fab_elec_core::{Angle, BBox, LayerId, LayerSet, Nm, Shape, P};
use fab_elec_parts::{PadKind, PadShape};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PlacementPath { Dialog, Job, Cli }

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PlacementFormat { Ascii, Csv, GerberX3 }

impl PlacementFormat {
    fn job_name(self) -> &'static str {
        match self { Self::Ascii => "ascii", Self::Csv => "csv", Self::GerberX3 => "gerber" }
    }
    fn extension(self) -> &'static str {
        match self { Self::Ascii => "pos", Self::Csv => "csv", Self::GerberX3 => "gbr" }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PlacementUnits { Inches, Millimeters }

impl PlacementUnits {
    fn factor(self) -> f64 {
        match self { Self::Millimeters => 1e-6, Self::Inches => 0.001 / 25400.0 }
    }
    fn label(self) -> &'static str {
        match self { Self::Millimeters => "mm", Self::Inches => "inches" }
    }
    fn job_name(self) -> &'static str {
        match self { Self::Millimeters => "mm", Self::Inches => "in" }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PlacementSide { Front, Back, Both }

impl PlacementSide {
    fn accepts(self, layer: LayerId) -> bool {
        self == Self::Both || (self == Self::Front && layer == LayerId::F_CU)
            || (self == Self::Back && layer == LayerId::B_CU)
    }
    fn label(self) -> &'static str {
        match self { Self::Front => "top", Self::Back => "bottom", Self::Both => "All" }
    }
    fn job_name(self) -> &'static str {
        match self { Self::Front => "front", Self::Back => "back", Self::Both => "both" }
    }
}

/// A fully resolved variant, including inherited values. The empty name selects
/// `PlacementFootprint::default_variant`; absent named entries inherit it too.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PlacementVariant {
    pub value: String,
    pub exclude_from_pos_files: bool,
    pub dnp: bool,
    pub exclude_from_bom: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub struct PlacementPad {
    pub number: String,
    pub kind: PadKind,
    /// Source file's pad `at`: local position, stored absolute orientation.
    pub at: P,
    pub rotation: Angle,
    pub layers: LayerSet,
    /// Shape and offset on the default pad layer, before any rotation.
    pub shape: PadShape,
    pub shape_offset: P,
    pub drill_size: P,
}

#[derive(Clone, PartialEq, Debug)]
pub struct PlacementFootprint {
    /// Shown reference with document text variables resolved by the host.
    pub reference: String,
    pub library_id: String,
    pub at: P,
    pub rotation: Angle,
    pub layer: LayerId,
    /// Footprint attributes, independent of pad kinds.
    pub smd: bool,
    pub through_hole: bool,
    pub default_variant: PlacementVariant,
    pub variants: BTreeMap<String, PlacementVariant>,
    pub pads: Vec<PlacementPad>,
}

impl PlacementFootprint {
    pub fn variant(&self, name: &str) -> &PlacementVariant {
        if name.is_empty() { &self.default_variant }
        else { self.variants.get(name).unwrap_or(&self.default_variant) }
    }
    fn package(&self) -> &str {
        self.library_id.split_once(':').map_or(self.library_id.as_str(), |(_, name)| name)
    }
    fn side(&self) -> PlacementSide {
        if self.layer == LayerId::B_CU { PlacementSide::Back } else { PlacementSide::Front }
    }
}

/// One drawing/footprint (including its texts)/track/zone extent in source axes.
/// Supply ALL physical items, including items on layers hidden in the editor.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlacementExtent { pub layers: LayerSet, pub bounds: BBox }

#[derive(Clone, PartialEq, Debug)]
pub struct PlacementBoard {
    pub filename: String,
    pub aux_origin: P,
    /// The board's pcbplotparams.useauxorigin, independent of placement options.
    pub use_aux_origin: bool,
    pub copper_layers: u8,
    pub current_variant: String,
    /// Named variants in document order; the dialog prepends the empty/default name.
    pub variants: Vec<String>,
    pub footprints: Vec<PlacementFootprint>,
    pub physical_extents: Vec<PlacementExtent>,
    /// Edge.Cuts strokes/shapes, in stored board axes, without component attributes.
    pub board_edges: Vec<Shape>,
}

impl Default for PlacementBoard {
    fn default() -> Self {
        Self { filename: "board.kicad_pcb".into(), aux_origin: P::ZERO,
            use_aux_origin: false, copper_layers: 2, current_variant: String::new(),
            variants: Vec::new(), footprints: Vec::new(), physical_extents: Vec::new(),
            board_edges: Vec::new() }
    }
}

impl PlacementBoard {
    pub fn variant_choices(&self) -> Vec<&str> {
        std::iter::once("").chain(self.variants.iter().map(String::as_str).filter(|s| !s.is_empty())).collect()
    }
    /// S46.R82/Q10: copper, silk, mask, adhesive and paste, regardless of visibility.
    pub fn report_bounds(&self) -> BBox {
        self.physical_extents.iter().filter(|e| e.layers.iter().any(physical_layer))
            .fold(BBox::EMPTY, |b, e| b.union(e.bounds))
    }
}

fn physical_layer(layer: LayerId) -> bool {
    layer.is_copper() || matches!(layer, LayerId::F_SILK | LayerId::B_SILK
        | LayerId::F_MASK | LayerId::B_MASK | LayerId::F_ADHES | LayerId::B_ADHES
        | LayerId::F_PASTE | LayerId::B_PASTE)
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlacementExportOptions {
    pub path: PlacementPath,
    pub description: String,
    /// A resolved folder for Dialog, a file for Job/Cli. Host expands project/env
    /// variables and roots relative dialog paths at the board's folder (S46.R71).
    /// `${VARIANT}` is expanded here after the CLI multiple-variant check.
    pub output_filename: String,
    pub format: PlacementFormat,
    pub units: PlacementUnits,
    pub side: PlacementSide,
    pub single_file: bool,
    pub use_drill_place_file_origin: bool,
    pub smd_only: bool,
    pub exclude_footprints_with_th: bool,
    pub exclude_dnp: bool,
    pub exclude_bom: bool,
    pub negate_bottom_x: bool,
    pub gerber_board_edge: bool,
    pub variant: String,
}

impl PlacementExportOptions {
    pub fn for_path(path: PlacementPath) -> Self {
        Self { path, description: String::new(), output_filename: String::new(),
            format: PlacementFormat::Ascii,
            units: if path == PlacementPath::Cli { PlacementUnits::Inches } else { PlacementUnits::Millimeters },
            side: PlacementSide::Both, single_file: path != PlacementPath::Job,
            use_drill_place_file_origin: path != PlacementPath::Cli,
            smd_only: false, exclude_footprints_with_th: false, exclude_dnp: false,
            exclude_bom: false, negate_bottom_x: false,
            gerber_board_edge: path == PlacementPath::Job, variant: String::new() }
    }
    pub fn for_dialog(board: &PlacementBoard) -> Self {
        Self { variant: board.current_variant.clone(), ..Self::for_path(PlacementPath::Dialog) }
    }
    pub fn effective_origin(&self, board: &PlacementBoard) -> P {
        let aux = if self.format == PlacementFormat::GerberX3 { board.use_aux_origin }
            else { self.use_drill_place_file_origin };
        if aux { board.aux_origin } else { P::ZERO }
    }
    pub fn set_dialog_format(&mut self, format: PlacementFormat) {
        self.format = format;
        if format == PlacementFormat::GerberX3 {
            self.smd_only = false;
            self.exclude_footprints_with_th = false;
            self.negate_bottom_x = false;
        }
    }
    /// Job mode stores both sides even when the input job selected one side.
    pub fn apply_dialog_edits(&mut self, edits: &Self) {
        let path = self.path;
        let description = self.description.clone();
        *self = edits.clone();
        self.path = path;
        self.description = description;
        self.side = PlacementSide::Both;
        self.set_dialog_format(self.format);
    }
    pub fn dialog_state(&self) -> PlacementDialogState {
        let text = self.format != PlacementFormat::GerberX3;
        let job = self.path == PlacementPath::Job;
        PlacementDialogState { units_enabled: text, single_file_enabled: text,
            smd_only_enabled: text, exclude_th_enabled: text, negate_bottom_x_enabled: text,
            origin_enabled: text, origin_reason: if text { "" } else {
                "Gerber X3 uses the board plot setting useauxorigin." },
            board_edge_enabled: !text, folder_browser_visible: !job,
            messages_visible: !job, output_label: if job { "Output file:" } else { "Output directory:" } }
    }
}

impl Default for PlacementExportOptions {
    fn default() -> Self { Self::for_path(PlacementPath::Dialog) }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlacementDialogState {
    pub units_enabled: bool,
    pub single_file_enabled: bool,
    pub smd_only_enabled: bool,
    pub exclude_th_enabled: bool,
    pub negate_bottom_x_enabled: bool,
    pub origin_enabled: bool,
    pub origin_reason: &'static str,
    pub board_edge_enabled: bool,
    pub folder_browser_visible: bool,
    pub messages_visible: bool,
    pub output_label: &'static str,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlacementError { pub exit_code: u8, pub message: String }

impl std::fmt::Display for PlacementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
}
impl std::error::Error for PlacementError {}

fn error(exit_code: u8, message: impl Into<String>) -> PlacementError {
    PlacementError { exit_code, message: message.into() }
}

impl PlacementError {
    pub fn board_unreadable() -> Self {
        error(3, "Board file does not exist or is not accessible")
    }
    pub fn directory(path: PlacementPath, folder: &str) -> Self {
        error(4, if path == PlacementPath::Dialog {
            format!("Could not write plot files to folder '{folder}'.")
        } else { "Failed to create output directory".into() })
    }
    pub fn file(path: &str) -> Self { error(4, format!("Failed to create file '{path}'.")) }
}

impl PlacementExportOptions {
    /// Arguments after `pcb export pos`, excluding the board filename. The host
    /// supplies accessibility before any option validation, and performs actual I/O.
    pub fn from_cli(board_accessible: bool, args: &[&str]) -> Result<Self, PlacementError> {
        if !board_accessible { return Err(PlacementError::board_unreadable()); }
        let mut out = Self::for_path(PlacementPath::Cli);
        let (mut format, mut units, mut side) = ("ascii", "in", "both");
        let mut variants = Vec::new();
        let mut args = args.iter().copied();
        while let Some(flag) = args.next() {
            match flag {
                "--format" | "--units" | "--side" | "--variant" | "-o" | "--output" => {
                    let value = args.next().ok_or_else(|| error(1, format!("Missing value for {flag}")))?;
                    match flag {
                        "--format" => format = value, "--units" => units = value,
                        "--side" => side = value, "--variant" => variants.push(value),
                        _ => out.output_filename = value.into(),
                    }
                }
                "--use-drill-file-origin" => out.use_drill_place_file_origin = true,
                "--smd-only" => out.smd_only = true,
                "--exclude-fp-th" => out.exclude_footprints_with_th = true,
                "--exclude-dnp" => out.exclude_dnp = true,
                "--exclude-bom" => out.exclude_bom = true,
                "--bottom-negate-x" => out.negate_bottom_x = true,
                "--gerber-board-edge" => out.gerber_board_edge = true,
                _ => return Err(error(1, format!("Unknown placement option {flag}"))),
            }
        }
        out.format = match format { "ascii" => PlacementFormat::Ascii, "csv" => PlacementFormat::Csv,
            "gerber" => PlacementFormat::GerberX3, _ => return Err(error(1, "Invalid format")) };
        out.units = match units { "in" => PlacementUnits::Inches, "mm" => PlacementUnits::Millimeters,
            _ => return Err(error(1, "Invalid units specified")) };
        out.side = match side { "front" => PlacementSide::Front, "back" => PlacementSide::Back,
            "both" => PlacementSide::Both, _ => return Err(error(1, "Invalid side specified")) };
        validate_cli_side(&out)?;
        if variants.len() > 1 && !out.output_filename.contains("${VARIANT}") {
            return Err(error(1, "When specifying multiple variants, the output path must contain ${VARIANT} to generate separate output files for each variant."));
        }
        out.variant = variants.first().copied().unwrap_or("").into();
        Ok(out)
    }
}

fn validate_cli_side(opts: &PlacementExportOptions) -> Result<(), PlacementError> {
    if opts.path == PlacementPath::Cli && opts.format == PlacementFormat::GerberX3
        && opts.side == PlacementSide::Both {
        return Err(error(1, "\"both\" not supported for Gerber format"));
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlacementOutput {
    pub file: OutFile,
    pub side: PlacementSide,
    pub component_count: usize,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PlacementExport {
    pub files: Vec<PlacementOutput>,
    /// Success messages to display ONLY after the host has written the files.
    pub messages: Vec<String>,
}

/// S46.R71 names preserve the job's extension; CLI names remain exactly as given.
fn output_name(board: &PlacementBoard, opts: &PlacementExportOptions, side: PlacementSide) -> String {
    let output = opts.output_filename.replace("${VARIANT}", &opts.variant);
    let board_name = basename(&board.filename);
    let (board_stem, _) = split_extension(board_name);
    if opts.path == PlacementPath::Cli {
        return if output.is_empty() { format!("{board_stem}.{}", opts.format.extension()) } else { output };
    }
    let default_name = format!("{board_stem}.{}", opts.format.extension());
    let input = if opts.path == PlacementPath::Dialog || output.is_empty() { &default_name } else { &output };
    let (stem, ext) = split_extension(input);
    let side = if side == PlacementSide::Both { "all" } else { side.label() };
    let name = match opts.format {
        PlacementFormat::GerberX3 => format!("{stem}-pnp_{side}.gbr"),
        PlacementFormat::Ascii | PlacementFormat::Csv => {
            let suffix = if opts.format == PlacementFormat::Csv { "-pos" } else { "" };
            let extension = if ext.is_empty() { String::new() } else { format!(".{ext}") };
            format!("{stem}-{side}{suffix}{extension}")
        }
    };
    if opts.path == PlacementPath::Dialog {
        let folder = if output.is_empty() { parent(&board.filename) } else { &output };
        join(folder, &name)
    } else { name }
}

fn basename(path: &str) -> &str { path.rsplit(['/', '\\']).next().unwrap_or(path) }
fn parent(path: &str) -> &str {
    path.rfind(['/', '\\']).map_or("", |i| if i == 0 { &path[..1] } else { &path[..i] })
}
fn split_extension(path: &str) -> (&str, &str) {
    let start = path.len() - basename(path).len();
    match path[start..].rfind('.') {
        Some(i) if i > 0 => (&path[..start + i], &path[start + i + 1..]),
        _ => (path, ""),
    }
}
fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() { name.into() } else { format!("{}/{name}", folder.trim_end_matches(['/', '\\'])) }
}

fn selected<'a>(board: &'a PlacementBoard, opts: &PlacementExportOptions, side: PlacementSide) -> Vec<&'a PlacementFootprint> {
    let gerber = opts.format == PlacementFormat::GerberX3;
    let mut out: Vec<_> = board.footprints.iter().filter(|fp| {
        let variant = fp.variant(&opts.variant);
        side.accepts(fp.layer) && !variant.exclude_from_pos_files
            && (gerber || !opts.smd_only || fp.smd)
            && (gerber || !opts.exclude_footprints_with_th || !fp.pads.iter().any(|p| matches!(p.kind, PadKind::Thru | PadKind::Npth)))
            && (!opts.exclude_dnp || !variant.dnp)
            && (!opts.exclude_bom || !variant.exclude_from_bom)
    }).collect();
    if !gerber {
        out.sort_by(|a, b| (a.layer != LayerId::B_CU).cmp(&(b.layer != LayerId::B_CU))
            .then_with(|| natural_cmp(&a.reference, &b.reference)));
    }
    out
}

/// S46.R72/R84: the same selected variant drives both counting and writing.
pub fn export_placement(board: &PlacementBoard, opts: &PlacementExportOptions) -> Result<PlacementExport, PlacementError> {
    validate_cli_side(opts)?;
    let side = if opts.path == PlacementPath::Dialog { PlacementSide::Both } else { opts.side };
    let gerber = opts.format == PlacementFormat::GerberX3;
    if opts.path == PlacementPath::Dialog && !gerber && selected(board, opts, PlacementSide::Both).is_empty() {
        return Ok(PlacementExport { files: Vec::new(), messages: vec!["No footprint for automated placement.".into()] });
    }
    let sides = if side != PlacementSide::Both { vec![side] }
        else if !gerber && (opts.single_file || opts.path == PlacementPath::Cli) { vec![PlacementSide::Both] }
        else { vec![PlacementSide::Front, PlacementSide::Back] };
    let mut out = PlacementExport::default();
    for side in sides {
        let fps = selected(board, opts, side);
        let name = output_name(board, opts, side);
        let bytes = match opts.format {
            PlacementFormat::Ascii => write_ascii(board, opts, side, &fps).into_bytes(),
            PlacementFormat::Csv => write_csv(board, opts, &fps).into_bytes(),
            PlacementFormat::GerberX3 => write_x3(board, opts, side, &fps)
                .map_err(|e| error(4, e.to_string()))?,
        };
        let count = fps.len();
        let message = if opts.path == PlacementPath::Dialog {
            match side {
                PlacementSide::Front => format!("Front (top side) placement file: '{name}'."),
                PlacementSide::Back => format!("Back (bottom side) placement file: '{name}'."),
                PlacementSide::Both => format!("Placement file: '{name}'."),
            }
        } else {
            let which = match side { PlacementSide::Front => "front ", PlacementSide::Back => "back ", PlacementSide::Both => "" };
            format!("Wrote {which}position data to '{name}'.")
        };
        out.messages.push(message);
        if opts.path == PlacementPath::Dialog { out.messages.push(format!("Component count: {count}.")); }
        out.files.push(PlacementOutput { file: OutFile { name, bytes }, side, component_count: count });
    }
    if opts.path == PlacementPath::Dialog {
        if out.files.len() == 2 {
            out.messages.push(format!("Full component count: {}.", out.files.iter().map(|f| f.component_count).sum::<usize>()));
        }
        out.messages.push("Done.".into());
    }
    Ok(out)
}

fn version() -> String { format!("{}.{}", env!("CARGO_PKG_VERSION_MAJOR"), env!("CARGO_PKG_VERSION_MINOR")) }

/// Formatting f64 directly rounds its exact binary value, ties to even, and
/// preserves negative zero. Do not round a scaled decimal first (S46.R75/R76).
fn fixed(value: f64, width: usize, decimals: usize) -> String {
    format!("{value:>width$.decimals$}")
}
fn length(nm: i64, units: PlacementUnits, width: usize, decimals: usize) -> String {
    fixed(nm as f64 * units.factor(), width, decimals)
}
fn angle(angle: Angle) -> f64 {
    let udeg = angle.udeg().rem_euclid(360_000_000);
    (if udeg > 180_000_000 { udeg - 360_000_000 } else { udeg }) as f64 / 1_000_000.0
}
fn position(fp: &PlacementFootprint, board: &PlacementBoard, opts: &PlacementExportOptions) -> (i64, i64) {
    let origin = opts.effective_origin(board);
    let x = fp.at.x as i64 - origin.x as i64;
    let x = if opts.negate_bottom_x && fp.layer == LayerId::B_CU { BottomX::NegateX.apply(x) } else { x };
    (x, origin.y as i64 - fp.at.y as i64)
}
fn lf(text: &str) -> String { text.replace("\r\n", "\n").replace('\r', "\n") }
fn text_cell(text: &str, width: usize) -> String { format!("{:<width$}", lf(text).replace(' ', "_")) }
fn quote_csv(text: &str) -> String { format!("\"{}\"", lf(text).replace('"', "\"\"")) }

fn write_ascii(board: &PlacementBoard, opts: &PlacementExportOptions, side: PlacementSide, fps: &[&PlacementFootprint]) -> String {
    let mut widths = [8, 8, 16];
    for fp in fps {
        for (i, text) in [&fp.reference, &fp.variant(&opts.variant).value, fp.package()].into_iter().enumerate() {
            widths[i] = widths[i].max(text.chars().count());
        }
    }
    let [r, v, p] = widths;
    let mut out = format!("### Footprint positions ###\n### Printed by Fab version {}\n## Unit = {}, Angle = deg.\n## Side : {}\n", version(), opts.units.label(), side.label());
    let _ = writeln!(out, "{:<r$}  {:<v$}  {:<p$}  {:>9}  {:>9}  {:>8}  Side", "# Ref", "Val", "Package", "PosX", "PosY", "Rot");
    for fp in fps {
        let (x, y) = position(fp, board, opts);
        let _ = writeln!(out, "{}  {}  {}  {}  {}  {}  {}",
            text_cell(&fp.reference, r), text_cell(&fp.variant(&opts.variant).value, v), text_cell(fp.package(), p),
            length(x, opts.units, 9, 4), length(y, opts.units, 9, 4), fixed(angle(fp.rotation), 8, 4), fp.side().label());
    }
    out.push_str("## End\n");
    out
}
fn write_csv(board: &PlacementBoard, opts: &PlacementExportOptions, fps: &[&PlacementFootprint]) -> String {
    let mut out = String::from("Ref,Val,Package,PosX,PosY,Rot,Side\n");
    for fp in fps {
        let (x, y) = position(fp, board, opts);
        let _ = writeln!(out, "{},{},{},{},{},{},{}", quote_csv(&fp.reference), quote_csv(&fp.variant(&opts.variant).value),
            quote_csv(fp.package()), length(x, opts.units, 0, 6), length(y, opts.units, 0, 6),
            fixed(angle(fp.rotation), 0, 6), fp.side().label());
    }
    out
}

/// S46's X3 policy: ComponentMain flashes in board order with component metadata.
/// Detailed assembly drawings/courtyards and pin graphics belong to S42.
fn write_x3(board: &PlacementBoard, opts: &PlacementExportOptions, side: PlacementSide, fps: &[&PlacementFootprint]) -> Result<Vec<u8>, CamError> {
    let origin = opts.effective_origin(board);
    let mut edges = Vec::new();
    if opts.gerber_board_edge {
        for shape in &board.board_edges {
            // The common geometry writer uses i32. Reject an unrepresentable edge
            // transform rather than wrap its coordinates. Component positions use i64.
            let bb = shape.bbox();
            if !bb.is_empty() {
                for p in [bb.min, bb.max] {
                    for n in [p.x as i64 - origin.x as i64, p.y as i64 - origin.y as i64] {
                        if n <= Nm::MIN as i64 || n > Nm::MAX as i64 {
                            return Err(CamError::Profile("placement edge exceeds coordinate range".into()));
                        }
                    }
                }
            }
            let offset = P::new(origin.x.checked_neg().ok_or_else(|| CamError::Profile("placement origin exceeds coordinate range".into()))?,
                origin.y.checked_neg().ok_or_else(|| CamError::Profile("placement origin exceeds coordinate range".into()))?);
            let shape = shape.translated(offset).mirrored_x(0).rotated_about(P::ZERO, Angle::deg(180.0));
            edges.push(GItem::dark(shape).with_attrs(ObjAttrs { aper_function: vec!["Profile".into()], ..Default::default() }));
        }
    }
    let (layer, side_name) = if side == PlacementSide::Front { (1, "Top") } else { (board.copper_layers, "Bot") };
    let attrs = vec![("FileFunction".into(), vec!["Component".into(), format!("L{layer}"), side_name.into()])];
    let bytes = write_gerber(&edges, &attrs, &GerberOpts::default())?;
    let mut out = String::from_utf8(bytes).expect("Gerber writer emits UTF-8");
    out.truncate(out.len() - "M02*\n".len());
    if !fps.is_empty() {
        // Keep the common edge writer's aperture numbers; allocate the marker after them.
        let aperture = out.lines().filter_map(|l| l.strip_prefix("%ADD"))
            .filter_map(|l| l.split(|c: char| !c.is_ascii_digit()).next()?.parse::<u32>().ok())
            .max().unwrap_or(9) + 1;
        let _ = writeln!(out, "%TA.AperFunction,ComponentMain*%\n%ADD{aperture}C,0.3*%\n%TD*%\n%LPD*%\nD{aperture}*");
        for fp in fps {
            let variant = fp.variant(&opts.variant);
            let mount = if fp.smd { "SMD" } else if fp.through_hole { "TH" } else { "Other" };
            let _ = writeln!(out, "%TO.C,{}*%\n%TO.CRot,{}*%\n%TO.CMnt,{mount}*%\n%TO.CVal,{}*%\n%TO.CFtp,{}*%",
                escape_attr(&fp.reference), fixed(angle(fp.rotation), 0, 6), escape_attr(&variant.value), escape_attr(fp.package()));
            let x = fp.at.x as i64 - origin.x as i64;
            let y = origin.y as i64 - fp.at.y as i64;
            let _ = writeln!(out, "X{x}Y{y}D03*\n%TD*%");
        }
    }
    out.push_str("M02*\n");
    Ok(out.into_bytes())
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ReportUserUnits { Millimeters, Inches, Mils }

impl ReportUserUnits {
    fn placement(self) -> PlacementUnits {
        if self == Self::Millimeters { PlacementUnits::Millimeters } else { PlacementUnits::Inches }
    }
}

/// S46.R81: all footprints, board origin and stored Y, current board variant.
/// The folder is already resolved by the host; no placement filter applies.
pub fn footprint_report(board: &PlacementBoard, user_units: ReportUserUnits, folder: &str) -> OutFile {
    let units = user_units.placement();
    let number = |nm: Nm| length(nm as i64, units, 9, 6);
    let mut out = format!("## Footprint report\n## Printed by Fab version {}\n## Unit = {}, Angle = deg.\n\n$BeginDESCRIPTION\n\n$BOARD\n", version(), units.label());
    let bounds = board.report_bounds();
    let corner = if bounds.is_empty() { P::ZERO } else { bounds.min };
    let _ = writeln!(out, "upper_left_corner {} {}\n$EndBOARD\n", number(corner.x), number(corner.y));
    let mut fps: Vec<_> = board.footprints.iter().collect();
    fps.sort_by_cached_key(|fp| NaturalKey(fp.reference.to_lowercase()));
    for fp in fps {
        let reference = lf(&fp.reference);
        let _ = writeln!(out, "$MODULE {reference}\nreference {reference}\nvalue {}\nfootprint {}",
            lf(&fp.variant(&board.current_variant).value), lf(&fp.library_id));
        out.push_str("attribut");
        if !fp.smd && !fp.through_hole { out.push_str(" virtual"); }
        if fp.smd { out.push_str(" smd"); }
        if fp.through_hole { out.push_str(" none"); }
        out.push('\n');
        let layer = if fp.layer == LayerId::F_CU { "front" } else if fp.layer == LayerId::B_CU { "back" } else { "other" };
        let _ = writeln!(out, "position {} {}  orientation {}\nlayer {layer}", number(fp.at.x), number(fp.at.y), fixed(angle(fp.rotation), 0, 2));
        let mut pads: Vec<_> = fp.pads.iter().collect();
        pads.sort_by_cached_key(|pad| NaturalKey(pad.number.to_lowercase()));
        for pad in pads {
            let (shape, size) = pad_shape(&pad.shape);
            let layer = match (pad.layers.has(LayerId::F_CU), pad.layers.has(LayerId::B_CU)) {
                (true, true) => "both", (true, false) => "front", (false, true) => "back", _ => "nocopper",
            };
            let _ = writeln!(out, "$PAD \"{}\"\nShape {shape} Layer {layer}\nposition {} {} size {} {} orientation {}\ndrill {}\nshape_offset {} {}\n$EndPAD",
                lf(&pad.number), number(pad.at.x), number(pad.at.y), number(size.x), number(size.y), fixed(angle(pad.rotation), 0, 2),
                number(pad.drill_size.x), number(pad.shape_offset.x), number(pad.shape_offset.y));
        }
        let _ = writeln!(out, "$EndMODULE {reference}\n");
    }
    out.push_str("$EndDESCRIPTION\n");
    let (stem, _) = split_extension(basename(&board.filename));
    OutFile { name: join(folder, &format!("{stem}.rpt")), bytes: out.into_bytes() }
}

pub fn footprint_report_message(file: &OutFile) -> String { format!("Footprint report file created: {}", file.name) }

fn pad_shape(shape: &PadShape) -> (&'static str, P) {
    match shape {
        PadShape::Circle { d } => ("Circle", P::new(*d, *d)),
        PadShape::Oval { size } => ("Oval", *size),
        PadShape::Rect { size } => ("Rect", *size),
        PadShape::Trapezoid { size, .. } => ("Trap", *size),
        PadShape::RoundRect { size, .. } => ("Roundrect", *size),
        PadShape::Chamfer { size, .. } => ("Chamferedrect", *size),
        PadShape::Custom { anchor, .. } => ("CustomShape", pad_shape(anchor).1),
    }
}

// Stable natural sort: equal digit runs (R01/R1) remain in board order.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    while !a.is_empty() && !b.is_empty() {
        let order = if a[0].is_ascii_digit() && b[0].is_ascii_digit() {
            let na = a.iter().position(|c| !c.is_ascii_digit()).unwrap_or(a.len());
            let nb = b.iter().position(|c| !c.is_ascii_digit()).unwrap_or(b.len());
            let za = a[..na].iter().position(|c| *c != b'0').unwrap_or(na);
            let zb = b[..nb].iter().position(|c| *c != b'0').unwrap_or(nb);
            let order = (na - za).cmp(&(nb - zb)).then_with(|| a[za..na].cmp(&b[zb..nb]));
            a = &a[na..]; b = &b[nb..]; order
        } else {
            let order = a[0].cmp(&b[0]); a = &a[1..]; b = &b[1..]; order
        };
        if order != Ordering::Equal { return order; }
    }
    a.len().cmp(&b.len())
}
#[derive(Eq)]
struct NaturalKey(String);
impl PartialEq for NaturalKey { fn eq(&self, other: &Self) -> bool { self.cmp(other) == Ordering::Equal } }
impl Ord for NaturalKey { fn cmp(&self, other: &Self) -> Ordering { natural_cmp(&self.0, &other.0) } }
impl PartialOrd for NaturalKey { fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) } }

impl PlacementExportOptions {
    pub fn to_job_json(&self) -> String {
        let mut fields = Vec::new();
        let mut put = |key: &str, value: String| fields.push(format!("  {}: {value}", json_quote(key)));
        put("description", json_quote(&self.description));
        put("output_filename", json_quote(&self.output_filename));
        macro_rules! bools { ($($field:ident),* $(,)?) => { $(put(stringify!($field), self.$field.to_string());)* }; }
        bools!(use_drill_place_file_origin, smd_only, exclude_footprints_with_th, exclude_dnp,
            exclude_bom, negate_bottom_x, single_file, gerber_board_edge);
        put("side", json_quote(self.side.job_name()));
        put("units", json_quote(self.units.job_name()));
        put("format", json_quote(self.format.job_name()));
        put("variant", json_quote(&self.variant));
        format!("{{\n{}\n}}\n", fields.join(",\n"))
    }
    pub fn from_job_json(json: &str) -> Result<Self, CamError> {
        let mut opts = Self::for_path(PlacementPath::Job);
        for (key, value) in FlatJobJson::read(json)? {
            match key.as_str() {
                "description" => opts.description = value.string(&key)?.into(),
                "output_filename" => opts.output_filename = value.string(&key)?.into(),
                "variant" => opts.variant = value.string(&key)?.into(),
                "use_drill_place_file_origin" => opts.use_drill_place_file_origin = value.boolean(&key)?,
                "smd_only" => opts.smd_only = value.boolean(&key)?,
                "exclude_footprints_with_th" => opts.exclude_footprints_with_th = value.boolean(&key)?,
                "exclude_dnp" => opts.exclude_dnp = value.boolean(&key)?,
                "exclude_bom" => opts.exclude_bom = value.boolean(&key)?,
                "negate_bottom_x" => opts.negate_bottom_x = value.boolean(&key)?,
                "single_file" => opts.single_file = value.boolean(&key)?,
                "gerber_board_edge" => opts.gerber_board_edge = value.boolean(&key)?,
                "side" => opts.side = match value.string(&key)? { "back" => PlacementSide::Back, "both" => PlacementSide::Both, _ => PlacementSide::Front },
                "units" => opts.units = match value.string(&key)? { "mm" => PlacementUnits::Millimeters, _ => PlacementUnits::Inches },
                "format" => opts.format = match value.string(&key)? { "csv" => PlacementFormat::Csv, "gerber" => PlacementFormat::GerberX3, _ => PlacementFormat::Ascii },
                _ => {}
            }
        }
        Ok(opts)
    }
}

fn json_quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""), '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"), '\r' => out.push_str("\\r"), '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

enum JsonScalar { String(String), Bool(bool), Number, Null }

impl JsonScalar {
    fn string(&self, key: &str) -> Result<&str, CamError> {
        match self { Self::String(s) => Ok(s), _ => Err(CamError::Profile(format!("{key} must be a string"))) }
    }
    fn boolean(&self, key: &str) -> Result<bool, CamError> {
        match self { Self::Bool(b) => Ok(*b), _ => Err(CamError::Profile(format!("{key} must be a boolean"))) }
    }
}

/// Dependency-free parser for this scalar, flat job schema. Rejects nested objects, malformed escapes, and trailing input.
struct FlatJobJson<'a> { input: &'a str, at: usize }

impl<'a> FlatJobJson<'a> {
    fn read(input: &'a str) -> Result<Vec<(String, JsonScalar)>, CamError> {
        let mut p = Self { input, at: 0 };
        let mut fields = Vec::new();
        p.expect('{')?;
        if !p.consume('}') {
            loop {
                p.space();
                let key = p.string()?;
                p.expect(':')?;
                p.space();
                let value = match p.peek() {
                    Some('"') => JsonScalar::String(p.string()?),
                    Some('t') => { p.literal("true")?; JsonScalar::Bool(true) }
                    Some('f') => { p.literal("false")?; JsonScalar::Bool(false) }
                    Some('n') => { p.literal("null")?; JsonScalar::Null }
                    Some('-' | '0'..='9') => { p.number()?; JsonScalar::Number },
                    _ => return Err(p.error("expected a scalar placement job value")),
                };
                fields.push((key, value));
                if p.consume('}') { break; }
                p.expect(',')?;
            }
        }
        p.space();
        if p.at != input.len() { return Err(p.error("trailing JSON input")); }
        Ok(fields)
    }
    fn error(&self, message: &str) -> CamError {
        CamError::parse(self.input[..self.at].bytes().filter(|&c| c == b'\n').count() + 1, message)
    }
    fn peek(&self) -> Option<char> { self.input[self.at..].chars().next() }
    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += c.len_utf8();
        Some(c)
    }
    fn space(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) { self.next(); }
    }
    fn consume(&mut self, c: char) -> bool {
        self.space();
        if self.peek() == Some(c) { self.next(); true } else { false }
    }
    fn expect(&mut self, c: char) -> Result<(), CamError> {
        if self.consume(c) { Ok(()) } else { Err(self.error(&format!("expected {c:?}"))) }
    }
    fn literal(&mut self, value: &str) -> Result<(), CamError> {
        if self.input[self.at..].starts_with(value) { self.at += value.len(); Ok(()) }
        else { Err(self.error("invalid JSON literal")) }
    }
    fn digits(&mut self) -> Result<(), CamError> {
        let start = self.at;
        while matches!(self.peek(), Some('0'..='9')) { self.next(); }
        if start == self.at { Err(self.error("expected a digit")) } else { Ok(()) }
    }
    fn number(&mut self) -> Result<String, CamError> {
        let start = self.at;
        if self.peek() == Some('-') { self.next(); }
        if self.peek() == Some('0') { self.next(); } else { self.digits()?; }
        if self.peek() == Some('.') { self.next(); self.digits()?; }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.next();
            if matches!(self.peek(), Some('+' | '-')) { self.next(); }
            self.digits()?;
        }
        Ok(self.input[start..self.at].into())
    }
    fn hex4(&mut self) -> Result<u32, CamError> {
        let mut n = 0;
        for _ in 0..4 {
            n = n * 16 + self.next().and_then(|c| c.to_digit(16)).ok_or_else(|| self.error("invalid Unicode escape"))?;
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String, CamError> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            let c = self.next().ok_or_else(|| self.error("unterminated JSON string"))?;
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let c = self.next().ok_or_else(|| self.error("unterminated JSON escape"))?;
                    out.push(match c {
                        '"' | '\\' | '/' => c,
                        'b' => '\u{8}', 'f' => '\u{c}', 'n' => '\n', 'r' => '\r', 't' => '\t',
                        'u' => {
                            let mut n = self.hex4()?;
                            if (0xd800..=0xdbff).contains(&n) {
                                self.literal("\\u")?;
                                let low = self.hex4()?;
                                if !(0xdc00..=0xdfff).contains(&low) { return Err(self.error("invalid low surrogate")); }
                                n = 0x10000 + ((n - 0xd800) << 10) + low - 0xdc00;
                            }
                            char::from_u32(n).ok_or_else(|| self.error("invalid Unicode scalar"))?
                        }
                        _ => return Err(self.error("invalid JSON escape")),
                    });
                }
                c if c < ' ' => return Err(self.error("unescaped control in JSON string")),
                c => out.push(c),
            }
        }
    }
}


#[cfg(test)]
#[path = "placement_export_tests.rs"]
mod tests;

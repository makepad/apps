//! S46.3 drill export policy. The host supplies classified holes (S41), board settings,
//! and resolved paths; this module does no filesystem I/O and does not mutate a board.
//! Maps and via protection are planned here for their respective drawing backends.

use crate::drill::{write_excellon_with_options, DrillOpts, ExcellonOptions, SlotMode, Zeros};
use crate::error::CamError;
use crate::gerber_write::{write_gerber, GerberOpts};
use crate::image::{DrillHole, FileAttr, GItem};
use fab_elec_core::{Nm, Shape, P};
use std::collections::BTreeMap;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DrillPath { Dialog, Job, Cli }

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DrillFormat { Excellon, Gerber }

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DrillOrigin { Absolute, Plot }

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DrillUnits { Inches, Millimeters }

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DrillMapFormat { Postscript, GerberX2, Dxf, Svg, Pdf }

impl DrillMapFormat {
    pub fn extension(self) -> &'static str {
        match self { Self::Postscript => "ps", Self::GerberX2 => "gbr", Self::Dxf => "dxf", Self::Svg => "svg", Self::Pdf => "pdf" }
    }
    fn job_name(self) -> &'static str {
        match self { Self::Postscript => "postscript", Self::GerberX2 => "gerberx2", Self::Dxf => "dxf", Self::Svg => "svg", Self::Pdf => "pdf" }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DrillMapPage {
    /// The host supplies the board's page when drawing a dialog export's maps.
    Board,
    A4Landscape,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DrillExportOptions {
    pub path: DrillPath,
    /// Board filename without its extension or folder, supplied by the host.
    pub board_stem: String,
    pub copper_layers: u8,
    pub description: String,
    pub output_dir: String,
    pub format: DrillFormat,
    pub origin: DrillOrigin,
    pub plot_origin: P,
    pub units: DrillUnits,
    pub zeros: Zeros,
    /// Fab extension. None selects 3:3 mm / 2:4 inch, including after a unit change.
    pub digit_counts: Option<(u8, u8)>,
    pub mirror_y: bool,
    pub minimal_header: bool,
    pub combine_pth_npth: bool,
    pub slots: SlotMode,
    pub gerber_precision: u8,
    pub generate_tenting: bool,
    pub generate_map: bool,
    pub map_format: DrillMapFormat,
    pub map_page: DrillMapPage,
    pub generate_report: bool,
    pub report_filename: String,
}

impl DrillExportOptions {
    pub fn for_path(path: DrillPath) -> Self {
        let dialog = path == DrillPath::Dialog;
        Self {
            path, board_stem: String::new(), copper_layers: 2, description: String::new(),
            output_dir: String::new(), format: DrillFormat::Excellon,
            origin: DrillOrigin::Absolute, plot_origin: P::ZERO,
            units: if path == DrillPath::Job { DrillUnits::Inches } else { DrillUnits::Millimeters },
            zeros: Zeros::Decimal, digit_counts: None, mirror_y: false, minimal_header: false,
            combine_pth_npth: !dialog,
            slots: if dialog { SlotMode::Route } else { SlotMode::Alternate },
            gerber_precision: 6, generate_tenting: false, generate_map: false,
            map_format: if dialog { DrillMapFormat::GerberX2 } else { DrillMapFormat::Pdf },
            map_page: if dialog { DrillMapPage::Board } else { DrillMapPage::A4Landscape },
            generate_report: false, report_filename: String::new(),
        }
    }

    /// S46.R52/R53: only dialog defaults inherit these board plot settings.
    pub fn with_board_settings(mut self, origin: DrillOrigin, output_dir: &str, precision: u8) -> Self {
        if self.path == DrillPath::Dialog {
            self.origin = origin;
            self.output_dir = output_dir.into();
            self.gerber_precision = if precision == 5 { 5 } else { 6 };
        }
        self
    }

    pub fn excellon_options(&self) -> ExcellonOptions {
        let mut format = DrillOpts::for_units(self.units == DrillUnits::Inches);
        format.zeros = self.zeros;
        if let Some((int, frac)) = self.digit_counts { format.int_digits = int; format.frac_digits = frac; }
        ExcellonOptions {
            format, slots: self.slots, minimal_header: self.minimal_header,
            origin: self.effective_origin(), negate_y: !self.mirror_y,
            decimal_places: Some(if self.units == DrillUnits::Inches { 4 } else { 3 }),
        }
    }

    pub fn effective_origin(&self) -> P {
        if self.origin == DrillOrigin::Plot { self.plot_origin } else { P::ZERO }
    }

    pub fn effective_gerber_precision(&self) -> u8 {
        if self.gerber_precision == 6 { 6 } else { 5 }
    }

    /// Basename only; an explicit report path is resolved independently of output_dir by the host.
    pub fn report_name(&self) -> String {
        if !self.report_filename.is_empty() { return self.report_filename.clone(); }
        format!("{}-{}.rpt", self.board_stem, if self.path == DrillPath::Dialog { "drl" } else { "drill" })
    }

    pub fn cli_warning(&self) -> Option<&'static str> {
        (self.path == DrillPath::Cli && !self.generate_report && !self.report_filename.is_empty())
            .then_some("Warning: Report path supplied without --generate-report, no report will be generated")
    }

    /// S46.R55/R57 control state; folder picking and board mutation belong to the host.
    pub fn dialog_state(&self) -> DrillDialogState {
        let excellon = self.format == DrillFormat::Excellon;
        let job = self.path == DrillPath::Job;
        let precision = if !excellon {
            format!("4.{}", if job { 6 } else { self.effective_gerber_precision() })
        } else if self.units == DrillUnits::Inches { "2:4".into() } else { "3:3".into() };
        DrillDialogState { excellon_controls_enabled: excellon, tenting_enabled: !excellon,
            precision_text: precision, precision_enabled: !excellon || self.zeros != Zeros::Decimal,
            report_button_visible: !job, message_box_visible: !job, folder_button_visible: !job }
    }

    /// Copy only the settings editable in the drill dialog, preserving job-only settings.
    pub fn apply_dialog_edits(&mut self, edits: &Self) {
        self.output_dir = edits.output_dir.replace('\\', "/");
        self.format = edits.format;
        self.units = edits.units;
        self.origin = edits.origin;
        self.combine_pth_npth = edits.combine_pth_npth;
        self.minimal_header = edits.minimal_header;
        self.mirror_y = edits.mirror_y;
        self.slots = edits.slots;
        self.map_format = edits.map_format;
        self.zeros = edits.zeros;
        self.generate_map = edits.generate_map;
        self.generate_tenting = edits.generate_tenting;
    }

    /// Validate one CLI flag at the CAM boundary. A host CLI handles tokenization and I/O.
    pub fn set_cli_option(&mut self, flag: &str, value: &str) -> Result<(), DrillCliError> {
        let bad = |message| DrillCliError { exit_code: 1, message };
        match flag {
            "--format" => self.format = match value { "excellon" => DrillFormat::Excellon, "gerber" => DrillFormat::Gerber, _ => return Err(bad("Invalid drill format")) },
            "--drill-origin" => self.origin = match value { "absolute" => DrillOrigin::Absolute, "plot" => DrillOrigin::Plot, _ => return Err(bad("Invalid origin mode specified")) },
            "-u" | "--excellon-units" => self.units = match value { "in" => DrillUnits::Inches, "mm" => DrillUnits::Millimeters, _ => return Err(bad("Invalid units specified")) },
            "--excellon-zeros-format" => self.zeros = match value { "decimal" => Zeros::Decimal, "suppressleading" => Zeros::KeepTrailing, "suppresstrailing" => Zeros::KeepLeading, "keep" => Zeros::Keep, _ => return Err(bad("Invalid zeros format specified")) },
            "--excellon-oval-format" => self.slots = match value { "route" => SlotMode::Route, "alternate" => SlotMode::Alternate, _ => return Err(bad("Invalid oval drill format specified")) },
            "--map-format" => self.map_format = match value { "ps" => DrillMapFormat::Postscript, "gerberx2" => DrillMapFormat::GerberX2, "dxf" => DrillMapFormat::Dxf, "svg" => DrillMapFormat::Svg, "pdf" => DrillMapFormat::Pdf, _ => return Err(bad("Invalid map format specified")) },
            "--gerber-precision" => self.gerber_precision = match value { "5" => 5, "6" => 6, _ => return Err(bad("Gerber coordinate precision should be either 5 or 6")) },
            "--excellon-mirror-y" => self.mirror_y = true,
            "--excellon-min-header" => self.minimal_header = true,
            "--excellon-separate-th" => self.combine_pth_npth = false,
            "--generate-tenting" => self.generate_tenting = true,
            "--generate-map" => self.generate_map = true,
            "--generate-report" => self.generate_report = true,
            "--report-path" => self.report_filename = value.into(),
            "-o" | "--output" => self.output_dir = value.into(),
            _ => return Err(bad("Unknown drill option")),
        }
        Ok(())
    }
}

impl Default for DrillExportOptions {
    fn default() -> Self { Self::for_path(DrillPath::Dialog) }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DrillDialogState {
    pub excellon_controls_enabled: bool,
    pub tenting_enabled: bool,
    pub precision_text: String,
    pub precision_enabled: bool,
    pub report_button_visible: bool,
    pub message_box_visible: bool,
    pub folder_button_visible: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DrillCliError { pub exit_code: u8, pub message: &'static str }

/// Physical copper layer numbers in UI order: front = 1, back = copper_layers.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct DrillSpan { pub first: u8, pub last: u8 }

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum DrillSide { Front, Back }

impl DrillSide {
    fn name(self) -> &'static str { match self { Self::Front => "front", Self::Back => "back" } }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum ViaProtection {
    Filling(DrillSpan), Capping(DrillSpan), Covering(DrillSide), Plugging(DrillSide), Tenting(DrillSide),
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum DrillClass {
    ThroughPlated,
    ThroughNonPlated,
    BlindBuried(DrillSpan),
    Backdrill(DrillSpan),
    Protection(ViaProtection),
}

pub type HolesByClass = BTreeMap<DrillClass, Vec<DrillHole>>;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DrillFileKind {
    Excellon, Gerber, Protection(ViaProtection), Map(DrillMapFormat), Report,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DrillFilePlan {
    /// Relative to output_dir, except an explicit report_filename (S46.R61).
    pub name: String,
    pub kind: DrillFileKind,
    pub classes: Vec<DrillClass>,
    pub hole_count: usize,
    pub span: Option<DrillSpan>,
    /// None means mixed plating, not an unknown plating status.
    pub plated: Option<bool>,
}

fn layer_name(layer: u8, copper_layers: u8) -> String {
    if layer == 1 { "front".into() } else if layer == copper_layers { "back".into() } else { format!("in{}", layer - 1) }
}

fn span_name(span: DrillSpan, copper_layers: u8) -> String {
    format!("{}-{}", layer_name(span.first, copper_layers), layer_name(span.last, copper_layers))
}

/// Plans the complete set in deterministic order: drills/protection, maps, report.
/// Hole geometry is borrowed later by class, not copied into every map/alias plan.
/// Planning visits classes only; writing sorts that file's holes in O(n log n).
/// Neither operation runs per frame.
pub fn drill_files(opts: &DrillExportOptions, holes: &HolesByClass) -> Vec<DrillFilePlan> {
    let gerber = opts.format == DrillFormat::Gerber;
    let separate = gerber || !opts.combine_pth_npth;
    let through = DrillSpan { first: 1, last: opts.copper_layers };
    let mut files = Vec::new();
    let mut maps = Vec::new();
    let mut add = |stem: String, classes: Vec<DrillClass>, span, plated, always, map| {
        let count = classes.iter().map(|c| holes.get(c).map_or(0, Vec::len)).sum();
        let plan = DrillFilePlan { name: String::new(), kind: if gerber { DrillFileKind::Gerber } else { DrillFileKind::Excellon }, classes,
            hole_count: count, span: Some(span), plated };
        if always && !gerber || count > 0 {
            files.push(DrillFilePlan { name: format!("{stem}{}", if gerber { "-drl.gbr" } else { ".drl" }), ..plan.clone() });
        }
        if opts.generate_map && map && (always || count > 0) {
            maps.push(DrillFilePlan { name: format!("{stem}-drl_map.{}", opts.map_format.extension()), kind: DrillFileKind::Map(opts.map_format), ..plan });
        }
    };
    if separate {
        add(format!("{}-PTH", opts.board_stem), vec![DrillClass::ThroughPlated], through, Some(true), true, true);
        add(format!("{}-NPTH", opts.board_stem), vec![DrillClass::ThroughNonPlated], through, Some(false), true, true);
    } else {
        add(opts.board_stem.clone(), vec![DrillClass::ThroughPlated, DrillClass::ThroughNonPlated], through, None, true, true);
    }
    for (&class, list) in holes {
        if list.is_empty() { continue; }
        match class {
            DrillClass::BlindBuried(span) => add(format!("{}-{}", opts.board_stem, span_name(span, opts.copper_layers)), vec![class], span, Some(true), false, true),
            DrillClass::Backdrill(span) => {
                add(format!("{}_Backdrills_Drill_{}_{}", opts.board_stem, span.first, span.last), vec![class], span, Some(true), false, true);
                // The layer-pair file aliases the same span; it does not add a second map.
                add(format!("{}-{}-backdrill", opts.board_stem, span_name(span, opts.copper_layers)), vec![class], span, Some(true), false, false);
            }
            _ => {}
        }
    }
    for (&class, list) in holes {
        if !gerber || list.is_empty() { continue; }
        let DrillClass::Protection(protection) = class else { continue; };
        let (suffix, span) = match protection {
            ViaProtection::Filling(s) => (format!("filling-{}", span_name(s, opts.copper_layers)), Some(s)),
            ViaProtection::Capping(s) => (format!("capping-{}", span_name(s, opts.copper_layers)), Some(s)),
            ViaProtection::Covering(s) => (format!("covering-{}", s.name()), None),
            ViaProtection::Plugging(s) => (format!("plugging-{}", s.name()), None),
            ViaProtection::Tenting(s) if opts.generate_tenting => (format!("tenting-{}", s.name()), None),
            ViaProtection::Tenting(_) => continue,
        };
        files.push(DrillFilePlan { name: format!("{}-{suffix}.gbr", opts.board_stem), kind: DrillFileKind::Protection(protection), classes: vec![class], hole_count: list.len(), span, plated: Some(true) });
    }
    files.extend(maps);
    if opts.generate_report {
        files.push(DrillFilePlan { name: opts.report_name(), kind: DrillFileKind::Report, classes: vec![], hole_count: 0, span: None, plated: None });
    }
    files
}

impl DrillFilePlan {
    /// Drill attributes only; map and protection backends choose their own file function.
    pub fn file_attrs(&self) -> Vec<FileAttr> {
        if !matches!(self.kind, DrillFileKind::Excellon | DrillFileKind::Gerber) { return vec![]; }
        let Some(span) = self.span else { return vec![]; };
        let (plating, hole_kind) = match self.plated {
            Some(true) => ("Plated", Some("PTH")),
            Some(false) => ("NonPlated", Some("NPTH")),
            None => ("MixedPlating", None),
        };
        let mut function = vec![plating.into(), span.first.to_string(), span.last.to_string()];
        if let Some(kind) = hole_kind { function.push(kind.into()); }
        if self.kind == DrillFileKind::Gerber { function.push("Drill".into()); }
        vec![("FileFunction".into(), function)]
    }

    /// S46.R64 message punctuation, for a host after a successful write.
    pub fn created_message(&self, resolved_path: &str) -> String {
        format!("Created file '{resolved_path}'{}", if self.kind == DrillFileKind::Excellon { "" } else { "." })
    }
}

/// Render a planned drill or report. Maps and protection drawings have separate S41
/// backends; fail explicitly if passed here instead of silently writing an empty file.
pub fn write_drill_file(plan: &DrillFilePlan, opts: &DrillExportOptions, holes: &HolesByClass) -> Result<Vec<u8>, CamError> {
    if plan.kind == DrillFileKind::Report { return Ok(drill_report(opts, holes).into_bytes()); }
    let mut selected: Vec<_> = plan.classes.iter().filter_map(|c| holes.get(c)).flatten().cloned().collect();
    match plan.kind {
        DrillFileKind::Excellon => Ok(write_excellon_with_options(&selected, &plan.file_attrs(), &opts.excellon_options())),
        DrillFileKind::Gerber => {
            selected.sort_by_key(|h| (h.dia, h.at.x, h.at.y, h.to.map(|p| (p.x, p.y))));
            let origin = opts.effective_origin();
            let transform = |p: P| -> Result<P, CamError> {
                let x = Nm::try_from(p.x as i64 - origin.x as i64);
                let y = Nm::try_from(origin.y as i64 - p.y as i64);
                match (x, y) { (Ok(x), Ok(y)) => Ok(P::new(x, y)), _ => Err(CamError::Geometry("drill coordinate exceeds board range after origin shift".into())) }
            };
            let mut items = Vec::with_capacity(selected.len());
            for h in selected {
                let at = transform(h.at)?;
                items.push(GItem::dark(match h.to {
                    Some(to) => Shape::Segment { a: at, b: transform(to)?, w: h.dia },
                    None => Shape::Circle { c: at, r: h.dia / 2 },
                }));
            }
            write_gerber(&items, &plan.file_attrs(), &GerberOpts { decimals: opts.effective_gerber_precision(), ..GerberOpts::default() })
        }
        _ => Err(CamError::Profile("map/protection plan requires its drawing backend".into())),
    }
}

/// S46.Q5/Q6: actual selected-format filenames, with no creation date or clock access.
pub fn drill_report(opts: &DrillExportOptions, holes: &HolesByClass) -> String {
    let mut text = format!("Drill report: {}\nGenerated by Fab {}\n", opts.board_stem, env!("CARGO_PKG_VERSION"));
    for file in drill_files(opts, holes) {
        if file.kind != DrillFileKind::Report {
            text.push_str(&format!("File: {}\nHoles: {}\n", file.name, file.hole_count));
        }
    }
    text
}

impl DrillExportOptions {
    /// Flat S46.R52 job object. Host context and Fab digit overrides are not job keys.
    pub fn to_job_json(&self) -> String {
        let mut fields = Vec::new();
        let mut put = |key: &str, value: String| fields.push(format!("  {}: {value}", json_quote(key)));
        put("description", json_quote(&self.description));
        put("output_dir", json_quote(&self.output_dir));
        put("excellon.mirror_y", self.mirror_y.to_string());
        put("excellon.minimal_header", self.minimal_header.to_string());
        put("excellon.combine_pth_npth", self.combine_pth_npth.to_string());
        put("excellon.oval_drill_route", (self.slots == SlotMode::Route).to_string());
        put("format", json_quote(if self.format == DrillFormat::Excellon { "excellon" } else { "gerber" }));
        put("drill_origin", json_quote(if self.origin == DrillOrigin::Absolute { "abs" } else { "plot" }));
        put("units", json_quote(if self.units == DrillUnits::Inches { "in" } else { "mm" }));
        put("zero_format", json_quote(match self.zeros {
            Zeros::Decimal => "decimal", Zeros::KeepTrailing => "surpress_leading",
            Zeros::KeepLeading => "surpress_trailing", Zeros::Keep => "keep_zeros",
        }));
        put("generate_map", self.generate_map.to_string());
        put("generate_tenting", self.generate_tenting.to_string());
        put("map_format", json_quote(self.map_format.job_name()));
        put("gerber_precision", self.effective_gerber_precision().to_string());
        put("generate_report", self.generate_report.to_string());
        put("report_filename", json_quote(&self.report_filename));
        format!("{{\n{}\n}}\n", fields.join(",\n"))
    }

    /// Missing keys use job defaults; unknown enum strings use that key's first entry.
    /// This reads the job's flat settings object, not a containing jobset document.
    pub fn from_job_json(json: &str) -> Result<Self, CamError> {
        let mut opts = Self::for_path(DrillPath::Job);
        for (key, value) in FlatJobJson::read(json)? {
            match key.as_str() {
                "description" => opts.description = value.string(&key)?.into(),
                "output_dir" => opts.output_dir = value.string(&key)?.into(),
                "report_filename" => opts.report_filename = value.string(&key)?.into(),
                "excellon.mirror_y" => opts.mirror_y = value.boolean(&key)?,
                "excellon.minimal_header" => opts.minimal_header = value.boolean(&key)?,
                "excellon.combine_pth_npth" => opts.combine_pth_npth = value.boolean(&key)?,
                "excellon.oval_drill_route" => opts.slots = if value.boolean(&key)? { SlotMode::Route } else { SlotMode::Alternate },
                "generate_map" => opts.generate_map = value.boolean(&key)?,
                "generate_tenting" => opts.generate_tenting = value.boolean(&key)?,
                "generate_report" => opts.generate_report = value.boolean(&key)?,
                "format" => opts.format = match value.string(&key)? { "gerber" => DrillFormat::Gerber, _ => DrillFormat::Excellon },
                "drill_origin" => opts.origin = match value.string(&key)? { "plot" => DrillOrigin::Plot, _ => DrillOrigin::Absolute },
                "units" => opts.units = match value.string(&key)? { "mm" => DrillUnits::Millimeters, _ => DrillUnits::Inches },
                "zero_format" => opts.zeros = match value.string(&key)? {
                    "surpress_leading" => Zeros::KeepTrailing, "surpress_trailing" => Zeros::KeepLeading,
                    "keep_zeros" => Zeros::Keep, _ => Zeros::Decimal,
                },
                "map_format" => opts.map_format = match value.string(&key)? {
                    "postscript" => DrillMapFormat::Postscript, "gerberx2" => DrillMapFormat::GerberX2,
                    "svg" => DrillMapFormat::Svg, "pdf" => DrillMapFormat::Pdf, _ => DrillMapFormat::Dxf,
                },
                "gerber_precision" => {
                    let JsonScalar::Number(number) = value else { return Err(CamError::Profile("gerber_precision must be an integer".into())); };
                    let precision: i64 = number.parse().map_err(|_| CamError::Profile("gerber_precision must be an integer".into()))?;
                    opts.gerber_precision = if precision == 6 { 6 } else { 5 };
                }
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

enum JsonScalar { String(String), Bool(bool), Number(String), Null }

impl JsonScalar {
    fn string(&self, key: &str) -> Result<&str, CamError> {
        match self { Self::String(s) => Ok(s), _ => Err(CamError::Profile(format!("{key} must be a string"))) }
    }
    fn boolean(&self, key: &str) -> Result<bool, CamError> {
        match self { Self::Bool(b) => Ok(*b), _ => Err(CamError::Profile(format!("{key} must be a boolean"))) }
    }
}

/// Dependency-free parser for this scalar, flat job schema. Rejects nested objects
/// (in particular `excellon: {...}`), malformed escapes, and trailing input.
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
                    Some('-' | '0'..='9') => JsonScalar::Number(p.number()?),
                    _ => return Err(p.error("expected a scalar drill job value")),
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
#[path = "drill_export_tests.rs"]
mod tests;

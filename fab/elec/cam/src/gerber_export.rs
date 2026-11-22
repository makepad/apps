//! S46.2 Gerber export settings and pure file/layer planning.
//!
//! The host owns board settings, jobset/CLI execution, geometry options and filesystem I/O.
//! JSON here is the settings object of a `gerbers` job, not the generated `.gbrjob` manifest.
//! No path expansion, clock, board mutation or external input is consulted. Work scales with
//! JSON size and layer selections; map insertion and sorting are O(n log n), never per frame.

use crate::error::CamError;
use crate::gerber_write::GerberOpts;
use fab_elec_core::{LayerId, LayerSet};
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum GerberJobPath {
    Board,
    Job,
    Cli,
}

/// Stored for shared plot settings only: Gerber never draws drill marks (S46.R49).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DrillShape {
    None,
    Small,
    Full,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GerberExportOptions {
    pub description: String,
    pub output_filename: String,
    pub layers: Vec<LayerId>,
    pub all_copper_layers: bool,
    pub layers_to_include_on_all_layers: Vec<LayerId>,
    pub use_protel_file_extension: bool,
    pub create_gerber_job_file: bool,
    pub precision: u8,
    pub use_x2_format: bool,
    pub include_netlist_attributes: bool,
    pub disable_aperture_macros: bool,
    pub use_drill_origin: bool,
    pub plot_drawing_sheet: bool,
    pub subtract_solder_mask_from_silk: bool,
    pub plot_footprint_values: bool,
    pub plot_ref_des: bool,
    pub sketch_pads_on_fab_layers: bool,
    pub plot_pad_numbers: bool,
    pub hide_dnp_footprints_on_fab_layers: bool,
    pub sketch_dnp_footprints_on_fab_layers: bool,
    pub crossout_dnp_footprints_on_fab_layers: bool,
    pub check_zones: bool,
    /// Empty means the host's current board variant.
    pub variant: String,
    /// Empty means the host's project drawing sheet.
    pub drawing_sheet: String,
    /// Project/jobset/CLI overrides, not keys of a `gerbers` settings object.
    pub text_variables: BTreeMap<String, String>,
    pub use_board_plot_params: bool,
    /// Retained for round trips, ignored by the Gerber writer (S46.R49 / Q4).
    pub mirror: bool,
    pub black_and_white: bool,
    pub negative: bool,
    pub drill_shape: DrillShape,
}

/// Defaults of the entry path, before the host resolves project/board state (S46.R28–R33).
pub fn defaults(path: GerberJobPath) -> GerberExportOptions {
    let board = path == GerberJobPath::Board;
    let mut layers = Vec::new();
    if board {
        layers = (1..=30).map(LayerId::inner).collect();
        layers.extend([
            LayerId::F_CU, LayerId::B_CU, LayerId::F_SILK, LayerId::B_SILK,
            LayerId::F_MASK, LayerId::B_MASK, LayerId::F_PASTE, LayerId::B_PASTE, LayerId::EDGE,
        ]);
        layers = ui_order(&layers);
    }
    GerberExportOptions {
        description: String::new(),
        output_filename: String::new(),
        layers,
        all_copper_layers: false,
        layers_to_include_on_all_layers: Vec::new(),
        use_protel_file_extension: !board,
        create_gerber_job_file: true,
        precision: if path == GerberJobPath::Job { 5 } else { 6 },
        use_x2_format: true,
        include_netlist_attributes: true,
        disable_aperture_macros: false,
        use_drill_origin: false,
        plot_drawing_sheet: false,
        subtract_solder_mask_from_silk: false,
        plot_footprint_values: true,
        plot_ref_des: true,
        sketch_pads_on_fab_layers: false,
        plot_pad_numbers: false,
        hide_dnp_footprints_on_fab_layers: false,
        sketch_dnp_footprints_on_fab_layers: path != GerberJobPath::Cli,
        crossout_dnp_footprints_on_fab_layers: path != GerberJobPath::Cli,
        check_zones: board,
        variant: String::new(),
        drawing_sheet: String::new(),
        text_variables: BTreeMap::new(),
        use_board_plot_params: false,
        mirror: false,
        black_and_white: board,
        negative: false,
        drill_shape: if board { DrillShape::Small } else { DrillShape::Full },
    }
}

impl GerberExportOptions {
    /// Only these four options affect the low-level writer. Other geometry options belong
    /// to the host's plot preparation. Mirror, drill marks, colour and negative never do.
    pub fn writer_options(&self) -> Result<GerberOpts, CamError> {
        Ok(GerberOpts {
            decimals: validate_precision(self.precision)?,
            x2: self.use_x2_format,
            netlist_attrs: self.include_netlist_attributes,
            no_aperture_macros: self.disable_aperture_macros,
        })
    }

    pub fn draws_pad_numbers(&self) -> bool {
        self.sketch_pads_on_fab_layers && self.plot_pad_numbers
    }
}

fn precision_error(value: impl std::fmt::Display) -> CamError {
    CamError::Profile(format!("Gerber coordinate precision should be either 5 or 6; got {value}"))
}

/// S46.R34, with main's stricter ruling on every path: never clamp or silently coerce.
pub fn validate_precision(value: u8) -> Result<u8, CamError> {
    match value {
        5 | 6 => Ok(value),
        _ => Err(precision_error(value)),
    }
}

/// Board stem, independent of host platform separators; the board name itself is not sanitized.
fn board_stem(board: &str) -> &str {
    let name = board.rsplit(['/', '\\']).next().unwrap_or(board);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}

/// S46.R45: `layer_name` is the resolved user/display name, not necessarily `layer.name()`.
/// `layer` determines the extension even when the user has renamed it.
pub fn gerber_file_name(board: &str, layer_name: &str, protel: bool, layer: LayerId) -> String {
    let suffix: String = layer_name.trim().chars().map(|c| {
        if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '%' | '.') { '_' } else { c }
    }).collect();
    let ext = if protel { protel_extension(layer) } else { "gbr".into() };
    let board = board_stem(board);
    if suffix.is_empty() { format!("{board}.{ext}") } else { format!("{board}-{suffix}.{ext}") }
}

pub fn job_file_name(board: &str) -> String {
    format!("{}-job.gbrjob", board_stem(board))
}

pub fn protel_extension(layer: LayerId) -> String {
    if let Some(n) = layer.inner_index() {
        return format!("g{n}"); // S46.Q3: In1.Cu is g1, not g2.
    }
    match layer {
        LayerId::F_CU => "gtl",
        LayerId::B_CU => "gbl",
        LayerId::F_ADHES => "gta",
        LayerId::B_ADHES => "gba",
        LayerId::F_PASTE => "gtp",
        LayerId::B_PASTE => "gbp",
        LayerId::F_SILK => "gto",
        LayerId::B_SILK => "gbo",
        LayerId::F_MASK => "gts",
        LayerId::B_MASK => "gbs",
        LayerId::EDGE => "gm1",
        _ => "gbr",
    }.into()
}

/// Standard display names for callers with no board-specific layer name (S46.R45).
pub fn display_layer_name(layer: LayerId) -> String {
    match layer {
        LayerId::F_SILK => "F.Silkscreen".into(),
        LayerId::B_SILK => "B.Silkscreen".into(),
        LayerId::F_ADHES => "F.Adhesive".into(),
        LayerId::B_ADHES => "B.Adhesive".into(),
        LayerId::F_CRTYD => "F.Courtyard".into(),
        LayerId::B_CRTYD => "B.Courtyard".into(),
        _ => layer.name(),
    }
}

fn ordered(layers: &[LayerId], key: impl Fn(&LayerId) -> u16) -> Vec<LayerId> {
    let mut out: Vec<_> = LayerSet::of(layers).iter().collect();
    out.sort_by_key(key);
    out
}

/// S46.R39 UI order, also the order in which layer files are written.
pub fn ui_order(layers: &[LayerId]) -> Vec<LayerId> {
    const TECH: [LayerId; 18] = [
        LayerId::F_ADHES, LayerId::B_ADHES, LayerId::F_PASTE, LayerId::B_PASTE,
        LayerId::F_SILK, LayerId::B_SILK, LayerId::F_MASK, LayerId::B_MASK,
        LayerId::DRAWINGS, LayerId::COMMENTS, LayerId::ECO1, LayerId::ECO2,
        LayerId::EDGE, LayerId::MARGIN, LayerId::F_CRTYD, LayerId::B_CRTYD,
        LayerId::F_FAB, LayerId::B_FAB,
    ];
    ordered(layers, |l| {
        if l.is_copper() { l.0 as u16 }
        else if let Some(n) = TECH.iter().position(|t| t == l) { 128 + n as u16 }
        else { 256 + l.0 as u16 }
    })
}

/// S46.R39 plotting stack, bottom to top through copper, then drawing/user/edge layers.
pub fn plotting_stack_order(layers: &[LayerId]) -> Vec<LayerId> {
    const BACK: [LayerId; 7] = [LayerId::B_CU, LayerId::B_MASK, LayerId::B_PASTE,
        LayerId::B_SILK, LayerId::B_ADHES, LayerId::B_CRTYD, LayerId::B_FAB];
    const FRONT: [LayerId; 11] = [LayerId::F_CU, LayerId::F_MASK, LayerId::F_PASTE,
        LayerId::F_SILK, LayerId::F_ADHES, LayerId::F_CRTYD, LayerId::F_FAB,
        LayerId::DRAWINGS, LayerId::COMMENTS, LayerId::ECO1, LayerId::ECO2];
    ordered(layers, |l| {
        if let Some(n) = BACK.iter().position(|t| t == l) { n as u16 }
        else if let Some(n) = l.inner_index() { 7 + (126 - n) as u16 }
        else if let Some(n) = FRONT.iter().position(|t| t == l) { 133 + n as u16 }
        else if l.0 >= LayerId::USER_BASE { 144 + (l.0 - LayerId::USER_BASE) as u16 }
        else if *l == LayerId::MARGIN { 240 }
        else if *l == LayerId::EDGE { 241 }
        else { 256 + l.0 as u16 }
    })
}

/// S46.R40. `enabled` includes only the board's actual copper and enabled technical layers.
/// CLI name resolution is the host's job: an explicitly invalid list must not call the
/// empty-list fallback here (S46.R42).
pub fn select_layers(path: GerberJobPath, options: &GerberExportOptions, enabled: &[LayerId]) -> Vec<LayerId> {
    let enabled = LayerSet::of(enabled);
    let mut chosen = LayerSet::of(&options.layers);
    match path {
        GerberJobPath::Board => {}
        GerberJobPath::Job => {
            if options.all_copper_layers { chosen = chosen.union(enabled.copper_only()); }
            else if chosen.is_empty() { chosen = enabled; }
        }
        GerberJobPath::Cli => {
            if options.use_board_plot_params {
                chosen = chosen.minus(LayerSet::ALL_COPPER).union(enabled.copper_only());
            } else if chosen.is_empty() { chosen = enabled; }
        }
    }
    ui_order(&chosen.intersect(enabled).iter().collect::<Vec<_>>())
}

/// S46.R41: the host still validates CLI precision even when board options replace it.
pub fn with_board_plot_params(board: &GerberExportOptions, cli: &GerberExportOptions) -> Result<GerberExportOptions, CamError> {
    validate_precision(cli.precision)?;
    validate_precision(board.precision)?;
    let mut out = board.clone();
    out.use_board_plot_params = true;
    out.create_gerber_job_file = true;
    out.check_zones = cli.check_zones;
    out.variant = cli.variant.clone();
    out.output_filename = cli.output_filename.clone();
    out.drawing_sheet = cli.drawing_sheet.clone();
    out.text_variables = cli.text_variables.clone();
    Ok(out)
}

/// S46.R43: own layer first; common layers retain list order with repeats removed.
pub fn plot_on_all_layers(own: LayerId, common: &[LayerId]) -> Vec<LayerId> {
    let mut seen = LayerSet::one(own);
    let mut out = vec![own];
    for &layer in common {
        if !seen.has(layer) {
            seen = seen.with(layer);
            out.push(layer);
        }
    }
    out
}

/// S46.R44: single-file export uses exactly the explicit order, with no common layers.
pub fn single_file_layers(layers: &[LayerId]) -> Result<Vec<LayerId>, CamError> {
    if layers.is_empty() {
        Err(CamError::Profile("At least one layer must be specified".into()))
    } else {
        Ok(layers.to_vec())
    }
}

/// S46.R29/R34 board-file layer numbers are NOT Fab's LayerId numbers.
fn numbered_layer(n: u8) -> Option<LayerId> {
    Some(match n {
        0 => LayerId::F_CU,
        2 => LayerId::B_CU,
        4..=62 if n % 2 == 0 => LayerId::inner((n - 2) / 2),
        1 => LayerId::F_MASK, 3 => LayerId::B_MASK,
        5 => LayerId::F_SILK, 7 => LayerId::B_SILK,
        9 => LayerId::F_ADHES, 11 => LayerId::B_ADHES,
        13 => LayerId::F_PASTE, 15 => LayerId::B_PASTE,
        17 => LayerId::DRAWINGS, 19 => LayerId::COMMENTS,
        21 => LayerId::ECO1, 23 => LayerId::ECO2,
        25 => LayerId::EDGE, 27 => LayerId::MARGIN,
        29 => LayerId::B_CRTYD, 31 => LayerId::F_CRTYD,
        33 => LayerId::B_FAB, 35 => LayerId::F_FAB,
        39..=127 if n % 2 == 1 => LayerId::user((n - 39) / 2),
        // Rescue has no corresponding Fab layer role.
        _ => return None,
    })
}

fn json_layers(value: &Json<'_>) -> Option<Vec<LayerId>> {
    let Json::Array(values) = value else { return None };
    Some(values.iter().filter_map(|v| match v {
        // R34 accepts board-file roles, not Fab's extra inner/user/unassigned identities.
        Json::String(name) => LayerId::parse(name).filter(|l| matches!(l.0, 0..=30 | 127..=145 | 160..=204)),
        Json::Number(n) => n.parse().ok().and_then(numbered_layer),
        _ => None,
    }).collect())
}

/// Read S46.R32 job settings. Unknown keys are ignored, unknown values keep the key's
/// job default; unknown layer entries are dropped (R34). Invalid precision is an error.
pub fn read_gerbers_job_json(text: &str) -> Result<GerberExportOptions, CamError> {
    let Json::Object(mut fields) = JsonReader::read(text)? else {
        return Err(CamError::parse(1, "gerbers settings must be a JSON object"));
    };
    let mut out = defaults(GerberJobPath::Job);
    macro_rules! bool_fields {
        ($($field:ident),* $(,)?) => { $(
            if let Some(Json::Bool(v)) = fields.get(stringify!($field)) { out.$field = *v; }
        )* };
    }
    bool_fields!(all_copper_layers, mirror, black_and_white, negative,
        plot_footprint_values, plot_ref_des, hide_dnp_footprints_on_fab_layers,
        sketch_dnp_footprints_on_fab_layers, crossout_dnp_footprints_on_fab_layers,
        sketch_pads_on_fab_layers, plot_pad_numbers, plot_drawing_sheet,
        subtract_solder_mask_from_silk, use_drill_origin, check_zones,
        include_netlist_attributes, use_x2_format, disable_aperture_macros,
        use_protel_file_extension, create_gerber_job_file);
    macro_rules! string_fields {
        ($($field:ident),* $(,)?) => { $(
            if let Some(Json::String(v)) = fields.remove(stringify!($field)) { out.$field = v; }
        )* };
    }
    string_fields!(description, output_filename, drawing_sheet, variant);
    if let Some(v) = fields.get("layers").and_then(json_layers) { out.layers = v; }
    if let Some(v) = fields.get("layers_to_include_on_all_layers").and_then(json_layers) {
        out.layers_to_include_on_all_layers = v;
    }
    if !fields.contains_key("all_copper_layers") {
        let layers = LayerSet::of(&out.layers);
        out.all_copper_layers = layers.has(LayerId::F_CU) && layers.has(LayerId::B_CU)
            && (1..=30).all(|n| layers.has(LayerId::inner(n)));
    }
    if let Some(Json::String(v)) = fields.get("drill_shape") {
        out.drill_shape = match v.as_str() {
            "none" => DrillShape::None,
            "small" => DrillShape::Small,
            _ => DrillShape::Full,
        };
    }
    if let Some(v) = fields.get("precision") {
        out.precision = match v {
            Json::Number("5") => 5,
            Json::Number("6") => 6,
            _ => return Err(precision_error(v.encode())),
        };
    }
    Ok(out)
}

/// Deterministic settings JSON, including `output_filename` (never `output_dir`).
/// Host-only project/CLI fields are deliberately not job settings keys (S46.R32).
pub fn write_gerbers_job_json(options: &GerberExportOptions) -> Result<String, CamError> {
    validate_precision(options.precision)?;
    let mut fields = Vec::new();
    macro_rules! field {
        ($name:ident, $value:expr) => { fields.push(format!("  \"{}\": {}", stringify!($name), $value)); };
    }
    macro_rules! strings {
        ($($name:ident),* $(,)?) => { $(field!($name, json_string(&options.$name));)* };
    }
    macro_rules! bools {
        ($($name:ident),* $(,)?) => { $(field!($name, options.$name);)* };
    }
    strings!(description, output_filename);
    let layers = |ls: &[LayerId]| format!("[{}]", ls.iter().map(|l| json_string(&l.name())).collect::<Vec<_>>().join(", "));
    field!(layers, layers(&options.layers));
    bools!(all_copper_layers);
    field!(layers_to_include_on_all_layers, layers(&options.layers_to_include_on_all_layers));
    bools!(mirror, black_and_white, negative, plot_footprint_values, plot_ref_des,
        hide_dnp_footprints_on_fab_layers, sketch_dnp_footprints_on_fab_layers,
        crossout_dnp_footprints_on_fab_layers, sketch_pads_on_fab_layers, plot_pad_numbers,
        plot_drawing_sheet, subtract_solder_mask_from_silk, use_drill_origin);
    field!(drill_shape, json_string(match options.drill_shape {
        DrillShape::None => "none", DrillShape::Small => "small", DrillShape::Full => "full",
    }));
    strings!(drawing_sheet);
    bools!(check_zones);
    strings!(variant);
    bools!(include_netlist_attributes, use_x2_format, disable_aperture_macros, use_protel_file_extension);
    field!(precision, options.precision);
    bools!(create_gerber_job_file);
    Ok(format!("{{\n{}\n}}\n", fields.join(",\n")))
}

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => { let _ = write!(out, "\\u{:04x}", c as u32); }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// The crate has no JSON dependency. This private parser accepts JSON values so future/unknown
// settings can be skipped, without treating nested keys as job settings. Number lexemes stay
// exact, including out-of-range precision values in error messages.
#[derive(Debug)]
enum Json<'a> {
    Null,
    Bool(bool),
    Number(&'a str),
    String(String),
    Array(Vec<Json<'a>>),
    Object(BTreeMap<String, Json<'a>>),
}

impl Json<'_> {
    fn encode(&self) -> String {
        match self {
            Self::Null => "null".into(),
            Self::Bool(v) => v.to_string(),
            Self::Number(v) => (*v).into(),
            Self::String(v) => json_string(v),
            Self::Array(v) => format!("[{}]", v.iter().map(Json::encode).collect::<Vec<_>>().join(",")),
            Self::Object(v) => format!("{{{}}}", v.iter().map(|(k, v)| format!("{}:{}", json_string(k), v.encode())).collect::<Vec<_>>().join(",")),
        }
    }
}

struct JsonReader<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> JsonReader<'a> {
    fn read(text: &'a str) -> Result<Json<'a>, CamError> {
        let mut r = Self { text, at: 0 };
        let value = r.value()?;
        r.space();
        if r.at != text.len() { return Err(r.error("trailing JSON content")); }
        Ok(value)
    }
    fn error(&self, msg: &str) -> CamError {
        CamError::parse(1 + self.text[..self.at].bytes().filter(|&c| c == b'\n').count(), msg)
    }
    fn peek(&self) -> Option<u8> { self.text.as_bytes().get(self.at).copied() }
    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) { self.at += 1; }
    }
    fn take(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) { self.at += 1; true } else { false }
    }
    fn expect(&mut self, byte: u8) -> Result<(), CamError> {
        if self.take(byte) { Ok(()) } else { Err(self.error("invalid JSON delimiter")) }
    }
    fn value(&mut self) -> Result<Json<'a>, CamError> {
        self.space();
        match self.peek() {
            Some(b'"') => self.string().map(Json::String),
            Some(b'{') => {
                self.at += 1;
                self.space();
                let mut fields = BTreeMap::new();
                if self.take(b'}') { return Ok(Json::Object(fields)); }
                loop {
                    let key = self.string()?;
                    self.space();
                    self.expect(b':')?;
                    fields.insert(key, self.value()?);
                    self.space();
                    if self.take(b'}') { break; }
                    self.expect(b',')?;
                    self.space();
                }
                Ok(Json::Object(fields))
            }
            Some(b'[') => {
                self.at += 1;
                self.space();
                let mut values = Vec::new();
                if self.take(b']') { return Ok(Json::Array(values)); }
                loop {
                    values.push(self.value()?);
                    self.space();
                    if self.take(b']') { break; }
                    self.expect(b',')?;
                }
                Ok(Json::Array(values))
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => {
                for (word, value) in [("true", Json::Bool(true)), ("false", Json::Bool(false)), ("null", Json::Null)] {
                    if self.text[self.at..].starts_with(word) { self.at += word.len(); return Ok(value); }
                }
                Err(self.error("expected JSON value"))
            }
        }
    }
    fn number(&mut self) -> Result<Json<'a>, CamError> {
        let start = self.at;
        self.take(b'-');
        if !self.take(b'0') {
            if !matches!(self.peek(), Some(b'1'..=b'9')) { return Err(self.error("invalid JSON number")); }
            self.digits();
        }
        if self.take(b'.') {
            if !self.digits() { return Err(self.error("invalid JSON fraction")); }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) { self.at += 1; }
            if !self.digits() { return Err(self.error("invalid JSON exponent")); }
        }
        Ok(Json::Number(&self.text[start..self.at]))
    }
    fn digits(&mut self) -> bool {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) { self.at += 1; }
        self.at > start
    }
    fn hex_unit(&mut self) -> Result<u16, CamError> {
        let mut n = 0;
        for _ in 0..4 {
            let Some(d) = self.peek().and_then(|b| (b as char).to_digit(16)) else {
                return Err(self.error("invalid JSON Unicode escape"));
            };
            n = (n << 4) | d as u16;
            self.at += 1;
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String, CamError> {
        self.expect(b'"')?;
        let mut out = String::new();
        while let Some(ch) = self.text[self.at..].chars().next() {
            self.at += ch.len_utf8();
            match ch {
                '"' => return Ok(out),
                '\\' => {
                    let Some(escape) = self.peek() else { return Err(self.error("unfinished JSON escape")); };
                    // Consume ASCII only; error locations must remain UTF-8 boundaries.
                    if !escape.is_ascii() { return Err(self.error("invalid JSON escape")); }
                    self.at += 1;
                    out.push(match escape {
                        b'"' => '"', b'\\' => '\\', b'/' => '/', b'b' => '\u{8}',
                        b'f' => '\u{c}', b'n' => '\n', b'r' => '\r', b't' => '\t',
                        b'u' => {
                            let first = self.hex_unit()?;
                            let code = if (0xd800..=0xdbff).contains(&first) {
                                self.expect(b'\\')?;
                                self.expect(b'u')?;
                                let second = self.hex_unit()?;
                                if !(0xdc00..=0xdfff).contains(&second) { return Err(self.error("invalid JSON surrogate pair")); }
                                0x10000 + (((first as u32 - 0xd800) << 10) | (second as u32 - 0xdc00))
                            } else { first as u32 };
                            char::from_u32(code).ok_or_else(|| self.error("invalid JSON Unicode scalar"))?
                        }
                        _ => return Err(self.error("invalid JSON escape")),
                    });
                }
                c if c < ' ' => return Err(self.error("unescaped JSON control character")),
                c => out.push(c),
            }
        }
        Err(self.error("unterminated JSON string"))
    }
}

#[cfg(test)]
#[path = "gerber_export_tests.rs"]
mod tests;

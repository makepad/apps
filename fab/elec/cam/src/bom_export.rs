//! S46.5 BOM options, tables and deterministic writers. The host owns file I/O,
//! project/environment expansion, UI confirmation and the S14 intermediate netlist.
//! No clock, environment, filesystem or process is consulted here. Work is per export:
//! grouping/sorting is O(n log n), scaled by field count and text length, never per frame.

use crate::error::CamError;
use crate::profile::{BomRow, BomTable};
use crate::tables::natural_cmp;
use crate::text::csv_cell_quoted;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BomPath { Dialog, Job, Cli }
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum BomSource { #[default] Schematic, Board }
impl BomSource {
    pub fn job_name(self) -> &'static str {
        match self { Self::Schematic => "sch_export_bom", Self::Board => "pcb_export_bom" }
    }
}
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum FilterScope { #[default] Reference, Visible, All }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SymbolScope {
    #[default]
    Project,
    Sheet(String),
    SheetAndChildren(String),
    Selection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BomField {
    pub name: String,
    pub label: String,
    pub show: bool,
    pub group_by: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BomView {
    pub fields: Vec<BomField>,
    pub sort_field: String,
    pub sort_asc: bool,
    pub filter_string: String,
    pub filter_scope: FilterScope,
    pub group_symbols: bool,
    pub exclude_dnp: bool,
    /// Retained for the editing view only. Export always omits BOM-excluded items.
    pub include_excluded_from_bom: bool,
}
impl Default for BomView {
    fn default() -> Self {
        Self { fields: Vec::new(), sort_field: String::new(), sort_asc: true,
            filter_string: String::new(), filter_scope: FilterScope::Reference,
            group_symbols: true, exclude_dnp: false, include_excluded_from_bom: false }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BomFormat {
    pub field_delimiter: String,
    pub string_delimiter: String,
    pub ref_delimiter: String,
    pub ref_range_delimiter: String,
    pub keep_tabs: bool,
    pub keep_line_breaks: bool,
    pub include_byte_order_mark: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BomExportOptions {
    pub path: BomPath,
    pub description: String,
    pub output_filename: String,
    pub fields_ordered: Vec<String>,
    pub fields_labels: Vec<String>,
    pub fields_group_by: Vec<String>,
    pub sort_field: String,
    pub sort_asc: bool,
    pub filter_string: String,
    pub filter_scope: FilterScope,
    pub group_symbols: bool,
    pub exclude_dnp: bool,
    pub include_excluded_from_bom: bool,
    pub bom_preset_name: String,
    pub bom_format_preset_name: String,
    pub format: BomFormat,
    pub variant_names: Vec<String>,
    pub symbol_scope: SymbolScope,
}

fn strings(values: &[&str]) -> Vec<String> { values.iter().map(|s| (*s).into()).collect() }

impl BomExportOptions {
    pub fn for_path(path: BomPath) -> Self {
        let mut out = Self {
            path, description: String::new(), output_filename: String::new(),
            fields_ordered: Vec::new(), fields_labels: Vec::new(), fields_group_by: Vec::new(),
            sort_field: String::new(), sort_asc: true, filter_string: String::new(),
            filter_scope: FilterScope::Reference, group_symbols: true, exclude_dnp: false,
            include_excluded_from_bom: false, bom_preset_name: String::new(),
            bom_format_preset_name: String::new(), format: BomFormat::default(),
            variant_names: Vec::new(), symbol_scope: SymbolScope::Project,
        };
        if path != BomPath::Job { out.format = builtin_format("CSV").unwrap(); }
        if path == BomPath::Dialog {
            out.apply_view(&builtin_view("Default Editing").unwrap());
            out.output_filename = "${PROJECTNAME}.csv".into();
        } else if path == BomPath::Cli {
            out.fields_ordered = strings(&["Reference", "Value", "Footprint", "QUANTITY", "DNP"]);
            out.fields_labels = strings(&["Refs", "Value", "Footprint", "Qty", "DNP"]);
            out.sort_field = "Reference".into();
            out.format.ref_range_delimiter = "-".into();
        }
        out
    }

    /// Also used when saving a view in job mode: hidden fields get `__`, names are cleared.
    pub fn apply_view(&mut self, view: &BomView) {
        self.fields_ordered = view.fields.iter().map(|f| {
            format!("{}{}", if f.show { "" } else { "__" }, f.name)
        }).collect();
        self.fields_labels = view.fields.iter().map(|f| f.label.clone()).collect();
        self.fields_group_by = view.fields.iter().filter(|f| f.group_by).map(|f| f.name.clone()).collect();
        self.sort_field = view.sort_field.clone();
        self.sort_asc = view.sort_asc;
        self.filter_string = view.filter_string.clone();
        self.filter_scope = view.filter_scope;
        self.group_symbols = view.group_symbols;
        self.exclude_dnp = view.exclude_dnp;
        self.include_excluded_from_bom = view.include_excluded_from_bom;
        self.bom_preset_name.clear();
    }

    /// Host CLI tokenization calls this once with the supplied lists; order of flags is irrelevant.
    pub fn set_cli_fields(&mut self, fields: Option<&str>, labels: Option<&str>, group_by: Option<&str>) {
        if let Some(fields) = fields {
            self.fields_ordered = fields.split(',').map(str::to_owned).collect();
            self.fields_labels.clear();
        }
        if let Some(labels) = labels { self.fields_labels = labels.split(',').map(str::to_owned).collect(); }
        if let Some(group_by) = group_by { self.fields_group_by = group_by.split(',').map(str::to_owned).collect(); }
    }

    pub fn set_sort_asc(&mut self, value: &str) -> Result<(), BomError> {
        self.sort_asc = match value {
            "true" => true, "false" => false,
            _ => return Err(BomError::new(1, "--sort-asc requires true or false")),
        };
        Ok(())
    }

    pub fn resolve(&self, document: &BomDocument, presets: &BomPresets) -> Result<ResolvedBomOptions, BomError> {
        let mut view = BomView {
            fields: resolve_fields(&self.fields_ordered, &self.fields_labels, &self.fields_group_by, &document.available_fields()),
            sort_field: normalize_field(&self.sort_field), sort_asc: self.sort_asc,
            filter_string: self.filter_string.clone(), filter_scope: self.filter_scope,
            group_symbols: self.group_symbols, exclude_dnp: self.exclude_dnp,
            include_excluded_from_bom: self.include_excluded_from_bom,
        };
        let mut format = self.format.clone();
        if self.path == BomPath::Cli {
            if !self.bom_preset_name.is_empty() {
                view = presets.views.get(&self.bom_preset_name).cloned()
                    .or_else(|| builtin_view(&self.bom_preset_name))
                    .ok_or_else(|| BomError::new(2, format!("BOM preset '{}' not found", self.bom_preset_name)))?;
            }
            if !self.bom_format_preset_name.is_empty() {
                format = presets.formats.get(&self.bom_format_preset_name).cloned()
                    .or_else(|| builtin_format(&self.bom_format_preset_name))
                    .ok_or_else(|| BomError::new(2, format!("BOM format preset '{}' not found", self.bom_format_preset_name)))?;
            }
        }
        Ok(ResolvedBomOptions { view, format, symbol_scope: if self.path == BomPath::Dialog {
            self.symbol_scope.clone()
        } else { SymbolScope::Project } })
    }
}
impl Default for BomExportOptions { fn default() -> Self { Self::for_path(BomPath::Dialog) } }

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BomPresets {
    pub views: BTreeMap<String, BomView>,
    pub formats: BTreeMap<String, BomFormat>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBomOptions {
    pub view: BomView,
    pub format: BomFormat,
    pub symbol_scope: SymbolScope,
}

pub fn builtin_format(name: &str) -> Option<BomFormat> {
    let (field, quote) = match name { "CSV" => (",", "\""), "TSV" => ("\t", ""), "Semicolons" => (";", "'"), _ => return None };
    Some(BomFormat { field_delimiter: field.into(), string_delimiter: quote.into(), ref_delimiter: ",".into(), ..Default::default() })
}

pub fn builtin_view(name: &str) -> Option<BomView> {
    let fields: &[(&str, &str, bool, bool)] = match name {
        "Default Editing" => &[
            ("Reference", "Reference", true, false), ("${QUANTITY}", "Qty", true, false),
            ("Value", "Value", true, true), ("${DNP}", "DNP", true, true),
            ("${EXCLUDE_FROM_BOM}", "Exclude from BOM", true, true),
            ("${EXCLUDE_FROM_BOARD}", "Exclude from Board", true, true),
            ("${EXCLUDE_FROM_SIM}", "Exclude from Simulation", true, true),
            ("${EXCLUDE_FROM_POS_FILES}", "Exclude from Position Files", true, true),
            ("Footprint", "Footprint", true, true), ("Datasheet", "Datasheet", true, false),
        ],
        "Grouped By Value" | "Grouped By Value and Footprint" => &[
            ("Reference", "Reference", true, false), ("Value", "Value", true, true),
            ("Datasheet", "Datasheet", true, false), ("Footprint", "Footprint", true, false),
            ("${QUANTITY}", "Qty", true, false), ("${DNP}", "DNP", true, true),
        ],
        "Attributes" => &[
            ("Reference", "Reference", true, false), ("Value", "Value", true, true),
            ("Datasheet", "Datasheet", false, false), ("Footprint", "Footprint", false, true),
            ("${DNP}", "Do Not Place", true, false), ("${EXCLUDE_FROM_BOM}", "Exclude from BOM", true, false),
            ("${EXCLUDE_FROM_BOARD}", "Exclude from Board", true, false),
            ("${EXCLUDE_FROM_SIM}", "Exclude from Simulation", true, false),
            ("${EXCLUDE_FROM_POS_FILES}", "Exclude from Position Files", true, false),
        ],
        _ => return None,
    };
    Some(BomView {
        fields: fields.iter().map(|&(field, label, show, group_by)| BomField {
            name: field.into(), label: label.into(), show,
            group_by: group_by || (name == "Grouped By Value and Footprint" && field == "Footprint"),
        }).collect(),
        sort_field: "Reference".into(),
        include_excluded_from_bom: matches!(name, "Default Editing" | "Attributes"),
        ..Default::default()
    })
}

fn normalize_field(name: &str) -> String {
    let name = name.strip_prefix("__").unwrap_or(name);
    match name {
        "QUANTITY" | "ITEM_NUMBER" | "DNP" | "EXCLUDE_FROM_BOM" | "EXCLUDE_FROM_BOARD" | "EXCLUDE_FROM_SIM" => format!("${{{name}}}"),
        _ => name.into(),
    }
}
fn field_label(name: &str) -> String {
    name.strip_prefix("${").and_then(|s| s.strip_suffix('}')).unwrap_or(name).into()
}

pub fn resolve_fields(ordered: &[String], labels: &[String], group_by: &[String], available: &[String]) -> Vec<BomField> {
    let explicit: BTreeSet<_> = ordered.iter().filter(|s| s.as_str() != "*").map(|s| normalize_field(s)).collect();
    let grouped: BTreeSet<_> = group_by.iter().filter(|s| !s.is_empty()).map(|s| normalize_field(s)).collect();
    let mut fields = Vec::new();
    let mut label_index = 0;
    for raw in ordered {
        if raw == "*" {
            for name in available {
                if !explicit.contains(name) && !fields.iter().any(|f: &BomField| f.name == *name) {
                    fields.push(BomField { name: name.clone(), label: name.clone(), show: true, group_by: false });
                }
            }
            continue;
        }
        let label = labels.get(label_index).filter(|s| !s.is_empty());
        label_index += 1;
        if raw.is_empty() { continue; }
        let name = normalize_field(raw);
        let label = label.cloned().unwrap_or_else(|| field_label(&name));
        fields.push(BomField { group_by: grouped.contains(&name), name, label, show: !raw.starts_with("__") });
    }
    for raw in group_by.iter().filter(|s| !s.is_empty()) {
        let name = normalize_field(raw);
        if let Some(field) = fields.iter_mut().find(|f| f.name == name) { field.group_by = true; }
        else { fields.push(BomField { label: field_label(&name), name, show: false, group_by: true }); }
    }
    fields
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BomAttributes {
    pub dnp: bool,
    pub exclude_from_bom: bool,
    pub exclude_from_board: bool,
    pub exclude_from_sim: bool,
    pub exclude_from_pos_files: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BomOverride {
    pub value: Option<String>,
    pub footprint: Option<String>,
    pub datasheet: Option<String>,
    pub description: Option<String>,
    pub fields: BTreeMap<String, String>,
    pub dnp: Option<bool>,
    pub exclude_from_bom: Option<bool>,
    pub exclude_from_board: Option<bool>,
    pub exclude_from_sim: Option<bool>,
    pub exclude_from_pos_files: Option<bool>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BomItem {
    /// Shared by units of one physical symbol; empty falls back to reference + channel.
    pub symbol_id: String,
    pub reference: String,
    /// Channel suffix, separate from the reference's trailing number, not a symbol unit letter.
    pub unit_suffix: String,
    pub value: String,
    /// Full library identifier, including the nickname when present.
    pub footprint: String,
    pub datasheet: String,
    pub description: String,
    pub fields: BTreeMap<String, String>,
    pub attributes: BomAttributes,
    pub variants: BTreeMap<String, BomOverride>,
    pub sheet_path: String,
    pub selected: bool,
    /// This footprint is physically present on the board.
    pub placed: bool,
}
impl BomItem {
    fn for_variant(&self, variant: &str) -> Self {
        let mut item = BomItem {
            symbol_id: self.symbol_id.clone(), reference: self.reference.clone(), unit_suffix: self.unit_suffix.clone(),
            value: self.value.clone(), footprint: self.footprint.clone(), datasheet: self.datasheet.clone(),
            description: self.description.clone(), fields: self.fields.clone(), attributes: self.attributes.clone(),
            sheet_path: self.sheet_path.clone(), selected: self.selected, placed: self.placed, variants: BTreeMap::new(),
        };
        if let Some(values) = self.variants.get(variant) {
            macro_rules! texts { ($($key:ident),*) => { $(if let Some(v) = &values.$key { item.$key = v.clone(); })* }; }
            macro_rules! attrs { ($($key:ident),*) => { $(if let Some(v) = values.$key { item.attributes.$key = v; })* }; }
            texts!(value, footprint, datasheet, description);
            attrs!(dnp, exclude_from_bom, exclude_from_board, exclude_from_sim, exclude_from_pos_files);
            item.fields.extend(values.fields.clone());
        }
        item
    }
    fn field(&self, name: &str) -> String {
        let flag = |v| if v { "1".into() } else { String::new() };
        match name {
            "Reference" => format!("{}{}", self.reference, self.unit_suffix),
            "Value" => self.value.clone(), "Footprint" => self.footprint.clone(),
            "Datasheet" => self.datasheet.clone(), "Description" => self.description.clone(),
            "${QUANTITY}" => "1".into(), "${ITEM_NUMBER}" => String::new(),
            "${DNP}" => flag(self.attributes.dnp),
            crate::tables::LCSC_PART => crate::tables::lcsc_number(&self.fields).into(),
            crate::tables::SUPPLIER_KEY => self.fields.get("supplier").map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default(),
            "${EXCLUDE_FROM_BOM}" => flag(self.attributes.exclude_from_bom),
            "${EXCLUDE_FROM_BOARD}" => flag(self.attributes.exclude_from_board),
            "${EXCLUDE_FROM_SIM}" => flag(self.attributes.exclude_from_sim),
            "${EXCLUDE_FROM_POS_FILES}" => flag(self.attributes.exclude_from_pos_files),
            _ => self.fields.get(name).cloned().unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BomDocument {
    pub source: BomSource,
    pub items: Vec<BomItem>,
    pub template_fields: Vec<String>,
    pub current_variant: String,
}
impl BomDocument {
    pub fn available_fields(&self) -> Vec<String> {
        let mut names = strings(&["Reference", "Value", "Footprint", "Datasheet", "Description"]);
        let mut custom: BTreeSet<String> = self.template_fields.iter().cloned().collect();
        for item in &self.items {
            custom.extend(item.fields.keys().cloned());
            for variant in item.variants.values() { custom.extend(variant.fields.keys().cloned()); }
        }
        for name in custom { if !names.contains(&name) { names.push(name); } }
        for name in ["${QUANTITY}", "${ITEM_NUMBER}", "${DNP}", "${EXCLUDE_FROM_BOM}", "${EXCLUDE_FROM_BOARD}", "${EXCLUDE_FROM_SIM}"] {
            if !names.iter().any(|n| n == name) { names.push(name.into()); }
        }
        names
    }
}

fn in_scope(item: &BomItem, scope: &SymbolScope) -> bool {
    match scope {
        SymbolScope::Project => true, SymbolScope::Selection => item.selected,
        SymbolScope::Sheet(path) => item.sheet_path == *path,
        SymbolScope::SheetAndChildren(path) => item.sheet_path == *path || item.sheet_path.strip_prefix(path.as_str())
            .is_some_and(|tail| path.ends_with('/') || tail.starts_with('/')),
    }
}
fn matches_filter(item: &BomItem, view: &BomView, all_fields: &[String]) -> bool {
    let needle = view.filter_string.to_lowercase();
    if needle.is_empty() { return true; }
    let matches = |field: &str| item.field(field).to_lowercase().contains(&needle);
    match view.filter_scope {
        FilterScope::Reference => matches("Reference"),
        FilterScope::Visible => view.fields.iter().filter(|f| f.show).any(|f| matches(&f.name)),
        FilterScope::All => all_fields.iter().any(|f| matches(f)),
    }
}
fn cmp_text(a: &str, b: &str) -> std::cmp::Ordering { natural_cmp(&a.to_lowercase(), &b.to_lowercase()) }

/// A table's cells and refs can also feed a factory profile; use `write_table` for reference quoting.
/// An empty variant is the base variant; the caller selects dialog current-variant via `plan_files`.
pub fn build_table(document: &BomDocument, options: &ResolvedBomOptions, variant: &str) -> BomTable {
    BomTable {
        header: options.view.fields.iter().filter(|f| f.show).map(|f| f.label.clone()).collect(),
        delimiter: options.format.field_delimiter.chars().next().unwrap_or(','),
        rows: build_rows(document, options, variant, |row, _| row),
    }
}

/// A live row retains the identities used by CAM, even when two symbols share a reference.
pub struct MemberRow {
    pub row: BomRow,
    pub paths: Vec<String>,
}

pub fn build_rows_with_paths(document: &BomDocument, options: &ResolvedBomOptions, variant: &str) -> Vec<MemberRow> {
    build_rows(document, options, variant, |row, group| MemberRow {
        row, paths: group.iter().map(|item| item.symbol_id.clone()).collect(),
    })
}

fn build_rows<R>(document: &BomDocument, options: &ResolvedBomOptions, variant: &str,
    finish: impl Fn(BomRow, &[&BomItem]) -> R) -> Vec<R> {
    let view = &options.view;
    let all_fields = document.available_fields();
    let mut seen = BTreeSet::new();
    let mut items: Vec<_> = document.items.iter().map(|i| i.for_variant(variant)).filter(|item| {
        if item.attributes.exclude_from_bom || (view.exclude_dnp && item.attributes.dnp)
            || (document.source == BomSource::Schematic && !in_scope(item, &options.symbol_scope)) || !matches_filter(item, view, &all_fields) { return false; }
        let identity = if item.symbol_id.is_empty() { (false, item.reference.clone(), item.unit_suffix.clone()) }
            else { (true, item.symbol_id.clone(), item.unit_suffix.clone()) };
        seen.insert(identity)
    }).collect();
    items.sort_by(|a, b| cmp_text(&a.field("Reference"), &b.field("Reference")));
    let keys: Vec<_> = view.fields.iter().filter(|f| f.group_by).map(|f| f.name.as_str()).collect();
    let mut groups: Vec<Vec<&BomItem>> = Vec::new();
    let mut group_indices = BTreeMap::new();
    for item in &items {
        if !view.group_symbols || keys.is_empty() { groups.push(vec![item]); continue; }
        let key: Vec<_> = keys.iter().map(|key| item.field(key)).collect();
        let index = *group_indices.entry(key).or_insert_with(|| { groups.push(Vec::new()); groups.len() - 1 });
        groups[index].push(item);
    }
    let sort_field = normalize_field(&view.sort_field);
    let sort_field = if view.fields.iter().any(|f| f.name == sort_field) { sort_field.as_str() } else { "Reference" };
    let cell = |group: &[&BomItem], name: &str| -> String {
        match name {
            "Reference" => reference_list(&group.iter().map(|i| BomReference { reference: i.reference.clone(), unit_suffix: i.unit_suffix.clone() }).collect::<Vec<_>>(), &options.format),
            "${QUANTITY}" => group.len().to_string(),
            // Assembly designators enumerate every fitted member, including repeated display refs.
            crate::tables::ASSEMBLY_REFS => group.iter().map(|i| i.reference.as_str()).collect::<Vec<_>>().join(","),
            // Group identity uses the specified footprint. Factory files show a package
            // only if at least one member has actually been placed with that footprint.
            crate::tables::PLACED_FOOTPRINT => group.iter().find(|i| i.placed).map(|i| i.footprint.clone()).unwrap_or_default(),
            "" => String::new(),
            _ => {
                let mut seen = BTreeSet::new();
                group.iter().map(|item| item.field(name)).filter(|value| seen.insert(value.clone())).collect::<Vec<_>>().join(",")
            }
        }
    };
    // Cache sort strings once: formatting a group never happens inside the comparator.
    let mut sorted: Vec<_> = groups.into_iter().map(|g| (cell(&g, sort_field), g)).collect();
    sorted.sort_by(|(a, ga), (b, gb)| {
        let order = cmp_text(a, b).then_with(|| cmp_text(&ga[0].field("Reference"), &gb[0].field("Reference")));
        if view.sort_asc { order } else { order.reverse() }
    });
    sorted.iter().enumerate().map(|(index, (_, group))| {
        let row = BomRow {
            refs: group.iter().map(|item| item.field("Reference")).collect(),
            cells: view.fields.iter().filter(|f| f.show).map(|f| {
                if f.name == "${ITEM_NUMBER}" { (index + 1).to_string() } else { cell(group, &f.name) }
            }).collect(),
        };
        finish(row, group)
    }).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BomReference { pub reference: String, pub unit_suffix: String }
fn reference_number(reference: &str) -> Option<(&str, &str)> {
    let start = reference.trim_end_matches(|c: char| c.is_ascii_digit() || c == '?').len();
    let number = &reference[start..];
    if number.is_empty() || number.contains('?') { return None; }
    let number = number.trim_start_matches('0');
    Some((&reference[..start], if number.is_empty() { "0" } else { number }))
}
fn successor(number: &str) -> String {
    let mut digits = number.as_bytes().to_vec();
    for digit in digits.iter_mut().rev() {
        if *digit != b'9' { *digit += 1; return String::from_utf8(digits).unwrap(); }
        *digit = b'0';
    }
    digits.insert(0, b'1');
    String::from_utf8(digits).unwrap()
}

/// S46.R93: preserve item order within each channel, channels in first-appearance order.
pub fn reference_list(references: &[BomReference], format: &BomFormat) -> String {
    let mut channels: Vec<Vec<&BomReference>> = Vec::new();
    let mut channel_indices = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for reference in references {
        if !seen.insert((&reference.reference, &reference.unit_suffix)) { continue; }
        let index = *channel_indices.entry(&reference.unit_suffix).or_insert_with(|| { channels.push(Vec::new()); channels.len() - 1 });
        channels[index].push(reference);
    }
    let display = |r: &BomReference| format!("{}{}", r.reference, r.unit_suffix);
    let mut out = Vec::new();
    for channel in channels {
        let mut start = 0;
        while start < channel.len() {
            let mut end = start + 1;
            if !format.ref_range_delimiter.is_empty() {
                while end < channel.len() {
                    let (Some((ap, an)), Some((bp, bn))) = (reference_number(&channel[end - 1].reference), reference_number(&channel[end].reference)) else { break; };
                    if ap != bp || successor(an) != bn { break; }
                    end += 1;
                }
            }
            if end - start >= 3 { out.push(format!("{}{}{}", display(channel[start]), format.ref_range_delimiter, display(channel[end - 1]))); }
            else { out.extend(channel[start..end].iter().map(|r| display(r))); }
            start = end;
        }
    }
    out.join(&format.ref_delimiter)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BomOutputFormat {
    /// S46.R91 text, including CSV, TSV and custom delimiters.
    Delimited,
    /// Fab table formats, not the S14 generator intermediate netlist.
    Html,
    Xml,
}

pub fn write_table(table: &BomTable, format: &BomFormat, output: BomOutputFormat) -> String {
    if table.header.is_empty() { return String::new(); }
    let mut out = if format.include_byte_order_mark { "\u{feff}".into() } else { String::new() };
    let clean = |value: &str| -> String { value.chars().filter(|c| {
        (format.keep_line_breaks || !matches!(c, '\r' | '\n')) && (format.keep_tabs || *c != '\t')
    }).collect() };
    match output {
        BomOutputFormat::Delimited => {
            for row in std::iter::once(&table.header).chain(table.rows.iter().map(|r| &r.cells)) {
                let cells: Vec<_> = row.iter().map(|cell| csv_cell_quoted(cell, &format.string_delimiter, !format.keep_line_breaks, !format.keep_tabs)).collect();
                out.push_str(&cells.join(&format.field_delimiter));
                out.push('\n');
            }
        }
        BomOutputFormat::Html => {
            out.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>Bill of materials</title></head><body>\n<table>\n<thead><tr>");
            for label in &table.header { let _ = write!(out, "<th>{}</th>", markup_escape(&clean(label))); }
            out.push_str("</tr></thead>\n<tbody>\n");
            for row in &table.rows {
                out.push_str("<tr>");
                for cell in &row.cells { let _ = write!(out, "<td>{}</td>", markup_escape(&clean(cell))); }
                out.push_str("</tr>\n");
            }
            out.push_str("</tbody>\n</table>\n</body></html>\n");
        }
        BomOutputFormat::Xml => {
            out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<bom generator=\"Fab\">\n  <columns>");
            for label in &table.header { let _ = write!(out, "<column>{}</column>", markup_escape(&clean(label))); }
            out.push_str("</columns>\n");
            for row in &table.rows {
                out.push_str("  <row>");
                for cell in &row.cells { let _ = write!(out, "<cell>{}</cell>", markup_escape(&clean(cell))); }
                out.push_str("</row>\n");
            }
            out.push_str("</bom>\n");
        }
    }
    out
}
fn markup_escape(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"), '<' => out.push_str("&lt;"), '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"), '\'' => out.push_str("&apos;"),
            // XML 1.0 cannot represent these characters, even with numeric escapes.
            c if (c < ' ' && !matches!(c, '\t' | '\r' | '\n')) || matches!(c, '\u{fffe}' | '\u{ffff}') => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
    out
}

/// S46.R97. Base data only, resolved via the host's pure text-variable resolver.
pub fn write_board_legacy(document: &BomDocument, resolve: impl Fn(&str) -> String) -> Result<String, BomError> {
    if document.items.is_empty() { return Err(BomError::new(2, "Cannot export BOM: there are no footprints on the PCB.")); }
    let mut groups: Vec<(String, String, Vec<String>)> = Vec::new();
    let mut indices = BTreeMap::new();
    for item in document.items.iter().filter(|i| !i.attributes.exclude_from_bom) {
        let key = (item.value.clone(), item.footprint.clone());
        if let Some(&index) = indices.get(&key) {
            let group: &mut (String, String, Vec<String>) = &mut groups[index];
            group.2.push(resolve(&item.reference));
        } else {
            let value = resolve(&item.value);
            // Compare subsequent stored values against the first resolved value (R97.5).
            indices.entry((value.clone(), item.footprint.clone())).or_insert(groups.len());
            groups.push((value, item.footprint.clone(), vec![resolve(&item.reference)]));
        }
    }
    for (_, _, refs) in &mut groups { refs.sort_by(|a, b| cmp_text(a, b)); }
    groups.sort_by(|a, b| cmp_text(&a.2[0], &b.2[0]));
    let mut out = String::from("\"Id\";\"Designator\";\"Footprint\";\"Quantity\";\"Designation\";\"Supplier and ref\";\n");
    for (index, (value, footprint, refs)) in groups.iter().enumerate() {
        let item = footprint.rsplit(':').next().unwrap_or(footprint);
        let _ = writeln!(out, "{};\"{}\";\"{}\";{};\"{}\";;;", index + 1, refs.join(", "), item, refs.len(), value);
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BomError { pub exit_code: u8, pub message: String }
impl BomError {
    fn new(exit_code: u8, message: impl Into<String>) -> Self { Self { exit_code, message: message.into() } }
}
impl std::fmt::Display for BomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
}
impl std::error::Error for BomError {}

/// Resolved host context. Environment/project expansion is supplied, never read by CAM.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BomPathContext {
    pub design_file: String,
    pub job_output_dir: String,
    pub current_dir: String,
    pub variables: BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BomFilePlan { pub variant: String, pub path: String }
fn file_stem(path: &str) -> &str {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    file.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()).map_or(file, |(stem, _)| stem)
}
fn parent_dir(path: &str) -> &str { path.rfind(['/', '\\']).map_or("", |i| &path[..=i]) }
fn join_path(folder: &str, file: &str) -> String {
    if folder.is_empty() { file.into() }
    else { format!("{}{file}", if folder.ends_with(['/', '\\']) { folder.into() } else { format!("{folder}/") }) }
}
fn absolute_path(path: &str) -> bool {
    path.starts_with(['/', '\\']) || (path.as_bytes().get(1) == Some(&b':') && path.as_bytes().get(2).is_some_and(|b| matches!(b, b'/' | b'\\')))
}
fn expand_variables(text: &str, variables: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let token = &rest[start..];
        let Some(end) = token.find('}') else { out.push_str(token); return out; };
        out.push_str(variables.get(&token[2..end]).map(String::as_str).unwrap_or(&token[..=end]));
        rest = &token[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Existing-directory checks and creation belong to the host. The callback sees the resolved path.
pub fn plan_files(options: &BomExportOptions, document: &BomDocument, context: &BomPathContext,
    is_directory: impl Fn(&str) -> bool) -> Result<Vec<BomFilePlan>, BomError> {
    if options.path == BomPath::Cli && options.variant_names.len() > 1 && !options.output_filename.contains("${VARIANT}") {
        return Err(BomError::new(1, "When specifying multiple variants, the output path must contain ${VARIANT} to generate separate output files for each variant."));
    }
    let variants = if options.variant_names.is_empty() {
        vec![if options.path == BomPath::Dialog { document.current_variant.clone() } else { String::new() }]
    } else if options.path != BomPath::Cli { vec![options.variant_names[0].clone()] }
    else { options.variant_names.clone() };
    let default = format!("{}.csv", file_stem(&context.design_file));
    let base = match options.path { BomPath::Dialog => parent_dir(&context.design_file), BomPath::Job => &context.job_output_dir, BomPath::Cli => &context.current_dir };
    Ok(variants.into_iter().map(|variant| {
        let mut variables = context.variables.clone();
        variables.insert("VARIANT".into(), variant.clone());
        let output = if options.output_filename.is_empty() { default.clone() } else { expand_variables(&options.output_filename, &variables) };
        let mut path = if absolute_path(&output) { output } else { join_path(base, &output) };
        if path.ends_with(['/', '\\']) || is_directory(&path) { path = join_path(&path, &default); }
        BomFilePlan { variant, path }
    }).collect())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BomOutputFile { pub plan: BomFilePlan, pub data: Vec<u8> }

/// Prepare all files before host I/O. Job and CLI paths use the same deterministic writer;
/// the variant from each plan drives values and flags, never just the output filename.
pub fn export_bom(options: &BomExportOptions, document: &BomDocument, presets: &BomPresets,
    context: &BomPathContext, output: BomOutputFormat, is_directory: impl Fn(&str) -> bool,
) -> Result<Vec<BomOutputFile>, BomError> {
    let resolved = options.resolve(document, presets)?;
    plan_files(options, document, context, is_directory).map(|plans| plans.into_iter().map(|plan| {
        let table = build_table(document, &resolved, &plan.variant);
        let data = write_table(&table, &resolved.format, output).into_bytes();
        BomOutputFile { plan, data }
    }).collect())
}

/// Map an existing Fabricate profile's resolved columns/cells onto the reference writer.
/// The caller opts in explicitly; `model::render` retains its existing factory CSV contract.
pub fn write_reference_profile(table: &BomTable) -> String {
    let mut format = builtin_format("CSV").unwrap();
    format.field_delimiter = table.delimiter.to_string();
    write_table(table, &format, BomOutputFormat::Delimited)
}

/// S14 owns XML netlist bytes. This function plans python-bom's destination, never a command.
pub fn python_bom_path(output: &str, context: &BomPathContext, is_directory: impl Fn(&str) -> bool) -> String {
    let default = format!("{}-bom.xml", file_stem(&context.design_file));
    let output = if output.is_empty() { default.clone() } else { expand_variables(output, &context.variables) };
    let path = if absolute_path(&output) { output } else { join_path(&context.current_dir, &output) };
    if path.ends_with(['/', '\\']) || is_directory(&path) { join_path(&path, &default) } else { path }
}

pub const GENERATOR_FILE_ERROR: &str = "Failed to create file.";
pub fn python_bom_input_error() -> BomError {
    BomError::new(3, "Schematic file does not exist or is not accessible")
}

pub const UNSAVED_EDITS_MESSAGE: &str = "Changes have not yet been saved. Export unsaved data?";
pub const INCLUDE_EXCLUDED_NOTICE: &str = "--include-excluded-from-bom is deprecated and has no effect.";
pub fn success_message(path: BomPath, output: &str) -> String {
    if path == BomPath::Dialog { format!("Wrote BOM output to '{output}'") }
    else { format!("Wrote bill of materials to '{output}'.") }
}
pub fn annotation_warnings(source: BomSource, annotation_errors: bool, duplicate_sheets: bool) -> Vec<&'static str> {
    let mut out = Vec::new();
    if annotation_errors { out.push(match source {
        BomSource::Schematic => "Warning: schematic has annotation errors, please use the schematic editor to fix them",
        BomSource::Board => "Warning: board has unannotated footprints, please use the PCB editor to annotate them",
    }); }
    if source == BomSource::Schematic && duplicate_sheets { out.push("Warning: duplicate sheet names."); }
    out
}
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BomIoFailure { Input, Directory, Open, Write }
pub fn io_error(path: BomPath, failure: BomIoFailure, output: &str) -> BomError {
    let code = match failure { BomIoFailure::Input | BomIoFailure::Open => 3, BomIoFailure::Directory => 4, BomIoFailure::Write => 2 };
    let message = match (path, failure) {
        (_, BomIoFailure::Input) => "Input file does not exist or is not accessible".into(),
        (BomPath::Dialog, BomIoFailure::Directory) => format!("Could not open/create path '{output}'."),
        (BomPath::Dialog, BomIoFailure::Open) => format!("Could not create BOM output '{output}'."),
        (BomPath::Dialog, BomIoFailure::Write) => format!("Could not write BOM output '{output}'."),
        (_, BomIoFailure::Directory) => "Failed to create output directory".into(),
        (_, BomIoFailure::Open) => format!("Unable to open destination '{output}'"),
        (_, BomIoFailure::Write) => format!("Could not write BOM output '{output}'."),
    };
    BomError::new(code, message)
}

/// S46.R99: only the host's user application settings may construct this value.
/// There is intentionally no project/job/board deserializer for commands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GeneratorSettings { pub commands: Vec<GeneratorCommand> }
impl GeneratorSettings {
    /// Only this app-settings boundary exposes expanded commands; export/job APIs cannot do so.
    pub fn preview(&self, index: usize, path: BomPath, context: &GeneratorContext) -> Result<GeneratorPreview, BomError> {
        self.commands.get(index).ok_or_else(|| BomError::new(2, "BOM generator not found in user settings"))?.preview(path, context)
    }
    /// The host records confirmation only after showing the exact preview and receiving consent.
    pub fn record_confirmation(&mut self, index: usize, context: &GeneratorContext, shown: &str) -> Result<(), BomError> {
        self.commands.get_mut(index).ok_or_else(|| BomError::new(2, "BOM generator not found in user settings"))?.record_confirmation(context, shown)
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GeneratorCommand {
    pub nickname: String,
    command: String,
    confirmed: bool,
    confirmed_expansion: String,
}
impl GeneratorCommand {
    pub fn new(nickname: impl Into<String>, command: impl Into<String>) -> Self {
        Self { nickname: nickname.into(), command: command.into(), ..Default::default() }
    }
    pub fn command(&self) -> &str { &self.command }
    /// Recorded template confirmation; use `GeneratorSettings::preview().confirmed` for current substitutions.
    pub fn confirmed(&self) -> bool { self.confirmed }
    pub fn set_command(&mut self, command: impl Into<String>) {
        let command = command.into();
        if command != self.command { self.command = command; self.confirmed = false; self.confirmed_expansion.clear(); }
    }
    /// Call only after the host displays and the user confirms this exact expanded text.
    fn record_confirmation(&mut self, context: &GeneratorContext, shown: &str) -> Result<(), BomError> {
        let preview = self.preview(BomPath::Dialog, context)?;
        if shown != preview.command { return Err(BomError::new(2, "Generator command changed; confirmation required")); }
        self.confirmed_expansion = shown.into();
        self.confirmed = true;
        Ok(())
    }
    fn preview(&self, path: BomPath, context: &GeneratorContext) -> Result<GeneratorPreview, BomError> {
        if path != BomPath::Dialog { return Err(BomError::new(2, "Jobs and the command line cannot run BOM generators")); }
        if !context.fully_annotated { return Err(BomError::new(2, "Generating BOM requires a fully annotated schematic.")); }
        if self.command.trim().is_empty() || self.command.contains('\0') { return Err(BomError::new(2, "Invalid BOM generator command")); }
        let stem = file_stem(&context.schematic_file);
        let output = join_path(parent_dir(&context.schematic_file), stem);
        let intermediate = format!("{output}.xml");
        let windows_output = if context.windows && !output.starts_with("\\\\") { output.replace('\\', "/") } else { output.clone() };
        let project = context.project_folder.trim_end_matches(['/', '\\']);
        let mut command = String::new();
        let mut chars = self.command.chars();
        while let Some(ch) = chars.next() {
            if ch != '%' { command.push(ch); continue; }
            match chars.next() {
                Some('P') => command.push_str(project), Some('B') => command.push_str(stem),
                Some('I') => command.push_str(&intermediate), Some('O') => command.push_str(&windows_output),
                Some(other) => { command.push('%'); command.push(other); }, None => command.push('%'),
            }
        }
        Ok(GeneratorPreview { confirmed: self.confirmed && self.confirmed_expansion == command, command, intermediate_file: intermediate })
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GeneratorContext {
    pub project_folder: String,
    pub schematic_file: String,
    pub fully_annotated: bool,
    pub windows: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratorPreview {
    /// Exact text to display. This crate never executes it, even when confirmed.
    pub command: String,
    pub confirmed: bool,
    pub intermediate_file: String,
}

fn json_object(value: &Json) -> Result<&BTreeMap<String, Json>, CamError> {
    match value { Json::Object(object) => Ok(object), _ => Err(CamError::parse(1, "expected BOM settings object")) }
}
fn setting_string(object: &BTreeMap<String, Json>, key: &str) -> Result<Option<String>, CamError> {
    match object.get(key) {
        None => Ok(None), Some(Json::String(value)) => Ok(Some(value.clone())),
        _ => Err(CamError::parse(1, format!("BOM setting '{key}' must be a string"))),
    }
}
fn setting_bool(object: &BTreeMap<String, Json>, key: &str) -> Result<Option<bool>, CamError> {
    match object.get(key) {
        None => Ok(None), Some(Json::Bool(value)) => Ok(Some(*value)),
        _ => Err(CamError::parse(1, format!("BOM setting '{key}' must be a boolean"))),
    }
}
fn setting_strings(object: &BTreeMap<String, Json>, key: &str) -> Result<Option<Vec<String>>, CamError> {
    match object.get(key) {
        None => Ok(None), Some(Json::Array(values)) => values.iter().map(|v| match v {
            Json::String(value) => Ok(value.clone()), _ => Err(CamError::parse(1, format!("BOM setting '{key}' must be an array of strings"))),
        }).collect::<Result<Vec<_>, _>>().map(Some),
        _ => Err(CamError::parse(1, format!("BOM setting '{key}' must be an array of strings"))),
    }
}
fn filter_scope(value: &str) -> Result<FilterScope, CamError> {
    match value { "reference" => Ok(FilterScope::Reference), "visible" => Ok(FilterScope::Visible), "all" => Ok(FilterScope::All),
        _ => Err(CamError::parse(1, "BOM filter_scope must be reference, visible or all")) }
}
fn scope_name(value: FilterScope) -> &'static str {
    match value { FilterScope::Reference => "reference", FilterScope::Visible => "visible", FilterScope::All => "all" }
}
fn read_format(object: &BTreeMap<String, Json>) -> Result<BomFormat, CamError> {
    let mut out = BomFormat::default();
    macro_rules! texts { ($($key:ident),*) => { $(if let Some(value) = setting_string(object, stringify!($key))? { out.$key = value; })* }; }
    macro_rules! bools { ($($key:ident),*) => { $(if let Some(value) = setting_bool(object, stringify!($key))? { out.$key = value; })* }; }
    texts!(field_delimiter, string_delimiter, ref_delimiter, ref_range_delimiter);
    bools!(keep_tabs, keep_line_breaks, include_byte_order_mark);
    Ok(out)
}

/// S46.R85/R88/R99: whitelist only data keys. Preset names, commands, generator lists,
/// confirmation flags and nested settings are never read as executable/user settings.
pub fn read_bom_job_json(text: &str) -> Result<BomExportOptions, CamError> {
    let value = JsonReader::read(text)?;
    let object = json_object(&value)?;
    let mut out = BomExportOptions::for_path(BomPath::Job);
    macro_rules! texts { ($($key:ident),*) => { $(if let Some(value) = setting_string(object, stringify!($key))? { out.$key = value; })* }; }
    macro_rules! bools { ($($key:ident),*) => { $(if let Some(value) = setting_bool(object, stringify!($key))? { out.$key = value; })* }; }
    macro_rules! lists { ($($key:ident),*) => { $(if let Some(value) = setting_strings(object, stringify!($key))? { out.$key = value; })* }; }
    texts!(description, output_filename, sort_field, filter_string);
    bools!(sort_asc, group_symbols, exclude_dnp);
    lists!(fields_ordered, fields_labels, fields_group_by, variant_names);
    if let Some(scope) = setting_string(object, "filter_scope")? { out.filter_scope = filter_scope(&scope)?; }
    out.format = read_format(object)?;
    Ok(out)
}

/// Saving editor job settings includes hidden columns and every delimiter; named presets are cleared.
pub fn write_bom_job_json(options: &BomExportOptions) -> String {
    let mut fields = Vec::new();
    macro_rules! field { ($key:ident, $value:expr) => { fields.push(format!("  \"{}\": {}", stringify!($key), $value)); }; }
    macro_rules! texts { ($($key:ident),*) => { $(field!($key, json_string(&options.$key));)* }; }
    macro_rules! bools { ($($key:ident),*) => { $(field!($key, options.$key);)* }; }
    macro_rules! lists { ($($key:ident),*) => { $(field!($key, format!("[{}]", options.$key.iter().map(|s| json_string(s)).collect::<Vec<_>>().join(", ")));)* }; }
    texts!(description, output_filename, sort_field, filter_string);
    bools!(sort_asc, group_symbols, exclude_dnp);
    lists!(fields_ordered, fields_labels, fields_group_by, variant_names);
    field!(filter_scope, json_string(scope_name(options.filter_scope)));
    field!(bom_preset_name, "\"\"");
    field!(bom_format_preset_name, "\"\"");
    for (key, value) in [
        ("field_delimiter", &options.format.field_delimiter), ("string_delimiter", &options.format.string_delimiter),
        ("ref_delimiter", &options.format.ref_delimiter), ("ref_range_delimiter", &options.format.ref_range_delimiter),
    ] { fields.push(format!("  {}: {}", json_string(key), json_string(value))); }
    field!(keep_tabs, options.format.keep_tabs);
    field!(keep_line_breaks, options.format.keep_line_breaks);
    field!(include_byte_order_mark, options.format.include_byte_order_mark);
    format!("{{\n{}\n}}\n", fields.join(",\n"))
}

/// The host supplies the schematic or board.design_settings object, not an app settings file.
/// Incomplete user presets are skipped (R87); all unrelated keys, including commands, are ignored.
pub fn read_bom_presets_json(text: &str) -> Result<BomPresets, CamError> {
    let value = JsonReader::read(text)?;
    let object = json_object(&value)?;
    let mut out = BomPresets::default();
    if let Some(Json::Array(presets)) = object.get("bom_presets") {
        for preset in presets {
            if let Ok((name, view)) = read_view_preset(preset) { out.views.insert(name, view); }
        }
    }
    if let Some(Json::Array(presets)) = object.get("bom_fmt_presets") {
        for preset in presets {
            let Ok(object) = json_object(preset) else { continue; };
            let Ok(Some(name)) = setting_string(object, "name") else { continue; };
            if ["field_delimiter", "string_delimiter", "ref_delimiter", "ref_range_delimiter", "keep_tabs", "keep_line_breaks"]
                .iter().any(|key| !object.contains_key(*key)) { continue; }
            if let Ok(format) = read_format(object) { out.formats.insert(name, format); }
        }
    }
    Ok(out)
}
fn read_view_preset(value: &Json) -> Result<(String, BomView), CamError> {
    let object = json_object(value)?;
    let missing = || CamError::parse(1, "incomplete BOM preset");
    let name = setting_string(object, "name")?.ok_or_else(missing)?;
    let mut view = BomView {
        sort_field: setting_string(object, "sort_field")?.ok_or_else(missing)?,
        sort_asc: setting_bool(object, "sort_asc")?.ok_or_else(missing)?,
        filter_string: setting_string(object, "filter_string")?.ok_or_else(missing)?,
        filter_scope: filter_scope(&setting_string(object, "filter_scope")?.unwrap_or_else(|| "reference".into()))?,
        group_symbols: setting_bool(object, "group_symbols")?.ok_or_else(missing)?,
        exclude_dnp: setting_bool(object, "exclude_dnp")?.ok_or_else(missing)?,
        include_excluded_from_bom: setting_bool(object, "include_excluded_from_bom")?.unwrap_or(false),
        fields: Vec::new(),
    };
    if let Some(fields) = object.get("fields_ordered") {
        let Json::Array(fields) = fields else { return Err(missing()); };
        for field in fields {
            let field = json_object(field)?;
            view.fields.push(BomField {
                name: setting_string(field, "name")?.ok_or_else(missing)?,
                label: setting_string(field, "label")?.ok_or_else(missing)?,
                show: setting_bool(field, "show")?.ok_or_else(missing)?,
                group_by: setting_bool(field, "group_by")?.ok_or_else(missing)?,
            });
        }
    }
    Ok((name, view))
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
// settings can be skipped, without treating nested keys as job settings. Unknown numbers
// are validated and discarded without converting through floating point.
#[derive(Debug)]
enum Json {
    Null,
    Bool(bool),
    Number,
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

struct JsonReader<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> JsonReader<'a> {
    fn read(text: &'a str) -> Result<Json, CamError> {
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
    fn value(&mut self) -> Result<Json, CamError> {
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
    fn number(&mut self) -> Result<Json, CamError> {
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
        Ok(Json::Number)
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
#[path = "bom_export_tests.rs"]
mod tests;

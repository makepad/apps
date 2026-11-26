use crate::prices::Sourcing;
use fab_elec_cam::bom_export::{build_rows_with_paths, BomFormat, BomView, ResolvedBomOptions, SymbolScope};
use fab_elec_cam::tables::{assembly_view, bom_from_board, bom_from_circuit};
use fab_elec_circuit::{Circuit, Component};
use fab_elec_core::{BoardId, CompId};
use fab_elec_design::Design;
use std::collections::{BTreeMap, BTreeSet};

pub struct SheetView {
    pub cam: BomView,
    pub placed_only: bool,
    /// Resolved member paths for a single part or an explicitly selected line.
    pub only: Option<BTreeSet<String>>,
}

impl Default for SheetView {
    fn default() -> Self {
        Self {
            cam: assembly_view(),
            only: None, placed_only: false,
        }
    }
}

impl SheetView {
    pub fn options(&self) -> ResolvedBomOptions {
        ResolvedBomOptions {
            view: self.cam.clone(), symbol_scope: SymbolScope::Project,
            format: BomFormat { ref_delimiter: ", ".into(), ..BomFormat::default() },
        }
    }
}

#[derive(Debug)]
pub struct BomSheet {
    pub header: Vec<String>,
    pub rows: Vec<BomRow>,
    /// Display lines in scroll order, indexed once for the virtual list.
    pub lines: Vec<(usize, Option<usize>)>,
}

/// Purchasing data stays attached to its member even when a custom view merges unlike parts.
#[derive(Clone, Debug)]
pub struct Member {
    pub path: String,
    pub reference: String,
    pub part: Sourcing,
    pub footprint: String,
    pub dnp: bool,
    pub assembly_class: Option<String>,
}

#[derive(Debug)]
pub struct BomRow {
    pub members: Vec<Member>,
    pub cells: Vec<String>,
    pub refs: Vec<String>,
    pub paths: Vec<String>,
    /// Mechanical footprints may have no corresponding circuit component.
    pub comps: Vec<CompId>,
    pub alternates: Vec<Alternate>,
    pub read_only: Option<String>,
    pub qty: usize,
    pub dnp: bool,
    /// Membership frozen at read time, including the unit and each member path.
    pub key: String,
    pub checks: BTreeSet<&'static str>,
    pub fields: BTreeMap<String, BTreeSet<String>>,
    /// Keys supplied by the part library on at least one member of this row.
    pub inherited: BTreeSet<String>,
}

/// One distinct sourcing option on this line. Its member paths may be a subset of the fitted row.
#[derive(Debug)]
pub struct Alternate {
    pub number: i64,
    pub mpn: String,
    pub supplier: String,
    pub supplier_part: String,
    pub note: String,
    pub ai: bool,
    pub refs: Vec<String>,
    pub comps: Vec<CompId>,
    pub key: String,
    pub read_only: Option<String>,
}

pub fn alternate_key(key: &str) -> Option<(i64, &str)> {
    let (n, field) = key.strip_prefix("alt.")?.split_once('.')?;
    let number = n.parse::<i64>().ok()?;
    (number > 0 && n.bytes().all(|b| b.is_ascii_digit()) && !n.starts_with('0')).then_some((number, field))
}

fn line_key(unit: &str, paths: &[String]) -> String {
    let identity = std::iter::once(unit).chain(paths.iter().map(String::as_str)).map(makepad_strict_json::s).collect();
    format!("line:{}", makepad_strict_json::Value::Arr(identity).to_json())
}

pub fn component<'a>(circuit: &'a Circuit, selector: &str) -> Result<&'a Component, String> {
    let path = selector.strip_prefix("path:");
    let mut found = circuit.comps.iter().filter(|c| match path {
        Some(path) => c.path == path,
        None => c.path == selector || c.refdes == selector,
    });
    let first = found.next().ok_or_else(|| format!("no component named {selector}"))?;
    if let Some(second) = found.next() {
        let paths = [first, second].into_iter().chain(found)
            .map(|c| format!("path:{}", c.path)).collect::<Vec<_>>().join(", ");
        return Err(format!("{selector} matches several components: {paths}"));
    }
    Ok(first)
}

pub fn sheet(design: &Design, unit: BoardId, view: &SheetView) -> Result<BomSheet, String> {
    let unit = design.unit(unit).ok_or("the unit no longer exists")?;
    let board = unit.board.as_deref();
    let mut doc = if view.placed_only {
        bom_from_board(board.ok_or("the unit has no board")?, &unit.circuit)
    } else {
        bom_from_circuit(&unit.circuit, board)
    };
    if let Some(paths) = &view.only {
        doc.items.retain(|i| paths.contains(&i.symbol_id));
    }
    let items: BTreeMap<_, _> = doc.items.iter().map(|i| (i.symbol_id.as_str(), i)).collect();
    let components: BTreeMap<_, _> = unit.circuit.comps.iter().map(|c| (c.path.as_str(), c)).collect();
    let header = view.cam.fields.iter().filter(|f| f.show).map(|f| f.label.clone()).collect();
    let rows: Vec<_> = build_rows_with_paths(&doc, &view.options(), "").into_iter().map(|built| {
        let mut alternatives = BTreeMap::<_, Vec<_>>::new();
        let mut checks = BTreeSet::new();
        let mut fields = BTreeMap::<String, BTreeSet<String>>::new();
        let mut comps = Vec::new();
        let mut inherited = BTreeSet::new();
        let mut dnp = true;
        let mut members = Vec::with_capacity(built.paths.len());
        for path in &built.paths {
            let item = items[path.as_str()];
            dnp &= item.attributes.dnp;
            let field = |k| item.fields.get(k).cloned().unwrap_or_default();
            members.push(Member { path: path.clone(), reference: item.reference.clone(), dnp: item.attributes.dnp,
                part: Sourcing { mpn: field("mpn"), supplier: field("supplier"), supplier_part: field("supplier_part") },
                footprint: item.footprint.clone(), assembly_class: item.fields.get("assembly_class").filter(|v| !v.is_empty()).cloned() });
            for (field, check) in [("mpn", "no MPN"), ("supplier_part", "no supplier part")] {
                if item.fields.get(field).is_none_or(|v| v.trim().is_empty()) { checks.insert(check); }
            }
            if item.footprint.trim().is_empty() { checks.insert("no footprint"); }
            if item.placed {
                if item.attributes.dnp { checks.insert("placed but DNP"); }
            } else if board.is_some() {
                checks.insert("in circuit but not placed");
            }
            for (key, value) in &item.fields {
                fields.entry(key.clone()).or_default().insert(value.clone());
            }
            if let Some(c) = components.get(path.as_str()) {
                comps.push(c.id);
                for (number, mpn) in item.fields.iter().filter_map(|(k, v)| match alternate_key(k) {
                    Some((n, "mpn")) => Some((n, v)), _ => None,
                }) {
                    let field = |name| item.fields.get(&format!("alt.{number}.{name}")).cloned().unwrap_or_default();
                    let option = (number, mpn.clone(), field("supplier"), field("supplier_part"), field("note"), field("by") == "AI", field("lcsc"));
                    alternatives.entry(option).or_default().push((c.id, c.refdes.clone(), c.path.clone()));
                }
                inherited.extend(c.part.fields.0.iter().filter(|(k, _)| c.fields.get(k).is_none()).map(|(k, _)| k.clone()));
            }
        }
        let key = line_key(&unit.name, &built.paths);
        let alternates = alternatives.into_iter().map(|((number, mpn, supplier, supplier_part, note, ai, _), parts)| {
            let paths = parts.iter().map(|(_, _, p)| p.clone()).collect::<Vec<_>>();
            Alternate { number, mpn, supplier, supplier_part, note, ai, key: line_key(&unit.name, &paths),
                refs: parts.iter().map(|(_, r, _)| r.clone()).collect(), comps: parts.into_iter().map(|(c, _, _)| c).collect(), read_only: None }
        }).collect();
        BomRow { members, alternates, read_only: None, cells: built.row.cells, refs: built.row.refs, qty: built.paths.len(), paths: built.paths,
            comps, dnp, key, checks, fields, inherited }
    }).collect();
    let mut lines = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        lines.push((i, None));
        lines.extend((0..row.alternates.len()).map(|a| (i, Some(a))));
    }
    Ok(BomSheet { header, rows, lines })
}

/// Decode only explicit paths from a line key. An edit must resolve every one on the current design.
pub fn line_members(key: &str) -> Result<(String, Vec<String>), String> {
    let value = makepad_strict_json::parse(key.strip_prefix("line:").ok_or("expected a BOM line")?.as_bytes())?;
    let members = value.as_arr().ok_or("invalid BOM line")?;
    let (unit, paths) = members.split_first().ok_or("empty BOM line")?;
    if paths.is_empty() { return Err("empty BOM line".into()); }
    Ok((unit.as_str().ok_or("invalid unit")?.into(),
        paths.iter().map(|v| v.as_str().map(str::to_owned).ok_or("invalid member path")).collect::<Result<_, _>>()?))
}

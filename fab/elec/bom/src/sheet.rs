use crate::prices::Sourcing;
use fab_elec_cam::bom_export::{build_rows_with_paths, BomFormat, BomView, ResolvedBomOptions, SymbolScope};
use fab_elec_cam::tables::{assembly_view, bom_from_board, bom_from_circuit};
use fab_elec_circuit::{Circuit, Component};
use fab_elec_core::{BoardId, CompId};
use fab_elec_design::Design;
use fab_elec_parts::Lifecycle;
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
    /// The worst lifecycle of the row's components (a component's `lifecycle` field over its
    /// part's); `None` when none states one.
    pub lifecycle: Option<Lifecycle>,
    /// Every component of the row is on a part the person approved; `false` for a row without
    /// circuit components.
    pub approved: bool,
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
        // The worst stated lifecycle (`Obsolete > Nrnd > Active`); approval only when every
        // circuit component's part is approved.
        let lifecycle = comps.iter().filter_map(|id| {
            let c = &unit.circuit.comps[id.ix()];
            c.fields.0.iter().find(|(key, _)| key.eq_ignore_ascii_case("lifecycle")).map(|(_, v)| v.as_str())
                .filter(|v| !v.is_empty()).and_then(Lifecycle::of_name).or(c.part.lifecycle)
        }).max();
        let approved = !comps.is_empty() && comps.iter().all(|id| unit.circuit.comps[id.ix()].part.approved);
        match lifecycle {
            Some(Lifecycle::Nrnd) => { checks.insert("NRND"); }
            Some(Lifecycle::Obsolete) => { checks.insert("obsolete"); }
            _ => {}
        }
        let alternates = alternatives.into_iter().map(|((number, mpn, supplier, supplier_part, note, ai, _), parts)| {
            let paths = parts.iter().map(|(_, _, p)| p.clone()).collect::<Vec<_>>();
            Alternate { number, mpn, supplier, supplier_part, note, ai, key: line_key(&unit.name, &paths),
                refs: parts.iter().map(|(_, r, _)| r.clone()).collect(), comps: parts.into_iter().map(|(c, _, _)| c).collect(), read_only: None }
        }).collect();
        BomRow { members, alternates, read_only: None, cells: built.row.cells, refs: built.row.refs, qty: built.paths.len(), paths: built.paths,
            comps, dnp, key, checks, fields, inherited, lifecycle, approved }
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

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn bom_sheet_surfaces_approved_and_warns_on_nrnd_and_obsolete() {
        let mut design = fab_elec_fixtures::ledctl();
        let id = design.units[0].id;
        let before = sheet(&design, id, &SheetView::default()).unwrap();
        assert!(before.rows.iter().all(|r| !r.approved && r.lifecycle.is_none()), "the fixture states no lifecycle or approval");
        assert!(before.rows.iter().all(|r| !r.checks.contains("NRND") && !r.checks.contains("obsolete")));
        let with_comps: Vec<usize> = (0..before.rows.len()).filter(|&i| !before.rows[i].comps.is_empty()).collect();
        let pair = *with_comps.iter().find(|&&i| before.rows[i].comps.len() >= 2).expect("the fixture groups a line of two parts");
        let singles: Vec<usize> = with_comps.iter().copied().filter(|&i| i != pair).take(3).collect();
        assert_eq!(singles.len(), 3, "the fixture has three more lines with components");
        let (approved_row, nrnd_row, field_row) = (singles[0], singles[1], singles[2]);

        let circuit = Arc::make_mut(&mut design.units[0].circuit);
        let mut part = |cid: CompId, f: &dyn Fn(&mut fab_elec_parts::PartDef)| f(Arc::make_mut(&mut circuit.comps[cid.ix()].part));
        // Every member approved and active: an approved, active line with no lifecycle check.
        for &cid in &before.rows[approved_row].comps {
            part(cid, &|p| { p.approved = true; p.lifecycle = Some(Lifecycle::Active); });
        }
        // Every member NRND (and approved): an approved line with an NRND check.
        for &cid in &before.rows[nrnd_row].comps {
            part(cid, &|p| { p.approved = true; p.lifecycle = Some(Lifecycle::Nrnd); });
        }
        // Mixed approval in a grouped line: not approved; an obsolete member makes it obsolete.
        let (first, second) = (before.rows[pair].comps[0], before.rows[pair].comps[1]);
        part(first, &|p| { p.approved = true; p.lifecycle = Some(Lifecycle::Active); });
        part(second, &|p| p.lifecycle = Some(Lifecycle::Obsolete));
        // A component's own `lifecycle` field wins over its part's.
        for &cid in &before.rows[field_row].comps {
            part(cid, &|p| p.lifecycle = Some(Lifecycle::Active));
        }
        let field_comp = before.rows[field_row].comps[0];
        circuit.comps[field_comp.ix()].fields.0.push(("Lifecycle".into(), "obsolete".into()));

        let after = sheet(&design, id, &SheetView::default()).unwrap();
        assert_eq!(after.rows.len(), before.rows.len(), "lifecycle and approval change no row count");
        for (a, b) in after.rows.iter().zip(&before.rows) {
            assert_eq!((&a.key, &a.refs, &a.comps), (&b.key, &b.refs, &b.comps), "lifecycle and approval change no grouping");
        }
        let row = &after.rows[approved_row];
        assert!(row.approved, "every member approved makes the line approved");
        assert_eq!(row.lifecycle, Some(Lifecycle::Active));
        assert!(!row.checks.contains("NRND") && !row.checks.contains("obsolete"), "an active line has no lifecycle check");
        let row = &after.rows[nrnd_row];
        assert!(row.approved && row.lifecycle == Some(Lifecycle::Nrnd));
        assert!(row.checks.contains("NRND") && !row.checks.contains("obsolete"), "an NRND line warns NRND: {:?}", row.checks);
        let row = &after.rows[pair];
        assert!(!row.approved, "mixed approval is not approved");
        assert_eq!(row.lifecycle, Some(Lifecycle::Obsolete), "the worst member's lifecycle is the line's");
        assert!(row.checks.contains("obsolete") && !row.checks.contains("NRND"), "{:?}", row.checks);
        let row = &after.rows[field_row];
        assert!(!row.approved, "unapproved parts make no approved line");
        assert_eq!(row.lifecycle, Some(Lifecycle::Obsolete), "the mixed-case component field wins over the part's lifecycle");
        assert!(row.checks.contains("obsolete"));
        for (i, row) in after.rows.iter().enumerate() {
            if ![approved_row, nrnd_row, pair, field_row].contains(&i) {
                assert!(!row.approved && row.lifecycle.is_none(), "untouched lines stay as they were");
            }
        }
    }
}

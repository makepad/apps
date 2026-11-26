//! Generic assembly tables resolved from the circuit and the board's placed footprints.

use crate::bom_export::{build_table, BomField, BomFormat, BomView, FilterScope, ResolvedBomOptions};
use crate::profile::{PlaceRow, ResolvedProfile};
use crate::text::deg_str;
use fab_elec_board::Board;
use fab_elec_circuit::{Circuit, Component};
use fab_elec_core::{Angle, Nm, Side, P};
use fab_elec_parts::PartKind;
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// S46.R75 negates X about the selected origin. S47 factory profiles may instead
/// mirror about a board width. Keep the distinction in the table for validation.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum BottomX {
    #[default]
    NegateX,
    MirrorAbout(Nm),
}

impl BottomX {
    pub fn apply(self, x: i64) -> i64 {
        match self { Self::NegateX => -x, Self::MirrorAbout(width) => width as i64 - x }
    }
}

#[derive(Clone, Debug)]
pub struct PlacementRules {
    /// The first footprint name prefix that matches supplies the rotation correction.
    pub rotation_offsets: Vec<(String, Angle)>,
    pub bottom_mirrored: bool,
    /// Selected convention when bottom_mirrored is enabled. Reference mode negates X.
    pub bottom_x: BottomX,
    /// Top and bottom side labels, respectively.
    pub side_words: (String, String),
    pub origin: PlacementOrigin,
    /// Write position cells in inches with four decimals; stored positions remain in nm.
    pub unit_inch: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PlacementOrigin {
    BoardLowerLeft,
    Absolute,
}

impl Default for PlacementRules {
    fn default() -> Self {
        Self {
            rotation_offsets: Vec::new(),
            bottom_mirrored: true,
            bottom_x: BottomX::NegateX,
            side_words: ("T".into(), "B".into()),
            origin: PlacementOrigin::BoardLowerLeft,
            unit_inch: false,
        }
    }
}

impl PlacementRules {
    /// JLC assembly coordinates share the Gerber writer's absolute, Y-up, top-view frame.
    /// Imported footprints already use the supplier's orientation, so no correction is added.
    pub fn jlc() -> Self {
        Self {
            rotation_offsets: Vec::new(),
            bottom_mirrored: false,
            bottom_x: BottomX::NegateX,
            side_words: ("Top".into(), "Bottom".into()),
            origin: PlacementOrigin::Absolute,
            unit_inch: false,
        }
    }
}

/// The orderable part's identity is shared by the live sheet and every factory profile.
pub fn assembly_view() -> BomView {
    let columns = [
        ("Reference", "Refs", false), ("${QUANTITY}", "Qty", false),
        ("Value", "Value", true), ("Footprint", "Footprint", true), ("mpn", "MPN", true),
        ("supplier", "Supplier", false), ("supplier_part", "Supplier part", true),
        ("Description", "Description", false), ("${DNP}", "DNP", true),
        (LCSC_PART, "LCSC Part #", true), (SUPPLIER_KEY, "Supplier key", true),
    ];
    BomView {
        fields: columns.into_iter().map(|(name, label, group_by)| BomField {
            name: name.into(), label: label.into(), group_by, show: !matches!(name, LCSC_PART | SUPPLIER_KEY),
        }).collect(),
        sort_field: "Reference".into(), filter_scope: FilterScope::Visible, ..BomView::default()
    }
}

pub(crate) const ASSEMBLY_REFS: &str = "${ASSEMBLY_REFS}";
pub(crate) const LCSC_PART: &str = "${LCSC_PART}";
pub(crate) const SUPPLIER_KEY: &str = "${SUPPLIER_KEY}";
pub(crate) const PLACED_FOOTPRINT: &str = "${PLACED_FOOTPRINT}";

const BOM_COLUMN_ALIASES: &[(&[&str], &str)] = &[
    (&["Comment", "Value"], "Value"),
    (&["Designator", "Reference", "Refs"], ASSEMBLY_REFS),
    (&["Footprint", "Package"], PLACED_FOOTPRINT),
    (&["Part", "MPN", "Manufacturer Part"], "mpn"),
    (&["LCSC Part #", "LCSC Part", "LCSC", "JLCPCB Part #"], LCSC_PART),
    (&["Qty", "Quantity"], "${QUANTITY}"),
];

/// Replaces the rows while retaining the profile's headers and delimiters.
pub fn fill_generic_tables(profile: &mut ResolvedProfile, board: &Board, circuit: &Circuit, rules: &PlacementRules) {
    let mut view = assembly_view();
    view.exclude_dnp = true;
    // Retain the assembly's grouping keys independently of which columns the factory wants.
    view.fields.retain(|f| f.group_by);
    for field in &mut view.fields { field.show = false; }
    for header in &profile.bom.header {
        let name = BOM_COLUMN_ALIASES.iter().find(|(aliases, _)| aliases.iter().any(|a| header.eq_ignore_ascii_case(a)))
            .map_or("", |(_, name)| *name);
        view.fields.push(BomField { name: name.into(), label: header.clone(), group_by: false, show: true });
    }
    let options = ResolvedBomOptions { view, symbol_scope: Default::default(),
        format: BomFormat { ref_delimiter: ",".into(), ..Default::default() } };
    profile.bom.rows = build_table(&bom_from_circuit(circuit, Some(board)), &options, "").rows;

    let bbox = board.outline_bbox();
    let origin = match rules.origin {
        PlacementOrigin::BoardLowerLeft if !bbox.is_empty() => bbox.min,
        _ => P::ZERO,
    };
    profile.placement.origin = origin;
    profile.placement.bottom_x = rules.bottom_mirrored.then_some(rules.bottom_x);
    let position_cell = if rules.unit_inch { inch_4 } else { mm_3 };
    let mut placement_rows = Vec::with_capacity(board.placed.len());
    for placed in &board.placed {
        if circuit.comps.get(placed.comp.ix()).map_or(false, |c| c.dnp) {
            continue;
        }
        let rot_offset = rules.rotation_offsets.iter()
            .find(|(prefix, _)| placed.footprint.name.starts_with(prefix.as_str()))
            .map_or(Angle::ZERO, |(_, offset)| *offset);
        let rot = placed.rot.add(rot_offset);
        let mirrored = placed.side == Side::Bottom && rules.bottom_mirrored;
        let mut at = placed.at.sub(origin);
        if mirrored {
            at.x = rules.bottom_x.apply(at.x as i64) as Nm;
        }
        // The validator compares the corrected board rotation; only the exported cell
        // uses the factory's mirrored bottom-side convention.
        let cell_rot = placement_rotation(rot, mirrored.then_some(rules.bottom_x));
        let side_word = match placed.side {
            Side::Top => &rules.side_words.0,
            Side::Bottom => &rules.side_words.1,
        };
        placement_rows.push(PlaceRow {
            cells: vec![placed.refdes.clone(), position_cell(at.x), position_cell(at.y), side_word.clone(), deg_str(cell_rot.udeg())],
            refdes: placed.refdes.clone(),
            at,
            rot,
            rot_offset,
            side: placed.side,
        });
    }
    placement_rows.sort_by(|a, b| natural_cmp(&a.refdes, &b.refdes));
    profile.placement.rows = placement_rows;
}

/// Part defaults followed by component overrides, shared by all circuit BOM tables.
fn merged_fields(comp: &Component) -> BTreeMap<String, String> {
    let mut fields: BTreeMap<_, _> = comp.part.fields.0.iter().cloned().collect();
    fields.extend(comp.fields.0.iter().cloned());
    fields
}

/// Resolve the LCSC number once so the displayed column and the group key cannot disagree.
pub(crate) fn lcsc_number(fields: &BTreeMap<String, String>) -> &str {
    if fields.get("supplier").is_some_and(|supplier| supplier.trim().eq_ignore_ascii_case("lcsc")) {
        return fields.get("supplier_part").map_or("", String::as_str);
    }
    fields.iter().find(|(name, _)| name.eq_ignore_ascii_case("lcsc")).map_or("", |(_, value)| value.as_str())
}

/// Convert a corrected board angle to the placement cell's sense. Board pads rotate CCW
/// with Y up, and Gerber writes those coordinates unchanged, so top-view angles keep their
/// sign. Only the legacy mirrored-about-width convention negates bottom angles. JLC's
/// bottom-side rotation sense still needs confirmation in a real order preview.
fn placement_rotation(rot: Angle, bottom_x: Option<BottomX>) -> Angle {
    if matches!(bottom_x, Some(BottomX::MirrorAbout(_))) { rot.neg() } else { rot }
}

/// Millimetres rounded to three decimals, without a negative zero.
fn mm_3(nm: Nm) -> String {
    let microns = ((nm as i64).unsigned_abs() + 500) / 1_000;
    let sign = if nm < 0 && microns != 0 { "-" } else { "" };
    format!("{sign}{}.{:03}", microns / 1_000, microns % 1_000)
}

/// Inches rounded to four decimals, without a negative zero (one step is 2540 nm).
fn inch_4(nm: Nm) -> String {
    let steps = ((nm as i64).unsigned_abs() + 1_270) / 2_540;
    let sign = if nm < 0 && steps != 0 { "-" } else { "" };
    format!("{sign}{}.{:04}", steps / 10_000, steps % 10_000)
}

/// Compare digit runs by magnitude without parsing into a bounded integer.
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut left, mut right) = (a.as_bytes(), b.as_bytes());
    while !left.is_empty() && !right.is_empty() {
        if left[0].is_ascii_digit() && right[0].is_ascii_digit() {
            let l_end = left.iter().position(|c| !c.is_ascii_digit()).unwrap_or(left.len());
            let r_end = right.iter().position(|c| !c.is_ascii_digit()).unwrap_or(right.len());
            let l_start = left[..l_end].iter().position(|c| *c != b'0').unwrap_or(l_end);
            let r_start = right[..r_end].iter().position(|c| *c != b'0').unwrap_or(r_end);
            let l_digits = &left[l_start..l_end];
            let r_digits = &right[r_start..r_end];
            let order = l_digits.len().cmp(&r_digits.len()).then_with(|| l_digits.cmp(r_digits));
            if order != Ordering::Equal {
                return order;
            }
            left = &left[l_end..];
            right = &right[r_end..];
        } else {
            let order = left[0].cmp(&right[0]);
            if order != Ordering::Equal {
                return order;
            }
            left = &left[1..];
            right = &right[1..];
        }
    }
    left.len().cmp(&right.len()).then_with(|| a.cmp(b))
}

/// S46 BOM input from circuit components; the host adds variant/sheet metadata that the
/// circuit model does not yet carry. Factory profiles and the live sheet consume this document.
pub fn bom_from_circuit(circuit: &Circuit, board: Option<&Board>) -> crate::bom_export::BomDocument {
    let footprints: BTreeMap<_, _> = board.into_iter().flat_map(|b| &b.placed)
        .map(|p| (p.comp, p.footprint.name.as_str())).collect();
    crate::bom_export::BomDocument {
        // A power flag exists only on the schematic and has no part to order.
        items: circuit.comps.iter().filter(|c| c.part.kind != PartKind::PowerFlag).map(|comp| {
            let on_board = footprints.get(&comp.id).copied();
            let footprint = on_board
                .or_else(|| comp.part.footprint.as_ref().map(|f| f.name.as_str())).unwrap_or("");
            let mut item = bom_component(comp, footprint);
            item.placed = on_board.is_some();
            item
        }).collect(),
        ..Default::default()
    }
}

/// Board BOMs contain only placed footprints, including mechanical footprints without a component.
pub fn bom_from_board(board: &Board, circuit: &Circuit) -> crate::bom_export::BomDocument {
    let components: BTreeMap<_, _> = circuit.comps.iter().map(|c| (c.id, c)).collect();
    crate::bom_export::BomDocument {
        source: crate::bom_export::BomSource::Board,
        items: board.placed.iter().map(|placed| {
            let mut item = components.get(&placed.comp).map_or_else(
                crate::bom_export::BomItem::default, |c| bom_component(c, &placed.footprint.name));
            item.placed = true;
            item.symbol_id = placed.path.clone();
            item.reference = placed.refdes.clone();
            item.footprint = placed.footprint.name.clone();
            item
        }).collect(),
        ..Default::default()
    }
}

pub(crate) fn excludes_bom(comp: &Component) -> bool {
    comp.fields.get("exclude_from_bom").or_else(|| comp.part.fields.get("exclude_from_bom"))
        .is_some_and(|value| matches!(value, "1" | "true" | "yes"))
}

fn bom_component(comp: &Component, footprint: &str) -> crate::bom_export::BomItem {
    let fields = merged_fields(comp);
    let flag = |name: &str| fields.get(name).is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"));
    let attributes = crate::bom_export::BomAttributes {
        dnp: comp.dnp, exclude_from_bom: excludes_bom(comp),
        exclude_from_board: flag("exclude_from_board"), exclude_from_sim: flag("exclude_from_sim"),
        exclude_from_pos_files: flag("exclude_from_pos_files"),
    };
    crate::bom_export::BomItem {
        symbol_id: comp.path.clone(), reference: comp.refdes.clone(),
        value: comp.value.as_ref().map_or_else(String::new, |v| v.text.clone()),
        footprint: footprint.into(),
        datasheet: fields.get("Datasheet").or_else(|| fields.get("datasheet")).cloned().unwrap_or_default(),
        description: fields.get("Description").or_else(|| fields.get("description")).cloned().unwrap_or_default(),
        fields, attributes, ..Default::default()
    }
}

#[cfg(test)]
#[path = "tables_tests.rs"]
mod tests;

//! Pure board check overlays: resolve findings to scene shapes once when a snapshot changes.

use super::scene::{DrawScene, Role};
use fab_elec_board::{Board, ItemKind};
use fab_elec_circuit::{CheckKind, Measure, Violation};
use fab_elec_core::{BBox, BoardId, CompId, FeatureRef, ItemId, LayerId, Sel, Severity, P};
use fab_elec_route::Collision;
use std::cmp::{Ordering, Reverse};
use std::collections::{HashMap, HashSet};

/// One finding as the board canvas shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckMark {
    pub id: u64,
    pub kind: CheckKind,
    pub severity: Severity,
    /// Drawn in pending ink while a zone refill or a shove wave may clear the finding.
    pub pending: bool,
    /// The finding's `at`, else the centre of its items' box, in board nm.
    pub at: P,
    /// The box of its scene shapes, in board nm; empty when none is drawn.
    pub bbox: BBox,
    /// Lit shapes: (layer, index into that layer's `shapes`), in scene order, without duplicates.
    pub shapes: Vec<(LayerId, u32)>,
    /// The first lit shape's layer, else None (all layers).
    pub layer: Option<LayerId>,
    /// The board items it names, sorted: hovering one of them shows a warning's or an info's label.
    pub items: Vec<ItemId>,
    /// Measured against required when both exist and differ; otherwise one line of the message,
    /// cut after 48 characters with an ellipsis when longer.
    pub label: String,
}

/// Placement resolution and shape bounds built on the worker alongside one board's scene.
/// Rebuild with that scene; finding updates only visit the selections they name.
#[derive(Clone, Debug)]
pub struct CheckIndex {
    unit: BoardId,
    /// Component -> (placed item, footprint pad count), including parts without scene shapes.
    placed: HashMap<CompId, (ItemId, usize)>,
    by_item: HashMap<ItemId, ShapeBox>,
    by_pad: HashMap<(ItemId, u32), ShapeBox>,
}

#[derive(Clone, Debug, Default)]
struct ShapeBox {
    shapes: Vec<(LayerId, u32)>,
    bbox: BBox,
}

impl ShapeBox {
    fn push(&mut self, shape: (LayerId, u32), bbox: BBox) {
        self.shapes.push(shape);
        self.bbox = self.bbox.union(bbox);
    }
}

impl CheckIndex {
    pub fn new(board: &Board, scene: &DrawScene) -> CheckIndex {
        let mut index = CheckIndex {
            unit: board.unit,
            placed: board.placed.iter().map(|p| (p.comp, (p.id, p.footprint.pads.len()))).collect(),
            by_item: HashMap::new(),
            by_pad: HashMap::new(),
        };
        for (layer, content) in &scene.layers {
            for (i, shape) in content.shapes.iter().enumerate() {
                let key = (*layer, i as u32);
                let bbox = shape.shape.bbox();
                index.by_item.entry(shape.tag.item).or_default().push(key, bbox);
                if matches!(shape.tag.role, Role::Pad | Role::MaskOpening | Role::Paste) {
                    index.by_pad.entry((shape.tag.item, shape.tag.sub)).or_default().push(key, bbox);
                }
            }
        }
        index
    }

    /// Shared by marker geometry and the host's item-state severity channel.
    fn resolve(&self, sel: &Sel, unit: BoardId) -> Option<(ItemId, Option<u32>)> {
        if unit != self.unit || sel.unit() != Some(unit) {
            return None;
        }
        match sel {
            Sel::Item(_, item) | Sel::Feature(FeatureRef::Hole(_, item)) => Some((*item, None)),
            Sel::Comp(comp) | Sel::Feature(FeatureRef::Part(comp)) => {
                self.placed.get(&comp.comp).map(|&(item, _)| (item, None))
            }
            Sel::Feature(FeatureRef::Pad(comp, pad)) => {
                let &(item, pads) = self.placed.get(&comp.comp)?;
                (pad.ix() < pads).then_some((item, Some(u32::from(pad.0))))
            }
            _ => None,
        }
    }
}

/// Sorted ids of findings whose two items match a collision involving a track of this board.
/// Builds the pair set in one pass over the collisions, scans the findings once, and sorts the matches.
pub fn wave_may_clear(violations: &[Violation], collisions: &[Collision], board: &Board) -> Vec<u64> {
    fn pair(mut items: [&Sel; 2]) -> [&Sel; 2] {
        items.sort_unstable();
        items
    }

    let is_track = |sel: &Sel| matches!(sel, Sel::Item(unit, id)
        if *unit == board.unit && matches!(board.locate(*id), Some((ItemKind::Track, _))));
    let pairs: HashSet<_> = collisions.iter().filter(|c| is_track(&c.a) || is_track(&c.b))
        .map(|c| pair([&c.a, &c.b])).collect();
    if pairs.is_empty() {
        return Vec::new();
    }
    let mut ids: Vec<_> = violations.iter().filter_map(|v| {
        let [a, b] = v.items.as_slice() else { return None };
        pairs.contains(&pair([a, b])).then_some(v.id)
    }).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The board unit's findings, worst severity first, then by id. Other units and findings with
/// no unit are excluded, as are findings with neither an `at` nor drawn shapes.
///
/// An item lights all its shapes. A component pad resolves through `Placed::comp`; the current
/// `drc::sel_of` stores its footprint `PadIx` directly, which is the scene tag's `sub` (not the
/// pad's printed number). Only pad, mask and paste roles belong to that pad: graphics and text
/// have their own sub indices. A track selection lights the whole track: `Sel` has no segment
/// index. Polygon entries are not indices into `SceneLayer::shapes` and are not included.
///
/// `index` belongs to this board snapshot. Each finding visits only its matching shapes and
/// sorts them (O(k log k) for k shapes), then the marks sort in O(findings log findings).
/// Neither the scene nor the placements are scanned here.
pub fn check_marks(violations: &[Violation], awaiting: &[u64], unit: BoardId, board: &Board, index: &CheckIndex) -> Vec<CheckMark> {
    debug_assert_eq!(board.unit, index.unit, "check index belongs to this board");
    let mut marks = Vec::new();
    for violation in violations.iter().filter(|v| v.unit == Some(unit)) {
        let mut shapes = Vec::new();
        let mut bbox = BBox::EMPTY;
        let mut items = Vec::new();
        for sel in &violation.items {
            let Some((item, sub)) = index.resolve(sel, unit) else { continue };
            items.push(item);
            let candidates = match sub {
                Some(sub) => index.by_pad.get(&(item, sub)),
                None => index.by_item.get(&item),
            };
            if let Some(candidates) = candidates {
                shapes.extend_from_slice(&candidates.shapes);
                bbox = bbox.union(candidates.bbox);
            }
        }
        // DrawScene layers are sorted by LayerId; indices preserve each layer's shape order.
        shapes.sort_unstable();
        shapes.dedup();
        items.sort_unstable();
        items.dedup();
        let at = match violation.at {
            Some(at) => at,
            None if !bbox.is_empty() => bbox.center(),
            None => continue,
        };
        marks.push(CheckMark {
            id: violation.id,
            kind: violation.kind.clone(),
            severity: violation.severity,
            pending: awaiting.binary_search(&violation.id).is_ok(),
            at,
            bbox,
            layer: shapes.first().map(|(layer, _)| *layer),
            shapes,
            items,
            label: label(violation),
        });
    }
    marks.sort_by_key(|mark| (Reverse(mark.severity), mark.id));
    marks
}

/// Worst severity per named item for the host's item-state z channel: info = 1, warning = 2,
/// error = 3, sorted by item. Valid selections count even without drawn shapes or a location.
pub fn violation_levels(violations: &[Violation], unit: BoardId, index: &CheckIndex) -> Vec<(ItemId, f32)> {
    let mut levels: HashMap<ItemId, Severity> = HashMap::new();
    for violation in violations.iter().filter(|v| v.unit == Some(unit)) {
        for sel in &violation.items {
            let Some((item, _)) = index.resolve(sel, unit) else { continue };
            levels.entry(item).and_modify(|level| *level = (*level).max(violation.severity)).or_insert(violation.severity);
        }
    }
    let mut levels: Vec<_> = levels.into_iter().map(|(item, severity)| {
        let level = match severity {
            Severity::Info => 1.0,
            Severity::Warning => 2.0,
            Severity::Error => 3.0,
        };
        (item, level)
    }).collect();
    levels.sort_unstable_by_key(|&(item, _)| item);
    levels
}

/// The marks whose marker lies in `view` (board nm), for drawing only what is on screen.
pub fn marks_in(marks: &[CheckMark], view: BBox) -> impl Iterator<Item = &CheckMark> {
    marks.iter().filter(move |mark| view.contains(mark.at))
}

pub(super) fn label(violation: &Violation) -> String {
    if let (Some(actual), Some(required)) = (violation.actual, violation.required) {
        let order = match (actual, required) {
            (Measure::Len(a), Measure::Len(b)) => Some(a.cmp(&b)),
            (Measure::Qty(a), Measure::Qty(b)) if a.unit == b.unit => a.si.partial_cmp(&b.si),
            _ => None,
        };
        // Equal measures show no fault (a silk overlap measures 0 against a 0 mm rule): the
        // message says what is wrong instead.
        let relation = match order {
            Some(Ordering::Less) => Some("<"),
            Some(Ordering::Equal) => None,
            Some(Ordering::Greater) => Some(">"),
            None => Some("vs"),
        };
        if let Some(relation) = relation {
            return format!("{} {relation} {}", measure(actual), measure(required));
        }
    }
    let message = violation.message.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = message.chars();
    let mut label: String = chars.by_ref().take(48).collect();
    if chars.next().is_some() {
        label.push('…');
    }
    label
}

fn measure(measure: Measure) -> String {
    match measure {
        Measure::Len(nm) => {
            let mm = format!("{:.3}", nm as f64 / 1_000_000.0);
            let mm = mm.trim_end_matches('0').trim_end_matches('.');
            format!("{} mm", if mm == "-0" { "0" } else { mm })
        }
        // Quantity currently has no Display implementation.
        Measure::Qty(qty) => format!("{qty:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::scene::scene_of;
    use fab_elec_board::{drc::sel_of, ConnItem};
    use fab_elec_circuit::{CheckKind, QtyUnit, Quantity};
    use fab_elec_core::{mm, CompRef, PadIx};
    use fab_elec_fixtures::ledctl;

    #[test]
    fn fixture_marks_resolve_items_pads_and_locations() {
        let design = ledctl();
        let unit = design.units[0].id;
        let mut board = design.units[0].board.as_deref().expect("fixture board").clone();
        // A component id and its placement id are independent, even when a fixture aligns them.
        let placed_id = board.alloc_id();
        board.placed.iter_mut().find(|p| p.path == "usb.cc[0]").expect("placed resistor").id = placed_id;
        let board = &board;
        let scene = scene_of(board, &[]);
        let index = CheckIndex::new(board, &scene);
        let part = board.placed.iter().find(|p| p.path == "usb.cc[0]").expect("placed resistor");
        let pad = part.footprint.pad("1").expect("resistor pad 1");
        let pad_sel = sel_of(board, ConnItem::Pad { placed: part.id, pad });
        assert_eq!(pad_sel, Sel::Feature(FeatureRef::Pad(CompRef { board: unit, comp: part.comp }, pad)), "invert DRC's component pad selection");
        let mut whole = Violation::new(CheckKind::Clearance, Severity::Warning, Some(unit),
            vec![Sel::Item(unit, part.id), pad_sel.clone(), Sel::Item(unit, part.id)], "clearance", "Whole part".into());
        whole.id = 10;
        let mut single = Violation::new(CheckKind::Clearance, Severity::Error, Some(unit), vec![pad_sel.clone()], "clearance", "Pad".into())
            .measures(Measure::Len(mm(0.12)), Measure::Len(mm(0.15)));
        single.id = 30;
        let isolated = P::mm(-10.0, -10.0);
        let mut position = Violation::new(CheckKind::Unconnected, Severity::Info, Some(unit), vec![], "", "At\nlocation".into()).at(isolated);
        position.id = 20;
        let mut other = whole.clone();
        other.unit = Some(BoardId(unit.0 + 1));
        let mut no_unit = whole.clone();
        no_unit.unit = None;
        let mut unlocated = position.clone();
        unlocated.at = None;
        let findings = [whole.clone(), position.clone(), other, no_unit, unlocated, single.clone()];
        let marks = check_marks(&findings, &[], unit, board, &index);
        assert_eq!(marks.iter().map(|m| m.id).collect::<Vec<_>>(), vec![30, 10, 20], "severity before id; only located findings of this unit");
        assert_eq!(marks[1].items, vec![part.id], "a mark names each item once, a pad by its part (hovering it shows the label)");
        assert_eq!(violation_levels(&findings, unit, &index), vec![(part.id, 3.0)], "the pad's error wins over the whole item's warning");
        assert_eq!(violation_levels(std::slice::from_ref(&whole), unit, &index), vec![(part.id, 2.0)], "warning is item-state level 2");
        assert!(violation_levels(&findings[1..5], unit, &index).is_empty(), "other units, no-unit and location-only findings light no items");

        let comp = CompRef { board: unit, comp: part.comp };
        let mut aliases = position.clone();
        aliases.items = vec![Sel::Comp(comp), Sel::Feature(FeatureRef::Part(comp)), Sel::Feature(FeatureRef::Hole(unit, part.id))];
        assert_eq!(violation_levels(std::slice::from_ref(&aliases), unit, &index), vec![(part.id, 1.0)], "component, part and hole aliases deduplicate; info is level 1");
        aliases.at = None;
        assert_eq!(check_marks(std::slice::from_ref(&aliases), &[], unit, board, &index)[0].shapes, marks[1].shapes, "marker aliases use the same placement resolution");
        let another = board.placed.iter().find(|p| p.id != part.id).expect("another placed part");
        let mut info = aliases.clone();
        info.items = vec![Sel::Item(unit, another.id)];
        let mut expected = vec![(part.id, 3.0), (another.id, 1.0)];
        expected.sort_unstable_by_key(|&(item, _)| item);
        assert_eq!(violation_levels(&[single.clone(), info, whole.clone(), aliases], unit, &index), expected, "levels sort by item and a later lesser severity cannot lower them");

        let track = Sel::Item(unit, board.tracks[0].id);
        let placed = Sel::Item(unit, another.id);
        let collision = |a, b| Collision { at: P::ZERO, layer: LayerId::F_CU, a, b, gap: 0, need: 1 };
        let collisions = [collision(pad_sel.clone(), track.clone()), collision(pad_sel.clone(), placed.clone())];
        let finding = |id, items| Violation { id, items, ..single.clone() };
        let wave_findings = [
            finding(5, vec![pad_sel.clone(), track.clone()]),
            finding(4, vec![track.clone(), placed.clone()]),
            finding(3, vec![track.clone(), pad_sel.clone(), placed.clone()]),
            finding(2, vec![pad_sel.clone(), placed]),
            finding(1, vec![track, pad_sel]),
        ];
        assert_eq!(wave_may_clear(&wave_findings, &collisions, board), vec![1, 5],
            "only the exact collision pair with a track is pending, in either item order");

        let mut foreign = position.clone();
        let foreign_unit = BoardId(unit.0 + 1);
        let foreign_comp = CompRef { board: foreign_unit, ..comp };
        foreign.items = vec![
            Sel::Item(foreign_unit, part.id), Sel::Comp(foreign_comp),
            Sel::Feature(FeatureRef::Part(foreign_comp)), Sel::Feature(FeatureRef::Pad(foreign_comp, pad)),
            Sel::Feature(FeatureRef::Hole(foreign_unit, part.id)),
            Sel::Comp(CompRef { comp: CompId(u32::MAX), ..comp }), Sel::Unit(unit),
        ];
        assert!(violation_levels(std::slice::from_ref(&foreign), unit, &index).is_empty(), "foreign selections, missing placements and unsupported selections light no items");

        let mut all_shapes = Vec::new();
        let mut pad_shapes = Vec::new();
        let mut all_bbox = BBox::EMPTY;
        let mut pad_bbox = BBox::EMPTY;
        for (layer, content) in &scene.layers {
            for (index, shape) in content.shapes.iter().enumerate().filter(|(_, s)| s.tag.item == part.id) {
                all_shapes.push((*layer, index as u32));
                all_bbox = all_bbox.union(shape.shape.bbox());
                if shape.tag.sub == pad.0 as u32 && matches!(shape.tag.role, Role::Pad | Role::MaskOpening | Role::Paste) {
                    pad_shapes.push((*layer, index as u32));
                    pad_bbox = pad_bbox.union(shape.shape.bbox());
                }
            }
        }
        assert!(all_shapes.iter().any(|(l, _)| *l != all_shapes[0].0), "whole part spans several layers");
        assert!(!pad_shapes.is_empty() && pad_shapes.len() < all_shapes.len(), "a pad lights only its own shapes");
        assert_eq!(marks[0].shapes, pad_shapes, "single pad shapes in scene order");
        assert_eq!(marks[1].shapes, all_shapes, "whole item includes all shapes without duplicates");
        assert_eq!((marks[0].bbox, marks[0].at), (pad_bbox, pad_bbox.center()), "pad bounds and fallback position");
        assert_eq!((marks[1].bbox, marks[1].at), (all_bbox, all_bbox.center()), "whole item bounds and fallback position");
        assert_eq!(marks[0].layer, Some(pad_shapes[0].0), "marker uses first lit layer");
        assert_eq!(marks[0].label, "0.12 mm < 0.15 mm", "length measures in trimmed mm");
        assert_eq!(marks[2].at, isolated, "explicit position");
        assert!(marks[2].shapes.is_empty() && marks[2].bbox.is_empty() && marks[2].layer.is_none(), "location-only mark has no scene geometry");
        assert_eq!(marks[2].label, "At location", "message stays on one line");
        assert_eq!(marks_in(&marks, BBox::point(isolated)).map(|m| m.id).collect::<Vec<_>>(), vec![20], "inclusive marker culling");
        assert_eq!(marks_in(&marks, all_bbox).map(|m| m.id).collect::<Vec<_>>(), vec![30, 10], "off-screen marker excluded");
        assert_eq!(marks_in(&marks, BBox::EMPTY).count(), 0, "empty view contains no markers");

        whole.severity = Severity::Error;
        whole.at = Some(isolated);
        let tied = check_marks(&[single.clone(), whole], &[], unit, board, &index);
        assert_eq!(tied.iter().map(|m| m.id).collect::<Vec<_>>(), vec![10, 30], "ids break severity ties");
        assert_eq!(tied[0].at, isolated, "explicit location overrides shape centre");
        assert!(marks_in(&tied, all_bbox).all(|m| m.id != 10), "culling uses the marker, not the shape bounds");
        single.items = vec![Sel::Feature(FeatureRef::Pad(CompRef { board: unit, comp: part.comp }, PadIx(u16::MAX)))];
        assert!(check_marks(std::slice::from_ref(&single), &[], unit, board, &index).is_empty(), "missing pad without a position is omitted");
        assert!(violation_levels(std::slice::from_ref(&single), unit, &index).is_empty(), "a missing pad does not light its placement");

        position.message = "é".repeat(49);
        assert_eq!(label(&position), format!("{}…", "é".repeat(48)), "truncate Unicode characters, not bytes");
        position.message = "é".repeat(48);
        assert_eq!(label(&position), position.message, "no ellipsis at the limit");
        for (nm, expected) in [(0, "0 mm"), (mm(1.0), "1 mm"), (mm(1.2346), "1.235 mm"), (-1, "0 mm")] {
            assert_eq!(measure(Measure::Len(nm)), expected, "millimetre precision and zero trimming");
        }
        let qty = Quantity { si: 0.2, unit: QtyUnit::Ampere };
        position.actual = Some(Measure::Qty(qty));
        position.required = Some(Measure::Qty(Quantity { si: 0.1, ..qty }));
        assert_eq!(label(&position), format!("{qty:?} > {:?}", Quantity { si: 0.1, ..qty }), "quantities use their Debug and compare measured to required");
        position.message = "text over pad 2 of R4".into();
        position.actual = Some(Measure::Len(0));
        position.required = Some(Measure::Len(0));
        assert_eq!(label(&position), "text over pad 2 of R4", "equal measures show no fault: the message labels the finding");
    }
}

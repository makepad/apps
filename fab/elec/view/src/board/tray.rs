//! The Board pane's unplaced circuit parts and their placement-gesture entry rows.

use fab_elec_board::{BoardOp, Item, Origin, Placed};
use fab_elec_core::{Angle, BBox, BoardId, CompId, ItemId, Side, P};
use fab_elec_design::{Design, DesignOp};
use fab_elec_parts::{Footprint, PartKind};
use makepad_widgets_core::*;
use std::collections::{HashMap, HashSet};

/// Internal drag item identifying an unplaced board part, encoded as `"<unit>:<comp>"`.
pub const TRAY_PART: LiveId = live_id!(fab_elec_tray_part);

/// Reads the unit and component ids from an internal unplaced-part tray drag.
pub fn tray_drag_part(items: &[DragItem]) -> Option<(BoardId, CompId)> {
    items.iter().find_map(|item| {
        let DragItem::String { value, internal_id: Some(TRAY_PART) } = item else {
            return None;
        };
        let (unit, comp) = value.split_once(':')?;
        Some((BoardId(unit.parse().ok()?), CompId(comp.parse().ok()?)))
    })
}

/// A part of a board unit's circuit that the board does not place yet.
#[derive(Clone, Debug, PartialEq)]
pub struct TrayPart {
    /// The board unit whose circuit owns this component.
    pub unit: BoardId,
    pub comp: CompId,
    pub refdes: String,
    pub value: String,
    pub footprint: String,
    /// The footprint's courtyard (or pad) bbox in footprint nm, for the drag chip.
    pub bbox: BBox,
}

/// Every physical component of `unit`'s circuit with no `Placed` on its board, in refdes natural
/// order (R2 before R10). Empty when the unit has no board or does not exist.
pub fn unplaced(design: &Design, unit: BoardId) -> Vec<TrayPart> {
    let Some(unit) = design.unit(unit) else {
        return Vec::new();
    };
    let Some(board) = &unit.board else {
        return Vec::new();
    };
    let placed: HashSet<_> = board.placed.iter().map(|placed| placed.comp).collect();
    let mut parts: Vec<_> = unit
        .circuit
        .comps
        .iter()
        // A power flag exists only on the schematic and cannot be placed on the board.
        .filter(|comp| comp.part.kind != PartKind::PowerFlag && !placed.contains(&comp.id))
        .map(|comp| {
            // This is the same resolution used by the Rust fixtures' placement helper.
            let (footprint, bbox) = comp
                .part
                .footprint
                .as_ref()
                .map(|fp| (fp.name.clone(), footprint_bbox(fp)))
                .unwrap_or_else(|| (String::new(), BBox::EMPTY));
            TrayPart {
                unit: unit.id,
                comp: comp.id,
                refdes: comp.refdes.clone(),
                value: comp
                    .value
                    .as_ref()
                    .map(|value| value.text.clone())
                    .unwrap_or_default(),
                footprint,
                bbox,
            }
        })
        .collect();
    parts.sort_by(|a, b| refdes_key(&a.refdes).cmp(&refdes_key(&b.refdes)));
    parts
}

/// The op that places component `comp` of `unit`'s circuit on its board at `at` (board nm),
/// turned `rot`, on `side`: one `BoardOp::Add` of a new `Placed`, its id the board's next free
/// id (the op is applied to the committed board, so the id is the one the host would allocate).
/// `None` when the unit has no board, the component does not exist, has no footprint, or is
/// already placed.
pub fn placement_op(
    design: &Design,
    unit: BoardId,
    comp: CompId,
    at: P,
    rot: Angle,
    side: Side,
) -> Option<DesignOp> {
    let board_unit = design.unit(unit)?;
    let board = board_unit.board.as_ref()?;
    if board.placed.iter().any(|placed| placed.comp == comp) {
        return None;
    }
    let comp = board_unit.circuit.comps.iter().find(|c| c.id == comp)?;
    let footprint = comp.part.footprint.as_ref()?.clone();
    let pad_nets = comp
        .part
        .pad_pins()
        .into_iter()
        .map(|pin| {
            pin.and_then(|pin| comp.conns.iter().find(|&&(p, _)| p == pin).map(|&(_, net)| net))
        })
        .collect();
    let placed = Placed {
        id: ItemId(board.next_item),
        comp: comp.id,
        path: comp.path.clone(),
        refdes: comp.refdes.clone(),
        footprint,
        pad_nets,
        at,
        rot,
        side,
        locked: false,
        origin: Origin::Authored,
        body_z: Placed::band_of(comp.part.body.as_ref()),
        ref_label: None, value_label: None, legend: None,
        value: None,
    };
    Some(DesignOp::Board {
        unit,
        op: BoardOp::Add(Item::Placed(placed)),
    })
}

fn footprint_bbox(footprint: &Footprint) -> BBox {
    let courtyard = footprint.courtyard.bbox();
    if !courtyard.is_empty() {
        return courtyard;
    }
    footprint.pads.iter().fold(BBox::EMPTY, |bbox, pad| {
        std::iter::once(&pad.stack.front)
            .chain(pad.stack.inner.iter())
            .chain(pad.stack.back.iter())
            .fold(bbox, |bbox, shape| {
                bbox.union(shape.shape(pad.at, pad.rot).bbox())
            })
    })
}

// Compare the alpha prefix, numeric suffix and rest. Comparing significant digit
// count before digits gives numeric order without overflowing an integer parser.
fn refdes_key(refdes: &str) -> (&str, usize, &str, &str) {
    let prefix_end = refdes
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(refdes.len());
    let (prefix, suffix) = refdes.split_at(prefix_end);
    let number_end = suffix
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(suffix.len());
    let (number, rest) = suffix.split_at(number_end);
    let number = number.trim_start_matches('0');
    (prefix, number.len(), number, rest)
}

// Registers `mod.widgets.BoardTray` and its row template.
script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.BoardTrayBase = #(BoardTray::register_widget(vm))
    mod.widgets.BoardTray = set_type_default() do mod.widgets.BoardTrayBase{
        width: Fill
        height: Fit
        flow: Down
        padding: 6
        show_bg: false

        draw_chip: mod.draw.DrawQuad{
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 3.0)
                let ground = #x1f232b
                sdf.fill_keep(vec4(ground.xyz, 0.96))
                sdf.stroke(#x3a4150, 1.0)
                return sdf.result
            }
        }
        draw_chip_refdes: mod.draw.DrawText{
            color: #xe6e9ef
            text_style: theme.font_bold{font_size: 9}
        }
        draw_chip_value: mod.draw.DrawText{
            color: #x9aa3b2
            text_style: theme.font_regular{font_size: 9}
        }

        header := Label{
            width: Fill
            height: 22
            padding: 0
            align: Align{y: 0.5}
            text: "All parts placed"
            max_lines: 1
            draw_text +: {
                color: #xc8cedc
                text_style: theme.font_bold{font_size: 9}
            }
        }
        parts := FlatList{
            width: Fill
            height: Fit
            flow: Down
            spacing: 1
            drag_scrolling: false
            scroll_bars +: {show_scroll_x: false show_scroll_y: false}

            Row := SolidView{
                width: Fill
                height: 22
                flow: Right
                spacing: 4
                align: Align{y: 0.5}
                cursor: MouseCursor.Hand
                capture_overload: true
                grab_key_focus: false
                draw_bg +: {
                    hover: instance(0.0)
                    get_color: fn() {
                        return mix(#0000, #x1f232b, self.hover)
                    }
                }
                animator: Animator{
                    hover: {
                        default: @off
                        off: AnimatorState{
                            from: {all: Snap}
                            apply: {draw_bg: {hover: 0.0}}
                        }
                        on: AnimatorState{
                            from: {all: Snap}
                            apply: {draw_bg: {hover: 1.0}}
                        }
                    }
                }

                refdes := Label{
                    width: Fit
                    padding: 0
                    text: ""
                    flow: Right
                    text_overflow: Clip
                    draw_text +: {
                        color: #xc8cedc
                        text_style: theme.font_bold{font_size: 9}
                    }
                }
                value := Label{
                    width: Fill
                    padding: 0
                    text: ""
                    max_lines: 1
                    text_overflow: Ellipsis
                    draw_text +: {
                        color: #xc8cedc
                        text_style +: {font_size: 9}
                    }
                }
                footprint := Label{
                    width: Fill
                    padding: 0
                    text: ""
                    max_lines: 1
                    text_overflow: Ellipsis
                    draw_text +: {
                        color: #x6b7280
                        text_style +: {font_size: 8}
                    }
                }
            }
        }
    }
}

/// A tray interaction; the board host owns the placement preview and commit.
#[derive(Clone, Debug, Default)]
pub enum TrayAction {
    /// No tray interaction.
    #[default]
    None,
    /// The pointer entered the component's row.
    Hover(CompId),
    /// A finger pressed the component's row.
    Pick(CompId),
}

/// The Board pane's unplaced-parts tray, registered as `mod.widgets.BoardTray`.
#[derive(Script, ScriptHook, Widget)]
pub struct BoardTray {
    #[deref]
    view: View,
    #[live]
    chip_list: DrawList2d,
    #[live]
    draw_chip: DrawQuad,
    #[live]
    draw_chip_refdes: DrawText,
    #[live]
    draw_chip_value: DrawText,
    #[rust]
    parts: Vec<TrayPart>,
    #[rust]
    rows: HashMap<LiveId, usize>,
    #[rust]
    press: Option<(CompId, Vec2d)>,
    #[rust]
    chip: Option<Chip>,
}

struct Chip {
    comp: CompId,
    refdes: String,
    value: String,
    cursor: Vec2d,
}

fn row_id(comp: CompId) -> LiveId {
    LiveId::from_num(0, comp.0 as u64)
}

impl BoardTray {
    /// Replaces the rows and redraws the tray.
    pub fn set_parts(&mut self, cx: &mut Cx, parts: Vec<TrayPart>) {
        let rows = parts.iter().enumerate().map(|(i, part)| (row_id(part.comp), i)).collect();
        let old_parts = std::mem::replace(&mut self.parts, parts);
        let old_rows = std::mem::replace(&mut self.rows, rows);
        super::shared::BoardDraws::discard(cx, (old_parts, old_rows));
        self.view.redraw(cx);
    }

    /// Returns this tray's actions from the frame, for the parent's action handler.
    pub fn tray_actions(&self, actions: &Actions) -> Vec<TrayAction> {
        actions
            .filter_widget_actions_cast::<TrayAction>(self.widget_uid())
            .filter(|action| !matches!(action, TrayAction::None))
            .collect()
    }
}

impl Widget for BoardTray {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        match event {
            Event::Drag(event) => {
                if let Some(chip) = self.chip.as_mut() {
                    chip.cursor = event.abs;
                    cx.set_cursor(MouseCursor::Grabbing);
                    self.view.redraw(cx);
                    self.chip_list.redraw(cx);
                }
            }
            Event::DragEnd => {
                self.press = None;
                if self.chip.take().is_some() {
                    self.view.redraw(cx);
                    self.chip_list.redraw(cx);
                }
            }
            _ => {}
        }
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        let list = self.view.flat_list(cx, ids!(parts));
        let uid = self.widget_uid();
        let mut seen = HashSet::new();
        for (id, item) in list.items_with_actions(&actions) {
            // FlatList reports an item for each of its child actions.
            if !seen.insert(id) {
                continue;
            }
            let Some(part) = self.rows.get(&id).and_then(|&i| self.parts.get(i)) else {
                continue;
            };
            let row = item.as_view();
            if row.finger_hover_in(&actions).is_some() {
                cx.widget_action(uid, TrayAction::Hover(part.comp));
            }
            if let Some(down) = row.finger_down(&actions) {
                self.press = Some((part.comp, down.abs));
                self.chip = Some(Chip {
                    comp: part.comp,
                    refdes: part.refdes.clone(),
                    value: part.value.clone(),
                    cursor: down.abs,
                });
                self.view.redraw(cx);
                self.chip_list.redraw(cx);
                cx.widget_action(uid, TrayAction::Pick(part.comp));
            }
            if let Some(moved) = row.finger_move(&actions) {
                if let Some(chip) = self.chip.as_mut().filter(|chip| chip.comp == part.comp) {
                    chip.cursor = moved.abs;
                    self.view.redraw(cx);
                    self.chip_list.redraw(cx);
                }
                if self.press.is_some_and(|(comp, abs)| {
                    comp == part.comp && (moved.abs - abs).length() > 4.0
                }) {
                    self.press = None;
                    cx.start_dragging(vec![DragItem::String {
                        value: format!("{}:{}", part.unit.0, part.comp.0),
                        internal_id: Some(TRAY_PART),
                    }]);
                    cx.set_cursor(MouseCursor::Grabbing);
                }
            }
            if row.finger_up(&actions).is_some() && self.press.is_some() {
                self.press = None;
                self.chip = None;
                self.view.redraw(cx);
                self.chip_list.redraw(cx);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let header = if self.parts.is_empty() {
            "All parts placed".to_owned()
        } else {
            format!("Unplaced {}", self.parts.len())
        };
        self.view.label(cx, ids!(header)).set_text(cx, &header);
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<FlatList>() else {
                continue;
            };
            for part in &self.parts {
                let Some(row) = list.item(cx, row_id(part.comp), id!(Row)) else {
                    continue;
                };
                row.label(cx, ids!(refdes)).set_text(cx, &part.refdes);
                row.label(cx, ids!(value)).set_text(cx, &part.value);
                row.label(cx, ids!(footprint)).set_text(cx, &part.footprint);
                row.draw_all(cx, scope);
            }
            list.items.retain_visible();
        }
        if let Some(chip) = &self.chip {
            self.chip_list.begin_overlay_last(cx);
            let size = cx.current_pass_size();
            cx.begin_root_turtle(size, Layout::default());
            self.draw_chip.begin(
                cx,
                Walk { abs_pos: Some(chip.cursor + dvec2(14.0, 10.0)), ..Walk::fit() },
                Layout {
                    padding: Inset { top: 4.0, bottom: 4.0, left: 8.0, right: 8.0 },
                    flow: Flow::right(),
                    spacing: 6.0,
                    ..Layout::default()
                },
            );
            self.draw_chip_refdes.draw_walk(cx, Walk::fit(), Align::default(), &chip.refdes);
            self.draw_chip_value.draw_walk(cx, Walk::fit(), Align::default(), &chip.value);
            self.draw_chip.end(cx);
            cx.end_pass_sized_turtle();
            self.chip_list.end(cx);
        } else {
            self.chip_list.begin_always(cx);
            self.chip_list.end(cx);
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn power_flag_stays_out_of_the_placement_tray() {
        use fab_elec_lang::{evaluate, std_libs, Sources};

        let mut sources = Sources::new();
        sources.add("tray.ckt.splash", r#"let e = mod.elec
let flag = e.part(mod.std.power_flag(), {})
let r = e.r(1kohm)
let sensor = e.part(e.define("Sensor", {kind: @ic symbol: e.f.ic_symbol("Sensor", {})}), {})
"#).expect("the tray circuit source has a valid name");
        let design = evaluate(&sources, &std_libs()).expect("the tray circuit evaluates").design;
        let unit = &design.units[0];
        let parts = unplaced(&design, unit.id);
        let refs: Vec<_> = parts.iter().map(|part| part.refdes.as_str()).collect();
        assert_eq!(refs, ["R1", "U1"], "the tray keeps physical parts, including one awaiting a footprint, and omits the power flag");
        let flag = unit.circuit.comp("flag").expect("the schematic contains the flag");
        assert!(placement_op(&design, unit.id, flag.id, P::ZERO, Angle::ZERO, Side::Top).is_none(),
            "a direct placement request for the schematic flag also produces no board edit");
    }

    #[test]
    fn removed_placements_become_tray_parts() {
        let fixture = fab_elec_fixtures::ledctl();
        let mut design = fixture.clone();
        let unit = design
            .units
            .iter_mut()
            .find(|unit| unit.board.is_some())
            .unwrap();
        let unit_id = unit.id;
        let mut board = (**unit.board.as_ref().unwrap()).clone();
        let removed: Vec<_> = board.placed.drain(..2).collect();
        unit.board = Some(Arc::new(board));

        let parts = unplaced(&design, unit_id);
        // The fixture's first two placements are J1 and R1, already in natural order.
        let expected: Vec<_> = removed.iter().map(|placed| placed.refdes.as_str()).collect();
        assert_eq!(
            parts.iter().map(|part| part.refdes.as_str()).collect::<Vec<_>>(),
            expected,
            "only the removed placements belong in the tray, in natural refdes order"
        );
        for part in &parts {
            assert!(
                removed.iter().any(|placed| placed.comp == part.comp),
                "tray component identity"
            );
            assert!(
                !part.footprint.is_empty(),
                "{} has a resolved footprint",
                part.refdes
            );
            assert!(
                !part.bbox.is_empty(),
                "{} has footprint bounds",
                part.refdes
            );
        }
        assert!(
            unplaced(&fixture, unit_id).is_empty(),
            "the untouched fixture is fully placed"
        );

        let placed = &design.unit(unit_id).unwrap().board.as_ref().unwrap().placed[0];
        assert!(
            placement_op(
                &design,
                unit_id,
                placed.comp,
                placed.at,
                placed.rot,
                placed.side,
            )
            .is_none(),
            "an already placed component cannot be placed again"
        );
        {
            let mut design = design.clone();
            for removed in &removed {
                let mut board = (**design.unit(unit_id).unwrap().board.as_ref().unwrap()).clone();
                let next_item = board.next_item;
                let Some(DesignOp::Board { unit, op }) = placement_op(
                    &design,
                    unit_id,
                    removed.comp,
                    removed.at,
                    removed.rot,
                    removed.side,
                ) else {
                    panic!("an unplaced component produces a board placement op");
                };
                assert_eq!(unit, unit_id, "the placement op targets the component's unit");
                board
                    .apply(&op)
                    .expect("the placement op applies to the board");
                let mut placed = board
                    .placed
                    .iter()
                    .find(|placed| placed.comp == removed.comp)
                    .expect("the board now places the removed component")
                    .clone();
                assert_eq!(
                    placed.id,
                    ItemId(next_item),
                    "placement uses the next free item id"
                );
                assert_eq!(
                    board.next_item,
                    next_item + 1,
                    "placement advances the next item id"
                );
                placed.id = removed.id;
                assert_eq!(
                    &placed, removed,
                    "placement matches every fixture field except id"
                );
                design
                    .units
                    .iter_mut()
                    .find(|unit| unit.id == unit_id)
                    .unwrap()
                    .board = Some(Arc::new(board));
            }
            assert!(
                unplaced(&design, unit_id).is_empty(),
                "restoring both placements empties the tray"
            );
        }

        // Give those same two components a numeric suffix that lexical order gets wrong.
        let circuit = Arc::make_mut(&mut design.units[0].circuit);
        circuit.comps[removed[0].comp.ix()].refdes = "R10".into();
        circuit.comps[removed[1].comp.ix()].refdes = "R2".into();
        assert_eq!(
            unplaced(&design, unit_id)
                .iter()
                .map(|part| part.refdes.as_str())
                .collect::<Vec<_>>(),
            ["R2", "R10"],
            "numeric refdes suffixes sort naturally"
        );
    }
}

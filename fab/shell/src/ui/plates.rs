//! The Plates panel, the first tab on the left: New plate and a search at the top, then every
//! plate as a card in a grid (as many columns as the panel's width holds), the most recently
//! edited first, each with its picture, its name (a `*` while its
//! edits are not in its file), its printer, when it was last edited and where it came from;
//! then the demos. A click puts a plate on the bed (the one it replaces keeps every edit); a
//! demo opens as it ships and its first edit makes the person's own plate. A plate's menu
//! (right-click or its ⋯) duplicates it, renames it, takes it off the list with its files kept,
//! or, after a confirm in its row, moves its files to the Trash. Under Demos, the demos without a
//! group come first, then each group ("Studio") under a head of its own, in the demos' order.
//!
//! The list is the model worker's (`AppState::plates`): the pictures are the ones saved with
//! the plates, read by the worker, so listing never renders a model.

use crate::api::*;
use crate::ui::popover::{menu_picked, open_menu, MenuItem, MenuPlace};
use makepad_widgets_core::tip::{TipAction, TipRequest};
use makepad_widgets_core::*;
use std::collections::HashMap;
use std::sync::Arc;

const PLATE_MENU: LiveId = live_id!(fab_plates_context);

/// One line of the list.
#[derive(Clone, Debug, PartialEq)]
enum Line {
    Head(&'static str),
    /// A group of demos, under the Demos head.
    Group(String),
    /// Index into [`cards`].
    Card(usize),
    Note(&'static str),
}

/// The plates, then the demos.
fn cards(list: &PlateList) -> Vec<&PlateCard> {
    list.plates.iter().chain(list.demos.iter()).collect()
}

/// A card matches a search when its name or its note holds every word of it.
fn matches(card: &PlateCard, query: &str) -> bool {
    let hay = format!("{} {} {}", card.name, card.note, card.printer).to_lowercase();
    query.split_whitespace().all(|w| hay.contains(&w.to_lowercase()))
}

fn lines(list: &PlateList, query: &str) -> Vec<Line> {
    let all = cards(list);
    let mut out = vec![Line::Head("Plates")];
    let plates: Vec<usize> = (0..list.plates.len()).filter(|&i| matches(all[i], query)).collect();
    if plates.is_empty() {
        out.push(Line::Note(if list.plates.is_empty() { "No plates yet. New plate starts one; a demo becomes your own plate when you edit it." } else { "No plate matches." }));
    }
    out.extend(plates.into_iter().map(Line::Card));
    let demos: Vec<usize> = (list.plates.len()..all.len()).filter(|&i| matches(all[i], query)).collect();
    if !demos.is_empty() {
        out.push(Line::Head("Demos"));
        out.extend(demos.iter().filter(|&&i| all[i].group.is_empty()).map(|&i| Line::Card(i)));
        let mut groups: Vec<&str> = Vec::new();
        for &i in &demos {
            let g = all[i].group.as_str();
            if !g.is_empty() && !groups.contains(&g) {
                groups.push(g);
            }
        }
        for g in groups {
            out.push(Line::Group(g.to_string()));
            out.extend(demos.iter().filter(|&&i| all[i].group == g).map(|&i| Line::Card(i)));
        }
    }
    out
}

/// The narrowest a card is drawn, the gap between cards and the grid's side padding (points),
/// and the most cards a grid row holds (its template's slots).
const TILE_MIN: f64 = 104.0;
const GAP: f64 = 8.0;
const PAD: f64 = 8.0;
const SLOTS: usize = 6;

/// How many cards a row of a panel `width` wide holds.
fn columns(width: f64) -> usize {
    (((width - 2.0 * PAD + GAP) / (TILE_MIN + GAP)).floor().max(1.0) as usize).min(SLOTS)
}

/// One item of the list as drawn: a head, a group or a note line, or a row of cards (their lines).
#[derive(Clone, Debug, PartialEq)]
enum Disp {
    Line(usize),
    Cards(Vec<usize>),
}

/// The lines laid out in rows of `cols` cards; heads, groups and notes stand on their own.
fn grid(lines: &[Line], cols: usize) -> Vec<Disp> {
    let mut out = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    for (l, line) in lines.iter().enumerate() {
        if matches!(line, Line::Card(_)) {
            run.push(l);
            if run.len() == cols { out.push(Disp::Cards(std::mem::take(&mut run))); }
        } else {
            if !run.is_empty() { out.push(Disp::Cards(std::mem::take(&mut run))); }
            out.push(Disp::Line(l));
        }
    }
    if !run.is_empty() { out.push(Disp::Cards(run)); }
    out
}

/// A card slot's name in a grid row.
fn slot(k: usize) -> &'static [LiveId; 1] {
    [ids!(t0), ids!(t1), ids!(t2), ids!(t3), ids!(t4), ids!(t5)][k.min(SLOTS - 1)]
}

/// The second line of a card: its printer, when it was last edited, where it came from.
fn info(card: &PlateCard) -> String {
    [card.printer.as_str(), card.when.as_str(), card.note.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// A row's tooltip: its line in full, and why its note says what it does.
fn tip(card: &PlateCard) -> String {
    [info(card), card.why.clone()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")
}

/// A card's key for the list's semantics.
fn key(card: &PlateCard) -> u64 {
    let mut h = if card.demo { 0x6465_6d6f_u64 } else { 0x706c_6174_u64 };
    for b in card.id.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

/// What opening a card does.
pub fn open_action(card: &PlateCard) -> ShellAction {
    if card.demo { ShellAction::OpenDemo(card.id.clone()) } else { ShellAction::OpenPlate(card.id.clone()) }
}

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    let ToolIcon = mod.widgets.FabIconButton{
        width: 24
        height: 22
        draw_icon +: {color: fab.color_text_dim}
    }

    // A plate's card: its picture (or a quiet cube until it has one) with its menu in the
    // corner, its name and its line. The plate on the bed is outlined in the accent.
    let Tile = View{
        width: 120
        height: Fit
        flow: Down
        padding: Inset{left: 5 right: 5 top: 5 bottom: 6}
        spacing: 3
        cursor: MouseCursor.Hand
        show_bg: true
        draw_bg +: {
            selected: instance(0.0)
            hover: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                sdf.fill_keep(fab.color_editor.mix(fab.color_row_hover, self.hover * 0.6).mix(fab.color_selection_bg, self.selected))
                sdf.stroke(vec4(fab.color_accent.xyz, self.selected), 1.0)
                return sdf.result
            }
        }
        animator: Animator{
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: { draw_bg: {hover: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {hover: 1.0} }
                }
            }
        }
        pic := View{
            width: Fill
            height: 80
            flow: Overlay
            align: Align{x: 0.5 y: 0.5}
            show_bg: true
            draw_bg +: {
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius)
                    sdf.fill_keep(fab.color_input)
                    sdf.stroke(fab.color_border, 1.0)
                    return sdf.result
                }
            }
            none := mod.widgets.FabIconMuted{
                width: 22
                height: 22
                icon_walk: Walk{ width: 22 height: 22 }
                draw_icon +: { svg: crate_resource("self://resources/icons/cube.svg") }
            }
            thumb := Image{
                width: Fill
                height: Fill
                fit: ImageFit.CropToFill
            }
            corner := View{
                width: Fill
                height: Fill
                align: Align{x: 1.0 y: 0.0}
                padding: Inset{top: 2 right: 2}
                more := ToolIcon{
                    a11y: {label: "Plate menu"}
                    draw_icon +: {svg: crate_resource("self://resources/icons/dots.svg")}
                }
            }
        }
        name := mod.widgets.FabLabel{ width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis }
        rename := mod.widgets.FabInput{ width: Fill height: 22 visible: false a11y: {label: "Rename plate"} }
        // cut short, it shows in full in its tooltip (the panel's: see `hover_tip`)
        info := mod.widgets.FabLabelSmall{ width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis }
        confirm := View{
            width: Fill
            height: Fit
            visible: false
            flow: Down
            spacing: 3
            trash := mod.widgets.FabButton{ width: Fill text: "Move to Trash" }
            keep := mod.widgets.FabButton{ width: Fill text: "Cancel" }
        }
    }

    mod.widgets.FabPlatesPanelBase = #(FabPlatesPanel::register_widget(vm))
    mod.widgets.FabPlatesPanel = set_type_default() do mod.widgets.FabPlatesPanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        tools := View{
            width: Fill
            height: 36
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 8 right: 8 top: 0 bottom: 0}
            spacing: 6
            new := mod.widgets.FabButtonAccent{ text: "New plate" a11y: {label: "New plate"} }
            search := mod.widgets.FabSearch{
                width: Fill{min: 60}
                row +: { input +: { empty_text: "Search plates" a11y: {label: "Search plates"} } }
            }
        }
        mod.widgets.FabHr{}
        list := PortalList{
            width: Fill
            height: Fill
            flow: Down
            auto_tail: false
            Head := View{
                width: Fill
                height: 30
                align: Align{x: 0.0 y: 0.5}
                padding: Inset{left: 10 right: 10 top: 6 bottom: 0}
                title := mod.widgets.FabGroupTitle{ text: "" }
            }
            Group := View{
                width: Fill
                height: 26
                align: Align{x: 0.0 y: 0.5}
                padding: Inset{left: 18 right: 10 top: 4 bottom: 0}
                title := mod.widgets.FabLabelDim{ text: "" }
            }
            Note := View{
                width: Fill
                height: Fit
                padding: Inset{left: 10 right: 10 top: 6 bottom: 8}
                text := mod.widgets.FabLabelDim{ width: Fill text: "" }
            }
            // A row of plate cards: as many slots as the panel's width holds show, each sized to
            // its share of the width.
            Grid := View{
                width: Fill
                height: Fit
                flow: Right
                spacing: #(GAP)
                padding: Inset{left: #(PAD) right: #(PAD) top: 4 bottom: 4}
                t0 := Tile{}
                t1 := Tile{}
                t2 := Tile{}
                t3 := Tile{}
                t4 := Tile{}
                t5 := Tile{}
            }
        }
    }
}

/// A line as last drawn: where, for the semantics and the menu's anchor. Kept as areas, whose
/// rects follow the list's scroll (it moves what it drew without drawing it again).
#[derive(Clone, Default)]
struct Shown {
    row: Area,
    /// Its line (the printer, when, the note): hovered, its tooltip shows.
    line: Area,
}

impl Shown {
    fn rect(&self, cx: &Cx) -> Option<Rect> {
        self.row.is_valid(cx).then(|| self.row.rect(cx))
    }

    fn line(&self, cx: &Cx) -> Option<Rect> {
        self.line.is_valid(cx).then(|| self.line.rect(cx))
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabPlatesPanel {
    #[deref]
    view: View,
    /// The search, as typed.
    #[rust]
    query: String,
    /// The list as last built, and the plates it was built from.
    #[rust]
    lines: Vec<Line>,
    #[rust]
    cards: Vec<PlateCard>,
    #[rust]
    shown: Vec<Shown>,
    /// The list positions drawn last frame (their items exist).
    #[rust]
    visible: Vec<usize>,
    /// The plate whose name is being edited (its id, and whether the field still needs focus).
    #[rust]
    rename: Option<(String, bool)>,
    /// The plate whose row asks whether to move its files to the Trash.
    #[rust]
    confirm: Option<String>,
    /// The plate the open menu is for.
    #[rust]
    menu_for: Option<String>,
    /// The card each picture widget last loaded (a reused row loads its new card's).
    #[rust]
    loaded: HashMap<WidgetUid, (String, usize)>,
    /// The card whose line the pointer is over, its tooltip raised (its id, and whether a demo).
    /// A press over it takes the tip down (the layer does) until the pointer leaves the line.
    #[rust]
    tip_for: Option<(String, bool)>,
}

impl FabPlatesPanel {
    /// The grid's columns at the list's width as last drawn (as its draw laid them out).
    fn cols(&self, cx: &mut Cx) -> usize {
        columns(self.view.portal_list(cx, ids!(list)).area().rect(cx).size.x)
    }

    /// Where card line `line` is drawn: its grid row's list position and its slot there.
    fn locate(&self, cols: usize, line: usize) -> Option<(usize, usize)> {
        grid(&self.lines, cols).iter().enumerate().find_map(|(pos, d)| match d {
            Disp::Cards(ls) => ls.iter().position(|&l| l == line).map(|k| (pos, k)),
            _ => None,
        })
    }

    fn card(&self, line: usize) -> Option<&PlateCard> {
        match self.lines.get(line)? {
            Line::Card(i) => self.cards.get(*i),
            _ => None,
        }
    }

    fn open(&mut self, cx: &mut Cx, card: &PlateCard, state: &AppState) {
        let on_bed = state.plates.on_bed.as_deref() == Some(card.id.as_str()) && state.plates.on_demo == card.demo;
        if !on_bed {
            cx.action(open_action(card));
        }
    }

    fn open_menu_for(&mut self, cx: &mut Cx, card: &PlateCard, anchor: Rect) {
        if card.demo {
            return;
        }
        self.menu_for = Some(card.id.clone());
        let items = vec![
            MenuItem::new(live_id!(plate_open), "Open"),
            MenuItem::new(live_id!(plate_duplicate), "Duplicate"),
            MenuItem::new(live_id!(plate_rename), "Rename"),
            MenuItem::sep(),
            MenuItem::new(live_id!(plate_remove), "Remove from List"),
            MenuItem::new(live_id!(plate_delete), "Move to Trash…"),
        ];
        open_menu(cx, PLATE_MENU, items, anchor, MenuPlace::At);
    }

    fn picked(&mut self, cx: &mut Cx, id: LiveId) {
        let Some(plate) = self.menu_for.take() else { return };
        if id == live_id!(plate_open) {
            cx.action(ShellAction::OpenPlate(plate));
        } else if id == live_id!(plate_duplicate) {
            cx.action(ShellAction::DuplicatePlate(plate));
        } else if id == live_id!(plate_rename) {
            self.confirm = None;
            self.rename = Some((plate, true));
        } else if id == live_id!(plate_remove) {
            cx.action(ShellAction::RemovePlate(plate));
        } else if id == live_id!(plate_delete) {
            self.rename = None;
            self.confirm = Some(plate);
        }
        self.view.redraw(cx);
    }

    /// Raise the tooltip of the line the pointer is over, or take it down: a row's line is cut short
    /// in a narrow panel, and a note (this Fab cannot read its file) says why only in full. The
    /// panel raises it, not a wrapper round the line: the row claims the pointer over its line.
    fn hover_tip(&mut self, cx: &mut Cx, over: Option<usize>) {
        let card = over.and_then(|l| Some((self.card(l)?, self.shown.get(l)?.line(cx)?)));
        let key = card.map(|(c, _)| (c.id.clone(), c.demo));
        if key == self.tip_for {
            return;
        }
        let action = match card {
            Some((c, line)) => TipAction::HoverInWith(TipRequest { wrap_width: 320.0, ..TipRequest::plain(tip(c), line) }),
            None => TipAction::HoverOut,
        };
        self.tip_for = key;
        cx.widget_action(self.widget_uid(), action);
    }

    fn sync(&mut self, state: &AppState) {
        let cards: Vec<PlateCard> = cards(&state.plates).into_iter().cloned().collect();
        if cards != self.cards || self.lines.is_empty() {
            self.cards = cards;
            self.lines = lines(&state.plates, &self.query);
        }
        let live = |id: &str| state.plates.plates.iter().any(|p| p.id == id);
        if self.rename.as_ref().is_some_and(|(id, _)| !live(id)) {
            self.rename = None;
        }
        if self.confirm.as_ref().is_some_and(|id| !live(id)) {
            self.confirm = None;
        }
        self.shown.resize(self.lines.len(), Shown::default());
    }
}

impl Widget for FabPlatesPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "Plates");
        node.set_size = self.lines.iter().filter(|l| matches!(l, Line::Card(_))).count() as u32;
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let mut pos = 0;
        for (line, shown) in self.lines.iter().zip(&self.shown) {
            let Line::Card(i) = line else { continue };
            let Some(card) = self.cards.get(*i) else { continue };
            pos += 1;
            let rect = shown.rect(cx);
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::ListItem,
                bounds: makepad_platform::a11y::semantic_rect(rect.unwrap_or_default()),
                pos_in_set: pos,
                actions: a11y_tree::Actions::PRESS | a11y_tree::Actions::SHOW_MENU,
                ..Default::default()
            };
            makepad_platform::a11y::set_string(&mut node.name, &card.title());
            let mut description = info(card);
            if card.demo {
                description = if description.is_empty() { "demo".to_string() } else { format!("demo · {description}") };
            }
            makepad_platform::a11y::set_string(&mut node.description, &description);
            node.states.set(a11y_tree::States::SELECTABLE, true);
            node.states.set(a11y_tree::States::OFFSCREEN, rect.is_none());
            out.push((key(card), None, node));
        }
    }

    fn semantics_part_area(&self, k: u64) -> Area {
        // a drawn row: one the list scrolled past an edge is offscreen
        self.visible.iter().copied().find(|&l| self.card(l).is_some_and(|c| key(c) == k)).map_or(Area::Empty, |l| self.shown[l].row)
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        let Some(k) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some(line) = (0..self.lines.len()).find(|&l| self.card(l).is_some_and(|c| key(c) == k)) else { return ActionResult::Unsupported };
        let card = self.card(line).cloned().unwrap();
        if request.action == Actions::PRESS {
            let Some(state) = scope.data.get::<AppState>() else { return ActionResult::Unhandled };
            self.open(cx, &card, state);
        } else if request.action == Actions::SHOW_MENU {
            let rect = self.shown[line].rect(cx).unwrap_or_default();
            self.open_menu_for(cx, &card, Rect { pos: rect.pos + dvec2(rect.size.x * 0.5, rect.size.y * 0.5), size: dvec2(0.0, 0.0) });
        } else {
            return ActionResult::Unhandled;
        }
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        match event {
            Event::MouseMove(e) => {
                let list = self.view.widget(cx, ids!(list)).area().clipped_rect(cx);
                let over = list.contains(e.abs).then(|| self.visible.iter().copied().find(|&l| self.shown.get(l).and_then(|s| s.line(cx)).is_some_and(|r| r.contains(e.abs)))).flatten();
                self.hover_tip(cx, over);
            }
            Event::MouseLeave(_) | Event::ClearHover => self.hover_tip(cx, None),
            // the layer took the tip down, and the rows move under the pointer: the line it is
            // over when it next moves raises its tip (a press leaves it down while still over it)
            Event::Scroll(_) => self.tip_for = None,
            _ => {}
        }
        let Event::Actions(actions) = event else { return };
        if let Some(id) = menu_picked(actions, PLATE_MENU) {
            self.picked(cx, id);
        }
        if self.view.button(cx, ids!(tools.new)).clicked(actions) {
            cx.action(ShellAction::Command("new plate".into()));
        }
        if let Some(text) = self.view.text_input(cx, ids!(tools.search.row.input)).changed(actions) {
            self.query = text;
            self.lines.clear();
            self.view.redraw(cx);
        }
        let Some(state) = scope.data.get::<AppState>() else { return };
        let list = self.view.portal_list(cx, ids!(list));
        let cols = self.cols(cx);
        for &line in &self.visible.clone() {
            let Some(card) = self.card(line).cloned() else { continue };
            let Some((pos, k)) = self.locate(cols, line) else { continue };
            let Some((_, row)) = list.get_item(pos) else { continue };
            let item = row.widget(cx, slot(k));
            if !card.demo && self.rename.as_ref().is_some_and(|(id, _)| *id == card.id) {
                let input = item.text_input(cx, ids!(rename));
                if input.escaped(actions) {
                    self.rename = None;
                    self.view.redraw(cx);
                } else if input.returned(actions).is_some() || input.key_focus_lost(actions) {
                    let name = input.text().trim().to_string();
                    self.rename = None;
                    if !name.is_empty() && name != card.name {
                        cx.action(ShellAction::RenamePlate(card.id.clone(), name));
                    }
                    self.view.redraw(cx);
                }
                continue;
            }
            if self.confirm.as_deref() == Some(card.id.as_str()) && !card.demo {
                if item.button(cx, ids!(confirm.trash)).clicked(actions) {
                    self.confirm = None;
                    cx.action(ShellAction::DeletePlate(card.id.clone()));
                    self.view.redraw(cx);
                    continue;
                }
                if item.button(cx, ids!(confirm.keep)).clicked(actions) {
                    self.confirm = None;
                    self.view.redraw(cx);
                    continue;
                }
            }
            if item.button(cx, ids!(pic.corner.more)).clicked(actions) {
                let r = item.widget(cx, ids!(pic.corner.more)).area().rect(cx);
                self.open_menu_for(cx, &card, Rect { pos: r.pos + dvec2(0.0, r.size.y), size: dvec2(0.0, 0.0) });
                continue;
            }
            if let Some(up) = item.as_view().finger_up(actions) {
                if !up.cancelled && up.is_over && up.device.is_primary_hit() {
                    if up.tap_count == 2 && !card.demo {
                        self.confirm = None;
                        self.rename = Some((card.id.clone(), true));
                        self.view.redraw(cx);
                    } else {
                        self.open(cx, &card, state);
                    }
                } else if !up.cancelled && up.is_over && !up.is_sweep
                    && up.device.mouse_button().is_some_and(|b| b.is_secondary())
                    && (up.abs - up.abs_start).length() < crate::nav::DRAG_THRESHOLD_PT
                {
                    self.open_menu_for(cx, &card, Rect { pos: up.abs, size: dvec2(0.0, 0.0) });
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get::<AppState>() else { return self.view.draw_walk(cx, scope, walk) };
        self.sync(state);
        let bed = (state.plates.on_bed.clone(), state.plates.on_demo);
        for shown in &mut self.shown {
            *shown = Shown::default();
        }
        self.visible.clear();
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = step.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else { continue };
            // the grid at the list's width as it draws now
            let width = cx.turtle().inner_rect().size.x;
            let cols = columns(width);
            let tile_w = ((width - 2.0 * PAD - (cols as f64 - 1.0) * GAP) / cols as f64).max(TILE_MIN.min(width - 2.0 * PAD).max(40.0));
            let disp = grid(&self.lines, cols);
            list.set_item_range(cx, 0, disp.len());
            while let Some(pos) = list.next_visible_item(cx) {
                let Some(d) = disp.get(pos).cloned() else { continue };
                match d {
                    Disp::Line(l) => {
                        self.visible.push(l);
                        match self.lines.get(l).cloned() {
                            Some(Line::Head(title)) => {
                                let item = list.item(cx, pos, live_id!(Head));
                                item.label(cx, ids!(title)).set_text(cx, title);
                                item.draw_all(cx, &mut Scope::empty());
                            }
                            Some(Line::Group(title)) => {
                                let item = list.item(cx, pos, live_id!(Group));
                                item.label(cx, ids!(title)).set_text(cx, &title);
                                item.draw_all(cx, &mut Scope::empty());
                            }
                            Some(Line::Note(text)) => {
                                let item = list.item(cx, pos, live_id!(Note));
                                item.label(cx, ids!(text)).set_text(cx, text);
                                item.draw_all(cx, &mut Scope::empty());
                            }
                            _ => {}
                        }
                    }
                    Disp::Cards(ls) => {
                        let row = list.item(cx, pos, live_id!(Grid));
                        let mut focus_line = None;
                        for k in 0..SLOTS {
                            let item = row.widget(cx, slot(k));
                            let Some(&line) = ls.get(k) else {
                                item.set_visible(cx, false);
                                continue;
                            };
                            let Some(Line::Card(i)) = self.lines.get(line).cloned() else { continue };
                            let Some(card) = self.cards.get(i) else { continue };
                            self.visible.push(line);
                            item.set_visible(cx, true);
                            if let Some(mut view) = item.borrow_mut::<View>() {
                                view.walk.width = Size::Fixed(tile_w);
                            }
                            if let Some(mut pic) = item.view(cx, ids!(pic)).borrow_mut() {
                                pic.walk.height = Size::Fixed(((tile_w - 10.0) * 0.72).round());
                            }
                            let on_bed = bed.0.as_deref() == Some(card.id.as_str()) && bed.1 == card.demo;
                            if let Some(mut view) = item.borrow_mut::<View>() {
                                view.draw_bg.set_dyn_instance(cx, live_id!(selected), &[if on_bed { 1.0 } else { 0.0 }]);
                            }
                            item.label(cx, ids!(name)).set_text(cx, &card.title());
                            item.label(cx, ids!(info)).set_text(cx, &info(card));
                            let renaming = self.rename.as_ref().filter(|(id, _)| *id == card.id && !card.demo).map(|(_, focus)| *focus);
                            let confirming = !card.demo && self.confirm.as_deref() == Some(card.id.as_str());
                            item.widget(cx, ids!(name)).set_visible(cx, renaming.is_none());
                            item.widget(cx, ids!(rename)).set_visible(cx, renaming.is_some());
                            item.widget(cx, ids!(info)).set_visible(cx, !confirming);
                            item.widget(cx, ids!(confirm)).set_visible(cx, confirming);
                            item.widget(cx, ids!(pic.corner.more)).set_visible(cx, !card.demo);
                            if renaming == Some(true) {
                                item.text_input(cx, ids!(rename)).set_text(cx, &card.name);
                                focus_line = Some(k);
                            }
                            let thumb = item.image(cx, ids!(pic.thumb));
                            item.widget(cx, ids!(pic.none)).set_visible(cx, card.picture.is_none());
                            match &card.picture {
                                Some(png) => {
                                    thumb.set_visible(cx, true);
                                    // a picture is loaded again only for another card or a new picture
                                    let mark = (card.id.clone(), Arc::as_ptr(png) as *const u8 as usize);
                                    if self.loaded.get(&thumb.widget_uid()) != Some(&mark) && thumb.load_png_from_data(cx, png).is_ok() {
                                        self.loaded.insert(thumb.widget_uid(), mark);
                                    }
                                }
                                None => {
                                    thumb.set_visible(cx, false);
                                    self.loaded.remove(&thumb.widget_uid());
                                }
                            }
                        }
                        row.draw_all(cx, &mut Scope::empty());
                        for (k, &line) in ls.iter().enumerate() {
                            let item = row.widget(cx, slot(k));
                            let confirming = self.card(line).is_some_and(|c| !c.demo && self.confirm.as_deref() == Some(c.id.as_str()));
                            if let Some(shown) = self.shown.get_mut(line) {
                                shown.row = item.area();
                                shown.line = if confirming { Area::Empty } else { item.widget(cx, ids!(info)).area() };
                            }
                        }
                        if let Some(k) = focus_line {
                            let input = row.widget(cx, slot(k)).text_input(cx, ids!(rename));
                            input.set_key_focus(cx);
                            if let Some(mut input) = input.borrow_mut() {
                                input.select_all(cx);
                            }
                            if let Some((_, focus)) = &mut self.rename {
                                *focus = false;
                            }
                        }
                    }
                }
            }
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(name: &str, group: &str, demo: bool) -> PlateCard {
        PlateCard { id: name.into(), demo, group: group.into(), name: name.into(), modified: false, printer: String::new(), when: String::new(), file: None, note: String::new(), why: String::new(), picture: None }
    }

    /// Under Demos the ungrouped demos come first, then each group under its head in the order
    /// its first demo has in the list, every group's demos together; a search keeps a group's
    /// head only while one of its demos matches.
    #[test]
    fn demos_list_the_ungrouped_then_each_group() {
        let list = PlateList {
            plates: vec![card("Mine", "", false)],
            demos: vec![card("Vase", "", true), card("Car", "Studio", true), card("Dragon", "Showpieces", true), card("Cube", "", true), card("Puppet", "Studio", true)],
            on_bed: None,
            on_demo: false,
        };
        let g = |s: &str| Line::Group(s.to_string());
        assert_eq!(lines(&list, ""), vec![Line::Head("Plates"), Line::Card(0), Line::Head("Demos"), Line::Card(1), Line::Card(4), g("Studio"), Line::Card(2), Line::Card(5), g("Showpieces"), Line::Card(3)]);
        assert_eq!(lines(&list, "puppet"), vec![Line::Head("Plates"), Line::Note("No plate matches."), Line::Head("Demos"), g("Studio"), Line::Card(5)]);
    }
}

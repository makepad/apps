//! The Plates panel, the first tab on the left: New plate and a search at the top, then every
//! plate as a card in a grid (as many columns as the panel's width holds), the most recently
//! edited first, each with its picture, its name (a `*` while its
//! edits are not in its file), its printer, when it was last edited and where it came from;
//! then the demos. A click puts a plate on the bed (the one it replaces keeps every edit); a
//! demo opens as it ships and its first edit makes the person's own plate. A plate's menu
//! (right-click or its ⋯) duplicates it, renames it, takes it off the list with its files kept,
//! or, after a confirm in a dialog at the window's centre (`FabTrashAsk`), moves its files to the
//! Trash. A marquee over the cards (or Cmd/Shift-click) marks several plates: Delete, or a marked
//! card's Move to Trash, asks to move them all. Under Demos, the demos without a
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
            // in the marquee's selection (`FabPlatesPanel::marked`)
            marked: instance(0.0)
            hover: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                sdf.fill_keep(fab.color_editor.mix(fab.color_row_hover, self.hover * 0.6).mix(fab.color_selection_bg, max(self.selected, self.marked)))
                sdf.stroke(vec4(fab.color_accent.xyz, max(self.selected, self.marked * 0.7)), 1.0)
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
                // its corners rounded as the well it fills
                draw_bg +: { border_radius: fab.radius }
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
    }

    // Whether to move a plate's files to the Trash: a dialog at the window's centre, over
    // everything (a layer of the shell), its plate named; Esc or a press outside keeps it.
    mod.widgets.FabTrashAskBase = #(FabTrashAsk::register_widget(vm))
    mod.widgets.FabTrashAsk = set_type_default() do mod.widgets.FabTrashAskBase{
        width: Fill
        height: Fill
        modal := Modal{
            a11y: {label: "Move to Trash"}
            content +: {
                width: 400
                height: Fit
                View{
                    width: Fill
                    height: Fit
                    flow: Down
                    padding: 16
                    spacing: 10
                    show_bg: true
                    draw_bg +: {
                        pixel: fn() {
                            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                            sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                            sdf.fill_keep(fab.color_popover)
                            sdf.stroke(fab.color_popover_border, 1.0)
                            return sdf.result
                        }
                    }
                    title := mod.widgets.FabTitleLabel{ width: Fill text: "" max_lines: 2 }
                    text := mod.widgets.FabLabel{
                        width: Fill
                        height: Fit
                        flow: Right{wrap: true}
                        text: ""
                    }
                    View{
                        width: Fill
                        height: Fit
                        flow: Right
                        spacing: 8
                        margin: Inset{top: 6}
                        align: Align{x: 0.0 y: 0.5}
                        Filler{}
                        keep := mod.widgets.FabButton{ text: "Cancel" }
                        trash := mod.widgets.FabButtonAccent{ text: "Move to Trash" }
                    }
                }
            }
        }
    }

    mod.widgets.FabPlatesPanelBase = #(FabPlatesPanel::register_widget(vm))
    mod.widgets.FabPlatesPanel = set_type_default() do mod.widgets.FabPlatesPanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        draw_shade: mod.draw.FabScrollShade{}
        // the marquee over the cards while it is dragged: a faint accent wash, its edge
        draw_marquee +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 2.0)
                sdf.fill_keep(vec4(fab.color_accent.xyz, 0.12))
                sdf.stroke(vec4(fab.color_accent.xyz, 0.8), 1.0)
                return sdf.result
            }
        }
        // the toolbar: the area's editor selector, then New plate and the search
        tools := mod.widgets.FabEditorBar{
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
    /// The shadow under the toolbar while the list is scrolled down.
    #[live]
    draw_shade: DrawQuad,
    #[live]
    draw_marquee: DrawQuad,
    /// The plates picked together (a marquee over the cards, or Cmd/Shift-click): Delete, or a
    /// marked card's Move to Trash, asks to move them all to the Trash.
    #[rust]
    marked: std::collections::HashSet<String>,
    /// A marquee being dragged over the cards: where it started, where the pointer is, the
    /// marks it adds to (Cmd/Shift held), and whether it has travelled yet.
    #[rust]
    marquee: Option<(DVec2, DVec2, std::collections::HashSet<String>, bool)>,
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
        let trash_label = if self.marked.contains(&card.id) && self.marked.len() > 1 {
            format!("Move {} Plates to Trash…", self.marked.len())
        } else {
            "Move to Trash…".to_string()
        };
        let items = vec![
            MenuItem::new(live_id!(plate_open), "Open"),
            MenuItem::new(live_id!(plate_duplicate), "Duplicate"),
            MenuItem::new(live_id!(plate_rename), "Rename"),
            MenuItem::sep(),
            MenuItem::new(live_id!(plate_remove), "Remove from List"),
            MenuItem::new(live_id!(plate_delete), &trash_label),
        ];
        open_menu(cx, PLATE_MENU, items, anchor, MenuPlace::At);
    }

    /// The marked plates in the list's order.
    fn marked_in_order(&self) -> Vec<String> {
        self.cards.iter().filter(|c| !c.demo && self.marked.contains(&c.id)).map(|c| c.id.clone()).collect()
    }

    /// The marquee's rectangle, its corners in either order.
    fn marquee_rect(from: DVec2, to: DVec2) -> Rect {
        let lo = dvec2(from.x.min(to.x), from.y.min(to.y));
        Rect { pos: lo, size: dvec2((from.x - to.x).abs(), (from.y - to.y).abs()) }
    }

    /// The marquee's raw pointer events: a primary press on the list's empty space starts it,
    /// a move marks the plates (not demos) whose cards it touches, the release ends it (one
    /// without travel clears the marks). Whether the event was its.
    fn marquee_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        match event {
            Event::MouseDown(e) if e.button.is_primary() => {
                let list = self.view.widget(cx, ids!(list)).area().clipped_rect(cx);
                if !list.contains(e.abs) {
                    return false;
                }
                // the keyboard comes here, for Delete
                cx.set_key_focus(self.view.area());
                let on_card = self.visible.iter().any(|&l| self.card(l).is_some() && self.shown.get(l).and_then(|s| s.rect(cx)).is_some_and(|r| r.contains(e.abs)));
                if on_card {
                    return false;
                }
                let keep = e.modifiers.shift || e.modifiers.is_primary();
                let base = if keep { self.marked.clone() } else { Default::default() };
                self.marquee = Some((e.abs, e.abs, base, false));
                true
            }
            Event::MouseMove(e) if self.marquee.is_some() => {
                let Some((from, to, base, travelled)) = &mut self.marquee else { return false };
                *to = e.abs;
                *travelled |= (*to - *from).length() >= crate::nav::DRAG_THRESHOLD_PT;
                if *travelled {
                    let rect = Self::marquee_rect(*from, *to);
                    let mut marked = base.clone();
                    for &l in &self.visible {
                        let Some(card) = self.card(l) else { continue };
                        if card.demo { continue; }
                        if self.shown.get(l).and_then(|s| s.rect(cx)).is_some_and(|r| r.intersects(rect)) {
                            marked.insert(card.id.clone());
                        }
                    }
                    self.marked = marked;
                    self.view.redraw(cx);
                }
                true
            }
            Event::MouseUp(_) if self.marquee.is_some() => {
                if let Some((_, _, base, travelled)) = self.marquee.take() {
                    if !travelled {
                        self.marked = base;
                    }
                }
                self.view.redraw(cx);
                true
            }
            Event::KeyDown(ke) if !self.marked.is_empty() && cx.has_key_focus(self.view.area()) => match ke.key_code {
                KeyCode::Delete | KeyCode::Backspace => {
                    cx.action(ShellAction::AskTrashPlates(self.marked_in_order()));
                    true
                }
                KeyCode::Escape => {
                    self.marked.clear();
                    self.view.redraw(cx);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn picked(&mut self, cx: &mut Cx, id: LiveId) {
        let Some(plate) = self.menu_for.take() else { return };
        if id == live_id!(plate_open) {
            cx.action(ShellAction::OpenPlate(plate));
        } else if id == live_id!(plate_duplicate) {
            cx.action(ShellAction::DuplicatePlate(plate));
        } else if id == live_id!(plate_rename) {
            self.rename = Some((plate, true));
        } else if id == live_id!(plate_remove) {
            cx.action(ShellAction::RemovePlate(plate));
        } else if id == live_id!(plate_delete) {
            self.rename = None;
            // a marked card's Trash takes every marked plate with it
            let plates = if self.marked.contains(&plate) && self.marked.len() > 1 {
                self.marked_in_order()
            } else {
                vec![plate]
            };
            cx.action(ShellAction::AskTrashPlates(plates));
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
        self.marked.retain(|id| live(id));
        if self.rename.as_ref().is_some_and(|(id, _)| !live(id)) {
            self.rename = None;
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
        if self.marquee_event(cx, event) {
            return;
        }
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
            if item.button(cx, ids!(pic.corner.more)).clicked(actions) {
                let r = item.widget(cx, ids!(pic.corner.more)).area().rect(cx);
                self.open_menu_for(cx, &card, Rect { pos: r.pos + dvec2(0.0, r.size.y), size: dvec2(0.0, 0.0) });
                continue;
            }
            if let Some(up) = item.as_view().finger_up(actions) {
                if !up.cancelled && up.is_over && up.device.is_primary_hit() && !card.demo && (up.modifiers.shift || up.modifiers.is_primary()) {
                    // Cmd/Shift-click: the card in or out of the marked plates
                    if !self.marked.remove(&card.id) {
                        self.marked.insert(card.id.clone());
                    }
                    self.view.redraw(cx);
                } else if !up.cancelled && up.is_over && up.device.is_primary_hit() {
                    self.marked.clear();
                    if up.tap_count == 2 && !card.demo {
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
                                view.draw_bg.set_dyn_instance(cx, live_id!(marked), &[if self.marked.contains(&card.id) && !card.demo { 1.0 } else { 0.0 }]);
                            }
                            item.label(cx, ids!(name)).set_text(cx, &card.title());
                            item.label(cx, ids!(info)).set_text(cx, &info(card));
                            let renaming = self.rename.as_ref().filter(|(id, _)| *id == card.id && !card.demo).map(|(_, focus)| *focus);
                            item.widget(cx, ids!(name)).set_visible(cx, renaming.is_none());
                            item.widget(cx, ids!(rename)).set_visible(cx, renaming.is_some());
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
                            if let Some(shown) = self.shown.get_mut(line) {
                                shown.row = item.area();
                                shown.line = item.widget(cx, ids!(info)).area();
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
        // the list scrolled down: a shadow along its top, under the toolbar
        let list = self.view.portal_list(cx, ids!(list));
        let (first_id, first_scroll) = (list.first_id(), list.scroll_position());
        crate::ui::widgets::draw_scroll_shade(cx, &mut self.draw_shade, list.area().rect(cx), first_id, first_scroll);
        if let Some((from, to, _, true)) = &self.marquee {
            let clip = list.area().clipped_rect(cx);
            let r = Self::marquee_rect(*from, *to);
            let lo = dvec2(r.pos.x.max(clip.pos.x), r.pos.y.max(clip.pos.y));
            let hi = dvec2((r.pos.x + r.size.x).min(clip.pos.x + clip.size.x), (r.pos.y + r.size.y).min(clip.pos.y + clip.size.y));
            if hi.x > lo.x && hi.y > lo.y {
                self.draw_marquee.draw_abs(cx, Rect { pos: lo, size: hi - lo });
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

/// The Trash question for `UiState::trash_ask`'s plates: Move to Trash deletes them
/// (`ShellAction::DeletePlate` each), Enter too; Cancel, Esc or a press outside closes it.
#[derive(Script, ScriptHook, Widget)]
pub struct FabTrashAsk {
    #[deref]
    view: View,
    /// The plates the dialog is open for.
    #[rust]
    shown: Vec<String>,
}

impl FabTrashAsk {
    fn trash(&self, cx: &mut Cx) {
        for id in &self.shown {
            cx.action(ShellAction::DeletePlate(id.clone()));
        }
        cx.action(ShellAction::AskTrashPlates(Vec::new()));
    }
}

impl Widget for FabTrashAsk {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let modal = self.view.modal(cx, ids!(modal));
            if self.view.button(cx, ids!(modal.trash)).clicked(actions) {
                self.trash(cx);
            } else if modal.dismissed(actions) || self.view.button(cx, ids!(modal.keep)).clicked(actions) {
                cx.action(ShellAction::AskTrashPlates(Vec::new()));
            }
        }
        if let Event::KeyDown(ke) = event {
            if !self.shown.is_empty() && ke.key_code == KeyCode::ReturnKey {
                self.trash(cx);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            // (a plate gone from the list meanwhile is not asked about)
            let ask: Vec<String> = state.ui.trash_ask.iter().filter(|id| state.plates.plates.iter().any(|p| p.id == **id)).cloned().collect();
            if ask != self.shown {
                let modal = self.view.modal(cx, ids!(modal));
                if ask.is_empty() {
                    modal.close(cx);
                } else {
                    let title = match ask.as_slice() {
                        [one] => {
                            let name = state.plates.plates.iter().find(|p| p.id == *one).map(|p| p.name.clone()).unwrap_or_default();
                            format!("Move \u{201c}{name}\u{201d} to the Trash?")
                        }
                        many => format!("Move {} plates to the Trash?", many.len()),
                    };
                    self.view.label(cx, ids!(modal.title)).set_text(cx, &title);
                    self.view.label(cx, ids!(modal.text)).set_text(cx, if ask.len() == 1 {
                        "Its files go to the Trash and it leaves the list. You can put them back from the Trash."
                    } else {
                        "Their files go to the Trash and they leave the list. You can put them back from the Trash."
                    });
                    if self.shown.is_empty() {
                        modal.open(cx);
                    }
                }
                self.shown = ask;
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

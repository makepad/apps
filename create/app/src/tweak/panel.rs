//! Property inspector for one App item. The share view owns its picture.
use super::props::layout::view::{set_face, LayoutControls};
use super::design::{d1, DesignTarget};
use super::design::view::{self as design_view, BuildTab, PaletteList};
use super::{DesignRequest, DesignReplyKind, DesignNotice};
use super::shader::{ShaderTab, view::ShaderPills};
use super::props::live::LiveSets;
use super::focus::{zoom_label, FocusState, MAX_ZOOM, MIN_ZOOM, ZOOM_STEP};
use super::overlay::{seat_colour, who};
use super::props::{AppliedEdit, Props, view::{HeaderId, PropLine, Sidebar, SidebarAction}};
use super::tree::{drop_zone, DropIndicator, TreeAction, TreeLine, TreeState};
use super::theme::{BuildAction, InstallSource, MixAction, PendingInstall, ThemeAction, ThemeMixerCache, ThemeSnapshot, ThemeTab};
use super::{ThemeOp, Notice, Request, RequestId, Seat, Session, SessionId, TreeQuery};
use crate::model::Model;
use makepad_town_core::{entity::{field, kind, Entity}, event::{Content, Id, Value}, fold::State};
use makepad_widgets::*;
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle};
use std::{collections::{BTreeMap, BTreeSet}, rc::Rc};

fn props_list(vm: &mut ScriptVm) -> ScriptValue {
    let row = TweakRow::script_proto(vm);
    super::props::view::sidebar_value(vm, row)
}

fn tree_list(vm: &mut ScriptVm) -> ScriptValue {
    let row = TweakRow::script_proto(vm);
    super::tree::view::tree_list_value(vm, row)
}

fn theme_wrap(vm: &mut ScriptVm) -> ScriptValue {
    let rows = super::theme::view::EqRows::register_widget(vm);
    super::theme::view::wrap_value(vm, rows)
}

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    let TweakList = #(TweakList::register_widget(vm))
    let TweakRow = #(TweakRow::register_widget(vm))
    let PropsList = #(props_list(vm))
    let TreeList = #(tree_list(vm))
    let BuildWrap = #(design_view::wrap_value(vm))
    let DesignStrip = #(design_view::strip_value(vm))
    let ThemeWrap = #(theme_wrap(vm))
    let TweakTabs = #(TweakTabs::register_widget(vm))
    let ShaderPills = #(ShaderPills::register_widget(vm))
    let PanelTabButton = PanelButton {
        min_height: 0
        width: Fit
        height: 20
        margin: Inset{left: 0 right: 0 top: 0 bottom: 0}
        padding: Inset{left: 5 right: 5 top: 2 bottom: 2}
        spacing: 0
        draw_text +: { text_style +: { font_size: 8.0 } }
        draw_bg +: {
            border_radius: 4.0
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box_y(
                    0.5
                    0.5
                    self.rect_size.x - 1.0
                    self.rect_size.y + 6.0
                    self.border_radius
                    0.5
                )
                let fill = self.color
                    .mix(self.color_hover, self.hover)
                    .mix(self.color_down, self.down)
                let stroke = self.border_color.mix(self.border_color_hover, self.hover)
                sdf.fill_keep(fill)
                sdf.stroke(stroke, self.border_size)
                return sdf.result
            }
        }
    }
    mod.widgets.AppTweakPanel = View{
        width: Fill height: Fill flow: Down spacing: 4
        padding: Inset{left: 4 right: 4 top: 6 bottom: 4}
        show_bg: true draw_bg +: {color: #x303030}
        tweak_tabs := TweakTabs{width: Fill height: Fit flow: Right{wrap: true} spacing: 3 align: Align{y: 0.5}
            tab_props := PanelTabButton{text: "Props"}
            tab_props_i := PanelTabButton{visible: false text: "" a11y: {label: "Props"} icon_walk: Walk{width: 13 height: Fit}
                draw_icon +: {color: #xd0d0d0 svg: crate_resource("makepad_widgets:resources/icons/tab_list.svg")}}
            tab_shader := PanelTabButton{text: "Shader"}
            tab_shader_i := PanelTabButton{visible: false text: "" a11y: {label: "Shader"} icon_walk: Walk{width: 13 height: Fit}
                draw_icon +: {color: #xd0d0d0 svg: crate_resource("makepad_widgets:resources/icons/tab_brush.svg")}}
            tab_tree := PanelTabButton{text: "Tree"}
            tab_tree_i := PanelTabButton{visible: false text: "" a11y: {label: "Tree"} icon_walk: Walk{width: 13 height: Fit}
                draw_icon +: {color: #xd0d0d0 svg: crate_resource("makepad_widgets:resources/icons/tab_tree.svg")}}
            tab_theme := PanelTabButton{text: "Theme"}
            tab_theme_i := PanelTabButton{visible: false text: "" a11y: {label: "Theme"} icon_walk: Walk{width: 13 height: Fit}
                draw_icon +: {color: #xd0d0d0 svg: crate_resource("makepad_widgets:resources/icons/tab_palette.svg")}}
            tab_build := PanelTabButton{text: "Build"}
            tab_build_i := PanelTabButton{visible: false text: "" a11y: {label: "Build"} icon_walk: Walk{width: 13 height: Fit}
                draw_icon +: {color: #xd0d0d0 svg: crate_resource("makepad_widgets:resources/icons/tab_hammer.svg")}}
            tweak_close := PanelButton{text: "Close" a11y: {label: "Close tweak panel"}}
        }
        tweak_ask := PanelButton{text: "Ask for control"}
        tweak_seats := TweakList{
            width: Fill height: 40
            tweak_seats_list := PortalList{width: Fill height: Fill
                Row := TweakRow{width: Fill height: Fit padding: 2
                    RoundedView{width: Fill height: Fit padding: 4 spacing: 5 align: Align{y: 0.5}
                        draw_bg +: {color: palette.panel2 border_radius: 5}
                        tweak_seat_name := RailText{width: Fill flow: Right{wrap: true}}
                        tweak_status := Kicker{width: Fit}
                        tweak_allow := PanelButton{text: "Resume"}
                        tweak_revoke := PanelButton{text: "Revoke"}
                        tweak_stop := PanelButton{text: "Stop"}
                    }
                }
            }
        }
        tweak_hint := BodyNote{width: Fill text: "Click a widget in the shared picture to see its properties."}
        tweak_props_wrap := View{width: Fill height: Fill{weight: 75.0}
            tweak_properties := TweakList{width: Fill height: Fill
                tweak_properties_list := PropsList{}
            }
        }
        tweak_shader_wrap := View{visible: false width: Fill height: Fill{weight: 75.0} flow: Down spacing: 3
            shader_pill_bar := ShaderPills{width: Fill height: Fit
                shader_layers := FlatList{width: Fill height: Fit flow: Right{wrap: true} spacing: 3
                    scroll_bars +: {show_scroll_y: false}
                    Pill := PanelButton{width: Fit height: 20 min_height: 0
                        padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                        draw_text.text_style.font_size: 8
                    }
                }
            }
            shader_title := RailText{width: Fill}
            shader_doc := BodyNote{width: Fill flow: Right{wrap: true}}
            tweak_shader_rows := TweakList{width: Fill height: Fill
                tweak_shader_list := PropsList{}
            }
            View{width: Fill height: Fit flow: Right{wrap: true} align: Align{y: 0.5} spacing: 4
                src_fold := PanelButton{width: Fit height: 20 min_height: 0
                    padding: Inset{left: 8 right: 8 top: 2 bottom: 2}
                    draw_text.text_style.font_size: 8 text: "- source"}
                shader_propose := PanelButton{width: Fit height: 20 min_height: 0
                    padding: Inset{left: 8 right: 8 top: 2 bottom: 2}
                    draw_text.text_style.font_size: 8 text: "Propose change"}
            }
            shader_src := PanelInput{
                width: Fill height: Fit{max: FitBound.Rel{base: Base.Full factor: 0.35}}
                is_multiline: true submit_on_enter: false keep_focus_on_submit: true
                draw_bg +: {color: #x1b1b1b color_hover: #x1b1b1b color_focus: #x1b1b1b color_down: #x1b1b1b
                    color_2_hover: #x1b1b1b color_2_focus: #x1b1b1b color_2_down: #x1b1b1b}
                draw_text +: {color: #xd0d0d0 text_style: theme.font_code text_style +: {font_size: 7.5}}
            }
            shader_src_status := BodyNote{width: Fill flow: Right{wrap: true}}
        }
        tweak_tree_wrap := View{visible: false width: Fill height: Fill{weight: 75.0} flow: Down spacing: 3
            tweak_tree_head := View{width: Fill height: Fit flow: Down spacing: 3
                View{width: Fill height: Fit flow: Right spacing: 3 align: Align{y: 0.5}
                    tree_filter := FabSearch{input +: {empty_text: "Filter tree"}}
                    tree_refresh := PanelTabButton{text: "↻" a11y: {label: "Refresh tree"}}
                }
                View{width: Fill height: Fit flow: Right{wrap: true} spacing: 3 align: Align{y: 0.5}
                    tree_isolate := PanelTabButton{text: "Isolate"}
                    tree_center := PanelTabButton{text: "Center"}
                    tree_zoom_out := PanelTabButton{text: "−" a11y: {label: "Zoom out"}}
                    tree_zoom := BodyNote{width: Fit text: "1.0×"}
                    tree_zoom_in := PanelTabButton{text: "+" a11y: {label: "Zoom in"}}
                }
                tree_strip := DesignStrip{}
                isolate_hint := BodyNote{visible: false width: Fill flow: Right{wrap: true}}
                tree_status := BodyNote{visible: false width: Fill flow: Right{wrap: true}}
            }
            tweak_tree := TweakList{width: Fill height: Fill drag_chip: DragChip{}
                tweak_tree_list := TreeList{}
            }
        }
        tweak_build_wrap := BuildWrap{}
        tweak_theme_wrap := ThemeWrap{
            tweak_theme := TweakList{width: Fill height: Fill
                tweak_theme_list := PropsList{}
            }
        }
        tweak_result := BodyNote{}
        history_title := Kicker{text: "EDIT HISTORY"}
        tweak_history := TweakList{
            width: Fill height: Fill{weight: 25.0}
            tweak_history_list := PortalList{width: Fill height: Fill
                Row := TweakRow{width: Fill height: Fit padding: Inset{top: 2 bottom: 2}
                    tweak_entry := RailText{width: Fill flow: Right{wrap: true}}
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialOrd, Ord, PartialEq, Eq)]
enum HistoryKey { Applied(u64), Note(u64, Id) }

#[derive(PartialEq)]
enum HistoryEntry {
    Applied { seq: u64, seat: Seat, who: String, edit: AppliedEdit },
    Note { key: HistoryKey, text: String },
}

impl HistoryEntry {
    fn key(&self) -> HistoryKey {
        match self {
            Self::Applied { seq, .. } => HistoryKey::Applied(*seq),
            Self::Note { key, .. } => *key,
        }
    }

    fn continues(&self, other: &Self) -> bool {
        matches!((self, other), (Self::Applied { seat, edit, .. }, Self::Applied { seat: s, edit: e, .. })
            if seat == s && edit.path == e.path && edit.prop == e.prop)
    }

    fn edit(&self) -> Option<&AppliedEdit> {
        match self { Self::Applied { edit, .. } => Some(edit), Self::Note { .. } => None }
    }
}

#[derive(PartialEq)]
struct HistoryRun {
    first: Rc<HistoryEntry>,
    last: Rc<HistoryEntry>,
    steps: usize,
}

impl HistoryRun {
    fn line(entries: &[Rc<HistoryEntry>]) -> Rc<Line> {
        Rc::new(Line::Edit(Self {
            first: entries[0].clone(), last: entries[entries.len() - 1].clone(), steps: entries.len(),
        }))
    }

    fn text(&self) -> String {
        match self.first.as_ref() {
            HistoryEntry::Note { text, .. } => text.clone(),
            HistoryEntry::Applied { seq, who, edit, .. } => {
                let new = &self.last.edit().unwrap().new;
                let mut text = format!("{seq}  {who}  {}.{}  {} → {new}", edit.path, edit.prop, edit.old);
                if self.steps > 1 { text.push_str(&format!(" ({} steps)", self.steps)); }
                text
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ChipAction { Revoke, Stop(Id), Resume(Id) }

#[derive(PartialEq)]
enum Line {
    Property(PropLine),
    Tree(TreeLine),
    Chip { share: SessionId, seat: Seat, item: Id, name: String, ink: Vec4f, status: &'static str, action: Option<ChipAction> },
    Edit(HistoryRun),
}

impl Line {
    fn bind(&self, cx: &mut Cx, row: &View, previous: Option<&Line>, controls: &mut LayoutControls, draggable: bool) {
        match self {
            Self::Property(p) => p.draw_sidebar(cx, row, match previous { Some(Self::Property(old)) => Some(old), _ => None }, controls),
            Self::Tree(line) => line.draw_tree(cx, row, draggable),
            Self::Chip { share, seat, name, ink, status, action, .. } => {
                let label = row.label(cx, ids!(tweak_seat_name));
                label.set_text(cx, name);
                label.set_text_color(cx, *ink);
                row.label(cx, ids!(tweak_status)).set_text(cx, status);
                row.widget(cx, ids!(tweak_status)).set_visible(cx, !status.is_empty());
                let revoke = row.button(cx, ids!(tweak_revoke));
                revoke.set_visible(cx, *action == Some(ChipAction::Revoke));
                if *action == Some(ChipAction::Revoke) {
                    revoke.set_action_data(crate::call::CallAction::OnShare(*share, crate::call::Press::RevokeControl(*seat)));
                } else {
                    revoke.set_action_data(());
                }
                row.widget(cx, ids!(tweak_allow)).set_visible(cx, matches!(action, Some(ChipAction::Resume(_))));
                row.widget(cx, ids!(tweak_stop)).set_visible(cx, matches!(action, Some(ChipAction::Stop(_))));
            }
            Self::Edit(run) => row.label(cx, ids!(tweak_entry)).set_text(cx, &run.text()),
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
struct TweakRow {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] shown: Option<Rc<Line>>,
    #[rust] controls: LayoutControls,
    #[rust] draggable: bool,
}

impl Widget for TweakRow {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(self.shown.as_deref(), Some(Line::Chip { .. })) {
            crate::ui::live::button_events(&mut self.view, cx, event, scope);
        } else {
            self.view.handle_event(cx, event, scope);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep { self.view.draw_walk(cx, scope, walk) }
}

/// One seat chip's height in the seats list.
const SEAT_ROW: f64 = 40.0;

#[derive(Script, ScriptHook, Widget)]
pub struct TweakList {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[live] drag_chip: ScriptObjectRef,
    #[rust] can_edit_tree: bool,
    #[rust] drag_enabled: bool,
    #[rust] press: Option<(DVec2, Rc<str>, String)>,
    #[rust] dragged: bool,
    #[rust] indicator: Option<(usize, Rc<str>, DropIndicator)>,
    #[rust] rows: Vec<Rc<Line>>,
    #[rust] history: Vec<Rc<HistoryEntry>>,
}

impl TweakList {
    pub(super) fn configure_tree_drag(&mut self, cx: &mut Cx, can_edit: bool, tree: &TreeState) {
        self.can_edit_tree = can_edit;
        let enabled = can_edit && tree.filter.is_empty();
        if self.drag_enabled == enabled { return }
        self.drag_enabled = enabled;
        if !enabled {
            self.press = None;
            self.indicate(cx, None);
        }
        self.view.redraw(cx);
    }

    fn indicate(&mut self, cx: &mut Cx, next: Option<(usize, Rc<str>, DropIndicator)>) {
        if self.indicator == next { return }
        let old = std::mem::replace(&mut self.indicator, next);
        for (entry, value) in [(old.as_ref(), None), (self.indicator.as_ref(), self.indicator.as_ref().map(|t| t.2))] {
            let Some((i, target, _)) = entry else { continue };
            let Some(row) = self.rows.get_mut(*i) else { continue };
            let Line::Tree(line @ TreeLine::Node { path, .. }) = row.as_ref() else { continue };
            if path != target { continue }
            let mut line = line.clone();
            if let TreeLine::Node { drop, .. } = &mut line { *drop = value; }
            *row = Rc::new(Line::Tree(line));
        }
        self.view.redraw(cx);
    }

    fn tree_pointer(&mut self, cx: &mut Cx, event: &Event) {
        match event {
            Event::MouseDown(e) if e.button.is_primary() => {
                let portal = self.view.portal_list(cx, ids!(tweak_tree_list));
                self.press = portal.item_at(cx, e.abs).and_then(|i| {
                    let Line::Tree(TreeLine::Node { path, label, .. }) = self.rows.get(i)?.as_ref() else { return None };
                    let (_, row) = portal.get_item(i)?;
                    let twisty = row.button(cx, ids!(twisty));
                    if twisty.visible() && twisty.area().clipped_rect(cx).contains(e.abs) { return None }
                    Some((e.abs, path.clone(), label.clone()))
                });
                self.dragged = false;
            }
            Event::MouseMove(e) if self.press.as_ref().is_some_and(|(at, ..)| (e.abs - *at).length() > 4.) => {
                let (_, path, label) = self.press.take().unwrap();
                if cx.has_internal_drag() { return }
                let chip = cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.drag_chip.as_object().into()));
                chip.label(cx, ids!(label)).set_text(cx, &label);
                self.dragged = true;
                let item = DragItem::String { value: path.to_string(), internal_id: Some(live_id!(town_design_node)) };
                cx.start_dragging_with(vec![item], chip, dvec2(14., 10.));
            }
            Event::MouseUp(_) => self.press = None,
            Event::DragEnd | Event::WindowLostFocus(_) => {
                self.press = None;
                if matches!(event, Event::WindowLostFocus(_)) { self.dragged = false; }
                self.indicate(cx, None);
            }
            Event::Drag(_) | Event::Drop(_) => {
                let items = match event { Event::Drag(e) => &e.items, Event::Drop(e) => &e.items, _ => unreachable!() };
                let Some(source) = items.iter().find_map(|item| match item {
                    DragItem::String { value, internal_id: Some(id) } if *id == live_id!(town_design_node) => Some(value),
                    _ => None,
                }) else { return };
                let portal = self.view.portal_list(cx, ids!(tweak_tree_list));
                let (abs, response) = match event.drag_hits(cx, portal.area()) {
                    DragHit::Drag(d) if d.state != DragState::Out => (d.abs, Some(d.response)),
                    DragHit::Drop(d) => (d.abs, None),
                    _ => { self.indicate(cx, None); return }
                };
                let target = portal.item_at(cx, abs).and_then(|i| {
                    let Line::Tree(TreeLine::Node { path, ty, .. }) = self.rows.get(i)?.as_ref() else { return None };
                    let rect = portal.get_item(i)?.1.area().rect(cx);
                    Some((i, path.clone(), drop_zone(source, path, ty, abs.y - rect.pos.y, rect.size.y)?))
                });
                if let Some(response) = response {
                    if target.is_some() {
                        *response.lock().unwrap_or_else(|e| e.into_inner()) = DragResponse::Move;
                        cx.set_cursor(MouseCursor::Hand);
                    } else { cx.set_cursor(MouseCursor::NotAllowed); }
                    self.indicate(cx, target);
                } else {
                    if let Some((_, target, zone)) = target {
                        cx.widget_action(self.widget_uid(), TreeAction::Move { source: source.clone(), target: target.to_string(), place: zone.place() });
                    }
                    self.indicate(cx, None);
                }
            }
            _ => {}
        }
    }
}

impl Widget for TweakList {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.drag_enabled { self.tree_pointer(cx, event); }
        else if matches!(event, Event::MouseDown(_) | Event::WindowLostFocus(_)) { self.dragged = false; }
        if self.dragged && matches!(event, Event::MouseUp(_)) {
            let mut actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
            actions.retain(|a| !a.as_widget_action().is_some_and(|w| matches!(w.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_)))));
            cx.extend_actions(actions);
            self.dragged = false;
        } else { self.view.handle_event(cx, event, scope); }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(cx, 0, self.rows.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some(line) = self.rows.get(i) else { continue };
                let template = match line.as_ref() { Line::Property(p) => p.template(), Line::Tree(t) => t.template(), _ => id!(Row) };
                let (item, existed) = list.item_with_existed(cx, i, template);
                if let Some(mut row) = item.borrow_mut::<TweakRow>() {
                    if !existed || row.draggable != self.drag_enabled || !row.shown.as_ref().is_some_and(|old| Rc::ptr_eq(old, line)) {
                        let mut controls = LayoutControls::default();
                        line.bind(cx, &row.view, row.shown.as_deref(), &mut controls, self.drag_enabled);
                        row.draggable = self.drag_enabled;
                        row.controls = controls;
                        row.shown = Some(line.clone());
                    }
                }
                item.draw_all(cx, scope);
            }
        }
        DrawStep::done()
    }
}

fn replace(cx: &mut Cx, list: WidgetRef, rows: Vec<Rc<Line>>, reset_scroll: bool) {
    if let Some(mut data) = list.borrow_mut::<TweakList>() {
        if reset_scroll && rows.is_empty() { data.history.clear(); }
        if data.rows == rows { return }
        data.rows = rows;
        if reset_scroll {
            for (_, child) in &data.view.children {
                if let Some(mut portal) = child.borrow_mut::<PortalList>() { portal.set_first_id_and_scroll(0, 0.); }
            }
        }
        data.view.redraw(cx);
    }
}

pub(super) fn set_tab_fill(cx: &mut Cx, btn: WidgetRef, selected: bool) {
    let (base, hover, down, border, ink): (Vec4f, Vec4f, Vec4f, Vec4f, Vec4f) = if selected {
        (
            vec4(0.20, 0.20, 0.21, 1.0),
            vec4(0.22, 0.22, 0.23, 1.0),
            vec4(0.18, 0.18, 0.19, 1.0),
            vec4(0.36, 0.36, 0.38, 1.0),
            vec4(0.93, 0.93, 0.93, 1.0),
        )
    } else {
        (
            vec4(0.12, 0.12, 0.13, 1.0),
            vec4(0.15, 0.15, 0.16, 1.0),
            vec4(0.10, 0.10, 0.11, 1.0),
            vec4(0.09, 0.09, 0.10, 1.0),
            vec4(0.64, 0.64, 0.66, 1.0),
        )
    };
    if let Some(mut btn) = btn.borrow_mut::<Button>() {
        set_face(cx, &mut btn, &[
            (live_id!(color), base),
            (live_id!(color_hover), hover),
            (live_id!(color_down), down),
            (live_id!(color_focus), base),
            (live_id!(border_color), border),
            (live_id!(border_color_hover), border),
            (live_id!(border_color_down), border),
            (live_id!(border_color_focus), border),
        ]);
        btn.draw_text.color = ink;
        btn.draw_icon.color = ink;
        btn.redraw(cx);
    }
}

/// The tweaker uses icon tabs when the words no longer fit beside the other controls.
#[derive(Script, ScriptHook, Widget)]
struct TweakTabs {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] room: Option<f64>,
    #[rust] board: bool,
}

impl Widget for TweakTabs {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let room = cx.turtle().max_width(walk);
        if let Some(width) = room.filter(|_| self.room != room) {
            let visible = width_override::WidthOverride { visible: Some(true), ..Default::default() };
            let tabs = [(ids!(tab_props), ids!(tab_props_i)), (ids!(tab_shader), ids!(tab_shader_i)),
                (ids!(tab_tree), ids!(tab_tree_i)), (ids!(tab_theme), ids!(tab_theme_i)), (ids!(tab_build), ids!(tab_build_i))];
            let start = if self.board { 3 } else { 0 };
            let words = tabs[start..].iter().map(|&(word, _)| word).chain([ids!(tweak_close)]);
            let widths: Option<f64> = words.map(|id| self.view.widget(cx, id).measure_width(cx, Some(&visible))).sum();
            if let Some(widths) = widths {
                let narrow = widths + (tabs.len() - start) as f64 * self.view.layout.spacing > width;
                for (index, (word, icon)) in tabs.into_iter().enumerate() {
                    self.view.widget(cx, word).set_visible(cx, index >= start && !narrow);
                    self.view.widget(cx, icon).set_visible(cx, index >= start && narrow);
                }
                self.room = room;
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PanelTab { #[default] Props, Shader, Tree, Theme, Build }

#[cfg(test)]
std::thread_local! {
    pub(super) static BOARD_INDEX_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn board_widget_paths(src: &str) -> Result<Vec<String>, d1::D1Error> {
    #[cfg(test)]
    BOARD_INDEX_RUNS.with(|count| count.set(count.get() + 1));
    Ok(d1::list_widgets(src)?.into_iter().map(|node| node.path).collect())
}

struct BoardDesign {
    item: Id,
    // Only a successfully indexed revision authorizes board_actions.
    revision: Option<Id>,
    failed_revision: Option<Id>,
    pending: Option<(Id, TaskHandle<Result<Vec<String>, String>>)>,
    paths: Vec<String>,
    theme: Option<super::ThemeInstall>,
}

impl BoardDesign {
    fn refresh_index(&mut self, cx: &mut Cx, model: &Model, first: bool) -> Option<Result<Vec<String>, String>> {
        let writer = model.state.entities.get(&self.item).and_then(|e| e.fields.get(&field::SRC)).map(|r| r.writer);
        if let Some((revision, handle)) = &mut self.pending {
            let result = handle.try_take()?;
            let current = Some(*revision) == writer;
            self.pending = None;
            if current {
                let result = result.map_err(|err| err.to_string()).and_then(|paths| paths);
                if result.is_ok() { self.revision = writer } else { self.failed_revision = writer }
                return Some(result)
            }
            // Multiple writes during the parse need just one snapshot of the latest source.
        } else if !first && writer == self.revision.or(self.failed_revision) { return None }
        self.revision = None;
        self.failed_revision = None;
        let snapshot = match d1::read_design(&model.state, self.item) {
            Ok(snapshot) => snapshot,
            Err(err) => return Some(Err(err.to_string())),
        };
        match cx.task_pool().submit_named(Class::Visible, "design widget index", move || {
            board_widget_paths(&snapshot.src).map_err(|err| err.to_string())
        }) {
            Ok(handle) => {
                self.pending = Some((snapshot.revision, handle));
                None
            }
            Err(err) => Some(Err(err.to_string())),
        }
    }
}

#[derive(Default)]
pub struct Panel {
    pub(super) asking: Option<SessionId>,
    board: Option<BoardDesign>,
    bound: Option<(SessionId, Id)>,
    worn_by_share: BTreeMap<(SessionId, Id), super::theme::store::SavedTheme>,
    last_selection: Option<RequestId>,
    props: Option<(RequestId, String)>,
    sidebar: Sidebar,
    theme: ThemeTab,
    pub(super) mixer_cache: ThemeMixerCache,
    tab: PanelTab,
    build: BuildTab,
    shader: ShaderTab,
    tree: TreeState,
    // Retain an isolated share's pages and outstanding reads while another share owns the panel.
    other_trees: BTreeMap<(SessionId, Id), TreeState>,
    proposals: BTreeMap<RequestId, (Rc<str>, String)>,
    edits: BTreeMap<RequestId, (Rc<str>, String)>,
    live: LiveSets,
    pending_resize_bake: Option<Rc<str>>,
    // Notices can beat signed-log replication. Keep those rows until their creates arrive.
    pending: BTreeMap<u64, Rc<HistoryEntry>>,
}

/// Keep optimistic composite fields ahead of the last Rows snapshot only
/// for the selected path. A queued Set can be released after selection moves.
fn send_set<L: Copy + Eq, T: Copy + Eq>(session: &mut Session<L, T>, sidebar: &mut Sidebar,
    theme: &mut ThemeTab, edits: &mut BTreeMap<RequestId, (Rc<str>, String)>, path: &Rc<str>, prop: &str, value: &str) -> Option<RequestId> {
    let token = path.as_ref() == "theme";
    let selected = !token && *path == sidebar.path;
    let request = if token { Request::Theme(ThemeOp::Set { name: prop.into(), value: value.into() }) }
        else { Request::Set { path: path.to_string(), prop: prop.into(), value: value.into() } };
    match session.request(None, request) {
        Ok(req) => {
            if token { theme.refusals.remove(prop); }
            if selected {
                sidebar.refusals.remove(prop);
                if super::props::layout_composited(prop) {
                    sidebar.layout_edits.insert(req, (prop.into(), value.into()));
                }
                let (kind, components) = super::props::parse_struct(value);
                if !components.is_empty() {
                    sidebar.components.insert(req, (prop.into(), kind, components));
                }
            }
            edits.insert(req, (path.clone(), prop.into()));
            Some(req)
        }
        Err(why) => {
            if token { theme.refusals.insert(prop.into(), why); }
            else if selected { sidebar.refusals.insert(prop.into(), why); }
            None
        }
    }
}

impl Panel {
    pub(super) fn is_build_mode(&self, share: SessionId, item: Id) -> bool {
        self.bound == Some((share, item)) && self.tab == PanelTab::Build
    }

    pub fn set_themes_dir(&mut self, dir: std::path::PathBuf) { self.theme.themes_dir = dir; }

    #[cfg(test)]
    pub(super) fn theme_state(&self) -> &ThemeTab { &self.theme }

    #[cfg(test)]
    pub(super) fn sidebar_rect(&self) -> Option<super::Rect> { self.sidebar.rect }

    pub(super) fn themes_dir(&self) -> std::path::PathBuf { self.theme.themes_dir() }

    pub(super) fn worn_for(&self, share: SessionId, item: Id) -> Option<(SessionId, Id, String)> {
        let worn = if self.bound == Some((share, item)) {
            self.theme.worn.as_ref()
        } else {
            self.worn_by_share.get(&(share, item))
        };
        Some((share, item, worn?.name.clone()))
    }

    pub(super) fn clear_worn(&mut self, cx: &mut Cx, root: &WidgetRef, share: SessionId, item: Id) {
        let key = (share, item);
        self.worn_by_share.remove(&key);
        if self.bound == Some(key) && self.theme.worn.take().is_some() {
            if self.theme.confirm.take().is_some() { self.theme.msg.clear(); }
            if self.tab == PanelTab::Theme { self.theme.sync_header(cx, root); }
        }
    }

    pub(super) fn retire_share(&mut self, share: SessionId, item: Id) {
        let key = (share, item);
        self.worn_by_share.remove(&key);
        self.other_trees.remove(&key);
        if self.bound == Some(key) {
            self.theme.worn = None;
            self.tree = TreeState::default();
        }
    }

    pub(super) fn apply_theme_tool_effect(&mut self, cx: &mut Cx, root: &WidgetRef, effect: crate::tools::ThemeToolEffect) {
        use crate::tools::ThemeToolEffect::*;
        match effect {
            Saved { share, item, saved } => {
                let key = (share, item);
                if self.bound == Some(key) {
                    self.theme.sync_ai_saved(saved, true);
                    match &self.theme.worn {
                        Some(worn) => { self.worn_by_share.insert(key, worn.clone()); }
                        None => { self.worn_by_share.remove(&key); }
                    }
                } else {
                    self.worn_by_share.insert(key, saved.clone());
                    self.theme.sync_ai_saved(saved, false);
                }
            }
            Deleted(name) => {
                self.worn_by_share.retain(|_, s| s.name != name);
                self.theme.sync_ai_deleted(&name);
            }
            InstalledSaved { share, item, saved } => {
                let key = (share, item);
                self.worn_by_share.insert(key, saved.clone());
                if self.bound == Some(key) {
                    self.theme.sync_ai_installed_saved(saved);
                } else {
                    return
                }
            }
        }
        if self.board.is_none() && self.tab == PanelTab::Theme { self.theme.sync_header(cx, root); }
    }

    fn bind<L, T>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &Session<L, T>) {
        self.bind_target(cx, root, DesignTarget::LiveApp(session.share), session.item);
    }

    fn bind_target(&mut self, cx: &mut Cx, root: &WidgetRef, target: DesignTarget, item: Id) {
        let key = match target {
            DesignTarget::LiveApp(share) => Some((share, item)),
            DesignTarget::BoardDesign(_) => None,
        };
        if self.bound == key && (key.is_some() || self.board.as_ref().is_some_and(|b| b.item == item)) { return }
        // A resize bake belongs to the share it was dragged on; it never follows the panel to another one.
        self.pending_resize_bake = None;
        if let Some(old) = self.bound {
            if self.tree.focus != FocusState::default() {
                self.other_trees.insert(old, std::mem::take(&mut self.tree));
            }
            match self.theme.worn.take() {
                Some(saved) => { self.worn_by_share.insert(old, saved); }
                None => { self.worn_by_share.remove(&old); }
            }
        }
        let themes_dir = std::mem::take(&mut self.theme.themes_dir);
        self.theme.close_build_popovers(cx, root);
        let mixer_cache = std::mem::take(&mut self.mixer_cache);
        let worn_by_share = std::mem::take(&mut self.worn_by_share);
        let tree = key.and_then(|key| self.other_trees.remove(&key)).unwrap_or_default();
        let other_trees = std::mem::take(&mut self.other_trees);
        *self = Self { asking: self.asking, bound: key, mixer_cache, worn_by_share, tree, other_trees, ..Default::default() };
        self.theme.sync_mixer(cx, root);
        self.theme.sync_builder(cx, root);
        self.set_themes_dir(themes_dir);
        if let Some((key, saved)) = key.and_then(|key| self.worn_by_share.get(&key).cloned().map(|saved| (key, saved))) {
            self.theme.refresh_saved();
            if self.theme.saved_names.iter().any(|n| n == &saved.name) {
                self.theme.sync_ai_installed_saved(saved);
            } else {
                self.worn_by_share.remove(&key);
            }
        }
        for list in [ids!(tweak_seats), ids!(tweak_properties), ids!(tweak_shader_rows), ids!(tweak_tree), ids!(tweak_theme), ids!(tweak_history)] {
            replace(cx, root.widget(cx, list), Vec::new(), true);
        }
        if let Some(mut tree) = root.widget(cx, ids!(tweak_tree)).borrow_mut::<TweakList>() {
            tree.configure_tree_drag(cx, false, &self.tree);
        }
        root.text_input(cx, ids!(tree_filter.input)).set_text(cx, &self.tree.filter);
        root.text_input(cx, ids!(theme_filter.input)).set_text(cx, "");
        root.text_input(cx, ids!(theme_name)).set_text(cx, &self.theme.name_text);
        root.label(cx, ids!(tree_status)).set_text(cx, "");
        root.widget(cx, ids!(tree_status)).set_visible(cx, false);
        if let Some(mut list) = root.widget(cx, ids!(build_palette)).borrow_mut::<PaletteList>() {
            list.set_drag_enabled(false);
            list.palette = Default::default();
            list.redraw(cx);
        }
        root.text_input(cx, ids!(palette_filter.input)).set_text(cx, "");
        root.widget(cx, ids!(build_preview)).set_visible(cx, false);
        let board = key.is_none();
        if board {
            self.tab = PanelTab::Build;
            self.board = Some(BoardDesign { item, revision: None, failed_revision: None, pending: None, paths: Vec::new(), theme: None });
            root.drop_down(cx, ids!(board_widget)).set_labels(cx, Vec::new());
        }
        if let Some(mut tabs) = root.widget(cx, ids!(tweak_tabs)).borrow_mut::<TweakTabs>() {
            tabs.board = board;
            tabs.room = None;
            tabs.redraw(cx);
        }
        for id in [ids!(design_session_actions), ids!(history_title), ids!(tweak_history), ids!(theme_save_row),
            ids!(theme_filter), ids!(tweak_theme)] { root.widget(cx, id).set_visible(cx, !board); }
        root.widget(cx, ids!(board_widget)).set_visible(cx, board);
        self.show_tab(cx, root);
        root.widget(cx, ids!(tweak_hint)).set_visible(cx, !board);
        root.label(cx, ids!(tweak_result)).set_text(cx, "");
    }

    pub(super) fn refresh_board(&mut self, cx: &mut Cx, root: &WidgetRef, model: &Model, item: Id) {
        let first = self.board.as_ref().is_none_or(|b| b.item != item);
        self.bind_target(cx, root, DesignTarget::BoardDesign(item), item);
        if first {
            for id in [ids!(tweak_ask), ids!(tweak_seats), ids!(design_commit), ids!(eq_fold), ids!(tb_fold)] {
                root.widget(cx, id).set_visible(cx, false);
            }
            if let Some(read) = cx.tweak_callback {
                if let Ok(palette) = read(cx, "design_palette", &[]) {
                    if let (Ok(value), Some(mut list)) = (super::parse_json(&palette), root.widget(cx, ids!(build_palette)).borrow_mut::<PaletteList>()) {
                        if let Err(why) = list.palette.page(&value, true) { root.label(cx, ids!(tweak_result)).set_text(cx, &why); }
                        list.redraw(cx);
                    }
                }
                self.theme.snapshot = read(cx, "design_theme", &[("get".into(), "1".into())]).ok()
                    .and_then(|json| ThemeSnapshot::parse(&json));
            }
            self.theme.refresh_saved();
            self.theme.entries = self.theme.choice_list();
            let choices = root.drop_down(cx, ids!(theme_pick));
            choices.set_labels(cx, self.theme.entries.iter().map(|e| e.label().into()).collect());
            let index = self.theme.selected_choice_index();
            choices.set_selected_item(cx, index);
            let candidate = self.theme.board_theme(index);
            self.board.as_mut().unwrap().theme = candidate.ok();
        }
        let board = self.board.as_mut().unwrap();
        let had_index_error = board.failed_revision.is_some();
        let loaded = board.refresh_index(cx, model, first);
        if had_index_error && board.failed_revision.is_none() {
            root.label(cx, ids!(tweak_result)).set_text(cx, "");
        }
        if let Some(loaded) = loaded {
            match loaded {
                Ok(paths) => {
                    board.paths = paths;
                    let index = board.paths.iter().position(|p| p == self.sidebar.path.as_ref()).unwrap_or(0);
                    self.sidebar.path = Rc::from(board.paths.get(index).map_or("", String::as_str));
                    let widgets = root.drop_down(cx, ids!(board_widget));
                    widgets.set_labels(cx, board.paths.clone());
                    widgets.set_selected_item(cx, index);
                }
                Err(err) => {
                    board.paths.clear();
                    self.sidebar.path = Rc::from("");
                    root.drop_down(cx, ids!(board_widget)).set_labels(cx, vec![]);
                    root.label(cx, ids!(tweak_result)).set_text(cx, &err);
                }
            }
            root.redraw(cx);
        }
        if board.pending.is_some() { cx.new_next_frame(); }
        let edit = model.state.active_member(&model.me()) && board.revision.is_some();
        design_view::enable_selection(cx, root, edit && !self.sidebar.path.is_empty());
        root.button(cx, ids!(theme_bake)).set_enabled(cx, edit && board.theme.is_some());
        root.label(cx, ids!(build_status)).set_text(cx, &format!("Insert: {}", self.build.ty()));
    }

    pub(super) fn board_index_frame(&mut self, cx: &mut Cx, selected: Option<DesignTarget>) -> bool {
        let Some(board) = &mut self.board else { return false };
        let Some((_, handle)) = &board.pending else { return false };
        if selected != Some(DesignTarget::BoardDesign(board.item)) {
            board.pending = None;
            return false
        }
        if handle.is_finished() { return true }
        cx.new_next_frame();
        false
    }

    pub(super) fn board_actions(&mut self, cx: &mut Cx, root: &WidgetRef, model: &mut Model, item: Id, actions: &Actions) -> Option<Id> {
        if !root.visible() || self.board.as_ref()?.item != item { return None }
        for (tab, word, icon) in [(PanelTab::Theme, ids!(tab_theme), ids!(tab_theme_i)), (PanelTab::Build, ids!(tab_build), ids!(tab_build_i))] {
            if root.button(cx, word).clicked(actions) || root.button(cx, icon).clicked(actions) {
                self.tab = tab;
                self.show_tab(cx, root);
                return None
            }
        }
        let board = self.board.as_mut()?;
        let revision = board.revision?;
        let outcome = match self.tab {
            PanelTab::Build => {
                if let Some(path) = root.drop_down(cx, ids!(board_widget)).selected(actions).and_then(|i| board.paths.get(i)) {
                    self.sidebar.path = Rc::from(path.as_str());
                    return None
                }
                if self.build.palette_action(cx, root, actions) {
                    root.label(cx, ids!(build_status)).set_text(cx, &format!("Insert: {}", self.build.ty()));
                    return None
                }
                let request = self.build.request(cx, root, &root.widget(cx, ids!(build_strip)), actions, &self.sidebar.path, true)?;
                if !model.state.active_member(&model.me()) { return None }
                d1::D1Op::from_request(&request).and_then(|op| d1::apply_op(model, item, revision, &op, crate::model::Origin::Ui))
            }
            PanelTab::Theme => match self.theme.header_action(cx, root, actions)? {
                ThemeAction::PickChoice(index) => {
                    match self.theme.board_theme(index) {
                        Ok(install) => board.theme = Some(install),
                        Err(why) => {
                            board.theme = None;
                            root.label(cx, ids!(tweak_result)).set_text(cx, &why);
                        }
                    }
                    root.button(cx, ids!(theme_bake)).set_enabled(cx, board.theme.is_some() && model.state.active_member(&model.me()));
                    return None
                }
                ThemeAction::Bake if model.state.active_member(&model.me()) => {
                    let theme = board.theme.as_ref()?;
                    let tokens = theme.globals.iter().chain(&theme.overrides).map(|(name, value)|
                        Ok((name.clone(), d1::theme_val_literal(*value)?))).collect::<Result<BTreeMap<_,_>, d1::D1Error>>();
                    tokens.and_then(|tokens| d1::apply_theme_tokens(model, item, revision, Some(&theme.base),
                        theme.sheet.as_deref(), &tokens.into_iter().collect::<Vec<_>>(), crate::model::Origin::Ui))
                }
                _ => return None,
            },
            _ => return None,
        };
        match outcome {
            Ok(outcome) => {
                if let Some(path) = outcome.edit.focus_path { self.sidebar.path = Rc::from(path); }
                root.label(cx, ids!(tweak_result)).set_text(cx, "");
                self.refresh_board(cx, root, model, item);
                outcome.changed.then_some(outcome.revision)
            }
            Err(err) => {
                self.refresh_board(cx, root, model, item);
                root.label(cx, ids!(tweak_result)).set_text(cx, &err.to_string());
                None
            }
        }
    }

    pub fn refresh<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &Session<L, T>, state: &State, clock: &crate::view::LocalClock) {
        self.bind(cx, root, session);
        if self.tree.sync_seats(&session.view, state) && self.tab == PanelTab::Tree { self.draw_rows(cx, root, false); }
        self.sync_design(cx, root, session);
        let owner = session.me == session.sharer;
        let supported = session.supports_control_tweak();
        // An older sharer's guest without the right is told why instead of being offered an ask.
        let needs_right = !session.is_ended() && !owner && !session.view.allowed.contains(&session.me);
        let ask = root.button(cx, ids!(tweak_ask));
        ask.set_visible(cx, supported && needs_right);
        ask.set_text(cx, if self.asking == Some(session.share) { "Asking for control…" } else { "Ask for control" });
        let result = root.label(cx, ids!(tweak_result));
        if !supported && needs_right { result.set_text(cx, super::OLDER_SHARER_CONTROL); }
        else if result.text() == super::OLDER_SHARER_CONTROL { result.set_text(cx, ""); }
        let view = &session.view;
        let mut actors: BTreeSet<_> = view.allowed.iter().map(|&seat| (seat, None)).collect();
        actors.extend(view.running.keys().copied());
        for &(seat, agent) in view.agents.keys() {
            actors.insert((seat, None));
            actors.insert((seat, Some(agent)));
        }
        let title = state.entities.get(&session.item).map_or("the shared app", |e| e.text(field::TITLE));
        let chips: Vec<_> = actors.into_iter().map(|(seat, agent)| {
            let mut name = who(state, &seat, agent);
            let activity = view.running.get(&(seat, agent)).and_then(|(_, r)| r.running_text(title)).or_else(|| {
                let ai = (seat, agent?);
                if view.looking.contains_key(&ai) { return Request::Look.running_text(title) }
                let (request, at) = view.finished.get(&ai)?;
                let done = request.done_text(title)?;
                Some(format!("{done} · {}", clock.hm(*at)))
            });
            if let Some(activity) = activity {
                name.push_str(" · ");
                name.push_str(&activity);
            }
            let (status, action) = match agent {
                Some(id) if view.agents.get(&(seat, id)) == Some(&true) => ("stopped", Some(ChipAction::Resume(id))),
                Some(id) => ("", Some(ChipAction::Stop(id))),
                None if session.control_rights().is_some_and(|rights| rights.seats.contains(&seat)) && view.allowed.contains(&seat) =>
                    ("Allowed", Some(ChipAction::Revoke)),
                None if view.allowed.contains(&seat) => ("Allowed", None),
                None => ("", None),
            };
            let action = if owner && !session.is_ended() { action } else { None };
            Rc::new(Line::Chip { share: session.share, seat, item: session.item, name, ink: seat_colour(state, &seat), status, action })
        }).collect();
        let list = root.widget(cx, ids!(tweak_seats));
        list.set_visible(cx, !chips.is_empty());
        // The chips take the rows they need, up to four, and scroll beyond; the properties keep the rest.
        let height = Size::Fixed(chips.len().clamp(1, 4) as f64 * SEAT_ROW);
        if let Some(mut seats) = list.borrow_mut::<TweakList>() {
            if seats.view.walk.height != height { seats.view.walk.height = height; seats.view.redraw(cx); }
        }
        replace(cx, list, chips, false);
    }

    /// Examine only actions captured while dispatching this original event to the panel.
    pub(super) fn local_input<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, event: &Event, root: &WidgetRef,
        actions: &Actions, session: &mut Session<L, T>, granted: impl FnOnce(&Cx) -> bool) {
        if !root.visible() || !matches!(event,
            Event::MouseUp(_) | Event::TouchUpdate(_) | Event::KeyUp(_) | Event::A11yAction(_)) { return }
        if self.bound == Some((session.share, session.item)) && self.tab == PanelTab::Build
            && session.me == session.sharer && !session.is_ended() && session.view.design.dirty_files > 0
            && root.button(cx, ids!(design_commit)).clicked(actions) {
            if granted(cx) {
                if let Err(why) = session.commit_design_to_disk() { root.label(cx, ids!(tweak_result)).set_text(cx, &why); }
            }
            return
        }
        for (_, row) in root.portal_list(cx, ids!(tweak_seats_list)).items_with_actions(actions) {
            // Use the binding actually painted under the press, even if the roster has since changed.
            let Some(shown) = row.borrow::<TweakRow>().and_then(|r| r.shown.clone()) else { continue };
            let Line::Chip { seat, item, action: Some(action), .. } = &*shown else { continue };
            if *item != session.item || session.me != session.sharer { continue }
            let (id, stop, button) = match *action {
                ChipAction::Stop(id) => (id, true, ids!(tweak_stop)),
                ChipAction::Resume(id) => (id, false, ids!(tweak_allow)),
                ChipAction::Revoke => continue, // The button emits the call's scoped RevokeControl action.
            };
            if session.view.agents.get(&(*seat, id)).copied() != Some(!stop) || !row.button(cx, button).clicked(actions) { continue }
            if !granted(cx) { return }
            if let Err(why) = session.stop_agent(*seat, id, stop) { root.label(cx, ids!(tweak_result)).set_text(cx, why); }
            break
        }
    }

    pub fn actions<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, actions: &Actions, session: &mut Session<L, T>) {
        if !root.visible() || self.bound != Some((session.share, session.item)) { return }
        let tab = if root.button(cx, ids!(tab_props)).clicked(actions) || root.button(cx, ids!(tab_props_i)).clicked(actions) { PanelTab::Props }
            else if root.button(cx, ids!(tab_shader)).clicked(actions) || root.button(cx, ids!(tab_shader_i)).clicked(actions) { PanelTab::Shader }
            else if root.button(cx, ids!(tab_tree)).clicked(actions) || root.button(cx, ids!(tab_tree_i)).clicked(actions) { PanelTab::Tree }
            else if root.button(cx, ids!(tab_theme)).clicked(actions) || root.button(cx, ids!(tab_theme_i)).clicked(actions) { PanelTab::Theme }
            else if root.button(cx, ids!(tab_build)).clicked(actions) || root.button(cx, ids!(tab_build_i)).clicked(actions) { PanelTab::Build }
            else { self.tab };
        if tab != self.tab {
            self.tree.cancel_reveal();
            if self.tab == PanelTab::Theme && self.theme.confirm.take().is_some() { self.theme.msg.clear(); }
            self.tab = tab;
            if tab == PanelTab::Theme {
                self.theme.refresh_saved();
                if self.theme.snapshot.is_none() && self.theme.inflight_get.is_none() { self.request_theme(session); }
                self.theme.sync_header(cx, root);
            }
            if tab == PanelTab::Tree {
                let ready = self.tree.loaded || self.tree.inflight_root.is_some() || self.send_tree(session, TreeState::root());
                if ready && !self.sidebar.path.is_empty() {
                    self.follow_selection(cx, root, session, &self.sidebar.path.clone(), true);
                }
            }
            if tab == PanelTab::Build { self.load_palette(cx, root, session); }
            self.sync_design(cx, root, session);
            self.show_tab(cx, root);
            self.draw_rows(cx, root, false);
            return;
        }
        if matches!(self.tab, PanelTab::Tree | PanelTab::Build) {
            let build = self.tab == PanelTab::Build;
            let strip = root.widget(cx, if build { ids!(build_strip) } else { ids!(tree_strip) });
            if let Some(req) = self.build.request(cx, root, &strip, actions, &self.sidebar.path, build) {
                self.request_design(cx, root, session, req);
                return
            }
        }
        let (properties, list) = match self.tab {
            PanelTab::Build => {
                if self.build.palette_action(cx, root, actions) { self.sync_design(cx, root, session); }
                return
            }
            PanelTab::Theme => {
                if let Some(action) = self.theme.header_action(cx, root, actions) {
                    self.handle_theme_action(cx, root, session, action);
                    return
                }
                (ids!(tweak_theme), ids!(tweak_theme_list))
            }
            PanelTab::Tree => {
                if self.focus_action(cx, root, actions, session) { return }
                if let Some(action) = self.tree_action(cx, root, actions) {
                    self.handle_tree_action(cx, root, action, session);
                }
                return
            }
            PanelTab::Props => (ids!(tweak_properties), ids!(tweak_properties_list)),
            PanelTab::Shader => {
                if self.shader.actions(cx, root, actions, &self.sidebar, session, &mut self.proposals) {
                    self.draw_rows(cx, root, true);
                    return;
                }
                (ids!(tweak_shader_rows), ids!(tweak_shader_list))
            }
        };
        self.handle_prop_list_actions(cx, root, actions, session, root.widget(cx, properties), root.portal_list(cx, list));
    }

    fn locate_selection<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &mut Session<L,T>) {
        if self.sidebar.location_checked || self.sidebar.path.is_empty() || !session.supports_design()
            || !session.view.design.active || session.is_ended() || (session.me != session.sharer && !session.view.allowed.contains(&session.me)) { return }
        self.sidebar.location_checked = true;
        self.request_design(cx, root, session, DesignRequest::State { path: Some(self.sidebar.path.to_string()) });
    }

    fn request_design<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef,
        session: &mut Session<L,T>, req: DesignRequest) -> bool {
        if !session.supports_design() || session.is_ended() { return false }
        let request = Request::Design(req.clone());
        if session.may_act(&session.me, None, &request).is_err() { return false }
        match session.request(None, request) {
            Ok(id) => { self.build.pending.insert(id, req); true }
            Err(why) => {
                root.label(cx, ids!(tweak_result)).set_text(cx, &why);
                false
            }
        }
    }

    fn load_palette<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &mut Session<L,T>) {
        if !self.build.loaded && !self.build.pending.values().any(|r| matches!(r, DesignRequest::Palette { .. })) {
            self.request_design(cx, root, session, DesignRequest::Palette { query: None, offset: 0, limit: super::wire::MAX_PALETTE_LIMIT });
        }
    }

    fn sync_design<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &Session<L,T>) {
        let supported = session.supports_design();
        let read = supported && !session.is_ended();
        let owner = session.me == session.sharer;
        let edit = read && (owner || session.view.allowed.contains(&session.me));
        if let Some(mut tree) = root.widget(cx, ids!(tweak_tree)).borrow_mut::<TweakList>() {
            tree.configure_tree_drag(cx, edit, &self.tree);
        }
        if let Some(mut list) = root.widget(cx, ids!(build_palette)).borrow_mut::<PaletteList>() { list.set_drag_enabled(edit); }
        root.button(cx, ids!(theme_bake)).set_enabled(cx, edit);
        let rename = edit && self.sidebar.located;
        if self.sidebar.rename.editable != rename {
            self.sidebar.rename.editable = rename;
            if !rename { self.sidebar.rename = Default::default(); }
            if self.tab == PanelTab::Props { self.draw_rows(cx, root, false); }
        }
        let status = &session.view.design;
        design_view::enable_selection(cx, root, edit && !self.sidebar.path.is_empty());
        for (id, enabled) in [(ids!(design_undo), edit && status.undo > 0), (ids!(design_redo), edit && status.redo > 0),
            (ids!(design_reset), edit && status.dirty_files > 0), (ids!(design_bake), edit), (ids!(design_patch), edit),
            (ids!(design_diff), read), (ids!(design_verify), read)] {
            root.button(cx, id).set_enabled(cx, enabled);
        }
        root.widget(cx, ids!(design_commit)).set_visible(cx, read && owner && status.dirty_files > 0);
        if self.tab == PanelTab::Build {
            let label = if !supported { super::OLDER_SHARER_DESIGN.into() } else {
                let target = if self.sidebar.path.is_empty() {
                    "Select a widget".into()
                } else if self.sidebar.ty.is_empty() {
                    self.sidebar.path.to_string()
                } else {
                    format!("{} ({})", self.sidebar.path, self.sidebar.ty)
                };
                let dirty = if status.dirty_files == 1 { "1 dirty file".into() }
                    else { format!("{} dirty files", status.dirty_files) };
                format!("{target} · Insert: {} · {dirty}", self.build.ty())
            };
            root.label(cx, ids!(build_status)).set_text(cx, &label);
        }
    }

    fn design_notice<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef,
        session: &mut Session<L,T>, seat: Seat, req: RequestId, agent: Option<Id>, notice: &Notice) -> bool {
        let own = seat == session.me && agent.is_none();
        match notice {
            Notice::Design(DesignNotice::Status(_)) => {
                self.locate_selection(cx, root, session);
                self.sync_design(cx, root, session);
                if self.tab == PanelTab::Build { self.load_palette(cx, root, session); }
            }
            Notice::Design(DesignNotice::Reply { kind, json }) if own => {
                let Some(request) = self.build.pending.remove(&req) else { return false };
                let Ok(value) = super::parse_json(json) else { return true };
                if let DesignRequest::State { path: Some(path) } = &request {
                    if path == self.sidebar.path.as_ref() {
                        self.sidebar.located = value.get("located").is_some_and(|v| v.get("file").and_then(|v| v.as_str()).is_some());
                        self.sync_design(cx, root, session);
                    }
                    return true
                }
                if let DesignRequest::Palette { offset, limit, .. } = request {
                    if let Some(mut list) = root.widget(cx, ids!(build_palette)).borrow_mut::<PaletteList>() {
                        if let Err(why) = list.palette.page(&value, offset == 0) {
                            root.label(cx, ids!(tweak_result)).set_text(cx, &why);
                            return true
                        }
                        list.redraw(cx);
                    }
                    if let Some(next) = value.get("next_offset").and_then(|v| v.as_u64()).and_then(|v| u32::try_from(v).ok()).filter(|&v| v > offset) {
                        self.request_design(cx, root, session, DesignRequest::Palette { query: None, offset: next, limit });
                    } else { self.build.loaded = true; }
                } else {
                    let preview = match kind {
                        DesignReplyKind::Diff => value.get("patch").and_then(|v| v.as_str()).unwrap_or(""),
                        DesignReplyKind::WritePatch => value.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                        DesignReplyKind::Verify => json.as_str(),
                        _ => return true,
                    };
                    root.label(cx, ids!(build_preview)).set_text(cx, if preview.is_empty() { "No changes" } else { preview });
                    root.widget(cx, ids!(build_preview)).set_visible(cx, true);
                }
            }
            Notice::Design(DesignNotice::Mutated { focus_path, .. }) => {
                if let Some(request) = own.then(|| self.build.pending.remove(&req)).flatten() {
                    let current = match &request {
                        DesignRequest::Rename { path, .. } => {
                            let current = self.sidebar.path.as_ref() == path;
                            if current { self.sidebar.rename = Default::default(); }
                            current
                        }
                        _ => true,
                    };
                    if let Some(path) = focus_path.as_ref().filter(|_| current) {
                        let _ = session.request(None, Request::Select(Some(path.clone())));
                    }
                }
                self.tree.loaded = false;
                if self.tab == PanelTab::Tree {
                    let query = self.tree.clear_for_refresh();
                    self.send_tree(session, query);
                }
                root.widget(cx, ids!(build_preview)).set_visible(cx, false);
            }
            Notice::Refused(why) if own => {
                match self.build.pending.remove(&req) {
                    Some(DesignRequest::State { path: Some(_) }) => {}
                    Some(DesignRequest::Rename { path, name, .. }) if path == self.sidebar.path.as_ref() => {
                        let field = &mut self.sidebar.rename;
                        field.busy = false;
                        field.draft = Some(name);
                        let refusal = super::parse_json(why).ok();
                        let lines = refusal.as_ref().and_then(|v| v.get("rust_refs")?.as_arr()).into_iter().flatten()
                            .filter_map(|r| r.get("line")?.as_u64()).map(|n| n.to_string()).collect::<Vec<_>>();
                        field.confirm = !lines.is_empty();
                        field.message = if field.confirm { format!("Referenced in Rust at lines {}", lines.join(", ")) } else { why.clone() };
                        self.draw_rows(cx, root, false);
                    }
                    Some(_) => root.label(cx, ids!(tweak_result)).set_text(cx, why),
                    None => return false,
                }
            }
            _ => return false,
        }
        true
    }

    fn show_tab(&self, cx: &mut Cx, root: &WidgetRef) {
        let build = self.tab == PanelTab::Build;
        root.widget(cx, ids!(tweak_build_wrap)).set_visible(cx, build);
        let props = self.tab == PanelTab::Props;
        root.widget(cx, ids!(tweak_props_wrap)).set_visible(cx, props);
        let shader = self.tab == PanelTab::Shader;
        let tree = self.tab == PanelTab::Tree;
        let theme = self.tab == PanelTab::Theme;
        root.widget(cx, ids!(tweak_theme_wrap)).set_visible(cx, theme);
        root.widget(cx, ids!(tweak_shader_wrap)).set_visible(cx, shader);
        root.widget(cx, ids!(tweak_tree_wrap)).set_visible(cx, tree);
        root.widget(cx, ids!(tweak_hint)).set_visible(cx, self.sidebar.path.is_empty() && (props || shader));
        for (id, active) in [(ids!(tab_props), props), (ids!(tab_props_i), props),
            (ids!(tab_shader), shader), (ids!(tab_shader_i), shader), (ids!(tab_tree), tree), (ids!(tab_tree_i), tree),
            (ids!(tab_theme), theme), (ids!(tab_theme_i), theme), (ids!(tab_build), build), (ids!(tab_build_i), build)] {
            set_tab_fill(cx, root.widget(cx, id), active);
        }
    }

    pub(super) fn focus_for(&self, share: SessionId, item: Id) -> Option<&FocusState> {
        let key = (share, item);
        if self.bound == Some(key) { Some(&self.tree.focus) }
        else { self.other_trees.get(&key).map(|tree| &tree.focus) }
    }

    pub(super) fn scale_focus_zoom(&mut self, cx: &mut Cx, root: &WidgetRef, share: SessionId, item: Id, factor: f64) {
        let key = (share, item);
        if self.bound == Some(key) {
            self.tree.focus.scale_zoom(factor);
            if self.tab == PanelTab::Tree { self.focus_header(cx, root); }
        } else if let Some(tree) = self.other_trees.get_mut(&key) { tree.focus.scale_zoom(factor); }
    }

    fn focus_header(&self, cx: &mut Cx, root: &WidgetRef) {
        let rule = self.tree.focus.rule();
        let isolate = root.widget(cx, ids!(tree_isolate));
        isolate.set_disabled(cx, !rule.controls_enabled && self.sidebar.path.is_empty());
        set_tab_fill(cx, isolate, rule.controls_enabled);
        let center = root.widget(cx, ids!(tree_center));
        center.set_disabled(cx, !rule.controls_enabled);
        set_tab_fill(cx, center, rule.center);
        root.widget(cx, ids!(tree_zoom_out)).set_disabled(cx, !rule.controls_enabled || rule.zoom <= MIN_ZOOM);
        root.widget(cx, ids!(tree_zoom_in)).set_disabled(cx, !rule.controls_enabled || rule.zoom >= MAX_ZOOM);
        root.label(cx, ids!(tree_zoom)).set_text(cx, &zoom_label(rule.zoom));
        let hint = root.label(cx, ids!(isolate_hint));
        hint.set_visible(cx, self.tree.focus.hint.is_some());
        hint.set_text(cx, self.tree.focus.hint.as_deref().unwrap_or(""));
    }

    fn focus_action<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, actions: &Actions,
        session: &mut Session<L, T>) -> bool {
        let clicked = |id| root.button(cx, id).clicked(actions);
        if clicked(ids!(tree_isolate)) {
            if let Some(q) = self.tree.toggle_isolate(&self.sidebar.path, self.sidebar.rect) { self.send_tree(session, q); }
            self.draw_rows(cx, root, false);
            return true
        } else if clicked(ids!(tree_center)) { self.tree.focus.set_center(!self.tree.focus.center); }
        else if clicked(ids!(tree_zoom_out)) { self.tree.focus.step_zoom(-ZOOM_STEP); }
        else if clicked(ids!(tree_zoom_in)) { self.tree.focus.step_zoom(ZOOM_STEP); }
        else { return false }
        self.focus_header(cx, root);
        true
    }

    fn send_tree<L: Copy + Eq, T: Copy + Eq>(&mut self, session: &mut Session<L, T>, query: TreeQuery) -> bool {
        Self::send_tree_for(&mut self.tree, session, query)
    }

    fn send_tree_for<L: Copy + Eq, T: Copy + Eq>(tree: &mut TreeState, session: &mut Session<L, T>, query: TreeQuery) -> bool {
        match session.request(None, Request::Tree(query.clone())) {
            Ok(req) => { tree.mark_sent(req, &query); true }
            Err(why) => { tree.mark_send_failed(&query, &why); false }
        }
    }

    fn follow_selection<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef,
        session: &mut Session<L, T>, path: &str, scroll_visible: bool) {
        if self.tree.find_row_index(path).is_some() {
            self.tree.cancel_reveal();
            if scroll_visible { self.tree.scroll_to = Some(path.into()); }
            return
        }
        if !self.tree.filter.is_empty() {
            self.tree.offer_search(String::new());
            root.text_input(cx, ids!(tree_filter.input)).set_text(cx, "");
        }
        if !self.tree.expand_cached_ancestors(path) {
            if let Some(query) = self.tree.offer_reveal(path) { self.send_tree(session, query); }
        }
    }

    fn tree_action(&self, cx: &mut Cx, root: &WidgetRef, actions: &Actions) -> Option<TreeAction> {
        let input = root.text_input(cx, ids!(tree_filter.input));
        if input.escaped(actions) {
            input.set_text(cx, "");
            return Some(TreeAction::Filter(String::new()))
        }
        if let Some(text) = input.changed(actions) { return Some(TreeAction::Filter(text)) }
        if root.button(cx, ids!(tree_refresh)).clicked(actions) { return Some(TreeAction::Refresh) }
        let tree = root.widget(cx, ids!(tweak_tree));
        if let Some(action) = actions.iter().filter_map(|a| a.as_widget_action()).find_map(|w| {
            (w.widget_uid == tree.widget_uid()).then(|| w.action.downcast_ref::<TreeAction>()).flatten()
        }) { return Some(action.clone()) }
        let list = tree.borrow::<TweakList>()?;
        for (i, item) in root.portal_list(cx, ids!(tweak_tree_list)).items_with_actions(actions) {
            let Some(row) = item.borrow::<TweakRow>() else { continue };
            let Some(shown) = row.shown.as_ref() else { continue };
            if !list.rows.get(i).is_some_and(|current| Rc::ptr_eq(shown, current) || shown == current) { continue }
            if let Line::Tree(line) = shown.as_ref() {
                if let Some(action) = line.handle_tree_actions(cx, &item, actions, !self.tree.filter.is_empty()) { return Some(action) }
            }
        }
        None
    }

    fn handle_tree_action<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, action: TreeAction,
        session: &mut Session<L, T>) {
        let query = match action {
            TreeAction::Move { source, target, place } => {
                if self.tree.filter.is_empty() {
                    self.request_design(cx, root, session, DesignRequest::Move { path: source, target, place });
                }
                return
            }
            TreeAction::Select(path) => {
                if let Err(why) = session.request(None, Request::Select(Some(path))) {
                    root.label(cx, ids!(tweak_result)).set_text(cx, &why);
                }
                return
            }
            TreeAction::Expand { path, offset } => self.tree.request_expand(&path, offset),
            TreeAction::Collapse(path) => { self.tree.collapse(&path); None }
            TreeAction::Filter(text) => {
                self.tree.cancel_reveal();
                self.tree.offer_search(text)
            }
            TreeAction::LoadMoreMatches(offset) => self.tree.more_matches(offset),
            TreeAction::Refresh => {
                root.text_input(cx, ids!(tree_filter.input)).set_text(cx, "");
                Some(self.tree.clear_for_refresh())
            }
        };
        if let Some(query) = query { self.send_tree(session, query); }
        self.draw_rows(cx, root, false);
    }

    fn handle_prop_list_actions<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef,
        actions: &Actions, session: &mut Session<L, T>, properties: WidgetRef, list: PortalListRef) {
        let mut redraw = false;
        // A step or typed number can emit Changed and Ended in one batch.
        for action in actions {
            let actions = std::slice::from_ref(action);
            for (index, item) in list.items_with_actions(actions) {
                let Some(shown) = item.borrow::<TweakRow>().and_then(|r| r.shown.clone()) else { continue };
                // A recycled field must still be bound to the displayed path and property.
                let Some(current) = properties.borrow::<TweakList>().and_then(|list| list.rows.get(index).cloned()) else { continue };
                let (Line::Property(painted), Line::Property(line)) = (shown.as_ref(), current.as_ref()) else { continue };
                if !Rc::ptr_eq(&shown, &current) && !painted.same_binding(line) { continue }
                if let PropLine::Header { open: Some((key, _)), .. } = line {
                    if item.view(cx, ids!(header)).finger_up(actions).is_some_and(|tap| tap.is_over && tap.was_tap()) {
                        match key {
                            HeaderId::ShaderConstants => { self.shader.consts_open = !self.shader.consts_open; redraw = true; }
                            HeaderId::Theme(i) => self.handle_theme_action(cx, root, session, ThemeAction::ToggleSection(*i)),
                        }
                    }
                    continue;
                }
                let Some(row) = item.borrow::<TweakRow>() else { continue };
                let Some(action) = self.sidebar.handle_sidebar_actions(cx, &item, line, actions, &row.controls, &self.live) else { continue };
                match action {
                    SidebarAction::Rename { name, force } => {
                        self.sidebar.rename.confirm = false;
                        self.sidebar.rename.busy = self.request_design(cx, root, session, DesignRequest::Rename {
                            path: self.sidebar.path.to_string(), name, force_rust_refs: force,
                        });
                        redraw = true;
                    }
                    SidebarAction::Redraw => redraw = true,
                    SidebarAction::ClearRefusal(path, prop) => {
                        if path.as_ref() == "theme" { self.theme.refusals.remove(&prop); }
                        else { self.sidebar.refusals.remove(&prop); }
                        redraw = true;
                    }
                    SidebarAction::PaletteOpened(uid) => {
                        let swatch = item.widget(cx, ids!(swatch));
                        if swatch.widget_uid() == uid {
                            if let Some(mut pick) = swatch.borrow_mut::<fab_controls::FabColorPick>() {
                                pick.set_palette(cx, self.theme.palette.entries());
                            }
                        }
                    }
                    SidebarAction::Sets { path, edits, phase } => {
                        for (prop, value) in edits.iter() {
                            let finished = self.live.offer(&path, &prop, value, phase, |path, prop, value| {
                                let had_refusal = if path.as_ref() == "theme" { self.theme.refusals.contains_key(prop) }
                                    else { self.sidebar.refusals.contains_key(prop) };
                                let sent = send_set(session, &mut self.sidebar, &mut self.theme, &mut self.edits, path, prop, value);
                                redraw |= had_refusal || sent.is_none() || super::props::layout_composited(prop)
                                    || matches!(line, PropLine::Prop { row, .. } if !row.comp_vals.is_empty());
                                sent
                            });
                            if let Some(path) = finished {
                                self.refresh_after_edit(cx, root, session, &path);
                            }
                        }
                    }
                }
            }
        }
        if redraw { self.draw_rows(cx, root, false); }
    }

    fn draw_rows(&mut self, cx: &mut Cx, root: &WidgetRef, reset_scroll: bool) {
        let (widget, lines) = match self.tab {
            PanelTab::Build => return,
            PanelTab::Theme => (ids!(tweak_theme), self.theme.lines()),
            PanelTab::Tree => {
                self.focus_header(cx, root);
                let (lines, scroll_index) = self.tree.lines(&self.sidebar.path);
                let status = root.label(cx, ids!(tree_status));
                status.set_text(cx, &self.tree.status);
                let status_in_list = matches!(lines.as_slice(), [TreeLine::Info { text, .. }] if text == &self.tree.status);
                status.set_visible(cx, !self.tree.status.is_empty() && !status_in_list);
                let widget = root.widget(cx, ids!(tweak_tree));
                let rows = {
                    let Some(mut list) = widget.borrow_mut::<TweakList>() else { return };
                    let edit = list.can_edit_tree;
                    list.configure_tree_drag(cx, edit, &self.tree);
                    lines.into_iter().enumerate().map(|(i, mut line)| {
                        if let (TreeLine::Node { path, drop, .. }, Some((index, target, zone))) = (&mut line, &list.indicator) {
                            if *index == i && path == target { *drop = Some(*zone); }
                        }
                        let line = Line::Tree(line);
                        match list.rows.get(i) {
                            Some(old) if **old == line => old.clone(),
                            _ => Rc::new(line),
                        }
                    }).collect()
                };
                replace(cx, widget, rows, false);
                if let Some(idx) = scroll_index {
                    let portal = root.portal_list(cx, ids!(tweak_tree_list));
                    portal.set_first_id_and_scroll(idx.saturating_sub(3), 0.);
                    portal.redraw(cx);
                }
                return
            }
            PanelTab::Props => (ids!(tweak_properties), self.sidebar.lines(&self.theme.palette)),
            PanelTab::Shader => {
                let layer = self.shader.sync(cx, root, &self.sidebar);
                let lines = layer.map_or_else(Vec::new, |l| self.shader.lines(&self.sidebar, l, &self.theme.palette));
                (ids!(tweak_shader_rows), lines)
            }
        };
        let widget = root.widget(cx, widget);
        let rows = {
            let Some(list) = widget.borrow::<TweakList>() else { return };
            lines.into_iter().enumerate().map(|(i, prop)| {
                let line = Line::Property(prop);
                match list.rows.get(i) {
                    Some(old) if **old == line => old.clone(),
                    _ => Rc::new(line),
                }
            }).collect()
        };
        replace(cx, widget, rows, reset_scroll);
    }

    fn mark_changed(&mut self, cx: &mut Cx, root: &WidgetRef) {
        if let Some(list) = root.widget(cx, ids!(tweak_history)).borrow::<TweakList>() {
            let edits = list.rows.iter().flat_map(|line| {
                let run = line.history_run();
                run.first.edit().into_iter().chain(run.last.edit())
            });
            self.sidebar.props.mark_changed(&self.sidebar.path, edits.clone());
            self.theme.mark_changed(edits);
        }
        if self.tab != PanelTab::Tree { self.draw_rows(cx, root, false); }
    }

    fn request_props<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &mut Session<L,T>,
        agent: Option<Id>, path: &str) {
        if agent.is_none() { self.locate_selection(cx, root, session); }
        match session.request(agent, Request::Props { path: path.to_owned() }) {
            Ok(id) => self.props = Some((id, path.to_owned())),
            Err(why) => root.label(cx, ids!(tweak_result)).set_text(cx, &why),
        }
    }

    fn refresh_after_edit<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef,
        session: &mut Session<L, T>, path: &str) {
        if path == "theme" { return }
        if self.sidebar.path.as_ref() == path && !self.live.open_on(path) && !session.is_ended()
            && session.may_act(&session.me, None, &Request::Props { path: path.into() }).is_ok() {
            self.request_props(cx, root, session, None, path);
        }
    }

    pub(super) fn resize_widget<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef,
        session: &mut Session<L,T>, resize: &super::overlay::Resize) {
        use super::{overlay::ResizePhase, props::live::Phase};
        if self.bound != Some((session.share, session.item)) { return }
        let path = if self.sidebar.path.as_ref() == resize.path.as_ref() { self.sidebar.path.clone() }
            else { Rc::from(resize.path.as_ref()) };
        let end = resize.phase != ResizePhase::Drag;
        let phase = if end { Phase::End } else { Phase::Step };
        if resize.phase == ResizePhase::Release && self.is_build_mode(session.share, session.item) {
            self.pending_resize_bake = Some(path.clone());
        }
        for (prop, value) in [("width", resize.width), ("height", resize.height)] {
            if let Some(value) = value {
                self.live.offer(&path, prop, format!("{}", value.round().max(1.) as u64), phase, |p, k, v|
                    send_set(session, &mut self.sidebar, &mut self.theme, &mut self.edits, p, k, v));
            }
        }
        if end { self.settle_resize(cx, root, session); }
    }

    fn settle_resize<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &mut Session<L,T>) {
        if session.is_ended() || !self.is_build_mode(session.share, session.item) {
            self.pending_resize_bake = None;
            return;
        }
        if self.pending_resize_bake.as_ref().is_some_and(|p| !self.live.open_on(p)) {
            let path = self.pending_resize_bake.take().unwrap();
            self.request_design(cx, root, session, DesignRequest::Bake { scope: super::BakeScope::Props, path: Some(path.to_string()), prop: None });
            if self.sidebar.path == path { let _ = session.request(None, Request::Pick(super::At::Path(path.to_string()))); }
        }
    }

    fn set_reply<L: Copy + Eq, T: Copy + Eq>(&mut self, session: &mut Session<L, T>, req: RequestId) -> Option<Rc<str>> {
        self.live.reply(req, |path, prop, value| send_set(session, &mut self.sidebar, &mut self.theme, &mut self.edits, path, prop, value))
    }

    fn select(&mut self, cx: &mut Cx, root: &WidgetRef, path: &str, ty: Option<&str>, rect: Option<super::Rect>) {
        let changed = self.sidebar.path.as_ref() != path;
        if changed {
            for id in [ids!(tweak_properties), ids!(tweak_shader_rows)] {
                replace(cx, root.widget(cx, id), Vec::new(), true);
            }
            self.shader.active_layer.clear();
            self.shader.synced_key = Default::default();
            self.shader.status.clear();
            self.sidebar = Sidebar { path: Rc::from(path), collapsed: self.sidebar.collapsed, ..Default::default() };
        }
        root.widget(cx, ids!(tweak_hint)).set_visible(cx, path.is_empty() && matches!(self.tab, PanelTab::Props | PanelTab::Shader));
        if let Some(ty) = ty { self.sidebar.ty = ty.to_owned(); }
        self.sidebar.rect = rect;
        self.props = None;
        if self.tab != PanelTab::Theme { self.draw_rows(cx, root, changed); }
    }

    fn ensure_theme<L: Copy + Eq, T: Copy + Eq>(&mut self, session: &mut Session<L, T>) {
        if self.theme.background_requested || self.theme.snapshot.is_some() || self.theme.inflight_get.is_some() || session.is_ended() { return }
        let get = Request::Theme(ThemeOp::Get);
        if session.may_act(&session.me, None, &get).is_err() { return }
        self.theme.background_requested = true;
        self.request_theme(session);
    }

    fn request_theme<L: Copy + Eq, T: Copy + Eq>(&mut self, session: &mut Session<L, T>) {
        match session.request(None, Request::Theme(ThemeOp::Get)) {
            Ok(req) => { self.theme.inflight_get = Some(req); self.theme.status = "Loading theme…".into(); }
            Err(why) => self.theme.status = why,
        }
    }

    fn send_install<L: Copy + Eq, T: Copy + Eq>(&mut self, session: &mut Session<L, T>, pending: PendingInstall) {
        let PendingInstall { install, meta } = pending;
        match session.request(None, Request::Theme(ThemeOp::Install(install))) {
            Ok(req) => {
                self.theme.inflight_install = Some(req);
                self.theme.last_own_install = Some(req);
                self.theme.installing = Some(meta);
            }
            Err(why) => self.theme.install_failed(meta, why),
        }
    }

    pub(super) fn ticking_share(&self) -> Option<(SessionId, Id)> {
        self.bound.filter(|_| self.needs_theme_tick())
    }

    pub(super) fn wants_mixer_cache(&self) -> Option<(SessionId, Id)> {
        self.bound.filter(|_| self.theme.eq_open && !self.theme.eq_reanchor_pending && self.theme.eq_lab.is_none())
    }

    pub(super) fn needs_theme_tick(&self) -> bool {
        self.theme.eq_open && !self.theme.eq_reanchor_pending && (self.theme.eq_lab.is_none()
            || self.theme.eq_apply_due && self.theme.inflight_install.is_none())
            || self.theme.tb_open && !self.theme.tb_reanchor_pending && (self.theme.tb_moving || self.theme.tb_apply_due && self.theme.inflight_install.is_none())
    }

    pub(super) fn tick_theme_at<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef,
        session: &mut Session<L, T>, now: f64) {
        if !self.needs_theme_tick() || self.bound != Some((session.share, session.item)) || session.is_ended() { return }
        if self.theme.tb_open {
            let next = self.theme.settle_build(now);
            if let Some(next) = next { self.send_install(session, next); }
            if !self.theme.tb_apply_due && self.tab == PanelTab::Theme { self.theme.sync_header(cx, root); }
        } else if self.theme.eq_lab.is_none() {
            self.theme.enter_prepared(&self.mixer_cache);
            if self.theme.eq_lab.is_some() && self.tab == PanelTab::Theme { self.theme.sync_mixer(cx, root); }
        } else {
            let immediate = self.theme.eq_apply_at_once;
            let next = self.theme.settle_mix(now);
            let settled = next.is_some() || !self.theme.eq_apply_due || immediate;
            if let Some(next) = next { self.send_install(session, next); }
            if settled && self.tab == PanelTab::Theme { self.theme.sync_header(cx, root); }
        }
    }

    fn handle_theme_action<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef,
        session: &mut Session<L, T>, action: ThemeAction) {
        let result = match action {
            ThemeAction::Bake => {
                self.request_design(cx, root, session, DesignRequest::Bake { scope: super::BakeScope::Theme, path: None, prop: None });
                return
            }
            ThemeAction::Build(action) => {
                if self.theme.tb_open && matches!(action, BuildAction::Fold) { self.theme.close_build_popovers(cx, root); }
                let moving = matches!(action, BuildAction::Slider { ended: false, .. } | BuildAction::Color { ended: false, .. });
                let pending = self.theme.build_action(action);
                if self.needs_theme_tick() { cx.new_next_frame(); }
                if moving {
                    self.theme.sync_build_controls(cx, root);
                    return
                }
                Ok(pending)
            }
            ThemeAction::Mix(action) => {
                if self.theme.tb_open && matches!(action, MixAction::Fold) { self.theme.close_build_popovers(cx, root); }
                let weight = matches!(action, MixAction::Weight { .. } | MixAction::Reset(_));
                let pending = self.theme.mix_action(action, &mut self.mixer_cache);
                if self.needs_theme_tick() { cx.new_next_frame(); }
                if weight {
                    self.theme.sync_weights(cx, root);
                    return
                }
                Ok(pending)
            }
            ThemeAction::PickChoice(i) => self.theme.pick(i),
            ThemeAction::SaveAs(text) => self.theme.save_as(&text).map(|_| None),
            ThemeAction::DeleteSaved => {
                let doomed = self.theme.worn.as_ref().map(|s| s.name.clone());
                match self.theme.delete_saved() {
                    Ok(Some(fallback)) => {
                        if let Some(name) = doomed { self.worn_by_share.retain(|_, s| s.name != name); }
                        let message = std::mem::take(&mut self.theme.msg);
                        let next = self.theme.pick(fallback);
                        self.theme.msg = message;
                        next
                    }
                    Ok(None) => Ok(None), Err(e) => Err(e),
                }
            }
            ThemeAction::Name(text) => {
                if self.theme.name_text != text {
                    self.theme.name_text = text;
                    if self.theme.confirm.take().is_some() { self.theme.msg.clear(); }
                }
                Ok(None)
            }
            ThemeAction::ToggleSection(i) => {
                let folded = &mut self.theme.collapsed[usize::from(i)];
                *folded = !*folded;
                self.draw_rows(cx, root, false);
                return
            }
            ThemeAction::Filter(text) => {
                self.theme.filter = text;
                self.draw_rows(cx, root, true);
                return
            }
        };
        match result {
            Ok(Some(install)) => self.send_install(session, install),
            Err(e) => { self.theme.confirm = None; self.theme.msg = e.to_string(); }
            Ok(None) => {}
        }
        self.theme.sync_header(cx, root);
    }

    fn theme_refusal<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, session: &mut Session<L, T>, req: RequestId, why: &str) -> Option<()> {
        if self.theme.inflight_get.take_if(|r| *r == req).is_some() {
            self.theme.status = why.into();
        } else if self.theme.inflight_install == Some(req) {
            let next = self.theme.install_settled(req);
            if let Some(meta) = self.theme.installing.take() { self.theme.install_failed(meta, why.into()); }
            if let Some(next) = next { self.send_install(session, next); }
            if self.needs_theme_tick() { cx.new_next_frame(); }
        } else { return None }
        Some(())
    }

    pub(super) fn restore_selection<L: Copy + Eq, T: Copy + Eq>(
        &mut self, cx: &mut Cx, root: &WidgetRef, session: &mut Session<L, T>,
    ) {
        self.ensure_theme(session);
        if self.last_selection.is_none() {
            if let Some(selection) = session.view.selections.get(&session.me).cloned() {
                self.last_selection = Some(0);
                self.select(cx, root, &selection.path, None, selection.rect);
                self.request_props(cx, root, session, selection.agent, &selection.path);
            }
        }
    }

    pub(super) fn background_tree_notice<L: Copy + Eq, T: Copy + Eq>(&mut self, session: &mut Session<L, T>,
        seat: Seat, req: RequestId, notice: &Notice) {
        let key = (session.share, session.item);
        let tree = if self.bound == Some(key) { &mut self.tree }
            else if let Some(tree) = self.other_trees.get_mut(&key) { tree } else { return };
        Self::observe_tree(tree, session, seat, req, notice);
    }

    /// Only tree reads owned by this panel can complete a refresh or confirm a missing target.
    fn observe_tree<L: Copy + Eq, T: Copy + Eq>(tree: &mut TreeState, session: &mut Session<L, T>,
        seat: Seat, req: RequestId, notice: &Notice) -> bool {
        let next = match notice {
            Notice::Tree { query, tree: json } if seat == session.me => tree.apply_reply(req, query, json),
            Notice::Refused(why) if seat == session.me => match tree.apply_refusal(req, why) {
                Some(next) => next,
                None => return false,
            },
            Notice::Picked { path, rect, .. } | Notice::Selected { path: Some(path), rect: Some(rect) }
                if tree.focus.isolated_path.as_ref() == Some(path) => {
                    tree.focus.update_rect(*rect);
                    return false
                }
            Notice::Applied { .. } => tree.focus.isolated_path.clone().and_then(|path| {
                let request = Request::Tree(super::tree::reveal(path.clone()));
                if session.may_act(&session.me, None, &request).is_ok() { tree.offer_reveal(&path) } else { None }
            }),
            _ => return false,
        };
        if let Some(q) = next { Self::send_tree_for(tree, session, q); }
        matches!(notice, Notice::Tree { .. } | Notice::Refused(_))
    }

    pub fn notice<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &mut Session<L, T>,
        seat: Seat, req: RequestId, agent: Option<Id>, state: &State, notice: &Notice) {
        if agent.is_some() && matches!(notice, Notice::Theme { .. }) { return }
        if agent.is_none() {
            self.bind(cx, root, session);
        } else if self.bound != Some((session.share, session.item)) {
            if matches!(notice, Notice::Applied { path, prop, .. } if path == "theme" && prop == "install") {
                self.clear_worn(cx, root, session.share, session.item);
            }
            return
        }
        if self.design_notice(cx, root, session, seat, req, agent, notice) { return }
        let had_isolation = self.tree.focus.isolated_path.is_some();
        if Self::observe_tree(&mut self.tree, session, seat, req, notice) {
            if self.tab == PanelTab::Tree { self.draw_rows(cx, root, false); }
            return
        }
        let dots_changed = self.tree.sync_seats(&session.view, state);
        let rect_refresh = seat == session.me && req == 0
            && matches!(notice, Notice::Selected { path: Some(p), rect: Some(_) } if p == self.sidebar.path.as_ref());
        let selects = seat == session.me && !rect_refresh && match notice {
            Notice::Picked { .. } => self.last_selection.is_none_or(|last| req > last),
            Notice::Selected { .. } => self.last_selection.is_none_or(|last| req >= last),
            _ => false,
        };
        let isolation_cleared = had_isolation && self.tree.focus.isolated_path.is_none();
        if self.tab == PanelTab::Tree && !selects && (dots_changed || isolation_cleared) {
            self.draw_rows(cx, root, false);
        }
        let who = who(state, &seat, agent);
        let text = match notice {
            Notice::Allowed(true) => {
                if seat == session.me {
                    self.ensure_theme(session);
                    self.locate_selection(cx, root, session);
                    if self.tab == PanelTab::Theme { self.theme.sync_header(cx, root); }
                }
                format!("{who} can edit")
            }
            Notice::Allowed(false) => {
                if seat == session.me { self.theme.background_requested = false; }
                format!("Editing permission revoked for {who}")
            }
            Notice::Theme { theme } => {
                if let Some(snapshot) = ThemeSnapshot::parse(theme) {
                    self.theme.palette = snapshot.palette();
                    self.theme.snapshot = Some(snapshot);
                    self.theme.received_snapshot();
                    if self.needs_theme_tick() { cx.new_next_frame(); }
                    self.theme.background_requested = true;
                    if seat == session.me && self.theme.inflight_get == Some(req) { self.theme.inflight_get = None; }
                    self.theme.status.clear();
                    if self.tab == PanelTab::Theme { self.theme.sync_header(cx, root); }
                    if self.tab != PanelTab::Tree { self.draw_rows(cx, root, false); }
                }
                return
            }
            Notice::Picked { path, ty, rect } => {
                if selects {
                    self.last_selection = Some(req);
                    if self.tab == PanelTab::Tree { self.follow_selection(cx, root, session, path, true); }
                    self.select(cx, root, path, Some(ty), Some(*rect));
                    self.request_props(cx, root, session, agent, path);
                }
                format!("{who} picked {ty} at {path}")
            }
            Notice::Selected { path, rect } => {
                if rect_refresh {
                    self.sidebar.rect = *rect;
                } else if selects {
                    self.last_selection = Some(req);
                    let ty = path.as_deref().and_then(|p| self.tree.type_of(p)).map(str::to_owned);
                    if self.tab == PanelTab::Tree {
                        if let Some(path) = path { self.follow_selection(cx, root, session, path, false); }
                        else { self.tree.cancel_reveal(); }
                    }
                    self.select(cx, root, path.as_deref().unwrap_or(""), ty.as_deref(), *rect);
                    if let Some(path) = path { self.request_props(cx, root, session, agent, path); }
                }
                return
            }
            Notice::Rows { path, rows } if seat == session.me => {
                if !self.props.as_ref().is_some_and(|(id, selected)| req == *id && path == selected) { return }
                let Some(props) = Props::rebuild_rows(rows) else { return };
                self.sidebar.layout_edits.retain(|set, _| *set > req);
                self.sidebar.components.retain(|set, _| *set > req);
                self.sidebar.props = props;
                self.mark_changed(cx, root);
                return
            }
            Notice::Applied { path, prop, old, new, seq } => {
                let own_edit = seat == session.me && self.edits.remove(&req).is_some();
                if path == "theme" {
                    if prop == "install" {
                        self.theme.awaiting_snapshot = true;
                        if seat != session.me || agent.is_some() || Some(req) != self.theme.last_own_install {
                            self.theme.close_build_popovers(cx, root);
                            self.theme.on_external_install();
                            self.theme.worn = None;
                            if self.theme.confirm.take().is_some() { self.theme.msg.clear(); }
                        } else if self.theme.inflight_install == Some(req) {
                            if let Some(meta) = self.theme.installing.take() {
                                self.theme.eq_stands = self.theme.eq_open && !self.theme.eq_reanchor_pending && meta.source == InstallSource::Mix;
                                self.theme.tb_stands = self.theme.tb_open && !self.theme.tb_reanchor_pending && meta.source == InstallSource::Build;
                                self.theme.worn = meta.saved;
                            }
                            if let Some(saved) = &self.theme.worn { self.theme.name_text.clone_from(&saved.name); }
                        }
                        self.theme.refusals.clear();
                        if seat == session.me {
                            if let Some(next) = self.theme.install_settled(req) { self.send_install(session, next); }
                        }
                        if self.needs_theme_tick() { cx.new_next_frame(); }
                        if self.tab == PanelTab::Theme { self.theme.sync_header(cx, root); }
                    } else {
                        if own_edit { self.theme.refusals.remove(prop); }
                        if let Some(snap) = &mut self.theme.snapshot {
                            if snap.patch_token(prop, new) && new.starts_with('#') { self.theme.palette = snap.palette(); }
                        }
                    }
                } else if own_edit && self.sidebar.path.as_ref() == path {
                    self.sidebar.refusals.remove(prop);
                    // Keep just the acknowledged baseline and any newer local edit.
                    self.sidebar.layout_edits.retain(|id, (p, _)| id >= &req || p != prop);
                    self.sidebar.components.retain(|id, (p, _, _)| id >= &req || p != prop);
                }
                if seat == session.me { self.set_reply(session, req); self.settle_resize(cx, root, session); }
                let line = applied(*seq, seat, &who, path, prop, old, new);
                let widget = root.widget(cx, ids!(tweak_history));
                if let Some(mut list) = widget.borrow_mut::<TweakList>() {
                    if list.insert_history(line.clone(), false) {
                        self.pending.insert(*seq, line);
                        list.view.redraw(cx);
                    }
                }
                if path == "theme" || self.sidebar.path.as_ref() == path && !session.is_ended() {
                    self.mark_changed(cx, root);
                    self.refresh_after_edit(cx, root, session, path);
                }
                format!("{who} set {path}.{prop} to {new}")
            }
            Notice::Ended(why) => why.clone(),
            Notice::Refused(why) if seat == session.me => {
                if self.theme_refusal(cx, session, req, why).is_some() {
                    if self.tab == PanelTab::Theme { self.theme.sync_header(cx, root); }
                    return
                }
                if let Some((path, layer)) = self.proposals.remove(&req) {
                    if path == self.sidebar.path && layer == self.shader.active_layer {
                        self.shader.status.clone_from(why);
                        root.label(cx, ids!(shader_src_status)).set_text(cx, why);
                    }
                    return;
                }
                if let Some((path, prop)) = self.edits.remove(&req) {
                    if path.as_ref() == "theme" {
                        self.theme.refusals.insert(prop, why.clone());
                    } else if path == self.sidebar.path {
                        self.sidebar.layout_edits.remove(&req);
                        self.sidebar.components.remove(&req);
                        self.sidebar.refusals.insert(prop, why.clone());
                    }
                    if let Some(path) = self.set_reply(session, req) {
                        self.refresh_after_edit(cx, root, session, &path);
                    }
                    self.settle_resize(cx, root, session);
                    self.draw_rows(cx, root, false);
                    return
                }
                why.clone()
            }
            _ => return,
        };
        root.label(cx, ids!(tweak_result)).set_text(cx, &text);
    }
}

impl Line {
    fn history_run(&self) -> &HistoryRun {
        match self { Line::Edit(run) => run, _ => unreachable!("only edits in history") }
    }
}

impl TweakList {
    /// Keep individual sequences addressable when their signed records arrive.
    /// Only the runs touching the insertion can change. Their endpoints give
    /// the new slices directly, even when a late edit divides an existing run.
    fn insert_history(&mut self, entry: Rc<HistoryEntry>, signed: bool) -> bool {
        let entries = &mut self.history;
        let lines = &mut self.rows;
        let (at, replacing) = match entries.binary_search_by_key(&entry.key(), |e| e.key()) {
            Ok(at) if !signed || entries[at] == entry => return false,
            Ok(at) => (at, true),
            Err(at) => (at, false),
        };
        let run_at = |i: usize| lines.partition_point(|l| l.history_run().last.key() < entries[i].key());
        let first = at.checked_sub(1).map_or(0, run_at);
        let next = at + usize::from(replacing);
        let end = if next < entries.len() { run_at(next) + 1 } else { lines.len() };
        let mut start = lines.get(first).map_or(at, |l| entries.partition_point(|e| e.key() < l.history_run().first.key()));
        let stop = if first == end { at } else {
            entries.partition_point(|e| e.key() <= lines[end - 1].history_run().last.key())
        } + usize::from(!replacing);

        if replacing { entries[at] = entry; } else { entries.insert(at, entry); }
        let mut runs = Vec::new();
        if start < at && !entries[at - 1].continues(&entries[at]) {
            runs.push(HistoryRun::line(&entries[start..at]));
            start = at;
        }
        if at + 1 < stop && !entries[at].continues(&entries[at + 1]) {
            runs.push(HistoryRun::line(&entries[start..=at]));
            start = at + 1;
        }
        runs.push(HistoryRun::line(&entries[start..stop]));
        lines.splice(first..end, runs);
        true
    }
}

fn applied(seq: u64, seat: Seat, who: &str, path: &str, prop: &str, old: &str, new: &str) -> Rc<HistoryEntry> {
    Rc::new(HistoryEntry::Applied { seq, seat, who: who.into(),
        edit: AppliedEdit { path: path.into(), prop: prop.into(), old: old.into(), new: new.into() } })
}

/// Attribution and values come from the owner's signed create. Undo and visibility come from the fold.
fn recorded<L, T>(model: &Model, session: &Session<L, T>, entity: &Entity) -> Option<Rc<HistoryEntry>> {
    if entity.kind != kind::APP_EDIT || entity.author != session.sharer { return None }
    let event = model.event(&entity.id)?;
    let Content::Entity { fields, .. } = Content::decode(event.header.kind, event.header.kind_version, event.content()?)? else { return None };
    let get = |key| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| v);
    if get(field::PARENT) != Some(&Value::Id(session.item)) { return None }
    let text = |key| if let Some(Value::Text(v)) = get(key) { Some(v.as_str()) } else { None };
    let Value::Key(seat) = get(field::EDIT_SEAT)? else { return None };
    let agent = match get(field::EDIT_AGENT) { Some(Value::Id(id)) => Some(*id), _ => None };
    let seat = seat.as_slice().try_into().ok()?;
    let mut who = who(&model.state, &seat, agent);
    if agent.is_none() && get(field::EDIT_AI) == Some(&Value::Bool(true)) { who.push_str("'s AI"); }
    let path = text(field::WIDGET_PATH)?;
    let prop = text(field::PROPERTY)?;
    let key = HistoryKey::Note(entity.created, entity.id);
    match text(field::LABEL)? {
        "applied" => Some(applied(text(field::EDIT_SEQ)?.parse().ok()?, seat, &who, path, prop, text(field::OLD_VALUE)?, text(field::NEW_VALUE)?)),
        "note" => Some(Rc::new(HistoryEntry::Note { key, text: format!("Note  {who}  {path}.{prop}  {}", text(field::TEXT)?) })),
        "sidebar" => Some(Rc::new(HistoryEntry::Note { key, text: format!("Sidebar  {who}  {}", text(field::TEXT)?) })),
        _ => None,
    }
}

impl Panel {
    /// Supply the signed AppEdit id on receipt. Open, undo/redo and profile changes refresh the snapshot.
    pub fn sync_history<L, T>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &Session<L, T>, model: &Model, created: Option<Id>) {
        self.bind(cx, root, session);
        let widget = root.widget(cx, ids!(tweak_history));
        let mut entry = |entity: &Entity| {
            let row = recorded(model, session, entity)?;
            if let HistoryKey::Applied(seq) = row.key() { self.pending.remove(&seq); }
            (!entity.flag(field::DELETED) && !entity.flag(field::ARCHIVED)).then_some(row)
        };
        if let Some(id) = created {
            let Some(row) = model.state.entities.get(&id).and_then(entry) else { return };
            if let Some(mut list) = widget.borrow_mut::<TweakList>() {
                if !list.insert_history(row, true) { return }
                list.view.redraw(cx);
            }
        } else {
            let mut history = BTreeMap::new();
            for entity in model.state.entities.values() {
                if let Some(row) = entry(entity) { history.insert(row.key(), row); }
            }
            for row in self.pending.values() { history.insert(row.key(), row.clone()); }
            let entries: Vec<_> = history.into_values().collect();
            let lines = entries.chunk_by(|a, b| a.continues(b)).map(HistoryRun::line).collect();
            if let Some(mut list) = widget.borrow_mut::<TweakList>() { list.history = entries; }
            replace(cx, widget, lines, false);
        }
        self.mark_changed(cx, root);
    }
}

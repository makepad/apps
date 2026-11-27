//! Property inspector for one App item. The share view owns its picture.
use super::props::layout::view::{set_face, LayoutControls};
use super::shader::{ShaderTab, view::ShaderPills};
use super::props::live::LiveSets;
use super::overlay::{seat_colour, who};
use super::props::{AppliedEdit, Props, view::{PropLine, Sidebar, SidebarAction}};
use super::tree::{TreeAction, TreeLine, TreeState};
use super::theme::{ThemePalette, ThemeSnapshot};
use super::{ThemeOp, Notice, Request, RequestId, Seat, Session, SessionId, TreeQuery};
use crate::model::Model;
use makepad_town_core::{entity::{field, kind, Entity}, event::{Content, Id, Value}, fold::State};
use makepad_widgets::*;
use std::{collections::{BTreeMap, BTreeSet}, rc::Rc};

fn props_list(vm: &mut ScriptVm) -> ScriptValue {
    let row = TweakRow::script_proto(vm);
    super::props::view::sidebar_value(vm, row)
}

fn tree_list(vm: &mut ScriptVm) -> ScriptValue {
    let row = TweakRow::script_proto(vm);
    super::tree::view::tree_list_value(vm, row)
}

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    let TweakList = #(TweakList::register_widget(vm))
    let TweakRow = #(TweakRow::register_widget(vm))
    let PropsList = #(props_list(vm))
    let TreeList = #(tree_list(vm))
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
        TweakTabs{width: Fill height: Fit flow: Right{wrap: true} spacing: 3 align: Align{y: 0.5}
            tab_props := PanelTabButton{text: "Props"}
            tab_props_i := PanelTabButton{visible: false text: "" a11y: {label: "Props"} icon_walk: Walk{width: 13 height: Fit}
                draw_icon +: {color: #xd0d0d0 svg: crate_resource("makepad_widgets:resources/icons/tab_list.svg")}}
            tab_shader := PanelTabButton{text: "Shader"}
            tab_shader_i := PanelTabButton{visible: false text: "" a11y: {label: "Shader"} icon_walk: Walk{width: 13 height: Fit}
                draw_icon +: {color: #xd0d0d0 svg: crate_resource("makepad_widgets:resources/icons/tab_brush.svg")}}
            tab_tree := PanelTabButton{text: "Tree"}
            tab_tree_i := PanelTabButton{visible: false text: "" a11y: {label: "Tree"} icon_walk: Walk{width: 13 height: Fit}
                draw_icon +: {color: #xd0d0d0 svg: crate_resource("makepad_widgets:resources/icons/tab_tree.svg")}}
            tweak_close := PanelButton{text: "Close" a11y: {label: "Close tweak panel"}}
        }
        tweak_ask := PanelButton{text: "Ask to edit"}
        tweak_seats := TweakList{
            width: Fill height: 40
            tweak_seats_list := PortalList{width: Fill height: Fill
                Row := TweakRow{width: Fill height: Fit padding: 2
                    RoundedView{width: Fill height: Fit padding: 4 spacing: 5 align: Align{y: 0.5}
                        draw_bg +: {color: palette.panel2 border_radius: 5}
                        tweak_seat_name := RailText{width: Fill flow: Right{wrap: true}}
                        tweak_status := Kicker{width: Fit}
                        tweak_allow := PanelButton{text: "Allow"}
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
                View{width: Fill height: Fit flow: Right spacing: 4 align: Align{y: 0.5}
                    tree_filter := FabSearch{input +: {empty_text: "Filter tree"}}
                    tree_refresh := PanelTabButton{text: "↻" a11y: {label: "Refresh tree"}}
                }
                tree_status := BodyNote{visible: false width: Fill flow: Right{wrap: true}}
            }
            tweak_tree := TweakList{width: Fill height: Fill
                tweak_tree_list := TreeList{}
            }
        }
        tweak_result := BodyNote{}
        Kicker{text: "EDIT HISTORY"}
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
enum ChipAction { Allow, Revoke, Stop(Id), Resume(Id) }

#[derive(PartialEq)]
enum Line {
    Property(PropLine),
    Tree(TreeLine),
    Chip { seat: Seat, item: Id, name: String, ink: Vec4f, status: &'static str, action: Option<ChipAction> },
    Edit(HistoryRun),
}

impl Line {
    fn bind(&self, cx: &mut Cx, row: &View, previous: Option<&Line>, controls: &mut LayoutControls) {
        match self {
            Self::Property(p) => p.draw_sidebar(cx, row, match previous { Some(Self::Property(old)) => Some(old), _ => None }, controls),
            Self::Tree(line) => line.draw_tree(cx, row),
            Self::Chip { name, ink, status, action, .. } => {
                let label = row.label(cx, ids!(tweak_seat_name));
                label.set_text(cx, name);
                label.set_text_color(cx, *ink);
                row.label(cx, ids!(tweak_status)).set_text(cx, status);
                row.widget(cx, ids!(tweak_status)).set_visible(cx, !status.is_empty());
                row.widget(cx, ids!(tweak_allow)).set_visible(cx, matches!(action, Some(ChipAction::Allow | ChipAction::Resume(_))));
                row.widget(cx, ids!(tweak_revoke)).set_visible(cx, *action == Some(ChipAction::Revoke));
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
}

impl Widget for TweakRow {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) { self.view.handle_event(cx, event, scope); }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep { self.view.draw_walk(cx, scope, walk) }
}

/// One seat chip's height in the seats list.
const SEAT_ROW: f64 = 40.0;

#[derive(Script, ScriptHook, Widget)]
struct TweakList {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] rows: Vec<Rc<Line>>,
    #[rust] history: Vec<Rc<HistoryEntry>>,
}

impl Widget for TweakList {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) { self.view.handle_event(cx, event, scope); }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(cx, 0, self.rows.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some(line) = self.rows.get(i) else { continue };
                let template = match line.as_ref() { Line::Property(p) => p.template(), Line::Tree(t) => t.template(), _ => id!(Row) };
                let (item, existed) = list.item_with_existed(cx, i, template);
                if let Some(mut row) = item.borrow_mut::<TweakRow>() {
                    if !existed || !row.shown.as_ref().is_some_and(|old| Rc::ptr_eq(old, line)) {
                        let mut controls = LayoutControls::default();
                        line.bind(cx, &row.view, row.shown.as_deref(), &mut controls);
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
}

impl Widget for TweakTabs {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let room = cx.turtle().max_width(walk);
        if let Some(width) = room.filter(|_| self.room != room) {
            let visible = width_override::WidthOverride { visible: Some(true), ..Default::default() };
            let words = [ids!(tab_props), ids!(tab_shader), ids!(tab_tree), ids!(tweak_close)];
            let widths: Option<f64> = words.into_iter().map(|id| self.view.widget(cx, id).measure_width(cx, Some(&visible))).sum();
            if let Some(widths) = widths {
                let narrow = widths + (words.len() - 1) as f64 * self.view.layout.spacing > width;
                for (word, icon) in [(ids!(tab_props), ids!(tab_props_i)), (ids!(tab_shader), ids!(tab_shader_i)),
                    (ids!(tab_tree), ids!(tab_tree_i))] {
                    self.view.widget(cx, word).set_visible(cx, !narrow);
                    self.view.widget(cx, icon).set_visible(cx, narrow);
                }
                self.room = room;
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PanelTab { #[default] Props, Shader, Tree }

#[derive(Default)]
pub struct Panel {
    bound: Option<(SessionId, Id)>,
    last_selection: Option<RequestId>,
    props: Option<(RequestId, String)>,
    sidebar: Sidebar,
    theme_snap: Option<ThemeSnapshot>,
    palette: ThemePalette,
    theme_requested: bool,
    theme_req: Option<RequestId>,
    tab: PanelTab,
    shader: ShaderTab,
    tree: TreeState,
    proposals: BTreeMap<RequestId, (Rc<str>, String)>,
    edits: BTreeMap<RequestId, (Rc<str>, String)>,
    live: LiveSets,
    // Notices can beat signed-log replication. Keep those rows until their creates arrive.
    pending: BTreeMap<u64, Rc<HistoryEntry>>,
}

/// Keep optimistic composite fields ahead of the last Rows snapshot only
/// for the selected path. A queued Set can be released after selection moves.
fn send_set<L: Copy + Eq, T: Copy + Eq>(session: &mut Session<L, T>, sidebar: &mut Sidebar,
    edits: &mut BTreeMap<RequestId, (Rc<str>, String)>, path: &Rc<str>, prop: &str, value: &str) -> Option<RequestId> {
    let selected = *path == sidebar.path;
    let request = Request::Set { path: path.to_string(), prop: prop.into(), value: value.into() };
    match session.request(None, request) {
        Ok(req) => {
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
            if selected { sidebar.refusals.insert(prop.into(), why); }
            None
        }
    }
}

impl Panel {
    fn bind<L, T>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &Session<L, T>) {
        let key = (session.share, session.item);
        if self.bound == Some(key) { return }
        *self = Self { bound: Some(key), ..Default::default() };
        for list in [ids!(tweak_seats), ids!(tweak_properties), ids!(tweak_shader_rows), ids!(tweak_tree), ids!(tweak_history)] {
            replace(cx, root.widget(cx, list), Vec::new(), true);
        }
        root.text_input(cx, ids!(tree_filter.input)).set_text(cx, "");
        root.label(cx, ids!(tree_status)).set_text(cx, "");
        root.widget(cx, ids!(tree_status)).set_visible(cx, false);
        self.show_tab(cx, root);
        root.widget(cx, ids!(tweak_hint)).set_visible(cx, true);
        root.label(cx, ids!(tweak_result)).set_text(cx, "");
    }

    pub fn refresh<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &Session<L, T>, state: &State, clock: &crate::view::LocalClock) {
        self.bind(cx, root, session);
        if self.theme_snap.is_some() && !session.supports_theme() {
            self.theme_snap = None;
            self.palette = ThemePalette::default();
            self.theme_requested = false;
            if self.tab != PanelTab::Tree { self.draw_rows(cx, root, false); }
        }
        if self.tree.sync_seats(&session.view, state) && self.tab == PanelTab::Tree { self.draw_rows(cx, root, false); }
        let owner = session.me == session.sharer;
        root.widget(cx, ids!(tweak_ask)).set_visible(cx, !session.is_ended() && !owner && !session.view.allowed.contains(&session.me));
        let view = &session.view;
        let mut actors: BTreeSet<_> = view.asked.union(&view.allowed).map(|&seat| (seat, None)).collect();
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
                None if view.allowed.contains(&seat) => ("Allowed", Some(ChipAction::Revoke)),
                None if view.asked.contains(&seat) => ("Asked", Some(ChipAction::Allow)),
                None => ("", None),
            };
            let action = if owner && !session.is_ended() && (agent.is_some() || seat != session.sharer) { action } else { None };
            Rc::new(Line::Chip { seat, item: session.item, name, ink: seat_colour(state, &seat), status, action })
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
        for (_, row) in root.portal_list(cx, ids!(tweak_seats_list)).items_with_actions(actions) {
            // Use the binding actually painted under the press, even if the roster has since changed.
            let Some(shown) = row.borrow::<TweakRow>().and_then(|r| r.shown.clone()) else { continue };
            let Line::Chip { seat, item, action: Some(action), .. } = &*shown else { continue };
            if *item != session.item || session.me != session.sharer { continue }
            let (button, current) = match *action {
                ChipAction::Allow => (ids!(tweak_allow), session.view.asked.contains(seat)),
                ChipAction::Revoke => (ids!(tweak_revoke), session.view.allowed.contains(seat)),
                ChipAction::Stop(id) => (ids!(tweak_stop), session.view.agents.get(&(*seat, id)) == Some(&false)),
                ChipAction::Resume(id) => (ids!(tweak_allow), session.view.agents.get(&(*seat, id)) == Some(&true)),
            };
            if !current || !row.button(cx, button).clicked(actions) { continue }
            if !granted(cx) { return }
            let result = match *action {
                ChipAction::Allow => session.consent(*seat, true),
                ChipAction::Revoke => session.consent(*seat, false),
                ChipAction::Stop(id) => session.stop_agent(*seat, id, true),
                ChipAction::Resume(id) => session.stop_agent(*seat, id, false),
            };
            if let Err(why) = result { root.label(cx, ids!(tweak_result)).set_text(cx, why); }
            break
        }
    }

    pub fn actions<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, actions: &Actions, session: &mut Session<L, T>) {
        if !root.visible() || self.bound != Some((session.share, session.item)) { return }
        if root.button(cx, ids!(tweak_ask)).clicked(actions) {
            if let Err(why) = session.request(None, Request::AskEdit) {
                root.label(cx, ids!(tweak_result)).set_text(cx, &why);
            }
            return
        }
        let tab = if root.button(cx, ids!(tab_props)).clicked(actions) || root.button(cx, ids!(tab_props_i)).clicked(actions) { PanelTab::Props }
            else if root.button(cx, ids!(tab_shader)).clicked(actions) || root.button(cx, ids!(tab_shader_i)).clicked(actions) { PanelTab::Shader }
            else if root.button(cx, ids!(tab_tree)).clicked(actions) || root.button(cx, ids!(tab_tree_i)).clicked(actions) { PanelTab::Tree }
            else { self.tab };
        if tab != self.tab {
            self.tree.cancel_reveal();
            self.tab = tab;
            if tab == PanelTab::Tree {
                let ready = self.tree.loaded || self.tree.inflight_root.is_some() || self.send_tree(session, TreeState::root());
                if ready && !self.sidebar.path.is_empty() {
                    self.follow_selection(cx, root, session, &self.sidebar.path.clone(), true);
                }
            }
            self.show_tab(cx, root);
            self.draw_rows(cx, root, false);
            return;
        }
        let (properties, list) = match self.tab {
            PanelTab::Tree => {
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

    fn show_tab(&self, cx: &mut Cx, root: &WidgetRef) {
        let props = self.tab == PanelTab::Props;
        root.widget(cx, ids!(tweak_props_wrap)).set_visible(cx, props);
        let shader = self.tab == PanelTab::Shader;
        let tree = self.tab == PanelTab::Tree;
        root.widget(cx, ids!(tweak_shader_wrap)).set_visible(cx, shader);
        root.widget(cx, ids!(tweak_tree_wrap)).set_visible(cx, tree);
        root.widget(cx, ids!(tweak_hint)).set_visible(cx, self.sidebar.path.is_empty() && !tree);
        for (id, active) in [(ids!(tab_props), props), (ids!(tab_props_i), props),
            (ids!(tab_shader), shader), (ids!(tab_shader_i), shader), (ids!(tab_tree), tree), (ids!(tab_tree_i), tree)] {
            set_tab_fill(cx, root.widget(cx, id), active);
        }
    }

    fn send_tree<L: Copy + Eq, T: Copy + Eq>(&mut self, session: &mut Session<L, T>, query: TreeQuery) -> bool {
        match session.request(None, Request::Tree(query.clone())) {
            Ok(req) => { self.tree.mark_sent(req, &query); true }
            Err(why) => { self.tree.mark_send_failed(&query, &why); false }
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
            TreeAction::Select(path) => {
                if let Err(why) = session.request(None, Request::Select(Some(path.to_string()))) {
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
                if let PropLine::Header { open: Some(_), .. } = line {
                    if item.view(cx, ids!(header)).finger_up(actions).is_some_and(|tap| tap.is_over && tap.was_tap()) {
                        self.shader.consts_open = !self.shader.consts_open;
                        redraw = true;
                    }
                    continue;
                }
                let Some(row) = item.borrow::<TweakRow>() else { continue };
                let Some(action) = self.sidebar.handle_sidebar_actions(cx, &item, line, actions, &row.controls, &self.live) else { continue };
                match action {
                    SidebarAction::Redraw => redraw = true,
                    SidebarAction::PaletteOpened(uid) => {
                        let swatch = item.widget(cx, ids!(swatch));
                        if swatch.widget_uid() == uid {
                            if let Some(mut pick) = swatch.borrow_mut::<fab_controls::FabColorPick>() {
                                pick.set_palette(cx, self.palette.entries());
                            }
                        }
                    }
                    SidebarAction::Sets { path, edits, phase } => {
                        for (prop, value) in edits.iter() {
                            let finished = self.live.offer(&path, &prop, value, phase, |path, prop, value| {
                                let had_refusal = self.sidebar.refusals.contains_key(prop);
                                let sent = send_set(session, &mut self.sidebar, &mut self.edits, path, prop, value);
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
            PanelTab::Tree => {
                let (lines, scroll_index) = self.tree.lines(&self.sidebar.path);
                let status = root.label(cx, ids!(tree_status));
                status.set_text(cx, &self.tree.status);
                let status_in_list = matches!(lines.as_slice(), [TreeLine::Info { text, .. }] if text == &self.tree.status);
                status.set_visible(cx, !self.tree.status.is_empty() && !status_in_list);
                let widget = root.widget(cx, ids!(tweak_tree));
                let rows = {
                    let Some(list) = widget.borrow::<TweakList>() else { return };
                    lines.into_iter().enumerate().map(|(i, line)| {
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
            PanelTab::Props => (ids!(tweak_properties), self.sidebar.lines(&self.palette)),
            PanelTab::Shader => {
                let layer = self.shader.sync(cx, root, &self.sidebar);
                let lines = layer.map_or_else(Vec::new, |l| self.shader.lines(&self.sidebar, l, &self.palette));
                (ids!(tweak_shader_rows), lines)
            }
        };
        replace(cx, root.widget(cx, widget), lines.into_iter().map(|p| Rc::new(Line::Property(p))).collect(), reset_scroll);
    }

    fn mark_changed(&mut self, cx: &mut Cx, root: &WidgetRef) {
        if let Some(list) = root.widget(cx, ids!(tweak_history)).borrow::<TweakList>() {
            self.sidebar.props.mark_changed(&self.sidebar.path, list.rows.iter().flat_map(|line| {
                let run = line.history_run();
                run.first.edit().into_iter().chain(run.last.edit())
            }));
        }
        if self.tab != PanelTab::Tree { self.draw_rows(cx, root, false); }
    }

    fn request_props<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &mut Session<L,T>,
        agent: Option<Id>, path: &str) {
        match session.request(agent, Request::Props { path: path.to_owned() }) {
            Ok(id) => self.props = Some((id, path.to_owned())),
            Err(why) => root.label(cx, ids!(tweak_result)).set_text(cx, &why),
        }
    }

    fn refresh_after_edit<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef,
        session: &mut Session<L, T>, path: &str) {
        if self.sidebar.path.as_ref() == path && !self.live.open_on(path) && !session.is_ended()
            && session.may_act(&session.me, None, &Request::Props { path: path.into() }).is_ok() {
            self.request_props(cx, root, session, None, path);
        }
    }

    fn set_reply<L: Copy + Eq, T: Copy + Eq>(&mut self, session: &mut Session<L, T>, req: RequestId) -> Option<Rc<str>> {
        self.live.reply(req, |path, prop, value| send_set(session, &mut self.sidebar, &mut self.edits, path, prop, value))
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
        root.widget(cx, ids!(tweak_hint)).set_visible(cx, path.is_empty() && self.tab != PanelTab::Tree);
        if let Some(ty) = ty { self.sidebar.ty = ty.to_owned(); }
        self.sidebar.rect = rect;
        self.props = None;
        self.draw_rows(cx, root, changed);
    }

    fn ensure_theme<L: Copy + Eq, T: Copy + Eq>(&mut self, session: &mut Session<L, T>) {
        if self.theme_requested || self.theme_snap.is_some() || self.theme_req.is_some() || session.is_ended() { return }
        let get = Request::Theme(ThemeOp::Get);
        if session.may_act(&session.me, None, &get).is_err() { return }
        self.theme_requested = true;
        self.theme_req = session.request(None, get).ok();
    }

    fn theme_refusal(&mut self, req: RequestId) -> Option<()> {
        self.theme_req.take_if(|pending| *pending == req).map(|_| ())
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

    pub fn notice<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef, session: &mut Session<L, T>,
        seat: Seat, req: RequestId, agent: Option<Id>, state: &State, notice: &Notice) {
        self.bind(cx, root, session);
        let dots_changed = self.tree.sync_seats(&session.view, state);
        let selects = seat == session.me && match notice {
            Notice::Picked { .. } => self.last_selection.is_none_or(|last| req > last),
            Notice::Selected { .. } => self.last_selection.is_none_or(|last| req >= last),
            _ => false,
        };
        if dots_changed && self.tab == PanelTab::Tree && !selects {
            self.draw_rows(cx, root, false);
        }
        let who = who(state, &seat, agent);
        let text = match notice {
            Notice::Asked => format!("{who} asked to edit"),
            Notice::Allowed(true) => {
                if seat == session.me { self.ensure_theme(session); }
                format!("{who} can edit")
            }
            Notice::Allowed(false) => {
                if seat == session.me { self.theme_requested = false; }
                format!("Editing permission revoked for {who}")
            }
            Notice::Theme { theme } => {
                if let Some(snapshot) = ThemeSnapshot::parse(theme) {
                    self.palette = snapshot.palette();
                    self.theme_snap = Some(snapshot);
                    self.theme_requested = true;
                    self.theme_req = None;
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
                if selects {
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
            Notice::Tree { query, tree } if seat == session.me => {
                if let Some(next) = self.tree.apply_reply(req, query, tree) { self.send_tree(session, next); }
                if self.tab == PanelTab::Tree { self.draw_rows(cx, root, false); }
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
                if path == "theme" && prop != "install" {
                    if let Some(snap) = &mut self.theme_snap {
                        if snap.patch_token(prop, new) {
                            self.palette = snap.palette();
                            if self.tab != PanelTab::Tree { self.draw_rows(cx, root, false); }
                        }
                    }
                }
                if seat == session.me && self.edits.remove(&req).is_some() && self.sidebar.path.as_ref() == path {
                    self.sidebar.refusals.remove(prop);
                    // Keep just the acknowledged baseline and any newer local edit.
                    self.sidebar.layout_edits.retain(|id, (p, _)| id >= &req || p != prop);
                    self.sidebar.components.retain(|id, (p, _, _)| id >= &req || p != prop);
                }
                if seat == session.me { self.set_reply(session, req); }
                let line = applied(*seq, seat, &who, path, prop, old, new);
                let widget = root.widget(cx, ids!(tweak_history));
                if let Some(mut list) = widget.borrow_mut::<TweakList>() {
                    if list.insert_history(line.clone(), false) {
                        self.pending.insert(*seq, line);
                        list.view.redraw(cx);
                    }
                }
                if self.sidebar.path.as_ref() == path && !session.is_ended() {
                    self.mark_changed(cx, root);
                    self.refresh_after_edit(cx, root, session, path);
                }
                format!("{who} set {path}.{prop} to {new}")
            }
            Notice::Ended(why) => why.clone(),
            Notice::Refused(why) if seat == session.me => {
                if self.theme_refusal(req).is_some() { return }
                if let Some((path, layer)) = self.proposals.remove(&req) {
                    if path == self.sidebar.path && layer == self.shader.active_layer {
                        self.shader.status.clone_from(why);
                        root.label(cx, ids!(shader_src_status)).set_text(cx, why);
                    }
                    return;
                }
                if let Some((path, prop)) = self.edits.remove(&req) {
                    if path == self.sidebar.path {
                        self.sidebar.layout_edits.remove(&req);
                        self.sidebar.components.remove(&req);
                        self.sidebar.refusals.insert(prop, why.clone());
                    }
                    if let Some(path) = self.set_reply(session, req) {
                        self.refresh_after_edit(cx, root, session, &path);
                    }
                    self.draw_rows(cx, root, false);
                    return
                }
                if let Some(next) = self.tree.apply_refusal(req, why) {
                    if let Some(next) = next { self.send_tree(session, next); }
                    if self.tab == PanelTab::Tree { self.draw_rows(cx, root, false); }
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

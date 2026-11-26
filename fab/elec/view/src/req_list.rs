//! The project's requirements, in document order. Text is prepared once per judged report.
//! Only the visible fixed-height rows have widgets. Evidence is resolved against the current
//! snapshot when the person points at a row.

use crate::{ElecLive, ReqShown, ViewAction};
use fab_elec_circuit::Violation;
use fab_elec_core::Sel;
use fab_elec_design::{Design, Focus, FocusKind, FocusOrigin};
use fab_elec_req::{Focus as Evidence, Report, Row, Status};
use makepad_widgets_core::tip::{TipAction, TipRequest, TipWidgetExt};
use makepad_widgets_core::*;
use std::sync::Arc;

const ROW_HEIGHT: f64 = 32.0;
const EMPTY: &str = "No requirements yet";

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    let Line = Label{
        padding: 0 align: Align{y: 0.5}
        max_lines: 1 text_overflow: Ellipsis
        draw_text +: {text_style: theme.font_regular{font_size: 9}}
    }
    let Press = Button{
        height: 23 padding: Inset{left: 5 right: 5}
        draw_text +: {text_style: theme.font_regular{font_size: 9}}
    }
    let Pick = DropDown{width: Fill height: 24}
    let Field = TextInput{width: Fill height: 24 is_multiline: false}
    let Item = #(ReqItem::register_widget(vm))
    mod.widgets.ReqListBase = #(ReqList::register_widget(vm))
    mod.widgets.ReqList = set_type_default() do mod.widgets.ReqListBase{
        width: Fill height: Fill flow: Down padding: 6
        a11y: {role: "group" label: "Requirements"}
        // Fab supplies its palette on the panel. Standalone board tools load this module too.
        color_ok: theme.color_success color_error: theme.color_error
        color_warning: theme.color_warning color_text_muted: theme.color_label_inner_inactive
        color_text_dim: theme.color_label_inner_inactive
        heading := View{
            width: Fill height: 26 flow: Right align: Align{y: 0.5}
            header := Line{width: Fill height: Fill text: #(EMPTY) draw_text +: {text_style: theme.font_bold{font_size: 9}}}
            Tip{text: "Add a requirement" open_add := Press{text: "Add" a11y_name: "Add a requirement"}}
        }
        form := View{
            width: Fill height: Fit flow: Down spacing: 3 visible: false
            Tip{text: "Choose what Fab measures" template := Pick{a11y: {label: "Measure template"}}}
            Tip{text: "Choose the checks to count" family := Pick{a11y: {label: "Check family"}}}
            Tip{text: "Choose the board unit" unit := Pick{a11y: {label: "Board unit"}}}
            only_unit := Line{width: Fill height: 18}
            Tip{text: "Name the bench to check" bench := Field{empty_text: "Bench name" a11y: {label: "Bench name"}}}
            Tip{text: "Name the expectation to check" expect := Field{empty_text: "Expectation name" a11y: {label: "Expectation name"}}}
            Tip{text: "Name this requirement in your own words" req_name := Field{empty_text: "Requirement name" a11y: {label: "Requirement name"}}}
            Tip{text: "Enter a maximum, or a Splash bound using min, max or is" bound := Field{a11y: {label: "Bound"}}}
            View{width: Fill height: Fit flow: Right{wrap: true} spacing: 4
                Tip{text: "Add this requirement" submit := Press{text: "Add" a11y_name: "Add this requirement"}}
                Tip{text: "Close the Add form" cancel := Press{text: "Cancel"}}
            }
            refusal := Label{width: Fill height: Fit visible: false a11y: {live: @Polite} draw_text +: {color: theme.color_error}}
        }
        mistakes_tip := Tip{
            wrap_width: 440
            mistakes := Line{width: Fill height: 16 visible: false hover_actions_enabled: true}
        }
        rows := FlatList{
            width: Fill height: Fill flow: Down spacing: 0 drag_scrolling: false
            scroll_bars +: {show_scroll_x: false show_scroll_y: true}
            Row := Item{
                width: Fill height: #(ROW_HEIGHT) flow: Right spacing: 3 align: Align{y: 0.5}
                padding: Inset{top: 3 bottom: 3 left: 6 right: 6}
                cursor: MouseCursor.Hand grab_key_focus: true
                show_bg: true
                draw_bg +: {
                    hover: instance(0.0)
                    get_color: fn() { return mix(theme.color_u_hidden, theme.color_outset_hover, self.hover) }
                }
                animator: Animator{hover: {
                    default: @off
                    off: AnimatorState{from: {all: Snap} apply: {draw_bg: {hover: 0.0}}}
                    on: AnimatorState{from: {all: Snap} apply: {draw_bg: {hover: 1.0}}}
                }}
                summary := View{
                    width: Fill height: Fill flow: Down
                    first := View{
                        width: Fill height: 14 flow: Right spacing: 4 align: Align{y: 0.5}
                        dot := CircleView{width: 7 height: 7}
                        title := Line{width: Fill height: Fill draw_text +: {text_style: theme.font_bold{font_size: 9}}}
                        value := Line{width: Fit height: Fill align: Align{x: 1.0 y: 0.5}}
                    }
                    detail := Line{width: Fill height: 12 draw_text +: {text_style: theme.font_regular{font_size: 8}}}
                }
                waiver := View{width: Fill height: Fill flow: Right align: Align{y: 0.5} spacing: 4 visible: false
                    why := Line{width: Fill height: Fill a11y: {label_for: @reason}}
                    reason_tip := Tip{reason := Field{empty_text: "Reason (Enter to waive)"}}
                }
                Tip{text: "Measure: run the bench or open the Slicer" measure := Press{text: "Measure" visible: false}}
                Tip{text: "Waive: count it as met for your reason" waive := Press{text: "Waive"}}
                Tip{text: "Accept: waive it for the proposed reason" accept := Press{text: "Accept" visible: false}}
                Tip{text: "Decline: take the proposal out" decline := Press{text: "Decline" visible: false}}
                Tip{text: "Unwaive: judge this requirement again" unwaive := Press{text: "Unwaive" visible: false}}
                menu := Popover{
                    width: 25 height: 23 trigger: mod.widgets.Manual
                    Tip{text: "More requirement actions" more := Press{width: Fill text: "⋯" a11y_name: "More requirement actions" visible: false}}
                    content := View{width: Fit height: Fit
                        Tip{text: "Remove this requirement" remove := Press{text: "Remove"}}
                    }
                }
            }
        }
    }
}

/// A person's command from the Requirements editor. Bench selection is UI state; the catalog
/// action itself takes no arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReqAction {
    Run { action: &'static str, args: Vec<(String, String)>, bench: Option<String> },
    Slicer,
}

impl ReqAction {
    fn run(action: &'static str, args: Vec<(String, String)>) -> Self {
        Self::Run { action, args, bench: None }
    }
}

/// A refused Add, returned to the form waiting for that name.
#[derive(Clone, Debug)]
pub struct AddRefused {
    pub name: String,
    pub why: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Template {
    Board(&'static str), Checks(&'static str), Case, Bench, Expect, Print(&'static str),
}

const TEMPLATES: &[(&str, Template)] = &[
    ("Board size", Template::Board("size")), ("Copper layers", Template::Board("layers")),
    ("Placed parts", Template::Board("parts")), ("Unplaced parts", Template::Board("unplaced")),
    ("Unrouted connections", Template::Board("unrouted")), ("Check errors", Template::Checks("errors")),
    ("Check warnings", Template::Checks("warnings")), ("Case fits", Template::Case),
    ("Bench passes", Template::Bench), ("Bench expectation", Template::Expect),
    ("Print time", Template::Print("time")), ("Filament mass", Template::Print("filament")),
];

impl Template {
    fn has_bound(self) -> bool { matches!(self, Self::Board(_) | Self::Checks(_) | Self::Print(_)) }

    fn add(self, name: &str, unit: Option<&str>, family: Option<&str>, bench: &str, expect: &str, bound: &str) -> Result<ReqAction, String> {
        use makepad_widgets_core::makepad_script::syntax::write::quote;
        let board = || unit.map(quote).ok_or("Choose a board unit for this measure.");
        let measure = match self {
            Self::Board(field) => format!("board({}).{field}", board()?),
            Self::Case => format!("case({}).fits", board()?),
            Self::Checks(field) => {
                let family = family.ok_or("Choose the family of checks to count.")?;
                let scope = unit.map_or_else(String::new, |unit| format!(", {{unit: {}}}", quote(unit)));
                format!("checks(@{family}{scope}).{field}")
            }
            Self::Bench | Self::Expect => {
                if bench.trim().is_empty() { return Err("Enter a bench name.".into()); }
                let mut expr = format!("bench({})", quote(bench.trim()));
                if self == Self::Expect {
                    if expect.trim().is_empty() { return Err("Enter an expectation name.".into()); }
                    expr.push_str(&format!(".expect({})", quote(expect.trim())));
                }
                expr
            }
            Self::Print(field) => format!("print.{field}"),
        };
        let mut args = vec![("name".into(), name.trim().into()), ("measure".into(), measure)];
        if self.has_bound() {
            let bound = bound.trim();
            if bound.is_empty() { return Err("Enter the bound for this requirement.".into()); }
            args.push(("bound".into(), if bound.starts_with('{') { bound.into() } else { format!("{{max: {bound}}}") }));
        }
        Ok(ReqAction::run("elec.req.add", args))
    }
}

#[derive(Clone, Default, PartialEq)]
struct RowControls {
    name: String,
    waived: bool,
    /// A met row has nothing to waive: it offers no Waive.
    passing: bool,
    proposed: Option<String>,
    measure: Option<ReqAction>,
}

impl RowControls {
    fn new(row: &Row, runnable: impl FnOnce(&str) -> bool) -> Self {
        let measure = match (&row.status, &row.focus) {
            (Status::Unknown(_), Evidence::Bench(b)) if runnable(b) =>
                Some(ReqAction::Run { action: "elec.bench.run", args: Vec::new(), bench: Some(b.clone()) }),
            (Status::Unknown(_), Evidence::Print) => Some(ReqAction::Slicer),
            _ => None,
        };
        Self {
            name: row.name.clone(), waived: matches!(row.status, Status::Waived { .. }), passing: row.status == Status::Pass,
            proposed: row.proposed.clone(), measure,
        }
    }

    fn offers_waive(&self) -> bool { !self.waived && !self.passing && self.proposed.is_none() }

    fn accept(&self) -> Option<ReqAction> {
        self.proposed.as_deref().map(|reason| self.waiver(reason))
    }

    fn unwaive(&self) -> ReqAction {
        ReqAction::run("elec.req.unwaive", vec![("name".into(), self.name.clone())])
    }

    fn waiver(&self, reason: &str) -> ReqAction {
        let mut args = vec![("name".into(), self.name.clone())];
        args.push(("reason".into(), reason.into()));
        ReqAction::run("elec.req.waive", args)
    }

    fn remove(&self) -> ReqAction {
        ReqAction::run("elec.req.remove", vec![("name".into(), self.name.clone())])
    }
}

fn header(report: &Report) -> String {
    let (met, total) = report.met_count();
    let mut text = format!("{met} of {total} met");
    for (n, status) in [(report.fail, "failing"), (report.unknown, "unknown"), (report.waived, "waived"), (report.proposed, "proposed")] {
        if n > 0 {
            use std::fmt::Write;
            write!(text, ", {n} {status}").unwrap();
        }
    }
    text
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Ink { Ok, Error, Warning, Muted, Dim }

struct RowText {
    title: String,
    value: String,
    detail: String,
    accessible: String,
    dot: Ink,
    value_ink: Ink,
}

impl RowText {
    fn new(row: &Row, at: Option<&(Arc<str>, usize)>) -> Self {
        let mut detail = row.source.to_string();
        let dot = match &row.status {
            Status::Pass => Ink::Ok,
            Status::Fail => Ink::Error,
            Status::Unknown(why) => {
                detail.push_str(&format!(" · {why}"));
                if matches!(row.focus, Evidence::Case(_)) { detail.push_str(" · case checks run on each plate build"); }
                Ink::Warning
            }
            Status::Waived { reason, .. } => {
                detail.push_str(&format!(" · waived: {reason}"));
                Ink::Muted
            }
        };
        if let Some(reason) = &row.proposed {
            detail.push_str(&format!(" · proposed waiver: {reason}"));
        }
        if row.stale && !matches!(&row.status, Status::Unknown(why) if why.starts_with("stale:")) {
            detail.push_str(" · stale");
        }
        if let Some((doc, line)) = at {
            detail.push_str(&format!(" · {doc}:{line}"));
        }
        Self {
            title: row.name.clone(),
            value: row.value.as_ref().map_or_else(|| row.bound.clone(), |v| format!("{v}, {}", row.bound)),
            detail,
            accessible: format!("{}: {}", row.name, row.status.word()),
            dot,
            value_ink: if row.stale { Ink::Dim } else { dot },
        }
    }
}

fn evidence(row: &Row, violations: &[Violation], design: &Design) -> Option<Sel> {
    match &row.focus {
        Evidence::Findings => {
            // DesignSnapshot orders violations by id. Preserve the requirement's evidence order.
            let index = row.findings.iter().find_map(|id| violations.binary_search_by_key(id, |v| v.id).ok())?;
            violations[index].items.first().cloned()
        }
        Evidence::Unit(name) => design.unit_by_name(name).filter(|u| u.board.is_some()).map(|u| Sel::Unit(u.id)),
        Evidence::Case(_) | Evidence::Bench(_) | Evidence::Print => None,
    }
}

fn focus(row: &Row, double: bool, violations: &[Violation], design: &Design) -> Option<Focus> {
    let (kind, sel) = if double {
        (FocusKind::SourceRange, Sel::Source { doc: row.doc, start: row.span.start, end: row.span.end })
    } else {
        let sel = evidence(row, violations, design)?;
        let kind = match sel {
            Sel::Comp(_) => FocusKind::Part,
            Sel::Net(_) => FocusKind::Net,
            Sel::Pin(_) => FocusKind::Pin,
            _ => FocusKind::Board,
        };
        (kind, sel)
    };
    Some(Focus { kind, sel, origin: FocusOrigin::Checks })
}

#[derive(Script, ScriptHook, Widget)]
struct ReqItem {
    #[deref] view: View,
    #[rust] accessible: String,
    #[rust] controls: RowControls,
    #[rust] editing: bool,
    #[rust] focus_reason: bool,
    #[rust] over: bool,
}

impl ReqItem {
    fn edit_reason(&mut self, cx: &mut Cx, edit: bool) {
        self.editing = edit;
        self.focus_reason = edit;
        self.view.widget(cx, ids!(summary)).set_visible(cx, !edit);
        self.view.widget(cx, ids!(waiver)).set_visible(cx, edit);
        self.view.button(cx, ids!(waive)).set_visible(cx, !edit && self.controls.offers_waive());
        let proposal = !edit && !self.controls.waived && self.controls.proposed.is_some();
        self.view.button(cx, ids!(accept)).set_visible(cx, proposal);
        self.view.button(cx, ids!(decline)).set_visible(cx, proposal);
        self.view.button(cx, ids!(unwaive)).set_visible(cx, !edit && self.controls.waived);
        self.view.button(cx, ids!(measure)).set_visible(cx, !edit && self.controls.measure.is_some());
        if edit {
            let why = format!("Why {} is waived", self.controls.name);
            self.view.label(cx, ids!(why)).set_text(cx, &why);
            self.view.tip(cx, ids!(reason_tip)).set_text(&why);
            let reason = self.view.text_input(cx, ids!(reason));
            reason.set_text(cx, "");
        }
    }
}

impl Widget for ReqItem {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::ListItem;
        makepad_platform::a11y::set_string(&mut node.name, &self.accessible);
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let menu = self.view.popover(cx, ids!(menu));
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        let was_over = self.over;
        match event {
            Event::MouseMove(mouse) => self.over = self.area().clipped_rect(cx).contains(mouse.abs),
            Event::MouseLeave(_) => self.over = false,
            Event::MouseDown(mouse) if mouse.button.is_secondary() && cx.sweep_lock_area().is_none()
                && self.area().clipped_rect(cx).contains(mouse.abs) => {
                menu.open_at(cx, Rect { pos: mouse.abs, size: dvec2(0., 0.) });
            }
            _ => {}
        }
        if self.over != was_over || matches!(event, Event::KeyFocus(_)) || menu.opened(&actions) || menu.closed(&actions) {
            let focused = self.key_focus(cx) || [ids!(waive), ids!(unwaive), ids!(accept), ids!(decline), ids!(measure), ids!(reason), ids!(more)]
                .into_iter().any(|id| self.view.widget(cx, id).key_focus(cx));
            self.view.widget(cx, ids!(more)).set_visible(cx, self.over || focused || menu.is_open());
        }
        if !actions.is_empty() {
            let reason = self.view.text_input(cx, ids!(reason));
            let command = if menu.content().button(cx, ids!(remove)).clicked(&actions) {
                menu.close(cx);
                Some(self.controls.remove())
            } else if self.view.button(cx, ids!(more)).clicked(&actions) {
                menu.toggle(cx);
                None
            } else if self.view.button(cx, ids!(waive)).clicked(&actions) {
                self.edit_reason(cx, true);
                None
            } else if self.editing && reason.escaped(&actions) {
                self.edit_reason(cx, false);
                None
            } else if self.editing {
                reason.returned(&actions).filter(|(reason, _)| !reason.trim().is_empty())
                    .map(|(reason, _)| self.controls.waiver(&reason))
            } else if self.view.button(cx, ids!(accept)).clicked(&actions) {
                self.controls.accept()
            } else if self.view.button(cx, ids!(decline)).clicked(&actions) || self.view.button(cx, ids!(unwaive)).clicked(&actions) {
                Some(self.controls.unwaive())
            } else if self.view.button(cx, ids!(measure)).clicked(&actions) {
                self.controls.measure.clone()
            } else { None };
            if let Some(command) = command { cx.widget_action(self.widget_uid(), command); }
        }
        cx.extend_actions(actions);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let step = self.view.draw_walk(cx, scope, walk);
        if step.is_done() && std::mem::take(&mut self.focus_reason) {
            self.view.text_input(cx, ids!(reason)).take_key_focus_from_user(cx);
        }
        step
    }
}

#[derive(Script, Widget)]
pub struct ReqList {
    #[deref] view: View,
    #[live] color_ok: Vec4f,
    #[live] color_error: Vec4f,
    #[live] color_warning: Vec4f,
    #[live] color_text_muted: Vec4f,
    #[live] color_text_dim: Vec4f,
    #[rust] shown: Option<Arc<ReqShown>>,
    #[rust] rows: Vec<(RowText, RowControls)>,
    #[rust] adding: bool,
    #[rust] pending: Option<String>,
    #[rust] units: Vec<String>,
    #[rust] design: std::sync::Weak<Design>,
    #[rust] refresh_header: bool,
    /// The row whose detail tip is up, and whether it also highlights evidence in another view.
    #[rust] hovered: Option<(LiveId, bool)>,
}

impl ScriptHook for ReqList {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, _apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        self.refresh_header = true;
    }
}

impl ReqList {
    fn template(&self, cx: &mut Cx) -> Option<Template> {
        let at = self.view.drop_down(cx, ids!(template)).selected_item().checked_sub(1)?;
        TEMPLATES.get(at).map(|&(_, template)| template)
    }

    fn set_form(&mut self, cx: &mut Cx, open: bool) {
        self.adding = open;
        self.pending = None;
        self.view.widget(cx, ids!(form)).set_visible(cx, open);
        self.view.button(cx, ids!(open_add)).set_visible(cx, !open);
        if !open { return; }
        let picker = self.view.drop_down(cx, ids!(template));
        picker.set_labels(cx, std::iter::once("Choose a measure").chain(TEMPLATES.iter().map(|t| t.0)).map(str::to_owned).collect());
        picker.set_selected_item(cx, 0);
        let family = self.view.drop_down(cx, ids!(family));
        family.set_labels(cx, std::iter::once("Choose a check family")
            .chain(fab_elec_req::Family::SPELLINGS.iter().map(|x| x.0)).map(str::to_owned).collect());
        family.set_selected_item(cx, 0);
        for id in [ids!(req_name), ids!(bound), ids!(bench), ids!(expect)] {
            self.view.text_input(cx, id).set_text(cx, "");
        }
        self.design = Default::default();
        self.set_refusal(cx, "");
        self.sync_units(cx);
        self.form_fields(cx);
    }

    fn sync_units(&mut self, cx: &mut Cx) {
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return };
        if self.design.as_ptr() == Arc::as_ptr(&live.snap.design) { return; }
        self.design = Arc::downgrade(&live.snap.design);
        self.units = live.snap.design.units.iter().filter(|u| u.board.is_some()).map(|u| u.name.clone()).collect();
        self.form_fields(cx);
    }

    fn form_fields(&self, cx: &mut Cx) {
        let template = self.template(cx);
        let checks = matches!(template, Some(Template::Checks(_)));
        let board = matches!(template, Some(Template::Board(_) | Template::Case));
        let field = self.view.drop_down(cx, ids!(unit));
        let previous = field.selected_item().checked_sub(1).map(|_| field.selected_label());
        let prompt = if checks { "All units" } else { "Choose a board unit" };
        let labels = std::iter::once(prompt.to_owned()).chain(self.units.iter().cloned()).collect::<Vec<_>>();
        if !field.has_labels(&labels) {
            let at = previous.and_then(|name| self.units.iter().position(|u| *u == name)).map_or(0, |at| at + 1);
            field.set_labels(cx, labels);
            field.set_selected_item(cx, at);
        }
        self.view.widget(cx, ids!(unit)).set_visible(cx, (board && self.units.len() > 1) || (checks && !self.units.is_empty()));
        let only = self.view.label(cx, ids!(only_unit));
        only.set_visible(cx, board && self.units.len() <= 1);
        if board {
            only.set_text(cx, self.units.first().map_or("This design has no board unit", String::as_str));
        }
        self.view.widget(cx, ids!(family)).set_visible(cx, checks);
        self.view.widget(cx, ids!(bench)).set_visible(cx, matches!(template, Some(Template::Bench | Template::Expect)));
        self.view.widget(cx, ids!(expect)).set_visible(cx, template == Some(Template::Expect));
        self.view.widget(cx, ids!(bound)).set_visible(cx, template.is_some_and(Template::has_bound));
        let hint = match template {
            Some(Template::Board("size")) => "Maximum [width height], with length units",
            Some(Template::Print("time")) => "Maximum print time, with time unit",
            Some(Template::Print(_)) => "Maximum filament mass, with mass unit",
            _ => "Maximum count or Splash bound",
        };
        self.view.text_input(cx, ids!(bound)).set_empty_text(cx, hint.into());
    }

    fn set_refusal(&mut self, cx: &mut Cx, why: &str) {
        let label = self.view.label(cx, ids!(refusal));
        label.set_visible(cx, !why.is_empty());
        label.set_text(cx, why);
        self.view.widget(cx, ids!(submit)).set_disabled(cx, self.pending.is_some());
    }

    fn submit(&mut self, cx: &mut Cx) {
        if self.pending.is_some() { return; }
        let Some(template) = self.template(cx) else { return self.set_refusal(cx, "Choose a measure."); };
        let name = self.view.text_input(cx, ids!(req_name)).text();
        if self.rows.iter().any(|(_, r)| r.name == name.trim()) {
            return self.set_refusal(cx, "A requirement with this name already exists.");
        }
        let board = matches!(template, Template::Board(_) | Template::Case);
        let unit = if board && self.units.len() == 1 { self.units.first() } else {
            self.view.drop_down(cx, ids!(unit)).selected_item().checked_sub(1).and_then(|at| self.units.get(at))
        };
        let family = self.view.drop_down(cx, ids!(family)).selected_item().checked_sub(1)
            .and_then(|at| fab_elec_req::Family::SPELLINGS.get(at)).map(|x| x.0);
        let bench = self.view.text_input(cx, ids!(bench)).text();
        let expect = self.view.text_input(cx, ids!(expect)).text();
        let bound = self.view.text_input(cx, ids!(bound)).text();
        match template.add(&name, unit.map(String::as_str), family, &bench, &expect, &bound) {
            Ok(command) => {
                self.pending = Some(name.trim().to_owned());
                self.set_refusal(cx, "");
                cx.widget_action(self.widget_uid(), command);
            }
            Err(why) => self.set_refusal(cx, &why),
        }
    }

    fn ink(&self, ink: Ink) -> Vec4f {
        match ink {
            Ink::Ok => self.color_ok,
            Ink::Error => self.color_error,
            Ink::Warning => self.color_warning,
            Ink::Muted => self.color_text_muted,
            Ink::Dim => self.color_text_dim,
        }
    }

    fn clear_hover(&mut self, cx: &mut Cx) {
        if let Some((_, highlighted)) = self.hovered.take() {
            cx.widget_action(self.widget_uid(), TipAction::HoverOut);
            if highlighted {
                cx.widget_action(self.widget_uid(), ViewAction::Hover(None));
            }
        }
    }

    fn sync(&mut self, cx: &mut Cx) {
        if self.adding { self.sync_units(cx); }
        let live = cx.get_global_ref::<ElecLive>();
        let report = live.and_then(|live| live.requirements.as_ref());
        if self.shown.as_ref().map(Arc::as_ptr) != report.map(Arc::as_ptr) {
            self.shown = report.cloned();
            self.rows = self.shown.as_ref().map_or_else(Vec::new, |shown| {
                shown.report.rows.iter().enumerate().map(|(i, row)| {
                    let controls = RowControls::new(row, |name| live.is_some_and(|live|
                        crate::snapshot::run_bench(&live.snap.benches, Some(name)).is_some_and(|b| b.out.name == name)));
                    (RowText::new(row, shown.at.get(i)), controls)
                }).collect()
            });
            if self.pending.as_ref().is_some_and(|name| self.rows.iter().any(|(_, row)| row.name == *name)) {
                self.set_form(cx, false);
            }
            self.clear_hover(cx);
        } else if !self.refresh_header {
            return;
        }
        self.refresh_header = false;
        let heading = self.shown.as_ref().map_or_else(|| EMPTY.to_owned(), |shown| header(&shown.report));
        self.view.label(cx, ids!(header)).set_text(cx, &heading);
        let mistakes = self.view.label(cx, ids!(mistakes));
        let notes = self.shown.as_ref().map_or(&[][..], |shown| shown.mistakes.as_slice());
        let count = notes.len();
        mistakes.set_visible(cx, count != 0);
        if count > 0 {
            mistakes.set_text(cx, &format!("{count} document mistake{}", if count == 1 { "" } else { "s" }));
            mistakes.set_text_color(cx, self.color_text_muted);
            self.view.tip(cx, ids!(mistakes_tip)).set_text(&notes.join("\n"));
        }
        self.view.redraw(cx);
    }
}

impl Widget for ReqList {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Signal) {
            self.sync(cx);
        }
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        if let Event::Actions(sent) = event {
            for refusal in sent.iter().filter_map(|a| a.downcast_ref::<AddRefused>()) {
                if self.pending.as_ref() == Some(&refusal.name) {
                    self.pending = None;
                    self.set_refusal(cx, &refusal.why);
                }
            }
        }
        if !actions.is_empty() {
            if self.view.button(cx, ids!(open_add)).clicked(&actions) {
                self.set_form(cx, true);
            } else if self.adding {
                if self.view.button(cx, ids!(cancel)).clicked(&actions) {
                    self.set_form(cx, false);
                } else if self.view.drop_down(cx, ids!(template)).selected(&actions).is_some() {
                    self.view.text_input(cx, ids!(bound)).set_text(cx, "");
                    self.form_fields(cx);
                } else if self.view.button(cx, ids!(submit)).clicked(&actions) {
                    self.submit(cx);
                }
            }
        }
        let list = self.view.flat_list(cx, ids!(rows));
        for action in actions.iter().filter_map(|a| a.as_widget_action()) {
            let Some(group) = &action.group else { continue };
            if group.group_uid != list.widget_uid() || group.item_uid != action.widget_uid {
                continue;
            }
            let Some(id) = cx.widget_tree().name_of(group.item_uid) else { continue };
            use makepad_widgets_core::view::ViewAction as Input;
            let Some(input) = action.action.downcast_ref::<Input>() else { continue };
            if matches!(input, Input::FingerHoverOut(_)) && self.hovered.is_some_and(|(old, _)| old == id) {
                self.clear_hover(cx);
            }
            let Some(row) = self.shown.as_ref().and_then(|s| s.report.rows.get(id.0 as usize)) else { continue };
            let Some(live) = cx.get_global_ref::<ElecLive>() else { continue };
            let violations = &live.snap.violations.value;
            let design = &live.snap.design;
            match input {
                Input::FingerHoverIn(e) => {
                    let sel = if row.focus == Evidence::Findings { evidence(row, violations, design) } else { None };
                    self.clear_hover(cx);
                    self.hovered = Some((id, sel.is_some()));
                    let tip = TipRequest { wrap_width: 360.0, ..TipRequest::plain(self.rows[id.0 as usize].0.detail.clone(), e.rect) };
                    cx.widget_action(self.widget_uid(), TipAction::HoverInWith(tip));
                    if sel.is_some() {
                        cx.widget_action(self.widget_uid(), ViewAction::Hover(sel));
                    }
                }
                Input::FingerDown(e) => {
                    if !e.is_primary_hit() { continue; }
                    if let Some(focus) = focus(row, e.tap_count == 2, violations, design) {
                        cx.widget_action(self.widget_uid(), ViewAction::Focus(focus));
                    }
                }
                _ => {}
            }
        }
        cx.extend_actions(actions);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // A newly opened tab may have missed the signal while its panel was off screen.
        self.sync(cx);
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<FlatList>() else { continue };
            let origin = cx.turtle().pos().y;
            let (top, bottom) = cx.turtle_visible_y_range();
            let start = (((top - origin) / ROW_HEIGHT).floor().max(0.0) as usize).min(self.rows.len());
            let end = (((bottom - origin) / ROW_HEIGHT).ceil().max(0.0) as usize).clamp(start, self.rows.len());
            let spacer = |cx: &mut Cx2d, count: usize| {
                if count > 0 {
                    cx.walk_turtle(Walk {width: Size::fill(), height: Size::Fixed(count as f64 * ROW_HEIGHT), ..Walk::default()});
                }
            };
            spacer(cx, start);
            for i in start..end {
                let (text, controls) = &self.rows[i];
                let Some(item) = list.item(cx, LiveId(i as u64), id!(Row)) else { continue };
                if let Some(mut row) = item.borrow_mut::<ReqItem>() {
                    if row.controls != *controls {
                        if row.controls.name != controls.name { row.view.popover(cx, ids!(menu)).close(cx); }
                        row.controls = controls.clone();
                        row.edit_reason(cx, false);
                    }
                    if row.accessible != text.accessible {
                        row.accessible.clone_from(&text.accessible);
                    }
                }
                item.label(cx, ids!(summary.first.title)).set_text(cx, &text.title);
                let value = item.label(cx, ids!(summary.first.value));
                value.set_text(cx, &text.value);
                value.set_text_color(cx, self.ink(text.value_ink));
                if let Some(mut dot) = item.view(cx, ids!(summary.first.dot)).borrow_mut() {
                    let c = self.ink(text.dot);
                    dot.draw_bg.set_dyn_instance(cx, id!(color), &[c.x, c.y, c.z, c.w]);
                }
                let detail = item.label(cx, ids!(summary.detail));
                detail.set_text(cx, &text.detail);
                detail.set_text_color(cx, self.color_text_dim);
                item.draw_all(cx, scope);
            }
            spacer(cx, self.rows.len() - end);
            if self.hovered.is_some_and(|(id, _)| !list.items.is_visible(&id)) {
                self.clear_hover(cx);
            }
            list.items.retain_visible();
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::{BoardId, DocId, NetId, NetRef, Severity};
    use fab_elec_req::{judge, read, BoardMeasures, CheckFacts, Facts, Family, FindingFact, PrintFacts, Producers, UnitFacts};

    #[test]
    fn judged_rows_show_their_status_and_focus_their_evidence() {
        let text = r#"require("fits the case", board("ledctl").size, {max: [60mm 40mm]})
require("DRC clean", checks(@drc).errors, {max: 0})
require("case clears", case("ledctl").fits)
require("two layers", board("ledctl").layers, {max: 2})
require("under 40 g", print.filament, {max: 40g})
require("starts up", bench("bringup"))
waive("two layers", "prototype stackup")
propose_waiver("DRC clean", "prototype run")"#;
        let doc = DocId(3);
        let (req, mistakes) = read(doc, text);
        assert!(mistakes.is_empty(), "the example requirements must parse: {mistakes:?}");
        let id = BoardId(4);
        let net = Sel::Net(NetRef { board: id, net: NetId(9) });
        let violation = Violation::new(fab_elec_circuit::CheckKind::Clearance, Severity::Error, Some(id),
            vec![net.clone()], "clearance", "Copper is too close".into());
        let finding = FindingFact { id: violation.id, unit: None, family: Family::Drc, severity: Severity::Error, unconnected: false };
        let facts = Facts {
            units: vec![UnitFacts { unit: "ledctl".into(), board: Some(BoardMeasures {
                size: Some([0.05, 0.03]), layers: 4, parts: 0, unplaced: 0,
            }) }],
            checks: Some(CheckFacts { current: true, findings: vec![FindingFact { id: !violation.id, ..finding.clone() }, finding], ..Default::default() }),
            print: Some(PrintFacts { current: false, at: 0.0, seconds: 0.0, grams: 35.0 }),
            benches: vec![fab_elec_req::BenchFacts { bench: "bringup".into(), run: None }],
            producers: Producers { cases: true, benches: true, print: true },
            ..Default::default()
        };
        let shown = ReqShown {
            report: judge(&[req], &facts),
            at: (1..=6).map(|line| (Arc::from("ledctl.req.splash"), line)).collect(),
            mistakes: Vec::new(),
        };
        assert_eq!(header(&shown.report), "2 of 6 met, 1 failing, 3 unknown, 1 waived, 1 proposed", "the header counts the proposal separately and the proposed failure still fails");
        assert_eq!(header(&Report::default()), "0 of 0 met", "the header omits zero status counts");
        let expected = [
            ("fits the case", "50 x 30 mm, max 60 x 40 mm", "board outline · ledctl.req.splash:1", Ink::Ok, Ink::Ok),
            ("DRC clean", "2, max 0", "checks · proposed waiver: prototype run · ledctl.req.splash:2", Ink::Error, Ink::Error),
            ("case clears", "holds", "case checks · the case checks have not run on ledctl yet · case checks run on each plate build · ledctl.req.splash:3", Ink::Warning, Ink::Warning),
            ("two layers", "4, max 2", "board · waived: prototype stackup · ledctl.req.splash:4", Ink::Muted, Ink::Muted),
            ("under 40 g", "35 g, max 40 g", "slice · stale: the plate changed since the slice · ledctl.req.splash:5", Ink::Warning, Ink::Dim),
        ];
        for (i, (title, value, detail, dot, value_ink)) in expected.into_iter().enumerate() {
            let row = &shown.report.rows[i];
            let display = RowText::new(row, shown.at.get(i));
            assert_eq!(display.title, title, "row {i} carries the person's words");
            assert_eq!(display.value, value, "{title} shows its measured value and bound");
            assert_eq!(display.detail, detail, "{title} explains its evidence and location");
            assert_eq!(display.dot, dot, "{title} uses the status colour");
            assert_eq!(display.value_ink, value_ink, "{title} uses the value's freshness colour");
            assert_eq!(display.accessible, format!("{title}: {}", row.status.word()), "{title} names its status for accessibility");
        }

        let catalog = |action, pairs: &[(&str, &str)]| ReqAction::run(action,
            pairs.iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect());
        let expressions = [
            r#"board("ledctl").size"#, r#"board("ledctl").layers"#, r#"board("ledctl").parts"#,
            r#"board("ledctl").unplaced"#, r#"board("ledctl").unrouted"#,
            r#"checks(@erc, {unit: "ledctl"}).errors"#, r#"checks(@erc, {unit: "ledctl"}).warnings"#,
            r#"case("ledctl").fits"#, r#"bench("bringup")"#, r#"bench("bringup").expect("power")"#,
            "print.time", "print.filament",
        ];
        assert_eq!(TEMPLATES.len(), expressions.len(), "Add offers one template for every catalog measure form");
        for (&(label, template), expr) in TEMPLATES.iter().zip(expressions) {
            let limit = match template {
                Template::Board("size") => "[50mm 30mm]", Template::Print("time") => "3h",
                Template::Print(_) => "40g", _ => "2",
            };
            let mut args = vec![("name", "target"), ("measure", expr)];
            let bound = format!("{{max: {limit}}}");
            if template.has_bound() {
                assert!(template.add("target", Some("ledctl"), Some("erc"), "bringup", "power", "").is_err(),
                    "{label} waits for the person's bound");
                args.push(("bound", &bound));
            }
            assert_eq!(template.add("target", Some("ledctl"), Some("erc"), "bringup", "power", limit),
                Ok(catalog("elec.req.add", &args)), "{label} emits Add with the chosen measure and only its own fields");
        }
        for row in &shown.report.rows {
            let controls = RowControls::new(row, |bench| bench == "bringup");
            let name = row.name.as_str();
            assert_eq!(controls.remove(), catalog("elec.req.remove", &[("name", name)]), "{name}'s menu removes that row");
            if name == "two layers" {
                assert!(controls.waived, "a waived row offers Unwaive in place of Waive");
                assert_eq!(controls.unwaive(), catalog("elec.req.unwaive", &[("name", name)]), "Unwaive names the waived row");
            } else if name == "DRC clean" {
                assert!(!controls.offers_waive(), "the proposed row offers no Waive");
                assert_eq!(controls.accept(), Some(catalog("elec.req.waive", &[("name", name), ("reason", "prototype run")])),
                    "Accept grants the waiver with exactly its proposed reason");
                assert_eq!(controls.unwaive(), catalog("elec.req.unwaive", &[("name", name)]), "Decline removes the proposal by name");
            } else {
                assert_eq!(controls.offers_waive(), row.status != Status::Pass, "{name} offers Waive with a reason unless it is met");
                assert_eq!(controls.waiver("prototype"), catalog("elec.req.waive", &[("name", name), ("reason", "prototype")]),
                    "{name} sends the person's reason");
            }
            let measure = match name {
                "starts up" => Some(ReqAction::Run { action: "elec.bench.run", args: vec![], bench: Some("bringup".into()) }),
                "under 40 g" => Some(ReqAction::Slicer),
                _ => None,
            };
            assert_eq!(controls.measure, measure, "{name} offers only its on-demand measurement; case checks run at plate build");
        }
        assert_eq!(RowControls::new(&shown.report.rows[5], |_| false).measure, None, "a missing bench cannot be measured");

        let unit = fab_elec_design::Unit { id, name: "ledctl".into(), kind: fab_elec_design::UnitKind::Board,
            board: Some(Arc::new(fab_elec_board::Board::new(id, "ledctl"))), circuit: Arc::default(), supplied: vec![], factory: None };
        let design = Design { units: vec![unit], ..Default::default() };
        let click = |i, double| focus(&shown.report.rows[i], double, std::slice::from_ref(&violation), &design);
        let checks = |kind, sel| Some(Focus { kind, sel, origin: FocusOrigin::Checks });
        assert_eq!(click(0, false), checks(FocusKind::Board, Sel::Unit(id)), "a unit row focuses its board");
        assert_eq!(click(1, false), checks(FocusKind::Net, net), "a findings row skips missing ids and focuses the first surviving finding's selection");
        assert_eq!(click(2, false), None, "case evidence awaits the case editor");
        let start = text.find("require(\"case clears\"").unwrap();
        let end = start + text[start..].lines().next().unwrap().len();
        assert_eq!(click(2, true), checks(FocusKind::SourceRange, Sel::Source { doc, start: start as u32, end: end as u32 }),
            "a double click focuses the statement even without a single-click target");
    }
}

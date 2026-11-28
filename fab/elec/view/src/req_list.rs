//! The project's requirements, in document order. Text is prepared once per judged report.
//! Only the visible fixed-height rows have widgets. Evidence is resolved against the current
//! snapshot when the person points at a row.

use crate::{ElecLive, ReqShown, ViewAction};
use crate::snapshot::ProposalId;
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
            Tip{text: "Write the brief Fab drafts requirements from" req_brief_open := Press{text: "Brief"}}
            Tip{text: "Add a requirement" open_add := Press{text: "Add" a11y_name: "Add a requirement"}}
        }
        View{width: Fill height: Fit flow: Right spacing: 4 align: Align{y: 0.5}
            req_brief_draft_state := Label{width: Fill height: Fit visible: false
                a11y: {live: @Polite}}
            req_brief_draft_review := Press{text: "Review" visible: false}
        }
        req_brief_shown := Label{width: Fill height: Fit visible: false}
        req_brief_box := View{
            width: Fill height: Fit flow: Down spacing: 3 visible: false
            req_brief_input := TextInput{
                width: Fill height: 84 is_multiline: true submit_on_enter: false
                empty_text: "Your brief" a11y: {label: "Your brief"}
            }
            View{width: Fill height: Fit flow: Right{wrap: true} spacing: 4
                req_brief_save_tip := Tip{text: "Save the brief" req_brief_save := Press{text: "Save brief" enabled: false}}
                req_draft_tip := Tip{text: #(NO_AI_MODEL) req_draft := Press{text: "Draft requirements" enabled: false}}
                Tip{text: "Close the brief" req_brief_close := Press{text: "Close"}}
            }
            req_brief_refusal := Label{width: Fill height: Fit visible: false a11y: {live: @Polite} draw_text +: {color: theme.color_error}}
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
                req_submit_tip := Tip{text: "Add this requirement" submit := Press{text: "Add" a11y_name: "Add this requirement"}}
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
    Draft,
    Review(ProposalId),
}

pub const NO_AI_MODEL: &str = "No AI model is connected";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftView {
    pub text: String,
    pub review: Option<ProposalId>,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReqDraft {
    pub slot: Option<&'static str>,
    pub admit: Result<(), &'static str>,
    pub view: Option<DraftView>,
}

impl Default for ReqDraft {
    fn default() -> Self { Self { slot: None, admit: Err(NO_AI_MODEL), view: None } }
}

impl ReqAction {
    fn run(action: &'static str, args: Vec<(String, String)>) -> Self {
        Self::Run { action, args, bench: None }
    }
}

/// The form and the submitted words that a refusal belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReqForm {
    Add(String),
    Brief(String),
}

#[derive(Clone, Debug)]
pub struct AddRefused {
    pub form: ReqForm,
    pub why: String,
}

struct SavingBrief {
    words: String,
    draft_after: bool,
    refusal: std::cell::Cell<Option<String>>,
}

#[derive(Default)]
struct SavingBriefs(Vec<std::rc::Weak<SavingBrief>>);

impl ReqDraft {
    fn requirements(&self) -> Option<&DraftView> {
        self.view.as_ref().filter(|_| self.slot == Some("requirements"))
    }

    /// A report from the replacement electronics must never acknowledge an armed save.
    pub fn disarm_saves(cx: &Cx) {
        if let Some(saves) = cx.get_global_ref::<SavingBriefs>() {
            for save in saves.0.iter().filter_map(std::rc::Weak::upgrade).filter(|s| s.draft_after) {
                let reason = save.refusal.take().unwrap_or_else(|| "The electronics reloaded before drafting started.".into());
                save.refusal.set(Some(reason));
            }
        }
    }
}

impl AddRefused {
    pub fn post(self, cx: &mut Cx) {
        // An off-screen tab misses the action. Keep its refusal with the submitted words
        // until the tab draws again. A closed editor keeps no draft alive here.
        if let ReqForm::Brief(words) = &self.form {
            for save in cx.global::<SavingBriefs>().0.iter().filter_map(std::rc::Weak::upgrade) {
                if save.words == *words { save.refusal.set(Some(self.why.clone())); }
            }
        }
        cx.action(self);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Template {
    Board(&'static str), Checks(&'static str), Case, Bench, Expect, Print(&'static str), Wish,
}

const TEMPLATES: &[(&str, Template)] = &[
    ("Board size", Template::Board("size")), ("Copper layers", Template::Board("layers")),
    ("Placed parts", Template::Board("parts")), ("Unplaced parts", Template::Board("unplaced")),
    ("Unrouted connections", Template::Board("unrouted")), ("Check errors", Template::Checks("errors")),
    ("Check warnings", Template::Checks("warnings")), ("Case fits", Template::Case),
    ("Bench passes", Template::Bench), ("Bench expectation", Template::Expect),
    ("Print time", Template::Print("time")), ("Filament mass", Template::Print("filament")),
    ("Wish (not measured)", Template::Wish),
];

impl Template {
    fn has_bound(self) -> bool { matches!(self, Self::Board(_) | Self::Checks(_) | Self::Print(_)) }

    fn add(self, name: &str, unit: Option<&str>, family: Option<&str>, bench: &str, expect: &str, bound: &str) -> Result<ReqAction, String> {
        use makepad_widgets_core::makepad_script::syntax::write::quote;
        let scope = || unit.map_or_else(String::new, |u| format!("({})", quote(u)));
        let measure = match self {
            Self::Wish => {
                if name.trim().is_empty() { return Err("Enter the wish in your words.".into()); }
                return Ok(ReqAction::run("elec.req.wish", vec![("words".into(), name.trim().into())]));
            }
            Self::Board(field) => format!("board{}.{field}", scope()),
            Self::Case => format!("case{}.fits", scope()),
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
    wish: bool,
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
            name: row.name.clone(), wish: false, waived: matches!(row.status, Status::Waived { .. }), passing: row.status == Status::Pass,
            proposed: row.proposed.clone(), measure,
        }
    }

    fn offers_waive(&self) -> bool { !self.wish && !self.waived && !self.passing && self.proposed.is_none() }

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
    for (n, status) in [(report.fail, "failing"), (report.unknown, "unknown"), (report.waived, "waived"), (report.proposed, "proposed"),
        (report.wishes.len(), "not measured")] {
        if n > 0 {
            use std::fmt::Write;
            write!(text, ", {n} {status}").unwrap();
        }
    }
    text
}

fn brief_label(report: &Report) -> Option<String> {
    report.brief.as_ref().map(|b| format!("Brief: {}", b.words))
}

fn wish_row(w: &fab_elec_req::WishRow, at: Option<&(Arc<str>, usize)>) -> (RowText, RowControls) {
    let text = RowText { title: w.words.clone(), value: "Not measured".into(),
        detail: at.map_or_else(String::new, |(doc, line)| format!("{doc}:{line}")),
        accessible: format!("{}: Not measured", w.words), dot: Ink::Dim, value_ink: Ink::Dim };
    (text, RowControls { name: w.words.clone(), wish: true, ..Default::default() })
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

fn wish_focus(wish: &fab_elec_req::WishRow) -> Focus {
    Focus { kind: FocusKind::SourceRange, sel: Sel::Source { doc: wish.doc, start: wish.span.start, end: wish.span.end }, origin: FocusOrigin::Checks }
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
        if self.controls.wish {
            self.view.handle_event(cx, event, scope);
            return;
        }
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
    #[rust] pending: Option<PendingAdd>,
    #[rust] brief_open: bool,
    #[rust] brief_pending: Option<std::rc::Rc<SavingBrief>>,
    #[rust] draft: ReqDraft,
    #[rust] units: Vec<String>,
    #[rust] design: std::sync::Weak<Design>,
    #[rust] refresh_header: bool,
    /// The row whose detail tip is up, and whether it also highlights evidence in another view.
    #[rust] hovered: Option<(LiveId, bool)>,
}

struct PendingAdd {
    words: String,
    wish: bool,
    previous: usize,
}

impl ScriptHook for ReqList {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, _apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        self.refresh_header = true;
    }
}

impl ReqList {
    fn brief_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.view.button(cx, ids!(req_brief_draft_review)).clicked(actions) {
            if let Some(id) = self.draft.requirements().and_then(|v| v.review) {
                cx.widget_action(self.widget_uid(), ReqAction::Review(id));
            }
        }
        if self.view.button(cx, ids!(req_brief_open)).clicked(actions) {
            self.set_brief_open(cx, !self.brief_open);
        } else if self.brief_open {
            if self.view.button(cx, ids!(req_brief_close)).clicked(actions) {
                self.set_brief_open(cx, false);
            } else if self.view.button(cx, ids!(req_brief_save)).clicked(actions) {
                self.save_brief(cx, false);
            } else if self.view.button(cx, ids!(req_draft)).clicked(actions) {
                self.save_brief(cx, true);
            } else if let Some(words) = self.view.text_input(cx, ids!(req_brief_input)).changed(actions) {
                self.brief_save_state(cx, &words);
            }
        }
    }

    fn saved_brief(&self) -> &str {
        self.shown.as_ref().and_then(|s| s.report.brief.as_ref()).map_or("", |b| b.words.as_str())
    }

    fn set_brief_open(&mut self, cx: &mut Cx, open: bool) {
        self.brief_open = open;
        self.view.widget(cx, ids!(req_brief_box)).set_visible(cx, open);
        if open {
            self.view.text_input(cx, ids!(req_brief_input)).set_text(cx, self.saved_brief());
        } else {
            self.view.label(cx, ids!(req_brief_refusal)).set_visible(cx, false);
        }
        self.show_brief(cx);
    }

    fn show_brief(&mut self, cx: &mut Cx) {
        self.take_brief_refusal(cx);
        let shown = self.view.label(cx, ids!(req_brief_shown));
        shown.set_visible(cx, !self.brief_open && !self.saved_brief().is_empty());
        if let Some(label) = self.shown.as_ref().and_then(|s| brief_label(&s.report)) {
            shown.set_text(cx, &label);
            shown.set_text_color(cx, self.color_text_dim);
        }
        if self.brief_pending.as_ref().is_some_and(|save| self.matches_saved_brief(&save.words)) {
            if self.brief_pending.take().unwrap().draft_after {
                cx.widget_action(self.widget_uid(), ReqAction::Draft);
            }
        }
        if self.brief_open {
            let words = self.view.text_input(cx, ids!(req_brief_input)).text();
            self.brief_save_state(cx, &words);
        }
    }

    fn matches_saved_brief(&self, words: &str) -> bool {
        // The writer removes the statement for whitespace alone. Every other byte is significant.
        self.saved_brief() == if words.trim().is_empty() { "" } else { words }
    }

    fn brief_save_state(&self, cx: &mut Cx, words: &str) {
        let reason = if self.brief_pending.is_some() { Some("Saving the brief") }
            else if self.matches_saved_brief(words) { Some("Nothing to save: this is the saved brief") }
            else { None };
        self.view.button(cx, ids!(req_brief_save)).set_enabled(cx, reason.is_none());
        self.view.tip(cx, ids!(req_brief_save_tip)).set_text(reason.unwrap_or("Save the brief"));
        self.draft_button(cx, words);
    }

    fn draft_reason(&self, words: &str) -> Option<&'static str> {
        if self.draft.view.as_ref().is_some_and(|v| !v.done) { Some(if self.draft.slot == Some("requirements") { "Drafting requirements from the brief" } else { "Another stage is being drafted" }) }
        else if self.brief_pending.is_some() { Some("Saving the brief") }
        else { self.draft.admit.err().or_else(|| words.trim().is_empty().then_some("Write the brief first")) }
    }

    fn draft_button(&self, cx: &mut Cx, words: &str) {
        let reason = self.draft_reason(words);
        self.view.button(cx, ids!(req_draft)).set_enabled(cx, reason.is_none());
        self.view.tip(cx, ids!(req_draft_tip)).set_text(reason.unwrap_or("Draft requirements"));
    }

    fn save_brief(&mut self, cx: &mut Cx, draft_after: bool) {
        let words = self.view.text_input(cx, ids!(req_brief_input)).text();
        if self.brief_pending.is_some() || (draft_after && self.draft_reason(&words).is_some()) { return; }
        if self.matches_saved_brief(&words) {
            if draft_after { cx.widget_action(self.widget_uid(), ReqAction::Draft); }
            return;
        }
        let save = std::rc::Rc::new(SavingBrief { words: words.clone(), draft_after, refusal: Default::default() });
        let pending = &mut cx.global::<SavingBriefs>().0;
        pending.retain(|p| p.strong_count() != 0);
        pending.push(std::rc::Rc::downgrade(&save));
        self.brief_pending = Some(save);
        self.view.label(cx, ids!(req_brief_refusal)).set_visible(cx, false);
        self.brief_save_state(cx, &words);
        cx.widget_action(self.widget_uid(), ReqAction::run("elec.req.brief", vec![("words".into(), words)]));
    }

    fn take_brief_refusal(&mut self, cx: &mut Cx) {
        let Some(why) = self.brief_pending.as_ref().and_then(|save| save.refusal.take()) else { return };
        self.brief_pending = None;
        let label = self.view.label(cx, ids!(req_brief_refusal));
        label.set_text(cx, &why);
        label.set_visible(cx, true);
        let words = self.view.text_input(cx, ids!(req_brief_input)).text();
        self.brief_save_state(cx, &words);
    }

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
            only.set_text(cx, self.units.first().map_or("Device's only board (when created)", String::as_str));
        }
        self.view.widget(cx, ids!(family)).set_visible(cx, checks);
        self.view.widget(cx, ids!(bench)).set_visible(cx, matches!(template, Some(Template::Bench | Template::Expect)));
        self.view.widget(cx, ids!(expect)).set_visible(cx, template == Some(Template::Expect));
        self.view.widget(cx, ids!(bound)).set_visible(cx, template.is_some_and(Template::has_bound));
        self.view.text_input(cx, ids!(req_name)).set_empty_text(cx,
            if template == Some(Template::Wish) { "The wish, in your words" } else { "Requirement name" }.into());
        let submit_name = if template == Some(Template::Wish) { "Add this wish" } else { "Add this requirement" };
        self.view.tip(cx, ids!(req_submit_tip)).set_text(submit_name);
        let mut submit = self.view.widget(cx, ids!(submit));
        script_apply_eval!(cx, submit, { a11y_name: #(submit_name) });
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
        let wish = template == Template::Wish;
        let previous = self.rows.iter().filter(|(_, r)| r.wish == wish && r.name == name.trim()).count();
        if !wish && previous != 0 {
            return self.set_refusal(cx, "A requirement with this name already exists.");
        }
        let board = matches!(template, Template::Board(_) | Template::Case);
        let unit = if board && self.units.len() == 1 { self.units.first() } else {
            self.view.drop_down(cx, ids!(unit)).selected_item().checked_sub(1).and_then(|at| self.units.get(at))
        };
        if board && self.units.len() > 1 && unit.is_none() {
            return self.set_refusal(cx, "Choose a board unit for this measure.");
        }
        let family = self.view.drop_down(cx, ids!(family)).selected_item().checked_sub(1)
            .and_then(|at| fab_elec_req::Family::SPELLINGS.get(at)).map(|x| x.0);
        let bench = self.view.text_input(cx, ids!(bench)).text();
        let expect = self.view.text_input(cx, ids!(expect)).text();
        let bound = self.view.text_input(cx, ids!(bound)).text();
        match template.add(&name, unit.map(String::as_str), family, &bench, &expect, &bound) {
            Ok(command) => {
                self.pending = Some(PendingAdd { words: name.trim().to_owned(), wish, previous });
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
        self.take_brief_refusal(cx);
        if let Some(draft) = cx.get_global_ref::<ReqDraft>() {
            if *draft != self.draft || self.refresh_header {
                self.draft.clone_from(draft);
                let state = self.view.label(cx, ids!(req_brief_draft_state));
                if let Some(v) = self.draft.requirements() { state.set_text(cx, &v.text); }
                state.set_visible(cx, self.draft.requirements().is_some());
                self.view.button(cx, ids!(req_brief_draft_review)).set_visible(cx, self.draft.requirements().and_then(|v| v.review).is_some());
                self.draft_button(cx, &self.view.text_input(cx, ids!(req_brief_input)).text());
            }
        }
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
                }).chain(shown.report.wishes.iter().enumerate().map(|(i, w)| wish_row(w, shown.wish_at.get(i)))).collect()
            });
            if self.pending.as_ref().is_some_and(|p| self.rows.iter().filter(|(_, r)| r.wish == p.wish && r.name == p.words).count() > p.previous) {
                self.set_form(cx, false);
            }
            self.clear_hover(cx);
        } else if !self.refresh_header {
            return;
        }
        self.refresh_header = false;
        let heading = self.shown.as_ref().map_or_else(|| EMPTY.to_owned(), |shown| header(&shown.report));
        self.view.label(cx, ids!(header)).set_text(cx, &heading);
        self.show_brief(cx);
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
                match &refusal.form {
                    ReqForm::Add(name) if self.pending.as_ref().is_some_and(|p| p.words == *name) => {
                        self.pending = None;
                        self.set_refusal(cx, &refusal.why);
                    }
                    ReqForm::Brief(_) => self.take_brief_refusal(cx),
                    _ => {}
                }
            }
        }
        if !actions.is_empty() {
            self.brief_actions(cx, &actions);
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
            let Some(shown) = &self.shown else { continue };
            let i = id.0 as usize;
            let Some((text, _)) = self.rows.get(i) else { continue };
            let row = shown.report.rows.get(i);
            let Some(live) = cx.get_global_ref::<ElecLive>() else { continue };
            let violations = &live.snap.violations.value;
            let design = &live.snap.design;
            match input {
                Input::FingerHoverIn(e) => {
                    let sel = row.filter(|r| r.focus == Evidence::Findings).and_then(|r| evidence(r, violations, design));
                    let tip = TipRequest { wrap_width: 360.0, ..TipRequest::plain(text.detail.clone(), e.rect) };
                    self.clear_hover(cx);
                    self.hovered = Some((id, sel.is_some()));
                    cx.widget_action(self.widget_uid(), TipAction::HoverInWith(tip));
                    if sel.is_some() {
                        cx.widget_action(self.widget_uid(), ViewAction::Hover(sel));
                    }
                }
                Input::FingerDown(e) => {
                    if !e.is_primary_hit() { continue; }
                    let target = match row {
                        Some(r) => focus(r, e.tap_count == 2, violations, design),
                        None if e.tap_count == 2 => shown.report.wishes.get(i - shown.report.rows.len()).map(wish_focus),
                        None => None,
                    };
                    if let Some(focus) = target {
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
                        if row.controls.name != controls.name || row.controls.wish != controls.wish {
                            row.view.popover(cx, ids!(menu)).close(cx);
                            row.view.widget(cx, ids!(more)).set_visible(cx, false);
                            row.over = false;
                        }
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
propose_waiver("DRC clean", "prototype run")
wish("warm light")
brief("Light the workbench")"#;
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
            wish_at: vec![(Arc::from("ledctl.req.splash"), 9)],
            brief_at: Some((Arc::from("ledctl.req.splash"), 10)),
            mistakes: Vec::new(),
        };
        assert_eq!(header(&shown.report), "2 of 6 met, 1 failing, 3 unknown, 1 waived, 1 proposed, 1 not measured", "the header counts wishes outside met and the proposed failure still fails");
        assert_eq!(brief_label(&shown.report).as_deref(), Some("Brief: Light the workbench"), "the label below the heading shows the brief");
        let wish = &shown.report.wishes[0];
        let (words, controls) = wish_row(wish, shown.wish_at.first());
        assert!(words.title == "warm light" && words.value == "Not measured" && words.value_ink == Ink::Dim && words.detail == "ledctl.req.splash:9",
            "the wish row shows the person's words, its unmeasured status and its source");
        assert!(controls.wish && !controls.offers_waive() && controls.measure.is_none(), "a wish has no waiver or measurement controls");
        assert_eq!(wish_focus(wish).sel, Sel::Source { doc, start: wish.span.start, end: wish.span.end }, "double clicking a wish focuses its own source range");
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
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut editor = cx.with_vm(|vm| {
            makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
            super::script_mod(vm);
            ReqList::script_new_with_default(vm)
        });
        editor.shown = Some(Arc::new(shown.clone()));
        editor.set_brief_open(&mut cx, true);
        let input = editor.view.text_input(&cx, ids!(req_brief_input));
        let saved = editor.view.label(&cx, ids!(req_brief_shown));
        let save = editor.view.button(&cx, ids!(req_brief_save));
        let tip = editor.view.tip(&cx, ids!(req_brief_save_tip));
        assert!(input.text() == "Light the workbench" && !saved.visible(), "opening Brief prefills the saved words and hides the separate label");
        assert!(save.disabled(&cx) && tip.borrow().unwrap().text == "Nothing to save: this is the saved brief",
            "Save explains why it is disabled for the saved words");
        let draft = "  Light the \"bench\".\nKeep it warm.  ";
        input.set_text(&mut cx, draft);
        editor.brief_save_state(&mut cx, draft);
        assert!(!save.disabled(&cx), "editing the saved brief enables Save");
        let actions = cx.capture_actions(|cx| editor.save_brief(cx, false));
        assert!(actions.iter().any(|a| a.as_widget_action().and_then(|a| a.action.downcast_ref::<ReqAction>())
            == Some(&catalog("elec.req.brief", &[("words", draft)]))), "Save emits the person's exact multiline words");
        assert!(save.disabled(&cx) && tip.borrow().unwrap().text == "Saving the brief", "the save stays pending until the host reports its outcome");
        let refusal = editor.view.label(&cx, ids!(req_brief_refusal));
        AddRefused { form: ReqForm::Add(draft.into()), why: "different form".into() }.post(&mut cx);
        editor.take_brief_refusal(&mut cx);
        assert!(save.disabled(&cx) && !refusal.visible(), "an Add refusal for the same words cannot end a brief save");
        AddRefused { form: ReqForm::Brief(draft.into()), why: "document changed".into() }.post(&mut cx);
        editor.sync(&mut cx);
        assert!(refusal.visible() && refusal.text() == "document changed" && !save.disabled(&cx) && input.text() == draft,
            "even when the tab missed the refusal event, its next sync explains the failure and leaves the draft ready to retry");
        editor.shown = Some(Arc::new(shown.clone()));
        cx.capture_actions(|cx| editor.save_brief(cx, false));
        input.set_text(&mut cx, "still typing");
        Arc::make_mut(editor.shown.as_mut().unwrap()).report.brief.as_mut().unwrap().words = draft.into();
        editor.show_brief(&mut cx);
        assert!(!save.disabled(&cx) && input.text() == "still typing" && !refusal.visible(),
            "publication acknowledges the submitted words without overwriting later typing");
        editor.set_brief_open(&mut cx, false);
        assert!(saved.visible() && saved.text() == format!("Brief: {draft}"), "closing the box shows the newly saved brief");
        editor.set_brief_open(&mut cx, true);
        assert_eq!(input.text(), draft, "reopening restores the published words verbatim");

        let mut live = ElecLive::new(Arc::new(crate::snapshot::tests::snapshot(Arc::default())));
        live.requirements = editor.shown.clone();
        cx.set_global(live);
        let draft_button = editor.view.widget(&cx, ids!(req_draft));
        let review = editor.view.widget(&cx, ids!(req_brief_draft_review));
        let state = editor.view.label(&cx, ids!(req_brief_draft_state));
        let draft_tip = editor.view.tip(&cx, ids!(req_draft_tip));
        let click = |editor: &mut ReqList, cx: &mut Cx, button: &WidgetRef| {
            let pressed = cx.capture_actions(|cx| cx.widget_action(button.widget_uid(), ButtonAction::Clicked(Default::default())));
            cx.capture_actions(|cx| editor.brief_actions(cx, &pressed)).iter()
                .filter_map(|a| a.as_widget_action()?.action.downcast_ref::<ReqAction>().cloned()).collect::<Vec<_>>()
        };
        let sent = |actions: &Actions| actions.iter().filter_map(|a| a.as_widget_action()?.action.downcast_ref::<ReqAction>()).any(|a| a == &ReqAction::Draft);
        input.set_text(&mut cx, " ");
        editor.brief_save_state(&mut cx, " ");
        assert!(draft_tip.borrow().unwrap().text == NO_AI_MODEL && editor.view.button(&cx, ids!(req_draft)).disabled(&cx),
            "a missing model disables Draft before the empty-input reason");
        cx.global::<ReqDraft>().admit = Ok(());
        editor.sync(&mut cx);
        assert_eq!(draft_tip.borrow().unwrap().text, "Write the brief first", "an available model needs a nonblank brief");
        input.set_text(&mut cx, draft);
        editor.brief_save_state(&mut cx, draft);
        assert_eq!(click(&mut editor, &mut cx, &draft_button), [ReqAction::Draft], "the saved brief drafts immediately without another save");
        let changed = "Add a power switch.";
        input.set_text(&mut cx, changed);
        editor.brief_save_state(&mut cx, changed);
        let save_first = catalog("elec.req.brief", &[("words", changed)]);
        assert_eq!(click(&mut editor, &mut cx, &draft_button), [save_first.clone()], "a changed brief only sends its save at first");
        cx.global::<ReqDraft>().admit = Err(NO_AI_MODEL);
        editor.sync(&mut cx);
        assert_eq!(draft_tip.borrow().unwrap().text, "Saving the brief", "the pending save takes priority over model availability");
        cx.global::<ReqDraft>().slot = Some("requirements");
        cx.global::<ReqDraft>().view = Some(DraftView { text: "Writing".into(), review: None, done: false });
        editor.sync(&mut cx);
        assert_eq!(draft_tip.borrow().unwrap().text, "Drafting requirements from the brief", "an in-flight draft takes priority over its pending save");
        assert!(state.visible() && state.text() == "Writing" && !review.visible(), "working shows the engine state without a Review button");
        cx.global::<ReqDraft>().slot = Some("architecture");
        editor.sync(&mut cx);
        assert!(!state.visible() && !review.visible() && editor.view.button(&cx, ids!(req_draft)).disabled(&cx),
            "another stage hides the requirements state and Review while keeping Draft busy");
        AddRefused { form: ReqForm::Brief(changed.into()), why: "plate changed".into() }.post(&mut cx);
        assert!(!sent(&cx.capture_actions(|cx| editor.sync(cx))) && refusal.text() == "plate changed" && refusal.visible(),
            "a refused save disarms Draft and shows the refusal");
        let publish = |cx: &mut Cx, words: &str| {
            Arc::make_mut(cx.get_global::<ElecLive>().requirements.as_mut().unwrap()).report.brief.as_mut().unwrap().words = words.into();
        };
        publish(&mut cx, changed);
        assert!(!sent(&cx.capture_actions(|cx| editor.sync(cx))), "later publication of refused words cannot start an abandoned draft");
        *cx.global::<ReqDraft>() = ReqDraft { slot: None, admit: Ok(()), view: None };
        publish(&mut cx, draft);
        editor.sync(&mut cx);
        assert_eq!(click(&mut editor, &mut cx, &draft_button), [save_first], "retrying Draft saves the same brief again");
        publish(&mut cx, "other words");
        assert!(!sent(&cx.capture_actions(|cx| editor.sync(cx))), "a different published brief cannot release Draft");
        publish(&mut cx, changed);
        assert!(sent(&cx.capture_actions(|cx| editor.sync(cx))), "the matched publication sends the armed draft");
        assert!(!sent(&cx.capture_actions(|cx| editor.sync(cx))), "a confirmed save sends Draft only once");
        editor.set_brief_open(&mut cx, false);
        let proposal = ProposalId(42);
        cx.global::<ReqDraft>().slot = Some("requirements");
        cx.global::<ReqDraft>().view = Some(DraftView { text: "Ready: review it in the strip".into(), review: Some(proposal), done: true });
        editor.sync(&mut cx);
        assert!(state.visible() && state.text() == "Ready: review it in the strip" && review.visible(), "the settled proposal stays visible with the brief box closed");
        assert_eq!(click(&mut editor, &mut cx, &review), [ReqAction::Review(proposal)], "Review names the proposal displayed beside it");
        cx.global::<ReqDraft>().view = None;
        editor.sync(&mut cx);
        assert!(!state.visible() && !review.visible(), "removing the reviewed row hides both the state and Review");
        editor.set_brief_open(&mut cx, true);
        input.set_text(&mut cx, draft);
        editor.brief_save_state(&mut cx, draft);
        click(&mut editor, &mut cx, &draft_button);
        ReqDraft::disarm_saves(&cx);
        publish(&mut cx, draft);
        assert!(!sent(&cx.capture_actions(|cx| editor.sync(cx))) && refusal.text() == "The electronics reloaded before drafting started.",
            "identical words on a replacement plate cannot send the previous plate's armed draft");

        editor.set_form(&mut cx, true);
        let picker = editor.view.drop_down(&cx, ids!(template));
        let submit = editor.view.button(&cx, ids!(submit));
        for (index, name) in [(TEMPLATES.len(), "Add this wish"), (1, "Add this requirement")] {
            picker.set_selected_item(&mut cx, index);
            editor.form_fields(&mut cx);
            let mut node = Default::default();
            submit.borrow().unwrap().semantics(&cx, &mut node);
            assert!(node.name == name && editor.view.tip(&cx, ids!(req_submit_tip)).borrow().unwrap().text == name,
                "the chosen template names the submit button and its tooltip {name}");
        }
        // Add on a brief-only plate, before a board unit exists.
        let only = editor.view.label(&cx, ids!(only_unit));
        assert!(editor.units.is_empty(), "the brief alone must leave the unit picker empty");
        for (template, measure) in [(Template::Board("size"), "board.size"), (Template::Case, "case.fits")] {
            editor.set_form(&mut cx, true);
            let at = TEMPLATES.iter().position(|&(_, t)| t == template).unwrap() + 1;
            picker.set_selected_item(&mut cx, at);
            editor.form_fields(&mut cx);
            assert!(only.visible() && only.text() == "Device's only board (when created)", "the {measure} form must explain what a future board will bind to");
            editor.view.text_input(&cx, ids!(req_name)).set_text(&mut cx, "target");
            editor.view.text_input(&cx, ids!(bound)).set_text(&mut cx, "[60mm 40mm]");
            let mut args = vec![("name", "target"), ("measure", measure)];
            if template.has_bound() { args.push(("bound", "{max: [60mm 40mm]}")); }
            let expected = catalog("elec.req.add", &args);
            let actions = cx.capture_actions(|cx| editor.submit(cx));
            assert!(actions.iter().any(|a| a.as_widget_action().and_then(|a| a.action.downcast_ref::<ReqAction>()) == Some(&expected)),
                "the Add form must submit {measure} without making up a unit");
        }
        editor.set_form(&mut cx, true);
        editor.units = vec!["power".into(), "ui".into()];
        picker.set_selected_item(&mut cx, 1);
        editor.form_fields(&mut cx);
        editor.view.text_input(&cx, ids!(req_name)).set_text(&mut cx, "target");
        let actions = cx.capture_actions(|cx| editor.submit(cx));
        assert!(!actions.iter().any(|a| a.as_widget_action().is_some_and(|a| a.action.downcast_ref::<ReqAction>().is_some())),
            "submitting without a choice on a multi-board plate must not send an edit");
        assert_eq!(editor.view.label(&cx, ids!(refusal)).text(), "Choose a board unit for this measure.", "the form must ask which of the two boards to measure");
        let expressions = [
            r#"board("ledctl").size"#, r#"board("ledctl").layers"#, r#"board("ledctl").parts"#,
            r#"board("ledctl").unplaced"#, r#"board("ledctl").unrouted"#,
            r#"checks(@erc, {unit: "ledctl"}).errors"#, r#"checks(@erc, {unit: "ledctl"}).warnings"#,
            r#"case("ledctl").fits"#, r#"bench("bringup")"#, r#"bench("bringup").expect("power")"#,
            "print.time", "print.filament",
        ];
        assert_eq!(TEMPLATES.len(), expressions.len() + 1, "Add offers every measure form and one unmeasured template");
        assert_eq!(TEMPLATES.last(), Some(&("Wish (not measured)", Template::Wish)), "Wish is last in the Add form");
        assert_eq!(Template::Wish.add(" warm light ", None, None, "", "", ""), Ok(catalog("elec.req.wish", &[("words", "warm light")])),
            "the wish template submits only the words, without a measure or invented bound");
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

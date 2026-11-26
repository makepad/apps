//! The Board pane's cached design-rule findings and their shared focus and hover actions.

use super::{checks::label, shared::BoardDraws};
use crate::snapshot::{ElecLive, ViewAction};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{BoardId, NetRef, Sel, Severity};
use fab_elec_board::length::{difference_text, mm_text, LengthReadout};
use fab_elec_design::{Focus, FocusKind, FocusOrigin};
use makepad_widgets_core::*;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Severity totals for settled findings. Unconnected is a subset of errors.
#[derive(Default)]
pub struct FindingCounts {
    pub errors: usize,
    pub unconnected: usize,
    pub warnings: usize,
    pub info: usize,
}

impl FindingCounts {
    pub fn count(&mut self, finding: &Violation) {
        match finding.severity {
            Severity::Error => self.errors += 1,
            Severity::Warning => self.warnings += 1,
            Severity::Info => self.info += 1,
        }
        if finding.kind == CheckKind::Unconnected {
            self.unconnected += 1;
        }
    }
}

// Fixed single-line boxes keep the Fit rows uniform, so FlatList need only draw visible rows.
const TITLE_HEIGHT: f64 = 14.0;
const DETAIL_HEIGHT: f64 = 12.0;
const ROW_PADDING: f64 = 3.0;
const ROW_HEIGHT: f64 = TITLE_HEIGHT + DETAIL_HEIGHT + 2.0 * ROW_PADDING;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.BoardChecksBase = #(BoardChecks::register_widget(vm))
    mod.widgets.BoardChecks = set_type_default() do mod.widgets.BoardChecksBase{
        width: Fill
        height: Fill
        flow: Down
        padding: 6
        show_bg: false

        color_error: theme.color_error
        color_warning: theme.color_warning
        color_info: theme.color_label_inner
        color_muted: theme.color_label_inner_inactive

        header := Label{
            width: Fill
            height: 22
            padding: 0
            align: Align{y: 0.5}
            text: "Checks  all clear"
            max_lines: 1
            text_overflow: Ellipsis
            draw_text +: {
                color: theme.color_label_inner
                text_style: theme.font_bold{font_size: 9}
            }
        }
        findings := FlatList{
            width: Fill
            height: Fill
            flow: Down
            spacing: 0
            drag_scrolling: false
            scroll_bars +: {show_scroll_x: false show_scroll_y: true}

            Lengths := Label{
                width: Fill height: Fit padding: 3
                text: ""
                draw_text +: {color: theme.color_label_inner text_style: theme.font_bold{font_size: 9}}
            }
            Match := SolidView{
                width: Fill height: Fit flow: Down cursor: MouseCursor.Hand
                capture_overload: true grab_key_focus: false
                padding: Inset{top: 3 bottom: 3 left: 6 right: 6}
                draw_bg +: {color: theme.color_u_hidden}
                nets := Label{
                    width: Fill height: Fit padding: 0 flow: Right{wrap: true}
                    draw_text +: {color: theme.color_label_inner text_style: theme.font_bold{font_size: 9}}
                }
                difference := Label{
                    width: Fill height: Fit padding: 0 flow: Right{wrap: true}
                    draw_text +: {color: theme.color_label_inner_inactive text_style: theme.font_regular{font_size: 9}}
                }
            }

            Row := SolidView{
                width: Fill
                height: Fit
                flow: Down
                padding: Inset{top: #(ROW_PADDING) bottom: #(ROW_PADDING) left: 6 right: 6}
                cursor: MouseCursor.Hand
                capture_overload: true
                grab_key_focus: false
                draw_bg +: {
                    hover: instance(0.0)
                    get_color: fn() {
                        return mix(theme.color_u_hidden, theme.color_outset_hover, self.hover)
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

                first := View{
                    width: Fill
                    height: #(TITLE_HEIGHT)
                    flow: Right
                    spacing: 5
                    align: Align{y: 0.5}

                    dot := CircleView{
                        width: 7
                        height: 7
                        draw_bg +: {color: theme.color_label_inner}
                    }
                    title := Label{
                        width: Fill
                        height: Fill
                        padding: 0
                        align: Align{y: 0.5}
                        text: ""
                        max_lines: 1
                        text_overflow: Ellipsis
                        draw_text +: {
                            color: theme.color_label_inner
                            text_style: theme.font_bold{font_size: 9}
                        }
                    }
                    measure := Label{
                        width: Fit
                        height: Fill
                        padding: 0
                        align: Align{x: 1.0 y: 0.5}
                        text: ""
                        max_lines: 1
                        text_overflow: Ellipsis
                        draw_text +: {
                            color: theme.color_label_inner
                            text_style: theme.font_regular{font_size: 9}
                        }
                    }
                }
                detail := Label{
                    width: Fill
                    height: #(DETAIL_HEIGHT)
                    padding: 0
                    align: Align{y: 0.5}
                    text: ""
                    max_lines: 1
                    text_overflow: Ellipsis
                    draw_text +: {
                        color: theme.color_label_inner_inactive
                        text_style: theme.font_regular{font_size: 8}
                    }
                }
            }
        }
    }
}

struct CheckRow {
    id: u64,
    severity: Severity,
    pending: bool,
    title: String,
    measure: String,
    detail: String,
    sels: Vec<Sel>,
}

/// The board's Checks list, registered as `mod.widgets.BoardChecks`.
#[derive(Script, ScriptHook, Widget)]
pub struct BoardChecks {
    #[deref]
    view: View,
    /// The Circuit's Check list: only the circuit's findings (no board location), each row its
    /// message, then the rule's name and how to fix it. Off: the board's Checks.
    #[live]
    circuit: bool,
    /// The header's name before its counts; empty: "Check" for the circuit's, "Checks" for the
    /// board's.
    #[live]
    title: String,
    #[live] color_error: Vec4f,
    #[live] color_warning: Vec4f,
    #[live] color_info: Vec4f,
    #[live] color_muted: Vec4f,
    #[rust] length_count: usize,
    #[rust] lengths_height: f64,
    #[rust] lengths_heading: String,
    #[rust]
    unit: Option<BoardId>,
    #[rust]
    violations: Option<Arc<Vec<Violation>>>,
    #[rust]
    awaiting: Arc<Vec<u64>>,
    #[rust]
    violations_gen: u64,
    #[rust]
    rows_key: Option<(u64, usize, BoardId, bool)>,
    #[rust]
    rows: Vec<CheckRow>,
    /// The header for `rows`, made in the same pass; set on the label each draw, as the rows are,
    /// so the template applied again (a live patch, a theme) cannot leave its default over them.
    #[rust]
    header: String,
    #[rust]
    row_indices: HashMap<LiveId, usize>,
    #[rust]
    hovered: Option<LiveId>,
}

fn row_id(id: u64) -> LiveId {
    LiveId::from_num(0, id)
}

fn kind_title(kind: &CheckKind) -> String {
    let name = format!("{kind:?}");
    // Custom's payload is a rule name, not part of the variant's name.
    let name = name.split('(').next().unwrap_or(&name);
    let mut title = String::new();
    for (i, ch) in name.chars().enumerate() {
        if i == 0 {
            title.push(ch);
        } else {
            if ch.is_uppercase() {
                title.push(' ');
            }
            title.extend(ch.to_lowercase());
        }
    }
    title
}

fn detail_line(violation: &Violation) -> String {
    let text = if violation.fix_hint.is_empty() {
        &violation.message
    } else {
        &violation.fix_hint
    };
    first_line(text)
}

fn first_line(text: &str) -> String {
    let mut chars = text.lines().next().unwrap_or_default().chars();
    let mut line: String = chars.by_ref().take(96).collect();
    if chars.next().is_some() {
        line.push('…');
    }
    line
}

/// The rule's own name, as a reason names it: `PowerNotDriven`.
fn rule_name(kind: &CheckKind) -> String {
    let name = format!("{kind:?}");
    name.split('(').next().unwrap_or(&name).to_string()
}

/// A finding's reason on one line, as the Circuit's status bar says it for the hovered pin, part
/// or net: `VA: no driver (PowerNotDriven) · connect a supply jack ...`.
pub fn reason_line(violation: &Violation) -> String {
    let rule = rule_name(&violation.kind);
    let message = violation.message.lines().next().unwrap_or_default();
    if violation.fix_hint.is_empty() {
        format!("{message} ({rule})")
    } else {
        format!("{message} ({rule}) · {}", violation.fix_hint.lines().next().unwrap_or_default())
    }
}

/// A Check row's lines for a circuit finding: the message (`VA: no driver`), then the rule and
/// the fix (`PowerNotDriven · connect a supply jack ...`).
fn circuit_lines(violation: &Violation) -> (String, String) {
    let rule = rule_name(&violation.kind);
    let detail = if violation.fix_hint.is_empty() { rule } else { format!("{rule} · {}", violation.fix_hint) };
    (first_line(&violation.message), first_line(&detail))
}

fn length_rows(snap: &crate::DesignSnapshot, unit: BoardId, rows: &mut Vec<CheckRow>) -> Option<String> {
    let u = snap.live().unit(unit)?;
    let board = u.board.as_deref()?;
    if board.length_matches.is_empty() { return None; }
    let conn = snap.of(&snap.conn, unit)?;
    let read = LengthReadout::new(board, &u.circuit, &conn.value, &[]);
    let pending = snap.checking(unit) || conn.value.generation != board.generation;
    for (i, group) in board.length_matches.iter().enumerate() {
        let Some(cmp) = group.compare(&read.lengths) else { continue };
        let title = group.nets.iter().map(|n| {
            let length = read.lengths[n];
            let partial = if length.partial { " · partial" } else { "" };
            let branched = if read.branched.contains(n) { " · branched" } else { "" };
            format!("{}  {}{partial}{branched}", board.nets[n.ix()].name, mm_text(length.nm))
        }).collect::<Vec<_>>().join("    ");
        let (state, severity) = if pending {
            ("Checking", Severity::Info)
        } else if !cmp.complete {
            ("Partial", Severity::Info)
        } else if cmp.spread > i64::from(group.tol) {
            ("Over tolerance", Severity::Error)
        } else {
            ("Within tolerance", Severity::Info)
        };
        let reference = &board.nets[cmp.reference.ix()].name;
        let (diff, tol) = difference_text(cmp.spread, group.tol);
        rows.push(CheckRow {
            id: i as u64, title, measure: String::new(), severity, pending,
            detail: format!("{state} · difference {diff} from {reference} · tolerance {tol}"),
            sels: group.nets.iter().map(|&net| Sel::Net(NetRef { board: unit, net })).collect(),
        });
    }
    Some(format!("Lengths · {}", read.measurement.text()))
}

impl BoardChecks {
    fn ink(&self, row: &CheckRow) -> Vec4f {
        if row.pending { return self.color_muted }
        match row.severity {
            Severity::Error => self.color_error,
            Severity::Warning => self.color_warning,
            Severity::Info => self.color_info,
        }
    }

    /// The height the list takes to show every row, header and padding included.
    pub fn full_height(&self) -> f64 {
        22.0 + 12.0 + self.lengths_height + (self.rows.len() - self.length_count) as f64 * ROW_HEIGHT
    }

    /// Shows findings for this board unit; `None` clears the list.
    pub fn set_unit(&mut self, cx: &mut Cx, unit: Option<BoardId>) {
        self.unit = unit;
        self.sync_rows(cx);
    }

    /// Sets findings for hosts without `ElecLive`; the host snapshot takes precedence.
    /// `awaiting` is the sorted ids of `v` that await a zone's refill, shown as pending; empty for none.
    /// Each call advances the generation; replaced findings and awaiting ids are freed on the pool.
    pub fn set_violations(&mut self, cx: &mut Cx, v: Option<Arc<Vec<Violation>>>, awaiting: Arc<Vec<u64>>) {
        if let Some(old) = std::mem::replace(&mut self.violations, v) {
            BoardDraws::discard(cx, old);
        }
        BoardDraws::discard(cx, std::mem::replace(&mut self.awaiting, awaiting));
        self.violations_gen = self.violations_gen.wrapping_add(1);
        self.sync_rows(cx);
    }

    fn violations(&self, cx: &mut Cx) -> Option<(u64, Arc<Vec<Violation>>, Arc<Vec<u64>>)> {
        if cx.has_global::<ElecLive>() {
            let snap = &cx.get_global::<ElecLive>().snap;
            return Some((snap.violations.generation, snap.violations.value.clone(), snap.awaiting.clone()));
        }
        self.violations.as_ref().map(|v| (self.violations_gen, v.clone(), self.awaiting.clone()))
    }

    // Filtering, ordering and text preparation happen only on a generation, awaiting, unit or
    // checking change.
    fn sync_rows(&mut self, cx: &mut Cx) {
        let violations = self.violations(cx);
        // The unit's board findings come with its whole-board checks (none are listed before them).
        let checking = self.unit.is_some_and(|unit| cx.get_global_ref::<ElecLive>().is_some_and(|live| live.snap.checking(unit)));
        let key = self
            .unit
            .zip(violations.as_ref())
            .map(|(unit, (generation, _, awaiting))| (*generation, Arc::as_ptr(awaiting) as usize, unit, checking));
        if self.rows_key == key {
            return;
        }
        let mut rows = Vec::new();
        if !self.circuit {
            if let (Some(unit), Some(live)) = (self.unit, cx.get_global_ref::<ElecLive>()) {
                self.lengths_heading = length_rows(&live.snap, unit, &mut rows).unwrap_or_default();
            }
        }
        self.length_count = rows.len();
        self.lengths_height = if rows.is_empty() { 0.0 } else { 20.0 + rows.len() as f64 * ROW_HEIGHT };
        let mut totals = FindingCounts::default();
        let mut pending = 0;
        if let (Some(unit), Some((_, violations, awaiting))) = (self.unit, violations.as_ref()) {
            let circuit = self.circuit;
            let mut findings: Vec<_> = violations.iter()
                .filter(|v| v.unit == Some(unit) && (!circuit || v.at.is_none())).collect();
            findings.sort_unstable_by(|a, b| {
                let length = |v: &Violation| matches!(v.kind, CheckKind::Skew | CheckKind::Length);
                (Reverse(a.severity), Reverse(length(a)), &a.kind, a.id).cmp(&(Reverse(b.severity), Reverse(length(b)), &b.kind, b.id))
            });
            let mut pending_id = None;
            let mut unconnected_finding = None;
            for violation in findings {
                if awaiting.binary_search(&violation.id).is_ok() {
                    pending += 1;
                    pending_id.get_or_insert(violation.id);
                    continue;
                }
                totals.count(violation);
                if violation.kind == CheckKind::Unconnected {
                    unconnected_finding.get_or_insert(violation);
                    continue;
                }
                let (title, detail) = if circuit {
                    circuit_lines(violation)
                } else {
                    (kind_title(&violation.kind), if matches!(violation.kind, CheckKind::Skew | CheckKind::Length) {
                        violation.message.clone()
                    } else { detail_line(violation) })
                };
                rows.push(CheckRow {
                    id: violation.id,
                    severity: violation.severity,
                    pending: false,
                    title,
                    measure: if !circuit && (violation.actual.is_some() || violation.required.is_some()) {
                        label(violation)
                    } else {
                        String::new()
                    },
                    detail,
                    sels: if violation.kind == CheckKind::Skew { violation.items.clone() }
                        else { violation.items.first().cloned().into_iter().collect() },
                });
            }
            if let Some(id) = pending_id {
                rows.push(CheckRow {
                    id,
                    severity: Severity::Info,
                    pending: true,
                    title: format!("{pending} waiting for the pour"),
                    measure: String::new(),
                    detail: "Rechecked when the pour refills".into(),
                    sels: Vec::new(),
                });
            }
            if let Some(violation) = unconnected_finding {
                rows.push(CheckRow {
                    id: violation.id,
                    severity: violation.severity,
                    pending: false,
                    title: format!("{} unconnected", totals.unconnected),
                    measure: String::new(),
                    detail: "Shown in the board's ratsnest".into(),
                    sels: Vec::new(),
                });
            }
        }
        let mut counts = Vec::new();
        for (count, singular, plural) in [
            (totals.errors - totals.unconnected, "error", "errors"),
            (totals.warnings, "warning", "warnings"),
            (totals.info, "info", "info"),
            (pending, "pending", "pending"),
        ] {
            if count != 0 {
                counts.push(format!("{count} {}", if count == 1 { singular } else { plural }));
            }
        }
        // The ratsnest draws each one; the header still says the board is not done.
        if totals.unconnected != 0 {
            counts.push(format!("{} unconnected", totals.unconnected));
        }
        if checking {
            counts.push("checking…".into());
        }
        let title = if !self.title.is_empty() { self.title.as_str() } else if self.circuit { "Check" } else { "Checks" };
        let header = if counts.is_empty() {
            format!("{title}  all clear")
        } else {
            format!("{title}  {}", counts.join(" · "))
        };
        self.view.label(cx, ids!(header)).set_text(cx, &header);
        self.header = header;
        let indices = rows.iter().enumerate().map(|(i, row)| {
            (if i < self.length_count { LiveId::from_num(1, row.id) } else { row_id(row.id) }, i)
        }).collect();
        let old_rows = std::mem::replace(&mut self.rows, rows);
        let old_indices = std::mem::replace(&mut self.row_indices, indices);
        BoardDraws::discard(cx, (old_rows, old_indices));
        self.rows_key = key;
        if self.hovered.take().is_some() {
            cx.widget_action(self.widget_uid(), ViewAction::Hover(None));
        }
        self.view.redraw(cx);
    }
}

impl Widget for BoardChecks {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Signal) {
            self.sync_rows(cx);
        }
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        let list = self.view.flat_list(cx, ids!(findings));
        let uid = self.widget_uid();
        let mut seen = HashSet::new();
        for (id, item) in list.items_with_actions(&actions) {
            if !seen.insert(id) {
                continue;
            }
            let Some(finding) = self.row_indices.get(&id).and_then(|&i| self.rows.get(i)) else {
                continue;
            };
            let row = item.as_view();
            if row.finger_hover_out(&actions).is_some() && self.hovered == Some(id) {
                self.hovered = None;
                cx.widget_action(uid, ViewAction::Hover(None));
            }
            let Some(sel) = finding.sels.first() else { continue };
            if row.finger_hover_in(&actions).is_some() {
                self.hovered = Some(id);
                cx.widget_action(uid, ViewAction::Hover(Some(sel.clone())));
            }
            if row.finger_down(&actions).is_some() {
                let kind = match sel {
                    Sel::Comp(_) => FocusKind::Part,
                    Sel::Net(_) => FocusKind::Net,
                    Sel::Pin(_) => FocusKind::Pin,
                    _ => FocusKind::Board,
                };
                cx.widget_action(
                    uid,
                    ViewAction::Focus(Focus {
                        kind,
                        sel: sel.clone(),
                        origin: FocusOrigin::Checks,
                    }),
                );
                if finding.sels.len() > 1 {
                    cx.widget_action(uid, crate::ViewState { selected: finding.sels.clone(), emphasis: vec![] });
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // a no-op unless the label lost the text (set_text skips equal text)
        if !self.header.is_empty() {
            self.view.label(cx, ids!(header)).set_text(cx, &self.header);
        }
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<FlatList>() else {
                continue;
            };
            if self.length_count != 0 {
                let start = cx.turtle().pos().y;
                if let Some(title) = list.item(cx, id!(lengths_heading), id!(Lengths)) {
                    title.set_text(cx, &self.lengths_heading);
                    title.draw_all(cx, scope);
                }
                for row in &self.rows[..self.length_count] {
                    if let Some(item) = list.item(cx, LiveId::from_num(1, row.id), id!(Match)) {
                        let nets = item.label(cx, ids!(nets));
                        nets.set_text(cx, &row.title);
                        nets.set_text_color(cx, self.ink(row));
                        item.label(cx, ids!(difference)).set_text(cx, &row.detail);
                        item.draw_all(cx, scope);
                    }
                }
                self.lengths_height = cx.turtle().pos().y - start;
            }
            let findings = &self.rows[self.length_count..];
            let origin = cx.turtle().pos().y;
            let (top, bottom) = cx.turtle_visible_y_range();
            let start = (((top - origin) / ROW_HEIGHT).floor().max(0.0) as usize).min(findings.len());
            let end = (((bottom - origin) / ROW_HEIGHT).ceil().max(0.0) as usize)
                .min(findings.len())
                .max(start);
            if start != 0 {
                cx.walk_turtle(Walk {
                    width: Size::fill(),
                    height: Size::Fixed(start as f64 * ROW_HEIGHT),
                    ..Walk::default()
                });
            }
            for finding in &findings[start..end] {
                let Some(row) = list.item(cx, row_id(finding.id), id!(Row)) else {
                    continue;
                };
                row.label(cx, ids!(first.title)).set_text(cx, &finding.title);
                let measure = row.label(cx, ids!(first.measure));
                measure.set_text(cx, &finding.measure);
                let ink = self.ink(finding);
                measure.set_text_color(cx, ink);
                if let Some(mut dot) = row.view(cx, ids!(first.dot)).borrow_mut() {
                    dot.draw_bg.set_dyn_instance(cx, id!(color), &[ink.x, ink.y, ink.z, ink.w]);
                }
                row.label(cx, ids!(detail)).set_text(cx, &finding.detail);
                row.draw_all(cx, scope);
            }
            if end != findings.len() {
                cx.walk_turtle(Walk {
                    width: Size::fill(),
                    height: Size::Fixed((findings.len() - end) as f64 * ROW_HEIGHT),
                    ..Walk::default()
                });
            }
            if self.hovered.is_some_and(|id| !list.items.is_visible(&id)) {
                self.hovered = None;
                cx.widget_action(self.widget_uid(), ViewAction::Hover(None));
            }
            list.items.retain_visible();
        }
        DrawStep::done()
    }
}

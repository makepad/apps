//! The Calibrate panel's state and actions: the guided steps (fits and friction for
//! Mechanics, then flow, temperature and pressure advance, then the print head), each making
//! a test part the app slices and shows in the Slice render mode. Printing it is the Printer panel's Print
//! button and its confirm, never this panel. The app fills the state; the panel only shows
//! it and sends [`CalibrateAction`]s.

use crate::api::AppState;
use makepad_widgets_core::*;

/// How a step's result is entered.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum AnswerKind {
    /// One variant number (1..=n).
    #[default]
    Number,
    /// Two variant numbers with their questions (slip / press, went in / held).
    Two(&'static str, &'static str),
    /// An angle in degrees.
    Angle,
    /// Measured by hand, entered elsewhere (the head shape).
    Measure,
}

/// One step as the list shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalStepRow {
    pub title: String,
    /// What the result feeds ("Mechanics: ...", "The slicer: ...").
    pub feeds: String,
    /// The date it was done for this printer and filament.
    pub done: Option<String>,
    /// The stored values, said short ("0.30 mm", "2 layers", "mu 0.32").
    pub summary: String,
    /// The step cannot run yet, and why (shown instead of Make).
    pub blocked: Option<String>,
}

/// The made test part of the selected step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CouponView {
    pub name: String,
    /// What to check on the printed part and what to enter.
    pub check: String,
    /// "1: 0.10 mm", ... in number order.
    pub variants: Vec<String>,
    pub answer: AnswerKind,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalibrateState {
    pub printer: String,
    pub filament: String,
    pub steps: Vec<CalStepRow>,
    pub selected: usize,
    /// The selected step's test part once made (it is the plate until something else is).
    pub coupon: Option<CouponView>,
    /// The last thing that happened ("Saved: clearance 0.30 mm for Mechanics", an error ...).
    pub status: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CalibrateAction {
    Select(usize),
    /// Make the selected step's test part: the plate becomes it and it slices (nothing prints).
    MakePart,
    /// The user's result for the made part. `a` / `b` are variant numbers (b only for
    /// [`AnswerKind::Two`]); `angle` in degrees for [`AnswerKind::Angle`].
    Answer { a: usize, b: usize, angle: f64 },
    /// Forget the selected step's stored result for this printer and filament.
    Clear,
}

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    let AnswerField = View{
        width: Fill height: Fit flow: Down spacing: 4
        label := mod.widgets.FabLabelDim{ width: Fill text: "Number" }
        input := mod.widgets.FabInput{ is_numeric_only: true empty_text: "Number" }
    }

    mod.widgets.FabCalibratePanelBase = #(FabCalibratePanel::register_widget(vm))
    mod.widgets.FabCalibratePanel = set_type_default() do mod.widgets.FabCalibratePanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        body := mod.widgets.FabScroll{
            padding: Inset{left: 10 right: 10 top: 10 bottom: 10}
            spacing: 10
            head := mod.widgets.FabCard{
                mod.widgets.FabTitleLabel{ text: "Calibrate" }
                context := mod.widgets.FabLabelDim{ width: Fill text: "Choose a printer and filament in Settings" }
            }
            selected := mod.widgets.FabCard{
                visible: false
                title := mod.widgets.FabTitleLabel{ width: Fill text: "" }
                feeds := mod.widgets.FabLabelDim{ width: Fill text: "" }
                blocked := mod.widgets.FabLabelDim{ width: Fill text: "" visible: false }
                make := View{
                    width: Fill height: Fit flow: Down spacing: 6
                    FabTip{ text: "Make and slice the selected step's test part, then show its slice"
                        part := mod.widgets.FabButton{ text: "Make test part" }
                    }
                    mod.widgets.FabLabelDim{ width: Fill text: "The part slices and its print shows. It prints only when you press Print and confirm." }
                }
            }
            coupon := mod.widgets.FabCard{
                visible: false
                name := mod.widgets.FabTitleLabel{ width: Fill text: "" }
                check := mod.widgets.FabLabel{ width: Fill text: "" }
                variants := mod.widgets.FabLabelDim{ width: Fill text: "" }
                first := AnswerField{}
                second := AnswerField{ visible: false }
                error := mod.widgets.FabLabel{ width: Fill visible: false text: "" draw_text +: { color: fab.color_error } }
                FabTip{ text: "Save the result for this printer and filament"
                    save := mod.widgets.FabButton{ text: "Save result" }
                }
            }
            clear_result := FabTip{
                visible: false
                text: "Forget the selected step's saved result for this printer and filament"
                clear := mod.widgets.FabFlatButton{ text: "Clear result" draw_text +: { text_style: theme.font_regular{ font_size: fab.font_size_small } } }
            }
            status := mod.widgets.FabLabelDim{ width: Fill text: "" }
            mod.widgets.FabLabelDim{ text: "STEPS" }
            steps := FlatList{
                width: Fill height: Fit flow: Down
                scroll_bars +: { show_scroll_x: false show_scroll_y: false }
                Step := View{
                    width: Fill height: Fit flow: Right spacing: 8
                    padding: Inset{left: 10 right: 10 top: 7 bottom: 7}
                    cursor: MouseCursor.Hand
                    show_bg: true
                    draw_bg +: {
                        selected: instance(0.0)
                        hover: instance(0.0)
                        pixel: fn() {
                            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                            sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                            sdf.fill(fab.color_editor.mix(fab.color_row_hover, self.hover * 0.6).mix(fab.color_selection_bg, self.selected))
                            sdf.rect(0.0, 0.0, 2.0 * self.selected, self.rect_size.y)
                            sdf.fill(fab.color_accent)
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
                    text := View{
                        width: Fill height: Fit
                        title := mod.widgets.FabLabel{ width: Fill text: "" }
                    }
                    result := View{
                        width: Fit height: Fit flow: Right spacing: 6 visible: false
                        summary := mod.widgets.FabLabel{ width: Fit text: "" }
                        date := mod.widgets.FabLabelDim{ width: Fit text: "" }
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabCalibratePanel {
    #[deref]
    view: View,
    /// Preserve drafts across redraws, but never carry an answer into another step or setup.
    #[rust]
    shown: Option<(String, String, usize, Option<CouponView>)>,
    #[rust]
    error: String,
}

impl FabCalibratePanel {
    fn sync_form(&mut self, cx: &mut Cx, state: &CalibrateState) -> bool {
        let changed = self.shown.as_ref().is_none_or(|(printer, filament, selected, coupon)| {
            printer != &state.printer || filament != &state.filament || *selected != state.selected || coupon != &state.coupon
        });
        if changed {
            self.shown = Some((state.printer.clone(), state.filament.clone(), state.selected, state.coupon.clone()));
            self.view.text_input(cx, ids!(body.coupon.first.input)).set_text(cx, "");
            self.view.text_input(cx, ids!(body.coupon.second.input)).set_text(cx, "");
            self.error.clear();
        }
        changed
    }

    fn sync(&mut self, cx: &mut Cx, state: &CalibrateState) {
        self.sync_form(cx, state);
        let v = &self.view;
        let context = if state.printer.is_empty() || state.filament.is_empty() {
            "Choose a printer and filament in Settings".to_string()
        } else {
            format!("{} · {}", state.printer, state.filament)
        };
        v.label(cx, ids!(body.head.context)).set_text(cx, &context);
        let step = state.steps.get(state.selected);
        let selected = v.view(cx, ids!(body.selected));
        selected.set_visible(cx, step.is_some());
        if let Some(step) = step {
            selected.label(cx, ids!(title)).set_text(cx, &step.title);
            selected.label(cx, ids!(feeds)).set_text(cx, &step.feeds);
            selected.label(cx, ids!(blocked)).set_visible(cx, step.blocked.is_some());
            selected.label(cx, ids!(blocked)).set_text(cx, step.blocked.as_deref().unwrap_or(""));
            selected.view(cx, ids!(make)).set_visible(cx, step.blocked.is_none());
        }
        v.widget(cx, ids!(body.clear_result)).set_visible(cx, step.is_some_and(|step| step.done.is_some()));
        let coupon = v.view(cx, ids!(body.coupon));
        coupon.set_visible(cx, state.coupon.is_some());
        if let Some(part) = &state.coupon {
            coupon.label(cx, ids!(name)).set_text(cx, &part.name);
            coupon.label(cx, ids!(check)).set_text(cx, &part.check);
            coupon.label(cx, ids!(variants)).set_text(cx, &part.variants.join("\n"));
            coupon.label(cx, ids!(variants)).set_visible(cx, !part.variants.is_empty());
            let (first, second) = match part.answer {
                AnswerKind::Number => ("Number", ""),
                AnswerKind::Two(q1, q2) => (q1, q2),
                AnswerKind::Angle => ("Degrees", ""),
                AnswerKind::Measure => ("", ""),
            };
            coupon.view(cx, ids!(first)).set_visible(cx, part.answer != AnswerKind::Measure);
            coupon.view(cx, ids!(second)).set_visible(cx, matches!(part.answer, AnswerKind::Two(..)));
            for (label, input, text) in [(ids!(first.label), ids!(first.input), first), (ids!(second.label), ids!(second.input), second)] {
                coupon.label(cx, label).set_text(cx, text);
                let input = coupon.text_input(cx, input);
                if input.empty_text() != text {
                    input.set_empty_text(cx, text.to_string());
                }
            }
        }
        coupon.label(cx, ids!(error)).set_text(cx, &self.error);
        coupon.label(cx, ids!(error)).set_visible(cx, !self.error.is_empty());
        v.label(cx, ids!(body.status)).set_text(cx, &state.status);
    }

    fn answer(&self, cx: &mut Cx, coupon: &CouponView) -> Result<CalibrateAction, String> {
        let first = self.view.text_input(cx, ids!(body.coupon.first.input)).text();
        let second = self.view.text_input(cx, ids!(body.coupon.second.input)).text();
        let number = |text: &str, label: &str| {
            let range = if coupon.variants.is_empty() { "greater than zero".to_string() } else { format!("from 1 to {}", coupon.variants.len()) };
            text.trim().parse::<usize>().ok()
                .filter(|&n| n > 0 && (coupon.variants.is_empty() || n <= coupon.variants.len()))
                .ok_or_else(|| format!("{label}: enter a whole number {range}."))
        };
        let (a, b, angle) = match coupon.answer {
            AnswerKind::Number => (number(&first, "Number")?, 0, 0.0),
            AnswerKind::Two(q1, q2) => (number(&first, q1)?, number(&second, q2)?, 0.0),
            AnswerKind::Angle => {
                let angle = first.trim().parse::<f64>().ok().filter(|v| v.is_finite())
                    .ok_or_else(|| "Degrees: enter a finite number.".to_string())?;
                (0, 0, angle)
            }
            AnswerKind::Measure => (0, 0, 0.0),
        };
        Ok(CalibrateAction::Answer { a, b, angle })
    }
}

impl Widget for FabCalibratePanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Calibrate");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some(state) = scope.data.get::<AppState>() else { return };
        let state = &state.print.calibrate;
        // An action from an old form must not save against a newly selected step/setup.
        if self.sync_form(cx, state) {
            self.view.redraw(cx);
            return;
        }
        let list = self.view.flat_list(cx, ids!(body.steps));
        for (id, item) in list.items_with_actions(actions) {
            let i = id.0 as usize - 1;
            if i < state.steps.len() && item.as_view().finger_up(actions).is_some_and(|up| up.is_over && !up.cancelled) {
                cx.action(CalibrateAction::Select(i));
                return;
            }
        }
        let step = state.steps.get(state.selected);
        if self.view.button(cx, ids!(body.selected.make.part)).clicked(actions) && step.is_some_and(|step| step.blocked.is_none()) {
            cx.action(CalibrateAction::MakePart);
        }
        if self.view.text_input(cx, ids!(body.coupon.first.input)).changed(actions).is_some()
            || self.view.text_input(cx, ids!(body.coupon.second.input)).changed(actions).is_some()
        {
            self.error.clear();
            self.view.redraw(cx);
        }
        if self.view.button(cx, ids!(body.coupon.save)).clicked(actions) {
            if let Some(coupon) = &state.coupon {
                match self.answer(cx, coupon) {
                    Ok(answer) => {
                        self.error.clear();
                        cx.action(answer);
                    }
                    Err(error) => self.error = error,
                }
                self.view.redraw(cx);
            }
        }
        if self.view.button(cx, ids!(body.clear_result.clear)).clicked(actions) && step.is_some_and(|step| step.done.is_some()) {
            self.shown = None;
            cx.action(CalibrateAction::Clear);
            self.view.redraw(cx);
        }
        if actions.iter().any(|a| a.downcast_ref::<CalibrateAction>().is_some()) {
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            self.sync(cx, &state.print.calibrate);
        }
        // FlatList fills the column in place; the outer FabScroll owns all scrolling.
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<FlatList>() else { continue };
            if let Some(state) = scope.data.get::<AppState>() {
                let state = &state.print.calibrate;
                for (i, row) in state.steps.iter().enumerate() {
                    let Some(item) = list.item(cx, LiveId(i as u64 + 1), live_id!(Step)) else { continue };
                    let selected = if i == state.selected { 1.0 } else { 0.0 };
                    if let Some(mut item) = item.borrow_mut::<View>() {
                        item.draw_bg.set_dyn_instance(cx, live_id!(selected), &[selected]);
                    }
                    item.label(cx, ids!(text.title)).set_text(cx, &row.title);
                    item.view(cx, ids!(result)).set_visible(cx, row.done.is_some());
                    if let Some(date) = &row.done {
                        item.label(cx, ids!(result.date)).set_text(cx, &format!("✓ {date}"));
                        item.label(cx, ids!(result.summary)).set_text(cx, &row.summary);
                    }
                    item.draw_all(cx, &mut Scope::empty());
                }
            }
            list.items.retain_visible();
        }
        DrawStep::done()
    }
}

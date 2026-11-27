//! Calibration, in the Printers editor's section of the printer in use: the guided steps (fits
//! and friction for Mechanics, then flow, temperature and pressure advance, then the print head),
//! each a section of its own that folds ([`FabCalibrateSteps`]). A step's section holds all of
//! it: what it feeds, Make test part (the app slices the part and shows it in the Slice render
//! mode), then that part's check and the answer to save, and Clear result once done. Printing
//! the part is the Print button's and its confirm, never this. The app fills the state; the
//! steps only show it and send [`CalibrateAction`]s.

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

    // The steps, each a nested section that folds (as the Part panel's parameter groups): its
    // title, the date it was done in the header's trail; in its body what it feeds, Make test part,
    // the made part's check and answer, Clear result.
    mod.widgets.FabCalibrateStepsBase = #(FabCalibrateSteps::register_widget(vm))
    mod.widgets.FabCalibrateSteps = set_type_default() do mod.widgets.FabCalibrateStepsBase{
        width: Fill height: Fit flow: Down
        Step := PropSection{
            margin: Inset{}
            header +: {hdr +: {
                margin: Inset{left: 10}
                padding: Inset{left: theme.prop_label_gap right: theme.prop_pad}
                title +: {text: ""}
                trail +: {
                    width: Fit height: Fit flow: Right align: Align{y: 0.5}
                    done := mod.widgets.FabLabelDim{ text: "" }
                }
            }}
            body +: {
                flow: Down spacing: 6 padding: Inset{left: 10 top: 6 bottom: 8}
                feeds := mod.widgets.FabLabelDim{ width: Fill text: "" }
                result := mod.widgets.FabLabel{ width: Fill text: "" visible: false }
                blocked := mod.widgets.FabLabelDim{ width: Fill text: "" visible: false }
                make := View{
                    width: Fill height: Fit flow: Down spacing: 6
                    FabTip{ text: "Make and slice this step's test part, then show its slice"
                        part := mod.widgets.FabButton{ text: "Make test part" }
                    }
                    mod.widgets.FabLabelSmall{ width: Fill text: "The part slices and its print shows. It prints only when you press Print and confirm." }
                }
                // the made part: what to check on the print, and the answer
                coupon := View{
                    visible: false
                    width: Fill height: Fit flow: Down spacing: 6
                    name := mod.widgets.FabLabel{ width: Fill text: "" draw_text +: { text_style: theme.font_bold{ font_size: fab.font_size_ui } } }
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
                    text: "Forget this step's saved result for this printer and filament"
                    clear := mod.widgets.FabFlatButton{ text: "Clear result" draw_text +: { text_style: theme.font_regular{ font_size: fab.font_size_small } } }
                }
            }
        }
    }
}

/// The calibration steps as sections, made from the `Step` template, one per step and kept.
/// One step is the app's selected one: its made part and its answer show in its section; Make in
/// another step selects it first.
#[derive(Script, Widget)]
pub struct FabCalibrateSteps {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[layout] layout: Layout,
    #[rust] templates: std::collections::HashMap<LiveId, ScriptObjectRef>,
    #[rust] items: ComponentMap<LiveId, WidgetRef>,
    /// The steps drawn last.
    #[rust] count: usize,
    /// Preserve drafts across redraws, but never carry an answer into another step or setup.
    #[rust]
    shown: Option<(String, String, usize, Option<CouponView>)>,
    #[rust]
    error: String,
    #[redraw] #[rust] area: Area,
}

impl ScriptHook for FabCalibrateSteps {
    fn on_before_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            self.templates.clear();
        }
    }

    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if let (Some(id), Some(t)) = (kv.key.as_id(), kv.value.as_object()) {
                            self.templates.insert(id, vm.new_object_ref(t));
                        }
                    }
                });
            }
        }
        if apply.is_reload() {
            if let Some(template) = self.templates.get(&live_id!(Step)) {
                let v: ScriptValue = template.as_object().into();
                for (_, w) in self.items.iter_mut() {
                    w.script_apply(vm, apply, scope, v);
                }
            }
        }
    }
}

fn step_id(i: usize) -> LiveId {
    LiveId(i as u64 + 1)
}

impl FabCalibrateSteps {
    /// The steps' sections, in order: their headers for the editor's swipe-fold.
    pub fn headers(&self) -> Vec<WidgetRef> {
        (0..self.count).filter_map(|i| self.items.get(&step_id(i)).cloned()).collect()
    }

    fn item(&mut self, cx: &mut Cx, i: usize) -> Option<WidgetRef> {
        let id = step_id(i);
        if let Some(w) = self.items.get(&id) {
            return Some(w.clone());
        }
        let value: ScriptValue = self.templates.get(&live_id!(Step))?.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        cx.widget_tree_insert_child(self.uid, id, w.clone());
        // a step starts folded; the selected one opens (`sync`)
        w.as_prop_section().set_is_open(cx, false, Animate::No);
        self.items.insert(id, w.clone());
        Some(w)
    }

    /// The selected step's form, emptied when the step, the setup or its part changes. Whether it did.
    fn sync_form(&mut self, cx: &mut Cx, state: &CalibrateState) -> bool {
        let changed = self.shown.as_ref().is_none_or(|(printer, filament, selected, coupon)| {
            printer != &state.printer || filament != &state.filament || *selected != state.selected || coupon != &state.coupon
        });
        if changed {
            let opened = self.shown.as_ref().is_none_or(|(_, _, selected, coupon)| *selected != state.selected || (coupon.is_none() && state.coupon.is_some()));
            self.shown = Some((state.printer.clone(), state.filament.clone(), state.selected, state.coupon.clone()));
            if let Some(w) = self.items.get(&step_id(state.selected)).cloned() {
                w.text_input(cx, ids!(coupon.first.input)).set_text(cx, "");
                w.text_input(cx, ids!(coupon.second.input)).set_text(cx, "");
                // the selected step opens (a new selection, or its part just made)
                if opened {
                    w.as_prop_section().set_is_open(cx, true, Animate::Yes);
                }
            }
            self.error.clear();
        }
        changed
    }

    fn sync_step(&self, cx: &mut Cx, w: &WidgetRef, i: usize, row: &CalStepRow, state: &CalibrateState) {
        w.label(cx, ids!(hdr.title)).set_text(cx, &row.title);
        w.label(cx, ids!(done)).set_text(cx, &row.done.as_ref().map(|d| format!("✓ {d}")).unwrap_or_default());
        w.label(cx, ids!(feeds)).set_text(cx, &row.feeds);
        w.label(cx, ids!(result)).set_visible(cx, row.done.is_some() && !row.summary.is_empty());
        w.label(cx, ids!(result)).set_text(cx, &row.summary);
        w.label(cx, ids!(blocked)).set_visible(cx, row.blocked.is_some());
        w.label(cx, ids!(blocked)).set_text(cx, row.blocked.as_deref().unwrap_or(""));
        w.view(cx, ids!(make)).set_visible(cx, row.blocked.is_none());
        w.widget(cx, ids!(clear_result)).set_visible(cx, row.done.is_some());
        let part = state.coupon.as_ref().filter(|_| i == state.selected);
        let coupon = w.view(cx, ids!(coupon));
        coupon.set_visible(cx, part.is_some());
        let Some(part) = part else { return };
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
        coupon.label(cx, ids!(error)).set_text(cx, &self.error);
        coupon.label(cx, ids!(error)).set_visible(cx, !self.error.is_empty());
    }

    fn answer(&self, cx: &mut Cx, w: &WidgetRef, coupon: &CouponView) -> Result<CalibrateAction, String> {
        let first = w.text_input(cx, ids!(coupon.first.input)).text();
        let second = w.text_input(cx, ids!(coupon.second.input)).text();
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

impl Widget for FabCalibrateSteps {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Calibrate");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Draw(_)) { return; }
        // (their headers' hover and swipe-fold are the Printers editor's: `headers`)
        let steps = self.headers();
        for w in &steps {
            w.handle_event(cx, event, scope);
        }
        let Event::Actions(actions) = event else { return };
        let Some(state) = scope.data.get::<AppState>() else { return };
        let state = &state.print.calibrate;
        // An action from an old form must not save against a newly selected step/setup.
        if self.sync_form(cx, state) {
            self.area.redraw(cx);
            return;
        }
        for (i, w) in steps.iter().enumerate() {
            let Some(row) = state.steps.get(i) else { continue };
            // Make: this step selected first, then its part
            if w.button(cx, ids!(make.part)).clicked(actions) && row.blocked.is_none() {
                if i != state.selected {
                    cx.action(CalibrateAction::Select(i));
                }
                cx.action(CalibrateAction::MakePart);
            }
            if w.button(cx, ids!(clear_result.clear)).clicked(actions) && row.done.is_some() {
                if i != state.selected {
                    cx.action(CalibrateAction::Select(i));
                }
                self.shown = None;
                cx.action(CalibrateAction::Clear);
                self.area.redraw(cx);
            }
        }
        let Some(w) = steps.get(state.selected) else { return };
        if w.text_input(cx, ids!(coupon.first.input)).changed(actions).is_some()
            || w.text_input(cx, ids!(coupon.second.input)).changed(actions).is_some()
        {
            self.error.clear();
            self.area.redraw(cx);
        }
        if w.button(cx, ids!(coupon.save)).clicked(actions) {
            if let Some(coupon) = &state.coupon {
                match self.answer(cx, w, coupon) {
                    Ok(answer) => {
                        self.error.clear();
                        cx.action(answer);
                    }
                    Err(error) => self.error = error,
                }
                self.area.redraw(cx);
            }
        }
        if actions.iter().any(|a| a.downcast_ref::<CalibrateAction>().is_some()) {
            self.area.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, self.layout);
        if let Some(app) = scope.data.get::<AppState>() {
            let state = &app.print.calibrate;
            // the steps first, so the form's reset finds the selected one
            for i in 0..state.steps.len() {
                self.item(cx, i);
            }
            self.count = state.steps.len();
            self.sync_form(cx, state);
            for (i, row) in state.steps.iter().enumerate() {
                let Some(w) = self.items.get(&step_id(i)).cloned() else { continue };
                self.sync_step(cx, &w, i, row, state);
                w.draw_all(cx, &mut Scope::empty());
            }
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

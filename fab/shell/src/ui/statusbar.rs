//! The window's status bar, along its foot under the dock and the chat: everything that comes
//! and goes, in one place rather than over the views and in the caption.
//!
//! - at its left, the newest notice (`UiState::notices`, of any view; a standing one, a
//!   problem in the way, in the warning ink), else the tool's one-line hint;
//! - the slice's state while it is worth saying: slicing with its progress, a failure, and
//!   while the slice shows why there is none yet or that printability is being checked;
//! - the job strip: the newest long job, its list on a press (and the save's status while the
//!   app quits: `quit_status`);
//! - at its right, what is on the plate (its parts and solids).
//!
//! It follows the state after each event (`FabStatusBar::handle_event`), redrawing only when
//! what it says changes.

use crate::api::*;
use crate::print::panel::{export_failure, slice_status};
use crate::print::SliceStatus;
use crate::ui::job_strip::JOB_LINE_CHARS;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabStatusBarBase = #(FabStatusBar::register_widget(vm))
    mod.widgets.FabStatusBar = set_type_default() do mod.widgets.FabStatusBarBase{
        width: Fill
        height: 24
        flow: Right
        align: Align{y: 0.5}
        spacing: 14
        padding: Inset{left: 10 right: 10}
        new_batch: true
        show_bg: true
        a11y: {role: "status" label: "Status"}
        // the caption's ground, a hairline along its top
        draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                sdf.fill(fab.color_topbar)
                sdf.rect(0.0, 0.0, self.rect_size.x, 1.0)
                sdf.fill(fab.color_border)
                return sdf.result
            }
        }
        message := mod.widgets.FabLabelSmall{
            width: Fill
            text: ""
            max_lines: 1
            text_overflow: TextOverflow.Ellipsis
            draw_text +: {
                warn: instance(0.0)
                get_color: fn() { return mix(fab.color_text_dim, fab.color_warning, self.warn) }
            }
        }
        slice := View{
            visible: false
            width: Fit
            height: Fill
            flow: Right
            align: Align{y: 0.5}
            spacing: 8
            slice_text := mod.widgets.FabLabelSmall{ text: "" draw_text +: { color: fab.color_text } }
            slice_bar := mod.widgets.FabProgress{ width: 80 height: 4 visible: false }
        }
        // The job strip, its line in a tooltip; while the app saves on quit, the save's status
        // holds the slot instead.
        jobs := mod.widgets.FabJobsSlotBase{
            width: Fit height: Fill
            align: Align{y: 0.5}
            jobs_tip := FabTip{wrap_width: 360 height: Fill flow: Right align: Align{y: 0.5}
                quit_line := Label{
                    visible: false width: Fit height: Fit
                    draw_text +: {color: fab.color_text text_style: theme.font_regular{font_size: fab.font_size_small}}
                }
                strip := mod.widgets.FabJobStrip{max_chars: #(JOB_LINE_CHARS)}
            }
        }
        // what is on the plate: words read, in the second voice
        info := mod.widgets.FabLabelSmall{ text: "" draw_text +: { color: fab.color_text_dim } }
    }
}

/// What the bar says, compared after each event.
#[derive(Clone, Debug, Default, PartialEq)]
struct Said {
    message: String,
    warn: bool,
    slice: Option<(String, Option<f32>)>,
    info: String,
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabStatusBar {
    #[deref]
    view: View,
    #[rust]
    said: Option<Said>,
}

impl FabStatusBar {
    fn say(&mut self, cx: &mut Cx, said: Said) {
        if self.said.as_ref() == Some(&said) {
            return;
        }
        let message = self.view.label(cx, ids!(message));
        message.set_text(cx, &said.message);
        if let Some(mut label) = message.borrow_mut() {
            label.draw_text.set_dyn_instance(cx, live_id!(warn), &[if said.warn { 1.0 } else { 0.0 }]);
        }
        let slice = self.view.view(cx, ids!(slice));
        slice.set_visible(cx, said.slice.is_some());
        if let Some((text, progress)) = &said.slice {
            slice.label(cx, ids!(slice_text)).set_text(cx, text);
            let bar = slice.view(cx, ids!(slice_bar));
            bar.set_visible(cx, progress.is_some());
            if let Some(f) = progress {
                if let Some(mut bar) = bar.borrow_mut() {
                    bar.draw_bg.set_dyn_instance(cx, live_id!(progress), &[*f]);
                }
            }
        }
        self.view.label(cx, ids!(info)).set_text(cx, &said.info);
        self.said = Some(said);
        self.view.redraw(cx);
    }
}

impl Widget for FabStatusBar {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if matches!(event, Event::Draw(_)) {
            return;
        }
        if let Some(state) = scope.data.get::<AppState>() {
            let said = said(state);
            self.say(cx, said);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            let said = said(state);
            self.say(cx, said);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

/// Bring the bar up to date with the state after an event (the app's pass, after its actions:
/// what an event changed shows without waiting for the next).
pub fn sync(cx: &mut Cx, bar: &WidgetRef, state: &AppState) {
    if let Some(mut bar) = bar.borrow_mut::<FabStatusBar>() {
        bar.say(cx, said(state));
    }
}

fn said(state: &AppState) -> Said {
    // the newest notice, of whichever view, else the tool's hint
    let notice = state.ui.notices.iter().max_by(|a, b| a.at.total_cmp(&b.at));
    let (message, warn) = match notice {
        Some(n) => (n.text.clone(), n.standing),
        None => (tool_hint(state), false),
    };
    Said { message, warn, slice: slice_line(state), info: state.ui.document_info.clone() }
}

/// The tool's one-line feedback: its hint, else its draft (with the Shape key while no pick
/// is asked for).
fn tool_hint(state: &AppState) -> String {
    crate::tools::session::with(|s| match (s.hint.is_empty(), s.draft.is_empty()) {
        (true, false) if !crate::tools::picks::active(state) => format!("{} ({})", s.draft, crate::ui::keymap::shortcut(crate::ui::keymap::ShortcutId::Shape)),
        (true, _) => s.draft.clone(),
        (false, _) => s.hint.clone(),
    })
}

/// The slice's state, with its progress: slicing and a failure whenever they are, the rest
/// (nothing to slice, waiting, checking printability, a part too big) while the slice shows.
pub fn slice_line(state: &AppState) -> Option<(String, Option<f32>)> {
    let p = &state.print;
    // an export that failed, in any view and mode
    if let Some(why) = export_failure(p) {
        return Some((why, None));
    }
    // a printer's job held in view (Monitor): the plate's slice is not what shows
    if p.monitor_holds() {
        return None;
    }
    let (status, progress) = slice_status(p);
    if let SliceStatus::Running { stage, .. } = &p.status {
        let text = match progress {
            _ if stage.starts_with(crate::print::GROWING) => format!("{stage}…"),
            Some(f) if stage == crate::print::PLANNING_SUPPORTS => format!("{stage}… {:.0}%", f * 100.0),
            Some(f) => format!("Slicing… {:.0}%", f * 100.0),
            None => "Slicing…".to_string(),
        };
        return Some((text, progress));
    }
    if p.calibration_error.is_some() || matches!(p.status, SliceStatus::Failed(_)) {
        return Some((status, None));
    }
    if !p.play.active {
        return None;
    }
    if let Some(r) = p.resin_shown() {
        // a resin stack being cut, or why it can't be
        return (!r.note.is_empty()).then(|| (r.note.clone(), None));
    }
    if state.plate.parts.is_empty() && p.preview.is_none() {
        // Empty plates have neither model parts nor a slice.
        return Some(("There is nothing on the plate yet.".to_string(), None));
    }
    if p.preview.is_none() && state.plate.parts.iter().all(|part| part.is_board()) {
        // A board alone: it is never printed, so no slice comes.
        return Some(("Nothing to print yet: a board is not printed.".to_string(), None));
    }
    if p.preview.is_none() || p.stale {
        return Some(("Waiting to slice…".to_string(), None));
    }
    if let Some((done, total)) = p.streamed.as_ref().and_then(|s| s.checking()) {
        // The sliced printability streams in after the slice: the preview stays usable.
        return Some((format!("Checking printability… {done}/{total} layers"), Some(done as f32 / total.max(1) as f32)));
    }
    // A part that won't fit the printer sliced anyway: the slice says so.
    p.fit_warning.clone().map(|w| (w, None))
}

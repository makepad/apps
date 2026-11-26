//! Inline speaking-time edit. The worker applies the same deck op as the AI.
use super::*;

pub(super) fn seconds(text: &str) -> Result<f64, String> {
    let text = text.trim();
    let invalid = || "Use m:ss or minutes, for example 30:00 or 30".to_string();
    let value = if let Some((minutes, seconds)) = text.split_once(':') {
        if minutes.is_empty() || seconds.is_empty() || !minutes.bytes().all(|b| b.is_ascii_digit())
            || !seconds.bytes().all(|b| b.is_ascii_digit()) { return Err(invalid()); }
        let minutes = minutes.parse::<u64>().map_err(|_| invalid())?;
        let seconds = seconds.parse::<u64>().map_err(|_| invalid())?;
        if seconds >= 60 { return Err(invalid()); }
        minutes as f64 * 60.0 + seconds as f64
    } else {
        text.parse::<f64>().map_err(|_| invalid())? * 60.0
    };
    if !value.is_finite() || value.round() < 1.0 {
        return Err("Enter a duration of at least one second".into());
    }
    Ok(value.round())
}

impl DeckPage {
    pub(super) fn cancel_duration(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if self.duration_edit.take().is_none() { return; }
        self.duration_focus = false;
        if has_focus(cx, ui.widget(cx, ids!(deck_duration_input)).area()) { cx.set_key_focus(Area::Empty); }
        ui.widget(cx, ids!(deck_duration_editor)).set_visible(cx, false);
        ui.widget(cx, ids!(deck_duration)).set_visible(cx, true);
        ui.redraw(cx);
    }

    pub(super) fn focus_duration(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if !self.duration_focus { return; }
        if ui.widget(cx, ids!(deck_duration_input)).area() == Area::Empty {
            // The newly visible field needs its first draw before it can own
            // the keys. Wake once more even when the rest of the deck is idle.
            cx.new_next_frame();
            return;
        }
        let input = ui.text_input(cx, ids!(deck_duration_input));
        if let Some(mut input) = input.borrow_mut() {
            input.take_key_focus(cx);
            input.select_all(cx);
            self.duration_focus = false;
        };
    }

    pub(super) fn duration_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        let input = ui.text_input(cx, ids!(deck_duration_input));
        if ui.button(cx, ids!(deck_duration)).clicked(actions) {
            let Some(session) = &self.session else { return };
            self.duration_edit = Some(session.revision);
            self.duration_focus = true;
            // The length set on the deck; empty: the slides' times add up to it.
            input.set_text(cx, &session.doc.talk_seconds.map(|talk| { let talk = talk.round().max(0.0) as u64; format!("{}:{:02}", talk / 60, talk % 60) }).unwrap_or_default());
            ui.label(cx, ids!(deck_duration_error)).set_text(cx, "");
            ui.widget(cx, ids!(deck_duration)).set_visible(cx, false);
            ui.widget(cx, ids!(deck_duration_editor)).set_visible(cx, true);
            ui.redraw(cx);
        }
        let Some(revision) = self.duration_edit else { return };
        if input.escaped(actions) {
            self.cancel_duration(cx, ui);
        } else if let Some((text, _)) = input.returned(actions) {
            let talk = if text.trim().is_empty() { Ok(None) } else { seconds(&text).map(Some) };
            let result = talk.and_then(|seconds| {
                if self.session.as_ref().is_none_or(|session| session.revision != revision) {
                    return Err("The deck changed. Escape and edit its new duration.".into());
                }
                if self.editing || !self.pending.is_empty() {
                    return Err("An edit is finishing. Press Enter again when it has finished.".into());
                }
                self.submit_op(cx, DeckOp::SetTalk { seconds })
            });
            match result {
                Ok(()) => self.cancel_duration(cx, ui),
                Err(error) => {
                    ui.label(cx, ids!(deck_duration_error)).set_text(cx, &error);
                    input.take_key_focus(cx);
                }
            }
        } else if input.key_focus_lost(actions) && !self.duration_focus {
            self.cancel_duration(cx, ui);
        }
    }
}

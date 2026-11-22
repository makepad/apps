//! The firmware's steps (Q3): to its next statement, to the next entry of any handler, and out of
//! the handler running. The transport and the Firmware pane's state panel hold the same widget, and
//! each button sends the command `elec.fw.step` sends (`SimCmd::StepFw`). It shows while a firmware
//! is bound (`SimLive::firmware`), and offers a step only where the device takes it, by the device's
//! own rules on its newest published state (`SimLive::co`): `FwState::steps` and, for the step out,
//! `FwState::steps_out_of`. A step runs unpaced and leaves the run paused.
//!
//! Beside a narrow strip the transport folds the buttons to icons, each with a tip and the same
//! accessible name ([`fold_to_icons`]); the Firmware pane keeps the words.
//!
//! Cost: per signal or frame one flag and one pointer compare; per new device publication two
//! binary searches of the image's step map. Nothing per item.

use super::sim::SimAction;
use crate::ElecLive;
use fab_elec_bench::SimCmd;
use fab_elec_mcu_cosim::FwState;
use fab_elec_sim::cosim::{CoState, StepTo};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.FwStepsBase = #(FwSteps::register_widget(vm))
    mod.widgets.FwSteps = set_type_default() do mod.widgets.FwStepsBase {
        visible: false
        width: Fit height: Fit
        flow: Right
        align: Align{x: 0.0 y: 0.5}
        spacing: 4
        words := View {
            width: Fit height: Fit
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            spacing: 4
            title := Label {
                text: "Step"
                margin: Inset{right: 2}
                draw_text +: {color: theme.color_text_meta text_style: theme.font_regular {font_size: theme.prop_font_size}}
            }
            statement := ButtonFlatter {
                width: Fit height: 22 padding: Inset{left: 6 right: 6} margin: 0 text: "Statement"
                draw_text +: {text_style: theme.font_regular {font_size: theme.prop_font_size}}
                a11y: {label: "Run the firmware to its next statement"}
            }
            entry := ButtonFlatter {
                width: Fit height: 22 padding: Inset{left: 6 right: 6} margin: 0 text: "Handler"
                draw_text +: {text_style: theme.font_regular {font_size: theme.prop_font_size}}
                a11y: {label: "Run the firmware to the next entry of any handler"}
            }
            out := ButtonFlatter {
                width: Fit height: 22 padding: Inset{left: 6 right: 6} margin: 0 text: "Out"
                draw_text +: {text_style: theme.font_regular {font_size: theme.prop_font_size}}
                a11y: {label: "Run the firmware until the handler running returns"}
            }
        }
        // The same steps as icons; the host gives them their pictures.
        icons := View {
            visible: false
            width: Fit height: Fit
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            spacing: 2
            statement_tip := Tip {
                text: "Step: the next statement"
                statement_icon := ButtonFlatterIcon {
                    width: 22 height: 22 text: ""
                    a11y: {label: "Run the firmware to its next statement"}
                    label_walk: Walk{width: 0 height: 0}
                    icon_walk: Walk{width: 14 height: 14}
                }
            }
            entry_tip := Tip {
                text: "Step: the next entry of any handler"
                entry_icon := ButtonFlatterIcon {
                    width: 22 height: 22 text: ""
                    a11y: {label: "Run the firmware to the next entry of any handler"}
                    label_walk: Walk{width: 0 height: 0}
                    icon_walk: Walk{width: 14 height: 14}
                }
            }
            out_tip := Tip {
                text: "Step: out of the handler running"
                out_icon := ButtonFlatterIcon {
                    width: 22 height: 22 text: ""
                    a11y: {label: "Run the firmware until the handler running returns"}
                    label_walk: Walk{width: 0 height: 0}
                    icon_walk: Walk{width: 14 height: 14}
                }
            }
        }
    }
}

/// What the step buttons offer: shown, the statement and handler-entry steps, the step out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Steps {
    pub shown: bool,
    pub step: bool,
    pub out: bool,
}

impl Steps {
    /// Whether a step of this kind can go now.
    pub fn offers(self, to: StepTo) -> bool {
        match to {
            StepTo::Statement | StepTo::HandlerEntry => self.step,
            StepTo::HandlerExit => self.out,
        }
    }
}

/// The steps a firmware offers: shown while one is bound; each step only where its device, by its
/// newest published state, takes it (the statement and handler-entry steps `FwState::steps`, the
/// step out `FwState::steps_out_of`).
pub fn steps(bound: bool, co: Option<&CoState>) -> Steps {
    let state = co.and_then(|co| co.get::<FwState>()).filter(|_| bound);
    Steps { shown: bound, step: state.is_some_and(FwState::steps), out: state.is_some_and(|s| s.steps_out_of().is_some()) }
}

/// Whether the steps show as icons beside a strip: when the row's `room` for the strip and the
/// steps, less the words' width, would leave the strip narrower than `wide`. It depends on the
/// row's width and the words' alone, never on how the steps were last drawn, so the row never flips
/// back and forth.
pub fn fold_to_icons(room: f64, words_w: f64, wide: f64) -> bool {
    room - words_w < wide
}

/// The words' width before they were first drawn: the title and the three buttons at the theme's
/// size (about 222 points measured).
const WORDS_GUESS: f64 = 224.0;

#[derive(Script, ScriptHook, Widget)]
pub struct FwSteps {
    #[deref]
    view: View,
    /// Whether a firmware was bound and the device state, as last read.
    #[rust]
    seen: Option<(bool, Option<CoState>)>,
    #[rust]
    shown: Steps,
    /// Shown as icons, and the words' width when they were last drawn.
    #[rust]
    folded: bool,
    #[rust]
    words_w: Option<f64>,
}

impl FwSteps {
    /// The steps as the buttons show them.
    pub fn steps(&self) -> Steps {
        self.shown
    }

    /// Sends a step of this kind when the firmware offers it; false otherwise.
    pub fn press(&mut self, cx: &mut Cx, to: StepTo) -> bool {
        self.sync(cx);
        if !self.shown.offers(to) {
            return false;
        }
        cx.action(SimAction::Cmd(SimCmd::StepFw(to)));
        true
    }

    /// Folds the steps to icons, or opens them, for a row with `room` points for the strip and the
    /// steps ([`fold_to_icons`]); called before the row is laid out, so the frame shows it.
    pub fn fit_row(&mut self, cx: &mut Cx, room: f64, wide: f64) {
        if !self.view.visible() {
            return;
        }
        let folded = fold_to_icons(room, self.words_w.unwrap_or(WORDS_GUESS), wide);
        if folded != self.folded {
            self.folded = folded;
            self.view.widget(cx, ids!(words)).set_visible(cx, !folded);
            self.view.widget(cx, ids!(icons)).set_visible(cx, folded);
        }
    }

    /// Keeps the words' width once they are drawn, for the next frame's [`FwSteps::fit_row`].
    pub fn measure(&mut self, cx: &Cx) {
        let drawn = self.view.area().rect(cx).size.x;
        if self.view.visible() && !self.folded && drawn > 0.0 {
            self.words_w = Some(drawn);
        }
    }

    fn sync(&mut self, cx: &mut Cx) {
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return };
        let bound = live.sim.firmware.is_some();
        if self.seen.as_ref().is_some_and(|(b, co)| *b == bound && *co == live.sim.co) {
            return;
        }
        let co = live.sim.co.clone();
        let now = steps(bound, co.as_ref());
        self.seen = Some((bound, co));
        if now == self.shown && self.view.visible() == now.shown {
            return;
        }
        self.shown = now;
        WidgetNode::set_visible(&mut self.view, cx, now.shown);
        for (words, icon, on) in [(ids!(statement), ids!(statement_icon), now.step), (ids!(entry), ids!(entry_icon), now.step), (ids!(out), ids!(out_icon), now.out)] {
            self.view.button(cx, words).set_enabled(cx, on);
            self.view.button(cx, icon).set_enabled(cx, on);
        }
        self.view.redraw(cx);
    }
}

impl Widget for FwSteps {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Signal | Event::NextFrame(_)) {
            self.sync(cx);
        }
        if !self.view.visible() {
            return;
        }
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        for (words, icon, to) in [(id!(statement), id!(statement_icon), StepTo::Statement), (id!(entry), id!(entry_icon), StepTo::HandlerEntry), (id!(out), id!(out_icon), StepTo::HandlerExit)] {
            if self.view.button(cx, &[words]).clicked(&actions) || self.view.button(cx, &[icon]).clicked(&actions) {
                self.press(cx, to);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.sync(cx);
        self.view.draw_walk(cx, scope, walk)
    }
}

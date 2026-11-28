//! The Calibrate panel's steps, test parts and stored results.
//! A made coupon becomes a project on the model worker, then prints through the Print button.

use crate::print::{CalWriteOutcome, CalibrationOwner, EffectiveCalibration, Printing};
use fab::print::PrintState;
use fab::print::calibrate::{cleared_status, inherited_summary, saved_status, AnswerKind, CalStepRow, CalibrateAction, CalibrateState, CouponView};
use fab_calibrate::results::{names, EffectiveResults};
use fab_calibrate::{Answer, Coupon, Step};
use std::sync::Arc;

/// The destination that produced a coupon is retained even if the target later changes.
#[derive(Clone, Debug)]
pub struct CapturedCoupon {
    pub coupon: Arc<Coupon>,
    pub owner: Option<CalibrationOwner>,
    pub preset: String,
    pub filament: String,
}

/// Posted by the model worker after making a test part.
#[derive(Debug)]
pub enum CalEvent {
    Made(Result<(CapturedCoupon, [f32; 4]), String>),
}

impl CalEvent {
    pub(super) fn make(step: Step, setup: &crate::print::CheckSetup, colour: [f32; 4]) -> Self {
        let made = fab_calibrate::make(step, &setup.coupon_setup()).map(|mut coupon| {
            let (preset, filament) = setup.calibration_names();
            coupon.printer.clone_from(&preset);
            coupon.filament.clone_from(&filament);
            (CapturedCoupon { coupon: Arc::new(coupon), owner: setup.calibration_owner().cloned(), preset, filament }, colour)
        });
        Self::Made(made.ok_or_else(|| format!("{} has no test part", step.title())))
    }
}

#[derive(Default)]
pub struct Calibrate {
    /// The last made part (its numbers and setup are what an answer refers to).
    coupon: Option<CapturedCoupon>,
    effective: Arc<EffectiveCalibration>,
    synced: bool,
    pending_make: Option<(Step, [f32; 4])>,
    making: bool,
}

fn answer_kind(step: Step) -> AnswerKind {
    match step {
        Step::SlipFit => AnswerKind::Two("Slides freely in", "Presses into"),
        Step::Snap => AnswerKind::Two("Largest that went in", "Smallest that held"),
        Step::Friction => AnswerKind::Angle,
        Step::HeadEnvelope => AnswerKind::Measure,
        _ => AnswerKind::Number,
    }
}

/// A step's stored values, said short.
fn summary(step: Step, r: &EffectiveResults) -> String {
    let g = |n: &str| r.get(n);
    let s = match step {
        Step::Clearance => g(names::C_XY).map(|v| format!("{v:.2} mm")),
        Step::ZGap => g(names::C_Z_LAYERS).map(|v| format!("{v:.0} layers")),
        Step::SlipFit => g(names::SLIP_FIT).zip(g(names::PRESS_FIT)).map(|(a, b)| format!("slip {a:.2} · press {b:.2} mm")),
        Step::Snap => g(names::SNAP_HOLD).zip(g(names::SNAP_MAX)).map(|(a, b)| format!("grip {a:.1}-{b:.1} mm")),
        Step::Friction => g(names::MU).map(|v| format!("mu {v:.2}")),
        Step::Flow => g(names::FLOW_RATIO).map(|v| format!("flow {v:.3}")),
        Step::Temperature => g(names::NOZZLE_TEMP).map(|v| format!("{v:.0} °C")),
        Step::PressureAdvance => g(names::PRESSURE_ADVANCE).map(|v| format!("K {v:.3}")),
        Step::HeadEnvelope => None,
    };
    s.unwrap_or_default()
}

impl Calibrate {
    /// Reuse the panel rows until the target, material, or resolved measurements change.
    pub fn sync(
        &mut self, state: &mut CalibrateState, effective: &Arc<EffectiveCalibration>,
        owner: Option<&CalibrationOwner>, preset: &str, filament: &str,
    ) -> bool {
        if self.synced && Arc::ptr_eq(&self.effective, effective) && state.owner_id.as_deref() == owner.map(|o| o.id.as_str())
            && state.printer == preset && state.filament == filament && !state.steps.is_empty() {
            return false;
        }
        self.effective = effective.clone();
        state.owner_id = owner.map(|o| o.id.clone());
        state.owner_title = owner.map(|o| o.title.clone());
        preset.clone_into(&mut state.printer);
        filament.clone_into(&mut state.filament);
        self.fill(state);
        self.synced = true;
        true
    }

    fn fill(&self, state: &mut CalibrateState) {
        let effective = &self.effective;
        let results = if effective.owner.as_ref().map(|o| &o.id) == state.owner_id.as_ref()
            && effective.preset == state.printer {
            effective.for_filament(&state.filament)
        } else {
            None
        };
        state.steps = Step::ALL.iter().map(|&s| {
            let mut value = results.map_or_else(String::new, |r| summary(s, r));
            if !value.is_empty() && results.is_some_and(|r| r.step_from_preset(s)) {
                value = inherited_summary(&value, &state.printer);
            }
            CalStepRow {
                title: s.title().into(),
                feeds: s.feeds().into(),
                done: results.and_then(|r| r.done(s)).map(|d| d.date.clone()),
                summary: value,
                can_clear: results.is_some_and(|r| r.can_clear(s)),
                blocked: (s == Step::HeadEnvelope).then(|| "Coming with non-planar printing: the head measurement form and its checks.".into()),
            }
        }).collect();
        state.selected = state.selected.min(state.steps.len().saturating_sub(1));
        let selected = Step::ALL[state.selected];
        state.coupon = self.coupon.as_ref().map(|captured| &captured.coupon).filter(|c| c.step == selected).map(|c| CouponView {
            name: c.name.clone(),
            check: c.check.clone(),
            variants: c.variants.iter().map(|v| format!("{}: {}", v.number, v.label)).collect(),
            answer: answer_kind(c.step),
        });
    }

    /// Handle a panel action with the job's settings snapshot for the coupon.
    /// Returns true when the panel changed.
    pub fn action(&mut self, print_state: &mut PrintState, a: &CalibrateAction, printing: &mut Printing, colour: [f32; 4], host: Option<&mut crate::model_host::ModelHost>) -> bool {
        let state = &mut print_state.calibrate;
        match a {
            CalibrateAction::Select(i) => {
                state.selected = (*i).min(Step::ALL.len() - 1);
                state.status.clear();
                self.fill(state);
            }
            CalibrateAction::MakePart => {
                if self.making {
                    return false;
                }
                let step = Step::ALL[state.selected.min(Step::ALL.len() - 1)];
                if printing.calibration_pending() {
                    self.pending_make = Some((step, colour));
                    state.status = "Loading calibration...".into();
                    self.making = true;
                    return true;
                }
                self.making = true;
                state.status = format!("Making the {} test part ...", step.title().to_lowercase());
                match host {
                    Some(host) => host.make_calibration(step, printing.check_setup(print_state), colour),
                    None => {
                        self.making = false;
                        state.status = "The model worker is not ready yet.".into();
                    }
                }
            }
            CalibrateAction::Answer { a, b, angle } => {
                let Some(captured) = self.coupon.as_ref() else {
                    state.status = "Make and print the test part first: the numbers refer to it.".into();
                    return true;
                };
                let answer = match answer_kind(captured.coupon.step) {
                    AnswerKind::Number => Answer::Number(*a),
                    AnswerKind::Two(..) => Answer::Two(*a, *b),
                    AnswerKind::Angle => Answer::Angle(*angle),
                    AnswerKind::Measure => return false,
                };
                printing.save_calibration(
                    print_state, captured.owner.clone(), captured.preset.clone(), captured.filament.clone(),
                    captured.coupon.clone(), answer, makepad_fab_slice::today(),
                );
                print_state.calibrate.status = "Saving result...".into();
            }
            CalibrateAction::Clear => {
                let step = Step::ALL[state.selected.min(Step::ALL.len() - 1)];
                printing.clear_calibration(print_state, step);
                print_state.calibrate.status = "Clearing result...".into();
            }
        }
        true
    }

    /// A made part arrived: remember it; the caller opens its plate and slices it.
    pub fn made(&mut self, state: &mut CalibrateState, made: &Result<(CapturedCoupon, [f32; 4]), String>) {
        self.making = false;
        match made {
            Ok((c, _)) => {
                self.coupon = Some(c.clone());
                state.status = format!("{} is open as its own plate and slicing; your plate is saved in File > Open Recent. Check its layers, then Print and confirm when you are ready.", c.coupon.name);
                self.fill(state);
            }
            Err(e) => {
                state.status = e.clone();
            }
        }
    }

    pub fn take_pending_make(&mut self) -> Option<(Step, [f32; 4])> {
        self.pending_make.take()
    }

    pub fn written(
        &mut self, state: &mut CalibrateState, outcome: &CalWriteOutcome,
        current_owner: Option<&CalibrationOwner>, current_preset: &str, current_filament: &str,
    ) {
        state.status = match outcome {
            CalWriteOutcome::Saved { owner, preset, filament, step, values } => {
                let different = owner.as_ref() != current_owner || preset != current_preset || filament != current_filament;
                let label = owner.as_ref().map_or(preset.as_str(), |o| o.title.as_str());
                let values = values.iter().map(|(k, v)| format!("{k} {v:.3}")).collect::<Vec<_>>().join(", ");
                saved_status(step.feeds(), &values, different.then_some((label, filament)))
            }
            CalWriteOutcome::Cleared { owner, preset, step, has_preset_fallback } =>
                cleared_status(owner.as_ref().map(|o| o.title.as_str()), preset, step.title(), *has_preset_fallback),
            CalWriteOutcome::Failed(error) => format!("Could not save: {error}"),
        };
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_calibrate::CouponSetup;
    use fab_plate::machine::Machine;

    #[test]
    fn calibrate_panel_copy_and_source_indicators() {
        use fab_calibrate::results::{EffectiveValue, ValueSource};
        let p = CalibrationOwner { id: "printer:P".into(), title: "P".into() };
        let preset = "Sim 0.4";
        let filament = "PLA";
        let mut r = EffectiveResults { owner: Some(p.id.clone()), preset: preset.into(), filament: filament.into(), ..Default::default() };
        r.values.insert(names::FLOW_RATIO.into(), EffectiveValue { value: 0.91, source: ValueSource::Owner });
        r.values.insert(names::PRESSURE_ADVANCE.into(), EffectiveValue { value: 0.035, source: ValueSource::Preset });
        r.clearable_steps.insert(Step::Flow.key().into());
        let mut arc = Arc::new(EffectiveCalibration {
            owner: Some(p.clone()), preset: preset.into(), by_filament: [(filament.into(), r)].into_iter().collect(),
        });
        let mut panel = Calibrate::default();
        let mut state = CalibrateState::default();
        assert!(panel.sync(&mut state, &arc, Some(&p), preset, filament));
        assert!(!panel.sync(&mut state, &arc, Some(&p), preset, filament), "an unchanged report reuses the panel rows");
        assert_eq!(state.context_text(), "Calibrating P with PLA");
        assert_eq!(state.clear_label(), "Clear P's results");
        let flow = state.steps.iter().find(|s| s.title == Step::Flow.title()).unwrap();
        assert!(flow.can_clear && flow.summary == "flow 0.910", "the owned measurement identifies its value without a fallback label");
        let pa = state.steps.iter().find(|s| s.title == Step::PressureAdvance.title()).unwrap();
        assert!(!pa.can_clear && pa.summary == "K 0.035 (from Sim 0.4)", "a fallback names its source and cannot be cleared here");
        for (fallback, status) in [(true, "Cleared P's results for Flow; using Sim 0.4's result."), (false, "Cleared P's results for Flow; back to Fab's defaults.")] {
            let r = Arc::make_mut(&mut arc).by_filament.get_mut(filament).unwrap();
            r.clearable_steps.clear();
            if fallback {
                r.values.insert(names::FLOW_RATIO.into(), EffectiveValue { value: 0.98, source: ValueSource::Preset });
            } else { r.values.remove(names::FLOW_RATIO); }
            panel.sync(&mut state, &arc, Some(&p), preset, filament);
            let flow = state.steps.iter().find(|s| s.title == Step::Flow.title()).unwrap();
            assert!(!flow.can_clear, "a cleared owner measurement cannot be cleared again");
            assert_eq!(flow.summary, if fallback { "flow 0.980 (from Sim 0.4)" } else { "" }, "clearing reveals the fallback or the default");
            panel.written(&mut state, &CalWriteOutcome::Cleared {
                owner: Some(p.clone()), preset: preset.into(), step: Step::Flow, has_preset_fallback: fallback,
            }, Some(&p), preset, filament);
            assert_eq!(state.status, status);
        }
        Arc::make_mut(&mut arc).owner = None;
        panel.sync(&mut state, &arc, None, preset, filament);
        assert_eq!(state.context_text(), "For Sim 0.4 · PLA");
        assert_eq!(state.clear_label(), "Clear result");
    }

    #[test]
    fn every_coupon_becomes_a_plate_on_the_bed() {
        let machine = Machine::dual_nozzle_350();
        for step in Step::ALL {
            let Some(c) = fab_calibrate::make(step, &CouponSetup::default()) else { continue };
            let mut session = fab_ai::Session::new(machine.clone(), std::env::temp_dir());
            let reply = session.calibration(&c, [0.8, 0.8, 0.8, 1.0]);
            assert!(reply.ok, "{}", reply.text);
            let plate = &session.last.as_ref().unwrap().plate;
            let reports = fab_plate::machine::check(&machine, plate);
            assert!(reports.iter().all(|r| !r.blocking()), "{:?}: {:?}", step, reports.iter().flat_map(|r| r.issues.iter().map(|i| i.describe(&machine))).collect::<Vec<_>>());
            assert_eq!(plate.settings.iter().find(|(k, _)| k == "fab_calibration_bands").map(|(_, v)| v), Some(&c.bands_text()));
        }
    }
}

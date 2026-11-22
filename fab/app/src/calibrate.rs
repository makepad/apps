//! The Calibrate panel's steps, test parts and stored results.
//! A made coupon becomes a project on the model worker, then prints through the Print button.

use fab::print::calibrate::{AnswerKind, CalStepRow, CalibrateAction, CalibrateState, CouponView};
use fab_calibrate::results::names;
use fab_calibrate::{Answer, Coupon, Results, Step};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static REVISION: AtomicU64 = AtomicU64::new(0);

/// Bumped whenever stored results change (part of the slice settings' identity).
pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

/// Posted by the model worker after making a test part.
#[derive(Debug)]
pub enum CalEvent {
    Made(Result<(Arc<Coupon>, [f32; 4]), String>),
}

#[derive(Default)]
pub struct Calibrate {
    /// The last made part (its numbers and setup are what an answer refers to).
    coupon: Option<Arc<Coupon>>,
    results: Results,
    results_loaded: bool,
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
fn summary(step: Step, r: &Results) -> String {
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

/// The values a step stores (to forget them).
fn step_names(step: Step) -> &'static [&'static str] {
    match step {
        Step::Clearance => &[names::C_XY],
        Step::ZGap => &[names::C_Z_LAYERS],
        Step::SlipFit => &[names::SLIP_FIT, names::PRESS_FIT],
        Step::Snap => &[names::SNAP_MAX, names::SNAP_HOLD],
        Step::Friction => &[names::MU],
        Step::Flow => &[names::FLOW_RATIO],
        Step::Temperature => &[names::NOZZLE_TEMP],
        Step::PressureAdvance => &[names::PRESSURE_ADVANCE],
        Step::HeadEnvelope => &[],
    }
}

impl Calibrate {
    /// Load a different printer/filament pair and fill the panel.
    pub fn refresh(&mut self, state: &mut CalibrateState, printer: &str, filament: &str) {
        if !self.results_loaded || self.results.printer != printer || self.results.filament != filament {
            self.results = Results::load(printer, filament);
            self.results_loaded = true;
        }
        state.printer = printer.to_string();
        state.filament = filament.to_string();
        self.fill(state);
    }

    fn fill(&self, state: &mut CalibrateState) {
        state.steps = Step::ALL
            .iter()
            .map(|&s| CalStepRow {
                title: s.title().to_string(),
                feeds: s.feeds().to_string(),
                done: self.results.done(s).map(|d| d.to_string()),
                summary: summary(s, &self.results),
                blocked: (s == Step::HeadEnvelope).then(|| "Coming with non-planar printing: the head measurement form and its checks.".to_string()),
            })
            .collect();
        state.selected = state.selected.min(state.steps.len().saturating_sub(1));
        let selected = Step::ALL[state.selected];
        state.coupon = self.coupon.as_ref().filter(|c| c.step == selected).map(|c| CouponView {
            name: c.name.clone(),
            check: c.check.clone(),
            variants: c.variants.iter().map(|v| format!("{}: {}", v.number, v.label)).collect(),
            answer: answer_kind(c.step),
        });
    }

    /// Handle a panel action with the job's settings snapshot for the coupon.
    /// Returns true when the panel changed.
    pub fn action(&mut self, state: &mut CalibrateState, a: &CalibrateAction, setup: crate::print::CheckSetup, colour: [f32; 4], host: Option<&mut crate::model_host::ModelHost>) -> bool {
        let (printer, filament) = setup.calibration_names();
        match a {
            CalibrateAction::Select(i) => {
                state.selected = (*i).min(Step::ALL.len() - 1);
                state.status.clear();
                self.fill(state);
                return true;
            }
            CalibrateAction::MakePart => {
                if self.making {
                    return false;
                }
                let step = Step::ALL[state.selected.min(Step::ALL.len() - 1)];
                self.making = true;
                state.status = format!("Making the {} test part ...", step.title().to_lowercase());
                match host {
                    Some(host) => host.make_calibration(step, setup, colour),
                    None => {
                        self.making = false;
                        state.status = "The model worker is not ready yet.".into();
                    }
                }
            }
            CalibrateAction::Answer { a, b, angle } => {
                let Some(c) = self.coupon.clone() else {
                    state.status = "Make and print the test part first: the numbers refer to it.".into();
                    return true;
                };
                let answer = match answer_kind(c.step) {
                    AnswerKind::Number => Answer::Number(*a),
                    AnswerKind::Two(..) => Answer::Two(*a, *b),
                    AnswerKind::Angle => Answer::Angle(*angle),
                    AnswerKind::Measure => return false,
                };
                let mut r = Results::load(&c.printer, &c.filament);
                match r.record(&c, &answer, &makepad_fab_slice::today()) {
                    Ok(set) => match r.save() {
                        Ok(_) => {
                            REVISION.fetch_add(1, Ordering::Relaxed);
                            state.status = format!("Saved for {}: {}.", c.step.feeds(), set.iter().map(|(k, v)| format!("{k} {v:.3}")).collect::<Vec<_>>().join(", "));
                            if c.printer != printer || c.filament != filament {
                                state.status.push_str(&format!(" Printer: {}; filament: {}.", c.printer, c.filament));
                            }
                            self.results_loaded = false;
                        }
                        Err(e) => state.status = format!("Could not save: {e}"),
                    },
                    Err(e) => state.status = e,
                }
            }
            CalibrateAction::Clear => {
                let step = Step::ALL[state.selected.min(Step::ALL.len() - 1)];
                let mut r = Results::load(&printer, &filament);
                for n in step_names(step) {
                    r.values.remove(*n);
                }
                r.dates.remove(step.key());
                match r.save() {
                    Ok(_) => {
                        REVISION.fetch_add(1, Ordering::Relaxed);
                        state.status = format!("{}: back to Fab's defaults.", step.title());
                        self.results_loaded = false;
                    }
                    Err(e) => state.status = format!("Could not save: {e}"),
                }
            }
        }
        self.refresh(state, &printer, &filament);
        true
    }

    /// A made part arrived: remember it; the caller opens its plate and slices it.
    pub fn made(&mut self, state: &mut CalibrateState, made: &Result<(Arc<Coupon>, [f32; 4]), String>) {
        self.making = false;
        match made {
            Ok((c, _)) => {
                self.coupon = Some(c.clone());
                state.status = format!("{} is open as its own plate and slicing; your plate is saved in File > Open Recent. Check its layers, then Print and confirm when you are ready.", c.name);
                let (p, f) = (state.printer.clone(), state.filament.clone());
                self.refresh(state, &p, &f);
            }
            Err(e) => {
                state.status = e.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_calibrate::CouponSetup;
    use fab_plate::machine::Machine;

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

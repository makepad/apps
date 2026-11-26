//! The two faces of a fit stay in the view until Clear or a change of tool.
use crate::api::*;
use crate::viewport::GhostCopy;
use makepad_widgets_core::{Cx, Geometry};
use std::{rc::Rc, sync::Arc};

#[derive(Clone, Debug, Default)]
pub struct FitDraft {
    pub a: Option<FitPick>,
    pub b: Option<FitPick>,
    turn: u16,
    pub request: Option<u64>,
    ghost: Option<Rc<Geometry>>,
    pub result: Option<Result<Arc<PlateFit>, String>>,
}

impl FitDraft {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn pick(&mut self, face: FitPick) -> Option<ShellAction> {
        if self.b.is_some() { return None; }
        let Some(a) = &self.a else {
            self.a = Some(face);
            self.result = None;
            return None;
        };
        if a.part == face.part {
            self.result = Some(Err(FIT_DIFFERENT_PARTS.into()));
            return None;
        }
        self.b = Some(face);
        self.measure()
    }

    pub(super) fn turn(&mut self) -> Option<ShellAction> {
        self.b.as_ref()?;
        self.turn = (self.turn + 90) % 360;
        self.measure()
    }

    fn measure(&mut self) -> Option<ShellAction> {
        self.request = None;
        self.ghost = None;
        self.result = None;
        Some(ShellAction::MeasureFit { a: self.a.clone()?, b: self.b.clone()?, turn: self.turn as f64 })
    }

    pub(super) fn same_elements(&self, scene: &Scene) -> bool {
        [&self.a, &self.b].into_iter().flatten().all(|pick| {
            scene.element(pick.element).is_some_and(|e| e.name == pick.part)
        })
    }

    pub fn matches(&self, a: &FitPick, b: &FitPick, turn: f64) -> bool {
        self.a.as_ref() == Some(a) && self.b.as_ref() == Some(b) && self.turn as f64 == turn
    }

    pub fn accept(&mut self, cx: &mut Cx, id: u64, result: &Result<Arc<PlateFit>, String>) -> bool {
        if self.request != Some(id) { return false; }
        self.request = None;
        self.ghost = result.as_ref().ok().and_then(|fit| fit.ghost_mesh.lock().unwrap().take()).map(|mesh| {
            let geometry = Geometry::new(cx);
            geometry.update(cx, mesh.indices, mesh.vertices);
            Rc::new(geometry)
        });
        self.result = Some(result.clone());
        true
    }

    pub(super) fn label(&self, units: &Units) -> String {
        match &self.result {
            Some(Err(words)) => words.clone(),
            Some(Ok(fit)) if fit.overlap_bounds.is_some() => format!("Overlaps {:.1} mm³", fit.overlap_mm3),
            Some(Ok(fit)) if !fit.seat => "The picked faces do not make seating contact.".into(),
            Some(Ok(fit)) => fit.smallest.and_then(|i| fit.gaps[i]).map_or_else(|| "Seats".into(),
                |gap| format!("Seats: smallest side {}", length(units, gap.distance_mm))),
            None if self.a.is_none() => "Pick the face the other part sits on.".into(),
            None if self.b.is_none() => "Pick the seating face on the other part.".into(),
            None => "Measuring fit…".into(),
        }
    }
}

pub(crate) fn in_pane(state: &AppState, pane: PaneId) -> bool {
    state.tool == Tool::MeasureFit && state.view.pane(pane).role == PaneRole::Plate
        && state.pane_poses(pane) == PanePoses::Stage && !state.ui.in_mode(PlateMode::Mechanics)
}

// A copy is drawn from the evaluated mesh, including when the original is a field.
pub(crate) fn ghost(state: &AppState, pane: PaneId) -> Option<GhostCopy> {
    if !in_pane(state, pane) { return None; }
    super::session::with(|s| {
        let source = s.fit.b.as_ref()?;
        let geometry = s.fit.ghost.as_ref()?;
        (state.scene.element(source.element)?.name == source.part).then(|| GhostCopy {
            geometry: geometry.geometry_id(), source: source.element, tint: 0x61c9dc,
        })
    })
}

pub(super) fn length(units: &Units, mm: f64) -> String {
    let precision = (units.display.meters() / 1e-5).log10().ceil() as u8;
    Units { precision: units.precision.max(precision), ..*units }.format_length(mm * 0.001)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{apply, session};

    #[test]
    fn fit_picks_turn_clear_and_leave() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut state = AppState::default();
        let face = |name: &str| FitPick { element: ElementId(0), part: name.into(), point: [2.0, 0.0, 1.0], normal: [0.0, 0.0, 1.0] };
        let (a, b) = (face("box"), face("lid"));
        let set_tool = |cx: &mut Cx, state: &mut AppState, tool| {
            let action = ShellAction::SetTool(tool);
            state.apply_core(&action);
            apply(cx, state, &action);
        };
        set_tool(&mut cx, &mut state, Tool::MeasureFit);
        session::with(|s| {
            assert!(s.fit.pick(a.clone()).is_none() && s.fit.a.as_ref() == Some(&a), "the first pick must retain A without requesting a fit");
            let another_face = FitPick { normal: [1.0, 0.0, 0.0], ..a.clone() };
            assert!(s.fit.pick(another_face).is_none() && s.fit.b.is_none(), "a second face on A must leave B open");
            assert_eq!(s.fit.label(&Units::millimeters()), FIT_DIFFERENT_PARTS, "the view must explain the same-part refusal");
            assert!(matches!(s.fit.pick(b.clone()), Some(ShellAction::MeasureFit { a: x, b: y, turn }) if x == a && y == b && turn == 0.0), "a valid B must measure the two clicked planes at zero turn");
            s.fit.request = Some(3);
            assert!(matches!(s.fit.turn(), Some(ShellAction::MeasureFit { turn, .. }) if turn == 90.0), "turn must request another quarter turn");
            assert!(!s.fit.accept(&mut cx, 3, &Err("old".into())), "the previous reply must not replace the turned fit");
        });
        session::with(|s| {
            s.fit.request = Some(5);
            assert!(s.fit.accept(&mut cx, 5, &Err("The picked surface is curved".into())), "the current worker refusal must reach the view");
        });
        apply(&mut cx, &mut state, &ShellAction::ClearFit);
        let empty = |d: &FitDraft| d.a.is_none() && d.b.is_none() && d.turn == 0 && d.result.is_none() && d.request.is_none() && d.ghost.is_none();
        session::with(|s| {
            assert!(empty(&s.fit), "Clear must empty the picks, turn, request and result");
            assert_eq!(length(&Units::millimeters(), 0.2), "0.20 mm", "a clearance must retain hundredths of a millimetre");
            assert!(s.fit.pick(a).is_none() && s.fit.pick(b).is_some(), "after Clear a new pair must start a fresh fit");
            s.fit.request = Some(4);
        });
        set_tool(&mut cx, &mut state, Tool::Select);
        session::with(|s| {
            assert!(empty(&s.fit), "leaving Measure Fit must discard the draft");
            assert!(!s.fit.accept(&mut cx, 4, &Err("late".into())), "a reply after leaving must stay discarded");
        });
    }
}

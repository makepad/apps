//! A momentary 3D input owns its pointer until release or cancellation, independently of a GPU;
//! so does a knob turned by a drag.

use fab_elec_bench::SimCmd;
use fab_elec_core::CompRef;

#[derive(Default)]
pub(super) struct LivePress {
    held: Option<CompRef>,
}

impl LivePress {
    /// A momentary input takes a press in every run state, as the CONTROLS strip's keys do: a
    /// press while the bench rests re-records, as a turn does.
    pub(super) fn allowed(comp: CompRef, momentary: &[CompRef]) -> bool {
        momentary.contains(&comp)
    }

    pub(super) fn down(&mut self, comp: CompRef, momentary: &[CompRef]) -> Option<SimCmd> {
        if self.held.is_some() || !Self::allowed(comp, momentary) { return None; }
        self.held = Some(comp);
        Some(SimCmd::press(comp, true))
    }

    pub(super) fn claimed(&self) -> bool { self.held.is_some() }

    /// All terminal pointer paths use this, including a cancelled or captured-away pointer.
    /// Release remains mandatory even if play or the lowering changed since the press.
    pub(super) fn up(&mut self) -> Option<SimCmd> {
        self.held.take().map(|comp| SimCmd::press(comp, false))
    }
}

/// Points of vertical drag that turn a knob through its whole travel (the CONTROLS strip's).
pub(super) const TURN_PT: f64 = 150.0;

/// Shift turns finely: this share of the drag's turn.
pub(super) const FINE: f64 = 0.2;

/// The smallest turn sent: a drag sends at most one place a pointer sample, and none for a move
/// under this, so a held pointer that trembles sends nothing (the host keeps only the newest of
/// any still queued).
const TURN_STEP: f64 = 1.0 / 512.0;

/// A knob turned by a vertical drag: up turns it toward its end at 1 (its B pin), down toward 0.
/// It owns the pointer from the press to the release or cancellation; a release sends nothing.
#[derive(Default)]
pub(super) struct LiveTurn {
    /// The knob, its place where the drag (or its fine part) began, that height, the place last
    /// sent, and whether the drag turns finely.
    turning: Option<(CompRef, f64, f64, f64, bool)>,
}

impl LiveTurn {
    pub(super) fn down(&mut self, comp: CompRef, at: f64, y: f64) {
        self.turning = Some((comp, at, y, at, false));
    }

    pub(super) fn claimed(&self) -> bool { self.turning.is_some() }

    /// The pointer at height `y` (points, down positive), Shift held for `fine`: the knob's new
    /// place, when it moved a step. A change of Shift starts afresh from the place last sent, so
    /// the knob never jumps.
    pub(super) fn moved(&mut self, y: f64, fine: bool) -> Option<SimCmd> {
        let (comp, at, y0, sent, was_fine) = self.turning.as_mut()?;
        if fine != *was_fine {
            (*at, *y0, *was_fine) = (*sent, y, fine);
        }
        let scale = if fine { FINE } else { 1.0 };
        let value = (*at + (*y0 - y) * scale / TURN_PT).clamp(0.0, 1.0);
        // an end is always reached, however small the last step to it
        let end = value == 0.0 || value == 1.0;
        if value == *sent || ((value - *sent).abs() < TURN_STEP && !end) {
            return None;
        }
        *sent = value;
        Some(SimCmd::SetParam { comp: *comp, value })
    }

    pub(super) fn up(&mut self) -> bool {
        self.turning.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::{BoardId, CompId};

    #[test]
    fn a_momentary_input_claims_the_pointer_in_every_run_state() {
        let comp = CompRef { board: BoardId(0), comp: CompId(0) };
        let mut press = LivePress::default();
        assert_eq!(press.down(comp, &[]), None, "a non-momentary input sends nothing");
        assert!(!press.claimed(), "a refused press leaves the navigator its pointer");
        assert_eq!(press.up(), None, "a refused press sends no release");
        assert_eq!(press.down(comp, &[comp]), Some(SimCmd::press(comp, true)), "a button sends down, the bench playing or resting");
        assert!(press.claimed(), "an accepted press owns the pointer before navigation");
        assert_eq!(press.down(comp, &[comp]), None, "another pointer cannot replace a held press");
        assert_eq!(press.up(), Some(SimCmd::press(comp, false)), "release and cancellation both send up");
        assert!(!press.claimed(), "release ends the claim");
        assert_eq!(press.up(), None, "a duplicate terminal event cannot release twice");
        let other = CompRef { board: BoardId(0), comp: CompId(1) };
        assert_eq!(press.down(other, &[comp, other]), Some(SimCmd::press(other, true)), "a second button takes the next press");
        assert_eq!(press.up(), Some(SimCmd::press(other, false)), "and its own release");
    }

    #[test]
    fn a_drag_turns_a_knob_by_its_height_in_steps_within_its_travel() {
        let comp = CompRef { board: BoardId(0), comp: CompId(3) };
        let mut turn = LiveTurn::default();
        assert_eq!(turn.moved(10.0, false), None, "no knob held, no turn");
        turn.down(comp, 0.25, 300.0);
        assert!(turn.claimed(), "a knob's press owns the pointer");
        assert_eq!(turn.moved(300.2, false), None, "a tremble under a step sends nothing");
        assert_eq!(turn.moved(300.0 - TURN_PT / 2.0, false), Some(SimCmd::SetParam { comp, value: 0.75 }), "up half the travel's drag turns it half its travel");
        assert_eq!(turn.moved(300.0 - TURN_PT / 2.0, true), None, "pressing Shift alone does not move the knob");
        let place = |cmd: Option<SimCmd>| match cmd { Some(SimCmd::SetParam { value, .. }) => value, other => panic!("a turn sends the knob's place, not {other:?}") };
        let fine = place(turn.moved(300.0 - TURN_PT, true));
        assert!((fine - (0.75 + FINE * 0.5)).abs() < 1e-9, "with Shift a drag turns finely from where the knob is: {fine}");
        assert_eq!(turn.moved(300.0 - TURN_PT, false), None, "letting go of Shift alone does not move the knob");
        let full = place(turn.moved(300.0 - TURN_PT * 1.1, false));
        assert!((full - (fine + 0.1)).abs() < 1e-9, "without Shift the drag carries on at full rate from there: {full}");
        assert_eq!(turn.moved(-500.0, false), Some(SimCmd::SetParam { comp, value: 1.0 }), "a drag past the end stops at it");
        assert_eq!(turn.moved(-900.0, false), None, "at the end, further up sends nothing");
        assert_eq!(turn.moved(2000.0, false), Some(SimCmd::SetParam { comp, value: 0.0 }), "down to the other end");
        assert!(turn.up() && !turn.claimed() && !turn.up(), "release ends the turn once");
    }
}

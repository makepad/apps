//! How a handle's drag turns the pointer into a value, for every placement handle (a part's or a
//! feature's move, turn and scale, a settings box's move and resize): continuous by default;
//! Shift goes at a tenth of the speed from where it was pressed (or from where Shift came on,
//! so the value never jumps); Cmd (Ctrl off a Mac) snaps the value itself to whole steps (1 mm,
//! 5°, 5 %). The handle shows the value by the pointer while it drags ([`readout_rect`]).

use makepad_widgets_core::*;

/// Whole millimetres.
pub const SNAP_MM: f64 = 1.0;
/// Five degrees.
pub const SNAP_DEG: f64 = 5.0;
/// Five per cent (of a scale factor).
pub const SNAP_SCALE: f64 = 0.05;
/// Shift's share of the pointer's speed.
pub const FINE: f64 = 0.1;

/// One value a drag sets. `raw` is what the pointer says at full speed (the value at the press
/// plus the drag so far); [`Fine::value`] is what the handle sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fine {
    /// Where the current speed took over: the raw value then and the value it gave.
    from_raw: f64,
    from: f64,
    fine: bool,
    /// The value last given, before a snap.
    last: f64,
}

impl Fine {
    /// A drag pressed at `value` (the pointer's raw value there).
    pub fn new(value: f64) -> Fine {
        Fine::at(value, value)
    }

    /// A drag pressed where the pointer's raw value is `raw`, on a handle that holds `value`
    /// (a grip a little off the value's own point): the value moves from where it is, by the
    /// pointer's moves, and never jumps to the pointer.
    pub fn at(raw: f64, value: f64) -> Fine {
        Fine { from_raw: raw, from: value, fine: false, last: value }
    }

    /// The value for the pointer's `raw` under `mods`: a tenth of its moves with Shift, snapped
    /// to a whole `step` with Cmd/Ctrl.
    pub fn value(&mut self, raw: f64, mods: &KeyModifiers, step: f64) -> f64 {
        self.value_with(raw, mods.shift, mods.is_primary(), step)
    }

    /// [`Self::value`] with the two modifiers given.
    pub fn value_with(&mut self, raw: f64, fine: bool, snap: bool, step: f64) -> f64 {
        if fine != self.fine {
            // the speed changes here: on from the value it gives now
            (self.from_raw, self.from, self.fine) = (raw, self.last, fine);
        }
        let v = self.from + (raw - self.from_raw) * if fine { FINE } else { 1.0 };
        self.last = v;
        if snap { (v / step).round() * step } else { v }
    }
}

/// Where a drag's value of `size` shows by the pointer: just off it, up and to the right; to its
/// left where that would run past the view's right edge, under it past the top, and inside
/// `view` near every edge.
pub fn readout_rect(pointer: DVec2, size: DVec2, view: Rect) -> Rect {
    let mut pos = pointer + dvec2(14.0, -6.0 - size.y);
    if pos.x + size.x > view.pos.x + view.size.x {
        pos.x = pointer.x - 14.0 - size.x;
    }
    if pos.y < view.pos.y {
        pos.y = pointer.y + 14.0;
    }
    inside(Rect { pos, size }, view)
}

/// `r` moved the least way to lie inside `view` (its top left inside, when it is the larger).
pub fn inside(r: Rect, view: Rect) -> Rect {
    let x = r.pos.x.min(view.pos.x + view.size.x - r.size.x).max(view.pos.x);
    let y = r.pos.y.min(view.pos.y + view.size.y - r.size.y).max(view.pos.y);
    Rect { pos: dvec2(x, y), size: r.size }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drag's value shows just off the pointer, up and to the right, and stays inside the view
    /// near every edge and corner, never over the pointer: to its left at the right edge, under
    /// it at the top.
    #[test]
    fn a_readout_stays_inside_the_view_near_every_edge() {
        let view = Rect { pos: dvec2(100.0, 50.0), size: dvec2(800.0, 600.0) };
        let size = dvec2(120.0, 22.0);
        assert_eq!(readout_rect(dvec2(500.0, 300.0), size, view).pos, dvec2(514.0, 272.0), "up and to the right of the pointer");
        for p in [(895.0, 300.0), (500.0, 52.0), (102.0, 300.0), (500.0, 648.0), (895.0, 52.0), (102.0, 52.0), (895.0, 648.0), (102.0, 648.0)].map(|(x, y)| dvec2(x, y)) {
            let r = readout_rect(p, size, view);
            let within = r.pos.x >= view.pos.x && r.pos.y >= view.pos.y && r.pos.x + r.size.x <= view.pos.x + view.size.x && r.pos.y + r.size.y <= view.pos.y + view.size.y;
            assert!(within, "by the pointer at {p:?} the readout {r:?} runs out of the view");
            let over = p.x >= r.pos.x && p.x <= r.pos.x + r.size.x && p.y >= r.pos.y && p.y <= r.pos.y + r.size.y;
            assert!(!over, "by the pointer at {p:?} the readout {r:?} covers it");
        }
    }

    /// A drag is continuous: the value follows the pointer exactly, with no steps.
    #[test]
    fn a_drag_follows_the_pointer_continuously() {
        let mut f = Fine::new(10.0);
        assert_eq!(f.value_with(10.37, false, false, SNAP_MM), 10.37);
        assert_eq!(f.value_with(12.01, false, false, SNAP_MM), 12.01);
    }

    /// Shift goes at a tenth of the speed from where it came on, without a jump either way.
    #[test]
    fn shift_goes_at_a_tenth_from_where_it_came_on() {
        let mut f = Fine::new(0.0);
        assert_eq!(f.value_with(4.0, false, false, SNAP_MM), 4.0);
        // on at raw 4: the value stays 4, then moves a tenth
        assert_eq!(f.value_with(4.0, true, false, SNAP_MM), 4.0);
        assert!((f.value_with(6.0, true, false, SNAP_MM) - 4.2).abs() < 1e-12);
        // off again at raw 6: on at full speed from 4.2
        assert!((f.value_with(6.0, false, false, SNAP_MM) - 4.2).abs() < 1e-12);
        assert!((f.value_with(7.0, false, false, SNAP_MM) - 5.2).abs() < 1e-12);
    }

    /// Cmd/Ctrl snaps the value itself to whole steps (not the drag's change), and letting go of
    /// it gives the free value back.
    #[test]
    fn cmd_snaps_the_value_to_whole_steps() {
        let mut f = Fine::new(10.3);
        assert_eq!(f.value_with(12.6, false, true, SNAP_MM), 13.0);
        assert_eq!(f.value_with(17.4, false, true, SNAP_DEG), 15.0);
        assert!((f.value_with(1.12, false, true, SNAP_SCALE) - 1.1).abs() < 1e-12);
        assert_eq!(f.value_with(12.6, false, false, SNAP_MM), 12.6);
        // both: fine and snapped
        let mut f = Fine::new(0.0);
        assert_eq!(f.value_with(0.0, true, true, SNAP_MM), 0.0);
        assert_eq!(f.value_with(14.0, true, true, SNAP_MM), 1.0);
    }
}

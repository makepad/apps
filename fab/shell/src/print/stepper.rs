//! Stepping through the items of one list row: the previous and next buttons at the row's right
//! with "3 / 8" between them (the Printability list's kinds of issue, a cluster of overhangs).

use makepad_widgets_core::*;

/// Where a step from `at` lands among `n` items, wrapping round. From none, forward starts at
/// the first and back at the last.
pub fn step(n: usize, at: Option<usize>, forward: bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    Some(match at.filter(|&a| a < n) {
        Some(a) if forward => (a + 1) % n,
        Some(a) => (a + n - 1) % n,
        None if forward => 0,
        None => n - 1,
    })
}

/// A row's count: "8", or "3 / 8" while the third of them is shown.
pub fn count_text(n: usize, at: Option<usize>) -> String {
    match at {
        Some(a) => format!("{} / {n}", a + 1),
        None => format!("{n}"),
    }
}

/// The part of a row the pointer is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowPart {
    Row,
    Prev,
    Next,
}

pub const BUTTON_W: f64 = 16.0;
const GAP: f64 = 3.0;

/// The previous and next buttons of `line`, ending at its right edge, around a count `count_w`
/// wide.
pub fn buttons(line: Rect, count_w: f64) -> (Rect, Rect) {
    let right = line.pos.x + line.size.x;
    let next = Rect { pos: dvec2(right - BUTTON_W, line.pos.y), size: dvec2(BUTTON_W, line.size.y) };
    let prev = Rect { pos: dvec2(next.pos.x - GAP - count_w - GAP - BUTTON_W, line.pos.y), size: dvec2(BUTTON_W, line.size.y) };
    (prev, next)
}

/// Where the count stands between the buttons.
pub fn count_x(prev: Rect) -> f64 {
    prev.pos.x + BUTTON_W + GAP
}

/// The icon inside a button.
pub fn icon(button: Rect) -> Rect {
    let side = 12.0;
    Rect { pos: dvec2(button.pos.x + (button.size.x - side) * 0.5, button.pos.y + (button.size.y - side) * 0.5), size: dvec2(side, side) }
}

/// The part of a row with buttons `steps` that `abs` is on.
pub fn part_at(steps: Option<(Rect, Rect)>, abs: DVec2) -> RowPart {
    match steps {
        Some((prev, _)) if prev.contains(abs) => RowPart::Prev,
        Some((_, next)) if next.contains(abs) => RowPart::Next,
        _ => RowPart::Row,
    }
}

/// An arrow key's step while a row has the focus: forward for Right and Down.
pub fn key_step(key: KeyCode) -> Option<bool> {
    match key {
        KeyCode::ArrowRight | KeyCode::ArrowDown => Some(true),
        KeyCode::ArrowLeft | KeyCode::ArrowUp => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_wraps_both_ways_and_starts_at_an_end() {
        assert_eq!(step(0, None, true), None);
        assert_eq!(step(3, None, true), Some(0));
        assert_eq!(step(3, None, false), Some(2));
        assert_eq!(step(3, Some(2), true), Some(0));
        assert_eq!(step(3, Some(0), false), Some(2));
        assert_eq!(step(3, Some(1), true), Some(2));
        assert_eq!(step(3, Some(7), true), Some(0), "a place past the end is none");
        assert_eq!(count_text(8, None), "8");
        assert_eq!(count_text(8, Some(2)), "3 / 8");
    }
}

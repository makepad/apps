//! Present mode: how a deck is put on the displays. Pure planning, so the
//! two-display case is checked without a second display.

use makepad_widgets::*;

/// How present mode runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PresentMode {
    /// One display: the picture alone, fullscreen, in Stage's own window.
    Single,
    /// Two displays: the output window fullscreen on the other display, and
    /// Stage's window is the presenter view.
    Dual,
}

/// What present mode needs the app to do with the output window (the app
/// owns it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PresentAsk {
    /// Open the output window if it is not open, put it on this display and
    /// make it fullscreen there.
    OpenOutput { screen: ScreenGeom },
    /// Present mode ended: close the output window when present mode opened
    /// it, else leave fullscreen.
    CloseOutput,
}

/// The output window while present mode owns it.
#[derive(Clone, Copy, Debug)]
pub struct PresentOutput {
    pub screen: ScreenGeom,
    /// Present mode opened the window (and closes it again).
    pub opened_here: bool,
    /// It has been put on the display and made fullscreen.
    pub placed: bool,
}

/// The display the output window goes to: any display other than the one
/// Stage's window is on. None with fewer than two displays (a platform that
/// cannot list them has none).
pub fn output_screen(screens: &[ScreenGeom], window_at: Vec2d) -> Option<ScreenGeom> {
    if screens.len() < 2 {
        return None;
    }
    let home = screens.iter().position(|screen| screen.bounds.contains(window_at)).or_else(|| screens.iter().position(|screen| screen.is_primary)).unwrap_or(0);
    screens.iter().enumerate().find(|(index, _)| *index != home).map(|(_, screen)| *screen)
}

/// The mode and the output display for the displays there are.
pub fn plan(screens: &[ScreenGeom], window_at: Vec2d) -> (PresentMode, Option<ScreenGeom>) {
    match output_screen(screens, window_at) {
        Some(screen) => (PresentMode::Dual, Some(screen)),
        None => (PresentMode::Single, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: f64, y: f64, primary: bool) -> ScreenGeom {
        let bounds = Rect { pos: dvec2(x, y), size: dvec2(1920.0, 1080.0) };
        ScreenGeom { bounds, work_area: bounds, is_primary: primary }
    }

    #[test]
    fn one_display_or_none_is_single() {
        assert_eq!(plan(&[], dvec2(10.0, 10.0)).0, PresentMode::Single);
        assert_eq!(plan(&[screen(0.0, 0.0, true)], dvec2(10.0, 10.0)), (PresentMode::Single, None));
    }

    #[test]
    fn two_displays_put_the_output_on_the_other_one() {
        let screens = [screen(0.0, 0.0, true), screen(1920.0, 0.0, false)];
        let (mode, out) = plan(&screens, dvec2(100.0, 100.0));
        assert_eq!((mode, out), (PresentMode::Dual, Some(screens[1])));
        // Stage's window on the second display: the output goes to the first.
        assert_eq!(plan(&screens, dvec2(2500.0, 100.0)).1, Some(screens[0]));
        // A window off every display counts as on the primary.
        assert_eq!(plan(&screens, dvec2(-5000.0, 0.0)).1, Some(screens[1]));
    }

    #[test]
    fn three_displays_take_the_first_other_one() {
        let screens = [screen(0.0, 0.0, true), screen(1920.0, 0.0, false), screen(3840.0, 0.0, false)];
        assert_eq!(plan(&screens, dvec2(2000.0, 10.0)).1, Some(screens[0]));
    }
}

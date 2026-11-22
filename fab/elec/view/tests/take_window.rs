use fab_elec_view::trace::take_window::{ruler_ticks, scroll_pans, TakeWindow, MIN_SPAN};
use std::collections::HashSet;

#[test]
fn navigate_take() {
    let end = 2.0;
    let full = TakeWindow::fit(end);
    let inner = TakeWindow { t0: 0.25, t1: 1.25 };
    let left = TakeWindow { t0: 0.0, t1: 1.0 };
    let right = TakeWindow { t0: 1.0, t1: end };
    let (x0, width, at) = (80.0, 1400.0, 0.625);
    let x = inner.x_of(at, x0, width);
    let zoomed = inner.zoom(at, 4.0, end);
    assert_eq!(x, x0 + 0.375 * width, "time maps to its offset from the plot origin");
    assert_eq!(inner.t_of(x, x0, width), at, "the pointer maps back to its time");
    assert!((zoomed.x_of(at, x0, width) - x).abs() <= 1e-9 * width, "zoom holds the pointer time in place");
    assert_eq!(zoomed.span(), inner.span() / 4.0, "zoom divides the span by its factor");
    assert_eq!(zoomed.zoom(at, 0.25, end), inner, "the inverse zoom restores an interior window");
    assert_eq!(inner.zoom(at, 0.125, end), full, "zooming past the take fits it");
    assert_eq!(full.zoom(0.0, f64::MAX, end).span(), MIN_SPAN, "zoom stops at the minimum span");
    let short = MIN_SPAN / 2.0;
    assert_eq!(inner.clamp(short), TakeWindow::fit(short), "a take shorter than the minimum stays fitted");
    let fine = TakeWindow::fit(100.0).zoom(100.0, f64::MAX, 100.0);
    assert!(fine.t1 == 100.0 && fine.t0 == fine.t1.next_down(), "the smallest visible interval at a late end stays representable");

    assert_eq!(TakeWindow { t0: -0.5, t1: 0.5 }.clamp(end), left, "clamp pushes the start inside");
    assert_eq!(TakeWindow { t0: 1.5, t1: 2.5 }.clamp(end), right, "clamp pushes the end inside");
    assert_eq!(inner.pan(0.25, end), TakeWindow { t0: 0.5, t1: 1.5 }, "pan moves both edges by the requested time");
    assert_eq!(inner.pan(-10.0, end), left, "pan stops at zero with the original span");
    assert_eq!(inner.pan(10.0, end), right, "pan stops at the end with the original span");
    // the gestures that pan (user 11-18: "i can zoom in to the scope but i cant pan")
    assert_eq!(inner.dragged(width / 4.0, width, end), TakeWindow { t0: 0.0, t1: 1.0 }, "a ruler drag to the right shows earlier times, the grabbed instant under the pointer");
    assert_eq!(inner.dragged(-width / 4.0, width, end), TakeWindow { t0: 0.5, t1: 1.5 }, "a ruler drag to the left shows later times");
    assert_eq!(inner.dragged(10.0 * width, width, end), left, "a long drag stops at the take's start");
    assert_eq!(scroll_pans(-12.0, 1.0, false), Some(-12.0), "a trackpad's sideways swipe pans");
    assert_eq!(scroll_pans(0.0, 40.0, true), Some(40.0), "Shift and the wheel pans");
    assert_eq!(scroll_pans(0.0, 40.0, false), None, "the wheel alone zooms");
    assert_eq!(inner.scrub_at(x, x0, width, end), at, "scrub returns the pointer time");
    assert_eq!(inner.scrub_at(x0 - width / 8.0, x0, width, end), 0.125, "scrub can reach outside the visible window");
    assert_eq!(inner.scrub_at(x0 - width, x0, width, end), 0.0, "scrub stops at the take start");
    assert_eq!(inner.scrub_at(x0 + 3.0 * width, x0, width, end), end, "scrub stops at the take end");

    let grown = 4.0;
    assert_eq!(right.rebound(end, grown), TakeWindow { t0: 3.0, t1: grown }, "a trailing window follows the new end with its span");
    let near = TakeWindow { t1: end.next_down(), ..right }.rebound(end, grown);
    assert_eq!(near.t1, grown, "a window within one minimum span follows the end");
    assert_eq!(inner.rebound(end, grown), inner, "an interior window stays in place as the take grows");
    assert_eq!(full.rebound(end, grown), TakeWindow::fit(grown), "a fitted window stays fitted");
    assert_eq!(TakeWindow::fit(0.0).rebound(0.0, end), full, "an empty fitted record follows its first samples");
    assert_eq!(inner.rebound(end, 0.5), TakeWindow::fit(0.5), "a shortened take clamps the old window");

    for bad in [0.0, -1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        assert_eq!(TakeWindow::fit(bad), TakeWindow { t0: 0.0, t1: MIN_SPAN }, "an invalid take fits to the minimum");
        assert_eq!(inner.zoom(at, bad, end), inner, "an invalid factor leaves zoom unchanged");
        assert_eq!(inner.zoom(at, 2.0, bad), inner, "an invalid end leaves zoom unchanged");
        assert_eq!(inner.pan(0.5, bad), inner, "an invalid end leaves pan unchanged");
        assert_eq!(inner.clamp(bad), inner, "an invalid end leaves clamp unchanged");
        assert_eq!(inner.rebound(end, bad), inner, "an invalid end leaves rebound unchanged");
    }
}

#[test]
fn time_ruler_geometry() {
    fn measure(label: &str) -> f64 {
        label.chars().count() as f64 * 7.0
    }
    let gap = 8.0;
    let full = TakeWindow::fit(2.0);
    let detail = TakeWindow { t0: 25e-6, t1: 75e-6 };
    for (window, width) in [(full, 1400.0), (full, 300.0), (detail, 300.0)] {
        let ticks = ruler_ticks(window, width, gap, &mut measure);
        assert!(ticks.len() >= 2, "the ruler shows multiple ticks at {width} points over {window:?}");
        let mut labels = HashSet::new();
        for tick in &ticks {
            assert!((window.t0..window.t1).contains(&tick.value), "tick time lies inside the window");
            assert!((tick.at - window.x_of(tick.value, 0.0, width)).abs() <= 1e-9 * width, "tick positions match the time mapping");
            assert!(!tick.label.is_empty() && labels.insert(&tick.label), "each tick has a distinct time label");
        }
        for pair in ticks.windows(2) {
            assert!(pair[0].value < pair[1].value && pair[0].at < pair[1].at, "tick times and positions increase");
            let half_widths = (measure(&pair[0].label) + measure(&pair[1].label)) * 0.5;
            assert!(pair[1].at - pair[0].at >= half_widths + gap, "centred labels leave the requested gap");
        }
    }
}

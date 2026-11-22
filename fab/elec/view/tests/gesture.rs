//! Gesture properties, without a window, feed or raw acquisition record.

use fab_elec_bench::trigger::Window;
use fab_elec_view::instruments::gesture::{drag_db_reference, drag_waterfall_share, fit_time, fit_values, lock_axis, waterfall_split, zoom_db_about, zoom_time_about, zoom_value_about, Axis};
use fab_elec_view::instruments::scope::FftSettings;
use makepad_widgets_core::dvec2;

fn near(actual: f64, expected: f64, scale: f64, property: &str) {
    assert!((actual - expected).abs() <= scale.abs().max(f64::MIN_POSITIVE) * 1e-12,
        "{property}: got {actual}, expected {expected}");
}

#[test]
fn time_zoom_preserves_pointer_and_reverses() {
    let window = Window { pre: 35e-6, post: 65e-6 };
    let span = window.pre + window.post;
    for at in [-window.pre, 0.0, window.post] {
        for factor in [0.4, 2.5] {
            let zoomed = zoom_time_about(window, at, factor);
            near((at + zoomed.pre) / (zoomed.pre + zoomed.post), (at + window.pre) / span, 1.0,
                "time zoom keeps the pointer fraction at left, trigger and right");
            near(zoomed.pre + zoomed.post, span * factor, span * factor, "time zoom applies the continuous factor");
            let back = zoom_time_about(zoomed, at, 1.0 / factor);
            near(back.pre, window.pre, span, "inverse time zoom restores the left edge");
            near(back.post, window.post, span, "inverse time zoom restores the right edge");
        }
    }
}

#[test]
fn value_zoom_preserves_pointer_and_reverses() {
    for span in [(0.0, 3.3), (-8.0, -2.0), (-2.0, 5.0)] {
        let width = span.1 - span.0;
        for at in [span.0, span.0 + width * 0.35, span.1] {
            for factor in [0.4, 2.5] {
                let zoomed = zoom_value_about(span, at, factor);
                near((at - zoomed.0) / (zoomed.1 - zoomed.0), (at - span.0) / width, 1.0,
                    "value zoom keeps the pointer fraction, including negative values");
                near(zoomed.1 - zoomed.0, width * factor, width * factor, "value zoom applies the continuous factor");
                let back = zoom_value_about(zoomed, at, 1.0 / factor);
                near(back.0, span.0, width, "inverse value zoom restores the low end");
                near(back.1, span.1, width, "inverse value zoom restores the high end");
            }
        }
    }
}

#[test]
fn db_zoom_and_drag_preserve_the_pointer_mapping() {
    let (reference, scale, height) = (-12.0, 5.0, 400.0);
    for divisions in [0.0, 3.5, 10.0] {
        let at = reference - divisions * scale;
        for factor in [0.5, 2.0] {
            let (top, db) = zoom_db_about(reference, scale, at, factor);
            near(top - divisions * db, at, 10.0 * scale, "dB zoom keeps the level under the pointer");
            near(db, scale * factor, scale, "dB zoom scales continuously");
            assert_eq!(zoom_db_about(top, db, at, 1.0 / factor), (reference, scale), "inverse dB zoom restores reference and divisions exactly");
        }
        for (factor, limit) in [(1e-6, 0.1), (1e6, 50.0)] {
            let (top, db) = zoom_db_about(reference, scale, at, factor);
            assert_eq!(db, limit, "dB zoom clamps at the requested lower or upper limit");
            near(top - divisions * db, at, 10.0 * scale, "clamped dB zoom still keeps the pointer level");
            assert_eq!(zoom_db_about(top, db, at, factor), (top, db), "further zoom at either dB limit leaves the reference unchanged");
        }
        for sign in [-1.0, 1.0] {
            let dy = sign * divisions * height / 10.0;
            let top = drag_db_reference(reference, scale, dy, height);
            near(top - reference, sign * divisions * scale, 10.0 * scale, "N dragged divisions move the reference by N times dB per division");
            near((top - at) / (10.0 * scale) * height, divisions / 10.0 * height + dy, height, "a fixed dB level follows the dragged picture");
            assert_eq!(drag_db_reference(top, scale, -dy, height), reference, "inverse dB drag restores the reference exactly");
        }
    }
    assert_eq!(zoom_db_about(reference, scale, 0.0, 1.0), (reference, scale), "identity dB zoom preserves the mapping");
    assert_eq!(zoom_db_about(reference, scale, 0.0, f64::NAN), (reference, scale), "invalid dB zoom leaves the mapping intact");
    assert_eq!(drag_db_reference(reference, scale, 10.0, 0.0), reference, "an undrawn plot cannot move the dB reference");
}

#[test]
fn waterfall_split_and_drag_preserve_height_and_bound_the_share() {
    let available = 300.0;
    let start = FftSettings::default().waterfall_share;
    let (plot, waterfall) = waterfall_split(available, start);
    assert_eq!(start, 1.0 / 3.0, "default waterfall share is one third");
    near(plot, available / 1.5, available, "default spectrum keeps today's two-thirds height");
    near(waterfall, available / 3.0, available, "default waterfall keeps today's one-third height");
    for dy in [-25.0, 0.0, 25.0, 100.0] {
        let share = drag_waterfall_share(start, dy, available);
        let (next_plot, next_waterfall) = waterfall_split(available, share);
        near(next_waterfall - waterfall, dy, available, "unclamped drag grows the waterfall by dy");
        near(next_plot - plot, -dy, available, "the divider follows upward pointer motion by dy");
        near(next_plot + next_waterfall, available, available, "drag preserves the total available height");
        near(drag_waterfall_share(share, -dy, available), start, 1.0, "unclamped reverse drag restores the starting share");
    }
    for (share, limit, dy) in [(-1.0, 0.15, -available), (2.0, 0.75, available)] {
        let (plot, waterfall) = waterfall_split(available, share);
        near(waterfall, available * limit, available, "split clamps the waterfall at both share limits");
        near(plot + waterfall, available, available, "clamped split preserves the total height");
        assert_eq!(drag_waterfall_share(start, dy, available), limit, "drag clamps at both share limits");
        assert_eq!(drag_waterfall_share(limit, dy, available), limit, "continued outward drag holds each limit");
    }
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(drag_waterfall_share(start, invalid, available), start, "nonfinite displacement preserves the starting share");
        assert_eq!(drag_waterfall_share(start, 25.0, invalid), start, "nonfinite available height preserves the starting share");
        assert_eq!(drag_waterfall_share(invalid, 25.0, available).to_bits(), invalid.to_bits(), "nonfinite starting share is returned unchanged");
        assert_eq!(waterfall_split(available, invalid), (plot, waterfall), "nonfinite split share falls back to the default layout");
        assert_eq!(waterfall_split(invalid, start), (0.0, 0.0), "nonfinite available height produces no picture area");
    }
    for empty in [0.0, -1.0] {
        assert_eq!(drag_waterfall_share(start, 25.0, empty), start, "nonpositive available height cannot move the divider");
        assert_eq!(waterfall_split(empty, start), (0.0, 0.0), "nonpositive available height produces no picture area");
    }
}

#[test]
fn pan_locks_after_slop_and_shift_frees_both_axes() {
    for free in [false, true] {
        assert_eq!(lock_axis(dvec2(2.0, 2.0), 4.0, free), None, "movement inside slop never pans");
        assert_eq!(lock_axis(dvec2(4.0, 0.0), 4.0, free), None, "movement at slop waits for intent");
    }
    for sign in [-1.0, 1.0] {
        let horizontal = dvec2(sign * 5.0, 2.0);
        let vertical = dvec2(2.0, sign * 5.0);
        assert_eq!(lock_axis(horizontal, 4.0, false), Some(Axis::Horizontal), "dominant horizontal motion locks time");
        assert_eq!(lock_axis(vertical, 4.0, false), Some(Axis::Vertical), "dominant vertical motion locks value");
        assert_eq!(lock_axis(horizontal, 4.0, true), Some(Axis::Both), "Shift frees a horizontal gesture");
        assert_eq!(lock_axis(vertical, 4.0, true), Some(Axis::Both), "Shift frees a vertical gesture");
    }
}

#[test]
fn fits_cover_signal_and_preserve_trigger_position() {
    for (min, max) in [(0.0, 3.3), (3.299, 3.301), (-4.0, -1.0)] {
        let (scale, offset) = fit_values(min, max);
        let (lo, hi) = ((offset - 4.0) * scale, (offset + 4.0) * scale);
        assert!(scale.is_finite() && scale > 0.0 && offset.is_finite(), "signal fit produces finite positive divisions");
        assert!(lo < min && hi > max, "square, DC ripple and negative signals have headroom on both sides");
        near(8.0 * scale, (max - min) * 1.1, max - min, "signal fit adds ten percent total headroom");
        near(offset * scale, (min + max) * 0.5, max.abs().max(1.0), "signal fit centres on its extrema");
    }
    for dc in [0.0, 3.3, -3.3] {
        let (scale, offset) = fit_values(dc, dc);
        assert!(scale.is_finite() && scale > 0.0 && offset.is_finite(), "flat signal fit never has a zero or invalid span");
        assert!((offset - 4.0) * scale < dc && (offset + 4.0) * scale > dc, "flat signal fit surrounds its DC value");
    }
    let current = Window { pre: 0.3e-3, post: 0.7e-3 };
    let fitted = fit_time(Some(100e-6), current);
    let span = fitted.pre + fitted.post;
    assert!((100e-6..=300e-6).contains(&span), "time fit shows one to three periods");
    near(fitted.pre / span, current.pre / (current.pre + current.post), 1.0, "time fit preserves trigger position");
    assert_eq!(fit_time(None, current), current, "no measured period leaves the time window unchanged");
    assert_eq!(fit_time(Some(f64::NAN), current), current, "an invalid measured period leaves the time window unchanged");
}

#[test]
fn retuned_frames_carry_actual_axes_even_without_a_new_generation() {
    use fab_elec_view::instruments::{
        acquire::LaneFrame,
        phosphor::{Hits, PhosphorMap, Raster},
        phosphor_view::Pushed,
        scope_host::LaneDelivery,
        scrub::measurement_rows,
    };
    let before = PhosphorMap::screen(4, 4, 0.25, 1.0, -1.0, 1.0);
    let after = PhosphorMap::screen(4, 4, 0.25, 1.0, -2.0, 2.0);
    let mut raster = Raster::new();
    let mut old = Hits::new(4, 4);
    let mut new = Hits::new(4, 4);
    raster.add(&before, &[0.0], &[0.5], &mut old);
    raster.add(&after, &[0.0], &[0.5], &mut new);
    assert_eq!(old.map, Some(before), "a completed stroke stamps its original raster map");
    assert_eq!(new.map, Some(after), "a value retune stamps the new map without relying on generation");
    assert!(!old.mixed_maps && !new.mixed_maps, "homogeneous publications can end a matching stretch");
    let mut mixed = old.clone();
    mixed.add(&new);
    assert!(mixed.mixed_maps, "a merger identifies incompatible axes within one source generation");
    assert_eq!(mixed.waveforms, 2, "map provenance preserves existing acquisition counts");
    mixed.reset(4, 4);
    assert!(mixed.map.is_none() && !mixed.mixed_maps, "recycled storage forgets its previous map provenance");
    raster.add(&before, &[0.0], &[0.5], &mut mixed);
    raster.add(&after, &[0.0], &[0.5], &mut mixed);
    assert!(mixed.mixed_maps, "overlapping old and new windows cannot masquerade as one current-map frame");
    let mut delivery = LaneDelivery::default();
    let _ = delivery.receive(LaneFrame { generation: 0, published: None, spectra: vec![None], lanes: vec![old] });
    delivery.present(|_, _, hits| Pushed::WrongSize(hits));
    assert!(!delivery.waiting(), "refusing the old-map picture releases the delivery gate");
    let _ = delivery.receive(LaneFrame { generation: 0, published: None, spectra: vec![None], lanes: vec![new] });
    let frame = delivery.take().expect("delivery retains the newest envelope");
    assert_eq!(frame.lanes[0].map, Some(after), "delivery retains the new map after refusing the old picture");
    assert_eq!(frame.lanes[0].waveforms, 1, "held old-map counts never contaminate a new-map frame");
    assert!(!frame.lanes[0].mixed_maps, "a current-map frame remains usable after refusing the old picture");

    let rows = measurement_rows(&[0.0, 1.0, 2.0, 3.0], &[0.0, 0.0, 0.0, 3.3], 0.1);
    let value = |name| rows.iter().find(|row| row.name == name).map(|row| row.value).expect("fit measurement exists");
    assert_eq!(value("min"), 0.0, "cached extrema include the low rail of an asymmetric signal");
    assert_eq!(value("max"), f64::from(3.3f32), "cached extrema include the high rail independently of the mean");
    assert_eq!(value("max") - value("min"), value("Vpp"), "fit extrema and peak-to-peak share the same scan");
}

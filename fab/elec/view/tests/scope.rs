//! Scope settings and their acquisition actions, without a window.

use fab_elec_bench::trigger::Slope;
use fab_elec_view::instruments::scope::{div_steps, Channel, Domain, Persistence, ScopeAction, ScopeSettings, DIV_MAX, DIV_MIN, INTENSITY_MAX, INTENSITY_MIN};

#[test]
fn pinned_interval_actions_round_trip_through_settings_and_trigger_slot() {
    use fab_elec_bench::trigger::Qualifier;
    use fab_elec_view::instruments::{acquire::TriggerSlot, scope::{Measurement, SignalLane}, trend::TrendQuantity};
    use makepad_widgets_core::*;
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut lane = cx.with_vm(|vm| {
        makepad_platform::script::script_mod(vm);
        makepad_draw::script_mod(vm);
        makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
        fab_elec_view::live::script_mod(vm);
        fab_elec_view::trace::script_mod(vm);
        fab_elec_view::instruments::script_mod(vm);
        let value = vm.eval(script! {
            use mod.prelude.widgets_internal.*
            use mod.widgets.*
            SignalLane {}
        });
        SignalLane::script_from_value(vm, value)
    });
    lane.set_lane_measurements(&mut cx, &[Measurement::exact(0, "width", 12e-6, "s"), Measurement::exact(0, "freq", 1000.0, "Hz")], 0);
    lane.set_pins(&mut cx, vec![TrendQuantity::Width, TrendQuantity::Freq]);
    let answers = lane.source_answers();
    let width = answers.items.iter().find(|answer| answer.name == "width ").expect("a pinned width gets a numeric answer row even when absent from the default selection");
    assert_eq!((&*width.value, &*width.unit), ("12", "µs"));
    lane.set_pins(&mut cx, Vec::new());
    assert!(!lane.source_answers().items.iter().any(|answer| answer.name == "width "), "unpinning removes the extra width row");
    let slot = TriggerSlot::default();
    let mut seen = 0;
    let mut settings = ScopeSettings::default();
    for (quantity, value, shorter, qualifier, slope) in [
        (TrendQuantity::Width, 12e-6, true, Qualifier::Width { min: 0.0, max: 12e-6 }, Slope::Falling),
        (TrendQuantity::Width, 12e-6, false, Qualifier::Width { min: 12e-6, max: f64::INFINITY }, Slope::Falling),
        (TrendQuantity::Freq, 1000.0, true, Qualifier::Period { min: 0.0, max: 1e-3 }, Slope::Rising),
        (TrendQuantity::Freq, 1000.0, false, Qualifier::Period { min: 1e-3, max: f64::INFINITY }, Slope::Rising),
    ] {
        let actions = settings.trigger_from_answer(quantity, value, shorter);
        assert_eq!(actions.first(), Some(&ScopeAction::TriggerLane), "the popover selects its lane before publishing the qualifier");
        let ScopeAction::Reconfigure(snapshot) = actions.last().unwrap() else { panic!("answer action publishes settings") };
        assert_eq!(snapshot.trigger.qualify, qualifier);
        assert_eq!(snapshot.trigger.slope, slope);
        lane.set_settings(&mut cx, snapshot.clone());
        settings = lane.settings().clone();
        assert_eq!(settings.trigger.qualify, qualifier, "authoritative settings retain the qualifier");
        slot.set(settings.trigger(), qualifier, settings.auto_after(), None);
        let (version, trigger, received, auto, _) = slot.read(seen).expect("new settings are published");
        assert_eq!(trigger, settings.trigger());
        assert_eq!(received, qualifier, "both interval bounds and the kind cross the atomic slot");
        assert_eq!(auto, settings.auto_after());
        assert!(slot.read(version).is_none(), "unchanged settings are not replayed");
        seen = version;
    }
    let before = settings.clone();
    for qualifier in [Qualifier::Width { min: -1.0, max: 1.0 }, Qualifier::Period { min: 1.0, max: 1.0 }, Qualifier::Width { min: 0.0, max: f64::NAN }] {
        assert!(settings.set_trigger_qualifier(qualifier).is_empty(), "invalid qualifiers are ignored");
        assert_eq!(settings, before);
    }
    assert!(settings.trigger_from_answer(TrendQuantity::Freq, 0.0, true).is_empty());
    let actions = settings.set_trigger_qualifier(Qualifier::None);
    changed(actions, &settings, false, "clearing the qualifier preserves the image");
    slot.set(settings.trigger(), Qualifier::None, None, None);
    let (_, _, qualifier, auto, _) = slot.read(seen).unwrap();
    assert_eq!((qualifier, auto), (Qualifier::None, None));
}

fn near(actual: f64, expected: f64, property: &str) {
    assert!(
        (actual - expected).abs() <= expected.abs().max(1e-15) * 1e-12,
        "{property}: got {actual}, expected {expected}"
    );
}

fn changed(actions: Vec<ScopeAction>, settings: &ScopeSettings, clear: bool, property: &str) {
    let expected = if clear {
        vec![ScopeAction::Clear, ScopeAction::Reconfigure(settings.clone())]
    } else {
        vec![ScopeAction::Reconfigure(settings.clone())]
    };
    assert_eq!(actions, expected, "{property}: the host receives the complete settings after any clear");
}

#[test]
fn scope_model() {
    let mut value = DIV_MIN;
    let mut menu = vec![value];
    while value < DIV_MAX {
        let next = div_steps(value, 1);
        assert!(next > value && next <= DIV_MAX, "the next division advances within the menu");
        near(div_steps(next, -1), value, "every increasing step reverses");
        menu.push(next);
        value = next;
    }
    assert_eq!(menu.len(), 73, "there are three steps per decade and the final endpoint");
    for (i, value) in menu.iter().enumerate() {
        near(*value, [1.0, 2.0, 5.0][i % 3] * 10.0_f64.powi(i as i32 / 3 - 12), "1-2-5 mantissa");
        near(div_steps(*value, 0), *value, "zero steps keep a menu value");
    }
    for pair in menu.windows(2).rev() {
        near(div_steps(pair[1], -1), pair[0], "descending sequence");
    }
    assert_eq!(div_steps(DIV_MIN, -1), DIV_MIN, "lower endpoint holds");
    assert_eq!(div_steps(DIV_MAX, 1), DIV_MAX, "upper endpoint holds");
    assert_eq!(div_steps(1.0, i32::MAX), DIV_MAX, "large positive steps saturate");
    assert_eq!(div_steps(1.0, i32::MIN), DIV_MIN, "large negative steps saturate");
    near(div_steps(3.0, 0), 2.0, "off-menu values snap in log space");
    assert_eq!(div_steps(f64::NAN, 0), DIV_MIN, "invalid menu input has a stable endpoint");

    let mut s = ScopeSettings::default();
    near(s.position, 0.5, "default trigger position");
    for position in [0.0, 0.25, 0.5, 1.0] {
        let actions = s.set_position(position);
        changed(actions, &s, true, "position");
        let window = s.window();
        near(window.pre + window.post, 10.0 * s.time_per_div, "ten time divisions");
        near(window.pre, position * 10.0 * s.time_per_div, "pre-trigger fraction");
    }
    let actions = s.set_channels(vec![
        Channel::default(),
        Channel {
            name: "CH2".into(),
            unit: "A".into(),
            volts_per_div: 0.02,
            offset_div: 1.5,
            on: true,
            color_index: 1,
        },
    ]);
    changed(actions, &s, true, "channel configuration");
    let actions = s.set_trigger_channel(1);
    changed(actions, &s, true, "trigger channel");
    let actions = s.set_position(0.3);
    changed(actions, &s, true, "map trigger position");
    let map = s.map((1000, 800));
    let c = &s.channels[1];
    let centre = c.offset_div * c.volts_per_div;
    near((map.v_top - centre) / map.v_px, 400.0, "offset value maps to the screen centre");
    near(8.0 * c.volts_per_div / map.v_px, 800.0, "eight vertical divisions fill the screen");
    near(
        (map.v_top - (centre + 4.0 * c.volts_per_div)) / map.v_px,
        0.0,
        "upper four divisions reach the top",
    );
    near(
        (map.v_top - (centre - 4.0 * c.volts_per_div)) / map.v_px,
        800.0,
        "lower four divisions reach the bottom",
    );
    near(-map.t_left / map.t_px, 300.0, "trigger time maps to its screen position");
    near(map.t_px * 1000.0, 10.0 * s.time_per_div, "horizontal map covers the window");
    assert_eq!(s.map_channel(1, (1000, 800)), Some(map), "default map uses the trigger channel");
    assert_ne!(s.map_channel(0, (1000, 800)), Some(map), "each channel has its own scale");
    assert_eq!(s.map_channel(2, (1000, 800)), None, "missing channel has no map");

    let actions = s.set_scale(1, 0.05);
    changed(actions, &s, true, "scale");
    let actions = s.step_scale(1, -1);
    changed(actions, &s, true, "scale step");
    near(s.channels[1].volts_per_div, 0.02, "channel scale uses 1-2-5 steps");
    let actions = s.set_offset(1, -0.5);
    changed(actions, &s, true, "offset");
    let actions = s.set_channel_on(0, false);
    changed(actions, &s, true, "channel visibility");
    let actions = s.set_time_per_div(20e-6);
    changed(actions, &s, true, "timebase");
    let actions = s.step_time(1);
    changed(actions, &s, true, "timebase step");
    near(s.time_per_div, 50e-6, "timebase uses 1-2-5 steps");
    let actions = s.set_trigger_level(0.015);
    changed(actions, &s, false, "trigger level keeps the image");
    let actions = s.set_trigger_slope(Slope::Falling);
    changed(actions, &s, false, "trigger slope keeps the image");
    let actions = s.set_holdoff(2e-6);
    changed(actions, &s, false, "trigger holdoff keeps the image");
    let trigger = s.trigger();
    near(trigger.level, 0.015, "trigger level reaches acquisition");
    near(
        trigger.hyst,
        0.02 * 8.0 * s.channels[1].volts_per_div,
        "hysteresis is two percent of the trigger channel height",
    );
    near(trigger.holdoff, 2e-6, "holdoff reaches acquisition");
    assert_eq!(trigger.slope, Slope::Falling, "slope reaches acquisition");

    for (p, tau) in Persistence::ALL.into_iter().zip([0.0, 0.05, 0.2, 1.0, 5.0, f64::INFINITY]) {
        assert_eq!(p.tau(), tau, "persistence decay seconds");
        assert_eq!(Persistence::from_tau(tau), Some(p), "decay round trip");
        let actions = s.set_persistence(p);
        changed(actions, &s, false, "persistence keeps the image");
        assert_eq!(s.look(false).tau, tau, "decay reaches the screen");
    }
    assert_eq!(Persistence::from_tau(0.3), None, "unsupported decay is not silently rounded");
    assert_eq!(Persistence::from_tau(f64::NAN), None, "invalid decay has no menu entry");
    let actions = s.set_intensity(10_000.0);
    changed(actions, &s, false, "gain keeps the image");
    assert_eq!(s.look(false).gain, 10_000.0, "gain reaches the screen");
    near(s.intensity_fraction(), 0.75, "logarithmic gain fraction");
    let actions = s.set_intensity_fraction(0.5);
    changed(actions, &s, false, "gain slider");
    near(s.intensity, 1000.0, "middle of the gain slider is geometric");
    let actions = s.set_intensity_fraction(-1.0);
    changed(actions, &s, false, "gain slider lower endpoint");
    assert_eq!(s.intensity, INTENSITY_MIN, "minimum gain");
    let actions = s.set_intensity(f64::MAX);
    changed(actions, &s, false, "gain upper endpoint");
    assert_eq!(s.intensity, INTENSITY_MAX, "maximum gain");
    assert!(!s.look(false).light && s.look(false).bloom == 0.6, "dark look blooms");
    assert!(s.look(true).light && s.look(true).bloom == 0.0, "light look has no bloom");
    let actions = s.set_running(false);
    changed(actions, &s, false, "stop keeps the image");
    assert!(!s.running && !s.single, "stop disarms acquisition");
    let actions = s.set_single(true);
    changed(actions, &s, true, "single starts a fresh image");
    assert!(s.running && s.single, "single arms acquisition");
    let actions = s.set_single(true);
    changed(actions, &s, true, "single can rearm while already armed");
    let actions = s.set_single(false);
    changed(actions, &s, false, "single disarm");
    let actions = s.set_position(2.0);
    changed(actions, &s, true, "position upper endpoint");
    assert_eq!(s.position, 1.0, "pre-trigger fraction is bounded");

    assert!(s.set_position(1.0).is_empty(), "unchanged settings do not reconfigure");
    assert!(s.set_scale(99, 1.0).is_empty(), "missing scale channel is harmless");
    assert!(s.step_scale(99, 1).is_empty(), "missing scale step is harmless");
    assert!(s.set_offset(99, 0.0).is_empty(), "missing offset channel is harmless");
    assert!(s.set_channel_on(99, true).is_empty(), "missing visibility channel is harmless");
    assert!(s.set_trigger_channel(99).is_empty(), "invalid trigger channel is rejected");
    assert!(s.set_scale(0, 0.0).is_empty(), "zero scale is rejected");
    assert!(s.set_time_per_div(-1.0).is_empty(), "negative timebase is rejected");
    assert!(s.set_offset(0, f64::NAN).is_empty(), "invalid offset is rejected");
    assert!(s.set_position(f64::NAN).is_empty(), "invalid position is rejected");
    assert!(s.set_trigger_level(f64::INFINITY).is_empty(), "invalid trigger level is rejected");
    assert!(s.set_holdoff(f64::NAN).is_empty(), "invalid holdoff is rejected");
    assert!(s.set_intensity(f64::NAN).is_empty(), "invalid gain is rejected");
    assert!(s.set_intensity_fraction(f64::NAN).is_empty(), "invalid slider fraction is rejected");
    assert!(
        s.set_channels(vec![Channel {
            volts_per_div: 0.0,
            ..Channel::default()
        }])
        .is_empty(),
        "invalid channel list is rejected"
    );
    let actions = s.set_channels(Vec::new());
    changed(actions, &s, true, "removing channels");
    let empty_map = s.map((0, 0));
    assert!(
        empty_map.t_px.is_finite() && empty_map.v_px.is_finite(),
        "empty scope and undrawn screen keep finite maps"
    );
}

#[test]
fn spectrum_controls_publish_mapping_and_popover_choices() {
    use fab_elec_sim::fft::Window::{Hann, BlackmanHarris, FlatTop, Rect};
    let mut settings = ScopeSettings { domain: Domain::Fft, ..ScopeSettings::default() };
    let channels = settings.channels.clone();
    let actions = settings.set_fft_view(-12.0, 0.35);
    changed(actions, &settings, true, "continuous dB mapping clears before acquisition redraws");
    assert_eq!((settings.fft.ref_dbv, settings.fft.db_per_div), (-12.0, 0.35), "pointer mapping is not snapped to a menu");
    let actions = settings.step_db(1);
    changed(actions, &settings, true, "keyboard dB scale step");
    near(settings.fft.db_per_div, 1.0, "keyboard scale steps from the nearest continuous division");
    let actions = settings.step_ref(-1);
    changed(actions, &settings, true, "keyboard reference step");
    near(settings.fft.ref_dbv, -13.0, "keyboard reference moves by one dB division");
    assert_eq!(settings.channels, channels, "FFT value edits leave the time-domain volts mapping intact");
    for (scale, limit) in [(1e-6, 0.1), (1e6, 50.0)] {
        let actions = settings.set_fft_view(-12.0, scale);
        changed(actions, &settings, true, "continuous dB limits");
        assert_eq!(settings.fft.db_per_div, limit, "settings enforce both dB division limits");
    }
    assert!(settings.step_db(1).is_empty(), "keyboard scale holds at the upper dB limit");
    assert!(settings.set_fft_view(f64::NAN, 5.0).is_empty(), "nonfinite reference is rejected");
    assert!(settings.set_fft_view(0.0, 0.0).is_empty(), "zero dB divisions are rejected");
    for window in [BlackmanHarris, FlatTop, Rect, Hann] {
        let actions = settings.set_window(window);
        changed(actions, &settings, true, "window choice restarts acquisition");
        let choices = settings.fft.window_choices();
        assert_eq!(choices.map(|(label, _)| label), ["Hann", "Blackman-Harris", "Flat top", "Rect"], "popover window labels keep their choice order");
        assert_eq!(choices.map(|(_, current)| current), [Hann, BlackmanHarris, FlatTop, Rect].map(|choice| choice == window), "only the selected window is marked current");
        assert!(settings.set_window(window).is_empty(), "pressing the current window does not restart acquisition");
    }
    let actions = settings.cycle_window(-1);
    changed(actions, &settings, true, "window stepper shares the direct choice path");
    assert_eq!(settings.fft.window, Rect, "window stepper wraps backwards from Hann");
    let actions = settings.toggle_log();
    changed(actions, &settings, true, "frequency mapping restarts acquisition");
    assert!(!settings.fft.log_freq, "log choice changes to linear");
    let spectrum = settings.spectrum();
    let actions = settings.toggle_waterfall();
    changed(actions, &settings, false, "waterfall remains a display-only choice");
    assert!(!settings.fft.waterfall, "waterfall choice changes to off");
    assert_eq!(settings.spectrum(), spectrum, "waterfall does not alter worker axes");
    for (share, expected) in [(0.6, 0.6), (-1.0, 0.15), (2.0, 0.75), (1.0 / 3.0, 1.0 / 3.0)] {
        let actions = settings.set_waterfall_share(share);
        changed(actions, &settings, false, "divider drag and reset preserve the picture");
        assert_eq!(settings.fft.waterfall_share, expected, "divider share clamps and resets to one third");
        assert_eq!(settings.spectrum(), spectrum, "divider drag and reset do not alter worker axes");
        assert!(settings.set_waterfall_share(expected).is_empty(), "an unchanged divider share emits no action");
    }
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(settings.set_waterfall_share(invalid).is_empty(), "nonfinite divider shares emit no action");
        assert_eq!(settings.fft.waterfall_share, 1.0 / 3.0, "nonfinite divider shares leave the split intact");
    }
}

#[test]
fn dragging_the_trace_moves_its_picture() {
    let mut settings = ScopeSettings::default();
    settings.set_scale(0, 0.5);
    settings.set_offset(0, 1.0);
    let start = settings.channels[0].offset_div;
    let map = settings.map_channel(0, (100, 400)).expect("the dragged channel has a map");
    let row = (map.v_top - 0.7) / map.v_px;
    assert_eq!(settings.offset_dragged(0, start, 0.0, 400.0), start, "no movement preserves the press offset");
    for dy in [50.0, -120.0] {
        let offset = settings.offset_dragged(0, start, dy, 400.0);
        settings.set_offset(0, offset);
        let map = settings.map_channel(0, (100, 400)).expect("the dragged channel keeps its map");
        let moved = (map.v_top - 0.7) / map.v_px;
        assert!((moved - row - dy).abs() <= 1e-9, "the fixed voltage follows the pointer by {dy} points from the press");
    }
}

#[test]
fn automatic_trigger_timeout_follows_the_screen_and_single_mode() {
    let mut settings = ScopeSettings::default();
    assert!(settings.trigger.auto, "scope cards default to automatic triggering");
    assert_eq!(settings.auto_after(), Some(10e-3), "short screens wait at least 10 ms of simulated time");
    settings.set_time_per_div(1e-3);
    near(
        settings.auto_after().expect("automatic mode has a timeout"),
        40e-3,
        "long screens wait four screen spans",
    );
    let actions = settings.set_trigger_auto(false);
    changed(actions, &settings, false, "normal mode keeps the image, as the slope chip does");
    assert_eq!(settings.auto_after(), None, "normal mode disables automatic sweeps");
    assert!(settings.set_trigger_auto(false).is_empty(), "unchanged trigger mode emits no action");
    let actions = settings.set_trigger_auto(true);
    changed(actions, &settings, false, "automatic mode emits the new settings and keeps the image");
    settings.set_single(true);
    assert_eq!(settings.auto_after(), None, "single acquisition always waits for a real trigger");
    settings.set_single(false);
    near(
        settings.auto_after().expect("disarming single restores auto"),
        40e-3,
        "single leaves the selected auto mode intact",
    );
}

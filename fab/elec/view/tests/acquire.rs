use fab_elec_bench::trigger::{events, Edge, EdgeTrigger, Slope, Window};
use fab_elec_sim::Sink;
use fab_elec_view::instruments::acquire::{AcquireIntake, LaneFrame, LaneSetup, SimIntake, Acquisition, Probe, Published, TriggerSlot};
use std::sync::Arc;
use fab_elec_view::instruments::phosphor::{Hits, PhosphorMap, Raster};
use fab_elec_instr::synth::Synth;
use std::time::Duration;

#[test]
fn qualified_pending_windows_keep_pre_history_and_survive_restarts() {
    use fab_elec_bench::trigger::Qualifier;
    let trigger = EdgeTrigger { level: 0.0, hyst: 1.0, slope: Slope::Falling, holdoff: 0.0 };
    let window = Window { pre: 1.0, post: 1.0 };
    let map = PhosphorMap::screen(12, 8, window.pre, window.pre + window.post, -1.5, 1.5);
    let mut acquisition = Acquisition::new(trigger, 0, window, vec![LaneSetup::time(map)])
        .with_qualifier(Qualifier::Width { min: 0.0, max: 3.0 });
    let mut actual = Hits::new(map.w, map.h);
    for (t, y) in [(0.0, -1.0), (1.0, 1.0), (2.0, 0.25), (3.0, -0.25), (4.0, -0.4), (8.0, -0.45)] {
        acquisition.push(t, &[y], std::slice::from_mut(&mut actual));
    }
    assert_eq!(acquisition.open_strokes(), 1, "qualified crossing keeps its tentative raster through late confirmation");
    assert_eq!(actual.waveforms, 0);
    acquisition.set_qualifier(Qualifier::Width { min: 0.0, max: 4.0 });
    assert_eq!(acquisition.push(9.0, &[-1.0], std::slice::from_mut(&mut actual)), Edge::Accepted(2.5));
    let mut expected = Hits::new(map.w, map.h);
    Raster::default().add(&map, &[-1.5, -0.5, 0.5, 1.5], &[1.0, 0.25, -0.25, -0.4], &mut expected);
    assert_eq!(actual, expected, "a qualifier edit preserves still-qualified candidates after their pre-history leaves the ring");
    acquisition.restart();
    acquisition.push(10.0, &[1.0], std::slice::from_mut(&mut actual));
    assert_eq!(acquisition.push(11.0, &[-1.0], std::slice::from_mut(&mut actual)), Edge::None, "restart retains the qualifier and forgets its references");
    assert_eq!(acquisition.events(), 0);
    assert_eq!(acquisition.open_strokes(), 0);
}

#[test]
fn the_picture_re_arms_after_its_window_and_every_trigger_still_counts() {
    // Windows four periods long on a PWM, so every window spans several triggers. As on a real
    // scope, the picture draws a window only once the one drawing has passed; the triggers all
    // count, and the windows drawn are exactly those, bit for bit.
    let synth = Synth { noise: 0.0, ..Synth::pwm() };
    let (t, y) = synth.record(60_000);
    let period = synth.period();
    let trigger = EdgeTrigger { level: (synth.lo + synth.hi) * 0.5, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
    let window = Window { pre: period / 2.0, post: 3.5 * period };
    let map = PhosphorMap::screen(300, 200, window.pre, window.pre + window.post, synth.lo - 0.5, synth.hi + 0.5);
    let mut acquisition = Acquisition::new(trigger, 0, window, vec![LaneSetup::time(map)]);
    let mut streamed = Hits::new(map.w, map.h);
    let mut marks = Vec::new();
    let mut most_open = 0;
    for (&time, &value) in t.iter().zip(&y) {
        if let Edge::Accepted(time) = acquisition.push(time, &[value], std::slice::from_mut(&mut streamed)) {
            marks.push(time);
        }
        most_open = most_open.max(acquisition.open_strokes());
    }
    assert_eq!(marks, events(&t, &y, &trigger), "every edge is still an accepted trigger");
    // the windows drawn: each the first trigger at or past the end of the one before
    let mut drawn: Vec<f64> = Vec::new();
    for &time in &marks {
        if drawn.last().is_none_or(|&last| time >= last + window.post) {
            drawn.push(time);
        }
    }
    assert!(drawn.len() > 3 && drawn.len() * 3 < marks.len(), "the windows span several triggers: {} drawn of {}", drawn.len(), marks.len());
    assert_eq!(most_open, 1, "one window draws at a time");
    let last = t[t.len() - 1];
    let complete = drawn.partition_point(|&time| time + window.post <= last);
    let mut batch = Hits::new(map.w, map.h);
    let mut raster = Raster::new();
    let mut relative = Vec::new();
    for &time in &drawn[..complete] {
        let start = t.partition_point(|&t| t < time - window.pre).saturating_sub(1);
        let end = t.partition_point(|&t| t < time + window.post);
        relative.clear();
        relative.extend(t[start..=end].iter().map(|t| t - time));
        raster.add(&map, &relative, &y[start..=end], &mut batch);
    }
    assert_eq!(streamed.waveforms, complete as u64, "the picture drew one window per re-arm");
    assert!(streamed.data.iter().zip(&batch.data).all(|(a, b)| a.to_bits() == b.to_bits()), "the windows drawn are the re-armed ones, bit for bit");
}

#[test]
fn a_long_window_sweeps_across_the_picture_as_it_draws() {
    // Windows four periods long, published many times a window, as a long timebase is. Each
    // publication carries the columns the drawing window has settled since the one before, left
    // to right, so the trace sweeps as a scope's does instead of appearing a window at a time;
    // summed, the publications are exactly the whole windows, each column once per window.
    use fab_elec_bench::source::Intake;
    let synth = Synth { noise: 0.0, ..Synth::pwm() };
    let (t, y) = synth.record(60_000);
    let period = synth.period();
    let trigger = EdgeTrigger { level: (synth.lo + synth.hi) * 0.5, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
    let window = Window { pre: period / 2.0, post: 3.5 * period };
    let map = PhosphorMap::screen(300, 200, window.pre, window.pre + window.post, synth.lo - 0.5, synth.hi + 0.5);
    let marks = events(&t, &y, &trigger);
    let mut drawn: Vec<f64> = Vec::new();
    for &time in &marks {
        if drawn.last().is_none_or(|&last| time >= last + window.post) {
            drawn.push(time);
        }
    }
    // Stop a quarter period after the fifth window completes: before the next one opens.
    let end = drawn[4] + window.post + period / 4.0;
    assert!(drawn[5] > end, "no window is open at the end");
    let n = t.partition_point(|&t| t <= end);
    let mut total = Hits::new(map.w, map.h);
    // Per column, the publication that brought the first window there.
    let mut arrival = vec![usize::MAX; map.w as usize];
    let mut publications = 0;
    {
        let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, vec![LaneSetup::time(map)]), Duration::MAX, |frame: LaneFrame, _| {
            let hits = &frame.lanes[0];
            for (c, &part) in hits.partial.iter().enumerate() {
                if part > 0.0 && arrival[c] == usize::MAX { arrival[c] = publications; }
            }
            total.add(hits);
            publications += 1;
            Published::Taken(frame)
        });
        for (k, (&time, &value)) in t[..n].iter().zip(&y[..n]).enumerate() {
            intake.point(time, &[value]);
            if k % 40 == 39 { intake.flush(); }
        }
        intake.flush();
    }
    assert_eq!((total.waveforms, total.partial_done, total.whole()), (5, 5, 0), "the five windows all swept: none arrived whole");
    assert!(total.partial.iter().all(|&n| n == 5.0), "each window brought each column once: {:?}", &total.partial[..8]);
    assert!(arrival.windows(2).all(|a| a[0] <= a[1]), "the first window sweeps left to right");
    let steps = arrival.windows(2).filter(|a| a[0] < a[1]).count();
    assert!(steps >= 8, "the first window arrives in many publications, not at once: {steps} steps over {publications} publications");
    let mut batch = Hits::new(map.w, map.h);
    let mut raster = Raster::new();
    let mut relative = Vec::new();
    for &time in &drawn[..5] {
        let start = t.partition_point(|&t| t < time - window.pre).saturating_sub(1);
        let stop = t.partition_point(|&t| t < time + window.post);
        relative.clear();
        relative.extend(t[start..=stop].iter().map(|t| t - time));
        raster.add(&map, &relative, &y[start..=stop], &mut batch);
    }
    assert!(total.data.iter().zip(&batch.data).all(|(a, b)| a.to_bits() == b.to_bits()), "the parts sum to the whole windows, bit for bit");
}

#[test]
fn streamed_windows_match_batch_rasterisation() {
    let synth = Synth { noise: 0.0, ..Synth::pwm() };
    let (t, y) = synth.record(300_000);
    let period = synth.period();
    let trigger = EdgeTrigger { level: (synth.lo + synth.hi) * 0.5, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
    let window = Window { pre: period / 2.0, post: period / 2.0 };
    let map = PhosphorMap::screen(300, 200, window.pre, period, synth.lo - 0.5, synth.hi + 0.5);
    let mut acquisition = Acquisition::new(trigger, 0, window, vec![LaneSetup::time(map)]);
    let mut streamed = Hits::new(map.w, map.h);
    let mut marks = Vec::new();
    let mut first_recent = 0;
    let mut ring_bounded = true;
    let mut strokes_bounded = true;
    for (index, (&time, &value)) in t.iter().zip(&y).enumerate() {
        if let Edge::Accepted(time) = acquisition.push(time, &[value], std::slice::from_mut(&mut streamed)) {
            marks.push(time);
        }
        while t[first_recent] < time - window.pre {
            first_recent += 1;
        }
        ring_bounded &= acquisition.ring_len() <= index + 1 - first_recent + 2;
        strokes_bounded &= acquisition.open_strokes() <= 2;
    }
    assert!(ring_bounded, "the pre-trigger ring retained more than its window and guards");
    assert!(strokes_bounded, "one-period windows retained more than two open strokes");

    let expected_marks = events(&t, &y, &trigger);
    assert_eq!(marks, expected_marks, "acquisition changed the detector's accepted event times");
    let last = t[t.len() - 1];
    let complete = expected_marks.partition_point(|&time| time + window.post <= last);
    let mut batch = Hits::new(map.w, map.h);
    let mut raster = Raster::new();
    let mut relative = Vec::new();
    for &time in &expected_marks[..complete] {
        let start = t.partition_point(|&t| t < time - window.pre).saturating_sub(1);
        let end = t.partition_point(|&t| t < time + window.post);
        relative.clear();
        relative.extend(t[start..=end].iter().map(|t| t - time));
        raster.add(&map, &relative, &y[start..=end], &mut batch);
    }
    assert_eq!(batch.waveforms, complete as u64, "the reference did not rasterise every complete window");
    assert_eq!(streamed.waveforms, batch.waveforms, "streaming lost or prematurely flushed a window");
    assert!(streamed.data.iter().zip(&batch.data).all(|(a, b)| a.to_bits() == b.to_bits()), "streaming changed the batch pixel bits");

    // The sink publishes once, at its flush, which also sweeps the open windows' settled columns into
    // the frame (`Acquisition::sweep`): sweep the direct acquisition alike before comparing.
    acquisition.sweep(std::slice::from_mut(&mut streamed));
    let mut published = Hits::new(map.w, map.h);
    let mut publications = 0;
    let mut sink = probe_sink(Probe { pos: 1, neg: 0, scale: 1.0 }, Acquisition::new(trigger, 0, window, vec![LaneSetup::time(map)]), map.w, map.h, Duration::MAX, |frame, _| {
        publications += 1;
        Published::Taken(LaneFrame { generation: 0, published: None, spectra: vec![None], lanes: vec![std::mem::replace(&mut published, frame)] })
    });
    for (&time, &value) in t.iter().zip(&y) {
        Sink::point(&mut sink, time, None, &[0.0, f64::from(value)]);
    }
    assert_eq!(sink.marks().as_slice(), expected_marks.as_slice(), "the simulation sink lost accepted edge marks");
    sink.marks().clear();
    sink.flush();
    assert!(sink.marks().is_empty(), "flushing restored edge marks already drained by the owner");
    drop(sink);
    assert_eq!(publications, 1, "the sink published before the requested interval or failed to flush");
    assert_eq!(published.waveforms, streamed.waveforms, "the sink changed the completed-window count");
    assert!(streamed.partial.iter().any(|&n| n > 0.0), "an open window had settled columns to sweep");
    assert_eq!(published.partial, streamed.partial, "the sink swept the same open columns");
    assert!(published.data.iter().zip(&streamed.data).all(|(a, b)| a.to_bits() == b.to_bits()), "the sink changed the acquisition pixel bits");

    // The candidate must survive after its pre history leaves the ring.
    let edge = EdgeTrigger { level: 0.0, hyst: 1.0, slope: Slope::Rising, holdoff: 0.0 };
    let short = Window { pre: 1.0, post: 1.0 };
    let small_map = PhosphorMap::screen(12, 8, short.pre, short.pre + short.post, -1.5, 1.5);
    let mut delayed = Acquisition::new(edge, 0, short, vec![LaneSetup::time(small_map)]);
    let mut actual = Hits::new(small_map.w, small_map.h);
    for (time, value) in [(0.0, -1.0), (1.0, -0.25), (2.0, 0.25), (3.0, 0.4), (8.0, 0.45)] {
        delayed.push(time, &[value], std::slice::from_mut(&mut actual));
    }
    assert_eq!(actual.waveforms, 0, "an unconfirmed candidate reached the hit frame");
    assert_eq!(delayed.open_strokes(), 1, "a completed candidate was discarded before confirmation");
    assert_eq!(delayed.push(9.0, &[1.0], std::slice::from_mut(&mut actual)), Edge::Accepted(1.5), "late confirmation changed its event time");
    let mut expected = Hits::new(small_map.w, small_map.h);
    raster.add(&small_map, &[-1.5, -0.5, 0.5, 1.5], &[-1.0, -0.25, 0.25, 0.4], &mut expected);
    assert_eq!(actual, expected, "late confirmation lost pre history or rasterised beyond its post guard");
    assert_eq!(delayed.open_strokes(), 0, "a confirmed complete candidate was not recycled");

    // A dropped candidate must not leak its spans when its scratch is reused.
    let mut recrossing = Acquisition::new(edge, 0, short, vec![LaneSetup::time(small_map)]);
    actual.reset(small_map.w, small_map.h);
    for (time, value) in [(0.0, -1.0), (1.0, 0.25), (2.0, -0.25), (3.0, 0.25), (4.0, 1.0)] {
        recrossing.push(time, &[value], std::slice::from_mut(&mut actual));
    }
    expected.reset(small_map.w, small_map.h);
    raster.add(&small_map, &[-1.5, -0.5, 0.5, 1.5], &[0.25, -0.25, 0.25, 1.0], &mut expected);
    assert_eq!(actual, expected, "a dropped crossing leaked coverage into its replacement");

    // Settings affect new windows while an accepted stroke keeps its map and end.
    let long = Window { pre: 0.25, post: 10.0 };
    let old_map = PhosphorMap::screen(12, 8, long.pre, long.pre + long.post, -1.5, 1.5);
    let new_map = PhosphorMap::screen(12, 8, 0.25, 1.0, -2.0, 2.0);
    let mut changed = Acquisition::new(edge, 0, long, vec![LaneSetup::time(old_map)]);
    actual.reset(old_map.w, old_map.h);
    for (time, value) in [(0.0, -1.0), (1.0, 1.0), (2.0, -1.0)] {
        changed.push(time, &[value], std::slice::from_mut(&mut actual));
    }
    changed.set_lanes(0, Window { pre: 0.25, post: 0.0 }, vec![LaneSetup { map: new_map, spectrum: None, drawn: true }]);
    changed.push(3.0, &[1.0], std::slice::from_mut(&mut actual));
    assert_eq!(actual.waveforms, 1, "a new completed window waited for an older unfinished stroke");
    changed.push(11.0, &[1.0], std::slice::from_mut(&mut actual));
    expected.reset(old_map.w, old_map.h);
    raster.add(&new_map, &[-0.5, 0.5], &[-1.0, 1.0], &mut expected);
    raster.add(&old_map, &[-0.5, 0.5, 1.5, 2.5, 10.5], &[-1.0, 1.0, -1.0, 1.0, 1.0], &mut expected);
    assert_eq!(actual, expected, "changing settings altered an accepted stroke's map or window");

    // A spent frame returned by the owner must be cleared before the next interval.
    let mut deliveries = 0;
    let mut recycling = probe_sink(
        Probe { pos: 1, neg: 2, scale: 0.5 },
        Acquisition::new(edge, 0, short, vec![LaneSetup::time(small_map)]),
        small_map.w,
        small_map.h,
        Duration::ZERO,
        |frame, _| {
            if deliveries == 0 {
                assert_eq!(frame.waveforms, 1, "periodic publishing lost the completed window");
                assert!(frame.data.iter().any(|&v| v > 0.0), "periodic publishing lost the window's pixels");
            } else {
                assert_eq!(frame.waveforms, 0, "a recycled frame retained its waveform count");
                assert!(frame.data.iter().all(|&v| v == 0.0), "a recycled frame retained old pixels");
            }
            deliveries += 1;
            Published::Taken(LaneFrame { generation: 0, published: None, spectra: vec![None], lanes: vec![frame] })
        },
    );
    for index in 0..128 {
        let value = if index == 0 { -1.0 } else { 1.0 };
        Sink::point(&mut recycling, index as f64, None, &[0.0, 2.0 * value + 3.0, 3.0]);
    }
    assert_eq!(recycling.marks().as_slice(), &[0.5], "a differential probe or frame publication changed edge marks");
    drop(recycling);
    assert_eq!(deliveries, 2, "the sink did not check the publication interval every 64 samples");
}

fn auto_sweep(after: Option<f64>, periodic: bool) -> (u64, Vec<f64>) {
    let trigger = EdgeTrigger { level: 1.65, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
    let window = Window { pre: 100e-6, post: 200e-6 };
    let map = PhosphorMap::screen(16, 12, window.pre, window.pre + window.post, -0.2, 3.5);
    let mut acquisition = Acquisition::new(trigger, 0, window, vec![LaneSetup::time(map)]);
    acquisition.set_auto(after);
    // Ordinary settings updates must preserve the explicitly selected mode.
    acquisition.set_lanes(0, window, vec![LaneSetup { map: map, spectrum: None, drawn: true }]);
    let mut waveforms = 0;
    let mut sink = probe_sink(Probe { pos: 1, neg: 0, scale: 1.0 }, acquisition, map.w, map.h, Duration::MAX, |hits, _| {
        waveforms += hits.waveforms;
        Published::Taken(LaneFrame { generation: 0, published: None, spectra: vec![None], lanes: vec![hits] })
    });
    for index in 0..=10_500 {
        let value = if periodic && index % 500 < 250 { 0.0 } else { 3.3 };
        sink.point(index as f64 * 1e-6, None, &[0.0, value]);
    }
    let marks = sink.marks().clone();
    sink.flush();
    drop(sink);
    (waveforms, marks)
}

#[test]
fn a_shared_trigger_retunes_a_running_sink_in_place() {
    // Starts above the signal in normal mode, so nothing triggers until the level moves.
    let high = EdgeTrigger { level: 5.0, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
    let window = Window { pre: 100e-6, post: 200e-6 };
    let map = PhosphorMap::screen(16, 12, window.pre, window.pre + window.post, -0.2, 3.5);
    let slot = Arc::new(TriggerSlot::default());
    let mut waveforms = 0;
    let mut sink = probe_sink(Probe { pos: 1, neg: 0, scale: 1.0 }, Acquisition::new(high, 0, window, vec![LaneSetup::time(map)]), map.w, map.h, Duration::MAX, |hits, _| {
        waveforms += hits.waveforms;
        Published::Taken(LaneFrame { generation: 0, published: None, spectra: vec![None], lanes: vec![hits] })
    });
    sink.follow_trigger(slot.clone());
    for index in 0..=10_500 {
        if index == 2_000 {
            slot.set(EdgeTrigger { level: 1.65, ..high }, fab_elec_bench::trigger::Qualifier::None, None, None);
        }
        // Rising edges at 0.25 ms + k * 0.5 ms.
        let value = if index % 500 < 250 { 0.0 } else { 3.3 };
        sink.point(index as f64 * 1e-6, None, &[0.0, value]);
    }
    let marks = sink.marks().clone();
    sink.flush();
    drop(sink);
    assert!(marks.first().is_some_and(|&first| (first - 2.25e-3).abs() < 1e-6), "the first trigger is the first rising edge after the level moved: {marks:?}");
    assert_eq!(marks.len(), 17, "every rising edge from 2.25 ms to 10.25 ms triggers at the new level");
    assert_eq!(waveforms, 17, "each trigger at the new level completes its window");
}

#[test]
fn auto_sweeps_a_constant_without_trigger_marks() {
    let (waveforms, marks) = auto_sweep(Some(1e-3), false);
    assert_eq!(waveforms, 10, "10.5 ms completes exactly ten automatic windows at a 1 ms timeout");
    assert!(marks.is_empty(), "automatic windows never create trigger marks");
}

#[test]
fn regular_edges_prevent_auto_sweeps() {
    let (waveforms, marks) = auto_sweep(Some(1e-3), true);
    assert_eq!(marks.len(), 21, "the input supplies a rising edge every 0.5 ms");
    assert_eq!(waveforms, marks.len() as u64, "edges faster than the timeout leave no forced waveforms");
}

#[test]
fn normal_mode_waits_for_an_edge() {
    let (waveforms, marks) = auto_sweep(None, false);
    assert_eq!(waveforms, 0, "normal acquisition produces no windows on DC");
    assert!(marks.is_empty(), "DC produces no accepted trigger marks");
}

#[test]
fn restarting_auto_uses_the_new_time_origin_and_waits_for_pre_history() {
    let trigger = EdgeTrigger { level: 0.0, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
    let window = Window { pre: 2e-3, post: 0.0 };
    let map = PhosphorMap::screen(8, 8, window.pre, window.pre, 0.0, 4.0);
    let mut acquisition = Acquisition::new(trigger, 0, window, vec![LaneSetup::time(map)]);
    acquisition.set_auto(Some(1e-3));
    let mut hits = Hits::new(map.w, map.h);
    for origin in [10.0, 0.0] {
        acquisition.restart();
        let before = hits.waveforms;
        for index in 0..=1500 {
            assert_eq!(acquisition.push(origin + index as f64 * 1e-6, &[3.3], std::slice::from_mut(&mut hits)), Edge::None, "a forced sweep preserves the detector verdict");
        }
        assert_eq!(hits.waveforms, before, "auto waits until the restarted ring spans the pre window");
        acquisition.push(origin + 2.1e-3, &[3.3], std::slice::from_mut(&mut hits));
        assert_eq!(hits.waveforms, before + 1, "restart preserves auto and resets its time origin");
    }
}

#[test]
fn nonuniform_three_tone_spectrum_keeps_frequency_and_dbv_on_both_axes() {
    use fab_elec_sim::fft::Window as FftWindow;
    use fab_elec_view::instruments::{scope::FftSettings, spectrum::{bin_x, fft_size, SpectrumStroke}};
    let plot = (400, 500);
    let n = fft_size(plot.0);
    let record = 0.01;
    let tones = [(13, 1.0_f64), (61, 0.5), (173, 0.2)];
    let value = |t: f64| tones.iter().map(|&(bin, amp)| amp * (std::f64::consts::TAU * bin as f64 * t / record).cos()).sum::<f64>() as f32;
    let mut spectrum = SpectrumStroke::default();
    let mut hits = Hits::new(plot.0, plot.1);
    for window in [FftWindow::Hann, FftWindow::BlackmanHarris, FftWindow::FlatTop, FftWindow::Rect] {
        for log_freq in [false, true] {
            let settings = FftSettings { log_freq, window, ..FftSettings::default() };
            hits.reset(plot.0, plot.1);
            spectrum.begin(0.0, record, plot, settings);
            let mut previous = record / (n * 32) as f64;
            for i in 1..=n * 8 + 1 {
                let t = record * (i as f64 + 0.2 * (i as f64 * 1.7).sin()) / (n * 8) as f64;
                spectrum.segment(previous, value(previous), t, value(t));
                previous = t;
            }
            spectrum.finish(&mut hits);
            assert_eq!(hits.waveforms, 1, "a spectrum must count as one waveform");
            for &(bin, amp) in &tones {
                let dbv = 20.0 * amp.log10();
                assert!((spectrum.bins()[bin] - dbv).abs() < 0.5, "{window:?} log={log_freq} bin {bin}: {} dBV, expected {dbv}", spectrum.bins()[bin]);
                let expected = bin_x(bin as f64, n, plot.0, log_freq).floor() as u32;
                let left = bin_x((bin - 5) as f64, n, plot.0, log_freq).floor() as u32;
                let right = (bin_x((bin + 5) as f64, n, plot.0, log_freq).ceil() as u32).min(plot.0);
                let peak_row = (left..right).filter_map(|c| (0..plot.1).find(|&r| hits.at(c, r) > 0.0)).min().expect("tone is visible");
                assert!((expected.saturating_sub(1)..=(expected + 1).min(plot.0 - 1)).any(|c| hits.at(c, peak_row) > 0.0), "{window:?} log={log_freq} bin {bin}: raster peak missed column {expected}");
                let raster_dbv = settings.ref_dbv - (peak_row as f64 + 0.5) / plot.1 as f64 * 10.0 * settings.db_per_div;
                assert!((raster_dbv - dbv).abs() < 0.5, "{window:?} log={log_freq} bin {bin}: raster reads {raster_dbv} dBV, expected {dbv}");
            }
            let m = spectrum.measurements().unwrap();
            assert_eq!((m.peak_f, m.second.map(|(f, _)| f), m.span), (1300.0, Some(6100.0), n as f64 * 50.0), "{window:?}: the second peak is the next resolved peak, outside the main peak's lobe");
            let enbw = match window { FftWindow::Rect => 1.0, FftWindow::Hann => 1.5, FftWindow::BlackmanHarris => 2.0044, FftWindow::FlatTop => 3.7702, FftWindow::Hamming => 1.3628 };
            assert!((m.rbw / 100.0 - enbw).abs() < 1e-3, "{window:?}: RBW is the bin spacing times the window's noise bandwidth, got {} Hz", m.rbw);
        }
    }
}

#[test]
fn lens_strokes_time_and_spectrum_from_the_same_capture() {
    use fab_elec_bench::source::{Block, ChanData, Intake};
    use fab_elec_view::instruments::{scope::FftSettings, spectrum::{bin_x, fft_size}};
    let map = PhosphorMap::screen(96, 40, 0.5, 1.0, -1.5, 1.5);
    let pixels = (128, 80);
    let fft = FftSettings { log_freq: false, ..FftSettings::default() };
    let lanes = vec![LaneSetup::time(map), LaneSetup { map, spectrum: Some((fft, pixels)), drawn: true }];
    let trigger = EdgeTrigger { level: 0.0, hyst: 0.0, slope: Slope::Rising, holdoff: 0.0 };
    let acquisition = Acquisition::new(trigger, 0, Window { pre: 0.5, post: 0.5 }, lanes);
    let mut frames = Vec::new();
    let mut intake = AcquireIntake::new(acquisition, Duration::MAX, |frame, stats| {
        frames.push((frame.clone(), stats));
        Published::Taken(frame)
    });
    let bin = 16;
    let n = fft_size(pixels.0);
    let samples = n * 4;
    let y: Vec<_> = (0..=samples).map(|i| (std::f64::consts::TAU * bin as f64 * i as f64 / samples as f64).cos() as f32).collect();
    let chans = [ChanData::F32(&y), ChanData::F32(&y)];
    intake.block(&Block { index: 0, t0: 0.0, dt: 1.0 / samples as f64, chans: &chans, clipped: &[], trigger: Some(0.5) });
    drop(intake);
    let (frame, stats) = &frames[0];
    assert_eq!(frame.lanes[0].waveforms, 1, "one hardware capture completes one time picture");
    assert_eq!(frame.lanes[1], frame.lanes[0], "opening a lens leaves the time hits bit-for-bit equal for the same samples");
    assert!(frame.spectra[0].is_none(), "a closed lens has no spectrum buffer");
    let spectrum = frame.spectra[1].as_ref().expect("an open lens has its second picture");
    assert_eq!((spectrum.w, spectrum.h, spectrum.waveforms), (pixels.0, pixels.1, 1), "the cell uses its own pixel size and the same capture count");
    let measured = stats.spectrum[1].measurements.expect("the same capture publishes its spectral peak");
    assert_eq!(measured.n, n, "FFT resolution follows the cell width");
    assert!((measured.peak_f / (measured.rate / n as f64) - bin as f64).abs() < 1e-9, "the spectrum peak is the known tone's bin");
    let column = bin_x(bin as f64, n, pixels.0, false).floor() as u32;
    let peak_row = (0..pixels.1).find(|&r| (0..pixels.0).any(|c| spectrum.at(c, r) > 0.0)).expect("the spectrum contains visible hits");
    assert!((column - 1..=column + 1).any(|c| spectrum.at(c, peak_row) > 0.0), "the brightest-height peak reaches the tone's raster column");
}

#[test]
fn spectrum_cell_tiles_the_plot_and_clamps_its_share() {
    use fab_elec_view::instruments::scope::cell_layout;
    use makepad_widgets_core::{dvec2, Rect};
    for width in [10.0, 320.0, 901.5] {
        let plot = Rect { pos: dvec2(13.0, 21.0), size: dvec2(width, 120.0) };
        for (share, clamped) in [(-1.0, 0.2), (0.2, 0.2), (0.38, 0.38), (0.6, 0.6), (2.0, 0.6)] {
            let (time, divider, cell) = cell_layout(plot, true, false, share);
            let divider = divider.expect("an open cell has a divider");
            let cell = cell.expect("an expanded lens has a cell");
            assert_eq!(time.pos, plot.pos, "time remains at the plot's left edge");
            assert_eq!(divider.size, dvec2(1.0, plot.size.y), "the divider is one pixel across the whole row");
            assert_eq!(divider.pos.x, time.pos.x + time.size.x, "time meets the divider without overlap");
            assert_eq!(cell.pos.x, divider.pos.x + divider.size.x, "the cell meets the divider without a gap");
            assert_eq!((cell.pos.y, cell.size.y), (plot.pos.y, plot.size.y), "both columns share the row");
            assert!((cell.pos.x + cell.size.x - plot.pos.x - width).abs() < 1e-10, "the columns tile the entire plot");
            assert!((cell.size.x / width - clamped).abs() < 1e-12, "the requested share clamps to 0.2 through 0.6");
            assert_eq!(cell_layout(plot, true, true, share), (plot, None, None), "a collapsed lane never opens the lens");
            assert_eq!(cell_layout(plot, false, false, share), (plot, None, None), "a closed lens keeps the time plot unchanged");
        }
    }
}

// Keep the original single-channel assertions while exercising the simulator adapter.
fn probe_sink<F: FnMut(Hits, fab_elec_view::instruments::acquire::AcquireStats) -> Published + Send>(probe: Probe, acquisition: Acquisition, _: u32, _: u32, every: Duration, mut publish: F) -> SimIntake<AcquireIntake<impl FnMut(LaneFrame, fab_elec_view::instruments::acquire::AcquireStats) -> Published + Send>> {
    use fab_elec_sim::stamp::{UnknownInfo, UnkKind};
    use fab_elec_view::instruments::acquire::NetProbe;
    let names = (0..=probe.pos.max(probe.neg)).map(|i| UnknownInfo { name: i.to_string(), kind: UnkKind::Voltage }).collect::<Vec<_>>();
    let mut sink = SimIntake::by_name(NetProbe { pos: probe.pos.to_string(), neg: Some(probe.neg.to_string()), scale: probe.scale }, acquisition, every, move |mut frame, stats| publish(frame.lanes.remove(0), stats));
    sink.begin(&names);
    sink
}

#[test]
fn hidden_lanes_skip_both_rasters_and_visibility_changes_at_blocks_preserve_other_lanes() {
    use fab_elec_bench::source::{Block, ChanData, ChanId, ChannelInfo, ClockKind, Intake, Layout, Quantity};
    use fab_elec_view::instruments::{acquire::LaneSlot, scope::FftSettings};
    let map = PhosphorMap::screen(16, 8, 0.0, 1.0, -1.0, 1.0);
    let drawn = LaneSetup { map, spectrum: Some((FftSettings::default(), (16, 8))), drawn: true };
    let mut lanes = vec![LaneSetup { drawn: false, ..drawn }, drawn];
    let slot = Arc::new(LaneSlot::new(lanes.clone()));
    let trigger = EdgeTrigger { level: 0.0, hyst: 0.0, slope: Slope::Rising, holdoff: 0.0 };
    let window = Window { pre: 0.0, post: 1.0 };
    let layout = Layout { generation: 23, clock: ClockKind::Device,
        channels: (0..2).map(|i| ChannelInfo { id: ChanId(i), name: format!("lane {i}"), quantity: Quantity::Volt }).collect() };
    let mut frames = Vec::new();
    let mut reference = Vec::new();
    {
        // Kept frames accumulate across edits, so clearing the whole frame would lose lane 1.
        let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes.clone()), Duration::MAX, |frame, stats| {
            frames.push((frame.clone(), stats));
            Published::Kept(frame)
        });
        let mut baseline = AcquireIntake::new(Acquisition::new(trigger, 0, window, vec![drawn; 2]), Duration::MAX, |frame, stats| {
            reference.push((frame.clone(), stats));
            Published::Kept(frame)
        });
        intake.follow_lanes(slot.clone());
        intake.begin(&layout); baseline.begin(&layout);
        let values = [-0.75, -0.5, 0.0, 0.5, 0.75, 0.5, 0.0, -0.5];
        let chans = [ChanData::F32(&values), ChanData::F32(&values)];
        for (i, visible) in [false, true, false, true].into_iter().enumerate() {
            lanes[0].drawn = visible;
            assert!(LaneSlot::compatible(&[drawn; 2], &lanes), "visibility alone must remain compatible with the installed feed");
            assert!(slot.set(lanes.clone()), "a visibility edit must reach the existing intake through LaneSlot");
            let block = Block { index: i as u64 * 8, t0: i as f64, dt: 0.125, chans: &chans, clipped: &[], trigger: Some(i as f64) };
            intake.block(&block); baseline.block(&block);
            assert_eq!(intake.generation(), layout.generation, "a visibility edit must not begin a new source generation");
            intake.flush(); baseline.flush();
        }
    }
    assert!(frames.len() >= 4, "each short block must publish its visibility result");
    assert_eq!(frames.len(), reference.len(), "visibility must preserve the publication intervals");
    for (i, ((frame, stats), (expected, answers))) in frames.iter().zip(&reference).take(4).enumerate() {
        assert_eq!(frame.lanes[1], expected.lanes[1], "visibility edits must preserve the drawn neighbour's accumulated time hits at block {i}");
        assert_eq!(frame.spectra[1], expected.spectra[1], "visibility edits must preserve the drawn neighbour's accumulated spectrum at block {i}");
        assert_eq!(frame.lanes[1].waveforms, i as u64 + 1, "the drawn neighbour must retain every capture across block edits");
        assert_eq!(frame.waveforms(), i as u64 + 1, "a hidden first lane must not hide the frame's drawn waveforms");
        assert_eq!(stats.samples, answers.samples, "hidden lanes must not suppress sample accounting");
        assert_eq!(stats.trigger_events, answers.trigger_events, "hidden lanes must not suppress hardware triggers");
        assert_eq!(stats.spectrum, answers.spectrum, "spectral measurements must be identical with or without rasterisation");
        assert!(stats.spectrum[0].measurements.is_some(), "the hidden lane must still answer its spectrum measurements");
        for hits in [&frame.lanes[0], frame.spectra[0].as_ref().expect("the hidden lens retains reusable storage")] {
            if i % 2 == 0 {
                assert_eq!(hits.waveforms, 0, "hiding must leave zero rastered waveforms in both pictures");
                assert!(hits.dirty.is_empty(), "a hidden picture must have no dirty rectangle");
                assert!(hits.data.iter().all(|&v| v == 0.0), "a hidden picture must contain no pixel hits");
            } else {
                assert_eq!(hits.waveforms, 1, "revealing must rasterise the very next short block without a 64-point delay");
                assert!(!hits.dirty.is_empty(), "revealing must rebuild the picture from a fresh capture");
            }
        }
    }
}

#[test]
fn hidden_spectrum_completed_at_block_end_publishes_after_the_last_sample_frame() {
    use fab_elec_bench::source::{Block, ChanData, Intake};
    use fab_elec_view::instruments::scope::FftSettings;
    let map = PhosphorMap::screen(16, 8, 0.0, 1.0, -1.0, 1.0);
    let lane = LaneSetup { map, spectrum: Some((FftSettings::default(), (16, 8))), drawn: false };
    let trigger = EdgeTrigger { level: 0.0, hyst: 0.0, slope: Slope::Rising, holdoff: 0.0 };
    let mut frames = Vec::new();
    {
        let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, Window { pre: 0.0, post: 1.0 }, vec![lane]), Duration::ZERO, |frame, stats| {
            frames.push((frame.clone(), stats));
            Published::Taken(frame)
        });
        let values = [0.5; 64];
        let chans = [ChanData::F32(&values)];
        intake.block(&Block { index: 0, t0: 0.0, dt: 1.0 / 64.0, chans: &chans, clipped: &[], trigger: Some(0.0) });
        intake.flush();
    }
    assert_eq!(frames.len(), 2, "the all-hidden block must publish its final spectrum after the 64th sample already flushed");
    assert_eq!(frames[1].1.samples, 0, "the final publication must contain only the completed measurement");
    assert!(frames[1].1.spectrum_updated, "a hidden FFT completion must independently request publication");
    assert!(frames[1].1.spectrum[0].measurements.is_some(), "the hidden FFT must still answer at the block end");
    for (frame, _) in &frames {
        assert_eq!(frame.lanes[0].waveforms, 0, "all-hidden time pictures must remain unrastered");
        let spectrum = frame.spectra[0].as_ref().expect("an open lens retains its spectrum storage");
        assert_eq!(spectrum.waveforms, 0, "measurement publication must not rasterise the spectrum");
        assert!(spectrum.dirty.is_empty(), "measurement publication must not create an upload rectangle");
    }
}

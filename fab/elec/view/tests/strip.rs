//! The time strip's model (`fab_elec_view::live::strip`): its time mapping, the 1-2-5 rate menu,
//! frame steps, loop wrap and the collapsed state, without a window.

use fab_elec_design::{CursorSource, TimeCursor};
use fab_elec_sim::trace::{TraceStore, CHUNK};
use fab_elec_view::live::strip::*;
use fab_elec_view::trace::columns::Columns;

fn cursor(t: f64, rate: f64, playing: bool) -> TimeCursor {
    TimeCursor { t, playing, rate, source: CursorSource::Sim, fw: None }
}

/// The recording length field reads what people type and shows it one way.
#[test]
fn the_recording_length_reads_seconds_and_milliseconds() {
    use fab_elec_view::live::transport::{length_text, parse_length};
    for (typed, seconds) in [("2", 2.0), ("2.5 s", 2.5), ("1.5s", 1.5), ("500 ms", 0.5), (" 750MS ", 0.75), ("60", 60.0), ("0.1", 0.1)] {
        assert_eq!(parse_length(typed), Some(seconds), "{typed:?} reads as {seconds} s");
    }
    for typed in ["", "s", "fast", "0", "-1", "0.05", "61", "2 kHz", "nan"] {
        assert_eq!(parse_length(typed), None, "{typed:?} is no length");
    }
    assert_eq!(length_text(2.5), "2.50 s", "the field shows two decimals");
}

#[test]
fn strip_model() {
    let st = StripState::new((1.0, 3.0));
    assert!(!st.collapsed(), "a span of 2 s is a strip");
    for t in [1.0, 1.25, 2.0, 2.999, 3.0] {
        let back = st.t_of(st.x_of(t, 40.0, 500.0), 40.0, 500.0);
        assert!((back - t).abs() < 1e-12, "x <-> t round trip at {t}: {back}");
    }
    assert_eq!(st.x_of(1.0, 40.0, 500.0), 40.0, "span start maps to the bar's left edge");
    assert_eq!(st.x_of(3.0, 40.0, 500.0), 540.0, "span end maps to the bar's right edge");
    assert_eq!(st.t_of(-1e6, 40.0, 500.0), 1.0, "a pointer left of the bar clamps to the span start");
    assert_eq!(st.t_of(1e6, 40.0, 500.0), 3.0, "a pointer right of the bar clamps to the span end");

    // The rate menu: 19 entries from 1/1000x to 1000x, each step the next of 1, 2, 5.
    assert_eq!((RATES[0], RATES[18]), (0.001, 1000.0), "menu ends");
    let mut mantissas = [1.0f64, 2.0, 5.0].iter().cycle();
    for r in RATES {
        let m = mantissas.next().unwrap();
        let e = (r / m).log10().round();
        assert!((r - m * 10f64.powf(e)).abs() <= r * 1e-12, "{r} is a 1-2-5 step");
    }
    assert_eq!(StripState::rate_step(1.0, 1), 2.0, "1x up is 2x");
    assert_eq!(StripState::rate_step(2.0, 1), 5.0, "2x up is 5x");
    assert_eq!(StripState::rate_step(5.0, 1), 10.0, "5x up is 10x");
    assert_eq!(StripState::rate_step(1.0, -1), 0.5, "1x down is 0.5x");
    assert_eq!(StripState::rate_step(1000.0, 1), 1000.0, "held at the top");
    assert_eq!(StripState::rate_step(0.001, -1), 0.001, "held at the bottom");
    assert_eq!(StripState::rate_step(3.0, 0), 2.0, "an off-menu rate snaps to the nearest entry");
    assert_eq!(StripState::live_rate_step(1000.0, 1), f64::INFINITY, "a live source's menu goes on to max");
    assert_eq!(StripState::live_rate_step(f64::INFINITY, 1), f64::INFINITY, "held at max");
    assert_eq!(StripState::live_rate_step(f64::INFINITY, -1), 1000.0, "max down is the top step");
    assert_eq!(StripState::live_rate_step(1.0, -1), 0.5, "below max a live menu is the same 1-2-5 steps");

    // A step is one frame of the rate's simulated time; forward then back returns.
    assert!((StripState::step_size(2.0) - 2.0 / 60.0).abs() < 1e-15, "step size is rate / 60");
    let start = cursor(1.7, 5.0, true);
    let fwd = st.step(start, 1);
    assert!(!fwd.playing && fwd.t > start.t, "a step pauses and moves forward");
    let back = st.step(fwd, -1);
    assert!((back.t - start.t).abs() < 1e-12, "step forward then back returns to the start");
    assert_eq!(st.step(cursor(1.0, 1.0, false), -1).t, 1.0, "a step back at the start stays in the span");
    // Past a live head no frame is recorded: a paused step forward asks the run for one frame.
    let head = StripState { live: true, ..st };
    assert_eq!(head.step_ahead(cursor(3.0, 2.0, false), None), Some(2.0 / 60.0),
        "at a paused live head a step asks the run for one frame at the cursor's rate");
    assert_eq!(head.step_ahead(cursor(3.0, f64::INFINITY, false), Some(0.5)), Some(0.5 / 60.0),
        "at max, one frame of the rate the run reached");
    assert_eq!(head.step_ahead(cursor(3.0, f64::INFINITY, false), None), Some(1.0 / 60.0),
        "at max before the run reports a rate, one frame at 1x");
    assert_eq!(head.step_ahead(cursor(2.0, 2.0, false), None), None, "inside the record a step moves the cursor");
    assert_eq!(head.step_ahead(cursor(3.0, 2.0, true), None), None, "a playing cursor's step pauses it at the head");
    assert_eq!(st.step_ahead(cursor(3.0, 2.0, false), None), None, "a finished run's end has nothing ahead");

    // A playing cursor that passes the loop's end comes back to the loop's start.
    let looped = StripState { loop_range: Some((1.5, 2.0)), ..st };
    let c = looped.tick(cursor(1.99, 1.0, true), 0.03);
    assert!(c.playing && (c.t - 1.52).abs() < 1e-9, "loop wraps by the overshoot, got {}", c.t);
    assert!(looped.tick(cursor(1.6, 1.0, true), 0.1).t > 1.6, "inside the loop it just plays");
    let c = st.tick(cursor(2.99, 1.0, true), 0.5);
    assert!(!c.playing && c.t == 3.0, "without a loop playback stops at the span's end");
    assert_eq!(st.play_from(3.0), 1.0, "play at the end of a finished run starts over");
    let live = StripState { live: true, ..st };
    assert!(live.tick(cursor(2.99, 1.0, true), 0.5).playing, "playback at a live head keeps following it");
    assert!(!live.at_head(cursor(2.99, 1.0, true)), "playback before the live head still needs frames");
    assert!(live.at_head(cursor(3.0, 1.0, true)), "playback at the live head waits for the source");
    assert!(live.at_head(live.tick(cursor(2.99, 1.0, true), 0.5)), "an extrapolated cursor past the live head also waits");
    assert!(!st.at_head(cursor(3.0, 1.0, true)), "a finished recording still settles at its end");
    assert_eq!(live.play_from(3.0), 3.0, "play at a live head goes on from the head");

    // Collapsed: nothing runs.
    for span in [(0.0, 0.0), (2.0, 1.0), (0.0, f64::NAN)] {
        assert!(StripState::new(span).collapsed(), "span {span:?} collapses the strip");
        assert!(!StripState { span, live: true, ..st }.at_head(cursor(3.0, 1.0, true)), "span {span:?} has no live head while collapsed");
    }

    // Read-outs.
    assert_eq!(format_time(4.2e-3), "4.20 ms");
    assert_eq!(format_time(0.0), "0 s");
    assert_eq!(format_time(12.5e-6), "12.5 \u{b5}s");
    assert_eq!(format_time(0.9996), "1.00 s", "rounding carries into the next unit");
    assert_eq!(format_ratio(0.0005), "sim 1 : 2\u{2009}000", "a simulated second takes 2 000 real ones");
    assert_eq!([0.7142, 0.01234, 23.44, 9.996, 1234.4].map(format_reached), ["0.71", "0.0123", "23.4", "10.0", "1\u{2009}230"],
        "a reached rate has three significant digits, as a time (two decimals from a tenth to one), and grouped thousands");
    assert_eq!(format_ratio(2000.0), "sim 2\u{2009}000 : 1", "the simulation runs ahead of real time");
    assert_eq!(format_ratio(1.0), "sim 1 : 1");
    assert_eq!(format_rate(0.05), "1/20x");
    assert_eq!(format_rate(200.0), "200x");
}

#[test]
fn record_columns_keep_square_wave_edges() {
    let n = CHUNK + 64;
    let bin = n / 8;
    let (low, high) = (-2.0, 3.0);
    let mut store = TraceStore::new(1, &[false]);
    let mut closed = Vec::new();
    for i in 0..n {
        let y = if (i + bin / 2) / (2 * bin) % 2 == 0 { low } else { high };
        store.push(i as f64, &[y], &mut closed);
    }
    let mut columns = Columns { cols_data: Vec::with_capacity(8), ..Columns::default() };
    let capacity = columns.cols_data.capacity();
    columns.reduce_trace_into(&store, 0, 0.0, n as f64, 8);
    assert_eq!(columns.cols_data.len(), 8, "one envelope per occupied column");
    for (i, column) in columns.cols_data.iter().enumerate() {
        let expected = if i % 2 == 1 { (low as f32, high as f32) }
            else { let level = (if i % 4 == 0 { low } else { high }) as f32; (level, level) };
        assert_eq!((column.lo, column.hi), expected, "column {i}: edges keep both levels, flat columns stay flat");
        assert_eq!(column.count as usize, bin, "column {i} covers its whole time bin");
    }
    assert_eq!(columns.cols_data.capacity(), capacity, "reduction keeps the supplied buffer");
}

#[test]
fn record_layout_clamps_snaps_and_counts_hidden_lanes() {
    for (height, rows, max, released, expected) in [
        (-10.0, 3, 60.0, false, (0.0, 0, 3)),
        (50.0, 3, -1.0, false, (0.0, 0, 3)),
        (100.0, 6, 60.0, false, (60.0, 5, 1)),
        (11.5, 3, 60.0, false, (11.5, 0, 3)),
        (11.5, 3, 60.0, true, (0.0, 0, 3)),
        (40.0, 3, 11.5, true, (0.0, 0, 3)),
        (12.0, 3, 60.0, true, (12.0, 1, 2)),
        (25.0, 3, 60.0, true, (25.0, 2, 1)),
        (36.0, 3, 60.0, true, (36.0, 3, 0)),
        (60.0, 0, 60.0, true, (60.0, 0, 0)),
        (12_000.0, 1000, 12_000.0, true, (12_000.0, 1000, 0)),
    ] {
        let layout = RecordLayout::new(height, rows, max, released);
        assert_eq!((layout.height, layout.shown, layout.hidden), expected, "height {height}, rows {rows}, max {max}, released {released}");
        assert_eq!(layout.shown + layout.hidden, rows, "every lane is shown or counted in +N lanes");
        assert_eq!(layout.bar_top(40.0), 40.0 + layout.height, "the bar moves by exactly the record height");
        if layout.shown > 0 {
            assert!(layout.row_height() >= RECORD_ROW_MIN, "visible rows keep at least 12 px");
            assert_eq!(layout.row_height() * layout.shown as f64, layout.height, "visible rows split all the height evenly");
        } else { assert_eq!(layout.row_height(), 0.0, "no rows divide by zero"); }
    }
}

#[test]
fn violation_ticks_hit_only_their_columns_and_jump_to_exact_events() {
    use fab_elec_bench::{source::Intake, trigger::{EdgeTrigger, Slope, Window}};
    use fab_elec_view::instruments::{
        acquire::{AcquireIntake, Acquisition, LaneSetup, Published, TrendSlot},
        phosphor::PhosphorMap, simfeed::EventSnapshot, trend::{TrendBound, TrendQuantity},
    };
    use std::{sync::Arc, time::Duration};

    let window = Window { pre: 0.1, post: 0.65 };
    let trigger = EdgeTrigger { level: 0.5, hyst: 0.2, slope: Slope::Rising, holdoff: 0.0 };
    let lanes = vec![LaneSetup::time(PhosphorMap::screen(16, 6, window.pre, window.pre + window.post, 0.0, 1.0))];
    let slot = Arc::new(TrendSlot::default());
    assert!(slot.set_bounds(&[TrendBound { pin: (0, TrendQuantity::Width), min: Some(0.3), max: Some(0.6) }]));
    let mut snapshot = EventSnapshot::default();
    {
        let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes), Duration::MAX, |frame, stats| {
            snapshot = EventSnapshot { list: stats.events.unwrap(), violations: stats.violations, ..EventSnapshot::default() };
            Published::Taken(frame)
        });
        intake.record_events(); intake.follow_trends(slot);
        for cycle in 0..6 {
            let runt = cycle == 1 || cycle == 4;
            let width = if runt { 0.2 } else { 0.5 };
            for (i, (dt, y)) in [(0.0, 0.0), (0.1, 0.0), (0.2, 0.5), (0.3, 1.0),
                (0.1 + width, 1.0), (0.2 + width, 0.5), (0.3 + width, 0.0), (0.9, 0.0)].into_iter().enumerate() {
                if runt && i == 4 { continue; }
                intake.point(cycle as f64 + dt, &[y]);
            }
        }
        intake.idle();
    }
    let (events, violations) = (&snapshot.list, &snapshot.violations);
    assert_eq!(violations.len(), 2, "only the two runt events fail");
    let strip = StripState::new((0.0, 6.0));
    let times = [events.get(1).unwrap(), events.get(4).unwrap()];
    let columns = times.map(|t| strip.x_of(t, 0.0, 120.0).floor() as usize);
    assert_eq!(violation_columns(events, violations, strip.span, 120).collect::<Vec<_>>(), columns.map(|x| (x, 1)).to_vec());
    for x in 0..120 {
        let expected = columns.iter().position(|&column| column == x).map(|i| times[i]);
        assert_eq!(strip.violation_at(events, violations, x as f64 + 0.5, 120), expected, "column {x} selects only its drawn violation");
    }
    assert_eq!(strip.adjacent_violation(events, violations, times[0], true), Some(times[1]));
    assert_eq!(strip.adjacent_violation(events, violations, times[1], false), Some(times[0]));
    assert_eq!(strip.adjacent_violation(events, violations, times[0], false), None);
    let trimmed = StripState::new((times[0], times[1]));
    assert_eq!(violation_columns(events, violations, trimmed.span, 1).collect::<Vec<_>>(), vec![(0, 2)], "dense endpoint failures share one tick");
    assert_eq!(trimmed.violation_at(events, violations, 0.5, 1), Some(times[0]), "a dense tick chooses its first failing event");
    assert_eq!(strip.violation_at(events, violations, -0.5, 120), None);
    assert_eq!(violation_columns(events, violations, strip.span, 0).count(), 0);
}

/// The rate slider (the user's "speed slider"): 1/1000x to 1000x on a log scale, readable rates in
/// between, 1x within a few points of its mark, and every menu rate back where the knob shows it.
#[test]
fn rate_slider_spans_the_menu_on_a_log_scale_and_rests_at_1x() {
    let w = 104.0;
    assert_eq!(StripState::slider_of(0.001), 0.0, "the slowest rate at the left end");
    assert_eq!(StripState::slider_of(1000.0), 1.0, "the fastest at the right end");
    assert_eq!(StripState::slider_of(f64::INFINITY), 1.0, "max at the right end too");
    assert!((StripState::slider_of(1.0) - 0.5).abs() < 1e-12, "1x in the middle of the log scale");
    assert_eq!(StripState::rate_at_slider(0.0, w), 0.001, "the left end is 1/1000x");
    assert_eq!(StripState::rate_at_slider(1.0, w), 1000.0, "the right end is 1000x");
    assert_eq!(StripState::rate_at_slider(-0.5, w), 0.001, "held at the left end");
    assert_eq!(StripState::rate_at_slider(1.5, w), 1000.0, "held at the right end");
    // The detent's own edge is a float compare; a tenth of a point inside it is the property.
    for points in [0.1 - SLIDER_DETENT, -1.0, 0.0, 1.0, SLIDER_DETENT - 0.1] {
        assert_eq!(StripState::rate_at_slider(0.5 + points / w, w), 1.0, "{points} points from the 1x mark rests at 1x");
    }
    let past = SLIDER_DETENT + 1.0;
    assert!(StripState::rate_at_slider(0.5 + past / w, w) > 1.0 && StripState::rate_at_slider(0.5 - past / w, w) < 1.0,
        "past the detent the rate moves on either side");
    for (at, want) in [(0.0347, 0.035), (3.47, 3.5), (347.0, 350.0), (0.00123, 0.0012)] {
        assert_eq!(StripState::rate_at_slider(StripState::slider_of(at), w), want, "{at}x reads as two significant digits");
    }
    assert_eq!(format_rate(0.035), "0.035x", "a slider rate reads plainly");
    assert_eq!(format_rate(350.0), "350x");
    for r in RATES {
        assert_eq!(StripState::rate_at_slider(StripState::slider_of(r), w), r, "the menu rate {r}x is where the knob shows it");
    }
    let mut last = 0.0;
    for i in 0..=400 {
        let rate = StripState::rate_at_slider(i as f64 / 400.0, w);
        assert!(rate >= last, "the rate never falls along the slider: {rate} after {last} at {i}/400");
        last = rate;
    }
}

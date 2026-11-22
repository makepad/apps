use fab_elec_bench::trigger::{EdgeTrigger, Slope, Window};
use fab_elec_sim::{Sink, stamp::{UnknownInfo, UnkKind}};
use fab_elec_view::{
    instruments::{acquire::{AcquireIntake, Acquisition, LaneSetup, NetProbe, Published, SimChannel, SimIntake}, events::EventList, phosphor::PhosphorMap},
    live::strip::{tick_columns, StripState},
};
use fab_elec_bench::source::ChanId;
use std::time::Duration;

fn square(cycles: usize) -> Vec<(f64, f64)> {
    // A 1 kHz square with finite edges, starting at a nonzero simulator time. The level
    // sample leaves a crossing pending until the next sample confirms hysteresis.
    (0..cycles).flat_map(|cycle| {
        let height = if cycle == 2 { 0.25 } else { 1.0 };
        [(0.0, 0.0), (0.0001, 0.0), (0.0002, 0.5), (0.0003, 1.0), (0.0006, 1.0), (0.0007, 0.5), (0.0008, 0.0)]
            .into_iter().map(move |(dt, fraction)| (2.0 + cycle as f64 * 0.001 + dt, height * fraction))
    }).collect()
}

fn acquire(samples: &[(f64, f64)], trigger: EdgeTrigger, ends: &[usize], refuse_once: bool) -> Vec<EventList> {
    let window = Window { pre: 0.0001, post: 0.0002 };
    let lanes = vec![LaneSetup::time(PhosphorMap::screen(16, 6, window.pre, window.pre + window.post, -0.25, 1.25))];
    let mut snapshots = Vec::new();
    let mut refused = false;
    let mut accepted = 0;
    {
        let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes), Duration::MAX, |frame, stats| {
            if refuse_once && !refused { refused = true; return Published::Kept(frame); }
            accepted += stats.trigger_events;
            let events = stats.events.expect("the nominal intake publishes accepted times");
            assert_eq!(events.len() as u64, accepted, "snapshot times and accepted-trigger counters agree");
            snapshots.push(events);
            Published::Taken(frame)
        });
        intake.record_events();
        let mut sink = SimIntake::new(vec![SimChannel::voltage(ChanId(0), NetProbe { pos: "signal".into(), neg: None, scale: 1.0 })], intake);
        sink.begin(&[
            UnknownInfo { name: "0".into(), kind: UnkKind::Voltage },
            UnknownInfo { name: "signal".into(), kind: UnkKind::Voltage },
        ]);
        let mut from = 0;
        for &end in ends {
            for &(t, y) in &samples[from..end] { sink.point(t, None, &[0.0, y]); }
            sink.idle();
            from = end;
        }
        sink.idle(); // Also retry a refused final publication without any new samples.
        let &(last, y) = samples.last().unwrap();
        sink.point(last + 0.001, None, &[0.0, y]);
        sink.idle(); // A sample with no new event must reuse the immutable snapshot.
    }
    assert!(snapshots[snapshots.len() - 2].same(snapshots.last().unwrap()), "unchanged events do not rebuild the list");
    snapshots
}

fn times(events: &EventList) -> Vec<f64> { (0..events.len()).map(|i| events.get(i).unwrap()).collect() }

#[test]
fn acquisition_events_follow_source_time_and_chunked_delivery() {
    let samples = square(8);
    let trigger = EdgeTrigger { level: 0.5, hyst: 0.2, slope: Slope::Rising, holdoff: 0.0015 };
    let whole = acquire(&samples, trigger, &[samples.len()], false);
    let expected: Vec<_> = [0, 3, 5, 7].into_iter().map(|cycle| samples[cycle * 7 + 2].0).collect();
    assert_eq!(times(&whole[0]), expected, "hysteresis and holdoff reject the runt and intervening edges at their exact simulator crossing times");

    // First cut: pending confirmation. Second cut: two accepted events, during holdoff.
    let chunks = acquire(&samples, trigger, &[3, 28, samples.len()], false);
    assert!(chunks[0].is_empty(), "a crossing waits for confirmation across a delivery boundary");
    assert_eq!(times(&chunks[1]), expected[..2], "an earlier snapshot remains immutable after later publication");
    assert_eq!(times(&chunks[2]), expected, "three chunks equal one pass, with no duplicate edge at a cut");
    let retried = acquire(&samples, trigger, &[samples.len()], true);
    assert_eq!(times(&retried[0]), expected, "a full frame queue loses and duplicates no event on retry");

    let falling = acquire(&samples, EdgeTrigger { slope: Slope::Falling, ..trigger }, &[3, 28, samples.len()], false);
    let expected: Vec<_> = [0, 3, 5, 7].into_iter().map(|cycle| samples[cycle * 7 + 5].0).collect();
    assert!(falling[0].is_empty(), "a new feed begins without the previous slope's events");
    assert_eq!(times(&falling[2]), expected, "the replacement acquisition records only falling crossings");
    let unheld = acquire(&samples, EdgeTrigger { holdoff: 0.0, ..trigger }, &[samples.len()], false);
    assert_eq!(unheld[0].len(), 7, "without holdoff only the runt is excluded");

    let events = &chunks[2];
    let t = times(events);
    let span = (t[0], t[3]);
    for width in [1, 2, 3, 100] {
        let columns: Vec<_> = tick_columns(events, span, width).collect();
        assert_eq!(columns.iter().map(|(_, count)| count).sum::<usize>(), events.len(), "each event belongs to exactly one column at width {width}");
        assert!(columns.windows(2).all(|pair| pair[0].0 < pair[1].0), "occupied columns are unique and sorted at width {width}");
        assert!(columns.iter().all(|&(x, count)| x < width && count > 0), "columns stay within the track at width {width}");
        assert_eq!(columns.first().unwrap().0, 0, "the left endpoint is included at width {width}");
        assert_eq!(columns.last().unwrap().0, width - 1, "the right endpoint is included once at width {width}");
    }
    assert_eq!(tick_columns(events, (t[1], t[2]), 1).collect::<Vec<_>>(), vec![(0, 2)], "visible endpoints across sealed chunks are counted once");
    assert_eq!(tick_columns(events, span, 0).count(), 0, "a zero-width track has no ticks");
    assert_eq!(tick_columns(events, (1.0, 1.0), 10).count(), 0, "a collapsed span has no ticks");
    let strip = StripState::new(span);
    let x = strip.x_of(t[1], 0.0, 100.0).floor();
    assert_eq!(strip.event_at(events, x + 2.9, 100, 3.0), Some(t[1]), "a click within tick slop keeps the exact event time");
    assert_eq!(strip.event_at(events, x + 3.1, 100, 3.0), None, "a click outside tick slop remains a free scrub");
    assert_eq!(strip.event_at(events, 96.1, 100, 3.0), Some(t[3]), "hit slop follows the drawn right-end column");
    for i in 0..t.len() {
        assert_eq!(strip.adjacent_event(events, t[i], true), t.get(i + 1).copied(), "next visits every event once, including the chunk boundary");
        assert_eq!(strip.adjacent_event(events, t[i], false), i.checked_sub(1).map(|i| t[i]), "previous visits every event once, including the chunk boundary");
    }
    let trimmed = StripState::new((t[1], t[2]));
    assert_eq!(trimmed.adjacent_event(events, t[0] - 1.0, true), Some(t[1]), "a cursor before the record skips retained older events");
    assert_eq!(trimmed.adjacent_event(events, t[3] + 1.0, false), Some(t[2]), "a cursor after the visible span skips newer events");
    assert_eq!(trimmed.adjacent_event(events, t[1], false), None, "an event older than the record cannot be jumped to");
    assert_eq!(trimmed.event_at(events, -4.0, 100, 3.0), None, "retained older events cannot be clicked outside the record");
}

#[test]
fn sealed_chunks_and_dense_columns_preserve_boundary_events() {
    // Enough events for full chunks, a partial tail, and several directory carries. Only
    // the intake sees samples; the strip works from chunk counts even in a one-pixel view.
    let samples = square(1030);
    let trigger = EdgeTrigger { level: 0.5, hyst: 0.2, slope: Slope::Rising, holdoff: 0.0 };
    let snapshots = acquire(&samples, trigger, &[260 * 7, 519 * 7, samples.len()], false);
    let events = &snapshots[2];
    let expected: Vec<_> = (0..1030).filter(|&cycle| cycle != 2).map(|cycle| samples[cycle * 7 + 2].0).collect();
    assert_eq!(times(events), expected, "sealing and joining chunks preserves every time");
    let span = (expected[0], *expected.last().unwrap());
    assert_eq!(tick_columns(events, span, 1).collect::<Vec<_>>(), vec![(0, expected.len())], "a dense column includes all full and partial chunks");
    for i in [255, 256, 258, 259, 511, 512, 514, 515, 517, 518, 767, 768, 773, 774, 1023, 1024, 1028] {
        let t = expected[i];
        assert_eq!(events.partition_point(|event| event < t), i, "lower bound at a chunk edge or the open tail's edge");
        assert_eq!(events.partition_point(|event| event <= t), i + 1, "upper bound counts the boundary once");
        let strip = StripState::new(span);
        assert_eq!(strip.adjacent_event(events, t, false), Some(expected[i - 1]));
        assert_eq!(strip.adjacent_event(events, t, true), expected.get(i + 1).copied());
        assert_eq!(tick_columns(events, (expected[i - 1], t), 2).collect::<Vec<_>>(), vec![(0, 1), (1, 1)], "a chunk boundary at the closed right endpoint is drawn once");
    }
}

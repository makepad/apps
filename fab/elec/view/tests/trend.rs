use fab_elec_bench::{source::ChanId, trigger::{EdgeTrigger, Slope, Window}};
use fab_elec_sim::{Sink, stamp::{UnknownInfo, UnkKind}};
use fab_elec_view::{
    instruments::{
        acquire::{AcquireIntake, Acquisition, LaneSetup, NetProbe, Published, SimChannel, SimIntake, TrendSlot},
        events::EventList, phosphor::PhosphorMap, simfeed::EventSnapshot, trend::{TrendBound, TrendQuantity, TrendSeries},
    },
    live::strip::{tick_columns, StripState},
};
use std::{sync::Arc, time::Duration};

fn acquire(samples: &[(f64, f64)], pins: &[(usize, TrendQuantity)]) -> Vec<(EventList, Arc<Vec<TrendSeries>>)> {
    acquire_bounds(samples, pins, &[], &[]).into_iter().map(|snapshot| (snapshot.list, snapshot.columns)).collect()
}

fn acquire_bounds(samples: &[(f64, f64)], pins: &[(usize, TrendQuantity)], bounds: &[TrendBound], stop: &[usize]) -> Vec<EventSnapshot> {
    let window = Window { pre: 0.0001, post: 0.00115 };
    let lanes = vec![LaneSetup::time(PhosphorMap::screen(16, 6, window.pre, window.pre + window.post, -0.25, 1.25)); 2];
    let trigger = EdgeTrigger { level: 0.5, hyst: 0.2, slope: Slope::Rising, holdoff: 0.0 };
    let slot = Arc::new(TrendSlot::default());
    assert!(slot.set(pins), "an uncontended pin request is published");
    assert!(slot.set_bounds(bounds), "an uncontended bound request is published");
    assert!(slot.arm_stop_on_violation(stop), "an uncontended arm request is published");
    let mut snapshots = Vec::new();
    {
        let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes), Duration::MAX, |frame, stats| {
            snapshots.push(EventSnapshot { list: stats.events.expect("nominal snapshots carry events"), columns: stats.columns, violations: stats.violations, event_epoch: stats.event_epoch, stopped: stats.stopped, ..EventSnapshot::default() });
            Published::Taken(frame)
        });
        intake.record_events();
        intake.follow_trends(slot);
        let channels = ["signal", "other"].into_iter().enumerate().map(|(i, name)| {
            SimChannel::voltage(ChanId(i as u32), NetProbe { pos: name.into(), neg: None, scale: 1.0 })
        }).collect();
        let mut sink = SimIntake::new(channels, intake);
        sink.begin(&[
            UnknownInfo { name: "0".into(), kind: UnkKind::Voltage },
            UnknownInfo { name: "signal".into(), kind: UnkKind::Voltage },
            UnknownInfo { name: "other".into(), kind: UnkKind::Voltage },
        ]);
        for (i, &(t, y)) in samples.iter().enumerate() {
            // The second lane has the same crossings but a different voltage range.
            sink.point(t, None, &[0.0, y, 2.0 + 2.0 * y]);
            if [256 * 8 - 1, 257 * 8 - 1, 512 * 8 - 1, 513 * 8 - 1, 770 * 8 - 1, 1024 * 8 - 1, 1025 * 8 - 1].contains(&i) { sink.idle(); }
        }
        sink.idle();
        let &(t, y) = samples.last().unwrap();
        sink.point(t + 0.00001, None, &[0.0, y, 2.0 + 2.0 * y]);
        sink.idle();
    }
    snapshots
}

#[test]
fn pinned_answers_keep_event_indices_extrema_and_exact_scrubs() {
    // 1,000 complete spans and a final rise to measure the last period. That rise is
    // event 1,000, whose own span remains unfinished and must not append a value.
    // Crossings first land at the level, then confirm on a later sample.
    let mut samples: Vec<_> = (0..1000).flat_map(|cycle| {
        let width = if cycle == 613 { 0.0002 } else { 0.0005 };
        [(0.0, 0.0), (0.0001, 0.0), (0.0002, 0.5), (0.0003, 1.0),
            (0.0001 + width, 1.0), (0.0002 + width, 0.5), (0.0003 + width, 0.0), (0.0009, 0.0)]
            .into_iter().enumerate().filter(move |&(i, _)| cycle != 613 || i != 4)
            .map(move |(_, (dt, y))| (2.0 + cycle as f64 * 0.001 + dt, y))
    }).collect();
    // The short pulse's top begins at the end of its rise; don't duplicate that time.
    samples.extend([(3.0, 0.0), (3.0001, 0.0), (3.0002, 0.5), (3.0003, 1.0), (3.0004, 1.0)]);
    let pins = [(0, TrendQuantity::Width), (0, TrendQuantity::Freq), (0, TrendQuantity::Duty),
        (0, TrendQuantity::Vpp), (0, TrendQuantity::Mean), (1, TrendQuantity::Width)];
    let snapshots = acquire(&samples, &pins);
    let (events, columns) = snapshots.last().unwrap();
    let width = &columns[0].values;
    let freq = &columns[1].values;
    assert_eq!(events.len(), 1001, "the trailing rise is recorded before its span finishes");
    assert_eq!(width.len(), 1000, "exactly the 1,000 finished spans append values");
    assert!(columns.iter().all(|column| column.values.len() == 1000), "every pin appends once for each completed span");
    let outliers: Vec<_> = (0..width.len()).filter(|&i| (width.get(i).unwrap() - 0.0005).abs() > 0.0005 * 0.02).collect();
    assert_eq!(outliers, vec![613], "only cycle 613 lies outside two percent of nominal width");
    for i in 0..1000 {
        assert!(width.get(i).unwrap().is_finite(), "trigger-lane width is finite even for the first span: {i}");
        assert!((freq.get(i).unwrap() - 1000.0).abs() < 1.0, "frequency stays within 0.1 percent at event {i}");
        let duty = if i == 613 { 20.0 } else { 50.0 };
        assert!((columns[2].values.get(i).unwrap() - duty).abs() < 0.01, "duty follows the first pulse at event {i}");
        assert_eq!(columns[3].values.get(i), Some(1.0), "Vpp includes clipped endpoints and interior peaks at event {i}");
        let expected = (if i == 612 { 0.00061875 } else if i == 613 { 0.000325 } else { 0.000625 }) / 0.00115;
        assert!((f64::from(columns[4].values.get(i).unwrap()) - expected).abs() < 1e-6, "mean integrates trapezoids over the post span at event {i}");
    }
    let (min, argmin, max, _) = width.extremes(0, width.len()).unwrap();
    assert_eq!(argmin, 613, "whole-column argmin identifies the one bad pulse");
    assert!((min - 0.0002).abs() < 1e-8 && (max - 0.0005).abs() < 1e-8, "whole-column extrema retain their values");
    for i in [255, 256, 511, 512, 767, 768, 999] {
        assert_eq!(width.extremes(i, i + 1), Some((width.get(i).unwrap(), i, width.get(i).unwrap(), i)), "single-value queries preserve chunk and tree/tail boundary index {i}");
        assert!((events.get(i).unwrap() - (2.0002 + i as f64 * 0.001)).abs() < 1e-12, "event/value alignment survives boundary {i}");
    }
    for (old_events, old_columns) in &snapshots {
        for (old, new) in old_columns.iter().zip(columns.iter()) {
            for i in 0..old.values.len() {
                assert_eq!(old.values.get(i).unwrap().to_bits(), new.values.get(i).unwrap().to_bits(), "publication never changes, loses or repeats an earlier value at {i}");
            }
        }
        assert!(old_columns.iter().all(|column| column.values.len() <= old_events.len()), "no column can lead its event list");
    }
    let previous = &snapshots[snapshots.len() - 2].1;
    assert!(previous.iter().zip(columns.iter()).all(|(a, b)| a.values.same(&b.values)), "unchanged tails are shared at publication");
    let other = &columns[5].values;
    assert!(other.get(0).unwrap().is_nan() && other.get(1).unwrap().is_nan(), "overlapping non-trigger spans wait for the first completed span");
    assert!((2..1000).all(|i| other.get(i).unwrap().is_finite()), "later non-trigger spans use the last completed range");
    let span = (events.get(0).unwrap(), events.get(999).unwrap());
    let strip = StripState::new(span);
    assert_eq!(tick_columns(events, span, 1).collect::<Vec<_>>(), vec![(0, 1000)], "one dense pixel includes the entire completed record once");
    assert_eq!(strip.trend_at(events, width, 0.5, 1, 1.0, (min, max)), events.get(613), "pressing the bar's lower half scrubs to event 613's exact source time");
    let max_index = width.extremes(0, 1000).unwrap().3;
    assert_eq!(strip.trend_at(events, width, 0.5, 1, 0.0, (min, max)), events.get(max_index), "pressing the upper half selects the maximum event");
    let unpinned = acquire(&samples, &[]);
    assert!(unpinned.iter().all(|(_, columns)| columns.is_empty()), "an unpinned second pass produces no columns");
    assert_eq!(unpinned.last().unwrap().0.len(), events.len(), "pinning does not alter accepted triggers");
}

#[test]
fn live_pin_edits_share_lane_thresholds_and_restarts_discard_open_spans() {
    use fab_elec_bench::source::Intake;
    use fab_elec_view::instruments::acquire::TriggerSlot;
    use std::sync::Mutex;

    let window = Window { pre: 0.0001, post: 0.00115 };
    let lanes = vec![LaneSetup::time(PhosphorMap::screen(16, 6, window.pre, window.pre + window.post, 0.0, 5.0)); 2];
    let trigger = EdgeTrigger { level: 0.5, hyst: 0.2, slope: Slope::Rising, holdoff: 0.0 };
    let latest = Arc::new(Mutex::new((EventList::default(), Arc::new(Vec::<TrendSeries>::new()))));
    let published = latest.clone();
    let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes), Duration::MAX, move |frame, stats| {
        *published.lock().unwrap() = (stats.events.unwrap(), stats.columns);
        Published::Taken(frame)
    });
    let pins = Arc::new(TrendSlot::default());
    assert!(pins.set(&[(1, TrendQuantity::Vpp)]), "the first pin request is accepted");
    intake.record_events();
    intake.follow_trends(pins.clone());
    let cycles = |range: std::ops::Range<usize>, intake: &mut dyn Intake| {
        for cycle in range {
            for (dt, y) in [(0.0, 0.0), (0.0001, 0.0), (0.0002, 0.5), (0.0003, 1.0), (0.0006, 1.0), (0.0007, 0.5), (0.0008, 0.0), (0.0009, 0.0)] {
                intake.point(cycle as f64 * 0.001 + dt, &[y, 2.0 + 2.0 * y]);
            }
        }
        intake.idle();
    };
    cycles(0..4, &mut intake);
    let before = latest.lock().unwrap().clone();
    assert_eq!((before.0.len(), before.1[0].values.len()), (4, 3), "one unfinished span is still waiting when pins change");
    assert!(pins.set(&[(1, TrendQuantity::Width), (1, TrendQuantity::Vpp)]), "a timing pin can join an already observed lane");
    intake.flush();
    cycles(4..8, &mut intake);
    let added = latest.lock().unwrap().clone();
    let width = &added.1[0].values;
    assert_eq!(width.len(), 7, "adding a pin preserves absolute event indices");
    assert!((0..4).all(|i| width.get(i).unwrap().is_nan()), "a late pin represents its unobserved prefix as NaN");
    assert!((4..7).all(|i| (width.get(i).unwrap() - 0.0005).abs() < 1e-8), "a new timing pin reuses its lane's completed range immediately");
    assert_eq!(added.1[1].values.len(), 7, "reordering pins preserves the original column and its open spans");
    assert!(pins.set(&[(1, TrendQuantity::Width)]), "removing the other quantity keeps the lane observed");
    intake.flush();
    assert!(latest.lock().unwrap().1[0].values.same(width), "removing another pin shares the retained column");
    cycles(0..4, &mut intake);
    let restarted = latest.lock().unwrap().clone();
    assert_eq!((restarted.0.len(), restarted.1[0].values.len()), (4, 3), "backward time restarts events and columns together without the cut-short span");
    assert!(restarted.1[0].values.get(0).unwrap().is_nan() && restarted.1[0].values.get(1).unwrap().is_nan(), "backward time clears the previous threshold history");
    assert!((restarted.1[0].values.get(2).unwrap() - 0.0005).abs() < 1e-8, "the first completed new range enables later timing again");
    assert_eq!(width.len(), 7, "restarting does not mutate an older published column");
    let trigger_slot = Arc::new(TriggerSlot::default());
    trigger_slot.set(EdgeTrigger { level: 0.6, ..trigger }, fab_elec_bench::trigger::Qualifier::None, None, None);
    intake.follow_trigger(trigger_slot);
    intake.flush();
    let edited = latest.lock().unwrap().clone();
    assert!(edited.0.is_empty() && edited.1[0].values.is_empty(), "trigger edits clear event and trend history before the next sample");
}

fn pulses(count: usize, runt: usize) -> Vec<(f64, f64)> {
    let mut samples: Vec<_> = (0..count).flat_map(|cycle| {
        let width = if cycle == runt { 0.0002 } else { 0.0005 };
        [(0.0, 0.0), (0.0001, 0.0), (0.0002, 0.5), (0.0003, 1.0),
            (0.0001 + width, 1.0), (0.0002 + width, 0.5), (0.0003 + width, 0.0), (0.0009, 0.0)]
            .into_iter().enumerate().filter(move |&(i, _)| cycle != runt || i != 4)
            .map(move |(_, (dt, y))| (cycle as f64 * 0.001 + dt, y))
    }).collect();
    let end = count as f64 * 0.001;
    samples.extend([(end, 0.0), (end + 0.0001, 0.0), (end + 0.0002, 0.5), (end + 0.0003, 1.0), (end + 0.0004, 1.0)]);
    samples
}

#[test]
fn bounds_find_the_one_runt_without_a_pin() {
    let samples = pulses(10, 6);
    let bound = TrendBound { pin: (0, TrendQuantity::Width), min: Some(0.0003), max: Some(0.0006) };
    for bound in [bound.clone(), TrendBound { max: None, ..bound.clone() }] {
        let snapshots = acquire_bounds(&samples, &[], &[bound], &[0]);
        let snapshot = snapshots.last().unwrap();
        assert!(snapshot.columns.is_empty(), "automatic bound recording adds no user trend row");
        assert_eq!(snapshot.violations.len(), 1, "only the short pulse fails the width bound");
        let violation = snapshot.violations.get(0).unwrap();
        assert_eq!((violation.event, violation.bound), (6, 0), "the failure uses the accepted event and supplied bound indices");
        assert!((violation.value - 0.0002).abs() < 1e-12, "the stored value is the runt's measured width");
        assert_eq!(snapshot.stopped.map(|(_, v)| v), Some(violation), "the worker selects the first armed failure");
        assert!((snapshot.list.get(violation.event).unwrap() - 0.0062).abs() < 1e-12, "the stop resolves to the runt's exact crossing time");
    }
    let maximum = TrendBound { min: None, max: Some(0.0003), ..bound };
    let snapshots = acquire_bounds(&samples, &[], &[maximum], &[]);
    let snapshot = snapshots.last().unwrap();
    assert_eq!((0..snapshot.violations.len()).map(|i| snapshot.violations.get(i).unwrap().event).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4, 5, 7, 8, 9], "a maximum alone rejects the normal widths and accepts the runt");
    assert!(snapshot.stopped.is_none(), "judgement alone never arms a stop");
    let snapshots = acquire_bounds(&samples, &[(0, TrendQuantity::Width)], &[], &[]);
    assert!(snapshots.iter().all(|snapshot| snapshot.violations.is_empty()), "a pin without a bound produces no violations");
}

#[test]
fn violations_share_chunks_and_keep_event_order_across_lanes() {
    let samples = pulses(1030, 613);
    let bounds = [
        TrendBound { pin: (1, TrendQuantity::Vpp), min: None, max: Some(1.5) },
        TrendBound { pin: (0, TrendQuantity::Width), min: Some(0.0006), max: None },
    ];
    let snapshots = acquire_bounds(&samples, &[], &bounds, &[1]);
    let snapshot = snapshots.last().unwrap();
    assert_eq!(snapshot.violations.len(), 2060, "every finished event fails both bounds exactly once");
    for i in 0..snapshot.violations.len() {
        let v = snapshot.violations.get(i).unwrap();
        assert_eq!((v.event, v.bound), (i / 2, i % 2), "multiple lanes publish in event/bound order at {i}");
    }
    for i in [255, 256, 511, 512, 1023, 1024, 2059] {
        let v = snapshot.violations.get(i).unwrap();
        assert_eq!(snapshot.violations.partition_point(|kept| (kept.event, kept.bound) < (v.event, v.bound)), i, "chunk boundary {i} occurs once");
    }
    for old in &snapshots {
        for i in 0..old.violations.len() {
            assert_eq!(old.violations.get(i), snapshot.violations.get(i), "later publication preserves the old immutable prefix");
        }
    }
    assert!(snapshots[snapshots.len() - 2].violations.same(&snapshot.violations), "unchanged publications share their tail as well as sealed chunks");
    assert_eq!(snapshot.stopped.map(|(_, v)| (v.event, v.bound)), Some((0, 1)), "only an armed bound can select the stop");
}

#[test]
fn stop_arming_excludes_open_old_events_and_rearms_after_a_stop() {
    use fab_elec_bench::source::Intake;
    use std::sync::Mutex;

    let window = Window { pre: 0.0001, post: 0.00115 };
    let trigger = EdgeTrigger { level: 0.5, hyst: 0.2, slope: Slope::Rising, holdoff: 0.0 };
    let lanes = vec![LaneSetup::time(PhosphorMap::screen(16, 6, window.pre, window.pre + window.post, 0.0, 1.0))];
    let slot = Arc::new(TrendSlot::default());
    assert!(slot.set_bounds(&[TrendBound { pin: (0, TrendQuantity::Vpp), min: Some(1.5), max: None }]));
    let latest = Arc::new(Mutex::new(EventSnapshot::default()));
    let published = latest.clone();
    let mut refuse = true;
    let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes), Duration::MAX, move |frame, stats| {
        if stats.stopped.is_some() && refuse { refuse = false; return Published::Kept(frame); }
        *published.lock().unwrap() = EventSnapshot { list: stats.events.unwrap(), violations: stats.violations, event_epoch: stats.event_epoch, stopped: stats.stopped, ..EventSnapshot::default() };
        Published::Taken(frame)
    });
    intake.record_events(); intake.follow_trends(slot.clone());
    let samples = pulses(8, usize::MAX);
    for &(t, y) in &samples[..3 * 8] { intake.point(t, &[y as f32]); }
    intake.idle();
    assert_eq!(latest.lock().unwrap().violations.len(), 2, "old failures are recorded before arming");
    assert!(slot.arm_stop_on_violation(&[0]));
    intake.flush();
    for &(t, y) in &samples[3 * 8..6 * 8] { intake.point(t, &[y as f32]); }
    intake.idle(); intake.idle();
    let first = latest.lock().unwrap().clone();
    let (request, violation) = first.stopped.unwrap();
    assert_eq!(violation.event, 3, "event 2 was accepted before arming even though it finished afterward");
    assert_eq!(first.violations.len(), 5, "retrying a refused stop publication loses or duplicates no failure");
    assert!(slot.arm_stop_on_violation(&[0]));
    intake.flush();
    assert!(latest.lock().unwrap().stopped.is_none(), "rearming clears the old stop without clearing violations");
    for &(t, y) in &samples[6 * 8..] { intake.point(t, &[y as f32]); }
    intake.idle();
    let second = latest.lock().unwrap().clone();
    let (next, violation) = second.stopped.unwrap();
    assert!(next > request, "a new arm distinguishes the next stop from an old snapshot");
    assert_eq!(violation.event, 6, "resuming selects the first newly accepted failing event");
    assert_eq!(second.violations.len(), 8, "the entire violation record survives rearming");
    assert_eq!(first.violations.len(), 5, "an older publication remains immutable");
    for &(t, y) in &samples[..3 * 8] { intake.point(t, &[y as f32]); }
    intake.idle();
    let restarted = latest.lock().unwrap().clone();
    assert!(restarted.event_epoch > second.event_epoch, "backward time distinguishes a new timeline even with the same arm request");
    assert_eq!(restarted.stopped.map(|(_, v)| v.event), Some(0), "the toggle remains armed in the restarted timeline");
    assert_eq!(restarted.violations.len(), 2, "events and violations restart together");
    assert_eq!(second.violations.len(), 8, "restarting preserves older immutable snapshots");
}

use fab_elec_bench::{source::Intake, trigger::{EdgeTrigger, Slope, Window}};
use fab_elec_view::instruments::{
    acquire::{AcquireIntake, Acquisition, LaneSetup, Published, TrendSlot},
    distribution::Distribution,
    phosphor::PhosphorMap,
    trend::TrendQuantity,
};
use std::{sync::Arc, time::Duration};

fn distribution(values: impl IntoIterator<Item = f32>) -> Distribution {
    let mut result = Distribution::default();
    for value in values { result.push(value); }
    result
}

#[test]
fn shuffled_events_have_the_same_canonical_grid_and_counts() {
    let sorted: Vec<_> = (1..=1000).map(|i| i as f32).collect();
    let mut shuffled = sorted.clone();
    let mut state = 0x61c88647u32;
    for i in (1..shuffled.len()).rev() {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        shuffled.swap(i, state as usize % (i + 1));
    }
    let actual = distribution(shuffled);
    assert_eq!(actual.total, 1000, "every finite event is counted");
    assert_eq!(actual.counts.iter().sum::<u64>(), 1000, "rebucketing loses no events");
    assert_eq!(actual, distribution(sorted.iter().copied()), "arrival order cannot choose the grid or its counts");
    assert_eq!(actual, distribution(sorted.iter().rev().copied()), "repeated downward shifts keep the same grid");
    for value in sorted {
        let bin = ((f64::from(value) * 2.0_f64.powi(-actual.k)).floor() - actual.start) as usize;
        assert!(bin < 64, "value {value} is in the final window");
        let (lo, hi) = actual.bin_range(bin);
        assert!(lo <= f64::from(value) && f64::from(value) < hi, "value {value} is inside its half-open bin");
    }
    assert_eq!(actual.lo(), actual.bin_range(0).0, "the drawing origin is the first bin's lower edge");
}

#[test]
fn growth_merges_global_bins_without_count_loss_or_order_dependence() {
    for old in [(-31..=32).map(|v| v as f32).collect::<Vec<_>>(), (1..=64).map(|v| v as f32).collect()] {
        let before = distribution(old.iter().copied());
        let mut after = before.clone();
        after.push(512.0);
        assert!(after.k > before.k, "a distant value requires doubling");
        let mut expected = before.counts;
        let mut start = before.start;
        for _ in before.k..after.k {
            let next_start = (start * 0.5).floor();
            let mut merged = [0; 64];
            for (i, count) in expected.into_iter().enumerate() {
                let to = (((start + i as f64) * 0.5).floor() - next_start) as usize;
                merged[to] += count;
            }
            assert_eq!(merged.iter().sum::<u64>(), before.total, "each doubling conserves all old counts");
            expected = merged;
            start = next_start;
        }
        let added = ((512.0 * 2.0_f64.powi(-after.k)).floor() - after.start) as usize;
        expected[added] += 1;
        assert_eq!(after.counts, expected, "global bin j merges into floor(j / 2), including negative odd bins");
        assert_eq!(after.total, before.total + 1, "only the new value increases the total");
        assert_eq!(after, distribution([512.0].into_iter().chain(old.into_iter().rev())), "the outlier may arrive first");
    }
    assert_eq!(distribution([63.0, 64.0, 0.0]), distribution([0.0, 63.0, 64.0]), "an empty window tail cannot force an extra doubling");
    let extremes = [f32::MAX, -f32::MAX, f32::MIN_POSITIVE, f32::from_bits(1), -f32::from_bits(1), 0.0];
    assert_eq!(distribution(extremes), distribution(extremes.into_iter().rev()), "indices larger than i64 and subnormal values use the same canonical grid");
}

#[test]
fn nonfinite_values_are_counted_separately_and_never_enter_bins() {
    let finite = distribution([1.0, -2.0, 7.0]);
    let mixed = distribution([f32::NAN, 1.0, f32::INFINITY, -2.0, f32::NEG_INFINITY, 7.0]);
    assert_eq!(mixed.nonfinite, 3);
    assert_eq!(mixed.total, 3);
    assert_eq!(mixed.counts.iter().sum::<u64>(), 3);
    assert_eq!((mixed.k, mixed.start, mixed.counts), (finite.k, finite.start, finite.counts), "nonfinite values cannot widen or populate the grid");
    let empty = distribution([f32::NAN, f32::INFINITY]);
    assert_eq!((empty.k, empty.total, empty.nonfinite), (-40, 0, 2), "an unmeasured run keeps its initial grid");
}

fn square(intake: &mut dyn Intake, cycle: usize, runt: bool) {
    let width = if runt { 0.0002 } else { 0.0005 };
    for (i, (dt, y)) in [(0.0, 0.0), (0.0001, 0.0), (0.0002, 0.5), (0.0003, 1.0),
        (0.0001 + width, 1.0), (0.0002 + width, 0.5), (0.0003 + width, 0.0), (0.0009, 0.0)].into_iter().enumerate() {
        if runt && i == 4 { continue; }
        intake.point(cycle as f64 * 0.001 + dt, &[y]);
    }
}

#[test]
fn recorder_counts_completed_events_and_isolates_the_one_runt() {
    let window = Window { pre: 0.0001, post: 0.00115 };
    let lanes = vec![LaneSetup::time(PhosphorMap::screen(16, 6, window.pre, window.pre + window.post, -0.25, 1.25))];
    let trigger = EdgeTrigger { level: 0.5, hyst: 0.2, slope: Slope::Rising, holdoff: 0.0 };
    let pins = Arc::new(TrendSlot::default());
    assert!(pins.set(&[(0, TrendQuantity::Width)]));
    let mut snapshots = Vec::new();
    {
        let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes), Duration::MAX, |frame, stats| {
            snapshots.push((stats.events.unwrap(), stats.columns));
            Published::Taken(frame)
        });
        intake.record_events();
        intake.follow_trends(pins.clone());
        for cycle in 0..16 { square(&mut intake, cycle, cycle == 7); }
        for (dt, y) in [(0.0, 0.0), (0.0001, 0.0), (0.0002, 0.5), (0.0003, 1.0), (0.0004, 1.0)] {
            intake.point(0.016 + dt, &[y]);
        }
        intake.idle();
        intake.point(0.01645, &[1.0]);
        intake.idle();
        assert!(pins.set(&[]));
        intake.flush();
        assert!(pins.set(&[(0, TrendQuantity::Width)]));
        intake.flush();
        for cycle in 0..4 { square(&mut intake, cycle, false); }
        intake.idle();
    }
    let (events, series) = &snapshots[0];
    let column = &series[0];
    let histogram = &column.distribution;
    assert_eq!(events.len(), 17, "the last accepted trigger still has an open span");
    assert_eq!(column.values.len(), 16);
    assert_eq!(histogram.total, column.values.len() as u64, "only completed events are measured");
    assert_eq!(histogram.counts.iter().sum::<u64>(), 16, "every completed width is retained");
    assert_eq!(histogram.nonfinite, 0, "the trigger lane has a threshold from its first event");
    let runt = column.values.get(7).unwrap();
    assert!((runt - 0.0002).abs() < 1e-8);
    let lowest = histogram.counts.iter().position(|&count| count > 0).unwrap();
    let (lo, hi) = histogram.bin_range(lowest);
    assert!(lo <= f64::from(runt) && f64::from(runt) < hi, "the known runt occupies the lowest bin");
    assert_eq!(histogram.counts[lowest], 1, "the runt is alone in that bin");
    assert_eq!(*histogram, distribution((0..column.values.len()).map(|i| column.values.get(i).unwrap())), "publication covers exactly the retained event column");
    assert!(Arc::ptr_eq(series, &snapshots[1].1), "a publication with no newly completed event shares the histogram");
    assert!(snapshots[2].1.is_empty(), "removing the pin removes its distribution");
    assert_eq!(snapshots[3].1[0].distribution.total, 0, "repinning starts a new distribution without replaying the old prefix");
    let restarted = &snapshots.last().unwrap().1[0];
    assert_eq!((restarted.values.len(), restarted.distribution.total), (3, 3), "restart resets the column and its distribution together");
    assert_eq!(histogram.total, 16, "later resets cannot mutate a published distribution");
}

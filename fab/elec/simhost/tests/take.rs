//! The take keeps the whole run while older frames lose resolution, and an edit replaces only
//! what it reaches.

use fab_elec_sim::cosim::CoState;
use fab_elec_sim::live::GridFrame;
use fab_elec_simhost::take::{EnvSlot, Means, Take, CHANNEL_BYTES, CO_STATE_BYTES, SLOT_BYTES};
use std::sync::Arc;

const GRID: f64 = 0.25;

fn frame(k: i64, value: f32) -> GridFrame {
    GridFrame {
        k,
        since: k as f64 * GRID,
        t: (k + 1) as f64 * GRID,
        net_mean: vec![value, 7.0],
        net_min: vec![value - 2.0, 7.0],
        net_max: vec![value + 3.0, 7.0],
        pin_mean: vec![2.0 * value, 0.0],
        part_mean: vec![3.0 * value],
    }
}

fn close(actual: f32, expected: f32, property: &str) {
    assert!((actual - expected).abs() <= 1e-4 * expected.abs().max(1.0),
        "{property}: got {actual}, expected {expected}");
}

fn means(out: &Means, expected: f32, property: &str) {
    assert_eq!((out.net.len(), out.pin.len(), out.part.len()), (2, 2, 1), "{property}: row counts");
    close(out.net[0], expected, property);
    close(out.net[1], 7.0, property);
    close(out.pin[0], 2.0 * expected, property);
    close(out.pin[1], 0.0, property);
    close(out.part[0], 3.0 * expected, property);
}

#[test]
fn a_take_keeps_its_span_and_rebuilds_only_the_slots_the_render_reaches() {
    let mut frames: Vec<_> = (0..768).map(|k| frame(k, k as f32)).collect();
    frames[0].since = GRID / 2.0;
    let states: Vec<_> = (0..5).map(|k| CoState(Arc::new(k))).collect();
    let mut out = Means { net: vec![99.0; 8], pin: vec![], part: vec![99.0; 4] };
    let mut take = Take::new(usize::MAX);
    assert!(take.is_empty() && take.generation().is_none() && take.span().is_none()
        && take.finest_from().is_none() && take.bytes() == 0 && take.co_at(0.0).is_none(),
        "a new take has neither frames nor state");
    assert_eq!(take.means(0.0, 1.0, &mut out), None, "empty means have no time");
    assert_eq!(out.net, vec![99.0; 8], "an empty read leaves its output alone");
    for f in &frames[..4] { take.push(7, GRID, f); }
    assert_eq!(take.generation(), Some(7), "frames establish the numbering");
    assert_eq!(take.finest_from(), Some(GRID / 2.0), "a partial first frame starts the finest stretch");
    assert_eq!(take.means(0.1875, 0.625, &mut out), Some(0.75), "means return the slot holding b's end");
    means(&out, 8.0 / 7.0, "interval means weight the clipped overlaps of partial and whole slots");
    for (a, b, value, t) in [
        (0.5, 0.5, 1.0, 0.5),
        (1.0, 0.375, 1.0, 0.5),
        (-2.0, -1.0, 0.0, GRID),
        (2.0, 3.0, 3.0, 1.0),
    ] {
        assert_eq!(take.means(a, b, &mut out), Some(t), "a held, wrapped or out-of-span read returns its slot's time");
        means(&out, value, "held, wrapped and out-of-span reads use one slot");
    }

    let revision = take.revision();
    take.push(7, GRID, &frames[4]);
    assert_eq!(take.revision(), revision, "appending leaves kept slots and revision alone");
    let mut replacement = frame(1, 20.0);
    replacement.since += GRID / 4.0;
    take.push(7, GRID, &replacement);
    assert_eq!(take.revision(), revision + 1, "replacing a kept frame bumps revision once");
    take.means(0.45, 0.45, &mut out);
    means(&out, 20.0, "a kept frame is replaced");
    take.means(0.25, 0.625, &mut out);
    means(&out, 12.8, "replacement uses its new partial span");
    take.means(0.625, 0.625, &mut out);
    means(&out, 2.0, "slots ahead of a replacement keep the old render");

    let (mut constant, mut varying) = (Take::new(usize::MAX), Take::new(usize::MAX));
    for f in &frames[..64] {
        constant.push(7, GRID, &frame(f.k, 2.0));
        varying.push(7, GRID, f);
    }
    assert_eq!(constant.bytes(), 64 * SLOT_BYTES + 9 * CHANNEL_BYTES, "each of nine flat channels costs one value's place");
    assert_eq!(varying.bytes() - constant.bytes(), 5 * 4 * 64,
        "only the five varying channels expand to one value per slot");
    constant.push(7, GRID, &frame(0, 10.0));
    assert_eq!(constant.bytes(), varying.bytes(), "replacement promotes a flat channel without losing earlier slots");
    constant.means(GRID, GRID, &mut out);
    means(&out, 10.0, "the promoted channel keeps the replacement");
    constant.means(2.0 * GRID, 2.0 * GRID, &mut out);
    means(&out, 2.0, "promotion fills all other slots with the old flat value");
    for f in &frames[64..257] { varying.push(7, GRID, f); }
    assert_eq!(varying.bytes(), 256 * SLOT_BYTES + 9 * CHANNEL_BYTES + 5 * 4 * 256 + SLOT_BYTES + 9 * CHANNEL_BYTES,
        "frame 257 starts a new segment, whose channels each begin flat");
    assert_eq!(varying.means(frames[255].since, frames[256].t, &mut out), Some(frames[256].t),
        "an interval across a segment boundary returns the later slot's time");
    means(&out, 255.5, "interval means cross segment boundaries");

    let mut gaps = Take::new(usize::MAX);
    for &k in &[4, 8, 6, 0] { gaps.push(7, GRID, &frames[k]); }
    assert_eq!(gaps.span(), Some((frames[0].since, frames[8].t)), "front and gap insertion preserve time order");
    gaps.means(frames[4].t, frames[6].since, &mut out);
    means(&out, 6.0, "a no-overlap gap reads the next slot holding b");
    gaps.means(frames[8].t, frames[8].t, &mut out);
    means(&out, 8.0, "insertion keeps later segments intact");
    gaps.push(7, GRID, &frames[1]);
    gaps.means(frames[1].t, frames[1].t, &mut out);
    means(&out, 1.0, "rendering forward into a gap extends the preceding segment");

    let mut folded = Take::new(14_000 + 30 * CHANNEL_BYTES);
    for f in &frames {
        folded.push(7, GRID, f);
        assert_eq!(folded.span(), Some((frames[0].since, f.t)), "folding never drops any of the run's span");
    }
    assert!(folded.bytes() <= 14_000 + 30 * CHANNEL_BYTES, "a foldable run fits its byte budget");
    // a long run keeps few segments: folded neighbours join, so it fits a budget smaller than
    // its segments' own costs would be apart (256 of them)
    let budget = 64 * (SLOT_BYTES + 9 * CHANNEL_BYTES + 5 * 4) + 256 * (SLOT_BYTES + 5 * 4);
    let mut long = Take::new(budget);
    let many: Vec<_> = (0..65_536).map(|k| frame(k, k as f32)).collect();
    for f in &many { long.push(7, GRID, f); }
    assert!(256 * 9 * CHANNEL_BYTES > budget && long.bytes() <= budget, "a long run fits its budget: {} of {budget}", long.bytes());
    assert_eq!(long.span(), Some((many[0].since, many[65_535].t)), "a long folded run keeps its span");
    long.means(many[65_535].t, many[65_535].t, &mut out);
    means(&out, 65_535.0, "the newest frame stays whole");
    assert!(folded.finest_from().is_some_and(|t| t > frames[0].since), "folding moves full resolution later");
    let end = folded.means(0.0, 0.0, &mut out).expect("the oldest stretch is kept");
    let count = frames.partition_point(|f| f.t <= end);
    assert!(count > 1, "the oldest slot holds several original frames");
    let weight: f64 = frames[..count].iter().map(|f| f.t - f.since).sum();
    let expected = (frames[..count].iter().map(|f| f.net_mean[0] as f64 * (f.t - f.since)).sum::<f64>() / weight) as f32;
    means(&out, expected, "a folded slot keeps the span-weighted mean of its frames");
    assert_eq!(folded.net_extremes(0.0), Some((vec![-2.0, 7.0], vec![count as f32 + 2.0, 7.0])),
        "folding keeps every minimum and maximum, including flat extrema");
    folded.means(frames[0].since, end, &mut out);
    means(&out, expected, "an interval read of a folded slot agrees with its held read");

    let revision = folded.revision();
    for (i, f) in frames[..count].iter().enumerate() {
        folded.push(7, GRID, f);
        if i == 0 {
            assert_eq!(folded.means(f.t, f.t, &mut out), Some(f.t), "the first frame resets the folded slot's statistics span");
            means(&out, f.net_mean[0], "the first frame replaces the folded mean");
            assert_eq!(folded.net_extremes(f.t), Some((f.net_min.clone(), f.net_max.clone())),
                "the first frame resets old extrema too");
            folded.means(frames[767].t, frames[767].t, &mut out);
            means(&out, 767.0, "a folded re-render leaves the future intact");
        }
    }
    assert_eq!(folded.revision(), revision + count as u64, "every replace or merge into a folded slot bumps revision");
    assert_eq!(folded.means(0.0, 0.0, &mut out), Some(end), "re-rendering finds the folded slot by its fixed grid bounds");
    means(&out, expected, "re-rendering through a folded slot rebuilds its original means");
    assert_eq!(folded.net_extremes(0.0), Some((vec![-2.0, 7.0], vec![count as f32 + 2.0, 7.0])),
        "re-rendering through a folded slot rebuilds its extremes");

    // a budget that one fold meets: three slots, the five varying channels full
    let mut odd = Take::new(3 * SLOT_BYTES + 9 * CHANNEL_BYTES + 5 * 4 * 3);
    for f in &frames[..5] { odd.push(7, GRID, f); }
    assert_eq!(odd.finest_from(), None, "one segment can fold before it is full");
    assert_eq!(odd.span(), Some((frames[0].since, frames[4].t)), "an odd folded tail keeps its own span");
    odd.means(frames[4].t, frames[4].t, &mut out);
    means(&out, 4.0, "an odd folded slot holds just its unpaired frame");
    odd.push(7, GRID, &frame(4, 40.0));
    odd.means(frames[4].t, frames[4].t, &mut out);
    means(&out, 40.0, "the first frame of an odd folded slot replaces it");
    odd.push(7, GRID, &frames[5]);
    assert_eq!(odd.span(), Some((frames[0].since, frames[5].t)), "appending after an odd folded tail starts a new segment");

    take.push_co(0.125, &states[0]);
    let one_state = take.bytes();
    take.push_co(0.2, &states[1]);
    assert_eq!(take.bytes(), one_state, "two device states in one grid frame keep the first");
    take.push_co(0.375, &states[1]);
    take.push_co(0.625, &states[1]);
    assert_eq!(take.bytes(), one_state + CO_STATE_BYTES, "an unchanged Arc is kept once even in another frame");
    take.push_co(0.75, &states[2]);
    assert_eq!(take.co_at(-1.0), Some(&states[0]), "before all states, read the first");
    assert_eq!(take.co_at(0.25), Some(&states[0]), "a state read follows its preceding instant");
    assert_eq!(take.co_at(0.5), Some(&states[1]), "a changed state is visible at its time");
    assert_eq!(take.co_at(0.75), Some(&states[2]), "an exact device-state boundary selects that state");
    take.push_co(0.5, &states[3]);
    assert_eq!(take.co_at(10.0), Some(&states[3]), "a backward state drops all states after it");
    assert_eq!(take.bytes(), one_state + 2 * CO_STATE_BYTES, "rewinding updates nominal state bytes");
    take.push_co(0.3125, &states[0]);
    assert_eq!(take.bytes(), one_state, "a rewind drops later states before deduplicating the incoming Arc");
    let same_value = CoState(Arc::new(0));
    take.push_co(0.875, &same_value);
    assert_eq!(take.co_at(1.0), Some(&same_value), "equal payloads in different Arcs are distinct states");
    assert_eq!(take.bytes(), one_state + CO_STATE_BYTES, "a distinct Arc is charged as another state");

    let mut co_fold = Take::new(4 * CO_STATE_BYTES + 4_000);
    co_fold.push(7, GRID, &frames[0]);
    for (i, state) in states[..4].iter().enumerate() {
        co_fold.push_co((i as f64 + 0.75) * GRID, state);
    }
    for f in &frames[1..128] { co_fold.push(7, GRID, f); }
    assert!(co_fold.finest_from().is_some_and(|t| t > GRID), "state storage participates in the folding budget");
    assert_eq!(co_fold.co_at(0.499), Some(&states[0]), "a folded slot keeps its first device state");
    assert_eq!(co_fold.co_at(1.0), Some(&states[2]), "the next folded slot keeps its own first state");

    take.push(8, GRID, &frames[10]);
    assert_eq!(take.generation(), Some(8), "a new generation establishes a new numbering");
    assert_eq!(take.span(), Some((frames[10].since, frames[10].t)), "a new generation forgets the earlier frames");
    assert!(take.co_at(1.0).is_none(), "a new generation forgets the earlier device states");
    assert_eq!(take.bytes(), SLOT_BYTES + 9 * CHANNEL_BYTES, "a new generation resets the byte count");
    let mut shorter = frame(0, 5.0);
    shorter.t = GRID / 2.0;
    take.push(8, GRID / 2.0, &shorter);
    assert_eq!(take.span(), Some((0.0, GRID / 2.0)), "a different grid starts the take over");
    let mut fewer = frames[2].clone();
    fewer.since /= 2.0;
    fewer.t /= 2.0;
    fewer.pin_mean.pop();
    take.push(8, GRID / 2.0, &fewer);
    assert_eq!(take.span(), Some((fewer.since, fewer.t)), "different row counts start the take over");
    take.means(fewer.t, fewer.t, &mut out);
    assert_eq!(out.pin, fewer.pin_mean, "means resize for the new row counts");
    take.clear();
    assert!(take.is_empty() && take.generation().is_none() && take.span().is_none()
        && take.bytes() == 0 && take.finest_from().is_none(), "clear removes the complete take");

    let mut over = Take::new(0);
    for f in &frames[..8] { over.push(7, GRID, f); }
    assert_eq!(over.span(), Some((frames[0].since, frames[7].t)), "an irreducible take keeps going over budget without dropping span");
    assert!(over.bytes() > 0, "an impossible budget keeps the data");
}

#[test]
fn net_envelope_appends_the_kept_spans_after_the_cutoff() {
    let mut take = Take::new(usize::MAX);
    let expected = [
        EnvSlot { t0: 0.25, t1: 1.0, mean: 2.0, min: 1.0, max: 4.0 },
        EnvSlot { t0: 1.0, t1: 2.0, mean: 5.0, min: 3.0, max: 6.0 },
        EnvSlot { t0: 3.0, t1: 4.0, mean: 0.0, min: -1.0, max: 1.0 },
    ];
    for slot in expected {
        take.push(1, 1.0, &GridFrame {
            k: slot.t0 as i64, since: slot.t0, t: slot.t1,
            net_mean: vec![slot.mean, 7.0], net_min: vec![slot.min, 7.0], net_max: vec![slot.max, 7.0],
            ..GridFrame::default()
        });
    }
    let mut slots = Vec::new();
    take.net_envelope(0, f64::NEG_INFINITY, &mut slots);
    assert_eq!(slots, expected, "the varying net keeps its partial first span, extremes and gap");
    take.net_envelope(1, 1.0, &mut slots);
    let flat = |slot: EnvSlot| EnvSlot { mean: 7.0, min: 7.0, max: 7.0, ..slot };
    assert_eq!(&slots[3..], &[flat(expected[1]), flat(expected[2])], "the flat net appends only slots ending after the cutoff");
    slots.clear();
    take.net_envelope(0, 3.5, &mut slots);
    assert_eq!(slots, expected[2..], "a cutoff inside a slot returns its whole statistics span");
}

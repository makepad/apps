use fab_elec_instr::Signal;
use fab_elec_bench::trigger::{EdgeTrigger, Slope, Window};
use fab_elec_view::instruments::acquire::{Acquisition, LaneFrame, LaneSetup};
use fab_elec_view::instruments::feed::{FeedConfig, Member, Merger};
use fab_elec_view::instruments::phosphor::PhosphorMap;
use fab_elec_instr::prbs::Prbs;
use fab_elec_instr::synth::{Spread, Synth};
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

#[test]
fn members_feed_one_frame_at_a_time() {
    let synth = Synth { noise: 0.0, ..Synth::pwm() };
    let period = synth.period();
    let window = Window { pre: period / 2.0, post: period / 2.0 };
    let trigger = EdgeTrigger { level: (synth.lo + synth.hi) / 2.0, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
    let config = FeedConfig {
        members: 2,
        source: Signal::Synth(synth),
        spread: Spread::default(),
        record_periods: 5,
        trigger,
        window,
        auto: None,
        plot: (32, 24),
        volts: (-0.5, 3.8),
        frame_every: Duration::from_millis(8),
        pace: None,
    };
    let map = PhosphorMap::screen(config.plot.0, config.plot.1, window.pre, window.pre + window.post, config.volts.0, config.volts.1);
    let mut member = Member::new(&config, 0);
    let mut first = LaneFrame::single(map.w, map.h);
    member.run_for(member.record_len() * 4, &mut first);
    assert!((19..=20).contains(&first.lanes[0].waveforms), "twenty PWM periods must complete nineteen or twenty windows");

    let Signal::Synth(synth) = &config.source else { panic!("expected a PWM source") };
    let source = Synth { seed: synth.seed + 1, ..synth.clone() };
    let (times, values) = source.record(member.record_len());
    let mut acquisition = Acquisition::new(trigger, 0, window, vec![LaneSetup::time(map)]);
    let mut expected = LaneFrame::single(map.w, map.h);
    let span = config.record_periods as f64 * period;
    for pass in 0..4 {
        let offset = pass as f64 * span;
        for (&time, &value) in times.iter().zip(&values) {
            acquisition.push(offset + time, &[value], &mut expected.lanes);
        }
    }
    assert_eq!(first, expected, "member replay changed the acquisition counts or pixels");

    let mut other = Member::new(&config, 1);
    let mut second = LaneFrame::single(map.w, map.h);
    // Chunk boundaries must not change either the replay or the acquisition.
    for _ in 0..4 {
        other.run_for(other.record_len(), &mut second);
    }
    assert_eq!(second, expected, "noise-free members must agree regardless of chunk size");
    let mut sum = first.clone();
    sum.add(&second);
    let (mut merger, mailbox) = Merger::new(config.plot);
    let first = merger.add(first);
    let second = merger.add(second);
    assert!(merger.publish(), "the first merged frame was not published");

    let stale = merger.add(LaneFrame::single(map.w + 1, map.h));
    assert_eq!(merger.dropped(), 1, "a stale-size frame must be discarded and counted once");
    assert_eq!((stale.lanes[0].w, stale.lanes[0].h), (map.w + 1, map.h), "a stale member buffer must remain available for return");
    let first = merger.add(first);
    assert!(!merger.publish(), "the mailbox accepted a second ready frame");
    let second = merger.add(second);
    assert!(!merger.publish(), "a full mailbox must keep accumulating into the next frame");
    let ready = mailbox.try_recv().expect("the first frame disappeared");
    assert!(ready.published.is_some(), "a successful publication carries its transport timestamp");
    assert_eq!(merger.refused(), 2, "each full mailbox attempt is counted without dropping the pending sum");
    assert_eq!(merger.lane_waveforms(), &[2 * sum.lanes[0].waveforms], "rastered totals count summed input once, excluding stale sizes and publish retries");
    sum.published = ready.published;
    assert_eq!(ready, sum, "later frames changed the ready frame's pixels or waveform count");
    assert_eq!(mailbox.try_recv(), Err(TryRecvError::Empty), "the mailbox held more than one frame");
    merger.recycle(ready);
    assert!(merger.publish(), "the accumulated next frame was lost while the mailbox was full");
    let ready = mailbox.try_recv().expect("the next frame was not delivered");
    sum.published = ready.published;
    assert_eq!(ready, sum, "the next frame lost member pixels or waveforms");
    merger.recycle(ready);
    assert!(!merger.publish(), "a consumed sum was published twice");
    assert_eq!(merger.dropped(), 1, "a slow UI must not discard acquired data");

    // Exhaust reusable UI buffers without returning them; accumulation must survive.
    let first = merger.add(first);
    assert!(merger.publish());
    let held = mailbox.try_recv().unwrap();
    let first = merger.add(first);
    assert!(merger.publish());
    let held_too = mailbox.try_recv().unwrap();
    let _first = merger.add(first);
    let _second = merger.add(second);
    assert!(!merger.publish(), "publishing without spare storage must retain the sum");
    merger.recycle(held);
    assert!(merger.publish(), "returning a buffer did not resume publication");
    let ready = mailbox.try_recv().unwrap();
    sum.published = ready.published;
    assert_eq!(ready, sum, "running out of returned buffers lost acquired data");
    merger.recycle(held_too);
    merger.recycle(ready);
    assert_eq!(merger.dropped(), 1, "buffer starvation must not count as dropped data");

    let _spent = merger.add(expected);
    merger.set_plot((map.w + 1, map.h));
    assert_eq!(merger.dropped(), 2, "resizing must count the discarded pending sum");
    assert!(!merger.publish(), "resizing published an old-size pending frame");
    let _spent = merger.add(LaneFrame::single(map.w + 1, map.h));
    assert!(merger.publish(), "the merger did not accept the new size");
    let ready = mailbox.try_recv().unwrap();
    assert_eq!((ready.lanes[0].w, ready.lanes[0].h), (map.w + 1, map.h), "the resized mailbox retained old dimensions");
    assert_eq!(ready.lanes[0].waveforms, 0, "resizing retained an old waveform count");
    assert!(ready.lanes[0].data.iter().all(|&hit| hit == 0.0), "resizing retained old pixel counts");
}

#[test]
fn prbs_members_draw_an_open_eye() {
    let prbs = Prbs::gigabit();
    let ui = prbs.ui();
    let window = Window { pre: 0.5 * ui, post: 1.5 * ui };
    let trigger = EdgeTrigger { level: 0.4, hyst: 0.04, slope: Slope::Either, holdoff: 0.0 };
    let config = FeedConfig {
        members: 1,
        source: Signal::Prbs(prbs),
        spread: Spread::default(),
        record_periods: 2,
        trigger,
        window,
        auto: None,
        plot: (64, 48),
        volts: (-0.2, 1.0),
        frame_every: Duration::from_millis(8),
        pace: None,
    };
    let map = PhosphorMap::screen(config.plot.0, config.plot.1, window.pre, window.pre + window.post, config.volts.0, config.volts.1);
    let mut member = Member::new(&config, 0);
    let mut hits = LaneFrame::single(64, 48);
    // Four records: the picture re-arms after each window, so it draws one window per two-UI stretch.
    member.run_for(member.record_len() * 4, &mut hits);

    let rows_in_band = |lo: f64, hi: f64| -> Vec<u32> {
        (0..map.h)
            .filter(|&row| {
                let voltage = map.v_top - (row as f64 + 0.5) * map.v_px;
                (lo..=hi).contains(&voltage)
            })
            .collect()
    };
    let any_hit = |column: u32, rows: &[u32]| rows.iter().any(|&row| hits.lanes[0].at(column, row) > 0.0);
    let middle = rows_in_band(0.32, 0.48);

    assert!(hits.lanes[0].waveforms >= 300, "many waveforms: expected at least 300 complete windows, got {}", hits.lanes[0].waveforms);
    assert!(!any_hit(32, &middle), "open eye centre: the 0.32 V to 0.48 V band must have no hits");
    assert!(any_hit(32, &rows_in_band(0.8 - 0.15, 0.8 + 0.15)), "both rails lit: the centre must have hits near 0.8 V");
    assert!(any_hit(32, &rows_in_band(-0.15, 0.15)), "both rails lit: the centre must have hits near 0.0 V");
    assert!(any_hit(16, &middle), "crossing at trigger: column 16 must have hits in the 0.32 V to 0.48 V band");
}

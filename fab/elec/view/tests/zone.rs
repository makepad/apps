use fab_elec_bench::{source::Intake, trigger::{events, Edge, EdgeTrigger, Qualifier, Slope, Window}};
use fab_elec_view::instruments::{
    acquire::{AcquireIntake, AcquireStats, Acquisition, LaneFrame, LaneSetup, Published, TrendSlot, TriggerSlot, Zone},
    gesture::ZoneDrag,
    phosphor::PhosphorMap,
    scope::{ScopeAction, ScopeSettings},
};
use fab_elec_view::instruments::trend::TrendQuantity;
use makepad_widgets_core::{dvec2, Rect};
use std::{sync::{Arc, atomic::AtomicBool, mpsc::sync_channel}, time::Duration};

fn trigger(holdoff: f64) -> EdgeTrigger { EdgeTrigger { level: 0.3, hyst: 0.1, slope: Slope::Rising, holdoff } }
fn top(must: bool) -> Zone { Zone { lane: 0, t0: 0.0001, t1: 0.0003, v0: 0.9, v1: 1.1, must } }
fn window() -> Window { Window { pre: 0.0006, post: 0.0007 } }

fn acquisition(trigger: EdgeTrigger, window: Window, zone: Option<Zone>, lanes: usize) -> (Acquisition, LaneFrame) {
    let lanes = vec![LaneSetup::time(PhosphorMap::screen(24, 12, window.pre, window.pre + window.post, -0.1, 1.2)); lanes];
    let frame = LaneFrame::new(&lanes);
    let mut acquisition = Acquisition::new(trigger, 0, window, lanes);
    acquisition.set_zone(zone);
    (acquisition, frame)
}

// A 1 kHz square train with 10 us ramps, including history before its first rising edge.
fn pulses(tops: &[f32]) -> Vec<(f64, f32)> {
    let mut samples = vec![(-0.0006, 0.0), (-0.0004, 0.0), (-0.0002, 0.0)];
    for (i, &top) in tops.iter().enumerate() {
        for (dt, value) in [(0.0, 0.0), (0.00001, top), (0.0001, top), (0.0003, top), (0.0005, top), (0.00051, 0.0), (0.0009, 0.0)] {
            samples.push((i as f64 * 0.001 + dt, value));
        }
    }
    samples
}

fn edge_times(samples: &[(f64, f32)]) -> Vec<f64> {
    events(&samples.iter().map(|s| s.0).collect::<Vec<_>>(), &samples.iter().map(|s| s.1).collect::<Vec<_>>(), &trigger(0.0))
}

#[test]
fn normal_and_low_pulses_gate_strokes_strip_width_trends_and_captures() {
    let samples = pulses(&[1.0, 1.0, 1.0, 0.7, 1.0, 1.0, 1.0, 1.0]);
    let edges = edge_times(&samples);
    assert_eq!(edges.len(), 8, "the fixture has eight rising edges, including one low pulse");
    for must in [true, false] {
        let expected: Vec<_> = edges.iter().enumerate().filter(|&(i, _)| (i != 3) == must).map(|(_, &t)| t).collect();
        let (mut direct, mut frame) = acquisition(trigger(0.0), window(), Some(top(must)), 1);
        let mut accepted = Vec::new();
        for &(t, y) in &samples {
            if let Edge::Accepted(edge) = direct.push_frame(t, &[y], &mut frame) {
                assert!(t > edge + 0.0001, "acceptance waits for the box but keeps the original edge timestamp");
                accepted.push(edge);
            }
        }
        assert_eq!(accepted, expected, "must-cross accepts 7 of 8 pulses; must-not-cross accepts only the 1 low pulse");
        assert_eq!(direct.events(), expected.len() as u64, "event counts exclude zone rejections");
        assert_eq!(direct.zone_rejected(), (8 - expected.len()) as u64, "must-cross rejects 1 low pulse; must-not-cross rejects 7 normal pulses");
        assert_eq!(frame.waveforms(), expected.len() as u64, "only accepted strokes finish into the hits");
        assert_eq!(direct.open_strokes(), 0, "rejected strokes return to the pool unfinished");
        let (acquisition, _) = acquisition(trigger(0.0), window(), Some(top(must)), 1);
        let mut snapshots = Vec::<AcquireStats>::new();
        let pins = Arc::new(TrendSlot::default());
        assert!(pins.set(&[(0, TrendQuantity::Width)]));
        let request = Arc::new(AtomicBool::new(true));
        let (send, captures) = sync_channel(1);
        {
            let mut intake = AcquireIntake::new(acquisition, Duration::MAX, |frame, stats| {
                snapshots.push(stats);
                Published::Taken(frame)
            });
            intake.record_events();
            intake.follow_trends(pins);
            intake.capture(request, send, 1);
            for &(t, y) in &samples { intake.point(t, &[y]); }
            assert_eq!(intake.marks().as_slice(), expected.as_slice(), "accepted marks retain exactly the selected edge times");
            intake.flush();
        }
        let stats = snapshots.last().unwrap();
        let list = stats.events.as_ref().unwrap();
        assert_eq!((0..list.len()).map(|i| list.get(i).unwrap()).collect::<Vec<_>>(), expected, "the strip EventLog has exactly the zone-accepted edge timestamps");
        assert_eq!(stats.trigger_events, expected.len() as u64, "publication counts only zone-accepted triggers");
        assert_eq!(stats.columns.len(), 1);
        let width = &stats.columns[0].values;
        assert_eq!(width.len(), expected.len(), "one width value per accepted pulse, none for rejected pulses");
        for i in 0..width.len() {
            let value = width.get(i).unwrap();
            assert!(value > 0.00049 && value < 0.00051, "width {i} keeps the provisional rising edge through the zone wait: {value}");
        }
        let capture = captures.try_recv().expect("the first accepted pulse completes the requested capture");
        assert_eq!(capture.trigger, expected[0], "rejected pulses never consume the capture request");
        assert!((capture.samples.times[0] - (expected[0] - window().pre)).abs() < 1e-12, "the pending copy retains the full pre-trigger history through the zone wait");
        assert_eq!(*capture.samples.times.last().unwrap(), expected[0] + window().post);
    }
}

#[test]
fn negative_box_decides_from_history_even_after_late_confirmation() {
    let samples = pulses(&[1.0, 1.0, 0.7, 1.0]);
    let expected = edge_times(&samples);
    let zone = Zone { t0: -0.0004, t1: -0.0002, v0: -0.1, v1: 0.1, ..top(true) };
    let (mut acquisition, mut frame) = acquisition(trigger(0.0), window(), Some(zone), 1);
    let mut accepted = Vec::new();
    for &(t, y) in &samples {
        let verdict = acquisition.push_frame(t, &[y], &mut frame);
        if let Edge::Accepted(edge) = verdict {
            assert!((t - (edge / 0.001).floor() * 0.001 - 0.00001).abs() < 1e-12, "a wholly negative box decides on the detector's confirming sample");
            accepted.push(edge);
        }
    }
    assert_eq!(accepted, expected, "all four preceding low plateaus cross the negative box");

    let zone = Zone { lane: 1, t0: -0.4, t1: -0.2, v0: 0.9, v1: 1.1, must: true };
    let (mut late, mut frame) = self::acquisition(EdgeTrigger { hyst: 1.0, level: 0.0, ..trigger(0.0) }, Window { pre: 0.5, post: 0.5 }, Some(zone), 2);
    for (t, a, b) in [(0.0, -1.0, 0.0), (0.5, -0.1, 1.0), (0.75, -0.1, 1.0), (1.0, 0.1, 0.0), (2.0, 0.2, 0.0), (4.0, 0.3, 0.0)] {
        late.push_frame(t, &[a, b], &mut frame);
    }
    assert_eq!(late.pending(), [Some(0.875), None], "the unconfirmed crossing retains its side");
    assert_eq!(late.push_frame(5.0, &[1.0, 0.0], &mut frame), Edge::Accepted(0.875), "confirmation several windows later still sees the box at 0.475 to 0.675 seconds");
    assert_eq!(frame.waveforms(), 1, "a late decision precedes completion in the same push");
}

#[test]
fn holdoff_uses_only_accepted_events_and_busy_edges_do_not_replace_waiting() {
    let samples = pulses(&[1.0, 0.7, 1.0, 0.7, 1.0, 0.7, 1.0, 0.7, 1.0, 0.7]);
    let expected: Vec<_> = edge_times(&samples).into_iter().enumerate().filter(|(i, _)| i % 2 == 1).map(|(_, t)| t).collect();
    let (mut held, mut frame) = acquisition(trigger(0.0015), window(), Some(top(false)), 1);
    let mut accepted = Vec::new();
    for &(t, y) in &samples { if let Edge::Accepted(t) = held.push_frame(t, &[y], &mut frame) { accepted.push(t); } }
    assert_eq!(accepted, expected, "10 alternating pulses accept the 5 low pulses at indices 1, 3, 5, 7 and 9 with 1.5-period holdoff");
    assert_eq!((held.events(), held.held_off(), held.zone_rejected()), (5, 4, 1), "first normal pulse fails the zone; four later normal pulses fall within accepted-event holdoff");

    let samples = pulses(&[1.0; 4]);
    let edges = edge_times(&samples);
    let zone = Zone { t0: 0.0001, t1: 0.0013, v0: 1.5, v1: 2.0, ..top(false) };
    let (mut busy, mut frame) = acquisition(trigger(0.0), Window { pre: 0.0006, post: 0.0015 }, Some(zone), 1);
    let mut accepted = Vec::new();
    for &(t, y) in &samples {
        let verdict = busy.push_frame(t, &[y], &mut frame);
        if t > 0.001 && t < 0.0013 {
            assert_eq!(verdict, Edge::Pending(edges[0]), "the second confirmed edge cannot replace the waiting first edge");
            assert_eq!(busy.pending(), [Some(edges[0]), None], "capture and trend consumers keep the original rising-side copy");
        }
        if let Edge::Accepted(t) = verdict { accepted.push(t); }
    }
    assert_eq!(accepted, vec![edges[0], edges[2]], "a 1.3-period zone accepts 2 windows from 4 pulses");
    assert_eq!((busy.events(), busy.zone_busy(), busy.zone_rejected(), busy.held_off()), (2, 2, 0, 0), "the two intervening confirmations are counted only as zone_busy");
    assert_eq!(frame.waveforms(), 2);

    let (mut either, mut frame) = acquisition(EdgeTrigger { level: 0.5, hyst: 0.0, slope: Slope::Either, holdoff: 0.0 },
        Window { pre: 0.5, post: 0.5 }, Some(Zone { t0: 0.1, t1: 0.2, ..zone }), 1);
    either.push_frame(0.0, &[1.0], &mut frame);
    assert_eq!(either.push_frame(1.0, &[0.5], &mut frame), Edge::Pending(1.0));
    assert_eq!(either.pending(), [None, Some(1.0)], "a falling edge landing exactly on a zero-width band waits on its falling side");
    assert_eq!(either.push_frame(1.3, &[0.0], &mut frame), Edge::Accepted(1.0));
}

#[test]
fn segments_are_clipped_in_time_and_invalid_or_empty_boxes_are_ignored() {
    let trigger = EdgeTrigger { hyst: 0.0, ..trigger(0.0) };
    for (v0, v1, accepted) in [(8.0, 9.0, false), (4.4, 4.6, true)] {
        let zone = Zone { lane: 1, t0: 0.1, t1: 0.2, v0, v1, must: true };
        let (mut acquisition, mut frame) = acquisition(trigger, Window { pre: 0.1, post: 0.5 }, Some(zone), 2);
        acquisition.push_frame(0.0, &[0.0, 0.0], &mut frame);
        let verdict = acquisition.push_frame(1.0, &[1.0, 10.0], &mut frame);
        assert_eq!(verdict, if accepted { Edge::Accepted(0.3) } else { Edge::Dropped }, "only the clipped lane-1 segment at absolute t=0.4 to 0.5, values 4 to 5, intersects");
        assert_eq!(frame.waveforms(), u64::from(accepted));
    }
    for zone in [Zone { t0: f64::NAN, ..top(true) }, Zone { v1: 0.9, ..top(true) }, Zone { lane: 5, ..top(true) }, Zone { t0: 1.0, t1: 2.0, ..top(true) }] {
        let (mut acquisition, mut frame) = acquisition(trigger, Window { pre: 0.1, post: 0.5 }, Some(zone), 1);
        acquisition.push_frame(0.0, &[0.0], &mut frame);
        assert_eq!(acquisition.push_frame(1.0, &[1.0], &mut frame), Edge::Accepted(0.3), "invalid, out-of-range and empty clamped boxes leave ordinary edge detection intact");
    }
    let zone = Zone { t0: -10.0, t1: 10.0, v0: 2.0, v1: 3.0, ..top(false) };
    let (mut clipped, mut frame) = acquisition(trigger, Window { pre: 0.1, post: 0.5 }, Some(zone), 1);
    clipped.push_frame(0.0, &[0.0], &mut frame);
    assert_eq!(clipped.push_frame(1.0, &[1.0], &mut frame), Edge::Accepted(0.3), "a box extending beyond the capture decides at the clamped post boundary");
    assert_eq!(frame.waveforms(), 1);
}

#[test]
fn settings_slot_and_pointer_box_round_trip() {
    let zone = Zone { lane: 1, t0: -0.0002, ..top(false) };
    let mut settings = ScopeSettings::default();
    settings.channels.push(Default::default());
    let actions = settings.set_zone(Some(zone));
    assert_eq!(settings.trigger.zone, Some(zone));
    assert_eq!(actions, vec![ScopeAction::Reconfigure(settings.clone())], "a zone edit broadcasts the complete settings without clearing the picture");
    let saved = settings.clone();
    assert!(settings.set_zone(Some(Zone { t1: zone.t0, ..zone })).is_empty(), "empty boxes are refused");
    assert_eq!(settings, saved);
    let slot = TriggerSlot::default();
    slot.set(settings.trigger(), Qualifier::Width { min: 0.0, max: 0.001 }, settings.auto_after(), settings.trigger.zone);
    let (version, trigger, qualifier, auto, received) = slot.read(0).unwrap();
    assert_eq!((trigger, qualifier, auto, received), (settings.trigger(), Qualifier::Width { min: 0.0, max: 0.001 }, settings.auto_after(), Some(zone)), "all seven zone fields cross the same version bracket as trigger, qualifier and auto");
    assert!(slot.read(version).is_none());
    settings.set_zone(None);
    slot.set(settings.trigger(), Qualifier::None, None, settings.trigger.zone);
    assert_eq!(slot.read(version).unwrap().4, None, "removal publishes the absent flag");

    let drag = ZoneDrag { press: dvec2(20.0, 30.0), pointer: dvec2(60.0, 70.0), original: None, lane: 1,
        plot: Rect { pos: dvec2(10.0, 20.0), size: dvec2(100.0, 100.0) }, window: Window { pre: 0.001, post: 0.001 }, values: (0.0, 2.0) };
    let box_ = drag.finish().unwrap();
    assert_eq!(box_.lane, 1);
    assert!((box_.t0 + 0.0008).abs() < 1e-12 && box_.t1.abs() < 1e-12 && (box_.v0 - 1.0).abs() < 1e-12 && (box_.v1 - 1.8).abs() < 1e-12, "screen bounds become trigger-relative seconds and ascending lane values");
    assert!(ZoneDrag { pointer: drag.press + dvec2(3.9, 40.0), ..drag }.finish().is_none(), "a box narrower than 4 px is ignored");
    assert!(ZoneDrag { pointer: drag.press + dvec2(40.0, 3.9), ..drag }.finish().is_none(), "a box shorter than 4 px is ignored");
    assert_eq!(ZoneDrag { pointer: drag.press, original: Some(box_), ..drag }.finish(), Some(Zone { must: false, ..box_ }), "pressing an existing box flips its polarity");
    assert!(ZoneDrag { pointer: dvec2(40.0, 121.0), original: Some(box_), ..drag }.finish().is_none(), "dragging off the lane removes the box like a bracket");
}

#[test]
fn edits_restarts_and_stream_breaks_discard_undecided_strokes() {
    let (mut acquisition, mut frame) = acquisition(trigger(0.0), window(), Some(top(true)), 1);
    acquisition.push_frame(0.0, &[0.0], &mut frame);
    assert!(matches!(acquisition.push_frame(0.00001, &[1.0], &mut frame), Edge::Pending(_)));
    assert_eq!(acquisition.open_strokes(), 1, "the waiting event owns only one raster");
    acquisition.set_trigger(trigger(0.0));
    assert_eq!(acquisition.pending(), [None; 2], "set_trigger clears a waiting zone");
    assert_eq!(acquisition.open_strokes(), 0);
    acquisition.push_frame(0.0009, &[0.0], &mut frame);
    assert!(matches!(acquisition.push_frame(0.001, &[1.0], &mut frame), Edge::Pending(_)));
    acquisition.restart();
    assert_eq!(acquisition.pending(), [None; 2], "restart clears a waiting zone");
    assert_eq!(acquisition.open_strokes(), 0);
    acquisition.push_frame(0.0, &[0.0], &mut frame);
    acquisition.push_frame(0.00001, &[1.0], &mut frame);
    acquisition.push_frame(0.00002, &[f32::NAN], &mut frame);
    assert_eq!(acquisition.pending(), [None; 2], "a broken trigger stream clears its waiting event");
    assert_eq!(acquisition.open_strokes(), 0);
    assert_eq!(frame.waveforms(), 0, "none of the interrupted strokes reached the hits");

    let slot = Arc::new(TriggerSlot::default());
    slot.set(trigger(0.0), Qualifier::None, None, Some(top(false)));
    let (acquisition, _) = self::acquisition(trigger(0.0), window(), None, 1);
    let mut intake = AcquireIntake::new(acquisition, Duration::MAX, |frame, _| Published::Taken(frame));
    intake.follow_trigger(slot.clone());
    for (t, y) in pulses(&[1.0]) { intake.point(t, &[y]); }
    assert_eq!((intake.acquisition().events(), intake.acquisition().zone_rejected()), (0, 1), "a running intake takes the box from TriggerSlot");
    slot.set(trigger(0.0), Qualifier::None, None, None);
    intake.follow_trigger(slot);
    intake.point(0.001, &[0.0]);
    intake.point(0.00101, &[1.0]);
    assert_eq!(intake.acquisition().events(), 1, "clearing the slot restores ordinary edge acceptance");
}

#[test]
fn hardware_timestamps_pass_the_same_zone_before_publication() {
    use fab_elec_bench::source::{Block, ChanData, Clipped};
    let window = Window { pre: 0.0001, post: 0.0007 };
    for must in [true, false] {
        let (acquisition, _) = acquisition(trigger(0.0), window, Some(top(must)), 1);
        let mut snapshots = Vec::new();
        {
            let mut intake = AcquireIntake::new(acquisition, Duration::MAX, |frame, stats| {
                snapshots.push((frame.waveforms(), stats.trigger_events));
                Published::Taken(frame)
            });
            let y = [0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0];
            intake.block(&Block { index: 0, t0: 0.0, dt: 0.0001, chans: &[ChanData::F32(&y)], clipped: &[Clipped::default()], trigger: Some(0.00013) });
            intake.flush();
        }
        assert_eq!(snapshots, vec![(u64::from(must), u64::from(must))], "hardware openings must also wait: crossing accepts one stroke, not-crossing rejects it before publication");
    }
}

#[test]
fn hidden_zone_lanes_keep_sample_based_decisions_in_single_and_multi_lane_paths() {
    let samples = pulses(&[1.0, 0.7, 1.0]);
    for count in [1, 2] {
        for must in [true, false] {
            let window = window();
            let zone = Zone { lane: count - 1, ..top(must) };
            let (mut baseline, mut expected) = acquisition(trigger(0.0), window, Some(zone), count);
            let lanes = (0..count).map(|lane| LaneSetup { drawn: lane != zone.lane,
                ..LaneSetup::time(PhosphorMap::screen(24, 12, window.pre, window.pre + window.post, -0.1, 1.2)) }).collect::<Vec<_>>();
            let mut hidden = Acquisition::new(trigger(0.0), 0, window, lanes.clone());
            hidden.set_zone(Some(zone));
            let mut frame = LaneFrame::new(&lanes);
            for &(t, y) in &samples {
                let values = vec![y; count];
                assert_eq!(hidden.push_frame(t, &values, &mut frame), baseline.push_frame(t, &values, &mut expected),
                    "zone decisions must use samples even when the zone lane has no raster");
            }
            assert_eq!(hidden.events(), baseline.events(), "hiding the zone lane must preserve accepted triggers");
            assert_eq!(hidden.zone_rejected(), baseline.zone_rejected(), "hiding the zone lane must preserve rejected triggers");
            assert_eq!(frame.lanes[zone.lane].waveforms, 0, "a hidden zone lane must never finish a time raster");
            assert!(frame.lanes[zone.lane].dirty.is_empty(), "zone evaluation must not dirty the hidden lane's picture");
            if count == 2 { assert_eq!(frame.lanes[0], expected.lanes[0], "the visible lane must draw every zone-accepted capture"); }
        }
    }
}

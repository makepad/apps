//! Lane layout, in-place acquisition, answer selection and independent frame delivery.

use fab_elec_bench::{source::Intake, trigger::{EdgeTrigger, Slope, Window}};
use fab_elec_view::instruments::{
    acquire::{AcquireIntake, AcquireStats, Acquisition, LaneFrame, LaneSetup, LaneSlot, Published},
    bench::{bench_time_per_div, new_lane_height, BenchProbe, LaneStack},
    feed::{FeedStats, Merger, ScopeFeed},
    phosphor::{Hits, PhosphorMap},
    phosphor_view::Pushed,
    scope::{lane_answers, resize, Domain, FftSettings, Measurement, ResizeOutcome},
    scope_host::ScopeHost,
};
use std::{collections::VecDeque, sync::{mpsc::Receiver, Arc, Mutex}, time::Duration};

#[test]
fn lane_bottom_edge_clamps_snaps_and_reopens() {
    for (current, dy, height, collapse) in [
        (160.0, 80.0, 240.0, false),
        (160.0, -104.0, 56.0, false),
        (160.0, -110.0, 56.0, false),
        (56.0, -16.0, 56.0, false),
        (56.0, -16.01, 56.0, true),
    ] {
        assert_eq!(resize(current, dy, false), ResizeOutcome { height, collapse }, "the signal minimum holds until the pointer is more than 16 pt below it");
    }
    for dy in [-100.0, 0.0, 16.0] {
        assert_eq!(resize(28.0, dy, true), ResizeOutcome { height: 56.0, collapse: true }, "a meter waits for a downward drag greater than 16 pt");
    }
    let opened = resize(28.0, 17.0, true);
    assert_eq!(opened, ResizeOutcome { height: 56.0, collapse: false }, "a meter opens at exactly the signal minimum");
    assert_eq!(resize(opened.height, 24.0, opened.collapse), ResizeOutcome { height: 80.0, collapse: false }, "movement after reopening continues from the new minimum");
    assert_eq!(resize(160.0, 0.0, true), ResizeOutcome { height: 160.0, collapse: true }, "pressing a meter preserves its stored expanded height");
    assert_eq!(resize(160.0, 17.0, true), opened, "reopening starts at 56 even when the meter retained a taller height");
    assert_eq!(resize(160.0, 10000.0, false).height, 10160.0, "the stack scrolls instead of capping lane height");
}

#[test]
fn a_benchs_probe_statements_are_its_default_cards() {
    let mut sources = fab_elec_fixtures::docs("ledctl").expect("stock documents");
    sources.add("cards.bench.splash", r#"bench("cards", {circuit: "ledctl"}, fn(b, c) {
    b.probe(c.lamps[6].d.K, {label: "LED6 chase"})
    b.probe(b.v(c.net("3V3"), c.net("GND")), {label: "Supply difference"})
    b.probe(c.lamps[2].d.K, {label: "LED2 PWM"})
    b.probe(b.i(c.mcu.u.PA0))
    b.probe(c.net("3V3"), {label: "Rail"})
})
"#).expect("bench document");
    let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).expect("the documents evaluate");
    let bench = evaluated.benches.iter().find(|bench| bench.name == "cards").expect("the bench evaluates");
    assert_eq!(bench.state, fab_elec_lang::UnitState::Built, "the bench builds");
    let (cards, skipped) = BenchProbe::from_bench(&bench.probes);
    let card = |net: &str, label: &str| BenchProbe { net: net.into(), label: label.into() };
    assert_eq!(cards, vec![card("LED6", "LED6 chase"), card("LED2", "LED2 PWM"), card("3V3", "Rail")], "cards: one per voltage of a net, in call order, under its label");
    assert_eq!(skipped, 2, "cards: a difference and a current have no card yet");
    let circuit = bench.circuit(&evaluated.design).expect("the bench's circuit");
    assert!(cards.iter().all(|card| circuit.nets.iter().any(|net| net.name == card.net)), "cards: each names a net of the circuit the simulation runs");

    let sources = fab_elec_fixtures::docs("ledavr").expect("ledavr's documents");
    let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).expect("ledavr evaluates");
    let chase = evaluated.benches.iter().find(|bench| bench.name == "chase").expect("ledavr's chase bench");
    assert_eq!(chase.state, fab_elec_lang::UnitState::Built, "ledavr: its bench builds");
    let (cards, skipped) = BenchProbe::from_bench(&chase.probes);
    assert_eq!(cards.iter().map(|card| card.label.as_str()).collect::<Vec<_>>(),
        ["D1 PD6", "D2 PD5", "D3 PB1", "D4 PB2", "D5 PB3", "D6 PD3", "D7 PD4", "D8 PD7", "Button"],
        "ledavr: its nine probe statements (each LED's cathode, then the button) are its cards, in call order");
    assert_eq!(cards.iter().map(|card| card.net.as_str()).collect::<Vec<_>>(),
        ["LED0", "LED1", "LED2", "LED3", "LED4", "LED5", "LED6", "LED7", "BTN"], "ledavr: the cards read the eight LED nets and the button");
    // The cards open on the bench's scenario: its 200 ms fill ten 20 ms divisions, where the
    // default (a bench without a stop) shows 200 us; nine lanes share the height of four.
    assert_eq!(bench_time_per_div(chase.stop), 0.02, "ledavr: the chase's 200 ms run sets 20 ms per division");
    assert_eq!(bench_time_per_div(None), 20e-6, "a bench without a run length keeps the default timebase");
    assert_eq!((new_lane_height(3), new_lane_height(4)), (160.0, 160.0), "up to four lanes open at the default height");
    assert!((new_lane_height(9) - 160.0 * 4.0 / 9.0).abs() < 1e-9 && new_lane_height(40) == 56.0,
        "more lanes share four lanes' height, never below the minimum");
    assert_eq!(skipped, 0, "ledavr: every probe reads a net's voltage");
    let circuit = chase.circuit(&evaluated.design).expect("ledavr's circuit");
    assert!(cards.iter().all(|card| circuit.nets.iter().any(|net| net.name == card.net)), "ledavr: each card names a net of its circuit");
}

#[test]
fn layout_membership_and_display_order_are_independent() {
    let probe = |net: &str| BenchProbe { net: net.into(), label: net.into() };
    let mut stack = LaneStack::default();
    assert!(stack.sync(&[probe("a"), probe("b"), probe("c")]), "initial probes create a channel layout");
    assert_eq!(stack.channels(), &["a", "b", "c"], "channel indices follow the initial probe list");
    let installed = stack.generation();
    assert!(stack.reorder("c", 0), "a vertical tag move changes display order");
    assert_eq!(stack.display(), &["c", "a", "b"], "the carried tag previews its destination");
    assert_eq!(stack.channels(), &["a", "b", "c"], "reorder preserves channel indices");
    assert_eq!(stack.generation(), installed, "reorder preserves the installed feed generation");
    assert!(!stack.sync(&[probe("b"), probe("c"), probe("a")]), "an authoritative reorder also keeps the feed");
    assert_eq!(stack.channels(), &["a", "b", "c"], "authoritative display order is independent of channels");
    assert_eq!(stack.display(), &["b", "c", "a"], "authoritative order reaches the display");
    assert!(stack.sync(&[probe("b"), probe("c"), probe("a"), probe("d")]), "adding a probe creates a new layout");
    assert_eq!(stack.channels(), &["b", "c", "a", "d"], "new membership follows the new probe list");
    assert_eq!(stack.generation(), installed + 1, "one add advances the feed generation once");
    assert!(stack.sync(&[probe("b"), probe("a"), probe("d")]), "removing a probe creates a new layout");
    assert_eq!(stack.channels(), &["b", "a", "d"], "removal excludes the old channel");
    assert_eq!(stack.generation(), installed + 2, "one remove advances the feed generation once");
    assert!(!stack.reorder("missing", 0), "an absent tag cannot reorder the layout");
}

/// A probe's hue is its own: removing or adding a probe recolours no other, a new probe takes the
/// lowest hue no probe wears, and past the palette the hue fewest wear.
#[test]
fn a_probes_hue_stays_with_it_through_removals_and_additions() {
    let probe = |net: &str| BenchProbe { net: net.into(), label: net.into() };
    let mut stack = LaneStack::default();
    stack.sync(&[probe("a"), probe("b"), probe("c"), probe("d")]);
    assert_eq!(stack.hues(), &[0, 1, 2, 3], "first probes take the hues in order");
    stack.sync(&[probe("a"), probe("c"), probe("d")]);
    assert_eq!(stack.hues(), &[0, 2, 3], "removing b recolours none of the others");
    assert_eq!(stack.next_hue(), 1, "the hue b wore is free again");
    stack.sync(&[probe("e"), probe("a"), probe("c"), probe("d")]);
    assert_eq!((stack.hue_of("e"), stack.hue_of("a"), stack.hue_of("c")), (Some(1), Some(0), Some(2)),
        "a probe added first takes the free hue; the others keep theirs");
    let many: Vec<BenchProbe> = (0..10).map(|i| probe(&format!("n{i}"))).collect();
    stack.sync(&many);
    assert_eq!(&stack.hues()[..], &[0, 1, 2, 3, 4, 5, 6, 7, 0, 1], "past eight probes the hues repeat, fewest worn first");
    stack.sync(&many[1..]);
    assert_eq!(stack.hue_of("n9"), Some(1), "and stay with their probes too");
    assert_eq!(stack.next_hue(), 0, "the hue now worn least is n0's");
}

#[test]
fn overlays_preserve_channels_order_and_surviving_probes() {
    let probe = |net: &str| BenchProbe { net: net.into(), label: net.into() };
    let mut stack = LaneStack::default();
    stack.sync(&[probe("a"), probe("b"), probe("c"), probe("d")]);
    stack.reorder("c", 0);
    let generation = stack.generation();
    let display = stack.display().to_vec();
    assert!(!stack.overlay("a", "a"), "a net cannot overlay itself");
    assert!(!stack.overlay("absent", "a"), "an unknown guest is refused");
    assert!(!stack.overlay("a", "absent"), "an unknown host is refused");
    assert!(stack.overlay("c", "a"), "two unpaired probes can share a lane");
    assert_eq!(stack.host_of("c"), Some("a"), "guest lookup identifies its shared map");
    assert_eq!(stack.guest_of("a"), Some("c"), "host lookup identifies its second source");
    assert_eq!(stack.host_of("a"), None, "the host remains a displayed lane");
    for (guest, host) in [("b", "a"), ("b", "c"), ("a", "b"), ("c", "b"), ("c", "a")] {
        assert!(!stack.overlay(guest, host), "neither member of a pair can join another pair");
    }
    assert!(stack.overlay("d", "b"), "independent pairs can coexist");
    assert!(!stack.separate("absent"), "separating an unknown probe is refused");
    assert!(stack.separate("a"), "separating the host releases both probes");
    assert_eq!(stack.host_of("c"), None, "the separated guest is visible again");
    assert_eq!(stack.guest_of("a"), None, "the separated host becomes plain");
    assert!(!stack.separate("c"), "an already plain probe cannot separate again");
    assert!(stack.separate("d"), "separating the guest also releases its host");
    assert_eq!(stack.display(), display, "separation restores every probe at its retained display position");
    assert_eq!(stack.channels(), &["a", "b", "c", "d"], "overlays and separation preserve solver channel indices");
    assert_eq!(stack.generation(), generation, "overlays and separation never restart the feed");
    assert!(stack.overlay("c", "a"), "a separated probe can be overlaid again");
    assert!(stack.overlay("d", "b"), "the other pair can also be restored");
    stack.sync(&[probe("a"), probe("b"), probe("d")]);
    assert_eq!(stack.guest_of("a"), None, "removing a guest drops its pair");
    assert_eq!(stack.host_of("d"), Some("b"), "sync preserves an unrelated complete pair");
    stack.sync(&[probe("a"), probe("d")]);
    assert_eq!(stack.host_of("d"), None, "removing a host releases the surviving guest");
    assert_eq!(stack.display(), &["a", "d"], "sync keeps the surviving probes in the stack");
}

#[test]
fn lane_slot_retunes_value_windows_without_disturbing_open_strokes() {
    let window = Window { pre: 0.25, post: 0.75 };
    let trigger = EdgeTrigger { level: 0.0, hyst: 0.0, slope: Slope::Rising, holdoff: 0.0 };
    let original = vec![LaneSetup::time(PhosphorMap::screen(32, 16, 0.25, 1.0, -1.0, 1.0)); 2];
    let mut retuned = original.clone();
    retuned[1].map = PhosphorMap::screen(32, 16, 0.25, 1.0, -2.0, 2.0);
    let slot = Arc::new(LaneSlot::new(original.clone()));
    type Publisher = Box<dyn FnMut(LaneFrame, AcquireStats) -> Published + Send>;
    let make = |lanes: Vec<LaneSetup>| {
        let frames = Arc::new(Mutex::new(Vec::<LaneFrame>::new()));
        let out = frames.clone();
        let spare = lanes.clone();
        let publish: Publisher = Box::new(move |frame, _| { out.lock().unwrap().push(frame); Published::Taken(LaneFrame::new(&spare)) });
        (AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes), Duration::MAX, publish), frames)
    };
    let (mut baseline, before) = make(original.clone());
    let (mut changed, after) = make(original.clone());
    let (mut new_map, expected) = make(retuned.clone());
    changed.follow_lanes(slot.clone());
    for i in 0..=280 {
        if i == 40 {
            assert!(slot.set(retuned.clone()), "a value-only lane edit is accepted in place");
            let mut resized = retuned.clone();
            resized[1].map.w += 1;
            assert!(!slot.set(resized), "pixel-size edits are refused by the running lane slot");
            let mut spectrum = retuned.clone();
            spectrum[1].spectrum = Some((FftSettings::default(), (32, 16)));
            assert!(!slot.set(spectrum), "spectrum edits require a new feed");
            let mut time = retuned.clone();
            time[1].map.t_px *= 2.0;
            assert!(!slot.set(time), "shared time edits require a new feed");
        }
        let t = i as f64 / 64.0;
        // The next edge is sample 104, exactly 64 samples after publication of the edit.
        let y = [if (t - 0.5).rem_euclid(1.125) < 0.5625 { 0.75 } else { -0.75 }, 0.5];
        baseline.point(t, &y);
        changed.point(t, &y);
        new_map.point(t, &y);
        if i == 90 { baseline.idle(); changed.idle(); new_map.idle(); }
    }
    baseline.idle(); changed.idle(); new_map.idle();
    let before = before.lock().unwrap();
    let after = after.lock().unwrap();
    let expected = expected.lock().unwrap();
    assert_eq!(after.len(), 2, "two explicit publication intervals retain their envelopes");
    assert_eq!(after[0].lanes[0].waveforms, 1, "lane 0's open window completes across the retune");
    assert_eq!(after[0].lanes, before[0].lanes, "every open window finishes on its original axes");
    assert_eq!(after[1].lanes[0], before[1].lanes[0], "lane 0's subsequent hits are bit-for-bit unchanged");
    assert_eq!(after[1].lanes[1].waveforms, 2, "lane 1 continues acquiring without restarting its detector");
    assert_eq!(after[1].lanes[1], expected[1].lanes[1], "lane 1's new windows use the new value map within 64 samples");
    assert_ne!(after[1].lanes[1].data, before[1].lanes[1].data, "retuning changes the affected lane's raster rows");
}

#[test]
fn answers_choose_edge_dc_fft_and_empty_rows_per_lane() {
    let row = |lane, name: &str, value, unit: &str| -> Measurement { Measurement::exact(lane, name, value, unit) };
    let rows = vec![
        row(0, "mean", 1.65, "V"), row(0, "freq", 10_000.0, "Hz"), row(0, "duty", 50.0, "%"), row(0, "Vpp", 3.3, "V"), row(0, "rise", 2e-9, "s"), row(0, "overshoot", 0.0, "%"),
        row(1, "mean", 3.299, "V"), row(1, "Vpp", 0.002, "V"),
        row(2, "peak f", 1000.0, "Hz"), row(2, "peak", -6.0, "dBV"), row(2, "2nd f", 2000.0, "Hz"), row(2, "2nd", -40.0, "dBV"), row(2, "RBW", 100.0, "Hz"),
    ];
    let edge = lane_answers(&rows, &[], 0, Domain::Time, false, false, false);
    assert_eq!(edge.items.len(), 4, "edge answers select frequency, duty, swing and rise only");
    assert_eq!(edge.items.iter().map(|a| a.unit.as_str()).collect::<Vec<_>>(), ["kHz", "% high", "V pp", "ns"], "edge answers carry engineering units and high duty");
    assert_eq!(edge.items[3].name, "rise ", "rise remains identified beside its value");
    let dc = lane_answers(&rows, &[], 1, Domain::Time, false, false, false);
    assert_eq!(dc.items[0].value, "3.299", "answers retain four significant digits of the rail mean");
    assert_eq!(dc.items.len(), 2, "DC answers select mean and ripple only");
    assert_eq!(dc.items[1].name, "ripple ", "DC peak-to-peak is named ripple");
    assert_eq!(dc.items[1].unit, "mV pp", "DC ripple uses its own SI prefix");
    let meter = lane_answers(&rows, &[], 0, Domain::Time, true, false, false);
    assert_eq!(meter.items.len(), 2, "a collapsed edge lane answers mean and ripple");
    let fft = lane_answers(&rows, &[], 2, Domain::Fft, false, false, false);
    assert_eq!(fft.items.len(), 4, "FFT answers select both resolved peaks and omit RBW");
    assert_eq!(fft.items[1].unit, "dBV", "FFT amplitude retains dBV units");
    assert_eq!(fft.items[2].name, "2nd ", "the second resolved peak is identified");
    let mut combined = rows.iter().filter(|row| row.lane == 0).cloned().collect::<Vec<_>>();
    combined.extend(rows.iter().filter(|row| row.lane == 2).cloned().map(|mut row| { row.lane = 0; row }));
    let lens = lane_answers(&combined, &[], 0, Domain::Fft, false, false, false);
    assert_eq!(&lens.items[..edge.items.len()], edge.items.as_slice(), "the lens keeps every time answer in order");
    assert_eq!(&lens.items[edge.items.len()..], fft.items.as_slice(), "the spectral suffix reuses the existing answer formatting");
    assert_eq!(lane_answers(&combined, &[], 0, Domain::Fft, true, false, false), meter, "a collapsed lens keeps only the meter answers");
    let low_duty = lane_answers(&[row(0, "freq", 1000.0, "Hz"), row(0, "duty", 0.25, "%")], &[], 0, Domain::Time, false, false, false);
    assert_eq!(low_duty.items[1].value, "0.25", "small duty percentages are not rescaled into a misleading percent value");
    let low_db = lane_answers(&[row(0, "peak", -0.125, "dBV")], &[], 0, Domain::Fft, false, false, false);
    assert_eq!(low_db.items[0].unit, "dBV", "small logarithmic amplitudes do not acquire SI prefixes");
    let none = lane_answers(&rows, &[], 3, Domain::Time, false, true, false);
    assert!(none.items.is_empty(), "a missing lane never borrows another lane's numbers");
    let held = lane_answers(&[row(1, "mean", 3.3, "V"), row(1, "Vpp", 0.0, "V"), row(1, "held", 1.0, "")], &[], 1, Domain::Time, false, false, false);
    assert_eq!(held.items.last().map(|a| a.name.as_str()), Some("held"), "a held window says held after its answers");
    assert!(held.text.ends_with("held"), "the accessible answer text says held");
    let empty_held = lane_answers(&[row(1, "held", 1.0, "")], &[], 1, Domain::Time, false, true, false);
    assert!(empty_held.items.is_empty(), "held alone is no answer: a lane without numbers still waits");
    assert_eq!(none.text, "waiting for a trigger", "normal acquisition names the waiting state");
    assert_eq!(lane_answers(&[], &[], 0, Domain::Time, false, true, true).text, "no samples at this instant", "scrub absence takes precedence over trigger mode");
}

#[derive(Default)]
struct Feed { frames: VecDeque<LaneFrame>, mailbox: Option<Receiver<LaneFrame>>, recycle: Option<Arc<Mutex<Merger>>>, returned: Vec<LaneFrame>, takes: usize }
impl ScopeFeed for Feed {
    fn take(&mut self) -> Option<LaneFrame> { self.takes += 1; if let Some(mailbox) = &self.mailbox { mailbox.try_recv().ok() } else { self.frames.pop_front() } }
    fn give_back(&mut self, frame: LaneFrame) { if let Some(merger) = &self.recycle { merger.lock().unwrap().recycle(frame); } else { self.returned.push(frame); } }
    fn reconfigure(&mut self, _: EdgeTrigger, _: fab_elec_bench::trigger::Qualifier, _: usize, _: Window, _: Vec<LaneSetup>, _: Option<f64>, _: Option<fab_elec_view::instruments::acquire::Zone>) {}
    fn stats(&self) -> FeedStats { FeedStats::default() }
    fn stop(&mut self) {}
}

#[test]
fn host_splits_lanes_and_gates_takes_until_busy_pictures_are_accepted_or_hidden() {
    let frame = |waveforms| {
        let mut frame = LaneFrame::single(2, 2);
        frame.lanes.resize_with(3, || Hits::new(2, 2));
        frame.spectra.resize_with(3, || None);
        let mut spectrum = Hits::new(3, 1);
        spectrum.waveforms = waveforms;
        spectrum.data[1] = waveforms as f32;
        spectrum.mark(0, 3, 0, 1);
        frame.spectra[0] = Some(spectrum);
        for (i, hits) in frame.lanes.iter_mut().enumerate() {
            hits.waveforms = waveforms;
            hits.data[i] = waveforms as f32;
            hits.mark(0, 2, 0, 2);
        }
        frame
    };
    let mut host = ScopeHost::default();
    host.install(Feed { frames: VecDeque::from([frame(1), frame(2), frame(4)]), ..Feed::default() });
    assert!(host.running(), "an installed feed is running");
    assert!(!host.wants_frame(), "a running feed waits for publication without polling");
    let mut views: [Option<Hits>; 3] = [None, None, None];
    let mut spectrum_view = Some(Hits::new(3, 1));
    spectrum_view.as_mut().unwrap().waveforms = 99;
    assert!(host.present_lanes(|i, spectrum, hits| if spectrum || i == 1 { Pushed::Busy(hits) } else { Pushed::Taken(views[i].replace(hits)) }), "a received frame changes the view");
    assert!(host.wants_frame(), "a busy lane needs a retry");
    assert_eq!(views[0].as_ref().unwrap().waveforms, 1, "channel 0 reaches view 0");
    assert!(views[1].is_none(), "a busy view leaves its channel pending");
    assert_eq!(views[2].as_ref().unwrap().waveforms, 1, "channel 2 reaches view 2 despite channel 1 being busy");
    assert!(host.present_lanes(|i, spectrum, hits| {
        if spectrum { Pushed::Taken(spectrum_view.replace(hits)) }
        else { assert_eq!(i, 1, "only the busy time lane is retried"); Pushed::Busy(hits) }
    }), "accepting the held spectrum changes the view");
    assert_eq!(spectrum_view.as_ref().unwrap().waveforms, 1, "the spectrum receives its original picture without UI addition");
    assert_eq!(spectrum_view.as_ref().unwrap().data[1], 1.0, "spectrum routing preserves its pixel counts");
    assert_eq!(host.feed().unwrap().takes, 1, "a busy time lane prevents another mailbox take");
    assert_eq!(views[0].as_ref().unwrap().waveforms, 1, "ready lanes wait for the busy lane before receiving a new envelope");
    let mut lane_one = Vec::new();
    assert!(host.present_lanes(|i, spectrum, hits| {
        if spectrum || i == 2 { Pushed::Busy(hits) }
        else { if i == 1 { lane_one.push((hits.waveforms, hits.data[1])); } Pushed::Taken(views[i].replace(hits)) }
    }), "accepting the final held picture opens the gate for the next envelope");
    assert_eq!(lane_one, [(1, 1.0), (2, 2.0)], "a busy lane accepts its original hits before the next frame's hits");
    assert_eq!(host.feed().unwrap().takes, 2, "the cleared gate permits one new mailbox take");
    assert_eq!(host.feed().unwrap().returned.len(), 1, "the preceding envelope returns only after its pending pictures finish");
    assert!(!host.present_lanes(|i, spectrum, hits| {
        if spectrum { Pushed::Hidden(hits) }
        else { assert_eq!(i, 2, "already accepted time lanes are not replayed"); Pushed::Busy(hits) }
    }), "a hidden refusal alone does not count as an upload or new frame");
    assert_eq!(host.feed().unwrap().takes, 2, "a hidden spectrum releases only its own pending bit");
    assert!(host.wants_frame(), "a remaining busy time lane still holds the gate");
    assert!(host.present_lanes(|i, spectrum, hits| if spectrum || i == 2 { Pushed::Hidden(hits) } else { Pushed::Taken(views[i].replace(hits)) }), "a hidden time lane releases the gate for the other lanes");
    assert_eq!(host.feed().unwrap().takes, 3, "hidden pictures allow the next envelope to be taken");
    assert_eq!(host.counters().hidden, 4, "hidden time and spectrum refusals are counted on each envelope");
    assert_eq!(views[0].as_ref().unwrap().waveforms, 4, "channel 0 receives the next frame after the hidden lane releases the gate");
    assert_eq!(views[1].as_ref().unwrap().waveforms, 4, "channel 1 receives the next frame after the hidden lane releases the gate");
    assert_eq!(views[2].as_ref().unwrap().waveforms, 1, "hidden refusals preserve the time view's last accepted picture");
    assert_eq!(spectrum_view.as_ref().unwrap().waveforms, 1, "hidden refusals preserve the spectrum's last accepted picture");
    assert_eq!(host.counters().presented, [7, 7, 1], "only accepted time waveforms count as presented");
    assert!(!host.wants_frame(), "accepted or hidden pictures leave no retry pending");
    assert!(host.running(), "draining the mailbox does not stop the feed");
    assert!(!host.present_lanes(|_, _, _| panic!("accepted and hidden pictures must not be replayed")), "an empty mailbox and no pending hits leave the view unchanged");
    host.stop();
    assert!(!host.running() && !host.wants_frame(), "a stopped feed neither runs nor retries");
    let returned = &host.feed().unwrap().returned;
    assert_eq!(returned.len(), 3, "stopping returns the final held envelope");
    assert_eq!(returned[0].spectra[0].as_ref().unwrap().waveforms, 99, "the replaced spectrum buffer returns through give_back");
    assert_eq!(returned[1].lanes[2].waveforms, 2, "a hidden time buffer returns through its held envelope");
    assert_eq!(returned[1].spectra[0].as_ref().unwrap().waveforms, 2, "a hidden spectrum buffer returns through its held envelope");
    assert!(returned.iter().all(|f| f.lanes.len() == 3), "recycling preserves all three channel slots");
}

#[test]
fn transport_counts_gated_takes_hidden_refusals_transposed_uploads_and_deferred_latency() {
    let frame = |waveforms| {
        let mut frame = LaneFrame::single(4, 3);
        frame.lanes.push(Hits::new(4, 3));
        frame.spectra.push(None);
        frame.published = Some(std::time::Instant::now());
        for hits in &mut frame.lanes { hits.waveforms = waveforms; }
        frame
    };
    let mut first = frame(1);
    first.lanes[0].mark(1, 3, 0, 3);
    first.lanes[1].mark(1, 3, 1, 2);
    first.lanes[1].data[4] = 1.0;
    let mut host = ScopeHost::default();
    host.install(Feed { frames: VecDeque::from([first, frame(2)]), ..Feed::default() });
    host.note_draw(7);
    host.note_draw(7);
    host.present_lanes(|i, _, hits| if i == 1 { Pushed::Busy(hits) } else { Pushed::Taken(None) });
    assert_eq!(host.counters().draws, 1, "repeated callbacks in one drawing count one draw");
    assert_eq!(host.counters().upload_bytes, 24, "whole transposed rows upload their rectangle area times four");
    assert_eq!(host.counters().packed_bytes, 0, "rows 0..h require no platform packing");
    assert_eq!(host.counters().latency_us.len(), 1, "the first taken lane samples the first envelope once");
    host.present_lanes(|_, _, hits| Pushed::Busy(hits));
    assert_eq!(host.counters().takes, 1, "a busy lane gates the second mailbox frame");
    assert_eq!(host.feed().unwrap().takes, 1, "a closed gate does not even attempt a mailbox take");
    assert_eq!(host.counters().busy, 2, "each busy callback is counted");
    assert_eq!(host.counters().pushes, 1, "busy callbacks count no pushes");
    assert_eq!(host.counters().rects, 1, "refused pictures count no upload rectangles");
    let mut shown = None;
    host.present_lanes(|i, _, hits| {
        if i == 1 && hits.waveforms == 1 { shown = Some(hits); Pushed::Taken(None) } else { Pushed::Busy(hits) }
    });
    assert_eq!(host.counters().takes, 1, "a view that accepted a picture in this drawing keeps the next envelope in the mailbox");
    assert!(host.wants_frame(), "the take that waits for the drawing asks for a frame");
    assert_eq!(host.counters().pushes, 2, "the held partial picture counts one more push");
    assert_eq!(host.counters().upload_bytes, 32, "the retained partial rectangle adds eight upload bytes");
    assert_eq!(host.counters().packed_bytes, 8, "partial transposed rows pack all their upload bytes");
    assert_eq!(host.counters().rects, 2, "the partial upload counts one more rectangle");
    assert_eq!(host.counters().presented, [1, 1], "pending pictures retain their original waveform count");
    assert_eq!(shown.unwrap().data[4], 1.0, "instrumentation preserves held hit values");
    assert_eq!(host.counters().latency_us.len(), 1, "an entirely untaken second frame waits for a later acceptance");
    host.reset_counters();
    host.note_draw(7);
    host.note_draw(8);
    host.present_lanes(|i, _, hits| if i == 0 { Pushed::Hidden(hits) } else { Pushed::Taken(None) });
    assert_eq!(host.counters().draws, 1, "reset preserves the last drawing id across report boundaries");
    assert_eq!(host.counters().takes, 1, "the next drawing takes the waiting envelope");
    assert_eq!(host.counters().hidden, 1, "a hidden view counts one refusal");
    assert_eq!(host.counters().pushes, 1, "a hidden view contributes no push");
    assert_eq!(host.counters().presented, [0, 2], "hidden waveforms do not count as presented");
    assert_eq!(host.counters().latency_us.len(), 1, "a hidden refusal leaves the stamp for an accepted lane");
    assert!(!host.wants_frame(), "a take that emptied the mailbox waits for the merger's next signal, not a frame");
    host.note_draw(9);
    host.present_lanes(|_, _, _| panic!("accepted and hidden pictures must not be replayed"));
    assert_eq!(host.counters().takes, 1, "an empty mailbox invents no take");

    host.install(Feed { frames: VecDeque::from([frame(1), frame(2)]), ..Feed::default() });
    host.reset_counters();
    host.present_lanes(|_, _, hits| Pushed::Busy(hits));
    host.present_lanes(|_, _, hits| Pushed::Busy(hits));
    assert_eq!(host.counters().takes, 1, "fully busy views keep the next envelope in the mailbox");
    assert!(host.counters().latency_us.is_empty(), "fully busy views defer publication latency");
    host.present_lanes(|_, _, _| Pushed::Taken(None));
    assert_eq!(host.counters().takes, 1, "acceptance holds the gate until the views draw");
    host.note_draw(10);
    host.present_lanes(|_, _, _| Pushed::Taken(None));
    assert_eq!(host.counters().takes, 2, "the next drawing opens the gate for the second envelope");
    assert_eq!(host.counters().latency_us.len(), 2, "each gated envelope contributes one sample when first accepted");
    assert_eq!(host.counters().pushes, 4, "both lanes accept both envelopes even with empty dirty rectangles");
    assert_eq!(host.counters().upload_bytes, 0, "empty dirty pictures upload no bytes");
    assert_eq!(host.counters().packed_bytes, 0, "empty dirty pictures pack no bytes");
    assert_eq!(host.counters().rects, 0, "empty dirty pictures count no rectangles");
}

#[test]
fn two_hidden_lanes_never_gate_takes_and_two_slow_visible_lanes_keep_every_waveform() {
    const FRAMES: u64 = 9;
    let lanes = vec![LaneSetup::time(PhosphorMap::screen(2, 2, 0.0, 1.0, -1.0, 1.0)); 4];
    let (merger, mailbox) = Merger::with_lanes(&lanes);
    let merger = Arc::new(Mutex::new(merger));
    let mut host = ScopeHost::default();
    host.install(Feed { mailbox: Some(mailbox), recycle: Some(merger.clone()), ..Feed::default() });
    let mut views: [Option<Hits>; 2] = [None, None];
    let mut waveforms = [0; 2];
    let mut pixels = [0.0; 2];
    let mut retained = 0;
    let mut hidden_waveforms = [0; 2];
    // Four final calls drain the mailbox and the last worker sum, including their busy retries.
    for call in 1..=FRAMES + 4 {
        if call <= FRAMES {
            let mut frame = LaneFrame::new(&lanes);
            for (i, hits) in frame.lanes.iter_mut().enumerate() {
                hits.waveforms = 1;
                hits.data[i] = (i + 1) as f32;
                hits.mark(0, 2, 0, 2);
            }
            merger.lock().unwrap().add(frame);
        }
        let published = merger.lock().unwrap().publish();
        if call <= FRAMES && !published { retained += 1; }
        let was_waiting = host.wants_frame();
        let takes = host.feed().unwrap().takes;
        let mut accepted = [false; 2];
        host.present_lanes(|i, spectrum, hits| {
            assert!(!spectrum, "time-only lanes must not route a spectrum picture");
            if i >= 2 { hidden_waveforms[i - 2] += hits.waveforms; return Pushed::Hidden(hits); }
            if call % 2 != 0 || accepted[i] { return Pushed::Busy(hits); }
            accepted[i] = true;
            waveforms[i] += hits.waveforms;
            pixels[i] += hits.data[i];
            Pushed::Taken(views[i].replace(hits))
        });
        if was_waiting && call % 2 != 0 {
            assert_eq!(host.feed().unwrap().takes, takes, "a busy display call must not take a new mailbox frame");
        }
    }
    assert!(!host.wants_frame(), "the final flush drains every pending picture");
    assert_eq!(hidden_waveforms, [FRAMES; 2], "each hidden lane must release every waveform without a busy retry");
    assert_eq!(host.counters().hidden, host.counters().takes * 2, "both hidden pictures must be counted once per taken envelope");
    assert_eq!(waveforms, [FRAMES; 2], "each lane presents every input waveform exactly once");
    assert_eq!(pixels, [FRAMES as f32, (FRAMES * 2) as f32], "worker sums preserve each lane's known pixel counts");
    assert_eq!(host.counters().presented, [FRAMES; 2], "transport counters account for all accepted waveforms");
    assert!(host.counters().takes < FRAMES, "slow views cause worker sums rather than one UI take per input");
    host.stop();
    let merger = merger.lock().unwrap();
    assert_eq!(merger.lane_waveforms(), &[FRAMES; 4], "the merger counts every lane's input");
    assert!(retained > 0, "the test exercises publication backpressure while the UI gate is closed");
    assert_eq!(merger.dropped(), 0, "gating and returning every envelope must not drop a frame");
}

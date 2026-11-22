//! What a person does while the recording plays is heard where they did it: the host's run heard
//! through the app's Listen tap and player (`support/ears.rs`), below real time, each edit made
//! with the run well ahead of the play head.

use fab_elec_bench::{SimCmd, SimMode};
use fab_elec_core::ids::{CompRef, NetRef};
use fab_elec_sim::live::LiveValues;
use fab_elec_sim::listen::{voice_key, AudioTap, Player, TapMix, TapSpec, TapVoice, FADE_SECONDS};
use fab_elec_sim::recording::{hold_tapes, settle_tapes, window_frames, Shape, CHUNK};
use fab_elec_sim::livesim::{LiveSpec, Snapshots};
use fab_elec_simhost::host::{MIN_PRESS, RING_SECONDS};
use fab_elec_simhost::{SimHost, Spawn};
use fab_elec_view::live::listen::{JACK_TAP, LISTEN_DC_HZ, LISTEN_GAIN, LISTEN_LATENCY, LISTEN_TAP};
use fab_elec_view::live::sim::{SimAction, SimLive, SinkSlot};
use fab_elec_sim::Sink;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "support/ears.rs"]
mod ears;
use ears::{ready, Ears, Take, EAR_GAIN, EAR_RATE, SIDE, WINDOW};

/// The test's threads: one std thread per worker and per job.
struct Threads;

impl Spawn for Threads {
    fn worker(&self, _name: &'static str, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
    fn job(&self, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
}

/// The mean pitch over the rising crossings of `level` in `samples` at `rate` Hz: None with
/// fewer than four.
fn pitch_in(samples: &[f32], rate: f64, level: f32) -> Option<f64> {
    let rises: Vec<f64> = samples.windows(2).enumerate()
        .filter_map(|(i, w)| (w[0] <= level && w[1] > level).then(|| i as f64 + f64::from((level - w[0]) / (w[1] - w[0]))))
        .collect();
    (rises.len() >= 4).then(|| (rises.len() - 1) as f64 * rate / (rises[rises.len() - 1] - rises[0]))
}

/// What a person does while the recording plays is heard where they did it: the tone box run
/// below real time (paced as a heavy board runs), its oscillator heard through the app's Listen
/// tap and player, the key held from the start. With the run well ahead of the play head, the
/// PITCH knob turned to the top, r_t's value edited (68k) and the key let go each go in at the
/// time the listener hears: the run goes back there and renders on, and the recording from there
/// carries the change, the stretch before it as it was. The oscillator heard before the turn
/// plays at the RC formula's pitch at mid travel and after it at the top; after the value edit at
/// the formula's pitch with the new part; after the release it is still. An edit taken at the
/// run's front would leave the stretch after it as it was.
#[test]
fn a_knob_a_value_and_a_press_while_the_recording_plays_are_heard_where_they_were_made() {
    let sources = fab_elec_fixtures::docs("tonebox").expect("the tone box is a stock project");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("the tone box evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "box").expect("the tone box has its bench");
    let design = Arc::new(ev.design.clone());
    let unit = design.unit(bench.scope_id()).expect("the bench runs the board");
    let comp = |path: &str| CompRef { board: unit.id, comp: unit.circuit.comp(path).unwrap_or_else(|| panic!("the tone box has {path}")).id };
    let (key, pitch) = (comp("key"), comp("pitch"));
    let libs = fab_elec_bench::ModelLibs::generic();
    let formula = |r_t: f64, pos: f64| {
        let r = r_t + fab_elec_bench::input::Taper::Linear.halves(250e3, pos, libs.pot_end).1;
        let (vp, vm, vdd) = (libs.schmitt.vt_plus, libs.schmitt.vt_minus, libs.schmitt.vdd);
        1.0 / (r * 10e-9 * ((vp * (vdd - vm)) / (vm * (vdd - vp))).ln())
    };
    // the oscillator's square wave swings the supply: its crossings at half of it, as heard
    let level = (2.5 * f64::from(EAR_GAIN)) as f32;
    let pitch_of = |take: &Take, from: f64, to: f64| {
        pitch_in(take.between(from, to), take.rate, level).unwrap_or_else(|| panic!("a tone is heard over [{from:.4}, {to:.4}) s"))
    };

    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    host.commit(&design);
    let mut live = SimLive::default();
    let mut ears = Ears::new(&mut host, &mut live, "OSC", WINDOW);
    ears.until(&mut host, &mut live, start, "the box runs", |h, _| h.values().is_some());
    // nothing of the recording is heard yet: the key goes down at the run's front
    let down = host.values().map_or(0.0, |v| v.t);
    host.act(&SimAction::Cmd(SimCmd::press(key, true)));
    // each edit once the play head is past the last one's stretch (the tone's start's first)
    ears.until(&mut host, &mut live, start, "the recording plays, the run ahead of it", ready(down + 0.01));
    let speed = ears.bus.ring.tap_speed();

    // the knob to the top, where the listener hears
    let rewinds = host.store().rewinds;
    let turned = ears.heard_at().expect("the recording plays");
    host.act(&SimAction::Cmd(SimCmd::SetParam { comp: pitch, value: 1.0 }));
    let knob_edit = host.heard_edit().map(|(name, t)| (name.to_owned(), t));
    assert_eq!(knob_edit.as_ref().map(|e| e.1), Some(turned), "the knob goes in where the listener hears: {knob_edit:?}");
    let take = ears.around(&mut host, &mut live, start, turned, rewinds, "the run renders past the turn again");
    let (mid, top) = (pitch_of(&take, turned - SIDE, turned - 1e-3), pitch_of(&take, turned + 3e-3, turned + SIDE));
    let (want_mid, want_top) = (formula(100e3, 0.5), formula(100e3, 1.0));
    assert!((mid / want_mid - 1.0).abs() < 0.02, "before the turn the take plays {mid:.1} Hz, the formula's {want_mid:.1} Hz at mid travel within 2 %");
    assert!((top / want_top - 1.0).abs() < 0.02, "from the turn the take plays {top:.1} Hz, the formula's {want_top:.1} Hz at the top within 2 %");

    // r_t's value edited (68k), the knob at the top: the run takes it where the listener hears
    ears.until(&mut host, &mut live, start, "the play head past the turn, the run ahead of it", ready(turned));
    let rewinds = host.store().rewinds;
    let valued = ears.heard_at().expect("the recording plays");
    assert!(host.command(&SimCmd::SetParam { comp: comp("r_t"), value: 68e3 }), "a resistor's value is an edit");
    let value_edit = host.heard_edit().map(|(name, t)| (name.to_owned(), t));
    assert!(value_edit.as_ref().is_some_and(|e| e.1 == valued && Some(e) != knob_edit.as_ref()), "the value goes in where the listener hears, named: {value_edit:?}");
    let take = ears.around(&mut host, &mut live, start, valued, rewinds, "the run renders past the value again");
    let (old, new) = (pitch_of(&take, valued - SIDE, valued - 1e-3), pitch_of(&take, valued + 3e-3, valued + SIDE));
    let want_new = formula(68e3, 1.0);
    assert!((old / want_top - 1.0).abs() < 0.02, "before the value edit the take plays {old:.1} Hz, the formula's {want_top:.1} Hz within 2 %");
    assert!((new / want_new - 1.0).abs() < 0.02, "from the value edit the take plays {new:.1} Hz, the formula's {want_new:.1} Hz with r_t 68k within 2 %");

    // the key let go where the listener hears, a press's least length past its going down
    ears.until(&mut host, &mut live, start, "the play head past the value edit, the run ahead of it", ready(valued.max(down + MIN_PRESS)));
    let rewinds = host.store().rewinds;
    let released = ears.heard_at().expect("the recording plays");
    host.act(&SimAction::Cmd(SimCmd::press(key, false)));
    let take = ears.around(&mut host, &mut live, start, released, rewinds, "the run renders past the release again");
    let before = pitch_of(&take, released - SIDE, released - 1e-3);
    assert!((before / want_new - 1.0).abs() < 0.02, "up to the release the take plays {before:.1} Hz, the formula's {want_new:.1} Hz within 2 %");
    let after = take.between(released + 1e-3, released + SIDE);
    let loudest = after.iter().fold(0.0f32, |m, &s| m.max(s));
    assert!(loudest < level / 4.0, "from a millisecond after the release the oscillator is still: {loudest} at most, against its swing of {}", 2.0 * level);

    // auto re-record off: an edit while the recording plays goes in at the run's front and waits
    // for the next record, which takes it in
    assert!(host.command(&SimCmd::SetAutoRerecord(false)), "auto re-record is a host setting");
    let heard = host.heard_edit().map(|(name, t)| (name.to_owned(), t));
    assert!(host.command(&SimCmd::SetParam { comp: comp("r_t"), value: 82e3 }), "a resistor's value is an edit");
    assert_eq!(host.heard_edit().map(|(name, t)| (name.to_owned(), t)), heard, "with auto re-record off an edit is not taken where it is heard");
    assert_eq!(host.status().waiting, 1, "with auto re-record off the edit waits for the next record");
    host.act(&SimAction::Cmd(SimCmd::Stop));
    host.act(&SimAction::Cmd(SimCmd::Play));
    host.commit(&design); // as the app's frames do: the design lowers again
    ears.until(&mut host, &mut live, start, "a new record takes the waiting edit in", |h, _| h.status().waiting == 0 && h.values().is_some());
    host.act(&SimAction::Cmd(SimCmd::Stop));
    eprintln!("heard edits: the run at {speed:.2}x real time; knob at {turned:.4} s {mid:.1} -> {top:.1} Hz, value at {valued:.4} s -> {new:.1} Hz, release at {released:.4} s; {:?}",
        start.elapsed());
}

/// A take of finite length (the bench's `run.stop`, here one window): the run renders to its end
/// and rests there, and the recording plays it round. The transport's Stop (`Hold`) silences what
/// is heard within a fade and holds the play head where it is, and nothing that comes later plays
/// it again; Play goes on from there. A Pause silences and holds it the same way.
#[test]
fn a_take_renders_to_its_end_loops_and_stop_holds_it_silent() {
    let sources = fab_elec_fixtures::docs("tonebox").expect("the tone box is a stock project");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("the tone box evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "box").expect("the tone box has its bench");
    let design = Arc::new(ev.design.clone());
    let unit = design.unit(bench.scope_id()).expect("the bench runs the board");
    let key = CompRef { board: unit.id, comp: unit.circuit.comp("key").expect("the tone box has its key").id };
    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_take_len(Some(WINDOW));
    // the take keeps whole audio chunks at the recording rate: its loop and its passes agree
    let take = host.take_len().expect("the take has a length");
    assert!((take - WINDOW).abs() < 6e-3, "the kept length {take} is the asked one within a chunk");
    assert!(host.command(&SimCmd::SetContinuous(true)), "the host accepts Continuous");
    assert_eq!(host.take_len(), None, "Continuous opens the run");
    assert!(host.status().continuous, "the status reports Continuous");
    assert_eq!(host.status().ring, RING_SECONDS, "Continuous starts with the default ring length");
    let ring = 0.05;
    assert!(host.command(&SimCmd::SetRing(ring)), "the host accepts a new ring length");
    assert_eq!(host.status().ring, ring, "the status reports the chosen ring length");
    assert!(host.command(&SimCmd::SetRealtime(true)), "the host accepts Realtime");
    assert!(host.status().realtime && host.status().continuous, "Realtime uses Continuous's open run");
    assert_eq!((host.take_len(), host.status().ring), (None, ring), "Realtime keeps the selected ring without a finite take");
    assert!(host.command(&SimCmd::SetRealtime(false)), "the host can leave Realtime");
    assert!(!host.status().realtime && host.status().continuous, "leaving Realtime returns to Continuous");
    assert!(host.command(&SimCmd::SetRealtime(true)), "Realtime can be selected again");
    assert!(host.command(&SimCmd::SetContinuous(false)), "the host accepts Loop");
    assert!(!host.status().realtime, "selecting Loop clears Realtime");
    assert_eq!(host.take_len(), Some(take), "Loop restores the bench's take length");
    assert!(!host.status().continuous, "the status clears Continuous in Loop mode");
    assert!(host.command(&SimCmd::SetRealtime(true)), "Loop can switch directly to Realtime");
    host.set_take_len(None);
    assert!(!host.status().realtime, "a change of bench take length clears Realtime");
    assert!(host.status().continuous, "a bench without a take defaults to Continuous");
    assert!(host.command(&SimCmd::SetContinuous(false)), "a bench without a take accepts Loop");
    assert_eq!(host.take_len(), Some(ring), "Loop uses the ring length when the bench has no take");
    host.set_take_len(Some(WINDOW));
    assert!(!host.status().continuous, "a bench with a take defaults to Loop");
    host.set_take_live(true);
    assert!(host.status().realtime, "a bench that opens live (its run: {{live: true}}) defaults to Realtime");
    assert!(host.command(&SimCmd::SetContinuous(false)), "a live bench accepts Loop");
    assert!(!host.status().realtime && !host.status().continuous, "the person's Loop stands over the live bench's Realtime");
    host.set_take_live(false);
    assert!(!host.status().realtime && !host.status().continuous, "a bench no longer live defaults to Loop again");
    assert!(host.command(&SimCmd::SetTakeLen(Some(2.0 * WINDOW))), "the host accepts the person's take length");
    let longer = host.take_len().expect("the person's length is a take");
    assert!((longer - 2.0 * WINDOW).abs() < 6e-3 && !host.status().continuous, "the person's length {longer} stands in for the bench's, and Loop stays");
    host.set_take_len(Some(WINDOW / 2.0));
    assert_eq!(host.take_len(), Some(longer), "a bench's new stop leaves the person's length standing");
    host.set_take_len(Some(WINDOW));
    assert!(host.command(&SimCmd::SetTakeLen(None)), "the person's length can be cleared");
    assert_eq!(host.take_len(), Some(take), "cleared, the bench's take length is back");
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    host.commit(&design);
    let mut live = SimLive::default();
    let mut ears = Ears::new(&mut host, &mut live, "OSC", take);
    ears.until(&mut host, &mut live, start, "the box runs", |h, _| h.values().is_some());
    host.act(&SimAction::Cmd(SimCmd::press(key, true)));
    ears.until(&mut host, &mut live, start, "the take renders to its end and rests", |h, _| h.values().is_some_and(|v| (v.t - take).abs() < 1e-9 && !v.running));
    assert_eq!(host.status().take, Some(take), "the status says the take's length");
    // the recording plays it round: the heard time comes back near the take's start
    let mut latest = 0.0f64;
    ears.until(&mut host, &mut live, start, "the take plays round", |_, e| {
        let at = e.heard_at().unwrap_or(0.0);
        let wrapped = at + take / 2.0 < latest;
        latest = latest.max(at);
        wrapped
    });
    let level = 0.5 * 2.5 * EAR_GAIN;
    let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    // frames until `n` in a row played as `loud` says, bounded by frames
    let run_of = |host: &mut SimHost, live: &mut SimLive, ears: &mut Ears, n: usize, loud: bool, what: &str| {
        let mut row = 0;
        for _ in 0..20_000 {
            let p = peak(ears.frame(host, live, start));
            row = if (p > level) == loud { row + 1 } else { 0 };
            if row >= n {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("{what}: not {n} frames in a row");
    };
    run_of(&mut host, &mut live, &mut ears, 20, true, "the loop is heard");
    // Stop: silent within a fade, the play head held, and nothing late plays it again
    assert!(host.command(&SimCmd::Hold), "the transport's Stop is taken");
    let mut frames = 0;
    // silent once a fade has played after the frame that reads the Stop (the take has silent
    // stretches of its own, so a quiet frame inside the fade proves nothing)
    let fade = (FADE_SECONDS * EAR_RATE).ceil() as usize;
    let mut faded = 0;
    loop {
        let out = ears.frame(&mut host, &mut live, start);
        let p = peak(out);
        if frames > 0 {
            faded += out.len();
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        frames += 1;
        if faded > fade && p == 0.0 {
            break;
        }
        assert!(frames < 100, "Stop silences what is heard within a fade ({p} still at frame {frames})");
    }
    // the instant heard trails the player by the device's latency (here longer than the loop), so
    // it settles on the held play head once that has passed, and stays
    let mut held = None;
    for i in 0..300 {
        let p = peak(ears.frame(&mut host, &mut live, start));
        assert!(p == 0.0, "after Stop nothing is heard: {p} at frame {i}");
        if i == 150 {
            held = ears.heard_at();
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(held.is_some_and(|t| (0.0..take).contains(&t)), "the play head holds inside the take: {held:?}");
    assert_eq!(ears.heard_at(), held, "the play head holds where Stop found it");
    assert!(!host.status().playing && host.values().is_some_and(|v| (v.t - take).abs() < 1e-9), "Stop keeps the run and its take");
    // Play goes on from there; Pause holds it silent the same way
    assert!(host.command(&SimCmd::Play), "Play is taken");
    run_of(&mut host, &mut live, &mut ears, 5, true, "Play brings the loop back");
    assert!(host.command(&SimCmd::Pause), "Pause is taken");
    run_of(&mut host, &mut live, &mut ears, 100, false, "Pause silences the loop");
    assert!(host.command(&SimCmd::SetContinuous(true)), "the running bench accepts Continuous");
    assert!(host.command(&SimCmd::Play), "Play resumes the open run after Pause");
    ears.until(&mut host, &mut live, start, "the record fills a trace chunk", |h, _| !h.store().time.chunks.is_empty());
    let old_base = host.store().base;
    let old_end = *host.store().time.chunks[0].last().expect("a closed trace chunk has points");
    ears.until(&mut host, &mut live, start, "the open run advances a ring past the old chunk", |h, _| {
        let store = h.store();
        !store.is_empty() && store.time.at(store.len() - 1) > take.max(old_end) + ring
    });
    let store = host.store();
    let end = store.time.at(store.len() - 1);
    let oldest = store.time.at(0);
    let chunk_end = store.time.at(store.len().min(fab_elec_sim::trace::CHUNK) - 1);
    assert!(store.base > old_base && chunk_end >= end - ring,
        "Continuous drops expired chunks and keeps at most one chunk across the ring boundary: oldest {oldest}, chunk end {chunk_end}, boundary {}", end - ring);
    host.act(&SimAction::Cmd(SimCmd::Stop));
}

/// While the recording plays, the views show the instant heard, not the render's front: the tone
/// box heard as the app hears its jack (a 96 kHz take played on a 48 kHz output in buffers of
/// 10 ms). The test drives both clocks by count: the device takes three buffers a frame (30 ms of
/// sound), and the run renders the Steps the test sends, one of 4 ms a frame (a Step sent while
/// one runs waits for it), so the sound outruns the render however long a frame takes, and the
/// take's first window plays round while the next renders. The run is paced to a quarter of real
/// time at most (the rig's `ears::pace`, a board too heavy to keep up), so the player's switch to
/// its recording (it runs out of sound while the run runs, measured below real time) holds on any
/// box: unpaced, a native build ends each Step well within a frame, the player runs out of sound
/// with the run resting, and it stays live, nothing heard. The key is pressed half way into the
/// first window, where a Step from the run's start leaves it standing, so nothing can be heard yet
/// however late a frame comes (and the press goes in at the front). At each frame the QUIET net's
/// row (the key's level) and the time the views show (`SimLive::t`, `heard`) are the run's at the
/// instant heard (`SampleRing::heard_now`): a frame's row is the sound's stretch since the frame
/// before (a box, the frame at the instant alone after a wrap), so a stretch 20 ms or more from the
/// key's edge reads the run's level there, across the window's wraps, also while the run's front is
/// past the edge (where the front's level would be the other one).
#[test]
fn the_rows_show_the_instant_heard_while_the_recording_plays() {
    let sources = fab_elec_fixtures::docs("tonebox").expect("the tone box is a stock project");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("the tone box evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "box").expect("the tone box has its bench");
    let design = Arc::new(ev.design.clone());
    let unit = design.unit(bench.scope_id()).expect("the bench runs the board");
    let key = CompRef { board: unit.id, comp: unit.circuit.comp("key").expect("the tone box has its key").id };
    let quiet = NetRef { board: unit.id, net: unit.circuit.net("QUIET").expect("the tone box has QUIET").id };

    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: 0.1, ..LiveSpec::default() });
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    assert!(host.command(&SimCmd::Pause), "the run starts paused: the test steps it");
    host.commit(&design);
    let mut live = SimLive::default();
    // the jack heard as the app's Listen hears it: its shape to the player, a tap recording takes
    const RATE: f64 = 48_000.0;
    // the device's buffers a frame (10 ms each), and the run's Step
    const BUFFERS: usize = 3;
    const STEP: f64 = 4e-3;
    // the most frames a phase takes: a bound on a hang, never on time
    const FRAMES: u32 = 50_000;
    let bus = live.listen_bus();
    let ring = bus.ring.clone();
    ring.set_device_rate(RATE as u32);
    ring.set_open(true);
    let mut player = Player::new(bus.take_player());
    let mix = TapMix::new(1);
    bus.links.shapes.try_send(Box::new(Shape::new(1, WINDOW, 1, mix.clone()))).ok().expect("the shape goes");
    let voices = vec![TapVoice { net: "OUT".into(), reference: None, gain: 0.4 }];
    let tap = AudioTap::new(TapSpec { voices, latency: 0.06, dc_block_hz: 5.0, mix }, ring.clone()).recording(bus.record(1, LISTEN_TAP, WINDOW));
    host.act(&SimAction::Listen { id: LISTEN_TAP, sink: SinkSlot::new(Box::new(tap)) });
    // the run below real time on any box, as a board too heavy to keep up: the player hears its
    // recording once it runs out of sound while the run runs
    ears::pace(&mut host);

    // one frame: the device takes its buffers, then the host steps; the run's publications as
    // they come are the truth (their QUIET means)
    let mut out = vec![0.0f32; 480];
    let mut truth: Vec<(f64, f64, f32)> = Vec::new();
    let mut seen = None;
    let mut frame = |host: &mut SimHost, live: &mut SimLive, truth: &mut Vec<(f64, f64, f32)>| {
        for _ in 0..BUFFERS {
            player.fill(&ring, &mut out, RATE);
        }
        host.set_rendering(ring.plays_recording());
        host.step(start.elapsed().as_secs_f64(), live);
        if let Some(v) = host.values().filter(|v| seen.as_ref().is_none_or(|s: &Arc<LiveValues>| !Arc::ptr_eq(s, v))).cloned() {
            seen = Some(v.clone());
            if let Some(q) = host.index().net(quiet) {
                truth.push((v.since, v.t, v.net_mean[q]));
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    // the key goes down half way into the first window (a stretch of it on each side of the
    // edge), where a Step from the start of the run (at the audio rate) leaves it standing
    let standing = |h: &SimHost, at: f64| {
        h.restart_pending().is_none() && h.values().is_some_and(|v| v.generation == h.index().generation && !v.running && v.t >= at)
    };
    for _ in 0..FRAMES {
        frame(&mut host, &mut live, &mut truth);
        if standing(&host, 0.0) {
            break;
        }
    }
    assert!(standing(&host, 0.0) && host.command(&SimCmd::Step(0.5 * WINDOW + 2e-3)), "the box stands at its start and takes a Step: {:?}", host.error());
    for _ in 0..FRAMES {
        frame(&mut host, &mut live, &mut truth);
        if standing(&host, 0.5 * WINDOW) {
            break;
        }
    }
    assert!(host.error().is_none() && standing(&host, 0.5 * WINDOW), "the Step ends half way into the first window: {:?}", host.error());
    assert!(live.heard.is_none(), "nothing is heard before the first window is rendered: {:?}", live.heard);
    let pressed = host.values().map_or(0.0, |v| v.t);
    host.act(&SimAction::Cmd(SimCmd::press(key, true)));

    // until the window played round with enough frames heard on both sides of the key's edge (at
    // or after the front when pressed), the run's front well into the third window; at most
    // twenty windows
    let mut shown: Vec<(f64, f32, f64)> = Vec::new();
    let (mut before, mut after) = (0, 0);
    for _ in 0..FRAMES {
        frame(&mut host, &mut live, &mut truth);
        // the render's pace is the test's: at most a Step a frame
        assert!(host.command(&SimCmd::Step(STEP)), "the Step reaches the run");
        if let (Some((t, _)), Some(q), Some(v)) = (live.heard, host.index().net(quiet), host.values()) {
            assert_eq!(live.t, t, "the views' time is the instant heard");
            let a = shown.last().map_or(t, |&(p, _, _)| if p <= t { p } else { t });
            before += usize::from(t < pressed - 0.02);
            after += usize::from(a > pressed + 0.05);
            shown.push((t, host.rows().nets().row(q)[1], v.t));
        }
        let wrapped = shown.windows(2).any(|p| p[1].0 < p[0].0);
        if host.values().is_some_and(|v| (v.t > 2.5 * WINDOW && wrapped && before > 5 && after > 5) || v.t > 20.0 * WINDOW) {
            break;
        }
    }
    let high = truth.iter().find(|p| p.1 > 0.01).map(|p| p.2).expect("QUIET's level before the press");
    let edge = truth.iter().find(|p| p.0 > 0.03 && p.2 < 0.5 * high).map(|p| p.0).expect("QUIET falls after the press");
    let wraps = shown.windows(2).filter(|p| p[1].0 < p[0].0).count();
    // a frame's row is its stretch of the sound, since the frame before (the frame at `t` alone
    // after a wrap): wrong where all of that stretch lies 20 ms or more on one side of the edge
    // and the row reads the other side's level
    let from = |i: usize| if i > 0 && shown[i - 1].0 <= shown[i].0 { shown[i - 1].0 } else { shown[i].0 };
    let wrong: Vec<_> = (0..shown.len()).map(|i| (from(i), shown[i]))
        .filter(|&(a, (t, v, _))| (t < edge - 0.02 && v < 0.7 * high) || (a > edge + 0.02 && v > 0.3 * high)).collect();
    let behind = shown.iter().filter(|&&(t, _, front)| t < edge - 0.02 && front > edge + 0.02).count();
    let past = (0..shown.len()).filter(|&i| from(i) > edge + 0.02).count();
    assert!(wrong.is_empty(), "QUIET's row reads the run's level at the instant heard (edge at {edge:.4} s, high {high:.2} V): {} of {} frames read otherwise, first {:?}", wrong.len(), shown.len(), wrong.first());
    assert!(wraps > 0 && behind > 3 && past > 3, "the recording played round the first window ({wraps} wraps, {} frames heard: {behind} before the edge with the front past it, {past} after it)", shown.len());
}

/// The slots of the held-take test's recorders: the oscillator alone from the run's start, and
/// a voice the hearing recorder does not tape.
const ALONE: u64 = 11;
const OTHER: u64 = 12;
const GATE: u64 = 13;

/// Host frames at 2 ms until `done` holds, bounded by frames, not wall time.
fn step(host: &mut SimHost, live: &mut SimLive, start: Instant, what: &str, mut done: impl FnMut(&SimHost) -> bool) {
    for _ in 0..50_000 {
        host.step(start.elapsed().as_secs_f64(), live);
        assert!(host.error().is_none(), "{what}: the run failed: {:?}", host.error());
        if done(host) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("{what}: not reached within 50 000 frames; the run at {:?} s", host.values().map(|v| v.t));
}

/// A Listen over a take at rest plays the take from the hearing recorder's tape with no run
/// again. The tone box's take renders to its end with the key held, and a value edit where the
/// silent clock holds sends it back and it renders on to its end again. A Listen of a voice the
/// hearing recorder does not tape (another gain) records the take again, the reason on the bar.
/// A Listen of the oscillator's Scope lane (the voice the app makes for it) then has the whole
/// take at once: the run neither goes back nor moves, the tap's chunks are the take in order from
/// its first point's time, and they are bit for bit what a recorder of the oscillator alone,
/// present from the run's start, taped; the hearing recorder's tape is the take's length exactly.
/// The listener hears it, and a value edit heard late in it goes in where it is heard (F5): the
/// run goes back to the take's snapshot inside it (the last render paced) and the Listen follows
/// the hearing recorder as it tapes the take again, never past a cut the tape writer has not made
/// yet (the writer held, as a disk that stalls): nothing reaches the player while the cut waits;
/// then the chunks from the snapshot go again under the take's number, each what the oscillator
/// alone recorder tapes anew, the take before them as it was. One rewind, no restart. In Fast and
/// in Exact.
#[test]
fn a_listen_over_a_take_at_rest_plays_its_recording_with_no_run_again() {
    for exact in [false, true] {
        listen_held(exact);
    }
}

/// A Listen asked as the take comes to rest hears the take whole from its recording. The run is
/// held in its sinks' end of the take (`Sink::ended`, where the hearing recorder pads its last
/// chunk) while the host reads what the run published: the take is not at rest yet, so a Listen
/// asked then follows the tape, and once the run rests it has the take's chunks in order, once,
/// bit for bit what the oscillator alone recorder taped, with no run again. A host that read the
/// rest before the recorders had the take's end found the tape short ("the tape stopped short")
/// and recorded a whole take again. In Fast and in Exact.
#[test]
fn a_listen_asked_as_the_take_comes_to_rest_hears_it_whole() {
    for exact in [false, true] {
        listen_at_rest_edge(exact);
    }
}

/// Holds the run in its take's end (`Sink::ended`) once, wherever it sits among the sinks that
/// follow it: it says so on `inside` and waits there until `release` sends or is dropped. Installed
/// before the hearing recorder, whose end it then holds back.
struct EndGate {
    inside: SyncSender<()>,
    release: Receiver<()>,
    armed: bool,
}

impl Sink for EndGate {
    fn ended(&mut self, _at: f64) {
        if std::mem::take(&mut self.armed) {
            let _ = self.inside.send(());
            let _ = self.release.recv();
        }
    }
}

fn listen_at_rest_edge(exact: bool) {
    let mode = if exact { "Exact" } else { "Fast" };
    let start = Instant::now();
    let ((inside, entered), (release, held)) = (sync_channel(1), sync_channel(1));
    let (mut host, mut live, _) = tone_box(exact, false, Some(WINDOW), start, Some(EndGate { inside, release: held, armed: true }));
    let bus = live.listen_bus();
    let take = host.take_len().expect("the take has a length");
    let rests = |h: &SimHost| h.values().is_some_and(|v| (v.t - take).abs() < 1e-9 && !v.running);
    step(&mut host, &mut live, start, "the run reaches the take's end and its sinks take it", |_| entered.try_recv().is_ok());
    step(&mut host, &mut live, start, "a frame reads what the run published", |_| true);
    assert!(!rests(&host), "{mode}: the take is not at rest while its sinks still take its end: the run at {:?}", host.values().map(|v| (v.t, v.running)));

    // the Listen asked now, the hearing recorder's last chunk not taped yet
    let (rewinds, generation) = (host.store().rewinds, host.store().generation);
    let mut ears = Ears::listen(&mut host, &mut live, lane(), LISTEN_DC_HZ, take, true);
    assert_eq!(host.status().rerender_reason, None, "{mode}: a Listen asked as the take comes to rest records nothing again");
    drop(release);
    let rate = host.content().expect("the run is keyed").recording_rate;
    let frames = window_frames(take, rate) as u64;
    let chunks = frames as usize / CHUNK;
    let got = |e: &Ears| e.tee.as_ref().expect("the Listen is tapped").got.clone();
    ears.until(&mut host, &mut live, start, "the run rests and the Listen has the whole take", |h, e| {
        assert_eq!(h.status().rerender_reason, None, "{mode}: the take renders nothing again");
        rests(h) && got(e).len() >= chunks
    });
    assert_eq!((host.store().rewinds, host.store().generation), (rewinds, generation), "{mode}: the run neither went back nor started again");
    let held = got(&ears);
    let n = held[0].0;
    assert!(held.len() == chunks && held.iter().enumerate().all(|(i, c)| c.0 == n && c.1 == i as u64), "{mode}: the take's {chunks} chunks in order, once: {:?}", held.iter().map(|c| (c.0, c.1)).collect::<Vec<_>>());
    settle_tapes();
    let alone = bus.tapes.current(ALONE).expect("the oscillator alone is taped");
    let mut want = vec![0.0f32; frames as usize];
    assert_eq!(alone.read(0, &mut want).expect("the take reads back"), frames as usize, "{mode}: the oscillator alone is taped whole");
    assert_eq!(held[0].2, alone.t0, "{mode}: the take starts at its first point's time");
    let heard: Vec<f32> = held.iter().flat_map(|c| c.3.iter().copied()).collect();
    let differ = heard.iter().zip(&want).position(|(a, b)| a.to_bits() != b.to_bits());
    assert_eq!(differ, None, "{mode}: the Listen's take is bit for bit what the oscillator alone taped");
    host.act(&SimAction::Cmd(SimCmd::Stop));
}

/// The oscillator's Scope lane as Listen hears it (`listen::sync`).
fn lane() -> TapVoice {
    TapVoice { net: "OSC".into(), reference: None, gain: LISTEN_GAIN }
}

/// A tap of `voice` as Listen makes it.
fn lane_spec(voice: &TapVoice) -> TapSpec {
    TapSpec { voices: vec![voice.clone()], latency: LISTEN_LATENCY, dc_block_hz: LISTEN_DC_HZ, mix: TapMix::new(1) }
}

/// The tone box's bench in a host for the held-take tests, its run begun with the key pressed:
/// the take [`WINDOW`] long, snapshots a millisecond of stepping apart at most (a share of it
/// they may take: the take keeps snapshots inside it on any box, so an edit late in it goes back
/// to one there), a recorder of the oscillator's lane alone from the run's start ([`ALONE`]: what
/// a Listen there from the start records), and with `paced` the run paced from its start
/// (`ears::pace`), as a board too heavy to keep up renders; with no `take`, a run with no take end
/// (Continuous), its recorder [`WINDOW`] long; with `gate`, an [`EndGate`] among the sinks before
/// the hearing recorder. The host, the views' state and the pitch knob.
fn tone_box(exact: bool, paced: bool, take: Option<f64>, start: Instant, gate: Option<EndGate>) -> (SimHost, SimLive, CompRef) {
    let mode = if exact { "Exact" } else { "Fast" };
    let sources = fab_elec_fixtures::docs("tonebox").expect("the tone box is a stock project");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("the tone box evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "box").expect("the tone box has its bench");
    let design = Arc::new(ev.design.clone());
    let unit = design.unit(bench.scope_id()).expect("the bench runs the board");
    let comp = |path: &str| CompRef { board: unit.id, comp: unit.circuit.comp(path).unwrap_or_else(|| panic!("the tone box has {path}")).id };
    let (key, pitch) = (comp("key"), comp("pitch"));
    let mut host = SimHost::with_spawn(Box::new(Threads));
    let snapshots = Snapshots { every: Duration::from_millis(1), share: 0.5, ..Snapshots::default() };
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, snapshots, ..LiveSpec::default() });
    host.set_take_len(take);
    let take = host.take_len().unwrap_or(WINDOW);
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    assert!(host.command(&SimCmd::SetSimMode(if exact { SimMode::Exact } else { SimMode::Fast })), "{mode}: the host takes the solver");
    let mut live = SimLive::default();
    let bus = live.listen_bus();
    if let Some(gate) = gate {
        host.act(&SimAction::ShowScope { id: GATE, sink: SinkSlot::new(Box::new(gate)), column: 0.0 });
    }
    let alone = AudioTap::recorder(lane_spec(&lane()), bus.ring.clone(), bus.record(ALONE, ALONE, take));
    host.act(&SimAction::ShowScope { id: ALONE, sink: SinkSlot::new(Box::new(alone)), column: 0.0 });
    if paced {
        ears::pace(&mut host);
    }
    host.commit(&design);
    step(&mut host, &mut live, start, "the box runs", |h| h.values().is_some());
    host.act(&SimAction::Cmd(SimCmd::press(key, true)));
    (host, live, pitch)
}

/// A Listen while the take renders (slice 3): at half the take, a Listen of the oscillator's lane
/// follows the hearing recorder's tape with no run again, its reason never said: the run
/// neither goes back nor stops, and at its end the Listen has the whole take, its chunks once
/// each in order from the take's first point, bit for bit what the oscillator alone recorder
/// taped from the start.
fn listen_rendering(exact: bool) {
    let mode = if exact { "Exact" } else { "Fast" };
    let start = Instant::now();
    let (mut host, mut live, _) = tone_box(exact, true, Some(WINDOW), start, None);
    let bus = live.listen_bus();
    let take = host.take_len().expect("the take has a length");
    step(&mut host, &mut live, start, "half the take renders", |h| h.values().is_some_and(|v| v.t > 0.5 * take));
    let (rewinds, joined) = (host.store().rewinds, host.values().map_or(0.0, |v| v.t));
    assert!(joined < 0.9 * take && host.values().is_some_and(|v| v.running), "{mode}: the Listen joins while the take renders, at {joined} s");
    let mut ears = Ears::listen(&mut host, &mut live, lane(), LISTEN_DC_HZ, take, true);
    let rate = host.content().expect("the run is keyed").recording_rate;
    let frames = window_frames(take, rate) as u64;
    let chunks = frames as usize / CHUNK;
    let got = |e: &Ears| e.tee.as_ref().expect("the Listen is tapped").got.clone();
    let rests = |h: &SimHost| h.values().is_some_and(|v| (v.t - take).abs() < 1e-9 && !v.running);
    ears.until(&mut host, &mut live, start, "the take renders to its end, the Listen following", |h, e| {
        assert_eq!(h.status().rerender_reason, None, "{mode}: a Listen while the take renders renders nothing again");
        rests(h) && got(e).len() >= chunks
    });
    assert_eq!(host.store().rewinds, rewinds, "{mode}: the run went back nowhere");
    let held = got(&ears);
    assert!(held.len() == chunks && held.iter().enumerate().all(|(i, c)| c.0 == held[0].0 && c.1 == i as u64), "{mode}: the take's {chunks} chunks in order, once: {:?}", held.iter().map(|c| (c.0, c.1)).collect::<Vec<_>>());
    settle_tapes();
    let alone = bus.tapes.current(ALONE).expect("the oscillator alone is taped");
    let mut want = vec![0.0f32; frames as usize];
    assert_eq!(alone.read(0, &mut want).expect("the take reads back"), frames as usize, "{mode}: the oscillator alone is taped whole");
    assert_eq!(held[0].2, alone.t0, "{mode}: the take starts at its first point's time");
    let heard: Vec<f32> = held.iter().flat_map(|c| c.3.iter().copied()).collect();
    let differ = heard.iter().zip(&want).position(|(a, b)| a.to_bits() != b.to_bits());
    assert_eq!(differ, None, "{mode}: the Listen's take is bit for bit what the oscillator alone taped (joined at {joined:.3} s)");
    host.act(&SimAction::Cmd(SimCmd::Stop));
}

/// A Listen in a run with no take end (Continuous) records the run itself: its windows go on past
/// the first, where the hearing recorder's tape has no take to follow.
fn listen_continuous() {
    let start = Instant::now();
    let (mut host, mut live, _) = tone_box(false, false, None, start, None);
    assert!(host.take_len().is_none() && host.status().continuous, "a bench without a take runs Continuous");
    step(&mut host, &mut live, start, "the run goes past a window", |h| h.values().is_some_and(|v| v.t > WINDOW));
    let mut ears = Ears::listen(&mut host, &mut live, lane(), LISTEN_DC_HZ, WINDOW, true);
    ears.until(&mut host, &mut live, start, "the Listen hears the run past its first window", |h, e| {
        let window = h.content().map_or(u64::MAX, |c| (window_frames(WINDOW, c.recording_rate) / CHUNK) as u64);
        e.tee.as_ref().is_some_and(|tee| tee.got.iter().any(|c| c.1 >= window))
    });
    host.act(&SimAction::Cmd(SimCmd::Stop));
}

/// A Listen while the take renders follows the hearing recorder's tape: no run again. In Fast
/// and in Exact. In Continuous it records the run itself.
#[test]
fn a_listen_while_the_take_renders_follows_its_recording_with_no_run_again() {
    for exact in [false, true] {
        listen_rendering(exact);
    }
    listen_continuous();
}

fn listen_held(exact: bool) {
    let mode = if exact { "Exact" } else { "Fast" };
    let start = Instant::now();
    let (mut host, mut live, pitch) = tone_box(exact, false, Some(WINDOW), start, None);
    let bus = live.listen_bus();
    let take = host.take_len().expect("the take has a length");
    let (osc, spec) = (lane(), lane_spec);
    let rests = |h: &SimHost| h.values().is_some_and(|v| (v.t - take).abs() < 1e-9 && !v.running);
    step(&mut host, &mut live, start, "the take renders to its end", rests);
    // the bench's probes (OUT, OSC, QUIET) are kept whole from the run's start with no view
    // asking: the run starts with them, so a Scope opened later finds its lanes whole however far
    // the unpaced run's first slice went
    let store = host.store();
    let kept: Vec<usize> = host.whole().iter().enumerate().filter(|w| *w.1).map(|w| w.0).collect();
    let absent = kept.iter().find_map(|&row| (0..store.len()).find(|&i| store.signals[row].at(i).is_nan()).map(|i| (row, store.time.at(i))));
    assert!(kept.len() >= 3 && absent.is_none(), "{mode}: the bench's probes are kept whole from the run's start (rows {kept:?}; absent at {absent:?})");

    // a value edit where the silent clock holds: the run goes back there and renders on
    let rewinds = host.store().rewinds;
    assert!(host.command(&SimCmd::Seek(take / 2.0)), "{mode}: a seek holds the silent clock");
    assert!(host.command(&SimCmd::SetParam { comp: pitch, value: 1.0 }), "{mode}: the knob is an edit");
    step(&mut host, &mut live, start, "the edit sends the run back and it renders to its end again", |h| h.store().rewinds > rewinds && rests(h));
    // the take's state, not the clock: the run can rest at the take's end before the hearing
    // recorder has queued its last chunk, and a Listen asked between the two finds the tape short;
    // the recorder has queued the whole take (`Tape::promised`) before the next Listen is asked
    let rate = host.content().expect("the run is keyed").recording_rate;
    let frames = window_frames(take, rate) as u64;
    let promised = || bus.tapes.current(JACK_TAP).map_or(0, |t| t.promised());
    step(&mut host, &mut live, start, "the hearing recorder has the whole take queued", |h| rests(h) && promised() >= frames);

    // a voice the hearing recorder does not tape: the take is recorded again, and the bar says why;
    // paced from here (`ears::pace`), as a board too heavy to keep up renders
    ears::pace(&mut host);
    let other = TapVoice { gain: EAR_GAIN, ..osc.clone() };
    let rewinds = host.store().rewinds;
    host.act(&SimAction::Listen { id: OTHER, sink: SinkSlot::new(Box::new(AudioTap::recorder(spec(&other), bus.ring.clone(), bus.record(OTHER, OTHER, take)))) });
    step(&mut host, &mut live, start, "the take renders again for the Listen", |h| h.status().rerender_reason.is_some());
    assert_eq!(host.status().rerender_reason.as_deref(), Some("voice not recorded in this take (OSC)"), "{mode}: the status says why the take renders again");
    step(&mut host, &mut live, start, "the take is recorded again", |h| h.store().rewinds > rewinds && rests(h));
    assert_eq!(host.status().rerender_reason, None, "{mode}: the reason goes once the take is whole");
    host.act(&SimAction::Unlisten { id: OTHER });

    // the tapes of the take at rest: the hearing recorder's is the take's length, with the lane's voice
    step(&mut host, &mut live, start, "the hearing recorder has the take queued again", |h| rests(h) && promised() >= frames);
    settle_tapes();
    let hearing = bus.tapes.current(JACK_TAP).expect("the hearing recorder tapes the take");
    assert_eq!(hearing.frames(), frames, "{mode}: the hearing recorder's tape is the take's length");
    assert!(hearing.voices > 1 && hearing.keys.contains(&voice_key(&osc, LISTEN_DC_HZ)), "{mode}: the hearing recorder tapes the jack and the probes, the oscillator's lane among them");
    let alone = bus.tapes.current(ALONE).expect("the oscillator alone is taped");
    let mut want = vec![0.0f32; frames as usize];
    assert_eq!(alone.read(0, &mut want).expect("the take reads back"), frames as usize, "{mode}: the oscillator alone is taped whole");
    let (low, high) = want.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    assert!(high - low > 0.1, "{mode}: the oscillator swings in the take: {low} to {high}");

    // the Listen of the lane over the take at rest
    let (rewinds, t) = (host.store().rewinds, host.values().map(|v| v.t));
    let asked = Instant::now();
    let mut ears = Ears::listen(&mut host, &mut live, osc.clone(), LISTEN_DC_HZ, take, true);
    let chunks = frames as usize / CHUNK;
    let got = |e: &Ears| e.tee.as_ref().expect("the Listen is tapped").got.clone();
    ears.until(&mut host, &mut live, start, "the Listen has the whole take", |_, e| got(e).len() >= chunks);
    let whole = asked.elapsed();
    assert_eq!(host.status().rerender_reason, None, "{mode}: a Listen that plays the recording renders nothing again");
    assert_eq!((host.store().rewinds, host.values().map(|v| v.t)), (rewinds, t), "{mode}: the run neither goes back nor moves");
    let held = got(&ears);
    let n = held[0].0;
    assert!(held.len() == chunks && held.iter().enumerate().all(|(i, c)| c.0 == n && c.1 == i as u64), "{mode}: the take's {chunks} chunks in order, once: {:?}", held.iter().map(|c| (c.0, c.1)).collect::<Vec<_>>());
    assert_eq!(held[0].2, alone.t0, "{mode}: the take starts at its first point's time");
    let heard: Vec<f32> = held.iter().flat_map(|c| c.3.iter().copied()).collect();
    let differ = heard.iter().zip(&want).position(|(a, b)| a.to_bits() != b.to_bits());
    assert_eq!(differ, None, "{mode}: the Listen's take is bit for bit what the oscillator alone taped");
    eprintln!("{mode}: a Listen over the take at rest had the whole {take:.3} s ({chunks} chunks) in {:.1} ms", whole.as_secs_f64() * 1e3);

    // F5: heard late in the take, past its snapshot inside, a value edit goes in where it is
    // heard, the tape writer held: the run goes back to the snapshot and renders the take again,
    // and nothing reaches the player while the recorder's cut waits
    ears.until(&mut host, &mut live, start, "the listener hears the take late in it", |_, e| e.bus.ring.plays_recording() && e.heard_at().is_some_and(|t| t > 0.8 * take));
    let (rewinds, before) = (host.store().rewinds, got(&ears).len());
    let hold = hold_tapes();
    assert!(host.command(&SimCmd::SetParam { comp: pitch, value: 0.0 }), "{mode}: the knob is an edit");
    let edited = host.heard_edit().expect("the edit is heard").1;
    ears.until(&mut host, &mut live, start, "the edit renders the take again to its end", |h, _| h.store().rewinds > rewinds && rests(h));
    let early: Vec<_> = got(&ears)[before..].iter().map(|c| (c.0, c.1, c.2)).collect();
    let now_hearing = bus.tapes.current(JACK_TAP).expect("the hearing recorder tapes");
    assert!(early.is_empty(), "{mode}: nothing reaches the player while the cut waits for the writer (the edit at {edited} s): {early:?}; the hearing tape the same {}, frames {} promised {} settled {}, rewinds {} -> {}",
        Arc::ptr_eq(&now_hearing, &hearing), now_hearing.frames(), now_hearing.promised(), now_hearing.settled(), rewinds, host.store().rewinds);
    // the writer goes on: the chunks from the snapshot go again as the recorder tapes them
    drop(hold);
    let last = chunks as u64 - 1;
    ears.until(&mut host, &mut live, start, "the Listen has the take again", |_, e| got(e)[before..].iter().any(|c| c.1 == last));
    let again = got(&ears).split_off(before);
    let from = again[0].1;
    assert!(again.iter().enumerate().all(|(i, c)| c.0 == n && c.1 == from + i as u64) && from > 0 && again[0].2 <= edited,
        "{mode}: the take's chunks go again under its number, in order, from past its start to the edit at {edited} s: {:?}", again.iter().map(|c| (c.0, c.1, c.2)).collect::<Vec<_>>());
    settle_tapes();
    let mut now = vec![0.0f32; frames as usize];
    let alone = bus.tapes.current(ALONE).expect("the oscillator alone is taped");
    assert_eq!(alone.read(0, &mut now).expect("the take reads back"), frames as usize, "{mode}: the oscillator alone is taped whole again");
    let k = from as usize * CHUNK;
    let stale = again.iter().find(|c| c.3.iter().zip(&now[c.1 as usize * CHUNK..]).any(|(a, b)| a.to_bits() != b.to_bits())).map(|c| c.1);
    assert_eq!(stale, None, "{mode}: each chunk sent again is what the oscillator alone recorder taped anew");
    assert!(now[..k].iter().zip(&want[..k]).all(|(a, b)| a.to_bits() == b.to_bits()), "{mode}: the take before the chunks sent again is as it was");
    assert!(now[k..].iter().zip(&want[k..]).any(|(a, b)| a.to_bits() != b.to_bits()), "{mode}: the edit changed the take after them");
    assert_eq!(host.store().rewinds, rewinds + 1, "{mode}: one rewind, to the snapshot, and no restart");
    assert!(host.heard_edit().is_some(), "{mode}: the edit on the bar");
    host.act(&SimAction::Cmd(SimCmd::Stop));
}

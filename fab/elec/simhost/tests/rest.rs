use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::SimCmd;
use fab_elec_core::ids::{CompRef, NetRef};
use fab_elec_fixtures::ledctl;
use fab_elec_sim::livesim::LiveSpec;
use fab_elec_simhost::{SimHost, Spawn};
use fab_elec_view::live::sim::{SimAction, SimLive, SinkSlot};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "support/helped.rs"]
mod helped;
use helped::Helped;

/// The test's threads: one std thread per worker and per job.
struct Threads(Arc<AtomicUsize>);

impl Spawn for Threads {
    fn worker(&self, _name: &'static str, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
    fn job(&self, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
    fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        let wakes = self.0.clone();
        Arc::new(move || {
            wakes.fetch_add(1, Ordering::Relaxed);
        })
    }
}

const PHASE_FRAMES: u32 = 50_000;

/// Frames at 2 ms until `done` holds, counting calls that put something new into the views.
/// The bound is the frames the test drives, not wall time: a busy box makes each frame wait
/// longer, and only a worker that stopped making progress (a hang) runs out of them.
fn frames(host: &mut SimHost, live: &mut SimLive, start: Instant, what: &str, mut done: impl FnMut(&SimHost, usize) -> bool) -> usize {
    let mut changed = 0;
    for _ in 0..PHASE_FRAMES {
        if host.step(start.elapsed().as_secs_f64(), live) {
            changed += 1;
        }
        if done(host, changed) {
            return changed;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("{what}: not reached within {PHASE_FRAMES} frames");
}

/// The time of the newest trace point the host has taken in.
fn trace_end(host: &SimHost) -> Option<f64> {
    let time = &host.store().time;
    time.len().checked_sub(1).map(|i| time.at(i))
}

#[test]
fn a_settled_bench_stops_publishing_a_pwm_keeps_on_and_its_pause_shows_the_instant() {
    let start = Instant::now();
    let wakes = Arc::new(AtomicUsize::new(0));
    let mut host = SimHost::with_spawn(Box::new(Threads(wakes.clone())));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(ledctl_bench(10e3, [0.0; 8]));
    let design = Arc::new(ledctl());
    host.commit(&design);
    let mut live = SimLive::default();
    // settled, and the host's trace holds every point up to the settled values: the worker has
    // nothing left to send
    frames(&mut host, &mut live, start, "the DC bench settles", |h, _| {
        h.settled() && h.values().is_some_and(|v| trace_end(h) == Some(v.t))
    });
    assert!(wakes.load(Ordering::Relaxed) > 0, "the live worker wakes its host after publishing");
    let settled_t = host.values().expect("a settled bench has published values").t;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(2));
        assert!(!host.step(start.elapsed().as_secs_f64(), &mut live),
            "a settled bench publishes nothing and needs no more view frames");
        assert_eq!(host.values().expect("a settled bench keeps its last values").t, settled_t,
            "a settled bench publishes nothing: its last values' time stays unchanged");
    }

    host.set_bench(ledctl_bench(10e3, [0.3; 8]));
    host.commit(&design);
    const PERIOD: f64 = 1e-4;
    let changed = frames(&mut host, &mut live, start, "the PWM bench keeps publishing", |h, changed| {
        let Some(values) = h.values() else { return false };
        // The last DC values remain visible until the stimulus retune reaches the worker.
        if values.t <= settled_t {
            return false;
        }
        assert!(!h.settled(), "a PWM bench does not settle while its pins keep switching");
        // An unbounded run may pass five periods in one publication; also observe repeated updates.
        values.t > settled_t + 5.0 * PERIOD && changed > 5
    });
    assert!(changed > 5, "a PWM bench keeps publishing new values or traces for the views");

    // paused, the rows show the voltages at the pause, the instant the device's state and the scopes
    // show: a lamp's PWM net stands high or low, where the eye's mean over its periods reads neither
    assert!(host.command(&SimCmd::Pause), "Pause reaches the live bench");
    frames(&mut host, &mut live, start, "the pause is published", |h, _| h.values().is_some_and(|v| !v.running));
    let v = host.values().expect("the pause is published").clone();
    let off: Vec<(usize, f32, f32)> = (0..host.nets().nets())
        .filter(|&n| v.net_v.get(n).is_some_and(|x| x.is_finite()) && host.nets().row(n)[1] != v.net_v[n])
        .map(|n| (n, host.nets().row(n)[1], v.net_v[n]))
        .collect();
    assert!(off.is_empty(), "paused, every net's row holds its voltage at the pause (net, row, at t): {off:?}");
}

/// Play on a held take nobody hears plays it back by the host's silent clock, as the player does
/// with sound: the head the views follow (`SimLive::heard`) moves with the frames' clock at one
/// second a second, comes round at the take's end, and no run starts (the take is not simulated
/// again). Pause holds the head where it is; a seek moves it and Play goes on from there.
#[test]
fn play_on_a_held_take_nobody_hears_moves_the_head_at_real_time_round_the_take_and_runs_nothing() {
    const TAKE: f64 = 0.02;
    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads(Arc::new(AtomicUsize::new(0)))));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_take_len(Some(TAKE));
    host.set_bench(ledctl_bench(10e3, [0.3; 8]));
    host.commit(&Arc::new(ledctl()));
    let mut live = SimLive::default();
    let take = host.take_len().expect("the bench has a take");
    frames(&mut host, &mut live, start, "the take renders to its end and rests", |h, _| {
        h.values().is_some_and(|v| !v.running && (v.t - take).abs() < 1e-9)
    });
    let rests = |h: &SimHost| h.values().is_some_and(|v| !v.running && (v.t - take).abs() < 1e-9);
    let (run, rewinds) = (host.live_run(), host.store().rewinds);
    assert!(!host.status().playing && !host.status().plays_back && live.heard.is_none(), "a rested take nobody hears shows the run's end");

    assert!(host.command(&SimCmd::Play), "Play is taken");
    let (a, b) = live.record;
    let mut last: Option<(f64, f64)> = None;
    let mut wraps = 0;
    for _ in 0..PHASE_FRAMES {
        let now = start.elapsed().as_secs_f64();
        host.step(now, &mut live);
        let (t, moving) = live.heard.expect("Play plays the held take back");
        assert!(moving && host.status().playing && host.status().plays_back, "the take plays back, moving");
        assert!(rests(&host) && host.live_run() == run && host.store().rewinds == rewinds, "Play starts no run: the run rests at the take's end");
        match last {
            None => assert!((t - a).abs() < 1e-9, "Play at the take's end plays from its start: {t} against {a}"),
            Some((then, from)) => {
                let on = from + (now - then);
                let want = if on < b { on } else { a + (on - a) % (b - a) };
                assert!((t - want).abs() < 1e-9, "the head moves at one second a second, round [{a}, {b}): {t} against {want}");
                wraps += usize::from(on >= b);
            }
        }
        last = Some((now, t));
        if wraps >= 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(wraps >= 2, "the head comes round at the take's end");

    assert!(host.command(&SimCmd::Pause), "Pause is taken");
    host.step(start.elapsed().as_secs_f64(), &mut live);
    let held = live.heard.expect("paused, the take is held at the head");
    std::thread::sleep(Duration::from_millis(5));
    host.step(start.elapsed().as_secs_f64(), &mut live);
    assert!(live.heard == Some((held.0, false)) && !host.status().playing && host.status().plays_back, "Pause holds the head: {:?} against {held:?}", live.heard);
    let mid = a + (b - a) / 2.0;
    assert!(host.command(&SimCmd::Seek(mid)), "a seek is taken");
    host.step(start.elapsed().as_secs_f64(), &mut live);
    assert_eq!(live.heard, Some((mid, false)), "a seek moves the held head");
    assert!(host.command(&SimCmd::Play), "Play is taken again");
    host.step(start.elapsed().as_secs_f64(), &mut live);
    assert_eq!(live.heard, Some((mid, true)), "Play goes on from the head");
    assert!(rests(&host) && host.live_run() == run && host.store().rewinds == rewinds, "nothing of this starts a run");
}

/// The pitch of the rising crossings of `level` in a trace over `[from, to)`: (time, volts)
/// points, linear between them. None with fewer than four crossings or an absent stretch.
fn trace_pitch(points: &[(f64, f32)], from: f64, to: f64, level: f32) -> Option<f64> {
    let inside: Vec<(f64, f32)> = points.iter().copied().filter(|p| p.0 >= from && p.0 < to).collect();
    if inside.iter().any(|p| p.1.is_nan()) {
        return None;
    }
    let rises: Vec<f64> = inside.windows(2)
        .filter(|w| w[0].1 <= level && w[1].1 > level)
        .map(|w| w[0].0 + (w[1].0 - w[0].0) * f64::from((level - w[0].1) / (w[1].1 - w[0].1)))
        .collect();
    (rises.len() >= 4).then(|| (rises.len() - 1) as f64 / (rises[rises.len() - 1] - rises[0]))
}

/// A knob turned while a held take plays back with nobody listening goes in at the play head, as
/// it does where a listener hears the recording: the tone box's take (the key held from its
/// start) played round by the silent clock, its PITCH knob turned to the top mid-take. The edit is
/// taken at the clock's head; the run goes back there and renders the take again while the clock
/// plays on; and the take carries the turn from the head: its oscillator at the RC formula's pitch
/// at mid travel before it and at the top after it. The knob's face shows the turn from the head
/// on and keeps it, the design's place before the head once the clock comes round (the take there
/// is as it was).
#[test]
fn a_knob_turned_while_a_take_plays_back_with_nobody_listening_goes_in_at_the_play_head() {
    const TAKE: f64 = 0.12;
    const SIDE: f64 = 0.035;
    let sources = fab_elec_fixtures::docs("tonebox").expect("the tone box is a stock project");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("the tone box evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "box").expect("the tone box has its bench");
    let design = Arc::new(ev.design.clone());
    let unit = design.unit(bench.scope_id()).expect("the bench runs the board");
    let comp = |path: &str| CompRef { board: unit.id, comp: unit.circuit.comp(path).unwrap_or_else(|| panic!("the tone box has {path}")).id };
    let (key, pitch) = (comp("key"), comp("pitch"));
    let osc = NetRef { board: unit.id, net: unit.circuit.net("OSC").expect("the tone box has OSC").id };
    let libs = fab_elec_bench::ModelLibs::generic();
    let formula = |pos: f64| {
        let r = 100e3 + fab_elec_bench::input::Taper::Linear.halves(250e3, pos, libs.pot_end).1;
        let (vp, vm, vdd) = (libs.schmitt.vt_plus, libs.schmitt.vt_minus, libs.schmitt.vdd);
        1.0 / (r * 10e-9 * ((vp * (vdd - vm)) / (vm * (vdd - vp))).ln())
    };

    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads(Arc::new(AtomicUsize::new(0)))));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_take_len(Some(TAKE));
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    host.commit(&design);
    let mut live = SimLive::default();
    frames(&mut host, &mut live, start, "the box runs", |h, _| h.values().is_some());
    // nothing plays back yet: the key goes down at the run's front and stays down
    assert!(host.command(&SimCmd::press(key, true)), "the key is a press");
    let take = host.take_len().expect("the bench has a take");
    let rests = |h: &SimHost| h.values().is_some_and(|v| !v.running && (v.t - take).abs() < 1e-9);
    frames(&mut host, &mut live, start, "the take renders to its end and rests", |h, _| rests(h));
    // the key goes down again at the take's start, where the silent clock is set: the run goes
    // back there and renders the take with the key held throughout. (The press at the run's front
    // lands wherever the free run is when it comes: on a fast box the run can rest at the take's
    // end before the first frame sees it, and a press there waits for the next run.)
    let rewinds = host.store().rewinds;
    assert!(host.command(&SimCmd::Seek(0.0)), "a seek sets the silent clock at the take's start");
    assert!(host.command(&SimCmd::press(key, true)), "the key is pressed again at the take's start");
    frames(&mut host, &mut live, start, "the take renders again with the key held from its start", |h, _| h.store().rewinds > rewinds && rests(h));

    // the silent clock plays the take round; the knob turned once its head is mid-take
    assert!(host.command(&SimCmd::Play), "Play is taken");
    let mut head = None;
    for _ in 0..PHASE_FRAMES {
        host.step(start.elapsed().as_secs_f64(), &mut live);
        if let Some((t, true)) = live.heard.filter(|h| (0.5 * take..0.7 * take).contains(&h.0)) {
            head = Some(t);
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let head = head.expect("the silent clock plays the take back mid-take");
    let rewinds = host.store().rewinds;
    assert!(host.command(&SimCmd::SetParam { comp: pitch, value: 1.0 }), "a knob's turn is taken");
    assert_eq!(host.heard_edit().map(|e| e.1), Some(head), "the turn goes in at the silent clock's head: {:?}", host.heard_edit());
    let faces = |live: &SimLive, t: f64| {
        let want: &[(CompRef, f64)] = if t >= head { &[(pitch, 1.0)] } else { &[] };
        assert_eq!(&*live.turned, want, "the knob's face at {t:.4} s (turned at {head:.4} s) shows the take there");
    };

    // the run goes back to the head and renders the take again while the clock plays on
    let mut rendered = false;
    for _ in 0..PHASE_FRAMES {
        host.step(start.elapsed().as_secs_f64(), &mut live);
        let (t, moving) = live.heard.expect("the clock plays on while the take renders again");
        assert!(moving && host.status().playing && host.status().plays_back, "the take plays back, moving, while it renders again");
        faces(&live, t);
        if host.store().rewinds > rewinds && rests(&host) {
            rendered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(rendered, "the run goes back to the turn and renders the take to its end again");

    // the take carries the turn from the head on, the stretch before it as it was
    let row = host.index().net(osc).expect("OSC is lowered");
    let store = host.store();
    let points: Vec<(f64, f32)> = (0..store.len()).map(|i| (store.time.at(i), store.signals[row].at(i))).collect();
    // what the trace holds where it has no tone: its points there, its span, where the tone starts
    let pitch_of = |from: f64, to: f64| trace_pitch(&points, from, to, 2.5).unwrap_or_else(|| {
        let inside: Vec<&(f64, f32)> = points.iter().filter(|p| p.0 >= from && p.0 < to).collect();
        let nan = inside.iter().filter(|p| p.1.is_nan()).count();
        let onset = points.windows(2).find(|w| w[0].1 <= 2.5 && w[1].1 > 2.5).map(|w| w[0].0);
        let span = (points.first().map_or(f64::NAN, |p| p.0), points.last().map_or(f64::NAN, |p| p.0));
        panic!("the take holds a tone over [{from:.4}, {to:.4}) s: {} points there ({nan} NaN), the trace over [{:.4}, {:.4}] s with its first rise at {onset:?} s, {} rewinds, the head at {head:.4} s", inside.len(), span.0, span.1, store.rewinds)
    });
    let (mid, top) = (pitch_of(head - SIDE, head - 2e-3), pitch_of(head + 3e-3, head + SIDE));
    let (want_mid, want_top) = (formula(0.5), formula(1.0));
    assert!((mid / want_mid - 1.0).abs() < 0.02, "before the head the take plays {mid:.1} Hz, the formula's {want_mid:.1} Hz at mid travel within 2 %");
    assert!((top / want_top - 1.0).abs() < 0.02, "from the head the take plays {top:.1} Hz, the formula's {want_top:.1} Hz at the top within 2 %");

    // round the take again: the face keeps the turn from the head, the design's place before it
    let (mut wrapped, mut past) = (false, false);
    let mut last = head;
    for _ in 0..PHASE_FRAMES {
        host.step(start.elapsed().as_secs_f64(), &mut live);
        let (t, moving) = live.heard.expect("the take plays round");
        assert!(moving, "the take plays round, moving");
        faces(&live, t);
        wrapped |= t < last;
        past = wrapped && t > head;
        last = t;
        if past {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(past, "the clock comes round past the turn");
    assert_eq!(host.status().waiting, 0, "with auto re-record on nothing waits for a record");
}

/// The next run of a circuit starts in rounds after either the host's meter asked for them or
/// the heard run chose them at its start. The organ plays a short take between host frames, so
/// only its own set-up can make that choice. Its next run has the choice before set-up finishes.
#[test]
fn a_run_of_a_circuit_whose_last_run_went_in_rounds_starts_in_them() {
    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Helped));
    // a step this short keeps the run far below real time, the solver its bound
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, max_step: 1e-8, ..LiveSpec::default() });
    host.set_bench(ledctl_bench(10e3, [0.3; 8]));
    let design = Arc::new(ledctl());
    host.commit(&design);
    let mut live = SimLive::default();
    frames(&mut host, &mut live, start, "the run falls behind and goes on in rounds", |h, _| h.rounds() == Some(true));
    let run = host.live_run();
    assert!(host.command(&SimCmd::Stop), "Stop is taken");
    assert!(host.command(&SimCmd::Play), "Play after Stop runs the circuit again");
    // the app commits its design each frame: the stopped run's lowering is asked again
    host.commit(&design);
    frames(&mut host, &mut live, start, "the circuit runs again", |h, _| h.live_run().is_some_and(|r| Some(r) != run));
    assert_eq!(host.rounds(), Some(true), "the new run of the same circuit starts in rounds, with no meter window stepped whole");
    drop(host);

    struct TakeEnd(std::sync::mpsc::Sender<()>);
    impl fab_elec_sim::Sink for TakeEnd {
        fn ended(&mut self, _at: f64) {
            let _ = self.0.send(());
        }
    }

    let sources = fab_elec_fixtures::docs("organ8").expect("organ8 is a stock project");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("the organ evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "organ").expect("the organ has its bench");
    let design = Arc::new(ev.design.clone());
    let mut host = SimHost::with_spawn(Box::new(Helped));
    // A finite pace makes the run heard. Its eight voices split at 48 kHz, and their set-up
    // on one stepper falls short of the requested thousand simulated seconds per wall second.
    host.set_live_spec(LiveSpec { rate: 1_000.0, ..LiveSpec::default() });
    host.set_sample_rate(Some(48_000.0));
    host.set_take_len(Some(0.01));
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    let (end_tx, end_rx) = std::sync::mpsc::channel();
    host.act(&SimAction::ShowScope { id: 1, sink: SinkSlot::new(Box::new(TakeEnd(end_tx))), column: 400e-6 });
    let mut live = SimLive::default();
    for first in [true, false] {
        if first {
            assert!(host.command(&SimCmd::Pause), "the organ can start paused for the choice at zero");
        } else {
            assert!(host.command(&SimCmd::Stop), "Stop ends the organ's first run");
            assert!(host.command(&SimCmd::Step(1.0 / 48_000.0)), "one sample starts the next run and pauses it before its set-up decision");
        }
        host.commit(&design);
        frames(&mut host, &mut live, start, "the organ publishes its paused set-up", |h, _| {
            h.values().is_some_and(|v| !v.running && v.preparing && (first || v.t > 0.0))
        });
        let status = host.status();
        assert_eq!(status.preparing.map(|p| p.first), Some(first), "only the first run of this board is a first play");
        assert_eq!((status.limited_by, status.split), (None, None), "set-up reports no stepping limit or shares");
        if first {
            assert_eq!(host.rounds(), None, "the first run starts with the host's choice undecided");
        } else {
            assert_eq!(host.rounds(), Some(true), "the host remembered this circuit's key before its next run could choose rounds");
        }
        assert!(host.command(&SimCmd::Play), "Play releases the paused set-up");
        let sent = host.sent_mark();
        // Leave the host's meter idle until after the worker has chosen and rendered the take.
        end_rx.recv_timeout(Duration::from_secs(100)).expect("the organ reaches the end of its take without a host frame");
        frames(&mut host, &mut live, start, "the organ publishes the end of its take", |h, _| {
            h.values().is_some_and(|v| !v.running && !v.preparing && v.t > 0.0)
        });
        assert!(host.values().expect("the completed take has values").in_rounds, "the organ actually rendered in rounds (first play: {first})");
        assert_eq!(host.rounds(), Some(true), "the host learned the organ's own choice from the start publication");
        assert_eq!(host.sent_mark(), sent, "learning the run's choice sends no command back to the worker");
        assert!(host.status().preparing.is_none(), "the preparing status clears once the run starts");
    }
}

use fab_elec_bench::bench::ScopeProbe;
use fab_elec_bench::expect::Probe;
use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::input::LedColor;
use fab_elec_bench::{bench_spec, ModelLibs};
use fab_elec_core::ids::{BoardId, CompRef, NetRef, PinRef, BENCH_SCOPE_BASE};
use fab_elec_fixtures::ledctl;
use fab_elec_design::{CursorSource, Design, TimeCursor, Tip};
use fab_elec_lang::sys_sim::{SimMode, SimSettings};
use fab_elec_lang::BenchScope;
use fab_elec_sim::analysis::Sink;
use fab_elec_sim::Options;
use fab_elec_sim::livesim::{LiveSpec, AUDIO_SAMPLE_RATE, FAST_RELTOL, REALTIME_RECORDING_RATE, REALTIME_SAMPLE_RATE};
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_bench::SimCmd;
use fab_elec_bench::audio::AudioLoad;
use fab_elec_bench::models::LED_RATED_A;
use fab_elec_sim::live::LiveValues;
use fab_elec_sim::listen::{voice_key, AudioTap, SampleRing, TapMix, TapSpec, TapVoice};
use fab_elec_simhost::events::EventSource;
use fab_elec_simhost::host::CoSimMaker;
use fab_elec_simhost::take::Means;
use fab_elec_simhost::{Limit, SimHost, Spawn};
use fab_elec_view::live::listen::{JACK_TAP, LISTEN_DC_HZ, LISTEN_GAIN};
use fab_elec_view::live::mini::{MINI_COLS, MINI_EMPTY, MINI_RING, MINI_SLOT};
use fab_elec_view::live::segs::{SegKey, TreeIn, TreeOwner};
use fab_elec_view::live::sim::{SimAction, SimLive, SinkSlot};
use fab_elec_view::live::emission_attr;
use fab_elec_view::ViewAction;
use makepad_widgets_core::ActionTrait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[path = "support/helped.rs"]
mod helped;
use helped::Helped;

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

struct Counting(Arc<AtomicUsize>);

impl Sink for Counting {
    fn point(&mut self, _at: f64, _outer: Option<f64>, _x: &[f64]) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// The times of the points a scope's sink gets.
struct Times(Arc<Mutex<Vec<f64>>>);

impl Sink for Times {
    fn point(&mut self, at: f64, _outer: Option<f64>, _x: &[f64]) {
        if let Ok(mut times) = self.0.lock() {
            times.push(at);
        }
    }
}

fn times_len(times: &Mutex<Vec<f64>>) -> usize {
    times.lock().map_or(0, |t| t.len())
}

const PHASE_FRAMES: u32 = 50_000;

/// Frames at 2 ms until `done` holds. The bound is the frames the test drives, not wall time: a
/// busy box makes each frame wait longer, and only a worker that stopped making progress (a
/// hang) runs out of them; a phase here takes a few hundred.
fn frames(host: &mut SimHost, live: &mut SimLive, start: Instant, what: &str, mut done: impl FnMut(&SimHost) -> bool) {
    for _ in 0..PHASE_FRAMES {
        host.step(start.elapsed().as_secs_f64(), live);
        if done(host) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("{what}: not reached within {PHASE_FRAMES} frames");
}

/// Publications starting after a change the rows must have taken before a test reads them: the
/// eye shows the newest publication's means (a box with no tail), so one.
const SETTLED: f64 = 1.0;

/// Whether `v` is a publication the test has not seen yet; it becomes the seen one.
fn newer(seen: &mut Option<Arc<LiveValues>>, v: Option<&Arc<LiveValues>>) -> Option<Arc<LiveValues>> {
    let v = v.filter(|v| seen.as_ref().is_none_or(|s| !Arc::ptr_eq(s, v)))?.clone();
    *seen = Some(v.clone());
    Some(v)
}

/// After a command was sent: frames until the second publication the host sees from now on, and
/// its time. The worker takes commands at the head of its loop and publishes at most once before
/// it gets there (one more may have been waiting unseen), so the run had the command by this time
/// and every later publication's interval starts after it. No frame or wall bound: this follows
/// the run's own publications.
fn command_taken(host: &mut SimHost, live: &mut SimLive, start: Instant) -> f64 {
    let mut seen = host.values().cloned();
    let mut fresh = 0;
    loop {
        host.step(start.elapsed().as_secs_f64(), live);
        if let Some(v) = newer(&mut seen, host.values()) {
            fresh += 1;
            if fresh == 2 {
                return v.t;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Frames until `SETTLED` publications the host takes into its eye start at or after `from`
/// (simulated seconds) and span simulated time: the rows then show the run after `from`, whatever
/// came before. This follows the run's own progress, never a frame
/// count or the wall clock: a busy box takes longer and never fails here. Returns the windows
/// covered and the simulated time past `from` when `shown` first held (None if it never did).
fn eye_past(host: &mut SimHost, live: &mut SimLive, start: Instant, from: f64, mut shown: impl FnMut(&SimHost) -> bool) -> Option<(f64, f64)> {
    let mut seen = host.values().cloned();
    let (mut covered, mut first) = (0.0, None);
    while covered < SETTLED {
        host.step(start.elapsed().as_secs_f64(), live);
        if let Some(v) = newer(&mut seen, host.values()) {
            if v.since >= from && v.t > v.since {
                covered += 1.0;
            }
            if first.is_none() && shown(host) {
                first = Some((covered, v.t - from));
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    first
}

/// The fixture's PWM period (`ledctl_bench(10e3, ..)`) and the periods a lamp mean spans.
const PERIOD: f64 = 1e-4;
const PERIODS: u32 = 20;
/// Every lamp pin's PWM duty: the fraction of each period it is high, the LED dark.
const DUTY: f64 = 0.3;

/// A lamp's on-current from `rail` through `ohms`: the LED on the simulator's own green curve in
/// series with the resistor and the conducting controller pin (the library's `pin_ron`).
fn lamp_on_current(rail: f64, ohms: f64) -> f64 {
    let libs = ModelLibs::generic();
    let (mut lo, mut hi) = (0.0, rail / ohms);
    for _ in 0..64 {
        let i = 0.5 * (lo + hi);
        let volts = libs.led_vf(LedColor::Green, i).expect("the generic library has green") + i * (ohms + libs.pin_ron);
        if volts < rail { lo = i } else { hi = i }
    }
    0.5 * (lo + hi)
}

/// The nets a lamp sink records: the 3V3 rail and LED 0's and LED 1's anodes (each lamp's resistor
/// runs from 3V3 to its LED's anode, the cathode to its PWM pin).
const LAMP_NETS: [&str; 3] = ["3V3", "lamps[0].d.A", "lamps[1].d.A"];

/// The lamp nets' unknowns (once `begin` resolved them) and their voltages at every accepted point.
type LampPoints = Arc<Mutex<(Option<[usize; 3]>, Vec<(f64, [f64; 3])>)>>;

/// A scope's sink that records the lamp nets.
struct Lamps(LampPoints);

impl Sink for Lamps {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        let at = |name: &str| unknowns.iter().position(|u| u.name == name);
        if let Ok(mut p) = self.0.lock() {
            p.0 = match LAMP_NETS.map(at) {
                [Some(a), Some(b), Some(c)] => Some([a, b, c]),
                _ => None,
            };
        }
    }
    fn point(&mut self, at: f64, _outer: Option<f64>, x: &[f64]) {
        if let Ok(mut p) = self.0.lock() {
            if let Some([a, b, c]) = p.0 {
                p.1.push((at, [x[a], x[b], x[c]]));
            }
        }
    }
}

/// The PWM period starts at or after and at or before `t`.
fn period_after(t: f64) -> f64 {
    (t / PERIOD).ceil() * PERIOD
}
fn period_before(t: f64) -> f64 {
    (t / PERIOD).floor() * PERIOD
}

/// The means of the 3V3 rail and of LED 0's and LED 1's currents (`ohms` from 3V3 to each anode)
/// over `PERIODS` whole PWM periods from the period start `t0`: the integral of the accepted
/// points' linear interpolant, so a function of simulated time and the points alone (never of the
/// frames or of wall time; a whole number of periods holds no PWM phase). None until the points
/// cover the window.
fn lamp_means(points: &LampPoints, t0: f64, ohms: f64) -> Option<[f64; 3]> {
    let t1 = t0 + f64::from(PERIODS) * PERIOD;
    let p = points.lock().ok()?;
    let pts = &p.1;
    if pts.first()?.0 > t0 || pts.last()?.0 < t1 {
        return None;
    }
    let mut sum = [0.0f64; 3];
    for w in pts.windows(2) {
        let ((ta, va), (tb, vb)) = (w[0], w[1]);
        let (a, b) = (ta.max(t0), tb.min(t1));
        if b <= a {
            continue;
        }
        let lerp = |t: f64| -> [f64; 3] {
            let f = (t - ta) / (tb - ta);
            [0, 1, 2].map(|k| va[k] + f * (vb[k] - va[k]))
        };
        let (ya, yb) = (lerp(a), lerp(b));
        for k in 0..3 {
            sum[k] += 0.5 * (ya[k] + yb[k]) * (b - a);
        }
    }
    let [rail, a0, a1] = sum.map(|v| v / (t1 - t0));
    Some([rail, (rail - a0) / ohms, (rail - a1) / ohms])
}

/// Frames until the lamp points cover the window from `t0`, then its means.
fn lamp_window(host: &mut SimHost, live: &mut SimLive, start: Instant, lamps: &LampPoints, t0: f64, ohms: f64, what: &str) -> [f64; 3] {
    frames(host, live, start, what, |_| lamp_means(lamps, t0, ohms).is_some());
    lamp_means(lamps, t0, ohms).expect("the window is covered")
}

fn pins_on(design: &Design, net: NetRef) -> Vec<PinRef> {
    let circuit = &design.unit(net.board).expect("the fixture has its board").circuit;
    circuit
        .comps
        .iter()
        .flat_map(|c| c.conns.iter().filter(|(_, n)| *n == net.net).map(move |&(pin, _)| PinRef { board: net.board, comp: c.id, pin }))
        .collect()
}

#[test]
fn sim_host_lowers_runs_and_writes_rows() {
    let b0 = BoardId(0);
    let design = Arc::new(ledctl());
    let circuit = &design.unit(b0).expect("ledctl has board 0").circuit;
    let net_ref = |name: &str| NetRef { board: b0, net: circuit.net(name).unwrap_or_else(|| panic!("ledctl has net {name}")).id };
    let comp_ref = |path: &str| CompRef { board: b0, comp: circuit.comp(path).unwrap_or_else(|| panic!("ledctl has {path}")).id };
    let (v33, anode0) = (net_ref("3V3"), net_ref("lamps[0].d.A"));
    let (led0, r0) = (comp_ref("lamps[0].d"), comp_ref("lamps[0].r"));

    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(ledctl_bench(10e3, [DUTY; 8]));
    host.commit(&design);
    // the lamp nets at every accepted point from the run's start (queued until the worker runs)
    let lamps = LampPoints::default();
    host.act(&SimAction::ShowScope { id: 6, sink: SinkSlot::new(Box::new(Lamps(lamps.clone()))), column: 0.0 });
    let mut live = SimLive::default();
    let slots = live.minis.set(TreeOwner::Sheet(b0, 0), &[anode0]);
    frames(&mut host, &mut live, start, "first values", |h| h.values().is_some_and(|v| v.t > 2e-3));
    assert!(lamps.lock().is_ok_and(|p| p.0.is_some()), "the lamp sink resolves {LAMP_NETS:?} by name");
    assert_eq!(host.generation(), design.generation, "the rows are of the committed generation");
    assert!(host.error().is_none(), "the live bench runs: {:?}", host.error());

    // nets: the regulator's output reads as its mean voltage
    let index = live.index.clone();
    assert_eq!(index.generation, host.values().expect("the running lowering has values").generation, "views get the running lowering's numbering");
    let latest = host.values().map_or(f64::NAN, |v| v.t);
    assert_eq!(live.t, latest, "views get the simulated time of the latest values");
    assert!(live.ratio.is_some_and(|r| r > 0.0), "views get the simulation's speed: {:?}", live.ratio);
    // the transport's status while the bench plays as fast as it goes, with no device
    frames(&mut host, &mut live, start, "a reached rate", |h| h.status().rate_achieved.is_some());
    let status = host.status();
    assert!(status.playing && !status.stepping && !status.steady && status.rate_asked == f64::INFINITY, "the status of a playing run: {status:?}");
    assert!(status.rate_achieved.is_some_and(|r| r > 0.0), "the run reaches a rate: {status:?}");
    // which of the run's own parts takes longer, the solver or the scopes' sinks, is the
    // machine's: the limit names the larger share, never the clock, a device or its events
    assert!(matches!(status.limited_by, Some(Limit::Solver | Limit::Scope)) && status.limited_by == status.split.map(|s| s.largest()),
        "unbounded and with no device, the run's own work limits the rate: {status:?}");
    // the device's event feed: a full one is the run's limit, and one step drains it without
    // waiting and with no redraw asked, every event kept in order
    let feed = host.set_events::<u32>(|_| EventSource::Trace);
    let mut sent = 0u32;
    while feed.offer(&mut vec![(0.0, sent)]) {
        sent += 1;
    }
    assert_eq!(host.status().limited_by, Some(Limit::Events), "a full event feed holds the run: {:?}", host.status());
    host.step(start.elapsed().as_secs_f64(), &mut live);
    assert_ne!(host.status().limited_by, Some(Limit::Events), "a step drained the feed: {:?}", host.status());
    let store = host.events::<u32>().expect("the log holds u32 frames");
    let reader = store.register();
    assert_eq!(store.read(reader).map(|e| e.frame).collect::<Vec<_>>(), (0..sent).collect::<Vec<_>>(),
        "every event the feed took arrived, in order");
    assert!(host.events::<i64>().is_none(), "a store of another frame type is none");
    host.clear_events();
    let v33_row = host.rows().nets().row(index.net(v33).expect("3V3 is lowered"));
    assert!((v33_row[1] - 3.3).abs() < 0.1, "the 3V3 net row carries its voltage: {v33_row:?}");
    assert!(v33_row[0] >= 0.0, "the 3V3 net row has a ramp position");

    // parts: a lit LED's light output from its mean forward current
    let emission = |h: &SimHost, c: CompRef| h.rows().parts().row(index.comp(c).expect("the LED is lowered"))[1];
    // read once the eye holds only the steady run: the lamp nets are steady from 1 ms on
    let early = emission(&host, led0);
    eye_past(&mut host, &mut live, start, 1e-3, |_| false);
    let lit0 = emission(&host, led0);
    assert!(lit0 > 0.0, "LED 0 at 30 % duty gives light: {lit0}");
    eprintln!("LED 0's row once the run had a rate: {early}; with the eye settled past 1 ms: {lit0}");

    // segments: a sheet tree on LED 0's anode net, from the resistor's pin to the LED's anode
    let pins = pins_on(&design, anode0);
    assert_eq!(pins.len(), 2, "the anode net joins the resistor and the LED");
    let (r_pin, d_pin) = if pins[0].comp == r0.comp { (pins[0], pins[1]) } else { (pins[1], pins[0]) };
    let tree = TreeIn {
        net: anode0,
        nodes: 3,
        segs: vec![(0, 1), (1, 2)],
        keys: vec![SegKey::pins(&[r_pin]), SegKey::pins(&[d_pin])],
        pins: vec![(0, r_pin), (2, d_pin)],
        ..TreeIn::default()
    };
    let rows = live.trees.set(TreeOwner::Sheet(b0, 0), vec![tree.clone()]);
    let seg = |h: &SimHost, k: usize| h.rows().segs().row(rows[0][k] as usize);
    frames(&mut host, &mut live, start, "segment flow", |h| seg(h, 0)[1] != 0.0);
    assert!(seg(&host, 0)[1] > 0.0 && seg(&host, 1)[1] > 0.0, "current flows from the resistor to the LED along both segments");
    assert!((seg(&host, 0)[2] - seg(&host, 1)[2]).abs() < 1e-4, "a chain carries one current level");

    // mini traces: the subscribed anode net's strip has filled while the bench ran
    let mini = slots[0] as usize * MINI_SLOT;
    frames(&mut host, &mut live, start, "mini filled", |h| {
        let scale = h.mini().row(mini);
        scale[0] <= scale[1]
    });
    let scale = host.mini().row(mini);
    assert!(scale[0] <= scale[1] && (1..=MINI_RING).any(|column| {
        let row = host.mini().row(mini + column);
        row[0] <= row[1]
    }), "mini: the subscribed net's slot has a scale and filled columns");
    assert_eq!(host.mini().row(live.minis.len() * MINI_SLOT), MINI_EMPTY,
        "mini: a texel past the subscribed slots stays empty");

    // the knob: twice the resistance, at once, halves LED 0's light (its row) and current; LED 1
    // keeps its current but for its share of the rail. The currents are means over 20 whole PWM
    // periods of simulated time, from the lamp sink: the last 20 before the knob is sent (from
    // 1 ms on, past the start), and 20 from ten periods after the rows show the knob's effect.
    let ohms = circuit.comp("lamps[0].r").and_then(|c| c.value.as_ref()).and_then(|v| v.qty).expect("lamp resistor value").si;
    let ohms1 = circuit.comp("lamps[1].r").and_then(|c| c.value.as_ref()).and_then(|v| v.qty).expect("lamp resistor value").si;
    assert_eq!(ohms, ohms1, "the lamps' resistors are alike");
    let t_knob = host.values().map_or(0.0, |v| v.t);
    let before = lamp_window(&mut host, &mut live, start, &lamps, (period_before(t_knob) - f64::from(PERIODS) * PERIOD).max(1e-3), ohms, "the lamp means before the knob");
    assert!(host.command(&SimCmd::SetParam { comp: r0, value: 2.0 * ohms }), "a resistor's value reaches the bench");
    assert!(!host.command(&SimCmd::SetParam { comp: led0, value: 1.0 }), "an LED has no tunable value");
    // the rows show the knob once the eye holds only the run after it: they must by then, and
    // waiting for that follows the run (a busy box is slower, never failed)
    let taken = command_taken(&mut host, &mut live, start);
    let first = eye_past(&mut host, &mut live, start, taken, |h| emission(h, led0) < 0.7 * lit0);
    let lit_knob = emission(&host, led0);
    assert!(lit_knob < 0.7 * lit0, "LED 0's row shows the knob {SETTLED} publication(s) after the run had it: {lit_knob} against {lit0} before");
    eprintln!("the knob's effect first showed {first:?} (publications covered, simulated seconds) after the run had it at {taken:e} s, res {:e} s", host.res());
    let t_seen = host.values().map_or(0.0, |v| v.t);
    let after = lamp_window(&mut host, &mut live, start, &lamps, period_after(t_seen) + 10.0 * PERIOD, ohms, "the lamp means after the knob");
    // LED 0's resistor carries (3V3 - anode) / R; the knob's R is twice the sink's `ohms`. Each mean
    // is the low share of each period times the on-current the simulator's own green curve gives
    // through that resistor and the conducting pin, from the window's rail, within twice the live
    // solve's reltol (the host runs Fast). Twice the resistance does not halve it: the LED's drop
    // falls with its current.
    let (i0_before, i0_after) = (before[1], after[1] / 2.0);
    let lamp_tol = 2.0 * FAST_RELTOL;
    let (want_before, want_after) = ((1.0 - DUTY) * lamp_on_current(before[0], ohms), (1.0 - DUTY) * lamp_on_current(after[0], 2.0 * ohms));
    assert!((i0_before / want_before - 1.0).abs() <= lamp_tol,
        "LED 0 at R = {ohms} ohm: mean {i0_before} A, the green curve gives {want_before} A (tolerance {lamp_tol})");
    assert!((i0_after / want_after - 1.0).abs() <= lamp_tol,
        "LED 0 at 2R = {} ohm: mean {i0_after} A, the green curve gives {want_after} A (tolerance {lamp_tol})", 2.0 * ohms);
    assert!(i0_after * (1.0 + lamp_tol) < i0_before * (1.0 - lamp_tol),
        "LED 0's current falls with twice its resistance, beyond the means' tolerance: {i0_before} then {i0_after} A");
    // LED 1 sees the knob only through the rail: a rail step dV moves its on-current by at most
    // dV / R (the LED and the pin add resistance), and the lamps share the PWM phase, so its mean
    // moves by at most the rail mean's step over R. Each mean is the live solve's, within its
    // reltol of the exact one.
    let rail = (after[0] - before[0]).abs() / ohms;
    let tol = 2.0 * FAST_RELTOL * before[2];
    let moved = (after[2] - before[2]).abs();
    eprintln!("lamp means: rail {} then {} V; LED 0 {i0_before} then {i0_after} A; LED 1 {} then {} A (moved {moved}, rail share {rail}, tolerance {tol})", before[0], after[0], before[2], after[2]);
    assert!(moved <= rail + tol, "LED 1 is untouched by LED 0's resistor: its mean current {} then {} A moves {moved}, past the rail's share {rail} and the means' tolerance {tol}", before[2], after[2]);

    // a scope's sink: points while shown, none once hidden
    let count = Arc::new(AtomicUsize::new(0));
    host.act(&SimAction::ShowScope { id: 7, sink: SinkSlot::new(Box::new(Counting(count.clone()))), column: 0.0 });
    frames(&mut host, &mut live, start, "points into a shown scope", |_| count.load(Ordering::Relaxed) > 100);
    // the app's routing: a frame's actions, as views emit them
    let hide: Vec<Box<dyn ActionTrait>> = vec![Box::new(SimAction::HideScope { id: 7 })];
    host.handle_actions(&hide);
    let t_hide = host.values().map_or(0.0, |v| v.t);
    frames(&mut host, &mut live, start, "the hide to take effect", |h| h.values().is_some_and(|v| v.t > t_hide + 1e-3));
    let after = count.load(Ordering::Relaxed);
    let t_more = host.values().map_or(0.0, |v| v.t);
    frames(&mut host, &mut live, start, "more simulated time", |h| h.values().is_some_and(|v| v.t > t_more + 1e-3));
    assert_eq!(count.load(Ordering::Relaxed), after, "a hidden scope's sink gets no points");

    // the rolling record and the take; a review: the rows show the take at the cursor's time while
    // the run goes on
    let store = host.store();
    assert!(store.len() > 100, "the consumer store holds the record");
    let span = store.time.at(store.len() - 1) - store.time.at(0);
    assert!(span <= 20e-3 + 2.0 * 4096.0 * 1e-4, "the record keeps about its 20 ms (whole chunks): {span}");
    assert_eq!(store.signals.len(), index.counts().0, "the trace carries the nets only: the take has the pins");
    let (first, last) = live.record;
    assert!(first <= store.time.at(0) && last > first && host.take().span() == Some(live.record),
        "the span a strip reviews is the take's, from the run's start: {:?} against the record from {}", live.record, store.time.at(0));
    let mid = 0.5 * (first + last);
    host.cursor(TimeCursor { t: mid, playing: false, rate: 1.0, source: CursorSource::Sim, fw: None });
    host.step(start.elapsed().as_secs_f64(), &mut live);
    let n = index.net(v33).expect("3V3 is lowered");
    let mut at_mid = Means::default();
    host.take().means(mid, mid, &mut at_mid).expect("the take holds the cursor's time");
    assert_eq!(host.rows().nets().row(n)[1], at_mid.net[n], "a review writes the take's frame at the cursor");
    // a dimmed LED under review shows its duty: the frame's mean over whole PWM periods, not the
    // instant's on or off
    let anode1 = net_ref("lamps[1].d.A");
    let led1 = comp_ref("lamps[1].d").comp;
    let d1 = pins_on(&design, anode1).into_iter().find(|p| p.comp == led1).expect("LED 1's anode is on its net");
    let i1 = f64::from(at_mid.pin[index.pin(d1).expect("LED 1's anode is lowered")]);
    let want1 = (1.0 - DUTY) * lamp_on_current(3.3, ohms1);
    assert!((i1 / want1 - 1.0).abs() < 0.05, "LED 1 under review reads its duty: {i1} A against {want1} A ({} of its on-current)", 1.0 - DUTY);
    let reviewed_t = host.values().map(|v| v.t).expect("values");
    frames(&mut host, &mut live, start, "the run going on under a review", |h| h.values().is_some_and(|v| v.t > reviewed_t + 0.01));
    assert!(live.playing && live.scrub == Some(mid) && live.t == mid, "the run goes on while the cursor reviews, the views at the cursor's time: {:?} at {}", live.scrub, live.t);
    host.take().means(mid, mid, &mut at_mid).expect("the take still holds the cursor's time");
    assert_eq!(host.rows().nets().row(n)[1], at_mid.net[n], "the rows stay at the reviewed instant as the run goes on");
    let play: Vec<Box<dyn ActionTrait>> = vec![Box::new(ViewAction::Cursor(TimeCursor { t: mid, playing: true, rate: 1.0, source: CursorSource::Sim, fw: None }))];
    host.handle_actions(&play);
    frames(&mut host, &mut live, start, "back to live after the review", |h| h.values().is_some_and(|v| v.t > reviewed_t + 0.02));
    assert!(live.scrub.is_none() && live.t > mid, "back to live: the views at the run's front ({})", live.t);

    // probe-only bench commits (a new generation, the same circuit and stimulus): no lowering, the
    // run and the other probe's points go on, the new probe's sink comes and goes
    let (kept, added) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
    host.act(&SimAction::ShowScope { id: 8, sink: SinkSlot::new(Box::new(Times(kept.clone()))), column: 0.0 });
    frames(&mut host, &mut live, start, "points into the kept probe", |_| times_len(&kept) > 100);
    let mut probed = design.clone();
    let numbering = host.index().generation;
    for show in [true, false] {
        Arc::make_mut(&mut probed).generation += 1;
        host.commit(&probed);
        let action: Box<dyn ActionTrait> = if show {
            Box::new(SimAction::ShowScope { id: 9, sink: SinkSlot::new(Box::new(Times(added.clone()))), column: 0.0 })
        } else {
            Box::new(SimAction::HideScope { id: 9 })
        };
        host.handle_actions(&[action]);
        let (seen, t) = (times_len(&kept), host.values().map_or(0.0, |v| v.t));
        frames(&mut host, &mut live, start, "simulated time after a probe-only commit", |h| {
            h.values().is_some_and(|v| v.t > t + 2e-3) && !h.lowering()
        });
        assert_eq!(host.generation(), probed.generation, "the running bench reflects the probe-only commit");
        assert!(host.values().is_some_and(|v| v.generation == numbering), "values stay of the running lowering");
        assert!(times_len(&kept) > seen, "the kept probe gets points across the probe change");
        if show {
            assert!(times_len(&added) > 0, "the added probe's sink gets points");
        }
    }
    let after_hide = times_len(&added);
    let t_hidden = host.values().map_or(0.0, |v| v.t);
    frames(&mut host, &mut live, start, "time after the removed probe", |h| h.values().is_some_and(|v| v.t > t_hidden + 1e-3));
    assert_eq!(times_len(&added), after_hide, "the removed probe's sink gets no more points");
    let kept_times = kept.lock().map(|t| t.clone()).unwrap_or_default();
    let back = kept_times.windows(2).position(|w| w[1] < w[0]).map(|i| (i, kept_times[i], kept_times[i + 1]));
    assert!(back.is_none(),
        "the kept probe's points run on in time through both commits, never back (of {}: {back:?})", kept_times.len());
    host.handle_actions(&[Box::new(SimAction::HideScope { id: 8 }) as Box<dyn ActionTrait>]);

    // a stimulus retune (PA0's duty 0.3 -> 0.7, a value of the bench, not its structure): no Replace,
    // the run and its probe go on, and LED 0 (lit while PA0 is low) dims to its new duty
    let times = Arc::new(Mutex::new(Vec::new()));
    host.act(&SimAction::ShowScope { id: 10, sink: SinkSlot::new(Box::new(Times(times.clone()))), column: 0.0 });
    frames(&mut host, &mut live, start, "points into the retune probe", |_| times_len(&times) > 100);
    let (numbering, generation) = (host.index().clone(), host.generation());
    // LED 0's light (its row) and its mean current over 20 whole periods (the lamp sink), the last
    // 20 before the retune is sent and 20 from ten periods after the row shows it
    let now = host.values().map_or(0.0, |v| v.t);
    eye_past(&mut host, &mut live, start, now, |_| false);
    let lit_before = emission(&host, led0);
    let t_retune = host.values().map_or(0.0, |v| v.t);
    let before = lamp_window(&mut host, &mut live, start, &lamps, period_before(t_retune) - f64::from(PERIODS) * PERIOD, ohms, "the lamp means before the retune");
    let mut duty = [0.3; 8];
    duty[0] = 0.7;
    host.set_bench(ledctl_bench(10e3, duty));
    host.commit(&probed);
    frames(&mut host, &mut live, start, "the retune's lowering", |h| !h.lowering());
    // the lowering sent the retune; the PWM switches at its next period start
    let taken = command_taken(&mut host, &mut live, start) + PERIOD;
    let first = eye_past(&mut host, &mut live, start, taken, |h| emission(h, led0) < 0.75 * lit_before);
    let lit_retuned = emission(&host, led0);
    assert!(lit_retuned < 0.75 * lit_before, "LED 0's row shows the retune {SETTLED} publication(s) after the run had it: {lit_retuned} against {lit_before} before");
    eprintln!("the retune's effect first showed {first:?} (publications covered, simulated seconds) after the run had it at {taken:e} s");
    assert!(Arc::ptr_eq(host.index(), &numbering), "a duty change is a retune: the run keeps its lowering's numbering (no Replace)");
    assert_eq!(host.generation(), generation, "a retune keeps the running lowering");
    let t_seen = host.values().map_or(0.0, |v| v.t);
    let after = lamp_window(&mut host, &mut live, start, &lamps, period_after(t_seen) + 10.0 * PERIOD, ohms, "the lamp means after the retune");
    // LED 0's resistor is twice `ohms` since the knob
    let (i0_before, i0_after) = (before[1] / 2.0, after[1] / 2.0);
    assert!(i0_after < 0.6 * i0_before, "LED 0 dims to its new duty (lit 30 % of the time, was 70 %): {i0_before} then {i0_after} A");
    host.handle_actions(&[Box::new(SimAction::HideScope { id: 6 }) as Box<dyn ActionTrait>]);
    let retune_times = times.lock().map(|t| t.clone()).unwrap_or_default();
    let back = retune_times.windows(2).position(|w| w[1] < w[0]);
    assert!(back.is_none(), "the probe's points run on in time through the retune (of {}: {back:?})", retune_times.len());
    host.handle_actions(&[Box::new(SimAction::HideScope { id: 10 }) as Box<dyn ActionTrait>]);

    // a value edited in the circuit: the run takes it where it is, keeping its numbering
    let t_before = host.values().map_or(0.0, |v| v.t);
    let numbering_before = host.index().generation;
    let edit = |design: &Design, edit: &dyn Fn(&mut fab_elec_circuit::Circuit)| {
        let mut edited = design.clone();
        edited.generation += 3;
        let unit = edited.units.iter_mut().find(|u| u.id == b0).expect("ledctl has board 0");
        edit(Arc::make_mut(&mut unit.circuit));
        Arc::new(edited)
    };
    let valued = edit(&design, &|c| {
        let r0 = c.comps.iter_mut().find(|c| c.path == "lamps[0].r").expect("ledctl has lamps[0].r");
        r0.value.as_mut().and_then(|v| v.qty.as_mut()).expect("lamps[0].r has a resistance").si = 330.0;
    });
    host.commit(&valued);
    frames(&mut host, &mut live, start, "the circuit's value taken", |h| h.generation() == valued.generation && !h.lowering());
    assert!(Arc::ptr_eq(&live.index, host.index()) && host.index().generation == numbering_before, "a value edit keeps the run's numbering (no Replace)");
    assert!(host.values().is_some_and(|v| v.t >= t_before), "simulated time continues through the value edit");

    // a part taken out: the bench takes the new lowering, time continues, the segment rows stay
    let t_before = host.values().map_or(0.0, |v| v.t);
    let edited = edit(&valued, &|c| c.comps.retain(|comp| comp.path != "reg.c_in"));
    host.commit(&edited);
    frames(&mut host, &mut live, start, "the new generation's values", |h| {
        h.generation() == edited.generation && h.values().is_some_and(|v| v.generation == h.index().generation)
    });
    assert_eq!(host.generation(), edited.generation, "the host follows the commit");
    assert_eq!(live.index.generation, host.index().generation, "views get the new lowering's numbering");
    assert_ne!(live.index.generation, numbering_before, "views get the new lowering's numbering");
    assert!(host.values().is_some_and(|v| v.t >= t_before), "simulated time continues across the replace");
    assert_eq!(live.trees.set(TreeOwner::Sheet(b0, 0), vec![tree]), rows, "segment rows are independent of the lowering");
    frames(&mut host, &mut live, start, "flow after the replace", |h| seg(h, 0)[1] > 0.0);

    // a probe's tip: an ideal one (a bandwidth only) is the channel's and keeps the run; a tip load
    // is the circuit's, so giving it one replaces the lowering (a new numbering, time goes on)
    let mut probed = ledctl_bench(10e3, duty);
    probed.probes.push(ScopeProbe { probe: Probe::V(anode0), label: "tip".into(), tip: Tip { r: None, c: None, bw: Some(1e6) } });
    let numbering = host.index().generation;
    host.set_bench(probed.clone());
    host.commit(&edited);
    frames(&mut host, &mut live, start, "the ideal probe's lowering", |h| !h.lowering());
    assert_eq!(host.index().generation, numbering, "an ideal probe lowers nothing: the run keeps its numbering");
    // the bench's voltage probe is kept whole over the take with no view asking (a Scope opened
    // later finds its lane from the run's start), under each numbering
    let probe_whole = |h: &SimHost| h.index().net(anode0).is_some_and(|row| h.whole().get(row) == Some(&true));
    assert!(probe_whole(&host), "the bench's probe is kept whole: {:?}", host.whole());
    probed.probes[0].tip = Tip { r: Some(10e6), c: Some(15e-12), bw: Some(1e6) };
    let t_before = host.values().map_or(0.0, |v| v.t);
    host.set_bench(probed);
    host.commit(&edited);
    frames(&mut host, &mut live, start, "the loaded probe's values", |h| {
        h.values().is_some_and(|v| v.generation == h.index().generation && v.generation != numbering)
    });
    assert!(host.values().is_some_and(|v| v.t >= t_before), "simulated time continues across the tip's replace");
    assert!(probe_whole(&host), "the bench's probe stays whole under the new numbering: {:?}", host.whole());

    // at rest an LED shows its current at the instant the run stands, not the eye's mean: a Step's
    // end is published standing, and each LED's light is its anode current's there
    let t_rest = host.values().map_or(0.0, |v| v.t);
    assert!(host.command(&SimCmd::Step(1e-3)), "Step reaches the live bench");
    frames(&mut host, &mut live, start, "the Step's end", |h| h.values().is_some_and(|v| !v.running && v.t >= t_rest + 1e-3));
    let status = host.status();
    assert!(!status.playing && status.limited_by.is_none(), "a run at rest has no limit: {status:?}");
    let leds = host.leds(&edited);
    assert!(leds.len() >= 8, "ledctl's lamps are LEDs: {leds:?}");
    for led in &leds {
        assert_eq!(led.brightness, emission_attr(led.i, LED_RATED_A as f32), "at rest {:?} shows its current at t: {led:?}", led.part);
    }
}

/// A block bench runs its own circuit with numbering distinct from either design run.
#[test]
fn a_block_bench_scope_runs_its_own_circuit_and_never_crosses_the_design_run() {
    let b0 = BoardId(0);
    let design = Arc::new(ledctl());
    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(ledctl_bench(10e3, [DUTY; 8]));
    host.commit(&design);
    let mut live = SimLive::default();
    frames(&mut host, &mut live, start, "the design run's first values", |h| h.values().is_some());
    let n0 = host.index().generation;
    let g = design.generation;
    let design_parts = host.index().counts().2;

    let mut c = design.unit(b0).expect("ledctl has board 0").circuit.as_ref().clone();
    c.unit = BoardId(BENCH_SCOPE_BASE);
    // one part fewer that leaves no net without a DC path: the regulator's input capacitor
    assert!(c.comps.iter().any(|comp| comp.path == "reg.c_in"), "ledctl has reg.c_in");
    c.comps.retain(|comp| comp.path != "reg.c_in");
    let circuit = Arc::new(c);
    host.set_scope(Some(BenchScope::Own { circuit: Arc::clone(&circuit), generation: g }));
    host.commit(&design);
    frames(&mut host, &mut live, start, "the bench run's own values", |h| {
        h.scope_id() == Some(BoardId(BENCH_SCOPE_BASE)) && h.values().is_some_and(|v| v.generation == h.index().generation)
    });
    assert_eq!(host.generation(), g, "the bench run keeps its own source generation");
    assert_ne!(host.index().generation, n0, "the bench run's numbering differs from the design run's at the same source generation");
    assert_eq!(host.index().counts().2, design_parts - 1, "the bench's own circuit runs, not the design unit");
    assert!(host.error().is_none(), "the bench's own circuit runs without an error: {:?}", host.error());
    let bench_numbering = host.index().generation;

    host.set_scope(Some(BenchScope::Own { circuit: Arc::clone(&circuit), generation: g }));
    host.commit(&design);
    let mut same_frames = 0;
    frames(&mut host, &mut live, start, "the same bench circuit's check", |h| {
        same_frames += 1;
        same_frames >= 3 && !h.lowering()
    });
    assert_eq!(host.index().generation, bench_numbering, "the same bench circuit is not lowered again");

    host.set_scope(Some(BenchScope::Design(b0)));
    host.commit(&design);
    frames(&mut host, &mut live, start, "the design run's values after the bench", |h| {
        h.scope_id() == Some(b0) && h.values().is_some_and(|v| v.generation == h.index().generation)
    });
    assert!(host.index().generation > bench_numbering, "the design run's numbering advances past the bench run's");
    assert_eq!(host.index().counts().2, design_parts, "switching back runs the design's full circuit");
}

/// Fast's time resolution follows the finest view: a quarter of the finest open scope's pixel
/// column or of the eye's window (`EYE` of the playback timebase), at most the largest step; Exact
/// steps at 0. A host built as the app builds it (no spec set) runs Fast from its first run. Each
/// change reaches the running worker, whose publications carry the resolution it steps at, as one
/// command: a setting that leaves the resolution as it is sends none.
#[test]
fn the_time_resolution_follows_the_finest_view_and_exact_steps_at_zero() {
    let design = Arc::new(ledctl());
    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    let spec = LiveSpec::default();
    host.set_bench(ledctl_bench(10e3, [DUTY; 8]));
    assert!(!host.exact(), "Fast is the default");
    assert_eq!(host.res(), spec.max_step, "a host built fresh is Fast: with no scope open at 1x the eye's 30 ms window is far coarser than the largest step");
    host.commit(&design);
    let mut live = SimLive::default();
    let runs_at = |res: f64| move |h: &SimHost| h.values().is_some_and(|v| v.res == res);
    frames(&mut host, &mut live, start, "the first run starts at Fast's resolution, not Exact's 0", runs_at(spec.max_step));
    // a SetRes ends the run's clean stretch: none is sent for a setting that keeps the resolution
    let clean = |h: &SimHost| h.content().map(|c| c.clean_until());
    assert!(host.command(&SimCmd::SetSimMode(SimMode::Fast)), "Fast is a bench command");
    assert_eq!(clean(&host), Some(f64::INFINITY), "Fast again sends no command");
    assert!(host.command(&SimCmd::SetSimMode(SimMode::Exact)));
    assert!(host.command(&SimCmd::SetSimMode(SimMode::Exact)));
    frames(&mut host, &mut live, start, "the worker steps at Exact", runs_at(0.0));
    assert!(clean(&host).is_some_and(f64::is_finite), "the change to Exact is a command");
    assert!(host.command(&SimCmd::SetSimMode(SimMode::Fast)));
    frames(&mut host, &mut live, start, "back in Fast the worker steps at its resolution", runs_at(spec.max_step));

    let count = Arc::new(AtomicUsize::new(0));
    host.act(&SimAction::ShowScope { id: 1, sink: SinkSlot::new(Box::new(Counting(count.clone()))), column: 150e-6 });
    assert_eq!(host.res(), 37.5e-6, "a scope at 150 us per column: a quarter of its column");
    host.act(&SimAction::ShowScope { id: 2, sink: SinkSlot::new(Box::new(Counting(count.clone()))), column: 2e-6 });
    assert_eq!(host.res(), 0.5e-6, "a narrower timebase lowers it");
    frames(&mut host, &mut live, start, "the worker steps at the narrower scope's resolution", runs_at(0.5e-6));
    let status = host.status();
    assert_eq!((status.mode, status.res), (SimMode::Fast, 0.5e-6), "the transport reads out Fast and the resolution the worker steps at");
    host.act(&SimAction::HideScope { id: 2 });
    assert_eq!(host.res(), 37.5e-6, "hiding the narrower scope gives the finest open one's");
    assert!(host.command(&SimCmd::SetRate(1e-3)), "the rate is a bench command");
    let eye = 0.03 * 1e-3 / 4.0;
    assert!((host.res() - eye).abs() <= 1e-12 * eye, "in slow motion the eye's window is the finest: {} s", host.res());
    let slow = host.res();
    frames(&mut host, &mut live, start, "the worker steps at the eye's resolution", runs_at(slow));
    assert!(host.command(&SimCmd::SetSimMode(SimMode::Exact)), "Exact is a bench command");
    assert_eq!(host.res(), 0.0, "Exact steps at 0");
    frames(&mut host, &mut live, start, "the worker steps at Exact", runs_at(0.0));
    let status = host.status();
    assert_eq!((status.mode, status.res), (SimMode::Exact, 0.0), "the transport reads out Exact");
    host.act(&SimAction::HideScope { id: 1 });
    assert_eq!(host.res(), 0.0, "Exact stays 0 whatever the views");
    assert!(count.load(Ordering::Relaxed) > 0, "the scopes got points");

    // the project's run settings (its sim statement) are followed: the mode at once, and a picked
    // sample rate starts the run over at it, named as an edit; the same settings again do nothing
    assert_eq!((status.sample_rate, status.sample_rate_chosen), (0.0, None), "with nothing picked and nothing heard the run has no sample rate");
    let picked = SimSettings { mode: Some(SimMode::Fast), sample_rate: Some(48_000.0), ..SimSettings::default() };
    host.follow_settings(Some(picked.clone()));
    assert!(!host.exact(), "the statement's mode is followed");
    assert_eq!(host.restart_pending(), Some(Some("the sample rate")), "a picked rate starts the run over, named");
    // a run that is running at `hz` (between runs the status reads the next run's rate)
    let at = |hz: f64| move |h: &SimHost| h.restart_pending().is_none() && h.content().is_some() && h.status().sample_rate == hz;
    frames(&mut host, &mut live, start, "the run starts over at 48 kHz", at(48_000.0));
    assert_eq!(host.status().sample_rate_chosen, Some(48_000.0), "the transport reads out the pick");
    host.follow_settings(Some(picked.clone()));
    assert_eq!(host.restart_pending(), None, "the same settings again change nothing");
    // with no rate picked the host chooses: none for a run nobody hears, the audio rate once one
    // is heard (Listen joining a run without it starts the run over); a run no longer heard keeps it
    host.follow_settings(Some(SimSettings { sample_rate: None, ..picked }));
    frames(&mut host, &mut live, start, "the run starts over with no sample rate", at(0.0));
    host.act(&SimAction::Listen { id: 9, sink: SinkSlot::new(Box::new(Counting(count.clone()))) });
    assert_eq!(host.restart_pending(), Some(Some("the audio rate")), "Listen joining a run without the audio rate starts it over at it");
    frames(&mut host, &mut live, start, "the heard run steps at the audio rate", at(AUDIO_SAMPLE_RATE));
    // in Realtime the host's choice is the Realtime rate for the run and the common audio rate for
    // its takes: a heard run at the audio rate starts over at it, named, and leaving Realtime brings
    // the audio rate back
    assert!(host.command(&SimCmd::SetRealtime(true)), "Realtime is a bench command");
    assert_eq!(host.restart_pending(), Some(Some("Realtime")), "Realtime starts a heard run at the audio rate over, named");
    frames(&mut host, &mut live, start, "the heard run starts over at the Realtime rate", at(REALTIME_SAMPLE_RATE));
    assert_eq!(host.status().recording_rate, REALTIME_RECORDING_RATE, "its takes are made at the common audio rate, under the run's");
    assert!(host.command(&SimCmd::SetRealtime(false)), "Realtime is left by a bench command");
    frames(&mut host, &mut live, start, "out of Realtime the heard run starts over at the audio rate", at(AUDIO_SAMPLE_RATE));
    host.act(&SimAction::Unlisten { id: 9 });
    assert_eq!((host.restart_pending(), host.status().sample_rate), (None, AUDIO_SAMPLE_RATE), "a run no longer heard keeps its rate");
    host.follow_settings(None);
    assert!(host.command(&SimCmd::SetSampleRate(Some(96_000.0))), "without a project the rate is a bench command");
    assert_eq!((host.restart_pending(), host.status().sample_rate_chosen), (None, Some(96_000.0)), "the pick that matches the run's rate starts nothing over");

    // the record keeps whole the signals any view probes, and only while one asks
    assert!(host.whole().iter().all(|&w| !w), "with no view asking, every signal keeps the front only");
    host.act(&SimAction::Probes { id: 3, rows: vec![1, 4] });
    host.act(&SimAction::Probes { id: 4, rows: vec![2, 4] });
    assert_eq!(host.whole(), [false, true, true, false, true], "every view's probes are kept whole");
    host.act(&SimAction::Probes { id: 4, rows: Vec::new() });
    assert_eq!(host.whole(), [false, true, false, false, true], "a withdrawn ask leaves the others'");
    host.act(&SimAction::Probes { id: 3, rows: Vec::new() });
    assert!(host.whole().iter().all(|&w| !w), "and none keeps none whole");

    // Realtime must keep up: a scope's zoom asks no resolution there, only outside it
    assert!(host.command(&SimCmd::SetSimMode(SimMode::Fast)) && host.command(&SimCmd::SetRate(1.0)), "Fast at 1x");
    let none = host.res();
    host.act(&SimAction::ShowScope { id: 11, sink: SinkSlot::new(Box::new(Counting(count.clone()))), column: 2e-6 });
    assert_eq!(host.res(), 0.5e-6, "out of Realtime a scope zoomed in asks a quarter of its column");
    assert!(host.command(&SimCmd::SetRealtime(true)), "Realtime is a bench command");
    assert_eq!(host.res(), none, "in Realtime the open scope asks nothing: the resolution with no scope open");
    frames(&mut host, &mut live, start, "the worker in Realtime steps at it", runs_at(none));
    assert!(host.command(&SimCmd::SetRealtime(false)), "Realtime is left by a bench command");
    assert_eq!(host.res(), 0.5e-6, "out of Realtime the scope's zoom asks again");
    host.act(&SimAction::HideScope { id: 11 });
}

/// The lamp sink of one run: a run's start (`begin`) clears the points, and counts the runs.
struct RunLamps(LampPoints, Arc<AtomicUsize>);

impl Sink for RunLamps {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        self.1.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut p) = self.0.lock() {
            p.1.clear();
        }
        Lamps(self.0.clone()).begin(unknowns);
    }
    fn point(&mut self, at: f64, outer: Option<f64>, x: &[f64]) {
        Lamps(self.0.clone()).point(at, outer, x);
    }
}

/// While the recording plays, a value edit goes into the running run and never restarts it; with
/// nothing of the recording heard yet, it goes in at the run's front, as live. A knob drag takes
/// each value as it comes, heard in the same run from where it applies (LED 0's current through
/// twice its resistance), the run's clean stretch ended there; a resistor edited in the circuit is
/// a value edit too, the circuit's value taken in the same way. An edit that is no value (a part
/// taken out) starts the run over, named, from 0 with the circuit's values, as does a program where
/// there was none; the same program built again does not. Live again, an edit changes the running
/// run where it is and ends its clean stretch there.
#[test]
fn a_value_edit_while_the_recording_plays_goes_into_the_run() {
    let b0 = BoardId(0);
    let design = Arc::new(ledctl());
    let circuit = &design.unit(b0).expect("ledctl has board 0").circuit;
    let lamp_r = circuit.comp("lamps[0].r").expect("ledctl has lamps[0].r");
    let r0 = CompRef { board: b0, comp: lamp_r.id };
    let qty = lamp_r.value.as_ref().and_then(|v| v.qty).expect("lamp resistor value");
    let ohms = qty.si;
    let lamp_tol = 2.0 * FAST_RELTOL;
    let led0 = |means: [f64; 3], sink_ohms: f64, r: f64| (means[1] * sink_ohms / r, (1.0 - DUTY) * lamp_on_current(means[0], r));
    let t_now = |host: &SimHost| host.values().map_or(0.0, |v| v.t);

    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(ledctl_bench(10e3, [DUTY; 8]));
    host.commit(&design);
    let (lamps, runs) = (LampPoints::default(), Arc::new(AtomicUsize::new(0)));
    host.act(&SimAction::ShowScope { id: 6, sink: SinkSlot::new(Box::new(RunLamps(lamps.clone(), runs.clone()))), column: 0.0 });
    let mut live = SimLive::default();
    host.set_rendering(true);
    frames(&mut host, &mut live, start, "the first run", |h| h.values().is_some_and(|v| v.t > 2e-3));
    let first = host.content().cloned().expect("a run has its content");
    assert!(live.runs.current().is_some_and(|c| Arc::ptr_eq(&c, &first)), "the views' slot holds the run's content for the Listen tap");
    let (i0, want) = led0(lamp_window(&mut host, &mut live, start, &lamps, 1e-3, ohms, "LED 0 at R"), ohms, ohms);
    assert!((i0 / want - 1.0).abs() <= lamp_tol, "LED 0 at R = {ohms} ohm: {i0} A, the green curve gives {want} A");

    // a knob drag, a value a frame: each goes into the running run at its front
    let mut edited_at = f64::INFINITY;
    for k in [1.5, 1.8, 2.0] {
        edited_at = edited_at.min(t_now(&host));
        assert!(host.command(&SimCmd::SetParam { comp: r0, value: k * ohms }), "a resistor's value is an edit");
        host.step(start.elapsed().as_secs_f64(), &mut live);
    }
    assert!(host.restart_pending().is_none() && host.content().is_some_and(|c| Arc::ptr_eq(c, &first)), "no restart: the running run takes the drag");
    assert_eq!(first.clean_until(), edited_at, "the run's samples follow its key up to the drag's first value, where the host saw the run");
    assert!(host.heard_edit().is_none(), "nothing of the recording is heard yet: the drag went in at the front");
    let taken = command_taken(&mut host, &mut live, start);
    let (i0, want) = led0(lamp_window(&mut host, &mut live, start, &lamps, period_after(taken), ohms, "LED 0 at 2R"), ohms, 2.0 * ohms);
    assert!((i0 / want - 1.0).abs() <= lamp_tol, "LED 0 at 2R = {} ohm in the same run: {i0} A, the green curve gives {want} A", 2.0 * ohms);

    // the resistor edited in the circuit, as the schematic edits it: the circuit's value, in place
    let edit_circuit = |design: &Design, generation: u64, edit: &dyn Fn(&mut fab_elec_circuit::Circuit)| {
        let mut edited = design.clone();
        edited.generation += generation;
        let unit = edited.units.iter_mut().find(|u| u.id == b0).expect("ledctl has board 0");
        edit(Arc::make_mut(&mut unit.circuit));
        Arc::new(edited)
    };
    let valued = edit_circuit(&design, 3, &|c| {
        let value = c.comps.iter_mut().find(|c| c.path == "lamps[0].r").and_then(|c| c.value.as_mut()).expect("lamps[0].r has a value");
        (value.text, value.qty) = ("330".into(), Some(fab_elec_circuit::Quantity { si: 330.0, ..qty }));
    });
    host.commit(&valued);
    frames(&mut host, &mut live, start, "the circuit's value taken in", |h| !h.lowering());
    assert!(runs.load(Ordering::Relaxed) == 1 && host.restart_pending().is_none() && host.content().is_some_and(|c| Arc::ptr_eq(c, &first)),
        "a value edited in the circuit is no restart either");
    let taken = command_taken(&mut host, &mut live, start);
    let means = lamp_window(&mut host, &mut live, start, &lamps, period_after(taken), ohms, "LED 0 at 330 ohm");
    let (i0, want) = led0(means, ohms, 330.0);
    assert!((i0 / want - 1.0).abs() <= lamp_tol, "LED 0 at the circuit's 330 ohm in the same run: {i0} A, the green curve gives {want} A");

    // a part taken out: the run starts over from 0 with the circuit's values, named for the edit
    let fewer = edit_circuit(&valued, 3, &|c| c.comps.retain(|comp| comp.path != "reg.c_in"));
    host.commit(&fewer);
    frames(&mut host, &mut live, start, "the part's restart", |h| {
        assert!(h.lowered().is_some(), "a restart keeps the lowering (and the firmware bound to it) at every frame");
        h.content().is_some_and(|c| !Arc::ptr_eq(c, &first)) && h.values().is_some_and(|v| v.t > 2e-3)
    });
    let second = host.content().cloned().expect("the new run has its content");
    assert_eq!(runs.load(Ordering::Relaxed), 2, "the part's edit restarted the run once");
    assert_eq!(second.edit.as_deref(), Some("the circuit edit"), "the new run names the edit that started it");
    assert_ne!(second.key, first.key, "the circuit is in the content key");
    let t0 = lamps.lock().ok().and_then(|p| p.1.first().map(|q| q.0));
    assert!(t0.is_some_and(|t| t < 1e-9), "the new run starts from 0 (its first point is its first step's): {t0:?}");
    let (i0, want) = led0(lamp_window(&mut host, &mut live, start, &lamps, 1e-3, ohms, "LED 0 at 330 ohm again"), ohms, 330.0);
    assert!((i0 / want - 1.0).abs() <= lamp_tol, "LED 0 at the circuit's 330 ohm from the new run's start: {i0} A, the green curve gives {want} A");

    // a program where there was none restarts the run, named as the firmware's first build (with
    // no glue to wait for it, the run started without one); the same program built again (a new
    // maker) does not
    fn program(_: &fab_elec_bench::Lowered, _: u32) -> Option<fab_elec_sim::cosim::CoSimDevice> {
        None
    }
    host.set_cosim(Some((Arc::new(program) as CoSimMaker, 7)));
    assert_eq!(host.restart_pending(), Some(Some("the firmware's first build")), "a first program is an edit, named as the first build");
    frames(&mut host, &mut live, start, "the firmware's restart", |h| h.content().is_some_and(|c| !Arc::ptr_eq(c, &second)) && h.values().is_some_and(|v| v.t > 2e-3));
    let third = host.content().cloned().expect("the new run has its content");
    assert!(third.key != second.key, "the program is in the key");
    host.set_cosim(Some((Arc::new(program) as CoSimMaker, 7)));
    host.step(start.elapsed().as_secs_f64(), &mut live);
    assert!(host.restart_pending().is_none() && host.content().is_some_and(|c| Arc::ptr_eq(c, &third)), "the same program built again restarts nothing");

    // live again: an edit changes the running run where it is and ends the clean stretch there
    host.set_rendering(false);
    let t = t_now(&host);
    assert!(host.command(&SimCmd::SetParam { comp: r0, value: 470.0 }));
    host.step(start.elapsed().as_secs_f64(), &mut live);
    assert!(host.restart_pending().is_none() && host.content().is_some_and(|c| Arc::ptr_eq(c, &third)), "live, an edit restarts nothing");
    assert_eq!(third.clean_until(), t, "the run's samples follow its key up to the edit, as the host saw the run then");
    eprintln!("content keys: {:032x} {:032x} {:032x}; runs {}", first.key, second.key, third.key, runs.load(Ordering::Relaxed));
}

/// Every sample the Listen tap's ring holds, drained as the audio device would, in order.
fn drain(ring: &SampleRing, heard: &mut Vec<f32>) {
    let mut block = [0.0f32; 1024];
    loop {
        let n = ring.pop(&mut block);
        if n == 0 {
            return;
        }
        heard.extend_from_slice(&block[..n]);
    }
}

/// The mean pitch of audio at `rate` Hz from its rising zero crossings, interpolated.
fn heard_pitch(samples: &[f32], rate: f64) -> f64 {
    let rises: Vec<f64> = samples.windows(2).enumerate().filter_map(|(i, w)| {
        let (a, b) = (f64::from(w[0]), f64::from(w[1]));
        (a <= 0.0 && b > 0.0).then(|| i as f64 + a / (a - b))
    }).collect();
    assert!(rises.len() > 20, "a tone is heard: {} rising crossings", rises.len());
    (rises.len() - 1) as f64 * rate / (rises[rises.len() - 1] - rises[0])
}

/// A knob turned the way the closed box turns one is heard: the tone box's PITCH knob, turned
/// from where its design puts it to the top through the host (`SimAction::Cmd(SetParam)`, as a
/// knob's drag sends it) while the key is held, raises the note in the Listen tap's audio from
/// the RC formula's pitch at mid travel to the formula's pitch at the top. Values then edited in
/// the circuit (the pot twice as large, r_t smaller) go into the run where it is, its numbering
/// kept, the knob still at the top (the pot's halves split where the knob stands, not where the
/// circuit has it): the note heard is the formula's with the new parts. The same documents and
/// path as the app's, nothing named but the parts' paths.
#[test]
fn a_knob_turned_through_the_host_is_heard_at_its_formula_pitch() {
    let sources = fab_elec_fixtures::docs("tonebox").expect("the tone box is a stock project");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("the tone box evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "box").expect("the tone box has its bench");
    let design = Arc::new(ev.design.clone());
    let unit = design.unit(bench.scope_id()).expect("the bench runs the board");
    let comp = |path: &str| CompRef { board: unit.id, comp: unit.circuit.comp(path).unwrap_or_else(|| panic!("the tone box has {path}")).id };
    let (key, pitch) = (comp("key"), comp("pitch"));

    // the RC formula with the generic Schmitt card, r_t 100k in series with the 250k pot's B-W half
    let libs = fab_elec_bench::ModelLibs::generic();
    let formula_of = |r_t: f64, pot: f64, pos: f64| {
        let r = r_t + fab_elec_bench::input::Taper::Linear.halves(pot, pos, libs.pot_end).1;
        let (vp, vm, vdd) = (libs.schmitt.vt_plus, libs.schmitt.vt_minus, libs.schmitt.vdd);
        1.0 / (r * 10e-9 * ((vp * (vdd - vm)) / (vm * (vdd - vp))).ln())
    };
    let formula = |pos: f64| formula_of(100e3, 250e3, pos);

    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    host.commit(&design);
    // the jack's net heard as the app's Listen hears it: one voice, DC blocked, at 48 kHz
    const RATE: f64 = 48_000.0;
    let ring = SampleRing::new(4096);
    ring.set_device_rate(RATE as u32);
    ring.set_open(true);
    let voices = vec![TapVoice { net: "OUT".into(), reference: None, gain: 0.4 }];
    let tap = AudioTap::new(TapSpec { voices, latency: 0.06, dc_block_hz: 5.0, mix: TapMix::new(1) }, Arc::clone(&ring));
    host.act(&SimAction::Listen { id: 1, sink: SinkSlot::new(Box::new(tap)) });
    let mut live = SimLive::default();
    frames(&mut host, &mut live, start, "the box runs", |h| h.values().is_some());
    assert!(host.error().is_none(), "the box runs: {:?}", host.error());
    host.act(&SimAction::Cmd(SimCmd::press(key, true)));

    // half a second with the key held, the knob where the design puts it; the last 0.2 s is heard
    let mut heard = Vec::new();
    frames(&mut host, &mut live, start, "the tone at mid travel", |_| {
        drain(&ring, &mut heard);
        heard.len() >= (0.5 * RATE) as usize
    });
    let sent = heard.len();
    let before = heard_pitch(&heard[sent - (0.2 * RATE) as usize..sent], RATE);
    host.act(&SimAction::Cmd(SimCmd::SetParam { comp: pitch, value: 1.0 }));
    host.step(start.elapsed().as_secs_f64(), &mut live);
    assert_eq!(&*live.turned, &[(pitch, 1.0)], "every view sees the knob at the top");

    // the samples the worker made before it took the turn are at most the ring and the tap's
    // lead ahead of what was heard; 0.2 s heard past them
    let from = sent + ring.capacity() + (0.06 * RATE) as usize + (0.05 * RATE) as usize;
    frames(&mut host, &mut live, start, "the tone turned up", |_| {
        drain(&ring, &mut heard);
        heard.len() >= from + (0.2 * RATE) as usize
    });
    let after = heard_pitch(&heard[from..from + (0.2 * RATE) as usize], RATE);

    // the circuit edited with the knob at the top: the pot 500k, r_t 68k
    let numbering = host.index().generation;
    let mut edited = (*design).clone();
    edited.generation += 3;
    let board = edited.units.iter_mut().find(|u| u.id == unit.id).expect("the box has its board");
    let circuit = Arc::make_mut(&mut board.circuit);
    for (path, si) in [("pitch", 500e3), ("r_t", 68e3)] {
        let value = circuit.comps.iter_mut().find(|c| c.path == path).and_then(|c| c.value.as_mut()).unwrap_or_else(|| panic!("{path} has a value"));
        value.qty = Some(fab_elec_circuit::Quantity { si, ..value.qty.unwrap_or_else(|| panic!("{path} has a quantity")) });
    }
    let edited = Arc::new(edited);
    host.commit(&edited);
    frames(&mut host, &mut live, start, "the circuit's values taken", |h| {
        drain(&ring, &mut heard);
        h.generation() == edited.generation && !h.lowering()
    });
    assert_eq!(host.index().generation, numbering, "values edited in the circuit go into the run: its numbering is kept");
    assert_eq!(&*live.turned, &[(pitch, 1.0)], "the knob stays at the top");
    let from = heard.len() + ring.capacity() + (0.06 * RATE) as usize + (0.05 * RATE) as usize;
    frames(&mut host, &mut live, start, "the tone with the circuit's new values", |_| {
        drain(&ring, &mut heard);
        heard.len() >= from + (0.2 * RATE) as usize
    });
    let edited_tone = heard_pitch(&heard[from..from + (0.2 * RATE) as usize], RATE);
    host.act(&SimAction::Cmd(SimCmd::Stop));
    let want_edited = formula_of(68e3, 500e3, 1.0);
    assert!((edited_tone / want_edited - 1.0).abs() < 0.02, "with the new values and the knob at the top the heard tone is {edited_tone:.1} Hz, the formula's {want_edited:.1} Hz within 2 % (at the circuit's own place {:.1} Hz)", formula_of(68e3, 500e3, 0.5));

    let (want_before, want_after) = (formula(0.5), formula(1.0));
    assert!((before / want_before - 1.0).abs() < 0.02, "at mid travel the heard tone is {before:.1} Hz, the formula's {want_before:.1} Hz within 2 %");
    assert!((after / want_after - 1.0).abs() < 0.02, "turned up the heard tone is {after:.1} Hz, the formula's {want_after:.1} Hz within 2 %");
    assert!(after > before * 2.0, "turning the knob up raises the note heard: {before:.1} Hz to {after:.1} Hz");
}

/// A board's output jack is loaded as what is plugged in, found by its part's data alone: the
/// tone box runs at the audio rate with nothing heard, a line input (10 kOhm a channel) loads its
/// jack in the run's content key, and headphones (32 Ohm) start the run over under another key with
/// the held tone heard far quieter; the jack's own load (open) wins over that default, louder than
/// into a line input. With nothing heard, the jack is recorded from each run's start at the run's
/// rate, under its run's key, its line out once for both channels.
#[test]
fn an_output_jack_is_loaded_by_what_is_plugged_in_and_keyed_by_it() {
    let sources = fab_elec_fixtures::docs("tonebox").expect("the tone box is a stock project");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("the tone box evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "box").expect("the tone box has its bench");
    let design = Arc::new(ev.design.clone());
    let unit = design.unit(bench.scope_id()).expect("the bench runs the board");
    let key = CompRef { board: unit.id, comp: unit.circuit.comp("key").expect("the tone box has its key").id };

    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    host.commit(&design);
    let mut live = SimLive::default();
    frames(&mut host, &mut live, start, "the box runs", |h| h.values().is_some() && h.content().is_some());
    assert!(host.error().is_none(), "the box runs: {:?}", host.error());
    assert_eq!(host.status().sample_rate, AUDIO_SAMPLE_RATE, "a board with an output jack runs at the audio rate with nothing heard");
    let line_key = host.content().expect("the run is keyed").key;
    let jack_take = |live: &SimLive| live.listen.as_ref().and_then(|bus| bus.tapes.current(JACK_TAP));

    // the key held, the line out's swing over 10 ms of the tone from 10 ms after QUIET fell
    // (the key's press reached the logic), from the run's take: its grid frames are where they are
    // however fast the box steps and whichever publications the test's frames see
    let net = |name: &str| NetRef { board: unit.id, net: unit.circuit.net(name).unwrap_or_else(|| panic!("the tone box has {name}")).id };
    let (out, quiet) = (net("OUT"), net("QUIET"));
    let heard = |lowered: &fab_elec_bench::Lowered| {
        let mut names: Vec<_> = lowered.circuit.nodes.iter().filter(|n| n.heard).map(|n| n.name.clone()).collect();
        names.sort();
        names
    };
    assert_eq!(heard(host.lowered().expect("the tone box is lowered")), ["OSC", "OUT", "QUIET"],
        "the stock jack and bench probes declare their voltage nets heard, excluding ground");
    // OUT is also a stock probe. Move the probes off the jack to check the two declarations independently.
    let mut bench_diff = bench.spec.clone();
    bench_diff.probes = [Probe::VDiff(net("TIMING"), net("MID")), Probe::V(net("GND"))].into_iter()
        .map(|probe| ScopeProbe { probe, label: String::new(), tip: Tip::default() }).collect();
    let lowered = fab_elec_bench::lower(&design, unit.id, &bench_diff, &ModelLibs::generic());
    assert_eq!(heard(&lowered), ["MID", "OUT", "TIMING"],
        "the unprobed jack and both points of a differential probe are heard, and a ground probe stays unheard");

    let swing = |host: &mut SimHost, live: &mut SimLive, what: &str| {
        host.act(&SimAction::Cmd(SimCmd::press(key, true)));
        let [out, quiet] = [out, quiet].map(|n| host.index().net(n).expect("the net is lowered"));
        let (mut slots, mut swing) = (Vec::new(), 0.0);
        frames(host, live, start, what, |h| {
            slots.clear();
            h.take().net_envelope(quiet, f64::NEG_INFINITY, &mut slots);
            let Some(from) = slots.iter().find(|s| s.max < 1.0).map(|s| s.t0 + 10e-3) else { return false };
            slots.clear();
            h.take().net_envelope(out, from, &mut slots);
            let (mut covered, mut low, mut high) = (0.0, f32::INFINITY, f32::NEG_INFINITY);
            for s in slots.iter().filter(|s| s.t0 >= from) {
                (covered, low, high) = (covered + (s.t1 - s.t0), low.min(s.min), high.max(s.max));
                if covered >= 10e-3 {
                    swing = high - low;
                    return true;
                }
            }
            false
        });
        swing
    };
    let line = swing(&mut host, &mut live, "the tone into a line input");
    fab_elec_sim::recording::settle_tapes();
    let line_take = jack_take(&live).expect("the jack is recorded with nothing heard");
    let (keyed, rate) = (line_take.content.as_ref().map(|c| c.key), line_take.rate);
    assert!(keyed == Some(line_key) && rate == AUDIO_SAMPLE_RATE && line_take.frames() > 0, "the jack's take starts with its run, at the run's rate: {keyed:?} against {line_key:?}, {rate} Hz, {} frames", line_take.frames());
    let channel = voice_key(&TapVoice { net: "OUT".into(), reference: None, gain: LISTEN_GAIN / 2.0 }, LISTEN_DC_HZ);
    assert!(line_take.voices == 4 && line_take.keys.iter().filter(|&&k| k == channel).count() == 1,
        "the line out on both channels of the jack is recorded once, beside the bench's three probes: {} voices", line_take.voices);

    // headphones plugged in: the running tone takes the load at once (a retune), and a run from
    // t = 0 with it is keyed apart from one into a line input
    host.act(&SimAction::Cmd(SimCmd::SetJackLoad(Some(AudioLoad::Phones))));
    host.act(&SimAction::Cmd(SimCmd::Stop));
    host.act(&SimAction::Cmd(SimCmd::Play));
    host.commit(&design);
    frames(&mut host, &mut live, start, "the run starts over into headphones", |h| {
        h.content().is_some_and(|c| c.key != line_key) && h.values().is_some_and(|v| v.generation == h.index().generation)
    });
    let phones = swing(&mut host, &mut live, "the tone into headphones");
    let phones_take = jack_take(&live).expect("the jack is recorded in the next run");
    let phones_key = host.content().expect("the run is keyed").key;
    assert!(!Arc::ptr_eq(&phones_take, &line_take) && phones_take.content.as_ref().is_some_and(|c| c.key == phones_key), "the next run's take is the jack's next, under its key");
    host.act(&SimAction::Cmd(SimCmd::Stop));

    // the jack's own load over the default (the Bench's per-jack pick): open, nothing plugged in
    let jack = fab_elec_bench::audio::output_jacks(&unit.circuit).first().expect("the tone box has its output jack").refdes.clone();
    host.act(&SimAction::Cmd(SimCmd::SetJackLoadOf(jack.clone(), Some(AudioLoad::Open))));
    host.act(&SimAction::Cmd(SimCmd::Play));
    host.commit(&design);
    frames(&mut host, &mut live, start, "the run starts over with the jack open", |h| {
        h.content().is_some_and(|c| c.key != line_key && c.key != phones_key) && h.values().is_some_and(|v| v.generation == h.index().generation)
    });
    assert_eq!(host.status().jack_load, Some(AudioLoad::Phones), "the default stays: {jack} has its own load");
    let open = swing(&mut host, &mut live, "the tone into an open jack");
    host.act(&SimAction::Cmd(SimCmd::Stop));

    // the divider's 8 kohm into 5 kohm (two channels of 10 kohm) swings about 0.4 V; into 16 ohm
    // (two of 32 ohm) a few millivolts; unloaded, more than into a line input
    assert!(line > 0.2, "the held tone swings the line out into a line input: {line:.4} V");
    assert!(phones < line * 0.1, "headphones load the jack far harder: {phones:.4} V against {line:.4} V into a line input");
    assert!(open > line * 1.2, "an open jack leaves the divider unloaded: {open:.4} V against {line:.4} V into a line input");
}

/// The recording rate makes a run's takes without changing a run that never goes in rounds: the
/// tone box simulated at 96 kHz and recorded at 48 kHz with no helper cores keeps its run key, and
/// its 48 kHz take is its 96 kHz take through the read's rate change (`listen::rate_change`), to
/// the takes' f32 rounding: both are the run's read, at 96 kHz as it is and at 48 kHz through the
/// rate change. The jacks' recording saves as 48 kHz WAV. The sample rate changes the run itself:
/// another key. A run that can go in rounds holds Fast's plan accurate to half its takes' rate, at
/// most half its own (`PartOpts::band`): a take at or over the run's rate keeps the key, one under
/// it narrows the band and is another run.
#[test]
fn the_recording_rate_makes_the_takes_and_the_sample_rate_the_run() {
    use fab_elec_sim::listen::{rate_change, RATE_HALF};
    use fab_elec_sim::recording::{settle_tapes, write_wav};
    let sources = fab_elec_fixtures::docs("tonebox").expect("the tone box is a stock project");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("the tone box evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "box").expect("the tone box has its bench");
    let design = Arc::new(ev.design.clone());

    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    let mut live = SimLive::default();
    let bus = live.listen_bus();
    // a recorder of the line out without a DC block: each frame is the run's mean over its period
    let spec = TapSpec { voices: vec![TapVoice { net: "OUT".into(), reference: None, gain: 1.0 }], latency: 0.06, dc_block_hz: 0.0, mix: TapMix::new(1) };
    let recorder = AudioTap::recorder(spec, bus.ring.clone(), bus.record(7, 7, 1.0));
    host.act(&SimAction::ShowScope { id: 7, sink: SinkSlot::new(Box::new(recorder)), column: 0.0 });
    host.commit(&design);
    let status = host.status();
    assert_eq!((status.recording_rate, status.recording_rate_chosen), (AUDIO_SAMPLE_RATE, None), "with nothing picked the takes are made at the default audio rate");
    // a run recording at `hz`, to its rest: its key and its take
    let rested = |host: &mut SimHost, live: &mut SimLive, hz: f64, what: &str| {
        frames(host, live, start, what, |h| h.restart_pending().is_none() && h.content().is_some_and(|c| c.recording_rate == hz) && h.values().is_some_and(|v| v.steady));
        settle_tapes();
        let tape = bus.tapes.current(7).expect("the recorder tapes from the run's start");
        let mut out = vec![0.0f32; tape.frames() as usize];
        assert_eq!(tape.read(0, &mut out).expect("the take reads back"), out.len());
        assert_eq!(tape.rate, hz, "{what}: the take is made at the recording rate");
        (host.content().expect("the run is keyed").key, out)
    };
    let (key96, at96) = rested(&mut host, &mut live, AUDIO_SAMPLE_RATE, "the box rests, recorded at 96 kHz");

    assert!(host.command(&SimCmd::SetRecordingRate(Some(48_000.0))), "without a project the recording rate is a bench command");
    assert_eq!(host.restart_pending(), Some(Some("the recording rate")), "another recording rate starts the run over, named");
    let (key48, at48) = rested(&mut host, &mut live, 48_000.0, "the box rests, recorded at 48 kHz");
    assert_eq!(key48, key96, "with no helper cores the run never goes in rounds: the recording rate leaves it as it was");
    let status = host.status();
    assert_eq!((status.sample_rate, status.recording_rate, status.recording_rate_chosen), (AUDIO_SAMPLE_RATE, 48_000.0, Some(48_000.0)), "the run simulates at 96 kHz and records at 48 kHz");
    // Compare 48 kHz frames whose rate change reaches only frames the 96 kHz tape holds and whose
    // distance from the 48 kHz take's end exceeds the rate change's half-width. At rest the read's
    // last samples go out as they are, so the take's last frames come through a rate change cut short there.
    let last = at96.len() as i64 - 1;
    let n = at48.len().saturating_sub(RATE_HALF).min(((last - 2 * RATE_HALF as i64) / 2 + 1).max(0) as usize);
    assert!(n >= 2048, "both takes hold the box's start: {} and {} frames", at48.len(), at96.len());
    let (low, high) = at96[..2 * n].iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    assert!(high - low > 1e-3, "the line out moves as the box starts: {low} to {high} V");
    for k in 0..n {
        let (mut through, mut scale) = (0.0, 0.0f64);
        let sum = rate_change(2.0 * k as f64, 2.0, 0, last, |j, w| {
            let x = f64::from(at96[j as usize]);
            (through, scale) = (through + w * x, scale.max(x.abs()));
            true
        });
        through /= sum.expect("the 96 kHz tape holds every frame the rate change reads");
        let made = f64::from(at48[k]);
        // both takes are f32 roundings of the run's read: the 48 kHz frame's, and each 96 kHz
        // frame's under weights whose magnitudes sum to under 2.1 (a half-band sinc's over these
        // taps)
        let tol = 4.0 * f64::from(f32::EPSILON) * scale.max(made.abs()).max(1e-6);
        assert!((made - through).abs() <= tol, "48 kHz frame {k} is the 96 kHz take through the read's rate change: {made} against {through}");
    }
    settle_tapes();
    let jacks = bus.tapes.current(JACK_TAP).expect("the jack is recorded with nothing heard");
    let mut wav = Vec::new();
    write_wav(&jacks, &TapMix::new(jacks.voices), &mut wav).expect("the jacks' recording saves");
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().expect("a WAV header")), 48_000, "a saved WAV is at the recording rate");

    assert!(host.command(&SimCmd::SetSampleRate(Some(48_000.0))), "the sample rate is a bench command too");
    assert_eq!(host.restart_pending(), Some(Some("the sample rate")), "another sample rate starts the run over, named");
    frames(&mut host, &mut live, start, "the run starts over at 48 kHz", |h| h.restart_pending().is_none() && h.content().is_some() && h.status().sample_rate == 48_000.0);
    assert_ne!(host.content().expect("the run is keyed").key, key96, "the sample rate changes the run");
    assert_eq!(host.status().recording_rate, 48_000.0, "and leaves the recording rate as picked");
    host.act(&SimAction::Cmd(SimCmd::Stop));

    // with helper cores the run can go in rounds, and its plan is held to the band its takes ask
    let mut host = SimHost::with_spawn(Box::new(Helped));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    assert!(host.command(&SimCmd::SetSampleRate(Some(AUDIO_SAMPLE_RATE))), "the sample rate is a bench command");
    let mut live = SimLive::default();
    host.commit(&design);
    let keyed = |host: &mut SimHost, live: &mut SimLive, hz: f64, what: &str| {
        frames(host, live, start, what, |h| h.restart_pending().is_none() && h.content().is_some_and(|c| c.recording_rate == hz));
        host.content().expect("the run is keyed").key
    };
    let at96 = keyed(&mut host, &mut live, AUDIO_SAMPLE_RATE, "with helper cores the box runs, recorded at 96 kHz");
    assert!(host.command(&SimCmd::SetRecordingRate(Some(192_000.0))), "the recording rate is a bench command");
    let over = keyed(&mut host, &mut live, 192_000.0, "the box runs over, recorded at 192 kHz");
    assert_eq!(over, at96, "a take over the run's rate leaves the band at half the run's: the run as it was");
    assert!(host.command(&SimCmd::SetRecordingRate(Some(48_000.0))), "the recording rate is a bench command");
    let under = keyed(&mut host, &mut live, 48_000.0, "the box runs over, recorded at 48 kHz");
    assert_ne!(under, at96, "a take under the run's rate narrows the band Fast's plan is held accurate to (24 kHz): another run");
    host.act(&SimAction::Cmd(SimCmd::Stop));
}

/// In Realtime, which must keep up, the trace writes only the rows a view shows: the probes kept
/// whole and the nets with a strip (`LiveSpec::fine`); a net no view shows has no point in the
/// record, its coarse envelope still in the take. A strip added mid-run gets its net at full rate
/// from the run's next point on, and its window shows no blank: the columns before that point take
/// the envelope (`MiniRing::fill_gaps`). Out of Realtime every row is written again.
#[test]
fn realtime_traces_the_rows_a_view_shows_and_a_strip_added_mid_run_has_no_blank() {
    let b0 = BoardId(0);
    let design = Arc::new(ledctl());
    let circuit = &design.unit(b0).expect("ledctl has board 0").circuit;
    let net_ref = |name: &str| NetRef { board: b0, net: circuit.net(name).unwrap_or_else(|| panic!("ledctl has net {name}")).id };
    let (v33, anode0, anode1, anode2) = (net_ref("3V3"), net_ref("lamps[0].d.A"), net_ref("lamps[1].d.A"), net_ref("lamps[2].d.A"));
    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec::default());
    host.set_bench(ledctl_bench(10e3, [DUTY; 8]));
    host.commit(&design);
    let mut live = SimLive::default();
    assert!(host.command(&SimCmd::SetRealtime(true)), "Realtime is a bench command");
    frames(&mut host, &mut live, start, "first values", |h| h.values().is_some());
    let index = live.index.clone();
    let row = move |net: NetRef| index.net(net).expect("the net is lowered");
    host.act(&SimAction::Probes { id: 3, rows: vec![row(v33)] });
    live.minis.set(TreeOwner::Sheet(b0, 0), &[anode0]);
    let shown = |h: &SimHost, net: NetRef| h.fine().is_some_and(|f| f.get(row(net)).copied().unwrap_or(false));
    frames(&mut host, &mut live, start, "the probe's and the strip's rows named", |h| shown(h, v33) && shown(h, anode0));
    assert_eq!(host.fine().map(|f| f.iter().filter(|&&w| w).count()), Some(2), "Realtime traces the probe's row and the strip's, no other");

    // the shown rows have every point from the first written on (the strip's in the open tail, the
    // front its record keeps), a net no view shows none in the record
    let first = |h: &SimHost, net: NetRef| (0..h.store().len()).find(|&i| h.store().signals[row(net)].at(i).is_finite());
    frames(&mut host, &mut live, start, "the shown rows written and the run past 30 ms", |h| {
        first(h, v33).is_some() && first(h, anode0).is_some() && h.values().is_some_and(|v| v.t > 30e-3)
    });
    let store = host.store();
    let open = store.time.chunks.len() * fab_elec_sim::trace::CHUNK;
    for (net, from) in [(v33, first(&host, v33)), (anode0, first(&host, anode0).map(|i| i.max(open)))] {
        let from = from.expect("a shown row's first point");
        assert!((from..store.len()).all(|i| store.signals[row(net)].at(i).is_finite()), "a shown row has every point from {from} on");
    }
    assert!((0..store.len()).all(|i| store.signals[row(anode2)].at(i).is_nan()), "a net no view shows has no point in Realtime's record");

    // a strip added mid-run: its net at full rate from its first point on, and once the take holds
    // the frames up to that point, no blank column in the strip's window
    let slots = live.minis.set(TreeOwner::Sheet(b0, 1), &[anode1]);
    let mini = slots[0] as usize * MINI_SLOT;
    frames(&mut host, &mut live, start, "the added strip's net written", |h| first(h, anode1).is_some());
    let first_t = host.store().time.at(first(&host, anode1).expect("the added net's first point"));
    frames(&mut host, &mut live, start, "the take past the added net's first point", |h| h.take().span().is_some_and(|(_, end)| end > first_t));
    host.step(start.elapsed().as_secs_f64(), &mut live);
    let header = host.mini().row(mini);
    assert!(header[0] <= header[1], "the added strip has a scale: {header:?}");
    let head = header[2] as usize;
    let blank: Vec<usize> = (0..MINI_COLS).filter(|&d| {
        let c = host.mini().row(mini + 1 + (head + MINI_RING - d) % MINI_RING);
        c[0] > c[1]
    }).collect();
    assert!(blank.is_empty(), "the added strip's window has no blank column: {} of {MINI_COLS} empty, {blank:?} columns back from its head", blank.len());
    let store = host.store();
    let from = first(&host, anode1).expect("the added net's first point").max(store.time.chunks.len() * fab_elec_sim::trace::CHUNK);
    assert!((from..store.len()).all(|i| store.signals[row(anode1)].at(i).is_finite()), "the added strip's net has every point from its first on");

    assert!(host.command(&SimCmd::SetRealtime(false)), "Realtime is left by a bench command");
    assert_eq!(host.fine(), None, "out of Realtime the trace writes every row");
    frames(&mut host, &mut live, start, "every row written again", |h| {
        let s = h.store();
        !s.is_empty() && s.signals.get(row(anode2)).is_some_and(|t| t.at(s.len() - 1).is_finite())
    });
}

/// The app's Exact is the bench's: a host built as the app builds it steps Exact at the spec
/// `bench_run` steps at (`bench_spec`), with the same tolerance (the kernel's default, which every
/// oracle judges at), largest step and resolution, so what the bench judges is what the app plays.
/// Fast keeps its own tolerance. A sim proof holds the running solvers to the spec as the mode
/// switches (`a_run_steps_each_mode_at_its_own_tolerance`).
/// Logic picked while a run plays: the organ lowers again with its eight oscillators as timing
/// cells and the run takes that lowering where it is (handed over, not started again); the
/// transport reads out the cells, and an oscillator's timing net, which its cell sets now, swings
/// between the thresholds as it did. Back in Fast the switch pairs return and the run goes on.
#[test]
fn logic_picked_while_the_organ_plays_hands_the_run_to_its_cells_and_back() {
    let sources = fab_elec_fixtures::docs("organ8").expect("organ8 is a stock project");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("organ8 evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "organ").expect("organ8 has its bench");
    let design = Arc::new(ev.design.clone());
    let unit = design.unit(bench.scope_id()).expect("the bench runs the organ");
    let timing = NetRef { board: unit.id, net: unit.circuit.net("v1.timing").expect("voice 1 has its timing net").id };
    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    host.commit(&design);
    let mut live = SimLive::default();
    // the timing net's lowest and highest voltage over the publications of the running lowering
    let swing = |h: &SimHost, seen: &mut (f32, f32)| {
        let Some(v) = h.values().filter(|v| v.generation == h.index().generation) else { return };
        if let Some(k) = h.index().net(timing) {
            *seen = (seen.0.min(v.net_min[k]), seen.1.max(v.net_max[k]));
        }
    };
    let mut fast = (f32::INFINITY, f32::NEG_INFINITY);
    frames(&mut host, &mut live, start, "the organ runs in Fast", |h| { swing(h, &mut fast); fast.0 < 2.0 && fast.1 > 2.6 });
    assert_eq!((host.status().mode, host.status().cells), (SimMode::Fast, 0), "Fast runs the switch pairs");

    assert!(host.command(&SimCmd::SetSimMode(SimMode::Logic)), "Logic is a bench command");
    let picked = host.values().map_or(0.0, |v| v.t);
    // the cells' lowering's first values
    frames(&mut host, &mut live, start, "the run takes the cells", |h| h.status().cells == 8 && h.values().is_some_and(|v| v.generation == h.index().generation));
    let taken = host.values().map_or(0.0, |v| v.t);
    assert!(taken >= picked, "the run goes on from where it was ({taken} s, {picked} s at the pick): handed over, not started again");
    assert_eq!(host.status().mode, SimMode::Logic, "the transport reads out Logic");
    let mut logic = (f32::INFINITY, f32::NEG_INFINITY);
    frames(&mut host, &mut live, start, "the timing net swings in Logic", |h| { swing(h, &mut logic); logic.0 < 2.0 && logic.1 > 2.6 });
    assert!(logic.0 > 1.6 && logic.1 < 3.0, "the cell keeps the timing net between its thresholds: {logic:?}");

    assert!(host.command(&SimCmd::SetSimMode(SimMode::Fast)), "and back to Fast");
    let back = host.values().map_or(0.0, |v| v.t);
    frames(&mut host, &mut live, start, "the switch pairs return", |h| h.status().cells == 0 && h.values().is_some_and(|v| v.t > back + 2e-3));
    assert!(host.error().is_none(), "the run goes on: {:?}", host.error());
}

#[test]
fn the_apps_exact_is_the_benchs() {
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_bench(ledctl_bench(10e3, [DUTY; 8]));
    host.set_sim_mode(SimMode::Exact);
    let (app, bench) = (host.live_spec().clone(), bench_spec());
    assert_eq!((host.res(), bench.res), (0.0, 0.0), "the app's Exact and the bench step every edge");
    assert_eq!(app.reltol_at(host.res()), bench.reltol_at(bench.res), "the app's Exact tolerance is the bench's");
    assert_eq!(app.reltol_at(0.0), Options::default().reltol, "Exact's tolerance is the kernel's default, which oracles judge at");
    assert_eq!(app.max_step, bench.max_step, "the app's largest step in Exact is the bench's");
    host.set_sim_mode(SimMode::Fast);
    assert!(host.res() > 0.0, "Fast steps at a resolution");
    assert_eq!(host.live_spec().reltol_at(host.res()), FAST_RELTOL, "Fast keeps its own tolerance");
}

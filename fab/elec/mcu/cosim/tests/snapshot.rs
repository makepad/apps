//! A live run with a compiled AVR firmware attached, brought back to a snapshot
//! (`LiveSim::snapshot`, `restore`; the MCU by `CoSim::fork`), goes on exactly as it did.

use fab_elec_mcu_avr::machine::AvrMachine;
use fab_elec_mcu_cosim::{CoMachine, Config, EventSink, FwFrame};
use fab_elec_sim::cosim::{CoSimDevice, CoSimPin};
use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::alloc::{measure, Counting};
use fab_elec_sim::livesim::{LiveSim, LiveSpec, Snapshots};
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_sim::{GpioParams, ModelKind, PinMode, PinPull, Record, SimCircuit, Sink, SinkState, Source, Wave};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[global_allocator]
static COUNTING: Counting = Counting(std::alloc::System);

const HZ: u64 = 16_000_000;
const PARAMS: GpioParams = GpioParams { r_on_h: 26.0, r_on_l: 23.5, r_pull: 36e3, r_leak: 5.5e6, t_edge: 5e-9, vih: 0.524, vil: 0.418 };
const END: f64 = 5e-3;
/// How far a restored run is followed.
const HORIZON: f64 = 1e-3;

/// Every millisecond: the PWM's duty from ADC6, and the watched input copied to the LED.
const FIRMWARE: &str = "\
let pot = analog(ADC6)
let key = input(PB0, pull_none)
let tone = pwm(PB1, hz(490))
let led = output(PB5)
let tick = every(ms(1), fn() {
    duty(tone, analog_read(pot) >> 2)
    write(led, read(key))
})
";

/// A 5 V supply; a 300 Hz sine on ADC6; a 700 Hz square through 1 kohm on the input; the PWM
/// through 10 kohm into 100 nF clipped by a diode, with a noise current into it; the LED's load.
fn rig() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(5.0));
    c.gpio("u1#ADC6.io", "pot", "vdd", "0", PARAMS, PinMode::Input(PinPull::None));
    c.v("wiper", "pot", "0", Source::wave(Wave::Sin { vo: 2.5, va: 2.0, freq: 300.0, td: 0.0, theta: 0.0, phase: 0.0 }));
    c.gpio("u1#PB0.io", "key", "vdd", "0", PARAMS, PinMode::Input(PinPull::None));
    c.v("keys", "k", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 0.2e-3, tr: 20e-6, tf: 20e-6, pw: 0.7e-3, per: 1.43e-3 }));
    c.r("rk", "k", "key", 1e3);
    c.gpio("u1#PB1.io", "tone", "vdd", "0", PARAMS, PinMode::Input(PinPull::None));
    c.model("dm", ModelKind::Diode, &[("cjo", 1e-12)]);
    c.r("rf", "tone", "f", 10e3);
    c.c("cf", "f", "0", 100e-9);
    c.d("df", "f", "0", "dm");
    c.i("noise", "0", "f", Source::wave(Wave::Noise { seed: 3, sigma: 1e-6, dt: 10e-6, base: 10e-6 }));
    c.gpio("u1#PB5.io", "led", "vdd", "0", PARAMS, PinMode::Input(PinPull::None));
    c.r("load", "led", "0", 10e3);
    c
}

/// The rig with its MCU, whose events (statements too) go to `events`, keeping snapshots within
/// `span` of the front (`LiveSpec::snapshots`; 0: none) in `bytes`. With `co_state` a publication
/// reads the MCU's state, so an advance ends where the MCU settles (`CoSim::settle`); without, it
/// ends where its budget does, mid-stretch.
fn live(res: f64, events: Option<EventSink>, span: f64, bytes: u64, co_state: bool) -> LiveSim {
    let fw = fab_elec_mcu_emit_avr::firmware(FIRMWARE, HZ as f64, &[]).expect("the firmware builds");
    let snapshots = Snapshots { span, every: Duration::ZERO, share: f64::INFINITY, bytes };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 20e-6, res, trace: false, snapshots, co_state, ..LiveSpec::default() };
    let mut sim = LiveSim::new(rig(), ValuePlan::default(), 1, spec).expect("the rig's operating point converges");
    let row = |name: &str, pad: &str| CoSimPin { name: name.into(), gpio: format!("u1#{name}.io"), pad: pad.into(), vss: "0".into(), vdd: "vdd".into(), vih: PARAMS.vih, vil: PARAMS.vil };
    let pins = vec![row("ADC6", "pot"), row("PB0", "key"), row("PB1", "tone"), row("PB5", "led")];
    let mut dev = CoMachine::new(AvrMachine::new(&fw.words, HZ), Config { stretch: 16_000, debug: Some(Arc::new(fw.debug)) }, &pins).expect("the co-simulated MCU");
    if events.is_some() {
        dev.set_events(events, true);
    }
    sim.attach(Some(CoSimDevice { dev: Box::new(dev), pins }));
    sim
}

/// Where `got` and `want` from its `from`-th point part, if they do: the first point whose time
/// or any value differs in its bits, or `got` running past `want`'s end (or, `whole`, either
/// ending first).
fn parted(got: &Record, want: &Record, from: usize, whole: bool) -> Option<String> {
    let (at, x) = (&want.at[from..], &want.x[from..]);
    for (k, (t, v)) in got.at.iter().zip(&got.x).enumerate() {
        let Some(&w) = at.get(k) else { return Some(format!("point {k} at {t:e} s past the run's end")) };
        let same = w.to_bits() == t.to_bits() && x[k].len() == v.len() && x[k].iter().zip(v).all(|(a, b)| a.to_bits() == b.to_bits());
        if !same {
            let off = x[k].iter().zip(v).enumerate().find(|(_, (a, b))| a.to_bits() != b.to_bits()).map(|(u, (a, b))| format!(", {} {a:e} against {b:e}", got.names[u]));
            return Some(format!("point {k} at {t:e} s against {w:e} s{}", off.unwrap_or_default()));
        }
    }
    (whole && got.at.len() != at.len()).then(|| format!("{} points against {}", got.at.len(), at.len()))
}

/// In Exact and in Fast: a run stepped a loop top at a time with a snapshot at every 11th (so
/// they fall mid-stretch, between ADC reads, around the input's edges and the PWM's; and, with
/// the MCU's state read, where the MCU settled) is the straight run, bit for bit; each snapshot
/// restored in turn goes on as the straight run did from there for 1 ms, every point's time and
/// values by bits; the first, restored again last, goes on as it did the first time.
#[test]
fn a_restored_snapshot_with_an_avr_goes_on_as_the_run_did() {
    for (name, res, co_state) in [("Exact", 0.0, false), ("Fast", 1.0 / 96e3, false), ("Exact settling", 0.0, true)] {
        let mut straight = live(res, None, 0.0, 0, false);
        let mut want = Record::default();
        want.begin(&straight.solver().unknowns);
        while straight.t() < END {
            straight.advance(END, None, &mut want).unwrap_or_else(|d| panic!("{name}: the rig steps: {d}"));
        }

        let mut sim = live(res, None, 0.0, 0, co_state);
        let mut got = Record::default();
        got.begin(&sim.solver().unknowns);
        let mut snaps = Vec::new();
        let mut top = 0;
        while sim.t() < END {
            if top % 11 == 0 {
                snaps.push((got.at.len(), sim.snapshot().expect("the MCU forks")));
            }
            top += 1;
            sim.advance(END, Some(Duration::ZERO), &mut got).unwrap_or_else(|d| panic!("{name}: the rig steps: {d}"));
        }
        if let Some(d) = parted(&got, &want, 0, true) {
            panic!("{name}: a run taking snapshots a loop top at a time is the straight run: {d}");
        }
        eprintln!("{name}: {} points, {top} loop tops, {} snapshots", want.at.len(), snaps.len());
        for (k, (from, snap)) in snaps.iter().chain(snaps.first()).enumerate() {
            sim.restore(snap);
            let mut again = Record::default();
            again.begin(&sim.solver().unknowns);
            let until = (snap.t() + HORIZON).min(END);
            while sim.t() < until {
                sim.advance(until, None, &mut again).unwrap_or_else(|d| panic!("{name}: the restored rig steps: {d}"));
            }
            if let Some(d) = parted(&again, &want, *from, false) {
                panic!("{name}: restored at snapshot {k} ({:e} s), the run goes on as it did: {d}", snap.t());
            }
        }
    }
}

/// The events a sink took (the void ones dropped), how many it voided and refused; while `hold`
/// it refuses every batch (`held` of them).
#[derive(Default)]
struct Taken {
    got: Vec<(f64, FwFrame)>,
    voided: u64,
    refused: usize,
    hold: bool,
    held: usize,
}

/// An event sink that keeps what it takes, dropping the void events an offer names first, and
/// refuses the first of every `refuse` offers (0: never), and every one while held.
fn events(refuse: usize) -> (EventSink, Arc<Mutex<Taken>>) {
    let taken = Arc::new(Mutex::new(Taken::default()));
    let (t, mut offers) = (taken.clone(), 0usize);
    let sink: EventSink = Box::new(move |void: u64, batch: &mut Vec<(f64, FwFrame)>| {
        offers += 1;
        let mut t = t.lock().expect("the events");
        if t.hold {
            t.held += 1;
            return false;
        }
        if refuse != 0 && (offers - 1) % refuse == 0 {
            t.refused += 1;
            return false;
        }
        assert!(void as usize <= t.got.len(), "a void count within what the sink took");
        let kept = t.got.len() - void as usize;
        t.got.truncate(kept);
        t.voided += void;
        t.got.append(batch);
        true
    });
    (sink, taken)
}

/// A sink of the run that keeps its points; its state at a snapshot is how many it held, and a
/// rewind goes back to that.
#[derive(Default)]
struct Points {
    at: Vec<f64>,
    x: Vec<Vec<f64>>,
}

impl Sink for Points {
    fn point(&mut self, at: f64, _outer: Option<f64>, x: &[f64]) {
        self.at.push(at);
        self.x.push(x.to_vec());
    }
    fn save(&self) -> Option<Box<dyn SinkState>> {
        Some(Box::new(self.at.len()))
    }
    fn rewind(&mut self, _at: f64, saved: Option<&dyn SinkState>, _unknowns: &[UnknownInfo]) {
        let n = *saved.and_then(|s| s.downcast_ref::<usize>()).expect("the state this sink saved");
        self.at.truncate(n);
        self.x.truncate(n);
    }
}

/// An edit behind the front with the MCU's events on, statements included: the run goes back to a
/// snapshot, the MCU's copy there takes over the event sink (`CoSim::take_outputs`, `put_outputs`),
/// and the sink holds the event stream of the run that had the edit all along, by bits and in
/// order (each committed event once: what the running MCU sent past its copy's place is voided in
/// the stream, and what it had not sent yet goes up to there), as do the run's points. In Exact
/// and in Fast, with a sink that takes every batch, one that refuses every third, and one that is
/// held full from 3 ms until it has refused a batch, the edit then coming while the MCU waits with
/// events past its copy's place (the copy takes the waiting ones up to its place only). The edit,
/// the LED's load from 10 to 2.2 kohm at 2.1 ms, is sent once the run is at 3 ms (the held sink:
/// once it refused); snapshots are kept within 0.5 ms of the front, at every seventh loop top; the
/// events are compared to 5 ms, with both runs stepped on past it so that a batch refused last is
/// taken.
#[test]
fn an_edit_behind_the_front_hands_the_mcus_events_to_its_copy() {
    const EDIT: f64 = 2.1e-3;
    const SENT: f64 = 3e-3;
    for (name, res) in [("Exact", 0.0), ("Fast", 1.0 / 96e3)] {
        for (refuse, hold) in [(0, false), (3, false), (0, true)] {
            let run = |behind: bool| {
                let (sink, taken) = events(refuse);
                let mut sim = live(res, Some(sink), if behind { 0.5e-3 } else { 0.0 }, Snapshots::default().bytes, false);
                assert!(sim.apply(LiveCmd::Play), "the run plays");
                let edit = vec![(EDIT, LiveCmd::SetParam { generation: 1, device: "load".into(), key: "value".into(), value: 2.2e3 })];
                let mut later = behind.then_some(edit.clone());
                if !behind {
                    assert!(sim.apply(LiveCmd::Edit(edit)), "the edit is taken");
                }
                let mut points = Points::default();
                let mut top = 0;
                while sim.t() < END {
                    if sim.t() >= SENT && later.is_some() {
                        let mut t = taken.lock().expect("the events");
                        t.hold = hold && t.held == 0;
                        if !t.hold {
                            drop(t);
                            assert!(sim.apply(LiveCmd::Edit(later.take().expect("the edit"))), "the edit is taken");
                        }
                    }
                    sim.advance(END, Some(Duration::ZERO), &mut points).unwrap_or_else(|d| panic!("{name}: the rig steps: {d}"));
                    if top % 7 == 0 {
                        sim.keep_snapshot(&points);
                    }
                    top += 1;
                }
                // a batch the sink refused last waits with the MCU: on a little, it goes
                while sim.t() < END + 1.5e-3 {
                    sim.advance(END + 1.5e-3, None, &mut points).unwrap_or_else(|d| panic!("{name}: the rig steps: {d}"));
                }
                let mut taken = std::mem::take(&mut *taken.lock().expect("the events"));
                taken.got.retain(|e| e.0 <= END);
                (points, taken, sim.stats().clone())
            };
            let (want, want_events, _) = run(false);
            let (got, got_events, st) = run(true);
            let name = format!("{name}, refusing {refuse}{}", if hold { ", held" } else { "" });
            assert_eq!((st.rewinds, st.late_edits), (1, 0), "{name}: the edit sends the run back to a snapshot before it");
            assert!(got_events.voided > 0 && (refuse == 0 || got_events.refused > 0) && (!hold || got_events.held > 0), "{name}: the sink voided events ({}) and refused batches ({}, {} held)", got_events.voided, got_events.refused, got_events.held);
            let n = want_events.got.len().min(got_events.got.len());
            let first = (0..n).find(|&i| want_events.got[i].0.to_bits() != got_events.got[i].0.to_bits() || want_events.got[i].1 != got_events.got[i].1);
            assert!(first.is_none() && got_events.got.len() == want_events.got.len(), "{name}: the events are the straight run's ({} against {}), first differing {:?}", got_events.got.len(), want_events.got.len(), first.map(|i| (&want_events.got[i], &got_events.got[i])));
            let same = got.at.len() == want.at.len() && got.at.iter().zip(&want.at).all(|(a, b)| a.to_bits() == b.to_bits()) && got.x.iter().zip(&want.x).all(|(a, b)| a.iter().zip(b).all(|(p, q)| p.to_bits() == q.to_bits()));
            assert!(same, "{name}: the points are the straight run's ({} against {})", got.at.len(), want.at.len());
            eprintln!("{name}: {} events ({} voided, {} batches refused, {} held), {} points, {} snapshots ({} kept)", got_events.got.len(), got_events.voided, got_events.refused, got_events.held, got.at.len(), st.snapshots, st.snapshots_kept);
        }
    }
}

/// With the counting allocator installed (as in this test binary) a snapshot's size is measured:
/// what the worker's thread allocates taking it, the rig's MCU included. In Exact and in Fast the
/// ring's measure of each is within a quarter of a snapshot measured by hand at 1 ms, and with a
/// budget of three and a half of them the kept snapshots stay within it at every loop top of a run
/// keeping one at each third, two to four of them once it is full.
#[test]
fn measured_snapshots_keep_to_their_budget() {
    for (name, res) in [("Exact", 0.0), ("Fast", 1.0 / 96e3)] {
        let mut probe = live(res, None, 0.0, 0, false);
        while probe.t() < 1e-3 {
            probe.advance(1e-3, None, &mut Points::default()).unwrap_or_else(|d| panic!("{name}: the rig steps: {d}"));
        }
        let (snap, one) = measure(|| probe.snapshot().expect("the MCU forks"));
        let one = one.expect("the counting allocator is installed");
        drop((snap, probe));
        let budget = one * 7 / 2;
        let mut sim = live(res, None, 1.0, budget, false);
        assert!(sim.apply(LiveCmd::Play), "the run plays");
        let mut points = Points::default();
        let mut top = 0;
        while sim.t() < END {
            sim.advance(END, Some(Duration::ZERO), &mut points).unwrap_or_else(|d| panic!("{name}: the rig steps: {d}"));
            if top % 3 == 0 {
                sim.keep_snapshot(&points);
                let st = sim.stats();
                let near = st.snapshot_last >= one * 3 / 4 && st.snapshot_last <= one * 5 / 4;
                assert!(near && st.snapshot_bytes <= budget && st.snapshots_kept >= 1, "{name}: at {:e} s the last snapshot holds {} bytes (one by hand: {one}), the kept {} hold {} of {budget}", sim.t(), st.snapshot_last, st.snapshots_kept, st.snapshot_bytes);
            }
            top += 1;
        }
        let st = sim.stats();
        assert!(st.snapshots > 6 && (2..=4).contains(&st.snapshots_kept), "{name}: {} taken, {} kept", st.snapshots, st.snapshots_kept);
        eprintln!("{name}: a snapshot holds {one} bytes; {} taken, {} kept in {} bytes", st.snapshots, st.snapshots_kept, st.snapshot_bytes);
    }
}

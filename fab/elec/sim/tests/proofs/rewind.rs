//! An edit behind a live run's front (`LiveCmd::Edit`, `LiveSpec::snapshots`): the run goes back
//! to its newest snapshot at or before the edit and steps on with it, and what it publishes (the
//! trace through `TraceDelta::rewind`, the sinks through `Sink::rewind`) is the run that had the
//! edit all along.

use crate::snapshot::{rig, Pwm, Recut, Thirds, LEAD, PARAMS, SAMPLE};
use fab_elec_sim::cosim::{Bus, CoRun, CoSim, CoSimDevice, CoSimPin, CoState, StepTo, Watch};
use fab_elec_sim::listen::{AudioTap, ListenBus, TapMix, TapSpec, TapVoice};
use fab_elec_sim::live::{Latest, LiveCmd, LiveValues, ValuePlan};
use fab_elec_sim::livesim::{run_live, LiveSim, LiveSpec, LiveStats, Replan, SinkCmd, SinkSet, Snapshots, TraceOut, UNMEASURED};
use fab_elec_sim::recording::{RunContent, RunSlot, Tape, CHUNK};
use fab_elec_sim::rounds::{Helpers, Post};
use fab_elec_sim::spill::LogicSpill;
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_sim::trace::{Keep, LogicEdge, SignalId, TraceDelta, TracePublisher, TraceStore, LOGIC_CHUNK};
use fab_elec_sim::{PinMode, PinPull, SimCircuit, SimDiag, Sink, SinkState, Source, Wave};
use std::sync::mpsc::{channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A trace cut by a rewind (`TraceStore::rewind_producer`) is the trace of what came up to the
/// cut, and a consumer that takes the rewind in a delta (`TraceStore::apply`) ends as the
/// producer, then continues with it. On a store with two closed chunks of points and an open tail,
/// marks, a logic trace with sealed chunks, and a logic trace of 1.6 M entries whose chunk at the
/// cut is spilled to the producer's file:
/// - the points, marks and logic entries after the cut go; the chunk the cut falls in is open
///   again with its kept part (an exact copy's too);
/// - the spilled chunk at the cut comes back from the file, kept part open, and the rewind names
///   it as resent; the file forgets it, and keeps the chunks before it;
/// - the consumer, given the rewind and the resent entries, holds the producer's points, marks and
///   entries, and the next delta continues it.
#[test]
fn a_rewound_trace_is_the_trace_up_to_the_cut() {
    const N: usize = 12_000;
    const DT: f64 = 1e-6;
    const CUT: f64 = 5000.5e-6;
    const B_FROM: f64 = 4e-3;
    const B_DT: f64 = 5e-9;
    const B_N: usize = 1_600_000;
    let (a, b) = (SignalId(2), SignalId(3));
    let point = |i: usize| (i as f64 * DT, [(i as f64 * 0.01).sin(), (i as f64 * 0.003).cos()]);
    let b_edge = |k: usize| LogicEdge { t: B_FROM + k as f64 * B_DT, high: k % 2 == 0 };
    // the producer, and the delta that brings a consumer all of it
    let mut p = TraceStore::new(1, &[false, true]);
    let mut want = TraceStore::new(1, &[false, true]);
    let mut spill = LogicSpill::new();
    let (mut time, mut values, mut closed, mut marks) = (Vec::new(), [Vec::new(), Vec::new()], Vec::new(), Vec::new());
    let (mut a_found, mut b_found, mut logic_closed, mut pages) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in 0..N {
        let (t, y) = point(i);
        p.push(t, &y, &mut closed);
        time.push(t);
        values[0].push(y[0] as f32);
        values[1].push(y[1] as f32);
        let edge = LogicEdge { t, high: i % 2 == 0 };
        p.push_logic(a, edge, &mut logic_closed);
        a_found.push(edge);
        if i % 97 == 0 {
            p.marks.push(t);
            marks.push(t);
        }
        if t <= CUT {
            want.push(t, &y, &mut Vec::new());
            want.push_logic(a, edge, &mut Vec::new());
            if i % 97 == 0 {
                want.marks.push(t);
            }
        }
    }
    for k in 0..B_N {
        let edge = b_edge(k);
        p.push_logic(b, edge, &mut logic_closed);
        b_found.push(edge);
        if edge.t <= CUT {
            want.push_logic(b, edge, &mut Vec::new());
        }
    }
    let spilled = p.spill_logic(&mut spill, &[], &mut pages);
    let cut_chunk = ((CUT - B_FROM) / B_DT) as usize / LOGIC_CHUNK;
    let cut_first = b_edge(cut_chunk * LOGIC_CHUNK).t;
    assert!(spilled > (cut_chunk + 1) * LOGIC_CHUNK && spill.holds(b, cut_first), "the chunk at the cut is spilled ({spilled} entries)");
    let mut c = TraceStore::default();
    c.apply(&TraceDelta {
        generation: 1,
        from: 0,
        time: time.into(),
        values: values.into_iter().map(Arc::from).collect(),
        closed,
        marks,
        logic: vec![(a, a_found.into()), (b, b_found.into())],
        logic_closed,
        logic_pages: pages,
        folds: Vec::new(),
        rewind: None,
    });
    assert_eq!(c.base + c.len(), N, "the consumer holds every point before the rewind");

    let rewind = p.rewind_producer(CUT, &mut spill);
    assert_eq!(rewind.resent, [(b, cut_first)], "the logic trace whose cut fell in a spilled chunk is resent from that chunk");
    assert!(!spill.holds(b, cut_first) && spill.holds(b, b_edge(0).t), "the file forgets the chunk opened again and keeps those before it");
    let resent = p.logic(b).expect("the spilled trace").open.clone();
    c.apply(&TraceDelta {
        generation: 1,
        from: p.base + p.len(),
        time: Arc::from(vec![]),
        values: vec![Arc::from(vec![]), Arc::from(vec![])],
        closed: Vec::new(),
        marks: Vec::new(),
        logic: vec![(b, resent.into())],
        logic_closed: Vec::new(),
        logic_pages: Vec::new(),
        folds: Vec::new(),
        rewind: Some(rewind),
    });

    let kept = (CUT / DT) as usize + 1;
    for (name, s) in [("the producer", &p), ("the consumer", &c)] {
        assert_eq!((s.base, s.len(), s.rewinds), (0, kept, 1), "{name}: the points up to the cut stay, once rewound");
        assert!((0..kept).all(|i| s.time.at(i).to_bits() == want.time.at(i).to_bits()), "{name}: each kept point's time");
        assert!((0..2).all(|k| (0..kept).all(|i| s.signals[k].at(i).to_bits() == want.signals[k].at(i).to_bits())), "{name}: each kept point's values");
        assert_eq!(s.marks, want.marks, "{name}: the marks up to the cut");
        let (sa, wa) = (s.logic(a).expect("trace a"), want.logic(a).expect("trace a"));
        assert!(sa.len() == wa.len() && (0..wa.len()).all(|i| sa.get(i) == wa.get(i)), "{name}: the logic entries up to the cut");
        let (sb, wb) = (s.logic(b).expect("trace b"), want.logic(b).expect("trace b"));
        assert_eq!((sb.len(), sb.chunks.len(), sb.spilled), (wb.len(), cut_chunk, cut_chunk), "{name}: the spilled trace keeps its chunks before the cut, spilled");
        assert_eq!(sb.open, wb.open, "{name}: the spilled chunk's entries up to the cut are its open tail");
        assert!((0..wb.len()).all(|i| sb.get(i).is_none_or(|e| Some(e) == wb.get(i))), "{name}: every entry it holds is the run's");
    }
    let exact = p.signals[1].open_exact.as_ref().expect("signal 1 keeps an exact copy");
    assert!(exact.len() == kept - 4096 && exact.iter().enumerate().all(|(k, &e)| e.to_bits() == point(4096 + k).1[1].to_bits()), "the opened chunk's exact copy is kept");

    // the run goes on from the cut with other values: the consumer follows
    let mut closed = Vec::new();
    let mut time = Vec::new();
    let mut values = [Vec::new(), Vec::new()];
    for i in kept..kept + 5000 {
        let t = i as f64 * DT;
        p.push(t, &[1.0, 2.0], &mut closed);
        time.push(t);
        values[0].push(1.0f32);
        values[1].push(2.0f32);
    }
    c.apply(&TraceDelta { generation: 1, from: kept, time: time.into(), values: values.into_iter().map(Arc::from).collect(), closed, marks: Vec::new(), logic: Vec::new(), logic_closed: Vec::new(), logic_pages: Vec::new(), folds: Vec::new(), rewind: None });
    assert_eq!((c.base + c.len(), c.signals[1].at(kept + 4999)), (kept + 5000, 2.0), "the consumer continues from the cut");
}

const END: f64 = 4e-3;

/// A sink that keeps every point it is given (shared, for the test to read); its state at a
/// snapshot is how many it held, and a rewind goes back to that.
#[derive(Clone, Default)]
struct Points(Arc<Mutex<Kept>>);

#[derive(Default)]
struct Kept {
    at: Vec<f64>,
    x: Vec<Vec<f64>>,
    rewinds: Vec<f64>,
}

impl Sink for Points {
    fn point(&mut self, at: f64, _outer: Option<f64>, x: &[f64]) {
        let mut k = self.0.lock().expect("the points");
        k.at.push(at);
        k.x.push(x.to_vec());
    }
    fn save(&self) -> Option<Box<dyn SinkState>> {
        Some(Box::new(self.0.lock().expect("the points").at.len()))
    }
    fn rewind(&mut self, at: f64, saved: Option<&dyn SinkState>, _unknowns: &[UnknownInfo]) {
        let n = *saved.and_then(|s| s.downcast_ref::<usize>()).expect("the state this sink saved");
        let mut k = self.0.lock().expect("the points");
        assert!(k.at[..n].iter().all(|&t| t <= at) && k.at[n..].iter().all(|&t| t > at), "a rewind to {at:e} s goes back to the points up to it");
        k.at.truncate(n);
        k.x.truncate(n);
        k.rewinds.push(at);
    }
}

/// The rig traced (its nets, the pads as logic), with its firmware and planner, keeping snapshots
/// within `span` simulated seconds of its front, one at every return of `advance`, in `bytes`.
fn bench(res: f64, span: f64, bytes: u64) -> LiveSim {
    let replan = Replan { edit: 1e-3, pins: 0.5e-3, lead: LEAD, slow: if res == 0.0 { -1.0 } else { f64::INFINITY } };
    let snapshots = Snapshots { span, every: Duration::ZERO, share: f64::INFINITY, bytes };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: SAMPLE, res, round: 16, record: 1.0, replan, snapshots, ..LiveSpec::default() };
    let plan = ValuePlan { nets: ["0", "vdd", "f", "v", "n", "pa", "pb", "pc"].map(String::from).to_vec(), ..ValuePlan::default() };
    let mut sim = LiveSim::new(rig(), plan, 1, spec).expect("the rig's operating point converges");
    let pin = |name: &str, gpio: &str, pad: &str| CoSimPin { name: name.into(), gpio: gpio.into(), pad: pad.into(), vss: "0".into(), vdd: "vdd".into(), vih: PARAMS.vih, vil: PARAMS.vil };
    sim.attach(Some(CoSimDevice { dev: Box::new(Pwm::default()), pins: vec![pin("PA0", "pa0", "pa"), pin("PB0", "pb0", "pb"), pin("PC0", "pc0", "pc")] }));
    if res == 0.0 {
        let inline: Post = Arc::new(|job| job());
        sim.set_helpers(Some(Helpers { post: inline, count: 1 }));
    }
    sim.set_planner(Some(Box::new(Thirds { calls: 0, recut: Recut::Same }))).expect("the first plan keeps one stepper");
    sim
}

/// The edits: the voice's bias pressed down to 1.5 kohm and back, and later the hiss's load.
fn edits() -> [Vec<(f64, LiveCmd)>; 2] {
    let set = |device: &str, value: f64| LiveCmd::SetParam { generation: 1, device: device.into(), key: "value".into(), value };
    [vec![(1.2e-3, set("rb", 1.5e3)), (1.5e-3, set("rb", 2.2e3))], vec![(2.4e-3, set("rn", 4.7e3))]]
}

/// The rate the tap records at (its run's): 4 ms make 12 whole chunks.
const TAKE_RATE: f64 = 1_536_000.0;

/// What the tap made: its take's tape, and its run's clean stretch.
struct Heard {
    tape: Arc<Tape>,
    clean: f64,
}

/// Runs the bench through `run_live` and its channels to past `END`: the edits sent before Play
/// (`ahead`), or each once the run has passed it by 0.4 ms. A tap records the voice and the hiss
/// (DC blocked) at [`TAKE_RATE`], nobody listening live. Returns the published trace, the sink's
/// points, the worker's statistics and what the tap made.
fn through_run_live(res: f64, span: f64, ahead: bool) -> (Arc<TraceStore>, Points, LiveStats, Heard) {
    let sim = bench(res, span, Snapshots::default().bytes);
    let (tx, rx) = sync_channel(64);
    let values: Latest<LiveValues> = Latest::new();
    let out = Arc::new(Latest::new());
    let mut publisher = TracePublisher::new(out.clone());
    let (sink_tx, mut sinks) = SinkSet::new();
    let points = Points::default();
    assert!(sink_tx.try_send(SinkCmd::Add(1, Box::new(points.clone()))).is_ok(), "the sink goes in");
    let content = Arc::new(RunContent::new(1, None, TAKE_RATE));
    let bus = ListenBus::new(1 << 12, Arc::new(RunSlot::default()));
    bus.runs.set(Some(content.clone()));
    bus.ring.set_device_rate(48_000);
    let voice = |net: &str| TapVoice { net: net.into(), reference: None, gain: 1.0 };
    let spec = TapSpec { voices: vec![voice("v"), voice("n")], latency: 0.01, dc_block_hz: 20.0, mix: TapMix::new(2) };
    let tap = AudioTap::new(spec, bus.ring.clone()).recording(bus.record(1, 2, 0.25));
    assert!(sink_tx.try_send(SinkCmd::Add(2, Box::new(tap))).is_ok(), "the tap goes in");
    let (woke_tx, woke) = channel();
    let wake = move || {
        let _ = woke_tx.send(());
    };
    let (values, publisher_out, sinks_in, wake) = (&values, &mut publisher, &mut sinks, &wake);
    let at = |t: f64| move || values.latest().is_some_and(|(_, v)| v.t >= t);
    let mut sim = std::thread::scope(|s| {
        let worker = s.spawn(move || run_live(sim, &rx, values, publisher_out, sinks_in, wake));
        // each wait ends on the worker's own progress (its publications wake it), never on a wall
        // clock: the run is unpaced, so a busy box only makes it later
        let wait = |done: &dyn Fn() -> bool| {
            while !done() {
                assert!(!worker.is_finished(), "the worker ended before the proof was done with it");
                let _ = woke.recv_timeout(Duration::from_millis(20));
            }
        };
        if ahead {
            for e in edits() {
                tx.send(LiveCmd::Edit(e)).expect("the worker takes commands");
            }
        }
        tx.send(LiveCmd::Play).expect("the worker takes commands");
        if !ahead {
            for (k, e) in edits().into_iter().enumerate() {
                wait(&at(e[0].0 + 0.4e-3));
                tx.send(LiveCmd::Edit(e)).expect("the worker takes commands");
                // the worker takes commands at the head of its loop and publishes once a pass: the
                // second publication from now comes after it took the edit, and its sinks went back
                let seen = values.published();
                wait(&|| values.published() >= seen + 2);
                assert_eq!(points.0.lock().expect("the points").rewinds.len(), k + 1, "edit {k} sends the run back (the run's start is kept, so it is never late)");
            }
        }
        // the tap tapes behind the run's front (the run's read and its rate change each reach
        // ahead), so the run goes on until the take's tape is promised its whole chunks to `END`
        let taped = ((END * TAKE_RATE) as usize / CHUNK * CHUNK) as u64;
        wait(&|| at(END + 0.2e-3)() && bus.tapes.current(2).is_some_and(|tape| tape.promised() >= taped));
        tx.send(LiveCmd::Stop).expect("the worker takes commands");
        worker.join().expect("the worker ends").unwrap_or_else(|d| panic!("the rig steps: {d}"))
    });
    while let Some(d) = sim.take_delta() {
        assert!(publisher.offer(d, &Keep::last(1.0)).is_ok(), "the publisher takes every delta");
    }
    let (_, store) = out.latest().expect("a published trace");
    fab_elec_sim::recording::settle_tapes();
    let heard = Heard { tape: bus.tapes.current(2).expect("the take is taped"), clean: content.clean_until() };
    (store, points, sim.stats().clone(), heard)
}

/// Where two traces part up to `END`: the points' times and values by bits, the marks and the
/// logic entries.
fn trace_parted(got: &TraceStore, want: &TraceStore) -> Option<String> {
    let upto = |s: &TraceStore| (0..s.len()).take_while(|&i| s.time.at(i) <= END).count();
    let n = upto(want);
    if upto(got) != n || got.signals.len() != want.signals.len() {
        return Some(format!("{} points to the end against {n}", upto(got)));
    }
    for i in 0..n {
        if got.time.at(i).to_bits() != want.time.at(i).to_bits() {
            return Some(format!("point {i} at {:e} s against {:e} s", got.time.at(i), want.time.at(i)));
        }
        if let Some(k) = (0..want.signals.len()).find(|&k| got.signals[k].at(i).to_bits() != want.signals[k].at(i).to_bits()) {
            return Some(format!("point {i} at {:e} s, signal {k}: {} against {}", want.time.at(i), got.signals[k].at(i), want.signals[k].at(i)));
        }
    }
    let marks = |s: &TraceStore| s.marks.iter().copied().filter(|&m| m <= END).collect::<Vec<_>>();
    if marks(got) != marks(want) {
        return Some("the marks".into());
    }
    for w in &want.logic {
        let entries = |s: &TraceStore| s.logic(w.id).map(|l| (0..l.len()).filter_map(|i| l.get(i)).filter(|e| e.t <= END).collect::<Vec<_>>()).unwrap_or_default();
        if entries(got) != entries(want) {
            return Some(format!("logic signal {}", w.id.0));
        }
    }
    None
}

/// An edit sent once the run has passed its time sends the run back, and what the run publishes
/// is the run that had the edit on its timeline from the start. Through `run_live` and its
/// channels, on the snapshot rig (a diode, a noise source, a PWM pad, a watch whose handler writes
/// a pad, a run that a plan moves into three blocks and plans again after the edits), traced (its
/// nets, its pads as logic), with a sink that saves its state at each snapshot; in Exact (plans
/// posted) and Fast (plans on the worker). Two edits, a press (down and up) and a later value,
/// each sent 0.4 ms after its time:
/// - the run goes back twice, to a snapshot at or before each edit, and no edit comes late;
/// - the published trace up to 4 ms is the straight run's by bits (every point's time and
///   values, the marks, the logic entries), the straight run keeping no snapshots: so keeping
///   them, with the commits they take of the firmware, is passive too;
/// - the sink's points up to 4 ms are the straight run's by bits, having gone back to the state it
///   saved at each snapshot;
/// - a recording audio tap goes on with its take: one take, whose tape to 4 ms is the straight
///   run's by bits, and its run's clean stretch ends at the first return.
#[test]
fn an_edit_behind_the_front_publishes_the_run_that_had_it_all_along() {
    for (name, res) in [("Exact", 0.0), ("Fast", SAMPLE)] {
        let (want, want_points, _, want_heard) = through_run_live(res, 0.0, true);
        let (got, got_points, st, got_heard) = through_run_live(res, 1.0, false);
        assert_eq!((st.rewinds, st.late_edits), (2, 0), "{name}: each edit sends the run back to a snapshot before it");
        assert!(st.snapshots > 2, "{name}: snapshots were kept ({})", st.snapshots);
        if let Some(d) = trace_parted(&got, &want) {
            panic!("{name}: the trace after the edits is the straight run's: {d}");
        }
        let (got_points, want_points) = (got_points.0.lock().expect("the points"), want_points.0.lock().expect("the points"));
        let n = want_points.at.iter().take_while(|&&t| t <= END).count();
        let same = (0..n).all(|i| got_points.at.get(i).is_some_and(|t| t.to_bits() == want_points.at[i].to_bits()) && got_points.x[i].iter().zip(&want_points.x[i]).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(same && got_points.at.get(n).is_none_or(|&t| t > END), "{name}: the sink's points are the straight run's");
        assert_eq!(got_heard.tape.take, want_heard.tape.take, "{name}: the take goes on through the returns");
        let frames = (END * TAKE_RATE) as usize / CHUNK * CHUNK;
        let read = |tape: &Tape| {
            let mut frame = vec![0.0f32; 2 * frames];
            assert_eq!(tape.read(0, &mut frame).expect("the tape reads"), frames, "{name}: the take is taped to 4 ms");
            frame
        };
        let (taped, straight) = (read(&got_heard.tape), read(&want_heard.tape));
        let parted = taped.iter().zip(&straight).position(|(a, b)| a.to_bits() != b.to_bits());
        assert!(parted.is_none() && straight.iter().any(|&x| x != 0.0), "{name}: the take to 4 ms is the straight run's by bits: parts at frame {:?}", parted.map(|i| i / 2));
        assert!(want_heard.clean.is_infinite() && got_heard.clean == got_points.rewinds[0], "{name}: the run's clean stretch ends at its first return ({:e} s): {:e} s", got_points.rewinds[0], got_heard.clean);
        eprintln!("{name}: {n} points, {} snapshots ({} kept), {} rewinds to {:?}, the last {:?}", st.snapshots, st.snapshots_kept, st.rewinds, got_points.rewinds, st.rewind_wall);
    }
}

/// Firmware that reads its pad once a millisecond and keeps what it read.
#[derive(Clone)]
struct Poll {
    next: f64,
    seen: Arc<Mutex<Vec<(f64, bool)>>>,
}

impl CoSim for Poll {
    fn next(&self) -> f64 {
        self.next
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        self.seen.lock().expect("the readings").push((bus.t(), bus.v(0) > 1.65));
        self.next += 1e-3;
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &[]
    }
    fn watch_gen(&self) -> u32 {
        0
    }
    fn arm(&mut self, _t: f64, _to: Option<StepTo>) -> bool {
        false
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
}

struct Nothing;
impl Sink for Nothing {}

/// Quick presses stamped by a host whose view lags an unpaced run: the release is stamped 50 ms
/// after the press, and the run is past the press when it comes. Once 200 ms ahead with both sent
/// at once (both are in its past), once 30 ms ahead (the release is not), and once 10 ms ahead
/// with the release sent 20 ms of the run later (the run is further past the press than it came
/// late): each press takes effect at the next step boundary and its release keeps its 50 ms from
/// there, so the firmware (reading its pad each millisecond) sees the key down for 50 ms, then up,
/// every time; a later command of the same key, on time, is not moved.
#[test]
fn a_late_quick_press_keeps_its_length() {
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(3.3));
    c.v("key", "k", "0", Source::dc(0.0));
    c.r("rk", "k", "pa", 1e3);
    c.r("rd", "pa", "0", 100e3);
    c.gpio("pa0", "pa", "vdd", "0", PARAMS, PinMode::Input(PinPull::None));
    let spec = LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the key's operating point converges");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let pin = CoSimPin { name: "PA0".into(), gpio: "pa0".into(), pad: "pa".into(), vss: "0".into(), vdd: "vdd".into(), vih: PARAMS.vih, vil: PARAMS.vil };
    sim.attach(Some(CoSimDevice { dev: Box::new(Poll { next: 0.0, seen: seen.clone() }), pins: vec![pin] }));
    let key = |dc: f64| Box::new(LiveCmd::SetParam { generation: 1, device: "key".into(), key: "dc".into(), value: dc });
    let advance = |sim: &mut LiveSim, to: f64| {
        while sim.t() < to {
            sim.advance(to, None, &mut Nothing).unwrap_or_else(|d| panic!("the key steps: {d}"));
        }
    };
    let mut took = Vec::new();
    for (run, stamp, lift) in [(0.3, 0.1, 0.0), (0.5, 0.47, 0.0), (0.6, 0.59, 0.02)] {
        advance(&mut sim, run);
        took.push(sim.t());
        sim.apply(LiveCmd::At(stamp, key(3.3)));
        advance(&mut sim, run + lift);
        sim.apply(LiveCmd::At(stamp + 0.05, key(0.0)));
    }
    advance(&mut sim, 0.7);
    sim.apply(LiveCmd::At(0.75, key(1.0)));
    advance(&mut sim, 0.8);
    let seen = seen.lock().expect("the readings");
    // the runs of readings with the key down: (first, last, count)
    let mut down: Vec<(f64, f64, usize)> = Vec::new();
    for (k, r) in seen.iter().enumerate() {
        match (r.1, k > 0 && seen[k - 1].1) {
            (true, false) => down.push((r.0, r.0, 1)),
            (true, true) => {
                let d = down.last_mut().expect("a run started");
                d.1 = r.0;
                d.2 += 1;
            }
            _ => {}
        }
    }
    assert_eq!(down.len(), 3, "the firmware sees each press, then its release: {down:?}");
    for ((first, last, n), now) in down.iter().zip(&took) {
        assert!((49..=51).contains(n) && *first > *now && *first <= now + 1e-3 && (last - first - 0.049).abs() <= 1e-3, "the key reads down for 50 ms from the press's effect at {now:e} s: {n} readings, {first:e} s to {last:e} s");
    }
    let times: Vec<f64> = sim.timeline().iter().map(|e| e.0).collect();
    let held = |k: usize| (times[k + 1] - times[k] - 0.05).abs() < 2e-4;
    assert!(times.len() == 7 && held(0) && held(2) && held(4) && (times[6] - 0.75).abs() < 2e-4, "each release keeps its 50 ms from its press; the later command stays at its time: {times:?}");
}

/// A late press's move is forgotten once the run is past it (`a_late_quick_press_keeps_its_length`),
/// and the run going back for an edit does not bring it back: the key, pressed 50 ms late, is let
/// go 200 ms after where the press took effect, where the listener hears the run, and the release
/// takes effect there, not 50 ms later.
#[test]
fn a_late_press_does_not_move_a_release_made_where_it_is_heard() {
    let mut c = SimCircuit::new();
    c.v("key", "k", "0", Source::dc(0.0));
    c.r("rk", "k", "0", 1e3);
    let hum = Source { wave: Wave::Sin { vo: 0.0, va: 1.0, freq: 1e3, td: 0.0, theta: 0.0, phase: 0.0 }, ..Source::dc(0.0) };
    c.v("hum", "h", "0", hum);
    c.r("rh", "h", "0", 1e3);
    let snapshots = Snapshots { span: 1.0, every: Duration::ZERO, share: f64::INFINITY, bytes: u64::MAX };
    let spec = LiveSpec { rate: f64::INFINITY, snapshots, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the operating point converges");
    sim.keep_snapshot(&Nothing);
    assert_eq!(sim.stats().snapshots_kept, 1, "a snapshot at the start to go back to");
    let key = |dc: f64| LiveCmd::SetParam { generation: 1, device: "key".into(), key: "dc".into(), value: dc };
    let advance = |sim: &mut LiveSim, to: f64| {
        while sim.t() < to {
            sim.advance(to, None, &mut Nothing).unwrap_or_else(|d| panic!("the run steps: {d}"));
        }
    };
    advance(&mut sim, 0.1);
    let pressed = sim.t();
    sim.apply(LiveCmd::At(pressed - 0.05, Box::new(key(3.3))));
    advance(&mut sim, 0.4);
    let released = pressed + 0.2;
    assert!(sim.apply(LiveCmd::Edit(vec![(released, key(0.0))])), "the release is taken");
    assert_eq!(sim.stats().rewinds, 1, "the run goes back for the release");
    advance(&mut sim, 0.4);
    let times: Vec<f64> = sim.timeline().iter().map(|e| e.0).collect();
    assert!(times.len() == 2 && (times[0] - pressed).abs() < 2e-4 && (times[1] - released).abs() < 2e-4,
        "the press at the run's time ({pressed:.4} s), the release where it was heard ({released:.4} s): {times:?}");
}

/// Over their byte budget the snapshots thin into the dense span, the newest kept, and nothing is
/// refused. With no counting allocator installed (as in this test binary) each counts as
/// `UNMEASURED`: a budget of three holds three at most at every loop top of a run that keeps one
/// at each, in Exact and Fast; an edit 1 ms behind the front still goes back to one of them.
#[test]
fn snapshots_over_their_budget_thin_and_an_edit_still_goes_back() {
    for (name, res) in [("Exact", 0.0), ("Fast", SAMPLE)] {
        let mut sim = bench(res, 1.0, 3 * UNMEASURED);
        assert!(sim.apply(LiveCmd::Play), "the run plays");
        while sim.t() < END {
            sim.advance(END, Some(Duration::ZERO), &mut Nothing).unwrap_or_else(|d| panic!("{name}: the rig steps: {d}"));
            sim.keep_snapshot(&Nothing);
            let st = sim.stats();
            assert!((1..=3).contains(&st.snapshots_kept) && st.snapshot_bytes == st.snapshots_kept as u64 * UNMEASURED && st.snapshot_last == UNMEASURED,
                "{name}: within the budget at {:e} s: {} kept, {} bytes", sim.t(), st.snapshots_kept, st.snapshot_bytes);
        }
        assert!(sim.stats().snapshots > 6, "{name}: more snapshots were taken than the budget holds ({})", sim.stats().snapshots);
        let set = LiveCmd::SetParam { generation: 1, device: "rb".into(), key: "value".into(), value: 1.5e3 };
        assert!(sim.apply(LiveCmd::Edit(vec![(sim.t() - 1e-3, set)])), "{name}: the edit is taken");
        let st = sim.stats();
        assert_eq!((st.rewinds, st.late_edits), (1, 0), "{name}: an edit 1 ms behind goes back to a kept snapshot");
    }
}

/// The times of the run's `Sink::ended` calls.
struct Ends(Arc<Mutex<Vec<f64>>>);
impl Sink for Ends {
    fn ended(&mut self, at: f64) {
        self.0.lock().unwrap().push(at);
    }
}

/// A take of finite length (`LiveCmd::SetUntil`): the playing run steps exactly to its end and
/// rests there, saying so to its sinks; an edit behind the end and a `Restart` each send it back
/// (the restart to its start) and it renders to the end and rests again; a later end plays on.
#[test]
fn a_take_renders_to_its_end_and_rests_until_an_edit_sends_it_back() {
    const TAKE: f64 = 2.5e-3;
    let mut sim = bench(SAMPLE, 1.0, 1 << 30);
    assert!(sim.apply(LiveCmd::SetUntil(TAKE)) && sim.apply(LiveCmd::Play), "the take's end and Play are taken");
    let render = |sim: &mut LiveSim, what: &str| {
        let mut steps = 0;
        while sim.running() {
            sim.keep_snapshot(&Nothing);
            sim.advance(f64::INFINITY, Some(Duration::ZERO), &mut Nothing).unwrap_or_else(|d| panic!("{what}: the rig steps: {d}"));
            steps += 1;
            assert!(steps < 100_000, "{what}: the run rests at the take's end");
        }
        assert!((sim.t() - TAKE).abs() < 1e-12, "{what}: the run stands at the take's end ({:e} s)", sim.t());
    };
    render(&mut sim, "the first render");
    let set = LiveCmd::SetParam { generation: 1, device: "rb".into(), key: "value".into(), value: 1.5e3 };
    assert!(sim.apply(LiveCmd::Edit(vec![(TAKE - 1e-3, set)])), "an edit behind the end is taken");
    assert!(sim.running() && sim.t() <= TAKE - 1e-3, "the edit sends the rested run back before its time ({:e} s) and it plays", sim.t());
    render(&mut sim, "the render after the edit");
    assert!(sim.apply(LiveCmd::Restart), "a restart is taken");
    assert!(sim.running() && sim.t() < 1e-4, "the restart sends the run back to its start ({:e} s)", sim.t());
    render(&mut sim, "the render after the restart");
    let st = sim.stats();
    assert_eq!((st.rewinds, st.late_edits), (2, 0), "the edit and the restart each went back to a kept snapshot");
    assert!(sim.apply(LiveCmd::SetUntil(2.0 * TAKE)), "a later end is taken");
    assert!(sim.running(), "a later end sets the rested run playing");
    sim.advance(f64::INFINITY, Some(Duration::from_millis(5)), &mut Nothing).expect("the rig steps on");
    assert!(sim.t() > TAKE, "the run goes on past the old end");

    // through the worker's loop: the sinks hear of the end once, at it, and before the run
    // publishes its rest: a host that reads the rest reads a take its sinks have closed (a
    // recorder pads its last chunk at the end)
    let mut sim = bench(SAMPLE, 1.0, 1 << 30);
    assert!(sim.apply(LiveCmd::SetUntil(TAKE)), "the take's end is taken");
    let ends = Arc::new(Mutex::new(Vec::new()));
    let (sink_tx, mut sinks) = SinkSet::new();
    sink_tx.try_send(SinkCmd::Add(1, Box::new(Ends(ends.clone())))).expect("the sink is queued");
    let latest = Arc::new(Latest::<LiveValues>::new());
    let (commands, command_rx) = sync_channel(4);
    let (mut traces, _trace_rx) = sync_channel(1 << 12);
    let published = Arc::clone(&latest);
    let worker = std::thread::spawn(move || run_live(sim, &command_rx, &published, &mut traces, &mut sinks, &|| {}).map(|_| ()));
    commands.send(LiveCmd::Play).expect("Play reaches the worker");
    let start = std::time::Instant::now();
    let at_rest = || latest.latest().is_some_and(|(_, v)| (v.t - TAKE).abs() < 1e-12 && !v.running);
    while !at_rest() {
        assert!(start.elapsed() < Duration::from_secs(20), "the run publishes its rest at its take's end");
        std::thread::yield_now();
    }
    let heard = ends.lock().unwrap().clone();
    assert!(heard.len() == 1 && (heard[0] - TAKE).abs() < 1e-12, "the sinks heard of the end, at it, before the run published its rest: {heard:?}");
    commands.send(LiveCmd::Stop).expect("Stop reaches the worker");
    assert!(worker.join().expect("the worker joins").is_ok(), "the run ends without a diagnostic");
    let ends = ends.lock().unwrap().clone();
    assert!(ends.len() == 1 && (ends[0] - TAKE).abs() < 1e-12, "one end, at the take's end: {ends:?}");
}

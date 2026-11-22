//! A co-simulated process through the host: its pin table comes from the lowering, the pin the
//! bench leaves to it starts hi-Z, the pad follows what the process writes at its instants, and a
//! firmware step (`SimCmd::StepFw`) stops it bit-exact with its state in the published values.

use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::{Lowered, ModelLibs, SimCmd};
use fab_elec_fixtures::ledctl;
use fab_elec_sim::analysis::Sink;
use fab_elec_sim::cosim::{Bus, CoRun, CoSim, CoSimDevice, CoSimPin, CoState, StepTo, Watch};
use fab_elec_sim::livesim::LiveSpec;
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_core::ids::{BoardId, PinIx, PinRef};
use fab_elec_sim::{PinMode, PinPull, SimDiag};
use fab_elec_simhost::host::CoSimMaker;
use fab_elec_simhost::{SimHost, Spawn};
use fab_elec_view::live::sim::{SimAction, SimLive, SinkSlot};
use fab_elec_view::live::{pin_row, PinLiveMode, PIN_OFF};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The process's instants: it writes PA0 every `HALF` seconds from t = 0.
const HALF: f64 = 50e-6;

struct Threads;

impl Spawn for Threads {
    fn worker(&self, _name: &'static str, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
    fn job(&self, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
}

/// Drives its pin 0 low (LED 0 sinks into it and lights) on even instants, high on odd ones. Each
/// instant is a statement: a `Statement` step stops before the next one.
struct Toggle {
    k: u64,
    runs: Arc<AtomicUsize>,
    armed: Option<f64>,
    /// PA0's supply as the bus gave it at the first instant.
    supply: Arc<Mutex<Option<f64>>>,
}

/// What a `Toggle` shows of itself at a publication.
struct ToggleState {
    t: f64,
    k: u64,
}

impl CoSim for Toggle {
    fn next(&self) -> f64 {
        self.k as f64 * HALF
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        if self.armed.is_some_and(|a| self.next() > a) {
            self.armed = None;
            return Ok(CoRun::Stopped);
        }
        self.supply.lock().expect("supply lock").get_or_insert(bus.supply(0));
        assert!(bus.write(0, if self.k % 2 == 0 { PinMode::OutLow } else { PinMode::OutHigh }), "the process writes PA0");
        self.k += 1;
        self.runs.fetch_add(1, Ordering::Relaxed);
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &[]
    }
    fn watch_gen(&self) -> u32 {
        0
    }
    fn arm(&mut self, t: f64, to: Option<StepTo>) -> bool {
        match to {
            None => self.armed = None,
            Some(StepTo::Statement) => self.armed = Some(t),
            Some(_) => return false,
        }
        true
    }
    fn state(&mut self, t: f64) -> Option<CoState> {
        Some(CoState(Arc::new(ToggleState { t, k: self.k })))
    }
}

/// PA0's pad voltage at each point the scope gets.
struct Pad {
    node: String,
    index: Option<usize>,
    points: Arc<Mutex<Vec<(f64, f64)>>>,
}

impl Sink for Pad {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        self.index = unknowns.iter().position(|u| u.name == self.node);
        assert!(self.index.is_some(), "the scope resolves PA0's pad {} by name", self.node);
    }
    fn point(&mut self, at: f64, _outer: Option<f64>, x: &[f64]) {
        if let Some(k) = self.index {
            self.points.lock().expect("pad points lock").push((at, x[k]));
        }
    }
}

const PHASE_FRAMES: u32 = 50_000;

/// Frames at 2 ms until `done` holds. The bound is the frames the test drives, not wall time: a
/// busy box makes each frame wait longer, and only a worker that stopped making progress (a
/// hang) runs out of them; a phase here takes a few hundred.
fn frames(host: &mut SimHost, live: &mut SimLive, start: Instant, what: &str, mut done: impl FnMut(&SimHost) -> bool) {
    for _ in 0..PHASE_FRAMES {
        host.step(start.elapsed().as_secs_f64(), live);
        assert!(host.error().is_none(), "{what}: the worker failed: {:?}", host.error());
        if done(host) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("{what}: not reached within {PHASE_FRAMES} frames");
}

#[test]
fn a_firmware_bound_pin_starts_hi_z_and_follows_its_process() {
    let start = Instant::now();
    let design = Arc::new(ledctl());
    // every lamp pin held high (dark) by the bench, except PA0: the firmware's, with no stub drive
    let mut bench = ledctl_bench(10e3, [1.0; 8]);
    bench.stubs[0].pins.retain(|(name, _)| name != "PA0");

    type Made = Arc<Mutex<Vec<(u32, Vec<CoSimPin>, Option<PinMode>)>>>;
    let made: Made = Arc::new(Mutex::new(Vec::new()));
    let runs = Arc::new(AtomicUsize::new(0));
    let supply = Arc::new(Mutex::new(None));
    let maker: CoSimMaker = {
        let (made, runs, supply) = (made.clone(), runs.clone(), supply.clone());
        Arc::new(move |lowered: &Lowered, member: u32| {
            let pins: Vec<CoSimPin> = lowered.cosim_pins("mcu.u").into_iter().filter(|p| p.name == "PA0").collect();
            let lowered_mode = lowered.circuit.devices.gpio.iter().find(|g| g.name == "mcu.u#PA0.io").map(|g| g.mode);
            made.lock().expect("made lock").push((member, pins.clone(), lowered_mode));
            Some(CoSimDevice { dev: Box::new(Toggle { k: 0, runs: runs.clone(), armed: None, supply: supply.clone() }), pins })
        })
    };

    let points = Arc::new(Mutex::new(Vec::new()));
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(bench);
    host.set_cosim(Some((maker, 1)));
    // the pin rows read the toggle's state: PA0 an output, high after an odd toggle (k even)
    host.set_co_pins(Some(Arc::new(|co: &CoState, pin: &mut dyn FnMut(&str, PinLiveMode, Option<bool>, f32)| {
        if let Some(s) = co.get::<ToggleState>() {
            pin("PA0", PinLiveMode::Output, Some(s.k % 2 == 0), 0.0);
        }
    })));
    assert!(host.command(&SimCmd::Pause), "the run starts paused");
    host.act(&SimAction::ShowScope {
        id: 3,
        sink: SinkSlot::new(Box::new(Pad { node: "LED0".into(), index: None, points: points.clone() })),
        // the step's 200 us across a thousand columns: Fast resolves it at 50 ns
        column: 0.2e-6,
    });
    host.commit(&design);
    let mut live = SimLive::default();
    frames(&mut host, &mut live, start, "the first lowering runs", |h| !h.lowering() && h.values().is_some());

    let makes = made.clone();
    let made = made.lock().expect("made lock").clone();
    assert_eq!(made.len(), 1, "one process, for the nominal run: {made:?}");
    let (member, pins, lowered_mode) = &made[0];
    assert_eq!(*member, 0, "the nominal run is member 0");
    assert_eq!(*lowered_mode, Some(PinMode::Input(PinPull::None)), "a pin the bench leaves to the firmware lowers hi-Z");
    let generic = ModelLibs::generic();
    let row = CoSimPin {
        name: "PA0".into(), gpio: "mcu.u#PA0.io".into(), pad: "LED0".into(), vss: "0".into(), vdd: "3V3".into(),
        vih: generic.pin_vih, vil: generic.pin_vil,
    };
    assert_eq!(pins.as_slice(), [row], "the pin table names PA0's GPIO, its pad, its rails and the generic thresholds");

    let end = 4.0 * HALF;
    assert!(host.command(&SimCmd::Step(end)), "Step reaches the live bench");
    frames(&mut host, &mut live, start, "the run reaches the step's end", |_| {
        points.lock().expect("pad points lock").last().is_some_and(|p| p.0 >= end)
    });
    assert!(runs.load(Ordering::Relaxed) >= 4, "the process ran at its instants 0, 50, 100 and 150 us");
    let supply = supply.lock().expect("supply lock").expect("the process read its supply");
    assert!(supply > 3.2 && supply < 3.4, "the bus gives PA0's supply, the 3V3 rail: {supply} V");
    let pad_points = points.clone();
    let points = points.lock().expect("pad points lock").clone();
    for k in 0..4 {
        let (from, to) = (k as f64 * HALF + 10e-6, (k + 1) as f64 * HALF - 1e-6);
        let inside: Vec<f64> = points.iter().filter(|p| p.0 >= from && p.0 <= to).map(|p| p.1).collect();
        assert!(!inside.is_empty(), "the scope has points between {from:e} and {to:e} s");
        for v in inside {
            if k % 2 == 0 {
                assert!(v < 0.3, "PA0 written low at {:e} s holds its pad low (LED 0 sinking): {v} V", k as f64 * HALF);
            } else {
                assert!(v > 3.0, "PA0 written high at {:e} s holds its pad high (LED 0 dark): {v} V", k as f64 * HALF);
            }
        }
    }

    // a firmware step: the Step ended at 200 us, where the toggle at 4 HALF ran; the next stop is
    // before the toggle at 5 HALF, landed on bit-exact and published with the device's state there
    assert!(host.command(&SimCmd::StepFw(StepTo::Statement)), "StepFw reaches the live bench");
    let stop = 5.0 * HALF;
    frames(&mut host, &mut live, start, "the firmware step stops", |h| h.values().is_some_and(|v| v.t >= stop && !v.stepping));
    let v = host.values().expect("the stop is published").clone();
    assert_eq!(v.t.to_bits(), stop.to_bits(), "the step stops bit-exact on the device's next instant: {:e}", v.t);
    let state = v.co.as_ref().and_then(|c| c.get::<ToggleState>()).expect("the published values carry the device's state");
    assert_eq!((state.t.to_bits(), state.k), (v.t.to_bits(), 5), "the state is the device's at the published instant, its toggle 5 not yet run");
    assert!(live.co.is_some() && live.co == v.co, "the views get the published state itself (SimLive::co, the same Arc)");
    assert!((0.0..=1.0).contains(&v.co_share), "the device's share of the stepping time is a share: {}", v.co_share);
    assert!((0.0..=1.0).contains(&v.sink_share) && v.co_share + v.sink_share <= 1.0 + 1e-6,
        "the sinks' share is a share, apart from the device's: {} and {}", v.sink_share, v.co_share);
    let mcu = design.unit(BoardId(0)).and_then(|u| u.circuit.comp("mcu.u")).expect("ledctl has its MCU");
    let pin_ref = |name: &str| {
        let pin = mcu.part.pins.iter().position(|p| p.name == name).expect("the MCU has the pin");
        PinRef { board: BoardId(0), comp: mcu.id, pin: PinIx(pin as u16) }
    };
    let row = |name: &str| host.index().pin(pin_ref(name)).map(|i| host.pins().row(i)).expect("the pin has a row");
    assert_eq!(row("PA0"), pin_row(Some(PinLiveMode::Output), Some(false), 0.0, 0.0), "PA0's row is the stop's state: written low by toggle 4");
    assert_eq!(row("PA1"), PIN_OFF, "a pin the device's state does not name stays off");
    // the stop stands, and PA0's net shows the same instant as its pin row: its pad's voltage at
    // the stop, low
    assert!(!v.running, "the stop stands");
    let pad = design.net_of(pin_ref("PA0")).and_then(|n| host.index().net(n)).expect("PA0 is on a net with a row");
    assert!(host.nets().row(pad)[1] == v.net_v[pad] && v.net_v[pad] < 0.3,
        "PA0's net row holds its pad's voltage at the stop, low: row {} V, at t {} V", host.nets().row(pad)[1], v.net_v[pad]);

    // a pin hovered in the Firmware pane: its pin row and its pad's net light in the rows every
    // view draws, and the views hear of it (SimLive::emphasis) on the next frame, paused as here
    let net = design.net_of(pin_ref("PA0")).and_then(|n| host.index().net(n)).expect("PA0 is on a net with a row");
    let pa0 = host.index().pin(pin_ref("PA0")).expect("PA0 has a row");
    let lit = |host: &SimHost| -> (Vec<usize>, Vec<usize>) {
        ((0..host.nets().nets()).filter(|&n| host.nets().row(n)[3] != 0.0).collect(),
            (0..host.pins().nets()).filter(|&p| host.pins().row(p)[3] != 0.0).collect())
    };
    let heard = live.emphasis;
    host.act(&SimAction::EmphasisDevicePin(Some("PA0".into())));
    assert_eq!(lit(&host), (vec![net], vec![pa0]), "PA0 hovered: its pin row and its pad's net, nothing else");
    assert!(host.step(start.elapsed().as_secs_f64(), &mut live) && live.emphasis != heard,
        "the views hear of the new emphasis on the next frame, with no publication");
    host.act(&SimAction::EmphasisDevicePin(Some("PB7".into())));
    assert_eq!(lit(&host), (vec![], vec![]), "a pin the device has no pad for here lights nothing, and clears the last");
    host.act(&SimAction::EmphasisDevicePin(Some("PA0".into())));
    host.act(&SimAction::EmphasisDevicePin(None));
    assert_eq!(lit(&host), (vec![], vec![]), "the hover gone: nothing lit");

    // Stop keeps the lowering, the firmware's binding: a firmware runner sees no change and builds
    // nothing again, and the next run makes its process at its start, with no set_cosim
    let bound = host.lowered().cloned().expect("the run's lowering");
    assert!(host.command(&SimCmd::Stop), "stop the worker");
    assert!(host.lowered().is_some_and(|l| Arc::ptr_eq(l, &bound)), "Stop keeps the lowering the firmware is bound to");
    pad_points.lock().expect("pad points lock").clear();
    assert!(host.command(&SimCmd::Play), "Play after Stop runs the design again from 0");
    host.commit(&design); // as the app's frames do: the design lowers again
    frames(&mut host, &mut live, start, "the run after Stop passes its first toggle", |_| {
        pad_points.lock().expect("pad points lock").last().is_some_and(|p| p.0 >= HALF)
    });
    let made = makes.lock().expect("made lock").clone();
    assert_eq!(made.iter().map(|m| m.0).collect::<Vec<_>>(), [0, 0], "the run after Stop made its process at its start, from the kept maker: {made:?}");
    let first: Vec<(f64, f64)> = pad_points.lock().expect("pad points lock").iter().copied().filter(|p| p.0 <= HALF - 1e-6).collect();
    assert!(first.first().is_some_and(|p| p.0 < 1e-9), "the run after Stop starts from 0: {:?}", first.first());
    assert!(first.iter().filter(|p| p.0 >= 10e-6).all(|p| p.1 < 0.3),
        "the process writes PA0 low from the run's start (hi-Z without it), its pad low from 10 us: {first:?}");
    assert!(host.command(&SimCmd::Stop), "stop the worker");
}

/// Counts the runs a scope sees start: one `begin` a run.
struct Begins(Arc<AtomicUsize>);

impl Sink for Begins {
    fn begin(&mut self, _unknowns: &[UnknownInfo]) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// With the app's firmware glue, a board's run waits for it: the first run for its first process,
/// and, while the recording plays, a run an edit starts over for the image the glue builds for the
/// new lowering (an edit of what the image compiles in rebuilds it, as a pin or an address does for
/// a chip whose image holds its wiring). Such an edit restarts the run once, with the new image's
/// process from t = 0, named for the edit and not again when the image comes. An edit the glue
/// builds nothing for restarts once too, with the image the run had.
#[test]
fn an_edit_that_rebuilds_the_image_restarts_the_recording_once_with_it() {
    let sources = fab_elec_fixtures::docs("ledavr").expect("ledavr is a stock project");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("ledavr evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "chase").expect("ledavr has its chase bench");
    let design = Arc::new(ev.design.clone());
    let board = design.unit(bench.scope_id()).expect("the bench runs the board").id;
    // each image's maker says which image made the nominal run's process; none runs here
    let made = Arc::new(Mutex::new(Vec::<u128>::new()));
    let image = |id: u128| -> CoSimMaker {
        let made = made.clone();
        Arc::new(move |_: &Lowered, member: u32| {
            if member == 0 {
                made.lock().expect("made lock").push(id);
            }
            None
        })
    };
    let (start, runs) = (Instant::now(), Arc::new(AtomicUsize::new(0)));
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    host.act(&SimAction::ShowScope { id: 6, sink: SinkSlot::new(Box::new(Begins(runs.clone()))), column: 0.0 });
    host.set_rendering(true);
    host.commit(&design);
    let mut live = SimLive::default();
    // the glue at the end of each app frame: the lowering it reads, and whether it builds that
    // one's image (`builds`)
    let glue = |host: &mut SimHost, live: &mut SimLive, builds: &dyn Fn(&Arc<Lowered>) -> bool, what: &str, done: &dyn Fn(&SimHost) -> bool| {
        for _ in 0..PHASE_FRAMES {
            host.step(start.elapsed().as_secs_f64(), live);
            assert!(host.error().is_none(), "{what}: the worker failed: {:?}", host.error());
            let read = host.lowered().cloned();
            let building = read.as_ref().is_some_and(builds);
            host.cosim_building(read.as_ref(), building);
            if done(host) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("{what}: not reached within {PHASE_FRAMES} frames");
    };
    let runs_now = |runs: &AtomicUsize| runs.load(Ordering::Relaxed);

    // the first run waits while its image builds, and starts with it
    let (all, none) = (|_: &Arc<Lowered>| true, |_: &Arc<Lowered>| false);
    glue(&mut host, &mut live, &all, "the first lowering", &|h| h.lowered().is_some());
    let first_lowered = host.lowered().cloned().expect("the first lowering");
    assert!(!first_lowered.firmware.is_empty(), "ledavr's board has firmware");
    for _ in 0..20 {
        glue(&mut host, &mut live, &all, "a frame while the image builds", &|_| true);
    }
    assert!(runs_now(&runs) == 0 && host.values().is_none() && host.lowering(), "no run while its first image builds");
    host.set_cosim(Some((image(1), 1)));
    glue(&mut host, &mut live, &none, "the first run", &|h| h.values().is_some());
    let first = host.content().cloned().expect("the run has its content");
    assert_eq!((runs_now(&runs), made.lock().expect("made lock").clone(), first.edit.clone()), (1, vec![1], None), "one run, with the first image, named by no edit");

    // a part taken out of the circuit while the recording plays (a decoupling capacitor), a new
    // lowering whose image the glue builds: the restart's lowering waits while its image builds
    let mut edited = (*design).clone();
    edited.generation += 3;
    let unit = edited.units.iter_mut().find(|u| u.id == board).expect("ledavr has its board");
    let circuit = Arc::make_mut(&mut unit.circuit);
    let gone = circuit.comps.iter().find(|c| c.path.contains("dec")).map(|c| c.path.clone()).expect("ledavr's MCU has its decoupling");
    circuit.comps.retain(|c| c.path != gone);
    host.commit(&Arc::new(edited));
    let new_lowering = |l: &Arc<Lowered>| !Arc::ptr_eq(l, &first_lowered);
    glue(&mut host, &mut live, &new_lowering, "the edit's restart asked", &|h| h.restart_pending().is_some());
    let name = host.restart_pending().flatten().map(str::to_owned).expect("the restart is named for the edit");
    glue(&mut host, &mut live, &new_lowering, "the restart's lowering", &|h| h.lowered().is_some_and(|l| !Arc::ptr_eq(l, &first_lowered)));
    for _ in 0..20 {
        glue(&mut host, &mut live, &new_lowering, "a frame while the new image builds", &|_| true);
    }
    assert!(runs_now(&runs) == 1 && host.content().is_none() && host.lowering(), "the restarted run waits for the new image");
    host.set_cosim(Some((image(2), 2)));
    glue(&mut host, &mut live, &none, "the restarted run", &|h| h.values().is_some());
    let second = host.content().cloned().expect("the restarted run has its content");
    assert_eq!((runs_now(&runs), made.lock().expect("made lock").clone()), (2, vec![1, 2]), "the edit restarted the run once, its process the new image's");
    assert!(second.edit.as_deref() == Some(name.as_str()) && second.key != first.key, "the run is named for the edit, under a new key: {:?}", second.edit);
    for _ in 0..50 {
        glue(&mut host, &mut live, &none, "a frame of the restarted run", &|_| true);
    }
    assert!(runs_now(&runs) == 2 && host.restart_pending().is_none() && host.content().is_some_and(|c| Arc::ptr_eq(c, &second)), "the image coming restarts nothing more");

    // an edit the glue builds nothing for (the part back): one restart, with the image the run had
    let mut again = (*design).clone();
    again.generation += 6;
    host.commit(&Arc::new(again));
    glue(&mut host, &mut live, &none, "the second edit's restart", &|h| h.content().is_some_and(|c| !Arc::ptr_eq(c, &second)) && h.values().is_some());
    assert_eq!((runs_now(&runs), made.lock().expect("made lock").clone()), (3, vec![1, 2, 2]), "one restart, with the image the run had");
    assert!(host.command(&SimCmd::Stop), "stop the worker");
}

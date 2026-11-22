//! Live instruments on the LED controller fixture. ELEC_LIGHT=1 selects the light theme. The D key
//! steps PA0's PWM duty, a bench stimulus edit: the run retunes in place.
//! D keeps the old run as a ghost under the new one; G drops it.
//! The generator card edits the retained fixture bench spec's pin modes, duties and frequencies.
//! PA7 stands in for a refused source value: its dim controls show the source reason on press.
//! The left list stands in for the explorer and board as a probe drop target; P over a row
//! probes it. The × on a card removes its probe. The strip at the bottom plays, pauses and scrubs the
//! run. ELEC_PROBES=N starts with the first N probes (a tour drops the rest itself).
//! ELEC_FFT=1 starts the PA0 card with a density spectrum and waterfall.
//! ELEC_EXPECT="CLK:freq:9900:10100;3V3:mean:3:" supplies net/answer bounds in base units.
//! Empty min or max leaves that side unbounded; malformed entries are logged once and skipped.
//! ELEC_FW=1 shows the firmware lane over a scripted store that mirrors PA0's starting PWM (a Write at
//! each edge, a timer handler around each rise), standing in until the simulator's firmware store lands.

pub use makepad_widgets;

mod nets;

use fab_elec_bench::{fixture::ledctl_bench, input::Drive, BenchSpec};
use fab_elec_board::Factories;
use fab_elec_design::ops::{BenchOp, DesignOp, ProbeOf};
use fab_elec_host::Engine;
use fab_elec_simhost::SimHost;
use fab_elec_view::{
    instruments::{bench::{BenchAction, BenchInstruments, BenchProbe}, fw_events::{FwEvent, FwKind, FwSource, ScriptedFwEvents}, scope::LaneExpect, sources::{BenchEdit, BenchKey, BenchOut, BenchScope, BenchView, Diag, DocId, UnitState}},
    live::sim::SimAction,
    DesignSnapshot, ElecLive, ViewAction,
};
use makepad_widgets::*;
use nets::{ProbeNets, ProbeNetsAction};
use std::sync::{atomic::AtomicU64, Arc};

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    load_all_resources() do #(App::script_component(vm)) {
        ui: Root {
            main_window := Window {
                window.title: "Bench instruments"
                window.inner_size: vec2(1400, 900)
                body +: {
                    flow: Right
                    nets := ProbeNets {}
                    instruments := BenchInstruments {width: Fill height: Fill}
                }
            }
        }
    }
}

type Loaded = (Arc<DesignSnapshot>, Vec<BenchProbe>, Vec<String>);

/// PA0's duties, stepped by the D key; the other lamps retain their startup duties.
const PA0_DUTIES: [f64; 4] = [0.1, 0.25, 0.4, 0.6];

/// PA0's nominal 10 kHz PWM as firmware would report it over the first two simulated seconds: the
/// image's Writes at each edge, and the emulator's timer handler around each rise.
fn scripted_firmware(duty: f64) -> ScriptedFwEvents {
    const PERIOD: f64 = 100e-6;
    let mut events = Vec::new();
    for k in 0..20_000u64 {
        let rise = k as f64 * PERIOD;
        let seq = k * 5;
        events.push(FwEvent { seq, t: rise - 2e-6, source: FwSource::Trace, kind: FwKind::Enter { handler: 1 }, site: None });
        events.push(FwEvent { seq: seq + 1, t: rise, source: FwSource::Image, kind: FwKind::Write { pin: 0, level: true }, site: Some(3) });
        events.push(FwEvent { seq: seq + 2, t: rise, source: FwSource::Trace, kind: FwKind::Statement { statement: 3 }, site: Some(3) });
        events.push(FwEvent { seq: seq + 3, t: rise + 3e-6, source: FwSource::Trace, kind: FwKind::Exit { handler: 1 }, site: None });
        events.push(FwEvent { seq: seq + 4, t: rise + duty * PERIOD, source: FwSource::Image, kind: FwKind::Write { pin: 0, level: false }, site: Some(5) });
    }
    let mut store = ScriptedFwEvents::new(events);
    store.pin_names.insert(0, "PA0".into());
    store.handler_names.insert(1, "timer".into());
    store
}

fn bench_stimulus(dc: bool, pa0: f64) -> BenchSpec {
    ledctl_bench(10e3, if dc { [0.0; 8] } else { [pa0, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8] })
}

/// Read once at startup, before probes arrive; parsing never enters the frame loop.
fn startup_expects() -> Vec<LaneExpect> {
    let Ok(text) = std::env::var("ELEC_EXPECT") else { return Vec::new() };
    if text.trim().is_empty() { return Vec::new(); }
    let bound = |text: &str| -> Option<Option<f64>> {
        if text.is_empty() { Some(None) }
        else { text.parse::<f64>().ok().filter(|v| v.is_finite()).map(Some) }
    };
    text.split(';').filter_map(|entry| {
        let fields: Vec<_> = entry.split(':').map(str::trim).collect();
        if let [net, answer, min, max] = fields.as_slice() {
            if !net.is_empty() && !answer.is_empty() {
                if let (Some(min), Some(max)) = (bound(min), bound(max)) {
                    if (min.is_some() || max.is_some()) && min.zip(max).is_none_or(|(min, max)| min <= max) {
                        return Some(LaneExpect { name: format!("{net}:{answer}"), net: (*net).into(), answer: (*answer).into(), min, max });
                    }
                }
            }
        }
        log!("benchview: malformed ELEC_EXPECT entry {entry:?}; expected net:answer:min:max with finite ordered bounds");
        None
    }).collect()
}

/// Build the fixture and its full engine results on the task pool.
fn load_fixture() -> Loaded {
    let design = fab_elec_fixtures::ledctl();
    let circuit = &design.units[0].circuit;
    let pin_net = |path: &str, pin: &str| {
        let comp = circuit.comp(path).expect("fixture component exists");
        let pin = comp.part.pin(pin).expect("fixture pin exists");
        let net = comp.conns.iter().find(|(p, _)| *p == pin).expect("fixture pin is connected").1;
        circuit.nets.iter().find(|n| n.id == net).expect("fixture net exists").name.clone()
    };
    // The panes' probe label: "<refdes>.<pad>", then the pin's own name on a second line.
    let pin_label = |path: &str, pin: &str| {
        let comp = circuit.comp(path).expect("fixture component exists");
        let def = comp.part.pin(pin).and_then(|pin| comp.part.pin_def(pin)).expect("fixture pin exists");
        format!("{}.{}\n{}", comp.refdes, def.pads.first().map_or("", String::as_str), def.name)
    };
    let probes = vec![
        BenchProbe {
            net: pin_net("mcu.u", "PA0"),
            label: pin_label("mcu.u", "PA0"),
        },
        BenchProbe {
            net: pin_net("lamps[0].d", "A"),
            label: pin_label("lamps[0].d", "A"),
        },
        BenchProbe {
            net: circuit.net("3V3").expect("fixture supply exists").name.clone(),
            label: "3V3".into(),
        },
    ];
    let mut probes = probes;
    if let Some(n) = std::env::var("ELEC_PROBES").ok().and_then(|n| n.parse::<usize>().ok()) {
        probes.truncate(n);
    }
    let nets = circuit.nets.iter().map(|net| net.name.clone()).collect();
    let engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into());
    (engine.snapshot(), probes, nets)
}

/// The bench text splice is ELANG's; this stand-in applies the op to the tool's probe list only.
fn apply_probe_op(probes: &mut Vec<BenchProbe>, op: &BenchOp) -> bool {
    match op {
        BenchOp::AddProbe { of, label } => {
            let net = &of.target;
            if probes.iter().any(|probe| probe.net == *net) {
                return false;
            }
            probes.push(BenchProbe { net: net.clone(), label: label.clone() });
            true
        }
        BenchOp::RemoveProbe { of } => {
            let net = &of.target;
            let before = probes.len();
            probes.retain(|probe| probe.net != *net);
            probes.len() != before
        }
        _ => false,
    }
}

/// Stand in for ELANG's drive splices on the retained fixture spec.
fn apply_drive_ops(spec: &mut BenchSpec, ops: &[DesignOp]) {
    for op in ops {
        let DesignOp::Bench { bench, op: BenchOp::SetDrive { stub, pin, drive } } = op else { continue };
        if bench != "fixture" { continue; }
        let Some(stub) = spec.stubs.iter_mut().find(|source| source.path == *stub) else { continue };
        let Some((_, value)) = stub.pins.iter_mut().find(|(name, _)| name == pin) else { continue };
        *value = drive.clone();
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    sim: SimHost,
    #[rust]
    snapshot: Option<Arc<DesignSnapshot>>,
    #[rust]
    probes: Vec<BenchProbe>,
    #[rust]
    loading: Option<TaskHandle<Loaded>>,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    finished: bool,
    #[rust]
    duty: usize,
    #[rust]
    bench: BenchSpec,
    #[rust]
    preview: Option<(BenchSpec, String)>,
}

impl App {
    fn show_sources(&mut self, cx: &mut Cx) {
        let Some(unit) = self.snapshot.as_ref().and_then(|snapshot| snapshot.live().units.first()).map(|unit| unit.id) else { return };
        let spec = self.preview.as_ref().map_or(&self.bench, |(spec, _)| spec);
        let sources = BenchView {
            out: BenchOut {
                name: "fixture".into(), doc: DocId(0), scope: BenchScope::Design(unit), spec: spec.clone(),
                expects: vec![], probes: vec![], stop: None, live: false, panel: None, state: UnitState::Built,
            },
            edits: spec.stubs.iter().flat_map(|stub| stub.pins.iter().map(move |(pin, _)| {
                let edit = if pin == "PA7" {
                    BenchEdit::Refused(Diag::error("PA7's duty is computed in the bench source, not a literal: edit it there", Vec::new()))
                } else { BenchEdit::Literal };
                (BenchKey::Stub(stub.path.clone(), pin.clone()), edit)
            })).collect(),
            insert: BenchEdit::Literal,
        };
        if let Some(mut instruments) = self.ui.widget(cx, ids!(instruments)).borrow_mut::<BenchInstruments>() {
            instruments.set_bench(cx, sources);
        }
    }

    fn view_action(&mut self, cx: &mut Cx, action: &ViewAction) {
        match action {
            ViewAction::Preview { ops, label, .. } => {
                let mut preview = self.bench.clone();
                apply_drive_ops(&mut preview, ops);
                self.sim.set_bench(preview.clone());
                self.preview = Some((preview, label.clone()));
                return;
            }
            ViewAction::CommitPreview => {
                if let Some((preview, label)) = self.preview.take() {
                    self.bench = preview;
                    log!("benchview: {label} (stub splice)");
                }
            }
            ViewAction::CancelPreview => {
                self.preview = None;
                self.sim.set_bench(self.bench.clone());
            }
            ViewAction::Ops { ops, label, .. } => {
                self.preview = None;
                let mut probes_changed = false;
                for op in ops {
                    if let DesignOp::Bench { op, .. } = op { probes_changed |= apply_probe_op(&mut self.probes, op); }
                }
                if probes_changed {
                    if let Some(mut instruments) = self.ui.widget(cx, ids!(instruments)).borrow_mut::<BenchInstruments>() {
                        instruments.set_probes(cx, self.probes.clone());
                    }
                }
                if ops.iter().any(|op| matches!(op, DesignOp::Bench { op: BenchOp::SetDrive { .. }, .. })) {
                    apply_drive_ops(&mut self.bench, ops);
                    self.sim.set_bench(self.bench.clone());
                }
                log!("benchview: {label} (stub splice)");
            }
            _ => return,
        }
        self.show_sources(cx);
        if let Some(mut instruments) = self.ui.widget(cx, ids!(instruments)).borrow_mut::<BenchInstruments>() {
            instruments.set_refusal(cx, None);
        }
    }

    fn request_frame(&mut self, cx: &mut Cx) {
        if !self.finished && !cx.next_frame_is_pending(self.next_frame) {
            self.next_frame = cx.new_next_frame();
        }
    }

    fn poll_fixture(&mut self, cx: &mut Cx) {
        let Some(result) = self.loading.as_mut().and_then(|job| job.try_take()) else {
            return;
        };
        self.loading = None;
        match result {
            Ok((snapshot, probes, nets)) => {
                for probe in &probes {
                    log!("benchview: {} probes {}", probe.label, probe.net);
                }
                cx.set_global(ElecLive::new(snapshot.clone()));
                self.snapshot = Some(snapshot);
                self.probes = probes;
                if let Some(mut pane) = self.ui.widget(cx, ids!(nets)).borrow_mut::<ProbeNets>() {
                    pane.set_nets(cx, nets);
                }
                if let Some(mut instruments) = self.ui.widget(cx, ids!(instruments)).borrow_mut::<BenchInstruments>() {
                    instruments.set_probes(cx, self.probes.clone());
                    if std::env::var("ELEC_FFT").is_ok_and(|value| value == "1") {
                        if let Some(pa0) = self.probes.first() { instruments.set_probe_domain(cx, &pa0.net, fab_elec_view::instruments::scope::Domain::Fft); }
                    }
                    if std::env::var("ELEC_FW").is_ok_and(|value| value == "1") {
                        instruments.set_fw_events(cx, Some(Arc::new(scripted_firmware(PA0_DUTIES[self.duty]))));
                    }
                }
                self.show_sources(cx);
                self.ui.redraw(cx);
            }
            Err(error) => {
                log!("benchview: fixture load failed: {error:?}");
                self.finished = true;
                cx.quit();
            }
        }
    }
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        self.sim = SimHost::new();
        let expects = startup_expects();
        if let Some(mut instruments) = self.ui.widget(cx, ids!(instruments)).borrow_mut::<BenchInstruments>() {
            instruments.set_expects(cx, expects);
        }
        let dc = std::env::var("ELEC_BENCH").is_ok_and(|bench| bench == "dc");
        self.bench = bench_stimulus(dc, PA0_DUTIES[self.duty]);
        self.sim.set_bench(self.bench.clone());
        log!("benchview: bench {}", if dc { "dc" } else { "pwm" });
        match cx.task_pool().submit(Class::Visible, load_fixture) {
            Ok(job) => self.loading = Some(job),
            Err(error) => {
                log!("benchview: could not schedule fixture load: {error:?}");
                self.finished = true;
                cx.quit();
            }
        }
        self.request_frame(cx);
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        self.sim.handle_actions(actions);
        let uid = self.ui.widget(cx, ids!(nets)).widget_uid();
        for action in actions.filter_widget_actions_cast::<ProbeNetsAction>(uid) {
            if let ProbeNetsAction::Probe(net) = action {
                let op = BenchOp::AddProbe { of: ProbeOf::v(net.clone()), label: net };
                if apply_probe_op(&mut self.probes, &op) {
                    if let BenchOp::AddProbe { of, .. } = &op {
                        let net = &of.target;
                        log!("benchview: probe {net} (stub splice)");
                    }
                    if let Some(mut instruments) = self.ui.widget(cx, ids!(instruments)).borrow_mut::<BenchInstruments>() {
                        instruments.set_probes(cx, self.probes.clone());
                    }
                }
            }
        }
        let instruments = self.ui.widget(cx, ids!(instruments));
        let uid = instruments.widget_uid();
        for action in actions.filter_widget_actions_cast::<BenchAction>(uid) {
            match action {
                BenchAction::RemoveProbe { net } => {
                    let op = BenchOp::RemoveProbe { of: ProbeOf::v(net.clone()) };
                    if apply_probe_op(&mut self.probes, &op) {
                        log!("benchview: unprobe {net} (stub splice)");
                        if let Some(mut instruments) = instruments.borrow_mut::<BenchInstruments>() {
                            instruments.set_probes(cx, self.probes.clone());
                        }
                    }
                }
                BenchAction::None => {}
            }
        }
        for action in actions.filter_widget_actions(uid).filter_map(|action| action.action.downcast_ref::<ViewAction>()) {
            self.view_action(cx, action);
        }
        self.request_frame(cx);
    }

    fn handle_key_down(&mut self, cx: &mut Cx, e: &KeyEvent) {
        if e.key_code == KeyCode::KeyD && !e.is_repeat {
            cx.action(SimAction::BeginGesture);
            self.duty = (self.duty + 1) % PA0_DUTIES.len();
            let freq = self.bench.stubs.iter().find(|stub| stub.path == "mcu.u")
                .and_then(|stub| stub.pins.iter().find(|(pin, _)| pin == "PA0"))
                .and_then(|(_, drive)| match drive { Drive::Pwm { freq, .. } => Some(*freq), _ => None }).unwrap_or(10e3);
            let uid = self.ui.widget(cx, ids!(instruments)).widget_uid();
            cx.widget_action(uid, ViewAction::Ops {
                place: vec![],
                ops: vec![DesignOp::Bench { bench: "fixture".into(), op: BenchOp::SetDrive {
                    stub: "mcu.u".into(), pin: "PA0".into(), drive: Drive::Pwm { freq, duty: PA0_DUTIES[self.duty] },
                } }],
                label: "mcu.u PA0 duty".into(),
            });
        } else if e.key_code == KeyCode::KeyG && !e.is_repeat {
            cx.action(SimAction::ClearGhost);
        }
    }

    fn handle_shutdown(&mut self, _cx: &mut Cx) {
        self.finished = true;
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        if std::env::var("ELEC_LIGHT").is_ok_and(|value| value == "1") {
            set_base_theme(vm.cx_mut(), BaseTheme::Light);
        }
        makepad_widgets::script_mod(vm);
        fab_elec_view::live::script_mod(vm);
        fab_elec_view::trace::script_mod(vm);
        fab_elec_view::instruments::script_mod(vm);
        nets::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        if self.finished {
            self.ui.handle_event(cx, event, &mut Scope::empty());
            return;
        }
        let tick = self.next_frame.is_event(event).is_some();
        if matches!(event, Event::Signal) || tick {
            self.poll_fixture(cx);
            if let Some(snapshot) = &self.snapshot {
                if self.sim.frame(cx, &snapshot.design, tick) {
                    self.request_frame(cx);
                }
            } else if self.loading.is_some() {
                self.request_frame(cx);
            }
        }
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}

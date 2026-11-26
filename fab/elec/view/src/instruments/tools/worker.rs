//! Serial tool execution and worker-owned source lifetimes.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use fab_elec_bench::source::*;
use fab_elec_bench::trigger::Slope;
use fab_elec_design::{BenchOp, ProbeOf};
use fab_elec_instr::{Signal, Synth, SynthChannel, SynthConfig};
use makepad_widgets_core::ThreadSpawner;
use crate::instruments::acquire::SimChannel;
use crate::instruments::device::DeviceSource;
use crate::instruments::acquire::NetProbe;
use super::call::Arguments;
use super::capture::{Capture, CaptureIntake, CaptureSpec, CaptureStore};
use super::{compare, decode, expect, features, query};
use super::reply::{self, Object};
use super::{CallId, HostRequest, Invocation, ProbeInfo, ProbeSnapshot, SaveWriter, ToolReply, TraceReader};

const TICK: Duration = Duration::from_millis(5);

struct Pending {
    spec: CaptureSpec,
    resolved: Resolved,
    cancel: Arc<AtomicBool>,
    setup: Option<Setup>,
}
struct Factory {
    intake: Option<CaptureIntake>,
    generation: u64,
}
impl IntakeFactory for Factory {
    fn make(&mut self, layout: &Layout) -> Box<dyn Intake> {
        // Old packets may precede the new configuration's layout announcement.
        if layout.generation != self.generation { return Box::new(Empty); }
        self.intake.take().map_or_else(|| Box::new(Empty) as Box<dyn Intake>, |i| Box::new(i))
    }
}
struct Empty;
impl Intake for Empty {
    fn begin(&mut self, _: &Layout) {}
    fn point(&mut self, _: f64, _: &[f32]) {}
    fn block(&mut self, _: &Block) {}
    fn mark(&mut self, _: f64, _: Mark) {}
    fn gap(&mut self, _: f64, _: f64, _: Gap) {}
    fn idle(&mut self) {}
}

pub(super) fn synth() -> Synth {
    let mut signal = fab_elec_instr::synth::Synth::pwm();
    signal.freq = 1000.0; signal.duty = 0.5; signal.noise = 0.0;
    Synth::new(SynthConfig { side: Side::Sim, channels: vec![SynthChannel { signal: Signal::Synth(signal), name: "CH1".into() }],
        seed: 1, limits: None, member_spread: None, pace: None }).expect("built-in synth configuration is valid")
}

pub(super) fn run(
    calls: Receiver<Invocation>, replies: Sender<(CallId, ToolReply)>, host: Sender<HostRequest>,
    stop: Arc<AtomicBool>, spawner: ThreadSpawner, cap: usize, save: SaveWriter, read: TraceReader, now: fn() -> Instant,
) {
    let (complete, completed) = channel();
    let mut worker = Worker { host, stop, spawner, cap, save, read, opened: BTreeMap::new(), now, store: CaptureStore::default(), pending: BTreeMap::new(),
        complete, completed, lists: BTreeMap::new(), next_list: 0, expect_lists: expect::Lists::default(), expect_runs: expect::Runs::default(), synth: None, active_synth: None,
        synth_queue: VecDeque::new(), synth_generation: 0, errors: BTreeMap::new(), decoded: decode::Cache::default(), compared: compare::Cache::default() };
    while !worker.stop.load(Ordering::Acquire) {
        worker.collect();
        match calls.recv_timeout(TICK) {
            Ok(call) => {
                let result = Arguments::parse(&call.call).and_then(|args| worker.call(&call.call.tool, &args));
                let result = result.unwrap_or_else(reply::failed);
                let _ = replies.send((call.id, result));
            }
            Err(RecvTimeoutError::Timeout) => (),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    for pending in worker.pending.values() { pending.cancel.store(true, Ordering::Release); }
}

struct Worker {
    host: Sender<HostRequest>,
    stop: Arc<AtomicBool>,
    /// The clock every deadline reads (`Instant::now` but in tests).
    now: fn() -> Instant,
    spawner: ThreadSpawner,
    cap: usize,
    save: SaveWriter,
    read: TraceReader,
    /// Each opened trace's path and its capture: opening it again replaces that capture.
    opened: BTreeMap<String, String>,
    store: CaptureStore,
    decoded: decode::Cache,
    compared: compare::Cache,
    pending: BTreeMap<String, Pending>,
    complete: Sender<Arc<Capture>>,
    completed: Receiver<Arc<Capture>>,
    lists: BTreeMap<u64, Vec<String>>,
    next_list: u64,
    expect_lists: expect::Lists,
    expect_runs: expect::Runs,
    synth: Option<DeviceSource<Synth>>,
    active_synth: Option<(String, AttachId)>,
    synth_queue: VecDeque<String>,
    synth_generation: u64,
    errors: BTreeMap<String, String>,
}
impl Worker {
    fn completed(&mut self, capture: Arc<Capture>) {
        if let Some(pending) = self.pending.remove(&capture.id) {
            // The capture takes no more points: its host detaches it (`HostRequest::SimCapture`).
            pending.cancel.store(true, Ordering::Release);
            self.detach(&capture.id);
            self.store.insert(capture);
        }
    }
    fn detach(&mut self, id: &str) {
        if self.active_synth.as_ref().is_some_and(|(active, _)| active == id) {
            let (_, attach) = self.active_synth.take().unwrap();
            if let Some(source) = &mut self.synth {
                source.detach(attach);
                // A cancelled first request must still announce its layout before the next
                // request can compare against the applied configuration.
                if source.layout_generation() >= self.synth_generation { source.park(); }
            }
        }
    }
    fn start_next(&mut self) {
        if self.active_synth.is_some() { return; }
        if let Some(source) = &mut self.synth {
            if source.layout_generation() < self.synth_generation { return; }
            source.park();
        }
        while let Some(id) = self.synth_queue.pop_front() {
            let Some(pending) = self.pending.get_mut(&id) else { continue };
            let Some(setup) = pending.setup.take() else { continue };
            if self.synth.is_none() {
                match DeviceSource::open_on(self.spawner.clone(), synth()) {
                    Ok(source) => self.synth = Some(source),
                    Err(e) => { self.errors.insert(id, format!("source synth: {e}")); continue; }
                }
            }
            let source = self.synth.as_mut().unwrap();
            let resolved = source.resolve(&setup);
            if resolved.restart { self.synth_generation += 1; }
            pending.resolved = resolved.clone();
            let intake = CaptureIntake::new(id.clone(), pending.spec.clone(), source.info().side, resolved, None, self.complete.clone(), pending.cancel.clone());
            let attach = source.attach(Box::new(Factory { intake: Some(intake), generation: self.synth_generation }));
            source.request(setup);
            self.active_synth = Some((id, attach));
            break;
        }
    }
    fn collect(&mut self) {
        while let Ok(capture) = self.completed.try_recv() { self.completed(capture); }
        if let Some((id, _)) = &self.active_synth {
            if let Some(error) = self.synth.as_ref().and_then(|s| s.status().error) {
                let id = id.clone();
                self.errors.insert(id.clone(), format!("source synth: {error}"));
                self.detach(&id);
                self.synth = None; self.synth_generation = 0;
                if let Some(pending) = self.pending.get(&id) { pending.cancel.store(true, Ordering::Release); }
            }
        }
        self.start_next();
    }
    /// Ask the host and wait for its answer, however long a busy machine keeps it: the wait ends
    /// on the answer, on the host going away (a host that drops the request, or its whole queue,
    /// drops the reply sender with it) or on the tools stopping. No clock bounds it: a slow host
    /// is no failed call. The host answers every request it takes, refusals included.
    fn host<T>(&self, request: impl FnOnce(Sender<Result<T, String>>) -> HostRequest) -> Result<T, String> {
        let (reply, response) = channel();
        self.host.send(request(reply)).map_err(|_| "host: disconnected")?;
        loop {
            if self.stop.load(Ordering::Acquire) { return Err("host: tools stopped".into()); }
            match response.recv_timeout(TICK) {
                Ok(answer) => return answer,
                Err(RecvTimeoutError::Disconnected) => return Err("host: reply sender disconnected".into()),
                Err(RecvTimeoutError::Timeout) => (),
            }
        }
    }
    fn probes(&self) -> Result<ProbeSnapshot, String> { self.host(HostRequest::Probes) }
    fn call(&mut self, tool: &str, args: &Arguments) -> Result<ToolReply, String> {
        let budget = args.budget(self.cap);
        match tool {
            "elec.expect.list" => {
                let snapshot = if args.text("after").is_none() { Some(self.host(HostRequest::Expects)?) } else { None };
                self.expect_lists.list(args, budget, snapshot)
            }
            "elec.expect.run" => match args.text("after") {
                Some(after) => self.expect_runs.after(after, budget),
                None => expect::run(args, &self.host, &self.stop, TICK, self.now).map(|answer| self.expect_runs.answer(answer, budget)),
            },
            "elec.source.list" => {
                let probes = self.probes()?;
                let caps = sim_caps(&probes)?;
                let sim_state = if self.pending.values().any(|p| p.spec.source == "sim") { "busy" } else { "open" };
                let synth_state = if self.active_synth.is_some() { "busy" } else if self.synth.is_some() { "open" } else { "closed" };
                Ok(query::sources(&caps, &synth().caps(), sim_state, synth_state))
            }
            "elec.probe.add" => self.add_probe(args),
            "elec.probe.remove" => {
                let snapshot = self.probes()?;
                let id = args.text("probe").unwrap();
                let probe = find_probe(&snapshot.probes, id).ok_or_else(|| format!("probe {id}: unknown probe"))?;
                self.host(|reply| HostRequest::Edit(BenchOp::RemoveProbe { of: ProbeOf::v(probe.target.clone()) }, reply))?;
                Ok(Object::default().reply())
            }
            "elec.capture.plan" | "elec.capture.run" => {
                let source = args.text("source").unwrap();
                let probes = match source { "sim" => Some(self.probes()?), "synth" => None, _ => return Err(format!("source: unknown {source}")) };
                let caps = if let Some(probes) = &probes { sim_caps(probes)? } else { synth().caps() };
                let (spec, setup) = plan(args, &caps, probes.as_ref())?;
                let resolved = resolve(&caps, &setup, None);
                if tool == "elec.capture.plan" { return Ok(query::resolved(&spec, &resolved).reply()); }
                let wait = timeout(args)?;
                let id = self.store.reserve();
                let cancel = Arc::new(AtomicBool::new(false));
                let mut pending = Pending { spec: spec.clone(), resolved: resolved.clone(), cancel: cancel.clone(), setup: None };
                if source == "synth" {
                    pending.setup = Some(setup);
                    self.synth_queue.push_back(id.clone());
                } else {
                    let snapshot = probes.unwrap();
                    // a channel is its net, as a saved take names it; the probe's label is its display name
                    let channels = snapshot.probes.iter().enumerate().map(|(i, p)| SimChannel {
                        info: ChannelInfo { id: ChanId(i as u32), name: p.target.clone(), quantity: p.quantity },
                        probe: NetProbe { pos: p.target.clone(), neg: None, scale: 1.0 },
                    }).collect();
                    let labels = snapshot.probes.iter().map(|p| p.label.clone()).collect();
                    let ended = cancel.clone();
                    let intake = CaptureIntake::new(id.clone(), spec, Side::Sim, resolved, snapshot.firmware, self.complete.clone(), cancel).with_labels(labels);
                    self.host(|reply| HostRequest::SimCapture { channels, intake: Box::new(intake), ended, reply })?;
                }
                self.pending.insert(id.clone(), pending);
                self.wait(&id, wait)
            }
            "elec.capture.wait" => self.wait(args.text("capture").unwrap(), timeout(args)?),
            "elec.capture.list" => self.list(args, budget),
            "elec.capture.open" => {
                let path = args.text("path").unwrap();
                let parsed = super::capture::parse_trace(&(self.read)(path)?).map_err(|why| format!("{path}: {why}"))?;
                let name = path.rsplit('/').next().and_then(|file| file.strip_suffix(".trace")).unwrap_or(path).to_string();
                let (spec, resolved) = replay_plan(&parsed, &name);
                let id = self.store.reserve();
                let capture = Arc::new(Capture::replayed(id.clone(), name, spec, resolved, parsed).map_err(|why| format!("{path}: {why}"))?);
                // one capture per opened file: opening it again replaces the one before
                if let Some(old) = self.opened.insert(path.to_string(), id.clone()) {
                    if self.store.drop_capture(&old) { self.decoded.drop_capture(&old); self.compared.drop_capture(&old); }
                }
                self.store.insert(capture.clone());
                let shown = super::capture::OpenedTrace { path: path.into(), layout: capture.layout.clone(), labels: capture.labels.clone(), store: capture.trace.clone() };
                let _ = self.host.send(HostRequest::Show(Arc::new(shown)));
                let mut out = Object::default();
                out.text("capture", &id); out.text("path", path);
                out.field("channels", reply::list(capture.layout.channels.iter().map(|c| reply::quote(&c.name))));
                out.num("from", Some(capture.span.0), fab_elec_circuit::QtyUnit::Second); out.num("to", Some(capture.span.1), fab_elec_circuit::QtyUnit::Second);
                out.int("samples", capture.trace.len());
                Ok(out.reply())
            }
            "elec.capture.drop" => {
                let id = args.text("capture").unwrap();
                let cached_id = self.store.get(id).map(|capture| capture.id.clone());
                let dropped = if let Some(pending) = self.pending.remove(id) { pending.cancel.store(true, Ordering::Release); true } else { false };
                self.detach(id);
                self.errors.remove(id);
                self.start_next();
                if !self.store.drop_capture(id) && !dropped { return Err(format!("capture: unknown {id}")); }
                if let Some(id) = cached_id { self.decoded.drop_capture(&id); self.compared.drop_capture(&id); }
                Ok(Object::default().reply())
            }
            "elec.capture.save" => {
                let id = args.text("capture").ok_or_else(|| "capture: name the capture to save (the host saves the Scope's take without one)".to_string())?;
                let capture = self.store.get(id).ok_or_else(|| format!("capture: unknown or unfinished {id}"))?;
                let path = super::capture::trace_path(args.text("name").unwrap_or(&capture.name))?;
                let path = (self.save)(&path, &capture.bytes())?;
                self.store.set_saved(id.into(), path.clone());
                let mut out = Object::default(); out.text("path", &path); Ok(out.reply())
            }
            "elec.capture.decode" => {
                let id = args.text("capture").unwrap();
                let capture = self.store.get(id).ok_or_else(|| format!("capture: unknown or unfinished {id}"))?;
                decode::decode(capture, args, budget, &mut self.decoded)
            }
            "elec.capture.compare" => {
                let e = args.text("expected").unwrap(); let a = args.text("actual").unwrap();
                let expected = self.store.get(e).ok_or_else(|| format!("expected: unknown or unfinished capture {e}"))?;
                let actual = self.store.get(a).ok_or_else(|| format!("actual: unknown or unfinished capture {a}"))?;
                compare::compare(expected, actual, args, budget, &mut self.compared)
            }
            "elec.capture.summary" | "elec.capture.samples" | "elec.measure" => {
                let id = args.text("capture").unwrap();
                let capture = self.store.get(id).ok_or_else(|| format!("capture: unknown or unfinished {id}"))?;
                match tool {
                    "elec.capture.summary" => query::summary(capture, args, budget),
                    "elec.capture.samples" => query::samples(capture, args, budget),
                    _ => query::measure(capture, args, budget),
                }
            }
            "elec.capture.edges" | "elec.capture.pulses" => {
                let id = args.text("capture").unwrap();
                let capture = self.store.get(id).ok_or_else(|| format!("capture: unknown or unfinished {id}; use capture.list or capture.wait"))?;
                if tool == "elec.capture.edges" { features::edges(capture, args, budget) }
                else { features::pulses(capture, args, budget) }
            }
            _ => Err(format!("tool: unknown {tool}")),
        }
    }
    fn add_probe(&self, args: &Arguments) -> Result<ToolReply, String> {
        let quantity = args.text("quantity").unwrap();
        let target = args.text("target").unwrap();
        if quantity == "p" { return Err(format!("probe {target}: power needs the simulator Sink power extension; it cannot be computed from node voltages")); }
        let snapshot = self.probes()?;
        let probe = snapshot.targets.iter().chain(&snapshot.probes).find(|p| p.target == target || p.label == target || p.id == target)
            .ok_or_else(|| format!("probe {target}: net or branch is not in this circuit"))?;
        if quantity == "i" {
            if probe.quantity != Quantity::Amp { return Err(format!("probe {target}: current requires a branch-current unknown (a source or inductor), not a net voltage")); }
            return Err(format!("probe {target}: the bench AddProbe edit currently supports only nets; branch-current edits need the bench-language extension"));
        }
        if probe.quantity != Quantity::Volt { return Err(format!("probe {target}: voltage requires a net")); }
        let label = args.text("label").unwrap_or(target);
        let site = self.host(|reply| HostRequest::Edit(BenchOp::AddProbe { of: ProbeOf::v(probe.target.clone()), label: label.into() }, reply))?;
        let existing = snapshot.probes.iter().find(|p| p.target == probe.target);
        let id = existing.map_or_else(|| format!("probe:{}", probe.target), |p| p.id.clone());
        let lane = existing.map_or_else(|| snapshot.probes.iter().map(|p| p.lane + 1).max().unwrap_or(0), |p| p.lane);
        let mut out = Object::default(); out.text("probe", &id); out.int("lane", lane); out.text("unit", "V");
        out.text("net", &probe.target); out.site("site", &site); Ok(out.reply())
    }
    fn wait(&mut self, id: &str, wait: Duration) -> Result<ToolReply, String> {
        let deadline = (self.now)().checked_add(wait).ok_or("timeout: duration is too large")?;
        loop {
            self.collect();
            if let Some(capture) = self.store.get(id) { return Ok(query::done(capture).reply()); }
            if let Some(error) = self.errors.get(id) { return Err(error.clone()); }
            let pending = self.pending.get(id).ok_or_else(|| format!("capture: unknown {id}"))?;
            if self.stop.load(Ordering::Acquire) { return Err("capture: tools stopped".into()); }
            let left = deadline.saturating_duration_since((self.now)());
            if left.is_zero() { return Ok(query::waiting(id, &pending.spec, &pending.resolved).reply()); }
            match self.completed.recv_timeout(left.min(TICK)) {
                Ok(capture) => self.completed(capture),
                Err(RecvTimeoutError::Timeout) => (),
                Err(RecvTimeoutError::Disconnected) => return Err("capture: acquisition disconnected".into()),
            }
        }
    }
    fn list(&mut self, args: &Arguments, budget: usize) -> Result<ToolReply, String> {
        let side = args.text("side").unwrap_or("all");
        let version = if let Some(cursor) = args.text("after") {
            cursor.split('/').nth(1).and_then(|s| s.parse::<u64>().ok()).ok_or("after: invalid capture list cursor")?
        } else {
            self.next_list += 1;
            let mut entries: BTreeMap<u64, String> = self.store.captures().filter(|c| side == "all" || query::side(c.side) == side)
                .map(|c| (c.id[3..].parse().unwrap(), query::list_item(c, self.store.saved(&c.id)))).collect();
            if side == "all" || side == "sim" {
                for (id, pending) in &self.pending {
                    let mut out = Object::default(); out.text("id", id);
                    out.text("name", if pending.spec.name.is_empty() { id } else { &pending.spec.name });
                    out.symbol("side", "sim"); out.text("source", &pending.spec.source);
                    out.field("span", reply::list([reply::qty(pending.spec.span.0, fab_elec_circuit::QtyUnit::Second), reply::qty(pending.spec.span.1, fab_elec_circuit::QtyUnit::Second)]));
                    out.int("channels", pending.resolved.channels.len()); out.int("samples", 0);
                    entries.insert(id[3..].parse().unwrap(), out.render());
                }
            }
            let items = entries.into_values().collect();
            self.lists.insert(self.next_list, items);
            self.next_list
        };
        let items = self.lists.get(&version).ok_or("after: unknown capture list cursor")?;
        let key = format!("captures/{version}/{side}");
        let start = reply::offset(args.text("after"), &key, items.len())?;
        Ok(reply::page(budget, start, items.len(), &key, |i| items[i].clone(), |items, next| {
            let mut out = Object::default(); out.field("captures", reply::list(items.iter().cloned()));
            out.int("total", self.lists[&version].len()); out.next(next); out.render()
        }))
    }
}

fn timeout(args: &Arguments) -> Result<Duration, String> {
    Duration::try_from_secs_f64(args.num("timeout").unwrap()).map_err(|_| "timeout: expected a nonnegative representable duration".into())
}
fn find_probe<'a>(probes: &'a [ProbeInfo], id: &str) -> Option<&'a ProbeInfo> {
    probes.iter().find(|p| p.id == id || p.label == id || p.target == id)
}
fn sim_caps(snapshot: &ProbeSnapshot) -> Result<Caps, String> {
    if !snapshot.max_step.is_finite() || snapshot.max_step <= 0.0 || !(1.0 / snapshot.max_step).is_finite() {
        return Err("source sim: host must provide a finite positive maximum step".into());
    }
    let mut caps = synth().caps();
    caps.channels = snapshot.probes.iter().enumerate().map(|(i, p)| ChannelCaps {
        id: ChanId(i as u32), name: p.target.clone(), kind: ChanKind::Analog, quantity: p.quantity,
        ranges: Ranges::Any, bits: 0, coupling: vec![Coupling::Dc], attenuation: Atten::Free, bandwidth: None, bw_limits: Vec::new(),
    }).collect();
    caps.rate = RateCaps::List(vec![1.0 / snapshot.max_step]);
    caps.powers.probe_any_net = true; caps.powers.current = true;
    Ok(caps)
}
/// What an opened trace was taken with, as far as its file says: its whole record as the window
/// around its trigger instant, every channel on at the record's mean rate, no trigger to wait for.
fn replay_plan(trace: &super::capture::ParsedTrace, name: &str) -> (CaptureSpec, Resolved) {
    let (first, last) = (trace.times.first().copied().unwrap_or(0.0), trace.times.last().copied().unwrap_or(0.0));
    let n = trace.times.len();
    let rate = if n > 1 && last > first { (n - 1) as f64 / (last - first) } else { 1.0 };
    let mut caps = synth().caps();
    caps.channels = trace.layout.channels.iter().map(|c| ChannelCaps {
        id: c.id, name: c.name.clone(), kind: ChanKind::Analog, quantity: c.quantity,
        ranges: Ranges::Any, bits: 0, coupling: vec![Coupling::Dc], attenuation: Atten::Free, bandwidth: None, bw_limits: Vec::new(),
    }).collect();
    caps.rate = RateCaps::List(vec![rate]);
    let span = (first - trace.t_trigger, last - trace.t_trigger);
    let setup = Setup {
        channels: caps.channels.iter().map(|c| ChannelSetup { id: c.id, on: true, window: (-1.0, 1.0), coupling: Coupling::Dc,
            probe: Probe::default(), bw_limit: None, invert: false }).collect(),
        time: TimeSetup { span: span.1 - span.0, pre: (-span.0).max(0.0), rate: RateWant::Exact(rate), depth: DepthWant::Samples(n as u64), plot_px: n as u32 },
        trigger: TriggerSpec { kind: fab_elec_bench::trigger::Qualifier::None.kind(), channel: caps.channels.first().map_or(ChanId(0), |c| c.id),
            level: 0.0, hyst: 0.0, slope: Slope::Rising, holdoff: 0.0, run: TriggerRun::Single },
        mode: ModeWant::Stream, drives: Vec::new(),
    };
    let spec = CaptureSpec { source: "trace".into(), name: name.into(), span, trigger: "none".into(), qualifier: fab_elec_bench::trigger::Qualifier::None,
        channel: 0, on: trace.layout.channels.first().map_or_else(String::new, |c| c.name.clone()), level: 0.0, slope: Slope::Rising, holdoff: 0.0 };
    (spec, resolve(&caps, &setup, None))
}

fn plan(args: &Arguments, caps: &Caps, probes: Option<&ProbeSnapshot>) -> Result<(CaptureSpec, Setup), String> {
    let from = args.num("from").unwrap(); let to = args.num("to").unwrap();
    if from >= to || to < 0.0 || !(to - from).is_finite() { return Err("to: window must be increasing and end at or after the trigger".into()); }
    if caps.channels.is_empty() { return Err("on: source has no probes; add a probe first".into()); }
    let requested = args.text("on");
    let channel = if let Some(name) = requested {
        let found = if let Some(snapshot) = probes {
            snapshot.probes.iter().position(|p| p.id == name || p.label == name || p.target == name)
        } else { caps.channels.iter().position(|c| c.name == name || c.id.0.to_string() == name) };
        found.ok_or_else(|| format!("on: unknown probe {name}"))?
    } else { 0 };
    let range = probes.and_then(|p| p.probes[channel].range).unwrap_or(if probes.is_none() { (0.0, 3.3) } else { (-1.0, 1.0) });
    if !range.0.is_finite() || !range.1.is_finite() || range.0 > range.1 {
        return Err("on: host supplied an invalid probe range".into());
    }
    let level = args.num("level").unwrap_or(range.0 * 0.5 + range.1 * 0.5);
    let slope = match args.text("slope").unwrap() { "fall" => Slope::Falling, "either" => Slope::Either, _ => Slope::Rising };
    let holdoff = args.num("holdoff").unwrap();
    if holdoff < 0.0 { return Err("holdoff: expected a nonnegative duration".into()); }
    let rate = args.num("rate").unwrap_or(args.int("resolution").unwrap() as f64 / (to - from));
    if !rate.is_finite() || rate <= 0.0 || !(rate * (to - from)).is_finite() { return Err("rate: expected a positive finite rate and depth".into()); }
    let trigger = args.text("trigger").unwrap();
    let qualifier = if matches!(trigger, "width" | "period") {
        use fab_elec_bench::trigger::{validate_qualifier, Qualifier};
        if args.num("min_interval").is_none() && args.num("max_interval").is_none() {
            return Err("min_interval, max_interval: a width or period trigger requires at least one bound".into());
        }
        let min = args.num("min_interval").unwrap_or(0.0);
        let max = args.num("max_interval").unwrap_or(f64::INFINITY);
        let qualifier = if trigger == "width" { Qualifier::Width { min, max } } else { Qualifier::Period { min, max } };
        if !validate_qualifier(qualifier) { return Err("min_interval, max_interval: expected finite min >= 0 and max > min".into()); }
        qualifier
    } else if trigger == "runt" {
        use fab_elec_bench::trigger::{validate_runt, EdgeTrigger, Qualifier};
        let other = args.num("runt_level").ok_or("runt_level: a runt trigger requires a second level in volts")?;
        if !validate_runt(&EdgeTrigger { level, hyst: 0.0, slope, holdoff }, other) {
            return Err("runt_level: expected a finite level beyond the trigger on the selected slope's side".into());
        }
        Qualifier::Runt { other }
    } else { fab_elec_bench::trigger::Qualifier::None };
    let spec = CaptureSpec { source: args.text("source").unwrap().into(), name: args.text("name").unwrap_or("").into(), span: (from, to),
        trigger: trigger.into(), qualifier, channel, on: requested.unwrap_or(&caps.channels[channel].name).into(), level, slope, holdoff };
    let setup = Setup {
        channels: caps.channels.iter().map(|c| ChannelSetup { id: c.id, on: true, window: range, coupling: Coupling::Dc,
            probe: Probe::default(), bw_limit: None, invert: false }).collect(),
        time: TimeSetup { span: to - from, pre: (-from).max(0.0), rate: RateWant::Exact(rate),
            depth: args.int("depth").map_or(DepthWant::Auto, |n| DepthWant::Samples(n as u64)), plot_px: args.int("resolution").unwrap() as u32 },
        trigger: TriggerSpec { kind: qualifier.kind(), channel: caps.channels[channel].id, level, hyst: 0.0, slope, holdoff, run: TriggerRun::Single },
        mode: ModeWant::Stream, drives: Vec::new(),
    };
    Ok((spec, setup))
}

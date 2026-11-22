//! The Scope's tools for the AI (`fab_elec_view::instruments::tools`): probes, captures of the
//! running bench and what is read from them, on their own long-lived worker, which starts with the
//! first call. A call waits in the host's queue like any other, and the calls behind it wait for
//! its answer. The worker asks this host for what only it has: the running bench's probes and
//! nets, a probe edit (the bench's own `elec.bench.probe` step, as + Add probe and a probe card's
//! remove make it) and a capture's intake on the running bench, which attaches as a shown Scope
//! lane's does and comes off when the capture ends.
//! Cost: nothing before the first call; then per event two empty channel drains and one flag per
//! attached capture. On the bench's thread an attached capture costs O(its channels) per
//! accepted point, as a Scope lane does.

use super::*;
use fab_elec_bench::source::Quantity;
use fab_elec_circuit::NetKind;
use fab_elec_design::action::ActionDef;
use fab_elec_design::{BenchOp, ProbeQuantity};
use fab_elec_view::instruments::acquire::SimIntake;
use fab_elec_bench::expect::{CheckState, Verdict};
use fab_elec_view::instruments::bench_setup::expect_probe;
use fab_elec_view::instruments::tools::{CallId, ExpectRow, ExpectSnapshot, HostRequest, ProbeInfo, ProbeSnapshot, ScopeTools, Site, ToolCall, ToolReply};
use fab_elec_view::live::sim::SinkSlot;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::LazyLock;

/// The Scope's tools this app runs.
const TOOLS: &[&str] = &[
    "elec.capture.save",
    "elec.capture.open",
    "elec.capture.close",
    "elec.source.list", "elec.probe.add", "elec.probe.remove", "elec.expect.list", "elec.expect.run",
    "elec.capture.plan", "elec.capture.run", "elec.capture.wait", "elec.capture.list", "elec.capture.drop",
    "elec.capture.summary", "elec.capture.samples", "elec.measure", "elec.capture.edges", "elec.capture.pulses",
    "elec.capture.decode", "elec.capture.compare",
];

/// Their catalog entries as the Scope's tools define them (`chat::register_elec_actions`).
pub static SCOPE_ACTIONS: LazyLock<Vec<ActionDef>> = LazyLock::new(|| {
    fab_elec_view::instruments::tools::actions::ACTIONS.iter().filter(|def| TOOLS.contains(&def.name)).copied().collect()
});

/// The longest answer a tool writes, in bytes; a call's `budget` asks for less.
const REPLY_BYTES: usize = 16 * 1024;

/// A capture's sink id on the bench (`SimAction::ShowScope`): this tag and a count, apart from
/// the views' ids.
const CAPTURE_SINK: u64 = 0x5343_4150_0000_0000;


pub(super) fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// The folder saved traces go under: the saved project's own folder, else (never saved, or a
/// demo) the documents folder, as exports do.
pub(super) fn traces_root(project: Option<&std::path::Path>) -> std::path::PathBuf {
    project.and_then(std::path::Path::parent).map_or_else(crate::model_host::documents_dir, std::path::Path::to_path_buf)
}

/// A trace's file: `path` (`captures/<name>.trace`) under `root`, refused when it would leave it.
fn trace_file(root: &std::path::Path, path: &str) -> Result<std::path::PathBuf, String> {
    use std::path::{Component, Path};
    let rel = Path::new(path);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) || rel.extension().is_none_or(|ext| ext != "trace") {
        return Err(format!("{path}: a trace's path is a .trace name within the project's captures"));
    }
    Ok(root.join(rel))
}

/// A trace's bytes at `path` (`captures/<name>.trace`, from `trace_path`) under `root`, written
/// whole and then moved over the name, so a reader never meets half a trace. O(bytes).
fn write_trace(root: &std::path::Path, path: &str, bytes: &[u8]) -> Result<std::path::PathBuf, String> {
    let file = trace_file(root, path)?;
    let folder = file.parent().expect("a name within a folder");
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let part = file.with_extension("trace.part");
    std::fs::write(&part, bytes).map_err(|e| format!("{}: {e}", part.display()))?;
    std::fs::rename(&part, &file).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file)
}

/// The writer the tools worker and the take's save share: into the project's folder as it is
/// when the trace is written (`ScopeDesk::project`, a brief lock), answering the project-relative
/// path.
fn writer(project: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>) -> fab_elec_view::instruments::tools::SaveWriter {
    Arc::new(move |path: &str, bytes: &[u8]| {
        let root = traces_root(project.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_deref());
        write_trace(&root, path, bytes).map(|_| path.to_string())
    })
}

/// The saved traces in `root`'s captures, project-relative, newest first: one listing of the
/// folder.
fn saved_traces(root: &std::path::Path) -> Vec<String> {
    let mut traces: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(root.join("captures")).into_iter().flatten().flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.strip_suffix(".trace").filter(|stem| !stem.is_empty())?.to_string();
            let modified = entry.metadata().and_then(|meta| meta.modified()).unwrap_or(std::time::UNIX_EPOCH);
            Some((modified, format!("captures/{name}.trace")))
        })
        .collect();
    traces.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    traces.into_iter().map(|(_, path)| path).collect()
}

/// What a trace job on the pool came back with.
pub(super) enum TraceDone {
    /// A take saved: its project-relative path, and the traces now saved.
    Saved(Result<String, String>, Vec<String>),
    Listed(Vec<String>),
}

/// The reader the tools worker opens saved traces with (`elec.capture.open`): from the same
/// folder the writer writes, as the project is when the trace is read; a missing one is answered
/// with the traces saved there.
fn reader(project: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>) -> fab_elec_view::instruments::tools::TraceReader {
    Arc::new(move |path: &str| {
        let root = traces_root(project.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_deref());
        std::fs::read(trace_file(&root, path)?).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => format!("{path}: no such saved trace; saved: {}", saved_traces(&root).join(", ")),
            _ => format!("{path}: {error}"),
        })
    })
}

/// The first `take-N` with no trace of that name in `root`'s captures: one listing of the folder.
fn next_take(root: &std::path::Path) -> String {
    let taken = std::fs::read_dir(root.join("captures")).into_iter().flatten().flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_prefix("take-")?.strip_suffix(".trace")?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("take-{}", taken + 1)
}

/// A probe edit the engine makes for the worker, which hears where its statement is: an added
/// probe's once the shown snapshot has it, a removed probe's where it was (`was`, found before).
pub(super) struct ScopeEdit {
    reply: Sender<Result<Site, String>>,
    bench: String,
    removed: bool,
    was: Option<Site>,
}

impl ScopeEdit {
    pub(super) fn refuse(self, why: String) {
        let _ = self.reply.send(Err(why));
    }
}

/// The worker and what it is doing.
#[derive(Default)]
pub(super) struct ScopeDesk {
    tools: Option<ScopeTools>,
    /// The calls on the worker, oldest first: its id, the caller's call, who asked and the tool.
    calls: VecDeque<(CallId, Option<u64>, By, &'static str)>,
    /// Probe edits waiting for the engine to take an edit, with the call that asked.
    edits: VecDeque<(CallId, BenchOp, Sender<Result<Site, String>>)>,
    /// Added probes the engine made, waiting for the shown snapshot to have them: the call that
    /// asked, the step, what it wrote and the edit.
    written: Vec<(CallId, u64, Vec<Sel>, ScopeEdit)>,
    /// Attached captures: their sink id and their end flag.
    pub(super) sinks: Vec<(u64, Arc<AtomicBool>)>,
    next_sink: u64,
    /// The saved project's file the traces go beside (`ElecHost::set_project`), read by the
    /// writer when it writes.
    pub(super) project: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    /// The trace jobs on the pool (a take's save, a listing), each with the call to answer
    /// (None: a person's).
    jobs: Vec<(Option<u64>, ToUIOneshotReceiver<TraceDone>)>,
    /// The project changed (`ElecHost::set_project`): its traces are no longer the ones shown.
    pub(super) left_project: bool,
}

impl ScopeDesk {
    /// A call is on the worker: the queue waits for its answer.
    pub(super) fn busy(&self) -> bool {
        !self.calls.is_empty()
    }

    pub(super) fn started(&self) -> bool {
        self.tools.is_some() || !self.jobs.is_empty() || self.left_project
    }
}

impl ElecHost {
    /// Hand a Scope tool's call to the worker, which starts with the first.
    pub(super) fn scope_call(&mut self, cx: &mut Cx, held: ActionCall) {
        if self.scope.tools.is_none() {
            match ScopeTools::new(cx, REPLY_BYTES, writer(self.scope.project.clone()), reader(self.scope.project.clone())) {
                Ok(tools) => self.scope.tools = Some(tools),
                Err(error) => return self.answer(held.call, Err(format!("the Scope's tools did not start: {error:?}"))),
            }
        }
        let tools = self.scope.tools.as_mut().expect("started above");
        let id = tools.call(ToolCall { tool: held.name.to_string(), args: held.args });
        self.scope.calls.push_back((id, held.call, held.by, held.name));
    }

    /// The worker's asks and answers, and the captures that ended come off the bench.
    pub(super) fn scope_pump(&mut self, cx: &mut Cx) {
        self.pump_traces(cx);
        let Some(tools) = self.scope.tools.as_mut() else { return };
        let requests = tools.host_requests();
        let replies = tools.poll();
        for request in requests {
            self.scope_request(cx, request);
        }
        for (id, reply) in replies {
            let Some(at) = self.scope.calls.iter().position(|(call, ..)| *call == id) else { continue };
            let (_, call, by, tool) = self.scope.calls.remove(at).expect("found above");
            // a person's press that failed (a saved trace in the Scope menu) says why in the Scope
            if let (By::Person, ToolReply::Failed(why)) = (by, &reply) {
                self.trace_notice(cx, Some(format!("{}: {why}", if tool == "elec.capture.open" { "Trace not opened" } else { "Not done" })));
            }
            self.answer(call, match reply {
                ToolReply::Ok(text) => Ok(text),
                ToolReply::Failed(why) => Err(why),
            });
        }
        self.scope_edits(cx);
        self.scope_written();
        self.scope.sinks.retain(|(id, ended)| {
            let ended = ended.load(Ordering::Acquire);
            if ended {
                cx.action(SimAction::HideScope { id: *id });
            }
            !ended
        });
    }

    fn scope_request(&mut self, cx: &mut Cx, request: HostRequest) {
        match request {
            HostRequest::Probes(reply) => {
                let _ = reply.send(self.scope_probes(cx));
            }
            // The worker waits on one call's asks at a time: the oldest call on it asked.
            HostRequest::Edit(op, reply) => match self.scope.calls.front() {
                Some(&(asked, ..)) => self.scope.edits.push_back((asked, op, reply)),
                None => {
                    let _ = reply.send(Err("no Scope call asked for this edit".into()));
                }
            },
            // A capture records the run as a shown Scope lane does: its want makes the bench and
            // plays it when there is none, and a paused run stays paused.
            HostRequest::SimCapture { channels, intake, ended, reply } => {
                self.scope.next_sink += 1;
                let id = CAPTURE_SINK | self.scope.next_sink;
                let sink = SimIntake::new(channels, intake);
                cx.action(SimAction::ShowScope { id, sink: SinkSlot::new(Box::new(sink)), column: 0.0 });
                self.scope.sinks.push((id, ended));
                let _ = reply.send(Ok(()));
            }
            HostRequest::Expects(reply) => {
                let _ = reply.send(self.scope_expects(cx));
            }
            // the transport's power cycle, as a person presses it (Stop, then Play): the next run is
            // the one started after the runs so far
            // an opened trace (`elec.capture.open`) in the Scope tab in place of the run
            HostRequest::Show(opened) => {
                if cx.has_global::<ElecLive>() {
                    cx.get_global::<ElecLive>().opened = Some(opened);
                }
                self.trace_notice(cx, None);
            }
            HostRequest::Restart(reply) => {
                let run = self.sim.as_ref().map_or(0, |sim| sim.runs()) + 1;
                cx.action(SimAction::Cmd(SimCmd::Stop));
                cx.action(SimAction::Cmd(SimCmd::Play));
                let _ = reply.send(Ok(run));
            }
        }
    }

    /// The bench the run follows, its circuit's nets and its probes, as its Scope lanes read
    /// them: a voltage probe's lane is its place among the bench's probes. A ground net is the
    /// run's `0` and no target. O(probes + nets).
    fn scope_probes(&self, cx: &mut Cx) -> Result<ProbeSnapshot, String> {
        let snap = self.driver.snap().ok_or("the design is still loading")?;
        let choice = cx.get_global_ref::<ElecLive>().and_then(|live| live.chosen_bench.clone());
        let bench = fab_elec_view::snapshot::run_bench(&snap.benches, choice.as_deref())
            .ok_or("no bench runs: the design has no bench, or its bench does not build")?;
        let circuit = bench.out.circuit(&snap.design).ok_or("the bench's circuit is not built")?;
        let ground = |net: &str| circuit.net(net).is_some_and(|net| net.kind == NetKind::Ground);
        let volts = |id: String, label: String, target: String, lane: usize| ProbeInfo {
            id, label, target, quantity: Quantity::Volt, unit: "V".into(), lane, range: None,
        };
        let probes = bench.out.probes.iter().enumerate()
            .filter(|(_, probe)| probe.of.quantity == ProbeQuantity::V && probe.of.reference.is_none())
            .map(|(lane, probe)| {
                let target = if ground(&probe.of.target) { "0".to_string() } else { probe.of.target.clone() };
                volts(format!("probe:{}", probe.of.target), probe.label.clone(), target, lane)
            })
            .collect();
        let targets = circuit.nets.iter().filter(|net| net.kind != NetKind::Ground)
            .map(|net| volts(String::new(), net.name.clone(), net.name.clone(), 0))
            .collect();
        let max_step = self.sim.as_ref().map_or_else(|| fab_elec_sim::livesim::LiveSpec::default().max_step, |sim| sim.live_spec().max_step);
        Ok(ProbeSnapshot { probes, targets, max_step, firmware: None })
    }

    /// The running bench's expectations with the judge's verdicts on the run, as the Bench editor
    /// shows them (`fab_elec_bench::judge`): pending until the judge has these expectations.
    /// O(expectations), and O(the bench's text) for each statement's line.
    fn scope_expects(&self, cx: &mut Cx) -> Result<ExpectSnapshot, String> {
        let snap = self.driver.snap().ok_or("the design is still loading")?;
        let choice = cx.get_global_ref::<ElecLive>().and_then(|live| live.chosen_bench.clone());
        let bench = fab_elec_view::snapshot::run_bench(&snap.benches, choice.as_deref())
            .ok_or("no bench runs: the design has no bench, or its bench does not build")?;
        let circuit = bench.out.circuit(&snap.design);
        let judged = self.sim.as_ref().map(|sim| sim.judged().lock().unwrap().clone()).unwrap_or_default();
        let states = (judged.expects == bench.out.expects).then_some(&judged.states);
        let rows = bench.out.expects.iter().enumerate().map(|(i, expect)| ExpectRow {
            name: expect.name.clone(),
            probe: expect_probe(&expect.probe, circuit),
            unit: expect.unit().into(),
            measure: expect.measure_name().into(),
            bound: expect.bound.clone(),
            when: expect.when,
            state: states.and_then(|states| states.get(i).cloned()).unwrap_or_else(|| CheckState {
                verdict: Verdict::Pending, done: false, value: f64::NAN, at: f64::NAN, failed: None, worst: 0.0, sel: Vec::new(),
            }),
            site: bench_site(snap, &bench.out.name, &fab_elec_lang::BenchKey::Expect(expect.name.clone()))
                .unwrap_or(Site { file: String::new(), line: 0 }),
        }).collect();
        let running = self.sim.as_ref().is_some_and(|sim| sim.advancing());
        Ok(ExpectSnapshot { run: judged.run, t: judged.t, running, rows })
    }

    /// The worker's probe edits, once the engine takes edits: each is the bench's own
    /// `elec.bench.probe` step, by the call that asked for it; one whose call has ended (the
    /// worker stopped waiting for it) is dropped, never made late. A net the bench probes already
    /// is refused: `elec.bench.probe` names its lane.
    fn scope_edits(&mut self, cx: &mut Cx) {
        while !self.scope.edits.is_empty() && self.settled && self.outside.is_none() && !self.driver.previewing() {
            let (asked, op, reply) = self.scope.edits.pop_front().expect("checked above");
            let Some(&(_, _, by, _)) = self.scope.calls.iter().find(|(call, ..)| *call == asked) else { continue };
            let Some(snap) = self.driver.snap() else {
                let _ = reply.send(Err("the design is still loading".into()));
                continue;
            };
            let choice = cx.get_global_ref::<ElecLive>().and_then(|live| live.chosen_bench.clone());
            let Some(bench) = fab_elec_view::snapshot::run_bench(&snap.benches, choice.as_deref()) else {
                let _ = reply.send(Err("no bench runs".into()));
                continue;
            };
            let (of, more, removed) = match op {
                BenchOp::AddProbe { of, label } => {
                    if let Some(probe) = bench.out.probes.iter().find(|probe| probe.of == of) {
                        let _ = reply.send(Err(format!("{} probes {} already, as {}: elec.bench.probe names its lane", bench.out.name, of.target, probe.label)));
                        continue;
                    }
                    (of, ("label", label), false)
                }
                BenchOp::RemoveProbe { of } => (of, ("remove", "true".to_string()), true),
                other => {
                    let _ = reply.send(Err(format!("the Scope's tools make no {other:?} edit")));
                    continue;
                }
            };
            let name = bench.out.name.clone();
            let was = if removed { probe_site(snap, &name, &of) } else { None };
            let args = vec![("net".to_string(), of.target.clone()), (more.0.to_string(), more.1), ("bench".to_string(), name.clone())];
            let (tx, rx) = std::sync::mpsc::channel();
            self.driver.invoke("elec.bench.probe".to_string(), args, false, by, tx);
            let edit = ScopeEdit { reply, bench: name, removed, was };
            self.engine_calls.push(EngineCall { call: None, name: "elec.bench.probe", rx, text: None, scope: Some(edit), before: None });
        }
    }

    /// A probe edit the engine made: a removed probe's answer is where it was; an added probe's,
    /// where the step wrote it, waits for the shown snapshot to have the step (`scope_written`).
    pub(super) fn scope_done(&mut self, seq: u64, sites: Vec<Sel>, edit: ScopeEdit) {
        if edit.removed {
            let _ = edit.reply.send(Ok(edit.was.unwrap_or_else(|| Site { file: String::new(), line: 0 })));
            return;
        }
        if let Some(&(asked, ..)) = self.scope.calls.front() {
            self.scope.written.push((asked, seq, sites, edit));
        }
    }

    /// The added probes the shown snapshot has: where each step wrote its statement (its first
    /// written range's document and line, from 1). One whose call has ended is dropped.
    /// O(waiting edits), and O(that document's text) for each answered.
    fn scope_written(&mut self) {
        if self.scope.written.is_empty() {
            return;
        }
        let Some(snap) = self.driver.snap().cloned() else { return };
        let shown = snap.last_commit.as_ref().map(|commit| commit.seq);
        let calls = &self.scope.calls;
        let texts = snap.docs.as_deref().map_or(&[][..], |docs| docs.texts.as_slice());
        self.scope.written.retain(|(asked, seq, sites, edit)| {
            if !calls.iter().any(|(call, ..)| call == asked) {
                return false;
            }
            if shown.is_none_or(|shown| shown < *seq) {
                return true;
            }
            let site = sites.iter().find_map(|sel| match sel {
                Sel::Source { doc, start, .. } => texts.iter().find(|text| text.doc == *doc)
                    .map(|text| Site { file: text.name.clone(), line: line_col(&text.text, *start as usize).0 }),
                _ => None,
            });
            let _ = edit.reply.send(site.ok_or_else(|| format!("{}: the step wrote no statement", edit.bench)));
            false
        });
    }
}

/// Where `bench` writes the probe `of`: its document and line, from 1. O(that document's text).
fn probe_site(snap: &DesignSnapshot, bench: &str, of: &fab_elec_design::ProbeOf) -> Option<Site> {
    bench_site(snap, bench, &fab_elec_lang::BenchKey::Probe(of.clone()))
}

/// Where `bench` writes the statement `key`: its document and line, from 1. O(that document's
/// text).
fn bench_site(snap: &DesignSnapshot, bench: &str, key: &fab_elec_lang::BenchKey) -> Option<Site> {
    let at = snap.source_map.as_deref()?.bench_site(bench, key)?;
    let text = snap.docs.as_deref()?.texts.iter().find(|text| text.doc == at.doc)?;
    Some(Site { file: text.name.clone(), line: line_col(&text.text, at.span.start as usize).0 })
}

/// The Scope's take as a trace: its lanes (the chosen bench's voltage probes, by their labels)
/// at their rows of the live record, the record whole as it is held. O(probes).
/// Each channel is its net, as a capture of the run names it (a ground net `0`), the probe's
/// label its display name.
fn take_of(live: &ElecLive) -> Result<(Arc<fab_elec_sim::trace::TraceStore>, fab_elec_bench::source::Layout, Vec<String>, Vec<usize>), String> {
    use fab_elec_bench::source::{ChanId, ChannelInfo, ClockKind, Layout};
    let index = &live.sim.index;
    let store = live.sim.store.clone().filter(|s| s.generation == index.generation && !s.is_empty())
        .ok_or("there is no take to save: play the bench first")?;
    let bench = live.run_bench().ok_or("no bench runs, so there is no take to save")?;
    let circuit = bench.out.circuit(live.snap.live()).ok_or("the bench's board has no circuit")?;
    let (mut channels, mut labels, mut rows) = (Vec::new(), Vec::new(), Vec::new());
    for probe in bench.out.probes.iter().filter(|p| p.of.quantity == ProbeQuantity::V && p.of.reference.is_none()) {
        let Some(net) = circuit.net(&probe.of.target) else { continue };
        let Some(row) = index.net(fab_elec_core::NetRef { board: circuit.unit, net: net.id }).filter(|&row| row < store.signals.len()) else { continue };
        let name = if net.kind == NetKind::Ground { "0".to_string() } else { probe.of.target.clone() };
        channels.push(ChannelInfo { id: ChanId(channels.len() as u32), name, quantity: Quantity::Volt });
        labels.push(probe.label.clone());
        rows.push(row);
    }
    if rows.is_empty() {
        return Err("the take has no lane to save: probe a net on the bench".into());
    }
    Ok((store, Layout { generation: index.generation, clock: ClockKind::Simulated, channels }, labels, rows))
}

impl ElecHost {
    /// `elec.capture.save` without a capture (the recording bar's Save trace): the Scope's take
    /// through the writer the tools' saves use, named `name` or the next free `take-N`.
    /// O(probes) here; the bytes (O(samples × lanes)) and the file on the pool.
    pub(super) fn save_take(&mut self, cx: &mut Cx, held: ActionCall) {
        use fab_elec_view::instruments::tools::capture::{trace_bytes, trace_path};
        let name = held.args.iter().find(|(key, _)| key == "name").map(|(_, value)| value.clone());
        let take = cx.get_global_ref::<ElecLive>().ok_or_else(|| "no bench runs, so there is no take to save".to_string()).and_then(take_of);
        let (store, layout, labels, rows) = match take {
            Ok(take) => take,
            Err(why) => return self.save_failed(cx, held.call, why),
        };
        let project = self.scope.project.clone();
        let write = writer(project.clone());
        let (tx, rx) = to_ui_oneshot();
        let job = move || {
            let root = traces_root(project.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_deref());
            let saved = trace_path(&name.unwrap_or_else(|| next_take(&root))).and_then(|path| {
                let bytes = trace_bytes(&layout, &labels, &store, &rows, store.time.at(0), &[], &[]);
                write(&path, &bytes)
            });
            let _ = tx.send(TraceDone::Saved(saved, saved_traces(&root)));
        };
        if let Err(why) = self.trace_job(cx, held.call, "elec trace save", rx, job) {
            self.save_failed(cx, held.call, why);
        }
    }

    /// `elec.capture.close` (the Scope's Back to live): the run in the Scope tab again. O(1).
    pub(super) fn close_trace(&mut self, cx: &mut Cx, held: ActionCall) {
        let closed = cx.has_global::<ElecLive>().then(|| cx.get_global::<ElecLive>().opened.take()).flatten();
        let closed = closed.map_or_else(|| fab_ai::out::S::Nil, |opened| fab_ai::out::s(&opened.path));
        self.answer(held.call, Ok(fab_ai::out::text(&fab_ai::out::obj(vec![("closed", closed)]))));
    }

    /// The Scope menu opened: its saved traces listed again on the pool.
    pub(super) fn list_traces(&mut self, cx: &mut Cx) {
        let project = self.scope.project.clone();
        let (tx, rx) = to_ui_oneshot();
        let job = move || {
            let root = traces_root(project.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_deref());
            let _ = tx.send(TraceDone::Listed(saved_traces(&root)));
        };
        // the menu keeps the list it has
        let _ = self.trace_job(cx, None, "elec trace list", rx, job);
    }

    fn trace_job(&mut self, cx: &mut Cx, call: Option<u64>, name: &'static str, rx: ToUIOneshotReceiver<TraceDone>, job: impl FnOnce() + Send + 'static) -> Result<(), String> {
        let task = cx.task_pool().submit_named(Class::Interactive, name, job).map_err(|error| format!("{name} could not start: {error}"))?;
        task.detach();
        self.scope.jobs.push((call, rx));
        Ok(())
    }

    /// The trace jobs that came back: the take's path and the traces listed (the saved take shows
    /// in the Scope menu: no line says it was saved), and why a person's save failed, in the
    /// Scope. A project left drops what was shown of the old one.
    fn pump_traces(&mut self, cx: &mut Cx) {
        let live = cx.has_global::<ElecLive>();
        if std::mem::take(&mut self.scope.left_project) && live {
            let shown = cx.get_global::<ElecLive>();
            (shown.opened, shown.traces) = (None, Arc::default());
            self.trace_notice(cx, None);
        }
        let mut at = 0;
        while at < self.scope.jobs.len() {
            let done = match self.scope.jobs[at].1.try_recv() {
                Ok(done) => done,
                Err(TryRecvError::Empty) => {
                    at += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => TraceDone::Saved(Err("the trace job ended without a word".into()), Vec::new()),
            };
            let (call, _) = self.scope.jobs.remove(at);
            match done {
                TraceDone::Saved(Ok(path), traces) => {
                    if live { cx.get_global::<ElecLive>().traces = Arc::new(traces); }
                    if call.is_none() { self.trace_notice(cx, None); }
                    self.answer(call, Ok(fab_ai::out::text(&fab_ai::out::obj(vec![("path", fab_ai::out::s(&path))]))));
                }
                TraceDone::Saved(Err(why), _) => self.save_failed(cx, call, why),
                TraceDone::Listed(traces) => {
                    if live { cx.get_global::<ElecLive>().traces = Arc::new(traces); }
                }
            }
        }
    }

    /// A save that failed: the AI's call hears why, a person's press (None) reads it in the Scope.
    fn save_failed(&mut self, cx: &mut Cx, call: Option<u64>, why: String) {
        if call.is_none() { self.trace_notice(cx, Some(format!("Trace not saved: {why}"))); }
        self.answer(call, Err(why));
    }

    /// A failed trace press says why in the Scope. A successful one clears its notice.
    fn trace_notice(&mut self, _cx: &mut Cx, why: Option<String>) {
        self.notices.push((NoticeView::Scope, why));
    }
}

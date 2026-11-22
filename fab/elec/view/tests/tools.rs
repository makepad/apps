use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use fab_elec_bench::expect::{Bound, CheckState, Verdict, When};
use fab_elec_bench::source::{ClockKind, Intake, Layout, Quantity};
use fab_elec_design::{BenchOp, ProbeOf, action::{check_returns, Risk}};
use fab_elec_lang::units::circuit_units;
use fab_elec_view::instruments::fw_events::{self, FwEvent, FwEvents, FwKind, FwSource, ScriptedFwEvents};
use fab_elec_view::instruments::tools::{ExpectRow, ExpectSnapshot, HostRequest, ProbeInfo, ProbeSnapshot, ScopeTools, Site, ToolCall, ToolReply};
use fab_elec_view::instruments::tools::actions::ACTIONS;
use fab_elec_view::instruments::tools::call::{Arguments, parse_field};
use makepad_widgets_core::{Cx, makepad_script::syntax::{self, Node}};

static SAVED: Mutex<Option<(String, Vec<u8>)>> = Mutex::new(None);
/// Every trace the writer took, by path: what the reader reads.
static FILES: Mutex<Vec<(String, Vec<u8>)>> = Mutex::new(Vec::new());
/// The clock of a host made with `Host::moving`: an origin plus the time that host lets pass, so
/// a deadline falls after the host reads the test needs, however slowly the box runs.
static PASSED: AtomicU64 = AtomicU64::new(0);
fn moved_now() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now) + Duration::from_nanos(PASSED.load(Ordering::Acquire))
}
/// What a moving host lets pass per answered expectation read and per loop while it holds one.
const STEP: Duration = Duration::from_millis(5);
fn pass() { PASSED.fetch_add(STEP.as_nanos() as u64, Ordering::AcqRel); }
fn save(path: &str, bytes: &[u8]) -> Result<String, String> {
    *SAVED.lock().unwrap() = Some((path.into(), bytes.to_vec()));
    FILES.lock().unwrap().push((path.into(), bytes.to_vec())); Ok(path.into())
}
fn read(path: &str) -> Result<Vec<u8>, String> {
    FILES.lock().unwrap().iter().rev().find(|(at, _)| at == path).map(|(_, bytes)| bytes.clone()).ok_or_else(|| format!("{path}: no such saved trace"))
}
fn call(tool: &str, args: &[(&str, &str)]) -> ToolCall {
    ToolCall { tool: tool.into(), args: args.iter().map(|(name, value)| ((*name).into(), (*value).into())).collect() }
}
fn object(reply: ToolReply, budget: usize) -> Node {
    let ToolReply::Ok(text) = reply else { panic!("expected a successful object: {reply:?}") };
    assert!(text.len() <= budget, "the complete UTF-8 reply fits its byte budget");
    assert!(text.starts_with('{') && text.ends_with('}'), "success is exactly one Splash object");
    let value = syntax::parse_value_with(&text, &circuit_units()).expect("reply is a pasteable Splash literal");
    assert!(value.object().is_some(), "reply parses as an object");
    value
}
fn field<'a>(node: &'a Node, name: &str) -> &'a Node {
    &node.field(name).unwrap_or_else(|| panic!("reply has field {name}")).value
}
fn string(node: &Node, name: &str) -> String { field(node, name).string().expect("field is text").into() }
fn refused(reply: ToolReply, reason: &str) {
    let ToolReply::Failed(text) = reply else { panic!("expected a refusal: {reply:?}") };
    assert!(!text.contains(['\n', '\r']), "refusals are one line");
    assert!(text.contains(reason), "refusal names the offending field or reason: {text}");
}
fn probe() -> ProbeInfo {
    ProbeInfo { id: "probe:VOUT".into(), label: "VOUT".into(), target: "VOUT".into(), quantity: Quantity::Volt,
        unit: "V".into(), lane: 0, range: Some((0.0, 1.0)) }
}
struct Host {
    tools: ScopeTools,
    _cx: Cx,
    points: Vec<(f64, f32)>,
    firmware: Option<Arc<dyn FwEvents>>,
    defer: bool,
    held: Option<Box<dyn Intake>>,
    /// The opened traces the tools asked the Scope to show, by path.
    shown: Vec<String>,
    /// Each simulator capture's end flag, in attach order.
    ended: Vec<Arc<std::sync::atomic::AtomicBool>>,
    edits: Vec<BenchOp>,
    expects: ExpectSnapshot,
    expect_runs: VecDeque<VecDeque<ExpectSnapshot>>,
    expect_steps: VecDeque<ExpectSnapshot>,
    expect_reads: Vec<Instant>,
    expect_defer_after: Option<usize>,
    expect_held: Option<std::sync::mpsc::Sender<Result<ExpectSnapshot, String>>>,
    restarts: usize,
    /// The tools' clock; `moving`: it is `moved_now`, which only this host moves.
    now: fn() -> Instant,
    moving: bool,
}
impl Host {
    fn new(cap: usize) -> Self {
        Self::with_clock(cap, Instant::now, false)
    }
    /// A host whose time passes only as it answers expectation reads, or holds one.
    fn moving(cap: usize) -> Self {
        Self::with_clock(cap, moved_now, true)
    }
    fn with_clock(cap: usize, now: fn() -> Instant, moving: bool) -> Self {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let tools = ScopeTools::with_clock(&mut cx, cap, Arc::new(save), Arc::new(read), now).expect("platform tools worker starts");
        Self { tools, _cx: cx, points: vec![(0.0, 0.0), (0.01, 1.0)], firmware: None, defer: false, held: None, shown: Vec::new(), ended: Vec::new(), edits: Vec::new(),
            expects: ExpectSnapshot { run: 0, t: 0.0, running: false, rows: Vec::new() },
            expect_runs: VecDeque::new(), expect_steps: VecDeque::new(), expect_reads: Vec::new(),
            expect_defer_after: None, expect_held: None, restarts: 0, now, moving }
    }
    fn answer(&mut self) {
        for request in self.tools.host_requests() {
            match request {
                HostRequest::Expects(reply) => {
                    self.expect_reads.push((self.now)());
                    if self.expect_defer_after.is_some_and(|count| self.expect_reads.len() > count) {
                        assert!(self.expect_held.is_none(), "at most one expectation snapshot is outstanding");
                        self.expect_held = Some(reply);
                    } else {
                        let snapshot = self.expect_steps.pop_front().unwrap_or_else(|| self.expects.clone());
                        if snapshot.run == self.expects.run { self.expects = snapshot.clone(); }
                        if self.moving { pass(); }
                        // A deadline may expire between the host drain and its nonblocking reply.
                        let _ = reply.send(Ok(snapshot));
                    }
                }
                HostRequest::Restart(reply) => {
                    self.restarts += 1; self.expects.run += 1; self.expects.t = 0.0; self.expects.running = true;
                    for row in &mut self.expects.rows { row.state = expect_pending(); }
                    self.expect_steps = self.expect_runs.pop_front().unwrap_or_default();
                    let _ = reply.send(Ok(self.expects.run));
                }
                HostRequest::Probes(reply) => {
                    reply.send(Ok(ProbeSnapshot { probes: vec![probe()], targets: vec![probe()], max_step: 1e-6, firmware: self.firmware.clone() })).unwrap();
                }
                HostRequest::Show(opened) => self.shown.push(opened.path.clone()),
                HostRequest::Edit(op, reply) => {
                    self.edits.push(op);
                    reply.send(Ok(Site { file: "main.splash".into(), line: 12 })).unwrap();
                }
                HostRequest::SimCapture { channels, mut intake, ended, reply } => {
                    self.ended.push(ended);
                    intake.begin(&Layout { generation: 1, clock: ClockKind::Simulated, channels: channels.into_iter().map(|c| c.info).collect() });
                    if self.defer {
                        intake.point(0.0, &[0.0]); self.held = Some(intake);
                    } else {
                        for &(t, y) in &self.points { intake.point(t, &[y]); }
                        intake.idle();
                    }
                    reply.send(Ok(())).unwrap();
                }
            }
        }
    }
    fn request(&mut self, tool: &str, args: &[(&str, &str)]) -> ToolReply {
        let id = self.tools.call(call(tool, args));
        // No deadline: a tool that never answers shows up as a hang.
        loop {
            self.answer();
            if self.moving && self.expect_held.is_some() { pass(); }
            for (got, reply) in self.tools.poll() {
                assert_eq!(got, id, "test host receives the requested invocation id"); return reply;
            }
            std::thread::yield_now();
        }
    }
    /// The capture's final status: a waiting reply is waited on again, with no deadline.
    fn done(&mut self, reply: ToolReply) -> Node {
        let mut node = object(reply, 4096);
        while field(&node, "state").name() == Some("waiting") {
            let id = string(&node, "capture");
            let reply = self.request("elec.capture.wait", &[("capture", &id)]);
            node = object(reply, 4096);
        }
        node
    }
}

#[test]
fn catalog_defaults_and_argument_refusals_are_checked() {
    fab_elec_view::instruments::tools::actions::register_actions();
    assert_eq!(ACTIONS.len(), 20, "all twenty scope tools are catalog commands");
    for action in ACTIONS {
        check_returns(action.returns).unwrap_or_else(|e| panic!("{} result shape: {e}", action.name));
        assert_eq!(action.group, "Scope", "every tool belongs to the scope palette group");
        for field in action.fields {
            if !field.default.is_empty() { assert!(parse_field(field, field.default).is_ok(), "{}.{} default parses against its kind", action.name, field.name); }
        }
    }
    for name in ["elec.capture.list", "elec.capture.summary"] {
        let action = ACTIONS.iter().find(|action| action.name == name).unwrap();
        assert!(action.returns.contains("side: choice sim|real|replay"), "{name} declares the capture side choices");
    }
    for name in ["elec.expect.list", "elec.expect.run"] {
        let action = ACTIONS.iter().find(|action| action.name == name).unwrap();
        assert_eq!(action.risk, if name == "elec.expect.run" { Risk::Run } else { Risk::Read }, "risk for {name}");
        assert!(action.returns.contains("first_failure: {..}"), "{name} declares retained failure evidence");
    }
    for (tool, args, field) in [
        ("elec.capture.run", vec![("bogus", "1")], "bogus"),
        ("elec.capture.plan", vec![("rate", "3V")], "rate"),
        ("elec.probe.add", vec![], "target"),
        ("elec.capture.samples", vec![("capture", "cap1"), ("points", "0")], "points"),
        ("elec.capture.run", vec![("source", "sim"), ("source", "synth")], "source"),
        ("elec.expect.run", vec![("repeats", "0")], "repeats"),
        ("elec.expect.list", vec![("on", "replay")], "on"),
        ("elec.expect.list", vec![("limit", "0")], "limit"),
    ] {
        let error = Arguments::parse(&call(tool, &args)).unwrap_err();
        assert!(error.contains(field), "invalid argument names its field: {error}");
    }
    let args = Arguments::parse(&call("elec.capture.run", &[("rate", "12.4kHz"), ("from", "-2ms"), ("on", "\"VOUT\""), ("slope", "rise")])).unwrap();
    assert_eq!(args.num("rate"), Some(12400.0), "numeric suffix converts to exact SI");
    assert_eq!(args.num("from"), Some(-0.002), "negative time literal is accepted");
    assert_eq!(args.text("on"), Some("VOUT"), "quoted text is decoded");
    assert_eq!(args.text("slope"), Some("rise"), "bare choices are accepted");
}

fn expect_pending() -> CheckState {
    CheckState { verdict: Verdict::Pending, done: false, value: f64::NAN, at: f64::NAN, failed: None, worst: 0.0, sel: Vec::new() }
}
fn expect_snapshot(run: u64, failed: bool) -> ExpectSnapshot {
    ExpectSnapshot { run, t: 0.01, running: true, rows: vec![
        ExpectRow { name: "rail".into(), probe: "v(VOUT)".into(), unit: "V".into(), measure: "mean".into(),
            bound: Bound::Range { min: 0.9, max: 1.1 }, when: When::Span { from: 0.0, to: 0.01 },
            state: CheckState { verdict: Verdict::Pass, done: true, value: 1.0, at: 0.01, failed: None, worst: 0.0, sel: Vec::new() },
            site: Site { file: "main.splash".into(), line: 20 } },
        ExpectRow { name: "clock".into(), probe: "v(CLK)".into(), unit: "Hz".into(), measure: "freq".into(),
            bound: Bound::Range { min: 900.0, max: 1100.0 }, when: When::Trailing { window: 0.005, after: 0.0 },
            state: CheckState { verdict: Verdict::Pass, done: false, value: 1000.0, at: 0.01,
                failed: failed.then_some((0.006, 1500.0)), worst: if failed { 400.0 } else { 0.0 }, sel: Vec::new() },
            site: Site { file: "main.splash".into(), line: 21 } },
    ] }
}
fn expect_items(answer: &Node) -> &[Node] { field(answer, "items").items().unwrap() }
fn expect_counts(row: &Node, pass: f64, fail: f64, not_judged: f64) {
    for (name, count) in [("pass", pass), ("fail", fail), ("not_judged", not_judged)] {
        assert_eq!(field(field(row, "repeats"), name).number(), Some(count), "finished repeats count {name}");
    }
}

#[test]
fn expectations_list_filters_sides_retained_failures_and_snapshot_pages() {
    let mut host = Host::new(65536); host.expects = expect_snapshot(7, true);
    let answer = object(host.request("elec.expect.list", &[]), 4096);
    assert_eq!(field(&answer, "run").number(), Some(7.0), "list carries the live run id");
    assert_eq!(field(&answer, "t").number(), Some(0.01), "list carries live simulation time");
    assert_eq!(field(&answer, "running").ident(), Some("true"), "list carries transport state");
    assert_eq!(field(&answer, "total").number(), Some(2.0), "both expectations are listed");
    assert!(answer.field("next").is_none(), "complete list has no cursor");
    let rows = expect_items(&answer); assert_eq!(rows.len(), 2, "both whole rows fit the default budget");
    assert_eq!(string(&rows[0], "name"), "rail");
    assert_eq!(string(&rows[0], "probe"), "v(VOUT)");
    assert_eq!(string(&rows[0], "unit"), "V");
    assert_eq!(string(&rows[0], "measure"), "mean");
    assert_eq!(field(&rows[0], "min").number(), Some(0.9));
    assert_eq!(field(&rows[0], "max").number(), Some(1.1));
    assert_eq!(field(field(field(&rows[0], "when"), "span"), "to").number(), Some(0.01));
    assert_eq!(field(field(field(&rows[1], "when"), "trailing"), "window").number(), Some(0.005));
    assert_eq!(string(field(&rows[0], "site"), "file"), "main.splash");
    assert_eq!(field(field(&rows[0], "site"), "line").number(), Some(20.0));
    assert_eq!(string(field(&rows[0], "sim"), "outcome"), "pass");
    assert_eq!(string(field(&rows[1], "sim"), "outcome"), "fail", "earlier failure wins despite the latest passing judgment");
    assert_eq!(field(field(&rows[1], "sim"), "value").number(), Some(1000.0));
    assert_eq!(field(field(&rows[1], "sim"), "at").number(), Some(0.01));
    assert_eq!(field(field(&rows[1], "sim"), "worst").number(), Some(400.0));
    let first = field(field(&rows[1], "sim"), "first_failure");
    assert_eq!(field(first, "t").number(), Some(0.006));
    assert_eq!(field(first, "value").number(), Some(1500.0));
    assert!(field(&rows[0], "sim").field("first_failure").is_none(), "passing check has no invented failure");
    let filtered = object(host.request("elec.expect.list", &[("names", " clock, clock ")]), 4096);
    assert_eq!(expect_items(&filtered).len(), 1, "trimmed repeated names select one row");
    assert_eq!(string(&expect_items(&filtered)[0], "name"), "clock");
    assert_eq!(field(&filtered, "total").number(), Some(1.0), "total follows the name filter");
    let unknown = host.request("elec.expect.list", &[("names", "missing")]);
    for reason in ["missing", "known names", "rail", "clock"] { refused(unknown.clone(), reason); }
    for on in ["real", "both"] {
        let answer = object(host.request("elec.expect.list", &[("on", on)]), 4096);
        assert_eq!(expect_items(&answer).len(), 2, "real placeholders retain every expectation");
        for row in expect_items(&answer) {
            assert_eq!(string(field(row, "real"), "outcome"), "not judged");
            assert_eq!(string(field(row, "real"), "reason"), "no real source is open");
            assert_eq!(row.field("sim").is_some(), on == "both", "only requested sides are present");
        }
    }
    let first = object(host.request("elec.expect.list", &[("limit", "1")]), 4096);
    let cursor = string(&first, "next"); let reads = host.expect_reads.len();
    host.expects.rows.clear(); host.expects.run += 1;
    let last = object(host.request("elec.expect.list", &[("limit", "1"), ("after", &cursor)]), 4096);
    assert_eq!(expect_items(&first).len(), 1); assert_eq!(expect_items(&last).len(), 1);
    assert_eq!(string(&expect_items(&first)[0], "name"), "rail");
    assert_eq!(string(&expect_items(&last)[0], "name"), "clock");
    assert_eq!(string(field(&expect_items(&last)[0], "sim"), "outcome"), "fail", "continuation preserves the old judgment");
    assert_eq!(field(&last, "run").number(), Some(7.0), "continuation retains the original run");
    assert_eq!(host.expect_reads.len(), reads, "continuation uses its immutable snapshot");
    assert!(last.field("next").is_none(), "last page ends the walk");
    refused(host.request("elec.expect.list", &[("after", &cursor), ("on", "both")]), "after");
    refused(host.request("elec.expect.list", &[("after", &cursor), ("names", "clock")]), "after");
    refused(host.request("elec.expect.list", &[("after", "invalid")]), "after");
    host.expects = expect_snapshot(8, false);
    for (row, bound) in host.expects.rows.iter_mut().zip([Bound::TextEquals("OK".into()), Bound::TextContains("ready".into())]) {
        row.bound = bound; row.measure = "uart".into(); row.unit.clear(); row.state = expect_pending();
    }
    let text = object(host.request("elec.expect.list", &[]), 4096);
    assert_eq!(string(&expect_items(&text)[0], "equals"), "OK");
    assert_eq!(string(&expect_items(&text)[1], "contains"), "ready");
    for row in expect_items(&text) {
        assert!(row.field("min").is_none() && row.field("max").is_none(), "text bounds omit numeric bounds");
        let sim = field(row, "sim"); assert_eq!(string(sim, "outcome"), "not judged");
        assert!(sim.field("value").is_none() && sim.field("at").is_none(), "pending NaNs are omitted");
    }
    // A budget too small for one row still answers, never refuses: each page carries one whole
    // row and says plainly how many are left, and its cursor walks every row in order.
    let mut walked = Vec::new(); let mut after = None::<String>;
    loop {
        let mut args = vec![("budget", "1")]; if let Some(after) = &after { args.push(("after", after)); }
        let page = object(host.request("elec.expect.list", &args), usize::MAX);
        let rows = expect_items(&page); assert_eq!(rows.len(), 1, "an over-budget page carries one whole row");
        walked.push(string(&rows[0], "name"));
        let Some(next) = page.field("next") else { assert!(page.field("more").is_none(), "the last page has no continuation"); break };
        assert_eq!(string(&page, "more"), format!("{} more; ask again with after: next", 2 - walked.len()), "the page says how many are left and how to ask");
        after = Some(next.value.string().unwrap().into());
    }
    assert_eq!(walked, ["rail", "clock"], "the one-row pages reach every row in order");
}

#[test]
fn expectations_run_restarts_counts_and_waits_for_the_requested_run() {
    let mut host = Host::new(65536); host.expects = expect_snapshot(0, true);
    let mut early = expect_snapshot(1, true); early.t = 0.007; early.rows[0].state.done = false;
    let mut stale = expect_snapshot(0, false); stale.running = false;
    let mut pending = expect_snapshot(2, false);
    for row in &mut pending.rows { row.state = expect_pending(); }
    host.expect_runs = VecDeque::from([
        VecDeque::from([stale, early, expect_snapshot(1, true)]),
        VecDeque::from([pending, expect_snapshot(2, false)]),
    ]);
    let answer = object(host.request("elec.expect.run", &[("repeats", "2")]), 4096);
    assert_eq!(host.restarts, 2, "each repeat requests its own restart");
    assert!(host.expect_runs.is_empty() && host.expect_steps.is_empty(), "span waits through early passes and stale stopped snapshots");
    assert_eq!(string(&answer, "state"), "done");
    assert_eq!(field(&answer, "run").number(), Some(2.0));
    assert_eq!(field(&answer, "total").number(), Some(2.0));
    let rows = expect_items(&answer); expect_counts(&rows[0], 2.0, 0.0, 0.0); expect_counts(&rows[1], 1.0, 1.0, 0.0);
    for row in rows {
        assert_eq!(field(row, "final").ident(), Some("true"));
        assert_eq!(string(field(row, "sim"), "outcome"), "pass", "row state comes from the last repeat");
    }
    assert_eq!(field(field(&rows[1], "sim"), "done").ident(), Some("false"), "trailing checks finish a repeat while remaining live");
    assert!(field(&rows[1], "sim").field("first_failure").is_none(), "last repeat did not fail");
    let failure = field(&rows[1], "first_failure");
    assert_eq!(field(failure, "repeat").number(), Some(1.0), "cross-repeat failure index is one-based");
    assert_eq!(field(failure, "t").number(), Some(0.006));
    assert_eq!(field(failure, "value").number(), Some(1500.0));
    assert!(rows[0].field("first_failure").is_none(), "always passing row has no cross-repeat failure");
    let reads = host.expect_reads.len();
    for on in ["real", "both"] { refused(host.request("elec.expect.run", &[("on", on)]), "no real source is open"); }
    refused(host.request("elec.expect.run", &[("timeout", "-1s")]), "timeout");
    assert_eq!(host.expect_reads.len(), reads, "unsupported sides and invalid timeout need no host requests");
    refused(host.request("elec.expect.run", &[("names", "missing")]), "known names");
    assert_eq!(host.restarts, 2, "refused calls never restart");
    let mut filtered = expect_snapshot(3, false); filtered.rows[1].state = expect_pending();
    host.expect_runs.push_back(VecDeque::from([filtered]));
    let answer = object(host.request("elec.expect.run", &[("names", "rail")]), 4096);
    assert_eq!(string(&answer, "state"), "done", "unselected pending checks cannot delay a run");
    assert_eq!(expect_items(&answer).len(), 1); expect_counts(&expect_items(&answer)[0], 1.0, 0.0, 0.0);
}

#[test]
fn expectations_run_finishes_unjudgeable_checks_and_stopped_runs() {
    let mut host = Host::new(65536); host.expects = expect_snapshot(0, false);
    let mut invalid = expect_snapshot(1, false);
    invalid.rows[0].state = expect_pending(); invalid.rows[0].state.done = true;
    host.expect_runs.push_back(VecDeque::from([invalid]));
    let answer = object(host.request("elec.expect.run", &[("names", "rail")]), 4096);
    assert_eq!(string(&answer, "state"), "done", "done pending span is final without a judgment");
    expect_counts(&expect_items(&answer)[0], 0.0, 0.0, 1.0);
    let mut stopped = expect_snapshot(2, false); stopped.running = false;
    for row in &mut stopped.rows { row.state = expect_pending(); }
    host.expect_runs.push_back(VecDeque::from([stopped]));
    let answer = object(host.request("elec.expect.run", &[]), 4096);
    assert_eq!(string(&answer, "state"), "done", "stopped simulation finalizes pending checks");
    for row in expect_items(&answer) {
        assert_eq!(field(row, "final").ident(), Some("true"));
        assert_eq!(string(field(row, "sim"), "outcome"), "not judged");
        expect_counts(row, 0.0, 0.0, 1.0);
    }
}

#[test]
fn expectations_run_deadline_stops_polling_and_bounds_host_replies() {
    let mut host = Host::moving(65536); host.expects = expect_snapshot(0, false);
    let mut pending = expect_snapshot(2, false); pending.rows[1].state = expect_pending();
    host.expect_runs = VecDeque::from([VecDeque::from([expect_snapshot(1, true)]), VecDeque::from([pending])]);
    let answer = object(host.request("elec.expect.run", &[("repeats", "2"), ("timeout", "0.2s")]), 4096);
    assert_eq!(string(&answer, "state"), "waiting", "never-final row returns at the whole-call timeout");
    assert_eq!(host.restarts, 2); assert_eq!(field(&answer, "run").number(), Some(2.0));
    let rows = expect_items(&answer);
    assert_eq!(field(&rows[0], "final").ident(), Some("true"));
    assert_eq!(field(&rows[1], "final").ident(), Some("false"));
    expect_counts(&rows[0], 1.0, 0.0, 0.0); expect_counts(&rows[1], 0.0, 1.0, 0.0);
    assert!(field(&rows[1], "sim").field("value").is_none(), "waiting row has no invented judgment");
    assert_eq!(field(field(&rows[1], "first_failure"), "repeat").number(), Some(1.0));
    let reads = host.expect_reads.len(); let restarts = host.restarts;
    let deadline = host.expect_reads[0] + Duration::from_millis(200);
    assert!(host.expect_reads.iter().all(|&at| at <= deadline), "host is not asked for snapshots after the deadline");
    object(host.request("elec.source.list", &[]), 4096);
    assert_eq!(host.expect_reads.len(), reads, "a later worker call sees no background expectation polling");
    let zero = object(host.request("elec.expect.run", &[("timeout", "0s")]), 4096);
    assert_eq!(string(&zero, "state"), "waiting");
    assert!(expect_items(&zero).is_empty() && zero.field("run").is_none(), "no snapshot means no invented run metadata");
    assert_eq!(host.expect_reads.len(), reads); assert_eq!(host.restarts, restarts, "zero bound issues no restart");
    host.expect_defer_after = Some(reads + 1);
    let held = object(host.request("elec.expect.run", &[("timeout", "0.02s")]), 4096);
    assert_eq!(string(&held, "state"), "waiting", "an unanswered host read obeys the same deadline");
    assert!(host.expect_held.take().unwrap().send(Ok(host.expects.clone())).is_err(), "timed-out host receiver is already gone");
    object(host.request("elec.source.list", &[]), 4096);
    assert_eq!(host.expect_reads.len(), reads + 2, "deadline leaves no further host reads queued");
}

#[test]
fn width_and_period_capture_bounds_resolve_to_software_width() {
    let mut host = Host::new(65536);
    host.points = (0..=400).map(|i| (i as f64 * 1e-5, if (10..30).contains(&(i % 100)) { 1.0 } else { 0.0 })).collect();
    for (kind, bound) in [("width", "0.3ms"), ("period", "1.1ms")] {
        let reply = host.request("elec.capture.run", &[("trigger", kind), ("max_interval", bound), ("slope", "fall"), ("to", "1ms")]);
        let done = host.done(reply);
        let trigger = field(field(&done, "resolved"), "trigger");
        assert_eq!(field(trigger, "kind").name(), Some("width"), "both interval kinds resolve as width");
        assert_eq!(field(trigger, "how").name(), Some("sw"));
        assert_eq!(field(&done, "state").name(), Some("done"));
        assert!(host.ended.last().expect("the capture attached").load(Ordering::Acquire), "a finished capture tells its host it takes no more points");
        let reply = host.request("elec.capture.run", &[("trigger", kind)]);
        let ToolReply::Failed(reason) = reply else { panic!("an unbounded qualified capture must be refused") };
        assert!(reason.contains("min_interval") && reason.contains("max_interval"), "refusal names both missing bounds: {reason}");
    }
}

#[test]
fn runt_capture_requires_a_second_level_and_retains_it_in_the_spec() {
    let mut host = Host::new(65536);
    host.points = (0..=400).map(|i| (i as f64 * 1e-5, if (10..30).contains(&(i % 100)) { if i < 100 { 1.0 } else { 0.75 } } else { 0.0 })).collect();
    refused(host.request("elec.capture.run", &[("trigger", "runt")]), "runt_level");
    refused(host.request("elec.capture.run", &[("trigger", "runt"), ("level", "0.5V"), ("runt_level", "0.2V")]), "runt_level");
    let reply = host.request("elec.capture.run", &[("trigger", "runt"), ("level", "0.5V"), ("runt_level", "0.9V"), ("to", "1ms")]);
    let done = host.done(reply);
    let trigger = field(field(&done, "resolved"), "trigger");
    assert_eq!(field(trigger, "kind").name(), Some("runt"), "the capture spec retains Qualifier::Runt rather than an interval qualifier");
    assert_eq!(field(trigger, "runt_level").number(), Some(0.9), "the spec's runt qualifier retains its second level in volts");
    assert_eq!(field(trigger, "how").name(), Some("sw"), "simulator runts resolve through the software detector");
    assert_eq!(field(&done, "state").name(), Some("done"), "a runt closes the capture");
    let at = field(&done, "t_trigger").number().unwrap();
    assert!((at - (129.0 + 1.0 / 3.0) * 1e-5).abs() < 1e-12, "the full pulse is skipped and the runt triggers at its return edge");
}

#[test]
fn host_edits_synth_capture_late_trigger_save_and_drop() {
    let mut host = Host::new(65536);
    let source = object(host.request("elec.source.list", &[]), 4096);
    assert_eq!(field(&source, "sources").items().unwrap().len(), 2, "simulator and synth are listed");
    refused(host.request("elec.probe.add", &[("target", "VOUT"), ("quantity", "@p")]), "power");
    refused(host.request("elec.probe.add", &[("target", "VOUT"), ("quantity", "@i")]), "branch-current");
    assert!(host.edits.is_empty(), "refused quantities never send a bench edit");
    let added = object(host.request("elec.probe.add", &[("target", "VOUT"), ("label", "rail")]), 4096);
    assert_eq!(host.edits, vec![BenchOp::AddProbe { of: ProbeOf::v("VOUT"), label: "rail".into() }], "a voltage probe sends exactly one undoable AddProbe");
    assert_eq!(string(field(&added, "site"), "file"), "main.splash", "probe reply carries the written file");
    assert_eq!(field(field(&added, "site"), "line").number(), Some(12.0), "probe reply carries the written line");
    let reply = host.request("elec.capture.run", &[("source", "synth"), ("to", "1ms")]);
    let done = host.done(reply);
    assert_eq!(field(&done, "state").name(), Some("done"), "unpaced synth completes through DeviceSource");
    assert!(field(&done, "samples").number().unwrap() > 100.0, "synth capture contains recorded samples");
    assert_eq!(field(field(&done, "resolved"), "rate").number(), Some(2e6), "reply includes the settings in force");
    let id = string(&done, "capture");
    let saved = object(host.request("elec.capture.save", &[("capture", &id), ("name", "reference")]), 4096);
    assert_eq!(string(&saved, "path"), "captures/reference.trace", "save uses a project-relative path");
    let saved = SAVED.lock().unwrap().take().unwrap();
    assert_eq!(saved.0, "captures/reference.trace", "host writer gets the requested path");
    assert_eq!(&saved.1[..8], b"FABTRACE", "host writer gets the documented binary format");
    let back = fab_elec_view::instruments::tools::capture::read_trace(&saved.1, 1).expect("the saved capture reads back");
    assert!(back.store.len() > 100 && !back.layout.channels.is_empty(), "with its channels and every sample");
    // The saved capture opens again as a capture of its own, which the Scope is asked to show:
    // it measures and compares as the capture it was saved from; opening the file again replaces
    // it, and a file that is not there is refused.
    let opened = object(host.request("elec.capture.open", &[("path", "captures/reference.trace")]), 4096);
    let reopened = string(&opened, "capture");
    host.answer();
    assert!(reopened != id && host.shown == ["captures/reference.trace"], "an opened trace is a capture of its own, shown in the Scope: {reopened}");
    let mean = |host: &mut Host, capture: &str| {
        let measured = object(host.request("elec.measure", &[("capture", capture), ("what", "mean")]), 4096);
        field(&field(&measured, "items").items().unwrap()[0], "value").number().unwrap()
    };
    let (was, again) = (mean(&mut host, &id), mean(&mut host, &reopened));
    assert!((was - again).abs() <= 1e-6 * was.abs().max(1.0), "the opened trace measures as its capture did: {again} against {was}");
    let compared = object(host.request("elec.capture.compare", &[("expected", &reopened), ("actual", &id)]), 4096);
    assert_eq!(field(&compared, "verdict").name(), Some("pass"), "the capture compared against its own saved trace passes");
    object(host.request("elec.capture.open", &[("path", "captures/reference.trace")]), 4096);
    refused(host.request("elec.capture.summary", &[("capture", &reopened)]), "unknown");
    refused(host.request("elec.capture.open", &[("path", "captures/missing.trace")]), "no such saved trace");
    refused(host.request("elec.capture.save", &[]), "capture");
    object(host.request("elec.capture.drop", &[("capture", &id)]), 4096);
    refused(host.request("elec.capture.wait", &[("capture", &id), ("timeout", "0s")]), "unknown");
    host.defer = true;
    let waiting = object(host.request("elec.capture.run", &[("trigger", "edge"), ("level", "0.5V"), ("to", "1ms"), ("timeout", "0s")]), 4096);
    assert_eq!(field(&waiting, "state").name(), Some("waiting"), "timeout leaves an untriggered capture waiting");
    let pending = string(&waiting, "capture");
    assert_eq!(pending, "cap4", "the next capture never reuses a dropped id (cap1 dropped, the opened trace's cap2 replaced by cap3)");
    let mut intake = host.held.take().unwrap();
    intake.point(0.001, &[1.0]); intake.point(0.002, &[1.0]); intake.idle();
    let reply = host.request("elec.capture.wait", &[("capture", &pending)]);
    let done = host.done(reply);
    assert_eq!(field(&done, "state").name(), Some("done"), "later crossing finishes the same pending capture");
    assert_eq!(field(&done, "t_trigger").number(), Some(0.0005), "trigger time uses the interpolated level crossing");
    refused(host.request("elec.capture.plan", &[("rate", "3V")]), "rate");
    refused(host.request("elec.capture.plan", &[("unknown", "1")]), "unknown");
    refused(host.request("elec.probe.remove", &[]), "probe");
    host.defer = false;
    host.points = vec![(0.0, 0.0), (0.0001, 1.0), (0.0004, 2.0), (0.001, 3.0)];
    let done = object(host.request("elec.capture.run", &[("trigger", "none"), ("to", "1ms")]), 4096);
    let id = string(&done, "capture");
    let first = object(host.request("elec.capture.samples", &[("capture", &id), ("reduce", "raw"), ("points", "2")]), 4096);
    assert!(first.field("dt").is_none(), "nonuniform raw samples provide times instead of an invented dt");
    let next = string(&first, "next");
    let second = object(host.request("elec.capture.samples", &[("capture", &id), ("reduce", "raw"), ("points", "2"), ("after", &next)]), 4096);
    let times: Vec<_> = [&first, &second].into_iter().flat_map(|p| field(p, "t").items().unwrap().iter().map(|v| v.number().unwrap())).collect();
    assert_eq!(times, vec![0.0, 0.0001, 0.0004, 0.001], "raw cursor walks nonuniform source times exactly once");
    assert!(second.field("next").is_none(), "raw sample walk ends at the last recorded point");
}

#[test]
fn immutable_cursor_walks_obey_budgets_and_preserve_order() {
    let mut host = Host::new(65536);
    host.points = (0..=4400).map(|i| {
        let bit = i / 8;
        let active = (100..120).contains(&bit) || (300..320).contains(&bit);
        (i as f64 / (9600.0 * 8.0), if active && bit % 2 == 0 { 1.0 } else { 0.0 })
    }).collect();
    let end = format!("{}s", host.points.last().unwrap().0);
    let captured = object(host.request("elec.capture.run", &[("trigger", "none"), ("to", &end), ("name", "burst")]), 4096);
    let id = string(&captured, "capture");
    host.points = vec![(0.0, 0.0), (0.01, 0.0)];
    for i in 0..12 {
        let name = format!("{i} {}", "µ".repeat(60));
        object(host.request("elec.capture.run", &[("trigger", "none"), ("name", &name)]), 4096);
    }
    let mut expected_segments = None;
    for budget in [512, 4096, 65536] {
        let budget_text = budget.to_string();
        let mut after = String::new(); let mut got = Vec::new();
        loop {
            let mut args = vec![("capture", id.as_str()), ("budget", budget_text.as_str())];
            if !after.is_empty() { args.push(("after", &after)); }
            let answer = object(host.request("elec.capture.summary", &args), budget);
            for channel in field(&answer, "channels").items().unwrap() {
                for segment in field(channel, "segments").items().unwrap() {
                    got.push((field(segment, "from").number().unwrap(), field(segment, "to").number().unwrap(), string(segment, "what")));
                }
            }
            assert!(got.len() <= 5, "a cursor never duplicates a segment");
            let Some(next) = answer.field("next") else { break };
            after = next.value.string().unwrap().into();
        }
        assert_eq!(got.len(), 5, "every segment is reachable at each budget");
        assert!(got.windows(2).all(|p| p[0].1 == p[1].0), "segment cursor preserves contiguous source order");
        if let Some(expected) = &expected_segments { assert_eq!(&got, expected, "changing budget never changes the query result"); }
        else { expected_segments = Some(got); }
        let mut after = String::new(); let mut ids = Vec::new();
        loop {
            let mut args = vec![("budget", budget_text.as_str())];
            if !after.is_empty() { args.push(("after", &after)); }
            let answer = object(host.request("elec.capture.list", &args), budget);
            ids.extend(field(&answer, "captures").items().unwrap().iter().map(|c| string(c, "id")));
            assert!(ids.len() <= 13, "capture cursor never duplicates an item");
            let Some(next) = answer.field("next") else { break };
            after = next.value.string().unwrap().into();
        }
        assert_eq!(ids, (1..=13).map(|i| format!("cap{i}")).collect::<Vec<_>>(), "capture cursor visits every id in creation order");
        object(host.request("elec.capture.samples", &[("capture", &id), ("budget", &budget_text), ("points", "200")]), budget);
        let measured = object(host.request("elec.measure", &[("capture", &id), ("what", "mean"), ("budget", &budget_text)]), budget);
        let item = &field(&measured, "items").items().unwrap()[0];
        assert!((field(item, "value").number().unwrap() - 20.0 / 550.0).abs() < 1e-12, "measure tool publishes the indexed time-weighted mean");
    }
    // One byte holds no summary item; the answer still carries one whole segment and its continuation.
    let tiny = object(host.request("elec.capture.summary", &[("capture", &id), ("budget", "1")]), usize::MAX);
    let segments: usize = field(&tiny, "channels").items().unwrap().iter().map(|c| field(c, "segments").items().unwrap().len()).sum();
    assert_eq!(segments, 1, "an over-budget summary page carries one whole segment, never a refusal");
    assert_eq!(string(&tiny, "more"), "4 more; ask again with after: next", "and says how many are left and how to ask");
    let first = object(host.request("elec.capture.list", &[("budget", "512")]), 512);
    let mut snapshot_ids: Vec<_> = field(&first, "captures").items().unwrap().iter().map(|c| string(c, "id")).collect();
    let mut after = string(&first, "next");
    object(host.request("elec.capture.drop", &[("capture", "cap2")]), 4096);
    object(host.request("elec.capture.run", &[("trigger", "none")]), 4096);
    loop {
        let answer = object(host.request("elec.capture.list", &[("budget", "512"), ("after", &after)]), 512);
        snapshot_ids.extend(field(&answer, "captures").items().unwrap().iter().map(|c| string(c, "id")));
        let Some(next) = answer.field("next") else { break };
        after = next.value.string().unwrap().into();
    }
    assert_eq!(snapshot_ids, (1..=13).map(|i| format!("cap{i}")).collect::<Vec<_>>(), "list cursors keep their original snapshot through drop and creation");
    let mut limited = Host::new(512);
    object(limited.request("elec.capture.list", &[("budget", "65536")]), 512);
}

#[test]
fn a_full_command_queue_retains_every_invocation() {
    let mut host = Host::new(4096);
    let ids: Vec<_> = (0..48).map(|_| host.tools.call(call("elec.capture.list", &[]))).collect();
    let mut got = Vec::new();
    // No deadline: a call lost from the full queue shows up as a hang.
    while got.len() < ids.len() {
        host.answer();
        for (id, reply) in host.tools.poll() { object(reply, 4096); got.push(id); }
        std::thread::yield_now();
    }
    assert_eq!(got, ids, "full queues keep every call and preserve FIFO order");
}

#[test]
fn synth_queue_survives_cancelled_trigger_and_reconfiguration() {
    let mut host = Host::new(4096);
    let first = object(host.request("elec.capture.run", &[("source", "synth"), ("level", "100V"), ("timeout", "0s")]), 4096);
    assert_eq!(field(&first, "state").name(), Some("waiting"), "unreachable synth threshold waits without stopping the source");
    let second = object(host.request("elec.capture.run", &[("source", "synth"), ("level", "1.65V"), ("to", "1ms"), ("timeout", "0s")]), 4096);
    assert_eq!(field(&second, "state").name(), Some("waiting"), "another synth capture queues behind the open trigger");
    object(host.request("elec.capture.drop", &[("capture", "cap1")]), 4096);
    let reply = host.request("elec.capture.wait", &[("capture", "cap2")]);
    let done = host.done(reply);
    assert_eq!(field(&done, "state").name(), Some("done"), "queued capture finishes when its signal crosses the level");
    assert_eq!(field(field(&done, "resolved"), "rate").number(), Some(2e6), "new capture receives the changed configuration's samples");
    let reply = host.request("elec.capture.run", &[("source", "synth"), ("trigger", "none"), ("to", "1ms")]);
    let third = host.done(reply);
    assert_eq!(string(&third, "capture"), "cap3", "a reused source still allocates a new monotonic capture id");
    assert_eq!(field(&third, "state").name(), Some("done"), "unchanged configuration resumes the same parked source");
}

#[test]
fn simulator_capture_keeps_its_side() {
    let mut host = Host::new(4096);
    let reply = host.request("elec.capture.run", &[("source", "sim"), ("trigger", "none")]);
    let capture = host.done(reply);
    let id = string(&capture, "capture");
    let summary = object(host.request("elec.capture.summary", &[("capture", &id)]), 4096);
    assert_eq!(field(&summary, "side").name(), Some("sim"), "simulator summary reports the originating side");
    let listed = object(host.request("elec.capture.list", &[]), 4096);
    let entries = field(&listed, "captures").items().unwrap();
    assert_eq!(entries.len(), 1, "capture list contains the simulator record");
    assert_eq!(field(&entries[0], "side").name(), Some("sim"), "simulator list item reports the originating side");
}

#[test]
fn firmware_capture_window_and_pages_share_the_lane_timeline() {
    use FwKind::*;
    use FwSource::*;
    let event = |seq, t, source, kind, site| FwEvent { seq, t, source, kind, site };
    let mut store = ScriptedFwEvents::new(vec![
        event(8, 0.008, Image, Write { pin: 1, level: true }, None),
        event(7, 0.007, Trace, Exit { handler: 7 }, Some(23)),
        event(6, 0.006, Image, Write { pin: 1, level: false }, Some(22)),
        event(5, 0.005, Image, Duty { pin: 1, level: 128 }, Some(21)),
        event(4, 0.005, Trace, Statement { statement: 17 }, Some(20)),
        event(3, 0.004, Trace, Enter { handler: 7 }, None),
        event(2, 0.0035, Image, Write { pin: 1, level: true }, Some(19)),
        event(1, 0.001, Image, Write { pin: 2, level: false }, None),
    ]);
    store.pin_names.insert(1, "LED1".into()); store.pin_names.insert(2, "outside".into());
    store.handler_names.insert(7, "timer".into()); store.handler_names.insert(9, "unused".into());
    store.lost = 2;
    let window = ScriptedFwEvents::window(&store, 0.003, 0.007);
    let expected: Vec<_> = (0..window.len()).map(|i| window.get(i).unwrap()).collect();
    assert_eq!(expected.iter().map(|event| event.seq).collect::<Vec<_>>(), vec![2, 3, 4, 5, 6, 7],
        "window bounds: exclude earlier and later events, include the end, and order ties by sequence");
    assert_eq!(window.lost(), 0, "window lost: retained history before the start covers the window");
    assert_eq!(ScriptedFwEvents::window(&store, 0.001, 0.007).lost(), 0, "window lost: starting at the oldest event is covered");
    assert_eq!(window.pin_name(1), Some("LED1"), "window names: mentioned pins are copied");
    assert_eq!(window.handler_name(7), Some("timer"), "window names: mentioned handlers are copied");
    assert_eq!(window.pin_name(2), None, "window names: excluded pins are not copied");
    assert_eq!(window.handler_name(9), None, "window names: unused handlers are not copied");
    let inside = ScriptedFwEvents::window(&store, 0.0045, 0.007);
    assert_eq!(inside.handler_at(0.005), None, "window handlers: an Enter before the start is not carried");
    assert_eq!(inside.handler_name(7), Some("timer"), "window handlers: an unmatched Exit still carries its name");
    let mut empty = ScriptedFwEvents::default(); empty.lost = 2;
    assert_eq!(ScriptedFwEvents::window(&empty, 0.003, 0.007).lost(), 2, "window lost: an empty rolled-off store keeps its warning");

    // Roll off the early event before capture. The oldest retained event is now inside
    // the capture, while the later event still proves that finish takes only its window.
    let mut retained = ScriptedFwEvents::window(&store, 0.003, 0.01); retained.lost = 2;
    let mut retained = Arc::new(retained);
    let mut host = Host::new(4096); host.firmware = Some(retained.clone());
    let reply = host.request("elec.capture.run", &[("source", "sim"), ("trigger", "edge"), ("level", "0.5V"), ("from", "-2ms"), ("to", "2ms")]);
    let done = host.done(reply); let id = string(&done, "capture");
    assert_eq!(field(&done, "state").name(), Some("done"), "firmware capture: simulator finishes");
    assert_eq!(field(&done, "firmware").number(), Some(expected.len() as f64), "firmware capture: completion reports its event count");
    let trigger = field(&done, "t_trigger").number().unwrap();
    assert_eq!(trigger, 0.005, "firmware capture: the analog crossing anchors the shared timeline");
    let listed = object(host.request("elec.capture.list", &[]), 4096);
    let entry = &field(&listed, "captures").items().unwrap()[0];
    assert_eq!(field(entry, "firmware").number(), Some(expected.len() as f64), "firmware list: the count accompanies the completed record");

    let rows = |answer: &Node| {
        let times = field(answer, "t").items().expect("firmware times: a list");
        let sources = field(answer, "source").items().expect("firmware sources: a list");
        let events = field(answer, "event").items().expect("firmware labels: a list");
        let sites = field(answer, "site").items().expect("firmware sites: a list");
        assert_eq!((sources.len(), events.len(), sites.len()), (times.len(), times.len(), times.len()), "firmware columns: every event has a time, source, label and site");
        (0..times.len()).map(|i| (times[i].number().unwrap(), sources[i].name().unwrap().to_owned(),
            events[i].string().unwrap().to_owned(), sites[i].number().unwrap())).collect::<Vec<_>>()
    };
    let full = object(host.request("elec.capture.samples", &[("capture", &id), ("stream", "firmware")]), 4096);
    let all = rows(&full);
    assert_eq!(all.len(), expected.len(), "firmware samples: exactly the in-window events survive");
    assert_eq!(field(&full, "total").number(), Some(expected.len() as f64), "firmware total: counts both sources");
    assert_eq!(field(&full, "lost").number(), Some(2.0), "firmware lost: the window begins before retained history");
    assert!(full.field("next").is_none(), "firmware complete page: no continuation");
    for (row, event) in all.iter().zip(&expected) {
        assert!((row.0 - (event.t - trigger)).abs() < 1e-12, "firmware time: sequence {} is relative to the analog trigger", event.seq);
        assert_eq!(row.1, fw_events::source_name(event.source), "firmware source: sequence {} stays in combined time order", event.seq);
        assert_eq!(row.2, fw_events::label(&store, *event), "firmware label: sequence {} uses the lane's names and format", event.seq);
        assert_eq!(row.3, event.site.map_or(-1.0, |site| site as f64), "firmware site: sequence {} preserves presence or absence", event.seq);
    }

    host.firmware = None;
    *Arc::get_mut(&mut retained).expect("firmware ownership: finished captures release the live store") = empty;
    let mut after = String::new(); let mut joined = Vec::new(); let mut pages = 0;
    loop {
        let mut args = vec![("capture", id.as_str()), ("stream", "firmware"), ("points", "2")];
        if !after.is_empty() { args.push(("after", after.as_str())); }
        let page = object(host.request("elec.capture.samples", &args), 4096);
        let part = rows(&page); pages += 1;
        assert_eq!(part.len(), 2, "firmware paging: points limits each page to two whole events");
        assert_eq!(field(&page, "total").number(), Some(expected.len() as f64), "firmware paging: total describes the whole query");
        assert_eq!(field(&page, "lost").number(), Some(2.0), "firmware paging: lost belongs to the frozen record");
        joined.extend(part);
        assert!(joined.len() <= all.len(), "firmware paging: cursors never repeat events");
        let Some(next) = page.field("next") else { break };
        after = next.value.string().unwrap().into();
    }
    assert_eq!(pages, 3, "firmware paging: six events require three pages");
    assert_eq!(joined, all, "firmware snapshot: pages preserve events and names after live roll-off");
    let narrow = object(host.request("elec.capture.samples", &[("capture", &id), ("stream", "firmware"), ("span", "[-1ms 0s]")]), 4096);
    assert_eq!(rows(&narrow), all[1..4], "firmware span: both ends are inclusive and equal-time sources stay ordered");
    assert_eq!(field(&narrow, "total").number(), Some(3.0), "firmware span: total follows the narrower bounds");
    assert_eq!(field(&narrow, "lost").number(), Some(2.0), "firmware span: loss remains the record's count");
    let absent = object(host.request("elec.capture.samples", &[("capture", &id), ("stream", "firmware"), ("span", "[250us 750us]")]), 4096);
    assert!(rows(&absent).is_empty(), "firmware empty span: a query between events returns empty parallel lists");
    assert_eq!(field(&absent, "total").number(), Some(0.0), "firmware empty span: total is zero");
    assert!(absent.field("next").is_none(), "firmware empty span: no continuation");
    for reduce in ["raw", "mean", "minmax"] {
        let old = host.request("elec.capture.samples", &[("capture", &id), ("reduce", reduce)]);
        let analog = host.request("elec.capture.samples", &[("capture", &id), ("stream", "analog"), ("reduce", reduce)]);
        assert_eq!(analog, old, "analog compatibility: explicit stream preserves the {reduce} reply");
        let analog = object(analog, 4096);
        assert_eq!(field(&analog, "reduce").name(), Some(reduce), "analog compatibility: the old reduction shape remains");
    }
    host.firmware = Some(retained.clone()); host.defer = true;
    let waiting = object(host.request("elec.capture.run", &[("source", "sim"), ("trigger", "none"), ("to", "10ms"), ("timeout", "0s")]), 4096);
    let gap_id = string(&waiting, "capture");
    let mut intake = host.held.take().expect("firmware gap: the host holds the pending intake");
    intake.point(0.004, &[0.0]); intake.gap(0.004, 0.006, fab_elec_bench::source::Gap::Overrun);
    intake.point(0.006, &[1.0]); intake.point(0.01, &[1.0]); intake.idle();
    let reply = host.request("elec.capture.wait", &[("capture", &gap_id)]);
    let done = host.done(reply);
    assert_eq!(field(&done, "firmware").number(), Some(0.0), "firmware empty record: completion distinguishes it from no record");
    let empty = object(host.request("elec.capture.samples", &[("capture", &gap_id), ("stream", "firmware")]), 4096);
    assert!(rows(&empty).is_empty(), "firmware gap: analog gaps do not reject firmware queries");
    assert_eq!(field(&empty, "total").number(), Some(0.0), "firmware empty record: total is zero");
    assert_eq!(field(&empty, "lost").number(), Some(2.0), "firmware empty record: loss survives even when every event has rolled off");
    refused(host.request("elec.capture.samples", &[("capture", &gap_id), ("stream", "analog")]), "samples cross a gap");
    let reply = host.request("elec.capture.run", &[("source", "synth"), ("trigger", "none"), ("to", "10us")]);
    let synth = host.done(reply); let synth_id = string(&synth, "capture");
    assert!(synth.field("firmware").is_none(), "synth completion: there is no firmware record");
    let reply = host.request("elec.capture.samples", &[("capture", &synth_id), ("stream", "firmware")]);
    assert_eq!(reply, ToolReply::Failed(format!("stream: capture {synth_id} has no firmware record")), "synth firmware: the refusal names the capture and missing record");
}

fn pwm_capture(host: &mut Host) -> String {
    // Sixteen 10 kHz periods, 1000 samples each; the ninth high pulse is a 2 us runt.
    // A nonzero source origin also checks every feature time is relative to the trigger.
    host.points = (0..=16000).map(|i| {
        let phase = i % 1000; let end = if i / 1000 == 8 { 120 } else { 400 };
        (0.002 + i as f64 * 1e-7, if (100..end).contains(&phase) { 3.0 } else { 0.0 })
    }).collect();
    let reply = host.request("elec.capture.run", &[("trigger", "none"), ("to", "1.6ms")]);
    let done = host.done(reply); string(&done, "capture")
}
fn edge_rows(answer: &Node) -> Vec<(f64, String)> {
    field(answer, "items").items().unwrap().iter().map(|item| {
        (field(item, "t").number().unwrap(), field(item, "dir").name().unwrap().into())
    }).collect()
}
fn pulse_rows(answer: &Node) -> Vec<(f64, f64, String, Option<f64>)> {
    field(answer, "items").items().unwrap().iter().map(|item| {
        (field(item, "t").number().unwrap(), field(item, "width").number().unwrap(),
            field(item, "level").name().unwrap().into(), item.field("period").map(|p| p.value.number().unwrap()))
    }).collect()
}

#[test]
fn pwm_features_find_the_runt_and_page_every_edge_and_pulse() {
    let mut host = Host::new(65536); let id = pwm_capture(&mut host);
    let baseline = object(host.request("elec.capture.edges", &[("capture", &id), ("budget", "65536")]), 65536);
    let expected_edges = edge_rows(&baseline);
    assert_eq!(expected_edges.len(), 32, "sixteen complete PWM periods have thirty-two edges");
    assert_eq!(field(&baseline, "threshold").number(), Some(1.5), "auto threshold is the mid-swing voltage");
    assert!((field(&baseline, "hysteresis").number().unwrap() - 0.3).abs() < 1e-12, "auto hysteresis is ten percent of the swing");
    for (i, (t, dir)) in expected_edges.iter().enumerate() {
        let period = i / 2; let sample = if i % 2 == 0 { 100 } else if period == 8 { 120 } else { 400 };
        let expected = (period * 1000 + sample) as f64 * 1e-7 - 0.5e-7;
        assert!((t - expected).abs() < 1e-7, "edge {i} has its closed-form trigger-relative time");
        assert_eq!(dir, if i % 2 == 0 { "rising" } else { "falling" }, "edge {i} has its physical direction");
    }
    for item in field(&baseline, "items").items().unwrap() {
        assert!((field(item, "rise").number().unwrap() - 0.8e-7).abs() < 1e-12, "both edge directions report their indexed 10-90 percent transition duration");
    }
    for (limit, budget) in [(3, 512), (1, 4096), (7, 640)] {
        let mut after = String::new(); let mut got = Vec::new();
        let limit_text = limit.to_string(); let budget_text = budget.to_string();
        loop {
            let mut args = vec![("capture", id.as_str()), ("limit", &limit_text), ("budget", &budget_text)];
            if !after.is_empty() { args.push(("after", &after)); }
            let answer = object(host.request("elec.capture.edges", &args), budget);
            let rows = edge_rows(&answer);
            assert!(!rows.is_empty() && rows.len() <= limit, "each edge page advances within its item limit");
            assert_eq!(field(&answer, "total").number(), Some(32.0), "edge total agrees with the closed-form count on every page");
            got.extend(rows);
            assert!(got.len() <= 32, "edge cursor never repeats an item");
            let Some(next) = answer.field("next") else { break };
            after = next.value.string().unwrap().into();
        }
        assert_eq!(got, expected_edges, "every limit and budget walks all edges exactly once in the same order");
        assert!(got.windows(2).all(|p| p[0].0 < p[1].0), "edge cursor times strictly increase");
    }
    for kind in ["rising", "falling"] {
        let answer = object(host.request("elec.capture.edges", &[("capture", &id), ("kind", kind), ("budget", "65536")]), 65536);
        let expected: Vec<_> = expected_edges.iter().filter(|e| e.1 == kind).cloned().collect();
        assert_eq!(edge_rows(&answer), expected, "direction filter returns every edge of only the requested kind");
        assert_eq!(field(&answer, "total").number(), Some(16.0), "each direction has one edge per period");
    }
    for (threshold, hysteresis) in [("1.5V", "\"\""), ("1.5V", "0V"), ("1.6V", "0V")] {
        let answer = object(host.request("elec.capture.edges", &[("capture", &id), ("threshold", threshold), ("hysteresis", hysteresis), ("budget", "65536")]), 65536);
        let rows = edge_rows(&answer);
        assert_eq!(rows.len(), expected_edges.len(), "explicit thresholds retain the PWM edge count");
        for (a, b) in rows.iter().zip(&expected_edges) {
            assert!((a.0 - b.0).abs() <= 1e-7 && a.1 == b.1, "cached-equivalent and scanned thresholds agree within one sample and preserve direction");
        }
    }
    let pulses = object(host.request("elec.capture.pulses", &[("capture", &id), ("level", "high"), ("order", "deviation"), ("budget", "65536")]), 65536);
    let high = pulse_rows(&pulses);
    assert_eq!(high.len(), 16, "every complete high pulse is present");
    assert!((high[0].0 - 809.95e-6).abs() <= 1e-7, "deviation order puts the ninth-period runt first at its start time");
    assert!((high[0].1 - 2e-6).abs() <= 1e-7, "the anomalous pulse is two microseconds wide");
    assert!((field(field(&pulses, "median"), "high").number().unwrap() - 30e-6).abs() <= 1e-7, "the filtered high median remains thirty microseconds");
    assert!(field(&pulses, "median").field("low").is_none(), "a high-only query omits the absent low median");
    for filter in ["width < 10us", "width < 10us and period < 101us"] {
        let answer = object(host.request("elec.capture.pulses", &[("capture", &id), ("where", filter)]), 4096);
        let rows = pulse_rows(&answer);
        assert_eq!(rows.len(), 1, "width and combined width-period filters select exactly the runt");
        assert!((rows[0].0 - high[0].0).abs() <= 1e-7, "the filtered runt has the same start as the deviation-first runt");
        assert_eq!(field(&answer, "total").number(), Some(1.0), "filtered total counts only matching pulses");
        assert!((field(field(&answer, "median"), "high").number().unwrap() - 2e-6).abs() <= 1e-7, "median is recomputed over the filtered set");
    }
    for order in ["time", "deviation"] {
        let full = object(host.request("elec.capture.pulses", &[("capture", &id), ("order", order), ("budget", "65536")]), 65536);
        let expected = pulse_rows(&full);
        assert_eq!(expected.len(), 31, "sixteen highs and fifteen complete lows are pulses");
        let mut after = String::new(); let mut got = Vec::new(); let mut page = 0;
        loop {
            // The cursor identifies the list, so limit and budget may change mid-walk.
            let (limit, budget) = if page % 2 == 0 { ("2", 512) } else { ("3", 1024) };
            let budget_text = budget.to_string();
            let mut args = vec![("capture", id.as_str()), ("order", order), ("limit", limit), ("budget", &budget_text)];
            if !after.is_empty() { args.push(("after", &after)); }
            let answer = object(host.request("elec.capture.pulses", &args), budget);
            let rows = pulse_rows(&answer);
            assert!(!rows.is_empty() && rows.len() <= limit.parse::<usize>().unwrap(), "pulse pages advance within their changing limits");
            assert_eq!(field(&answer, "total").number(), Some(31.0), "pulse total is stable on every page");
            got.extend(rows);
            assert!(got.len() <= 31, "pulse cursor never duplicates a row");
            let Some(next) = answer.field("next") else { break };
            after = next.value.string().unwrap().into(); page += 1;
        }
        assert_eq!(got, expected, "pulse cursor preserves the complete deterministic order through limit and budget changes");
        for level in ["high", "low"] {
            let mut rows: Vec<_> = got.iter().filter(|p| p.2 == level).collect();
            rows.sort_by(|a, b| a.0.total_cmp(&b.0));
            assert!(rows.last().unwrap().3.is_none(), "the last complete pulse of each level has no period");
            for pair in rows.windows(2) {
                assert!((pair[0].3.unwrap() - (pair[1].0 - pair[0].0)).abs() < 1e-12, "period is start to next start of the same level");
            }
        }
    }
    for hysteresis in ["\"\"", "0V"] {
        let answer = object(host.request("elec.capture.pulses", &[("capture", &id), ("span", "[20us 120us]"), ("hysteresis", hysteresis)]), 4096);
        let rows = pulse_rows(&answer);
        assert_eq!(rows.len(), 1, "auto and scanned spans exclude high pulses cut at either end");
        assert_eq!(rows[0].2, "low", "only the complete low pulse inside the cut span remains");
        assert!((rows[0].1 - 70e-6).abs() <= 1e-7, "the surviving pulse keeps its full width");
    }
    let empty = object(host.request("elec.capture.pulses", &[("capture", &id), ("where", "width > 2ms")]), 4096);
    assert_eq!(field(&empty, "total").number(), Some(0.0), "an unmatched filter has zero total");
    assert!(pulse_rows(&empty).is_empty() && empty.field("next").is_none(), "an empty pulse query has no rows or cursor");
    assert!(field(&empty, "median").field("high").is_none() && field(&empty, "median").field("low").is_none(), "empty filtered levels omit both medians");
    for tool in ["elec.capture.edges", "elec.capture.pulses"] {
        refused(host.request(tool, &[("capture", "missing")]), "capture:");
        refused(host.request(tool, &[("capture", &id), ("threshold", "fast")]), "threshold:");
        refused(host.request(tool, &[("capture", &id), ("hysteresis", "-1V")]), "hysteresis:");
        refused(host.request(tool, &[("capture", &id), ("threshold", "1ms")]), "threshold:");
        let first = object(host.request(tool, &[("capture", &id), ("limit", "1")]), 4096);
        let next = string(&first, "next");
        for change in [("kind", "rising"), ("threshold", "1.6V"), ("hysteresis", "0V"), ("span", "[20us 120us]")] {
            refused(host.request(tool, &[("capture", &id), ("after", &next), change]), "after:");
        }
        if tool == "elec.capture.pulses" {
            for change in [("level", "high"), ("order", "deviation"), ("where", "width < 10us")] {
                refused(host.request(tool, &[("capture", &id), ("after", &next), change]), "after:");
            }
        }
    }
    for clause in ["width ~ 3us", "period < fast", "width < 3V"] {
        refused(host.request("elec.capture.pulses", &[("capture", &id), ("where", clause)]), clause);
    }
    refused(host.request("elec.capture.pulses", &[("capture", &id), ("where", "width > 1us and period ~ 3us")]), "period ~ 3us");
}

#[test]
fn feature_gaps_break_pulses_and_incomplete_transitions_omit_rise() {
    let mut host = Host::new(65536); host.defer = true;
    let waiting = object(host.request("elec.capture.run", &[("trigger", "none"), ("to", "14us"), ("timeout", "0s")]), 4096);
    let id = string(&waiting, "capture"); let mut intake = host.held.take().unwrap();
    intake.point(1e-6, &[1.0]); intake.point(2e-6, &[1.0]);
    intake.gap(2e-6, 4e-6, fab_elec_bench::source::Gap::Overrun);
    for (t, y) in [(4, 1.0), (5, 0.0), (6, 1.0), (7, 0.0), (8, 1.0), (9, 0.0), (10, f32::NAN), (11, 0.0), (12, 1.0), (13, 0.0), (14, 0.0)] {
        intake.point(t as f64 * 1e-6, &[y]);
    }
    intake.idle(); drop(intake);
    let reply = host.request("elec.capture.wait", &[("capture", &id)]); host.done(reply);
    for hysteresis in ["\"\"", "0V"] {
        let answer = object(host.request("elec.capture.pulses", &[("capture", &id), ("hysteresis", hysteresis)]), 4096);
        let rows = pulse_rows(&answer);
        assert_eq!(rows.len(), 5, "auto and scanned pulses exclude intervals crossing source gaps and nonfinite samples");
        assert!(rows.iter().all(|p| (p.1 - 1e-6).abs() < 1e-12), "only uninterrupted one-microsecond pulses survive");
        assert_eq!(rows.iter().filter(|p| p.3.is_some()).count(), 2, "same-level periods never bridge either kind of gap");
    }
    host.defer = false;
    host.points = vec![(0.0, 0.0), (1e-6, 0.0), (2e-6, 1.0), (3e-6, 1.0), (4e-6, 1.0), (5e-6, 0.0), (6e-6, 0.6), (7e-6, 0.6)];
    let reply = host.request("elec.capture.run", &[("trigger", "none"), ("to", "7us")]);
    let done = host.done(reply); let id = string(&done, "capture");
    let answer = object(host.request("elec.capture.edges", &[("capture", &id)]), 4096);
    let items = field(&answer, "items").items().unwrap();
    assert_eq!(items.len(), 3, "a final mid-swing crossing is an edge even without a complete 10-90 transition");
    assert!(items[..2].iter().all(|item| item.field("rise").is_some()), "complete rising and falling transitions have duration fields");
    assert!(items[2].field("rise").is_none(), "the incomplete final transition omits its rise field");
}

fn uart_points(baud: f64, parity: bool, invert: bool, framing: Option<usize>, parity_error: Option<usize>) -> Vec<(f64, f32)> {
    let mut levels = vec![true; 3];
    for (i, &byte) in b"Hello\n".iter().enumerate() {
        levels.push(false);
        for bit in 0..8 { levels.push(byte & (1 << bit) != 0); }
        if parity { levels.push((byte.count_ones() % 2 != 0) ^ (parity_error == Some(i))); }
        levels.push(framing != Some(i));
        levels.extend([true; 3]);
    }
    // Closed-form rectangular frames at sixteen samples per bit, with idle between frames.
    (0..levels.len() * 16).map(|i| (i as f64 / (baud * 16.0), if levels[i / 16] ^ invert { 1.0 } else { 0.0 })).collect()
}
fn uart_capture(host: &mut Host, points: Vec<(f64, f32)>) -> String {
    host.points = points;
    let end = format!("{}s", host.points.last().unwrap().0 - host.points[0].0);
    let reply = host.request("elec.capture.run", &[("trigger", "none"), ("to", &end)]);
    string(&host.done(reply), "capture")
}
fn uart_bytes(answer: &Node) -> Vec<u16> {
    field(answer, "frames").items().unwrap().iter().map(|f| field(f, "byte").number().unwrap() as u16).collect()
}
fn uart_walk(host: &mut Host, id: &str) -> Vec<ToolReply> {
    let mut after = String::new(); let mut bytes = Vec::new(); let mut pages = Vec::new();
    let mut times = Vec::new();
    loop {
        let mut args = vec![("capture", id), ("limit", "2")];
        if !after.is_empty() { args.push(("after", &after)); }
        let reply = host.request("elec.capture.decode", &args);
        let answer = object(reply.clone(), 4096);
        let page = uart_bytes(&answer);
        assert_eq!(page.len(), 2, "limit-two UART walk returns two complete frames per page");
        let expected_text = match pages.len() { 0 => "He", 1 => "ll", _ => "o\\x0A" };
        assert_eq!(string(&answer, "text"), expected_text, "UART page text contains exactly that page's bytes");
        assert_eq!(field(field(&answer, "errors"), "framing").number(), Some(1.0), "UART page carries whole-span framing count");
        bytes.extend(page);
        times.extend(field(&answer, "frames").items().unwrap().iter().map(|f| field(f, "t0").number().unwrap()));
        assert!(times.windows(2).all(|p| p[0] < p[1]), "UART cursor preserves strictly increasing frame times even for repeated bytes");
        assert!(bytes.len() <= 6, "UART cursor never duplicates a frame");
        pages.push(reply);
        let Some(next) = answer.field("next") else {
            assert_eq!(Some(bytes.len() as f64), field(&answer, "total").number(), "UART cursor count equals total");
            break;
        };
        after = next.value.string().unwrap().into();
    }
    assert_eq!(bytes, b"Hello\n".iter().map(|&b| b as u16).collect::<Vec<_>>(), "UART cursor visits every received byte once in order");
    pages
}

#[test]
fn uart_auto_decode_framing_paging_cache_and_refusals() {
    let mut host = Host::new(65536);
    let baud = 115200.0;
    let points = uart_points(baud, false, false, Some(3), None).into_iter().map(|(t, y)| (t + 0.125, y)).collect();
    let id = uart_capture(&mut host, points);
    let answer = object(host.request("elec.capture.decode", &[("capture", &id)]), 4096);
    assert_eq!(uart_bytes(&answer), b"Hello\n".iter().map(|&b| b as u16).collect::<Vec<_>>(), "auto 8N1 preserves every byte including the framing error");
    assert_eq!(string(&answer, "text"), "Hello\\x0A", "UART text escapes nonprintable bytes and preserves bad-frame bytes");
    assert_eq!(field(&answer, "total").number(), Some(6.0), "whole UART span contains six frames");
    assert!(answer.field("next").is_none(), "whole UART reply has no continuation");
    let frames = field(&answer, "frames").items().unwrap();
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame.field("error").and_then(|e| e.value.name()), if i == 3 { Some("framing") } else { None }, "8N1 frame {i}: only the fourth stop bit is bad");
        let start = (3.0 + i as f64 * 13.0 - 0.5 / 16.0) / baud;
        assert!((field(frame, "t0").number().unwrap() - start).abs() < 0.01 / baud, "8N1 frame {i}: start is relative to the nonzero trigger");
        assert!((field(frame, "t1").number().unwrap() - start - 9.5 / baud).abs() < 0.01 / baud, "8N1 frame {i}: stop centre is relative to the trigger");
    }
    let rate = field(&answer, "baud");
    assert_eq!(field(rate, "nominal").number(), Some(baud), "auto baud identifies 115200 exactly");
    let measured = field(rate, "measured").number().unwrap();
    assert!((measured / baud - 1.0).abs() < 0.005, "auto measured baud is within half a percent");
    assert_eq!(field(rate, "used").number(), Some(measured), "auto decode uses the measured baud");
    assert!(field(rate, "error").number().unwrap().abs() < 0.005, "nominal baud error is near zero");
    for (name, count) in [("framing", 1.0), ("parity", 0.0), ("break", 0.0)] {
        assert_eq!(field(field(&answer, "errors"), name).number(), Some(count), "whole UART span has the expected {name} count");
    }
    let first_walk = uart_walk(&mut host, &id);
    assert_eq!(uart_walk(&mut host, &id), first_walk, "cached UART walk returns identical pages and cursors");
    let first = object(first_walk[0].clone(), 4096);
    let next = string(&first, "next");
    for rate in ["115100", "115200", "115300", "115400"] {
        let answer = object(host.request("elec.capture.decode", &[("capture", &id), ("baud", rate)]), 4096);
        assert_eq!(field(field(&answer, "baud"), "used").number(), rate.parse::<f64>().ok(), "explicit baud is used verbatim");
    }
    assert_eq!(host.request("elec.capture.decode", &[("capture", &id), ("limit", "2"), ("after", &next)]), first_walk[1], "an evicted decode replays an existing cursor unchanged");
    assert_eq!(uart_walk(&mut host, &id), first_walk, "re-decoding after eviction preserves every page");
    let explicit = object(host.request("elec.capture.decode", &[("capture", &id), ("threshold", "0.5V")]), 4096);
    assert_eq!(uart_bytes(&explicit), uart_bytes(&answer), "explicit threshold uses the same edge definition as the index");
    assert_eq!(field(field(&explicit, "baud"), "nominal").number(), Some(baud), "explicit-threshold edge scan estimates the same nominal baud");
    let span = format!("[0s {}s]", 28.0 / baud);
    let partial = object(host.request("elec.capture.decode", &[("capture", &id), ("span", &span)]), 4096);
    assert_eq!(uart_bytes(&partial), vec![b'H' as u16, b'e' as u16], "narrow UART span decodes only its complete frames");
    assert_eq!(field(field(&partial, "errors"), "framing").number(), Some(0.0), "narrow span excludes the later framing error");
    refused(host.request("elec.capture.decode", &[("capture", &id), ("span", &span), ("after", &next)]), "after");
    refused(host.request("elec.capture.decode", &[("capture", &id), ("baud", "115200"), ("after", &next)]), "after");
    refused(host.request("elec.capture.decode", &[("capture", &id), ("bits", "4")]), "bits");
    refused(host.request("elec.capture.decode", &[("capture", &id), ("baud", "0")]), "baud");
    refused(host.request("elec.measure", &[("capture", &id), ("what", "uart")]), "elec.capture.decode");
    // A frame and its metadata cannot fit one byte; the page still answers the first whole frame.
    let tiny = object(host.request("elec.capture.decode", &[("capture", &id), ("budget", "1")]), usize::MAX);
    assert_eq!(uart_bytes(&tiny), uart_bytes(&answer)[..1], "an over-budget UART page carries the first whole frame, never a refusal");
    assert_eq!(string(&tiny, "text").len(), 1, "its text is that frame's byte");
    assert!(string(&tiny, "more").ends_with(" more; ask again with after: next"), "and says how to ask for the rest");
    object(host.request("elec.capture.drop", &[("capture", &id)]), 4096);
    refused(host.request("elec.capture.decode", &[("capture", &id), ("after", &next)]), "unknown");
    let dc = uart_capture(&mut host, vec![(0.0, 1.0), (0.01, 1.0)]);
    refused(host.request("elec.capture.decode", &[("capture", &dc), ("baud", "auto")]), "baud: too few edges to estimate; give baud or a longer span");
    let explicit = object(host.request("elec.capture.decode", &[("capture", &dc), ("baud", "9600")]), 4096);
    assert_eq!(field(&explicit, "total").number(), Some(0.0), "explicit baud permits an idle line with no frames");
    assert_eq!(string(&explicit, "text"), "", "empty UART decode returns empty valid text");
    assert!(field(&explicit, "baud").field("measured").is_none(), "idle line does not invent a measured baud");
}

#[test]
fn uart_even_parity_and_inverted_line_preserve_bytes() {
    let mut host = Host::new(4096);
    let mut expected = None;
    for invert in [false, true] {
        let id = uart_capture(&mut host, uart_points(9600.0, true, invert, None, Some(2)));
        let answer = object(host.request("elec.capture.decode", &[("capture", &id), ("parity", "even"), ("invert", if invert { "yes" } else { "no" })]), 4096);
        let bytes = uart_bytes(&answer);
        assert_eq!(bytes, b"Hello\n".iter().map(|&b| b as u16).collect::<Vec<_>>(), "8E1 invert={invert}: every received byte is preserved");
        if let Some(expected) = &expected { assert_eq!(&bytes, expected, "inverting both line and decoder preserves the same bytes"); }
        else { expected = Some(bytes); }
        for (i, frame) in field(&answer, "frames").items().unwrap().iter().enumerate() {
            assert_eq!(frame.field("error").and_then(|e| e.value.name()), if i == 2 { Some("parity") } else { None }, "8E1 invert={invert} frame {i}: only the flipped parity bit is bad");
        }
        let rate = field(&answer, "baud");
        assert_eq!(field(rate, "nominal").number(), Some(9600.0), "8E1 invert={invert}: nominal baud is 9600");
        assert!((field(rate, "measured").number().unwrap() / 9600.0 - 1.0).abs() < 0.005, "8E1 invert={invert}: measured baud is within half a percent");
        assert_eq!(string(&answer, "text"), "Hello\\x0A", "8E1 invert={invert}: text preserves the byte with bad parity");
        for (name, count) in [("framing", 0.0), ("parity", 1.0), ("break", 0.0)] {
            assert_eq!(field(field(&answer, "errors"), name).number(), Some(count), "8E1 invert={invert}: whole-span {name} count is correct");
        }
    }
}

fn compare_wave(host: &mut Host, delay: usize, missing: Option<usize>) -> String {
    host.points = (0..=4000usize).map(|i| {
        let high = i.checked_sub(delay + 100).is_some_and(|at| at / 400 < 10 && at % 400 < 200 && missing != Some(at / 400));
        (i as f64 * 2e-6, if high { 1.0 } else { 0.0 })
    }).collect();
    let end = format!("{}s", host.points.last().unwrap().0);
    let reply = host.request("elec.capture.run", &[("trigger", "none"), ("to", &end)]);
    let done = host.done(reply); string(&done, "capture")
}
fn difference_rows(answer: &Node) -> Vec<(f64, String, String, usize, Option<f64>)> {
    field(answer, "differences").items().unwrap().iter().map(|item| (
        field(item, "t").number().unwrap(), field(item, "kind").name().unwrap().into(),
        field(item, "dir").name().unwrap().into(), field(item, "channel").number().unwrap() as usize,
        item.field("dt").and_then(|dt| dt.value.number()),
    )).collect()
}

#[test]
fn compare_fits_delays_locates_missing_pulses_and_pages_after_eviction() {
    let mut host = Host::new(65536);
    let expected = compare_wave(&mut host, 0, None);
    let actual = compare_wave(&mut host, 20, Some(4));
    let args = [("expected", expected.as_str()), ("actual", actual.as_str()), ("align", "fit")];
    let answer = object(host.request("elec.capture.compare", &args), 4096);
    assert_eq!(field(&answer, "verdict").name(), Some("fail"), "a removed pulse fails comparison");
    let alignment = field(&answer, "alignment");
    assert_eq!(field(alignment, "method").name(), Some("fit"), "unanchored comparison reports fit");
    assert!((field(alignment, "offset").number().unwrap() - 40e-6).abs() < 2e-6, "comparison recovers delay within one sample step");
    assert!(field(alignment, "runner_up").number().unwrap() < field(alignment, "matched").number().unwrap(), "the delayed finite wave has a unique best alignment");
    assert!((field(field(&answer, "tolerance_used"), "time").number().unwrap() - 4e-6).abs() < 1e-12, "derived tolerance is twice the largest channel step");
    let channel = &field(&answer, "channels").items().unwrap()[0];
    for (name, count) in [("expected", 0.0), ("actual", 0.0), ("matched", 18.0), ("missing", 2.0), ("extra", 0.0), ("late", 0.0), ("early", 0.0)] {
        assert_eq!(field(channel, name).number(), Some(count), "comparison channel {name} reports the expected count or id");
    }
    let first = field(channel, "first_difference");
    assert_eq!(field(first, "kind").name(), Some("missing_edge"), "first difference is a missing edge");
    assert_eq!(field(first, "dir").name(), Some("rising"), "first difference is the removed pulse's rising edge");
    assert!((field(first, "t").number().unwrap() - 3399e-6).abs() < 1e-12, "missing pulse is located at its expected interpolated crossing");
    assert!(first.field("dt").is_none(), "missing edges omit residual dt");
    let rows = difference_rows(&answer);
    assert_eq!(rows.len(), 2, "only the missing pulse's two edges differ");
    assert!((rows[1].0 - 3799e-6).abs() < 1e-12 && rows[1].2 == "falling", "second difference is the removed pulse's falling edge");
    let total = field(&answer, "total").number().unwrap() as usize;
    let mut after = String::new(); let mut walked = Vec::new(); let mut first_cursor = None;
    loop {
        let mut page_args = args.to_vec(); page_args.push(("limit", "1"));
        if !after.is_empty() { page_args.push(("after", &after)); }
        let page = object(host.request("elec.capture.compare", &page_args), 4096);
        let page_rows = difference_rows(&page);
        assert_eq!(page_rows.len(), 1, "limit one returns exactly one whole difference per page");
        walked.extend(page_rows);
        assert!(walked.len() <= total, "cursor walk never repeats a difference");
        let Some(next) = page.field("next") else { break };
        after = next.value.string().unwrap().into();
        if first_cursor.is_none() { first_cursor = Some(after.clone()); }
    }
    assert_eq!(walked, rows, "cursor walk returns every difference once in deterministic time order");
    assert_eq!(walked.len(), total, "cursor walk count equals whole-query total");
    let first_cursor = first_cursor.unwrap();
    for tolerance in ["10us", "11us", "12us", "13us", "14us"] {
        let mut changed = args.to_vec(); changed.push(("tolerance", tolerance));
        object(host.request("elec.capture.compare", &changed), 4096);
    }
    let mut resumed = args.to_vec(); resumed.extend([("limit", "1"), ("after", first_cursor.as_str())]);
    let page = object(host.request("elec.capture.compare", &resumed), 4096);
    assert_eq!(difference_rows(&page), rows[1..], "evicted comparison recomputes with the same cursor order");
    for changed in [("tolerance", "10us"), ("channels", "0:0,0:9"), ("span", "[1ms 7ms]"), ("anchor", "falling"), ("skew", "none")] {
        let mut changed_args = resumed.clone(); changed_args.push(changed);
        refused(host.request("elec.capture.compare", &changed_args), if changed.0 == "channels" { "channels" } else { "after" });
    }
    let same = object(host.request("elec.capture.compare", &[("expected", &expected), ("actual", &expected), ("align", "fit")]), 4096);
    let edges = object(host.request("elec.capture.edges", &[("capture", &expected)]), 4096);
    assert_eq!(field(&same, "verdict").name(), Some("pass"), "a capture compared against itself passes");
    assert_eq!(field(field(&same, "alignment"), "matched").number(), field(&edges, "total").number(), "identical comparison matches every cached edge");
    assert_eq!(field(&same, "total").number(), Some(0.0), "identical comparison has no differences");
    assert!(field(&same, "channels").items().unwrap()[0].field("first_difference").is_none(), "identical channel omits first_difference");
    for (align, skew, method) in [("trigger", "fit", "trigger_fit"), ("event", "fit", "event_fit"), ("event", "none", "event"), ("trigger", "none", "trigger")] {
        let result = object(host.request("elec.capture.compare", &[("expected", &expected), ("actual", &actual), ("align", align), ("skew", skew)]), 4096);
        let value = field(field(&result, "alignment"), "method");
        assert_eq!(value.name(), Some(method), "{method} is the reported method choice");
        assert!(field(&result, "alignment").field("runner_up").is_none(), "{method} omits unanchored runner_up");
        if skew == "none" {
            assert_eq!(field(field(&result, "alignment"), "skew").number(), Some(0.0), "anchored skew none has exactly zero skew");
        }
    }
    for channel in ["9:0", "0:9"] {
        refused(host.request("elec.capture.compare", &[("expected", &expected), ("actual", &actual), ("channels", channel)]), "channels");
    }
    for tolerance in ["\"fast\"", "1V", "-1us"] {
        refused(host.request("elec.capture.compare", &[("expected", &expected), ("actual", &actual), ("tolerance", tolerance)]), "tolerance");
    }
    refused(host.request("elec.capture.compare", &[("expected", "unknown"), ("actual", &actual)]), "expected");
    refused(host.request("elec.capture.compare", &[("expected", &expected), ("actual", "unknown")]), "actual");
    object(host.request("elec.capture.drop", &[("capture", &actual)]), 4096);
    refused(host.request("elec.capture.compare", &args), "actual");
    object(host.request("elec.capture.drop", &[("capture", &expected)]), 4096);
    refused(host.request("elec.capture.compare", &[("expected", &expected), ("actual", &expected)]), "expected");
}

#[test]
fn compare_gaps_prevent_judgment_and_missing_anchors_name_the_side() {
    let mut host = Host::new(4096); host.defer = true;
    let waiting = object(host.request("elec.capture.run", &[("trigger", "none"), ("to", "14us"), ("timeout", "0s")]), 4096);
    let gapped = string(&waiting, "capture"); let mut intake = host.held.take().unwrap();
    intake.point(1e-6, &[1.0]); intake.point(2e-6, &[1.0]);
    intake.gap(2e-6, 4e-6, fab_elec_bench::source::Gap::Overrun);
    for i in 4..=14 { intake.point(i as f64 * 1e-6, &[if i % 2 == 0 { 1.0 } else { 0.0 }]); }
    intake.idle(); drop(intake);
    let reply = host.request("elec.capture.wait", &[("capture", &gapped)]); host.done(reply);
    host.defer = false;
    host.points = (0..=14).map(|i| (i as f64 * 1e-6, if i % 2 == 0 { 1.0 } else { 0.0 })).collect();
    let reply = host.request("elec.capture.run", &[("trigger", "none"), ("to", "14us")]);
    let done = host.done(reply); let clean = string(&done, "capture");
    for (expected, actual, reason) in [(&clean, &gapped, "actual"), (&gapped, &clean, "expected"), (&gapped, &gapped, "expected and actual")] {
        let result = object(host.request("elec.capture.compare", &[("expected", expected), ("actual", actual), ("skew", "none")]), 4096);
        assert_eq!(field(&result, "verdict").name(), Some("not_judged"), "overlapping {reason} gap prevents judgment");
        assert!(string(&result, "reason").contains(reason), "gap reason identifies {reason}");
    }
    let result = object(host.request("elec.capture.compare", &[("expected", &gapped), ("actual", &gapped), ("span", "[5us 14us]"), ("skew", "none"), ("tolerance", "0.01us")]), 4096);
    assert_eq!(field(&result, "verdict").name(), Some("pass"), "a gap outside the compared span permits judgment");
    host.points = vec![(0.0, 0.0), (14e-6, 0.0)];
    let reply = host.request("elec.capture.run", &[("trigger", "none"), ("to", "14us")]);
    let done = host.done(reply); let idle = string(&done, "capture");
    refused(host.request("elec.capture.compare", &[("expected", &clean), ("actual", &idle), ("align", "event")]), "anchor: no rising edge in actual; use align: fit");
    refused(host.request("elec.capture.compare", &[("expected", &idle), ("actual", &clean), ("align", "event"), ("anchor", "falling")]), "anchor: no falling edge in expected; use align: fit");
}

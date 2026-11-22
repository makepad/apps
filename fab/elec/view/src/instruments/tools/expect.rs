//! Live expectation snapshots and bounded, serial simulator repeats on the tools worker.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use fab_elec_bench::expect::{Bound, Outcome, Verdict, When};
use fab_elec_circuit::QtyUnit;
use super::call::Arguments;
use super::reply::{self, Object};
use super::{ExpectRow, ExpectSnapshot, HostRequest, ToolReply};

const NO_REAL: &str = "no real source is open";

fn names(args: &Arguments) -> Option<BTreeSet<String>> {
    args.text("names").map(|text| text.split(',').map(|name| name.trim().to_owned()).collect())
}
fn select(snapshot: &mut ExpectSnapshot, names: Option<&BTreeSet<String>>) -> Result<(), String> {
    if let Some(names) = names {
        let known: BTreeSet<_> = snapshot.rows.iter().map(|row| row.name.as_str()).collect();
        if let Some(name) = names.iter().find(|name| !known.contains(name.as_str())) {
            return Err(format!("names: unknown {name:?}; known names: {}", known.into_iter().collect::<Vec<_>>().join(", ")));
        }
        snapshot.rows.retain(|row| names.contains(&row.name));
    }
    Ok(())
}
fn metadata(snapshot: &ExpectSnapshot) -> Object {
    let mut out = Object::default(); out.field("run", snapshot.run.to_string());
    out.num("t", Some(snapshot.t), QtyUnit::Second); out.field("running", snapshot.running.to_string()); out
}
fn failure(t: f64, value: f64) -> Object {
    let mut out = Object::default(); out.num("t", Some(t), QtyUnit::Second);
    out.num("value", Some(value), QtyUnit::Count); out
}
fn outcome(outcome: Outcome) -> &'static str {
    match outcome { Outcome::Pass => "pass", Outcome::Fail => "fail", Outcome::NotJudged => "not judged" }
}
/// A compact row keeps what reads a verdict (name, unit, bounds, outcome, value, when judged);
/// elec.expect.list has the rest (probe, measure, window, site, worst).
fn row(row: &ExpectRow, on: &str, compact: bool) -> Object {
    let mut out = Object::default(); out.text("name", &row.name);
    if !compact { out.text("probe", &row.probe); }
    out.text("unit", &row.unit);
    if !compact {
        out.text("measure", &row.measure);
        let mut when = Object::default(); let mut times = Object::default();
        match row.when {
            When::Span { from, to } => {
                times.num("from", Some(from), QtyUnit::Second); times.num("to", Some(to), QtyUnit::Second);
                when.field("span", times.render());
            }
            When::Trailing { window, after } => {
                times.num("window", Some(window), QtyUnit::Second); times.num("after", Some(after), QtyUnit::Second);
                when.field("trailing", times.render());
            }
        }
        out.field("when", when.render());
    }
    match &row.bound {
        Bound::Range { min, max } => {
            out.num("min", Some(*min), QtyUnit::Count); out.num("max", Some(*max), QtyUnit::Count);
        }
        Bound::TextEquals(text) => out.text("equals", text),
        Bound::TextContains(text) => out.text("contains", text),
    }
    if !compact { out.site("site", &row.site); }
    if on != "real" {
        let mut sim = Object::default(); sim.text("outcome", outcome(row.state.outcome()));
        sim.num("value", Some(row.state.value), QtyUnit::Count); sim.num("at", Some(row.state.at), QtyUnit::Second);
        if !compact {
            sim.field("done", row.state.done.to_string());
            if let Some((t, value)) = row.state.failed { sim.field("first_failure", failure(t, value).render()); }
            sim.num("worst", Some(row.state.worst), QtyUnit::Count);
        }
        out.field("sim", sim.render());
    }
    if on != "sim" {
        let mut real = Object::default(); real.text("outcome", "not judged"); real.text("reason", NO_REAL);
        out.field("real", real.render());
    }
    out
}

struct Listing {
    snapshot: ExpectSnapshot,
    names: Option<BTreeSet<String>>,
    on: String,
}
/// As with capture lists, a cursor retains its snapshot while the live source changes.
#[derive(Default)]
pub(super) struct Lists {
    next: u64,
    entries: BTreeMap<u64, Listing>,
}
impl Lists {
    pub(super) fn list(&mut self, args: &Arguments, budget: usize, snapshot: Option<ExpectSnapshot>) -> Result<ToolReply, String> {
        let names = names(args); let on = args.text("on").unwrap();
        let version = if let Some(after) = args.text("after") {
            after.split('/').nth(1).and_then(|s| s.parse::<u64>().ok()).ok_or("after: invalid expectation list cursor")?
        } else {
            let mut snapshot = snapshot.ok_or("host: missing expectation snapshot")?; select(&mut snapshot, names.as_ref())?;
            self.next += 1;
            self.entries.insert(self.next, Listing { snapshot, names: names.clone(), on: on.into() }); self.next
        };
        let listing = self.entries.get(&version).ok_or("after: unknown expectation list cursor")?;
        if listing.names != names || listing.on != on { return Err("after: cursor belongs to another query".into()); }
        let snapshot = &listing.snapshot; let total = snapshot.rows.len();
        let key = format!("expects/{version}"); let start = reply::offset(args.text("after"), &key, total)?;
        let limit = usize::try_from(args.int("limit").unwrap()).unwrap_or(usize::MAX);
        reply::page_values(budget, start, total, limit, &key, |i| Ok(row(&snapshot.rows[i], on, false).render()), |items, next| {
            let mut out = metadata(snapshot); out.field("items", reply::list(items.iter().cloned()));
            out.int("total", total); out.next(next); out.render()
        })
    }
}

#[derive(Default)]
struct Repeats {
    pass: u64,
    fail: u64,
    not_judged: u64,
    first_failure: Option<(u64, f64, f64)>,
}
fn final_row(row: &ExpectRow, running: bool) -> bool {
    !running || row.state.done || matches!(row.when, When::Trailing { .. }) && row.state.verdict != Verdict::Pending
}
/// A finished run's answer, kept so its later pages read it without running again.
pub(super) struct Answer {
    head: Object,
    full: Vec<String>,
    compact: Vec<String>,
}
fn result(snapshot: Option<&ExpectSnapshot>, counts: &BTreeMap<String, Repeats>, current: bool, done: bool) -> Answer {
    let mut head = snapshot.map(metadata).unwrap_or_default(); head.text("state", if done { "done" } else { "waiting" });
    let rows = |compact: bool| snapshot.into_iter().flat_map(|snapshot| snapshot.rows.iter().map(move |entry| (snapshot, entry))).map(|(snapshot, entry)| {
        let mut out = row(entry, "sim", compact); out.field("final", (current && final_row(entry, snapshot.running)).to_string());
        let mut repeats = Object::default(); let empty = Repeats::default(); let count = counts.get(&entry.name).unwrap_or(&empty);
        repeats.field("pass", count.pass.to_string()); repeats.field("fail", count.fail.to_string()); repeats.field("not_judged", count.not_judged.to_string());
        out.field("repeats", repeats.render());
        if let Some((repeat, t, value)) = count.first_failure {
            let mut first = failure(t, value); first.field("repeat", repeat.to_string()); out.field("first_failure", first.render());
        }
        out.render()
    }).collect::<Vec<_>>();
    Answer { head, full: rows(false), compact: rows(true) }
}

/// Finished runs by cursor. A run's answer pages in full rows when they all fit the first page's
/// budget, else in compact rows, and its later pages keep that choice.
#[derive(Default)]
pub(super) struct Runs {
    next: u64,
    entries: BTreeMap<u64, (Object, Vec<String>)>,
}
impl Runs {
    pub(super) fn answer(&mut self, answer: Answer, budget: usize) -> ToolReply {
        let render = |head: &Object, items: &[String], total: usize| {
            let mut out = head.clone(); out.field("items", reply::list(items.iter().cloned())); out.int("total", total); out.render()
        };
        let Answer { mut head, full, compact } = answer;
        let items = if render(&head, &full, full.len()).len() <= budget { full } else {
            head.field("compact", "true".into()); compact
        };
        self.next += 1;
        self.entries.insert(self.next, (head, items));
        self.page(self.next, None, budget).unwrap()
    }
    pub(super) fn page(&self, version: u64, after: Option<&str>, budget: usize) -> Result<ToolReply, String> {
        let (head, items) = self.entries.get(&version).ok_or("after: unknown expectation run cursor")?;
        let key = format!("runs/{version}"); let total = items.len();
        let start = reply::offset(after, &key, total)?;
        Ok(reply::page(budget, start, total, &key, |i| items[i].clone(), |page, next| {
            let mut out = head.clone(); out.field("items", reply::list(page.iter().cloned()));
            out.int("total", total); out.next(next); out.render()
        }))
    }
    pub(super) fn after(&self, after: &str, budget: usize) -> Result<ToolReply, String> {
        let version = after.strip_prefix("runs/").and_then(|rest| rest.split('/').next()).and_then(|v| v.parse::<u64>().ok())
            .ok_or("after: invalid expectation run cursor")?;
        self.page(version, Some(after), budget)
    }
}

struct Host<'a> {
    requests: &'a Sender<HostRequest>,
    stop: &'a AtomicBool,
    tick: Duration,
    deadline: Instant,
    now: fn() -> Instant,
}
impl Host<'_> {
    /// None means the whole-call deadline elapsed, including while waiting for a host reply.
    fn read<T>(&self, request: impl FnOnce(Sender<Result<T, String>>) -> HostRequest) -> Result<Option<T>, String> {
        if self.stop.load(Ordering::Acquire) { return Err("expect: tools stopped".into()); }
        let (reply, response) = channel();
        if (self.now)() >= self.deadline { return Ok(None); }
        self.requests.send(request(reply)).map_err(|_| "host: disconnected")?;
        loop {
            if self.stop.load(Ordering::Acquire) { return Err("expect: tools stopped".into()); }
            let left = self.deadline.saturating_duration_since((self.now)());
            if left.is_zero() { return Ok(None); }
            match response.recv_timeout(left.min(self.tick)) {
                Ok(_) if (self.now)() >= self.deadline => return Ok(None),
                Ok(answer) => return answer.map(Some),
                Err(RecvTimeoutError::Disconnected) => return Err("host: reply sender disconnected".into()),
                Err(RecvTimeoutError::Timeout) => (),
            }
        }
    }
    fn tick(&self) { std::thread::sleep(self.tick.min(self.deadline.saturating_duration_since((self.now)()))); }
}

/// Deadlines read `now` (the worker's clock).
pub(super) fn run(args: &Arguments, requests: &Sender<HostRequest>, stop: &AtomicBool, tick: Duration, now: fn() -> Instant) -> Result<Answer, String> {
    if args.text("on") != Some("sim") { return Err(NO_REAL.into()); }
    let wait = Duration::try_from_secs_f64(args.num("timeout").unwrap()).map_err(|_| "timeout: expected a nonnegative representable duration")?;
    let deadline = now().checked_add(wait).ok_or("timeout: duration is too large")?;
    let host = Host { requests, stop, tick, deadline, now };
    let mut counts = BTreeMap::<String, Repeats>::new();
    let Some(mut snapshot) = host.read(HostRequest::Expects)? else { return Ok(result(None, &counts, false, false)); };
    select(&mut snapshot, names(args).as_ref())?;
    // Pin the selection before any restart; a misspelt name must not restart the simulator.
    let names = snapshot.rows.iter().map(|row| row.name.clone()).collect();
    let mut current = false;
    for repeat in 1..=args.int("repeats").unwrap() as u64 {
        let Some(run) = host.read(HostRequest::Restart)? else { return Ok(result(Some(&snapshot), &counts, current, false)); };
        current = false;
        loop {
            let Some(mut next) = host.read(HostRequest::Expects)? else { return Ok(result(Some(&snapshot), &counts, current, false)); };
            // A restart is acknowledged before its first snapshot is published. Old runs do not count.
            if next.run == run {
                select(&mut next, Some(&names))?; snapshot = next; current = true;
                for row in &snapshot.rows {
                    let count = counts.entry(row.name.clone()).or_default();
                    if count.first_failure.is_none() {
                        if let Some((t, value)) = row.state.failed { count.first_failure = Some((repeat, t, value)); }
                    }
                }
                if snapshot.rows.iter().all(|row| final_row(row, snapshot.running)) {
                    for row in &snapshot.rows {
                        let count = counts.get_mut(&row.name).unwrap();
                        match row.state.outcome() {
                            Outcome::Pass => count.pass += 1,
                            Outcome::Fail => count.fail += 1,
                            Outcome::NotJudged => count.not_judged += 1,
                        }
                    }
                    break;
                }
            }
            host.tick();
        }
    }
    Ok(result(Some(&snapshot), &counts, current, true))
}

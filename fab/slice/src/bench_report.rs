//! The slicer bench's report (GPU-EXEC §11.2): where one scenario's time went against the
//! bound, from the GPU's dispatch intervals and the pool's task spans.
//!
//! The runners record [`Span`]s: every unit of work, on the CPU pool, the GPU, the encoder or a
//! readback, with when it became runnable, ran and ended and the spans it waited on. A layer the
//! GPU sends has no `after`: it is ready when it arrives. [`report`] then measures
//! - T: the wall time from the first unit's ready to the newest revision's last end (settled);
//! - G: the GPU's busy time, the union of its dispatch intervals (not their sum);
//! - C/N: the pool's task time over its N cores;
//! - E = T / max(G, C/N), and which of the two bounds it;
//! - each device's idle time while it still had work to come, by cause: a dependency, a
//!   readback in flight, a submission not yet encoded, CPU tasks not yet runnable;
//! - the critical path: the chain of last-finishing dependencies ending the scenario, with its
//!   hand-offs and queueing named;
//! - per part, the ms per layer of each stage on each device;
//! - for an edit, the layers resliced against those the edit changed, and the work spent on
//!   revisions a newer one superseded.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

/// The spans the pool records ([`makepad_fab_slicer::par::record_spans`]) and the GPU runtime
/// pushes ([`makepad_fab_slicer::par::push_span`]).
pub use makepad_fab_slicer::par::{Lane, Span, NO_LAYER, NO_PART};

/// A lane's device: the pool's workers are one device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Device {
    Cpu,
    Gpu,
    Encode,
    Readback,
}

/// The device `lane` is on.
pub fn device(lane: Lane) -> Device {
    match lane {
        Lane::Cpu(_) => Device::Cpu,
        Lane::Gpu => Device::Gpu,
        Lane::Encode => Device::Encode,
        Lane::Readback => Device::Readback,
    }
}

impl Device {
    pub fn name(self) -> &'static str {
        match self {
            Device::Cpu => "cpu",
            Device::Gpu => "gpu",
            Device::Encode => "encode",
            Device::Readback => "readback",
        }
    }
}

/// The pipeline's stages in order, for the per-part table (others follow by name).
pub const STAGES: [&str; 19] = ["trace", "outline", "walls", "paths", "faces", "skin", "shells", "bridges", "classes", "aux", "infill", "layer", "supports", "finish", "merge", "checks", "gcode", "package", "decode"];

fn ns(s: &Span) -> u64 {
    s.end_ns.saturating_sub(s.start_ns)
}

fn what(s: &Span) -> String {
    format!("{} {}", device(s.lane).name(), s.stage)
}

/// A device's idle time while work for it was still to come, by cause.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Idle {
    /// For the pool: core-ns (an idle core for 1 ms is 1 ms).
    pub ns: u64,
    /// (cause, ns), the largest first.
    pub causes: Vec<(String, u64)>,
    /// The longest windows with the whole device idle: (from, to) ns after the start, and the
    /// window's main cause.
    pub stalls: Vec<(u64, u64, String)>,
}

/// One step of the critical path: a stage on a device, a hand-off or queueing, merged with
/// its neighbours of the same kind.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub what: String,
    pub count: usize,
    pub ns: u64,
}

/// One part's stage on one device.
#[derive(Clone, Debug, PartialEq)]
pub struct StageCost {
    pub part: u32,
    pub stage: &'static str,
    pub device: Device,
    pub ns: u64,
    /// The distinct layers it ran for (0: a part-level stage).
    pub layers: usize,
}

impl StageCost {
    pub fn ms_per_layer(&self) -> f64 {
        ms(self.ns) / self.layers.max(1) as f64
    }
}

/// One scenario's report; its `Display` is the bench's text, [`Report::line`] its one line.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub name: String,
    pub cores: usize,
    /// The newest revision: what "settled" means.
    pub revision: u64,
    pub t_ns: u64,
    pub g_ns: u64,
    /// The pool's task time (C; C/N is [`Report::c_per_core_ns`]).
    pub c_ns: u64,
    pub encode_ns: u64,
    pub readback_ns: u64,
    pub gpu_idle: Idle,
    pub cpu_idle: Idle,
    pub path_ns: u64,
    pub path: Vec<Step>,
    pub stages: Vec<StageCost>,
    /// Distinct (part, layer)s the newest revision worked on.
    pub resliced: usize,
    /// The layers the edit changed (an edit scenario; see [`Report::changed`]).
    pub changed: Option<usize>,
    /// The CPU and GPU time spent on superseded revisions.
    pub superseded_cpu_ns: u64,
    pub superseded_gpu_ns: u64,
}

fn ms(ns: u64) -> f64 {
    ns as f64 * 1e-6
}

impl Report {
    pub fn named(mut self, name: &str) -> Self {
        self.name = name.into();
        self
    }
    /// An edit scenario: `layers` is how many layers the edit changed.
    pub fn changed(mut self, layers: usize) -> Self {
        self.changed = Some(layers);
        self
    }
    pub fn c_per_core_ns(&self) -> u64 {
        self.c_ns / self.cores.max(1) as u64
    }
    /// The bound, max(G, C/N), and which it is.
    pub fn bound(&self) -> (&'static str, u64) {
        let c = self.c_per_core_ns();
        if self.g_ns >= c { ("GPU", self.g_ns) } else { ("CPU", c) }
    }
    /// E = T / max(G, C/N): 1 when the scenario runs at its device bound.
    pub fn e(&self) -> Option<f64> {
        let (_, b) = self.bound();
        (b > 0).then(|| self.t_ns as f64 / b as f64)
    }
    /// The scenario in one line.
    pub fn line(&self) -> String {
        let (bound, _) = self.bound();
        let e = self.e().map_or("-".into(), |e| format!("{e:.2}"));
        format!(
            "bench {}: T {:.1} ms, G {:.1} ms, C/N {:.1} ms ({} cores), E {e} ({bound} bound), critical path {:.1} ms",
            self.name,
            ms(self.t_ns),
            ms(self.g_ns),
            ms(self.c_per_core_ns()),
            self.cores,
            ms(self.path_ns)
        )
    }
}

/// The causes, largest first: the first [`CAUSES_SHOWN`], then the rest summed.
fn causes(c: &[(String, u64)]) -> String {
    let mut out: Vec<String> = c.iter().take(CAUSES_SHOWN).map(|(what, ns)| format!("{what} {:.1}", ms(*ns))).collect();
    if c.len() > CAUSES_SHOWN {
        out.push(format!("{} more {:.1}", c.len() - CAUSES_SHOWN, ms(c[CAUSES_SHOWN..].iter().map(|x| x.1).sum())));
    }
    out.join(", ")
}

/// How many causes a line names.
const CAUSES_SHOWN: usize = 6;

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.line())?;
        for (name, unit, idle) in [("gpu", "ms", &self.gpu_idle), ("cpu", "core-ms", &self.cpu_idle)] {
            if idle.ns == 0 {
                continue;
            }
            write!(f, "  {name} idle {:.1} {unit} with work to come: {}", ms(idle.ns), causes(&idle.causes))?;
            if !idle.stalls.is_empty() {
                let stalls: Vec<String> = idle.stalls.iter().map(|(a, b, c)| format!("{:.1}-{:.1} ms ({c})", ms(*a), ms(*b))).collect();
                write!(f, "; all idle {}", stalls.join(", "))?;
            }
            writeln!(f)?;
        }
        if self.encode_ns + self.readback_ns > 0 {
            writeln!(f, "  encode {:.1} ms, readback in flight {:.1} ms", ms(self.encode_ns), ms(self.readback_ns))?;
        }
        if !self.path.is_empty() {
            let mut by: BTreeMap<&str, u64> = BTreeMap::new();
            for s in &self.path {
                *by.entry(&s.what).or_default() += s.ns;
            }
            let mut by: Vec<(String, u64)> = by.into_iter().map(|(w, ns)| (w.to_string(), ns)).collect();
            by.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            writeln!(f, "  critical path {:.1} ms: {}", ms(self.path_ns), causes(&by))?;
            let step = |s: &Step| if s.count > 1 { format!("{} x{} {:.1}", s.what, s.count, ms(s.ns)) } else { format!("{} {:.1}", s.what, ms(s.ns)) };
            let chain: Vec<String> = if self.path.len() <= 12 {
                self.path.iter().map(step).collect()
            } else {
                self.path[..8].iter().map(step).chain(std::iter::once(format!("({} more)", self.path.len() - 12))).chain(self.path[self.path.len() - 4..].iter().map(step)).collect()
            };
            writeln!(f, "  chain: {}", chain.join(" > "))?;
        }
        let mut parts: BTreeMap<u32, Vec<&StageCost>> = BTreeMap::new();
        for s in &self.stages {
            parts.entry(s.part).or_default().push(s);
        }
        for (part, stages) in parts {
            let cells: Vec<String> = stages
                .iter()
                .map(|s| if s.layers > 0 { format!("{} {} {:.2}", s.stage, s.device.name(), s.ms_per_layer()) } else { format!("{} {} {:.1} ms", s.stage, s.device.name(), ms(s.ns)) })
                .collect();
            let layers = stages.iter().map(|s| s.layers).max().unwrap_or(0);
            let part = if part == NO_PART { "the plate".to_string() } else { format!("part {part}") };
            writeln!(f, "  {part} ms/layer over {layers} layers: {}", cells.join(", "))?;
        }
        if let Some(c) = self.changed {
            let ratio = if c > 0 { format!("{:.1}x", self.resliced as f64 / c as f64) } else { "-".into() };
            writeln!(f, "  edit: resliced {} layers for {c} changed ({ratio})", self.resliced)?;
        }
        if self.superseded_cpu_ns + self.superseded_gpu_ns > 0 {
            writeln!(f, "  superseded revisions: cpu {:.1} ms, gpu {:.1} ms", ms(self.superseded_cpu_ns), ms(self.superseded_gpu_ns))?;
        }
        Ok(())
    }
}

/// The report of `spans` on a pool of `cores`, from the first ready to the newest revision's
/// last end.
pub fn report(spans: &[Span], cores: usize) -> Report {
    let start = spans.iter().map(|s| s.ready_ns.min(s.start_ns)).min().unwrap_or(0);
    let revision = spans.iter().map(|s| s.revision).max().unwrap_or(0);
    let settled = spans.iter().filter(|s| s.revision == revision).map(|s| s.end_ns).max().unwrap_or(start);
    report_window(spans, cores, start, settled)
}

/// [`report`] over a window the runner measured: from `start_ns` (the scenario's input) to
/// `settled_ns` (its settled event). The window's work is what began before the settle, cut at
/// it: what runs after it (the sliced printability) is not the settle's, and the critical path
/// ends in work that ended by it.
pub fn report_window(spans: &[Span], cores: usize, start_ns: u64, settled_ns: u64) -> Report {
    let ended: Vec<Span> = spans.iter().filter(|s| s.end_ns <= settled_ns).cloned().collect();
    let spans: Vec<Span> = spans.iter().filter(|s| s.start_ns < settled_ns).map(|s| Span { end_ns: s.end_ns.min(settled_ns), ..s.clone() }).collect();
    let spans = &spans[..];
    let by_id: HashMap<u64, &Span> = spans.iter().map(|s| (s.id, s)).collect();
    let revision = spans.iter().map(|s| s.revision).max().unwrap_or(0);
    let on = |d: Device| spans.iter().filter(move |s| device(s.lane) == d);
    let total = |d: Device| on(d).map(ns).sum::<u64>();
    let gpu: Vec<&Span> = on(Device::Gpu).collect();
    let cpu: Vec<&Span> = on(Device::Cpu).collect();
    let superseded = |d: Device| on(d).filter(|s| s.revision < revision).map(ns).sum::<u64>();
    let (path_ns, path) = critical_path(&ended, &by_id, revision, start_ns);
    Report {
        name: String::new(),
        cores: cores.max(1),
        revision,
        t_ns: settled_ns.saturating_sub(start_ns),
        g_ns: union(&gpu),
        c_ns: total(Device::Cpu),
        encode_ns: total(Device::Encode),
        readback_ns: total(Device::Readback),
        gpu_idle: idle(&gpu, spans, &by_id, 1, true, start_ns, settled_ns),
        cpu_idle: idle(&cpu, spans, &by_id, cores.max(1), false, start_ns, settled_ns),
        path_ns,
        path,
        stages: stage_costs(spans),
        resliced: spans.iter().filter(|s| s.revision == revision && s.layer != NO_LAYER).map(|s| (s.part, s.layer)).collect::<BTreeSet<_>>().len(),
        changed: None,
        superseded_cpu_ns: superseded(Device::Cpu),
        superseded_gpu_ns: superseded(Device::Gpu),
    }
}

/// The length of the union of the spans' run intervals.
fn union(spans: &[&Span]) -> u64 {
    let mut iv: Vec<(u64, u64)> = spans.iter().map(|s| (s.start_ns, s.end_ns)).filter(|(a, b)| b > a).collect();
    iv.sort_unstable();
    let (mut sum, mut cur) = (0, None::<(u64, u64)>);
    for (a, b) in iv {
        cur = match cur {
            Some((ca, cb)) if a <= cb => Some((ca, cb.max(b))),
            Some((ca, cb)) => {
                sum += cb - ca;
                Some((a, b))
            }
            None => Some((a, b)),
        };
    }
    sum + cur.map_or(0, |(a, b)| b - a)
}

/// What `s` waits on at `t`: its last-finishing dependency, or, while that has not started
/// either, what that one waits on.
fn waits_on(s: &Span, by_id: &HashMap<u64, &Span>, t: u64, depth: usize) -> String {
    let Some(dep) = s.after.iter().filter_map(|id| by_id.get(id)).max_by_key(|d| (d.end_ns, d.id)) else {
        return format!("{} not yet requested", s.stage);
    };
    if dep.end_ns <= t {
        return format!("hand-off after {}", what(dep));
    }
    if dep.start_ns > t && depth < 64 {
        return if dep.ready_ns > t { waits_on(dep, by_id, t, depth + 1) } else { format!("{} queued", what(dep)) };
    }
    match device(dep.lane) {
        Device::Readback => "readback in flight".to_string(),
        Device::Encode => "submission not yet encoded".to_string(),
        _ => format!("dependency wait on {}", what(dep)),
    }
}

/// Why a unit of `device` is not runnable at `t`.
fn blocked(device: Device, s: &Span, by_id: &HashMap<u64, &Span>, t: u64) -> String {
    let waits = waits_on(s, by_id, t, 0);
    match device {
        Device::Cpu => format!("CPU tasks not yet runnable: {waits}"),
        _ => waits,
    }
}

/// A device's idle time while it had work to come (a unit that starts later), by cause. `cap`
/// units run at once (the pool's cores); `whole`: the device is busy while any unit runs.
fn idle(spans: &[&Span], all: &[Span], by_id: &HashMap<u64, &Span>, cap: usize, whole: bool, lo: u64, hi: u64) -> Idle {
    let Some(last_start) = spans.iter().map(|s| s.start_ns).max() else { return Idle::default() };
    let hi = hi.min(last_start);
    if hi <= lo {
        return Idle::default();
    }
    // (time, busy delta, runnable delta); every span's start and end splits the causes
    let mut ev: Vec<(u64, i64, i64)> = Vec::with_capacity(spans.len() * 4 + all.len() * 2);
    for s in all {
        ev.push((s.start_ns, 0, 0));
        ev.push((s.end_ns, 0, 0));
    }
    for s in spans {
        ev.push((s.start_ns, 1, 0));
        ev.push((s.end_ns, -1, 0));
        if s.ready_ns < s.start_ns {
            ev.push((s.ready_ns, 0, 1));
            ev.push((s.start_ns, 0, -1));
        }
    }
    ev.sort_unstable();
    let mut by_ready: Vec<&Span> = spans.to_vec();
    by_ready.sort_by_key(|s| (s.ready_ns, s.start_ns));
    let runnable_cause = if whole { "submission not yet encoded" } else { "runnable, not started" };
    let (mut busy, mut runnable, mut next, mut i) = (0i64, 0i64, 0usize, 0usize);
    let mut causes: HashMap<String, u64> = HashMap::new();
    let mut out = Idle::default();
    let mut stall: Option<(u64, HashMap<String, u64>)> = None;
    let mut stalls: Vec<(u64, u64, String)> = Vec::new();
    let close = |stall: &mut Option<(u64, HashMap<String, u64>)>, t: u64, stalls: &mut Vec<(u64, u64, String)>| {
        if let Some((a, c)) = stall.take() {
            let main = c.into_iter().max_by(|x, y| x.1.cmp(&y.1).then(y.0.cmp(&x.0))).map_or(String::new(), |c| c.0);
            stalls.push((a - lo, t - lo, main));
        }
    };
    let mut t = lo;
    while t < hi {
        while i < ev.len() && ev[i].0 <= t {
            busy += ev[i].1;
            runnable += ev[i].2;
            i += 1;
        }
        let t2 = ev.get(i).map_or(hi, |e| e.0).min(hi);
        let dt = t2 - t;
        let idle_units = if whole { (busy <= 0) as u64 } else { (cap as i64 - busy).max(0) as u64 };
        if idle_units > 0 && dt > 0 {
            while next < by_ready.len() && by_ready[next].ready_ns <= t {
                next += 1;
            }
            let ready_units = idle_units.min(runnable.max(0) as u64);
            let rest = idle_units - ready_units;
            let add = |what: String, ns: u64, causes: &mut HashMap<String, u64>, stall: &mut Option<(u64, HashMap<String, u64>)>| {
                *causes.entry(what.clone()).or_default() += ns;
                if let Some((_, c)) = stall {
                    *c.entry(what).or_default() += ns;
                }
            };
            let full = idle_units == if whole { 1 } else { cap as u64 };
            if full && stall.is_none() {
                stall = Some((t, HashMap::new()));
            } else if !full {
                close(&mut stall, t, &mut stalls);
            }
            if ready_units > 0 {
                add(runnable_cause.to_string(), ready_units * dt, &mut causes, &mut stall);
            }
            if rest > 0 {
                let why = match by_ready.get(next) {
                    Some(s) => blocked(if whole { Device::Gpu } else { Device::Cpu }, s, by_id, t),
                    None => "too few runnable tasks".to_string(),
                };
                add(why, rest * dt, &mut causes, &mut stall);
            }
            out.ns += idle_units * dt;
        } else {
            close(&mut stall, t, &mut stalls);
        }
        t = t2;
    }
    close(&mut stall, hi, &mut stalls);
    stalls.sort_by(|a, b| (b.1 - b.0).cmp(&(a.1 - a.0)).then(a.0.cmp(&b.0)));
    stalls.truncate(3);
    out.stalls = stalls;
    out.causes = causes.into_iter().collect();
    out.causes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// The chain of last-finishing dependencies back from the newest revision's last span, as
/// merged steps, and its length from `start`. The plate's own work (merge, G-code, package)
/// names no dependencies: it runs once the plate's work before it is done, so its
/// predecessor is its revision's span that ended last before it became ready.
fn critical_path(spans: &[Span], by_id: &HashMap<u64, &Span>, revision: u64, start: u64) -> (u64, Vec<Step>) {
    let Some(last) = spans.iter().filter(|s| s.revision == revision).max_by_key(|s| (s.end_ns, s.id)) else { return (0, Vec::new()) };
    let before = |s: &Span| spans.iter().filter(|d| d.revision == s.revision && d.id != s.id && d.end_ns <= s.ready_ns).max_by_key(|d| (d.end_ns, d.id));
    let mut chain = vec![last];
    let mut cur = last;
    while chain.len() <= spans.len() {
        let dep = match cur.after.iter().filter_map(|id| by_id.get(id)).max_by_key(|d| (d.end_ns, d.id)) {
            Some(d) => Some(*d),
            None if cur.part == NO_PART => before(cur),
            None => None,
        };
        let Some(dep) = dep else { break };
        chain.push(dep);
        cur = dep;
    }
    chain.reverse();
    let mut steps: Vec<Step> = Vec::new();
    let mut push = |what: String, ns: u64| {
        if ns == 0 {
            return;
        }
        match steps.last_mut() {
            Some(s) if s.what == what => {
                s.count += 1;
                s.ns += ns;
            }
            _ => steps.push(Step { what, count: 1, ns }),
        }
    };
    let first = chain[0];
    push(format!("{} not yet requested", first.stage), first.ready_ns.saturating_sub(start));
    let mut prev: Option<&Span> = None;
    for s in &chain {
        if let Some(p) = prev {
            push(format!("hand-off {} to {}", device(p.lane).name(), device(s.lane).name()), s.ready_ns.saturating_sub(p.end_ns));
        }
        push(format!("queued {}", device(s.lane).name()), s.start_ns.saturating_sub(s.ready_ns));
        push(what(s), ns(s));
        prev = Some(s);
    }
    (last.end_ns.saturating_sub(start), steps)
}

/// Per part, each stage's time on each device and the layers it ran for.
fn stage_costs(spans: &[Span]) -> Vec<StageCost> {
    let mut by: HashMap<(u32, &'static str, Device), (u64, BTreeSet<u32>)> = HashMap::new();
    for s in spans {
        let e = by.entry((s.part, s.stage, device(s.lane))).or_default();
        e.0 += ns(s);
        if s.layer != NO_LAYER {
            e.1.insert(s.layer);
        }
    }
    let rank = |stage: &str| STAGES.iter().position(|&x| x == stage).unwrap_or(STAGES.len());
    let mut out: Vec<StageCost> = by.into_iter().map(|((part, stage, device), (ns, layers))| StageCost { part, stage, device, ns, layers: layers.len() }).collect();
    out.sort_by(|a, b| (a.part, rank(a.stage), a.stage, a.device).cmp(&(b.part, rank(b.stage), b.stage, b.device)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn span(id: u64, lane: Lane, (ready, start, end): (u64, u64, u64), after: &[u64], stage: &'static str, layer: u32) -> Span {
        Span { id, lane, ready_ns: ready * MS, start_ns: start * MS, end_ns: end * MS, after: after.to_vec(), stage, part: 0, layer, revision: 1 }
    }

    /// Three layers: each traced on the GPU (4 ms), read back (1 ms) and walled on one of two
    /// cores (5 ms). The GPU encodes layer 2 late, at 12 ms.
    fn pipeline() -> Vec<Span> {
        vec![
            span(1, Lane::Gpu, (0, 0, 4), &[], "trace", 0),
            span(2, Lane::Readback, (4, 4, 5), &[1], "trace", 0),
            span(3, Lane::Cpu(0), (5, 5, 10), &[2], "walls", 0),
            span(4, Lane::Gpu, (0, 4, 8), &[], "trace", 1),
            span(5, Lane::Readback, (8, 8, 9), &[4], "trace", 1),
            span(6, Lane::Cpu(1), (9, 9, 14), &[5], "walls", 1),
            span(7, Lane::Encode, (8, 8, 12), &[], "trace", 2),
            span(8, Lane::Gpu, (12, 12, 16), &[7], "trace", 2),
            span(9, Lane::Readback, (16, 16, 17), &[8], "trace", 2),
            span(10, Lane::Cpu(0), (17, 17, 22), &[9], "walls", 2),
            span(11, Lane::Cpu(1), (22, 22, 24), &[3, 6, 10], "merge", NO_LAYER),
        ]
    }

    #[test]
    fn totals_bound_and_efficiency() {
        let r = report(&pipeline(), 2);
        assert_eq!(r.t_ns, 24 * MS);
        assert_eq!(r.g_ns, 12 * MS);
        assert_eq!(r.c_ns, 17 * MS);
        assert_eq!(r.c_per_core_ns(), 8_500_000);
        assert_eq!(r.bound(), ("GPU", 12 * MS));
        assert!((r.e().unwrap() - 2.0).abs() < 1e-9);
        assert_eq!((r.encode_ns, r.readback_ns), (4 * MS, 3 * MS));
        assert_eq!(r.resliced, 3);
        assert!(r.line().starts_with("bench : T 24.0 ms, G 12.0 ms, C/N 8.5 ms (2 cores), E 2.00 (GPU bound)"), "{}", r.line());
    }

    #[test]
    fn the_gpu_is_a_union_of_its_dispatches() {
        let mut s = pipeline();
        s.push(span(12, Lane::Gpu, (0, 2, 6), &[], "trace", 3));
        assert_eq!(report(&s, 2).g_ns, 12 * MS);
    }

    #[test]
    fn idle_gaps_name_their_cause() {
        let r = report(&pipeline(), 2);
        // the GPU idles 8..12 ms waiting for layer 2's encoding; after 12 ms it has nothing to come
        assert_eq!(r.gpu_idle.ns, 4 * MS);
        assert_eq!(r.gpu_idle.causes, vec![("submission not yet encoded".to_string(), 4 * MS)]);
        assert_eq!(r.gpu_idle.stalls, vec![(8 * MS, 12 * MS, "submission not yet encoded".to_string())]);
        // the cores, to the last CPU start at 22 ms: both wait 0..5 (the GPU, then the readback), one
        // 5..9 (the same for layer 1), one 10..14 (layer 2's encoding, then its trace), both 14..17
        // (its trace, then its readback) and one 17..22 (the merge waits on the last walls)
        let causes: Vec<(&str, u64)> = r.cpu_idle.causes.iter().map(|(c, ns)| (c.strip_prefix("CPU tasks not yet runnable: ").expect("CPU idle is waiting"), ns / MS)).collect();
        assert_eq!(causes, vec![("dependency wait on gpu trace", 2 * 4 + 3 + 2 + 2 * 2), ("dependency wait on cpu walls", 5), ("readback in flight", 2 + 1 + 2), ("submission not yet encoded", 2)]);
        assert_eq!(r.cpu_idle.ns, 29 * MS);
        let stalls: Vec<(u64, u64, &str)> = r.cpu_idle.stalls.iter().map(|(a, b, c)| (a / MS, b / MS, c.as_str())).collect();
        assert_eq!(stalls, vec![(0, 5, "CPU tasks not yet runnable: dependency wait on gpu trace"), (14, 17, "CPU tasks not yet runnable: dependency wait on gpu trace")]);
        let text = r.to_string();
        assert!(text.contains("gpu idle 4.0 ms with work to come: submission not yet encoded 4.0; all idle 8.0-12.0 ms (submission not yet encoded)"), "{text}");
    }

    #[test]
    fn the_critical_path_follows_the_last_dependency() {
        let r = report(&pipeline(), 2);
        assert_eq!(r.path_ns, 24 * MS);
        // merge <- layer 2's walls <- its readback <- its trace <- its late encoding
        let what: Vec<(&str, u64)> = r.path.iter().map(|s| (s.what.as_str(), s.ns / MS)).collect();
        assert_eq!(what, vec![("trace not yet requested", 8), ("encode trace", 4), ("gpu trace", 4), ("readback trace", 1), ("cpu walls", 5), ("cpu merge", 2)]);
        assert!(r.to_string().contains("chain: trace not yet requested 8.0 > encode trace 4.0 > gpu trace 4.0 > readback trace 1.0 > cpu walls 5.0 > cpu merge 2.0"), "{r}");
    }

    #[test]
    fn the_plates_own_work_follows_what_ended_before_it() {
        let mut s = pipeline();
        // the package, the plate's own work, named no dependency: it follows the merge
        s.push(Span { id: 12, lane: Lane::Cpu(0), ready_ns: 25 * MS, start_ns: 25 * MS, end_ns: 28 * MS, after: vec![], stage: "package", part: NO_PART, layer: NO_LAYER, revision: 1 });
        let r = report(&s, 2);
        assert_eq!(r.path_ns, 28 * MS);
        assert!(r.to_string().contains("> cpu walls 5.0 > cpu merge 2.0 > hand-off cpu to cpu 1.0 > cpu package 3.0"), "{r}");
        assert!(r.to_string().contains("the plate ms/layer over 0 layers: package cpu 3.0 ms"), "{r}");
    }

    #[test]
    fn per_part_stage_costs_per_layer() {
        let r = report(&pipeline(), 2);
        let walls = r.stages.iter().find(|s| s.stage == "walls").unwrap();
        assert_eq!((walls.device, walls.layers, walls.ms_per_layer()), (Device::Cpu, 3, 5.0));
        let trace: Vec<(Device, f64)> = r.stages.iter().filter(|s| s.stage == "trace").map(|s| (s.device, s.ms_per_layer())).collect();
        assert_eq!(trace, vec![(Device::Gpu, 4.0), (Device::Encode, 4.0), (Device::Readback, 1.0)]);
        let merge = r.stages.iter().find(|s| s.stage == "merge").unwrap();
        assert_eq!(merge.layers, 0);
        assert_eq!(r.stages.iter().map(|s| s.stage).collect::<Vec<_>>(), vec!["trace", "trace", "trace", "walls", "merge"]);
        assert!(r.to_string().contains("part 0 ms/layer over 3 layers: trace gpu 4.00, trace encode 4.00, trace readback 1.00, walls cpu 5.00, merge cpu 2.0 ms"), "{r}");
    }

    #[test]
    fn an_edit_reports_resliced_layers_and_superseded_work() {
        let mut s = pipeline();
        for x in &mut s {
            x.revision = 1;
        }
        // revision 2 reslices layers 1 and 2 after the edit changed layer 2
        s.push(Span { id: 20, lane: Lane::Cpu(0), ready_ns: 24 * MS, start_ns: 24 * MS, end_ns: 27 * MS, after: vec![], stage: "walls", part: 0, layer: 1, revision: 2 });
        s.push(Span { id: 21, lane: Lane::Cpu(1), ready_ns: 24 * MS, start_ns: 24 * MS, end_ns: 28 * MS, after: vec![], stage: "walls", part: 0, layer: 2, revision: 2 });
        s.push(Span { id: 22, lane: Lane::Cpu(1), ready_ns: 28 * MS, start_ns: 28 * MS, end_ns: 29 * MS, after: vec![20, 21], stage: "merge", part: 0, layer: NO_LAYER, revision: 2 });
        let r = report(&s, 2).named("part drag").changed(1);
        assert_eq!(r.revision, 2);
        assert_eq!(r.t_ns, 29 * MS);
        assert_eq!(r.resliced, 2);
        assert_eq!((r.superseded_cpu_ns, r.superseded_gpu_ns), (17 * MS, 12 * MS));
        let text = r.to_string();
        assert!(text.starts_with("bench part drag: T 29.0 ms"), "{text}");
        assert!(text.contains("edit: resliced 2 layers for 1 changed (2.0x)"), "{text}");
        assert!(text.contains("superseded revisions: cpu 17.0 ms, gpu 12.0 ms"), "{text}");
        // (the report's window starts at the first revision's first unit: a runner passes the edit's time to report_window)
        assert!(text.contains("chain: walls not yet requested 24.0 > cpu walls 4.0 > cpu merge 1.0"), "{text}");
    }

    #[test]
    fn the_window_ends_at_the_settle() {
        let mut s = pipeline();
        // checks after the settle at 24 ms, and a check that began before it
        s.push(span(12, Lane::Cpu(0), (24, 24, 30), &[11], "checks", NO_LAYER));
        s.push(span(13, Lane::Cpu(0), (20, 20, 26), &[], "checks", NO_LAYER));
        let r = report_window(&s, 2, 0, 24 * MS);
        assert_eq!((r.t_ns, r.c_ns, r.path_ns), (24 * MS, 21 * MS, 24 * MS));
        assert_eq!(r.path.last().map(|s| s.what.as_str()), Some("cpu merge"));
    }

    #[test]
    fn a_cpu_only_scenario_is_cpu_bound() {
        let s: Vec<Span> = (0..8).map(|k| span(k, Lane::Cpu((k % 4) as u16), (0, 2 * (k / 4), 2 * (k / 4) + 2), &[], "walls", k as u32)).collect();
        let r = report(&s, 4).named("cpu only");
        assert_eq!((r.t_ns, r.g_ns, r.c_per_core_ns()), (4 * MS, 0, 4 * MS));
        assert_eq!(r.bound().0, "CPU");
        assert!((r.e().unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(r.cpu_idle.ns, 0);
        assert_eq!(r.gpu_idle, Idle::default());
        assert!(report(&[], 4).e().is_none());
    }
}

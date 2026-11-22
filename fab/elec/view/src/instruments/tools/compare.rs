//! Cached edge comparisons in expected trigger-relative time, without reading samples.

use std::collections::VecDeque;
use fab_elec_bench::align::{self, Alignment, Difference, Edge, Kind, Map};
use fab_elec_bench::source::Quantity;
use fab_elec_circuit::QtyUnit;
use super::call::{number, Arguments};
use super::capture::Capture;
use super::{features, query};
use super::reply::{self, list, percent, Object, ToolReply};

const CACHE_ENTRIES: usize = 4;

struct Compared {
    key: String,
    expected: String,
    actual: String,
    alignment: Alignment,
    method: String,
    runner_up: Option<usize>,
    summary: Object,
    differences: Vec<(usize, Difference)>,
}

/// FIFO of immutable comparisons; cursors remain valid after eviction and recomputation.
#[derive(Default)]
pub(super) struct Cache {
    entries: VecDeque<Compared>,
}
impl Cache {
    /// Invalidate comparisons involving either side of a dropped capture.
    pub(super) fn drop_capture(&mut self, id: &str) {
        self.entries.retain(|entry| entry.expected != id && entry.actual != id);
    }
}

fn channel(capture: &Capture, id: u32, side: &str) -> Result<usize, String> {
    capture.layout.channels.iter().position(|c| c.id.0 == id).ok_or_else(|| format!("channels: unknown {side} channel {id}"))
}
fn channels(expected: &Capture, actual: &Capture, args: &Arguments) -> Result<Vec<(usize, usize)>, String> {
    let text = args.text("channels").unwrap_or("").trim();
    // the channels of one net (a saved take and a capture of the run name a net alike), or by
    // position when no net is in both
    let named = || expected.layout.channels.iter().enumerate()
        .filter_map(|(e, info)| actual.layout.channels.iter().position(|other| other.name == info.name).map(|a| (e, a))).collect::<Vec<_>>();
    let pairs = if text.is_empty() {
        Some(named()).filter(|pairs| !pairs.is_empty())
            .unwrap_or_else(|| (0..expected.layout.channels.len().min(actual.layout.channels.len())).map(|c| (c, c)).collect())
    }
    else {
        let mut pairs = Vec::new();
        for pair in text.split(',') {
            let (e, a) = pair.trim().split_once(':').ok_or("channels: expected channel pairs such as 0:0,1:1")?;
            let e = e.trim().parse::<u32>().map_err(|_| "channels: expected an expected channel id before ':'")?;
            let a = a.trim().parse::<u32>().map_err(|_| "channels: expected an actual channel id after ':'")?;
            let pair = (channel(expected, e, "expected")?, channel(actual, a, "actual")?);
            if !pairs.contains(&pair) { pairs.push(pair); }
        }
        pairs
    };
    if pairs.is_empty() { return Err("channels: select at least one channel pair".into()); }
    for &(e, a) in &pairs {
        for (capture, c, side) in [(expected, e, "expected"), (actual, a, "actual")] {
            let info = &capture.layout.channels[c];
            if !matches!(info.quantity, Quantity::Volt | Quantity::Logic) {
                return Err(format!("channels: {side} channel {} requires voltage or logic", info.id.0));
            }
        }
    }
    Ok(pairs)
}
fn actual_span(edges: &[Edge], span: (f64, f64), start: Map, window: f64) -> &[Edge] {
    let from = start.offset + start.gain * span.0 - window;
    let to = start.offset + start.gain * span.1 + window;
    let first = edges.partition_point(|e| e.t < from); let end = edges.partition_point(|e| e.t <= to);
    &edges[first..end]
}
fn slices<'a>(edges: &'a [(Vec<Edge>, Vec<Edge>)], span: (f64, f64), start: Map, tolerance: f64) -> Vec<(&'a [Edge], &'a [Edge])> {
    edges.iter().map(|(e, a)| (e.as_slice(), actual_span(a, span, start, align::search_window(e, tolerance)))).collect()
}
fn anchored(channels: &[(&[Edge], &[Edge])], map: Map, tolerance: f64) -> Alignment {
    let channels: Vec<_> = channels.iter().map(|&(e, a)| align::match_edges(e, a, map, align::search_window(e, tolerance))).collect();
    Alignment { map, rms: 0.0, matched: channels.iter().map(|c| c.pairs.len()).sum(), channels }
}
fn kind(kind: Kind) -> &'static str {
    match kind { Kind::Missing => "missing_edge", Kind::Extra => "extra_edge", Kind::Late => "late", Kind::Early => "early" }
}
fn difference(difference: &Difference) -> Object {
    let mut out = Object::default(); out.num("t", Some(difference.t), QtyUnit::Second);
    out.symbol("kind", kind(difference.kind)); out.symbol("dir", if difference.rising { "rising" } else { "falling" });
    if matches!(difference.kind, Kind::Late | Kind::Early) { out.num("dt", Some(difference.dt), QtyUnit::Second); }
    out
}
fn overlap(capture: &Capture, span: (f64, f64)) -> bool {
    capture.gaps.iter().any(|&(from, to, _)| from - capture.t_trigger < span.1 && to - capture.t_trigger > span.0)
}

fn compute(expected: &Capture, actual: &Capture, pairs: &[(usize, usize)], span: (f64, f64), tolerance: f64, args: &Arguments, key: String) -> Result<Compared, String> {
    let edges: Vec<_> = pairs.iter().map(|&(e, a)| (
        features::alignment_edges(expected, e, span.0 + expected.t_trigger, span.1 + expected.t_trigger),
        features::alignment_edges(actual, a, actual.span.0, actual.span.1),
    )).collect();
    let method = args.text("align").unwrap(); let skew = args.text("skew") == Some("fit");
    let mut start = Map { offset: 0.0, gain: 1.0 }; let mut runner_up = 0;
    if method == "event" {
        let anchor = args.text("anchor").unwrap(); let rising = anchor == "rising";
        let first = |edges: &[Edge], side: &str| edges.iter().find(|e| e.rising == rising).map(|e| e.t)
            .ok_or_else(|| format!("anchor: no {anchor} edge in {side}; use align: fit"));
        start.offset = first(&edges[0].1, "actual")? - first(&edges[0].0, "expected")?;
    }
    let mut alignment = if method == "fit" {
        let mut best: Option<(Alignment, Map)> = None; let mut seen = Vec::new();
        if let Some((e, a)) = edges.iter().find(|(e, a)| !e.is_empty() && !a.is_empty()) {
            // Each candidate has its own actual span; indices in its Matching refer to that slice.
            for edge in a.iter().filter(|a| a.rising == e[0].rising).take(16) {
                let candidate = Map { offset: edge.t - e[0].t, gain: 1.0 };
                let result = align::align(&slices(&edges, span, candidate, tolerance), candidate, tolerance, skew);
                seen.push((result.matched, result.map));
                if best.as_ref().is_none_or(|(best, _)| align::better(&result, best)) { best = Some((result, candidate)); }
            }
            if let Some((best, _)) = &best { runner_up = align::runner_up(&seen, best.map, align::search_window(e, tolerance)); }
        }
        if let Some((result, candidate)) = best { start = candidate; result }
        else { align::align(&slices(&edges, span, start, tolerance), start, tolerance, skew) }
    } else if skew { align::align(&slices(&edges, span, start, tolerance), start, tolerance, true) }
    else { anchored(&slices(&edges, span, start, tolerance), start, tolerance) };
    let lists = slices(&edges, span, start, tolerance);
    let mut squares = 0.0; let mut merged = Vec::new(); let mut summaries = Vec::new();
    for ((&(e, a), &(ei, ai)), matching) in lists.iter().zip(pairs).zip(&alignment.channels) {
        let differences = align::differences(e, a, matching, alignment.map, tolerance);
        for &(i, j) in &matching.pairs { squares += (a[j].t - (alignment.map.offset + alignment.map.gain * e[i].t)).powi(2); }
        let id = expected.layout.channels[ei].id.0 as usize;
        let mut out = Object::default(); out.int("expected", id); out.int("actual", actual.layout.channels[ai].id.0 as usize);
        out.int("matched", matching.pairs.len()); out.int("missing", matching.missing.len()); out.int("extra", matching.extra.len());
        out.int("late", differences.iter().filter(|d| d.kind == Kind::Late).count());
        out.int("early", differences.iter().filter(|d| d.kind == Kind::Early).count());
        if let Some(first) = differences.first() { out.field("first_difference", difference(first).render()); }
        summaries.push(out.render()); merged.extend(differences.into_iter().map(|d| (id, d)));
    }
    alignment.rms = if alignment.matched == 0 { 0.0 } else { (squares / alignment.matched as f64).sqrt() };
    merged.sort_unstable_by(|(ac, a), (bc, b)| a.t.total_cmp(&b.t).then_with(|| ac.cmp(bc))
        .then_with(|| (a.kind as u8).cmp(&(b.kind as u8))).then_with(|| a.rising.cmp(&b.rising)).then_with(|| a.dt.total_cmp(&b.dt)));
    let actual_span = (alignment.map.offset + alignment.map.gain * span.0, alignment.map.offset + alignment.map.gain * span.1);
    let egap = overlap(expected, span); let agap = overlap(actual, actual_span);
    let mut out = Object::default();
    out.symbol("verdict", if egap || agap { "not_judged" } else if merged.is_empty() { "pass" } else { "fail" });
    if egap || agap {
        out.text("reason", match (egap, agap) { (true, true) => "expected and actual captures have gaps overlapping the compared span",
            (true, false) => "expected capture has a gap overlapping the compared span", _ => "actual capture has a gap overlapping the compared span" });
    }
    let method = match (method, skew) { ("trigger", true) => "trigger_fit", ("event", true) => "event_fit", _ => method };
    let mut used = Object::default(); used.num("time", Some(tolerance), QtyUnit::Second); out.field("tolerance_used", used.render());
    out.field("channels", list(summaries));
    Ok(Compared { key, expected: expected.id.clone(), actual: actual.id.clone(), alignment, method: method.into(),
        runner_up: (method == "fit").then_some(runner_up), summary: out, differences: merged })
}

/// Compare cached auto edges once, then serve deterministic difference pages from a bounded cache.
pub(super) fn compare(expected: &Capture, actual: &Capture, args: &Arguments, budget: usize, cache: &mut Cache) -> Result<ToolReply, String> {
    let pairs = channels(expected, actual, args)?;
    let (from, to) = query::bounds(expected, args)?;
    let span = (from - expected.t_trigger, to - expected.t_trigger);
    let tolerance = args.text("tolerance").unwrap_or("").trim();
    let tolerance = if tolerance.is_empty() {
        2.0 * pairs.iter().map(|&(e, a)| expected.features[e].max_step.max(actual.features[a].max_step)).fold(0.0, f64::max)
    } else { number(tolerance, "s").map_err(|e| format!("tolerance: {e}"))? };
    if tolerance < 0.0 || !(2.0 * tolerance).is_finite() { return Err("tolerance: expected a finite nonnegative time with a finite search window".into()); }
    let selection = pairs.iter().map(|(e, a)| format!("{e}:{a}")).collect::<Vec<_>>().join(",");
    let key = query::key(expected, &format!("compare/{}/{selection}/{}/{}/{}/{:016x}", actual.id,
        args.text("align").unwrap(), args.text("anchor").unwrap(), args.text("skew").unwrap(), tolerance.to_bits()), from, to);
    let at = if let Some(at) = cache.entries.iter().position(|entry| entry.key == key) { at } else {
        let compared = compute(expected, actual, &pairs, span, tolerance, args, key)?;
        if cache.entries.len() == CACHE_ENTRIES { cache.entries.pop_front(); }
        cache.entries.push_back(compared); cache.entries.len() - 1
    };
    let compared = &cache.entries[at]; let total = compared.differences.len();
    let alignment = &compared.alignment;
    let mut fitted = Object::default();
    fitted.symbol("method", &compared.method);
    fitted.num("offset", Some(alignment.map.offset), QtyUnit::Second); fitted.field("skew", percent(100.0 * (alignment.map.gain - 1.0)));
    fitted.num("residual_rms", Some(alignment.rms), QtyUnit::Second); fitted.int("matched", alignment.matched);
    if let Some(runner_up) = compared.runner_up { fitted.int("runner_up", runner_up); }
    let start = reply::offset(args.text("after"), &compared.key, total)?;
    let limit = usize::try_from(args.int("limit").unwrap()).unwrap_or(usize::MAX);
    reply::page_values(budget, start, total, limit, &compared.key, |i| {
        let (channel, value) = &compared.differences[i]; let mut out = difference(value); out.int("channel", *channel); Ok(out.render())
    }, |items, next| {
        let mut out = compared.summary.clone(); out.field("alignment", fitted.render());
        out.field("differences", list(items.iter().cloned())); out.int("total", total);
        out.next(next); out.render()
    })
}

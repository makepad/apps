//! Answer-sized reads from immutable capture metadata and measurement indices.

use fab_elec_bench::measure_index::Reading;
use fab_elec_bench::source::*;
use fab_elec_bench::trigger::{Qualifier, Slope};
use fab_elec_circuit::QtyUnit;
use super::call::Arguments;
use super::capture::{Capture, CaptureSpec};
use super::reply::{self, list, percent, qty, quote, Object, ToolReply};
use super::super::fw_events::{self, FwEvents};

pub(super) fn side(side: Side) -> &'static str {
    match side { Side::Sim => "sim", Side::Real => "real", Side::Replay => "replay" }
}
pub(super) fn unit(quantity: Quantity) -> QtyUnit {
    match quantity { Quantity::Volt => QtyUnit::Volt, Quantity::Amp => QtyUnit::Ampere, Quantity::Logic => QtyUnit::Count }
}
fn span(span: (f64, f64)) -> String { list([qty(span.0, QtyUnit::Second), qty(span.1, QtyUnit::Second)]) }
fn gaps(capture: &Capture) -> String {
    list(capture.gaps.iter().map(|&(from, to, why)| {
        let mut out = Object::default();
        out.num("from", Some(from - capture.t_trigger), QtyUnit::Second);
        out.num("to", Some(to - capture.t_trigger), QtyUnit::Second);
        out.symbol("why", match why { Gap::Overrun => "overrun", Gap::Rearm => "rearm", Gap::Blind => "blind", Gap::Restart => "restart" });
        out.render()
    }))
}
pub(super) fn resolved(spec: &CaptureSpec, resolved: &Resolved) -> Object {
    let mut out = Object::default(); out.text("source", &spec.source); out.num("rate", Some(resolved.rate), QtyUnit::Hertz);
    out.field("depth", resolved.depth.to_string()); out.field("span", span(spec.span));
    out.field("ranges", list(resolved.channels.iter().map(|c| qty(c.range.1 - c.range.0, QtyUnit::Volt))));
    if resolved.channels.iter().any(|c| c.step != 0.0) {
        out.field("step", list(resolved.channels.iter().map(|c| qty(c.step, QtyUnit::Volt))));
    }
    let mut trigger = Object::default();
    trigger.symbol("kind", match spec.qualifier { Qualifier::None => &spec.trigger, Qualifier::Runt { .. } => "runt", _ => "width" }); trigger.text("on", &spec.on);
    trigger.num("level", Some(spec.level), QtyUnit::Volt);
    if let Qualifier::Runt { other } = spec.qualifier { trigger.num("runt_level", Some(other), QtyUnit::Volt); }
    trigger.symbol("slope", match spec.slope { Slope::Rising => "rise", Slope::Falling => "fall", Slope::Either => "either" });
    trigger.symbol("how", if resolved.trigger == TrigResolved::Hardware { "hw" } else { "sw" });
    let blind = match resolved.trigger { TrigResolved::Software { over: Over::Blocks { blind } } => blind as f64 * 100.0, _ => 0.0 };
    trigger.field("blind", percent(blind)); out.field("trigger", trigger.render());
    out.field("notes", list(resolved.notes.iter().map(|n| quote(&n.text))));
    out
}
pub(super) fn waiting(id: &str, spec: &CaptureSpec, setup: &Resolved) -> Object {
    let mut out = Object::default(); out.text("capture", id); out.symbol("state", "waiting");
    out.field("resolved", resolved(spec, setup).render()); out
}
pub(super) fn done(capture: &Capture) -> Object {
    let mut out = Object::default(); out.text("capture", &capture.id); out.symbol("state", "done");
    out.num("t_trigger", Some(capture.t_trigger), QtyUnit::Second); out.int("samples", capture.trace.len());
    if let Some(firmware) = &capture.firmware { out.int("firmware", firmware.len()); }
    out.field("span", span((capture.span.0 - capture.t_trigger, capture.span.1 - capture.t_trigger)));
    out.field("gaps", gaps(capture)); out.field("resolved", resolved(&capture.spec, &capture.resolved).render()); out
}
pub(super) fn list_item(capture: &Capture, saved: Option<&str>) -> String {
    let mut out = Object::default(); out.text("id", &capture.id); out.text("name", &capture.name);
    out.symbol("side", side(capture.side)); out.text("source", &capture.source);
    out.field("span", span((capture.span.0 - capture.t_trigger, capture.span.1 - capture.t_trigger)));
    out.int("channels", capture.layout.channels.len()); out.int("samples", capture.trace.len());
    if let Some(firmware) = &capture.firmware { out.int("firmware", firmware.len()); }
    if let Some(path) = saved { out.text("saved", path); } out.render()
}
pub(super) fn sources(sim: &Caps, synth: &Caps, sim_state: &str, synth_state: &str) -> ToolReply {
    let mut out = Object::default();
    out.field("sources", list([("sim", "Simulator", sim), ("synth", "Synthetic", synth)].into_iter().map(|(id, name, caps)| {
        let mut out = Object::default(); out.text("id", id); out.symbol("side", "sim"); out.text("name", name);
        out.symbol("state", if id == "sim" { sim_state } else { synth_state }); out.text("access", "ok");
        out.field("channels", list(caps.channels.iter().map(|c| {
            let mut out = Object::default(); out.int("id", c.id.0 as usize);
            out.symbol("kind", match c.quantity { Quantity::Volt => "volt", Quantity::Amp => "amp", Quantity::Logic => "logic" });
            let ranges = match &c.ranges { Ranges::Any => Vec::new(), Ranges::Steps(ranges) => ranges.iter().map(|r| qty(r.1 - r.0, unit(c.quantity))).collect() };
            out.field("range", list(ranges)); out.int("bits", c.bits as usize); out.render()
        })));
        let rates = match &caps.rate { RateCaps::Any => Vec::new(), RateCaps::List(rates) => rates.clone(), RateCaps::Range { lo, hi } => vec![*lo, *hi] };
        out.field("rates", list(rates.into_iter().map(|r| qty(r, QtyUnit::Hertz))));
        if let Depth::Samples(depth) = caps.depth { out.field("depth", depth.to_string()); }
        out.field("trigger", list([quote("edge sw"), quote("level sw"), quote("width sw")])); out.field("outputs", list(Vec::new()));
        out.field("cannot", list([quote("power requires the simulator Sink power extension")])); out.render()
    })));
    out.reply()
}
fn selected(capture: &Capture, args: &Arguments) -> Result<Vec<usize>, String> {
    let Some(text) = args.text("channels") else { return Ok((0..capture.layout.channels.len()).collect()) };
    let mut channels = Vec::new();
    for token in text.trim_matches(['[', ']']).split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()) {
        let id = token.parse::<u32>().map_err(|_| "channels: expected channel ids")?;
        let i = capture.layout.channels.iter().position(|c| c.id.0 == id).ok_or_else(|| format!("channels: unknown channel {id}"))?;
        if !channels.contains(&i) { channels.push(i); }
    }
    if channels.is_empty() { return Err("channels: select at least one channel".into()); }
    Ok(channels)
}
pub(super) fn channel(capture: &Capture, args: &Arguments) -> Result<usize, String> {
    let id = args.int("channel").unwrap() as u32;
    capture.layout.channels.iter().position(|c| c.id.0 == id).ok_or_else(|| format!("channel: unknown {id}"))
}
pub(super) fn bounds(capture: &Capture, args: &Arguments) -> Result<(f64, f64), String> {
    let relative = args.span((capture.span.0 - capture.t_trigger, capture.span.1 - capture.t_trigger))?;
    Ok(((relative.0 + capture.t_trigger).max(capture.span.0), (relative.1 + capture.t_trigger).min(capture.span.1)))
}
pub(super) fn key(capture: &Capture, query: &str, from: f64, to: f64) -> String {
    format!("{}/{query}/{:016x}/{:016x}", capture.id, from.to_bits(), to.to_bits())
}
fn spread(value: Reading, percent_unit: bool) -> String {
    let mut out = Object::default();
    if percent_unit { out.field("value", percent(value.value)); out.field("sigma", percent(value.sigma)); }
    else { out.num("value", Some(value.value), QtyUnit::Hertz); out.num("sigma", Some(value.sigma), QtyUnit::Hertz); }
    out.render()
}
/// A channel's display name (`Capture::labels`), when it has one.
fn label(out: &mut Object, capture: &Capture, c: usize) {
    if let Some(label) = capture.labels.get(c).filter(|label| !label.is_empty()) { out.text("label", label); }
}
fn summary_channel(capture: &Capture, c: usize, from: f64, to: f64) -> Object {
    let feature = &capture.features[c]; let index = &feature.index;
    let info = &capture.layout.channels[c]; let unit = unit(info.quantity);
    let mut out = Object::default(); out.int("channel", info.id.0 as usize); out.text("name", &info.name); label(&mut out, capture, c); out.symbol("class", feature.class);
    for what in ["min", "max", "mean", "rms"] { out.num(what, index.reading(what, from, to).map(|r| r.value), unit); }
    out.num("top", Some(index.top), unit); out.num("base", Some(index.base), unit);
    let (first, end) = index.edges.range(from, to); out.int("edges", end - first);
    if let Some(f) = index.reading("frequency", from, to) { out.field("frequency", spread(f, false)); }
    if let Some(d) = index.reading("duty", from, to) { out.field("duty", spread(d, true)); }
    if end > first {
        out.num("first_edge", Some(index.edges.items[first].from - capture.t_trigger), QtyUnit::Second);
        out.num("last_edge", Some(index.edges.items[end - 1].from - capture.t_trigger), QtyUnit::Second);
    }
    out.field("clipped", list(Vec::new()));
    let mut step = Object::default(); step.num("time", Some(index.steps(from, to).0), QtyUnit::Second);
    step.num("level", capture.resolved.channels.get(c).map(|c| c.step), unit); out.field("step", step.render());
    out
}
pub(super) fn summary(capture: &Capture, args: &Arguments, budget: usize) -> Result<ToolReply, String> {
    let channels = selected(capture, args)?;
    let (from, to) = bounds(capture, args)?;
    // Each range is found by binary search, even for a very long burst sequence.
    let ranges: Vec<_> = channels.iter().map(|&c| {
        let segments = &capture.features[c].segments;
        let first = segments.partition_point(|s| if from == to { s.to < from } else { s.to <= from });
        let end = segments.partition_point(|s| if from == to { s.from <= to } else { s.from < to });
        (c, first.min(end), end)
    }).collect();
    let total: usize = ranges.iter().map(|(_, a, b)| b - a).sum();
    let channel_key = channels.iter().map(usize::to_string).collect::<Vec<_>>().join(",");
    let key = key(capture, &format!("segments/{channel_key}"), from, to);
    let start = reply::offset(args.text("after"), &key, total)?;
    let locate = |mut i: usize| {
        for &(c, first, end) in &ranges {
            if i < end - first { return (c, first + i); }
            i -= end - first;
        }
        unreachable!("segment cursor within query")
    };
    let render = |items: &[String], next: Option<&reply::Next>, compact: bool| {
        let mut result = Vec::new();
        let mut current = None;
        let mut segments = Vec::new();
        let flush = |c: usize, segments: &mut Vec<String>, result: &mut Vec<String>| {
            let mut channel = if compact {
                let mut out = Object::default(); out.int("channel", capture.layout.channels[c].id.0 as usize);
                out.text("name", &capture.layout.channels[c].name); label(&mut out, capture, c); out
            } else { summary_channel(capture, c, from, to) };
            channel.field("segments", list(std::mem::take(segments))); result.push(channel.render());
        };
        for item in items {
            let (c, i) = locate(item.parse().unwrap());
            if current != Some(c) {
                if let Some(c) = current { flush(c, &mut segments, &mut result); }
                current = Some(c);
            }
            let segment = &capture.features[c].segments[i];
            let mut out = Object::default(); out.num("from", Some(segment.from.max(from) - capture.t_trigger), QtyUnit::Second);
            out.num("to", Some(segment.to.min(to) - capture.t_trigger), QtyUnit::Second); out.text("what", &segment.what); segments.push(out.render());
        }
        if let Some(c) = current { flush(c, &mut segments, &mut result); }
        let mut out = Object::default(); out.symbol("side", side(capture.side)); out.field("channels", list(result)); out.field("gaps", gaps(capture)); out.int("total", total);
        let mut notes = Vec::new();
        if compact { notes.push(quote("segment page; raise budget for channel statistics")); }
        if from != capture.span.0 || to != capture.span.1 { notes.push(quote("class, top and base describe the whole capture; numeric measures use the requested span")); }
        if !notes.is_empty() { out.field("notes", list(notes)); }
        out.next(next); out.render()
    };
    let first = if start < total { vec![start.to_string()] } else { Vec::new() };
    let next = (start + 1 < total).then(|| reply::Next { cursor: format!("{key}/{}", start + 1), left: total - start - 1 });
    let compact = render(&first, next.as_ref(), false).len() > budget;
    Ok(reply::page(budget, start, total, &key, |i| i.to_string(), |items, next| render(items, next, compact)))
}

fn measurement_unit(what: &str, channel: Quantity) -> QtyUnit {
    match what { "frequency" => QtyUnit::Hertz, "width" | "rise" | "fall" | "delay" => QtyUnit::Second,
        "edges" | "uart" => QtyUnit::Count, _ => unit(channel) }
}
fn measurement_item(what: &str, r: Option<Reading>, unit: QtyUnit, trigger: f64) -> String {
    let mut out = Object::default(); out.text("what", what);
    if let Some(r) = r {
        if what == "duty" { out.field("value", percent(r.value)); out.field("sigma", percent(r.sigma)); }
        else { out.num("value", Some(r.value), unit); out.num("sigma", Some(r.sigma), unit); }
        out.int("n", r.n);
        for (name, (value, t)) in [("min", r.min), ("max", r.max)] {
            let mut extreme = Object::default();
            if what == "duty" { extreme.field("value", percent(value)); } else { extreme.num("value", Some(value), unit); }
            extreme.num("t", Some(t - trigger), QtyUnit::Second); out.field(name, extreme.render());
        }
    } else { out.int("n", 0); }
    out.render()
}
pub(super) fn measure(capture: &Capture, args: &Arguments, budget: usize) -> Result<ToolReply, String> {
    let c = channel(capture, args)?; let (from, to) = bounds(capture, args)?;
    let what = args.text("what").unwrap(); let per = args.text("per").unwrap();
    if what == "delay" { return Err("what: delay needs a target probe and edge directions, which this tool contract does not yet supply".into()); }
    if what == "uart" { return Err("what: uart needs a baud rate and format; use elec.capture.decode".into()); }
    let index = &capture.features[c].index;
    let events = if per == "cycle" { Some(&index.cycles) } else if per == "event" {
        Some(index.events(what).ok_or_else(|| format!("per: {what} has no event decomposition"))?)
    } else { None };
    let (first, end) = events.map_or((0, 1), |e| e.range(from, to));
    let total = end - first;
    let key = key(capture, &format!("measure/{c}/{what}/{per}"), from, to);
    let start = reply::offset(args.text("after"), &key, total)?;
    let unit = measurement_unit(what, capture.layout.channels[c].quantity);
    Ok(reply::page(budget, start, total, &key, |i| {
        let r = if let Some(events) = events {
            if index.events(what).is_some_and(|e| std::ptr::eq(e, events)) { events.reading(first + i, first + i + 1, what) }
            else { let event = events.items[first + i]; index.reading(what, event.from, event.to) }
        } else { index.reading(what, from, to) };
        measurement_item(what, r, unit, capture.t_trigger)
    }, |items, next| {
        let mut out = Object::default(); out.field("items", list(items.iter().cloned())); out.int("total", total);
        out.next(next); out.render()
    }))
}

pub(super) fn samples(capture: &Capture, args: &Arguments, budget: usize) -> Result<ToolReply, String> {
    if args.text("stream") == Some("firmware") { return firmware_samples(capture, args, budget); }
    let c = channel(capture, args)?; let (from, to) = bounds(capture, args)?;
    let reduce = args.text("reduce").unwrap(); let points = usize::try_from(args.int("points").unwrap()).unwrap_or(usize::MAX);
    let feature = &capture.features[c]; let trace = &capture.trace;
    let first = trace.index_at(from).map_or(0, |i| i - trace.base);
    let first = first + usize::from(trace.time.at(first) < from);
    let end = trace.index_at(to).map_or(0, |i| i - trace.base + 1);
    let total = if reduce == "raw" { end.saturating_sub(first) } else { points.min(trace.len()) };
    let dt = if total == 0 { 0.0 } else { (to - from) / total as f64 };
    let key = key(capture, &format!("samples/{c}/{reduce}/{points}"), from, to);
    let start = reply::offset(args.text("after"), &key, total)?;
    let unit = unit(capture.layout.channels[c].quantity);
    let (real_step, max_step, uniform) = if reduce == "raw" && end > first {
        feature.index.steps(trace.time.at(first), trace.time.at(end - 1))
    } else { feature.index.steps(from, to) };
    if capture.gaps.iter().any(|&(a, b, _)| a < to && b > from) {
        return Err("span: samples cross a gap; choose a continuous span from capture.summary".into());
    }
    let item = |i: usize| {
        let (a, b) = if reduce == "raw" { let t = trace.time.at(first + i); (t, t) }
            else { (from + i as f64 * dt, if i + 1 == total { to } else { from + (i + 1) as f64 * dt }) };
        let (low, high) = if reduce == "raw" { let v = trace.signals[c].at(first + i) as f64; (v, v) }
            else if reduce == "mean" { let v = feature.index.reading("mean", a, b).map_or(f64::NAN, |r| r.value); (v, v) }
            else {
                let lo = trace.index_at(a).map_or(0, |i| i - trace.base);
                let lo = lo + usize::from(trace.time.at(lo) < a);
                let hi = trace.index_at(b).map_or(0, |i| i - trace.base + 1);
                // Boundary interpolation belongs to the same numeric index as Expect.
                let low = feature.index.reading("min", a, b).map_or(f64::NAN, |r| r.value);
                let high = feature.index.reading("max", a, b).map_or(f64::NAN, |r| r.value);
                capture.minmax(c, lo, hi).map_or((low, high), |(min, max)| (low.min(min), high.max(max)))
            };
        if !low.is_finite() || !high.is_finite() { return Err("span: samples include an unresolved channel".to_owned()); }
        Ok((a, qty(low, unit), qty(high, unit)))
    };
    let render = |items: &[(f64, String, String)], next: Option<&reply::Next>| {
        let mut out = Object::default();
        if let Some(first) = items.first() { out.num("t0", Some(first.0 - capture.t_trigger), QtyUnit::Second); }
        if reduce == "raw" && !uniform {
            out.field("t", list(items.iter().map(|p| qty(p.0 - capture.t_trigger, QtyUnit::Second))));
        } else { out.num("dt", Some(if reduce == "raw" { real_step } else { dt }), QtyUnit::Second); }
        out.symbol("reduce", reduce);
        if reduce == "minmax" {
            out.field("min", list(items.iter().map(|p| p.1.clone()))); out.field("max", list(items.iter().map(|p| p.2.clone())));
        } else { out.field("y", list(items.iter().map(|p| p.1.clone()))); }
        out.num("real_step", Some(real_step), QtyUnit::Second); out.num("max_step", Some(max_step), QtyUnit::Second);
        out.int("total", total); out.next(next); out.render()
    };
    reply::page_values(budget, start, total, points, &key, item, render)
}

fn firmware_samples(capture: &Capture, args: &Arguments, budget: usize) -> Result<ToolReply, String> {
    let record = capture.firmware.as_ref().ok_or_else(|| format!("stream: capture {} has no firmware record", capture.id))?;
    let (from, to) = bounds(capture, args)?;
    let first = record.first_at(from); let end = record.first_at(to.next_up());
    let total = end.saturating_sub(first);
    let points = usize::try_from(args.int("points").unwrap()).unwrap_or(usize::MAX);
    let key = key(capture, "firmware", from, to);
    let start = reply::offset(args.text("after"), &key, total)?;
    reply::page_values(budget, start, total, points, &key, |i| {
        let event = record.get(first + i).expect("firmware cursor within immutable record");
        Ok((qty(event.t - capture.t_trigger, QtyUnit::Second), format!("@{}", fw_events::source_name(event.source)),
            quote(&fw_events::label(record, event)), event.site.map_or(-1, i32::from).to_string()))
    }, |items, next| {
        let mut out = Object::default();
        out.field("t", list(items.iter().map(|p| p.0.clone())));
        out.field("source", list(items.iter().map(|p| p.1.clone())));
        out.field("event", list(items.iter().map(|p| p.2.clone())));
        out.field("site", list(items.iter().map(|p| p.3.clone())));
        out.field("lost", record.lost().to_string()); out.int("total", total);
        out.next(next); out.render()
    })
}

//! UART decoding on the tools worker; immutable results make subsequent pages cheap.

use std::collections::VecDeque;
use std::sync::Arc;
use fab_elec_bench::source::Quantity;
use fab_elec_bench::uart::{self, BaudEstimate, Frame, FrameError, Parity, UartFormat};
use fab_elec_circuit::QtyUnit;
use super::call::{number, Arguments};
use super::capture::Capture;
use super::query;
use super::reply::{self, list, percent, quote, Object, ToolReply};

// Eviction only costs a later re-decode; cursors do not refer to cache slots.
const CACHE_ENTRIES: usize = 4;

struct Key {
    capture: String,
    channel: usize,
    format: UartFormat,
    span: (f64, f64),
    auto_baud: bool,
    indexed: bool,
}
impl Key {
    fn matches(&self, request: &Self) -> bool {
        let format = UartFormat {
            baud: if request.auto_baud { self.format.baud } else { request.format.baud },
            ..request.format
        };
        self.capture == request.capture && self.channel == request.channel && self.span == request.span
            && self.auto_baud == request.auto_baud && self.indexed == request.indexed && self.format == format
    }
}
struct Decoded {
    key: Key,
    estimate: Option<BaudEstimate>,
    frames: Arc<[Frame]>,
    errors: [usize; 3],
}

#[derive(Default)]
pub(super) struct Cache {
    entries: VecDeque<Decoded>,
}
impl Cache {
    pub(super) fn drop_capture(&mut self, id: &str) {
        self.entries.retain(|entry| entry.key.capture != id);
    }
}

pub(super) fn decode(capture: &Capture, args: &Arguments, budget: usize, cache: &mut Cache) -> Result<ToolReply, String> {
    let c = query::channel(capture, args)?;
    let quantity = capture.layout.channels[c].quantity;
    if !matches!(quantity, Quantity::Volt | Quantity::Logic) {
        return Err("channel: UART requires a voltage or logic channel".into());
    }
    let (from, to) = query::bounds(capture, args)?;
    let baud = args.text("baud").unwrap().trim();
    let auto_baud = baud == "auto";
    let baud = if auto_baud { 0.0 } else {
        let baud = number(baud, "").map_err(|_| "baud: expected auto or a positive finite number")?;
        if baud <= 0.0 || !(12.0 / baud).is_finite() { return Err("baud: expected a positive finite rate with a finite frame duration".into()); }
        baud
    };
    let index = &capture.features[c].index;
    let mut threshold = index.threshold;
    let level = args.text("threshold").map(str::trim).filter(|s| !s.is_empty());
    if let Some(level) = level {
        threshold.level = number(level, if quantity == Quantity::Volt { "V" } else { "" })
            .map_err(|e| format!("threshold: {e}"))?;
    }
    if !threshold.level.is_finite() || !threshold.hysteresis.is_finite() {
        return Err("threshold: channel has no finite signal levels".into());
    }
    let parity = match args.text("parity").unwrap() { "even" => Parity::Even, "odd" => Parity::Odd, _ => Parity::None };
    let stop = match args.text("stop").unwrap() { "1.5" => 1.5, "2" => 2.0, _ => 1.0 };
    let format = UartFormat { baud, bits: args.int("bits").unwrap() as u8, parity, stop,
        invert: args.text("invert") == Some("yes"), threshold };
    let mut key = Key { capture: capture.id.clone(), channel: c, format, span: (from, to), auto_baud, indexed: level.is_none() };
    let cached = cache.entries.iter().position(|entry| entry.key.matches(&key));
    let at = if let Some(at) = cached { at } else {
        if capture.gaps.iter().any(|&(a, b, _)| a < to && b > from) {
            return Err("span: samples cross a gap; choose a continuous span from capture.summary".into());
        }
        let trace = &capture.trace;
        let first = trace.index_at(from).map_or(0, |i| i - trace.base);
        let first = first + usize::from(trace.time.at(first) < from);
        let end = trace.index_at(to).map_or(0, |i| i - trace.base + 1);
        let t: Vec<_> = (first..end).map(|i| trace.time.at(i)).collect();
        let y: Vec<_> = (first..end).map(|i| trace.signals[c].at(i)).collect();
        if y.iter().any(|y| !y.is_finite()) { return Err("span: samples include an unresolved channel".into()); }
        let edges = if key.indexed {
            let (first, end) = index.edges.range(from, to);
            index.edges.items[first..end].iter().map(|edge| edge.from).collect()
        } else { uart::edge_times(&t, &y, threshold) };
        let estimate = uart::estimate_baud(&edges);
        if auto_baud {
            key.format.baud = estimate.ok_or("baud: too few edges to estimate; give baud or a longer span")?.measured;
        }
        let frames: Arc<[Frame]> = uart::decode(&t, &y, key.format).into();
        let mut errors = [0; 3];
        for frame in frames.iter() {
            if let Some(error) = frame.error { errors[error_index(error)] += 1; }
        }
        if cache.entries.len() == CACHE_ENTRIES { cache.entries.pop_front(); }
        cache.entries.push_back(Decoded { key, estimate, frames, errors });
        cache.entries.len() - 1
    };
    let decoded = &cache.entries[at];
    let key = &decoded.key; let format = key.format;
    let cursor = query::key(capture, &format!("decode/{c}/{}/{}/{:016x}/{}/{}/{:016x}/{}/{:016x}/{:016x}",
        u8::from(key.auto_baud), u8::from(key.indexed), format.baud.to_bits(), format.bits,
        args.text("parity").unwrap(), format.stop.to_bits(), u8::from(format.invert),
        threshold.level.to_bits(), threshold.hysteresis.to_bits()), from, to);
    let total = decoded.frames.len();
    let start = reply::offset(args.text("after"), &cursor, total)?;
    let limit = usize::try_from(args.int("limit").unwrap()).unwrap_or(usize::MAX);
    let mut baud = Object::default();
    if let Some(estimate) = decoded.estimate {
        baud.num("measured", Some(estimate.measured), QtyUnit::Count);
        baud.num("nominal", estimate.nominal, QtyUnit::Count);
        if let Some(nominal) = estimate.nominal { baud.field("error", percent(100.0 * (estimate.measured / nominal - 1.0))); }
    }
    baud.num("used", Some(format.baud), QtyUnit::Count);
    let mut errors = Object::default();
    for (name, count) in ["framing", "parity", "break"].into_iter().zip(decoded.errors) { errors.int(name, count); }
    let mut out = Object::default(); out.field("baud", baud.render()); out.field("errors", errors.render()); out.int("total", total);
    Ok(page(decoded, capture.t_trigger, out, start, limit, &cursor, budget))
}

fn error_index(error: FrameError) -> usize {
    match error { FrameError::Framing => 0, FrameError::Parity => 1, FrameError::Break => 2 }
}
fn frame(frame: Frame, trigger: f64) -> String {
    let mut out = Object::default();
    out.num("t0", Some(frame.t0 - trigger), QtyUnit::Second); out.num("t1", Some(frame.t1 - trigger), QtyUnit::Second);
    out.int("byte", frame.value as usize);
    if let Some(error) = frame.error { out.symbol("error", ["framing", "parity", "break"][error_index(error)]); }
    out.render()
}

fn page(decoded: &Decoded, trigger: f64, out: Object, start: usize, limit: usize, key: &str, budget: usize) -> ToolReply {
    let total = decoded.frames.len();
    let render = |frames: &[String], text: &str, next: Option<&reply::Next>| {
        let mut out = out.clone(); out.field("frames", list(frames.iter().cloned())); out.text("text", text);
        out.next(next); out.render()
    };
    let next_at = |end: usize| (end < total).then(|| reply::Next { cursor: format!("{key}/{end}"), left: total - end });
    let empty = render(&[], "", None);
    if start == total { return ToolReply::Ok(empty); }
    // Exact byte prefixes keep pagination O(page), including the text column. All
    // text fragments are ASCII, so their quoted lengths add without boundary effects.
    let mut bytes = empty.len();
    let mut frames = Vec::new(); let mut text = String::new();
    let mut accepted = 0; let mut text_end = 0; let mut first_end = 0; let mut minimum = usize::MAX;
    for &value in decoded.frames[start..].iter().take(limit) {
        let item = frame(value, trigger);
        // A bad frame still contributes its received byte to the tool's text.
        let piece = uart::text(&[Frame { error: None, ..value }]);
        bytes += item.len() + usize::from(!frames.is_empty()) * 2 + quote(&piece).len() - 2;
        frames.push(item); text.push_str(&piece);
        if frames.len() == 1 { first_end = text.len(); }
        let required = bytes + next_at(start + frames.len()).map_or(0, |next| next.bytes());
        minimum = minimum.min(required);
        if required <= budget { accepted = frames.len(); text_end = text.len(); }
        // Only dropping the final cursor can shrink a later page. Stop once even
        // the cursorless prefix exceeds the budget (or the smallest first item).
        if bytes > budget.max(minimum) { break; }
    }
    // Nothing fits: the first frame answers alone, over the budget, rather than nothing.
    if accepted == 0 && !frames.is_empty() { accepted = 1; text_end = first_end; }
    ToolReply::Ok(render(&frames[..accepted], &text[..text_end], next_at(start + accepted).as_ref()))
}

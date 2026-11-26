//! A value's keys as a curve editor reads and writes them: Motion's
//! `keys([t, v], [t, v, @ease], …)` call, each key's bytes (its time, its
//! value, its ease) so a lane can drag one key and write only its numbers,
//! add a key, take one away or set a segment's ease, all as splices of the
//! text as it is now. The code is the truth: a lane reads its keys from the
//! code each time the code changes.

use makepad_script::tokenizer::{lex, LexedToken, ScriptToken, ScriptTokenTag};
use makepad_script::LiveId;
use std::ops::Range;

/// One key of a `keys(…)` call.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    /// The key's brackets, `[` to `]`.
    pub bracket: Range<usize>,
    /// Its time's bytes, and the time when it is a number (an anchor, a
    /// sum, is shown where the document puts it but not dragged in time).
    pub t_range: Range<usize>,
    pub t: Option<f64>,
    /// Its value's bytes, and the value when it is a number.
    pub v_range: Range<usize>,
    pub v: Option<f64>,
    /// The ease of the segment into it (`@ease_out_cubic`), or its record
    /// (`{ease: @bezier handle: @free in: [-0.3, 5] out: [0.25, -4]}`), its bytes.
    pub ease: Option<(Range<usize>, String)>,
    /// The segment into it, as a curve editor shows it.
    pub interp: Interp,
    /// Its handles as written (mode, written offsets).
    pub handles: crate::model::KeyHandles,
}

/// How the segment into a key goes.
#[derive(Clone, Debug, PartialEq)]
pub enum Interp {
    /// Straight (no ease written).
    Linear,
    /// Held, then a jump (`@hold`).
    Constant,
    /// A bezier through both keys' handles.
    Bezier,
    /// A named ease (`ease_out_cubic`).
    Ease(String),
}

impl Interp {
    fn of(part: Option<&str>) -> (Interp, crate::model::KeyHandles) {
        use crate::model::{HandleMode, KeyHandles};
        let Some(part) = part.map(str::trim) else { return (Interp::Linear, KeyHandles::default()) };
        if part.starts_with('{') {
            let fields = crate::paths::record_fields(part).unwrap_or_default();
            let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.trim().to_string());
            let pair = |k: &str| get(k).and_then(|v| {
                let inner = v.trim().strip_prefix('[')?.strip_suffix(']')?.to_string();
                let mut it = inner.split(',').map(|x| x.trim().parse::<f64>());
                Some([it.next()?.ok()?, it.next()?.ok()?])
            });
            let (r#in, out) = (pair("in"), pair("out"));
            let mode = get("handle").and_then(|h| HandleMode::from_name(h.trim_start_matches('@'))).unwrap_or(if r#in.is_some() || out.is_some() { HandleMode::Free } else { HandleMode::Clamped });
            let interp = match get("ease").as_deref().map(|e| e.trim_start_matches('@')) {
                None | Some("bezier") => Interp::Bezier,
                Some("hold") => Interp::Constant,
                Some("linear") => Interp::Linear,
                Some(e) => Interp::Ease(e.to_string()),
            };
            let bezier = interp == Interp::Bezier;
            return (interp, KeyHandles { bezier, mode, r#in, out });
        }
        let name = part.trim_start_matches('@');
        let interp = match name {
            "bezier" => Interp::Bezier,
            "hold" | "step" => Interp::Constant,
            "linear" => Interp::Linear,
            e => Interp::Ease(e.to_string()),
        };
        let bezier = interp == Interp::Bezier;
        (interp, KeyHandles { bezier, ..KeyHandles::default() })
    }
}

/// A `keys(…)` call: its bytes and its keys in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Keys {
    pub call: Range<usize>,
    pub keys: Vec<Key>,
    /// The record after the keys (`{before: @linear after: @cycle}`), its
    /// bytes, and what it says.
    pub extrap: Option<Range<usize>>,
    pub before: crate::model::Extrap,
    pub after: crate::model::Extrap,
}

fn op_is(t: &ScriptToken, s: &str) -> bool {
    t.operator_or_separator() == Some(LiveId::from_str(s))
}

/// The number one token (or a `-` and one) holds.
fn number(toks: &[LexedToken], text: &str) -> Option<f64> {
    let t = match toks {
        [one] => text.get(one.span().clone())?.to_string(),
        [sign, one] if op_is(sign.token(), "-") => format!("-{}", text.get(one.span().clone())?),
        _ => return None,
    };
    let end = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(t.len());
    let v: f64 = t[..end].parse().ok()?;
    // A unit: seconds and pixels are the base; ms and degrees convert.
    Some(match &t[end..] {
        "ms" => v / 1000.0,
        "deg" => v.to_radians(),
        _ => v,
    })
}

/// The `keys(…)` call written at `range` of `text` (the value a path names).
pub fn parse(text: &str, range: Range<usize>) -> Option<Keys> {
    let toks: Vec<LexedToken> = lex(text).into_iter().filter(|t| t.span().start >= range.start && t.span().end <= range.end).collect();
    let first = toks.first()?;
    if first.token().as_identifier() != Some(LiveId::from_str("keys")) || !matches!(toks.get(1)?.token().kind(), ScriptTokenTag::OpenRound) {
        return None;
    }
    let mut keys = Vec::new();
    let mut depth = 0i32;
    let mut k = 1;
    let mut call_end = None;
    let mut extrap: Option<Range<usize>> = None;
    while k < toks.len() {
        match toks[k].token().kind() {
            ScriptTokenTag::OpenCurly if depth == 1 => {
                // The record after the keys.
                let mut d = 0;
                for j in k..toks.len() {
                    match toks[j].token().kind() {
                        ScriptTokenTag::OpenSquare | ScriptTokenTag::OpenRound | ScriptTokenTag::OpenCurly => d += 1,
                        ScriptTokenTag::CloseSquare | ScriptTokenTag::CloseRound | ScriptTokenTag::CloseCurly => {
                            d -= 1;
                            if d == 0 {
                                extrap = Some(toks[k].span().start..toks[j].span().end);
                                k = j + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }
            ScriptTokenTag::OpenRound | ScriptTokenTag::OpenCurly => depth += 1,
            ScriptTokenTag::CloseRound | ScriptTokenTag::CloseCurly => {
                depth -= 1;
                if depth == 0 {
                    call_end = Some(toks[k].span().end);
                    break;
                }
            }
            // A key: `[` at the call's own depth.
            ScriptTokenTag::OpenSquare if depth == 1 => {
                let open = k;
                let mut d = 0;
                let mut close = open;
                for j in open..toks.len() {
                    match toks[j].token().kind() {
                        ScriptTokenTag::OpenSquare | ScriptTokenTag::OpenRound | ScriptTokenTag::OpenCurly => d += 1,
                        ScriptTokenTag::CloseSquare | ScriptTokenTag::CloseRound | ScriptTokenTag::CloseCurly => {
                            d -= 1;
                            if d == 0 {
                                close = j;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                // Its parts, split at the commas at its own depth.
                let mut parts: Vec<Vec<LexedToken>> = vec![Vec::new()];
                let mut d = 0;
                for t in &toks[open + 1..close] {
                    match t.token().kind() {
                        ScriptTokenTag::OpenSquare | ScriptTokenTag::OpenRound | ScriptTokenTag::OpenCurly => d += 1,
                        ScriptTokenTag::CloseSquare | ScriptTokenTag::CloseRound | ScriptTokenTag::CloseCurly => d -= 1,
                        _ => {}
                    }
                    if d == 0 && op_is(t.token(), ",") {
                        parts.push(Vec::new());
                    } else {
                        parts.last_mut().unwrap().push(t.clone());
                    }
                }
                let span = |p: &[LexedToken]| p.first().map(|f| f.span().start).unwrap_or(0)..p.last().map(|l| l.span().end).unwrap_or(0);
                if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
                    let ease = parts.get(2).filter(|p| !p.is_empty()).map(|p| (span(p), text[span(p)].to_string()));
                    let (interp, handles) = Interp::of(ease.as_ref().map(|(_, e)| e.as_str()));
                    keys.push(Key {
                        interp,
                        handles,
                        bracket: toks[open].span().start..toks[close].span().end,
                        t_range: span(&parts[0]),
                        t: number(&parts[0], text),
                        v_range: span(&parts[1]),
                        v: number(&parts[1], text),
                        ease,
                    });
                }
                k = close + 1;
                continue;
            }
            _ => {}
        }
        k += 1;
    }
    let (mut before, mut after) = (crate::model::Extrap::Constant, crate::model::Extrap::Constant);
    if let Some(r) = &extrap {
        for (k, v) in crate::paths::record_fields(&text[r.clone()]).unwrap_or_default() {
            let e = match v.trim().trim_start_matches('@') {
                "linear" => crate::model::Extrap::Linear,
                "cycle" => crate::model::Extrap::Cycle,
                _ => crate::model::Extrap::Constant,
            };
            if k == "before" {
                before = e;
            } else if k == "after" {
                after = e;
            }
        }
    }
    Some(Keys { call: first.span().start..call_end?, keys, extrap, before, after })
}

// ------------------------------------------------------------------ curves

/// A key written again: `[t, v]`, `[t, v, @hold]`, `[t, v, @ease_out]`,
/// `[t, v, @bezier]`, or with a record when its handles or a non-bezier
/// segment with handles need saying. `t` and `v` as code.
pub fn key_text(t: &str, v: &str, interp: &Interp, h: &crate::model::KeyHandles) -> String {
    use crate::model::HandleMode;
    let n = |x: f64| crate::patch::fmt_num((x * 1000.0).round() / 1000.0);
    let mut rec: Vec<String> = Vec::new();
    let default_handles = h.mode == HandleMode::Clamped && h.r#in.is_none() && h.out.is_none();
    if !default_handles {
        match interp {
            Interp::Bezier => {}
            Interp::Linear => rec.push("ease: @linear".into()),
            Interp::Constant => rec.push("ease: @hold".into()),
            Interp::Ease(e) => rec.push(format!("ease: @{e}")),
        }
        if !(h.mode == HandleMode::Free && (h.r#in.is_some() || h.out.is_some())) {
            rec.push(format!("handle: @{}", h.mode.name()));
        }
        if h.mode.written() {
            if let Some([dt, dv]) = h.r#in {
                rec.push(format!("in: [{}, {}]", n(dt), n(dv)));
            }
            if let Some([dt, dv]) = h.out {
                rec.push(format!("out: [{}, {}]", n(dt), n(dv)));
            }
        }
        return format!("[{t}, {v}, {{{}}}]", rec.join(" "));
    }
    match interp {
        Interp::Linear => format!("[{t}, {v}]"),
        Interp::Constant => format!("[{t}, {v}, @hold]"),
        Interp::Bezier => format!("[{t}, {v}, @bezier]"),
        Interp::Ease(e) => format!("[{t}, {v}, @{e}]"),
    }
}

/// The splice that writes key `k` as `text` (its brackets).
pub fn set_key(keys: &Keys, k: usize, text: String) -> Option<(Range<usize>, String)> {
    Some((keys.keys.get(k)?.bracket.clone(), text))
}

/// The splice that writes what the track does before and after its keys
/// (the record taken away when both are constant).
pub fn set_extrap(keys: &Keys, before: crate::model::Extrap, after: crate::model::Extrap) -> Option<(Range<usize>, String)> {
    use crate::model::Extrap;
    let mut parts = Vec::new();
    if before != Extrap::Constant {
        parts.push(format!("before: @{}", before.name()));
    }
    if after != Extrap::Constant {
        parts.push(format!("after: @{}", after.name()));
    }
    let last = keys.keys.last()?;
    match (&keys.extrap, parts.is_empty()) {
        (Some(r), true) => Some((last.bracket.end..r.end, String::new())),
        (Some(r), false) => Some((r.clone(), format!("{{{}}}", parts.join(" ")))),
        (None, true) => None,
        (None, false) => Some((last.bracket.end..last.bracket.end, format!(", {{{}}}", parts.join(" ")))),
    }
}

/// The numeric keys as the engine evaluates them (a key not a number is
/// left out), with each one's index in `keys`.
pub fn track(keys: &Keys) -> (crate::model::Track<1>, Vec<usize>) {
    use crate::model::{Key as MKey, Time, Track};
    let mut out = Vec::new();
    let mut idx = Vec::new();
    for (i, k) in keys.keys.iter().enumerate() {
        let (Some(t), Some(v)) = (k.t, k.v) else { continue };
        let ease = match &k.interp {
            Interp::Constant => makepad_tween::named::Ease::Hold,
            Interp::Ease(e) => makepad_tween::named::Ease::from_id(e).unwrap_or_default(),
            _ => makepad_tween::named::Ease::Linear,
        };
        let has_handles = k.interp == Interp::Bezier || k.handles != crate::model::KeyHandles::default();
        out.push(MKey { t: Time { expr: crate::timing::TimeExpr::Seconds(t), value: t }, v: [v], ease, bez: has_handles.then_some(k.handles) });
        idx.push(i);
    }
    (Track { keys: out, repeat: None, before: keys.before, after: keys.after }, idx)
}

/// The curve's value at `t`, exactly as the document evaluates it.
pub fn sample(keys: &Keys, t: f64) -> Option<f64> {
    let (track, _) = track(keys);
    (!track.keys.is_empty()).then(|| crate::eval::sample_track(&track, t)[0])
}

/// Each numeric key's handles as the curve draws them: (in, out) offsets
/// (seconds, value), in `track`'s order.
pub fn handles(keys: &Keys) -> Vec<([f64; 2], [f64; 2])> {
    let (track, _) = track(keys);
    (0..track.keys.len()).map(|k| {
        let (i, o) = crate::eval::key_handles(&track.keys, k);
        ([i.0, i.1[0]], [o.0, o.1[0]])
    }).collect()
}

/// The splice (bytes, text) that adds a key `[t, v]` in time order.
pub fn add_key(keys: &Keys, t: f64, v: f64) -> (Range<usize>, String) {
    // A new key's time to the hundredth, its value to the thousandth.
    let key = format!("[{}, {}]", crate::patch::fmt_num((t * 100.0).round() / 100.0), crate::patch::fmt_num((v * 1000.0).round() / 1000.0));
    match keys.keys.iter().find(|k| k.t.is_some_and(|kt| kt > t)) {
        Some(next) => (next.bracket.start..next.bracket.start, format!("{key}, ")),
        None => match keys.keys.last() {
            Some(last) => (last.bracket.end..last.bracket.end, format!(", {key}")),
            None => (keys.call.end - 1..keys.call.end - 1, key),
        },
    }
}

/// The splice that takes key `k` away (with the comma beside it); `None`
/// for the only key.
pub fn remove_key(keys: &Keys, k: usize) -> Option<(Range<usize>, String)> {
    if keys.keys.len() < 2 {
        return None;
    }
    let key = keys.keys.get(k)?;
    Some(match keys.keys.get(k + 1) {
        Some(next) => (key.bracket.start..next.bracket.start, String::new()),
        None => (keys.keys[k - 1].bracket.end..key.bracket.end, String::new()),
    })
}

/// The splice that sets the ease into key `k` (`None`: linear, the ease
/// taken away).
pub fn set_ease(text: &str, keys: &Keys, k: usize, ease: Option<&str>) -> Option<(Range<usize>, String)> {
    let key = keys.keys.get(k)?;
    Some(match (&key.ease, ease) {
        (Some((range, _)), Some(e)) => (range.clone(), format!("@{e}")),
        (Some((range, _)), None) => {
            // `, @ease`: from the value's end.
            (key.v_range.end..range.end, String::new())
        }
        (None, Some(e)) => (key.v_range.end..key.v_range.end, format!(", @{e}")),
        (None, None) => return None,
    })
    .filter(|_| text.len() >= key.bracket.end)
}

/// Eased progress `u` (0..1) by the named ease (`ease_out_cubic`; linear
/// when unknown).
pub fn ease(name: &str, u: f64) -> f64 {
    let name = name.trim().trim_start_matches('@');
    if name == "hold" {
        return if u >= 1.0 { 1.0 } else { 0.0 };
    }
    match makepad_tween::named::Ease::from_id(name) {
        Some(e) => e.apply(u.clamp(0.0, 1.0), 1.0),
        None => u.clamp(0.0, 1.0),
    }
}

/// The value at time `t` of numeric keys (each segment eased into its end
/// key); `None` without two numeric keys.
pub fn value_at(keys: &Keys, t: f64) -> Option<f64> {
    let pts: Vec<(f64, f64, Option<&str>)> = keys.keys.iter().filter_map(|k| Some((k.t?, k.v?, k.ease.as_ref().map(|(_, e)| e.as_str())))).collect();
    let first = pts.first()?;
    if t <= first.0 {
        return Some(first.1);
    }
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t <= b.0 {
            let u = if b.0 > a.0 { (t - a.0) / (b.0 - a.0) } else { 1.0 };
            let e = b.2.map_or(u, |name| ease(name, u));
            return Some(a.1 + (b.1 - a.1) * e);
        }
    }
    pts.last().map(|p| p.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keys_call_reads_its_keys_and_writes_them_back() {
        let text = "let Y = keys([0, 30], [0.6s, -1.5, @ease_out_back], [beat(2), 5])\n";
        let at = text.find("keys").unwrap();
        let keys = parse(text, at..text.len() - 1).unwrap();
        assert_eq!(keys.keys.len(), 3);
        assert_eq!(keys.keys[1].t, Some(0.6));
        assert_eq!(keys.keys[1].v, Some(-1.5));
        assert_eq!(keys.keys[1].ease.as_ref().unwrap().1, "@ease_out_back");
        assert_eq!(keys.keys[2].t, None);
        assert_eq!(&text[keys.keys[2].t_range.clone()], "beat(2)");
        let apply = |(r, s): (Range<usize>, String)| {
            let mut t = text.to_string();
            t.replace_range(r, &s);
            t
        };
        assert_eq!(apply(add_key(&keys, 0.3, 7.0)), "let Y = keys([0, 30], [0.3, 7], [0.6s, -1.5, @ease_out_back], [beat(2), 5])\n");
        assert_eq!(apply(remove_key(&keys, 1).unwrap()), "let Y = keys([0, 30], [beat(2), 5])\n");
        assert_eq!(apply(remove_key(&keys, 2).unwrap()), "let Y = keys([0, 30], [0.6s, -1.5, @ease_out_back])\n");
        assert_eq!(apply(set_ease(text, &keys, 1, None).unwrap()), "let Y = keys([0, 30], [0.6s, -1.5], [beat(2), 5])\n");
        assert_eq!(apply(set_ease(text, &keys, 0, Some("ease_in_cubic")).unwrap()), "let Y = keys([0, 30, @ease_in_cubic], [0.6s, -1.5, @ease_out_back], [beat(2), 5])\n");
        assert_eq!(value_at(&keys, 0.0), Some(30.0));
        assert!((value_at(&keys, 0.6).unwrap() + 1.5).abs() < 1e-9);
    }

    #[test]
    fn bezier_keys_and_extrapolation_read_and_write() {
        use crate::model::{Extrap, HandleMode, KeyHandles};
        let text = "let Y = keys([0, 0], [1, 10, @bezier], [2, 4, {handle: @aligned in: [-0.2, 3] out: [0.3, -4.5]}], {after: @cycle})\n";
        let at = text.find("keys").unwrap();
        let keys = parse(text, at..text.len() - 1).unwrap();
        assert_eq!(keys.keys.len(), 3);
        assert_eq!(keys.keys[1].interp, Interp::Bezier);
        assert_eq!(keys.keys[2].handles.mode, HandleMode::Aligned);
        assert_eq!(keys.keys[2].handles.r#in, Some([-0.2, 3.0]));
        assert_eq!(keys.after, Extrap::Cycle);
        assert_eq!(&text[keys.extrap.clone().unwrap()], "{after: @cycle}");
        assert_eq!(key_text("2", "4", &Interp::Bezier, &keys.keys[2].handles), "[2, 4, {handle: @aligned in: [-0.2, 3] out: [0.3, -4.5]}]");
        assert_eq!(key_text("1", "10", &Interp::Bezier, &KeyHandles::default()), "[1, 10, @bezier]");
        assert_eq!(key_text("1", "10", &Interp::Linear, &KeyHandles { mode: HandleMode::Vector, ..Default::default() }), "[1, 10, {ease: @linear handle: @vector}]");
        let apply = |(r, s): (Range<usize>, String)| {
            let mut t = text.to_string();
            t.replace_range(r, &s);
            t
        };
        assert_eq!(apply(set_extrap(&keys, Extrap::Linear, Extrap::Cycle).unwrap()), text.replace("{after: @cycle}", "{before: @linear after: @cycle}"));
        assert_eq!(apply(set_extrap(&keys, Extrap::Constant, Extrap::Constant).unwrap()), text.replace(", {after: @cycle}", ""));
        // What the curve draws is what the document evaluates.
        assert!((sample(&keys, 1.0).unwrap() - 10.0).abs() < 1e-9);
        assert!((sample(&keys, 2.5).unwrap() - sample(&keys, 0.5).unwrap()).abs() < 1e-9);
        assert_eq!(handles(&keys).len(), 3);
    }
}

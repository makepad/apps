//! Modulation: `modulate(base, @source, {rate depth phase})` is `base` moved
//! by a source over the document's clock, `base + depth * source(t)`:
//!
//! - `@lfo` sin(2π · rate · t + phase), −1..1
//! - `@noise` smooth noise at `rate` per second, −1..1
//! - `@beat` a pulse on each beat of the document's music, halving every
//!   1/rate s (0..1; 0 without music)
//! - `@word` a pulse on each sung word's start, the same (0 without a voice)
//! - `@time` t · rate
//!
//! It is written in the code like any value, so a record field reads
//! `lift: modulate(0.55, @lfo, {rate: 0.5 depth: 0.2})`. While the document
//! loads, modulate gives `base`.
//!
//! A top-level `let` whose value calls modulate is a live let, and so is
//! every later `let` whose value names one. Each frame, before anything
//! reads them, their values are worked out again at the frame's time, in
//! source order, and rebound. Drawing code that reads `WORD_SPARKS.lift`
//! reads a number as before. Only live lets run; the rest of the document
//! keeps what it loaded with. A value a function computed at load from a
//! live let (rather than a `let` naming it) stays the base's.
//!
//! The live lets compile from a copy of the document's text that keeps every
//! character of their values on its line and column (everything else
//! blanked), as the file `motion.splash#live`: a literal at a line and column
//! of the document is at the same line and column of the copy, so an
//! editor's literal patch can reach both (a modulation's base, depth, rate).

use crate::obj;
use crate::signals::SignalSource;
use crate::value::{doc_host, doc_host_ref, DOC_FILE};
use makepad_script::tokenizer::lex;
use makepad_script::*;
use std::ops::Range;
use std::sync::Arc;

/// A document's modulation state, kept by its VM's host.
#[derive(Default)]
pub(crate) struct LiveState {
    /// The frame's time (the document's clock) once frames are evaluated;
    /// `None` while the document loads (modulate gives its base).
    pub time: Option<f64>,
    /// The scope the document ended in (where its lets are bound).
    scope: Option<ScriptObject>,
    /// Each live let: its name and its value as a function, in source order.
    pub lets: Vec<(LiveId, ScriptValue)>,
    _roots: Vec<ScriptObjectRef>,
}

/// `modulate(base, @source, {rate depth phase})` on `module`.
pub(crate) fn install(vm: &mut ScriptVm, module: ScriptObject, signals: Arc<dyn SignalSource>, offset: f64) {
    vm.add_method(module, id_lut!(modulate), script_args!(base = NIL, source = NIL, o = NIL), move |vm, args| {
        let base = vm.bx.heap_ref().value(args, id!(base).into(), NoTrap);
        let source = vm.bx.heap_ref().value(args, id!(source).into(), NoTrap);
        let Some(source) = obj::ident(source) else {
            return script_err_invalid_args!(vm.trap(), "modulate's source is @lfo, @beat, @word, @noise or @time");
        };
        if !["lfo", "beat", "word", "noise", "time"].contains(&source.as_str()) {
            return script_err_invalid_args!(vm.trap(), "modulate's source is @lfo, @beat, @word, @noise or @time, not @{}", source);
        }
        let Some(b) = base.as_number() else {
            return script_err_invalid_args!(vm.trap(), "modulate's base is a number");
        };
        let Some(t) = doc_host_ref(vm).live.time else { return base };
        let o = vm.bx.heap_ref().value(args, id!(o).into(), NoTrap).as_object();
        let field = |vm: &ScriptVm, name: &str, default: f64| o.and_then(|o| obj::opt(vm, o, name)).and_then(|v| obj::num(vm, v)).unwrap_or(default);
        let rate = field(vm, "rate", 1.0);
        let depth = field(vm, "depth", 0.0);
        let phase = field(vm, "phase", 0.0);
        let song = t + offset;
        let s = match source.as_str() {
            "lfo" => (std::f64::consts::TAU * rate * t + phase).sin(),
            "noise" => noise(rate * t + phase),
            "time" => t * rate,
            "beat" => match signals.default_music() {
                Some(m) => {
                    let b = signals.beat_index(m, song);
                    let bpm = signals.bpm(m);
                    if bpm > 0.0 { half_life_pulse((b - b.floor()) * 60.0 / bpm, rate) } else { 0.0 }
                }
                None => 0.0,
            },
            _ => match signals.default_voice().and_then(|v| signals.voice(v)) {
                Some(voice) => voice.words.iter().rev().find(|w| w.start <= song).map_or(0.0, |w| half_life_pulse(song - w.start, rate)),
                None => 0.0,
            },
        };
        (b + depth * s).into()
    });
}

/// A pulse `dt` s after its onset, halving every 1/rate s.
fn half_life_pulse(dt: f64, rate: f64) -> f64 {
    0.5f64.powf(dt.max(0.0) * rate.max(0.0))
}

/// Smooth value noise, −1..1.
fn noise(x: f64) -> f64 {
    let hash = |i: f64| ((i * 12.9898).sin() * 43758.5453).fract() * 2.0 - 1.0;
    let i = x.floor();
    let f = x - i;
    let u = f * f * (3.0 - 2.0 * f);
    hash(i) * (1.0 - u) + hash(i + 1.0) * u
}

/// A live let as written: its name, the `let NAME =` before its value, and
/// its value (byte ranges of the document's code).
struct LivePlan {
    name: String,
    head: Range<usize>,
    value: Range<usize>,
}

/// The live lets of `code` (the document's text as it is evaluated, the
/// author's from byte `from` on): those whose value calls modulate, and the
/// later ones whose value names one.
fn plan(code: &str, from: usize) -> Vec<LivePlan> {
    let Ok(syntax) = makepad_script::syntax::parse(&code[from..]) else { return Vec::new() };
    let mut live: Vec<LivePlan> = Vec::new();
    let mut names: Vec<LiveId> = Vec::new();
    let modulate = LiveId::from_str("modulate");
    for statement in syntax.statements() {
        let makepad_script::syntax::Kind::Let { names: bound, pattern_span, value: Some(value), .. } = &statement.kind else { continue };
        let [name] = bound.as_slice() else { continue };
        let value_span = from + value.span.start..from + value.span.end;
        let text = &code[value_span.clone()];
        let tokens = lex(text);
        let reads = |id: LiveId| tokens.iter().any(|t| t.token().as_identifier() == Some(id));
        if !(reads(modulate) || names.iter().any(|n| reads(*n))) {
            continue;
        }
        // `let NAME =` (or `var`) right before the value.
        let Some(eq) = code[from + pattern_span.end..value_span.start].find('=').map(|at| from + pattern_span.end + at) else { continue };
        names.push(LiveId::from_str(name));
        live.push(LivePlan { name: name.clone(), head: from + statement.span.start..eq + 1, value: value_span });
    }
    live
}

/// The live lets' values as one list of functions, in a copy of `code` that
/// keeps each of their characters on its line and column (all else blank):
/// `[fn(){ VALUE },fn(){ VALUE }…]`, the brackets and `fn(){` written over
/// each `let NAME =`.
fn copy_text(code: &str, plans: &[LivePlan]) -> String {
    let mut out: Vec<char> = Vec::with_capacity(code.len());
    let mut at_byte: Vec<usize> = Vec::with_capacity(code.len());
    for (b, ch) in code.char_indices() {
        at_byte.push(b);
        out.push(if ch == '\n' { '\n' } else { ' ' });
    }
    let index = |byte: usize| at_byte.partition_point(|b| *b < byte);
    for (i, p) in plans.iter().enumerate() {
        let opener = if i == 0 { "[fn(){" } else { "},fn(){" };
        let h = index(p.head.start);
        for (k, ch) in opener.chars().enumerate() {
            out[h + k] = ch;
        }
        for (b, ch) in code[p.value.clone()].char_indices() {
            out[index(p.value.start + b)] = ch;
        }
    }
    let mut text: String = out.into_iter().collect();
    text.push_str("\n}]");
    text
}

/// Compile `code`'s live lets in the document's VM, once the document has
/// run (before it is frozen). Nothing when the document has none.
pub(crate) fn compile(vm: &mut ScriptVm, code: &str, from: usize) -> Vec<ScriptErrorRecord> {
    if !code.contains("modulate") {
        return Vec::new();
    }
    let plans = plan(code, from.min(code.len()));
    if plans.is_empty() {
        return Vec::new();
    }
    let Some(scope) = vm.end_scope_of(DOC_FILE) else { return Vec::new() };
    // Every live let's `let NAME =` holds its opener: the shortest is 7 characters.
    if plans.iter().any(|p| code[p.head.clone()].chars().count() < 7) {
        return Vec::new();
    }
    let text = copy_text(code, &plans);
    vm.capture_errors_nested();
    let list = vm.eval_in_scope(ScriptMod::new("", "", &(format!("{DOC_FILE}#live")), 0, 0, &(text), &[]), scope);
    let errors = vm.take_error_records();
    vm.capture_errors_restore();
    let fns = obj::items(vm, list).unwrap_or_default();
    if fns.len() != plans.len() {
        return errors;
    }
    let mut lets = Vec::new();
    let mut roots = Vec::new();
    for (p, f) in plans.iter().zip(fns) {
        if let Some(o) = f.as_object() {
            roots.push(vm.bx.heap_mut().new_object_ref(o));
        }
        lets.push((LiveId::from_str(&p.name), f));
    }
    let live = &mut doc_host(vm).live;
    live.scope = Some(scope);
    live.lets = lets;
    live._roots = roots;
    errors
}

/// A frame at `t` (the document's clock): modulation reads `t`, and the live
/// lets are worked out again and rebound, in source order.
pub(crate) fn frame(vm: &mut ScriptVm, t: f64) {
    let live = &mut doc_host(vm).live;
    live.time = Some(t);
    let (Some(scope), false) = (live.scope, live.lets.is_empty()) else { return };
    let lets = live.lets.clone();
    vm.capture_errors_nested();
    for (name, f) in lets {
        let (value, _) = vm.eval_bounded(&crate::value::FRAME_LIMITS, |vm| vm.call(f, &[]));
        if !value.is_err() {
            vm.bx.heap_mut().rebind_scope_value(scope, name, value);
        }
    }
    vm.take_error_records();
    vm.capture_errors_restore();
}

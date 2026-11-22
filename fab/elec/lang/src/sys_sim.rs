//! The project's simulation settings: one
//! `sim({mode: fast sample_rate: 96kHz recording_rate: 48kHz audio_load: line jack_loads: {J3: phones} recording_length: 4s})` statement in the
//! system document, read on every change of its text (not at evaluation: system text does not
//! re-evaluate the project) and written by the run settings' pickers. `sample_rate` is the rate
//! the circuit is simulated at; `recording_rate` the rate its sound is recorded and saved at;
//! `recording_length` how long a take is, in place of the bench's `stop`.
//!
//! A field left out is not chosen (the host decides: Fast, its own sample rate, takes at the
//! default audio rate, a line input on each output jack); no statement is none chosen. An unknown field, a value of the wrong kind or a second statement is a located
//! diag, and the statement it is in counts as none chosen.
//!
//! Cost: one syntax parse of the system document per change of its text, O(text); no VM.

use crate::board_splice::{after_statement, line_start};
use crate::source::located;
use crate::sys_doc::{self, declaration};
pub use fab_elec_bench::audio::AudioLoad;
pub use fab_elec_bench::SimMode;
use fab_elec_core::{Diag, DocId};
use fab_elec_design::TextEdit;
use makepad_script::syntax::{self, Kind, Node};
use std::collections::BTreeMap;
use std::ops::Range;


/// The `sim` statement's settings; None is not chosen.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SimSettings {
    pub mode: Option<SimMode>,
    /// The runs' sample rate, the rate the circuit is simulated at, Hz: positive and finite.
    pub sample_rate: Option<f64>,
    /// The rate the runs' sound is recorded at (their takes and saved WAVs), Hz: positive and
    /// finite.
    pub recording_rate: Option<f64>,
    /// What an output jack without its own load drives (None: a line input).
    pub audio_load: Option<AudioLoad>,
    /// Each jack's own load, once per reference designator in ascending order.
    pub jack_loads: Vec<(String, AudioLoad)>,
    /// A take's length, seconds, in place of the bench's `stop` (None: the bench's): positive and
    /// finite.
    pub recording_length: Option<f64>,
}

impl SimSettings {
    pub fn is_empty(&self) -> bool {
        self.mode.is_none() && self.sample_rate.is_none() && self.recording_rate.is_none() && self.audio_load.is_none()
            && self.jack_loads.is_empty() && self.recording_length.is_none()
    }
}

/// The rates the pickers offer, sample and recording rate alike, Hz. A hand-written statement may
/// name any positive rate. The low two are for a live run a machine cannot keep up at the audio
/// rates: fewer, longer steps, noise and the highest sounds cut at half the rate.
pub const SAMPLE_RATES: [f64; 6] = [24_000.0, 32_000.0, 44_100.0, 48_000.0, 96_000.0, 192_000.0];

/// The fields a `sim` statement takes, in the order they are written.
pub const SIM_FIELDS: [&str; 6] = ["mode", "sample_rate", "recording_rate", "audio_load", "jack_loads", "recording_length"];

/// The settings read from a system document, with where the statement is and what is wrong with
/// it.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SimEval {
    pub settings: SimSettings,
    /// The statement's whole span (None: the document has none).
    pub span: Option<Range<u32>>,
    pub diags: Vec<Diag>,
}

/// The statement is written by the system document's own parser; a line that does not parse is
/// the lowering's syntax diag, except a `sim` line, whose diag is here (the lowering skips it).
pub fn read(doc: DocId, text: &str) -> SimEval {
    let mut out = SimEval::default();
    let table = sys_doc::units();
    let mut found = 0;
    let mut take = |offset: u32, text: &str, whole: &Node, out: &mut SimEval| {
        let (n, binding) = declaration(whole, text);
        if n.callee_path() != Some("sim") || binding.is_some() {
            return;
        }
        let span = offset + whole.span.start as u32..offset + whole.span.end as u32;
        found += 1;
        if found > 1 {
            out.diags.push(located(doc, span, "a project has one sim statement: this one is not read"));
            return;
        }
        out.span = Some(span.clone());
        match settings(n, text) {
            Ok(settings) => out.settings = settings,
            Err((at, message)) => {
                let at = at.map_or(span, |at| offset + at.start as u32..offset + at.end as u32);
                out.diags.push(located(doc, at, format!("{message}; the run settings are not chosen")));
            }
        }
    };
    match syntax::parse_with(text, &table) {
        Ok(tree) => {
            for whole in tree.statements() {
                take(0, text, whole, &mut out);
            }
        }
        Err(_) => {
            let mut offset = 0;
            for line in text.split_inclusive('\n') {
                match syntax::parse_with(line, &table) {
                    Ok(tree) => {
                        for whole in tree.statements() {
                            take(offset, line, whole, &mut out);
                        }
                    }
                    Err(e) if is_sim_line(line) => {
                        let at = offset + e.at as u32;
                        out.diags.push(located(doc, at..at, format!("the sim statement: syntax: {}; the run settings are not chosen", e.message)));
                    }
                    Err(_) => {}
                }
                offset += line.len() as u32;
            }
        }
    }
    out
}

/// A line the lowering leaves to [`read`]: one that starts a `sim` call.
pub(crate) fn is_sim_line(line: &str) -> bool {
    line.trim_start().strip_prefix("sim").is_some_and(|rest| rest.trim_start().starts_with('('))
}

fn settings(n: &Node, text: &str) -> Result<SimSettings, (Option<Range<usize>>, String)> {
    let usage = "sim takes one options object, as in sim({mode: fast, sample_rate: 96kHz})";
    let Some([opts]) = n.call("sim") else { return Err((None, usage.into())) };
    let Kind::Object { ty: None, fields, .. } = &opts.kind else { return Err((Some(opts.span.clone()), usage.into())) };
    let mut out = SimSettings::default();
    for (i, f) in fields.iter().enumerate() {
        let name = f.name.as_deref().filter(|name| f.op == ":" && SIM_FIELDS.contains(name));
        let Some(name) = name else {
            return Err((Some(f.span.clone()), format!("sim has no field {}: its fields are {}", f.name.as_deref().unwrap_or("without a name"), SIM_FIELDS.join(", "))));
        };
        if fields[..i].iter().any(|other| other.name == f.name) {
            return Err((Some(f.span.clone()), format!("sim has {name} twice")));
        }
        let at = Some(f.value.span.clone());
        match name {
            "mode" => {
                out.mode = Some(match f.value.ident() {
                    Some("exact") => SimMode::Exact,
                    Some("fast") => SimMode::Fast,
                    Some("logic") => SimMode::Logic,
                    _ => return Err((at, "mode is exact, fast or logic".into())),
                });
            }
            "audio_load" => {
                out.audio_load = Some(audio_load(&f.value, text).ok_or_else(|| (at, "audio_load is open, line, phones or a resistance".to_string()))?);
            }
            "jack_loads" => {
                let usage = "jack_loads is a set of jack: load, as in {J3: phones}";
                let Kind::Object { ty: None, fields, .. } = &f.value.kind else { return Err((at, usage.into())) };
                let mut loads = BTreeMap::new();
                for field in fields {
                    let Some(jack) = field.name.as_ref().filter(|_| field.op == ":") else {
                        return Err((Some(field.span.clone()), usage.into()));
                    };
                    let load = audio_load(&field.value, text).ok_or_else(|| {
                        (Some(field.value.span.clone()), "a jack's load is open, line, phones or a resistance".to_string())
                    })?;
                    if loads.insert(jack.clone(), load).is_some() {
                        return Err((Some(field.span.clone()), format!("jack_loads has {jack} twice")));
                    }
                }
                out.jack_loads = loads.into_iter().collect();
            }
            "recording_length" => {
                let seconds = seconds(&f.value, text).ok_or_else(|| (at, "recording_length is a positive time, as in 4s or 500ms".to_string()))?;
                out.recording_length = Some(seconds);
            }
            _ => {
                let hz = frequency(&f.value, text).ok_or_else(|| (at, format!("{name} is a positive frequency, as in 96kHz or 44.1kHz")))?;
                if name == "sample_rate" {
                    out.sample_rate = Some(hz);
                } else {
                    out.recording_rate = Some(hz);
                }
            }
        }
    }
    Ok(out)
}

/// A named load or a positive, finite resistance with a circuit unit suffix.
fn audio_load(n: &Node, text: &str) -> Option<AudioLoad> {
    match n.ident() {
        Some("open") => Some(AudioLoad::Open),
        Some("line") => Some(AudioLoad::Line),
        Some("phones") => Some(AudioLoad::Phones),
        _ => {
            let Kind::Number(r) = n.kind else { return None };
            (n.text(text).ends_with("ohm") && r.is_finite() && r > 0.0).then_some(AudioLoad::Ohms(r))
        }
    }
}

/// A frequency literal in Hz (`96kHz`, `44100Hz`), rounded to a thousandth of a hertz so the
/// written decimals read back exactly.
fn frequency(n: &Node, text: &str) -> Option<f64> {
    let Kind::Number(v) = n.kind else { return None };
    let literal = n.text(text);
    let hertz = literal.ends_with("Hz") && v.is_finite() && v > 0.0;
    hertz.then(|| (v * 1000.0).round() / 1000.0)
}

/// A time literal in seconds (`4s`, `500ms`), rounded to a microsecond so the written decimals
/// read back exactly.
fn seconds(n: &Node, text: &str) -> Option<f64> {
    let Kind::Number(v) = n.kind else { return None };
    let literal = n.text(text);
    let timed = literal.ends_with('s') && !literal.ends_with("Hz") && v.is_finite() && v > 0.0;
    timed.then(|| (v * 1e6).round() / 1e6)
}

/// A length as the statement writes it: `4s`, `2.5s`, `500ms`.
pub fn length_literal(seconds: f64) -> String {
    let ms = seconds * 1000.0;
    if seconds < 1.0 && (ms - ms.round()).abs() < 1e-9 {
        format!("{}ms", ms.round())
    } else {
        format!("{seconds}s")
    }
}

/// A rate as the statement writes it: `96kHz`, `44.1kHz`, `22050Hz`.
pub fn rate_literal(hz: f64) -> String {
    if hz >= 1000.0 && (hz / 100.0).fract() == 0.0 {
        format!("{}kHz", hz / 1000.0)
    } else {
        format!("{hz}Hz")
    }
}

/// The statement for `settings`, its fields in [`SIM_FIELDS`] order; None when nothing is chosen.
pub fn statement(settings: &SimSettings) -> Option<String> {
    let mut fields = Vec::new();
    if let Some(mode) = settings.mode {
        let mode = match mode {
            SimMode::Exact => "exact",
            SimMode::Fast => "fast",
            SimMode::Logic => "logic",
        };
        fields.push(format!("mode: {mode}"));
    }
    if let Some(hz) = settings.sample_rate {
        fields.push(format!("sample_rate: {}", rate_literal(hz)));
    }
    if let Some(hz) = settings.recording_rate {
        fields.push(format!("recording_rate: {}", rate_literal(hz)));
    }
    if let Some(load) = settings.audio_load {
        fields.push(format!("audio_load: {}", load.literal()));
    }
    if !settings.jack_loads.is_empty() {
        let loads: BTreeMap<_, _> = settings.jack_loads.iter().map(|(jack, load)| (jack, load)).collect();
        let entries: Vec<_> = loads.into_iter().map(|(jack, load)| {
            let key = syntax::write::key(jack).unwrap_or_else(|_| syntax::write::quote(jack));
            format!("{key}: {}", load.literal())
        }).collect();
        fields.push(format!("jack_loads: {{{}}}", entries.join(" ")));
    }
    if let Some(seconds) = settings.recording_length {
        fields.push(format!("recording_length: {}", length_literal(seconds)));
    }
    (!fields.is_empty()).then(|| format!("sim({{{}}})", fields.join(" ")))
}

/// The edit that makes the system document `text` say `settings`: the statement replaced in place,
/// added as the document's first line, or its line removed when nothing is chosen. Refused while
/// the document does not parse or has two statements: the person sees why and fixes it first.
pub fn edit(doc: DocId, text: &str, settings: &SimSettings) -> Result<Vec<TextEdit>, Diag> {
    let tree = syntax::parse_with(text, &sys_doc::units()).map_err(|e| {
        let at = e.at as u32;
        located(doc, at..at, format!("the system document does not parse ({}): fix it before changing the run settings", e.message))
    })?;
    let mut statements = tree.statements().iter().filter(|whole| {
        let (n, binding) = declaration(whole, text);
        n.callee_path() == Some("sim") && binding.is_none()
    });
    let (first, second) = (statements.next(), statements.next());
    if let Some(second) = second {
        return Err(located(doc, second.span.start as u32..second.span.end as u32, "the system document has two sim statements: keep one"));
    }
    let new = statement(settings);
    let (span, line) = match (first, new) {
        (None, None) => return Ok(Vec::new()),
        (None, Some(line)) => {
            let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
            (0..0, format!("{line}{newline}"))
        }
        (Some(whole), None) => {
            let span = whole.span.start as u32..whole.span.end as u32;
            (line_start(text, span.start)..after_statement(text, span.end), String::new())
        }
        (Some(whole), Some(line)) => (whole.span.start as u32..whole.span.end as u32, line),
    };
    if text[span.start as usize..span.end as usize] == line {
        return Ok(Vec::new());
    }
    Ok(vec![TextEdit { doc, span, text: line.into() }])
}

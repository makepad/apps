//! Overlays that follow an app run (reports/M4-demo-overlays.md): a Motion
//! document reads the run a Graphic is bound to (`Graphic{… bind: @demo}`,
//! the Clip that plays the run) through `run()` in its property functions:
//!
//! ```splash
//! let r = run()
//! Motion{
//!     size: vec2(1080, 1920) duration: 10.0
//!     layers: [
//!         Rect{at: fn(t) { r.focus(t, {pad: 16}).at } size: fn(t) { r.focus(t, {pad: 16}).size }
//!              radius: 18 stroke: #ffcc33 width: 6 opacity: fn(t) { r.focus(t).presence }}
//!         Ellipse{at: fn(t) { let c = r.click(t) [c.at[0] - 60 * c.progress, c.at[1] - 60 * c.progress] }
//!                 size: fn(t) { let c = r.click(t) [120 * c.progress, 120 * c.progress] }
//!                 stroke: #ffffff width: 4 opacity: fn(t) { let c = r.click(t) c.presence * (1 - c.progress) }}
//!         Text{source: r.keys() show: @active size: 64 at: vec2(540, 1500) align: @center}
//!     ]
//! }
//! ```
//!
//! Everything is read from the run's recorded data (its `run.json`), never
//! from coordinates baked into the document, so an updated run (§6.5)
//! re-drives the overlays: the host passes the new run and the same
//! document lands on the new layout and timing. Geometry comes out in the
//! document's coordinates through the placement the host computed from the
//! edit (the clip's fit, every Reframe crop), so overlays stay on their
//! widget under a moving crop.
//!
//! - [`run`]: the run's data as functions of its time.
//! - [`place`]: the recording → document mapping at one moment.
//! - [`expr`]: the bound values (`focus`, `widget`, `click`, …) at one moment.
//! - [`when`]: the stretches of a run an overlay shows in ("when save is
//!   clicked"), for the AI's overlay tools.
//! - [`tools`]: the bindable overlay documents and the tool calls the AI
//!   uses to place them.

pub mod expr;
pub mod place;
pub mod run;
pub mod tools;
pub mod when;

#[cfg(test)]
mod tests;

use crate::features::{FeatureCurve, FeatureWord, MusicFeatures, VoiceFeatures};
use crate::signals::SignalSource;
use crate::timing::{SpokenTiming, Timing, VoiceTiming};
use expr::{BindValue, Bound, Field, Subject};
use makepad_script::*;
use place::BindFrame;
use run::{Rect, RunData, RunKey};
use std::sync::Arc;

/// One run a document can read: its data and where it is (run time and
/// placement) along the document's clock, sampled at `rate` from `start`.
#[derive(Clone, Debug, PartialEq)]
pub struct RunBinding {
    /// What `run(@name)` asks for: the Clip's id in the edit. The first
    /// binding is also `run()`.
    pub name: String,
    pub data: Arc<RunData>,
    /// Document seconds of `frames[0]`.
    pub start: f64,
    /// Samples per document second.
    pub rate: f64,
    pub frames: Vec<BindFrame>,
}

impl RunBinding {
    /// The run shown whole in a document of `size`, filling it (cropped to
    /// cover, centred) and in step with the document's clock from
    /// `run_in`: what a Motion document previewed on its own (tools,
    /// thumbnails) binds to.
    pub fn standalone(name: &str, data: Arc<RunData>, size: [f64; 2], run_in: f64) -> RunBinding {
        let (sw, sh) = data.size;
        let scale = (size[0] / sw).max(size[1] / sh);
        let (w, h) = (size[0] / scale, size[1] / scale);
        let crop = Rect::new((sw - w) * 0.5, (sh - h) * 0.5, w, h);
        let dst = Rect::new(0.0, 0.0, size[0], size[1]);
        let end = data.duration().max(1.0);
        let frames = vec![BindFrame { run_time: run_in, crop, dst }, BindFrame { run_time: run_in + end, crop, dst }];
        RunBinding { name: name.to_string(), data, start: 0.0, rate: 1.0 / end, frames }
    }

    /// The placement at document time `t`: between samples it
    /// interpolates, past the ends the run's time goes on at the last rate
    /// and the placement holds.
    pub fn at(&self, t: f64) -> BindFrame {
        let n = self.frames.len();
        if n == 0 {
            return BindFrame::identity(t, self.data.size);
        }
        let x = (t - self.start) * self.rate;
        if n == 1 || !x.is_finite() {
            return BindFrame { run_time: self.frames[0].run_time + (t - self.start), ..self.frames[0] };
        }
        if x <= 0.0 {
            let speed = (self.frames[1].run_time - self.frames[0].run_time) * self.rate;
            return BindFrame { run_time: self.frames[0].run_time + x / self.rate * speed, ..self.frames[0] };
        }
        let last = (n - 1) as f64;
        if x >= last {
            let speed = (self.frames[n - 1].run_time - self.frames[n - 2].run_time) * self.rate;
            return BindFrame { run_time: self.frames[n - 1].run_time + (x - last) / self.rate * speed, ..self.frames[n - 1] };
        }
        let i = x.floor() as usize;
        self.frames[i].lerp(&self.frames[i + 1], x - i as f64)
    }

    /// The first document time the run reaches `run_time` at (the run's
    /// time only moves forward within a clip), `None` when it never does.
    pub fn doc_time(&self, run_time: f64) -> Option<f64> {
        let n = self.frames.len();
        if n < 2 {
            return self.frames.first().map(|f| self.start + (run_time - f.run_time));
        }
        for i in 0..n - 1 {
            let (a, b) = (self.frames[i].run_time, self.frames[i + 1].run_time);
            if a <= run_time && run_time <= b && b > a {
                return Some(self.start + (i as f64 + (run_time - a) / (b - a)) / self.rate);
            }
        }
        None
    }

    /// A bound value at document time `t`.
    pub fn sample(&self, subject: Subject, field: Field, t: f64) -> BindValue {
        Bound { subject, field }.sample(&self.data, &self.at(t))
    }

    /// The run's text as voiceover-shaped words on the document's clock,
    /// so `Text{source: r.keys()}` (and `typed()`, `steps()`) draw with the
    /// voice-bound Text's machinery (`show`, `pop`, `highlight`):
    /// - `keys`: every shortcut's label, shown for `hold` seconds (or until
    ///   the next one);
    /// - `typed`: the words of each `type` call as they are typed, one line
    ///   per call;
    /// - `steps`: "n / count" from each mark to the next.
    pub fn voices(&self, hold: f64) -> Vec<(String, VoiceFeatures)> {
        let run = &*self.data;
        let secs = |frame: u64| frame as f64 / run.fps;
        let at = |frame: u64| self.doc_time(secs(frame));
        let mut keys = VoiceFeatures::default();
        let shortcuts: Vec<(f64, String)> = run.keys.iter().filter(|k| k.is_shortcut()).filter_map(|k| Some((at(k.frame())?, k.label()))).collect();
        for (i, (start, label)) in shortcuts.iter().enumerate() {
            let end = shortcuts.get(i + 1).map_or(start + hold, |next| next.0.min(start + hold));
            keys.words.push(FeatureWord { text: label.clone(), start: *start, end, emphasis: 1.0, line: i as u32, syllables: Vec::new() });
            keys.lines.push((*start, end));
        }
        let mut typed = VoiceFeatures::default();
        for call in run.calls.iter().filter(|c| c.call == "type") {
            let (Some(start), Some(end)) = (at(call.from), at(call.to)) else { continue };
            let line = typed.lines.len() as u32;
            typed.lines.push((start, end + hold));
            // Words begin at their first character, as the keys give them.
            let mut word = String::new();
            let mut word_start = None;
            let chars = run.keys.iter().filter(|k| k.frame() >= call.from && k.frame() < call.to);
            let flush = |word: &mut String, word_start: &mut Option<f64>, words: &mut Vec<FeatureWord>| {
                if let Some(start) = word_start.take() {
                    if !word.is_empty() {
                        words.push(FeatureWord { text: std::mem::take(word), start, end: end + hold, emphasis: 0.5, line, syllables: Vec::new() });
                    }
                }
                word.clear();
            };
            for k in chars {
                let text = match k {
                    RunKey::Text { text, .. } => text.as_str(),
                    _ => " ",
                };
                if text.trim().is_empty() {
                    flush(&mut word, &mut word_start, &mut typed.words);
                } else {
                    if word_start.is_none() {
                        word_start = at(k.frame());
                    }
                    word.push_str(text);
                }
            }
            flush(&mut word, &mut word_start, &mut typed.words);
        }
        let mut steps = VoiceFeatures::default();
        let count = run.marks.len();
        for (i, (_, frame)) in run.marks.iter().enumerate() {
            let Some(start) = at(*frame) else { continue };
            let end = run.marks.get(i + 1).and_then(|m| at(m.1)).or_else(|| at(run.frames)).unwrap_or(start + hold).max(start + 1e-3);
            steps.words.push(FeatureWord { text: format!("{} / {count}", i + 1), start, end, emphasis: 1.0, line: i as u32, syllables: Vec::new() });
            steps.lines.push((start, end));
        }
        for voice in [&mut keys, &mut typed, &mut steps] {
            voice.amplitude = FeatureCurve::default();
            voice.pitch = FeatureCurve::default();
        }
        vec![(voice_id("keys", &self.name), keys), (voice_id("typed", &self.name), typed), (voice_id("steps", &self.name), steps)]
    }
}

/// The voiceover id a run's text goes under: `run_keys`, `run_typed`,
/// `run_steps`, suffixed `_<name>` for a named binding.
pub fn voice_id(what: &str, name: &str) -> String {
    if name.is_empty() {
        format!("run_{what}")
    } else {
        format!("run_{what}_{name}")
    }
}

/// Every run the document's graphic is bound to (usually one).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RunContext {
    pub runs: Vec<RunBinding>,
}

/// How long a shortcut's badge text stays when nothing follows it.
pub const KEY_HOLD: f64 = 1.2;

impl RunContext {
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// `run()` (empty name: the first) or `run(@name)`.
    pub fn get(&self, name: &str) -> Option<&RunBinding> {
        if name.is_empty() {
            return self.runs.first();
        }
        self.runs.iter().find(|r| r.name == name)
    }

    /// The text voices of every binding (see [`RunBinding::voices`]),
    /// moved onto the host's clock (`offset`: the host time of the
    /// document's 0); the first binding's also go under the unsuffixed ids.
    pub fn voices(&self, offset: f64) -> Vec<(String, VoiceFeatures)> {
        let mut out = Vec::new();
        for (i, binding) in self.runs.iter().enumerate() {
            let mut voices = binding.voices(KEY_HOLD);
            if i == 0 && !binding.name.is_empty() {
                let unnamed = RunBinding { name: String::new(), ..binding.clone() };
                voices.extend(unnamed.voices(KEY_HOLD));
            }
            for (_, voice) in voices.iter_mut() {
                for word in voice.words.iter_mut() {
                    word.start += offset;
                    word.end += offset;
                }
                for line in voice.lines.iter_mut() {
                    line.0 += offset;
                    line.1 += offset;
                }
            }
            out.extend(voices);
        }
        out
    }

    /// `context` with these runs: its signals wrapped so `run()` reads
    /// them and the runs' text voices (`r.keys()`, …) are voices.
    pub fn apply(self, mut context: crate::MotionContext) -> crate::MotionContext {
        let voices = self.voices(context.offset);
        context.signals = Arc::new(RunSignals { inner: context.signals.clone(), runs: self, voices });
        context
    }
}

/// A host's signals plus the runs a bound graphic follows. Everything else
/// is the inner source's.
#[derive(Debug)]
pub struct RunSignals {
    pub inner: Arc<dyn SignalSource>,
    pub runs: RunContext,
    voices: Vec<(String, VoiceFeatures)>,
}

impl SignalSource for RunSignals {
    fn beat_index(&self, music: &str, t: f64) -> f64 {
        self.inner.beat_index(music, t)
    }
    fn bar(&self, music: &str) -> (u32, u32) {
        self.inner.bar(music)
    }
    fn bpm(&self, music: &str) -> f64 {
        self.inner.bpm(music)
    }
    fn energy(&self, music: &str, t: f64) -> f64 {
        self.inner.energy(music, t)
    }
    fn band(&self, music: &str, band: &str, t: f64) -> f64 {
        self.inner.band(music, band, t)
    }
    fn hit(&self, music: &str, t: f64, window: f64) -> f64 {
        self.inner.hit(music, t, window)
    }
    fn amplitude(&self, voice: &str, t: f64) -> f64 {
        self.inner.amplitude(voice, t)
    }
    fn pitch(&self, voice: &str, t: f64) -> f64 {
        self.inner.pitch(voice, t)
    }
    fn voice(&self, voice: &str) -> Option<&VoiceFeatures> {
        self.voices.iter().find(|(id, _)| id == voice).map(|(_, v)| v).or_else(|| self.inner.voice(voice))
    }
    fn music(&self, music: &str) -> Option<&MusicFeatures> {
        self.inner.music(music)
    }
    fn timing(&self, music: Option<&str>) -> Timing {
        let mut timing = self.inner.timing(music);
        for (id, voice) in &self.voices {
            let lines = voice
                .lines
                .iter()
                .enumerate()
                .map(|(index, (start, _))| SpokenTiming { start: *start, words: voice.words.iter().filter(|w| w.line as usize == index).map(|w| w.start).collect(), texts: voice.words.iter().filter(|w| w.line as usize == index).map(|w| w.text.clone()).collect() })
                .collect();
            timing.voices.push((id.clone(), VoiceTiming { lines }));
        }
        timing
    }
    fn runs(&self) -> Option<&RunContext> {
        Some(&self.runs)
    }
}

/// An id whose name reads back (the loader reads a voice's `id` by name).
fn interned(name: &str) -> LiveId {
    LiveId::from_str_with_intern(name, InternLiveId::Yes)
}

fn set(vm: &mut ScriptVm, object: ScriptObject, key: LiveId, value: ScriptValue) {
    vm.bx.heap_mut().set_value_def(object, key.into(), value);
}

fn arg(vm: &ScriptVm, args: ScriptObject, key: LiveId) -> ScriptValue {
    vm.bx.heap_ref().value(args, key.into(), NoTrap)
}

/// A number option of an `{…}` options object, or its default.
fn opt(vm: &ScriptVm, opts: ScriptValue, key: LiveId, default: f64) -> f64 {
    match opts.as_object() {
        Some(object) => vm.bx.heap_ref().value(object, key.into(), NoTrap).as_number().unwrap_or(default),
        None => default,
    }
}

/// An id (`@save`) or a string (`"main.save"`): a widget path or a name.
fn name_arg(vm: &ScriptVm, value: ScriptValue) -> Option<String> {
    if let Some(id) = value.as_id() {
        return Some(id.as_string(|s| s.map(str::to_string)).unwrap_or_else(|| format!("{id}")));
    }
    if let Some(s) = value.as_string() {
        return Some(vm.bx.heap_ref().string(s).to_string());
    }
    value.as_inline_string(|s| s.to_string())
}

fn value_to_script(vm: &mut ScriptVm, value: BindValue) -> ScriptValue {
    match value {
        BindValue::Num(n) => n.into(),
        BindValue::Vec2(x, y) => {
            let array = vm.bx.heap_mut().new_array();
            vm.bx.heap_mut().array_push(array, x.into(), NoTrap);
            vm.bx.heap_mut().array_push(array, y.into(), NoTrap);
            array.into()
        }
        BindValue::Vec4(v) => {
            let array = vm.bx.heap_mut().new_array();
            for n in v {
                vm.bx.heap_mut().array_push(array, n.into(), NoTrap);
            }
            array.into()
        }
        BindValue::Str(s) => vm.new_string_with(|_, out| out.push_str(&s)),
    }
}

/// An object with every field of `subject` at document time `t`.
fn fields(vm: &mut ScriptVm, binding: &RunBinding, subject: Subject, t: f64) -> ScriptValue {
    let object = vm.bx.heap_mut().new_object();
    let at = binding.at(t);
    for field in Field::of(&subject) {
        if *field == Field::Text {
            continue;
        }
        let value = Bound { subject: subject.clone(), field: *field }.sample(&binding.data, &at);
        let value = value_to_script(vm, value);
        set(vm, object, interned(field.name()), value);
    }
    object.into()
}

/// A `Voice` of the run's keys or typing, what `Text{source: …}` takes.
fn voice_source(vm: &mut ScriptVm, what: &str, name: &str) -> ScriptValue {
    let id = ScriptValue::from_id(interned(&voice_id(what, name)));
    crate::layers::make(vm, "Voice", &[(id_lut!(id), id)]).into()
}

/// `run()` / `run(@name)` in `mod.motion`: the bound run's object, built
/// once per binding (so a property function that calls `run()` every
/// frame allocates nothing new). Without a binding, `run()` is an object
/// whose every value is absent (presence 0, the frame's centre), so a
/// document previews without its run and shows no overlay.
///
/// Methods (times are seconds of the document's clock, `t` of `fn(t)`;
/// geometry is in document units):
/// - `focus(t, {pad, settle})` → `{at size centre rect presence}`
/// - `widget(path, t, {pad})` → `{at size centre rect presence}`
/// - `cursor(t)` → `{at presence}`
/// - `click(t, {n, dur})` → `{at presence age progress}`
/// - `typing(t, {pad})` → `{at size centre rect presence age progress}`
/// - `key(t, {hold})` → `{presence age progress}`
/// - `step(t)` → `{presence age index count}`
/// - `mark(name, t, {hold})` → `{presence age progress}`; `mark_time(name)`
///   → document seconds (nil when the run has no such mark)
/// - `time(t)` → the run's seconds; `scale(t)` → document units per
///   recording pixel
/// - `keys()`, `typed()`, `steps()` → text sources for `Text{source: …}`
pub fn install(vm: &mut ScriptVm, module: ScriptObject, runs: RunContext) {
    let empty = Arc::new(RunBinding {
        name: String::new(),
        data: Arc::new(RunData { fps: 60.0, size: (1.0, 1.0), ..Default::default() }),
        start: 0.0,
        rate: 1.0,
        frames: Vec::new(),
    });
    let mut objects: Vec<(String, ScriptValue)> = Vec::new();
    let bindings: Vec<Arc<RunBinding>> = runs.runs.iter().cloned().map(Arc::new).collect();
    for (i, binding) in bindings.iter().enumerate() {
        let object = run_object(vm, binding.clone());
        if i == 0 {
            objects.push((String::new(), object));
        }
        objects.push((binding.name.clone(), object));
    }
    let absent = run_object(vm, empty);
    vm.add_method(module, id_lut!(run), script_args!(name = NIL), move |vm, args| {
        let name = name_arg(vm, arg(vm, args, id!(name))).unwrap_or_default();
        objects.iter().find(|(n, _)| *n == name).map_or(absent, |(_, o)| *o)
    });
}

fn run_object(vm: &mut ScriptVm, binding: Arc<RunBinding>) -> ScriptValue {
    let object = vm.bx.heap_mut().new_object();
    let name = binding.name.clone();
    set(vm, object, id_lut!(__run), ScriptValue::from_id(interned(if name.is_empty() { "run" } else { &name })));
    set(vm, object, id_lut!(duration), binding.data.duration().into());
    set(vm, object, id_lut!(width), binding.data.size.0.into());
    set(vm, object, id_lut!(height), binding.data.size.1.into());
    let b = binding.clone();
    vm.add_method(object, id_lut!(focus), script_args!(t = NIL, opts = NIL), move |vm, args| {
        let (t, opts) = (arg(vm, args, id!(t)).as_number().unwrap_or(0.0), arg(vm, args, id!(opts)));
        let subject = Subject::Focus { pad: opt(vm, opts, id!(pad), 0.0), settle: opt(vm, opts, id!(settle), 0.3) };
        fields(vm, &b, subject, t)
    });
    let b = binding.clone();
    vm.add_method(object, id_lut!(widget), script_args!(path = NIL, t = NIL, opts = NIL), move |vm, args| {
        let path = name_arg(vm, arg(vm, args, id!(path))).unwrap_or_default();
        let (t, opts) = (arg(vm, args, id!(t)).as_number().unwrap_or(0.0), arg(vm, args, id!(opts)));
        let subject = Subject::Widget { path, pad: opt(vm, opts, id!(pad), 0.0) };
        fields(vm, &b, subject, t)
    });
    let b = binding.clone();
    vm.add_method(object, id_lut!(cursor), script_args!(t = NIL), move |vm, args| {
        let t = arg(vm, args, id!(t)).as_number().unwrap_or(0.0);
        fields(vm, &b, Subject::Cursor, t)
    });
    let b = binding.clone();
    vm.add_method(object, id_lut!(click), script_args!(t = NIL, opts = NIL), move |vm, args| {
        let (t, opts) = (arg(vm, args, id!(t)).as_number().unwrap_or(0.0), arg(vm, args, id!(opts)));
        let subject = Subject::Click { nth: opt(vm, opts, id!(n), 0.0).max(0.0) as usize, dur: opt(vm, opts, id!(dur), 0.6) };
        fields(vm, &b, subject, t)
    });
    let b = binding.clone();
    vm.add_method(object, id_lut!(typing), script_args!(t = NIL, opts = NIL), move |vm, args| {
        let (t, opts) = (arg(vm, args, id!(t)).as_number().unwrap_or(0.0), arg(vm, args, id!(opts)));
        fields(vm, &b, Subject::Typing { pad: opt(vm, opts, id!(pad), 0.0) }, t)
    });
    let b = binding.clone();
    vm.add_method(object, id_lut!(key), script_args!(t = NIL, opts = NIL), move |vm, args| {
        let (t, opts) = (arg(vm, args, id!(t)).as_number().unwrap_or(0.0), arg(vm, args, id!(opts)));
        fields(vm, &b, Subject::Key { hold: opt(vm, opts, id!(hold), KEY_HOLD) }, t)
    });
    let b = binding.clone();
    vm.add_method(object, id_lut!(step), script_args!(t = NIL), move |vm, args| {
        let t = arg(vm, args, id!(t)).as_number().unwrap_or(0.0);
        fields(vm, &b, Subject::Step, t)
    });
    let b = binding.clone();
    vm.add_method(object, id_lut!(mark), script_args!(name = NIL, t = NIL, opts = NIL), move |vm, args| {
        let name = name_arg(vm, arg(vm, args, id!(name))).unwrap_or_default();
        let (t, opts) = (arg(vm, args, id!(t)).as_number().unwrap_or(0.0), arg(vm, args, id!(opts)));
        fields(vm, &b, Subject::Mark { name, hold: opt(vm, opts, id!(hold), 1.5) }, t)
    });
    let b = binding.clone();
    vm.add_method(object, id_lut!(mark_time), script_args!(name = NIL), move |vm, args| {
        let name = name_arg(vm, arg(vm, args, id!(name))).unwrap_or_default();
        b.data.mark(&name).and_then(|m| b.doc_time(m)).map_or(NIL, |t| t.into())
    });
    let b = binding.clone();
    vm.add_method(object, id_lut!(time), script_args!(t = NIL), move |vm, args| b.at(arg(vm, args, id!(t)).as_number().unwrap_or(0.0)).run_time.into());
    let b = binding.clone();
    vm.add_method(object, id_lut!(scale), script_args!(t = NIL), move |vm, args| b.at(arg(vm, args, id!(t)).as_number().unwrap_or(0.0)).scale().0.into());
    let n = name.clone();
    vm.add_method(object, id_lut!(keys), script_args!(), move |vm, _| voice_source(vm, "keys", &n));
    let n = name.clone();
    vm.add_method(object, id_lut!(typed), script_args!(), move |vm, _| voice_source(vm, "typed", &n));
    let n = name;
    vm.add_method(object, id_lut!(steps), script_args!(), move |vm, _| voice_source(vm, "steps", &n));
    object.into()
}

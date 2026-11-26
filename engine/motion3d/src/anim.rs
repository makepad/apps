//! Keyable values: every property of a scene is a constant, keys, or sugar
//! that lowers to keys (`tween`, `loop`, `delay`), plus the procedural
//! kinds a motion piece needs (`wiggle`, `spin`, audio bindings) and, as the
//! escape hatch, a Splash closure `fn(t) {...}` evaluated by the scene's VM.
//! Evaluation is a pure function of the time and the host's audio features.
//!
//! Eases are Motion's, from `makepad_tween::named` (`@ease_out_back`,
//! `spring(k, d)`, `bezier(x1, y1, x2, y2)`).

pub use makepad_tween::named::{Curve, Ease};
use makepad_tween::named::edit_distance;

/// The closest of `names` to `wrong` (for "did you mean").
pub fn closest<'a>(wrong: &str, names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let wrong_l = wrong.to_ascii_lowercase().replace('_', "");
    names
        .into_iter()
        .map(|n| (edit_distance(&wrong_l, &n.to_ascii_lowercase().replace('_', "")), n))
        .filter(|(d, n)| *d <= (n.len() / 3).max(2))
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| n)
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

/// A per-frame audio feature a value can follow. The host
/// supplies precomputed curves, so a frame stays a pure function of `t`.
#[derive(Clone, Debug, PartialEq)]
pub enum AudioFeature {
    /// Short-term loudness 0..1.
    Energy,
    /// A band's energy 0..1: `low`/`bass`, `mid`, `high`/`treble` or an index.
    Band(String),
    /// 1 on each beat, decaying to 0 before the next (`@beat`).
    BeatPulse,
    /// Position within the beat 0..1 (`@beat_phase`).
    BeatPhase,
    /// Position within the bar 0..1 (`@bar_phase`).
    BarPhase,
    /// 1 on each onset, decaying (`@onset`).
    Onset,
    /// A voice's amplitude envelope 0..1 (`@voice`, `voice(@vo).amplitude`).
    Voice,
    /// The beat's number since the grid's first beat (not 0..1).
    BeatIndex,
}

impl AudioFeature {
    pub const NAMES: &'static [&'static str] = &["energy", "low", "bass", "mid", "high", "treble", "beat", "beat_phase", "bar_phase", "onset", "voice", "beat_index"];

    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "energy" | "level" | "loudness" => Self::Energy,
            "beat" | "kick" => Self::BeatPulse,
            "beat_phase" => Self::BeatPhase,
            "bar_phase" | "bar" => Self::BarPhase,
            "onset" | "hit" => Self::Onset,
            "voice" | "amplitude" => Self::Voice,
            "beat_index" => Self::BeatIndex,
            "low" | "bass" | "mid" | "high" | "treble" => Self::Band(id.to_string()),
            other if other.parse::<usize>().is_ok() => Self::Band(other.to_string()),
            _ => return None,
        })
    }
}

/// The host's audio analysis. `t` is the EDIT's global time (audio is
/// read on the edit's clock, not the clip's): a pulse on every beat stays on the beat wherever
/// the clip sits. A render samples precomputed analysis; inside the live VJ
/// graph the host binds the same features to the live transport and levels.
/// A missing source reads 0.
pub trait AudioSource {
    fn sample(&self, source: Option<&str>, feature: &AudioFeature, t: f32) -> f32;
}

/// No audio: every feature reads 0.
pub struct NoAudio;

impl AudioSource for NoAudio {
    fn sample(&self, _: Option<&str>, _: &AudioFeature, _: f32) -> f32 {
        0.0
    }
}

/// A deterministic stand-in track for tests and showcase strips: a kick on
/// every beat, a snare on 2 and 4, hats on the off-beats, an energy swell
/// over 8 bars and a drop at `drop` seconds.
#[derive(Clone, Debug)]
pub struct SyntheticMusic {
    pub bpm: f32,
    pub drop: f32,
}

impl Default for SyntheticMusic {
    fn default() -> Self {
        Self { bpm: 120.0, drop: 4.0 }
    }
}

impl AudioSource for SyntheticMusic {
    fn sample(&self, _: Option<&str>, feature: &AudioFeature, t: f32) -> f32 {
        let beat = (t.max(0.0) * self.bpm / 60.0) as f64;
        let phase = beat.fract() as f32;
        let beat_i = beat.floor() as i64;
        let after_drop = t >= self.drop;
        let swell = if after_drop { 1.0 } else { (t / self.drop.max(1e-3)).clamp(0.0, 1.0) * 0.6 + 0.2 };
        let kick = (-phase * 7.0).exp();
        let snare = if beat_i % 2 == 1 { (-phase * 9.0).exp() } else { 0.0 };
        let hat = { let p = (phase + 0.5).fract(); (-p * 18.0).exp() };
        match feature {
            AudioFeature::BeatPulse => kick,
            AudioFeature::BeatPhase => phase,
            AudioFeature::BarPhase => ((beat / 4.0).fract()) as f32,
            AudioFeature::Onset => kick.max(snare * 0.8),
            AudioFeature::Energy => (swell * (0.55 + 0.45 * kick)).clamp(0.0, 1.0),
            AudioFeature::Voice => 0.0,
            AudioFeature::BeatIndex => beat.floor() as f32,
            AudioFeature::Band(name) => match name.as_str() {
                "low" | "bass" | "0" => (swell * kick).clamp(0.0, 1.0),
                "mid" | "1" => (swell * (0.3 + 0.7 * snare)).clamp(0.0, 1.0),
                _ => (swell * (0.2 + 0.8 * hat) * if after_drop { 1.0 } else { 0.5 }).clamp(0.0, 1.0),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Animated values
// ---------------------------------------------------------------------------

/// One key: at `t` the value is `v`; `ease` shapes the segment arriving at
/// it from the previous key.
#[derive(Clone, Debug, PartialEq)]
pub struct Key<const N: usize> {
    pub t: f32,
    pub v: [f32; N],
    pub ease: Ease,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopMode {
    Repeat,
    PingPong,
}

/// An animated value of N floats (1: scalar, 3: vector/angles, 4: colour).
#[derive(Clone, Debug, PartialEq)]
pub enum Anim<const N: usize> {
    Const([f32; N]),
    /// Sorted by time; held before the first and after the last.
    Keys(Vec<Key<N>>),
    /// `loop(anim, period)` / `pingpong(anim, period)`: the inner value's
    /// first `period` seconds, repeated.
    Loop { inner: Box<Anim<N>>, period: f32, mode: LoopMode },
    /// `delay(anim, seconds)`: the inner value `seconds` later (staggers).
    Delay { inner: Box<Anim<N>>, seconds: f32 },
    /// `wiggle(base, amount, frequency, seed)`: smooth noise around `base`.
    Wiggle { base: Box<Anim<N>>, amount: [f32; N], frequency: f32, seed: u64 },
    /// `spin(rate)` / `spin(base, rate)`: base + rate·t (degrees per second
    /// for rotations).
    Spin { base: Box<Anim<N>>, rate: [f32; N] },
    /// `follow_music(@feature, lo, hi)`: lo + (hi − lo) · feature, where the
    /// feature is 0..1; `smooth` > 0 averages over that many seconds back.
    Audio { source: Option<String>, feature: AudioFeature, lo: [f32; N], hi: [f32; N], smooth: f32 },
    /// `add(a, b)` and `mul(a, b)` combinations (per component).
    Add(Box<Anim<N>>, Box<Anim<N>>),
    Mul(Box<Anim<N>>, Box<Anim<N>>),
    /// A Splash closure `fn(t) {...}` returning a number, vec or colour,
    /// by index into the scene's closure table.
    Script(u32),
}

impl<const N: usize> Default for Anim<N> {
    fn default() -> Self {
        Anim::Const([0.0; N])
    }
}

/// Evaluates Splash closures for [`Anim::Script`]; the scene owns the VM.
pub trait ScriptEval {
    /// `t` is the scene's local time, `edit` the edit's global time.
    fn eval_script(&mut self, index: u32, t: f32, edit: f32, out: &mut [f32]) -> bool;
    /// A closure returning an array (numbers or vectors), flattened into
    /// `out`: a per-frame kernel input (`cams: input(vec4, fn(t) {..})`).
    fn eval_script_array(&mut self, _index: u32, _t: f32, _edit: f32, _out: &mut Vec<f32>) -> bool {
        false
    }
}

/// What a value is evaluated against.
pub struct EvalCtx<'a> {
    pub audio: &'a dyn AudioSource,
    /// The edit time of the scene's t = 0: audio features are sampled at
    /// `t + edit_offset` (the edit's clock), keys at the scene's own `t`.
    pub edit_offset: f32,
    pub script: Option<&'a mut dyn ScriptEval>,
}

impl<const N: usize> Anim<N> {
    pub fn constant(v: [f32; N]) -> Self {
        Anim::Const(v)
    }

    /// `tween(from, to, start, end, ease)`.
    pub fn tween(from: [f32; N], to: [f32; N], start: f32, end: f32, ease: Ease) -> Self {
        Anim::Keys(vec![Key { t: start, v: from, ease: Ease::Linear }, Key { t: end.max(start), v: to, ease }])
    }

    /// Whether the value changes with time.
    pub fn is_static(&self) -> bool {
        match self {
            Anim::Const(_) => true,
            Anim::Keys(keys) => keys.windows(2).all(|w| w[0].v == w[1].v),
            Anim::Loop { inner, .. } | Anim::Delay { inner, .. } => inner.is_static(),
            Anim::Add(a, b) | Anim::Mul(a, b) => a.is_static() && b.is_static(),
            _ => false,
        }
    }

    /// Shift every key by `dt` seconds (a stagger or a parent's clock).
    pub fn delayed(self, dt: f32) -> Self {
        if dt == 0.0 || self.is_static() {
            return self;
        }
        Anim::Delay { inner: Box::new(self), seconds: dt }
    }

    pub fn eval(&self, t: f32, cx: &mut EvalCtx) -> [f32; N] {
        match self {
            Anim::Const(v) => *v,
            Anim::Keys(keys) => eval_keys(keys, t),
            Anim::Loop { inner, period, mode } => {
                let p = period.max(1e-4);
                let local = match mode {
                    LoopMode::Repeat => t.rem_euclid(p),
                    LoopMode::PingPong => {
                        let x = t.rem_euclid(2.0 * p);
                        if x > p { 2.0 * p - x } else { x }
                    }
                };
                inner.eval(local, cx)
            }
            Anim::Delay { inner, seconds } => inner.eval(t - seconds, cx),
            Anim::Wiggle { base, amount, frequency, seed } => {
                let mut v = base.eval(t, cx);
                for k in 0..N {
                    v[k] += amount[k] * makepad_geom3d::math::fbm3(seed.wrapping_add(k as u64 * 131), [t * frequency, k as f32 * 17.3, 0.5], 2, 0.5);
                }
                v
            }
            Anim::Spin { base, rate } => {
                let mut v = base.eval(t, cx);
                for k in 0..N {
                    v[k] += rate[k] * t;
                }
                v
            }
            Anim::Audio { source, feature, lo, hi, smooth } => {
                let f = if *smooth > 0.0 {
                    // A box average over the last `smooth` seconds (8 taps).
                    let mut s = 0.0;
                    for i in 0..8 {
                        s += cx.audio.sample(source.as_deref(), feature, t + cx.edit_offset - smooth * i as f32 / 8.0);
                    }
                    s / 8.0
                } else {
                    cx.audio.sample(source.as_deref(), feature, t + cx.edit_offset)
                };
                let mut v = [0.0; N];
                for k in 0..N {
                    v[k] = lo[k] + (hi[k] - lo[k]) * f;
                }
                v
            }
            Anim::Add(a, b) => {
                let (x, y) = (a.eval(t, cx), b.eval(t, cx));
                std::array::from_fn(|k| x[k] + y[k])
            }
            Anim::Mul(a, b) => {
                let (x, y) = (a.eval(t, cx), b.eval(t, cx));
                std::array::from_fn(|k| x[k] * y[k])
            }
            Anim::Script(index) => {
                let mut out = [0.0; N];
                if let Some(script) = cx.script.as_deref_mut() {
                    script.eval_script(*index, t, t + cx.edit_offset, &mut out);
                }
                out
            }
        }
    }

    /// The last key's time, when the value has keys (for durations).
    pub fn end_time(&self) -> Option<f32> {
        match self {
            Anim::Keys(keys) => keys.last().map(|k| k.t),
            Anim::Delay { inner, seconds } => inner.end_time().map(|e| e + seconds),
            Anim::Add(a, b) | Anim::Mul(a, b) => match (a.end_time(), b.end_time()) {
                (Some(x), Some(y)) => Some(x.max(y)),
                (x, y) => x.or(y),
            },
            _ => None,
        }
    }
}

fn eval_keys<const N: usize>(keys: &[Key<N>], t: f32) -> [f32; N] {
    let Some(first) = keys.first() else { return [0.0; N] };
    if t.is_nan() || t <= first.t || keys.len() == 1 {
        return first.v;
    }
    let last = keys.last().unwrap();
    if t >= last.t {
        return last.v;
    }
    // Keys are sorted at parse; binary search the segment.
    let i = keys.partition_point(|k| k.t <= t).clamp(1, keys.len() - 1);
    let (a, b) = (&keys[i - 1], &keys[i]);
    let span = b.t - a.t;
    if span <= 0.0 {
        return b.v;
    }
    let f = b.ease.apply(((t - a.t) / span) as f64, span as f64) as f32;
    std::array::from_fn(|k| a.v[k] + (b.v[k] - a.v[k]) * f)
}

pub type Anim1 = Anim<1>;
pub type Anim3 = Anim<3>;
pub type Anim4 = Anim<4>;

impl Anim<1> {
    pub fn scalar(v: f32) -> Self {
        Anim::Const([v])
    }
    pub fn get(&self, t: f32, cx: &mut EvalCtx) -> f32 {
        self.eval(t, cx)[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx<'a>(audio: &'a dyn AudioSource) -> EvalCtx<'a> {
        EvalCtx { audio, edit_offset: 0.0, script: None }
    }

    #[test]
    fn eases_start_and_end_on_their_keys() {
        for id in Ease::ids() {
            let e = Ease::from_id(&id).unwrap();
            assert_eq!(e.apply(0.0, 1.0), 0.0, "{id}");
            assert_eq!(e.apply(1.0, 1.0), 1.0, "{id}");
        }
        let css_ease = Ease::Bezier { x1: 0.25, y1: 0.1, x2: 0.25, y2: 1.0 };
        assert!((css_ease.apply(0.5, 1.0) - 0.8024).abs() < 0.01, "{}", css_ease.apply(0.5, 1.0));
        assert!((Ease::Bezier { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 }.apply(0.3, 1.0) - 0.3).abs() < 1e-3);
    }

    #[test]
    fn tween_holds_outside_and_eases_inside() {
        let a = Anim3::tween([0.0, 0.0, 0.0], [10.0, 20.0, 30.0], 1.0, 3.0, Ease::Linear);
        let mut c = cx(&NoAudio);
        assert_eq!(a.eval(0.0, &mut c), [0.0, 0.0, 0.0]);
        assert_eq!(a.eval(2.0, &mut c), [5.0, 10.0, 15.0]);
        assert_eq!(a.eval(9.0, &mut c), [10.0, 20.0, 30.0]);
        assert!(!a.is_static());
        assert_eq!(a.end_time(), Some(3.0));
    }

    #[test]
    fn loop_delay_spin_wiggle() {
        let base = Anim1::tween([0.0], [1.0], 0.0, 1.0, Ease::Linear);
        let mut c = cx(&NoAudio);
        let looped = Anim1::Loop { inner: Box::new(base.clone()), period: 1.0, mode: LoopMode::Repeat };
        assert!((looped.get(2.25, &mut c) - 0.25).abs() < 1e-6);
        let pp = Anim1::Loop { inner: Box::new(base.clone()), period: 1.0, mode: LoopMode::PingPong };
        assert!((pp.get(1.25, &mut c) - 0.75).abs() < 1e-6);
        assert!((base.clone().delayed(0.5).get(0.75, &mut c) - 0.25).abs() < 1e-6);
        let spin = Anim1::Spin { base: Box::new(Anim1::scalar(10.0)), rate: [90.0] };
        assert_eq!(spin.get(2.0, &mut c), 190.0);
        let w = Anim1::Wiggle { base: Box::new(Anim1::scalar(1.0)), amount: [0.5], frequency: 2.0, seed: 4 };
        let a = w.get(0.3, &mut c);
        assert_eq!(a, w.get(0.3, &mut c));
        assert!((a - 1.0).abs() <= 0.5);
    }

    #[test]
    fn audio_maps_feature_to_range() {
        let music = SyntheticMusic::default();
        let mut c = cx(&music);
        let a = Anim1::Audio { source: None, feature: AudioFeature::BeatPulse, lo: [1.0], hi: [2.0], smooth: 0.0 };
        assert!((a.get(0.0, &mut c) - 2.0).abs() < 1e-5, "on the beat");
        assert!(a.get(0.4, &mut c) < 1.2, "decayed before the next");
        assert_eq!(a.get(1.0, &mut c), a.get(1.0, &mut c));
        // The same scene placed 0.25 s later on the edit: its local 0 is
        // no longer on a beat, its local 0.25 is (the grid is the edit's).
        let mut late = EvalCtx { audio: &music, edit_offset: 0.25, script: None };
        assert!(a.get(0.0, &mut late) < 1.5);
        assert!((a.get(0.25, &mut late) - 2.0).abs() < 1e-5);
    }

    #[test]
    fn closest_names() {
        assert_eq!(closest("metalnes", ["metalness", "roughness"]), Some("metalness"));
        assert_eq!(closest("emissiveIntensity", ["emissive_intensity", "emissive"]), Some("emissive_intensity"));
        assert_eq!(closest("zzzzzz", ["metalness"]), None);
    }
}

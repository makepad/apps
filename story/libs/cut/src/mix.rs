//! The cut's mix: every sound of a cut, from
//! decoded PCM, at its gain and pan per sample (keys, fades and ducking
//! included), band-limited resampled into 48 kHz stereo, in pieces of any
//! size (the export pushes one video frame's worth at a time); and the
//! loudness normalization of the whole mix. Pure: no clock, no device, no
//! renderer, so the desktop's export and preview and the web film all mix
//! with this one code.

use relax_attr::relax;
use crate::audio::{db_to_gain, loudness_gain};
use crate::{AssetRef, CutDoc, CutEval, CutTree, MediaRef, RunRef, TakeInfo};
use makepad_audio_dsp::loudness::LoudnessMeter;
use makepad_stage_audio_media::pcm::TrackPcm;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The mix's rate.
pub const SAMPLE_RATE: u32 = 48_000;

/// The sample peak a normalized mix may reach: -1 dBFS.
pub const PEAK_CEILING_DB: f64 = -1.0;
/// Output samples measured at a time while normalizing.
const MEASURE_BLOCK: usize = SAMPLE_RATE as usize;

/// Everything the mixdown reads: the cut, the marks of its runs, its
/// voice takes' timings and the decoded sound of every source it mixes.
/// Shared with the encoder worker.
pub struct AudioMix {
    pub doc: Arc<CutDoc>,
    /// The cut with every cut it nests; `None` evaluates `doc`
    /// alone.
    pub tree: Option<Arc<CutTree>>,
    pub marks: HashMap<RunRef, HashMap<String, f64>>,
    pub takes: HashMap<AssetRef, TakeInfo>,
    pub sources: HashMap<MediaRef, Arc<TrackPcm>>,
    /// The whole mix's gain: 1, or what brings it to `output.loudness`
    /// ([`AudioMix::normalize`]).
    pub gain: f64,
    /// Resampling kernels by (source rate, speed).
    pub kernels: Mutex<HashMap<(u32, u64), Arc<Kernel>>>,
}

#[relax(O3, always)]
impl AudioMix {
    pub fn new(doc: Arc<CutDoc>, sources: HashMap<MediaRef, Arc<TrackPcm>>) -> Self {
        Self { doc, tree: None, marks: HashMap::new(), takes: HashMap::new(), sources, gain: 1.0, kernels: Mutex::new(HashMap::new()) }
    }

    fn eval(&self) -> CutEval<'_> {
        let mut eval = match &self.tree {
            Some(tree) => tree.eval(),
            None => CutEval::new(&self.doc),
        };
        for (run, marks) in &self.marks {
            eval.set_marks(run.clone(), marks.clone());
        }
        for (take, info) in &self.takes {
            eval.set_take(take.clone(), info.clone());
        }
        eval
    }

    /// `count` stereo frames of the mix from output sample `first`,
    /// interleaved i16, at the mix's gain. Normalization keeps its peaks
    /// under [`PEAK_CEILING_DB`]; an authored mix past full scale clips.
    pub fn mix(&self, first: u64, count: usize) -> Vec<i16> {
        let gain = self.gain as f32;
        self.mix_f32(first, count).into_iter().map(|v| ((v * gain).clamp(-1.0, 1.0) * 32767.0).round() as i16).collect()
    }

    /// Measure the whole mix of `samples` stereo frames (BS.1770-4) in
    /// blocks, never holding it, and set the gain that brings it to
    /// `output.loudness`, capped so the sample peak stays under
    /// [`PEAK_CEILING_DB`]. A silent mix, or no target, keeps gain 1.
    /// Returns the measured loudness and whether the peak capped the gain.
    pub fn normalize(&mut self, samples: u64) -> Option<(f64, bool)> {
        let mut normalizer = Normalizer::new(samples);
        while normalizer.step(self, MEASURE_BLOCK) {}
        normalizer.finish(self)
    }

    fn kernel(&self, rate: u32, step: f64) -> Arc<Kernel> {
        let key = (rate, step.to_bits());
        let mut kernels = self.kernels.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        kernels.entry(key).or_insert_with(|| Arc::new(Kernel::new(step))).clone()
    }

    /// The mix before its gain, interleaved, full scale 1.
    pub fn mix_f32(&self, first: u64, count: usize) -> Vec<f32> {
        let eval = self.eval();
        let rate = SAMPLE_RATE as f64;
        let mut acc = vec![0f32; count * 2];
        let t0 = first as f64 / rate;
        let t1 = (first + count as u64) as f64 / rate;
        for segment in eval.audio_plan(t0, t1).segments {
            let (Some(pcm), Some(source_t0)) = (self.sources.get(&segment.media), segment.source_t0) else {
                continue;
            };
            if pcm.frames.is_empty() || pcm.sample_rate == 0 {
                continue;
            }
            // The samples whose time lies in [segment.t0, segment.t1).
            let start = ((segment.t0 * rate - 1e-6).ceil() as i64 - first as i64).clamp(0, count as i64) as usize;
            let end = ((segment.t1 * rate - 1e-6).ceil() as i64 - first as i64).clamp(0, count as i64) as usize;
            let source_rate = pcm.sample_rate as f64;
            // Source samples per output sample.
            let step = source_rate / rate * segment.speed.abs();
            let exact = pcm.sample_rate == SAMPLE_RATE && segment.speed == 1.0;
            let kernel = (!exact).then(|| self.kernel(pcm.sample_rate, step));
            for i in start..end {
                let t = (first + i as u64) as f64 / rate;
                // Along the segment's path: a nested sound carries every
                // compound clip's gain and pan on the way.
                let (gain, pan) = if segment.keyed { (eval.segment_gain(&segment, t), eval.segment_pan(&segment, t)) } else { (segment.gain.0, segment.pan.0) };
                if gain <= 0.0 {
                    continue;
                }
                let at = (source_t0 + (t - segment.t0) * segment.speed) * source_rate;
                // Same rate and speed: the nearest source sample, exactly
                // (at most half a sample, 10 µs, of shift); otherwise the
                // band-limited kernel.
                let [l, r] = match &kernel {
                    None => frame_at(pcm, at.round()),
                    Some(kernel) => kernel.sample(pcm, at),
                };
                let [gl, gr] = balance(pan);
                acc[i * 2] += l * (gain * gl) as f32;
                acc[i * 2 + 1] += r * (gain * gr) as f32;
            }
        }
        acc
    }
}

/// [`AudioMix::normalize`] in steps, for a host that must not spend a long
/// stretch on it at once (a block of the mix measured per step).
pub struct Normalizer {
    meter: LoudnessMeter,
    peak: f32,
    first: u64,
    samples: u64,
}

impl Normalizer {
    pub fn new(samples: u64) -> Self {
        Self { meter: LoudnessMeter::new(SAMPLE_RATE), peak: 0.0, first: 0, samples }
    }

    /// Measure the next `frames` of the mix; false once all is measured
    /// (or the cut asks for no loudness).
    pub fn step(&mut self, mix: &AudioMix, frames: usize) -> bool {
        if mix.doc.output.loudness.is_none() || self.first >= self.samples {
            return false;
        }
        let count = (self.samples - self.first).min(frames.max(1) as u64) as usize;
        let block = mix.mix_f32(self.first, count);
        self.peak = block.iter().fold(self.peak, |peak, v| peak.max(v.abs()));
        self.meter.push(&block);
        self.first += count as u64;
        self.first < self.samples
    }

    /// Set the mix's gain from what was measured; the measured loudness and
    /// whether the peak capped the gain.
    pub fn finish(self, mix: &mut AudioMix) -> Option<(f64, bool)> {
        let target = mix.doc.output.loudness?;
        let measured = self.meter.finish()?;
        let mut gain = loudness_gain(measured, target);
        let ceiling = db_to_gain(PEAK_CEILING_DB);
        let capped = self.peak as f64 * gain > ceiling;
        if capped {
            gain = ceiling / self.peak as f64;
        }
        mix.gain = gain;
        Some((measured, capped))
    }
}

/// Source frame `index`; silence outside the clip.
#[relax(O3, always)]
fn frame_at(pcm: &TrackPcm, index: f64) -> [f32; 2] {
    if !(index >= 0.0) || index as usize >= pcm.frames.len() {
        return [0.0, 0.0];
    }
    pcm.frame_f32(index as usize)
}

/// A windowed-sinc resampling kernel (Blackman window) as a polyphase
/// table: [`KERNEL_HALF`] taps either side of the sample per unit of
/// down-sampling, cut off at 96% of the lower Nyquist frequency, so
/// neither a rate change nor a speed-up aliases and the pass band stays
/// flat to about 18 kHz.
pub struct Kernel {
    half: i64,
    phases: usize,
    /// `phases + 1` rows of `2 * half` weights, each row summing to 1.
    table: Vec<f32>,
}

/// Taps either side of the sample, per unit of down-sampling.
pub const KERNEL_HALF: i64 = 32;
const KERNEL_PHASES: usize = 1024;

#[relax(O3, always)]
impl Kernel {
    /// The kernel for `step` source samples per output sample.
    pub fn new(step: f64) -> Self {
        let scale = step.max(1.0);
        let half = (KERNEL_HALF as f64 * scale).ceil() as i64;
        let cutoff = 0.5 * 0.96 / scale;
        let taps = (2 * half) as usize;
        let mut table = Vec::with_capacity((KERNEL_PHASES + 1) * taps);
        for phase in 0..=KERNEL_PHASES {
            let frac = phase as f64 / KERNEL_PHASES as f64;
            let row: Vec<f64> = (0..taps)
                .map(|j| {
                    // Distance from the wanted position to tap j.
                    let x = (j as i64 - half + 1) as f64 - frac;
                    let u = x / half as f64;
                    if u.abs() >= 1.0 {
                        return 0.0;
                    }
                    let w = (u + 1.0) * 0.5;
                    let window = 0.42 - 0.5 * (std::f64::consts::TAU * w).cos() + 0.08 * (2.0 * std::f64::consts::TAU * w).cos();
                    let arg = 2.0 * cutoff * x;
                    let sinc = if arg.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * arg).sin() / (std::f64::consts::PI * arg) };
                    2.0 * cutoff * sinc * window
                })
                .collect();
            let sum: f64 = row.iter().sum();
            table.extend(row.iter().map(|v| (v / sum) as f32));
        }
        Self { half, phases: KERNEL_PHASES, table }
    }

    /// The band-limited value at fractional source index `at`.
    pub fn sample(&self, pcm: &TrackPcm, at: f64) -> [f32; 2] {
        if !at.is_finite() {
            return [0.0, 0.0];
        }
        let base = at.floor();
        let phase = ((at - base) * self.phases as f64).round() as usize;
        let taps = (2 * self.half) as usize;
        let row = &self.table[phase * taps..(phase + 1) * taps];
        let first = base as i64 - self.half + 1;
        let len = pcm.frames.len() as i64;
        let mut out = [0f32; 2];
        for (j, weight) in row.iter().enumerate() {
            let index = first + j as i64;
            if index < 0 || index >= len {
                continue;
            }
            let [l, r] = pcm.frame_f32(index as usize);
            out[0] += l * weight;
            out[1] += r * weight;
        }
        out
    }
}

/// Stereo balance: centre leaves both sides at unity, full left silences
/// the right side and the reverse.
#[relax(O3, always)]
fn balance(pan: f64) -> [f64; 2] {
    let pan = pan.clamp(-1.0, 1.0);
    [(1.0 - pan).min(1.0), (1.0 + pan).min(1.0)]
}

#[cfg(test)]
mod tests {
    use super::*;
    

    const A: &str = "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu";
    const A_REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";

    fn doc(audio: &str) -> Arc<CutDoc> {
        doc_with(audio, "")
    }

    fn doc_with(audio: &str, output: &str) -> Arc<CutDoc> {
        let source = format!(
            "use mod.flow.*\nuse mod.cut.*\n{audio}\nlet title = Title{{text: \"t\"}}\n\
             let out = CutOutput{{frame: title.out(@frame) audio: [conn(1, music.out(@audio))]}}\n\
             Flow{{\n kind: @cut\n output: {{width: 64 height: 36 fps: 30 {output}}}\n duration: 2.0\n music, title, out\n}}\n"
        );
        Arc::new(CutDoc::load(&source).unwrap_or_else(|errors| panic!("{errors:?}")))
    }

    /// A constant 0.5 on the left and -0.25 on the right at 48 kHz.
    fn dc(seconds: f64, rate: u32) -> Arc<TrackPcm> {
        let frames = (seconds * rate as f64) as usize;
        Arc::new(TrackPcm { frames: vec![[16384, -8192]; frames], sample_rate: rate })
    }

    fn mix(doc: Arc<CutDoc>, pcm: Arc<TrackPcm>) -> AudioMix {
        let media = MediaRef::Asset(AssetRef { id: A.into(), rev: A_REV.into() });
        AudioMix::new(doc, HashMap::from([(media, pcm)]))
    }


    #[test]
    fn a_span_gates_the_sound_to_the_sample() {
        let doc = doc(&format!("let music = AudioClip{{src: asset(\"{A}\", \"{A_REV}\") span: [0.5, 1.0] gain: 0.5}}"));
        let out = mix(doc, dc(3.0, 48_000)).mix(0, 96_000);
        let at = |i: usize| (out[i * 2], out[i * 2 + 1]);
        assert_eq!(at(23_999), (0, 0));
        // 0.5 * 0.5 of full scale on the left, 0.5 * -0.25 on the right.
        assert_eq!(at(24_000), (8192, -4096));
        assert_eq!(at(71_999), (8192, -4096));
        assert_eq!(at(72_000), (0, 0));
    }

    #[test]
    fn keyed_gain_and_pan_are_exact_per_sample_and_resample() {
        let doc = doc(&format!(
            "let music = AudioClip{{src: asset(\"{A}\", \"{A_REV}\") in: 1.0 span: [0.0, 2.0] gain: keys([0.0, 0.0], [1.0, 1.0]) pan: -1.0}}"
        ));
        // A 24 kHz source plays at its own pitch: source time is edit time.
        let pcm = Arc::new(TrackPcm { frames: (0..72_000).map(|i| [(i / 10) as i16, 0]).collect(), sample_rate: 24_000 });
        let mixer = mix(doc, pcm);
        let whole = mixer.mix(0, 96_000);
        // Chunked in frame-sized pieces the mix is the same, sample for sample.
        let mut chunks = Vec::new();
        for n in 0..60 {
            // 30 fps: 1600 samples a frame.
            let (a, b) = (n * 1600, (n + 1) * 1600);
            chunks.extend(mixer.mix(a, (b - a) as usize));
        }
        assert_eq!(whole, chunks);
        // Halfway up the gain ramp: edit 0.5000417 s is source 1.5000417 s,
        // frame 36001 at 24 kHz on a straight ramp of i / 10, which a
        // symmetric kernel reproduces exactly: 3600.1.
        let left = |i: usize| whole[i * 2] as f64 / 32767.0;
        let expected = (24_002.0 / 48_000.0) * 3600.1 / 32768.0;
        assert!((left(24_002) - expected).abs() < 1e-4, "{} != {expected}", left(24_002));
        // Full left: the right side is silent everywhere.
        assert!(whole.iter().skip(1).step_by(2).all(|&r| r == 0));
    }

    #[test]
    fn output_loudness_normalizes_the_whole_mix() {
        let doc = doc_with(&format!("let music = AudioClip{{src: asset(\"{A}\", \"{A_REV}\") span: [0.0, 2.0]}}"), "loudness: -14");
        let tone = |i: usize| ((i as f64 / 48_000.0 * 1000.0 * std::f64::consts::TAU).sin() * 0.05 * 32767.0) as i16;
        let pcm = Arc::new(TrackPcm { frames: (0..96_000).map(|i| [tone(i), tone(i)]).collect(), sample_rate: 48_000 });
        let mut mixer = mix(doc, pcm);
        let (before, capped) = mixer.normalize(96_000).expect("measured");
        assert!(before < -20.0 && !capped, "{before}");
        let after: Vec<f32> = mixer.mix(0, 96_000).iter().map(|&v| v as f32 / 32767.0).collect();
        let lufs = makepad_audio_dsp::loudness::integrated_lufs(&after, SAMPLE_RATE).expect("loud");
        assert!((lufs + 14.0).abs() < 0.1, "{lufs}");
    }

    fn tone(freq: f64, rate: u32, seconds: f64, amplitude: f64) -> Arc<TrackPcm> {
        let frames = (seconds * rate as f64) as usize;
        let v = |i: usize| ((i as f64 / rate as f64 * freq * std::f64::consts::TAU).sin() * amplitude * 32767.0) as i16;
        Arc::new(TrackPcm { frames: (0..frames).map(|i| [v(i), v(i)]).collect(), sample_rate: rate })
    }

    fn rms_db(samples: &[i16]) -> f64 {
        let left: Vec<f64> = samples.iter().step_by(2).map(|&v| v as f64 / 32767.0).collect();
        10.0 * (left.iter().map(|v| v * v).sum::<f64>() / left.len() as f64).log10()
    }

    fn played(pcm: Arc<TrackPcm>, source_in: f64) -> Vec<i16> {
        let doc = doc(&format!("let music = AudioClip{{src: asset(\"{A}\", \"{A_REV}\") in: {source_in} span: [0.0, 1.0]}}"));
        // The middle half second: no edge effects.
        mix(doc, pcm).mix(12_000, 24_000)
    }

    #[test]
    fn resampling_keeps_the_pass_band_and_refuses_what_is_past_nyquist() {
        // A 0.5 amplitude sine is -9.03 dB RMS.
        let reference = -9.03;
        let high = rms_db(&played(tone(15_000.0, 44_100, 1.2, 0.5), 0.0));
        assert!((high - reference).abs() < 0.5, "15 kHz from 44.1 kHz: {high} dB");
        let off_grid = rms_db(&played(tone(15_000.0, 48_000, 1.2, 0.5), 0.123_456_7));
        assert!((off_grid - reference).abs() < 0.1, "15 kHz at 48 kHz, off the grid: {off_grid} dB");
        // 30 kHz in a 96 kHz source has no place in 48 kHz: it must not
        // fold back to 18 kHz.
        let folded = rms_db(&played(tone(30_000.0, 96_000, 1.2, 0.5), 0.0));
        assert!(folded < reference - 40.0, "30 kHz from 96 kHz folded to {folded} dB");
    }

    #[test]
    fn the_streaming_meter_agrees_with_the_whole_measurement() {
        let pcm = tone(997.0, 48_000, 3.0, 0.25);
        let samples: Vec<f32> = pcm.frames.iter().flat_map(|f| [f[0] as f32 / 32768.0, f[1] as f32 / 32768.0]).collect();
        let whole = makepad_audio_dsp::loudness::integrated_lufs(&samples, 48_000).unwrap();
        let mut meter = LoudnessMeter::new(SAMPLE_RATE);
        for piece in samples.chunks(1234 * 2) {
            meter.push(piece);
        }
        let streamed = meter.finish().unwrap();
        assert!((whole - streamed).abs() < 0.01, "{whole} vs {streamed}");
    }

    #[test]
    fn normalization_never_pushes_peaks_past_the_ceiling() {
        // A quiet tone with sharp clicks asked to reach -8 LUFS: the gain
        // that needs would put the clicks far past full scale.
        let doc = doc_with(&format!("let music = AudioClip{{src: asset(\"{A}\", \"{A_REV}\") span: [0.0, 2.0]}}"), "loudness: -8");
        let mut pcm = (*tone(1000.0, 48_000, 2.0, 0.1)).frames.clone();
        for frame in pcm.iter_mut().step_by(4800) {
            *frame = [26000, 26000];
        }
        let mut mixer = mix(doc, Arc::new(TrackPcm { frames: pcm, sample_rate: 48_000 }));
        let (measured, capped) = mixer.normalize(96_000).unwrap();
        assert!(capped, "measured {measured}, gain {}", mixer.gain);
        let peak = mixer.mix(0, 96_000).iter().map(|v| v.unsigned_abs()).max().unwrap() as f64 / 32767.0;
        assert!(peak <= db_to_gain(PEAK_CEILING_DB) + 1e-3, "{peak}");
    }
}

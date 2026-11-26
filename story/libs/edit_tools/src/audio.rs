//! `audio analyze`: the cut's mixdown exactly as a render mixes it (the
//! renderer's `AudioMix`, loudness normalization included), measured per
//! segment and whole: integrated loudness (BS.1770-4, L8's
//! `LoudnessMeter`), true peak (4x oversampled), sample peak, clipped
//! samples and silent stretches. Segments are cut at every edge of every
//! sound (clip, audio clip, voice line), so a line that is too quiet under
//! the music shows as its own row.
//!
//! [`analyze`] decodes every source first (files or the store: call it on a
//! worker); [`analyze_mix`] measures a mix that is already decoded (an
//! engine's `audio_mix()` in process).
//!
//! `audio analyze --waveform` adds the mix's waveform ([`waveform`]: min
//! and max per stretch, and a picture), and `audio at <t>` ([`at`],
//! [`at_mix`]) what sounds at one moment: each source with its gain, pan
//! and duck, and the mix's level, spectrum and waveform there (the same
//! spectrum and waveform a Motion document reads from a track,
//! `music(@m).spectrum(t, n)` and `.wave(t, n, window)`).

use crate::input::Document;
use crate::inspect::num;
use makepad_asset_data::MediaType;
use makepad_stage_audio_media::decode::{decode_audio_clip, open_container_audio};
use makepad_stage_audio_media::pcm::TrackPcm;
use makepad_story_edit_render::audio::{fps_ratio, frame_count, frame_sample};
use makepad_story_cut::mix::{AudioMix, SAMPLE_RATE};
use makepad_story_edit_render::engine::tree_nodes;
use makepad_story_edit_render::{MediaKind, MediaResolver, ResolvedMedia};
use makepad_audio_dsp::loudness::LoudnessMeter;
use makepad_story_cut::*;
use makepad_strict_json::{obj, s, Value};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct AudioOptions {
    /// A 50 ms window quieter than this (dBFS, RMS) is silent.
    pub silence_db: f64,
    /// Silent stretches shorter than this (seconds) are not reported.
    pub min_silence: f64,
    /// A sample at or above this magnitude (full scale 1) counts as clipped.
    pub clip_level: f32,
}

impl Default for AudioOptions {
    fn default() -> Self {
        Self { silence_db: -50.0, min_silence: 0.5, clip_level: 0.999 }
    }
}

/// One stretch of the mix between two edges of its sounds.
#[derive(Clone, Debug, PartialEq)]
pub struct SegmentLevels {
    pub start: f64,
    pub end: f64,
    /// The nodes sounding in it.
    pub sources: Vec<String>,
    /// Integrated loudness of the stretch; `None` when it is silent or
    /// shorter than one 400 ms block.
    pub lufs: Option<f64>,
    pub true_peak_db: f64,
    pub sample_peak_db: f64,
    pub clipped: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioReport {
    pub duration: f64,
    /// `output.loudness`, when the cut normalizes its mix.
    pub target: Option<f64>,
    /// The gain the render applies to the whole mix (dB; 0 without a
    /// target).
    pub gain_db: f64,
    pub lufs: Option<f64>,
    pub true_peak_db: f64,
    pub sample_peak_db: f64,
    pub clipped: u64,
    /// Stretches with clipped samples (merged when closer than 100 ms).
    pub clipping: Vec<(f64, f64)>,
    pub silence: Vec<(f64, f64)>,
    pub segments: Vec<SegmentLevels>,
    /// Sources the mix plays silent, and why.
    pub silent_sources: Vec<(String, String)>,
}

fn db(value: f64) -> f64 {
    if value > 0.0 {
        20.0 * value.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Taps per phase of the true-peak interpolator.
const TP_TAPS: usize = 16;

/// A 4x oversampling interpolator (windowed sinc, Blackman) for the true
/// peak of BS.1770-4 Annex 2: the peak of the three samples between every
/// pair, besides the samples themselves.
struct TruePeak {
    phases: [[f32; TP_TAPS]; 3],
    history: [[f32; TP_TAPS]; 2],
    at: usize,
    peak: f32,
}

impl TruePeak {
    fn new() -> Self {
        let mut phases = [[0f32; TP_TAPS]; 3];
        let half = TP_TAPS as f64 / 2.0;
        for (p, phase) in phases.iter_mut().enumerate() {
            let frac = (p + 1) as f64 / 4.0;
            let mut sum = 0.0;
            for (k, tap) in phase.iter_mut().enumerate() {
                // Tap k weighs the sample k - half + 1 steps from the newest
                // pair's older end; x is its distance from the point.
                let x = k as f64 - (half - 1.0) - frac;
                let sinc = if x.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
                let n = (k as f64 + 1.0 - frac) / TP_TAPS as f64;
                let window = 0.42 - 0.5 * (std::f64::consts::TAU * n).cos() + 0.08 * (2.0 * std::f64::consts::TAU * n).cos();
                *tap = (sinc * window) as f32;
                sum += sinc * window;
            }
            for tap in phase.iter_mut() {
                *tap /= sum as f32;
            }
        }
        Self { phases, history: [[0.0; TP_TAPS]; 2], at: 0, peak: 0.0 }
    }

    fn push(&mut self, frame: [f32; 2]) {
        for channel in 0..2 {
            self.history[channel][self.at] = frame[channel];
            self.peak = self.peak.max(frame[channel].abs());
        }
        self.at = (self.at + 1) % TP_TAPS;
        for channel in 0..2 {
            let history = &self.history[channel];
            for phase in &self.phases {
                let mut y = 0f32;
                for (k, tap) in phase.iter().enumerate() {
                    y += tap * history[(self.at + k) % TP_TAPS];
                }
                self.peak = self.peak.max(y.abs());
            }
        }
    }
}

/// Merge `[t, t + len)` stretches closer than `gap` into ranges.
fn merge(mut ranges: Vec<(f64, f64)>, gap: f64) -> Vec<(f64, f64)> {
    ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (t0, t1) in ranges {
        match out.last_mut() {
            Some(last) if t0 <= last.1 + gap => last.1 = last.1.max(t1),
            _ => out.push((t0, t1)),
        }
    }
    out
}

/// The cuts of the mix: every edge of every sound the plan plays, and the
/// sounding nodes of each stretch.
fn segments_of(eval: &CutEval<'_>, duration: f64) -> Vec<(f64, f64, Vec<String>)> {
    let plan = eval.audio_plan(0.0, duration).segments;
    let mut cuts = vec![0.0, duration];
    for segment in &plan {
        cuts.push(segment.t0.clamp(0.0, duration));
        cuts.push(segment.t1.clamp(0.0, duration));
    }
    cuts.sort_by(f64::total_cmp);
    cuts.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    cuts.windows(2)
        .filter(|pair| pair[1] - pair[0] > 1e-6)
        .map(|pair| {
            let mid = (pair[0] + pair[1]) / 2.0;
            let mut sources: Vec<String> = plan
                .iter()
                .filter(|seg| seg.t0 <= mid && mid < seg.t1)
                .map(|seg| match seg.line {
                    Some(line) => format!("{} line {line}", seg.node),
                    None => seg.node.clone(),
                })
                .collect();
            sources.dedup();
            (pair[0], pair[1], sources)
        })
        .collect()
}

/// The mix's cut evaluated with its marks and takes.
fn mix_eval(mix: &AudioMix) -> CutEval<'_> {
    let mut eval = match &mix.tree {
        Some(tree) => tree.eval(),
        None => CutEval::new(&mix.doc),
    };
    for (run, marks) in &mix.marks {
        eval.set_marks(run.clone(), marks.clone());
    }
    for (take, info) in &mix.takes {
        eval.set_take(take.clone(), info.clone());
    }
    eval
}

/// Measure a decoded mix over `[0, duration)`, at the gain it carries.
pub fn analyze_mix(mix: &AudioMix, duration: f64, options: &AudioOptions) -> AudioReport {
    let eval = mix_eval(mix);
    let cuts = segments_of(&eval, duration);
    drop(eval);
    let rate = SAMPLE_RATE as f64;
    let fps = fps_ratio(mix.doc.output.fps);
    let total = frame_sample(frame_count(duration, fps), fps);
    let gain = mix.gain as f32;
    let window = (rate * 0.05) as usize;
    let silence_floor = crate::audio::db_to_power(options.silence_db);

    let mut whole = LoudnessMeter::new(SAMPLE_RATE);
    let mut whole_tp = TruePeak::new();
    let mut sample_peak = 0f32;
    let mut clipped = 0u64;
    let mut clip_ranges = Vec::new();
    let mut silent_windows = Vec::new();
    let mut window_energy = 0f64;
    let mut window_len = 0usize;
    let mut segments: Vec<(LoudnessMeter, TruePeak, f32, u64)> = cuts.iter().map(|_| (LoudnessMeter::new(SAMPLE_RATE), TruePeak::new(), 0f32, 0u64)).collect();
    let mut segment = 0usize;

    let mut first = 0u64;
    while first < total {
        let count = (total - first).min(SAMPLE_RATE as u64) as usize;
        let mut block = mix.mix_f32(first, count);
        for v in block.iter_mut() {
            *v *= gain;
        }
        whole.push(&block);
        for (i, frame) in block.chunks_exact(2).enumerate() {
            let n = first + i as u64;
            let t = n as f64 / rate;
            while segment + 1 < cuts.len() && t >= cuts[segment].1 {
                segment += 1;
            }
            let frame = [frame[0], frame[1]];
            let (meter, tp, peak, clips) = &mut segments[segment.min(cuts.len().saturating_sub(1))];
            meter.push(&frame);
            tp.push(frame);
            whole_tp.push(frame);
            let magnitude = frame[0].abs().max(frame[1].abs());
            *peak = peak.max(magnitude);
            sample_peak = sample_peak.max(magnitude);
            if magnitude >= options.clip_level {
                clipped += 1;
                *clips += 1;
                clip_ranges.push((t, t + 1.0 / rate));
            }
            window_energy += (frame[0] as f64).powi(2).max((frame[1] as f64).powi(2));
            window_len += 1;
            if window_len == window {
                if window_energy / window as f64 <= silence_floor {
                    silent_windows.push((t + 1.0 / rate - window as f64 / rate, t + 1.0 / rate));
                }
                window_energy = 0.0;
                window_len = 0;
            }
        }
        first += count as u64;
    }
    let silence = merge(silent_windows, 1e-6).into_iter().filter(|(t0, t1)| t1 - t0 >= options.min_silence - 1e-9).collect();
    AudioReport {
        duration,
        target: mix.doc.output.loudness,
        gain_db: db(mix.gain),
        lufs: whole.finish(),
        true_peak_db: db(whole_tp.peak as f64),
        sample_peak_db: db(sample_peak as f64),
        clipped,
        clipping: merge(clip_ranges, 0.1),
        silence,
        segments: cuts
            .into_iter()
            .zip(segments)
            .map(|((start, end, sources), (meter, tp, peak, clips))| SegmentLevels {
                start,
                end,
                sources,
                lufs: meter.finish(),
                true_peak_db: db(tp.peak as f64),
                sample_peak_db: db(peak as f64),
                clipped: clips,
            })
            .collect(),
        silent_sources: Vec::new(),
    }
}

pub(crate) fn db_to_power(db: f64) -> f64 {
    10f64.powf(db / 10.0)
}

/// Decode what the cut mixes and measure it. Blocking (files, the store,
/// decoding): call it on a worker. Sources that cannot be decoded play
/// silent, as in a render, and are listed.
pub fn analyze(doc: &Document, resolver: &mut dyn MediaResolver, marks: &HashMap<RunRef, HashMap<String, f64>>, options: &AudioOptions) -> Result<AudioReport, String> {
    let (mix, silent) = mixdown(doc, resolver, marks)?;
    let mut report = analyze_mix(&mix, mix.doc.duration, options);
    report.silent_sources = silent;
    Ok(report)
}

/// The cut's mix, decoded as a render mixes it (loudness normalization
/// included), and the sources that play silent with why. Blocking.
pub fn mixdown(doc: &Document, resolver: &mut dyn MediaResolver, marks: &HashMap<RunRef, HashMap<String, f64>>) -> Result<(AudioMix, Vec<(String, String)>), String> {
    let edit = Arc::new(doc.load().map_err(|diags| diags.iter().map(|d| format!("{}:{d}", doc.name)).collect::<Vec<_>>().join("\n"))?);
    let tree = CutTree::from_doc((*edit).clone(), None, &mut |asset| resolver.cut_source(asset))
        .map_err(|diags| diags.iter().map(|d| format!("{}:{d}", doc.name)).collect::<Vec<_>>().join("\n"))?;
    let mut takes = HashMap::new();
    for (_, node) in tree_nodes(&tree) {
        if let CutNodeKind::VoiceOver { lines, .. } = &node.kind {
            for take in lines.iter().filter_map(|line| line.take.as_ref()) {
                if let Ok(Some(info)) = resolver.take_info(take) {
                    takes.insert(take.clone(), info);
                }
            }
        }
    }
    let mut eval = tree.eval();
    for (run, marks) in marks {
        eval.set_marks(run.clone(), marks.clone());
    }
    for (take, info) in &takes {
        eval.set_take(take.clone(), info.clone());
    }
    // The furthest source second each sound is read at.
    let mut reach: HashMap<MediaRef, f64> = HashMap::new();
    for segment in eval.audio_plan(0.0, edit.duration).segments {
        if let Some(start) = segment.source_t0 {
            let end = start + (segment.t1 - segment.t0) * segment.speed;
            let entry = reach.entry(segment.media).or_insert(0.0);
            *entry = entry.max(end);
        }
    }
    drop(eval);
    let mut sources = HashMap::new();
    let mut silent = Vec::new();
    let mut reach: Vec<_> = reach.into_iter().collect();
    reach.sort_by(|a, b| a.0.cmp(&b.0));
    for (media, seconds) in reach {
        let name = match &media {
            MediaRef::Asset(asset) => asset.id.clone(),
            MediaRef::Run(run) => run.render_id.clone(),
            MediaRef::MissingOptional(path) => path.clone(),
        };
        match resolver.resolve(&media).and_then(|resolved| decode(&resolved, seconds)) {
            Ok(pcm) => {
                sources.insert(media, Arc::new(pcm));
            }
            Err(error) => silent.push((name, error)),
        }
    }
    let mut mix = AudioMix::new(edit.clone(), sources);
    mix.tree = Some(Arc::new(tree));
    mix.marks = marks.clone();
    mix.takes = takes;
    if edit.output.loudness.is_some() {
        let fps = fps_ratio(edit.output.fps);
        mix.normalize(frame_sample(frame_count(edit.duration, fps), fps));
    }
    Ok((mix, silent))
}

/// The mix as mono (the channels' mean) at its gain, frames `first..first + count`.
fn mono(mix: &AudioMix, first: u64, count: usize) -> Vec<f32> {
    let gain = mix.gain as f32;
    mix.mix_f32(first, count).chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5 * gain).collect()
}

/// The mix's waveform: the lowest and highest mono sample of each of
/// `buckets` equal stretches of `[0, duration)`, and its picture.
#[derive(Clone, Debug, PartialEq)]
pub struct Waveform {
    /// Seconds per bucket.
    pub bucket: f64,
    pub peaks: Vec<(f32, f32)>,
    /// The waveform strip (the asset previews' recipe), `None` for silence.
    pub picture: Option<crate::image::Picture>,
}

/// The picture's size.
pub const WAVEFORM_PICTURE: (usize, usize) = (1600, 240);

pub fn waveform(mix: &AudioMix, duration: f64, buckets: usize) -> Waveform {
    let rate = SAMPLE_RATE as f64;
    let total = (duration.max(0.0) * rate).round() as u64;
    let samples = mono(mix, 0, total as usize);
    let buckets = buckets.max(1);
    let per = samples.len().div_ceil(buckets).max(1);
    let peaks = samples.chunks(per).map(|c| c.iter().fold((0.0f32, 0.0f32), |(lo, hi), v| (lo.min(*v), hi.max(*v)))).collect();
    let (w, h) = WAVEFORM_PICTURE;
    let picture = makepad_audio_picture::wave::wave_rgba(&samples, w, h, makepad_audio_picture::wave::WavePalette::default())
        .map(|rgba| crate::image::Picture { width: w as u32, height: h as u32, rgba });
    Waveform { bucket: per as f64 / rate, peaks, picture }
}

impl Waveform {
    pub fn to_json(&self) -> Value {
        obj(vec![
            ("bucket_seconds", Value::F64((self.bucket * 1e6).round() / 1e6)),
            ("peaks", Value::Arr(self.peaks.iter().map(|(lo, hi)| Value::Arr(vec![sample_value(*lo), sample_value(*hi)])).collect())),
        ])
    }

    /// One line: buckets and where the loudest one is.
    pub fn to_text(&self) -> String {
        let loudest = self.peaks.iter().enumerate().max_by(|a, b| a.1 .0.abs().max(a.1 .1).total_cmp(&b.1 .0.abs().max(b.1 .1)));
        match loudest {
            Some((i, (lo, hi))) => format!("waveform: {} buckets of {:.3} s, loudest at {:.2} s ({} dBFS)", self.peaks.len(), self.bucket, i as f64 * self.bucket, db_text(db(lo.abs().max(*hi) as f64))),
            None => "waveform: no sound".into(),
        }
    }
}

fn sample_value(v: f32) -> Value {
    Value::F64((v as f64 * 10_000.0).round() / 10_000.0)
}

/// One source sounding at a moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Sounding {
    pub node: String,
    /// A VoiceOver's line.
    pub line: Option<usize>,
    pub media: String,
    /// The source second playing.
    pub source_t: Option<f64>,
    /// Its gain at the moment (keys, fades, duck), dB.
    pub gain_db: f64,
    pub pan: f64,
    /// How far its `duck` lowers it now, dB (0: not ducked).
    pub duck_db: f64,
}

/// What sounds at `t`: the sources and the mix's level, spectrum and
/// waveform there.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioAt {
    pub t: f64,
    pub sources: Vec<Sounding>,
    /// The mix's RMS over 50 ms centred on `t`, and its sample peak, dBFS.
    pub rms_db: f64,
    pub peak_db: f64,
    /// [`makepad_motion::AudioSignal::spectrum`] of the mix at `t`.
    pub spectrum: Vec<f64>,
    /// [`makepad_motion::AudioSignal::wave`] of the mix at `t`.
    pub wave: Vec<f64>,
    pub wave_window: f64,
}

/// What sounds at `t` in a decoded mix: `bins` spectrum bands, `slots`
/// waveform samples across `window` seconds.
pub fn at_mix(mix: &AudioMix, t: f64, bins: usize, slots: usize, window: f64) -> AudioAt {
    let eval = mix_eval(mix);
    let mut sources = Vec::new();
    for segment in eval.audio_plan(t, t + 1e-6).segments.into_iter().filter(|s| s.t0 <= t && t < s.t1) {
        let Some(node) = eval.node(&segment.node) else { continue };
        let media = match &segment.media {
            MediaRef::Asset(asset) => asset.id.clone(),
            MediaRef::Run(run) => run.render_id.clone(),
            MediaRef::MissingOptional(path) => path.clone(),
        };
        sources.push(Sounding {
            node: segment.node.clone(),
            line: segment.line,
            media,
            source_t: segment.source_t0.map(|s0| s0 + (t - segment.t0) * segment.speed),
            gain_db: db(eval.gain(node, t)),
            pan: eval.pan(node, t),
            duck_db: eval.duck_db(node, t),
        });
    }
    drop(eval);
    // The sound around t: enough for the spectrum's window and the wave's.
    let rate = SAMPLE_RATE as f64;
    let half = (window * 0.5).max(makepad_motion::features::SPECTRUM_WINDOW).max(0.025) + 0.01;
    let first = ((t - half).max(0.0) * rate).floor() as u64;
    let last = ((t + half) * rate).ceil() as u64;
    let samples = mono(mix, first, (last - first) as usize);
    let signal = makepad_motion::AudioSignal::new(first as f64 / rate, rate, samples.into());
    let (a, b) = ((((t - 0.025) * rate).round() as i64 - first as i64).max(0) as usize, (((t + 0.025) * rate).round() as i64 - first as i64).max(0) as usize);
    let around = &signal.samples[a.min(signal.samples.len())..b.min(signal.samples.len())];
    let rms = (around.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / around.len().max(1) as f64).sqrt();
    let peak = around.iter().fold(0.0f32, |m, v| m.max(v.abs())) as f64;
    AudioAt { t, sources, rms_db: db(rms), peak_db: db(peak), spectrum: signal.spectrum(t, bins), wave: signal.wave(t, slots, window), wave_window: window }
}

/// [`at_mix`] of the cut's decoded mix. Blocking.
pub fn at(doc: &Document, resolver: &mut dyn MediaResolver, marks: &HashMap<RunRef, HashMap<String, f64>>, t: f64, bins: usize, slots: usize, window: f64) -> Result<(AudioAt, Vec<(String, String)>), String> {
    let (mix, silent) = mixdown(doc, resolver, marks)?;
    Ok((at_mix(&mix, t, bins, slots, window), silent))
}

/// A band's centre in Hz, of `bins` from 30 Hz to 16 kHz.
fn band_hz(i: usize, bins: usize) -> f64 {
    use makepad_motion::features::{SPECTRUM_HIGH_HZ, SPECTRUM_LOW_HZ};
    SPECTRUM_LOW_HZ * (SPECTRUM_HIGH_HZ / SPECTRUM_LOW_HZ).powf((i as f64 + 0.5) / bins.max(1) as f64)
}

impl AudioAt {
    /// The loudest band: its index and centre frequency.
    pub fn loudest_band(&self) -> Option<(usize, f64)> {
        let (i, level) = self.spectrum.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
        (*level > 0.0).then(|| (i, band_hz(i, self.spectrum.len())))
    }

    pub fn to_json(&self) -> Value {
        let round = |v: f64| Value::F64((v * 10_000.0).round() / 10_000.0);
        obj(vec![
            ("t", num(self.t)),
            ("sources", Value::Arr(self.sources.iter().map(|s| obj(vec![
                ("node", Value::Str(s.node.clone())),
                ("line", s.line.map_or(Value::Null, |l| Value::Int(l as i64))),
                ("media", Value::Str(s.media.clone())),
                ("source_t", s.source_t.map_or(Value::Null, num)),
                ("gain_db", db_value(s.gain_db)),
                ("pan", num(s.pan)),
                ("duck_db", db_value(s.duck_db)),
            ])).collect())),
            ("rms_dbfs", db_value(self.rms_db)),
            ("peak_dbfs", db_value(self.peak_db)),
            ("spectrum", obj(vec![
                ("low_hz", num(makepad_motion::features::SPECTRUM_LOW_HZ)),
                ("high_hz", num(makepad_motion::features::SPECTRUM_HIGH_HZ)),
                ("floor_db", num(makepad_motion::features::SPECTRUM_FLOOR_DB)),
                ("loudest_hz", self.loudest_band().map_or(Value::Null, |(_, hz)| num(hz.round()))),
                ("levels", Value::Arr(self.spectrum.iter().map(|v| round(*v)).collect())),
            ])),
            ("wave", obj(vec![("window", num(self.wave_window)), ("samples", Value::Arr(self.wave.iter().map(|v| round(*v)).collect()))])),
        ])
    }

    pub fn to_text(&self) -> String {
        const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let mut out = format!("at {:.3} s: mix {} dBFS RMS, peak {} dBFS", self.t, db_text(self.rms_db), db_text(self.peak_db));
        if let Some((_, hz)) = self.loudest_band() {
            out.push_str(&format!(", loudest band {:.0} Hz", hz));
        }
        out.push('\n');
        for s in &self.sources {
            out.push_str(&format!("  {}{}: gain {} dB, pan {:.2}{}{}\n",
                s.node,
                s.line.map_or(String::new(), |l| format!(" line {l}")),
                db_text(s.gain_db),
                s.pan,
                if s.duck_db < -0.05 { format!(", ducked {} dB", db_text(s.duck_db)) } else { String::new() },
                s.source_t.map_or(String::new(), |st| format!(", source {st:.3} s"))));
        }
        if self.sources.is_empty() {
            out.push_str("  nothing sounds\n");
        }
        let bars: String = self.spectrum.iter().map(|v| BARS[((v.clamp(0.0, 1.0) * 7.0).round()) as usize]).collect();
        out.push_str(&format!("spectrum ({} bands, 30 Hz–16 kHz): {bars}\n", self.spectrum.len()));
        out
    }
}

/// A source's sound up to `seconds` (containers stop decoding there).
fn decode(resolved: &ResolvedMedia, seconds: f64) -> Result<TrackPcm, String> {
    if resolved.kind == MediaKind::Image {
        return Err("an image has no sound".into());
    }
    if resolved.media != MediaType::Mp4 {
        return decode_audio_clip(&resolved.path, resolved.media, usize::MAX);
    }
    let (_input, mut decoder) = open_container_audio(&resolved.path)?;
    let mut frames: Vec<[i16; 2]> = Vec::new();
    let mut sample_rate = decoder.info().audio_sample_rate.max(1);
    while let Some(chunk) = decoder.next_audio().map_err(|e| e.to_string())? {
        sample_rate = chunk.sample_rate.max(1);
        let channels = chunk.channels.max(1) as usize;
        for frame in chunk.samples.chunks_exact(channels) {
            frames.push(if channels == 1 { [frame[0], frame[0]] } else { [frame[0], frame[1]] });
        }
        if frames.len() as f64 > (seconds + 0.1) * sample_rate as f64 {
            break;
        }
    }
    if frames.is_empty() {
        return Err("no sound".into());
    }
    Ok(TrackPcm { frames, sample_rate })
}

fn db_value(value: f64) -> Value {
    if value.is_finite() {
        Value::F64((value * 100.0).round() / 100.0)
    } else {
        Value::Null
    }
}

fn db_text(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.1}")
    } else {
        "-inf".into()
    }
}

impl AudioReport {
    /// Clipping anywhere, a true peak over -1 dBTP, or a stretch that should
    /// sound but is silent: what a publish should not go out with.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.clipped > 0 {
            out.push(format!("{} samples clip (at {})", self.clipped, ranges_text(&self.clipping)));
        }
        if self.true_peak_db > -1.0 {
            out.push(format!("the true peak is {} dBTP, over -1 dBTP", db_text(self.true_peak_db)));
        }
        for segment in &self.segments {
            if !segment.sources.is_empty() && !segment.sample_peak_db.is_finite() {
                out.push(format!("{} should sound from {:.2} s to {:.2} s but the mix is silent", segment.sources.join(", "), segment.start, segment.end));
            }
        }
        for (source, why) in &self.silent_sources {
            out.push(format!("{source} plays silent: {why}"));
        }
        out
    }

    pub fn to_json(&self) -> Value {
        let ranges = |ranges: &[(f64, f64)]| Value::Arr(ranges.iter().map(|(a, b)| Value::Arr(vec![num(*a), num(*b)])).collect());
        obj(vec![
            ("duration", num(self.duration)),
            ("target_lufs", self.target.map(num).unwrap_or(Value::Null)),
            ("gain_db", db_value(self.gain_db)),
            ("lufs", self.lufs.map(db_value).unwrap_or(Value::Null)),
            ("true_peak_dbtp", db_value(self.true_peak_db)),
            ("sample_peak_dbfs", db_value(self.sample_peak_db)),
            ("clipped_samples", Value::Int(self.clipped as i64)),
            ("clipping", ranges(&self.clipping)),
            ("silence", ranges(&self.silence)),
            (
                "segments",
                Value::Arr(
                    self.segments
                        .iter()
                        .map(|seg| {
                            obj(vec![
                                ("start", num(seg.start)),
                                ("end", num(seg.end)),
                                ("sources", Value::Arr(seg.sources.iter().cloned().map(Value::Str).collect())),
                                ("lufs", seg.lufs.map(db_value).unwrap_or(Value::Null)),
                                ("true_peak_dbtp", db_value(seg.true_peak_db)),
                                ("sample_peak_dbfs", db_value(seg.sample_peak_db)),
                                ("clipped_samples", Value::Int(seg.clipped as i64)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("silent_sources", Value::Arr(self.silent_sources.iter().map(|(id, why)| obj(vec![("source", s(id.clone())), ("why", s(why.clone()))])).collect())),
            ("problems", Value::Arr(self.problems().into_iter().map(Value::Str).collect())),
        ])
    }

    pub fn to_text(&self) -> String {
        let mut out = format!(
            "mix: {} LUFS, true peak {} dBTP, sample peak {} dBFS, {} clipped samples{}\n",
            self.lufs.map(db_text).unwrap_or_else(|| "silent".into()),
            db_text(self.true_peak_db),
            db_text(self.sample_peak_db),
            self.clipped,
            match self.target {
                Some(target) => format!(", normalized to {target} LUFS by {} dB", db_text(self.gain_db)),
                None => String::new(),
            }
        );
        out.push_str("segments:\n");
        for seg in &self.segments {
            out.push_str(&format!(
                "  {:6.2}–{:6.2}  {:>6} LUFS  tp {:>6}  {}{}\n",
                seg.start,
                seg.end,
                seg.lufs.map(db_text).unwrap_or_else(|| "-".into()),
                db_text(seg.true_peak_db),
                if seg.sources.is_empty() { "(nothing)".into() } else { seg.sources.join(", ") },
                if seg.clipped > 0 { format!("  CLIPS {}", seg.clipped) } else { String::new() }
            ));
        }
        if !self.silence.is_empty() {
            out.push_str(&format!("silence: {}\n", ranges_text(&self.silence)));
        }
        for problem in self.problems() {
            out.push_str(&format!("problem: {problem}\n"));
        }
        out
    }
}

fn ranges_text(ranges: &[(f64, f64)]) -> String {
    ranges.iter().map(|(a, b)| format!("{a:.2}–{b:.2} s")).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect::tests::{asset_id, asset_ref, edit};

    fn tone(freq: f64, seconds: f64, amplitude: f64) -> Arc<TrackPcm> {
        let frames = (seconds * 48_000.0) as usize;
        let v = |i: usize| ((i as f64 / 48_000.0 * freq * std::f64::consts::TAU).sin() * amplitude * 32767.0) as i16;
        Arc::new(TrackPcm { frames: (0..frames).map(|i| [v(i), v(i)]).collect(), sample_rate: 48_000 })
    }

    fn mix_of(nodes: &str, list: &str, duration: f64, pcm: Vec<(u8, Arc<TrackPcm>)>) -> AudioMix {
        let doc = Arc::new(edit(nodes, list, "{width: 64 height: 64 fps: 30}", duration).load().unwrap());
        let sources = pcm
            .into_iter()
            .map(|(seed, pcm)| (MediaRef::Asset(AssetRef { id: asset_id(seed), rev: format!("arev_{}", format!("{seed:02x}").repeat(32)) }), pcm))
            .collect();
        AudioMix::new(doc, sources)
    }

    #[test]
    fn segments_loudness_peaks_clipping_and_silence() {
        // A 1 kHz tone at -20 dBFS from 0 to 2 s, a full-scale one from 3 to 4 s.
        let nodes = format!(
            "let quiet = AudioClip{{src: {q} span: [0.0, 2.0]}}\nlet loud = AudioClip{{src: {l} span: [3.0, 1.0] gain: 1.5}}\nlet t = Title{{text: \"a\" span: [0.0, 4.0]}}\n\
             let out = CutOutput{{frame: t.out(@frame) audio: [conn(1, quiet.out(@audio)), conn(2, loud.out(@audio))]}}",
            q = asset_ref(0x51),
            l = asset_ref(0x52)
        );
        let mix = mix_of(&nodes, "quiet, loud, t, out", 4.0, vec![(0x51, tone(1000.0, 2.5, 0.1)), (0x52, tone(1000.0, 1.5, 1.0))]);
        let report = analyze_mix(&mix, 4.0, &AudioOptions::default());
        let spans: Vec<(f64, f64)> = report.segments.iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(spans, vec![(0.0, 2.0), (2.0, 3.0), (3.0, 4.0)]);
        assert_eq!(report.segments[0].sources, vec!["quiet"]);
        assert!(report.segments[1].sources.is_empty());
        // A stereo 1 kHz sine at -20 dBFS peak measures about -20.3 LUFS
        // (K-weighting adds ~+0.7 dB at 1 kHz, the sine's RMS is -3 dB, two
        // channels add +3 dB).
        let quiet = report.segments[0].lufs.unwrap();
        assert!((quiet - -20.3).abs() < 0.6, "{quiet}");
        assert!((report.segments[0].true_peak_db - -20.0).abs() < 0.2, "{}", report.segments[0].true_peak_db);
        assert!(report.segments[1].lufs.is_none());
        assert!(report.segments[2].clipped > 0);
        assert_eq!(report.silence.len(), 1);
        let (s0, s1) = report.silence[0];
        assert!((s0 - 2.0).abs() < 0.06 && (s1 - 3.0).abs() < 0.06, "{:?}", report.silence);
        assert!(!report.clipping.is_empty() && report.clipping[0].0 >= 3.0);
        assert!(report.problems().iter().any(|p| p.contains("clip")), "{:?}", report.problems());
        let json = report.to_json().to_json();
        assert!(json.contains("\"segments\""), "{json}");
        assert!(report.to_text().contains("CLIPS"));
    }

    /// An exponential sine sweep 100 Hz → 8 kHz over `seconds` at 0.5.
    fn sweep(seconds: f64) -> (Arc<TrackPcm>, impl Fn(f64) -> f64) {
        let k = (8000.0f64 / 100.0).ln() / seconds;
        let frames = (0..(seconds * 48_000.0) as usize).map(|i| {
            let t = i as f64 / 48_000.0;
            let v = (0.5 * (std::f64::consts::TAU * 100.0 * ((k * t).exp() - 1.0) / k).sin() * 32767.0) as i16;
            [v, v]
        }).collect();
        (Arc::new(TrackPcm { frames, sample_rate: 48_000 }), move |t: f64| 100.0 * (k * t).exp())
    }

    /// `audio at`: the sweep's tone in the spectrum at each moment, the
    /// mix's level, the sources with their gain, and the duck under a voice.
    #[test]
    fn at_a_moment_the_mix_shows_its_sources_level_spectrum_and_wave() {
        let (song, playing) = sweep(4.0);
        let nodes = format!(
            "let song = AudioClip{{src: {s} span: [0.0, 4.0] duck: {{under: [@vo] depth_db: -12}}}}\nlet vo = AudioClip{{src: {v} span: [2.0, 1.0]}}\nlet t = Title{{text: \"a\" span: [0.0, 4.0]}}\n\
             let out = CutOutput{{frame: t.out(@frame) audio: [conn(1, song.out(@audio)), conn(2, vo.out(@audio))]}}",
            s = asset_ref(0x71),
            v = asset_ref(0x72)
        );
        let mix = mix_of(&nodes, "song, vo, t, out", 4.0, vec![(0x71, song), (0x72, tone(440.0, 1.0, 0.001))]);
        for t in [0.5, 1.0, 1.5, 3.5] {
            let now = at_mix(&mix, t, 32, 64, 0.05);
            let (_, hz) = now.loudest_band().unwrap();
            let octaves = (hz / playing(t)).log2().abs();
            assert!(octaves < 0.3, "at {t}: {hz:.0} Hz for {:.0} Hz", playing(t));
            assert_eq!((now.spectrum.len(), now.wave.len()), (32, 64));
        }
        let before = at_mix(&mix, 1.0, 32, 64, 0.05);
        assert_eq!(before.sources.len(), 1);
        assert_eq!(before.sources[0].node, "song");
        assert!((before.sources[0].source_t.unwrap() - 1.0).abs() < 1e-6);
        assert!((before.rms_db - -9.03).abs() < 0.2, "{}", before.rms_db);
        assert!(before.sources[0].duck_db.abs() < 1e-9);
        // Under the voice the music is ducked 12 dB, and the mix is quieter.
        let ducked = at_mix(&mix, 2.6, 32, 64, 0.05);
        let music = ducked.sources.iter().find(|s| s.node == "song").unwrap();
        assert!((music.duck_db - -12.0).abs() < 0.5, "{}", music.duck_db);
        assert!((music.gain_db - -12.0).abs() < 0.5, "{}", music.gain_db);
        assert!(ducked.sources.iter().any(|s| s.node == "vo"));
        assert!(ducked.rms_db < before.rms_db - 10.0, "{} {}", ducked.rms_db, before.rms_db);
        let json = before.to_json().to_json();
        assert!(json.contains("\"loudest_hz\"") && json.contains("\"duck_db\""), "{json}");
        assert!(before.to_text().contains("song: gain 0.0 dB"), "{}", before.to_text());
        // Past the sound: nothing sounds.
        let after = at_mix(&mix, 3.99, 8, 8, 0.05);
        assert!(after.sources.iter().all(|s| s.node == "song"));
    }

    #[test]
    fn the_waveform_has_each_stretch_s_lowest_and_highest_sample() {
        let nodes = format!(
            "let a = AudioClip{{src: {a} span: [1.0, 1.0]}}\nlet t = Title{{text: \"a\" span: [0.0, 3.0]}}\nlet out = CutOutput{{frame: t.out(@frame) audio: [conn(1, a.out(@audio))]}}",
            a = asset_ref(0x73)
        );
        let mix = mix_of(&nodes, "a, t, out", 3.0, vec![(0x73, tone(440.0, 1.0, 0.25))]);
        let wave = waveform(&mix, 3.0, 3);
        assert_eq!(wave.peaks.len(), 3);
        assert!((wave.bucket - 1.0).abs() < 1e-9);
        assert_eq!(wave.peaks[0], (0.0, 0.0));
        assert!((wave.peaks[1].1 - 0.25).abs() < 0.01 && (wave.peaks[1].0 + 0.25).abs() < 0.01, "{:?}", wave.peaks[1]);
        assert_eq!(wave.peaks[2], (0.0, 0.0));
        let picture = wave.picture.as_ref().unwrap();
        assert_eq!((picture.width as usize, picture.height as usize), WAVEFORM_PICTURE);
        assert!(wave.to_text().contains("3 buckets of 1.000 s"), "{}", wave.to_text());
    }

    #[test]
    fn the_true_peak_sees_between_samples() {
        // A sine at fs/4 sampled at its 45° points: every sample is 0.707 of
        // the peak, the true peak is 1.
        let pcm = Arc::new(TrackPcm {
            frames: (0..48_000).map(|i| {
                let v = ((i as f64 * std::f64::consts::FRAC_PI_2 + std::f64::consts::FRAC_PI_4).sin() * 0.5 * 32767.0) as i16;
                [v, v]
            }).collect(),
            sample_rate: 48_000,
        });
        let nodes = format!(
            "let a = AudioClip{{src: {a} span: [0.0, 1.0]}}\nlet t = Title{{text: \"a\" span: [0.0, 1.0]}}\nlet out = CutOutput{{frame: t.out(@frame) audio: [conn(1, a.out(@audio))]}}",
            a = asset_ref(0x53)
        );
        let mix = mix_of(&nodes, "a, t, out", 1.0, vec![(0x53, pcm)]);
        let report = analyze_mix(&mix, 1.0, &AudioOptions::default());
        assert!((report.sample_peak_db - db(0.5 * 0.7071)).abs() < 0.1, "{}", report.sample_peak_db);
        assert!((report.true_peak_db - db(0.5)).abs() < 0.3, "{}", report.true_peak_db);
    }

    #[test]
    fn analyze_decodes_through_the_resolver() {
        let dir = std::env::temp_dir().join(format!("edit_tools_audio_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        write_wav(&wav, &tone(440.0, 1.0, 0.25));
        let nodes = format!(
            "let a = AudioClip{{src: {a} span: [0.0, 1.0]}}\nlet b = AudioClip{{src: {b} span: [0.5, 0.5]}}\nlet t = Title{{text: \"a\" span: [0.0, 1.0]}}\n\
             let out = CutOutput{{frame: t.out(@frame) audio: [conn(1, a.out(@audio)), conn(2, b.out(@audio))]}}",
            a = asset_ref(0x61),
            b = asset_ref(0x62)
        );
        let doc = edit(&nodes, "a, b, t, out", "{width: 64 height: 64 fps: 30}", 1.0);
        let mut media = makepad_story_edit_render::LocalMedia::new();
        media.insert(asset_id(0x61), &wav);
        let report = analyze(&doc, &mut media, &HashMap::new(), &AudioOptions::default()).unwrap();
        assert!(report.lufs.is_some());
        assert_eq!(report.silent_sources.len(), 1, "{:?}", report.silent_sources);
        assert_eq!(report.silent_sources[0].0, asset_id(0x62));
        let _ = std::fs::remove_dir_all(&dir);
    }

    pub(crate) fn write_wav(path: &std::path::Path, pcm: &TrackPcm) {
        let data: Vec<u8> = pcm.frames.iter().flat_map(|[l, r]| l.to_le_bytes().into_iter().chain(r.to_le_bytes())).collect();
        let mut out = Vec::new();
        out.extend(b"RIFF");
        out.extend((36 + data.len() as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend(pcm.sample_rate.to_le_bytes());
        out.extend((pcm.sample_rate * 4).to_le_bytes());
        out.extend(4u16.to_le_bytes());
        out.extend(16u16.to_le_bytes());
        out.extend(b"data");
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
        std::fs::write(path, out).unwrap();
    }
}

//! The film's sound as its cut's whole mix: every sound of the cut (the
//! song, voice lines, effects, with their keys, fades and ducking) mixed by
//! Stage's own mix (`makepad_story_cut::mix`, the export's
//! arithmetic) into one 48 kHz clip, which the scratchable deck then plays.
//!
//! The mix runs once, while the film loads, one slice per display frame so
//! it never takes a frame's budget ([`CutMixJob::step`]): first the
//! loudness measure the cut's output asks for, then the mix itself.

use makepad_stage_audio_media::pcm::TrackPcm;
use makepad_story_cut::mix::{AudioMix, Normalizer, SAMPLE_RATE};

/// Sound measured or mixed per slice (frames at the mix's rate): a tenth
/// of a second, about a millisecond of work natively.
const SLICE: u64 = SAMPLE_RATE as u64 / 10;
/// Wall time the slices of one display frame may take while the film loads
/// (the loader's frame is otherwise idle).
const FRAME_BUDGET: f64 = 0.004;

pub struct CutMixJob {
    mix: AudioMix,
    total: u64,
    measuring: Option<Normalizer>,
    next: u64,
    frames: Vec<[i16; 2]>,
}

impl CutMixJob {
    /// The mix of a cut `duration` seconds long; `gain`: the loudness
    /// gain the export measured (the measure is skipped), else measured here.
    pub fn new(mut mix: AudioMix, duration: f64, gain: Option<f64>) -> Self {
        let total = (duration.max(0.0) * SAMPLE_RATE as f64).ceil() as u64;
        let measuring = match gain {
            Some(gain) => {
                mix.gain = gain;
                None
            }
            None => Some(Normalizer::new(total)),
        };
        Self { mix, total, measuring, next: 0, frames: Vec::with_capacity(total as usize) }
    }

    /// One slice of the work; the clip once whole.
    pub fn step(&mut self) -> Option<TrackPcm> {
        if let Some(normalizer) = &mut self.measuring {
            if !normalizer.step(&self.mix, SLICE as usize) {
                let normalizer = self.measuring.take().expect("measuring");
                normalizer.finish(&mut self.mix);
            }
            return None;
        }
        if self.next < self.total {
            let count = SLICE.min(self.total - self.next) as usize;
            let pcm = self.mix.mix(self.next, count);
            self.frames.extend(pcm.chunks_exact(2).map(|f| [f[0], f[1]]));
            self.next += count as u64;
        }
        (self.next >= self.total).then(|| TrackPcm { frames: std::mem::take(&mut self.frames), sample_rate: SAMPLE_RATE })
    }
}

use crate::bundle::{Bytes, Fetcher, FileEntry};
use crate::job::Job;
use makepad_strict_json::Value;
use makepad_story_cut::{AssetRef, CutDoc, CutTree, MediaRef, RunRef, TakeInfo, Word};
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::Arc;

/// The film's edit as its manifest names it (`"cut"`: the document, the
/// edits it nests, its sound files, its takes' timings and its runs' marks).
#[derive(Clone, Debug)]
pub struct CutManifest {
    pub doc: FileEntry,
    pub asset: Option<AssetRef>,
    pub nested: Vec<(AssetRef, FileEntry)>,
    pub sources: Vec<(MediaRef, FileEntry)>,
    pub takes: Vec<(AssetRef, TakeInfo)>,
    pub marks: Vec<(RunRef, HashMap<String, f64>)>,
    /// The mix's loudness gain as the export measured it (`"gain"`), so the
    /// player need not measure the whole mix again.
    pub gain: Option<f64>,
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Int(i) => Some(*i as f64),
        Value::F64(f) => Some(*f),
        _ => None,
    }
}

fn text(v: &Value, key: &str) -> Result<String, String> {
    v.get(key).and_then(|v| v.as_str()).map(str::to_string).ok_or_else(|| format!("manifest.json: edit: no `{key}`"))
}

fn entry(v: &Value) -> Result<FileEntry, String> {
    Ok(FileEntry { file: text(v, "file")?, bytes: num(v.get("bytes")).unwrap_or(0.0) as u64 })
}

fn asset(v: &Value) -> Result<AssetRef, String> {
    Ok(AssetRef { id: text(v, "asset")?, rev: text(v, "rev")? })
}

fn run(v: &Value) -> Result<RunRef, String> {
    Ok(RunRef { id: text(v, "id")?, rev: text(v, "rev")?, render_id: text(v, "render_id")?, render: text(v, "render")? })
}

fn words(v: Option<&Value>) -> Vec<Word> {
    v.and_then(|v| v.as_arr()).unwrap_or(&[]).iter().filter_map(|w| {
        Some(Word { text: w.get("text")?.as_str()?.to_string(), start: num(w.get("start"))?, end: num(w.get("end"))? })
    }).collect()
}

fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(|v| v.as_arr()).unwrap_or(&[])
}

impl CutManifest {
    pub fn parse(v: &Value) -> Result<CutManifest, String> {
        let mut sources = Vec::new();
        for s in list(v, "sources") {
            let m = s.get("media").ok_or("manifest.json: edit: a source without `media`")?;
            let media = match m.get("run") {
                Some(r) => MediaRef::Run(run(r)?),
                None => MediaRef::Asset(asset(m)?),
            };
            sources.push((media, entry(s)?));
        }
        let mut takes = Vec::new();
        for t in list(v, "takes") {
            takes.push((asset(t)?, TakeInfo { duration: num(t.get("duration")).unwrap_or(0.0), words: words(t.get("words")), phones: words(t.get("phones")) }));
        }
        let mut marks = Vec::new();
        for m in list(v, "marks") {
            let r = run(m.get("run").ok_or("manifest.json: edit: marks without `run`")?)?;
            let values = match m.get("marks") {
                Some(Value::Obj(pairs)) => pairs.iter().filter_map(|(k, v)| Some((k.clone(), num(Some(v))?))).collect(),
                _ => HashMap::new(),
            };
            marks.push((r, values));
        }
        let mut nested = Vec::new();
        for n in list(v, "nested") {
            nested.push((asset(n)?, entry(n)?));
        }
        Ok(CutManifest { doc: entry(v)?, asset: v.get("asset").map(asset).transpose()?, nested, sources, takes, marks, gain: num(v.get("gain")) })
    }

    /// Every file the cut's sound needs, with its size (for the loader).
    pub fn files(&self) -> impl Iterator<Item = (&str, u64)> {
        std::iter::once(&self.doc).chain(self.nested.iter().map(|(_, e)| e)).chain(self.sources.iter().map(|(_, e)| e)).map(|e| (e.file.as_str(), e.bytes))
    }
}

enum Stage {
    /// Fetching the documents and the sound files, decoding the sounds.
    Loading,
    Mixing(CutMixJob),
    Done,
}

/// The cut's sound, from its files to the mixed clip.
pub struct CutSound {
    manifest: CutManifest,
    duration: f64,
    fetches: HashMap<LiveId, usize>,
    /// Index 0: the document; 1..: nested cuts; then the sources.
    texts: Vec<Option<String>>,
    decodes: Vec<(usize, Job<Result<TrackPcm, String>>)>,
    pcm: Vec<Option<Arc<TrackPcm>>>,
    stage: Stage,
    /// The display frame the last slice ran in: one slice per frame.
    slice_frame: u64,
}

impl CutSound {
    pub fn start(cx: &mut Cx, fetcher: &mut Fetcher, manifest: CutManifest, duration: f64) -> CutSound {
        let mut fetches = HashMap::new();
        let docs = 1 + manifest.nested.len();
        for (i, (file, _)) in manifest.files().enumerate() {
            // Documents ride in the pack; sound files sit beside the page.
            let id = if i < docs { fetcher.fetch(cx, file) } else { fetcher.fetch_app(cx, file) };
            fetches.insert(id, i);
        }
        let sources = manifest.sources.len();
        CutSound { manifest, duration, fetches, texts: vec![None; docs], decodes: Vec::new(), pcm: vec![None; sources], stage: Stage::Loading, slice_frame: u64::MAX }
    }

    /// A fetch this load asked for.
    pub fn owns(&self, id: LiveId) -> bool {
        self.fetches.contains_key(&id)
    }

    pub fn fetched(&mut self, cx: &mut Cx, id: LiveId, result: Result<Bytes, String>) -> Result<(), String> {
        let Some(i) = self.fetches.remove(&id) else { return Ok(()) };
        let bytes = result?;
        let docs = self.texts.len();
        if i < docs {
            self.texts[i] = Some(String::from_utf8_lossy(&bytes).into_owned());
        } else {
            self.decodes.push((i - docs, crate::audio::decode(cx, bytes)));
        }
        Ok(())
    }

    /// Advance: decodes taken, the mix built once everything is here, then
    /// one slice of mixing per display frame. The clip once mixed.
    pub fn step(&mut self) -> Option<Result<TrackPcm, String>> {
        let mut failed = None;
        self.decodes.retain_mut(|(i, job)| match job.try_take() {
            Some(Ok(pcm)) => {
                self.pcm[*i] = Some(Arc::new(pcm));
                false
            }
            Some(Err(e)) => {
                failed = Some(e);
                false
            }
            None => true,
        });
        if let Some(e) = failed {
            return Some(Err(e));
        }
        if matches!(self.stage, Stage::Loading) && self.fetches.is_empty() && self.decodes.is_empty() {
            match self.build() {
                Ok(job) => self.stage = Stage::Mixing(job),
                Err(e) => return Some(Err(e)),
            }
        }
        let frame = crate::job::frame();
        if let Stage::Mixing(job) = &mut self.stage {
            if self.slice_frame == frame {
                return None;
            }
            self.slice_frame = frame;
            // Slices until this frame's share is spent.
            let start = Cx::monotonic_now();
            while Cx::monotonic_now() - start < FRAME_BUDGET {
                if let Some(pcm) = job.step() {
                    self.stage = Stage::Done;
                    return Some(Ok(pcm));
                }
            }
        }
        None
    }

    fn build(&mut self) -> Result<CutMixJob, String> {
        let source = self.texts[0].take().ok_or("the cut: no document")?;
        let nested: HashMap<AssetRef, String> = self.manifest.nested.iter().zip(self.texts[1..].iter_mut()).filter_map(|((asset, _), text)| Some((asset.clone(), text.take()?))).collect();
        let own = self.manifest.asset.as_ref().map(|a| a.id.clone());
        let tree = CutTree::load(&source, own.as_deref(), &mut |asset| nested.get(asset).cloned().ok_or_else(|| format!("nested cut {}:{} is not in the film", asset.id, asset.rev)))
            .map_err(|diags| format!("the cut: {}", diags.first().map_or(String::new(), |d| d.message.clone())))?;
        let doc: Arc<CutDoc> = Arc::new(tree.root.clone());
        let sources = self.manifest.sources.iter().zip(self.pcm.iter()).filter_map(|((media, _), pcm)| Some((media.clone(), pcm.clone()?))).collect();
        let mut mix = AudioMix::new(doc, sources);
        mix.tree = Some(Arc::new(tree));
        mix.marks = self.manifest.marks.iter().cloned().collect();
        mix.takes = self.manifest.takes.iter().cloned().collect();
        Ok(CutMixJob::new(mix, self.duration, self.manifest.gain))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu";
    const A_REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";

    fn doc(music: &str, output: &str) -> Arc<CutDoc> {
        let source = format!(
            "use mod.flow.*\nuse mod.cut.*\n{music}\nlet title = Title{{text: \"t\"}}\n\
             let out = CutOutput{{frame: title.out(@frame) audio: [conn(1, music.out(@audio))]}}\n\
             Flow{{\n kind: @cut\n output: {{width: 64 height: 36 fps: 30 {output}}}\n duration: 2.0\n music, title, out\n}}\n"
        );
        Arc::new(CutDoc::load(&source).unwrap_or_else(|errors| panic!("{errors:?}")))
    }

    fn song(rate: u32, seconds: f64) -> Arc<TrackPcm> {
        let v = |i: usize| ((i as f64 / rate as f64 * 523.25 * std::f64::consts::TAU).sin() * 0.6 * 32767.0) as i16;
        Arc::new(TrackPcm { frames: (0..(seconds * rate as f64) as usize).map(|i| [v(i), v(i) / 2]).collect(), sample_rate: rate })
    }

    fn mixer(doc: Arc<CutDoc>, pcm: Arc<TrackPcm>) -> AudioMix {
        AudioMix::new(doc, HashMap::from([(MediaRef::Asset(AssetRef { id: A.into(), rev: A_REV.into() }), pcm)]))
    }

    fn run(mut job: CutMixJob) -> TrackPcm {
        let mut slowest = 0.0f64;
        loop {
            let t0 = std::time::Instant::now();
            let done = job.step();
            slowest = slowest.max(t0.elapsed().as_secs_f64());
            if let Some(pcm) = done {
                eprintln!("slowest slice {:.2} ms", slowest * 1000.0);
                return pcm;
            }
        }
    }

    /// Mixed in slices with the measure first, the clip is the export's
    /// mix exactly.
    #[test]
    fn the_sliced_mix_is_the_export_mix() {
        let d = doc(&format!("let music = AudioClip{{src: asset(\"{A}\", \"{A_REV}\") in: 0.25 span: [0.1, 1.8] gain: keys([0.0, 0.3], [1.5, 1.0])}}"), "loudness: -16");
        let pcm = song(44_100, 3.0);
        let clip = run(CutMixJob::new(mixer(d.clone(), pcm.clone()), 2.0, None));
        let mut whole = mixer(d, pcm);
        whole.normalize(96_000);
        let expected = whole.mix(0, 96_000);
        let got: Vec<i16> = clip.frames.iter().flat_map(|f| [f[0], f[1]]).collect();
        assert_eq!(clip.sample_rate, SAMPLE_RATE);
        assert_eq!(got, expected);
    }

    /// One song, as the film's edit plays it: at the mix's rate the clip is
    /// the song to the last step; at 44.1 kHz it is the song resampled,
    /// within a hair of the true signal.
    #[test]
    fn a_song_only_edit_sounds_as_the_song() {
        let d = doc(&format!("let music = AudioClip{{src: asset(\"{A}\", \"{A_REV}\") span: [0.0, 2.0]}}"), "");
        let pcm = song(48_000, 2.0);
        let clip = run(CutMixJob::new(mixer(d.clone(), pcm.clone()), 2.0, None));
        // The export's full scale is 32767 for the decoder's 32768: within
        // one step of the song everywhere.
        let worst = clip.frames[..95_000].iter().zip(&pcm.frames[..95_000]).map(|(a, b)| (a[0] as i32 - b[0] as i32).abs().max((a[1] as i32 - b[1] as i32).abs())).max().unwrap();
        eprintln!("48 kHz song through the cut mix: at most {worst} step from the song");
        assert!(worst <= 1, "{worst}");
        let clip = run(CutMixJob::new(mixer(d, song(44_100, 2.0)), 2.0, None));
        let truth = song(48_000, 2.0);
        // Away from the ends (the band-limit kernel's reach).
        let (mut err, mut sig) = (0.0f64, 0.0f64);
        for i in 2_000..94_000 {
            for c in 0..2 {
                let (a, b) = (clip.frames[i][c] as f64, truth.frames[i][c] as f64);
                err += (a - b) * (a - b);
                sig += b * b;
            }
        }
        let snr = 10.0 * (sig / err.max(1e-9)).log10();
        eprintln!("44.1 kHz song through the cut mix: {snr:.1} dB SNR against the true signal");
        assert!(snr > 60.0, "{snr} dB");
    }
}

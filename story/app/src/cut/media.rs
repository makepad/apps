//! The CUT page's media worker: loudness outlines of the sounds on the
//! timeline, and voiceover takes generated and published.
//! Both are slow (a whole decode; a TTS model), so they run here rather
//! than on the asset worker that serves the library. One long-lived
//! thread; bounded channels both ways.

use makepad_asset_connection as asset_connection;
use makepad_library_browser::lanes::Thumb;
use makepad_library_browser::draw::Peaks;
use makepad_asset_client::AssetClient;
use makepad_story_edit_render::MediaResolver;
use makepad_story_edit_voice::publish::take_bundle;
use makepad_story_edit_voice::synth::{Synthesizer, VoicePaths};
use makepad_story_cut::{AssetRef, MediaRef, TakeInfo, VoiceSpec};
use makepad_widgets::makepad_platform::video_file::VideoFileDecoder;
use makepad_widgets::*;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

/// Loudness outline resolution.
const PEAKS_PER_SECOND: f64 = 50.0;
const NAMESPACE: &str = "stage";
/// Filmstrip pictures per video, and their height.
const FILM_FRAMES: usize = 16;
const FILM_HEIGHT: usize = 72;

pub enum MediaJob {
    /// The Kokoro voices installed here.
    Voices,
    Peaks { media: MediaRef },
    /// Pictures across a video's length, for its filmstrip and card skim.
    Film { media: MediaRef },
    /// A file the platform player can open (a named link to the store blob).
    Local { media: MediaRef },
    Voice { node: String, index: usize, voice: VoiceSpec, text: String },
}

pub enum MediaReply {
    Voices(Vec<String>),
    Peaks { asset: String, result: Result<Arc<Peaks>, String> },
    /// `(source seconds, picture)` in time order.
    Film { asset: String, result: Result<(Vec<(f64, Thumb)>, FilmInfo), String> },
    Local { asset: String, result: Result<std::path::PathBuf, String> },
    Voice { node: String, index: usize, text: String, result: Result<(AssetRef, String, TakeInfo), String> },
}

pub struct MediaWorker {
    jobs: Option<SyncSender<MediaJob>>,
    replies: Receiver<MediaReply>,
    pub error: Option<String>,
}

impl MediaWorker {
    pub fn new(cx: &mut Cx) -> Self {
        let (job_tx, job_rx) = sync_channel(16);
        let (reply_tx, reply_rx) = sync_channel(16);
        let options = ThreadOptions { name: Some(Arc::from("stage-edit-media")), stack_size: Some(16 * 1024 * 1024), priority: CxThreadPriority::Utility };
        let mut worker = Self { jobs: None, replies: reply_rx, error: None };
        match cx.thread_spawner().spawn_worker(options, move || run(job_rx, reply_tx)) {
            Ok(handle) => {
                handle.detach();
                worker.jobs = Some(job_tx);
            }
            Err(error) => worker.error = Some(format!("edit media worker unavailable: {error:?}")),
        }
        worker
    }

    pub fn try_send(&self, job: MediaJob) -> Result<(), MediaJob> {
        let Some(jobs) = &self.jobs else { return Err(job) };
        jobs.try_send(job).map_err(|error| match error {
            TrySendError::Full(job) | TrySendError::Disconnected(job) => job,
        })
    }

    pub fn try_recv(&self) -> Option<MediaReply> {
        self.replies.try_recv().ok()
    }

    pub fn stop(&mut self) {
        self.jobs.take();
    }
}


fn run(jobs: Receiver<MediaJob>, replies: SyncSender<MediaReply>) {
    let origin = asset_connection::origin();
    let mut client: Option<AssetClient> = None;
    // Kokoro's weights where the AI Hub keeps them: `MAKEPAD_ASSET_AI_CACHE`,
    // else the home folders' `tts/`, the model and the voice packs each
    // from the first folder that really has them.
    let paths = if std::env::var_os("MAKEPAD_ASSET_AI_CACHE").is_some() {
        VoicePaths::default()
    } else {
        let home = makepad_ai_hub::home::makepad_home();
        let dirs = ["weights", "ai_content"].map(|name| home.join(name).join("tts"));
        let voices = |dir: &std::path::Path| std::fs::read_dir(dir).ok().is_some_and(|mut entries| {
            entries.any(|entry| entry.ok().is_some_and(|entry| entry.path().extension().is_some_and(|ext| ext == "mkvoice") && entry.path().is_file()))
        });
        VoicePaths {
            kokoro_model: dirs.iter().map(|dir| dir.join("kokoro-v1_0.mktts")).find(|path| path.is_file()),
            kokoro_voices: dirs.iter().find(|dir| voices(dir)).cloned(),
            indextts: None,
        }
    };
    let installed = paths.kokoro_voices.as_ref().and_then(|dir| std::fs::read_dir(dir).ok()).map(|entries| {
        let mut voices = entries.filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.is_file() && path.extension()? == "mkvoice" { Some(path.file_stem()?.to_string_lossy().into_owned()) } else { None }
        }).collect::<Vec<_>>();
        voices.sort();
        voices
    }).unwrap_or_default();
    let mut synth = Synthesizer::new(paths);
    while let Ok(job) = jobs.recv() {
        let reply = match job {
            MediaJob::Voices => MediaReply::Voices(installed.clone()),
            MediaJob::Peaks { media } => {
                let asset = match &media {
                    MediaRef::Asset(asset) => asset.id.clone(),
                    MediaRef::Run(run) => run.render_id.clone(),
                    MediaRef::MissingOptional(path) => path.clone(),
                };
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| peaks(client, &media)).map(Arc::new);
                MediaReply::Peaks { asset, result }
            }
            MediaJob::Film { media } => {
                let asset = match &media {
                    MediaRef::Asset(asset) => asset.id.clone(),
                    MediaRef::Run(run) => run.render_id.clone(),
                    MediaRef::MissingOptional(path) => path.clone(),
                };
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| film(client, &media));
                MediaReply::Film { asset, result }
            }
            MediaJob::Local { media } => {
                let asset = match &media {
                    MediaRef::Asset(asset) => asset.id.clone(),
                    MediaRef::Run(run) => run.render_id.clone(),
                    MediaRef::MissingOptional(path) => path.clone(),
                };
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| {
                    let mut store = makepad_story_edit_render::StoreMedia::new(client.lane_clone());
                    let resolved = store.resolve(&media)?;
                    named_file(&resolved)
                });
                MediaReply::Local { asset, result }
            }
            MediaJob::Voice { node, index, voice, text } => {
                let result = synth.generate_line(&voice, &text).map_err(|error| error.to_string()).and_then(|take| {
                    let client = asset_connection::connected(&mut client, &origin)?;
                    // The store's labels are `[a-z0-9_-]`; cut_voice's
                    // `timing:…` tag is not (reported to its lane), so the
                    // tags are made label-safe here before publishing.
                    let mut bundle = take_bundle(NAMESPACE, &take, &text, &voice)?;
                    for tag in &mut bundle.tags {
                        *tag = tag.to_ascii_lowercase().chars().map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '-' }).collect();
                    }
                    let published = client.publish_bundle(&bundle).map_err(|error| format!("publish take: {error:?}"))?;
                    let asset = AssetRef { id: published.asset_id.to_string(), rev: published.revision.to_string() };
                    Ok((asset, take.key.clone(), TakeInfo { duration: take.duration(), words: take.words.clone(), phones: take.phones.clone() }))
                });
                MediaReply::Voice { node, index, text, result }
            }
        };
        if replies.send(reply).is_err() {
            break;
        }
    }
}

/// The whole source decoded to a loudness outline: per window its peak and
/// its RMS (the same pass).
fn peaks(client: &mut AssetClient, media: &MediaRef) -> Result<Peaks, String> {
    let mut store = makepad_story_edit_render::StoreMedia::new(client.lane_clone());
    let resolved = store.resolve(media)?;
    let mut outline = Outline::default();
    if resolved.media == makepad_asset_data::MediaType::Mp4 {
        let (_input, mut decoder) = makepad_stage_audio_media::decode::open_container_audio(&resolved.path)?;
        while let Some(chunk) = decoder.next_audio().map_err(|error| error.to_string())? {
            let per_bucket = ((chunk.sample_rate.max(1) as f64 / PEAKS_PER_SECOND) as usize).max(1);
            let channels = chunk.channels.max(1) as usize;
            for frame in chunk.samples.chunks_exact(channels) {
                outline.push(frame.iter().map(|sample| (*sample as f32 / 32768.0).abs()).fold(0.0, f32::max), per_bucket);
            }
        }
    } else {
        let pcm = makepad_stage_audio_media::decode::decode_audio_clip(&resolved.path, resolved.media, usize::MAX)?;
        let per_bucket = ((pcm.sample_rate.max(1) as f64 / PEAKS_PER_SECOND) as usize).max(1);
        for frame in &pcm.frames {
            outline.push((frame[0].unsigned_abs().max(frame[1].unsigned_abs()) as f32 / 32768.0).min(1.0), per_bucket);
        }
    }
    Ok(Peaks { per_second: PEAKS_PER_SECOND, values: outline.values, rms: outline.rms })
}

/// Windows of samples folded into a peak and an RMS each.
#[derive(Default)]
struct Outline {
    peak: f32,
    squares: f64,
    count: usize,
    values: Vec<u8>,
    rms: Vec<u8>,
}

impl Outline {
    fn push(&mut self, level: f32, per_bucket: usize) {
        self.peak = self.peak.max(level);
        self.squares += (level as f64) * (level as f64);
        self.count += 1;
        if self.count >= per_bucket {
            self.values.push((self.peak.min(1.0) * 255.0) as u8);
            self.rms.push(makepad_library_browser::draw::Peaks::code_level((self.squares / self.count as f64).sqrt()));
            self.peak = 0.0;
            self.squares = 0.0;
            self.count = 0;
        }
    }
}

/// `FILM_FRAMES` pictures spread over the video, each the first frame at or
/// after its time, scaled to `FILM_HEIGHT`.
fn film(client: &mut AssetClient, media: &MediaRef) -> Result<(Vec<(f64, Thumb)>, FilmInfo), String> {
    let mut store = makepad_story_edit_render::StoreMedia::new(client.lane_clone());
    let resolved = store.resolve(media)?;
    if resolved.media != makepad_asset_data::MediaType::Mp4 {
        return Err("not a video".into());
    }
    let path = named_file(&resolved)?;
    let path = path.to_string_lossy().into_owned();
    let mut decoder = VideoFileDecoder::open(&path).map_err(|error| format!("{error:?}"))?;
    let info = decoder.info();
    let seconds = info.duration_100ns as f64 / 1e7;
    let film_info = FilmInfo {
        width: info.width,
        height: info.height,
        fps: if info.fps_den > 0 { info.fps_num as f64 / info.fps_den as f64 } else { 0.0 },
        seconds,
        sound: info.has_audio.then_some((info.audio_channels, info.audio_sample_rate)),
    };
    let mut frames = Vec::new();
    for i in 0..FILM_FRAMES {
        let t = if seconds > 0.0 { seconds * i as f64 / FILM_FRAMES as f64 } else { 0.0 };
        if i > 0 {
            if seconds <= 0.0 || decoder.seek((t * 1e7) as i64).is_err() {
                break;
            }
        }
        let Some(frame) = decoder.next_frame().map_err(|error| format!("{error:?}"))? else { break };
        frames.push((t, scaled(&frame.to_rgb8(), frame.width as usize, frame.height as usize)));
    }
    if frames.is_empty() {
        return Err("no pictures".into());
    }
    Ok((frames, film_info))
}

/// What a video file is: picture size and rate, and its sound.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FilmInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub seconds: f64,
    /// Channels and rate of its sound, when it has one.
    pub sound: Option<(u16, u32)>,
}

/// The platform decoder and player go by a file's extension; store blobs
/// have none, so they read a named link to the blob.
fn named_file(resolved: &makepad_story_edit_render::ResolvedMedia) -> Result<std::path::PathBuf, String> {
    if resolved.path.extension().is_some() {
        return Ok(resolved.path.clone());
    }
    let ext = match resolved.media {
        makepad_asset_data::MediaType::Mp4 => "mp4",
        makepad_asset_data::MediaType::Wav => "wav",
        makepad_asset_data::MediaType::Mp3 => "mp3",
        makepad_asset_data::MediaType::Ogg => "ogg",
        makepad_asset_data::MediaType::Png => "png",
        makepad_asset_data::MediaType::Jpeg => "jpg",
        other => return Err(format!("no player for {other:?}")),
    };
    let dir = makepad_widgets::makepad_platform::home::app_state_home().join("stage-product").join("edit-cache");
    let revision = resolved.revision.revision.to_string();
    let named = dir.join(format!("{}-{}.{ext}", resolved.revision.asset_id, &revision[revision.len().saturating_sub(12)..]));
    if !named.exists() {
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&resolved.path, &named).map_err(|error| error.to_string())?;
        #[cfg(not(unix))]
        std::fs::copy(&resolved.path, &named).map_err(|error| error.to_string())?;
    }
    Ok(named)
}

/// An RGB picture box-sampled to `FILM_HEIGHT` rows, as BGRA pixels.
fn scaled(rgb: &[u8], width: usize, height: usize) -> Thumb {
    let (h, w) = (FILM_HEIGHT.min(height.max(1)), ((FILM_HEIGHT.min(height.max(1)) * width.max(1)) / height.max(1)).max(1));
    let mut pixels = vec![0xff00_0000u32; w * h];
    for y in 0..h {
        let (y0, y1) = (y * height / h, ((y + 1) * height / h).max(y * height / h + 1).min(height));
        for x in 0..w {
            let (x0, x1) = (x * width / w, ((x + 1) * width / w).max(x * width / w + 1).min(width));
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            // At most a 4×4 grid of samples per output pixel.
            let (sy, sx) = (((y1 - y0) / 4).max(1), ((x1 - x0) / 4).max(1));
            for yy in (y0..y1).step_by(sy) {
                for xx in (x0..x1).step_by(sx) {
                    let i = (yy * width + xx) * 3;
                    if i + 2 < rgb.len() {
                        r += rgb[i] as u32;
                        g += rgb[i + 1] as u32;
                        b += rgb[i + 2] as u32;
                        n += 1;
                    }
                }
            }
            let n = n.max(1);
            pixels[y * w + x] = 0xff00_0000 | ((r / n) << 16) | ((g / n) << 8) | (b / n);
        }
    }
    Thumb { width: w, height: h, pixels: Arc::new(pixels) }
}

#[cfg(test)]
mod tests {
    use super::{scaled, FILM_HEIGHT};

    #[test]
    fn film_pictures_keep_their_aspect() {
        let rgb = vec![200u8; 320 * 180 * 3];
        let thumb = scaled(&rgb, 320, 180);
        assert_eq!((thumb.width, thumb.height), (FILM_HEIGHT * 16 / 9, FILM_HEIGHT));
        assert_eq!(thumb.pixels[0], 0xffc8c8c8);
    }
}

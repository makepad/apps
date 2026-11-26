//! Takes and transcripts in the library.
//!
//! - A take is an `Audio` asset: the WAV plus its word times as the
//!   `Lyrics` side-channel (`vj-lyrics` v5). Its tags carry its
//!   [`take_key`](makepad_story_cut::take_key), so the library can
//!   say which text and voice it speaks. Its only dependency is an IndexTTS
//!   voice's reference recording; the cut pins the take, not the reverse.
//! - A transcript is a `Data` asset (the v5 document as its `Source` text)
//!   depending on the exact source revision it transcribes. It is the
//!   user's to cut: a cut publishes a new revision of the same asset
//!   (`asset_id`), while a re-transcription mints a new asset, so edits are
//!   never overwritten.
//!
//! The bundle builders are pure; publishing needs the native client.

use crate::lyrics::Transcript;
use crate::synth::Take;
use crate::wav::encode_wav_mono16;
use makepad_asset_client::{PublishBundle, PublishBundleFile, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetId, AssetKind, AssetRevisionId, AssetRevisionRef, DerivativePolicy, FileRole, MediaType, Redistribution, Sha256, ThumbnailMedia};
use makepad_story_cut::{AssetRef, VoiceEngine, VoiceSpec};

pub const THUMB_WIDTH: u32 = 512;
pub const THUMB_HEIGHT: u32 = 256;

/// Content digest of a take, as `makepad_audio_lyrics::track_digest` takes
/// it of decoded audio: rate, frame count, then every frame as stereo i16
/// (the mono take on both sides).
pub fn take_digest(pcm: &[f32], rate: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(&rate.to_le_bytes());
    hasher.update(&(pcm.len() as u64).to_le_bytes());
    let mut block = Vec::with_capacity(64 * 1024);
    for sample in pcm {
        let value = ((sample.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes();
        block.extend_from_slice(&value);
        block.extend_from_slice(&value);
        if block.len() >= 64 * 1024 {
            hasher.update(&block);
            block.clear();
        }
    }
    hasher.update(&block);
    hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A waveform picture of mono `pcm`: the peak per column, mirrored about
/// the middle, light on dark.
pub fn waveform_thumbnail(pcm: &[f32]) -> Result<PublishThumbnail, String> {
    let (w, h) = (THUMB_WIDTH as usize, THUMB_HEIGHT as usize);
    let mut rgb = vec![0u8; w * h * 3];
    for pixel in rgb.chunks_exact_mut(3) {
        pixel.copy_from_slice(&[0x1c, 0x1f, 0x26]);
    }
    let per_column = pcm.len().div_ceil(w).max(1);
    let peak = pcm.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    for x in 0..w {
        let column = pcm.get(x * per_column..((x + 1) * per_column).min(pcm.len())).unwrap_or(&[]);
        let level = column.iter().fold(0.0f32, |m, v| m.max(v.abs())) / peak;
        let half = ((level * (h as f32 * 0.45)) as usize).max(1);
        for y in h / 2 - half..(h / 2 + half).min(h) {
            rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[0x7c, 0xc4, 0xff]);
        }
    }
    let mut bytes = Vec::new();
    jpeg_encoder::Encoder::new(&mut bytes, 85)
        .encode(&rgb, THUMB_WIDTH as u16, THUMB_HEIGHT as u16, jpeg_encoder::ColorType::Rgb)
        .map_err(|error| format!("thumbnail: {error}"))?;
    Ok(PublishThumbnail::plain(bytes, ThumbnailMedia::Jpeg, THUMB_WIDTH, THUMB_HEIGHT))
}

/// A picture of text without a font: the first words as bars as long as
/// the words, set in lines like a page.
pub fn text_thumbnail(text: &str) -> Result<PublishThumbnail, String> {
    let (w, h) = (THUMB_WIDTH as usize, THUMB_HEIGHT as usize);
    let mut rgb = vec![0u8; w * h * 3];
    for pixel in rgb.chunks_exact_mut(3) {
        pixel.copy_from_slice(&[0x1c, 0x1f, 0x26]);
    }
    let (margin, char_w, line_h, bar_h) = (24usize, 9usize, 28usize, 12usize);
    let (mut x, mut y) = (margin, margin);
    for word in text.split_whitespace() {
        let len = word.chars().count().min(40) * char_w;
        if x + len > w - margin && x > margin {
            x = margin;
            y += line_h;
        }
        if y + bar_h > h - margin {
            break;
        }
        for row in y..y + bar_h {
            for col in x..(x + len).min(w - margin) {
                rgb[(row * w + col) * 3..(row * w + col) * 3 + 3].copy_from_slice(&[0xc8, 0xcc, 0xd4]);
            }
        }
        x += len + char_w;
    }
    let mut bytes = Vec::new();
    jpeg_encoder::Encoder::new(&mut bytes, 85)
        .encode(&rgb, THUMB_WIDTH as u16, THUMB_HEIGHT as u16, jpeg_encoder::ColorType::Rgb)
        .map_err(|error| format!("thumbnail: {error}"))?;
    Ok(PublishThumbnail::plain(bytes, ThumbnailMedia::Jpeg, THUMB_WIDTH, THUMB_HEIGHT))
}

/// A reference as the store's exact revision.
pub fn revision_ref(asset: &AssetRef) -> Result<AssetRevisionRef, String> {
    Ok(AssetRevisionRef {
        asset_id: asset.id.parse::<AssetId>().map_err(|_| format!("`{}` is not an asset id", asset.id))?,
        revision: asset.rev.parse::<AssetRevisionId>().map_err(|_| format!("`{}` is not a revision id", asset.rev))?,
    })
}

/// The rights a take is published under: Kokoro speech is generated here
/// (CC0, as other generated media); IndexTTS output carries its weights'
/// restricted licence and stays on the LAN.
pub fn take_rights(engine: VoiceEngine) -> PublishRights {
    match engine {
        VoiceEngine::Kokoro => PublishRights::generated_cc0(),
        VoiceEngine::IndexTts => PublishRights::declared(
            "LicenseRef-bilibili-Model-Use",
            "IndexTTS-2.5 (bilibili IndexTeam)",
            "https://huggingface.co/IndexTeam/IndexTTS-2.5",
            Redistribution::LanLocal,
            DerivativePolicy::LocalPreview,
        ),
    }
}

/// The `Audio` asset of `take`, which speaks `text` in `voice`.
pub fn take_bundle(namespace: &str, take: &Take, text: &str, voice: &VoiceSpec) -> Result<PublishBundle, String> {
    if take.pcm.is_empty() || take.rate == 0 {
        return Err("the take is empty".into());
    }
    let lyrics = take.lyrics(text, voice).to_json(&take_digest(&take.pcm, take.rate));
    let files = vec![
        PublishBundleFile::bytes(FileRole::Audio, MediaType::Wav, encode_wav_mono16(&take.pcm, take.rate), None),
        PublishBundleFile::bytes(FileRole::Lyrics, MediaType::Json, lyrics.into_bytes(), None),
    ];
    let title = title_of(text);
    let mut bundle = PublishBundle::new(namespace, AssetKind::Audio, format!("Voice: {title}"), files, waveform_thumbnail(&take.pcm)?, take_rights(voice.engine));
    bundle.media_millis = ((take.duration() * 1000.0).round() as u32).max(1);
    bundle.capabilities.loopable = false;
    bundle.categories = vec!["voiceover".into()];
    bundle.tags = vec!["voiceover".into(), label(&take.key), label(&format!("timing-{}", take.timing.id()))];
    bundle.generator = "stage-voice".into();
    bundle.backend = voice.engine.id().into();
    bundle.model = voice.id.clone();
    bundle.prompt = text.to_string();
    if let Some(reference) = &voice.reference {
        bundle.dependencies = vec![revision_ref(reference)?];
    }
    Ok(bundle)
}

/// The `Data` asset of `transcript`, which transcribes `source`. Pass the
/// transcript's own `asset_id` to publish a cut of it as a new revision.
pub fn transcript_bundle(namespace: &str, transcript: &Transcript, source: AssetRevisionRef, title: &str, asset_id: Option<AssetId>) -> Result<PublishBundle, String> {
    let files = vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, transcript.to_json("").into_bytes(), None)];
    let words = transcript.lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join(" ");
    let mut bundle = PublishBundle::new(namespace, AssetKind::Data, format!("Transcript: {}", title_of(title)), files, text_thumbnail(&words)?, PublishRights::generated_cc0());
    bundle.asset_id = asset_id;
    bundle.dependencies = vec![source];
    bundle.categories = vec!["transcript".into()];
    bundle.tags = vec!["transcript".into(), label(&format!("lang-{}", transcript.language))];
    bundle.generator = "stage-transcribe".into();
    bundle.backend = transcript.backend.clone();
    bundle.model = transcript.model.clone();
    bundle.description = title_of(&words);
    Ok(bundle)
}

/// The longest search label the store takes.
pub const MAX_LABEL_BYTES: usize = 48;

/// `text` as a search label (tag or category) the store accepts:
/// `[a-z0-9][a-z0-9_-]*`, at most [`MAX_LABEL_BYTES`]. Letters are lowered,
/// anything else becomes `-`.
pub fn label(text: &str) -> String {
    let mut out = text.trim().chars().map(|c| {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' { c } else { '-' }
    }).collect::<String>();
    out.truncate(MAX_LABEL_BYTES);
    if !out.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) {
        out.insert(0, 'x');
        out.truncate(MAX_LABEL_BYTES);
    }
    out
}

/// Whether `text` is a label the store accepts (its `check_label` rule).
pub fn is_label(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_LABEL_BYTES
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}

/// At most 80 characters of `text` on one line.
fn title_of(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(80) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use makepad_asset_client::AssetClient;

    fn asset_ref(published: &makepad_asset_client::PublishedBundle) -> AssetRef {
        AssetRef { id: published.asset_id.to_string(), rev: published.revision.to_string() }
    }

    /// Publish `take` and return the reference a `line(…)` pins. Blocking.
    pub fn publish_take(client: &mut AssetClient, namespace: &str, take: &Take, text: &str, voice: &VoiceSpec) -> Result<AssetRef, String> {
        let bundle = take_bundle(namespace, take, text, voice)?;
        client.publish_bundle(&bundle).map(|published| asset_ref(&published)).map_err(|error| format!("publish take: {error:?}"))
    }

    /// Publish `transcript` of `source` and return the reference a
    /// `Subtitles{transcript: …}` pins. Blocking.
    pub fn publish_transcript(client: &mut AssetClient, namespace: &str, transcript: &Transcript, source: &AssetRef, title: &str, asset_id: Option<AssetId>) -> Result<AssetRef, String> {
        let bundle = transcript_bundle(namespace, transcript, revision_ref(source)?, title, asset_id)?;
        client.publish_bundle(&bundle).map(|published| asset_ref(&published)).map_err(|error| format!("publish transcript: {error:?}"))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{publish_take, publish_transcript};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::TranscriptLine;
    use crate::timing::TimingSource;
    use makepad_story_cut::{take_key, Word};

    const REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";

    fn take() -> Take {
        let pcm = (0..24_000).map(|i| (i as f32 * 0.05).sin() * 0.3).collect::<Vec<_>>();
        let words = vec![Word { text: "That's".into(), start: 0.1, end: 0.4 }, Word { text: "it.".into(), start: 0.45, end: 0.9 }];
        Take { pcm, rate: 24_000, words, timing: TimingSource::Envelope, key: take_key(&VoiceSpec::default(), "That's it."), phones: Vec::new() }
    }

    /// What the contract checks offline (`PublishBundle::validate` is
    /// crate-private to the client): roles carry their media, the
    /// thumbnail is legal.
    fn check(bundle: &PublishBundle) {
        assert!(bundle.files.iter().all(|file| file.role.allows(file.media)), "{:?}", bundle.files.iter().map(|f| (f.role, f.media)).collect::<Vec<_>>());
        assert!(bundle.thumbnail.bytes.starts_with(&[0xff, 0xd8]) && bundle.thumbnail.width >= 256 && bundle.thumbnail.height >= 256);
        assert!(bundle.tags.iter().chain(&bundle.categories).all(|label| is_label(label)), "{:?} {:?}", bundle.tags, bundle.categories);
    }

    #[test]
    fn a_take_is_an_audio_asset_with_its_word_times() {
        let take = take();
        let bundle = take_bundle("stage", &take, "That's it.", &VoiceSpec::default()).unwrap();
        check(&bundle);
        assert_eq!((bundle.kind, bundle.media_millis, bundle.dependencies.len()), (AssetKind::Audio, 1000, 0));
        assert!(bundle.tags.contains(&take.key));
        let lyrics = bundle.files.iter().find(|file| file.role == FileRole::Lyrics).unwrap();
        let doc = Transcript::from_json(&lyrics.bytes, Some(&take_digest(&take.pcm, take.rate))).unwrap();
        assert_eq!(doc.take_info().words, take.words);
        assert_eq!((doc.timing.as_str(), doc.backend.as_str(), doc.model.as_str()), ("envelope", "kokoro", "bm_daniel"));
        let wav = bundle.files.iter().find(|file| file.role == FileRole::Audio).unwrap();
        assert_eq!(crate::wav::decode_wav16(&wav.bytes).unwrap().0.len(), 24_000);
        // An IndexTTS take depends on its reference and keeps its licence.
        let cloned = VoiceSpec { engine: VoiceEngine::IndexTts, reference: Some(AssetRef { id: "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu".into(), rev: REV.into() }), ..VoiceSpec::default() };
        let bundle = take_bundle("stage", &take, "That's it.", &cloned).unwrap();
        check(&bundle);
        assert_eq!(bundle.dependencies.len(), 1);
        assert_eq!(bundle.rights.redistribution, Redistribution::LanLocal);
        assert!(take_bundle("stage", &Take { pcm: vec![], ..take }, "x", &VoiceSpec::default()).is_err());
    }

    #[test]
    fn a_transcript_is_a_text_asset_of_its_source() {
        let transcript = Transcript {
            backend: "whisper".into(), model: "ggml-large-v3-turbo.bin".into(), language: "en".into(), duration_secs: 3.0, timing: "whisper".into(),
            lines: vec![TranscriptLine { start: 0.5, end: 1.0, text: "hello there".into(), words: vec![], confident: false }],
            phones: Vec::new(),
        };
        let source = revision_ref(&AssetRef { id: "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu".into(), rev: REV.into() }).unwrap();
        let bundle = transcript_bundle("stage", &transcript, source, "Demo soundtrack", None).unwrap();
        check(&bundle);
        assert_eq!((bundle.kind, bundle.dependencies.clone(), bundle.description.as_str()), (AssetKind::Data, vec![source], "hello there"));
        let doc = Transcript::from_json(&bundle.files[0].bytes, None).unwrap();
        assert_eq!(doc, transcript);
        assert!(revision_ref(&AssetRef { id: "nope".into(), rev: REV.into() }).is_err());
    }

    #[test]
    fn labels_follow_the_store_rule() {
        assert_eq!(label("timing-tts"), "timing-tts");
        assert_eq!(label("lang-pt-BR"), "lang-pt-br");
        assert_eq!(label("Language: en"), "language--en");
        assert_eq!(label("-x"), "x-x");
        assert_eq!(label(&"a".repeat(60)).len(), MAX_LABEL_BYTES);
        assert!(is_label(&take_key(&VoiceSpec::default(), "Hi.")), "take keys are labels");
        assert!(!is_label("timing:tts") && !is_label("") && !is_label("-a") && !is_label("A"));
    }

    #[test]
    fn titles_are_one_short_line() {
        assert_eq!(title_of(" a\n b "), "a b");
        assert_eq!(title_of(&"x".repeat(100)).chars().count(), 81);
    }
}

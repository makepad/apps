//! Decoding a local audio file to PCM, whatever it is called.
//!
//! WAV and AIFF parse here, MP3, Ogg Vorbis and FLAC go through this repo's
//! own decoders, and everything else -- AAC in an MP4 or M4A, mostly -- goes
//! to the platform's container decoder. The extension is the first answer
//! and the file's own first bytes are the second: a file another decoder in
//! this repo reads is not unplayable, it is misnamed.

use crate::pcm::TrackPcm;
use makepad_asset_data::MediaType;
use makepad_audio_decode::{decode_audio_limited, AudioFormat, Limits as AudioLimits};
use makepad_platform::video_file::VideoFileDecoder;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static DECODER_ALIAS_ID: AtomicU64 = AtomicU64::new(0);

/// A number no other decoder input of this process has had: what a typed
/// link is named by, and what a test names a scratch directory by.
pub fn alias_ticket() -> u64 {
    DECODER_ALIAS_ID.fetch_add(1, Ordering::Relaxed)
}

/// Longest local file the music explorer will lift into memory.
pub const MAX_LOCAL_TRACK_FRAMES: usize = 48_000 * 60 * 15;

/// The platform demuxers take a path's extension as the container hint,
/// and an asset-cache object is named by its digest: nothing in that name
/// says what the bytes are, so an otherwise valid clip is reported as
/// having no video track.
///
/// Such a source -- and ONLY such a source -- gets an extension-bearing
/// hard link beside (but outside) the cache's content-addressed `objects/`
/// tree. A hard link does not duplicate a potentially large clip. The
/// decode thread owns this lease and removes the link only after every
/// reopen/loop has stopped, which keeps detached slot teardown safe.
///
/// A source that already carries a name is handed over by its own path:
/// no directory, no link, and no lease to outlive a crash.
pub struct DecoderInput {
    pub path: String,
    pub alias: Option<PathBuf>,
}

impl DecoderInput {
    pub fn prepare(source: &Path, media: MediaType) -> Result<Self, String> {
        let extension = match media {
            MediaType::Mp4 => "mp4",
            other => return Err(format!("unsupported video media {other:?}")),
        };
        if !source.is_file() {
            return Err(format!("media file not found: {}", source.display()));
        }
        // A source that already carries a name is opened under the name it
        // has. The link below was written for one case -- a cache object,
        // whose name is a digest and says nothing about the container --
        // and every other source was paying for it: a link beside the file
        // on a volume that may allow none, and a hidden directory left in
        // the operator's own music folder.
        if source
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| !value.is_empty())
        {
            if let Some(path) = source.to_str() {
                return Ok(Self { path: path.to_string(), alias: None });
            }
            // A name this platform cannot spell in UTF-8 still goes out
            // under a typed link, exactly as it always has.
        }
        Self::aliased(source, extension)
    }

    /// The typed link, whatever the source is called. The audio path asks
    /// for this by name when a file handed over under its own name turned
    /// out to be something else.
    pub fn aliased(source: &Path, extension: &str) -> Result<Self, String> {
        if !source.is_file() {
            return Err(format!("media file not found: {}", source.display()));
        }

        // Cache paths are `<root>/objects/<prefix>/<digest>`. Keep aliases
        // under `<root>/decoder-input/`; for a nameless source that is not a
        // cache object, a sibling private directory. The cache root is the
        // app's own volume, where linking works. The sibling case is the one
        // a link-less volume can still refuse, and a byte copy is the answer
        // there -- with the sweep for what a crash leaves, both later.
        let parent = source.parent().ok_or("media path has no parent")?;
        let objects = parent.parent();
        let alias_dir = match objects {
            Some(objects) if objects.file_name().is_some_and(|name| name == "objects") => objects
                .parent()
                .unwrap_or(parent)
                .join("decoder-input"),
            _ => parent.join(".makepad-decoder-input"),
        };
        std::fs::create_dir_all(&alias_dir)
            .map_err(|e| format!("create decoder input directory: {e}"))?;
        let ticket = alias_ticket();
        let alias = alias_dir.join(format!(
            "decoder-{}-{ticket}.{extension}",
            std::process::id()
        ));
        std::fs::hard_link(source, &alias)
            .map_err(|e| format!("create typed decoder link: {e}"))?;
        let path = match alias.to_str() {
            Some(path) => path.to_string(),
            None => {
                let _ = std::fs::remove_file(&alias);
                return Err(format!("non-utf8 decoder path: {}", alias.display()));
            }
        };
        Ok(Self { path, alias: Some(alias) })
    }
}

impl Drop for DecoderInput {
    fn drop(&mut self) {
        if let Some(alias) = self.alias.take() {
            let _ = std::fs::remove_file(alias);
        }
    }
}

// ---------------------------------------------------------------------------
// audio decode (music tracks + sfx pads)
// ---------------------------------------------------------------------------

/// Minimal bounded RIFF/WAVE parse (eight, sixteen, twenty-four and
/// thirty-two bit PCM, and float32), emitting interleaved stereo i16.
///
/// Reachable across the crate because a wav is not only a track: the
/// rack loads the operator's own impulse responses through this same
/// parser rather than carrying a second, slightly different one.
pub fn parse_wav(bytes: &[u8], max_frames: usize) -> Result<TrackPcm, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut format = 0u16;
    let mut channels = 0u16;
    let mut sample_rate = 0u32;
    let mut bits = 0u16;
    let mut data: Option<&[u8]> = None;
    let mut at = 12usize;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body_end = (at + 8 + size).min(bytes.len());
        let body = &bytes[at + 8..body_end];
        match id {
            b"fmt " if body.len() >= 16 => {
                format = u16::from_le_bytes(body[0..2].try_into().unwrap());
                channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
                sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
                // The extensible tag says the real format is in the
                // sub-format that follows: two bytes of it name the same
                // codes the plain tag would. Every file wider than stereo
                // or deeper than sixteen bits is written this way, and the
                // parser used to refuse them all as "unsupported 65534".
                if format == 0xFFFE && body.len() >= 26 {
                    format = u16::from_le_bytes(body[24..26].try_into().unwrap());
                }
            }
            b"data" => data = Some(body),
            _ => {}
        }
        at = body_end + (size & 1);
    }
    let data = data.ok_or("wav: no data chunk")?;
    if channels == 0 || sample_rate == 0 {
        return Err("wav: no fmt chunk".into());
    }
    let ch = channels as usize;
    // The front pair, and mono to both ears: the other decode paths took
    // that law in 62ec728ae and this parser went on pairing the first
    // channel with the LAST, which on a six-channel file is a rear or the
    // low-frequency send.
    let [left, right] = crate::dsp_math::stereo_pair_indices(ch).ok_or("wav: no channels")?;
    // The sample law for the format the chunk names, decided once: eight
    // bits are unsigned, twenty-four and thirty-two keep their top sixteen,
    // float clamps to full scale. Sixteen-bit integer and float were the
    // only two accepted, so a 24-bit export -- the common lossless case --
    // failed on the deck. Anything else is still refused by name.
    let bytes_per_sample = match (format, bits) {
        (1, 8) | (1, 16) | (1, 24) | (1, 32) | (3, 32) => (bits / 8) as usize,
        other => return Err(format!("wav: unsupported format {other:?}")),
    };
    let sample_at = |frame: &[u8], i: usize| -> i16 {
        let s = &frame[i * bytes_per_sample..(i + 1) * bytes_per_sample];
        match (format, bits) {
            (1, 8) => ((s[0] as i16) - 128) << 8,
            (1, 16) => i16::from_le_bytes([s[0], s[1]]),
            (1, 24) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 16) as i16,
            (1, 32) => (i32::from_le_bytes([s[0], s[1], s[2], s[3]]) >> 16) as i16,
            _ => {
                let v = f32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                (v.clamp(-1.0, 1.0) * 32767.0) as i16
            }
        }
    };
    let mut frames: Vec<[i16; 2]> = Vec::new();
    for frame in data.chunks_exact(bytes_per_sample * ch) {
        if frames.len() >= max_frames {
            return Err("audio clip exceeds the decode budget".into());
        }
        frames.push([sample_at(frame, left), sample_at(frame, right)]);
    }
    if frames.is_empty() {
        return Err("wav: empty data".into());
    }
    Ok(TrackPcm { frames, sample_rate })
}

/// The rate an AIFF writes: eighty bits of extended floating point, which
/// nothing in the language reads.
///
/// A sign bit, a fifteen-bit exponent biased by 16383, and a sixty-four bit
/// mantissa whose leading one is written out rather than implied. Decoded by
/// shifting rather than through a float, because every rate in use is a
/// whole number and a round trip through an f64 is one more place for
/// 44099.999 to come from. `None` for zero, for a negative, for a full
/// exponent, and for anything that will not fit.
pub fn extended80_rate(field: &[u8; 10]) -> Option<u32> {
    let exponent = u16::from_be_bytes([field[0], field[1]]);
    if exponent & 0x8000 != 0 || exponent == 0 || exponent == 0x7FFF {
        return None;
    }
    let mantissa = u64::from_be_bytes(field[2..10].try_into().ok()?);
    if mantissa == 0 {
        return None;
    }
    // The mantissa's top bit stands for two to the exponent, so the value is
    // the mantissa shifted by however far bit sixty-three is from bit zero.
    let shift = exponent as i32 - 16383 - 63;
    let value = match shift {
        0 => mantissa,
        s if s > 0 => mantissa.checked_shl(s as u32)?,
        s => {
            let s = (-s) as u32;
            if s >= 64 {
                return None;
            }
            // To nearest, so a rate written one bit under still lands on it.
            (mantissa >> s) + ((mantissa >> (s - 1)) & 1)
        }
    };
    u32::try_from(value).ok().filter(|rate| *rate > 0)
}

/// Bounded FORM/AIFF (and AIFC) parse, emitting interleaved stereo i16 --
/// the big-endian sibling of `parse_wav`, shaped the same way.
///
/// It parts company in exactly two places, both deliberate: eight-bit
/// samples are SIGNED here where a RIFF file's are unsigned around 128, and
/// an AIFF declares its own frame count, so sound that ends early is damage
/// rather than a shorter track.
pub fn parse_aiff(bytes: &[u8], max_frames: usize) -> Result<TrackPcm, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"FORM" {
        return Err("not a FORM/AIFF file".into());
    }
    let aifc = match &bytes[8..12] {
        b"AIFF" => false,
        b"AIFC" => true,
        _ => return Err("not a FORM/AIFF file".into()),
    };
    let mut channels = 0usize;
    let mut declared_frames = 0usize;
    let mut bits = 0u16;
    let mut sample_rate = 0u32;
    let mut little_endian = false;
    let mut sound: Option<&[u8]> = None;
    let mut at = 12usize;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        // A size out of the file is not to be trusted into an add.
        let body_end = (at + 8).saturating_add(size).min(bytes.len());
        let body = &bytes[at + 8..body_end];
        match id {
            b"COMM" => {
                if body.len() < 18 {
                    return Err("aiff: short COMM chunk".into());
                }
                channels = u16::from_be_bytes(body[0..2].try_into().unwrap()) as usize;
                declared_frames = u32::from_be_bytes(body[2..6].try_into().unwrap()) as usize;
                bits = u16::from_be_bytes(body[6..8].try_into().unwrap());
                sample_rate = extended80_rate(body[8..18].try_into().unwrap())
                    .ok_or("aiff: no sample rate")?;
                if aifc {
                    if body.len() < 22 {
                        return Err("aiff: short COMM chunk".into());
                    }
                    match &body[18..22] {
                        // Big-endian PCM, which is what this parser reads.
                        // Two spellings of the identical bytes: some
                        // converters write the second, and refusing it
                        // refused a file this parser can read.
                        b"NONE" | b"twos" => {}
                        // The same samples with their bytes the other way.
                        b"sowt" => little_endian = true,
                        id => {
                            let name: String = id
                                .iter()
                                .map(|b| if b.is_ascii_graphic() { *b as char } else { '?' })
                                .collect();
                            return Err(format!("aiff: unsupported compression {name}"));
                        }
                    }
                }
            }
            b"SSND" => {
                if body.len() < 8 {
                    return Err("aiff: short SSND chunk".into());
                }
                let offset = u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize;
                // The block size at 4..8 is for a writer aligning its own
                // chunks and is read by nobody; it is not read here either.
                sound = Some(body.get(8usize.saturating_add(offset)..).unwrap_or_default());
            }
            _ => {}
        }
        at = body_end + (size & 1);
    }
    let sound = sound.ok_or("aiff: no SSND chunk")?;
    if channels == 0 || sample_rate == 0 {
        return Err("aiff: no COMM chunk".into());
    }
    let [left, right] =
        crate::dsp_math::stereo_pair_indices(channels).ok_or("aiff: no channels")?;
    // Two's complement, most significant byte first, in whole bytes. EIGHT
    // bits are SIGNED here, which is the one place this parser parts company
    // with parse_wav: a RIFF file's eight-bit samples are unsigned around
    // 128 and an AIFF's are signed around zero, and reading one law with the
    // other inverts every sample's top bit.
    let bytes_per_sample = match bits {
        8 | 16 | 24 | 32 => (bits / 8) as usize,
        other => return Err(format!("aiff: unsupported sample size {other}")),
    };
    let sample_at = |frame: &[u8], i: usize| -> i16 {
        let s = &frame[i * bytes_per_sample..(i + 1) * bytes_per_sample];
        let byte = |n: usize| if little_endian { s[bytes_per_sample - 1 - n] } else { s[n] };
        match bytes_per_sample {
            1 => (byte(0) as i8 as i16) << 8,
            2 => i16::from_be_bytes([byte(0), byte(1)]),
            3 => (i32::from_be_bytes([byte(0), byte(1), byte(2), 0]) >> 16) as i16,
            _ => (i32::from_be_bytes([byte(0), byte(1), byte(2), byte(3)]) >> 16) as i16,
        }
    };
    let block = bytes_per_sample * channels;
    let available = sound.len() / block.max(1);
    if declared_frames == 0 {
        return Err("aiff: empty sound data".into());
    }
    // The frame count is DECLARED here, unlike a RIFF file where the data
    // chunk's own length IS the count. So a file whose sound ends before its
    // header promised is damaged rather than short, and is refused as one:
    // playing the part that survived would hide the damage behind a deck
    // that looks like it worked.
    if available < declared_frames {
        return Err(format!(
            "aiff: sound data ends {} frames early",
            declared_frames - available
        ));
    }
    // Checked BEFORE the reserve, which parse_wav cannot do because it has
    // no count to check. Same wording and same boundary: a clip of exactly
    // the budget loads, one frame more does not.
    if declared_frames > max_frames {
        return Err("audio clip exceeds the decode budget".into());
    }
    let mut frames: Vec<[i16; 2]> = Vec::with_capacity(declared_frames);
    for frame in sound.chunks_exact(block).take(declared_frames) {
        frames.push([sample_at(frame, left), sample_at(frame, right)]);
    }
    Ok(TrackPcm { frames, sample_rate })
}

/// What a local file's NAME says its container is -- the one table.
///
/// `None` is not "an MP4": it means the name names no decoder this app
/// has. The two readers answer that differently and both are right. A deck
/// turns it into [`MediaType::Mp4`], which is the arm below that looks at
/// the file's own first bytes before asking the platform; the import
/// scanner turns it into "not something the catalog publishes yet".
///
/// The extension is the whole answer HERE. Bytes are consulted in the
/// `Mp4` arm of [`decode_audio_clip`], never in this table.
///
/// The deck side used to spell this out three times over and those copies
/// had drifted from the import scanner's: `.wave` is a name the drop
/// filter accepts and the importer publishes, and every deck-side copy
/// dropped it through to the platform decoder, which said the RIFF file it
/// was handed had no video stream.
pub fn named_audio_media(extension: &str) -> Option<MediaType> {
    match extension.to_ascii_lowercase().as_str() {
        "wav" | "wave" => Some(MediaType::Wav),
        "ogg" | "oga" => Some(MediaType::Ogg),
        "mp3" => Some(MediaType::Mp3),
        _ => None,
    }
}

/// The container a local path is decoded as: the table, with the deck's
/// own answer for a name it does not know.
pub fn local_media_type(path: &Path) -> MediaType {
    path.extension()
        .and_then(|value| value.to_str())
        .and_then(named_audio_media)
        .unwrap_or(MediaType::Mp4)
}

/// Decode an audio clip fully to memory. WAV and AIFF both parse directly
/// here, MP3 and Ogg Vorbis go through this repo's own decoders, and
/// anything else has its own first bytes looked at before it is handed to
/// the platform decoder for its sound.
pub fn decode_audio_clip(
    path: &PathBuf,
    media: MediaType,
    max_frames: usize,
) -> Result<TrackPcm, String> {
    match media {
        MediaType::Wav => {
            let bytes = makepad_asset_data::source::read(path).map_err(|e| e.to_string())?;
            parse_wav(&bytes, max_frames).or_else(|refused| {
                retry_by_content(path, LocalContainer::Riff, refused, max_frames)
            })
        }
        MediaType::Mp4 => {
            // Everything unrecognised lands in this arm: the local
            // extension map falls back to Mp4, and so does the store's
            // `MediaType`, whose enum has no seat for FLAC at all --
            // that enum is a wire contract and a local file has no
            // business widening it. So before handing the file to the
            // platform, look at what it actually IS. A `.flac` has been
            // offered by the explorer all along and failed here with
            // "no video stream", which is the platform decoder being
            // asked for something it was never given.
            let named = match sniff_container(&audio_magic(path).unwrap_or_default()) {
                // The frame scan is not let loose here. A head this short
                // cannot confirm a frame anyway -- the shortest frame is
                // ninety-six bytes -- and if it ever could, the answer
                // would still be the platform: a file that reached this
                // arm is far likelier to be a raw stream of another codec
                // than an MP3, and a real MP3 is named by its extension or
                // by the store long before it gets here.
                Some(LocalContainer::Repo(AudioFormat::Mp3)) | None => LocalContainer::Platform,
                Some(found) => found,
            };
            // The local parsers are PREFERRED here, not exclusive: a RIFF
            // this app cannot decode -- a companded or an adaptive one --
            // is still a container the platform reads, and it loaded
            // through this arm before there was a wrapper row at all.
            decode_container(path, named, max_frames).or_else(|refused| match named {
                LocalContainer::Riff | LocalContainer::Aiff => {
                    // And when the platform will not have it either, the
                    // LOCAL parser's refusal is the one to show: it names
                    // the codec the file actually carries, where the
                    // platform can only say it does not know the bytes.
                    decode_platform_container(path, max_frames, container_trim_enabled())
                        .map_err(|_| refused)
                }
                _ => Err(refused),
            })
        }
        // MP3 and Ogg Vorbis go through the repo's own decoders, the same way
        // WAV does: whole file in, interleaved PCM out, no platform codec.
        MediaType::Mp3 => decode_repo_audio(path, AudioFormat::Mp3, max_frames).or_else(
            |refused| {
                retry_by_content(
                    path,
                    LocalContainer::Repo(AudioFormat::Mp3),
                    refused,
                    max_frames,
                )
            },
        ),
        // One case this deliberately does not catch: a FLAC stream inside
        // an Ogg container sniffs as the same container that just refused
        // it, so there is no retry -- this repo has no Ogg-FLAC demux to
        // retry into, and the Vorbis decoder's own error is the honest one.
        MediaType::Ogg => decode_repo_audio(path, AudioFormat::OggVorbis, max_frames).or_else(
            |refused| {
                retry_by_content(
                    path,
                    LocalContainer::Repo(AudioFormat::OggVorbis),
                    refused,
                    max_frames,
                )
            },
        ),
        other => Err(format!("unsupported audio media {other:?}")),
    }
}

/// A decoder's interleaved output in the mixer's own frames: the stereo
/// pair of however many channels it hands back, clipped to i16. The one
/// conversion, so the whole decode and the streamed one cannot drift.
pub fn push_interleaved(out: &mut Vec<[i16; 2]>, pcm: &[f32], channels: usize) {
    let sample = |v: f32| (v.clamp(-1.0, 1.0) * 32767.0) as i16;
    for frame in pcm.chunks_exact(channels.max(1)) {
        let Some(pair) = crate::dsp_math::stereo_pair(frame) else { continue };
        out.push([sample(pair[0]), sample(pair[1])]);
    }
}

/// One container, one decoder. Total, and it never re-enters
/// `decode_audio_clip`, so a retry cannot start another one.
pub fn decode_container(
    path: &PathBuf,
    container: LocalContainer,
    max_frames: usize,
) -> Result<TrackPcm, String> {
    match container {
        LocalContainer::Riff => {
            let bytes = makepad_asset_data::source::read(path).map_err(|e| e.to_string())?;
            parse_wav(&bytes, max_frames)
        }
        LocalContainer::Aiff => {
            let bytes = makepad_asset_data::source::read(path).map_err(|e| e.to_string())?;
            parse_aiff(&bytes, max_frames)
        }
        LocalContainer::Repo(format) => decode_repo_audio(path, format, max_frames),
        LocalContainer::Platform => {
            decode_platform_container(path, max_frames, container_trim_enabled())
        }
    }
}

/// How far the second look reads. An MP3 frame is only real once the frame
/// after it agrees, which is the one thing here that needs a window rather
/// than a marker; this is the decoder's own scan window, so the probe and
/// the decode make the same call.
pub const CONTENT_SCAN_BYTES: usize = makepad_audio_decode::mp3::SNIFF_WINDOW;

/// A second look when the decoder the NAME chose refused the bytes.
///
/// The extension stays the first answer: it is right for every file that is
/// what it says it is, and it costs nothing. But a refusal is the one
/// moment when the name is KNOWN to be no use, and a file another decoder
/// in this repo reads is not unplayable, it is misnamed. Only a DIFFERENT
/// container is retried -- when the bytes agree with the decoder that just
/// refused them the file really is broken, and its own error is the honest
/// one to show. When a retry does run, its error is the one that stands:
/// telling an operator holding a truncated container that it is "not a
/// RIFF/WAVE file" is worse than useless.
pub fn retry_by_content(
    path: &PathBuf,
    refused_by: LocalContainer,
    refusal: String,
    max_frames: usize,
) -> Result<TrackPcm, String> {
    let Some(window) = audio_head(path, CONTENT_SCAN_BYTES) else {
        return Err(refusal);
    };
    // Two readers disagree about how long a tag is, and the difference is
    // not the ten bytes of a footer: the frame side refuses a size field
    // that is not seven bits to the byte and answers zero, which would
    // leave the whole window inside the tag. When the first look finds
    // nothing, look again from where the other side says the tag ends.
    let found = sniff_container(&window).or_else(|| {
        let skip = makepad_audio_decode::flac::metadata::skip_id3(&window);
        window.get(skip..).and_then(sniff_container)
    });
    let Some(found) = found else { return Err(refusal) };
    if found == refused_by {
        return Err(refusal);
    }
    decode_container(path, found, max_frames)
}

/// Whether the sound of a container is cut to the edit its index declares.
/// Off unless `VJ_CONTAINER_EDIT_TRIM=1`: what a platform does with the
/// encoder's front padding differs from one to the next and has been
/// measured on one, so the default is the decode exactly as it was.
pub fn container_trim_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("VJ_CONTAINER_EDIT_TRIM").map(|value| value.trim() == "1").unwrap_or(false)
    })
}

/// One codec frame: how far a decode may miss the length the container
/// declares and still be the same sound.
pub const TRIM_SLACK_FRAMES: usize = 1024;

/// Where the sound the container declares sits inside what the platform
/// handed back. `skip` and `keep` are the edit in frames; `decoded` is how
/// many frames came out. Returns (frames to drop at the front, frames to
/// keep after them).
///
/// A platform that applied the edit itself hands back about `keep`
/// frames; one that left the padding in hands back about `skip + keep`,
/// often with the encoder's tail padding after that. The first case is
/// tested first, and a decode that is not clearly the second is left
/// alone: dropping `skip` frames from a sound that was already cut would
/// take the beginning of the music with it. The slack is always less than
/// the padding, so the two cases cannot meet.
pub fn container_front_trim(decoded: usize, skip: u64, keep: u64) -> (usize, usize) {
    let (skip, keep) = (skip as usize, keep as usize);
    if skip == 0 || keep == 0 {
        return (0, decoded);
    }
    let slack = TRIM_SLACK_FRAMES.min(skip - 1);
    if decoded.abs_diff(keep) <= slack {
        return (0, decoded);
    }
    if decoded + slack >= skip + keep {
        return (skip, keep.min(decoded - skip));
    }
    (0, decoded)
}

/// What the container's index says about its sound, read from the file
/// without decoding it: the `moov` box alone, found and bounded the way the
/// picture path finds it.
pub fn container_audio_edit(path: &Path) -> Result<Option<makepad_mp4_index::AudioEdit>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let header = makepad_mp4_index::locate_moov(size, &mut |offset, len| {
        file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(len);
        (&mut file).take(len as u64).read_to_end(&mut out).map_err(|e| e.to_string())?;
        Ok(out)
    })
    .map_err(|e| e.to_string())?;
    let moov_size = header.size.unwrap_or(size.saturating_sub(header.offset));
    if moov_size > makepad_mp4_index::MAX_MOOV_BYTES || moov_size < header.header_len {
        return Err("mp4 index over the size limit".into());
    }
    let payload = (moov_size - header.header_len) as usize;
    file.seek(SeekFrom::Start(header.offset + header.header_len)).map_err(|e| e.to_string())?;
    let mut moov = vec![0u8; payload];
    file.read_exact(&mut moov).map_err(|e| e.to_string())?;
    makepad_mp4_index::audio_edit(&moov).map_err(|e| e.to_string())
}

/// Cut the decoded sound to the edit the container declares, when the
/// platform left the encoder's front padding in. Says what it did in the
/// log, and when the index could not be read; the sound is then used as
/// decoded.
pub fn trim_to_container_edit(path: &Path, frames: &mut Vec<[i16; 2]>, sample_rate: u32) {
    match container_audio_edit(path) {
        Ok(Some(edit)) => {
            let (skip, keep) = edit.in_samples(sample_rate);
            let (drop, keep) = container_front_trim(frames.len(), skip, keep);
            if drop > 0 || keep < frames.len() {
                makepad_platform::log!(
                    "container edit: {} frames decoded, {keep} declared after {skip} of padding: cut to the edit",
                    frames.len()
                );
                frames.drain(..drop);
                frames.truncate(keep);
            }
        }
        Ok(None) => {}
        Err(error) => makepad_platform::log!(
            "container edit: index not read ({error}); the sound is used as decoded"
        ),
    }
}

/// The platform's own decoder, for its sound alone.
///
/// `trim` is [`container_trim_enabled`] on every production path; it is a
/// parameter so a test says which decode it is measuring rather than
/// asking the process environment, which one `OnceLock` fixes for the
/// whole run.
pub fn decode_platform_container(
    path: &PathBuf,
    max_frames: usize,
    trim: bool,
) -> Result<TrackPcm, String> {
    // A cache object's name is a digest and the platform demuxers key on
    // the extension, so a nameless source goes out under a typed link. A
    // file that has a name is opened by it.
    //
    // Opened for its SOUND: this path never wanted a picture, and every
    // platform opener refuses a file that has none before it looks at the
    // audio.
    let (_input, mut decoder) = open_container_audio(path)?;
    let mut frames: Vec<[i16; 2]> = Vec::new();
    let mut sample_rate = decoder.info().audio_sample_rate.max(1);
    loop {
        match decoder.next_audio().map_err(|e| e.to_string())? {
            None => break,
            Some(chunk) => {
                sample_rate = chunk.sample_rate.max(1);
                let ch = chunk.channels.max(1) as usize;
                for frame in chunk.samples.chunks_exact(ch) {
                    if frames.len() >= max_frames {
                        return Err("audio clip exceeds the decode budget".into());
                    }
                    let Some(pair) = crate::dsp_math::stereo_pair(frame) else {
                        continue;
                    };
                    frames.push(pair);
                }
            }
        }
    }
    if frames.is_empty() {
        return Err("the container decoded to zero frames".into());
    }
    if trim {
        trim_to_container_edit(path, &mut frames, sample_rate);
    }
    Ok(TrackPcm { frames, sample_rate })
}

/// Open a container for its sound, under its own name when it has one.
///
/// The lease comes back with the decoder because it owns the typed link:
/// dropping it removes the link, and the decode has to finish first.
///
/// A file whose name MISDESCRIBES its container used to reach the platform
/// under a correct hint, because everything was renamed. Now that a named
/// file goes out as itself, such a file is offered once more under a typed
/// link -- the door it always went through -- and the FIRST refusal is the
/// one reported, since that is the answer about the file as it is named.
pub fn open_container_audio(path: &PathBuf) -> Result<(DecoderInput, VideoFileDecoder), String> {
    let input = DecoderInput::prepare(path, MediaType::Mp4)?;
    match VideoFileDecoder::open_audio(&input.path) {
        Ok(decoder) => Ok((input, decoder)),
        Err(first) if input.alias.is_none() => {
            let leased = DecoderInput::aliased(path, "mp4").map_err(|_| first.to_string())?;
            match VideoFileDecoder::open_audio(&leased.path) {
                Ok(decoder) => Ok((leased, decoder)),
                Err(_) => Err(first.to_string()),
            }
        }
        Err(other) => Err(other.to_string()),
    }
}

/// Whole file in, interleaved PCM out, no platform codec.
pub fn decode_repo_audio(
    path: &PathBuf,
    format: AudioFormat,
    max_frames: usize,
) -> Result<TrackPcm, String> {
    let bytes = makepad_asset_data::source::read(path).map_err(|e| e.to_string())?;
    let audio =
        decode_audio_limited(&bytes, format, AudioLimits::with_max_frames(max_frames))
            .map_err(|e| e.to_string())?;
    let channels = audio.channels.max(1) as usize;
    let mut frames: Vec<[i16; 2]> = Vec::with_capacity(audio.frames());
    push_interleaved(&mut frames, &audio.pcm_interleaved_f32, channels);
    if frames.is_empty() {
        return Err(format!("{format:?} decoded to zero frames"));
    }
    Ok(TrackPcm { frames, sample_rate: audio.rate.max(1) })
}

/// The longest ID3v2 block a sniff will read past. Tags carrying cover art
/// run to a few hundred kilobytes; past this the file is claiming something
/// silly and can go to the platform, which will make its own mind up.
pub const MAX_TAG_SKIP: usize = 4 << 20;

/// Enough of a file's front to name its format.
///
/// Four bytes of magic is all a marker needs, but a tagger may have put an
/// ID3v2 block in front of one -- so the header is read first and what it
/// says about its own length decides how much more to read. Bounded,
/// because that length comes out of the file: a corrupt one must not turn
/// a sniff into a whole-file read of something that was never audio.
pub fn audio_magic(path: &PathBuf) -> Option<Vec<u8>> {
    audio_head(path, 4)
}

/// The same, with `tail` bytes wanted past whatever tag is in front. Four
/// is all a marker needs; a frame scan wants a window, because a frame is
/// only real once the frame after it agrees.
pub fn audio_head(path: &Path, tail: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 12];
    file.read_exact(&mut head).ok()?;
    let skip = makepad_audio_decode::flac::metadata::skip_id3(&head);
    if skip > MAX_TAG_SKIP {
        return None;
    }
    let want = skip.saturating_add(tail);
    if want <= head.len() {
        return Some(head.to_vec());
    }
    let mut out = head.to_vec();
    // A short read is what a small file HAS, not a failure: the probe works
    // on whatever the front of the file holds.
    file.take((want - head.len()) as u64).read_to_end(&mut out).ok()?;
    Some(out)
}

/// What a file's first bytes say it holds, whatever its name says.
///
/// Deliberately not a `MediaType`: that enum is a wire contract with no
/// seat for a FLAC or an AIFF, and a file on this machine has no business
/// widening it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalContainer {
    /// A RIFF wrapper, read by `parse_wav`.
    Riff,
    /// A FORM wrapper, read by `parse_aiff`.
    Aiff,
    /// One of this repo's own decoders.
    Repo(AudioFormat),
    /// A box container the platform decoder demuxes.
    Platform,
}

/// A whole `ftyp` box at the front. The size is the only other field worth
/// reading: zero means "to the end of the file" and one means a 64-bit size
/// follows, neither of which a real `ftyp` box uses, and refusing them keeps
/// out a text file that happens to read `....ftyp`.
pub fn is_box_container(bytes: &[u8]) -> bool {
    let Some(head) = bytes.get(0..8) else { return false };
    &head[4..8] == b"ftyp" && u32::from_be_bytes(head[0..4].try_into().unwrap()) >= 8
}

/// Which container a file's first bytes name, whatever it is called.
///
/// Exact markers first and the frame scan last: no file begins with
/// `RIFF....WAVE` by accident, while a run of audio bytes containing an MP3
/// sync is ordinary. The scan is never satisfied by a tag prefix -- a
/// tagged stream of another codec carries the same three letters.
pub fn sniff_container(bytes: &[u8]) -> Option<LocalContainer> {
    // The length word at 4..8 is deliberately not checked: a streaming
    // writer leaves it zero or all ones.
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return Some(LocalContainer::Riff);
    }
    if bytes.len() >= 12
        && &bytes[0..4] == b"FORM"
        && matches!(&bytes[8..12], b"AIFF" | b"AIFC")
    {
        return Some(LocalContainer::Aiff);
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"OggS" {
        return Some(LocalContainer::Repo(AudioFormat::OggVorbis));
    }
    // Exact, but a tagger may have put a block in front of it; that
    // function reads past one.
    if makepad_audio_decode::flac::metadata::looks_like_flac(bytes) {
        return Some(LocalContainer::Repo(AudioFormat::Flac));
    }
    if is_box_container(bytes) {
        return Some(LocalContainer::Platform);
    }
    // Last, and only on a frame the frame after it agrees with.
    if makepad_audio_decode::mp3::holds_mp3_frame(bytes) {
        return Some(LocalContainer::Repo(AudioFormat::Mp3));
    }
    None
}

// ---------------------------------------------------------------------------
// local files
// ---------------------------------------------------------------------------

/// Audio extensions the local explorer lane will offer. Both spellings of
/// an AIFF: the four-letter one has been listed all along and could not be
/// loaded, and the three-letter one is what the desktop tools write.
pub const LOCAL_AUDIO_EXTENSIONS: [&str; 10] =
    ["wav", "mp3", "ogg", "oga", "m4a", "aac", "flac", "aiff", "aif", "mp4"];

/// Decode a local audio file. WAV, MP3 and Ogg Vorbis parse in-process
/// (`makepad-audio-decode`); everything else goes to `decode_audio_clip`,
/// which looks at the file own first bytes before handing it to the
/// platform decoder. That is how a `.flac` reaches the FLAC decoder this
/// repo already has, and how an `.aiff` reaches the parser next to the WAV
/// one: there is no `MediaType` that names either.
pub fn decode_audio_file(path: &Path) -> Result<TrackPcm, String> {
    let media = local_media_type(path);
    decode_audio_clip(&path.to_path_buf(), media, MAX_LOCAL_TRACK_FRAMES)
}

/// List playable audio files in a directory (not recursive, sorted).
pub fn list_local_audio(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .is_some_and(|e| LOCAL_AUDIO_EXTENSIONS.contains(&e.as_str()))
        })
        .collect();
    out.sort();
    out
}

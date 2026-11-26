//! One off-UI decode of a manifest-typed Wav, Mp3, or Ogg file into stereo i16.
//!
//! The caller reserves [`TRACK_RESERVATION_BYTES`] on `AudioDevice` before
//! calling [`decode_prepared`], then finishes that lease with the returned
//! [`TrackPcm`]. This function additionally holds one process-local transient
//! permit for its own encoded-file buffer and PCM realloc workspace. Codec
//! header tables and granule scratch stay inside the existing decoder
//! `Limits` (frame count and channel count); they are not charged to either
//! of those two reservations, and this module does not add a general budget.

use crate::decode::{self, push_interleaved, MAX_LOCAL_TRACK_FRAMES};
use crate::pcm::TrackPcm;
use makepad_asset_data::MediaType;
use makepad_audio_decode::mp3::Mp3Decoder;
use makepad_audio_decode::vorbis::VorbisDecoder;
use makepad_audio_decode::{AudioError, Limits};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Largest encoded file this decode will read: the decode reservation, so the bound is the
/// decoded length (15 minutes of stereo) and an uncompressed WAV of a long track still loads.
/// The encoded bytes are dropped once decoded.
pub const MAX_ENCODED_BYTES: u64 = TRACK_RESERVATION_BYTES as u64;

/// Bytes the caller reserves on `AudioDevice` before this function runs.
/// The worst-case stereo clip is `48_000 * 60 * 15 * 4` = 172_800_000 bytes,
/// which fits in this reservation. `shrink_to_fit` runs here so a short
/// track does not keep the worst-case allocation.
pub const TRACK_RESERVATION_BYTES: usize = 256 * 1024 * 1024;

const MIN_RATE: u32 = 8_000;
const MAX_RATE: u32 = 384_000;

static DECODE_BUSY: AtomicBool = AtomicBool::new(false);

struct DecodePermit;

impl DecodePermit {
    fn try_acquire() -> Result<Self, String> {
        match DECODE_BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire) {
            Ok(false) => Ok(Self),
            _ => Err("decoder busy".into()),
        }
    }
}

impl Drop for DecodePermit {
    fn drop(&mut self) {
        DECODE_BUSY.store(false, Ordering::Release);
    }
}

fn limits() -> Limits {
    Limits { max_frames: MAX_LOCAL_TRACK_FRAMES, max_channels: 8 }
}

fn aborted_err() -> String {
    "audio decode aborted".into()
}

/// Decode `path` as the manifest `media` (Wav, Mp3, or Ogg only).
///
/// Call off the UI thread. One decode runs at a time; a second caller gets
/// `"decoder busy"` and does not read or allocate. `aborted` is polled
/// before any file IO, between codec blocks, and before the `TrackPcm` is
/// returned.
pub fn decode_prepared(
    path: &Path,
    media: MediaType,
    aborted: &dyn Fn() -> bool,
) -> Result<TrackPcm, String> {
    let _permit = DecodePermit::try_acquire()?;
    if aborted() {
        return Err(aborted_err());
    }
    let bytes = read_encoded(path)?;
    if aborted() {
        return Err(aborted_err());
    }
    let mut track = match media {
        MediaType::Wav => decode::parse_wav(&bytes, MAX_LOCAL_TRACK_FRAMES)?,
        MediaType::Mp3 => decode_mp3(&bytes, aborted)?,
        MediaType::Ogg => decode_vorbis(&bytes, aborted)?,
        other => return Err(format!("unsupported audio media {other:?}")),
    };
    if track.frames.is_empty() {
        return Err(AudioError::Empty.to_string());
    }
    if !(MIN_RATE..=MAX_RATE).contains(&track.sample_rate) {
        return Err(format!("audio sample rate {} out of range", track.sample_rate));
    }
    // Compact on this worker. Peak while the old buffer and the copy both
    // exist is full PCM capacity plus the compact copy plus the encoded
    // bytes: under 512 MiB, covered by the caller's 256 MiB reservation
    // plus this function's 256 MiB transient workspace. WAV's parser grows
    // geometrically up to `next_power_of_two(MAX_LOCAL_TRACK_FRAMES) * 4`
    // (256 MiB); the same compact copy and encoded cap apply. The stereo
    // ceiling is 172_800_000 bytes.
    track.frames.shrink_to_fit();
    if aborted() {
        return Err(aborted_err());
    }
    Ok(track)
}

/// Metadata length, then one bounded read of that same file.
///
/// Capacity is `declared_len + 1` and the read stops at `declared_len + 1`.
/// A length that no longer matches the metadata is a changed file: return
/// without growing the buffer past that reservation. Anything above 64 MiB
/// is refused.
fn read_encoded(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let declared = file.metadata().map_err(|e| e.to_string())?.len();
    if declared > MAX_ENCODED_BYTES {
        return Err(format!("encoded audio exceeds {MAX_ENCODED_BYTES} bytes"));
    }
    let cap = (declared as usize).saturating_add(1);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(cap)
        .map_err(|e| format!("encoded audio reservation failed: {e}"))?;
    file.take(declared.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != declared {
        return Err("audio file changed during read".into());
    }
    if bytes.len() as u64 > MAX_ENCODED_BYTES {
        return Err(format!("encoded audio exceeds {MAX_ENCODED_BYTES} bytes"));
    }
    Ok(bytes)
}

fn push_block(out: &mut Vec<[i16; 2]>, pcm: &[f32], channels: usize) -> Result<(), String> {
    let ch = channels.max(1);
    let incoming = pcm.len() / ch;
    if out.len().saturating_add(incoming) > MAX_LOCAL_TRACK_FRAMES {
        return Err("audio clip exceeds the decode budget".into());
    }
    push_interleaved(out, pcm, ch);
    Ok(())
}

fn reserve_stereo() -> Result<Vec<[i16; 2]>, String> {
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(MAX_LOCAL_TRACK_FRAMES)
        .map_err(|e| format!("pcm reservation failed: {e}"))?;
    Ok(frames)
}

fn decode_mp3(bytes: &[u8], aborted: &dyn Fn() -> bool) -> Result<TrackPcm, String> {
    let mut decoder = Mp3Decoder::with_limits(bytes, limits()).map_err(|e| e.to_string())?;
    let mut frames = reserve_stereo()?;
    let mut rate = 0u32;
    let mut channels = 0u16;
    loop {
        if aborted() {
            return Err(aborted_err());
        }
        let Some(frame) = decoder.next_frame().map_err(|e| e.to_string())? else {
            break;
        };
        if channels != 0 && (frame.rate != rate || frame.channels != channels) {
            return Err(AudioError::Unsupported("format changes mid-stream").to_string());
        }
        if frame.channels == 0 {
            return Err(AudioError::Empty.to_string());
        }
        rate = frame.rate;
        channels = frame.channels;
        push_block(&mut frames, frame.pcm, channels as usize)?;
    }
    // Front trim already happened inside `next_frame`. The tail gap is the
    // same `trim()` `decode_all_limited` applies after the whole stream,
    // counted in frames once each input frame is one stereo frame.
    let (_, skip_back) = decoder.trim();
    if skip_back > 0 {
        let drop = (skip_back as usize).min(frames.len());
        frames.truncate(frames.len() - drop);
    }
    if frames.is_empty() || channels == 0 {
        return Err(AudioError::Empty.to_string());
    }
    Ok(TrackPcm { frames, sample_rate: rate })
}

fn decode_vorbis(bytes: &[u8], aborted: &dyn Fn() -> bool) -> Result<TrackPcm, String> {
    let mut decoder = VorbisDecoder::with_limits(bytes, limits()).map_err(|e| e.to_string())?;
    let rate = decoder.rate();
    let channels = decoder.channels();
    if channels == 0 {
        return Err(AudioError::Empty.to_string());
    }
    if let Some(total) = decoder.total_frames() {
        if total > MAX_LOCAL_TRACK_FRAMES as u64 {
            return Err(AudioError::TooLarge("vorbis frame count").to_string());
        }
    }
    let mut frames = reserve_stereo()?;
    loop {
        if aborted() {
            return Err(aborted_err());
        }
        let Some(block) = decoder.next_block().map_err(|e| e.to_string())? else {
            break;
        };
        push_block(&mut frames, block, channels as usize)?;
    }
    if frames.is_empty() {
        return Err(AudioError::Empty.to_string());
    }
    Ok(TrackPcm { frames, sample_rate: rate })
}

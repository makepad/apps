//! Exact PCM sidecars: canonical WAV, 16-bit stereo, 44-byte header.
//!
//! One sidecar per segment holds exactly the segment's frames at the
//! timeline rate, so a read seeks by arithmetic and never through a codec:
//! no priming, no padding, no trim heuristics.

use crate::index::{verify_file, write_atomic, FileName, IndexError, IndexStore, SidecarRef};
use crate::limits::*;
use makepad_asset_data::BlobId;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const WAV_HEADER_BYTES: u64 = 44;
/// Frames one [`SidecarFile::read`] reads at most: the size of its fixed
/// byte scratch (64 KiB), held for as long as the file is open.
pub const SIDECAR_READ_FRAMES: usize = MAX_PAGE_FRAMES;

fn header(rate: u32, frames: u64) -> [u8; 44] {
    let data = (frames * 4) as u32;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&2u16.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * 4).to_le_bytes());
    h[32..34].copy_from_slice(&4u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

/// Write one segment's exact frames as an immutable sidecar in `store`.
/// Runs on the recorder's worker: it allocates the file image once.
pub fn write_sidecar(store: &IndexStore, name: FileName, rate: u32, frames: &[[i16; 2]]) -> Result<SidecarRef, IndexError> {
    if rate == 0 || rate > MAX_SAMPLE_RATE {
        return Err(IndexError::OverLimit("sample rate"));
    }
    if frames.is_empty() || frames.len() as u64 > MAX_SEGMENT_FRAMES {
        return Err(IndexError::OverLimit("segment frames"));
    }
    let mut bytes = Vec::with_capacity(44 + frames.len() * 4);
    bytes.extend_from_slice(&header(rate, frames.len() as u64));
    for frame in frames {
        bytes.extend_from_slice(&frame[0].to_le_bytes());
        bytes.extend_from_slice(&frame[1].to_le_bytes());
    }
    if !name.as_str().ends_with(".wav") {
        return Err(IndexError::BadFileName);
    }
    let digest = BlobId::hash_of(&bytes);
    // No-clobber: a different file already under this name is an error.
    write_atomic(&store.file_path(&name), &bytes)?;
    Ok(SidecarRef { file: name, digest, bytes: bytes.len() as u64 })
}

/// An open sidecar, header checked once.
pub struct SidecarFile {
    file: std::fs::File,
    frames: u64,
    scratch: Vec<u8>,
}

impl SidecarFile {
    /// Open and verify: exact length, the digest the index names (hashed
    /// once, in bounded blocks) and the canonical header.
    pub fn open(path: &Path, rate: u32, expected: &SidecarRef, frames: u64) -> Result<Self, IndexError> {
        if frames == 0 || frames > MAX_SEGMENT_FRAMES || rate == 0 || rate > MAX_SAMPLE_RATE {
            return Err(IndexError::OverLimit("sidecar format"));
        }
        verify_file(path, expected.bytes, &expected.digest)?;
        let mut file = std::fs::File::open(path).map_err(|e| IndexError::Io(e.to_string()))?;
        let len = file.metadata().map_err(|e| IndexError::Io(e.to_string()))?.len();
        if len != expected.bytes || len != WAV_HEADER_BYTES + frames * 4 {
            return Err(IndexError::RefMismatch("sidecar length"));
        }
        let mut head = [0u8; 44];
        file.read_exact(&mut head).map_err(|e| IndexError::Io(e.to_string()))?;
        if head != header(rate, frames) {
            return Err(IndexError::Malformed("sidecar header"));
        }
        // The byte scratch is fixed at one read's worth and never grows.
        Ok(Self { file, frames, scratch: Vec::with_capacity(SIDECAR_READ_FRAMES * 4) })
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Append frames `offset..offset + count` of this sidecar to `out`.
    /// At most [`SIDECAR_READ_FRAMES`] frames per call.
    pub fn read(&mut self, offset: u64, count: usize, out: &mut Vec<[i16; 2]>) -> Result<(), IndexError> {
        if count > SIDECAR_READ_FRAMES {
            return Err(IndexError::OverLimit("sidecar read frames"));
        }
        if offset.checked_add(count as u64).map_or(true, |end| end > self.frames) {
            return Err(IndexError::OverLimit("sidecar read past end"));
        }
        self.file
            .seek(SeekFrom::Start(WAV_HEADER_BYTES + offset * 4))
            .map_err(|e| IndexError::Io(e.to_string()))?;
        self.scratch.resize(count * 4, 0);
        self.file.read_exact(&mut self.scratch).map_err(|e| IndexError::Io(e.to_string()))?;
        out.extend(self.scratch.chunks_exact(4).map(|b| [i16::from_le_bytes([b[0], b[1]]), i16::from_le_bytes([b[2], b[3]])]));
        Ok(())
    }
}

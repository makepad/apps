//! The live run's logic spill (`TraceStore::spill_logic`, `TraceStore::load_logic`): a logic
//! trace's sealed chunks past [`LOGIC_KEEP`] entries in memory leave it for a file of the session,
//! written once by the producer (the live worker) and read back, on the worker too, when a view
//! zooms into them. Every store keeps their summaries (`LogicTrace`'s spans: first and last
//! times, longest gap, levels, newest edges), so the overview, busy runs and triggers draw without
//! them. The file goes in the system's temporary folder at the first spill, anonymous as a tape's
//! (`recording::anonymous`: gone when the run lets go of it, and after a crash too); one that
//! cannot be made or written leaves the chunks in memory (logged once).

use crate::trace::{LogicEdge, SignalId, LOGIC_CHUNK, LOGIC_KEEP};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// One chunk in the file: its times (f64, little-endian), then its levels, a bit each.
const CHUNK_BYTES: usize = LOGIC_CHUNK * 8 + LOGIC_CHUNK / 8;

/// Spill files this process made, for their names.
static FILES: AtomicU64 = AtomicU64::new(0);

/// A producer's spilled logic chunks, by signal and first time, and the file that holds them.
#[derive(Default)]
pub struct LogicSpill {
    file: Option<(File, PathBuf)>,
    end: u64,
    /// The file could not be made or written: chunks stay in memory.
    failed: bool,
    places: HashMap<(SignalId, u64), u64>,
    /// The spilled chunks in memory again because a view asked for them, by signal and first time.
    pub back: Vec<(SignalId, f64)>,
    buf: Vec<u8>,
}

impl LogicSpill {
    pub fn new() -> LogicSpill {
        LogicSpill::default()
    }

    /// Whether chunks can spill: no write has failed.
    pub fn works(&self) -> bool {
        !self.failed
    }

    /// Bytes written to the file.
    pub fn bytes(&self) -> u64 {
        self.end
    }

    /// The file, once the first chunk spilled.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.file.as_ref().map(|(_, path)| path.as_path())
    }

    /// Whether signal `id`'s chunk starting at `first` is in the file.
    pub fn holds(&self, id: SignalId, first: f64) -> bool {
        self.places.contains_key(&(id, first.to_bits()))
    }

    /// Writes signal `id`'s sealed `chunk` (once: a chunk in the file already is not written
    /// again). False when the file cannot be made or written; logged once, and no later chunk is
    /// tried. O(chunk).
    pub fn write(&mut self, id: SignalId, chunk: &[LogicEdge]) -> bool {
        if self.failed || chunk.len() != LOGIC_CHUNK {
            return false;
        }
        let key = (id, chunk[0].t.to_bits());
        if self.places.contains_key(&key) {
            return true;
        }
        self.buf.clear();
        for e in chunk {
            self.buf.extend_from_slice(&e.t.to_le_bytes());
        }
        for bits in chunk.chunks(8) {
            self.buf.push(bits.iter().enumerate().fold(0u8, |b, (k, e)| b | (u8::from(e.high) << k)));
        }
        let (at, buf) = (self.end, std::mem::take(&mut self.buf));
        let written = self.open().and_then(|file| {
            file.seek(SeekFrom::Start(at))?;
            file.write_all(&buf)
        });
        self.buf = buf;
        match written {
            Ok(()) => {
                self.places.insert(key, at);
                self.end += CHUNK_BYTES as u64;
                true
            }
            Err(e) => {
                self.failed = true;
                eprintln!("fab-elec live: the logic spill file cannot be written ({e}); logic traces keep their whole history in memory");
                false
            }
        }
    }

    /// Signal `id`'s chunk starting at `first`, read back from the file; None when it is not
    /// there or the read failed (logged). O(chunk).
    pub fn read(&mut self, id: SignalId, first: f64) -> Option<Arc<[LogicEdge]>> {
        let at = *self.places.get(&(id, first.to_bits()))?;
        let (file, _) = self.file.as_mut()?;
        self.buf.resize(CHUNK_BYTES, 0);
        let read = file.seek(SeekFrom::Start(at)).and_then(|_| file.read_exact(&mut self.buf));
        if let Err(e) = read {
            eprintln!("fab-elec live: a spilled logic chunk cannot be read back ({e}); it draws from its summary");
            return None;
        }
        let (times, levels) = self.buf.split_at(LOGIC_CHUNK * 8);
        Some(times.chunks_exact(8).enumerate()
            .map(|(k, t)| LogicEdge { t: f64::from_le_bytes(t.try_into().expect("eight bytes")), high: levels[k / 8] >> (k % 8) & 1 == 1 })
            .collect())
    }

    /// Forgets the chunks `keep` says no longer exist (by signal and first time), and those back
    /// in memory with them: a rewind dropped or opened them, and one sealed again with the same
    /// first time may hold other entries, so it is written anew. Their bytes stay in the file.
    /// O(chunks held).
    pub fn retain(&mut self, keep: impl Fn(SignalId, f64) -> bool) {
        self.places.retain(|&(id, first), _| keep(id, f64::from_bits(first)));
        self.back.retain(|&(id, first)| keep(id, first));
    }

    /// The file, made at the first spill (and logged).
    fn open(&mut self) -> std::io::Result<&mut File> {
        if self.file.is_none() {
            let path = spill_path()?;
            let file = crate::recording::anonymous(std::fs::OpenOptions::new().read(true).write(true).create_new(true), &path)?;
            eprintln!("fab-elec live: logic traces past {LOGIC_KEEP} entries spill their oldest chunks to a temporary file ({})", path.display());
            self.file = Some((file, path));
        }
        Ok(&mut self.file.as_mut().expect("the file was just made").0)
    }
}

/// A new spill file's path, in the system's temporary folder.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn spill_path() -> std::io::Result<PathBuf> {
    let n = FILES.fetch_add(1, Ordering::Relaxed);
    Ok(std::env::temp_dir().join(format!("fab-elec-logic-{}-{n}.spill", std::process::id())))
}

/// No file system: chunks stay in memory.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn spill_path() -> std::io::Result<PathBuf> {
    let _ = FILES.load(Ordering::Relaxed);
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no file system"))
}

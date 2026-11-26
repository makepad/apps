//! A film pack: named files in one byte string, read in place.
//!
//! ```text
//! "MPFILM01"  u32 count
//! count x { u32 name length, name (UTF-8), u32 offset, u32 length }
//! data (each file at its offset from the start of the pack)
//! ```
//!
//! All integers are little-endian. Names are unique and sorted, so a
//! lookup is a binary search and the same files give the same pack byte
//! for byte. A pack embedded in a program (`include_bytes!`) is read
//! without copying: [`Pack::get`] returns a slice of it.

pub const MAGIC: &[u8; 8] = b"MPFILM01";

/// Reserved names: what the web export puts beside the bundle's files.
pub mod names {
    /// The precompiled draw-shader table (the platform's format).
    pub const SHADERS: &str = "film/shaders.bin";
    /// One wasm module holding every kernel the film compiles.
    pub const KERNELS: &str = "film/kernels.wasm";
    /// Per kernel: its program key and the export indices of its scalar and
    /// four wide entries ([`super::KernelEntry`]).
    pub const KERNEL_KEYS: &str = "film/kernels.keys";
    /// The URL of the song beside the page, when it is not in the pack.
    pub const SONG_URL: &str = "film/song.url";
    /// One wasm module holding every audio program the film compiles
    /// (`run<k>` renders program k).
    pub const AUDIO: &str = "film/audio.wasm";
    /// The program key of each export of [`AUDIO`], in export order
    /// ([`super::write_audio_keys`]).
    pub const AUDIO_KEYS: &str = "film/audio.keys";
}

/// The film analysis's names for the audio module and its keys among the
/// collect manifest's kernel blobs (the web export packs them as
/// [`names::AUDIO`] and [`names::AUDIO_KEYS`]).
pub const AUDIO_BLOB: &str = "audio.wasm";
pub const AUDIO_KEYS_BLOB: &str = "audio.keys";

/// [`names::AUDIO_KEYS`]: the u64 program keys, little-endian.
pub fn write_audio_keys(keys: &[u64]) -> Vec<u8> {
    keys.iter().flat_map(|k| k.to_le_bytes()).collect()
}

pub fn read_audio_keys(bytes: &[u8]) -> Result<Vec<u64>, String> {
    if bytes.len() % 8 != 0 {
        return Err(format!("audio keys: {} bytes is not a whole number of keys", bytes.len()));
    }
    Ok(bytes.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect())
}

/// A kernel of [`names::KERNELS`]: its program key and its entries' export
/// indices in the module (`simd`: None when it has no four wide form).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelEntry {
    pub key: u64,
    pub scalar: u32,
    pub simd: Option<u32>,
}

impl KernelEntry {
    const BYTES: usize = 16;

    pub fn write_all(entries: &[KernelEntry]) -> Vec<u8> {
        let mut out = Vec::with_capacity(entries.len() * Self::BYTES);
        for e in entries {
            out.extend_from_slice(&e.key.to_le_bytes());
            out.extend_from_slice(&e.scalar.to_le_bytes());
            out.extend_from_slice(&e.simd.unwrap_or(u32::MAX).to_le_bytes());
        }
        out
    }

    pub fn read_all(bytes: &[u8]) -> Result<Vec<KernelEntry>, String> {
        if bytes.len() % Self::BYTES != 0 {
            return Err(format!("kernel keys: {} bytes is not a whole number of entries", bytes.len()));
        }
        Ok(bytes
            .chunks_exact(Self::BYTES)
            .map(|c| {
                let simd = u32::from_le_bytes(c[12..16].try_into().unwrap());
                KernelEntry {
                    key: u64::from_le_bytes(c[0..8].try_into().unwrap()),
                    scalar: u32::from_le_bytes(c[8..12].try_into().unwrap()),
                    simd: (simd != u32::MAX).then_some(simd),
                }
            })
            .collect())
    }
}

/// A pack being written: name to contents.
#[derive(Default)]
pub struct PackWriter {
    files: std::collections::BTreeMap<String, Vec<u8>>,
}

impl PackWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file; a name given twice keeps the last contents.
    pub fn add(&mut self, name: &str, bytes: Vec<u8>) {
        self.files.insert(name.to_string(), bytes);
    }

    pub fn contains(&self, name: &str) -> bool {
        self.files.contains_key(name)
    }

    /// Names and sizes, for a size report.
    pub fn sizes(&self) -> impl Iterator<Item = (&str, usize)> {
        self.files.iter().map(|(n, b)| (n.as_str(), b.len()))
    }

    pub fn finish(&self) -> Vec<u8> {
        let header: usize = 8 + 4 + self.files.keys().map(|n| 12 + n.len()).sum::<usize>();
        let total = header + self.files.values().map(|b| b.len()).sum::<usize>();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.files.len() as u32).to_le_bytes());
        let mut at = header;
        for (name, bytes) in &self.files {
            out.extend_from_slice(&(name.len() as u32).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&(at as u32).to_le_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            at += bytes.len();
        }
        for bytes in self.files.values() {
            out.extend_from_slice(bytes);
        }
        out
    }
}

/// A pack read in place.
#[derive(Clone, Copy)]
pub struct Pack<'a> {
    bytes: &'a [u8],
    count: usize,
}

impl<'a> Pack<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Pack<'a>, String> {
        if bytes.len() < 12 || &bytes[..8] != MAGIC {
            return Err("not a film pack".into());
        }
        let count = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let pack = Pack { bytes, count };
        // Walk every entry once: a damaged pack fails here, not at a fetch.
        let mut at = 12;
        for _ in 0..count {
            let (_, _, next) = pack.entry_at(at).ok_or("film pack: damaged index")?;
            at = next;
        }
        Ok(pack)
    }

    /// (name, contents, the next entry's position).
    fn entry_at(&self, at: usize) -> Option<(&'a str, &'a [u8], usize)> {
        let b = self.bytes;
        let len = u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize;
        let name = std::str::from_utf8(b.get(at + 4..at + 4 + len)?).ok()?;
        let p = at + 4 + len;
        let offset = u32::from_le_bytes(b.get(p..p + 4)?.try_into().ok()?) as usize;
        let size = u32::from_le_bytes(b.get(p + 4..p + 8)?.try_into().ok()?) as usize;
        Some((name, b.get(offset..offset.checked_add(size)?)?, p + 8))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'a str, &'a [u8])> + '_ {
        let mut at = 12;
        (0..self.count).filter_map(move |_| {
            let (name, bytes, next) = self.entry_at(at)?;
            at = next;
            Some((name, bytes))
        })
    }

    /// A file's contents (a slice of the pack); None when absent. Names
    /// are sorted, but the index has variable-length entries, so this walks
    /// it: a pack holds tens of files.
    pub fn get(&self, name: &str) -> Option<&'a [u8]> {
        self.iter().find(|(n, _)| *n == name).map(|(_, b)| b)
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pack_reads_back_what_was_written() {
        let mut w = PackWriter::new();
        w.add("b.ttf", vec![1, 2, 3]);
        w.add("a.splash", b"let x = 1".to_vec());
        w.add("empty", Vec::new());
        let bytes = w.finish();
        let pack = Pack::parse(&bytes).unwrap();
        assert_eq!(pack.len(), 3);
        assert_eq!(pack.get("a.splash"), Some(&b"let x = 1"[..]));
        assert_eq!(pack.get("b.ttf"), Some(&[1u8, 2, 3][..]));
        assert_eq!(pack.get("empty"), Some(&[][..]));
        assert_eq!(pack.get("c"), None);
        let names: Vec<&str> = pack.iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["a.splash", "b.ttf", "empty"]);
        // The same files give the same bytes.
        let mut w2 = PackWriter::new();
        w2.add("empty", Vec::new());
        w2.add("a.splash", b"let x = 1".to_vec());
        w2.add("b.ttf", vec![1, 2, 3]);
        assert_eq!(w2.finish(), bytes);
    }

    #[test]
    fn a_damaged_pack_is_refused() {
        let mut w = PackWriter::new();
        w.add("a", vec![0; 10]);
        let bytes = w.finish();
        assert!(Pack::parse(&bytes[..bytes.len() - 1]).is_err());
        assert!(Pack::parse(b"MPFILM00\0\0\0\0").is_err());
    }

    #[test]
    fn kernel_entries_round_trip() {
        let e = [KernelEntry { key: 0x0123_4567_89ab_cdef, scalar: 0, simd: Some(1) }, KernelEntry { key: 7, scalar: 2, simd: None }];
        assert_eq!(KernelEntry::read_all(&KernelEntry::write_all(&e)).unwrap(), e);
        assert!(KernelEntry::read_all(&[0; 15]).is_err());
    }
}

//! A settled slice's G-code text. Nothing reads it while the slice is shown: its hash and the
//! print safety check read it once when it settles, then only Export and Print do, to package
//! it. So once those checks have run the app moves it to a file in its slice cache
//! ([`GcodeText::spill`]) and reads it back for a package ([`GcodeText::text`]), checking it is
//! still the text that settled. A big print's G-code is gigabytes.

use std::collections::HashMap;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

/// The G-code of a settled slice: held, or in a file of a [`SliceCache`].
pub struct GcodeText {
    len: usize,
    hash: u64,
    kept: Mutex<Kept>,
}

#[derive(Clone)]
enum Kept {
    Held(Arc<str>),
    Spilled(Arc<SpillFile>),
}

impl GcodeText {
    pub fn new(text: String) -> Self {
        let hash = hash_of(&text);
        GcodeText { len: text.len(), hash, kept: Mutex::new(Kept::Held(text.into())) }
    }

    /// Its length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The hash of the text (the preview's G-code key): the file it moves to is named by it.
    pub fn hash(&self) -> u64 {
        self.hash
    }

    /// Whether it is in a file now, not held.
    pub fn spilled(&self) -> bool {
        matches!(*self.kept.lock().unwrap(), Kept::Spilled(_))
    }

    /// The text: the held one, else read back from its file, refused unless it is the text that
    /// settled (its length and hash).
    pub fn text(&self) -> Result<Arc<str>, String> {
        let kept = self.kept.lock().unwrap().clone();
        match kept {
            Kept::Held(text) => Ok(text),
            Kept::Spilled(file) => {
                let mut bytes = Vec::with_capacity(self.len);
                File::open(&file.path).and_then(|mut f| f.read_to_end(&mut bytes)).map_err(|e| format!("the slice's G-code could not be read back from {}: {e}", file.path.display()))?;
                let text = String::from_utf8(bytes).map_err(|_| format!("the slice's G-code in {} is no longer text", file.path.display()))?;
                if text.len() != self.len || hash_of(&text) != self.hash {
                    return Err(format!("the slice's G-code in {} changed since it settled: slice the plate again", file.path.display()));
                }
                Ok(text.into())
            }
        }
    }

    /// Moves the text to a file of `cache` and lets go of it (readers that still hold it keep
    /// theirs). A slice of the same text already there shares its file. On a worker: a big
    /// print's text takes seconds to write. When the write fails (a full disk) the text stays
    /// held, and the error says why.
    pub fn spill(&self, cache: &SliceCache) -> Result<(), String> {
        let Kept::Held(text) = self.kept.lock().unwrap().clone() else { return Ok(()) };
        let file = cache.file_of(self.hash, &text)?;
        *self.kept.lock().unwrap() = Kept::Spilled(file);
        Ok(())
    }
}

/// The same text, byte for byte (one that can't be read back equals none).
impl PartialEq for GcodeText {
    fn eq(&self, other: &GcodeText) -> bool {
        self.len == other.len && self.hash == other.hash && self.text().is_ok_and(|a| other.text().is_ok_and(|b| a == b))
    }
}

impl PartialEq<String> for GcodeText {
    fn eq(&self, other: &String) -> bool {
        self.len == other.len() && self.text().is_ok_and(|text| *text == **other)
    }
}

impl PartialEq<GcodeText> for String {
    fn eq(&self, other: &GcodeText) -> bool {
        other == self
    }
}

impl std::fmt::Debug for GcodeText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcodeText").field("len", &self.len).field("hash", &format_args!("{:016x}", self.hash)).field("spilled", &self.spilled()).finish()
    }
}

fn hash_of(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// One spilled text's file, removed when the last slice of that text goes.
struct SpillFile {
    path: PathBuf,
}

impl Drop for SpillFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// This process's folder of spilled G-code under the app's slice cache: `<root>/<pid>/`, its
/// `lock` held while the process runs. Opening it removes the folders of processes that ended
/// (their `lock` is free), whatever they left behind.
pub struct SliceCache {
    dir: PathBuf,
    _lock: File,
    /// Each text's file by its hash, and the files written so far.
    files: Mutex<(HashMap<u64, Weak<SpillFile>>, u64)>,
}

impl SliceCache {
    pub fn open(root: &Path) -> Result<SliceCache, String> {
        let swept = sweep(root);
        let dir = root.join(std::process::id().to_string());
        std::fs::create_dir_all(&dir).map_err(|e| format!("the slice cache {} could not be made: {e}", dir.display()))?;
        let lock = File::create(dir.join("lock")).map_err(|e| format!("the slice cache's lock in {} could not be made: {e}", dir.display()))?;
        lock.try_lock().map_err(|e| format!("the slice cache {} is held by another process: {e}", dir.display()))?;
        if swept > 0 {
            eprintln!("[fab] the slice cache {} removed what {swept} ended process(es) left", root.display());
        }
        Ok(SliceCache { dir, _lock: lock, files: Mutex::new((HashMap::new(), 0)) })
    }

    /// The file holding `text` (hash `hash`): this process's one already, else written now. Each
    /// write gets a file of its own (the hash, then a count): the one a slice that just went
    /// still removes is never written again.
    fn file_of(&self, hash: u64, text: &str) -> Result<Arc<SpillFile>, String> {
        // Spills are one a settle, on workers: one at a time, never two files of one text.
        let mut files = self.files.lock().unwrap();
        files.0.retain(|_, f| f.strong_count() > 0);
        if let Some(file) = files.0.get(&hash).and_then(Weak::upgrade) {
            return Ok(file);
        }
        files.1 += 1;
        let path = self.dir.join(format!("{hash:016x}-{}.gcode", files.1));
        let written = File::create(&path).and_then(|mut f| f.write_all(text.as_bytes()));
        // Made before the write's result is read: a partial file goes with it.
        let file = Arc::new(SpillFile { path });
        written.map_err(|e| format!("the slice's G-code could not be written to {}: {e}", file.path.display()))?;
        files.0.insert(hash, Arc::downgrade(&file));
        Ok(file)
    }
}

/// Removes the folders under `root` whose process ended (their `lock` is free), returning how
/// many. A folder with no lock yet is a process starting: it stays.
fn sweep(root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else { return 0 };
    let mut swept = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(lock) = File::open(path.join("lock")) else { continue };
        if lock.try_lock().is_ok() && std::fs::remove_dir_all(&path).is_ok() {
            swept += 1;
        }
    }
    swept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spilled_text_reads_back_once_per_text_and_its_file_goes_with_its_last_slice() {
        let root = std::env::temp_dir().join(format!("fab-slice-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // An ended process's folder, and one still starting (no lock yet).
        std::fs::create_dir_all(root.join("1")).unwrap();
        std::fs::write(root.join("1").join("lock"), b"").unwrap();
        std::fs::write(root.join("1").join("00.gcode"), b"left").unwrap();
        std::fs::create_dir_all(root.join("2")).unwrap();
        let cache = SliceCache::open(&root).unwrap();
        assert!(!root.join("1").exists(), "an ended process's folder is swept");
        assert!(root.join("2").exists(), "a starting process's folder stays");

        let (a, b) = (GcodeText::new("G1 X1\n".repeat(1000)), GcodeText::new("G1 X1\n".repeat(1000)));
        let held = a.text().unwrap();
        a.spill(&cache).unwrap();
        b.spill(&cache).unwrap();
        assert!(a.spilled() && b.spilled());
        let files = || std::fs::read_dir(&cache.dir).unwrap().filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "gcode")).count();
        assert_eq!(files(), 1, "two slices of one text share one file");
        assert_eq!(*a.text().unwrap(), *held, "the text reads back");
        drop(a);
        assert_eq!(*b.text().unwrap(), *held, "the other slice of the text still reads it");
        let path = std::fs::read_dir(&cache.dir).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|x| x == "gcode")).unwrap();
        assert!(path.file_name().unwrap().to_string_lossy().starts_with(&format!("{:016x}-", b.hash())), "the file is named by the text's hash");
        std::fs::write(&path, "G1 X2\n".repeat(1000)).unwrap();
        assert!(b.text().is_err(), "a file that changed is refused");
        drop(b);
        assert_eq!(files(), 0, "the file goes with the last slice of its text");
        drop(cache);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

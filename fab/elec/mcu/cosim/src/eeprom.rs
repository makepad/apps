//! A simulated chip's EEPROM between runs: one raw file per MCU part beside the project's
//! benches, `<circuit stem>.<part>.eeprom`, holding the chip's bytes exactly. It is the chip's
//! memory, not source: a host loads it at run start, the nominal run saves it back, and
//! clearing deletes it. No file is a new chip, which reads 0xFF everywhere (as real parts do).
//!
//! Performance: nothing here runs per frame; each call is one small file operation (about 1 KB).

use std::path::{Path, PathBuf};

/// The file of `part`'s EEPROM in the circuit `circuit` (its file stem), in `dir`. A name that
/// could leave `dir` (a path separator, `..`, or empty) is refused.
pub fn path(dir: &Path, circuit: &str, part: &str) -> Result<PathBuf, String> {
    Ok(dir.join(file_name(circuit, part)?))
}

/// The file name of `part`'s EEPROM in the circuit `circuit`: `<circuit>.<part>.eeprom`. A name
/// that could leave its folder (a path separator, `..`, or empty) is refused.
pub fn file_name(circuit: &str, part: &str) -> Result<String, String> {
    for (what, name) in [("circuit", circuit), ("part", part)] {
        if name.is_empty() || name == ".." || name.contains(['/', '\\']) {
            return Err(format!("EEPROM file: the {what} name `{name}` cannot name a file"));
        }
    }
    Ok(format!("{circuit}.{part}.eeprom"))
}

/// The saved bytes of a chip with `size` bytes of EEPROM; `None` when there is no file (a new
/// chip). A file of another length is an error naming both lengths, never a silent new chip.
pub fn load(path: &Path, size: usize) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() == size => Ok(Some(bytes)),
        Ok(bytes) => Err(format!("{}: {} bytes where the chip has {size} bytes of EEPROM", path.display(), bytes.len())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Saves `bytes` so that a crash mid-write never leaves the file half written: a flushed temp
/// file beside it, renamed over it.
pub fn save(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension("eeprom.tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Back to a new chip: deletes the file. Clearing a chip that has no file is not an error.
pub fn clear(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", path.display())),
        _ => Ok(()),
    }
}

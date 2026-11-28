use crate::table::{at_least, Deny, Table, Tree};
use crate::SyncError;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::str;

fn git(clone: &Path, args: &[&str], paths: &[String]) -> Result<Vec<u8>, SyncError> {
    let out = Command::new("git")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .arg("-C").arg(clone).args(args).args(paths)
        .output().map_err(|e| format!("git {}: {e}", args[0]))?;
    if !out.status.success() {
        return Err(format!("git {}: {}", args[0], String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout)
}

// Only names proven to be dependencies of removed code are probed here.
// No fixture content enters the stage. A directory or symlink cannot stand
// in for the regular file required by include_str! or include_bytes!.
pub fn fixture_blobs(clone: &Path, pin: &str, paths: &[String]) -> Result<BTreeSet<String>, SyncError> {
    let mut blobs = BTreeSet::new();
    if paths.is_empty() { return Ok(blobs); }
    let listing = git(clone, &["ls-tree", "-z", pin, "--"], paths)?;
    for record in listing.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let (meta, path) = str::from_utf8(record).map_err(|e| e.to_string())?
            .split_once('\t').ok_or("S2: malformed ls-tree record")?;
        if !meta.starts_with("100644 blob ") && !meta.starts_with("100755 blob ") {
            return Err(format!("S2: fixture is not a regular file at the pin: {path}"));
        }
        blobs.insert(path.to_owned());
    }
    Ok(blobs)
}

fn checked_entry(path: &str, kind: u8, copy: &[String], deny: &Deny) -> Result<(), SyncError> {
    if path.is_empty() || path.contains(['\\', ':']) || path.bytes().any(|b| b.is_ascii_control())
        || path.split('/').any(|c| matches!(c, "" | "." | ".."))
    {
        return Err(format!("C1: invalid archive path {path:?}"));
    }
    if deny.matches(path) {
        return Err(format!("C2: denied path {path}"));
    }
    if kind == b'2' {
        return Err(format!("C1: symlink {path}"));
    }
    let directory = kind == b'5';
    if !directory && kind != b'0' {
        return Err(format!("C1: unexpected archive entry type {kind} for {path}"));
    }
    let selected = copy.iter().any(|p| {
        if p.ends_with('/') && path.starts_with(p) {
            true
        } else if directory {
            p.strip_prefix(path).is_some_and(|rest| rest.starts_with('/'))
        } else {
            p == path
        }
    });
    if !selected {
        return Err(format!("C1: path outside copy set: {path}"));
    }
    Ok(())
}

// Compare the archive with the pinned tree too: export-ignore can otherwise
// silently omit a file (even a forbidden file) below a selected directory.
fn inventory(listing: &[u8], copy: &[String], deny: &Deny, minimum: usize) -> Result<BTreeSet<String>, SyncError> {
    let mut files = BTreeSet::new();
    for record in listing.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let (meta, path) = str::from_utf8(record).map_err(|e| format!("C1: git path: {e}"))?
            .split_once('\t').ok_or("C1: malformed ls-tree record")?;
        let kind = match meta.split(' ').next() {
            Some("100644" | "100755") => b'0',
            Some("120000") => b'2',
            _ => return Err(format!("C1: unexpected git mode for {path}: {meta}")),
        };
        checked_entry(path, kind, copy, deny)?;
        files.insert(path.to_owned());
    }
    for p in copy {
        let count = if p.ends_with('/') { files.iter().filter(|f| f.starts_with(p)).count() }
            else { usize::from(files.contains(p)) };
        at_least(&format!("C1 {p}"), minimum, count)?;
    }
    Ok(files)
}

fn tar_field(bytes: &[u8]) -> Result<&str, SyncError> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    str::from_utf8(&bytes[..end]).map_err(|e| format!("C1: invalid tar header: {e}"))
}

// Git uses a global pax comment for the commit id, and a per-file path
// record for names that do not fit the ustar header. Refuse other metadata.
fn pax<'a>(mut data: &'a [u8], kind: u8, pin: &str) -> Result<Option<&'a str>, SyncError> {
    let mut path = None;
    while !data.is_empty() {
        let space = data.iter().position(|&b| b == b' ').ok_or("C1: bad pax length")?;
        let len: usize = tar_field(&data[..space])?.parse().map_err(|_| "C1: bad pax length")?;
        let record = data.get(space + 1..len).ok_or("C1: truncated pax record")?;
        let field = str::from_utf8(record).map_err(|e| format!("C1: bad pax record: {e}"))?
            .strip_suffix('\n').ok_or("C1: unterminated pax record")?;
        match field.split_once('=') {
            Some(("comment", id)) if kind == b'g' && id == pin => {}
            Some(("path", name)) if kind == b'x' && path.is_none() => path = Some(name),
            _ => return Err(format!("C1: unexpected pax field {field:?}")),
        }
        data = &data[len..];
    }
    Ok(path)
}

fn unpack(mut tar: &[u8], pin: &str, copy: &[String], deny: &Deny, mut expected: BTreeSet<String>)
    -> Result<Tree, SyncError>
{
    let mut files = Tree::new();
    let mut long_path = None;
    loop {
        let header = tar.get(..512).ok_or("C1: truncated git archive")?;
        if header.iter().all(|b| *b == 0) {
            if tar.len() < 1024 || tar.iter().any(|b| *b != 0) || long_path.is_some() {
                return Err("C1: invalid end of git archive".into());
            }
            break;
        }
        let size = usize::from_str_radix(tar_field(&header[124..136])?.trim(), 8)
            .map_err(|_| "C1: bad tar size")?;
        tar = &tar[512..];
        let data = tar.get(..size).ok_or("C1: truncated archive entry")?;
        tar = tar.get(size.div_ceil(512) * 512..).ok_or("C1: missing archive padding")?;
        let kind = header[156];
        if kind == b'g' || kind == b'x' {
            if long_path.is_some() {
                return Err("C1: stacked extended path headers".into());
            }
            long_path = pax(data, kind, pin)?;
            continue;
        }
        let prefix = tar_field(&header[345..500])?;
        let name = tar_field(&header[..100])?;
        let full = match long_path.take() {
            Some(path) => path.to_owned(),
            None if prefix.is_empty() => name.to_owned(),
            None => format!("{prefix}/{name}"),
        };
        let path = if kind == b'5' { full.strip_suffix('/').unwrap_or(&full) } else { &full };
        checked_entry(path, kind, copy, deny)?;
        if kind == b'5' {
            if !data.is_empty() {
                return Err(format!("C1: directory has file contents: {path}"));
            }
        } else {
            if !expected.remove(path) {
                return Err(format!("C1: extra or duplicate archive entry: {path}"));
            }
            files.insert(full, data.to_vec());
        }
    }
    if let Some(path) = expected.first() {
        return Err(format!("C1: git archive omitted {path}"));
    }
    Ok(files)
}

fn fs_error(path: &Path, error: io::Error) -> SyncError {
    format!("C1: {}: {error}", path.display())
}

fn ensure_directory(path: &Path) -> Result<(), SyncError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path).map_err(|e| fs_error(path, e))?.is_dir() {
                Ok(())
            } else {
                Err(format!("C1: expected a directory at {}", path.display()))
            }
        }
        Err(e) => Err(fs_error(path, e)),
    }
}

fn verify_raw(root: &Path, relative: &Path, tree: &Tree) -> Result<usize, SyncError> {
    let dir = root.join(relative);
    let mut count = 0;
    for entry in fs::read_dir(&dir).map_err(|e| fs_error(&dir, e))? {
        let entry = entry.map_err(|e| fs_error(&dir, e))?;
        let path = relative.join(entry.file_name());
        let kind = entry.file_type().map_err(|e| fs_error(&path, e))?;
        if kind.is_dir() {
            count += verify_raw(root, &path, tree)?;
        } else {
            let name = path.to_str().ok_or("C1: non-UTF-8 raw path")?.replace('\\', "/");
            if !kind.is_file() || !tree.contains_key(&name) {
                return Err(format!("C1: unexpected entry in raw stage: {name}"));
            }
            if fs::read(entry.path()).map_err(|e| fs_error(&path, e))? != tree[&name] {
                return Err(format!("C1: existing raw file differs: {name}"));
            }
            count += 1;
        }
    }
    Ok(count)
}

fn write_raw(pin: &str, tree: &Tree) -> Result<(), SyncError> {
    let target = Path::new("target");
    let stages = target.join("solidsync");
    let dir = stages.join(pin);
    for parent in [target, &stages, &dir] {
        ensure_directory(parent)?;
    }
    let raw = dir.join("raw");
    match fs::symlink_metadata(&raw) {
        Ok(meta) if meta.is_dir() => {
            if verify_raw(&raw, Path::new(""), tree)? != tree.len() {
                return Err("C1: existing raw stage is incomplete".into());
            }
            return Ok(());
        }
        Ok(_) => return Err(format!("C1: expected directory at {}", raw.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(fs_error(&raw, e)),
    }
    let tmp = dir.join(format!(".raw-{}", std::process::id()));
    fs::create_dir(&tmp).map_err(|e| fs_error(&tmp, e))?;
    let write = || -> Result<(), SyncError> {
        for (path, bytes) in tree {
            let dest = tmp.join(path);
            fs::create_dir_all(dest.parent().unwrap()).map_err(|e| fs_error(&dest, e))?;
            let mut file = fs::OpenOptions::new().write(true).create_new(true)
                .open(&dest).map_err(|e| fs_error(&dest, e))?;
            file.write_all(bytes).map_err(|e| fs_error(&dest, e))?;
        }
        fs::rename(&tmp, &raw).map_err(|e| fs_error(&raw, e))
    };
    let result = write();
    if result.is_err() {
        fs::remove_dir_all(&tmp).map_err(|e| fs_error(&tmp, e))?;
    }
    result
}

pub fn resolve(clone: &Path, pin: &str) -> Result<String, SyncError> {
    if !(4..=40).contains(&pin.len()) || !pin.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("expected a hexadecimal commit id for --pin".into());
    }
    let resolved = git(clone, &["rev-parse", "--verify", &format!("{pin}^{{commit}}")], &[])?;
    let resolved = str::from_utf8(&resolved).map_err(|e| e.to_string())?.trim();
    if resolved.len() != 40 || !resolved.bytes().all(|b| b.is_ascii_hexdigit())
        || !resolved.starts_with(&pin.to_ascii_lowercase())
    {
        return Err(format!("expected a commit object for --pin {pin}"));
    }
    if !git(clone, &["log", "-1", "--format=%H", resolved, "--not", "refs/remotes/origin/main", "--"], &[])?.is_empty() {
        return Err(format!("pin {resolved} is outside origin/main"));
    }
    Ok(resolved.into())
}

pub fn metadata(clone: &Path, resolved: &str) -> Result<(String, String), SyncError> {
    let out = git(clone, &["log", "-1", "--first-parent", "--format=%cI%x00%s", resolved, "--"], &[])?;
    let text = str::from_utf8(&out).map_err(|e| e.to_string())?.strip_suffix('\n').ok_or("invalid git log output")?;
    let (date, subject) = text.split_once('\0').ok_or("missing commit metadata")?;
    Ok((date.into(), subject.into()))
}

pub fn stage_at(clone: &Path, resolved: &str, table: &Table, persist: bool) -> Result<Tree, SyncError> {
    let copy = table.copy_set();
    let listing = git(clone, &["ls-tree", "-rz", resolved, "--"], &copy)?;
    let expected = inventory(&listing, &copy, &table.deny, table.expect.c1_each_path_min)?;
    let archive = git(clone, &["archive", "--format=tar", resolved, "--"], &copy)?;
    let tree = unpack(&archive, resolved, &copy, &table.deny, expected)?;
    if persist { write_raw(resolved, &tree)?; }
    Ok(tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_refuses_symlinks_and_brand_paths() {
        let copy = vec!["crate/src/".to_owned()];
        let deny = Deny {
            names: vec!["assets".into(), "LICENSE-BRAND".into()], directories: vec!["crates/look".into()],
            extensions: ["svg", "png", "ico", "icns", "ttf", "otf"].map(String::from).into(),
        };
        for (path, kind, rule) in [
            ("crate/src/link", b'2', "C1"), ("crate/src/assets", b'5', "C2"),
            ("crate/src/crates/look/design", b'5', "C2"), ("crate/src/LICENSE-BRAND", b'0', "C2"),
            ("crate/src/face.otf", b'0', "C2"), ("crate/src/a/../x", b'0', "C1"),
            ("other/file", b'0', "C1"), ("crate/src/device", b'3', "C1"),
        ] {
            let error = checked_entry(path, kind, &copy, &deny).unwrap_err();
            assert!(error.contains(rule) && error.contains(path), "{error}");
        }
        for ext in ["svg", "png", "ico", "icns", "ttf", "otf"] {
            assert!(checked_entry(&format!("crate/src/icon.{ext}"), b'0', &copy, &deny).is_err());
        }
        assert!(checked_entry("crate/src/lib.rs", b'0', &copy, &deny).is_ok());
        assert!(checked_entry("crate", b'5', &copy, &deny).is_ok());
        assert!(inventory(b"120000 blob unused\tcrate/src/link\0", &copy, &deny, 1).unwrap_err().contains("symlink"));
        let expected = inventory(b"100644 blob unused\tcrate/src/lib.rs\0", &copy, &deny, 1).unwrap();
        assert!(unpack(&[0; 1024], "", &copy, &deny, expected).unwrap_err().contains("omitted crate/src/lib.rs"));
    }
}

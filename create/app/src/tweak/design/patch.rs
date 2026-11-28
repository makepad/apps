use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Format a patch for an existing workspace file with a root-relative header.
pub(crate) fn unified_patch(root: &Path, file: &Path, original: &str, working: &str) -> Result<String, String> {
    let root = root.canonicalize().map_err(|e| format!("open workspace root: {e}"))?;
    let file = root.join(file).canonicalize().map_err(|e| format!("open design source: {e}"))?;
    let relative = file.strip_prefix(&root).map_err(|_| "design source is outside the workspace")?;
    if !file.is_file() {
        return Err("design source must be a file".into());
    }
    let parts = relative.iter().map(|part| part.to_str()).collect::<Option<Vec<_>>>()
        .ok_or("design source path is not UTF-8")?;
    Ok(makepad_git::diff::unified_diff(original, working, Some(&parts.join("/")), 3))
}

pub(crate) fn sanitize_stem(stem: &str) -> String {
    let stem: String = stem.chars().take(64).map(|c| {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }
    }).collect();
    if stem.is_empty() { "design".into() } else { stem }
}

/// Atomically replace a named patch under the workspace's local/design directory.
pub(crate) fn write_patch(root: &Path, stem: &str, patch: &str) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| format!("open workspace root: {e}"))?;
    let mut dir = root.clone();
    for component in ["local", "design"] {
        dir.push(component);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("create patch directory: {e}")),
        }
        dir = dir.canonicalize().map_err(|e| format!("open patch directory: {e}"))?;
        if !dir.starts_with(&root) {
            return Err("patch directory is outside the workspace".into());
        }
    }
    let stem = sanitize_stem(stem);
    let path = dir.join(format!("{stem}.patch"));
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_symlink() => return Err("patch target is a symbolic link".into()),
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(format!("inspect patch target: {e}")),
    }

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);
    let (temp, mut file) = loop {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(".patch-{}-{serial}.tmp", std::process::id()));
        match fs::File::create_new(&temp) {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create temporary patch: {e}")),
        }
    };
    let written = file.write_all(patch.as_bytes());
    drop(file);
    if let Err(e) = written.and_then(|()| fs::rename(&temp, &path)) {
        let _ = fs::remove_file(temp);
        return Err(format!("save patch: {e}"));
    }
    Ok(path)
}

/// Page by byte offset. A positive limit always fits at least one whole character.
pub(crate) fn page(text: &str, offset: u32, limit: u32) -> (&str, Option<u32>) {
    let start = text.ceil_char_boundary(offset as usize);
    if limit == 0 || start == text.len() {
        return ("", None);
    }
    let mut end = text.floor_char_boundary(start.saturating_add(limit as usize));
    if end == start {
        end = text.ceil_char_boundary(start + 1);
    }
    let next = if end < text.len() { u32::try_from(end).ok() } else { None };
    (&text[start..end], next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_writes_sanitized_stem_and_pages_on_utf8_boundaries() {
        let fixture = std::env::temp_dir().join(format!("makepad-town-patch-test-{}", std::process::id()));
        fs::create_dir(&fixture).unwrap();
        let root = fixture.join("workspace");
        fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src/ui.rs");
        fs::write(&file, "old\n").unwrap();
        let patch = unified_patch(&root, &file, "old\n", "é🛠\n").unwrap();
        assert_eq!(patch, "--- a/src/ui.rs\n+++ b/src/ui.rs\n@@ -1,1 +1,1 @@\n-old\n+é🛠\n");
        let outside = fixture.join("outside.rs");
        fs::write(&outside, "outside").unwrap();
        assert!(unified_patch(&root, &root.join("../outside.rs"), "", "x").is_err());

        let dir = root.canonicalize().unwrap().join("local/design");
        assert_eq!(sanitize_stem("../a/é"), "___a__");
        assert_eq!(sanitize_stem(""), "design");
        assert_eq!(sanitize_stem(&"x".repeat(65)), "x".repeat(64));
        let path = write_patch(&root, "../a/é", &patch).unwrap();
        assert_eq!(path, dir.join("___a__.patch"));
        assert_eq!(fs::read_to_string(&path).unwrap(), patch);
        assert_eq!(write_patch(&root, "../a/é", "updated").unwrap(), path);
        assert_eq!(fs::read_to_string(path).unwrap(), "updated");
        assert_eq!(write_patch(&root, "", "").unwrap(), dir.join("design.patch"));
        assert_eq!(write_patch(&root, &"x".repeat(65), "").unwrap(), dir.join(format!("{}.patch", "x".repeat(64))));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&outside, dir.join("link.patch")).unwrap();
            assert!(write_patch(&root, "link", "changed").is_err());
            assert_eq!(fs::read_to_string(&outside).unwrap(), "outside");
            let linked_root = fixture.join("linked");
            fs::create_dir(&linked_root).unwrap();
            symlink(&fixture, linked_root.join("local")).unwrap();
            assert!(write_patch(&linked_root, "escape", "").is_err());
            assert!(!fixture.join("design").exists());
            let dangling_root = fixture.join("dangling");
            fs::create_dir(&dangling_root).unwrap();
            symlink(fixture.join("missing-outside"), dangling_root.join("local")).unwrap();
            assert!(write_patch(&dangling_root, "escape", "").is_err());
            assert!(!fixture.join("missing-outside").exists());
        }

        let mut joined = String::new();
        let mut offset = 0;
        loop {
            let (part, next) = page(&patch, offset, 1);
            assert_eq!(part.chars().count(), 1);
            joined.push_str(part);
            let Some(next) = next else { break };
            assert_eq!(next as usize, joined.len());
            assert!(next > offset);
            offset = next;
        }
        assert_eq!(joined, patch);
        assert_eq!(page("a🛠é", 1, 1), ("🛠", Some(5)));
        assert_eq!(page("a🛠é", 2, 4), ("é", None));
        assert_eq!(page("a🛠é", 0, 4), ("a", Some(1)));
        assert_eq!(page(&patch, 0, 0), ("", None));
        assert_eq!(page(&patch, u32::MAX, u32::MAX), ("", None));

        fs::remove_dir_all(std::env::temp_dir().join(format!("makepad-town-patch-test-{}", std::process::id()))).unwrap();
    }
}

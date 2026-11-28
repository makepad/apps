use crate::table::{Outcome, Table, Tree};
use std::collections::BTreeMap;
use std::path::Path;

pub type Changes = BTreeMap<String, Vec<String>>;

pub fn record(changes: &mut Changes, path: &str, id: &str) {
    let ids = changes.entry(path.to_owned()).or_default();
    if !ids.iter().any(|old| old == id) { ids.push(id.to_owned()); }
}

// Re-key both the raw and ruled trees the same way. The root legal files
// are real archived blobs, unlike the upstream crate-local symlinks.
pub fn project(tree: &Tree, table: &Table) -> Result<Tree, String> {
    let mut files = Tree::new();
    let root = Path::new(&table.workspace_manifest).parent().unwrap();
    for spec in &table.crates {
        let prefix = format!("{}/", spec.upstream);
        for (path, bytes) in tree.range(prefix.clone()..).take_while(|(p, _)| p.starts_with(&prefix)) {
            files.insert(format!("{}/{}", spec.dest, &path[prefix.len()..]), bytes.clone());
        }
        for name in ["LICENSE", "NOTICE", "ATTRIBUTION.md"] {
            let path = root.join(name);
            let bytes = tree.get(path.to_str().unwrap()).ok_or_else(|| format!("missing staged {}", path.display()))?;
            if files.insert(format!("{}/{name}", spec.dest), bytes.clone()).is_some() {
                return Err(format!("{}: duplicate staged {name}", spec.dest));
            }
        }
    }
    Ok(files)
}

pub fn rule_changes(table: &Table, outcomes: &[Outcome]) -> Changes {
    let mut changes = Changes::new();
    for spec in &table.crates {
        let prefix = format!("{}/", spec.upstream);
        for outcome in outcomes.iter().filter(|o| o.crate_ == spec.upstream) {
            for path in &outcome.files {
                if let Some(rel) = path.strip_prefix(&prefix) {
                    record(&mut changes, &format!("{}/{rel}", spec.dest), outcome.rule);
                }
            }
        }
        record(&mut changes, &format!("{}/Cargo.toml", spec.dest), "M1");
    }
    changes
}

fn append_section(bytes: &mut Vec<u8>, section: &str) -> Result<(), String> {
    let heading = section.lines().find(|s| s.starts_with("# ") || s.starts_with("## "))
        .ok_or("A1: missing attribution section heading")?;
    let source = std::str::from_utf8(bytes).map_err(|e| format!("A1: {e}"))?;
    if source.lines().any(|line| line == heading) {
        return Err(format!("A1: upstream already contains {heading}"));
    }
    if !bytes.is_empty() && !bytes.ends_with(b"\n") { bytes.push(b'\n'); }
    if !bytes.is_empty() && !bytes.ends_with(b"\n\n") { bytes.push(b'\n'); }
    bytes.extend_from_slice(section.as_bytes());
    Ok(())
}

pub fn attribution(files: &mut Tree, table: &Table, section: &str, changes: &mut Changes) -> Result<usize, String> {
    let mut count = 0;
    for spec in &table.crates {
        let path = format!("{}/ATTRIBUTION.md", spec.dest);
        let bytes = files.get_mut(&path).ok_or_else(|| format!("A1 {path}: expected one file, found zero"))?;
        append_section(bytes, section).map_err(|e| format!("{path}: {e}"))?;
        record(changes, &path, "A1");
        count += 1;
    }
    Ok(count)
}

const NOTICE: &str = "Modified for Makepad by fab/brep/sync/tool (fab/brep/SYNC.md): ";

pub fn apply(raw: &Tree, files: &mut Tree, changes: &Changes) -> Result<usize, String> {
    let mut count = 0;
    for (path, bytes) in files {
        if raw.get(path) == Some(bytes) { continue; }
        let (prefix, suffix) = match Path::new(path).extension().and_then(|e| e.to_str()) {
            Some("rs") => ("// ", "."),
            Some("toml") => ("# ", "."),
            Some("md") => ("<!-- ", ". -->"),
            _ => return Err(format!("N1: cannot modify {path}")),
        };
        let ids = changes.get(path).filter(|ids| !ids.is_empty())
            .ok_or_else(|| format!("N1: unexplained change to {path}"))?;
        let source = std::str::from_utf8(bytes).map_err(|e| format!("N1 {path}: {e}"))?;
        if source.lines().any(|l| l.strip_prefix(prefix).is_some_and(|s| s.starts_with(NOTICE))) {
            return Err(format!("N1: existing modification notice in {path}"));
        }
        if !bytes.is_empty() && !bytes.ends_with(b"\n") { bytes.push(b'\n'); }
        bytes.extend_from_slice(format!("{prefix}{NOTICE}{}{suffix}\n", ids.join(", ")).as_bytes());
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_go_on_the_last_line_of_changed_files_only() {
        let changed = [("lib.rs", "// ", "."), ("Cargo.toml", "# ", "."), ("about.md", "<!-- ", ". -->")];
        let mut raw = Tree::from([("same.rs".into(), b"same".to_vec()), ("deleted.rs".into(), vec![])]);
        let mut files = raw.clone();
        files.remove("deleted.rs");
        let mut changes = Changes::new();
        for (path, _, _) in changed {
            raw.insert(path.into(), b"original".to_vec());
            files.insert(path.into(), b"one\ntwo".to_vec());
            record(&mut changes, path, "S1");
            record(&mut changes, path, "patch 0001");
        }
        assert_eq!(apply(&raw, &mut files, &changes).unwrap(), 3);
        for (path, prefix, suffix) in changed {
            assert_eq!(files[path], format!("one\ntwo\n{prefix}{NOTICE}S1, patch 0001{suffix}\n").as_bytes());
        }
        assert_eq!(files["same.rs"], b"same");
    }

    #[test]
    fn attribution_section_is_appended_once() {
        let mut bytes = b"# Original\n".to_vec();
        let section = "## References\n\nA reference.\n";
        append_section(&mut bytes, section).unwrap();
        assert_eq!(bytes, format!("# Original\n\n{section}").as_bytes());
        let before = bytes.clone();
        assert!(append_section(&mut bytes, section).unwrap_err().contains("already contains ## References"));
        assert_eq!(bytes, before);
    }
}

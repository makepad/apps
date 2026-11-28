use crate::files::{self, Scratch};
use crate::table::{CrateSpec, Tree};
use makepad_core_util::sha256::sha256_hex;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const MANIFEST: &str = "sync/manifest.txt";
pub const PIN: &str = "sync/pin.toml";
pub type Index = BTreeMap<String, (String, usize)>;

pub fn index(tree: &Tree) -> Index {
    tree.iter().map(|(p, data)| (p.clone(), (sha256_hex(data), data.len()))).collect()
}

fn rows(index: &Index, prefix: &str) -> String {
    let mut text = String::new();
    for (p, (sha, bytes)) in index { writeln!(text, "{prefix}{sha} {bytes} {p}").unwrap(); }
    text
}

pub fn manifest(tree: &Tree) -> String { rows(&index(tree), "") }

fn parse_row(row: &str, entries: &mut Index) -> Result<(), String> {
    let mut fields = row.splitn(3, ' ');
    let hash = fields.next().unwrap_or("");
    let size = fields.next().ok_or("missing file size")?.parse::<usize>().map_err(|e| e.to_string())?;
    let path = fields.next().ok_or("missing file path")?;
    files::local(path)?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) ||
        entries.insert(path.into(), (hash.into(), size)).is_some() {
        return Err(format!("invalid or duplicate manifest entry: {path}"));
    }
    Ok(())
}

fn differences(expected: &Index, found: &Index) -> Vec<String> {
    expected.keys().chain(found.keys()).collect::<BTreeSet<_>>().into_iter().filter_map(|p|
        match (expected.get(p), found.get(p)) {
            (None, _) => Some(format!("extra {p}")), (_, None) => Some(format!("missing {p}")),
            (a, b) if a != b => Some(format!("changed {p}")), _ => None,
        }).collect()
}

pub fn compare(data: &[u8], tree: &Tree) -> Result<(), String> {
    let text = std::str::from_utf8(data).map_err(|e| e.to_string())?;
    let mut expected = Index::new();
    for line in text.lines() { parse_row(line, &mut expected)?; }
    let diff = differences(&expected, &index(tree));
    if !diff.is_empty() { return Err(diff.join("\n")); }
    if text != rows(&expected, "") { return Err("manifest.txt is not in canonical path order and format".into()); }
    Ok(())
}

pub fn rederive(installed_pin: &str, requested: &str, manifest: &[u8],
    derive: impl FnOnce(&str) -> Result<Tree, String>) -> Result<(), String> {
    if installed_pin != requested {
        derive(installed_pin).and_then(|t| compare(manifest, &t)).map_err(|e|
            format!("re-derive {installed_pin}: {e}\nland the tool change with a re-sync at the installed pin first"))?;
    }
    Ok(())
}

struct Unit { path: String, directory: bool }
fn units(crates: &[CrateSpec], metadata: bool) -> Vec<Unit> {
    let mut result = Vec::new();
    for c in crates {
        for name in ["src", "Cargo.toml", "LICENSE", "NOTICE", "ATTRIBUTION.md", "build.rs"] {
            if name == "build.rs" && !c.copy.iter().any(|p| p == name) { continue; }
            result.push(Unit { path: format!("{}/{name}", c.dest), directory: name == "src" });
        }
    }
    result.push(Unit { path: "sync/patches".into(), directory: true });
    if metadata {
        for name in [MANIFEST, PIN] { result.push(Unit { path: name.into(), directory: false }); }
    }
    result
}

fn collect(root: &Path, unit: &Unit, output: &mut Tree) -> Result<(), String> {
    let path = files::checked(root, &unit.path)?;
    if unit.directory {
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(files::error(&path, e)),
            Ok(m) if m.is_dir() => files::collect(root, &unit.path, output),
            Ok(_) => Err(format!("expected owned directory {}", path.display())),
        }
    } else {
        if let Some(data) = files::read_file(&path)? { output.insert(unit.path.clone(), data); }
        Ok(())
    }
}

pub fn snapshot(dest: &Path, crates: &[CrateSpec], metadata: bool) -> Result<Tree, String> {
    let mut files = Tree::new();
    for unit in units(crates, metadata) { collect(dest, &unit, &mut files)?; }
    Ok(files)
}

pub fn verify(dest: &Path, crates: &[CrateSpec]) -> Result<usize, String> {
    let data = files::read_file(&files::checked(dest, MANIFEST)?)?.ok_or("missing sync/manifest.txt")?;
    let tree = snapshot(dest, crates, false)?;
    compare(&data, &tree)?;
    Ok(tree.len())
}

pub fn checkout() -> Result<PathBuf, String> {
    let root = crate::table::dest_root().canonicalize().map_err(|e| e.to_string())?;
    let root = scanner(&root)?.ancestors().nth(3).ok_or("missing checkout")?.to_owned();
    let cwd = std::env::current_dir().and_then(|p| p.canonicalize()).map_err(|e| e.to_string())?;
    if cwd != root { return Err(format!("run solidsync from the checkout root {}", root.display())); }
    Ok(root)
}

pub fn scanner(dest: &Path) -> Result<PathBuf, String> {
    dest.ancestors().map(|p| p.join("tools/private_check/scan.sh")).find(|p| p.is_file())
        .ok_or_else(|| format!("no tools/private_check/scan.sh above {}", dest.display()))
}

pub fn destination(root: &Path, alternate: Option<&str>) -> Result<PathBuf, String> {
    let Some(arg) = alternate else { return crate::table::dest_root().canonicalize().map_err(|e| e.to_string()); };
    let dest = root.join(arg);
    let target = root.join("target");
    let rel = dest.strip_prefix(&target).map_err(|_| "--dest must be under the checkout's target/")?;
    if rel.as_os_str().is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err("--dest must be a subdirectory of target/ without traversal".into());
    }
    let path = files::checked(&target, rel.to_str().ok_or("non-UTF-8 destination")?)?;
    if let Ok(stage) = rel.strip_prefix("solidsync") {
        let first = stage.components().next().and_then(|c| c.as_os_str().to_str()).unwrap_or("");
        if first.is_empty() || first.starts_with('.') || (first.len() == 40 && first.bytes().all(|b| b.is_ascii_hexdigit())) {
            return Err("--dest overlaps the tool's staging area".into());
        }
    }
    match fs::symlink_metadata(&path) {
        Ok(m) if m.is_dir() => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        _ => return Err(format!("expected destination directory {}", path.display())),
    }
    Ok(path)
}

// The kernel releases this worktree-wide lock if the process is interrupted.
// All pin stages (including patch save) and destination transactions share it.
pub fn lock(root: &Path) -> Result<fs::File, String> {
    let path = files::checked(root, "target/solidsync/.lock")?;
    files::directory(path.parent().unwrap())?;
    let f = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)
        .map_err(|e| files::error(&path, e))?;
    f.lock().map_err(|e| files::error(&path, e))?;
    Ok(f)
}

fn subset(index: &Index, unit: &Unit) -> Index {
    if !unit.directory {
        return index.get(&unit.path).map(|v| (unit.path.clone(), v.clone())).into_iter().collect();
    }
    let prefix = format!("{}/", unit.path);
    index.range(prefix.clone()..).take_while(|(p, _)| p.starts_with(&prefix))
        .map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn move_path(src: &Path, dest: &Path) -> Result<(), String> {
    files::directory(dest.parent().unwrap())?;
    fs::rename(src, dest).map_err(|e| format!("rename {} to {}: {e}", src.display(), dest.display()))
}

fn atomic_file(dir: &Path, name: &str, data: &[u8]) -> Result<(), String> {
    let tmp = Scratch::new(dir, "write")?;
    files::write(&tmp.0, "file", data)?;
    move_path(&tmp.0.join("file"), &dir.join(name))
}

const JOURNAL: &str = "install.journal";

fn begin(dir: &Path, dest: &Path, before: &Index, after: &Tree, summary: &str) -> Result<(), String> {
    if dir.join(JOURNAL).try_exists().map_err(|e| e.to_string())? { return Err("an install journal already exists; resume it first".into()); }
    let tmp = Scratch::new(dir, "install")?;
    let stage = tmp.0.join("stage");
    files::directory(&stage)?;
    let new = index(after);
    // Stage complete replacement units. They can be moved into place on the
    // same filesystem, with their predecessors parked inside the transaction.
    for (path, bytes) in after { files::write(&stage.join("new"), path, bytes)?; }
    files::write(&stage, "summary", summary.as_bytes())?;
    let stage_name = format!("install-{}", tmp.0.file_name().unwrap().to_str().unwrap());
    let permanent = dir.join(&stage_name);
    move_path(&stage, &permanent)?;
    let dest = dest.to_str().ok_or("non-UTF-8 destination")?;
    if dest.contains(['\r', '\n']) { return Err("unsupported newline in destination".into()); }
    let header = format!("{dest}\n{stage_name}\n{}\n", sha256_hex(summary.as_bytes()));
    atomic_file(dir, JOURNAL, format!("{header}{}{}", rows(before, "old "), rows(&new, "new ")).as_bytes())
}

pub fn resume(dir: &Path, dest: &Path, crates: &[CrateSpec]) -> Result<String, String> {
    let journal = dir.join(JOURNAL);
    let data = fs::read_to_string(&journal).map_err(|e| files::error(&journal, e))?;
    let mut lines = data.lines();
    if lines.next() != dest.to_str() { return Err(format!("{} belongs to a different destination", journal.display())); }
    let stage = lines.next().filter(|s| s.starts_with("install-.install-") && !s.contains('/')).ok_or("invalid journal stage")?;
    let stage = files::checked(dir, stage)?;
    let summary = fs::read_to_string(stage.join("summary")).map_err(|e| files::error(&stage, e))?;
    if lines.next() != Some(&sha256_hex(summary.as_bytes())) { return Err("journal summary hash mismatch".into()); }
    let (mut old, mut new) = (Index::new(), Index::new());
    for line in lines {
        if let Some(row) = line.strip_prefix("old ") { parse_row(row, &mut old)?; }
        else if let Some(row) = line.strip_prefix("new ") { parse_row(row, &mut new)?; }
        else { return Err("invalid install journal line".into()); }
    }
    let current = index(&snapshot(dest, crates, true)?);
    let mut pending = Vec::new();
    for unit in units(crates, true) {
        let (before, after, found) = (subset(&old, &unit), subset(&new, &unit), subset(&current, &unit));
        if found == after { continue; }
        let backup = files::checked(&stage.join("old"), &unit.path)?;
        if found != before && !(found.is_empty() && backup.try_exists().map_err(|e| e.to_string())?) {
            return Err(format!("journal: unexpected installed data at {}\n{}", unit.path, differences(&before, &found).join("\n")));
        }
        let mut staged = Tree::new();
        collect(&stage.join("new"), &unit, &mut staged)?;
        if index(&staged) != after { return Err(format!("journal: damaged staged data at {}", unit.path)); }
        pending.push(unit);
    }
    for unit in pending {
        let path = files::checked(dest, &unit.path)?;
        let backup = files::checked(&stage.join("old"), &unit.path)?;
        if path.try_exists().map_err(|e| files::error(&path, e))? {
            if backup.try_exists().map_err(|e| files::error(&backup, e))? { return Err(format!("journal: ambiguous backup of {}", unit.path)); }
            move_path(&path, &backup)?;
        }
        let staged = files::checked(&stage.join("new"), &unit.path)?;
        if staged.try_exists().map_err(|e| files::error(&staged, e))? { move_path(&staged, &path)?; }
    }
    let diff = differences(&new, &index(&snapshot(dest, crates, true)?));
    if !diff.is_empty() { return Err(format!("incomplete install:\n{}", diff.join("\n"))); }
    atomic_file(dir, "summary.txt", summary.as_bytes())?;
    fs::remove_file(&journal).map_err(|e| files::error(&journal, e))?;
    fs::remove_dir_all(&stage).map_err(|e| files::error(&stage, e))?;
    Ok(summary)
}

pub fn interrupted(stages: &Path, dest: &Path) -> Result<Option<PathBuf>, String> {
    let mut found = None;
    for entry in fs::read_dir(stages).map_err(|e| files::error(stages, e))? {
        let entry = entry.map_err(|e| files::error(stages, e))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() != 40 || !name.bytes().all(|b| b.is_ascii_hexdigit()) { continue; }
        let path = files::checked(stages, &format!("{name}/{JOURNAL}"))?;
        if let Some(bytes) = files::read_file(&path)? {
            if bytes.split(|&b| b == b'\n').next() == dest.to_str().map(str::as_bytes) {
                if found.is_some() { return Err("multiple journals for destination".into()); }
                found = Some(entry.path());
            }
        }
    }
    Ok(found)
}

pub fn install(dir: &Path, dest: &Path, crates: &[CrateSpec], before: &Tree, after: &Tree, summary: &str) -> Result<String, String> {
    let units = units(crates, true);
    for path in after.keys() {
        let owners = units.iter().filter(|u| if u.directory {
            path.strip_prefix(&u.path).is_some_and(|rest| rest.starts_with('/'))
        } else { u.path == *path }).count();
        if owners != 1 { return Err(format!("expected one tool-owned path for {path}, found {owners}")); }
    }
    if before == after {
        atomic_file(dir, "summary.txt", summary.as_bytes())?;
        return Ok(summary.into());
    }
    begin(dir, dest, &index(before), after, summary)?;
    resume(dir, dest, crates)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Tree {
        Tree::from([("dest/src/lib.rs".into(), b"lib\n".into()), ("dest/LICENSE".into(), b"terms\n".into()),
            ("sync/patches/0001-change.patch".into(), b"patch\n".into())])
    }
    #[test]
    fn install_is_journaled_and_resumes() {
        let temp = Scratch::new(&std::env::temp_dir(), "solidsync-install").unwrap();
        let (dir, dest) = (temp.0.join("pin"), temp.0.join("dest"));
        let table = crate::modules::tests::sample_rules();
        files::write(&dest, "dest/ci.toml", b"ours").unwrap();
        files::write(&dest, "dest/src/old.rs", b"old").unwrap();
        files::write(&dest, "dest/NOTICE", b"same").unwrap();
        let before = snapshot(&dest, &table.crates, true).unwrap();
        let mut after = fixture();
        after.insert("dest/NOTICE".into(), b"same".into());
        after.insert(MANIFEST.into(), manifest(&after).into_bytes());
        after.insert(PIN.into(), b"pin".into());
        begin(&dir, &dest, &index(&before), &after, "report").unwrap();
        let j = fs::read_to_string(dir.join(JOURNAL)).unwrap();
        let stage = dir.join(j.lines().nth(1).unwrap());
        move_path(&dest.join("dest/src"), &stage.join("old/dest/src")).unwrap();
        fs::write(dest.join("dest/NOTICE"), b"edited").unwrap();
        assert!(resume(&dir, &dest, &table.crates).unwrap_err().contains("dest/NOTICE"));
        fs::write(dest.join("dest/NOTICE"), b"same").unwrap();
        assert_eq!(resume(&dir, &dest, &table.crates).unwrap(), "report");
        assert_eq!(snapshot(&dest, &table.crates, true).unwrap(), after);
        assert_eq!(fs::read(dest.join("dest/ci.toml")).unwrap(), b"ours");
        assert!(!dir.join(JOURNAL).exists());
    }
    #[test]
    fn verify_finds_changed_missing_and_extra_files() {
        let temp = Scratch::new(&std::env::temp_dir(), "solidsync-verify").unwrap();
        let table = crate::modules::tests::sample_rules();
        let tree = fixture();
        for (p, data) in &tree { files::write(&temp.0, p, data).unwrap(); }
        files::write(&temp.0, MANIFEST, manifest(&tree).as_bytes()).unwrap();
        assert_eq!(verify(&temp.0, &table.crates).unwrap(), 3);
        fs::write(temp.0.join("dest/src/lib.rs"), b"hand edit").unwrap();
        fs::remove_file(temp.0.join("sync/patches/0001-change.patch")).unwrap();
        files::write(&temp.0, "dest/src/extra.rs", b"extra").unwrap();
        let err = verify(&temp.0, &table.crates).unwrap_err();
        for p in ["changed dest/src/lib.rs", "extra dest/src/extra.rs", "missing sync/patches/0001-change.patch"] {
            assert!(err.contains(p), "{err}");
        }
    }
    #[test]
    fn a_pin_move_re_derives_the_installed_tree_first() {
        let temp = Scratch::new(&std::env::temp_dir(), "solidsync-pin").unwrap();
        let table = crate::modules::tests::sample_rules();
        let mut tree = fixture();
        let expected = manifest(&tree);
        tree.insert("dest/src/lib.rs".into(), b"tool changed".into());
        let fail = rederive("old", "new", expected.as_bytes(), |pin| { assert_eq!(pin, "old"); Ok(tree.clone()) }).unwrap_err();
        assert!(fail.contains("changed dest/src/lib.rs") && fail.contains("land the tool change with a re-sync at the installed pin first"));
        rederive("old", "old", expected.as_bytes(), |_| panic!("same pin re-derived")).unwrap();
        install(&temp.0.join("stage"), &temp.0.join("dest"), &table.crates, &Tree::new(), &tree, "same pin").unwrap();
        assert_eq!(snapshot(&temp.0.join("dest"), &table.crates, true).unwrap(), tree);
    }
}

use crate::files::{self, Scratch};
use crate::notices::{self, Changes};
use crate::table::{CrateSpec, Tree};
use makepad_core_util::sha256::sha256_hex;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

#[derive(Clone)]
struct Patch {
    number: String,
    name: String,
    pin: String,
    why: String,
    guard: String,
    upstreamable: String,
    files: BTreeMap<String, (String, String)>,
    diff: Vec<u8>,
}

fn number_name(number: &str, name: &str) -> Result<(), String> {
    if number.len() != 4 || !number.bytes().all(|b| b.is_ascii_digit()) || name.is_empty()
        || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    { return Err("expected a four-digit patch number and a lowercase descriptive name".into()); }
    Ok(())
}

fn digest(bytes: Option<&Vec<u8>>) -> String {
    bytes.map_or_else(|| "-".into(), |b| sha256_hex(b))
}

fn commit(pin: &str) -> bool { pin.len() == 40 && pin.bytes().all(|b| b.is_ascii_hexdigit()) }

impl Patch {
    fn filename(&self) -> String { format!("{}-{}.patch", self.number, self.name) }

    fn parse(name: &str, data: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(data).map_err(|e| format!("{name}: {e}"))?;
        let (header, diff) = text.split_once("\n---\n").ok_or("missing patch header separator")?;
        let mut lines = header.lines();
        let mut field = |key: &str| -> Result<String, String> {
            lines.next().and_then(|l| l.strip_prefix(key)).filter(|s| !s.trim().is_empty())
                .map(String::from).ok_or_else(|| format!("{name}: expected {key}"))
        };
        let id = field("Solidsync-Patch: ")?;
        let (number, slug) = id.split_once(' ').ok_or("missing patch name")?;
        number_name(number, slug)?;
        let patch_pin = field("Refreshed-At: ")?;
        let why = field("Why: ")?;
        let guard = field("Guard: ")?;
        let upstreamable = field("Upstreamable: ")?;
        if !commit(&patch_pin) || !matches!(upstreamable.split_once(": "), Some(("yes" | "no", note)) if !note.trim().is_empty()) {
            return Err(format!("{name}: invalid Refreshed-At or Upstreamable"));
        }
        let mut paths = BTreeMap::new();
        for line in lines {
            let (path, hashes) = line.strip_prefix("Files: ").and_then(|l| l.split_once(" base=")).ok_or("expected Files header")?;
            files::local(path)?;
            let (base, result) = hashes.split_once(" result=").ok_or("missing result hash")?;
            if matches!(path, "LICENSE" | "NOTICE" | "ATTRIBUTION.md")
                || result == "-" && matches!(path, "Cargo.toml" | "src/lib.rs" | "build.rs")
            {
                return Err(format!("{name}: patch would change legal text or delete a required crate file: {path}"));
            }
            if [base, result].iter().any(|s| *s != "-" && !(s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))) {
                return Err(format!("{name}: invalid file hash for {path}"));
            }
            if paths.insert(path.into(), (base.into(), result.into())).is_some() {
                return Err(format!("{name}: duplicate file {path}"));
            }
        }
        let p = Self { number: number.into(), name: slug.into(), pin: patch_pin, why, guard, upstreamable,
            files: paths, diff: diff.as_bytes().into() };
        if p.filename() != name || p.files.is_empty() { return Err(format!("{name}: filename/header mismatch or no files")); }
        Ok(p)
    }

    fn encode(&self) -> Vec<u8> {
        let mut s = format!("Solidsync-Patch: {} {}\nRefreshed-At: {}\nWhy: {}\nGuard: {}\nUpstreamable: {}\n",
            self.number, self.name, self.pin, self.why, self.guard, self.upstreamable);
        for (path, (base, result)) in &self.files { s.push_str(&format!("Files: {path} base={base} result={result}\n")); }
        s.push_str("---\n");
        let mut bytes = s.into_bytes();
        bytes.extend_from_slice(&self.diff);
        bytes
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<Output, String> {
    // Git must resolve the stage on its own filesystem before any command
    // (especially init). A virtual process cwd is insufficient for native Git.
    let dir = dir.canonicalize().map_err(|e| files::error(dir, e))?;
    let output = Command::new("git").arg("-C").arg(&dir)
        .env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").env_remove("GIT_INDEX_FILE")
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_ATTR_NOSYSTEM", "1")
        .args(["-c", "core.autocrlf=false", "-c", "core.hooksPath=/dev/null"])
        .env("LC_ALL", "C").args(args).output().map_err(|e| format!("git {}: {e}", args[0]))?;
    if !output.status.success() && String::from_utf8_lossy(&output.stderr).contains("cannot change to") {
        return Err(format!("solidsync must run from a real checkout: git cannot see {} (is it served by a VFS?)", dir.display()));
    }
    Ok(output)
}

fn success(out: Output, action: &str) -> Result<Vec<u8>, String> {
    if out.status.success() { Ok(out.stdout) } else {
        Err(format!("{action}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn owned(spec: &CrateSpec, path: &str) -> bool {
    path.starts_with("src/") || path == "Cargo.toml"
        || path == "build.rs" && spec.copy.iter().any(|f| f == path)
}

fn selection(tree: &Tree, spec: &CrateSpec, p: &Patch) -> Tree {
    p.files.keys().filter_map(|path| tree.get(&format!("{}/{path}", spec.dest))
        .map(|b| (path.to_owned(), b.clone()))).collect()
}

fn matches(tree: &Tree, p: &Patch, reverse: bool) -> bool {
    p.files.iter().all(|(path, (base, result))| {
        digest(tree.get(path)) == *if reverse { result } else { base }
    })
}

fn apply(dir: &Path, p: &Patch, before: &Tree, reverse: bool) -> Result<Tree, String> {
    let scratch = Scratch::new(&dir.join("ruled"), "apply")?;
    let repo = &scratch.0;
    success(git(repo, &["init", "-q", "--template="])?, "git init")?;
    // Keep the input outside the worktree so no diff can address it.
    let input = Scratch::new(dir, "diff")?;
    files::write(&input.0, "patch", &p.diff)?;
    let patchfile = input.0.join("patch").canonicalize().map_err(|e| e.to_string())?;
    let path = patchfile.to_str().ok_or("non-UTF-8 stage path")?;
    for (name, bytes) in before { files::write(repo, name, bytes)?; }
    // Refuse modes and renames which cannot be represented by a Tree of blobs.
    let summary = success(git(repo, &["apply", "--summary", path])?, "git apply --summary")?;
    for line in String::from_utf8_lossy(&summary).lines() {
        if !line.starts_with(" create mode 100644 ") && !line.starts_with(" delete mode 100644 ") {
            return Err(format!("patch {}: unsupported change: {line}", p.number));
        }
    }
    let stat = success(git(repo, &["apply", "--numstat", "-z", path])?, "git apply --numstat")?;
    let mut names = BTreeSet::new();
    for rec in stat.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let name = std::str::from_utf8(rec).map_err(|e| e.to_string())?.splitn(3, '\t').nth(2).ok_or("invalid patch numstat")?;
        if !p.files.contains_key(name) || !names.insert(name.to_owned()) {
            return Err(format!("patch {}: undeclared or repeated file {name}", p.number));
        }
    }
    let mut args = vec!["apply", "--whitespace=nowarn"];
    if reverse { args.push("--reverse"); }
    let mut check = args.clone();
    check.extend(["--check", path]);
    success(git(repo, &check)?, "git apply --check")?;
    args.push(path);
    success(git(repo, &args)?, "git apply")?;
    let mut after = Tree::new();
    for (name, (base, result)) in &p.files {
        let data = files::read_file(&files::checked(repo, name)?)?;
        if data.as_ref() == before.get(name) { return Err(format!("patch {} left {name} unchanged", p.number)); }
        let expected = if reverse { base } else { result };
        if digest(data.as_ref()) != *expected { return Err(format!("patch {}: {} hash mismatch for {name}",
            p.number, if reverse { "base" } else { "result" })); }
        if let Some(bytes) = data { after.insert(name.clone(), bytes); }
    }
    Ok(after)
}

fn diff(dir: &Path, before: &Tree, after: &Tree) -> Result<(BTreeMap<String, (String, String)>, Vec<u8>), String> {
    let scratch = Scratch::new(dir, "diff")?;
    let mut records = BTreeMap::new();
    let mut diff = Vec::new();
    for path in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
        let (old, new) = (before.get(path), after.get(path));
        if old == new { continue; }
        let (a, b) = (format!("a/{path}"), format!("b/{path}"));
        if let Some(bytes) = old { files::write(&scratch.0, &a, bytes)?; }
        if let Some(bytes) = new { files::write(&scratch.0, &b, bytes)?; }
        let output = git(&scratch.0, &["diff", "--no-index", "--no-renames", "--no-ext-diff", "--no-textconv",
            "--no-color", "--no-prefix", "--", if old.is_some() { &a } else { "/dev/null" },
            if new.is_some() { &b } else { "/dev/null" }])?;
        if output.status.code() != Some(1) || !output.stderr.is_empty() {
            return Err(format!("generating diff for {path}: {}", String::from_utf8_lossy(&output.stderr)));
        }
        diff.extend(output.stdout);
        records.insert(path.clone(), (digest(old), digest(new)));
    }
    Ok((records, diff))
}

fn without_notice(mut bytes: Vec<u8>, expected: &str, path: &str) -> Result<Vec<u8>, String> {
    if sha256_hex(&bytes) == expected { return Ok(bytes); }
    let source = std::str::from_utf8(&bytes).map_err(|e| format!("{path}: {e}"))?;
    let line = source.strip_suffix('\n').and_then(|text| text.rsplit('\n').next()).unwrap_or("");
    if ["// ", "# ", "<!-- "].iter().any(|p| line.strip_prefix(p)
        .is_some_and(|s| s.starts_with("Modified for Makepad by fab/brep/sync/tool (fab/brep/SYNC.md): "))) {
        bytes.truncate(bytes.len() - line.len() - 1);
        if sha256_hex(&bytes) == expected { return Ok(bytes); }
        if bytes.ends_with(b"\n") { bytes.pop(); }
        if sha256_hex(&bytes) == expected { return Ok(bytes); }
    }
    Err(format!("installed result hash mismatch: {path}"))
}

fn replace(tree: &mut Tree, spec: &CrateSpec, p: &Patch, after: &Tree) {
    for path in p.files.keys() {
        let key = format!("{}/{path}", spec.dest);
        match after.get(path) {
            Some(bytes) => { tree.insert(key, bytes.clone()); }
            None => { tree.remove(&key); }
        }
    }
}

struct Base { krate: usize, before: Tree, after: Tree }

fn reconstruct(dir: &Path, crates: &[CrateSpec], queue: &[Patch],
    installed: &mut impl FnMut(&str) -> Result<Option<Vec<u8>>, String>) -> Result<Vec<Base>, String> {
    let mut old = Tree::new();
    let mut loaded = BTreeSet::new();
    let mut bases = Vec::new();
    for p in queue.iter().rev() {
        let mut candidates = Vec::new();
        for (i, spec) in crates.iter().enumerate() {
            let mut after = Tree::new();
            let mut valid = true;
            for (name, (_, hash)) in &p.files {
                if !owned(spec, name) { valid = false; break; }
                let path = format!("{}/{name}", spec.dest);
                if loaded.insert(path.clone()) {
                    if let Some(bytes) = installed(&path)? { old.insert(path.clone(), bytes); }
                }
                match old.get(&path) {
                    Some(bytes) => match without_notice(bytes.clone(), hash, &path) {
                        Ok(data) => { after.insert(name.clone(), data); }
                        Err(_) => { valid = false; break; }
                    },
                    None if hash == "-" => (),
                    None => { valid = false; break; }
                }
            }
            if valid { candidates.push((i, after)); }
        }
        if candidates.len() != 1 { return Err(format!("patch {}: installed result hashes must match exactly one crate ({})", p.number, p.files.keys().cloned().collect::<Vec<_>>().join(", "))); }
        let (krate, after) = candidates.pop().unwrap();
        let before = apply(dir, p, &after, true)?;
        replace(&mut old, &crates[krate], p, &before);
        bases.push(Base { krate, before, after });
    }
    bases.reverse();
    Ok(bases)
}

fn refresh(dir: &Path, p: &mut Patch, current: &Tree, base: &Base, pin: &str) -> Result<Tree, String> {
    let mut merged = Tree::new();
    let mut conflicts = Vec::new();
    let scratch = Scratch::new(dir, "merge")?;
    for path in p.files.keys() {
        let (new, before, after) = (current.get(path), base.before.get(path), base.after.get(path));
        let result = if new == before { after.cloned() }
        else {
            for (name, bytes) in [("new", new), ("before", before), ("after", after)] {
                fs::write(scratch.0.join(name), bytes.map_or(&[][..], Vec::as_slice)).map_err(|e| e.to_string())?;
            }
            let out = git(&scratch.0, &["merge-file", "-p", "-L", "new upstream", "-L", "installed base", "-L", "installed patch",
                "new", "before", "after"])?;
            match out.status.code() {
                Some(0) if new.is_some() && before.is_some() && after.is_some() && new != after && new != Some(&out.stdout) => Some(out.stdout),
                Some(0..=127) => {
                    let target = files::checked(&dir.join("conflicts"), path)?;
                    files::directory(target.parent().unwrap())?;
                    let bytes = if out.status.success() {
                        let mut marked = b"<<<<<<< new upstream\n".to_vec();
                        for (body, label) in [(new, &b"=======\n"[..]), (after, &b">>>>>>> installed patch\n"[..])] {
                            if let Some(data) = body { marked.extend_from_slice(data); }
                            if !marked.ends_with(b"\n") { marked.push(b'\n'); }
                            marked.extend_from_slice(label);
                        }
                        marked
                    } else { out.stdout };
                    let tmp = Scratch::new(target.parent().unwrap(), "conflict")?;
                    files::write(&tmp.0, "file", &bytes)?;
                    fs::rename(tmp.0.join("file"), &target).map_err(|e| files::error(&target, e))?;
                    conflicts.push(path.to_owned());
                    None
                }
                _ => return Err(format!("git merge-file {path}: {}", String::from_utf8_lossy(&out.stderr))),
            }
        };
        if let Some(bytes) = result { merged.insert(path.clone(), bytes); }
    }
    if !conflicts.is_empty() { return Err(format!("HAND BACK: patch {} conflicts in {}", p.number, conflicts.join(", "))); }
    (p.files, p.diff) = diff(dir, current, &merged)?;
    p.pin = pin.into();
    apply(dir, p, current, false)
}

pub struct Applied {
    pub patches: Tree,
    pub applied: Vec<String>,
    pub refreshed: Vec<String>,
}

// Queue filenames are relative to sync/patches, source paths to dest_root.
pub fn run(dir: &Path, pin: &str, crates: &[CrateSpec], files: &mut Tree, patches: &Tree, changes: &mut Changes,
    mut installed: impl FnMut(&str) -> Result<Option<Vec<u8>>, String>) -> Result<Applied, String> {
    let mut queue = Vec::new();
    let mut numbers = BTreeSet::new();
    for (name, data) in patches {
        let p = Patch::parse(name, data)?;
        if !numbers.insert(p.number.clone()) { return Err(format!("duplicate patch {}", p.number)); }
        queue.push(p);
    }
    let mut result = Applied { patches: Tree::new(), applied: Vec::new(), refreshed: Vec::new() };
    let mut bases = None;
    for (i, original) in queue.iter().enumerate() {
        let mut p = original.clone();
        let candidates: Vec<_> = crates.iter().enumerate().filter(|(_, c)| p.files.keys().all(|n| owned(c, n))
            && matches(&selection(files, c, &p), &p, false)).map(|(i, _)| i).collect();
        let (spec, after) = if let [idx] = candidates.as_slice() {
            let spec = &crates[*idx];
            (spec, apply(dir, &p, &selection(files, spec, &p), false)?)
        } else {
            if p.pin == pin || !candidates.is_empty() { return Err(format!("patch {}: base hash mismatch or ambiguous crate for {}", p.number, p.filename())); }
            if bases.is_none() { bases = Some(reconstruct(dir, crates, &queue, &mut installed)?); }
            let base = &bases.as_ref().unwrap()[i];
            let spec = &crates[base.krate];
            let current = selection(files, spec, &p);
            let after = refresh(dir, &mut p, &current, base, pin)?;
            result.refreshed.push(p.filename());
            (spec, after)
        };
        replace(files, spec, &p, &after);
        for path in p.files.keys() { notices::record(changes, &format!("{}/{path}", spec.dest), &format!("patch {}", p.number)); }
        result.applied.push(p.filename());
        p.pin = pin.into();
        result.patches.insert(p.filename(), p.encode());
    }
    Ok(result)
}

pub fn load(root: &Path) -> Result<Tree, String> {
    let mut files = Tree::new();
    files::collect(root, "sync/patches", &mut files)?;
    files.into_iter().map(|(path, data)| {
        let name = path.strip_prefix("sync/patches/").ok_or("expected sync/patches directory")?;
        Ok((name.into(), data))
    }).collect()
}

// Publish the pre-patch tree for patch save. An interrupted publication can
// be rebuilt from the pin. The editable work/ is a separate directory.
pub fn stage_ruled(dir: &Path, tree: &Tree) -> Result<(), String> {
    let ruled = dir.join("ruled");
    let fresh = Scratch::new(dir, "ruled")?;
    for (path, bytes) in tree { files::write(&fresh.0, path, bytes)?; }
    success(git(&fresh.0, &["init", "-q", "--template="])?, "git init")?;
    if ruled.exists() {
        let old = Scratch::new(dir, "previous")?;
        fs::rename(&ruled, old.0.join("ruled")).map_err(|e| files::error(&ruled, e))?;
    }
    fs::rename(&fresh.0, &ruled).map_err(|e| files::error(&ruled, e))
}

fn crate_tree(root: &Path, crates: &[CrateSpec]) -> Result<Tree, String> {
    let mut tree = Tree::new();
    for spec in crates { files::collect(root, &spec.dest, &mut tree)?; }
    Ok(tree)
}

pub struct Save<'a> {
    pub number: &'a str,
    pub name: &'a str,
    pub why: Option<String>,
    pub guard: Option<String>,
    pub upstreamable: Option<String>,
}

pub fn save(dir: &Path, dest: &Path, pin: &str, crates: &[CrateSpec], info: Save<'_>,
    installed: impl FnMut(&str) -> Result<Option<Vec<u8>>, String>) -> Result<String, String> {
    number_name(info.number, info.name)?;
    if !commit(pin) { return Err("patch save requires the full staged pin".into()); }
    let filename = format!("{}-{}.patch", info.number, info.name);
    let mut tree = crate_tree(&dir.join("ruled"), crates)?;
    if tree.is_empty() { return Err(format!("run sync --dry-run at {pin} first")); }
    let mut queue = load(dest)?;
    let old = queue.remove(&filename).map(|data| Patch::parse(&filename, &data)).transpose()?;
    let mut previous = Tree::new();
    for (name, data) in queue {
        let p = Patch::parse(&name, &data)?;
        if p.number == info.number { return Err(format!("patch {} already named {name}", info.number)); }
        if p.number.as_str() < info.number { previous.insert(name, data); }
    }
    run(dir, pin, crates, &mut tree, &previous, &mut Changes::new(), installed)?;
    let mut identity = format!("{pin} {filename}\n");
    for (path, bytes) in &tree { identity.push_str(&format!("{} {path}\n", sha256_hex(bytes))); }
    let identity = sha256_hex(identity.as_bytes());
    let work = dir.join("work");
    if !work.exists() {
        let fresh = Scratch::new(dir, "work")?;
        let mut edited = tree;
        if let Some(p) = &old {
            let queue = Tree::from([(p.filename(), p.encode())]);
            run(dir, pin, crates, &mut edited, &queue, &mut Changes::new(), |_| Err("existing patch needs a sync first".into()))?;
        }
        for (path, data) in &edited { files::write(&fresh.0, path, data)?; }
        files::write(&fresh.0, ".solidsync-base", identity.as_bytes())?;
        fs::rename(&fresh.0, &work).map_err(|e| files::error(&work, e))?;
        return Ok(format!("edit {} and repeat patch save with its Why, Guard and Upstreamable fields", work.display()));
    }
    if files::read_file(&work.join(".solidsync-base"))?.as_deref() != Some(identity.as_bytes()) {
        return Err("work/ belongs to a different base or patch; move it aside before preparing another".into());
    }
    let mut edited = Tree::new();
    for entry in fs::read_dir(&work).map_err(|e| files::error(&work, e))? {
        let name = entry.map_err(|e| files::error(&work, e))?.file_name().into_string().map_err(|_| "non-UTF-8 work path")?;
        if name != ".solidsync-base" { files::collect(&work, &name, &mut edited)?; }
    }
    for name in edited.keys() {
        if !crates.iter().any(|c| name.starts_with(&format!("{}/", c.dest))) {
            return Err(format!("edited file outside patchable crates: {name}"));
        }
    }
    let mut changed = Vec::new();
    for spec in crates {
        let prefix = format!("{}/", spec.dest);
        let select = |t: &Tree| t.iter().filter_map(|(path, data)| path.strip_prefix(&prefix)
            .map(|s| (s.into(), data.clone()))).collect::<Tree>();
        let (before, after) = (select(&tree), select(&edited));
        if before != after {
            let (files, diff) = diff(dir, &before, &after)?;
            if let Some(path) = files.keys().find(|path| !owned(spec, path)) {
                return Err(format!("cannot patch {path}"));
            }
            changed.push((before, files, diff));
        }
    }
    if changed.len() != 1 { return Err("a patch must change files in exactly one crate".into()); }
    let (before, records, diff) = changed.pop().unwrap();
    let p = Patch { number: info.number.into(), name: info.name.into(), pin: pin.into(), files: records, diff,
        why: info.why.or_else(|| old.as_ref().map(|p| p.why.clone())).ok_or("supply --why <reason>")?,
        guard: info.guard.or_else(|| old.as_ref().map(|p| p.guard.clone())).ok_or("supply --guard <test>")?,
        upstreamable: info.upstreamable.or_else(|| old.as_ref().map(|p| p.upstreamable.clone())).ok_or("supply --upstreamable 'yes|no: note'")? };
    let data = p.encode();
    let parsed = Patch::parse(&filename, &data)?;
    let selected = before.into_iter().filter(|(name, _)| parsed.files.contains_key(name)).collect();
    apply(dir, &parsed, &selected, false)?;
    let target = files::checked(dest, &format!("sync/patches/{filename}"))?;
    let parent = target.parent().unwrap();
    let tmp = Scratch::new(parent, "save")?;
    files::write(&tmp.0, "patch", &data)?;
    fs::rename(tmp.0.join("patch"), &target).map_err(|e| files::error(&target, e))?;
    Ok(format!("saved {filename}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    const OLD: &str = "1111111111111111111111111111111111111111";
    const NEW: &str = "2222222222222222222222222222222222222222";

    fn scratch() -> Scratch {
        Scratch::new(&std::env::temp_dir(), "solidsync-patch-test").unwrap()
    }

    fn tree(s: &str) -> Tree { Tree::from([("src/lib.rs".into(), s.as_bytes().into())]) }
    fn installed(s: &str) -> Tree { Tree::from([("dest/src/lib.rs".into(), s.as_bytes().into())]) }
    fn patch(dir: &Path, number: &str, before: &str, after: &str) -> Patch {
        let (files, diff) = diff(dir, &tree(before), &tree(after)).unwrap();
        Patch { number: number.into(), name: "change".into(), pin: OLD.into(),
            why: "fixture".into(), guard: "fixture test".into(), upstreamable: "yes: independent".into(), files, diff }
    }

    #[test]
    fn patches_apply_in_order_and_a_skipped_hunk_fails() {
        let dir = scratch();
        let table = crate::modules::tests::sample_rules();
        let first = patch(&dir.0, "0001", "a\n", "b\n");
        let second = patch(&dir.0, "0002", "b\n", "c\n");
        let run_queue = |patches| {
            let mut files = installed("a\n");
            run(&dir.0, OLD, &table.crates, &mut files, &patches, &mut Changes::new(), |_| panic!("unexpected refresh"))?;
            Ok::<_, String>(files)
        };
        assert_eq!(run_queue(Tree::from([(second.filename(), second.encode()), (first.filename(), first.encode())])).unwrap(), installed("c\n"));
        let check = |p: Patch, expected: &str| {
            let err = run_queue(Tree::from([(p.filename(), p.encode())])).unwrap_err();
            assert!(err.contains(expected), "{err}");
        };
        let mut bad = first.clone();
        bad.files.get_mut("src/lib.rs").unwrap().0 = "0".repeat(64);
        check(bad, "base hash");
        let mut bad = first.clone();
        bad.files.get_mut("src/lib.rs").unwrap().1 = "0".repeat(64);
        check(bad, "result hash");
        let mut skipped = first;
        skipped.files.insert("src/skipped.rs".into(), ("-".into(), sha256_hex(b"must exist")));
        check(skipped, "left src/skipped.rs unchanged");

        // patch save must round-trip a hook that adds an entirely new file.
        stage_ruled(&dir.0, &installed("a\n")).unwrap();
        let dest = dir.0.join("install");
        let save_hook = || save(&dir.0, &dest, OLD, &table.crates, Save {
            number: "0001", name: "hook", why: Some("observe changes".into()),
            guard: Some("hook test".into()), upstreamable: Some("yes: observer".into()),
        }, |_| panic!("unexpected refresh"));
        save_hook().unwrap();
        files::write(&dir.0.join("work"), "dest/src/hook.rs", b"fn hook() {}\n").unwrap();
        save_hook().unwrap();
        let result = run_queue(load(&dest).unwrap()).unwrap();
        assert_eq!(result["dest/src/hook.rs"], b"fn hook() {}\n");
    }

    #[test]
    fn a_patch_refreshes_cleanly_or_stops_on_conflict() {
        let dir = scratch();
        let table = crate::modules::tests::sample_rules();
        let base = "a\nb\nc\nd\ne\nf\nold\n";
        let changed = base.replace("old", "patched");
        let patch = patch(&dir.0, "0001", base, &changed);
        let queue = Tree::from([(patch.filename(), patch.encode())]);
        let before = installed(base);
        let mut old = installed(&changed);
        let mut changes = Changes::new();
        notices::record(&mut changes, "dest/src/lib.rs", "patch 0001");
        notices::apply(&before, &mut old, &changes).unwrap();
        let mut files = installed(&base.replacen("a\n", "new upstream\n", 1));
        let mut apply_queue = |files: &mut Tree| run(&dir.0, NEW, &table.crates, files, &queue, &mut changes, |path| Ok(old.get(path).cloned()));
        let result = apply_queue(&mut files).unwrap();
        assert_eq!(result.refreshed, [patch.filename()]);
        assert_eq!(files, installed(&changed.replacen("a\n", "new upstream\n", 1)));
        assert_ne!(result.patches, queue);
        let err = apply_queue(&mut installed(&base.replace("old", "different"))).err().unwrap();
        assert!(err.contains("patch 0001 conflicts in src/lib.rs"), "{err}");
        assert!(fs::read_to_string(dir.0.join("conflicts/src/lib.rs")).unwrap().contains("<<<<<<<"));
    }
}

// For a real install recover historical patch bases from HEAD, so working
// copy edits cannot become part of a refreshed patch.
pub fn installed_file(dest: &Path, alternate: bool, path: &str) -> Result<Option<Vec<u8>>, String> {
    if alternate { return files::read_file(&files::checked(dest, path)?); }
    files::local(path)?;
    let object = format!("HEAD:./{path}");
    let dest = dest.canonicalize().map_err(|e| files::error(dest, e))?;
    let out = Command::new("git").arg("-C").arg(&dest).env("GIT_NO_LAZY_FETCH", "1").env("GIT_LITERAL_PATHSPECS", "1")
        .args(["ls-tree", "-z", "HEAD", "--", path]).output().map_err(|e| e.to_string())?;
    let listing = success(out, "git ls-tree HEAD")?;
    if listing.is_empty() { return Ok(None); }
    if !listing.starts_with(b"100644 blob ") && !listing.starts_with(b"100755 blob ") {
        return Err(format!("expected a regular installed blob at {path}"));
    }
    let out = Command::new("git").arg("-C").arg(&dest).env("GIT_NO_LAZY_FETCH", "1").args(["cat-file", "blob", &object])
        .output().map_err(|e| e.to_string())?;
    success(out, "git cat-file HEAD").map(Some)
}

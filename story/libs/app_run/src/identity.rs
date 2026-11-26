//! Build identity and the render cache key.
//!
//! A run's render is fresh while its cache key — run revision, build
//! identity, output format, platform (Makepad) commit — is the one a new
//! run would use. Nothing here builds or runs anything: `stale` only looks.

use crate::doc::RunOutput;
use makepad_app_registry::AppEntry;
use makepad_core_util::sha256::{to_hex, Sha256};
use makepad_strict_json::{self as json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

/// What was built: the package and the command that builds it, for its
/// repository and for Makepad the commit, whether tracked files differ from
/// it and a hash of those differences (tracked diff plus untracked files),
/// the binary's SHA-256, and whether this run built it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildIdentity {
    pub package: String,
    pub bin: String,
    /// The registry's build, as a stable string (no machine paths).
    pub build_command: String,
    pub repo_commit: String,
    pub repo_dirty: bool,
    pub repo_diff_sha256: String,
    pub makepad_commit: String,
    pub makepad_dirty: bool,
    pub makepad_diff_sha256: String,
    pub binary_sha256: String,
    /// The executor ran the build before this run (a `--no-build` run used
    /// whatever binary was there; `publish_run` refuses it).
    pub built: bool,
    /// `git`, or `no-git` for sources without a repository (an
    /// installed release): commits read `no-git` and only the binary identifies it.
    pub source: String,
}

/// The state of the git repository holding a directory.
pub struct GitState {
    pub commit: String,
    pub dirty: bool,
    /// SHA-256 of `git diff HEAD` and every untracked, not ignored file
    /// (name and content): what the commit does not say about the tree.
    pub diff_sha256: String,
    pub git: bool,
}

impl BuildIdentity {
    /// The identity of `entry`'s binary as it is on disk now, in the
    /// checkout at `root`. Reads git and hashes the binary and the trees'
    /// differences (a large release binary takes a few hundred ms): never
    /// on a UI thread.
    pub fn current(entry: &AppEntry, root: &Path, binary: &Path, built: bool) -> Result<BuildIdentity, String> {
        Self::with_binary(entry, root, file_sha256(binary)?, built)
    }

    pub fn with_binary(entry: &AppEntry, root: &Path, binary_sha256: String, built: bool) -> Result<BuildIdentity, String> {
        let repo = git_state(&root.join(entry.dir));
        let makepad = git_state(root);
        Ok(BuildIdentity {
            package: entry.package.to_string(),
            bin: entry.bin.to_string(),
            build_command: format!("cargo build --release -p {} --bin {}", entry.package, entry.bin),
            repo_commit: repo.commit,
            repo_dirty: repo.dirty,
            repo_diff_sha256: repo.diff_sha256,
            makepad_commit: makepad.commit,
            makepad_dirty: makepad.dirty,
            makepad_diff_sha256: makepad.diff_sha256,
            binary_sha256,
            built,
            source: if repo.git && makepad.git { "git" } else { "no-git" }.into(),
        })
    }

    /// The part of the identity the cache key uses: everything but
    /// `built`. The binary always counts, so an old binary never passes as
    /// a new build; the diff hashes make unbuilt edits count too.
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}:{}|{}:{}|{}",
            self.package,
            self.bin,
            self.build_command,
            self.repo_commit,
            self.repo_diff_sha256,
            self.makepad_commit,
            self.makepad_diff_sha256,
            self.binary_sha256
        )
    }

    pub fn to_json(&self) -> Value {
        json::obj(vec![
            ("package", json::s(&self.package)),
            ("bin", json::s(&self.bin)),
            ("build_command", json::s(&self.build_command)),
            ("repo_commit", json::s(&self.repo_commit)),
            ("repo_dirty", Value::Bool(self.repo_dirty)),
            ("repo_diff_sha256", json::s(&self.repo_diff_sha256)),
            ("makepad_commit", json::s(&self.makepad_commit)),
            ("makepad_dirty", Value::Bool(self.makepad_dirty)),
            ("makepad_diff_sha256", json::s(&self.makepad_diff_sha256)),
            ("binary_sha256", json::s(&self.binary_sha256)),
            ("built", Value::Bool(self.built)),
            ("source", json::s(&self.source)),
        ])
    }

    pub fn from_json(v: &Value) -> Option<BuildIdentity> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let b = |k: &str| v.get(k).and_then(Value::as_bool);
        Some(BuildIdentity {
            package: s("package")?,
            bin: s("bin")?,
            build_command: s("build_command")?,
            repo_commit: s("repo_commit")?,
            repo_dirty: b("repo_dirty")?,
            repo_diff_sha256: s("repo_diff_sha256")?,
            makepad_commit: s("makepad_commit")?,
            makepad_dirty: b("makepad_dirty")?,
            makepad_diff_sha256: s("makepad_diff_sha256")?,
            binary_sha256: s("binary_sha256")?,
            built: b("built")?,
            source: s("source")?,
        })
    }
}

/// `(run revision, build identity, output format, platform commit)`,
/// hashed to one hex string. The run revision is the `AppRun` asset
/// revision the script was published as (`exec::unpublished_revision` for
/// one that is not).
pub fn cache_key(run_revision: &str, build: &BuildIdentity, output: &RunOutput) -> String {
    let text = format!(
        "app-run/2\nrun {run_revision}\nbuild {}\noutput {}x{}@{}/{}\nplatform {}\n",
        build.key(),
        output.width,
        output.height,
        output.dpi_scale,
        output.fps,
        build.makepad_commit
    );
    makepad_core_util::sha256::sha256_hex(text.as_bytes())
}

/// Untracked files bigger than this are hashed by name and size only.
const UNTRACKED_CONTENT_BYTES: u64 = 1 << 20;

/// The git state of the repository holding `dir`; a directory outside any
/// repository (an installed release's sources) is `no-git`, clean.
pub fn git_state(dir: &Path) -> GitState {
    let run = |args: &[&str]| -> Option<Vec<u8>> {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
        out.status.success().then_some(out.stdout)
    };
    let Some(commit) = run(&["rev-parse", "HEAD"]) else {
        return GitState { commit: "no-git".into(), dirty: false, diff_sha256: String::new(), git: false };
    };
    let top = run(&["rev-parse", "--show-toplevel"]).map(|t| PathBuf::from(String::from_utf8_lossy(&t).trim()));
    let diff = run(&["diff", "HEAD", "--binary"]).unwrap_or_default();
    let untracked = run(&["ls-files", "--others", "--exclude-standard", "-z", "--full-name"]).unwrap_or_default();
    let status = run(&["status", "--porcelain", "--untracked-files=no"]).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&diff);
    let mut any_untracked = false;
    for name in untracked.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        any_untracked = true;
        hasher.update(name);
        let path = top.as_ref().map(|t| t.join(String::from_utf8_lossy(name).as_ref()));
        if let Some(meta) = path.as_ref().and_then(|p| std::fs::metadata(p).ok()) {
            hasher.update(&meta.len().to_le_bytes());
            if meta.is_file() && meta.len() <= UNTRACKED_CONTENT_BYTES {
                if let Ok(bytes) = std::fs::read(path.as_ref().unwrap()) {
                    hasher.update(&bytes);
                }
            }
        }
    }
    let dirty = !status.is_empty() || !diff.is_empty();
    let diff_sha256 = if dirty || any_untracked { to_hex(&hasher.finalize()) } else { String::new() };
    GitState { commit: String::from_utf8_lossy(&commit).trim().to_string(), dirty, diff_sha256, git: true }
}

pub fn file_sha256(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// The registry entry, checkout root and binary path of an app id.
pub fn locate(app: &str) -> Result<(&'static AppEntry, PathBuf, PathBuf), String> {
    let entry = makepad_app_registry::find(app).ok_or_else(|| format!("`{app}` is not a registry app id"))?;
    let root = makepad_app_registry::repo_root().ok_or("no Makepad checkout found (app runs build from sources)")?;
    let binary = makepad_app_registry::binary_path(entry).ok_or_else(|| format!("no binary path for `{app}`"))?;
    Ok((entry, root, binary))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> BuildIdentity {
        BuildIdentity {
            package: "makepad-example-counter".into(),
            bin: "makepad-example-counter".into(),
            build_command: "cargo build --release -p makepad-example-counter --bin makepad-example-counter".into(),
            repo_commit: "abc".into(),
            repo_dirty: false,
            repo_diff_sha256: String::new(),
            makepad_commit: "abc".into(),
            makepad_dirty: false,
            makepad_diff_sha256: String::new(),
            binary_sha256: "11".into(),
            built: true,
            source: "git".into(),
        }
    }

    #[test]
    fn the_cache_key_follows_each_of_its_parts() {
        let out = RunOutput { width: 1080, height: 1920, dpi_scale: 3.0, fps: 60 };
        let a = identity();
        let key = cache_key("r1", &a, &out);
        assert_eq!(key, cache_key("r1", &a, &out));
        assert_ne!(key, cache_key("r2", &a, &out));
        assert_ne!(key, cache_key("r1", &a, &RunOutput { fps: 30, ..out }));
        // Another binary, an unbuilt edit or another commit is another key.
        assert_ne!(key, cache_key("r1", &BuildIdentity { binary_sha256: "22".into(), ..a.clone() }, &out));
        assert_ne!(key, cache_key("r1", &BuildIdentity { repo_diff_sha256: "ff".into(), ..a.clone() }, &out));
        assert_ne!(key, cache_key("r1", &BuildIdentity { makepad_commit: "def".into(), ..a.clone() }, &out));
        // Whether this run built it is not part of it.
        assert_eq!(key, cache_key("r1", &BuildIdentity { built: false, ..a.clone() }, &out));
        assert_eq!(BuildIdentity::from_json(&a.to_json()), Some(a));
    }

    #[test]
    fn a_directory_outside_git_is_a_clean_no_git_source() {
        let state = git_state(Path::new("/"));
        assert!(!state.git && !state.dirty);
        assert_eq!(state.commit, "no-git");
    }
}

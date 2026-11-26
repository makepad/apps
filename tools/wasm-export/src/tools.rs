//! The export's tools, found and built the way Makepad's launchers find theirs
//! (`makepad_app_registry`: Code finds `makepad run` so, the WM its apps).
//!
//! The one tool the export runs is the trace host (`wasm-export`, which is `makepad-web-trace`
//! with the export around it): the drivers' runs, the coverage merges and the gate. It compiles
//! the app itself (Relax's JIT, from the sources), so no app binary is needed.
//!
//! - The **target directory** is the running executable's own (`<t>/release/<bin>`, or a test's
//!   `<t>/release/deps/<bin>`, `<t>` holding cargo's `CACHEDIR.TAG`), else cargo's
//!   (`CARGO_TARGET_DIR`, else the workspace's `target/`: [`makepad_app_registry::checkout_binary`]).
//!   The intermediates go to `<t>/wasm-export/<title>`.
//! - The **trace host**, from a checkout, is built there on every export (`cargo build --release
//!   -p makepad-wasm-export --bin wasm-export` into `<t>`, the launchers' cargo environment:
//!   [`makepad_app_registry::cargo_command`]; nothing compiles when it is fresh), so the export
//!   never runs one older than its sources. An
//!   installed layout without sources runs the one beside the executable.
//! - The trace host's processes get the checkout's std (`RELAX_STD`), a 16 MiB default
//!   thread stack (`RUST_MIN_STACK`: the JIT's deep compiles), no flight recorder
//!   (`RELAX_FLIGHTREC=0`), the capture's code generation in every run (no speculation,
//!   no zone swap: `RELAX_SPEC=0`, `RELAX_ZONE_SWAP=0`; else the training union
//!   names bodies the capture never compiles) and a Relax home of the export's own in the build
//!   folder (`RELAX_HOME`, `_TARGET_DIR`, `_CACHE_DIR`: nothing of another session's):
//!   [`host_env`]. The caller's environment is not relied on.

use crate::{Failure, Options, Phase, Progress};
use makepad_app_registry::{self as registry, AppTarget, ProcessOutput};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// The trace host's package and binary.
pub const TRACE_HOST: AppTarget<'static> = AppTarget {
    id: "wasm-export",
    package: "makepad-wasm-export",
    dir: "apps/apps/tools/wasm-export",
    bin: "wasm-export",
    manifest: None,
    profile: "release",
};

/// The cargo target directory an executable at `exe` was built into: `<t>/release/<bin>` or
/// `<t>/release/deps/<bin>` where `<t>` is a cargo target directory (it has `CACHEDIR.TAG`).
/// Release only: the export's tools are never debug builds.
pub fn target_of_exe(exe: &Path) -> Option<PathBuf> {
    let mut dir = exe.parent()?;
    if dir.file_name().is_some_and(|n| n == "deps") {
        dir = dir.parent()?;
    }
    if dir.file_name().is_none_or(|n| n != "release") {
        return None;
    }
    let target = dir.parent()?;
    target.join("CACHEDIR.TAG").is_file().then(|| target.to_path_buf())
}

/// The target directory the export builds into and keeps its intermediates in (see the module).
pub fn target_dir(source_root: &Path) -> PathBuf {
    let own = std::env::current_exe().ok().and_then(|exe| exe.canonicalize().ok()).and_then(|exe| target_of_exe(&exe));
    own.unwrap_or_else(|| {
        let binary = registry::checkout_binary(&TRACE_HOST, source_root, std::env::var_os("CARGO_TARGET_DIR").as_deref());
        // <t>/release/<bin>
        binary.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(|| source_root.join("target"))
    })
}

/// `<t>/wasm-export/<title>`: the intermediates of an export with this title.
pub fn build_dir(source_root: &Path, title: &str) -> PathBuf {
    let name: String = title.trim().chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' }).collect();
    let name = name.trim_matches(['-', '.']);
    target_dir(source_root).join("wasm-export").join(if name.is_empty() { "export" } else { name })
}

/// The environment of every trace host process of an export whose build folder is `build`
/// (see the module).
pub fn host_env(source_root: &Path, build: &Path) -> Vec<(String, String)> {
    let home = build.join("relax-home");
    let mut env: Vec<(String, String)> = [
        ("RUST_MIN_STACK", "16777216".to_string()),
        ("RELAX_FLIGHTREC", "0".to_string()),
        ("RELAX_SPEC", "0".to_string()),
        ("RELAX_ZONE_SWAP", "0".to_string()),
        ("RELAX_HOME", home.display().to_string()),
        ("RELAX_TARGET_DIR", home.join("target").display().to_string()),
        ("RELAX_CACHE_DIR", home.join("cache").display().to_string()),
    ].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    let std = source_root.join("libs/relax/compiler/std");
    if std.is_dir() {
        env.push(("RELAX_STD".into(), std.display().to_string()));
    }
    env
}

/// The trace host to run: `opts.trace_host`, else built (or found) as the module says.
pub fn trace_host(opts: &Options, progress: &mut dyn Progress) -> Result<PathBuf, Failure> {
    if let Some(host) = &opts.trace_host {
        return Ok(host.clone());
    }
    let root = &opts.source_root;
    let fail = |e: String| Failure::new(Phase::Trace, e);
    if !registry::has_sources(&TRACE_HOST, root) {
        // an installed layout: the host beside the executable
        return registry::installed_binary(TRACE_HOST.bin)
            .ok_or_else(|| fail(format!("no trace host: {} is not beside this executable and {} holds no sources to build it from", TRACE_HOST.bin, root.display())));
    }
    registry::on_disk(root).map_err(fail)?;
    let (program, args) = registry::build_argv(&TRACE_HOST, root);
    let target = target_dir(root);
    let mut command = registry::cargo_command(&program, &args, root);
    command.env("CARGO_TARGET_DIR", &target);
    let log = target.join("wasm-export").join("trace-host-build.log");
    let _ = std::fs::create_dir_all(log.parent().unwrap_or(&target));
    progress.phase(Phase::Trace, 0.0, &format!("building the trace host ({})", TRACE_HOST.package));
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()
        .map_err(|e| fail(format!("{}: {e}", program.display())))?;
    let stderr = child.stderr.take().ok_or_else(|| fail("cargo's output".into()))?;
    let mut errors = Vec::new();
    let mut cancelled = false;
    registry::read_output(stderr, std::fs::File::create(&log).ok(), |output| {
        if let ProcessOutput::Line(line) = output {
            if line.starts_with("error") || !errors.is_empty() {
                errors.push(line.clone());
            }
            if let Some((note, _)) = registry::cargo_progress(&line) {
                progress.phase(Phase::Trace, 0.0, &format!("trace host: {note}"));
            }
        }
        cancelled = progress.cancelled();
        !cancelled
    });
    if cancelled {
        let _ = child.kill();
        let _ = child.wait();
        return Err(fail("cancelled".into()));
    }
    let status = child.wait().map_err(|e| fail(e.to_string()))?;
    if !status.success() {
        let shown: Vec<String> = errors.into_iter().take(24).collect();
        return Err(fail(format!("building the trace host failed ({status}):\n{}", shown.join("\n"))).details(&log));
    }
    let mut host = target.join("release").join(TRACE_HOST.bin);
    if cfg!(windows) {
        host.set_extension("exe");
    }
    if !host.is_file() {
        return Err(fail(format!("the trace host build wrote no {}", host.display())).details(&log));
    }
    Ok(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_executable_names_its_release_target_dir() {
        let t = std::env::temp_dir().join(format!("wasm-export-tools-{}", std::process::id()));
        std::fs::create_dir_all(t.join("release/deps")).unwrap();
        // not a cargo target dir (yet)
        assert_eq!(target_of_exe(&t.join("release/x")), None);
        std::fs::write(t.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55").unwrap();
        assert_eq!(target_of_exe(&t.join("release/x")), Some(t.clone()));
        assert_eq!(target_of_exe(&t.join("release/deps/x-0123")), Some(t.clone()));
        // debug builds and other folders are not the export's
        assert_eq!(target_of_exe(&t.join("debug/x")), None);
        assert_eq!(target_of_exe(&t.join("x")), None);
        let _ = std::fs::remove_file(t.join("CACHEDIR.TAG"));
        let _ = std::fs::remove_dir(t.join("release/deps"));
        let _ = std::fs::remove_dir(t.join("release"));
        let _ = std::fs::remove_dir(&t);
    }

    #[test]
    fn this_test_builds_into_its_own_target_dir() {
        // a cargo test binary runs from <t>/release/deps (release only: the gates are release)
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        if let Some(own) = target_of_exe(&exe) {
            assert_eq!(target_dir(Path::new("/nonexistent")), own);
            assert_eq!(build_dir(Path::new("/nonexistent"), "Harbour (night): the film"), own.join("wasm-export").join("Harbour--night---the-film"));
        }
    }

    #[test]
    fn the_host_gets_its_environment() {
        let env = host_env(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..").as_path(), Path::new("/b"));
        assert!(env.iter().any(|(k, v)| k == "RUST_MIN_STACK" && v == "16777216"));
        assert!(env.iter().any(|(k, v)| k == "RELAX_FLIGHTREC" && v == "0"));
        assert!(env.iter().any(|(k, v)| k == "RELAX_SPEC" && v == "0"));
        assert!(env.iter().any(|(k, v)| k == "RELAX_HOME" && v == "/b/relax-home"));
        assert!(env.iter().any(|(k, v)| k == "RELAX_STD" && v.ends_with("libs/relax/compiler/std")));
    }
}

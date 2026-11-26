//! The link: the player built for the browser with the film in it.
//!
//! A real cargo build of `makepad-story-player` for `wasm32-unknown-unknown`
//! with the `film` feature: the pack (documents, subset fonts, small
//! pictures, the kernel module) comes in with `include_bytes!` from
//! `MAKEPAD_FILM_PACK` (the shader pack is one of its files). Threaded:
//! atomics and one shared memory, the standard library rebuilt for them
//! (the nightly toolchain's `build-std`), so the audio engine runs in the
//! AudioWorklet and the task pool has threads; the page needs a
//! cross-origin isolated host (COOP and COEP headers). Fat LTO, one
//! codegen unit, abort on panic; `--cfg makepad_precompiled_shaders` leaves the shader compiler
//! out. The function table is exported and growable: the kernel module
//! is linked into it once at start.
//!
//! The toolchain is rustup's `nightly`, with its `rust-src` for `build-std`.

use crate::analyse::Analysis;
use crate::{Diagnostic, ExportOptions, Phase, Progress};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const TARGET: &str = "wasm32-unknown-unknown";

/// No SIMD128 in the player's own code: Safari 26.2's optimising wasm tier
/// miscompiles a v128 compare in it (a TypeId equality the compiler had
/// vectorised failed with equal bytes in memory, so a downcast trapped).
/// The kernels the player generates and links at run time keep SIMD128:
/// they are their own modules.
pub const RUSTFLAGS: &str = "-C target-feature=+bulk-memory,+mutable-globals,+sign-ext,+nontrapping-fptoint,+atomics \
-C link-arg=--shared-memory -C link-arg=--max-memory=4294967296 -C link-arg=--import-memory \
-C link-arg=--export=__stack_pointer -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size \
-C link-arg=--export=__tls_align -C link-arg=--export=__tls_base \
-C link-arg=--export-table -C link-arg=--growable-table \
-C link-arg=--compress-relocations --cfg makepad_no_studio";

/// The target's features for the rebuilt standard library.
pub const TARGET_SPEC_FEATURES: &str = "+atomics,+bulk-memory,+mutable-globals";

/// Leaves the shader compiler out (every shader comes from the pack).
pub const PRECOMPILED_SHADERS_CFG: &str = " --cfg makepad_precompiled_shaders";

/// Crates on the frame's hot path, built for speed (opt-level 3) while the
/// rest is built for size (z). Measured on a full film: the Splash VM alone at
/// 3; six crates of the frame path at 3 (VM, kernel glue, draw, render
/// graph, Motion, its drawing) grew the code by 1.57 MB raw / 0.39 MB brotli
/// for about 5 % less document evaluation time, not worth it until a phone
/// measurement shows a crate that fixes a real frame-rate problem.
pub const HOT_CRATES: [&str; 1] = ["makepad-script"];

/// What the link made.
pub struct Linked {
    /// The wasm as the linker wrote it (with its name section).
    pub wasm: Vec<u8>,
    pub path: PathBuf,
}

/// A cargo command, or a diagnostic saying how to get one.
pub fn cargo(source_root: &Path) -> Result<Command, Diagnostic> {
    let cargo = std::env::var_os("CARGO").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("cargo"));
    match Command::new(&cargo).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status() {
        Ok(s) if s.success() => {}
        _ => {
            return Err(Diagnostic::error(Phase::Link, "no Rust toolchain found (cargo is not on PATH)")
                .help("install Rust from rustup.rs, then run the export again"))
        }
    }
    if !source_root.join("Cargo.toml").exists() {
        return Err(Diagnostic::error(Phase::Link, format!("{} is not a Makepad source tree", source_root.display()))
            .help("the web export builds the player from Makepad's sources; point it at a Makepad checkout"));
    }
    let mut c = Command::new(cargo);
    c.current_dir(source_root);
    Ok(c)
}

/// Runs cargo, reporting `Compiling <crate>` lines as progress and its
/// errors as diagnostics.
pub fn run_cargo(mut cmd: Command, phase: Phase, progress: &mut dyn Progress) -> Result<(), Vec<Diagnostic>> {
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| vec![Diagnostic::error(phase, format!("running cargo: {e}"))])?;
    let mut errors: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let mut compiled = 0usize;
    if let Some(err) = child.stderr.take() {
        for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("Compiling ") {
                compiled += 1;
                // Cargo does not say how many crates are left: show a
                // count, and the fraction creeps towards 1.
                progress.phase(phase, 1.0 - 1.0 / (1.0 + compiled as f64 / 40.0), rest.split_whitespace().next().unwrap_or(rest));
            } else if trimmed.starts_with("error") {
                if let Some(e) = current.take() {
                    errors.push(e);
                }
                current = Some(line.clone());
            } else if trimmed.starts_with("warning") || trimmed.starts_with("Finished") || trimmed.starts_with("Checking") {
                if let Some(e) = current.take() {
                    errors.push(e);
                }
            } else if let Some(e) = &mut current {
                e.push('\n');
                e.push_str(&line);
            }
            if progress.cancelled() {
                let _ = child.kill();
            }
        }
    }
    if let Some(e) = current.take() {
        errors.push(e);
    }
    let status = child.wait().map_err(|e| vec![Diagnostic::error(phase, format!("cargo: {e}"))])?;
    if progress.cancelled() {
        return Err(vec![Diagnostic::error(phase, "cancelled")]);
    }
    if !status.success() {
        let mut diags: Vec<Diagnostic> = errors.into_iter().map(|e| Diagnostic::error(phase, e)).collect();
        if diags.is_empty() {
            diags.push(Diagnostic::error(phase, format!("cargo failed ({status})")));
        }
        return Err(diags);
    }
    Ok(())
}

/// The threaded target: the nightly toolchain's wasm32 spec with atomics
/// on, written beside the build (`<target>/wasm32-unknown-unknown.json`).
fn target_spec(source_root: &Path, target_dir: &Path) -> Result<PathBuf, Diagnostic> {
    let out = Command::new("rustup")
        .current_dir(source_root)
        .args(["run", "nightly", "rustc", "-Z", "unstable-options", "--print", "target-spec-json", "--target", TARGET])
        .output()
        .map_err(|e| Diagnostic::error(Phase::Link, format!("running rustup: {e}")).help("install Rust from rustup.rs"))?;
    if !out.status.success() {
        return Err(Diagnostic::error(Phase::Link, format!("the nightly toolchain cannot print the {TARGET} spec: {}", String::from_utf8_lossy(&out.stderr).trim()))
            .help("add it: `rustup toolchain install nightly --component rust-src`"));
    }
    let mut spec = String::from_utf8_lossy(&out.stdout).to_string();
    if spec.contains("\"features\"") {
        return Err(Diagnostic::error(Phase::Link, "the nightly wasm32 spec already names features; the export does not know how to add atomics to it"));
    }
    let at = spec.rfind('}').ok_or_else(|| Diagnostic::error(Phase::Link, "the nightly wasm32 spec is not JSON"))?;
    spec.insert_str(at, &format!(",\n  \"features\": \"{TARGET_SPEC_FEATURES}\"\n"));
    let dir = target_dir.join("spec");
    std::fs::create_dir_all(&dir).map_err(|e| Diagnostic::error(Phase::Link, format!("{}: {e}", dir.display())))?;
    let path = dir.join(format!("{TARGET}.json"));
    std::fs::write(&path, spec).map_err(|e| Diagnostic::error(Phase::Link, format!("{}: {e}", path.display())))?;
    Ok(path)
}

/// Builds the film's wasm.
pub fn build(opts: &ExportOptions, pack: &Path, analysis: &Analysis, progress: &mut dyn Progress) -> Result<Linked, Vec<Diagnostic>> {
    progress.phase(Phase::Link, 0.0, "cargo build --target wasm32-unknown-unknown (threaded)");
    // The film's own target directory: its RUSTFLAGS differ from any other
    // wasm build, so sharing one would rebuild both every time.
    let target_dir = opts.source_root.join("target/web-export");
    let spec = target_spec(&opts.source_root, &target_dir).map_err(|e| vec![e])?;
    let mut cmd = Command::new("rustup");
    cmd.current_dir(&opts.source_root).args(["run", "nightly", "cargo"]);
    // The compiler stays when asked to, or when there is no shader pack.
    let prebuilt_only = opts.pack.shaders == crate::ShadersKnob::PrebuiltOnly && !analysis.shaders.is_empty();
    let rustflags = if prebuilt_only { format!("{RUSTFLAGS}{PRECOMPILED_SHADERS_CFG}") } else { RUSTFLAGS.to_string() };
    let mut features = vec!["film".to_string()];
    features.extend(analysis.features.iter().cloned());
    cmd.args(["build", "--release", "--target"]).arg(&spec).args(["-Z", "json-target-spec", "-Z", "build-std=panic_abort,std"]);
    cmd.args(["-p", "makepad-story-player", "--no-default-features", "--features", &features.join(",")]);
    for krate in HOT_CRATES {
        cmd.args(["--config", &format!("profile.release.package.\"{krate}\".opt-level=3")]);
    }
    cmd
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("RUSTFLAGS", rustflags)
        .env("CARGO_PROFILE_RELEASE_LTO", "fat")
        .env("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1")
        .env("CARGO_PROFILE_RELEASE_PANIC", "abort")
        .env("CARGO_PROFILE_RELEASE_INCREMENTAL", "false")
        .env("CARGO_PROFILE_RELEASE_OPT_LEVEL", "z")
        .env("CARGO_PROFILE_RELEASE_DEBUG", "0")
        .env("MAKEPAD_FILM_PACK", pack);
    run_cargo(cmd, Phase::Link, progress)?;
    let path = target_dir.join(spec.file_stem().unwrap()).join("release/makepad-story-player.wasm");
    let wasm = std::fs::read(&path).map_err(|e| vec![Diagnostic::error(Phase::Link, format!("{}: {e}", path.display()))])?;
    progress.phase(Phase::Link, 1.0, &format!("{} linked", crate::report::mb(wasm.len())));
    Ok(Linked { wasm, path })
}

/// The linked wasm made small by our own optimiser (dead code, identical
/// functions, locals, compaction, ordering; custom sections out), and the
/// size profile of the linked code by crate (from its name section) with
/// the optimiser's report.
///
/// With the analysis's census (`modules`), the module strip runs first: the
/// registration of every script module the film never used is dropped,
/// with what only it reached (the optimiser's `units::plan_modules`). The
/// film's table is exported only for the kernel linker to grow it, so it
/// is pruned too.
/// A panic becomes a plain trap (its message and location leave the wasm);
/// the symbol file (`film.symbols`, beside the build, not served) maps a
/// trap's code offset back to them.
pub fn optimise(wasm: &[u8], modules: Option<&makepad_wasm_strip::ModuleUse>) -> Result<(Vec<u8>, String, Option<String>), Diagnostic> {
    let profile = makepad_wasm_strip::wasm_size_profile(wasm, &makepad_wasm_strip::ProfileOptions::default())
        .map(|p| p.to_text(25))
        .unwrap_or_default();
    let opts = makepad_wasm_strip::OptimizeOptions { modules: modules.cloned(), prune_exported_table: false, panic_trap: true, symbols: true, ..Default::default() };
    let (out, report) = makepad_wasm_strip::wasm_optimize_checked(wasm, &opts)
        .map_err(|e| Diagnostic::error(Phase::Optimise, format!("the linked wasm: {e}")))?;
    Ok((out, format!("{}{}\n{profile}", report.units.clone().unwrap_or_default(), report.to_text()), report.symbols.clone()))
}

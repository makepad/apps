//! The analysis: the film evaluated and drawn by the player itself,
//! natively, every frame of the edit, with the recorders on (compute's
//! `record_compiled`, the platform's shader table recording, the text
//! layouter's and 3D type's glyph recording). The player writes what it
//! saw into a directory ([`FILES`]); this module builds the player when
//! needed, runs it and reads that directory back.
//!
//! The player runs hidden and muted (`--analyse <dir>`), as its own
//! process: a fresh process records every shader it builds (one already
//! running has built most of them before recording could start).

use crate::{Diagnostic, ExportOptions, Phase, Progress, Severity};
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the player writes into the analysis directory: the collect
/// manifest's files (`makepad_web_pack::manifest`).
pub use makepad_web_pack::manifest::files;

/// The analysis's numbers, for the report.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub values: BTreeMap<String, String>,
}

impl Summary {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub glyphs: BTreeMap<String, BTreeSet<u16>>,
    /// Per font hash: (text, frames it was laid out in).
    pub texts: BTreeMap<String, Vec<(String, u32)>>,
    /// (family as the document names it, coverage spec).
    pub coverage: Vec<(String, String)>,
    /// Per font hash: every set of variation axis values ((tag, value))
    /// the font was laid out at.
    pub instances: BTreeMap<String, Vec<Vec<(u32, f32)>>>,
    /// Per font hash: the font file it was read from (the app's own
    /// fonts, which load by resource path).
    pub font_files: BTreeMap<String, PathBuf>,
    pub kernels_wasm: Vec<u8>,
    pub kernel_keys: Vec<u8>,
    /// The film's audio programs as one wasm module, and their keys in
    /// export order (empty when the film plays none).
    pub audio_wasm: Vec<u8>,
    pub audio_keys: Vec<u8>,
    pub shaders: Vec<u8>,
    pub features: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
    pub summary: Summary,
    /// The script modules the film registered and used (the census): the
    /// optimiser drops the registration of the unused ones.
    pub modules: Option<makepad_wasm_strip::ModuleUse>,
    /// The UI kit's Splash the run never reached, as a blank list
    /// (`makepad_platform::splash_blank::BlankList` bytes; empty for none).
    pub splash_blank: Vec<u8>,
}

/// The native player executable in the source tree's target directory.
/// The analysis's blank list file (the player's `analyse::SPLASH_BLANK`).
pub const SPLASH_BLANK: &str = "splash_blank.bin";

pub fn player_binary(source_root: &Path) -> PathBuf {
    let name = if cfg!(windows) { "makepad-story-player.exe" } else { "makepad-story-player" };
    source_root.join("target/release").join(name)
}

/// Builds the native player (cargo, release) and runs its analysis of the
/// bundle in `bundle_dir`.
pub fn run(opts: &ExportOptions, bundle_dir: &Path, progress: &mut dyn Progress) -> Result<Analysis, Vec<Diagnostic>> {
    progress.phase(Phase::Analyse, 0.0, "building the player");
    let mut cargo = crate::link::cargo(&opts.source_root).map_err(|e| vec![e])?;
    cargo.args(["build", "--release", "-p", "makepad-story-player"]).current_dir(&opts.source_root);
    crate::link::run_cargo(cargo, Phase::Analyse, progress)?;
    let dir = opts.build_dir.join("analysis");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| vec![Diagnostic::error(Phase::Analyse, format!("{}: {e}", dir.display()))])?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| vec![Diagnostic::error(Phase::Analyse, format!("{}: {e}", dir.display()))])?;
    let mut child = Command::new(player_binary(&opts.source_root))
        .arg("--film")
        .arg(bundle_dir)
        .arg("--analyse")
        .arg(&dir)
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .env("MAKEPAD_MUTE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| vec![Diagnostic::error(Phase::Analyse, format!("starting the player: {e}"))])?;
    // `analyse <t> <duration>` lines: how far the evaluation is.
    if let Some(out) = child.stdout.take() {
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            let mut words = line.split_whitespace();
            if words.next() == Some("analyse") {
                let t: f64 = words.next().and_then(|w| w.parse().ok()).unwrap_or(0.0);
                let d: f64 = words.next().and_then(|w| w.parse().ok()).unwrap_or(1.0);
                progress.phase(Phase::Analyse, (t / d.max(1e-9)).clamp(0.0, 1.0), &format!("{t:.2} s of {d:.2} s"));
                if progress.cancelled() {
                    let _ = child.kill();
                }
            }
        }
    }
    let status = child.wait().map_err(|e| vec![Diagnostic::error(Phase::Analyse, format!("the player: {e}"))])?;
    if progress.cancelled() {
        return Err(vec![Diagnostic::error(Phase::Analyse, "cancelled")]);
    }
    if !status.success() || !dir.join(files::DONE).exists() {
        let mut diags = read_diagnostics(&dir);
        diags.push(Diagnostic::error(Phase::Analyse, format!("the player's analysis did not finish ({status})")));
        return Err(diags);
    }
    let analysis = read(&dir).map_err(|e| vec![Diagnostic::error(Phase::Analyse, e)])?;
    progress.phase(
        Phase::Kernels,
        1.0,
        &format!("{} kernels, {} bytes of wasm", analysis.summary.get("kernels").unwrap_or("0"), analysis.kernels_wasm.len()),
    );
    progress.phase(Phase::Shaders, 1.0, &format!("{} shaders", analysis.summary.get("shaders").unwrap_or("0")));
    Ok(analysis)
}

/// Draws the film in `bundle_dir` (cut fonts) at random times with the
/// player and counts glyphs shaped to `.notdef`: 0 when the fonts miss
/// nothing.
/// With `splash_blank` (a blank list file) the film runs on the blanked UI
/// kit too, and the second value says whether that run reported a Splash
/// error (a blanked definition something still needed).
pub fn check_glyphs(opts: &ExportOptions, bundle_dir: &Path, splash_blank: Option<&Path>, progress: &mut dyn Progress) -> Result<(usize, Vec<String>), Vec<Diagnostic>> {
    let dir = opts.build_dir.join("check");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| vec![Diagnostic::error(Phase::Fonts, format!("{}: {e}", dir.display()))])?;
    }
    progress.phase(Phase::Fonts, 0.9, "checking the cut fonts at random times");
    let mut cmd = Command::new(player_binary(&opts.source_root));
    cmd.arg("--film")
        .arg(bundle_dir)
        .arg("--analyse")
        .arg(&dir)
        .args(["--check-glyphs", "300"])
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .env("MAKEPAD_MUTE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(list) = splash_blank {
        cmd.env("MAKEPAD_SPLASH_BLANK", list);
    }
    let out = cmd.output().map_err(|e| vec![Diagnostic::error(Phase::Fonts, format!("starting the player: {e}"))])?;
    let status = out.status;
    // Script errors the blanked kit caused (`[E] <file:line> - ...`).
    let splash_errors: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&out.stderr).lines())
        .filter(|l| l.starts_with("[E] widgets/") && (l.contains("not found") || l.contains("type mismatch") || l.contains("pop_stack")))
        .map(String::from)
        .collect();
    let text = std::fs::read_to_string(dir.join("check.txt")).unwrap_or_default();
    let missing = text.lines().find_map(|l| l.strip_prefix("missing=")).and_then(|v| v.parse().ok());
    match missing {
        Some(n) if status.success() => Ok((n, splash_errors)),
        _ => Err(vec![Diagnostic::error(Phase::Fonts, format!("the glyph check did not finish ({status})"))]),
    }
}

/// Reads an analysis directory.
pub fn read(dir: &Path) -> Result<Analysis, String> {
    let m = makepad_web_pack::manifest::Manifest::read(dir)?;
    makepad_story_film_pack::KernelEntry::read_all(&m.kernel_keys)?;
    let blob = |name: &str| m.kernel_blobs.get(name).map(|b| b.bytes.clone()).unwrap_or_default();
    let (audio_wasm, audio_keys) = (blob(makepad_story_film_pack::AUDIO_BLOB), blob(makepad_story_film_pack::AUDIO_KEYS_BLOB));
    makepad_story_film_pack::read_audio_keys(&audio_keys)?;
    Ok(Analysis {
        glyphs: m.glyphs,
        texts: m.texts.into_iter().map(|(font, rows)| (font, rows.into_iter().collect())).collect(),
        coverage: m.coverage.into_iter().map(|c| (c.face, c.spec)).collect(),
        instances: read_instances(dir),
        font_files: std::fs::read_to_string(dir.join("fontfiles.txt"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(h, p)| (h.to_string(), PathBuf::from(p))))
            .collect(),
        kernels_wasm: m.kernels_wasm,
        kernel_keys: m.kernel_keys,
        audio_wasm,
        audio_keys,
        shaders: m.shaders,
        features: m.features.into_iter().collect(),
        diagnostics: read_diagnostics(dir),
        summary: Summary { values: m.summary },
        modules: (!m.modules_registered.is_empty()).then(|| makepad_wasm_strip::ModuleUse { registered: m.modules_registered, used: m.modules.into_keys().collect() }),
        splash_blank: std::fs::read(dir.join(SPLASH_BLANK)).unwrap_or_default(),
    })
}

/// `instances.txt`: `<font hash>\t<tag>=<value>,...` per line.
fn read_instances(dir: &Path) -> BTreeMap<String, Vec<Vec<(u32, f32)>>> {
    let mut out: BTreeMap<String, Vec<Vec<(u32, f32)>>> = BTreeMap::new();
    let text = std::fs::read_to_string(dir.join("instances.txt")).unwrap_or_default();
    for line in text.lines() {
        let Some((font, coords)) = line.split_once('\t').or_else(|| Some((line, ""))) else { continue };
        let coords = coords
            .split(',')
            .filter_map(|c| {
                let (tag, v) = c.split_once('=')?;
                let tag: [u8; 4] = tag.as_bytes().try_into().ok()?;
                Some((u32::from_be_bytes(tag), v.parse().ok()?))
            })
            .collect();
        out.entry(font.to_string()).or_default().push(coords);
    }
    out
}

fn read_diagnostics(dir: &Path) -> Vec<Diagnostic> {
    makepad_web_pack::manifest::read_diagnostics(dir)
        .into_iter()
        .map(|d| {
            let severity = match d.severity {
                makepad_web_pack::manifest::Severity::Error => Severity::Error,
                makepad_web_pack::manifest::Severity::Warning => Severity::Warning,
                makepad_web_pack::manifest::Severity::Note => Severity::Note,
            };
            let out = Diagnostic { severity, ..Diagnostic::error(Phase::Analyse, d.message) };
            if d.file.is_empty() {
                out
            } else {
                out.at(d.file, d.line, d.col)
            }
        })
        .collect()
}

/// One diagnostics line as [`read_diagnostics`] reads it (for the player).
pub use makepad_web_pack::manifest::diagnostic_line;

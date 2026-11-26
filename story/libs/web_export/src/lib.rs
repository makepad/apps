//! Web export: an edit compiled into a film for the browser.
//!
//! ```text
//! makepad-film-build --edit film.cut.splash --media media.txt --out dir [--credits credits.txt] [--mobile] [--no-music]
//! ```
//!
//! The export is a compiler. Its phases ([`Phase`]):
//!
//! 1. **resolve**: the edit, every Motion document and module it draws and
//!    every file they use, in memory (`makepad-story-player-bundle`).
//! 2. **analyse**: the film evaluated and drawn frame by frame by the
//!    player itself, natively, with recorders on: every kernel compiled,
//!    every draw shader built, every glyph laid out ([`analyse`]).
//! 3. **kernels**: those kernels as one wasm module (SIMD128 where a
//!    kernel has the shape), made by the compute crate's wasm backend.
//! 4. **shaders**: those shaders as GLSL ES with their reflection, made by
//!    the platform's shader compiler.
//! 5. **fonts**: each font cut to the glyphs the film draws ([`fonts`]).
//! 6. **link**: a cargo build of the player for wasm32 with the film
//!    packed into it ([`link`]): single-threaded, LTO, only the subsystems
//!    the film uses; the kernel and shader compilers are not linked.
//! 7. **optimise**: the linked wasm stripped and optimised by our own
//!    optimiser, then brotli-compressed.
//! 8. **write**: `index.html`, `film.js`, `film.wasm` (+ `.br`), the music
//!    beside them (optional), large pictures, `CREDITS.txt` and `README.txt`
//!    ([`page`]).
//!
//! Problems are [`Diagnostic`]s, written like a compiler's (the document,
//! line and column when there is one). The same library runs Stage's
//! the Web export card, the `makepad-film-build` CLI and the studio API's
//! `export_web`.

pub mod analyse;
pub mod fonts;
pub mod link;
pub mod page;
pub mod report;
pub mod safety;
pub mod shader_loops;
pub mod units;

use makepad_story_film_pack::{names, PackWriter};
use makepad_story_player_bundle::Bundle;
use std::fmt;
use std::path::{Path, PathBuf};

/// Who the film is tuned for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preset {
    #[default]
    Desktop,
    /// Smaller pictures; plates that are heavy on a phone are warned about.
    Mobile,
}

/// The crunch knobs, shared with `cargo makepad wasm --pack`.
pub use makepad_web_pack::pack::{AssetsKnob, FontsKnob, KernelsKnob, PackOptions, PackPreset, ShadersKnob};

#[derive(Clone, Debug)]
pub struct ExportOptions {
    /// A render job (a tree cut and every file it reads, where they lie),
    /// shared with the renderer.
    pub job: Option<PathBuf>,
    /// The edit (`.cut.splash`).
    pub edit: PathBuf,
    /// Its tree folder, or a media table (`id=file`, `lyrics:id=file`).
    pub media: PathBuf,
    /// A bundle written earlier (`makepad-story-player-bundle`, or this export's
    /// build folder) to export instead of resolving `edit` and `media`: a
    /// fixed snapshot of a film.
    pub bundle: Option<PathBuf>,
    /// Where the film is written. Only files the export names are written
    /// there (see [`page::OUTPUT_FILES`]).
    pub out_dir: PathBuf,
    pub preset: Preset,
    /// Put the song beside the page (it plays when present; the film plays
    /// silent without it).
    pub include_music: bool,
    /// The film's credits as the author gives them (the song, the
    /// visuals): the shipped fonts' notices and the engine line are added.
    pub credits: String,
    /// The Makepad checkout the player is built from (the cargo workspace).
    pub source_root: PathBuf,
    /// Intermediate files (the bundle, the analysis, the pack, cargo's
    /// target directory). Content-addressed; safe to delete.
    pub build_dir: PathBuf,
    /// The crunch knobs.
    pub pack: PackOptions,
    /// The browser safety gate ([`safety`]): on by default.
    pub safety: safety::SafetyOptions,
}

/// The phases, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    Resolve,
    Analyse,
    Kernels,
    Shaders,
    Fonts,
    Link,
    Optimise,
    Write,
    Safety,
}

impl Phase {
    pub const ALL: [Phase; 9] = [Phase::Resolve, Phase::Analyse, Phase::Kernels, Phase::Shaders, Phase::Fonts, Phase::Link, Phase::Optimise, Phase::Write, Phase::Safety];

    pub fn name(self) -> &'static str {
        match self {
            Phase::Resolve => "resolve",
            Phase::Analyse => "analyse",
            Phase::Kernels => "kernels",
            Phase::Shaders => "shaders",
            Phase::Fonts => "fonts",
            Phase::Link => "link",
            Phase::Optimise => "optimise",
            Phase::Write => "write",
            Phase::Safety => "safety",
        }
    }

    /// What the phase does, for a progress line.
    pub fn title(self) -> &'static str {
        match self {
            Phase::Resolve => "Resolving assets",
            Phase::Analyse => "Evaluating the timeline",
            Phase::Kernels => "Compiling kernels (wasm SIMD128)",
            Phase::Shaders => "Compiling shaders (WebGL2)",
            Phase::Fonts => "Subsetting fonts",
            Phase::Link => "Linking the runtime",
            Phase::Optimise => "Optimising and compressing",
            Phase::Write => "Writing the output",
            Phase::Safety => "Playing it in a browser (safety gate)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Note,
    Warning,
    Error,
}

/// A problem, as a compiler reports one.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub phase: Phase,
    pub message: String,
    /// The document (or file) and where in it, when known.
    pub file: Option<String>,
    pub line: u32,
    pub col: u32,
    /// How to fix it.
    pub help: Option<String>,
}

impl Diagnostic {
    pub fn error(phase: Phase, message: impl Into<String>) -> Self {
        Diagnostic { severity: Severity::Error, phase, message: message.into(), file: None, line: 0, col: 0, help: None }
    }

    pub fn warning(phase: Phase, message: impl Into<String>) -> Self {
        Diagnostic { severity: Severity::Warning, ..Self::error(phase, message) }
    }

    pub fn note(phase: Phase, message: impl Into<String>) -> Self {
        Diagnostic { severity: Severity::Note, ..Self::error(phase, message) }
    }

    pub fn at(mut self, file: impl Into<String>, line: u32, col: u32) -> Self {
        self.file = Some(file.into());
        self.line = line;
        self.col = col;
        self
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.severity {
            Severity::Note => "note",
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        write!(f, "{kind}[{}]: {}", self.phase.name(), self.message)?;
        if let Some(file) = &self.file {
            if self.line > 0 {
                write!(f, "\n  --> {file}:{}:{}", self.line, self.col.max(1))?;
            } else {
                write!(f, "\n  --> {file}")?;
            }
        }
        if let Some(help) = &self.help {
            write!(f, "\n   = help: {help}")?;
        }
        Ok(())
    }
}

/// Where the export reports to (Stage's dialog, the CLI's terminal).
pub trait Progress {
    /// `phase` is running, `done` of it (0..1), with a short note.
    fn phase(&mut self, phase: Phase, done: f64, note: &str);
    fn diagnostic(&mut self, d: &Diagnostic);
    /// The user cancelled: the export stops at the next step.
    fn cancelled(&self) -> bool {
        false
    }
}

/// Progress on standard error (the CLI).
pub struct StderrProgress;

impl Progress for StderrProgress {
    fn phase(&mut self, phase: Phase, done: f64, note: &str) {
        eprintln!("[{:>8}] {:>3.0}% {}", phase.name(), done * 100.0, note);
    }

    fn diagnostic(&mut self, d: &Diagnostic) {
        eprintln!("{d}");
    }
}

/// What the export wrote and what it weighs.
#[derive(Clone, Debug, Default)]
pub struct ExportReport {
    /// Every output file: name, bytes, brotli bytes.
    pub files: Vec<report::SizedFile>,
    /// The film wasm by part (code, kernels, shaders, fonts, documents,
    /// pictures, data), raw and brotli.
    pub breakdown: Vec<report::Part>,
    pub diagnostics: Vec<Diagnostic>,
    /// The analysis's numbers: frames evaluated, kernels, shaders, glyphs.
    pub analysis: analyse::Summary,
    /// Every font, subset or full, with its size and why.
    pub fonts: Vec<fonts::ShippedFont>,
    /// The knobs the export ran with.
    pub pack: PackOptions,
}

/// The cancel check between steps.
fn check(progress: &dyn Progress, phase: Phase) -> Result<(), Vec<Diagnostic>> {
    if progress.cancelled() {
        return Err(vec![Diagnostic::error(phase, "cancelled")]);
    }
    Ok(())
}

/// Pictures larger than this stay files beside the page (fetched when a
/// plate needs them) instead of being packed into the wasm: photos are
/// compressed already (brotli gains nothing), and a film's budget is its
/// wasm; small pictures (UI bits, masks) ride in the pack.
pub const PACK_PICTURE_LIMIT: usize = 32 * 1024;

/// Runs the export. Diagnostics that are not errors are in the report;
/// an error stops it and comes back with the others.
pub fn export(opts: &ExportOptions, progress: &mut dyn Progress) -> Result<ExportReport, Vec<Diagnostic>> {
    let mut diags: Vec<Diagnostic> = Vec::new();
    let fail = |diags: &mut Vec<Diagnostic>, d: Diagnostic, progress: &mut dyn Progress| {
        progress.diagnostic(&d);
        diags.push(d);
        std::mem::take(diags)
    };
    std::fs::create_dir_all(&opts.build_dir).map_err(|e| vec![Diagnostic::error(Phase::Resolve, format!("{}: {e}", opts.build_dir.display()))])?;

    // 1. Resolve.
    progress.phase(Phase::Resolve, 0.0, &opts.edit.display().to_string());
    let resolved = bundle_of(opts);
    let mut bundle = match resolved {
        Ok(b) => b,
        Err(e) => return Err(fail(&mut diags, resolve_diagnostic(&e, &opts.edit), progress)),
    };
    progress.phase(Phase::Resolve, 1.0, &format!("{} plates, {} files", bundle.plates.len(), bundle.entries.len()));
    check(progress, Phase::Resolve)?;

    // 2-4. Analyse (with kernels and shaders made from what it recorded).
    let bundle_dir = opts.build_dir.join("bundle");
    bundle.write(&bundle_dir, false).map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Analyse, e), progress))?;
    let mut analysis = match analyse::run(opts, &bundle_dir, progress) {
        Ok(a) => a,
        Err(mut e) => {
            diags.append(&mut e);
            return Err(diags);
        }
    };
    for d in &analysis.diagnostics {
        progress.diagnostic(d);
        diags.push(d.clone());
    }
    if diags.iter().any(|d| d.severity == Severity::Error) {
        return Err(diags);
    }
    check(progress, Phase::Shaders)?;

    // 5. Fonts.
    let shipped_fonts = match fonts::subset_bundle(&mut bundle, &analysis, opts.pack.fonts, &opts.source_root, progress) {
        Ok(f) => f,
        Err(e) => return Err(fail(&mut diags, e, progress)),
    };
    check(progress, Phase::Fonts)?;
    // The safety net, before anything ships: the film drawn at random
    // times with its cut fonts may not miss a glyph.
    if shipped_fonts.iter().any(|f| f.subset) {
        let check_dir = opts.build_dir.join("bundle-check");
        bundle.write(&check_dir, false).map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Fonts, e), progress))?;
        // The same run checks the blanked UI kit (see the optimise step).
        let blank_file = (!analysis.splash_blank.is_empty()).then(|| opts.build_dir.join(analyse::SPLASH_BLANK));
        if let Some(f) = &blank_file {
            let _ = std::fs::write(f, &analysis.splash_blank);
        }
        match analyse::check_glyphs(opts, &check_dir, blank_file.as_deref(), progress) {
            Ok((0, splash_errors)) => {
                progress.phase(Phase::Fonts, 1.0, "no glyph missing at 300 random times");
                if !splash_errors.is_empty() {
                    progress.diagnostic(&Diagnostic::warning(Phase::Fonts, format!("the UI kit's unused Splash stays: the film reported {} errors without it (first: {})", splash_errors.len(), splash_errors[0])));
                    analysis.splash_blank.clear();
                }
            }
            Ok((n, _)) => {
                return Err(fail(
                    &mut diags,
                    Diagnostic::error(Phase::Fonts, format!("{n} glyphs missing from the cut fonts when the film is drawn at random times"))
                        .help("declare the text's coverage (`glyphs: \"latin\"` or `glyphs: @all` on the Text or Draw2D text), or export with fonts: full; the player log names the faces and code points"),
                    progress,
                ))
            }
            Err(mut e) => {
                diags.append(&mut e);
                return Err(diags);
            }
        }
    }

    // The pack: every bundle file but the song and the large pictures,
    // the manifest, the kernels and the shader table.
    let manifest = bundle.manifest_json();
    let mut pack = PackWriter::new();
    let mut beside: Vec<(String, Vec<u8>)> = Vec::new();
    let song_asset = bundle.song.as_ref().map(|s| s.asset.clone());
    for (logical, entry) in &bundle.entries {
        let is_song = song_asset.as_deref() == Some(logical.as_str());
        let kind = makepad_story_player_bundle::kind_of(&entry.file);
        if is_song {
            if opts.include_music {
                beside.push((entry.file.clone(), entry.bytes.clone()));
            }
        } else if kind == "textures"
            && match opts.pack.assets {
                AssetsKnob::BySize => entry.bytes.len() > PACK_PICTURE_LIMIT,
                AssetsKnob::External => true,
                AssetsKnob::Embedded => false,
            }
        {
            beside.push((entry.file.clone(), entry.bytes.clone()));
        } else {
            pack.add(&entry.file, entry.bytes.clone());
        }
    }
    pack.add("manifest.json", manifest.into_bytes());
    pack.add(names::KERNELS, analysis.kernels_wasm.clone());
    pack.add(names::KERNEL_KEYS, analysis.kernel_keys.clone());
    if !analysis.audio_wasm.is_empty() {
        pack.add(names::AUDIO, analysis.audio_wasm.clone());
        pack.add(names::AUDIO_KEYS, analysis.audio_keys.clone());
    }
    if !analysis.shaders.is_empty() {
        pack.add(names::SHADERS, analysis.shaders.clone());
    }
    let pack_path = opts.build_dir.join("film.pack");
    let pack_bytes = pack.finish();
    std::fs::write(&pack_path, &pack_bytes).map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Link, format!("{}: {e}", pack_path.display())), progress))?;

    // 6. Link.
    let linked = match link::build(opts, &pack_path, &analysis, progress) {
        Ok(l) => l,
        Err(e) => {
            for d in &e {
                progress.diagnostic(d);
            }
            diags.extend(e);
            return Err(diags);
        }
    };
    check(progress, Phase::Link)?;

    // 7. Optimise and compress.
    progress.phase(Phase::Optimise, 0.0, "stripping");
    let (mut optimised, optimise_report, symbols) = link::optimise(
        &linked.wasm,
        // The module cut and the Splash blanking are off: a film built with
        // them drew nothing (H16 found the post-link stripping at fault);
        // they come back once each is shown safe on its own.
        None,
    ).map_err(|e| fail(&mut diags, e, progress))?;
    if let Some(symbols) = symbols {
        let _ = std::fs::write(opts.build_dir.join("film.symbols"), symbols);
    }
    // The UI kit's Splash the film never reached, blanked in place (checked
    // by the glyph run above).
    if false && !analysis.splash_blank.is_empty() {
        match makepad_platform::splash_blank::BlankList::read(&analysis.splash_blank)
            .and_then(|list| makepad_web_pack::splash_blank::apply(&mut optimised, &list.pairs()))
        {
            Ok(r) => progress.diagnostic(&Diagnostic::note(Phase::Optimise, format!("unused UI-kit Splash blanked: {} blocks, {} bytes", r.texts, r.bytes_blanked))),
            Err(e) => progress.diagnostic(&Diagnostic::warning(Phase::Optimise, format!("the UI kit's unused Splash stays: {e}"))),
        }
    }
    let _ = std::fs::write(opts.build_dir.join("optimise.txt"), &optimise_report);
    progress.phase(Phase::Optimise, 0.5, "brotli");
    let wasm_br = report::brotli(&optimised);
    progress.phase(Phase::Optimise, 1.0, &format!("film.wasm {} ({} brotli)", report::mb(optimised.len()), report::mb(wasm_br.len())));
    check(progress, Phase::Optimise)?;

    // 8. Write.
    let credited = fonts::credited(&shipped_fonts);
    let credits = page::credits_text(&opts.credits, &credited);
    let notices = page::notices_text(&credited);
    let stripped: Vec<&str> = opts.pack.js_stripped.iter().map(|s| s.as_str()).collect();
    let js = page::film_js(&opts.source_root, &stripped).map_err(|e| fail(&mut diags, e, progress))?;
    let mut out = makepad_web_pack::output::Output::new(&opts.out_dir, &page::OUTPUT_FILES).map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Write, e), progress))?;
    let song_file = if opts.include_music { song_asset.as_ref().and_then(|a| bundle.entries.get(a)).map(|e| e.file.clone()) } else { None };
    out.write_sized("film.wasm", &optimised, Some(wasm_br.len())).map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Write, e), progress))?;
    out.write("film.wasm.br", &wasm_br).map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Write, e), progress))?;
    out.write("film.js", js.as_bytes()).map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Write, e), progress))?;
    out.write("index.html", page::index_html(&bundle, opts.credits.trim(), &notices, optimised.len(), wasm_br.len()).as_bytes()).map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Write, e), progress))?;
    out.write("CREDITS.txt", credits.as_bytes()).map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Write, e), progress))?;
    out.write("README.txt", page::readme(song_file.as_deref()).as_bytes()).map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Write, e), progress))?;
    for (name, bytes) in &beside {
        out.write(name, bytes).map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Write, e), progress))?;
    }
    out.prune().map_err(|e| fail(&mut diags, Diagnostic::error(Phase::Write, e), progress))?;
    progress.phase(Phase::Write, 1.0, &opts.out_dir.display().to_string());

    // 9. Safety: a film that could take the browser down is not published.
    let has_music = song_file.is_some();
    let mut plates: Vec<f64> = bundle.plates.iter().map(|p| p.start).collect();
    plates.sort_by(|a, b| a.total_cmp(b));
    let film = safety::Film { dir: &opts.out_dir, duration: bundle.duration, plates, has_music, shaders: (!analysis.shaders.is_empty()).then_some(&analysis.shaders[..]) };
    let gate = safety::gate(&film, &opts.safety, &opts.build_dir, progress);
    if let Some(why) = &gate.skipped {
        let d = Diagnostic::warning(Phase::Safety, format!("not checked for browser safety: {why}"));
        progress.diagnostic(&d);
        diags.push(d);
    }
    let details = gate.details();
    if !details.is_empty() {
        let _ = std::fs::write(opts.build_dir.join("safety.txt"), format!("{}\n{details}", gate.summary()));
    }
    for note in &gate.notes {
        let d = Diagnostic::note(Phase::Safety, note.clone());
        progress.diagnostic(&d);
        diags.push(d);
    }
    if !gate.failures.is_empty() {
        // Not published: the written files move to the build folder.
        let rejected = opts.build_dir.join("rejected");
        let _ = std::fs::create_dir_all(&rejected);
        for f in out.written() {
            let from = opts.out_dir.join(&f.name);
            if std::fs::rename(&from, rejected.join(&f.name)).is_err() {
                let _ = std::fs::remove_file(&from);
            }
        }
        for why in &gate.failures {
            let d = Diagnostic::error(Phase::Safety, format!("unsafe in a browser: {why}")).help(format!("the film was not published (its files are in {}); {}\n{details}", rejected.display(), gate.summary()));
            progress.diagnostic(&d);
            diags.push(d);
        }
        return Err(diags);
    }
    progress.phase(Phase::Safety, 1.0, &gate.summary());

    let mut report = ExportReport { diagnostics: diags, analysis: analysis.summary.clone(), fonts: shipped_fonts.clone(), pack: opts.pack.clone(), ..Default::default() };
    report.files = out.written().to_vec();
    report.breakdown = report::breakdown(&optimised, &linked, &pack_bytes, &makepad_story_film_pack::Pack::parse(&pack_bytes).map_err(|e| vec![Diagnostic::error(Phase::Write, e)])?);
    Ok(report)
}

/// A bundle error as a diagnostic: `path: message` names the file.
fn resolve_diagnostic(error: &str, edit: &Path) -> Diagnostic {
    let d = Diagnostic::error(Phase::Resolve, error.to_string());
    match error.split_once(": ") {
        Some((file, message)) if Path::new(file).extension().is_some() => Diagnostic { message: message.to_string(), ..d }.at(file, 0, 0),
        _ => d.at(edit.display().to_string(), 0, 0),
    }
}

/// The film's bundle as the export changed it (for tests and tools).
pub fn bundle_of(opts: &ExportOptions) -> Result<Bundle, String> {
    if let Some(dir) = &opts.bundle { return Bundle::read(dir); }
    match &opts.job {
        Some(job) => makepad_story_player_bundle::build_job(job),
        None => makepad_story_player_bundle::build_bundle(&opts.edit, &opts.media),
    }
}

//! `wasm-export`: a makepad app as one trace-shaken wasm page.
//!
//! ```text
//! wasm-export <package> --out <dir> --driver <spec>... [--replay <spec>...] [options]
//! ```
//!
//! The output is exactly `index.html`, `<name>.js` (the platform's web runtime and the page's
//! start) and ONE `<name>.wasm` (raw; the host compresses it on the wire), which carries everything: the code, the crates' Splash, the files the
//! app fetched (its documents' Splash with their unused items blanked, fonts cut to the glyphs
//! drawn, other data), the shader pack. Files under an `--external` URL prefix (a film's song) are
//! copied beside the page and fetched as before.
//!
//! The phases ([`Phase`]), each a run of the trace host (`makepad-web-trace`: the app's web build
//! on Relax's JIT against a virtual browser) or a step on what the runs wrote:
//!
//! 1. **trace**: every driver ([`Driver`]: a deterministic player's query, or a UI-automation
//!    script) once, recording coverage, the Splash used, the glyphs drawn, the shader pack.
//!    Coverage is the union of the runs.
//! 2. **pack**: the runs' shader packs as one, with the browser safety gate's bounded-loop check.
//! 3. **embed**: what the runs fetched, as the embed (`makepad_web_manifest::embed`).
//! 4. **capture**: the drivers again as the page ships, with the pack and the embed compiled in
//!    (`--cfg makepad_precompiled_shaders makepad_shader_pack makepad_embed`: no shader compiler,
//!    no fetches of embedded files); their union drives the final module capture
//!    (`RELAX_WASM_EXPORT`, Relax's RIR -> wasm backend). Entered functions
//!    and their optimized static call/data-pointer closure ship whole by default.
//! 5. **gate** (replay B): the held-out replays (other sizes, another seek order, stress) in the
//!    shipped configuration. Functions must be in the emitted function manifest;
//!    fetched bytes, glyphs and definitions must be in the training union. The
//!    opt-in block mode also requires block coverage. One miss fails the export. The replays
//!    need only the pack and the embed, so they run beside step 4's runs ([`run::Pool`]).
//! 6. **link**: the captured module.
//! 7. **package**: the three files, and the size report ([`report`]).
//! 8. **nodegate**: the packaged HTML/loader/wasm in Node, with each driver's
//!    virtual clock and DOM inputs; every frame must match its trace GL digest.
//!
//! Every step's files are in the build folder (`--build`): runs' logs and coverage, the pack, the
//! embed, the gate's report.

pub mod coverage;
pub mod embed;
pub mod packs;
pub mod page;
pub mod report;
pub mod run;
pub mod shader_loops;
pub mod nodegate;
pub mod tools;
pub mod environment;
pub mod seek_variations;

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// One run of the app: what drives it and how its window is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Driver {
    /// Names the run's files (`<build>/<pass>/<name>.json`).
    pub name: String,
    /// `WxH@dpi`.
    pub window: String,
    /// The page's `location.search` (a player: `?doc=...&deterministic&end=exit`).
    pub query: String,
    /// The run ends at the first console line holding this (`makepad-story-doc-player: exit`).
    pub until: Option<String>,
    /// A UI-automation script (the trace host's `--script` format).
    pub script: Option<PathBuf>,
    /// The run's length in virtual milliseconds (else: `until`, or the script's `end`).
    pub ms: Option<u64>,
    /// The rare-but-required paths: the script heap collects at every safepoint.
    pub stress: bool,
    pub environment: environment::Environment,
}

impl Driver {
    /// `name=<n>;window=<WxH@dpi>;query=<q>;until=<text>;script=<file>;ms=<n>;stress`
    /// (`;`-separated, any order; `window` defaults to 1280x720@1).
    pub fn parse(spec: &str) -> Result<Driver, String> {
        let mut d = Driver { window: "1280x720@1".into(), ..Default::default() };
        for part in spec.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            match key {
                "name" => d.name = value.to_string(),
                "window" => d.window = value.to_string(),
                "query" => d.query = value.to_string(),
                "until" => d.until = Some(value.to_string()),
                "script" => d.script = Some(PathBuf::from(value)),
                "ms" => d.ms = Some(value.parse().map_err(|_| format!("driver `{spec}`: ms={value} is not a number"))?),
                "stress" => d.stress = true,
                "cores" | "gpu" | "clock" | "seed" | "path" => d.environment.set(key, value)?,
                _ => return Err(format!("driver `{spec}`: unknown key `{key}` (name, window, query, until, script, ms, stress)")),
            }
        }
        if d.name.is_empty() {
            return Err(format!("driver `{spec}`: needs name=<n>"));
        }
        if d.until.is_none() && d.script.is_none() && d.ms.is_none() {
            return Err(format!("driver `{}`: needs until=, script= or ms= (when does the run end?)", d.name));
        }
        Ok(d)
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// The app's package (`makepad-story-doc-player`).
    pub package: String,
    /// Where the trace host runs (the checkout of the repository that holds the package, `apps/<repo>`).
    pub package_dir: PathBuf,
    /// The Makepad checkout (the platform's web runtime is read from it).
    pub source_root: PathBuf,
    /// The child host executable (`wasm-export`, or the standalone `makepad-web-trace`). None:
    /// built from `source_root` and found as [`tools`] says.
    pub trace_host: Option<PathBuf>,
    pub out_dir: PathBuf,
    /// The runs' files and the intermediate steps. None: `<target dir>/wasm-export/<title>`
    /// ([`tools::build_dir`]).
    pub build_dir: Option<PathBuf>,
    /// Page folders: `/<prefix>` served from a directory (`/film` = a document's folder).
    pub serves: Vec<(String, PathBuf)>,
    /// URL prefixes (`song/`) not embedded: copied beside the page, fetched by it.
    pub external: Vec<String>,
    /// Raster image URLs permitted by the caller, and still required to be
    /// referenced by a fetched document. JPEGs are never permitted.
    pub allow_images: Vec<String>,
    /// Author credits; shipped font notices and the engine are added.
    pub credits: Option<PathBuf>,
    /// The coverage runs (union).
    pub drivers: Vec<Driver>,
    /// The held-out replays of the gate.
    pub replays: Vec<Driver>,
    /// The page's default query, embedded (a key the page's own URL gives wins).
    pub query: String,
    /// The page's title.
    pub title: String,
    /// The stem of the script and the wasm (`<name>.js`, `<name>.wasm`).
    pub name: String,
    /// Runs at once.
    pub jobs: usize,
    /// Trace host options for the capture and replay runs (the shipped page's configuration).
    pub ship_args: Vec<String>,
    /// Marked sections of the web runtime the page drops (`makepad_web_pack::js::SECTIONS`).
    pub js_stripped: Vec<String>,
    /// Only skip the final gate when Node cannot be found. A failed gate is never skipped.
    pub allow_missing_node: bool,
    /// Smaller, trace-specific code with traps in unobserved blocks. Opt-in only.
    pub block_shaking: bool,
    /// Add deterministic browser environment variations to the training union.
    pub environment_variations: bool,
    /// Add seek syntax coverage from training queries, independently of environments.
    pub seek_input_class_variations: bool,
}

/// The phases, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Trace,
    Pack,
    Embed,
    Capture,
    Gate,
    Link,
    Package,
    Nodegate,
}

impl Options {
    /// The trace host; set once [`export_until`] has resolved it.
    pub(crate) fn host(&self) -> &Path {
        self.trace_host.as_deref().expect("export_until resolves the trace host")
    }

    /// The build folder; set once [`export_until`] has resolved it.
    pub(crate) fn build(&self) -> &Path {
        self.build_dir.as_deref().expect("export_until resolves the build folder")
    }
}

impl Phase {
    pub const ALL: [Phase; 8] = [Phase::Trace, Phase::Pack, Phase::Embed, Phase::Capture, Phase::Gate, Phase::Link, Phase::Package, Phase::Nodegate];

    pub fn name(self) -> &'static str {
        match self {
            Phase::Trace => "trace",
            Phase::Pack => "pack",
            Phase::Embed => "embed",
            Phase::Capture => "capture",
            Phase::Gate => "gate",
            Phase::Link => "link",
            Phase::Package => "package",
            Phase::Nodegate => "nodegate",
        }
    }

    /// For a progress line.
    pub fn title(self) -> &'static str {
        match self {
            Phase::Trace => "Tracing the drivers' runs",
            Phase::Pack => "Merging the shader pack (loop safety check)",
            Phase::Embed => "Embedding files, fonts cut to their glyphs",
            Phase::Capture => "Running as shipped, capturing the wasm",
            Phase::Gate => "Replay gate: held-out runs, zero stub hits",
            Phase::Link => "Linking the captured module",
            Phase::Package => "Writing the page",
            Phase::Nodegate => "Comparing the packaged page with the trace",
        }
    }
}

/// Why the export stopped.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub phase: Phase,
    pub message: String,
    /// The file with the details (a run's log, the gate's report).
    pub details: Option<PathBuf>,
}

impl Failure {
    pub fn new(phase: Phase, message: impl Into<String>) -> Self {
        Failure { phase, message: message.into(), details: None }
    }

    pub fn details(mut self, path: &Path) -> Self {
        self.details = Some(path.to_path_buf());
        self
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[{}]: {}", self.phase.name(), self.message)?;
        if let Some(d) = &self.details {
            write!(f, "\n  --> {}", d.display())?;
        }
        Ok(())
    }
}

/// Where the export reports to (Story's dialog, the CLI's terminal).
pub trait Progress {
    /// `phase` is `done` (0..1) along, with a short note.
    fn phase(&mut self, phase: Phase, done: f64, note: &str);
    /// Something worth knowing that does not stop the export.
    fn note(&mut self, phase: Phase, text: &str);
    /// The export stops at the next check (running traces are stopped).
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

    fn note(&mut self, phase: Phase, text: &str) {
        eprintln!("note[{}]: {}", phase.name(), text);
    }
}

fn check(progress: &dyn Progress, phase: Phase) -> Result<(), Failure> {
    if progress.cancelled() {
        return Err(Failure::new(phase, "cancelled"));
    }
    Ok(())
}

fn abs(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Runs the export; the report of what it wrote.
pub fn export(opts: &Options, progress: &mut dyn Progress) -> Result<report::Report, Failure> {
    export_until(opts, Phase::Nodegate, progress)?.ok_or_else(|| Failure::new(Phase::Nodegate, "export ended before the final gate"))
}

/// Runs the same pipeline through `until`, inclusive. An export that stops before the page is
/// packaged leaves its intermediates in `build_dir` and returns no report; one through package
/// returns the page's report (its gate line without the Node gate's).
pub fn export_until(opts: &Options, until: Phase, progress: &mut dyn Progress) -> Result<Option<report::Report>, Failure> {
    let mut expanded = opts.clone();
    if opts.seek_input_class_variations {
        for driver in seek_variations::drivers(&opts.drivers).map_err(|e| Failure::new(Phase::Trace, e))? {
            progress.note(Phase::Trace, &format!("SEEK-INPUT-CLASS {}: {}", driver.name, driver.query));
            expanded.drivers.push(driver);
        }
    }
    if opts.environment_variations {
        if let Some(base) = opts.drivers.first() {
            for (i, (gpu, cores, window, jitter, path)) in [
                ("nvidia", 1, "1280x800@1", true, "/"),
                ("amd", 16, "1600x900@1", true, "/a/b/film-x/"),
                ("intel", 4, "800x960@1.5", false, "/a/b/film-x/index.html"),
                ("apple", 16, "1280x800@2", true, "/index.html"),
                ("mesa", 1, "720x1000@1.5", true, "/"),
                ("adreno", 4, "390x844@3", true, "/"),
                ("mali", 16, "412x915@2", false, "/index.html"),
            ].into_iter().enumerate() {
                let mut driver = base.clone();
                driver.name = format!("environment-{gpu}");
                driver.window = window.into();
                driver.environment = environment::Environment { cores, gpu: gpu.into(), jitter, seed: i as u32 + 1, path: path.into() };
                expanded.drivers.push(driver);
            }
        }
    }
    let opts = &expanded;
    progress.note(Phase::Trace, if opts.block_shaking {
        "profile: opt-in block shaking; gate requires trained functions and blocks"
    } else {
        "profile: whole functions, optimized call/data-pointer closure; gate requires shipped functions"
    });
    if opts.drivers.is_empty() {
        return Err(Failure::new(Phase::Trace, "no driver: give at least one --driver"));
    }
    if until >= Phase::Gate && opts.replays.is_empty() {
        return Err(Failure::new(Phase::Gate, "no replay: the gate needs at least one held-out --replay"));
    }
    let mut names: Vec<&str> = opts.drivers.iter().chain(&opts.replays).map(|d| d.name.as_str()).collect();
    names.sort_unstable();
    if names.windows(2).any(|w| w[0] == w[1]) {
        return Err(Failure::new(Phase::Trace, "two drivers or replays share a name"));
    }
    // the tools and the build folder (see tools.rs)
    let build = opts.build_dir.clone().unwrap_or_else(|| tools::build_dir(&opts.source_root, &opts.title));
    let trace_host = tools::trace_host(opts, progress)?;
    check(progress, Phase::Trace)?;
    let opts = &Options { trace_host: Some(trace_host), build_dir: Some(build), ..opts.clone() };
    let build = opts.build();
    for dir in ["trace", "training", "capture", "gate", "original-fonts", "link"] {
        std::fs::create_dir_all(build.join(dir)).map_err(|e| Failure::new(Phase::Trace, format!("{}: {e}", build.join(dir).display())))?;
    }
    let build = abs(build);
    let opts = &Options { build_dir: Some(build.clone()), ..opts.clone() };
    std::fs::write(build.join("training-drivers.json"), report::drivers_json(&opts.drivers).to_json())
        .map_err(|e| Failure::new(Phase::Trace, e.to_string()))?;
    let report_path = build.join("report.json");
    if report_path.exists() {
        std::fs::remove_file(&report_path).map_err(|e| Failure::new(Phase::Trace, e.to_string()))?;
    }
    let mut started = Instant::now();
    let mut completed = |phase: Phase, progress: &mut dyn Progress| {
        progress.note(phase, &format!("completed in {:.3} s", started.elapsed().as_secs_f64()));
        started = Instant::now();
        phase == until
    };

    // 1. Trace: coverage, Splash use, glyphs, shader pack.
    let pass1: Vec<run::Job> = opts.drivers.iter().map(|d| {
        let mut job = run::Job::trace(opts, d, &build.join("trace"));
        job.args.splice(1..1, ["--record-pack".to_string(), build.join("trace").join(format!("{}.pack", d.name)).display().to_string()]);
        job
    }).collect();
    run::run_all(opts, &pass1, Phase::Trace, progress)?;
    check(progress, Phase::Trace)?;
    let traced = build.join("trace.json");
    run::merge(opts, &traced, &pass1.iter().map(|j| j.out.clone()).collect::<Vec<_>>(), Phase::Trace)?;
    // a run whose app failed (a document that did not load) fails here, not after every phase
    let facts = coverage::Facts::read(&traced).map_err(|e| Failure::new(Phase::Trace, e).details(&traced))?;
    if !facts.panics.is_empty() || !facts.errors.is_empty() {
        return Err(Failure::new(Phase::Trace, format!("the recording runs {}:\n{}", if facts.panics.is_empty() { "reported errors" } else { "panicked" },
            facts.panics.iter().chain(&facts.errors).cloned().collect::<Vec<_>>().join("\n"))).details(&traced));
    }
    if completed(Phase::Trace, progress) {
        return Ok(None);
    }

    // 2. Pack.
    progress.phase(Phase::Pack, 0.0, "merging");
    let pack_path = build.join("shaders.pack");
    let packs: Vec<PathBuf> = opts.drivers.iter().map(|d| build.join("trace").join(format!("{}.pack", d.name))).collect();
    let pack = packs::merge_to(&pack_path, &packs).map_err(|e| Failure::new(Phase::Pack, e))?;
    let problems = pack.problems();
    if !problems.is_empty() {
        // A loop that does not end hangs the GPU, and the browser's GPU process takes every tab with it.
        return Err(Failure::new(Phase::Pack, format!("the shader pack may not ship (a loop that cannot be shown to end hangs the browser's GPU):\n{}", problems.join("\n"))).details(&pack_path));
    }
    progress.phase(Phase::Pack, 1.0, &pack.summary());
    check(progress, Phase::Pack)?;
    if completed(Phase::Pack, progress) {
        return Ok(None);
    }

    // 3. Embed.
    progress.phase(Phase::Embed, 0.0, "reading the runs' coverage");
    let built = embed::build(opts, &facts, progress)?;
    std::fs::write(build.join("embedded-files.json"), built.inventory_json()).map_err(|e| Failure::new(Phase::Embed, e.to_string()))?;
    let embed_path = build.join("embed.bin");
    std::fs::write(&embed_path, built.embed.write()).map_err(|e| Failure::new(Phase::Embed, format!("{}: {e}", embed_path.display())))?;
    progress.phase(Phase::Embed, 1.0, &built.summary());
    check(progress, Phase::Embed)?;
    if completed(Phase::Embed, progress) {
        return Ok(None);
    }

    // 4. Train again as shipped: embed/pack change the code paths and block
    // positions, so the recording build's function coverage is not interchangeable.
    let module_prefix = format!("{}.", opts.name);
    let ship = |job: &mut run::Job, capture: bool| {
        job.args.splice(1..1, opts.ship_args.iter().cloned());
        // makepad_no_studio: a page has no Studio connection (the rustc film exporter's flag too)
        for cfg in ["makepad_precompiled_shaders", "makepad_shader_pack", "makepad_embed", "makepad_no_studio"] {
            job.args.splice(1..1, ["--cfg".to_string(), cfg.to_string()]);
        }
        job.env.push(("MAKEPAD_SHADER_PACK".into(), pack_path.display().to_string()));
        job.env.push(("MAKEPAD_EMBED".into(), embed_path.display().to_string()));
        if capture {
            job.env.push(("RELAX_WASM_EXPORT".into(), build.join("link").join(format!("{module_prefix}%p.wasm")).display().to_string()));
            job.env.push(("RELAX_WASM_EXPORT_BLOCKS".into(), if opts.block_shaking { "1" } else { "0" }.into()));
            job.env.push(("RELAX_WASM_EXPORT_FUNCTIONS".into(), build.join("shipped-functions.txt").display().to_string()));
        }
    };
    let training: Vec<run::Job> = opts.drivers.iter().map(|d| {
        let mut job = run::Job::trace(opts, d, &build.join("training"));
        ship(&mut job, false);
        job
    }).collect();
    let mut pool = run::Pool::new(opts);
    let training_ids = pool.queue(training.clone(), Phase::Capture);
    // The gate's held-out replays need only the pack and the embed: queued now, they run in the
    // slots the training and the capture leave free (the capture is one run). The gate waits
    // for them after the capture.
    let replays: Vec<run::Job> = opts.replays.iter().map(|d| {
        let mut job = run::Job::trace(opts, d, &build.join("gate"));
        ship(&mut job, false);
        job
    }).collect();
    // B with full fonts is an independent oracle: neither it nor the subset's GSUB closure
    // may enlarge the drawn training set. Otherwise a removed cmap entry could hide a miss.
    let original_embed_path = build.join("original-fonts.bin");
    let originals: Vec<run::Job> = opts.replays.iter().map(|d| {
        let mut job = run::Job::trace(opts, d, &build.join("original-fonts"));
        ship(&mut job, false);
        for (key, value) in &mut job.env {
            if key == "MAKEPAD_EMBED" {
                *value = original_embed_path.display().to_string();
            }
        }
        job
    }).collect();
    let gate_ids = if until >= Phase::Gate {
        let mut original_embed = built.embed.clone();
        for (url, body) in &mut original_embed.files {
            if let Some((_, original)) = built.original_fonts.iter().find(|(u, _)| u == url) {
                *body = original.clone();
            }
        }
        std::fs::write(&original_embed_path, original_embed.write()).map_err(|e| Failure::new(Phase::Gate, e.to_string()))?;
        pool.queue(replays.iter().chain(&originals).cloned().collect(), Phase::Gate)
    } else {
        Vec::new()
    };
    pool.wait(&training_ids, Phase::Capture, progress)?;
    let trained = build.join("training.json");
    run::merge(opts, &trained, &training.iter().map(|j| j.out.clone()).collect::<Vec<_>>(), Phase::Capture)?;
    let primary = &opts.drivers[0];
    let mut capture = run::Job::trace(opts, primary, &build.join("capture"));
    ship(&mut capture, true);
    capture.env.push(("RELAX_WASM_EXPORT_UNION".into(), trained.display().to_string()));
    // stale modules of an earlier export are not this one's
    run::remove_modules(&build.join("link"), &module_prefix);
    let capture_ids = pool.queue(vec![capture.clone()], Phase::Capture);
    pool.wait(&capture_ids, Phase::Capture, progress)?;
    check(progress, Phase::Capture)?;
    // Missing/invalid union coverage must fail here, even for --until capture.
    let wasm = run::captured_module(&build.join("link"), &module_prefix, &capture.log)?;
    let captured = build.join("capture.json");
    run::merge(opts, &captured, &[trained, capture.out.clone()], Phase::Capture)?;
    if completed(Phase::Capture, progress) {
        return Ok(None);
    }

    // 5. Gate: held-out replays as shipped, against the complete training union.
    pool.wait(&gate_ids, Phase::Gate, progress)?;
    drop(pool);
    check(progress, Phase::Gate)?;
    let replayed = build.join("replay.json");
    run::merge(opts, &replayed, &replays.iter().map(|j| j.out.clone()).collect::<Vec<_>>(), Phase::Gate)?;
    let original_replay = build.join("original-fonts.json");
    run::merge(opts, &original_replay, &originals.iter().map(|j| j.out.clone()).collect::<Vec<_>>(), Phase::Gate)?;
    let read = |path: &Path| coverage::Facts::read(path).map_err(|e| Failure::new(Phase::Gate, e).details(path));
    let font_gate = coverage::gate_fonts(&facts, &read(&original_replay)?, &read(&replayed)?);
    let font_report = build.join("glyph-gate.txt");
    let _ = std::fs::write(&font_report, match &font_gate { Ok(s) | Err(s) => s });
    progress.note(Phase::Gate, &font_gate.map_err(|e| Failure::new(Phase::Gate, e).details(&font_report))?);
    let gate = run::gate(opts, &captured, &replayed, &build.join("gate.txt"))?;
    // a panic is a trap in the shipped page
    for (what, path) in [("the training and capture runs", &captured), ("the replays", &replayed), ("the original-font replays", &original_replay)] {
        let facts = coverage::Facts::read(path).map_err(|e| Failure::new(Phase::Gate, e).details(path))?;
        if !facts.panics.is_empty() {
            return Err(Failure::new(Phase::Gate, format!("{what} panicked:\n{}", facts.panics.join("\n"))).details(path));
        }
        if !facts.errors.is_empty() {
            return Err(Failure::new(Phase::Gate, format!("{what} reported errors:\n{}", facts.errors.join("\n"))).details(path));
        }
    }
    progress.phase(Phase::Gate, 1.0, &gate);
    if completed(Phase::Gate, progress) {
        return Ok(None);
    }

    // 6. Link.
    progress.phase(Phase::Link, 0.0, "finding the captured module");
    progress.phase(Phase::Link, 1.0, &format!("{} bytes", wasm.len()));
    if completed(Phase::Link, progress) {
        return Ok(None);
    }

    // 7. Package.
    let shipped = coverage::Facts::read(&captured).map_err(|e| Failure::new(Phase::Package, e).details(&captured))?;
    let mut report = page::write(opts, &wasm, &built, &pack, &shipped, &gate, progress)?;
    progress.phase(Phase::Package, 1.0, &opts.out_dir.display().to_string());
    if completed(Phase::Package, progress) {
        return Ok(Some(report));
    }
    let result = nodegate::run(opts, progress)?;
    report.gate.push_str(&format!("; nodegate: {result}"));
    std::fs::write(&report_path, report.to_json()).map_err(|e| Failure::new(Phase::Nodegate, e.to_string()))?;
    completed(Phase::Nodegate, progress);
    Ok(Some(report))
}

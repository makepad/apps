//! `wasm-export`: the CLI over the library (see lib.rs for the phases).
//!
//! ```text
//! wasm-export <package> --out <dir> --driver <spec>... --replay <spec>...
//!     [--serve /<prefix>=<dir>]... [--external <url prefix>]... [--query <default query>]
//!     [--title <t>] [--name <stem>] [--jobs <n>] [--build <dir>] [--ship-arg <trace host option>]...
//!     [--playback] [--js-strip <section>]... [--trace-host <bin>] [--source-root <dir>] [--package-dir <dir>]
//!     [--until trace|pack|embed|capture|gate|link|package|nodegate] [--allow-missing-node]
//!
//! For closure byte attribution, set `RELAX_WASM_EXPORT_FNLIST=<prefix>`
//! and run `node tools/wasm-export/analyze-closure.js <linked.wasm> <prefix>`.
//! It reports exact encoded body bytes and independent br11 groups, including
//! code-materialized versus data-only vtables and overlapping generic instances.
//!     [--script <file>]  // wide, compact and narrow desktop training; held-out replay
//!     [--credits <file>] [--allow-image <document-referenced URL>]...
//!     [--block-shaking] [--no-environment-variations] [--no-seek-input-class-variations]
//! wasm-export trace|merge|gate <trace host arguments>
//! ```
//! A driver spec is `name=<n>;window=<WxH@dpi>;query=<q>;until=<text>;script=<file>;ms=<n>;stress`.
//! Environment keys: `cores=16;gpu=amd;clock=jitter;seed=1;path=/`.
//! Whole functions and their static closure ship by default. Block shaking is
//! a smaller trace-specific mode; environment variations are enabled in either mode.
//! Seek-input-class variations independently add one run per distinct training seek
//! query, with seconds and percent shares derived from its targets in reverse order.
//! `--no-seek-input-class-variations` opts out; `--no-environment-variations` does not.
//! `--playback` drops the web runtime's input, clipboard, storage and other sections a playback
//! page never uses (`makepad_web_pack::js::PLAYBACK_JS_STRIPPED`).
//!
//! A film folder with an optional song, from the checkout of the repository that holds the
//! exported package (`apps/<repo>` of a Makepad checkout):
//! ```text
//! wasm-export makepad-story-doc-player --out ~/film-web --name player --title "Film" --playback --jobs 2 \
//!   --serve /film=stage/content/<film> --serve /fonts=~/.makepad/weights/fonts --serve /song=<song dir> --external song/ \
//!   --query "doc=film/<film>.cut.splash&fill=media/song.mp3=song.mp3" \
//!   --driver "name=a1;window=1280x720@1;until=makepad-story-doc-player: exit;query=?doc=film/<film>.cut.splash&deterministic&shutter-max=4&fill=media/song.mp3=song/song.mp3" \
//!   --driver "name=a2;window=800x450@2;until=makepad-story-doc-player: exit;query=..." \
//!   --driver "name=s1;window=1920x1080@1;stress;until=makepad-story-doc-player: exit;query=..." \
//!   --replay "name=b1;window=1600x900@1.5;stress;until=makepad-story-doc-player: exit;query=...&seek=120,30,90,5,150,60"
//! ```
//! Exit status: 0 written, 1 the export failed (the message names the phase and its file), 2 usage.

use makepad_wasm_export::{export_until, Driver, Options, Phase, StderrProgress};
use std::path::{Path, PathBuf};

fn usage(why: &str) -> ! {
    eprintln!("wasm-export: {why}\nusage: wasm-export <package> --out <dir> --driver <spec>... --replay <spec>... [options] (see the crate's main.rs)");
    std::process::exit(2)
}

fn home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

/// The Makepad checkout above `dir` (it holds the platform's web runtime).
fn find_source_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|d| d.join("platform/src/os/web/web.js").is_file()).map(Path::to_path_buf)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The host also runs its build scripts through this executable. Preserve the
    // loader's command line for those calls; export invocations identify their output.
    if matches!(args.first().map(String::as_str), Some("trace" | "merge" | "gate"))
        || !args.iter().any(|a| a == "--out")
    {
        makepad_web_trace::main();
        return;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut package = None;
    let mut out = None;
    let mut build = None;
    let mut trace_host = None;
    let mut source_root = None;
    let mut package_dir = None;
    let mut opts_title = String::new();
    let mut allow_images = Vec::new();
    let mut credits = None;
    let mut name = "app".to_string();
    let mut query = String::new();
    let mut jobs = 1usize;
    let mut serves = Vec::new();
    let mut external = Vec::new();
    let mut drivers = Vec::new();
    let mut replays = Vec::new();
    let mut script = None;
    let mut ship_args = vec!["--no-splash-records".to_string()];
    let mut until = Phase::Nodegate;
    let mut allow_missing_node = false;
    let mut block_shaking = false;
    let mut environment_variations = true;
    let mut seek_input_class_variations = true;
    let mut js_stripped: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--block-shaking" { block_shaking = true; i += 1; continue; }
        if a == "--no-environment-variations" { environment_variations = false; i += 1; continue; }
        if a == "--no-seek-input-class-variations" { seek_input_class_variations = false; i += 1; continue; }
        if !a.starts_with("--") {
            if package.replace(a.to_string()).is_some() {
                usage("one package");
            }
            i += 1;
            continue;
        }
        if a == "--allow-missing-node" {
            allow_missing_node = true;
            i += 1;
            continue;
        }
        if a == "--playback" {
            js_stripped.extend(makepad_web_pack::js::PLAYBACK_JS_STRIPPED.iter().map(|s| s.to_string()));
            i += 1;
            continue;
        }
        let Some(v) = args.get(i + 1).cloned() else { usage(&format!("{a} needs a value")) };
        match a {
            "--out" => out = Some(home(&v)),
            "--build" => build = Some(home(&v)),
            "--until" => until = Phase::ALL.into_iter().find(|p| p.name() == v).unwrap_or_else(|| usage("--until trace|pack|embed|capture|gate|link|package|nodegate")),
            "--trace-host" => trace_host = Some(home(&v)),
            "--source-root" => source_root = Some(home(&v)),
            "--package-dir" => package_dir = Some(home(&v)),
            "--title" => opts_title = v,
            "--name" => name = v,
            "--query" => query = v,
            "--jobs" => jobs = v.parse().unwrap_or_else(|_| usage("--jobs <n>")),
            "--serve" => {
                let (prefix, dir) = v.split_once('=').unwrap_or_else(|| usage("--serve /<prefix>=<dir>"));
                let dir = std::fs::canonicalize(home(dir)).unwrap_or_else(|e| usage(&format!("--serve {dir}: {e}")));
                serves.push((prefix.to_string(), dir));
            }
            "--external" => external.push(v),
            "--allow-image" => allow_images.push(v),
            "--credits" => credits = Some(home(&v)),
            "--driver" => drivers.push(Driver::parse(&v).unwrap_or_else(|e| usage(&e))),
            "--replay" => replays.push(Driver::parse(&v).unwrap_or_else(|e| usage(&e))),
            "--script" => script = Some(home(&v)),
            "--ship-arg" => ship_args.push(v),
            "--js-strip" => js_stripped.push(v),
            o => usage(&format!("unknown option {o}")),
        }
        i += 2;
    }
    let package = package.unwrap_or_else(|| usage("no package"));
    if let Some(script) = script {
        if !drivers.is_empty() || !replays.is_empty() {
            usage("--script supplies the default driver set; use script= in explicit --driver/--replay specs to customise it");
        }
        for (name, window) in [("a1", "1280x800@1"), ("a2", "400x640@2"), ("a3", "800x960@1.5"), ("b1", "720x1000@1.5")] {
            let driver = Driver { name: name.into(), window: window.into(), script: Some(script.clone()), ..Default::default() };
            if name == "b1" { replays.push(driver); } else { drivers.push(driver); }
        }
    }
    let out = out.unwrap_or_else(|| usage("--out <dir>"));
    let package_dir = package_dir.unwrap_or_else(|| cwd.clone());
    let source_root = source_root.or_else(|| find_source_root(&package_dir)).unwrap_or_else(|| usage("no Makepad checkout above the package dir: --source-root <dir>"));
    // this executable is the trace host (`--trace-host` another one); the build folder, unless
    // `--build`, is `<target dir>/wasm-export/<title>` (makepad_wasm_export::tools)
    let trace_host = trace_host.or_else(|| std::env::current_exe().ok());
    let opts = Options {
        title: if opts_title.is_empty() { package.clone() } else { opts_title },
        package,
        package_dir,
        source_root,
        trace_host,
        out_dir: out,
        build_dir: build,
        serves,
        external,
        allow_images,
        credits,
        drivers,
        replays,
        query,
        name,
        jobs,
        ship_args,
        js_stripped,
        allow_missing_node,
        block_shaking,
        environment_variations,
        seek_input_class_variations,
    };
    match export_until(&opts, until, &mut StderrProgress) {
        Ok(Some(report)) => {
            print!("{}", report.text());
        }
        Ok(None) => println!("completed through {}; intermediates: {}", until.name(), opts.build_dir.as_ref().map_or_else(|| makepad_wasm_export::tools::build_dir(&opts.source_root, &opts.title), |b| b.clone()).display()),
        Err(failure) => {
            eprintln!("{failure}");
            std::process::exit(1);
        }
    }
}

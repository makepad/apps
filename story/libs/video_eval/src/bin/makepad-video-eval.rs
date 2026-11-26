//! `makepad-video-eval`: run the video benchmark and score the results.
//!
//! ```text
//! makepad-video-eval list [--json]
//! makepad-video-eval run --generator CMD --model BACKEND:MODEL [--cases IDS] [--repeat N] [--label L]
//! makepad-video-eval score --case ID --edit CUT.splash [--motion M.splash]... [--label L]
//! makepad-video-eval rescore RUN_DIR
//! makepad-video-eval judge --case ID IMAGE.png...
//! makepad-video-eval richness RUN_DIR
//! makepad-video-eval report RUN_DIR
//! makepad-video-eval compare A/results.json B/results.json
//! ```
//! Exit status: 0 done, 1 some case failed, 2 anything else.

use makepad_micro_serde::*;
use makepad_story_video_eval::bench::{self, Case};
use makepad_story_video_eval::contract::GenResult;
use makepad_story_video_eval::eval::{self, Ctx, GenInfo};
use makepad_story_video_eval::judge::Rubric;
use makepad_story_video_eval::report::{self, RunResults};
use makepad_story_video_eval::tools::{self, CutTools};
use std::path::{Path, PathBuf};
use std::time::Duration;

const USAGE: &str = "usage:
  makepad-video-eval list [--json]
  makepad-video-eval run --generator CMD --model BACKEND:MODEL [--cases IDS|CATEGORY|acceptance] [--repeat N]
                         [--label L] [--out DIR] [--max-iterations N] [--gen-timeout S] [--max-load L]
                         [--rerun-failed] [--song AUDIO --lyrics TEXT] [--keep-library] [--library-source DIR] [COMMON]
  makepad-video-eval score --case ID --edit CUT.splash [--motion M.splash]... [--label L] [--out DIR] [COMMON]
  makepad-video-eval rescore RUN_DIR [COMMON]      measure and judge a run's cases again
  makepad-video-eval judge --case ID IMAGE.png...  judge pictures made elsewhere
  makepad-video-eval richness RUN_DIR [COMMON]     add the separate visual-richness review to a run
  makepad-video-eval recheck RUN_DIR               re-measure the checks from the files (no generation, no judge)
  makepad-video-eval reaccept RUN_DIR [--turns T,..] [COMMON]  redo an acceptance run's evidence and checklist
                                                   (no generation; --turns overview,cuts,montage limits the re-judging)
  makepad-video-eval report RUN_DIR                rewrite results.md and gallery.html
  makepad-video-eval compare A/results.json B/results.json
COMMON: [--judge claude-cli[:MODEL]|none] [--rubric FILE] [--edit-tools BIN|none] [--no-render]
        [--strip-count N] [--tool-timeout S] [--judge-timeout S]
The generator command gets {request} {out} {result} {model} substituted (see the crate docs);
`--generator stage` is Stage's headless Generate (target/release/makepad-stage, hidden), on an isolated
copy-on-write clone of local/asset-library ({library} = <out>/.library), never the person's library.
Defaults: judge claude-cli:claude-opus-5-5 (a judge needs --rubric FILE), edit tools
target/release/makepad-edit-tools, out local/video-eval/<time>-<label>.";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse() -> Result<Args, String> {
        let takes_value = [
            "--generator", "--model", "--cases", "--max-load", "--repeat", "--label", "--out", "--max-iterations", "--gen-timeout", "--case", "--edit",
            "--motion", "--song", "--lyrics", "--turns", "--library-source", "--judge", "--rubric", "--edit-tools", "--strip-count", "--tool-timeout", "--judge-timeout",
        ];
        let switches = ["--json", "--no-render", "--rerun-failed", "--keep-library", "--help", "-h"];
        let mut args = Args { positional: Vec::new(), flags: Vec::new() };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            if let Some((k, v)) = arg.split_once('=').filter(|_| arg.starts_with("--")) {
                if !takes_value.contains(&k) {
                    return Err(format!("unknown option {k}\n{USAGE}"));
                }
                args.flags.push((k.to_string(), Some(v.to_string())));
            } else if takes_value.contains(&arg.as_str()) {
                let v = it.next().ok_or_else(|| format!("{arg} needs a value"))?;
                args.flags.push((arg, Some(v)));
            } else if switches.contains(&arg.as_str()) {
                args.flags.push((arg, None));
            } else if arg.starts_with('-') {
                return Err(format!("unknown option {arg}\n{USAGE}"));
            } else {
                args.positional.push(arg);
            }
        }
        Ok(args)
    }
    fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == k)
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(f, _)| f == k).and_then(|(_, v)| v.as_deref())
    }
    fn all(&self, k: &str) -> Vec<&str> {
        self.flags.iter().filter(|(f, _)| f == k).filter_map(|(_, v)| v.as_deref()).collect()
    }
    fn need(&self, k: &str) -> Result<&str, String> {
        self.get(k).ok_or_else(|| format!("{k} is required\n{USAGE}"))
    }
    fn num<T: std::str::FromStr>(&self, k: &str, default: T) -> Result<T, String> {
        self.get(k).map_or(Ok(default), |v| v.parse().map_err(|_| format!("{k}: not a number: {v}")))
    }
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            2
        }
    };
    std::process::exit(code);
}

fn root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().ok();
    tools::repo_root(&cwd)
        .or_else(|| exe.as_deref().and_then(tools::repo_root))
        .ok_or_else(|| "could not find a Makepad checkout containing both makepad.splash and platform/Cargo.toml above the working directory or executable".to_string())
}

fn ctx(args: &Args, root: &Path) -> Result<Ctx, String> {
    let tools = match args.get("--edit-tools") {
        Some("none") => None,
        other => {
            let exe = other.map(PathBuf::from).unwrap_or_else(|| tools::release_bin(root, "makepad-edit-tools"));
            if !exe.is_file() {
                return Err(format!(
                    "{} not found: build it (cargo build --release -p makepad-story-edit-tools) or pass --edit-tools none",
                    exe.display()
                ));
            }
            Some(CutTools { exe, timeout: Duration::from_secs(args.num("--tool-timeout", 600u64)?) })
        }
    };
    let judge = match args.get("--judge").unwrap_or("claude-cli:claude-opus-5-5") {
        "none" => None,
        j => Some(j.to_string()),
    };
    let rubric = match args.get("--rubric").map(PathBuf::from) {
        Some(rubric_path) => match std::fs::read_to_string(&rubric_path) {
            Ok(text) => Some(Rubric::parse(&text)),
            Err(_) if judge.is_none() => None,
            Err(e) => return Err(format!("rubric {}: {e} (pass --rubric FILE or --judge none)", rubric_path.display())),
        },
        None if judge.is_none() => None,
        None => return Err("the judge needs a rubric: pass --rubric FILE (or --judge none)".into()),
    };
    Ok(Ctx {
        tools,
        judge,
        rubric,
        judge_timeout: Duration::from_secs(args.num("--judge-timeout", 600u64)?),
        render: !args.has("--no-render"),
        strip_count: args.num("--strip-count", 8u32)?,
    })
}

/// The machine's 1-minute load average (macOS/BSD `sysctl`, else Linux
/// `/proc/loadavg`).
fn load_average() -> Option<f64> {
    let text = std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .or_else(|| std::fs::read_to_string("/proc/loadavg").ok())?;
    text.split_whitespace().find_map(|w| w.trim_matches(|c| c == '{' || c == '}').parse().ok())
}

/// With `--max-load L`, start the next case only once the machine's load
/// is below L, so an evaluation never piles onto a busy machine.
fn wait_for_load(max_load: f64) {
    if max_load <= 0.0 {
        return;
    }
    let mut told = false;
    while let Some(load) = load_average().filter(|l| *l > max_load) {
        if !told {
            eprintln!("load {load:.1} > {max_load}: waiting before the next case");
            told = true;
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn out_dir(args: &Args, root: &Path, label: &str) -> PathBuf {
    args.get("--out").map(PathBuf::from).unwrap_or_else(|| {
        let slug: String = label.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '-' }).collect();
        root.join("local/video-eval").join(format!("{}-{}", now(), if slug.is_empty() { "run" } else { &slug }))
    })
}

fn write_run(dir: &Path, run: &RunResults) -> Result<(), String> {
    std::fs::write(dir.join("results.json"), run.serialize_json_pretty()).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("results.md"), report::markdown(run)).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("gallery.html"), report::gallery(run)).map_err(|e| e.to_string())
}

fn load_run(path: &Path) -> Result<RunResults, String> {
    let file = if path.is_dir() { path.join("results.json") } else { path.to_path_buf() };
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    RunResults::deserialize_json_lenient(&text).map_err(|e| format!("{}: {e:?}", file.display()))
}

fn case_dir(run_dir: &Path, case: &Case, repeat: u32) -> PathBuf {
    if repeat == 0 {
        run_dir.join(&case.id)
    } else {
        run_dir.join(format!("{}-r{}", case.id, repeat + 1))
    }
}

fn finish(dir: &Path, run: &RunResults) -> Result<i32, String> {
    write_run(dir, run)?;
    print!("{}", report::markdown(run));
    println!("\nresults: {}\ngallery: {}", dir.join("results.md").display(), dir.join("gallery.html").display());
    Ok(if run.results.iter().all(|r| r.success) { 0 } else { 1 })
}

fn run() -> Result<i32, String> {
    let args = Args::parse()?;
    if args.has("--help") || args.has("-h") || args.positional.is_empty() {
        println!("{USAGE}");
        return Ok(0);
    }
    match args.positional[0].as_str() {
        "list" => {
            let cases = bench::all_cases();
            if args.has("--json") {
                println!("{}", cases.serialize_json_pretty());
            } else {
                for c in &cases {
                    println!("{:<28} {:<10} {:<13} {:<5} {:>4.0} s  {}", c.id, c.set, c.category, c.aspect_label(), c.length_s, c.prompt);
                }
            }
            Ok(0)
        }
        "run" => {
            let root = root()?;
            let generator = match args.need("--generator")? {
                // Stage's own headless Generate (M3): hidden, muted, exits
                // when done. Each case gets its own empty draft and
                // storyboard folders, so no run reopens the previous run's
                // edit or board (the library stays shared).
                // Its library is the run's isolated clone ({library}).
                "stage" => format!(
                    "env MAKEPAD_ASSET_LIBRARY={{library}} STAGE_EDIT_DRAFTS={{out}}/.drafts STAGE_STORYBOARDS={{out}}/.boards STAGE_GENERATE_REQUEST={{request}} MAKEPAD_HIDE_WINDOWS=1 MAKEPAD_MUTE=1 {}",
                    tools::release_bin(&root, "makepad-stage").display()
                ),
                other => other.to_string(),
            };
            let model = args.need("--model")?.to_string();
            let label = args.get("--label").unwrap_or("").to_string();
            let cases = bench::select(args.get("--cases").unwrap_or("all"))?;
            let repeat = args.num("--repeat", 1u32)?.max(1);
            let max_iterations = args.num("--max-iterations", 8u32)?;
            let gen_timeout = Duration::from_secs(args.num("--gen-timeout", 1800u64)?);
            let max_load: f64 = args.num("--max-load", 0.0)?;
            // A song brief's inputs, local files never committed.
            let song = match (args.get("--song"), args.get("--lyrics")) {
                (Some(a), Some(l)) => Some((
                    std::fs::canonicalize(a).map_err(|e| format!("--song {a}: {e}"))?,
                    std::fs::canonicalize(l).map_err(|e| format!("--lyrics {l}: {e}"))?,
                )),
                (None, None) => None,
                _ => return Err("--song and --lyrics go together".into()),
            };
            let ctx = ctx(&args, &root)?;
            let dir = out_dir(&args, &root, &label);
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            // Absolute: the generator runs from the checkout, not from here.
            let dir = std::fs::canonicalize(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            // The run's own library: a copy-on-write clone of the person's,
            // made once per run (a rerun reuses it) and removed at the end
            // unless --keep-library. Generations never touch the person's
            // library or its server.
            let library = dir.join(".library");
            if generator.contains("{library}") && !library.join("store/catalog.sqlite3").is_file() {
                let src = args.get("--library-source").map(PathBuf::from).unwrap_or_else(|| root.join("local/asset-library"));
                eprintln!("cloning {} into {} (copy-on-write)", src.display(), library.display());
                tools::clone_library(&src, &library)?;
            }
            // `--rerun-failed`: continue a run in `--out`, keeping its
            // successful rows and generating only the failed or missing ones
            // (a provider outage mid-run, say) into their places.
            let rerun = args.has("--rerun-failed");
            let mut run = if rerun {
                let prev = load_run(&dir)?;
                if prev.model != model || prev.judge != ctx.judge.clone().unwrap_or_default() || prev.rubric_hash != ctx.rubric.as_ref().map_or(String::new(), |r| r.hash.clone()) {
                    return Err(format!("--rerun-failed: {} was made with another model, judge or rubric", dir.display()));
                }
                prev
            } else {
                RunResults {
                    label: label.clone(),
                    model: model.clone(),
                    judge: ctx.judge.clone().unwrap_or_default(),
                    rubric_hash: ctx.rubric.as_ref().map_or(String::new(), |r| r.hash.clone()),
                    richness_hash: ctx.rubric.as_ref().filter(|r| r.richness.is_some()).map(|r| r.richness_hash.clone()),
                    started: now(),
                    generator: generator.clone(),
                    results: Vec::new(),
                }
            };
            for case in &cases {
                for r in 0..repeat {
                    let slot = run.results.iter().position(|x| x.case == case.id && x.repeat == r);
                    if rerun && slot.is_some_and(|i| run.results[i].success) {
                        continue;
                    }
                    wait_for_load(max_load);
                    let cdir = case_dir(&dir, case, r);
                    std::fs::create_dir_all(&cdir).map_err(|e| format!("{}: {e}", cdir.display()))?;
                    eprintln!("[{}] generating {} ({}/{})", case.id, case.aspect_label(), r + 1, repeat);
                    let gen = eval::generate(&generator.replace("{library}", &library.display().to_string()), case, &model, max_iterations, &label, &cdir, &root, gen_timeout, song.as_ref());
                    let result = eval::score(&ctx, case, &dir, &cdir, r, gen);
                    eprintln!("[{}] {} overall {:.0}{}", case.id, if result.success { "ok" } else { "FAILED" }, result.overall, result.error.as_deref().map_or(String::new(), |e| format!(": {e}")));
                    match slot.filter(|_| rerun) {
                        Some(i) => run.results[i] = result,
                        None => run.results.push(result),
                    }
                    write_run(&dir, &run)?;
                }
            }
            if !args.has("--keep-library") && library.ends_with(".library") && library.starts_with(&dir) && library.is_dir() {
                let _ = std::fs::remove_dir_all(&library);
            }
            finish(&dir, &run)
        }
        "score" => {
            let root = root()?;
            let case = bench::select(args.need("--case")?)?.into_iter().next().ok_or("no case")?;
            let edit = std::fs::canonicalize(args.need("--edit")?).map_err(|e| format!("--edit: {e}"))?;
            let motions: Vec<String> = args
                .all("--motion")
                .iter()
                .map(|m| std::fs::canonicalize(m).map(|p| p.display().to_string()).map_err(|e| format!("--motion {m}: {e}")))
                .collect::<Result<_, _>>()?;
            let label = args.get("--label").unwrap_or("manual").to_string();
            let ctx = ctx(&args, &root)?;
            let dir = out_dir(&args, &root, &label);
            let cdir = case_dir(&dir, &case, 0);
            std::fs::create_dir_all(&cdir).map_err(|e| format!("{}: {e}", cdir.display()))?;
            let result = GenResult {
                success: true,
                cut: Some(edit.display().to_string()),
                motions: (!motions.is_empty()).then(|| motions.into_iter().map(|path| makepad_story_video_eval::contract::GenMotion { path, ..Default::default() }).collect()),
                ..Default::default()
            };
            std::fs::write(cdir.join("result.json"), result.serialize_json_pretty()).map_err(|e| e.to_string())?;
            let r = eval::score(&ctx, &case, &dir, &cdir, 0, GenInfo { result, ..Default::default() });
            let run = RunResults {
                label,
                model: "given".into(),
                judge: ctx.judge.clone().unwrap_or_default(),
                rubric_hash: ctx.rubric.as_ref().map_or(String::new(), |r| r.hash.clone()),
                richness_hash: ctx.rubric.as_ref().filter(|r| r.richness.is_some()).map(|r| r.richness_hash.clone()),
                started: now(),
                generator: "none (score)".into(),
                results: vec![r],
            };
            finish(&dir, &run)
        }
        "rescore" => {
            let root = root()?;
            let dir = std::fs::canonicalize(args.positional.get(1).ok_or("rescore needs RUN_DIR")?).map_err(|e| format!("RUN_DIR: {e}"))?;
            let mut run = load_run(&dir)?;
            let ctx = ctx(&args, &root)?;
            run.judge = ctx.judge.clone().unwrap_or_default();
            run.rubric_hash = ctx.rubric.as_ref().map_or(String::new(), |r| r.hash.clone());
            run.richness_hash = ctx.rubric.as_ref().filter(|r| r.richness.is_some()).map(|r| r.richness_hash.clone());
            let all = bench::all_cases();
            let old = std::mem::take(&mut run.results);
            for prev in old {
                let Some(case) = all.iter().find(|c| c.id == prev.case) else {
                    run.results.push(prev);
                    continue;
                };
                let cdir = dir.join(&prev.dir);
                let gen = match std::fs::read_to_string(cdir.join("result.json")).map_err(|e| e.to_string()).and_then(|t| GenResult::parse(&t)) {
                    Ok(result) => GenInfo { error: (!result.success).then(|| prev.error.clone().unwrap_or("generator failed".into())), result, wall_s: prev.wall_s },
                    Err(e) => GenInfo { error: Some(prev.error.clone().unwrap_or(e)), wall_s: prev.wall_s, ..Default::default() },
                };
                eprintln!("[{}] rescoring", case.id);
                run.results.push(eval::score(&ctx, case, &dir, &cdir, prev.repeat, gen));
                write_run(&dir, &run)?;
            }
            finish(&dir, &run)
        }
        "judge" => {
            // Judge pictures made elsewhere (a hand-made strip, D2's library).
            let root = root()?;
            let case = bench::select(args.need("--case")?)?.into_iter().next().ok_or("no case")?;
            let files = &args.positional[1..];
            if files.is_empty() {
                return Err("judge needs one or more PNG files".into());
            }
            let ctx = ctx(&args, &root)?;
            let (Some(judge), Some(rubric)) = (&ctx.judge, &ctx.rubric) else {
                return Err("judge needs a judge and a rubric".into());
            };
            let images: Vec<(String, Vec<u8>)> =
                files.iter().map(|f| std::fs::read(f).map(|b| (f.clone(), b)).map_err(|e| format!("{f}: {e}"))).collect::<Result<_, _>>()?;
            let labels: Vec<String> = images.iter().map(|(l, _)| l.clone()).collect();
            let format = format!("{} {}x{}", case.aspect_label(), case.width, case.height);
            let user = makepad_story_video_eval::judge::brief_prompt(&case.prompt, &format, case.length_s, &labels);
            let (verdict, _) = makepad_story_video_eval::judge::review(judge, rubric, &user, images, ctx.judge_timeout)?;
            println!("{}", verdict.serialize_json_pretty());
            Ok(0)
        }
        "richness" => {
            // Add the separate richness review to a finished run, leaving
            // every other measurement and score as it was.
            let root = root()?;
            let dir = std::fs::canonicalize(args.positional.get(1).ok_or("richness needs RUN_DIR")?).map_err(|e| format!("RUN_DIR: {e}"))?;
            let mut run = load_run(&dir)?;
            let mut ctx = ctx(&args, &root)?;
            ctx.tools = None;
            let (Some(judge), Some(rubric)) = (ctx.judge.clone(), ctx.rubric.as_ref()) else {
                return Err("richness needs a judge and a rubric".into());
            };
            if rubric.richness.is_none() {
                return Err("the rubric has no richness section".into());
            }
            run.richness_hash = Some(rubric.richness_hash.clone());
            let all = bench::all_cases();
            for i in 0..run.results.len() {
                let Some(case) = all.iter().find(|c| c.id == run.results[i].case) else { continue };
                let cdir = dir.join(&run.results[i].dir);
                eprintln!("[{}] richness", case.id);
                eval::judge_richness(&ctx, &judge, rubric, case, &cdir, &mut run.results[i]);
                write_run(&dir, &run)?;
            }
            finish(&dir, &run)
        }
        "reaccept" => {
            // Redo only the acceptance evidence and checklist of a run.
            let root = root()?;
            let dir = std::fs::canonicalize(args.positional.get(1).ok_or("reaccept needs RUN_DIR")?).map_err(|e| format!("RUN_DIR: {e}"))?;
            let mut run = load_run(&dir)?;
            let ctx = ctx(&args, &root)?;
            // `--turns montage,cuts`: re-judge only those turns.
            let turns: Option<Vec<String>> = args.get("--turns").map(|t| t.split(',').map(|s| s.trim().to_string()).collect());
            let all = bench::all_cases();
            for i in 0..run.results.len() {
                let Some(case) = all.iter().find(|c| c.id == run.results[i].case && c.set == "acceptance") else { continue };
                let cdir = dir.join(&run.results[i].dir);
                eprintln!("[{}] reaccept #{}", case.id, run.results[i].repeat + 1);
                eval::reaccept(&ctx, case, &cdir, turns.as_deref(), &mut run.results[i]);
                write_run(&dir, &run)?;
            }
            finish(&dir, &run)
        }
        "recheck" => {
            // Re-measure the checks of a finished run from its files: no
            // generation, no tools, no judge.
            let dir = std::fs::canonicalize(args.positional.get(1).ok_or("recheck needs RUN_DIR")?).map_err(|e| format!("RUN_DIR: {e}"))?;
            let mut run = load_run(&dir)?;
            let all = bench::all_cases();
            for r in run.results.iter_mut() {
                if let Some(case) = all.iter().find(|c| c.id == r.case) {
                    let cdir = dir.join(&r.dir);
                    eval::recheck(case, &cdir, r);
                }
            }
            finish(&dir, &run)
        }
        "report" => {
            let dir = PathBuf::from(args.positional.get(1).ok_or("report needs RUN_DIR")?);
            let run = load_run(&dir)?;
            finish(&dir, &run)
        }
        "compare" => {
            let (a, b) = match (args.positional.get(1), args.positional.get(2)) {
                (Some(a), Some(b)) => (load_run(Path::new(a))?, load_run(Path::new(b))?),
                _ => return Err(format!("compare needs two results.json files\n{USAGE}")),
            };
            print!("{}", report::compare(&a, &b));
            Ok(0)
        }
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

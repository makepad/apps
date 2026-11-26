//! `makepad-app-run`: execute, validate, dump and publish app runs.
//!
//! ```text
//! makepad-app-run RUN.splash --out DIR [--validate] [--no-build] [--thumbnail] [--compare RUN.json]
//! makepad-app-run RUN.splash --out DIR --publish --control HOST:PORT --data HOST:PORT
//!                 --token-file FILE --namespace NS [--asset ASSET_ID]
//! makepad-app-run --dump APP --output WxH@SCALE[/FPS] [--out DIR] [--no-build]
//! makepad-app-run --check RUN.splash
//! makepad-app-run --stale RUN.json --head RUN_REVISION
//! ```
//! Exit status: 0 done, 1 the run failed (or is stale), 2 anything else.

use makepad_story_app_run::*;
use std::path::PathBuf;

const USAGE: &str = "usage:
  makepad-app-run RUN.splash --out DIR [--validate] [--no-build] [--thumbnail] [--compare RUN.json]
  makepad-app-run RUN.splash --out DIR --publish --control HOST:PORT --data HOST:PORT --token-file FILE --namespace NS [--asset ID]
  makepad-app-run --dump APP --output WxH@SCALE[/FPS] [--out DIR] [--no-build]
  makepad-app-run --dump RUN.splash [--out DIR] [--no-build]
  makepad-app-run --check RUN.splash
  makepad-app-run --stale RUN.json --head RUN_REVISION";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse() -> Result<Args, String> {
        let takes_value = ["--out", "--compare", "--control", "--data", "--token-file", "--namespace", "--asset", "--dump", "--output", "--check", "--stale", "--head"];
        let mut args = Args { positional: Vec::new(), flags: Vec::new() };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            if let Some((k, v)) = arg.split_once('=').filter(|_| arg.starts_with("--")) {
                if !takes_value.contains(&k) {
                    return Err(format!("unknown option {k}"));
                }
                args.flags.push((k.to_string(), Some(v.to_string())));
            } else if takes_value.contains(&arg.as_str()) {
                let v = it.next().ok_or_else(|| format!("{arg} needs a value"))?;
                args.flags.push((arg, Some(v)));
            } else if ["--validate", "--no-build", "--publish", "--thumbnail", "--help", "-h"].contains(&arg.as_str()) {
                args.flags.push((arg, None));
            } else if arg.starts_with('-') {
                return Err(format!("unknown option {arg}"));
            } else {
                args.positional.push(arg);
            }
        }
        Ok(args)
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == name)
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.flags.iter().find(|(k, _)| k == name).and_then(|(_, v)| v.as_deref())
    }

    fn need(&self, name: &str) -> Result<&str, String> {
        self.get(name).ok_or_else(|| format!("{name} is required here\n{USAGE}"))
    }
}

/// `WxH@SCALE[/FPS]`, pixels.
fn parse_output(s: &str) -> Result<RunOutput, String> {
    let bad = || format!("--output is WxH@SCALE[/FPS] in pixels, e.g. 1080x1920@3/60, not {s}");
    let (size, fps) = s.split_once('/').map_or((s, "60"), |(a, b)| (a, b));
    let (wh, scale) = size.split_once('@').unwrap_or((size, "1"));
    let (w, h) = wh.split_once('x').ok_or_else(bad)?;
    Ok(RunOutput {
        width: w.parse().map_err(|_| bad())?,
        height: h.parse().map_err(|_| bad())?,
        dpi_scale: scale.parse().map_err(|_| bad())?,
        fps: fps.parse().map_err(|_| bad())?,
    })
}

fn read_json(path: &str) -> Result<makepad_strict_json::Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    makepad_strict_json::parse_depth(text.as_bytes(), 32).map_err(|e| format!("{path}: {e}"))
}

fn main() {
    // A run's `theme` may name one of the extra style sheets.
    makepad_widgets_sheets::register();
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            2
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32, String> {
    let args = Args::parse()?;
    if args.has("--help") || args.has("-h") {
        println!("{USAGE}");
        return Ok(0);
    }
    if let Some(what) = args.get("--dump") {
        let dir = PathBuf::from(args.get("--out").unwrap_or("."));
        let build = !args.has("--no-build");
        // A run file: its app at its output, args, theme and env; else an
        // app id with --output.
        if what.ends_with(".splash") {
            let source = std::fs::read_to_string(what).map_err(|e| format!("{what}: {e}"))?;
            let doc = AppRunDoc::load(&source).map_err(|d| d.iter().map(|d| format!("{what}:{d}")).collect::<Vec<_>>().join("\n"))?;
            print!("{}", app_dump_doc(&doc, build, &dir)?);
        } else {
            let output = parse_output(args.need("--output")?)?;
            let problems = output.problems();
            if !problems.is_empty() {
                return Err(format!("--output: {}", problems.join("; ")));
            }
            print!("{}", app_dump(what, output, build, &dir)?);
        }
        return Ok(0);
    }
    if let Some(path) = args.get("--check") {
        let source = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        return match AppRunDoc::load(&source) {
            Ok(doc) => {
                println!("{path}: `{}` runs `{}` at {}x{} px, {} fps", doc.label, doc.app, doc.output.width, doc.output.height, doc.output.fps);
                Ok(0)
            }
            Err(diags) => {
                for d in diags {
                    eprintln!("{path}:{d}");
                }
                Ok(1)
            }
        };
    }
    if let Some(path) = args.get("--stale") {
        let record = read_json(path)?;
        let stale = stale(&record, args.need("--head")?)?;
        println!("{}", if stale { "stale" } else { "fresh" });
        return Ok(stale as i32);
    }
    let [path] = args.positional.as_slice() else {
        return Err(USAGE.to_string());
    };
    let source = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut opts = RunOptions::new(args.need("--out")?);
    opts.build = !args.has("--no-build");
    opts.validate = args.has("--validate");
    opts.thumbnail = args.has("--publish") || args.has("--thumbnail");
    opts.progress = Box::new(|line| eprintln!("[app-run] {line}"));
    let mut result = execute(&source, opts)?;
    for w in &result.warnings {
        eprintln!("[app-run] review: {w}");
    }
    for f in &result.failures {
        eprintln!("[app-run] FAILED {f}");
    }
    println!(
        "{} frames at {} fps -> {} ({}), marks {:?}",
        result.frames,
        result.output.fps,
        result.mp4.display(),
        result.run_json.display(),
        result.marks()
    );
    let mut code = if result.ok() { 0 } else { 1 };
    if let Some(other) = args.get("--compare") {
        let differences = compare_marks(&result.record, &read_json(other)?);
        for d in &differences {
            eprintln!("[app-run] marks differ: {d}");
        }
        if !differences.is_empty() {
            code = 1;
        }
    }
    if args.has("--publish") && code == 0 {
        let doc = AppRunDoc::load(&source).map_err(|_| "the run file changed")?;
        let token = std::fs::read_to_string(args.need("--token-file")?).map_err(|e| format!("token: {e}"))?;
        let endpoints = makepad_asset_client::ApiEndpoints {
            control: args.need("--control")?.parse().map_err(|e| format!("--control: {e}"))?,
            data: args.need("--data")?.parse().map_err(|e| format!("--data: {e}"))?,
        };
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("no HOME for the asset cache")?;
        let mut config = makepad_asset_client::ClientConfig::new(home.join(".makepad").join("cache").join("app-run"));
        config.token = Some(token.trim().to_string());
        let mut client = makepad_asset_client::AssetClient::connect(config, endpoints, None).map_err(|e| format!("connect: {e:?}"))?;
        // The app run's current head: its script is reused when unchanged.
        let head = match args.get("--asset") {
            Some(id) => {
                let id = id.parse().map_err(|_| format!("--asset: not an asset id: {id}"))?;
                let detail = client.asset_detail(&id).map_err(|e| format!("--asset: {e:?}"))?;
                let head = detail.latest_published().ok_or("--asset has no published revision")?;
                Some(makepad_asset_data::AssetRevisionRef { asset_id: id, revision: head.revision })
            }
            None => None,
        };
        let published = publish_run(&mut client, args.need("--namespace")?, head, &source, &doc.label, &mut result).map_err(|f| match f.run {
            Some(run) => format!("{} (the app run {run} is published; retry with it)", f.message),
            None => f.message,
        })?;
        println!("published app run {} and render {} (alias {:?})", published.run, published.render, published.alias);
    }
    Ok(code)
}

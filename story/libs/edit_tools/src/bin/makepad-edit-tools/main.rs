//! `makepad-edit-tools`: the agent loop over edits and motion documents,
//! for people and AI agents. Every command prints a short
//! human report, or with `--json` one JSON object on stdout.
//!
//! ```text
//! makepad-edit-tools inspect <file> [--safe reels|tiktok|shorts|none]        exit 1 on errors
//! makepad-edit-tools timeline <file>
//! makepad-edit-tools audio analyze <file> [--silence-db -50] [--min-silence 0.5] [--waveform [--buckets n] [--out f.png]]
//! makepad-edit-tools audio at <t|@n> <file> [--bins 32] [--slots 64] [--window 0.05]   what sounds at a moment
//! makepad-edit-tools frame <file> --at <t|@n> [--width px] [--crop x,y,w,h] [--zoom k] [--names] [--out f.png]
//! makepad-edit-tools strip <file> [--every s | --count n] [--from t] [--to t] [--width px] [--columns n] [--out f.png]
//! makepad-edit-tools onion <file> --from t0 --to t1 [--steps n] [--width px] [--out f.png]
//! makepad-edit-tools render <file> [--draft] [--out f.mp4]
//! makepad-edit-tools effect-strip <vjfx/name | kit/name | effect.splash> [--param name=value]... [--count n] [--duration s] [--size WxH]
//! makepad-edit-tools screen-crop <run.json> --aspect 9:19.5 [--zoom min_width_px]   crop keys for a device screen
//!
//! every command: --media <id>=<file> ... | --media-file <list> (default: media.txt beside the file) | --no-media,
//!     --job <render.job> in place of <file>: a cut and every file it reads where they lie (Stage writes it),
//!     --marks <render id>=<run.json> ... (an app run's marker times), --json
//! ```
//!
//! `inspect`, `timeline` and `audio analyze` run without a window. `frame`,
//! `strip`, `onion` and `render` draw on the real GPU in this process's own
//! hidden window, and exit when their parent process goes away. The
//! picture commands also report each frame's exposure (clipped and
//! near-black shares, mean luma, range; for several frames the worst ones)
//! and, for each 3D scene they show, each node's share of the scene's
//! picture and its screen box (from an id map drawn only for these grabs).
//! Exit codes: 0 done, 1 the document has errors or the command failed,
//! 2 usage, 3 the parent went away.

mod host;

use makepad_story_edit_render::LocalMedia;
use makepad_story_edit_tools::audio::{analyze_mix, at, mixdown, waveform, AudioOptions};
use makepad_story_edit_tools::input::{local_media, run_marks, spread, DocKind, Document};
use makepad_story_edit_tools::motion::timeline_motion;
use makepad_story_edit_tools::inspect::{inspect, InspectOptions, InspectReport};
use makepad_story_edit_tools::parse_time;
use makepad_story_edit_tools::timeline::timeline;
use makepad_story_cut::reframe::SafeZone;
use makepad_story_cut::{CutDoc, CutEval};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn usage() -> String {
    "usage: makepad-edit-tools <command> <file> [options] [--json]\n\
     \x20 inspect <file> [--safe reels|tiktok|shorts|none] [--sample-fps n] [--min-text-px n]\n\
     \x20 timeline <file>\n\
     \x20 audio analyze <file> [--silence-db -50] [--min-silence 0.5] [--waveform [--buckets 400] [--out file.png]]\n\
     \x20 audio at <seconds|@frame> <file> [--bins 32] [--slots 64] [--window 0.05]   (sources, gains, ducking, the mix's level, spectrum and waveform there)\n\
     \x20 frame <file> --at <seconds|@frame> [--width px] [--crop x,y,w,h] [--zoom k] [--names] [--out file.png]\n\
     \x20   (--crop in the output's pixels; --zoom renders k times larger; --names outlines each layer of a motion document with its name)\n\
     \x20 strip <file> [--every seconds | --count n] [--from t] [--to t] [--width px] [--columns n] [--out file.png]\n\
     \x20 onion <file> --from t0 --to t1 [--steps n] [--width px] [--out file.png]\n\
     \x20 render <file> [--draft] [--out file.mp4]\n\
     \x20 effect-strip <vjfx/name | kit/name | effect.splash> [--param name=value]... [--count n] [--duration s] [--size WxH] [--width px] [--columns n] [--out file.png]\n\
     \x20 screen-crop <run.json> --aspect w:h [--zoom min_width_px]   (crop keys following an app run's focus, for Device/Seq3D `crop:`)\n\
     media: --media <asset id>=<file> (repeat), --media-file <list>, or media.txt beside <file>; --no-media skips it\n\
     job: --job <render.job> in place of <file>: the cut and every file it reads, where they lie\n\
     app runs: --marks <render asset id>=<run.json> (repeat) resolves marker() times\n\
     times: seconds (2.5), m:ss (1:02.5) or a frame (@45)"
        .into()
}

/// The parsed command line.
struct Args {
    command: String,
    file: PathBuf,
    json: bool,
    flags: HashMap<String, String>,
    switches: Vec<String>,
    media: Vec<String>,
    marks: Vec<String>,
    params: Vec<String>,
}

impl Args {
    fn parse(argv: &[String]) -> Result<Args, String> {
        let mut rest = argv.iter().skip(1).filter(|arg| !arg.starts_with("--remote") && *arg != "--stdin-loop").peekable();
        let mut command = rest.next().ok_or_else(usage)?.clone();
        if command == "audio" {
            match rest.next().map(String::as_str) {
                Some("analyze") => command = "audio analyze".into(),
                Some("at") => command = "audio at".into(),
                _ => return Err(format!("`audio` takes `analyze` or `at <t>`\n{}", usage())),
            }
        }
        let mut flags = HashMap::new();
        if command == "audio at" {
            let t = rest.next().filter(|t| !t.starts_with("--")).ok_or_else(|| format!("`audio at` needs a time: audio at 2.5 <file>\n{}", usage()))?;
            flags.insert("at".to_string(), t.clone());
        }
        if command == "help" || command == "--help" || command == "-h" {
            return Err(usage());
        }
        let mut file = None;
        let mut switches = Vec::new();
        let mut media = Vec::new();
        let mut marks = Vec::new();
        let mut params = Vec::new();
        const SWITCHES: &[&str] = &["--json", "--draft", "--no-media", "--waveform", "--names"];
        while let Some(arg) = rest.next() {
            if SWITCHES.contains(&arg.as_str()) {
                switches.push(arg.clone());
            } else if let Some(name) = arg.strip_prefix("--") {
                let value = rest.next().ok_or_else(|| format!("--{name} needs a value\n{}", usage()))?;
                if name == "media" {
                    media.push(value.clone());
                } else if name == "marks" {
                    marks.push(value.clone());
                } else if name == "param" {
                    params.push(value.clone());
                } else {
                    flags.insert(name.to_string(), value.clone());
                }
            } else if file.is_none() {
                file = Some(PathBuf::from(arg));
            } else {
                return Err(format!("unexpected `{arg}`\n{}", usage()));
            }
        }
        let file = match file {
            Some(_) if flags.contains_key("job") => return Err(format!("give a file or a --job\n{}", usage())),
            Some(file) => file,
            None if flags.contains_key("job") => PathBuf::new(),
            None => return Err(format!("`{command}` needs a file\n{}", usage())),
        };
        if flags.contains_key("job") && (!media.is_empty() || flags.contains_key("media-file")) {
            return Err("give a render job, or a standalone document and media files".into());
        }
        Ok(Args { command, json: switches.iter().any(|s| s == "--json"), file, flags, switches, media, marks, params })
    }

    fn switch(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }

    fn number(&self, name: &str) -> Result<Option<f64>, String> {
        self.flags.get(name).map(|v| v.parse::<f64>().map_err(|_| format!("--{name}: `{v}` is not a number"))).transpose()
    }

    fn time(&self, name: &str, fps: f64) -> Result<Option<f64>, String> {
        self.flags.get(name).map(|v| parse_time(v, fps).map_err(|e| format!("--{name}: {e}"))).transpose()
    }

    /// The render job's media, or the standalone document's media table;
    /// `None` with `--no-media` or when there is none.
    fn media(&self, doc: &Document, project: Option<&LocalMedia>) -> Result<Option<LocalMedia>, String> {
        if self.switch("--no-media") {
            return Ok(None);
        }
        if let Some(project) = project {
            return Ok(Some(project.clone()));
        }
        let list = self.flags.get("media-file").map(PathBuf::from).or_else(|| doc.sibling_media_file());
        if self.media.is_empty() && list.is_none() {
            return Ok(None);
        }
        local_media(&self.media, list.as_deref()).map(Some)
    }
}

/// Print a report and leave with `code`.
fn leave(code: i32, text: &str) -> ! {
    if code == 0 || code == 1 && !text.contains("FAILED") {
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
    } else {
        eprintln!("{text}");
    }
    std::process::exit(code)
}

/// A failure, as JSON when asked for.
fn fail(json: bool, error: &str) -> ! {
    if json {
        println!("{}", makepad_strict_json::obj(vec![("ok", makepad_strict_json::Value::Bool(false)), ("error", makepad_strict_json::s(error))]).to_json());
        std::process::exit(1)
    }
    leave(1, &format!("makepad-edit-tools: FAILED: {error}"))
}

/// The cut, or its diagnostics as an inspect report (exit 1).
fn load_or_report(doc: &Document, json: bool) -> CutDoc {
    match doc.load() {
        Ok(edit) => edit,
        Err(_) => {
            let report: InspectReport = inspect(doc, None, &InspectOptions::default());
            let text = if json { report.to_json().to_json() } else { report.to_text() };
            leave(1, &text)
        }
    }
}

fn out_path(args: &Args, doc_path: &Path, suffix: &str) -> PathBuf {
    if let Some(out) = args.flags.get("out") {
        return PathBuf::from(out);
    }
    let stem = doc_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "cut".into());
    doc_path.with_file_name(format!("{stem}.{suffix}"))
}

/// `effect-strip`: an effect or transition over the test cards.
fn effect_strip(args: &Args) -> ! {
    use makepad_story_edit_tools::effect_strip::{kit_param, EffectSpec, StripOptions};
    let json = args.json;
    let name = args.file.to_string_lossy().to_string();
    let mut options = StripOptions::default();
    let number = |name: &str| args.number(name).unwrap_or_else(|e| leave(2, &e));
    if let Some(count) = number("count") {
        options.count = count.max(1.0) as usize;
    }
    if let Some(duration) = number("duration") {
        options.duration = duration.max(0.1);
    }
    if let Some(width) = number("width") {
        options.width = width.max(8.0) as u32;
    }
    if let Some(size) = args.flags.get("size") {
        let parsed = size.split_once('x').and_then(|(w, h)| Some((w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?)));
        options.size = parsed.filter(|(w, h)| *w >= 16 && *h >= 16).unwrap_or_else(|| leave(2, &format!("--size: `{size}` is WxH, e.g. 640x360")));
    }
    options.width = options.width.min(options.size.0);
    let spec = if name.ends_with(".splash") {
        // A library effect's source file.
        let source = std::fs::read_to_string(&args.file).unwrap_or_else(|e| fail(json, &format!("{name}: {e}")));
        let digest = makepad_asset_data::BlobId::hash_of(source.as_bytes());
        let asset = makepad_story_cut::AssetRef {
            id: makepad_asset_data::AssetId::from_bytes([0xef; 16]).to_string(),
            rev: format!("arev_{}", digest.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>()),
        };
        EffectSpec::Library { asset, source }
    } else {
        let params = args.params.iter().map(|p| kit_param(p)).collect::<Result<Vec<_>, _>>().unwrap_or_else(|e| leave(2, &e));
        EffectSpec::parse(&name, params)
    };
    let stem: String = name.rsplit('/').next().unwrap_or("effect").trim_end_matches(".splash").chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    let out = args.flags.get("out").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{stem}.strip.png")));
    let columns = args.number("columns").unwrap_or_else(|e| leave(2, &e)).map(|n| n.max(1.0) as usize);
    host::run_effect_strip(spec, options, columns, out, json)
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let args = match Args::parse(&argv) {
        Ok(args) => args,
        Err(error) => leave(2, &error),
    };
    let json = args.json;
    if args.command == "effect-strip" {
        effect_strip(&args)
    }
    if args.command == "screen-crop" {
        let aspect = match args.flags.get("aspect") {
            Some(a) => match a.split_once(':') {
                Some((w, h)) => w.trim().parse::<f64>().ok().zip(h.trim().parse::<f64>().ok()).map(|(w, h)| w / h),
                None => a.parse::<f64>().ok(),
            }
            .filter(|a| *a > 0.0)
            .unwrap_or_else(|| leave(2, "--aspect is w:h (9:19.5) or a number")),
            None => leave(2, "screen-crop needs --aspect w:h"),
        };
        let zoom = args.number("zoom").unwrap_or_else(|e| leave(2, &e));
        let result = std::fs::read(&args.file)
            .map_err(|e| format!("{}: {e}", args.file.display()))
            .and_then(|b| makepad_story_edit_tools::screen_crop::read_focus(&b))
            .and_then(|focus| makepad_story_edit_tools::screen_crop::screen_crop(&focus, aspect, zoom));
        match result {
            Ok(keys) => {
                println!("crop: {keys}");
                std::process::exit(0)
            }
            Err(error) => fail(json, &error),
        }
    }
    let (file, project) = match args.flags.get("job") {
        Some(job) => {
            let (file, media) = makepad_story_edit_render::job::Job::read(Path::new(job)).and_then(|job| job.load())
                .unwrap_or_else(|error| fail(json, &error));
            (file, Some(media))
        }
        None => (args.file.clone(), None),
    };
    let mut doc = Document::read(&file).unwrap_or_else(|error| fail(json, &error));
    if let Some((tree, path)) = project.as_ref().and_then(|media| media.tree.as_ref()) {
        doc.name = path.clone();
        doc.tree = Some(tree.context(path));
    }
    match args.command.as_str() {
        "inspect" => {
            let safe = match args.flags.get("safe") {
                Some(id) => Some(SafeZone::from_id(id.trim_start_matches('@')).unwrap_or_else(|| leave(2, &format!("--safe: `{id}` is reels, tiktok, shorts or none")))),
                None => None,
            };
            let mut options = InspectOptions { safe, ..InspectOptions::default() };
            if let Ok(edit) = doc.load() {
                options.marks = run_marks(&edit, &args.marks).unwrap_or_else(|error| fail(json, &error));
            }
            if let Some(fps) = args.number("sample-fps").unwrap_or_else(|e| leave(2, &e)) {
                options.sample_fps = fps;
            }
            if let Some(px) = args.number("min-text-px").unwrap_or_else(|e| leave(2, &e)) {
                if !px.is_finite() || px < 0.0 {
                    leave(2, "--min-text-px must be a finite non-negative number (default: 16)");
                }
                options.min_text_px = px;
            }
            let mut media = args.media(&doc, project.as_ref()).unwrap_or_else(|error| fail(json, &error));
            let report = inspect(&doc, media.as_mut().map(|m| m as &mut dyn makepad_story_edit_render::MediaResolver), &options);
            let text = if json { report.to_json().to_json() } else { report.to_text() };
            leave(if report.ok() { 0 } else { 1 }, &text)
        }
        "timeline" if doc.kind == DocKind::Motion => match timeline_motion(&doc.source) {
            Ok(timeline) => leave(0, &if json { timeline.to_json().to_json() } else { timeline.to_text() }),
            Err(_) => {
                let report = inspect(&doc, None, &InspectOptions::default());
                leave(1, &if json { report.to_json().to_json() } else { report.to_text() })
            }
        },
        "audio analyze" | "audio at" if doc.kind == DocKind::Motion => fail(json, "a motion document has no sound of its own; analyze the cut that plays it"),
        "timeline" => {
            let edit = load_or_report(&doc, json);
            let eval = CutEval::new(&edit);
            let timeline = timeline(&edit, &eval);
            leave(0, &if json { timeline.to_json().to_json() } else { timeline.to_text() })
        }
        "audio analyze" => {
            let edit = load_or_report(&doc, json);
            let marks = run_marks(&edit, &args.marks).unwrap_or_else(|error| fail(json, &error));
            let mut options = AudioOptions::default();
            if let Some(db) = args.number("silence-db").unwrap_or_else(|e| leave(2, &e)) {
                options.silence_db = db;
            }
            if let Some(seconds) = args.number("min-silence").unwrap_or_else(|e| leave(2, &e)) {
                options.min_silence = seconds;
            }
            let mut media = args.media(&doc, project.as_ref()).unwrap_or_else(|error| fail(json, &error)).unwrap_or_default();
            let (mix, silent) = mixdown(&doc, &mut media, &marks).unwrap_or_else(|error| fail(json, &error));
            let mut report = analyze_mix(&mix, mix.doc.duration, &options);
            report.silent_sources = silent;
            if !args.switch("--waveform") {
                leave(0, &if json { report.to_json().to_json() } else { report.to_text() })
            }
            let buckets = args.number("buckets").unwrap_or_else(|e| leave(2, &e)).map_or(400, |n| n.clamp(1.0, 100_000.0) as usize);
            let wave = waveform(&mix, mix.doc.duration, buckets);
            let out = out_path(&args, &file, "waveform.png");
            let written = match &wave.picture {
                Some(picture) => {
                    let png = makepad_story_edit_tools::encode_png(picture).unwrap_or_else(|error| fail(json, &error));
                    std::fs::write(&out, png).unwrap_or_else(|error| fail(json, &format!("{}: {error}", out.display())));
                    Some(out)
                }
                None => None,
            };
            if json {
                let mut value = report.to_json();
                if let makepad_strict_json::Value::Obj(fields) = &mut value {
                    let mut w = wave.to_json();
                    if let makepad_strict_json::Value::Obj(wf) = &mut w {
                        wf.push(("out".into(), written.as_ref().map_or(makepad_strict_json::Value::Null, |p| makepad_strict_json::Value::Str(p.to_string_lossy().into()))));
                    }
                    fields.push(("waveform".into(), w));
                }
                leave(0, &value.to_json())
            }
            let picture = written.map_or(" (silent: no picture)".to_string(), |p| format!(" -> {}", p.display()));
            leave(0, &format!("{}{}{picture}", report.to_text(), wave.to_text()))
        }
        "audio at" => {
            let edit = load_or_report(&doc, json);
            let t = args.time("at", edit.output.fps).unwrap_or_else(|e| leave(2, &e)).unwrap_or(0.0);
            let marks = run_marks(&edit, &args.marks).unwrap_or_else(|error| fail(json, &error));
            let count = |name: &str, default: usize| args.number(name).unwrap_or_else(|e| leave(2, &e)).map_or(default, |n| n.clamp(1.0, 4096.0) as usize);
            let (bins, slots) = (count("bins", 32), count("slots", 64));
            let window = args.number("window").unwrap_or_else(|e| leave(2, &e)).filter(|w| *w > 0.0).unwrap_or(0.05);
            let mut media = args.media(&doc, project.as_ref()).unwrap_or_else(|error| fail(json, &error)).unwrap_or_default();
            match at(&doc, &mut media, &marks, t, bins, slots, window) {
                Ok((now, silent)) => {
                    if json {
                        let mut value = now.to_json();
                        if let makepad_strict_json::Value::Obj(fields) = &mut value {
                            fields.push(("silent_sources".into(), makepad_strict_json::Value::Arr(silent.iter().map(|(id, why)| makepad_strict_json::obj(vec![("source", makepad_strict_json::s(id.clone())), ("why", makepad_strict_json::s(why.clone()))])).collect())));
                        }
                        leave(0, &value.to_json())
                    }
                    let silent: String = silent.iter().map(|(id, why)| format!("{id} plays silent: {why}\n")).collect();
                    leave(0, &format!("{}{silent}", now.to_text()))
                }
                Err(error) => fail(json, &error),
            }
        }
        "frame" | "strip" | "onion" | "render" => {
            let mut edit = load_or_report(&doc, json);
            let (fps, duration, output) = (edit.output.fps, edit.duration, (edit.output.width, edit.output.height));
            let time = |name: &str| args.time(name, fps).unwrap_or_else(|e| leave(2, &e));
            let count = |name: &str| args.number(name).unwrap_or_else(|e| leave(2, &e)).map(|n| n.max(1.0) as usize);
            let width = count("width").map(|w| w as u32).unwrap_or(match args.command.as_str() {
                "frame" => edit.output.width,
                _ => 480.min(edit.output.width),
            });
            let media = args.media(&doc, project.as_ref()).unwrap_or_else(|error| fail(json, &error)).unwrap_or_default();
            let marks = run_marks(&edit, &args.marks).unwrap_or_else(|error| fail(json, &error));
            let gpu = match args.command.as_str() {
                "frame" => {
                    let at = time("at").unwrap_or_else(|| leave(2, &format!("frame needs --at <seconds|@frame>\n{}", usage())));
                    let times = spread(at, at, 1, fps, duration);
                    let zoom = args.number("zoom").unwrap_or_else(|e| leave(2, &e)).unwrap_or(1.0);
                    if !(zoom > 0.0 && zoom.is_finite()) {
                        leave(2, "--zoom is a factor above 0 (2 renders the frame twice as large)");
                    }
                    let crop = args.flags.get("crop").map(|c| {
                        let parts: Vec<f64> = c.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                        match parts[..] {
                            [x, y, w, h] if w > 0.0 && h > 0.0 => [x, y, w, h],
                            _ => leave(2, &format!("--crop: `{c}` is x,y,w,h in the output's pixels (e.g. 0,0,960,540)")),
                        }
                    });
                    // Layer names: a motion document's own layers, at the frame's time.
                    let names = args.switch("--names").then(|| match makepad_story_edit_tools::motion::frame_layers(&doc.source, times[0]) {
                        Some(layers) => layers,
                        None => leave(2, "--names outlines the layers of a motion document; frame the graphic's own document"),
                    });
                    let width = ((width as f64 * zoom).round() as u32).clamp(16, 8192);
                    if zoom != 1.0 && doc.kind == DocKind::Motion {
                        // A motion document is drawn larger: real detail, not
                        // enlarged pixels (a cut's frame is enlarged).
                        let even = |v: f64| (((v * zoom).round() as u32 + 1) / 2 * 2).clamp(16, 8192);
                        edit.output.width = even(output.0 as f64);
                        edit.output.height = even(output.1 as f64);
                    }
                    host::Gpu::Frame { times, width, out: out_path(&args, &file, &format!("at{}.png", makepad_story_edit_tools::input::frame_of(at, fps, duration))), crop, output, names }
                }
                "strip" => {
                    let (from, to) = (time("from").unwrap_or(0.0), time("to").unwrap_or(duration).min(duration));
                    let times = match (args.number("every").unwrap_or_else(|e| leave(2, &e)), count("count")) {
                        (Some(every), _) if every > 0.0 => {
                            let steps = ((to - from) / every).floor() as usize + 1;
                            spread(from, from + every * (steps - 1) as f64, steps, fps, duration)
                        }
                        (_, count) => spread(from, to, count.unwrap_or(12), fps, duration),
                    };
                    host::Gpu::Strip { times, width, columns: count("columns"), out: out_path(&args, &file, "strip.png") }
                }
                "onion" => {
                    let from = time("from").unwrap_or_else(|| leave(2, &format!("onion needs --from and --to\n{}", usage())));
                    let to = time("to").unwrap_or_else(|| leave(2, &format!("onion needs --from and --to\n{}", usage())));
                    let steps = count("steps").unwrap_or(8).max(2);
                    host::Gpu::Onion { times: spread(from, to.max(from), steps, fps, duration), width, out: out_path(&args, &file, "onion.png") }
                }
                _ => {
                    let draft = args.switch("--draft");
                    host::Gpu::Render { draft, out: out_path(&args, &file, if draft { "draft.mp4" } else { "mp4" }) }
                }
            };
            let job = host::Job { doc: Some(std::sync::Arc::new(edit)), resolver: doc.resolver(Box::new(media)), marks, json };
            host::run(job, gpu)
        }
        other => leave(2, &format!("unknown command `{other}`\n{}", usage())),
    }
}

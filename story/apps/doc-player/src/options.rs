//! What to play and how: from the command line natively, from the page's
//! query in the browser (`?doc=deck.splash&deterministic&end=exit` reads as
//! `deck.splash --deterministic --end exit`).

use std::path::PathBuf;

/// What the player does when the timeline ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum End {
    /// Print the summary and exit with its status (deterministic default).
    Exit,
    /// Stay on the last frame (the default when playing on the wall clock).
    Hold,
    /// Start again at the first frame.
    Loop,
}

/// A frame to capture: a time in seconds or a frame number (`@n`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum At {
    Seconds(f64),
    Frame(u64),
}

/// A `--seek` target: seconds into the timeline, or (`75%`) a share of its length (a
/// document-independent seek order for an export's replay).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seek {
    At(f64),
    Share(f64),
}

#[derive(Clone, Debug)]
pub struct Options {
    /// The document: a `.cut.splash`, a deck (`deck.splash`,
    /// `*.deck.splash`) or a Motion document (`*.splash`).
    pub document: PathBuf,
    /// The folder whose tree the document is a file of (default: the
    /// document's folder).
    pub tree: Option<PathBuf>,
    /// Explicit optional-entry descriptor; no folder discovery is performed.
    pub tree_descriptor: Option<PathBuf>,
    /// Optional tree entries filled from files elsewhere (a song kept
    /// outside the source tree): `logical path -> file`.
    pub fills: Vec<(String, PathBuf)>,
    /// Frames on a fixed step along the timeline, each drawn exactly.
    pub deterministic: bool,
    /// The clock's frame rate where the document has none of its own (a
    /// deck): 60.
    pub fps: f64,
    /// The span played, in timeline seconds (a deck's clock from its start).
    pub from: f64,
    pub to: Option<f64>,
    /// A deck starts at this slide.
    pub slide: usize,
    /// A deck holds each stop (a beat, a slide's end) this long before the
    /// next is taken.
    pub dwell: f64,
    pub end: End,
    /// Write captured frames as PNG here.
    pub capture: Option<PathBuf>,
    /// The frames captured (none: every `every`-th, or none).
    pub at: Vec<At>,
    pub every: Option<u64>,
    /// Write `<frame> <rgba hash>` for every frame drawn (deterministic).
    pub hashes: Option<PathBuf>,
    /// Write the sound as a 48 kHz stereo WAV (deterministic).
    pub audio_out: Option<PathBuf>,
    /// After the span plays, seek to each of these in turn and play `seek_play`
    /// seconds from each (deterministic): what a viewer's seeks run.
    pub seeks: Vec<Seek>,
    pub seek_play: f64,
    /// Cap motion blur's adaptive sub-frames at this many (a coverage run: every path of the
    /// schedule in fewer paints; the pictures are then not the film's).
    pub shutter_max: Option<u32>,
    /// The name the loader shows (default: the cut's label, else the
    /// document's file name).
    pub title: Option<String>,
}

pub const USAGE: &str = "usage: makepad-story-doc-player <document> [--tree <dir>] [--tree-descriptor <file>] [--fill <logical path>=<file>]...\n\
    \x20 [--deterministic] [--fps <rate>] [--from <s>] [--to <s>] [--slide <n>] [--dwell <s>] [--end exit|hold|loop]\n\
    \x20 [--capture <dir> [--at <s|@n>,...] [--every <k>]] [--hashes <file>] [--audio-out <file.wav>]\n\
    \x20 [--seek <s>,<s>,... [--seek-play <s>]] [--shutter-max <n>] [--title <name>]";

impl Options {
    /// Parse `args` (the program name not included).
    pub fn parse(args: &[String]) -> Result<Options, String> {
        let mut document = None;
        let mut options = Options {
            document: PathBuf::new(), tree: None, tree_descriptor: None, fills: Vec::new(), deterministic: false, fps: 60.0, from: 0.0, to: None,
            slide: 0, dwell: 1.0, end: End::Hold, capture: None, at: Vec::new(), every: None, hashes: None, audio_out: None,
            seeks: Vec::new(), seek_play: 2.0, shutter_max: None, title: None,
        };
        let mut end = None;
        let mut i = 0;
        while i < args.len() {
            let arg = args[i].as_str();
            let mut value = || {
                i += 1;
                args.get(i).cloned().ok_or_else(|| format!("{arg} needs a value"))
            };
            let number = |text: String| text.parse::<f64>().map_err(|_| format!("{arg}: `{text}` is not a number"));
            match arg {
                "--tree" => options.tree = Some(PathBuf::from(value()?)),
                "--tree-descriptor" => options.tree_descriptor = Some(PathBuf::from(value()?)),
                "--fill" => {
                    let pair = value()?;
                    let (path, file) = pair.split_once('=').ok_or_else(|| format!("--fill {pair}: expected <logical path>=<file>"))?;
                    options.fills.push((path.to_string(), PathBuf::from(file)));
                }
                "--deterministic" => options.deterministic = true,
                "--fps" => options.fps = number(value()?)?.max(1.0),
                "--from" => options.from = number(value()?)?.max(0.0),
                "--to" => options.to = Some(number(value()?)?),
                "--slide" => options.slide = value()?.parse().map_err(|_| "--slide: a slide number".to_string())?,
                "--dwell" => options.dwell = number(value()?)?.max(0.0),
                "--end" => end = Some(match value()?.as_str() {
                    "exit" => End::Exit,
                    "hold" => End::Hold,
                    "loop" => End::Loop,
                    other => return Err(format!("--end: `{other}` (exit, hold or loop)")),
                }),
                "--capture" => options.capture = Some(PathBuf::from(value()?)),
                "--at" => {
                    for at in value()?.split(',').map(str::trim).filter(|at| !at.is_empty()) {
                        options.at.push(match at.strip_prefix('@') {
                            Some(n) => At::Frame(n.parse().map_err(|_| format!("--at: `{at}`"))?),
                            None => At::Seconds(at.parse().map_err(|_| format!("--at: `{at}`"))?),
                        });
                    }
                }
                "--every" => options.every = Some(value()?.parse::<u64>().map_err(|_| "--every: a frame count".to_string())?.max(1)),
                "--hashes" => options.hashes = Some(PathBuf::from(value()?)),
                "--audio-out" => options.audio_out = Some(PathBuf::from(value()?)),
                "--seek" => {
                    for t in value()?.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                        let seek = match t.strip_suffix('%') {
                            Some(share) => Seek::Share(share.parse::<f64>().map_err(|_| format!("--seek: `{t}` is not a share"))?.clamp(0.0, 100.0) / 100.0),
                            None => Seek::At(t.parse::<f64>().map_err(|_| format!("--seek: `{t}` is not a time"))?.max(0.0)),
                        };
                        options.seeks.push(seek);
                    }
                }
                "--seek-play" => options.seek_play = number(value()?)?.max(0.0),
                "--title" => options.title = Some(value()?),
                "--shutter-max" => options.shutter_max = Some(value()?.parse::<u32>().map_err(|_| "--shutter-max: a sub-frame count".to_string())?.max(4)),
                _ if arg.starts_with("--") => return Err(format!("unknown option {arg}\n{USAGE}")),
                _ if document.is_none() => document = Some(PathBuf::from(arg)),
                _ => return Err(format!("one document only (`{arg}`)\n{USAGE}")),
            }
            i += 1;
        }
        options.document = document.ok_or_else(|| USAGE.to_string())?;
        options.end = end.unwrap_or(if options.deterministic { End::Exit } else { End::Hold });
        if (!options.at.is_empty() || options.every.is_some()) && options.capture.is_none() {
            return Err("--at and --every capture into a --capture folder".into());
        }
        if (options.hashes.is_some() || options.audio_out.is_some() || options.capture.is_some() || !options.seeks.is_empty()) && !options.deterministic {
            return Err("--capture, --hashes, --audio-out and --seek need --deterministic".into());
        }
        Ok(options)
    }

    /// The browser's page query as arguments: `doc` is the document, a key
    /// without a value is a flag (`deterministic`), others take their value.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn query_args(search: &str) -> Vec<String> {
        let mut args = Vec::new();
        for pair in search.trim_start_matches('?').split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            // The platform consumes this page-only clock hook directly.
            if key == "test" { continue; }
            if key == "doc" {
                args.push(value.to_string());
                continue;
            }
            args.push(format!("--{key}"));
            if !value.is_empty() {
                args.push(value.to_string());
            }
        }
        args
    }
}

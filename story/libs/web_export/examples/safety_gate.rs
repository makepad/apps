//! The browser safety gate on a folder the export (or anything else) wrote:
//!
//! ```text
//! cargo run --release -p makepad-story-web-export --example safety_gate -- <dir>
//!     (--bundle <bundle dir> | --duration <s> [--plates <s,s,...>])
//!     [--shaders <shaders.pack>] [--work <dir>] [--chrome <path>]
//!     [--passes N] [--seeks N] [--seek-play S] [--size <w>x<h>] [--dpr D]
//!     [--keep-playing]
//! ```
//!
//! `--keep-playing`: the runtime's console reports fail the film without
//! ending the play (a coverage run of an instrumented build, whose probe
//! memory the gate writes to `<work>/coverage.bin`).
//!
//! `--bundle` (the export's build folder `bundle`, or a player bundle)
//! gives the duration and the plates. The music counts as present when the
//! folder has an audio file. Exits 1 when the film fails the gate, 2 when
//! it was not checked.

use makepad_story_web_export::safety::{gate, Film, SafetyOptions};
use makepad_story_web_export::{Diagnostic, Phase, Progress};
use std::path::PathBuf;

struct Log;

impl Progress for Log {
    fn phase(&mut self, phase: Phase, done: f64, note: &str) {
        eprintln!("[{:>8}] {:>3.0}% {}", phase.name(), done * 100.0, note);
    }

    fn diagnostic(&mut self, d: &Diagnostic) {
        eprintln!("{d}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
    let number = |name: &str| value(name).map(|v| v.parse::<f64>().unwrap_or_else(|_| panic!("{name}: not a number")));
    let Some(dir) = args.first().filter(|a| !a.starts_with("--")).map(PathBuf::from) else {
        eprintln!("usage: safety_gate <dir> (--bundle <dir> | --duration <s> [--plates <s,s,...>]) [--shaders <pack>] [--work <dir>] [--chrome <path>] [--passes N] [--seeks N] [--seek-play S] [--size <w>x<h>] [--dpr D]");
        std::process::exit(2);
    };
    let shaders = value("--shaders").map(|p| std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}")));
    let has_music = std::fs::read_dir(&dir).map(|d| d.flatten().any(|e| e.path().extension().and_then(|x| x.to_str()).is_some_and(|x| ["mp3", "m4a", "aac", "ogg", "opus", "flac", "wav"].contains(&x)))).unwrap_or(false);
    let mut opts = SafetyOptions { chrome: value("--chrome").map(PathBuf::from), keep_playing: args.iter().any(|a| a == "--keep-playing"), ..Default::default() };
    if let Some(n) = number("--passes") {
        opts.passes = n as usize;
    }
    if let Some(n) = number("--seeks") {
        opts.seeks = n as usize;
    }
    if let Some(s) = number("--seek-play") {
        opts.seek_play_s = s;
    }
    if let Some((w, h)) = value("--size").and_then(|s| s.split_once('x').map(|(w, h)| (w.parse().unwrap_or(1920), h.parse().unwrap_or(1080)))) {
        opts.viewport = (w, h, opts.viewport.2);
    }
    if let Some(d) = number("--dpr") {
        opts.viewport.2 = d;
    }
    let (mut duration, mut plates) = (number("--duration").unwrap_or(10.0), vec![0.0]);
    if let Some(b) = value("--bundle") {
        let bundle = makepad_story_player_bundle::Bundle::read(std::path::Path::new(&b)).unwrap_or_else(|e| panic!("{b}: {e}"));
        duration = bundle.duration;
        plates = bundle.plates.iter().map(|p| p.start).collect();
        plates.sort_by(|a, b| a.total_cmp(b));
    } else if let Some(p) = value("--plates") {
        plates = p.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    }
    let work = value("--work").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join(format!("safety-gate-{}", std::process::id())));
    let film = Film { dir: &dir, duration, plates, has_music, shaders: shaders.as_deref() };
    let report = gate(&film, &opts, &work, &mut Log);
    println!("{}", report.summary());
    print!("{}", report.details());
    for n in &report.notes {
        println!("note: {n}");
    }
    if let Some(why) = &report.skipped {
        println!("NOT CHECKED: {why}");
        std::process::exit(2);
    }
    for f in &report.failures {
        println!("FAIL: {f}");
    }
    if !report.failures.is_empty() {
        std::process::exit(1);
    }
    println!("PASS");
}

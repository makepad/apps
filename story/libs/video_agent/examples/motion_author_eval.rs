//! Can a model write a working Motion document from the authoring guide?
//! A headless check of the guide itself: the model gets the same guide the
//! in-app agent gets (no tools: it answers with the whole document), the
//! document is loaded and inspected (errors, then layout warnings), the
//! report goes back, and it fixes, for a few rounds.
//!
//!   cargo run --release -p makepad-story-video-agent --example motion_author_eval -- \
//!       [--backend hub|claude] [--model id] [--rounds 4] [--out dir] [--full-guide] "explain the derivative of x²"
//!
//! Writes `<out>/round<N>.splash` and `<out>/round<N>.report.txt`; exits 0 when
//! the last document loads with no errors and no layout warnings.

use makepad_motion::MotionDoc;
use makepad_story_video_agent::guide::{authoring_guide, compact_guide};
use makepad_story_video_agent::{Backend, Brain, BrainEvent};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const TASK: &str = "\
You write ONE Motion document (1920x1080, 60 fps) for Stage, following the authoring guide below. \
There are no tools: answer with the complete document in a single ```splash block and nothing after it. \
The document must start with `use mod.motion.*` and contain one `Motion{...}`. \
You will get back the loader's errors and the layout checker's warnings (line:column, layer path, \
message, fix); answer each report with the whole corrected document. Keep it self-contained: no \
imports of library assets, no media files.";

fn reply(brain: &mut Box<dyn Brain>, limit: Duration) -> Result<String, String> {
    let start = Instant::now();
    let mut text = String::new();
    loop {
        for event in brain.poll() {
            match event {
                BrainEvent::Delta(t) => text.push_str(&t),
                BrainEvent::Status(s) => eprintln!("[status] {s}"),
                BrainEvent::TurnDone => return Ok(text),
                BrainEvent::Failed(why) => return Err(why),
                _ => {}
            }
        }
        if start.elapsed() > limit {
            brain.cancel();
            return Err(format!("no reply within {} s", limit.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The last ```splash (or plain ```) block of a reply.
fn document_of(text: &str) -> Option<String> {
    let mut found = None;
    let mut rest = text;
    while let Some(i) = rest.find("```") {
        let after = &rest[i + 3..];
        let body_start = after.find('\n').map(|n| n + 1)?;
        let body = &after[body_start..];
        let end = body.find("```")?;
        found = Some(body[..end].to_string());
        rest = &body[end + 3..];
    }
    found
}

/// Errors and layout warnings, as the in-app inspect would show them.
fn check(source: &str) -> (usize, String) {
    let doc = match MotionDoc::load(source) {
        Ok(doc) => doc,
        Err(diags) => {
            let text = diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n");
            return (diags.len().max(1), format!("The document does not load:\n{text}"));
        }
    };
    let mut lines: Vec<String> = doc.warnings.iter().map(|w| format!("warning {w}")).collect();
    let times = makepad_motion::inspect::sample_times(&doc, 10.0);
    // Errors a frame reports while it evaluates (a property function that
    // returns the wrong kind of value), once each with where they started.
    let mut seen = std::collections::BTreeSet::new();
    for &t in &times {
        for error in doc.eval(t).errors {
            let text = error.to_string();
            if seen.insert(text.clone()) {
                lines.push(format!("error at {t:.2} s: {text}"));
            }
        }
    }
    let places: std::collections::HashMap<String, (u32, u32)> = doc.all_layers().iter().map(|l| (l.path.clone(), (l.loc.line, l.loc.col))).collect();
    for issue in makepad_motion::inspect::layout_checks(&doc, &times, makepad_motion::inspect::DEFAULT_MIN_TEXT_PX) {
        let (line, col) = places.get(&issue.path).copied().unwrap_or((0, 0));
        let other = issue.other.as_ref().map(|o| format!(" (with `{o}`)")).unwrap_or_default();
        lines.push(format!("{line}:{col}: warning[{}] `{}`{other} {:.2}-{:.2} s: {}\n    fix: {}", issue.code, issue.path, issue.from, issue.to, issue.message, issue.hint));
    }
    if lines.is_empty() {
        (0, "The document loads with no errors and no layout warnings.".into())
    } else {
        (lines.len(), format!("The document loads, with these warnings:\n{}", lines.join("\n")))
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut backend, mut model, mut rounds, mut out, mut prompt, mut full) = ("hub".to_string(), None, 4usize, PathBuf::from("motion_author_eval"), String::new(), false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--backend" => backend = args.next().expect("--backend hub|claude"),
            "--model" => model = args.next(),
            "--rounds" => rounds = args.next().and_then(|n| n.parse().ok()).expect("--rounds n"),
            "--out" => out = PathBuf::from(args.next().expect("--out dir")),
            // The full guide instead of the compact explainer reference.
            "--full-guide" => full = true,
            _ => prompt = arg,
        }
    }
    if prompt.is_empty() {
        prompt = "Make a 12 second math explainer: the derivative of x squared. Show the parabola on axes, a tangent line sliding along it, and the result d/dx x^2 = 2x.".into();
    }
    let backend = match backend.as_str() {
        "claude" => Backend::ClaudeCode { model: model.unwrap_or_else(|| makepad_story_video_agent::DEFAULT_CLAUDE_MODEL.into()) },
        _ => Backend::Hub { model },
    };
    let system = format!("{TASK}\n\n# The authoring guide\n\n{}", if full { authoring_guide() } else { compact_guide() });
    let mut brain = backend.start(&system, &[]).unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir_all(&out).expect("out dir");
    brain.send_user(format!("# Brief\n{prompt}"), Vec::new());
    let mut clean = false;
    for round in 1..=rounds {
        let text = match reply(&mut brain, Duration::from_secs(900)) {
            Ok(text) => text,
            Err(why) => {
                eprintln!("round {round}: {why}");
                break;
            }
        };
        let Some(source) = document_of(&text) else {
            eprintln!("round {round}: no ```splash block in the reply");
            std::fs::write(out.join(format!("round{round}.reply.txt")), &text).ok();
            brain.send_user("Your reply had no ```splash block. Answer with the complete document in one ```splash block.".into(), Vec::new());
            continue;
        };
        let (issues, report) = check(&source);
        std::fs::write(out.join(format!("round{round}.splash")), &source).ok();
        std::fs::write(out.join(format!("round{round}.report.txt")), &report).ok();
        println!("round {round}: {issues} issue(s)\n{report}\n");
        if issues == 0 {
            clean = true;
            break;
        }
        brain.send_user(format!("# Check report\n{report}\n\nFix these and answer with the whole corrected document."), Vec::new());
    }
    std::process::exit(if clean { 0 } else { 1 });
}

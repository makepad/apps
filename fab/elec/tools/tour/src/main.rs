//! External tour player. Inputs and frame capture use the same client as `makepad-remote app`.

#![forbid(unsafe_code)]

mod document;
mod player;

use std::path::PathBuf;

const USAGE: &str = "usage: fab-elec-tour <doc.splash> --app <pid|name|socket> [--out DIR]\n       fab-elec-tour <doc.splash> --plan [--out DIR]";

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next().ok_or(USAGE)?;
    if first == "--help" || first == "-h" { println!("{USAGE}"); return Ok(()); }
    let doc = PathBuf::from(first);
    let (mut app, mut out, mut plan) = (None, None, false);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--app") if app.is_none() => { app = Some(args.next().ok_or("--app expects a pid, app name or socket")?.into_string().map_err(|_| "--app expects UTF-8")?); }
            Some("--out") if out.is_none() => { out = Some(PathBuf::from(args.next().ok_or("--out expects a directory")?)); }
            Some("--plan") if !plan => plan = true,
            _ => return Err(format!("unknown or repeated option {}\n{USAGE}", arg.to_string_lossy())),
        }
    }
    if !plan && app.as_ref().is_none_or(|s| s.is_empty()) { return Err(USAGE.into()); }
    let source = std::fs::read_to_string(&doc).map_err(|e| format!("{}: {e}", doc.display()))?;
    let tour = document::read(&source).map_err(|e| format!("{}:{e}", doc.display()))?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let out = cwd.join(out.unwrap_or_else(|| PathBuf::from(".")));
    if plan { return player::plan(&tour, &out); }
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    if let Some(record) = &tour.record {
        if let Some(parent) = out.join(record).parent() { std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?; }
    }
    let spec = app.as_deref().unwrap();
    eprintln!("tour {} for {} via {spec}", doc.display(), tour.app);
    player::play(&tour, &out, spec).map_err(|e| e.to_string())
}

fn main() {
    if let Err(error) = run() { eprintln!("fab-elec-tour: {error}"); std::process::exit(1); }
}

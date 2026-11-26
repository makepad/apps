//! `makepad-story-player-bundle (--job <render.job> | --edit <film.cut.splash> (--tree <folder> | --media <media.txt>)) --out <dir> [--keep-old]`:
//! writes the film bundle a browser player loads (see the library's docs):
//! every file named by its content, then `manifest.json`, then removes the
//! content-named files of earlier bundles in `dir` unless `--keep-old`.

use makepad_story_player_bundle::{build_bundle, build_job};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Err(error) = run(&args) {
        eprintln!("makepad-story-player-bundle: {error}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let usage = || "usage: makepad-story-player-bundle (--job <render.job> | --edit <film.cut.splash> (--tree <folder> | --media <media.txt>)) --out <dir> [--keep-old]".to_string();
    let value = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].as_str());
    let out = PathBuf::from(value("--out").ok_or_else(usage)?);
    if value("--tree").is_some() && value("--media").is_some() { return Err(usage()); }
    let started = std::time::Instant::now();
    let bundle = match (value("--job"), value("--edit"), value("--tree").or_else(|| value("--media"))) {
        (Some(job), None, None) => build_job(std::path::Path::new(job))?,
        (None, Some(edit), Some(media)) => build_bundle(std::path::Path::new(edit), std::path::Path::new(media))?,
        _ => return Err(usage()),
    };
    let done = bundle.write(&out, args.iter().any(|a| a == "--keep-old"))?;
    println!("bundle {} ({} files, {} written, {} pruned, {:.1} s)", out.display(), done.files, done.written, done.pruned, started.elapsed().as_secs_f64());
    for (kind, (n, bytes)) in bundle.kinds() {
        println!("  {kind:<9} {n:>3} files {bytes:>12} bytes");
    }
    println!("  {:<9} {:>3} files {:>12} bytes  (manifest.json {} bytes)", "total", done.files, bundle.total_bytes(), bundle.manifest_json().len());
    println!("  fonts named: {}", bundle.fonts_named.join(", "));
    println!("  music analysed: {}", if bundle.reads_music { "yes (a document reads music())" } else { "no (no document reads music())" });
    Ok(())
}

//! `makepad-film-build`: Stage's web export from the command line (CI,
//! agents). The same library as the Web export card.
//!
//! ```text
//! makepad-film-build (--job <render.job> | --edit <film.cut.splash> (--tree <folder> | --media <media.txt>) | --bundle <dir>) --out <dir>
//!     [--credits <credits.txt>] [--mobile] [--no-music] [--source <makepad checkout>] [--build <dir>]
//!     [--pack film|app|full] [--fonts subset|full]
//!     [--serve | --serve-lan]   (then serve the folder: localhost, or the local network too)
//!     [--no-safety] [--chrome <path>]   (skip the browser safety gate; the Chrome it plays the film in)
//! ```

use makepad_story_web_export::{export, report, ExportOptions, FontsKnob, PackPreset, Preset, StderrProgress};
use std::path::PathBuf;

fn usage() -> ! {
    eprintln!("usage: makepad-film-build (--job <render.job> | --edit <film.cut.splash> (--tree <folder> | --media <media.txt>) | --bundle <dir>) --out <dir> [--credits <file>] [--mobile] [--no-music] [--source <makepad checkout>] [--build <dir>]");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
    let flag = |name: &str| args.iter().any(|a| a == name);
    let bundle = value("--bundle").map(PathBuf::from);
    let job = value("--job").map(PathBuf::from);
    if value("--tree").is_some() && value("--media").is_some() { usage(); }
    let (edit, media) = match (value("--edit"), value("--tree").or_else(|| value("--media")), &bundle, &job) {
        (Some(e), Some(m), None, None) => (e, m),
        (None, None, Some(_), None) | (None, None, None, Some(_)) => (String::new(), String::new()),
        _ => usage(),
    };
    let Some(out) = value("--out") else { usage() };
    let credits = match value("--credits") {
        Some(path) => std::fs::read_to_string(&path).unwrap_or_else(|e| {
            eprintln!("makepad-film-build: {path}: {e}");
            std::process::exit(1)
        }),
        None => String::new(),
    };
    // The checkout this tool was built from, unless told otherwise.
    let source_root = value("--source").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../.."));
    let source_root = source_root.canonicalize().unwrap_or(source_root);
    let build_dir = value("--build").map(PathBuf::from).unwrap_or_else(|| source_root.join("target/web-export/build"));
    let mut pack = match value("--pack") {
        Some(p) => PackPreset::parse(&p).unwrap_or_else(|| usage()).options(),
        None => PackPreset::Film.options(),
    };
    if let Some(f) = value("--fonts") {
        pack.fonts = match f.as_str() {
            "full" => FontsKnob::Full,
            "subset" | "analysed" => FontsKnob::Analysed,
            _ => usage(),
        };
    }
    let opts = ExportOptions {
        job,
        edit: edit.into(),
        media: media.into(),
        bundle,
        out_dir: out.into(),
        preset: if flag("--mobile") { Preset::Mobile } else { Preset::Desktop },
        include_music: !flag("--no-music"),
        credits,
        source_root,
        build_dir,
        pack,
        safety: makepad_story_web_export::safety::SafetyOptions { enabled: !flag("--no-safety"), chrome: value("--chrome").map(PathBuf::from), ..Default::default() },
    };
    match export(&opts, &mut StderrProgress) {
        Ok(report) => {
            print!("{}", report::table(&report.breakdown, &report.files));
            if flag("--serve") || flag("--serve-lan") {
                match makepad_web_pack::serve::serve(&opts.out_dir, flag("--serve-lan"), 0) {
                    Ok(server) => {
                        println!("serving {} at {}{}", opts.out_dir.display(), server.local_url, server.lan_url.map(|u| format!(" and {u}")).unwrap_or_default());
                        loop {
                            std::thread::park();
                        }
                    }
                    Err(e) => eprintln!("makepad-film-build: {e}"),
                }
            }
        }
        Err(diags) => {
            for d in &diags {
                eprintln!("{d}");
            }
            std::process::exit(1);
        }
    }
}

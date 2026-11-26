//! The page: `index.html` (the loader and the credits), `film.js` (Makepad's
//! web runtime and the film's start), `CREDITS.txt`, `README.txt`, and the
//! output folder they are written into.
//!
//! `film.js` is the platform's own JavaScript, taken from the source tree
//! at export time (never a copy kept here): the wasm bridge, the browser
//! host (`web.js`) and its WebGL backend (`web_gl.js`), concatenated into
//! one module with their imports and exports resolved, minus the sections
//! a film does not use ([`STRIPPED`]: marked `// @section <name>` ...
//! `// @end <name>` in those files), plus the audio worklet as an inline
//! module. Not minified: brotli takes the slack, and the page stays
//! readable.

use crate::fonts::ShippedFont;
use crate::{Diagnostic, Phase};
use makepad_story_player_bundle::Bundle;
use std::path::Path;

/// Marked sections of the runtime a film never uses (web_pack's list).
pub use makepad_web_pack::js::PLAYBACK_JS_STRIPPED as STRIPPED;

/// Files the export writes by fixed name (others are the music and
/// pictures, named by content: `<stem>.<16 hex>.<ext>`).
pub const OUTPUT_FILES: [&str; 6] = ["index.html", "film.js", "film.wasm", "film.wasm.br", "CREDITS.txt", "README.txt"];

/// `film.js`: the platform's web runtime without the stripped sections,
/// and the start (`makepad_web_pack::js`).
pub fn film_js(source_root: &Path, stripped: &[&str]) -> Result<String, Diagnostic> {
    makepad_web_pack::js::runtime_js(source_root, stripped, "film").map_err(|e| Diagnostic::error(Phase::Write, e))
}

/// The credits as one text (CREDITS.txt): the author's, then the notices.
pub fn credits_text(author: &str, fonts: &[ShippedFont]) -> String {
    let author = author.trim();
    let notices = notices_text(fonts);
    if author.is_empty() { notices } else { format!("{author}\n\n{notices}") }
}

/// The notices under the author's credits: the fonts the film ships with
/// their licence, then the engine.
pub fn notices_text(fonts: &[ShippedFont]) -> String {
    let mut out = String::new();
    if !fonts.is_empty() {
        out.push_str("Fonts\n\n");
        for f in fonts {
            out.push_str(&format!("- {}", f.family));
            if let Some(from) = &f.renamed_from {
                out.push_str(&format!(" (a subset of {from}, renamed as its licence's Reserved Font Name requires)"));
            }
            out.push_str(&format!(": {} glyphs of the original, subsetted for this film.\n", f.glyphs));
            if !f.copyright.is_empty() {
                out.push_str(&format!("  {}\n", f.copyright.trim()));
            }
            if !f.license.is_empty() {
                out.push_str(&format!("  {}\n", f.license.trim()));
            }
        }
        out.push_str("\nThese fonts are licensed under the SIL Open Font License, Version 1.1:\n\n");
        out.push_str(OFL_TEXT.trim());
        out.push_str("\n\n");
    }
    out.push_str("Engine: Makepad (https://makepad.nl).\n");
    out
}

/// The SIL Open Font License 1.1, as every shipped font's notice quotes it.
const OFL_TEXT: &str = include_str!("ofl.txt");

/// HTML-escaped text with its URLs as links.
fn html_text(s: &str) -> String {
    let mut out = String::new();
    for (i, word) in s.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let esc = |w: &str| w.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let lead = word.len() - word.trim_start_matches(['(', '[', '"']).len();
        let body = &word[lead..];
        let url = body.trim_end_matches([')', ']', '.', ',', ';', '"']);
        if url.starts_with("https://") || url.starts_with("http://") {
            let href = url.replace('&', "&amp;").replace('"', "&quot;");
            out.push_str(&format!("{}<a href=\"{href}\" target=\"_blank\" rel=\"noopener\">{}</a>{}", esc(&word[..lead]), esc(url), esc(&body[url.len()..])));
        } else {
            out.push_str(&esc(word));
        }
    }
    out
}

/// `index.html`: the canvas, the loader until the film draws, the credits.
/// The author's credits lead the dialog, one paragraph each; the font
/// notices and the engine fold away under them.
pub fn index_html(bundle: &Bundle, author: &str, notices: &str, wasm_raw: usize, wasm_br: usize) -> String {
    let title = if bundle.title.is_empty() { "Film".to_string() } else { bundle.title.clone() };
    let title_html = html_text(&title);
    let author_html: String = author.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).map(|p| format!("<p>{}</p>", html_text(&p.replace('\n', " ")))).collect();
    let notices_html: String = notices.lines().map(|l| format!("{}\n", html_text(l))).collect();
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no">
<title>{title_html}</title>
<link rel="modulepreload" href="film.js">
<link rel="icon" href="data:,">
<style>
html, body {{ margin: 0; height: 100%; overflow: hidden; background: #000; }}
body {{ position: fixed; inset: 0; }}
canvas.full_canvas {{ width: 100%; height: 100%; display: block; touch-action: none; user-select: none; }}
.canvas_loader {{ position: fixed; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; background: #eee9df; color: #0a0a0b; font: 13px/1.4 ui-monospace, Menlo, monospace; }}
.canvas_loader h1 {{ font: 700 28px/1.1 system-ui, sans-serif; letter-spacing: 0.04em; margin: 24px 0 6px; }}
.canvas_loader .note {{ opacity: 0.6; }}
.canvas_loader .error {{ color: #b3200e; max-width: 34em; text-align: center; }}
#credits_link {{ position: fixed; right: 12px; bottom: 10px; z-index: 10; font: 12px system-ui, sans-serif; color: rgba(238,233,223,0.7); background: rgba(10,10,11,0.4); border: 0; padding: 4px 8px; border-radius: 3px; cursor: pointer; }}
#credits_link:hover {{ color: #eee9df; }}
#credits {{ max-width: min(44em, 90vw); max-height: 80vh; overflow: auto; background: #eee9df; color: #0a0a0b; border: 0; border-radius: 4px; padding: 20px 24px; font: 13px/1.5 system-ui, sans-serif; }}
#credits h2 {{ font-size: 18px; margin: 0 0 10px; }}
#credits p {{ font-size: 15px; margin: 0 0 12px; }}
#credits details {{ margin-top: 16px; font-size: 12px; color: #444; }}
#credits summary {{ cursor: pointer; }}
#credits pre {{ white-space: pre-wrap; font: inherit; margin: 8px 0 0; }}
#credits a {{ color: #b3200e; }}
#credits::backdrop {{ background: rgba(0,0,0,0.6); }}
</style>
</head>
<body>
<canvas class="full_canvas"></canvas>
<div class="canvas_loader">
<svg width="140" height="140" viewBox="0 0 140 140" aria-hidden="true">
<circle cx="70" cy="70" r="62" fill="none" stroke="#0a0a0b" stroke-opacity="0.45" stroke-width="1"/>
</svg>
<h1>{title_html}</h1>
<div class="note" id="loader_note">loading</div>
</div>
<button id="credits_link" type="button">Credits</button>
<dialog id="credits"><h2>Credits</h2>{author_html}<details><summary>Fonts and licences</summary><pre>{notices_html}</pre></details><form method="dialog"><p><button>Close</button></p></form></dialog>
<script type="module">
import {{ start_app }} from "./film.js";
const FILM_SIZES = {{ raw: {wasm_raw}, br: {wasm_br} }};
const credits = document.getElementById("credits");
document.getElementById("credits_link").onclick = () => credits.showModal();
const note = document.getElementById("loader_note");
// No progress here: the code's download is only the first step of the start,
// and a bar that reached 100 % before the film's own loader began counting
// read as a start that went back. The film's loader (drawn by the player)
// shows the one progress, over everything the start needs.
const show = _p => {{}};
if (location.protocol === "file:") {{
    note.className = "error";
    note.textContent = "Browsers do not run a film from a file: serve this folder over HTTP (see README.txt).";
}} else if (!("crossOriginIsolated" in globalThis) || typeof WebAssembly !== "object" || typeof Atomics !== "object") {{
    // A browser without cross-origin isolation (Safari before 15.2) cannot
    // give a page shared memory at all, whatever the host sends.
    note.className = "error";
    note.textContent = "This browser is too old to play this film. It plays in a current Safari (iOS and macOS 15.2 or later), Chrome, Edge or Firefox.";
}} else if (!globalThis.crossOriginIsolated) {{
    // The film runs on threads (shared memory): browsers allow that only on
    // a cross-origin isolated page.
    note.className = "error";
    note.textContent = "This film needs a host that sends two headers with its files: Cross-Origin-Opener-Policy: same-origin and Cross-Origin-Embedder-Policy: require-corp (or credentialless). See README.txt.";
}} else {{
    start_app(document.getElementsByClassName("full_canvas")[0], FILM_SIZES, show).catch(e => {{
        note.className = "error";
        note.textContent = "The film could not start: " + (e && e.message ? e.message : e);
    }});
}}
</script>
</body>
</html>
"##
    )
}

/// `README.txt`: how to host the folder.
pub fn readme(song_file: Option<&str>) -> String {
    let music = match song_file {
        Some(f) => format!("- {f}: the music. The film plays without it (silently), so it can be removed on its own.\n"),
        None => "- (no music file: the film plays silently)\n".to_string(),
    };
    format!(
        "This folder is a film made with Makepad (Stage's web export).\n\n\
Files\n\
- index.html: the page. Open it over HTTP.\n\
- film.js: Makepad's web runtime and the film's start.\n\
- film.wasm.br and film.wasm: the film (code, kernels, shaders, documents, fonts and small pictures in one wasm), brotli-compressed and plain.\n\
{music}\
- other <name>.<hash>.<ext> files: larger pictures, fetched when a scene needs them.\n\
- CREDITS.txt: who made what, and the licences.\n\n\
Hosting\n\
- Any static web server works, copy the folder as it is, if it sends these two headers with every file (the film runs on threads with shared memory, which browsers allow only on a cross-origin isolated page):\n\
    Cross-Origin-Opener-Policy: same-origin\n\
    Cross-Origin-Embedder-Policy: require-corp   (or credentialless)\n\
  Without them the page says so instead of playing. Stage's own server (the Web export card) sends them.\n\
- Serve .wasm as application/wasm. If the server sends film.wasm.br with Content-Encoding: br, the browser decodes it; otherwise the page decodes it itself where the browser can, and fetches film.wasm where it cannot.\n\
- Opening index.html straight from the disk (file://) does not work: browsers do not fetch wasm from files. Use a local server that sends the two headers above.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_link_urls() {
        let html = html_text("lyrics by A. Writer (https://example.org/x), & more.");
        assert!(html.contains("<a href=\"https://example.org/x\""));
        assert!(html.contains("&amp; more."));
    }
}

//! The page: `index.html`, `<name>.js` (the platform's web runtime and the start that fetches
//! one wasm: `makepad_web_pack::js::one_wasm_js`), `<name>.wasm` (the captured module, raw: the
//! host compresses it on the wire), and the external files beside them. Nothing else is written to
//! the output folder.

use crate::coverage::Facts;
use crate::embed::Built;
use crate::packs::MergedPack;
use crate::report::{self, Report};
use crate::{Failure, Options, Phase, Progress};
use makepad_web_pack::output::Output;

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `index.html`: the canvas, a loader line until the app starts, the script.
pub fn index_html(title: &str, name: &str, raw: usize, author: &str, notices: &str) -> String {
    let title = html_escape(if title.trim().is_empty() { name } else { title.trim() });
    let author_html: String = author.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).map(|p| format!("<p>{}</p>", html_text(&p.replace('\n', " ")))).collect();
    let notices_html = html_text(notices);
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no">
<title>{title}</title>
<link rel="modulepreload" href="{name}.js">
<link rel="icon" href="data:,">
<style>
html, body {{ margin: 0; height: 100%; overflow: hidden; background: #000; }}
body {{ position: fixed; inset: 0; }}
canvas.full_canvas {{ width: 100%; height: 100%; display: block; touch-action: none; user-select: none; }}
#loader {{ position: fixed; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; background: #eee9df; color: #0a0a0b; font: 13px/1.4 ui-monospace, Menlo, monospace; }}
#loader h1 {{ font: 700 28px/1.1 system-ui, sans-serif; letter-spacing: 0.04em; margin: 24px 0 6px; }}
#loader_note {{ opacity: 0.6; }}
#loader.error {{ color: #e5533d; padding: 0 16px; text-align: center; }}
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
<div id="loader" class="canvas_loader"><h1>{title}</h1><div id="loader_note">loading</div></div>
<button id="credits_link" type="button">Credits</button>
<dialog id="credits"><h2>Credits</h2>{author_html}<details><summary>Fonts and licences</summary><pre>{notices_html}</pre></details><form method="dialog"><p><button>Close</button></p></form></dialog>
<script type="module">
import {{ start_app }} from "./{name}.js";
const credits = document.getElementById("credits");
document.getElementById("credits_link").onclick = () => credits.showModal();
const loader = document.getElementById("loader");
const note = document.getElementById("loader_note");
const fail = text => {{ loader.className = "error"; note.textContent = text; }};
if (location.protocol === "file:") {{
    fail("Browsers do not run this page from a file: serve this folder over HTTP.");
}} else if (typeof WebAssembly !== "object") {{
    fail("This browser is too old for this page.");
}} else {{
    start_app(document.querySelector("canvas.full_canvas"), {{ raw: {raw} }}, p => {{ note.textContent = `loading ${{Math.round(p * 100)}}%`; }})
        .then(() => {{ globalThis.__bt_ready?.(); }}, e => fail(`${{e}}`));
}}
</script>
</body>
</html>
"##
    )
}

/// Escaped text with HTTP(S) URLs as links, as in the film player's credits.
fn html_text(text: &str) -> String {
    text.split_inclusive(char::is_whitespace).map(|word| {
        let lead = word.len() - word.trim_start_matches(['(', '[', '"']).len();
        let body = &word[lead..];
        let url = body.trim_end().trim_end_matches([')', ']', '.', ',', ';', '"']);
        if url.starts_with("https://") || url.starts_with("http://") {
            format!("{}<a href=\"{}\" target=\"_blank\" rel=\"noopener\">{}</a>{}", html_escape(&word[..lead]), html_escape(url), html_escape(url), html_escape(&body[url.len()..]))
        } else { html_escape(word) }
    }).collect()
}

fn font_notices(built: &Built) -> String {
    let mut notices = String::new();
    for (url, bytes) in &built.embed.files {
        if !built.files.iter().any(|f| f.kind == crate::embed::Kind::Font && makepad_web_manifest::embed::key(&f.url) == Some(url.as_str())) { continue; }
        let Ok(face) = ttf_parser::Face::parse(bytes, 0) else { continue; };
        let get = |id| face.names().into_iter().filter(|n| n.name_id == id && n.is_unicode()).find_map(|n| n.to_string()).unwrap_or_default();
        let family = get(16);
        notices.push_str(&format!("- {} ({url}; subset for this page)\n  {}\n  {}\n\n", if family.is_empty() { get(1) } else { family }, get(0), get(13)));
        if let Some((_, original)) = built.original_fonts.iter().find(|(path, _)| path == url) {
            if let Ok(original) = ttf_parser::Face::parse(original, 0) {
                let original_name = original.names().into_iter().filter(|n| n.name_id == 1 && n.is_unicode()).find_map(|n| n.to_string()).unwrap_or_default();
                notices.push_str(&format!("  Original family: {original_name}. Reserved names are replaced in modified subsets.\n\n"));
            }
        }
    }
    if !notices.is_empty() {
        notices.insert_str(0, "Fonts\n\n");
        notices.push_str(include_str!("ofl.txt"));
        notices.push_str("\n\n");
    }
    notices.push_str("Engine: Makepad (https://makepad.nl).\n");
    notices
}

fn check_folder(opts: &Options, dir: &std::path::Path, requests: &std::collections::BTreeMap<String, Vec<String>>) -> Result<(), String> {
    if !dir.exists() { return Ok(()); }
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() { check_folder(opts, &path, requests)?; }
        else {
            let name = path.strip_prefix(&opts.out_dir).map_err(|e| e.to_string())?.to_string_lossy();
            crate::embed::check_image(opts, &name, requests.get(name.as_ref()).map(Vec::as_slice).unwrap_or(&[]))?;
        }
    }
    Ok(())
}

pub fn write(opts: &Options, wasm: &[u8], built: &Built, pack: &MergedPack, shipped: &Facts, gate: &str, progress: &mut dyn Progress) -> Result<Report, Failure> {
    let fail = |e: String| Failure::new(Phase::Package, e);
    let requests = crate::embed::document_requests(opts, shipped).map_err(fail)?;
    // Audit stale output too: pruning is not a way to conceal a forbidden image.
    check_folder(opts, &opts.out_dir, &requests).map_err(fail)?;
    for (url, _) in &built.embed.files {
        let callers = built.files.iter().find(|f| makepad_web_manifest::embed::key(&f.url) == Some(url.as_str())).map(|f| f.requesters.as_slice()).unwrap_or(&[]);
        crate::embed::check_image(opts, url, callers).map_err(fail)?;
    }
    for external in &built.external {
        crate::embed::check_image(opts, &external.url, requests.get(makepad_web_manifest::embed::key(&external.url).unwrap_or("")).map(Vec::as_slice).unwrap_or(&[])).map_err(fail)?;
    }
    let stripped: Vec<&str> = opts.js_stripped.iter().map(String::as_str).collect();
    let js = makepad_web_pack::js::one_wasm_js(&opts.source_root, &stripped, &opts.name).map_err(fail)?;
    makepad_web_pack::js::check_exports(&js, wasm).map_err(fail)?;
    // the module ships raw: the host compresses it on the wire (HTTP brotli, as the rustc film
    // page's host does); a zlib copy inside the page cost ~30% over brotli and an inflate
    progress.phase(Phase::Package, 0.3, "measuring (brotli 11)");
    let wasm_br = makepad_web_pack::brotli(wasm).len();
    let author = opts.credits.as_ref().map(|path| std::fs::read_to_string(path).map_err(|e| fail(format!("credits {}: {e}", path.display())))).transpose()?.unwrap_or_default();
    let notices = font_notices(built);
    let html = index_html(&opts.title, &opts.name, wasm.len(), &author, &notices);
    let credits = format!("{}\n\n{notices}", author.trim());

    let js_name = format!("{}.js", opts.name);
    let wasm_name = format!("{}.wasm", opts.name);
    let mut fixed: Vec<String> = vec!["index.html".into(), js_name.clone(), wasm_name.clone(), "CREDITS".into()];
    for x in &built.external {
        if x.name.contains('/') {
            return Err(fail(format!("{}: an external file goes beside the page (`{}` has a folder)", x.url, x.name)));
        }
        fixed.push(x.name.clone());
    }
    let fixed_refs: Vec<&str> = fixed.iter().map(String::as_str).collect();
    let mut out = Output::new(&opts.out_dir, &fixed_refs).map_err(fail)?;
    progress.phase(Phase::Package, 0.6, "writing");
    out.write_sized(&wasm_name, wasm, Some(wasm_br)).map_err(fail)?;
    out.write(&js_name, js.as_bytes()).map_err(fail)?;
    out.write("index.html", html.as_bytes()).map_err(fail)?;
    out.write("CREDITS", credits.as_bytes()).map_err(fail)?;
    for x in &built.external {
        let bytes = std::fs::read(&x.from).map_err(|e| fail(format!("{}: {e}", x.from.display())))?;
        out.write_sized(&x.name, &bytes, Some(bytes.len())).map_err(fail)?;
    }
    out.prune().map_err(fail)?;
    progress.phase(Phase::Package, 0.8, "the size report");
    Ok(report::Report::new(opts, wasm, wasm_br, built, pack, shipped, gate, out.written()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_loads_its_one_script() {
        let html = index_html("Harbour (night) <film>", "player", 10, "", "");
        assert!(html.contains("<title>Harbour (night) &lt;film&gt;</title>"));
        assert!(html.contains("from \"./player.js\"") && html.contains("raw: 10 }"));
    }
}

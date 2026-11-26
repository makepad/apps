//! The embed (`makepad_web_manifest::embed`): every file and directory listing the trace runs
//! fetched, answered from the wasm instead of the network.
//!
//! - a font file the runs drew from: cut to the glyphs they drew and what the font's
//!   substitutions reach from them (`makepad_font_subset`: ids kept, every character whose
//!   glyph is kept stays mapped);
//! - a Splash text: the top-level statements no run used blanked to spaces, newlines kept
//!   (`makepad_web_pack::splash_blank`: the kept statements stay verbatim at their offsets);
//! - a directory: its listing as the trace host served it (`f <size> <name>` / `d - <name>`);
//! - anything else: as it is.
//!
//! A file under an `--external` prefix is not embedded: it is copied beside the page (the
//! prefix taken off its path) and the page fetches it.

use crate::coverage::{Asset, Facts};
use crate::{Failure, Options, Phase, Progress};
use makepad_web_manifest::embed::{key, Embed};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What an embedded file is, for the size report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Splash,
    Font,
    Listing,
    Data,
    Texture,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Splash => "document Splash",
            Kind::Font => "glyphs (fonts)",
            Kind::Listing => "directory listings",
            Kind::Data => "media and data",
            Kind::Texture => "allowed image textures",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Embedded {
    pub url: String,
    pub kind: Kind,
    /// The file as it is on disk, and as embedded.
    pub before: usize,
    pub after: usize,
    pub requesters: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct External {
    pub url: String,
    pub from: PathBuf,
    /// Its path beside the page.
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Built {
    pub embed: Embed,
    /// Original font bodies for the held-out replay, never used to grow the training set.
    pub original_fonts: Vec<(String, Vec<u8>)>,
    pub files: Vec<Embedded>,
    pub external: Vec<External>,
}

impl Built {
    pub fn inventory_json(&self) -> String {
        use makepad_strict_json::{self as json, Value};
        Value::Arr(self.files.iter().map(|file| json::obj(vec![
            ("url", json::s(&file.url)),
            ("kind", json::s(file.kind.name())),
            ("source_bytes", Value::Int(file.before as i64)),
            ("embedded_bytes", Value::Int(file.after as i64)),
            ("requesters", Value::Arr(file.requesters.iter().map(|s| json::s(s)).collect())),
        ])).collect()).to_json()
    }

    pub fn summary(&self) -> String {
        let (before, after) = self.files.iter().fold((0, 0), |(b, a), f| (b + f.before, a + f.after));
        format!("{} files embedded ({} of {} bytes), {} beside the page", self.files.len(), after, before, self.external.len())
    }

    pub fn bytes_of(&self, kind: Kind) -> usize {
        self.files.iter().filter(|f| f.kind == kind).map(|f| f.after).sum()
    }

    /// The embedded bodies of one kind, concatenated (the report compresses them).
    pub fn bodies_of(&self, kind: Kind) -> Vec<u8> {
        let mut out = Vec::new();
        for f in self.files.iter().filter(|f| f.kind == kind) {
            if let Some((_, body)) = self.embed.files.iter().find(|(u, _)| u == key(&f.url).unwrap_or("")) {
                out.extend_from_slice(body);
            }
        }
        out
    }
}

/// A directory's listing as the trace host serves it.
pub fn listing(dir: &Path) -> Result<Vec<u8>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut lines: Vec<String> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let ty = e.file_type().ok()?;
            let name = e.file_name().into_string().ok()?;
            if ty.is_dir() {
                Some(format!("d - {}", name))
            } else if ty.is_file() {
                Some(format!("f {} {}", e.metadata().ok()?.len(), name))
            } else {
                None
            }
        })
        .collect();
    lines.sort();
    Ok(lines.into_iter().map(|l| l + "\n").collect::<String>().into_bytes())
}

/// Keep drawn glyphs and source cmap inputs, then close GSUB. The existing subsetter adds
/// composite components and rewrites GSUB/GPOS/GDEF while preserving glyph ids and metrics.
pub fn cut_font(font: &[u8], glyphs: &BTreeSet<u16>, chars: &BTreeSet<char>) -> Result<Vec<u8>, String> {
    let face = ttf_parser::Face::parse(font, 0).map_err(|e| format!("{e:?}"))?;
    let mut keep = glyphs.clone();
    keep.extend(chars.iter().filter_map(|&c| face.glyph_index(c)).map(|g| g.0));
    makepad_font_subset::gsub_closure(&face, &mut keep);
    let copyright = face.names().into_iter().filter(|n| n.name_id == 0 && n.is_unicode()).find_map(|n| n.to_string()).unwrap_or_default();
    let reserved = makepad_font_subset::reserved_font_names(&copyright);
    let rename = if reserved.is_empty() { None } else {
        let hash = font.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
        Some(makepad_font_subset::Rename { reserved, family: format!("Makepad Subset {hash:016x}") })
    };
    makepad_font_subset::subset(font, &keep, chars, &makepad_font_subset::SubsetOptions { rename, ..Default::default() }).map_err(|e| e.to_string())
}

fn is_splash(url: &str) -> bool {
    url.ends_with(".splash")
}

fn image_extension(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}

/// Image policy is applied before either embedding or external copying.
pub fn check_image(opts: &Options, url: &str, requesters: &[String]) -> Result<(), String> {
    match image_extension(url).as_str() {
        "jpg" | "jpeg" => Err(format!("{url}: JPEG images are forbidden")),
        "png" | "gif" | "webp" => {
            if !opts.allow_images.iter().any(|allowed| key(allowed) == key(url)) {
                return Err(format!("{url}: image is not on the caller's --allow-image list"));
            }
            if requesters.is_empty() { return Err(format!("{url}: image is not referenced by a document")); }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Resolve document references from parsed syntax, never comments or unrelated
/// string literals. Unadorned paths use the document tree, ./ and ../ the caller.
pub fn document_requests(opts: &Options, facts: &Facts) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    use makepad_platform::makepad_script::syntax;
    let query_value = |name: &str| opts.query.trim_start_matches('?').split('&').find_map(|part| part.split_once('=').filter(|(k, _)| *k == name).map(|(_, v)| v));
    let root = query_value("tree").or_else(|| query_value("doc").and_then(|doc| doc.rsplit_once('/').map(|(dir, _)| dir))).unwrap_or("");
    let mut requests = std::collections::BTreeMap::<String, Vec<String>>::new();
    for argument in ["doc", "tree-descriptor"] {
        if let Some(path) = query_value(argument).and_then(key) {
            requests.entry(path.into()).or_default().push(format!("{}: --{argument}", opts.package));
        }
    }
    // Match authored family declarations against the original font names. The
    // inventory describes the document's requests before subsetting/renaming.
    let normal = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    let mut families = std::collections::BTreeMap::<String, Vec<String>>::new();
    for asset in &facts.assets {
        if !matches!(image_extension(&asset.url).as_str(), "ttf" | "otf" | "ttc") { continue; }
        let bytes = std::fs::read(&asset.path).map_err(|e| format!("{}: {e}", asset.url))?;
        if let Ok(face) = ttf_parser::Face::parse(&bytes, 0) {
            for name in face.names().into_iter().filter(|n| matches!(n.name_id, 1 | 16) && n.is_unicode()) {
                if let Some(name) = name.to_string() {
                    let urls = families.entry(normal(&name)).or_default();
                    if let Some(url) = key(&asset.url) { if !urls.iter().any(|u| u == url) { urls.push(url.into()); } }
                }
            }
        }
    }
    for asset in facts.assets.iter().filter(|a| is_splash(&a.url)) {
        let text = std::fs::read_to_string(&asset.path).map_err(|e| format!("{}: {e}", asset.url))?;
        let parsed = syntax::parse(&text).map_err(|e| format!("{}: {e}", asset.url))?;
        let mut paths = Vec::new();
        parsed.walk(&mut |_, node| {
            let caller = || format!("{}:{}", asset.url, text.as_bytes()[..node.span.start].iter().filter(|&&c| c == b'\n').count() + 1);
            if let Some((ty, fields)) = node.object() {
                if ty.is_some_and(|ty| ty.rsplit('.').next() == Some("Font")) {
                    for field in fields.iter().filter(|f| f.name.as_deref() == Some("family")) {
                        if let Some(family) = field.value.string() {
                            if let Some(urls) = families.get(&normal(family)) {
                                for url in urls { requests.entry(url.clone()).or_default().push(format!("{}: Font family {family}", caller())); }
                            }
                        }
                    }
                }
                for field in fields.iter().filter(|f| matches!(f.name.as_deref(), Some("src" | "graphic"))) {
                    if let Some(path) = field.value.string() { paths.push((path.to_string(), caller())); }
                }
            }
            if let syntax::Kind::Call { callee, args, .. } = &node.kind {
                let callee = callee.ident().unwrap_or("");
                let name = ["mod.cut.", "mod.deck.", "mod.motion.", "mod.forms."].iter().find_map(|prefix| callee.strip_prefix(*prefix)).unwrap_or(callee);
                if matches!(name, "graphic" | "import") {
                    if let [arg] = args.as_slice() { if let Some(path) = arg.string() { paths.push((path.to_string(), caller())); } }
                }
            }
        });
        for (mut path, caller) in paths {
            if !path.rsplit('/').next().unwrap_or("").contains('.') { path.push_str(".splash"); }
            let base = if path.starts_with("./") || path.starts_with("../") { key(&asset.url).unwrap_or("").rsplit_once('/').map_or("", |(dir, _)| dir) } else { root };
            let joined = format!("{base}/{path}");
            let mut components = Vec::new();
            for part in joined.split('/') {
                match part { "" | "." => {}, ".." => { components.pop(); }, part => components.push(part) }
            }
            let target = components.join("/");
            let callers = requests.entry(target).or_default();
            if !callers.contains(&caller) { callers.push(caller); }
        }
    }
    Ok(requests)
}

pub fn build(opts: &Options, facts: &Facts, progress: &mut dyn Progress) -> Result<Built, Failure> {
    let requests = document_requests(opts, facts).map_err(|e| Failure::new(Phase::Embed, e))?;
    let mut built = Built { embed: Embed { query: opts.query.clone(), files: Vec::new() }, ..Default::default() };
    let mut assets: Vec<&Asset> = facts.assets.iter().collect();
    assets.sort_by(|a, b| a.url.cmp(&b.url));
    let n = assets.len().max(1);
    for (i, a) in assets.into_iter().enumerate() {
        progress.phase(Phase::Embed, i as f64 / n as f64, &a.url);
        let Some(k) = key(&a.url) else { continue };
        let fail = |e: String| Failure::new(Phase::Embed, format!("{}: {e}", a.url));
        let requesters = requests.get(k).cloned().unwrap_or_default();
        check_image(opts, &a.url, &requesters).map_err(fail)?;
        if let Some(prefix) = opts.external.iter().find(|p| k.starts_with(p.trim_start_matches('/'))) {
            if k.is_empty() || k.ends_with('/') {
                continue;
            }
            let prefix = prefix.trim_start_matches('/');
            let name = if k == prefix { k.rsplit('/').next().unwrap() } else { k[prefix.len()..].trim_start_matches('/') }.to_string();
            built.external.push(External { url: a.url.clone(), from: a.path.clone(), name, size: a.size });
            continue;
        }
        let (kind, before, body) = if k.ends_with('/') || k.is_empty() {
            let body = listing(&a.path).map_err(fail)?;
            (Kind::Listing, body.len(), body)
        } else {
            let bytes = std::fs::read(&a.path).map_err(|e| fail(format!("{}: {e}", a.path.display())))?;
            let before = bytes.len();
            if facts.glyphs.contains_key(&a.url) || ttf_parser::Face::parse(&bytes, 0).is_ok()
                || matches!(bytes.get(..4), Some(b"\0\x01\0\0" | b"OTTO" | b"ttcf" | b"true"))
            {
                // A fetched but undrawn face has an empty training set. Keeping it whole
                // would hide both lost recording and the largest remaining font payloads.
                let empty = BTreeSet::new();
                let glyphs = facts.glyphs.get(&a.url).unwrap_or(&empty);
                let empty_chars = BTreeSet::new();
                let chars = facts.glyph_chars.get(&a.url).unwrap_or(&empty_chars);
                let cut = cut_font(&bytes, glyphs, chars).map_err(fail)?;
                progress.note(Phase::Embed, &format!("{}: {} drawn glyphs, {} -> {} bytes", a.url, glyphs.len(), before, cut.len()));
                built.original_fonts.push((k.to_string(), bytes));
                (Kind::Font, before, cut)
            } else if is_splash(k) {
                let ranges = facts.unused_ranges(&bytes);
                match std::str::from_utf8(&bytes) {
                    Ok(text) if !ranges.is_empty() => (Kind::Splash, before, makepad_web_pack::splash_blank::blank(text, &ranges).into_bytes()),
                    _ => (Kind::Splash, before, bytes),
                }
            } else {
                (if matches!(image_extension(k).as_str(), "png" | "gif" | "webp") { Kind::Texture } else { Kind::Data }, before, bytes)
            }
        };
        let requesters = if requesters.is_empty() { vec![format!("{}: {} ({})", opts.package, if kind == Kind::Font { "application font declaration or text fallback" } else { "application resource request" }, a.url)] } else { requesters };
        progress.note(Phase::Embed, &format!("{}: {} bytes; requested by {}", a.url, body.len(), requesters.join(", ")));
        built.files.push(Embedded { url: a.url.clone(), kind, before, after: body.len(), requesters });
        built.embed.files.push((k.to_string(), body));
    }
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listings_are_the_trace_hosts() {
        let dir = std::env::temp_dir().join(format!("wasm-export-listing-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.splash"), b"abc").unwrap();
        assert_eq!(String::from_utf8(listing(&dir).unwrap()).unwrap(), "d - sub\nf 3 b.splash\n");
        let _ = std::fs::remove_file(dir.join("b.splash"));
        let _ = std::fs::remove_dir(dir.join("sub"));
        let _ = std::fs::remove_dir(&dir);
    }
}

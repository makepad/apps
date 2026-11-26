//! Sizes: every output file raw and brotli, and the film wasm by part.

use crate::link::Linked;
use makepad_story_film_pack::{names, Pack};

pub use makepad_web_pack::output::SizedFile;
pub use makepad_web_pack::{brotli, mb};

/// One part of the film wasm, raw and brotli (each part compressed on its
/// own, at quality 9, so the parts add up to a little more than the whole).
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub name: String,
    pub bytes: usize,
    pub brotli: usize,
}


fn part(name: &str, bytes: &[u8]) -> Part {
    Part { name: name.to_string(), bytes: bytes.len(), brotli: makepad_web_pack::brotli_estimate(bytes) }
}

/// The film wasm by part: code (what the wasm is besides its pack: the
/// runtime and the shader table), then the pack's contents by kind:
/// kernels, documents, fonts, pictures, data.
pub fn breakdown(optimised: &[u8], _linked: &Linked, pack_bytes: &[u8], pack: &Pack) -> Vec<Part> {
    let mut kinds: std::collections::BTreeMap<&str, Vec<u8>> = std::collections::BTreeMap::new();
    for (name, bytes) in pack.iter() {
        let kind = match name {
            names::KERNELS | names::KERNEL_KEYS => "kernels",
            names::AUDIO | names::AUDIO_KEYS => "audio",
            names::SHADERS => "shaders",
            "manifest.json" => "data",
            _ => makepad_story_player_bundle::kind_of(name),
        };
        kinds.entry(kind).or_default().extend_from_slice(bytes);
    }
    let code = optimised.len().saturating_sub(pack_bytes.len());
    let mut parts = vec![Part { name: "code".into(), bytes: code, brotli: 0 }];
    // The code's brotli: the whole wasm's, less the pack's on its own.
    let whole = makepad_web_pack::brotli_estimate(optimised);
    let packed = makepad_web_pack::brotli_estimate(pack_bytes);
    parts[0].brotli = whole.saturating_sub(packed);
    for (kind, bytes) in &kinds {
        parts.push(part(kind, bytes));
    }
    parts
}

/// The film's brotli budget by part (bytes): the whole film.wasm within
/// about 2 MB.
pub const BUDGET: [(&str, usize); 6] = [("code", 1_000_000), ("kernels", 300_000), ("shaders", 200_000), ("fonts", 150_000), ("docs", 400_000), ("total", 2_000_000)];

/// A part's budget, when it has one.
pub fn budget(part: &str) -> Option<usize> {
    BUDGET.iter().find(|(n, _)| *n == part).map(|(_, b)| *b)
}

/// A table for a terminal: each part raw and brotli against its budget,
/// the biggest overrun flagged.
pub fn table(parts: &[Part], files: &[SizedFile]) -> String {
    let mut out = String::from("film.wasm by part (brotli against budget):\n");
    let worst = parts.iter().filter_map(|p| budget(&p.name).map(|b| (p.name.as_str(), p.brotli as f64 / b as f64))).filter(|(_, r)| *r > 1.0).fold(None, |w: Option<(&str, f64)>, x| if w.is_none_or(|w| x.1 > w.1) { Some(x) } else { w });
    for p in parts {
        let b = budget(&p.name).map_or(String::from("-"), mb);
        let flag = if worst.is_some_and(|(n, _)| n == p.name) { "  <- biggest overrun" } else { "" };
        out.push_str(&format!("  {:10} {:>12} {:>12} brotli / {:>9}{flag}\n", p.name, mb(p.bytes), mb(p.brotli), b));
    }
    let total: usize = parts.iter().map(|p| p.brotli).sum();
    out.push_str(&format!("  {:10} {:>12} {:>12} brotli / {:>9}\n", "total", "", mb(total), mb(2_000_000)));
    out.push_str("files:\n");
    for f in files {
        out.push_str(&format!("  {:40} {:>12} {:>12} brotli\n", f.name, mb(f.bytes), mb(f.brotli)));
    }
    out
}

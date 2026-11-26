//! What the export reads from a coverage file (`relax-web-trace/1`, the trace host's
//! trace.rs): the files fetched, the glyphs drawn per font file, the Splash items and whether a
//! run used them, the runs' panics.

use makepad_strict_json::{self as json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A file (or a directory listing: the URL ends in `/`) the page fetched.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    /// `/film/film.cut.splash`.
    pub url: String,
    /// Where it is on this machine.
    pub path: PathBuf,
    pub size: u64,
}

/// A top-level Splash statement of a text a VM evaluated.
#[derive(Clone, Debug, PartialEq)]
pub struct SplashItem {
    pub id: String,
    /// FNV-1a 64 of the text the statement is in.
    pub block_hash: u64,
    /// Byte range in that text.
    pub start: usize,
    pub end: usize,
    /// None: defines no object (always kept).
    pub used: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub assets: Vec<Asset>,
    pub glyphs: BTreeMap<String, BTreeSet<u16>>,
    pub glyph_chars: BTreeMap<String, BTreeSet<char>>,
    pub splash: Vec<SplashItem>,
    /// The crates' Splash (script_mod! texts): (bytes, kept bytes) over every crate.
    pub crate_splash: (u64, u64),
    pub functions: usize,
    pub panics: Vec<String>,
    pub errors: Vec<String>,
    pub frame_ticks: u64,
    pub frames: Vec<(u64, u64, u64, u64, String)>,
}

/// FNV-1a 64 (the trace's file and block hashes).
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_arr).unwrap_or(&[])
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn num(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

impl Facts {
    pub fn read(path: &Path) -> Result<Facts, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let j = json::parse_depth(&bytes, 64).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_json(&j)
    }

    pub fn from_json(j: &Value) -> Result<Facts, String> {
        if text(j, "format") != "relax-web-trace/1" {
            return Err(format!("not a relax-web-trace/1 coverage file (format `{}`)", text(j, "format")));
        }
        let mut f = Facts::default();
        for a in arr(j, "assets") {
            f.assets.push(Asset { url: text(a, "url"), path: PathBuf::from(text(a, "path")), size: num(a, "size") });
        }
        for g in arr(j, "glyphs") {
            let ids = f.glyphs.entry(text(g, "url")).or_default();
            for id in arr(g, "glyphs") {
                let id = id.as_u64().and_then(|id| u16::try_from(id).ok()).ok_or("invalid recorded glyph id")?;
                ids.insert(id);
            }
        }
        for g in arr(j, "glyph_chars") {
            let chars = f.glyph_chars.entry(text(g, "url")).or_default();
            for c in arr(g, "glyph_chars") {
                let c = c.as_u64().and_then(|c| u32::try_from(c).ok()).and_then(char::from_u32)
                    .ok_or("invalid recorded font character")?;
                chars.insert(c);
            }
        }
        if let Some(splash) = j.get("splash") {
            for c in arr(splash, "crates") {
                f.crate_splash.0 += num(c, "bytes");
                f.crate_splash.1 += num(c, "used_bytes");
            }
            for i in arr(splash, "items") {
                let used = match i.get("used") {
                    Some(v) if v.is_null() => None,
                    Some(v) => v.as_bool(),
                    None => None,
                };
                let hash = u64::from_str_radix(text(i, "block_hash").trim_start_matches("0x"), 16).unwrap_or(0);
                f.splash.push(SplashItem { id: text(i, "id"), block_hash: hash, start: num(i, "start") as usize, end: num(i, "end") as usize, used });
            }
        }
        f.functions = arr(j, "functions").len();
        for frame in arr(j, "frames") {
            let digest = text(frame, "digest");
            if digest.len() != 16 || u64::from_str_radix(&digest, 16).is_err() {
                return Err("a frame has no valid digest".into());
            }
            f.frames.push((num(frame, "draw_calls"), num(frame, "passes"), num(frame, "upload_bytes"), num(frame, "texture_bytes"), digest));
        }
        for r in arr(j, "runs") {
            f.frame_ticks += num(r, "frames");
            for p in arr(r, "panics") {
                f.panics.push(format!("{}: {}", text(r, "name"), p.as_str().unwrap_or("")));
            }
            for e in arr(r, "errors") {
                f.errors.push(format!("{}: {}", text(r, "name"), e.as_str().unwrap_or("")));
            }
        }
        Ok(f)
    }

    /// The byte ranges of `text`'s statements no run used (its hash names it).
    pub fn unused_ranges(&self, text: &[u8]) -> Vec<(usize, usize)> {
        let hash = fnv1a64(text);
        let mut ranges: Vec<(usize, usize)> = self
            .splash
            .iter()
            .filter(|i| i.block_hash == hash && i.used == Some(false) && i.start < i.end && i.end <= text.len())
            .map(|i| (i.start, i.end))
            .collect();
        ranges.sort_unstable();
        ranges.dedup();
        ranges
    }
}

/// Compare B with the original fonts against the drawn training union, before comparing B's
/// subset frames. A removed character mapping must not disguise a new glyph as `.notdef`.
pub fn gate_fonts(training: &Facts, original: &Facts, subset: &Facts) -> Result<String, String> {
    let mut misses = Vec::new();
    for (url, glyphs) in &original.glyphs {
        let empty = BTreeSet::new();
        let trained = training.glyphs.get(url).unwrap_or(&empty);
        for glyph in glyphs.difference(trained) {
            misses.push(format!("glyphs: {url}: {glyph}"));
        }
    }
    if !misses.is_empty() {
        return Err(format!("{} glyph stub hits\n{}", misses.len(), misses.join("\n")));
    }
    if original.glyphs != subset.glyphs {
        return Err("the subset replay shaped different glyphs than the original-font replay".into());
    }
    if original.frame_ticks != subset.frame_ticks || original.frames.len() != subset.frames.len() {
        return Err(format!("font replay frame counts differ: original {}/{}, subset {}/{}", original.frames.len(), original.frame_ticks, subset.frames.len(), subset.frame_ticks));
    }
    for (i, (a, b)) in original.frames.iter().zip(&subset.frames).enumerate() {
        if a != b {
            return Err(format!("font replay frame {i} differs: original {a:?}, subset {b:?}"));
        }
    }
    Ok(format!("glyphs: 0 stub hits; {} frame digests identical across {} frame ticks", original.frames.len(), original.frame_ticks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_export_needs() {
        let src = br#"let a = 1
let b = 2
"#;
        let hash = format!("{:016x}", fnv1a64(src));
        let doc = format!(
            r#"{{"format": "relax-web-trace/1", "runs": [{{"name": "r", "panics": ["boom"]}}],
            "functions": [{{"key": "f"}}],
            "assets": [{{"url": "/film/a.splash", "path": "/x/a.splash", "size": 20, "ranges": [[0, 20]], "fetches": 1}}],
            "glyphs": [{{"url": "/fonts/a.ttf", "glyphs": [3, 7]}}],
            "splash": {{"crates": [{{"crate": "c", "bytes": 100, "used_bytes": 40}}],
              "items": [{{"id": "m::b", "block_hash": "{hash}", "start": 10, "end": 19, "used": false}},
                        {{"id": "m::a", "block_hash": "{hash}", "start": 0, "end": 9, "used": true}},
                        {{"id": "m#0", "block_hash": "{hash}", "start": 0, "end": 3, "used": null}}]}}}}"#
        );
        let f = Facts::from_json(&json::parse(doc.as_bytes()).unwrap()).unwrap();
        assert_eq!(f.assets[0].url, "/film/a.splash");
        assert_eq!(f.glyphs["/fonts/a.ttf"].iter().copied().collect::<Vec<_>>(), vec![3, 7]);
        assert_eq!(f.crate_splash, (100, 40));
        assert_eq!(f.functions, 1);
        assert_eq!(f.panics, vec!["r: boom".to_string()]);
        assert_eq!(f.unused_ranges(src), vec![(10, 19)]);
        assert!(f.unused_ranges(b"other text").is_empty());
    }
}

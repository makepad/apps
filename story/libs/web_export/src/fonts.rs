//! Fonts cut to the glyphs the film can show.
//!
//! The analysis recorded, per font file (by its content hash), every glyph
//! a layout returned and every row of text it appeared in (with how many
//! frames showed it), and the coverage documents declared (`glyphs:` on a
//! Text or a Draw2D text; a number Text declares digits). Each face is then
//! classified ([`Coverage`]) and cut:
//!
//! - **static**: only the glyphs it was seen drawing;
//! - **known sets**: plus every digit and number sign when it shows a
//!   counter (text that differs only in its digits), plus every character
//!   of the song's lyrics when it shows lyric words;
//! - **declared**: plus the declared ranges;
//! - **open**: text the analysis cannot bound (strings that change from
//!   frame to frame in more than their digits, e.g. a scramble, or
//!   `glyphs: @all`): the face ships complete.
//!
//! When in doubt the face keeps more: a missing glyph at run time is a
//! bug, a few kilobytes are not. Every face gets a note in the report:
//! subset or full, glyphs, size, and why.
//!
//! Licences: the fonts are under the SIL Open Font License; a font whose
//! notice reserves a font name is renamed in its subset (the OFL treats a
//! subset as a modified version), and every shipped font's copyright and
//! licence go into the credits ([`ShippedFont`]).

use crate::analyse::Analysis;
use crate::{Diagnostic, FontsKnob, Phase, Progress};
use makepad_font_subset::{reserved_font_names, ClosureInputs, Rename, ShapeRun, SubsetOptions};
use makepad_story_player_bundle::{content_hash_hex, kind_of, Bundle};
use makepad_web_pack::coverage::{classify, same_family, Coverage};
use std::collections::BTreeSet;

/// A font the film ships, for the credits and the report.
#[derive(Clone, Debug, PartialEq)]
pub struct ShippedFont {
    /// The logical name of its file in the bundle.
    pub logical: String,
    /// Its family name as the shipped file names it.
    pub family: String,
    /// The original family name when the subset was renamed.
    pub renamed_from: Option<String>,
    pub copyright: String,
    pub license: String,
    /// Subset (true) or complete.
    pub subset: bool,
    pub glyphs: usize,
    pub glyphs_total: usize,
    pub bytes_before: usize,
    pub bytes_after: usize,
    /// Why it was cut this way.
    pub why: String,
}

/// The lyrics' characters (every `lyrics:` file of the bundle, as text).
fn lyric_text(bundle: &Bundle) -> String {
    bundle.entries.iter().filter(|(l, _)| l.starts_with("lyrics:")).map(|(_, e)| String::from_utf8_lossy(&e.bytes).to_string()).collect::<Vec<_>>().join("\n")
}

/// The web path a wasm build serves a source-tree file at: the crate that
/// holds it (its nearest Cargo.toml's package name, `-` as `_`) and the
/// path inside it (`makepad_widgets/resources/Inter.ttf`).
pub fn web_path(file: &std::path::Path) -> Option<String> {
    let file = file.canonicalize().ok()?;
    let mut dir = file.parent();
    while let Some(d) = dir {
        if let Ok(text) = std::fs::read_to_string(d.join("Cargo.toml")) {
            let name = text
                .lines()
                .skip_while(|l| l.trim() != "[package]")
                .find_map(|l| l.trim().strip_prefix("name").map(|r| r.trim_start().trim_start_matches('=').trim().trim_matches('"').to_string()))?;
            let rel = file.strip_prefix(d).ok()?.to_string_lossy().replace('\\', "/");
            return Some(format!("{}/{rel}", name.replace('-', "_")));
        }
        dir = d.parent();
    }
    None
}

/// Adds the fonts the analysis saw laid out that the bundle does not hold
/// (the app's own fonts, which load by resource path) under their web
/// paths; the logical names added.
pub fn add_laid_out_fonts(bundle: &mut Bundle, analysis: &Analysis, source_root: &std::path::Path) -> Vec<String> {
    let have: BTreeSet<String> = bundle.entries.values().filter(|e| kind_of(&e.file) == "fonts").map(|e| content_hash_hex(&e.bytes)).collect();
    let mut added = Vec::new();
    for (hash, path) in &analysis.font_files {
        if have.contains(hash) {
            continue;
        }
        let (Some(web), Ok(bytes)) = (web_path(path), std::fs::read(path)) else { continue };
        bundle.set(&web, &path.to_string_lossy(), bytes);
        added.push(web);
    }
    // The UI font set the runtime loads at startup: a member the film never
    // lays out is still requested, so it ships too (as a one-glyph stub,
    // see `subset_bundle`) instead of a request that fails.
    let manifest = String::from_utf8_lossy(makepad_platform::LATIN_FONT_ASSET_PACKAGE_MANIFEST).to_string();
    for web in manifest.lines().filter_map(|l| l.strip_prefix("asset=")) {
        if bundle.entries.contains_key(web) {
            continue;
        }
        let Some(rel) = web.strip_prefix("makepad_widgets/") else { continue };
        let path = source_root.join("widgets").join(rel);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        if have.contains(&content_hash_hex(&bytes)) {
            continue;
        }
        bundle.set(web, &path.to_string_lossy(), bytes);
        added.push(web.to_string());
    }
    added
}

/// Cuts every font of the bundle in place; the fonts shipped (all of
/// them, with what happened to each).
pub fn subset_bundle(bundle: &mut Bundle, analysis: &Analysis, knob: FontsKnob, source_root: &std::path::Path, progress: &mut dyn Progress) -> Result<Vec<ShippedFont>, Diagnostic> {
    for web in add_laid_out_fonts(bundle, analysis, source_root) {
        progress.diagnostic(&Diagnostic::note(Phase::Fonts, format!("{web}: the app's own font, packed")));
    }
    let fonts: Vec<(String, String, Vec<u8>)> = bundle
        .entries
        .iter()
        .filter(|(_, e)| kind_of(&e.file) == "fonts")
        .map(|(logical, e)| (logical.clone(), e.file.clone(), e.bytes.clone()))
        .collect();
    let lyrics = lyric_text(bundle);
    let mut shipped = Vec::new();
    for (i, (logical, file, bytes)) in fonts.iter().enumerate() {
        progress.phase(Phase::Fonts, i as f64 / fonts.len().max(1) as f64, logical);
        let hash = content_hash_hex(bytes);
        let seen = analysis.glyphs.get(&hash).cloned().unwrap_or_default();
        let texts = analysis.texts.get(&hash).cloned().unwrap_or_default();
        let families: Vec<&String> = bundle.fonts.iter().filter(|(_, faces)| faces.contains(logical)).map(|(f, _)| f).collect();
        let declared: Vec<&str> = analysis.coverage.iter().filter(|(f, _)| families.iter().any(|fam| same_family(fam, f))).map(|(_, s)| s.as_str()).collect();
        let face = ttf_parser::Face::parse(bytes, 0).map_err(|_| Diagnostic::error(Phase::Fonts, format!("{logical}: not a font the text stack reads")))?;
        let names = FontNames::read(&face);
        let total = face.number_of_glyphs() as usize;
        let coverage = match knob {
            FontsKnob::Full => Coverage::Full("fonts: full (export option)".into()),
            FontsKnob::Analysed => classify(&seen, &texts, &declared, &lyrics),
        };
        let mut entry = ShippedFont {
            logical: logical.clone(),
            family: names.family.clone(),
            renamed_from: None,
            copyright: names.copyright.clone(),
            license: names.license.clone(),
            subset: false,
            glyphs: total,
            glyphs_total: total,
            bytes_before: bytes.len(),
            bytes_after: bytes.len(),
            why: String::new(),
        };
        match coverage {
            Coverage::Full(why) => entry.why = why,
            Coverage::Subset { glyphs, chars, why } if glyphs.is_empty() && chars.is_empty() => {
                // Nothing draws from it: a one-glyph stub keeps it loadable.
                let cut = makepad_font_subset::stub(bytes, &SubsetOptions::default()).map_err(|e| Diagnostic::error(Phase::Fonts, format!("{logical}: {e}")))?;
                entry.subset = true;
                entry.glyphs = 1;
                entry.bytes_after = cut.len();
                entry.why = if why.is_empty() { "never drawn: a one-glyph stub".into() } else { why.join("; ") };
                bundle.set(logical, file, cut);
            }
            Coverage::Subset { mut glyphs, chars, why } => {
                // Characters added beyond what was drawn bring their
                // substitutions (tabular figures, ligatures) along.
                let mut extra: BTreeSet<u16> = chars.iter().filter_map(|&c| face.glyph_index(c).map(|g| g.0)).collect();
                makepad_font_subset::gsub_closure(&face, &mut extra);
                glyphs.extend(extra);
                let reserved = reserved_font_names(&names.copyright);
                let rename = (!reserved.is_empty()).then(|| Rename { reserved: reserved.clone(), family: format!("{} Film", names.family.replace(reserved[0].as_str(), "Makepad")) });
                // Axes no layout moved off their default lose their
                // variation data.
                let instances = analysis.instances.get(&hash).cloned().unwrap_or_default();
                let pin_axes = if instances.is_empty() { Vec::new() } else { makepad_font_subset::unvaried_axes(bytes, &instances).unwrap_or_default() };
                // What the cut font must shape exactly as the original:
                // every text seen, at every instance.
                let runs: Vec<ShapeRun> = texts
                    .iter()
                    .flat_map(|(t, _)| {
                        let variations: Vec<Vec<(u32, f32)>> = if instances.is_empty() { vec![Vec::new()] } else { instances.clone() };
                        variations.into_iter().map(move |v| ShapeRun { text: t.clone(), variations: v, features: Vec::new(), rtl: false })
                    })
                    .collect();
                let inputs = ClosureInputs { runs, gsub_chars: chars.clone() };
                let cut_with = |opts: &SubsetOptions| makepad_font_subset::subset_with_report(bytes, &glyphs, &chars, opts);
                let tight = SubsetOptions { rename: rename.clone(), drop_aat: true, pin_axes: pin_axes.clone(), ..SubsetOptions::default() };
                let (cut, report, note) = match cut_with(&tight) {
                    Ok((cut, report)) if makepad_font_subset::verify(bytes, &cut, &inputs).is_ok() => (cut, report, None),
                    tight_result => {
                        // The tight cut changed how a text shapes (or failed):
                        // keep the Apple layout and every axis.
                        let why = match tight_result {
                            Ok((cut, _)) => makepad_font_subset::verify(bytes, &cut, &inputs).err().unwrap_or_default(),
                            Err(e) => e.to_string(),
                        };
                        let safe = SubsetOptions { rename: rename.clone(), ..SubsetOptions::default() };
                        let (cut, report) = cut_with(&safe).map_err(|e| Diagnostic::error(Phase::Fonts, format!("{logical}: {e}")).help("the web export subsets TrueType (glyf) fonts; convert this font or use one that is"))?;
                        (cut, report, Some(format!("kept every axis and the Apple layout ({why})")))
                    }
                };
                entry.subset = true;
                entry.glyphs = report.kept.len();
                entry.bytes_after = cut.len();
                let mut why = why;
                if note.is_none() && !pin_axes.is_empty() {
                    why.push(format!("{} unvaried axes pinned", pin_axes.len()));
                }
                why.extend(note);
                entry.why = if why.is_empty() { "never drawn: .notdef only".into() } else { why.join("; ") };
                if let Some(r) = rename {
                    entry.family = r.family;
                    entry.renamed_from = Some(names.family.clone());
                }
                bundle.set(logical, file, cut);
            }
        }
        progress.diagnostic(&Diagnostic::note(
            Phase::Fonts,
            format!(
                "{} ({logical}): {}, {} of {} glyphs, {} -> {} bytes: {}",
                entry.family,
                if entry.subset { "subset" } else { "full" },
                entry.glyphs,
                entry.glyphs_total,
                entry.bytes_before,
                entry.bytes_after,
                entry.why
            ),
        ));
        shipped.push(entry);
    }
    progress.phase(Phase::Fonts, 1.0, &format!("{} fonts", shipped.len()));
    Ok(shipped)
}

/// The shipped fonts the credits name: those that draw anything, one per
/// family and notice.
pub fn credited(fonts: &[ShippedFont]) -> Vec<ShippedFont> {
    let mut out: Vec<ShippedFont> = fonts.iter().filter(|f| !f.subset || f.glyphs > 1).cloned().collect();
    out.sort_by(|a, b| a.family.cmp(&b.family));
    out.dedup_by(|a, b| a.family == b.family && a.copyright == b.copyright);
    out
}

/// A font's naming records the credits quote.
struct FontNames {
    family: String,
    copyright: String,
    license: String,
}

impl FontNames {
    fn read(face: &ttf_parser::Face) -> Self {
        let get = |id: u16| face.names().into_iter().filter(|n| n.name_id == id && n.is_unicode()).find_map(|n| n.to_string()).unwrap_or_default();
        let typographic = get(16);
        FontNames { family: if typographic.is_empty() { get(1) } else { typographic }, copyright: get(0), license: get(13) }
    }
}


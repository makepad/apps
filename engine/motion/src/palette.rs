//! A source file's palette, read from its colour literals: every distinct
//! colour written in it (`#ff5a1f`, `color(4.0, 1.2, 0.3)` for light over
//! 1), each with the places it is written, its name when it has one (a
//! `/**name*/` before it, or the `let` it is the whole value of), and which
//! colours are near enough to be one (an alpha of another, or a hair apart)
//! to suggest merging. And the same colours written anew: each place keeps
//! its form (a hex stays hex, a `color(…)` stays a call).

use makepad_script::tokenizer::{lex, ScriptTokenTag};
use std::ops::Range;

/// One colour of a file, and where it is written.
#[derive(Clone, Debug, PartialEq)]
pub struct Swatch {
    /// Its channels (over 1 for light: `color(4.0, …)`) and alpha.
    pub rgba: [f64; 4],
    /// Written as light over 1 (`color(…)` with a channel above 1).
    pub hdr: bool,
    /// Its name: a `/**name*/` before one of its places, or the `let`
    /// one of its places is the whole value of.
    pub name: Option<String>,
    /// The bytes of each literal that writes it.
    pub sites: Vec<Range<usize>>,
    /// The colours that are one with it (the same colour at another alpha,
    /// or near enough to merge) share this index.
    pub group: usize,
}

/// How close two colours may be to be one (per channel, 0..1).
const NEAR: f64 = 2.0 / 255.0;

/// The colour literals of `text`: bytes, value, whether light over 1.
fn literals(text: &str) -> Vec<(Range<usize>, [f64; 4], bool)> {
    let tokens = lex(text);
    let color_id = makepad_script::LiveId::from_str("color");
    let minus = makepad_script::LiveId::from_str("-");
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].token().kind() {
            ScriptTokenTag::Color => {
                let c = tokens[i].token().as_color().unwrap();
                let ch = |s: u32| ((c >> s) & 0xff) as f64 / 255.0;
                out.push((tokens[i].span().clone(), [ch(24), ch(16), ch(8), ch(0)], false));
            }
            ScriptTokenTag::Identifier if tokens[i].token().identifier() == color_id && tokens.get(i + 1).is_some_and(|t| t.token().is_open_round()) => {
                // `color(r, g, b[, a])` of plain numbers (a computed colour
                // is code, not a palette entry).
                let mut values = Vec::new();
                let mut k = i + 2;
                let mut sign = 1.0;
                let mut plain = true;
                while let Some(t) = tokens.get(k) {
                    match t.token().kind() {
                        ScriptTokenTag::CloseRound => break,
                        ScriptTokenTag::Separator => {}
                        ScriptTokenTag::Operator if t.token().operator() == minus => sign = -sign,
                        ScriptTokenTag::F64 => {
                            let v = t.token().as_f64().unwrap();
                            values.push(sign * v);
                            sign = 1.0;
                        }
                        ScriptTokenTag::F32 | ScriptTokenTag::F16 => {
                            let v = t.token().as_f32().or_else(|| t.token().as_f16()).unwrap();
                            values.push(sign * v as f64);
                            sign = 1.0;
                        }
                        ScriptTokenTag::I32 => {
                            let v = t.token().as_i32().unwrap();
                            values.push(sign * v as f64);
                            sign = 1.0;
                        }
                        ScriptTokenTag::U32 => {
                            let v = t.token().as_u32().unwrap();
                            values.push(sign * v as f64);
                            sign = 1.0;
                        }
                        _ => plain = false,
                    }
                    k += 1;
                }
                if plain && (values.len() == 3 || values.len() == 4) {
                    if let Some(close) = tokens.get(k) {
                        let rgba = [values[0], values[1], values[2], values.get(3).copied().unwrap_or(1.0)];
                        let hdr = rgba[..3].iter().any(|v| *v > 1.0);
                        out.push((tokens[i].span().start..close.span().end, rgba, hdr));
                        i = k;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The name written for the literal at `site`: a `/**name*/` just before
/// it, or the `let` it is the whole value of.
fn name_before(text: &str, site: &Range<usize>) -> Option<String> {
    let before = text[..site.start].trim_end();
    if let Some(body) = before.strip_suffix("*/") {
        if let Some(open) = body.rfind("/**") {
            let name = body[open + 3..].trim();
            if !name.is_empty() && !name.contains('\n') {
                return Some(name.to_string());
            }
        }
    }
    let line_start = before.rfind('\n').map_or(0, |n| n + 1);
    let head = text[line_start..site.start].trim();
    let rest = text[site.end..].lines().next().unwrap_or("").trim();
    let head = head.strip_suffix('=')?.trim();
    let name = head.strip_prefix("let ")?.trim();
    (rest.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then(|| name.to_string())
}

/// The palette of a source file's text, most used first.
pub fn of(text: &str) -> Vec<Swatch> {
    let mut swatches: Vec<Swatch> = Vec::new();
    for (site, rgba, hdr) in literals(text) {
        let name = name_before(text, &site);
        match swatches.iter_mut().find(|s| s.rgba.iter().zip(&rgba).all(|(a, b)| (a - b).abs() < 1e-9)) {
            Some(s) => {
                s.sites.push(site);
                if s.name.is_none() {
                    s.name = name;
                }
            }
            None => {
                let group = swatches.len();
                swatches.push(Swatch { rgba, hdr, name, sites: vec![site], group });
            }
        }
    }
    // Groups: the same colour at another alpha, or a hair apart.
    for i in 0..swatches.len() {
        for j in 0..i {
            let (a, b) = (swatches[i].rgba, swatches[j].rgba);
            if (0..3).all(|c| (a[c] - b[c]).abs() <= NEAR) {
                swatches[i].group = swatches[j].group;
                break;
            }
        }
    }
    swatches.sort_by(|a, b| b.sites.len().cmp(&a.sites.len()).then(a.sites[0].start.cmp(&b.sites[0].start)));
    swatches
}

/// A colour in the form a literal had: `#rrggbb` (with alpha when it has
/// one, or the old literal had it), or `color(r, g, b[, a])`.
fn write(old: &str, rgba: [f64; 4]) -> String {
    if old.trim_start().starts_with('#') {
        let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        let digits = old.trim().len() - 1;
        if rgba[3] < 1.0 - 1e-9 || digits == 8 || digits == 4 {
            format!("#{:02x}{:02x}{:02x}{:02x}", b(rgba[0]), b(rgba[1]), b(rgba[2]), b(rgba[3]))
        } else {
            format!("#{:02x}{:02x}{:02x}", b(rgba[0]), b(rgba[1]), b(rgba[2]))
        }
    } else {
        let n = |v: f64| crate::schema::show_number(v);
        if old.matches(',').count() >= 3 || rgba[3] < 1.0 - 1e-9 {
            format!("color({}, {}, {}, {})", n(rgba[0]), n(rgba[1]), n(rgba[2]), n(rgba[3]))
        } else {
            format!("color({}, {}, {})", n(rgba[0]), n(rgba[1]), n(rgba[2]))
        }
    }
}

/// Every colour literal of `text`: its bytes and colour (what [`set_at`]
/// rewrites, read once for a gesture's many steps).
pub fn sites(text: &str) -> Vec<(Range<usize>, [f64; 4])> {
    literals(text).into_iter().map(|(site, rgba, _)| (site, rgba)).collect()
}

/// [`set`] over the colour literals [`sites`] read from `text`.
pub fn set_at(text: &str, sites: &[(Range<usize>, [f64; 4])], map: &[([f64; 4], [f64; 4])]) -> String {
    let mut hits: Vec<(Range<usize>, [f64; 4])> = sites.iter().filter_map(|(site, rgba)| {
        let new = map.iter().find(|(old, _)| old.iter().zip(rgba).all(|(a, b)| (a - b).abs() < 1e-6))?.1;
        Some((site.clone(), new))
    }).collect();
    hits.sort_by_key(|(s, _)| std::cmp::Reverse(s.start));
    let mut out = text.to_string();
    for (site, rgba) in hits {
        let written = write(&text[site.clone()], rgba);
        out.replace_range(site, &written);
    }
    out
}

/// `text` with every place a colour of `map` is written given its new
/// colour, each in its own form.
pub fn set(text: &str, map: &[([f64; 4], [f64; 4])]) -> String {
    let mut sites: Vec<(Range<usize>, [f64; 4])> = literals(text).into_iter().filter_map(|(site, rgba, _)| {
        let new = map.iter().find(|(old, _)| old.iter().zip(&rgba).all(|(a, b)| (a - b).abs() < 1e-6))?.1;
        Some((site, new))
    }).collect();
    sites.sort_by_key(|(s, _)| std::cmp::Reverse(s.start));
    let mut out = text.to_string();
    for (site, rgba) in sites {
        let written = write(&text[site.clone()], rgba);
        out.replace_range(site, &written);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_colours_with_their_places_names_and_groups() {
        let src = "let SIGNAL = #ff4d12\nlet c = /**ember*/ color(4.0, 1.2, 0.3)\nlet draw = fn(c) {\n    c.rect(0, 0, 1, 1, #ff4d12)\n    c.rect(0, 0, 1, 1, #ff4d1280)\n    c.rect(0, 0, 1, 1, #00ff00)\n}\n";
        let p = of(src);
        let signal = p.iter().find(|s| s.name.as_deref() == Some("SIGNAL")).expect("named by its let");
        assert_eq!(signal.sites.len(), 2);
        let ember = p.iter().find(|s| s.name.as_deref() == Some("ember")).expect("named by its doc");
        assert!(ember.hdr && (ember.rgba[0] - 4.0).abs() < 1e-9);
        let half = p.iter().find(|s| (s.rgba[3] - 128.0 / 255.0).abs() < 1e-9).expect("the alpha variant");
        assert_eq!(half.group, signal.group, "an alpha of SIGNAL is in its group");
        let green = p.iter().find(|s| s.rgba[1] == 1.0 && s.rgba[0] == 0.0).unwrap();
        assert_ne!(green.group, signal.group);
        let out = set(src, &[(signal.rgba, [0.0, 0.0, 1.0, 1.0]), (ember.rgba, [2.0, 2.0, 2.0, 1.0])]);
        assert!(out.contains("let SIGNAL = #0000ff") && out.contains("c.rect(0, 0, 1, 1, #0000ff)"), "{out}");
        assert!(out.contains("color(2, 2, 2)"), "{out}");
        assert!(out.contains("#ff4d1280"), "the alpha variant is its own colour: {out}");
    }
}

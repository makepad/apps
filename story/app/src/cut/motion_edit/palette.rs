//! A scene's palette, read by `makepad_motion::palette`: every colour its
//! code writes (`#rrggbb`, HDR `color(r, g, b)`), one swatch per colour
//! with each place that writes it, its name (a `/**name*/` before it, or
//! the `let` it is) and what the places mean (the `///` intent lines of
//! the `let`s they are in).
//!
//! A palette change is a map from old colours to new ones, written at
//! every place in one undo step. The map comes from a perceptual move
//! (warmer, colder, more contrast, a hue turn, muted but the accent,
//! brighter HDR, computed in OKLab so the colours keep their relations),
//! from the AI ("5 palettes for this scene"), or from a saved palette;
//! each is a variant in the strip, drawn and previewed like any other.
//!
//! Main-loop cost: reading the palette reads each file's colours (linear in
//! the code), when a palette is asked for; nothing runs per frame.

use super::session::MotionSession;
use makepad_motion::patch;

/// One colour of the scene.
#[derive(Clone, Debug)]
pub struct Swatch {
    /// sRGB (HDR: linear and may pass 1), alpha.
    pub rgba: [f64; 4],
    pub hdr: bool,
    pub name: Option<String>,
    /// Each place: the `let` (and field) it is in, and that `let`'s
    /// intent line.
    pub uses: Vec<Use>,
}

#[derive(Clone, Debug)]
pub struct Use {
    pub what: String,
    pub intent: Option<String>,
}

impl Swatch {
    /// How the colour is written (the key of a map).
    pub fn key(&self) -> String {
        if self.hdr {
            format!("color({}, {}, {})", patch::fmt_num(self.rgba[0]), patch::fmt_num(self.rgba[1]), patch::fmt_num(self.rgba[2]))
        } else {
            patch::fmt_color(self.rgba)
        }
    }
}

/// A colour written in code: its value, and whether it is HDR.
fn colour_at(text: &str) -> Option<([f64; 4], bool)> {
    let t = text.trim();
    if t.starts_with('#') {
        return patch::literal_color(t).map(|c| (c, false));
    }
    let args = t.strip_prefix("color(")?.strip_suffix(')')?;
    let v: Vec<f64> = args.split(',').map(|a| a.trim().parse::<f64>()).collect::<Result<_, _>>().ok()?;
    (v.len() >= 3).then(|| ([v[0], v[1], v[2], v.get(3).copied().unwrap_or(1.0)], true))
}

/// The files the palette is read from (built-in modules are not the scene's).
fn files(session: &MotionSession) -> Vec<(usize, &makepad_motion::schema::code::CodeOutline, &str)> {
    std::iter::once((0, &session.code, session.source.as_str()))
        .chain(session.modules.iter().enumerate().filter(|(_, m)| m.id != "builtin").map(|(k, m)| (k + 1, &m.code, m.source.as_str())))
        .collect()
}

/// The `let` a byte is in (top level), its name and first intent line.
fn owner(code: &makepad_motion::schema::code::CodeOutline, text: &str, at: usize) -> (Option<String>, Option<String>) {
    use makepad_motion::schema::code::RegionKind;
    let Some(region) = code.regions.iter().find(|r| r.kind == RegionKind::Let && r.depth == 0 && r.range.start <= at && at < r.range.end) else { return (None, None) };
    let line_start = text[..region.range.start].rfind('\n').map_or(0, |n| n + 1);
    let mut intent = None;
    let mut end = line_start;
    while end > 0 {
        let prev_start = text[..end - 1].rfind('\n').map_or(0, |n| n + 1);
        let line = text[prev_start..end - 1].trim();
        match line.strip_prefix("///") {
            Some(doc) => intent = Some(doc.trim().to_string()),
            None => break,
        }
        end = prev_start;
    }
    (Some(region.name.clone()), intent)
}

/// The scene's palette, the most used colours first: each file's colours
/// (`makepad_motion::palette::of`) joined by value, each place with the
/// `let` it is in and that `let`'s intent.
pub fn palette(session: &MotionSession) -> Vec<Swatch> {
    let mut out: Vec<Swatch> = Vec::new();
    for (_, code, text) in files(session) {
        for found in makepad_motion::palette::of(text) {
            let uses = found.sites.iter().map(|site| {
                let (let_name, intent) = owner(code, text, site.start);
                let field = code.literals.iter().find(|l| l.range.start <= site.start && site.start < l.range.end).map(|l| l.label.clone()).unwrap_or_default();
                let what = match (&let_name, field.is_empty()) {
                    (Some(l), false) if *l != field => format!("{l}.{field}"),
                    (Some(l), _) => l.clone(),
                    (None, _) => field,
                };
                Use { what, intent }
            });
            match out.iter_mut().find(|s| s.hdr == found.hdr && s.rgba.iter().zip(&found.rgba).all(|(a, b)| (a - b).abs() < 1e-9)) {
                Some(swatch) => {
                    swatch.uses.extend(uses);
                    if swatch.name.is_none() {
                        swatch.name = found.name;
                    }
                }
                None => out.push(Swatch { rgba: found.rgba, hdr: found.hdr, name: found.name, uses: uses.collect() }),
            }
        }
    }
    out.sort_by(|a, b| b.uses.len().cmp(&a.uses.len()));
    out
}

/// The palette as the model reads it: a line per colour, its name and
/// where it is used, with what those places are for.
pub fn listing(palette: &[Swatch]) -> String {
    let mut out = String::new();
    for swatch in palette {
        let mut places: Vec<String> = Vec::new();
        for u in &swatch.uses {
            let place = match &u.intent {
                Some(intent) => format!("{} ({intent})", u.what),
                None => u.what.clone(),
            };
            if !places.contains(&place) {
                places.push(place);
            }
        }
        let shown: Vec<String> = places.iter().take(8).cloned().collect();
        let more = if places.len() > 8 { format!(" and {} more", places.len() - 8) } else { String::new() };
        out.push_str(&format!(
            "{}{}{} — {} use{}: {}{more}\n",
            swatch.key(),
            swatch.name.as_ref().map(|n| format!(" `{n}`")).unwrap_or_default(),
            if swatch.hdr { " (HDR)" } else { "" },
            swatch.uses.len(),
            if swatch.uses.len() == 1 { "" } else { "s" },
            shown.join("; "),
        ));
    }
    out
}

/// The files' texts with every place of each mapped colour rewritten,
/// each in its form (`makepad_motion::palette::set`): (file index, text)
/// of each file that changes.
pub fn apply_map(session: &MotionSession, map: &[(String, [f64; 4])]) -> Result<Vec<(usize, String)>, String> {
    let palette = palette(session);
    let mut pairs: Vec<([f64; 4], [f64; 4])> = Vec::new();
    for (old, new) in map {
        let swatch = palette.iter().find(|s| s.key().eq_ignore_ascii_case(old) || s.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(old.trim_matches('`')))).ok_or_else(|| format!("{old} is not a colour of this scene"))?;
        pairs.push((swatch.rgba, *new));
    }
    let mut out = Vec::new();
    for (doc, _, text) in files(session) {
        let next = makepad_motion::palette::set(text, &pairs);
        if next != text {
            out.push((doc, next));
        }
    }
    Ok(out)
}

/// Parse a map block's lines `old -> new` (a colour as listed, or its
/// `name`, to `#rrggbb` or `color(r, g, b)`).
pub fn parse_map(block: &str) -> Vec<(String, [f64; 4])> {
    block.lines().filter_map(|line| {
        let (old, new) = line.trim().trim_start_matches(['-', '*', ' ']).split_once("->")?;
        let new = new.trim().split_whitespace().next().map(|first| if first.starts_with("color(") { new.trim().split(')').next().map(|c| format!("{c})")).unwrap_or_default() } else { first.to_string() })?;
        let (rgba, _) = colour_at(&new)?;
        Some((old.trim().to_string(), rgba))
    }).collect()
}

// ------------------------------------------------------------------ moves

/// sRGB (0..1) to OKLab.
fn oklab(c: [f64; 4]) -> [f64; 3] {
    let lin = |v: f64| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    let (r, g, b) = (lin(c[0]), lin(c[1]), lin(c[2]));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s, 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s, 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s]
}

/// OKLab to sRGB (0..1), clipped to the gamut.
fn srgb(lab: [f64; 3], alpha: f64) -> [f64; 4] {
    let l = lab[0] + 0.3963377774 * lab[1] + 0.2158037573 * lab[2];
    let m = lab[0] - 0.1055613458 * lab[1] - 0.0638541728 * lab[2];
    let s = lab[0] - 0.0894841775 * lab[1] - 1.2914855480 * lab[2];
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    let b = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;
    let gamma = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    };
    [gamma(r), gamma(g), gamma(b), alpha]
}

/// The perceptual moves a palette offers, by name: label and note.
pub const MOVES: &[(&str, &str, &str)] = &[
    ("warmer", "Warmer", "every colour toward amber, relations kept"),
    ("colder", "Colder", "every colour toward blue, relations kept"),
    ("contrast", "More contrast", "lights lighter, darks darker"),
    ("hue", "Hue +40°", "every hue turned together"),
    ("mute", "Muted", "all but the accent drained of colour"),
    ("hdr", "Brighter glow", "HDR colours at 1.6× intensity"),
];

/// A move's map over the palette (`amount` scales it; 1 is the default).
pub fn move_map(palette: &[Swatch], name: &str, amount: f64) -> Result<Vec<(String, [f64; 4])>, String> {
    let labs: Vec<[f64; 3]> = palette.iter().map(|s| oklab(s.rgba)).collect();
    let mean_l = if labs.is_empty() { 0.5 } else { labs.iter().map(|l| l[0]).sum::<f64>() / labs.len() as f64 };
    // The accent: the most colourful colour, weighted by how much it is used.
    let accent = (0..palette.len()).max_by(|&a, &b| {
        let score = |i: usize| (labs[i][1].hypot(labs[i][2])) * (palette[i].uses.len() as f64).sqrt();
        score(a).partial_cmp(&score(b)).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out = Vec::new();
    for (i, swatch) in palette.iter().enumerate() {
        let [l, a, b] = labs[i];
        let new = if swatch.hdr {
            match name {
                "hdr" => {
                    let k = 1.0 + 0.6 * amount;
                    [swatch.rgba[0] * k, swatch.rgba[1] * k, swatch.rgba[2] * k, swatch.rgba[3]]
                }
                _ => continue,
            }
        } else {
            let lab = match name {
                "warmer" => [l, a + 0.012 * amount, b + 0.035 * amount],
                "colder" => [l, a - 0.008 * amount, b - 0.04 * amount],
                "contrast" => [(mean_l + (l - mean_l) * (1.0 + 0.45 * amount)).clamp(0.0, 1.0), a, b],
                "hue" => {
                    let (c, h) = (a.hypot(b), b.atan2(a) + (40.0 * amount).to_radians());
                    [l, c * h.cos(), c * h.sin()]
                }
                "mute" if Some(i) == accent => [l, a, b],
                "mute" => [l, a * 0.15, b * 0.15],
                "hdr" => continue,
                _ => return Err(format!("no move `{name}`: {}", MOVES.iter().map(|m| m.0).collect::<Vec<_>>().join(", "))),
            };
            srgb(lab, swatch.rgba[3])
        };
        out.push((swatch.key(), new));
    }
    Ok(out)
}

// ------------------------------------------------------------------ saved palettes

/// A palette as a library module: its colours with their names and what
/// they were for, as Splash (the module's export is the record).
pub fn module_source(palette: &[Swatch], title: &str, from: &str) -> String {
    let mut out = format!("// The palette \"{title}\", saved from {from}: each colour, its name, and what it was used for.\n{{name: {} colours: [\n", patch::fmt_text(title));
    for swatch in palette {
        let uses: Vec<String> = swatch.uses.iter().map(|u| u.what.clone()).collect();
        out.push_str(&format!(
            "    {{name: {} color: {} uses: {}}}\n",
            patch::fmt_text(swatch.name.as_deref().unwrap_or("")),
            swatch.key(),
            patch::fmt_text(&uses.join(", ")),
        ));
    }
    out.push_str("]}\n");
    out
}

/// A saved palette's colours mapped onto this scene's: by name where the
/// names agree, else by role (the scene's colours and the saved ones each
/// ordered by lightness, matched by rank). HDR colours keep theirs.
pub fn map_saved(palette: &[Swatch], saved: &[(String, [f64; 4])]) -> Vec<(String, [f64; 4])> {
    let mut out = Vec::new();
    let mut free: Vec<usize> = (0..saved.len()).collect();
    let mut rest: Vec<&Swatch> = Vec::new();
    for swatch in palette.iter().filter(|s| !s.hdr) {
        match swatch.name.as_ref().and_then(|n| free.iter().position(|&i| saved[i].0.eq_ignore_ascii_case(n))) {
            Some(at) => {
                out.push((swatch.key(), saved[free[at]].1));
                free.remove(at);
            }
            None => rest.push(swatch),
        }
    }
    let lightness = |c: [f64; 4]| oklab(c)[0];
    rest.sort_by(|a, b| lightness(a.rgba).partial_cmp(&lightness(b.rgba)).unwrap_or(std::cmp::Ordering::Equal));
    free.sort_by(|&a, &b| lightness(saved[a].1).partial_cmp(&lightness(saved[b].1)).unwrap_or(std::cmp::Ordering::Equal));
    if !free.is_empty() {
        for (k, swatch) in rest.iter().enumerate() {
            let pick = if rest.len() <= 1 { free.len() / 2 } else { k * (free.len() - 1) / (rest.len() - 1) };
            out.push((swatch.key(), saved[free[pick]].1));
        }
    }
    out
}

/// A saved palette's colours (its module's `{name: "…" color: #…}`
/// entries).
pub fn parse_saved(source: &str) -> Vec<(String, [f64; 4])> {
    let mut out = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find("{name: ") {
        rest = &rest[at + 7..];
        let Some(name) = rest.strip_prefix('"').and_then(|r| r.split('"').next()) else { continue };
        let Some(color_at) = rest.find("color: ") else { break };
        let value: String = rest[color_at + 7..].chars().take_while(|c| *c == '#' || c.is_ascii_hexdigit()).collect();
        if let Some(rgba) = patch::literal_color(&value) {
            out.push((name.to_string(), rgba));
        }
    }
    out
}

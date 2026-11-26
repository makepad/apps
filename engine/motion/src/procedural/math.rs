//! `Math{tex: "\frac{a}{b}" size: 48}`: a typeset formula as a Group of Paths,
//! one per glyph or rule (`crate::math`), each named by its piece key and
//! filled with the layer's `color`. The pieces animate as any layer does
//! (trim, keys, morph); `parts` reaches whole parts of the formula:
//!
//! - `parts: {top: {color: #f60 opacity: tween(…)}}` the pieces inside
//!   `\id{top}{…}` in the formula source (names never move anything);
//! - `paths: {"1/num": {…}}` the pieces under a path through the formula
//!   tree (`1/num` is the numerator of the second item; whole segments);
//! - `chars: {x: {…}}` the pieces showing that character.
//!
//! A piece takes the first selection that matches it, in that order (ids in
//! the order written, then paths, longest first, then characters). A part
//! takes `color stroke width trim_start trim_end opacity x y scale rotate`.

use super::*;
use crate::math;
use crate::pieces::{Piece, Select};

const PART_FIELDS: &[&str] = &["color", "stroke", "width", "trim_start", "trim_end", "opacity", "x", "y", "scale", "rotate"];

/// A selection: how it picks pieces and the fields it sets on them.
pub(super) struct Part {
    pub(super) select: String,
    pub(super) fields: Vec<(String, ScriptValue)>,
}

pub(super) fn read_parts(cx: &Cx, o: ScriptObject, field: &str) -> Result<Vec<Part>, (String, String)> {
    let Some(v) = obj::opt(cx.vm, o, field).filter(|v| !v.is_nil()) else { return Ok(Vec::new()) };
    let Some(map) = obj::record_of(cx.vm, v) else {
        return Err((field.into(), format!("`{field}` maps names to settings (`{{top: {{color: #ff6a00}}}}`), not {}", kind(cx, v))));
    };
    let mut out = Vec::new();
    for (name, value, _) in obj::fields(cx.vm, map) {
        let Some(part) = obj::record_of(cx.vm, value) else {
            return Err((field.into(), format!("`{field}.{name}` is `{{color: … opacity: …}}`, not {}", kind(cx, value))));
        };
        let mut fields = Vec::new();
        for (k, v, _) in obj::fields(cx.vm, part) {
            if !PART_FIELDS.contains(&k.as_str()) {
                return Err((field.into(), format!("`{field}.{name}` takes {}, not `{k}`", PART_FIELDS.join(" "))));
            }
            fields.push((if k == "color" { "fill".to_string() } else { k }, v));
        }
        out.push(Part { select: name, fields });
    }
    Ok(out)
}

/// The part that styles `piece`: ids, then paths (the longest prefix), then characters.
pub(super) fn pick<'a>(piece: &Piece, ids: &'a [Part], paths: &'a [Part], chars: &'a [Part]) -> Option<&'a Part> {
    ids.iter()
        .find(|p| piece.matches(Select::Id(&p.select)))
        .or_else(|| paths.iter().filter(|p| piece.matches(Select::Path(&p.select))).max_by_key(|p| p.select.len()))
        .or_else(|| chars.iter().find(|p| p.select.chars().count() == 1 && p.select.chars().next().is_some_and(|c| piece.matches(Select::Char(c)))))
}

pub(super) fn lower(cx: &mut Cx, o: ScriptObject) -> Made {
    let tex = get_text(cx, o, "tex").ok_or_else(|| ("tex".to_string(), "a Math needs its formula: `tex: \"\\frac{a}{b}\"`".to_string()))?;
    let size = get_num(cx, o, "size", 48.0)?.max(0.1);
    let color = obj::opt(cx.vm, o, "color").filter(|v| !v.is_nil());
    let display = get_bool(cx, o, "display", true);
    let align = match obj::opt(cx.vm, o, "align").and_then(|v| obj::name(cx.vm, v)).as_deref() {
        None | Some("left") => 0.0,
        Some("center") => 0.5,
        Some("right") => 1.0,
        Some(other) => return Err(("align".into(), format!("`align` is @left, @center or @right, not `{other}`"))),
    };
    let face = get_face(cx, o, "font", "STIX Two Math")?;
    let ids = read_parts(cx, o, "parts")?;
    let paths = read_parts(cx, o, "paths")?;
    let chars = read_parts(cx, o, "chars")?;
    // A `Text`'s size is in points; the em is `size` points of document units.
    let set = math::typeset(&tex, face.family, size * 4.0 / 3.0, display).map_err(|m| ("tex".to_string(), m))?;

    let mut layers = Vec::with_capacity(set.pieces.len());
    for piece in &set.pieces {
        let x = piece.origin[0] - set.width * align;
        let mut fields: Vec<(&str, Lit)> = vec![
            ("svg", crate::icons::path_data(&piece.outline).into()),
            ("name", piece.key.clone().into()),
            ("at", v2(x, piece.origin[1])),
        ];
        let part = pick(piece, &ids, &paths, &chars);
        let styled = |key: &str| part.is_some_and(|p| p.fields.iter().any(|(k, _)| k == key));
        if !styled("fill") {
            fields.push(("fill", color.map_or(Lit::Color(0xffffffff), Lit::Value)));
        }
        if let Some(part) = part {
            for (k, v) in &part.fields {
                fields.push((k.as_str(), Lit::Value(*v)));
            }
        }
        layers.push(layer(cx, "Path", fields));
    }
    Ok(group(cx, o, layers, vec![("size", v2(set.width, set.height)), ("glyphs", Lit::Bool(true))]))
}

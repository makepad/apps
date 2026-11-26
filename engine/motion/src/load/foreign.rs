//! Words other tools use for a Motion field, for error messages only.
//!
//! An author (often a model) brings its vocabulary from style sheets, page
//! layout, video editors and 3D packages: `position`, `rotation`,
//! `font_size`, `background_color`. Edit distance cannot connect those to
//! `at`, `rotate`, `size` or `fill`, so the plain "did you mean" finds
//! nothing useful. This table turns that error into the fix: it names our
//! field and how its value is written.
//!
//! These words are never accepted: a document says each thing one way,
//! and the error stays an error. A test checks that every target is a real
//! field of the types it is given for, and that no word is one.

/// One entry: the words, the types they are hinted on (empty: every
/// layer, through the fields all layers share), our field and its value.
pub(crate) struct Foreign {
    pub words: &'static [&'static str],
    pub types: &'static [&'static str],
    pub field: &'static str,
    pub how: &'static str,
}

const SHAPES: &[&str] = &["Rect", "Ellipse", "Path"];
const MEDIA: &[&str] = &["Image", "Video", "Svg", "Vector"];

pub(crate) const FOREIGN: &[Foreign] = &[
    // Every layer (the common fields).
    Foreign { words: &["position", "pos", "location", "translate", "translation", "offset", "center", "centre"], types: &[], field: "at", how: "vec2(x, y) in px, the layer's origin (a box's top-left); or `x:` and `y:` alone" },
    Foreign { words: &["rotation", "angle", "rotation_z", "rotate_z"], types: &[], field: "rotate", how: "degrees, clockwise" },
    Foreign { words: &["alpha", "transparency", "fade"], types: &[], field: "opacity", how: "0..1, a number or an animation of one" },
    Foreign { words: &["scale_x", "scale_y", "scale_xy"], types: &[], field: "scale", how: "vec2(sx, sy), or one number for both" },
    Foreign { words: &["anchor_point", "pivot", "origin", "transform_origin", "registration"], types: &[], field: "anchor", how: "vec2(0..1, 0..1) of the layer's box, the point it scales and rotates about; its centre, vec2(0.5, 0.5), by default" },
    Foreign { words: &["blend_mode", "mix_blend_mode", "composite", "composite_mode", "blending"], types: &[], field: "blend", how: "@normal, @add, @screen or @multiply" },
    Foreign { words: &["start_time", "in_point", "begin", "delay", "end", "end_time", "out_point", "lifetime"], types: &[], field: "span", how: "[start, length] in seconds, when the layer shows" },
    Foreign { words: &["hidden", "hide", "visibility", "display", "enabled"], types: &[], field: "visible", how: "true or false" },
    Foreign { words: &["filters", "filter", "fx"], types: &[], field: "effects", how: "a list of effects on this layer" },
    // Shapes.
    Foreign { words: &["color", "colour", "fill_color", "fill_colour", "background", "background_color", "bg_color"], types: SHAPES, field: "fill", how: "a colour (#rrggbb, #rrggbbaa) or an animation of one" },
    Foreign { words: &["border", "outline", "stroke_color", "border_color", "line_color"], types: SHAPES, field: "stroke", how: "a colour; its thickness is `width`" },
    Foreign { words: &["stroke_width", "border_width", "line_width", "thickness", "stroke_weight"], types: SHAPES, field: "width", how: "the stroke's thickness in px" },
    Foreign { words: &["border_radius", "corner_radius", "corners", "rounding", "round"], types: &["Rect"], field: "radius", how: "the corner radius in px" },
    Foreign { words: &["height", "dimensions", "extent", "bounds", "w", "h"], types: &["Rect", "Ellipse"], field: "size", how: "vec2(width, height) in px; `width` is the stroke's thickness" },
    Foreign { words: &["radius", "radius_x", "radius_y", "rx", "ry", "diameter"], types: &["Ellipse"], field: "size", how: "vec2(width, height) of the ellipse's box in px" },
    Foreign { words: &["d", "path", "data", "path_data"], types: &["Path"], field: "svg", how: "SVG path data as a string, \"M0 0 L100 0\"" },
    Foreign { words: &["start_trim", "trim_in", "draw_start"], types: SHAPES, field: "trim_start", how: "0..1 along the outline, where the stroke starts" },
    Foreign { words: &["end_trim", "trim_out", "draw_end", "progress"], types: SHAPES, field: "trim_end", how: "0..1 along the outline, where the stroke ends" },
    // Text.
    Foreign { words: &["font_size", "text_size", "point_size", "fontsize", "em"], types: &["Text"], field: "size", how: "the font size in points" },
    Foreign { words: &["fill", "text_color", "font_color", "colour", "fill_color"], types: &["Text"], field: "color", how: "a colour (#rrggbb, #rrggbbaa) or an animation of one" },
    Foreign { words: &["content", "string", "value", "label", "caption", "body"], types: &["Text"], field: "text", how: "the words, as a string" },
    Foreign { words: &["font_family", "family", "typeface", "font_weight", "weight", "font_style", "italic"], types: &["Text"], field: "font", how: "Font{family: \"Inter\" weight: 700 italic: false}" },
    Foreign { words: &["text_align", "justify", "alignment", "horizontal_align", "h_align", "halign"], types: &["Text"], field: "align", how: "@left, @center or @right" },
    Foreign { words: &["vertical_align", "v_align", "baseline"], types: &["Text"], field: "valign", how: "@top, @center or @bottom" },
    Foreign { words: &["letter_spacing", "kerning", "character_spacing", "spacing"], types: &["Text"], field: "tracking", how: "extra space between letters, in px" },
    Foreign { words: &["line_spacing", "leading"], types: &["Text"], field: "line_height", how: "a factor of the font's line, 1.0 by default" },
    Foreign { words: &["max_width", "wrap_width", "text_width", "box_width", "width_limit"], types: &["Text"], field: "wrap", how: "the width in px to break lines at, or @safe" },
    // Pictures and video.
    Foreign { words: &["source", "url", "file", "path", "image", "texture", "href"], types: MEDIA, field: "src", how: "asset(\"id\", \"rev\") or in(@name)" },
    Foreign { words: &["object_fit", "scale_mode", "fit_mode", "resize", "content_mode"], types: &["Image", "Video"], field: "fit", how: "@fill, @fit, @stretch or @none" },
    Foreign { words: &["height", "width", "dimensions"], types: MEDIA, field: "size", how: "vec2(width, height) in px" },
    // Groups.
    Foreign { words: &["children", "items", "elements", "nodes", "objects", "content"], types: &["Group"], field: "layers", how: "a list of layers, the first drawn first (at the back)" },
    // The document.
    Foreign { words: &["width", "height", "resolution", "dimensions", "frame_size", "canvas"], types: &["Motion"], field: "size", how: "vec2(width, height) in px" },
    Foreign { words: &["frame_rate", "framerate", "frames_per_second", "rate"], types: &["Motion"], field: "fps", how: "frames per second" },
    Foreign { words: &["length", "total_time", "time", "seconds", "end"], types: &["Motion"], field: "duration", how: "the document's length in seconds" },
    Foreign { words: &["background_color", "bg", "bg_color", "clear_color", "backdrop", "fill"], types: &["Motion"], field: "background", how: "a colour (#rrggbb)" },
    Foreign { words: &["tempo"], types: &["Motion"], field: "bpm", how: "the music's beats per minute" },
    Foreign { words: &["children", "items", "elements", "nodes", "objects", "scene", "content"], types: &["Motion"], field: "layers", how: "a list of layers, the first drawn first (at the back)" },
    Foreign { words: &["title", "name"], types: &["Motion"], field: "label", how: "the document's name, as a string" },
    Foreign { words: &["effects", "filters", "post_processing", "postprocessing", "passes"], types: &["Motion"], field: "post", how: "the post effects over the whole frame" },
];

/// `fontSize`, `font-size` and `Font_Size` all as `font_size`.
pub(crate) fn normal(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 4);
    for (i, c) in word.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else if c == '-' {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    out
}

/// The entry for `word` written on type `ty` (`Motion`, a layer type),
/// when it is another tool's word for one of its fields.
pub(crate) fn lookup(word: &str, ty: &str) -> Option<&'static Foreign> {
    let word = normal(word);
    // The 3D types' scenes have their own words (`position` is a node's
    // field there): only 2D layers get the shared hints.
    let layer = crate::layers::LAYERS.iter().any(|l| l.name == ty) && !crate::layers::OPEN.contains(&ty);
    // The type's own entries win over the ones every layer shares
    // (`fill` on a Text is its `color`).
    FOREIGN.iter().find(|f| f.types.contains(&ty) && f.words.contains(&word.as_str()))
        .or_else(|| FOREIGN.iter().find(|f| layer && f.types.is_empty() && f.words.contains(&word.as_str())))
}

/// How field `field` of `ty` is written, where the table says it (the
/// type's own entry first, then the one every 2D layer shares).
pub(crate) fn how(ty: &str, field: &str) -> Option<&'static str> {
    let layer = crate::layers::LAYERS.iter().any(|l| l.name == ty) && !crate::layers::OPEN.contains(&ty);
    FOREIGN.iter().find(|f| f.field == field && f.types.contains(&ty))
        .or_else(|| FOREIGN.iter().find(|f| layer && f.field == field && f.types.is_empty()))
        .map(|f| f.how)
}

/// The error for `word` on `ty`, naming our field: ``Rect has no
/// `position`; write `at` here: vec2(x, y), …``.
pub(crate) fn message(word: &str, ty: &str) -> Option<String> {
    let f = lookup(word, ty)?;
    Some(format!("{ty} has no `{word}`; write `{}` here: {}", f.field, f.how))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every hint names a field its types really have, and no word is
    /// itself a field there (a hint is never an alias, and a word that is
    /// a field would never be hinted).
    #[test]
    fn every_hint_names_a_real_field_and_no_word_is_one() {
        for f in FOREIGN {
            for word in f.words {
                assert_eq!(normal(word), *word, "`{word}` is written in its normal form");
            }
            if f.types.is_empty() {
                // Hinted on every layer: a field every layer has.
                assert!(crate::layers::is_common(f.field), "`{}` is not a field every layer has (hinted for {:?})", f.field, f.words);
                for word in f.words {
                    assert!(!crate::layers::is_common(word), "`{word}` is a field every layer has: it cannot be hinted as `{}`", f.field);
                }
                continue;
            }
            for ty in f.types {
                let fields = if *ty == "Motion" { crate::layers::document_fields() } else { crate::layers::fields(ty) };
                assert!(!fields.is_empty(), "`{ty}` is no Motion type");
                assert!(fields.iter().any(|field| field == f.field), "`{}` is not a field of {ty} (hinted for {:?})", f.field, f.words);
                for word in f.words {
                    assert!(!fields.iter().any(|field| field == word), "`{word}` is a field of {ty}: it cannot be hinted as `{}`", f.field);
                }
            }
        }
    }

    #[test]
    fn words_are_matched_in_any_spelling_and_the_type_wins() {
        assert_eq!(normal("fontSize"), "font_size");
        assert_eq!(normal("font-size"), "font_size");
        assert_eq!(normal("Font_Size"), "font_size");
        assert_eq!(lookup("fill", "Text").map(|f| f.field), Some("color"));
        assert_eq!(lookup("position", "Text").map(|f| f.field), Some("at"));
        assert_eq!(lookup("position", "Motion").map(|f| f.field), None, "a layer word is not hinted on the document");
        assert_eq!(lookup("zorder", "Rect").map(|f| f.field), None);
        assert_eq!(lookup("rotation", "Scene3D").map(|f| f.field), None, "a 3D type keeps its own words");
    }
}

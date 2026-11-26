//! Typeset formulas as addressable pieces. The TeX-subset layout
//! (`makepad-latex-math`, the font's OpenType MATH table) places glyphs and
//! rules; here each becomes one piece: an outline in the document's units
//! with the part of the formula it belongs to (its path through the formula
//! tree, the `\id{name}{…}` names around it, its character) as a
//! [`Piece`], the same thing a morph matches. Pieces are pure data: a layer
//! draws them, a morph matches them by key.

use crate::geometry;
use crate::pieces::Piece;
use makepad_latex_math::{LayoutItem, MathStyle, OutlineSeg};
use makepad_svg::path::{FillRule, PathCmd, VectorPath};

/// A typeset formula: its box and its pieces.
#[derive(Clone, Debug)]
pub struct MathSet {
    pub width: f64,
    pub height: f64,
    /// The baseline, from the box's top.
    pub baseline: f64,
    pub pieces: Vec<Piece>,
}

/// `tex` set in `family`'s math face with the em `em` document units tall,
/// `display` for display style (large operators, limits above and below),
/// else inline style.
pub fn typeset(tex: &str, family: &str, em: f64, display: bool) -> Result<MathSet, String> {
    let path = makepad_doc_fonts::font_file(family, 400.0, false).ok_or_else(|| format!("`{family}` is not an installed document font"))?;
    let bytes = makepad_doc_fonts::read_font(&path)?;
    typeset_with(tex, &bytes, em, display)
}

/// [`typeset`] in font `bytes` (an OpenType file with a MATH table).
pub fn typeset_with(tex: &str, bytes: &[u8], em: f64, display: bool) -> Result<MathSet, String> {
    let (nodes, unknown) = makepad_latex_math::parse_checked(tex);
    if !unknown.is_empty() {
        let list = unknown.iter().map(|c| format!("\\{c}")).collect::<Vec<_>>().join(", ");
        return Err(format!("the formula uses {list}, which the math typesetter does not know (it would show as text); use the common TeX commands (\\frac \\sqrt \\sum \\int \\lim \\cap \\cup \\in \\to \\cdot \\alpha … \\text{{…}}), and in a Splash string write each backslash twice"));
    }
    let style = if display { MathStyle::Display } else { MathStyle::Text };
    let laid = makepad_latex_math::layout(&nodes, bytes, em as f32, style).ok_or("the font does not parse")?;
    let mut counts: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    let mut pieces = Vec::new();
    let ascent = laid.ascent as f64;
    for item in &laid.items {
        let part = &laid.parts[item.part() as usize];
        let n = counts.entry(item.part()).or_insert(0);
        let key = format!("{}#{}", part.path_string(), *n);
        *n += 1;
        let (mut outline, ch, at) = match item {
            LayoutItem::Glyph(g) => {
                let Some(segs) = makepad_latex_math::glyph_outline(bytes, g.glyph_id, g.size) else { continue };
                (path_of(&segs), Some(g.ch), [g.x as f64, ascent + g.y as f64])
            }
            LayoutItem::Rule(r) => (rect(r.width as f64, r.height as f64), None, [r.x as f64, ascent + r.y as f64]),
            LayoutItem::Rect(r) => (rect(r.width as f64, r.height as f64), None, [r.x as f64, ascent + r.y as f64]),
        };
        let Some([x0, y0, x1, y1]) = geometry::bounds(&outline) else { continue };
        shift(&mut outline, -x0, -y0);
        let content = ch.map_or_else(|| "\u{2501}".to_string(), |c| c.to_string());
        pieces.push(Piece { content, key, path: part.path_string(), ids: part.ids.clone(), ch, outline, origin: [at[0] + x0, at[1] + y0], size: [x1 - x0, y1 - y0] });
    }
    Ok(MathSet { width: laid.width as f64, height: (laid.ascent + laid.descent) as f64, baseline: ascent, pieces })
}

fn rect(w: f64, h: f64) -> VectorPath {
    let (w, h) = (w as f32, h as f32);
    VectorPath { cmds: vec![PathCmd::MoveTo(0.0, 0.0), PathCmd::LineTo(w, 0.0), PathCmd::LineTo(w, h), PathCmd::LineTo(0.0, h), PathCmd::Close], fill_rule: None }
}

/// A glyph outline as a path (quadratic segments raised to cubic).
fn path_of(segs: &[OutlineSeg]) -> VectorPath {
    let mut cmds = Vec::with_capacity(segs.len());
    let mut at = (0.0f32, 0.0f32);
    for seg in segs {
        match *seg {
            OutlineSeg::MoveTo(x, y) => {
                cmds.push(PathCmd::MoveTo(x, y));
                at = (x, y);
            }
            OutlineSeg::LineTo(x, y) => {
                cmds.push(PathCmd::LineTo(x, y));
                at = (x, y);
            }
            OutlineSeg::QuadTo(cx, cy, x, y) => {
                cmds.push(PathCmd::BezierTo(at.0 + 2.0 / 3.0 * (cx - at.0), at.1 + 2.0 / 3.0 * (cy - at.1), x + 2.0 / 3.0 * (cx - x), y + 2.0 / 3.0 * (cy - y), x, y));
                at = (x, y);
            }
            OutlineSeg::CurveTo(a, b, c, d, x, y) => {
                cmds.push(PathCmd::BezierTo(a, b, c, d, x, y));
                at = (x, y);
            }
            OutlineSeg::Close => cmds.push(PathCmd::Close),
        }
    }
    // A glyph's contours overlap in variable fonts: non-zero.
    VectorPath { cmds, fill_rule: Some(FillRule::NonZero) }
}

pub(crate) fn shift(path: &mut VectorPath, dx: f64, dy: f64) {
    let (dx, dy) = (dx as f32, dy as f32);
    for cmd in &mut path.cmds {
        match cmd {
            PathCmd::MoveTo(x, y) | PathCmd::LineTo(x, y) => {
                *x += dx;
                *y += dy;
            }
            PathCmd::BezierTo(a, b, c, d, x, y) => {
                *a += dx;
                *b += dy;
                *c += dx;
                *d += dy;
                *x += dx;
                *y += dy;
            }
            _ => {}
        }
    }
}

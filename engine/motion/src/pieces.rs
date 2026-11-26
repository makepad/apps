//! Pieces: the things a transition matches. A piece is a keyed outline with
//! a place: a glyph, a word or a line of a text, a part of a typeset
//! formula, or a shape the author named. [`pair_pieces`] decides which piece
//! of one layout becomes which of another (same key, the n-th occurrence
//! with the n-th, left to right); `MatchMorph` ([`crate::procedural`])
//! flies the pairs, morphs their outlines ([`crate::align`]) and fades
//! the rest. A typeset formula ([`crate::math::MathSet`]) is made of the same
//! pieces: the Math layer draws them, [`math_pieces`] re-keys them for a match.

use crate::geometry::VectorPath;
use crate::math::MathSet;

/// One matchable piece.
#[derive(Clone, Debug)]
pub struct Piece {
    /// What it is matched by: pieces of the two layouts with the same key
    /// pair up (a character, a word's text, a line's text, an `\id` name, or
    /// a name the author gave a shape). In a [`MathSet`] it is instead unique
    /// and stable for the formula source: `<part path>#<n>`, the n-th piece of
    /// that part in drawing order; [`math_pieces`] re-keys.
    pub key: String,
    /// A formula piece's part: its path through the formula tree (`1/num/0`,
    /// `0/row:1/col:0/2`); empty for a piece of a text.
    pub path: String,
    /// The `\id` names around a formula piece, outermost first.
    pub ids: Vec<String>,
    /// The character a glyph shows; `None` for a rule or a word, line or group.
    pub ch: Option<char>,
    /// What the piece shows, as text: a glyph's character, a word's or line's
    /// text, the characters of an `\id` group in order (a rule is `\u{2501}`).
    /// A matched pair morphs its outlines only when both show the same
    /// content; otherwise the old fades out and the new fades in.
    pub content: String,
    /// The outline with its bounding box's top-left corner at (0, 0).
    pub outline: VectorPath,
    /// Where that corner sits in the layout (x right, y down).
    pub origin: [f64; 2],
    /// The bounding box's size.
    pub size: [f64; 2],
}

/// How a formula piece is selected: by an `\id` name, by a path through the
/// formula tree (a prefix, whole segments), or by its character.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Select<'a> {
    Id(&'a str),
    Path(&'a str),
    Char(char),
}

impl Piece {
    pub fn matches(&self, select: Select) -> bool {
        match select {
            Select::Id(name) => self.ids.iter().any(|i| i == name),
            Select::Path(prefix) => self.path == prefix || self.path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('/')),
            Select::Char(c) => self.ch == Some(c),
        }
    }
}

/// What a text is cut into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keying {
    /// One piece per glyph, keyed by its character.
    Char,
    /// One piece per word, keyed by the word's text.
    Word,
    /// One piece per line, keyed by the line's text.
    Line,
}

/// The outcome of pairing two layouts' pieces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pairing {
    /// `(index in from, index in to)`, in `to` order.
    pub matched: Vec<(usize, usize)>,
    /// Pieces of `from` with no partner: they fade out.
    pub from_only: Vec<usize>,
    /// Pieces of `to` with no partner: they fade in.
    pub to_only: Vec<usize>,
}

/// Pair the pieces of two layouts. Walking `to` in order, the n-th piece of
/// a key takes the n-th piece of that key in `from`; what is left over on
/// either side is unmatched. Deterministic: the same layouts, the same pairs.
pub fn pair_pieces(from: &[Piece], to: &[Piece]) -> Pairing {
    let mut out = Pairing::default();
    let mut used = vec![false; from.len()];
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (j, piece) in to.iter().enumerate() {
        let n = seen.entry(piece.key.as_str()).or_insert(0);
        let partner = from.iter().enumerate().filter(|(_, f)| f.key == piece.key).nth(*n).map(|(i, _)| i);
        *n += 1;
        match partner {
            Some(i) => {
                used[i] = true;
                out.matched.push((i, j));
            }
            None => out.to_only.push(j),
        }
    }
    out.from_only = (0..from.len()).filter(|i| !used[*i]).collect();
    out
}

/// A piece from polygons in layout coordinates (`None` when empty).
pub fn piece_of_polygons(key: String, contours: &[Vec<[f64; 2]>]) -> Option<Piece> {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in contours.iter().flatten() {
        b = [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])];
    }
    if b[0] > b[2] {
        return None;
    }
    let mut outline = VectorPath::new();
    for c in contours.iter().filter(|c| c.len() > 2) {
        for (i, p) in c.iter().enumerate() {
            let (x, y) = ((p[0] - b[0]) as f32, (p[1] - b[1]) as f32);
            if i == 0 {
                outline.move_to(x, y);
            } else {
                outline.line_to(x, y);
            }
        }
        outline.close();
    }
    Some(Piece { content: key.clone(), key, path: String::new(), ids: Vec::new(), ch: None, outline, origin: [b[0], b[1]], size: [b[2] - b[0], b[3] - b[1]] })
}

/// A text's pieces, in reading order: glyphs, words or lines with ink.
pub(crate) fn text_pieces(glyphs: &[crate::type_fns::GlyphInk], keying: Keying) -> Vec<Piece> {
    match keying {
        Keying::Char => glyphs
            .iter()
            .filter_map(|g| {
                let mut p = piece_of_polygons(g.ch.to_string(), &g.contours)?;
                p.ch = Some(g.ch);
                Some(p)
            })
            .collect(),
        Keying::Word | Keying::Line => {
            // Group in order of first appearance, key = the group's characters.
            let group_of = |g: &crate::type_fns::GlyphInk| if keying == Keying::Word { g.word } else { Some(g.line) };
            let mut order: Vec<usize> = Vec::new();
            for g in glyphs {
                if let Some(id) = group_of(g) {
                    if !order.contains(&id) {
                        order.push(id);
                    }
                }
            }
            order
                .into_iter()
                .filter_map(|id| {
                    let members: Vec<&crate::type_fns::GlyphInk> = glyphs.iter().filter(|g| group_of(g) == Some(id)).collect();
                    let key: String = members.iter().map(|g| g.ch).collect::<String>().trim().to_string();
                    let contours: Vec<Vec<[f64; 2]>> = members.iter().flat_map(|g| g.contours.iter().cloned()).collect();
                    piece_of_polygons(key, &contours)
                })
                .collect()
        }
    }
}

/// How a formula's pieces are keyed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathKeying {
    /// By character (rules are never matched).
    Char,
    /// By the outermost `\id{name}{…}` around a piece, the pieces of one
    /// name making one piece; pieces outside any name by character.
    Id,
}

/// One piece made of several: their outlines in one, at the union's corner.
fn merged(key: String, members: Vec<Piece>) -> Piece {
    let x0 = members.iter().map(|p| p.origin[0]).fold(f64::MAX, f64::min);
    let y0 = members.iter().map(|p| p.origin[1]).fold(f64::MAX, f64::min);
    let x1 = members.iter().map(|p| p.origin[0] + p.size[0]).fold(f64::MIN, f64::max);
    let y1 = members.iter().map(|p| p.origin[1] + p.size[1]).fold(f64::MIN, f64::max);
    let content: String = members.iter().map(|p| p.content.as_str()).collect();
    let mut outline = VectorPath::new();
    for mut p in members {
        crate::math::shift(&mut p.outline, p.origin[0] - x0, p.origin[1] - y0);
        outline.cmds.extend(p.outline.cmds);
    }
    Piece { content, key: key.clone(), path: String::new(), ids: vec![key], ch: None, outline, origin: [x0, y0], size: [x1 - x0, y1 - y0] }
}

/// A formula's pieces, in drawing order, keyed for a match.
pub fn math_pieces(set: MathSet, keying: MathKeying) -> Vec<Piece> {
    let mut entries: Vec<(usize, Piece)> = Vec::new();
    let mut groups: Vec<(String, usize, Vec<Piece>)> = Vec::new();
    for (n, mut piece) in set.pieces.into_iter().enumerate() {
        let id = (keying == MathKeying::Id).then(|| piece.ids.first().cloned()).flatten();
        match id {
            Some(name) => match groups.iter_mut().find(|(k, _, _)| *k == name) {
                Some((_, _, members)) => members.push(piece),
                None => groups.push((name, n, vec![piece])),
            },
            None => {
                piece.key = match piece.ch {
                    Some(c) => c.to_string(),
                    None => format!("rule:{n}"),
                };
                entries.push((n, piece));
            }
        }
    }
    // Drawing order: a group sits where its first member was.
    for (name, first, members) in groups {
        entries.push((first, merged(name, members)));
    }
    entries.sort_by_key(|(n, _)| *n);
    entries.into_iter().map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(key: &str, x: f64) -> Piece {
        let mut outline = VectorPath::new();
        outline.move_to(0.0, 0.0);
        outline.line_to(10.0, 0.0);
        outline.line_to(10.0, 10.0);
        outline.close();
        Piece { content: key.into(), key: key.into(), path: String::new(), ids: Vec::new(), ch: None, outline, origin: [x, 0.0], size: [10.0, 10.0] }
    }

    fn row(keys: &str) -> Vec<Piece> {
        keys.chars().enumerate().map(|(i, c)| piece(&c.to_string(), i as f64 * 12.0)).collect()
    }

    #[test]
    fn the_nth_occurrence_pairs_with_the_nth_left_to_right() {
        // "a b a" into "a a c": the first a with the first a, the second with the second.
        let p = pair_pieces(&row("aba"), &row("aac"));
        assert_eq!(p.matched, vec![(0, 0), (2, 1)]);
        assert_eq!(p.to_only, vec![2]);
        assert_eq!(p.from_only, vec![1]);
        // Extra occurrences on the target side are new; on the source side, gone.
        let p = pair_pieces(&row("a"), &row("aaa"));
        assert_eq!((p.matched.clone(), p.to_only.clone(), p.from_only.clone()), (vec![(0, 0)], vec![1, 2], vec![]));
        // Identical layouts pair everything in order; disjoint ones pair nothing.
        let p = pair_pieces(&row("hello"), &row("hello"));
        assert_eq!(p.matched, (0..5).map(|i| (i, i)).collect::<Vec<_>>());
        assert!(p.from_only.is_empty() && p.to_only.is_empty());
        let p = pair_pieces(&row("ab"), &row("cd"));
        assert!(p.matched.is_empty() && p.from_only == vec![0, 1] && p.to_only == vec![0, 1]);
        // Deterministic.
        assert_eq!(pair_pieces(&row("abcab"), &row("bacba")), pair_pieces(&row("abcab"), &row("bacba")));
    }

    #[test]
    fn polygons_become_pieces_at_their_corner() {
        let square = vec![vec![[100.0, 50.0], [120.0, 50.0], [120.0, 80.0], [100.0, 80.0]]];
        let p = piece_of_polygons("sq".into(), &square).unwrap();
        assert_eq!((p.origin, p.size), ([100.0, 50.0], [20.0, 30.0]));
        assert!(piece_of_polygons("x".into(), &[]).is_none());
    }
}

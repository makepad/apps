//! The calibration print: one patch of every ink of a set (each top-two stack of its filaments),
//! over the light backing the tiles use and over the darkest filament, to tell whether the third
//! layer shows. A frame of the lightest filament is the white reference and the photo's flat
//! field; a strip holds the darkest and lightest filament pure and three line ramps of one on
//! the other (a quarter, a half, three quarters), from which the camera's tone curve is read;
//! four corner marks of the darkest filament and a dot by the first find and turn the print in
//! a photo.
//!
//! The layout is as the face is seen, in mm from its top left (u right, v down); printed face
//! down it is mirrored like a tile.

use crate::palette::FilamentSet;
use crate::tile::{Face, Tile, TileSpec};

/// A patch's side, the gap between patches, the frame round the patches (mm).
pub const PATCH: f64 = 6.0;
pub const GAP: f64 = 1.2;
pub const MARGIN: f64 = 8.0;
/// A corner mark's side (mm), and the dot's beside the first.
pub const MARK: f64 = 5.0;
pub const DOT: f64 = 2.0;

/// What a patch holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    /// One filament per layer, the face first.
    Stack(Vec<u8>),
    /// Lines of the darkest filament across the lightest, this share of them (every layer).
    Ramp(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Patch {
    pub content: Content,
    /// Its rectangle as the face is seen: [u, v] of its top left and its size (mm).
    pub at: [f64; 2],
    pub size: [f64; 2],
}

/// The swatch of `n` filaments for a tile `layers` deep: its size (mm), the patches and the
/// marks, as the face is seen.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub size: [f64; 2],
    pub patches: Vec<Patch>,
    pub marks: Vec<[f64; 2]>,
    pub dot: [f64; 2],
}

/// The swatch's size (mm) for `n` filaments: two blocks of n x n patches side by side and the
/// strip under them, in the margin.
pub fn size(n: usize) -> [f64; 2] {
    let block = n as f64 * (PATCH + GAP) - GAP;
    [2.0 * MARGIN + 2.0 * block + 3.0 * GAP, 2.0 * MARGIN + block + 2.0 * GAP + PATCH]
}

/// The layout for `set` in a tile like `spec` whose layers past the second are the backing
/// (`light`: the backing its tiles use, then the darkest filament).
pub fn layout(set: &FilamentSet, spec: &TileSpec) -> Layout {
    let layers = spec.layers;
    let n = set.filaments.len();
    let darkest = (0..n).min_by(|&a, &b| set.filaments[a].lightness().total_cmp(&set.filaments[b].lightness())).unwrap_or(0) as u8;
    let lightest = (0..n).max_by(|&a, &b| set.filaments[a].lightness().total_cmp(&set.filaments[b].lightness())).unwrap_or(0) as u8;
    let block = n as f64 * (PATCH + GAP) - GAP;
    let mut patches = Vec::new();
    for (bi, dark) in [false, true].into_iter().enumerate() {
        let left = MARGIN + bi as f64 * (block + 3.0 * GAP);
        for a in 0..n {
            for b in 0..n {
                let pair = [a as u8, b as u8];
                let stack: Vec<u8> = (0..layers)
                    .map(|l| match (l, dark, set.white()) {
                        (0 | 1, _, _) => pair[l],
                        (_, true, _) => darkest,
                        (_, false, Some(w)) => w,
                        (_, false, None) => pair[spec.repeats(l, 2)],
                    })
                    .collect();
                patches.push(Patch { content: Content::Stack(stack), at: [left + b as f64 * (PATCH + GAP), MARGIN + a as f64 * (PATCH + GAP)], size: [PATCH, PATCH] });
            }
        }
    }
    let total = size(n);
    let strip_v = MARGIN + block + 2.0 * GAP;
    let strip = [Content::Stack(vec![darkest; layers]), Content::Ramp(0.25), Content::Ramp(0.5), Content::Ramp(0.75), Content::Stack(vec![lightest; layers])];
    let w = (total[0] - 2.0 * MARGIN - 4.0 * GAP) / 5.0;
    for (i, c) in strip.into_iter().enumerate() {
        patches.push(Patch { content: c, at: [MARGIN + i as f64 * (w + GAP), strip_v], size: [w, PATCH] });
    }
    let e = 0.5 * (MARGIN - MARK);
    let marks = vec![[e, e], [total[0] - e - MARK, e], [e, total[1] - e - MARK], [total[0] - e - MARK, total[1] - e - MARK]];
    Layout { size: total, patches, marks, dot: [e + MARK + 1.0, e + 0.5 * (MARK - DOT)] }
}

/// The swatch as a tile: every cell's filament per layer, and the colour its face shows (the
/// set's measured stacks', else the first guess's).
pub fn tile(set: &FilamentSet, spec: &TileSpec) -> (Tile, Layout) {
    let lay = layout(set, spec);
    let [nx, ny] = spec.size;
    let n = set.filaments.len();
    let darkest = (0..n).min_by(|&a, &b| set.filaments[a].lightness().total_cmp(&set.filaments[b].lightness())).unwrap_or(0) as u8;
    let lightest = (0..n).max_by(|&a, &b| set.filaments[a].lightness().total_cmp(&set.filaments[b].lightness())).unwrap_or(0) as u8;
    let mut layers = vec![vec![lightest; nx * ny]; spec.layers];
    let inside = |at: [f64; 2], size: [f64; 2], u: f64, v: f64| u >= at[0] && u < at[0] + size[0] && v >= at[1] && v < at[1] + size[1];
    for y in 0..ny {
        for x in 0..nx {
            // the cell's middle as the face is seen
            let px = match spec.face {
                Face::Down => nx - 1 - x,
                Face::Up => x,
            };
            let (u, v) = ((px as f64 + 0.5) * spec.cell, (ny - 1 - y) as f64 * spec.cell + 0.5 * spec.cell);
            let i = y * nx + x;
            let mark = lay.marks.iter().any(|m| inside(*m, [MARK, MARK], u, v)) || inside(lay.dot, [DOT, DOT], u, v);
            if mark {
                for l in 0..spec.layers {
                    layers[l][i] = darkest;
                }
                continue;
            }
            if let Some(p) = lay.patches.iter().find(|p| inside(p.at, p.size, u, v)) {
                match &p.content {
                    Content::Stack(s) => {
                        for l in 0..spec.layers {
                            layers[spec.layer_of(l)][i] = s[l];
                        }
                    }
                    Content::Ramp(share) => {
                        // lines along the face's first layer's lines: every k-th row of cells dark
                        let row = ((v - p.at[1]) / spec.cell) as usize;
                        let dark = ((row as f64 + 0.5) * share).floor() != ((row as f64 - 0.5) * share).floor();
                        let f = if dark { darkest } else { lightest };
                        for l in 0..spec.layers {
                            layers[l][i] = f;
                        }
                    }
                }
            }
        }
    }
    // what each cell's face shows: its layers from the face, each one layer thick
    let mut known = std::collections::HashMap::new();
    let shown: Vec<[f32; 3]> = (0..nx * ny)
        .map(|i| {
            let stack: Vec<u8> = (0..spec.layers).map(|l| layers[spec.layer_of(l)][i]).collect();
            *known.entry(stack.clone()).or_insert_with(|| set.colour_of(&stack.iter().map(|f| (*f, spec.layer)).collect::<Vec<_>>(), spec.background).0)
        })
        .collect();
    (Tile { size: [nx, ny], cell: spec.cell, layers, ink: vec![0; nx * ny], wanted: shown.clone(), shown }, lay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dither::Rules;
    use crate::palette::Filament;
    use crate::tile::Axis;

    /// Four filaments with a white give 32 stack patches (16 over the white, 16 over black) and the strip of
    /// five; every patch lies in the swatch apart from the others; and printed face down, the
    /// first patch (cyan over cyan, top left as seen) sits at the top right in plan, its four
    /// layers cyan, cyan, white, white.
    #[test]
    fn the_swatch_holds_every_stack_once_and_mirrors_face_down() {
        let set = FilamentSet::new("cmyw", vec![Filament::guess("Cyan", [0, 160, 224]), Filament::guess("Magenta", [224, 0, 122]), Filament::guess("Yellow", [255, 224, 0]), Filament::guess("White", [242, 242, 242])]);
        let cell = 0.4;
        let size = size(4);
        let spec = TileSpec { cell, size: [(size[0] / cell) as usize, (size[1] / cell) as usize], layers: 4, layer: 0.16, face: Face::Down, depth: 2, rules: Rules { min_run: 1, switch: 0.0, lightness: 1.0 }, first_lines: Axis::X, background: [0.8; 3], viewing: 0.0 };
        let lay = layout(&set, &spec);
        assert_eq!(lay.patches.len(), 32 + 5);
        for (i, a) in lay.patches.iter().enumerate() {
            assert!(a.at[0] >= 0.0 && a.at[1] >= 0.0 && a.at[0] + a.size[0] <= lay.size[0] && a.at[1] + a.size[1] <= lay.size[1]);
            for b in &lay.patches[i + 1..] {
                let apart = a.at[0] + a.size[0] <= b.at[0] || b.at[0] + b.size[0] <= a.at[0] || a.at[1] + a.size[1] <= b.at[1] || b.at[1] + b.size[1] <= a.at[1];
                assert!(apart, "{a:?} {b:?}");
            }
        }
        let (t, _) = tile(&set, &spec);
        let [nx, ny] = t.size;
        // the first patch's middle as seen: (MARGIN + 3, MARGIN + 3) mm
        let (u, v) = (MARGIN + 3.0, MARGIN + 3.0);
        let (x, y) = (nx - 1 - (u / cell) as usize, ny - 1 - (v / cell) as usize);
        let at = y * nx + x;
        assert_eq!((0..4).map(|l| t.layers[l][at]).collect::<Vec<_>>(), vec![0, 0, 3, 3]);
    }

    /// Without a white the light backing repeats the pair in turn, each layer the one whose
    /// lines run its way: magenta over yellow is magenta, yellow, magenta, yellow.
    #[test]
    fn a_swatch_without_white_backs_each_pair_with_itself() {
        let set = FilamentSet::new("cmyk", vec![Filament::guess("Cyan", [0, 160, 224]), Filament::guess("Magenta", [224, 0, 122]), Filament::guess("Yellow", [255, 224, 0]), Filament::guess("Black", [26, 26, 26])]);
        let cell = 0.4;
        let size = size(4);
        let spec = TileSpec { cell, size: [(size[0] / cell) as usize, (size[1] / cell) as usize], layers: 4, layer: 0.16, face: Face::Down, depth: 2, rules: Rules { min_run: 1, switch: 0.0, lightness: 1.0 }, first_lines: Axis::X, background: [0.8; 3], viewing: 0.0 };
        let lay = layout(&set, &spec);
        let my = lay.patches.iter().find(|p| p.content == Content::Stack(vec![1, 2, 1, 2]));
        assert!(my.is_some());
        let black = lay.patches.iter().filter(|p| matches!(&p.content, Content::Stack(s) if s[2..] == [3, 3])).count();
        // the darkest block, black over black backed by itself and the strip's black
        assert_eq!(black, 16 + 1 + 1);
    }
}

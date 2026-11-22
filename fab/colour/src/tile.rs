//! A flat picture tile: the picture's cells on a grid one bead apart, each cell a stack of the
//! tile's layers. Printed face down, the first layer is the picture and the layers behind it
//! tint it through it; the picture is mirrored so it reads right turned over. The layers
//! below the dithered ones are a backing: the set's white, else the deepest dithered layer
//! again.
//!
//! The grid is in plan (x right, y to the back, as the bed is seen from above); layer 0 is the
//! one printed first.

use crate::dither::{dither, refine, Cells, Rules};
use crate::gamut::Gamut;
use crate::image::Image;
use crate::palette::{Backing, FilamentSet, Palette, StackModel};

/// Which face is the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// The first layer, on the plate: the picture is mirrored.
    Down,
    /// The last layer.
    Up,
}

/// The way a layer's lines run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TileSpec {
    /// The grid's pitch: one bead's spacing (mm).
    pub cell: f64,
    /// Cells along x and y.
    pub size: [usize; 2],
    pub layers: usize,
    /// Each layer's height (mm).
    pub layer: f64,
    pub face: Face,
    /// Dithered layers from the face (the rest are the backing).
    pub depth: usize,
    pub rules: Rules,
    /// How the first printed layer's lines run; each layer after turns a quarter.
    pub first_lines: Axis,
    /// What lies behind the tile when it is seen (linear).
    pub background: [f32; 3],
    /// The distance (mm) the dither is refined to look right from (0: error diffusion alone).
    pub viewing: f64,
}

impl TileSpec {
    /// The print layer stack level `level` is (0: the face).
    pub fn layer_of(&self, level: usize) -> usize {
        match self.face {
            Face::Down => level,
            Face::Up => self.layers - 1 - level,
        }
    }

    pub fn lines(&self, layer: usize) -> Axis {
        match (self.first_lines, layer % 2) {
            (a, 0) => a,
            (Axis::X, _) => Axis::Y,
            (Axis::Y, _) => Axis::X,
        }
    }

    /// How the tile stacks `set`: its dithered levels a layer each, then the backing: the set's
    /// white, else the dithered levels again (see [`TileSpec::repeats`]).
    pub fn model(&self, set: &FilamentSet) -> StackModel {
        let depth = self.depth.clamp(1, self.layers);
        let backing = match set.white() {
            Some(index) => Backing::Filament { index, mm: (self.layers - depth) as f64 * self.layer },
            None => Backing::Repeat { levels: (depth..self.layers).map(|level| self.repeats(level, depth)).collect(), layer: self.layer },
        };
        StackModel { levels: vec![self.layer; depth], backing, background: self.background }
    }

    /// The dithered level (of `depth`) a backing level repeats when the set has no white: the
    /// deepest whose layer runs its lines the same way, so the backing's runs are the dither's.
    pub fn repeats(&self, level: usize, depth: usize) -> usize {
        let lines = self.lines(self.layer_of(level));
        (0..depth).rev().find(|&l| self.lines(self.layer_of(l)) == lines).unwrap_or(depth - 1)
    }
}

/// A tile made: per print layer the filament of every cell (row by row from the front), the ink
/// of each cell, the colour it aimed at (the picture's, in the gamut) and the colour its face
/// shows (linear).
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub size: [usize; 2],
    pub cell: f64,
    pub layers: Vec<Vec<u8>>,
    pub ink: Vec<u16>,
    pub wanted: Vec<[f32; 3]>,
    pub shown: Vec<[f32; 3]>,
}

impl Tile {
    /// The colours its face shows as the eye sees them: a face-down tile turned over (its
    /// mirror undone), the picture's top row at the top.
    pub fn face(&self, face: Face) -> Image {
        self.as_seen(face, &self.shown)
    }

    /// A colour per cell (in plan, as [`Tile::shown`]) as the face is seen.
    pub fn as_seen(&self, face: Face, cells: &[[f32; 3]]) -> Image {
        let [nx, ny] = self.size;
        let pixels = (0..nx * ny)
            .map(|i| {
                let (u, v) = (i % nx, i / nx);
                let x = match face {
                    Face::Down => nx - 1 - u,
                    Face::Up => u,
                };
                cells[(ny - 1 - v) * nx + x]
            })
            .collect();
        Image { width: nx, height: ny, pixels }
    }

    /// Each filament's runs along a layer's lines (the separate pieces it prints there).
    pub fn runs(&self, layer: usize, lines: Axis, filaments: usize) -> Vec<usize> {
        let [nx, ny] = self.size;
        let mut out = vec![0; filaments];
        let fil = &self.layers[layer];
        let (outer, inner) = match lines {
            Axis::X => (ny, nx),
            Axis::Y => (nx, ny),
        };
        for a in 0..outer {
            let mut last = None;
            for b in 0..inner {
                let i = match lines {
                    Axis::X => a * nx + b,
                    Axis::Y => b * nx + a,
                };
                if last != Some(fil[i]) {
                    out[fil[i] as usize] += 1;
                    last = Some(fil[i]);
                }
            }
        }
        out
    }
}

/// The grid in the order it is chosen: rows from the front, each row the other way from the
/// one before (serpentine), the error spread as Floyd and Steinberg's.
struct Grid<'a> {
    nx: usize,
    ny: usize,
    target: Vec<[f32; 3]>,
    spec: &'a TileSpec,
}

impl Grid<'_> {
    fn at(&self, i: usize) -> (usize, usize) {
        let (y, k) = (i / self.nx, i % self.nx);
        (if y % 2 == 0 { k } else { self.nx - 1 - k }, y)
    }
    fn order(&self, x: usize, y: usize) -> usize {
        y * self.nx + if y % 2 == 0 { x } else { self.nx - 1 - x }
    }
    /// The step along a row in its order.
    fn ahead(&self, y: usize) -> isize {
        if y % 2 == 0 { 1 } else { -1 }
    }
    fn cell(&self, x: isize, y: usize) -> Option<usize> {
        (x >= 0 && (x as usize) < self.nx && y < self.ny).then(|| self.order(x as usize, y))
    }
}

impl Cells for Grid<'_> {
    fn len(&self) -> usize {
        self.nx * self.ny
    }
    fn target(&self, i: usize) -> [f32; 3] {
        let (x, y) = self.at(i);
        self.target[y * self.nx + x]
    }
    fn spread(&self, i: usize, out: &mut Vec<(usize, f32)>) {
        let (x, y) = self.at(i);
        let (x, d) = (x as isize, self.ahead(y));
        for (dx, dy, w) in [(d, 0, 7.0), (-d, 1, 3.0), (0, 1, 5.0), (d, 1, 1.0)] {
            if let Some(j) = self.cell(x + dx, y + dy) {
                out.push((j, w / 16.0));
            }
        }
    }
    fn before(&self, i: usize, level: usize) -> Option<usize> {
        let (x, y) = self.at(i);
        match self.spec.lines(self.spec.layer_of(level)) {
            Axis::X => self.cell(x as isize - self.ahead(y), y),
            Axis::Y => y.checked_sub(1).map(|y| self.order(x, y)),
        }
    }
    fn along(&self, i: usize, level: usize) -> [Option<usize>; 2] {
        let (x, y) = self.at(i);
        match self.spec.lines(self.spec.layer_of(level)) {
            Axis::X => [self.cell(x as isize - 1, y), self.cell(x as isize + 1, y)],
            Axis::Y => [y.checked_sub(1).map(|y| self.order(x, y)), self.cell(x as isize, y + 1)],
        }
    }
    fn near(&self, i: usize, reach: usize, out: &mut Vec<(usize, [i32; 2])>) {
        out.clear();
        let (x, y) = self.at(i);
        let r = reach as isize;
        for dy in -r..=r {
            let yy = y as isize + dy;
            if yy < 0 || yy >= self.ny as isize {
                continue;
            }
            for dx in -r..=r {
                if let Some(j) = self.cell(x as isize + dx, yy as usize) {
                    out.push((j, [dx as i32, dy as i32]));
                }
            }
        }
    }
    fn place(&self, i: usize) -> [i32; 2] {
        let (x, y) = self.at(i);
        [x as i32, y as i32]
    }
    fn pitch(&self) -> f64 {
        self.spec.cell
    }
}

/// The tile of `image` in `set`'s filaments: the picture fitted to the grid (mirrored for a
/// face-down tile), brought into the palette's gamut as a whole (its lightness by its own
/// range), dithered and refined to look right from the spec's viewing distance.
pub fn make(image: &Image, set: &FilamentSet, spec: &TileSpec) -> Tile {
    let [nx, ny] = spec.size;
    let model = spec.model(set);
    let palette = Palette::new(set, &model);
    let gamut = Gamut::of(&palette);
    let pic = image.cells(nx, ny);
    let tone = gamut.tone(&pic);
    // cell (x, y) in plan; the picture's top row at the back, its left at the left as the face is seen
    let target = (0..nx * ny)
        .map(|i| {
            let (x, y) = (i % nx, i / nx);
            let u = match spec.face {
                Face::Down => nx - 1 - x,
                Face::Up => x,
            };
            gamut.map(pic[(ny - 1 - y) * nx + u], &tone)
        })
        .collect();
    let grid = Grid { nx, ny, target, spec };
    let mut chosen = dither(&palette, &grid, &spec.rules);
    if spec.viewing > 0.0 {
        refine(&palette, &grid, &spec.rules, spec.viewing, &mut chosen);
    }
    let mut ink = vec![0u16; nx * ny];
    for (i, k) in chosen.into_iter().enumerate() {
        let (x, y) = grid.at(i);
        ink[y * nx + x] = k;
    }
    let depth = palette.depth;
    let layers = (0..spec.layers)
        .map(|layer| {
            // the level from the face this layer is
            let level = (0..spec.layers).find(|&l| spec.layer_of(l) == layer).unwrap_or(layer);
            ink.iter()
                .map(|&k| {
                    let stack = &palette.inks[k as usize].stack;
                    match &model.backing {
                        _ if level < depth => stack[level],
                        Backing::Filament { index, .. } => *index,
                        Backing::Repeat { levels, .. } => stack[levels[level - depth]],
                    }
                })
                .collect()
        })
        .collect();
    let shown = ink.iter().map(|&k| palette.inks[k as usize].colour).collect();
    Tile { size: [nx, ny], cell: spec.cell, layers, ink, wanted: grid.target, shown }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oklab;
    use crate::palette::Filament;

    fn cmyw() -> FilamentSet {
        FilamentSet::new("cmyw", vec![Filament::guess("Cyan", [0, 160, 224]), Filament::guess("Magenta", [224, 0, 122]), Filament::guess("Yellow", [255, 224, 0]), Filament::guess("White", [242, 242, 242])])
    }

    fn spec(size: [usize; 2], face: Face) -> TileSpec {
        TileSpec { cell: 0.4, size, layers: 4, layer: 0.16, face, depth: 2, rules: Rules { min_run: 2, switch: 0.0, lightness: 1.0 }, first_lines: Axis::X, background: [0.8; 3], viewing: crate::view::VIEWING }
    }

    /// A red at the edge of what the set prints, refined to look right from the viewing distance,
    /// still averages to the red the dither aimed at, with and without a white: flatter inks show
    /// less texture but a paler red, and the refine's guard on patches' colours keeps it.
    #[test]
    fn a_saturated_picture_keeps_its_colour() {
        let (w, h) = (60, 60);
        let img = Image { width: w, height: h, pixels: vec![oklab::linear([0.62, 0.18, 0.02]); w * h] };
        let black = FilamentSet::new("cmyk", vec![Filament::guess("Cyan", [0, 163, 224]), Filament::guess("Magenta", [214, 0, 126]), Filament::guess("Yellow", [255, 212, 0]), Filament::guess("Black", [26, 26, 26])]);
        for set in [cmyw(), black] {
            let s = TileSpec { rules: Rules { min_run: 2, switch: 0.03, lightness: 2.0 }, ..spec([w, h], Face::Down) };
            let t = make(&img, &set, &s);
            let mean = |cells: &[[f32; 3]]| oklab::oklab(std::array::from_fn(|k| cells.iter().map(|c| c[k]).sum::<f32>() / cells.len() as f32));
            let (aim, got) = (mean(&t.wanted), mean(&t.shown));
            assert!(aim[1] > 0.1, "{}: the red aimed at is {aim:?}", set.name);
            // within one and a half ΔE: side by side, barely told apart (unguarded, over six)
            assert!(oklab::delta_e(aim, got) < 0.015, "{}: the red {aim:?} averages to {got:?}", set.name);
        }
    }

    /// A face-down tile of a picture with a yellow left half and a cyan right half: seen from the
    /// face (mirrored back) its left is yellow and its right cyan; the face shows the
    /// average colour of each half within a small error; layers 3 and 4 are all the white
    /// backing; every run along a layer's lines is at least two cells but at a row's end.
    #[test]
    fn a_face_down_tile_shows_its_picture_mirrored_with_a_white_back() {
        let (w, h) = (40, 20);
        let pixels = (0..w * h).map(|i| if i % w < w / 2 { oklab::linear_of_srgb8([250, 220, 30]) } else { oklab::linear_of_srgb8([20, 150, 220]) }).collect();
        let img = Image { width: w, height: h, pixels };
        let s = spec([40, 20], Face::Down);
        let set = cmyw();
        let t = make(&img, &set, &s);
        let half = |left_as_seen: bool| -> [f32; 3] {
            let mut sum = [0.0f32; 3];
            let mut n = 0.0;
            for y in 2..18 {
                for x in 0..40 {
                    // seen from the face, plan x is mirrored
                    let seen_left = 39 - x < 20;
                    if seen_left == left_as_seen && (39 - x) % 20 > 2 && (39 - x) % 20 < 17 {
                        for k in 0..3 {
                            sum[k] += t.shown[y * 40 + x][k];
                        }
                        n += 1.0;
                    }
                }
            }
            sum.map(|v| v / n)
        };
        let (l, r) = (oklab::oklab(half(true)), oklab::oklab(half(false)));
        assert!(l[2] > 0.05 && r[2] < -0.02 && l[2] - r[2] > 0.08, "left as seen is yellow {l:?}, right is cyan {r:?}");
        for layer in [2, 3] {
            assert!(t.layers[layer].iter().all(|&f| f == 3), "layer {layer} is the white backing");
        }
        for layer in [0, 1] {
            let lines = s.lines(layer);
            let [nx, ny] = t.size;
            let (outer, inner) = if lines == Axis::X { (ny, nx) } else { (nx, ny) };
            for a in 0..outer {
                let row: Vec<u8> = (0..inner).map(|b| t.layers[layer][if lines == Axis::X { a * nx + b } else { b * nx + a }]).collect();
                let mut runs = vec![1usize];
                for p in row.windows(2) {
                    if p[0] == p[1] { *runs.last_mut().unwrap() += 1 } else { runs.push(1) }
                }
                // a run may be short only at a line's two ends (the serpentine order runs it from either)
                assert!(runs.len() < 3 || runs[1..runs.len() - 1].iter().all(|&r| r >= 2), "layer {layer} line {a}: {runs:?}");
            }
            let pieces: usize = t.runs(layer, lines, 4).iter().sum();
            assert!(pieces >= outer, "{pieces}");
        }
    }
}

//! Pixel paint layers (plan 9c, pixel lane): a picture over the whole sky that a brush paints into.
//!
//! A pixel layer is an equirect picture `PAINT_WIDTH` x `PAINT_HEIGHT` in the map's own frame and its mapping
//! (`dir_to_equirect_uv`), cut into `PAINT_TILE` square tiles. A texel holds premultiplied radiance and its cover:
//! `[r a, g a, b a, a]`, the brush's colour already x 2^its EV. A tile nothing was painted on is not stored, and
//! tiles are shared (`Arc`): a stroke copies only the tiles it touches, so the versions the undo history keeps
//! (one a stroke) cost the tiles each stroke changed, not a whole picture each.
//!
//! A stroke (`BrushStroke`) paints over the layer as it stood when the stroke began: each texel keeps the most of
//! the brush that reached it (its cover), and the stroke's result is the layer then with the brush at that cover x
//! the brush's opacity on top. A stroke that crosses itself does not build up, as a painting program's stroke at an
//! opacity does not. Paint lays the colour over what is there, Erase takes the layer away, Blur blends it toward a
//! box blur of itself.
//!
//! The renderer reads a layer through `ImageSet::paints` (`PaintParams::pixels` names it) and samples it bilinearly
//! at every direction it draws (`sample`): a lookup of at most four texels, whatever was painted.

use super::*;
use makepad_render_material::ibl::{dir_to_equirect_uv, equirect_uv_to_dir, EnvMap};
use std::f32::consts::PI;
use std::sync::Arc;

/// A pixel layer's width in texels (about 0.18 degrees a texel); its height is half.
pub const PAINT_WIDTH: usize = 2048;
pub const PAINT_HEIGHT: usize = PAINT_WIDTH / 2;
/// A tile's side in texels.
pub const PAINT_TILE: usize = 64;
const TILES_X: usize = PAINT_WIDTH / PAINT_TILE;
const TILES_Y: usize = PAINT_HEIGHT / PAINT_TILE;
const TILE_TEXELS: usize = PAINT_TILE * PAINT_TILE;
/// One texel's height on the sky, radians.
const TEXEL_ANGLE: f32 = PI / PAINT_HEIGHT as f32;

type Tile = Arc<Vec<[f32; 4]>>;

/// A pixel layer's picture: its tiles, None where nothing was painted.
#[derive(Clone, Default)]
pub struct PaintRaster {
    /// Row-major tiles, TILES_X a row; empty for a layer nothing was painted on.
    tiles: Vec<Option<Tile>>,
}

impl std::fmt::Debug for PaintRaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PaintRaster({} of {} tiles painted)", self.painted_tiles(), TILES_X * TILES_Y)
    }
}

impl PaintRaster {
    pub fn is_empty(&self) -> bool {
        self.painted_tiles() == 0
    }

    /// How many tiles hold paint.
    pub fn painted_tiles(&self) -> usize {
        self.tiles.iter().filter(|tile| tile.is_some()).count()
    }

    /// The texel at column `x` (wrapping) and row `y` (held to the picture).
    #[inline]
    pub fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        let x = x.rem_euclid(PAINT_WIDTH as i64) as usize;
        let y = y.clamp(0, PAINT_HEIGHT as i64 - 1) as usize;
        match self.tiles.get((y / PAINT_TILE) * TILES_X + x / PAINT_TILE) {
            Some(Some(tile)) => tile[(y % PAINT_TILE) * PAINT_TILE + x % PAINT_TILE],
            _ => [0.0; 4],
        }
    }

    /// The layer along `dir` (unit, in the map's own frame), bilinear: premultiplied radiance and cover.
    pub fn sample(&self, dir: Vec3f) -> [f32; 4] {
        if self.tiles.is_empty() {
            return [0.0; 4];
        }
        let uv = dir_to_equirect_uv([dir.x, dir.y, dir.z]);
        let fx = uv[0] * PAINT_WIDTH as f32 - 0.5;
        let fy = uv[1] * PAINT_HEIGHT as f32 - 0.5;
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (a, b, c, d) = (self.texel(x0, y0), self.texel(x0 + 1, y0), self.texel(x0, y0 + 1), self.texel(x0 + 1, y0 + 1));
        let mut out = [0.0; 4];
        for k in 0..4 {
            let top = a[k] + (b[k] - a[k]) * tx;
            let bottom = c[k] + (d[k] - c[k]) * tx;
            out[k] = top + (bottom - top) * ty;
        }
        out
    }

    /// The tile holding texel (x, y), made this raster's own to write (a copy of a shared one, a blank one where
    /// nothing was painted).
    fn tile_mut(&mut self, x: usize, y: usize) -> &mut Vec<[f32; 4]> {
        if self.tiles.is_empty() {
            self.tiles = vec![None; TILES_X * TILES_Y];
        }
        let slot = &mut self.tiles[(y / PAINT_TILE) * TILES_X + x / PAINT_TILE];
        Arc::make_mut(slot.get_or_insert_with(|| Arc::new(vec![[0.0; 4]; TILE_TEXELS])))
    }

    fn set(&mut self, x: usize, y: usize, value: [f32; 4]) {
        self.tile_mut(x, y)[(y % PAINT_TILE) * PAINT_TILE + x % PAINT_TILE] = value;
    }

    /// The layer as one picture (premultiplied, alpha the cover), for a file.
    pub fn to_env_map(&self) -> EnvMap {
        let mut data = vec![[0.0f32; 4]; PAINT_WIDTH * PAINT_HEIGHT];
        for (i, tile) in self.tiles.iter().enumerate() {
            let Some(tile) = tile else { continue };
            let (tx, ty) = ((i % TILES_X) * PAINT_TILE, (i / TILES_X) * PAINT_TILE);
            for row in 0..PAINT_TILE {
                let at = (ty + row) * PAINT_WIDTH + tx;
                data[at..at + PAINT_TILE].copy_from_slice(&tile[row * PAINT_TILE..(row + 1) * PAINT_TILE]);
            }
        }
        EnvMap { width: PAINT_WIDTH, height: PAINT_HEIGHT, data }
    }

    /// A layer from a file `to_env_map` wrote, as `image::read_exr` reads it: the colour straight (OpenEXR keeps
    /// it premultiplied, as a layer does, and the reader divides the alpha out), so it is multiplied by the cover
    /// again. Another size is resampled; texels that are not finite read as nothing, and the cover is held to 0..1.
    pub fn from_env_map(map: &EnvMap) -> PaintRaster {
        let mut raster = PaintRaster::default();
        if map.width == 0 || map.height == 0 || map.data.len() < map.width * map.height {
            return raster;
        }
        let same = map.width == PAINT_WIDTH && map.height == PAINT_HEIGHT;
        for y in 0..PAINT_HEIGHT {
            for x in 0..PAINT_WIDTH {
                let t = if same {
                    map.data[y * PAINT_WIDTH + x]
                } else {
                    map.sample_uv([(x as f32 + 0.5) / PAINT_WIDTH as f32, (y as f32 + 0.5) / PAINT_HEIGHT as f32])
                };
                let clean = |v: f32| if v.is_finite() { v.max(0.0) } else { 0.0 };
                let a = clean(t[3]).min(1.0);
                let t = [clean(t[0]) * a, clean(t[1]) * a, clean(t[2]) * a, a];
                if t != [0.0; 4] {
                    raster.set(x, y, t);
                }
            }
        }
        raster
    }
}

/// What a brush does to the layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrushTool {
    /// Lays its colour over the layer.
    #[default]
    Paint,
    /// Takes the layer away.
    Erase,
    /// Blends the layer toward a box blur of itself, as far as the brush's `strength` reaches.
    Blur,
}

impl BrushTool {
    pub const ALL: [BrushTool; 3] = [BrushTool::Paint, BrushTool::Erase, BrushTool::Blur];

    pub fn word(self) -> &'static str {
        match self {
            BrushTool::Paint => "paint",
            BrushTool::Erase => "erase",
            BrushTool::Blur => "blur",
        }
    }
}

/// The brush's size on the sky (its diameter), degrees: the row's Size slider spans this, from a fine line to half
/// the sky across.
pub const BRUSH_SIZE_MIN_DEG: f32 = 0.1;
pub const BRUSH_SIZE_MAX_DEG: f32 = 180.0;
pub const BRUSH_SIZE_DEFAULT_DEG: f32 = 2.0;

/// Blur's strength when nothing sets it: a box a quarter of the brush's radius each way.
pub const BLUR_STRENGTH_DEFAULT: f32 = 0.5;

/// A brush as one stroke uses it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Brush {
    pub tool: BrushTool,
    /// The brush's radius on the sky, radians.
    pub radius: f32,
    /// 0..1: the share of the radius its edge fades over (0 is a hard edge, antialiased over a texel).
    pub softness: f32,
    /// 0..1: how much of the brush a texel it fully covers takes.
    pub opacity: f32,
    /// Paint's radiance (linear colour x 2^EV).
    pub color: [f32; 3],
    /// 0..1, Blur's reach: its box is this share of half the brush's radius each way (at least a texel).
    pub strength: f32,
}

/// One stroke under way: the layer it began on, the cover it has laid, and the layer with it.
#[derive(Clone, Debug)]
pub struct BrushStroke {
    brush: Brush,
    base: PaintRaster,
    result: PaintRaster,
    /// Per tile, the most of the brush each texel took so far.
    cover: Vec<Option<Box<[f32]>>>,
    last: Option<Vec3f>,
    dabs: usize,
}

impl BrushStroke {
    pub fn new(base: PaintRaster, brush: Brush) -> BrushStroke {
        let radius = if brush.radius.is_finite() { brush.radius.clamp(0.5 * TEXEL_ANGLE, PI) } else { TEXEL_ANGLE };
        let clean = |v: f32, hi: f32| if v.is_finite() { v.clamp(0.0, hi) } else { 0.0 };
        let brush = Brush {
            radius,
            softness: clean(brush.softness, 1.0),
            opacity: clean(brush.opacity, 1.0),
            color: brush.color.map(|c| clean(c, f32::MAX)),
            strength: clean(brush.strength, 1.0),
            ..brush
        };
        BrushStroke { brush, result: base.clone(), base, cover: vec![None; TILES_X * TILES_Y], last: None, dabs: 0 }
    }

    /// The layer with the stroke so far.
    pub fn result(&self) -> &PaintRaster {
        &self.result
    }

    /// How many dabs the stroke has laid.
    pub fn dabs(&self) -> usize {
        self.dabs
    }

    /// The brush goes on through `dirs` (unit directions in the map's own frame, in order): a dab every fifth of
    /// its radius along the great circles between them (at least a texel apart), the first one where it starts.
    pub fn add(&mut self, dirs: &[Vec3f]) {
        let shaped: Vec<(Vec3f, Option<DabShape>)> = dirs.iter().map(|&dir| (dir, None)).collect();
        self.add_shaped(&shaped);
    }

    /// `add`, each direction with the shape its dabs take there: None is the round brush of `Brush::radius`, Some
    /// a brush that lies on a surface (`DabShape`), whose dabs are spaced a fifth of its narrowest reach apart.
    pub fn add_shaped(&mut self, dirs: &[(Vec3f, Option<DabShape>)]) {
        for &(dir, shape) in dirs {
            let dir = dir.normalize();
            if !(dir.length_squared() > 0.5) {
                continue;
            }
            let reach = shape.map_or(self.brush.radius, |j| singular_values(j).1);
            let step = (0.2 * reach).max(TEXEL_ANGLE);
            match self.last {
                None => self.dab(dir, shape),
                Some(last) => {
                    let angle = last.dot(dir).clamp(-1.0, 1.0).acos();
                    if angle < step {
                        continue;
                    }
                    let n = (angle / step).floor() as usize;
                    for i in 1..=n {
                        self.dab(slerp(last, dir, angle, i as f32 * step / angle), shape);
                    }
                }
            }
        }
    }

    /// One dab at `centre` (`shape` as `add_shaped` takes it): the cover of every texel the brush reaches is raised,
    /// and those texels are drawn again from the layer the stroke began on.
    fn dab(&mut self, centre: Vec3f, shape: Option<DabShape>) {
        let brush = self.brush;
        let Some(foot) = Footprint::new(centre, brush, shape) else { return };
        self.last = Some(centre);
        self.dabs += 1;
        let r = foot.reach;
        let cos_r = r.cos();
        let theta_c = centre.y.clamp(-1.0, 1.0).acos();
        let phi_c = centre.x.atan2(-centre.z);
        let row_of = |theta: f32| theta / PI * PAINT_HEIGHT as f32;
        let y0 = (row_of(theta_c - r).floor() as i64 - 1).max(0) as usize;
        let y1 = (row_of(theta_c + r).ceil() as i64 + 1).min(PAINT_HEIGHT as i64 - 1) as usize;
        let pole = theta_c - r <= 0.0 || theta_c + r >= PI;
        let (st_c, ct_c) = theta_c.sin_cos();
        // The texels the blur reads, filled once a dab.
        let blur = (brush.tool == BrushTool::Blur).then(|| BoxBlur::of(&self.base, y0, y1, phi_c, r, brush.strength, pole));
        for y in y0..=y1 {
            let theta = (y as f32 + 0.5) / PAINT_HEIGHT as f32 * PI;
            let (st, ct) = theta.sin_cos();
            // The columns within the reach on this row: |dphi| <= acos((cos r - cos t cos tc) / (sin t sin tc)).
            let (x_from, count) = if pole || st * st_c <= 1.0e-6 {
                (0i64, PAINT_WIDTH as i64)
            } else {
                let arg = (cos_r - ct * ct_c) / (st * st_c);
                if arg >= 1.0 {
                    continue;
                }
                let half = if arg <= -1.0 { PI } else { arg.acos() };
                let cols = half / (2.0 * PI) * PAINT_WIDTH as f32;
                let mid = (0.5 + phi_c / (2.0 * PI)) * PAINT_WIDTH as f32 - 0.5;
                let from = (mid - cols).floor() as i64 - 1;
                (from, ((2.0 * cols).ceil() as i64 + 3).min(PAINT_WIDTH as i64))
            };
            for i in 0..count {
                let x = (x_from + i).rem_euclid(PAINT_WIDTH as i64) as usize;
                let d = equirect_uv_to_dir([(x as f32 + 0.5) / PAINT_WIDTH as f32, (y as f32 + 0.5) / PAINT_HEIGHT as f32]);
                let w = foot.weight(vec3f(d[0], d[1], d[2]));
                if !(w > 0.0) {
                    continue;
                }
                self.apply(x, y, w, blur.as_ref());
            }
        }
    }

    /// The brush over the whole layer at full cover (Flood): Paint lays its colour everywhere, Erase takes the
    /// layer away, Blur blurs all of it, each at the brush's opacity.
    pub fn flood(&mut self) {
        self.dabs += 1;
        let brush = self.brush;
        let blur = (brush.tool == BrushTool::Blur).then(|| BoxBlur::of(&self.base, 0, PAINT_HEIGHT - 1, 0.0, brush.radius, brush.strength, true));
        for y in 0..PAINT_HEIGHT {
            for x in 0..PAINT_WIDTH {
                self.apply(x, y, 1.0, blur.as_ref());
            }
        }
    }

    /// Raises texel (x, y)'s cover to `w` and draws it again from the layer the stroke began on; a cover it already
    /// has changes nothing.
    fn apply(&mut self, x: usize, y: usize, w: f32, blur: Option<&BoxBlur>) {
        let brush = self.brush;
        let cover = self.cover_mut(x, y);
        if w <= *cover {
            return;
        }
        *cover = w;
        let a = w * brush.opacity;
        let under = self.base.texel(x as i64, y as i64);
        let out = match brush.tool {
            BrushTool::Paint => {
                let keep = 1.0 - a;
                [under[0] * keep + brush.color[0] * a, under[1] * keep + brush.color[1] * a, under[2] * keep + brush.color[2] * a, under[3] * keep + a]
            }
            BrushTool::Erase => under.map(|v| v * (1.0 - a)),
            BrushTool::Blur => {
                let blurred = blur.map_or(under, |b| b.at(x, y));
                std::array::from_fn(|k| under[k] + (blurred[k] - under[k]) * a)
            }
        };
        // Unchanged (erasing or blurring where nothing is painted): no tile is made for it.
        if out != under {
            self.result.set(x, y, out);
        }
    }

    fn cover_mut(&mut self, x: usize, y: usize) -> &mut f32 {
        let tile = self.cover[(y / PAINT_TILE) * TILES_X + x / PAINT_TILE].get_or_insert_with(|| vec![0.0; TILE_TEXELS].into_boxed_slice());
        &mut tile[(y % PAINT_TILE) * PAINT_TILE + x % PAINT_TILE]
    }
}

/// A dab's shape where the brush lies on a surface (the Object view's Follow surface): the 2 x 2 map, row-major,
/// from offsets on the surface in units of the brush's radius (the unit disc is the brush) to offsets on the sky,
/// radians along `sky_basis(centre)`'s two axes. The brush's sky footprint is that disc's image, an ellipse.
pub type DabShape = [f32; 4];

/// The two axes on the sky round `centre` a `DabShape` is written in: east and up along it (any turn about the
/// vertical keeps them, so a map rotation leaves a shape as it is); about a pole, along +X.
pub fn sky_basis(centre: Vec3f) -> (Vec3f, Vec3f) {
    let up = vec3f(0.0, 1.0, 0.0);
    let east = Vec3f::cross(up, centre);
    let e1 = if east.length() > 1.0e-4 { east.normalize() } else { vec3f(1.0, 0.0, 0.0) };
    (e1, Vec3f::cross(centre, e1).normalize())
}

/// The largest and smallest stretch of a `DabShape` (its singular values), radians per brush radius.
fn singular_values(j: DabShape) -> (f32, f32) {
    let a = j[0] * j[0] + j[2] * j[2];
    let b = j[0] * j[1] + j[2] * j[3];
    let c = j[1] * j[1] + j[3] * j[3];
    let mid = 0.5 * (a + c);
    let spread = (0.25 * (a - c) * (a - c) + b * b).sqrt();
    ((mid + spread).max(0.0).sqrt(), (mid - spread).max(0.0).sqrt())
}

/// Where one dab reaches and how much of it each direction takes.
struct Footprint {
    centre: Vec3f,
    /// Nothing beyond this angle from the centre is touched.
    reach: f32,
    cos_reach: f32,
    kind: FootKind,
}

enum FootKind {
    /// A round brush: full cover inside `inner`, fading to the radius `reach` (angles).
    Round { inner: f32, cos_inner: f32 },
    /// A brush on a surface: directions taken back onto the surface's unit disc through `inv`, full cover inside
    /// `inner`, fading to 1.
    Shaped { e1: Vec3f, e2: Vec3f, inv: [f32; 4], inner: f32 },
}

impl Footprint {
    /// None for a shape that has no inside (seen edge on).
    fn new(centre: Vec3f, brush: Brush, shape: Option<DabShape>) -> Option<Footprint> {
        match shape {
            None => {
                let r = brush.radius;
                // The edge fades from `inner` to the radius, at least a texel wide.
                let inner = (r * (1.0 - brush.softness)).min(r - TEXEL_ANGLE).max(0.0);
                Some(Footprint { centre, reach: r, cos_reach: r.cos(), kind: FootKind::Round { inner, cos_inner: inner.cos() } })
            }
            Some(j) => {
                let (most, least) = singular_values(j);
                let det = j[0] * j[3] - j[1] * j[2];
                if !(least > 1.0e-6) || !(det.abs() > 1.0e-12) || !most.is_finite() {
                    return None;
                }
                let inv = [j[3] / det, -j[1] / det, -j[2] / det, j[0] / det];
                // Held to the hemisphere round the centre, where the tangent offsets below are one to one.
                let reach = most.min(0.5 * PI);
                let inner = (1.0 - brush.softness).min(1.0 - TEXEL_ANGLE / least).max(0.0);
                let (e1, e2) = sky_basis(centre);
                Some(Footprint { centre, reach, cos_reach: reach.cos(), kind: FootKind::Shaped { e1, e2, inv, inner } })
            }
        }
    }

    /// How much of the brush direction `d` (unit) takes, 0..1.
    fn weight(&self, d: Vec3f) -> f32 {
        let c = d.dot(self.centre);
        if c <= self.cos_reach {
            return 0.0;
        }
        match &self.kind {
            FootKind::Round { inner, cos_inner } => {
                if c >= *cos_inner {
                    1.0
                } else {
                    1.0 - smoothstep(*inner, self.reach, c.clamp(-1.0, 1.0).acos())
                }
            }
            FootKind::Shaped { e1, e2, inv, inner } => {
                let (s, t) = (d.dot(*e1), d.dot(*e2));
                let (u, v) = (inv[0] * s + inv[1] * t, inv[2] * s + inv[3] * t);
                1.0 - smoothstep(*inner, 1.0, (u * u + v * v).sqrt())
            }
        }
    }
}

/// A box blur of a layer over the rows and columns a dab may touch: each texel the mean of the texels within
/// `strength` x half the brush's radius of it (at least one texel), by sliding sums along rows and columns.
struct BoxBlur {
    y0: usize,
    x0: i64,
    width: usize,
    data: Vec<[f32; 4]>,
}

impl BoxBlur {
    fn of(raster: &PaintRaster, y0: usize, y1: usize, phi_c: f32, r: f32, strength: f32, pole: bool) -> BoxBlur {
        let k = ((0.5 * strength * r / TEXEL_ANGLE).round() as i64).max(1);
        // The columns: the whole row near a pole, else as wide as the dab is at its widest row, padded by the box.
        let (x0, width) = if pole {
            (0, PAINT_WIDTH)
        } else {
            let widest = (y0..=y1).map(|y| ((y as f32 + 0.5) / PAINT_HEIGHT as f32 * PI).sin()).fold(1.0f32, f32::min).max(1.0e-3);
            let half = ((r / widest).min(PI) / (2.0 * PI) * PAINT_WIDTH as f32).ceil() as i64 + 2;
            let mid = ((0.5 + phi_c / (2.0 * PI)) * PAINT_WIDTH as f32) as i64;
            let width = ((2 * half + 1) as usize).min(PAINT_WIDTH);
            (mid - half, width)
        };
        let (ry0, ry1) = (y0 as i64 - k, y1 as i64 + k);
        let rows = (ry1 - ry0 + 1) as usize;
        // Across: each row's sums over 2k + 1 columns (the row wraps).
        let mut across = vec![[0.0f32; 4]; rows * width];
        for (j, y) in (ry0..=ry1).enumerate() {
            let mut sum = [0.0f32; 4];
            for x in x0 - k..=x0 + k {
                add(&mut sum, raster.texel(x, y), 1.0);
            }
            for i in 0..width {
                let x = x0 + i as i64;
                across[j * width + i] = sum;
                add(&mut sum, raster.texel(x + k + 1, y), 1.0);
                add(&mut sum, raster.texel(x - k, y), -1.0);
            }
        }
        // Down: each column's sums of those over 2k + 1 rows, as a mean.
        let n = ((2 * k + 1) * (2 * k + 1)) as f32;
        let out_rows = y1 - y0 + 1;
        let mut data = vec![[0.0f32; 4]; out_rows * width];
        let k = k as usize;
        for i in 0..width {
            let mut sum = [0.0f32; 4];
            for j in 0..2 * k + 1 {
                add(&mut sum, across[j * width + i], 1.0);
            }
            for o in 0..out_rows {
                data[o * width + i] = sum.map(|v| v / n);
                if o + 1 < out_rows {
                    add(&mut sum, across[(o + 2 * k + 1) * width + i], 1.0);
                    add(&mut sum, across[o * width + i], -1.0);
                }
            }
        }
        BoxBlur { y0, x0, width, data }
    }

    fn at(&self, x: usize, y: usize) -> [f32; 4] {
        let i = (x as i64 - self.x0).rem_euclid(PAINT_WIDTH as i64) as usize;
        if i >= self.width || y < self.y0 {
            return [0.0; 4];
        }
        self.data.get((y - self.y0) * self.width + i).copied().unwrap_or([0.0; 4])
    }
}

fn add(sum: &mut [f32; 4], t: [f32; 4], sign: f32) {
    for k in 0..4 {
        sum[k] += sign * t[k];
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = if e1 > e0 { ((x - e0) / (e1 - e0)).clamp(0.0, 1.0) } else if x < e0 { 0.0 } else { 1.0 };
    t * t * (3.0 - 2.0 * t)
}

/// The point a share `t` of the way from `a` to `b` along their great circle (`angle` apart).
fn slerp(a: Vec3f, b: Vec3f, angle: f32, t: f32) -> Vec3f {
    let s = angle.sin();
    if s < 1.0e-6 {
        return (a + (b - a) * t).normalize();
    }
    (a * (((1.0 - t) * angle).sin() / s) + b * ((t * angle).sin() / s)).normalize()
}

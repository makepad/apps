//! A scan's detail as it gathers: each keyframe's dense points ([`crate::detail`]) started from and
//! held to the base surface as its cameras see it ([`BaseSeen`]), and gathered into one set of
//! points a cell wide each ([`Refined`]) that says which of the base's cells it stands in for.

use crate::detail::{DetailPoint, Matcher};
use makepad_ai_geometry::views::GrayView;
use makepad_desk_link::xr_capture::{CaptureCamera, CaptureKeyframe};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// The base surface as one camera sees it: the depth of its nearest point through each of the
/// picture's pixels, `shrink` pixels a cell.
pub struct BaseSeen {
    centre: [f64; 3],
    /// The camera's axes in the world, as rows.
    axes: [[f64; 3]; 3],
    k: [f64; 4],
    shrink: f64,
    width: usize,
    height: usize,
    depth: Vec<f32>,
}

impl BaseSeen {
    /// `base`: the surface's points and the width of the square each stands for.
    pub fn of(view: &GrayView, base: &[([f32; 3], f32)], shrink: usize) -> BaseSeen {
        let m = &view.cam_to_world;
        let centre = [m[0][3], m[1][3], m[2][3]];
        let axes: [[f64; 3]; 3] = std::array::from_fn(|a| [m[0][a], m[1][a], m[2][a]]);
        let shrink = shrink.max(1);
        let (width, height) = (view.width.div_ceil(shrink), view.height.div_ceil(shrink));
        let mut depth = vec![f32::INFINITY; width * height];
        for (point, size) in base {
            let d: [f64; 3] = std::array::from_fn(|i| f64::from(point[i]) - centre[i]);
            let p: [f64; 3] = std::array::from_fn(|a| axes[a][0] * d[0] + axes[a][1] * d[1] + axes[a][2] * d[2]);
            if p[2] < 0.05 {
                continue;
            }
            let (u, v) = ((view.k[0] * p[0] / p[2] + view.k[2]) / shrink as f64, (view.k[1] * p[1] / p[2] + view.k[3]) / shrink as f64);
            // half the square's width on the picture, and a cell more: no gaps between neighbours
            let reach = (0.5 * f64::from(*size) * view.k[0] / p[2] / shrink as f64).ceil() + 1.0;
            let (x0, x1) = ((u - reach).floor().max(0.0), (u + reach).ceil().min(width as f64 - 1.0));
            let (y0, y1) = ((v - reach).floor().max(0.0), (v + reach).ceil().min(height as f64 - 1.0));
            if x0 > x1 || y0 > y1 {
                continue;
            }
            for y in y0 as usize..=y1 as usize {
                for x in x0 as usize..=x1 as usize {
                    let at = &mut depth[y * width + x];
                    *at = at.min(p[2] as f32);
                }
            }
        }
        BaseSeen { centre, axes, k: view.k, shrink: shrink as f64, width, height, depth }
    }

    /// How far along the unit `ray` (in the world) from the camera the base surface lies.
    pub fn along(&self, ray: [f64; 3]) -> Option<f64> {
        let p: [f64; 3] = std::array::from_fn(|a| self.axes[a][0] * ray[0] + self.axes[a][1] * ray[1] + self.axes[a][2] * ray[2]);
        if p[2] <= 1e-6 {
            return None;
        }
        let (u, v) = ((self.k[0] * p[0] / p[2] + self.k[2]) / self.shrink, (self.k[1] * p[1] / p[2] + self.k[3]) / self.shrink);
        if u < 0.0 || v < 0.0 || u >= self.width as f64 || v >= self.height as f64 {
            return None;
        }
        let z = self.depth[v as usize * self.width + u as usize];
        z.is_finite().then(|| f64::from(z) / p[2])
    }
}

/// The dense points of a keyframe, started from and held to the base surface as each of its
/// cameras sees it.
pub fn refine(matcher: &dyn Matcher, keyframe: &CaptureKeyframe, cameras: &[CaptureCamera], base: &[([f32; 3], f32)], shrink: usize, cell: f32) -> Result<Vec<DetailPoint>, String> {
    refine_around(matcher, keyframe, cameras, base, shrink, cell, None)
}

/// The square of each picture a keyframe's detail is matched in, pixels a side: what a match
/// costs is its pixels, so the square is always this size and what changes is how much of the
/// picture it holds.
pub const WINDOW: usize = 640;
/// How far each way of what the cameras look at the detail is wanted, metres.
pub const REGION: f64 = 0.2;

/// [`refine`] as a scan runs it: in a square of [`WINDOW`] pixels about `place` (what the
/// cameras look at; the pictures' middles where there is none). From far enough that the
/// [`REGION`] about the place fits the square at the pictures' own size, it is matched at that
/// size: twice the detail on what is scanned. From nearer, where the region is wider than the
/// square, the pictures are halved first and the square holds twice as much of them: the detail
/// a nearer look gives anyway. `hurried` (keyframes waiting): half the size again.
/// Returns the points and the size the pictures were matched at (1: their own).
pub fn refine_looked(matcher: &dyn Matcher, keyframe: &CaptureKeyframe, cameras: &[CaptureCamera], base: &[([f32; 3], f32)], cell: f32, place: Option<[f64; 3]>, hurried: bool) -> Result<(Vec<DetailPoint>, usize), String> {
    let first = crate::detail::pair_of(keyframe, cameras, 1)?.into_iter().next().ok_or("a keyframe with no pictures")?;
    let away = place.map(|p| (0..3).map(|i| (p[i] - first.cam_to_world[i][3]).powi(2)).sum::<f64>().sqrt());
    let fits = away.is_some_and(|away| 2.0 * REGION * first.k[0] / away.max(0.05) <= WINDOW as f64);
    let shrink = if fits { 1 } else { 2 } * if hurried { 2 } else { 1 };
    let side = if hurried { WINDOW / 2 } else { WINDOW };
    Ok((refine_window(matcher, keyframe, cameras, base, shrink, cell, place, side)?, shrink))
}

/// [`refine`], of a square of the pictures alone, `side` pixels a side, about where `place` shows
/// ([`crate::detail::window`]): the pictures' own size on what is being scanned.
pub fn refine_window(matcher: &dyn Matcher, keyframe: &CaptureKeyframe, cameras: &[CaptureCamera], base: &[([f32; 3], f32)], shrink: usize, cell: f32, place: Option<[f64; 3]>, side: usize) -> Result<Vec<DetailPoint>, String> {
    let whole = crate::detail::pair_of(keyframe, cameras, shrink)?;
    let pictures = crate::detail::pictures_of(keyframe, cameras, shrink)?;
    let (views, pictures): (Vec<GrayView>, Vec<_>) = whole.iter().zip(&pictures).map(|(view, picture)| crate::detail::window(view, picture, place, side)).unzip();
    refine_views(matcher, views, &pictures, base, cell)
}

/// [`crate::detail::floor_pixels_by_slant`] of a keyframe as [`refine_looked`] matches it.
pub fn looked_floor_pixels(matcher: &dyn Matcher, keyframe: &CaptureKeyframe, cameras: &[CaptureCamera], base: &[([f32; 3], f32)], place: Option<[f64; 3]>, hurried: bool) -> Result<[[usize; 6]; 5], String> {
    let first = crate::detail::pair_of(keyframe, cameras, 1)?.into_iter().next().ok_or("a keyframe with no pictures")?;
    let away = place.map(|p| (0..3).map(|i| (p[i] - first.cam_to_world[i][3]).powi(2)).sum::<f64>().sqrt());
    let fits = away.is_some_and(|away| 2.0 * REGION * first.k[0] / away.max(0.05) <= WINDOW as f64);
    let shrink = if fits { 1 } else { 2 } * if hurried { 2 } else { 1 };
    let whole = crate::detail::pair_of(keyframe, cameras, shrink)?;
    let pictures = crate::detail::pictures_of(keyframe, cameras, shrink)?;
    let views: Vec<GrayView> = whole.iter().zip(&pictures).map(|(view, picture)| crate::detail::window(view, picture, place, if hurried { WINDOW / 2 } else { WINDOW }).0).collect();
    let seen: Vec<BaseSeen> = views.iter().map(|view| BaseSeen::of(view, base, 4)).collect();
    let coarse = |centre: [f64; 3], ray: [f64; 3]| {
        let off = |s: &BaseSeen| (0..3).map(|i| (s.centre[i] - centre[i]).powi(2)).sum::<f64>();
        seen.iter().min_by(|a, b| off(a).total_cmp(&off(b))).and_then(|s| s.along(ray))
    };
    crate::detail::floor_pixels_by_slant(matcher, &views, &coarse)
}

/// [`refine`], of the part of the pictures around a place in the world alone (`around`: the place
/// and how far each way of it, metres) where one is given.
pub fn refine_around(matcher: &dyn Matcher, keyframe: &CaptureKeyframe, cameras: &[CaptureCamera], base: &[([f32; 3], f32)], shrink: usize, cell: f32, around: Option<([f64; 3], f64)>) -> Result<Vec<DetailPoint>, String> {
    let mut views = crate::detail::pair_of(keyframe, cameras, shrink)?;
    let mut pictures = crate::detail::pictures_of(keyframe, cameras, shrink)?;
    if let Some((place, half)) = around {
        let cut: Option<Vec<_>> = views.iter().zip(&pictures).map(|(view, picture)| crate::detail::around(view, picture, place, half)).collect();
        (views, pictures) = cut.ok_or("the place is not in the keyframe's pictures")?.into_iter().unzip();
    }
    refine_views(matcher, views, &pictures, base, cell)
}

fn refine_views(matcher: &dyn Matcher, views: Vec<GrayView>, pictures: &[crate::colour::Picture], base: &[([f32; 3], f32)], cell: f32) -> Result<Vec<DetailPoint>, String> {
    let seen: Vec<BaseSeen> = views.iter().map(|view| BaseSeen::of(view, base, 4)).collect();
    // (the camera a ray leaves from is the one whose centre it is given)
    let coarse = |centre: [f64; 3], ray: [f64; 3]| {
        let off = |s: &BaseSeen| (0..3).map(|i| (s.centre[i] - centre[i]).powi(2)).sum::<f64>();
        seen.iter().min_by(|a, b| off(a).total_cmp(&off(b))).and_then(|s| s.along(ray))
    };
    crate::detail::detail_of_views(matcher, views.iter().map(|v| GrayView { pixels: v.pixels.clone(), ..*v }).collect(), pictures, &coarse, cell)
}

/// The detail gathered so far: one point a cell, the mean of what the keyframes gave there.
pub struct Refined {
    cell: f32,
    /// Per cell: the sums of its points, normals and colours, their count, and the narrowest
    /// square any of them stood for (a nearer or larger look at the place gives the finer one).
    cells: BTreeMap<[i32; 3], ([f64; 3], [f64; 3], [f64; 3], f64, f32)>,
    /// Per cell: how many keyframes saw through it ([`Refined::carve`]).
    through: HashMap<[i32; 3], f64>,
    /// The surface in the large: per patch ([`PATCH`] cells wide) the sums of its points and
    /// normals and their count. A patch's mean point and normal are the plane the surface has
    /// there, which a new keyframe's points are held against.
    patches: BTreeMap<[i32; 3], ([f64; 3], [f64; 3], f64)>,
    /// The picture on the surface, in the large: per field ([`FIELDS`] cells wide, the widest
    /// first) the sums of its points, normals and brightnesses and their count. What a new
    /// keyframe's picture is held against along the surface, where the shape holds nothing
    /// ([`Refined::settle`]); the cells are the finest of them.
    fields: [BTreeMap<[i32; 3], Field>; 2],
    /// Every keyframe added so far, and the sums of the turns and shifts that settled them. The
    /// first keyframe is where the others are settled onto, so the detail lies where the tracker
    /// put that one keyframe, its error and all. The tracker's errors differ keyframe to
    /// keyframe: the mean of the moves is how far the first lies from where the keyframes put
    /// the surface between them, and the points are given moved back by it ([`Refined::points`]).
    keyframes: f64,
    turned: [f64; 3],
    shifted: [f64; 3],
}

/// The sums of a field's points, normals and brightnesses, and their count.
type Field = ([f64; 3], [f64; 3], f64, f64);

/// A patch's width, in cells.
const PATCH: f32 = 4.0;
/// The fields' widths, in cells: the tracker is off by a few cells, and a picture is only held
/// against one whose features are wider than what it is off by, so the widest goes first.
const FIELDS: [f64; 2] = [16.0, 4.0];
/// How many metres off the surface a brightness difference of one (black to white) counts as:
/// one keyframe's matching scatters a point by about a millimetre and its brightness by a few
/// hundredths, and both pull a keyframe with the same weight at that ratio.
const PATTERN: f64 = 0.03;
/// The largest brightness difference between a keyframe and the detail that is the same paint.
const SAME_PAINT: f64 = 0.25;
/// Of the cells, one in so many is held against the picture: a keyframe has a hundred thousand.
const CELLS_HELD: i32 = 8;

fn brightness(colour: [f32; 3]) -> f64 {
    0.299 * f64::from(colour[0]) + 0.587 * f64::from(colour[1]) + 0.114 * f64::from(colour[2])
}

/// What the picture says of a keyframe's place along the surface, at one width. The keyframe's
/// points are gathered in fields `width` wide (`place` gives where a point is gathered, `held`
/// which fields are taken); `stored` gives the detail's field there: its mean point, the sum of
/// its normals, its mean brightness and its count. Where the detail's brightness rises across
/// the neighbouring fields, a keyframe brighter than the detail lies up that rise: per field
/// the keyframe's mean point, the rise along the surface (brightness a metre, no part along the
/// normal: the shape holds that), how much brighter the keyframe is there, and its points.
fn pattern(
    points: &[DetailPoint], width: f64, place: &dyn Fn(&DetailPoint) -> [f64; 3], held: &dyn Fn([i32; 3]) -> bool, stored: &dyn Fn([i32; 3]) -> Option<([f64; 3], [f64; 3], f64, f64)>,
) -> Vec<([f64; 3], [f64; 3], f64, f64)> {
    let mut seen: BTreeMap<[i32; 3], ([f64; 3], f64, f64)> = BTreeMap::new();
    for p in points {
        let at = place(p);
        let key = at.map(|v| (v / width).floor() as i32);
        if held(key) {
            let entry = seen.entry(key).or_insert(([0.0; 3], 0.0, 0.0));
            for i in 0..3 {
                entry.0[i] += at[i];
            }
            entry.1 += brightness(p.colour);
            entry.2 += 1.0;
        }
    }
    let mut rows = Vec::with_capacity(seen.len());
    for (key, (sum, bright, count)) in seen {
        let Some((centre, normal, here, _)) = stored(key) else { continue };
        let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if length < 1e-9 {
            continue;
        }
        let n = normal.map(|v| v / length);
        // two directions along the surface
        let aside = if n[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        let u = [n[1] * aside[2] - n[2] * aside[1], n[2] * aside[0] - n[0] * aside[2], n[0] * aside[1] - n[1] * aside[0]];
        let size = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
        let u = u.map(|v| v / size);
        let v = [n[1] * u[2] - n[2] * u[1], n[2] * u[0] - n[0] * u[2], n[0] * u[1] - n[1] * u[0]];
        // the rise of the detail's brightness across the fields around this one, least squares
        let (mut uu, mut uv, mut vv, mut ub, mut vb, mut around) = (0.0, 0.0, 0.0, 0.0, 0.0, 0usize);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if (dx, dy, dz) == (0, 0, 0) {
                        continue;
                    }
                    let Some((there, _, other, _)) = stored([key[0] + dx, key[1] + dy, key[2] + dz]) else { continue };
                    let d: [f64; 3] = std::array::from_fn(|i| there[i] - centre[i]);
                    let (a, b) = ((0..3).map(|i| d[i] * u[i]).sum::<f64>(), (0..3).map(|i| d[i] * v[i]).sum::<f64>());
                    uu += a * a;
                    uv += a * b;
                    vv += b * b;
                    ub += a * (other - here);
                    vb += b * (other - here);
                    around += 1;
                }
            }
        }
        let det = uu * vv - uv * uv;
        if around < 5 || det < 1e-3 * uu * vv || det <= 0.0 {
            continue;
        }
        let (gu, gv) = ((vv * ub - uv * vb) / det, (uu * vb - uv * ub) / det);
        let rise: [f64; 3] = std::array::from_fn(|i| gu * u[i] + gv * v[i]);
        let at = sum.map(|s| s / count);
        let brighter = bright / count - here - (0..3).map(|i| rise[i] * (at[i] - centre[i])).sum::<f64>();
        rows.push((at, rise, brighter, count));
    }
    // (one keyframe's exposure differs from another's: what all its fields are brighter by says nothing of its place)
    let total: f64 = rows.iter().map(|r| r.3).sum();
    let exposure = rows.iter().map(|r| r.2 * r.3).sum::<f64>() / total.max(1.0);
    for row in &mut rows {
        row.2 -= exposure;
    }
    rows.retain(|r| r.2.abs() < SAME_PAINT);
    rows
}
/// The fewest points a patch holds for its plane to count.
const PATCH_FEWEST: f64 = 8.0;
/// How far from a patch's plane a point may lie and still be of that surface, metres: several
/// times what one keyframe's matching scatters by.
const OF_THE_SURFACE: f64 = 0.006;

impl Refined {
    pub fn new(cell: f32) -> Refined {
        Refined { cell, cells: BTreeMap::new(), through: HashMap::new(), patches: BTreeMap::new(), fields: [BTreeMap::new(), BTreeMap::new()], keyframes: 0.0, turned: [0.0; 3], shifted: [0.0; 3] }
    }

    /// The plane the surface has where `point` lies: a point of it and its unit normal.
    ///
    /// The patches are cubes fixed to the world, and a surface lies wherever it lies in them: a
    /// point a little off it (a pixel's mismatch at an edge, a few millimetres up) is as often in
    /// the cube over the surface's as in its own, with a few stray points for company or none.
    /// Held against that cube alone it found no plane, or the strays' own, and stood in a cell of
    /// its own over the surface: a second layer, wherever the scan happened to sit in the cubes.
    /// So the cubes next to the point's along its normal are asked too. Of those that hold a plane
    /// near the point, the well filled ones (a quarter of the fullest at least) are the surface,
    /// and the nearest of them is the point's.
    fn plane_at(&self, p: &DetailPoint) -> Option<([f64; 3], [f64; 3])> {
        let at = p.point.map(f64::from);
        let own = p.point.map(|v| (v / (PATCH * self.cell)).floor() as i32);
        // (the axis the point's normal lies most along)
        let axis = (0..3).max_by(|a, b| p.normal[*a].abs().total_cmp(&p.normal[*b].abs())).unwrap_or(1);
        let mut found: [Option<(f64, f64, [f64; 3], [f64; 3])>; 3] = [None; 3];
        for (slot, step) in found.iter_mut().zip([0, 1, -1]) {
            let mut key = own;
            key[axis] += step;
            let Some((sum, normal, count)) = self.patches.get(&key) else { continue };
            let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
            // (a patch across an edge holds two surfaces: its normals disagree, and it is no plane)
            if *count < PATCH_FEWEST || length <= 0.9 * count {
                continue;
            }
            let (q, n) = (sum.map(|v| v / count), normal.map(|v| v / length));
            let off: f64 = (0..3).map(|i| n[i] * (at[i] - q[i])).sum();
            // (a neighbour's plane that faces another way is another surface: a wall beside a floor)
            let faces: f64 = (0..3).map(|i| n[i] * f64::from(p.normal[i])).sum();
            if off.abs() < 2.0 * OF_THE_SURFACE && (step == 0 || faces > 0.8) {
                *slot = Some((*count, off.abs(), q, n));
            }
        }
        let fullest = found.iter().flatten().map(|f| f.0).fold(0.0, f64::max);
        found.iter().flatten().filter(|f| f.0 >= 0.25 * fullest).min_by(|a, b| a.1.total_cmp(&b.1)).map(|f| (f.2, f.3))
    }

    /// Adds a keyframe's points. One keyframe's matching scatters a point along its ray by more
    /// than a cell, so points of one place from several keyframes would fill several cells one
    /// over the other, as layers. A point near the surface already there is gathered in the cell
    /// under it on that surface (its place along the surface, its own height in the mean): the
    /// keyframes' heights average in one cell, and the surface comes out as one sheet.
    pub fn add(&mut self, points: &[DetailPoint]) {
        self.add_settled(points, None);
    }

    /// [`Refined::add`] of a keyframe's points after [`Refined::settle`] moved them: the move
    /// counts towards where the keyframes put the surface between them.
    pub fn add_settled(&mut self, points: &[DetailPoint], settled: Option<&Settled>) {
        self.add_in(points, settled, None);
    }

    /// [`Refined::add_settled`], keeping what was added so it can be taken out again
    /// ([`Refined::take_out`]): a keyframe matched in a hurry, to be matched again at leisure.
    pub fn add_kept(&mut self, points: &[DetailPoint], settled: Option<Settled>) -> Kept {
        let mut cells = Vec::with_capacity(points.len());
        self.add_in(points, settled.as_ref(), Some(&mut cells));
        Kept { points: points.to_vec(), cells, settled }
    }

    /// Takes a keyframe's points out again, from the cells, patches and fields they went in.
    pub fn take_out(&mut self, kept: &Kept) {
        if let Some(settled) = &kept.settled {
            for i in 0..3 {
                self.turned[i] -= settled.turn[i];
                self.shifted[i] -= settled.shift[i];
            }
        }
        self.keyframes -= 1.0;
        self.pull(kept);
    }

    /// A keyframe's points out of the cells, patches and fields they went in.
    fn pull(&mut self, kept: &Kept) {
        for (p, key) in kept.points.iter().zip(&kept.cells) {
            if let Some(entry) = self.cells.get_mut(key) {
                for i in 0..3 {
                    entry.0[i] -= f64::from(p.point[i]);
                    entry.1[i] -= f64::from(p.normal[i]);
                    entry.2[i] -= f64::from(p.colour[i]);
                }
                entry.3 -= 1.0;
                if entry.3 < 0.5 {
                    self.cells.remove(key);
                }
            }
            let key = p.point.map(|v| (v / (PATCH * self.cell)).floor() as i32);
            if let Some(entry) = self.patches.get_mut(&key) {
                for i in 0..3 {
                    entry.0[i] -= f64::from(p.point[i]);
                    entry.1[i] -= f64::from(p.normal[i]);
                }
                entry.2 -= 1.0;
                if entry.2 < 0.5 {
                    self.patches.remove(&key);
                }
            }
            for (fields, width) in self.fields.iter_mut().zip(FIELDS) {
                let key = p.point.map(|v| (f64::from(v) / (width * f64::from(self.cell))).floor() as i32);
                if let Some(entry) = fields.get_mut(&key) {
                    for i in 0..3 {
                        entry.0[i] -= f64::from(p.point[i]);
                        entry.1[i] -= f64::from(p.normal[i]);
                    }
                    entry.2 -= brightness(p.colour);
                    entry.3 -= 1.0;
                    if entry.3 < 0.5 {
                        fields.remove(&key);
                    }
                }
            }
        }
    }

    fn add_in(&mut self, points: &[DetailPoint], settled: Option<&Settled>, mut cells: Option<&mut Vec<[i32; 3]>>) {
        if let Some(settled) = settled {
            for i in 0..3 {
                self.turned[i] += settled.turn[i];
                self.shifted[i] += settled.shift[i];
            }
        }
        self.keyframes += 1.0;
        self.put(points, cells.as_deref_mut());
    }

    /// Points into the cells, patches and fields.
    fn put(&mut self, points: &[DetailPoint], mut cells: Option<&mut Vec<[i32; 3]>>) {
        for p in points {
            let at = p.point.map(f64::from);
            let key = self.cell_of(p);
            if let Some(cells) = cells.as_mut() {
                cells.push(key);
            }
            let entry = self.cells.entry(key).or_insert(([0.0; 3], [0.0; 3], [0.0; 3], 0.0, f32::INFINITY));
            entry.4 = entry.4.min(p.size);
            for i in 0..3 {
                entry.0[i] += at[i];
                entry.1[i] += f64::from(p.normal[i]);
                entry.2[i] += f64::from(p.colour[i]);
            }
            entry.3 += 1.0;
        }
        // (the patches after the keyframe's points, so all of them were held against the same surface)
        for p in points {
            let entry = self.patches.entry(p.point.map(|v| (v / (PATCH * self.cell)).floor() as i32)).or_insert(([0.0; 3], [0.0; 3], 0.0));
            for i in 0..3 {
                entry.0[i] += f64::from(p.point[i]);
                entry.1[i] += f64::from(p.normal[i]);
            }
            entry.2 += 1.0;
            for (fields, width) in self.fields.iter_mut().zip(FIELDS) {
                let entry = fields.entry(p.point.map(|v| (f64::from(v) / (width * f64::from(self.cell))).floor() as i32)).or_insert(([0.0; 3], [0.0; 3], 0.0, 0.0));
                for i in 0..3 {
                    entry.0[i] += f64::from(p.point[i]);
                    entry.1[i] += f64::from(p.normal[i]);
                }
                entry.2 += brightness(p.colour);
                entry.3 += 1.0;
            }
        }
    }

    /// Where a point is gathered: on the surface already there under it, where it is near one.
    fn on_the_surface(&self, p: &DetailPoint) -> [f64; 3] {
        self.gathered_at(p).0
    }

    /// [`Refined::on_the_surface`], and for a point that is on a surface the axis that surface's
    /// normal lies most along.
    fn gathered_at(&self, p: &DetailPoint) -> ([f64; 3], Option<usize>) {
        let at = p.point.map(f64::from);
        match self.plane_at(p) {
            Some((q, n)) => {
                let off: f64 = (0..3).map(|i| n[i] * (at[i] - q[i])).sum();
                if off.abs() < OF_THE_SURFACE {
                    (std::array::from_fn(|i| at[i] - n[i] * off), (0..3).max_by(|a, b| n[*a].abs().total_cmp(&n[*b].abs())))
                } else {
                    (at, None)
                }
            }
            None => (at, None),
        }
    }

    /// The cell a point is gathered in. A surface that lies where two layers of cells meet has its
    /// points now in the one, now in the other, as the plane they are held to moves by a hair
    /// with each keyframe: the same place came to be held by two cells, one over the other. A
    /// point on a surface whose own cell is not there yet joins the cell next to it along the
    /// surface's normal, where one is.
    fn cell_of(&self, p: &DetailPoint) -> [i32; 3] {
        let (on, along) = self.gathered_at(p);
        let key = on.map(|v| (v / f64::from(self.cell)).floor() as i32);
        let Some(axis) = along else { return key };
        if self.cells.contains_key(&key) {
            return key;
        }
        for step in [1, -1] {
            let mut next = key;
            next[axis] += step;
            if self.cells.contains_key(&next) {
                return next;
            }
        }
        key
    }

    /// What the picture says of a keyframe's place along the surface ([`pattern`]), at the
    /// `level`th width: the fields, the widest first, then the cells.
    fn pattern_at(&self, points: &[DetailPoint], level: usize) -> Vec<([f64; 3], [f64; 3], f64, f64)> {
        match self.fields.get(level) {
            Some(fields) => pattern(points, FIELDS[level] * f64::from(self.cell), &|p| p.point.map(f64::from), &|_| true, &|key| {
                // (a field across an edge holds two surfaces, and its brightness is of neither)
                fields.get(&key).filter(|f| f.3 >= PATCH_FEWEST && (f.1[0] * f.1[0] + f.1[1] * f.1[1] + f.1[2] * f.1[2]).sqrt() > 0.9 * f.3).map(|f| (f.0.map(|v| v / f.3), f.1, f.2 / f.3, f.3))
            }),
            None => pattern(points, f64::from(self.cell), &|p| self.on_the_surface(p), &|key| (key[0] + 3 * key[1] + 5 * key[2]).rem_euclid(CELLS_HELD) == 0, &|key| {
                self.cells.get(&key).map(|c| (c.0.map(|v| v / c.3), c.1, (0.299 * c.2[0] + 0.587 * c.2[1] + 0.114 * c.2[2]) / c.3, c.3))
            }),
        }
    }

    /// Moves a keyframe's points onto the detail gathered so far, before they are added: the
    /// tracker places each keyframe a few millimetres and a fraction of a degree off the others,
    /// and points added as they come would lie as sheets beside each other. The small turn and
    /// shift that bring the points nearest the surface already there (each point against the
    /// plane of the patch it lies in) are found and applied, round after round until a round moves them no further. The shape holds a
    /// keyframe only across its surfaces: along a flat table it holds nothing, and a keyframe
    /// left where the tracker put it there smears the table's edges and what is painted on it.
    /// Along the surface the picture holds it: the keyframe's brightness against the detail's,
    /// up the detail's rise ([`pattern`]), over wide fields first and the cells last. The
    /// picture's rise has no part along a normal, so it moves a keyframe only where the shape
    /// leaves it free. A move neither holds (along a table of one paint) is not made. Returns
    /// how far the points moved, metres (their mean), or None where too few of them fall in
    /// cells that hold detail.
    pub fn settle(&self, points: &mut [DetailPoint]) -> Option<Settled> {
        let mut moved: Option<Settled> = None;
        let from: Vec<[f32; 3]> = points.iter().map(|p| p.point).collect();
        for round in 0..SETTLE_MOST_ROUNDS {
            // The move is a turn about the points' own middle and a shift of it: about the
            // world's origin, metres from a scan in a room, a small turn is a large shift, and
            // neither is the size of anything.
            let mut middle = [0.0f64; 3];
            for p in points.iter() {
                for i in 0..3 {
                    middle[i] += f64::from(p.point[i]) / points.len().max(1) as f64;
                }
            }
            // least squares over x = (turn, shift): (p x n, n) . x = -n . (p - q)
            let (mut a, mut b, mut pairs) = ([[0.0f64; 6]; 6], [0.0f64; 6], 0usize);
            for p in points.iter() {
                let Some((q, n)) = self.plane_at(p) else { continue };
                let at = p.point.map(f64::from);
                let off: f64 = (0..3).map(|i| n[i] * (at[i] - q[i])).sum();
                if off.abs() >= 2.0 * OF_THE_SURFACE {
                    continue;
                }
                let at: [f64; 3] = std::array::from_fn(|i| at[i] - middle[i]);
                let row = [at[1] * n[2] - at[2] * n[1], at[2] * n[0] - at[0] * n[2], at[0] * n[1] - at[1] * n[0], n[0], n[1], n[2]];
                for i in 0..6 {
                    for j in 0..6 {
                        a[i][j] += row[i] * row[j];
                    }
                    b[i] -= row[i] * off;
                }
                pairs += 1;
            }
            if pairs < SETTLE_FEWEST {
                return moved;
            }
            // over x again: (p x g, g) . x = how much brighter, both in metres by PATTERN
            for (at, rise, brighter, count) in self.pattern_at(points, round) {
                let g = rise.map(|v| PATTERN * v);
                let at: [f64; 3] = std::array::from_fn(|i| at[i] - middle[i]);
                let row = [at[1] * g[2] - at[2] * g[1], at[2] * g[0] - at[0] * g[2], at[0] * g[1] - at[1] * g[0], g[0], g[1], g[2]];
                for i in 0..6 {
                    for j in 0..6 {
                        a[i][j] += count * row[i] * row[j];
                    }
                    b[i] += count * row[i] * PATTERN * brighter;
                }
            }
            // (a little weight on no move at all: what the surface does not hold stays put)
            for (i, row) in a.iter_mut().enumerate() {
                row[i] += 1e-4 * pairs as f64;
            }
            let Some(x) = solved(a, b) else { return moved };
            let (turn, shift) = ([x[0], x[1], x[2]], [x[3], x[4], x[5]]);
            let size = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            // A round's move is held to what a tracker is off by: a larger one is a step that
            // way, and the next round goes on from there. (It once left the keyframe where the
            // tracker put it: a sheet beside the others.)
            let held = (SETTLE_MOST_TURN / size(turn).max(1e-12)).min(SETTLE_MOST_SHIFT / size(shift).max(1e-12)).min(1.0);
            let (turn, shift) = (turn.map(|v| v * held), shift.map(|v| v * held));
            let mut stepped = 0.0;
            for p in points.iter_mut() {
                let at = p.point.map(f64::from);
                let from: [f64; 3] = std::array::from_fn(|i| at[i] - middle[i]);
                let by = [turn[1] * from[2] - turn[2] * from[1] + shift[0], turn[2] * from[0] - turn[0] * from[2] + shift[1], turn[0] * from[1] - turn[1] * from[0] + shift[2]];
                p.point = std::array::from_fn(|i| (at[i] + by[i]) as f32);
                stepped += size(by);
                let n = p.normal.map(f64::from);
                p.normal = [(n[0] + turn[1] * n[2] - turn[2] * n[1]) as f32, (n[1] + turn[2] * n[0] - turn[0] * n[2]) as f32, (n[2] + turn[0] * n[1] - turn[1] * n[0]) as f32];
            }
            // (how far from where they came: the rounds' moves partly undo each other)
            let total: f64 = points.iter().zip(&from).map(|(p, f)| size(std::array::from_fn(|i| f64::from(p.point[i] - f[i])))).sum();
            // (as a turn about the world's origin and a shift, which is how the moves add up)
            let shift = [shift[0] - (turn[1] * middle[2] - turn[2] * middle[1]), shift[1] - (turn[2] * middle[0] - turn[0] * middle[2]), shift[2] - (turn[0] * middle[1] - turn[1] * middle[0])];
            let so_far = moved.unwrap_or_default();
            moved = Some(Settled {
                by: (total / points.len().max(1) as f64) as f32,
                turn: std::array::from_fn(|i| so_far.turn[i] + turn[i]),
                shift: std::array::from_fn(|i| so_far.shift[i] + shift[i]),
            });
            // (the picture is held against wide fields, narrower ones, then the cells: once the
            // cells' round moves the points by next to nothing, the keyframe is on)
            if round > FIELDS.len() && stepped / (points.len().max(1) as f64) < SETTLED_WITHIN {
                break;
            }
        }
        moved
    }

    /// What a keyframe sees through. Where a nearer surface ends and a farther one shows behind
    /// it (the far edge of a step, seen across the step), the matching gives the farther
    /// surface's pixels next to the edge the nearer one's depth: a keyframe that looks across a
    /// step leaves a sheet of points over the lower surface beyond it, and both its pictures
    /// agree on them. The keyframes that see that place from outside or from the side see the
    /// lower surface there, through the sheet. So each keyframe says what it sees through: for
    /// each of its points, the cells on the way from `eye` (where its camera stood, as the
    /// keyframe was settled) that lie over the keyframe's own surface there by more than the
    /// matching scatters ([`SEEN_OVER`], across the surface: a floor seen at a slant is not seen
    /// through), up to [`SEEN_REACH`] cells before the point. A keyframe counts once a cell.
    /// A cell more keyframes saw through than hold it, two at least, is not of the surface, and
    /// is not given ([`Refined::points`]); it stays, and counts again once more keyframes hold it.
    ///
    /// Per keyframe: a few lookups a point, on whoever gathers (the detail worker).
    pub fn carve(&mut self, eye: [f64; 3], points: &[DetailPoint]) {
        let cell = f64::from(self.cell);
        let mut seen: HashSet<[i32; 3]> = HashSet::new();
        for p in points {
            let at = p.point.map(f64::from);
            let to: [f64; 3] = std::array::from_fn(|i| at[i] - eye[i]);
            let range = (to[0] * to[0] + to[1] * to[1] + to[2] * to[2]).sqrt();
            if range < 1e-6 {
                continue;
            }
            let n = p.normal.map(f64::from);
            for step in 1..=SEEN_REACH {
                let back = step as f64 * cell / range;
                let on_the_way: [f64; 3] = std::array::from_fn(|i| at[i] - to[i] * back);
                let key: [i32; 3] = on_the_way.map(|v| (v / cell).floor() as i32);
                let Some((sum, _, _, count, _)) = self.cells.get(&key) else { continue };
                let over: f64 = (0..3).map(|i| n[i] * (sum[i] / count - at[i])).sum();
                // (a cell is named by where its points were gathered, on the surface under them; it
                // is seen through only where its points themselves are on the way)
                let off: f64 = (0..3).map(|i| (sum[i] / count - on_the_way[i]).powi(2)).sum::<f64>().sqrt();
                if over.abs() > SEEN_OVER && off < cell {
                    seen.insert(key);
                }
            }
        }
        for key in seen {
            *self.through.entry(key).or_insert(0.0) += 1.0;
        }
    }

    /// Is a cell seen through: by two keyframes at least, and by more than hold it.
    fn seen_through(&self, key: &[i32; 3], held: f64) -> bool {
        self.through.get(key).is_some_and(|through| *through >= 2.0 && *through > held)
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The points, in the cells' order, where the keyframes put the surface between them: moved
    /// back from the first keyframe's place by the mean of the moves that settled the others
    /// onto it.
    pub fn points(&self) -> Vec<DetailPoint> {
        let (turn, shift) = (self.turned.map(|v| -v / self.keyframes.max(1.0)), self.shifted.map(|v| -v / self.keyframes.max(1.0)));
        self.cells
            .iter()
            .filter(|(key, cell)| !self.seen_through(key, cell.3))
            .map(|(_, (sum, normal, colour, count, size))| {
                let at = sum.map(|v| v / count);
                let sum = [at[0] + turn[1] * at[2] - turn[2] * at[1] + shift[0], at[1] + turn[2] * at[0] - turn[0] * at[2] + shift[1], at[2] + turn[0] * at[1] - turn[1] * at[0] + shift[2]].map(|v| v * count);
                let normal = &[normal[0] + turn[1] * normal[2] - turn[2] * normal[1], normal[1] + turn[2] * normal[0] - turn[0] * normal[2], normal[2] + turn[0] * normal[1] - turn[1] * normal[0]];
                let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
                DetailPoint { point: sum.map(|v| (v / count) as f32), normal: if length > 1e-9 { normal.map(|v| (v / length) as f32) } else { [0.0, 1.0, 0.0] }, colour: colour.map(|v| (v / count) as f32), size: *size }
            })
            .collect()
    }

    /// The base's cells (`base_cell` wide) the detail has a point in. The detail stands in for
    /// those and for the ones around them (the base lies up to a cell off the surface): whoever
    /// leaves base points out looks a cell around ([`stands_in`]).
    pub fn covered(&self, base_cell: f32) -> HashSet<[i32; 3]> {
        self.cells.values().map(|(sum, _, _, count, _)| std::array::from_fn(|i| ((sum[i] / count) as f32 / base_cell).floor() as i32)).collect()
    }
}

/// A keyframe's points as they were added, and the cells they went in ([`Refined::add_kept`]).
#[derive(Clone)]
pub struct Kept {
    points: Vec<DetailPoint>,
    cells: Vec<[i32; 3]>,
    settled: Option<Settled>,
}

impl Kept {
    /// How many bytes [`Kept::write`] makes of it.
    pub fn bytes(&self) -> usize {
        64 + 52 * self.points.len()
    }

    /// Written to a file: a fast walk gives hundreds of hurried keyframes, megabytes each, and
    /// they wait on disk for their second match, not in memory.
    pub fn write(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut out = Vec::with_capacity(self.bytes());
        out.extend((self.points.len() as u64).to_le_bytes());
        let settled = self.settled.unwrap_or_default();
        out.extend(u32::from(self.settled.is_some()).to_le_bytes());
        out.extend(settled.by.to_le_bytes());
        for v in settled.turn.iter().chain(&settled.shift) {
            out.extend(v.to_le_bytes());
        }
        for (p, cell) in self.points.iter().zip(&self.cells) {
            for v in p.point.iter().chain(&p.normal).chain(&p.colour).chain([&p.size]) {
                out.extend(v.to_le_bytes());
            }
            for v in cell {
                out.extend(v.to_le_bytes());
            }
        }
        std::fs::write(path, out)
    }

    /// Read back from a file [`Kept::write`] made.
    pub fn read(path: &std::path::Path) -> std::io::Result<Kept> {
        let bytes = std::fs::read(path)?;
        let short = || std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "a kept keyframe cut short");
        let word = |at: usize| bytes.get(at..at + 4).map(|b| [b[0], b[1], b[2], b[3]]).ok_or_else(short);
        let long = |at: usize| bytes.get(at..at + 8).map(|b| [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]).ok_or_else(short);
        let count = u64::from_le_bytes(long(0)?) as usize;
        if bytes.len() != 64usize.saturating_add(count.saturating_mul(52)) {
            return Err(short());
        }
        let mut six = [0.0f64; 6];
        for (i, v) in six.iter_mut().enumerate() {
            *v = f64::from_le_bytes(long(16 + 8 * i)?);
        }
        let settled = (u32::from_le_bytes(word(8)?) == 1).then_some(Settled { by: f32::from_le_bytes(word(12)?), turn: [six[0], six[1], six[2]], shift: [six[3], six[4], six[5]] });
        let (mut points, mut cells) = (Vec::with_capacity(count), Vec::with_capacity(count));
        for i in 0..count {
            let at = 64 + 52 * i;
            let mut f = [0.0f32; 10];
            for (k, v) in f.iter_mut().enumerate() {
                *v = f32::from_le_bytes(word(at + 4 * k)?);
            }
            points.push(DetailPoint { point: [f[0], f[1], f[2]], normal: [f[3], f[4], f[5]], colour: [f[6], f[7], f[8]], size: f[9] });
            cells.push([i32::from_le_bytes(word(at + 40)?), i32::from_le_bytes(word(at + 44)?), i32::from_le_bytes(word(at + 48)?)]);
        }
        Ok(Kept { points, cells, settled })
    }
}

/// A keyframe matched in a hurry (at half the size, others waiting): what it was matched from
/// and what it added, kept so it can be matched again at the full size once nothing waits.
pub struct Hurried {
    pub keyframe: Arc<CaptureKeyframe>,
    pub cameras: Arc<Vec<CaptureCamera>>,
    pub place: Option<[f64; 3]>,
    pub kept: Kept,
}

/// Matches a hurried keyframe again, unhurried, and puts what that gives in the place of what
/// it added before. Nothing is taken out until the new match is there: a match that is given
/// up (the matcher's "cancelled") leaves the detail as it was. `base` is the base surface as
/// it stands now (it has grown since the keyframe came). Returns how the new points settled.
pub fn rematch(matcher: &dyn Matcher, refined: &mut Refined, hurried: &Hurried, base: &[([f32; 3], f32)], cell: f32) -> Result<Option<Settled>, String> {
    let (mut points, _) = refine_looked(matcher, &hurried.keyframe, &hurried.cameras, base, cell, hurried.place, false)?;
    refined.take_out(&hurried.kept);
    let settled = refined.settle(&mut points);
    refined.add_settled(&points, settled.as_ref());
    Ok(settled)
}

/// How a keyframe's points were moved onto the detail: how far (their mean, metres), and the
/// small turn (radians about each axis) and shift (metres) that did it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Settled {
    pub by: f32,
    pub turn: [f64; 3],
    pub shift: [f64; 3],
}

impl Settled {
    /// Where the settling took a place of the keyframe's (its camera, say).
    pub fn moved(&self, at: [f64; 3]) -> [f64; 3] {
        let (t, s) = (self.turn, self.shift);
        [at[0] + t[1] * at[2] - t[2] * at[1] + s[0], at[1] + t[2] * at[0] - t[0] * at[2] + s[1], at[2] + t[0] * at[1] - t[1] * at[0] + s[2]]
    }
}

/// The fewest of a keyframe's points in cells that hold detail for it to be settled on them.
const SETTLE_FEWEST: usize = 500;
/// The most a keyframe is turned (radians, about its points' middle) and shifted (metres, there)
/// onto the detail in one round: a tracker's error.
const SETTLE_MOST_TURN: f64 = 0.02;
const SETTLE_MOST_SHIFT: f64 = 0.015;

/// How far over a keyframe's surface (across it, metres) a cell lies for the keyframe to see
/// through it: several times what its matching scatters by, under what a matched edge bleeds.
const SEEN_OVER: f64 = 0.0015;
/// How many cells before each of its points a keyframe looks for what it sees through: what the
/// matching bleeds past an edge lies within its window of the edge.
const SEEN_REACH: usize = 6;

/// A keyframe is on the detail once a round moves its points by less than this, metres (their
/// mean): a thirtieth of a cell.
const SETTLED_WITHIN: f64 = 0.00005;
/// The rounds a keyframe is given to get there. Most are on in four; one the tracker put far off
/// takes more, and one that is not there after these stays as near as it got.
const SETTLE_MOST_ROUNDS: usize = 12;

/// The x of a x = b, by elimination with the largest pivot; None where a has no inverse.
fn solved(mut a: [[f64; 6]; 6], mut b: [f64; 6]) -> Option<[f64; 6]> {
    for i in 0..6 {
        let pivot = (i..6).max_by(|&p, &q| a[p][i].abs().total_cmp(&a[q][i].abs()))?;
        if a[pivot][i].abs() < 1e-12 {
            return None;
        }
        a.swap(i, pivot);
        b.swap(i, pivot);
        for r in i + 1..6 {
            let f = a[r][i] / a[i][i];
            for c in i..6 {
                a[r][c] -= f * a[i][c];
            }
            b[r] -= f * b[i];
        }
    }
    let mut x = [0.0; 6];
    for i in (0..6).rev() {
        x[i] = (b[i] - (i + 1..6).map(|c| a[i][c] * x[c]).sum::<f64>()) / a[i][i];
    }
    Some(x)
}

/// The detail stands in for the base at `cell`: it has a point there or in a cell next to it.
pub fn stands_in(covered: &HashSet<[i32; 3]>, cell: [i32; 3]) -> bool {
    (-1..=1).any(|dx| (-1..=1).any(|dy| (-1..=1).any(|dz| covered.contains(&[cell[0] + dx, cell[1] + dy, cell[2] + dz]))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_is_seen_through_a_camera_and_detail_gathers_a_point_a_cell() {
        // a camera at the origin looking along z; a wall of base points at z = 0.5, 10 mm apart
        let view = GrayView { width: 64, height: 48, pixels: Vec::new(), k: [60.0, 60.0, 31.5, 23.5], cam_to_world: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]] };
        let wall: Vec<([f32; 3], f32)> = (-20..=20).flat_map(|x| (-20..=20).map(move |y| ([x as f32 * 0.01, y as f32 * 0.01, 0.5], 0.01))).collect();
        let seen = BaseSeen::of(&view, &wall, 4);
        assert!((seen.along([0.0, 0.0, 1.0]).unwrap() - 0.5).abs() < 1e-6, "straight ahead the wall is at its depth");
        let slanted = [0.3_f64, 0.0, 0.953_939_2];
        assert!((seen.along(slanted).unwrap() - 0.5 / 0.953_939_2).abs() < 0.01, "along a slanted ray it is farther by the slant: {:?}", seen.along(slanted));
        assert!(seen.along([0.0, 0.0, -1.0]).is_none() && seen.along([0.99, 0.0, 0.14]).is_none(), "behind the camera and outside the picture there is none");
        // a nearer point hides the wall behind it, and nothing is seen where no point lies
        let mut nearer = wall.clone();
        nearer.push(([0.0, 0.0, 0.3], 0.01));
        assert!((BaseSeen::of(&view, &nearer, 4).along([0.0, 0.0, 1.0]).unwrap() - 0.3).abs() < 1e-6, "the nearest point through a pixel is the one seen");
        assert!(BaseSeen::of(&view, &wall[..1], 4).along([0.0, 0.0, 1.0]).is_none(), "no point, no base");
        // detail: two keyframes' points in one cell are one point at their mean; the cells are in order
        let mut refined = Refined::new(0.004);
        let up = [0.0, 1.0, 0.0];
        refined.add(&[DetailPoint { point: [0.101, 0.0, 0.5], normal: up, colour: [0.2, 0.4, 0.6], size: 0.006 }, DetailPoint { point: [0.3, 0.0, 0.5], normal: up, colour: [1.0; 3], size: 0.004 }]);
        refined.add(&[DetailPoint { point: [0.103, 0.0, 0.5], normal: up, colour: [0.4, 0.4, 0.2], size: 0.004 }]);
        let points = refined.points();
        assert_eq!(points.len(), 2, "one point a cell");
        assert_eq!(points[0].size, 0.004, "a cell's square is the narrowest its points stood for");
        assert!((points[0].point[0] - 0.102).abs() < 1e-6 && points[1].point[0] == 0.3, "the mean of a cell's points, cells in order: {points:?}");
        assert!((0..3).all(|c| (points[0].colour[c] - [0.3, 0.4, 0.4][c]).abs() < 1e-6) && points[1].colour == [1.0; 3], "the mean of a cell's colours: {points:?}");
        // a keyframe the tracker placed 3 mm off settles onto the detail already there: a floor
        // and a wall, 2 mm apart, then the same points pushed off both
        let mut gathered = Refined::new(0.004);
        let corner: Vec<DetailPoint> = (0..60)
            .flat_map(|i| (0..60).map(move |j| (i as f32 * 0.002, j as f32 * 0.002)))
            .flat_map(|(u, v)| [DetailPoint { point: [u, 0.0, v], normal: up, colour: [0.5; 3], size: 0.004 }, DetailPoint { point: [0.0, u + 0.002, v], normal: [1.0, 0.0, 0.0], colour: [0.5; 3], size: 0.004 }])
            .collect();
        gathered.add(&corner);
        let mut off: Vec<DetailPoint> = corner.iter().map(|p| DetailPoint { point: [p.point[0] + 0.003, p.point[1] + 0.002, p.point[2]], ..*p }).collect();
        let moved = gathered.settle(&mut off).unwrap().by;
        let worst = off.iter().zip(&corner).map(|(a, b)| (a.point[0] - b.point[0]).abs().max((a.point[1] - b.point[1]).abs())).fold(0.0, f32::max);
        assert!(moved > 0.002 && worst < 0.0006, "a keyframe 3.6 mm off the detail is brought onto it: moved {moved}, still {worst} off");
        let slid = off.iter().zip(&corner).map(|(a, b)| (a.point[2] - b.point[2]).abs()).fold(0.0, f32::max);
        assert!(slid < 0.0005, "along the corner's edge, where nothing holds it, it is not moved: {slid}");
        assert!(Refined::new(0.004).settle(&mut off).is_none(), "with no detail yet a keyframe stays where the tracker put it");
        // a keyframe turned more than one round's move allows is brought on in steps, not left where
        // the tracker put it (it was once given up, and lay as a sheet beside the others); and
        // metres from the world's origin, where a turn about the origin is a shift of centimetres
        let far = |p: &DetailPoint| DetailPoint { point: [p.point[0] + 3.0, p.point[1], p.point[2] + 2.0], ..*p };
        let mut room = Refined::new(0.004);
        room.add(&corner.iter().map(far).collect::<Vec<_>>());
        let mut off_there: Vec<DetailPoint> = corner.iter().map(|p| far(&DetailPoint { point: [p.point[0] + 0.003 + 0.025 * p.point[2], p.point[1] + 0.002, p.point[2] - 0.025 * p.point[0]], ..*p })).collect();
        let brought = room.settle(&mut off_there);
        let worst_there = off_there.iter().zip(&corner).map(|(a, b)| (a.point[0] - 3.0 - b.point[0]).abs().max((a.point[1] - b.point[1]).abs())).fold(0.0, f32::max);
        assert!(brought.is_some() && worst_there < 0.0008, "a keyframe a few millimetres and 25 mrad off, 3.6 m from the origin, is brought onto the detail: {brought:?}, still {worst_there} off");
        // along a flat floor the shape holds nothing, and what is painted on it does: a painted
        // floor slid 3 mm one way and 2 mm the other along itself is brought back
        let painted: Vec<DetailPoint> = (0..160)
            .flat_map(|i| (0..160).map(move |j| (i as f32 * 0.002, j as f32 * 0.002)))
            .map(|(x, z)| DetailPoint { point: [x, 0.0, z], normal: up, colour: [0.5 + 0.2 * (25.0 * x).sin() + 0.2 * (21.0 * z + 1.0).sin(); 3], size: 0.004 })
            .collect();
        let mut floor = Refined::new(0.004);
        floor.add(&painted);
        let mut slid: Vec<DetailPoint> = painted.iter().map(|p| DetailPoint { point: [p.point[0] + 0.003, p.point[1], p.point[2] - 0.002], ..*p }).collect();
        floor.settle(&mut slid).unwrap();
        let still = slid.iter().zip(&painted).map(|(a, b)| (a.point[0] - b.point[0]).abs().max((a.point[2] - b.point[2]).abs())).fold(0.0, f32::max);
        assert!(still < 0.0007, "a keyframe slid 3.6 mm along a painted floor is brought back by the paint: still {still} off");
        // slid and turned 12 mrad as well, it takes more rounds than most keyframes do, and gets them
        let mut turned: Vec<DetailPoint> = painted.iter().map(|p| DetailPoint { point: [p.point[0] + 0.003 + 0.012 * (p.point[2] - 0.16), p.point[1], p.point[2] - 0.002 - 0.012 * (p.point[0] - 0.16)], ..*p }).collect();
        floor.settle(&mut turned).unwrap();
        let still = turned.iter().zip(&painted).map(|(a, b)| (a.point[0] - b.point[0]).abs().max((a.point[2] - b.point[2]).abs())).fold(0.0, f32::max);
        assert!(still < 0.0004, "a keyframe slid 3.6 mm and turned 12 mrad along a painted floor is brought back: still {still} off");
        // a surface that lies just under where two patches meet: points a little off it (a mismatch
        // at an edge, 2 mm up) are in the patch above, and still join the surface's cells; they once
        // stood in cells of their own over it, a second layer, wherever the scan sat that way
        let mut under = Refined::new(0.004);
        let level: Vec<DetailPoint> = (0..40).flat_map(|i| (0..40).map(move |j| DetailPoint { point: [i as f32 * 0.002, 0.0155, j as f32 * 0.002], normal: up, colour: [0.5; 3], size: 0.004 })).collect();
        under.add(&level);
        under.add(&level);
        let cells = under.len();
        let lifted: Vec<DetailPoint> = (0..30).map(|i| DetailPoint { point: [0.011 + i as f32 * 0.0022, 0.0175, 0.013 + i as f32 * 0.0017], normal: up, colour: [0.5; 3], size: 0.004 }).collect();
        under.add(&lifted);
        assert!(under.len() == cells, "points 2 mm over a surface that lies under a patch's top made a second layer: {cells} cells to {}", under.len());
        // a floor that lies where two layers of cells meet, looked at three times with a hair's
        // scatter: one cell a place, not one over the other
        let mut between = Refined::new(0.0015);
        for look in 0..3 {
            let scattered: Vec<DetailPoint> = (0..40)
                .flat_map(|i| (0..40).map(move |j| (i, j)))
                .map(|(i, j)| DetailPoint { point: [i as f32 * 0.0015 + 0.0007, 0.003 + 0.0002 * (((i * 7 + j * 13 + look * 5) % 5) as f32 - 2.0), j as f32 * 0.0015 + 0.0007], normal: up, colour: [0.5; 3], size: 0.0015 })
                .collect();
            between.add(&scattered);
        }
        let places: HashSet<(i32, i32)> = between.points().iter().map(|p| ((p.point[0] / 0.0015) as i32, (p.point[2] / 0.0015) as i32)).collect();
        assert!(between.len() <= places.len() + places.len() / 20, "a floor where two layers of cells meet is held twice over: {} cells for {} places", between.len(), places.len());
        // what keyframes see through goes: a sheet 3 mm over a floor that two keyframes left there
        // (looking across a step), and three keyframes standing over the floor see the floor under
        // it; a rib two keyframes hold and none sees through stays; so does what one alone sees through
        let mut carved = Refined::new(0.0015);
        let at = |x: f32, y: f32, z: f32| DetailPoint { point: [x, y, z], normal: up, colour: [0.5; 3], size: 0.0015 };
        let ground: Vec<DetailPoint> = (0..40).flat_map(|i| (0..40).map(move |j| at(i as f32 * 0.0015 + 0.0007, 0.0052, j as f32 * 0.0015 + 0.0007))).collect();
        let sheet: Vec<DetailPoint> = (10..20).flat_map(|i| (10..20).map(move |j| at(i as f32 * 0.0015 + 0.0007, 0.0082, j as f32 * 0.0015 + 0.0007))).collect();
        let rib: Vec<DetailPoint> = (25..30).flat_map(|i| (10..20).map(move |j| at(i as f32 * 0.0015 + 0.0007, 0.0072, j as f32 * 0.0015 + 0.0007))).collect();
        let lone: Vec<DetailPoint> = (32..36).flat_map(|i| (16..20).map(move |j| at(i as f32 * 0.0015 + 0.0007, 0.0082, j as f32 * 0.0015 + 0.0007))).collect();
        // (the ground as a keyframe over the rib sees it: not under the rib)
        let beside_rib: Vec<DetailPoint> = ground.iter().copied().filter(|p| !(0.0375..0.045).contains(&p.point[0]) || !(0.015..0.030).contains(&p.point[2])).collect();
        // (the floor lies just under a patch's top: what is 2 and 3 mm over it is in the patch above, a layer of its own)
        for _ in 0..4 {
            carved.add(&beside_rib);
        }
        for _ in 0..2 {
            carved.add(&sheet);
            carved.add(&rib);
        }
        carved.add(&lone);
        carved.add(&lone);
        for (n, eye) in [[0.03, 0.4, 0.03], [0.02, 0.4, 0.04], [0.04, 0.4, 0.02]].into_iter().enumerate() {
            carved.add(&beside_rib);
            // (the third keyframe alone sees through the lone points)
            let looked: Vec<DetailPoint> = beside_rib.iter().copied().filter(|p| n == 2 || p.point[0] < 0.048).collect();
            carved.carve(eye, &looked);
        }
        let given = carved.points();
        let over = |low: f32, high: f32| given.iter().filter(|p| p.point[1] > low && p.point[1] < high).count();
        assert!(over(0.0077, 0.0087) == lone.len(), "a sheet two keyframes left over a floor three keyframes see goes, and what one keyframe alone sees through stays: {} points 3 mm up, {} of them the lone ones", over(0.0077, 0.0087), lone.len());
        let rib_stands: HashSet<(i32, i32)> = given.iter().filter(|p| p.point[1] > 0.0067 && p.point[1] < 0.0077).map(|p| ((p.point[0] / 0.0015) as i32, (p.point[2] / 0.0015) as i32)).collect();
        assert!(rib_stands.len() == rib.len(), "a rib two keyframes hold and none sees through stays, every place of it: {} of {}", rib_stands.len(), rib.len());
        assert!(over(0.0045, 0.006) == beside_rib.len(), "the floor itself is seen through by none: {} of {}", over(0.0045, 0.006), beside_rib.len());
        // a second look at the corner whose heights came out 2 mm off: no second layer, the mean
        let before = gathered.len();
        let lifted: Vec<DetailPoint> = corner.iter().filter(|p| p.normal == up && p.point[0] > 0.02).map(|p| DetailPoint { point: [p.point[0], 0.002, p.point[2]], ..*p }).collect();
        gathered.add(&lifted);
        assert!(gathered.len() <= before + before / 50, "a second look 2 mm off made a second layer: {before} to {}", gathered.len());
        let heights: Vec<f32> = gathered.points().iter().filter(|p| p.normal[1] > 0.9 && p.point[0] > 0.03 && p.point[0] < 0.1).map(|p| p.point[1]).collect();
        assert!(heights.iter().all(|y| (y - 0.001).abs() < 0.0003), "two looks' heights average in one sheet");
        // a keyframe taken out again leaves the detail as it was before it
        let (cells, first) = (gathered.len(), gathered.points()[0]);
        let again = gathered.add_kept(&off, Some(Settled { by: 0.001, turn: [0.0; 3], shift: [0.001, 0.0, 0.0] }));
        assert!(gathered.points()[0] != first, "a keyframe added moves the detail");
        // (through a file and back, as a fast walk's keyframes wait for their second match)
        let file = std::env::temp_dir().join(format!("fab-scan-kept-test-{}.bin", std::process::id()));
        again.write(&file).unwrap();
        let again = Kept::read(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        gathered.take_out(&again);
        let back = gathered.points()[0];
        assert!(gathered.len() == cells && (0..3).all(|i| (back.point[i] - first.point[i]).abs() < 1e-6), "a keyframe taken out again leaves the detail as it was: {cells} cells to {}, {first:?} to {back:?}", gathered.len());
        // the detail is given where the keyframes put the surface between them: one keyframe
        // settled 2 mm down onto the first gives both a millimetre up from where the first lay
        let mut between = Refined::new(0.004);
        let floor: Vec<DetailPoint> = (0..40).flat_map(|i| (0..40).map(move |j| DetailPoint { point: [i as f32 * 0.002, 0.0, j as f32 * 0.002], normal: up, colour: [0.5; 3], size: 0.004 })).collect();
        between.add(&floor);
        between.add_settled(&floor, Some(&Settled { by: 0.002, turn: [0.0; 3], shift: [0.0, -0.002, 0.0] }));
        assert!(between.points().iter().all(|p| (p.point[1] - 0.001).abs() < 1e-5), "the first keyframe's place is not the scan's: the mean of the keyframes' is");
        let covered = refined.covered(0.01);
        assert!(covered.len() == 2 && covered.contains(&[10, 0, 50]), "the base's cells the detail has a point in: {covered:?}");
        assert!(stands_in(&covered, [10, 0, 50]) && stands_in(&covered, [11, 1, 49]) && !stands_in(&covered, [13, 0, 50]), "the detail stands in for the base at its cells and next to them, not farther");
    }
}

//! Dense patches from one keyframe's pictures: PatchMatch on the pair of cameras inside the keyframe,
//! which are rigidly calibrated, so the stereo does not depend on the tracker's noise. The coarse surface
//! (the headset's depth) starts the search and rejects what lies far from it.

use crate::colour::Picture;
use crate::session::session_view;
use makepad_ai_geometry::{
    fusion::{fuse_view, ConsistencyParams, FusedMap},
    patchmatch::{DepthNormalMap, PatchMatchParams},
    program::patchmatch_program,
    views::GrayView,
};
use makepad_compute_program::gpu::GpuLink;
use makepad_desk_link::xr_capture::{CaptureCamera, CaptureKeyframe};
use std::collections::BTreeMap;

/// The depth range of the search, in metres.
pub const DEPTH_MIN: f64 = 0.15;
pub const DEPTH_MAX: f64 = 2.0;
/// The most a kept pixel's matching cost may be.
pub const MAX_COST: f64 = 0.3;
/// How far from the coarse surface a kept pixel may lie, in metres.
pub const NEAR_COARSE: f64 = 0.05;
/// The most slanted look the allowance is widened for (the slant's cosine).
const MOST_SLANT: f64 = 0.1;

/// The coarse surface: from a camera centre along a unit ray, both in the world, the distance to it.
pub type Coarse<'a> = &'a dyn Fn([f64; 3], [f64; 3]) -> Option<f64>;

/// The keyframe's views, each `shrink` times smaller a side.
pub fn pair_of(keyframe: &CaptureKeyframe, cameras: &[CaptureCamera], shrink: usize) -> Result<Vec<GrayView>, String> {
    let mut views = Vec::new();
    for view in &keyframe.views {
        let camera = cameras.get(view.camera as usize).ok_or("a picture of a camera not listed")?;
        views.push(session_view(view, camera, keyframe.index as usize)?.view.downsampled(shrink)?);
    }
    if views.len() < 2 {
        return Err("a keyframe with fewer than two pictures".into());
    }
    Ok(views)
}

/// The keyframe's pictures in colour, each `shrink` times smaller a side (view for view with [`pair_of`]'s).
pub fn pictures_of(keyframe: &CaptureKeyframe, cameras: &[CaptureCamera], shrink: usize) -> Result<Vec<Picture>, String> {
    keyframe
        .views
        .iter()
        .map(|view| {
            let camera = cameras.get(view.camera as usize).ok_or("a picture of a camera not listed")?;
            Picture::of(view, camera)?.downsampled(shrink)
        })
        .collect()
}

fn centre_of(view: &GrayView) -> [f64; 3] {
    std::array::from_fn(|i| view.cam_to_world[i][3])
}

/// The unit ray through a pixel in the camera, and in the world.
fn rays_of(view: &GrayView, x: usize, y: usize) -> ([f64; 3], [f64; 3]) {
    let r = [(x as f64 - view.k[2]) / view.k[0], (y as f64 - view.k[3]) / view.k[1], 1.0];
    let length = (r[0] * r[0] + r[1] * r[1] + 1.0).sqrt();
    let local = r.map(|v| v / length);
    (local, rotated(view, local))
}

fn rotated(view: &GrayView, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| (0..3).map(|j| view.cam_to_world[i][j] * v[j]).sum())
}

/// A starting map from the coarse surface; None when under 60% of the pixels get a depth.
pub fn prior_of(view: &GrayView, coarse: Coarse) -> Option<DepthNormalMap> {
    let centre = centre_of(view);
    let count = view.width * view.height;
    let mut depth = Vec::with_capacity(count);
    let mut normal = Vec::with_capacity(count);
    let mut known = Vec::new();
    for y in 0..view.height {
        for x in 0..view.width {
            let (local, world) = rays_of(view, x, y);
            let d = coarse(centre, world).filter(|d| d.is_finite()).map(|d| (d * local[2]).clamp(DEPTH_MIN, DEPTH_MAX));
            if let Some(d) = d {
                known.push(d);
            }
            depth.push(d);
            normal.push(local.map(|v| -v));
        }
    }
    if (known.len() as f64) < 0.6 * count as f64 {
        return None;
    }
    known.sort_by(f64::total_cmp);
    let median = known[known.len() / 2];
    Some(DepthNormalMap {
        width: view.width,
        height: view.height,
        depth: depth.into_iter().map(|d| d.unwrap_or(median)).collect(),
        normal,
        cost: vec![2.0; count],
    })
}

/// A point of the surface in the world, and the way it faces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetailPoint {
    pub point: [f32; 3],
    pub normal: [f32; 3],
    /// The colour of the pixel the point is from (the mean of them where several are in one cube), 0..1.
    pub colour: [f32; 3],
    /// The width of the square it stands for, metres: its cube's, or what a pixel covers there
    /// where that is wider (a far surface: its points lie farther apart than the cubes, and
    /// squares of a cube's width would leave gaps between them).
    pub size: f32,
}

/// How many pixels each way a point's neighbours are taken from for its normal.
const NORMAL_REACH: usize = 2;

/// The surface's normal at a pixel from the points around it: across the two differences of its
/// neighbours [`NORMAL_REACH`] pixels left and right, up and down, facing the camera (`ray`: from
/// the camera to the point, unit). None at the picture's edge, where a neighbour has no point, or
/// where a neighbour lies more than `jump` from the point (a depth edge: the neighbours are on
/// another surface). The matching's own plane at a pixel is one pixel's guess and scatters; the
/// points around it are what the two pictures agreed on.
fn across(fused: &FusedMap, x: usize, y: usize, ray: [f64; 3], jump: f64) -> Option<[f64; 3]> {
    let r = NORMAL_REACH;
    if x < r || y < r || x + r >= fused.width || y + r >= fused.height {
        return None;
    }
    let at = |x: usize, y: usize| fused.points[y * fused.width + x];
    let here = at(x, y)?;
    let near = |p: [f64; 3]| ((p[0] - here[0]).powi(2) + (p[1] - here[1]).powi(2) + (p[2] - here[2]).powi(2)).sqrt() <= jump;
    let (left, right, up, down) = (at(x - r, y)?, at(x + r, y)?, at(x, y - r)?, at(x, y + r)?);
    if ![left, right, up, down].into_iter().all(near) {
        return None;
    }
    let (a, b): ([f64; 3], [f64; 3]) = (std::array::from_fn(|k| right[k] - left[k]), std::array::from_fn(|k| down[k] - up[k]));
    let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if length < 1e-18 {
        return None;
    }
    let facing = if n[0] * ray[0] + n[1] * ray[1] + n[2] * ray[2] > 0.0 { -1.0 } else { 1.0 };
    Some(n.map(|v| facing * v / length))
}

/// Is a matched point of the base surface: within [`NEAR_COARSE`] of it, across the surface. The
/// base is known along the ray, and a surface seen at a slant lies farther along the ray for the
/// same distance across it (and the base, cells seen through a coarse picture of them, is that
/// much less sure along it): the allowance along the ray is the one across, over the slant's
/// cosine. Held to the one length along every ray, a floor seen at a slant lost three points in
/// four, in bands. No base along the ray: nothing to be far from.
fn near_base(point: [f64; 3], centre: [f64; 3], ray: [f64; 3], normal: [f64; 3], coarse: Coarse) -> bool {
    let Some(near) = coarse(centre, ray) else { return true };
    let to = [point[0] - centre[0], point[1] - centre[1], point[2] - centre[2]];
    let away = (to[0] * to[0] + to[1] * to[1] + to[2] * to[2]).sqrt();
    let facing = (normal[0] * ray[0] + normal[1] * ray[1] + normal[2] * ray[2]).abs();
    (away - near).abs() <= NEAR_COARSE / facing.max(MOST_SLANT)
}

/// The pixels the two pictures agree on as world points (`fused`: view 0's pixels kept by the round trip through the
/// other view), one per `cell`-wide cube, in the order of the cubes' keys, in the colour of view 0's pixels
/// (`picture`: view 0 in colour, the same size; a pixel that was matched is one that is seen).
pub fn points_of(view: &GrayView, picture: &Picture, map: &DepthNormalMap, fused: &FusedMap, coarse: Coarse, cell: f32) -> Vec<DetailPoint> {
    let centre = centre_of(view);
    let cell = f64::from(cell);
    // Per cube: the sum of the points, of the normals and of the colours, the count.
    let mut cubes: BTreeMap<[i64; 3], ([f64; 3], [f64; 3], [f64; 3], f64, f64)> = BTreeMap::new();
    for y in 0..map.height.min(view.height) {
        for x in 0..map.width.min(view.width) {
            let i = y * map.width + x;
            let Some(point) = fused.points[i] else { continue };
            if !(map.cost[i] <= MAX_COST) || !(DEPTH_MIN..=DEPTH_MAX).contains(&map.depth[i]) {
                continue;
            }
            let ray = rays_of(view, x, y).1;
            // what the pixel covers on a surface facing the camera: its width at that depth
            let covers = map.depth[i] / view.k[0];
            let normal = across(fused, x, y, ray, 4.0 * NORMAL_REACH as f64 * covers).unwrap_or_else(|| rotated(view, map.normal[i]));
            if !near_base(point, centre, ray, normal, coarse) {
                continue;
            }
            let Some(colour) = picture.pixel(x, y) else { continue };
            let key = point.map(|v| (v / cell).floor() as i64);
            let entry = cubes.entry(key).or_insert(([0.0; 3], [0.0; 3], [0.0; 3], 0.0, 0.0));
            entry.4 = entry.4.max(covers);
            for k in 0..3 {
                entry.0[k] += point[k];
                entry.1[k] += normal[k];
                entry.2[k] += f64::from(colour[k]);
            }
            entry.3 += 1.0;
        }
    }
    cubes
        .into_values()
        .map(|(p, n, c, count, covers)| {
            let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-12);
            // (a fifth more than a pixel covers: neighbours' squares meet)
            DetailPoint { point: p.map(|v| (v / count) as f32), normal: n.map(|v| (v / length) as f32), colour: c.map(|v| (v / count) as f32), size: cell.max(1.2 * covers) as f32 }
        })
        .collect()
}

/// Something that matches a reference picture (view 0) against the sources (the others).
pub trait Matcher {
    fn run(&self, views: &[GrayView], prior: Option<&DepthNormalMap>, params: &PatchMatchParams) -> Result<DepthNormalMap, String>;
}

/// PatchMatch as a compute program on this machine: the CPU device with no `gpu`, else that GPU.
pub struct Local {
    pub gpu: Option<GpuLink>,
}

impl Matcher for Local {
    fn run(&self, views: &[GrayView], prior: Option<&DepthNormalMap>, params: &PatchMatchParams) -> Result<DepthNormalMap, String> {
        let sources: Vec<usize> = (1..views.len()).collect();
        patchmatch_program(views, 0, &sources, prior, params, self.gpu.clone())
    }
}

/// [`Local`] on a GPU for as long as `keep` says: asked once an iteration of the matching, and
/// the first false ends the match with "cancelled".
pub struct LocalWhile<'a> {
    pub gpu: GpuLink,
    pub keep: &'a dyn Fn() -> bool,
}

impl Matcher for LocalWhile<'_> {
    fn run(&self, views: &[GrayView], prior: Option<&DepthNormalMap>, params: &PatchMatchParams) -> Result<DepthNormalMap, String> {
        let sources: Vec<usize> = (1..views.len()).collect();
        makepad_ai_geometry::program::patchmatch_program_while(views, 0, &sources, prior, params, Some(self.gpu.clone()), self.keep)
    }
}

/// The points one keyframe's pictures give.
pub fn detail_of(
    matcher: &dyn Matcher,
    keyframe: &CaptureKeyframe,
    cameras: &[CaptureCamera],
    coarse: Coarse,
    shrink: usize,
    cell: f32,
) -> Result<Vec<DetailPoint>, String> {
    detail_of_views(matcher, pair_of(keyframe, cameras, shrink)?, &pictures_of(keyframe, cameras, shrink)?, coarse, cell)
}

/// The points a keyframe's views give (each with its picture), as [`detail_of`] makes them.
pub fn detail_of_views(matcher: &dyn Matcher, views: Vec<GrayView>, pictures: &[Picture], coarse: Coarse, cell: f32) -> Result<Vec<DetailPoint>, String> {
    if views.len() < 2 || pictures.is_empty() {
        return Err("a keyframe needs two views".into());
    }
    let (map, fused) = matched_pair(matcher, &views, coarse)?;
    Ok(points_of(&views[0], &pictures[0], &map, &fused, coarse, cell))
}

/// The part of a view and its picture around a place in the world: the square `half` metres each
/// way of `around` as the view sees it, cut to the picture. Matching what is being scanned at the
/// cameras' own size costs what its part of the picture costs, not the whole picture's. None
/// where the place is behind the view or its square is off the picture.
pub fn around(view: &GrayView, picture: &Picture, around: [f64; 3], half: f64) -> Option<(GrayView, Picture)> {
    let m = &view.cam_to_world;
    let d: [f64; 3] = std::array::from_fn(|i| around[i] - m[i][3]);
    let p: [f64; 3] = std::array::from_fn(|a| m[0][a] * d[0] + m[1][a] * d[1] + m[2][a] * d[2]);
    if p[2] < 0.05 || picture.width != view.width || picture.height != view.height {
        return None;
    }
    let (u, v, reach) = (view.k[0] * p[0] / p[2] + view.k[2], view.k[1] * p[1] / p[2] + view.k[3], half * view.k[0] / p[2]);
    let (x0, x1) = ((u - reach).floor().max(0.0) as usize, ((u + reach).ceil().min(view.width as f64 - 1.0)).max(0.0) as usize);
    let (y0, y1) = ((v - reach).floor().max(0.0) as usize, ((v + reach).ceil().min(view.height as f64 - 1.0)).max(0.0) as usize);
    if x1 < x0 + 16 || y1 < y0 + 16 {
        return None;
    }
    let (width, height) = (x1 - x0 + 1, y1 - y0 + 1);
    let k = [view.k[0], view.k[1], view.k[2] - x0 as f64, view.k[3] - y0 as f64];
    let rows = |at: usize| (y0 + at / width) * view.width + x0 + at % width;
    let cut = GrayView { width, height, pixels: (0..width * height).map(|i| view.pixels[rows(i)]).collect(), k, cam_to_world: view.cam_to_world };
    let shown = Picture { width, height, rgb: (0..width * height).map(|i| picture.rgb[rows(i)]).collect(), k, cam_to_world: picture.cam_to_world, blur_px: picture.blur_px };
    Some((cut, shown))
}

/// The square of a view and its picture, `side` pixels a side, about where `place` shows in it
/// (the picture's middle where it does not show, or none is given), moved to lie inside the
/// picture: what is being scanned at the pictures' own size, for the cost of that many pixels.
pub fn window(view: &GrayView, picture: &Picture, place: Option<[f64; 3]>, side: usize) -> (GrayView, Picture) {
    let m = &view.cam_to_world;
    let shows = place.and_then(|place| {
        let d: [f64; 3] = std::array::from_fn(|i| place[i] - m[i][3]);
        let p: [f64; 3] = std::array::from_fn(|a| m[0][a] * d[0] + m[1][a] * d[1] + m[2][a] * d[2]);
        let (u, v) = (view.k[0] * p[0] / p[2] + view.k[2], view.k[1] * p[1] / p[2] + view.k[3]);
        (p[2] > 0.05 && u >= 0.0 && v >= 0.0 && u < view.width as f64 && v < view.height as f64).then_some((u, v))
    });
    let (u, v) = shows.unwrap_or((view.width as f64 / 2.0, view.height as f64 / 2.0));
    let (width, height) = (side.min(view.width), side.min(view.height));
    let start = |middle: f64, size: usize, whole: usize| ((middle - size as f64 / 2.0).round().max(0.0) as usize).min(whole - size);
    let (x0, y0) = (start(u, width, view.width), start(v, height, view.height));
    let k = [view.k[0], view.k[1], view.k[2] - x0 as f64, view.k[3] - y0 as f64];
    let at = |i: usize| (y0 + i / width) * view.width + x0 + i % width;
    let cut = GrayView { width, height, pixels: (0..width * height).map(|i| view.pixels[at(i)]).collect(), k, cam_to_world: view.cam_to_world };
    let same = picture.width == view.width && picture.height == view.height;
    let rgb = (0..width * height).map(|i| if same { picture.rgb[at(i)] } else { [0.5; 3] }).collect();
    (cut, Picture { width, height, rgb, k, cam_to_world: picture.cam_to_world, blur_px: picture.blur_px })
}

/// The search settings. From a prior, 4 iterations reach what 6 reach from nothing (measured on a
/// GPU at a keyframe's size: 1.46 against 1.47 mm p95); more buys under 0.2 mm.
pub fn params_of(prior: Option<&DepthNormalMap>) -> PatchMatchParams {
    PatchMatchParams {
        radius: 4,
        step: 1,
        top_k: 1,
        iterations: if prior.is_some() { 4 } else { 6 },
        depth_min: DEPTH_MIN,
        depth_max: DEPTH_MAX,
        ..Default::default()
    }
}

/// Where a keyframe's pixels that look down at a level floor go, by how slanted the look is: per
/// slant (the ray's cosine to straight down: over 0.7, 0.5, 0.35, 0.2, and under) the pixels, and
/// of them, in the order they are tested, those the other picture's match does not agree with,
/// those whose match costs too much, those out of the depth range, those far from the base
/// surface, and those kept. A check's measure of what thins the detail on a floor seen at a
/// slant; `detail_of_views` keeps what this counts as kept.
pub fn floor_pixels_by_slant(matcher: &dyn Matcher, views: &[GrayView], coarse: Coarse) -> Result<[[usize; 6]; 5], String> {
    let (map, fused) = matched_pair(matcher, views, coarse)?;
    let view = &views[0];
    let centre = centre_of(view);
    let mut counts = [[0usize; 6]; 5];
    for y in 0..map.height.min(view.height) {
        for x in 0..map.width.min(view.width) {
            let ray = rays_of(view, x, y).1;
            if ray[1] >= 0.0 {
                continue;
            }
            let slant = [0.7, 0.5, 0.35, 0.2].iter().position(|c| -ray[1] > *c).unwrap_or(4);
            let i = y * map.width + x;
            counts[slant][0] += 1;
            let why = match fused.points[i] {
                None => 1,
                Some(_) if !(map.cost[i] <= MAX_COST) => 2,
                Some(_) if !(DEPTH_MIN..=DEPTH_MAX).contains(&map.depth[i]) => 3,
                Some(point) => {
                    let covers = map.depth[i] / view.k[0];
                    let normal = across(&fused, x, y, ray, 4.0 * NORMAL_REACH as f64 * covers).unwrap_or_else(|| rotated(view, map.normal[i]));
                    if near_base(point, centre, ray, normal, coarse) { 5 } else { 4 }
                }
            };
            counts[slant][why] += 1;
        }
    }
    Ok(counts)
}

/// View 0's map, and which of its pixels view 1's map agrees with (each picture matched against the other, the
/// geometry crate's round-trip check of 1 px, 1% of depth and cost 0.3).
fn matched_pair(matcher: &dyn Matcher, views: &[GrayView], coarse: Coarse) -> Result<(DepthNormalMap, FusedMap), String> {
    let prior = prior_of(&views[0], coarse);
    let first = matcher.run(views, prior.as_ref(), &params_of(prior.as_ref()))?;
    // The same pictures with view 1 as the reference.
    let mut swapped: Vec<GrayView> = views.iter().map(|v| GrayView { pixels: v.pixels.clone(), ..*v }).collect();
    swapped.swap(0, 1);
    let prior = prior_of(&swapped[0], coarse);
    let second = matcher.run(&swapped, prior.as_ref(), &params_of(prior.as_ref()))?;
    let maps = [first, second];
    let fused = fuse_view(views, &maps, 0, &[1], &ConsistencyParams { min_views: 1, ..Default::default() })?;
    let [first, _] = maps;
    Ok((first, fused))
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_desk_link::xr_capture::{plane_codec, CaptureLensPose, CapturePlane, CapturePose, CaptureView};

    const WIDTH: usize = 80;
    const FOCAL: f64 = 160.0;
    const PLANE_Z: f64 = 0.4;
    const HALF_BASELINE: f64 = 0.032;
    const CELL: f32 = 0.01;

    fn noise(x: i64, y: i64) -> f64 {
        let mut h = (x as u64).wrapping_mul(0x9e3779b97f4a7c15) ^ (y as u64).wrapping_mul(0xbf58476d1ce4e5b9);
        h = (h ^ (h >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        h = (h ^ (h >> 27)).wrapping_mul(0x94d049bb133111eb);
        h ^= h >> 31;
        0.1 + 0.8 * (h >> 11) as f64 / 9007199254740992.0
    }

    /// Value noise over the plane, a grid cell 4 mm wide.
    fn texture(px: f64, py: f64) -> f64 {
        let (u, v) = (px / 0.004, py / 0.004);
        let (x, y) = (u.floor() as i64, v.floor() as i64);
        let (tx, ty) = (u - x as f64, v - y as f64);
        (noise(x, y) * (1.0 - tx) + noise(x + 1, y) * tx) * (1.0 - ty) + (noise(x, y + 1) * (1.0 - tx) + noise(x + 1, y + 1) * tx) * ty
    }

    /// Two cameras 64 mm apart looking at a fronto-parallel textured plane; the cameras' rotations are the identity.
    fn made_up() -> (CaptureKeyframe, Vec<CaptureCamera>) {
        let size = WIDTH as u32;
        let mut cameras = Vec::new();
        let mut views = Vec::new();
        for (index, cx) in [-HALF_BASELINE, HALF_BASELINE].into_iter().enumerate() {
            cameras.push(CaptureCamera {
                position: index as u8,
                size: [size, size],
                intrinsics: Some([FOCAL as f32, FOCAL as f32, size as f32 / 2.0, size as f32 / 2.0, 0.0]),
                array: [0, 0, size as i32, size as i32],
                active: [0, 0, size as i32, size as i32],
                distortion: None,
                lens_pose: Some(CaptureLensPose { rotation: [0.0, 0.0, 0.0, 1.0], translation: [cx as f32, 0.0, 0.0], reference: 0 }),
            });
            let mid = (WIDTH as f64 - 1.0) / 2.0;
            let data = (0..WIDTH * WIDTH)
                .map(|i| {
                    let (x, y) = ((i % WIDTH) as f64, (i / WIDTH) as f64);
                    let value = texture(cx + (x - mid) / FOCAL * PLANE_Z, (y - mid) / FOCAL * PLANE_Z);
                    (value * 255.0).round() as u8
                })
                .collect();
            views.push(CaptureView {
                camera: index as u8,
                head: CapturePose { orientation: [0.0, 0.0, 0.0, 1.0], position: [0.0; 3] },
                planes: vec![CapturePlane { width: size, height: size, bytes_per_sample: 1, codec: plane_codec::RAW, data }],
                ..Default::default()
            });
        }
        (CaptureKeyframe { index: 0, views, depth: None }, cameras)
    }

    fn plane_distance(centre: [f64; 3], ray: [f64; 3], offset: f64) -> Option<f64> {
        Some((PLANE_Z + offset - centre[2]) / ray[2])
    }

    fn angle_ok(n: [f32; 3]) -> bool {
        f64::from(-n[2]) >= 20.0_f64.to_radians().cos()
    }

    /// The plane's cells (in the world's xy) whose centres show in both pictures with the window's margin.
    fn overlap_cells() -> Vec<[i64; 2]> {
        let mid = (WIDTH as f64 - 1.0) / 2.0;
        let margin = 5.0;
        let shows = |cx: f64, x: f64, y: f64| {
            let (u, v) = (mid + (x - cx) / PLANE_Z * FOCAL, mid + y / PLANE_Z * FOCAL);
            [u, v].iter().all(|&c| c >= margin && c <= WIDTH as f64 - 1.0 - margin)
        };
        let mut cells = Vec::new();
        for i in -40..40 {
            for j in -40..40 {
                let (x, y) = ((i as f64 + 0.5) * f64::from(CELL), (j as f64 + 0.5) * f64::from(CELL));
                if shows(-HALF_BASELINE, x, y) && shows(HALF_BASELINE, x, y) {
                    cells.push([i, j]);
                }
            }
        }
        cells
    }

    #[test]
    fn detail_of_a_made_up_plane() {
        let (keyframe, cameras) = made_up();
        let matcher = Local { gpu: None };
        let near = |c, r| plane_distance(c, r, 0.008);
        let none = |_: [f64; 3], _: [f64; 3]| None;
        let cells = overlap_cells();
        assert!(cells.len() > 100, "the made-up overlap is too small:: {} cells", cells.len());
        let cell_of = |p: &DetailPoint| [0, 1].map(|k| (f64::from(p.point[k]) / f64::from(CELL)).floor() as i64);
        let measure = |points: &[DetailPoint], what: &str, least: f64| {
            let covered = cells.iter().filter(|c| points.iter().any(|p| cell_of(p) == **c)).count();
            let share = covered as f64 / cells.len() as f64;
            let rate = |ok: &dyn Fn(&DetailPoint) -> bool| points.iter().filter(|p| ok(p)).count() as f64 / points.len().max(1) as f64;
            let flat = rate(&|p| (f64::from(p.point[2]) - PLANE_Z).abs() <= 0.002);
            let facing = rate(&|p| angle_ok(p.normal));
            assert!(share >= least, "{what}: only {share:.2} of the plane's cells in the overlap give a point");
            assert!(flat >= 0.95, "{what}: only {flat:.3} of the points are within 2 mm of the plane");
            assert!(facing >= 0.95, "{what}: only {facing:.3} of the normals are within 20 degrees of the plane's");
        };

        // One match with the coarse surface serves the measure, the order and the far check: the
        // points are made from its maps each time.
        let views = pair_of(&keyframe, &cameras, 1).unwrap();
        let (map, fused) = matched_pair(&matcher, &views, &near).unwrap();
        let pictures = pictures_of(&keyframe, &cameras, 1).unwrap();
        let with = points_of(&views[0], &pictures[0], &map, &fused, &near, CELL);
        measure(&with, "with a coarse surface", 0.6);
        assert_eq!(with, points_of(&views[0], &pictures[0], &map, &fused, &near, CELL), "the same maps give other points or another order");
        // The made-up texture is grey. With cubes finer than a pixel each point is one pixel's: its colour is that pixel's grey.
        let single = points_of(&views[0], &pictures[0], &map, &fused, &near, CELL / 20.0);
        let (mut checked, mut worst) = (0, 0.0f32);
        for p in &single {
            let d: [f64; 3] = std::array::from_fn(|i| f64::from(p.point[i]) - views[0].cam_to_world[i][3]);
            let (x, y) = ((views[0].k[0] * d[0] / d[2] + views[0].k[2]).round(), (views[0].k[1] * d[1] / d[2] + views[0].k[3]).round());
            let grey = views[0].pixels[y as usize * views[0].width + x as usize];
            checked += 1;
            worst = worst.max(p.colour.iter().map(|c| (c - grey).abs()).fold(0.0, f32::max));
        }
        assert!(checked > 500 && worst < 0.02, "{checked} points' colours are up to {worst:.3} off their pixels' grey");
        // A coarse surface 0.2 m away from the matched plane keeps none of it.
        let far = |c, r| plane_distance(c, r, 0.2);
        let kept = points_of(&views[0], &pictures[0], &map, &fused, &far, CELL);
        assert!(kept.is_empty(), "{} points 0.2 m off the coarse surface were kept", kept.len());
        // The whole path, with no coarse surface to start from or to hold it.
        let without = detail_of(&matcher, &keyframe, &cameras, &none, 1, CELL).unwrap();
        measure(&without, "without a coarse surface", 0.4);
    }
}

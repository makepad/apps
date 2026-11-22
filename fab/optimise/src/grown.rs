//! A grown part (`m.grow`) between its script and [`crate::grow`]: the object's force study posed
//! over the part's body frame, so moving or turning it never grows it again, and each shape a grow
//! shows as the distance the part reads ([`Options::solved`]).
//!
//! The levels nest: each has twice the last's cells along every axis over the same box, so a level
//! starts from the one before it.
//!
//! [`Options::solved`]: makepad_model_script::lang::Options::solved

use crate::grade::{part_places, with_source};
use crate::grow::{Edit, Job, Level, AVOID, KEEP};
use fab_force::material::{Materials, Print, FILAMENTS};
use fab_force::study::{Load, Model};
use fab_force::V3;
use makepad_csg_math::Vec3d;
use makepad_model_script::lang::csg_sdf::{self, SdfGrid, SdfNode, SdfPlace};
use makepad_model_script::lang::GrowRequest;

/// Cells over the solid's box at a grow's first level.
const FIRST_CELLS: f64 = 2500.0;
/// Each level's design iterations and its projection's sharpness, coarsest first.
const LEVELS: [(usize, [f64; 2]); 4] = [(12, [1.0, 3.0]), (24, [3.0, 8.0]), (16, [8.0, 12.0]), (12, [12.0, 16.0])];
/// The levels that answer a change as it is made: a shape in tens of milliseconds. The ones after
/// refine what these settle at.
pub const COARSE_LEVELS: usize = 2;
/// A level is fine enough at this share of the part's thinnest member.
const CELLS_PER_MEMBER: f64 = 3.0;
/// The design iterations a settled refinement runs for a change.
const FINE_WARM_ITS: usize = 12;
/// Past any fit: a refinement follows what the coarse levels settle at, and those decide when a
/// change wants a new shape.
const NEVER_REFIT: f64 = 1e30;

fn dot(a: V3, b: V3) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}

/// The places from a part's body to the object, as a rigid map with a uniform scale, row-major:
/// what a host hands the study's frame when it wants the loads in the body's.
pub fn body_to_object(object: &SdfNode, part: &str) -> Result<[[f64; 4]; 3], String> {
    let places = part_places(object, csg_sdf::part_id(part))?;
    let out = |mut p: V3| {
        for place in &places {
            p = place.out_of(p);
        }
        p
    };
    let o = out([0.0; 3]);
    let axes = [out([1.0, 0.0, 0.0]), out([0.0, 1.0, 0.0]), out([0.0, 0.0, 1.0])];
    Ok(std::array::from_fn(|r| [axes[0][r] - o[r], axes[1][r] - o[r], axes[2][r] - o[r], o[r]]))
}

/// A grown part's study, posed over a span of the levels: the [`Job`] that grows it and what its
/// shapes and edits need.
#[derive(Debug)]
pub struct Posed {
    job: Job,
    models: Vec<Model>,
    member: f64,
    source: SdfNode,
    /// Its levels' place among all of a part's, and the finest level the part asks for.
    first_level: usize,
    finest: usize,
    /// What its job holds and loads.
    loads: Vec<Load>,
}

impl Posed {
    /// Poses `request`'s part at the levels that answer a change: `object` (its field in the
    /// model's frame) with the part solid, under `loads` given in the part's body frame. An error
    /// when nothing holds it, nothing loads it, or it has no one frame.
    pub fn new(object: &SdfNode, request: &GrowRequest, loads: &[Load]) -> Result<Posed, String> {
        Posed::span(object, request, loads, 0, |_| COARSE_LEVELS)
    }

    /// The levels that refine these, if the part asks for any.
    pub fn refinement(object: &SdfNode, request: &GrowRequest, loads: &[Load]) -> Result<Option<Posed>, String> {
        let posed = Posed::span(object, request, loads, COARSE_LEVELS, |finest| finest + 1)?;
        Ok((!posed.models.is_empty()).then_some(posed))
    }

    /// Whether the part asks for levels past these.
    pub fn refines(&self) -> bool {
        self.finest >= COARSE_LEVELS
    }

    fn span(object: &SdfNode, request: &GrowRequest, loads: &[Load], first_level: usize, end: impl Fn(usize) -> usize) -> Result<Posed, String> {
        let id = csg_sdf::part_id(&request.part);
        let places: Vec<SdfPlace> = part_places(object, id)?;
        let solid = with_source(object, id, &request.source);
        let scale: f64 = places.iter().map(|p| p.scale).product();
        let Some(bounds) = csg_sdf::node_bounds(&solid) else { return Err("the solid has no bounds".into()) };
        let bounds = csg_sdf::bounds::map_bounds(&bounds, |mut p| {
            for place in places.iter().rev() {
                p = place.into(p);
            }
            p
        });
        let mut distance = |ps: &[V3], out: &mut [f64]| {
            let qs: Vec<Vec3d> = ps.iter().map(|p| {
                let mut q = *p;
                for place in &places {
                    q = place.out_of(q);
                }
                Vec3d::new(q[0], q[1], q[2])
            }).collect();
            csg_sdf::distance_many(&solid, &qs, out);
            out.iter_mut().for_each(|d| *d /= scale);
        };

        // the first level's cells over the box, then the box they make, centred on it
        let span: V3 = std::array::from_fn(|a| (bounds.1[a] - bounds.0[a]).max(1e-3));
        let first = (span[0] * span[1] * span[2] / FIRST_CELLS).cbrt();
        let cells: V3 = std::array::from_fn(|a| (span[a] / first).ceil().max(1.0));
        let centre: V3 = std::array::from_fn(|a| 0.5 * (bounds.0[a] + bounds.1[a]));
        let materials = Materials::new(FILAMENTS[0], &Print::default(), false);
        let finest = finest_level(first, request.member);
        let mut models = Vec::new();
        for l in first_level..end(finest) {
            let fold = (1usize << l) as f64;
            let h = first / fold;
            // a Model adds a cell each side: this level's reach, a hair under whole cells, so that
            // it comes to the first level's box
            let half: V3 = std::array::from_fn(|a| 0.5 * (cells[a] * first + 2.0 * (fold - 1.0) * h - 1e-6 * h));
            let lo = std::array::from_fn(|a| centre[a] - half[a]);
            let hi = std::array::from_fn(|a| centre[a] + half[a]);
            let model = Model::new(lo, hi, h, &mut distance, &materials);
            for a in 0..3 {
                assert_eq!(model.grid.n[a], (1usize << l) * (cells[a] as usize + 2), "level {l} nests");
            }
            models.push(model);
        }
        // past the part's finest level there is no job to run
        let job = job_of(&models, first_level, request.member, &request.source, loads, request.mass)?;
        Ok(Posed { job, models, member: request.member, source: request.source.clone(), first_level, finest, loads: loads.to_vec() })
    }

    /// The same space under other loads: its fixtures or loads on other nodes, so a grow of its own.
    pub fn with_loads(self, loads: &[Load], mass: f64) -> Result<Posed, String> {
        let job = if self.loads == loads {
            Job { volume: mass, ..self.job }
        } else {
            job_of(&self.models, self.first_level, self.member, &self.source, loads, mass)?
        };
        Ok(Posed { job, loads: loads.to_vec(), ..self })
    }

    /// The same refinement started from what `coarse` settled at (`x`, its last level's).
    pub fn refining(self, coarse: &Posed, loads: &[Load], mass: f64, x: &[f32]) -> Result<Posed, String> {
        let mut posed = self.with_loads(loads, mass)?;
        posed.start_from(coarse.last_n(), x);
        Ok(posed)
    }

    fn last_n(&self) -> [usize; 3] {
        self.job.levels[self.job.levels.len() - 1].n
    }

    /// Starts its first level from a density on the nesting level before it, a coarse cell's
    /// density in each of the eight it holds.
    fn start_from(&mut self, n: [usize; 3], x: &[f32]) {
        let fine = self.job.levels[0].n;
        assert!((0..3).all(|a| fine[a] == 2 * n[a]) && x.len() == n[0] * n[1] * n[2], "a refinement starts from the level before it");
        let mut start = Vec::with_capacity(fine[0] * fine[1] * fine[2]);
        for k in 0..fine[2] {
            for j in 0..fine[1] {
                for i in 0..fine[0] {
                    start.push(x[i / 2 + n[0] * (j / 2 + n[1] * (k / 2))]);
                }
            }
        }
        self.job.start = Some(start.into());
    }

    /// A level's cell, mm, by its place in this span.
    pub fn h(&self, level: usize) -> f64 {
        self.job.levels[level].h
    }

    /// The levels this span runs.
    pub fn levels(&self) -> usize {
        self.job.levels.len()
    }

    /// Where this span's first level sits among a part's levels.
    pub fn first_level(&self) -> usize {
        self.first_level
    }

    pub fn job(&self) -> &Job {
        &self.job
    }

    /// The message a held grow takes for these loads and this mass, or none when they hold or load
    /// other nodes (the cells kept solid there change: that one grows again).
    pub fn edit(&self, loads: &[Load], mass: f64) -> Option<Edit> {
        let mut load = Vec::new();
        for (model, level) in self.models.iter().zip(&self.job.levels) {
            let (held, force) = fab_force::bc::nodes(&model.grid, &model.surface, &model.conditions(loads));
            let same_held = held.iter().zip(&level.held).all(|(a, b)| *a as u32 == *b);
            let loaded = |f: &[f32]| f.iter().any(|c| *c != 0.0);
            let next = load_of(&force);
            let same_loaded = next.iter().zip(&level.load).all(|(a, b)| loaded(a) == loaded(b));
            if !same_held || !same_loaded {
                return None;
            }
            load.push(next);
        }
        Some(Edit { volume: mass, load })
    }

    /// Whether iteration `it` of `level` (its place in this span) is the level's last before it
    /// settles. The ones an edit runs count on past that.
    pub fn level_done(&self, level: usize, it: usize) -> bool {
        it + 1 == self.job.levels[level].its
    }

    /// A shape of level `level`'s cells: how many, their size, where the first is centred and how
    /// many of them come to `mass`.
    pub fn cells(&self, level: usize, mass: f64) -> ([u32; 3], f32, [f32; 3], f64) {
        let grid = &self.models[level].grid;
        let h = grid.h[0];
        (grid.n.map(|n| n as u32), h as f32, grid.origin.map(|o| (o + 0.5 * h) as f32), self.job.levels[level].target(mass))
    }

    /// A shape of level `level` (its projected density) as the part reads it, in the body's frame:
    /// the densest cells that come to `mass`, so it weighs what it should from its first look, when
    /// its densities are still soft.
    pub fn grid(&self, level: usize, xp: &[f32], mass: f64) -> SdfGrid {
        let (n, h, origin, solid) = self.cells(level, mass);
        let data = fab_model::grown::distance_of(n.map(|c| c as usize), h as f64, xp, solid);
        SdfGrid::new(origin, h, n, 0.0, data.into())
    }
}

/// The finest level a part wants: the first whose cells are a third of its thinnest member, or the
/// last there is.
fn finest_level(first: f64, member: f64) -> usize {
    (1..LEVELS.len()).find(|l| first / (1usize << l) as f64 <= member / CELLS_PER_MEMBER).unwrap_or(LEVELS.len() - 1)
}

fn job_of(models: &[Model], first_level: usize, member: f64, source: &SdfNode, loads: &[Load], mass: f64) -> Result<Job, String> {
    let mut levels = Vec::new();
    for (model, (its, beta)) in models.iter().zip(LEVELS.into_iter().skip(first_level)) {
        let (held, force) = fab_force::bc::nodes(&model.grid, &model.surface, &model.conditions(loads));
        if !held.iter().any(|h| *h != 0) {
            return Err("nothing in its force study holds it".into());
        }
        if !force.iter().any(|f| f.iter().any(|c| *c != 0.0)) {
            return Err("nothing in its force study loads it".into());
        }
        levels.push(Level {
            n: model.grid.n,
            h: model.h,
            its,
            rmin: (0.5 * member).max(1.5 * model.h),
            beta,
            held: held.iter().map(|h| *h as u32).collect(),
            load: load_of(&force),
            mask: mask(model, &held, &force, member, source),
        });
    }
    let mut job = Job::new(levels, mass);
    job.hold = true;
    if first_level > 0 {
        job.warm_its = FINE_WARM_ITS;
        job.refit = NEVER_REFIT;
    }
    Ok(job)
}

fn load_of(force: &[V3]) -> Vec<[f32; 4]> {
    force.iter().map(|f| [f[0] as f32, f[1] as f32, f[2] as f32, 0.0]).collect()
}

/// Which cells a level leaves to the grow. Outside the solid: never. Within the part's thinnest
/// member of what holds or loads it (and at least the cells those nodes touch), and on the object's
/// other solids: always.
fn mask(model: &Model, held: &[u8], force: &[V3], member: f64, source: &SdfNode) -> Vec<u32> {
    let g = &model.grid;
    let h = g.h[0];
    let corner = 3f64.sqrt() * 0.5 * h;
    let mut mask: Vec<u32> = (0..g.cells()).map(|c| if g.active(c) { 0 } else { AVOID }).collect();

    // the object's own, wholly outside the part's space
    let free: Vec<usize> = (0..g.cells()).filter(|c| mask[*c] == 0).collect();
    let centres: Vec<Vec3d> = free.iter().map(|c| {
        let p = g.cell_center(*c);
        Vec3d::new(p[0], p[1], p[2])
    }).collect();
    let mut outside = vec![0.0; centres.len()];
    csg_sdf::distance_many(source, &centres, &mut outside);
    for (c, d) in free.iter().zip(&outside) {
        if *d > corner {
            mask[*c] = KEEP;
        }
    }

    let reach = member.max(corner * (1.0 + 1e-9));
    let cells = (reach / h).ceil() as isize;
    for node in (0..g.nodes()).filter(|n| held[*n] != 0 || force[*n].iter().any(|c| *c != 0.0)) {
        let at = g.node_ijk(node);
        let pos = g.node_pos(node);
        for k in -cells..cells {
            for j in -cells..cells {
                for i in -cells..cells {
                    let ijk = [at[0] as isize + i, at[1] as isize + j, at[2] as isize + k];
                    if (0..3).any(|a| ijk[a] < 0 || ijk[a] >= g.n[a] as isize) {
                        continue;
                    }
                    let c = g.cell(ijk[0] as usize, ijk[1] as usize, ijk[2] as usize);
                    let centre = g.cell_center(c);
                    let d: V3 = std::array::from_fn(|a| centre[a] - pos[a]);
                    if mask[c] != AVOID && dot(d, d) <= reach * reach {
                        mask[c] = KEEP;
                    }
                }
            }
        }
    }
    mask
}

/// A grid's samples read at the cells of another over the same box: linear between its samples,
/// its edge's value past them.
fn resampled(g: &SdfGrid, origin: [f32; 3], h: f32, n: [u32; 3]) -> Vec<f32> {
    // the fractional sample and the pair round it, along one axis
    let along = |a: usize| -> Vec<(usize, usize, f32)> {
        let last = g.dims[a] as usize - 1;
        (0..n[a] as usize).map(|i| {
            let at = (((origin[a] + i as f32 * h - g.origin[a]) / g.h).max(0.0)).min(last as f32);
            let low = (at as usize).min(last.saturating_sub(1));
            (low, (low + 1).min(last), at - low as f32)
        }).collect()
    };
    let (xs, ys, zs) = (along(0), along(1), along(2));
    let (dx, dy) = (g.dims[0] as usize, g.dims[0] as usize * g.dims[1] as usize);
    let d = &g.data;
    let mut out = Vec::with_capacity((n[0] * n[1] * n[2]) as usize);
    for &(z0, z1, tz) in &zs {
        for &(y0, y1, ty) in &ys {
            let rows = [(y0 * dx + z0 * dy, 1.0 - ty, 1.0 - tz), (y1 * dx + z0 * dy, ty, 1.0 - tz), (y0 * dx + z1 * dy, 1.0 - ty, tz), (y1 * dx + z1 * dy, ty, tz)];
            for &(x0, x1, tx) in &xs {
                let mut v = 0.0;
                for &(row, wy, wz) in &rows {
                    v += wy * wz * (d[row + x0] * (1.0 - tx) + d[row + x1] * tx);
                }
                out.push(v);
            }
        }
    }
    out
}

/// Whether `g`'s samples are those of a whole grid (a crop reads only part).
fn whole(g: &SdfGrid) -> bool {
    g.first == [0; 3] && g.count == g.dims
}

/// What `fine` shows while the coarse levels follow a change: its detail, moved as the coarse shape
/// moved from `base` to `now` (those two of one level). A distance's sum climbs as fast as both, so
/// the field is scaled back to the slope a distance has; the surface stays where the sum is zero.
pub fn preview(fine: &SdfGrid, base: &SdfGrid, now: &SdfGrid) -> SdfGrid {
    assert!(whole(fine) && whole(base) && whole(now) && base.dims == now.dims, "a preview of whole grids, its coarse pair of one level");
    let change: Vec<f32> = base.data.iter().zip(now.data.iter()).map(|(b, n)| n - b).collect();
    // the change's steepest climb, along each axis
    let [nx, ny, _] = base.dims.map(|d| d as usize);
    let strides = [1, nx, nx * ny];
    let mut steep = 0f32;
    for (a, stride) in strides.into_iter().enumerate() {
        let run = base.dims[a] as usize;
        for (i, v) in change.iter().enumerate() {
            let along = i / stride % run;
            if along + 1 < run {
                steep = steep.max((change[i + stride] - v).abs());
            }
        }
    }
    let scale = 1.0 / (1.0 + steep / base.h);
    let moved = SdfGrid::new(base.origin, base.h, base.dims, 0.0, change.into());
    let at_fine = resampled(&moved, fine.origin, fine.h, fine.dims);
    let data: Vec<f32> = fine.data.iter().zip(&at_fine).map(|(f, c)| scale * (f + c)).collect();
    SdfGrid::new(fine.origin, fine.h, fine.dims, 0.0, data.into())
}

/// One shape turning into another: the mix of their distances, at the finer one's cells, so a level
/// taking over from another does not pop. The levels share their box.
pub struct Fade {
    origin: [f32; 3],
    h: f32,
    dims: [u32; 3],
    from: Vec<f32>,
    to: Vec<f32>,
}

impl Fade {
    pub fn new(from: &SdfGrid, to: &SdfGrid) -> Fade {
        assert!(whole(from) && whole(to), "a fade of whole grids");
        let finer = if to.h <= from.h { to } else { from };
        let at = |g: &SdfGrid| if g.h == finer.h && g.origin == finer.origin { g.data.to_vec() } else { resampled(g, finer.origin, finer.h, finer.dims) };
        Fade { origin: finer.origin, h: finer.h, dims: finer.dims, from: at(from), to: at(to) }
    }

    /// A newer shape of the same level to turn into.
    pub fn retarget(&mut self, to: &SdfGrid) {
        self.to = if to.dims == self.dims { to.data.to_vec() } else { resampled(to, self.origin, self.h, self.dims) };
    }

    /// The mix, `t` of the way (0 to 1).
    pub fn at(&self, t: f32) -> SdfGrid {
        let data: Vec<f32> = self.from.iter().zip(&self.to).map(|(a, b)| a + t * (b - a)).collect();
        SdfGrid::new(self.origin, self.h, self.dims, 0.0, data.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    fn ramp(origin: [f32; 3], h: f32, n: u32, at: impl Fn([f32; 3]) -> f32) -> SdfGrid {
        let mut data = Vec::new();
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    data.push(at([origin[0] + i as f32 * h, origin[1] + j as f32 * h, origin[2] + k as f32 * h]));
                }
            }
        }
        SdfGrid::new(origin, h, [n; 3], 0.0, data.into())
    }

    /// Reading a field linear in space gives the field, at the cells of a level over the same box,
    /// and a fade ends on the shapes it joins.
    #[test]
    fn a_fade_joins_two_levels_over_one_box() {
        let plane = |p: [f32; 3]| 0.5 * p[0] - 0.25 * p[1] + p[2] - 3.0;
        // cell-centred levels of a box from 0 to 16: cells of 2, and of 1
        let coarse = ramp([1.0; 3], 2.0, 8, plane);
        let fine = ramp([0.5; 3], 1.0, 16, |p| plane(p) + 0.25);
        let fade = Fade::new(&coarse, &fine);
        assert_eq!(fade.dims, fine.dims);
        let start = fade.at(0.0);
        for (at, v) in start.data.iter().enumerate() {
            let (i, j, k) = (at % 16, at / 16 % 16, at / 256);
            // inside the coarse samples' span the read is exact; past it, the edge's value holds
            if [i, j, k].iter().all(|c| (1..15).contains(c)) {
                let p = [i as f32 + 0.5, j as f32 + 0.5, k as f32 + 0.5];
                assert!((v - plane(p)).abs() < 1e-4, "{at}: {v} against {}", plane(p));
            }
        }
        assert!(fade.at(1.0).data.iter().zip(fine.data.iter()).all(|(a, b)| (a - b).abs() < 1e-5), "it ends on the finer shape");
        let back = Fade::new(&fine, &coarse);
        assert_eq!(back.at(0.0).data, fine.data, "it starts on the finer shape going back too");
        let mid = fade.at(0.5);
        assert!(mid.data.iter().zip(start.data.iter()).zip(fine.data.iter()).all(|((m, a), b)| (m - 0.5 * (a + b)).abs() < 1e-4));
    }

    /// With no change the preview is the fine shape. A change moves its surface by what the coarse
    /// shape moved, its slope stays a distance's, and its surface is where the sum is zero.
    #[test]
    fn a_preview_keeps_the_fine_shape_and_follows_the_coarse_change() {
        let sphere = |c: f32, r: f32| move |p: [f32; 3]| ((p[0] - c).powi(2) + (p[1] - 8.0).powi(2) + (p[2] - 8.0).powi(2)).sqrt() - r;
        let base = ramp([1.0; 3], 2.0, 8, sphere(8.0, 5.0));
        let fine = ramp([0.5; 3], 1.0, 16, sphere(8.0, 5.0));
        let same = preview(&fine, &base, &base);
        assert_eq!(same.data, fine.data);

        let now = ramp([1.0; 3], 2.0, 8, sphere(8.0, 6.0));
        let moved = preview(&fine, &base, &now);
        // on the axis through the centre at x = 8 + 5.5: it was outside, and is inside now
        let at = |g: &SdfGrid, i: usize| g.data[i + 16 * (7 + 16 * 7)];
        assert!(at(&fine, 13) > 0.0 && at(&moved, 13) < 0.0, "{} {}", at(&fine, 13), at(&moved, 13));
        let mut steepest = 0f32;
        for i in 0..15 {
            steepest = steepest.max((at(&moved, i + 1) - at(&moved, i)).abs());
        }
        assert!(steepest <= 1.05, "slope {steepest}");

        // a change that differs from place to place: the scale takes up its climb
        let aside = preview(&fine, &base, &ramp([1.0; 3], 2.0, 8, sphere(10.0, 5.0)));
        let mut steepest = 0f32;
        for i in 0..15 {
            steepest = steepest.max((at(&aside, i + 1) - at(&aside, i)).abs());
        }
        assert!(steepest <= 1.05, "slope {steepest}");
    }

    /// A part's finest level is the first with three cells across its thinnest member, and there
    /// is a last one however thin it gets.
    #[test]
    fn a_thin_member_asks_for_a_fine_level() {
        assert_eq!(finest_level(12.0, 12.0), 2, "12 mm cells, 3 mm at the third level");
        assert_eq!(finest_level(12.0, 18.0), 1);
        assert_eq!(finest_level(4.0, 3.0), 2);
        assert_eq!(finest_level(4.0, 0.1), LEVELS.len() - 1);
    }

}

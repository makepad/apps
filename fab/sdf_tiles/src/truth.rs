//! The tile view against an exact trace: every pixel's ray walked by a true Lipschitz bound.
use super::*;
use makepad_csg_math::Vec3d;
use makepad_csg_sdf::SdfOp;

/// The machine's threads, or fewer when the run was given a CPU budget (`MAKEPAD_CPU_CORES`, a
/// devhub lease's width): a test sized from the box runs past the width its run was told.
pub(crate) fn budget_threads() -> usize {
    let machine = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::env::var("MAKEPAD_CPU_CORES").ok().and_then(|v| v.trim().parse::<usize>().ok()).filter(|n| *n > 0).map_or(machine, |n| n.min(machine))
}

/// A pass run to its end: the moving one, and with `still` the one after it. Its evals are
/// those of the view alone.
pub(super) fn pass_to_the_end(root: &SdfNode, cam: Cam, still: bool) -> (Field, Pass, u64) {
    let pool = TaskPool::closed();
    let f = Field::new(root, false, &pool).unwrap();
    let e0 = f.evals.load(Ordering::Relaxed);
    let mut grid = Grid::default();
    let mut pass = Pass::new(cam, &mut grid);
    let never = || false;
    let mut queue = pass.roots(&f);
    let mut level = 0;
    while !queue.is_empty() && level < LEVELS {
        queue = pass.level(&f, &pool, &mut grid, level, &queue, MOVING, &never).unwrap();
        level += 1;
    }
    for t in std::mem::take(&mut pass.standing) {
        pass.alive[t as usize] = false;
    }
    pass.moving_done = true;
    if still {
        let mut queue: Vec<Req> = Vec::new();
        for level in 0..LEVELS {
            queue.append(&mut pass.deferred[level]);
            if !queue.is_empty() {
                queue = pass.level(&f, &pool, &mut grid, level, &queue, STILL, &never).unwrap();
            }
        }
        for t in std::mem::take(&mut pass.standing) {
            pass.alive[t as usize] = false;
        }
    }
    (f, pass, e0)
}

fn box_d(p: V, s: V) -> f64 {
    let q = [p[0].abs() - s[0] * 0.5, p[1].abs() - s[1] * 0.5, p[2].abs() - s[2] * 0.5];
    len([q[0].max(0.0), q[1].max(0.0), q[2].max(0.0)]) + q[0].max(q[1].max(q[2])).min(0.0)
}

/// The lattice cube of the Fab demo in f64.
struct LatticeField { size: f64, period: f64 }
impl makepad_csg_sdf::Sdf3 for LatticeField {
    fn distance(&self, p: Vec3d) -> f64 {
        let p = [p.x, p.y, p.z];
        let z = self.size;
        let c = sub(p, [0.0, z * 0.5, 0.0]);
        let cube = box_d(c, [z; 3]);
        let inner = z - 3.0;
        let open = box_d(c, [z + 2.0, inner, inner]).min(box_d(c, [inner, z + 2.0, inner])).min(box_d(c, [inner, inner, z + 2.0]));
        let frame = cube.max(-open);
        let k = std::f64::consts::TAU / self.period;
        let [x, y, z] = p.map(|a| a * k);
        let g = x.sin() * y.cos() + y.sin() * z.cos() + z.sin() * x.cos();
        let n = len([x.cos() * y.cos() - z.sin() * x.sin(), y.cos() * z.cos() - x.sin() * y.sin(), z.cos() * x.cos() - y.sin() * z.sin()]);
        let sheet = g.abs() / (k * n.max(1.0)) - 0.6;
        sheet.max(cube).min(frame)
    }
}

/// The wavy cube: the Fab example's field, a kernel, at a size and a period.
fn lattice(size: f64, period: f64) -> SdfNode {
    let h = size * 0.5;
    SdfNode::Kernel(makepad_csg_sdf::SdfKernel { key: 1, field: Arc::new(LatticeField { size, period }), splash: None, bounds: Some(([-h, 0.0, -h], [h, size, h])), solid: None })
}

/// The same cube in engine nodes, its gyroid the raw one.
fn wavy_native() -> SdfNode {
    let boxed = |x: f64, y: f64, z: f64| SdfNode::Box { size: [x, y, z], radius: 0.0 };
    let all = |op, of: Vec<SdfNode>| SdfNode::Combine { op, blend: 0.0, of };
    let open = all(SdfOp::Union, vec![boxed(26.0, 21.0, 21.0), boxed(21.0, 26.0, 21.0), boxed(21.0, 21.0, 26.0)]);
    let frame = all(SdfOp::Difference, vec![boxed(24.0, 24.0, 24.0), open]);
    let sheet = all(SdfOp::Intersection, vec![SdfNode::Gyroid { period: 8.0, wall: 1.2 }, boxed(24.0, 24.0, 24.0)]);
    let cube = all(SdfOp::Union, vec![sheet, frame]);
    SdfNode::Transform { at: [0.0, 12.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(cube) }
}

/// Where the ray through each of `at` first meets the field: depth along the ray, infinite for
/// none. Walks by the field's value over `lip` (a true Lipschitz bound, given), never less than
/// a twentieth of a pixel, then halves the bracket of the first sign change.
pub(super) fn exact(root: &SdfNode, lo: V, hi: V, cam: &Cam, lip: f64, at: &[(f64, f64)]) -> Vec<f64> {
    struct W { i: u32, o: V, d: V, t: f64, t1: f64, a: f64, b: f64, n: u8 }
    let threads = budget_threads();
    let found: Vec<Vec<(u32, f64)>> = std::thread::scope(|s| {
        let hs: Vec<_> = at
            .chunks((at.len() / threads).max(1))
            .enumerate()
            .map(|(band, part)| {
                let base = band * (at.len() / threads).max(1);
                s.spawn(move || {
                    let mut out = Vec::new();
                    for chunk in 0..part.len().div_ceil(1024) {
                        let mut live: Vec<W> = part[chunk * 1024..((chunk + 1) * 1024).min(part.len())]
                            .iter()
                            .enumerate()
                            .filter_map(|(j, xy)| {
                                let (o, d) = cam.ray(xy.0, xy.1);
                                let (a, b) = slab(o, d, lo, hi)?;
                                Some(W { i: (base + chunk * 1024 + j) as u32, o, d, t: a, t1: b, a: -1.0, b: -1.0, n: 0 })
                            })
                            .collect();
                        while !live.is_empty() {
                            let pts: Vec<V> = live.iter().map(|w| add(w.o, mul(w.d, w.t))).collect();
                            let vals = raw(root, &pts);
                            let mut k = 0;
                            live.retain_mut(|w| {
                                let v = vals[k];
                                k += 1;
                                if w.b >= 0.0 {
                                    if v > 0.0 { w.a = w.t } else { w.b = w.t }
                                    w.n += 1;
                                    if w.n >= 24 || w.b - w.a < cam.pw(w.t) * 0.005 {
                                        out.push((w.i, w.b));
                                        return false;
                                    }
                                    w.t = (w.a + w.b) * 0.5;
                                    return true;
                                }
                                if v <= 0.0 {
                                    if w.a < 0.0 {
                                        out.push((w.i, w.t));
                                        return false;
                                    }
                                    w.b = w.t;
                                    w.t = (w.a + w.b) * 0.5;
                                    return true;
                                }
                                w.a = w.t;
                                w.t += (v / lip).max(cam.pw(w.t) * 0.05);
                                w.t <= w.t1
                            });
                        }
                    }
                    out
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut d = vec![f64::INFINITY; at.len()];
    for (i, t) in found.into_iter().flatten() {
        d[i as usize] = t;
    }
    d
}

/// The tiles standing, drawn: per pixel the depth along its ray and the side of the tile.
fn draw_tiles(pass: &Pass) -> (Vec<f64>, Vec<u8>) {
    let cam = pass.cam;
    let (w, h) = (cam.w as usize, cam.h as usize);
    let (mut depth, mut side) = (vec![f64::INFINITY; w * h], vec![0u8; w * h]);
    for ((t, size), _) in pass.tiles.iter().zip(&pass.sizes).zip(&pass.alive).filter(|(_, a)| **a) {
        let at = |k: usize| -> V { [t[k * 4] as f64, t[k * 4 + 1] as f64, t[k * 4 + 2] as f64] };
        let c = at(4);
        let corners = [at(0), at(1), at(2), at(3)];
        for k in 0..4 {
            let (a, b) = (corners[k], corners[(k + 1) % 4]);
            let (e1, e2) = (sub(a, c), sub(b, c));
            if len(cross(e1, e2)) == 0.0 {
                continue;
            }
            let tri: Vec<(f64, f64, f64)> = [c, a, b].iter().filter_map(|p| cam.proj(*p)).collect();
            if tri.len() < 3 {
                continue;
            }
            let x0 = tri.iter().map(|p| p.0).fold(f64::INFINITY, f64::min).floor().max(0.0) as usize;
            let y0 = tri.iter().map(|p| p.1).fold(f64::INFINITY, f64::min).floor().max(0.0) as usize;
            let x1 = (tri.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max).ceil().max(0.0) as usize).min(w);
            let y1 = (tri.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max).ceil().max(0.0) as usize).min(h);
            for y in y0..y1 {
                for x in x0..x1 {
                    let (o, d) = cam.ray(x as f64 + 0.5, y as f64 + 0.5);
                    let pv = cross(d, e2);
                    let det = dot(e1, pv);
                    if det.abs() < 1e-14 {
                        continue;
                    }
                    let tv = sub(o, c);
                    let (u, q) = (dot(tv, pv) / det, cross(tv, e1));
                    let v = dot(d, q) / det;
                    if u < 0.0 || v < 0.0 || u + v > 1.0 {
                        continue;
                    }
                    let along = dot(e2, q) / det;
                    if along > 0.0 && along < depth[y * w + x] {
                        depth[y * w + x] = along;
                        side[y * w + x] = *size as u8;
                    }
                }
            }
        }
    }
    (depth, side)
}

/// What the tiles get wrong against the exact trace. A pixel is wrong when its depth is more
/// than 2 px off the exact one at it and at each of its neighbours: an edge a pixel out is not.
#[derive(Default, Debug)]
struct Wrong {
    covered: usize,
    /// Off at the pixel itself, edges included.
    loose: usize,
    /// Drawn in front of what the exact trace finds, or where it finds nothing.
    solid: usize,
    /// Drawn behind it, or nothing drawn.
    missing: usize,
    /// The side of the largest square of wrong pixels.
    patch: usize,
    /// Wrong solid pixels by the side of the tile over them (1, 2, 4, 8, 16).
    by_tile: [usize; 5],
}

fn wrong_pixels(cam: &Cam, truth: &[f64], depth: &[f64], side: &[u8]) -> Wrong {
    let (w, h) = (cam.w as usize, cam.h as usize);
    let mut out = Wrong::default();
    let mut square = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let (e, d) = (truth[i], depth[i]);
            out.covered += (e < f64::INFINITY) as usize;
            let near = if e < f64::INFINITY { e } else if d < f64::INFINITY { d } else { continue };
            let tol = cam.pw(near) * 2.0;
            let off = |a: f64, b: f64| if a.is_finite() && b.is_finite() { (a - b).abs() > tol } else { a.is_finite() != b.is_finite() };
            if !off(d, e) {
                continue;
            }
            out.loose += 1;
            // an edge or a steep face a pixel out is not wrong: the depth lies among its neighbours'
            let (mut lo, mut hi, mut none) = (f64::INFINITY, f64::NEG_INFINITY, false);
            for ny in y.saturating_sub(1)..(y + 2).min(h) {
                for nx in x.saturating_sub(1)..(x + 2).min(w) {
                    let t = truth[ny * w + nx];
                    if t.is_finite() {
                        lo = lo.min(t);
                        hi = hi.max(t);
                    } else {
                        none = true;
                    }
                }
            }
            if if d.is_finite() { d >= lo - tol && d <= hi + tol } else { none } {
                continue;
            }
            if d < e {
                out.solid += 1;
                out.by_tile[(side[i].max(1) as f64).log2().round().min(4.0) as usize] += 1;
            } else {
                out.missing += 1;
            }
            let up = |at: usize| square[at];
            let m = if x > 0 && y > 0 { up(i - 1).min(up(i - w)).min(up(i - w - 1)) } else { 0 };
            square[i] = m + 1;
            out.patch = out.patch.max(square[i] as usize);
        }
    }
    out
}

/// The view from an angle with the part `across` pixels high, around its middle.
fn orbit_at(at: V, size: f64, az: f64, el: f64, across: f64, w: f64, h: f64) -> Cam {
    let half = 20f64.to_radians().tan();
    let dist = size / (across / h * 2.0 * half);
    let (a, e) = (az.to_radians(), el.to_radians());
    let eye = add(at, [a.sin() * e.cos() * dist, e.sin() * dist, a.cos() * e.cos() * dist]);
    let f = norm(sub(at, eye));
    let r = norm(cross(f, [0.0, 1.0, 0.0]));
    Cam { eye, f, r, u: cross(r, f), half: [half * w / h, half], off: [0.0; 2], ortho: false, w, h }
}

/// Fab's demo vase: an ellipsoid's belly and a neck blended, hollowed to a thin wall.
fn vase_native() -> SdfNode {
    let at = |at: [f64; 3], of: SdfNode| SdfNode::Transform { at, rotate: [0.0; 3], scale: 1.0, of: Box::new(of) };
    let (height, belly, neck, wall) = (70.0, 21.0, 9.5, 1.6);
    let body = SdfNode::Combine {
        op: SdfOp::Union,
        blend: 9.0,
        of: vec![at([0.0, height * 0.34, 0.0], SdfNode::Ellipsoid { radii: [belly, height * 0.36, belly] }), at([0.0, height * 0.74, 0.0], SdfNode::Cylinder { radius: neck, top_radius: neck, height: height * 0.42 })],
    };
    let footed = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![body, at([0.0, -7.0, 0.0], SdfNode::Box { size: [4.0 * belly, 20.0, 4.0 * belly], radius: 0.0 })] };
    let hollow = SdfNode::Shell { thickness: wall, of: Box::new(footed) };
    let rimmed = SdfNode::Combine { op: SdfOp::Union, blend: 1.5, of: vec![hollow, at([0.0, height * 0.94, 0.0], SdfNode::Torus { radius: neck + 1.2, tube: 1.8 })] };
    let (low, high) = (height * 0.87 - wall, height + 20.0);
    SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![rimmed, at([0.0, (low + high) / 2.0, 0.0], SdfNode::Cylinder { radius: neck - wall, top_radius: neck - wall, height: high - low })] }
}

/// From any distance, the finished tiles show what a ray of each pixel finds: a tile above the leaf
/// has nothing wrong under it, however its samples agree, and no wall is lost.
#[test]
fn finished_tiles_show_what_every_pixel_finds() {
    let pool = TaskPool::closed();
    let (default, dense, native, vase) = (lattice(24.0, 8.0), lattice(24.0, 3.5), wavy_native(), vase_native());
    let (raw_lip, kernel_lip) = (1.7321, 3.05);
    for (name, root, lip, az, el, across) in [("cube", &default, kernel_lip, 25.0, 20.0, 120.0), ("cube", &default, kernel_lip, 40.0, 10.0, 190.0), ("small holes", &dense, kernel_lip, 25.0, 20.0, 120.0), ("raw gyroid", &native, raw_lip, 25.0, 20.0, 120.0), ("vase", &vase, 1.3, 30.0, 15.0, 300.0), ("vase from above", &vase, 1.3, 30.0, 65.0, 300.0)] {
        let f = Field::new(root, false, &pool).unwrap();
        let size = (0..3).map(|i| f.hi[i] - f.lo[i]).fold(0.0, f64::max);
        let cam = orbit_at(mul(add(f.lo, f.hi), 0.5), size, az, el, across, 320.0, 320.0);
        let at: Vec<(f64, f64)> = (0..320 * 320).map(|i| ((i % 320) as f64 + 0.5, (i / 320) as f64 + 0.5)).collect();
        let truth = exact(root, f.lo, f.hi, &cam, lip, &at);
        // the reference against a walk too short to pass anything, on a few rays
        let few: Vec<(f64, f64)> = at.iter().copied().step_by(97).collect();
        let walked = exact(root, f.lo, f.hi, &cam, lip * 8.0, &few);
        let off = truth.iter().step_by(97).zip(&walked).filter(|(a, b)| if a.is_finite() && b.is_finite() { (*a - *b).abs() > cam.pw(**a) } else { a.is_finite() != b.is_finite() }).count();
        assert!(off <= 1, "{name}: the reference is off a finer walk on {off} of {} rays", few.len());
        let (fp, pass, e0) = pass_to_the_end(root, cam, true);
        let (depth, side) = draw_tiles(&pass);
        let wr = wrong_pixels(&cam, &truth, &depth, &side);
        eprintln!("{name} {across} px: {} covered, {} solid, {} missing, patch {}, by side {:?}, {} tiles, {} evals", wr.covered, wr.solid, wr.missing, wr.patch, wr.by_tile, pass.alive.iter().filter(|a| **a).count(), fp.evals.load(Ordering::Relaxed) - e0);
        assert!(wr.covered > 5000, "{name}: the part is on screen");
        assert_eq!(wr.by_tile[1..].iter().sum::<usize>(), 0, "{name} {across} px: a tile above the leaf has wrong pixels: {wr:?}");
        assert_eq!(wr.missing, 0, "{name} {across} px: a wall is lost: {wr:?}");
        assert!(wr.solid * 200 <= wr.covered && wr.patch <= 2, "{name} {across} px: {wr:?}");
    }
}

/// A thick solid's faces are proven, so they stay in big tiles: the rule costs what it must.
#[test]
fn a_solid_keeps_big_tiles() {
    let root = SdfNode::Box { size: [24.0; 3], radius: 0.0 };
    let cam = orbit_at([0.0; 3], 24.0, 25.0, 20.0, 200.0, 320.0, 320.0);
    let (_, pass, _) = pass_to_the_end(&root, cam, true);
    let (depth, _) = draw_tiles(&pass);
    let (tiles, covered) = (pass.alive.iter().filter(|a| **a).count(), depth.iter().filter(|d| d.is_finite()).count());
    eprintln!("solid: {covered} covered, {tiles} tiles");
    assert!(tiles * 2 < covered, "{tiles} tiles over {covered} pixels");
}

/// The vase the app showed a speck on: an ellipsoid's belly and a neck blended, a flat foot cut,
/// hollowed to a 0.8 wall, a lip on its rim, open at the top, and stretched to about twice its
/// width along one axis, as a drag of its scale left it.
pub(super) fn thin_vase_stretched() -> SdfNode {
    let at = |p: V, of: SdfNode| SdfNode::Transform { at: p, rotate: [0.0; 3], scale: 1.0, of: Box::new(of) };
    let body = SdfNode::Combine {
        op: SdfOp::Union,
        blend: 9.0,
        of: vec![at([0.0, 23.8, 0.0], SdfNode::Ellipsoid { radii: [21.0, 25.2, 21.0] }), at([0.0, 51.8, 0.0], SdfNode::Cylinder { radius: 9.5, top_radius: 9.5, height: 29.4 })],
    };
    let footed = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![body, at([0.0, -7.0, 0.0], SdfNode::Box { size: [84.0, 20.0, 84.0], radius: 0.0 })] };
    let hollow = SdfNode::Shell { thickness: 0.8, of: Box::new(SdfNode::Round { radius: -0.8, of: Box::new(footed) }) };
    let rimmed = SdfNode::Combine { op: SdfOp::Union, blend: 1.5, of: vec![hollow, at([0.0, 65.8, 0.0], SdfNode::Torus { radius: 10.7, tube: 1.8 })] };
    let vase = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![rimmed, at([0.0, 74.65, 0.0], SdfNode::Cylinder { radius: 7.9, top_radius: 7.9, height: 30.7 })] };
    let stretch = makepad_csg_sdf::SdfDeform::Stretch { scale: [1.9988499137435307, 1.0, 1.0] };
    let lipschitz = stretch.lipschitz(None);
    SdfNode::Deform { deform: stretch, lipschitz, of: Box::new(vase) }
}

/// The app's view of [`thin_vase_stretched`], from above and in front, in the part's frame.
pub(super) fn app_view() -> Cam {
    Cam {
        eye: [326.3891906738281, 306.3897399902344, 461.6325988769531],
        f: [-0.6005875242807023, -0.4404308582151912, -0.6673194773196771],
        r: [0.7432941726356009, 1.8796674570404753e-08, -0.6689647023019651],
        u: [-0.2946327056259051, 0.8977865284053859, -0.32736969650437137],
        half: [0.2254442418543611, 0.36397021943192825],
        off: [0.0; 2],
        ortho: false,
        w: 581.0,
        h: 938.0,
    }
}

/// Each pixel's ray, along it, to what it meets first (infinite: nothing).
fn exact_view(root: &SdfNode, cam: &Cam, lip: f64) -> Vec<f64> {
    let (lo, hi) = makepad_csg_sdf::node_bounds(root).unwrap();
    let (w, h) = (cam.w as usize, cam.h as usize);
    let px: Vec<(f64, f64)> = (0..w * h).map(|i| ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5)).collect();
    exact(root, lo, hi, cam, lip, &px)
}

/// The pixels more than `edge` pixels from the shape's edges whose drawn depth along the ray
/// (`drawn`, infinite: nothing drawn) is a surface a wall in front of it hides: past the depth the
/// ray meets first, the ray goes into the wall, out of it, and comes to a surface again by the
/// depth drawn. Within `edge` of an edge, a coarser look's edge may be that far off. How many
/// pixels were asked, and those.
fn hidden(root: &SdfNode, cam: &Cam, truth: &[f64], drawn: &[f64], edge: usize) -> (usize, Vec<(usize, usize)>) {
    let (w, h) = (cam.w as usize, cam.h as usize);
    // an edge: a pixel that meets nothing, or a neighbour a jump away, more than 4 pixels' worth
    // nearer or further, as the tracer takes a jump
    let edges: Vec<bool> = (0..w * h).map(|k| {
        let (x, y, t0) = (k % w, k / w, truth[k]);
        let jump = |i: usize, j: usize| !((truth[j * w + i] - t0).abs() <= cam.pw(t0) * 4.0);
        !t0.is_finite() || (x > 0 && jump(x - 1, y)) || (x + 1 < w && jump(x + 1, y)) || (y > 0 && jump(x, y - 1)) || (y + 1 < h && jump(x, y + 1))
    }).collect();
    let (mut met, mut out) = (0, Vec::new());
    for y in edge..h - edge {
        for x in edge..w - edge {
            let t0 = truth[y * w + x];
            let pw = cam.pw(t0);
            if (y - edge..y + edge + 1).any(|j| (x - edge..x + edge + 1).any(|i| edges[j * w + i])) {
                continue;
            }
            met += 1;
            let t1 = drawn[y * w + x];
            if !t1.is_finite() || t1 - t0 <= pw {
                continue;
            }
            let (o, d) = cam.ray(x as f64 + 0.5, y as f64 + 0.5);
            let end = t1 + pw * 2.0;
            let n = ((end - t0) / (pw * 0.25)).ceil() as usize;
            let pts: Vec<V> = (1..=n).map(|k| add(o, mul(d, t0 + (end - t0) * k as f64 / n as f64))).collect();
            let (mut inside, mut left) = (false, false);
            for v in raw(root, &pts) {
                if !inside {
                    inside = v < -pw * 0.25;
                } else if !left {
                    left = v > pw * 0.25;
                } else if v < pw * 0.25 {
                    out.push((x, y));
                    break;
                }
            }
        }
    }
    (met, out)
}

/// Every mesh a look can put on screen of the vase the app showed a speck on, from the app's view
/// and a hair beside it, from nothing and after another look: none shows a surface the thin wall
/// in front of it hides. A look goes out when it is done; on its grid of four pixels a moving look
/// left a hole in the wall between its samples (19 pixels of the inside at the lip from the app's
/// view, where the app showed its speck) until its last stage asked each pixel in a gap in what
/// is nearer.
#[test]
fn no_look_shows_what_a_thin_wall_hides() {
    use crate::surface::{Cover, Gaze, Surface};
    let pool = TaskPool::closed();
    let root = thin_vase_stretched();
    let f = Field::new(&root, false, &pool).unwrap();
    let app = app_view();
    let beside = Cam { eye: add(app.eye, mul(app.r, 0.6)), ..app };
    let check = |name: &str, s: &Surface, cam: &Cam, truth: &[f64], tol: &Tolerance| {
        let mut cover = Cover::default();
        cover.fill(cam, 1.0, s, &pool);
        let w = cam.w as usize;
        let drawn: Vec<f64> = (0..truth.len()).map(|i| {
            let z = cover.at(i % w, i / w) as f64;
            if z.is_finite() { cam.along(cam.ray((i % w) as f64 + 0.5, (i / w) as f64 + 0.5).1, z) } else { f64::INFINITY }
        }).collect();
        let (met, bad) = hidden(&root, cam, truth, &drawn, tol.leaf as usize);
        eprintln!("{name}: {} of {met} pixels show what the wall hides, {} quads", bad.len(), s.held().quads);
        assert!(met > 10_000, "{name}: {met} pixels");
        assert!(bad.is_empty(), "{name}: {} pixels show what the wall hides, at {:?}", bad.len(), &bad[..bad.len().min(8)]);
    };
    let truth = [exact_view(&root, &app, 2.0), exact_view(&root, &beside, 2.0)];
    let look = |s: &mut Surface, g: &mut Gaze, cam: Cam, tol: &Tolerance| {
        let mut rounds = 0;
        while !g.step(s, &f, &pool, cam, tol, &|| false).unwrap().done {
            rounds += 1;
            assert!(rounds < 200, "a look that never ends");
        }
    };
    for (first, then) in [(0, 1), (1, 0)] {
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        let cams = [app, beside];
        for (k, tol, tn) in [(first, MOVING, "moving"), (first, STILL, "still"), (then, MOVING, "moving"), (then, STILL, "still")] {
            look(&mut s, &mut g, cams[k], &tol);
            check(&format!("{} after {}: {tn}", ["the app's view", "beside it"][k], ["the app's view", "beside it"][first]), &s, &cams[k], &truth[k], &tol);
        }
    }
}

/// A drag's version of view tiles at each size it can go to, of the vase as it is and stretched,
/// from round about it: none shows a surface a wall in front of it hides. A tile that splits
/// stands for its pieces in a coarser version; over a jump in depth it showed what its centre met,
/// behind the wall its corners met: 79 pixels of the plain vase's version to 16 px.
#[test]
fn no_version_shows_what_a_wall_hides() {
    let pool = TaskPool::closed();
    let vase = vase_native();
    let stretch = makepad_csg_sdf::SdfDeform::Stretch { scale: [2.0, 1.0, 1.3] };
    let lipschitz = stretch.lipschitz(None);
    let stretched = SdfNode::Deform { deform: stretch, lipschitz, of: Box::new(vase.clone()) };
    let app = thin_vase_stretched();
    let mut views: Vec<(&str, &SdfNode, Cam)> = vec![("the app's", &app, app_view())];
    for (name, root) in [("plain", &vase), ("stretched", &stretched)] {
        let (lo, hi) = makepad_csg_sdf::node_bounds(root).unwrap();
        let size = (0..3).map(|i| hi[i] - lo[i]).fold(0.0, f64::max);
        for (az, el, across) in [(30.0, 15.0, 300.0), (30.0, 65.0, 300.0), (120.0, 35.0, 450.0), (200.0, 50.0, 600.0), (10.0, 80.0, 250.0)] {
            views.push((name, root, orbit_at(mul(add(lo, hi), 0.5), size, az, el, across, 640.0, 640.0)));
        }
    }
    for (name, root, cam) in views {
        let f = Field::new(root, false, &pool).unwrap();
        let truth = exact_view(root, &cam, 2.0);
        for levels in 1..=DRAG_LEVELS {
            let mut grid = Grid::default();
            let mut pass = Pass::new(cam, &mut grid);
            pass.version(&f, &pool, &mut grid, levels, MOVING, &|| false).unwrap();
            let (drawn, _) = draw_tiles(&pass);
            let (met, bad) = hidden(root, &cam, &truth, &drawn, 2);
            let px = BASE >> (levels - 1);
            assert!(met > 5_000, "{name} vase, {px} px: {met} pixels");
            assert!(bad.is_empty(), "{name} vase, a version to {px} px: {} pixels show what a wall hides, at {:?}", bad.len(), &bad[..bad.len().min(8)]);
        }
    }
}

/// The surface's mesh drawn from `cam`: the pixels asked, and those that show what a wall hides.
fn shows_hidden(root: &SdfNode, s: &crate::surface::Surface, cam: &Cam, truth: &[f64], edge: usize, pool: &TaskPool) -> (usize, Vec<(usize, usize)>) {
    let mut cover = crate::surface::Cover::default();
    cover.fill(cam, 1.0, s, pool);
    let w = cam.w as usize;
    let drawn: Vec<f64> = (0..truth.len()).map(|i| {
        let z = cover.at(i % w, i / w) as f64;
        if z.is_finite() { cam.along(cam.ray((i % w) as f64 + 0.5, (i / w) as f64 + 0.5).1, z) } else { f64::INFINITY }
    }).collect();
    hidden(root, cam, truth, &drawn, edge)
}

/// A look's finished mesh stays on screen as an orbit goes on, until the next look is done: drawn
/// from a view a small turn on, it shows no surface a wall in front of it hides. The wall's side,
/// just past edge-on to the look, comes into view first, and no ray of the look can land on it. A
/// turn of several degrees can still show what is behind a side the look did not walk.
#[test]
fn a_small_orbit_step_shows_nothing_a_wall_hides() {
    use crate::surface::{Gaze, Surface};
    let pool = TaskPool::closed();
    for (name, root, looks) in [("plain", vase_native(), 5), ("thin", thin_vase_stretched(), 4)] {
        let f = Field::new(&root, false, &pool).unwrap();
        let (lo, hi) = makepad_csg_sdf::node_bounds(&root).unwrap();
        let (mid, size) = (mul(add(lo, hi), 0.5), (0..3).map(|i| hi[i] - lo[i]).fold(0.0, f64::max));
        let view = |az: f64| orbit_at(mid, size, az, 80.0, 500.0, 640.0, 640.0);
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        for k in 0..looks {
            let az = 20.0 + k as f64 * 40.0;
            let mut tolerances = vec![&MOVING];
            if k % 2 == 1 {
                tolerances.push(&STILL);
            }
            for tol in tolerances {
                let mut rounds = 0;
                while !g.step(&mut s, &f, &pool, view(az), tol, &|| false).unwrap().done {
                    rounds += 1;
                    assert!(rounds < 200, "a look that never ends");
                }
            }
            for d in [0.0, 0.5, 2.0] {
                let cam = view(az + d);
                let truth = exact_view(&root, &cam, 2.0);
                let (met, bad) = shows_hidden(&root, &s, &cam, &truth, 2, &pool);
                assert!(met > 0, "{name} az {az} +{d}: no pixel asked");
                assert!(bad.is_empty(), "{name} az {az} +{d}: {} of {met} pixels show what a wall hides, at {:?}", bad.len(), &bad[..bad.len().min(8)]);
            }
        }
    }
}

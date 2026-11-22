//! A capture session reconstructed and measured: against the object's model when one is given, else by the size of
//! what stands on the table. The output is numbers, and the object's points alone (the model's frame, or the
//! frame of the box around it; millimetres) go to `<out>/object_points.ply`.
//!
//! usage: scan-session [--every <n>] [--coarse-levels <n>] [--rounds <n>] [--sources <n>] <session> [<model.stl>] <out>
//!
//! Each `--every`th keyframe; PatchMatch on the GPU (no starting depth) against each view's nearest
//! `--sources`, fusion, the fit; between rounds the poses are refined on the CPU, first on the views
//! downsampled by 2^`--coarse-levels`, ... 2. `FIT tag=prior` (`SIZE` with no model) is what the tracker's poses give.

use makepad_ai_geometry::alignment::{refine_poses_among, AlignParams, Alignment, PosedMaps};
use makepad_ai_geometry::cuda::patchmatch_cuda;
use makepad_ai_geometry::fusion::{fuse_view, ConsistencyParams};
use makepad_ai_geometry::patchmatch::{DepthNormalMap, PatchMatchParams};
use makepad_ai_geometry::views::GrayView;
use makepad_fab_scan::fit::{fit_object, measure_object, Mesh, ObjectFit};
use makepad_fab_scan::session::{looked_at, neighbours, read_session, SessionView};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

const DEPTH_MIN: f64 = 0.15;
const DEPTH_MAX: f64 = 2.0;
/// How far from what was looked at a fused point stays in memory: the room beyond it never does.
const KEPT_REACH: f64 = 0.5;
const FEWEST_SOURCES: usize = 2;
/// A round that moves no pose by more than these (at full size) ends the refinement.
const SETTLED_MM: f64 = 0.05;
const SETTLED_DEG: f64 = 0.01;

fn threads(count: usize) -> impl Fn(usize, &(dyn Fn(usize) + Sync)) + Sync {
    move |len, f| {
        let next = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..count {
                scope.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= len {
                        break;
                    }
                    f(i);
                });
            }
        });
    }
}

fn centre_of(m: &[[f64; 4]; 4]) -> [f64; 3] {
    [m[0][3], m[1][3], m[2][3]]
}

/// The rotation taking `b` to `a`, as an axis times its angle, in the world.
fn rotation_between(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> [f64; 3] {
    let product = |i: usize, j: usize| (0..3).map(|k| a[i][k] * b[j][k]).sum::<f64>();
    let trace = product(0, 0) + product(1, 1) + product(2, 2);
    let angle = ((trace - 1.0) / 2.0).clamp(-1.0, 1.0).acos();
    let factor = if angle < 1e-9 { 0.5 } else { angle / (2.0 * angle.sin()) };
    [product(2, 1) - product(1, 2), product(0, 2) - product(2, 0), product(1, 0) - product(0, 1)].map(|v| v * factor)
}

fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// p50, p95 and the largest.
fn spread(mut values: Vec<f64>) -> (f64, f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    values.sort_by(f64::total_cmp);
    let at = |q: f64| values[(q * (values.len() - 1) as f64).round() as usize];
    (at(0.5), at(0.95), at(1.0))
}

/// How far each pose is from where it was, and from its prior.
fn motion_fields(tag: &str, poses: &[[[f64; 4]; 4]], from: &[[[f64; 4]; 4]]) -> String {
    let moved = spread(poses.iter().zip(from).map(|(a, b)| 1000.0 * norm(std::array::from_fn(|i| centre_of(a)[i] - centre_of(b)[i]))).collect());
    let turned = spread(poses.iter().zip(from).map(|(a, b)| norm(rotation_between(a, b)).to_degrees()).collect());
    format!("{tag}_mm={:.3}/{:.3}/{:.3} {tag}_deg={:.4}/{:.4}/{:.4}", moved.0, moved.1, moved.2, turned.0, turned.1, turned.2)
}

/// What a clock error of the camera against the tracker would look like: the refined poses' moves
/// from the priors, regressed on the head's velocity (ms, per position and per rotation), and the
/// rig's baseline against the calibrated one.
fn timing_fields(poses: &[[[f64; 4]; 4]], priors: &[[[f64; 4]; 4]], session: &[SessionView]) -> String {
    let count = poses.len().max(1) as f64;
    let moves: Vec<[f64; 3]> = poses.iter().zip(priors).map(|(a, b)| std::array::from_fn(|i| centre_of(a)[i] - centre_of(b)[i])).collect();
    let mean: [f64; 3] = std::array::from_fn(|i| moves.iter().map(|m| m[i]).sum::<f64>() / count);
    let regress = |pairs: &mut dyn Iterator<Item = ([f64; 3], [f64; 3])>| {
        let (mut along, mut power) = (0.0, 0.0);
        for (change, rate) in pairs {
            along += (0..3).map(|i| change[i] * rate[i]).sum::<f64>();
            power += (0..3).map(|i| rate[i] * rate[i]).sum::<f64>();
        }
        if power > 0.0 { 1000.0 * along / power } else { 0.0 }
    };
    let by_position = regress(&mut moves.iter().zip(session).map(|(m, s)| (std::array::from_fn(|i| m[i] - mean[i]), s.velocity)));
    let by_rotation = regress(&mut poses.iter().zip(priors).zip(session).map(|((a, b), s)| (rotation_between(a, b), s.angular_velocity)));
    let baseline = |list: &[[[f64; 4]; 4]]| {
        spread(session.windows(2).zip(list.windows(2))
            .filter(|(s, _)| s[0].keyframe == s[1].keyframe)
            .map(|(_, p)| 1000.0 * norm(std::array::from_fn(|i| centre_of(&p[0])[i] - centre_of(&p[1])[i])))
            .collect()).0
    };
    format!("time_offset_ms={by_position:.2} time_offset_rot_ms={by_rotation:.2} baseline_mm={:.3} prior_baseline_mm={:.3}", baseline(poses), baseline(priors))
}

fn match_views(views: &[GrayView], sources: &[Vec<usize>], params: &PatchMatchParams) -> Result<Vec<DepthNormalMap>, String> {
    (0..views.len()).map(|r| patchmatch_cuda(views, r, &sources[r], None, params)).collect()
}

fn map_fields(maps: &[DepthNormalMap], max_cost: f64) -> String {
    let (mut kept, mut total, mut cost) = (0usize, 0usize, 0.0);
    for map in maps {
        for &c in &map.cost {
            total += 1;
            if c <= max_cost {
                kept += 1;
                cost += c;
            }
        }
    }
    format!("kept_share={:.3} kept_cost={:.4}", kept as f64 / total.max(1) as f64, cost / kept.max(1) as f64)
}

/// The fused points of every view within [`KEPT_REACH`] of `around`.
fn fused(views: &[GrayView], maps: &[DepthNormalMap], sources: &[Vec<usize>], around: [f64; 3], consistency: &ConsistencyParams) -> Result<Vec<[f64; 3]>, String> {
    type Part = Mutex<Option<Result<Vec<[f64; 3]>, String>>>;
    let parts: Vec<Part> = views.iter().map(|_| Mutex::new(None)).collect();
    let fan_out = threads(std::thread::available_parallelism().map_or(1, |n| n.get()));
    fan_out(views.len(), &|r| {
        let part = fuse_view(views, maps, r, &sources[r], consistency).map(|map| {
            map.points
                .into_iter()
                .flatten()
                .filter(|p| norm(std::array::from_fn(|i| p[i] - around[i])) < KEPT_REACH)
                .collect()
        });
        *parts[r].lock().unwrap() = Some(part);
    });
    let mut points = Vec::new();
    for part in parts {
        points.extend(part.into_inner().unwrap().ok_or("a view was not fused")??);
    }
    Ok(points)
}

fn distance_fields(tag: &str, d: &makepad_fab_scan::fit::Distances) -> String {
    format!("{tag}p50_mm={:.3} {tag}p95_mm={:.3} {tag}mean_mm={:.3} {tag}signed_mm={:.3}", d.p50_mm, d.p95_mm, d.mean_mm, d.signed_mm)
}

fn write_ply(path: &std::path::Path, points: &[[f32; 3]]) -> std::io::Result<()> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(out, "ply\nformat binary_little_endian 1.0\nelement vertex {}\nproperty float x\nproperty float y\nproperty float z\nend_header\n", points.len())?;
    for point in points {
        for v in point {
            out.write_all(&(v * 1000.0).to_le_bytes())?;
        }
    }
    out.flush()
}

struct Run<'a> {
    views: Vec<GrayView>,
    sources: &'a [Vec<usize>],
    priors: &'a [[[f64; 4]; 4]],
    session: &'a [SessionView],
    around: [f64; 3],
    mesh: Option<&'a Mesh>,
    window: PatchMatchParams,
    consistency: ConsistencyParams,
    align: AlignParams,
    out: PathBuf,
}

impl Run<'_> {
    fn poses(&self) -> Vec<[[f64; 4]; 4]> {
        self.views.iter().map(|v| v.cam_to_world).collect()
    }

    /// The views as they stand, matched and fused, the fit at them, the object's points kept.
    fn measure(&self, tag: &str) -> Result<Vec<DepthNormalMap>, Box<dyn std::error::Error>> {
        let start = Instant::now();
        let maps = match_views(&self.views, self.sources, &self.window)?;
        let pm_s = start.elapsed().as_secs_f64();
        let start = Instant::now();
        let points = fused(&self.views, &maps, self.sources, self.around, &self.consistency)?;
        let fuse_s = start.elapsed().as_secs_f64();
        let start = Instant::now();
        let motion = motion_fields("from_prior", &self.poses(), self.priors);
        let timing = timing_fields(&self.poses(), self.priors, self.session);
        let Some(mesh) = self.mesh else {
            match measure_object(&points, self.around) {
                Ok(size) => {
                    println!(
                        "SIZE tag={tag} {} fused_near={} table_points={} object_points={} size_mm={:.2}/{:.2}/{:.2} {motion} {timing} pm_s={pm_s:.1} fuse_s={fuse_s:.1} size_s={:.1}",
                        map_fields(&maps, self.consistency.max_cost), points.len(), size.table.points, size.object_points, size.size_mm[0], size.size_mm[1], size.size_mm[2], start.elapsed().as_secs_f64(),
                    );
                    write_ply(&self.out.join("object_points.ply"), &size.points)?;
                }
                Err(e) => println!("SIZE tag={tag} {} fused_near={} no_size=\"{e}\" {motion} {timing} pm_s={pm_s:.1} fuse_s={fuse_s:.1}", map_fields(&maps, self.consistency.max_cost), points.len()),
            }
            std::io::stdout().flush()?;
            return Ok(maps);
        };
        let fit = fit_object(&points, self.around, mesh);
        let fit_s = start.elapsed().as_secs_f64();
        match fit {
            Ok(fit) => {
                let ObjectFit { table, object_points, yaw_deg, all, near, near_share, coverage_1mm, scale_ppm, scaled, model_points } = &fit;
                println!(
                    "FIT tag={tag} {} fused_near={} table_points={} object_points={object_points} yaw_deg={yaw_deg:.1} {} near_share={near_share:.3} {} coverage_1mm={coverage_1mm:.3} scale_ppm={scale_ppm:.0} {} {motion} {timing} pm_s={pm_s:.1} fuse_s={fuse_s:.1} fit_s={fit_s:.1}",
                    map_fields(&maps, self.consistency.max_cost), points.len(), table.points, distance_fields("", all), distance_fields("near_", near), distance_fields("scaled_", scaled),
                );
                write_ply(&self.out.join("object_points.ply"), model_points)?;
            }
            Err(e) => println!("FIT tag={tag} {} fused_near={} no_fit=\"{e}\" {motion} {timing} pm_s={pm_s:.1} fuse_s={fuse_s:.1}", map_fields(&maps, self.consistency.max_cost), points.len()),
        }
        std::io::stdout().flush()?;
        Ok(maps)
    }

    /// One refinement of `views` (the run's, or downsampled) from `maps`. Returns its fields and
    /// how far it moved a pose, millimetres and degrees.
    fn refine(&self, views: &[GrayView], maps: &[DepthNormalMap], align: &AlignParams) -> Result<(Alignment, f64, f64, f64), String> {
        let start = Instant::now();
        let fan_out = threads(std::thread::available_parallelism().map_or(1, |n| n.get()));
        let input = PosedMaps { views, maps, sources: self.sources };
        let alignment = refine_poses_among(&input, 0, self.priors, &self.window, align, &fan_out)?;
        let moved = alignment.cam_to_world.iter().zip(views).map(|(a, v)| 1000.0 * norm(std::array::from_fn(|i| centre_of(a)[i] - centre_of(&v.cam_to_world)[i]))).fold(0.0, f64::max);
        let turned = alignment.cam_to_world.iter().zip(views).map(|(a, v)| norm(rotation_between(a, &v.cam_to_world)).to_degrees()).fold(0.0, f64::max);
        Ok((alignment, moved, turned, start.elapsed().as_secs_f64()))
    }

    fn align_line(&self, tag: &str, a: &Alignment, moved: f64, turned: f64, seconds: f64) {
        println!(
            "ALIGN tag={tag} steps={} selections={} converged={} windows={} start_cost={:.5} cost={:.5} moved_mm={moved:.3} moved_deg={turned:.4} align_s={seconds:.1}",
            a.steps, a.selections, a.converged, a.windows, a.start_cost, a.cost,
        );
    }

    fn coarse(&mut self, level: usize, rounds: usize) -> Result<(), Box<dyn std::error::Error>> {
        let factor = 1 << level;
        let align = AlignParams { stride: (self.align.stride >> level).max(2), ..self.align.downsampled(factor) };
        for round in 0..rounds {
            let small = self.views.iter().map(|v| v.downsampled(factor)).collect::<Result<Vec<_>, _>>()?;
            let start = Instant::now();
            let maps = match_views(&small, self.sources, &self.window)?;
            let pm_s = start.elapsed().as_secs_f64();
            let (alignment, moved, turned, seconds) = self.refine(&small, &maps, &align)?;
            println!("COARSE level={level} round={round} {} pm_s={pm_s:.1}", map_fields(&maps, self.consistency.max_cost));
            self.align_line(&format!("level={level},round={round}"), &alignment, moved, turned, seconds);
            for (view, pose) in self.views.iter_mut().zip(alignment.cam_to_world) {
                view.cam_to_world = pose;
            }
            if moved <= SETTLED_MM * factor as f64 && turned <= SETTLED_DEG * factor as f64 {
                break;
            }
        }
        Ok(())
    }
}

fn option<T: std::str::FromStr>(args: &mut impl Iterator<Item = String>, usage: &str) -> Result<T, String> {
    args.next().and_then(|v| v.parse().ok()).ok_or_else(|| usage.to_string())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let usage = "usage: scan-session [--every <n>] [--coarse-levels <n>] [--rounds <n>] [--sources <n>] <session> [<model.stl>] <out>";
    let mut args = std::env::args().skip(1);
    let (mut every, mut coarse_levels, mut rounds, mut source_count) = (2_usize, 2_usize, 3_usize, 8_usize);
    let mut paths = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--every" => every = option(&mut args, usage)?,
            "--coarse-levels" => coarse_levels = option(&mut args, usage)?,
            "--rounds" => rounds = option(&mut args, usage)?,
            "--sources" => source_count = option(&mut args, usage)?,
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    let (folder, model, out) = match <[PathBuf; 3]>::try_from(paths) {
        Ok([folder, model, out]) => (folder, Some(model), out),
        Err(paths) => {
            let [folder, out] = <[PathBuf; 2]>::try_from(paths).map_err(|_| usage.to_string())?;
            (folder, None, out)
        }
    };
    std::fs::create_dir_all(&out)?;
    let mesh = match model {
        Some(model) => Some(Mesh::from_stl(&std::fs::read(&model)?)?),
        None => None,
    };
    let read = read_session(&folder, every)?;
    let (keyframes, unreadable) = (read.keyframes, read.unreadable);
    // Views that see what was looked at with too few others take no part.
    let mut session = read.views;
    let (around, sources) = loop {
        let views: Vec<&GrayView> = session.iter().map(|s| &s.view).collect();
        let copies: Vec<GrayView> = views.iter().map(|v| GrayView { pixels: Vec::new(), ..**v }).collect();
        let around = looked_at(&copies)?;
        let sources = neighbours(&copies, around, source_count);
        if sources.iter().all(|list| list.len() >= FEWEST_SOURCES) {
            break (around, sources);
        }
        let mut lists = sources.iter();
        session.retain(|_| lists.next().is_some_and(|list| list.len() >= FEWEST_SOURCES));
        if session.len() < 3 {
            return Err("too few views see what was looked at".into());
        }
    };
    let views: Vec<GrayView> = session.iter().map(|s| GrayView { pixels: s.view.pixels.clone(), ..s.view }).collect();
    let priors: Vec<[[f64; 4]; 4]> = views.iter().map(|v| v.cam_to_world).collect();
    let (exposure, skew) = (spread(session.iter().map(|s| f64::from(s.exposure_ns) / 1e6).collect()).0, spread(session.iter().map(|s| f64::from(s.rolling_skew_ns) / 1e6).collect()).0);
    let speed = spread(session.iter().map(|s| 1000.0 * norm(s.velocity)).collect());
    let turn = spread(session.iter().map(|s| norm(s.angular_velocity).to_degrees()).collect());
    let distance = spread(views.iter().map(|v| 1000.0 * norm(std::array::from_fn(|i| centre_of(&v.cam_to_world)[i] - around[i]))).collect());
    println!(
        "SESSION keyframes={keyframes} unreadable={unreadable} views={} size={}x{} fx={:.2} exposure_ms={exposure:.2} rolling_skew_ms={skew:.2} speed_mm_s={:.0}/{:.0} turn_deg_s={:.1}/{:.1} object_distance_mm={:.0}/{:.0}",
        views.len(), views.first().map_or(0, |v| v.width), views.first().map_or(0, |v| v.height), views.first().map_or(0.0, |v| v.k[0]), speed.0, speed.1, turn.0, turn.1, distance.0, distance.1,
    );
    let window = PatchMatchParams { radius: 4, step: 1, top_k: 2, iterations: 12, depth_min: DEPTH_MIN, depth_max: DEPTH_MAX, ..Default::default() };
    let mut run = Run {
        views,
        sources: &sources,
        priors: &priors,
        session: &session,
        around,
        mesh: mesh.as_ref(),
        window,
        consistency: ConsistencyParams::default(),
        align: AlignParams { stride: 16, ..Default::default() },
        out,
    };
    run.measure("prior")?;
    for level in (1..=coarse_levels).rev() {
        run.coarse(level, 3)?;
    }
    for round in 0..rounds {
        let maps = run.measure(&format!("round={round}"))?;
        if round + 1 == rounds {
            break;
        }
        let (alignment, moved, turned, seconds) = run.refine(&run.views, &maps, &run.align)?;
        run.align_line(&format!("round={round}"), &alignment, moved, turned, seconds);
        for (view, pose) in run.views.iter_mut().zip(alignment.cam_to_world) {
            view.cam_to_world = pose;
        }
        if moved <= SETTLED_MM && turned <= SETTLED_DEG {
            run.measure("settled")?;
            break;
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("scan-session: {e}");
        std::process::exit(1);
    }
}

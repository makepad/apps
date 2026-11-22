//! The PatchMatch program on this machine's GPU, as the scan's detail runs it: against the CPU
//! device on a small made-up stereo pair, then timed at a keyframe's size from no start and from
//! a coarse one (the true depth pushed 8 mm off, as the headset's depth is), at several
//! iteration counts. The output is numbers.
//!
//! usage: scan-detail-check [--size <px>] [--repeats <n>]

use makepad_ai_geometry::patchmatch::{DepthNormalMap, PatchMatchParams};
use makepad_ai_geometry::program::patchmatch_program;
use makepad_ai_geometry::synthetic::textured_plane;
use makepad_ai_geometry::views::GrayView;
use makepad_fab_scan::detail::params_of;
use std::time::Instant;

/// The two cameras' distance apart, metres.
const APART: f64 = 0.064;
/// The coarse start's error, metres.
const COARSE_OFF: f64 = 0.008;
const MAX_COST: f64 = 0.3;

fn share(count: usize, of: usize) -> f64 {
    count as f64 / of.max(1) as f64
}

/// The coarse start: the truth pushed off, each normal facing its camera along its ray.
fn coarse_start(view: &GrayView, truth: &DepthNormalMap) -> DepthNormalMap {
    let normal = (0..view.width * view.height)
        .map(|i| {
            let ray = [((i % view.width) as f64 - view.k[2]) / view.k[0], ((i / view.width) as f64 - view.k[3]) / view.k[1], 1.0];
            let length = (ray[0] * ray[0] + ray[1] * ray[1] + 1.0).sqrt();
            ray.map(|v| -v / length)
        })
        .collect();
    DepthNormalMap { width: view.width, height: view.height, depth: truth.depth.iter().map(|d| d + COARSE_OFF).collect(), normal, cost: vec![2.0; truth.depth.len()] }
}

/// Of a map against the truth: the share of pixels kept (cost at most [`MAX_COST`]) and their
/// p95 depth error, millimetres.
fn against(map: &DepthNormalMap, truth: &DepthNormalMap) -> (f64, f64) {
    let mut errors: Vec<f64> = (0..map.depth.len()).filter(|&i| map.cost[i] <= MAX_COST).map(|i| 1000.0 * (map.depth[i] - truth.depth[i]).abs()).collect();
    errors.sort_by(f64::total_cmp);
    let p95 = errors.get((0.95 * errors.len().saturating_sub(1) as f64).round() as usize).copied().unwrap_or(0.0);
    (share(errors.len(), map.depth.len()), p95)
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (mut size, mut repeats) = (640_usize, 3_usize);
    while let Some(arg) = args.next() {
        let value = args.next().and_then(|v| v.parse().ok()).ok_or("usage: scan-detail-check [--size <px>] [--repeats <n>]")?;
        match arg.as_str() {
            "--size" => size = value,
            "--repeats" => repeats = value,
            _ => return Err("usage: scan-detail-check [--size <px>] [--repeats <n>]".into()),
        }
    }
    let gpu = makepad_compute_program_gpu::spawn().map_err(|e| format!("no GPU device: {e}"))?;
    // the GPU against the CPU device, where the CPU device is quick
    for degrees in [0.0_f64, 25.0] {
        let (views, truth) = textured_plane(96, 120.0, degrees.to_radians(), &[APART]);
        let params = PatchMatchParams { depth_min: 0.15, depth_max: 2.0, ..params_of(None) };
        let cpu = patchmatch_program(&views, 0, &[1], None, &params, None)?;
        let on_gpu = patchmatch_program(&views, 0, &[1], None, &params, Some(gpu.clone()))?;
        let both: Vec<usize> = (0..cpu.depth.len()).filter(|&i| cpu.cost[i] < 2.0 && on_gpu.cost[i] < 2.0).collect();
        let agree = share(both.iter().filter(|&&i| (on_gpu.depth[i] / cpu.depth[i] - 1.0).abs() <= 0.01).count(), both.len());
        let same = share(both.iter().filter(|&&i| on_gpu.depth[i] == cpu.depth[i]).count(), both.len());
        let (cpu_kept, cpu_p95) = against(&cpu, &truth);
        let (gpu_kept, gpu_p95) = against(&on_gpu, &truth);
        println!("PARITY degrees={degrees} pixels={} both_valid={} gpu_within_1pct_of_cpu={agree:.4} identical={same:.4} cpu_kept={cpu_kept:.3} cpu_p95_mm={cpu_p95:.2} gpu_kept={gpu_kept:.3} gpu_p95_mm={gpu_p95:.2}", cpu.depth.len(), both.len());
    }
    // a keyframe's pair, timed
    let focal = 0.68 * size as f64;
    let (views, truth) = textured_plane(size, focal, 20.0_f64.to_radians(), &[APART]);
    let start = coarse_start(&views[0], &truth);
    for (name, init) in [("none", None), ("coarse", Some(&start))] {
        for iterations in [2_usize, 4, 6, 12] {
            let params = PatchMatchParams { iterations, ..params_of(init) };
            let mut ms = Vec::new();
            let mut last = None;
            for _ in 0..repeats {
                let began = Instant::now();
                last = Some(patchmatch_program(&views, 0, &[1], init, &params, Some(gpu.clone()))?);
                ms.push(began.elapsed().as_secs_f64() * 1000.0);
            }
            let (kept, p95) = against(last.as_ref().ok_or("no repeats")?, &truth);
            let runs: Vec<String> = ms.iter().map(|v| format!("{v:.0}")).collect();
            ms.sort_by(f64::total_cmp);
            println!("TIME size={size} start={name} iterations={iterations} ms={} best_ms={:.0} kept={kept:.3} p95_mm={p95:.2}", runs.join("/"), ms[0]);
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("scan-detail-check: {e}");
        std::process::exit(1);
    }
}

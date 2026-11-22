//! The quick benchmark: one command, about a minute, the same before and after every step.
//! `cargo bench -p fab-gpuslice --bench quick` (timed numbers only on a quiet machine).
//! - PROBE: the GPU's own costs (a dispatch, a submission, bandwidth, ALU);
//! - CORE: the batch core's own costs (a scan, a pass sized by the GPU);
//! - SAMPLE: what one field sample of each of the demo plate's field parts costs;
//! - TRACE: the trace's stages per field part (batches of 64 layers, one submission each), with
//!   the dense reference and the signs checked against it and the exact field;
//! - then one line per stage of the demo plate: ms per plate, submissions, elements, bytes and
//!   the error against the exact field.
//! The plate is the app's demo plate at its defaults (the vase, the 24 mm lattice cube; the
//! bracket is a mesh part), sliced at 0.2 mm, classic walls.
use fab_gpuslice::batch::{Batch, CoreKernels, NONE, SCAN_MAX};
use fab_gpuslice::trace::{self, Caps, Opts, PartTrace, TraceKernels};
use fab_gpuslice::{field::{self, PartProgram}, gpu::Gpu, probe};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::gate::{self, GatePart};
use makepad_fab_slice::live::{Frame, PartField};
use makepad_fab_slicer::sdf::SdfSource;
use std::collections::BTreeMap;

fn setup() -> JobSetup {
    let mut process = Preset::default();
    for (k, v) in [("wall_generator", "classic"), ("layer_height", "0.2"), ("sparse_infill_density", "15%"), ("sparse_infill_pattern", "gyroid"), ("wall_loops", "2"), ("support_type", "tree(auto)")] {
        process.values.set_str(k, v);
    }
    JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default(); 2], filament_map: vec![1, 2], nozzle_volume_type: vec!["Standard".into(); 2], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None; 2] }
}

/// The demo plate's parts as the slicer takes them.
fn demo_parts() -> Vec<GatePart> {
    let scripts: BTreeMap<String, String> = [
        ("bracket.cad.splash", include_str!("../../app/resources/bracket.cad.splash")),
        ("vase.cad.splash", include_str!("../../app/resources/vase.cad.splash")),
        ("lattice_cube.cad.splash", include_str!("../../app/resources/lattice_cube.cad.splash")),
    ].into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let m = FabModel::load("quick", include_str!("../../app/resources/demo.fab.splash"), scripts).expect("the demo project loads");
    let mut e = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
    let ev = m.evaluate(&mut e, None).expect("the demo plate builds").1;
    let plate = ev.plate.printable();
    let frames: Vec<Frame> = ev.frames.iter().enumerate().map(|(i, f)| {
        let field = ev.fields.get(i).and_then(Option::as_ref).and_then(|pf| PartField::placed(pf.shape.clone(), pf.to_part).map(|f| f.with_bounds(pf.bounds.map(|b| (b.min, b.max)))));
        Frame { local: f.local.clone(), offset: f.offset, field }
    }).collect();
    gate::frame_parts(&setup(), &plate, &frames).expect("the demo plate's parts")
}

/// A field part's frame inverse: printer mm to shape space.
fn to_shape(field: &PartField) -> [[f64; 4]; 3] {
    let m = field.to_frame;
    std::array::from_fn(|r| [m[0][r], m[1][r], m[2][r], 0.0 - (m[0][r] * m[0][3] + m[1][r] * m[1][3] + m[2][r] * m[2][3])])
}

/// The batch core: the scan of a full batch's values (host-sized), best of five after a warm run.
fn core_costs(gpu: &mut Gpu) {
    let k = CoreKernels::new(gpu).unwrap_or_else(|e| panic!("the core's kernels: {e}"));
    let n = SCAN_MAX;
    let source = gpu.words(&(0..n).map(|i| i & 1).collect::<Vec<u32>>(), false).unwrap();
    let out = gpu.buffer(n as u64 * 4, false).unwrap();
    let sums = gpu.buffer(n.div_ceil(256) as u64 * 4, false).unwrap();
    let mut best = f64::INFINITY;
    for rep in 0..6 {
        let mut batch = Batch::new(gpu, "core scan", 2, 1, true).unwrap();
        batch.stage("scan");
        batch.scan(&k, &source, &out, &sums, n, NONE, 1).unwrap();
        let ran = batch.submit(gpu, &[]).unwrap();
        assert_eq!(ran.counts[1], n / 2, "the scan's total");
        let ms: f64 = ran.label_ms(gpu).iter().map(|s| s.2).sum();
        if rep > 0 { best = best.min(ms); }
    }
    println!("CORE scan of {n} values: {best:.3} ms GPU in 3 dispatches ({:.3} ns a value)", best * 1e6 / n as f64);
}

fn main() {
    let mut gpu = match Gpu::open(fab_gpuslice::script_mod) {
        Ok(gpu) => gpu,
        Err(e) => {
            println!("QUICK skipped: {e}");
            return;
        }
    };
    println!("QUICK backend {}", gpu.backend);
    let costs = probe::costs(&mut gpu).unwrap_or_else(|e| panic!("the probe failed: {e}"));
    for l in &costs.lines { println!("{l}"); }
    core_costs(&mut gpu);

    let parts = demo_parts();
    for part in parts.iter().filter(|p| p.field.is_some()) {
        let f = part.field.as_ref().unwrap();
        let program = PartProgram::new(&f.shape.root, to_shape(f)).unwrap_or_else(|e| panic!("{}: its program: {e}", part.name));
        let zs = part.zs();
        let (lo, hi) = part.source.bounds();
        // 16 layers spread over the part, a 256 x 256 grid over its box on each
        let layers: Vec<f64> = (0..16).map(|i| zs[i * (zs.len() - 1) / 15] - SdfSource::BELOW_PLANE).collect();
        let points: Vec<[f64; 3]> = layers.iter().flat_map(|&z| (0..256 * 256).map(move |k| {
            let (i, j) = ((k % 256) as f64 / 255.0, (k / 256) as f64 / 255.0);
            [lo[0] + (hi[0] - lo[0]) * i, lo[1] + (hi[1] - lo[1]) * j, z]
        })).collect();
        let (ms, values) = field::sample_cost(&mut gpu, &program, &points).unwrap_or_else(|e| panic!("{}: sampling: {e}", part.name));
        let exact = f.field().expect("the exact field");
        let worst = points.iter().zip(&values).step_by(points.len() / 4096).map(|(p, &v)| (exact(p[0], p[1], p[2]) - v as f64).abs()).fold(0.0, f64::max);
        println!("SAMPLE {}: {ms:.3} ms for {} samples, {:.2} ns a sample; program {} texels, {} kernels; {} layers; GPU against the exact field: worst {worst:.2e} mm of 4096",
            part.name, points.len(), ms * 1e6 / points.len() as f64, program.program.texels.len(), program.slots.len(), zs.len());
        trace_part(&mut gpu, part, &program);
    }
}

/// Layers a batch.
const BATCH: usize = 64;

/// The trace's stages over a part: a checked run (node signs against the dense reference and
/// the exact field, crossings against the exact zero set), then the best of three timed runs.
fn trace_part(gpu: &mut Gpu, part: &GatePart, program: &PartProgram) {
    let f = part.field.as_ref().unwrap();
    let exact = f.field().expect("the exact field");
    let lipschitz = f.shape.lipschitz().max(1.0);
    let clearance = f.shape.clearance();
    let zs: Vec<f64> = part.zs().iter().map(|z| z - SdfSource::BELOW_PLANE).collect();
    let pt = PartTrace::new(gpu, program, lipschitz, part.source.bounds(), zs.clone()).unwrap_or_else(|e| panic!("{}: {e}", part.name));
    let k = TraceKernels::new(gpu, program).unwrap_or_else(|e| panic!("{}: the trace kernels: {e}", part.name));
    let g = pt.grid;
    let chunks: Vec<std::ops::Range<usize>> = (0..zs.len()).step_by(BATCH).map(|a| a..(a + BATCH).min(zs.len())).collect();

    // checked: room found; every node's sign against the dense reference and both ends of every
    // sign-changing fine edge sampled (the dense bits); crossings against the exact field
    let mut caps = Vec::new();
    let (mut kept, mut node_samples, mut crosses, mut cross_samples, mut retries) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut nodes, mut sign_off, mut bits_off, mut edges, mut checked, mut exact_off) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    // per solve pass (A, B): the checked crossings' in-plane distances and normal angles
    let (mut off, mut angle): ([Vec<f64>; 2], [Vec<f64>; 2]) = Default::default();
    // the farthest crossings: (in-plane distance, |f|, |grad f| of the exact field there, layer, pass)
    let mut far: Vec<(f64, f64, f64, u32, usize)> = Vec::new();
    // the links: crossings with no valid successor, crossings without exactly one predecessor,
    // loops over more than one layer, and per layer the loops (signed area in mm^2, positive:
    // counterclockwise, an outer boundary; their bounding box's centre)
    let (mut broken, mut bad_pred, mut mixed) = (0u64, 0u64, 0u64);
    let mut loops_at: BTreeMap<u32, Vec<(f64, [f64; 2])>> = BTreeMap::new();
    let mut room = Caps { slots: g.cells() * BATCH as u32 / 8 + 1024, crosses: g.cells() * BATCH as u32 / 2 + 4096 };
    let opts = Opts { timed: false, dense: true, read: true };
    for r in &chunks {
        let run = loop {
            let run = trace::trace_batch(gpu, &k, &pt, r.clone(), room, opts).unwrap_or_else(|e| panic!("{}: {e}", part.name));
            match run.overflow {
                None => break run,
                Some((slots, crosses)) => {
                    if slots { room.slots *= 2; }
                    if crosses || !slots { room.crosses *= 2; }
                }
            }
        };
        caps.push(room);
        kept += run.kept as u64;
        node_samples += run.node_samples as u64;
        crosses += run.crosses as u64;
        cross_samples += run.cross_samples as u64;
        retries += run.retries as u64;
        for (s, &e) in run.kept_ids.iter().enumerate() {
            let (p, c) = (e / g.cells(), e % g.cells());
            let (v, d) = (&run.node_d[s * 121..s * 121 + 121], &run.dense_d[s * 121..s * 121 + 121]);
            for m in 0..121 {
                nodes += 1;
                if (v[m] <= 0.0) != (d[m] <= 0.0) { sign_off += 1; }
                // the exact field at every 97th node, where it is clear of zero
                if nodes % 97 == 0 {
                    let (i, j) = ((c % g.nx) * 10 + m as u32 % 11, (c / g.nx) * 10 + m as u32 / 11);
                    let q = g.mm(i as f64, j as f64);
                    let x = exact(q[0], q[1], zs[r.start + p as usize]);
                    if x.abs() > 1e-5 { checked += 1; if (x <= 0.0) != (v[m] <= 0.0) { exact_off += 1; } }
                }
            }
            // every fine edge that changes sign has both ends sampled: the dense bits
            for m in 0..121 {
                for n in [m + 1, m + 11] {
                    if n >= 121 || (n == m + 1 && m % 11 == 10) { continue; }
                    if (d[m] <= 0.0) != (d[n] <= 0.0) {
                        edges += 1;
                        for x in [m, n] { if v[x].to_bits() != d[x].to_bits() { bits_off += 1; } }
                    }
                }
            }
        }
        // every 23rd crossing and every one pass B solved against the exact zero set (in-plane
        // distance) and gradient
        let mut pass = vec![0usize; run.crossings.len()];
        for &x in &run.retried { pass[x as usize] = 1; }
        let mut picked: Vec<usize> = (0..run.crossings.len()).step_by(23).chain(run.retried.iter().map(|&x| x as usize)).collect();
        picked.sort_unstable();
        picked.dedup();
        for (c, ab) in picked.iter().map(|&x| (&run.crossings[x], pass[x])) {
            let q = g.mm(c.at[0] as f64, c.at[1] as f64);
            let z = zs[c.layer as usize];
            let d = gate::off_plane(&exact, &clearance, q, z);
            off[ab].push(d);
            let h = 1e-6;
            let (gx, gy) = ((exact(q[0] + h, q[1], z) - exact(q[0] - h, q[1], z)) / (2.0 * h), (exact(q[0], q[1] + h, z) - exact(q[0], q[1] - h, z)) / (2.0 * h));
            let gl = gx.hypot(gy);
            if d > 1e-4 { far.push((d, exact(q[0], q[1], z).abs(), gl, c.layer, ab)); }
            if gl > 0.05 {
                let cos = ((gx * c.normal[0] as f64 + gy * c.normal[1] as f64) / gl).clamp(-1.0, 1.0);
                angle[ab].push(cos.acos().to_degrees());
            }
        }
        let n = run.crossings.len();
        let mut pred = vec![0u32; n];
        for &x in &run.next {
            if (x as usize) < n { pred[x as usize] += 1; } else { broken += 1; }
        }
        let off = pred.iter().filter(|&&p| p != 1).count() as u64;
        bad_pred += off;
        if off == 0 && run.next.len() == n {
            let mut seen = vec![false; n];
            for first in 0..n {
                if seen[first] { continue; }
                let (layer, mut area, mut c) = (run.crossings[first].layer, 0.0f64, first);
                let (mut one_layer, mut bb) = (true, [f64::MAX, f64::MAX, f64::MIN, f64::MIN]);
                let mm = |x: usize| { let a = run.crossings[x].at; g.mm(a[0] as f64, a[1] as f64) };
                loop {
                    seen[c] = true;
                    let (a, b) = (mm(c), mm(run.next[c] as usize));
                    area += 0.5 * (a[0] * b[1] - b[0] * a[1]);
                    bb = [bb[0].min(a[0]), bb[1].min(a[1]), bb[2].max(a[0]), bb[3].max(a[1])];
                    one_layer &= run.crossings[c].layer == layer;
                    c = run.next[c] as usize;
                    if c == first { break; }
                }
                if !one_layer { mixed += 1; }
                loops_at.entry(layer).or_default().push((area, [(bb[0] + bb[2]) * 0.5, (bb[1] + bb[3]) * 0.5]));
            }
        }
        room = Caps { slots: ((run.kept as f64 * 1.5) as u32 + 1024).max(room.slots / 2), crosses: ((run.crosses as f64 * 1.5) as u32 + 4096).max(room.crosses / 2) };
    }

    // timed: best of three after a warm run
    let mut best: Option<(f64, Vec<(String, String, f64)>, f64)> = None;
    for rep in 0..4 {
        let mut labels: Vec<(String, String, f64)> = Vec::new();
        let mut largest = 0.0f64;
        for (r, &room) in chunks.iter().zip(&caps) {
            let run = trace::trace_batch(gpu, &k, &pt, r.clone(), room, Opts { timed: true, dense: true, read: false }).unwrap_or_else(|e| panic!("{}: {e}", part.name));
            assert!(run.overflow.is_none());
            let mut batch = 0.0;
            for (stage, label, ms) in run.label_ms {
                if stage != "dense" { batch += ms; }
                match labels.iter_mut().find(|s| s.0 == stage && s.1 == label) { Some(s) => s.2 += ms, None => labels.push((stage, label, ms)) }
            }
            largest = largest.max(batch);
        }
        let total: f64 = labels.iter().filter(|s| s.0 != "dense").map(|s| s.2).sum();
        if rep > 0 && best.as_ref().is_none_or(|b| total < b.0) { best = Some((total, labels, largest)); }
    }
    let (total, labels, largest) = best.unwrap();
    let stage_ms = |name: &str| labels.iter().filter(|l| l.0 == name).map(|l| l.2).sum::<f64>();
    let by_label = |name: &str| labels.iter().filter(|l| l.0 == name).map(|l| format!("{} {:.2}", l.1, l.2)).collect::<Vec<String>>().join(", ");
    let pct = |v: &mut Vec<f64>, q: f64| { v.sort_by(f64::total_cmp); v.get(((v.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0) };
    let coarse_samples = g.nodes() as u64 * zs.len() as u64;
    println!("TRACE {}: {} layers in {} batches of <= {BATCH}, one submission each; grid {} x {} cells; L {lipschitz:.3}", part.name, zs.len(), chunks.len(), g.nx, g.ny);
    println!("TRACE {}: {total:.2} ms in all on the GPU, largest batch {largest:.2} ms: coarse {:.2} ms ({coarse_samples} samples), keep {:.2} ms ({kept} cells kept), nodes {:.2} ms ({node_samples} samples, {:.1} a kept cell; dense {:.2} ms for {}), crossings {:.2} ms ({crosses}, {cross_samples} samples, {:.2} a crossing; {retries} to pass B), links {:.2} ms",
        part.name, stage_ms("coarse"), stage_ms("keep"), stage_ms("nodes"), node_samples as f64 / kept.max(1) as f64, stage_ms("dense"), kept * 121, stage_ms("crossings"), cross_samples as f64 / crosses.max(1) as f64, stage_ms("links"));
    // the CPU's contours at every layer: its rings (islands' outlines positive, holes negative)
    // against the GPU's loops, paired by sign and area (within 1% + 0.01 mm^2); a GPU loop left
    // over is checked against the exact field at its box's centre (outside for a hole)
    let cpu = part.source.contours(&part.zs());
    let ring = |r: &makepad_fab_slicer::Polygon| {
        let n = r.pts.len();
        (0..n).map(|t| { let (p, q) = (r.pts[t], r.pts[(t + 1) % n]); 0.5 * (p.x_mm() * q.y_mm() - q.x_mm() * p.y_mm()) }).sum::<f64>()
    };
    let (mut layers_off, mut gpu_left, mut gpu_true, mut cpu_left, mut cpu_flat) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut gpu_most, mut cpu_most) = (0.0f64, 0.0f64);
    for (k, islands) in cpu.iter().enumerate() {
        let mut theirs: Vec<Option<f64>> = islands.iter().flat_map(|i| std::iter::once(&i.contour).chain(&i.holes)).map(|r| Some(ring(r))).collect();
        let empty = Vec::new();
        let ours = loops_at.get(&(k as u32)).unwrap_or(&empty);
        let mut off = false;
        for &(a, at) in ours {
            let best = theirs.iter().enumerate().filter_map(|(i, t)| t.filter(|b| (b > &0.0) == (a > 0.0)).map(|b| ((b - a).abs(), i))).min_by(|x, y| x.0.total_cmp(&y.0));
            match best {
                Some((d, i)) if d <= 0.01 * a.abs() + 0.01 => theirs[i] = None,
                _ => {
                    off = true;
                    gpu_left += 1;
                    gpu_most = gpu_most.max(a.abs());
                    if (exact(at[0], at[1], zs[k]) > 0.0) == (a < 0.0) { gpu_true += 1; }
                }
            }
        }
        for b in theirs.into_iter().flatten() {
            off = true;
            cpu_left += 1;
            if b.abs() < 1e-6 { cpu_flat += 1; } else { cpu_most = cpu_most.max(b.abs()); }
        }
        layers_off += off as usize;
    }
    let (outer, holes) = loops_at.values().flatten().fold((0, 0), |n, l| if l.0 > 0.0 { (n.0 + 1, n.1) } else { (n.0, n.1 + 1) });
    println!("TRACE {}: links: {broken} crossings without a successor, {bad_pred} without exactly one predecessor, {mixed} loops over two layers; {outer} outer loops (counterclockwise), {holes} holes; against the CPU's rings: {layers_off} of {} layers differ, {gpu_left} GPU loops left over (largest {gpu_most:.4} mm2; the exact field agrees with {gpu_true} at their centre), {cpu_left} CPU rings left over ({cpu_flat} of zero area; largest of the rest {cpu_most:.4} mm2)",
        part.name, cpu.len());
    println!("TRACE {}: nodes by dispatch (ms): {}; crossings: {}", part.name, by_label("nodes"), by_label("crossings"));
    println!("TRACE {}: nodes against dense: {sign_off} of {nodes} signs differ, {bits_off} ends of {edges} sign-changing edges not sampled; against the exact field: {exact_off} of {checked} nodes off sign (clear of zero by 1e-5 mm)", part.name);
    for (ab, name) in ["A (every 23rd)", "B (every one)"].iter().enumerate() {
        let (off, angle) = (&mut off[ab], &mut angle[ab]);
        let n_off = off.len();
        let worst = off.iter().copied().fold(0.0, f64::max);
        let past = off.iter().filter(|&&d| d > 1e-4).count();
        println!("TRACE {}: pass {name} crossings against the exact zero set ({n_off} checked): p50 {:.2e} mm, p99 {:.2e} mm, worst {worst:.2e} mm, {past} past 1e-4 mm; normals off the exact gradient: p99 {:.3} deg, worst {:.3} deg ({} checked)",
            part.name, pct(off, 0.5), pct(off, 0.99), pct(angle, 0.99), angle.iter().copied().fold(0.0, f64::max), angle.len());
    }
    far.sort_by(|a, b| b.0.total_cmp(&a.0));
    if !far.is_empty() {
        let worst: Vec<String> = far.iter().take(4).map(|f| format!("{:.2e} mm (|f| {:.1e}, |grad| {:.1e}, layer {}, pass {})", f.0, f.1, f.2, f.3, ["A", "B"][f.4])).collect();
        println!("TRACE {}: farthest crossings: {}", part.name, worst.join("; "));
    }
}

//! What the GPU charges for execution itself, measured: a dispatch inside a pass (direct and
//! indirect), a submission waited on and one queued behind others, grid threads with no work,
//! streaming bandwidth and straight-line ALU rate. The quick benchmark prints these first, so
//! every stage's line can be read against its floors.
use crate::gpu::{dispatch, u4, Gpu, Groups};
use makepad_platform::*;

script_mod! {
    use mod.pod.*
    use mod.math.*
    use mod.shader

    // The cheapest dispatch: its first thread bumps one word.
    mod.gpuslice.ProbeTouch = {
        workgroup_size: 64
        out: shader.buffer_rw(u32)
        main: fn() {
            if global_id().x == 0u { self.out[0u] = self.out[0u] + 1u }
        }
    }
    // Indirect args (spec.x groups, 1, 1) into slot spec.y.
    mod.gpuslice.ProbeArgs = {
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        args: shader.buffer_rw(u32)
        main: fn() {
            if global_id().x == 0u {
                let k = self.spec.y * 4u
                self.args[k] = self.spec.x
                self.args[k + 1u] = 1u
                self.args[k + 2u] = 1u
            }
        }
    }
    // Threads past spec.x return at once: with spec.x = 0, the cost of a grid with no work.
    mod.gpuslice.ProbeIdle = {
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        out: shader.buffer_rw(u32)
        main: fn() {
            if global_id().x < self.spec.x { self.out[global_id().x] = 1u }
        }
    }
    // dst = src + 1 over spec.x words, 16 a thread strided by the spec.y threads.
    mod.gpuslice.ProbeCopy = {
        workgroup_size: 256
        spec: shader.uniform(vec4u)
        src: shader.buffer(u32)
        dst: shader.buffer_rw(u32)
        main: fn() {
            var i = global_id().x
            for j in 0..16u {
                if i < self.spec.x { self.dst[i] = self.src[i] + 1u }
                i += self.spec.y
            }
        }
    }
    // Four independent multiply-add chains of spec.x steps a thread.
    mod.gpuslice.ProbeAlu = {
        workgroup_size: 256
        spec: shader.uniform(vec4u)
        out: shader.buffer_rw(f32)
        main: fn() {
            let t = global_id().x
            var a = f32(t) * 0.001
            var b = a + 1.0
            var c = a + 2.0
            var d = a + 3.0
            for j in 0..self.spec.x {
                a = a * 0.9999 + 0.0001
                b = b * 0.9998 + 0.0002
                c = c * 0.9997 + 0.0003
                d = d * 0.9996 + 0.0004
            }
            self.out[t] = a + b + c + d
        }
    }
}

struct Kernels { touch: ComputeKernel, args: ComputeKernel, idle: ComputeKernel, copy: ComputeKernel, alu: ComputeKernel }

/// Median of `v` (sorted in place).
fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    if v.is_empty() { 0.0 } else { v[v.len() / 2] }
}

/// The GPU ms of a whole pass named `name` (its submission's span) after it ran.
fn pass_ms(gpu: &mut Gpu, name: &str) -> f64 {
    gpu.timer(name, 1, 200).first().map_or(f64::NAN, |&ms| ms as f64)
}

/// The probe's result lines, "PROBE <what> ...", and the per-dispatch and per-submission costs
/// in µs for the stage lines that follow.
pub struct Costs { pub lines: Vec<String>, pub dispatch_us: f64, pub indirect_us: f64, pub round_trip_ms: f64, pub queued_us: f64, pub gb_s: f64 }

pub fn costs(gpu: &mut Gpu) -> Result<Costs, String> {
    let m = id!(gpuslice);
    let k = Kernels {
        touch: gpu.kernel(m, id!(ProbeTouch))?, args: gpu.kernel(m, id!(ProbeArgs))?, idle: gpu.kernel(m, id!(ProbeIdle))?,
        copy: gpu.kernel(m, id!(ProbeCopy))?, alu: gpu.kernel(m, id!(ProbeAlu))?,
    };
    let out = gpu.buffer(16, false)?;
    let args = gpu.buffer(16, true)?;
    let words: u32 = 16 << 20;
    let src = gpu.buffer(words as u64 * 4, false)?;
    let dst = gpu.buffer(words as u64 * 4, false)?;
    let alu_threads: u32 = 1 << 20;
    let alu_out = gpu.buffer(alu_threads as u64 * 4, false)?;
    let read = |b: &GpuBuffer| (b.clone(), 0u64, 4u64);
    let mut lines = Vec::new();

    // warm: every pipeline compiled and run once
    {
        let mut pass = ComputePass::new("probe warm", None);
        dispatch(&mut pass, &k.touch, Groups::Direct([1, 1, 1]), &[], &[(id!(out), &out)], None)?;
        dispatch(&mut pass, &k.args, Groups::Direct([1, 1, 1]), &[(id!(spec), &u4([1, 0, 0, 0]))], &[(id!(args), &args)], None)?;
        dispatch(&mut pass, &k.touch, Groups::Indirect(&args, 0), &[], &[(id!(out), &out)], None)?;
        dispatch(&mut pass, &k.idle, Groups::Direct([1, 1, 1]), &[(id!(spec), &u4([0; 4]))], &[(id!(out), &out)], None)?;
        dispatch(&mut pass, &k.copy, Groups::Direct([1, 1, 1]), &[(id!(spec), &u4([256, 256, 0, 0]))], &[(id!(src), &src), (id!(dst), &dst)], None)?;
        dispatch(&mut pass, &k.alu, Groups::Direct([1, 1, 1]), &[(id!(spec), &u4([1, 0, 0, 0]))], &[(id!(out), &alu_out)], None)?;
        let r = read(&out);
        gpu.run(pass, &[(&r.0, r.1, r.2)])?;
    }

    // a dispatch inside one pass: the pass's GPU span at n dispatches, direct and indirect
    let span = |gpu: &mut Gpu, n: u32, indirect: bool| -> Result<(f64, f64), String> {
        let (mut g, mut w) = (Vec::new(), Vec::new());
        for rep in 0..5 {
            let name = format!("probe {} x{n} r{rep}", if indirect { "indirect" } else { "direct" });
            gpu_frame_timer::watch(&name);
            let mut pass = ComputePass::new(name.clone(), None);
            if indirect {
                dispatch(&mut pass, &k.args, Groups::Direct([1, 1, 1]), &[(id!(spec), &u4([1, 0, 0, 0]))], &[(id!(args), &args)], None)?;
            }
            for _ in 0..n {
                let groups = if indirect { Groups::Indirect(&args, 0) } else { Groups::Direct([1, 1, 1]) };
                dispatch(&mut pass, &k.touch, groups, &[], &[(id!(out), &out)], None)?;
            }
            let (_, wall) = gpu.run(pass, &[(&out, 0, 4)])?;
            g.push(pass_ms(gpu, &name));
            w.push(wall);
        }
        Ok((median(&mut g), median(&mut w)))
    };
    let mut direct = Vec::new();
    for n in [1u32, 16, 128, 1024] {
        let (d, dw) = span(gpu, n, false)?;
        let (i, iw) = span(gpu, n, true)?;
        lines.push(format!("PROBE pass of {n} dispatches: GPU {d:.3} ms (indirect {i:.3}), submit to readback {dw:.3} ms (indirect {iw:.3})"));
        direct.push((n, d, i));
    }
    let (first, last) = (direct[0], direct[direct.len() - 1]);
    let dispatch_us = (last.1 - first.1) / (last.0 - first.0) as f64 * 1e3;
    let indirect_us = (last.2 - first.2) / (last.0 - first.0) as f64 * 1e3;
    lines.push(format!("PROBE dispatch {dispatch_us:.2} us direct, {indirect_us:.2} us indirect (GPU, within one pass, slope 1..{} dispatches)", last.0));

    // a submission waited on: one dispatch, submit to its readback, back to back
    let mut trips = Vec::new();
    for _ in 0..40 {
        let mut pass = ComputePass::new("probe trip", None);
        dispatch(&mut pass, &k.touch, Groups::Direct([1, 1, 1]), &[], &[(id!(out), &out)], None)?;
        trips.push(gpu.run(pass, &[(&out, 0, 4)])?.1);
    }
    let lo = trips.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = trips.iter().cloned().fold(0.0, f64::max);
    let round_trip_ms = median(&mut trips);
    lines.push(format!("PROBE submission round trip (1 dispatch, 4-byte readback): median {round_trip_ms:.3} ms, min {lo:.3}, max {hi:.3} over 40"));

    // submissions queued behind each other, only the last read back
    let mut queued = Vec::new();
    for _ in 0..5 {
        let n = 32u32;
        let t = std::time::Instant::now();
        for _ in 0..n - 1 {
            let mut pass = ComputePass::new("probe queued", None);
            dispatch(&mut pass, &k.touch, Groups::Direct([1, 1, 1]), &[], &[(id!(out), &out)], None)?;
            pass.submit(&mut gpu.cx);
        }
        let mut pass = ComputePass::new("probe queued", None);
        dispatch(&mut pass, &k.touch, Groups::Direct([1, 1, 1]), &[], &[(id!(out), &out)], None)?;
        gpu.run(pass, &[(&out, 0, 4)])?;
        queued.push(t.elapsed().as_secs_f64() * 1e3 / n as f64);
    }
    let queued_us = median(&mut queued) * 1e3;
    lines.push(format!("PROBE queued submission: {queued_us:.1} us each (32 passes of 1 dispatch, one readback)"));

    // timed single dispatches: idle grid threads, bandwidth, ALU
    let timed = |gpu: &mut Gpu, name: &str, kernel: &ComputeKernel, groups: u32, spec: [u32; 4], bufs: &[(LiveId, &GpuBuffer)]| -> Result<f64, String> {
        let mut ms = Vec::new();
        for _ in 0..5 {
            gpu_frame_timer::watch(name);
            let mut pass = ComputePass::new(format!("probe {name}"), None);
            dispatch(&mut pass, kernel, Groups::Direct([groups, 1, 1]), &[(id!(spec), &u4(spec))], bufs, Some(name))?;
            gpu.run(pass, &[(&out, 0, 4)])?;
            ms.extend(gpu.timer(name, 1, 200).iter().map(|&m| m as f64));
        }
        Ok(ms.iter().cloned().fold(f64::INFINITY, f64::min))
    };
    let idle_groups = 1u32 << 14;
    let idle = timed(gpu, "probe idle", &k.idle, idle_groups, [0; 4], &[(id!(out), &out)])?;
    let idle_small = timed(gpu, "probe idle 1", &k.idle, 1, [0; 4], &[(id!(out), &out)])?;
    lines.push(format!("PROBE idle grid: {:.3} ms for {} threads with no work ({:.3} ns a thread), one group {idle_small:.4} ms", idle, idle_groups * 64, (idle - idle_small) * 1e6 / (idle_groups * 64) as f64));
    let threads = words / 16;
    let copy = timed(gpu, "probe copy", &k.copy, threads / 256, [words, threads, 0, 0], &[(id!(src), &src), (id!(dst), &dst)])?;
    let gb_s = words as f64 * 8.0 / (copy * 1e-3) / 1e9;
    lines.push(format!("PROBE stream: {copy:.3} ms for {} MiB read + written, {gb_s:.1} GB/s", words as u64 * 8 >> 20));
    let steps = 1024u32;
    let alu = timed(gpu, "probe alu", &k.alu, alu_threads / 256, [steps, 0, 0, 0], &[(id!(out), &alu_out)])?;
    let flops = alu_threads as f64 * steps as f64 * 8.0;
    lines.push(format!("PROBE alu: {alu:.3} ms for {:.1} Gflop of multiply-adds, {:.0} Gflop/s", flops / 1e9, flops / (alu * 1e-3) / 1e9));
    Ok(Costs { lines, dispatch_us, indirect_us, round_trip_ms, queued_us, gb_s })
}

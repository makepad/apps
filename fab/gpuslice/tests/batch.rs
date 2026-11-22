//! The batch core on the GPU, against the CPU: the scan (host-sized and GPU-counted, across
//! block boundaries), a count past its cap (the overflow word and the clamp) and passes sized by
//! the GPU from counts, all in one submission as the slicer records them.
use fab_gpuslice::batch::{Batch, CoreKernels, Size, NONE};
use fab_gpuslice::gpu::Gpu;
use makepad_platform::*;

script_mod! {
    use mod.pod.*
    use mod.math.*
    use mod.shader

    // Each thread of the pass counts itself into word spec.x.
    mod.gpuslice.TestThreads = {
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        counts: shader.buffer_rw(u32)
        main: fn() {
            let r = atomic_add(self.counts[self.spec.x], 1u)
        }
    }
    // Word spec.x = spec.y: a producer's count, written on the GPU.
    mod.gpuslice.TestCount = {
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        counts: shader.buffer_rw(u32)
        main: fn() {
            if global_id().x == 0u { self.counts[self.spec.x] = self.spec.y }
        }
    }
}

fn open() -> Option<Gpu> {
    match Gpu::open(|vm| {
        fab_gpuslice::script_mod(vm);
        script_mod(vm);
    }) {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            println!("skipped: {e}");
            None
        }
    }
}

fn values(n: usize) -> Vec<u32> {
    let mut x = 0x9e37_79b9u32;
    (0..n).map(|_| {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        // mostly zeros and ones, as a keep flag; now and then a run of a few
        [0, 1, 0, 1, 1, 0, 3, 0][(x >> 7) as usize % 8]
    }).collect()
}

fn exclusive(v: &[u32]) -> (Vec<u32>, u32) {
    let mut s = 0u32;
    let out = v.iter().map(|&x| { let o = s; s += x; o }).collect();
    (out, s)
}

#[test]
fn scan_and_counted_passes() {
    let Some(mut gpu) = open() else { return };
    let k = CoreKernels::new(&mut gpu).unwrap();
    let threads = gpu.kernel(id!(gpuslice), id!(TestThreads)).unwrap();
    let count = gpu.kernel(id!(gpuslice), id!(TestCount)).unwrap();

    // words: 0 overflow, 1 host-sized total, 2 the GPU's count, 3 its total, 4 the counted
    // pass's threads, 5 a count past its cap, 6 the clamped pass's threads
    let mut batch = Batch::new(&mut gpu, "batch test", 8, 2, false).unwrap();
    let cap = 300_001u32; // past a block of 256 and past 1024 blocks
    let counted = 299_873u32;
    let source_values = values(cap as usize);
    let source = gpu.words(&source_values, false).unwrap();
    let all = gpu.buffer(cap as u64 * 4, false).unwrap();
    let part = gpu.buffer(cap as u64 * 4, false).unwrap();
    let sums = gpu.buffer(cap.div_ceil(256) as u64 * 4, false).unwrap();
    let sums2 = gpu.buffer(cap.div_ceil(256) as u64 * 4, false).unwrap();

    batch.scan(&k, &source, &all, &sums, cap, NONE, 1).unwrap();
    batch.run("count", &count, Size::Direct([1, 1, 1]), [2, counted, 0, 0], &[]).unwrap();
    batch.scan(&k, &source, &part, &sums2, cap, 2, 3).unwrap();
    // a pass over the GPU's count, 3 threads an element
    let size = batch.counted(&k, 0, 2, cap, 3).unwrap();
    batch.run("threads", &threads, size, [4, 0, 0, 0], &[]).unwrap();
    // a count of 1000 against a cap of 500: overflow, clamped to 500
    batch.run("count", &count, Size::Direct([1, 1, 1]), [5, 1000, 0, 0], &[]).unwrap();
    let size = batch.counted(&k, 1, 5, 500, 1).unwrap();
    batch.run("threads", &threads, size, [6, 0, 0, 0], &[]).unwrap();

    let ran = batch.submit(&mut gpu, &[(&all, 0, cap as u64 * 4), (&part, 0, counted as u64 * 4)]).unwrap();
    let c = &ran.counts;
    let (want_all, total) = exclusive(&source_values);
    let (want_part, part_total) = exclusive(&source_values[..counted as usize]);
    let got_all = fab_gpuslice::gpu::words_of(&ran.reads[0]);
    let got_part = fab_gpuslice::gpu::words_of(&ran.reads[1]);
    let first_bad = |a: &[u32], b: &[u32]| a.iter().zip(b).position(|(x, y)| x != y);
    assert_eq!(first_bad(&got_all, &want_all), None, "host-sized scan");
    assert_eq!(c[1], total, "host-sized total");
    assert_eq!(first_bad(&got_part, &want_part), None, "GPU-counted scan");
    assert_eq!(c[3], part_total, "GPU-counted total");
    // whole workgroups of 64: the pass ran ceil(n * per / 64) * 64 threads
    assert_eq!(c[4], (counted * 3).div_ceil(64) * 64, "a pass sized by the GPU's count");
    assert_eq!(c[0], 1, "a count past its cap sets the overflow word");
    assert_eq!(c[5], 500, "and is clamped to the cap");
    assert_eq!(c[6], 500u32.div_ceil(64) * 64, "the clamped pass's size");
}

//! The execution core every stage records into: one compute pass per batch (one submission),
//! a counts buffer of named words the GPU writes, indirect args the GPU writes from those words
//! (so a pass's size is its producer's count, never a host readback), and the deterministic
//! exclusive scan that compacts a pass's active elements in id order.
//!
//! Conventions of every core kernel: workgroups of 64 threads; `spec` (a vec4u uniform) is the
//! kernel's own; an element past its count returns at once. Integer sums only, so the same
//! inputs give the same bytes however a GPU schedules the work.
use crate::gpu::{dispatch, u4, Gpu, Groups};
use makepad_platform::*;

script_mod! {
    use mod.pod.*
    use mod.math.*
    use mod.shader

    mod.gpuslice = {}

    // The scan's block sums: a workgroup sums one block of 256 values (4 a thread) of the first n
    // (n = counts[spec.y] when spec.y is a word, else spec.x); spec.z: the first block.
    mod.gpuslice.ScanSums = {
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        counts: shader.buffer_rw(u32)
        source: shader.buffer(u32)
        sums: shader.buffer_rw(u32)
        part: shader.workgroup(u32, 64)
        main: fn() {
            var n = self.spec.x
            if self.spec.y != 4294967295u { n = min(self.counts[self.spec.y], self.spec.x) }
            let b = self.spec.z + group_id().x
            let li = local_index()
            var t = 0u
            for j in 0..4u {
                let k = b * 256u + li * 4u + j
                if k < n { t += self.source[k] }
            }
            self.part[li] = t
            barrier()
            var step = 32u
            for r in 0..6u {
                if li < step { self.part[li] = self.part[li] + self.part[li + step] }
                barrier()
                step = step / 2u
            }
            if li == 0u { self.sums[b] = self.part[0u] }
        }
    }
    // One workgroup scans the block sums in place (at most 4096: 64 a thread) and writes the
    // total to counts[spec.w]. spec.x: the values' cap, spec.y: their count word or NONE.
    mod.gpuslice.ScanTop = {
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        counts: shader.buffer_rw(u32)
        sums: shader.buffer_rw(u32)
        part: shader.workgroup(u32, 64)
        main: fn() {
            if group_id().x != 0u { return }
            var n = self.spec.x
            if self.spec.y != 4294967295u { n = min(self.counts[self.spec.y], self.spec.x) }
            let blocks = (n + 255u) / 256u
            let li = local_index()
            var t = 0u
            for j in 0..64u {
                let k = li * 64u + j
                if k < blocks { t += self.sums[k] }
            }
            self.part[li] = t
            barrier()
            var d = 1u
            for r in 0..6u {
                var add = 0u
                if li >= d { add = self.part[li - d] }
                barrier()
                self.part[li] = self.part[li] + add
                barrier()
                d = d * 2u
            }
            var s = self.part[li] - t
            for j in 0..64u {
                let k = li * 64u + j
                if k < blocks {
                    let v = self.sums[k]
                    self.sums[k] = s
                    s += v
                }
            }
            if li == 63u { self.counts[self.spec.w] = self.part[63u] }
        }
    }
    // The block's values offset by its block's scanned sum: destination[k] = the exclusive
    // prefix sum of source at k, for k < n (spec as ScanSums).
    mod.gpuslice.ScanApply = {
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        counts: shader.buffer_rw(u32)
        source: shader.buffer(u32)
        sums: shader.buffer(u32)
        destination: shader.buffer_rw(u32)
        part: shader.workgroup(u32, 64)
        main: fn() {
            var n = self.spec.x
            if self.spec.y != 4294967295u { n = min(self.counts[self.spec.y], self.spec.x) }
            let b = self.spec.z + group_id().x
            let li = local_index()
            let first = b * 256u + li * 4u
            var t = 0u
            for j in 0..4u {
                let k = first + j
                if k < n { t += self.source[k] }
            }
            self.part[li] = t
            barrier()
            var d = 1u
            for r in 0..6u {
                var add = 0u
                if li >= d { add = self.part[li - d] }
                barrier()
                self.part[li] = self.part[li] + add
                barrier()
                d = d * 2u
            }
            var s = self.sums[b] + self.part[li] - t
            for j in 0..4u {
                let k = first + j
                if k < n {
                    let v = self.source[k]
                    self.destination[k] = s
                    s += v
                }
            }
        }
    }
    // Indirect args from a count: slot spec.z of `args` = (ceil(min(counts[spec.x], cap) *
    // per / 64) workgroups, 1, 1), cap = spec.y, per = spec.w threads an element. A count past
    // its cap sets the overflow word (counts[0]) and is clamped there, so no pass reads past
    // what was allocated; the batch then delivers nothing and runs again with room.
    mod.gpuslice.Args = {
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        counts: shader.buffer_rw(u32)
        args: shader.buffer_rw(u32)
        main: fn() {
            if global_id().x != 0u { return }
            var n = self.counts[self.spec.x]
            if n > self.spec.y {
                self.counts[0u] = 1u
                n = self.spec.y
                self.counts[self.spec.x] = n
            }
            let k = self.spec.z * 4u
            self.args[k] = (n * self.spec.w + 63u) / 64u
            self.args[k + 1u] = 1u
            self.args[k + 2u] = 1u
        }
    }
}

/// A batch dispatch's size: workgroups given by the host, or the indirect slot an
/// [`Batch::counted`] wrote earlier in the same pass.
#[derive(Clone, Copy, Debug)]
pub enum Size {
    Direct([u32; 3]),
    Slot(u32),
}

/// No count word: the element count is the host's.
pub const NONE: u32 = u32::MAX;
/// Values one scan takes (the top's workgroup scans 4096 block sums of 256).
pub const SCAN_MAX: u32 = 4096 * 256;

/// The core's kernels, made once per GPU.
pub struct CoreKernels { sums: ComputeKernel, top: ComputeKernel, apply: ComputeKernel, args: ComputeKernel }

impl CoreKernels {
    pub fn new(gpu: &mut Gpu) -> Result<Self, String> {
        let m = id!(gpuslice);
        Ok(Self { sums: gpu.kernel(m, id!(ScanSums))?, top: gpu.kernel(m, id!(ScanTop))?, apply: gpu.kernel(m, id!(ScanApply))?, args: gpu.kernel(m, id!(Args))? })
    }
}

/// A batch being recorded: its one compute pass, its counts words (word 0 is the overflow
/// word) and its indirect args slots, and the stage each dispatch is timed under.
pub struct Batch {
    pub pass: ComputePass,
    pub counts: GpuBuffer,
    pub args: GpuBuffer,
    words: u32,
    slots: u32,
    /// Timers of the dispatches (stage, label, timer name): the bench sums them.
    pub timers: Vec<(String, String, String)>,
    timed: bool,
    stage: String,
}

impl Batch {
    /// A batch with `words` count words and `slots` indirect slots; `timed`: every dispatch has its own GPU timer.
    pub fn new(gpu: &mut Gpu, name: &str, words: u32, slots: u32, timed: bool) -> Result<Batch, String> {
        let counts = gpu.words(&vec![0u32; words as usize], false)?;
        let args = gpu.buffer(slots.max(1) as u64 * 16, true)?;
        Ok(Batch { pass: ComputePass::new(name, None), counts, args, words, slots, timers: Vec::new(), timed, stage: String::new() })
    }
    /// The stage the next dispatches are timed under.
    pub fn stage(&mut self, stage: &str) { self.stage = stage.to_string(); }
    fn timer(&mut self, label: &str) -> Option<String> {
        if !self.timed { return None; }
        let name = format!("{} {} {}", self.stage, label, self.timers.len());
        gpu_frame_timer::watch(&name);
        self.timers.push((self.stage.clone(), label.to_string(), name.clone()));
        Some(name)
    }
    /// One dispatch of `kernel` (its `counts` and `args` bindings are the batch's).
    pub fn run(&mut self, label: &str, kernel: &ComputeKernel, size: Size, spec: [u32; 4], buffers: &[(LiveId, &GpuBuffer)]) -> Result<(), String> {
        self.run_with(label, kernel, size, &[(id!(spec), &u4(spec))], buffers)
    }
    /// One dispatch with its uniforms given by name (`spec` among them when the kernel has one).
    pub fn run_with(&mut self, label: &str, kernel: &ComputeKernel, size: Size, uniforms: &[(LiveId, &[u8])], buffers: &[(LiveId, &GpuBuffer)]) -> Result<(), String> {
        let (counts, args) = (self.counts.clone(), self.args.clone());
        let mut all: Vec<(LiveId, &GpuBuffer)> = vec![(id!(counts), &counts), (id!(args), &args)];
        all.extend_from_slice(buffers);
        let groups = match size {
            Size::Direct(g) => Groups::Direct(g),
            Size::Slot(s) => {
                assert!(s < self.slots);
                Groups::Indirect(&args, s as u64 * 16)
            }
        };
        let timer = self.timer(label);
        dispatch(&mut self.pass, kernel, groups, uniforms, &all, timer.as_deref())
    }
    /// A pass over the elements counted in `word` (at most `cap`, `per` threads each): the GPU
    /// writes its size into indirect slot `slot` first.
    pub fn counted(&mut self, k: &CoreKernels, slot: u32, word: u32, cap: u32, per: u32) -> Result<Size, String> {
        assert!(slot < self.slots && word < self.words);
        self.run("args", &k.args, Size::Direct([1, 1, 1]), [word, cap, slot, per], &[])?;
        Ok(Size::Slot(slot))
    }
    /// The exclusive scan of the first n values of `source` into `destination` (another
    /// buffer), n = counts[`word`] (at most `cap`) or `cap` when `word` is NONE, the total into
    /// counts[`total`]. `sums` holds a word per block of 256.
    pub fn scan(&mut self, k: &CoreKernels, source: &GpuBuffer, destination: &GpuBuffer, sums: &GpuBuffer, cap: u32, word: u32, total: u32) -> Result<(), String> {
        assert!(cap <= SCAN_MAX, "a scan of {cap} values: batches are sized under {SCAN_MAX}");
        let blocks = cap.div_ceil(256).max(1);
        assert!(sums.size() >= blocks as u64 * 4);
        self.run("scan sums", &k.sums, Size::Direct([blocks, 1, 1]), [cap, word, 0, 0], &[(id!(source), source), (id!(sums), sums)])?;
        self.run("scan top", &k.top, Size::Direct([1, 1, 1]), [cap, word, 0, total], &[(id!(sums), sums)])?;
        self.run("scan apply", &k.apply, Size::Direct([blocks, 1, 1]), [cap, word, 0, 0], &[(id!(source), source), (id!(sums), sums), (id!(destination), destination)])
    }
    /// Submits the batch (its one submission) and reads back `reads` after it.
    pub fn submit(self, gpu: &mut Gpu, reads: &[(&GpuBuffer, u64, u64)]) -> Result<Ran, String> {
        let mut all: Vec<(&GpuBuffer, u64, u64)> = vec![(&self.counts, 0, self.words as u64 * 4)];
        all.extend_from_slice(reads);
        let (mut out, wall_ms) = gpu.run(self.pass, &all)?;
        let counts = crate::gpu::words_of(&out.remove(0));
        Ok(Ran { counts, reads: out, wall_ms, timers: self.timers })
    }
}

/// A submitted batch: its counts words (word 0: the overflow word), the reads' bytes, the wall
/// ms from submit to the last read, and its dispatch timers (stage, label, timer name).
pub struct Ran {
    pub counts: Vec<u32>,
    pub reads: Vec<HostBuf>,
    pub wall_ms: f64,
    pub timers: Vec<(String, String, String)>,
}

impl Ran {
    /// GPU ms per (stage, label), in first-dispatch order (the timers land with the pass's
    /// completion); sum by stage for a stage's ms.
    pub fn label_ms(&self, gpu: &mut Gpu) -> Vec<(String, String, f64)> {
        let mut out: Vec<(String, String, f64)> = Vec::new();
        for (stage, label, name) in &self.timers {
            let ms: f64 = gpu.timer(name, 1, 200).iter().map(|&v| v as f64).sum();
            match out.iter_mut().find(|(s, l, _)| s == stage && l == label) {
                Some(e) => e.2 += ms,
                None => out.push((stage.clone(), label.clone(), ms)),
            }
        }
        out
    }
}

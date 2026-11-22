//! A part's SDF on the GPU: the shape lowered to the SDF engine's program,
//! the part's Splash kernels spliced into the slicer's kernels, and the frame
//! that maps printer mm to shape space. Every slicer kernel that samples the field extends
//! `mod.sdf.Eval` and calls its `eval_program`: one evaluation, run to its end.
use crate::gpu::{dispatch, u4, Gpu, Groups};
use makepad_platform::*;
use fab_sdf_engine::kernel::{self, ComputeBake, KernelSlot};
use fab_sdf_engine::makepad_csg_sdf::SdfNode;
use fab_sdf_engine::program::{self, Program};

script_mod! {
    use mod.pod.*
    use mod.math.*
    use mod.shader

    // The field at spec.x points (shape space): its cost per sample.
    mod.gpuslice.Sample = {
        ..mod.sdf.Eval
        precise_math: true
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        pts: shader.buffer(vec4f)
        values: shader.buffer_rw(f32)
        main: fn() {
            let i = global_id().x
            if i >= self.spec.x { return }
            let p = self.pts[i]
            self.values[i] = self.eval_program(0.0, p.x, p.y, p.z).x
        }
    }
}

/// A part revision's field as the GPU samples it.
pub struct PartProgram {
    pub slots: Vec<KernelSlot>,
    pub program: Program,
    /// Printer mm (x, y, z) to shape space, rows of an affine map.
    pub to_shape: [[f64; 4]; 3],
}

impl PartProgram {
    pub fn new(root: &SdfNode, to_shape: [[f64; 4]; 3]) -> Result<PartProgram, String> {
        let mut slots = Vec::new();
        kernel::collect(root, &mut slots).map_err(|e| format!("{e:?}"))?;
        let program = program::lower_with(root, &slots, 1.0).map_err(|e| format!("{e:?}"))?;
        Ok(PartProgram { slots, program, to_shape })
    }
    /// The program's words, as the kernels' `program` buffer holds them.
    pub fn words(&self) -> Vec<u32> {
        self.program.texels.iter().flatten().map(|f| f.to_bits()).collect()
    }
    /// A printer-mm point in shape space.
    pub fn shape_point(&self, p: [f64; 3]) -> [f32; 3] {
        let m = &self.to_shape;
        std::array::from_fn(|r| (m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]) as f32)
    }
}

/// Kernel `name` of `mod.gpuslice` with `slots`' kernels spliced into its field.
pub fn spliced(gpu: &mut Gpu, slots: &[KernelSlot], name: LiveId) -> Result<ComputeKernel, String> {
    gpu.cx.with_vm(|vm| {
        let module = vm.heap().module(id!(gpuslice));
        let base = vm.value(module, name).as_object().ok_or_else(|| format!("kernel gpuslice.{name} missing"))?;
        let mut splice = ComputeBake::new(vm, slots).ok_or("the part's kernels could not be spliced")?;
        let kernel = splice.draw_variant(vm, base).ok_or_else(|| "the spliced variant".to_string())
            .and_then(|object| ComputeKernel::new(vm, object, &format!("{name}")));
        splice.release(vm);
        kernel
    })
}

/// What one sample of a part costs on this GPU: `n` points (printer mm) through its field in one
/// timed dispatch, the best of five; ms, and the values for a check against the CPU.
pub fn sample_cost(gpu: &mut Gpu, part: &PartProgram, points: &[[f64; 3]]) -> Result<(f64, Vec<f32>), String> {
    let kernel = spliced(gpu, &part.slots, id!(Sample))?;
    let program = gpu.words(&part.words(), false)?;
    let pts: Vec<u32> = points.iter().flat_map(|p| { let q = part.shape_point(*p); [q[0].to_bits(), q[1].to_bits(), q[2].to_bits(), 0] }).collect();
    let pts = gpu.words(&pts, false)?;
    let n = points.len() as u32;
    let values = gpu.buffer(n as u64 * 4, false)?;
    let mut best = f64::INFINITY;
    let mut out = Vec::new();
    for rep in 0..6 {
        let name = "field sample";
        gpu_frame_timer::watch(name);
        let mut pass = ComputePass::new("field sample", None);
        dispatch(&mut pass, &kernel, Groups::Direct([n.div_ceil(64), 1, 1]), &[(id!(spec), &u4([n, 0, 0, 0]))],
            &[(id!(program), &program), (id!(pts), &pts), (id!(values), &values)], Some(name))?;
        let (bytes, _) = gpu.run(pass, &[(&values, 0, n as u64 * 4)])?;
        // the first run compiles and warms
        if rep > 0 {
            best = gpu.timer(name, 1, 200).iter().fold(best, |b, &ms| b.min(ms as f64));
        } else {
            let _ = gpu.timer(name, 1, 200);
        }
        out = crate::gpu::words_of(&bytes[0]).into_iter().map(f32::from_bits).collect();
    }
    Ok((best, out))
}

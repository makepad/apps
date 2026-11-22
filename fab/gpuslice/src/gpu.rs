//! The slicer's own GPU host on the plain HAL: a windowless Cx on an offscreen GPU session,
//! kernels by name, buffers, and a batch run as one submission (one compute pass) followed by
//! its readbacks. Nothing here belongs to another engine's job runtime.
use makepad_platform::makepad_hal::{backend_id, dialect, hal_open, open_flags, OpenReq};
use makepad_platform::*;
use std::{cell::RefCell, rc::Rc, time::Instant};

/// A windowless Cx on an offscreen GPU session.
pub struct Gpu {
    pub cx: Box<Cx>,
    /// "metal", "vulkan" or "d3d".
    pub backend: &'static str,
}

impl Gpu {
    /// Opens the GPU and registers the kernels' script modules (`register`); an error where
    /// there is no GPU with compute.
    pub fn open(register: impl FnOnce(&mut ScriptVm)) -> Result<Gpu, String> {
        let mut cx = Box::new(Cx::new(Box::new(|_, _| {})));
        cx.init_cx_os();
        let mut req = OpenReq::from_env(0, open_flags::OFFSCREEN_WINDOWS, 1.0);
        req.flags |= open_flags::OFFSCREEN_WINDOWS;
        let info = hal_open(&req);
        if info.session == 0 || info.backend == backend_id::NONE {
            return Err("no GPU".into());
        }
        cx.hal_attach_session(info);
        let backend = match info.dialect { dialect::MSL => "metal", dialect::SPIRV => "vulkan", dialect::HLSL => "d3d", _ => "other" };
        // (dropped, it closes the session it opened)
        let mut gpu = Gpu { cx, backend };
        if info.backend == backend_id::HEADLESS {
            return Err("no GPU".into());
        }
        if !gpu.cx.compute_supported() {
            return Err("the GPU backend has no compute".into());
        }
        gpu.cx.with_vm(register);
        Ok(gpu)
    }

    /// Kernel `name` of script module `module`.
    pub fn kernel(&mut self, module: LiveId, name: LiveId) -> Result<ComputeKernel, String> {
        self.cx.with_vm(|vm| {
            let m = vm.heap().module(module);
            let object = vm.value(m, name).as_object().ok_or_else(|| format!("kernel {module}.{name} missing: register its script module"))?;
            ComputeKernel::new(vm, object, &format!("{name}"))
        })
    }

    /// A storage buffer of `bytes` (rounded up to a word), `indirect` when dispatch sizes are read from it.
    pub fn buffer(&mut self, bytes: u64, indirect: bool) -> Result<GpuBuffer, String> {
        GpuBuffer::new(&mut self.cx, bytes.max(4).next_multiple_of(4), GpuBufferFlags { indirect, readback: true })
    }

    /// A buffer holding `words`.
    pub fn words(&mut self, words: &[u32], indirect: bool) -> Result<GpuBuffer, String> {
        let b = self.buffer(words.len() as u64 * 4, indirect)?;
        b.update_owned(&mut self.cx, 0, words.iter().flat_map(|w| w.to_le_bytes()).collect())?;
        Ok(b)
    }

    /// Submits `pass` (one submission) and reads back `reads` (buffer, byte offset, bytes) once
    /// it ran, turning the session until every read has landed: the bytes in order and the wall
    /// time from submit to the last read.
    pub fn run(&mut self, pass: ComputePass, reads: &[(&GpuBuffer, u64, u64)]) -> Result<(Vec<HostBuf>, f64), String> {
        let t = Instant::now();
        pass.submit(&mut self.cx);
        if reads.is_empty() {
            return Ok((Vec::new(), t.elapsed().as_secs_f64() * 1e3));
        }
        let landed: Rc<RefCell<Option<Result<Vec<HostBuf>, String>>>> = Rc::new(RefCell::new(None));
        let ranges: Vec<(&GpuBuffer, std::ops::Range<u64>)> = reads.iter().map(|&(b, offset, len)| (b, offset..offset + len)).collect();
        let into = landed.clone();
        self.cx.read(&ranges, move |_, r| { *into.borrow_mut() = Some(r.map_err(|e| format!("{e:?}"))); })
            .map_err(|e| format!("readback refused: {e:?}"))?;
        while landed.borrow().is_none() {
            if !self.cx.hal_session_turn(1_000_000, false) {
                return Err("the GPU session ended".into());
            }
        }
        let wall = t.elapsed().as_secs_f64() * 1e3;
        let out = landed.borrow_mut().take().unwrap()?;
        Ok((out, wall))
    }

    /// Turns the session until the watched timer `name` has `n` samples (they land with the
    /// pass's completion, independently of its readbacks), at most `turns` turns; its ms.
    pub fn timer(&mut self, name: &str, n: usize, turns: usize) -> Vec<f32> {
        let mut got = gpu_frame_timer::take_watched(name);
        for _ in 0..turns {
            if got.len() >= n { break; }
            if !self.cx.hal_session_turn(1_000_000, false) { break; }
            got.extend(gpu_frame_timer::take_watched(name));
        }
        got
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        self.cx.hal_close_session();
    }
}

/// Records one dispatch of `kernel`: uniforms by name (those the kernel reads), buffers by
/// binding name; `timer` names a per-dispatch GPU timer.
pub fn dispatch(pass: &mut ComputePass, kernel: &ComputeKernel, groups: Groups, uniforms: &[(LiveId, &[u8])], buffers: &[(LiveId, &GpuBuffer)], timer: Option<&str>) -> Result<(), String> {
    let mut params = kernel.params();
    for (name, bytes) in uniforms {
        if kernel.layout().slots.iter().any(|s| s.name == *name) {
            params.set(*name, bytes)?;
        }
    }
    let bound = kernel.layout().buffers.iter().map(|b| {
        buffers.iter().find(|(n, _)| *n == b.name).map(|(_, buf)| (*buf).clone()).ok_or_else(|| format!("no buffer for {}", b.name))
    }).collect::<Result<Vec<_>, String>>()?;
    match (groups, timer) {
        (Groups::Direct(g), None) => pass.dispatch(kernel, g, &params, &bound, &[]),
        (Groups::Direct(g), Some(t)) => pass.dispatch_timed(t, kernel, g, &params, &bound, &[]),
        (Groups::Indirect(args, at), None) => pass.dispatch_indirect(kernel, args, at, &params, &bound, &[]),
        (Groups::Indirect(args, at), Some(t)) => pass.dispatch_indirect_timed(t, kernel, args, at, &params, &bound, &[]),
    }
}

/// A dispatch's workgroups: given now, or read by the GPU from three words of an indirect
/// buffer at a byte offset (written by an earlier dispatch of the same pass).
#[derive(Clone, Copy)]
pub enum Groups<'a> {
    Direct([u32; 3]),
    Indirect(&'a GpuBuffer, u64),
}

/// Little-endian bytes of a uniform vec4u.
pub fn u4(v: [u32; 4]) -> [u8; 16] {
    let mut b = [0u8; 16];
    for (i, w) in v.iter().enumerate() { b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes()); }
    b
}

/// The u32 words of `bytes`.
pub fn words_of(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

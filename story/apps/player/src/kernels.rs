//! Kernels in the browser: every kernel compiled on wasm32 queues for
//! linking; once a frame the linker takes the queue, generates one wasm
//! module for it (one entry per kernel: SIMD128 where the generator has
//! that form, else scalar), has the page link it into this module's
//! function table and gives each kernel its slot. A kernel runs only on linked code: until then the
//! layer it feeds waits (no interpreter). Natively kernels are compiled to
//! native code where they are made, and there is nothing to do here.

#[cfg(target_arch = "wasm32")]
mod imp {
    use makepad_script_compute::kernel::{self, Kernel, KernelRef};
    use makepad_script_compute::wasm::Target;
    use makepad_widgets::*;


    /// Whether this engine's `f32x4.relaxed_madd` is fused (the compute
    /// backend's probe, linked and called once before any kernel links).
    #[derive(Default)]
    enum Fma {
        #[default]
        Unknown,
        Probing(WasmLinkId),
        Known(bool),
    }

    /// How this module's memory is imported: the threaded build's is a
    /// shared memory of 4 GiB at most.
    fn target(relaxed_fma: bool) -> Target {
        let shared = cfg!(target_feature = "atomics");
        Target { shared, max_pages: shared.then_some(65536), relaxed_fma }
    }

    #[derive(Default)]
    pub struct KernelLinker {
        fma: Fma,
        /// Links on their way: the kernels (kernel k is export k) and
        /// whether each one's entry is four wide.
        pending: Vec<(WasmLinkId, Vec<(KernelRef, bool)>)>,
        pub linked: usize,
        pub failed: usize,
    }

    impl KernelLinker {
        fn relaxed_fma(&mut self, cx: &mut Cx) -> Option<bool> {
            match self.fma {
                Fma::Known(fused) => return Some(fused),
                Fma::Unknown => self.fma = Fma::Probing(cx.link_wasm_module(makepad_script_compute::wasm::fma_probe(target(false)))),
                Fma::Probing(id) => {
                    let result = cx.wasm_link_result(id)?;
                    let fused = match result.ok().and_then(|slots| slots.first().copied()) {
                        Some(slot) if slot != 0 => {
                            // A linked export is a function pointer: its table slot.
                            let fma: extern "C" fn(f32, f32, f32) -> f32 = unsafe { std::mem::transmute(slot as usize) };
                            let [a, b, c] = makepad_script_compute::wasm::PROBE;
                            fma(a, b, c) == makepad_script_compute::wasm::PROBE_FUSED
                        }
                        // No relaxed SIMD here: the exact form.
                        _ => false,
                    };
                    log!("player: relaxed multiply-add is {}", if fused { "fused" } else { "not fused" });
                    self.fma = Fma::Known(fused);
                    return Some(fused);
                }
            }
            None
        }

        pub fn pump(&mut self, cx: &mut Cx) {
            // This (the UI) thread's table holds the linked kernel code.
            kernel::mark_wasm_link_thread();
            // Kernels wait in the queue until the probe answered.
            let Some(relaxed_fma) = self.relaxed_fma(cx) else { return };
            let kernels = kernel::take_unlinked();
            if !kernels.is_empty() {
                let refs: Vec<&Kernel> = kernels.iter().map(|k| k.as_ref()).collect();
                match kernel::wasm_module(&refs, target(relaxed_fma)) {
                    Some((bytes, index)) => {
                        log!("player: linking {} kernels ({} bytes)", kernels.len(), bytes.len());
                        let id = cx.link_wasm_module(bytes);
                        self.pending.push((id, kernels.into_iter().zip(index).collect()));
                    }
                    None => {
                        self.failed += kernels.len();
                        error!("player: {} kernels have no wasm form", kernels.len());
                    }
                }
            }
            let mut i = 0;
            while i < self.pending.len() {
                match cx.wasm_link_result(self.pending[i].0) {
                    None => i += 1,
                    Some(result) => {
                        let (_, kernels) = self.pending.remove(i);
                        match result {
                            Ok(slots) => {
                                for (i, (k, simd)) in kernels.iter().enumerate() {
                                    let slot = slots.get(i).copied().unwrap_or(0);
                                    if *simd {
                                        k.set_wasm_slots(0, slot);
                                    } else {
                                        k.set_wasm_slots(slot, 0);
                                    }
                                }
                                self.linked += kernels.len();
                                log!("player: {} kernels linked ({} in all)", kernels.len(), self.linked);
                            }
                            Err(e) => {
                                self.failed += kernels.len();
                                error!("player: linking {} kernels: {e}", kernels.len());
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use makepad_widgets::*;

    #[derive(Default)]
    pub struct KernelLinker;

    impl KernelLinker {
        pub fn pump(&mut self, _cx: &mut Cx) {}
    }
}

pub use imp::KernelLinker;

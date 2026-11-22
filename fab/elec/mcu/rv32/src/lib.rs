//! RV32IMC instruction-set model of the ESP32-C3 core for microsplash compiler tests.
//!
//! Implements RV32I, M, integer compressed instructions (RV32 Zca), Zicsr,
//! FENCE.I, and machine-mode trap entry/return with the C3 CSRs specified by
//! this model. Instruction bytes and data are little-endian; addresses are bytes.
//! [`sim::Cpu::instret`] counts completed instructions, not cycles. There are no
//! interrupts, caches, timing, other privilege modes, or built-in peripherals;
//! peripherals implement the emulator core's bus ([`sim::Bus`]). This is not a complete C3 SoC model.
//!
//! Misaligned loads/stores trap before accessing the bus (unprivileged manual
//! §2.1.6 permits this EEI choice). Whether the C3 traps or handles these in
//! hardware is unknown; the compiler never emits misaligned accesses. Only M is
//! modeled: an MRET whose MPP names U stops the run as unmodeled. WFI retires and stops; there is no interrupt source to wake the core.
//! Reserved and unsupported instructions are illegal, including all FP/atomic
//! instructions. No assembler, emulator implementation, or external dependency
//! is used by this oracle.

pub mod insn;
pub mod sim;

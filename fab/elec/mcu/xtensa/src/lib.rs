//! Little-endian Xtensa LX oracle for CALL0 and windowed firmware.
//!
//! Independently derived from Cadence's *Xtensa Instruction Set Architecture
//! (ISA) Summary for all Xtensa LX Processors*. Page references in this crate
//! refer to that document's printed/PDF page numbers. The encoding diagrams
//! on pp. 656–659 were read from the PDF, not inferred from another encoder.
//!
//! Models the core instructions of §3.8, Code Density, 32-bit multiply (with
//! configurable high product), configurable 32-bit divide, Exception Option 2,
//! relocatable vectors, the three unaligned-access behaviours (none, the exception option,
//! hardware handling as on the S3), a configurable EXCMLEVEL, and level-1
//! interrupts and timers, the Boolean Option, and the single-precision FP
//! Coprocessor Option. FP arithmetic and conversions round exactly in all four
//! modes, including fused multiply-add/subtract, gradual underflow and sticky
//! flags. Coprocessor-0 access is gated by CPENABLE. The 64-register Windowed
//! Register Option models CALL4/8/12, CALLX4/8/12, ENTRY, RETW/RETW.N, MOVSP,
//! ROTW, L32E/S32E and RFWO/RFWU, including overflow, underflow and alloca.
//! Public `ar` stays the 16 visible registers; a private physical file is
//! synchronized on rotation. Direct WindowBase writes relabel `ar`, whereas
//! WSR/XSR.WINDOWBASE rotate it. `physical_ar()` supplies a synchronized view.
//! BREAK and BREAK.N stop without implementing debug state. All addresses
//! are byte addresses.
//!
//! There are no caches, pipeline, allocation on the step path, or cycle timing.
//! Each retired instruction increments CCOUNT once. Bus accesses are ordered;
//! synchronizations (including EXCW and implementation-defined FSYNC, p. 434)
//! consequently have no additional effect. Special-register writes become
//! visible immediately (pp. 70, 277–278 permit implementation-dependent latency).
//!
//! FP stops: unspecified approximation tables (DIV0.S, RECIP0.S, SQRT0.S,
//! RSQRT0.S), unspecified adjustment helpers (DIVN.S, MKDADJ.S, MKSADJ.S),
//! arithmetic NaN result bits, signaling-sensitive comparisons on NaNs,
//! enabled FP traps (including undetermined Underflow, p. 109), reading FSR
//! with unknown Underflow (p. 109; result and Inexact are delivered, sticky U
//! makes it known, WUR.FSR clears the unknown mark), MBZ writes, reserved
//! CONST.S indices, UTRUNC.S's unclear Invalid flag for negative signed-range
//! results, and NEXP01.S's conflicting
//! formula/range rule below one (except normal powers of four). Bit copies,
//! explicit NEXP01.S special values, NaN-to-integer saturation, and OLE.S/OLT.S
//! on NaNs are modeled. Boolean reductions stop on undefined source alignment;
//! WSR.BR/XSR.BR stop on undefined nonzero high bits.
//!
//! Window stops: undefined ENTRY/RETW state and nonzero high bits written to
//! WindowBase/WindowStart; CALLX source-register overflow where the generic
//! and instruction-specific checks conflict. CALLXn reads its target before
//! writing a(4n), as §6.1.4 says (CALLX8's own page omits the temporary;
//! compiled long calls rely on `CALLX8 a8`). RETW preserves CALLINC, choosing the
//! variant in pp. 247, 568 (p. 567 permits an optional update). Dedicated
//! window exceptions enter their vectors in both TrapModes; general faults
//! including alloca follow TrapMode. No window overflow is possible in EXCM.
//! LBEG/LEND/LCOUNT are stored and cloned; loop-back stops when LCOUNT is
//! nonzero, EXCM is clear and the sequential next PC equals LEND (pp. 53, 86).
//! Config-gated MAC16 ACCLO/ACCHI/M0–M3 and Conditional Store SCOMPARE1
//! have RSR/WSR/XSR storage (pp. 280–283); ACCHI reads sign-extend and writes
//! stop on a non-sign-extended byte. THREADPTR has RUR/WUR storage (p. 316).
//! These registers are cloned; undefined reset values choose zero.
//!
//! Not modeled: double precision, Loop instructions/loop-back, MAC16 arithmetic, 16-bit multiply,
//! Miscellaneous Operations (NSA, MIN/MAX, CLAMPS, SEXT), Conditional
//! Store (S32C1I), Exclusive Access (L32EX), Extended L32R (LITBASE), debug and
//! memory-error state, MMU/MPU options, S3 vector extensions, and CUST0/CUST1.
//! Those encodings are rejected. Double exceptions, high-priority interrupts,
//! RFDE/RFDO/RFDD/RFI instead produce named stops.
//! PS.RING is stored but provides no protection: without MMU/MPU, CRING is zero
//! (p. 134). Board address permissions are enforced by the emulator core's bus.
//!
//! Model choices and document inconsistencies are recorded beside the relevant
//! operations. In particular, bus errors are precise, including stores, as
//! required by the simulator contract (contrast the hardware discussion p. 58).
//! A bus must leave storage unchanged when a write returns `Err(())`; the CPU
//! cannot undo device side effects through the supplied bus interface.

pub mod insn;
pub mod sim;
mod fp;
#[cfg(feature = "jit")]
pub mod jit;

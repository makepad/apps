//! AVR encoding and label assembly for microsplash's ATmega328P backend.
//!
//! [`sim`] is a deterministic instruction/peripheral oracle for compiler tests,
//! not a firmware runtime. Addresses in [`insn`] and [`asm`] are flash **word**
//! addresses; LPM and the simulator's data bus use byte addresses. No host AVR
//! tools or external dependencies are required. [`machine`] adapts the oracle to
//! the shared co-simulation machine interface.

pub mod insn;
pub mod asm;
pub mod sim;
pub mod pins;
pub mod chip;
pub mod machine;

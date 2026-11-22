//! microsplash's forthcoming 32-bit back end targets the ESP32-C3 (RV32IMC)
//! and ESP32-S3 (Xtensa LX7). This first piece provides the RV32 instruction
//! encoders and label assembler ported from MicroPython, with checked operands
//! and a caller-selected scratch register; frames come later.
//! The Xtensa CALL0 encoder also provides checked instructions, label helpers, and literal tables.

pub mod asm;
pub mod bounds;
pub mod control;
pub mod literals;
pub mod regalloc;
pub mod rv32;
pub mod xtensa;

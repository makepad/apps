//! Phase 6a compiler limits. Phase 6d completes the pin, PWM and serial tables.
use fab_elec_mcu_rt::{pwm, serial, Profile};

pub const PROFILE_ID: u32 = 4;

/// f32 is always supported by rt::Profile; f64 is explicitly disabled here.
/// Reserve 32 KiB of the S3's 416 KiB SRAM1 (TRM pp. 403–404, facts.md)
/// for runtime use, leaving a shared 384 KiB pool for state, scratch and buffers.
/// Long handlers are legal: the notes mark those above 20 ms, and a bound the image's
/// RWDT cannot cover is refused. A handler's cost travels in one module word (the loader
/// refuses a high word), so a cost past that word is refused at its source.
/// Pin numbers cover 0..48 provisionally; 6d supplies the usable pin table.
pub const fn profile() -> Profile {
    Profile {
        id: PROFILE_ID, pins: 49, max_entry_ops: u32::MAX as u64, call_ops: None, ram_words: 384 * 1024 / 4, buffer_bytes: 384 * 1024, eeprom: 0, eeprom_write_ns: 0,
        f64: false, op_ns: 1_000_000_000_u32.div_ceil(crate::CPU_HZ), clock_hz: crate::CPU_HZ,
        pwm: &pwm::NONE, serial: &serial::NONE,
        analog: &fab_elec_mcu_rt::analog::NONE,
        display: None,
        motion: None, touch: false, audio: None,
    }
}

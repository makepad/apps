//! The ATmega328P as the microsplash compiler and this image builder target it: its runtime
//! profile at a CPU clock, and the names a program uses for its pins, from the chip's one pin
//! table ([`fab_elec_mcu_avr::chip`]), which also gives the machine's pin rows.

pub use fab_elec_mcu_avr::chip::{by_name, by_number, row, ChipPin, ANALOG_ONLY, PINS};
use fab_elec_mcu_rt::{analog, pwm, serial, Profile};

/// The profile id a module for the ATmega328P carries.
pub const PROFILE_ID: u32 = 3;

/// The ATmega328P's runtime profile with its CPU clock at `f_clk` Hz (above 0): its 20 GPIO and
/// two analog-only pins, 2 KB of RAM, no f64, the handler cost bound, and its analog, PWM and
/// serial tables. Periods and baud rates use that clock. A nominal interpreted op is one CPU cycle.
pub const fn profile(f_clk: u32) -> Profile {
    Profile {
        id: PROFILE_ID,
        pins: (PINS.len() + ANALOG_ONLY.len()) as u32,
        // 62.5 ms of ops at any clock (1,000,000 at 16 MHz): a limit in time, so a 16-byte EEPROM
        // save (16 accesses of up to 3.663 ms each) fits one handler at every clock.
        max_entry_ops: f_clk as u64 / 16,
        call_ops: None,
        ram_words: 2048,
        buffer_bytes: 0,
        eeprom: 1024,
        // DS40002061B Table 8-2: 26,368 calibrated RC cycles. Table 29-9:
        // factory 8 MHz ±10% at 3 V, 25 °C; charge the 7.2 MHz end, rounded up.
        // Note: the write figure is typical and calibration covers one operating
        // point only. This full-range write bound is Assumed, not guaranteed.
        // The emulator's nominal 3.4 ms must be at or below this charged time.
        eeprom_write_ns: (26_368u64 * 1_000_000_000).div_ceil(7_200_000) as u32,
        f64: false,
        op_ns: 1_000_000_000 / f_clk,
        clock_hz: f_clk,
        pwm: &pwm::ATMEGA328P,
        serial: &serial::ATMEGA328P,
        analog: &analog::ATMEGA328P,
        display: None,
        motion: None, touch: false, audio: None,
    }
}

/// The pin names a program may use, each the runtime number of its pin: the data sheet's port
/// pin names (`PD6`), as the part names its pins.
pub fn compiler_pins() -> Vec<(&'static str, u32)> {
    PINS.iter().map(|p| (p.name, p.number))
        .chain(ANALOG_ONLY.iter().map(|p| (p.name, p.number))).collect()
}

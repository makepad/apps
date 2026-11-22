//! The chip's analog inputs and conversion resolution.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalogPin { pub pin: u32, pub channel: u8, pub digital: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalogTable { pub pins: &'static [AnalogPin], pub bits: u8 }

pub const NONE: AnalogTable = AnalogTable { pins: &[], bits: 0 };

/// The ATmega328P (DS40002061B section 24): PC0..PC5, runtime pins 14..19, are
/// ADC0..ADC5 and keep their digital function; ADC6 and ADC7, runtime pins 20
/// and 21 (the TQFP/QFN packages), are analog only. 10 bits against AVCC.
pub const ATMEGA328P: AnalogTable = AnalogTable {
    pins: &[
        AnalogPin { pin: 14, channel: 0, digital: true },
        AnalogPin { pin: 15, channel: 1, digital: true },
        AnalogPin { pin: 16, channel: 2, digital: true },
        AnalogPin { pin: 17, channel: 3, digital: true },
        AnalogPin { pin: 18, channel: 4, digital: true },
        AnalogPin { pin: 19, channel: 5, digital: true },
        AnalogPin { pin: 20, channel: 6, digital: false },
        AnalogPin { pin: 21, channel: 7, digital: false },
    ],
    bits: 10,
};

impl AnalogTable {
    pub fn get(&self, pin: u32) -> Option<&AnalogPin> {
        self.pins.iter().find(|p| p.pin == pin)
    }

    pub fn max(&self) -> i32 {
        ((1u32 << self.bits) - 1) as i32
    }
}

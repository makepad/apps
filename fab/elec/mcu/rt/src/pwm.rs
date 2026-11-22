//! PWM capabilities and resolved settings, in CPU clock cycles.

/// How a PWM timer counts (non-inverting output in both).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PwmMode {
    /// Counts 0..=TOP and wraps. A new level takes effect at BOTTOM.
    Fast,
    /// Counts 0..=TOP..=0. A new level takes effect at TOP.
    PhaseCorrect,
}

/// One timer that drives PWM pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PwmTimer {
    pub name: &'static str,
    pub mode: PwmMode,
    /// TOP of the table settings (255: 8-bit).
    pub top: u16,
    /// The table settings' prescalers (clk/N), ascending. The clock timer has exactly one.
    pub prescalers: &'static [u16],
    /// Largest free TOP (any TOP in 3..=free_top through `free_top_reg`); 0: none.
    pub free_top: u16,
    pub free_top_reg: &'static str,
    /// The device clock runs on this timer: its one setting is fixed and never changes.
    pub clock: bool,
}

/// A pin with a PWM output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PwmPin { pub pin: u32, pub timer: u8, pub output: &'static str }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PwmTable { pub pins: &'static [PwmPin], pub timers: &'static [PwmTimer] }

/// What a timer is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PwmSetting { pub timer: u8, pub mode: PwmMode, pub prescale: u16, pub top: u16 }

/// A resolved PWM declaration (declaration order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PwmOut { pub decl: u32, pub pin: u32, pub asked_mhz: u32, pub real_mhz: u64, pub setting: PwmSetting }

pub const NONE: PwmTable = PwmTable { pins: &[], timers: &[] };

pub const ATMEGA328P: PwmTable = PwmTable {
    timers: &[
        PwmTimer { name: "Timer0", mode: PwmMode::Fast, top: 255, prescalers: &[8], free_top: 0, free_top_reg: "", clock: true },
        PwmTimer { name: "Timer1", mode: PwmMode::PhaseCorrect, top: 255, prescalers: &[1, 8, 64, 256, 1024], free_top: 0xFFFF, free_top_reg: "ICR1", clock: false },
        PwmTimer { name: "Timer2", mode: PwmMode::PhaseCorrect, top: 255, prescalers: &[1, 8, 32, 64, 128, 256, 1024], free_top: 0, free_top_reg: "", clock: false },
    ],
    pins: &[
        PwmPin { pin: 3, timer: 2, output: "OC2B" },
        PwmPin { pin: 5, timer: 0, output: "OC0B" },
        PwmPin { pin: 6, timer: 0, output: "OC0A" },
        PwmPin { pin: 9, timer: 1, output: "OC1A" },
        PwmPin { pin: 10, timer: 1, output: "OC1B" },
        PwmPin { pin: 11, timer: 2, output: "OC2A" },
    ],
};

pub const TOLERANCE_PERCENT: u64 = 3;

impl PwmSetting {
    pub fn period_cycles(&self) -> u64 {
        let counts = match self.mode {
            PwmMode::Fast => self.top as u64 + 1,
            PwmMode::PhaseCorrect => 2 * self.top as u64,
        };
        counts * self.prescale as u64
    }

    pub fn real_mhz(&self, clock_hz: u32) -> u64 {
        let p = self.period_cycles();
        if p == 0 { return 0; }
        (clock_hz as u64 * 1000 + p / 2) / p
    }

    pub fn ocr(&self, level: u8) -> u16 {
        if self.top == 255 { level as u16 } else { ((level as u32 * self.top as u32 + 127) / 255) as u16 }
    }

    pub fn high_cycles(&self, level: u8) -> u64 {
        let ocr = self.ocr(level) as u64;
        let n = self.prescale as u64;
        match self.mode {
            PwmMode::PhaseCorrect => 2 * ocr * n,
            PwmMode::Fast if level == 0 => 0,
            PwmMode::Fast if ocr == self.top as u64 => self.period_cycles(),
            PwmMode::Fast => (ocr + 1) * n,
        }
    }
}

/// Within the request's inclusive tolerance, without overflowing even for u64 inputs.
pub fn matches(real_mhz: u64, asked_mhz: u64) -> bool {
    // floor(asked * tolerance / 100), splitting before the multiplication.
    let tolerance = asked_mhz / 100 * TOLERANCE_PERCENT + asked_mhz % 100 * TOLERANCE_PERCENT / 100;
    real_mhz.abs_diff(asked_mhz) <= tolerance
}

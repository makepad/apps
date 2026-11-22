use fab_elec_mcu_emu::board::{I2cAns, I2cOp, Payload, Res, SpiXfer};
use fab_elec_mcu_emu::{Drive, Pull};

use crate::{Cx, Model, Stimulus};

/// A released or pressed switch to ground.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Key {
    down: bool,
}

impl Model for Key {
    fn pins(&self) -> &'static [&'static str] {
        &["A"]
    }

    fn drive(&self, pin: u8) -> Drive {
        assert_eq!(pin, 0, "Key has only pin A");
        if self.down {
            Drive::Low
        } else {
            Drive::Input(Pull::None)
        }
    }

    fn pin(&mut self, _cx: &mut Cx<'_>, pin: u8, _high: bool) -> Res {
        match pin {
            0 => Ok(()),
            _ => Err("Key: unknown pin"),
        }
    }

    fn next_event(&self) -> Option<u64> {
        None
    }

    fn event(&mut self, _cx: &mut Cx<'_>) -> Res {
        Ok(())
    }

    fn i2c(&mut self, _cx: &mut Cx<'_>, _op: I2cOp) -> Res<I2cAns> {
        Err("Key has no I2C")
    }

    fn spi(&mut self, _cx: &mut Cx<'_>, _x: &SpiXfer, _tx: &dyn Payload, _rx: &mut Vec<u8>) -> Res {
        Err("Key has no SPI")
    }

    fn input(&mut self, _cx: &mut Cx<'_>, ev: &Stimulus) -> Res {
        let Stimulus::Key { down, .. } = *ev else {
            return Err("Key takes only key stimulus");
        };
        self.down = down;
        Ok(())
    }
}

/// An N-channel switch with its source grounded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Not {
    gate_high: bool,
}

impl Model for Not {
    fn pins(&self) -> &'static [&'static str] {
        &["G", "D"]
    }

    fn drive(&self, pin: u8) -> Drive {
        match pin {
            0 => Drive::Input(Pull::None),
            1 => Drive::OpenDrain { low: self.gate_high, pull: Pull::None },
            _ => panic!("Not has only pins G and D"),
        }
    }

    fn pin(&mut self, _cx: &mut Cx<'_>, pin: u8, high: bool) -> Res {
        match pin {
            0 => self.gate_high = high,
            1 => (),
            _ => return Err("Not: unknown pin"),
        }
        Ok(())
    }

    fn next_event(&self) -> Option<u64> {
        None
    }

    fn event(&mut self, _cx: &mut Cx<'_>) -> Res {
        Ok(())
    }

    fn i2c(&mut self, _cx: &mut Cx<'_>, _op: I2cOp) -> Res<I2cAns> {
        Err("Not has no I2C")
    }

    fn spi(&mut self, _cx: &mut Cx<'_>, _x: &SpiXfer, _tx: &dyn Payload, _rx: &mut Vec<u8>) -> Res {
        Err("Not has no SPI")
    }

    fn input(&mut self, _cx: &mut Cx<'_>, _ev: &Stimulus) -> Res {
        Err("Not takes no stimulus")
    }
}

/// A speaker amplifier with an enable pin. Its inputs sit on a codec's analog output nets (the
/// board routes the codec's sound to it). EN at its enable level starts it, and `start_ms` later
/// it passes that sound times its gain to the speaker, saturating at full scale; otherwise the
/// speaker is silent. The enable level, the gain and the start time are the board's data
/// (`PartKind::Amp`), from the amplifier's datasheet, never a chip's built-in values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Amp {
    enable_high: bool,
    gain_db: i8,
    start_ms: u16,
    start_ticks: u64,
    /// The tick its output starts, once EN enables it; None while EN disables it.
    on_at: Option<u64>,
    /// Its model limit was noted (once per run).
    noted: bool,
}

impl Amp {
    pub const PINS: &'static [&'static str] = &["EN", "INP", "INN"];

    pub fn new(enable_high: bool, gain_db: i8, start_ms: u16) -> Self {
        Self { enable_high, gain_db, start_ms, start_ticks: 0, on_at: None, noted: false }
    }

    pub(crate) fn bind(&mut self, tick_hz: u64) -> Result<(), String> {
        self.start_ticks = u64::from(self.start_ms) * tick_hz / 1000;
        Ok(())
    }

    /// The speaker's sample for the codec's `sample` at tick `t`.
    pub fn speak(&self, t: u64, sample: i16) -> i16 {
        if !self.on_at.is_some_and(|at| t >= at) { return 0; }
        let y = (f64::from(sample) * 10f64.powf(f64::from(self.gain_db) / 20.0)).round();
        y.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
    }
}

impl Model for Amp {
    fn pins(&self) -> &'static [&'static str] {
        Self::PINS
    }

    fn drive(&self, pin: u8) -> Drive {
        assert!(pin < 3, "Amp has only pins EN, INP and INN");
        Drive::Input(Pull::None)
    }

    fn pin(&mut self, cx: &mut Cx<'_>, pin: u8, high: bool) -> Res {
        match pin {
            0 if high == self.enable_high => {
                if self.on_at.is_none() { self.on_at = Some(cx.t + self.start_ticks); }
            }
            0 => {
                if !self.noted {
                    self.noted = true;
                    cx.note("AMP: the turn-off time is not modelled; the speaker is silent once EN disables it");
                }
                self.on_at = None;
            }
            1 | 2 => (),
            _ => return Err("Amp: unknown pin"),
        }
        Ok(())
    }

    fn next_event(&self) -> Option<u64> {
        None
    }

    fn event(&mut self, _cx: &mut Cx<'_>) -> Res {
        Ok(())
    }

    fn i2c(&mut self, _cx: &mut Cx<'_>, _op: I2cOp) -> Res<I2cAns> {
        Err("Amp has no I2C")
    }

    fn spi(&mut self, _cx: &mut Cx<'_>, _x: &SpiXfer, _tx: &dyn Payload, _rx: &mut Vec<u8>) -> Res {
        Err("Amp has no SPI")
    }

    fn input(&mut self, _cx: &mut Cx<'_>, _ev: &Stimulus) -> Res {
        Err("Amp takes no stimulus")
    }
}

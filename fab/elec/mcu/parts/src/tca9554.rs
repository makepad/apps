//! TCA9554 registers, I2C framing and delayed pin changes (E01–E08, E10, O02).

use std::collections::VecDeque;

use fab_elec_mcu_emu::board::{I2cAns, I2cOp, Payload, Res, SpiXfer};
use fab_elec_mcu_emu::{Drive, Pull};

use crate::{Cx, I2cTarget, Model, Stimulus, TargetOp};

const POINTER_POISON: &str = "TCA9554: register access before its pointer is set; the pointer's power-up value is undocumented";
const COMMAND_UNDOCUMENTED: &str = "TCA9554: command byte outside its four registers; undocumented";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Registers {
    target: I2cTarget,
    pointer: Option<u8>,
    command: bool,
    sampled: Option<u8>,
    output: u8,
    polarity: u8,
    config: u8,
    active_output: u8,
    active_config: u8,
    levels: u8,
    known: u8,
    remembered: u8,
    interrupt: bool,
    requested_interrupt: bool,
    delays: Option<(u64, u64)>,
    output_note: bool,
    interrupt_note: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Output { output: u8, config: u8 },
    Interrupt(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    t: u64,
    change: Change,
}

/// Eight GPIOs with a persistent register pointer and active-low INT (E01–E08).
#[derive(Debug, PartialEq, Eq)]
pub struct Tca9554 {
    r: Registers,
    pending: VecDeque<Pending>,
}

impl Clone for Tca9554 {
    fn clone(&self) -> Self {
        Self { r: self.r, pending: self.pending.clone() }
    }

    fn clone_from(&mut self, source: &Self) {
        self.r = source.r;
        self.pending.clone_from(&source.pending);
    }
}

impl Tca9554 {
    /// Its model pins, in the order the netlist binds; supplies are implicit.
    pub const PINS: &'static [&'static str] = &["P0", "P1", "P2", "P3", "P4", "P5", "P6", "P7", "INT", "SCL", "SDA"];

    /// E01: the board selects address 0x20 through 0x27.
    pub fn new(addr: u8) -> Result<Self, String> {
        if !(0x20..=0x27).contains(&addr) {
            return Err("TCA9554: address must be 0x20..0x27 (E01)".into());
        }
        Ok(Self {
            r: Registers {
                target: I2cTarget::new(addr),
                pointer: None, // E10: no documented power-up pointer.
                command: false,
                sampled: None,
                output: 0xff, // E03.
                polarity: 0, // E04.
                config: 0xff, // E05.
                active_output: 0xff,
                active_config: 0xff,
                levels: 0,
                known: 0,
                remembered: 0,
                interrupt: false,
                requested_interrupt: false,
                delays: None,
                output_note: false,
                interrupt_note: false,
            },
            pending: VecDeque::new(),
        })
    }

    /// E08: convert the 350 ns and 4 us maxima once, rounding up.
    pub fn bind(&mut self, tick_hz: u64) -> Result<(), String> {
        if tick_hz == 0 {
            return Err("TCA9554: tick rate must be nonzero".into());
        }
        if self.r.delays.is_some() {
            return Err("TCA9554: already bound".into());
        }
        let ticks = |ns: u128| -> u64 {
            ((ns * u128::from(tick_hz) + 999_999_999) / 1_000_000_000) as u64
        };
        self.r.delays = Some((ticks(350), ticks(4_000)));
        Ok(())
    }

    fn schedule(&mut self, t: u64, delay: u64, change: Change) -> Res {
        let t = t.checked_add(delay).ok_or("TCA9554: event tick overflow")?;
        let index = self.pending.iter().position(|event| event.t > t)
            .unwrap_or(self.pending.len());
        self.pending.insert(index, Pending { t, change });
        Ok(())
    }

    /// E05: input pulls up; output presents its latch.
    fn port_drive(output: u8, config: u8, pin: u8) -> Drive {
        let mask = 1 << pin;
        if config & mask != 0 {
            Drive::Input(Pull::Up)
        } else if output & mask != 0 {
            Drive::High
        } else {
            Drive::Low
        }
    }

    /// E08: only input pins can request INT.
    fn update_interrupt(&mut self, cx: &mut Cx<'_>) -> Res {
        if cx.initializing {
            self.r.remembered = self.r.levels;
            return Ok(());
        }
        let asserted = (self.r.levels ^ self.r.remembered)
            & self.r.active_config & self.r.known != 0;
        if !asserted {
            self.r.remembered = self.r.levels;
        }
        if asserted != self.r.requested_interrupt {
            let (_, delay) = self.r.delays.ok_or("TCA9554: interrupt before bind")?;
            self.schedule(cx.t, delay, Change::Interrupt(asserted))?;
            self.r.requested_interrupt = asserted;
            if !self.r.interrupt_note {
                cx.note("TCA9554 interrupt valid and reset at their 4 us maximum");
                self.r.interrupt_note = true;
            }
        }
        Ok(())
    }

    /// E02–E05, E07: sample the selected register at the preceding ACK.
    fn sample(&mut self, cx: &mut Cx<'_>) -> Res<u8> {
        match self.r.pointer.ok_or(POINTER_POISON)? {
            0 => {
                if self.r.known != 0xff {
                    return Err("TCA9554: Input Port read before its pin levels are resolved");
                }
                self.r.remembered = self.r.levels;
                self.update_interrupt(cx)?;
                Ok(self.r.levels ^ (self.r.polarity & self.r.config))
            }
            1 => Ok(self.r.output),
            2 => Ok(self.r.polarity),
            3 => Ok(self.r.config),
            _ => Err(COMMAND_UNDOCUMENTED),
        }
    }

    /// E02–E06: data writes keep the pointer; Input Port ignores writes.
    fn write(&mut self, cx: &mut Cx<'_>, value: u8) -> Res {
        let old_output = self.r.output;
        let old_config = self.r.config;
        match self.r.pointer.ok_or(POINTER_POISON)? {
            0 => (),
            1 => self.r.output = value,
            2 => self.r.polarity = value,
            3 => self.r.config = value,
            _ => return Err(COMMAND_UNDOCUMENTED),
        }
        if (0..8).any(|pin| {
            Self::port_drive(old_output, old_config, pin)
                != Self::port_drive(self.r.output, self.r.config, pin)
        }) {
            let (delay, _) = self.r.delays.ok_or("TCA9554: output change before bind")?;
            self.schedule(cx.t, delay, Change::Output {
                output: self.r.output,
                config: self.r.config,
            })?;
            if !self.r.output_note {
                cx.note("TCA9554 output valid at its 350 ns maximum after the ACK");
                self.r.output_note = true;
            }
        }
        Ok(())
    }
}

impl Model for Tca9554 {
    fn pins(&self) -> &'static [&'static str] {
        Self::PINS
    }

    fn drive(&self, pin: u8) -> Drive {
        match pin {
            0..=7 => Self::port_drive(self.r.active_output, self.r.active_config, pin),
            8 => Drive::OpenDrain { low: self.r.interrupt, pull: Pull::None }, // E08.
            9 | 10 => Drive::Input(Pull::None), // E06–E07: bytes use the bus seam.
            _ => panic!("TCA9554: unknown pin"),
        }
    }

    fn pin(&mut self, cx: &mut Cx<'_>, pin: u8, high: bool) -> Res {
        match pin {
            0..=7 => {
                let mask = 1 << pin;
                self.r.levels = (self.r.levels & !mask) | if high { mask } else { 0 };
                if self.r.known & mask == 0 {
                    self.r.remembered = (self.r.remembered & !mask) | (self.r.levels & mask);
                    self.r.known |= mask;
                }
                self.update_interrupt(cx)
            }
            8..=10 => Ok(()),
            _ => Err("TCA9554: unknown pin"),
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.pending.front().map(|event| event.t)
    }

    fn event(&mut self, cx: &mut Cx<'_>) -> Res {
        while self.next_event() == Some(cx.t) {
            let event = self.pending.pop_front().expect("a due TCA9554 event exists");
            match event.change {
                Change::Output { output, config } => {
                    self.r.active_output = output;
                    self.r.active_config = config;
                    self.update_interrupt(cx)?;
                }
                Change::Interrupt(asserted) => self.r.interrupt = asserted,
            }
        }
        Ok(())
    }

    fn i2c_addr(&self) -> Option<u8> {
        Some(self.r.target.addr())
    }

    fn i2c(&mut self, cx: &mut Cx<'_>, op: I2cOp) -> Res<I2cAns> {
        let selected = self.r.target.step(op).map_err(|_| {
            "TCA9554: I2C operation outside its documented transfer framing"
        })?;
        match selected {
            TargetOp::None => Ok(I2cAns::None),
            TargetOp::Address { read } => {
                self.r.command = !read;
                self.r.sampled = if read { Some(self.sample(cx)?) } else { None };
                Ok(I2cAns::Ack(true))
            }
            TargetOp::Write(value) => {
                if self.r.command {
                    if value > 3 {
                        return Err(COMMAND_UNDOCUMENTED);
                    }
                    self.r.pointer = Some(value);
                    self.r.command = false;
                } else {
                    self.write(cx, value)?;
                }
                Ok(I2cAns::Ack(true))
            }
            TargetOp::Read { ack } => {
                let value = self.r.sampled.ok_or("TCA9554: read without a preceding ACK sample")?;
                self.r.sampled = if ack { Some(self.sample(cx)?) } else { None };
                Ok(I2cAns::Byte(value))
            }
        }
    }

    fn input(&mut self, _cx: &mut Cx<'_>, _ev: &Stimulus) -> Res {
        Err("TCA9554 takes no stimulus")
    }

    fn spi(&mut self, _cx: &mut Cx<'_>, _x: &SpiXfer, _tx: &dyn Payload, _rx: &mut Vec<u8>) -> Res {
        Err("TCA9554 has no SPI")
    }
}

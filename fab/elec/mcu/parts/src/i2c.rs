use fab_elec_mcu_emu::board::{I2cOp, Res};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Address,
    Write,
    Read,
    ReadEnd,
}

/// The selected part's side of an I2C step (E01, E06, E07).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetOp {
    None,
    Address { read: bool },
    Write(u8),
    Read { ack: bool },
}

/// Address selection shared by I2C parts (E01, E06, E07).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct I2cTarget {
    addr: u8,
    phase: Phase,
}

impl I2cTarget {
    pub fn new(addr: u8) -> Self {
        Self { addr, phase: Phase::Idle }
    }

    pub fn addr(&self) -> u8 {
        self.addr
    }

    pub fn step(&mut self, op: I2cOp) -> Res<TargetOp> {
        match op {
            I2cOp::Start => self.phase = Phase::Address,
            I2cOp::Stop => self.phase = Phase::Idle,
            I2cOp::Write(value) => match self.phase {
                Phase::Address => {
                    if value >> 1 == self.addr {
                        let read = value & 1 != 0;
                        self.phase = if read { Phase::Read } else { Phase::Write };
                        return Ok(TargetOp::Address { read });
                    }
                    self.phase = Phase::Idle;
                }
                Phase::Write => return Ok(TargetOp::Write(value)),
                Phase::Read | Phase::ReadEnd => return Err("I2C target: write during a read transaction"),
                Phase::Idle => (),
            },
            I2cOp::Read { ack } => match self.phase {
                Phase::Read => {
                    if !ack {
                        self.phase = Phase::ReadEnd;
                    }
                    return Ok(TargetOp::Read { ack });
                }
                Phase::Write => return Err("I2C target: read during a write transaction"),
                Phase::ReadEnd => return Err("I2C target: read after the master's NACK"),
                Phase::Address | Phase::Idle => (),
            },
        }
        Ok(TargetOp::None)
    }
}

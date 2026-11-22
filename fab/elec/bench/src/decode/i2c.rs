//! I2C on the decoder core, from the bus specification: SDA falling while SCL is high starts a
//! transaction (again, while one is open: a repeated start), SDA rising while SCL is high stops
//! it; SDA is read at each SCL rise and is a bit once SCL falls again without a condition between
//! (the SCL rise before a stop or repeated start is no bit), eight to a byte MSB first, the ninth
//! the receiver's acknowledge (SDA low). The first byte after a start is the address and the
//! direction (7-bit addressing).

use super::{Ann, Decoder, DecoderSpec, Input, InputFrom, InputSpec, flags};

pub const SPEC: DecoderSpec = DecoderSpec {
    name: "I2C",
    inputs: &[
        InputSpec { role: "scl", from: InputFrom::Line, optional: false },
        InputSpec { role: "sda", from: InputFrom::Line, optional: false },
    ],
    rows: &["conditions", "bytes", "transactions"],
};

pub const SCL: u8 = 0;
pub const SDA: u8 = 1;

/// Rows.
pub const CONDITIONS: u8 = 0;
pub const BYTES: u8 = 1;
pub const TRANSACTIONS: u8 = 2;

/// The conditions row's kinds (marks at their times).
pub const START: u16 = 0;
pub const RESTART: u16 = 1;
pub const STOP: u16 = 2;
pub const ACK: u16 = 3;
pub const NACK: u16 = 4;
/// The bytes row's kinds: value is the byte as on the wire, bit 8 set when acknowledged. A byte cut
/// short by a start or stop carries [`flags::ERROR`] and the bits it had.
pub const ADDRESS: u16 = 5;
pub const DATA: u16 = 6;
/// The transactions row's kinds, from the start to the stop or repeated start: value is the 7-bit
/// address, plus the data bytes' count shifted left by 8. An address nobody acknowledged carries
/// [`flags::WARN`].
pub const WRITE: u16 = 7;
pub const READ: u16 = 8;

/// The value's acknowledged bit, on a byte.
pub const ACKED: u64 = 1 << 8;

struct Byte {
    t0: f64,
    bits: u8,
    value: u8,
}

struct Transaction {
    t0: f64,
    /// The address byte once it is in (address and direction), and whether it was acknowledged.
    address: Option<(u8, bool)>,
    data: u64,
}

pub struct I2c {
    scl: Option<bool>,
    sda: Option<bool>,
    open: Option<Transaction>,
    byte: Option<Byte>,
    /// SDA read at the last SCL rise, with its time: a bit when SCL falls.
    read: Option<(f64, bool)>,
}

impl I2c {
    pub fn new() -> I2c {
        I2c { scl: None, sda: None, open: None, byte: None, read: None }
    }

    /// A byte cut short at `t`: its bits so far, flagged.
    fn cut(&mut self, t: f64, out: &mut dyn FnMut(Ann)) {
        if let Some(byte) = self.byte.take().filter(|byte| byte.bits > 0) {
            let kind = if self.open.as_ref().is_some_and(|open| open.address.is_some()) { DATA } else { ADDRESS };
            out(Ann { t0: byte.t0, t1: t, row: BYTES, kind, value: u64::from(byte.value), flags: flags::ERROR });
        }
    }

    /// The open transaction ends at `t`.
    fn close(&mut self, t: f64, out: &mut dyn FnMut(Ann)) {
        let Some(open) = self.open.take() else { return };
        let Some((address, acked)) = open.address else { return };
        let kind = if address & 1 == 1 { READ } else { WRITE };
        let flags = if acked { 0 } else { flags::WARN };
        out(Ann { t0: open.t0, t1: t, row: TRANSACTIONS, kind, value: u64::from(address >> 1) | open.data << 8, flags });
    }

    /// SCL rose at `t` with SDA at `high` and fell again: a bit of the open transaction.
    fn bit(&mut self, t: f64, high: bool, out: &mut dyn FnMut(Ann)) {
        let Some(open) = self.open.as_mut() else { return };
        let byte = self.byte.get_or_insert(Byte { t0: t, bits: 0, value: 0 });
        if byte.bits < 8 {
            byte.value = byte.value << 1 | u8::from(high);
            byte.bits += 1;
            return;
        }
        // the ninth bit: the receiver's acknowledge
        let acked = !high;
        let (value, t0) = (byte.value, byte.t0);
        self.byte = None;
        let kind = match open.address {
            None => {
                open.address = Some((value, acked));
                ADDRESS
            }
            Some(_) => {
                open.data += 1;
                DATA
            }
        };
        out(Ann { t0, t1: t, row: BYTES, kind, value: u64::from(value) | if acked { ACKED } else { 0 }, flags: 0 });
        out(Ann { t0: t, t1: t, row: CONDITIONS, kind: if acked { ACK } else { NACK }, value: 0, flags: 0 });
    }
}

impl Default for I2c {
    fn default() -> I2c {
        I2c::new()
    }
}

impl Decoder for I2c {
    fn spec(&self) -> &'static DecoderSpec {
        &SPEC
    }

    /// O(1) per input.
    fn push(&mut self, input: Input, out: &mut dyn FnMut(Ann)) {
        match input {
            Input::Edge { line: SCL, t, high } => {
                let was = self.scl.replace(high);
                if was == Some(false) && high {
                    self.read = self.sda.map(|sda| (t, sda));
                } else if was == Some(true) && !high {
                    if let Some((t, sda)) = self.read.take() {
                        self.bit(t, sda, out);
                    }
                }
            }
            Input::Edge { line: SDA, t, high } => {
                let changed = self.sda.is_some_and(|was| was != high);
                self.sda = Some(high);
                if changed && self.scl == Some(true) {
                    self.read = None;
                    self.cut(t, out);
                    if high {
                        self.close(t, out);
                        out(Ann { t0: t, t1: t, row: CONDITIONS, kind: STOP, value: 0, flags: 0 });
                    } else {
                        let kind = if self.open.is_some() { RESTART } else { START };
                        self.close(t, out);
                        out(Ann { t0: t, t1: t, row: CONDITIONS, kind, value: 0, flags: 0 });
                        self.open = Some(Transaction { t0: t, address: None, data: 0 });
                    }
                }
            }
            Input::Gap { from, to } => {
                out(Ann::gap(from, to));
                self.reset();
            }
            Input::Edge { .. } | Input::Ann { .. } | Input::End { .. } => {}
        }
    }

    fn reset(&mut self) {
        *self = I2c::new();
    }

    fn label(&self, ann: &Ann) -> String {
        let byte = ann.value as u8;
        match ann.kind {
            START => "S".into(),
            RESTART => "Sr".into(),
            STOP => "P".into(),
            ACK => "A".into(),
            NACK => "N".into(),
            ADDRESS if ann.is_error() => format!("{byte:02X}?"),
            ADDRESS => format!("{:02X} {}", byte >> 1, if byte & 1 == 1 { "R" } else { "W" }),
            DATA if ann.is_error() => format!("{byte:02X}?"),
            DATA => format!("{byte:02X}"),
            WRITE | READ => format!("{} {:02X} ({})", if ann.kind == READ { "R" } else { "W" }, ann.value & 0x7F, ann.value >> 8),
            _ => "gap".into(),
        }
    }
}

//! Serial receiver capabilities and resolved baud settings, in CPU cycles.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerialTable {
    /// Hardware links (UARTs).
    pub links: u32,
    pub ubrr_max: u16,
    /// Bits per received byte on the wire (8N1: 10).
    pub frame_bits: u32,
    /// The datasheet's recommended maximum receiver error, per mille,
    /// for normal and double speed.
    pub tolerance_permille: [u32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerialSetting { pub link: u32, pub asked: u32, pub ubrr: u16, pub u2x: bool }

/// A resolved receive declaration, in declaration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerialOut { pub decl: u32, pub link: u32, pub entry: u16, pub setting: SerialSetting, pub fifo: u32 }

pub const NONE: SerialTable = SerialTable { links: 0, ubrr_max: 0, frame_bits: 10, tolerance_permille: [0, 0] };

/// USART0 baud rate generator and asynchronous receiver error tolerance,
/// 8 data bits, no parity: normal speed +-2.0 %, double speed +-1.5 %.
pub const ATMEGA328P: SerialTable = SerialTable { links: 1, ubrr_max: 4095, frame_bits: 10, tolerance_permille: [20, 15] };

impl SerialSetting {
    pub fn divisor(&self) -> u64 {
        (if self.u2x { 8 } else { 16 }) * (self.ubrr as u64 + 1)
    }

    pub fn real_milli_baud(&self, clock_hz: u32) -> u64 {
        let divisor = self.divisor();
        (clock_hz as u64 * 1000 + divisor / 2) / divisor
    }

    pub fn byte_cycles(&self, frame_bits: u32) -> u64 {
        frame_bits as u64 * self.divisor()
    }

    pub fn byte_ns(&self, clock_hz: u32, frame_bits: u32) -> u64 {
        if clock_hz == 0 { return 0; }
        // Widen before scaling so even a table with u32::MAX frame bits
        // cannot wrap. Times beyond the device clock saturate.
        let ns = (self.byte_cycles(frame_bits) as u128 * 1_000_000_000 + clock_hz as u128 / 2) / clock_hz as u128;
        ns.min(u64::MAX as u128) as u64
    }
}

/// Choose the nearest setting within its receiver tolerance. Ties favour
/// normal speed's 16x oversampling. A refusal gives the nearest milli-baud.
pub fn resolve(table: &SerialTable, clock_hz: u32, link: u32, asked: u32) -> Result<SerialSetting, u64> {
    if asked == 0 || clock_hz == 0 { return Err(0); }
    let requested = asked as u64 * 1000;
    let mut nearest = (u64::MAX, 0);
    let mut accepted: Option<(u64, SerialSetting)> = None;
    for (u2x, tolerance) in [false, true].into_iter().zip(table.tolerance_permille) {
        let scaled = (if u2x { 8 } else { 16 }) * asked as u64;
        let counts = (clock_hz as u64 + scaled / 2) / scaled;
        let ubrr = counts.saturating_sub(1).min(table.ubrr_max as u64) as u16;
        let setting = SerialSetting { link, asked, ubrr, u2x };
        let real = setting.real_milli_baud(clock_hz);
        let error = real.abs_diff(requested);
        if error < nearest.0 { nearest = (error, real); }
        if error as u128 * 1000 <= tolerance as u128 * requested as u128
            && accepted.is_none_or(|(best, _)| error < best) {
            accepted = Some((error, setting));
        }
    }
    accepted.map(|(_, setting)| setting).ok_or(nearest.1)
}

use fab_elec_mcu_rt::Profile;

/// CST820 on the board's shared I2C bus; TP_INT is unused by this poller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Touch { pub addr: u8 }

/// facts-parts T01: 0x15 is CST816-family evidence, Assumed for CST820 (O04).
/// R01/R04: shared SDA15/SCL14; R02: TP_RESET is EXIO2, left an input.
pub const VOICEBOX_TOUCH: Touch = Touch { addr: 0x15 };

impl Touch {
    pub fn profile(&self) -> Profile {
        Profile { touch: true, ..crate::chip::profile() }
    }
}

/// Touch timer and last delivered report. No declaration means no allocation.
#[derive(Clone, Debug)]
pub struct TouchSram {
    pub timer: u32,
    pub initialized: u32,
    /// Three words: x, y, down. A release preserves x/y.
    pub report: u32,
}

use crate::{image::refusal, Refusal};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct I2cBus { pub sda: u32, pub scl: u32, pub hz: u32 }

/// facts-parts R01/R02: SDA15/SCL14, including the panel's TCA9554.
/// All documented maxima agree on 400 kHz: QMI8658C 2588–2593,
/// TCA9554 E09, AXP2101 A01, CST816-family T01. T19 additionally needs
/// 4.5 us bus-free, 600 ns data setup and 100 ns hold.
pub const VOICEBOX_I2C: I2cBus = I2cBus { sda: 15, scl: 14, hz: 400_000 };

pub(crate) struct Timing {
    pub divider: u32,
    pub low: u32,
    pub high: u32,
    pub wait: u32,
    pub hold: u32,
    pub sample: u32,
    pub start: u32,
    pub stop: u32,
}

impl I2cBus {
    pub(crate) fn timing(self) -> Result<Timing, Refusal> {
        if !(10_000..=400_000).contains(&self.hz) {
            return Err(refusal("shared I2C clock must fit the touch controller's documented 10–400 kHz range (T01)"));
        }
        // facts-io I11/I12 (TRM 27.4.1/27.4.7): XTAL / (DIV_NUM+1);
        // SCL period = LOW+HIGH+WAIT+3. The two nine-bit phase counters
        // can share 1000 clocks, leaving headroom for rising allowance.
        // Round up, never exceed requested Hz. DIV=1 at 100/400 kHz.
        let divider = 40_000_000u32.div_ceil(self.hz * 1000);
        let period = 40_000_000u32.div_ceil(self.hz * divider);
        let cycles = |ns: u32| (40 * ns).div_ceil(1000 * divider);
        let low = (period / 2).max(cycles(1400));
        let wait = cycles(300);
        let high = period - low - wait;
        // I13: HOLD >= 4 + ceil(3*SCLK/APB), derived for an 8 MHz
        // APB floor; build_image checks the exact programmed inequality.
        // Across DIV=1..4 this is 500..900 ns,
        // satisfying E09/A01/T19. At 400 kHz: low/high/rise=1.4/.8/.3 us,
        // hold=500 ns and data setup=900 ns. Filters disabled.
        let hold = (cycles(500) - 1).max(4 + 15u32.div_ceil(divider));
        // E09 and QMI Table 35: standard-mode START setup/bus-free 4.7 us,
        // hold/STOP setup >=4 us. Fast mode >=600 ns; touch T19 bus-free
        // is 4.5 us. One setup/hold value covers each mode's stricter minimum.
        let start = cycles(if self.hz <= 100_000 { 4700 } else { 600 }) - 1;
        let stop = cycles(if self.hz <= 100_000 { 4700 } else { 4500 }) - 1;
        // I13: WAIT < SAMPLE < HIGH and SAMPLE < WAIT+START+RSTART.
        Ok(Timing { divider, low: low - 1, high: high - 1, wait: wait - 1, hold,
            sample: wait - 1 + ((high - wait) / 2).min(start), start, stop })
    }
}

impl Timing {
    /// Same four-times allowance as panel bring-up, now derived from every
    /// programmed phase. I05/I12: nine clocks/byte plus START/repeated START
    /// setup+hold and STOP setup+bus-free. Convert divided XTAL to SYSTIMER upward.
    pub fn deadline(&self, bytes: u32, starts: u32) -> u32 {
        let cycles = u64::from(bytes) * 9 * u64::from(self.low + self.high + self.wait + 3)
            + u64::from(starts) * 2 * u64::from(self.start + 1)
            + u64::from(self.start + self.stop + 2);
        (4 * cycles * u64::from(self.divider) * 16).div_ceil(40) as u32
    }
}

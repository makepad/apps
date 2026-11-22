//! The ATmega328P's I/O pin electrics, for a circuit simulator's pad model and for the input
//! levels at which the emulated chip sees an edge.
//!
//! Source: Microchip DS40002061B, "ATmega48A/PA/88A/PA/168A/PA/328/P Data Sheet Complete":
//! Table 30-1 "Common DC characteristics" and section 31.8 "ATmega328P Typical Characteristics".
//! Typical values are read off the 25 °C curves at VCC = 5 V; the figures are linear there, so
//! each driver and the pull-up are a resistance. The data sheet gives no I/O edge time and no
//! typical leakage: the edge is the board model's, the leakage below is the table's bound.

/// One supply voltage's pin electrics. Resistances in ohms; thresholds as fractions of VCC.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PinElectrics {
    /// The supply these values hold at, volts.
    pub vcc: f64,
    /// High-side driver on resistance. Figure 31-355: VOH 4.475 V at IOH 20 mA.
    pub r_on_h: f64,
    /// Low-side driver on resistance. Figure 31-353: VOL 0.47 V at IOL 20 mA.
    pub r_on_l: f64,
    /// Pull-up resistance. Figure 31-348: 138 uA at 0 V, falling linearly to 0 at VCC.
    /// Table 30-1 bounds it to 20..50 kOhm.
    pub r_pull: f64,
    /// The smallest leakage resistance Table 30-1 allows: IIL and IIH at most 1 uA at 5.5 V.
    pub r_leak_min: f64,
    /// Typical rising-input threshold. Figure 31-356: 2.62 V at VCC 5 V.
    pub vih: f64,
    /// Typical falling-input threshold. Figure 31-357: 2.09 V at VCC 5 V. `vih - vil` is the
    /// input hysteresis, 0.53 V, as Figure 31-358 gives it at 5 V.
    pub vil: f64,
    /// Table 30-1, VCC 2.4..5.5 V: a pin is guaranteed to read low below this fraction.
    pub vil_max: f64,
    /// Table 30-1, VCC 2.4..5.5 V: a pin is guaranteed to read high above this fraction.
    pub vih_min: f64,
}

/// The ATmega328P at VCC = 5 V, 25 °C.
pub const ATMEGA328P_5V: PinElectrics = PinElectrics {
    vcc: 5.0,
    r_on_h: 0.525 / 0.020,
    r_on_l: 0.47 / 0.020,
    r_pull: 5.0 / 138e-6,
    r_leak_min: 5.5 / 1e-6,
    vih: 2.62 / 5.0,
    vil: 2.09 / 5.0,
    vil_max: 0.3,
    vih_min: 0.6,
};

// The typical thresholds lie inside the guaranteed band, and the pull-up inside its bounds.
const _: () = {
    let p = ATMEGA328P_5V;
    assert!(p.vil_max < p.vil && p.vil < p.vih && p.vih < p.vih_min);
    assert!(20e3 <= p.r_pull && p.r_pull <= 50e3);
};

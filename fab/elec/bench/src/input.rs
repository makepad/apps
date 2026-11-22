//! Classified design parts for the model-binding table, in design net, part and pin order.

use crate::bench::{BenchSource, Firmware, ScopeProbe, Supply};
pub use fab_elec_core::drive::{Drive, Pull};
use fab_elec_core::ids::{CompRef, NetRef};
use fab_elec_mcu_parts::PartKind as BusKind;
use fab_elec_sim::GpioParams;

#[derive(Clone, Debug, PartialEq)]
pub struct LowerInput {
    pub nets: Vec<LowerNet>,
    pub ground: NetRef,
    pub parts: Vec<LowerPart>,
    /// Bench supplies on nets in this scope.
    pub supplies: Vec<Supply>,
    /// Bench sources and loads between nets in this scope.
    pub sources: Vec<BenchSource>,
    /// Firmware specs and their crystal clocks in Hz, in bench order, including unresolved paths.
    pub firmware: Vec<(Firmware, Option<f64>)>,
    /// The bench's scope probes; a voltage probe's tip load is lowered across its two points.
    pub probes: Vec<ScopeProbe>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LowerNet {
    pub id: NetRef,
    pub name: String,
    /// Declared a supply: a power net, or a net on a power-output port (a block's port or a
    /// part's pin). The simulator's round plan may take its consumers' currents a round late there.
    pub supply: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LowerPart {
    pub id: CompRef,
    pub path: String,
    pub kind: PartKind,
    /// Bounds (lo, hi) of the real R, C or L value in SI units; None without a tolerance or for other kinds.
    pub tol: Option<(f64, f64)>,
    pub pins: Vec<LowerPin>,
    /// A resistor whose thermal (Johnson-Nyquist) noise is simulated: its `noise` field says
    /// `thermal`, as a noise source's resistor does.
    pub noise: bool,
}

/// Pin index (`PinIx`) = position in the part's `pins`.
#[derive(Clone, Debug, PartialEq)]
pub struct LowerPin {
    pub name: String,
    pub net: Option<NetRef>,
    /// Declared not connected on purpose (the part's `nc`): left unconnected, it is no mistake.
    pub nc: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PartKind {
    Resistor { ohms: f64 },
    Capacitor { farads: f64 },
    Inductor { henries: f64 },
    /// Pins A and K.
    Led { color: LedColor },
    /// Pins A and K; `model` names a diode card in `ModelLibs`.
    Diode { model: String },
    /// Pins C, B and E; the discrete substrate is tied to E.
    Bjt { pnp: bool, model: String },
    /// Pins D, G and S, with an optional B (bulk); absent B is tied to S.
    Mosfet { pmos: bool, model: String },
    /// Pins VIN, GND, VOUT and EN; NC is ignored.
    Regulator { vout: f64, ilim: f64 },
    /// A part whose pins are driven by a bench stub; `mcu` adds the MCU's quiescent supply current.
    /// `pads` are its GPIO pads' electrics: its part's GPIO card, or the library's generic profile.
    /// `supply` names the preferred I/O supply pin, before VDD, VCC and VIO.
    Stub { drives: Vec<(String, Drive)>, rail: Option<NetRef>, mcu: bool, pads: GpioParams, supply: Option<String> },
    /// An MCU running firmware: a stub with no drives whose pins in `rails` (pin indices, ascending)
    /// get no GPIO: supply pins and the crystal's pins. The clock is ideal; no oscillator is solved.
    /// `crystal` (of `rails`, ascending) are the crystal's pins, which lower the oscillator
    /// amplifier's DC bias and its feedback resistor. `pads` as for a stub; `c_pin` is the card's
    /// capacitance per I/O pin in farads, or unknown, for the bus side only.
    /// `chip` is the component's `model` field, then the part's; `None` when neither names one.
    /// `supply` selects the same I/O supply as a stub.
    Firmware { chip: Option<String>, rails: Vec<usize>, crystal: Vec<usize>, pads: GpioParams, c_pin: Option<f64>, supply: Option<String> },
    /// A bus part beside the firmware part, never in the analog circuit (`crate::bus`). `model`
    /// is its part-data key; `kind` is the validated model. `c_pin` is its card's capacitance per
    /// I/O pin in farads, or unknown; it has no generic fallback. `rails` are its power pins
    /// (pin indices, ascending), by the part library's pin kind: on the ground net a ground,
    /// otherwise a supply.
    Bus { model: String, kind: BusKind, c_pin: Option<f64>, rails: Vec<usize> },
    /// Pins VBUS and GND; SHIELD, CC1 and CC2 carry no model current.
    UsbPower { volts: f64 },
    /// Pins A and B.
    Button { pressed: bool },
    /// An op-amp: per section its output, non-inverting and inverting input (pin indices); its
    /// supply pins (VDD or VCC, and VSS, VEE or GND) feed every section.
    OpAmp { sections: Vec<[usize; 3]> },
    /// Pins A and B (the track's ends) and W (the wiper): `ohms` end to end, the wiper at `pos`
    /// (0 at A, 1 at B), the A-W resistance following `taper`. A person turns it live.
    Pot { ohms: f64, pos: f64, taper: Taper },
    /// A Schmitt-trigger inverter: per section its input and output (pin indices); its supply
    /// pins (VDD or VCC, and VSS or GND) feed every section.
    Schmitt { sections: Vec<[usize; 2]> },
    /// A logic part simulated on the digital side beside the board's firmware (`crate::logic`):
    /// only its outputs into the circuit are lowered, as GPIO pads with `pads`' electrics.
    Logic { kind: crate::logic::LogicKind, pads: GpioParams },
    /// Connectors, headers and test points without an electrical model.
    Passive,
    Other(String),
}

/// How a pot's A-W resistance follows its wiper's place along the turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taper {
    /// In proportion.
    Linear,
    /// The audio (logarithmic) law: a tenth of the track at half the turn.
    Log,
}

/// The audio taper's base: `(LOG_BASE^p - 1) / (LOG_BASE - 1)` is 0.1 at p = 0.5.
const LOG_BASE: f64 = 81.0;

impl Taper {
    /// The taper by its name in a part's `taper` field (`lin`, `linear`, `log`, `audio`).
    pub fn of_name(name: &str) -> Option<Taper> {
        match name.to_lowercase().as_str() {
            "lin" | "linear" => Some(Taper::Linear),
            "log" | "audio" => Some(Taper::Log),
            _ => None,
        }
    }

    /// The fraction of the track between A and the wiper at `pos` (0..1, clamped).
    pub fn track(self, pos: f64) -> f64 {
        let p = pos.clamp(0.0, 1.0);
        match self {
            Taper::Linear => p,
            Taper::Log => (LOG_BASE.powf(p) - 1.0) / (LOG_BASE - 1.0),
        }
    }

    /// The A-W and W-B resistances of an `ohms` pot with its wiper at `pos`; `end` (a fraction of
    /// the track) stays at either end with the wiper turned fully to it. They add to `ohms`.
    pub fn halves(self, ohms: f64, pos: f64, end: f64) -> (f64, f64) {
        let span = 1.0 - 2.0 * end;
        let t = self.track(pos);
        (ohms * (end + span * t), ohms * (end + span * (1.0 - t)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    White,
    Infrared,
    YellowGreen,
}

impl LedColor {
    /// The colour by its lowercase name (`"green"`); any case is accepted. None for an unknown name.
    pub fn of_name(name: &str) -> Option<LedColor> {
        match name.to_lowercase().as_str() {
            "red" => Some(Self::Red),
            "orange" => Some(Self::Orange),
            "yellow" => Some(Self::Yellow),
            "green" => Some(Self::Green),
            "blue" => Some(Self::Blue),
            "white" => Some(Self::White),
            "infrared" => Some(Self::Infrared),
            "yellow-green" => Some(Self::YellowGreen),
            _ => None,
        }
    }
}

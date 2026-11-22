//! Fab electronics part definitions: contract C2, `fab-elec-parts` (E1 1.2, E2.18, E4).
//!
//! A `PartDef` is what a library gives: pins with their electrical kinds and limits, a symbol on
//! the connection grid, a footprint with pad stacks, an optional 3D body with ports. Parts are
//! plain data shared by `Arc`; circuits point at them, boards copy the footprint they place.
//! Generators (`std` packages, connectors) are Splash in `elec/lang`; nothing here ships a
//! third-party library.
//!
//! Additive after the freeze (E2.18 7, E4): `PartDef.mate`, `PartDef.through`, per-pin
//! `functions`.
//!
//! Cost: plain data, nothing per frame; lookups by name are linear in pins (parts are small).

pub mod body;
pub mod footprint;
pub mod model_board;
pub mod symbol;

use fab_elec_core::{canon_enum, canon_struct, canon_tags, Nm, PinIx};
use std::sync::Arc;

pub use body::{port_axes, Body3d, BodyColour, BodyPort, BodyPortKind, BodySource, BodySurface, Emitter, EmitterFace, Light, PackageKind, SHAFT_TURN_DEG};
pub use footprint::{
    Drill, FpAttrs, FpMount, FpText, FpTextKind, Footprint, Forbid, Gfx, Keepout, Pad, PadKind, PadProp, PadShape, PadStack,
    PasteRule, ThermalRule, ZoneConnect,
};
pub use symbol::{PinDir, PinShape, SymFill, SymGraphic, SymPin, Symbol, SymbolUnit};

/// A pad number as the footprint names it: `"1"`, `"A1"`, `"EP"`, `"MP"`.
pub type PadNo = String;

/// A part as a library defines it.
#[derive(Clone, PartialEq, Debug)]
pub struct PartDef {
    pub name: String,
    /// What the part is, for the explorer's block centres, the schematic and generators.
    pub kind: PartKind,
    pub symbol: Symbol,
    /// `None` for virtual and simulation-only parts (E4 A12).
    pub footprint: Option<Arc<Footprint>>,
    pub body: Option<Body3d>,
    pub pins: Vec<PinDef>,
    pub fields: Fields,
    /// A light source's emitting face (LEDs; E7 1.3): what light pipes and the tracer read.
    pub emitter: Option<Emitter>,
    /// A part's pad electrics; `None` takes the simulator's generic profile.
    pub gpio: Option<GpioCard>,
}
// Additive: `gpio` is written only when set, so a part without a card keeps its hash.
impl fab_elec_core::canon::Canon for PartDef {
    fn canon(&self, w: &mut fab_elec_core::canon::Writer) {
        let PartDef { name, kind, symbol, footprint, body, pins, fields, emitter, gpio } = self;
        name.canon(w);
        kind.canon(w);
        symbol.canon(w);
        footprint.canon(w);
        body.canon(w);
        pins.canon(w);
        fields.canon(w);
        emitter.canon(w);
        if gpio.is_some() {
            gpio.canon(w);
        }
    }
}

/// A part's I/O pad electrics: one card per part, for every I/O pin. SI units; thresholds are
/// fractions of the supply. The bus side reads `c_pin`; the circuit's pad model does not.
#[derive(Clone, PartialEq, Debug)]
pub struct GpioCard {
    /// The high-side driver's on resistance, ohms.
    pub r_high: f64,
    /// The low-side driver's on resistance, ohms.
    pub r_low: f64,
    /// The pull-up resistance, ohms (one typical value).
    pub pull_up: f64,
    /// The rising-input threshold, a fraction of the supply.
    pub vih: f64,
    /// The falling-input threshold, a fraction of the supply.
    pub vil: f64,
    /// The pad's leakage current at the supply voltage, amps.
    pub leak: f64,
    /// A mode change's edge time, seconds; `None` takes the simulator's default.
    pub edge: Option<f64>,
    /// Input capacitance of one I/O pin, farads (C_IN or C_I); `None` means unknown.
    pub c_pin: Option<f64>,
}
// Additive: an absent capacitance keeps the card's exact pre-capacitance canonical form.
impl fab_elec_core::canon::Canon for GpioCard {
    fn canon(&self, w: &mut fab_elec_core::canon::Writer) {
        let GpioCard { r_high, r_low, pull_up, vih, vil, leak, edge, c_pin } = self;
        r_high.canon(w);
        r_low.canon(w);
        pull_up.canon(w);
        vih.canon(w);
        vil.canon(w);
        leak.canon(w);
        edge.canon(w);
        if c_pin.is_some() {
            c_pin.canon(w);
        }
    }
}

/// What a part is (`kind: @regulator` in a definition). `Ic` is any other integrated circuit (a
/// processor, memory, a PHY, logic), which the explorer treats as a block centre like `Mcu`;
/// `Analog` is one whose pins carry voltages rather than logic levels (an op-amp, an audio
/// amplifier), placed and drawn like an `Ic`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PartKind {
    Resistor,
    Capacitor,
    Inductor,
    Diode,
    Led,
    Regulator,
    Mcu,
    Ic,
    Connector,
    Switch,
    Crystal,
    Transistor,
    Fuse,
    /// A variable resistor turned by hand: its value is its track's resistance.
    Potentiometer,
    /// An analog integrated circuit (an op-amp, an audio amplifier): its inputs and outputs carry
    /// voltages, so the instruments draw its nets as waveforms, not logic levels.
    Analog,
    /// A schematic marker for a net supplied from outside the circuit.
    PowerFlag,
    /// A kind with no variant yet, by its name.
    Other(String),
}
canon_enum!(PartKind {
    0 => Resistor, 1 => Capacitor, 2 => Inductor, 3 => Diode, 4 => Led, 5 => Regulator, 6 => Mcu, 7 => Ic,
    8 => Connector, 9 => Switch, 10 => Crystal, 11 => Transistor, 12 => Fuse, 13 => Other(name), 14 => Potentiometer, 15 => Analog, 16 => PowerFlag,
});

impl PartKind {
    const NAMED: [(&'static str, PartKind); 16] = [
        ("resistor", PartKind::Resistor),
        ("capacitor", PartKind::Capacitor),
        ("inductor", PartKind::Inductor),
        ("diode", PartKind::Diode),
        ("led", PartKind::Led),
        ("regulator", PartKind::Regulator),
        ("mcu", PartKind::Mcu),
        ("ic", PartKind::Ic),
        ("connector", PartKind::Connector),
        ("switch", PartKind::Switch),
        ("power_flag", PartKind::PowerFlag),
        ("crystal", PartKind::Crystal),
        ("transistor", PartKind::Transistor),
        ("fuse", PartKind::Fuse),
        ("potentiometer", PartKind::Potentiometer),
        ("analog", PartKind::Analog),
    ];
    /// The kind a snake_case name gives (`"regulator"`); an unknown name is `Other(name)`.
    pub fn of_name(name: &str) -> PartKind {
        Self::NAMED.iter().find(|(n, _)| *n == name).map_or_else(|| PartKind::Other(name.into()), |(_, k)| k.clone())
    }
    /// The snake_case name (`of_name`'s inverse).
    pub fn name(&self) -> &str {
        match self {
            PartKind::Other(name) => name,
            k => Self::NAMED.iter().find(|(_, n)| n == k).map_or("", |(name, _)| name),
        }
    }

    /// The common class tolerance, for a value that declares none (a declared one wins):
    /// resistors ±1 %, capacitors ±10 %, inductors ±20 %, pots ±20 %, crystals ±20 ppm; no
    /// other kind has one. O(1).
    pub fn default_tol(&self) -> Option<Tol> {
        match self {
            PartKind::Resistor => Some(Tol::pct(1.0)),
            PartKind::Potentiometer => Some(Tol::pct(20.0)),
            PartKind::Capacitor => Some(Tol::pct(10.0)),
            PartKind::Inductor => Some(Tol::pct(20.0)),
            PartKind::Crystal => Some(Tol::pct(0.002)),
            _ => None,
        }
    }
}

/// A value's allowed deviation below and above its nominal value.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Tol {
    /// Percent of the nominal: 1% is `Pct { minus: 1.0, plus: 1.0 }`.
    Pct { minus: f64, plus: f64 },
    /// Absolute deviation in the value's SI unit.
    Abs { minus: f64, plus: f64 },
}
canon_enum!(Tol { 0 => Pct { minus, plus }, 1 => Abs { minus, plus } });

impl Tol {
    /// A symmetric percentage tolerance.
    pub fn pct(p: f64) -> Tol {
        Tol::Pct { minus: p, plus: p }
    }

    /// A symmetric absolute tolerance in the value's SI unit.
    pub fn abs(a: f64) -> Tol {
        Tol::Abs { minus: a, plus: a }
    }

    /// The lowest and highest real value, in O(1).
    pub fn bounds(&self, nominal: f64) -> (f64, f64) {
        match *self {
            Tol::Pct { minus, plus } => {
                let below = nominal * (1.0 - minus / 100.0);
                let above = nominal * (1.0 + plus / 100.0);
                if nominal < 0.0 { (above, below) } else { (below, above) }
            }
            Tol::Abs { minus, plus } => (nominal - minus, nominal + plus),
        }
    }
}

/// One electrical pin; it may land on several pads (`GND` on `2` and `EP`).
#[derive(Clone, PartialEq, Debug)]
pub struct PinDef {
    pub name: String,
    pub pads: Vec<PadNo>,
    pub kind: PinKind,
    /// The symbol unit the pin is drawn in (0 for single-unit parts).
    pub unit: u8,
    pub limits: Option<Limits>,
    /// Other names and kinds the pin can take (an MCU pin's alternate functions).
    pub alternates: Vec<(String, PinKind)>,
    /// Package length or delay from the die to the pad (E6 13.1 6), counted in length matching.
    pub delay: Option<PinDelay>,
}
canon_struct!(PinDef { name, pads, kind, unit, limits, alternates, delay });

/// A pin's package contribution to a matched length.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum PinDelay {
    Len(Nm),
    /// Seconds.
    Time(f64),
}
canon_enum!(PinDelay { 0 => Len(l), 1 => Time(t) });

/// The electrical kind of a pin, for ERC.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum PinKind {
    In,
    Out,
    Bidi,
    TriState,
    Passive,
    PowerIn,
    PowerOut,
    OpenCollector,
    OpenEmitter,
    NoConnect,
    Free,
    Unspecified,
}
canon_tags!(PinKind);

impl PinKind {
    /// A pin that can drive a net (ERC's "driver" and the trace queries' drivers).
    pub fn drives(self) -> bool {
        matches!(self, PinKind::Out | PinKind::Bidi | PinKind::TriState | PinKind::PowerOut | PinKind::OpenCollector | PinKind::OpenEmitter)
    }
    /// A pin that is driven (a load).
    pub fn loads(self) -> bool {
        matches!(self, PinKind::In | PinKind::Bidi | PinKind::PowerIn)
    }
}

/// Electrical limits of a pin, in SI units.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Limits {
    pub volts: Option<VRange>,
    pub current: Option<f64>,
    pub power: Option<f64>,
}
canon_struct!(Limits { volts, current, power });

/// A voltage range in volts (`min == max` for a fixed voltage).
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct VRange {
    pub min: f64,
    pub max: f64,
}
canon_struct!(VRange { min, max });

impl VRange {
    pub fn fixed(v: f64) -> VRange {
        VRange { min: v, max: v }
    }
    pub fn contains(&self, o: &VRange) -> bool {
        self.min <= o.min && o.max <= self.max
    }
}

/// Free-form fields in order (`mpn`, `datasheet`, `description`, ...).
/// Simulation reads `sim` to select the model family and `model` for the chip or
/// model-card key. `supply` names the pin supplying a stub's I/O (for example
/// `supply: "VDD3P3_RTC"`); it is tried before VDD, VCC and VIO. Component fields
/// override the part library's fields. The selected pin also supplies firmware
/// GPIOs and their oscillator bias; its voltage rating sets the GPIO card's leakage.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Fields(pub Vec<(String, String)>);
canon_struct!(Fields { 0 });

impl Fields {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

impl PartDef {
    /// The pin named `name`.
    pub fn pin(&self, name: &str) -> Option<PinIx> {
        self.pins.iter().position(|p| p.name == name).map(|i| PinIx(i as u16))
    }
    pub fn pin_def(&self, pin: PinIx) -> Option<&PinDef> {
        self.pins.get(pin.ix())
    }
    /// The footprint pads a pin lands on, as pad indices.
    pub fn pads_of(&self, pin: PinIx) -> Vec<fab_elec_core::PadIx> {
        let (Some(fp), Some(def)) = (&self.footprint, self.pins.get(pin.ix())) else { return Vec::new() };
        def.pads.iter().filter_map(|no| fp.pad(no)).collect()
    }
    /// The pin each footprint pad belongs to (`None` for mechanical pads), indexed by pad.
    pub fn pad_pins(&self) -> Vec<Option<PinIx>> {
        let Some(fp) = &self.footprint else { return Vec::new() };
        fp.pads
            .iter()
            .map(|pad| self.pins.iter().position(|p| p.pads.iter().any(|n| *n == pad.no)).map(|i| PinIx(i as u16)))
            .collect()
    }
    /// The model's invariants (E4): unique pin names, every pin pad names a footprint pad.
    pub fn check(&self) -> Result<(), String> {
        for (i, p) in self.pins.iter().enumerate() {
            if self.pins[..i].iter().any(|q| q.name == p.name) {
                return Err(format!("{}: pin name {} twice", self.name, p.name));
            }
            if let Some(fp) = &self.footprint {
                for no in &p.pads {
                    if fp.pad(no).is_none() {
                        return Err(format!("{}: pin {} names pad {no}, which the footprint lacks", self.name, p.name));
                    }
                }
            }
        }
        Ok(())
    }
}

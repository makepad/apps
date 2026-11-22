//! Generic electrical model data used to classify design parts and lower kernel devices.

use crate::input::LedColor;
use fab_elec_circuit::GpioCard;
use fab_elec_sim::{GpioParams, ModelKind};

/// Thermal voltage in volts at the model's nominal temperature, 300.15 K.
const VT: f64 = 8.617333262e-5 * 300.15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bind {
    Resistor,
    Capacitor,
    Inductor,
    Led,
    Diode,
    Npn,
    Pnp,
    Nmos,
    Pmos,
    Regulator,
    Mcu,
    UsbPower,
    Button,
    /// An operational amplifier, one or more sections: `OpAmpCard`'s macro-model each.
    OpAmp,
    /// A potentiometer: two resistors meeting at the wiper, which a person turns live.
    Pot,
    /// A Schmitt-trigger inverter, one or more sections: `SchmittCard`'s switches each.
    Schmitt,
    /// An 8-bit shift register with output latch, on the digital side (`crate::logic`).
    ShiftLatch8,
    /// A part-data model on the bus side (`crate::bus`), resolved by the parts factory.
    Bus,
    Passive,
}

/// An op-amp section's macro-model, as a rail-to-rail CMOS part's data sheet gives it: its
/// open-loop gain and gain-bandwidth (one dominant pole), its output resistance and current limit,
/// and its supply current. Its inputs draw nothing; its output swings rail to rail, less what
/// its load pulls through `rout`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpAmpCard {
    /// Open-loop DC gain, V/V.
    pub a0: f64,
    /// Gain-bandwidth product, Hz: the dominant pole sits at `gbw / a0`.
    pub gbw: f64,
    /// Open-loop output resistance, ohms.
    pub rout: f64,
    /// Output current limit, amperes, either way.
    pub ilim: f64,
    /// Supply current per section, amperes.
    pub iq: f64,
}

/// A CMOS Schmitt-trigger inverter section on a `vdd` supply: its output switches low once its
/// input rises past `vt_plus` and high once it falls past `vt_minus`, through `ron` either way.
/// Its input draws nothing but its capacitance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SchmittCard {
    /// The rising input threshold, volts above the low supply pin.
    pub vt_plus: f64,
    /// The falling input threshold, volts above the low supply pin.
    pub vt_minus: f64,
    /// The output's on resistance to either rail, ohms.
    pub ron: f64,
    /// The output's off resistance, ohms.
    pub roff: f64,
    /// Input capacitance, farads.
    pub cin: f64,
    /// Output capacitance (the pin), farads.
    pub cout: f64,
    /// The supply the thresholds are for, volts.
    pub vdd: f64,
    /// Supply current per section, amperes.
    pub iq: f64,
}

/// The current an LED of the table is rated at (its first operating point, `leds`' 20 mA): its
/// light output is full there.
pub const LED_RATED_A: f64 = 0.02;

/// One colour's LED in the model library: the two forward operating points its DC fit reads, its
/// series resistance, and its junction's charge, which make it a capacitor at rest and a slow
/// diode when it switches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LedCard {
    pub color: LedColor,
    /// Forward volts at 20 mA and at 2 mA.
    pub v20: f64,
    pub v2: f64,
    /// Series resistance, ohms.
    pub rs: f64,
    /// Zero-bias junction capacitance (F), its grading coefficient, and the junction potential (V)
    /// it falls towards in reverse.
    pub cjo: f64,
    pub m: f64,
    pub vj: f64,
    /// Transit time (s): the charge the junction stores per ampere of forward current, set by the
    /// die's carrier lifetime.
    pub tt: f64,
    /// Reverse breakdown (V) and the current at it (A).
    pub bv: f64,
    pub ibv: f64,
}

/// A card from its die family's dynamics: `junction` is [cjo, m, vj], `reverse` [bv, ibv].
const fn led(color: LedColor, v20: f64, v2: f64, rs: f64, junction: [f64; 3], tt: f64, reverse: [f64; 2]) -> LedCard {
    LedCard { color, v20, v2, rs, cjo: junction[0], m: junction[1], vj: junction[2], tt, bv: reverse[0], ibv: reverse[1] }
}

// Die families, as datasheets of small indicator LEDs (a die about 0.25 mm square) give them: the
// capacitance at zero bias and 1 MHz, the switching time (the carrier lifetime the transit time
// stands for), and the reverse rating (10 uA at the reverse voltage they test, the breakdown of
// such dies lying several times above it). Class values, not one product's.
/// AlInGaP (red to yellow-green): about 30 pF, lifetime about 20 ns, breakdown about 20 V.
const ALINGAP: ([f64; 3], f64, [f64; 2]) = ([30e-12, 0.4, 1.9], 20e-9, [20.0, 10e-6]);
/// InGaN on sapphire (true green, blue, and white's blue die): about 50 pF, lifetime about 5 ns,
/// breakdown about 15 V.
const INGAN: ([f64; 3], f64, [f64; 2]) = ([50e-12, 0.35, 2.7], 5e-9, [15.0, 10e-6]);
/// GaAs infrared (940 nm): about 40 pF of an abrupt junction, slow (lifetime about 0.5 us),
/// breakdown about 30 V.
const GAAS_IR: ([f64; 3], f64, [f64; 2]) = ([40e-12, 0.5, 1.25], 500e-9, [30.0, 10e-6]);

#[derive(Clone, Debug, PartialEq)]
pub struct ModelLibs {
    /// Binding rules by part-name prefix, first match wins.
    pub rules: Vec<(String, Bind)>,
    /// One card per colour. Every colour used by the bench must have an entry.
    pub leds: Vec<LedCard>,
    /// Named discrete-device cards: kind and parameters in SI units.
    pub cards: Vec<(String, ModelKind, Vec<(String, f64)>)>,
    /// MCU GPIO high- and low-side driver on resistance, in ohms.
    pub pin_ron: f64,
    /// MCU GPIO mode-change and PWM edge ramp time, in seconds.
    pub pin_edge: f64,
    /// MCU GPIO pad capacitance to VSS, lowered on every pin, in farads.
    pub pin_cin: f64,
    /// MCU GPIO input pull-up or pull-down resistance, in ohms.
    pub pin_pull: f64,
    /// MCU GPIO leakage to VSS in every mode, in ohms: a pin is never an open circuit
    /// (hi-Z with its input leakage, a few nanoamperes at 3.3 V).
    pub pin_leak: f64,
    /// MCU GPIO input thresholds, rising and falling, as fractions of the pad's supply: where a
    /// typical pad switches (a part's GPIO card names its own).
    pub pin_vih: f64,
    pub pin_vil: f64,
    /// A net's insulation resistance to ground when nothing else gives it a DC path (board
    /// surface and laminate leakage), in ohms.
    pub board_leak: f64,
    /// MCU quiescent current from VDD to VSS, in amperes.
    pub mcu_iq: f64,
    /// An MCU crystal oscillator's internal feedback resistor between its two crystal pins, in
    /// ohms: about 1 Mohm on a CMOS Pierce oscillator such as the AVR's.
    pub osc_rf: f64,
    /// USB cable series resistance, in ohms.
    pub usb_cable_r: f64,
    /// USB supply voltage when the part declares no fixed voltage, in volts.
    pub usb_volts: f64,
    /// Closed button resistance, in ohms.
    pub button_ron: f64,
    /// Released button resistance, in ohms.
    pub button_roff: f64,
    /// Regulator dropout voltage, in volts.
    pub reg_vdo: f64,
    /// Regulator output resistance, in ohms.
    pub reg_rout: f64,
    /// Regulator quiescent input current, in amperes.
    pub reg_iq: f64,
    /// Regulator output current limit when the part declares none, in amperes.
    pub reg_ilim: f64,
    /// Every op-amp section's macro-model.
    pub opamp: OpAmpCard,
    /// Every Schmitt-trigger inverter section's model.
    pub schmitt: SchmittCard,
    /// A pot's end resistance, as a fraction of its track: what remains between the wiper and an
    /// end with the wiper turned fully to it (and keeps both halves finite).
    pub pot_end: f64,
}

impl ModelLibs {
    pub fn generic() -> ModelLibs {
        ModelLibs {
            rules: [
                ("R_", Bind::Resistor),
                ("C_", Bind::Capacitor),
                ("L_", Bind::Inductor),
                ("LED_", Bind::Led),
                ("Q_NMOS", Bind::Nmos),
                ("Q_PMOS", Bind::Pmos),
                ("Q_NPN", Bind::Npn),
                ("Q_PNP", Bind::Pnp),
                ("D_", Bind::Diode),
                ("LDO_", Bind::Regulator),
                ("MCU_", Bind::Mcu),
                ("USB_C_POWER", Bind::UsbPower),
                ("SW_", Bind::Button),
                ("OPAMP_", Bind::OpAmp),
                ("POT_", Bind::Pot),
                ("SCHMITT_", Bind::Schmitt),
                ("SHIFT_LATCH8_", Bind::ShiftLatch8),
                ("PinHeader_", Bind::Passive),
                ("Conn_", Bind::Passive),
                ("TestPoint", Bind::Passive),
            ].into_iter().map(|(prefix, bind)| (prefix.to_owned(), bind)).collect(),
            leds: vec![
                // AlInGaP red, orange and yellow dies: ideality about 1.8 (the old 2 mA points, 0.2 V
                // below 20 mA, fitted 0.94, below 1, which no diode has).
                led(LedColor::Red, 2.00, 1.75, 8.0, ALINGAP.0, ALINGAP.1, ALINGAP.2),
                led(LedColor::Orange, 2.05, 1.80, 8.0, ALINGAP.0, ALINGAP.1, ALINGAP.2),
                led(LedColor::Yellow, 2.10, 1.85, 8.0, ALINGAP.0, ALINGAP.1, ALINGAP.2),
                // A true green (525 nm) die is InGaN like blue (ideality about 2, not the old fit's unphysical 0.94).
                led(LedColor::Green, 3.00, 2.70, 10.0, INGAN.0, INGAN.1, INGAN.2),
                led(LedColor::Blue, 3.00, 2.70, 10.0, INGAN.0, INGAN.1, INGAN.2),
                led(LedColor::White, 3.00, 2.70, 10.0, INGAN.0, INGAN.1, INGAN.2),
                // A typical 940 nm GaAs IR die: 1.20 V at 20 mA, 1.06 V at 2 mA, 2 ohms series (fitted ideality 1.75).
                led(LedColor::Infrared, 1.20, 1.06, 2.0, GAAS_IR.0, GAAS_IR.1, GAAS_IR.2),
                // A typical 570 nm AlInGaP yellow-green die: 2.10 V at 20 mA, 1.88 V at 2 mA, 6 ohms series (fitted ideality 1.88).
                led(LedColor::YellowGreen, 2.10, 1.88, 6.0, ALINGAP.0, ALINGAP.1, ALINGAP.2),
            ],
            cards: [
                ("D_GENERIC", ModelKind::Diode, vec![
                    ("is", 2.5e-9), ("n", 1.75), ("rs", 0.6), ("cjo", 2e-12),
                    ("m", 0.33), ("vj", 0.7), ("tt", 6e-9), ("bv", 100.0), ("ibv", 1e-7),
                ]),
                // A small-signal switching diode (a glass or SOD-323 signal diode class): 0.58 V
                // at 1 mA and 0.69 V at 10 mA, a few picofarads, recovery in nanoseconds.
                ("D_SIGNAL", ModelKind::Diode, vec![
                    ("is", 2.5e-9), ("n", 1.75), ("rs", 0.6), ("cjo", 4e-12),
                    ("m", 0.33), ("vj", 0.7), ("tt", 4e-9), ("bv", 100.0), ("ibv", 1e-7),
                ]),
                // A general-purpose small-signal NPN (the 200 mA, SOT-23 / TO-92 class): gain
                // about 300 at a milliampere, falling above tens of milliamperes; a few
                // picofarads at each junction and a transit time near 0.3 ns (300 MHz class).
                ("Q_NPN_SIGNAL", ModelKind::Npn, vec![
                    ("is", 7e-15), ("bf", 300.0), ("vaf", 75.0), ("br", 4.0), ("ikf", 0.07),
                    ("cje", 4.5e-12), ("cjc", 3.5e-12), ("tf", 3e-10), ("tr", 2e-7),
                    ("rb", 10.0), ("rc", 1.0),
                ]),
                ("Q_NPN_GENERIC", ModelKind::Npn, vec![
                    ("is", 1e-14), ("bf", 200.0), ("vaf", 100.0), ("br", 4.0), ("ikf", 0.1),
                    ("cje", 8e-12), ("cjc", 4e-12), ("tf", 4e-10), ("tr", 5e-8),
                    ("rb", 10.0), ("rc", 1.0),
                ]),
                ("Q_PNP_GENERIC", ModelKind::Pnp, vec![
                    ("is", 1e-14), ("bf", 150.0), ("vaf", 50.0), ("br", 4.0), ("ikf", 0.1),
                    ("cje", 8e-12), ("cjc", 4e-12), ("tf", 4e-10), ("tr", 5e-8),
                    ("rb", 10.0), ("rc", 1.0),
                ]),
                // W = L = 100 um: beta = kp, overlap capacitances about 40 pF and 10 pF.
                ("Q_NMOS_GENERIC", ModelKind::Nmos, vec![
                    ("vto", 1.8), ("kp", 0.4), ("lambda", 0.01), ("rd", 0.5), ("rs", 0.3),
                    ("cgso", 4e-7), ("cgdo", 1e-7), ("cbd", 2e-11), ("pb", 0.8), ("is", 1e-14),
                ]),
                ("Q_PMOS_GENERIC", ModelKind::Pmos, vec![
                    ("vto", -1.8), ("kp", 0.25), ("lambda", 0.01), ("rd", 0.5), ("rs", 0.3),
                    ("cgso", 4e-7), ("cgdo", 1e-7), ("cbd", 2e-11), ("pb", 0.8), ("is", 1e-14),
                ]),
            ].into_iter().map(|(name, kind, params)| (
                name.to_owned(), kind,
                params.into_iter().map(|(key, value)| (key.to_owned(), value)).collect(),
            )).collect(),
            pin_ron: 25.0,
            pin_edge: 5e-9,
            pin_cin: 5e-12,
            pin_pull: 40e3,
            pin_leak: 1e9,
            pin_vih: 0.55,
            pin_vil: 0.45,
            board_leak: 1e12,
            mcu_iq: 2e-3,
            osc_rf: 1e6,
            usb_cable_r: 0.1,
            usb_volts: 5.0,
            button_ron: 0.1,
            button_roff: 1e9,
            reg_vdo: 0.25,
            reg_rout: 0.05,
            reg_iq: 50e-6,
            reg_ilim: 0.6,
            // A 5 V rail-to-rail CMOS audio op-amp class: 100 dB, 1 MHz, 100 uA a section.
            opamp: OpAmpCard { a0: 1e5, gbw: 1e6, rout: 50.0, ilim: 0.02, iq: 1e-4 },
            // A high-speed CMOS hex Schmitt inverter class at 5 V: thresholds near 58 % and 34 %
            // of the supply, about 50 ohms out, a few picofarads in.
            schmitt: SchmittCard { vt_plus: 2.9, vt_minus: 1.7, ron: 50.0, roff: 1e9, cin: 3.5e-12, cout: 10e-12, vdd: 5.0, iq: 2e-6 },
            // Carbon-track pots leave about half a percent at each end.
            pot_end: 0.005,
        }
    }

    /// Electrical parameters for each stub GPIO; its pad capacitance is lowered separately.
    pub fn gpio_params(&self) -> GpioParams {
        GpioParams {
            r_on_h: self.pin_ron,
            r_on_l: self.pin_ron,
            r_pull: self.pin_pull,
            r_leak: self.pin_leak,
            t_edge: self.pin_edge,
            vih: self.pin_vih,
            vil: self.pin_vil,
        }
    }

    /// A part's GPIO card as electrical parameters: its drivers, pull, thresholds and edge (the
    /// generic edge when it names none), and its leakage as a resistance at `rated` volts, the
    /// supply its leakage is rated at (the generic leakage without one).
    pub fn card_params(&self, card: &GpioCard, rated: Option<f64>) -> GpioParams {
        GpioParams {
            r_on_h: card.r_high,
            r_on_l: card.r_low,
            r_pull: card.pull_up,
            r_leak: rated.filter(|v| v.is_finite() && *v > 0.0).map_or(self.pin_leak, |v| v / card.leak),
            t_edge: card.edge.unwrap_or(self.pin_edge),
            vih: card.vih,
            vil: card.vil,
        }
    }

    /// Resolve a discrete model override, diagnosing an unknown or incompatible card before
    /// falling back to the generic card. Missing generic cards leave the part passive.
    pub(crate) fn discrete_model(
        &self, name: Option<&str>, kind: ModelKind, diag: &mut impl FnMut(String),
    ) -> Option<&str> {
        let generic = match kind {
            ModelKind::Diode => "D_GENERIC",
            ModelKind::Npn => "Q_NPN_GENERIC",
            ModelKind::Pnp => "Q_PNP_GENERIC",
            ModelKind::Nmos => "Q_NMOS_GENERIC",
            ModelKind::Pmos => "Q_PMOS_GENERIC",
            _ => return None,
        };
        if let Some(name) = name {
            match self.cards.iter().find(|(card, _, _)| card.eq_ignore_ascii_case(name)) {
                Some((card, card_kind, _)) if *card_kind == kind => return Some(card),
                Some((_, card_kind, _)) => diag(format!(
                    "model card '{name}' is {card_kind:?}, expected {kind:?}; using {generic}"
                )),
                None => diag(format!("unknown model card '{name}'; using {generic}")),
            }
        }
        let card = self.cards.iter().find(|(card, card_kind, _)| {
            card.eq_ignore_ascii_case(generic) && *card_kind == kind
        });
        if let Some((name, _, _)) = card {
            Some(name)
        } else {
            diag(format!("the model library has no {kind:?} card '{generic}'; not simulated"));
            None
        }
    }

    /// The colour's diode card: saturation current (A), ideality factor and series resistance
    /// (ohms) fitted to its two operating points after removing the series-resistance voltage
    /// drop, then its junction's capacitance, transit time and breakdown as the card states them.
    /// None when the library has no entry for the colour.
    pub fn led_params(&self, c: LedColor) -> Option<[(&'static str, f64); 9]> {
        let card = self.leds.iter().find(|card| card.color == c)?;
        let v20 = card.v20 - 0.02 * card.rs;
        let v2 = card.v2 - 0.002 * card.rs;
        let n = (v20 - v2) / (VT * 10.0_f64.ln());
        let is = 0.02 / (v20 / (n * VT)).exp_m1();
        Some([
            ("is", is), ("n", n), ("rs", card.rs), ("cjo", card.cjo), ("m", card.m), ("vj", card.vj),
            ("tt", card.tt), ("bv", card.bv), ("ibv", card.ibv),
        ])
    }

    /// Forward voltage in volts at current `i` in amperes, including series resistance.
    /// None when the library has no entry for the colour.
    pub fn led_vf(&self, c: LedColor, i: f64) -> Option<f64> {
        let [(_, is), (_, n), (_, rs), ..] = self.led_params(c)?;
        Some(n * VT * (i / is).ln_1p() + i * rs)
    }
}

impl Default for ModelLibs {
    fn default() -> Self {
        Self::generic()
    }
}

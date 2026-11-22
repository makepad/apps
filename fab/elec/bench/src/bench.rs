//! Bench stimulus added to a design without duplicating its circuit.

use crate::input::Drive;
use fab_elec_core::ids::NetRef;

/// Stimulus applied to a design without changing its parts or nets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BenchSpec {
    /// Stubs by component path: the part's model is replaced by pin drives (any part, not only MCUs).
    pub stubs: Vec<Stub>,
    /// Buttons held pressed, by component path (every other button is released).
    pub pressed: Vec<String>,
    /// Pots turned by the bench: component path and where the wiper starts (0 to 1), in place of
    /// the pot's own `pos`.
    pub turned: Vec<(String, f64)>,
    /// Bench supplies standing in for power connectors or feeding nets directly.
    pub supplies: Vec<Supply>,
    /// Sources and loads between any two nets.
    pub sources: Vec<BenchSource>,
    /// The scope channels, one per `b.probe` in call order: what each reads and its tip. The
    /// simulator lowers a tip's R and C as elements and its bandwidth as a filter on the channel.
    pub probes: Vec<ScopeProbe>,
    /// Parts running firmware, by component path; each part at most once, never also stubbed.
    pub firmware: Vec<Firmware>,
    /// The firmware parts' connections (`b.net`), each part's `c` at most once.
    pub nets: Vec<crate::net::NetEndpoint>,
}

/// A scope channel: the quantity it reads (as expectations name it), its label and its tip.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeProbe {
    pub probe: crate::expect::Probe,
    pub label: String,
    pub tip: fab_elec_design::Tip,
}

/// A part whose electrical model is replaced by driven or input pins.
#[derive(Clone, Debug, PartialEq)]
pub struct Stub {
    /// Component path of the part to replace.
    pub path: String,
    /// Drives by pin name; a pin with no entry is `Drive::Input(Pull::None)`.
    pub pins: Vec<(String, Drive)>,
    /// HIGH rail for a part with no connected supply pin.
    pub rail: Option<NetRef>,
}

/// A part that runs firmware in co-simulation (`b.firmware(comp, doc, params)`): its model is replaced
/// as a stub's is, and its port pins are GPIOs the firmware drives.
#[derive(Clone, Debug, PartialEq)]
pub struct Firmware {
    /// Component path of the part (`mcu.u`).
    pub path: String,
    /// The firmware document's file name in the project (`ledavr.fw.splash`).
    pub doc: String,
    /// The compile-time constants the bench sets, by name, in the order written.
    pub params: Vec<(String, f64)>,
}

/// An ideal voltage source on a net, optionally ramped and current-limited.
#[derive(Clone, Debug, PartialEq)]
pub struct Supply {
    /// Supplied net, relative to ground.
    pub net: NetRef,
    /// Source voltage in volts.
    pub v: f64,
    /// Current limit in amperes: holds current at the limit while voltage drops, not a fuse.
    pub limit: Option<f64>,
    /// Seconds to ramp from 0 V to `v`; `None` or zero is on at t = 0.
    pub ramp: Option<f64>,
}

/// A source or load between two nets, with positive current from `a` through it to `b`.
#[derive(Clone, Debug, PartialEq)]
pub struct BenchSource {
    /// Positive voltage terminal, or the net current is drawn from.
    pub a: NetRef,
    /// Negative voltage terminal, or the net current is delivered to.
    pub b: NetRef,
    /// Electrical stimulus applied between the terminals.
    pub kind: SourceKind,
}

/// Voltage, current or resistance applied by a bench source or load.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceKind {
    /// Holds V(a) - V(b) at this voltage in volts.
    Vdc(f64),
    /// Carries this current in amperes from `a` through the source to `b`.
    Idc(f64),
    /// Resistance in ohms between `a` and `b`.
    LoadR(f64),
    /// Draws this current in amperes from `a` through the load to `b`.
    LoadI(f64),
    /// Holds V(a) - V(b) at `low` until `at`, then at `high` for `width`, again every `every`
    /// (once without it); each edge takes `edge` seconds.
    Pulse {
        low: f64,
        high: f64,
        at: f64,
        width: f64,
        every: Option<f64>,
        edge: f64,
    },
    /// Holds `from` until `at`, then moves linearly to `to` over `rise`.
    /// Current flows from `a` through the load to `b` (a load to ground draws from the net).
    LoadStep {
        /// Initial current in amperes.
        from: f64,
        /// Final current in amperes.
        to: f64,
        /// Time in seconds when the change begins.
        at: f64,
        /// Duration of the linear change, in seconds.
        rise: f64,
    },
}

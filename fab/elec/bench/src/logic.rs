//! The digital side of a scope: logic parts simulated beside its firmware part, never in the
//! analog circuit, and the nets that only they and the firmware part's GPIO pins share.
//!
//! - A net is digital when it is not the ground, carries no bench supply or source, every pin on
//!   it is a GPIO pin of the firmware part, a pin of a logic part or of a part with no electrical
//!   model (a connector, a test point), exactly one of them drives it
//!   (a firmware GPIO pin or a logic output) and a logic input reads it. A digital net is not
//!   lowered: no device touches its node (its insulation leak holds it), so the edges on it (a
//!   shift register's clock and data) cost the analog solver nothing.
//! - A logic input on its part's own supply net is tied high, on its ground net or the scope's
//!   ground tied low; on any other net the digital side cannot read it (it reads low, with a
//!   diag).
//! - A logic output on any other net is a pin in the circuit: a GPIO device `{part}#{pin}.io`
//!   on the part's supply, its co-simulation row named `{part}.{pin}`.
//!
//! The host joins the logic parts to the firmware part's machine (`FwBinding::logic`). Cost:
//! O(pins) per lowering.

use crate::input::{LowerInput, PartKind};
use fab_elec_core::ids::NetRef;
use std::collections::{HashMap, HashSet};

/// What a logic part is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicKind {
    /// An 8-bit shift register with output latch: a rising SRCLK shifts SER in, a rising RCLK
    /// copies the stages to QA..QH, OE low enables them, SRCLR low clears the stages, QHS carries
    /// stage H out.
    ShiftLatch8,
}

impl LogicKind {
    /// Its logic pins in order, each with whether it is an output (supply pins are not listed).
    pub fn pins(self) -> &'static [(&'static str, bool)] {
        match self {
            LogicKind::ShiftLatch8 => &[
                ("SER", false), ("SRCLK", false), ("RCLK", false), ("OE", false), ("SRCLR", false),
                ("QA", true), ("QB", true), ("QC", true), ("QD", true), ("QE", true), ("QF", true),
                ("QG", true), ("QH", true), ("QHS", true),
            ],
        }
    }
}

/// How a logic pin connects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogicConn {
    /// A digital net: an index into `LogicSide::nets`.
    Net(u32),
    /// A supply (true) or the ground (false).
    Tied(bool),
    /// An output that is a pin in the circuit: its co-simulation row's name.
    Out(String),
    /// Unconnected, or an input the digital side cannot read: it reads low.
    Open,
}

/// A logic part: its path, kind and every logic pin of the kind, in the kind's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicPart {
    pub path: String,
    pub kind: LogicKind,
    pub pins: Vec<(String, LogicConn)>,
}

/// What drives a digital net.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogicDriver {
    /// A GPIO pin of the firmware part, by its pin name.
    Machine(String),
    /// A logic part's output: the part's index in `LogicSide::parts` and the pin's name.
    Part { part: u32, pin: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicNet {
    pub name: String,
    pub driver: LogicDriver,
}

/// The logic parts beside one firmware part and their digital nets; empty without logic parts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogicSide {
    pub parts: Vec<LogicPart>,
    pub nets: Vec<LogicNet>,
}

impl LogicSide {
    /// The digital side as the co-simulation's logic plan (`LogicMachine`): each part's pins in
    /// its kind's order, a part output driving a net by its index in that order. O(pins).
    pub fn plan(&self) -> fab_elec_mcu_cosim::LogicPlan {
        use fab_elec_mcu_cosim::{LogicKind as Kind, LogicNet as Net, LogicPart as Part, LogicPin as Pin, LogicPlan, NetDriver};
        let pin = |conn: &LogicConn| match conn {
            LogicConn::Net(net) => Pin::Net(*net),
            LogicConn::Tied(high) => Pin::Tied(*high),
            LogicConn::Out(name) => Pin::Out(name.clone()),
            LogicConn::Open => Pin::Open,
        };
        let parts = self.parts.iter().map(|part| Part {
            name: part.path.clone(),
            kind: match part.kind { LogicKind::ShiftLatch8 => Kind::ShiftLatch8 },
            pins: part.pins.iter().map(|(_, conn)| pin(conn)).collect(),
        }).collect();
        let nets = self.nets.iter().map(|net| Net {
            name: net.name.clone(),
            driver: match &net.driver {
                LogicDriver::Machine(name) => NetDriver::Machine(name.clone()),
                LogicDriver::Part { part, pin } => {
                    let pins = self.parts[*part as usize].kind.pins();
                    let index = pins.iter().position(|(name, _)| name == pin).expect("a driver is an output of its kind");
                    NetDriver::Part { part: *part, pin: index as u8 }
                }
            },
        }).collect();
        LogicPlan { parts, nets, ..LogicPlan::default() }
    }
}

/// What the lowering needs from the digital side.
#[derive(Debug, Default)]
pub(crate) struct Digital {
    /// The firmware part's path the logic parts run beside, and their description.
    pub host: Option<(String, LogicSide)>,
    /// The firmware part's GPIO pins on digital nets (pin indices): they get no GPIO device.
    pub skip: Vec<usize>,
    /// Per logic part index, its outputs into the circuit: (pin index, row name).
    pub outs: HashMap<usize, Vec<(usize, String)>>,
    /// The digital nets: the floating-net notes leave them out.
    pub nets: HashSet<NetRef>,
    /// Notes for the lowering's diagnostics: (part index, message).
    pub notes: Vec<(usize, String)>,
}

/// The digital side of `input`: its logic parts beside the first firmware part the bench runs.
/// Without logic parts, nothing; without a firmware part, every logic part is noted and lowers
/// nothing (its outputs float). O(pins).
pub(crate) fn digital(input: &LowerInput) -> Digital {
    let mut d = Digital::default();
    let logic: Vec<usize> = input.parts.iter().enumerate()
        .filter(|(_, p)| matches!(p.kind, PartKind::Logic { .. })).map(|(i, _)| i).collect();
    if logic.is_empty() {
        return d;
    }
    let host = input.firmware.iter().find_map(|(fw, _)| {
        input.parts.iter().position(|p| p.path == fw.path && matches!(p.kind, PartKind::Firmware { .. }))
    });
    let Some(host) = host else {
        for &i in &logic {
            d.notes.push((i, "a logic part runs beside the board's firmware; this bench runs none, so its outputs float".into()));
        }
        return d;
    };
    let PartKind::Firmware { rails, .. } = &input.parts[host].kind else { unreachable!("host is a firmware part") };

    // Every connection of every net: (part index, pin index), and the nets bench elements touch.
    let mut on: HashMap<NetRef, Vec<(usize, usize)>> = HashMap::new();
    for (i, part) in input.parts.iter().enumerate() {
        for (j, pin) in part.pins.iter().enumerate() {
            if let Some(net) = pin.net {
                on.entry(net).or_default().push((i, j));
            }
        }
    }
    let bench: HashSet<NetRef> = input.supplies.iter().map(|s| s.net)
        .chain(input.sources.iter().flat_map(|s| [s.a, s.b])).collect();
    let kind_pin = |i: usize, j: usize| -> Option<(LogicKind, bool)> {
        let PartKind::Logic { kind, .. } = input.parts[i].kind else { return None };
        let name = &input.parts[i].pins[j].name;
        kind.pins().iter().find(|(pin, _)| pin.eq_ignore_ascii_case(name)).map(|&(_, out)| (kind, out))
    };
    let host_gpio = |i: usize, j: usize| i == host && !rails.contains(&j);

    // The digital nets, in design net order, each with its one driver.
    let mut nets: HashMap<NetRef, u32> = HashMap::new();
    let mut side = LogicSide::default();
    for net in &input.nets {
        if net.id == input.ground || bench.contains(&net.id) {
            continue;
        }
        let Some(conns) = on.get(&net.id) else { continue };
        // a connector or test point pin (no electrical model) loads nothing: the ISP header
        // shares the programming pins a shift register's clock and data often use
        let passive = |i: usize| matches!(input.parts[i].kind, PartKind::Passive);
        if !conns.iter().all(|&(i, j)| host_gpio(i, j) || kind_pin(i, j).is_some() || passive(i)) {
            continue;
        }
        let drivers: Vec<(usize, usize)> = conns.iter().copied()
            .filter(|&(i, j)| host_gpio(i, j) || kind_pin(i, j).is_some_and(|(_, out)| out)).collect();
        let reads = conns.iter().any(|&(i, j)| kind_pin(i, j).is_some_and(|(_, out)| !out));
        if drivers.len() != 1 || !reads {
            continue;
        }
        let (i, j) = drivers[0];
        let driver = if i == host {
            LogicDriver::Machine(input.parts[i].pins[j].name.clone())
        } else {
            LogicDriver::Part { part: logic.iter().position(|&l| l == i).expect("a logic part") as u32, pin: input.parts[i].pins[j].name.clone() }
        };
        nets.insert(net.id, side.nets.len() as u32);
        side.nets.push(LogicNet { name: net.name.clone(), driver });
        d.nets.insert(net.id);
    }
    d.skip = input.parts[host].pins.iter().enumerate()
        .filter(|(j, pin)| host_gpio(host, *j) && pin.net.is_some_and(|n| nets.contains_key(&n))).map(|(j, _)| j).collect();

    for &i in &logic {
        let part = &input.parts[i];
        let PartKind::Logic { kind, .. } = part.kind else { continue };
        let net_of = |names: &[&str]| names.iter().find_map(|name| {
            part.pins.iter().find(|p| p.name.eq_ignore_ascii_case(name)).and_then(|p| p.net)
        });
        let (vcc, gnd) = (net_of(&["VCC", "VDD"]), net_of(&["GND", "VSS"]));
        let mut pins = Vec::new();
        let mut outs = Vec::new();
        for &(name, out) in kind.pins() {
            let Some(j) = part.pins.iter().position(|p| p.name.eq_ignore_ascii_case(name)) else {
                d.notes.push((i, format!("no {name} pin; it reads low")));
                pins.push((name.to_owned(), LogicConn::Open));
                continue;
            };
            let conn = match part.pins[j].net {
                None => LogicConn::Open,
                Some(net) if nets.contains_key(&net) => LogicConn::Net(nets[&net]),
                Some(_) if out => {
                    let row = format!("{}.{name}", part.path);
                    outs.push((j, row.clone()));
                    LogicConn::Out(row)
                }
                Some(net) if Some(net) == vcc => LogicConn::Tied(true),
                Some(net) if Some(net) == gnd || net == input.ground => LogicConn::Tied(false),
                Some(_) => {
                    d.notes.push((i, format!("input {name} is on a net the digital side cannot read (only firmware pins, logic outputs and its own supply pins); it reads low")));
                    LogicConn::Open
                }
            };
            pins.push((name.to_owned(), conn));
        }
        d.outs.insert(i, outs);
        side.parts.push(LogicPart { path: part.path.clone(), kind, pins });
    }
    d.host = Some((input.parts[host].path.clone(), side));
    d
}

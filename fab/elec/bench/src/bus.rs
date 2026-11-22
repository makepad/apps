//! The bus side of a scope: part-data models beside its firmware part, never in the analog
//! circuit. A signal net may carry firmware GPIOs, model pins, connectors and pulls to the
//! participants' supplies or ground. Two model pins may also be coupled by an isolated series
//! path of resistors and capacitors. The bus joins the pins while the circuit keeps that path.
//!
//! Pull resistors are consumed only for nets of accepted parts. Supply decoupling stays in the
//! circuit. Pin capacitance is explicit part data: an unknown contribution stays unknown.

use crate::bench::SourceKind;
use crate::expect::Probe;
use crate::input::{LowerInput, LowerNet, LowerPart, LowerPin, PartKind};
use crate::lower::LowerDiag;
use fab_elec_core::ids::{CompRef, NetRef};
use fab_elec_mcu_emu::Pull;
use fab_elec_mcu_parts::{BoardDesc, NetDesc, PartDesc, PartKind as BusKind, PinRef};
use std::collections::{HashMap, HashSet};

/// How a model pin connects; supplies and grounds are the part's own rails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BusConn {
    /// An index into `BusSide::nets`.
    Net(u32),
    High,
    Low,
}

/// A resolved bus part, with model pins in the model's order.
#[derive(Clone, Debug, PartialEq)]
pub struct BusPart {
    pub path: String,
    pub comp: CompRef,
    pub model: String,
    pub kind: BusKind,
    pub pins: Vec<(String, BusConn)>,
}

/// Parallel pulls in one direction; every consumed resistor is named by its component path.
#[derive(Clone, Debug, PartialEq)]
pub struct BusPull {
    pub high: bool,
    pub ohms: f64,
    pub resistors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BusNet {
    pub name: String,
    /// The firmware part's GPIO pin names, in package order.
    pub chip: Vec<String>,
    pub pull: Option<BusPull>,
    /// Sum of every firmware and bus model pin's `c_pin`; unknown if any contribution is unknown.
    pub farads: Option<f64>,
    /// The firmware part's input threshold as a fraction of its supply, when
    /// this net carries a firmware GPIO. Bus model parts carry no threshold.
    pub vih: Option<f64>,
}

/// The bus parts beside one firmware part and their nets; empty without bus parts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BusSide {
    pub parts: Vec<BusPart>,
    pub nets: Vec<BusNet>,
}

impl BusSide {
    /// Pure conversion to the machine's board description. The netlist has no strong tie: a
    /// pin tied to its own rail becomes a one-pin `{refdes}.{pin}` net with a pull up or down.
    /// Known pull-up resistance, pin capacitance and input threshold give a rounded-up rise time.
    pub fn board(&self, seed: u64) -> BoardDesc {
        let parts = self.parts.iter().map(|part| PartDesc {
            refdes: part.path.clone().into(), kind: part.kind,
        }).collect();
        let mut nets: Vec<NetDesc> = self.nets.iter().map(|net| NetDesc {
            name: net.name.clone().into(),
            pull: net.pull.as_ref().map_or(Pull::None, |p| if p.high { Pull::Up } else { Pull::Down }),
            rise_ps: net.pull.as_ref().filter(|p| p.high).zip(net.farads).zip(net.vih)
                .map(|((pull, farads), vih)| (pull.ohms * farads * (1.0 / (1.0 - vih)).ln() * 1e12).ceil() as u64),
            pins: net.chip.iter().map(|name| PinRef::Chip(name.clone().into())).collect(),
        }).collect();
        for (i, part) in self.parts.iter().enumerate() {
            for (name, conn) in &part.pins {
                let pin = PinRef::Part {
                    part: u16::try_from(i).expect("board part indices fit in u16"), pin: name.clone().into(),
                };
                match conn {
                    BusConn::Net(net) => nets[*net as usize].pins.push(pin),
                    BusConn::High | BusConn::Low => nets.push(NetDesc {
                        name: format!("{}.{name}", part.path).into(),
                        pull: if *conn == BusConn::High { Pull::Up } else { Pull::Down },
                        rise_ps: None,
                        pins: vec![pin],
                    }),
                }
            }
        }
        BoardDesc { seed, parts, nets }
    }
}

/// What the analog lowering needs from the bus side.
#[derive(Debug, Default)]
pub(crate) struct Buses {
    pub host: Option<(String, BusSide)>,
    /// Firmware GPIO pin indices with no circuit device.
    pub skip: Vec<usize>,
    /// Consumed pull resistor part indices, with no circuit device or live-value tuning.
    pub consumed: HashSet<usize>,
    /// Signal nets with no analog node or floating-net note.
    pub nets: HashSet<NetRef>,
    pub diags: Vec<LowerDiag>,
}

/// A firmware or bus part's power pins, by the part library's pin kind (the firmware part's
/// crystal pins excluded). Names never decide: a panel's VCI or a sensor's VDDIO is a supply.
fn power(part: &LowerPart) -> impl Iterator<Item = &LowerPin> + '_ {
    let (rails, crystal): (&[usize], &[usize]) = match &part.kind {
        PartKind::Bus { rails, .. } => (rails, &[]),
        PartKind::Firmware { rails, crystal, .. } => (rails, crystal),
        _ => (&[], &[]),
    };
    rails.iter().filter(move |r| !crystal.contains(r)).map(move |&r| &part.pins[r])
}

fn model_pin(part: &LowerPart, pin: usize) -> bool {
    let PartKind::Bus { kind, .. } = part.kind else { return false };
    kind.pins().iter().any(|n| n.eq_ignore_ascii_case(&part.pins[pin].name))
}

fn tied(part: &LowerPart, net: NetRef, gnd: NetRef) -> Option<BusConn> {
    if net == gnd {
        Some(BusConn::Low)
    } else if power(part).any(|p| p.net == Some(net)) {
        Some(BusConn::High)
    } else {
        None
    }
}

fn part_diag(diags: &mut Vec<LowerDiag>, part: &LowerPart, message: String) {
    diags.push(LowerDiag { part: Some(part.id), net: None, path: part.path.clone(), message: format!("{}: {message}", part.path) });
}

fn net_diag(diags: &mut Vec<LowerDiag>, net: &LowerNet, message: String) {
    diags.push(LowerDiag { part: None, net: Some(net.id), path: net.name.clone(), message: format!("{}: {message}", net.name) });
}

type Pin = (usize, usize);

struct Series {
    ends: [Pin; 2],
    other_net: NetRef,
    parts: Vec<usize>,
}

/// Starting at a model pin, follow the only other connection on each net. With exactly two
/// ends per net, a path from this pin cannot enter a cycle without encountering a branch.
fn series(input: &LowerInput, on: &HashMap<NetRef, Vec<Pin>>, start: NetRef, blocked: &HashSet<NetRef>) -> Option<Series> {
    if blocked.contains(&start) { return None }
    let &[a, b] = on.get(&start)?.as_slice() else { return None };
    let (first, mut next) = if model_pin(&input.parts[a.0], a.1) { (a, b) } else { (b, a) };
    if !model_pin(&input.parts[first.0], first.1) { return None }
    let mut parts = Vec::new();
    loop {
        let (i, j) = next;
        let part = &input.parts[i];
        if part.pins.len() != 2 || !matches!(part.kind, PartKind::Resistor { .. } | PartKind::Capacitor { .. }) {
            return None;
        }
        let far = part.pins[1 - j].net?;
        if blocked.contains(&far) { return None }
        parts.push(i);
        let &[a, b] = on.get(&far)?.as_slice() else { return None };
        next = if a == (i, 1 - j) { b } else { a };
        let peer = &input.parts[next.0];
        if model_pin(peer, next.1) {
            if next.0 == first.0 || tied(peer, far, input.ground).is_some() { return None }
            return Some(Series { ends: [first, next], other_net: far, parts });
        }
    }
}

/// Bus models beside the first firmware part the bench runs. Classification uses a connection
/// map, as the logic side does. Refusals are collected before any pull or firmware pin is removed
/// from the circuit, so a rejected part leaves no orphan bus nets or consumed pulls.
pub(crate) fn buses(input: &LowerInput) -> Buses {
    let mut b = Buses::default();
    let bus: Vec<usize> = input.parts.iter().enumerate()
        .filter(|(_, p)| matches!(p.kind, PartKind::Bus { .. })).map(|(i, _)| i).collect();
    if bus.is_empty() { return b }
    let host = input.firmware.iter().find_map(|(fw, _)| {
        input.parts.iter().position(|p| p.path == fw.path && matches!(p.kind, PartKind::Firmware { .. }))
    });
    let Some(host) = host else {
        for &i in &bus {
            part_diag(&mut b.diags, &input.parts[i], "a bus part runs beside the board's firmware; this bench runs none, so it is not simulated".into());
        }
        return b;
    };
    let PartKind::Firmware { rails, .. } = &input.parts[host].kind else { unreachable!("host is a firmware part") };
    let host_gpio = |i: usize, j: usize| i == host && !rails.contains(&j);
    let mut on: HashMap<NetRef, Vec<Pin>> = HashMap::new();
    for (i, part) in input.parts.iter().enumerate() {
        for (j, pin) in part.pins.iter().enumerate() {
            if let Some(net) = pin.net { on.entry(net).or_default().push((i, j)); }
        }
    }
    let bench: HashSet<NetRef> = input.supplies.iter().map(|s| s.net)
        .chain(input.sources.iter().flat_map(|s| [s.a, s.b])).collect();
    // Steady sources and enabled regulators hold their rails. A switched or otherwise unheld
    // rail is assumed held only on the bus side, with a note; its analog devices stay intact.
    let mut held: HashSet<NetRef> = input.supplies.iter().map(|s| s.net).collect();
    held.insert(input.ground);
    for part in &input.parts {
        if matches!(part.kind, PartKind::UsbPower { .. }) {
            held.extend(part.pins.iter().filter(|p| p.name.eq_ignore_ascii_case("VBUS")).filter_map(|p| p.net));
        }
    }
    loop {
        let before = held.len();
        for source in &input.sources {
            if matches!(source.kind, SourceKind::Vdc(_)) {
                if held.contains(&source.b) { held.insert(source.a); }
                if held.contains(&source.a) { held.insert(source.b); }
            }
        }
        for part in &input.parts {
            if matches!(part.kind, PartKind::Regulator { .. }) {
                let net = |name: &str| part.pins.iter().find(|p| p.name.eq_ignore_ascii_case(name)).and_then(|p| p.net);
                if let (Some(vin), Some(gnd), Some(en), Some(out)) = (net("VIN"), net("GND"), net("EN"), net("VOUT")) {
                    if en != gnd && [vin, gnd, en].iter().all(|n| held.contains(n)) { held.insert(out); }
                }
            }
        }
        if held.len() == before { break }
    }

    let mut valid: HashSet<usize> = bus.iter().copied().collect();
    let mut unmodeled = HashSet::new();
    for &i in &bus {
        let part = &input.parts[i];
        let PartKind::Bus { kind, rails, .. } = &part.kind else { unreachable!("a bus part") };
        for (j, pin) in part.pins.iter().enumerate() {
            if rails.contains(&j) {
                match pin.net {
                    None => {
                        part_diag(&mut b.diags, part, format!("supply pin {} is not connected; not simulated", pin.name));
                        valid.remove(&i);
                    }
                    Some(net) if !held.contains(&net) => part_diag(&mut b.diags, part,
                        format!("supply pin {} is on a rail not held by a supply; treated as held on the bus side", pin.name)),
                    Some(_) => (),
                }
            } else if !pin.name.eq_ignore_ascii_case("NC") && !kind.pins().iter().any(|n| n.eq_ignore_ascii_case(&pin.name)) {
                let shared = pin.net.and_then(|net| on.get(&net)).is_some_and(|pins| pins.iter().any(|&(p, k)| {
                    host_gpio(p, k) || (p != i && model_pin(&input.parts[p], k))
                }));
                if shared {
                    part_diag(&mut b.diags, part, format!("pin {} is not a model pin; not simulated", pin.name));
                    valid.remove(&i);
                } else {
                    unmodeled.insert((i, j));
                    part_diag(&mut b.diags, part, format!("pin {} has no bus model; left to the circuit", pin.name));
                }
            }
        }
        for name in kind.pins() {
            let pins: Vec<_> = part.pins.iter().filter(|p| p.name.eq_ignore_ascii_case(name)).collect();
            let problem = match pins.as_slice() {
                [] => Some(format!("no {name} pin; not simulated")),
                [pin] if pin.net.is_none() => Some(format!("pin {name} is not connected; not simulated")),
                [_] => None,
                _ => Some(format!("more than one {name} pin; not simulated")),
            };
            if let Some(problem) = problem {
                part_diag(&mut b.diags, part, problem);
                valid.remove(&i);
            }
        }
    }

    // A source, supply or probe load anywhere along a chain is a branch too.
    let mut blocked = bench.clone();
    blocked.insert(input.ground);
    for probe in &input.probes {
        if probe.tip.r.is_some() || probe.tip.c.is_some() {
            match probe.probe {
                Probe::V(n) => { blocked.insert(n); }
                Probe::VDiff(a, b) => blocked.extend([a, b]),
                _ => (),
            }
        }
    }
    // Each chain contributes a single candidate in schematic order, just like a direct net.
    let mut other_ends = HashSet::new();
    let mut peers: HashMap<usize, Vec<(usize, &LowerNet)>> = HashMap::new();
    let mut candidates = Vec::new();
    for net in &input.nets {
        if other_ends.contains(&net.id) { continue }
        let Some(conns) = on.get(&net.id) else { continue };
        let participants: Vec<usize> = conns.iter().filter(|&&(i, j)| model_pin(&input.parts[i], j))
            .map(|&(i, _)| i).collect();
        if !participants.iter().any(|&i| tied(&input.parts[i], net.id, input.ground).is_none()) { continue }
        if let Some(chain) = series(input, &on, net.id, &blocked) {
            let [a, b] = chain.ends.map(|(part, _)| part);
            peers.entry(a).or_default().push((b, net));
            peers.entry(b).or_default().push((a, net));
            other_ends.insert(chain.other_net);
            candidates.push((net, vec![a, b], Vec::new(), Some(chain)));
            continue;
        }
        let mut ok = true;
        if net.id == input.ground || bench.contains(&net.id) {
            net_diag(&mut b.diags, net, "carries a bench supply or source; bus parts not simulated".into());
            ok = false;
        }
        let mut pulls: Vec<(usize, bool, f64)> = Vec::new();
        let mut seen = HashSet::new();
        for &(i, j) in conns {
            let part = &input.parts[i];
            if host_gpio(i, j) || model_pin(part, j) || unmodeled.contains(&(i, j)) || matches!(part.kind, PartKind::Passive)
                || (matches!(part.kind, PartKind::Bus { .. }) && part.pins[j].name.eq_ignore_ascii_case("NC")) { continue }
            if !seen.insert(i) { continue }
            if let PartKind::Resistor { ohms } = part.kind {
                let other = if part.pins.len() == 2 { part.pins[1 - j].net } else { None };
                let high = other.and_then(|other| {
                    if other == input.ground { return Some(false) }
                    std::iter::once(host).chain(participants.iter().copied()).any(|p| {
                        power(&input.parts[p]).any(|pin| pin.net == Some(other))
                    }).then_some(true)
                });
                if let Some(high) = high {
                    if ohms.is_finite() && ohms > 0.0 {
                        pulls.push((i, high, ohms));
                        continue;
                    }
                    net_diag(&mut b.diags, net, format!("{}: pull resistance must be finite and > 0 ohm; not simulated", part.path));
                    ok = false;
                    continue;
                }
            }
            let problem = if matches!(part.kind, PartKind::Logic { .. }) {
                format!("{} shares a net with a bus part; not simulated yet", part.path)
            } else {
                format!("{} couples a bus net to the circuit; not simulated yet", part.path)
            };
            net_diag(&mut b.diags, net, problem);
            ok = false;
        }
        // A probe's non-ideal tip is an analog element too; an ideal probe loads nothing.
        for probe in &input.probes {
            let touches = match probe.probe {
                Probe::V(n) => n == net.id,
                Probe::VDiff(a, c) => a == net.id || c == net.id,
                _ => false,
            };
            if touches && (probe.tip.r.is_some() || probe.tip.c.is_some()) {
                net_diag(&mut b.diags, net, format!("probe {} couples a bus net to the circuit; not simulated yet", probe.label));
                ok = false;
            }
        }
        if pulls.iter().any(|p| p.1) && pulls.iter().any(|p| !p.1) {
            let names = |high| pulls.iter().filter(|p| p.1 == high)
                .map(|p| input.parts[p.0].path.as_str()).collect::<Vec<_>>().join(", ");
            net_diag(&mut b.diags, net, format!("pull-up {} and pull-down {} conflict; not simulated", names(true), names(false)));
            ok = false;
        }
        if ok {
            candidates.push((net, participants, pulls, None));
        } else {
            for i in participants { valid.remove(&i); }
        }
    }

    // A rejected peer leaves its end in the analog circuit. Refuse that coupling at the other
    // end as well, including when a part is joined to several peers by different chains.
    let mut rejected: Vec<_> = bus.iter().copied().filter(|i| !valid.contains(i)).collect();
    while let Some(i) = rejected.pop() {
        if let Some(neighbors) = peers.get(&i) {
            for &(peer, net) in neighbors {
                if valid.remove(&peer) {
                    net_diag(&mut b.diags, net, format!("series path reaches rejected {}; bus coupling not simulated", input.parts[i].path));
                    rejected.push(peer);
                }
            }
        }
    }
    let mut side = BusSide::default();
    let mut nets = HashMap::new();
    for (net, participants, pulls, chain) in candidates {
        if !participants.iter().any(|i| valid.contains(i)) { continue }
        let mut farads = 0.0;
        let mut missing = Vec::new();
        let mut chip = Vec::new();
        let mut vih = None;
        let pins = chain.as_ref().map_or(on[&net.id].as_slice(), |c| c.ends.as_slice());
        for &(i, j) in pins {
            let part = &input.parts[i];
            let c_pin = if host_gpio(i, j) {
                chip.push(part.pins[j].name.clone());
                let PartKind::Firmware { c_pin, pads, .. } = part.kind else { unreachable!("host") };
                vih = Some(pads.vih);
                c_pin
            } else if model_pin(part, j) {
                let PartKind::Bus { c_pin, .. } = part.kind else { unreachable!("model pin") };
                c_pin
            } else { continue };
            if let Some(c) = c_pin { farads += c; }
            else if !missing.contains(&part.path.as_str()) { missing.push(part.path.as_str()); }
        }
        let farads = if missing.is_empty() { Some(farads) } else {
            net_diag(&mut b.diags, net, format!("no c_pin for {}; bus pin capacitance is unknown", missing.join(", ")));
            None
        };
        let pull = pulls.first().map(|&(_, high, _)| BusPull {
            high,
            ohms: 1.0 / pulls.iter().map(|p| 1.0 / p.2).sum::<f64>(),
            resistors: pulls.iter().map(|p| input.parts[p.0].path.clone()).collect(),
        });
        // An unmodeled pin can share its own part's model net. Keep its analog node and pulls.
        let keep_analog = pins.iter().any(|p| unmodeled.contains(p));
        for (i, high, _) in pulls {
            if !keep_analog { b.consumed.insert(i); }
            let side = if keep_analog { "also retained in the circuit" } else { "digital side" };
            part_diag(&mut b.diags, &input.parts[i], format!("pull-{} of {}, {side}", if high { "up" } else { "down" }, net.name));
        }
        nets.insert(net.id, side.nets.len() as u32);
        if let Some(chain) = chain {
            nets.insert(chain.other_net, side.nets.len() as u32);
            let names = chain.parts.iter().map(|&i| input.parts[i].path.as_str()).collect::<Vec<_>>();
            net_diag(&mut b.diags, net, format!("series coupling through {}; component values are not simulated on the bus", names.join(", ")));
        } else if !keep_analog {
            b.nets.insert(net.id);
        }
        side.nets.push(BusNet { name: net.name.clone(), chip, pull, farads, vih });
    }
    for i in bus.into_iter().filter(|i| valid.contains(i)) {
        let part = &input.parts[i];
        let PartKind::Bus { model, kind, .. } = &part.kind else { unreachable!("a bus part") };
        let pins = kind.pins().iter().map(|&name| {
            let net = part.pins.iter().find(|p| p.name.eq_ignore_ascii_case(name)).and_then(|p| p.net)
                .expect("an accepted model pin has a net");
            let conn = tied(part, net, input.ground).unwrap_or_else(|| BusConn::Net(nets[&net]));
            (name.to_owned(), conn)
        }).collect();
        side.parts.push(BusPart { path: part.path.clone(), comp: part.id, model: model.clone(), kind: *kind, pins });
    }
    b.skip = input.parts[host].pins.iter().enumerate()
        .filter(|(j, p)| host_gpio(host, *j) && p.net.is_some_and(|n| b.nets.contains(&n))).map(|(j, _)| j).collect();
    b.host = Some((input.parts[host].path.clone(), side));
    b
}

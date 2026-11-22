//! Trace queries (C4, E2.5): pure functions of the design that answer "what drives this, what
//! does it drive, where does its power come from", across boards (mates and cables are hops).
//! They feed the explorer's reveal, the AI's `elec.trace` and ERC.
//!
//! First version by ELEC at M0; EVIEW and EXPLORE own them from here. Transparent hops: a
//! two-terminal part valued in ohms or henries is a series hop; one valued in farads an AC hop
//! (marked, never part of a signal group); a part with power inputs and power outputs (a
//! regulator) a power hop, by inference until parts declare `through` (E2.18 7); a link pair a
//! mate hop (a cable hop when either side is a cable unit). Signal groups never enter power or
//! ground nets (a pull-up is a termination, not a hop).
//!
//! Cost: each query is a breadth-first walk, O(pins + hops) of the part of the design it
//! reaches; they run on the explorer worker, never per frame.

use crate::{Design, UnitKind};
use fab_elec_circuit::{NetKind, QtyUnit};
use fab_elec_core::{CompRef, NetRef, PinIx, PinRef};
use fab_elec_parts::{PinKind, VRange};
use std::collections::{HashMap, VecDeque};

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum HopKind {
    Series,
    /// A capacitor: an AC path, shown marked.
    Ac,
    /// A regulator or load switch: power in to power out.
    Power,
    Switch,
    Buffer,
    Level,
    Mate,
    Cable,
}

/// One step through a part (or a link) from one pin to another.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Hop {
    pub through: CompRef,
    pub from: PinRef,
    pub to: PinRef,
    pub kind: HopKind,
}

/// How sure a traced answer is (as Scope's resolved, conditional, candidate).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Evidence {
    Declared,
    Typed,
    Inferred,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Traced {
    pub end: PinRef,
    pub hops: Vec<Hop>,
    pub evidence: Evidence,
}

/// Nets joined through transparent hops.
#[derive(Clone, PartialEq, Debug)]
pub struct SignalGroup {
    pub nets: Vec<NetRef>,
    pub hops: Vec<Hop>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Source {
    /// A power output pin.
    Pin(PinRef),
    /// Nothing on the board drives it: it comes in through this net (a connector or a port).
    External(NetRef),
}

/// A supply pin's rail, climbed to its source.
#[derive(Clone, PartialEq, Debug)]
pub struct RailPath {
    pub pin: PinRef,
    pub rail: NetRef,
    pub hops: Vec<Hop>,
    pub source: Source,
    pub volts: Option<VRange>,
    /// The source's current limit in amperes, when it declares one.
    pub budget: Option<f64>,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ControlRole {
    Drives,
    Senses,
}

/// A controller pin that drives or reads one of a part's pins.
#[derive(Clone, PartialEq, Debug)]
pub struct Control {
    pub mcu_pin: PinRef,
    pub target_pin: PinRef,
    pub hops: Vec<Hop>,
    pub role: ControlRole,
}

/// Shortest part chains between two sets of parts.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct PathSet {
    pub paths: Vec<Vec<CompRef>>,
}

/// Parts within some hops of a selection, with their distance, and the nets crossed.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Neighbourhood {
    pub comps: Vec<(CompRef, u32)>,
    pub nets: Vec<NetRef>,
}

fn net_kind(d: &Design, n: NetRef) -> NetKind {
    d.unit(n.board).and_then(|u| u.circuit.nets.get(n.net.ix())).map_or(NetKind::Signal, |n| n.kind)
}

fn pin_kind(d: &Design, p: PinRef) -> Option<PinKind> {
    d.unit(p.board)?.circuit.pin_kind(p.comp, p.pin)
}

/// Every hop out of net `n`, with the net it reaches.
fn hops_from(d: &Design, n: NetRef) -> Vec<(Hop, NetRef)> {
    let mut out = Vec::new();
    let Some(u) = d.unit(n.board) else { return out };
    let c = &u.circuit;
    for (comp, pin) in c.pins_of(n.net) {
        let k = &c.comps[comp.ix()];
        let through = CompRef { board: n.board, comp };
        let from = PinRef { board: n.board, comp, pin };
        if k.part.pins.len() == 2 && k.conns.len() == 2 {
            let kind = match k.value.as_ref().and_then(|v| v.qty).map(|q| q.unit) {
                Some(QtyUnit::Ohm | QtyUnit::Henry) => Some(HopKind::Series),
                Some(QtyUnit::Farad) => Some(HopKind::Ac),
                _ => None,
            };
            if let Some(kind) = kind {
                let (op, on) = *k.conns.iter().find(|(p, _)| *p != pin).unwrap_or(&k.conns[0]);
                if on != n.net {
                    out.push((Hop { through, from, to: PinRef { board: n.board, comp, pin: op }, kind }, NetRef { board: n.board, net: on }));
                }
            }
        }
        if c.pin_kind(comp, pin) == Some(PinKind::PowerIn) && net_kind(d, n) != NetKind::Ground {
            for (op, on) in &k.conns {
                if c.pin_kind(comp, *op) == Some(PinKind::PowerOut) && *on != n.net {
                    out.push((Hop { through, from, to: PinRef { board: n.board, comp, pin: *op }, kind: HopKind::Power }, NetRef { board: n.board, net: *on }));
                }
            }
        }
    }
    for l in &d.links {
        for (a, b) in d.link_pairs(l) {
            for (x, y) in [(a, b), (b, a)] {
                if d.net_of(x) == Some(n) {
                    if let Some(m) = d.net_of(y) {
                        let cable = [x.board, y.board].iter().any(|u| d.unit(*u).is_some_and(|u| matches!(u.kind, UnitKind::Cable(_))));
                        let kind = if cable { HopKind::Cable } else { HopKind::Mate };
                        out.push((Hop { through: x.comp_ref(), from: x, to: y, kind }, m));
                    }
                }
            }
        }
    }
    out
}

/// Walk from `start` over hops `allow` accepts; returns each reached net with the hop path.
fn walk(d: &Design, start: NetRef, depth: Option<u32>, allow: impl Fn(&Hop, NetRef) -> bool) -> Vec<(NetRef, Vec<Hop>)> {
    let mut seen: HashMap<NetRef, Vec<Hop>> = HashMap::new();
    let mut order = vec![start];
    seen.insert(start, Vec::new());
    let mut q = VecDeque::from([(start, 0u32)]);
    while let Some((n, dist)) = q.pop_front() {
        if depth.is_some_and(|m| dist >= m) {
            continue;
        }
        for (hop, m) in hops_from(d, n) {
            if seen.contains_key(&m) || !allow(&hop, m) {
                continue;
            }
            let mut path = seen[&n].clone();
            path.push(hop);
            seen.insert(m, path);
            order.push(m);
            q.push_back((m, dist + 1));
        }
    }
    order.into_iter().map(|n| (n, seen.remove(&n).unwrap_or_default())).collect()
}

fn transparent(d: &Design) -> impl Fn(&Hop, NetRef) -> bool + '_ {
    move |h: &Hop, m: NetRef| matches!(h.kind, HopKind::Series | HopKind::Mate | HopKind::Cable) && net_kind(d, m) == NetKind::Signal
}

/// Nets joined to `net` through transparent hops.
pub fn signal_group(d: &Design, net: NetRef) -> SignalGroup {
    let reached = walk(d, net, None, transparent(d));
    let mut hops: Vec<Hop> = reached.iter().flat_map(|(_, h)| h.last().copied()).collect();
    hops.sort_by_key(|h| (h.from, h.to));
    let mut nets: Vec<NetRef> = reached.into_iter().map(|(n, _)| n).collect();
    nets.sort();
    SignalGroup { nets, hops }
}

fn pins_on(d: &Design, n: NetRef) -> Vec<PinRef> {
    d.unit(n.board).map(|u| u.circuit.pins_of(n.net).into_iter().map(|(comp, pin)| PinRef { board: n.board, comp, pin }).collect()).unwrap_or_default()
}

fn traced_where(d: &Design, pin: PinRef, keep: impl Fn(PinKind) -> bool) -> Vec<Traced> {
    let Some(net) = d.net_of(pin) else { return Vec::new() };
    let mut out = Vec::new();
    for (n, hops) in walk(d, net, None, transparent(d)) {
        for p in pins_on(d, n) {
            let through_hop = hops.iter().any(|h| h.through.comp == p.comp && h.through.board == p.board);
            if p != pin && !through_hop && pin_kind(d, p).is_some_and(&keep) {
                out.push(Traced { end: p, hops: hops.clone(), evidence: Evidence::Typed });
            }
        }
    }
    out.sort_by_key(|t| (t.evidence, t.hops.len(), t.end));
    out
}

/// The pins that drive `pin`'s signal, traced through series parts and links.
pub fn drivers(d: &Design, pin: PinRef) -> Vec<Traced> {
    traced_where(d, pin, |k| k.drives())
}

/// The pins `pin`'s signal drives.
pub fn loads(d: &Design, pin: PinRef) -> Vec<Traced> {
    traced_where(d, pin, |k| k.loads())
}

/// Every supply pin of `comp` with its rail climbed to the source.
pub fn power_tree(d: &Design, comp: CompRef) -> Vec<RailPath> {
    let Some(u) = d.unit(comp.board) else { return Vec::new() };
    let Some(k) = u.circuit.comps.get(comp.comp.ix()) else { return Vec::new() };
    let mut out = Vec::new();
    for (pin, net) in &k.conns {
        let pr = PinRef { board: comp.board, comp: comp.comp, pin: *pin };
        let rail = NetRef { board: comp.board, net: *net };
        if pin_kind(d, pr) != Some(PinKind::PowerIn) || net_kind(d, rail) == NetKind::Ground {
            continue;
        }
        // climb: on each rail find a power output; through a regulator continue from its input
        let mut hops = Vec::new();
        let mut cur = rail;
        let mut source = Source::External(rail);
        let mut seen = vec![rail];
        'climb: loop {
            for (n, h) in walk(d, cur, None, |h, m| matches!(h.kind, HopKind::Series | HopKind::Mate | HopKind::Cable) && net_kind(d, m) != NetKind::Ground) {
                for p in pins_on(d, n) {
                    if pin_kind(d, p) != Some(PinKind::PowerOut) {
                        continue;
                    }
                    hops.extend(h.iter().copied());
                    source = Source::Pin(p);
                    // a regulator: continue from its power input
                    let reg = d.unit(p.board).map(|u| &u.circuit.comps[p.comp.ix()]);
                    let input = reg.and_then(|r| r.conns.iter().find(|(q, qn)| {
                        let qr = PinRef { board: p.board, comp: p.comp, pin: *q };
                        let qn = NetRef { board: p.board, net: *qn };
                        pin_kind(d, qr) == Some(PinKind::PowerIn) && net_kind(d, qn) != NetKind::Ground && !seen.contains(&qn)
                    }));
                    if let Some((q, qn)) = input {
                        let from = PinRef { board: p.board, comp: p.comp, pin: *q };
                        hops.push(Hop { through: p.comp_ref(), from, to: p, kind: HopKind::Power });
                        cur = NetRef { board: p.board, net: *qn };
                        seen.push(cur);
                        continue 'climb;
                    }
                    break 'climb;
                }
            }
            break;
        }
        let volts = d.unit(rail.board).and_then(|u| u.circuit.nets[rail.net.ix()].volts);
        let budget = match &source {
            Source::Pin(p) => d.unit(p.board).and_then(|u| u.circuit.comps[p.comp.ix()].part.pins[p.pin.ix()].limits.as_ref().and_then(|l| l.current)),
            Source::External(_) => None,
        };
        out.push(RailPath { pin: pr, rail, hops, source, volts, budget });
    }
    out
}

/// Controller pins (pins of parts with many pins that drive or read) acting on `comp`'s pins.
pub fn controllers(d: &Design, comp: CompRef) -> Vec<Control> {
    let Some(u) = d.unit(comp.board) else { return Vec::new() };
    let Some(k) = u.circuit.comps.get(comp.comp.ix()) else { return Vec::new() };
    let is_controller = |p: PinRef| d.unit(p.board).is_some_and(|u| u.circuit.comps[p.comp.ix()].part.pins.len() >= 8);
    let mut out = Vec::new();
    for (pin, _) in &k.conns {
        let pr = PinRef { board: comp.board, comp: comp.comp, pin: *pin };
        if matches!(pin_kind(d, pr), Some(PinKind::PowerIn | PinKind::PowerOut)) {
            continue;
        }
        for (list, role) in [(drivers(d, pr), ControlRole::Drives), (loads(d, pr), ControlRole::Senses)] {
            for t in list {
                if t.end.comp != comp.comp && is_controller(t.end) && matches!(pin_kind(d, t.end), Some(PinKind::Bidi | PinKind::Out | PinKind::In | PinKind::TriState)) {
                    out.push(Control { mcu_pin: t.end, target_pin: pr, hops: t.hops, role });
                }
            }
        }
    }
    out
}

/// Everything downstream of a net, through every kind of hop, to `depth` hops.
pub fn downstream(d: &Design, from: NetRef, depth: Option<u32>) -> Vec<Traced> {
    let mut out = Vec::new();
    for (n, hops) in walk(d, from, depth, |h, m| h.kind != HopKind::Ac && net_kind(d, m) != NetKind::Ground) {
        for p in pins_on(d, n) {
            if pin_kind(d, p).is_some_and(|k| k.loads()) {
                out.push(Traced { end: p, hops: hops.clone(), evidence: if hops.iter().any(|h| h.kind == HopKind::Power) { Evidence::Inferred } else { Evidence::Typed } });
            }
        }
    }
    out
}

/// Parts adjacent to a part: through signal nets of its unit and through links.
fn comp_neighbours(d: &Design, c: CompRef) -> Vec<(CompRef, NetRef)> {
    let Some(u) = d.unit(c.board) else { return Vec::new() };
    let mut out = Vec::new();
    for net in u.circuit.nets_of(c.comp) {
        let nr = NetRef { board: c.board, net };
        if net_kind(d, nr) != NetKind::Signal {
            continue;
        }
        for (other, _) in u.circuit.pins_of(net) {
            if other != c.comp {
                out.push((CompRef { board: c.board, comp: other }, nr));
            }
        }
        for (h, m) in hops_from(d, nr) {
            if matches!(h.kind, HopKind::Mate | HopKind::Cable) {
                for p in pins_on(d, m) {
                    out.push((p.comp_ref(), m));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Shortest part chains from any of `a` to any of `b`.
pub fn paths(d: &Design, a: &[CompRef], b: &[CompRef]) -> PathSet {
    let mut set = PathSet::default();
    for &s in a {
        let mut prev: HashMap<CompRef, Option<CompRef>> = HashMap::from([(s, None)]);
        let mut q = VecDeque::from([s]);
        while let Some(c) = q.pop_front() {
            for (n, _) in comp_neighbours(d, c) {
                if let std::collections::hash_map::Entry::Vacant(e) = prev.entry(n) {
                    e.insert(Some(c));
                    q.push_back(n);
                }
            }
        }
        for &t in b {
            if t == s || !prev.contains_key(&t) {
                continue;
            }
            let mut chain = vec![t];
            let mut cur = t;
            while let Some(Some(p)) = prev.get(&cur) {
                chain.push(*p);
                cur = *p;
            }
            chain.reverse();
            set.paths.push(chain);
        }
    }
    set
}

/// The parts within `hops` of the selection (signal nets and links), nearest first.
pub fn neighbourhood(d: &Design, sel: &[CompRef], hops: u32) -> Neighbourhood {
    let mut dist: HashMap<CompRef, u32> = sel.iter().map(|c| (*c, 0)).collect();
    let mut nets: Vec<NetRef> = Vec::new();
    let mut q: VecDeque<CompRef> = sel.iter().copied().collect();
    while let Some(c) = q.pop_front() {
        let dc = dist[&c];
        if dc >= hops {
            continue;
        }
        for (n, net) in comp_neighbours(d, c) {
            nets.push(net);
            if !dist.contains_key(&n) {
                dist.insert(n, dc + 1);
                q.push_back(n);
            }
        }
    }
    let mut comps: Vec<(CompRef, u32)> = dist.into_iter().collect();
    comps.sort_by_key(|(c, k)| (*k, *c));
    nets.sort();
    nets.dedup();
    Neighbourhood { comps, nets }
}

/// A pin by its part's pin name, for queries written by hand (tests, the AI).
pub fn pin_named(d: &Design, comp: CompRef, name: &str) -> Option<PinRef> {
    let pin: PinIx = d.unit(comp.board)?.circuit.comps.get(comp.comp.ix())?.part.pin(name)?;
    Some(PinRef { board: comp.board, comp: comp.comp, pin })
}

//! Logic mode's lowering rule, report only: which nets of a circuit are digital, by element kinds
//! and roles, never names, as a fixed point over the board that fails closed; the switch matrices
//! and indicator loads on them; and the unknowns the analog side would keep. Nothing here changes
//! a run.
//!
//! - A logic pin is a GPIO pad the caller names digital: a firmware pin never in an analog mode, a
//!   logic part's output. A pad it does not name is analog, and so is its net.
//! - Rails are the ground and the nets the design declares supplies (`NodeInfo::supply`).
//! - The nets judged: each with a logic pin, and each with no pad that a resistor joins to one of
//!   those and that holds only resistors, capacitors, diodes and switches (a pin's series resistor
//!   to a key row, say).
//! - A net is digital when it is no rail, a logic pin is on it or drives it through a series
//!   resistor, and everything else on it is one of:
//!   - a capacitor to a rail (a pad's input capacitance; the time constant below decides the rest);
//!   - a resistor to a rail (a pull);
//!   - a resistor to another digital net (a series resistor);
//!   - a voltage-controlled switch to another digital net or a rail, directly or through a diode
//!     whose middle node touches nothing else, the switch's control held by a source alone (a key
//!     and its press; a button to ground);
//!   - an indicator branch: resistors and diodes in a chain whose inner nodes touch nothing else,
//!     ending at a rail or at another digital net (an LED and its resistor; a diode to a rail); a
//!     chain with a diode in it is a branch before it is a series resistor.
//! - Its time constant, its capacitance to the rails times the weakest output that drives it (a
//!   pad's on resistance, through a series resistor or a switch), is under a tenth of the read
//!   interval when the caller knows it, else under its pads' edge time. A digital net whose release
//!   on its pull (a pad's pull in an input mode with one, directly or through a series resistor) is
//!   slower than that is timed: it changes its own R C after a release.
//! - Anything else on a net (an analog pad, a source, a device of another kind, a capacitor or a
//!   resistor to a net that is not digital) keeps it analog with that reason, and the nets that
//!   leaned on it are judged again, until nothing changes.
//!
//! Cost: O(devices + nodes) a pass, a few passes; one solver layout for the unknowns' count.

use crate::circuit::{Cell, Gpio, PinMode, PinPull, SimCircuit, Source};
use crate::dev::sw::SwModel;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

/// What the caller knows of the circuit's pads, by their GPIO devices' names.
#[derive(Clone, Debug, Default)]
pub struct Pads {
    /// The logic pins: a firmware pin never in an analog mode, a logic part's output. Every other
    /// pad is analog.
    pub digital: HashSet<String>,
    /// The pull a firmware declares on a pad (`input(pin, pull_up)`), which its reset mode lacks.
    pub pulls: HashMap<String, PinPull>,
    /// The pads nothing ever reads: a logic part's outputs, a firmware pin declared only an output.
    pub write_only: HashSet<String>,
}

/// A digital net's timing.
#[derive(Clone, Debug, PartialEq)]
pub struct Timing {
    /// Its capacitance to the rails times the weakest output that drives it, seconds.
    pub tau: f64,
    /// Its capacitance times its pulls in parallel, seconds; None without a pull.
    pub release: Option<f64>,
    /// The bound `tau` is under (a tenth of the read interval, or its pads' edge time).
    pub bound: f64,
}

/// What the rule made of a net with a logic pin on it.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    Digital(Timing),
    /// Analog, and why.
    Analog(String),
}

impl Timing {
    /// The release is slower than the bound: the net changes its own R C after a release.
    pub fn timed(&self) -> bool {
        self.release.is_some_and(|r| r > self.bound)
    }
}

/// A key matrix: switches (each alone or with its diode) between digital nets, and the sources
/// that press them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Matrix {
    /// Its digital nets (a rail a button closes to is not one).
    pub nets: Vec<String>,
    /// Its keys that close to a rail (buttons).
    pub to_rail: usize,
    /// (switch, its diode, its press source).
    pub keys: Vec<(String, Option<String>, String)>,
}

/// An indicator branch: its devices from a digital net to a rail or another digital net.
#[derive(Clone, Debug, PartialEq)]
pub struct Indicator {
    pub from: String,
    pub to: String,
    pub devices: Vec<String>,
}

/// The rule's report on a circuit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogicReport {
    /// Every net judged, in node order, and its verdict.
    pub nets: Vec<(String, Verdict)>,
    pub matrices: Vec<Matrix>,
    pub indicators: Vec<Indicator>,
    /// The GPIO pads on the circuit that the caller did not name digital.
    pub analog_pads: usize,
    /// The devices the Logic lowering takes off the analog side, by name.
    pub taken: Vec<String>,
    /// The solver's unknowns now, and those the analog side keeps.
    pub unknowns: (usize, usize),
}

impl LogicReport {
    pub fn digital(&self) -> impl Iterator<Item = (&str, &Timing)> {
        self.nets.iter().filter_map(|(n, v)| match v {
            Verdict::Digital(t) => Some((n.as_str(), t)),
            Verdict::Analog(_) => None,
        })
    }
}

impl fmt::Display for LogicReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digital = self.digital().count();
        let timed = self.digital().filter(|(_, t)| t.timed()).count();
        writeln!(f, "logic lowering: {digital} of {} nets judged digital ({timed} timed), {} matrices ({} keys), {} indicator branches, {} devices taken; unknowns {} -> {}; {} pads analog",
            self.nets.len(), self.matrices.len(), self.matrices.iter().map(|m| m.keys.len()).sum::<usize>(), self.indicators.len(), self.taken.len(),
            self.unknowns.0, self.unknowns.1, self.analog_pads)?;
        for (net, v) in &self.nets {
            match v {
                Verdict::Digital(t) => writeln!(f, "  digital {net}: tau {:.3e} s, release {}, bound {:.3e} s{}", t.tau,
                    t.release.map_or("none".into(), |r| format!("{r:.3e} s")), t.bound, if t.timed() { " (timed)" } else { "" })?,
                Verdict::Analog(why) => writeln!(f, "  analog {net}: {why}")?,
            }
        }
        for m in &self.matrices {
            writeln!(f, "  matrix over {} nets ({}), {} keys ({} with a diode, {} to a rail)", m.nets.len(), m.nets.join(" "), m.keys.len(),
                m.keys.iter().filter(|k| k.1.is_some()).count(), m.to_rail)?;
        }
        for i in &self.indicators {
            writeln!(f, "  indicator {} -> {}: {}", i.from, i.to, i.devices.join(" "))?;
        }
        Ok(())
    }
}

/// A device of the circuit by bank and index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Dev {
    R(usize),
    C(usize),
    L(usize),
    V(usize),
    I(usize),
    Boundary(usize),
    Ctl(usize),
    B(usize),
    Sw(usize),
    Gpio(usize),
    D(usize),
    Q(usize),
    Reg(usize),
    M(usize),
    J(usize),
    O(usize),
}

/// How a device touches a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Term {
    /// A two-terminal device's end, a switch's switched end, a pad's pad.
    Main,
    /// A switch's or controlled source's control, a behavioural source's read.
    Control,
    /// A pad's supply or ground.
    Supply,
    /// Any terminal of a device of another kind.
    Other,
}

struct Board<'a> {
    c: &'a SimCircuit,
    pads: &'a Pads,
    /// Per node: the devices on it and how.
    on: Vec<Vec<(Dev, Term)>>,
    rail: Vec<bool>,
    digital_pad: Vec<bool>,
}

impl<'a> Board<'a> {
    fn new(c: &'a SimCircuit, pads: &'a Pads) -> Self {
        let d = &c.devices;
        let mut on: Vec<Vec<(Dev, Term)>> = vec![Vec::new(); c.nodes.len()];
        let mut put = |n: u32, dev: Dev, t: Term| {
            if let Some(v) = on.get_mut(n as usize) {
                if !v.contains(&(dev, t)) {
                    v.push((dev, t));
                }
            }
        };
        for (i, x) in d.r.iter().enumerate() { put(x.a, Dev::R(i), Term::Main); put(x.b, Dev::R(i), Term::Main); }
        for (i, x) in d.c.iter().enumerate() { put(x.a, Dev::C(i), Term::Main); put(x.b, Dev::C(i), Term::Main); }
        for (i, x) in d.l.iter().enumerate() { put(x.a, Dev::L(i), Term::Other); put(x.b, Dev::L(i), Term::Other); }
        for (i, x) in d.v.iter().enumerate() { put(x.p, Dev::V(i), Term::Main); put(x.n, Dev::V(i), Term::Main); }
        for (i, x) in d.i.iter().enumerate() { put(x.p, Dev::I(i), Term::Other); put(x.n, Dev::I(i), Term::Other); }
        for (i, x) in d.boundary.iter().enumerate() { put(x.node, Dev::Boundary(i), Term::Other); }
        for (i, x) in d.ctl.iter().enumerate() {
            put(x.p, Dev::Ctl(i), Term::Other); put(x.n, Dev::Ctl(i), Term::Other);
            put(x.cp, Dev::Ctl(i), Term::Control); put(x.cn, Dev::Ctl(i), Term::Control);
        }
        for (i, x) in d.b.iter().enumerate() {
            put(x.p, Dev::B(i), Term::Other); put(x.n, Dev::B(i), Term::Other);
            for (_, n) in &x.reads { put(*n, Dev::B(i), Term::Control); }
        }
        for (i, x) in d.sw.iter().enumerate() {
            put(x.p, Dev::Sw(i), Term::Main); put(x.n, Dev::Sw(i), Term::Main);
            put(x.cp, Dev::Sw(i), Term::Control); put(x.cn, Dev::Sw(i), Term::Control);
        }
        for (i, x) in d.gpio.iter().enumerate() {
            put(x.pad, Dev::Gpio(i), Term::Main); put(x.vdd, Dev::Gpio(i), Term::Supply); put(x.vss, Dev::Gpio(i), Term::Supply);
        }
        for (i, x) in d.d.iter().enumerate() { put(x.a, Dev::D(i), Term::Main); put(x.k, Dev::D(i), Term::Main); }
        for (i, x) in d.q.iter().enumerate() { for n in [x.c, x.b, x.e, x.s] { put(n, Dev::Q(i), Term::Other); } }
        for (i, x) in d.reg.iter().enumerate() { for n in [x.vin, x.gnd, x.vout, x.en] { put(n, Dev::Reg(i), Term::Other); } }
        for (i, x) in d.m.iter().enumerate() { for n in [x.d, x.g, x.s, x.b] { put(n, Dev::M(i), Term::Other); } }
        for (i, x) in d.j.iter().enumerate() { for n in [x.d, x.g, x.s] { put(n, Dev::J(i), Term::Other); } }
        for (i, x) in d.o.iter().enumerate() { for n in [x.p1, x.n1, x.p2, x.n2] { put(n, Dev::O(i), Term::Other); } }
        let rail: Vec<bool> = c.nodes.iter().enumerate().map(|(i, n)| i == 0 || n.supply).collect();
        let mut digital_pad = vec![false; d.gpio.len()];
        for (i, g) in d.gpio.iter().enumerate() {
            digital_pad[i] = pads.digital.contains(&g.name);
        }
        Board { c, pads, on, rail, digital_pad }
    }

    fn name(&self, dev: Dev) -> &'a str {
        let d = &self.c.devices;
        match dev {
            Dev::R(i) => &d.r[i].name,
            Dev::C(i) => &d.c[i].name,
            Dev::L(i) => &d.l[i].name,
            Dev::V(i) => &d.v[i].name,
            Dev::I(i) => &d.i[i].name,
            Dev::Boundary(i) => &d.boundary[i].name,
            Dev::Ctl(i) => &d.ctl[i].name,
            Dev::B(i) => &d.b[i].name,
            Dev::Sw(i) => &d.sw[i].name,
            Dev::Gpio(i) => &d.gpio[i].name,
            Dev::D(i) => &d.d[i].name,
            Dev::Q(i) => &d.q[i].name,
            Dev::Reg(i) => &d.reg[i].name,
            Dev::M(i) => &d.m[i].name,
            Dev::J(i) => &d.j[i].name,
            Dev::O(i) => &d.o[i].name,
        }
    }

    fn kind(dev: Dev) -> &'static str {
        match dev {
            Dev::R(_) => "a resistor",
            Dev::C(_) => "a capacitor",
            Dev::L(_) => "an inductor",
            Dev::V(_) => "a voltage source",
            Dev::I(_) => "a current source",
            Dev::Boundary(_) => "a boundary source",
            Dev::Ctl(_) => "a controlled source",
            Dev::B(_) => "a behavioural source",
            Dev::Sw(_) => "a switch",
            Dev::Gpio(_) => "a pad",
            Dev::D(_) => "a diode",
            Dev::Q(_) => "a BJT",
            Dev::Reg(_) => "a regulator",
            Dev::M(_) => "a MOSFET",
            Dev::J(_) => "a JFET",
            Dev::O(_) => "a line",
        }
    }

    fn node(&self, n: u32) -> &'a str {
        &self.c.nodes[n as usize].name
    }

    /// A two-terminal device's other end from `n`: resistors, capacitors, diodes, a switch's
    /// switched ends.
    fn other(&self, dev: Dev, n: u32) -> Option<u32> {
        let d = &self.c.devices;
        let (a, b) = match dev {
            Dev::R(i) => (d.r[i].a, d.r[i].b),
            Dev::C(i) => (d.c[i].a, d.c[i].b),
            Dev::D(i) => (d.d[i].a, d.d[i].k),
            Dev::Sw(i) => (d.sw[i].p, d.sw[i].n),
            _ => return None,
        };
        if a == n { Some(b) } else if b == n { Some(a) } else { None }
    }

    /// The source that alone holds switch `i`'s control (its press), when one does: a voltage
    /// source across the control nodes (one of them a rail), and nothing else on the other but
    /// switches' controls.
    fn press(&self, i: usize) -> Option<Dev> {
        let s = &self.c.devices.sw[i];
        if s.ctrl.is_some() {
            return None;
        }
        let (cp, cn) = (s.cp, s.cn);
        let (inner, outer) = if self.rail[cn as usize] { (cp, cn) } else if self.rail[cp as usize] { (cn, cp) } else { return None };
        if self.rail[inner as usize] {
            return None;
        }
        let mut source = None;
        for &(dev, t) in &self.on[inner as usize] {
            match (dev, t) {
                (Dev::Sw(_), Term::Control) => {}
                (Dev::V(v), _) => {
                    let x = &self.c.devices.v[v];
                    let across = (x.p == inner && x.n == outer) || (x.n == inner && x.p == outer);
                    if !across || source.is_some() {
                        return None;
                    }
                    source = Some(dev);
                }
                _ => return None,
            }
        }
        source
    }

    /// The node touches only these devices.
    fn only(&self, x: u32, devs: &[Dev]) -> bool {
        self.on[x as usize].iter().all(|(d, _)| devs.contains(d))
    }

    /// An indicator branch from `n` through `first` (a resistor or diode): the chain's devices and
    /// its far end (a rail or another net with a logic pin), when it is one.
    fn branch(&self, n: u32, first: Dev) -> Option<(Vec<Dev>, u32)> {
        let mut devs = vec![first];
        let mut at = n;
        let mut dev = first;
        loop {
            let x = self.other(dev, at)?;
            if x == n || devs.len() > 16 {
                return None;
            }
            if self.rail[x as usize] || self.on[x as usize].iter().any(|&(d, t)| t == Term::Main && matches!(d, Dev::Gpio(g) if self.digital_pad[g])) {
                return Some((devs, x));
            }
            // an inner node: this device and one more resistor or diode, nothing else
            let rest: Vec<Dev> = self.on[x as usize].iter().map(|&(d, _)| d).filter(|&d| d != dev).collect();
            if rest.len() != 1 || !matches!(rest[0], Dev::R(_) | Dev::D(_)) || self.on[x as usize].iter().any(|&(_, t)| t != Term::Main) {
                return None;
            }
            at = x;
            dev = rest[0];
            devs.push(dev);
        }
    }

    /// A key from `n`: switch `sw` (and the diode `d` past its middle node) to another net, and
    /// the press. `via` is the device on `n`: the switch or the diode.
    fn key(&self, n: u32, via: Dev) -> Option<(usize, Option<usize>, u32, Dev)> {
        let m = self.other(via, n)?;
        match via {
            Dev::Sw(i) => {
                let press = self.press(i)?;
                // straight to a rail (a button) or to a net with a pad
                if self.rail[m as usize] || self.on[m as usize].iter().any(|&(d, t)| t == Term::Main && matches!(d, Dev::Gpio(_))) {
                    return Some((i, None, m, press));
                }
                // through a diode: the middle node has the switch and the diode only
                let rest: Vec<Dev> = self.on[m as usize].iter().map(|&(d, _)| d).filter(|&d| d != via).collect();
                let [Dev::D(k)] = rest[..] else { return None };
                if !self.only(m, &[via, Dev::D(k)]) {
                    return None;
                }
                let far = self.other(Dev::D(k), m)?;
                Some((i, Some(k), far, press))
            }
            Dev::D(k) => {
                let rest: Vec<Dev> = self.on[m as usize].iter().map(|&(d, _)| d).filter(|&d| d != via).collect();
                let [Dev::Sw(i)] = rest[..] else { return None };
                if !self.only(m, &[via, Dev::Sw(i)]) || self.c.devices.sw[i].cp == m || self.c.devices.sw[i].cn == m {
                    return None;
                }
                let press = self.press(i)?;
                let far = self.other(Dev::Sw(i), m)?;
                Some((i, Some(k), far, press))
            }
            _ => None,
        }
    }
}

/// What a candidate net leans on and holds, found by [`check`].
#[derive(Default)]
struct Found {
    timing: Option<Timing>,
    /// Other nets it needs digital.
    needs: Vec<u32>,
    keys: Vec<(usize, Option<usize>, u32, Dev)>,
    indicators: Vec<(Vec<Dev>, u32)>,
    taken: Vec<Dev>,
}

/// A pad's pull resistance: the one its firmware declares, else its mode's.
fn pull(pads: &Pads, g: &Gpio) -> Option<f64> {
    let p = match (pads.pulls.get(&g.name), g.mode) {
        (Some(&p), _) => p,
        (None, PinMode::Input(p) | PinMode::OpenDrain { pull: p, .. }) => p,
        _ => PinPull::None,
    };
    (p != PinPull::None && g.params.r_pull.is_finite() && g.params.r_pull > 0.0).then_some(g.params.r_pull)
}

/// Judges net `n` against the nets still digital (`cand`). Err: why it is analog.
fn check(b: &Board, n: u32, cand: &BTreeSet<u32>, read_interval: Option<f64>) -> Result<Found, String> {
    let mut f = Found::default();
    let (mut cap, mut pull_g) = (0.0f64, 0.0f64);
    let (mut r_drive, mut edge) = (0.0f64, f64::INFINITY);
    let mut series: Vec<(f64, u32)> = Vec::new();
    for &(dev, t) in &b.on[n as usize] {
        let why = |what: String| Err(format!("{what} ({})", b.name(dev)));
        match (dev, t) {
            (Dev::Gpio(i), Term::Main) => {
                if !b.digital_pad[i] {
                    return why("an analog pad".into());
                }
                let g = &b.c.devices.gpio[i];
                r_drive = r_drive.max(g.params.r_on_h).max(g.params.r_on_l);
                edge = edge.min(g.params.t_edge);
                if let Some(r) = pull(b.pads, g) {
                    pull_g += 1.0 / r;
                }
                f.taken.push(dev);
            }
            (Dev::Gpio(_), _) => return why("a pad's supply".into()),
            (Dev::C(_), _) => {
                let o = b.other(dev, n).expect("a capacitor's end");
                if o == n {
                    continue;
                }
                if !b.rail[o as usize] {
                    return why(format!("a capacitor to {}", b.node(o)));
                }
                cap += b.c.devices.c[match dev { Dev::C(i) => i, _ => unreachable!() }].c;
                f.taken.push(dev);
            }
            (Dev::R(i), _) => {
                let o = b.other(dev, n).expect("a resistor's end");
                let r = b.c.devices.r[i].r;
                if o == n {
                    continue;
                }
                let branch = b.branch(n, dev);
                let lit = branch.as_ref().is_some_and(|(devs, _)| devs.iter().any(|d| matches!(d, Dev::D(_))));
                if b.rail[o as usize] {
                    pull_g += 1.0 / r;
                    f.taken.push(dev);
                } else if cand.contains(&o) && !lit {
                    series.push((r, o));
                    f.needs.push(o);
                    f.taken.push(dev);
                } else if let Some((devs, far)) = branch {
                    if !b.rail[far as usize] {
                        if !cand.contains(&far) {
                            return why(format!("an indicator branch to {}, which is not digital", b.node(far)));
                        }
                        f.needs.push(far);
                    }
                    f.indicators.push((devs, far));
                } else {
                    return why(format!("a resistor to {}", b.node(o)));
                }
            }
            (Dev::D(_), _) => {
                let o = b.other(dev, n).expect("a diode's end");
                if o == n {
                    continue;
                }
                if let Some(key) = b.key(n, dev) {
                    if !b.rail[key.2 as usize] {
                        if !cand.contains(&key.2) {
                            return why(format!("a key to {}, which is not digital", b.node(key.2)));
                        }
                        f.needs.push(key.2);
                    }
                    f.keys.push(key);
                } else if let Some((devs, far)) = b.branch(n, dev) {
                    if !b.rail[far as usize] {
                        if !cand.contains(&far) {
                            return why(format!("an indicator branch to {}, which is not digital", b.node(far)));
                        }
                        f.needs.push(far);
                    }
                    f.indicators.push((devs, far));
                } else {
                    return why(format!("a diode to {}", b.node(o)));
                }
            }
            (Dev::Sw(i), Term::Main) => {
                let Some(key) = b.key(n, dev) else {
                    return why("a switch that is no key".into());
                };
                if !b.rail[key.2 as usize] {
                    if !cand.contains(&key.2) {
                        return why(format!("a key to {}, which is not digital", b.node(key.2)));
                    }
                    f.needs.push(key.2);
                }
                // the drive through a closed key: its on resistance
                let on = b.c.models.iter().find(|m| m.name == b.c.devices.sw[i].model).map_or(1.0, |m| (-SwModel::from_card(m).ln_gon).exp());
                series.push((on, key.2));
                f.keys.push(key);
            }
            (Dev::Sw(_), _) => return why("it controls a switch".into()),
            (_, Term::Control) => return why(format!("{} reads it", Board::kind(dev))),
            _ => return why(Board::kind(dev).to_string()),
        }
    }
    // a series resistor or key drives this net through its resistance from the other's pads
    for &(r, o) in &series {
        let mut far = 0.0f64;
        for &(d, t) in &b.on[o as usize] {
            if let (Dev::Gpio(g), Term::Main) = (d, t) {
                let g = &b.c.devices.gpio[g];
                far = far.max(g.params.r_on_h.max(g.params.r_on_l));
                edge = edge.min(g.params.t_edge);
                if let Some(rp) = pull(b.pads, g) {
                    pull_g += 1.0 / (r + rp);
                }
            }
        }
        r_drive = r_drive.max(r + far);
    }
    if r_drive == 0.0 {
        return Err("no pad drives it".into());
    }
    let bound = read_interval.map_or(edge, |t| 0.1 * t);
    let tau = cap * r_drive;
    if !(tau <= bound) {
        return Err(format!("an RC: {cap:.3e} F through {r_drive:.3e} ohm is {tau:.3e} s, over {bound:.3e} s"));
    }
    f.timing = Some(Timing { tau, release: (pull_g > 0.0).then(|| cap / pull_g), bound });
    Ok(f)
}

/// The rule on circuit `c`: `pads` says which GPIO pads are logic pins (every other pad is analog)
/// and the pulls their firmware declares; `read_interval`, the shortest time between a change on a
/// net and a read of it, when the caller knows it.
pub fn lower(c: &SimCircuit, pads: &Pads, read_interval: Option<f64>) -> LogicReport {
    let b = Board::new(c, pads);
    let mut cand: BTreeSet<u32> = BTreeSet::new();
    for (i, g) in c.devices.gpio.iter().enumerate() {
        if b.digital_pad[i] && !b.rail[g.pad as usize] {
            cand.insert(g.pad);
        }
    }
    // a net with no pad one resistor from a pad's net, holding only passives and keys
    let passive = |x: u32| b.on[x as usize].iter().all(|&(d, t)| matches!((d, t), (Dev::R(_) | Dev::C(_) | Dev::D(_), _) | (Dev::Sw(_), Term::Main)));
    let mut reached = Vec::new();
    for &n in &cand {
        for &(dev, _) in &b.on[n as usize] {
            let Dev::R(_) = dev else { continue };
            let Some(o) = b.other(dev, n) else { continue };
            let lit = b.branch(n, dev).is_some_and(|(devs, _)| devs.iter().any(|d| matches!(d, Dev::D(_))));
            if !lit && !b.rail[o as usize] && !cand.contains(&o) && passive(o) {
                reached.push(o);
            }
        }
    }
    cand.extend(reached);
    let first: Vec<u32> = cand.iter().copied().collect();
    let mut refused: BTreeMap<u32, String> = BTreeMap::new();
    let mut found: BTreeMap<u32, Found> = BTreeMap::new();
    loop {
        found.clear();
        let mut dropped = Vec::new();
        for &n in &cand {
            match check(&b, n, &cand, read_interval) {
                Ok(f) => {
                    found.insert(n, f);
                }
                Err(why) => dropped.push((n, why)),
            }
        }
        if dropped.is_empty() {
            break;
        }
        for (n, why) in dropped {
            cand.remove(&n);
            refused.insert(n, why);
        }
    }
    // the nets, in node order
    let mut nets: Vec<u32> = first;
    nets.sort_unstable();
    let report_nets = nets.iter().map(|&n| {
        let v = match (found.get(&n), refused.get(&n)) {
            (Some(f), _) => Verdict::Digital(f.timing.clone().expect("a digital net's timing")),
            (None, Some(why)) => Verdict::Analog(why.clone()),
            (None, None) => Verdict::Analog("not judged".into()),
        };
        (b.node(n).to_string(), v)
    }).collect();
    // matrices: the keys' nets joined
    let mut keys: BTreeMap<usize, (Option<usize>, u32, u32, Dev)> = BTreeMap::new();
    for (&n, f) in &found {
        for &(sw, d, far, press) in &f.keys {
            keys.entry(sw).or_insert((d, n, far, press));
        }
    }
    let mut group: BTreeMap<u32, u32> = BTreeMap::new();
    fn root(g: &mut BTreeMap<u32, u32>, x: u32) -> u32 {
        let p = *g.entry(x).or_insert(x);
        if p == x { x } else { let r = root(g, p); g.insert(x, r); r }
    }
    // keys join their nets into a matrix; a rail (a button's ground) joins nothing
    for &(_, a, z, _) in keys.values() {
        if b.rail[z as usize] {
            root(&mut group, a);
            continue;
        }
        let (ra, rz) = (root(&mut group, a), root(&mut group, z));
        if ra != rz {
            group.insert(ra.max(rz), ra.min(rz));
        }
    }
    let mut matrices: BTreeMap<u32, (BTreeSet<u32>, Matrix)> = BTreeMap::new();
    for (&sw, &(d, a, z, press)) in &keys {
        let r = root(&mut group, a);
        let (members, m) = matrices.entry(r).or_default();
        members.insert(a);
        if b.rail[z as usize] {
            m.to_rail += 1;
        } else {
            members.insert(z);
        }
        m.keys.push((b.name(Dev::Sw(sw)).to_string(), d.map(|k| b.name(Dev::D(k)).to_string()), b.name(press).to_string()));
    }
    let matrices: Vec<Matrix> = matrices.into_values().map(|(members, mut m)| {
        m.nets = members.iter().map(|&n| b.node(n).to_string()).collect();
        m
    }).collect();
    // indicators, each once (one between two digital nets is found from both)
    let mut seen: HashSet<Vec<Dev>> = HashSet::new();
    let mut indicators = Vec::new();
    let mut taken: BTreeSet<Dev> = BTreeSet::new();
    for (&n, f) in &found {
        taken.extend(f.taken.iter().copied());
        for (devs, far) in &f.indicators {
            let mut key = devs.clone();
            key.sort_unstable();
            if seen.insert(key) {
                indicators.push(Indicator { from: b.node(n).to_string(), to: b.node(*far).to_string(), devices: devs.iter().map(|&d| b.name(d).to_string()).collect() });
            }
            taken.extend(devs.iter().copied());
        }
    }
    for (&sw, &(d, _, _, press)) in &keys {
        taken.insert(Dev::Sw(sw));
        taken.insert(press);
        if let Some(k) = d {
            taken.insert(Dev::D(k));
        }
    }
    // the unknowns the analog side keeps: the solver's, less the digital nets', the taken devices'
    // own (a diode's inner node, a source's branch) and the nodes only taken devices touch
    let mut gone: HashSet<&str> = found.keys().map(|&n| b.node(n)).collect();
    for (x, devs) in b.on.iter().enumerate() {
        if x != 0 && !b.rail[x] && !devs.is_empty() && devs.iter().all(|(d, _)| taken.contains(d)) {
            gone.insert(b.node(x as u32));
        }
    }
    let prefixes: Vec<String> = taken.iter().map(|&d| format!("{}#", b.name(d))).collect();
    let unknowns = match crate::newton::Solver::new(c) {
        Ok(s) => {
            let kept = s.unknowns.iter().filter(|u| !gone.contains(u.name.as_str()) && !prefixes.iter().any(|p| u.name.starts_with(p.as_str()))).count();
            (s.unknowns.len(), kept)
        }
        Err(_) => (0, 0),
    };
    LogicReport {
        nets: report_nets,
        matrices,
        indicators,
        analog_pads: b.digital_pad.iter().filter(|&&d| !d).count(),
        taken: taken.iter().map(|&d| b.name(d).to_string()).collect(),
        unknowns,
    }
}

/// An indicator branch as a load on the supply it draws from (Logic mode, A-L1 section 5). The
/// branch, its pad's net and the capacitors on that net leave the circuit; the pad's GPIO stays, by
/// its name and in its place (its co-simulation row drives it as before), between that supply and a
/// node held at the branch's offset, so that in the state that lights it it draws
/// `g (dv - v_off)` with the pad's own edge, `dv` the drop across the branch and its driver. Loads
/// alike (one rail, one state, one offset) share the held node (`logic#x{k}`) and its source
/// (`logic#v{k}`): a row of LEDs costs two unknowns.
#[derive(Clone, Debug, PartialEq)]
pub struct Load {
    /// The pad's GPIO device.
    pub pad: String,
    /// The branch's devices, from the pad's net.
    pub devices: Vec<String>,
    /// It lights with the pad high (drawing from the pad's vdd into the rail it ends at), else low
    /// (from the rail it ends at into the pad's vss).
    pub high: bool,
    /// Its slope at the operating point, siemens.
    pub g: f64,
    /// Its offset, volts: the drop at which the line through the operating point draws nothing.
    pub v_off: f64,
    /// The current at the operating point, amperes.
    pub i: f64,
}

/// The branch's current from `src` (a rail at `v_src`) through the pad's on resistance `r_on`
/// and the branch to its far end `far` (at `v_far`): one operating point of the branch alone.
fn branch_current(c: &SimCircuit, devs: &[String], pad_net: &str, far: &str, r_on: f64, v_src: f64, v_far: f64) -> Result<f64, String> {
    let mut t = c.subcircuit(|name| devs.iter().any(|d| d == name));
    t.r("logic#ron", "logic#src", pad_net, r_on);
    t.v("logic#vsrc", "logic#src", "0", Source::dc(v_src));
    if far != "0" {
        t.v("logic#vfar", far, "0", Source::dc(v_far));
    }
    let mut s = crate::newton::Solver::new(&t).map_err(|e| format!("the branch alone: {e:?}"))?;
    super::op::operating_point(&mut s, false).map_err(|e| format!("the branch's operating point: {e:?}"))?;
    let k = s.unknowns.iter().position(|u| u.name == "logic#vsrc#branch").ok_or("no source branch")?;
    // a source's branch current flows into its + end: what it delivers is the negative
    Ok(-s.x[k])
}

/// The circuit with each indicator branch the report found taken as a [`Load`], where the branch's
/// net holds one write-only logic pad, capacitors to rails and branches to rails, and nothing else,
/// and the branch lights in one of the pad's states only. Every other net stays as it is.
/// The operating point is the circuit's own (the rails' voltages at rest). Cost: one operating point
/// of the circuit, four of each branch alone.
pub fn with_loads(c: &SimCircuit, pads: &Pads, report: &LogicReport) -> Result<(SimCircuit, Vec<Load>), String> {
    let b = Board::new(c, pads);
    let mut s = crate::newton::Solver::new(c).map_err(|e| format!("the circuit: {e:?}"))?;
    super::op::operating_point(&mut s, true).map_err(|e| format!("the circuit's operating point: {e:?}"))?;
    let volts: HashMap<&str, f64> = s.unknowns.iter().zip(&s.x).map(|(u, &x)| (u.name.as_str(), x)).collect();
    let v = |n: u32| if n == 0 { 0.0 } else { volts.get(b.node(n)).copied().unwrap_or(0.0) };
    let digital: HashSet<&str> = report.digital().map(|(n, _)| n).collect();
    // the indicators by the net they leave
    let mut by_net: BTreeMap<&str, Vec<&Indicator>> = BTreeMap::new();
    for i in &report.indicators {
        by_net.entry(i.from.as_str()).or_default().push(i);
    }
    let mut out = c.clone();
    let mut loads = Vec::new();
    let mut gone: HashSet<String> = HashSet::new();
    // loads alike (one rail, one state, one offset) share their held node and its source
    let mut held: HashMap<(String, bool, u64), String> = HashMap::new();
    'net: for (net, branches) in by_net {
        let Some(n) = c.find_node(net) else { continue };
        if !digital.contains(net) || branches.iter().any(|i| c.find_node(&i.to).is_none_or(|t| !b.rail[t as usize])) {
            continue;
        }
        let in_branch: HashSet<&str> = branches.iter().flat_map(|i| i.devices.iter().map(String::as_str)).collect();
        let mut pad = None;
        let mut caps = Vec::new();
        for &(dev, t) in &b.on[n as usize] {
            match (dev, t) {
                (Dev::Gpio(i), Term::Main) if pad.is_none() && pads.write_only.contains(&c.devices.gpio[i].name) => pad = Some(i),
                (Dev::C(_), _) if b.other(dev, n).is_some_and(|o| b.rail[o as usize]) => caps.push(b.name(dev).to_string()),
                _ if in_branch.contains(b.name(dev)) => {}
                _ => continue 'net,
            }
        }
        let Some(p) = pad else { continue };
        let g0 = c.devices.gpio[p].clone();
        let mut lit = Vec::new();
        for i in &branches {
            let far = c.find_node(&i.to).expect("a rail");
            let (vdd, vss, vf) = (v(g0.vdd), v(g0.vss), v(far));
            let dv = 1e-3;
            // each state's current into the branch at rest and with the drop dv larger
            let hi = branch_current(c, &i.devices, net, &i.to, g0.params.r_on_h, vdd, vf)?;
            let hi2 = branch_current(c, &i.devices, net, &i.to, g0.params.r_on_h, vdd + dv, vf)?;
            let lo = -branch_current(c, &i.devices, net, &i.to, g0.params.r_on_l, vss, vf)?;
            let lo2 = -branch_current(c, &i.devices, net, &i.to, g0.params.r_on_l, vss - dv, vf)?;
            let (high, i0, g, drop) = if hi > 0.0 && lo.abs() < 1e-6 * hi {
                (true, hi, (hi2 - hi) / dv, vdd - vf)
            } else if lo > 0.0 && hi.abs() < 1e-6 * lo {
                (false, lo, (lo2 - lo) / dv, vf - vss)
            } else {
                continue 'net;
            };
            if !(g > 0.0 && g.is_finite()) {
                continue 'net;
            }
            lit.push(Load { pad: g0.name.clone(), devices: i.devices.clone(), high, g, v_off: drop - i0 / g, i: i0 });
        }
        // one load a pad: its branches in parallel, when all end at one rail and light together
        let far = branches[0].to.as_str();
        if branches.iter().any(|i| i.to != far) || lit.iter().any(|l| l.high != lit[0].high) {
            continue;
        }
        let g: f64 = lit.iter().map(|l| l.g).sum();
        let i0: f64 = lit.iter().map(|l| l.i).sum();
        let drop = lit[0].v_off + lit[0].i / lit[0].g;
        let load = Load { pad: g0.name.clone(), devices: lit.iter().flat_map(|l| l.devices.iter().cloned()).collect(), high: lit[0].high, g, v_off: drop - i0 / g, i: i0 };
        let k = held.len();
        let x = held.entry((far.to_string(), load.high, load.v_off.to_bits())).or_insert_with(|| {
            let (x, src) = (format!("logic#x{k}"), format!("logic#v{k}"));
            if load.high {
                out.v(&src, &x, far, Source::dc(load.v_off));
            } else {
                out.v(&src, far, &x, Source::dc(load.v_off));
            }
            x
        }).clone();
        let xn = out.find_node(&x).expect("the load's node");
        let gp = &mut out.devices.gpio[p];
        gp.pad = xn;
        if load.high { gp.vss = xn } else { gp.vdd = xn }
        gp.params.r_on_h = 1.0 / g;
        gp.params.r_on_l = 1.0 / g;
        gone.extend(load.devices.iter().cloned());
        gone.extend(caps);
        loads.push(load);
    }
    Ok((out.subcircuit(|name| !gone.contains(name)), loads))
}

/// A key matrix as Logic mode takes it off the analog side (A-L1 section 4): its nets, each by
/// the pad that drives it (directly or through series resistors), and its keys between them.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyMatrix {
    pub nets: Vec<MatrixNet>,
    pub keys: Vec<MatrixKey>,
}

/// A key matrix's net: the GPIO pad that drives it, and how long it takes to rise (or fall) on
/// that pad's pull to its threshold once nothing drives it: R C ln(1 / (1 - vih)), seconds (0
/// without a pull).
#[derive(Clone, Debug, PartialEq)]
pub struct MatrixNet {
    pub pad: String,
    pub release: f64,
}

/// A key matrix's key: its switch and the source that presses it, whether it starts pressed, its
/// ends (indices into the matrix's nets) and its diode, which passes from `a` to `b` (Some(true))
/// or from `b` to `a` (Some(false)).
#[derive(Clone, Debug, PartialEq)]
pub struct MatrixKey {
    pub switch: String,
    pub press: String,
    pub pressed: bool,
    pub a: usize,
    pub b: usize,
    pub diode: Option<bool>,
}

/// Logic mode's lowering of a circuit: its analog side, and what left it.
#[derive(Clone, Debug)]
pub struct Logic {
    pub circuit: SimCircuit,
    pub loads: Vec<Load>,
    pub matrices: Vec<KeyMatrix>,
    pub cells: Vec<CellTaken>,
    /// The threshold inverters left analog: (input node, why).
    pub analog_cells: Vec<(String, String)>,
}

/// The circuit in Logic mode: its indicator loads ([`with_loads`]), then each key matrix the
/// report found whose nets are each driven by one logic pad, directly or through series
/// resistors, and hold nothing but that pad, capacitors to rails, those resistors and the keys:
/// the matrix's pads, capacitors, resistors, switches, diodes and press sources leave the circuit
/// and the matrix is returned for the logic side. A matrix with a key to a rail, a pull resistor
/// or anything else on its nets stays analog. Cost: [`with_loads`], then O(devices) a matrix.
pub fn lower_logic(c: &SimCircuit, pads: &Pads, report: &LogicReport) -> Result<Logic, String> {
    let (circuit, loads) = with_loads(c, pads, report)?;
    let (circuit, cells, analog_cells) = with_cells(&circuit);
    let b = Board::new(c, pads);
    let digital: HashSet<&str> = report.digital().map(|(n, _)| n).collect();
    let mut gone: HashSet<String> = HashSet::new();
    let mut matrices = Vec::new();
    'matrix: for m in &report.matrices {
        if m.to_rail > 0 {
            continue;
        }
        // the matrix's nets, joined into groups by series resistors between digital nets
        let mut group: HashMap<u32, usize> = HashMap::new();
        let mut members: Vec<Vec<u32>> = Vec::new();
        for name in &m.nets {
            let Some(n) = c.find_node(name) else { continue 'matrix };
            if group.contains_key(&n) {
                continue;
            }
            let g = members.len();
            let mut todo = vec![n];
            members.push(Vec::new());
            while let Some(x) = todo.pop() {
                if group.insert(x, g).is_some() {
                    continue;
                }
                members[g].push(x);
                for &(dev, _) in &b.on[x as usize] {
                    if let (Dev::R(_), Some(o)) = (dev, b.other(dev, x)) {
                        if !b.rail[o as usize] && digital.contains(b.node(o)) && !group.contains_key(&o) {
                            todo.push(o);
                        }
                    }
                }
            }
        }
        let keys: HashSet<&str> = m.keys.iter().flat_map(|(sw, d, _)| std::iter::once(sw.as_str()).chain(d.as_deref())).collect();
        let mut nets = Vec::new();
        let mut taken: Vec<String> = Vec::new();
        for g in &members {
            let (mut pad, mut cap, mut series) = (None, 0.0, 0.0);
            for &x in g {
                for &(dev, t) in &b.on[x as usize] {
                    match (dev, t) {
                        (Dev::Gpio(i), Term::Main) if b.digital_pad[i] && pad.is_none() => pad = Some(i),
                        (Dev::C(i), _) if b.other(dev, x).is_some_and(|o| b.rail[o as usize]) => cap += c.devices.c[i].c,
                        (Dev::R(i), _) if b.other(dev, x).is_some_and(|o| group.get(&o) == group.get(&x)) => series += 0.5 * c.devices.r[i].r,
                        _ if keys.contains(b.name(dev)) => continue,
                        _ => continue 'matrix,
                    }
                    taken.push(b.name(dev).to_string());
                }
            }
            let Some(p) = pad else { continue 'matrix };
            let g0 = &c.devices.gpio[p];
            if loads.iter().any(|l| l.pad == g0.name) {
                continue 'matrix;
            }
            let release = pull(pads, g0).map_or(0.0, |r| {
                let up = matches!(pads.pulls.get(&g0.name).copied().unwrap_or(match g0.mode {
                    PinMode::Input(p) | PinMode::OpenDrain { pull: p, .. } => p,
                    _ => PinPull::None,
                }), PinPull::Up);
                let level = if up { 1.0 / (1.0 - g0.params.vih) } else { 1.0 / g0.params.vil };
                (r + series) * cap * level.ln()
            });
            nets.push(MatrixNet { pad: g0.name.clone(), release });
        }
        let mut mkeys = Vec::new();
        for (sw, d, press) in &m.keys {
            let Some(i) = c.devices.sw.iter().position(|s| &s.name == sw) else { continue 'matrix };
            let s = &c.devices.sw[i];
            let (a, b_end, diode) = match d {
                None => (s.p, s.n, None),
                Some(d) => {
                    let Some(k) = c.devices.d.iter().position(|x| &x.name == d) else { continue 'matrix };
                    let dd = &c.devices.d[k];
                    // the switch and the diode share the middle node
                    let mid = if s.p == dd.a || s.p == dd.k { s.p } else { s.n };
                    let a = if s.p == mid { s.n } else { s.p };
                    let (far, toward) = if dd.a == mid { (dd.k, true) } else { (dd.a, false) };
                    (a, far, Some(toward))
                }
            };
            let (Some(&ga), Some(&gb)) = (group.get(&a), group.get(&b_end)) else { continue 'matrix };
            mkeys.push(MatrixKey { switch: sw.clone(), press: press.clone(), pressed: s.on, a: ga, b: gb, diode });
            taken.push(sw.clone());
            taken.push(press.clone());
            taken.extend(d.iter().cloned());
        }
        gone.extend(taken);
        matrices.push(KeyMatrix { nets, keys: mkeys });
    }
    Ok(Logic { circuit: circuit.subcircuit(|name| !gone.contains(name)), loads, matrices, cells, analog_cells })
}

/// A timing cell Logic mode made (`with_cells`): its name, its input's node, the devices it took
/// and its network's time constant.
#[derive(Clone, Debug, PartialEq)]
pub struct CellTaken {
    pub name: String,
    pub input: String,
    pub devices: Vec<String>,
    pub tau: f64,
    /// Its calibration: the switch pair's period alone (a driven cell's: its mean response, a
    /// drive edge to its flip), the cell's with the delay, the delay a flip.
    pub period: f64,
    pub cell_period: f64,
    pub delay: f64,
}

/// The circuit with each threshold inverter whose input network qualifies run as a timing cell
/// (`dev::cell`, A-L1 section 3), the cells it made, and the inverters it left analog with why.
/// - A threshold inverter is a pair of voltage-controlled switches, `down` from an output to a
///   node its control reads against (vss), `up` from another node to that output, reading vss
///   against the same input, their models mirrored (thresholds `vt` and `-vt`, the same
///   hysteresis and band): a Schmitt section as the bench lowers one.
/// - Its input qualifies when it is no rail and no pad, and everything else on it is a capacitor
///   to a rail or a resistor to any node the solve keeps (its own output, an oscillator; another
///   inverter's output, a pad or a pull-up's node, a delay or a debounce), not another inverter's
///   input. A resistor that goes on through nodes only resistors touch, one way on each (a pot's
///   wiper on a series resistor), is one resistor to where it ends: those nodes leave the solve.
///   One node with capacitance: a network of several stays analog.
/// - The cell starts where its pair does at the circuit's operating point: its input's voltage (an
///   initial condition held), its output where the up switch stands.
/// - The guard: each cell is calibrated against its switch pair ([`calibrate`]); one that fails
///   stays analog with why.
/// Cost: O(devices) a pair; with any pair, one operating point of the circuit and two to four short
/// transients of each candidate alone.
pub fn with_cells(c: &SimCircuit) -> (SimCircuit, Vec<CellTaken>, Vec<(String, String)>) {
    let d = &c.devices;
    let rail: Vec<bool> = c.nodes.iter().enumerate().map(|(i, n)| i == 0 || n.supply).collect();
    let pad: HashSet<u32> = d.gpio.iter().map(|g| g.pad).collect();
    let model = |name: &str| c.find_model(name).filter(|m| m.kind == crate::circuit::ModelKind::Sw).map(SwModel::from_card);
    // (down, up) by index
    let mut pairs = Vec::new();
    for (i, dn) in d.sw.iter().enumerate() {
        if dn.ctrl.is_some() || dn.n != dn.cn || dn.cp == dn.cn {
            continue;
        }
        let Some(md) = model(&dn.model) else { continue };
        let up = d.sw.iter().position(|u| {
            u.ctrl.is_none() && u.n == dn.p && u.cp == dn.cn && u.cn == dn.cp && u.p != dn.p
                && model(&u.model).is_some_and(|mu| mu.vt == -md.vt && mu.vh == md.vh && mu.w == md.w)
        });
        if let Some(j) = up {
            pairs.push((i, j));
        }
    }
    let inputs: HashSet<u32> = pairs.iter().map(|&(i, _)| d.sw[i].cp).collect();
    // the circuit's operating point: a cell starts where its pair does (its input's voltage, the
    // up switch's position), and the nodes around it are held there while it is calibrated
    let (volts, closed): (HashMap<String, f64>, HashMap<String, bool>) = if pairs.is_empty() {
        (HashMap::new(), HashMap::new())
    } else {
        let mut s = match crate::newton::Solver::new(c) {
            Ok(s) => s,
            Err(e) => return (c.clone(), Vec::new(), vec![("*".into(), format!("the circuit: {e:?}"))]),
        };
        if let Err(e) = super::op::operating_point(&mut s, true) {
            return (c.clone(), Vec::new(), vec![("*".into(), format!("the circuit's operating point: {e:?}"))]);
        }
        let closed = pairs.iter().flat_map(|&(i, j)| [i, j]).filter_map(|k| {
            let name = &d.sw[k].name;
            s.find_device(name).and_then(|(b, n)| s.banks[b].state(n).first().map(|&on| (name.clone(), on != 0.0)))
        });
        (s.unknowns.iter().zip(&s.x).map(|(u, &x)| (u.name.clone(), x)).collect(), closed.collect())
    };
    // every reference to a node, to tell whether the input holds anything else
    let mut refs = vec![0usize; c.nodes.len()];
    d.for_each_node(|n| refs[n as usize] += 1);
    let mut circuit = c.clone();
    let (mut cells, mut analog, mut gone) = (Vec::new(), Vec::new(), HashSet::new());
    for &(i, j) in &pairs {
        let (dn, up) = (&d.sw[i], &d.sw[j]);
        let input = dn.cp;
        let name = c.nodes[input as usize].name.clone();
        let refuse = |why: String| (name.clone(), why);
        if rail[input as usize] || pad.contains(&input) {
            analog.push(refuse("its input is a rail or a pad".into()));
            continue;
        }
        let (mut caps, mut res, mut seen, mut parts, mut inner) = (Vec::new(), Vec::new(), 2, Vec::new(), Vec::new());
        let mut why = None;
        // a resistor may reach any node the solve keeps (the cell holds it at the step's end and
        // loads it), not another section's input, which its own cell would take out; one that
        // goes on through nodes only resistors touch (a pot's wiper on a series resistor) reaches
        // where that chain ends
        for r in d.r.iter().filter(|r| r.a == input || r.b == input) {
            let (other, ohms, more, passed) = chain(c, &refs, &rail, &pad, input, r);
            if other == input || inputs.contains(&other) {
                why = Some(format!("{} reaches {}, another section's input", r.name, c.nodes[other as usize].name));
                break;
            }
            let entry = res.len() as u32;
            parts.push((r.name.clone(), entry, r.r, r.a == input));
            parts.extend(more.into_iter().map(|(name, ohms, inward)| (name, if inward.is_some() { entry } else { u32::MAX }, ohms, inward == Some(true))));
            // the k-th node passed has the first resistor and k more between it and the input
            inner.extend(passed.into_iter().zip(1..).map(|(node, between)| (node, entry, between)));
            res.push((r.name.clone(), other, ohms));
            seen += 1;
        }
        for k in d.c.iter().filter(|k| k.a == input || k.b == input) {
            let other = if k.a == input { k.b } else { k.a };
            if other == input || !rail[other as usize] {
                why = Some(format!("{} reaches {}, which is no rail", k.name, c.nodes[other as usize].name));
                break;
            }
            caps.push((k.name.clone(), k.c, k.a == input));
            seen += 1;
        }
        if why.is_none() && refs[input as usize] != seen {
            why = Some("something besides resistors and capacitors is on its input".into());
        }
        if why.is_none() && (res.is_empty() || caps.iter().all(|k| k.1 <= 0.0)) {
            why = Some("its input has no resistor or no capacitance".into());
        }
        if let Some(why) = why {
            analog.push(refuse(why));
            continue;
        }
        let g: f64 = res.iter().map(|r| 1.0 / r.2).sum();
        let cap: f64 = caps.iter().map(|k| k.1).sum();
        let mut devices = vec![dn.name.clone(), up.name.clone()];
        devices.extend(caps.iter().map(|k| k.0.clone()));
        devices.extend(parts.iter().map(|p| p.0.clone()));
        let mut cell = Cell {
            name: format!("{}.cell", dn.name), out: dn.p, vdd: up.p, vss: dn.n, inp: input, input: name.clone(),
            down: (dn.name.clone(), dn.model.clone()), up: (up.name.clone(), up.model.clone()),
            caps, res, parts, inner, v0: volts.get(&name).copied().unwrap_or(0.0), high: closed.get(&up.name).copied().unwrap_or(up.on), delay: 0.0,
        };
        let (period, cell_period, delay) = match calibrate(c, &devices, &cell, input, &volts) {
            Ok(k) => k,
            Err(why) => {
                analog.push(refuse(format!("its calibration: {why}")));
                continue;
            }
        };
        cell.delay = delay;
        cells.push(CellTaken { name: cell.name.clone(), input: name, devices: devices.clone(), tau: cap / g, period, cell_period, delay });
        gone.extend(devices);
        circuit.devices.cell.push(cell);
    }
    (circuit.subcircuit(|n| !gone.contains(n)), cells, analog)
}

/// Where resistor `first`, on node `from`, leads: through each node only resistors touch (no rail,
/// no pad) with one other way on (a resistor with both ends there carries nothing and comes along),
/// to the first node that is anything else. Returns that node, the chain's ohms, its resistors
/// after `first` with their ohms and, for each that carries its current, whether its `a` end is
/// the one nearer `from` (None: it carries nothing), and the inner nodes passed, in order. Cost:
/// O(resistors) a node passed.
type Chain = (u32, f64, Vec<(String, f64, Option<bool>)>, Vec<u32>);
fn chain(c: &SimCircuit, refs: &[usize], rail: &[bool], pad: &HashSet<u32>, from: u32, first: &crate::circuit::Res) -> Chain {
    let d = &c.devices;
    let (mut at, mut ohms, mut via) = (if first.a == from { first.b } else { first.a }, first.r, first);
    let (mut more, mut passed) = (Vec::new(), vec![from]);
    while !rail[at as usize] && !pad.contains(&at) && !passed.contains(&at) {
        let on: Vec<&crate::circuit::Res> = d.r.iter().filter(|r| r.a == at || r.b == at).collect();
        let ends: usize = on.iter().map(|r| usize::from(r.a == at) + usize::from(r.b == at)).sum();
        let ways: Vec<&crate::circuit::Res> = on.iter().copied().filter(|r| r.a != r.b && r.name != via.name).collect();
        let [next] = ways[..] else { break };
        if ends != refs[at as usize] {
            break;
        }
        more.extend(on.iter().filter(|r| r.a == r.b).map(|r| (r.name.clone(), r.r, None)));
        more.push((next.name.clone(), next.r, Some(next.a == at)));
        passed.push(at);
        ohms += next.r;
        at = if next.a == at { next.b } else { next.a };
        via = next;
    }
    passed.remove(0);
    (at, ohms, more, passed)
}

/// A timing cell's calibration (A-L1 section 3's guard): its switch pair and input network alone,
/// with the capacitors and resistors on its output, every other node they reach held at the
/// circuit's operating point (`volts`), run at transistor level and as the cell at a tight
/// tolerance, and the delay a flip that closes the gap between them (none when the cell is within
/// 1e-5 already). Returns the pair's time, the cell's after the delay, and the delay.
/// - An oscillator (its output reaches its input) is timed over a dozen periods: its period.
/// - A driven cell (an RC from other nodes: a debounce, a delay) has its outside nodes that are no
///   rail driven together by a square wave from its low rail to its high one, slow enough for the
///   input to settle, and each output edge is timed from the drive's edge before it: its mean
///   response.
///
/// Fails when either does not flip a few times, or when after the delay the period is off by
/// more than 0.1 %, or any response by more than 0.1 % of the pair's mean (a delay a flip cannot
/// make the cell what the pair is: a soft band crossed at different slopes). Cost: two to four
/// transients of a few nodes, a few thousand steps each.
pub fn calibrate(c: &SimCircuit, devices: &[String], cell: &Cell, input: u32, volts: &HashMap<String, f64>) -> Result<(f64, f64, f64), String> {
    let node = |n: u32| c.nodes[n as usize].name.clone();
    let v = |n: u32| if n == 0 { 0.0 } else { volts.get(&node(n)).copied().unwrap_or(0.0) };
    let rail = |n: u32| n == 0 || c.nodes[n as usize].supply;
    let g_out: f64 = cell.res.iter().filter(|r| r.1 == cell.out).map(|r| 1.0 / r.2).sum();
    let m = c.find_model(&cell.down.1).filter(|m| m.kind == crate::circuit::ModelKind::Sw).map(SwModel::from_card).ok_or("no switch model")?;
    let (rise, fall) = crate::dev::cell::commits(&m);
    if !(rise > fall) {
        return Err(format!("its switches have no hysteresis left: their band ({} V) is twice their hysteresis ({} V)", m.w, m.vh));
    }
    let (rise, fall) = (v(cell.vss) + rise, v(cell.vss) + fall);
    let g: f64 = cell.res.iter().map(|r| 1.0 / r.2).sum();
    let cap: f64 = cell.caps.iter().map(|k| k.1).sum();
    let tau = cap / g;
    // a driven cell's drive: every outside node that is no rail, from the low rail to the high
    let mut driven: Vec<String> = if g_out == 0.0 { cell.res.iter().filter(|r| !rail(r.1)).map(|r| node(r.1)).collect() } else { Vec::new() };
    driven.sort();
    driven.dedup();
    let half = 12.0 * tau;
    let drive = Source::wave(crate::circuit::Wave::Pulse { v1: v(cell.vss), v2: v(cell.vdd), td: half, tr: 1e-6 * tau, tf: 1e-6 * tau, pw: half, per: 2.0 * half });
    // the output's own capacitors and resistors come along
    let mut keep: HashSet<String> = devices.iter().cloned().collect();
    let d = &c.devices;
    keep.extend(d.r.iter().filter(|r| (r.a == cell.out || r.b == cell.out) && !keep.contains(&r.name)).map(|r| r.name.clone()).collect::<Vec<_>>());
    keep.extend(d.c.iter().filter(|k| (k.a == cell.out || k.b == cell.out) && !keep.contains(&k.name)).map(|k| k.name.clone()).collect::<Vec<_>>());
    let (out, mid) = (node(cell.out), 0.5 * (v(cell.vdd) + v(cell.vss)));
    // the nodes only the cell's devices touch (its input, a resistor chain's inner nodes) move with
    // it: none is held
    let refs = |t: &SimCircuit| {
        let mut k: HashMap<String, usize> = HashMap::new();
        t.devices.for_each_node(|n| *k.entry(t.nodes[n as usize].name.clone()).or_default() += 1);
        k
    };
    let all = refs(c);
    let own: HashSet<String> = refs(&c.subcircuit(|n| devices.iter().any(|dev| dev == n))).into_iter()
        .filter(|(n, k)| all.get(n) == Some(k)).map(|(n, _)| n).collect();
    let alone = |with: Option<Cell>| -> SimCircuit {
        let mut t = c.clone();
        let mut keep = keep.clone();
        if let Some(cell) = with {
            for dev in devices {
                keep.remove(dev);
            }
            keep.insert(cell.name.clone());
            t.devices.cell.push(cell);
        }
        let mut t = t.subcircuit(|n| keep.contains(n));
        // tight, so the runs measure the cell's model and not the stepping's error
        t.opts.reltol = 1e-6;
        let input = node(input);
        let held: Vec<String> = t.nodes.iter().skip(1).map(|n| n.name.clone()).filter(|n| *n != out && *n != input && !own.contains(n)).collect();
        for (k, name) in held.iter().enumerate() {
            let src = if driven.contains(name) { drive.clone() } else { Source::dc(volts.get(name).copied().unwrap_or(0.0)) };
            t.v(&format!("cal#v{k}"), name, "0", src);
        }
        t
    };
    // the cell starts where the pair stands at the operating point of the run alone (a driven
    // cell's drive starts low, wherever the circuit's own operating point put its input)
    let cell = {
        let t = alone(None);
        let mut s = crate::newton::Solver::new(&t).map_err(|e| format!("the switch pair alone: {}", e.message))?;
        super::op::operating_point(&mut s, true).map_err(|e| format!("the switch pair alone at its operating point: {}", e.message))?;
        let v0 = s.unknown(&node(input)).map_or(cell.v0, |u| s.x[u as usize]);
        let high = s.find_device(&cell.up.0).and_then(|(b, i)| s.banks[b].state(i).first().map(|&on| on != 0.0)).unwrap_or(cell.high);
        Cell { v0, high, ..cell.clone() }
    };
    let cell = &cell;
    // each output edge at the point the output leaves its level from: the switch pair commits and
    // the cell flips on a point, and the output moves in the step after it (an interpolated
    // crossing would carry that step's length)
    let edges = |what: &str, t: &SimCircuit, span: f64, max_step: f64, rising: bool| -> Result<Vec<f64>, String> {
        let tran = super::Analysis::Tran(super::TranSpec { tstop: span, max_step: Some(max_step), uic: false });
        let mut record = super::Record::default();
        super::run(t, &tran, &mut record, &super::Control::default()).map_err(|e| format!("{what} alone: {}", e.message))?;
        let o = record.v(&out);
        let at: Vec<f64> = (1..record.at.len()).filter(|&k| if rising { o[k - 1] < mid && o[k] >= mid } else { (o[k - 1] < mid) != (o[k] < mid) }).map(|k| record.at[k - 1]).collect();
        if at.len() < 5 {
            let (lo, hi) = o.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &v| (l.min(v), h.max(v)));
            return Err(format!("{what} alone: {} edges through {mid} V in {span:.3e} s, the output from {lo} V to {hi} V", at.len()));
        }
        Ok(at)
    };
    if g_out == 0.0 {
        if driven.is_empty() {
            return Err("its input does not move: every resistor on it goes to a rail".into());
        }
        let rest: f64 = cell.res.iter().filter(|r| rail(r.1)).map(|r| v(r.1) / r.2).sum();
        let g_d = g - cell.res.iter().filter(|r| rail(r.1)).map(|r| 1.0 / r.2).sum::<f64>();
        let (hi, lo) = ((rest + g_d * v(cell.vdd)) / g, (rest + g_d * v(cell.vss)) / g);
        if !(hi > rise && lo < fall) {
            return Err(format!("its input does not cross its thresholds when driven rail to rail ({} V to {} V at the operating point)", v(cell.vss), v(cell.vdd)));
        }
        // three periods of the drive; each edge from the drive's edge before it
        let responses = |what: &str, t: &SimCircuit| -> Result<Vec<f64>, String> {
            Ok(edges(what, t, 7.0 * half, tau / 200.0, false)?.into_iter().map(|e| e - (e / half).floor() * half).collect())
        };
        let pair = responses("the switch pair", &alone(None))?;
        let first = responses("the cell", &alone(Some(cell.clone())))?;
        if pair.len() != first.len() {
            return Err(format!("its cell flips {} times where its pair flips {}", first.len(), pair.len()));
        }
        let n = pair.len() as f64;
        let mean = pair.iter().sum::<f64>() / n;
        let delay = pair.iter().zip(&first).map(|(p, q)| p - q).sum::<f64>() / n;
        // a driven cell's output does not reach its input: a delay moves each flip and nothing else
        let delay = if delay.abs() <= 1e-5 * mean { 0.0 } else { delay };
        let off = pair.iter().zip(&first).map(|(p, q)| (p - q - delay).abs()).fold(0.0, f64::max);
        if off > 1e-3 * mean {
            return Err(format!("its flips are up to {off:.3e} s off its pair's after a delay of {delay:.3e} s a flip (the pair's response {mean:.3e} s)"));
        }
        return Ok((mean, first.iter().sum::<f64>() / n + delay, delay));
    }
    // the period by hand, from the switches' thresholds and the rails at the operating point
    let rest: f64 = cell.res.iter().filter(|r| r.1 != cell.out).map(|r| v(r.1) / r.2).sum();
    let (hi, lo) = ((rest + g_out * v(cell.vdd)) / g, (rest + g_out * v(cell.vss)) / g);
    let (a, b) = ((hi - fall) / (hi - rise), (lo - rise) / (lo - fall));
    if !(a > 1.0 && b > 1.0) {
        return Err(format!("its input does not swing between its thresholds (rails {} V to {} V at the operating point)", v(cell.vss), v(cell.vdd)));
    }
    let guess = tau * (a.ln() + b.ln());
    // eight periods by hand; the mean period over the rising edges from the third
    let period = |what: &str, t: &SimCircuit| -> Result<f64, String> {
        let ups = edges(what, t, 8.0 * guess, guess / 500.0, true)?;
        Ok((ups[ups.len() - 1] - ups[2]) / (ups.len() - 3) as f64)
    };
    let pair = period("the switch pair", &alone(None))?;
    let first = period("the cell", &alone(Some(cell.clone())))?;
    // within the runs' own error the cell needs no delay; else a secant on the delay a flip, from
    // half the difference (a flip's delay also moves the next half period's start)
    if (first / pair - 1.0).abs() <= 1e-5 {
        return Ok((pair, first, 0.0));
    }
    let d1 = 0.5 * (pair - first);
    let p1 = period("the cell with a delay", &alone(Some(Cell { delay: d1, ..cell.clone() })))?;
    let slope = (p1 - first) / d1;
    let delay = if slope.is_finite() && slope > 0.0 { (pair - first) / slope } else { d1 };
    let corrected = if delay == d1 { p1 } else { period("the cell with its delay", &alone(Some(Cell { delay, ..cell.clone() })))? };
    if (corrected / pair - 1.0).abs() > 1e-3 {
        return Err(format!("its period {corrected:.6e} s against the pair's {pair:.6e} s after a delay of {delay:.3e} s a flip"));
    }
    Ok((pair, corrected, delay))
}


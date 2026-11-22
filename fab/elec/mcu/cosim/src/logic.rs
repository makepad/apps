//! Zero-delay digital parts between a machine and its circuit-facing pins.

use fab_elec_mcu_emu::board::{Audio, BusEvent, NetInfo, NetKind};
use fab_elec_mcu_emu::{
    ChipState, Drive, DriveEdge, LinkOut, Machine, Note, PinUse, Pull, Ran, SampleAt, Status, Tier, Until,
};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicKind {
    /// Eight shift stages and an output latch. Pins: SER, SRCLK, RCLK, OE (low enables),
    /// SRCLR (low clears stages), QA..QH, QHS. QHS always drives stage H, independent of OE.
    ShiftLatch8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogicPin {
    /// Index into the plan's nets, for either an input or the net's declared output driver.
    Net(u32),
    Tied(bool),
    /// A part output exposed to the circuit under this name.
    Out(String),
    /// Disconnected; an input reads low.
    Open,
}

#[derive(Clone, Debug)]
pub struct LogicPart {
    pub name: String,
    pub kind: LogicKind,
    pub pins: Vec<LogicPin>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetDriver {
    Machine(String),
    Part { part: u32, pin: u8 },
}

#[derive(Clone, Debug)]
pub struct LogicNet {
    pub name: String,
    pub driver: NetDriver,
}

/// A key between two of the plan's nets (a switch, alone or with its diode): pressed, it joins
/// them; a diode passes only from its anode's net to its cathode's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicKey {
    /// The name a host presses it by (`KeyPress`).
    pub name: String,
    pub a: u32,
    pub b: u32,
    /// Its diode: Some(true) passes from `a` to `b` (its anode on `a`'s side), Some(false) from
    /// `b` to `a`.
    pub diode: Option<bool>,
    pub pressed: bool,
}

/// A host's press or release of a plan key (`Machine::host_input`), by its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPress {
    pub key: String,
    pub down: bool,
}

#[derive(Clone, Debug, Default)]
pub struct LogicPlan {
    pub parts: Vec<LogicPart>,
    pub nets: Vec<LogicNet>,
    /// Keys between nets (a key matrix). A net a key touches takes its level by strength: low
    /// when a driver holding it low reaches it through pressed keys and diodes that pass that
    /// way, else high when one holding it high does, else its own driver's pull.
    pub keys: Vec<LogicKey>,
    /// Per net a key touches, the ticks it takes to rise or fall on its pull once no driver holds
    /// it (its R C to the reading threshold); a net not listed moves at once.
    pub release: Vec<(u32, u64)>,
}

fn intern(name: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES.get_or_init(|| Mutex::new(HashSet::new())).lock().unwrap();
    if let Some(&name) = names.get(name) {
        return name;
    }
    let name: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(name);
    name
}

fn level(drive: Drive, was: bool) -> bool {
    match drive {
        Drive::High => true,
        Drive::Low | Drive::OpenDrain { low: true, .. } => false,
        Drive::Input(pull) | Drive::OpenDrain { low: false, pull } => match pull {
            Pull::Up => true,
            Pull::Down => false,
            Pull::None => was,
        },
    }
}

/// Each keyed net's level by strength (`LogicPlan::keys`), and whether it is its own pull's (a
/// release, which takes the net's release ticks): low where a driver holding a net low reaches it
/// through pressed keys and diodes that pass toward that driver, else high where one holding it
/// high reaches it through keys and diodes that pass away from it, else its driver's pull, else
/// as it was. O(keys x keyed nets) at worst; a key matrix's nets are few.
fn resolve(keys: &[LogicKey], pressed: &[bool], held: &[Drive], keyed: &[bool], levels: &[bool]) -> Vec<(usize, bool, bool)> {
    let n = held.len();
    let strong = |net: usize, high: bool| match held[net] {
        Drive::High => high,
        Drive::Low | Drive::OpenDrain { low: true, .. } => !high,
        _ => false,
    };
    // reach[net]: a driver holding that level reaches it
    let reach = |high: bool| {
        let mut on: Vec<bool> = (0..n).map(|net| keyed[net] && strong(net, high)).collect();
        let mut moved = true;
        while moved {
            moved = false;
            for (k, key) in keys.iter().enumerate() {
                if !pressed[k] {
                    continue;
                }
                let (a, b) = (key.a as usize, key.b as usize);
                // current flows from the higher net to the lower: a low pulls the side it flows
                // from, a high the side it flows to
                let a_to_b = key.diode != Some(false);
                let b_to_a = key.diode != Some(true);
                let (from_b, from_a) = if high { (b_to_a, a_to_b) } else { (a_to_b, b_to_a) };
                if on[b] && !on[a] && from_b {
                    on[a] = true;
                    moved = true;
                }
                if on[a] && !on[b] && from_a {
                    on[b] = true;
                    moved = true;
                }
            }
        }
        on
    };
    let (low, high) = (reach(false), reach(true));
    (0..n).filter(|&net| keyed[net]).map(|net| {
        if low[net] {
            (net, false, false)
        } else if high[net] {
            (net, true, false)
        } else {
            match held[net] {
                Drive::Input(Pull::Up) | Drive::OpenDrain { low: false, pull: Pull::Up } => (net, true, true),
                Drive::Input(Pull::Down) | Drive::OpenDrain { low: false, pull: Pull::Down } => (net, false, true),
                _ => (net, levels[net], true),
            }
        }
    }).collect()
}

#[derive(Clone, Copy)]
enum Output {
    Net(usize),
    Pin(u16),
    Open,
}

#[derive(Clone, Copy)]
struct Feed {
    part: usize,
    /// All inputs of this part connected to the same net change together.
    mask: u8,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct PartState {
    inputs: u8,
    stages: u8,
    latch: u8,
}

impl PartState {
    fn drive(self, output: usize) -> Drive {
        let high = if output == 8 {
            self.stages & 0x80 != 0
        } else if self.inputs & (1 << 3) != 0 {
            return Drive::Input(Pull::None);
        } else {
            self.latch & (1 << output) != 0
        };
        if high { Drive::High } else { Drive::Low }
    }

    fn change(&mut self, mask: u8, high: bool) {
        let before = self.inputs;
        self.inputs = (before & !mask) | if high { mask } else { 0 };
        let rising = self.inputs & !before;
        // Coincident clocks sample the stages before this edge's shift.
        if rising & (1 << 2) != 0 {
            self.latch = self.stages;
        }
        if self.inputs & (1 << 4) == 0 {
            self.stages = 0;
        } else if rising & (1 << 1) != 0 {
            self.stages = (self.stages << 1) | (self.inputs & 1);
        }
    }
}

#[derive(Clone)]
struct State {
    parts: Vec<PartState>,
    levels: Vec<bool>,
    drives: Vec<Drive>,
    edges: Vec<DriveEdge>,
    samples: Vec<SampleAt>,
    generation: u64,
    /// The digital nets' untaken level changes (`take_bus`), in board time.
    bus: Vec<BusEvent>,
    /// Board ticks before the inner chip's last reset: board time is `base + inner.now()`.
    base: u64,
    /// Each key's state.
    pressed: Vec<bool>,
    /// Each net's driver's drive, for the nets keys touch.
    held: Vec<Drive>,
    /// A key net's rise or fall on its pull, due at a tick (inner time): (tick, net).
    due: Vec<(u64, usize)>,
}

impl State {
    fn same_state(&self, other: &Self, marked: bool) -> Option<&'static str> {
        macro_rules! compare_fields {
            ($($field:ident),+) => {
                let Self { $($field),+ } = self;
                $(if *$field != other.$field {
                    return Some(if marked { concat!("logic.saved.", stringify!($field)) }
                        else { concat!("logic.", stringify!($field)) });
                })+
            };
        }
        compare_fields!(parts, levels, drives, edges, samples, generation, bus, base, pressed, held, due);
        None
    }

    fn copy_from(&mut self, other: &Self) {
        self.parts.clone_from(&other.parts);
        self.levels.clone_from(&other.levels);
        self.drives.clone_from(&other.drives);
        self.edges.clone_from(&other.edges);
        self.samples.clone_from(&other.samples);
        self.generation = other.generation;
        self.bus.clone_from(&other.bus);
        self.base = other.base;
        self.pressed.clone_from(&other.pressed);
        self.held.clone_from(&other.held);
        self.due.clone_from(&other.due);
    }
}

/// A machine with digital nets hidden from the analog circuit. Stages and latches start at zero;
/// wiring attaches to the inner machine's current drives without manufacturing clock edges.
/// A chip reset preserves the other parts, propagates reset drives, and discards untaken edges
/// as required by `Machine`; the adapter reads the resulting drives at the reset instant.
///
/// The digital nets are the machine's board nets (`Machine::nets`, after the inner machine's own):
/// each level change is a `BusEvent::Level` at its board tick, and every net's level at tick 0
/// starts the log, so a view of the nets needs nothing from the circuit. Board time runs on across
/// a chip reset. The log rewinds with the machine and is taken before a mark, as `take_bus` says.
#[derive(Clone)]
pub struct LogicMachine<M: Machine> {
    inner: M,
    pins: Vec<&'static str>,
    /// Visible inner pins, in inner order (the prefix of `pins`).
    exposed: Vec<usize>,
    inner_pins: Vec<Option<u16>>,
    inner_nets: Vec<Vec<usize>>,
    feeds: Vec<Vec<Feed>>,
    outputs: Vec<[Output; 9]>,
    state: State,
    saved: Option<State>,
    edges: Vec<DriveEdge>,
    samples: Vec<SampleAt>,
    pending: Vec<(usize, Drive)>,
    /// The inner machine's board nets, then the plan's digital nets.
    info: Vec<NetInfo>,
    /// The first digital net's index in `info`.
    first_net: usize,
    /// The plan's keys and, per net, whether a key touches it, the machine pin that drives it
    /// (it reads the net's level back) and its release ticks.
    keys: Vec<LogicKey>,
    keyed: Vec<bool>,
    reader: Vec<Option<usize>>,
    release: Vec<u64>,
}

impl<M: Machine> LogicMachine<M> {
    pub fn new(mut inner: M, plan: LogicPlan) -> Result<Self, String> {
        let mut inner_nets = vec![Vec::new(); inner.pins().len()];
        let mut feeds: Vec<Vec<Feed>> = vec![Vec::new(); plan.nets.len()];
        for part in &plan.parts {
            let count = match part.kind { LogicKind::ShiftLatch8 => 14 };
            if part.pins.len() != count {
                return Err(format!("part {} needs {count} pins, got {}", part.name, part.pins.len()));
            }
        }
        for (net, wire) in plan.nets.iter().enumerate() {
            match &wire.driver {
                NetDriver::Machine(name) => {
                    let pin = inner.pins().iter().position(|p| *p == name)
                        .ok_or_else(|| format!("net {} names unknown machine pin {name}", wire.name))?;
                    inner_nets[pin].push(net);
                }
                NetDriver::Part { part, pin } => {
                    let part = plan.parts.get(*part as usize)
                        .ok_or_else(|| format!("net {} names unknown part {part}", wire.name))?;
                    if !(5..14).contains(pin) || part.pins[*pin as usize] != LogicPin::Net(net as u32) {
                        return Err(format!("net {} must be connected to its declared part output", wire.name));
                    }
                }
            }
        }
        let mut names: HashSet<&str> = inner.pins().iter().enumerate()
            .filter(|(pin, _)| inner_nets[*pin].is_empty()).map(|(_, &name)| name).collect();
        for (part_index, part) in plan.parts.iter().enumerate() {
            for (pin, connection) in part.pins.iter().enumerate() {
                match connection {
                    LogicPin::Net(net) => {
                        let net = *net as usize;
                        let wire = plan.nets.get(net)
                            .ok_or_else(|| format!("part {} pin {pin} names unknown net {net}", part.name))?;
                        if pin < 5 {
                            if let Some(feed) = feeds[net].last_mut().filter(|f| f.part == part_index) {
                                feed.mask |= 1 << pin;
                            } else {
                                feeds[net].push(Feed { part: part_index, mask: 1 << pin });
                            }
                        } else if wire.driver != (NetDriver::Part { part: part_index as u32, pin: pin as u8 }) {
                            return Err(format!("net {} has more than one driver", wire.name));
                        }
                    }
                    LogicPin::Out(name) => {
                        if pin < 5 {
                            return Err(format!("part {} input {pin} cannot be an Out pin", part.name));
                        }
                        if !names.insert(name) {
                            return Err(format!("duplicate outward pin {name}"));
                        }
                    }
                    LogicPin::Tied(_) if pin >= 5 => {
                        return Err(format!("part {} output {pin} cannot be tied", part.name));
                    }
                    LogicPin::Tied(_) | LogicPin::Open => {}
                }
            }
        }
        if names.len() > usize::from(u16::MAX) + 1 {
            return Err("outward pin indices do not fit DriveEdge::pin".into());
        }
        let mut info = inner.nets().to_vec();
        let first_net = info.len();
        info.extend(plan.nets.iter().map(|net| NetInfo { name: net.name.as_str().into(), kind: NetKind::Level }));
        if info.len() > usize::from(u16::MAX) + 1 {
            return Err("board net indices do not fit BusEvent::Level".into());
        }
        let mut pins = Vec::new();
        let mut exposed = Vec::new();
        let mut inner_pins = vec![None; inner.pins().len()];
        let mut drives = Vec::new();
        let mut levels = vec![false; plan.nets.len()];
        for (pin, &name) in inner.pins().iter().enumerate() {
            let drive = inner.drive(pin);
            if inner_nets[pin].is_empty() {
                inner_pins[pin] = Some(pins.len() as u16);
                exposed.push(pin);
                pins.push(name);
                drives.push(drive);
            } else {
                for &net in &inner_nets[pin] {
                    levels[net] = level(drive, false);
                }
            }
        }
        let mut parts = Vec::with_capacity(plan.parts.len());
        let mut outputs = Vec::with_capacity(plan.parts.len());
        for part in &plan.parts {
            let mut state = PartState::default();
            for (pin, connection) in part.pins[..5].iter().enumerate() {
                let high = match connection {
                    LogicPin::Net(net) => levels[*net as usize],
                    LogicPin::Tied(high) => *high,
                    _ => false,
                };
                state.inputs |= u8::from(high) << pin;
            }
            let mut targets = [Output::Open; 9];
            for (output, connection) in part.pins[5..].iter().enumerate() {
                targets[output] = match connection {
                    LogicPin::Net(net) => Output::Net(*net as usize),
                    LogicPin::Out(name) => {
                        let pin = pins.len() as u16;
                        pins.push(intern(name));
                        drives.push(state.drive(output));
                        Output::Pin(pin)
                    }
                    _ => Output::Open,
                };
            }
            parts.push(state);
            outputs.push(targets);
        }
        // the keys: the nets they touch, each net's reading machine pin, its release and its driver
        let n = plan.nets.len();
        let mut keyed = vec![false; n];
        for key in &plan.keys {
            for net in [key.a, key.b] {
                *keyed.get_mut(net as usize).ok_or_else(|| format!("key {} names unknown net {net}", key.name))? = true;
            }
        }
        let mut reader = vec![None; n];
        let mut held = vec![Drive::Input(Pull::None); n];
        for (pin, nets) in inner_nets.iter().enumerate() {
            for &net in nets {
                reader[net] = Some(pin);
                held[net] = inner.drive(pin);
            }
        }
        for (part, targets) in outputs.iter().enumerate() {
            for (output, target) in targets.iter().enumerate() {
                if let Output::Net(net) = *target {
                    held[net] = parts[part].drive(output);
                }
            }
        }
        let mut release = vec![0u64; n];
        for &(net, ticks) in &plan.release {
            *release.get_mut(net as usize).ok_or_else(|| format!("a release names unknown net {net}"))? = ticks;
        }
        let pressed: Vec<bool> = plan.keys.iter().map(|k| k.pressed).collect();
        for (net, high, _) in resolve(&plan.keys, &pressed, &held, &keyed, &levels) {
            levels[net] = high;
            if let Some(pin) = reader[net] {
                inner.set_input(pin, high);
            }
        }
        let mut edges = Vec::new();
        inner.take_drives(&mut edges);
        edges.clear();
        let bus = levels.iter().enumerate().map(|(net, &high)| BusEvent::Level { t: 0, net: (first_net + net) as u16, high }).collect();
        Ok(Self {
            inner, pins, exposed, inner_pins, inner_nets, feeds, outputs,
            state: State {
                parts, levels, drives, edges: Vec::new(), samples: Vec::new(), generation: 0, bus, base: 0, pressed, held, due: Vec::new(),
            },
            saved: None, edges, samples: Vec::new(), pending: Vec::new(), info, first_net,
            keys: plan.keys, keyed, reader, release,
        })
    }

    fn outward(&mut self, pin: u16, drive: Drive, tick: u64) {
        if self.state.drives[pin as usize] != drive {
            self.state.drives[pin as usize] = drive;
            self.state.generation = self.state.generation.wrapping_add(1);
            self.state.edges.push(DriveEdge { tick, pin, drive });
        }
    }

    fn inner_edge(&mut self, pin: usize, drive: Drive, tick: u64) {
        if let Some(pin) = self.inner_pins[pin] {
            self.outward(pin, drive, tick);
        } else {
            for &net in &self.inner_nets[pin] {
                self.pending.push((net, drive));
            }
            self.propagate(tick);
        }
    }

    fn propagate(&mut self, tick: u64) {
        let mut next = 0;
        loop {
            let mut rekey = false;
            while next < self.pending.len() {
                let (net, drive) = self.pending[next];
                next += 1;
                if self.keyed[net] {
                    // a keyed net takes its level from all its drivers at once, below
                    self.state.held[net] = drive;
                    rekey = true;
                    continue;
                }
                let high = level(drive, self.state.levels[net]);
                self.set_level(net, high, tick);
            }
            if !rekey {
                break;
            }
            self.rekey(tick);
        }
        self.pending.clear();
    }

    /// A net's new level: its bus event, the machine pin that reads it (a keyed net's), and its
    /// fanout. Finishes the whole fanout before propagating any resulting output: chained stages
    /// on a shared clock all sample the serial levels from before that clock edge.
    fn set_level(&mut self, net: usize, high: bool, tick: u64) {
        if high == self.state.levels[net] { return; }
        self.state.levels[net] = high;
        self.state.bus.push(BusEvent::Level { t: self.state.base + tick, net: (self.first_net + net) as u16, high });
        if self.keyed[net] {
            if let Some(pin) = self.reader[net] {
                self.inner.set_input(pin, high);
            }
        }
        for feed in 0..self.feeds[net].len() {
            let Feed { part, mask } = self.feeds[net][feed];
            let before = self.state.parts[part];
            self.state.parts[part].change(mask, high);
            let after = self.state.parts[part];
            for output in 0..9 {
                let drive = after.drive(output);
                if drive == before.drive(output) { continue; }
                match self.outputs[part][output] {
                    Output::Net(net) => self.pending.push((net, drive)),
                    Output::Pin(pin) => self.outward(pin, drive, tick),
                    Output::Open => {}
                }
            }
        }
    }

    /// The keyed nets' levels after a change of a key or a driver: a driven change at once, a
    /// release onto a pull after the net's release ticks (an earlier release still due keeps its
    /// tick; one whose net no longer moves is dropped).
    fn rekey(&mut self, tick: u64) {
        for (net, high, on_pull) in resolve(&self.keys, &self.state.pressed, &self.state.held, &self.keyed, &self.state.levels) {
            let due = self.state.due.iter().position(|&(_, n)| n == net);
            if high == self.state.levels[net] || !on_pull || self.release[net] == 0 {
                if let Some(i) = due {
                    self.state.due.remove(i);
                }
                self.set_level(net, high, tick);
            } else if due.is_none() {
                self.state.due.push((tick + self.release[net], net));
            }
        }
    }

    /// The releases due by the inner machine's now, then their fanout.
    fn fire_due(&mut self) {
        let now = self.inner.now();
        let ready: Vec<usize> = self.state.due.iter().filter(|&&(t, _)| t <= now).map(|&(_, n)| n).collect();
        if ready.is_empty() {
            return;
        }
        self.state.due.retain(|&(t, _)| t > now);
        for (net, high, _) in resolve(&self.keys, &self.state.pressed, &self.state.held, &self.keyed, &self.state.levels) {
            if ready.contains(&net) {
                self.set_level(net, high, now);
            }
        }
        self.propagate(now);
    }
}

impl<M: Machine> Machine for LogicMachine<M> {
    fn set_tier(&mut self, tier: Tier) { self.inner.set_tier(tier); }

    fn same_state(&self, other: &Self) -> Option<&'static str> {
        // Pin names and mappings, fanout, output targets and net metadata are fixed by the wiring plan.
        let Self {
            inner, pins: _, exposed: _, inner_pins: _, inner_nets: _, feeds: _, outputs: _,
            state, saved, edges, samples, pending, info: _, first_net: _, keys: _, keyed: _, reader: _, release: _,
        } = self;
        if let Some(field) = inner.same_state(&other.inner) { return Some(field); }
        if let Some(field) = state.same_state(&other.state, false) { return Some(field); }
        if *edges != other.edges { return Some("logic.edges (scratch)"); }
        if *samples != other.samples { return Some("logic.samples (scratch)"); }
        if *pending != other.pending { return Some("logic.pending"); }
        match (saved, &other.saved) {
            (Some(a), Some(b)) => a.same_state(b, true),
            (None, None) => None,
            _ => Some("logic.saved"),
        }
    }

    /// A `KeyPress` of a plan key at the machine's current tick; any other input is the inner
    /// machine's.
    fn host_input(&mut self, input: &dyn std::any::Any) -> bool {
        let Some(press) = input.downcast_ref::<KeyPress>() else { return self.inner.host_input(input) };
        let Some(k) = self.keys.iter().position(|k| k.name == press.key) else { return false };
        if self.state.pressed[k] != press.down {
            self.state.pressed[k] = press.down;
            let now = self.inner.now();
            self.rekey(now);
            self.propagate(now);
        }
        true
    }
    fn publish(&mut self, t: f64) { self.inner.publish(t); }
    fn take_feeds(&mut self) -> Option<Box<dyn std::any::Any + Send>> { self.inner.take_feeds() }
    fn put_feeds(&mut self, feeds: Box<dyn std::any::Any + Send>) { self.inner.put_feeds(feeds); }
    fn tick_hz(&self) -> u64 { self.inner.tick_hz() }
    fn now(&self) -> u64 { self.inner.now() }
    fn pc(&self) -> u32 { self.inner.pc() }

    fn run(&mut self, until: &Until) -> Ran {
        let private = Until {
            tick: until.tick, marks: until.marks, stops: until.stops,
            stops_after: until.stops_after, on_drive: true,
        };
        loop {
            // stop at the first release due before the deadline
            let due = self.state.due.iter().map(|&(t, _)| t).min().filter(|&t| t < until.tick);
            let ran = self.inner.run(&Until { tick: due.unwrap_or(until.tick), ..private });
            self.inner.take_drives(&mut self.edges);
            if let Ran::Reset { at } = ran {
                // releases counted in the old epoch's ticks go with it
                self.state.due.clear();
                // The inner reset discards its old log. Feed its new drives through the nets
                // without resetting any other chip, then expose the state at the reset instant.
                self.edges.clear();
                for pin in 0..self.inner_pins.len() {
                    self.inner_edge(pin, self.inner.drive(pin), at);
                }
                self.state.edges.clear();
                self.state.samples.clear();
                // the inner chip's ticks restart; board time runs on
                self.state.base += at;
                return ran;
            }
            let start = self.state.edges.len();
            for i in 0..self.edges.len() {
                let edge = self.edges[i];
                self.inner_edge(usize::from(edge.pin), edge.drive, edge.tick);
            }
            self.edges.clear();
            self.inner.take_samples(&mut self.samples);
            let mut sample_at = None;
            for at in self.samples.drain(..) {
                if let Some(pin) = self.inner_pins[usize::from(at.pin)] {
                    self.state.samples.push(SampleAt { tick: at.tick, pin });
                    self.state.generation = self.state.generation.wrapping_add(1);
                    sample_at.get_or_insert(at.tick);
                } else {
                    // A hidden digital net has no pad in the analog circuit.
                    self.inner.set_sample(at, f64::NAN, f64::NAN);
                }
            }
            let early = matches!(ran, Ran::Tick) && due.is_some();
            if early {
                self.fire_due();
            }
            if matches!(ran, Ran::Drive { .. }) || early {
                if until.on_drive {
                    if let Some(at) = self.state.edges.get(start).map(|e| e.tick).into_iter().chain(sample_at).min() {
                        return Ran::Drive { at };
                    }
                }
            } else {
                return ran;
            }
        }
    }

    fn mark(&mut self) {
        self.inner.mark();
        if let Some(saved) = &mut self.saved {
            saved.copy_from(&self.state);
        } else {
            self.saved = Some(self.state.clone());
        }
    }

    fn rewind(&mut self) {
        let saved = self.saved.as_ref().expect("rewind requires a mark");
        self.inner.rewind();
        self.state.copy_from(saved);
        self.edges.clear();
        self.samples.clear();
        self.pending.clear();
    }

    fn pins(&self) -> &[&'static str] { &self.pins }
    fn drive(&self, pin: usize) -> Drive { self.state.drives[pin] }
    fn drive_gen(&self) -> u64 { self.state.generation }
    fn take_drives(&mut self, out: &mut Vec<DriveEdge>) { out.append(&mut self.state.edges); }
    fn take_samples(&mut self, out: &mut Vec<SampleAt>) { out.append(&mut self.state.samples); }
    fn set_sample(&mut self, at: SampleAt, volts: f64, supply: f64) {
        if let Some(&pin) = self.exposed.get(usize::from(at.pin)) {
            self.inner.set_sample(SampleAt { tick: at.tick, pin: pin as u16 }, volts, supply);
        }
    }
    fn analog_only(&self, pin: usize) -> bool {
        self.exposed.get(pin).is_some_and(|&inner| self.inner.analog_only(inner))
    }
    fn set_input(&mut self, pin: usize, high: bool) {
        if let Some(&inner) = self.exposed.get(pin) { self.inner.set_input(inner, high); }
    }
    fn link_in(&mut self, link: u8, byte: u8) { self.inner.link_in(link, byte); }
    fn take_out(&mut self, out: &mut Vec<LinkOut>) { self.inner.take_out(out); }
    fn take_trace(&mut self, out: &mut Vec<(u64, u32)>) { self.inner.take_trace(out); }
    fn take_notes(&mut self, out: &mut Vec<Note>) { self.inner.take_notes(out); }
    fn nets(&self) -> &[NetInfo] { &self.info }
    fn take_bus(&mut self, out: &mut Vec<BusEvent>) {
        self.inner.take_bus(out);
        out.append(&mut self.state.bus);
    }
    fn take_audio(&mut self, out: &mut Audio) { self.inner.take_audio(out); }
    fn status(&self) -> Status { self.inner.status() }
    fn halt_detail(&self) -> Option<String> { self.inner.halt_detail() }
    fn regs(&self, out: &mut Vec<(&'static str, u32)>) { self.inner.regs(out); }
    fn read_data(&self, addr: u32, out: &mut [u8]) -> bool { self.inner.read_data(addr, out) }
    fn eeprom_bytes(&self) -> Option<std::sync::Arc<[u8]>> { self.inner.eeprom_bytes() }
    fn load_eeprom(&mut self, bytes: &[u8]) -> bool { self.inner.load_eeprom(bytes) }

    fn chip(&self, out: &mut ChipState) {
        self.inner.chip(out);
        // ChipState's optional pin uses are indexed by this machine's visible pins too.
        for (pin, &inner) in self.exposed.iter().enumerate() {
            if inner < out.pins.len() { out.pins[pin] = out.pins[inner]; }
        }
        out.pins.truncate(self.exposed.len());
        for pin in out.pins.len()..self.pins.len() {
            let usage = match self.drive(pin) {
                Drive::High | Drive::Low | Drive::OpenDrain { low: true, .. } => PinUse::Output,
                Drive::Input(Pull::Up) | Drive::OpenDrain { low: false, pull: Pull::Up } => PinUse::PullUp,
                _ => PinUse::Input,
            };
            out.pins.push((usage, 0.0));
        }
    }
}

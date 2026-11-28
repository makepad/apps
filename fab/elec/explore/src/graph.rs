//! The layout's own input: parts with ports and roles, nets as hyper-edges with kinds, and a block
//! tree. `layout` adapts a `Design` to it; the corpus tools feed it from drawn sheets. Nothing here
//! knows a circuit crate: keys are the caller's indices, and the caller maps results back.
//!
//! Coordinates are integer grid steps (one step is the 1.27 mm connection grid), x right, y UP, as
//! the sheet's world frame. A part's geometry is in its own frame at turn 0: the body box and each
//! port's connection point, with the side of the body the port leaves from.

use crate::layered::Side;

/// A point on the grid, x right, y up.
pub type Pt = [i32; 2];

/// What a part is for the layout: the symbol family it is drawn as and how it groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// A chip: an anchor with named pins.
    Ic,
    /// A connector or header: an anchor at the sheet's edge of its flow.
    Connector,
    Resistor,
    Capacitor,
    Inductor,
    Fuse,
    /// A diode; port 0 is the anode.
    Diode,
    /// A light-emitting diode; port 0 is the anode.
    Led,
    Switch,
    Crystal,
    Transistor,
    /// A one-pin schematic marker for an externally supplied net.
    Flag,
    /// Anything else: drawn as its symbol, placed as an anchor.
    Other,
}

impl Role {
    /// Two-terminal parts that can hang on a pin as part of a molecule.
    pub fn is_small(self) -> bool {
        matches!(self, Role::Resistor | Role::Capacitor | Role::Inductor | Role::Fuse | Role::Diode | Role::Led | Role::Switch | Role::Crystal)
    }
}

/// The electrical kind of a port, as its pin declares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PortKind {
    In,
    Out,
    Bidi,
    Passive,
    PowerIn,
    PowerOut,
    /// Open collector, open emitter, tri-state: drives, but not alone.
    WeakOut,
    NoConnect,
}

impl PortKind {
    /// The port drives its net (the source end of a flow edge).
    pub fn drives(self) -> bool {
        matches!(self, PortKind::Out | PortKind::PowerOut | PortKind::WeakOut)
    }
    /// The port is driven by its net (the load end of a flow edge).
    pub fn loads(self) -> bool {
        matches!(self, PortKind::In | PortKind::PowerIn)
    }
}

/// One port of a part: where its wire attaches.
#[derive(Clone, Debug, PartialEq)]
pub struct GPort {
    /// The pin's name as drawn (`VDD`, `PA0`, `1`).
    pub name: String,
    /// The connection point in the part's frame at turn 0.
    pub at: Pt,
    /// The side of the body the pin leaves from (its wire continues outward from here).
    pub side: Side,
    pub kind: PortKind,
    /// Marked not connected on purpose (drawn with a cross).
    pub nc: bool,
}

/// One part.
#[derive(Clone, Debug, PartialEq)]
pub struct GNode {
    /// The caller's key (a component index).
    pub key: u32,
    pub role: Role,
    /// The block that made it (an index into `LayoutGraph::blocks`).
    pub block: u32,
    /// The body's box in the part's frame at turn 0: min x, min y, max x, max y, pin ends included.
    pub body: [i32; 4],
    pub ports: Vec<GPort>,
    /// Source order: the tie-break everywhere, smaller first.
    pub order: u32,
    /// The reference designator (`U2`).
    pub name: String,
    /// The value as written (`10k`), if any.
    pub value: Option<String>,
    /// A position pinned by a hint, in the block's frame (the part's origin).
    pub pin_at: Option<Pt>,
    /// A sheet hint's quarter turns counter-clockwise.
    pub turn: Option<u8>,
    /// The sheet hint's left-right reflection before its turn. Used when `turn` is present.
    pub mirror: bool,
    /// The sheet hint's `free`: with `pin_at`, the part sits at its pin outside the layered
    /// columns and its signal pins continue by name.
    pub free: bool,
    /// The part a `near` intent puts it next to, and the pin when the intent names one: the first
    /// such intent in document order (a decoupling capacitor hangs there before connectivity guesses).
    pub near: Option<(u32, Option<u32>)>,
    /// Whether `body` and the port points are the symbol's quarter-turn fit (`adapt::sheet_unit`).
    pub quarter: bool,
    /// A box symbol's geometry for the other turn parity: its body and port points.
    pub refit: Option<Box<Refit>>,
}

/// A redrawn box symbol's body and port points (indexed like `GNode::ports`) for one turn parity.
#[derive(Clone, Debug, PartialEq)]
pub struct Refit {
    pub body: [i32; 4],
    pub at: Vec<Pt>,
}

impl GNode {
    /// Takes the symbol's geometry for the other turn parity, if it has one: a pose that turns the
    /// part an odd number of quarters from its fit draws it so.
    pub fn flip_fit(&mut self) -> bool {
        let Some(refit) = self.refit.as_deref_mut() else { return false };
        std::mem::swap(&mut self.body, &mut refit.body);
        for (port, at) in self.ports.iter_mut().zip(&mut refit.at) {
            std::mem::swap(&mut port.at, at);
        }
        self.quarter = !self.quarter;
        true
    }
}

/// The kind of a net, which decides how it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NetKind {
    Signal,
    /// A supply rail: drawn as tags pointing up where it would otherwise cross the sheet.
    Power,
    /// Ground: tags pointing down.
    Ground,
}

/// How a hint asks a net to be drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NetStyle {
    Wire,
    Label,
}

/// Electrical accounting for one rail, supplied by the circuit adapter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GRailSummary {
    pub volts: Option<(f64, f64)>,
    pub volts_conflict: bool,
    pub demand: f64,
    pub has_demand: bool,
    pub budget: Option<f64>,
    pub source_conflict: bool,
}

/// A net: a hyper-edge over part ports and block ports.
#[derive(Clone, Debug, PartialEq)]
pub struct GNet {
    /// The caller's key (a net index).
    pub key: u32,
    pub name: String,
    pub kind: NetKind,
    /// The nominal potential in volts when known (orders rails top to bottom).
    pub volts: Option<f64>,
    pub rail: Option<GRailSummary>,
    /// The part ports on it: (node index, port index).
    pub pins: Vec<(u32, u32)>,
    /// The bus it is a member of, if any (an index into `LayoutGraph::buses`).
    pub bus: Option<u32>,
    pub style: Option<NetStyle>,
}

/// A port of a block instance: where its nets cross the block's frame.
#[derive(Clone, Debug, PartialEq)]
pub struct GBlockPort {
    pub name: String,
    /// The side its definition asks for, if any.
    pub side: Option<Side>,
    pub kind: PortKind,
    pub ground: bool,
    pub volts: Option<(f64, f64)>,
    pub current: Option<f64>,
    pub open: bool,
    /// The nets bound to it (a bus port binds several), as indices into `LayoutGraph::nets`.
    pub nets: Vec<u32>,
}

/// The flow direction inside a block: where signal goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Flow {
    #[default]
    Right,
    Down,
    Left,
    Up,
}

/// A block instance. `blocks[0]` is the root (the whole circuit).
#[derive(Clone, Debug, PartialEq)]
pub struct GBlock {
    /// The caller's key (a block index).
    pub key: u32,
    pub parent: Option<u32>,
    /// The instance path (`lamps[3]`), `""` for the root.
    pub path: String,
    /// The definition's title (`LED driver`), if any.
    pub title: Option<String>,
    pub planned: bool,
    pub role: Option<String>,
    pub flow: Flow,
    pub ports: Vec<GBlockPort>,
    /// Source order among its siblings.
    pub order: u32,
    /// A position pinned by a hint: where the block's content origin lands in its parent's local
    /// frame (`sheet::to_local(sheet, parent, sheet::block_origin(sheet, block))`).
    pub pin_at: Option<Pt>,
}

/// A bus: nets drawn as one thick line where they run together.
#[derive(Clone, Debug, PartialEq)]
pub struct GBus {
    pub name: String,
    /// Member nets in member order.
    pub nets: Vec<u32>,
}

/// The whole input.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayoutGraph {
    pub nodes: Vec<GNode>,
    pub nets: Vec<GNet>,
    pub blocks: Vec<GBlock>,
    pub buses: Vec<GBus>,
    /// The part a drag is moving: pinned parts it runs into make room for it.
    pub lead: Option<u32>,
    /// The block a drag is moving (index into `blocks`), as `lead` for a part.
    pub lead_block: Option<u32>,
}

impl LayoutGraph {
    /// Terminals that carry the circuit connection independently of its power flags, in net order.
    pub(crate) fn unflagged_pins(&self, net: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.nets[net as usize].pins.iter().copied().filter(move |&(n, _)| self.nodes[n as usize].role != Role::Flag)
    }

    /// The net each port of each node is on, built once: `[node][port]`.
    pub fn port_nets(&self) -> Vec<Vec<Option<u32>>> {
        let mut out: Vec<Vec<Option<u32>>> = self.nodes.iter().map(|n| vec![None; n.ports.len()]).collect();
        for (i, net) in self.nets.iter().enumerate() {
            for &(node, port) in &net.pins {
                out[node as usize][port as usize] = Some(i as u32);
            }
        }
        out
    }

    /// The children of each block, in source order: `[block]`.
    pub fn children(&self) -> Vec<Vec<u32>> {
        let mut out = vec![Vec::new(); self.blocks.len()];
        for (i, b) in self.blocks.iter().enumerate() {
            if let Some(p) = b.parent {
                out[p as usize].push(i as u32);
            }
        }
        for kids in &mut out {
            kids.sort_by_key(|&k| (self.blocks[k as usize].order, k));
        }
        out
    }

    /// The parts each block made directly, in source order: `[block]`.
    pub fn parts_of(&self) -> Vec<Vec<u32>> {
        let mut out = vec![Vec::new(); self.blocks.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            out[n.block as usize].push(i as u32);
        }
        for parts in &mut out {
            parts.sort_by_key(|&k| (self.nodes[k as usize].order, k));
        }
        out
    }

    /// Whether `block` is `ancestor` or inside it.
    pub fn within(&self, block: u32, ancestor: u32) -> bool {
        let mut b = Some(block);
        while let Some(x) = b {
            if x == ancestor {
                return true;
            }
            b = self.blocks[x as usize].parent;
        }
        false
    }
}

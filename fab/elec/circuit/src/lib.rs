//! Fab electronics circuit model: contract C2, `fab-elec-circuit` (E1 1.2, E2.7, E2.18, E4).
//!
//! One `Circuit` per unit (a board, a cable, an off-board part): components with their pin-to-net
//! connections, nets, classes, buses, the block tree and its definitions, intents and the
//! editor's sheet hints. The circuit is code (`.ckt.splash`); this is what evaluation gives, and
//! edits to it are span edits on the text (`ops`).
//!
//! Invariants: ids are indices (`comps[i].id == CompId(i)`, `nets[i].id == NetId(i)`,
//! `classes[i].id == ClassId(i)`, `tree[i].id == BlockId(i)`, `buses[i].id == BusId(i)`,
//! `block_defs[i].id == BlockDefId(i)`); paths and net names are unique within the circuit and
//! never merge across units; `ClassId(0)` is the default class; `BlockId(0)` is the root; a pin is
//! at most once in `conns` (unless tied); a pin in `nc` is in no conn.
//!
//! Cost: plain data, nothing per frame. `pins_of` and `nets_of` are linear in connections; a
//! caller that asks per net for many nets builds `NetPins` once.

pub mod check;
pub mod erc;
pub mod ops;

use fab_elec_core::{canon_enum, canon_struct, canon_tags, Angle, BlockId, BoardId, BusId, ClassId, CompId, LayerSet, NetId, Nm, PinIx, PolySet, SchPt, Side, P};
use fab_elec_parts::{Fields, PartDef, PinKind, VRange};
use std::sync::Arc;

pub use check::{CheckKind, Measure, Violation};
pub use erc::erc;
pub use fab_elec_parts::{GpioCard, Tol};
pub use ops::{CircuitOp, End};

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Circuit {
    /// The unit this circuit belongs to.
    pub unit: BoardId,
    pub name: String,
    pub comps: Vec<Component>,
    pub nets: Vec<Net>,
    pub classes: Vec<NetClass>,
    pub buses: Vec<Bus>,
    /// The block instances; `tree[0]` is the root.
    pub tree: Vec<Block>,
    pub block_defs: Vec<BlockDef>,
    pub intents: Vec<Intent>,
    pub hints: SheetHints,
}
canon_struct!(Circuit { unit, name, comps, nets, classes, buses, tree, block_defs, intents, hints });

/// A component: one part instance.
#[derive(Clone, PartialEq, Debug)]
pub struct Component {
    pub id: CompId,
    /// The instance path (`reg.c_in`, `lamps[3].r`): the id a selection, a hint or a board
    /// placement names it by.
    pub path: String,
    pub refdes: String,
    pub ref_kind: RefKind,
    pub part: Arc<PartDef>,
    pub value: Option<Value>,
    pub conns: Vec<(PinIx, NetId)>,
    /// Pins marked not connected on purpose.
    pub nc: Vec<PinIx>,
    /// The block instance that made it.
    pub block: BlockId,
    /// Do not populate.
    pub dnp: bool,
    pub fields: Fields,
}
canon_struct!(Component { id, path, refdes, ref_kind, part, value, conns, nc, block, dnp, fields });

impl Component {
    /// The declared tolerance or the part's class default; no value or quantity means none. O(1).
    pub fn tolerance(&self) -> Option<Tol> {
        let value = self.value.as_ref()?;
        value.qty?;
        value.tol.or_else(|| self.part.kind.default_tol())
    }
}

/// Where a component's reference designator comes from. Editor add paths write `added: N`
/// so an automatic part is numbered after the counted parts, in that order, and adding it
/// renumbers no other part. An explicit `ref:` still supplies the displayed reference.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RefKind {
    /// Evaluation numbers the only component made by a call in a project document.
    #[default]
    Auto,
    /// The call supplies a reference with `ref:`.
    Stored,
    /// Evaluation keeps numbering this component because its call makes several components
    /// across the design or belongs to a read-only library.
    Shared,
}
canon_tags!(RefKind);

/// A component value: as written, and as a quantity when it is one.
#[derive(Clone, PartialEq, Debug)]
pub struct Value {
    pub text: String,
    pub qty: Option<Quantity>,
    /// The declared tolerance; `None` when the value declares none.
    pub tol: Option<Tol>,
}
// Additive: `tol` is written only when set, so a value without a tolerance keeps its hash.
impl fab_elec_core::canon::Canon for Value {
    fn canon(&self, w: &mut fab_elec_core::canon::Writer) {
        let Value { text, qty, tol } = self;
        text.canon(w);
        qty.canon(w);
        if tol.is_some() {
            tol.canon(w);
        }
    }
}

/// A physical quantity in SI units.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Quantity {
    pub si: f64,
    pub unit: QtyUnit,
}
canon_struct!(Quantity { si, unit });

/// The quantity a part of this kind is valued in: a resistor's or a pot's ohms, a capacitor's
/// farads, an inductor's henries, a crystal's hertz. Every other kind's value is a label.
pub fn value_unit(kind: &fab_elec_parts::PartKind) -> Option<QtyUnit> {
    use fab_elec_parts::PartKind;
    match kind {
        PartKind::Resistor | PartKind::Potentiometer => Some(QtyUnit::Ohm),
        PartKind::Capacitor => Some(QtyUnit::Farad),
        PartKind::Inductor => Some(QtyUnit::Henry),
        PartKind::Crystal => Some(QtyUnit::Hertz),
        _ => None,
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum QtyUnit {
    Ohm,
    Farad,
    Henry,
    Volt,
    Ampere,
    Hertz,
    Watt,
    Metre,
    Second,
    /// A plain count (layers, pins, parts): `si` is the number.
    Count,
}
canon_tags!(QtyUnit);

#[derive(Clone, PartialEq, Debug)]
pub struct Net {
    pub id: NetId,
    pub name: String,
    pub class: ClassId,
    pub volts: Option<VRange>,
    pub kind: NetKind,
    /// The block instance whose path this derived name sits under (a bound net, a pin's net,
    /// or a bus member). None when the document named it, so component or block renames do not move it.
    pub scope: Option<BlockId>,
}
canon_struct!(Net { id, name, class, volts, kind, scope });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum NetKind {
    #[default]
    Signal,
    Power,
    Ground,
}
canon_tags!(NetKind);

#[derive(Clone, PartialEq, Debug)]
pub struct NetClass {
    pub id: ClassId,
    pub name: String,
    pub rules: ClassRules,
}
canon_struct!(NetClass { id, name, rules });

/// A class's rules; `None` takes the board's base rule.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ClassRules {
    pub track: Option<Nm>,
    pub clearance: Option<Nm>,
    pub via: Option<ViaSize>,
    pub pair: Option<PairRule>,
    pub length_tol: Option<Nm>,
    /// Single-ended impedance target in ohms (widths are solved from the stackup, E8 1.3).
    pub z0: Option<f64>,
    /// Neck-down inside the courtyard of a part with an area array (BGA escape, E6 13.1 4).
    pub neck: Option<NeckRule>,
    /// The longest stub a net of the class may have.
    pub max_stub: Option<Nm>,
    /// Vias in pads allowed (the factory profile must allow it too).
    pub via_in_pad: Option<bool>,
    /// A return (ground) via within this distance of every signal via of the class (E6 6.5).
    pub return_via: Option<Nm>,
}
canon_struct!(ClassRules { track, clearance, via, pair, length_tol, z0, neck, max_stub, via_in_pad, return_via });

/// Track width and clearance a class may neck down to inside an array part's courtyard.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct NeckRule {
    pub width: Nm,
    pub clearance: Nm,
}
canon_struct!(NeckRule { width, clearance });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct ViaSize {
    pub drill: Nm,
    pub pad: Nm,
}
canon_struct!(ViaSize { drill, pad });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct PairRule {
    pub width: Nm,
    pub gap: Nm,
}
canon_struct!(PairRule { width, gap });

#[derive(Clone, PartialEq, Debug)]
pub struct Bus {
    pub id: BusId,
    pub name: String,
    /// Member name (`SWDIO`, `LED0`) and its net.
    pub members: Vec<(String, NetId)>,
}
canon_struct!(Bus { id, name, members });

/// A block definition (E2.7): typed ports, parameters, sheet defaults, an optional board template.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Default)]
pub struct BlockDefId(pub u32);
canon_struct!(BlockDefId { 0 });

#[derive(Clone, PartialEq, Debug)]
pub struct BlockDef {
    pub id: BlockDefId,
    pub name: String,
    pub ports: Vec<PortDef>,
    /// Parameters and their defaults.
    pub params: Vec<(String, ParamValue)>,
    pub sheet: BlockSheet,
    /// The layout document of the block's board template.
    pub template: Option<String>,
    /// An architectural block whose implementation may still be absent.
    pub planned: bool,
    pub role: Option<String>,
}

impl fab_elec_core::Canon for BlockDef {
    fn canon(&self, w: &mut fab_elec_core::Writer) {
        let Self { id, name, ports, params, sheet, template, planned, role } = self;
        id.canon(w);
        name.canon(w);
        ports.canon(w);
        params.canon(w);
        sheet.canon(w);
        template.canon(w);
        // Preserve the six-field encoding of existing definitions.
        if *planned {
            w.tag(1);
        }
        if let Some(role) = role {
            w.tag(2);
            role.canon(w);
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct PortDef {
    pub name: String,
    pub kind: PortKind,
    pub volts: Option<VRange>,
    pub current: Option<f64>,
    /// What an unbound port connects to (a net name of the caller).
    pub default: Option<String>,
}
canon_struct!(PortDef { name, kind, volts, current, default });

#[derive(Clone, PartialEq, Debug)]
pub enum PortKind {
    PowerIn,
    PowerOut,
    Ground,
    In,
    Out,
    Bidi,
    Passive,
    Bus { width: u16 },
    Pair,
    Protocol(String),
}
canon_enum!(PortKind {
    0 => PowerIn, 1 => PowerOut, 2 => Ground, 3 => In, 4 => Out, 5 => Bidi, 6 => Passive, 7 => Bus { width }, 8 => Pair,
    9 => Protocol(p),
});

/// How a block lays out on a sheet by default.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct BlockSheet {
    pub flow: Flow,
    pub sides: Vec<(String, PortSide)>,
}
canon_struct!(BlockSheet { flow, sides });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Flow {
    #[default]
    Right,
    Down,
    Left,
    Up,
}
canon_tags!(Flow);

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum PortSide {
    Left,
    Right,
    Top,
    Bottom,
}
canon_tags!(PortSide);

/// A block instance in the tree.
#[derive(Clone, PartialEq, Debug)]
pub struct Block {
    pub id: BlockId,
    /// The instance path (`mcu`, `lamps[3]`); `""` for the root.
    pub path: String,
    /// `None` for the root and for plain fn calls.
    pub def: Option<BlockDefId>,
    pub parent: Option<BlockId>,
    /// Bound nets per port, in port order (a bus port binds several).
    pub ports: Vec<(String, Vec<NetId>)>,
    pub params: Vec<(String, ParamValue)>,
    pub source: BlockSource,
}
canon_struct!(Block { id, path, def, parent, ports, params, source });

#[derive(Clone, PartialEq, Debug)]
pub enum BlockSource {
    Code,
    /// A drawn sheet, by its document name.
    Sheet(String),
}
canon_enum!(BlockSource { 0 => Code, 1 => Sheet(s) });

#[derive(Clone, PartialEq, Debug)]
pub enum ParamValue {
    Int(i64),
    Num(f64),
    Qty(Quantity),
    Bool(bool),
    Text(String),
    /// A symbol (`@green`).
    Sym(String),
    List(Vec<ParamValue>),
}
canon_enum!(ParamValue { 0 => Int(v), 1 => Num(v), 2 => Qty(q), 3 => Bool(b), 4 => Text(s), 5 => Sym(s), 6 => List(l) });

/// What an intent is about.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Target {
    Comp(CompId),
    Pin(CompId, PinIx),
    Block(BlockId),
}
canon_enum!(Target { 0 => Comp(c), 1 => Pin(c, p), 2 => Block(b) });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}
canon_tags!(Edge);

/// Design intent the placer, the router and the checks honour (E1 2.2).
#[derive(Clone, PartialEq, Debug)]
pub enum Intent {
    Near { a: Target, b: Target, max: Nm },
    Group { name: String, members: Vec<Target> },
    Side { t: Target, side: Side },
    Fixed { t: Target, edge: Option<Edge>, at: Option<P>, rot: Option<Angle> },
    Pair { p: NetId, n: NetId, gap: Option<Nm>, z0: Option<f64> },
    LengthMatch { nets: Vec<NetId>, tol: Nm },
    Width { net: NetId, w: Nm },
    LayerPref { net: NetId, layers: LayerSet },
    Star { net: NetId, at: Target },
    /// A homotopy hint (E6 10): the net goes through these points or round these parts on the
    /// given side; it survives part moves.
    Guide { net: NetId, through: Vec<GuidePoint>, layers: Option<LayerSet> },
    /// Nets routed together as one band, in order.
    Bundle { name: String, nets: Vec<NetId> },
    /// A memory byte lane: data nets matched to their strobe pair, kept on one layer pair.
    Lane { name: String, data: Vec<NetId>, strobe: Vec<NetId>, tol: Option<Nm> },
    Topology { net: NetId, topo: Topology },
    MaxVias { net: NetId, max: u32 },
    /// Keep these nets (all when empty) out of an area.
    Avoid { who: Vec<NetId>, region: AvoidRegion },
    /// The net needs an unbroken reference plane under it and a return via at each layer change.
    NeedsRef { nets: Vec<NetId> },
    /// The routing style of these nets (the whole board when empty).
    Style { nets: Vec<NetId>, style: RouteStyle },
}
canon_enum!(Intent {
    0 => Near { a, b, max }, 1 => Group { name, members }, 2 => Side { t, side }, 3 => Fixed { t, edge, at, rot },
    4 => Pair { p, n, gap, z0 }, 5 => LengthMatch { nets, tol }, 6 => Width { net, w }, 7 => LayerPref { net, layers },
    8 => Star { net, at }, 9 => Guide { net, through, layers }, 10 => Bundle { name, nets }, 11 => Lane { name, data, strobe, tol },
    12 => Topology { net, topo }, 13 => MaxVias { net, max }, 14 => Avoid { who, region }, 15 => NeedsRef { nets },
    16 => Style { nets, style },
});

/// One step of a guide: a point, or a part passed on the left or right in travel direction.
#[derive(Clone, PartialEq, Debug)]
pub enum GuidePoint {
    At(P),
    Pass { t: Target, left: bool },
}
canon_enum!(GuidePoint { 0 => At(p), 1 => Pass { t, left } });

#[derive(Clone, PartialEq, Debug)]
pub enum Topology {
    Tree,
    Star(Target),
    /// Daisy chain in this order, with the longest stub allowed.
    Chain { order: Vec<Target>, stub_max: Option<Nm> },
}
canon_enum!(Topology { 0 => Tree, 1 => Star(t), 2 => Chain { order, stub_max } });

#[derive(Clone, PartialEq, Debug)]
pub enum AvoidRegion {
    Area(PolySet),
    /// Within this distance of a part or pin.
    Near(Target, Nm),
}
canon_enum!(AvoidRegion { 0 => Area(a), 1 => Near(t, d) });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum RouteStyle {
    #[default]
    FortyFive,
    AnyAngle,
    Organic,
}
canon_tags!(RouteStyle);

/// The editor-written `sheet({...})` table at the tail of the circuit (E2.6): pins (`at`) and
/// styles keyed by path. `at` is in grid steps in the part's block frame. An entry's options are
/// `at`, `flow`, `turn`, `mirror` and `free` (`"r9": {at: [40 12] free: true}`); `"<path>.pins"`
/// entries give pin sides.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SheetHints {
    pub parts: Vec<(String, SheetHint)>,
    pub nets: Vec<(String, NetStyle)>,
}
canon_struct!(SheetHints { parts, nets });

/// A part or block's sheet entry. Independently placed parts take its turn and mirror.
/// A molecule member takes its orientation from its group.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SheetHint {
    pub at: Option<SchPt>,
    pub flow: Option<Flow>,
    /// Quarter turns counter-clockwise.
    pub turn: u8,
    /// Reflect left to right across the part's vertical axis before applying its turn.
    pub mirror: bool,
    /// Pin sides for a part's or block's pins (`"mcu.pins": {PA0: @right}`).
    pub pins: Vec<(String, PortSide)>,
    /// `free: true`: a placed part sits exactly at `at`, outside its level's columns, and its
    /// signal pins continue by name (a name flag at each pin; the net's wired side shows the name
    /// too) instead of by wire. The sheet editor writes it for a part placed where joining the
    /// columns would move something else, so that every other part stays where it is drawn. A drag
    /// keeps it; removing the part's hint (unpin, fit) returns the part to the generated layout.
    /// Ignored without `at`, and for blocks.
    pub free: bool,
}
canon_struct!(SheetHint { at, flow, turn, mirror, pins, free });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum NetStyle {
    Wire,
    Label,
}
canon_tags!(NetStyle);

/// The pins of every net, built once (`Circuit::net_pins`).
pub struct NetPins(pub Vec<Vec<(CompId, PinIx)>>);

impl Circuit {
    pub fn comp(&self, path: &str) -> Option<&Component> {
        self.comps.iter().find(|c| c.path == path)
    }
    pub fn net(&self, name: &str) -> Option<&Net> {
        self.nets.iter().find(|n| n.name == name)
    }
    /// The net of a pin, if it is connected.
    pub fn net_of(&self, comp: CompId, pin: PinIx) -> Option<NetId> {
        self.comps.get(comp.ix())?.conns.iter().find(|(p, _)| *p == pin).map(|(_, n)| *n)
    }
    pub fn pins_of(&self, net: NetId) -> Vec<(CompId, PinIx)> {
        let mut v = Vec::new();
        for c in &self.comps {
            for (p, n) in &c.conns {
                if *n == net {
                    v.push((c.id, *p));
                }
            }
        }
        v
    }
    /// Every net's pins in one pass.
    pub fn net_pins(&self) -> NetPins {
        let mut v = vec![Vec::new(); self.nets.len()];
        for c in &self.comps {
            for (p, n) in &c.conns {
                if let Some(list) = v.get_mut(n.ix()) {
                    list.push((c.id, *p));
                }
            }
        }
        NetPins(v)
    }
    pub fn nets_of(&self, comp: CompId) -> Vec<NetId> {
        let mut v: Vec<NetId> = self.comps.get(comp.ix()).map(|c| c.conns.iter().map(|(_, n)| *n).collect()).unwrap_or_default();
        v.sort();
        v.dedup();
        v
    }
    /// The components a block instance made, its descendants' included.
    pub fn block_comps(&self, block: BlockId) -> Vec<CompId> {
        let inside = |mut b: BlockId| loop {
            if b == block {
                return true;
            }
            match self.tree.get(b.ix()).and_then(|k| k.parent) {
                Some(p) => b = p,
                None => return false,
            }
        };
        self.comps.iter().filter(|c| inside(c.block)).map(|c| c.id).collect()
    }
    pub fn pin_kind(&self, comp: CompId, pin: PinIx) -> Option<PinKind> {
        Some(self.comps.get(comp.ix())?.part.pins.get(pin.ix())?.kind)
    }
    /// The model's invariants; the first broken one.
    pub fn check_ids(&self) -> Result<(), String> {
        let bad = |what: &str, i: usize| Err(format!("{}: {what}[{i}] has the wrong id", self.name));
        for (i, c) in self.comps.iter().enumerate() {
            if c.id.ix() != i {
                return bad("comps", i);
            }
            for (p, n) in &c.conns {
                if n.ix() >= self.nets.len() || p.ix() >= c.part.pins.len() {
                    return Err(format!("{}: {} connects a pin or net that does not exist", self.name, c.path));
                }
                if c.nc.contains(p) {
                    return Err(format!("{}: {} pin {} is connected and marked not connected", self.name, c.path, c.part.pins[p.ix()].name));
                }
            }
            if c.block.ix() >= self.tree.len() {
                return Err(format!("{}: {} names a block that does not exist", self.name, c.path));
            }
            if self.comps[..i].iter().any(|o| o.path == c.path) {
                return Err(format!("{}: path {} twice", self.name, c.path));
            }
        }
        for (i, n) in self.nets.iter().enumerate() {
            if n.id.ix() != i {
                return bad("nets", i);
            }
            if n.class.ix() >= self.classes.len().max(1) {
                return Err(format!("{}: net {} names a class that does not exist", self.name, n.name));
            }
            if self.nets[..i].iter().any(|o| o.name == n.name) {
                return Err(format!("{}: net name {} twice", self.name, n.name));
            }
        }
        for (i, c) in self.classes.iter().enumerate() {
            if c.id.ix() != i {
                return bad("classes", i);
            }
        }
        for (i, b) in self.tree.iter().enumerate() {
            if b.id.ix() != i {
                return bad("tree", i);
            }
        }
        for (i, b) in self.buses.iter().enumerate() {
            if b.id.ix() != i {
                return bad("buses", i);
            }
        }
        for (i, d) in self.block_defs.iter().enumerate() {
            if d.id.0 as usize != i {
                return bad("block_defs", i);
            }
        }
        Ok(())
    }
}

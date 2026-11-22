//! Fab electronics design model: contract C2, `fab-elec-design` (E2.18 as corrected by E4).
//!
//! A `Design` is the whole project's electronics: units (a board, a cable or an off-board part,
//! each with its own `Circuit`, boards with a `Board`), links between units (mates and wires),
//! the assembly (where each unit sits), and bindings of blocks and parts to mechanism joints.
//! The host (`ElecHost`) publishes one `Arc<Design>`; views and queries take `&Design`. A
//! single-board project is a design with one unit, no links and one `Mount { World, identity }`.
//!
//! Ids are scoped to their unit; `CompRef`, `PinRef`, `NetRef`, `FeatureRef` and `Sel` (core)
//! carry the unit. Net names are per unit and never merge: units join only through links, and
//! `sysnets` computes the union (it is not cached here, E4 item 2).
//!
//! Also here: `Focus` and `TimeCursor`, the two broadcasts every view answers; the action
//! catalog (C13, `action`); the design ops
//! and the op log (C3, `ops`); the pure functions on `&Design` (C4): `sysnets`, `system_erc`,
//! `project`, the trace queries (`trace`); the hand-written fixtures (`fixture`).
//!
//! Cost: plain data; lookups by unit id are linear in units (a handful).

pub mod action;
mod derived;
pub mod edit;
pub mod engines;
pub mod fixture;
pub mod ops;
pub mod options;
pub mod project;
pub mod syserc;
pub mod sysnets;
pub mod trace;

use fab_elec_board::{pairs_of, Board};
use fab_elec_circuit::Circuit;
use fab_elec_core::{canon_enum, canon_struct, canon_tags, BlockId, BoardId, CompRef, Diag, FeatureRef, LinkId, NetId, NetRef, Nm, PinIx, PinRef, Sel, Xform3};
use fab_elec_parts::{BodySource, VRange};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

pub use action::ActionDef;
pub use edit::{By, PartRef, Place, Resolved};
pub use ops::{AssetBytes, AssetStep, BenchOp, DesignOp, EntryKind, FileOp, OpEntry, OpLog, ProbeOf, ProbeQuantity, SysOp, TextEdit, TextStep, Tip};
pub use project::{project, Feature, ProjectInto};
pub use syserc::system_erc;
pub use sysnets::{sysnets, SystemNet};

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Design {
    pub units: Vec<Unit>,
    pub links: Vec<Link>,
    pub assembly: Assembly,
    pub bindings: Vec<Binding>,
    /// Bumped by every applied op; not content.
    pub generation: u64,
}
canon_struct!(Design { units, links, assembly, bindings });

/// One unit: a board, a cable or harness, or an off-board part (E4 Q3).
/// Invariant: `board.is_some()` exactly when `kind == UnitKind::Board`.
#[derive(Clone, PartialEq, Debug)]
pub struct Unit {
    pub id: BoardId,
    pub name: String,
    pub kind: UnitKind,
    pub circuit: Arc<Circuit>,
    /// Nets of this circuit driven by the bench, sorted and without repeats.
    pub supplied: Vec<NetId>,
    pub board: Option<Arc<Board>>,
    /// The factory profile the unit is made by (its name); its capabilities raise the effective
    /// rules, never the stored ones.
    pub factory: Option<String>,
}
// A unit with no bench supplies keeps its canonical hash.
impl fab_elec_core::canon::Canon for Unit {
    fn canon(&self, w: &mut fab_elec_core::canon::Writer) {
        let Unit { id, name, kind, circuit, board, factory, supplied } = self;
        id.canon(w);
        name.canon(w);
        kind.canon(w);
        circuit.canon(w);
        board.canon(w);
        factory.canon(w);
        if !supplied.is_empty() {
            supplied.canon(w);
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum UnitKind {
    Board,
    /// A cable or harness: its circuit has the end housings as components and its conductors
    /// as nets (E4 Q2); the definition (routes, folds) is `elec/cable`'s.
    Cable(CableRef),
    /// A part off any board (a fan, a motor with its leads).
    Part,
}
canon_enum!(UnitKind { 0 => Board, 1 => Cable(c), 2 => Part });

/// A cable definition, by its document name (`fan.cable.splash`).
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct CableRef(pub String);
canon_struct!(CableRef { 0 });

/// A link between units (E4 Q2): two connectors mated, or two pins wired.
#[derive(Clone, PartialEq, Debug)]
pub struct Link {
    pub id: LinkId,
    pub kind: LinkKind,
    pub a: LinkEnd,
    pub b: LinkEnd,
    pub pinmap: PinMap,
}
canon_struct!(Link { id, kind, a, b, pinmap });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum LinkKind {
    /// Two connectors joined: board to board, or a cable end to a board.
    Mate,
    /// Two pins joined by a wire.
    Wire,
}
canon_tags!(LinkKind);

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum LinkEnd {
    Comp(CompRef),
    Pin(PinRef),
}
canon_enum!(LinkEnd { 0 => Comp(c), 1 => Pin(p) });

#[derive(Clone, PartialEq, Debug)]
pub enum PinMap {
    /// From the connectors: pins of the same name pair (until mate specs, E2.18 7, say more).
    Derived,
    /// Explicit pairs (pin of `a`, pin of `b`).
    Given(Vec<(PinIx, PinIx)>),
}
canon_enum!(PinMap { 0 => Derived, 1 => Given(v) });

/// Where every unit sits, and in what order the product is put together.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Assembly {
    /// One per unit, sorted by unit.
    pub mounts: Vec<Mount>,
    pub spacers: Vec<Spacer>,
    pub order: Vec<AsmStep>,
}
canon_struct!(Assembly { mounts, spacers, order });

/// A unit's pose: `xform` in its parent frame (E2.18 5, E4 Q4).
#[derive(Clone, PartialEq, Debug)]
pub struct Mount {
    pub unit: BoardId,
    pub parent: Frame,
    pub xform: Xform3,
}
canon_struct!(Mount { unit, parent, xform });

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Frame {
    World,
    /// A fixed offset from another unit with no connector pair (a spacer stack).
    Unit(BoardId),
    /// Solved from the link's mate frames.
    Mate(LinkId),
    /// A body of a mechanism, by the mechanics port's stable names.
    Body { mechanism: String, body: String },
}
canon_enum!(Frame { 0 => World, 1 => Unit(b), 2 => Mate(l), 3 => Body { mechanism, body } });

/// A spacer or standoff between two holes of two units.
#[derive(Clone, PartialEq, Debug)]
pub struct Spacer {
    pub a: FeatureRef,
    pub b: FeatureRef,
    pub length: Nm,
    /// The part used, by library name.
    pub part: Option<String>,
}
canon_struct!(Spacer { a, b, length, part });

#[derive(Clone, PartialEq, Debug)]
pub struct AsmStep {
    pub what: Sel,
    pub note: String,
}
canon_struct!(AsmStep { what, note });

/// A block or part bound to a mechanism joint (E2.16).
#[derive(Clone, PartialEq, Debug)]
pub struct Binding {
    pub target: BindTarget,
    pub role: BindRole,
    /// (mechanism, joint) by the mechanics port's names.
    pub joint: (String, String),
}
canon_struct!(Binding { target, role, joint });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum BindTarget {
    Block(BoardId, BlockId),
    Comp(CompRef),
}
canon_enum!(BindTarget { 0 => Block(u, b), 1 => Comp(c) });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum BindRole {
    Drives,
    Senses,
}
canon_tags!(BindRole);

/// The one selection message (ARCHITECTURE 2.4, E4): every view answers it.
#[derive(Clone, PartialEq, Debug)]
pub struct Focus {
    pub kind: FocusKind,
    pub sel: Sel,
    pub origin: FocusOrigin,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum FocusKind {
    Part,
    Net,
    Pin,
    Block,
    Board,
    Cable,
    Joint,
    Feature,
    Solid,
    SourceRange,
}

/// Which view or tool sent a focus (a view does not answer its own).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum FocusOrigin {
    Explorer,
    Sheet,
    Board,
    Part,
    Scene,
    Code,
    Bench,
    Checks,
    System,
    Fab,
    Ai,
}

/// The firmware at a cursor time (recorded events only): the last event at or before `t` and the
/// handler active at `t`. Valid only for the cursor whose `t` it was computed for.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct FwAt { pub t: f64, pub event: Option<u64>, pub handler: Option<u16> }

/// The shared time cursor, broadcast like `Focus` (E4): simulation time, motion time or a
/// replay position, the same in every view.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct TimeCursor {
    /// Seconds.
    pub t: f64,
    pub playing: bool,
    /// Playback rate (1.0 real time).
    pub rate: f64,
    pub source: CursorSource,
    pub fw: Option<FwAt>,
}

impl TimeCursor {
    /// Firmware recorded at this paused cursor's bit-equal time. A copy with `..c` and a
    /// moved `t` never reports stale firmware through this method.
    pub fn fw(&self) -> Option<FwAt> {
        self.fw.filter(|fw| !self.playing && fw.t.to_bits() == self.t.to_bits())
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum CursorSource {
    Sim,
    Motion,
    Replay,
}

impl Unit {
    /// Map project-relative body meshes to registered asset keys after evaluation, so every
    /// reader uses the plate's bytes. Unknown and absolute paths stay unchanged. O(components ×
    /// log assets); only changed circuits and parts lose sharing through `Arc::make_mut`.
    pub fn resolve_meshes(&mut self, keys: &BTreeMap<String, String>) {
        for i in 0..self.circuit.comps.len() {
            let Some(body) = &self.circuit.comps[i].part.body else { continue };
            let BodySource::Mesh(path) = &body.source else { continue };
            if Path::new(path).is_absolute() { continue }
            let Some(resolved) = keys.get(path) else { continue };
            if resolved == path { continue }
            let resolved = resolved.clone();
            let comp = &mut Arc::make_mut(&mut self.circuit).comps[i];
            if let Some(body) = &mut Arc::make_mut(&mut comp.part).body {
                body.source = BodySource::Mesh(resolved);
            }
        }
    }
}

impl Design {
    /// Resolve each unit's project-relative body meshes, preserving unchanged circuit and part
    /// Arcs. Cost: O(components × log assets); see [`Unit::resolve_meshes`].
    pub fn resolve_meshes(&mut self, keys: &BTreeMap<String, String>) {
        for unit in &mut self.units {
            unit.resolve_meshes(keys);
        }
    }

    /// The meshes the parts' bodies name that the project does not hold yet
    /// (`Body3d::missing_mesh`), each once, in name order: the models the project lacks.
    /// Cost: O(components) plus the sort of what is missing.
    pub fn missing_meshes(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.units.iter()
            .flat_map(|unit| unit.circuit.comps.iter())
            .filter_map(|comp| comp.part.body.as_ref()?.missing_mesh.clone())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        paths
    }

    pub fn unit(&self, id: BoardId) -> Option<&Unit> {
        self.units.iter().find(|u| u.id == id)
    }
    pub fn unit_by_name(&self, name: &str) -> Option<&Unit> {
        self.units.iter().find(|u| u.name == name)
    }
    pub fn link(&self, id: LinkId) -> Option<&Link> {
        self.links.iter().find(|l| l.id == id)
    }
    pub fn mount(&self, unit: BoardId) -> Option<&Mount> {
        self.assembly.mounts.iter().find(|m| m.unit == unit)
    }
    /// The net of a pin, as a design-wide reference.
    pub fn net_of(&self, pin: PinRef) -> Option<NetRef> {
        let net = self.unit(pin.board)?.circuit.net_of(pin.comp, pin.pin)?;
        Some(NetRef { board: pin.board, net })
    }
    /// The name a `BenchOp` probe takes for a net: its circuit net's name, which the lowering
    /// names the net's solver node by (the ground net's node is "0"; the bench maps its name).
    pub fn probe_net(&self, net: NetRef) -> Option<&str> {
        Some(self.unit(net.board)?.circuit.nets.get(net.net.ix())?.name.as_str())
    }
    /// The pin pairs a link joins (E4 Q2: every query uses this resolved view).
    pub fn link_pairs(&self, link: &Link) -> Vec<(PinRef, PinRef)> {
        let pins_of = |c: CompRef| self.unit(c.board).and_then(|u| u.circuit.comps.get(c.comp.ix())).map(|k| k.part.clone());
        match (link.a, link.b, &link.pinmap) {
            (LinkEnd::Pin(a), LinkEnd::Pin(b), _) => vec![(a, b)],
            (LinkEnd::Comp(a), LinkEnd::Comp(b), PinMap::Given(pairs)) => pairs
                .iter()
                .map(|(pa, pb)| (PinRef { board: a.board, comp: a.comp, pin: *pa }, PinRef { board: b.board, comp: b.comp, pin: *pb }))
                .collect(),
            (LinkEnd::Comp(a), LinkEnd::Comp(b), PinMap::Derived) => {
                let (Some(pa), Some(pb)) = (pins_of(a), pins_of(b)) else { return Vec::new() };
                pa.pins
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| {
                        pb.pin(&p.name).map(|j| (PinRef { board: a.board, comp: a.comp, pin: PinIx(i as u16) }, PinRef { board: b.board, comp: b.comp, pin: j }))
                    })
                    .collect()
            }
            (LinkEnd::Comp(c), LinkEnd::Pin(p), PinMap::Given(pairs)) | (LinkEnd::Pin(p), LinkEnd::Comp(c), PinMap::Given(pairs)) => {
                pairs.iter().map(|(pc, _)| (PinRef { board: c.board, comp: c.comp, pin: *pc }, p)).collect()
            }
            _ => Vec::new(),
        }
    }
    /// The model's invariants across units (E4 A5): the first broken ones as diagnostics. Tests
    /// call it on every fixture; the host after building a design.
    pub fn check_consistent(&self) -> Vec<Diag> {
        let mut out = Vec::new();
        let mut err = |m: String, sel: Vec<Sel>| out.push(Diag::error(m, sel));
        for (i, u) in self.units.iter().enumerate() {
            let us = vec![Sel::Unit(u.id)];
            if self.units[..i].iter().any(|o| o.id == u.id || o.name == u.name) {
                err(format!("unit {} ({:?}) twice", u.name, u.id), us.clone());
            }
            if u.circuit.unit != u.id {
                err(format!("unit {}: its circuit says unit {:?}", u.name, u.circuit.unit), us.clone());
            }
            if let Err(e) = u.circuit.check_ids() {
                err(e, us.clone());
            }
            for c in &u.circuit.comps {
                if let Err(e) = c.part.check() {
                    err(e, vec![Sel::Comp(CompRef { board: u.id, comp: c.id })]);
                }
            }
            match (&u.kind, &u.board) {
                (UnitKind::Board, Some(b)) => board_consistent(u, b, &mut err),
                (UnitKind::Board, None) => err(format!("board unit {} has no board", u.name), us.clone()),
                (_, Some(_)) => err(format!("unit {} is not a board but has one", u.name), us.clone()),
                (_, None) => {}
            }
            if self.mount(u.id).is_none() {
                err(format!("unit {} has no mount", u.name), us);
            }
        }
        for (i, l) in self.links.iter().enumerate() {
            if self.links[..i].iter().any(|o| o.id == l.id) {
                err(format!("link {:?} twice", l.id), vec![Sel::Link(l.id)]);
            }
            for end in [l.a, l.b] {
                let ok = match end {
                    LinkEnd::Comp(c) => self.unit(c.board).is_some_and(|u| c.comp.ix() < u.circuit.comps.len()),
                    LinkEnd::Pin(p) => self.unit(p.board).and_then(|u| u.circuit.comps.get(p.comp.ix())).is_some_and(|c| p.pin.ix() < c.part.pins.len()),
                };
                if !ok {
                    err(format!("link {:?} names a part or pin that does not exist", l.id), vec![Sel::Link(l.id)]);
                }
            }
        }
        for m in &self.assembly.mounts {
            let ok = match &m.parent {
                Frame::World | Frame::Body { .. } => true,
                Frame::Unit(b) => *b != m.unit && self.unit(*b).is_some(),
                Frame::Mate(l) => self.link(*l).is_some(),
            };
            if self.unit(m.unit).is_none() || !ok {
                err(format!("mount of {:?} names a unit or link that does not exist", m.unit), vec![Sel::Unit(m.unit)]);
            }
        }
        out
    }
}

fn board_consistent(u: &Unit, b: &Board, err: &mut impl FnMut(String, Vec<Sel>)) {
    let c = &u.circuit;
    let us = vec![Sel::Unit(u.id)];
    if b.unit != u.id {
        err(format!("unit {}: its board says unit {:?}", u.name, b.unit), us.clone());
    }
    if b.nets.len() != c.nets.len() || b.nets.iter().zip(&c.nets).any(|(bn, cn)| bn.name != cn.name || bn.class != cn.class) {
        err(format!("unit {}: the board's nets do not mirror the circuit's", u.name), us.clone());
    }
    if b.classes != c.classes {
        err(format!("unit {}: the board's classes do not mirror the circuit's", u.name), us.clone());
    }
    if b.pairs != pairs_of(c) {
        err(format!("unit {}: the board's pairs do not mirror the circuit's", u.name), us.clone());
    }
    let mut ids: Vec<fab_elec_core::ItemId> = Vec::new();
    for p in &b.placed {
        ids.push(p.id);
        let sel = vec![Sel::Item(u.id, p.id)];
        let Some(comp) = c.comps.get(p.comp.ix()) else {
            err(format!("placement {} names a component that does not exist", p.refdes), sel);
            continue;
        };
        if comp.path != p.path || comp.refdes != p.refdes {
            err(format!("placement {} does not match component {}", p.refdes, comp.path), sel.clone());
        }
        if b.placed.iter().filter(|q| q.comp == p.comp).count() > 1 {
            err(format!("component {} placed twice", comp.path), sel.clone());
        }
        if p.pad_nets.len() != p.footprint.pads.len() {
            err(format!("placement {}: pad nets and pads differ in number", p.refdes), sel.clone());
            continue;
        }
        let pad_pins = comp.part.pad_pins();
        for (pi, want) in pad_pins.iter().enumerate() {
            let net = want.and_then(|pin| c.net_of(comp.id, pin));
            if p.pad_nets.get(pi).copied().flatten() != net {
                err(format!("placement {} pad {}: net differs from the circuit", p.refdes, p.footprint.pads[pi].no), sel.clone());
            }
        }
    }
    for t in &b.tracks {
        ids.push(t.id);
        for (e, a) in t.ends.iter().enumerate() {
            let at = if e == 0 { t.path.start } else { t.path.end() };
            if let Some(pos) = b.anchor_pos(*a) {
                if pos != at {
                    err(format!("track {:?}: an anchored end is off its anchor", t.id), vec![Sel::Item(u.id, t.id)]);
                }
            }
        }
    }
    ids.extend(b.vias.iter().map(|x| x.id));
    ids.extend(b.zones.iter().map(|x| x.id));
    ids.extend(b.keepouts.iter().map(|x| x.id));
    ids.extend(b.holes.iter().map(|x| x.id));
    ids.extend(b.cutouts.iter().map(|x| x.id));
    ids.extend(b.graphics.iter().map(|x| x.id));
    ids.extend(b.texts.iter().map(|x| x.id));
    ids.extend(b.tunings.iter().map(|x| x.id));
    let n = ids.len();
    ids.sort();
    ids.dedup();
    if ids.len() != n || ids.last().is_some_and(|i| i.0 >= b.next_item) {
        err(format!("unit {}: board item ids repeat or reach next_item", u.name), us);
    }
}

impl Default for Mount {
    fn default() -> Self {
        Mount { unit: BoardId(0), parent: Frame::World, xform: Xform3::IDENTITY }
    }
}

/// Volts of a system net from its members' declarations: their intersection, `None` when none
/// declares or they disagree (system ERC reports the disagreement).
pub fn common_volts(v: impl IntoIterator<Item = VRange>) -> Option<VRange> {
    let mut acc: Option<VRange> = None;
    for r in v {
        acc = Some(match acc {
            None => r,
            Some(a) => {
                let m = VRange { min: a.min.max(r.min), max: a.max.min(r.max) };
                if m.min > m.max {
                    return None;
                }
                m
            }
        });
    }
    acc
}

//! Ids. Plain indices with no meaning outside the model that issued them.
//!
//! Scope (a project is a `Design` of units, one per board; no id is global):
//! - `BoardId`: a unit (one board with its own circuit) in the design.
//! - `CompId`, `NetId`, `PinIx`, `ClassId`, `BlockId`, `BusId`: unique within one unit's `Circuit`.
//! - `ItemId`: unique within one unit's `Board` (its statements); never written into a document.
//! - `PadIx`: index into one footprint's pads.
//! - `LinkId`: a link between units (a mate, a cable conductor, a wire) in the design.
//!
//! Anything that crosses units says which: `CompRef`, `PinRef`, `NetRef` (and `FeatureRef`,
//! `Sel` in `sel`) carry the `BoardId`.
//! Net names are per board and never merge across boards; boards join only through links.

macro_rules! id {
    ($(#[$m:meta])* $name:ident($t:ty)) => {
        $(#[$m])*
        #[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Default)]
        pub struct $name(pub $t);
        impl $name {
            #[inline]
            pub fn ix(self) -> usize {
                self.0 as usize
            }
        }
        impl $crate::canon::Canon for $name {
            fn canon(&self, w: &mut $crate::canon::Writer) {
                $crate::canon::Canon::canon(&self.0, w)
            }
        }
    };
}

id!(
    /// A unit of the design: one board and its circuit.
    BoardId(u16)
);

/// Ids from here up name a bench's own circuit, never a unit of the design.
pub const BENCH_SCOPE_BASE: u16 = 0x8000;

impl BoardId {
    /// Whether this id names a bench's own circuit.
    pub fn is_bench_scope(self) -> bool {
        self.0 >= BENCH_SCOPE_BASE
    }
}

id!(
    /// A link between units; the index into `Design::links`.
    LinkId(u32)
);
id!(
    /// A component of a circuit; the index into `Circuit::comps`.
    CompId(u32)
);
id!(
    /// A net of a circuit; the index into `Circuit::nets`.
    NetId(u32)
);
id!(
    /// A pin of a part; the index into `PartDef::pins`.
    PinIx(u16)
);
id!(
    /// A net class; the index into `Circuit::classes` (0 is the default class).
    ClassId(u16)
);
id!(
    /// A block (one sub-circuit fn call, or a drawn sheet); the index into `Circuit::tree`.
    BlockId(u32)
);
id!(
    /// A bus; the index into `Circuit::buses`.
    BusId(u32)
);
id!(
    /// A board item (one statement of the board document: a placement, track, via, zone, ...).
    ItemId(u32)
);
id!(
    /// A pad of a footprint; the index into `Footprint::pads`.
    PadIx(u16)
);
id!(
    /// A source document of the project (a `.ckt.splash`, `.pcb.splash`, ... file), issued by
    /// the language's source set (C5).
    DocId(u32)
);
id!(
    /// The engine that made a `Solved` board item (`Origin::Solved { by }`); the values are
    /// named by `fab_elec_route::solvers`.
    SolverId(u16)
);

/// A component of one unit.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct CompRef {
    pub board: BoardId,
    pub comp: CompId,
}

/// A pin of a component of one unit.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct PinRef {
    pub board: BoardId,
    pub comp: CompId,
    pub pin: PinIx,
}

/// A net of one unit.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct NetRef {
    pub board: BoardId,
    pub net: NetId,
}

crate::canon_struct!(CompRef { board, comp });
crate::canon_struct!(PinRef { board, comp, pin });
crate::canon_struct!(NetRef { board, net });

impl PinRef {
    pub fn comp_ref(self) -> CompRef {
        CompRef { board: self.board, comp: self.comp }
    }
}

//! The one selection id (E4 A16, item 4): every view, check, query and the source map speak `Sel`.

use crate::ids::{BlockId, BoardId, CompRef, DocId, ItemId, LinkId, NetRef, PadIx, PinRef};

/// A feature another unit's geometry can be derived from (E2.17): projected through the assembly
/// into the target unit's frame.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum FeatureRef {
    /// A part's origin (placement point and rotation).
    Part(CompRef),
    /// One pad of a placed part.
    Pad(CompRef, PadIx),
    /// A part's 3D body silhouette.
    Body(CompRef),
    /// A declared port profile of a part's body (`Body3d::ports` index).
    Port(CompRef, u16),
    /// A hole item of a board.
    Hole(BoardId, ItemId),
    /// The holes of a board sharing a name prefix (`holes("mount")`).
    HoleSet(BoardId, String),
    /// A board's outline.
    Outline(BoardId),
    /// A sketch an object script exports, by its binding name.
    Sketch(String),
    /// One face of a CAD solid (E7 1.3): the solid's binding and the kernel's stable face id.
    Face { solid: String, face: u32 },
    /// A panel body that takes derived cuts (`panel(...)` in a case script, E7 1.1).
    Panel(String),
    /// A generated part (a generator instance in an object script, by its binding name): the key of
    /// the board items it derives (`Origin::Derived.from`) and the subject of its parameters.
    Generated(String),
}

crate::canon_enum!(FeatureRef {
    0 => Part(c), 1 => Pad(c, p), 2 => Body(c), 3 => Port(c, i), 4 => Hole(b, i),
    5 => HoleSet(b, s), 6 => Outline(b), 7 => Sketch(s), 8 => Face { solid, face }, 9 => Panel(s), 10 => Generated(s),
});

/// The one id of anything a person or the AI can point at.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Sel {
    /// A unit: a board, a cable or an off-board part.
    Unit(BoardId),
    Comp(CompRef),
    Pin(PinRef),
    Net(NetRef),
    Block(BoardId, BlockId),
    /// A board item (placement, track, via, zone, keepout, hole, cutout, graphic, text).
    Item(BoardId, ItemId),
    Link(LinkId),
    Feature(FeatureRef),
    /// A mechanism joint, by the mechanics port's stable names.
    Joint { mechanism: String, joint: String },
    /// A solid of an object script, by its binding.
    Solid(String),
    /// A range of a source document that maps to nothing else (the code panel's selection).
    Source { doc: DocId, start: u32, end: u32 },
}

crate::canon_enum!(Sel {
    0 => Unit(b), 1 => Comp(c), 2 => Pin(p), 3 => Net(n), 4 => Block(b, k), 5 => Item(b, i), 6 => Link(l),
    7 => Feature(f), 8 => Joint { mechanism, joint }, 9 => Solid(s), 10 => Source { doc, start, end },
});

impl Sel {
    /// The unit the selection is in, if it is in one.
    pub fn unit(&self) -> Option<BoardId> {
        match self {
            Sel::Unit(b) | Sel::Block(b, _) | Sel::Item(b, _) => Some(*b),
            Sel::Comp(c) => Some(c.board),
            Sel::Pin(p) => Some(p.board),
            Sel::Net(n) => Some(n.board),
            Sel::Feature(f) => match f {
                FeatureRef::Part(c) | FeatureRef::Pad(c, _) | FeatureRef::Body(c) | FeatureRef::Port(c, _) => Some(c.board),
                FeatureRef::Hole(b, _) | FeatureRef::HoleSet(b, _) | FeatureRef::Outline(b) => Some(*b),
                FeatureRef::Sketch(_) | FeatureRef::Face { .. } | FeatureRef::Panel(_) | FeatureRef::Generated(_) => None,
            },
            Sel::Link(_) | Sel::Joint { .. } | Sel::Solid(_) | Sel::Source { .. } => None,
        }
    }
}

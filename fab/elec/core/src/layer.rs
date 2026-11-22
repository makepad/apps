//! Layer identities and sets.
//!
//! A `LayerId` is a fixed role identity, the same on every board and in every footprint, so a
//! library footprint can name "front copper" or "back silk" without knowing a board's stackup.
//! Which layers a board actually has, and their physical order and thickness, is the stackup's
//! business (`fab_elec_board::Stackup`). Numbering (E4 A1):
//!
//! - copper `0..=127`: `0` = front (`F`), `127` = back (`B`), `1..=126` = inner 1..126 counted
//!   from the front;
//! - technical pairs `128..=139` (silk, mask, paste, adhesive, courtyard, fabrication; even =
//!   front);
//! - `140` edge, `141` margin, `142` drawings, `143` comments, `144`/`145` the two engineering
//!   layers; `146..=159` unassigned;
//! - `160..=255` user layers.
//!
//! The conventional board-file names (`F.Cu`, `In2.Cu`, `B.SilkS`) are a table for the interop
//! edge (`name`, `parse`), not identities.

/// A layer identity (see the module doc for the numbering).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct LayerId(pub u8);

/// What a layer is for.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum LayerKind {
    Copper,
    Silk,
    Mask,
    Paste,
    Adhesive,
    Courtyard,
    Fab,
    Edge,
    Margin,
    Drawing,
    Comment,
    Eco,
    User,
    /// An identity in the unassigned range `146..=159`.
    Unassigned,
}

/// The side a layer belongs to.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum LayerSide {
    Front,
    Back,
    /// Inner copper, edge, drawings and user layers.
    Neither,
}

/// The side of the board a component sits on.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Side {
    #[default]
    Top,
    Bottom,
}

crate::canon_tags!(Side);

impl Side {
    pub fn flipped(self) -> Side {
        match self {
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }
}

impl LayerId {
    pub const F_CU: LayerId = LayerId(0);
    pub const B_CU: LayerId = LayerId(127);
    /// The most inner copper layers a board can have.
    pub const MAX_INNER: u8 = 126;
    pub const F_SILK: LayerId = LayerId(128);
    pub const B_SILK: LayerId = LayerId(129);
    pub const F_MASK: LayerId = LayerId(130);
    pub const B_MASK: LayerId = LayerId(131);
    pub const F_PASTE: LayerId = LayerId(132);
    pub const B_PASTE: LayerId = LayerId(133);
    pub const F_ADHES: LayerId = LayerId(134);
    pub const B_ADHES: LayerId = LayerId(135);
    pub const F_CRTYD: LayerId = LayerId(136);
    pub const B_CRTYD: LayerId = LayerId(137);
    pub const F_FAB: LayerId = LayerId(138);
    pub const B_FAB: LayerId = LayerId(139);
    pub const EDGE: LayerId = LayerId(140);
    pub const MARGIN: LayerId = LayerId(141);
    pub const DRAWINGS: LayerId = LayerId(142);
    pub const COMMENTS: LayerId = LayerId(143);
    pub const ECO1: LayerId = LayerId(144);
    pub const ECO2: LayerId = LayerId(145);
    pub const USER_BASE: u8 = 160;
    pub const USER_COUNT: u8 = 96;

    /// Inner copper layer `n` (1-based from the front, `In1` .. `In126`).
    pub fn inner(n: u8) -> LayerId {
        assert!((1..=Self::MAX_INNER).contains(&n), "inner copper layer {n} out of 1..=126");
        LayerId(n)
    }
    /// User layer `n` (0-based, `User.1` is `user(0)`).
    pub fn user(n: u8) -> LayerId {
        assert!(n < Self::USER_COUNT, "user layer {n} out of range");
        LayerId(Self::USER_BASE + n)
    }
    #[inline]
    pub fn is_copper(self) -> bool {
        self.0 <= 127
    }
    /// The inner copper number (`In<n>`), `None` for outer copper and other layers.
    pub fn inner_index(self) -> Option<u8> {
        (1..=126).contains(&self.0).then_some(self.0)
    }
    pub fn kind(self) -> LayerKind {
        match self.0 {
            0..=127 => LayerKind::Copper,
            128 | 129 => LayerKind::Silk,
            130 | 131 => LayerKind::Mask,
            132 | 133 => LayerKind::Paste,
            134 | 135 => LayerKind::Adhesive,
            136 | 137 => LayerKind::Courtyard,
            138 | 139 => LayerKind::Fab,
            140 => LayerKind::Edge,
            141 => LayerKind::Margin,
            142 => LayerKind::Drawing,
            143 => LayerKind::Comment,
            144 | 145 => LayerKind::Eco,
            146..=159 => LayerKind::Unassigned,
            _ => LayerKind::User,
        }
    }
    pub fn side(self) -> LayerSide {
        match self.0 {
            0 => LayerSide::Front,
            127 => LayerSide::Back,
            128..=139 if self.0 % 2 == 0 => LayerSide::Front,
            128..=139 => LayerSide::Back,
            _ => LayerSide::Neither,
        }
    }
    /// The same-kind technical layer on `side` (`F_SILK.on(Side::Bottom) == B_SILK`); copper's
    /// outer layer for copper; other layers unchanged.
    pub fn on(self, side: Side) -> LayerId {
        let front = match self.0 {
            0 | 127 => 0,
            n @ 128..=139 => n & !1,
            _ => return self,
        };
        match (front, side) {
            (0, Side::Top) => Self::F_CU,
            (0, Side::Bottom) => Self::B_CU,
            (n, Side::Top) => LayerId(n),
            (n, Side::Bottom) => LayerId(n + 1),
        }
    }
    /// The layer seen from the other side: front and back swap; inner copper mirrors through
    /// the stack (`In1` <-> `In(n-2)` on a board of `copper_layers` copper layers).
    pub fn flipped(self, copper_layers: u8) -> LayerId {
        match self.0 {
            0 => Self::B_CU,
            127 => Self::F_CU,
            n @ 1..=126 => {
                let inner = copper_layers.saturating_sub(2);
                if n <= inner { LayerId(inner + 1 - n) } else { self }
            }
            n @ 128..=139 => LayerId(if n % 2 == 0 { n + 1 } else { n - 1 }),
            _ => self,
        }
    }
    /// The conventional short name (`F.Cu`, `In2.Cu`, `B.SilkS`, `Edge.Cuts`, `User.3`): the
    /// names people already know from board files, written by our own table.
    pub fn name(self) -> String {
        match self.0 {
            0 => "F.Cu".into(),
            127 => "B.Cu".into(),
            n @ 1..=126 => format!("In{n}.Cu"),
            n @ 128..=145 => FIXED[(n - 128) as usize].0.into(),
            n if n >= Self::USER_BASE => format!("User.{}", n - Self::USER_BASE + 1),
            n => format!("Layer{n}"),
        }
    }
    /// The inverse of `name`.
    pub fn parse(name: &str) -> Option<LayerId> {
        match name {
            "F.Cu" => return Some(Self::F_CU),
            "B.Cu" => return Some(Self::B_CU),
            _ => {}
        }
        if let Some((_, n)) = FIXED.iter().find(|(s, _)| *s == name) {
            return Some(LayerId(*n));
        }
        if let Some(n) = name.strip_prefix("In").and_then(|r| r.strip_suffix(".Cu")).and_then(|n| n.parse::<u8>().ok()) {
            return (1..=Self::MAX_INNER).contains(&n).then_some(LayerId(n));
        }
        if let Some(n) = name.strip_prefix("User.").and_then(|n| n.parse::<u8>().ok()) {
            return (1..=Self::USER_COUNT).contains(&n).then(|| LayerId(Self::USER_BASE + (n - 1)));
        }
        if let Some(n) = name.strip_prefix("Layer").and_then(|n| n.parse::<u8>().ok()) {
            return (146..=159).contains(&n).then_some(LayerId(n));
        }
        None
    }
}

/// Names of the technical and drawing layers `128..=145`, in id order.
const FIXED: [(&str, u8); 18] = [
    ("F.SilkS", 128), ("B.SilkS", 129), ("F.Mask", 130), ("B.Mask", 131), ("F.Paste", 132), ("B.Paste", 133),
    ("F.Adhes", 134), ("B.Adhes", 135), ("F.CrtYd", 136), ("B.CrtYd", 137), ("F.Fab", 138), ("B.Fab", 139),
    ("Edge.Cuts", 140), ("Margin", 141), ("Dwgs.User", 142), ("Cmts.User", 143), ("Eco1.User", 144), ("Eco2.User", 145),
];

/// A set of layers, one bit per `LayerId` (256 bits, E4 A2).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct LayerSet(pub [u64; 4]);

impl LayerSet {
    pub const EMPTY: LayerSet = LayerSet([0; 4]);
    /// Every copper layer identity (0..=127); intersect with a stackup's copper for a board's.
    pub const ALL_COPPER: LayerSet = LayerSet([u64::MAX, u64::MAX, 0, 0]);
    #[inline]
    pub fn one(l: LayerId) -> LayerSet {
        LayerSet::EMPTY.with(l)
    }
    pub fn of(layers: &[LayerId]) -> LayerSet {
        layers.iter().fold(LayerSet::EMPTY, |s, l| s.with(*l))
    }
    #[inline]
    pub fn with(mut self, l: LayerId) -> LayerSet {
        self.0[(l.0 >> 6) as usize] |= 1u64 << (l.0 & 63);
        self
    }
    #[inline]
    pub fn without(mut self, l: LayerId) -> LayerSet {
        self.0[(l.0 >> 6) as usize] &= !(1u64 << (l.0 & 63));
        self
    }
    #[inline]
    pub fn has(self, l: LayerId) -> bool {
        self.0[(l.0 >> 6) as usize] >> (l.0 & 63) & 1 == 1
    }
    #[inline]
    pub fn union(self, o: LayerSet) -> LayerSet {
        LayerSet([self.0[0] | o.0[0], self.0[1] | o.0[1], self.0[2] | o.0[2], self.0[3] | o.0[3]])
    }
    #[inline]
    pub fn intersect(self, o: LayerSet) -> LayerSet {
        LayerSet([self.0[0] & o.0[0], self.0[1] & o.0[1], self.0[2] & o.0[2], self.0[3] & o.0[3]])
    }
    #[inline]
    pub fn minus(self, o: LayerSet) -> LayerSet {
        LayerSet([self.0[0] & !o.0[0], self.0[1] & !o.0[1], self.0[2] & !o.0[2], self.0[3] & !o.0[3]])
    }
    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == [0; 4]
    }
    #[inline]
    pub fn len(self) -> u32 {
        self.0.iter().map(|w| w.count_ones()).sum()
    }
    /// The copper layers of the set.
    #[inline]
    pub fn copper_only(self) -> LayerSet {
        self.intersect(Self::ALL_COPPER)
    }
    /// Copper layers `F`, `In1`..`In(n-2)`, `B` of an `n`-layer board (`n` in `1..=128`).
    pub fn copper(n: u8) -> LayerSet {
        let mut s = LayerSet::one(LayerId::F_CU);
        if n >= 2 {
            s = s.with(LayerId::B_CU);
        }
        for i in 1..n.saturating_sub(1).min(LayerId::MAX_INNER + 1) {
            s = s.with(LayerId(i));
        }
        s
    }
    /// The layers in id order.
    pub fn iter(self) -> impl Iterator<Item = LayerId> {
        (0u16..256).map(|i| i as u8).filter(move |i| self.has(LayerId(*i))).map(LayerId)
    }
    /// Every layer flipped (see `LayerId::flipped`).
    pub fn flipped(self, copper_layers: u8) -> LayerSet {
        self.iter().fold(LayerSet::EMPTY, |s, l| s.with(l.flipped(copper_layers)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names, kinds, sides, flips and sets, across the whole 256-identity range.
    #[test]
    fn identities_flips_and_sets() {
        for n in 0u16..256 {
            let l = LayerId(n as u8);
            assert_eq!(LayerId::parse(&l.name()), Some(l), "name round trip of {n}");
            let mut one = LayerSet::EMPTY.with(l);
            assert!(one.has(l) && one.len() == 1, "set holds identity {n}");
            one = one.without(l);
            assert!(one.is_empty(), "set drops identity {n}");
        }
        assert_eq!(LayerId::B_CU.name(), "B.Cu", "back copper is 127");
        assert_eq!(LayerId::inner(126).name(), "In126.Cu", "the last inner layer");
        assert_eq!(LayerId::F_SILK.kind(), LayerKind::Silk, "kind");
        assert_eq!(LayerId(150).kind(), LayerKind::Unassigned, "unassigned range");
        assert_eq!(LayerId::user(95).kind(), LayerKind::User, "last user layer");
        assert_eq!(LayerId::B_MASK.side(), LayerSide::Back, "side");
        assert_eq!(LayerId::F_SILK.on(Side::Bottom), LayerId::B_SILK, "technical layer on a side");
        assert_eq!(LayerId::F_CU.on(Side::Bottom), LayerId::B_CU, "copper on a side");
        assert_eq!(LayerId::F_CU.flipped(4), LayerId::B_CU, "front copper flips to back");
        assert_eq!(LayerId::inner(1).flipped(4), LayerId::inner(2), "inner copper mirrors through the stack");
        assert_eq!(LayerId::inner(1).flipped(128), LayerId::inner(126), "a 128-layer stack mirrors end to end");
        assert_eq!(LayerId::F_PASTE.flipped(4), LayerId::B_PASTE, "technical pairs swap");
        assert_eq!(LayerId::EDGE.flipped(4), LayerId::EDGE, "edge stays");
        let cu = LayerSet::copper(4);
        assert_eq!(cu.iter().collect::<Vec<_>>(), vec![LayerId::F_CU, LayerId::inner(1), LayerId::inner(2), LayerId::B_CU], "4-layer copper");
        assert_eq!(cu.flipped(4), cu, "copper set is symmetric");
        assert_eq!(LayerSet::copper(2).len(), 2, "2-layer copper");
        assert_eq!(LayerSet::copper(128), LayerSet::ALL_COPPER, "128 copper layers fill the copper range");
        let s = LayerSet::of(&[LayerId::F_CU, LayerId::F_MASK, LayerId::user(95)]);
        assert_eq!(s.copper_only(), LayerSet::one(LayerId::F_CU), "copper part of a set");
        assert_eq!(s.minus(LayerSet::ALL_COPPER).len(), 2, "difference");
        assert!(s.intersect(cu).has(LayerId::F_CU) && !s.intersect(cu).has(LayerId::F_MASK), "intersection");
    }
}

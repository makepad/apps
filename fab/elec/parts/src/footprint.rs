//! Footprints: land patterns with pad stacks, drawn on the component side (front). Placement
//! on the bottom flips them (`fab_elec_board::Placed`).

use crate::PadNo;
use fab_elec_core::{canon_enum, canon_struct, canon_tags, Angle, BBox, Contour, LayerId, LayerSet, Nm, PadIx, Path, PolySet, Seg, Shape, P};

/// Polygon error for pad shapes that are not primitives (chamfers, rounded customs): 1 µm.
pub const PAD_POLY_ERR: Nm = 1_000;

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Footprint {
    pub name: String,
    pub pads: Vec<Pad>,
    pub graphics: Vec<(LayerId, Gfx)>,
    pub texts: Vec<FpText>,
    /// The front courtyard (`F.CrtYd`).
    pub courtyard: PolySet,
    pub keepouts: Vec<Keepout>,
    /// The placement reference point (pick-and-place centre), footprint coordinates.
    pub anchor: P,
    pub attrs: FpAttrs,
}
canon_struct!(Footprint { name, pads, graphics, texts, courtyard, keepouts, anchor, attrs });

#[derive(Clone, PartialEq, Debug)]
pub struct Pad {
    pub no: PadNo,
    pub kind: PadKind,
    pub stack: PadStack,
    pub at: P,
    pub rot: Angle,
    pub drill: Option<Drill>,
    /// Solder mask expansion; `None` takes the board rule.
    pub mask_margin: Option<Nm>,
    pub paste: PasteRule,
    /// Thermal relief; `None` takes the zone's.
    pub thermal: Option<ThermalRule>,
    /// How zones connect; `None` takes the zone's.
    pub zone_connect: Option<ZoneConnect>,
    /// Pad-to-die length for length matching.
    pub die_len: Nm,
    pub prop: PadProp,
}
canon_struct!(Pad { no, kind, stack, at, rot, drill, mask_margin, paste, thermal, zone_connect, die_len, prop });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum PadKind {
    Smd,
    /// Plated through hole.
    Thru,
    /// Non-plated hole (no copper).
    Npth,
    /// An edge contact (copper, no paste).
    Edge,
}
canon_tags!(PadKind);

/// Which layers a pad is on and its shape per copper layer. `front` is the component side's
/// outer copper, `back` the far side's, `inner` every inner layer; `None` repeats `front`.
#[derive(Clone, PartialEq, Debug)]
pub struct PadStack {
    pub copper: LayerSet,
    /// Mask and paste openings (`F.Mask`, `F.Paste`, ... in front terms).
    pub tech: LayerSet,
    pub front: PadShape,
    pub inner: Option<PadShape>,
    pub back: Option<PadShape>,
}
canon_struct!(PadStack { copper, tech, front, inner, back });

impl PadStack {
    /// One shape on front copper with its mask and paste: a surface pad.
    pub fn smd(shape: PadShape) -> PadStack {
        PadStack { copper: LayerSet::one(LayerId::F_CU), tech: LayerSet::of(&[LayerId::F_MASK, LayerId::F_PASTE]), front: shape, inner: None, back: None }
    }
    /// One shape on every copper layer with mask both sides: a through pad.
    pub fn thru(shape: PadShape) -> PadStack {
        PadStack { copper: LayerSet::ALL_COPPER, tech: LayerSet::of(&[LayerId::F_MASK, LayerId::B_MASK]), front: shape, inner: None, back: None }
    }
    /// The shape on copper layer `l` (footprint terms), `None` when the pad is not there.
    pub fn shape_on(&self, l: LayerId) -> Option<&PadShape> {
        if !self.copper.has(l) {
            return None;
        }
        Some(match l {
            LayerId::F_CU => &self.front,
            LayerId::B_CU => self.back.as_ref().unwrap_or(&self.front),
            _ => self.inner.as_ref().unwrap_or(&self.front),
        })
    }
}

/// Pad shapes. Sizes are full widths and heights; ratios are parts per million of the smaller
/// side.
#[derive(Clone, PartialEq, Debug)]
pub enum PadShape {
    Circle { d: Nm },
    Rect { size: P },
    RoundRect { size: P, ratio_ppm: u32 },
    /// Chamfered corners (`corners` bits: 1 top left, 2 top right, 4 bottom left, 8 bottom
    /// right), the others rounded by `ratio_ppm`.
    Chamfer { size: P, ratio_ppm: u32, chamfer_ppm: u32, corners: u8 },
    Oval { size: P },
    /// The top edge is `size.x - delta.y` wide and the bottom `size.x + delta.y`; the right edge
    /// is `size.y - delta.x` tall and the left `size.y + delta.x`.
    Trapezoid { size: P, delta: P },
    /// The anchor shape united with free shapes (pad coordinates).
    Custom { anchor: Box<PadShape>, shapes: Vec<Shape> },
}
canon_enum!(PadShape {
    0 => Circle { d }, 1 => Rect { size }, 2 => RoundRect { size, ratio_ppm }, 3 => Chamfer { size, ratio_ppm, chamfer_ppm, corners },
    4 => Oval { size }, 5 => Trapezoid { size, delta }, 6 => Custom { anchor, shapes },
});

fn ppm(v: Nm, ppm: u32) -> Nm {
    (v as i64 * ppm as i64 / 1_000_000) as Nm
}

impl PadShape {
    /// The shape placed at `at`, turned by `rot` (footprint coordinates in, same frame out).
    pub fn shape(&self, at: P, rot: Angle) -> Shape {
        let place = |p: P| p.rotated(rot).add(at);
        match self {
            PadShape::Circle { d } => Shape::Circle { c: at, r: d / 2 },
            PadShape::Rect { size } => Shape::Rect { c: at, half: P::new(size.x / 2, size.y / 2), rot, corner: 0 },
            PadShape::RoundRect { size, ratio_ppm } => {
                Shape::Rect { c: at, half: P::new(size.x / 2, size.y / 2), rot, corner: ppm(size.x.min(size.y), *ratio_ppm) }
            }
            PadShape::Oval { size } => Shape::Oval { c: at, half: P::new(size.x / 2, size.y / 2), rot },
            PadShape::Trapezoid { size, delta } => {
                let (hx, hy, dx, dy) = (size.x / 2, size.y / 2, delta.x / 2, delta.y / 2);
                let ring = vec![
                    place(P::new(-hx - dy, -hy - dx)),
                    place(P::new(hx + dy, -hy + dx)),
                    place(P::new(hx - dy, hy - dx)),
                    place(P::new(-hx + dy, hy + dx)),
                ];
                Shape::Poly(PolySet::from_ring_nonzero(ring))
            }
            PadShape::Chamfer { size, ratio_ppm, chamfer_ppm, corners } => {
                let (hx, hy) = (size.x / 2, size.y / 2);
                let side = size.x.min(size.y);
                let ch = ppm(side, *chamfer_ppm);
                let rr = ppm(side, *ratio_ppm);
                // corners counter-clockwise from bottom left: (point, bit, incoming dir, outgoing dir)
                let cs = [(P::new(-hx, -hy), 4u8), (P::new(hx, -hy), 8), (P::new(hx, hy), 2), (P::new(-hx, hy), 1)];
                let mut pts: Vec<(P, Option<P>)> = Vec::new(); // (point, arc mid to it)
                for (i, (c, bit)) in cs.iter().enumerate() {
                    let prev = cs[(i + 3) % 4].0;
                    let next = cs[(i + 1) % 4].0;
                    let toward = |from: P, to: P, d: Nm| {
                        let v = to.sub(from);
                        let l = v.len().max(1.0);
                        from.add(P::from_f(v.x as f64 * d as f64 / l, v.y as f64 * d as f64 / l))
                    };
                    if corners & bit != 0 && ch > 0 {
                        pts.push((toward(*c, prev, ch), None));
                        pts.push((toward(*c, next, ch), None));
                    } else if rr > 0 {
                        let a = toward(*c, prev, rr);
                        let b = toward(*c, next, rr);
                        let centre = a.add(b).sub(*c);
                        let m = toward(centre, *c, rr);
                        pts.push((a, None));
                        pts.push((b, Some(m)));
                    } else {
                        pts.push((*c, None));
                    }
                }
                let mut path = Path::new(place(pts[0].0));
                for (p, mid) in pts.iter().skip(1).chain(std::iter::once(&pts[0])) {
                    match mid {
                        Some(m) => path.segs.push(Seg::Arc { mid: place(*m), end: place(*p) }),
                        None => path.segs.push(Seg::Line(place(*p))),
                    }
                }
                Shape::Poly(PolySet::from_ring_nonzero(Contour::new(path).to_ring(PAD_POLY_ERR)))
            }
            PadShape::Custom { anchor, shapes } => {
                let mut set = anchor.shape(at, rot).to_polyset(PAD_POLY_ERR);
                for s in shapes {
                    set = set.union(&s.rotated_about(P::ZERO, rot).translated(at).to_polyset(PAD_POLY_ERR));
                }
                Shape::Poly(set)
            }
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Drill {
    /// `x == y` a round hole, otherwise a slot of that size.
    pub size: P,
    /// Offset of the hole from the pad centre (pad coordinates).
    pub offset: P,
}
canon_struct!(Drill { size, offset });

#[derive(Clone, PartialEq, Debug, Default)]
pub struct PasteRule {
    pub margin: Option<Nm>,
    pub ratio_ppm: Option<i32>,
}
canon_struct!(PasteRule { margin, ratio_ppm });

#[derive(Clone, PartialEq, Debug)]
pub struct ThermalRule {
    pub gap: Nm,
    pub spoke: Nm,
    pub angle: Angle,
}
canon_struct!(ThermalRule { gap, spoke, angle });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum ZoneConnect {
    #[default]
    Thermal,
    Solid,
    /// Thermal reliefs on through pads only, surface pads solid.
    ThruThermal,
    Off,
}
canon_tags!(ZoneConnect);

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum PadProp {
    #[default]
    None,
    Bga,
    FiducialLocal,
    FiducialGlobal,
    TestPoint,
    HeatSink,
    Castellated,
    Mechanical,
}
canon_tags!(PadProp);

/// A footprint or board graphic: a stroked path (lines and arcs) or a filled shape.
#[derive(Clone, PartialEq, Debug)]
pub enum Gfx {
    Stroke { path: Path, w: Nm },
    Fill(Shape),
}
canon_enum!(Gfx { 0 => Stroke { path, w }, 1 => Fill(s) });

impl Gfx {
    /// The filled shapes the graphic covers (strokes as round-ended segments and arcs).
    pub fn shapes(&self) -> Vec<Shape> {
        match self {
            Gfx::Fill(s) => vec![s.clone()],
            Gfx::Stroke { path, w } => path
                .steps()
                .map(|(a, seg)| match seg {
                    Seg::Line(b) => Shape::Segment { a, b, w: *w },
                    Seg::Arc { mid, end } => Shape::Arc { a, mid, b: end, w: *w },
                })
                .collect(),
        }
    }
    pub fn bbox(&self) -> BBox {
        match self {
            Gfx::Fill(s) => s.bbox(),
            Gfx::Stroke { path, w } => path.bbox().inflate(w / 2),
        }
    }
    pub fn mapped(&self, f: impl Fn(&Shape) -> Shape, g: impl Fn(&Path) -> Path) -> Gfx {
        match self {
            Gfx::Fill(s) => Gfx::Fill(f(s)),
            Gfx::Stroke { path, w } => Gfx::Stroke { path: g(path), w: *w },
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum FpTextKind {
    Reference,
    Value,
    User,
}
canon_tags!(FpTextKind);

#[derive(Clone, PartialEq, Debug)]
pub struct FpText {
    pub kind: FpTextKind,
    /// The text; `Reference` and `Value` texts are replaced by the component's at placement.
    pub text: String,
    pub layer: LayerId,
    pub at: P,
    pub rot: Angle,
    pub size: Nm,
    pub thick: Nm,
    pub visible: bool,
}
canon_struct!(FpText { kind, text, layer, at, rot, size, thick, visible });

/// What a keepout forbids (bits).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Forbid(pub u8);
canon_struct!(Forbid { 0 });

impl Forbid {
    pub const TRACKS: Forbid = Forbid(1);
    pub const VIAS: Forbid = Forbid(2);
    pub const PADS: Forbid = Forbid(4);
    pub const ZONES: Forbid = Forbid(8);
    pub const FOOTPRINTS: Forbid = Forbid(16);
    pub const ALL: Forbid = Forbid(31);
    pub fn has(self, o: Forbid) -> bool {
        self.0 & o.0 == o.0
    }
    pub fn with(self, o: Forbid) -> Forbid {
        Forbid(self.0 | o.0)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Keepout {
    pub area: PolySet,
    pub layers: LayerSet,
    pub forbid: Forbid,
    pub reason: String,
}
canon_struct!(Keepout { area, layers, forbid, reason });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum FpMount {
    #[default]
    Smd,
    Thru,
    Other,
}
canon_tags!(FpMount);

#[derive(Clone, PartialEq, Debug, Default)]
pub struct FpAttrs {
    pub mount: FpMount,
    pub exclude_bom: bool,
    pub exclude_pos: bool,
    /// On the board only (a logo, a test pad): no schematic component.
    pub board_only: bool,
}
canon_struct!(FpAttrs { mount, exclude_bom, exclude_pos, board_only });

impl Footprint {
    /// The pad numbered `no` (the first, when a number repeats).
    pub fn pad(&self, no: &str) -> Option<PadIx> {
        self.pads.iter().position(|p| p.no == no).map(|i| PadIx(i as u16))
    }
    /// The bounding box of pads, courtyard and graphics, footprint coordinates.
    pub fn bbox(&self) -> BBox {
        let mut b = self.courtyard.bbox();
        for p in &self.pads {
            if let Some(s) = p.stack.shape_on(LayerId::F_CU).or_else(|| p.stack.copper.iter().next().and_then(|l| p.stack.shape_on(l))) {
                b = b.union(s.shape(p.at, p.rot).bbox());
            }
            if let Some(d) = &p.drill {
                b = b.union(BBox::point(p.at.add(d.offset)).inflate(d.size.x.max(d.size.y) / 2));
            }
        }
        for (_, g) in &self.graphics {
            b = b.union(g.bbox());
        }
        b
    }
}

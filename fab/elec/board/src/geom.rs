//! Item geometry on the board: placed pads, copper shapes per layer, bounding boxes.
//!
//! Cost: O(1) per pad or segment; `copper_shapes` of a placement is O(pads), of a track
//! O(segments). Opening queries search placements linearly; non-primitive and NPTH shapes
//! additionally use polygon operations.

use crate::text::{shown_text, text_bound, text_pose};
use crate::{Board, ConnItem, Hole, Item, ItemExtent, Placed, RuleSet, Tent, Track, Via};
use fab_elec_core::{Angle, BBox, ClassId, ItemId, LayerId, LayerSet, NetId, Nm, PadIx, PolySet, Seg, Shape, Side, P};
use fab_elec_parts::{footprint::PAD_POLY_ERR, Pad, PadKind};

/// How far a pad's own thermal relief reaches past its box; `None` when it uses its zone's.
pub fn pad_relief(pad: &Pad) -> Option<Nm> {
    pad.thermal.as_ref().map(|t| 2 * t.gap + t.spoke)
}

impl Placed {
    /// The placement box united with visible text bounds, without laying out strokes.
    pub(crate) fn text_extent(&self) -> BBox {
        self.texts().filter(|t| t.visible).fold(self.bbox(), |b, t| {
            let text = shown_text(self, t);
            b.union(text_bound(self.to_board(text_pose(self, t).0), text, t.size, t.thick))
        })
    }

    /// The largest outward mask margin of this placement's pads.
    pub(crate) fn mask_growth(&self, board: &Board) -> Nm {
        let margin = board.mask_margin();
        self.footprint.pads.iter().map(|p| p.mask_margin.unwrap_or(margin)).max().unwrap_or(0).max(0)
    }

    /// A footprint point in the board frame.
    pub fn to_board(&self, p: P) -> P {
        let p = if self.side == Side::Bottom { P::new(-p.x, p.y) } else { p };
        p.rotated(self.rot).add(self.at)
    }
    /// A footprint shape in the board frame.
    pub fn shape_to_board(&self, s: &Shape) -> Shape {
        let s = if self.side == Side::Bottom { s.mirrored_x(0) } else { s.clone() };
        s.rotated_about(P::ZERO, self.rot).translated(self.at)
    }
    /// A board layer in the footprint's terms (and back: the map is its own inverse).
    pub fn fp_layer(&self, l: LayerId, copper_count: u8) -> LayerId {
        if self.side == Side::Bottom { l.flipped(copper_count) } else { l }
    }
    /// The centre of pad `pad` in the board frame.
    pub fn pad_at(&self, pad: PadIx) -> Option<P> {
        Some(self.to_board(self.footprint.pads.get(pad.ix())?.at))
    }
    /// The copper layers pad `pad` is on, board terms, within `copper` (the board's copper).
    pub fn pad_layers(&self, pad: PadIx, copper: LayerSet, copper_count: u8) -> LayerSet {
        let Some(p) = self.footprint.pads.get(pad.ix()) else { return LayerSet::EMPTY };
        copper.iter().filter(|l| p.stack.copper.has(self.fp_layer(*l, copper_count))).fold(LayerSet::EMPTY, |s, l| s.with(l))
    }
    /// Pad `pad`'s copper shape on board layer `l`.
    pub fn pad_shape(&self, pad: PadIx, l: LayerId, copper_count: u8) -> Option<Shape> {
        let p = self.footprint.pads.get(pad.ix())?;
        let shape = p.stack.shape_on(self.fp_layer(l, copper_count))?.shape(p.at, p.rot);
        Some(self.shape_to_board(&shape))
    }
    /// The bounding box of the placement (pads, courtyard, graphics) in the board frame.
    pub fn bbox(&self) -> BBox {
        let b = self.footprint.bbox();
        if b.is_empty() {
            return BBox::point(self.at);
        }
        BBox::of_points([b.min, P::new(b.max.x, b.min.y), b.max, P::new(b.min.x, b.max.y)].map(|p| self.to_board(p)))
    }
}

impl Track {
    /// Segment `i` as a filled shape.
    pub fn seg_shape(&self, i: usize, a: P, seg: &Seg) -> Shape {
        let w = self.seg_width(i);
        match *seg {
            Seg::Line(b) => Shape::Segment { a, b, w },
            Seg::Arc { mid, end } => Shape::Arc { a, mid, b: end, w },
        }
    }
    pub fn bbox(&self) -> BBox {
        self.path.bbox().inflate(self.max_width() / 2 + 1)
    }
}

impl Via {
    /// The copper layers the via's drill spans, within the board's copper. Every layer here has the barrel;
    /// which of them also carry a ring is the via's flashing.
    pub fn span_layers(&self, copper: LayerSet) -> LayerSet {
        let (a, b) = if self.span.0 <= self.span.1 { self.span } else { (self.span.1, self.span.0) };
        copper.iter().filter(|l| a <= *l && *l <= b).fold(LayerSet::EMPTY, |s, l| s.with(l))
    }
    pub fn shape(&self) -> Shape {
        Shape::Circle { c: self.at, r: self.pad / 2 }
    }
}

/// One piece of copper: what connectivity, DRC, fills and the router test against each other.
#[derive(Clone, PartialEq, Debug)]
pub struct Copper {
    pub item: ConnItem,
    pub net: Option<NetId>,
    /// The net's class (the default class for no net): with the item kind, the rule key.
    pub class: ClassId,
    pub layer: LayerId,
    pub shape: Shape,
}

impl Copper {
    pub fn key(&self) -> crate::RuleKey {
        crate::RuleKey { class: self.class, item: crate::zone::rule_item(self.item) }
    }
    /// Both pieces belong to one item (one pad, one track, one via, one hole, one zone).
    pub fn same_item(&self, o: &Copper) -> bool {
        match (self.item, o.item) {
            (ConnItem::Pad { placed: a, pad: x }, ConnItem::Pad { placed: b, pad: y }) => a == b && x == y,
            (ConnItem::Pad { .. }, _) | (_, ConnItem::Pad { .. }) => false,
            (a, b) => a.item() == b.item(),
        }
    }
}

impl Board {
    /// The signed mask margin inherited by pads and vias.
    pub(crate) fn mask_margin(&self) -> Nm {
        self.rules.base.mask_margin.or(RuleSet::standard().mask_margin).unwrap_or(0)
    }

    /// Every piece of fixed copper of an item on every board layer it is on (zones excluded:
    /// their copper is their fill).
    pub fn item_copper(&self, id: ItemId, out: &mut Vec<Copper>) {
        let copper = self.copper_set();
        let n = self.copper_count();
        let Some((kind, i)) = self.locate(id) else { return };
        let class = |net| crate::zone::class_of(self, net);
        match kind {
            crate::ItemKind::Placed => {
                let p = &self.placed[i];
                for pi in 0..p.footprint.pads.len() {
                    let pix = PadIx(pi as u16);
                    let net = p.pad_nets.get(pi).copied().flatten();
                    for l in p.pad_layers(pix, copper, n).iter() {
                        if let Some(shape) = p.pad_shape(pix, l, n) {
                            out.push(Copper { item: ConnItem::Pad { placed: p.id, pad: pix }, net, class: class(net), layer: l, shape });
                        }
                    }
                }
            }
            crate::ItemKind::Track => {
                let t = &self.tracks[i];
                for (si, (a, seg)) in t.path.steps().enumerate() {
                    out.push(Copper { item: ConnItem::Track { item: t.id, seg: si as u32 }, net: Some(t.net), class: class(Some(t.net)), layer: t.layer, shape: t.seg_shape(si, a, &seg) });
                }
            }
            crate::ItemKind::Via => {
                let v = &self.vias[i];
                for l in self.rings(v).fixed.iter() {
                    out.push(Copper { item: ConnItem::Via(v.id), net: Some(v.net), class: class(Some(v.net)), layer: l, shape: v.shape() });
                }
            }
            crate::ItemKind::Hole => {
                let h = &self.holes[i];
                if let (true, Some(pad)) = (h.plated, h.pad) {
                    for l in copper.iter() {
                        out.push(Copper { item: ConnItem::Hole(h.id), net: h.net, class: class(h.net), layer: l, shape: Shape::Circle { c: h.at, r: pad / 2 } });
                    }
                }
            }
            _ => {}
        }
    }
    /// All fixed copper of the board, in item order.
    pub fn all_copper(&self) -> Vec<Copper> {
        let mut out = Vec::new();
        let ids: Vec<ItemId> = self
            .placed
            .iter()
            .map(|x| x.id)
            .chain(self.tracks.iter().map(|x| x.id))
            .chain(self.vias.iter().map(|x| x.id))
            .chain(self.holes.iter().map(|x| x.id))
            .collect();
        for id in ids {
            self.item_copper(id, &mut out);
        }
        out
    }
    /// Where an anchor puts a track end, including a junction's position on its host's
    /// centreline; `None` for `Free` anchors and missing items.
    pub fn anchor_pos(&self, a: crate::Anchor) -> Option<P> {
        match a {
            crate::Anchor::Pad { placed, pad } => self.placed_item(placed)?.pad_at(pad),
            crate::Anchor::Via(v) => self.vias.iter().find(|x| x.id == v).map(|x| x.at),
            crate::Anchor::Hole(h) => self.holes.iter().find(|x| x.id == h).map(|x| x.at),
            crate::Anchor::Track { id, at } => {
                let (crate::ItemKind::Track, i) = self.locate(id)? else { return None };
                Some(self.tracks[i].path.point_on(at))
            }
            crate::Anchor::Free => None,
        }
    }
    /// An anchor at the nearest point to `p` on the host track's centreline.
    pub fn track_anchor(&self, host: ItemId, p: P) -> Option<crate::Anchor> {
        let (crate::ItemKind::Track, i) = self.locate(host)? else { return None };
        Some(crate::Anchor::Track { id: host, at: self.tracks[i].path.locate(p).0 })
    }
    /// Every drilled hole, in board item order: footprint pads, vias, holes. Zone fill, DRC, the
    /// router's world and the mechanical scene all read holes from here (one drill function).
    pub fn drills(&self) -> Vec<DrillHole> {
        let mut out = Vec::new();
        for p in &self.placed {
            out.extend((0..p.footprint.pads.len()).filter_map(|pi| p.drill_hole(PadIx(pi as u16))));
        }
        out.extend(self.vias.iter().map(Via::drill_hole));
        out.extend(self.holes.iter().map(Hole::drill_hole));
        out
    }

    /// The solder mask opening of pad `pad` of placed part `placed` on `side`, board frame: the pad's
    /// shape on that side's outer copper grown by its mask margin (the pad's, else the board rule's);
    /// for a non-plated hole, the drill's outline where it leaves the land. `None` when the pad has no
    /// mask opening on that side or the opening collapses.
    pub fn mask_opening(&self, placed: ItemId, pad: PadIx, side: Side) -> Option<Shape> {
        let p = self.placed_item(placed)?;
        let pad = p.footprint.pads.get(pad.ix())?;
        let n = self.copper_count();
        let has_mask = pad.stack.tech.has(p.fp_layer(LayerId::F_MASK.on(side), n));
        if !has_mask {
            return None;
        }
        let fp_copper = p.fp_layer(LayerId::F_CU.on(side), n);
        let def = if fp_copper == LayerId::B_CU { pad.stack.back.as_ref().unwrap_or(&pad.stack.front) } else { &pad.stack.front };
        let shape = def.shape(pad.at, pad.rot);
        let shape = if pad.kind == PadKind::Npth { npth_mask_shape(pad, shape) } else { shape };
        let margin = pad.mask_margin.unwrap_or_else(|| self.mask_margin());
        let open = opening(shape, margin, 0, pad.at)?;
        Some(p.shape_to_board(&open))
    }

    /// The paste opening, likewise: surface pads only; the pad's paste margin and ratio, else the board
    /// rule's.
    pub fn paste_opening(&self, placed: ItemId, pad: PadIx, side: Side) -> Option<Shape> {
        let p = self.placed_item(placed)?;
        let pad = p.footprint.pads.get(pad.ix())?;
        let n = self.copper_count();
        let has_paste = pad.kind == PadKind::Smd && pad.stack.tech.has(p.fp_layer(LayerId::F_PASTE.on(side), n));
        if !has_paste {
            return None;
        }
        let fp_copper = p.fp_layer(LayerId::F_CU.on(side), n);
        let def = if fp_copper == LayerId::B_CU { pad.stack.back.as_ref().unwrap_or(&pad.stack.front) } else { &pad.stack.front };
        let shape = def.shape(pad.at, pad.rot);
        let std = RuleSet::standard();
        let margin = pad.paste.margin.or(self.rules.base.paste_margin).or(std.paste_margin).unwrap_or(0);
        let ratio_ppm = pad.paste.ratio_ppm.or(self.rules.base.paste_ratio_ppm).or(std.paste_ratio_ppm).unwrap_or(0);
        let open = opening(shape, margin, ratio_ppm, pad.at)?;
        Some(p.shape_to_board(&open))
    }

    /// The solder mask opening of via `via` on `side`, board frame: its pad grown by the board's mask
    /// margin (else the standard rule's). `None` when the via is tented on that side, does not reach
    /// that side's outer copper, or the opening collapses.
    pub fn via_mask_opening(&self, via: ItemId, side: Side) -> Option<Shape> {
        let v = self.vias.iter().find(|v| v.id == via)?;
        let tented = matches!((v.tent, side), (Tent::Both, _) | (Tent::Front, Side::Top) | (Tent::Back, Side::Bottom));
        if tented || !v.span_layers(self.copper_set()).has(LayerId::F_CU.on(side)) {
            return None;
        }
        let margin = self.mask_margin();
        opening(v.shape(), margin, 0, v.at)
    }
}

/// A mechanical pad may have no land at all, or a drill offset beyond that land.
fn npth_mask_shape(pad: &Pad, shape: Shape) -> Shape {
    let Some(drill) = &pad.drill else { return shape };
    let c = pad.at.add(drill.offset.rotated(pad.rot));
    let hole = if drill.size.x == drill.size.y {
        Shape::Circle { c, r: drill.size.x / 2 }
    } else {
        Shape::Oval { c, half: P::new(drill.size.x / 2, drill.size.y / 2), rot: pad.rot }
    };
    let land = shape.to_polyset(PAD_POLY_ERR);
    if land.is_empty() {
        return hole;
    }
    let drilled = hole.to_polyset(PAD_POLY_ERR);
    if drilled.difference(&land).is_empty() { shape } else { Shape::Poly(land.union(&drilled)) }
}

/// Signed paste ratio scales the pad about its centre (negative shrinks), then the signed
/// margin moves each edge. Mask uses a zero ratio. Collapsed openings emit no aperture.
fn opening(shape: Shape, margin: Nm, ratio_ppm: i32, origin: P) -> Option<Shape> {
    let factor = 1_000_000i64 + ratio_ppm as i64;
    if factor <= 0 {
        return None;
    }
    let scale = |v: Nm| (v as i64 * factor / 1_000_000) as Nm;
    let point = |p: P| origin.add(P::new(scale(p.x - origin.x), scale(p.y - origin.y)));
    match shape {
        Shape::Circle { c, r } => {
            let r = scale(r) + margin;
            (r > 0).then(|| Shape::Circle { c: point(c), r })
        }
        Shape::Rect { c, half, rot, corner } => {
            let half = P::new(scale(half.x) + margin, scale(half.y) + margin);
            let corner = if corner == 0 { 0 } else { (scale(corner) + margin).max(0).min(half.x.min(half.y).max(0)) };
            (half.x > 0 && half.y > 0).then(|| Shape::Rect { c: point(c), half, rot, corner })
        }
        Shape::Oval { c, half, rot } => {
            let half = P::new(scale(half.x) + margin, scale(half.y) + margin);
            (half.x > 0 && half.y > 0).then(|| Shape::Oval { c: point(c), half, rot })
        }
        other => {
            let mut set = other.to_polyset(PAD_POLY_ERR);
            if ratio_ppm != 0 {
                let rings: Vec<Vec<P>> = set.rings().map(|r| r.iter().copied().map(point).collect()).collect();
                set = PolySet::from_rings_even_odd(&rings);
            }
            let set = set.offset(margin, PAD_POLY_ERR);
            (!set.is_empty()).then_some(Shape::Poly(set))
        }
    }
}

/// A drilled hole in the board frame. Round when `size.x == size.y`; otherwise a slot `size.x`
/// long along `rot` and `size.y` wide (a footprint drill's own axes, turned with its pad and part).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct DrillHole {
    /// The pad, via or hole that owns it.
    pub item: ConnItem,
    /// The owner's net (a pad's net, a via's net, a plated hole's net), if any.
    pub net: Option<NetId>,
    pub c: P,
    pub size: P,
    pub rot: Angle,
    pub plated: bool,
}

impl DrillHole {
    /// The hole as a shape: a circle, or an oval for a slot.
    pub fn shape(&self) -> Shape {
        if self.size.x == self.size.y {
            Shape::Circle { c: self.c, r: self.size.x / 2 }
        } else {
            Shape::Oval { c: self.c, half: P::new(self.size.x / 2, self.size.y / 2), rot: self.rot }
        }
    }
    /// The smaller dimension: a round hole's diameter, a slot's width.
    pub fn width(&self) -> Nm {
        self.size.x.min(self.size.y)
    }
}

impl Placed {
    /// Pad `pad`'s drilled hole in the board frame, `None` for a pad without a drill.
    pub fn drill_hole(&self, pad: PadIx) -> Option<DrillHole> {
        let p = self.footprint.pads.get(pad.ix())?;
        let d = p.drill.as_ref()?;
        // mirroring a bottom part negates the pad's angle (as `shape_to_board` does)
        let rot = if self.side == Side::Bottom { p.rot.neg() } else { p.rot }.add(self.rot);
        Some(DrillHole {
            item: ConnItem::Pad { placed: self.id, pad },
            net: self.pad_nets.get(pad.ix()).copied().flatten(),
            c: self.to_board(p.at.add(d.offset.rotated(p.rot))),
            size: d.size,
            rot,
            plated: p.kind == PadKind::Thru,
        })
    }
}

impl Hole {
    /// The hole itself.
    pub fn drill_hole(&self) -> DrillHole {
        DrillHole { item: ConnItem::Hole(self.id), net: self.net, c: self.at, size: P::new(self.drill, self.drill), rot: Angle::ZERO, plated: self.plated }
    }
}

impl Via {
    /// The via's drilled hole.
    pub fn drill_hole(&self) -> DrillHole {
        DrillHole { item: ConnItem::Via(self.id), net: Some(self.net), c: self.at, size: P::new(self.drill, self.drill), rot: Angle::ZERO, plated: true }
    }
}

impl Item {
    pub fn id(&self) -> ItemId {
        match self {
            Item::Placed(x) => x.id,
            Item::Track(x) => x.id,
            Item::Via(x) => x.id,
            Item::Zone(x) => x.id,
            Item::Keepout(x) => x.id,
            Item::Hole(x) => x.id,
            Item::Cutout(x) => x.id,
            Item::Graphic(x) => x.id,
            Item::Text(x) => x.id,
            Item::Tuning(x) => x.id,
        }
    }
    pub fn origin(&self) -> &crate::Origin {
        match self {
            Item::Placed(x) => &x.origin,
            Item::Track(x) => &x.origin,
            Item::Via(x) => &x.origin,
            Item::Zone(x) => &x.origin,
            Item::Keepout(x) => &x.origin,
            Item::Hole(x) => &x.origin,
            Item::Cutout(x) => &x.origin,
            Item::Graphic(x) => &x.origin,
            Item::Text(x) => &x.origin,
            Item::Tuning(x) => &x.origin,
        }
    }
    /// The item's extent in the board frame (copper, courtyard, area; texts approximately).
    pub fn bbox(&self, board: &Board) -> BBox {
        match self {
            Item::Placed(x) => x.bbox(),
            Item::Track(x) => x.bbox(),
            Item::Via(x) => BBox::point(x.at).inflate(x.pad.max(x.drill) / 2 + 1),
            Item::Zone(x) => match &x.area {
                crate::Area::Board => board.outline_bbox(),
                crate::Area::Poly(p) => p.bbox(),
            },
            Item::Keepout(x) => x.area.bbox(),
            Item::Hole(x) => BBox::point(x.at).inflate(x.pad.unwrap_or(0).max(x.drill) / 2 + 1),
            Item::Cutout(x) => x.contour.bbox(),
            Item::Graphic(x) => x.gfx.bbox(),
            Item::Text(x) => BBox::point(x.at).inflate(x.size * (x.text.chars().count() as Nm + 1)),
            // the tracks it hosts on and generated: what a change of its parameters reaches
            Item::Tuning(x) => x.host.iter().chain(&x.copper).filter_map(|id| board.tracks.iter().find(|t| t.id == *id)).fold(BBox::EMPTY, |b, t| b.union(t.bbox())),
        }
    }
    /// The box and copper layers read by zone fill. Placed pad copper and own-net reliefs use
    /// the pad layers; footprint keepouts forbidding zones add their board-frame boxes and mapped
    /// copper layers, since the placement box excludes keepouts. Tracks use their own layer.
    /// Pad and hole drills reach every copper layer; vias conservatively reach every copper layer.
    /// Higher-priority zone bases and board keepouts reach their copper layers. Cutouts change the
    /// outline on every layer; tuning conservatively reaches every layer through its host and
    /// generated tracks. Graphics and text are not read.
    pub fn fill_extent(&self, board: &Board) -> ItemExtent {
        let copper = board.copper_set();
        let mut bbox = self.bbox(board);
        let layers = match self {
            Item::Placed(x) => {
                let n = board.copper_count();
                let mut layers = LayerSet::EMPTY;
                for i in 0..x.footprint.pads.len() {
                    let pad = PadIx(i as u16);
                    layers = layers.union(x.pad_layers(pad, copper, n));
                    if x.drill_hole(pad).is_some() {
                        layers = copper;
                        break;
                    }
                }
                for keepout in &x.footprint.keepouts {
                    if keepout.forbid.has(fab_elec_parts::Forbid::ZONES) {
                        bbox = bbox.union(x.shape_to_board(&Shape::Poly(keepout.area.clone())).bbox());
                        for layer in copper.iter() {
                            if keepout.layers.has(x.fp_layer(layer, n)) {
                                layers = layers.with(layer);
                            }
                        }
                    }
                }
                layers
            }
            Item::Track(x) => LayerSet::one(x.layer),
            Item::Via(_) | Item::Hole(_) | Item::Cutout(_) | Item::Tuning(_) => copper,
            Item::Zone(x) => x.layers.intersect(copper),
            Item::Keepout(x) => x.layers.intersect(copper),
            Item::Graphic(_) | Item::Text(_) => LayerSet::EMPTY,
        };
        // A pad's own thermal relief reaches past the zone's (which the fill's reach covers).
        let relief = match self {
            Item::Placed(x) => x.footprint.pads.iter().filter_map(pad_relief).max().unwrap_or(0),
            _ => 0,
        };
        ItemExtent { bbox: bbox.inflate(relief), layers }
    }
    /// Everything the item puts on the board (copper, courtyard, silk, mask openings), and what
    /// a change of it can affect. The DRC merge rule relies on this extent containing it all.
    pub fn extent(&self, board: &Board) -> BBox {
        let b = self.bbox(board);
        match self {
            Item::Placed(p) => b.inflate(p.mask_growth(board)).union(p.text_extent()),
            Item::Via(_) => b.inflate(board.mask_margin().max(0)),
            Item::Text(t) => b.union(text_bound(t.at, &t.text, t.size, t.thick)),
            _ => b,
        }
    }
    /// The nets the item carries.
    pub fn nets(&self) -> Vec<NetId> {
        match self {
            Item::Placed(x) => {
                let mut v: Vec<NetId> = x.pad_nets.iter().flatten().copied().collect();
                v.sort();
                v.dedup();
                v
            }
            Item::Track(x) => vec![x.net],
            Item::Via(x) => vec![x.net],
            Item::Zone(x) => x.net.into_iter().collect(),
            Item::Hole(x) => x.net.into_iter().collect(),
            Item::Tuning(x) => x.pair.into_iter().chain([x.net]).collect(),
            _ => Vec::new(),
        }
    }
}

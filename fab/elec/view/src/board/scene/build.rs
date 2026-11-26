use super::{DrawScene, Role, SceneLayer, ScenePoly, SceneShape, Tag, OUTLINE_ITEM};
use fab_elec_board::{Area, Board, ConnItem, DrillHole, ItemKind, ZoneFill};
use fab_elec_core::{BBox, ItemId, LayerId, LayerSet, NetId, Nm, PadIx, Path, PolySet, Seg, Shape, Side, P};
use fab_elec_board::text::{footprint_text, text_strokes};

/// Maximum outline arc flattening error in nanometres.
const OUTLINE_ARC_ERR: Nm = 2_000;

pub(super) fn scene_of(board: &Board, fills: &[ZoneFill]) -> DrawScene {
    let mut out = Builder::new(board.outline_bbox());
    out.items(board, fills, None);
    let tag = Tag { item: OUTLINE_ITEM, sub: 0, role: Role::Outline };
    for contour in &board.outline {
        for shape in path_strokes(&contour.path, 100_000) {
            out.push(LayerId::EDGE, shape, None, tag, None);
        }
    }
    let mut scene = out.finish(board.generation);
    scene.outline = board.outline.clone();
    scene.substrate = board.outline_polyset(OUTLINE_ARC_ERR);
    scene
}

/// Only the requested items, in the same list/fill order as the full scene.
/// `Board::locate` uses the board's index, with its usual fallback for direct list edits.
pub fn scene_of_items(board: &Board, fills: &[ZoneFill], items: &[ItemId]) -> DrawScene {
    let mut selected: [Vec<usize>; ITEM_KINDS.len()] = std::array::from_fn(|_| Vec::new());
    for &id in items {
        let Some((kind, index)) = board.locate(id) else { continue };
        if let Some(slot) = ITEM_KINDS.iter().position(|&k| k == kind) {
            selected[slot].push(index);
        }
    }
    for indices in &mut selected {
        indices.sort_unstable();
        indices.dedup();
    }
    let mut out = Builder::new(BBox::EMPTY);
    out.items(board, fills, Some(&selected));
    // Unlike the full scene's historical box, the partial also bounds bare drills.
    for (drill, _) in &out.drills {
        out.bbox = out.bbox.union(drill.shape().bbox());
    }
    out.finish(board.generation)
}

/// Only fill tiles meeting the region's interior, in the full scene's order.
pub fn scene_of_fill_region(board: &Board, fill: &ZoneFill, region: BBox) -> DrawScene {
    let mut out = Builder::new(BBox::EMPTY);
    if let Some((ItemKind::Zone, index)) = board.locate(fill.zone) {
        let zone = &board.zones[index];
        let tag = Tag { item: zone.id, sub: 0, role: Role::Zone };
        out.fill(fill, Some(region), tag, zone.net);
    }
    out.finish(board.generation)
}

// This is the historical scene order, independent of ItemId allocation order.
const ITEM_KINDS: [ItemKind; 9] = [
    ItemKind::Placed, ItemKind::Track, ItemKind::Via, ItemKind::Hole,
    ItemKind::Zone, ItemKind::Graphic, ItemKind::Text, ItemKind::Keepout, ItemKind::Cutout,
];

struct Builder {
    // LayerId is u8: direct buckets preserve insertion order and need no sorting.
    layers: [SceneLayer; 256],
    bbox: BBox,
    drills: Vec<(DrillHole, Tag)>,
}

impl Builder {
    fn new(bbox: BBox) -> Self {
        Self { layers: std::array::from_fn(|_| SceneLayer::default()), bbox, drills: Vec::new() }
    }

    fn finish(self, generation: u64) -> DrawScene {
        DrawScene {
            layers: self.layers.into_iter().enumerate()
                .filter(|(_, l)| !l.shapes.is_empty() || !l.polys.is_empty())
                .map(|(i, l)| (LayerId(i as u8), l)).collect(),
            drills: self.drills,
            bbox: self.bbox,
            generation,
            ..DrawScene::default()
        }
    }

    fn drill(&mut self, drill: DrillHole) {
        if let Some(tag) = drill_tag(drill.item) {
            self.drills.push((drill, tag));
        }
    }

    fn items(&mut self, board: &Board, fills: &[ZoneFill], selected: Option<&[Vec<usize>; ITEM_KINDS.len()]>) {
        let copper = board.stackup.copper();
        let lengths = [board.placed.len(), board.tracks.len(), board.vias.len(), board.holes.len(),
            board.zones.len(), board.graphics.len(), board.texts.len(), board.keepouts.len(), board.cutouts.len()];
        for (slot, kind) in ITEM_KINDS.into_iter().enumerate() {
            let indices = (0..if selected.is_none() { lengths[slot] } else { 0 })
                .chain(selected.into_iter().flat_map(|lists| lists[slot].iter().copied()));
            if kind == ItemKind::Zone {
                // Fills precede unfilled zone outlines, in input order (including repeated fills).
                let mut filled = std::collections::BTreeSet::new();
                let matched = if let Some(lists) = selected {
                    if lists[slot].is_empty() { continue; }
                    let zones: std::collections::BTreeMap<_, _> = lists[slot].iter()
                        .map(|&i| (board.zones[i].id, i)).collect();
                    fills.iter().map(|f| zones.get(&f.zone).copied()).collect()
                } else {
                    fill_zones(board, fills)
                };
                for (fill, index) in fills.iter().zip(matched) {
                    let Some(index) = index else { continue };
                    filled.insert(index);
                    let zone = &board.zones[index];
                    let tag = Tag { item: zone.id, sub: 0, role: Role::Zone };
                    self.fill(fill, None, tag, zone.net);
                }
                for index in indices.filter(|i| !filled.contains(i)) {
                    self.item(board, kind, index, &copper);
                }
            } else {
                for index in indices {
                    self.item(board, kind, index, &copper);
                }
            }
        }
    }

    fn item(&mut self, board: &Board, kind: ItemKind, index: usize, copper: &[LayerId]) {
        let copper_set = LayerSet::of(copper);
        let n = copper.len() as u8;
        match kind {
            ItemKind::Placed => {
                let placed = &board.placed[index];
                for i in 0..placed.footprint.pads.len() {
                    let tag = Tag { item: placed.id, sub: i as u32, role: Role::Pad };
                    let net = placed.pad_nets.get(i).copied().flatten();
                    let ix = PadIx(i as u16);
                    let hole = placed.drill_hole(ix);
                    if let Some(drill) = hole { self.drill(drill); }
                    for l in placed.pad_layers(ix, copper_set, n).iter() {
                        let Some(shape) = placed.pad_shape(ix, l, n) else { continue };
                        self.push(l, shape, hole, tag, net);
                    }
                    for side in [Side::Top, Side::Bottom] {
                        if let Some(open) = board.mask_opening(placed.id, ix, side) {
                            self.push(LayerId::F_MASK.on(side), open, None, Tag { role: Role::MaskOpening, ..tag }, net);
                        }
                        if let Some(open) = board.paste_opening(placed.id, ix, side) {
                            self.push(LayerId::F_PASTE.on(side), open, None, Tag { role: Role::Paste, ..tag }, net);
                        }
                    }
                }
                for (i, (layer, gfx)) in placed.footprint.graphics.iter().enumerate() {
                    let tag = Tag { item: placed.id, sub: i as u32, role: Role::Graphic };
                    for shape in gfx.shapes() {
                        self.push(placed.fp_layer(*layer, n), placed.shape_to_board(&shape), None, tag, None);
                    }
                }
                let tag = Tag { item: placed.id, sub: 0, role: Role::Graphic };
                for ring in placed.footprint.courtyard.rings() {
                    for shape in ring_strokes(ring, 50_000) {
                        self.push(LayerId::F_CRTYD.on(placed.side), placed.shape_to_board(&shape), None, tag, None);
                    }
                }
                for (i, fp) in placed.texts().enumerate() {
                    if !fp.visible {
                        continue;
                    }
                    let (layer, strokes) = footprint_text(placed, fp, n);
                    let tag = Tag { item: placed.id, sub: i as u32, role: Role::Text };
                    for shape in strokes {
                        self.push(layer, shape, None, tag, None);
                    }
                }
            }
            ItemKind::Track => {
                let track = &board.tracks[index];
                for (j, (a, seg)) in track.path.steps().enumerate() {
                    let tag = Tag { item: track.id, sub: j as u32, role: Role::Track };
                    self.push(track.layer, track.seg_shape(j, a, &seg), None, tag, Some(track.net));
                }
            }
            ItemKind::Via => {
                let via = &board.vias[index];
                let tag = Tag { item: via.id, sub: 0, role: Role::Via };
                let hole = via.drill_hole();
                self.drill(hole);
                let shape = Shape::Circle { c: via.at, r: via.pad / 2 };
                // Spans refer to stack order, which need not be LayerId order.
                if let (Some(a), Some(b)) = (
                    copper.iter().position(|l| *l == via.span.0),
                    copper.iter().position(|l| *l == via.span.1),
                ) {
                    for &l in &copper[a.min(b)..=a.max(b)] {
                        self.push(l, shape.clone(), Some(hole), tag, Some(via.net));
                    }
                }
                let tag = Tag { role: Role::MaskOpening, ..tag };
                for side in [Side::Top, Side::Bottom] {
                    if let Some(open) = board.via_mask_opening(via.id, side) {
                        self.push(LayerId::F_MASK.on(side), open, None, tag, Some(via.net));
                    }
                }
            }
            ItemKind::Hole => {
                let hole = &board.holes[index];
                let tag = Tag { item: hole.id, sub: 0, role: Role::HoleRing };
                let drill = hole.drill_hole();
                self.drill(drill);
                if let (true, Some(pad)) = (hole.plated, hole.pad) {
                    for &l in copper {
                        self.push(l, Shape::Circle { c: hole.at, r: pad / 2 }, Some(drill), tag, hole.net);
                    }
                }
                for l in [LayerId::F_MASK, LayerId::B_MASK] {
                    self.push(
                        l,
                        Shape::Circle { c: hole.at, r: hole.pad.unwrap_or(hole.drill) / 2 },
                        None,
                        Tag { role: Role::MaskOpening, ..tag },
                        hole.net,
                    );
                }
            }
            ItemKind::Zone => {
                let zone = &board.zones[index];
                let tag = Tag { item: zone.id, sub: 0, role: Role::Zone };
                for l in zone.layers.iter() {
                    match &zone.area {
                        Area::Board => {
                            for contour in &board.outline {
                                for shape in path_strokes(&contour.path, 100_000) {
                                    self.push(l, shape, None, tag, zone.net);
                                }
                            }
                        }
                        Area::Poly(poly) => {
                            for ring in poly.rings() {
                                for shape in ring_strokes(ring, 100_000) {
                                    self.push(l, shape, None, tag, zone.net);
                                }
                            }
                        }
                    }
                }
            }
            ItemKind::Graphic => {
                let graphic = &board.graphics[index];
                let tag = Tag { item: graphic.id, sub: 0, role: Role::Graphic };
                for shape in graphic.gfx.shapes() {
                    self.push(graphic.layer, shape, None, tag, None);
                }
            }
            ItemKind::Text => {
                let t = &board.texts[index];
                let tag = Tag { item: t.id, sub: 0, role: Role::Text };
                for shape in text_strokes(&t.text, t.at, t.rot, t.size, t.thick, t.mirrored) {
                    self.push(t.layer, shape, None, tag, None);
                }
            }
            ItemKind::Keepout => {
                let keepout = &board.keepouts[index];
                let tag = Tag { item: keepout.id, sub: 0, role: Role::Graphic };
                for layer in keepout.layers.iter() {
                    for ring in keepout.area.rings() {
                        for shape in ring_strokes(ring, 100_000) {
                            self.push(layer, shape, None, tag, None);
                        }
                    }
                }
            }
            ItemKind::Cutout => {
                let cutout = &board.cutouts[index];
                let tag = Tag { item: cutout.id, sub: 0, role: Role::Outline };
                for shape in path_strokes(&cutout.contour.path, 100_000) {
                    self.push(LayerId::EDGE, shape, None, tag, None);
                }
            }
            ItemKind::Tuning => {}
        }
    }

    fn push(&mut self, layer: LayerId, shape: Shape, hole: Option<DrillHole>, tag: Tag, net: Option<NetId>) {
        self.bbox = self.bbox.union(shape.bbox());
        let layer = &mut self.layers[layer.0 as usize];
        match shape {
            Shape::Poly(poly) => {
                if !poly.is_empty() {
                    // Polygon pads refer to their hole through the scene's tagged drill list.
                    layer.polys.push(ScenePoly { poly, clip: None, tag, net });
                }
            }
            shape => layer.shapes.push(SceneShape { shape, hole, tag, net }),
        }
    }

    fn fill(&mut self, fill: &ZoneFill, region: Option<BBox>, tag: Tag, net: Option<NetId>) {
        for layer in &fill.layers {
            for (poly_ref, poly) in layer.kept() {
                let clip = fill.lattice.tile_box(poly_ref.tile);
                if region.is_some_and(|b| !(clip.min.x < b.max.x && b.min.x < clip.max.x
                    && clip.min.y < b.max.y && b.min.y < clip.max.y)) {
                    continue;
                }
                self.push_fill(layer.layer, poly, clip, tag, net);
            }
        }
    }

    fn push_fill(&mut self, layer: LayerId, poly: &PolySet, clip: BBox, tag: Tag, net: Option<NetId>) {
        self.bbox = self.bbox.union(poly.bbox());
        if !poly.is_empty() {
            self.layers[layer.0 as usize].polys.push(ScenePoly { poly: poly.clone(), clip: Some(clip), tag, net });
        }
    }
}

/// The scene tag of a drill's owner (the same tags its copper carries).
fn drill_tag(item: ConnItem) -> Option<Tag> {
    match item {
        ConnItem::Pad { placed, pad } => Some(Tag { item: placed, sub: pad.ix() as u32, role: Role::Pad }),
        ConnItem::Via(id) => Some(Tag { item: id, sub: 0, role: Role::Via }),
        ConnItem::Hole(id) => Some(Tag { item: id, sub: 0, role: Role::HoleRing }),
        ConnItem::Track { .. } | ConnItem::Zone { .. } => None,
    }
}

fn path_strokes(path: &Path, w: Nm) -> impl Iterator<Item = Shape> + '_ {
    path.steps().map(move |(a, seg)| match seg {
        Seg::Line(b) => Shape::Segment { a, b, w },
        Seg::Arc { mid, end } => Shape::Arc { a, mid, b: end, w },
    })
}

fn ring_strokes(ring: &[P], w: Nm) -> impl Iterator<Item = Shape> + '_ {
    ring.iter().copied().zip(ring.iter().copied().cycle().skip(1)).map(move |(a, b)| Shape::Segment { a, b, w })
}

/// Match fills to zones in linear time, even with sparse or out-of-order item IDs.
/// Four stable byte passes join the IDs; drawing still follows the original input order.
fn fill_zones(board: &Board, fills: &[ZoneFill]) -> Vec<Option<usize>> {
    let mut matched = vec![None; fills.len()];
    if fills.is_empty() || board.zones.is_empty() {
        return matched;
    }
    let nz = board.zones.len();
    let mut entries: Vec<_> = board.zones.iter().enumerate().map(|(i, z)| (z.id, i))
        .chain(fills.iter().enumerate().map(|(i, f)| (f.zone, nz + i))).collect();
    let mut scratch = vec![(ItemId(0), 0); entries.len()];
    for shift in (0..32).step_by(8) {
        let byte = |id: ItemId| ((id.0 >> shift) & 255) as usize;
        let mut offsets = [0usize; 256];
        for &(id, _) in &entries {
            offsets[byte(id)] += 1;
        }
        let mut start = 0;
        for offset in &mut offsets {
            let count = *offset;
            *offset = start;
            start += count;
        }
        for &entry in &entries {
            let offset = &mut offsets[byte(entry.0)];
            scratch[*offset] = entry;
            *offset += 1;
        }
        std::mem::swap(&mut entries, &mut scratch);
    }
    let mut zone = None;
    for (id, i) in entries {
        if i < nz {
            zone = Some((id, i));
        } else {
            matched[i - nz] = zone.filter(|(z, _)| *z == id).map(|(_, i)| i);
        }
    }
    matched
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_board::zone::{FillLattice, FillLayer, FillTile, PolyRef, TileKey, TileLabels};
    use fab_elec_board::{Anchor, BoardNet, Flash, Hole, Origin, Placed, Tent, Text, Track, Via, ViaKind, Zone};
    use fab_elec_circuit::NetClass;
    use fab_elec_core::{mm, Angle, BoardId, ClassId, CompId, Contour, PolySet};
    use fab_elec_parts::{FpTextKind, PadKind};
    use fab_elec_design::fixture::fp::{chip, pin_header, tssop};
    use std::sync::Arc;

    #[test]
    fn keepout_and_cutout_edges_survive_a_scene_rebuild() {
        let mut board = Board::new(BoardId(0), "openings");
        board.outline.push(Contour::rect(P::ZERO, P::mm(20.0, 20.0), 0));
        let keepout = board.alloc_id();
        board.keepouts.push(fab_elec_board::BoardKeepout {
            id: keepout, area: PolySet::rect(BBox::new(P::mm(1.0, 1.0), P::mm(3.0, 3.0))),
            layers: LayerSet::one(LayerId::B_CU), forbid: fab_elec_parts::Forbid::ALL,
            reason: "antenna".into(), origin: Origin::Authored, z: None,
        });
        let cutout = board.alloc_id();
        board.cutouts.push(fab_elec_board::Cutout { id: cutout, name: "slot".into(), origin: Origin::Authored,
            contour: Contour::rect(P::mm(5.0, 5.0), P::mm(10.0, 10.0), 0) });
        let full = scene_of(&board, &[]);
        let edited = scene_of_items(&board, &[], &[keepout, cutout]);
        for scene in [&full, &edited] {
            let strokes = |layer, id| scene.layer(layer).unwrap().shapes.iter().filter(|s| s.tag.item == id).count();
            assert_eq!(strokes(LayerId::B_CU, keepout), 4);
            assert_eq!(strokes(LayerId::EDGE, cutout), 4);
        }
        assert!(full.substrate.contains(P::mm(2.0, 2.0)));
        assert!(!full.substrate.contains(P::mm(6.0, 6.0)));
    }

    #[test]
    fn board_scene_preserves_geometry_layers_and_tags() {
        let mut board = Board::new(BoardId(0), "scene");
        board.outline = vec![Contour::rect(P::ZERO, P::mm(30.0, 20.0), 0)];
        board.nets.push(BoardNet { name: "signal".into(), class: ClassId(0) });
        board.classes.push(NetClass { id: ClassId(0), name: "default".into(), rules: Default::default() });
        board.generation = 17;
        for (footprint, at, side, rot) in [
            (chip("R", "0603"), P::mm(5.0, 5.0), Side::Top, Angle::ZERO),
            (tssop("U", 14, 0.65, (5.0, 4.4)), P::mm(15.0, 10.0), Side::Bottom, Angle::QUARTER),
            (pin_header("J", 4, 2.54), P::mm(25.0, 10.0), Side::Top, Angle::ZERO),
        ] {
            let id = board.alloc_id();
            board.placed.push(Placed {
                id,
                comp: CompId(id.0),
                path: footprint.name.clone(),
                refdes: footprint.name.clone(),
                pad_nets: vec![Some(NetId(0)); footprint.pads.len()],
                footprint: Arc::new(footprint),
                at, rot, side,
                locked: false,
                origin: Origin::Authored,
                body_z: None,
                ref_label: None, value_label: None, legend: None,
                value: None,
            });
        }
        let track_id = board.alloc_id();
        board.tracks.push(Track {
            id: track_id,
            net: NetId(0),
            layer: LayerId::F_CU,
            w: mm(0.25),
            widths: None,
            path: Path::new(P::mm(6.0, 5.0)).line(P::mm(8.0, 5.0))
                .arc(P::mm(9.0, 6.0), P::mm(10.0, 5.0)).line(P::mm(12.0, 5.0)),
            ends: [Anchor::Free; 2],
            locked: false,
            origin: Origin::Authored,
        });
        let via_id = board.alloc_id();
        board.vias.push(Via {
            id: via_id,
            net: NetId(0),
            at: P::mm(12.0, 5.0),
            drill: mm(0.3),
            pad: mm(0.6),
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through,
            tent: Tent::None,
            backdrill: None,
            locked: false,
            origin: Origin::Authored,
            flash: Flash::All,
        });
        let hole_id = board.alloc_id();
        board.holes.push(Hole {
            id: hole_id,
            name: "mount".into(),
            at: P::mm(3.0, 17.0),
            drill: mm(2.0),
            pad: Some(mm(3.0)),
            net: Some(NetId(0)),
            plated: true,
            origin: Origin::Authored,
        });

        let text_id = board.alloc_id();
        board.texts.push(Text {
            id: text_id,
            layer: LayerId::F_SILK,
            text: "R1".into(),
            at: P::mm(10.0, 10.0),
            rot: Angle::ZERO,
            size: mm(1.0),
            thick: mm(0.15),
            mirrored: false,
            locked: false,
            origin: Origin::Authored,
        });

        let scene = super::super::scene_of(&board, &[]);
        let silk = scene.layer(LayerId::F_SILK).expect("board text creates a front silk layer");
        let text_tag = Tag { item: text_id, sub: 0, role: Role::Text };
        let text_shapes: Vec<_> = silk.shapes.iter().filter(|s| s.tag == text_tag).collect();
        assert!(!text_shapes.is_empty(), "board text produces tagged strokes");
        assert!(text_shapes.iter().all(|s| matches!(s.shape, Shape::Segment { a, b, w: 150_000 } if a != b)), "board text uses nonzero segments with the requested width");
        assert!(text_shapes.iter().all(|s| s.net.is_none() && s.hole.is_none()), "text has no net or drill");
        let text_bbox = text_shapes.iter().fold(BBox::EMPTY, |bbox, s| bbox.union(s.shape.bbox()));
        assert!(text_bbox.center().dist(P::mm(10.0, 10.0)) < mm(0.3) as f64, "board text is centred at its anchor");
        assert!((600_000..=1_200_000).contains(&text_bbox.height()), "board text height follows the requested cap height");

        let chip = &board.placed[0];
        if let Some((i, fp)) = chip.footprint.texts.iter().enumerate().find(|(_, fp)| fp.visible && fp.kind == FpTextKind::Reference) {
            let layer = scene.layer(chip.fp_layer(fp.layer, board.copper_count())).expect("chip reference layer exists");
            let tag = Tag { item: chip.id, sub: i as u32, role: Role::Text };
            assert!(layer.shapes.iter().any(|s| s.tag == tag && matches!(s.shape, Shape::Segment { .. })), "chip reference produces tagged text strokes");
        }
        let copper = board.copper_set();
        let n = board.copper_count();
        let mut pad_count = 0;
        for placed in &board.placed {
            for (i, pad) in placed.footprint.pads.iter().enumerate() {
                let tag = Tag { item: placed.id, sub: i as u32, role: Role::Pad };
                for l in placed.pad_layers(PadIx(i as u16), copper, n).iter() {
                    pad_count += 1;
                    let layer = scene.layer(l).expect("every pad copper layer exists");
                    let count = layer.shapes.iter().filter(|s| s.tag == tag).count()
                        + layer.polys.iter().filter(|p| p.tag == tag).count();
                    assert_eq!(count, 1, "pad {i} of {:?} appears once on {l:?}", placed.id);
                    assert!(layer.shapes.iter().filter(|s| s.tag == tag).all(|s| s.net == placed.pad_nets[i]), "pad keeps its net");
                    if pad.kind == PadKind::Thru {
                        let shape = layer.shapes.iter().find(|s| s.tag == tag).expect("header pad is a primitive");
                        let hole = shape.hole.expect("through pad copper carries its drill");
                        assert!(hole.plated, "through pad drill is plated");
                        assert_eq!(scene.drills.iter().filter(|(d, t)| *t == tag && *d == hole).count(), 1, "through pad has one physical drill across layers");
                    }
                }
            }
        }
        let actual_pads: usize = scene.layers.iter().map(|(_, l)| {
            l.shapes.iter().filter(|s| s.tag.role == Role::Pad).count()
                + l.polys.iter().filter(|p| p.tag.role == Role::Pad).count()
        }).sum();
        assert_eq!(actual_pads, pad_count, "no pad copper is lost or duplicated");

        let bottom = &board.placed[1];
        for (l, role) in [(LayerId::B_CU, Role::Pad), (LayerId::B_MASK, Role::MaskOpening), (LayerId::B_PASTE, Role::Paste)] {
            let layer = scene.layer(l).expect("bottom pad technical and copper layers exist");
            assert_eq!(layer.shapes.iter().filter(|s| s.tag.item == bottom.id && s.tag.role == role).count(), 14, "all bottom pads occur on {l:?}");
        }
        for l in [LayerId::F_CU, LayerId::F_MASK, LayerId::F_PASTE] {
            let layer = scene.layer(l).expect("front chip creates front layers");
            assert!(layer.shapes.iter().all(|s| s.tag.item != bottom.id), "bottom pads do not leak to front layer {l:?}");
        }
        let first_bottom = scene.layer(LayerId::B_CU).expect("bottom copper exists").shapes.iter()
            .find(|s| s.tag.item == bottom.id && s.tag.sub == 0 && s.tag.role == Role::Pad).expect("first bottom pad exists");
        assert!(matches!(first_bottom.shape, Shape::Rect { c, rot, .. } if c == P::mm(13.05, 12.9) && rot == Angle::QUARTER), "bottom pad is mirrored then rotated in the board frame");

        let tracks: Vec<_> = scene.layers.iter().flat_map(|(_, l)| &l.shapes).filter(|s| s.tag.role == Role::Track).collect();
        assert_eq!(tracks.len(), 3, "three path segments produce three Track shapes");
        assert_eq!(tracks.iter().filter(|s| matches!(s.shape, Shape::Arc { .. })).count(), 1, "the track arc stays analytic");
        for (i, s) in tracks.iter().enumerate() {
            assert_eq!(s.tag, Tag { item: track_id, sub: i as u32, role: Role::Track }, "track segment order and tag are preserved");
            assert_eq!(s.net, Some(NetId(0)), "track retains its net");
        }
        for l in [LayerId::F_CU, LayerId::B_CU] {
            let layer = scene.layer(l).expect("both copper layers exist");
            let vias: Vec<_> = layer.shapes.iter().filter(|s| s.tag.item == via_id && s.tag.role == Role::Via).collect();
            assert_eq!(vias.len(), 1, "via appears once on {l:?}");
            assert_eq!(vias[0].hole.map(|d| (d.c, d.size, d.rot, d.plated)), Some((P::mm(12.0, 5.0), P::mm(0.3, 0.3), Angle::ZERO, true)), "via carries its plated drill");
            let rings: Vec<_> = layer.shapes.iter().filter(|s| s.tag.item == hole_id && s.tag.role == Role::HoleRing).collect();
            assert_eq!(rings.len(), 1, "plated mounting hole has a ring on {l:?}");
            assert!(rings[0].hole.is_some(), "mounting hole ring carries its drill");
        }
        for l in [LayerId::F_MASK, LayerId::B_MASK] {
            let layer = scene.layer(l).expect("both mask layers exist");
            for item in [via_id, hole_id] {
                assert_eq!(layer.shapes.iter().filter(|s| s.tag.item == item && s.tag.role == Role::MaskOpening && s.hole.is_none()).count(), 1, "untented via and mounting hole open mask on {l:?}");
            }
        }
        assert_eq!(scene.drills.len(), 6, "four header drills, one via and one free hole are recorded once");
        let edge = scene.layer(LayerId::EDGE).expect("outline creates an EDGE layer");
        assert_eq!(edge.shapes.len(), 4, "rectangular outline has four strokes");
        assert!(edge.shapes.iter().all(|s| s.tag == Tag { item: OUTLINE_ITEM, sub: 0, role: Role::Outline } && matches!(s.shape, Shape::Segment { w: 100_000, .. })), "outline strokes have the sentinel and 0.1 mm width");
        assert_eq!(scene.outline, board.outline, "original outline is retained");
        assert!(!scene.substrate.is_empty(), "board outline produces a filled substrate");
        assert_eq!(scene.substrate.bbox(), board.outline_bbox(), "substrate bounds match the board outline");
        assert!(scene.bbox.contains_box(&board.outline_bbox()), "scene bounds contain the board outline");
        assert!(scene.layers.windows(2).all(|w| w[0].0 < w[1].0), "layers are strictly sorted by LayerId");
        assert!(scene.layers.iter().all(|(_, l)| !l.shapes.is_empty() || !l.polys.is_empty()), "every emitted layer has content");
        assert_eq!(scene.generation, board.generation, "scene retains board generation");
        assert_eq!(scene, super::super::scene_of(&board, &[]), "repeated scene builds are deterministic");

        // Sparse IDs and reversed fill order exercise the join without relying on allocation order.
        board.zones = [ItemId(0x0100_0001), ItemId(0x0001_0001), ItemId(0x0000_0101)]
            .into_iter().enumerate().map(|(i, id)| Zone {
                id,
                net: (i != 1).then_some(NetId(0)),
                name: format!("zone-{i}"),
                layers: copper,
                area: Area::Board,
                priority: 0,
                settings: Default::default(),
                locked: false,
                origin: Origin::Authored,
            }).collect();
        board.next_item = 0x0100_0002;
        let poly = PolySet::rect(BBox::new(P::mm(10.0, 10.0), P::mm(11.0, 11.0)));
        let key = TileKey { x: 0, y: 0 };
        let layer = FillLayer {
            layer: LayerId::F_CU,
            tiles: vec![Arc::new(FillTile {
                key,
                polys: vec![poly.clone()],
                own: vec![true],
                cuts: Default::default(),
                node: vec![0],
                group: vec![0],
                foreign: vec![false],
                core_cuts: Default::default(), lone: Default::default(),
                hash: Default::default(),
            })],
            labels: vec![TileLabels {
                island: vec![PolyRef { tile: key, ord: 0 }],
                kept: vec![true],
                view_hash: Default::default(),
            }],
            reached: Vec::new(),
        };
        let fills: Vec<_> = [board.placed[0].id, board.zones[1].id, board.zones[0].id]
            .into_iter().map(|zone| ZoneFill {
                zone,
                lattice: FillLattice { level: 14 },
                layers: vec![layer.clone()],
            }).collect();
        let filled_scene = super::super::scene_of(&board, &fills);
        let front = filled_scene.layer(LayerId::F_CU).expect("zone fills have a copper layer");
        assert_eq!(front.polys.iter().map(|p| p.tag.item).collect::<Vec<_>>(), vec![board.zones[1].id, board.zones[0].id], "fills preserve input order and ignore IDs that are not zones");
        assert_eq!(front.polys.iter().map(|p| p.net).collect::<Vec<_>>(), vec![None, Some(NetId(0))], "fills take the matched zone's net");
        assert!(front.polys.iter().all(|p| p.tag.role == Role::Zone && p.tag.sub == 0 && p.poly == poly), "zone fill polygons and tags are preserved");
        for l in copper.iter() {
            let layer = filled_scene.layer(l).expect("unfilled board zone has both layers");
            let strokes: Vec<_> = layer.shapes.iter().filter(|s| s.tag.role == Role::Zone).collect();
            assert_eq!(strokes.len(), 4, "only the unfilled zone draws its board outline on {l:?}");
            assert!(strokes.iter().all(|s| s.tag.item == board.zones[2].id && matches!(s.shape, Shape::Segment { w: 100_000, .. })), "unfilled zone strokes keep their item and 0.1 mm width");
        }
        assert_eq!(filled_scene, super::super::scene_of(&board, &fills), "filled scenes are deterministic too");

        let ids: Vec<_> = board.placed.iter().map(|x| x.id)
            .chain(board.tracks.iter().map(|x| x.id))
            .chain(board.vias.iter().map(|x| x.id))
            .chain(board.holes.iter().map(|x| x.id))
            .chain(board.zones.iter().map(|x| x.id))
            .chain(board.graphics.iter().map(|x| x.id))
            .chain(board.texts.iter().map(|x| x.id)).collect();
        let mut repeated_fills = fills.clone();
        repeated_fills.push(fills[1].clone());
        for fills in [&[][..], fills.as_slice(), repeated_fills.as_slice()] {
            let full = super::super::scene_of(&board, fills);
            let check = |ids: &[ItemId], label: &str| {
                let mut expected = full.clone();
                for (_, layer) in &mut expected.layers {
                    layer.shapes.retain(|s| s.tag.item != OUTLINE_ITEM && ids.contains(&s.tag.item));
                    layer.polys.retain(|p| ids.contains(&p.tag.item));
                }
                expected.layers.retain(|(_, l)| !l.shapes.is_empty() || !l.polys.is_empty());
                expected.drills.retain(|(_, tag)| ids.contains(&tag.item));
                expected.outline.clear();
                expected.substrate = PolySet::default();
                // A partial bounds its own geometry, not the removed board body.
                expected.bbox = expected.layers.iter().flat_map(|(_, l)| {
                    l.shapes.iter().map(|s| s.shape.bbox()).chain(l.polys.iter().map(|p| p.poly.bbox()))
                }).chain(expected.drills.iter().map(|(d, _)| d.shape().bbox()))
                    .fold(BBox::EMPTY, BBox::union);
                assert_eq!(scene_of_items(&board, fills, ids), expected,
                    "{label}: partial scene is exactly the filtered full scene, including order, tags, bounds and generation");
            };
            check(&ids, "every item minus outline and substrate");
            let mut reversed = ids.clone();
            reversed.reverse();
            reversed.push(ids[0]);
            check(&reversed, "request order and duplicate IDs do not change scene order");
            check(&[board.zones[0].id, board.placed[2].id, track_id, via_id], "selected geometry, fills and drills only");
            check(&[ItemId(board.next_item), OUTLINE_ITEM], "removed and non-item IDs produce nothing");
            check(&[], "empty selection has no content");
        }
    }
}

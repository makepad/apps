//! Board geometry and supplied zone fills in manufacturing order. No filling or language
//! evaluation happens here; placement transforms are the board geometry's own transforms.
//! Export work only: sorting is O(n log n), followed by emitted geometry and polygon offsets.

use crate::image::{DrillHole, GItem, ObjAttrs};
use crate::model::{CamModel, Pose};
use fab_elec_board::{Board, ZoneFill};
use fab_elec_circuit::Circuit;
use fab_elec_core::{Angle, ItemId, LayerId, NetId, PadIx, PolySet, Seg, Shape, Side, P};
use fab_elec_parts::{PadKind, PartKind};
use std::collections::BTreeMap;


/// Build the factory model, keeping item ids ascending within each geometry class.
pub fn build_model(board: &Board, circuit: &Circuit, fills: &[ZoneFill]) -> CamModel {
    let copper = board.copper_set();
    let n = board.copper_count();
    let mut model = CamModel { copper_layers: n, lattice: fills.first().map(|f| f.lattice), ..CamModel::default() };
    for layer in copper.iter().chain([
        LayerId::F_MASK, LayerId::B_MASK, LayerId::F_PASTE, LayerId::B_PASTE,
        LayerId::F_SILK, LayerId::B_SILK, LayerId::EDGE,
    ]) {
        model.layers.insert(layer, Vec::new());
    }
    let net_name = |net: Option<NetId>| net.and_then(|id| board.nets.get(id.ix())).map(|net| net.name.clone());
    let zones: BTreeMap<_, _> = board.zones.iter().map(|z| (z.id, z)).collect();
    // a fill's pour is the union of its kept polygons (the one-shot read, never a tile at a time)
    let pours: Vec<(ItemId, Vec<(LayerId, PolySet)>)> = fills.iter().map(|f| (f.zone, f.layers.iter().map(|l| (l.layer, l.set())).collect())).collect();
    for (id, sets) in by_id(&pours, |f| f.0) {
        let Some(zone) = zones.get(id) else { continue };
        for (layer, set) in sets {
            if copper.has(*layer) {
                model.layers.entry(*layer).or_default().push(GItem::dark(Shape::Poly(set.clone())).with_attrs(ObjAttrs {
                    net: net_name(zone.net), ..ObjAttrs::default()
                }));
            }
        }
    }

    let placed = by_id(&board.placed, |p| p.id);
    for p in &placed {
        for (i, pad) in p.footprint.pads.iter().enumerate() {
            let ix = PadIx(i as u16);
            if pad.kind != PadKind::Npth {
                let function: &[&str] = match pad.kind {
                    PadKind::Smd => &["SMDPad", "CuDef"],
                    PadKind::Thru => &["ComponentPad"],
                    PadKind::Edge => &["ConnectorPad"],
                    PadKind::Npth => &[],
                };
                let attrs = ObjAttrs {
                    aper_function: function.iter().map(|s| (*s).to_string()).collect(),
                    net: net_name(p.pad_nets.get(i).copied().flatten()),
                    pin: Some((p.refdes.clone(), pad.no.clone())),
                    component: Some(p.refdes.clone()),
                };
                for layer in p.pad_layers(ix, copper, n).iter() {
                    if let Some(shape) = p.pad_shape(ix, layer, n) {
                        model.layers.entry(layer).or_default().push(GItem::dark(shape).with_attrs(attrs.clone()));
                    }
                }
            }
            // openings come from the board's one definition (margins in its rules)
            for side in [Side::Top, Side::Bottom] {
                if let Some(open) = board.mask_opening(p.id, ix, side) {
                    model.layers.entry(LayerId::F_MASK.on(side)).or_default().push(GItem::dark(open));
                }
                if let Some(open) = board.paste_opening(p.id, ix, side) {
                    model.layers.entry(LayerId::F_PASTE.on(side)).or_default().push(GItem::dark(open));
                }
            }
        }
        if !circuit.comps.get(p.comp.ix()).is_some_and(|comp| comp.dnp) {
            model.poses.push(Pose { refdes: p.refdes.clone(), at: p.at, rot: p.rot, side: p.side });
        }
    }
    for track in by_id(&board.tracks, |t| t.id) {
        if !copper.has(track.layer) {
            continue;
        }
        let attrs = ObjAttrs { aper_function: vec!["Conductor".into()], net: net_name(Some(track.net)), ..ObjAttrs::default() };
        for (i, (a, seg)) in track.path.steps().enumerate() {
            model.layers.entry(track.layer).or_default().push(GItem::dark(track.seg_shape(i, a, &seg)).with_attrs(attrs.clone()));
        }
    }
    for via in by_id(&board.vias, |v| v.id) {
        let layers = via.span_layers(copper);
        let attrs = ObjAttrs { aper_function: vec!["ViaPad".into()], net: net_name(Some(via.net)), ..ObjAttrs::default() };
        for layer in layers.iter() {
            model.layers.entry(layer).or_default().push(GItem::dark(via.shape()).with_attrs(attrs.clone()));
        }
        for side in [Side::Top, Side::Bottom] {
            if let Some(open) = board.via_mask_opening(via.id, side) {
                model.layers.entry(LayerId::F_MASK.on(side)).or_default().push(GItem::dark(open));
            }
        }
    }

    for graphic in by_id(&board.graphics, |g| g.id) {
        if matches!(graphic.layer, LayerId::F_SILK | LayerId::B_SILK) {
            model.layers.entry(graphic.layer).or_default().extend(graphic.gfx.shapes().into_iter().map(GItem::dark));
        }
    }
    for p in &placed {
        for (layer, gfx) in &p.footprint.graphics {
            let layer = p.fp_layer(*layer, n);
            if matches!(layer, LayerId::F_SILK | LayerId::B_SILK) {
                model.layers.entry(layer).or_default().extend(gfx.shapes().iter().map(|s| GItem::dark(p.shape_to_board(s))));
            }
        }
    }
    // a part's legend is its marking, plotted with its silk; references, values and board text
    // wait on the plot's own reference and value switches
    for p in &placed {
        let Some(legend) = p.legend.as_ref().filter(|legend| legend.visible) else { continue };
        let (layer, strokes) = fab_elec_board::text::footprint_text(p, legend, n);
        if matches!(layer, LayerId::F_SILK | LayerId::B_SILK) {
            model.layers.entry(layer).or_default().extend(strokes.into_iter().map(GItem::dark));
        }
    }
    let cutouts = by_id(&board.cutouts, |c| c.id);
    for contour in board.outline.iter().chain(cutouts.iter().map(|c| &c.contour)) {
        for (a, seg) in contour.path.steps() {
            let shape = match seg {
                Seg::Line(b) => Shape::Segment { a, b, w: 100_000 },
                Seg::Arc { mid, end } => Shape::Arc { a, mid, b: end, w: 100_000 },
            };
            model.layers.entry(LayerId::EDGE).or_default().push(GItem::dark(shape));
        }
    }

    // Board::drills() is the one source of drill shapes: a circle is a hole, an oval a slot
    // swept between its end-cap centres.
    let mut drills = board.drills();
    drills.sort_by_key(|h| (h.item.item(), h.item));
    for h in drills {
        let mut hole = DrillHole { at: h.c, dia: h.width(), to: None };
        if h.size.x != h.size.y {
            let (long, axis) = if h.size.x > h.size.y { (h.size.x, h.rot) } else { (h.size.y, h.rot.add(Angle::QUARTER)) };
            let reach = P::new((long - h.width()) / 2, 0).rotated(axis);
            hole.at = h.c.sub(reach);
            hole.to = Some(h.c.add(reach));
        }
        model.holes.push((h.plated, hole));
    }
    // A power flag exists only on the schematic, so BOM validation expects no reference for it.
    model.bom_refs = circuit.comps.iter().filter(|c| !c.dnp && c.part.kind != PartKind::PowerFlag).map(|c| c.refdes.clone()).collect();
    model
}

fn by_id<T>(items: &[T], id: impl Fn(&T) -> ItemId) -> Vec<&T> {
    let mut sorted: Vec<_> = items.iter().collect();
    sorted.sort_by_key(|item| id(item));
    sorted
}

#[cfg(test)]
#[path = "plot_tests.rs"]
mod tests;

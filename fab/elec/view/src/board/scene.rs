//! What a board looks like to the GPU: per layer, the filled shapes to draw, each tagged with the
//! board item (and the pad or segment of it) it comes from, plus the drill holes and the outline.
//!
//! `scene_of` is a pure function of a `Board`; it runs on a worker, never per frame. The batcher
//! (`batch.rs`) turns a scene into instance segments per (layer, kind, tile); picking and the
//! semantic nodes read the same tags.

pub use fab_elec_board::DrillHole;
use fab_elec_board::{Board, ZoneFill};
use fab_elec_core::{BBox, Contour, ItemId, LayerId, NetId, PolySet, Shape};

/// Sentinel for outline tags: the board outline is not a board item.
pub const OUTLINE_ITEM: ItemId = ItemId(u32::MAX);

/// Where a drawn shape comes from: the item and its part (pad index of a placement, segment index
/// of a track, 0 otherwise).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Tag {
    pub item: ItemId,
    pub sub: u32,
    pub role: Role,
}

/// What kind of thing a shape draws; it picks the look (pads and tracks differ in hover, the
/// courtyard is a line) and the semantic node's role.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Role {
    Pad,
    Track,
    Via,
    /// A free hole's copper ring (a plated mounting hole).
    HoleRing,
    /// A zone's outline or, once fills exist, its fill.
    Zone,
    /// Footprint or board graphics: silk, fabrication, courtyard, drawings.
    Graphic,
    /// Stroke text (silk text, a reference designator).
    Text,
    /// A mask opening (pad shape grown by the mask margin) on a mask layer.
    MaskOpening,
    /// Paste over a pad on a paste layer.
    Paste,
    /// A board outline contour on the edge layer.
    Outline,
}

/// One filled shape. `shape` is never `Shape::Poly` (those go to `ScenePoly`).
#[derive(Clone, Debug, PartialEq)]
pub struct SceneShape {
    pub shape: Shape,
    /// A hole through a pad or via: drawn as missing copper (the layer shows through to what lies
    /// under the board), so a ring reads as a ring on every layer.
    pub hole: Option<DrillHole>,
    pub tag: Tag,
    pub net: Option<NetId>,
}

/// A polygon set on a layer (custom pads, filled graphics, zone fills).
#[derive(Clone, Debug, PartialEq)]
pub struct ScenePoly {
    pub poly: PolySet,
    /// For a zone fill polygon, its fill tile's box: the batcher keeps the polygon's rows
    /// in the batch tiles under that box.
    pub clip: Option<BBox>,
    pub tag: Tag,
    pub net: Option<NetId>,
}

/// Everything drawn on one layer, shapes in item order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SceneLayer {
    pub shapes: Vec<SceneShape>,
    pub polys: Vec<ScenePoly>,
}

/// A whole board, ready to batch.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DrawScene {
    /// Only layers with content, sorted by `LayerId`.
    pub layers: Vec<(LayerId, SceneLayer)>,
    /// Every drilled hole (pads, vias, free holes), with the item it belongs to.
    pub drills: Vec<(DrillHole, Tag)>,
    /// The board outline (also drawn as `Role::Outline` strokes on the edge layer).
    pub outline: Vec<Contour>,
    /// The board body under all layers, with outline cutouts removed.
    pub substrate: PolySet,
    /// The box of everything drawn.
    pub bbox: BBox,
    /// The board's `generation` this scene was built from.
    pub generation: u64,
}

impl DrawScene {
    pub fn layer(&self, l: LayerId) -> Option<&SceneLayer> {
        self.layers.binary_search_by_key(&l, |(id, _)| *id).ok().map(|i| &self.layers[i].1)
    }
    /// Shapes over every layer (the 100 000 segment bar counts these).
    pub fn shape_count(&self) -> usize {
        self.layers.iter().map(|(_, l)| l.shapes.len()).sum()
    }
}

/// The board as drawable shapes (module docs). Pure; O(items).
pub fn scene_of(board: &Board, fills: &[ZoneFill]) -> DrawScene {
    build::scene_of(board, fills)
}

/// The requested board items, without the board outline or substrate.
pub fn scene_of_items(board: &Board, fills: &[ZoneFill], items: &[ItemId]) -> DrawScene {
    build::scene_of_items(board, fills, items)
}

pub use build::scene_of_fill_region;

/// Plain polygon planes as a scene, for geometry that is not a board's items (an export read
/// back, a fill computed elsewhere): each non-empty set becomes one `ScenePoly` on its layer, role
/// `Zone`, no net, tagged `OUTLINE_ITEM` (it never highlights and is no semantic node). Planes on
/// one layer keep their order. The batcher draws them exactly like a board's fills (exact
/// triangles, ring hairlines), so they go through the same raster. Pure; O(planes).
pub fn scene_of_planes(planes: &[(LayerId, PolySet)]) -> DrawScene {
    let mut scene = DrawScene { bbox: BBox::EMPTY, ..DrawScene::default() };
    for (layer, poly) in planes {
        if poly.is_empty() {
            continue;
        }
        let i = match scene.layers.binary_search_by_key(layer, |(id, _)| *id) {
            Ok(i) => i,
            Err(i) => {
                scene.layers.insert(i, (*layer, SceneLayer::default()));
                i
            }
        };
        scene.bbox = scene.bbox.union(poly.bbox());
        let tag = Tag { item: OUTLINE_ITEM, sub: 0, role: Role::Zone };
        scene.layers[i].1.polys.push(ScenePoly { poly: poly.clone(), clip: None, tag, net: None });
    }
    scene
}

mod build;

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::P;

    #[test]
    fn planes_round_trip_into_one_poly_layer_each() {
        let a = PolySet::rect(BBox::new(P::mm(0.0, 0.0), P::mm(4.0, 3.0)));
        let b = PolySet::rect(BBox::new(P::mm(10.0, -2.0), P::mm(12.0, 1.0)));
        let scene = scene_of_planes(&[
            (LayerId::F_MASK, b.clone()),
            (LayerId::F_CU, a.clone()),
            (LayerId::F_CU, PolySet::default()),
            (LayerId::F_CU, b.clone()),
        ]);
        assert_eq!(scene.layers.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![LayerId::F_CU, LayerId::F_MASK],
            "one layer per plane layer, sorted, empty sets skipped");
        let cu = scene.layer(LayerId::F_CU).expect("copper layer");
        assert_eq!(cu.polys.iter().map(|p| &p.poly).collect::<Vec<_>>(), vec![&a, &b], "the sets themselves, in order");
        assert!(cu.shapes.is_empty() && scene.drills.is_empty() && scene.outline.is_empty() && scene.substrate.is_empty(),
            "planes add no shapes, drills, outline or substrate");
        let tag = Tag { item: OUTLINE_ITEM, sub: 0, role: Role::Zone };
        assert!(scene.layers.iter().flat_map(|(_, l)| &l.polys).all(|p| p.tag == tag && p.net.is_none()),
            "planes carry no item and no net");
        assert_eq!(scene.bbox, a.bbox().union(b.bbox()), "the box of every plane");
        assert_eq!(scene_of_planes(&[]), DrawScene { bbox: BBox::EMPTY, ..DrawScene::default() }, "no planes, empty scene");
    }
}

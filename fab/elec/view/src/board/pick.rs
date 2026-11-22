//! Spatial picking over the same retained geometry used to draw the board.
//! Build once with the scene on a worker; query only for pointer events and marquees.

use super::scene::{DrawScene, DrillHole, SceneLayer, ScenePoly, SceneShape, Tag};
use fab_elec_core::{point_clearance, BBox, Index, ItemId, LayerId, LayerSet, NetId, Nm, PolySet, Shape, P};
use std::ops::ControlFlow;

/// One thing under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub layer: LayerId,
    pub tag: Tag,
    pub net: Option<NetId>,
    /// Index into the layer's `shapes` (`poly == false`) or `polys` (`poly == true`).
    pub index: u32,
    pub poly: bool,
    /// Filled-area distance, rounded down to nm; zero for marquee hits.
    pub dist: Nm,
}

struct LayerPick {
    layer: LayerId,
    shapes: Index<u32>,
    polys: Index<u32>,
}

/// Per-layer spatial indices over a scene's shapes and polygons, plus whole-item press boxes.
pub struct ScenePick {
    layers: Vec<LayerPick>,
    bodies: Index<u32>,
    generation: u64,
}

impl ScenePick {
    pub fn new(scene: &DrawScene, bodies: Vec<(BBox, ItemId)>) -> ScenePick {
        let layers = scene
            .layers
            .iter()
            .map(|(layer, content)| LayerPick {
                layer: *layer,
                shapes: Index::build(
                    content
                        .shapes
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (shape_bbox(&s.shape), u32::try_from(i).expect("scene shape index fits u32")))
                        .collect(),
                ),
                polys: Index::build(
                    content
                        .polys
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (s.poly.bbox(), u32::try_from(i).expect("scene polygon index fits u32")))
                        .collect(),
                ),
            })
            .collect();
        let bodies = Index::build(bodies.into_iter().map(|(bbox, item)| (bbox, item.0)).collect());
        ScenePick { layers, bodies, generation: scene.generation }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Visit whole-item bodies whose box inflated by `tol` contains `p`.
    pub fn bodies_at(&self, p: P, tol: Nm, mut f: impl FnMut(ItemId, BBox)) {
        if tol < 0 {
            return;
        }
        self.bodies.query(BBox::point(p).inflate(tol), |bbox, item| {
            if bbox.inflate(tol).contains(p) {
                f(ItemId(item), bbox);
            }
            ControlFlow::Continue(())
        });
    }

    /// The topmost hit within `tol`, searching `order` from first (top) to last.
    /// On a layer, shapes precede polygons, then nearer hits precede farther ones,
    /// with ties going to later entries in the scene. Negative tolerances have no hits.
    pub fn pick(&self, scene: &DrawScene, p: P, tol: Nm, order: &[LayerId]) -> Option<Hit> {
        debug_assert_eq!(self.generation, scene.generation, "picking requires the indexed scene");
        if tol < 0 {
            return None;
        }
        let area = BBox::point(p).inflate(tol);
        for (index, content) in self.ordered_layers(scene, order) {
            let mut best: Option<Hit> = None;
            // `nearest` keeps its first tie and prunes other zero-distance entries. Visit
            // all candidates so scene order, rather than tree packing, decides ties.
            index.shapes.query(area, |_, i| {
                if let Some(hit) = shape_hit(index.layer, i, &content.shapes[i as usize], p, tol) {
                    if best.is_none_or(|old| pick_key(&hit) < pick_key(&old)) {
                        best = Some(hit);
                    }
                }
                ControlFlow::Continue(())
            });
            if best.is_some() {
                return best;
            }
            index.polys.query(area, |_, i| {
                if let Some(hit) = poly_hit(index.layer, i, &content.polys[i as usize], p, tol) {
                    if best.is_none_or(|old| pick_key(&hit) < pick_key(&old)) {
                        best = Some(hit);
                    }
                }
                ControlFlow::Continue(())
            });
            if best.is_some() {
                return best;
            }
        }
        None
    }

    /// Every hit within `tol`, in the same priority order as `pick`, for cycling.
    pub fn pick_all(&self, scene: &DrawScene, p: P, tol: Nm, order: &[LayerId]) -> Vec<Hit> {
        debug_assert_eq!(self.generation, scene.generation, "picking requires the indexed scene");
        let mut hits = Vec::new();
        if tol < 0 {
            return hits;
        }
        let area = BBox::point(p).inflate(tol);
        for (index, content) in self.ordered_layers(scene, order) {
            let start = hits.len();
            index.shapes.query(area, |_, i| {
                if let Some(hit) = shape_hit(index.layer, i, &content.shapes[i as usize], p, tol) {
                    hits.push(hit);
                }
                ControlFlow::Continue(())
            });
            index.polys.query(area, |_, i| {
                if let Some(hit) = poly_hit(index.layer, i, &content.polys[i as usize], p, tol) {
                    hits.push(hit);
                }
                ControlFlow::Continue(())
            });
            sort_keys(&mut hits[start..], pick_key);
        }
        hits
    }

    /// Boxes meeting `area`, each once, in layer order then ascending scene order
    /// (shapes followed by polygons). With `whole`, require the entire box inside.
    pub fn in_rect(&self, scene: &DrawScene, area: BBox, whole: bool, order: &[LayerId]) -> Vec<Hit> {
        debug_assert_eq!(self.generation, scene.generation, "picking requires the indexed scene");
        let mut hits = Vec::new();
        if area.is_empty() {
            return hits;
        }
        for (index, content) in self.ordered_layers(scene, order) {
            let start = hits.len();
            index.shapes.query(area, |bbox, i| {
                if !whole || area.contains_box(&bbox) {
                    let shape = &content.shapes[i as usize];
                    hits.push(Hit {
                        layer: index.layer, tag: shape.tag, net: shape.net, index: i, poly: false, dist: 0,
                    });
                }
                ControlFlow::Continue(())
            });
            index.polys.query(area, |bbox, i| {
                if !whole || area.contains_box(&bbox) {
                    let poly = &content.polys[i as usize];
                    hits.push(Hit {
                        layer: index.layer, tag: poly.tag, net: poly.net, index: i, poly: true, dist: 0,
                    });
                }
                ControlFlow::Continue(())
            });
            sort_keys(&mut hits[start..], |hit| (u128::from(hit.poly) << 32) | u128::from(hit.index));
        }
        hits
    }

    fn ordered_layers<'a>(
        &'a self,
        scene: &'a DrawScene,
        order: &'a [LayerId],
    ) -> impl Iterator<Item = (&'a LayerPick, &'a SceneLayer)> + 'a {
        let mut seen = LayerSet::EMPTY;
        order.iter().filter_map(move |&layer| {
            if seen.has(layer) {
                return None;
            }
            seen = seen.with(layer);
            let i = self.layers.binary_search_by_key(&layer, |entry| entry.layer).ok()?;
            Some((&self.layers[i], &scene.layers[i].1))
        })
    }
}

// Shape::bbox builds an allocated convex core for rectangles. Reproduce those exact
// rounded core vertices on the stack; all other analytic shapes have allocation-free boxes.
fn shape_bbox(shape: &Shape) -> BBox {
    if let Shape::Rect { c, half, rot, corner } = *shape {
        let r = corner.max(0).min(half.x.min(half.y));
        let (hx, hy) = (half.x - r, half.y - r);
        BBox::of_points(
            [P::new(-hx, -hy), P::new(hx, -hy), P::new(hx, hy), P::new(-hx, hy)]
                .into_iter()
                .map(|p| p.rotated(rot).add(c)),
        )
        .inflate(r)
    } else {
        shape.bbox()
    }
}

fn shape_hit(layer: LayerId, index: u32, shape: &SceneShape, p: P, tol: Nm) -> Option<Hit> {
    if shape.hole.is_some_and(|hole| in_drill(p, hole)) {
        return None;
    }
    let dist = point_clearance(p, &shape.shape);
    (dist <= tol).then_some(Hit { layer, tag: shape.tag, net: shape.net, index, poly: false, dist })
}

fn poly_hit(layer: LayerId, index: u32, poly: &ScenePoly, p: P, tol: Nm) -> Option<Hit> {
    let dist = poly_distance(p, &poly.poly)?;
    (dist <= tol).then_some(Hit { layer, tag: poly.tag, net: poly.net, index, poly: true, dist })
}

fn in_drill(p: P, drill: DrillHole) -> bool {
    if drill.size.x <= 0 || drill.size.y <= 0 {
        return false;
    }
    if drill.size.x == drill.size.y {
        // A disc is rotation-independent; doubling its radius keeps this exact
        // even for odd diameters and points on the boundary.
        let diameter = drill.size.x as i128;
        return 4 * p.dist2(drill.c) <= diameter * diameter;
    }
    // Inverse rotation in f64 preserves half-nm radii and avoids overflow in p - c.
    let dx = p.x as f64 - drill.c.x as f64;
    let dy = p.y as f64 - drill.c.y as f64;
    let (sin, cos) = drill.rot.sin_cos();
    let (hx, hy) = (drill.size.x as f64 * 0.5, drill.size.y as f64 * 0.5);
    let radius = hx.min(hy);
    let x = ((dx * cos + dy * sin).abs() - (hx - radius)).max(0.0);
    let y = ((dy * cos - dx * sin).abs() - (hy - radius)).max(0.0);
    x * x + y * y <= radius * radius
}

fn poly_distance(p: P, poly: &PolySet) -> Option<Nm> {
    if poly.contains(p) {
        return Some(0);
    }
    let mut best = f64::INFINITY;
    for ring in poly.rings() {
        for (&a, &b) in ring.iter().zip(ring.iter().cycle().skip(1)) {
            best = best.min(point_segment_distance(p, a, b));
        }
    }
    // Match point_clearance's downward rounding and floating-point tolerance.
    best.is_finite().then(|| (best + best.abs() * 1e-12 + 1e-9).floor() as Nm)
}

fn point_segment_distance(p: P, a: P, b: P) -> f64 {
    let dx = b.x as i128 - a.x as i128;
    let dy = b.y as i128 - a.y as i128;
    let px = p.x as i128 - a.x as i128;
    let py = p.y as i128 - a.y as i128;
    let length2 = dx * dx + dy * dy;
    let dot = px * dx + py * dy;
    if dot <= 0 {
        return (p.dist2(a) as f64).sqrt();
    }
    if dot >= length2 {
        return (p.dist2(b) as f64).sqrt();
    }
    (px * dy - py * dx).abs() as f64 / (length2 as f64).sqrt()
}

fn pick_key(hit: &Hit) -> u128 {
    (u128::from(hit.poly) << 63) | ((hit.dist as u128) << 32) | u128::from(!hit.index)
}

// In-place MSB radix ordering: O(hits) work with fixed-width keys and bounded stack space.
// Whole-item keys also carry a 64-bit body area and board-order ties. Skip shared leading
// bits so sparse result sets stay cheap; scene keys still use only their original 64 bits.
pub(super) fn sort_keys<T>(hits: &mut [T], key: fn(&T) -> u128) {
    if hits.len() < 2 {
        return;
    }
    let first = key(&hits[0]);
    let differences = hits.iter().fold(0, |bits, hit| bits | (first ^ key(hit)));
    if differences == 0 {
        return;
    }
    let bit = 1u128 << (127 - differences.leading_zeros());
    let mut split = 0;
    for i in 0..hits.len() {
        if key(&hits[i]) & bit == 0 {
            hits.swap(split, i);
            split += 1;
        }
    }
    let (left, right) = hits.split_at_mut(split);
    sort_keys(left, key);
    sort_keys(right, key);
}

#[cfg(test)]
mod tests {
    use super::super::scene::Role;
    use super::*;
    use fab_elec_core::{mm, Angle};

    #[test]
    fn scene_picking_layers_holes_slop_and_marquee() {
        let tag = |item, role| Tag { item: ItemId(item), sub: 0, role };
        let centre = P::mm(5.0, 5.0);
        let edge = P::mm(5.5, 5.0);
        let drill = DrillHole { item: fab_elec_board::ConnItem::Pad { placed: ItemId(1), pad: fab_elec_core::PadIx(0) }, net: Some(NetId(1)), c: centre, size: P::mm(0.4, 0.4), rot: Angle::ZERO, plated: true };
        let zone_box = BBox::new(P::ZERO, P::mm(20.0, 20.0));
        let mut scene = DrawScene {
            layers: vec![
                (LayerId::F_CU, SceneLayer {
                    shapes: vec![
                        SceneShape {
                            shape: Shape::Circle { c: centre, r: mm(0.5) },
                            hole: Some(drill), tag: tag(1, Role::Pad), net: Some(NetId(1)),
                        },
                        SceneShape {
                            shape: Shape::Segment { a: P::ZERO, b: P::mm(10.0, 0.0), w: mm(0.25) },
                            hole: None, tag: tag(2, Role::Track), net: Some(NetId(2)),
                        },
                    ],
                    polys: Vec::new(),
                }),
                (LayerId::B_CU, SceneLayer {
                    shapes: vec![SceneShape {
                        shape: Shape::Rect { c: centre, half: P::mm(0.5, 0.5), rot: Angle::ZERO, corner: 0 },
                        hole: None, tag: tag(3, Role::Pad), net: Some(NetId(3)),
                    }],
                    polys: vec![ScenePoly {
                        poly: PolySet::rect(zone_box), tag: tag(4, Role::Zone), net: Some(NetId(4)),
                        clip: None,
                    }],
                }),
            ],
            drills: vec![(drill, tag(1, Role::Pad))],
            outline: Vec::new(),
            substrate: PolySet::default(),
            bbox: zone_box.union(BBox::new(P::ZERO, P::mm(10.0, 0.0)).inflate(mm(0.125))),
            generation: 7,
        };
        let pick = ScenePick::new(&scene, Vec::new());
        let rebuilt = ScenePick::new(&scene, Vec::new());
        let body_box = BBox::new(P::mm(4.0, 4.0), P::mm(6.0, 6.0));
        let with_body = ScenePick::new(&scene, vec![(body_box, ItemId(1))]);
        let mut bodies = Vec::new();
        with_body.bodies_at(body_box.center(), 0, |item, bbox| bodies.push((item, bbox)));
        with_body.bodies_at(P::mm(7.0, 5.0), 0, |item, bbox| bodies.push((item, bbox)));
        assert_eq!(bodies, vec![(ItemId(1), body_box)], "body centre hits, a point 1 mm outside misses");
        let front = [LayerId::F_CU, LayerId::B_CU];
        let back = [LayerId::B_CU, LayerId::F_CU];
        let front_only = [LayerId::F_CU];
        let back_only = [LayerId::B_CU];
        let tol = mm(0.05);
        let item = |hit: Option<Hit>| hit.map(|h| h.tag.item.0);
        let items = |hits: Vec<Hit>| hits.into_iter().map(|h| h.tag.item.0).collect::<Vec<_>>();

        assert_eq!(pick.generation(), 7, "the picker records the scene generation");
        assert_eq!(item(pick.pick(&scene, edge, tol, &front)), Some(1), "front pad copper wins at its edge");
        assert_eq!(item(pick.pick(&scene, centre, tol, &front)), Some(3), "the drill exposes the back pad");
        assert_eq!(item(pick.pick(&scene, edge, tol, &back)), Some(3), "explicit layer order puts the back pad first");
        let track_slop = P::mm(5.0, 0.175);
        let outside_slop = P::new(track_slop.x, track_slop.y + 1);
        assert_eq!(item(pick.pick(&scene, P::mm(5.0, 0.0), 0, &front_only)), Some(2), "track copper hits with zero slop");
        assert_eq!(item(pick.pick(&scene, track_slop, tol, &front_only)), Some(2), "track slop includes its limit");
        assert_eq!(pick.pick(&scene, outside_slop, tol, &front_only), None, "one nm beyond the track slop misses");
        let zone = pick.pick(&scene, P::mm(15.0, 15.0), tol, &back_only).expect("zone interior is pickable");
        assert_eq!((zone.tag.item.0, zone.poly, zone.index, zone.dist, zone.net), (4, true, 0, 0, Some(NetId(4))), "zone hits retain polygon identity and net");
        assert_eq!(items(pick.pick_all(&scene, edge, tol, &front)), vec![1, 3, 4], "cycling follows layer and shape-before-zone priority");
        let marquee = BBox::new(P::mm(0.0, -1.0), P::mm(6.0, 1.0));
        assert_eq!(items(pick.in_rect(&scene, marquee, false, &front_only)), vec![2], "intersecting marquee finds the track");
        assert!(pick.in_rect(&scene, marquee, true, &front_only).is_empty(), "whole marquee rejects the longer track");
        assert_eq!(items(pick.in_rect(&scene, scene.bbox, true, &[LayerId::F_CU, LayerId::B_CU, LayerId::F_CU])), vec![1, 2, 3, 4], "marquee uses scene order and visits each layer once");
        let zone_slop = P::mm(20.05, 15.0);
        assert_eq!(item(pick.pick(&scene, zone_slop, tol, &back_only)), Some(4), "polygon edge slop includes its limit");
        assert_eq!(pick.pick(&scene, P::new(zone_slop.x + 1, zone_slop.y), tol, &back_only), None, "polygon edges outside slop miss");

        for (p, order) in [
            (edge, front.as_slice()), (centre, front.as_slice()), (edge, back.as_slice()),
            (track_slop, front_only.as_slice()), (outside_slop, front_only.as_slice()),
            (P::mm(15.0, 15.0), back_only.as_slice()),
        ] {
            assert_eq!(pick.pick(&scene, p, tol, order), rebuilt.pick(&scene, p, tol, order), "rebuilding preserves the first hit at {p:?}");
            assert_eq!(pick.pick_all(&scene, p, tol, order), rebuilt.pick_all(&scene, p, tol, order), "rebuilding preserves cycling at {p:?}");
        }
        for whole in [false, true] {
            assert_eq!(pick.in_rect(&scene, marquee, whole, &front), rebuilt.in_rect(&scene, marquee, whole, &front), "rebuilding preserves marquee hits with whole={whole}");
        }

        scene.layers[0].1.shapes.push(SceneShape {
            shape: Shape::Segment { a: P::mm(0.0, 0.1), b: P::mm(10.0, 0.1), w: mm(0.25) },
            hole: None, tag: tag(5, Role::Track), net: None,
        });
        scene.generation += 1;
        let overlap = ScenePick::new(&scene, Vec::new());
        for (p, expected) in [(P::mm(5.0, 0.05), vec![5, 2]), (P::mm(5.0, -0.2), vec![2, 5])] {
            assert_eq!(item(overlap.pick(&scene, p, mm(0.2), &front_only)), expected.first().copied(), "nearest wins and ties favour the later shape at {p:?}");
            assert_eq!(items(overlap.pick_all(&scene, p, mm(0.2), &front_only)), expected, "cycling uses the same distance and tie order at {p:?}");
        }
        for (size, rot) in [(P::mm(0.8, 0.2), Angle::deg(45.0)), (P::mm(0.2, 0.8), Angle::deg(315.0))] {
            let hole = DrillHole { size, rot, ..drill };
            scene.layers[0].1.shapes[0].hole = Some(hole);
            scene.drills[0].0 = hole;
            scene.generation += 1;
            let slot = ScenePick::new(&scene, Vec::new());
            assert_eq!(item(slot.pick(&scene, P::mm(5.2, 5.2), 0, &front)), Some(3), "the rotated slot exposes the back pad for size {size:?}");
            assert_eq!(item(slot.pick(&scene, P::mm(5.2, 4.8), 0, &front)), Some(1), "copper beside the rotated slot remains pickable for size {size:?}");
        }
    }
}

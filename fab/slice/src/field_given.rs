//! A GPU plane's plan ([`plan::LayerPlan`]) as the planner takes it: the layer's outline as
//! regions, and what the GPU stages made of each island ([`GivenLayer`]). A linear conversion:
//! no containment, winding repair, offsetting or sorting belongs at this boundary.
use crate::field_regions;
use makepad_csg_sdf::plan;
use makepad_fab_slicer::geom::{ExPolygon, Polygon, Pt};
use makepad_fab_slicer::source::{BeadLine, Classes, IslandWalls, WallShape};
use makepad_fab_toolpath::given::{GivenIsland, GivenLayer};
use std::sync::Arc;

fn polygon(points: &[[i32; 2]]) -> Polygon {
    Polygon::new(points.iter().map(|p| Pt::new(p[0] as i64, p[1] as i64)).collect())
}

/// Loops as regions: the GPU orders each contour immediately before its holes.
fn regions(loops: &[plan::Loop]) -> Vec<ExPolygon> {
    let mut out: Vec<ExPolygon> = Vec::new();
    for ring in loops {
        let points = polygon(&ring.pts);
        if ring.hole {
            out.last_mut().expect("a GPU hole follows its contour").holes.push(points);
        } else {
            out.push(ExPolygon::new(points));
        }
    }
    out
}

/// Plane `layer`'s outline, one region per island in its order, and the islands' pieces. The
/// outline is compensated: the GPU applies the part's XY compensation, and the first layer's
/// elephant foot is in its walls. `beads`: the walls are variable-width bead lines (arachne),
/// else rings. The layer's classes are its islands' together, when every island has them.
pub fn given_plane(layer: &plan::LayerPlan, beads: bool) -> (Vec<ExPolygon>, GivenLayer) {
    let mut outline = Vec::with_capacity(layer.islands.len());
    let mut islands = Vec::with_capacity(layer.islands.len());
    for (i, island) in layer.islands.iter().enumerate() {
        let mut own = regions(&island.outline);
        assert_eq!(own.len(), 1, "island {i}: one compensated contour per GPU island");
        outline.push(own.pop().unwrap());
        let walls = island.walls.as_ref().map(|w| {
            let shape = if beads {
                WallShape::Beads(w.lines.iter().enumerate().flat_map(|(inset, lines)| lines.iter().map(move |line| BeadLine {
                    inset,
                    closed: line.closed,
                    odd: line.odd,
                    pts: polygon(&line.pts).pts,
                    widths: line.widths.iter().map(|&w| w as f64).collect(),
                })).collect())
            } else {
                WallShape::Rings(w.lines.iter().map(|lines| lines.iter().map(|line| polygon(&line.pts)).collect()).collect())
            };
            Arc::new(IslandWalls { shape, fill: regions(&w.fill), gaps: Vec::new(), thin: Vec::new(), pieces: Vec::new() })
        });
        islands.push(GivenIsland {
            walls,
            labels: None,
            classes: island.regions.as_ref().map(field_regions::classes),
            narrow: island.regions.as_ref().map(|r| r.narrow.clone()),
            concentric: None,
        });
    }
    let classes = islands.iter().map(|i| i.classes.as_ref()).collect::<Option<Vec<&Classes>>>().map(|own| {
        let mut c = Classes::default();
        for i in own {
            for (to, from) in [(&mut c.top, &i.top), (&mut c.bottom, &i.bottom), (&mut c.solid, &i.solid), (&mut c.sparse, &i.sparse), (&mut c.bridge, &i.bridge), (&mut c.vertical, &i.vertical), (&mut c.band, &i.band)] {
                to.extend(from.iter().cloned());
            }
        }
        Arc::new(c)
    });
    (outline, GivenLayer { classes, islands, compensated: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plane_keeps_its_islands_holes_walls_and_classes_in_order() {
        let ring = |pts, hole| plan::Loop { pts, hole, area_mm2: 0.0, bbox: [0; 4] };
        let outer = ring(vec![[-900, 100], [900, 100], [900, 800], [-900, 800]], false);
        let hole = ring(vec![[-300, 300], [-300, 600], [300, 600], [300, 300]], true);
        let small = ring(vec![[1200, 100], [1800, 100], [1800, 700], [1200, 700]], false);
        let line = |r: &plan::Loop| plan::WallLine { closed: true, odd: false, pts: r.pts.clone(), widths: vec![0.42; r.pts.len()] };
        let regions_of = |solid: &plan::Loop| plan::IslandRegions { classes: vec![(plan::Region::Solid, vec![solid.clone()]), (plan::Region::Top, vec![outer.clone(), hole.clone()])], narrow: vec![true] };
        let layer = plan::LayerPlan { islands: vec![
            plan::IslandPlan {
                outline: vec![outer.clone(), hole.clone()],
                walls: Some(plan::WallPlan { lines: vec![vec![line(&outer), line(&hole)], vec![line(&small)]], fill: vec![outer.clone(), hole.clone(), small.clone()] }),
                regions: Some(regions_of(&small)),
            },
            plan::IslandPlan { outline: vec![small.clone()], walls: None, regions: Some(regions_of(&outer)) },
        ] };
        let (outline, given) = given_plane(&layer, false);
        assert!(given.compensated);
        assert_eq!(outline.len(), 2, "one region per island");
        assert_eq!(outline[0].contour, polygon(&outer.pts));
        assert_eq!(outline[0].holes, vec![polygon(&hole.pts)], "a hole stays with its contour");
        assert_eq!(outline[1].contour, polygon(&small.pts), "the GPU's island order");
        assert!(given.islands[1].walls.is_none(), "an island without walls plans them on the cores");
        let walls = given.islands[0].walls.as_ref().unwrap();
        assert_eq!(walls.shape, WallShape::Rings(vec![vec![polygon(&outer.pts), polygon(&hole.pts)], vec![polygon(&small.pts)]]));
        assert_eq!(walls.fill.len(), 2, "each fill contour starts its own piece");
        assert_eq!(walls.fill[0].holes, outline[0].holes);
        assert_eq!(given.islands[0].narrow, Some(vec![true]));
        let (island, layer_classes) = (given.islands[0].classes.as_ref().unwrap(), given.classes.as_ref().unwrap());
        assert_eq!((island.solid.len(), island.top.len(), island.top[0].holes.len()), (1, 1, 1));
        assert_eq!(layer_classes.solid, [island.solid.clone(), given.islands[1].classes.as_ref().unwrap().solid.clone()].concat(), "the layer's classes are its islands' in order");
        let (_, beads) = given_plane(&layer, true);
        let WallShape::Beads(lines) = &beads.islands[0].walls.as_ref().unwrap().shape else { panic!("arachne walls are bead lines") };
        assert_eq!(lines.iter().map(|b| b.inset).collect::<Vec<_>>(), vec![0, 0, 1]);
        assert_eq!(lines[1].pts, polygon(&hole.pts).pts);
        assert!(lines.iter().all(|b| b.closed && !b.odd && b.widths == vec![0.42_f32 as f64; b.pts.len()]));
        let mut partial = layer;
        partial.islands[1].regions = None;
        assert!(given_plane(&partial, false).1.classes.is_none(), "a layer's classes need every island's");
    }
}

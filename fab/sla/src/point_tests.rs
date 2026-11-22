//! Support points: island patterns on shapes of known widths, and the walk up a part's layers.
//! Every island cell is within the pattern's reach of a point, every new stretch of outline
//! within a support's reach, the points lie on the part's surface facing down, and a field and
//! a mesh of one shape are held alike.

use crate::grid::PixelGrid;
use crate::image::LayerStack;
use crate::island::{island_points, Mask, Pattern, Placed, Track};
use crate::parts::{links, over, LayerParts};
use crate::points::{reach, support_points, PointKind, PointSettings, SupportPoint, COAST, LAND};
use crate::printer::simulated;
use crate::section::{MeshSection, Section};
use crate::tests::{field, sphere_mesh};
use makepad_csg_sdf::{SdfNode, SdfOp};
use std::f64::consts::TAU;
use std::sync::Arc;

/// A shape on cells of `cell` mm over `[0, w] × [0, h]` mm, cell `(i, j)`'s centre at
/// `((i - 0.5) cell, (j - 0.5) cell)`.
fn mask(w: f64, h: f64, cell: f64, inside: impl Fn(f64, f64) -> bool) -> Mask {
    let (nw, nh) = ((w / cell).ceil() as usize + 2, (h / cell).ceil() as usize + 2);
    let origin = [-0.5 * cell, -0.5 * cell];
    let on = (0..nw * nh)
        .map(|k| {
            let (i, j) = (k % nw, k / nw);
            i > 0 && j > 0 && i < nw - 1 && j < nh - 1 && inside(origin[0] + i as f64 * cell, origin[1] + j as f64 * cell)
        })
        .collect();
    Mask { w: nw, h: nh, cell: [cell, cell], origin, on }
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// The farthest any cell of `m` is from the nearest point.
fn farthest(m: &Mask, points: &[Placed]) -> f64 {
    (0..m.on.len()).filter(|k| m.on[*k]).map(|k| points.iter().map(|q| dist(q.at, m.center(k))).fold(f64::INFINITY, f64::min)).fold(0.0, f64::max)
}

/// Whether every point lies on a cell of the mask at least `deep` in from its outline.
fn on_part(m: &Mask, points: &[Placed], deep: f64) -> bool {
    let depth = crate::island::depth(m);
    points.iter().all(|q| {
        let k = m.cell_at(q.at);
        m.on[k] && depth[k] >= deep
    })
}

/// R7's numbers for a 0.4 mm tip, and how density scales them; the reach of a support grows
/// with the part above it to its cap.
#[test]
fn pattern_numbers_and_reach() {
    let p = Pattern::new(0.2, 1.0);
    let near = |a: f64, b: f64| (a - b).abs() < 0.01;
    assert!(near(p.one, 1.664) && near(p.two, 6.49) && near(p.thin, 4.16) && near(p.thick, 3.578), "{p:?}");
    assert!(near(p.step, 5.192) && near(p.outline, 4.868) && near(p.grid, 6.49) && near(p.inset, 1.731), "{p:?}");
    let q = Pattern::new(0.2, 4.0);
    assert!(near(q.step, p.step / 4.0) && near(q.grid, p.grid / 2.0) && q.one == p.one);
    assert_eq!((reach(0.0, 1.0), reach(3.9, 1.0), reach(15.0, 1.0), reach(40.0, 1.0), reach(90.0, 1.0)), (3.2, 4.0, 5.0, 6.0, 6.0));
    assert!(near(reach(9.45, 1.0), 4.5) && near(reach(0.0, 4.0), 1.6) && near(reach(-1.0, 1.0), 3.2));
}

/// A speck, a small blob and a short bar get one, one and two fixed points; the bar's two sit
/// near its ends where it is as wide as the tip.
#[test]
fn small_islands_get_one_or_two_points() {
    let p = Pattern::new(0.2, 1.0);
    let speck = island_points(&mask(0.15, 0.15, 0.05, |_, _| true), &p);
    assert_eq!(speck.len(), 1);
    assert!(dist(speck[0].at, [0.075, 0.075]) < 0.03 && speck[0].track == Track::Fixed, "{speck:?}");
    let blob = island_points(&mask(1.2, 1.2, 0.05, |x, y| dist([x, y], [0.6, 0.6]) <= 0.6), &p);
    assert_eq!(blob.len(), 1);
    assert!(dist(blob[0].at, [0.6, 0.6]) < 0.15, "{blob:?}");
    let bar = island_points(&mask(4.0, 0.8, 0.05, |_, _| true), &p);
    assert_eq!(bar.len(), 2, "{bar:?}");
    let (a, b) = (bar[0].at[0].min(bar[1].at[0]), bar[0].at[0].max(bar[1].at[0]));
    assert!(a > 0.15 && a < 1.1 && b < 3.85 && b > 2.9, "near each end, the tip on the part: {bar:?}");
    assert!(bar.iter().all(|q| (q.at[1] - 0.4).abs() < 0.15 && q.track == Track::Fixed));
}

/// A long thin bar: points along its middle a step apart, none near the outline.
#[test]
fn thin_islands_get_points_along_their_middle() {
    let p = Pattern::new(0.2, 1.0);
    let m = mask(30.0, 1.0, 0.05, |_, _| true);
    let pts = island_points(&m, &p);
    let mut xs: Vec<f64> = pts.iter().map(|q| q.at[0]).collect();
    xs.sort_by(f64::total_cmp);
    assert!((5..=8).contains(&pts.len()), "{pts:?}");
    assert!(pts.iter().all(|q| (q.at[1] - 0.5).abs() < 0.1), "on the middle: {pts:?}");
    assert!(xs.windows(2).all(|w| w[1] - w[0] <= 1.1 * p.step && w[1] - w[0] >= 1.0), "a step apart: {xs:?}");
    assert!(farthest(&m, &pts) <= p.reach, "{}", farthest(&m, &pts));
}

/// Branches and loops of a thin island get points along them too: each arm of a Y, and all the
/// way round a ring.
#[test]
fn branches_and_rings_get_points() {
    let p = Pattern::new(0.2, 1.0);
    let c = [13.0, 13.0];
    // three 1 mm arms 12 mm long, a third of a turn apart
    let arm = |k: usize, x: f64, y: f64| {
        let t = TAU * k as f64 / 3.0;
        let (u, v) = ((x - c[0]) * t.cos() + (y - c[1]) * t.sin(), -(x - c[0]) * t.sin() + (y - c[1]) * t.cos());
        (-0.5..=12.0).contains(&u) && v.abs() <= 0.5
    };
    let m = mask(26.0, 26.0, 0.05, |x, y| (0..3).any(|k| arm(k, x, y)));
    let pts = island_points(&m, &p);
    for k in 0..3 {
        let on_arm = pts.iter().filter(|q| arm(k, q.at[0], q.at[1]) && dist(q.at, c) > 3.0).count();
        assert!(on_arm >= 1, "arm {k}: {pts:?}");
    }
    assert!(farthest(&m, &pts) <= p.reach, "{}", farthest(&m, &pts));
    // a ring 1 mm wide, 20 mm across: round it a step or so apart
    let ring = mask(22.0, 22.0, 0.05, |x, y| (dist([x, y], [11.0, 11.0]) - 10.0).abs() <= 0.5);
    let pts = island_points(&ring, &p);
    let mut angles: Vec<f64> = pts.iter().map(|q| (q.at[1] - 11.0).atan2(q.at[0] - 11.0)).collect();
    angles.sort_by(f64::total_cmp);
    let gaps: Vec<f64> = angles.windows(2).map(|w| w[1] - w[0]).chain(std::iter::once(angles[0] + TAU - angles[angles.len() - 1])).collect();
    assert!(gaps.iter().all(|g| *g * 10.0 <= 1.5 * p.step), "round the ring: {angles:?}");
    assert!(farthest(&ring, &pts) <= p.reach);
}

/// A wide island: a row of points along its outline the tip in, a grid inside, nothing out of
/// reach; a turned copy gets about as many.
#[test]
fn thick_islands_get_an_outline_row_and_a_grid() {
    let p = Pattern::new(0.2, 1.0);
    let m = mask(20.0, 20.0, 0.1, |_, _| true);
    let pts = island_points(&m, &p);
    let rows = pts.iter().filter(|q| q.track == Track::Outline).count();
    let inside = pts.iter().filter(|q| q.track == Track::Inside).count();
    assert!((12..=20).contains(&rows) && inside >= 1, "{rows} on the outline, {inside} inside: {pts:?}");
    assert!(on_part(&m, &pts, p.tip - 0.1), "the tips on the part: {pts:?}");
    // no point fills a gap the row and the grid leave (as R7 §2.3): none is wider than the grid
    assert!(farthest(&m, &pts) <= p.grid, "{}", farthest(&m, &pts));
    let wide = island_points(&mask(24.0, 12.0, 0.1, |_, _| true), &p);
    let tall = island_points(&mask(12.0, 24.0, 0.1, |_, _| true), &p);
    // as many, but for where the outline row's last gap and the grid's edge cells fall
    assert!(wide.len().abs_diff(tall.len()) <= 2, "{} and {}", wide.len(), tall.len());
}

/// The layers' parts: islands stand on nothing, the rest on what they overlap below.
#[test]
fn layer_parts_link_to_the_layer_below() {
    let p = simulated();
    // a 6 mm cube whose bottom is 2 mm up, and a second beside it from 3 mm
    let a = field(SdfNode::Box { size: [6.0, 4.0, 6.0], radius: 0.0 }, [20.0, 20.0, 4.0], ([17.0, 17.0, 2.0], [23.0, 23.0, 6.0]));
    let b = field(SdfNode::Box { size: [6.0, 3.0, 6.0], radius: 0.0 }, [40.0, 20.0, 4.5], ([37.0, 17.0, 3.0], [43.0, 23.0, 6.0]));
    let stack = LayerStack::new(vec![Arc::new(a), Arc::new(b)], &p);
    let layer = |k: u32| LayerParts::of(&stack.layer(k).spans);
    // 2 mm in 0.05 mm layers: layer 40 is the first of the first cube's, 60 of the second's
    assert!(layer(39).parts.is_empty());
    let (l40, l41) = (layer(40), layer(41));
    assert_eq!((l40.parts.len(), l40.parts[0].pixels), (1, 120 * 120));
    assert_eq!(links(&l40, &l41), vec![vec![0]]);
    let (l59, l60) = (layer(59), layer(60));
    assert_eq!(links(&l59, &l60), vec![vec![0], vec![]], "the second cube's first layer is an island");
    assert!(over(&l60.parts[0], &l59).is_empty() && over(&l60.parts[1], &l59).len() == 120);
}

/// Whether every new stretch of outline (a pixel a part adds over the layer below, beside one
/// it doesn't cure) is within reach of a point placed at or below its layer (within a coast
/// sample's spacing: the samples are what is checked); and every island pixel within the
/// pattern's reach.
fn held(stack: &LayerStack, points: &[SupportPoint], s: &PointSettings) -> Result<(), String> {
    let grid: PixelGrid = stack.grid;
    let pattern = Pattern::new(s.tip_radius, s.density);
    let mut below = LayerParts::default();
    for k in 0..stack.layers.count {
        let l = LayerParts::of(&stack.layer(k).spans);
        let z = stack.layers.z(k);
        let on = links(&below, &l);
        for (pi, part) in l.parts.iter().enumerate() {
            let island = k > 0 && on[pi].is_empty();
            let new = if k == 0 { Vec::new() } else { over(part, &below) };
            for run in &new {
                for x in run.x0..run.x1 {
                    let y = run.y;
                    let edge = [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)].iter().any(|(i, j)| !l.cured(*i, *j));
                    if !(edge || island) {
                        continue;
                    }
                    let c = grid.center(x, y);
                    let ok = points.iter().any(|q| {
                        let d = dist([q.at[0], q.at[1]], c);
                        q.layer <= k && if island { d <= pattern.reach + 0.1 } else { d <= reach(z - stack.layers.z(q.layer), s.density) + COAST }
                    });
                    if !ok {
                        return Err(format!("layer {k} part {pi}: ({x}, {y}) at {c:?} out of every point's reach ({} points)", points.len()));
                    }
                }
            }
        }
        below = l;
    }
    Ok(())
}

/// The points lie on the part's surface (within a hundredth of a millimetre), where it faces
/// down.
fn on_surface(section: &dyn Section, points: &[SupportPoint]) -> Result<(), String> {
    let at: Vec<[f64; 3]> = points.iter().map(|q| q.at).collect();
    let mut near = vec![Default::default(); at.len()];
    section.near(&at, &mut near);
    for (q, n) in points.iter().zip(&near) {
        if n.distance.abs() > 0.01 || q.normal[2] > 0.0 {
            return Err(format!("{q:?}: {} from the surface", n.distance));
        }
    }
    Ok(())
}

/// A cube held up off the plate: its bottom is one island, held by its pattern; its walls rise
/// straight, so nothing more.
#[test]
fn a_raised_cube_is_held_by_its_bottom() {
    let p = simulated();
    let cube = Arc::new(field(SdfNode::Box { size: [10.0, 4.0, 10.0], radius: 0.0 }, [30.0, 30.0, 7.0], ([25.0, 25.0, 5.0], [35.0, 35.0, 9.0])));
    let stack = LayerStack::new(vec![cube.clone()], &p);
    let s = PointSettings::default();
    let points = support_points(&stack, &s);
    assert!(points.iter().all(|q| q.kind == PointKind::Island && q.layer == 100), "{points:?}");
    assert!((8..=20).contains(&points.len()), "{} points", points.len());
    assert!(points.iter().all(|q| (q.at[2] - 5.0).abs() < 0.01 && q.normal[2] < -0.99), "on the bottom, facing down: {points:?}");
    held(&stack, &points, &s).unwrap();
    on_surface(cube.as_ref(), &points).unwrap();
}

fn sphere_field(c: [f64; 3], r: f64) -> Arc<dyn Section> {
    Arc::new(field(SdfNode::Sphere { radius: r }, c, ([c[0] - r, c[1] - r, c[2] - r], [c[0] + r, c[1] + r, c[2] + r])))
}

/// A sphere held up off the plate: one point at its bottom, rings of coast points as it
/// widens, none above its middle; every new stretch of its outline in reach; a field and a mesh
/// of it held alike.
#[test]
fn a_raised_sphere_is_held_as_it_widens() {
    let p = simulated();
    let s = PointSettings::default();
    let c = [30.0, 30.0, 12.0];
    let mut counts = Vec::new();
    for (name, section) in [("field", sphere_field(c, 6.0)), ("mesh", Arc::new(MeshSection::new(&sphere_mesh(c, 6.0, 96))) as Arc<dyn Section>)] {
        let stack = LayerStack::new(vec![section.clone()], &p);
        let points = support_points(&stack, &s);
        let islands = points.iter().filter(|q| q.kind == PointKind::Island).count();
        assert_eq!(islands, 1, "{name}: one island, its bottom: {points:?}");
        assert!(points.iter().all(|q| q.at[2] < c[2]), "{name}: none above the middle: {points:?}");
        held(&stack, &points, &s).unwrap_or_else(|e| panic!("{name}: {e}"));
        on_surface(section.as_ref(), &points).unwrap_or_else(|e| panic!("{name}: {e}"));
        counts.push(points.len());
    }
    assert!(counts[0] >= 4 && counts[0].abs_diff(counts[1]) * 4 <= counts[0], "the field's and the mesh's: {counts:?}");
}

/// A cone opening upward at 45°: points ring it all the way up, each new outline in reach.
#[test]
fn a_cone_is_held_all_the_way_up() {
    let p = simulated();
    let s = PointSettings::default();
    // model Y is the plate's Z: 0.5 mm wide at the bottom, 20 mm at the top, 10 mm tall
    let cone = Arc::new(field(SdfNode::Cylinder { radius: 0.25, top_radius: 10.0, height: 10.0 }, [40.0, 40.0, 10.0], ([29.5, 29.5, 4.9], [50.5, 50.5, 15.1])));
    let stack = LayerStack::new(vec![cone.clone()], &p);
    let points = support_points(&stack, &s);
    let coast = points.iter().filter(|q| q.kind == PointKind::Coast).count();
    assert!(coast >= 8, "{points:?}");
    held(&stack, &points, &s).unwrap();
    on_surface(cone.as_ref(), &points).unwrap();
}

/// A table: a wide top on a narrow leg. The top's first layer reaches far past the leg: its
/// shelf beyond the land gets points of its own, not just its edge.
#[test]
fn a_shelf_is_held_across() {
    let p = simulated();
    let s = PointSettings::default();
    let leg = SdfNode::Transform { at: [0.0, 2.5, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: [4.0, 5.0, 4.0], radius: 0.0 }) };
    let top = SdfNode::Transform { at: [0.0, 5.5, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: [20.0, 1.0, 20.0], radius: 0.0 }) };
    let table = Arc::new(field(SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![leg, top] }, [40.0, 40.0, 5.0], ([29.9, 29.9, 4.9], [50.1, 50.1, 11.1])));
    let stack = LayerStack::new(vec![table.clone()], &p);
    let points = support_points(&stack, &s);
    let shelf: Vec<&SupportPoint> = points.iter().filter(|q| q.kind == PointKind::Shelf).collect();
    assert!(shelf.len() >= 8 && shelf.iter().all(|q| (q.at[2] - 10.0).abs() < 0.01), "{points:?}");
    // the top's underside beyond the land: within the grid's side of its points (no point fills
    // a gap the row and the grid leave, as R7 §2.3)
    let pattern = Pattern::new(s.tip_radius, s.density);
    for x in 0..=40 {
        for y in 0..=40 {
            let c = [30.0 + 0.5 * x as f64, 30.0 + 0.5 * y as f64];
            let out = (c[0] - 40.0).abs().max((c[1] - 40.0).abs()) - 2.0;
            if out > LAND + 0.3 {
                assert!(shelf.iter().any(|q| dist([q.at[0], q.at[1]], c) <= pattern.grid), "{c:?}");
            }
        }
    }
    held(&stack, &points, &s).unwrap();
    on_surface(table.as_ref(), &points).unwrap();
}

//! A boolean's rounding can leave a tooth on a ring: two points a unit apart, an edge pointing
//! anywhere. Offsetting it must not cut the tooth's miter into the region.
use makepad_fab_slicer::clip;
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::grid::SegGrid;
use makepad_fab_slicer::offset::{self, Join};

/// A stretch of a mesh contour's top where a grow left a tooth, right to left.
const TOP: [(i64, i64); 17] = [
    (245326517, 231346294), (245241660, 231321982), (244902236, 231224738), (244820896, 231200598),
    (244495537, 231104039), (244425382, 231089786), (244144766, 231032777), (244091133, 231036540),
    (243876602, 231051592), (243841654, 231078408), (243701864, 231185673), (243684348, 231236504),
    (243614284, 231439831), (243609893, 231511283), (243592331, 231797093), (243594596, 231881909),
    (243603656, 232221174),
];

#[test]
fn a_closing_cuts_no_notch_at_a_tooth() {
    // The stretch, closed below.
    let mut pts: Vec<Pt> = TOP.iter().map(|&(x, y)| Pt::new(x, y)).collect();
    let low = pts.iter().map(|p| p.y).min().unwrap() - 1_000_000;
    let (left, right) = (pts[pts.len() - 1].x, pts[0].x);
    pts.push(Pt::new(left, low));
    pts.push(Pt::new(right, low));
    let mut c = Polygon::new(pts);
    if !c.is_ccw() {
        c.reverse();
    }
    let a = clip::union_ex(&[ExPolygon { contour: c, holes: vec![] }]);
    let r = 49_000.0;
    let closed = offset::offset_ex(&offset::offset_ex(&a, r, Join::Miter(3.0)), -r, Join::Miter(3.0));
    let edges = SegGrid::of_regions(&a, |k| k as u32);
    for p in closed.iter().flat_map(|e| e.rings()).flat_map(|r| r.pts.iter().copied()) {
        let d = edges.nearest(p, |k| k).unwrap().0;
        assert!(!(d > 8.0 && edges.contains_any(p)), "a notch {d:.0} units deep at ({}, {})", p.x, p.y);
    }
}

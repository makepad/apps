use crate::image::GItem;
use crate::model::{render, CamModel};
use crate::profile::{CheckKind, ResolvedProfile};
use crate::validate::{seam_faults, tile_at, validate};
use fab_elec_board::FillLattice;
use fab_elec_core::{mm, BBox, LayerId, PolySet, Shape, P};

const LATTICE: FillLattice = FillLattice { level: 10 }; // 1.024 mm tiles

fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> PolySet {
    PolySet::rect(BBox::new(P::new(x0, y0), P::new(x1, y1)))
}

fn model(pours: &[PolySet]) -> CamModel {
    let mut m = CamModel { copper_layers: 2, lattice: Some(LATTICE), ..Default::default() };
    m.layers.insert(LayerId::F_CU, pours.iter().map(|p| GItem::dark(Shape::Poly(p.clone()))).collect());
    m
}

#[test]
fn a_pour_must_not_show_where_its_tiles_met() {
    let s = LATTICE.side();
    let profile = ResolvedProfile::standard_2_layer("seam", "A");
    let whole = rect(mm(0.2), mm(0.2), 3 * s, 2 * s);
    let want = model(&[whole.clone()]);

    // a pour written whole passes, and tile numbers are the lattice's
    let v = validate(&render(&want, &profile).unwrap(), &profile, &want);
    assert!(v.ok(), "the whole pour validates: {:?}", v.failures().collect::<Vec<_>>());
    assert_eq!(tile_at(LATTICE, P::new(s, s)), (1, 1), "a point on a tile line belongs to the tile above and right");

    // two halves that meet exactly on the lattice line merge in the read-back: no seam
    let halves = model(&[rect(mm(0.2), mm(0.2), s, 2 * s), rect(s, mm(0.2), 3 * s, 2 * s)]);
    let v = validate(&render(&halves, &profile).unwrap(), &profile, &want);
    assert!(v.ok(), "tiles that meet exactly are one region: {:?}", v.failures().collect::<Vec<_>>());

    // a 1 um slit on the lattice line: invisible to the erosion tolerance, a fault for the seam check
    let slit = model(&[rect(mm(0.2), mm(0.2), s - 500, 2 * s), rect(s + 500, mm(0.2), 3 * s, 2 * s)]);
    let v = validate(&render(&slit, &profile).unwrap(), &profile, &want);
    let f: Vec<_> = v.failures().collect();
    assert_eq!(f.len(), 1, "exactly the seam fails: {f:?}");
    assert!(f[0].kind == CheckKind::Geometry && f[0].cause.contains("seam") && f[0].cause.contains("tiles (0, "), "the seam names the tiles: {}", f[0].cause);
    assert_eq!(f[0].file, "seam-F_Cu.gbr", "the layer's file");
    assert!(f[0].hint.contains("union"), "the fix hint says how to write it");
    assert!(f[0].at.is_some_and(|p| (p.x - s).abs() <= 1000), "the place is on the tile line");

    // a half missing on one side of the line is also a seam
    let half = model(&[rect(mm(0.2), mm(0.2), s, 2 * s)]);
    let v = validate(&render(&half, &profile).unwrap(), &profile, &want);
    assert!(v.failures().any(|c| c.cause.contains("differs") && c.tile.is_some()), "a missing half names its tile: {:?}", v.failures().collect::<Vec<_>>());
}

#[test]
fn copper_edges_on_a_lattice_line_are_not_seams() {
    let s = LATTICE.side();
    // a pad whose edge is exactly on the line: the model has no copper beyond it
    let pad = rect(s - mm(0.5), mm(0.2), s, mm(0.9));
    assert!(seam_faults(&pad, &pad, LATTICE, 1000).is_empty(), "a real boundary on a tile line is no seam");
    // the fill's own arc error (a vertex 4 um off the line) is an off-line edge, not a seam
    let off = rect(s - mm(0.5), mm(0.2), s - 4000, mm(0.9));
    assert!(seam_faults(&off, &off, LATTICE, 1000).is_empty(), "an edge farther than the tolerance from the line is not near it");
    // the read-back loses a slice the model has across the line: seam
    let model_side = rect(s - mm(0.5), mm(0.2), s + mm(0.5), mm(0.9));
    let read = rect(s - mm(0.5), mm(0.2), s, mm(0.9));
    assert_eq!(seam_faults(&read, &model_side, LATTICE, 1000).len(), 1, "copper missing across the line is one seam");
}

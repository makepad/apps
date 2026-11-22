use crate::drill::{read_excellon, write_excellon, DrillOpts, Zeros};
use crate::gerber_read::read_gerber;
use crate::gerber_write::{write_gerber, GerberOpts};
use crate::image::{items_polyset, DrillHole, GItem, Geom, ObjAttrs, Polarity};
use crate::model::{render, CamModel, Pose};
use crate::profile::{BomRow, CheckKind, FileKind, PlaceRow, ResolvedProfile};
use crate::validate::{region_difference, validate};
use fab_elec_core::{mm, Angle, Contour, LayerId, Path, PolySet, Shape, Side, P};

fn pt(x: f64, y: f64) -> P {
    P::mm(x, y)
}

fn art() -> Vec<GItem> {
    let net = |n: &str, r: &str, p: &str| ObjAttrs {
        aper_function: vec!["SMDPad".into(), "CuDef".into()],
        net: Some(n.into()),
        pin: Some((r.into(), p.into())),
        component: Some(r.into()),
    };
    let ring = |pts: &[(f64, f64)]| pts.iter().map(|&(x, y)| pt(x, y)).collect::<Vec<_>>();
    let outer = ring(&[(20.0, 0.0), (30.0, 0.0), (30.0, 10.0), (20.0, 10.0)]);
    let hole = ring(&[(22.0, 2.0), (22.0, 8.0), (28.0, 8.0), (28.0, 2.0)]);
    let holey = PolySet::from_rings_even_odd(&[outer, hole]);
    let arc_region = Contour::new(Path::new(pt(40.0, 0.0)).line(pt(46.0, 0.0)).arc(pt(48.0, 3.0), pt(46.0, 6.0)).line(pt(40.0, 6.0)));
    vec![
        GItem::dark(Shape::Circle { c: pt(1.0, 1.0), r: mm(0.4) }).with_attrs(net("GND", "J1", "1")),
        GItem::dark(Shape::Rect { c: pt(3.0, 1.0), half: P::mm(0.5, 0.3), rot: Angle::ZERO, corner: 0 }).with_attrs(net("VCC", "R1", "2")),
        GItem::dark(Shape::Rect { c: pt(5.0, 1.0), half: P::mm(0.5, 0.3), rot: Angle::deg(90.0), corner: 0 }),
        GItem::dark(Shape::Rect { c: pt(7.0, 1.0), half: P::mm(0.6, 0.4), rot: Angle::deg(30.0), corner: 0 }),
        GItem::dark(Shape::Rect { c: pt(9.0, 1.0), half: P::mm(0.7, 0.5), rot: Angle::deg(-20.0), corner: mm(0.2) }).with_attrs(net("N1", "U1", "3")),
        GItem::dark(Shape::Rect { c: pt(11.0, 1.0), half: P::mm(0.7, 0.5), rot: Angle::ZERO, corner: mm(0.25) }),
        GItem::dark(Shape::Oval { c: pt(13.0, 1.0), half: P::mm(0.8, 0.4), rot: Angle::ZERO }),
        GItem::dark(Shape::Oval { c: pt(15.0, 1.0), half: P::mm(0.4, 0.8), rot: Angle::deg(90.0) }),
        GItem::dark(Shape::Oval { c: pt(17.0, 1.0), half: P::mm(0.8, 0.4), rot: Angle::deg(35.0) }),
        GItem::dark(Shape::Segment { a: pt(1.0, 5.0), b: pt(9.0, 7.0), w: mm(0.25) }).with_attrs(ObjAttrs { net: Some("GND".into()), ..Default::default() }),
        GItem::dark(Shape::Arc { a: pt(1.0, 12.0), mid: pt(4.0, 14.0), b: pt(7.0, 12.0), w: mm(0.3) }),
        GItem::dark(Shape::Arc { a: pt(10.0, 12.0), mid: pt(12.0, 14.0), b: pt(14.0, 12.0), w: mm(0.3) }),
        GItem::dark(Shape::Poly(holey)),
        GItem { polarity: Polarity::Dark, geom: Geom::Region(arc_region), attrs: ObjAttrs::default() },
        GItem { polarity: Polarity::Clear, geom: Geom::Shape(Shape::Circle { c: pt(43.0, 3.0), r: mm(1.0) }), attrs: ObjAttrs::default() },
    ]
}

#[test]
fn gerber_roundtrip_every_shape_and_attribute() {
    let items = art();
    let attrs = vec![("FileFunction".to_string(), vec!["Copper".to_string(), "L1".into(), "Top".into()])];
    let model = items_polyset(&items, 200);
    for decimals in [6u8, 5] {
        let bytes = write_gerber(&items, &attrs, &GerberOpts { decimals, ..GerberOpts::default() }).unwrap();
        let img = read_gerber(&bytes).unwrap_or_else(|e| panic!("{e}\n{}", String::from_utf8_lossy(&bytes)));
        assert_eq!(img.file_attr("FileFunction"), Some(&attrs[0].1[..]), "file function");
        assert_eq!(img.decimals, decimals);
        let d = region_difference(&img.to_polyset(200), &model, 1000);
        assert!(d.is_none(), "round trip at {decimals} decimals differs: {d:?}\n{}", String::from_utf8_lossy(&bytes));
    }
    let bytes = write_gerber(&items, &attrs, &GerberOpts::default()).unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    assert!(text.contains("%TO.N,GND*%") && text.contains("%TO.P,J1,1*%") && text.contains("%TO.C,R1*%"), "object attributes written");
    assert!(text.contains("%TA.AperFunction,SMDPad,CuDef*%"), "aperture function written");
    assert!(text.contains("%AMROUNDR") && text.contains("%AMRECTR") && text.contains("%AMOVALR"), "macros for the shapes the standard apertures cannot say");
    assert!(text.contains("G03*") && text.contains("G02*") && text.contains("G36*"), "true arcs and regions");
    let img = read_gerber(&bytes).unwrap();
    let flashed: Vec<&GItem> = img.items.iter().filter(|i| i.attrs.net.is_some()).collect();
    assert_eq!(flashed.len(), 4, "net-bound objects");
    assert_eq!(flashed[0].attrs.pin, Some(("J1".into(), "1".into())));
    assert_eq!(flashed[0].attrs.component.as_deref(), Some("J1"));
    assert_eq!(flashed[0].attrs.aper_function, vec!["SMDPad".to_string(), "CuDef".to_string()]);
    assert!(matches!(flashed[0].geom, Geom::Shape(Shape::Circle { r, .. }) if r == mm(0.4)), "a circle flash reads back as the circle");
    assert!(img.items.iter().any(|i| matches!(i.geom, Geom::Shape(Shape::Arc { .. }))), "arcs read back as arcs");
    let rotated = |g: &Geom| matches!(g, Geom::Shape(Shape::Rect { rot, .. } | Shape::Oval { rot, .. }) if rot.udeg() != 0);
    assert!(img.items.iter().filter(|i| rotated(&i.geom)).count() >= 3, "our own macros read back as rotated rectangles and ovals, not polygons");
    assert!(img.items.iter().any(|i| matches!(i.geom, Geom::Shape(Shape::Rect { corner, .. }) if corner > 0)), "rounded rectangles stay rounded");
    assert!(img.items.iter().any(|i| matches!(&i.geom, Geom::Region(outer) if outer.path.segs.iter().any(|s| matches!(s, fab_elec_core::Seg::Arc { .. })))), "region arcs stay arcs");
    // S46.R48: X1 carries attributes as comments.
    let x1 = write_gerber(&items, &attrs, &GerberOpts { x2: false, ..GerberOpts::default() }).unwrap();
    assert!(!String::from_utf8_lossy(&x1).contains("%T"), "S46.R48: no attribute commands in X1");
    assert!(String::from_utf8_lossy(&x1).contains("G04 #@! TF.FileFunction,Copper,L1,Top*"), "S46.R48: X1 keeps attribute comments");
    // the reader names what it does not read
    let bad = String::from_utf8_lossy(&bytes).replace("G75*", "G74*");
    assert!(read_gerber(bad.as_bytes()).is_err(), "single-quadrant arcs are refused, not guessed");
    assert!(read_gerber(b"%FSLAX46Y46*%\n%MOMM*%\n").is_err(), "no M02");
}

#[test]
fn excellon_roundtrip_all_zero_modes() {
    let holes = vec![
        DrillHole { at: pt(1.5, 2.25), dia: mm(0.3), to: None },
        DrillHole { at: pt(-10.0, 0.001), dia: mm(0.3), to: None },
        DrillHole { at: pt(0.0, 0.0), dia: mm(1.0), to: None },
        DrillHole { at: pt(30.0, 20.0), dia: mm(0.8), to: Some(pt(33.0, 20.0)) },
    ];
    let attrs = vec![("FileFunction".to_string(), vec!["Plated".to_string(), "1".into(), "2".into(), "PTH".into()])];
    for (inch, zeros, i, f) in [(false, Zeros::Decimal, 3, 3), (false, Zeros::KeepLeading, 3, 3), (false, Zeros::KeepTrailing, 3, 3), (true, Zeros::KeepLeading, 2, 4), (true, Zeros::Decimal, 2, 4)] {
        let bytes = write_excellon(&holes, &attrs, &DrillOpts { inch, zeros, int_digits: i, frac_digits: f });
        let img = read_excellon(&bytes).unwrap_or_else(|e| panic!("{e}\n{}", String::from_utf8_lossy(&bytes)));
        let tol = if zeros == Zeros::Decimal { 1000 } else { (25_400_000.0 / 10f64.powi(f as i32) / 2.0) as i32 + 1 };
        let mut want = holes.clone();
        want.sort_by_key(|h| (h.dia, h.at.x, h.at.y));
        let mut got = img.holes.clone();
        got.sort_by_key(|h| (h.dia, h.at.x, h.at.y));
        assert_eq!(got.len(), want.len(), "hole count {inch} {zeros:?}");
        for (g, w) in got.iter().zip(&want) {
            let near = |a: i32, b: i32| (a - b).abs() <= tol.max(1100);
            assert!(near(g.at.x, w.at.x) && near(g.at.y, w.at.y) && near(g.dia, w.dia), "{g:?} vs {w:?} ({inch} {zeros:?})\n{}", String::from_utf8_lossy(&bytes));
            assert_eq!(g.to.is_some(), w.to.is_some(), "slot kept");
        }
        assert_eq!(img.file_attr("FileFunction"), Some(&attrs[0].1[..]));
        assert_eq!(img.inch, inch);
    }
    assert!(read_excellon(b"M48\nMETRIC\nT1C0.3\n%\nT1\nX1Y1\n").is_err(), "no M30");
}

fn model() -> (ResolvedProfile, CamModel) {
    let mut p = ResolvedProfile::standard_2_layer("ledctl", "A");
    let mut m = CamModel { copper_layers: 2, ..Default::default() };
    m.layers.insert(LayerId::F_CU, art());
    m.layers.insert(LayerId::EDGE, vec![GItem::dark(Shape::Segment { a: pt(0.0, 0.0), b: pt(50.0, 0.0), w: mm(0.1) })]);
    m.holes.push((true, DrillHole { at: pt(2.0, 2.0), dia: mm(0.4), to: None }));
    m.holes.push((false, DrillHole { at: pt(5.0, 5.0), dia: mm(3.0), to: None }));
    m.poses.push(Pose { refdes: "R1".into(), at: pt(3.0, 1.0), rot: Angle::deg(90.0), side: Side::Top });
    m.bom_refs = vec!["R1".into(), "U1".into()];
    p.bom.rows.push(BomRow { cells: vec!["10k, 1%".into(), "R1".into(), "0603".into(), "".into()], refs: vec!["R1".into()] });
    p.bom.rows.push(BomRow { cells: vec!["MCU".into(), "U1".into(), "QFN".into(), "x".into()], refs: vec!["U1".into()] });
    p.placement.rows.push(PlaceRow { cells: vec!["R1".into(), "3".into(), "1".into(), "T".into(), "270".into()], refdes: "R1".into(), at: pt(3.0, 1.0), rot: Angle::deg(270.0), rot_offset: Angle::deg(180.0), side: Side::Top });
    (p, m)
}

#[test]
fn package_validates_and_wrong_files_fail_with_the_cause() {
    let (p, m) = model();
    let files = render(&m, &p).unwrap();
    assert_eq!(files.len(), p.files.len());
    let v = validate(&files, &p, &m);
    assert!(v.ok(), "{:?}", v.failures().collect::<Vec<_>>());

    // a shifted layer
    let mut shifted = m.clone();
    for it in shifted.layers.get_mut(&LayerId::F_CU).unwrap() {
        if let Geom::Shape(s) = &mut it.geom {
            *s = s.translated(P::mm(0.5, 0.0));
        }
    }
    let bad = render(&shifted, &p).unwrap();
    let v = validate(&bad, &p, &m);
    let f: Vec<_> = v.failures().collect();
    assert!(f.iter().any(|c| c.kind == CheckKind::Geometry && c.file == "ledctl-F_Cu.gbr" && c.cause.contains("differs")), "shift named: {f:?}");
    assert!(f.iter().filter(|c| c.kind == CheckKind::Geometry).all(|c| c.at.is_some_and(|p| p.x > mm(1.0))), "a geometry failure carries its place");
    assert!(f.iter().all(|c| !c.hint.is_empty()), "every failure has a fix hint");

    // a missing drill hole
    let mut fewer = m.clone();
    fewer.holes.remove(0);
    let v = validate(&render(&fewer, &p).unwrap(), &p, &m);
    assert!(v.failures().any(|c| c.kind == CheckKind::Drill && c.file == "ledctl-PTH.drl" && c.cause.contains("0 holes")), "missing hole named");

    // a file under the wrong name, and a file left out
    let mut renamed = files.clone();
    renamed[0].name = "board.gbr".into();
    let v = validate(&renamed, &p, &m);
    assert!(v.failures().any(|c| c.kind == CheckKind::FileSet && c.cause.contains("ledctl-F_Cu.gbr") && c.cause.contains("board.gbr")), "names equal the profile");

    // a BOM that forgot a part, a placement for a part that is not on the board
    let mut p2 = p.clone();
    p2.bom.rows.pop();
    let v = validate(&render(&m, &p2).unwrap(), &p2, &m);
    assert!(v.failures().any(|c| c.kind == CheckKind::Bom && c.cause.contains("U1")), "BOM equals the circuit");
    let mut p3 = p.clone();
    p3.placement.rows[0].refdes = "R9".into();
    let v = validate(&render(&m, &p3).unwrap(), &p3, &m);
    assert!(v.failures().any(|c| c.kind == CheckKind::Placement), "placement equals the poses");
    let mut p4 = p.clone();
    p4.placement.rows[0].at = pt(3.5, 1.0);
    let v = validate(&render(&m, &p4).unwrap(), &p4, &m);
    assert!(v.failures().any(|c| c.kind == CheckKind::Placement && c.cause.contains("pose gives")), "placement positions equal the poses");
    assert!(p.files.iter().any(|f| matches!(f.kind, FileKind::Bom)));
}

#[test]
fn polygon_holes_never_erase_what_lies_in_them() {
    let ring = |pts: &[(f64, f64)]| pts.iter().map(|&(x, y)| pt(x, y)).collect::<Vec<_>>();
    // a pour with three holes, and a concave outline with a hole behind its reflex corner
    let pour = PolySet::from_rings_even_odd(&[
        ring(&[(0.0, 0.0), (30.0, 0.0), (30.0, 20.0), (0.0, 20.0)]),
        ring(&[(2.0, 2.0), (2.0, 8.0), (8.0, 8.0), (8.0, 2.0)]),
        ring(&[(12.0, 3.0), (12.0, 17.0), (18.0, 17.0), (18.0, 3.0)]),
        ring(&[(22.0, 6.0), (22.0, 9.0), (26.0, 9.0), (26.0, 6.0)]),
    ]);
    let concave = PolySet::from_rings_even_odd(&[
        ring(&[(40.0, 0.0), (60.0, 0.0), (60.0, 20.0), (50.0, 20.0), (50.0, 8.0), (45.0, 8.0), (45.0, 20.0), (40.0, 20.0)]),
        ring(&[(42.0, 1.0), (42.0, 5.0), (46.0, 5.0), (46.0, 1.0)]),
        ring(&[(54.0, 10.0), (54.0, 14.0), (57.0, 14.0), (57.0, 10.0)]),
    ]);
    let other_net = |s: Shape| GItem::dark(s).with_attrs(ObjAttrs { net: Some("SIG".into()), ..Default::default() });
    let inside = [
        other_net(Shape::Segment { a: pt(3.0, 5.0), b: pt(7.0, 5.0), w: mm(0.2) }),
        other_net(Shape::Circle { c: pt(15.0, 10.0), r: mm(1.0) }),
        other_net(Shape::Rect { c: pt(24.0, 7.5), half: P::mm(1.0, 0.5), rot: Angle::ZERO, corner: 0 }),
        other_net(Shape::Segment { a: pt(43.0, 3.0), b: pt(45.0, 3.0), w: mm(0.2) }),
        other_net(Shape::Circle { c: pt(55.5, 12.0), r: mm(0.8) }),
    ];
    let fills = [GItem::dark(Shape::Poly(pour)), GItem::dark(Shape::Poly(concave))];
    let orders: [Vec<GItem>; 2] = [fills.iter().chain(inside.iter()).cloned().collect(), inside.iter().chain(fills.iter()).cloned().collect()];
    for (n, items) in orders.iter().enumerate() {
        let bytes = write_gerber(items, &[], &GerberOpts::default()).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("%LPC"), "holes are cuts, not clear regions");
        let img = read_gerber(&bytes).unwrap_or_else(|e| panic!("{e}\n{}", String::from_utf8_lossy(&bytes)));
        let d = region_difference(&img.to_polyset(200), &items_polyset(items, 200), 1000);
        assert!(d.is_none(), "write order {n}: a track in a fill's hole survives the round trip: {d:?}");
        let want = items_polyset(&fills.iter().cloned().collect::<Vec<_>>(), 200);
        // the fills alone keep their holes
        let only: Vec<GItem> = fills.to_vec();
        let img = read_gerber(&write_gerber(&only, &[], &GerberOpts::default()).unwrap()).unwrap();
        assert!(region_difference(&img.to_polyset(200), &want, 1000).is_none(), "holes stay holes");
    }
}

fn grid_pour(cols: usize, rows: usize, stagger: bool) -> PolySet {
    let ring = |x0: f64, y0: f64, x1: f64, y1: f64| vec![pt(x0, y0), pt(x0, y1), pt(x1, y1), pt(x1, y0)];
    let mut rings = vec![vec![pt(0.0, 0.0), pt(cols as f64 * 2.0 + 2.0, 0.0), pt(cols as f64 * 2.0 + 2.0, rows as f64 * 2.0 + 2.0), pt(0.0, rows as f64 * 2.0 + 2.0)]];
    for r in 0..rows {
        for c in 0..cols {
            // every antipad of a row ends on the same y (a ray from one runs through its neighbours);
            // staggered rows shift by half a pitch, and sizes vary by column
            let x = 1.0 + c as f64 * 2.0 + if stagger && r % 2 == 1 { 0.5 } else { 0.0 };
            let y = 1.0 + r as f64 * 2.0;
            let w = 0.8 + 0.1 * (c % 3) as f64;
            rings.push(ring(x, y, x + w, y + 0.8));
        }
    }
    PolySet::from_rings_even_odd(&rings)
}

fn proper_crossings(ring: &[P]) -> usize {
    use fab_elec_core::geom::orient;
    let n = ring.len();
    let mut found = 0;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            let (c, d) = (ring[j], ring[(j + 1) % n]);
            let (o1, o2, o3, o4) = (orient(a, b, c), orient(a, b, d), orient(c, d, a), orient(c, d, b));
            if o1 * o2 < 0 && o3 * o4 < 0 {
                found += 1;
            }
        }
    }
    found
}

#[test]
fn bridge_of_a_via_antipad_grid_never_crosses_itself() {
    for (cols, rows, stagger) in [(12, 12, false), (12, 12, true), (7, 3, false)] {
        let pour = grid_pour(cols, rows, stagger);
        assert_eq!(pour.polygons().len(), 1, "one polygon with {} holes", cols * rows);
        let poly = &pour.polygons()[0];
        let ring = crate::gerber_write::bridge(poly).expect("every hole of a normalised polygon can be bridged");
        assert_eq!(ring.len(), poly.outer.len() + poly.holes.iter().map(|h| h.len() + 2).sum::<usize>(), "every vertex once, each bridge's two ends twice");
        assert_eq!(proper_crossings(&ring), 0, "{cols}x{rows} stagger {stagger}: no cut crosses another edge");
        assert_eq!(PolySet::from_ring_nonzero(ring), pour, "{cols}x{rows} stagger {stagger}: the cut-in ring is the polygon exactly");
        let items = [GItem::dark(Shape::Poly(pour.clone()))];
        let img = read_gerber(&write_gerber(&items, &[], &GerberOpts::default()).unwrap()).unwrap();
        assert_eq!(img.items.len(), 1, "one region per polygon");
        assert_eq!(img.to_polyset(200), pour, "{cols}x{rows} stagger {stagger}: the file reads back as the polygon exactly");
    }
}

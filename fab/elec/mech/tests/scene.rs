//! The scene seam (C14): one small two-board design, every property `scene_of` promises.

use fab_elec_board::{Board, Flash, Hole, Origin, Placed, Via};
use fab_elec_circuit::{Circuit, Component};
use fab_elec_core::{mm, Angle, BlockId, BoardId, CompId, ItemId, LayerId, NetId, Sel, Side, Xform3, P};
use fab_elec_design::fixture::fp;
use fab_elec_design::{Assembly, Design, Frame, Mount, Unit, UnitKind};
use fab_elec_mech::scene::{scene_of, MeshRef, NO_BODY_HEIGHT};
use fab_elec_mech::Pose;
use fab_elec_parts::{Body3d, BodyColour, BodySource, BodySurface, Fields, Footprint, PartDef, PartKind, Symbol};
use fab_materials::{Colour, FinishId, MaterialId, Surface};
use std::sync::Arc;

fn part(name: &str, footprint: &Arc<Footprint>, body: Option<Body3d>) -> Arc<PartDef> {
    Arc::new(PartDef {
        name: name.into(),
        kind: PartKind::of_name(name),
        symbol: Symbol::default(),
        footprint: Some(footprint.clone()),
        body,
        pins: Vec::new(),
        fields: Fields::default(),
        emitter: None,
        gpio: None, lifecycle: None, approved: false,
    })
}

fn comp(id: u32, refdes: &str, part: Arc<PartDef>) -> Component {
    Component {
        id: CompId(id),
        path: refdes.to_lowercase(),
        refdes: refdes.into(),
        ref_kind: Default::default(),
        part,
        value: None,
        conns: Vec::new(),
        nc: Vec::new(),
        block: BlockId(0),
        dnp: false,
        fields: Fields::default(),
    }
}

fn placed(id: u32, comp: &Component, at: P, rot: Angle, side: Side) -> Placed {
    let footprint = comp.part.footprint.clone().expect("a footprint");
    Placed {
        id: ItemId(id),
        comp: comp.id,
        path: comp.path.clone(),
        refdes: comp.refdes.clone(),
        pad_nets: vec![None; footprint.pads.len()],
        footprint,
        at,
        rot,
        side,
        locked: false,
        origin: Origin::Authored,
        body_z: Placed::band_of(comp.part.body.as_ref()),
        ref_label: None, value_label: None, legend: None,
        value: None,
    }
}

/// Board 0 with a 4-pin header (top), a body-less chip (bottom), a via and a mounting hole;
/// board 1 stacked 10 mm above it.
fn design() -> Design {
    let header_fp = Arc::new(fp::pin_header("hdr4", 4, 2.54));
    let chip_fp = Arc::new(fp::chip("r0603", "0603"));
    let tall = Body3d { source: BodySource::Extrude, offset: Xform3::IDENTITY, height: mm(8.5), ports: Vec::new(), surface: None, missing_mesh: None };
    let j1 = comp(0, "J1", part("hdr4", &header_fp, Some(tall)));
    let r1 = comp(1, "R1", part("r0603", &chip_fp, None));
    let mut b0 = Board::new(BoardId(0), "main");
    b0.placed.push(placed(1, &j1, P::mm(10.0, 5.0), Angle::QUARTER, Side::Top));
    b0.placed.push(placed(2, &r1, P::mm(20.0, 5.0), Angle::deg(0.0), Side::Bottom));
    b0.vias.push(Via { id: ItemId(3), net: NetId(0), at: P::mm(15.0, 15.0), drill: mm(0.3), pad: mm(0.6), span: (LayerId::F_CU, LayerId::B_CU), kind: Default::default(), tent: Default::default(), backdrill: None, locked: false, origin: Origin::Authored, flash: Flash::All });
    b0.holes.push(Hole { id: ItemId(4), name: "mount1".into(), at: P::mm(3.0, 3.0), drill: mm(3.2), pad: None, net: None, plated: false, origin: Origin::Authored });
    let c0 = Circuit { unit: BoardId(0), name: "main".into(), comps: vec![j1, r1], ..Circuit::default() };
    let b1 = Board::new(BoardId(1), "panel");
    let c1 = Circuit { unit: BoardId(1), name: "panel".into(), ..Circuit::default() };
    let unit = |id: u16, name: &str, c: Circuit, b: Board| Unit { id: BoardId(id), name: name.into(), kind: UnitKind::Board, circuit: Arc::new(c), supplied: Vec::new(), board: Some(Arc::new(b)), factory: None };
    Design {
        units: vec![unit(0, "main", c0, b0), unit(1, "panel", c1, b1)],
        assembly: Assembly {
            mounts: vec![
                Mount { unit: BoardId(0), parent: Frame::World, xform: Xform3::translation([mm(100.0), 0, 0]) },
                Mount { unit: BoardId(1), parent: Frame::Unit(BoardId(0)), xform: Xform3::translation([0, 0, mm(10.0)]) },
            ],
            ..Assembly::default()
        },
        ..Design::default()
    }
}

#[test]
fn the_scene_places_boards_holes_and_bodies() {
    let d = design();
    let s = scene_of(&d, &Pose::default());
    assert_eq!(s.boards.len(), 2, "both board units");
    let (b0, b1) = (&s.boards[0], &s.boards[1]);
    assert_eq!(b1.pose.apply([0, 0, 0]), [mm(100.0), 0, mm(10.0)], "a mount on a unit follows that unit's frame");
    let t = b0.thickness;
    assert!(t > 0, "the stackup's thickness");

    // holes in `Board::drills` order: the header's four pads, the via, the mounting hole
    assert_eq!(b0.holes.len(), 6, "board holes, vias and through-hole pads");
    assert!(b0.holes[4].plated && !b0.holes[5].plated, "a plated via, a bare mounting hole");
    assert!(b0.holes[..4].iter().all(|h| h.plated && matches!(h.sel, Sel::Feature(_))), "header pads are plated barrels");
    let pads: Vec<P> = b0.holes[..4].iter().map(|h| h.at).collect();
    assert!(pads.iter().all(|p| p.y == mm(5.0)) && pads[3].x == mm(10.0 + 3.0 * 2.54), "a header along the footprint's -Y, quarter-turned, runs along the board's +X");

    // bodies: the header's extruded courtyard on top, the chip's placeholder box below
    let j1 = &b0.bodies[0];
    assert_eq!(j1.surface, None, "a body without a stated surface keeps it unspecified");
    assert!(matches!(&j1.mesh, MeshRef::Extrude { height, .. } if *height == mm(8.5)), "a stated body keeps its height");
    assert_eq!(j1.pose.apply([0, 0, 0])[2], t, "a top part stands on the top face");
    let r1 = &b0.bodies[1];
    assert_eq!(r1.surface, None, "a placeholder body has no stated surface");
    assert!(matches!(r1.mesh, MeshRef::Box { max, .. } if max[2] == NO_BODY_HEIGHT), "no body: a box of the footprint");
    assert_eq!(r1.pose.apply([0, 0, 0])[2], 0, "a bottom part sits on the bottom face");
    assert!(r1.pose.apply([0, 0, mm(1.0)])[2] < 0, "and hangs below the board");
    assert_eq!(r1.pose.apply([mm(1.0), 0, 0]), [mm(19.0), mm(5.0), 0], "the footprint's X mirrored, as the 2D board maps it");

    // keys: a pose moves without new content; an edit is new content
    let mut moved = d.clone();
    moved.assembly.mounts[0].xform = Xform3::translation([mm(50.0), 0, 0]);
    assert_eq!(scene_of(&moved, &Pose::default()).hash, s.hash, "a pose change keeps the content key");
    let mut edited = d.clone();
    let mut b = (**edited.units[0].board.as_ref().unwrap()).clone();
    b.holes[0].drill = mm(3.4);
    edited.units[0].board = Some(Arc::new(b));
    let e = scene_of(&edited, &Pose::default());
    assert_ne!(e.hash, s.hash, "an edit changes the content key");
    assert_ne!(e.boards[0].art.hash, b0.art.hash, "and the board's art key");
    assert_eq!(e.boards[1].art.hash, b1.art.hash, "and no other board's");

    let mut surfaced = d.clone();
    let natural = Surface::new(MaterialId::lookup("stainless_303").expect("the stated material exists"));
    let colour = fab_materials::swatch::colour_by_name("smoke").expect("the smoke swatch exists");
    let cases = [
        ("material", BodySurface { material: "stainless_303".into(), finish: None, colour: None }, natural),
        ("finish", BodySurface { material: "stainless_303".into(), finish: Some("spi_a1".into()), colour: None },
            Surface { finish: FinishId::lookup("spi_a1").expect("the stated finish exists"), ..natural }),
        ("colour", BodySurface { material: "stainless_303".into(), finish: Some("spi_a1".into()),
            colour: Some(BodyColour { srgb: colour.srgb, translucency: colour.translucency, ral: colour.ral }) },
            Surface { finish: FinishId::lookup("spi_a1").expect("the stated finish exists"),
                colour: Colour { srgb: colour.srgb, translucency: colour.translucency, ral: colour.ral }, ..natural }),
    ];
    let mut previous_hash = s.hash;
    for (property, stated, expected) in cases {
        let circuit = Arc::make_mut(&mut surfaced.units[0].circuit);
        let part = Arc::make_mut(&mut circuit.comps[0].part);
        part.body.as_mut().expect("the header has a body").surface = Some(stated);
        let scene = scene_of(&surfaced, &Pose::default());
        assert_eq!(scene.boards[0].bodies[0].surface, Some(expected), "{property}: the scene resolves stated fields and material defaults");
        assert_eq!(scene.boards[0].bodies[1].surface, None, "{property}: another body's surface remains unspecified");
        assert_ne!(scene.hash, previous_hash, "{property}: changing a body surface changes the scene content key");
        assert_eq!(scene.boards[0].art.hash, b0.art.hash, "{property}: a surface change keeps the board's art key");
        previous_hash = scene.hash;
    }
}

#[test]
fn switch_scene_ports_share_the_mesh_frame_and_match_board_mechanics() {
    let mut design = fab_elec_fixtures::ledctl();
    let unit = design.units.iter_mut().find(|u| u.board.is_some()).expect("fixture has a board");
    let switch = unit.circuit.comps.iter().find(|c| c.part.name == "SW_TACT_4PAD")
        .expect("fixture has std's tactile switch").id;
    let part = Arc::make_mut(&mut Arc::make_mut(&mut unit.circuit).comps[switch.ix()].part);
    let body = part.body.as_mut().expect("switch has a body");
    body.offset = Xform3::axis_angle([mm(0.7), mm(-0.4), mm(0.2)], [1.0, 0.0, 0.0], Angle::deg(25.0));
    let local = body.ports.iter().find(|p| p.kind == fab_elec_parts::BodyPortKind::Press)
        .expect("switch states its press port").clone();
    let board = Arc::make_mut(unit.board.as_mut().unwrap());
    let placed = board.placed.iter_mut().find(|p| p.comp == switch).expect("switch is placed");
    placed.rot = Angle::deg(37.0);
    placed.side = Side::Bottom;
    let board_id = unit.id;
    let mechanics = fab_elec_mech::board_mech(board, &unit.circuit);
    let placed_port = mechanics.ports.iter().find(|p| p.comp == switch).expect("mechanics places the press port");
    let scene = scene_of(&design, &Pose::default());
    let shown = scene.boards.iter().find(|b| b.unit == board_id).unwrap().bodies.iter()
        .find(|b| b.sel == Sel::Comp(fab_elec_core::CompRef { board: board_id, comp: switch }))
        .expect("scene carries the switch");
    assert_eq!(shown.ports, vec![local.clone()], "ports retain the mesh's body-local frame before its offset");
    assert_eq!(shown.pose.apply(shown.ports[0].at), placed_port.at, "body pose gives board_mech's port position, including offset, rotation and bottom side");
    let dir = shown.pose.rotate(shown.ports[0].dir);
    assert!(dir.iter().zip(placed_port.dir).all(|(a, b)| (a - b).abs() < 1e-12), "body pose also gives board_mech's port direction");
    assert_eq!((local.stand, local.travel), (mm(0.3), mm(0.25)), "scene keeps std's plunger dimensions");
}

/// Generated package bodies (E3D bodies): the handheld is built once, every package body it
/// states is generated, and each family's row checks the result against the part's footprint.
/// A row per family; each assertion names the part and its family.
#[test]
fn package_bodies_match_their_footprints() {
    use fab_elec_core::to_mm;
    use fab_elec_mech::package::{package_meshes, QFN_STANDOFF_MM};
    use fab_elec_parts::PackageKind;
    let sources = fab_elec_fixtures::docs("handheld").expect("handheld is a registered stock project");
    let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs())
        .unwrap_or_else(|diags| panic!("handheld documents evaluate: {diags:?}"));
    let scene = scene_of(&evaluated.design, &Pose::default());
    let mut families = Vec::new();
    for board in &scene.boards {
        let unit = evaluated.design.units.iter().find(|u| u.id == board.unit).expect("each scene board is a design unit");
        for body in &board.bodies {
            let MeshRef::Package(p) = &body.mesh else { continue };
            let Sel::Comp(comp) = &body.sel else { panic!("a package body belongs to a component") };
            let part = &unit.circuit.comps[comp.comp.ix()].path;
            let family = p.kind;
            families.push(family);
            let meshes = package_meshes(p).unwrap_or_else(|e| panic!("{part} ({family:?}): the body generates: {e}"));
            let (mesh, _) = meshes.first().unwrap_or_else(|| panic!("{part} ({family:?}): a moulded body"));
            let points: Vec<[f64; 3]> = mesh.vertices.chunks_exact(8).map(|v| [v[0] as f64, v[1] as f64, v[2] as f64]).collect();
            let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
            for q in &points {
                for i in 0..3 {
                    lo[i] = lo[i].min(q[i]);
                    hi[i] = hi[i].max(q[i]);
                }
            }
            let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
            match family {
                PackageKind::Qfn => {
                    use fab_elec_mech::package::{QFN_TERMINAL_LEN_MM, QFN_TERMINAL_T_MM};
                    let (min, max, top) = ([to_mm(p.body[0].x), to_mm(p.body[0].y)], [to_mm(p.body[1].x), to_mm(p.body[1].y)], to_mm(p.height));
                    assert!(near(lo[0], min[0]) && near(lo[1], min[1]) && near(hi[0], max[0]) && near(hi[1], max[1]),
                        "{part} (Qfn): the body box {:?}..{:?} is the footprint's {min:?}..{max:?}", &lo[..2], &hi[..2]);
                    assert!(near(lo[2], QFN_STANDOFF_MM) && near(hi[2], top),
                        "{part} (Qfn): the body spans the stand-off to the part's height, got {}..{} for {QFN_STANDOFF_MM}..{top}", lo[2], hi[2]);
                    let pin1 = p.pin1().unwrap_or_else(|| panic!("{part} (Qfn): the footprint has pad 1"));
                    let sunk: Vec<&[f64; 3]> = points.iter().filter(|q| q[2] < top - 1e-3 && q[2] > QFN_TERMINAL_T_MM + 1e-3).collect();
                    assert!(!sunk.is_empty(), "{part} (Qfn): the pin-1 mark sinks into the top");
                    let centre = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0];
                    let side = |v: f64, c: f64| v < c;
                    let (px, py) = (to_mm(pin1.at.x), to_mm(pin1.at.y));
                    assert!(sunk.iter().all(|q| side(q[0], centre[0]) == side(px, centre[0]) && side(q[1], centre[1]) == side(py, centre[1])),
                        "{part} (Qfn): the pin-1 mark is in pad 1's corner of the body");

                    assert_eq!(meshes.len(), 2, "{part} (Qfn): the moulding and terminals each have a mesh");
                    let terminals: Vec<[f64; 3]> = meshes[1].0.vertices.chunks_exact(8)
                        .map(|v| [v[0] as f64, v[1] as f64, v[2] as f64]).collect();
                    for q in &terminals {
                        assert!((0..2).all(|i| q[i] >= min[i] - 1e-3 && q[i] <= max[i] + 1e-3)
                            && q[2] >= -1e-3 && q[2] <= QFN_TERMINAL_T_MM + 1e-3,
                            "{part} (Qfn): terminal vertex {q:?} exceeds the body outline or terminal thickness");
                    }
                    for pad in &p.pads {
                        let pad_min = [to_mm(pad.min.x), to_mm(pad.min.y)];
                        let pad_max = [to_mm(pad.max.x), to_mm(pad.max.y)];
                        if (0..2).all(|i| pad_min[i] <= centre[i] && centre[i] <= pad_max[i]) {
                            // The exposed pad has no metal in the interior beyond the edge terminals.
                            assert!(!terminals.iter().any(|q| (0..2).all(|i|
                                q[i] >= pad_min[i] - 1e-3 && q[i] <= pad_max[i] + 1e-3
                                && q[i] > min[i] + QFN_TERMINAL_LEN_MM + 1e-3
                                && q[i] < max[i] - QFN_TERMINAL_LEN_MM - 1e-3)),
                                "{part} (Qfn): a terminal vertex lies on exposed pad {}", pad.no);
                            continue;
                        }
                        let (x, y) = (to_mm(pad.at.x), to_mm(pad.at.y));
                        let edge_x = if x <= centre[0] { min[0] } else { max[0] };
                        let edge_y = if y <= centre[1] { min[1] } else { max[1] };
                        let on_x = (x - edge_x).abs() <= (y - edge_y).abs();
                        let along = if on_x { 1 } else { 0 };
                        for end in [pad_min[along].max(min[along]), pad_max[along].min(max[along])] {
                            for z in [0.0, QFN_TERMINAL_T_MM] {
                                let corner = if on_x { [edge_x, end, z] } else { [end, edge_y, z] };
                                assert!(terminals.iter().any(|q| (0..3).all(|i| near(q[i], corner[i]))),
                                    "{part} (Qfn): terminal for pad {} is missing outer corner {corner:?}", pad.no);
                            }
                        }
                    }
                }
                PackageKind::Sop => {
                    use fab_elec_mech::package::SOP_STANDOFF_MM;
                    use fab_elec_mech::scene::PackagePad;
                    let (min, max, top) = ([to_mm(p.body[0].x), to_mm(p.body[0].y)], [to_mm(p.body[1].x), to_mm(p.body[1].y)], to_mm(p.height));
                    assert!(near(lo[0], min[0]) && near(lo[1], min[1]) && near(hi[0], max[0]) && near(hi[1], max[1]),
                        "{part} (Sop): the body box {:?}..{:?} is the footprint's {min:?}..{max:?}", &lo[..2], &hi[..2]);
                    assert!(near(lo[2], SOP_STANDOFF_MM) && near(hi[2], top),
                        "{part} (Sop): the body spans the stand-off to the part's height, got {}..{} for {SOP_STANDOFF_MM}..{top}", lo[2], hi[2]);
                    let pin1 = p.pin1().unwrap_or_else(|| panic!("{part} (Sop): the footprint has pad 1"));
                    let sunk: Vec<_> = points.iter().filter(|q| q[2] > SOP_STANDOFF_MM + 1e-3 && q[2] < top - 1e-3).collect();
                    assert!(!sunk.is_empty(), "{part} (Sop): the pin-1 mark sinks into the top");
                    let centre = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0];
                    let (px, py) = (to_mm(pin1.at.x), to_mm(pin1.at.y));
                    assert!(sunk.iter().all(|q| (q[0] < centre[0]) == (px < centre[0]) && (q[1] < centre[1]) == (py < centre[1])),
                        "{part} (Sop): the pin-1 mark is in pad 1's corner of the body");

                    assert_eq!(meshes.len(), 2, "{part} (Sop): the moulding and leads each have a mesh");
                    let in_pad = |xy: [f64; 2], pad: &PackagePad, tol: f64| {
                        xy[0] >= to_mm(pad.min.x) - tol && xy[0] <= to_mm(pad.max.x) + tol
                            && xy[1] >= to_mm(pad.min.y) - tol && xy[1] <= to_mm(pad.max.y) + tol
                    };
                    let mut feet = Vec::new();
                    for v in meshes[1].0.vertices.chunks_exact(8) {
                        let (xy, z) = ([v[0] as f64, v[1] as f64], v[2] as f64);
                        assert!(z >= -1e-3 && z <= top + 1e-3,
                            "{part} (Sop): lead vertex at {xy:?}, z={z} is outside the board-to-top span 0..{top}");
                        if near(z, 0.0) {
                            assert!(p.pads.iter().any(|pad| in_pad(xy, pad, 1e-3)),
                                "{part} (Sop): lead foot vertex at {xy:?} lands outside every pad");
                            feet.push(xy);
                        }
                    }
                    for pad in &p.pads {
                        if in_pad(centre, pad, 0.0) {
                            continue;
                        }
                        assert!(feet.iter().any(|&xy| in_pad(xy, pad, 1e-3)),
                            "{part} (Sop): pad {} has no lead foot on the board", pad.no);
                    }
                }
                PackageKind::Chip => {
                    use fab_elec_mech::package::{CHIP_CAP_SHARE, CHIP_INSET_MM};
                    use fab_materials::part_look::PartClass;
                    let [min, max] = p.body.map(|q| [to_mm(q.x), to_mm(q.y)]);
                    let top = to_mm(p.height);
                    let inset = CHIP_INSET_MM;
                    let has_film = matches!(p.class, PartClass::Resistor | PartClass::Fuse);
                    assert_eq!(meshes.len(), if has_film { 3 } else { 2 },
                        "{part} (Chip): mesh count for the block, end caps and any film on a {:?}", p.class);
                    assert!((0..2).all(|i| near(lo[i], min[i] + inset) && near(hi[i], max[i] - inset)),
                        "{part} (Chip): block bounds {lo:?}..{hi:?} must be inset {inset} mm from outline {min:?}..{max:?}");
                    assert!(near(lo[2], inset) && near(hi[2], top - inset),
                        "{part} (Chip): block height {}..{} must be inset {inset} mm from 0..{top}", lo[2], hi[2]);
                    let material = match p.class {
                        PartClass::Capacitor => "ceramic_mlcc",
                        PartClass::Inductor => "ferrite",
                        _ => "alumina",
                    };
                    assert_eq!(Some(meshes[0].1.material), MaterialId::lookup(material),
                        "{part} (Chip): the {:?} block must use {material}", p.class);

                    let (first, second) = (p.pads[0].at, p.pads[1].at);
                    let a = if first.x.abs_diff(second.x) > first.y.abs_diff(second.y) { 0 } else { 1 };
                    let cap_limit = CHIP_CAP_SHARE * (max[a] - min[a]) + 1e-3;
                    let cap_vertices = meshes[meshes.len() - 1].0.vertices.chunks_exact(8);
                    for v in cap_vertices.clone() {
                        let along = f64::from(v[a]);
                        assert!((along - min[a]).abs().min((along - max[a]).abs()) <= cap_limit,
                            "{part} (Chip): cap vertex at {along} exceeds {cap_limit} mm from the nearest end");
                    }
                    // The extrema along the pad axis must reach both end faces.
                    for (axis, low, high) in [(0, min[0], max[0]), (1, min[1], max[1]), (2, 0.0, top)] {
                        let coords = cap_vertices.clone().map(|v| f64::from(v[axis]));
                        let cap_min = coords.clone().fold(f64::INFINITY, f64::min);
                        let cap_max = coords.fold(f64::NEG_INFINITY, f64::max);
                        assert!(near(cap_min, low) && near(cap_max, high),
                            "{part} (Chip): end cap vertices span {cap_min}..{cap_max} on axis {axis}, expected {low}..{high}");
                    }
                    if has_film {
                        let film = &meshes[1].0.vertices;
                        let z = (top - 2.0 * inset - 1e-3)..=(top - inset / 2.0 + 1e-3);
                        assert!(!film.is_empty() && film.chunks_exact(8).all(|v| z.contains(&f64::from(v[2]))),
                            "{part} (Chip): the film must have vertices within the height range {z:?}");
                    }
                }
                PackageKind::Display => {
                    use fab_elec_core::BBox;
                    use fab_elec_mech::mesh::BODY_VERTEX_FLOATS;
                    use fab_elec_mech::package::{
                        display_window, DISPLAY_ACTIVE_RECESS_MM, DISPLAY_BACK_MM, DISPLAY_BEZEL_MM,
                        DISPLAY_CHIN_MM, DISPLAY_FLEX_T_MM,
                    };

                    assert_eq!(meshes.len(), 4, "{part} (Display): expected meshes for cover, active area, back and flex");
                    let [min, max] = p.body.map(|q| [to_mm(q.x), to_mm(q.y)]);
                    let top = to_mm(p.height);
                    assert!((0..2).all(|i| near(lo[i], min[i]) && near(hi[i], max[i])),
                        "{part} (Display): cover bounds {lo:?}..{hi:?} must match the module outline {min:?}..{max:?} in xy");
                    assert!(near(lo[2], DISPLAY_BACK_MM) && near(hi[2], top),
                        "{part} (Display): cover height {}..{} must span {DISPLAY_BACK_MM}..{top}", lo[2], hi[2]);

                    let (window_min, window_max) = display_window(p)
                        .unwrap_or_else(|| panic!("{part} (Display): the panel must have an active window"));
                    let span = |vertices: &[f32], axis: usize| {
                        let coords = vertices.chunks_exact(BODY_VERTEX_FLOATS).map(|v| f64::from(v[axis]));
                        (coords.clone().fold(f64::INFINITY, f64::min), coords.fold(f64::NEG_INFINITY, f64::max))
                    };
                    let active = &meshes[1].0.vertices;
                    for i in 0..2 {
                        let (low, high) = span(active, i);
                        assert!(near(low, window_min[i]) && near(high, window_max[i]),
                            "{part} (Display): active glass spans {low}..{high} on axis {i}, expected {}..{}", window_min[i], window_max[i]);
                    }
                    let (_, active_top) = span(active, 2);
                    assert!(near(active_top, top - DISPLAY_ACTIVE_RECESS_MM),
                        "{part} (Display): active glass top {active_top} must sit {DISPLAY_ACTIVE_RECESS_MM} mm below {top}");
                    assert!(near(window_min[0] - min[0], DISPLAY_BEZEL_MM) && near(max[0] - window_max[0], DISPLAY_BEZEL_MM)
                        && near(max[1] - window_max[1], DISPLAY_BEZEL_MM) && near(window_min[1] - min[1], DISPLAY_CHIN_MM),
                        "{part} (Display): window {window_min:?}..{window_max:?} needs a {DISPLAY_CHIN_MM} mm lower border toward the contacts and {DISPLAY_BEZEL_MM} mm on the other three sides");

                    let flex = &meshes[3].0.vertices;
                    assert!(!flex.is_empty() && flex.chunks_exact(BODY_VERTEX_FLOATS)
                        .all(|v| (0.0..=DISPLAY_FLEX_T_MM as f32).contains(&v[2])),
                        "{part} (Display): flex vertices must lie between the board and {DISPLAY_FLEX_T_MM} mm");
                    let (x0, x1) = span(flex, 0);
                    let (y0, y1) = span(flex, 1);
                    let outline = BBox { min: p.body[0], max: p.body[1] };
                    for pad in &p.pads {
                        if outline.intersects(&BBox { min: pad.min, max: pad.max }) {
                            continue;
                        }
                        assert!(to_mm(pad.min.x) >= x0 - 1e-3 && to_mm(pad.max.x) <= x1 + 1e-3
                            && to_mm(pad.min.y) >= y0 - 1e-3 && to_mm(pad.max.y) <= y1 + 1e-3,
                            "{part} (Display): flex bounds [{x0}, {y0}]..[{x1}, {y1}] must cover contact pad {}", pad.no);
                    }
                }
            }
        }
    }
    assert!(families.contains(&PackageKind::Qfn), "the handheld's MCU states a QFN body: {families:?}");
    assert!(families.contains(&PackageKind::Sop), "the handheld's expander and regulator state SOP bodies: {families:?}");
    assert!(families.contains(&PackageKind::Chip), "the handheld's resistors and capacitors state Chip bodies: {families:?}");
    assert!(families.contains(&PackageKind::Display), "the handheld's display panel states a Display body: {families:?}");
}

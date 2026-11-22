use super::{fill_generic_tables, BottomX, PlacementOrigin, PlacementRules};
use crate::model::{render, CamModel, Pose};
use crate::profile::{BomRow, CheckKind, FileKind, ResolvedProfile};
use crate::validate::validate;
use fab_elec_board::{Board, Origin, Placed};
use fab_elec_circuit::{Circuit, Component, Value};
use fab_elec_core::{Angle, BlockId, CompId, Contour, ItemId, Side, P};
use fab_elec_parts::{Fields, Footprint, PartDef, Symbol};
use std::sync::Arc;

#[test]
fn generic_tables_group_sort_transform_and_validate() {
    let footprint = Arc::new(Footprint { name: "R_0603".into(), ..Default::default() });
    let part = Arc::new(PartDef {
        name: "Resistor".into(),
        kind: fab_elec_parts::PartKind::Resistor,
        symbol: Symbol::default(),
        footprint: Some(Arc::new(Footprint { name: "library-footprint".into(), ..Default::default() })),
        body: None,
        pins: Vec::new(),
        fields: Fields::default(),
        emitter: None,
        gpio: None,
    });
    let mut circuit = Circuit::default();
    for (id, refdes, value, mpn, dnp) in [
        (0, "R1", "1k", "RES-1K", true),
        (1, "R2", "10k, 1%", "RES-10K", false),
        (2, "R10", "10k, 1%", "RES-10K", false),
    ] {
        circuit.comps.push(Component {
            id: CompId(id),
            path: refdes.into(),
            refdes: refdes.into(),
            ref_kind: Default::default(),
            part: part.clone(),
            value: Some(Value { text: value.into(), qty: None, tol: None }),
            conns: Vec::new(),
            nc: Vec::new(),
            block: BlockId(0),
            dnp,
            fields: Fields(vec![("mpn".into(), mpn.into())]),
        });
    }
    let mut board = Board {
        outline: vec![Contour::rect(P::mm(10.0, -5.0), P::mm(30.0, 15.0), 0)],
        next_item: 3,
        ..Default::default()
    };
    for (id, refdes, at, rot, side) in [
        (2, "R10", P::mm(14.0, -2.25), Angle::deg(67.5), Side::Bottom),
        (0, "R1", P::mm(15.0, 0.0), Angle::ZERO, Side::Top),
        (1, "R2", P::mm(11.2346, -3.5), Angle::deg(350.0), Side::Top),
    ] {
        board.placed.push(Placed {
            id: ItemId(id),
            comp: CompId(id),
            path: refdes.into(),
            refdes: refdes.into(),
            footprint: footprint.clone(),
            pad_nets: Vec::new(),
            at,
            rot,
            side,
            locked: false,
            origin: Origin::Authored,
            body_z: None,
            ref_label: None, value_label: None, legend: None,
            value: None,
        });
    }
    let mut profile = ResolvedProfile::standard_2_layer("board", "A");
    profile.files.retain(|f| matches!(f.kind, FileKind::Bom | FileKind::Placement));
    let bom_header = profile.bom.header.clone();
    let placement_header = profile.placement.header.clone();
    let rules = PlacementRules {
        bottom_x: BottomX::MirrorAbout(board.outline_bbox().width() as i32),
        rotation_offsets: vec![("R_".into(), Angle::deg(22.5)), ("R_0603".into(), Angle::deg(180.0))],
        ..Default::default()
    };
    fill_generic_tables(&mut profile, &board, &circuit, &rules);
    assert_eq!(profile.bom.header, bom_header, "BOM header retained");
    assert_eq!(profile.placement.header, placement_header, "placement header retained");
    assert_eq!(profile.bom.rows, vec![BomRow {
        cells: vec!["10k, 1%".into(), "R2,R10".into(), "R_0603".into(), "RES-10K".into()],
        refs: vec!["R2".into(), "R10".into()],
    }], "DNP omitted, equal parts grouped, references naturally sorted, placed footprint used");
    assert_eq!(profile.placement.rows.len(), 2, "DNP omitted from placement");
    let top = &profile.placement.rows[0];
    let bottom = &profile.placement.rows[1];
    assert_eq!(top.refdes, "R2", "natural placement order");
    assert_eq!(bottom.refdes, "R10", "natural placement order");
    assert_eq!(top.cells, ["R2", "1.235", "1.500", "T", "12.5"], "lower-left origin, rounded millimetres and normalised rotation");
    assert_eq!(bottom.cells, ["R10", "16.000", "2.750", "B", "270"], "bottom X and exported rotation mirrored");
    assert_eq!(top.at, P::mm(1.2346, 1.5), "position keeps nanometre precision");
    assert_eq!(bottom.at, P::mm(16.0, 2.75), "mirrored position in board-width frame");
    assert_eq!(top.rot, Angle::deg(12.5), "rotation wraps past 360 degrees");
    assert_eq!(bottom.rot, Angle::deg(90.0), "stored rotation remains compatible with the validator");
    for row in &profile.placement.rows {
        assert_eq!(row.rot_offset, Angle::deg(22.5), "first matching prefix wins");
    }
    assert_eq!(top.side, Side::Top, "top side retained");
    assert_eq!(bottom.side, Side::Bottom, "bottom side retained");

    let mut model = CamModel {
        copper_layers: 2,
        poses: vec![
            Pose { refdes: "R2".into(), at: P::mm(11.2346, -3.5), rot: Angle::deg(350.0), side: Side::Top },
            Pose { refdes: "R10".into(), at: P::mm(14.0, -2.25), rot: Angle::deg(67.5), side: Side::Bottom },
        ],
        bom_refs: vec!["R2".into(), "R10".into()],
        ..Default::default()
    };
    let validation = validate(&render(&model, &profile).unwrap(), &profile, &model);
    assert!(validation.ok(), "assembly tables validate: {:?}", validation.checks);
    for kind in [CheckKind::Bom, CheckKind::Placement] {
        assert!(validation.checks.iter().any(|c| c.kind == kind && c.ok), "{kind:?} check ran and passed");
    }

    // An unplaced part still belongs to the BOM, and a different origin/convention
    // replaces the old placement rows while preserving the table's column choices.
    circuit.comps[0].dnp = false;
    circuit.comps[2].fields = Fields(vec![("mpn".into(), "RES-10K-ALT".into())]);
    board.placed.retain(|p| p.comp != CompId(0));
    profile.bom.header = ["Part", "Footprint", "Designator", "Comment"].iter().map(|s| s.to_string()).collect();
    profile.bom.delimiter = ';';
    let absolute = PlacementRules {
        origin: PlacementOrigin::Absolute,
        bottom_mirrored: false,
        side_words: ("Top".into(), "Bottom".into()),
        ..Default::default()
    };
    fill_generic_tables(&mut profile, &board, &circuit, &absolute);
    assert_eq!(profile.bom.rows, vec![
        BomRow { cells: vec!["RES-1K".into(), "".into(), "R1".into(), "1k".into()], refs: vec!["R1".into()] },
        BomRow { cells: vec!["RES-10K".into(), "R_0603".into(), "R2".into(), "10k, 1%".into()], refs: vec!["R2".into()] },
        BomRow { cells: vec!["RES-10K-ALT".into(), "R_0603".into(), "R10".into(), "10k, 1%".into()], refs: vec!["R10".into()] },
    ], "unplaced part has no footprint; distinct MPNs split groups; rows follow natural reference order and the existing header");
    assert_eq!(profile.bom.delimiter, ';', "delimiter retained");
    assert_eq!(profile.placement.rows.len(), 2, "unplaced part has no placement row; rows replaced");
    assert_eq!(profile.placement.rows[0].cells, ["R2", "11.235", "-3.500", "Top", "350"], "absolute top placement");
    assert_eq!(profile.placement.rows[1].cells, ["R10", "14.000", "-2.250", "Bottom", "67.5"], "bottom mirroring disabled");
    for row in &profile.placement.rows {
        assert_eq!(row.rot_offset, Angle::ZERO, "no prefix means no correction");
    }
    model.bom_refs.push("R1".into());
    let validation = validate(&render(&model, &profile).unwrap(), &profile, &model);
    assert!(validation.ok(), "unplaced BOM part and absolute placement validate: {:?}", validation.checks);
}

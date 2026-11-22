use crate::export::{export, ReadBack};
use crate::image::Geom;
use crate::profile::{BomRow, FileKind, ResolvedProfile};
use crate::tables::{fill_generic_tables, PlacementRules};
use fab_elec_core::{Angle, Contour, LayerId, Shape, Side, P};
use fab_elec_fixtures::ledctl;
use fab_elec_parts::Fields;
use std::sync::Arc;

fn bom_row<'a>(profile: &'a ResolvedProfile, refdes: &str) -> &'a BomRow {
    profile.bom.rows.iter().find(|row| row.refs.iter().any(|r| r == refdes))
        .expect("BOM contains the requested component")
}

#[test]
fn power_flag_is_absent_from_boms_and_their_validation_references() {
    let mut unit = fab_elec_design::fixture::probe::probe();
    let circuit = Arc::make_mut(&mut unit.circuit);
    let flag = circuit.comps.iter_mut().find(|c| c.refdes == "R1").expect("the probe has R1");
    let part = Arc::make_mut(&mut flag.part);
    part.kind = fab_elec_parts::PartKind::PowerFlag;
    part.footprint = None;
    let board = Arc::make_mut(unit.board.as_mut().expect("the probe has a board"));
    board.placed.retain(|p| p.refdes == "J1");
    board.reindex();

    let mut profile = ResolvedProfile::jlc_2_layer("probe", "A");
    fill_generic_tables(&mut profile, board, circuit, &PlacementRules::jlc());
    let refs: Vec<_> = profile.bom.rows.iter().flat_map(|row| row.refs.iter().map(String::as_str)).collect();
    assert_eq!(refs, ["D1", "J1"], "the assembly BOM includes the unplaced LED and the placed header only");
    let bom = crate::tables::bom_from_circuit(circuit, Some(board));
    let refs: Vec<_> = bom.items.iter().map(|item| item.reference.as_str()).collect();
    assert_eq!(refs, ["J1", "D1"], "the schematic BOM excludes the flag even with an ordinary reference");
    let model = crate::plot::build_model(board, circuit, &[]);
    assert_eq!(model.bom_refs, ["J1", "D1"], "the BOM validator expects both physical parts and omits the flag");
    let placed: Vec<_> = profile.placement.rows.iter().map(|row| row.refdes.as_str()).collect();
    assert_eq!(placed, ["J1"], "the position file contains only the placed header");
}

#[test]
fn jlc_bom_groups_by_merged_mpn_and_lcsc_number() {
    let design = ledctl();
    let unit = &design.units[0];
    let board = unit.board.as_deref().expect("ledctl has a board");
    let mut circuit = (*unit.circuit).clone();
    for comp in &mut circuit.comps {
        Arc::make_mut(&mut comp.part).fields = Fields::default();
        comp.fields = match comp.refdes.as_str() {
            "D1" | "D2" => Fields(vec![("supplier".into(), "lcsc".into()), ("supplier_part".into(), "C1".into())]),
            "D3" => Fields(vec![("lcsc".into(), "C2".into())]),
            _ => Fields::default(),
        };
    }
    let rules = PlacementRules::jlc();
    let mut profile = ResolvedProfile::jlc_2_layer("ledctl", "J");
    fill_generic_tables(&mut profile, board, &circuit, &rules);
    assert_eq!(profile.bom.header, ["Comment", "Designator", "Footprint", "LCSC Part #"], "JLC BOM headers");
    let first = bom_row(&profile, "D1");
    let third = bom_row(&profile, "D3");
    assert_eq!(first.refs, ["D1", "D2"], "equal LCSC numbers share one naturally sorted row");
    assert_eq!(first.cells[1], "D1,D2", "grouped designator cell contains both components");
    assert_eq!(first.cells[3], "C1", "supplier_part supplies the LCSC cell");
    assert_eq!(third.refs, ["D3"], "a different LCSC number has its own row");
    assert_eq!(third.cells[3], "C2", "the lcsc field supplies the fallback LCSC cell");
    assert_eq!(first.cells[0], third.cells[0], "LCSC split exercises equal values");
    assert_eq!(first.cells[2], third.cells[2], "LCSC split exercises equal footprints");
    for comp in circuit.comps.iter().filter(|c| !matches!(c.refdes.as_str(), "D1" | "D2" | "D3")) {
        assert_eq!(bom_row(&profile, &comp.refdes).cells[3], "", "{}: bare components have no LCSC number", comp.refdes);
    }

    let original_rows = profile.bom.rows.clone();
    profile.bom.header = ["lCsC pArT #", "LcSc PaRt", "lCsC", "jLcPcB pArT #"].iter().map(|s| s.to_string()).collect();
    fill_generic_tables(&mut profile, board, &circuit, &rules);
    assert_eq!(profile.bom.rows.len(), original_rows.len(), "aliases preserve the number of BOM groups");
    for (row, original) in profile.bom.rows.iter().zip(&original_rows) {
        assert_eq!(row.refs, original.refs, "aliases preserve group membership");
        assert_eq!(row.cells, vec![original.cells[3].clone(); 4], "all supplier header aliases ignore case");
    }

    // Inherited fields and component overrides must drive both the group key and its cells.
    for comp in circuit.comps.iter_mut().filter(|c| matches!(c.refdes.as_str(), "D1" | "D2" | "D3")) {
        Arc::make_mut(&mut comp.part).fields = Fields(vec![
            ("supplier".into(), "LcSc".into()), ("supplier_part".into(), "C1".into()), ("mpn".into(), "PART-A".into()),
        ]);
        comp.fields = match comp.refdes.as_str() {
            "D1" => Fields::default(),
            "D2" => {
                Arc::make_mut(&mut comp.part).fields = Fields(vec![
                    ("supplier".into(), "other".into()), ("supplier_part".into(), "C9".into()),
                    ("mpn".into(), "PART-B".into()),
                ]);
                Fields(vec![
                    ("supplier".into(), "LCSC".into()), ("supplier_part".into(), "C1".into()),
                    ("mpn".into(), "PART-A".into()), ("lcsc".into(), "C9".into()),
                ])
            }
            _ => Fields(vec![("mpn".into(), "PART-B".into())]),
        };
    }
    profile = ResolvedProfile::jlc_2_layer("ledctl", "J");
    profile.bom.header.push("MPN".into());
    fill_generic_tables(&mut profile, board, &circuit, &rules);
    assert_eq!(bom_row(&profile, "D1").refs, ["D1", "D2"], "component overrides and inherited MPNs merge equal parts");
    assert_eq!(bom_row(&profile, "D1").cells[3], "C1", "mixed-case supplier and supplier_part take precedence over lcsc");
    assert_eq!(bom_row(&profile, "D1").cells[4], "PART-A", "part-level MPN is retained in the cell");
    assert_eq!(bom_row(&profile, "D3").refs, ["D3"], "different merged MPNs split an otherwise equal LCSC group");
    assert_eq!(bom_row(&profile, "D3").cells[3], "C1", "part-level supplier number is inherited");
    assert_eq!(bom_row(&profile, "D3").cells[4], "PART-B", "component MPN overrides the part default");

    let third = circuit.comps.iter_mut().find(|c| c.refdes == "D3").expect("ledctl contains D3");
    Arc::make_mut(&mut third.part).fields.0.push(("LcSc".into(), "C9".into()));
    third.fields = Fields(vec![("supplier".into(), "other".into()), ("LcSc".into(), "C2".into())]);
    fill_generic_tables(&mut profile, board, &circuit, &rules);
    assert_eq!(bom_row(&profile, "D3").cells[3], "C2", "non-LCSC supplier uses case-insensitive, overridden fallback field");
    let third = circuit.comps.iter_mut().find(|c| c.refdes == "D3").expect("ledctl contains D3");
    Arc::make_mut(&mut third.part).fields.0.retain(|(name, _)| !name.eq_ignore_ascii_case("lcsc"));
    third.fields.0.retain(|(name, _)| !name.eq_ignore_ascii_case("lcsc"));
    fill_generic_tables(&mut profile, board, &circuit, &rules);
    assert_eq!(bom_row(&profile, "D3").cells[3], "", "another supplier's part number is never treated as an LCSC number");
}

#[test]
fn jlc_placement_matches_gerber_centres_and_top_rotation() {
    let design = ledctl();
    let unit = &design.units[0];
    let mut board = unit.board.as_deref().expect("ledctl has a board").clone();
    // A nonzero outline minimum distinguishes absolute Gerber coordinates from a shifted table.
    board.outline = vec![Contour::rect(P::mm(-2.0, -3.0), P::mm(52.0, 33.0), 0)];
    let top_ix = board.placed.iter().position(|p| p.refdes == "R1").expect("ledctl contains R1");
    assert_eq!(board.placed[top_ix].side, Side::Top, "rotation proof uses a top-side part");
    assert_eq!(board.placed[top_ix].rot, Angle::ZERO, "rotation proof starts at zero degrees");
    let rules = PlacementRules::jlc();
    let mut profile = ResolvedProfile::jlc_2_layer("ledctl", "J");
    let standard = ResolvedProfile::standard_2_layer("ledctl", "J");
    assert_eq!(profile.name, "JLCPCB 2-layer", "JLC profile name");
    assert_eq!(profile.files, standard.files, "JLC retains the standard two-layer file set");
    assert_ne!(profile.readme, standard.readme, "JLC has its own readme");
    assert!(profile.readme.contains("JLCPCB 2-layer"), "readme names the assembly profile");
    let mut baseline: Option<(P, P)> = None;
    for (rotation, cell) in [(Angle::ZERO, "0"), (Angle::QUARTER, "90")] {
        board.placed[top_ix].rot = rotation;
        fill_generic_tables(&mut profile, &board, &unit.circuit, &rules);
        // Pours are unrelated to the assembly frame; omit them to keep this proof small.
        let output = export(&board, &unit.circuit, &[], &profile).expect("JLC export succeeds");
        assert!(output.validation.ok(), "JLC package validates: {:?}", output.validation.failures().collect::<Vec<_>>());
        assert!(output.zip.is_some(), "validated JLC files are packaged");
        let copper = output.read_back.iter().find_map(|(_, kind, image)| match (kind, image) {
            (FileKind::Gerber { layer, .. }, ReadBack::Gerber(image)) if *layer == LayerId::F_CU => Some(image),
            _ => None,
        }).expect("F_Cu Gerber reads back");
        let placement = output.read_back.iter().find_map(|(_, kind, image)| match (kind, image) {
            (FileKind::Placement, ReadBack::Table(rows)) => Some(rows),
            _ => None,
        }).expect("placement CSV reads back");
        assert_eq!(placement[0], ["Designator", "Mid X", "Mid Y", "Layer", "Rotation"], "JLC placement headers");
        let row = placement[1..].iter().find(|r| r[0] == "R1").expect("placement contains R1");
        let pad_centre = |no: &str| {
            let item = copper.items.iter().find(|i| i.attrs.pin.as_ref().is_some_and(|(r, p)| r == "R1" && p == no))
                .expect("R1 pad has a pin-attributed Gerber object");
            match &item.geom {
                Geom::Shape(Shape::Rect { c, .. } | Shape::Circle { c, .. } | Shape::Oval { c, .. }) => *c,
                _ => panic!("R1 pad must read back as a flashed shape"),
            }
        };
        let (pad1, pad2) = (pad_centre("1"), pad_centre("2"));
        let centre = P::new((pad1.x + pad2.x) / 2, (pad1.y + pad2.y) / 2);
        for (index, coordinate) in [(1, centre.x), (2, centre.y)] {
            let nm = row[index].parse::<f64>().expect("placement coordinate is a millimetre number") * 1_000_000.0;
            assert!((nm - coordinate as f64).abs() <= profile.tolerance as f64,
                "Mid coordinate {index} equals the F_Cu pad centre within profile tolerance");
        }
        assert_eq!(row[3], "Top", "top-side layer cell");
        assert_eq!(row[4], cell, "top rotation is counter-clockwise positive with no footprint offset");
        if let Some((before_centre, before_pad1)) = baseline {
            assert_eq!(centre, before_centre, "rotation keeps the part centre fixed in Gerber");
            let before = before_pad1.sub(before_centre);
            let after = pad1.sub(centre);
            assert_ne!(before, P::ZERO, "pad 1 is off-centre so the rotation proof is meaningful");
            assert_eq!(after, P::new(-before.y, before.x), "pad 1 makes a CCW quarter turn in Gerber's Y-up frame");
        } else {
            baseline = Some((centre, pad1));
        }

        let bottom = board.placed.iter().find(|p| p.side == Side::Bottom).expect("ledctl has a bottom-side part");
        let row = placement[1..].iter().find(|r| r[0] == bottom.refdes).expect("placement contains the bottom-side part");
        assert_eq!(row[3], "Bottom", "bottom-side layer cell");
        for (index, coordinate) in [(1, bottom.at.x), (2, bottom.at.y)] {
            let nm = row[index].parse::<f64>().expect("bottom coordinate is a millimetre number") * 1_000_000.0;
            assert!((nm - coordinate as f64).abs() <= profile.tolerance as f64,
                "bottom Mid coordinate {index} keeps the absolute top-view position without mirroring");
        }
        // Bottom rotation is deliberately not asserted until a real order preview confirms its sense.
    }
}

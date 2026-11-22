use crate::export::export;
use crate::profile::{DrillFormat, FileKind, FileSpec, GerberVariant, ResolvedProfile};
use crate::tables::{fill_generic_tables, BottomX, PlacementRules};
use crate::text::csv_rows;
use crate::{read_excellon, read_gerber, Zeros};
use fab_elec_board::resolve_rules;
use fab_elec_board::zone::fill_all;
use fab_elec_core::{Angle, LayerId, Side};
use fab_elec_fixtures::ledctl;

#[test]
fn a_second_profile_with_other_names_units_and_columns_validates() {
    let d = ledctl();
    let u = &d.units[0];
    let (board, circuit) = (u.board.as_ref().unwrap(), &u.circuit);
    let fills = fill_all(board, &resolve_rules(board, None));
    let mut profile = ResolvedProfile::standard_2_layer("ledctl", "B");
    profile.name = "made-up fab sheet A".into();
    profile.files = [
        (LayerId::F_CU, "top"),
        (LayerId::B_CU, "bottom"),
        (LayerId::F_MASK, "top-mask"),
        (LayerId::B_MASK, "bottom-mask"),
        (LayerId::F_PASTE, "top-paste"),
        (LayerId::B_PASTE, "bottom-paste"),
        (LayerId::F_SILK, "top-silk"),
        (LayerId::B_SILK, "bottom-silk"),
        (LayerId::EDGE, "outline"),
    ].into_iter().map(|(layer, suffix)| FileSpec {
        name: format!("{}.{suffix}.gbr", profile.project),
        kind: FileKind::Gerber { layer, variant: GerberVariant::X1 },
    }).collect();
    profile.files.extend([
        FileSpec { name: format!("{}.holes.drl", profile.project), kind: FileKind::Drill { plated: None } },
        FileSpec { name: format!("{}.assembly.csv", profile.project), kind: FileKind::Bom },
        FileSpec { name: format!("{}.placement.csv", profile.project), kind: FileKind::Placement },
        FileSpec { name: format!("{}.fab-note.txt", profile.project), kind: FileKind::Readme },
    ]);
    profile.files.reverse();
    profile.gerber.decimals = 5;
    profile.drill = DrillFormat { inch: true, zeros: Zeros::KeepLeading, int_digits: 2, frac_digits: 4 };
    profile.bom.header = ["Designator", "Qty", "Value", "Footprint", "MPN"].iter().map(|s| s.to_string()).collect();
    profile.bom.delimiter = ';';
    profile.placement.header = ["Ref", "PosX", "PosY", "Side", "Rot"].iter().map(|s| s.to_string()).collect();
    profile.readme = "made-up fab sheet A\n".into();
    profile.zip = Some(format!("{}.{}.fabrication.zip", profile.project, profile.rev));
    let gerber_step_nm = 10_i32.pow(u32::from(6 - profile.gerber.decimals));
    let drill_step_nm = 25_400_000 / 10_i32.pow(u32::from(profile.drill.frac_digits));
    profile.tolerance = (gerber_step_nm / 2).max(drill_step_nm / 2) + 1_000;
    let rules = PlacementRules {
        bottom_x: BottomX::MirrorAbout(board.outline_bbox().width() as i32),
        unit_inch: true,
        rotation_offsets: vec![
            ("R_".into(), Angle::deg(90.0)),
            ("LED_".into(), Angle::deg(180.0)),
            ("PinHeader_".into(), Angle::deg(90.0)),
        ],
        side_words: ("top".into(), "bottom".into()),
        ..Default::default()
    };
    fill_generic_tables(&mut profile, board, circuit, &rules);
    let e = export(board, circuit, &fills, &profile).unwrap();
    assert!(e.validation.ok(), "made-up fab sheet A validates: {:?}", e.validation.failures().collect::<Vec<_>>());
    assert_eq!(e.files.iter().map(|f| &f.name).collect::<Vec<_>>(), profile.files.iter().map(|f| &f.name).collect::<Vec<_>>(), "file names and reversed order follow the profile");
    assert_eq!(e.zip.as_ref().expect("a validated package is zipped").name, "ledctl.B.fabrication.zip");

    let bom = e.files.iter().find(|f| f.name == "ledctl.assembly.csv").unwrap();
    let bom_text = std::str::from_utf8(&bom.bytes).unwrap();
    assert!(bom_text.starts_with("Designator;Qty;Value;Footprint;MPN\n"), "alternate BOM header and delimiter");
    let bom_rows = csv_rows(bom_text, ';');
    assert!(bom_rows[1..].iter().any(|r| r[1].parse::<usize>().unwrap() >= 2), "equal parts have a grouped quantity");
    for row in &bom_rows[1..] {
        assert_eq!(row[1].parse::<usize>().unwrap(), row[0].split(',').count(), "quantity counts the references");
    }

    let placement = e.files.iter().find(|f| f.name == "ledctl.placement.csv").unwrap();
    let placement_rows = csv_rows(std::str::from_utf8(&placement.bytes).unwrap(), ',');
    assert_eq!(placement_rows[0], ["Ref", "PosX", "PosY", "Side", "Rot"], "alternate placement header");
    assert_eq!(placement_rows.len(), board.placed.len() + 1, "every placed part has a row");
    for (cells, row) in placement_rows[1..].iter().zip(&profile.placement.rows) {
        let placed = board.placed.iter().find(|p| p.refdes == row.refdes).unwrap();
        let mut at = placed.at.sub(profile.placement.origin);
        if placed.side == Side::Bottom {
            at.x = profile.placement.bottom_x.unwrap().apply(at.x as i64) as i32;
        }
        assert_eq!(row.at, at, "{} retains its position in nm", row.refdes);
        for (cell, nm) in [(&cells[1], at.x), (&cells[2], at.y)] {
            assert_eq!(cell.split_once('.').unwrap().1.len(), 4, "inch cells have four decimals");
            let inches = cell.parse::<f64>().unwrap();
            assert!((inches * 25_400_000.0 - nm as f64).abs() <= 1_270.001, "{}: {cell} is the inch coordinate within half a step", row.refdes);
        }
    }
    for (refdes, side, rotation, offset) in [
        ("R1", "top", "90", 90.0),
        ("D1", "top", "90", 180.0),
        ("J2", "bottom", "180", 90.0),
    ] {
        let cells = placement_rows[1..].iter().find(|r| r[0] == refdes).unwrap();
        let row = profile.placement.rows.iter().find(|r| r.refdes == refdes).unwrap();
        assert_eq!(cells[3], side, "{refdes}: configured side word");
        assert_eq!(cells[4], rotation, "{refdes}: corrected and mirrored rotation");
        assert_eq!(row.rot_offset, Angle::deg(offset), "{refdes}: footprint prefix correction");
    }

    let drills = board.drills();
    assert!(drills.iter().any(|h| h.plated) && drills.iter().any(|h| !h.plated), "the board exercises both plating types");
    assert_eq!(profile.files.iter().filter(|f| matches!(f.kind, FileKind::Drill { .. })).count(), 1, "one merged drill file");
    let drill = e.files.iter().find(|f| f.name == "ledctl.holes.drl").unwrap();
    assert!(std::str::from_utf8(&drill.bytes).unwrap().lines().any(|line| line == "INCH,LZ"), "inch KeepLeading 2.4 drill format");
    let drill_image = read_excellon(&drill.bytes).unwrap();
    assert_eq!(drill_image.holes.len(), drills.len(), "merged drill file contains every plated and non-plated hole");
    assert_eq!(drill_image.file_attr("FileFunction").unwrap()[0], "MixedPlating", "merged drill file declares both plating types");
    for spec in profile.files.iter().filter(|f| matches!(f.kind, FileKind::Gerber { .. })) {
        let file = e.files.iter().find(|f| f.name == spec.name).unwrap();
        assert!(!std::str::from_utf8(&file.bytes).unwrap().lines().any(|line| line.starts_with("%T")), "{}: X1 has no attribute lines", file.name);
        assert_eq!(read_gerber(&file.bytes).unwrap().decimals, 5, "{}: five coordinate decimals", file.name);
    }

    // Alias choices only change the cells; the grouped references stay the same.
    let mut aliases = profile.clone();
    aliases.bom.header = [
        "cOmMeNt", "vAlUe", "dEsIgNaToR", "rEfErEnCe", "rEfS", "fOoTpRiNt", "pAcKaGe",
        "pArT", "mPn", "mAnUfAcTuReR pArT", "qTy", "qUaNtItY", "dEsCrIpTiOn", "unknown",
    ].iter().map(|s| s.to_string()).collect();
    fill_generic_tables(&mut aliases, board, circuit, &rules);
    assert_eq!(aliases.bom.rows.len(), profile.bom.rows.len(), "aliases retain every group");
    for (alias, row) in aliases.bom.rows.iter().zip(&profile.bom.rows) {
        assert_eq!(alias.refs, row.refs, "aliases retain the grouping");
        let expected: Vec<_> = [2, 2, 0, 0, 0, 3, 3, 4, 4, 4, 1, 1].into_iter().map(|i| row.cells[i].clone()).chain([String::new(), String::new()]).collect();
        assert_eq!(alias.cells, expected, "case-insensitive aliases, empty description and unknown column");
    }
}

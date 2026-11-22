use crate::export::export;
use crate::package::unpackage;
use crate::profile::ResolvedProfile;
use crate::read_gerber;
use crate::tables::{fill_generic_tables, PlacementRules};
use fab_elec_board::resolve_rules;
use fab_elec_board::zone::fill_all;
use fab_elec_core::LayerId;
use fab_elec_fixtures::ledctl;

#[test]
fn ledctl_exports_a_package_that_validates() {
    let d = ledctl();
    let u = &d.units[0];
    let (board, circuit) = (u.board.as_ref().unwrap(), &u.circuit);
    let fills = fill_all(board, &resolve_rules(board, None));
    let mut profile = ResolvedProfile::standard_2_layer("ledctl", "A");
    fill_generic_tables(&mut profile, board, circuit, &PlacementRules::default());
    let e = export(board, circuit, &fills, &profile).unwrap();
    assert!(e.validation.ok(), "the ledctl package validates: {:?}", e.validation.failures().collect::<Vec<_>>());
    assert_eq!(e.files.len(), profile.files.len(), "every file of the profile");
    assert_eq!(e.read_back.len(), profile.files.len(), "every file reads back for the proof view");
    assert!(!e.model.layers.is_empty(), "the export keeps the model it was made from");
    let zip = e.zip.expect("a validated package is zipped");
    assert_eq!(zip.name, "ledctl-A.zip");
    assert_eq!(unpackage(&zip.bytes).unwrap(), e.files, "the zip holds the files as written");
    let f_cu = e.files.iter().find(|f| f.name == "ledctl-F_Cu.gbr").unwrap();
    let img = read_gerber(&f_cu.bytes).unwrap();
    assert!(img.items.iter().any(|i| i.attrs.net.as_deref() == Some("GND")), "F.Cu carries net attributes");
    assert!(!img.to_polyset(200).is_empty() && LayerId::F_CU.is_copper());
    let pth = e.files.iter().find(|f| f.name == "ledctl-NPTH.drl").unwrap();
    assert_eq!(crate::read_excellon(&pth.bytes).unwrap().holes.len(), 2, "the two mounting holes are the non-plated drill file");
}

#[test]
fn ledctl_package_validates_at_every_coordinate_precision() {
    use crate::drill::Zeros;
    use crate::profile::{DrillFormat, FileKind};
    let d = ledctl();
    let u = &d.units[0];
    let (board, circuit) = (u.board.as_ref().unwrap(), &u.circuit);
    // no pours: precision is about pads, tracks and drills, and the pours are the cost of an export
    let fills: Vec<fab_elec_board::ZoneFill> = Vec::new();
    let mut profile = ResolvedProfile::standard_2_layer("ledctl", "A");
    fill_generic_tables(&mut profile, board, circuit, &PlacementRules::default());
    // each coordinate precision and each drill zero format once: the two are independent
    for (decimals, inch, zeros, int_digits, frac_digits, drill_half_step) in [
        (6u8, false, Zeros::Decimal, 3, 3, 0),
        (5, false, Zeros::KeepLeading, 3, 3, 500),
        (5, false, Zeros::KeepTrailing, 3, 3, 500),
        (6, true, Zeros::KeepLeading, 2, 4, 1270),
    ] {
        profile.gerber.decimals = decimals;
        let step_nm = 10_i32.pow(u32::from(6 - decimals));
        profile.drill = DrillFormat { inch, zeros, int_digits, frac_digits };
        profile.tolerance = (step_nm / 2).max(drill_half_step) + 1000;
        let combination = format!("Gerber {decimals} decimals, drill {} {zeros:?} {int_digits}.{frac_digits}", if inch { "inch" } else { "mm" });
        let e = export(board, circuit, &fills, &profile).unwrap_or_else(|err| panic!("{combination}: {err}"));
        assert!(e.validation.ok(), "{combination}: {:?}", e.validation.failures().collect::<Vec<_>>());
        for spec in profile.files.iter().filter(|f| matches!(f.kind, FileKind::Gerber { .. })) {
            let f = e.files.iter().find(|f| f.name == spec.name).unwrap();
            let img = read_gerber(&f.bytes).unwrap_or_else(|err| panic!("{combination}, {}: {err}", f.name));
            assert_eq!(img.decimals, decimals, "{combination}, {}", f.name);
        }
    }
}

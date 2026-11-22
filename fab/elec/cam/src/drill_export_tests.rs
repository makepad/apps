use super::*;
use crate::drill::{read_excellon, write_excellon};
use fab_elec_core::mm;

fn options(path: DrillPath) -> DrillExportOptions {
    DrillExportOptions { board_stem: "b".into(), ..DrillExportOptions::for_path(path) }
}

fn hole(x: f64, y: f64) -> DrillHole {
    DrillHole { at: P::new(mm(x), mm(y)), dia: mm(0.4), to: None }
}

fn plated() -> HolesByClass {
    BTreeMap::from([(DrillClass::ThroughPlated, vec![hole(200.0, 148.0)])])
}

fn names(opts: &DrillExportOptions, holes: &HolesByClass) -> Vec<String> {
    drill_files(opts, holes).into_iter().map(|p| p.name).collect()
}

fn through_text(opts: &DrillExportOptions, holes: &HolesByClass) -> String {
    let plan = drill_files(opts, holes).into_iter()
        .find(|p| matches!(p.kind, DrillFileKind::Excellon | DrillFileKind::Gerber))
        .expect("S46.R59 a nonempty through drill plan");
    String::from_utf8(write_drill_file(&plan, opts, holes).expect("S46.R62/R63 write drill"))
        .expect("S46.R62/R63 drill text")
}

#[test]
fn path_defaults_and_dialog_settings() {
    for path in [DrillPath::Dialog, DrillPath::Job, DrillPath::Cli] {
        let o = options(path);
        let dialog = path == DrillPath::Dialog;
        assert_eq!(o.format, DrillFormat::Excellon, "S46.R52 default format");
        assert_eq!(o.units, if path == DrillPath::Job { DrillUnits::Inches } else { DrillUnits::Millimeters }, "S46.R53 units by path");
        assert_eq!(o.combine_pth_npth, !dialog, "S46.R53 combined by path");
        assert_eq!(o.slots, if dialog { SlotMode::Route } else { SlotMode::Alternate }, "S46.R53 slots by path");
        assert_eq!(o.map_format, if dialog { DrillMapFormat::GerberX2 } else { DrillMapFormat::Pdf }, "S46.R53 maps by path");
        assert_eq!(o.map_page, if dialog { DrillMapPage::Board } else { DrillMapPage::A4Landscape }, "S46.R60 map page");
        assert_eq!(o.origin, DrillOrigin::Absolute, "S46.R52 initial absolute origin");
        assert_eq!(o.zeros, Zeros::Decimal, "S46.R52 decimal default");
        assert!(!o.mirror_y && !o.minimal_header && !o.generate_map && !o.generate_tenting && !o.generate_report,
            "S46.R52 switches default off");
        assert!(o.description.is_empty() && o.output_dir.is_empty() && o.report_filename.is_empty(), "S46.R52 empty job strings");
        assert_eq!(o.gerber_precision, 6, "S46.R52 precision default");
        let loaded = o.with_board_settings(DrillOrigin::Plot, "plots", 5);
        assert_eq!(loaded.origin, if dialog { DrillOrigin::Plot } else { DrillOrigin::Absolute }, "S46.R53 board origin only in dialog");
        assert_eq!(loaded.output_dir, if dialog { "plots" } else { "" }, "S46.R52 board output only in dialog");
        assert_eq!(loaded.gerber_precision, if dialog { 5 } else { 6 }, "S46.R52 board precision only in dialog");
    }
    let mut dialog = options(DrillPath::Dialog);
    let state = dialog.dialog_state();
    assert!(state.excellon_controls_enabled && !state.tenting_enabled && !state.precision_enabled, "S46.R55 decimal control state");
    assert_eq!(state.precision_text, "3:3", "S46.R55 mm digits");
    dialog.units = DrillUnits::Inches;
    dialog.zeros = Zeros::Keep;
    assert_eq!(dialog.dialog_state().precision_text, "2:4", "S46.R55 inch digits");
    assert!(dialog.dialog_state().precision_enabled, "S46.R55 integer precision text enabled");
    dialog.format = DrillFormat::Gerber;
    dialog.gerber_precision = 5;
    let state = dialog.dialog_state();
    assert!(!state.excellon_controls_enabled && state.tenting_enabled, "S46.R55 Gerber controls");
    assert_eq!(state.precision_text, "4.5", "S46.R55 board Gerber precision");
    assert!(state.report_button_visible && state.message_box_visible && state.folder_button_visible, "S46.R56 board dialog actions visible");
    let mut job = options(DrillPath::Job);
    job.gerber_precision = 5;
    job.generate_report = true;
    job.report_filename = "saved.rpt".into();
    dialog.output_dir = "plots\\drills".into();
    dialog.generate_map = true;
    dialog.generate_tenting = true;
    job.apply_dialog_edits(&dialog);
    assert_eq!(job.output_dir, "plots/drills", "S46.R56 normalized folder");
    assert!(job.generate_map && job.generate_tenting, "S46.R57 editable job options saved");
    assert!(job.generate_report && job.report_filename == "saved.rpt" && job.gerber_precision == 5, "S46.R57 hidden settings retained");
    let state = job.dialog_state();
    assert_eq!(state.precision_text, "4.6", "S46.R57 job display fixed at 4.6");
    assert!(!state.report_button_visible && !state.message_box_visible && !state.folder_button_visible, "S46.R57 hidden job controls");
}

#[test]
fn through_file_sets_maps_and_protection() {
    let empty = HolesByClass::new();
    let holes = plated();
    let mut o = options(DrillPath::Cli);
    assert_eq!(names(&o, &empty), ["b.drl"], "S46.R59 empty through Excellon always exists");
    o.combine_pth_npth = false;
    assert_eq!(names(&o, &holes), ["b-PTH.drl", "b-NPTH.drl"], "S46.R58/R59 O45 empty NPTH included");
    let npth = drill_files(&o, &holes).remove(1);
    let bytes = write_drill_file(&npth, &o, &holes).expect("S46.R59 empty NPTH write");
    let parsed = read_excellon(&bytes).expect("S46.R59 empty NPTH parse");
    assert!(parsed.tools.is_empty() && parsed.holes.is_empty(), "S46.R59 O45 empty NPTH has neither tool nor hit");
    o.format = DrillFormat::Gerber;
    assert_eq!(names(&o, &holes), ["b-PTH-drl.gbr"], "S46.R59 O45 Gerber only with holes");
    assert!(names(&o, &empty).is_empty(), "S46.R59 no empty Gerber drill files");
    for (format, combined, expected) in [
        (DrillFormat::Excellon, true, vec!["b-drl_map.pdf"]),
        (DrillFormat::Excellon, false, vec!["b-PTH-drl_map.pdf", "b-NPTH-drl_map.pdf"]),
        (DrillFormat::Gerber, true, vec!["b-PTH-drl_map.pdf", "b-NPTH-drl_map.pdf"]),
    ] {
        o.format = format; o.combine_pth_npth = combined; o.generate_map = true;
        let maps: Vec<_> = drill_files(&o, &holes).into_iter().filter(|p| matches!(p.kind, DrillFileKind::Map(_))).map(|p| p.name).collect();
        assert_eq!(maps, expected, "S46.R60 O47 maps include empty NPTH even in Gerber");
    }
    for (format, ext) in [(DrillMapFormat::GerberX2, "gbr"), (DrillMapFormat::Postscript, "ps"), (DrillMapFormat::Dxf, "dxf"), (DrillMapFormat::Svg, "svg")] {
        o.map_format = format;
        assert!(names(&o, &holes).contains(&format!("b-NPTH-drl_map.{ext}")), "S46.R60 O47 map extensions");
    }
    let mut protected = holes;
    let span = DrillSpan { first: 1, last: 2 };
    for feature in [ViaProtection::Filling(span), ViaProtection::Capping(span), ViaProtection::Covering(DrillSide::Front),
        ViaProtection::Covering(DrillSide::Back), ViaProtection::Plugging(DrillSide::Front), ViaProtection::Plugging(DrillSide::Back),
        ViaProtection::Tenting(DrillSide::Front), ViaProtection::Tenting(DrillSide::Back)] {
        protected.insert(DrillClass::Protection(feature), vec![hole(1.0, 2.0)]);
    }
    o.generate_map = false;
    let without_tenting = names(&o, &protected);
    assert_eq!(without_tenting, ["b-PTH-drl.gbr", "b-filling-front-back.gbr", "b-capping-front-back.gbr", "b-covering-front.gbr",
        "b-covering-back.gbr", "b-plugging-front.gbr", "b-plugging-back.gbr"], "S46.R59 protection names without -drl");
    o.generate_tenting = true;
    let with_tenting = names(&o, &protected);
    assert!(with_tenting.contains(&"b-tenting-front.gbr".into()) && with_tenting.contains(&"b-tenting-back.gbr".into()), "S46.R59 O53 both tenting sides");
    o.format = DrillFormat::Excellon;
    assert_eq!(names(&o, &protected), ["b.drl"], "S46.R59 O54 Excellon ignores protection and tenting");
    o.format = DrillFormat::Gerber;
    protected.insert(DrillClass::Protection(ViaProtection::Tenting(DrillSide::Back)), vec![]);
    assert!(!names(&o, &protected).contains(&"b-tenting-back.gbr".into()), "S46.R59 protection only with feature holes");
}

#[test]
fn blind_buried_and_backdrill_generic_names() {
    let mut holes = plated();
    for class in [DrillClass::BlindBuried(DrillSpan { first: 1, last: 2 }), DrillClass::BlindBuried(DrillSpan { first: 2, last: 3 })] {
        holes.insert(class, vec![hole(1.0, 2.0)]);
    }
    let mut o = options(DrillPath::Cli);
    o.copper_layers = 4;
    assert_eq!(names(&o, &holes), ["b.drl", "b-front-in1.drl", "b-in1-in2.drl"], "S46.R58 O46 generic blind/buried names");
    o.format = DrillFormat::Gerber;
    assert_eq!(names(&o, &holes), ["b-PTH-drl.gbr", "b-front-in1-drl.gbr", "b-in1-in2-drl.gbr"], "S46.R58 O46 Gerber spans");
    holes.insert(DrillClass::Backdrill(DrillSpan { first: 2, last: 4 }), vec![hole(3.0, 4.0)]);
    for (format, suffix) in [(DrillFormat::Excellon, ".drl"), (DrillFormat::Gerber, "-drl.gbr")] {
        o.format = format;
        let files = names(&o, &holes);
        assert!(files.contains(&format!("b_Backdrills_Drill_2_4{suffix}")), "S46.R58 physical backdrill numbers");
        assert!(files.contains(&format!("b-in1-back-backdrill{suffix}")), "S46.R58/R59 backdrill layer-pair alias");
    }
    holes.insert(DrillClass::BlindBuried(DrillSpan { first: 1, last: 3 }), vec![]);
    assert!(!names(&o, &holes).iter().any(|n| n.contains("front-in2")), "S46.R59 empty other span omitted");
    o.generate_map = true;
    let files = names(&o, &holes);
    assert!(files.contains(&"b-front-in1-drl_map.pdf".into()) && files.contains(&"b-in1-in2-drl_map.pdf".into()), "S46.R60 span maps use Excellon stems");
    assert_eq!(files.iter().filter(|n| n.contains("Backdrills") && n.contains("map")).count(), 1, "S46.R60 one map per backdrill span");
}

#[test]
fn zero_formats_and_unit_digits() {
    let holes = BTreeMap::from([(DrillClass::ThroughPlated, vec![hole(25.4, 0.0)])]);
    let mut o = options(DrillPath::Cli);
    for (units, layout, unit, cases) in [
        (DrillUnits::Millimeters, "3:3", "METRIC", ["X25.4Y0.0", "X25400Y0", "X0254Y0", "X025400Y000000"]),
        (DrillUnits::Inches, "2:4", "INCH", ["X1.0Y0.0", "X10000Y0", "X01Y0", "X010000Y000000"]),
    ] {
        o.units = units;
        for ((zeros, suffix, comment), hit) in [
            (Zeros::Decimal, "", "decimal"), (Zeros::KeepTrailing, ",TZ", "suppress leading zeros"),
            (Zeros::KeepLeading, ",LZ", "suppress trailing zeros"), (Zeros::Keep, "", "keep zeros"),
        ].into_iter().zip(cases) {
            o.zeros = zeros;
            let text = through_text(&o, &holes);
            let digits = if zeros == Zeros::Decimal { "-:-" } else { layout };
            assert!(text.contains(&format!("; FORMAT={{{digits}/ absolute / {} / {comment}}}", if units == DrillUnits::Inches { "inch" } else { "metric" })), "S46.R62 O51 format comment: {text}");
            assert!(text.lines().any(|line| line == format!("{unit}{suffix}")), "S46.R62 O51 units/suppression: {text}");
            assert!(text.lines().any(|line| line == hit), "S46.R62 fixed digits and zero handling: {text}");
            let got = read_excellon(text.as_bytes()).expect("S46.R62 read all zero formats");
            assert_eq!(got.holes[0].at, P::new(mm(25.4), 0), "S46.R62 zero formats preserve coordinates");
        }
    }
    o.units = DrillUnits::Millimeters;
    o.zeros = Zeros::Keep;
    o.digit_counts = Some((4, 4));
    o.minimal_header = true;
    let text = through_text(&o, &holes);
    assert!(text.contains("METRIC,0000.0000\n") && text.contains("X00254000Y00000000"), "S46.R62 explicit digit-count extension retained");
    assert_eq!(read_excellon(text.as_bytes()).expect("S46.R62 extension read").holes[0].at.x, mm(25.4), "S46.R62 extension round trip");
    let legacy = [DrillHole { at: P::new(1_234_567, -2_345_678), dia: 456_789, to: None }];
    let bytes = write_excellon(&legacy, &[], &DrillOpts::default());
    assert_eq!(read_excellon(&bytes).expect("S46.R62 legacy reader").holes, legacy, "S46.R62 legacy entry point retains six-place precision and output axes");
}

#[test]
fn alternate_and_routed_slots() {
    let slot = DrillHole { at: P::new(mm(1.0), mm(2.0)), dia: mm(0.4), to: Some(P::new(mm(3.0), mm(4.0))) };
    let holes = BTreeMap::from([(DrillClass::ThroughPlated, vec![slot])]);
    let mut o = options(DrillPath::Cli);
    let alternate = through_text(&o, &holes);
    assert!(alternate.contains("X1.0Y-2.0G85X3.0Y-4.0\n"), "S46.R62 alternate G85 slot");
    o.slots = SlotMode::Route;
    let route = through_text(&o, &holes);
    assert!(route.contains("G00X1.0Y-2.0\nM15\nG01X3.0Y-4.0\nM16\nG05\n") && !route.contains("G85"), "S46.R62 routed tool-down/up slot");
    let expected = vec![DrillHole { at: P::new(mm(1.0), -mm(2.0)), dia: mm(0.4), to: Some(P::new(mm(3.0), -mm(4.0))) }];
    for text in [alternate, route] {
        assert_eq!(read_excellon(text.as_bytes()).expect("S46.R62 slot read").holes, expected, "S46.R62 both slot variants preserve endpoints");
    }
}

#[test]
fn mirror_origin_and_minimal_header() {
    let holes = plated();
    let mut o = options(DrillPath::Cli);
    let normal = through_text(&o, &holes);
    assert!(normal.contains("X200.0Y-148.0") && normal.contains("T1C0.400"), "S46.R62 mm decimal precision and default Y up");
    assert!(normal.contains("; #@! TF.FileFunction,MixedPlating,1,2\n"), "S46.R62 combined file function");
    o.mirror_y = true;
    assert!(through_text(&o, &holes).contains("X200.0Y148.0"), "S46.R62 mirrored Y unchanged");
    o.mirror_y = false;
    o.origin = DrillOrigin::Plot;
    o.plot_origin = P::new(mm(100.0), mm(50.0));
    assert!(through_text(&o, &holes).contains("X100.0Y-98.0"), "S46.R62 O55 plot origin subtraction");
    o.mirror_y = true;
    assert!(through_text(&o, &holes).contains("X100.0Y98.0"), "S46.R62 mirror applied after origin");
    o.origin = DrillOrigin::Absolute;
    o.mirror_y = false;
    o.units = DrillUnits::Inches;
    let inch = through_text(&o, &holes);
    assert!(inch.contains("T1C0.0157") && inch.contains("X7.874Y-5.8268"), "S46.R62 inch precision");
    o.units = DrillUnits::Millimeters;
    o.minimal_header = true;
    let minimal = through_text(&o, &holes);
    assert!(minimal.starts_with("M48\nMETRIC\nT1C0.400\n") && !minimal.contains(';') && !minimal.contains("FMAT"), "S46.R62 O52 minimal header");
}

#[test]
fn gerber_precision_and_ignored_excellon_options() {
    let holes = plated();
    let mut o = options(DrillPath::Cli);
    o.format = DrillFormat::Gerber;
    let standard = through_text(&o, &holes);
    assert!(standard.contains("%FSLAX46Y46*%") && standard.contains("%MOMM*%"), "S46.R63 Gerber 4.6 mm");
    assert!(standard.contains("%TF.FileFunction,Plated,1,2,PTH,Drill*%"), "S46.R63 Gerber drill file function");
    o.units = DrillUnits::Inches; o.zeros = Zeros::KeepLeading; o.mirror_y = true;
    o.minimal_header = true; o.combine_pth_npth = false; o.slots = SlotMode::Route;
    o.digit_counts = Some((2, 2));
    assert_eq!(through_text(&o, &holes), standard, "S46.R63 all Excellon options ignored by Gerber");
    o.gerber_precision = 4;
    assert!(through_text(&o, &holes).contains("%FSLAX45Y45*%"), "S46.R54/R63 O50 non-six job precision becomes 5");
    o.origin = DrillOrigin::Plot;
    o.plot_origin = P::new(mm(100.0), mm(50.0));
    let shifted = through_text(&o, &holes);
    assert!(shifted.contains("X10000000Y-9800000"), "S46.R63 Gerber origin and Y up unaffected by mirror");
}

#[test]
fn job_json_defaults_fallbacks_and_spelling() {
    let default = DrillExportOptions::for_path(DrillPath::Job);
    let expected = r#"{
  "description": "",
  "output_dir": "",
  "excellon.mirror_y": false,
  "excellon.minimal_header": false,
  "excellon.combine_pth_npth": true,
  "excellon.oval_drill_route": false,
  "format": "excellon",
  "drill_origin": "abs",
  "units": "in",
  "zero_format": "decimal",
  "generate_map": false,
  "generate_tenting": false,
  "map_format": "pdf",
  "gerber_precision": 6,
  "generate_report": false,
  "report_filename": ""
}
"#;
    assert_eq!(default.to_job_json(), expected, "S46.R52 O43 exact new-job keys and defaults");
    assert_eq!(DrillExportOptions::from_job_json(expected).expect("S46.R52 O43 read default"), default, "S46.R52 job round trip");
    assert_eq!(DrillExportOptions::from_job_json("{}").expect("S46.R52 empty job"), default, "S46.R52 missing fields use defaults");
    let unknown = DrillExportOptions::from_job_json(r#"{"units":"cm","map_format":"jpeg","format":"unknown","drill_origin":"unknown","zero_format":"unknown","gerber_precision":4}"#).expect("S46.R54 unknown enum strings accepted");
    assert_eq!(unknown.units, DrillUnits::Inches, "S46.R54 O44 unknown units use first entry");
    assert_eq!(unknown.map_format, DrillMapFormat::Dxf, "S46.R54 O44 unknown map uses DXF, not PDF");
    assert_eq!((unknown.format, unknown.origin, unknown.zeros), (DrillFormat::Excellon, DrillOrigin::Absolute, Zeros::Decimal), "S46.R54 enum first entries");
    assert_eq!(unknown.gerber_precision, 5, "S46.R54 O50 job non-six precision");
    for (zeros, spelling) in [(Zeros::Decimal, "decimal"), (Zeros::KeepTrailing, "surpress_leading"), (Zeros::KeepLeading, "surpress_trailing"), (Zeros::Keep, "keep_zeros")] {
        let mut o = default.clone();
        o.zeros = zeros;
        o.description = "quoted \"text\"\\\n\t\u{8} Ω 😀".into();
        o.mirror_y = true; o.minimal_header = true; o.combine_pth_npth = false; o.slots = SlotMode::Route;
        o.format = DrillFormat::Gerber; o.origin = DrillOrigin::Plot; o.units = DrillUnits::Millimeters;
        o.generate_map = true; o.generate_tenting = true; o.generate_report = true;
        o.map_format = DrillMapFormat::Postscript; o.gerber_precision = 5;
        o.output_dir = "output/subdir".into(); o.report_filename = "custom.rpt".into();
        let json = o.to_job_json();
        assert!(json.contains(&format!("\"zero_format\": \"{spelling}\"")), "S46.R52 exact historical zero spelling");
        assert_eq!(DrillExportOptions::from_job_json(&json).expect("S46.R52 parse escaped job"), o, "S46.R52 every setting and escaped string round trips");
    }
    let unicode = DrillExportOptions::from_job_json(r#"{"description":"\u03a9 \ud83d\ude00"}"#).expect("S46.R52 Unicode JSON");
    assert_eq!(unicode.description, "Ω 😀", "S46.R52 Unicode surrogate decoding");
    for bad in [r#"{"excellon":{"mirror_y":true}}"#, r#"{"units":true}"#, r#"{"gerber_precision":01}"#, r#"{"description":"\ud800"}"#, r#"{"generate_map":true,}"#, "{} trailing"] {
        assert!(DrillExportOptions::from_job_json(bad).is_err(), "S46.R52 malformed or non-flat job refused: {bad}");
    }
}

#[test]
fn cli_value_rules_and_success_messages() {
    let mut o = options(DrillPath::Cli);
    for (flag, value, message) in [
        ("--format", "other", "Invalid drill format"), ("--excellon-units", "cm", "Invalid units specified"),
        ("--excellon-zeros-format", "surpress_leading", "Invalid zeros format specified"),
        ("--excellon-oval-format", "other", "Invalid oval drill format specified"),
        ("--map-format", "jpeg", "Invalid map format specified"), ("--drill-origin", "abs", "Invalid origin mode specified"),
        ("--gerber-precision", "4", "Gerber coordinate precision should be either 5 or 6"),
    ] {
        let error = o.set_cli_option(flag, value).expect_err("S46.R54 invalid CLI value");
        assert_eq!((error.exit_code, error.message), (1, message), "S46.R54 O50 CLI status and message");
    }
    o.set_cli_option("--excellon-zeros-format", "suppressleading").expect("S46.R54 O51 valid CLI spelling");
    assert_eq!(o.zeros, Zeros::KeepTrailing, "S46.R54 O51 CLI spelling maps to TZ");
    o.set_cli_option("--report-path", "x.rpt").expect("S46.R54 O49 report path");
    assert_eq!(o.cli_warning(), Some("Warning: Report path supplied without --generate-report, no report will be generated"), "S46.R54 O49 warning");
    assert!(!drill_files(&o, &plated()).iter().any(|p| p.kind == DrillFileKind::Report), "S46.R54 O49 report path alone generates no report");
    let plan = drill_files(&o, &plated()).remove(0);
    assert_eq!(plan.created_message("out/b.drl"), "Created file 'out/b.drl'", "S46.R64 Excellon success punctuation");
    o.format = DrillFormat::Gerber;
    let plan = drill_files(&o, &plated()).remove(0);
    assert_eq!(plan.created_message("out/b-PTH-drl.gbr"), "Created file 'out/b-PTH-drl.gbr'.", "S46.R64 other file success punctuation");
}

#[test]
fn report_names_selected_format_and_determinism() {
    let holes = plated();
    for path in [DrillPath::Dialog, DrillPath::Job, DrillPath::Cli] {
        let mut o = options(path);
        o.generate_report = true;
        let expected = if path == DrillPath::Dialog { "b-drl.rpt" } else { "b-drill.rpt" };
        assert_eq!(o.report_name(), expected, "S46.R56/R61 O48 report default by path");
        assert_eq!(drill_files(&o, &holes).last().expect("S46.R61 report plan").name, expected, "S46.R61 report follows drill files");
        assert!(o.report_filename.is_empty(), "S46.R61 default report name not saved into job");
        for format in [DrillFormat::Excellon, DrillFormat::Gerber] {
            o.format = format;
            let report = drill_report(&o, &holes);
            for plan in drill_files(&o, &holes) {
                if plan.kind != DrillFileKind::Report {
                    assert!(report.contains(&format!("File: {}\n", plan.name)), "S46.R61/R65 selected format's actual files in report");
                    let first = write_drill_file(&plan, &o, &holes).expect("S46.R65 first export");
                    assert_eq!(first, write_drill_file(&plan, &o, &holes).expect("S46.R65 repeated export"), "S46.R65 byte-identical drill output");
                    let text = String::from_utf8(first).expect("S46.R65 text export");
                    assert!(!text.contains("CreationDate") && !text.contains(" date "), "S46.R65 no drill creation dates");
                    assert!(text.contains("Fab"), "S46.R65 generator names Fab");
                }
            }
            if format == DrillFormat::Gerber {
                assert!(!report.contains("b.drl") && !report.contains("b-PTH.drl") && !report.contains("b-NPTH"), "S46.R61/R65 report matches selected Gerber files, including omitted empty NPTH");
            }
            assert_eq!(report, drill_report(&o, &holes), "S46.R65 deterministic report");
            assert!(!report.contains("Created on") && !report.contains("CreationDate"), "S46.R65 report has no date");
        }
        o.report_filename = "chosen.rpt".into();
        assert_eq!(o.report_name(), "chosen.rpt", "S46.R61 explicit report name");
    }
    let attrs = vec![("CreationDate".into(), vec!["must not escape into output".into()])];
    let bytes = write_excellon(&[], &attrs, &DrillOpts::default());
    assert!(!String::from_utf8_lossy(&bytes).contains("CreationDate"), "S46.R65 supplied creation-date attribute omitted too");
}

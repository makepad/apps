use super::*;
use crate::text::csv_rows;

fn footprint(reference: &str, layer: LayerId, at: P) -> PlacementFootprint {
    PlacementFootprint {
        reference: reference.into(), library_id: "Resistor_SMD:R_0603_1608Metric".into(),
        at, rotation: Angle::ZERO, layer, smd: true, through_hole: false,
        default_variant: PlacementVariant { value: "10k".into(), ..Default::default() },
        variants: BTreeMap::new(), pads: Vec::new(),
    }
}
fn board(fps: Vec<PlacementFootprint>) -> PlacementBoard {
    PlacementBoard { filename: "b.kicad_pcb".into(), footprints: fps, ..Default::default() }
}
fn options(path: PlacementPath) -> PlacementExportOptions { PlacementExportOptions::for_path(path) }
fn run(board: &PlacementBoard, opts: &PlacementExportOptions) -> PlacementExport {
    export_placement(board, opts).expect("S46.R84 placement export")
}
fn text(file: &PlacementOutput) -> &str { std::str::from_utf8(&file.file.bytes).expect("S46.R80 UTF-8 output") }
fn names(export: &PlacementExport) -> Vec<&str> { export.files.iter().map(|f| f.file.name.as_str()).collect() }
fn report_text(board: &PlacementBoard) -> String {
    String::from_utf8(footprint_report(board, ReportUserUnits::Millimeters, "").bytes).expect("S46.R82 report text")
}

#[test]
fn defaults_job_settings_and_dialog_controls() {
    let d = options(PlacementPath::Dialog);
    let j = options(PlacementPath::Job);
    let c = options(PlacementPath::Cli);
    assert_eq!((d.units, j.units, c.units), (PlacementUnits::Millimeters, PlacementUnits::Millimeters, PlacementUnits::Inches), "S46.R67/R68 per-path units");
    assert_eq!((d.single_file, j.single_file, c.single_file), (true, false, true), "S46.R68 per-path file split");
    assert_eq!((d.use_drill_place_file_origin, j.use_drill_place_file_origin, c.use_drill_place_file_origin), (true, true, false), "S46.R68 per-path origin");
    assert_eq!((d.gerber_board_edge, j.gerber_board_edge, c.gerber_board_edge), (false, true, false), "S46.R68 per-path board edge");
    let expected = "{\n  \"description\": \"\",\n  \"output_filename\": \"\",\n  \"use_drill_place_file_origin\": true,\n  \"smd_only\": false,\n  \"exclude_footprints_with_th\": false,\n  \"exclude_dnp\": false,\n  \"exclude_bom\": false,\n  \"negate_bottom_x\": false,\n  \"single_file\": false,\n  \"gerber_board_edge\": true,\n  \"side\": \"both\",\n  \"units\": \"mm\",\n  \"format\": \"ascii\",\n  \"variant\": \"\"\n}\n";
    assert_eq!(j.to_job_json(), expected, "S46.R67 O60 complete job schema and defaults");
    assert_eq!(PlacementExportOptions::from_job_json(expected).expect("S46.R67 read job"), j, "S46.R67 job round trip");
    let unknown = PlacementExportOptions::from_job_json(r#"{"side":"top","units":"cm","format":"xml"}"#).expect("S46.R69 unknown enums");
    assert_eq!((unknown.side, unknown.units, unknown.format), (PlacementSide::Front, PlacementUnits::Inches, PlacementFormat::Ascii), "S46.R69 O61 unknown values take first enum entries");
    let fallback = run(&board(Vec::new()), &unknown);
    assert_eq!(names(&fallback), ["b-top.pos"], "S46.R69/R71 O61 fallback side is front");
    assert!(text(&fallback.files[0]).contains("## Unit = inches, Angle = deg."), "S46.R69 O61 fallback units reach the file");
    let mut escaped = j.clone();
    escaped.description = "quoted \"text\" and Unicode Ω".into();
    escaped.variant = "variant\\name".into();
    assert_eq!(PlacementExportOptions::from_job_json(&escaped.to_job_json()).expect("S46.R67 escaped settings"), escaped, "S46.R67 job strings round trip");
    assert!(PlacementExportOptions::from_job_json(r#"{"smd_only":"true"}"#).is_err(), "S46.R67 wrong JSON setting type rejected");
    let mut b = board(Vec::new());
    b.current_variant = "A".into();
    b.variants = vec!["A".into(), "B".into()];
    assert_eq!(b.variant_choices(), ["", "A", "B"], "S46.R67 default variant first");
    let mut edits = PlacementExportOptions::for_dialog(&b);
    assert_eq!(edits.variant, "A", "S46.R67 dialog starts at current variant");
    edits.smd_only = true;
    edits.exclude_footprints_with_th = true;
    edits.negate_bottom_x = true;
    edits.set_dialog_format(PlacementFormat::GerberX3);
    assert!(!edits.smd_only && !edits.exclude_footprints_with_th && !edits.negate_bottom_x, "S46.R70 O71 X3 clears ignored filters");
    assert!(edits.single_file && edits.use_drill_place_file_origin, "S46.R70 X3 retains disabled choices");
    let state = edits.dialog_state();
    assert!(!state.units_enabled && !state.single_file_enabled && !state.origin_enabled && !state.smd_only_enabled && !state.exclude_th_enabled && !state.negate_bottom_x_enabled, "S46.R70/R84 X3 disables ignored controls");
    assert!(state.board_edge_enabled && state.origin_reason.contains("useauxorigin"), "S46.R70/R84 origin explanation names plot setting");
    assert_eq!(names(&run(&b, &edits)), ["b-pnp_top.gbr", "b-pnp_bottom.gbr"], "S46.R70/R71 O71 X3 always splits");
    let mut edited_job = j.clone();
    edited_job.side = PlacementSide::Back;
    edited_job.description = "kept".into();
    edited_job.apply_dialog_edits(&edits);
    assert_eq!((edited_job.path, edited_job.side, edited_job.description.as_str()), (PlacementPath::Job, PlacementSide::Both, "kept"), "S46.R68 job dialog writes both and preserves description");
    let state = edited_job.dialog_state();
    assert!(!state.messages_visible && !state.folder_browser_visible && state.output_label == "Output file:", "S46.R67 job dialog presentation");
    edits.set_dialog_format(PlacementFormat::Csv);
    assert!(edits.dialog_state().origin_enabled && !edits.dialog_state().board_edge_enabled, "S46.R70 text controls restored");
}

#[test]
fn cli_validation_order_variants_and_flags() {
    let bad = PlacementExportOptions::from_cli(false, &["--format", "xml"]).expect_err("S46.R69 unreadable board");
    assert_eq!((bad.exit_code, bad.message.as_str()), (3, "Board file does not exist or is not accessible"), "S46.R69 O73 board check precedes format");
    for (args, message) in [
        (vec!["--side", "top", "--units", "cm", "--format", "xml"], "Invalid format"),
        (vec!["--side", "top", "--units", "cm"], "Invalid units specified"),
        (vec!["--side", "top"], "Invalid side specified"),
        (vec!["--format", "gerber"], "\"both\" not supported for Gerber format"),
    ] {
        let bad = PlacementExportOptions::from_cli(true, &args).expect_err("S46.R69 invalid CLI");
        assert_eq!((bad.exit_code, bad.message.as_str()), (1, message), "S46.R69 O63 ordered validation");
    }
    let bad = PlacementExportOptions::from_cli(true, &["--variant", "A", "--variant", "B", "-o", "out.pos"]).expect_err("S46.R69 multiple variants");
    assert_eq!(bad.message, "When specifying multiple variants, the output path must contain ${VARIANT} to generate separate output files for each variant.", "S46.R69 O72 multiple-variant diagnostic");
    assert_eq!(bad.exit_code, 1, "S46.R69 O72 multiple-variant exit");
    let front = PlacementExportOptions::from_cli(true, &["--format", "gerber", "--side", "front", "-o", "p.gbr"]).expect("S46.R69 O63 valid single-side Gerber CLI");
    let result = run(&board(Vec::new()), &front);
    assert_eq!(names(&result), ["p.gbr"], "S46.R71 O63 exact CLI Gerber path");
    assert!(text(&result.files[0]).contains("%TF.FileFunction,Component,L1,Top*%"), "S46.R79 O63 front CLI component layer");
    let opts = PlacementExportOptions::from_cli(true, &["--variant", "A", "--variant", "B", "-o", "out-${VARIANT}.pos"]).expect("S46.R69 variant template");
    assert_eq!(opts.variant, "A", "S46.R69 O72 only first variant selected");
    assert_eq!(names(&run(&board(Vec::new()), &opts)), ["out-A.pos"], "S46.R69/R71 O72 only first variant written");
    let opts = PlacementExportOptions::from_cli(true, &["--format", "csv", "--units", "mm", "--side", "back", "--use-drill-file-origin", "--smd-only", "--exclude-fp-th", "--exclude-dnp", "--exclude-bom", "--bottom-negate-x", "--gerber-board-edge", "--output", "x.csv"]).expect("S46.R67 all flags");
    assert!(opts.smd_only && opts.exclude_footprints_with_th && opts.exclude_dnp && opts.exclude_bom && opts.negate_bottom_x && opts.gerber_board_edge && opts.use_drill_place_file_origin, "S46.R67 boolean flags");
    assert_eq!((opts.format, opts.units, opts.side, opts.output_filename.as_str()), (PlacementFormat::Csv, PlacementUnits::Millimeters, PlacementSide::Back, "x.csv"), "S46.R67 value flags");
}

#[test]
fn filenames_splits_and_success_messages() {
    let b = board(vec![footprint("R1", LayerId::F_CU, P::ZERO)]);
    let mut j = options(PlacementPath::Job);
    j.output_filename = "build.v1/b.pos".into();
    let result = run(&b, &j);
    assert_eq!(names(&result), ["build.v1/b-top.pos", "build.v1/b-bottom.pos"], "S46.R71 O61 separate job files");
    assert_eq!(result.messages, ["Wrote front position data to 'build.v1/b-top.pos'.", "Wrote back position data to 'build.v1/b-bottom.pos'."], "S46.R83 O61 job messages");
    j.side = PlacementSide::Front;
    assert_eq!(names(&run(&b, &j)), ["build.v1/b-top.pos"], "S46.R71 O61 one side");
    j.side = PlacementSide::Back;
    j.format = PlacementFormat::Csv;
    j.output_filename = "b.custom".into();
    assert_eq!(names(&run(&b, &j)), ["b-bottom-pos.custom"], "S46.R71 job keeps custom extension");
    j.side = PlacementSide::Both;
    j.output_filename = "b.csv".into();
    j.single_file = true;
    assert_eq!(names(&run(&b, &j)), ["b-all-pos.csv"], "S46.R71 O62 CSV single job file");
    j.format = PlacementFormat::GerberX3;
    j.output_filename = "b.pos".into();
    assert_eq!(names(&run(&b, &j)), ["b-pnp_top.gbr", "b-pnp_bottom.gbr"], "S46.R71 O64 X3 extension ignores output extension");
    let mut d = options(PlacementPath::Dialog);
    d.output_filename = "output".into();
    let result = run(&b, &d);
    assert_eq!(names(&result), ["output/b-all.pos"], "S46.R71 dialog folder and board basename");
    assert_eq!(result.messages, ["Placement file: 'output/b-all.pos'.", "Component count: 1.", "Done."], "S46.R83 dialog combined-file messages");
    d.single_file = false;
    d.format = PlacementFormat::Csv;
    let result = run(&b, &d);
    assert_eq!(names(&result), ["output/b-top-pos.csv", "output/b-bottom-pos.csv"], "S46.R71 dialog separate CSV");
    assert_eq!(result.messages, ["Front (top side) placement file: 'output/b-top-pos.csv'.", "Component count: 1.", "Back (bottom side) placement file: 'output/b-bottom-pos.csv'.", "Component count: 0.", "Full component count: 1.", "Done."], "S46.R83 split counts include empty side");
    for (format, default) in [(PlacementFormat::Ascii, "b.pos"), (PlacementFormat::Csv, "b.csv"), (PlacementFormat::GerberX3, "b.gbr")] {
        let mut c = options(PlacementPath::Cli);
        c.format = format;
        c.side = PlacementSide::Front;
        assert_eq!(names(&run(&b, &c)), [default], "S46.R71 CLI default extension");
        c.output_filename = "exact.output".into();
        c.single_file = false;
        assert_eq!(names(&run(&b, &c)), ["exact.output"], "S46.R71 CLI exact path regardless of extension or split flag");
    }
}

#[test]
fn text_numbers_layout_units_and_signed_zero() {
    let mut b = board(vec![footprint("R1", LayerId::F_CU, P::mm(10.0, 20.0))]);
    let mut o = options(PlacementPath::Dialog);
    o.use_drill_place_file_origin = false;
    let result = run(&b, &o);
    let lines: Vec<_> = text(&result.files[0]).lines().collect();
    assert_eq!(lines[0], "### Footprint positions ###", "S46.R66/R77 no creation date");
    assert_eq!(lines[1], format!("### Printed by Fab version {}", version()), "S46.R77 major.minor Fab software line");
    assert_eq!(lines[2..4], ["## Unit = mm, Angle = deg.", "## Side : All"], "S46.R77 units and side");
    assert_eq!(lines[4], "# Ref     Val       Package                 PosX       PosY       Rot  Side", "S46.R77 O65 exact column header");
    assert_eq!(lines[5], "R1        10k       R_0603_1608Metric    10.0000   -20.0000    0.0000  top", "S46.R75/R77 O65 exact row");
    assert_eq!(lines[6], "## End", "S46.R77 end record");
    b.footprints[0].at = P::new(31_250, 93_750);
    b.footprints[0].default_variant.value = "10 k".into();
    let result = run(&b, &o);
    assert_eq!(text(&result.files[0]).lines().nth(5), Some("R1        10_k      R_0603_1608Metric     0.0312    -0.0938    0.0000  top"), "S46.R75/R76/R77 O66 binary ties to even and spaces");
    o.format = PlacementFormat::Csv;
    let result = run(&b, &o);
    assert_eq!(text(&result.files[0]), "Ref,Val,Package,PosX,PosY,Rot,Side\n\"R1\",\"10 k\",\"R_0603_1608Metric\",0.031250,-0.093750,0.000000,top\n", "S46.R78 O66 CSV row and no end line");
    b.footprints = [0, 20, -20].into_iter().enumerate().map(|(i, y)| footprint(&format!("R{i}"), LayerId::F_CU, P::new(0, y))).collect();
    o.format = PlacementFormat::Ascii;
    let result = run(&b, &o);
    let ys: Vec<_> = text(&result.files[0]).lines().skip(5).take(3).map(|l| l.split_whitespace().nth(4).expect("S46.R77 Y column")).collect();
    assert_eq!(ys, ["0.0000", "-0.0000", "0.0000"], "S46.R75/R76 O67 negate integer zero; retain rounded negative zero");
    o.format = PlacementFormat::Csv;
    let result = run(&b, &o);
    let rows = csv_rows(text(&result.files[0]), ',');
    assert_eq!(rows[1..].iter().map(|r| r[4].as_str()).collect::<Vec<_>>(), ["0.000000", "-0.000020", "0.000020"], "S46.R78 O67 CSV precision");
    assert_eq!(fixed(1.0078125, 0, 6), "1.007812", "S46.R76 six-place even lower tie");
    assert_eq!(fixed(-1.0234375, 0, 6), "-1.023438", "S46.R76 six-place even upper tie");
    assert_eq!(length(50, PlacementUnits::Millimeters, 0, 4), "0.0000", "S46.R75/R76 mm multiplication is below this rounding boundary; division would print 0.0001");
    assert_eq!(length(1_270, PlacementUnits::Inches, 0, 4), "0.0000", "S46.R75/R76 inch quotient factor is multiplied; direct division would print 0.0001");
    o.units = PlacementUnits::Inches;
    b.footprints = vec![footprint("R1", LayerId::F_CU, P::new(25_400_000, -12_700_000))];
    let result = run(&b, &o);
    assert!(text(&result.files[0]).contains(",1.000000,0.500000,0.000000,top\n"), "S46.R75 inch multiply factor");
    o.format = PlacementFormat::Ascii;
    assert!(text(&run(&b, &o).files[0]).contains("## Unit = inches, Angle = deg.\n"), "S46.R77 inch label");
}

#[test]
fn unicode_columns_and_csv_quotes() {
    let mut fp = footprint("Référence 1", LayerId::F_CU, P::ZERO);
    fp.library_id = "Lib:Package with space".into();
    fp.default_variant.value = "1/4\"".into();
    let b = board(vec![fp]);
    let mut o = options(PlacementPath::Dialog);
    let result = run(&b, &o);
    let line = text(&result.files[0]).lines().nth(5).expect("S46.R77 row");
    assert!(line.starts_with("Référence_1  1/4\"      Package_with_space  "), "S46.R77 widths count Unicode scalars and omit library nickname");
    o.format = PlacementFormat::Csv;
    let result = run(&b, &o);
    assert!(text(&result.files[0]).contains("\"Référence 1\",\"1/4\"\"\",\"Package with space\","), "S46.R78 doubled quote and spaces kept");
    let rows = csv_rows(text(&result.files[0]), ',');
    assert_eq!(&rows[1][..3], ["Référence 1", "1/4\"", "Package with space"], "S46.R78 quoted fields round trip");
}

#[test]
fn natural_order_and_stable_equal_references() {
    let b = board(vec![
        footprint("a1", LayerId::F_CU, P::ZERO), footprint("B1", LayerId::F_CU, P::ZERO),
        footprint("R10", LayerId::F_CU, P::ZERO), footprint("R2", LayerId::F_CU, P::ZERO),
        footprint("R01", LayerId::F_CU, P::new(1_000_000, 0)), footprint("R1", LayerId::F_CU, P::new(2_000_000, 0)),
        footprint("Z1", LayerId::B_CU, P::ZERO),
    ]);
    let mut o = options(PlacementPath::Dialog);
    o.format = PlacementFormat::Csv;
    let result = run(&b, &o);
    let rows = csv_rows(text(&result.files[0]), ',');
    assert_eq!(rows[1..].iter().map(|r| r[0].as_str()).collect::<Vec<_>>(), ["Z1", "B1", "R01", "R1", "R2", "R10", "a1"], "S46.R74 O69 bottom first, case-sensitive natural order, stable digit ties");
    let report = report_text(&b);
    assert_eq!(report.lines().filter_map(|l| l.strip_prefix("$MODULE ")).collect::<Vec<_>>(), ["a1", "B1", "R01", "R1", "R2", "R10", "Z1"], "S46.R82 O69 report ignores case");
    o.format = PlacementFormat::GerberX3;
    let result = run(&b, &o);
    let top = text(&result.files[0]);
    assert!(top.find("%TO.C,a1*%").expect("S46.R79 first component") < top.find("%TO.C,B1*%").expect("S46.R79 second component"), "S46.R74 X3 preserves board order");
}

#[test]
fn filters_chosen_variant_and_empty_files() {
    let mut b = board(vec![footprint("R1", LayerId::F_CU, P::ZERO)]);
    b.current_variant = "current".into();
    b.footprints[0].default_variant.exclude_from_pos_files = true;
    b.footprints[0].variants.insert("chosen".into(), PlacementVariant { value: "chosen value".into(), ..Default::default() });
    let mut o = PlacementExportOptions::for_dialog(&b);
    let empty = run(&b, &o);
    assert!(empty.files.is_empty(), "S46.R72 O70 dialog suppresses empty text export");
    assert_eq!(empty.messages, ["No footprint for automated placement."], "S46.R72 O70 empty message");
    let j = run(&b, &options(PlacementPath::Job));
    assert_eq!(names(&j), ["b-top.pos", "b-bottom.pos"], "S46.R72 O70 jobs retain empty files");
    for file in &j.files {
        assert_eq!(text(file).lines().count(), 6, "S46.R72/R77 O70 five header lines plus end");
        assert!(text(file).ends_with("## End\n"), "S46.R72/R77 empty text termination");
    }
    o.variant = "chosen".into();
    for format in [PlacementFormat::Ascii, PlacementFormat::Csv, PlacementFormat::GerberX3] {
        o.format = format;
        let result = run(&b, &o);
        assert_eq!(result.files.iter().map(|f| f.component_count).sum::<usize>(), 1, "S46.R84 chosen variant controls count in every format");
        let value = if format == PlacementFormat::Ascii { "chosen_value" } else { "chosen value" };
        assert!(text(&result.files[0]).contains(value), "S46.R84 chosen variant controls every file value");
    }
    o.variant.clear();
    let result = run(&b, &o);
    assert_eq!(names(&result), ["b-pnp_top.gbr", "b-pnp_bottom.gbr"], "S46.R72 O70 X3 always writes both sides");
    assert!(result.files.iter().all(|f| f.component_count == 0 && !text(f).contains("%TO.C,")), "S46.R72 O70 X3 empty component stream");

    let mut fp = footprint("U1", LayerId::F_CU, P::ZERO);
    fp.smd = false;
    fp.pads.push(PlacementPad { number: "1".into(), kind: PadKind::Smd, at: P::ZERO,
        rotation: Angle::ZERO, layers: LayerSet::one(LayerId::F_CU), shape: PadShape::Circle { d: 500_000 },
        shape_offset: P::ZERO, drill_size: P::ZERO });
    b = board(vec![fp]);
    o = options(PlacementPath::Cli);
    o.smd_only = true;
    assert_eq!(run(&b, &o).files[0].component_count, 0, "S46.R73 SMD filter uses footprint attribute, not pad kind");
    b.footprints[0].smd = true;
    assert_eq!(run(&b, &o).files[0].component_count, 1, "S46.R73 SMD footprint included");
    o.exclude_footprints_with_th = true;
    for kind in [PadKind::Thru, PadKind::Npth] {
        b.footprints[0].pads[0].kind = kind;
        assert_eq!(run(&b, &o).files[0].component_count, 0, "S46.R73 both plated and nonplated through pads excluded");
    }
    o.format = PlacementFormat::GerberX3;
    o.side = PlacementSide::Front;
    assert_eq!(run(&b, &o).files[0].component_count, 1, "S46.R70/R73 X3 ignores SMD and TH filters");
    for format in [PlacementFormat::Ascii, PlacementFormat::Csv, PlacementFormat::GerberX3] {
        o.format = format;
        o.smd_only = false;
        o.exclude_footprints_with_th = false;
        b.footprints[0].default_variant.dnp = true;
        b.footprints[0].default_variant.exclude_from_bom = true;
        o.exclude_dnp = false; o.exclude_bom = false;
        assert_eq!(run(&b, &o).files[0].component_count, 1, "S46.R73 DNP/BOM included by default");
        o.exclude_dnp = true;
        assert_eq!(run(&b, &o).files[0].component_count, 0, "S46.R73 DNP filter applies in every format");
        o.exclude_dnp = false; o.exclude_bom = true;
        assert_eq!(run(&b, &o).files[0].component_count, 0, "S46.R73 BOM filter applies in every format");
    }
}

#[test]
fn coordinate_origins_bottom_x_and_rotation() {
    let mut b = board(vec![footprint("B1", LayerId::B_CU, P::mm(10.0, 20.0)), footprint("T1", LayerId::F_CU, P::mm(10.0, 20.0))]);
    b.aux_origin = P::mm(3.0, 4.0);
    b.footprints[0].rotation = Angle::deg(270.0);
    b.footprints[1].rotation = Angle::deg(-180.0);
    let mut o = options(PlacementPath::Dialog);
    o.format = PlacementFormat::Csv;
    o.use_drill_place_file_origin = false;
    o.negate_bottom_x = true;
    let result = run(&b, &o);
    let rows = csv_rows(text(&result.files[0]), ',');
    assert_eq!(&rows[1][3..], ["-10.000000", "-20.000000", "-90.000000", "bottom"], "S46.R75 O68 bottom X negated, stored signed rotation");
    assert_eq!(&rows[2][3..], ["10.000000", "-20.000000", "180.000000", "top"], "S46.R75 O68 top unchanged, angle in (-180,180]");
    o.use_drill_place_file_origin = true;
    let result = run(&b, &o);
    assert!(text(&result.files[0]).contains(",-7.000000,-16.000000,-90.000000,bottom"), "S46.R75 origin subtracted before bottom negation");
    o.format = PlacementFormat::GerberX3;
    let result = run(&b, &o);
    assert!(text(&result.files[1]).contains("X10000000Y-20000000D03*"), "S46.R70/R79 O68 X3 ignores bottom negation and placement origin");
    b.use_aux_origin = true;
    let result = run(&b, &o);
    assert!(text(&result.files[1]).contains("X7000000Y-16000000D03*"), "S46.R79/R84 X3 uses useauxorigin");
    o.format = PlacementFormat::Csv;
    b.footprints = vec![footprint("B1", LayerId::B_CU, P::new(Nm::MIN, Nm::MIN))];
    b.aux_origin = P::new(Nm::MAX, Nm::MAX);
    let result = run(&b, &o);
    assert!(text(&result.files[0]).contains(",4294.967295,4294.967295,"), "S46.R75 subtraction and sign changes use wide integers");
}

#[test]
fn x3_file_attributes_mounts_edges_and_determinism() {
    let mut b = board(vec![footprint("T1", LayerId::F_CU, P::mm(10.0, 20.0)), footprint("B1", LayerId::B_CU, P::ZERO)]);
    b.copper_layers = 4;
    b.footprints[1].smd = false;
    b.footprints[1].through_hole = true;
    b.board_edges.push(Shape::Segment { a: P::ZERO, b: P::mm(5.0, 2.0), w: 100_000 });
    let mut o = options(PlacementPath::Job);
    o.format = PlacementFormat::GerberX3;
    let result = run(&b, &o);
    assert_eq!(names(&result), ["b-pnp_top.gbr", "b-pnp_bottom.gbr"], "S46.R71 O64 default X3 filenames");
    for (file, function, mount) in [(&result.files[0], "Component,L1,Top", "SMD"), (&result.files[1], "Component,L4,Bot", "TH")] {
        let s = text(file);
        assert!(s.contains(&format!("%TF.FileFunction,{function}*%")), "S46.R79 O63/O64 physical copper numbering");
        assert!(s.contains("%FSLAX46Y46*%\n%MOMM*%"), "S46.R79 fixed 4.6 millimeters");
        assert!(s.contains(&format!("%TO.CMnt,{mount}*%")), "S46.R70 X3 records mount type");
        assert!(s.contains("%TF.GenerationSoftware,Makepad,Fab*%"), "S46.R79 Fab generator");
        assert!(!s.contains("CreationDate") && !s.contains('\r') && s.ends_with("M02*\n"), "S46.R66/R80 deterministic LF stream");
        let image = crate::read_gerber(&file.file.bytes).expect("S46.R79 X3 basic geometry readback");
        assert_eq!(image.items.len(), 2, "S46.R79 one edge plus one component flash");
        assert_eq!(image.items[0].geom, crate::image::Geom::Shape(Shape::Segment { a: P::ZERO, b: P::mm(5.0, -2.0), w: 100_000 }), "S46.R79 edge Y uses same file axes as components");
    }
    assert_eq!(run(&b, &o), result, "S46.R66 all output bytes deterministic");
    o.gerber_board_edge = false;
    let without = run(&b, &o);
    assert_eq!(crate::read_gerber(&without.files[0].file.bytes).expect("S46.R79 no edge readback").items.len(), 1, "S46.R79 board-edge option controls geometry");
    o.units = PlacementUnits::Inches;
    o.single_file = true;
    o.negate_bottom_x = true;
    o.use_drill_place_file_origin = false;
    assert_eq!(run(&b, &o), without, "S46.R70 X3 ignores text-only unit/split/origin/negation options");
}

#[test]
fn footprint_report_layout_and_pad_fields() {
    let empty = board(Vec::new());
    assert_eq!(report_text(&empty), format!("## Footprint report\n## Printed by Fab version {}\n## Unit = mm, Angle = deg.\n\n$BeginDESCRIPTION\n\n$BOARD\nupper_left_corner  0.000000  0.000000\n$EndBOARD\n\n$EndDESCRIPTION\n", version()), "S46.R66/R82 O74 complete empty report");
    assert_eq!(report_text(&empty).lines().count(), 11, "S46.R82 O74 eleven lines");
    let mut fp = footprint("R1", LayerId::F_CU, P::mm(10.0, 20.0));
    for (number, x) in [("2", 825_000), ("1", -825_000)] {
        fp.pads.push(PlacementPad { number: number.into(), kind: PadKind::Smd, at: P::new(x, 0),
            rotation: Angle::ZERO, layers: LayerSet::one(LayerId::F_CU),
            shape: PadShape::RoundRect { size: P::new(800_000, 950_000), ratio_ppm: 250_000 },
            shape_offset: P::ZERO, drill_size: P::ZERO });
    }
    fp.default_variant.exclude_from_pos_files = true;
    let mut b = board(vec![fp]);
    b.aux_origin = P::mm(3.0, 4.0);
    let text = report_text(&b);
    let block = "$MODULE R1\nreference R1\nvalue 10k\nfootprint Resistor_SMD:R_0603_1608Metric\nattribut smd\nposition 10.000000 20.000000  orientation 0.00\nlayer front\n$PAD \"1\"\nShape Roundrect Layer front\nposition -0.825000  0.000000 size  0.800000  0.950000 orientation 0.00\ndrill  0.000000\nshape_offset  0.000000  0.000000\n$EndPAD\n$PAD \"2\"\nShape Roundrect Layer front\nposition  0.825000  0.000000 size  0.800000  0.950000 orientation 0.00\ndrill  0.000000\nshape_offset  0.000000  0.000000\n$EndPAD\n$EndMODULE R1\n\n";
    assert!(text.contains(block), "S46.R81/R82 O75 exact module/pad fields, pad order, absolute Y and no exclusion");
    let file = footprint_report(&b, ReportUserUnits::Mils, "out");
    assert_eq!(file.name, "out/b.rpt", "S46.R81 footprint report path");
    assert!(String::from_utf8(file.bytes).expect("S46.R82 text").contains("## Unit = inches, Angle = deg."), "S46.R81 mil editor units choose inches");
    b.current_variant = "A".into();
    b.footprints[0].variants.insert("A".into(), PlacementVariant { value: "A value".into(), dnp: true, ..Default::default() });
    assert!(report_text(&b).contains("value A value\n"), "S46.R81 report uses current variant, including DNP");
}

#[test]
fn report_all_physical_layers_and_shape_vocabulary() {
    let mut b = board(Vec::new());
    for (i, layer) in [LayerId::F_CU, LayerId::B_CU, LayerId::inner(1), LayerId::F_SILK, LayerId::B_SILK,
        LayerId::F_MASK, LayerId::B_MASK, LayerId::F_ADHES, LayerId::B_ADHES, LayerId::F_PASTE, LayerId::B_PASTE].into_iter().enumerate() {
        let p = P::new(-(i as i32 + 1) * 1_000_000, -(i as i32 + 2) * 1_000_000);
        b.physical_extents = vec![PlacementExtent { layers: LayerSet::one(layer), bounds: BBox::point(p) }];
        assert_eq!(b.report_bounds().min, p, "S46.R82/Q10 every physical layer contributes without visibility");
        assert!(report_text(&b).contains(&format!("upper_left_corner {} {}", length(p.x as i64, PlacementUnits::Millimeters, 9, 6), length(p.y as i64, PlacementUnits::Millimeters, 9, 6))), "S46.R82 physical bounds reach report");
    }
    b.physical_extents.push(PlacementExtent { layers: LayerSet::of(&[LayerId::EDGE, LayerId::F_CRTYD, LayerId::F_FAB]), bounds: BBox::point(P::mm(-100.0, -100.0)) });
    assert_eq!(b.report_bounds().min, P::mm(-11.0, -12.0), "S46.R82 nonphysical drawings do not move report corner");
    let size = P::mm(1.0, 2.0);
    for (shape, name) in [
        (PadShape::Circle { d: 1_000_000 }, "Circle"), (PadShape::Oval { size }, "Oval"),
        (PadShape::Rect { size }, "Rect"), (PadShape::Trapezoid { size, delta: P::ZERO }, "Trap"),
        (PadShape::RoundRect { size, ratio_ppm: 0 }, "Roundrect"),
        (PadShape::Chamfer { size, ratio_ppm: 0, chamfer_ppm: 100_000, corners: 1 }, "Chamferedrect"),
        (PadShape::Custom { anchor: Box::new(PadShape::Rect { size }), shapes: Vec::new() }, "CustomShape"),
    ] {
        let mut fp = footprint("X1", LayerId::DRAWINGS, P::ZERO);
        fp.smd = false;
        fp.pads.push(PlacementPad { number: "a\"1".into(), kind: PadKind::Npth, at: P::new(-1, 1),
            rotation: Angle::deg(270.0), layers: LayerSet::of(&[LayerId::F_CU, LayerId::B_CU]),
            shape, shape_offset: P::mm(0.1, -0.2), drill_size: P::mm(0.3, 0.8) });
        b.footprints = vec![fp];
        let report = report_text(&b);
        assert!(report.contains(&format!("Shape {name} Layer both\n")), "S46.R82 English pad shape and both-side membership");
        assert!(report.contains("attribut virtual\n") && report.contains("layer other\n") && report.contains("$PAD \"a\"1\"\n"), "S46.R82 virtual footprint, other layer, pad quotes unescaped");
        assert!(report.contains("orientation -90.00\ndrill  0.300000\nshape_offset  0.100000 -0.200000\n"), "S46.R82 stored pad angle, X drill size and default shape offset");
    }
}

#[test]
fn messages_and_host_io_failures() {
    assert_eq!(PlacementError::directory(PlacementPath::Dialog, "out").message, "Could not write plot files to folder 'out'.", "S46.R83 dialog folder failure");
    assert_eq!(PlacementError::directory(PlacementPath::Cli, "out"), error(4, "Failed to create output directory"), "S46.R83 CLI folder failure");
    assert_eq!(PlacementError::file("out.pos"), error(4, "Failed to create file 'out.pos'."), "S46.R81/R83 file failure is explicit");
    let report = footprint_report(&board(Vec::new()), ReportUserUnits::Millimeters, "out");
    assert_eq!(footprint_report_message(&report), "Footprint report file created: out/b.rpt", "S46.R81 report success message for host after successful write");
}

#[test]
fn table_bottom_modes_and_validator_reject_wrong_convention() {
    use crate::model::{render, CamModel, Pose};
    use crate::profile::{FileKind, ResolvedProfile};
    use crate::tables::{fill_generic_tables, PlacementOrigin, PlacementRules};
    use fab_elec_board::{Board, Origin, Placed};
    use fab_elec_circuit::Circuit;
    use fab_elec_core::{CompId, Contour, ItemId, Side};
    use fab_elec_parts::Footprint;
    use std::sync::Arc;
    let board = Board { outline: vec![Contour::rect(P::mm(3.0, 4.0), P::mm(23.0, 24.0), 0)],
        placed: vec![Placed { id: ItemId(0), comp: CompId(0), path: "R1".into(), refdes: "R1".into(),
            footprint: Arc::new(Footprint::default()), pad_nets: Vec::new(), at: P::mm(10.0, 20.0),
            rot: Angle::deg(90.0), side: Side::Bottom, locked: false, origin: Origin::Authored, body_z: None, ref_label: None, value_label: None, legend: None, value: None }],
        ..Default::default() };
    let mut profile = ResolvedProfile::standard_2_layer("b", "A");
    profile.files.retain(|f| f.kind == FileKind::Placement);
    let model = CamModel { poses: vec![Pose { refdes: "R1".into(), at: P::mm(10.0, 20.0), rot: Angle::deg(90.0), side: Side::Bottom }], ..Default::default() };
    assert_eq!(PlacementRules::default().bottom_x, BottomX::NegateX, "S46.R75 reference mode defaults to negation");
    for (mode, expected_x, expected_rot) in [(BottomX::NegateX, -7_000_000, "90"), (BottomX::MirrorAbout(20_000_000), 13_000_000, "270")] {
        let rules = PlacementRules { origin: PlacementOrigin::BoardLowerLeft, bottom_x: mode, ..Default::default() };
        fill_generic_tables(&mut profile, &board, &Circuit::default(), &rules);
        assert_eq!(profile.placement.bottom_x, Some(mode), "S46.R75 mode retained for validation");
        assert_eq!(profile.placement.rows[0].at, P::new(expected_x, 16_000_000), "S46.R75 bottom mode applies after origin");
        assert_eq!(profile.placement.rows[0].cells[4], expected_rot, "S46.R75 negation retains rotation; factory mirror preserves prior convention");
        let files = render(&model, &profile).expect("S46.R75 render placement");
        assert!(crate::validate(&files, &profile, &model).ok(), "S46.R75 validator accepts selected bottom mode");
        profile.placement.bottom_x = Some(if mode == BottomX::NegateX { BottomX::MirrorAbout(20_000_000) } else { BottomX::NegateX });
        assert!(!crate::validate(&files, &profile, &model).ok(), "S46.R75 validator rejects mismatched bottom convention");
    }
}

#[test]
fn every_placement_file_ends_its_lines_with_a_single_line_feed_on_any_platform() {
    let mut front = footprint("R1", LayerId::F_CU, P::mm(10.0, 20.0));
    front.default_variant.value = "line\r\nbreak\tvalue".into();
    let back = footprint("R2", LayerId::B_CU, P::mm(30.0, 5.0));
    let b = board(vec![front, back]);
    for path in [PlacementPath::Dialog, PlacementPath::Job, PlacementPath::Cli] {
        for format in [PlacementFormat::Ascii, PlacementFormat::Csv, PlacementFormat::GerberX3] {
            let mut o = options(path);
            o.format = format;
            if format == PlacementFormat::GerberX3 {
                // a Gerber position file is one side (S46.R70)
                o.side = PlacementSide::Front;
            }
            let export = run(&b, &o);
            assert!(!export.files.is_empty(), "S46.R80 {path:?} {format:?} writes a file");
            for f in &export.files {
                let t = text(f);
                assert!(!t.contains('\r'), "S46.R80 {path:?} {format:?} {}: no carriage return, embedded line breaks included", f.file.name);
                assert!(t.is_empty() || t.ends_with('\n'), "S46.R80 {path:?} {format:?} {}: the last line ends with a line feed", f.file.name);
            }
        }
    }
    let report = report_text(&b);
    assert!(!report.contains('\r') && report.ends_with('\n'), "S46.R80 the footprint report ends every line with a single line feed");
}

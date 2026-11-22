use super::*;

fn item(reference: &str, value: &str) -> BomItem {
    BomItem { reference: reference.into(), value: value.into(), ..Default::default() }
}
fn document(items: Vec<BomItem>) -> BomDocument { BomDocument { items, ..Default::default() } }
fn table(options: &BomExportOptions, doc: &BomDocument, variant: &str) -> BomTable {
    build_table(doc, &options.resolve(doc, &BomPresets::default()).unwrap(), variant)
}
fn text(options: &BomExportOptions, doc: &BomDocument, variant: &str) -> String {
    let resolved = options.resolve(doc, &BomPresets::default()).unwrap();
    write_table(&build_table(doc, &resolved, variant), &resolved.format, BomOutputFormat::Delimited)
}

#[test]
fn per_path_defaults_and_builtin_presets() {
    let dialog = BomExportOptions::for_path(BomPath::Dialog);
    let job = BomExportOptions::for_path(BomPath::Job);
    let cli = BomExportOptions::for_path(BomPath::Cli);
    let doc = document(vec![item("C1", "100µF"), item("C2", "100µF"), item("C3", "100µF")]);
    assert_eq!(dialog.output_filename, "${PROJECTNAME}.csv", "S46.R85 dialog output default");
    assert!(dialog.group_symbols && dialog.sort_asc && !dialog.exclude_dnp, "S46.R85 dialog defaults");
    assert_eq!(dialog.format, builtin_format("CSV").unwrap(), "S46.R85 editor CSV defaults");
    assert_eq!(dialog.fields_group_by, strings(&["Value", "${DNP}", "${EXCLUDE_FROM_BOM}", "${EXCLUDE_FROM_BOARD}", "${EXCLUDE_FROM_SIM}", "${EXCLUDE_FROM_POS_FILES}", "Footprint"]), "S46.R87 Default Editing grouping");
    assert_eq!(text(&job, &doc, ""), "", "S46.R86 job has no columns");
    assert_eq!(job.format, BomFormat::default(), "S46.R86 omitted job delimiters are empty");
    assert_eq!(text(&cli, &doc, ""), "\"Refs\",\"Value\",\"Footprint\",\"Qty\",\"DNP\"\n\"C1\",\"100µF\",\"\",\"1\",\"\"\n\"C2\",\"100µF\",\"\",\"1\",\"\"\n\"C3\",\"100µF\",\"\",\"1\",\"\"\n", "S46.R86 CLI has no grouping, all cells quoted, UTF-8 and LF");
    assert_eq!(table(&dialog, &doc, "").rows.len(), 1, "S46.R87 dialog groups equal values/attributes/footprints");
    let attrs = builtin_view("Attributes").unwrap();
    assert!(!attrs.fields[2].show && !attrs.fields[3].show && attrs.fields[3].group_by, "S46.R87 Attributes hides Datasheet and grouped Footprint");
    assert!(builtin_view("Grouped By Value and Footprint").unwrap().fields[3].group_by, "S46.R87 footprint preset groups footprint");
    assert!(builtin_view("grouped by value").is_none() && builtin_format("csv").is_none(), "S46.R87 preset names are exact");
    assert_eq!(builtin_format("TSV").unwrap().string_delimiter, "", "S46.R87 TSV is unquoted");
    assert_eq!(builtin_format("Semicolons").unwrap().string_delimiter, "'", "S46.R87 Semicolons uses apostrophes");
}

#[test]
fn field_lists_labels_wildcards_and_hidden_grouping() {
    let doc = document(vec![item("R1", "10k")]);
    let mut options = BomExportOptions::for_path(BomPath::Cli);
    for (fields, labels, header) in [
        ("${QUANTITY},Reference,Value", Some("Qty,Refs,Value"), "\"Qty\",\"Refs\",\"Value\""),
        ("Reference,Value,QUANTITY,DNP", Some("Refs,Value,Qty,DNP"), "\"Refs\",\"Value\",\"Qty\",\"DNP\""),
        ("ITEM_NUMBER,Reference,Value", Some("#,Refs,Value"), "\"#\",\"Refs\",\"Value\""),
        ("Reference,Value,EXCLUDE_FROM_BOM", Some("Refs,Value,Excl BOM"), "\"Refs\",\"Value\",\"Excl BOM\""),
        ("Reference,QUANTITY", None, "\"Reference\",\"QUANTITY\""),
        ("Reference,*,Value", None, "\"Reference\",\"Footprint\",\"Datasheet\",\"Description\",\"${QUANTITY}\",\"${ITEM_NUMBER}\",\"${DNP}\",\"${EXCLUDE_FROM_BOM}\",\"${EXCLUDE_FROM_BOARD}\",\"${EXCLUDE_FROM_SIM}\",\"Value\""),
        ("Reference,EXCLUDE_FROM_POS_FILES", None, "\"Reference\",\"EXCLUDE_FROM_POS_FILES\""),
    ] {
        options.set_cli_fields(Some(fields), labels, Some(""));
        assert_eq!(text(&options, &doc, "").lines().next().unwrap(), header, "S46.R89 O82–O85 field list {fields}");
    }
    assert_eq!(table(&options, &doc, "").rows[0].cells[1], "", "S46.R89 O85 bare EXCLUDE_FROM_POS_FILES is an unknown user field");
    options.set_cli_fields(Some("Reference,,*,__Value, DNP"), Some("Refs,unused,Val,spaced"), Some("__Value,EXCLUDE_FROM_BOARD"));
    let resolved = options.resolve(&doc, &BomPresets::default()).unwrap();
    let fields = &resolved.view.fields;
    assert_eq!(fields.iter().find(|f| f.name == "Value").unwrap(), &BomField { name: "Value".into(), label: "Val".into(), show: false, group_by: true }, "S46.R89 empty entry consumes a label, wildcard does not");
    assert_eq!(fields.iter().find(|f| f.name == " DNP").unwrap().label, "spaced", "S46.R89 no trimming of fields");
    assert!(fields.iter().find(|f| f.name == "${EXCLUDE_FROM_BOARD}").unwrap().group_by, "S46.R89 grouping marks wildcard-added fields");
    options.set_cli_fields(Some("Reference"), None, Some("Value"));
    let resolved = options.resolve(&doc, &BomPresets::default()).unwrap();
    assert!(!resolved.view.fields[1].show && resolved.view.fields[1].group_by, "S46.R89 missing group field appended hidden");
    options.sort_field = "Value".into();
    assert_eq!(table(&options, &document(vec![item("R10", "a"), item("R2", "z")]), "").rows[0].refs, ["R10"], "S46.R89 hidden columns remain available for sorting");
    options.sort_field = "nosuch".into();
    assert_eq!(table(&options, &document(vec![item("R10", "a"), item("R2", "z")]), "").rows[0].refs, ["R2"], "S46.R89 missing sort field falls back to references");
    let mut custom = doc.clone();
    custom.template_fields.push("Vendor".into());
    custom.items[0].fields.insert("MPN".into(), "part".into());
    options.set_cli_fields(Some("*"), None, Some(""));
    let header = table(&options, &custom, "").header;
    assert!(header.contains(&"MPN".into()) && header.contains(&"Vendor".into()), "S46.R89 wildcard includes user and template fields");
}

#[test]
fn text_quoting_breaks_tabs_multichar_delimiters_and_bom() {
    let mut options = BomExportOptions::for_path(BomPath::Cli);
    options.set_cli_fields(Some("Value"), None, None);
    let doc = document(vec![item("R1", "1/4\""), item("R2", "a\r\nb"), item("R3", "x\ty")]);
    assert_eq!(text(&options, &doc, ""), "\"Value\"\n\"1/4\"\"\"\n\"ab\"\n\"xy\"\n", "S46.R91/R98 O86/O87 all values quoted, quotes doubled, breaks/tabs stripped");
    options.format.keep_line_breaks = true;
    options.format.keep_tabs = true;
    assert_eq!(text(&options, &doc, ""), "\"Value\"\n\"1/4\"\"\"\n\"a\r\nb\"\n\"x\ty\"\n", "S46.R91 O87 preserve embedded breaks/tabs");
    options.bom_format_preset_name = "Semicolons".into();
    assert_eq!(text(&options, &document(vec![item("R1", "it's")]), ""), "'Value'\n'it''s'\n", "S46.R87/R91 O86 Semicolons doubles apostrophes");
    options.bom_format_preset_name.clear();
    options.set_cli_fields(Some("Reference,Value"), None, None);
    options.format.string_delimiter = "<>".into();
    options.format.field_delimiter = "||".into();
    assert_eq!(text(&options, &document(vec![item("R1", "a<>b")]), ""), "<>Reference<>||<>Value<>\n<>R1<>||<>a<><>b<>\n", "S46.R91 whole multi-character delimiters doubled");
    options.format.include_byte_order_mark = true;
    assert!(text(&options, &doc, "").as_bytes().starts_with(&[0xef, 0xbb, 0xbf]), "S46.R91 UTF-8 byte order mark");
    options.set_cli_fields(Some("__Reference,__Value"), None, None);
    assert_eq!(text(&options, &doc, ""), "", "S46.R91 no shown fields means zero bytes even with BOM");
    let mut profile = BomTable { header: strings(&["Ref", "Val"]), rows: vec![BomRow { cells: strings(&["R1", "1/4\""]), refs: strings(&["R1"]) }], delimiter: ';' };
    assert_eq!(write_reference_profile(&profile), "\"Ref\";\"Val\"\n\"R1\";\"1/4\"\"\"\n", "S46.R98 factory profile uses its labels/delimiter with reference quoting");
    profile.header.clear();
    assert_eq!(write_reference_profile(&profile), "", "S46.R91 reference profile with no columns is empty");
    assert_eq!(crate::text::csv_cell("plain", ','), "plain", "S46.R98 original factory CSV contract preserved");
    assert_eq!(crate::text::csv_cell_quoted("plain", "\"", true, true), "\"plain\"", "S46.R98 additive reference quoting");
}

#[test]
fn reference_ranges_channels_and_multiunit_deduplication() {
    let mut options = BomExportOptions::for_path(BomPath::Cli);
    options.set_cli_fields(Some("Reference,QUANTITY"), None, Some("Value"));
    let doc = document(["C9", "C3", "C1", "C2", "C5", "C6"].into_iter().map(|r| item(r, "100nF")).collect());
    assert_eq!(table(&options, &doc, "").rows[0].cells, ["C1-C3,C5,C6,C9", "6"], "S46.R93 O88 three consecutive refs form ranges, pairs stay separate");
    options.format.ref_range_delimiter.clear();
    assert_eq!(table(&options, &doc, "").rows[0].cells[0], "C1,C2,C3,C5,C6,C9", "S46.R93 O88 empty range delimiter");
    options.format.ref_range_delimiter = "-".into();
    let doc = document(["R01", "R2", "R3", "R3"].into_iter().map(|r| item(r, "10k")).collect());
    assert_eq!(table(&options, &doc, "").rows[0].cells, ["R01-R3", "3"], "S46.R93 O88 leading zeros and multi-unit physical symbol counted once");
    let refs = [("U1", "B"), ("U1", "A"), ("U2", "B"), ("U3", "B"), ("U2", "A"), ("U3", "A"), ("U?", "B"), ("U", "B")]
        .into_iter().map(|(r, suffix)| BomReference { reference: r.into(), unit_suffix: suffix.into() }).collect::<Vec<_>>();
    assert_eq!(reference_list(&refs, &options.format), "U1B-U3B,U?B,UB,U1A-U3A", "S46.R93 channel first-appearance order; '?' and empty number never in ranges");
    let refs = ["R999999999999999999999999", "R1000000000000000000000000", "R1000000000000000000000001"]
        .into_iter().map(|r| BomReference { reference: r.into(), unit_suffix: String::new() }).collect::<Vec<_>>();
    assert_eq!(reference_list(&refs, &options.format), "R999999999999999999999999-R1000000000000000000000001", "S46.R93 reference numbers do not overflow bounded integers");
}

#[test]
fn grouping_attributes_variants_sorting_and_filters() {
    let mut r1 = item("R1", "10k");
    r1.footprint = "R_0603".into();
    r1.variants.insert("Variant 1".into(), BomOverride { value: Some("10K_V1".into()), ..Default::default() });
    let mut r2 = item("R2", "10k");
    r2.footprint = "R_0805".into();
    r2.attributes.dnp = true;
    r2.variants.insert("Variant 1".into(), BomOverride { dnp: Some(false), ..Default::default() });
    let mut r3 = item("R3", "10k");
    r3.attributes.exclude_from_bom = true;
    r3.variants.insert("Variant 1".into(), BomOverride { exclude_from_bom: Some(false), ..Default::default() });
    let mut r4 = item("R4", "10k");
    r4.variants.insert("Variant 1".into(), BomOverride { exclude_from_bom: Some(true), ..Default::default() });
    let doc = document(vec![r1, r2, r3, r4]);
    let mut options = BomExportOptions::for_path(BomPath::Cli);
    options.set_cli_fields(Some("Reference,Value,Footprint,QUANTITY,ITEM_NUMBER"), None, Some("Value"));
    let base = table(&options, &doc, "");
    assert_eq!(base.rows[0].refs, ["R1", "R2", "R4"], "S46.R92 base excludes BOM flag but retains DNP by default");
    assert_eq!(base.rows[0].cells[2], "R_0603,R_0805,", "S46.R98 mixed field values joined once in item order");
    let mut preset = options.clone();
    preset.bom_preset_name = "Grouped By Value".into();
    let preset_table = table(&preset, &doc, "");
    assert_eq!(preset_table.rows.len(), 2, "S46.R87 Grouped By Value separates DNP state");
    assert_eq!(preset_table.rows[0].refs, ["R1", "R4"], "S46.R87 populated group excludes DNP members");
    options.exclude_dnp = true;
    assert_eq!(table(&options, &doc, "").rows[0].refs, ["R1", "R4"], "S46.R85 exclude DNP option");
    let variant = table(&options, &doc, "Variant 1");
    assert_eq!(variant.rows[0].cells[1], "10K_V1", "S46.R90 variant changes value");
    assert_eq!(variant.rows[1].refs, ["R2", "R3"], "S46.R90 variant DNP/exclusions applied before grouping");
    assert_eq!(variant.rows[1].cells[4], "2", "S46.R89 item numbering follows final row order");
    for path in [BomPath::Dialog, BomPath::Job, BomPath::Cli] {
        let mut preset = BomExportOptions::for_path(path);
        preset.apply_view(&builtin_view("Default Editing").unwrap());
        preset.bom_preset_name = "Attributes".into();
        assert!(!table(&preset, &doc, "").rows.iter().any(|r| r.refs.iter().any(|r| r == "R3")), "S46.R92 no path or include preset exports excluded R3: {path:?}");
    }
    options.set_sort_asc("false").unwrap();
    assert_eq!(table(&options, &doc, "Variant 1").rows[0].refs, ["R2", "R3"], "S46.R85 O96 descending sort works");
    assert!(options.set_sort_asc("no").is_err(), "S46.R85 sort accepts true/false only");
    options.filter_string = "0805".into();
    options.filter_scope = FilterScope::Visible;
    assert_eq!(table(&options, &doc, "Variant 1").rows[0].refs, ["R2"], "S46.R85 visible-field filter");
    options.filter_scope = FilterScope::Reference;
    assert!(table(&options, &doc, "Variant 1").rows.is_empty(), "S46.R85 reference filter does not search values");
    options.filter_scope = FilterScope::All;
    options.set_cli_fields(Some("Reference"), None, Some(""));
    assert_eq!(table(&options, &doc, "Variant 1").rows[0].refs, ["R2"], "S46.R85 all-field filter searches hidden fields");
}

#[test]
fn dialog_scope_and_job_whole_design() {
    let mut a = item("R1", "1k");
    a.sheet_path = "/sheet".into();
    let mut b = item("R2", "2k");
    b.sheet_path = "/sheet/child".into();
    b.selected = true;
    let mut c = item("R3", "3k");
    c.sheet_path = "/sheet-other".into();
    let doc = document(vec![a, b, c]);
    let mut options = BomExportOptions::for_path(BomPath::Dialog);
    options.symbol_scope = SymbolScope::Sheet("/sheet".into());
    assert_eq!(table(&options, &doc, "").rows.len(), 1, "S46.R85 current sheet only");
    options.symbol_scope = SymbolScope::SheetAndChildren("/sheet".into());
    assert_eq!(table(&options, &doc, "").rows.len(), 2, "S46.R85 descendants use path boundaries");
    options.symbol_scope = SymbolScope::Selection;
    assert_eq!(table(&options, &doc, "").rows[0].refs, ["R2"], "S46.R85 selected symbols only");
    let mut pcb = doc.clone();
    pcb.source = BomSource::Board;
    assert_eq!(table(&options, &pcb, "").rows.len(), 3, "S46.R85 schematic sheet/selection scope does not restrict a board BOM");
    options.path = BomPath::Job;
    assert_eq!(table(&options, &doc, "").rows.len(), 3, "S46.R85 jobs always whole design");
}

#[test]
fn job_settings_and_editor_job_round_trip() {
    let doc = document(vec![item("R1", "10k")]);
    let job = read_bom_job_json("{}").unwrap();
    assert_eq!(text(&job, &doc, ""), "", "S46.R86 O89 omitted fields yields zero bytes");
    let job = read_bom_job_json(r#"{"fields_ordered":["Reference","Value"],"bom_preset_name":"nosuch","bom_format_preset_name":"nosuch"}"#).unwrap();
    assert_eq!(text(&job, &doc, ""), "ReferenceValue\nR110k\n", "S46.R86/R88 O89 absent delimiters concatenate; job preset names cleared");
    assert!(job.bom_preset_name.is_empty() && job.bom_format_preset_name.is_empty(), "S46.R88 job cannot name presets");
    let mut view = builtin_view("Grouped By Value").unwrap();
    view.fields[2].show = false;
    let mut options = BomExportOptions::for_path(BomPath::Job);
    options.apply_view(&view);
    options.format = builtin_format("CSV").unwrap();
    options.bom_preset_name = "ignored".into();
    options.bom_format_preset_name = "ignored".into();
    options.variant_names = strings(&["Variant 1", "Variant2"]);
    let saved = write_bom_job_json(&options);
    let read = read_bom_job_json(&saved).unwrap();
    assert_eq!(read.fields_ordered, strings(&["Reference", "Value", "__Datasheet", "Footprint", "${QUANTITY}", "${DNP}"]), "S46.R89 O92 saved hidden column");
    assert_eq!(read.fields_group_by, strings(&["Value", "${DNP}"]), "S46.R89 O92 saved grouping");
    assert_eq!(read.fields_labels, options.fields_labels, "S46.R85 all labels survive round trip");
    assert_eq!(read.format, options.format, "S46.R86 saved editor format carries delimiters");
    assert_eq!(read.variant_names, options.variant_names, "S46.R85 variant names remain ordered");
    assert!(read.bom_preset_name.is_empty() && read.bom_format_preset_name.is_empty(), "S46.R88 O92 saved names empty");
    assert!(read_bom_job_json(r#"{"fields_ordered":[1]}"#).is_err(), "S46.R85 lists require strings");
    assert!(read_bom_job_json(r#"{"sort_asc":"false"}"#).is_err(), "S46.R85 job booleans are typed");
    assert_eq!(read_bom_job_json(r#"{"description":"\ud83d\udd27","future":{"count":1e40}}"#).unwrap().description, "🔧", "S46.R85 JSON Unicode and unknown structured keys");
}

#[test]
fn named_presets_override_flags_and_user_presets_override_builtins() {
    let doc = document(vec![item("C1", "100nF"), item("C2", "100nF"), item("C3", "100nF")]);
    let mut options = BomExportOptions::for_path(BomPath::Cli);
    options.bom_preset_name = "Grouped By Value".into();
    options.filter_string = "no match".into();
    options.sort_asc = false;
    assert_eq!(text(&options, &doc, ""), "\"Reference\",\"Value\",\"Datasheet\",\"Footprint\",\"Qty\",\"DNP\"\n\"C1-C3\",\"100nF\",\"\",\"\",\"3\",\"\"\n", "S46.R88 O90 preset replaces view flags but keeps CLI format ranges");
    options.bom_preset_name = "nosuch".into();
    assert_eq!(options.resolve(&doc, &BomPresets::default()).unwrap_err(), BomError::new(2, "BOM preset 'nosuch' not found"), "S46.R88 O91 missing view error");
    options.bom_preset_name.clear();
    options.bom_format_preset_name = "nosuch".into();
    assert_eq!(options.resolve(&doc, &BomPresets::default()).unwrap_err(), BomError::new(2, "BOM format preset 'nosuch' not found"), "S46.R88 O91 missing format error");
    let presets = read_bom_presets_json(r#"{
      "bom_presets":[{"name":"Grouped By Value","sort_field":"Reference","sort_asc":true,"filter_string":"","group_symbols":true,"exclude_dnp":false}],
      "bom_fmt_presets":[
        {"name":"CSV","field_delimiter":";","string_delimiter":"'","ref_delimiter":"|","ref_range_delimiter":"","keep_tabs":false,"keep_line_breaks":false,"include_byte_order_mark":true},
        {"name":"bad","field_delimiter":","}
      ]
    }"#).unwrap();
    assert!(presets.views["Grouped By Value"].fields.is_empty() && !presets.views["Grouped By Value"].include_excluded_from_bom, "S46.R87 missing fields/include use specified defaults");
    assert_eq!(presets.views["Grouped By Value"].filter_scope, FilterScope::Reference, "S46.R87 missing filter scope default");
    assert!(!presets.formats.contains_key("bad"), "S46.R87 incomplete format preset ignored");
    options.bom_format_preset_name = "CSV".into();
    let resolved = options.resolve(&doc, &presets).unwrap();
    assert_eq!(resolved.format.field_delimiter, ";", "S46.R88 user format wins over built-in");
    assert!(write_table(&build_table(&doc, &resolved, ""), &resolved.format, BomOutputFormat::Delimited).starts_with('\u{feff}'), "S46.R88 format preset replaces BOM option");
    options.bom_preset_name = "Grouped By Value".into();
    assert!(options.resolve(&doc, &presets).unwrap().view.fields.is_empty(), "S46.R88 user view wins over built-in");
}

#[test]
fn variants_paths_folders_and_status_messages() {
    let context = BomPathContext { design_file: "/project/board.sch".into(), job_output_dir: "/job".into(), current_dir: "/cwd".into(), variables: BTreeMap::from([("PROJECTNAME".into(), "demo".into()), ("OUT".into(), "exports".into())]) };
    let mut doc = BomDocument::default();
    doc.current_variant = "current".into();
    let mut options = BomExportOptions::for_path(BomPath::Cli);
    options.variant_names = strings(&["Variant 1", "Variant2"]);
    let err = plan_files(&options, &doc, &context, |_| false).unwrap_err();
    assert_eq!(err.exit_code, 1, "S46.R90 O79 multi-variant CLI without placeholder fails");
    assert_eq!(err.message, "When specifying multiple variants, the output path must contain ${VARIANT} to generate separate output files for each variant.", "S46.R90 multiple-variant message");
    options.output_filename = "bom_${VARIANT}.csv".into();
    let files = plan_files(&options, &doc, &context, |_| false).unwrap();
    assert_eq!(files, vec![BomFilePlan { variant: "Variant 1".into(), path: "/cwd/bom_Variant 1.csv".into() }, BomFilePlan { variant: "Variant2".into(), path: "/cwd/bom_Variant2.csv".into() }], "S46.R90 O80 variant names and spaces preserved");
    let mut part = item("R1", "base");
    part.variants.insert("Variant 1".into(), BomOverride { value: Some("10K_V1".into()), ..Default::default() });
    part.variants.insert("Variant2".into(), BomOverride { exclude_from_bom: Some(true), ..Default::default() });
    doc.items.push(part);
    let output = export_bom(&options, &doc, &BomPresets::default(), &context, BomOutputFormat::Delimited, |_| false).unwrap();
    assert!(String::from_utf8_lossy(&output[0].data).contains("10K_V1"), "S46.R90 per-file variant drives values");
    assert_eq!(String::from_utf8_lossy(&output[1].data).lines().count(), 1, "S46.R90 per-file variant drives exclusions");
    options.variant_names.clear();
    assert_eq!(plan_files(&options, &doc, &context, |_| false).unwrap()[0].path, "/cwd/bom_.csv", "S46.R90 O81 CLI default variant is empty, not current editor variant");
    options.output_filename = "out/".into();
    assert_eq!(plan_files(&options, &doc, &context, |_| false).unwrap()[0].path, "/cwd/out/board.csv", "S46.R94 O93 both board/schematic append design filename for trailing slash");
    options.output_filename = "out".into();
    assert_eq!(plan_files(&options, &doc, &context, |p| p == "/cwd/out").unwrap()[0].path, "/cwd/out/board.csv", "S46.R94 existing folder also appends filename");
    options.path = BomPath::Job;
    options.output_filename.clear();
    options.variant_names = strings(&["Variant 1", "Variant2"]);
    assert_eq!(plan_files(&options, &doc, &context, |_| false).unwrap(), vec![BomFilePlan { variant: "Variant 1".into(), path: "/job/board.csv".into() }], "S46.R90/R94 jobs use only first variant and run output folder");
    options = BomExportOptions::for_path(BomPath::Dialog);
    let files = plan_files(&options, &doc, &context, |_| false).unwrap();
    assert_eq!(files[0], BomFilePlan { variant: "current".into(), path: "/project/demo.csv".into() }, "S46.R85/R94 editor current variant and project variables");
    options.output_filename = "${OUT}/file.csv".into();
    assert_eq!(plan_files(&options, &doc, &context, |_| false).unwrap()[0].path, "/project/exports/file.csv", "S46.R94 host environment/text variables expanded");
    options.output_filename = "C:\\out\\file.csv".into();
    assert_eq!(plan_files(&options, &doc, &context, |_| false).unwrap()[0].path, "C:\\out\\file.csv", "S46.R94 absolute Windows paths independent of host OS");
    assert_eq!(python_bom_input_error(), BomError::new(3, "Schematic file does not exist or is not accessible"), "S46.R96 python-bom input error");
    assert_eq!(python_bom_path("", &context, |_| false), "/cwd/board-bom.xml", "S46.R96 python-bom default name");
    assert_eq!(python_bom_path("out/", &context, |_| false), "/cwd/out/board-bom.xml", "S46.R96 python-bom folder appends default");
    assert_eq!(success_message(BomPath::Dialog, "board.csv"), "Wrote BOM output to 'board.csv'", "S46.R95 editor success message");
    assert_eq!(success_message(BomPath::Cli, "board.csv"), "Wrote bill of materials to 'board.csv'.", "S46.R95 CLI success message");
    assert_eq!(annotation_warnings(BomSource::Board, true, true).len(), 1, "S46.R95 board annotation warning once");
    assert_eq!(annotation_warnings(BomSource::Schematic, true, true).len(), 2, "S46.R95 schematic annotation/sheet warnings");
    for (failure, code) in [(BomIoFailure::Input, 3), (BomIoFailure::Directory, 4), (BomIoFailure::Open, 3), (BomIoFailure::Write, 2)] {
        assert_eq!(io_error(BomPath::Cli, failure, "out").exit_code, code, "S46.R95 I/O exit status {failure:?}");
    }
}

#[test]
fn legacy_board_bom_exact_rows_and_base_attributes() {
    let mut r1 = item("R1", "10k");
    r1.footprint = "Resistor_SMD:R_0603_1608Metric".into();
    r1.attributes.dnp = true;
    let mut r2 = r1.clone();
    r2.reference = "R2".into();
    let mut r3 = r1.clone();
    r3.reference = "R3".into();
    r3.attributes.exclude_from_bom = true;
    let mut c1 = item("C1", "100nF");
    c1.footprint = "Capacitor_SMD:C_0603_1608Metric".into();
    let mut doc = document(vec![r2, r3, c1, r1]);
    doc.current_variant = "V1".into();
    doc.items[0].variants.insert("V1".into(), BomOverride { value: Some("ignored".into()), exclude_from_bom: Some(true), ..Default::default() });
    assert_eq!(write_board_legacy(&doc, str::to_owned).unwrap(), "\"Id\";\"Designator\";\"Footprint\";\"Quantity\";\"Designation\";\"Supplier and ref\";\n1;\"C1\";\"C_0603_1608Metric\";1;\"100nF\";;;\n2;\"R1, R2\";\"R_0603_1608Metric\";2;\"10k\";;;\n", "S46.R97 O94 exact legacy lines, DNP included, base exclusions, variants ignored");
    let doc = document(vec![item("R1", "${V}"), item("R2", "${V}"), item("R3", "resolved")]);
    let out = write_board_legacy(&doc, |s| s.replace("${V}", "resolved")).unwrap();
    assert!(out.contains("\"R1, R3\"") && out.contains("2;\"R2\""), "S46.R97 stored values compare with first resolved value");
    let mut a = item("u1", "a");
    a.footprint = "A:Same".into();
    let mut b = item("U1", "a");
    b.footprint = "B:Same".into();
    let out = write_board_legacy(&document(vec![a, b]), str::to_owned).unwrap();
    assert!(out.contains("1;\"u1\"") && out.contains("2;\"U1\""), "S46.R97 full library id splits groups, case-insensitive ties keep board order");
    assert_eq!(write_board_legacy(&BomDocument::default(), str::to_owned).unwrap_err().message, "Cannot export BOM: there are no footprints on the PCB.", "S46.R97 empty PCB produces no file");
}

#[test]
fn fab_html_xml_are_escaped_and_deterministic() {
    let mut options = BomExportOptions::for_path(BomPath::Cli);
    options.set_cli_fields(Some("Value"), None, None);
    let doc = document(vec![item("R1", "<script>&\"'100µF")]);
    let resolved = options.resolve(&doc, &BomPresets::default()).unwrap();
    let table = build_table(&doc, &resolved, "");
    for format in [BomOutputFormat::Html, BomOutputFormat::Xml] {
        let out = write_table(&table, &resolved.format, format);
        assert!(out.contains("&lt;script&gt;&amp;&quot;&apos;100µF"), "S46.R98 Fab markup format escapes data: {format:?}");
        assert_eq!(out, write_table(&table, &resolved.format, format), "S46.R66/R98 deterministic table without timestamps: {format:?}");
    }
}

#[test]
fn generators_are_settings_only_and_confirm_exact_command() {
    let malicious = r#"{"fields_ordered":["Reference"],"command":"project command","confirmed":true,"generators":[{"nickname":"evil","command":"board command","confirmed":true}],"nested":{"fields_ordered":["Value"]}}"#;
    let job = read_bom_job_json(malicious).unwrap();
    assert_eq!(job.fields_ordered, ["Reference"], "S46.R99 nested/project command keys do not become settings");
    assert_eq!(read_bom_presets_json(malicious).unwrap(), BomPresets::default(), "S46.R99 project generator entries ignored");
    let doc = document(vec![BomItem { fields: BTreeMap::from([("command".into(), "schematic command".into()), ("confirmed".into(), "true".into())]), ..item("R1", "1k") }]);
    assert_eq!(text(&job, &doc, ""), "Reference\nR1\n", "S46.R99 design commands are inert table data");
    let mut settings = GeneratorSettings::default();
    assert!(settings.commands.is_empty(), "S46.R99 default application generator list empty; no bundled scripts");
    settings.commands.push(GeneratorCommand::new("own", "tool %P %B %I %O %Z"));
    let context = GeneratorContext { project_folder: "/project/".into(), schematic_file: "/project/demo.sch".into(), fully_annotated: true, windows: false };
    assert!(!settings.commands[0].confirmed(), "S46.R99 each command starts unconfirmed");
    let preview = settings.preview(0, BomPath::Dialog, &context).unwrap();
    assert_eq!(preview.command, "tool /project demo /project/demo.xml /project/demo %Z", "S46.R96 exact substitutions, unknown percent token retained");
    assert_eq!(preview.intermediate_file, "/project/demo.xml", "S46.R96 legacy intermediate path");
    assert!(!preview.confirmed, "S46.R99 exact expanded command must be shown before first use");
    assert!(settings.record_confirmation(0, &context, "different text").is_err(), "S46.R99 cannot confirm stale or different displayed command");
    settings.record_confirmation(0, &context, &preview.command).unwrap();
    assert!(settings.preview(0, BomPath::Dialog, &context).unwrap().confirmed, "S46.R99 recorded confirmation exposed");
    let mut changed = context.clone();
    changed.schematic_file = "/project/other.sch".into();
    assert!(!settings.preview(0, BomPath::Dialog, &changed).unwrap().confirmed, "S46.R99 changed substitution requires fresh exact-text confirmation");
    settings.commands[0].set_command("other %I");
    assert!(!settings.commands[0].confirmed() && !settings.preview(0, BomPath::Dialog, &context).unwrap().confirmed, "S46.R99 edited command resets confirmation");
    for path in [BomPath::Job, BomPath::Cli] {
        assert!(settings.preview(0, path, &context).is_err(), "S46.R99 jobs and CLI cannot prepare a generator: {path:?}");
    }
    changed.fully_annotated = false;
    assert_eq!(settings.preview(0, BomPath::Dialog, &changed).unwrap_err().message, "Generating BOM requires a fully annotated schematic.", "S46.R96 annotation prerequisite");
    settings.commands.push(GeneratorCommand::new("win", "%O|%I"));
    let mut windows = GeneratorContext { project_folder: "C:\\project".into(), schematic_file: "C:\\project\\demo.sch".into(), fully_annotated: true, windows: true };
    assert_eq!(settings.preview(1, BomPath::Dialog, &windows).unwrap().command, "C:/project/demo|C:\\project\\demo.xml", "S46.R96 Windows conversion affects O only");
    windows.schematic_file = "\\\\server\\share\\demo.sch".into();
    assert!(settings.preview(1, BomPath::Dialog, &windows).unwrap().command.starts_with("\\\\server\\share\\demo|"), "S46.R96 UNC O retains backslashes");
}

#[test]
fn circuit_board_adapters_preserve_existing_model_data() {
    use fab_elec_board::{Board, Origin, Placed};
    use fab_elec_circuit::{Circuit, Component, Value};
    use fab_elec_core::{Angle, BlockId, CompId, ItemId, Side, P};
    use fab_elec_parts::{Fields, Footprint, PartDef, PartKind, Symbol};
    use std::sync::Arc;
    let part = Arc::new(PartDef {
        name: "R".into(), kind: PartKind::Resistor, symbol: Symbol::default(),
        footprint: Some(Arc::new(Footprint { name: "Lib:Default".into(), ..Default::default() })),
        body: None, pins: Vec::new(), fields: Fields(vec![("datasheet".into(), "part-data".into())]), emitter: None,
        gpio: None,
    });
    let mut circuit = Circuit::default();
    for (id, reference, dnp) in [(0, "R1", false), (1, "R2", true)] {
        circuit.comps.push(Component { id: CompId(id), path: reference.into(), refdes: reference.into(), ref_kind: Default::default(),
            part: part.clone(), value: Some(Value { text: "10k".into(), qty: None, tol: None }),
            conns: Vec::new(), nc: Vec::new(), block: BlockId(0), dnp,
            fields: Fields(vec![("description".into(), "resistor".into())]),
        });
    }
    let mut board = Board::default();
    board.placed.push(Placed { id: ItemId(0), comp: CompId(1), path: "R2".into(), refdes: "R2".into(),
        footprint: Arc::new(Footprint { name: "Lib:Placed".into(), ..Default::default() }),
        pad_nets: Vec::new(), at: P::ZERO, rot: Angle::ZERO, side: Side::Top, locked: false,
        origin: Origin::Authored, body_z: None, ref_label: None, value_label: None, legend: None, value: None,
    });
    let sch = crate::tables::bom_from_circuit(&circuit, Some(&board));
    let pcb = crate::tables::bom_from_board(&board, &circuit);
    assert_eq!(sch.items.len(), 2, "S46.R98 schematic includes unplaced components");
    assert_eq!(sch.items[0].footprint, "Lib:Default", "S46.R98 schematic library footprint fallback");
    assert_eq!(sch.items[1].footprint, "Lib:Placed", "S46.R98 placed footprint takes precedence");
    assert_eq!(pcb.items.len(), 1, "S46.R98 board contains only placements");
    assert!(pcb.items[0].attributes.dnp, "S46.R85 adapter retains DNP for exporter selection");
    assert_eq!(pcb.items[0].datasheet, "part-data", "S46.R89 part field inherited");
    assert_eq!(pcb.items[0].description, "resistor", "S46.R89 instance description available");
    assert_eq!(text(&BomExportOptions::for_path(BomPath::Cli), &sch, "").lines().count(), 3, "S46.R98 adapted circuit reaches reference CSV");
}

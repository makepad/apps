use super::*;
use crate::gerber_write::write_gerber;
use crate::image::{items_polyset, GItem, Geom, ObjAttrs, Polarity};
use crate::model::{render, CamModel};
use crate::profile::{FileKind, FileSpec, GerberVariant, ResolvedProfile};
use crate::read_gerber;
use crate::validate::region_difference;
use fab_elec_core::{mm, Angle, Shape, P};

fn pad() -> GItem {
    GItem::dark(Shape::Circle { c: P::new(123_456, -234_565), r: mm(0.2) }).with_attrs(ObjAttrs {
        aper_function: vec!["SMDPad".into(), "CuDef".into()],
        net: Some("s,1".into()),
        pin: Some(("R1".into(), "2".into())),
        component: Some("R1".into()),
    })
}

fn gerber(items: &[GItem], opts: &GerberOpts) -> String {
    let attrs = vec![("FileFunction".into(), vec!["Copper".into(), "L1".into(), "Top".into()]),
        ("FilePolarity".into(), vec!["Positive".into()]),
        ("CreationDate".into(), vec!["2000-01-01T00:00:00Z".into()])];
    String::from_utf8(write_gerber(items, &attrs, opts).expect("S46.R48: write our small geometry")).expect("S46.R48: ASCII Gerber")
}

#[test]
fn defaults_by_path() {
    for (path, protel, precision, dnp, zones, drill, bw) in [
        (GerberJobPath::Board, false, 6, true, true, DrillShape::Small, true),
        (GerberJobPath::Job, true, 5, true, false, DrillShape::Full, false),
        (GerberJobPath::Cli, true, 6, false, false, DrillShape::Full, false),
    ] {
        let o = defaults(path);
        assert_eq!(o.use_protel_file_extension, protel, "S46.R33: extensions for {path:?}");
        assert_eq!(o.precision, precision, "S46.R33: precision for {path:?}");
        assert_eq!((o.sketch_dnp_footprints_on_fab_layers, o.crossout_dnp_footprints_on_fab_layers), (dnp, dnp), "S46.R33: DNP for {path:?}");
        assert_eq!(o.check_zones, zones, "S46.R31/R33: zone check for {path:?}");
        assert_eq!((o.drill_shape, o.black_and_white), (drill, bw), "S46.R28/R32: stored shared defaults for {path:?}");
        assert!(o.create_gerber_job_file, "S46.R32/R46: job file default for {path:?}");
        assert!(o.use_x2_format && o.include_netlist_attributes && o.plot_footprint_values && o.plot_ref_des, "S46.R31/R32: attributes and texts for {path:?}");
        assert!(!o.all_copper_layers && !o.disable_aperture_macros && !o.use_drill_origin && !o.plot_drawing_sheet && !o.subtract_solder_mask_from_silk, "S46.R32: default switches for {path:?}");
        assert!(!o.sketch_pads_on_fab_layers && !o.plot_pad_numbers && !o.hide_dnp_footprints_on_fab_layers && !o.use_board_plot_params && !o.mirror && !o.negative, "S46.R32: sketch/shared switches for {path:?}");
        assert!(o.description.is_empty() && o.output_filename.is_empty() && o.drawing_sheet.is_empty() && o.variant.is_empty(), "S46.R32: host-resolved strings for {path:?}");
        assert!(o.layers_to_include_on_all_layers.is_empty() && o.text_variables.is_empty(), "S46.R32: no common layers or variable overrides for {path:?}");
        if path == GerberJobPath::Board {
            let selected = LayerSet::of(&o.layers);
            assert_eq!(selected.copper_only(), LayerSet::copper(32), "S46.R28/R29: default has all 32 reference copper roles");
            assert_eq!(selected.minus(LayerSet::ALL_COPPER), LayerSet::of(&[LayerId::F_SILK, LayerId::B_SILK, LayerId::F_MASK, LayerId::B_MASK, LayerId::F_PASTE, LayerId::B_PASTE, LayerId::EDGE]), "S46.R28/R29: default non-copper roles");
        } else {
            assert!(o.layers.is_empty(), "S46.R32: job/CLI resolve empty selection against enabled layers");
        }
    }
    assert_eq!(GerberOpts::default(), defaults(GerberJobPath::Board).writer_options().unwrap(), "S46.R48: existing writer defaults remain X2 with netlist attributes");
}

#[test]
fn precision_refusal_names_the_value() {
    let mut profile = ResolvedProfile::standard_2_layer("tiny", "A");
    profile.files = vec![FileSpec { name: "tiny.gbr".into(), kind: FileKind::Gerber { layer: LayerId::F_CU, variant: GerberVariant::X2 } }];
    for value in [0, 1, 2, 3, 4, 7, 255] {
        let opts = GerberOpts { decimals: value, ..GerberOpts::default() };
        let mut export = defaults(GerberJobPath::Job);
        export.precision = value;
        profile.gerber.decimals = value;
        for result in [
            write_gerber(&[], &[], &opts).map(|_| ()),
            export.writer_options().map(|_| ()),
            write_gerbers_job_json(&export).map(|_| ()),
            render(&CamModel::default(), &profile).map(|_| ()),
        ] {
            let error = result.expect_err("S46.R34: invalid precision is refused").to_string();
            assert!(error.contains("Gerber coordinate precision should be either 5 or 6") && error.contains(&format!("got {value}")), "S46.R34: message names {value}: {error}");
        }
    }
    for raw in ["4", "7", "-1", "99999999999999999999", "5.5", "\"7\"", "null", "true"] {
        let error = read_gerbers_job_json(&format!("{{\"precision\":{raw}}}")).expect_err("S46.R34: main ruling overrides job coercion").to_string();
        assert!(error.contains(&format!("got {raw}")), "S46.R34: JSON error retains the supplied value: {error}");
    }
    for (decimals, xy) in [(5, "X12346Y-23457D03*"), (6, "X123456Y-234565D03*")] {
        let text = gerber(&[pad()], &GerberOpts { decimals, ..GerberOpts::default() });
        assert!(text.contains(&format!("%FSLAX4{decimals}Y4{decimals}*%")) && text.contains("%MOMM*%") && text.contains(xy), "S46.R34/R48: 4.{decimals} millimetres and half-away-from-zero rounding");
    }
}

#[test]
fn x2_off_keeps_every_attribute_as_a_comment() {
    let items = vec![pad(), GItem::dark(Shape::Circle { c: P::mm(1.0, 0.0), r: mm(0.2) })];
    let x2 = gerber(&items, &GerberOpts::default());
    let x1 = gerber(&items, &GerberOpts { x2: false, ..GerberOpts::default() });
    let expected: Vec<_> = x2.lines().filter(|s| s.starts_with("%T")).map(|s| format!("G04 #@! {}*", &s[1..s.len() - 2])).collect();
    let comments: Vec<_> = x1.lines().filter(|s| s.starts_with("G04 #@! ")).collect();
    assert_eq!(comments, expected, "S46.R48: every TF/TA/TO/TD text is identical in X1");
    assert!(!x1.contains("%TF") && !x1.contains("%TA") && !x1.contains("%TO") && !x1.contains("%TD"), "S46.R48: no X2 attribute commands in X1");
    for body in ["TF.FileFunction,Copper,L1,Top", "TA.AperFunction,SMDPad,CuDef", "TO.N,s\\u002C1", "TO.P,R1,2", "TO.C,R1", "TD.N", "TD.P", "TD.C", "TD.AperFunction"] {
        assert!(x1.contains(&format!("G04 #@! {body}*")), "S46.R48: X1 retains {body}");
    }
    assert!(!x1.contains("CreationDate") && !x2.contains("CreationDate"), "S46.R48/R66: omit creation date in both attribute modes");
    assert_eq!(x1, gerber(&items, &GerberOpts { x2: false, ..GerberOpts::default() }), "S46.R48/R66: deterministic output");
    let geometry = |s: &str| s.lines().filter(|s| !s.starts_with("%T") && !s.starts_with("G04 #@! ")).map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(geometry(&x1), geometry(&x2), "S46.R48: attribute syntax does not alter geometry or aperture numbering");
}

#[test]
fn netlist_attributes_are_independent_of_x2() {
    let items = [pad(), GItem::dark(Shape::Circle { c: P::mm(1.0, 0.0), r: mm(0.1) })];
    for x2 in [false, true] {
        for netlist_attrs in [false, true] {
            let text = gerber(&items, &GerberOpts { x2, netlist_attrs, ..GerberOpts::default() });
            for name in ["TO.N", "TO.P", "TO.C", "TD.N", "TD.P", "TD.C"] {
                assert_eq!(text.contains(name), netlist_attrs, "S46.R48: {name} with x2={x2}, netlist={netlist_attrs}");
            }
            let prefix = if x2 { "%" } else { "G04 #@! " };
            assert!(text.contains(&format!("{prefix}TF.FileFunction")) && text.contains(&format!("{prefix}TA.AperFunction")), "S46.R48: disabling netlist preserves file/aperture attributes");
        }
    }
}

#[test]
fn no_aperture_macros_preserves_pad_regions() {
    let attrs = pad().attrs;
    let shapes = [
        Shape::Rect { c: P::ZERO, half: P::mm(0.4, 0.2), rot: Angle::deg(30.0), corner: 0 },
        Shape::Rect { c: P::mm(1.0, 0.0), half: P::mm(0.4, 0.2), rot: Angle::deg(-20.0), corner: mm(0.1) },
        Shape::Rect { c: P::mm(2.0, 0.0), half: P::mm(0.4, 0.2), rot: Angle::ZERO, corner: mm(0.1) },
        Shape::Oval { c: P::mm(3.0, 0.0), half: P::mm(0.4, 0.2), rot: Angle::deg(35.0) },
        Shape::Oval { c: P::mm(4.0, 0.0), half: P::mm(0.2, 0.4), rot: Angle::deg(25.0) },
    ];
    let mut items: Vec<_> = shapes.into_iter().map(|s| GItem::dark(s).with_attrs(attrs.clone())).collect();
    items[1].polarity = Polarity::Clear;
    for decimals in [5, 6] {
        let text = gerber(&items, &GerberOpts { decimals, no_aperture_macros: true, ..GerberOpts::default() });
        assert!(!text.contains("%AM") && !text.contains("D03*"), "S46.R48: every macro pad becomes a region");
        assert_eq!(text.matches("G36*").count(), items.len(), "S46.R48: one region per macro pad");
        let image = read_gerber(text.as_bytes()).expect("S46.R48: own macro-free output reads");
        assert!(image.items.iter().all(|i| matches!(&i.geom, Geom::Region(_))), "S46.R48: all macro-free pads are regions");
        assert_eq!(image.items[1].polarity, Polarity::Clear, "S46.R48: clear polarity survives lowering");
        assert_eq!(image.items[0].attrs.pin, attrs.pin, "S46.R48: netlist attributes survive lowering");
        assert!(image.items.iter().any(|i| matches!(&i.geom, Geom::Region(c) if c.path.segs.iter().any(|s| matches!(s, fab_elec_core::Seg::Arc { .. })))), "S46.R48: curved pads retain true arcs");
        let delta = region_difference(&image.to_polyset(200), &items_polyset(&items, 200), 1000);
        assert!(delta.is_none(), "S46.R48: macro-free pad geometry at precision {decimals}: {delta:?}");
    }
    let standards = [
        GItem::dark(Shape::Circle { c: P::ZERO, r: mm(0.1) }),
        GItem::dark(Shape::Rect { c: P::ZERO, half: P::mm(0.4, 0.2), rot: Angle::QUARTER, corner: 0 }),
        GItem::dark(Shape::Oval { c: P::ZERO, half: P::mm(0.4, 0.2), rot: Angle::ZERO }),
    ];
    let text = gerber(&standards, &GerberOpts { no_aperture_macros: true, ..GerberOpts::default() });
    assert_eq!(text.matches("D03*").count(), 3, "S46.R48: standard apertures still flash");
    assert!(!text.contains("G36*"), "S46.R48: disabling macros leaves standard shapes alone");
}

#[test]
fn file_names_sanitize_every_reserved_character_and_use_protel_extensions() {
    for ch in ['\\', '/', ':', '*', '?', '"', '<', '>', '|', '%', '.'] {
        assert_eq!(gerber_file_name("tiny.kicad_pcb", &format!(" a{ch}b "), false, LayerId::F_CU), "tiny-a_b.gbr", "S46.R45: replace {ch}");
    }
    assert_eq!(gerber_file_name("boards/tiny.kicad_pcb", " Top: sig/pwr. ", true, LayerId::F_CU), "tiny-Top_ sig_pwr_.gtl", "S46.R45: trim layer suffix and use custom name");
    assert_eq!(gerber_file_name("boards\\tiny.kicad_pcb", "   ", true, LayerId::B_CU), "tiny.gbl", "S46.R45: empty suffix has no hyphen");
    assert_eq!(job_file_name("boards/tiny.rev.kicad_pcb"), "tiny.rev-job.gbrjob", "S46.R45: job suffix and board stem");
    for (layer, suffix, ext) in [
        (LayerId::F_CU, "F_Cu", "gtl"), (LayerId::B_CU, "B_Cu", "gbl"),
        (LayerId::inner(1), "In1_Cu", "g1"), (LayerId::inner(2), "In2_Cu", "g2"),
        (LayerId::inner(30), "In30_Cu", "g30"),
        (LayerId::F_ADHES, "F_Adhesive", "gta"), (LayerId::B_ADHES, "B_Adhesive", "gba"),
        (LayerId::F_PASTE, "F_Paste", "gtp"), (LayerId::B_PASTE, "B_Paste", "gbp"),
        (LayerId::F_SILK, "F_Silkscreen", "gto"), (LayerId::B_SILK, "B_Silkscreen", "gbo"),
        (LayerId::F_MASK, "F_Mask", "gts"), (LayerId::B_MASK, "B_Mask", "gbs"),
        (LayerId::EDGE, "Edge_Cuts", "gm1"), (LayerId::F_FAB, "F_Fab", "gbr"),
        (LayerId::B_CRTYD, "B_Courtyard", "gbr"), (LayerId::user(0), "User_1", "gbr"),
    ] {
        let name = display_layer_name(layer);
        assert_eq!(gerber_file_name("tiny", &name, true, layer), format!("tiny-{suffix}.{ext}"), "S46.R45/Q3: Protel {layer:?}");
        assert_eq!(gerber_file_name("tiny", &name, false, layer), format!("tiny-{suffix}.gbr"), "S46.R45: no Protel {layer:?}");
    }
}

#[test]
fn job_json_keys_unknown_values_and_layer_identities() {
    let default = defaults(GerberJobPath::Job);
    let json = write_gerbers_job_json(&default).expect("S46.R32: encode defaults");
    let Json::Object(fields) = JsonReader::read(&json).unwrap() else { panic!("S46.R32: settings object") };
    let mut keys: Vec<_> = fields.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = vec!["description", "output_filename", "layers", "all_copper_layers", "layers_to_include_on_all_layers", "mirror", "black_and_white", "negative", "plot_footprint_values", "plot_ref_des", "hide_dnp_footprints_on_fab_layers", "sketch_dnp_footprints_on_fab_layers", "crossout_dnp_footprints_on_fab_layers", "sketch_pads_on_fab_layers", "plot_pad_numbers", "plot_drawing_sheet", "subtract_solder_mask_from_silk", "use_drill_origin", "drill_shape", "drawing_sheet", "check_zones", "variant", "include_netlist_attributes", "use_x2_format", "disable_aperture_macros", "use_protel_file_extension", "precision", "create_gerber_job_file"];
    expected.sort_unstable();
    assert_eq!(keys, expected, "S46.R32: exactly the gerbers settings keys, including output_filename");
    assert_eq!(read_gerbers_job_json(&json).unwrap(), default, "S46.R32: default settings round trip");
    let changed = r#"{"description":"changed","output_filename":"out/","layers":["F.Cu"],"all_copper_layers":true,"layers_to_include_on_all_layers":["Edge.Cuts"],"mirror":true,"black_and_white":true,"negative":true,"plot_footprint_values":false,"plot_ref_des":false,"hide_dnp_footprints_on_fab_layers":true,"sketch_dnp_footprints_on_fab_layers":false,"crossout_dnp_footprints_on_fab_layers":false,"sketch_pads_on_fab_layers":true,"plot_pad_numbers":true,"plot_drawing_sheet":true,"subtract_solder_mask_from_silk":true,"use_drill_origin":true,"drill_shape":"small","drawing_sheet":"custom","check_zones":true,"variant":"second","include_netlist_attributes":false,"use_x2_format":false,"disable_aperture_macros":true,"use_protel_file_extension":false,"precision":6,"create_gerber_job_file":false}"#;
    let written = write_gerbers_job_json(&read_gerbers_job_json(changed).unwrap()).unwrap();
    assert_eq!(JsonReader::read(&written).unwrap().encode(), JsonReader::read(changed).unwrap().encode(), "S46.R32: every key retains a nondefault value");
    let unknown = read_gerbers_job_json(r#"{"drill_shape":"huge","use_x2_format":"no","check_zones":7,"layers":false,"output_dir":"ignored","description":null,"future":{"precision":2,"x":[null,true,1.5e2]}}"#).unwrap();
    assert_eq!(unknown, default, "S46.R32/R34: unknown keys and values retain each key's default");
    let text = r#"{"description":"line\nquote\" slash\\ \u03a9 \ud83d\ude80","output_filename":"out/","layers":[0,2,4,1,5,25,39,127,"B.Fab","F.Cu","unknown","Layer150","In31.Cu","User.46",128,-1],"layers_to_include_on_all_layers":["Edge.Cuts","F.Cu","Edge.Cuts"],"precision":6,"drill_shape":"none","variant":"v","drawing_sheet":"sheet","use_x2_format":false,"include_netlist_attributes":true}"#;
    let opts = read_gerbers_job_json(text).unwrap();
    assert_eq!(opts.layers, vec![LayerId::F_CU, LayerId::B_CU, LayerId::inner(1), LayerId::F_MASK, LayerId::F_SILK, LayerId::EDGE, LayerId::user(0), LayerId::user(44), LayerId::B_FAB, LayerId::F_CU], "S46.R29/R34: numeric board identities mapped, unknown entries dropped");
    assert_eq!(opts.description, "line\nquote\" slash\\ Ω 🚀", "S46.R32: Unicode, surrogate pairs and string escapes");
    let encoded = write_gerbers_job_json(&opts).unwrap();
    assert!(encoded.contains("\"layers\": [\"F.Cu\", \"B.Cu\", \"In1.Cu\""), "S46.R34: layers are written as board-file names");
    assert_eq!(read_gerbers_job_json(&encoded).unwrap(), opts, "S46.R32/R34: custom strings, layer order and settings round trip");
    let mut all = defaults(GerberJobPath::Job);
    all.layers = defaults(GerberJobPath::Board).layers;
    let legacy = write_gerbers_job_json(&all).unwrap().replace("  \"all_copper_layers\": false,\n", "");
    assert!(read_gerbers_job_json(&legacy).unwrap().all_copper_layers, "S46.R34: legacy all-32-copper inference");
    assert!(!read_gerbers_job_json(&write_gerbers_job_json(&all).unwrap()).unwrap().all_copper_layers, "S46.R34: explicit false overrides inference");
    assert!(!read_gerbers_job_json(r#"{"layers":["F.Cu","B.Cu"]}"#).unwrap().all_copper_layers, "S46.R34: two copper roles do not mean all 32");
    for malformed in ["[]", "{", "{\"x\":01}", "{\"x\":1.}", "{\"x\":1e}", "{\"x\":[1,]}", "{\"x\":true,}", "{\"x\":\"\\ud800\"}", "{\"x\":\"\\udc00\"}", "{\"x\":\"\\é\"}", "{}x"] {
        assert!(read_gerbers_job_json(malformed).is_err(), "S46.R32: malformed settings rejected: {malformed}");
    }
}

#[test]
fn plot_order_selections_common_layers_and_board_overrides() {
    let ui = vec![LayerId::F_CU, LayerId::inner(1), LayerId::inner(2), LayerId::B_CU,
        LayerId::F_ADHES, LayerId::B_ADHES, LayerId::F_PASTE, LayerId::B_PASTE,
        LayerId::F_SILK, LayerId::B_SILK, LayerId::F_MASK, LayerId::B_MASK,
        LayerId::DRAWINGS, LayerId::COMMENTS, LayerId::ECO1, LayerId::ECO2,
        LayerId::EDGE, LayerId::MARGIN, LayerId::F_CRTYD, LayerId::B_CRTYD,
        LayerId::F_FAB, LayerId::B_FAB, LayerId::user(0), LayerId::user(1)];
    let mut unordered = ui.clone();
    unordered.reverse();
    unordered.push(LayerId::F_CU);
    assert_eq!(ui_order(&unordered), ui, "S46.R39: full UI/file order, duplicates removed");
    assert_eq!(plotting_stack_order(&unordered), vec![LayerId::B_CU, LayerId::B_MASK, LayerId::B_PASTE, LayerId::B_SILK, LayerId::B_ADHES, LayerId::B_CRTYD, LayerId::B_FAB,
        LayerId::inner(2), LayerId::inner(1), LayerId::F_CU, LayerId::F_MASK, LayerId::F_PASTE, LayerId::F_SILK, LayerId::F_ADHES, LayerId::F_CRTYD, LayerId::F_FAB,
        LayerId::DRAWINGS, LayerId::COMMENTS, LayerId::ECO1, LayerId::ECO2, LayerId::user(0), LayerId::user(1), LayerId::MARGIN, LayerId::EDGE], "S46.R39: full bottom-to-top plotting order");
    let enabled = [LayerId::F_CU, LayerId::inner(1), LayerId::B_CU, LayerId::F_MASK, LayerId::EDGE, LayerId::MARGIN, LayerId::F_FAB];
    let mut o = defaults(GerberJobPath::Job);
    assert_eq!(select_layers(GerberJobPath::Job, &o, &enabled), ui_order(&enabled), "S46.R40: empty job selects all enabled layers");
    assert_eq!(select_layers(GerberJobPath::Cli, &defaults(GerberJobPath::Cli), &enabled), ui_order(&enabled), "S46.R40: absent CLI layers selects enabled layers including fab/margin");
    o.layers = vec![LayerId::B_CU, LayerId::F_CU, LayerId::inner(2), LayerId::F_SILK, LayerId::EDGE];
    assert_eq!(select_layers(GerberJobPath::Board, &o, &enabled), vec![LayerId::F_CU, LayerId::B_CU, LayerId::EDGE], "S46.R40: drop unavailable copper and disabled technical layers");
    o.all_copper_layers = true;
    assert_eq!(select_layers(GerberJobPath::Job, &o, &enabled), vec![LayerId::F_CU, LayerId::inner(1), LayerId::B_CU, LayerId::EDGE], "S46.R40: all-copper adds actual inner layers");
    assert_eq!(plot_on_all_layers(LayerId::F_CU, &[LayerId::EDGE, LayerId::F_CU, LayerId::EDGE, LayerId::F_MASK]), vec![LayerId::F_CU, LayerId::EDGE, LayerId::F_MASK], "S46.R43: own first, common layers in given order once");
    assert_eq!(plot_on_all_layers(LayerId::EDGE, &[LayerId::EDGE]), vec![LayerId::EDGE], "S46.R43: own common layer not repeated");
    assert_eq!(single_file_layers(&[LayerId::F_MASK, LayerId::F_CU]).unwrap(), vec![LayerId::F_MASK, LayerId::F_CU], "S46.R44: single file preserves explicit order");
    assert!(single_file_layers(&[]).unwrap_err().to_string().contains("At least one layer must be specified"), "S46.R44: single file requires explicit layers");
    let mut board = defaults(GerberJobPath::Board);
    board.layers = vec![LayerId::B_CU, LayerId::EDGE];
    board.layers_to_include_on_all_layers = vec![LayerId::EDGE];
    board.create_gerber_job_file = false;
    let mut cli = defaults(GerberJobPath::Cli);
    cli.precision = 5;
    cli.layers = vec![LayerId::F_FAB];
    cli.layers_to_include_on_all_layers = vec![LayerId::MARGIN];
    cli.check_zones = true;
    cli.variant = "selected".into();
    cli.output_filename = "out/".into();
    cli.drawing_sheet = "custom".into();
    cli.text_variables.insert("LAYER".into(), "shown only".into());
    let merged = with_board_plot_params(&board, &cli).unwrap();
    assert_eq!(select_layers(GerberJobPath::Cli, &merged, &enabled), vec![LayerId::F_CU, LayerId::inner(1), LayerId::B_CU, LayerId::EDGE], "S46.R41: stored non-copper and every enabled copper");
    assert_eq!((merged.use_protel_file_extension, merged.precision, merged.layers_to_include_on_all_layers.clone()), (false, 6, vec![LayerId::EDGE]), "S46.R41: board settings replace CLI Gerber flags/common layers");
    assert!(merged.create_gerber_job_file && merged.check_zones, "S46.R41/R46: CLI job file is always on, zone check stays CLI");
    assert_eq!((&merged.variant, &merged.output_filename, &merged.drawing_sheet, &merged.text_variables), (&cli.variant, &cli.output_filename, &cli.drawing_sheet, &cli.text_variables), "S46.R41: CLI host context retained");
    cli.precision = 7;
    assert!(with_board_plot_params(&board, &cli).is_err(), "S46.R34/R41: ignored CLI precision is still validated");
}

#[test]
fn ignored_shared_settings_and_pad_number_gate() {
    let mut options = defaults(GerberJobPath::Job);
    let before = gerber(&[pad()], &options.writer_options().unwrap());
    options.mirror = true;
    options.negative = true;
    options.black_and_white = true;
    options.drill_shape = DrillShape::Small;
    let after = gerber(&[pad()], &options.writer_options().unwrap());
    assert_eq!(after, before, "S46.R49/Q4: shared mirror, colour, marks and negative never change a Gerber");
    assert_eq!(read_gerbers_job_json(&write_gerbers_job_json(&options).unwrap()).unwrap(), options, "S46.R32/R49: ignored settings are still stored");
    options.plot_pad_numbers = true;
    assert!(!options.draws_pad_numbers(), "S46.R48: pad numbers alone have no effect");
    options.sketch_pads_on_fab_layers = true;
    assert!(options.draws_pad_numbers(), "S46.R48: pad numbers require sketch pads");
}

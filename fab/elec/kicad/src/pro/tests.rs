use crate::{build_board, parse_kicad_pcb, parse_kicad_pro, read_kicad_board, KicadSources, SourceFile};
use super::pattern::{expand_bus, Matcher};
use fab_elec_circuit::{check::CheckKind, ViaSize};
use fab_elec_core::{ClassId, Severity};

#[test]
fn modern_project_rules_classes_and_patterns() {
    let board = br#"(kicad_pcb (version 20260206)
      (net 0 "") (net 1 CLK) (net 2 BUS01) (net 3 BUS02) (net 4 RX3)
      (net 5 AUX) (net 6 SPARE) (net 7 CLK2))"#;
    let project = br##"{
      "board":{"design_settings":{"meta":{"version":4},
        "rules":{"min_track_width":0.16,"min_via_annular_width":99,
          "min_copper_edge_clearance":0.42,"min_clearance":0.1,"min_groove_width":0.2,
          "min_via_diameter":0.5,"unknown_rule":true,"min_hole_clearance":"bad"},
        "track_widths":[0.4],"via_dimensions":[{}],"drc_exclusions":["x","y"],
        "zones_allow_external_fillets":true,"unknown_setting":42,
        "rule_severities":{"clearance":"ignore","hole_near_hole":"error",
          "hole_to_hole":"warning","track_width":"warning","via_diameter":"anything"}}},
      "net_settings":{"meta":{"version":5},"classes":[
        {"name":"Wide","priority":5,"clearance":0.4,"track_width":0.3,"via_diameter":0.8,
         "via_drill":0.4,"microvia_diameter":0.3,"microvia_drill":0.1,
         "diff_pair_width":0.3,"diff_pair_gap":0.35,"diff_pair_via_gap":0.25,
         "pcb_color":"rgba(0, 0, 0, 0)"},
        {"name":"Fast","priority":1,"clearance":0.22,"via_drill":0.32,"track_width":0.16,
         "pcb_color":"#ff0000ff","tuning_profile":{"length":1}}
      ],"netclass_patterns":[
        {"pattern":"CLK*","netclass":"Wide"},{"pattern":"CLK*","netclass":"Wide"},
        {"pattern":"^(CLK[0-9]?|RX[0-9]+)$","netclass":"Fast"},
        {"pattern":"BUS[02..01]","netclass":"Wide"}
      ],"netclass_assignments":{"AUX":["Missing"],"absent":["Unused"],"SPARE":"Wide"},
      "net_chain_classes":{"one":{}},"net_colors":{"CLK":"red"}},
      "text_variables":{"LABEL":"test"},"component_class_settings":{"assignments":[{}]}
    }"##;
    let parsed = parse_kicad_pro(project).expect("authored project parses");
    assert_eq!(parsed.rule("min_via_annular_width").expect("raw rule").value, 99.0, "parse retains out-of-range raw rule");
    assert_eq!(parsed.report.current, SourceFile::Project, "standalone project report has project source");
    assert_eq!(parsed.patterns.len(), 3, "duplicate pattern and class counted once");
    let read = read_kicad_board(&KicadSources { board, project: Some(project), rules: None, name: "modern" }).expect("board plus project reads");
    let b = &read.board;
    assert_eq!((b.rules.base.track, b.rules.base.annular, b.rules.base.drill, b.rules.base.hole_clearance), (Some(160_000), Some(500_000), Some(300_000), Some(250_000)), "project minima, range reset to the stored-setting default, absent and mistyped defaults");
    assert_eq!(b.rules.base.edge, Some(420_000), "project edge clearance");
    assert_eq!((b.rules.base.clearance, b.rules.base.via, b.rules.base.pair_gap, b.rules.base.mask_margin), (Some(200_000), Some(ViaSize { pad: 600_000, drill: 300_000 }), Some(250_000), Some(0)), "default class supplies base with source mask default");
    assert_eq!(b.classes.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Default", "Wide", "Fast", "Missing", "Fast,Wide", "Fast,Default"], "configured, implicit, then composite ordering");
    assert_eq!(b.nets.iter().map(|n| n.class).collect::<Vec<_>>(), [ClassId(0), ClassId(4), ClassId(1), ClassId(1), ClassId(5), ClassId(3), ClassId(1), ClassId(4)], "wildcard, regex, padded reversed bus, explicit assignments (an implicit class is complete) and shared composite");
    let mixed = &b.classes[4].rules;
    assert_eq!((mixed.clearance, mixed.via), (Some(220_000), Some(ViaSize { pad: 800_000, drill: 320_000 })), "higher priority fills individual fields before lower priority");
    assert!(b.classes.iter().all(|c| c.rules.track.is_none()), "router widths never become class DRC minima");
    assert_eq!(read.report.gaps["class.track_width"].count, 2, "only configured class widths different from board minimum counted");
    assert_eq!(read.report.gaps["drc.exclusions"].count, 2, "excluded findings counted individually");
    assert_eq!(read.report.gaps["rules.size_presets"].count, 1, "all preset arrays share one presence count");
    assert_eq!(read.report.gaps["project.bad_value"].count, 1, "mistyped numeric rule is recoverable");
    assert_eq!(read.report.gaps["rules.min_clearance"].count, 1, "board minimum clearance is reported rather than applied to classes");
    assert!(!read.report.gaps.contains_key("rules.via_min_size"), "default-valued unsupported minimum is not counted");
    assert!(!read.report.gaps.contains_key("rules.project_file"), "supplied project replaces provisional missing-project report");
    assert_eq!(read.report.unknown.len(), 2, "unknown design keys and rule keys retained");
    assert!(read.report.render().contains("at project "), "project report locations identify file");
    assert_eq!(read.severities[0], (CheckKind::Clearance, None), "ignored severity retained");
    assert_eq!(read.severities[3], (CheckKind::HoleToHole, Some(Severity::Warning)), "new severity spelling wins");
    assert_eq!(read.severities[4], (CheckKind::TrackWidth, Some(Severity::Warning)), "configured warning retained");
    assert_eq!(read.severities[7], (CheckKind::Drill, Some(Severity::Error)), "absent severity defaults to error");
    assert_eq!(read.severities.len(), 21, "only mapped checks have severity entries");
    for (pattern, yes, no) in [
        ("a?c", "aéc", "abbc"), ("^(?:ab|cd)+$", "abcd", "zabcd"),
        ("a{2,4}?b", "aaab", "aaaaab"), ("[^a-c]+", "XYZ", "Xb"),
        ("[]a]+", "]aa", "b"), (r"\d+\s\w", "12 x", "12 !"),
        (r"a\+b", "a+b", "ab"), ("(a?)*b", "aaaaab", "aaaaac"),
        ("a{0}b", "b", "ab"), ("a{2,}b", "aaaab", "ab"),
    ] {
        let m = Matcher::new(pattern);
        assert!(m.is_match(yes), "automaton accepts whole-name match for {pattern}");
        assert!(!m.is_match(no), "automaton rejects whole-name mismatch for {pattern}");
    }
    assert!(Matcher::new(r"(a)\1").regex_unsupported, "back-reference is reported unsupported");
    assert!(Matcher::new("(?=a)a").regex_unsupported, "look-around is reported unsupported");
    assert!(Matcher::new("[[:alpha:]]").regex_unsupported, "POSIX class is reported unsupported");
    assert!(!Matcher::new("(ab").is_match("ab"), "malformed expression does not match by regex");
    assert_eq!(expand_bus("group{D[2..1], nested{A/B C} \"two words\"}").0, ["D1", "D2", "A{slash}B", "C", "two words"], "nested groups expand without prefix and encode slash");
    assert_eq!(expand_bus("D_{[1..2]}"), (vec!["D_{[1..2]}".into()], true), "formatted range remains literal and reported");
    assert_eq!(expand_bus("\"D[1..2]\"").0, ["D[1..2]"], "quoted vector is literal");
    assert_eq!(expand_bus("D[1..1]").0, ["D[1..1]"], "equal bounds are not a bus");
}

#[test]
fn legacy_project_precedence_and_edge_widths() {
    let mut board = parse_kicad_pcb(br#"(kicad_pcb (version 20171130)
      (layers (0 F.Cu signal) (31 B.Cu signal) (44 Edge.Cuts user))
      (setup (trace_min 0.12) (clearance_min 0.7) (trace_clearance 0.28))
      (net 1 CLK) (net_class Default "" (via_dia 0.7))
      (net_class Old "" (clearance 0.33) (trace_width 0.4) (add_net CLK))
      (gr_line (start 0 0) (end 1 0) (layer Edge.Cuts) (width 0.2))
      (module edge (layer F.Cu) (fp_line (start 0 1) (end 1 1) (layer Edge.Cuts) (width 0.6))))"#).expect("legacy board parses");
    let project = parse_kicad_pro(br#"{"board":{"design_settings":{"rules":{"min_track_width":0.9},
      "rule_severities":{"clearance":"ignore"}}},"net_settings":{"classes":[{"name":"New","clearance":0.8}]}}"#).expect("project parses");
    let read = build_board(&board, Some(&project), "legacy");
    assert!(board.setup.legacy_rules, "legacy setup keys set the source flag");
    assert_eq!(read.board.rules.base.track, Some(120_000), "legacy minima win over project");
    assert_eq!(read.board.rules.base.clearance, Some(280_000), "default class rather than board minimum supplies base clearance");
    assert_eq!(read.board.rules.base.edge, Some(300_000), "widest footprint or board edge determines inferred half width");
    assert_eq!(read.board.classes.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Default", "Old"], "legacy class list wins");
    assert_eq!(read.board.nets[1].class, ClassId(1), "legacy add_net remains explicit assignment");
    assert!(read.board.classes.iter().all(|c| c.rules.track.is_none()), "legacy router widths do not become DRC minima");
    for key in ["project.design_settings_unused", "project.net_settings_unused", "rules.min_clearance"] {
        assert_eq!(read.report.gaps[key].count, 1, "legacy precedence count for {key}");
    }
    assert_eq!(read.severities[0], (CheckKind::Clearance, None), "project severities still apply to legacy board");
    let setup = parse_kicad_pcb(b"(kicad_pcb (setup (defaults (edge_clearance 0.09) (other arbitrary))))").expect("edge default parses").setup;
    assert!(setup.legacy_rules, "defaults list itself is legacy rule presence");
    board.setup.edge_clearance = setup.edge_clearance;
    assert_eq!(build_board(&board, Some(&project), "explicit edge").board.rules.base.edge, Some(90_000), "explicit edge default beats line-width inference");
}

#[test]
fn recoverable_project_legacy_migration_and_rules_report() {
    let board = br#"(kicad_pcb (version 20260206) (net 1 "~{RESET}") (net 2 "~RESET~") (net 3 AUX))"#;
    let broken = read_kicad_board(&KicadSources { board, project: Some(b"{\n\"x\":1e999}"), rules: None, name: "broken" }).expect("bad project never refuses board");
    assert_eq!((broken.board.rules.base.track, broken.board.rules.base.edge, broken.board.rules.base.clearance), (Some(200_000), Some(500_000), Some(200_000)), "unreadable project gives reference defaults");
    assert_eq!(broken.report.gaps["project.refused"].first, (SourceFile::Project, 2, 5), "project refusal carries source and location");
    assert_eq!(broken.report.gaps["rules.project_file"].count, 1, "unreadable project follows missing-project path");
    let project = br#"{"net_settings":{"meta":{"version":2},"classes":[
      {"name":"First","clearance":0.4,"nets":["~RESET~"]},
      {"name":"Second","clearance":0.6,"nets":["~RESET~"]}],
      "netclass_assignments":{"AUX":"Second"},"net_colors":null}}"#;
    let rules = br#"(version 1)
      (rule "spacing" (condition "A.NetName == 'CLK'") (constraint clearance (min 0.3mm)))
      (rule "edge" (constraint edge_clearance (min 0.4mm)))"#;
    let read = read_kicad_board(&KicadSources { board, project: Some(project), rules: Some(rules), name: "old schemas" }).expect("schema two and rules read");
    assert_eq!(read.board.classes[read.board.nets[2].class.ix()].name, "First,Second,Default", "schema two class nets become patterns ordered by class priority");
    assert_eq!(read.board.classes[read.board.nets[2].class.ix()].rules.clearance, Some(400_000), "older class order supplies precedence");
    assert_eq!(read.board.nets[1].class, ClassId(0), "schema two retains overbar spelling until schema zero conversion");
    assert!(!read.report.gaps.contains_key("project.bad_value"), "null stands for an absent map, not a bad value");
    let project_zero = String::from_utf8(project.to_vec()).expect("authored UTF-8").replace("\"version\":2", "\"version\":0");
    let zero = read_kicad_board(&KicadSources { board, project: Some(project_zero.as_bytes()), rules: None, name: "schema zero" }).expect("schema zero reads");
    assert_ne!(zero.board.nets[1].class, ClassId(0), "schema zero converts class nets to modern overbar names");
    assert_eq!(zero.board.nets[2].class, ClassId(0), "schema zero does not keep old overbar pattern");
    for (old, new) in [("~A~", "~{A}"), ("~~", "~"), ("~~{", "~~{"), ("~A B", "~{A} B"), ("~A)", "~{A})"), ("~", "~"), ("~{A}", "~{A}")] {
        assert_eq!(super::parse::overbar(old), new, "old overbar conversion for {old}");
    }
    assert_eq!(read.report.read["rules.custom"], 2, "two top-level rules counted");
    let net = &read.report.gaps["rules.custom.cond.NetName"];
    assert!(net.values[0].0.starts_with("spacing"), "an unmapped rule's loss names it: {:?}", net.values);
    assert_eq!(net.first.0, SourceFile::Rules, "rules locations name rules file");
    assert!(read.report.gaps["rules.custom.constraint.edge_clearance"].values[0].0.starts_with("edge"));
    let unreadable = read_kicad_board(&KicadSources { board, project: None, rules: Some(b"(rule \"open)"), name: "bad rules" }).expect("rules lexical error is recoverable");
    assert_eq!(unreadable.report.gaps["rules.custom_unreadable"].count, 1, "rules lexical failure reported");
    assert!(unreadable.report.gaps.keys().all(|k| !k.starts_with("rules.custom.")), "an unreadable file reports no per-rule losses");
    let mut merged = read.report.clone();
    merged.current = SourceFile::Board;
    let key = "rules.custom.cond.NetName";
    let value = read.report.gaps[key].values[0].0.clone();
    merged.gap_value(key, crate::GapKind::Dropped, (99, 1), value);
    merged.merge(&read.report);
    assert_eq!(merged.gaps[key].first, (SourceFile::Board, 99, 1), "report first location prefers board before line number");
    assert_eq!(merged.gaps[key].values[0].1, 3, "report merge adds repeated value counts");
}

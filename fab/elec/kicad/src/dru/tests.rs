use super::*;
use crate::{read_kicad_board, GapKind, KicadSources, SourceFile};
use fab_elec_board::{resolve_rules, RuleItem, RuleKey, RuleProps, RuleSource};
use fab_elec_core::{ClassId, LayerId, Severity};

#[test]
fn parser_values_expressions_and_located_refusals() {
    let text = br#"# File comment
(version 20200611)
  # Indented comment
(rule "values"
 (layer "*.Cu") (severity warning)
 (condition "A.NetClass == 'P*' && A.Type == 'Track' || A.Type != 'Pad'")
 (constraint clearance (min (1mm + 10mil) / 2) (opt "0.1in") (max 3 * 1mm - 0.25mm))
 (constraint skew (min 1ps) (opt 1500fs) (max 2ps) (within_diff_pairs))
 (constraint via_count (min 1 + 2 * 3) (opt 8) (max 9))
 (constraint hole_aspect_ratio (min 1.2345))
 (constraint solder_paste_rel_margin (min -0.1))
 (constraint track_angle (min 45) (max 90deg))
 (constraint solder_paste_abs_margin (min -0.1mm))
 (constraint mechanical_clearance (min 1mm))
 (constraint mechanical_hole_clearance (min 2mm))
 (constraint hole (min 0.3mm))
 (constraint text_thickness (min 0.1mm))
 (constraint disallow track via through_via blind_via buried_via micro_via pad zone text graphic hole "footprint")
 (constraint zone_connection thermal_reliefs)
 (constraint return_path (layer F.Cu) (net "GND"))
 (constraint assertion "!A.Net == 1 + 2 * 3 || L.Layer == 'F.Cu'"))
(rule outer (layer outer) (severity ignore) (constraint clearance (min 1mm)))
(rule inner (layer inner) (severity error) (constraint clearance (min 1mm)))
(rule named (layer F.Cu) (severity exclusion) (constraint clearance (min 1mm)))
(rule noncopper (layer B.SilkS))
(rule replace (condition "B.Net == 1") (condition "A.Unknown_Display == null") (constraint track_width))
(rule calls (condition "A.fromTo('U1-1', 'U2-?') || B.insideArea('a\\'b')"))
(rule aliases (condition "A.insideCourtyard('a') || A.memberOf('b')"))
(rule "${untouched}")
(rule arithmetic (condition "(A.Position_X + 1mm) / 2 <= A.Position_Y && A.Net >= 0 || A.Net < 2 || A.Net > 3"))
(rule solid (constraint zone_connection solid))
(rule none (constraint zone_connection none))
(rule spaced (condition "A.Orientation == 90 deg || A.Reference == 'TP*") (constraint clearance (min 0,2mm) (max 1 mm)))"#;
    let file = parse_kicad_dru(text).expect("complete rule syntax reads");
    assert!(file.too_recent, "a newer version is accepted and marked");
    assert_eq!(file.report.gaps["rules.text_variable"].count, 1, "variables remain unexpanded and reported");
    let rule = &file.rules[0];
    assert_eq!(rule.location, (4, 1), "rule location names the opening parenthesis");
    assert_eq!(&text[rule.offset..rule.offset + 5], b"(rule", "rule retains its byte offset");
    assert_eq!(rule.severity, Some(RuleSeverity::Warning));
    assert_eq!(rule.layers, Some(LayerSet::ALL_COPPER));
    let clearance = &rule.constraints[0];
    assert_eq!((clearance.min, clearance.opt, clearance.max), (Some(627_000), Some(2_540_000), Some(2_750_000)), "units, parentheses and arithmetic convert to nm");
    let skew = &rule.constraints[1];
    assert_eq!((skew.min, skew.opt, skew.max), (Some(1000), Some(1500), Some(2000)), "time values use fs");
    assert!(skew.in_time && skew.within_diff_pairs);
    assert_eq!(rule.constraints[2].min, Some(7), "counts use arithmetic without units");
    assert_eq!(rule.constraints[3].min, Some(1235), "ratios round to thousandths");
    assert_eq!(rule.constraints[4].min, Some(-100), "signed ratios retain sign");
    assert_eq!(rule.constraints[5].min, Some(45), "bare angles are degrees");
    assert_eq!(rule.constraints[6].min, Some(-100_000), "signed length is evaluated");
    for (index, kind) in [(7, ConstraintKind::PhysicalClearance), (8, ConstraintKind::PhysicalHoleClearance), (9, ConstraintKind::HoleSize), (10, ConstraintKind::LineWidth)] {
        assert_eq!(rule.constraints[index].kind, kind, "deprecated constraint normalises");
    }
    assert!(rule.constraints[11].disallow.contains(DisallowKinds::VIA));
    assert!(rule.constraints[11].disallow.contains(DisallowKinds::FOOTPRINT));
    assert_eq!(rule.constraints[12].zone_connection, Some(ZoneConnection::ThermalReliefs));
    assert_eq!(rule.constraints[13].return_path_net.as_deref(), Some("GND"));
    assert_eq!(rule.constraints[13].return_path_layer.as_deref(), Some("F.Cu"));
    let Some(Expr::Binary { op: BinaryOp::And, left, right }) = &rule.condition else { panic!("and must be the weaker operator"); };
    assert!(matches!(right.as_ref(), Expr::Binary { op: BinaryOp::Or, .. }), "or groups to the right of and");
    assert!(matches!(left.as_ref(), Expr::Binary { right, .. } if matches!(right.as_ref(), Expr::String { wildcard: true, .. })), "wildcard literal is marked");
    let Some(Expr::Binary { op: BinaryOp::Or, left, .. }) = &rule.constraints[14].assertion else { panic!("assertion parsed"); };
    assert!(matches!(left.as_ref(), Expr::Binary { op: BinaryOp::Equal, left, right }
        if matches!(left.as_ref(), Expr::Unary { op: UnaryOp::Not, .. })
        && matches!(right.as_ref(), Expr::Binary { op: BinaryOp::Add, .. })), "not binds above comparison, below arithmetic");
    assert_eq!(file.rules[1].layers, Some(LayerSet::EMPTY.with(LayerId::F_CU).with(LayerId::B_CU)));
    assert!(!file.rules[2].layers.expect("inner layers").has(LayerId::F_CU));
    assert_eq!(file.rules[3].layers, Some(LayerSet::EMPTY.with(LayerId::F_CU)));
    assert_eq!(file.rules[4].layers, Some(LayerSet::EMPTY.with(LayerId::B_SILK)));
    assert!(!file.rules[5].condition.as_ref().expect("replacement").names(&[Item::B]), "last condition replaces earlier B use");
    assert!(file.rules[6].condition.as_ref().expect("calls").names(&[Item::B]));
    let spaced = file.rules.last().expect("spaced rule");
    assert_eq!((spaced.constraints[0].min, spaced.constraints[0].max), (Some(200_000), Some(1_000_000)), "decimal comma and a unit after a space");
    let Some(Expr::Binary { op: BinaryOp::Or, right, .. }) = &spaced.condition else { panic!("spaced condition"); };
    assert!(matches!(right.as_ref(), Expr::Binary { right, .. } if matches!(right.as_ref(), Expr::String { value, .. } if value == "TP*")), "an unclosed string runs to the end of the condition");

    for kind in ["length", "creepage", "track_width", "diff_pair_gap", "silk_clearance", "hole_clearance",
        "diff_pair_uncoupled", "edge_clearance", "courtyard_clearance", "hole_to_hole", "via_diameter",
        "connection_width", "via_dangling", "microvia_stack_depth", "min_resolved_spokes", "bridged_mask",
        "annular_width", "text_height", "track_segment_length", "thermal_relief_gap", "thermal_spoke_width",
        "solder_mask_expansion", "solder_mask_sliver", "net_chain_length", "stub_length"] {
        parse_kicad_dru(format!("(version 1)(rule x (constraint {kind}))").as_bytes()).expect("every remaining constraint kind is recognised");
    }
    for (body, message) in [
        ("(other x)", "expected rule"),
        ("(rule 4)", "quoted name"),
        ("(rule x (unknown))", "unknown rule clause"),
        ("(rule x (constraint unknown))", "unknown constraint"),
        ("(rule x (constraint hole)(constraint hole_size))", "repeated constraint"),
        ("(rule x (layer outer)(layer inner))", "second layer"),
        ("(rule x (layer NotALayer))", "unknown layer"),
        ("(rule x (severity fatal))", "unknown severity"),
        ("(rule x (constraint clearance (min)))", "empty value"),
        ("(rule x (constraint clearance (min 1)))", "missing units"),
        ("(rule x (constraint clearance (min 1 + 2)))", "missing units"),
        ("(rule x (constraint clearance (min 1cm)))", "unknown numeric unit"),
        ("(rule x (constraint clearance (min 1ps)))", "time units"),
        ("(rule x (constraint clearance (min 1deg)))", "wrong unit"),
        ("(rule x (constraint length (min 1mm + 1ps)))", "mixed units"),
        ("(rule x (constraint length (min 1mm)(max 1ps)))", "mixed units"),
        ("(rule x (constraint via_count (min 1mm)))", "cannot have units"),
        ("(rule x (constraint hole_aspect_ratio (min 1mm)))", "cannot have units"),
        ("(rule x (constraint clearance (min 1mm / 0)))", "non-finite"),
        ("(rule x (constraint clearance (min 999999999999999999999mm)))", "integer range"),
        ("(rule x (constraint clearance (within_diff_pairs)))", "unknown constraint clause"),
        ("(rule x (constraint disallow banana))", "unknown disallow"),
        ("(rule x (constraint zone_connection banana))", "unknown zone connection"),
        ("(rule x (constraint return_path (other x)))", "return-path clause"),
        ("(rule x (constraint return_path (layer Invalid)))", "unknown layer"),
        ("(rule x (condition A.NetClass))", "quoted expression"),
        ("(rule x (condition \"A.noSuchFunction()\"))", "unknown function"),
        ("(rule x (condition \"A.Net\t== 1\"))", "bad expression token"),
        ("(rule x (condition \"${X}\"))", "bad expression token"),
        ("(rule x (condition \"A.Net = 1\"))", "comparison token"),
        ("(rule x (condition \"A.Net < 1 < 2\"))", "cannot be chained"),
        ("(rule x (condition \"A.Net == 2qq\"))", "unknown numeric unit"),
        ("(rule x (condition \"A.Net ==\"))", "operand"),
        ("(rule x (condition \"A.Net;\"))", "unexpected token"),
        ("(rule x (condition \"A.Net == 1 2\"))", "unexpected token"),
        ("(rule x (condition \"L.Other\"))", "only L.Layer"),
        ("(rule x (condition \"C.Net\"))", "expected A"),
        ("(rule x (condition \"B.Net == 1\") (constraint annular_width (min 1mm)))", "single-item"),
        ("(rule x (condition \"AB.isCoupledDiffPair() && B.Net == 1\") (constraint skew (max 1mm)))", "single-item"),
        ("(rule x (condition \"B.Net == 1\") (constraint length))", "single-item"),
        ("(rule x (constraint assertion \"B.Net == 1\"))", "assertion"),
        ("(rule x", "end of input"),
        ("(rule \"x)", "Unterminated"),
    ] {
        let error = parse_kicad_dru(format!("(version 1)\n  {body}").as_bytes()).expect_err(body);
        assert!(error.message.contains(message), "{body}: {error}");
        assert!(error.line == 2 && error.column >= 3, "error stays located: {body}: {error}");
    }
    for text in ["", "(rule x)", "(version nope)", "(version 1))"] {
        let error = parse_kicad_dru(text.as_bytes()).expect_err("bad file envelope");
        assert_eq!(error.line, 1);
        assert!(error.column > 0);
    }
    for expr in [format!("{}A.Net{}", "(".repeat(MAX_DEPTH + 1), ")".repeat(MAX_DEPTH + 1)),
        vec!["A.Net == 1"; MAX_DEPTH + 1].join(" && ")] {
        let error = parse_kicad_dru(format!("(version 1)\n(rule deep (condition \"{expr}\"))").as_bytes()).expect_err("bounded expression depth");
        assert!(error.message.contains("deep") && error.line == 2);
    }
    let error = parse_kicad_dru(b"(version 1)\n\xff").expect_err("invalid byte is a located refusal");
    assert_eq!((error.line, error.column), (2, 1));
}

#[test]
fn mapping_and_symmetric_rule_source_reach_the_resolver() {
    let board = br#"(kicad_pcb (version 20260206)
 (layers (0 F.Cu signal) (31 B.Cu signal))
 (net 1 SLOW) (net 2 FAST)
 (net_class Default "" (clearance 0.2) (trace_width 0.2) (via_dia 0.6) (via_drill 0.3) (add_net SLOW))
 (net_class Fast "" (clearance 0.25) (trace_width 0.25) (via_dia 0.7) (via_drill 0.35) (add_net FAST)))"#;
    let rules = br#"(version 1)
(rule spacing (condition "A.NetClass == 'Fast' && B.NetClass == 'Default'") (constraint clearance (min 0.4mm)))
(rule area (condition "A.intersectsArea('guard*')") (constraint clearance (min 0.8mm)))
(rule partial (condition "A.NetClass == 'Fast' && B.NetClass == 'Fast'") (constraint clearance (min 0.5mm)) (constraint edge_clearance (min 0.6mm)))
(rule via (condition "A.NetClass == 'Fast'") (constraint via_diameter (min 0.8mm)))
(rule hidden (severity ignore) (constraint clearance (min 9mm)))
(rule exclusion (severity exclusion) (constraint clearance (min 9mm)))
(rule silk (layer B.SilkS) (constraint clearance (min 9mm)))
(rule unknown (condition "A.My_Property == 'x'") (constraint clearance (min 9mm)))
(rule blockers (condition "A.NetName == 'x' && A.intersectsArea('x')") (layer B.SilkS) (constraint length (min 9mm)))
(rule kind (condition "A.Type == 'Footprint'") (constraint clearance (min 9mm)))
(rule warning (condition "A.NetClass == 'Missing'") (severity warning) (constraint track_width (min 0.9mm)))"#;
    let read = read_kicad_board(&KicadSources { board, project: None, rules: Some(rules), name: "custom rules" }).expect("board and companion read");
    let source = read.rules.as_ref().unwrap_or_else(|| panic!("rule source returned: {:?}", read.report.gaps.get("rules.custom_unreadable")));
    assert_eq!(read.report.read["rules.custom"], 11);
    assert_eq!(read.report.made["rules.custom"], 5);
    assert_eq!(source.len(), 5);
    let names: Vec<_> = read.board.rules.custom.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["spacing", "partial", "via", "kind", "warning"], "mapped rules retain file order");
    let slow = read.board.classes.iter().position(|c| c.name == "Default").expect("default class") as u16;
    let fast = read.board.classes.iter().position(|c| c.name == "Fast").expect("fast class") as u16;
    let key = |class, item| RuleKey { class: ClassId(class), item };
    let a = key(fast, RuleItem::Pad);
    let b = key(slow, RuleItem::Track);
    let without = resolve_rules(&read.board, None);
    let with = resolve_rules(&read.board, Some(source));
    assert_eq!(without.clearance(a, b, LayerId::F_CU), 250_000, "without a source only class rules resolve");
    assert_eq!(with.clearance_and_rule(a, b, LayerId::F_CU), (400_000, "spacing"));
    assert_eq!(with.clearance(b, a, LayerId::F_CU), 400_000, "source evaluates both orders");
    assert_eq!(with.clearance(a, a, LayerId::F_CU), 500_000, "a partial rule still contributes clearance");
    assert_eq!(with.via(ClassId(fast)).pad, 800_000);
    assert_eq!(with.via(ClassId(fast)).drill, read.board.rules.base.via.expect("base via").drill, "missing half comes from the board base");
    assert_eq!(read.board.rules.custom[4].severity, Severity::Warning);
    let area = &read.report.gaps["rules.custom.cond.intersectsArea"];
    assert_eq!(area.kind, GapKind::Dropped);
    assert_eq!(area.first, (SourceFile::Rules, 3, 1));
    assert!(area.values[0].0.contains("area"));
    assert_eq!(read.report.gaps["rules.custom.partial"].count, 1);
    assert!(read.report.gaps["rules.custom.partial"].values[0].0.contains("edge_clearance"));
    assert_eq!(read.report.gaps["rules.custom.severity_hidden"].count, 2);
    assert_eq!(read.report.gaps["rules.custom.layer.non_copper"].count, 1);
    assert_eq!(read.report.gaps["rules.custom.cond.My_Property"].count, 1, "unknown property is a mapping loss");
    let blockers = &read.report.gaps["rules.custom.constraint.length"];
    for name in ["blockers", "cond.NetName", "cond.intersectsArea", "layer.non_copper"] {
        assert!(blockers.values[0].0.contains(name), "loss includes {name}");
    }

    let props = |name: &str, item| RuleProps { class: ClassId(0), class_name: name.into(), item };
    let composite = props("Fast,Power", RuleItem::Track);
    let other = props("Default", RuleItem::Pad);
    for (condition, expected) in [
        ("A.NetClass == 'f*'", true), ("A.NetClass == 'fast,power'", true),
        ("A.NetClass != 'f?st'", true), // The other item satisfies the symmetric inequality.
        ("A.hasNetclass('P*')", true), ("A.hasNetclass('p*')", false),
        ("A.hasExactNetclass('Fast,Power')", true), ("A.hasExactNetclass('fast,power')", false),
        ("A.hasNetclass('')", false), ("A.hasExactNetclass()", false),
        ("A.Type == 'Track'", true), ("A.Type == 't*'", true), ("A.Type == 'Hole'", false),
        ("A.Type != 'Footprint'", true),
        ("A.NetClass == 'Missing' && A.Type == 'Pad' || A.Type == 'Track'", false),
        ("!(A.NetClass == 'Missing')", true),
        ("A.NetClass == B.NetClass", false), ("A.NetClass != B.NetClass", true),
    ] {
        let file = parse_kicad_dru(format!("(version 1)(rule c (condition \"{condition}\") (constraint clearance (min 1mm)))").as_bytes()).expect("condition reads");
        let mut board = read.board.clone();
        board.rules.custom.clear();
        let mut report = crate::ImportReport::default();
        let source = file.append_to(&mut board, &mut report);
        let code = &board.rules.custom[0].when;
        assert_eq!(source.when(code, &composite, &other), expected, "predicate {condition}");
        assert_eq!(source.when(code, &other, &composite), expected, "symmetric predicate {condition}");
        let mut wrong = code.clone();
        wrong.id = u32::MAX;
        assert!(!source.when(&wrong, &composite, &other), "foreign code cannot index beyond conditions");
        if condition.starts_with("A.Type") {
            let hole = props("Default", RuleItem::Hole);
            assert!(!source.when(code, &hole, &hole), "undefined hole kind makes either comparison false");
        }
    }
    let changes = br#"(version 1)
(rule dimensions (constraint track_width (min 0.3mm)) (constraint diff_pair_gap (min 0.4mm)) (constraint hole_size (min 0.25mm)))
(rule upper (layer F.Cu) (constraint clearance (min 0.6mm) (max 0.9mm)))
(rule relaxed (layer F.Cu) (constraint clearance (min 0.45mm)))"#;
    let dimensions = read_kicad_board(&KicadSources { board, project: None, rules: Some(changes), name: "dimension rules" }).expect("dimension rules read");
    let matrix = resolve_rules(&dimensions.board, dimensions.rules.as_ref().map(|s| s as &dyn RuleSource));
    assert_eq!(matrix.track_min(ClassId(fast)), 300_000, "track minima reach class resolution");
    assert_eq!(matrix.pair_gap(ClassId(fast)), 400_000, "pair gaps reach class resolution");
    assert_eq!(matrix.via(ClassId(fast)).drill, 250_000, "hole size supplies via drill");
    assert_eq!(matrix.via(ClassId(fast)).pad, dimensions.board.rules.base.via.expect("base via").pad);
    assert_eq!(matrix.clearance_and_rule(a, b, LayerId::F_CU), (450_000, "relaxed"), "later rules may lower minima");
    assert_eq!(matrix.clearance(a, b, LayerId::B_CU), 250_000, "layer selection protects other copper layers");
    assert!(dimensions.report.gaps["rules.custom.partial"].values[0].0.contains("clearance.max"), "unrepresentable value limits are reported");
    let broken = read_kicad_board(&KicadSources { board, project: None,
        rules: Some(b"(version 1)(rule good (constraint clearance (min 1mm)))(rule bad (constraint clearance (min)))"), name: "invalid rules" }).expect("rule refusal keeps board");
    assert!(broken.rules.is_none() && broken.board.rules.custom.is_empty(), "file failure discards earlier good rules");
    assert_eq!(broken.report.gaps["rules.custom_unreadable"].count, 1);
}
